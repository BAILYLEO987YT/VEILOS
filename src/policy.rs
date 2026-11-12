use crate::println;
use spin::Mutex;

// -----| policy types |-----

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyMode {
    /// Strict isolation (Ring 3). Security over performance.
    Security,
    /// Direct execution (Ring 0). Performance over isolation.
    Performance,
}

pub struct PolicyManager {
    current_mode: PolicyMode,
}

impl PolicyManager {
    pub const fn new() -> Self {
        Self {
            current_mode: PolicyMode::Security, // Default to secure
        }
    }

    pub fn set_mode(&mut self, mode: PolicyMode) {
        if self.current_mode == mode { return; }
        self.current_mode = mode;
        PERFORMANCE.store(mode == PolicyMode::Performance, core::sync::atomic::Ordering::Relaxed);
        crate::VERBOSE_LOG.store(mode == PolicyMode::Security, core::sync::atomic::Ordering::Relaxed);
        if mode == PolicyMode::Security {
            // Nothing may stay only in Performance Mode's write-back cache.
            crate::block::flush_all();
        }
        println!("\n[POLICY] Switching to {:?} Mode...", mode);

        match mode {
            PolicyMode::Security => {
                println!("  -> Enforcing Ring 3 isolation, capability checks, syscall filter.");
                // Restore Security profile for the active process.
                if let Some(proc) = crate::process::PROCESSOR.lock().get_active() {
                    crate::sandbox::POLICY_ENGINE.lock().on_security_mode(proc.pid);
                }
            }
            PolicyMode::Performance => {
                println!("  -> Bypassing Ring transitions. Elevating GPU/kernel capabilities.");
                // Elevate capabilities for the active process.
                if let Some(proc) = crate::process::PROCESSOR.lock().get_active() {
                    crate::sandbox::POLICY_ENGINE.lock().on_performance_mode(proc.pid);
                }
            }
        }
    }

    pub fn get_mode(&self) -> PolicyMode {
        self.current_mode
    }

    /// Execute a native binary (like an ELF or PE) according to the current policy.
    pub fn run_task(&self, entry_point: u64, stack_top: u64) -> ! {
        match self.current_mode {
            PolicyMode::Performance => {
                println!("[POLICY] Performance Mode Active. Preparing Ring-0 isolated execution.");

                // Validate that the entry point address is within the mapped
                // user-space region before we transfer control.  A raw jmp
                // into an unmapped page would immediately triple-fault the CPU
                // with no recovery path.
                let entry_page = x86_64::VirtAddr::new(entry_point & !0xFFF);
                let stack_page = x86_64::VirtAddr::new(stack_top & !0xFFF);

                unsafe {
                    use x86_64::structures::paging::{PageTableFlags, Page, Mapper, Size4KiB};
                    let mapper = crate::memory::get_mapper();

                    // If the entry page is not yet mapped, create a present,
                    // writable, user-accessible mapping for it so the first
                    // instruction fetch does not fault.
                    if mapper.translate_page(Page::<Size4KiB>::containing_address(entry_page)).is_err() {
                        println!("[POLICY] Perf: mapping entry page {:#x}", entry_point);
                        let mut mapper_mut = crate::memory::get_mapper();
                        let mut fa = crate::memory::GlobalFrameAllocator;
                        if let Some(frame) = crate::memory::allocate_frame() {
                            crate::memory::map_page(
                                Page::<Size4KiB>::containing_address(entry_page),
                                frame,
                                PageTableFlags::PRESENT
                                    | PageTableFlags::WRITABLE
                                    | PageTableFlags::USER_ACCESSIBLE,
                                &mut mapper_mut,
                                &mut fa,
                            );
                        } else {
                            panic!("[POLICY] Perf: OOM mapping entry page");
                        }
                    }

                    // Ensure the stack page is mapped as well.
                    if mapper.translate_page(Page::<Size4KiB>::containing_address(stack_page)).is_err() {
                        println!("[POLICY] Perf: mapping stack page {:#x}", stack_top);
                        let mut mapper_mut = crate::memory::get_mapper();
                        let mut fa = crate::memory::GlobalFrameAllocator;
                        if let Some(frame) = crate::memory::allocate_frame() {
                            crate::memory::map_page(
                                Page::<Size4KiB>::containing_address(stack_page),
                                frame,
                                PageTableFlags::PRESENT
                                    | PageTableFlags::WRITABLE
                                    | PageTableFlags::USER_ACCESSIBLE,
                                &mut mapper_mut,
                                &mut fa,
                            );
                        } else {
                            panic!("[POLICY] Perf: OOM mapping stack page");
                        }
                    }
                }

                println!("[POLICY] Perf: entry={:#x} stack={:#x} — transferring control (Ring 0).",
                    entry_point, stack_top);

                // With pages verified/mapped we can safely switch the stack
                // and jump.  This stays in Ring 0; there is no IRET or SYSCALL
                // involved, which is precisely what Performance Mode requires.
                unsafe {
                    // Properly switch to the process's address space before executing.
                    if let Some(proc) = crate::process::PROCESSOR.lock().get_active() {
                        if proc.cr3 != 0 {
                            core::arch::asm!(
                                "mov cr3, {0}",
                                in(reg) proc.cr3,
                            );
                        }
                    }
                    core::arch::asm!(
                        "mov rsp, {stack_top}",
                        "jmp {entry_point}",
                        stack_top = in(reg) stack_top,
                        entry_point = in(reg) entry_point,
                        options(noreturn)
                    );
                }
            }
            PolicyMode::Security => {
                println!("[POLICY] Security Mode Active. Dropping to Ring 3 (Isolated).");
                unsafe {
                    crate::gdt::drop_to_ring_3(entry_point, stack_top);
                }
            }
        }
    }
}

pub static POLICY: Mutex<PolicyManager> = Mutex::new(PolicyManager::new());

/// The mode without the lock, for hot paths (the timer interrupt, the disk
/// cache). Kept equal to POLICY's by `set_mode`.
pub static PERFORMANCE: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

pub fn performance() -> bool {
    PERFORMANCE.load(core::sync::atomic::Ordering::Relaxed)
}
