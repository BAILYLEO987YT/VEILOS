//! Fibers, address waits and named pipes.
//!
//! A fiber is a stack and saved registers, switched entirely in user mode. The
//! TEB says which fiber is running (FiberData) and where the stack is, which
//! stack-growth code such as rustc's reads.

use crate::{check, Bool, Handle, INVALID_HANDLE_VALUE};
use crate::util::wstr;

const TEB_STACK_BASE: usize = 0x08;
const TEB_STACK_LIMIT: usize = 0x10;
const TEB_FIBER_DATA: usize = 0x20;
const DEFAULT_STACK: usize = 1 << 20;
const MEM_COMMIT_RESERVE: u32 = 0x3000;
const MEM_RELEASE: u32 = 0x8000;
const PAGE_READWRITE: u32 = 4;
const ERROR_NOT_ENOUGH_MEMORY: u32 = 8;
const ERROR_ALREADY_FIBER: u32 = 1280;
const ERROR_ALREADY_THREAD: u32 = 1281;

/// Saved by `switch_stack` on the stack; only rsp is kept here.
#[repr(C)]
pub struct Fiber {
    /// What GetFiberData returns; first, since that reads *FiberData.
    data: usize,
    rsp: usize,
    stack_base: usize,
    stack_limit: usize,
    /// VirtualAlloc'd stack, 0 for a converted thread.
    allocation: usize,
    start: usize,
}

fn teb() -> *mut u8 {
    let t: usize;
    unsafe { core::arch::asm!("mov {}, gs:[0x30]", out(reg) t, options(nostack, readonly, preserves_flags)) };
    t as *mut u8
}

fn teb_read(at: usize) -> usize {
    unsafe { core::ptr::read_unaligned(teb().add(at) as *const usize) }
}

fn teb_write(at: usize, v: usize) {
    unsafe { core::ptr::write_unaligned(teb().add(at) as *mut usize, v) };
}

fn new_fiber() -> *mut Fiber {
    unsafe { crate::mem::HeapAlloc(crate::mem::GetProcessHeap(), 8, core::mem::size_of::<Fiber>()) as *mut Fiber }
}

fn free_fiber(f: *mut Fiber) {
    unsafe { crate::mem::HeapFree(crate::mem::GetProcessHeap(), 0, f as *mut u8) };
}

#[no_mangle]
pub extern "system" fn IsThreadAFiber() -> Bool {
    (teb_read(TEB_FIBER_DATA) != 0) as Bool
}

#[no_mangle]
pub extern "system" fn GetCurrentFiber() -> usize {
    teb_read(TEB_FIBER_DATA)
}

#[no_mangle]
pub extern "system" fn ConvertThreadToFiberEx(param: usize, _flags: u32) -> usize {
    if IsThreadAFiber() != 0 {
        crate::SetLastError(ERROR_ALREADY_FIBER);
        return 0;
    }
    let f = new_fiber();
    if f.is_null() {
        crate::SetLastError(ERROR_NOT_ENOUGH_MEMORY);
        return 0;
    }
    unsafe {
        (*f).data = param;
        (*f).stack_base = teb_read(TEB_STACK_BASE);
        (*f).stack_limit = teb_read(TEB_STACK_LIMIT);
    }
    teb_write(TEB_FIBER_DATA, f as usize);
    f as usize
}

#[no_mangle]
pub extern "system" fn ConvertThreadToFiber(param: usize) -> usize {
    ConvertThreadToFiberEx(param, 0)
}

#[no_mangle]
pub extern "system" fn ConvertFiberToThread() -> Bool {
    let f = teb_read(TEB_FIBER_DATA);
    if f == 0 {
        crate::SetLastError(ERROR_ALREADY_THREAD);
        return 0;
    }
    teb_write(TEB_FIBER_DATA, 0);
    free_fiber(f as *mut Fiber);
    1
}

#[no_mangle]
pub extern "system" fn CreateFiberEx(commit: usize, reserve: usize, _flags: u32, start: usize, param: usize) -> usize {
    let size = (reserve.max(commit).max(if reserve == 0 && commit == 0 { DEFAULT_STACK } else { 0 }).max(64 << 10) + 0xFFFF) & !0xFFFF;
    let stack = unsafe { crate::mem::VirtualAlloc(0, size, MEM_COMMIT_RESERVE, PAGE_READWRITE) };
    if stack == 0 {
        crate::SetLastError(ERROR_NOT_ENOUGH_MEMORY);
        return 0;
    }
    let f = new_fiber();
    if f.is_null() {
        unsafe { crate::mem::VirtualFree(stack, 0, MEM_RELEASE) };
        crate::SetLastError(ERROR_NOT_ENOUGH_MEMORY);
        return 0;
    }
    // What `switch_stack` pops the first time, returning into `fiber_entry`.
    // After that `ret` the stack is 8 off a 16-byte boundary, as at any entry.
    let top = ((stack + size - 64) & !0xF) - 8;
    let sp = top - SAVED_BYTES;
    unsafe {
        core::ptr::write_bytes(sp as *mut u8, 0, SAVED_BYTES);
        // MXCSR at its default, and the x87 control word.
        core::ptr::write_unaligned((sp + 160) as *mut u32, 0x1F80);
        core::ptr::write_unaligned((sp + 164) as *mut u16, 0x027F);
        core::ptr::write_unaligned((sp + SAVED_BYTES - 8) as *mut usize, fiber_entry as *const () as usize);
        (*f).data = param;
        (*f).rsp = sp;
        (*f).stack_base = stack + size;
        (*f).stack_limit = stack;
        (*f).allocation = stack;
        (*f).start = start;
    }
    f as usize
}

#[no_mangle]
pub extern "system" fn CreateFiber(stack: usize, start: usize, param: usize) -> usize {
    CreateFiberEx(stack, 0, 0, start, param)
}

#[no_mangle]
pub extern "system" fn DeleteFiber(fiber: usize) {
    let f = fiber as *mut Fiber;
    // A fiber deleting itself ends its thread, as on Windows.
    if fiber == teb_read(TEB_FIBER_DATA) {
        unsafe { crate::process::ExitThread(0) };
    }
    unsafe {
        if (*f).allocation != 0 {
            crate::mem::VirtualFree((*f).allocation, 0, MEM_RELEASE);
        }
    }
    free_fiber(f);
}

#[no_mangle]
pub extern "system" fn SwitchToFiber(fiber: usize) {
    let from = teb_read(TEB_FIBER_DATA) as *mut Fiber;
    let to = fiber as *mut Fiber;
    unsafe {
        (*from).stack_base = teb_read(TEB_STACK_BASE);
        (*from).stack_limit = teb_read(TEB_STACK_LIMIT);
        teb_write(TEB_FIBER_DATA, fiber);
        teb_write(TEB_STACK_BASE, (*to).stack_base);
        teb_write(TEB_STACK_LIMIT, (*to).stack_limit);
        switch_stack(&mut (*from).rsp, (*to).rsp);
    }
}

/// The bytes `switch_stack` saves: xmm6-15 (160), mxcsr and x87 control (8),
/// padding (8), eight registers (64), and the return address it pops last (8).
const SAVED_BYTES: usize = 248;

/// Save the callee-saved state on this stack, its rsp in *save, and carry on
/// from `load`. rcx: save, rdx: load.
#[unsafe(naked)]
unsafe extern "system" fn switch_stack(save: *mut usize, load: usize) {
    core::arch::naked_asm!(
        "push r15",
        "push r14",
        "push r13",
        "push r12",
        "push rbx",
        "push rbp",
        "push rsi",
        "push rdi",
        "sub rsp, 176",
        "movups [rsp], xmm6",
        "movups [rsp + 16], xmm7",
        "movups [rsp + 32], xmm8",
        "movups [rsp + 48], xmm9",
        "movups [rsp + 64], xmm10",
        "movups [rsp + 80], xmm11",
        "movups [rsp + 96], xmm12",
        "movups [rsp + 112], xmm13",
        "movups [rsp + 128], xmm14",
        "movups [rsp + 144], xmm15",
        "stmxcsr [rsp + 160]",
        "fnstcw [rsp + 164]",
        "mov [rcx], rsp",
        "mov rsp, rdx",
        "movups xmm6, [rsp]",
        "movups xmm7, [rsp + 16]",
        "movups xmm8, [rsp + 32]",
        "movups xmm9, [rsp + 48]",
        "movups xmm10, [rsp + 64]",
        "movups xmm11, [rsp + 80]",
        "movups xmm12, [rsp + 96]",
        "movups xmm13, [rsp + 112]",
        "movups xmm14, [rsp + 128]",
        "movups xmm15, [rsp + 144]",
        "ldmxcsr [rsp + 160]",
        "fldcw [rsp + 164]",
        "add rsp, 176",
        "pop rdi",
        "pop rsi",
        "pop rbp",
        "pop rbx",
        "pop r12",
        "pop r13",
        "pop r14",
        "pop r15",
        "ret",
    )
}

/// A new fiber starts here, on its own stack.
extern "system" fn fiber_entry() -> ! {
    let f = teb_read(TEB_FIBER_DATA) as *mut Fiber;
    let start: extern "system" fn(usize) = unsafe { core::mem::transmute((*f).start) };
    start(unsafe { (*f).data });
    // Returning from a fiber's function ends the thread.
    unsafe { crate::process::ExitThread(0) };
    #[allow(clippy::empty_loop)]
    loop {}
}

// -----| address waits |-----

#[no_mangle]
pub unsafe extern "system" fn WaitOnAddress(addr: *const u8, compare: *const u8, size: usize, ms: u32) -> Bool {
    let timeout = if ms == u32::MAX { u64::MAX } else { ms as u64 };
    let s = unsafe { crate::VeilWaitAddress(addr as u64, compare as u64, size as u64, timeout) };
    if s == 0x102 {
        crate::SetLastError(1460); // ERROR_TIMEOUT
        return 0;
    }
    check(s)
}

#[no_mangle]
pub unsafe extern "system" fn WakeByAddressSingle(addr: *const u8) {
    unsafe { crate::VeilWakeAddress(addr as u64) };
}

#[no_mangle]
pub unsafe extern "system" fn WakeByAddressAll(addr: *const u8) {
    unsafe { crate::VeilWakeAddress(addr as u64) };
}

// -----| named pipes |-----

const STATUS_PIPE_CONNECTED: i32 = 0xC000_00B2u32 as i32;
const ERROR_PIPE_CONNECTED: u32 = 535;

/// What follows "\\.\pipe\", if `name` is a pipe's name.
pub fn pipe_name(name: &[u16]) -> bool {
    const PREFIX: &[u8] = b"\\\\.\\pipe\\";
    name.len() > PREFIX.len() && name.iter().zip(PREFIX).all(|(&u, &b)| u < 0x80 && (u as u8).eq_ignore_ascii_case(&b))
}

/// CreateFile on a pipe's name: a client end.
pub fn pipe_client(name: *const u16) -> Handle {
    let mut h: Handle = 0;
    let s = unsafe { crate::VeilPipe(1, name as u64, &mut h as *mut Handle as u64, 0) };
    if s != 0 {
        crate::fail(s);
        return INVALID_HANDLE_VALUE;
    }
    h
}

#[no_mangle]
pub unsafe extern "system" fn CreateNamedPipeW(name: *const u16, _open: u32, _mode: u32, _max: u32, out_size: u32, in_size: u32, _timeout: u32, _sa: usize) -> Handle {
    if !pipe_name(unsafe { wstr(name) }) {
        crate::SetLastError(123); // ERROR_INVALID_NAME
        return INVALID_HANDLE_VALUE;
    }
    let mut h: Handle = 0;
    let s = unsafe { crate::VeilPipe(0, name as u64, &mut h as *mut Handle as u64, out_size.max(in_size) as u64) };
    if s != 0 {
        crate::fail(s);
        return INVALID_HANDLE_VALUE;
    }
    h
}

#[no_mangle]
pub unsafe extern "system" fn CreateNamedPipeA(name: *const u8, open: u32, mode: u32, max: u32, out_size: u32, in_size: u32, timeout: u32, sa: usize) -> Handle {
    let w = unsafe { crate::util::widen(name) };
    unsafe { CreateNamedPipeW(w.as_ptr(), open, mode, max, out_size, in_size, timeout, sa) }
}

/// Every operation finishes before it returns, so an OVERLAPPED needs only its
/// event set.
#[no_mangle]
pub unsafe extern "system" fn ConnectNamedPipe(h: Handle, overlapped: *mut u64) -> Bool {
    let s = unsafe { crate::VeilPipe(2, h, 0, 0) };
    if !overlapped.is_null() {
        let event = unsafe { *overlapped.add(3) } & !1;
        if event != 0 {
            crate::sync::SetEvent(event);
        }
    }
    if s == STATUS_PIPE_CONNECTED {
        crate::SetLastError(ERROR_PIPE_CONNECTED);
        return 0;
    }
    check(s)
}

#[no_mangle]
pub extern "system" fn DisconnectNamedPipe(h: Handle) -> Bool {
    check(unsafe { crate::VeilPipe(3, h, 0, 0) })
}

/// Only the count of bytes waiting; a peek at the bytes isn't offered yet.
#[no_mangle]
pub unsafe extern "system" fn PeekNamedPipe(h: Handle, _buffer: *mut u8, _size: u32, read: *mut u32, avail: *mut u32, left: *mut u32) -> Bool {
    let mut n = 0u32;
    let s = unsafe { crate::VeilPipe(4, h, &mut n as *mut u32 as u64, 0) };
    if s != 0 {
        return crate::fail(s);
    }
    for (p, v) in [(read, 0), (avail, n), (left, 0)] {
        if !p.is_null() {
            unsafe { *p = v };
        }
    }
    1
}

#[no_mangle]
pub unsafe extern "system" fn WaitNamedPipeW(name: *const u16, ms: u32) -> Bool {
    // NMPWAIT_USE_DEFAULT_WAIT is 0, NMPWAIT_WAIT_FOREVER is -1.
    let timeout = match ms {
        0 => 50,
        u32::MAX => u64::MAX,
        n => n as u64,
    };
    check(unsafe { crate::VeilPipe(5, name as u64, timeout, 0) })
}

#[no_mangle]
pub unsafe extern "system" fn WaitNamedPipeA(name: *const u8, ms: u32) -> Bool {
    let w = unsafe { crate::util::widen(name) };
    unsafe { WaitNamedPipeW(w.as_ptr(), ms) }
}

/// All pipes are byte streams that block: the mode is accepted.
#[no_mangle]
pub extern "system" fn SetNamedPipeHandleState(_h: Handle, _mode: *const u32, _max: *const u32, _timeout: *const u32) -> Bool {
    1
}
