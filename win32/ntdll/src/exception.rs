//! Structured exception handling, as on Windows x64 (WIN32_DESIGN.md, W5b).
//!
//! A fault in user mode arrives at `KiUserExceptionDispatcher`: the kernel
//! has put a CONTEXT and an EXCEPTION_RECORD on the thread's stack, with a
//! machine frame (the faulting RIP and RSP) above them. A software
//! exception (`RtlRaiseException`) captures the caller's context instead.
//! Either way `RtlDispatchException` offers the exception to the vectored
//! handlers and then to each frame's language handler, found through the
//! modules' unwind tables (`RtlLookupFunctionEntry`, `RtlVirtualUnwind`).
//! A handler that takes the exception calls `RtlUnwindEx`, which runs the
//! termination handlers of the frames in between and resumes at the target
//! with `RtlRestoreContext`.
//!
//! `KiUserExceptionDispatcher` and `RcConsolidateFrames` have unwind info
//! of their own (a machine frame and every non-volatile register saved in
//! the CONTEXT on the stack), so unwinding passes through them to the frame
//! below exactly as through any function. `RcConsolidateFrames` runs the
//! callback of a STATUS_UNWIND_CONSOLIDATE unwind (a C++ catch block) on the
//! current stack, looking to the unwinder as if called by the target frame.

use core::arch::{global_asm, naked_asm};
use core::ptr::{null_mut, read_unaligned};

// -----| the structures |-----

pub const CONTEXT_SIZE: usize = 0x4D0;
const CONTEXT_FULL: u32 = 0x10_000F;

/// CONTEXT offsets.
mod ctx {
    /// Integer register n (0 = RAX ... 4 = RSP ... 15 = R15), the order
    /// unwind codes number them.
    pub const fn gpr(n: usize) -> usize {
        0x78 + 8 * n
    }
    pub const RSP: usize = gpr(4);
    pub const RIP: usize = 0xF8;
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

pub const EXCEPTION_NONCONTINUABLE: u32 = 0x1;
pub const EXCEPTION_UNWINDING: u32 = 0x2;
pub const EXCEPTION_EXIT_UNWIND: u32 = 0x4;
pub const EXCEPTION_TARGET_UNWIND: u32 = 0x20;

const STATUS_UNWIND: u32 = 0xC000_0027;
const STATUS_NONCONTINUABLE_EXCEPTION: u32 = 0xC000_0025;
const STATUS_INVALID_DISPOSITION: u32 = 0xC000_0026;
const STATUS_BAD_STACK: u32 = 0xC000_0028;

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

/// A language-specific handler (`__C_specific_handler`, `__CxxFrameHandler3`).
pub type Handler = unsafe extern "C" fn(*mut ExceptionRecord, u64, *mut Context, *mut DispatcherContext) -> u32;

const DISPOSITION_CONTINUE_EXECUTION: u32 = 0;
const DISPOSITION_CONTINUE_SEARCH: u32 = 1;

const UNW_FLAG_EHANDLER: u32 = 1;
const UNW_FLAG_UHANDLER: u32 = 2;
const UNW_FLAG_CHAININFO: u8 = 4;

// -----| modules and function tables |-----

/// The TEB's stack bounds: (low, high).
fn stack_bounds() -> (u64, u64) {
    let (base, limit): (u64, u64);
    unsafe { core::arch::asm!("mov {}, gs:[0x08]", "mov {}, gs:[0x10]", out(reg) base, out(reg) limit, options(nostack, readonly)) };
    (limit, base)
}

/// The loaded image (base, size) containing `pc`, from PEB.Ldr.
fn image_of(pc: u64) -> Option<(u64, u64)> {
    let peb: u64;
    unsafe { core::arch::asm!("mov {}, gs:[0x60]", out(reg) peb, options(nostack, readonly)) };
    let ldr = unsafe { read_unaligned((peb + 0x18) as *const u64) };
    if ldr == 0 {
        return None;
    }
    let head = ldr + 0x10;
    let mut e = unsafe { read_unaligned(head as *const u64) };
    let mut guard = 0;
    while e != head && e != 0 && guard < 1024 {
        let base = unsafe { read_unaligned((e + 0x30) as *const u64) };
        let size = unsafe { read_unaligned((e + 0x40) as *const u32) } as u64;
        if pc >= base && pc < base + size {
            return Some((base, size));
        }
        e = unsafe { read_unaligned(e as *const u64) };
        guard += 1;
    }
    None
}

/// The image's exception directory: (first entry, count).
fn exception_directory(base: u64) -> Option<(*const RuntimeFunction, usize)> {
    unsafe {
        let nt = base + read_unaligned((base + 0x3C) as *const u32) as u64;
        let optional = nt + 0x18;
        if read_unaligned(optional as *const u16) != 0x20B {
            return None;
        }
        let entries = read_unaligned((optional + 0x6C) as *const u32);
        if entries <= 3 {
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

/// Function tables added at run time (RtlAddFunctionTable): JIT code.
struct DynamicTable {
    table: *const RuntimeFunction,
    count: u32,
    base: u64,
}

static mut DYNAMIC: [Option<DynamicTable>; 32] = [const { None }; 32];

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

#[no_mangle]
pub unsafe extern "system" fn RtlLookupFunctionEntry(pc: u64, image_base: *mut u64, _history: usize) -> *const RuntimeFunction {
    if let Some((base, _)) = image_of(pc) {
        unsafe { *image_base = base };
        let Some((table, count)) = exception_directory(base) else { return core::ptr::null() };
        return search(table, count, (pc - base) as u32);
    }
    for t in unsafe { (*(&raw const DYNAMIC)).iter().flatten() } {
        let f = search(t.table, t.count as usize, pc.wrapping_sub(t.base) as u32);
        if !f.is_null() {
            unsafe { *image_base = t.base };
            return f;
        }
    }
    core::ptr::null()
}

#[no_mangle]
pub unsafe extern "system" fn RtlPcToFileHeader(pc: u64, base: *mut u64) -> u64 {
    let b = image_of(pc).map_or(0, |(b, _)| b);
    unsafe { *base = b };
    b
}

#[no_mangle]
pub unsafe extern "system" fn RtlAddFunctionTable(table: *const RuntimeFunction, count: u32, base: u64) -> u8 {
    for slot in unsafe { (*(&raw mut DYNAMIC)).iter_mut() } {
        if slot.is_none() {
            *slot = Some(DynamicTable { table, count, base });
            return 1;
        }
    }
    0
}

#[no_mangle]
pub unsafe extern "system" fn RtlDeleteFunctionTable(table: *const RuntimeFunction) -> u8 {
    for slot in unsafe { (*(&raw mut DYNAMIC)).iter_mut() } {
        if slot.as_ref().is_some_and(|t| t.table == table) {
            *slot = None;
            return 1;
        }
    }
    0
}

// -----| virtual unwinding |-----

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

/// Is `pc` at an epilog of the function [begin, end)? If so, carry it out on
/// `c` (it then returns from the function) and say yes.
unsafe fn unwind_epilog(c: &mut Context, pc: u64, begin: u64, end: u64, frame_reg: usize) -> bool {
    let b = |i: u64| unsafe { *((pc + i) as *const u8) };
    let mut i = 0u64;
    let mut rsp = c.rsp();
    // add rsp, imm  |  lea rsp, [frame + disp]
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
    // pop reg ...
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
    // ...then ret, or a jump out of the function.
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

/// Undo the prologue of the function whose unwind info is at `info`, for
/// `pc_offset` bytes into it. Returns false if a machine frame ended it
/// (RIP and RSP already set).
unsafe fn unwind_prologue(c: &mut Context, image_base: u64, info: u64, pc_offset: u64, frame_base: u64) -> (bool, Option<u64>) {
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
                    return (false, None);
                }
                _ => {}
            }
        }
        k += slots;
    }
    // Chained: the primary function's codes follow (all of them apply).
    if p(0) >> 3 & UNW_FLAG_CHAININFO != 0 {
        let chained = info + 4 + ((count + 1) & !1) * 2;
        let primary = unsafe { read_unaligned(chained as *const RuntimeFunction) };
        return unsafe { unwind_prologue(c, image_base, image_base + primary.unwind as u64, u64::MAX, frame_base) };
    }
    (true, None)
}

/// Unwind one frame: `c` becomes the caller's context. Returns the frame's
/// handler if it has one of `kind` (and `pc` is past its prologue), setting
/// its data and the establisher frame.
#[no_mangle]
pub unsafe extern "system" fn RtlVirtualUnwind(
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

    // The frame's base: where its saved registers are found from, and its
    // establisher frame.
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

    let (pop_return, _) = unsafe { unwind_prologue(c, image_base, info, pc_offset, frame_base) };
    if pop_return {
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

/// Has the SET_FPREG code run by `pc_offset`?
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

/// Unwind one frame of `c`, whatever kind it is. Returns false at the end
/// of the stack.
unsafe fn step(c: &mut Context) -> bool {
    let pc = c.rip();
    let mut base = 0;
    let f = unsafe { RtlLookupFunctionEntry(pc, &mut base, 0) };
    if f.is_null() {
        if image_of(pc).is_none() {
            return false;
        }
        // A leaf function: only the return address is on the stack.
        let rsp = c.rsp();
        c.set_rip(unsafe { rd64(rsp) });
        c.set_rsp(rsp + 8);
    } else {
        let (mut data, mut frame) = (core::ptr::null(), 0);
        unsafe { RtlVirtualUnwind(0, base, pc, f, c, &mut data, &mut frame, 0) };
    }
    true
}

// -----| dispatching |-----

const MAX_VECTORED: usize = 32;
type VectoredHandler = unsafe extern "system" fn(*mut ExceptionPointers) -> i32;

#[repr(C)]
pub struct ExceptionPointers {
    pub record: *mut ExceptionRecord,
    pub context: *mut Context,
}

static mut VECTORED: [Option<VectoredHandler>; MAX_VECTORED] = [None; MAX_VECTORED];
static mut VECTORED_COUNT: usize = 0;

#[no_mangle]
pub unsafe extern "system" fn RtlAddVectoredExceptionHandler(first: u32, handler: VectoredHandler) -> usize {
    unsafe {
        let v = &mut *(&raw mut VECTORED);
        if VECTORED_COUNT == MAX_VECTORED {
            return 0;
        }
        if first != 0 {
            v.copy_within(0..VECTORED_COUNT, 1);
            v[0] = Some(handler);
        } else {
            v[VECTORED_COUNT] = Some(handler);
        }
        VECTORED_COUNT += 1;
        handler as usize
    }
}

#[no_mangle]
pub unsafe extern "system" fn RtlRemoveVectoredExceptionHandler(handle: usize) -> u32 {
    unsafe {
        let v = &mut *(&raw mut VECTORED);
        let n = VECTORED_COUNT;
        if let Some(i) = v[..n].iter().position(|h| h.map(|h| h as usize) == Some(handle)) {
            v.copy_within(i + 1..n, i);
            v[n - 1] = None;
            VECTORED_COUNT -= 1;
            return 1;
        }
    }
    0
}

/// Offer the exception to the vectored handlers, then to each frame's
/// handler from the faulting one up. True if one said to continue
/// execution (with `context` as it left it).
#[no_mangle]
pub unsafe extern "system" fn RtlDispatchException(rec: *mut ExceptionRecord, context: *mut Context) -> u8 {
    let mut ptrs = ExceptionPointers { record: rec, context };
    for i in 0..unsafe { VECTORED_COUNT } {
        if let Some(h) = unsafe { VECTORED[i] } {
            if unsafe { h(&mut ptrs) } == -1 {
                return 1;
            }
        }
    }

    let (low, high) = stack_bounds();
    let mut c = unsafe { *context };
    loop {
        let pc = c.rip();
        let rsp = c.rsp();
        if rsp < low || rsp >= high || rsp & 7 != 0 {
            unsafe { (*rec).flags |= 8 }; // EXCEPTION_STACK_INVALID
            return 0;
        }
        let mut base = 0;
        let f = unsafe { RtlLookupFunctionEntry(pc, &mut base, 0) };
        if f.is_null() {
            if image_of(pc).is_none() || pc == 0 {
                return 0;
            }
            c.set_rip(unsafe { rd64(rsp) });
            c.set_rsp(rsp + 8);
            continue;
        }
        let before = c;
        let (mut data, mut frame) = (core::ptr::null(), 0);
        let handler = unsafe { RtlVirtualUnwind(UNW_FLAG_EHANDLER, base, pc, f, &mut c, &mut data, &mut frame, 0) };
        let Some(handler) = handler else { continue };
        let mut before = before;
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
                    unsafe { raise_status(STATUS_NONCONTINUABLE_EXCEPTION, rec) };
                }
                return 1;
            }
            DISPOSITION_CONTINUE_SEARCH => {}
            // Nested and collided unwinds: keep searching.
            2 | 3 => {}
            _ => unsafe { raise_status(STATUS_INVALID_DISPOSITION, rec) },
        }
    }
}

/// Raise a noncontinuable status exception (about `inner`).
unsafe fn raise_status(code: u32, inner: *mut ExceptionRecord) -> ! {
    let mut r = ExceptionRecord { code, flags: EXCEPTION_NONCONTINUABLE, record: inner, address: 0, count: 0, info: [0; 15] };
    unsafe { RtlRaiseException(&mut r) };
    unsafe { unhandled(&mut r, null_mut()) }
}

/// Raise a software exception: dispatch it from the caller's frame.
#[no_mangle]
#[inline(never)]
pub unsafe extern "system" fn RtlRaiseException(rec: *mut ExceptionRecord) {
    let mut c = Context::zeroed();
    unsafe { RtlCaptureContext(&mut c) };
    // Now step out of this function: the context of our caller, at the
    // return address.
    unsafe { step(&mut c) };
    unsafe {
        (*rec).address = c.rip();
        if RtlDispatchException(rec, &mut c) != 0 {
            RtlRestoreContext(&mut c, null_mut());
        }
        unhandled(rec, &mut c)
    }
}

type TopLevelFilter = unsafe extern "system" fn(*mut ExceptionPointers) -> i32;
static mut TOP_LEVEL_FILTER: Option<TopLevelFilter> = None;

/// kernel32's SetUnhandledExceptionFilter keeps its filter here.
#[no_mangle]
pub unsafe extern "system" fn RtlSetUnhandledExceptionFilter(f: Option<TopLevelFilter>) -> Option<TopLevelFilter> {
    unsafe { core::mem::replace(&mut *(&raw mut TOP_LEVEL_FILTER), f) }
}

/// No frame handled it: the top-level filter, then the end of the process
/// (exit code = the exception code, as on Windows).
unsafe fn unhandled(rec: *mut ExceptionRecord, context: *mut Context) -> ! {
    if let Some(filter) = unsafe { TOP_LEVEL_FILTER } {
        let mut ptrs = ExceptionPointers { record: rec, context };
        if unsafe { filter(&mut ptrs) } == -1 && !context.is_null() {
            unsafe { RtlRestoreContext(context, null_mut()) };
        }
    }
    let code = unsafe { (*rec).code };
    loop {
        // NtTerminateProcess(NtCurrentProcess(), code)
        unsafe {
            core::arch::asm!("syscall", in("r10") u64::MAX, in("rdx") code as u64,
                inlateout("rax") nt_abi::sys::NT_TERMINATE_PROCESS as u64 => _, lateout("rcx") _, lateout("r11") _)
        };
    }
}

/// Where a fault in user mode starts (see KiUserExceptionDispatcher).
#[no_mangle]
unsafe extern "C" fn veil_dispatch_user(rec: *mut ExceptionRecord, context: *mut Context) -> ! {
    unsafe {
        if RtlDispatchException(rec, context) != 0 {
            RtlRestoreContext(context, null_mut());
        }
        unhandled(rec, context)
    }
}

// -----| unwinding |-----

/// Unwind to `target_frame`, running termination handlers on the way, and
/// resume there at `target_ip` with RAX = `return_value`.
#[no_mangle]
#[inline(never)]
pub unsafe extern "system" fn RtlUnwindEx(
    target_frame: u64,
    target_ip: u64,
    rec: *mut ExceptionRecord,
    return_value: u64,
    original: *mut Context,
    _history: usize,
) {
    let mut local = ExceptionRecord { code: STATUS_UNWIND, flags: 0, record: null_mut(), address: 0, count: 0, info: [0; 15] };
    let rec = if rec.is_null() { &raw mut local } else { rec };
    let mut c = Context::zeroed();
    unsafe { RtlCaptureContext(&mut c) };
    if unsafe { (*rec).address } == 0 {
        unsafe { (*rec).address = c.rip() };
    }
    unsafe { (*rec).flags |= EXCEPTION_UNWINDING };
    if target_frame == 0 {
        unsafe { (*rec).flags |= EXCEPTION_EXIT_UNWIND };
    }
    let (low, high) = stack_bounds();
    loop {
        let pc = c.rip();
        let rsp = c.rsp();
        if rsp < low || rsp >= high {
            unsafe { raise_status(STATUS_BAD_STACK, rec) };
        }
        let mut base = 0;
        let f = unsafe { RtlLookupFunctionEntry(pc, &mut base, 0) };
        if f.is_null() {
            if image_of(pc).is_none() {
                unsafe { raise_status(STATUS_BAD_STACK, rec) };
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
            // Resume in the target frame.
            before.set_rip(target_ip);
            before.set_reg(0, return_value);
            unsafe { RtlRestoreContext(&mut before, rec) };
        }
        if frame > target_frame && target_frame != 0 {
            // Went past it: the target isn't on this stack.
            unsafe { raise_status(STATUS_INVALID_DISPOSITION, rec) };
        }
    }
}

#[no_mangle]
pub unsafe extern "system" fn RtlUnwind(target_frame: u64, target_ip: u64, rec: *mut ExceptionRecord, return_value: u64) {
    unsafe { RtlUnwindEx(target_frame, target_ip, rec, return_value, null_mut(), 0) }
}

/// Walk the caller's stack: up to `count` return addresses after skipping
/// `skip` frames.
#[no_mangle]
pub unsafe extern "system" fn RtlCaptureStackBackTrace(skip: u32, count: u32, out: *mut u64, hash: *mut u32) -> u16 {
    let mut c = Context::zeroed();
    unsafe { RtlCaptureContext(&mut c) };
    let mut n = 0u32;
    let mut i = 0u32;
    let mut sum = 0u32;
    while n < count && unsafe { step(&mut c) } && c.rip() != 0 {
        if i > skip {
            unsafe { *out.add(n as usize) = c.rip() };
            sum = sum.wrapping_add(c.rip() as u32);
            n += 1;
        }
        i += 1;
    }
    if !hash.is_null() {
        unsafe { *hash = sum };
    }
    n as u16
}

// -----| context capture and restore |-----

/// The caller's registers, as at the return from this call.
#[unsafe(naked)]
#[no_mangle]
pub unsafe extern "system" fn RtlCaptureContext(_c: *mut Context) {
    naked_asm!(
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
        "mov word ptr [rcx + 0x3A], ds",
        "mov word ptr [rcx + 0x3C], es",
        "mov word ptr [rcx + 0x3E], fs",
        "mov word ptr [rcx + 0x40], gs",
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

unsafe extern "system" {
    /// Load every register from the context and continue there. With a
    /// STATUS_UNWIND_CONSOLIDATE record, first run its callback (through
    /// RcConsolidateFrames), which returns where to continue.
    pub fn RtlRestoreContext(c: *mut Context, rec: *mut ExceptionRecord) -> !;
}

// The two routines the unwinder must be able to see through, and the
// context restore. Offsets: CONTEXT registers at 0x78 + 8n, RIP 0xF8,
// XMM n at 0x1A0 + 16n, EFlags 0x44, MxCsr 0x34.
global_asm!(
    // KiUserExceptionDispatcher: entered from the kernel with RSP at 0x20
    // bytes of home space, the CONTEXT above it, the EXCEPTION_RECORD after
    // the CONTEXT (at +0x4F0), and the machine frame at +0x590.
    ".globl KiUserExceptionDispatcher",
    ".def KiUserExceptionDispatcher; .scl 2; .type 32; .endef",
    ".seh_proc KiUserExceptionDispatcher",
    "KiUserExceptionDispatcher:",
    ".seh_pushframe",
    ".seh_stackalloc 0x590",
    ".seh_savereg rbx, 0x20 + 0x90",
    ".seh_savereg rbp, 0x20 + 0xA0",
    ".seh_savereg rsi, 0x20 + 0xA8",
    ".seh_savereg rdi, 0x20 + 0xB0",
    ".seh_savereg r12, 0x20 + 0xD8",
    ".seh_savereg r13, 0x20 + 0xE0",
    ".seh_savereg r14, 0x20 + 0xE8",
    ".seh_savereg r15, 0x20 + 0xF0",
    ".seh_savexmm xmm6, 0x20 + 0x200",
    ".seh_savexmm xmm7, 0x20 + 0x210",
    ".seh_savexmm xmm8, 0x20 + 0x220",
    ".seh_savexmm xmm9, 0x20 + 0x230",
    ".seh_savexmm xmm10, 0x20 + 0x240",
    ".seh_savexmm xmm11, 0x20 + 0x250",
    ".seh_savexmm xmm12, 0x20 + 0x260",
    ".seh_savexmm xmm13, 0x20 + 0x270",
    ".seh_savexmm xmm14, 0x20 + 0x280",
    ".seh_savexmm xmm15, 0x20 + 0x290",
    ".seh_endprologue",
    "lea rcx, [rsp + 0x20 + 0x4D0]",
    "lea rdx, [rsp + 0x20]",
    "call veil_dispatch_user",
    "ud2",
    ".seh_endproc",
    //
    // RcConsolidateFrames: RtlRestoreContext jumps here with RCX = the
    // consolidate record, RSP at home space, a copy of the target CONTEXT
    // above it (+0x20) and a machine frame for the target at +0x4F0.
    ".def RcConsolidateFrames; .scl 2; .type 32; .endef",
    ".seh_proc RcConsolidateFrames",
    "RcConsolidateFrames:",
    ".seh_pushframe",
    ".seh_stackalloc 0x4F0",
    ".seh_savereg rbx, 0x20 + 0x90",
    ".seh_savereg rbp, 0x20 + 0xA0",
    ".seh_savereg rsi, 0x20 + 0xA8",
    ".seh_savereg rdi, 0x20 + 0xB0",
    ".seh_savereg r12, 0x20 + 0xD8",
    ".seh_savereg r13, 0x20 + 0xE0",
    ".seh_savereg r14, 0x20 + 0xE8",
    ".seh_savereg r15, 0x20 + 0xF0",
    ".seh_savexmm xmm6, 0x20 + 0x200",
    ".seh_savexmm xmm7, 0x20 + 0x210",
    ".seh_savexmm xmm8, 0x20 + 0x220",
    ".seh_savexmm xmm9, 0x20 + 0x230",
    ".seh_savexmm xmm10, 0x20 + 0x240",
    ".seh_savexmm xmm11, 0x20 + 0x250",
    ".seh_savexmm xmm12, 0x20 + 0x260",
    ".seh_savexmm xmm13, 0x20 + 0x270",
    ".seh_savexmm xmm14, 0x20 + 0x280",
    ".seh_savexmm xmm15, 0x20 + 0x290",
    ".seh_endprologue",
    "call qword ptr [rcx + 0x20]", // ExceptionInformation[0](record) -> where to go
    "mov [rsp + 0x20 + 0xF8], rax",
    "lea rcx, [rsp + 0x20]",
    "xor edx, edx",
    "jmp RtlRestoreContext",
    ".seh_endproc",
    //
    // RtlRestoreContext(context, record)
    ".globl RtlRestoreContext",
    ".def RtlRestoreContext; .scl 2; .type 32; .endef",
    "RtlRestoreContext:",
    "test rdx, rdx",
    "jz 2f",
    "cmp dword ptr [rdx], 0x80000029",
    "jne 2f",
    // Consolidate: a frame below the current stack holding a copy of the
    // context and a machine frame for it, then RcConsolidateFrames.
    "mov rax, rsp",
    "sub rax, 0x600",
    "and rax, -16",
    "mov r8, rcx",
    "mov r9, rdx",
    "lea rdi, [rax + 0x20]",
    "mov rsi, r8",
    "mov ecx, 0x4D0 / 8",
    "cld",
    "rep movsq",
    "mov r10, [r8 + 0xF8]",
    "mov [rax + 0x4F0], r10",
    "mov qword ptr [rax + 0x4F8], 0x33",
    "mov r10d, dword ptr [r8 + 0x44]",
    "mov [rax + 0x500], r10",
    "mov r10, [r8 + 0x98]",
    "mov [rax + 0x508], r10",
    "mov qword ptr [rax + 0x510], 0x2B",
    "mov rsp, rax",
    "mov rcx, r9",
    "jmp RcConsolidateFrames",
    // Plain restore.
    "2:",
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
    // RIP and RFLAGS go just below the target RSP, for popfq; ret.
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
);
