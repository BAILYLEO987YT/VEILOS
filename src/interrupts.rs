use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrame, PageFaultErrorCode};
use lazy_static::lazy_static;
use crate::{println, gdt};
use pic8259::ChainedPics;
use spin;
use alloc::vec::Vec;
use core::task::Waker;

// -----| PIC (Programmable Interrupt Controller) setup |-----

// Hardware interrupts are offset by 32 to avoid colliding with CPU exceptions (0-31)
pub const PIC_1_OFFSET: u8 = 32;
pub const PIC_2_OFFSET: u8 = PIC_1_OFFSET + 8;

pub static PICS: spin::Mutex<ChainedPics> =
    spin::Mutex::new(unsafe { ChainedPics::new(PIC_1_OFFSET, PIC_2_OFFSET) });

// Name the hardware interrupts for easier mapping
#[derive(Debug, Clone, Copy)]
#[repr(u8)]
pub enum InterruptIndex {
    Timer = PIC_1_OFFSET,
    Keyboard,
    Mouse = PIC_2_OFFSET + 4,
}

impl InterruptIndex {
    fn as_u8(self) -> u8 {
        self as u8
    }

    fn as_usize(self) -> usize {
        usize::from(self.as_u8())
    }
}

/// IDT vector for the per-AP LAPIC timer (programmed in `smp::ap_entry`).
///
/// APs must NOT share vector 32 with the BSP's PIT: that handler bumps the global `TICKS`
/// clock, runs the scheduler tick and acks the legacy PIC. On an AP it would also never send
/// a LAPIC EOI, so the AP would take one timer interrupt and then never receive another.
/// This vector's handler only EOIs the local APIC; its whole job is to wake a parked AP from
/// `hlt` so it re-checks `smp::AP_WORK_QUEUE`.
pub const AP_TIMER_VECTOR: u8 = 0x40;

/// Vector the LAPIC raises for spurious interrupts (low byte of the SVR, see smp.rs).
pub const SPURIOUS_VECTOR: u8 = 0xFF;

// -----| IDT (Interrupt Descriptor Table) setup |-----

lazy_static! {
    static ref IDT: InterruptDescriptorTable = {
        let mut idt = InterruptDescriptorTable::new();
        
        // ── CPU exception handlers ────────────────────────────────────────────
        // Every vector gets a handler so a missing IDT entry never silently
        // cascades into a #GP → #DF.  Unknown vectors print the vector number
        // and the faulting RIP so we can identify them instantly.
        idt.divide_error.set_handler_fn(divide_error_handler);
        idt.invalid_opcode.set_handler_fn(invalid_opcode_handler);
        idt.device_not_available.set_handler_fn(device_not_available_handler);
        idt.breakpoint.set_handler_fn(breakpoint_handler);
        idt.overflow.set_handler_fn(overflow_handler);
        idt.bound_range_exceeded.set_handler_fn(bound_range_handler);
        idt.invalid_tss.set_handler_fn(invalid_tss_handler);
        idt.segment_not_present.set_handler_fn(segment_not_present_handler);
        idt.stack_segment_fault.set_handler_fn(stack_segment_fault_handler);
        idt.alignment_check.set_handler_fn(alignment_check_handler);
        idt.x87_floating_point.set_handler_fn(x87_fp_handler);
        idt.simd_floating_point.set_handler_fn(simd_fp_handler);

        // #GP and #PF get dedicated IST stacks so a deep call stack cannot
        // overflow the fault-handler's stack and cause a secondary fault.
        unsafe {
            idt.page_fault
                .set_handler_fn(page_fault_handler)
                .set_stack_index(gdt::PAGE_FAULT_IST_INDEX);
            idt.general_protection_fault
                .set_handler_fn(general_protection_fault_handler)
                .set_stack_index(gdt::GPF_IST_INDEX);
            idt.double_fault
                .set_handler_fn(double_fault_handler)
                .set_stack_index(gdt::DOUBLE_FAULT_IST_INDEX);
        }
        
        // ── Hardware interrupt handlers ───────────────────────────────────────
        idt[InterruptIndex::Timer.as_usize()]
            .set_handler_fn(timer_interrupt_handler);
        idt[InterruptIndex::Keyboard.as_usize()]
            .set_handler_fn(keyboard_interrupt_handler);
        idt[InterruptIndex::Mouse.as_usize()]
            .set_handler_fn(mouse_interrupt_handler);

        // ── LAPIC-local interrupts (APs' timer + spurious) ────────────────────
        idt[AP_TIMER_VECTOR as usize]
            .set_handler_fn(ap_timer_interrupt_handler);
        idt[SPURIOUS_VECTOR as usize]
            .set_handler_fn(spurious_interrupt_handler);
            
        idt
    };
}

pub fn init_idt() {
    IDT.load();
    init_syscall();
}

/// Load the shared IDT on an Application Processor.
/// The IDT is built once on the BSP (via lazy_static) and is safe to load
/// on any CPU — each CPU just needs its IDTR register set to point at it.
pub fn init_idt_ap() {
    IDT.load();
}

/// Configure SYSCALL/SYSRET MSRs on an Application Processor.
/// The STAR/LSTAR/SFMASK values are identical to the BSP's — they reference
/// the same shared `syscall_entry` stub and GDT selectors.
pub fn init_syscall_ap() {
    init_syscall();
}

fn init_syscall() {
    use x86_64::registers::model_specific::{LStar, Star, SFMask, Efer, EferFlags};
    use x86_64::registers::rflags::RFlags;

    let selectors = crate::gdt::get_selectors();

    // STAR MSR layout (x86_64 crate order):
    //   bits 63:48 = SYSRET  CS base  → user_code_selector
    //   bits 47:32 = SYSCALL CS base  → kernel_code_selector
    // The crate's Star::write signature is:
    //   Star::write(sysret_cs, sysret_ss, syscall_cs, syscall_ss)
    Star::write(
        selectors.user_code_selector,
        selectors.user_data_selector,
        selectors.kernel_code_selector,
        selectors.kernel_data_selector,
    ).expect("Failed to write STAR MSR");

    // LSTAR: virtual address of our syscall entry stub.
    LStar::write(x86_64::VirtAddr::new(syscall_entry as *const () as u64));

    // SFMASK: bits set here are cleared in RFLAGS on syscall entry.
    // Clearing IF ensures interrupts are disabled until we're on the kernel stack.
    // DF must be clear because Rust/compiler-builtins memcpy (`rep movs`) assumes
    // it; TF/AC/NT are user-controlled flags that must not leak into ring 0.
    SFMask::write(
        RFlags::INTERRUPT_FLAG
            | RFlags::DIRECTION_FLAG
            | RFlags::TRAP_FLAG
            | RFlags::ALIGNMENT_CHECK
            | RFlags::NESTED_TASK,
    );

    // EFER.SCE: enable the SYSCALL/SYSRET instructions.
    unsafe {
        Efer::update(|efer| efer.insert(EferFlags::SYSTEM_CALL_EXTENSIONS));
    }
}

/// Raw assembly syscall entry point — must be `naked` to control the full prologue.
///
/// On `syscall` the CPU has:
///   • switched to kernel CS/SS (via STAR)
///   • saved user RIP → RCX, user RFLAGS → R11
///   • disabled interrupts (via SFMASK)
///   • NOT switched RSP — we are still on the user stack!
///
/// Calling convention (our IAT thunk: `mov r10, rcx; mov rax, id; syscall`):
///   RAX = syscall id, R10 = arg0, RDX = arg1, R8 = arg2, R9 = arg3.
///   Arguments 4+ are on the user stack — see `syscall_stack_arg()`.
///
/// Stack switch: `swapgs` makes GS point at this CPU's `smp::PerCpu` block
/// (KERNEL_GS_BASE). The user RSP is parked there and the kernel RSP loaded
/// from it, so no register is clobbered and the user stack is never touched
/// while in ring 0. (The previous version used R15 as scratch without saving
/// it — R15 is non-volatile in the Windows x64 ABI, so every syscall
/// corrupted it — and pushed R12/R13 onto the user-supplied RSP, letting a
/// bad RSP make the kernel write anywhere.)
///
/// Interrupts stay disabled for the whole syscall. Re-enabling them here is
/// not safe yet: the timer ISR paints the desktop (heap allocations) while a
/// process runs, which would deadlock against a handler holding the heap lock.
#[unsafe(no_mangle)]
#[unsafe(naked)]
pub unsafe extern "C" fn syscall_entry() {
    core::arch::naked_asm!(
        // ── switch to this CPU's syscall stack ──────────────────────────────
        "swapgs",
        "mov gs:[{user_rsp}], rsp",
        "mov rsp, gs:[{kernel_rsp}]",

        // ── save user state (16 pushes = 128 bytes; RSP stays 16-aligned) ──
        "push qword ptr gs:[{user_rsp}]", // user RSP
        "push rcx",   // user RIP
        "push r11",   // user RFLAGS
        "push rax",   // syscall id (slot discarded on return)
        "push rdi",
        "push rsi",
        "push rdx",
        "push r8",
        "push r9",
        "push r10",
        "push rbp",
        "push rbx",
        "push r12",
        "push r13",
        "push r14",
        "push r15",

        // ── save XMM6–XMM15 ─────────────────────────────────────────────────
        //
        // Callee-saved in the Windows x64 ABI but volatile in SysV, which the
        // Rust dispatcher uses — without this, any SIMD work in the kernel
        // (memcpy, formatting, ...) silently corrupts the app's XMM6–15.
        // 10 × 16 bytes keeps RSP 16-aligned for the `call` below.
        "sub rsp, 160",
        "movaps [rsp + 0*16], xmm6",
        "movaps [rsp + 1*16], xmm7",
        "movaps [rsp + 2*16], xmm8",
        "movaps [rsp + 3*16], xmm9",
        "movaps [rsp + 4*16], xmm10",
        "movaps [rsp + 5*16], xmm11",
        "movaps [rsp + 6*16], xmm12",
        "movaps [rsp + 7*16], xmm13",
        "movaps [rsp + 8*16], xmm14",
        "movaps [rsp + 9*16], xmm15",

        // ── Windows-style args → System V for syscall_dispatch ──────────────
        //   RDI=id, RSI=a0, RDX=a1 (already), RCX=a2, R8=a3
        "mov rcx, r8",
        "mov r8,  r9",
        "mov rsi, r10",
        "mov rdi, rax",
        "call {dispatch}",
        // Return value is in RAX.

        // ── restore XMM6–XMM15 ──────────────────────────────────────────────
        "movaps xmm6,  [rsp + 0*16]",
        "movaps xmm7,  [rsp + 1*16]",
        "movaps xmm8,  [rsp + 2*16]",
        "movaps xmm9,  [rsp + 3*16]",
        "movaps xmm10, [rsp + 4*16]",
        "movaps xmm11, [rsp + 5*16]",
        "movaps xmm12, [rsp + 6*16]",
        "movaps xmm13, [rsp + 7*16]",
        "movaps xmm14, [rsp + 8*16]",
        "movaps xmm15, [rsp + 9*16]",
        "add rsp, 160",

        // ── restore user registers (reverse push order) ─────────────────────
        "pop r15",
        "pop r14",
        "pop r13",
        "pop r12",
        "pop rbx",
        "pop rbp",
        "pop r10",
        "pop r9",
        "pop r8",
        "pop rdx",
        "pop rsi",
        "pop rdi",
        "add rsp, 8", // syscall id slot (RAX carries the return value)
        "pop r11",    // user RFLAGS → R11 (SYSRET loads RFLAGS from R11)
        "pop rcx",    // user RIP    → RCX (SYSRET loads RIP    from RCX)

        // SYSRET with a non-canonical RCX raises #GP *in ring 0* on Intel,
        // on the user's stack. A user RIP can only have bit 47 set if the
        // syscall instruction sat at the very top of the lower half.
        "bt rcx, 47",
        "jc 2f",

        "swapgs",
        "pop rsp",    // user RSP
        "sysretq",

        "2:",
        "and rsp, -16",
        "call {bad_return}",
        "ud2",

        user_rsp   = const crate::smp::PERCPU_USER_RSP,
        kernel_rsp = const crate::smp::PERCPU_KERNEL_RSP,
        dispatch   = sym syscall_dispatch,
        bad_return = sym syscall_bad_return,
    );
}

/// Reached instead of SYSRET when the saved user RIP is non-canonical.
/// Kills the process the same way a fault would.
extern "C" fn syscall_bad_return() -> ! {
    crate::serial_println!("[SYSCALL] non-canonical return RIP — terminating process");
    if let Some(proc) = crate::process::PROCESSOR.lock().get_active() {
        proc.terminate(-1);
    }
    USER_PROCESS_ACTIVE.store(false, core::sync::atomic::Ordering::Relaxed);
    unsafe { crate::loader::return_from_usermode() }
}

/// Read argument `index` (0-based) of the Win32 call currently being handled
/// on this CPU, for arguments the Windows x64 ABI passes on the stack
/// (`index >= 4`). Returns `None` for register arguments or an unreadable
/// stack slot.
///
/// At the `syscall` in our IAT thunk, `[rsp]` is the app's return address,
/// followed by the 32-byte shadow space, so arg 4 is at `rsp + 0x28`, arg 5
/// at `rsp + 0x30`, and so on.
pub fn syscall_stack_arg(index: usize) -> Option<u64> {
    if index < 4 { return None; }
    let rsp = crate::smp::current_syscall_user_rsp();
    let addr = rsp.checked_add(0x28 + (index as u64 - 4) * 8)?;
    if !crate::memory::user_ptr_ok(addr, 8) { return None; }
    Some(unsafe { core::ptr::read_unaligned(addr as *const u64) })
}

/// Pure Rust syscall dispatcher — called from the naked stub above with a valid
/// kernel stack.  Arguments follow Windows x64 ABI (rescued by thunk + asm):
///   id=RDI, a0=RSI, a1=RDX, a2=RCX, a3=R8  →  return value in RAX.
extern "C" fn syscall_dispatch(id: u64, a0: u64, a1: u64, a2: u64, a3: u64) -> u64 {
    // ── Universal syscall trace ──────────────────────────────────────────────
    // Every syscall from every subsystem (NT native, Win32, kernel32, etc.)
    // passes through here. This lets us see exactly what the EXE is calling
    // and in what order — including the early MSVC CRT NT calls that happen
    // before any Win32 API (CreateWindow etc.) is ever reached.
    crate::serial_println!(
        "[SYSCALL] id={:#08x}  a0={:#010x}  a1={:#010x}  a2={:#010x}  a3={:#010x}",
        id, a0, a1, a2, a3
    );

    // !! CRITICAL: Clone the Arc BEFORE releasing the lock. !!
    //
    // In Rust, temporaries in an `if let` scrutinee (e.g.
    //   `if let Some(p) = PROCESSOR.lock().get_active()`)
    // live for the ENTIRE body of the if-let — so the MutexGuard is held
    // while handle_syscall runs. Many syscall handlers (e.g. GetCurrentProcessId
    // 0x1006) call PROCESSOR.lock() themselves, which deadlocks the spin mutex.
    //
    // The fix: scope the lock to a block so it's dropped before handle_syscall.
    let proc_opt = {
        let guard = crate::process::PROCESSOR.lock();
        guard.get_active() // clones the Arc<Process> — guard drops at end of block
    };

    if let Some(proc) = proc_opt {
        let result = proc.handle_syscall(id, a0, a1, a2, a3);
        crate::serial_println!("[SYSCALL] id={:#08x}  → ret={:#010x}", id, result);
        result
    } else {
        crate::println!("\n[SYSCALL] ERROR: No active process for dispatch (id={})!", id);
        u64::MAX // Return sentinel error value
    }
}

// -----| CPU Exception Handlers |-----

// ── Helper macro: user-mode-aware exception handler ──────────────────────────
// Logs the exception to serial, terminates the user process if we were in
// ring 3, or kernel-panics if in ring 0.  Defined as a macro so the handler
// name appears in the serial log.
macro_rules! user_recoverable_handler {
    ($name:literal, $frame:expr) => {{
        let frame = $frame;
        let in_user = (frame.code_segment & 0x3) == 3;
        // A syscall handler runs at CPL=0 (kernel) even though it's doing work
        // ON BEHALF OF a currently-launched user process — e.g. dereferencing a
        // garbage pointer a Win32 app passed to a syscall.  The CPL check alone
        // misclassifies that as "a kernel bug" and used to fall through to the
        // fatal branch below, which never re-enables interrupts (IF stays 0
        // from SFMASK/the exception gate) and loops on `hlt` forever — a total,
        // unrecoverable freeze triggered by nothing worse than a bad pointer
        // from user-mode code we don't control.
        //
        // `has_exec_context()` tells us whether THIS cpu currently has a live
        // process to unwind back to, regardless of which privilege level was
        // executing at the instant of the fault. If so, this is recoverable:
        // kill just that process and keep the kernel running.
        let recoverable = in_user || crate::loader::has_exec_context();
        crate::serial_println!(
            "[EXCEPTION] {} at RIP={:#X}  CS={:#X}  RSP={:#X}  user={}  recoverable={}",
            $name,
            frame.instruction_pointer.as_u64(),
            frame.code_segment,
            frame.stack_pointer.as_u64(),
            in_user, recoverable
        );
        if recoverable {
            if let Some(proc) = crate::process::PROCESSOR.lock().get_active() {
                proc.terminate(-1);
            }
            USER_PROCESS_ACTIVE.store(false, core::sync::atomic::Ordering::Relaxed);
            // Longjmp back to the kernel context that launched the process.
            // This skips the faulting instruction entirely and resumes the
            // kernel after launch_into_usermode(). release_all_for_cpu() inside
            // return_from_usermode() force-releases any lock (WIN32_USER, etc.)
            // the syscall handler was holding at the moment it faulted.
            unsafe { crate::loader::return_from_usermode() }
        }
        // No active process to unwind to at all — this really is a kernel bug.
        panic!("[EXCEPTION] {} with no active process! RIP={:#X}", $name, frame.instruction_pointer.as_u64());
    }};
}

// ── #DE — Division by Zero (vector 0) ────────────────────────────────────────
extern "x86-interrupt" fn divide_error_handler(frame: InterruptStackFrame) {
    user_recoverable_handler!("#DE divide-by-zero", frame);
}

// ── #UD — Invalid Opcode (vector 6) ──────────────────────────────────────────
// Fires when the CPU encounters an illegal or unsupported instruction.
// Common cause: SSE/AVX used while CR4.OSFXSR=0, or ISA feature not present.
extern "x86-interrupt" fn invalid_opcode_handler(frame: InterruptStackFrame) {
    // Dump the bytes at RIP so we can identify the faulting instruction —
    // but only if RIP is actually mapped.  The old code did a blind
    // core::ptr::read here, which is exactly the "nested kernel page fault"
    // footgun that was deliberately removed from the page fault handler
    // below: if #UD fired because we jumped to an unmapped/bad address,
    // reading 8 bytes from it double-faults instead of logging cleanly.
    let rip = frame.instruction_pointer.as_u64();
    if crate::memory::is_mapped(rip) {
        let bytes: [u8; 8] = unsafe { core::ptr::read_unaligned(rip as *const [u8; 8]) };
        crate::serial_println!(
            "[#UD] RIP={:#X}  bytes: {:02x} {:02x} {:02x} {:02x} {:02x} {:02x} {:02x} {:02x}",
            rip,
            bytes[0], bytes[1], bytes[2], bytes[3],
            bytes[4], bytes[5], bytes[6], bytes[7]
        );
    } else {
        crate::serial_println!("[#UD] RIP={:#X} is not mapped — cannot dump bytes", rip);
    }
    user_recoverable_handler!("#UD invalid-opcode", frame);
}

// ── #NM — Device Not Available (vector 7) ────────────────────────────────────
// Fires when an x87/MMX/SSE/AVX instruction executes and CR0.TS=1 (task
// switched) or CR0.EM=1 (emulate).  On a bare-metal kernel that never sets
// these bits the handler should rarely fire, but if it does we need to
// clear CR0.TS and/or initialise the FPU state for the current context.
// For now: clear CR0.TS so execution can continue.
extern "x86-interrupt" fn device_not_available_handler(frame: InterruptStackFrame) {
    let in_user = (frame.code_segment & 0x3) == 3;
    crate::serial_println!(
        "[EXCEPTION] #NM device-not-available at RIP={:#X}  user={}",
        frame.instruction_pointer.as_u64(), in_user
    );
    // Clear CR0.TS so FPU/SSE instructions are allowed again.
    unsafe {
        core::arch::asm!(
            "mov rax, cr0",
            "and rax, ~0x8",   // clear bit 3 (TS)
            "mov cr0, rax",
            out("rax") _,
            options(nostack)
        );
    }
    // Also run FNINIT to put the x87 FPU into a known state.
    unsafe { core::arch::asm!("fninit", options(nostack, nomem)); }
    // If we were in user mode, let execution resume (the faulting instruction
    // will be retried now that TS is clear).  If kernel mode, this is fatal.
    if !in_user {
        panic!("#NM in kernel mode at RIP={:#X}", frame.instruction_pointer.as_u64());
    }
    // Return → CPU re-executes the faulting FPU/SSE instruction.
}

// ── #OF — Overflow (vector 4) ────────────────────────────────────────────────
extern "x86-interrupt" fn overflow_handler(frame: InterruptStackFrame) {
    user_recoverable_handler!("#OF overflow", frame);
}

// ── #BR — Bound Range Exceeded (vector 5) ────────────────────────────────────
extern "x86-interrupt" fn bound_range_handler(frame: InterruptStackFrame) {
    user_recoverable_handler!("#BR bound-range", frame);
}

// ── #TS — Invalid TSS (vector 10) ────────────────────────────────────────────
extern "x86-interrupt" fn invalid_tss_handler(frame: InterruptStackFrame, error: u64) {
    crate::serial_println!("[EXCEPTION] #TS invalid-TSS  error={:#X}  RIP={:#X}",
        error, frame.instruction_pointer.as_u64());
    panic!("#TS invalid TSS");
}

// ── #NP — Segment Not Present (vector 11) ────────────────────────────────────
extern "x86-interrupt" fn segment_not_present_handler(frame: InterruptStackFrame, error: u64) {
    let in_user = (frame.code_segment & 0x3) == 3;
    // See user_recoverable_handler!: a syscall handler running at CPL=0 on
    // behalf of an active process must still be recoverable, not just a
    // direct ring-3 fault, or any bad pointer passed to a syscall permanently
    // freezes the kernel instead of just killing the offending process.
    let recoverable = in_user || crate::loader::has_exec_context();
    crate::serial_println!("[EXCEPTION] #NP segment-not-present  error={:#X}  RIP={:#X}  user={}  recoverable={}",
        error, frame.instruction_pointer.as_u64(), in_user, recoverable);
    if recoverable {
        if let Some(proc) = crate::process::PROCESSOR.lock().get_active() {
            proc.terminate(-1);
        }
        USER_PROCESS_ACTIVE.store(false, Ordering::Relaxed);
        // Previously this just `return`ed, which resumes the interrupted
        // ring-3 instruction — the CPU immediately re-raises #NP on the same
        // segment and loops forever (with interrupts re-enabled by iretq,
        // this pegs a core at 100% rather than crashing outright, which is
        // arguably worse: it looks like a hang, not a crash).  Unwind back
        // to the launcher instead, exactly like the other user faults.
        unsafe { crate::loader::return_from_usermode() }
    }
    panic!("#NP with no active process! RIP={:#X}", frame.instruction_pointer.as_u64());
}

// ── #SS — Stack Segment Fault (vector 12) ────────────────────────────────────
// In 64-bit mode this fires mainly when RSP becomes non-canonical during a
// stack operation.  Much rarer than in 32-bit mode, but still needs a handler
// so a user-mode stack fault doesn't escalate to #DF.
extern "x86-interrupt" fn stack_segment_fault_handler(frame: InterruptStackFrame, error: u64) {
    let in_user = (frame.code_segment & 0x3) == 3;
    let recoverable = in_user || crate::loader::has_exec_context();
    crate::serial_println!("[EXCEPTION] #SS stack-fault  error={:#X}  RIP={:#X}  RSP={:#X}  user={}  recoverable={}",
        error, frame.instruction_pointer.as_u64(), frame.stack_pointer.as_u64(), in_user, recoverable);
    if recoverable {
        if let Some(proc) = crate::process::PROCESSOR.lock().get_active() {
            proc.terminate(-1);
        }
        USER_PROCESS_ACTIVE.store(false, Ordering::Relaxed);
        // See segment_not_present_handler: `return` here resumes the same
        // faulting instruction and loops forever instead of terminating.
        unsafe { crate::loader::return_from_usermode() }
    }
    panic!("#SS with no active process! RIP={:#X}", frame.instruction_pointer.as_u64());
}

// ── #AC — Alignment Check (vector 17) ────────────────────────────────────────
// Only fires in ring 3 when CR0.AM=1 and EFLAGS.AC=1.  Typically not enabled
// but handle it defensively.
extern "x86-interrupt" fn alignment_check_handler(frame: InterruptStackFrame, _error: u64) {
    user_recoverable_handler!("#AC alignment-check", frame);
}

// ── #MF — x87 Floating Point Exception (vector 16) ───────────────────────────
extern "x86-interrupt" fn x87_fp_handler(frame: InterruptStackFrame) {
    user_recoverable_handler!("#MF x87-fp-exception", frame);
}

// ── #XM/#XF — SIMD Floating Point Exception (vector 19) ─────────────────────
extern "x86-interrupt" fn simd_fp_handler(frame: InterruptStackFrame) {
    let in_user = (frame.code_segment & 0x3) == 3;
    let mxcsr: u32;
    unsafe {
        // stmxcsr stores the MXCSR register to a 4-byte memory location.
        core::arch::asm!("stmxcsr [rsp - 8]", "mov {:e}, [rsp - 8]",
            out(reg) mxcsr, options(nostack));
    }
    crate::serial_println!("[EXCEPTION] #XM SIMD-fp  MXCSR={:#010X}  RIP={:#X}  user={}",
        mxcsr, frame.instruction_pointer.as_u64(), in_user);
    let recoverable = in_user || crate::loader::has_exec_context();
    if recoverable {
        if let Some(proc) = crate::process::PROCESSOR.lock().get_active() {
            proc.terminate(-1);
        }
        USER_PROCESS_ACTIVE.store(false, Ordering::Relaxed);
        unsafe { crate::loader::return_from_usermode() }
    }
    panic!("#XM with no active process! RIP={:#X}", frame.instruction_pointer.as_u64());
}

extern "x86-interrupt" fn breakpoint_handler(
    stack_frame: InterruptStackFrame)
{
    crate::serial_println!("EXCEPTION: BREAKPOINT\n{:#?}", stack_frame);
    println!("EXCEPTION: BREAKPOINT\n{:#?}", stack_frame);

}

// Catches crashes that fail to catch themselves (e.g., stack overflows)
extern "x86-interrupt" fn double_fault_handler(
    stack_frame: InterruptStackFrame, _error_code: u64) -> ! 
{
    // A double fault means a fault fired and the fault handler itself faulted.
    // Common causes: kernel stack overflow, bad IDT entry, or a page fault
    // in the page fault handler (e.g. handler code itself accessed unmapped mem).
    // If this fires, the NEXT fault would be a TRIPLE FAULT (silent CPU reset).
    crate::serial_println!("\n!!! EXCEPTION: DOUBLE FAULT !!!");
    crate::serial_println!("  RIP = {:#X}", stack_frame.instruction_pointer.as_u64());
    crate::serial_println!("  RSP = {:#X}", stack_frame.stack_pointer.as_u64());
    crate::serial_println!("  CS  = {:#X}", stack_frame.code_segment);
    crate::serial_println!("  USER_PROCESS_ACTIVE = {}",
        USER_PROCESS_ACTIVE.load(core::sync::atomic::Ordering::Relaxed));
    crate::serial_println!("{:#?}", stack_frame);
    panic!("\nEXCEPTION: DOUBLE FAULT\n{:#?}", stack_frame);
}

// Catches memory access violations (Segfaults)
extern "x86-interrupt" fn page_fault_handler(
    stack_frame: InterruptStackFrame, error_code: PageFaultErrorCode)
{
    use x86_64::registers::control::Cr2;

    let faulting_addr = Cr2::read();
    let in_user_mode = (stack_frame.code_segment & 0x3) == 3;
    // See user_recoverable_handler! in this file: a syscall handler runs at
    // CPL=0 even while it's dereferencing a pointer a user-mode Win32 app
    // passed in. The OLD code treated every CPL=0 page fault as an
    // unrecoverable kernel bug, which fell through to `loop { hlt() }` with
    // interrupts still disabled (SFMASK/exception-gate cleared IF on entry
    // and nothing here ever re-enabled it) — a genuinely permanent freeze
    // triggered by nothing worse than a bad pointer from an app we don't
    // control. has_exec_context() tells us whether there is a live process to
    // unwind back to on THIS cpu, regardless of the privilege level that was
    // executing when the fault happened.
    let recoverable = in_user_mode || crate::loader::has_exec_context();

    crate::serial_println!("EXCEPTION: PAGE FAULT");
    crate::serial_println!("Accessed Address: {:?}", faulting_addr);
    crate::serial_println!("Error Code: {:?}", error_code);
    crate::serial_println!("User Mode: {}  Recoverable: {}", in_user_mode, recoverable);
    crate::serial_println!("{:#?}", stack_frame);

    if recoverable {
        // User-mode fault: log, terminate the process, and return to the
        // interrupted context so the timer ISR keeps firing and the display
        // stays live. The faulting instruction will re-execute and fault again,
        // but USER_PROCESS_ACTIVE=false means the timer ISR stops calling
        // user-mode paint code, and the desktop remains responsive.
        //
        // The serial log above gives us the crash address (RIP) and the
        // faulting address (CR2) so we can diagnose what the process was
        // trying to access.
        crate::serial_println!("[FAULT] PAGE FAULT at RIP={:#X} accessing {:#X?}  (in_user_mode={})",
            stack_frame.instruction_pointer.as_u64(), faulting_addr, in_user_mode);
        
        let rip = stack_frame.instruction_pointer.as_u64();
        // Fallback to read bytes if possible (avoid double fault if RIP itself is unmapped)
        crate::serial_println!("[FAULT] Error code bits — PROTECTION_VIOLATION={} WRITE={} USER={}",
            error_code.contains(PageFaultErrorCode::PROTECTION_VIOLATION),
            error_code.contains(PageFaultErrorCode::CAUSED_BY_WRITE),
            error_code.contains(PageFaultErrorCode::USER_MODE));
        let _ = rip;

        if let Some(proc) = crate::process::PROCESSOR.lock().get_active() {
            proc.terminate(-1);
        }
        USER_PROCESS_ACTIVE.store(false, core::sync::atomic::Ordering::Relaxed);

        // Longjmp back to the kernel context that launched the process.
        // release_all_for_cpu() inside return_from_usermode() force-releases
        // any lock (WIN32_USER, etc.) the syscall handler was holding when it
        // faulted, which is exactly the case a bad-pointer syscall argument
        // produces.
        unsafe { crate::loader::return_from_usermode() }
    }

    // No active process at all — this really is a kernel bug. Re-enable
    // interrupts before parking so the timer/mouse ISRs keep the cursor and
    // clock alive — the difference between a debuggable "something is dead"
    // state and a totally unresponsive black box.
    println!("EXCEPTION: PAGE FAULT (no active process — fatal)");
    println!("Accessed Address: {:?}", faulting_addr);
    println!("Error Code: {:?}", error_code);
    println!("{:#?}", stack_frame);
    unsafe { core::arch::asm!("sti"); }
    loop { x86_64_hlt(); }
}

// Catches privilege violations (like executing data or invalid segments)
extern "x86-interrupt" fn general_protection_fault_handler(
    stack_frame: InterruptStackFrame, error_code: u64)
{
    let in_user_mode = (stack_frame.code_segment & 0x3) == 3;
    let recoverable = in_user_mode || crate::loader::has_exec_context();

    crate::serial_println!("EXCEPTION: GENERAL PROTECTION FAULT");
    crate::serial_println!("Error Code: {:#X}", error_code);
    crate::serial_println!("User Mode: {}  Recoverable: {}", in_user_mode, recoverable);
    crate::serial_println!("{:#?}", stack_frame);

    if recoverable {
        crate::serial_println!("[FAULT] GPF at RIP={:#X} (in_user_mode={}) — terminating active process.",
            stack_frame.instruction_pointer.as_u64(), in_user_mode);
        // Error code for a segment selector violation encodes the selector
        // index in bits 15:3 — log it so we can see what segment was bad.
        if error_code != 0 {
            crate::serial_println!("[FAULT] Segment selector error: TI={} IDX={} EXT={}",
                (error_code >> 2) & 1, (error_code >> 3) & 0x1FFF, error_code & 1);
        }

        if let Some(proc) = crate::process::PROCESSOR.lock().get_active() {
            proc.terminate(-1);
        }
        USER_PROCESS_ACTIVE.store(false, core::sync::atomic::Ordering::Relaxed);
        // Longjmp back to the kernel context that launched the process.
        unsafe { crate::loader::return_from_usermode() }
    }

    // No active process at all — this really is a kernel bug. Re-enable
    // interrupts before parking — see the page fault handler for why.
    println!("EXCEPTION: GENERAL PROTECTION FAULT (no active process — fatal)");
    println!("Error Code: {:?}", error_code);
    println!("{:#?}", stack_frame);
    unsafe { core::arch::asm!("sti"); }
    loop { x86_64_hlt(); }
}

#[inline(always)]
fn x86_64_hlt() {
    unsafe {
        core::arch::asm!("hlt");
    }
}

// -----| Hardware Interrupt Handlers |-----

use core::sync::atomic::{AtomicU64, Ordering};

/// System-wide high-precision tick counter (1kHz).
pub static TICKS: AtomicU64 = AtomicU64::new(0);

/// Set to true by the loader when a user process is launched via iretq.
/// The timer ISR uses this to decide whether to drive the display pipeline.
pub static USER_PROCESS_ACTIVE: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

/// Tick timestamp of the last timer interrupt that observed the CPU in ring 3.
/// The timer ISR continues driving the display for up to 2000 ticks (~2 s)
/// after this, covering the window where a user-mode crash drops the CPU back
/// to ring 0 and `interrupted_user_mode` would otherwise become false.
pub static LAST_USER_MODE_TICK: AtomicU64 = AtomicU64::new(0);

/// Unbounded registry of pending sleepers.
///
/// Each `SleepFuture` pushes a `(target_tick, Waker)` pair when it is first
/// polled. The timer ISR calls `try_lock` every tick; if it gets the lock it
/// drains and wakes every entry whose `target_tick ≤ current_tick`, then
/// drops the lock. If `try_lock` fails (the lock is briefly held by a task),
/// the ISR simply skips that tick — the future will be woken at most 1 ms
/// late, which is harmless.
///
/// This replaces the old fixed 8-slot design, which permanently orphaned any
/// `SleepFuture` that could not claim a slot, causing it to never resolve.
/// The new design supports an arbitrary number of concurrent sleepers bounded
/// only by heap memory, handles cancellation cleanly, and does not require
/// any slot-numbering or generation-counter bookkeeping.
static SLEEP_REGISTRY: spin::Mutex<Vec<(u64, Waker)>> =
    spin::Mutex::new(Vec::new());

pub fn init_pit() {
    use x86_64::instructions::port::Port;
    let mut command_port = Port::<u8>::new(0x43);
    let mut data_port = Port::<u8>::new(0x40);
    
    // Command: 0x36 (Channel 0, Lobyte/Hibyte, Square Wave, Binary)
    // Divisor 0x04A9 (1193) → 1193182 Hz / 1193 ≈ 1000 Hz (1ms per tick)
    unsafe {
        command_port.write(0x36);
        data_port.write(0xA9); // Low byte of 0x04A9
        data_port.write(0x04); // High byte
    }
}

pub struct SleepFuture {
    target_tick: u64,
    /// Cloned waker stored after the first successful registration into
    /// SLEEP_REGISTRY. Used in Drop to eagerly remove the stale entry so
    /// the registry does not accumulate garbage from cancelled futures.
    waker: Option<Waker>,
}

impl SleepFuture {
    pub fn new(ms: u64) -> Self {
        let current = TICKS.load(Ordering::Relaxed);
        Self { target_tick: current + ms, waker: None }
    }
}

impl Drop for SleepFuture {
    fn drop(&mut self) {
        // Eagerly remove our entry from the registry when the future is
        // cancelled (dropped before resolving). This prevents spurious
        // wakeups and keeps the Vec compact.
        //
        // If try_lock fails (the ISR holds the lock at this exact moment),
        // we skip cleanup — the stale entry will remain until the ISR next
        // tries to fire it (which it won't, because the Waker's backing task
        // has already been dropped and wake_by_ref becomes a no-op). The
        // entry is eventually evicted once the tick counter passes target_tick.
        if let Some(ref w) = self.waker {
            if let Some(mut reg) = SLEEP_REGISTRY.try_lock() {
                reg.retain(|(_, stored)| !w.will_wake(stored));
            }
        }
    }
}

impl core::future::Future for SleepFuture {
    type Output = ();
    fn poll(mut self: core::pin::Pin<&mut Self>, cx: &mut core::task::Context<'_>) -> core::task::Poll<Self::Output> {
        // Fast path: already elapsed.
        if TICKS.load(Ordering::Acquire) >= self.target_tick {
            return core::task::Poll::Ready(());
        }

        // Register our waker into the registry if not yet done.
        // We only push once per future instance (tracked via self.waker).
        // On every subsequent poll we are already in the registry and the
        // ISR will call wake_by_ref when our target_tick arrives.
        if self.waker.is_none() {
            let w = cx.waker().clone();
            match SLEEP_REGISTRY.try_lock() {
                Some(mut reg) => {
                    reg.push((self.target_tick, w.clone()));
                    self.waker = Some(w);
                }
                None => {
                    // ISR briefly holds the lock. Schedule an immediate
                    // re-poll so we retry on the next executor pass.
                    cx.waker().wake_by_ref();
                }
            }
        }

        // Double-check after registering — closes the race between the
        // TICKS increment and our waker being pushed to the registry.
        if TICKS.load(Ordering::Acquire) >= self.target_tick {
            core::task::Poll::Ready(())
        } else {
            core::task::Poll::Pending
        }
    }
}

/// Per-AP LAPIC timer tick. Deliberately does nothing but acknowledge the local APIC: no
/// TICKS, no scheduler, no PIC. Returning from this ISR is what wakes the AP out of `hlt`
/// so `park_and_wait_for_work` can poll the work queue again.
extern "x86-interrupt" fn ap_timer_interrupt_handler(_stack_frame: InterruptStackFrame) {
    crate::smp::lapic_eoi();
}

/// LAPIC spurious-interrupt vector. A spurious interrupt is not recorded in the ISR, so it
/// must NOT be acknowledged with an EOI.
extern "x86-interrupt" fn spurious_interrupt_handler(_stack_frame: InterruptStackFrame) {}

extern "x86-interrupt" fn timer_interrupt_handler(
    stack_frame: InterruptStackFrame)
{
    crate::sampler::sample(stack_frame.instruction_pointer.as_u64());
    
    TICKS.fetch_add(1, Ordering::Relaxed);
    let tick = TICKS.load(Ordering::Relaxed);

    crate::scheduler::scheduler_tick(tick);
    crate::debugger::profiler_tick(tick);

    // Wake any SleepFutures whose deadline has been reached.
    // retain() walks the Vec once, calling wake_by_ref() on expired entries
    // and removing them in-place — O(n) in the number of sleeping tasks.
    // try_lock is used so the ISR never spins: if a task briefly holds the
    // registry lock, we skip wakeups for this tick (at most 1 ms late).
    if let Some(mut reg) = SLEEP_REGISTRY.try_lock() {
        reg.retain(|(target, waker)| {
            if tick >= *target {
                waker.wake_by_ref();
                false // remove from registry
            } else {
                true  // keep waiting
            }
        });
    }

    // ── EOI sent FIRST — always, unconditionally ──────────────────────────────
    //
    // The PIC will not send ANY further interrupts (timer, keyboard, mouse) on
    // IRQ0-7 until EOI is acknowledged.  Any work below that crashes or spins
    // would permanently freeze all input if EOI were deferred to the end.
    // Sending it here is safe: IF is still 0 so we cannot be re-entered.
    unsafe {
        PICS.lock().notify_end_of_interrupt(InterruptIndex::Timer.as_u8());
    }

    // ── Optional display repaint when a user process is running ───────────────
    //
    // After iretq launches a user process the async executor is permanently
    // frozen — gui_refresh_task never runs again.  We drive the display from
    // the timer ISR instead at ~60 fps (every 16 ticks at 1 kHz).
    //
    // Safety guarantees:
    //   SYSCALL clears IF (SFMASK), so this ISR can ONLY fire while the CPU is
    //   in user mode between syscalls.  No kernel spinlock can be held by any
    //   syscall handler at this point.
    //   We use try_lock everywhere so we skip frames rather than spinning if a
    //   lock is unexpectedly taken.  EOI was already sent so a slow paint
    //   cannot freeze input.
    let interrupted_user_mode = (stack_frame.code_segment & 0x3) == 3;

    // Track the most recent tick where we observed ring-3 execution.
    if interrupted_user_mode {
        LAST_USER_MODE_TICK.store(tick, Ordering::Relaxed);
    }

    // Paint while a user process is running OR for up to 2000 ticks (~2 s)
    // after it last ran.  This grace period keeps the display live during the
    // window where a user-mode crash drops the CPU back to ring 0 (halting in
    // the fault handler), meaning `interrupted_user_mode` becomes false but we
    // still want to see the desktop repaint so the crash is visible.
    let ticks_since_user = tick.saturating_sub(LAST_USER_MODE_TICK.load(Ordering::Relaxed));
    let recently_in_user_mode = interrupted_user_mode || ticks_since_user < 2000;

    if recently_in_user_mode
        && USER_PROCESS_ACTIVE.load(Ordering::Relaxed)
        && tick % 16 == 0
    {
        let phase = crate::desktop::DesktopPhase::from_u8(
            crate::desktop::DESKTOP_PHASE.load(Ordering::Relaxed)
        );
        if phase == crate::desktop::DesktopPhase::Desktop {
            // ── ISR-safe frame render ──────────────────────────────────────────
            //
            // Every resource this block touches is acquired with try_lock so
            // the ISR NEVER spins.  A missed frame (skipped because a lock was
            // already held by a syscall handler) is entirely harmless — the
            // display updates at 60 fps anyway and the very next tick will
            // succeed.
            //
            // Lock order when a BackBufferGuard is NOT active:
            //   BACK_BUFFER → FRAMEBUFFER
            // We must not acquire them in reverse order anywhere.

            // Step 1 — try to claim the back buffer.  Skip this frame on contention.
            let bb_guard = crate::ui::BACK_BUFFER.try_lock();
            let bb_ptr = match bb_guard {
                Some(ref g) => match g.as_ref() {
                    Some(bb) => bb.as_ptr() as *mut u32,
                    None     => return, // back buffer not initialised yet
                },
                None => return, // held by a syscall — skip frame, not a problem
            };

            // Publish the pointer so draw_rect_atomic etc. use the fast path
            // (no re-lock) while we hold bb_guard.
            crate::ui::PAINTER_PTR.store(bb_ptr, Ordering::Release);
            crate::ui::PAINTER_ACTIVE.store(true, Ordering::Release);

            // Step 2 — clear background directly via the fast path.
            crate::ui::draw_rect_atomic(
                0, 0,
                crate::ui::SCREEN_W, crate::ui::SCREEN_H,
                crate::graphics::Color { r: 20, g: 20, b: 35 },
            );

            // Step 3 — full desktop paint (uses draw_rect_atomic fast path throughout).
            crate::desktop::paint_desktop(tick);

            // Step 4 — flush to hardware.  commit_back_buffer reads through
            // PAINTER_PTR (fast path), but still needs FRAMEBUFFER.
            // Use try_lock; if the framebuffer is momentarily busy, skip.
            {
                let mut fb_guard = crate::graphics::FRAMEBUFFER.try_lock();
                if let Some(ref mut fb_opt) = fb_guard {
                    if let Some(fb) = fb_opt.as_mut() {
                        // Blit the back buffer directly — fast path already populated bb_ptr.
                        let draw_w = fb.width.min(crate::ui::SCREEN_W);
                        for py in 0..fb.height.min(crate::ui::SCREEN_H) {
                            let row_dst = (fb.base_addr + (py * fb.stride) as u64 * 4) as *mut u32;
                            let src = py * crate::ui::SCREEN_W;
                            unsafe {
                                core::ptr::copy_nonoverlapping(bb_ptr.add(src), row_dst, draw_w);
                            }
                        }
                    }
                }
                // fb_guard drops here — FRAMEBUFFER released before cursor draw.
            }

            // Step 5 — tear down fast path BEFORE releasing bb_guard so no
            // window exists where PAINTER_ACTIVE=true but bb_guard is dropped.
            crate::ui::PAINTER_ACTIVE.store(false, Ordering::Release);
            crate::ui::PAINTER_PTR.store(core::ptr::null_mut(), Ordering::Relaxed);
            drop(bb_guard);

            // Step 6 — draw cursor directly on hardware (no back-buffer involvement).
            if let Some(state) = crate::task::mouse::MOUSE_STATE.try_lock() {
                let mx = state.x as usize;
                let my = state.y as usize;
                drop(state);
                if let Some(mut ui_wm) = crate::ui::WINDOW_MANAGER.try_lock() {
                    ui_wm.cursor_x = mx;
                    ui_wm.cursor_y = my;
                    ui_wm.draw_cursor_hw();
                }
            }
        }
    }
}

extern "x86-interrupt" fn keyboard_interrupt_handler(
    _stack_frame: InterruptStackFrame) 
{
    use x86_64::instructions::port::Port;

    let mut port = Port::new(0x60); // 0x60 is the PS/2 data port

    // Fast-Path: Read the raw scancode from the hardware and shove it into the Async Queue.
    // We do ZERO heavy processing in the hardware interrupt layer itself!
    let scancode: u8 = unsafe { port.read() };
    crate::task::keyboard::add_scancode(scancode);

    // Acknowledge the interrupt to the PIC immediately
    unsafe {
        PICS.lock()
            .notify_end_of_interrupt(InterruptIndex::Keyboard.as_u8());
    }
}

extern "x86-interrupt" fn mouse_interrupt_handler(_stack_frame: InterruptStackFrame) {
    use x86_64::instructions::port::Port;
    let mut port = Port::new(0x60);
    let packet: u8 = unsafe { port.read() };
    // Use add_byte_and_process instead of add_byte so MOUSE_STATE is updated
    // immediately inside the ISR. This keeps the cursor moving even when the
    // async executor is permanently frozen after iretq into a user process.
    crate::task::mouse::add_byte_and_process(packet);

    unsafe {
        PICS.lock().notify_end_of_interrupt(InterruptIndex::Mouse.as_u8());
    }
}
