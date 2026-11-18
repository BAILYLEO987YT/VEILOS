// -----| Restart |-----
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

use x86_64::instructions::port::Port;

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

/// Wait for a key on the PS/2 keyboard (USB keyboards too, where the
/// firmware emulates PS/2), polling with interrupts off, then reset. Keys
/// already held or queued when this starts don't count.
pub fn wait_key_then_reset() -> ! {
    x86_64::instructions::interrupts::disable();
    // SAFETY: the 8042's status and data ports, read only.
    let mut status: Port<u8> = Port::new(0x64);
    let mut data: Port<u8> = Port::new(0x60);
    let drain_until = crate::time::now_ms() + 500;
    while crate::time::now_ms() < drain_until {
        if unsafe { status.read() } & 0x01 != 0 {
            let _ = unsafe { data.read() };
        }
    }
    loop {
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
