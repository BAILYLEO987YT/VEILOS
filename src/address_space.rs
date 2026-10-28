// =============================================================================
// VEILOS: per-process address spaces
// =============================================================================
//
// Every Windows process gets its own page tables. Kernel mappings are shared
// by pointing at the kernel's own tables; user mappings go into tables that
// belong to the process alone, so processes can't see each other's memory
// and two EXEs with the same preferred base (0x140000000) no longer collide.
//
// The kernel and user code both live in the low 512 GiB (PML4 slot 0): the
// kernel image, identity-mapped MMIO (framebuffer, LAPIC, IOAPIC, AHCI) and,
// on the user side, KUSER_SHARED_DATA (0x7FFE0000), PE images (0x140000000),
// DLLs and pools (0x2_0000_0000+). So sharing happens one level down there:
//
//   process PML4[1..512] = kernel PML4[1..512]     (heap, physical map, ...)
//   process PML4[0]      = a private PDPT whose 1 GiB slots the kernel uses
//                          point at the kernel's tables; the rest are private.
//
// "Kernel slots" are the ones present in the kernel's tables when the address
// space is created. User mappings must never land in one: the user pages
// would show up in the kernel and every other process.
// `memory::map_page_checked` refuses such mappings.
//
// The kernel's tables never hold user mappings: the loader maps into the
// process's tables, and syscalls run on the process's CR3.

use core::sync::atomic::{AtomicU64, Ordering};
use x86_64::registers::control::{Cr3, Cr3Flags};
use x86_64::structures::paging::{OffsetPageTable, PageTable, PageTableFlags, PhysFrame};
use x86_64::{PhysAddr, VirtAddr};

/// Physical address of the kernel's PML4 (the boot page tables).
static KERNEL_CR3: AtomicU64 = AtomicU64::new(0);

/// Record the boot page tables as the kernel's. Call once from `memory::init`.
pub fn init() {
    let (frame, _) = Cr3::read();
    KERNEL_CR3.store(frame.start_address().as_u64(), Ordering::Release);
}

pub fn kernel_cr3() -> u64 {
    KERNEL_CR3.load(Ordering::Acquire)
}

fn table(phys: u64) -> &'static mut PageTable {
    let virt = crate::memory::get_phys_mem_offset() + phys;
    unsafe { &mut *virt.as_mut_ptr::<PageTable>() }
}

/// The kernel's PDPT for PML4 slot 0, if any.
fn kernel_low_pdpt() -> Option<&'static mut PageTable> {
    let entry = &table(kernel_cr3())[0];
    entry.flags().contains(PageTableFlags::PRESENT).then(|| table(entry.addr().as_u64()))
}

/// Log which PML4 slots, and which 1 GiB slots of the low 512 GiB, the kernel
/// uses. User regions must stay out of all of them.
pub fn log_kernel_layout() {
    let pml4 = table(kernel_cr3());
    let mut slots = alloc::vec::Vec::new();
    for (i, e) in pml4.iter().enumerate() {
        if e.flags().contains(PageTableFlags::PRESENT) {
            slots.push(i);
        }
    }
    crate::serial_println!("[ASPACE] kernel PML4 slots: {:?} (512 GiB each)", slots);
    if let Some(pdpt) = kernel_low_pdpt() {
        let gib: alloc::vec::Vec<usize> = pdpt.iter().enumerate()
            .filter(|(_, e)| e.flags().contains(PageTableFlags::PRESENT))
            .map(|(i, _)| i)
            .collect();
        crate::serial_println!("[ASPACE] kernel 1 GiB slots in the low 512 GiB: {:?}", gib);
    }
}

/// True if `va` falls in a slot shared with the kernel (see module docs), in
/// which case a user mapping there would leak into every address space.
pub fn is_kernel_slot(va: u64) -> bool {
    let va = VirtAddr::new(va);
    let p4 = usize::from(va.p4_index());
    if p4 != 0 {
        return table(kernel_cr3())[p4].flags().contains(PageTableFlags::PRESENT);
    }
    match kernel_low_pdpt() {
        Some(pdpt) => pdpt[usize::from(va.p3_index())].flags().contains(PageTableFlags::PRESENT),
        None => false,
    }
}

/// Build a new address space: kernel mappings shared, nothing else mapped.
/// Returns the physical address of its PML4.
pub fn create() -> Option<u64> {
    let pml4_frame = crate::memory::allocate_frame()?;
    let pdpt_frame = match crate::memory::allocate_frame() {
        Some(f) => f,
        None => {
            crate::memory::deallocate_frame(pml4_frame);
            return None;
        }
    };
    let pml4_phys = pml4_frame.start_address().as_u64();
    let pdpt_phys = pdpt_frame.start_address().as_u64();

    let kernel = table(kernel_cr3());
    let pml4 = table(pml4_phys);
    let pdpt = table(pdpt_phys);
    pml4.zero();
    pdpt.zero();

    for i in 1..512 {
        pml4[i] = kernel[i].clone();
    }
    if let Some(kpdpt) = kernel_low_pdpt() {
        for j in 0..512 {
            pdpt[j] = kpdpt[j].clone();
        }
    }
    pml4[0].set_addr(
        PhysAddr::new(pdpt_phys),
        PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::USER_ACCESSIBLE,
    );
    Some(pml4_phys)
}

/// A mapper for the address space whose PML4 is at `cr3`. It need not be the
/// active one; pages it maps are reachable only once `cr3` is loaded.
pub fn mapper_for(cr3: u64) -> OffsetPageTable<'static> {
    unsafe { OffsetPageTable::new(table(cr3), crate::memory::get_phys_mem_offset()) }
}

/// Load `cr3` (the kernel's if 0) into CR3, unless it is already active.
pub fn activate(cr3: u64) {
    let cr3 = if cr3 == 0 { kernel_cr3() } else { cr3 };
    let (current, _) = Cr3::read();
    if current.start_address().as_u64() != cr3 {
        let frame = PhysFrame::containing_address(PhysAddr::new(cr3));
        unsafe { Cr3::write(frame, Cr3Flags::empty()) };
    }
}

pub fn active() -> u64 {
    Cr3::read().0.start_address().as_u64()
}

/// Run `f` with `cr3` loaded, then restore the previous CR3. Kernel memory
/// stays reachable throughout (it is shared).
pub fn with<R>(cr3: u64, f: impl FnOnce() -> R) -> R {
    let previous = active();
    activate(cr3);
    let r = f();
    activate(previous);
    r
}

/// Free the page-table frames private to the address space at `cr3`: its
/// PML4, its low PDPT, and every table under a slot it doesn't share with the
/// kernel. Leaf frames are not touched: the process's own teardown
/// (`loader::release_process_memory`) frees the pages it mapped. `cr3` must
/// not be active on any CPU.
pub fn destroy(cr3: u64) {
    if cr3 == 0 || cr3 == kernel_cr3() {
        return;
    }
    assert!(active() != cr3, "[ASPACE] destroying the active address space");

    let kernel = table(kernel_cr3());
    let pml4 = table(cr3);
    let mut freed = 0usize;

    // PML4[0]: the private PDPT; free whatever hangs off its private slots.
    let low = &pml4[0];
    if low.flags().contains(PageTableFlags::PRESENT) {
        let pdpt_phys = low.addr().as_u64();
        let pdpt = table(pdpt_phys);
        let kpdpt = kernel_low_pdpt();
        for j in 0..512 {
            let e = &pdpt[j];
            if !e.flags().contains(PageTableFlags::PRESENT) { continue; }
            let shared = kpdpt.as_ref().is_some_and(|k| k[j].addr() == e.addr()
                && k[j].flags().contains(PageTableFlags::PRESENT));
            if !shared && !e.flags().contains(PageTableFlags::HUGE_PAGE) {
                freed += free_subtree(e.addr().as_u64(), 2);
            }
        }
        free_table(pdpt_phys);
        freed += 1;
    }

    for i in 1..512 {
        let e = &pml4[i];
        if !e.flags().contains(PageTableFlags::PRESENT) { continue; }
        let shared = kernel[i].flags().contains(PageTableFlags::PRESENT) && kernel[i].addr() == e.addr();
        if !shared {
            freed += free_subtree(e.addr().as_u64(), 3);
        }
    }
    free_table(cr3);
    freed += 1;
    crate::serial_println!("[ASPACE] freed {} page-table frames of address space {:#x}", freed, cr3);
}

/// Free the table at `phys` (a level-`level` table: 3 = PDPT, 2 = PD, 1 = PT)
/// and every table below it. Returns how many table frames were freed.
fn free_subtree(phys: u64, level: u8) -> usize {
    let mut freed = 0;
    if level > 1 {
        let t = table(phys);
        for e in t.iter() {
            let f = e.flags();
            if f.contains(PageTableFlags::PRESENT) && !f.contains(PageTableFlags::HUGE_PAGE) {
                freed += free_subtree(e.addr().as_u64(), level - 1);
            }
        }
    }
    free_table(phys);
    freed + 1
}

fn free_table(phys: u64) {
    crate::memory::deallocate_frame(PhysFrame::containing_address(PhysAddr::new(phys)));
}
