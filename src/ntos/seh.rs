// Structured exception handling in drivers (__try / __except / __finally),
// as Windows x64 does it in kernel mode.
//
// Adapted from VEILOS's user-mode version (win32/ntdll/src/exception.rs and
// win32/ucrtbase/src/eh.rs), with three differences:
//
//   * Images are the loaded drivers (image::DRIVERS), whose .pdata the
//     unwinder reads. VEILOS's own kernel has no Windows unwind tables, so
//     a walk ends at the first frame outside a driver: an exception nothing
//     in the driver handles is a bug check (0x1E), as on Windows.
//   * So every unwind starts in driver code. RtlRaiseException, ExRaiseStatus,
//     ProbeForRead/Write and RtlUnwindEx are assembly stubs (`capture!`) that
//     capture their caller's registers before any Rust runs; an RtlUnwindEx
//     made from inside a dispatch (by __C_specific_handler) starts from the
//     context the exception was raised in, kept per thread.
//   * CPU faults in ring 0 whose RIP is in a driver image come here
//     (nt_exception's entry stubs send them to `kernel_fault`) instead of to
//     the crash handlers: page faults, protection faults, divide errors, ...
//
// C++ exceptions aren't supported in Windows kernel mode, so there is no
// frame handler for them here.

use alloc::{collections::BTreeMap, vec::Vec};
use core::ptr::{null_mut, read_unaligned};
use spin::Mutex;

pub const CONTEXT_SIZE: usize = 0x4D0;
const CONTEXT_FULL: u32 = 0x10_000F;

mod ctx {
    pub const fn gpr(n: usize) -> usize {
        0x78 + 8 * n
    }
    pub const RSP: usize = gpr(4);
    pub const RIP: usize = 0xF8;
    pub const EFLAGS: usize = 0x44;
    pub const FLTSAVE: usize = 0x100;
    pub const fn xmm(n: usize) -> usize {
        0x1A0 + 16 * n
    }
}

#[repr(C, align(16))]
#[derive(Clone, Copy)]
pub struct Context(pub [u8; CONTEXT_SIZE]);

impl Context {
    pub const fn zeroed() -> Self {
        Context([0; CONTEXT_SIZE])
    }
    fn get(&self, off: usize) -> u64 {
        u64::from_le_bytes(self.0[off..off + 8].try_into().unwrap())
    }
    fn set(&mut self, off: usize, v: u64) {
        self.0[off..off + 8].copy_from_slice(&v.to_le_bytes());
    }
    pub fn rip(&self) -> u64 {
        self.get(ctx::RIP)
    }
    pub fn set_rip(&mut self, v: u64) {
        self.set(ctx::RIP, v)
    }
    pub fn rsp(&self) -> u64 {
        self.get(ctx::RSP)
    }
    fn set_rsp(&mut self, v: u64) {
        self.set(ctx::RSP, v)
    }
    fn reg(&self, n: usize) -> u64 {
        self.get(ctx::gpr(n))
    }
    fn set_reg(&mut self, n: usize, v: u64) {
        self.set(ctx::gpr(n), v)
    }
    fn set_xmm(&mut self, n: usize, v: [u8; 16]) {
        self.0[ctx::xmm(n)..ctx::xmm(n) + 16].copy_from_slice(&v);
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct ExceptionRecord {
    pub code: u32,
    pub flags: u32,
    pub record: *mut ExceptionRecord,
    pub address: u64,
    pub count: u32,
    pub info: [u64; 15],
}

impl ExceptionRecord {
    fn new(code: u32, flags: u32) -> Self {
        ExceptionRecord { code, flags, record: null_mut(), address: 0, count: 0, info: [0; 15] }
    }
}

const EXCEPTION_NONCONTINUABLE: u32 = 0x1;
const EXCEPTION_UNWINDING: u32 = 0x2;
const EXCEPTION_EXIT_UNWIND: u32 = 0x4;
const EXCEPTION_TARGET_UNWIND: u32 = 0x20;

const STATUS_UNWIND: u32 = 0xC000_0027;
const STATUS_NONCONTINUABLE_EXCEPTION: u32 = 0xC000_0025;
const STATUS_INVALID_DISPOSITION: u32 = 0xC000_0026;
const STATUS_BAD_STACK: u32 = 0xC000_0028;
pub const STATUS_ACCESS_VIOLATION: u32 = 0xC000_0005;
const STATUS_DATATYPE_MISALIGNMENT: u32 = 0x8000_0002;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct RuntimeFunction {
    pub begin: u32,
    pub end: u32,
    pub unwind: u32,
}

#[repr(C)]
pub struct DispatcherContext {
    pub control_pc: u64,
    pub image_base: u64,
    pub function_entry: *const RuntimeFunction,
    pub establisher_frame: u64,
    pub target_ip: u64,
    pub context: *mut Context,
    pub language_handler: Option<Handler>,
    pub handler_data: *const u8,
    pub history_table: usize,
    pub scope_index: u32,
    pub fill: u32,
}

pub type Handler = unsafe extern "win64" fn(*mut ExceptionRecord, u64, *mut Context, *mut DispatcherContext) -> u32;

const DISPOSITION_CONTINUE_EXECUTION: u32 = 0;
const DISPOSITION_CONTINUE_SEARCH: u32 = 1;

const UNW_FLAG_EHANDLER: u32 = 1;
const UNW_FLAG_UHANDLER: u32 = 2;
const UNW_FLAG_CHAININFO: u8 = 4;

// -----| images and function tables |-----

/// The driver image (base, size) containing `pc`.
fn image_of(pc: u64) -> Option<(u64, u64)> {
    let d = super::image::DRIVERS.try_lock()?;
    d.iter().find(|d| pc >= d.base && pc < d.base + d.size).map(|d| (d.base, d.size))
}

fn exception_directory(base: u64) -> Option<(*const RuntimeFunction, usize)> {
    unsafe {
        let nt = base + read_unaligned((base + 0x3C) as *const u32) as u64;
        let optional = nt + 0x18;
        if read_unaligned(optional as *const u16) != 0x20B {
            return None;
        }
        if read_unaligned((optional + 0x6C) as *const u32) <= 3 {
            return None;
        }
        let dir = optional + 0x70 + 3 * 8;
        let rva = read_unaligned(dir as *const u32);
        let size = read_unaligned((dir + 4) as *const u32);
        if rva == 0 || size == 0 {
            return None;
        }
        Some(((base + rva as u64) as *const RuntimeFunction, size as usize / 12))
    }
}

fn search(table: *const RuntimeFunction, count: usize, rva: u32) -> *const RuntimeFunction {
    let (mut lo, mut hi) = (0usize, count);
    while lo < hi {
        let mid = (lo + hi) / 2;
        let f = unsafe { &*table.add(mid) };
        if rva < f.begin {
            hi = mid;
        } else if rva >= f.end {
            lo = mid + 1;
        } else {
            return f;
        }
    }
    core::ptr::null()
}

pub unsafe extern "win64" fn RtlLookupFunctionEntry(pc: u64, image_base: *mut u64, _history: usize) -> *const RuntimeFunction {
    let Some((base, _)) = image_of(pc) else { return core::ptr::null() };
    unsafe { *image_base = base };
    let Some((table, count)) = exception_directory(base) else { return core::ptr::null() };
    search(table, count, (pc - base) as u32)
}

pub unsafe extern "win64" fn RtlPcToFileHeader(pc: u64, base: *mut u64) -> u64 {
    let b = image_of(pc).map_or(0, |(b, _)| b);
    unsafe { *base = b };
    b
}

// -----| virtual unwinding (as ntdll's) |-----

const UWOP_PUSH_NONVOL: u8 = 0;
const UWOP_ALLOC_LARGE: u8 = 1;
const UWOP_ALLOC_SMALL: u8 = 2;
const UWOP_SET_FPREG: u8 = 3;
const UWOP_SAVE_NONVOL: u8 = 4;
const UWOP_SAVE_NONVOL_FAR: u8 = 5;
const UWOP_EPILOG: u8 = 6;
const UWOP_SPARE: u8 = 7;
const UWOP_SAVE_XMM128: u8 = 8;
const UWOP_SAVE_XMM128_FAR: u8 = 9;
const UWOP_PUSH_MACHFRAME: u8 = 10;

unsafe fn rd64(a: u64) -> u64 {
    unsafe { read_unaligned(a as *const u64) }
}

unsafe fn unwind_epilog(c: &mut Context, pc: u64, begin: u64, end: u64, frame_reg: usize) -> bool {
    let b = |i: u64| unsafe { *((pc + i) as *const u8) };
    let mut i = 0u64;
    let mut rsp = c.rsp();
    if b(0) == 0x48 && b(1) == 0x83 && b(2) == 0xC4 {
        rsp = rsp.wrapping_add(b(3) as i8 as i64 as u64);
        i = 4;
    } else if b(0) == 0x48 && b(1) == 0x81 && b(2) == 0xC4 {
        rsp = rsp.wrapping_add(unsafe { read_unaligned((pc + 3) as *const i32) } as i64 as u64);
        i = 7;
    } else if (b(0) & 0xFE) == 0x48 && b(1) == 0x8D && frame_reg != 0 {
        let modrm = b(2);
        let rm = (modrm & 7) as usize | if b(0) & 1 != 0 { 8 } else { 0 };
        if (modrm >> 3) & 7 == 4 && rm == frame_reg && rm & 7 != 4 {
            match modrm >> 6 {
                1 => {
                    rsp = c.reg(rm).wrapping_add(b(3) as i8 as i64 as u64);
                    i = 4;
                }
                2 => {
                    rsp = c.reg(rm).wrapping_add(unsafe { read_unaligned((pc + 3) as *const i32) } as i64 as u64);
                    i = 7;
                }
                _ => return false,
            }
        } else {
            return false;
        }
    }
    let mut pops = [0usize; 16];
    let mut n = 0;
    loop {
        let (rex, op) = if b(i) & 0xF0 == 0x40 { (b(i), b(i + 1)) } else { (0, b(i)) };
        if (0x58..=0x5F).contains(&op) && (rex == 0 || rex == 0x41) && n < 16 {
            pops[n] = (op - 0x58) as usize | if rex == 0x41 { 8 } else { 0 };
            n += 1;
            i += if rex != 0 { 2 } else { 1 };
        } else {
            break;
        }
    }
    let tail = match b(i) {
        0xC3 | 0xC2 => true,
        0xE9 => {
            let target = (pc + i + 5).wrapping_add(unsafe { read_unaligned((pc + i + 1) as *const i32) } as i64 as u64);
            target < begin || target >= end
        }
        0xEB => {
            let target = (pc + i + 2).wrapping_add(b(i + 1) as i8 as i64 as u64);
            target < begin || target >= end
        }
        0xFF => b(i + 1) == 0x25,
        0x48 => b(i + 1) == 0xFF && b(i + 2) == 0x25,
        _ => false,
    };
    if !tail {
        return false;
    }
    for &r in &pops[..n] {
        c.set_reg(r, unsafe { rd64(rsp) });
        rsp += 8;
    }
    c.set_rip(unsafe { rd64(rsp) });
    c.set_rsp(rsp + 8);
    true
}

unsafe fn unwind_prologue(c: &mut Context, image_base: u64, info: u64, pc_offset: u64, frame_base: u64) -> bool {
    let p = |i: u64| unsafe { *((info + i) as *const u8) };
    let count = p(2) as u64;
    let code = |k: u64| (p(4 + 2 * k), p(5 + 2 * k));
    let slot16 = |k: u64| unsafe { read_unaligned((info + 4 + 2 * k) as *const u16) } as u64;
    let slot32 = |k: u64| unsafe { read_unaligned((info + 4 + 2 * k) as *const u32) } as u64;
    let frame_reg = (p(3) & 0xF) as usize;
    let frame_offset = (p(3) >> 4) as u64 * 16;
    let mut k = 0;
    while k < count {
        let (offset, opinfo) = code(k);
        let op = opinfo & 0xF;
        let info_bits = (opinfo >> 4) as usize;
        let slots = match op {
            UWOP_ALLOC_LARGE => if info_bits == 0 { 2 } else { 3 },
            UWOP_SAVE_NONVOL | UWOP_SAVE_XMM128 => 2,
            UWOP_SAVE_NONVOL_FAR | UWOP_SAVE_XMM128_FAR | UWOP_SPARE => 3,
            UWOP_EPILOG => 2,
            _ => 1,
        };
        let done = op == UWOP_EPILOG || (offset as u64) <= pc_offset;
        if done && op != UWOP_EPILOG {
            let rsp = c.rsp();
            match op {
                UWOP_PUSH_NONVOL => {
                    c.set_reg(info_bits, unsafe { rd64(rsp) });
                    c.set_rsp(rsp + 8);
                }
                UWOP_ALLOC_LARGE => {
                    let size = if info_bits == 0 { slot16(k + 1) * 8 } else { slot32(k + 1) };
                    c.set_rsp(rsp + size);
                }
                UWOP_ALLOC_SMALL => c.set_rsp(rsp + info_bits as u64 * 8 + 8),
                UWOP_SET_FPREG => c.set_rsp(c.reg(frame_reg) - frame_offset),
                UWOP_SAVE_NONVOL => c.set_reg(info_bits, unsafe { rd64(frame_base + slot16(k + 1) * 8) }),
                UWOP_SAVE_NONVOL_FAR => c.set_reg(info_bits, unsafe { rd64(frame_base + slot32(k + 1)) }),
                UWOP_SAVE_XMM128 => c.set_xmm(info_bits, unsafe { read_unaligned((frame_base + slot16(k + 1) * 16) as *const [u8; 16]) }),
                UWOP_SAVE_XMM128_FAR => c.set_xmm(info_bits, unsafe { read_unaligned((frame_base + slot32(k + 1)) as *const [u8; 16]) }),
                UWOP_PUSH_MACHFRAME => {
                    let base = if info_bits == 1 { rsp + 8 } else { rsp };
                    c.set_rip(unsafe { rd64(base) });
                    c.set_rsp(unsafe { rd64(base + 24) });
                    return false;
                }
                _ => {}
            }
        }
        k += slots;
    }
    if p(0) >> 3 & UNW_FLAG_CHAININFO != 0 {
        let chained = info + 4 + ((count + 1) & !1) * 2;
        let primary = unsafe { read_unaligned(chained as *const RuntimeFunction) };
        return unsafe { unwind_prologue(c, image_base, image_base + primary.unwind as u64, u64::MAX, frame_base) };
    }
    true
}

fn fpreg_done(info: u64, pc_offset: u64) -> bool {
    let count = unsafe { *((info + 2) as *const u8) } as u64;
    for k in 0..count {
        let (offset, op) = unsafe { (*((info + 4 + 2 * k) as *const u8), *((info + 5 + 2 * k) as *const u8) & 0xF) };
        if op == UWOP_SET_FPREG {
            return offset as u64 <= pc_offset;
        }
    }
    false
}

pub unsafe extern "win64" fn RtlVirtualUnwind(
    kind: u32,
    image_base: u64,
    pc: u64,
    function: *const RuntimeFunction,
    c: *mut Context,
    handler_data: *mut *const u8,
    establisher: *mut u64,
    _pointers: usize,
) -> Option<Handler> {
    let c = unsafe { &mut *c };
    let f = unsafe { *function };
    let info = image_base + f.unwind as u64;
    let p = |i: u64| unsafe { *((info + i) as *const u8) };
    let flags = p(0) >> 3;
    let prolog = p(1) as u64;
    let count = p(2) as u64;
    let frame_reg = (p(3) & 0xF) as usize;
    let frame_offset = (p(3) >> 4) as u64 * 16;
    let pc_offset = pc - (image_base + f.begin as u64);

    let fp_set = frame_reg != 0 && (pc_offset >= prolog || fpreg_done(info, pc_offset));
    let frame_base = if fp_set { c.reg(frame_reg) - frame_offset } else { c.rsp() };
    unsafe { *establisher = frame_base };

    if pc_offset >= prolog && flags & UNW_FLAG_CHAININFO == 0 {
        let begin = image_base + f.begin as u64;
        let end = image_base + f.end as u64;
        if unsafe { unwind_epilog(c, pc, begin, end, if fp_set { frame_reg } else { 0 }) } {
            return None;
        }
    }
    if unsafe { unwind_prologue(c, image_base, info, pc_offset, frame_base) } {
        let rsp = c.rsp();
        c.set_rip(unsafe { rd64(rsp) });
        c.set_rsp(rsp + 8);
    }
    if flags & (kind as u8) != 0 && flags & UNW_FLAG_CHAININFO == 0 && pc_offset >= prolog {
        let after = info + 4 + ((count + 1) & !1) * 2;
        let handler = unsafe { read_unaligned(after as *const u32) } as u64;
        unsafe { *handler_data = (after + 4) as *const u8 };
        return Some(unsafe { core::mem::transmute::<u64, Handler>(image_base + handler) });
    }
    None
}

// -----| per-thread dispatch state |-----

/// Each thread's exceptions being dispatched, innermost last: the context
/// each was raised in, where an unwind made during its dispatch starts.
static DISPATCHING: Mutex<BTreeMap<u64, Vec<usize>>> = Mutex::new(BTreeMap::new());

fn with_dispatching<R>(f: impl FnOnce(&mut Vec<usize>) -> R) -> R {
    x86_64::instructions::interrupts::without_interrupts(|| {
        let mut m = DISPATCHING.lock();
        let id = crate::kthread::current_id();
        let v = m.entry(id).or_default();
        let r = f(v);
        if v.is_empty() {
            m.remove(&id);
        }
        r
    })
}

// -----| dispatching |-----

/// Offer the exception to each driver frame's handler, from the one it was
/// raised in up. True if a handler said to continue execution (with
/// `context` as it left it).
pub unsafe fn dispatch(rec: *mut ExceptionRecord, context: *mut Context) -> bool {
    with_dispatching(|v| v.push(context as usize));
    let r = unsafe { dispatch_frames(rec, context) };
    with_dispatching(|v| {
        v.pop();
    });
    r
}

unsafe fn dispatch_frames(rec: *mut ExceptionRecord, context: *mut Context) -> bool {
    let mut c = unsafe { *context };
    loop {
        let pc = c.rip();
        let rsp = c.rsp();
        if rsp & 7 != 0 || pc == 0 {
            return false;
        }
        let mut base = 0;
        let f = unsafe { RtlLookupFunctionEntry(pc, &mut base, 0) };
        if f.is_null() {
            if image_of(pc).is_none() {
                return false; // out of the driver: nobody handled it
            }
            c.set_rip(unsafe { rd64(rsp) });
            c.set_rsp(rsp + 8);
            continue;
        }
        let mut before = c;
        let (mut data, mut frame) = (core::ptr::null(), 0);
        let handler = unsafe { RtlVirtualUnwind(UNW_FLAG_EHANDLER, base, pc, f, &mut c, &mut data, &mut frame, 0) };
        let Some(handler) = handler else { continue };
        let mut dc = DispatcherContext {
            control_pc: pc,
            image_base: base,
            function_entry: f,
            establisher_frame: frame,
            target_ip: 0,
            context: &mut before,
            language_handler: Some(handler),
            handler_data: data,
            history_table: 0,
            scope_index: 0,
            fill: 0,
        };
        match unsafe { handler(rec, frame, context, &mut dc) } {
            DISPOSITION_CONTINUE_EXECUTION => {
                if unsafe { (*rec).flags } & EXCEPTION_NONCONTINUABLE != 0 {
                    bug_check(STATUS_NONCONTINUABLE_EXCEPTION, rec);
                }
                return true;
            }
            DISPOSITION_CONTINUE_SEARCH | 2 | 3 => {}
            _ => bug_check(STATUS_INVALID_DISPOSITION, rec),
        }
    }
}

/// KMODE_EXCEPTION_NOT_HANDLED: (code, address, info[0], info[1]).
fn bug_check(code: u32, rec: *mut ExceptionRecord) -> ! {
    let (addr, i0, i1) = if rec.is_null() { (0, 0, 0) } else { unsafe { ((*rec).address, (*rec).info[0], (*rec).info[1]) } };
    let first = if rec.is_null() { code } else { unsafe { (*rec).code } };
    crate::serial_println!("[NTOS] exception {:#010x} at {:#x} not handled in the driver ({:#x})", first, addr, code);
    super::ke::KeBugCheckEx(0x1E, first as u64, addr, i0, i1)
}

/// Raise `rec` from `c` (the raiser's context): dispatch it, then resume
/// where a handler said, or bug check.
unsafe fn raise_from(rec: *mut ExceptionRecord, c: *mut Context) -> ! {
    unsafe {
        if (*rec).address == 0 {
            (*rec).address = (*c).rip();
        }
        if dispatch(rec, c) {
            RtlRestoreContext(c, rec)
        }
    }
    bug_check(unsafe { (*rec).code }, rec)
}

// -----| software exceptions, from driver code |-----
//
// Each of these is entered through `capture!`, which saves the caller's
// registers into a CONTEXT (RIP: the return address; RSP: as after the
// return) and then calls the implementation with the caller's arguments
// (an array: RCX, RDX, R8, R9, then the stack ones) and that CONTEXT.

macro_rules! capture {
    ($name:ident, $imp:path) => {
        #[unsafe(naked)]
        pub unsafe extern "win64" fn $name() {
            core::arch::naked_asm!(
                "mov [rsp + 8], rcx",
                "mov [rsp + 16], rdx",
                "mov [rsp + 24], r8",
                "mov [rsp + 32], r9",
                "sub rsp, 0x4F8",
                "lea rcx, [rsp + 0x20]",
                "call {capture}",
                "lea rax, [rsp + 0x4F8 + 8]",
                "mov [rsp + 0x20 + 0x98], rax",
                "mov rax, [rsp + 0x4F8]",
                "mov [rsp + 0x20 + 0xF8], rax",
                "lea rcx, [rsp + 0x4F8 + 8]",
                "lea rdx, [rsp + 0x20]",
                "call {imp}",
                "add rsp, 0x4F8",
                "ret",
                capture = sym $crate::ntos::seh::RtlCaptureContext,
                imp = sym $imp,
            )
        }
    };
}

pub(crate) use capture;

capture!(RtlRaiseException, raise_exception_impl);
capture!(ExRaiseStatus, raise_status_impl);
capture!(RtlUnwindEx, unwind_ex_impl);
capture!(RtlUnwind, unwind_impl);
capture!(ProbeForRead, probe_read_impl);
capture!(ProbeForWrite, probe_write_impl);

extern "win64" fn raise_exception_impl(args: *const u64, c: *mut Context) {
    let rec = unsafe { *args } as *mut ExceptionRecord;
    unsafe {
        (*rec).address = (*c).rip();
        if dispatch(rec, c) {
            RtlRestoreContext(c, rec)
        }
    }
    bug_check(unsafe { (*rec).code }, rec)
}

extern "win64" fn raise_status_impl(args: *const u64, c: *mut Context) {
    let mut rec = ExceptionRecord::new(unsafe { *args } as u32, EXCEPTION_NONCONTINUABLE);
    unsafe { raise_from(&mut rec, c) }
}

/// The highest user address + 1, as drivers check it (MmUserProbeAddress).
pub static MM_USER_PROBE_ADDRESS: u64 = 0x7FFF_FFFF_0000;
pub static MM_HIGHEST_USER_ADDRESS: u64 = 0x7FFF_FFFE_FFFF;
pub static MM_SYSTEM_RANGE_START: u64 = 0xFFFF_8000_0000_0000;

/// ProbeForRead(Address, Length, Alignment): a user buffer the driver is
/// about to read: misaligned raises STATUS_DATATYPE_MISALIGNMENT, outside
/// user space STATUS_ACCESS_VIOLATION (into the driver's __try).
fn probe(args: *const u64, c: *mut Context, write: bool) {
    let (addr, len, align) = unsafe { (*args, *args.add(1), *args.add(2) as u32 as u64) };
    if len == 0 {
        return;
    }
    let code = if align != 0 && addr & (align - 1) != 0 {
        STATUS_DATATYPE_MISALIGNMENT
    } else if addr.checked_add(len).is_none_or(|end| end > MM_USER_PROBE_ADDRESS) {
        STATUS_ACCESS_VIOLATION
    } else {
        return;
    };
    let mut rec = ExceptionRecord::new(code, 0);
    if code == STATUS_ACCESS_VIOLATION {
        rec.count = 2;
        rec.info = [write as u64, MM_USER_PROBE_ADDRESS, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    }
    unsafe { raise_from(&mut rec, c) }
}

/// Raise STATUS_ACCESS_VIOLATION about `addr` from `c` (a driver's
/// context captured by `capture!`).
pub fn raise_access_violation(addr: u64, c: *mut Context) -> ! {
    let mut rec = ExceptionRecord::new(STATUS_ACCESS_VIOLATION, 0);
    rec.count = 2;
    rec.info[1] = addr;
    unsafe { raise_from(&mut rec, c) }
}

extern "win64" fn probe_read_impl(args: *const u64, c: *mut Context) {
    probe(args, c, false)
}

extern "win64" fn probe_write_impl(args: *const u64, c: *mut Context) {
    probe(args, c, true)
}

extern "win64" fn unwind_impl(args: *const u64, c: *mut Context) {
    let a = |i: usize| unsafe { *args.add(i) };
    unsafe { unwind(a(0), a(1), a(2) as *mut ExceptionRecord, a(3), null_mut(), c) }
}

/// RtlUnwindEx(TargetFrame, TargetIp, ExceptionRecord, ReturnValue,
/// OriginalContext, HistoryTable).
extern "win64" fn unwind_ex_impl(args: *const u64, c: *mut Context) {
    let a = |i: usize| unsafe { *args.add(i) };
    unsafe { unwind(a(0), a(1), a(2) as *mut ExceptionRecord, a(3), a(4) as *mut Context, c) }
}

/// Unwind to `target_frame`, running the termination handlers on the way,
/// and resume there at `target_ip` with RAX = `return_value`. From inside a
/// dispatch the walk starts where that exception was raised; otherwise from
/// the caller (`caller`, a driver frame).
unsafe fn unwind(target_frame: u64, target_ip: u64, rec: *mut ExceptionRecord, return_value: u64, original: *mut Context, caller: *mut Context) -> ! {
    let mut local = ExceptionRecord::new(STATUS_UNWIND, 0);
    let rec = if rec.is_null() { &raw mut local } else { rec };
    let start = with_dispatching(|v| v.last().copied());
    let mut c = match start {
        Some(p) => unsafe { *(p as *const Context) },
        None if caller.is_null() => bug_check(STATUS_BAD_STACK, rec),
        None => unsafe { *caller },
    };
    unsafe {
        if (*rec).address == 0 {
            (*rec).address = c.rip();
        }
        (*rec).flags |= EXCEPTION_UNWINDING;
        if target_frame == 0 {
            (*rec).flags |= EXCEPTION_EXIT_UNWIND;
        }
    }
    loop {
        let pc = c.rip();
        let rsp = c.rsp();
        let mut base = 0;
        let f = unsafe { RtlLookupFunctionEntry(pc, &mut base, 0) };
        if f.is_null() {
            if image_of(pc).is_none() {
                bug_check(STATUS_BAD_STACK, rec);
            }
            c.set_rip(unsafe { rd64(rsp) });
            c.set_rsp(rsp + 8);
            continue;
        }
        let mut before = c;
        let (mut data, mut frame) = (core::ptr::null(), 0);
        let handler = unsafe { RtlVirtualUnwind(UNW_FLAG_UHANDLER, base, pc, f, &mut c, &mut data, &mut frame, 0) };
        let at_target = frame == target_frame;
        if let Some(handler) = handler {
            if at_target {
                unsafe { (*rec).flags |= EXCEPTION_TARGET_UNWIND };
            }
            let mut dc = DispatcherContext {
                control_pc: pc,
                image_base: base,
                function_entry: f,
                establisher_frame: frame,
                target_ip,
                context: &mut before,
                language_handler: Some(handler),
                handler_data: data,
                history_table: 0,
                scope_index: 0,
                fill: 0,
            };
            let ctx_ptr = if original.is_null() { &raw mut before } else { original };
            unsafe { handler(rec, frame, ctx_ptr, &mut dc) };
            unsafe { (*rec).flags &= !EXCEPTION_TARGET_UNWIND };
        }
        if at_target {
            // The exception (if any) is over: resume in the target frame.
            if start.is_some() {
                with_dispatching(|v| {
                    v.pop();
                });
            }
            before.set_rip(target_ip);
            before.set_reg(0, return_value);
            unsafe { RtlRestoreContext(&mut before, rec) };
        }
        if frame > target_frame && target_frame != 0 {
            bug_check(STATUS_INVALID_DISPOSITION, rec);
        }
    }
}

// -----| __try / __except / __finally (the compiler's language handler) |-----

const CONTINUE_SEARCH: u32 = 1;
const CONTINUE_EXECUTION: u32 = 0;

#[repr(C)]
struct Pointers {
    record: *mut ExceptionRecord,
    context: *mut Context,
}

/// The scope table after the unwind info: count, then {begin, end, filter
/// or finally, __except target (0 for __finally)} as RVAs.
pub unsafe extern "win64" fn __C_specific_handler(rec: *mut ExceptionRecord, frame: u64, ctx: *mut Context, dc: *mut DispatcherContext) -> u32 {
    let dc = unsafe { &mut *dc };
    let base = dc.image_base;
    let pc = (dc.control_pc - base) as u32;
    let table = dc.handler_data as u64;
    let u32_at = |a: u64| unsafe { read_unaligned(a as *const u32) };
    let count = u32_at(table);
    let scope = |i: u32| {
        let s = table + 4 + 16 * i as u64;
        (u32_at(s), u32_at(s + 4), u32_at(s + 8), u32_at(s + 12))
    };
    let flags = unsafe { (*rec).flags };

    if flags & (EXCEPTION_UNWINDING | EXCEPTION_EXIT_UNWIND) == 0 {
        let mut ptrs = Pointers { record: rec, context: ctx };
        for i in dc.scope_index..count {
            let (begin, end, filter, target) = scope(i);
            if pc < begin || pc >= end || target == 0 {
                continue;
            }
            let verdict = if filter == 1 {
                1
            } else {
                let f: unsafe extern "win64" fn(*mut Pointers, u64) -> i32 = unsafe { core::mem::transmute(base + filter as u64) };
                unsafe { f(&mut ptrs, frame) }
            };
            if verdict < 0 {
                return CONTINUE_EXECUTION;
            }
            if verdict > 0 {
                // Into the __except block, with the code in EAX.
                unsafe { unwind(frame, base + target as u64, rec, (*rec).code as u64, dc.context, null_mut()) };
            }
        }
    } else {
        let target = dc.target_ip.wrapping_sub(base) as u32;
        for i in dc.scope_index..count {
            let (begin, end, handler, jump) = scope(i);
            if pc < begin || pc >= end {
                continue;
            }
            if flags & EXCEPTION_TARGET_UNWIND != 0 && target >= begin && target < end {
                break;
            }
            if jump != 0 {
                if flags & EXCEPTION_TARGET_UNWIND != 0 && target == jump {
                    break;
                }
                continue;
            }
            dc.scope_index = i + 1;
            let fin: unsafe extern "win64" fn(u8, u64) = unsafe { core::mem::transmute(base + handler as u64) };
            unsafe { fin(1, frame) };
        }
    }
    CONTINUE_SEARCH
}

// -----| CPU faults in drivers |-----

/// What nt_exception's ring-0 entry saved (see `kernel_entry` there): the
/// general registers, the FXSAVE area, then the vector, error code and the
/// interrupt frame.
#[repr(C)]
pub struct KernelTrap {
    pub fx: [u8; 512],
    pub r15: u64,
    pub r14: u64,
    pub r13: u64,
    pub r12: u64,
    pub r11: u64,
    pub r10: u64,
    pub r9: u64,
    pub r8: u64,
    pub rdi: u64,
    pub rsi: u64,
    pub rbp: u64,
    pub rbx: u64,
    pub rdx: u64,
    pub rcx: u64,
    pub rax: u64,
    pub vector: u64,
    pub error: u64,
    pub rip: u64,
    pub cs: u64,
    pub rflags: u64,
    pub rsp: u64,
    pub ss: u64,
}

/// A fault in a driver: as a Windows exception, into its handlers.
pub extern "C" fn kernel_fault(t: &KernelTrap) -> ! {
    let mut c = Context::zeroed();
    let regs = [t.rax, t.rcx, t.rdx, t.rbx, t.rsp, t.rbp, t.rsi, t.rdi, t.r8, t.r9, t.r10, t.r11, t.r12, t.r13, t.r14, t.r15];
    for (n, v) in regs.iter().enumerate() {
        c.set_reg(n, *v);
    }
    c.set_rip(t.rip);
    c.0[ctx::EFLAGS..ctx::EFLAGS + 4].copy_from_slice(&(t.rflags as u32).to_le_bytes());
    c.0[ctx::FLTSAVE..ctx::FLTSAVE + 512].copy_from_slice(&t.fx);
    c.0[0x34..0x38].copy_from_slice(&t.fx[24..28]); // MxCsr
    c.0[0x30..0x34].copy_from_slice(&CONTEXT_FULL.to_le_bytes());
    c.0[0x38..0x3A].copy_from_slice(&(t.cs as u16).to_le_bytes());
    c.0[0x42..0x44].copy_from_slice(&(t.ss as u16).to_le_bytes());

    let mut rec = ExceptionRecord::new(0, 0);
    rec.address = t.rip;
    match t.vector {
        0 => rec.code = 0xC000_0094, // STATUS_INTEGER_DIVIDE_BY_ZERO
        3 => {
            rec.code = 0x8000_0003; // STATUS_BREAKPOINT: at the int3
            rec.address = t.rip - 1;
        }
        4 => rec.code = 0xC000_0095, // STATUS_INTEGER_OVERFLOW
        6 => rec.code = 0xC000_001D, // STATUS_ILLEGAL_INSTRUCTION
        13 => {
            rec.code = STATUS_ACCESS_VIOLATION;
            rec.count = 2;
            rec.info[1] = u64::MAX;
        }
        14 => {
            let cr2: u64;
            unsafe { core::arch::asm!("mov {}, cr2", out(reg) cr2, options(nomem, nostack)) };
            rec.code = STATUS_ACCESS_VIOLATION;
            rec.count = 2;
            rec.info[0] = if t.error & 0x10 != 0 { 8 } else { (t.error >> 1) & 1 };
            rec.info[1] = cr2;
        }
        16 | 19 => rec.code = 0xC000_02B4, // STATUS_FLOAT_MULTIPLE_TRAPS
        17 => rec.code = STATUS_DATATYPE_MISALIGNMENT,
        _ => rec.code = 0xC000_001D,
    }
    crate::serial_println!("[NTOS] driver exception {:#010x} at {:#x}", rec.code, rec.address);
    // #PF and #GP arrive on IST stacks, where a second fault would start
    // over at the top. Dispatch on the driver's own stack instead, as
    // Windows does: the context and record go below its RSP (with room for
    // what the driver had there), and the dispatch runs under them.
    let below = (t.rsp - 0x200 - core::mem::size_of::<Context>() as u64 - core::mem::size_of::<ExceptionRecord>() as u64) & !0x3F;
    let cp = below as *mut Context;
    let rp = (below + core::mem::size_of::<Context>() as u64) as *mut ExceptionRecord;
    unsafe {
        cp.write(c);
        rp.write(rec);
        switch_stack(below - 0x40, fault_continue as usize as u64, cp as u64, rp as u64, t.rflags)
    }
}

/// On the driver's stack: interrupts as they were, then the dispatch.
extern "C" fn fault_continue(c: *mut Context, rec: *mut ExceptionRecord, rflags: u64) -> ! {
    if rflags & 0x200 != 0 {
        x86_64::instructions::interrupts::enable();
    }
    unsafe { raise_from(rec, c) }
}

/// Continue on another stack: RSP = `rsp`, then `f(a, b, c)` (System V).
#[unsafe(naked)]
unsafe extern "C" fn switch_stack(_rsp: u64, _f: u64, _a: u64, _b: u64, _c: u64) -> ! {
    core::arch::naked_asm!(
        "mov rsp, rdi",
        "mov rax, rsi",
        "mov rdi, rdx",
        "mov rsi, rcx",
        "mov rdx, r8",
        "call rax",
        "ud2",
    )
}

// -----| context capture and restore |-----

/// The caller's registers, as at the return from this call.
#[unsafe(naked)]
pub unsafe extern "win64" fn RtlCaptureContext(_c: *mut Context) {
    core::arch::naked_asm!(
        "mov [rcx + 0x78], rax",
        "mov [rcx + 0x80], rcx",
        "mov [rcx + 0x88], rdx",
        "mov [rcx + 0x90], rbx",
        "mov [rcx + 0xA0], rbp",
        "mov [rcx + 0xA8], rsi",
        "mov [rcx + 0xB0], rdi",
        "mov [rcx + 0xB8], r8",
        "mov [rcx + 0xC0], r9",
        "mov [rcx + 0xC8], r10",
        "mov [rcx + 0xD0], r11",
        "mov [rcx + 0xD8], r12",
        "mov [rcx + 0xE0], r13",
        "mov [rcx + 0xE8], r14",
        "mov [rcx + 0xF0], r15",
        "lea rax, [rsp + 8]",
        "mov [rcx + 0x98], rax",
        "mov rax, [rsp]",
        "mov [rcx + 0xF8], rax",
        "mov word ptr [rcx + 0x38], cs",
        "mov word ptr [rcx + 0x42], ss",
        "pushfq",
        "pop rax",
        "mov [rcx + 0x44], eax",
        "stmxcsr [rcx + 0x34]",
        "stmxcsr [rcx + 0x118]",
        "movups [rcx + 0x1A0], xmm0",
        "movups [rcx + 0x1B0], xmm1",
        "movups [rcx + 0x1C0], xmm2",
        "movups [rcx + 0x1D0], xmm3",
        "movups [rcx + 0x1E0], xmm4",
        "movups [rcx + 0x1F0], xmm5",
        "movups [rcx + 0x200], xmm6",
        "movups [rcx + 0x210], xmm7",
        "movups [rcx + 0x220], xmm8",
        "movups [rcx + 0x230], xmm9",
        "movups [rcx + 0x240], xmm10",
        "movups [rcx + 0x250], xmm11",
        "movups [rcx + 0x260], xmm12",
        "movups [rcx + 0x270], xmm13",
        "movups [rcx + 0x280], xmm14",
        "movups [rcx + 0x290], xmm15",
        "mov dword ptr [rcx + 0x30], {flags}",
        "mov rax, [rcx + 0x78]",
        "ret",
        flags = const CONTEXT_FULL,
    )
}

/// Load every register from the context and continue there (RFLAGS too, so
/// the interrupt flag is as it was when the context was taken).
#[unsafe(naked)]
pub unsafe extern "win64" fn RtlRestoreContext(_c: *mut Context, _rec: *mut ExceptionRecord) -> ! {
    core::arch::naked_asm!(
        "ldmxcsr [rcx + 0x34]",
        "movups xmm0, [rcx + 0x1A0]",
        "movups xmm1, [rcx + 0x1B0]",
        "movups xmm2, [rcx + 0x1C0]",
        "movups xmm3, [rcx + 0x1D0]",
        "movups xmm4, [rcx + 0x1E0]",
        "movups xmm5, [rcx + 0x1F0]",
        "movups xmm6, [rcx + 0x200]",
        "movups xmm7, [rcx + 0x210]",
        "movups xmm8, [rcx + 0x220]",
        "movups xmm9, [rcx + 0x230]",
        "movups xmm10, [rcx + 0x240]",
        "movups xmm11, [rcx + 0x250]",
        "movups xmm12, [rcx + 0x260]",
        "movups xmm13, [rcx + 0x270]",
        "movups xmm14, [rcx + 0x280]",
        "movups xmm15, [rcx + 0x290]",
        // RIP and RFLAGS just below the target RSP, for popfq; ret.
        "mov rax, [rcx + 0x98]",
        "sub rax, 16",
        "mov rdx, [rcx + 0xF8]",
        "mov [rax + 8], rdx",
        "mov edx, dword ptr [rcx + 0x44]",
        "mov [rax], rdx",
        "mov rdx, [rcx + 0x88]",
        "mov rbx, [rcx + 0x90]",
        "mov rbp, [rcx + 0xA0]",
        "mov rsi, [rcx + 0xA8]",
        "mov rdi, [rcx + 0xB0]",
        "mov r8, [rcx + 0xB8]",
        "mov r9, [rcx + 0xC0]",
        "mov r10, [rcx + 0xC8]",
        "mov r11, [rcx + 0xD0]",
        "mov r12, [rcx + 0xD8]",
        "mov r13, [rcx + 0xE0]",
        "mov r14, [rcx + 0xE8]",
        "mov r15, [rcx + 0xF0]",
        "mov rsp, rax",
        "mov rax, [rcx + 0x78]",
        "mov rcx, [rcx + 0x80]",
        "popfq",
        "ret",
    )
}
