use lazy_static::lazy_static;
use x86_64::structures::gdt::{Descriptor, GlobalDescriptorTable, SegmentSelector};
use x86_64::structures::tss::TaskStateSegment;
use x86_64::VirtAddr;

pub const DOUBLE_FAULT_IST_INDEX: u16 = 0;
pub const PAGE_FAULT_IST_INDEX:   u16 = 1;
pub const GPF_IST_INDEX:          u16 = 2;

// ── Per-CPU IST stacks ────────────────────────────────────────────────
//
// Each CPU needs its own IST / RSP0 stacks so that exception handlers on
// different CPUs don't trample each other's stacks.  We allocate a static
// array for up to MAX_CPUS CPUs.

const IST_STACK_SIZE:  usize = 4096 * 5; // 20 KiB — matches original
const RSP0_STACK_SIZE: usize = 4096 * 5;

/// One set of exception stacks for one CPU.
#[repr(C, align(16))]
struct CpuExceptionStacks {
    double_fault: [u8; IST_STACK_SIZE],
    page_fault:   [u8; IST_STACK_SIZE],
    gpf:          [u8; IST_STACK_SIZE],
    rsp0:         [u8; RSP0_STACK_SIZE],
}

// Wrapped in UnsafeCell so this is unambiguously mutable static storage.
//
// The previous declaration was a plain `static` initialised with a `const {
// ... }` block.  A zero-initialised, never-mutated-through-Rust static like
// that is exactly the shape LLVM is allowed to place in `.rodata` /
// `.bss`-as-readonly, depending on section merging.  We WRITE into these
// stacks at boot (implicitly, by using them as a stack) and the bootloader
// must map the segment writable; putting them behind UnsafeCell removes any
// ambiguity for the compiler and makes the intent explicit for anyone
// reading this later. If the ELF segment carrying .bss is ever mapped
// read-only, the first exception on that CPU triple-faults instead of
// running the handler — this is cheap insurance against that class of bug.
struct ExceptionStackStorage(core::cell::UnsafeCell<[CpuExceptionStacks; crate::smp::MAX_CPUS]>);
unsafe impl Sync for ExceptionStackStorage {}

static CPU_EXCEPTION_STACKS: ExceptionStackStorage = ExceptionStackStorage(core::cell::UnsafeCell::new(
    [const { CpuExceptionStacks {
        double_fault: [0u8; IST_STACK_SIZE],
        page_fault:   [0u8; IST_STACK_SIZE],
        gpf:          [0u8; IST_STACK_SIZE],
        rsp0:         [0u8; RSP0_STACK_SIZE],
    } }; crate::smp::MAX_CPUS]
));

/// Get a `'static` reference to CPU `cpu`'s exception stacks.
///
/// SAFETY: each CPU's slice is touched only by that CPU (during its own
/// `make_tss`/`init_ap`, and implicitly as its own IST/RSP0 stack at runtime),
/// so there is no cross-CPU mutation.
fn stacks_for(cpu: usize) -> &'static CpuExceptionStacks {
    unsafe { &(*CPU_EXCEPTION_STACKS.0.get())[cpu] }
}

/// One GDT + one TSS per CPU, stored in a static array.
/// The TSS holds pointers into `CPU_EXCEPTION_STACKS` for the matching CPU.
/// We can't use lazy_static for an array of mutable structs, so we use
/// a manually-initialised static with `UnsafeCell` under a spin lock.
struct PerCpuGdt {
    gdt: GlobalDescriptorTable,
    tss: TaskStateSegment,
    selectors: Option<Selectors>,
}

use spin::Mutex;
// Safety: PerCpuGdt is only ever accessed through init_ap() which is called
// exactly once per CPU before that CPU loads it, so there is no concurrent
// mutation.
static PER_CPU_GDT: [Mutex<PerCpuGdt>; crate::smp::MAX_CPUS] =
    [const { Mutex::new(PerCpuGdt {
        gdt: GlobalDescriptorTable::new(),
        tss: TaskStateSegment::new(),
        selectors: None,
    }) }; crate::smp::MAX_CPUS];

lazy_static! {
    static ref TSS: TaskStateSegment = make_tss(0);
}

fn make_tss(cpu: usize) -> TaskStateSegment {
    let stacks = stacks_for(cpu);

    let mut tss = TaskStateSegment::new();

    tss.interrupt_stack_table[DOUBLE_FAULT_IST_INDEX as usize] = {
        let start = VirtAddr::from_ptr(stacks.double_fault.as_ptr());
        start + IST_STACK_SIZE as u64
    };
    tss.interrupt_stack_table[PAGE_FAULT_IST_INDEX as usize] = {
        let start = VirtAddr::from_ptr(stacks.page_fault.as_ptr());
        start + IST_STACK_SIZE as u64
    };
    tss.interrupt_stack_table[GPF_IST_INDEX as usize] = {
        let start = VirtAddr::from_ptr(stacks.gpf.as_ptr());
        start + IST_STACK_SIZE as u64
    };
    tss.privilege_stack_table[0] = {
        let start = VirtAddr::from_ptr(stacks.rsp0.as_ptr());
        start + RSP0_STACK_SIZE as u64
    };
    tss
}

lazy_static! {
    static ref GDT: (GlobalDescriptorTable, Selectors) = {
        let mut gdt = GlobalDescriptorTable::new();
        let kernel_code_selector = gdt.add_entry(Descriptor::kernel_code_segment());
        let kernel_data_selector = gdt.add_entry(Descriptor::kernel_data_segment());
        let user_data_selector   = gdt.add_entry(Descriptor::user_data_segment());
        let user_code_selector   = gdt.add_entry(Descriptor::user_code_segment());
        let tss_selector         = gdt.add_entry(Descriptor::tss_segment(&TSS));
        (
            gdt,
            Selectors {
                kernel_code_selector,
                kernel_data_selector,
                user_code_selector,
                user_data_selector,
                tss_selector,
            },
        )
    };
}

pub struct Selectors {
    pub kernel_code_selector: SegmentSelector,
    pub kernel_data_selector: SegmentSelector,
    pub user_code_selector:   SegmentSelector,
    pub user_data_selector:   SegmentSelector,
    pub tss_selector:         SegmentSelector,
}

pub fn get_selectors() -> &'static Selectors {
    &GDT.1
}

/// Load the BSP (CPU 0) GDT and TSS.
pub fn init() {
    use x86_64::instructions::tables::load_tss;
    use x86_64::instructions::segmentation::{Segment, CS, DS, ES, SS};

    let selectors = get_selectors();
    GDT.0.load();
    unsafe {
        CS::set_reg(selectors.kernel_code_selector);
        DS::set_reg(selectors.kernel_data_selector);
        ES::set_reg(selectors.kernel_data_selector);
        SS::set_reg(selectors.kernel_data_selector);
        load_tss(selectors.tss_selector);
    }
    // Set up the BSP's per-CPU block (KERNEL_GS_BASE → block, for swapgs).
    crate::smp::init_percpu(0);
}

/// Load a per-AP GDT and TSS for `cpu_index`.
///
/// This must be called on the AP itself (not the BSP) because `lgdt` and
/// `ltr` affect the currently-executing CPU's GDTR/TR registers.
pub fn init_ap(cpu_index: usize) {
    use x86_64::instructions::tables::load_tss;
    use x86_64::instructions::segmentation::{Segment, CS, DS, ES, SS};

    assert!(cpu_index < crate::smp::MAX_CPUS, "CPU index out of range");

    let mut guard = PER_CPU_GDT[cpu_index].lock();

    // Build a fresh TSS pointing at this CPU's private stacks.
    guard.tss = make_tss(cpu_index);

    // Rebuild the GDT with a TSS descriptor pointing at the new TSS.
    // SAFETY: We hold the mutex; no other code touches this slot.
    let tss_ref: &TaskStateSegment = unsafe {
        // We need a 'static reference for Descriptor::tss_segment().
        // The TSS lives inside the Mutex which is itself static, so
        // the effective lifetime is 'static — we just need to extend it.
        &*(&guard.tss as *const TaskStateSegment)
    };

    guard.gdt = GlobalDescriptorTable::new();
    let kernel_cs = guard.gdt.add_entry(Descriptor::kernel_code_segment());
    let kernel_ds = guard.gdt.add_entry(Descriptor::kernel_data_segment());
    let user_ds   = guard.gdt.add_entry(Descriptor::user_data_segment());
    let user_cs   = guard.gdt.add_entry(Descriptor::user_code_segment());
    let tss_sel   = guard.gdt.add_entry(Descriptor::tss_segment(tss_ref));

    guard.selectors = Some(Selectors {
        kernel_code_selector: kernel_cs,
        kernel_data_selector: kernel_ds,
        user_code_selector:   user_cs,
        user_data_selector:   user_ds,
        tss_selector:         tss_sel,
    });

    // Load — this executes on the AP.
    // SAFETY: The GDT and TSS are in static storage with 'static lifetime.
    unsafe {
        guard.gdt.load_unsafe();
        CS::set_reg(kernel_cs);
        DS::set_reg(kernel_ds);
        ES::set_reg(kernel_ds);
        SS::set_reg(kernel_ds);
        load_tss(tss_sel);
    }
    // Set up this AP's per-CPU block (KERNEL_GS_BASE → block, for swapgs).
    crate::smp::init_percpu(cpu_index);
}

/// Force the CPU to transition from Ring 0 (Kernel) to Ring 3 (User).
/// This is used in Security Mode to ensure code is isolated.
pub unsafe fn drop_to_ring_3(entry_point: u64, stack_top: u64) -> ! {
    let selectors = get_selectors();
    
    // We use iretq to drop to user mode. This requires pushing the 
    // destination state onto the stack and calling 'iretq'.
    // Pushes: SS, RSP, RFLAGS, CS, RIP
    unsafe {
        core::arch::asm!(
            "cli",                      // Disable interrupts
            "push {user_data_selector}",// SS
            "push {stack_top}",         // RSP
            "pushfq",                   // RFLAGS (current)
            "pop rax",
            "or rax, 0x200",            // Set Interrupt Enable bit in RFLAGS
            "push rax",
            "push {user_code_selector}",// CS
            "push {entry_point}",       // RIP
            "iretq",                    // Drop to Ring 3!
            user_data_selector = in(reg) u64::from(selectors.user_data_selector.0),
            user_code_selector = in(reg) u64::from(selectors.user_code_selector.0),
            stack_top = in(reg) stack_top,
            entry_point = in(reg) entry_point,
            options(noreturn)
        );
    }
}
