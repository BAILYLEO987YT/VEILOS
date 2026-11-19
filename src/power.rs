// -----| Power: off, the power button, restart |-----
//
// Power-off is ACPI's S5: the sleep type comes from the `\_S5` package in the
// DSDT (scanned for, without an AML interpreter: it is a constant package on
// every PC), written with SLP_EN to the FADT's PM1a (and PM1b) control
// registers. The firmware is first put in ACPI mode (SMI_CMD <- ACPI_ENABLE)
// if it isn't. The power button sets PWRBTN_STS in the PM1 status register;
// `poll_button` (from the BSP's timer tick; the SCI isn't routed) clears it and
// the GUI loop shuts down, as Windows does by default.
//
// Restart:
//
// `reboot` saves what power-off saves, then resets the machine. `reset` tries
// the ways a PC can be reset, in the order that works on the most machines
// (Linux's order on modern PCs), and moves on when one doesn't take:
//
//   1. the ACPI reset register, from the FADT, when the firmware sets
//      RESET_REG_SUP: the way the firmware itself says to;
//   2. port 0xCF9, the chipset's reset control (Intel and AMD since ~2000);
//   3. the 8042 keyboard controller's reset line (command 0xFE), the oldest;
//   4. a triple fault: an empty IDT and an exception, which every x86 CPU
//      answers with a reset.
//
// The crash screen and the no-system-volume screen wait for a key and then
// reset without saving (`wait_key_then_reset`): what's in memory can't be
// trusted there.

use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use spin::Mutex;
use x86_64::instructions::port::Port;

/// What the FADT and DSDT say about power management (I/O ports).
#[derive(Clone, Copy, Debug)]
struct Pm {
    pm1a_cnt: u16,
    pm1b_cnt: u16,
    pm1a_evt: u16,
    pm1b_evt: u16,
    /// Bytes of the PM1 event block: status then enable, half each.
    evt_len: u8,
    /// \_S5's SLP_TYPa and SLP_TYPb, if the DSDT has it.
    s5: Option<(u8, u8)>,
}

static PM: Mutex<Option<Pm>> = Mutex::new(None);
static BUTTON_PRESSED: AtomicBool = AtomicBool::new(false);
static BUTTON_POLL: AtomicU32 = AtomicU32::new(0);

const PWRBTN: u16 = 1 << 8;
const SCI_EN: u16 = 1 << 0;
const SLP_EN: u16 = 1 << 13;

/// Read the FADT and the DSDT's \_S5, put the firmware in ACPI mode, and
/// leave the power button's event disabled (it is polled). Call once after
/// the ACPI tables are reachable (smp::init has found the RSDP).
pub fn init() {
    let Some(fadt) = crate::smp::acpi_table(b"FACP") else {
        crate::serial_println!("[POWER] no FADT: ACPI power-off unavailable");
        return;
    };
    let off = crate::memory::get_phys_mem_offset().as_u64();
    let at = |p: u64| unsafe { core::ptr::read_unaligned((off + p) as *const u8) };
    let u32_at = |p: u64| u32::from_le_bytes([at(p), at(p + 1), at(p + 2), at(p + 3)]);
    let u64_at = |p: u64| u32_at(p) as u64 | (u32_at(p + 4) as u64) << 32;
    let len = u32_at(fadt + 4) as u64;
    // An I/O-space GAS (ACPI 2.0+ X_ fields) when the old 32-bit field is 0.
    let port = |legacy: u64, gas: u64| -> u16 {
        let p = u32_at(fadt + legacy);
        if p != 0 {
            return p as u16;
        }
        if len >= gas + 12 && at(fadt + gas) == 1 {
            return u64_at(fadt + gas + 4) as u16;
        }
        0
    };
    let pm1a_evt = port(56, 148);
    let pm1b_evt = port(60, 160);
    let pm1a_cnt = port(64, 172);
    let pm1b_cnt = port(68, 184);
    let evt_len = at(fadt + 88);
    let smi_cmd = u32_at(fadt + 48) as u16;
    let acpi_enable = at(fadt + 52);
    let flags = if len >= 116 { u32_at(fadt + 112) } else { 0 };
    if flags & (1 << 20) != 0 || pm1a_cnt == 0 {
        crate::serial_println!("[POWER] hardware-reduced ACPI or no PM1 control block: ACPI power-off unavailable");
        return;
    }
    let dsdt = if len >= 148 && u64_at(fadt + 140) != 0 { u64_at(fadt + 140) } else { u32_at(fadt + 40) as u64 };
    let s5 = if dsdt != 0 {
        let dlen = u32_at(dsdt + 4) as usize;
        let bytes = unsafe { core::slice::from_raw_parts((off + dsdt) as *const u8, dlen) };
        find_s5(bytes)
    } else {
        None
    };

    // ACPI mode: SCI_EN set. Firmware that starts in legacy mode switches
    // when asked through SMI_CMD.
    unsafe {
        let mut cnt: Port<u16> = Port::new(pm1a_cnt);
        if cnt.read() & SCI_EN == 0 && smi_cmd != 0 && acpi_enable != 0 {
            Port::<u8>::new(smi_cmd).write(acpi_enable);
            if !crate::time::spin_until(3_000_000, || cnt.read() & SCI_EN != 0) {
                crate::serial_println!("[POWER] WARN: the firmware didn't enter ACPI mode");
            }
        }
        // The button's status is polled: clear it and enable the event.
        // Real chipsets set the status bit regardless of the enable bit, but
        // QEMU only when it is set. The SCI it raises goes nowhere: IRQ 9
        // isn't routed (ioapic.rs), so nothing but this polling sees it.
        for evt in [pm1a_evt, pm1b_evt] {
            if evt != 0 && evt_len >= 4 {
                Port::<u16>::new(evt).write(PWRBTN);
                let mut en: Port<u16> = Port::new(evt + evt_len as u16 / 2);
                let v = en.read();
                en.write(v | PWRBTN);
            }
        }
    }
    crate::serial_println!(
        "[POWER] ACPI: PM1a_CNT {:#x}, PM1b_CNT {:#x}, PM1a_EVT {:#x}, S5 {:?}",
        pm1a_cnt, pm1b_cnt, pm1a_evt, s5
    );
    *PM.lock() = Some(Pm { pm1a_cnt, pm1b_cnt, pm1a_evt, pm1b_evt, evt_len, s5 });
}

/// Find `Name(\_S5, Package(n) { SLP_TYPa, SLP_TYPb, ... })` in AML.
fn find_s5(aml: &[u8]) -> Option<(u8, u8)> {
    for i in 1..aml.len().saturating_sub(8) {
        if &aml[i..i + 4] != b"_S5_" {
            continue;
        }
        let named = aml[i - 1] == 0x08 || (i >= 2 && aml[i - 1] == b'\\' && aml[i - 2] == 0x08);
        if !named || aml[i + 4] != 0x12 {
            continue; // not a NameOp with a PackageOp
        }
        let mut p = i + 5;
        p += (aml[p] >> 6) as usize + 1; // PkgLength
        p += 1; // NumElements
        let mut value = || -> Option<u8> {
            let op = *aml.get(p)?;
            let v = match op {
                0x0A => {
                    p += 1;
                    *aml.get(p)? // BytePrefix
                }
                0x00 => 0,
                0x01 => 1,
                0xFF => 0xFF,
                v => v,
            };
            p += 1;
            Some(v)
        };
        let a = value()?;
        let b = value()?;
        return Some((a, b));
    }
    None
}

/// ACPI S5. Returns only if the machine is still on afterwards.
pub fn acpi_power_off() {
    let Some(pm) = *PM.lock() else { return };
    let Some((a, b)) = pm.s5 else {
        crate::serial_println!("[POWER] no \\_S5 in the DSDT: can't power off through ACPI");
        return;
    };
    crate::serial_println!("[POWER] ACPI S5: SLP_TYP {}/{}", a, b);
    x86_64::instructions::interrupts::disable();
    unsafe {
        if pm.pm1b_cnt != 0 {
            let mut c: Port<u16> = Port::new(pm.pm1b_cnt);
            let v = c.read() & !(0x7 << 10);
            c.write(v | (b as u16 & 7) << 10 | SLP_EN);
        }
        let mut c: Port<u16> = Port::new(pm.pm1a_cnt);
        let v = c.read() & !(0x7 << 10);
        c.write(v | (a as u16 & 7) << 10 | SLP_EN);
    }
    crate::time::delay_ms(500);
    x86_64::instructions::interrupts::enable();
    crate::serial_println!("[POWER] WARN: still on after ACPI S5");
}

/// From the BSP's timer tick, every 50 ms: the power button.
pub fn poll_button() {
    if BUTTON_POLL.fetch_add(1, Ordering::Relaxed) % 50 != 0 {
        return;
    }
    let Some(pm) = PM.try_lock().and_then(|p| *p) else { return };
    for evt in [pm.pm1a_evt, pm.pm1b_evt] {
        if evt == 0 || pm.evt_len < 4 {
            continue;
        }
        let mut sts: Port<u16> = Port::new(evt);
        unsafe {
            if sts.read() & PWRBTN != 0 {
                sts.write(PWRBTN); // write 1 to clear
                BUTTON_PRESSED.store(true, Ordering::Release);
            }
        }
    }
}

/// Was the power button pressed since the last call? (GUI loop.)
pub fn take_button_press() -> bool {
    BUTTON_PRESSED.swap(false, Ordering::AcqRel)
}

/// Which reset to use: `Any` tries them all in order; the others try only
/// that one (then the triple fault), for checking a machine (REBOOT CF9...).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Method {
    Any,
    Acpi,
    Cf9,
    Kbd,
    Triple,
}

/// Save users, profiles, the audit log and the disk cache, then reset.
pub fn reboot(method: Method) -> ! {
    crate::serial_println!("[POWER] restart requested ({:?})", method);
    crate::shell::save_for_power_off();
    reset(method)
}

/// Reset now, saving nothing.
pub fn reset(method: Method) -> ! {
    x86_64::instructions::interrupts::disable();
    let wait = || crate::time::delay_ms(100);
    if matches!(method, Method::Any | Method::Acpi) {
        match fadt_reset_register() {
            Some(reg) => {
                crate::serial_println!("[POWER] reset: ACPI reset register ({:?} {:#x} = {:#x})", reg.space, reg.address, reg.value);
                if reg.write() {
                    wait();
                }
            }
            None => crate::serial_println!("[POWER] reset: no ACPI reset register"),
        }
    }
    if matches!(method, Method::Any | Method::Cf9) {
        crate::serial_println!("[POWER] reset: port 0xCF9");
        // SAFETY: the chipset's reset control register. Bit 1 picks a
        // system reset, bit 2 starts it, bit 3 makes it a full (cold) one.
        unsafe {
            let mut cf9: Port<u8> = Port::new(0xCF9);
            let v = cf9.read() & !0x0E;
            cf9.write(v | 0x02);
            crate::time::delay_us(50);
            cf9.write(v | 0x0E);
        }
        wait();
    }
    if matches!(method, Method::Any | Method::Kbd) {
        crate::serial_println!("[POWER] reset: 8042 keyboard controller");
        // SAFETY: the 8042's status and command ports; 0xFE pulses the CPU's
        // reset line. Waits (bounded) for its input buffer to empty first.
        unsafe {
            let mut status: Port<u8> = Port::new(0x64);
            for _ in 0..10 {
                for _ in 0..10_000 {
                    if status.read() & 0x02 == 0 {
                        break;
                    }
                    crate::time::delay_us(10);
                }
                status.write(0xFE);
                crate::time::delay_ms(10);
            }
        }
        wait();
    }
    crate::serial_println!("[POWER] reset: triple fault");
    triple_fault()
}

/// An empty IDT and a breakpoint: the CPU can't deliver the exception, nor
/// the double fault that follows, and shuts down, which resets the machine.
fn triple_fault() -> ! {
    use x86_64::instructions::tables::lidt;
    use x86_64::structures::DescriptorTablePointer;
    let empty = DescriptorTablePointer { limit: 0, base: x86_64::VirtAddr::new(0) };
    // SAFETY: deliberately breaks interrupt delivery: nothing runs after this.
    unsafe {
        lidt(&empty);
        core::arch::asm!("int3", options(nomem, nostack));
    }
    loop {
        x86_64::instructions::hlt();
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Space {
    Memory,
    Io,
    PciConfig,
    Other(u8),
}

struct ResetRegister {
    space: Space,
    address: u64,
    value: u8,
}

impl ResetRegister {
    /// Write the reset value. False if the register is somewhere this can't
    /// reach (system memory is not mapped here; it is rare).
    fn write(&self) -> bool {
        // SAFETY: the firmware names this register, and this value, as the
        // way to reset the machine.
        unsafe {
            match self.space {
                Space::Io if self.address <= 0xFFFF => Port::<u8>::new(self.address as u16).write(self.value),
                Space::PciConfig => {
                    // Bus 0; the address holds device, function and offset.
                    let dev = ((self.address >> 32) & 0x1F) as u32;
                    let func = ((self.address >> 16) & 0x7) as u32;
                    let off = (self.address & 0xFF) as u32;
                    Port::<u32>::new(0xCF8).write(0x8000_0000 | dev << 11 | func << 8 | (off & 0xFC));
                    Port::<u8>::new(0xCFC + (off & 3) as u16).write(self.value);
                }
                _ => {
                    crate::serial_println!("[POWER] reset: the ACPI reset register is in {:?} space, not supported", self.space);
                    return false;
                }
            }
        }
        true
    }
}

/// The FADT's RESET_REG and RESET_VALUE, if the firmware says they work
/// (Flags bit 10, RESET_REG_SUP; FADT revision 2 and later).
fn fadt_reset_register() -> Option<ResetRegister> {
    let fadt = crate::smp::acpi_table(b"FACP")?;
    let base = crate::memory::get_phys_mem_offset().as_u64() + fadt;
    // SAFETY: the table lies in memory the bootloader maps (smp.rs reads
    // ACPI tables the same way); its length is checked before each field.
    let byte = |off: u64| unsafe { core::ptr::read_volatile((base + off) as *const u8) };
    let len = u32::from_le_bytes([byte(4), byte(5), byte(6), byte(7)]);
    if len < 129 {
        return None;
    }
    let flags = u32::from_le_bytes([byte(112), byte(113), byte(114), byte(115)]);
    if flags & (1 << 10) == 0 {
        return None;
    }
    let space = match byte(116) {
        0 => Space::Memory,
        1 => Space::Io,
        2 => Space::PciConfig,
        n => Space::Other(n),
    };
    let mut address = 0u64;
    for i in 0..8 {
        address |= (byte(120 + i) as u64) << (8 * i);
    }
    if address == 0 {
        return None;
    }
    Some(ResetRegister { space, address, value: byte(128) })
}

/// Wait for a key, polling with interrupts off, then reset: on the PS/2
/// keyboard (and USB keyboards the firmware emulates as PS/2), and on USB
/// keyboards VEILOS drives (usb::poll puts their keys in the same queue).
/// Keys already held or queued when this starts don't count.
pub fn wait_key_then_reset() -> ! {
    x86_64::instructions::interrupts::disable();
    // SAFETY: the 8042's status and data ports, read only.
    let mut status: Port<u8> = Port::new(0x64);
    let mut data: Port<u8> = Port::new(0x60);
    // No 8042 reads as 0xFF: then only USB.
    let ps2 = unsafe { status.read() } != 0xFF;
    let drain_until = crate::time::now_ms() + 500;
    while crate::time::now_ms() < drain_until {
        if ps2 && unsafe { status.read() } & 0x01 != 0 {
            let _ = unsafe { data.read() };
        }
        crate::usb::poll();
        while crate::task::keyboard::pop_scancode().is_some() {}
    }
    loop {
        crate::usb::poll();
        if let Some(b) = crate::task::keyboard::pop_scancode() {
            if b != 0xE0 && b < 0x80 {
                crate::serial_println!("[POWER] key {:#04x} (USB): restarting", b);
                reset(Method::Any);
            }
        }
        if !ps2 {
            continue;
        }
        let s = unsafe { status.read() };
        if s & 0x01 == 0 {
            core::hint::spin_loop();
            continue;
        }
        let b = unsafe { data.read() };
        // A key press from the keyboard (not the mouse, bit 5), set-1 make
        // codes only: not releases, prefixes or controller replies.
        if s & 0x20 == 0 && b != 0 && b < 0x80 {
            crate::serial_println!("[POWER] key {:#04x}: restarting", b);
            reset(Method::Any);
        }
    }
}
