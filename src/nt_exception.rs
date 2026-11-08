// VEILOS: CPU exceptions in Windows programs become Windows exceptions
// (WIN32_DESIGN.md, W5b).
//
// A fault in ring 3 (divide error, breakpoint, invalid opcode, protection or
// page fault, x87/SIMD floating point, alignment check) no longer ends the
// process here. The entry stubs below save every register; `deliver` writes
// a CONTEXT and an EXCEPTION_RECORD onto the thread's user stack, with a
// machine frame (faulting RIP/RSP) above them, and returns to user mode in
// ntdll's KiUserExceptionDispatcher, which dispatches the exception to the
// program's handlers and resumes or ends the process itself:
//
//   user RSP ->  +0x000  home space (0x20)
//                +0x020  CONTEXT (0x4D0)
//                +0x4F0  EXCEPTION_RECORD (0x98, padded to 0xA0)
//                +0x590  machine frame: RIP, CS, RFLAGS, RSP, SS
//
// Faults in ring 0 go to the old handlers unchanged, as does a fault the
// process can't take (no dispatcher, or no room on its stack): it ends.

use crate::interrupts;
use core::sync::atomic::Ordering;

/// What the entry stubs saved, lowest address first.
#[repr(C)]
pub struct Trap {
    r15: u64,
    r14: u64,
    r13: u64,
    r12: u64,
    r11: u64,
    r10: u64,
    r9: u64,
    r8: u64,
    rdi: u64,
    rsi: u64,
    rbp: u64,
    rbx: u64,
    rdx: u64,
    rcx: u64,
    rax: u64,
    vector: u64,
    error: u64,
    rip: u64,
    cs: u64,
    rflags: u64,
    rsp: u64,
    ss: u64,
}

macro_rules! entry {
    ($name:ident, $vector:expr, $old:path, no_error) => {
        #[unsafe(naked)]
        pub unsafe extern "C" fn $name() {
            core::arch::naked_asm!(
                "test qword ptr [rsp + 8], 3", // CS of the interrupted code
                "jz {old}",
                "push 0",
                "push {vector}",
                "jmp {common}",
                old = sym $old,
                vector = const $vector,
                common = sym common_entry,
            )
        }
    };
    ($name:ident, $vector:expr, $old:path, error) => {
        #[unsafe(naked)]
        pub unsafe extern "C" fn $name() {
            core::arch::naked_asm!(
                "test qword ptr [rsp + 16], 3",
                "jz {old}",
                "push {vector}",
                "jmp {common}",
                old = sym $old,
                vector = const $vector,
                common = sym common_entry,
            )
        }
    };
}

entry!(divide_error, 0, interrupts::divide_error_handler, no_error);
entry!(breakpoint, 3, interrupts::breakpoint_handler, no_error);
entry!(overflow, 4, interrupts::overflow_handler, no_error);
entry!(invalid_opcode, 6, interrupts::invalid_opcode_handler, no_error);
entry!(general_protection, 13, interrupts::general_protection_fault_handler, error);
entry!(page_fault, 14, interrupts::page_fault_handler, error);
entry!(x87_floating_point, 16, interrupts::x87_fp_handler, no_error);
entry!(alignment_check, 17, interrupts::alignment_check_handler, error);
entry!(simd_floating_point, 19, interrupts::simd_fp_handler, no_error);

/// Save the general registers and the FPU/SSE state (the kernel's own code
/// may use SSE), call `deliver`, and return to user mode with the frame it
/// changed. Entered with the stack 16-byte aligned (the CPU aligns it for
/// the interrupt frame, then 7 more pushes plus 15 here).
#[unsafe(naked)]
unsafe extern "C" fn common_entry() {
    core::arch::naked_asm!(
        "push rax",
        "push rcx",
        "push rdx",
        "push rbx",
        "push rbp",
        "push rsi",
        "push rdi",
        "push r8",
        "push r9",
        "push r10",
        "push r11",
        "push r12",
        "push r13",
        "push r14",
        "push r15",
        "sub rsp, 512",
        "fxsave [rsp]",
        "mov rdi, rsp",
        "lea rsi, [rsp + 512]",
        "call {deliver}",
        "fxrstor [rsp]",
        "add rsp, 512",
        "pop r15",
        "pop r14",
        "pop r13",
        "pop r12",
        "pop r11",
        "pop r10",
        "pop r9",
        "pop r8",
        "pop rdi",
        "pop rsi",
        "pop rbp",
        "pop rbx",
        "pop rdx",
        "pop rcx",
        "pop rax",
        "add rsp, 16", // vector, error code
        "iretq",
        deliver = sym deliver,
    )
}

const STATUS_ACCESS_VIOLATION: u32 = 0xC000_0005;
const STATUS_BREAKPOINT: u32 = 0x8000_0003;
const STATUS_DATATYPE_MISALIGNMENT: u32 = 0x8000_0002;
const STATUS_ILLEGAL_INSTRUCTION: u32 = 0xC000_001D;
const STATUS_PRIVILEGED_INSTRUCTION: u32 = 0xC000_0096;
const STATUS_INTEGER_DIVIDE_BY_ZERO: u32 = 0xC000_0094;
const STATUS_INTEGER_OVERFLOW: u32 = 0xC000_0095;
const STATUS_FLOAT_INVALID_OPERATION: u32 = 0xC000_0090;
const STATUS_FLOAT_DIVIDE_BY_ZERO: u32 = 0xC000_008E;
const STATUS_FLOAT_OVERFLOW: u32 = 0xC000_0091;
const STATUS_FLOAT_UNDERFLOW: u32 = 0xC000_0093;
const STATUS_FLOAT_INEXACT_RESULT: u32 = 0xC000_008F;
const STATUS_FLOAT_DENORMAL_OPERAND: u32 = 0xC000_008D;
const STATUS_FLOAT_MULTIPLE_TRAPS: u32 = 0xC000_02B5;

const CONTEXT: u64 = 0x20;
const RECORD: u64 = 0x4F0;
const MACHINE_FRAME: u64 = 0x590;
const FRAME_SIZE: u64 = 0x5B8;

/// The exception code and parameters for a fault.
fn describe(t: &Trap, fx: &[u8; 512]) -> (u32, u64, [u64; 2], u32) {
    let rip = t.rip;
    match t.vector {
        0 => (STATUS_INTEGER_DIVIDE_BY_ZERO, rip, [0; 2], 0),
        3 => (STATUS_BREAKPOINT, rip - 1, [0; 2], 1),
        4 => (STATUS_INTEGER_OVERFLOW, rip, [0; 2], 0),
        6 => (STATUS_ILLEGAL_INSTRUCTION, rip, [0; 2], 0),
        13 => {
            // Instructions only ring 0 may run raise #GP(0) too.
            let privileged = crate::memory::user_ptr_ok(rip, 1)
                && matches!(unsafe { *(rip as *const u8) }, 0xF4 | 0xFA | 0xFB | 0xE4..=0xE7 | 0xEC..=0xEF | 0x6C..=0x6F);
            if privileged {
                (STATUS_PRIVILEGED_INSTRUCTION, rip, [0; 2], 0)
            } else {
                (STATUS_ACCESS_VIOLATION, rip, [0, u64::MAX], 2)
            }
        }
        14 => {
            let cr2 = x86_64::registers::control::Cr2::read().as_u64();
            // Read 0, write 1, execute (DEP) 8.
            let kind = if t.error & 0x10 != 0 {
                8
            } else if t.error & 0x2 != 0 {
                1
            } else {
                0
            };
            (STATUS_ACCESS_VIOLATION, rip, [kind, cr2], 2)
        }
        16 => (STATUS_FLOAT_INVALID_OPERATION, rip, [0; 2], 0),
        17 => (STATUS_DATATYPE_MISALIGNMENT, rip, [0; 2], 0),
        19 => {
            // The first unmasked exception flag in MXCSR.
            let mxcsr = u32::from_le_bytes(fx[24..28].try_into().unwrap());
            let raised = mxcsr & 0x3F & !(mxcsr >> 7);
            let code = match raised.trailing_zeros() {
                0 => STATUS_FLOAT_INVALID_OPERATION,
                1 => STATUS_FLOAT_DENORMAL_OPERAND,
                2 => STATUS_FLOAT_DIVIDE_BY_ZERO,
                3 => STATUS_FLOAT_OVERFLOW,
                4 => STATUS_FLOAT_UNDERFLOW,
                5 => STATUS_FLOAT_INEXACT_RESULT,
                _ => STATUS_FLOAT_MULTIPLE_TRAPS,
            };
            (code, rip, [0; 2], 0)
        }
        _ => (STATUS_ILLEGAL_INSTRUCTION, rip, [0; 2], 0),
    }
}

/// Send the fault in `t` to the process's KiUserExceptionDispatcher by
/// rewriting the interrupt frame. Ends the process if it can't take it.
extern "C" fn deliver(fx: &[u8; 512], t: &mut Trap) {
    // A page of an image or view not read from its file yet: try again.
    if t.vector == 14 && t.error & 1 == 0 && crate::nt_demand::fault_in(x86_64::registers::control::Cr2::read_raw()) {
        return;
    }
    let proc = crate::process::PROCESSOR.lock().get_active();
    // A thread's stack growing past what's committed: commit down to it.
    if t.vector == 14 && t.error & 1 == 0 {
        if let Some(p) = &proc {
            if grow_stack(p, x86_64::registers::control::Cr2::read_raw()) {
                return;
            }
        }
    }
    let dispatcher = proc.as_ref().map_or(0, |p| p.exception_dispatcher.load(Ordering::Acquire));
    let (code, address, params, nparams) = describe(t, fx);
    let pid = proc.as_ref().map_or(0, |p| p.pid);

    let base = (t.rsp.wrapping_sub(FRAME_SIZE)) & !0xF;
    if dispatcher == 0 || !crate::memory::user_ptr_ok(base, FRAME_SIZE as usize) {
        crate::serial_println!(
            "[EXCEPTION] pid {} {:#010x} at {:#x}: no room on its stack (rsp {:#x}) — ending it",
            pid, code, address, t.rsp
        );
        if let Some(p) = proc {
            p.terminate(code as i32);
        }
        interrupts::USER_PROCESS_ACTIVE.store(false, Ordering::Relaxed);
        unsafe { crate::loader::return_from_usermode() }
    }
    crate::serial_println!("[EXCEPTION] pid {} {:#010x} at {:#x} -> dispatcher", pid, code, address);
    let top = if crate::memory::user_ptr_ok(t.rsp, 8) { unsafe { *(t.rsp as *const u64) } } else { 0 };
    crate::serial_println!(
        "[EXCEPTION]   data {:#x} error {:#x} rsp {:#x} [rsp] {:#x} rax {:#x} rcx {:#x} rdx {:#x} r8 {:#x} r11 {:#x}",
        params[1], t.error, t.rsp, top, t.rax, t.rcx, t.rdx, t.r8, t.r11
    );
    // Likely return addresses (in an image) further up the stack.
    {
        let mut trail = alloc::string::String::new();
        for i in 1..2048 {
            if !crate::memory::user_ptr_ok(t.rsp + i * 8, 8) {
                break;
            }
            let v = unsafe { *((t.rsp + i * 8) as *const u64) };
            if (0x1_4000_0000..0x1_5000_0000).contains(&v) || (0x2_0000_0000..0x3_0000_0000).contains(&v) {
                trail.push_str(&alloc::format!(" {:#x}", v));
            }
        }
        crate::serial_println!("[EXCEPTION]   stack:{}", trail);
        crate::serial_println!(
            "[EXCEPTION]   rbx {:#x} rbp {:#x} rsi {:#x} rdi {:#x} r9 {:#x} r10 {:#x} r12 {:#x} r13 {:#x} r14 {:#x} r15 {:#x}",
            t.rbx, t.rbp, t.rsi, t.rdi, t.r9, t.r10, t.r12, t.r13, t.r14, t.r15
        );
        // The words at rsp as they are, for what the trail leaves out.
        let mut raw = alloc::string::String::new();
        for i in 0..24 {
            if !crate::memory::user_ptr_ok(t.rsp + i * 8, 8) {
                break;
            }
            raw.push_str(&alloc::format!(" {:x}", unsafe { *((t.rsp + i * 8) as *const u64) }));
        }
        crate::serial_println!("[EXCEPTION]   raw:{}", raw);
        if crate::memory::user_ptr_ok(t.rip, 16) {
            let code = unsafe { core::slice::from_raw_parts(t.rip as *const u8, 16) };
            crate::serial_println!("[EXCEPTION]   code at rip: {:02x?}", code);
        }
    }

    unsafe {
        core::ptr::write_bytes(base as *mut u8, 0, FRAME_SIZE as usize);
        let c = base + CONTEXT;
        let w64 = |off: u64, v: u64| core::ptr::write_unaligned((c + off) as *mut u64, v);
        let w32 = |off: u64, v: u32| core::ptr::write_unaligned((c + off) as *mut u32, v);
        let w16 = |off: u64, v: u16| core::ptr::write_unaligned((c + off) as *mut u16, v);
        w32(0x30, 0x10_001F); // CONTEXT_ALL: control, integer, segments, floating point
        w32(0x34, u32::from_le_bytes(fx[24..28].try_into().unwrap()));
        w16(0x38, t.cs as u16);
        for off in [0x3A, 0x3C, 0x3E, 0x40, 0x42] {
            w16(off, t.ss as u16);
        }
        w32(0x44, t.rflags as u32);
        let gprs = [t.rax, t.rcx, t.rdx, t.rbx, t.rsp, t.rbp, t.rsi, t.rdi, t.r8, t.r9, t.r10, t.r11, t.r12, t.r13, t.r14, t.r15];
        for (n, v) in gprs.iter().enumerate() {
            w64(0x78 + 8 * n as u64, *v);
        }
        w64(0xF8, address); // a breakpoint reports the int3 itself
        core::ptr::copy_nonoverlapping(fx.as_ptr(), (c + 0x100) as *mut u8, 512);

        let r = base + RECORD;
        core::ptr::write_unaligned(r as *mut u32, code);
        core::ptr::write_unaligned((r + 0x10) as *mut u64, address);
        core::ptr::write_unaligned((r + 0x18) as *mut u32, nparams);
        core::ptr::write_unaligned((r + 0x20) as *mut u64, params[0]);
        core::ptr::write_unaligned((r + 0x28) as *mut u64, params[1]);

        let m = base + MACHINE_FRAME;
        for (i, v) in [address, t.cs, t.rflags, t.rsp, t.ss].iter().enumerate() {
            core::ptr::write_unaligned((m + 8 * i as u64) as *mut u64, *v);
        }
    }
    // Back to user mode in the dispatcher, single-stepping off.
    t.rip = dispatcher;
    t.rsp = base;
    t.rflags &= !0x100;
}

/// Windows grows a thread's stack as it's touched: its reservation runs from
/// TEB.DeallocationStack up to TEB.StackBase, and a touch below what's
/// committed (TEB.StackLimit) commits the pages down to it. Programs that
/// make their own stacks (Cygwin moves its main thread to one) rely on it.
/// True if `addr` was in the running thread's stack and is now there.
fn grow_stack(proc: &alloc::sync::Arc<crate::process::Process>, addr: u64) -> bool {
    let tid = unsafe { (*crate::kthread::current_user()).tid };
    let teb = proc
        .threads
        .lock()
        .iter()
        .find(|t| t.tid == tid)
        .map(|t| t.teb)
        .unwrap_or_else(|| proc.nt_env.lock().teb_base);
    if teb == 0 || !crate::memory::user_ptr_ok(teb, 0x1480) {
        return false;
    }
    let read = |o: u64| unsafe { core::ptr::read_volatile((teb + o) as *const u64) };
    let (base, limit, dealloc) = (read(0x08), read(0x10), read(0x1478));
    if dealloc == 0 || addr < dealloc || addr >= limit || limit > base {
        return false;
    }
    let first = addr & !0xFFF;
    let size = limit - first;
    let committed = proc.vad.lock().virtual_alloc(first, size as usize, 0x1000, 0x04).is_ok();
    if committed {
        unsafe { core::ptr::write_volatile((teb + 0x10) as *mut u64, first) };
    }
    committed
}
