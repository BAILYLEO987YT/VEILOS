// =====================================================================
// pci.rs — PCI bus enumeration and device registry
// =====================================================================
//
// The bus is scanned exactly once, by `init()`, early in boot. Every device
// found is decoded (BARs sized, capability list walked) and stored in the
// global `DEVICES` registry. Drivers query the registry with `devices()`,
// `find()` or `find_class()` instead of rescanning the bus themselves.
//
// Why only once: sizing a BAR means writing all-ones to it with decode
// disabled. Doing that on a live device (e.g. the scanout framebuffer, or a
// NIC mid-DMA) would glitch or break it, so it must happen before any driver
// has touched the hardware.
//
// Access mechanism: legacy port I/O (0xCF8/0xCFC), which only reaches the
// first 256 bytes of config space. PCIe extended config (ECAM via the ACPI
// MCFG table) is not wired up yet, so extended capabilities (AER, SR-IOV,
// resizable BAR, ...) are not visible.

use alloc::vec::Vec;
use spin::Mutex;
use x86_64::instructions::port::Port;
use crate::println;

const CONFIG_ADDRESS: u16 = 0xCF8;
const CONFIG_DATA: u16 = 0xCFC;

// -----| config-space register offsets |-----

const REG_VENDOR_ID:   u8 = 0x00;
const REG_COMMAND:     u8 = 0x04;
const REG_STATUS:      u8 = 0x06;
const REG_CLASS:       u8 = 0x08;
const REG_HEADER_TYPE: u8 = 0x0E;
const REG_BAR0:        u8 = 0x10;
const REG_BUS_NUMBERS: u8 = 0x18; // type-1 (bridge) header only
const REG_CAP_PTR:     u8 = 0x34;
const REG_INTERRUPT:   u8 = 0x3C;

/// Command register bits.
pub const CMD_IO_SPACE:      u16 = 1 << 0;
pub const CMD_MEMORY_SPACE:  u16 = 1 << 1;
pub const CMD_BUS_MASTER:    u16 = 1 << 2;
pub const CMD_INTX_DISABLE:  u16 = 1 << 10;

/// Status register bit 4: the device implements a capability list.
const STATUS_CAP_LIST: u16 = 1 << 4;

/// Standard capability IDs.
pub const CAP_ID_POWER_MGMT: u8 = 0x01;
pub const CAP_ID_MSI:        u8 = 0x05;
pub const CAP_ID_VENDOR:     u8 = 0x09;
pub const CAP_ID_PCIE:       u8 = 0x10;
pub const CAP_ID_MSIX:       u8 = 0x11;

// =====================================================================
// -----| Raw config-space access |-----
// =====================================================================

/// A bus/device/function triple.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct PciAddress {
    pub bus: u8,
    pub device: u8,
    pub function: u8,
}

impl PciAddress {
    pub const fn new(bus: u8, device: u8, function: u8) -> Self {
        Self { bus, device, function }
    }

    fn config_address(&self, offset: u8) -> u32 {
        (1 << 31)
            | ((self.bus as u32) << 16)
            | (((self.device & 0x1F) as u32) << 11)
            | (((self.function & 0x07) as u32) << 8)
            | ((offset as u32) & 0xFC)
    }

    pub fn read_u32(&self, offset: u8) -> u32 {
        let address = self.config_address(offset);
        with_config_lock(|| unsafe {
            Port::<u32>::new(CONFIG_ADDRESS).write(address);
            Port::<u32>::new(CONFIG_DATA).read()
        })
    }

    pub fn write_u32(&self, offset: u8, value: u32) {
        let address = self.config_address(offset);
        with_config_lock(|| unsafe {
            Port::<u32>::new(CONFIG_ADDRESS).write(address);
            Port::<u32>::new(CONFIG_DATA).write(value);
        })
    }

    pub fn read_u16(&self, offset: u8) -> u16 {
        (self.read_u32(offset) >> ((offset & 2) * 8)) as u16
    }

    pub fn read_u8(&self, offset: u8) -> u8 {
        (self.read_u32(offset) >> ((offset & 3) * 8)) as u8
    }

    /// 16-bit write via a 16-bit access to the data port, so neighbouring
    /// registers are never rewritten. This matters for COMMAND/STATUS: STATUS
    /// error bits are write-1-to-clear, and a 32-bit read-modify-write of
    /// offset 0x04 would clear any that happen to be set.
    pub fn write_u16(&self, offset: u8, value: u16) {
        let address = self.config_address(offset);
        let port = CONFIG_DATA + (offset & 2) as u16;
        with_config_lock(|| unsafe {
            Port::<u32>::new(CONFIG_ADDRESS).write(address);
            Port::<u16>::new(port).write(value);
        })
    }

    pub fn write_u8(&self, offset: u8, value: u8) {
        let address = self.config_address(offset);
        let port = CONFIG_DATA + (offset & 3) as u16;
        with_config_lock(|| unsafe {
            Port::<u32>::new(CONFIG_ADDRESS).write(address);
            Port::<u8>::new(port).write(value);
        })
    }
}

// The address/data port pair is a two-step protocol, so concurrent accesses
// must be serialised or they interleave and hit the wrong register. Interrupts
// are disabled while the lock is held so an ISR that touches config space
// can never spin on a lock its own CPU already holds.
static CONFIG_LOCK: Mutex<()> = Mutex::new(());

fn with_config_lock<R>(f: impl FnOnce() -> R) -> R {
    x86_64::instructions::interrupts::without_interrupts(|| {
        let _guard = CONFIG_LOCK.lock();
        f()
    })
}

// =====================================================================
// -----| BARs |-----
// =====================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BarKind {
    /// Not implemented by the device (or the upper half of a 64-bit BAR).
    None,
    /// I/O port space.
    Io,
    /// 32-bit memory space.
    Mem32,
    /// 64-bit memory space; occupies this BAR slot and the next one.
    Mem64,
}

/// A decoded Base Address Register.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bar {
    pub kind: BarKind,
    /// Physical address (memory BARs) or port number (I/O BARs).
    pub base: u64,
    /// Size in bytes as reported by BAR sizing; 0 if not implemented.
    pub size: u64,
    pub prefetchable: bool,
}

impl Bar {
    pub const EMPTY: Bar = Bar { kind: BarKind::None, base: 0, size: 0, prefetchable: false };

    pub fn is_present(&self) -> bool { self.kind != BarKind::None && self.size != 0 }
    pub fn is_io(&self) -> bool { self.kind == BarKind::Io && self.size != 0 }
    pub fn is_mem(&self) -> bool {
        matches!(self.kind, BarKind::Mem32 | BarKind::Mem64) && self.size != 0
    }

    /// I/O port base, if this is an I/O BAR.
    pub fn io_port(&self) -> Option<u16> {
        if self.is_io() { Some(self.base as u16) } else { None }
    }

    /// Physical memory base, if this is a memory BAR.
    pub fn mem_base(&self) -> Option<u64> {
        if self.is_mem() { Some(self.base) } else { None }
    }
}

/// Read and size every BAR of a device.
///
/// Memory and I/O decode are switched off for the duration, otherwise the
/// all-ones value written during sizing could briefly make the device claim
/// an address range that belongs to someone else. Interrupts stay off for the
/// whole probe: while an ISA/LPC bridge has I/O decode disabled, an ISR that
/// touches the PIC, PIT or PS/2 ports would talk to nothing.
///
/// Host bridges keep decode enabled (as Linux does): on some chipsets turning
/// it off cuts the CPU off from RAM or from config space itself.
fn probe_bars(addr: PciAddress, bar_count: usize, keep_decode: bool) -> [Bar; 6] {
    if bar_count == 0 { return [Bar::EMPTY; 6]; }
    x86_64::instructions::interrupts::without_interrupts(|| {
        probe_bars_inner(addr, bar_count, keep_decode)
    })
}

fn probe_bars_inner(addr: PciAddress, bar_count: usize, keep_decode: bool) -> [Bar; 6] {
    let mut bars = [Bar::EMPTY; 6];

    let command = addr.read_u16(REG_COMMAND);
    if !keep_decode {
        addr.write_u16(REG_COMMAND, command & !(CMD_IO_SPACE | CMD_MEMORY_SPACE));
    }

    let mut i = 0;
    while i < bar_count {
        let offset = REG_BAR0 + (i as u8) * 4;
        let original = addr.read_u32(offset);

        addr.write_u32(offset, 0xFFFF_FFFF);
        let mask = addr.read_u32(offset);
        addr.write_u32(offset, original);

        if mask == 0 || mask == 0xFFFF_FFFF {
            // Unimplemented BAR (reads back all zeros), or no device answered.
            i += 1;
            continue;
        }

        if original & 1 == 1 {
            // I/O BAR: bits 31:2 are the address. Upper 16 bits may read back
            // as zero on devices that only decode 16-bit port addresses.
            let size_mask = (mask & !0x3) | 0xFFFF_0000;
            bars[i] = Bar {
                kind: BarKind::Io,
                base: (original & !0x3) as u64,
                size: (!size_mask).wrapping_add(1) as u64,
                prefetchable: false,
            };
            i += 1;
            continue;
        }

        let prefetchable = original & 0x8 != 0;
        let is_64 = (original >> 1) & 0x3 == 0x2;

        if is_64 && i + 1 < bar_count {
            let hi_offset = offset + 4;
            let original_hi = addr.read_u32(hi_offset);
            addr.write_u32(hi_offset, 0xFFFF_FFFF);
            let mask_hi = addr.read_u32(hi_offset);
            addr.write_u32(hi_offset, original_hi);

            let full_mask = ((mask_hi as u64) << 32) | (mask & !0xF) as u64;
            bars[i] = Bar {
                kind: BarKind::Mem64,
                base: ((original_hi as u64) << 32) | (original & !0xF) as u64,
                size: (!full_mask).wrapping_add(1),
                prefetchable,
            };
            // bars[i + 1] stays EMPTY: it holds the upper half of this BAR.
            i += 2;
        } else {
            bars[i] = Bar {
                kind: BarKind::Mem32,
                base: (original & !0xF) as u64,
                size: (!(mask & !0xF)).wrapping_add(1) as u64,
                prefetchable,
            };
            i += 1;
        }
    }

    if !keep_decode {
        addr.write_u16(REG_COMMAND, command);
    }
    bars
}

// =====================================================================
// -----| Capabilities |-----
// =====================================================================

/// One entry in a device's capability list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capability {
    pub id: u8,
    /// Config-space offset of the capability header.
    pub offset: u8,
}

/// Decoded MSI capability.
#[derive(Debug, Clone, Copy)]
pub struct MsiInfo {
    pub offset: u8,
    pub is_64bit: bool,
    pub per_vector_masking: bool,
    /// Number of vectors the device can request (1, 2, 4, ... 32).
    pub max_vectors: u8,
}

/// Decoded MSI-X capability.
#[derive(Debug, Clone, Copy)]
pub struct MsixInfo {
    pub offset: u8,
    pub table_size: u16,
    /// BAR index and byte offset of the vector table.
    pub table_bar: u8,
    pub table_offset: u32,
    /// BAR index and byte offset of the pending-bit array.
    pub pba_bar: u8,
    pub pba_offset: u32,
}

fn walk_capabilities(addr: PciAddress) -> Vec<Capability> {
    let mut caps = Vec::new();
    if addr.read_u16(REG_STATUS) & STATUS_CAP_LIST == 0 {
        return caps;
    }

    let mut ptr = addr.read_u8(REG_CAP_PTR) & 0xFC;
    // 48 entries is the most that fit in 0x40..0xFF; the bound also stops a
    // malformed (looping) list from hanging boot.
    for _ in 0..48 {
        if ptr < 0x40 { break; }
        let header = addr.read_u16(ptr);
        caps.push(Capability { id: header as u8, offset: ptr });
        ptr = ((header >> 8) as u8) & 0xFC;
    }
    caps
}

// =====================================================================
// -----| Device |-----
// =====================================================================

#[derive(Debug, Clone)]
pub struct PciDevice {
    pub bus: u8,
    pub device: u8,
    pub function: u8,
    pub vendor_id: u16,
    pub device_id: u16,
    pub class: u8,
    pub subclass: u8,
    pub prog_if: u8,
    pub revision: u8,
    /// Header layout (bit 7 = multi-function flag stripped): 0 = endpoint,
    /// 1 = PCI-to-PCI bridge, 2 = CardBus bridge.
    pub header_type: u8,
    /// Legacy INTx routing as programmed by firmware (0xFF = none).
    pub interrupt_line: u8,
    /// INTx pin: 0 = none, 1..4 = INTA#..INTD#.
    pub interrupt_pin: u8,
    pub bars: [Bar; 6],
    pub capabilities: Vec<Capability>,
}

impl PciDevice {
    pub fn address(&self) -> PciAddress {
        PciAddress::new(self.bus, self.device, self.function)
    }

    pub fn bar(&self, index: usize) -> Bar {
        self.bars.get(index).copied().unwrap_or(Bar::EMPTY)
    }

    pub fn is_bridge(&self) -> bool { self.header_type == 1 }

    pub fn find_capability(&self, id: u8) -> Option<u8> {
        self.capabilities.iter().find(|c| c.id == id).map(|c| c.offset)
    }

    pub fn msi(&self) -> Option<MsiInfo> {
        let offset = self.find_capability(CAP_ID_MSI)?;
        let control = self.address().read_u16(offset + 2);
        Some(MsiInfo {
            offset,
            is_64bit: control & (1 << 7) != 0,
            per_vector_masking: control & (1 << 8) != 0,
            max_vectors: 1 << ((control >> 1) & 0x7).min(5),
        })
    }

    pub fn msix(&self) -> Option<MsixInfo> {
        let offset = self.find_capability(CAP_ID_MSIX)?;
        let a = self.address();
        let control = a.read_u16(offset + 2);
        let table = a.read_u32(offset + 4);
        let pba = a.read_u32(offset + 8);
        Some(MsixInfo {
            offset,
            table_size: (control & 0x7FF) + 1,
            table_bar: (table & 0x7) as u8,
            table_offset: table & !0x7,
            pba_bar: (pba & 0x7) as u8,
            pba_offset: pba & !0x7,
        })
    }

    pub fn command(&self) -> u16 {
        self.address().read_u16(REG_COMMAND)
    }

    /// Set bits in the command register (other bits are preserved).
    pub fn set_command_bits(&self, bits: u16) {
        let a = self.address();
        let cmd = a.read_u16(REG_COMMAND);
        if cmd & bits != bits {
            a.write_u16(REG_COMMAND, cmd | bits);
        }
    }

    /// Clear bits in the command register (other bits are preserved).
    pub fn clear_command_bits(&self, bits: u16) {
        let a = self.address();
        let cmd = a.read_u16(REG_COMMAND);
        if cmd & bits != 0 {
            a.write_u16(REG_COMMAND, cmd & !bits);
        }
    }

    /// Enable whichever address spaces the device's BARs actually use.
    /// Firmware usually does this already, but is not required to.
    pub fn enable_decode(&self) {
        let mut bits = 0;
        if self.bars.iter().any(|b| b.is_mem()) { bits |= CMD_MEMORY_SPACE; }
        if self.bars.iter().any(|b| b.is_io())  { bits |= CMD_IO_SPACE; }
        self.set_command_bits(bits);
    }

    /// Allow the device to master the bus (required for any DMA).
    pub fn enable_bus_master(&self) {
        self.set_command_bits(CMD_BUS_MASTER);
    }

    /// Convenience for DMA-capable drivers: decode + bus mastering.
    pub fn enable_for_driver(&self) {
        self.enable_decode();
        self.enable_bus_master();
    }
}

// =====================================================================
// -----| Enumeration |-----
// =====================================================================

/// Every PCI function found at boot, in discovery order.
static DEVICES: Mutex<Vec<PciDevice>> = Mutex::new(Vec::new());

/// Scan the bus once and populate the registry. Call before any driver
/// initialises (see the module comment for why rescans are not allowed).
pub fn init() {
    let mut found = Vec::new();
    let mut visited = [false; 256];

    let host_header = PciAddress::new(0, 0, 0).read_u8(REG_HEADER_TYPE);
    if host_header & 0x80 == 0 {
        // Single PCI host controller.
        scan_bus(0, &mut found, &mut visited);
    } else {
        // Multiple host controllers: function N of 0:0 is responsible for bus N.
        for function in 0..8 {
            if PciAddress::new(0, 0, function).read_u16(REG_VENDOR_ID) != 0xFFFF {
                scan_bus(function, &mut found, &mut visited);
            }
        }
    }

    for dev in &found {
        log_device(dev);
    }
    println!("[PCI] {} device(s) registered.", found.len());

    *DEVICES.lock() = found;
}

fn scan_bus(bus: u8, out: &mut Vec<PciDevice>, visited: &mut [bool; 256]) {
    // A misconfigured bridge can point back at an earlier bus; without this
    // guard the recursion never terminates.
    if visited[bus as usize] { return; }
    visited[bus as usize] = true;

    for device in 0..32 {
        let addr = PciAddress::new(bus, device, 0);
        if addr.read_u16(REG_VENDOR_ID) == 0xFFFF { continue; }

        scan_function(addr, out, visited);

        if addr.read_u8(REG_HEADER_TYPE) & 0x80 != 0 {
            for function in 1..8 {
                let f = PciAddress::new(bus, device, function);
                if f.read_u16(REG_VENDOR_ID) != 0xFFFF {
                    scan_function(f, out, visited);
                }
            }
        }
    }
}

fn scan_function(addr: PciAddress, out: &mut Vec<PciDevice>, visited: &mut [bool; 256]) {
    let id = addr.read_u32(REG_VENDOR_ID);
    let class_reg = addr.read_u32(REG_CLASS);
    let header_type = addr.read_u8(REG_HEADER_TYPE) & 0x7F;
    let interrupt = addr.read_u16(REG_INTERRUPT);

    let bar_count = match header_type {
        0 => 6,
        1 => 2,
        _ => 0, // CardBus: not supported
    };

    let class = (class_reg >> 24) as u8;
    let subclass = (class_reg >> 16) as u8;
    let is_host_bridge = class == 0x06 && subclass == 0x00;

    let dev = PciDevice {
        bus: addr.bus,
        device: addr.device,
        function: addr.function,
        vendor_id: id as u16,
        device_id: (id >> 16) as u16,
        class,
        subclass,
        prog_if: (class_reg >> 8) as u8,
        revision: class_reg as u8,
        header_type,
        interrupt_line: interrupt as u8,
        interrupt_pin: (interrupt >> 8) as u8,
        bars: probe_bars(addr, bar_count, is_host_bridge),
        capabilities: walk_capabilities(addr),
    };

    // A PCI-to-PCI bridge hides a whole secondary bus behind it.
    let secondary_bus = if dev.is_bridge() {
        Some((addr.read_u32(REG_BUS_NUMBERS) >> 8) as u8)
    } else {
        None
    };

    out.push(dev);

    if let Some(bus) = secondary_bus {
        if bus != 0 { scan_bus(bus, out, visited); }
    }
}

fn log_device(dev: &PciDevice) {
    println!(
        "PCI | {:02X}:{:02X}.{} | {:04X}:{:04X} | {}",
        dev.bus, dev.device, dev.function, dev.vendor_id, dev.device_id,
        get_class_name(dev.class, dev.subclass)
    );
    for (i, bar) in dev.bars.iter().enumerate() {
        if !bar.is_present() { continue; }
        crate::serial_println!(
            "[PCI]   BAR{} {:?} base={:#X} size={:#X}{}",
            i, bar.kind, bar.base, bar.size,
            if bar.prefetchable { " prefetch" } else { "" }
        );
    }
    if !dev.capabilities.is_empty() {
        crate::serial_println!(
            "[PCI]   caps: {:?}  irq line={} pin={}",
            dev.capabilities.iter().map(|c| c.id).collect::<Vec<_>>(),
            dev.interrupt_line, dev.interrupt_pin
        );
    }
}

// =====================================================================
// -----| Registry queries |-----
// =====================================================================

/// Snapshot of every registered device.
pub fn devices() -> Vec<PciDevice> {
    DEVICES.lock().clone()
}

pub fn device_count() -> usize {
    DEVICES.lock().len()
}

/// All devices matching a predicate.
pub fn find(pred: impl Fn(&PciDevice) -> bool) -> Vec<PciDevice> {
    DEVICES.lock().iter().filter(|d| pred(d)).cloned().collect()
}

/// All devices of a class, optionally narrowed to one subclass.
pub fn find_class(class: u8, subclass: Option<u8>) -> Vec<PciDevice> {
    find(|d| d.class == class && subclass.map_or(true, |s| d.subclass == s))
}

/// Re-read the vendor/device ID of every registered device and count how
/// many no longer answer with the same identity. Safe to call at runtime:
/// unlike a rescan it never touches BARs or the command register.
pub fn verify_present() -> (usize, usize) {
    let devs = DEVICES.lock();
    let missing = devs.iter()
        .filter(|d| {
            let id = d.address().read_u32(REG_VENDOR_ID);
            id as u16 != d.vendor_id || (id >> 16) as u16 != d.device_id
        })
        .count();
    (devs.len(), missing)
}

/// Translates raw numeric hardware classifications into human-readable strings.
pub fn get_class_name(class: u8, subclass: u8) -> &'static str {
    match class {
        0x01 => match subclass {
            0x01 => "IDE Storage Controller",
            0x06 => "SATA Storage Controller",
            0x08 => "NVMe Storage Controller",
            _ => "Mass Storage Controller",
        },
        0x02 => match subclass {
            0x00 => "Ethernet Controller",
            0x80 => "Network Controller",
            _ => "Network Device",
        },
        0x03 => "Display Controller (VGA/GPU)",
        0x04 => "Multimedia Audio Controller",
        0x06 => match subclass {
            0x00 => "Host Bridge",
            0x01 => "ISA Bridge",
            0x04 => "PCI-to-PCI Bridge",
            _ => "Bridge Device",
        },
        0x0C => match subclass {
            0x03 => "USB Controller",
            _ => "Serial Bus Controller",
        },
        _ => "Unknown Hardware Device",
    }
}
