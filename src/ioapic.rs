// =============================================================================
// VEILOS: I/O APIC
// =============================================================================
//
// Routes device interrupts through the I/O APIC(s) to local APICs instead of
// the legacy 8259 PIC, so a device IRQ can be sent to any CPU and to any
// vector.
//
// `init` reads the I/O APICs and ISA interrupt source overrides from the
// MADT, masks every redirection entry, routes the legacy IRQs the kernel
// handles (PIT, keyboard, mouse) to the BSP on their existing IDT vectors,
// and then masks the 8259 for good. If the MADT lists no I/O APIC the kernel
// stays on the 8259. `interrupts::{mask_irq, unmask_irq, end_of_interrupt}`
// hide which controller is in use.
//
// Not done yet: routing PCI INTx (needs the ACPI _PRT, i.e. AML) and MSI.

use core::sync::atomic::{AtomicBool, Ordering};
use x86_64::structures::paging::{FrameAllocator, Mapper, Page, PageTableFlags, PhysFrame, Size4KiB};
use x86_64::{PhysAddr, VirtAddr};

const IOREGSEL: u64 = 0x00;
const IOWIN:    u64 = 0x10;

const REG_ID:     u32 = 0x00;
const REG_VER:    u32 = 0x01;
const REG_REDTBL: u32 = 0x10;

const RTE_MASKED:     u64 = 1 << 16;
const RTE_LEVEL:      u64 = 1 << 15;
const RTE_ACTIVE_LOW: u64 = 1 << 13;

const MAX_IOAPICS: usize = 8;

/// Legacy ISA IRQs the kernel has handlers for. Each is routed to vector
/// `PIC_1_OFFSET + irq`, the same vector it had under the 8259.
const ROUTED_ISA_IRQS: [u8; 3] = [0, 1, 12];

#[derive(Clone, Copy)]
struct IoApic {
    base:     u64, // virtual (identity-mapped, uncached)
    gsi_base: u32,
    entries:  u32,
}

#[derive(Clone, Copy)]
struct IsaRoute {
    gsi:   u32,
    /// Polarity/trigger bits for the redirection entry.
    flags: u64,
}

struct State {
    ioapics: [Option<IoApic>; MAX_IOAPICS],
    isa:     [IsaRoute; 16],
}

static STATE: spin::Mutex<State> = spin::Mutex::new(State {
    ioapics: [None; MAX_IOAPICS],
    isa:     [IsaRoute { gsi: 0, flags: 0 }; 16],
});

static ACTIVE: AtomicBool = AtomicBool::new(false);

/// True once device IRQs are delivered by the I/O APIC (EOI goes to the LAPIC).
#[inline]
pub fn is_active() -> bool {
    ACTIVE.load(Ordering::Acquire)
}

unsafe fn read_reg(base: u64, reg: u32) -> u32 {
    unsafe {
        core::ptr::write_volatile((base + IOREGSEL) as *mut u32, reg);
        core::ptr::read_volatile((base + IOWIN) as *const u32)
    }
}

unsafe fn write_reg(base: u64, reg: u32, value: u32) {
    unsafe {
        core::ptr::write_volatile((base + IOREGSEL) as *mut u32, reg);
        core::ptr::write_volatile((base + IOWIN) as *mut u32, value);
    }
}

impl IoApic {
    fn read_rte(&self, index: u32) -> u64 {
        unsafe {
            let lo = read_reg(self.base, REG_REDTBL + index * 2) as u64;
            let hi = read_reg(self.base, REG_REDTBL + index * 2 + 1) as u64;
            (hi << 32) | lo
        }
    }

    /// Writes the high half (destination) first, so an entry that was masked
    /// is never live with a stale destination.
    fn write_rte(&self, index: u32, value: u64) {
        unsafe {
            write_reg(self.base, REG_REDTBL + index * 2 + 1, (value >> 32) as u32);
            write_reg(self.base, REG_REDTBL + index * 2, value as u32);
        }
    }
}

impl State {
    fn ioapic_for_gsi(&self, gsi: u32) -> Option<(IoApic, u32)> {
        self.ioapics.iter().flatten()
            .find(|a| gsi >= a.gsi_base && gsi < a.gsi_base + a.entries)
            .map(|a| (*a, gsi - a.gsi_base))
    }
}

/// Convert MPS INTI flags (MADT override) to redirection-entry bits. ISA
/// "conforms to bus" means active-high, edge-triggered.
fn inti_flags_to_rte(flags: u16) -> u64 {
    let mut rte = 0;
    if flags & 0b11 == 0b11 { rte |= RTE_ACTIVE_LOW; }
    if (flags >> 2) & 0b11 == 0b11 { rte |= RTE_LEVEL; }
    rte
}

/// Find the I/O APICs, route the legacy IRQs and retire the 8259. Call on
/// the BSP after `smp::init` (the LAPIC must be mapped and enabled).
pub fn init(
    mapper:          &mut impl Mapper<Size4KiB>,
    frame_allocator: &mut impl FrameAllocator<Size4KiB>,
) {
    let Some(madt) = crate::smp::parse_madt() else {
        crate::serial_println!("[IOAPIC] No MADT; staying on the 8259 PIC.");
        return;
    };
    if madt.ioapics.is_empty() {
        crate::serial_println!("[IOAPIC] MADT lists no I/O APIC; staying on the 8259 PIC.");
        return;
    }

    let mut state = STATE.lock();

    for (slot, entry) in madt.ioapics.iter().take(MAX_IOAPICS).enumerate() {
        // MMIO: identity-map uncached, as smp::init does for the LAPIC.
        let phys = entry.address as u64;
        let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::NO_CACHE;
        unsafe {
            let frame = PhysFrame::<Size4KiB>::containing_address(PhysAddr::new(phys));
            let page  = Page::<Size4KiB>::containing_address(VirtAddr::new(phys));
            if let Ok(tlb) = mapper.map_to(page, frame, flags, frame_allocator) {
                tlb.flush();
            }
        }

        let ver = unsafe { read_reg(phys, REG_VER) };
        let id  = unsafe { read_reg(phys, REG_ID) } >> 24;
        let apic = IoApic { base: phys, gsi_base: entry.gsi_base, entries: ((ver >> 16) & 0xFF) + 1 };
        for i in 0..apic.entries {
            apic.write_rte(i, RTE_MASKED);
        }
        crate::serial_println!(
            "[IOAPIC] #{} id {} at {:#x}: GSI {}..{} (version {:#x})",
            slot, id & 0xF, phys, apic.gsi_base, apic.gsi_base + apic.entries - 1, ver & 0xFF
        );
        state.ioapics[slot] = Some(apic);
    }
    if madt.ioapics.len() > MAX_IOAPICS {
        crate::serial_println!("[IOAPIC] WARN: {} I/O APICs, only the first {} used",
            madt.ioapics.len(), MAX_IOAPICS);
    }

    // ISA IRQs map 1:1 to GSIs unless the MADT overrides them.
    for irq in 0..16u8 {
        state.isa[irq as usize] = IsaRoute { gsi: irq as u32, flags: 0 };
    }
    for o in madt.overrides.iter().filter(|o| o.bus == 0 && o.source < 16) {
        state.isa[o.source as usize] = IsaRoute { gsi: o.gsi, flags: inti_flags_to_rte(o.flags) };
        crate::serial_println!("[IOAPIC] ISA IRQ {} -> GSI {} (flags {:#x})", o.source, o.gsi, o.flags);
    }

    let dest = crate::smp::current_apic_id() as u64 & 0xFF;

    // Switch over with interrupts off so no ISR runs half under each controller.
    x86_64::instructions::interrupts::without_interrupts(|| {
        let mut pics = crate::interrupts::PICS.lock();
        let [pic1_mask, pic2_mask] = unsafe { pics.read_masks() };

        for &irq in ROUTED_ISA_IRQS.iter() {
            let route = state.isa[irq as usize];
            let Some((apic, index)) = state.ioapic_for_gsi(route.gsi) else {
                crate::serial_println!("[IOAPIC] WARN: ISA IRQ {} (GSI {}) has no I/O APIC", irq, route.gsi);
                continue;
            };
            // Carry over the 8259 mask so a driver that hasn't enabled its IRQ yet
            // (the mouse) keeps it off. IRQs 8-15 were also gated by the cascade (IRQ 2).
            let pic_masked = if irq < 8 {
                pic1_mask & (1 << irq) != 0
            } else {
                pic2_mask & (1 << (irq - 8)) != 0 || pic1_mask & (1 << 2) != 0
            };
            let vector = (crate::interrupts::PIC_1_OFFSET + irq) as u64;
            let mut rte = (dest << 56) | route.flags | vector; // fixed delivery, physical dest
            if pic_masked { rte |= RTE_MASKED; }
            apic.write_rte(index, rte);
            crate::serial_println!(
                "[IOAPIC] ISA IRQ {} -> GSI {} -> vector {} on APIC {}{}",
                irq, route.gsi, vector, dest, if pic_masked { " (masked)" } else { "" }
            );
        }

        // Retire the 8259: every line masked from here on.
        unsafe { pics.write_masks(0xFF, 0xFF); }
        ACTIVE.store(true, Ordering::Release);
    });

    crate::serial_println!("[IOAPIC] Device interrupts now delivered via the I/O APIC; 8259 masked.");
}

/// Mask or unmask a legacy ISA IRQ's redirection entry. No-op if the IRQ
/// isn't wired to an I/O APIC.
pub fn set_isa_masked(irq: u8, masked: bool) {
    if irq >= 16 { return; }
    x86_64::instructions::interrupts::without_interrupts(|| {
        let state = STATE.lock();
        let route = state.isa[irq as usize];
        if let Some((apic, index)) = state.ioapic_for_gsi(route.gsi) {
            let rte = apic.read_rte(index);
            // Never unmask an entry `init` didn't route (vector 0).
            if !masked && rte & 0xFF == 0 { return; }
            let rte = if masked { rte | RTE_MASKED } else { rte & !RTE_MASKED };
            apic.write_rte(index, rte);
        }
    });
}
