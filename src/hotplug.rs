// =============================================================================
// VEILOS: PCIe native hot-plug (HAL)
// =============================================================================
//
// PCIe root and downstream ports with a hot-plug capable slot (PCIe
// capability: Slot Implemented, Slot Capabilities HPC) are found at boot and
// their Slot Status is polled (`poll`, from the GUI loop; no interrupts):
//
//   card inserted (Presence Detect Changed, now present) -> power the slot
//     on, wait for the link, give the new functions addresses from the
//     port's bridge windows and hand them to `hal::add` (drivers bind; a NIC
//     joins the logical NIC).
//   removal requested (Attention Button Pressed: the button on a real slot,
//     or QEMU's device_del) -> `hal::remove_bus` asks each driver to let go;
//     if they all can, the slot is powered off, which completes the removal.
//     If one can't (a mounted disk), the request is refused and the slot
//     stays on.
//   card pulled out (Presence Detect Changed, now absent) -> `hal::gone_bus`.
//
// Empty slots are kept powered off, as Linux does.

use alloc::string::String;
use alloc::vec::Vec;
use alloc::format;
use core::sync::atomic::{AtomicU64, Ordering};
use spin::Mutex;

use crate::pci::{self, PciAddress};

// PCIe capability registers (offsets from the capability).
const PCIE_CAPS: u8 = 0x02;
const LNKCAP: u8 = 0x0C;
const LNKSTA: u8 = 0x12;
const SLTCAP: u8 = 0x14;
const SLTCTL: u8 = 0x18;
const SLTSTA: u8 = 0x1A;

const CAPS_SLOT_IMPLEMENTED: u16 = 1 << 8;
const LNKCAP_DLLLA_REPORTING: u32 = 1 << 20;
const LNKSTA_DLLLA: u16 = 1 << 13;

const SLTCAP_ABP: u32 = 1 << 0;
const SLTCAP_PCP: u32 = 1 << 1;
const SLTCAP_AIP: u32 = 1 << 3;
const SLTCAP_PIP: u32 = 1 << 4;
const SLTCAP_HPC: u32 = 1 << 6;
const SLTCAP_NCCS: u32 = 1 << 18;

const SLTCTL_AIC_MASK: u16 = 3 << 6;
const SLTCTL_AIC_OFF: u16 = 3 << 6;
const SLTCTL_PIC_MASK: u16 = 3 << 8;
const SLTCTL_PIC_ON: u16 = 1 << 8;
const SLTCTL_PIC_OFF: u16 = 3 << 8;
/// Power Controller Control: set = power off.
const SLTCTL_PCC: u16 = 1 << 10;

const SLTSTA_ABP: u16 = 1 << 0;
const SLTSTA_PFD: u16 = 1 << 1;
const SLTSTA_MRLSC: u16 = 1 << 2;
const SLTSTA_PDC: u16 = 1 << 3;
const SLTSTA_CC: u16 = 1 << 4;
const SLTSTA_PDS: u16 = 1 << 6;
const SLTSTA_DLLSC: u16 = 1 << 8;
/// The write-1-to-clear event bits.
const SLTSTA_EVENTS: u16 = SLTSTA_ABP | SLTSTA_PFD | SLTSTA_MRLSC | SLTSTA_PDC | SLTSTA_CC | SLTSTA_DLLSC;

// Type-1 (bridge) header: forwarding windows.
const REG_IO_BASE: u8 = 0x1C;
const REG_MEM_BASE: u8 = 0x20;
const REG_PREF_BASE: u8 = 0x24;
const REG_PREF_BASE_UPPER: u8 = 0x28;
const REG_PREF_LIMIT_UPPER: u8 = 0x2C;
const REG_IO_UPPER: u8 = 0x30;

struct Slot {
    port: PciAddress,
    cap: u8,
    /// The port's secondary bus: where the card's functions appear.
    bus: u8,
    number: u32,
    sltcap: u32,
    occupied: bool,
}

static SLOTS: Mutex<Vec<Slot>> = Mutex::new(Vec::new());
static LAST_POLL_MS: AtomicU64 = AtomicU64::new(0);
const POLL_INTERVAL_MS: u64 = 250;

fn now_ms() -> u64 {
    crate::interrupts::TICKS.load(Ordering::Relaxed)
}

fn wait_ms(ms: u64) {
    let end = now_ms() + ms;
    while now_ms() < end {
        core::hint::spin_loop();
    }
}

impl Slot {
    fn read_status(&self) -> u16 {
        self.port.read_u16(self.cap + SLTSTA)
    }

    fn clear_status(&self, bits: u16) {
        self.port.write_u16(self.cap + SLTSTA, bits);
    }

    /// Change Slot Control bits under `mask`, waiting for the port to take
    /// the command when it reports completion.
    fn control(&self, mask: u16, value: u16) {
        let ctl = self.port.read_u16(self.cap + SLTCTL);
        self.port.write_u16(self.cap + SLTCTL, (ctl & !mask) | (value & mask));
        if self.sltcap & SLTCAP_NCCS == 0 {
            let end = now_ms() + 100;
            while self.read_status() & SLTSTA_CC == 0 && now_ms() < end {
                core::hint::spin_loop();
            }
            self.clear_status(SLTSTA_CC);
        }
    }

    /// Slot power and power indicator; the attention indicator goes off.
    fn power(&self, on: bool) {
        let mut mask = 0;
        let mut value = 0;
        if self.sltcap & SLTCAP_PCP != 0 {
            mask |= SLTCTL_PCC;
            value |= if on { 0 } else { SLTCTL_PCC };
        }
        if self.sltcap & SLTCAP_PIP != 0 {
            mask |= SLTCTL_PIC_MASK;
            value |= if on { SLTCTL_PIC_ON } else { SLTCTL_PIC_OFF };
        }
        if self.sltcap & SLTCAP_AIP != 0 {
            mask |= SLTCTL_AIC_MASK;
            value |= SLTCTL_AIC_OFF;
        }
        if mask != 0 {
            self.control(mask, value);
        }
    }

    /// After power-on: wait for the link (when the port reports it), then the
    /// 100 ms the PCIe spec asks before the first config access.
    fn wait_link(&self) -> bool {
        if self.port.read_u32(self.cap + LNKCAP) & LNKCAP_DLLLA_REPORTING != 0 {
            let end = now_ms() + 1000;
            while self.port.read_u16(self.cap + LNKSTA) & LNKSTA_DLLLA == 0 {
                if now_ms() >= end {
                    return false;
                }
                core::hint::spin_loop();
            }
        }
        wait_ms(100);
        true
    }

    fn location(&self) -> String {
        format!("{:02x}:{:02x}.{}", self.port.bus, self.port.device, self.port.function)
    }
}

/// An address range a bridge forwards, handed out from the bottom.
#[derive(Clone, Copy, Debug)]
struct Window {
    next: u64,
    end: u64, // inclusive
}

impl Window {
    fn new(base: u64, limit: u64) -> Option<Window> {
        (base <= limit && limit != 0).then_some(Window { next: base, end: limit })
    }

    /// `size` bytes aligned to `size` (BARs are naturally aligned).
    fn take(&mut self, size: u64) -> Option<u64> {
        let base = self.next.checked_add(size - 1)? & !(size - 1);
        let last = base.checked_add(size - 1)?;
        if last > self.end {
            return None;
        }
        self.next = last + 1;
        Some(base)
    }
}

struct Windows {
    io: Option<Window>,
    mem: Option<Window>,
    pref: Option<Window>,
}

/// The I/O, memory and prefetchable windows firmware gave the port.
fn windows(port: PciAddress) -> Windows {
    let io_lo = port.read_u16(REG_IO_BASE);
    let io_up = port.read_u32(REG_IO_UPPER);
    let io_base = ((io_lo as u64 & 0xF0) << 8) | ((io_up as u64 & 0xFFFF) << 16);
    let io_limit = ((io_lo as u64 & 0xF000) | 0xFFF) | ((io_up as u64 >> 16) << 16);
    let mem = port.read_u32(REG_MEM_BASE);
    let mem_base = (mem as u64 & 0xFFF0) << 16;
    let mem_limit = ((mem as u64 >> 16) & 0xFFF0) << 16 | 0xF_FFFF;
    let pref = port.read_u32(REG_PREF_BASE);
    let pref64 = pref & 0xF == 1;
    let (up_base, up_limit) = if pref64 {
        (port.read_u32(REG_PREF_BASE_UPPER) as u64, port.read_u32(REG_PREF_LIMIT_UPPER) as u64)
    } else {
        (0, 0)
    };
    let pref_base = ((pref as u64 & 0xFFF0) << 16) | (up_base << 32);
    let pref_limit = ((((pref as u64 >> 16) & 0xFFF0) << 16) | 0xF_FFFF) | (up_limit << 32);
    Windows { io: Window::new(io_base, io_limit), mem: Window::new(mem_base, mem_limit), pref: Window::new(pref_base, pref_limit) }
}

/// Give every BAR of a freshly plugged function an address in the port's
/// windows (firmware never saw the card, so they are all unassigned).
fn assign_bars(addr: PciAddress, dev: &pci::PciDevice, win: &mut Windows) -> Result<(), String> {
    for (i, bar) in dev.bars.iter().enumerate() {
        if !bar.is_present() {
            continue;
        }
        let reg = 0x10 + (i as u8) * 4;
        let take = |w: &mut Option<Window>| w.as_mut().and_then(|w| w.take(bar.size));
        let base = match bar.kind {
            pci::BarKind::Io => take(&mut win.io),
            _ if bar.prefetchable => take(&mut win.pref).or_else(|| take(&mut win.mem)),
            _ => take(&mut win.mem),
        }
        .ok_or_else(|| format!("BAR{} ({:#x} bytes) does not fit the slot's windows", i, bar.size))?;
        match bar.kind {
            pci::BarKind::Io => addr.write_u32(reg, base as u32),
            pci::BarKind::Mem64 => {
                addr.write_u32(reg, base as u32);
                addr.write_u32(reg + 4, (base >> 32) as u32);
            }
            _ => addr.write_u32(reg, base as u32),
        }
    }
    Ok(())
}

/// Find the hot-plug slots. Call after `hal::init`, before drivers bind (an
/// empty slot is powered off; an occupied one powered on).
pub fn init() {
    let mut slots = Vec::new();
    for dev in pci::devices().iter().filter(|d| d.is_bridge()) {
        let Some(cap) = dev.find_capability(pci::CAP_ID_PCIE) else { continue };
        let port = dev.address();
        if port.read_u16(cap + PCIE_CAPS) & CAPS_SLOT_IMPLEMENTED == 0 {
            continue;
        }
        let sltcap = port.read_u32(cap + SLTCAP);
        if sltcap & SLTCAP_HPC == 0 {
            continue;
        }
        let slot = Slot {
            port,
            cap,
            bus: (port.read_u32(0x18) >> 8) as u8,
            number: sltcap >> 19,
            sltcap,
            occupied: port.read_u16(cap + SLTSTA) & SLTSTA_PDS != 0,
        };
        slot.clear_status(SLTSTA_EVENTS);
        slot.power(slot.occupied);
        let w = windows(port);
        crate::serial_println!("[HOTPLUG] slot {} at {} (bus {}): {}, windows io {:x?} mem {:x?} pref {:x?}",
            slot.number, slot.location(), slot.bus, if slot.occupied { "occupied" } else { "empty, powered off" },
            w.io.map(|w| (w.next, w.end)), w.mem.map(|w| (w.next, w.end)), w.pref.map(|w| (w.next, w.end)));
        slots.push(slot);
    }
    crate::serial_println!("[HOTPLUG] {} hot-plug slot(s)", slots.len());
    *SLOTS.lock() = slots;
}

/// Check every slot for events. Cheap when nothing happened; call often (the
/// GUI loop does, rate-limited here).
pub fn poll() {
    let now = now_ms();
    if now.saturating_sub(LAST_POLL_MS.load(Ordering::Relaxed)) < POLL_INTERVAL_MS {
        return;
    }
    LAST_POLL_MS.store(now, Ordering::Relaxed);
    let Some(mut slots) = SLOTS.try_lock() else { return };
    for slot in slots.iter_mut() {
        let status = slot.read_status();
        let events = status & SLTSTA_EVENTS;
        if events == 0 {
            continue;
        }
        slot.clear_status(events);
        let present = status & SLTSTA_PDS != 0;
        if events & SLTSTA_PDC != 0 && present && !slot.occupied {
            insert(slot);
        } else if events & SLTSTA_PDC != 0 && !present && slot.occupied {
            crate::serial_println!("[HOTPLUG] slot {}: card pulled out", slot.number);
            crate::hal::gone_bus(slot.bus);
            slot.power(false);
            slot.occupied = false;
        } else if events & SLTSTA_ABP != 0 && slot.occupied {
            request_removal(slot);
        }
    }
}

fn insert(slot: &mut Slot) {
    crate::serial_println!("[HOTPLUG] slot {}: card inserted", slot.number);
    slot.power(true);
    if !slot.wait_link() {
        crate::serial_println!("[HOTPLUG] slot {}: link did not come up", slot.number);
        slot.power(false);
        return;
    }
    // The port forwards its windows and the card's DMA only with these on.
    slot.port.write_u16(0x04, slot.port.read_u16(0x04) | pci::CMD_IO_SPACE | pci::CMD_MEMORY_SPACE | pci::CMD_BUS_MASTER);
    let mut win = windows(slot.port);
    let mut added = 0;
    for function in 0..8 {
        let addr = PciAddress::new(slot.bus, 0, function);
        if addr.read_u16(0x00) == 0xFFFF {
            if function == 0 {
                break;
            }
            continue;
        }
        let sized = pci::read_function(addr);
        if let Err(why) = assign_bars(addr, &sized, &mut win) {
            crate::serial_println!("[HOTPLUG] slot {}: {:02x}:00.{} not added: {}", slot.number, slot.bus, function, why);
            continue;
        }
        let dev = pci::read_function(addr);
        pci::register(dev.clone());
        crate::hal::add(dev);
        added += 1;
        if function == 0 && addr.read_u8(0x0E) & 0x80 == 0 {
            break; // single-function card
        }
    }
    slot.occupied = true;
    crate::serial_println!("[HOTPLUG] slot {}: {} function(s) added", slot.number, added);
}

fn request_removal(slot: &mut Slot) {
    crate::serial_println!("[HOTPLUG] slot {}: removal requested", slot.number);
    match crate::hal::remove_bus(slot.bus) {
        Ok(()) => {
            // Powering the slot off completes the removal.
            slot.power(false);
            slot.occupied = false;
            crate::serial_println!("[HOTPLUG] slot {}: powered off, card can be removed", slot.number);
        }
        Err(why) => {
            crate::serial_println!("[HOTPLUG] slot {}: removal refused: {}", slot.number, why);
            slot.power(true);
        }
    }
}

/// One line per slot, for the terminal's SLOTS command.
pub fn describe() -> Vec<String> {
    let slots = SLOTS.lock();
    if slots.is_empty() {
        return alloc::vec![String::from("  no hot-plug slots")];
    }
    slots.iter().map(|s| format!(
        "  slot {} at {} (bus {}): {}{}",
        s.number, s.location(), s.bus,
        if s.occupied { "occupied" } else { "empty" },
        if s.sltcap & SLTCAP_ABP != 0 { ", attention button" } else { "" }
    )).collect()
}
