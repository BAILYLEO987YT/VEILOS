//! Exception handling in the C runtime (W5b): `__C_specific_handler` for
//! `__try`/`__except`/`__finally`, and C++ exceptions (`_CxxThrowException`,
//! `__CxxFrameHandler3`, the msvcrt `std::exception` class).
//!
//! A C++ throw is a software exception (0xE06D7363) carrying the object, its
//! ThrowInfo and the image base. The frame handler of each function with
//! try blocks finds its state from the IP-to-state map; a try block covering
//! that state with a handler whose type matches one of the throw's
//! catchable types catches it: the catch object is built, then RtlUnwindEx
//! unwinds to the function with a STATUS_UNWIND_CONSOLIDATE record whose
//! callback (`call_catch_block`) runs the catch funclet on the current stack,
//! where the thrown object still lives, and returns where to continue.
//!
//! While a catch block runs, the frame that holds its try block is still
//! seen by the unwinder at the try's code. The `ACTIVE` list remembers each
//! running catch so that frame isn't given the exception again (only try
//! blocks around the whole try/catch may take it) and its locals aren't
//! destroyed twice. One list per thread.

use crate::startup::fatal;
use core::ptr::{null_mut, read_unaligned};

#[link(name = "ntdll", kind = "raw-dylib")]
unsafe extern "system" {
    fn RtlUnwindEx(frame: u64, target_ip: u64, rec: *mut Record, retval: u64, ctx: *mut u8, history: usize);
    fn RtlPcToFileHeader(pc: u64, base: *mut u64) -> u64;
}

#[link(name = "kernel32", kind = "raw-dylib")]
unsafe extern "system" {
    fn RaiseException(code: u32, flags: u32, count: u32, args: *const u64);
}

#[repr(C)]
pub struct Record {
    code: u32,
    flags: u32,
    record: *mut Record,
    address: u64,
    count: u32,
    info: [u64; 15],
}

#[repr(C)]
pub struct Dispatcher {
    control_pc: u64,
    image_base: u64,
    function_entry: *const [u32; 3],
    establisher_frame: u64,
    target_ip: u64,
    context: *mut u8,
    language_handler: usize,
    handler_data: *const u8,
    history_table: usize,
    scope_index: u32,
    fill: u32,
}

#[repr(C)]
struct Pointers {
    record: *mut Record,
    context: *mut u8,
}

const UNWINDING: u32 = 0x2;
const EXIT_UNWIND: u32 = 0x4;
const TARGET_UNWIND: u32 = 0x20;
const CONTINUE_EXECUTION: u32 = 0;
const CONTINUE_SEARCH: u32 = 1;

unsafe fn u32_at(a: u64) -> u32 {
    unsafe { read_unaligned(a as *const u32) }
}

unsafe fn i32_at(a: u64) -> i32 {
    unsafe { read_unaligned(a as *const i32) }
}

// -----| __try / __except / __finally |-----

/// The scope table after the unwind info: count, then {begin, end,
/// filter or finally, __except target (0 for __finally)} as RVAs.
#[no_mangle]
pub unsafe extern "C" fn __C_specific_handler(rec: *mut Record, frame: u64, ctx: *mut u8, dc: *mut Dispatcher) -> u32 {
    let dc = unsafe { &mut *dc };
    let base = dc.image_base;
    let pc = (dc.control_pc - base) as u32;
    let table = dc.handler_data as u64;
    let count = unsafe { u32_at(table) };
    let scope = |i: u32| unsafe {
        let s = table + 4 + 16 * i as u64;
        (u32_at(s), u32_at(s + 4), u32_at(s + 8), u32_at(s + 12))
    };
    let flags = unsafe { (*rec).flags };

    if flags & (UNWINDING | EXIT_UNWIND) == 0 {
        // Looking for an __except whose filter takes it.
        let mut ptrs = Pointers { record: rec, context: ctx };
        for i in dc.scope_index..count {
            let (begin, end, filter, target) = scope(i);
            if pc < begin || pc >= end || target == 0 {
                continue;
            }
            let verdict = if filter == 1 {
                1 // __except(EXCEPTION_EXECUTE_HANDLER), folded by the compiler
            } else {
                let f: unsafe extern "C" fn(*mut Pointers, u64) -> i32 = unsafe { core::mem::transmute(base + filter as u64) };
                unsafe { f(&mut ptrs, frame) }
            };
            if verdict < 0 {
                return CONTINUE_EXECUTION;
            }
            if verdict > 0 {
                // Into the __except block, with the code in EAX.
                unsafe { RtlUnwindEx(frame, base + target as u64, rec, (*rec).code as u64, dc.context, dc.history_table) };
            }
        }
    } else {
        // Unwinding: run the __finally blocks being left.
        let target = (dc.target_ip.wrapping_sub(base)) as u32;
        for i in dc.scope_index..count {
            let (begin, end, handler, jump) = scope(i);
            if pc < begin || pc >= end {
                continue;
            }
            if flags & TARGET_UNWIND != 0 && target >= begin && target < end {
                break; // a jump within this scope
            }
            if jump != 0 {
                if flags & TARGET_UNWIND != 0 && target == jump {
                    break;
                }
                continue;
            }
            dc.scope_index = i + 1;
            let fin: unsafe extern "C" fn(u8, u64) = unsafe { core::mem::transmute(base + handler as u64) };
            unsafe { fin(1, frame) };
        }
    }
    CONTINUE_SEARCH
}

#[no_mangle]
pub unsafe extern "C" fn __C_specific_handler_noexcept(rec: *mut Record, frame: u64, ctx: *mut u8, dc: *mut Dispatcher) -> u32 {
    unsafe { __C_specific_handler(rec, frame, ctx, dc) }
}

/// `return`/`goto` out of a __try: run the __finally blocks up to `target`.
#[no_mangle]
pub unsafe extern "C" fn _local_unwind(frame: u64, target: u64) {
    unsafe { RtlUnwindEx(frame, target, null_mut(), 0, null_mut(), 0) };
}

// -----| C++ exceptions: throwing |-----

const CXX_EXCEPTION: u32 = 0xE06D_7363;
const CXX_MAGIC: u64 = 0x1993_0520;
const STATUS_UNWIND_CONSOLIDATE: u32 = 0x8000_0029;

/// A catch block that is running (see the module comment).
#[derive(Clone, Copy)]
struct Active {
    /// Establisher frame of the function (or funclet) holding the try block.
    try_frame: u64,
    /// The catch funclet, and the frame its locals live in.
    funclet: u64,
    root: u64,
    /// The try block's states.
    low: i32,
    high: i32,
    catch_high: i32,
    object: u64,
    throw_info: u64,
    throw_base: u64,
    rethrown: bool,
    /// The catch block has been left by an exception.
    exited: bool,
}

const MAX_ACTIVE: usize = 64;

/// A thread's C++ exception state (threads.rs keeps one per thread).
pub struct EhState {
    active: [Option<Active>; MAX_ACTIVE],
    n: usize,
    /// Thrown and not yet caught.
    uncaught: i32,
}

impl EhState {
    pub const fn new() -> Self {
        EhState { active: [None; MAX_ACTIVE], n: 0, uncaught: 0 }
    }
}

fn st() -> &'static mut EhState {
    &mut crate::threads::this().eh
}

fn active() -> &'static mut [Option<Active>] {
    let s = st();
    &mut s.active[..s.n]
}

fn push_active(a: Active) {
    let s = st();
    if s.n == MAX_ACTIVE {
        fatal("too many nested C++ catch blocks", 3);
    }
    s.active[s.n] = Some(a);
    s.n += 1;
}

fn pop_active() -> Option<Active> {
    let s = st();
    if s.n == 0 {
        return None;
    }
    s.n -= 1;
    s.active[s.n].take()
}

/// The innermost running catch block's exception (for `throw;`).
fn current() -> Option<&'static mut Active> {
    active().iter_mut().rev().flatten().find(|a| !a.exited)
}

#[no_mangle]
pub unsafe extern "C" fn _CxxThrowException(object: u64, throw_info: u64) -> ! {
    let (object, throw_info, base) = if object == 0 && throw_info == 0 {
        // `throw;`: the exception being handled, again.
        match current() {
            Some(a) => {
                a.rethrown = true;
                (a.object, a.throw_info, a.throw_base)
            }
            None => crate::startup::terminate(),
        }
    } else {
        let mut base = 0;
        unsafe { RtlPcToFileHeader(throw_info, &mut base) };
        (object, throw_info, base)
    };
    st().uncaught += 1;
    let args = [CXX_MAGIC, object, throw_info, base];
    unsafe { RaiseException(CXX_EXCEPTION, 1, 4, args.as_ptr()) };
    crate::startup::terminate()
}

#[no_mangle]
pub extern "C" fn __uncaught_exception() -> bool {
    st().uncaught > 0
}

#[no_mangle]
pub extern "C" fn __uncaught_exceptions() -> i32 {
    st().uncaught
}

#[no_mangle]
pub extern "C" fn __processing_throw() -> *mut i32 {
    &mut st().uncaught
}

// -----| C++ exceptions: the compilers' metadata |-----
//
// FH3 (clang, older MSVC) is plain tables of RVAs; FH4 (MSVC 2019 on) packs
// the same information into variable-length integers. Both are read into
// `Meta`, which the frame handler works from.

#[derive(Clone, Copy)]
enum Action {
    None,
    /// An unwind funclet: (0, frame).
    Funclet(u64),
    /// A destructor on the object at frame + offset.
    DtorObject(u64, u32),
    /// A destructor on the object pointed to from frame + offset.
    DtorPointer(u64, u32),
}

#[derive(Clone, Copy)]
struct Handler {
    adjectives: u32,
    /// TypeDescriptor (absolute), 0 for catch (...).
    type_desc: u64,
    catch_obj: i32,
    funclet: u64,
    continuations: [u64; 2],
    continuation_count: u32,
}

struct TryBlock {
    low: i32,
    high: i32,
    catch_high: i32,
    handlers: alloc::vec::Vec<Handler>,
}

struct Meta {
    /// (next state, action) per state.
    unwind: alloc::vec::Vec<(i32, Action)>,
    tries: alloc::vec::Vec<TryBlock>,
    /// (RVA, state) in order.
    ip_map: alloc::vec::Vec<(u32, i32)>,
    noexcept: bool,
}

impl Meta {
    fn state_at(&self, rva: u32) -> i32 {
        let mut state = -1;
        for &(ip, s) in &self.ip_map {
            if ip > rva {
                break;
            }
            state = s;
        }
        state
    }
}

/// FH3: FuncInfo { magic, maxState, unwind map, try count, try map, IP map
/// count, IP map, UnwindHelp, ES types, flags }.
unsafe fn read_fh3(fi: u64, base: u64) -> Meta {
    unsafe {
        let magic = u32_at(fi);
        let max_state = i32_at(fi + 4).max(0) as u64;
        let unwind_map = base + u32_at(fi + 8) as u64;
        let try_count = u32_at(fi + 12) as u64;
        let try_map = base + u32_at(fi + 16) as u64;
        let ip_count = u32_at(fi + 20) as u64;
        let ip_map = base + u32_at(fi + 24) as u64;
        let flags = if magic >= 0x1993_0522 { u32_at(fi + 36) } else { 0 };
        let unwind = (0..max_state)
            .map(|s| {
                let e = unwind_map + 8 * s;
                let action = u32_at(e + 4);
                (i32_at(e), if action == 0 { Action::None } else { Action::Funclet(base + action as u64) })
            })
            .collect();
        let tries = (0..try_count)
            .map(|i| {
                let t = try_map + 20 * i;
                let handlers = base + u32_at(t + 16) as u64;
                TryBlock {
                    low: i32_at(t),
                    high: i32_at(t + 4),
                    catch_high: i32_at(t + 8),
                    handlers: (0..u32_at(t + 12) as u64)
                        .map(|j| {
                            let h = handlers + 20 * j;
                            let td = u32_at(h + 4);
                            Handler {
                                adjectives: u32_at(h),
                                type_desc: if td == 0 { 0 } else { base + td as u64 },
                                catch_obj: i32_at(h + 8),
                                funclet: base + u32_at(h + 12) as u64,
                                continuations: [0; 2],
                                continuation_count: 0,
                            }
                        })
                        .collect(),
                }
            })
            .collect();
        let ip_map = (0..ip_count).map(|i| (u32_at(ip_map + 8 * i), i32_at(ip_map + 8 * i + 4))).collect();
        Meta { unwind, tries, ip_map, noexcept: flags & 4 != 0 }
    }
}

/// FH4's compressed unsigned integer: its length is in the low bits of the
/// first byte.
unsafe fn read_unsigned(p: &mut u64) -> u32 {
    const NEG_LENGTH: [i8; 16] = [-1, -2, -1, -3, -1, -2, -1, -4, -1, -2, -1, -3, -1, -2, -1, -5];
    const SHIFT: [u8; 16] = [25, 18, 25, 11, 25, 18, 25, 4, 25, 18, 25, 11, 25, 18, 25, 0];
    let bits = (unsafe { *(*p as *const u8) } & 0xF) as usize;
    let len = -(NEG_LENGTH[bits] as i64) as u64;
    let word = unsafe { u32_at(*p + len - 4) };
    *p += len;
    word >> SHIFT[bits]
}

unsafe fn read_int(p: &mut u64) -> i32 {
    let v = unsafe { i32_at(*p) };
    *p += 4;
    v
}

/// FH4: a header byte, then the RVAs present, then (catch funclets) the
/// offset of the parent's frame pointer.
unsafe fn read_fh4(info: u64, base: u64, func_start: u32) -> (Meta, Option<u32>) {
    unsafe {
        let mut p = info;
        let header = *(p as *const u8);
        p += 1;
        if header & 0x04 != 0 {
            read_unsigned(&mut p); // BBT flags
        }
        let unwind_rva = if header & 0x08 != 0 { read_int(&mut p) } else { 0 };
        let try_rva = if header & 0x10 != 0 { read_int(&mut p) } else { 0 };
        let mut ip_rva = read_int(&mut p) as u32;
        let frame_disp = if header & 0x01 != 0 { Some(read_unsigned(&mut p)) } else { None };

        // Unwind map: entries linked backwards by byte offsets.
        let mut unwind = alloc::vec::Vec::new();
        if unwind_rva != 0 {
            let mut q = base + unwind_rva as u64;
            let count = read_unsigned(&mut q);
            let start = q;
            let mut offsets = alloc::vec::Vec::new();
            let mut raw = alloc::vec::Vec::new();
            for _ in 0..count {
                offsets.push(q - start);
                let v = read_unsigned(&mut q);
                let action = match v & 3 {
                    1 => {
                        let f = read_int(&mut q);
                        Action::DtorObject(base + f as u64, read_unsigned(&mut q))
                    }
                    2 => {
                        let f = read_int(&mut q);
                        Action::DtorPointer(base + f as u64, read_unsigned(&mut q))
                    }
                    3 => Action::Funclet(base + read_int(&mut q) as u64),
                    _ => Action::None,
                };
                raw.push((v >> 2, action));
            }
            for (i, &(back, action)) in raw.iter().enumerate() {
                let next = offsets[i].checked_sub(back as u64).filter(|_| back != 0);
                let to = next.and_then(|o| offsets.iter().position(|&x| x == o)).map_or(-1, |s| s as i32);
                unwind.push((to, action));
            }
        }

        let mut tries = alloc::vec::Vec::new();
        if try_rva != 0 {
            let mut q = base + try_rva as u64;
            for _ in 0..read_unsigned(&mut q) {
                let low = read_unsigned(&mut q) as i32;
                let high = read_unsigned(&mut q) as i32;
                let catch_high = read_unsigned(&mut q) as i32;
                let mut h = base + read_int(&mut q) as u64;
                let mut handlers = alloc::vec::Vec::new();
                for _ in 0..read_unsigned(&mut h) {
                    let hdr = *(h as *const u8);
                    h += 1;
                    let adjectives = if hdr & 0x01 != 0 { read_unsigned(&mut h) } else { 0 };
                    let td = if hdr & 0x02 != 0 { read_int(&mut h) } else { 0 };
                    let catch_obj = if hdr & 0x04 != 0 { read_unsigned(&mut h) as i32 } else { 0 };
                    let funclet = base + read_int(&mut h) as u64;
                    let count = ((hdr >> 4) & 3) as u32;
                    let mut continuations = [0u64; 2];
                    for c in continuations.iter_mut().take(count as usize) {
                        *c = if hdr & 0x08 != 0 {
                            base + read_int(&mut h) as u64
                        } else {
                            base + func_start as u64 + read_unsigned(&mut h) as u64
                        };
                    }
                    handlers.push(Handler {
                        adjectives,
                        type_desc: if td == 0 { 0 } else { base + td as u64 },
                        catch_obj,
                        funclet,
                        continuations,
                        continuation_count: count,
                    });
                }
                tries.push(TryBlock { low, high, catch_high, handlers });
            }
        }

        // IP-to-state map; a function split in pieces has one per piece.
        if header & 0x02 != 0 {
            let mut q = base + ip_rva as u64;
            for _ in 0..read_unsigned(&mut q) {
                let start = read_int(&mut q) as u32;
                let map = read_int(&mut q) as u32;
                if start == func_start {
                    ip_rva = map;
                }
            }
        }
        let mut ip_map = alloc::vec::Vec::new();
        let mut q = base + ip_rva as u64;
        let mut ip = func_start;
        for _ in 0..read_unsigned(&mut q) {
            ip = ip.wrapping_add(read_unsigned(&mut q));
            let state = read_unsigned(&mut q) as i32 - 1;
            ip_map.push((ip, state));
        }
        (Meta { unwind, tries, ip_map, noexcept: header & 0x40 != 0 }, frame_disp)
    }
}

// -----| C++ exceptions: catching |-----

/// Run unwind actions from state `from` back to `to` (while `keep` holds).
unsafe fn unwind_states(m: &Meta, root: u64, from: i32, to: i32, keep: impl Fn(i32) -> bool) {
    let mut s = from;
    let mut guard = 0;
    while s != to && s >= 0 && (s as usize) < m.unwind.len() && keep(s) && guard <= m.unwind.len() {
        let (next, action) = m.unwind[s as usize];
        unsafe {
            match action {
                Action::None => {}
                Action::Funclet(f) => core::mem::transmute::<u64, unsafe extern "C" fn(u64, u64)>(f)(0, root),
                Action::DtorObject(f, off) => core::mem::transmute::<u64, unsafe extern "C" fn(u64)>(f)(root + off as u64),
                Action::DtorPointer(f, off) => {
                    core::mem::transmute::<u64, unsafe extern "C" fn(u64)>(f)(read_unaligned((root + off as u64) as *const u64))
                }
            }
        }
        s = next;
        guard += 1;
    }
}

/// `this` adjusted from the thrown type to a base (PMD).
unsafe fn adjust(p: u64, mdisp: i32, pdisp: i32, vdisp: i32) -> u64 {
    let mut r = p.wrapping_add(mdisp as i64 as u64);
    if pdisp >= 0 {
        unsafe {
            let vbtable = read_unaligned((p.wrapping_add(pdisp as i64 as u64)) as *const u64);
            r = r.wrapping_add(pdisp as i64 as u64).wrapping_add(i32_at(vbtable + vdisp as u64) as i64 as u64);
        }
    }
    r
}

/// Does handler `h` take catchable type `ct` of a throw with attributes
/// `attrs`?
unsafe fn type_matches(h: &Handler, ct: u64, tbase: u64, attrs: u32) -> bool {
    if h.type_desc == 0 || unsafe { *((h.type_desc + 16) as *const u8) } == 0 {
        return true; // catch (...)
    }
    let ctd = tbase + unsafe { u32_at(ct + 4) } as u64;
    if h.type_desc != ctd && unsafe { crate::string::strcmp((h.type_desc + 16) as *const u8, (ctd + 16) as *const u8) } != 0 {
        return false;
    }
    let props = unsafe { u32_at(ct) };
    if props & 2 != 0 && h.adjectives & 8 == 0 {
        return false; // only by reference
    }
    if attrs & 1 != 0 && h.adjectives & 1 == 0 {
        return false; // const thrown, non-const catch
    }
    if attrs & 2 != 0 && h.adjectives & 2 == 0 {
        return false;
    }
    true
}

/// Copy (or point to) the thrown object as the catch block's parameter.
unsafe fn build_catch_object(h: &Handler, root: u64, object: u64, ct: u64, tbase: u64) {
    if h.catch_obj == 0 || h.type_desc == 0 {
        return;
    }
    let dest = root.wrapping_add(h.catch_obj as i64 as u64);
    unsafe {
        let props = u32_at(ct);
        let (mdisp, pdisp, vdisp) = (i32_at(ct + 8), i32_at(ct + 12), i32_at(ct + 16));
        let size = i32_at(ct + 20) as usize;
        let copy = u32_at(ct + 24);
        if h.adjectives & 8 != 0 {
            *(dest as *mut u64) = adjust(object, mdisp, pdisp, vdisp);
        } else if props & 1 != 0 {
            core::ptr::copy(object as *const u8, dest as *mut u8, size);
            if size == 8 {
                *(dest as *mut u64) = adjust(*(dest as *const u64), mdisp, pdisp, vdisp);
            }
        } else if copy == 0 {
            core::ptr::copy(adjust(object, mdisp, pdisp, vdisp) as *const u8, dest as *mut u8, size);
        } else if props & 4 != 0 {
            let f: unsafe extern "C" fn(u64, u64, i32) = core::mem::transmute(tbase + copy as u64);
            f(dest, adjust(object, mdisp, pdisp, vdisp), 1);
        } else {
            let f: unsafe extern "C" fn(u64, u64) = core::mem::transmute(tbase + copy as u64);
            f(dest, adjust(object, mdisp, pdisp, vdisp));
        }
    }
}

/// The consolidate callback: run the catch funclet, then destroy the
/// exception object (unless it was rethrown) and say where to continue.
unsafe extern "C" fn call_catch_block(rec: *mut Record) -> u64 {
    let info = unsafe { (*rec).info };
    let (root, funclet) = (info[1], info[2]);
    push_active(Active {
        try_frame: info[8],
        funclet,
        root,
        low: info[3] as i32,
        high: info[4] as i32,
        catch_high: info[5] as i32,
        object: info[9],
        throw_info: info[10],
        throw_base: info[11],
        rethrown: false,
        exited: false,
    });
    st().uncaught -= 1;
    let f: unsafe extern "C" fn(u64, u64) -> u64 = unsafe { core::mem::transmute(funclet) };
    let mut continuation = unsafe { f(0, root) };
    // FH4 funclets may return an index into the handler's continuations.
    let count = info[12];
    if count > 0 && continuation < count {
        continuation = info[6 + continuation as usize];
    }
    let a = pop_active().unwrap();
    if !a.rethrown {
        unsafe { destroy_object(a.object, a.throw_info, a.throw_base) };
    }
    continuation
}

unsafe fn destroy_object(object: u64, throw_info: u64, base: u64) {
    let dtor = unsafe { u32_at(throw_info + 4) };
    if dtor != 0 && object != 0 {
        let f: unsafe extern "C" fn(u64) = unsafe { core::mem::transmute(base + dtor as u64) };
        unsafe { f(object) };
    }
}

/// The frame handler proper, for either metadata format. FH3 numbers a
/// catch funclet's states within its parent's; FH4 gives each funclet
/// tables of its own (`own_states`), which then apply as they are.
unsafe fn frame_handler(rec: *mut Record, frame: u64, ctx: *mut u8, dc: &mut Dispatcher, m: &Meta, own_states: bool) -> u32 {
    let base = dc.image_base;
    let begin = base + unsafe { (*dc.function_entry)[0] } as u64;
    let pc_state = m.state_at((dc.control_pc - base) as u32);
    let flags = unsafe { (*rec).flags };

    // Is this frame a running catch block? Its locals live in the parent's
    // frame, and only try blocks inside the catch block apply to it.
    let funclet = active().iter().rev().flatten().find(|a| a.funclet == begin && !a.exited).copied();
    let root = funclet.map_or(frame, |a| a.root);
    // Or a frame whose catch block (further down) threw: only try blocks
    // around that try/catch apply, and its state is the try's.
    let running = active().iter().rev().flatten().find(|a| a.try_frame == frame).copied();

    if flags & (UNWINDING | EXIT_UNWIND) != 0 {
        let ours = unsafe { (*rec).code } == STATUS_UNWIND_CONSOLIDATE && unsafe { (*rec).info[0] } == call_catch_block as *const () as u64;
        let target = flags & TARGET_UNWIND != 0;
        let target_state = if target && ours { (unsafe { (*rec).info[3] }) as i32 } else { -2 };
        if let Some(a) = funclet {
            // A catch block being left: unwind only its own states; the
            // exception it caught is finished (unless rethrown).
            if own_states {
                let to = if target { target_state } else { -1 };
                unsafe { unwind_states(m, root, pc_state, to, |_| true) };
            } else {
                let to = if target { target_state } else { a.high };
                unsafe { unwind_states(m, root, pc_state, to, |s| s > a.high && s <= a.catch_high) };
            }
            if !target {
                if let Some(e) = active().iter_mut().rev().flatten().find(|e| e.funclet == begin && !e.exited) {
                    e.exited = true;
                    if !e.rethrown {
                        unsafe { destroy_object(e.object, e.throw_info, e.throw_base) };
                    }
                }
            }
        } else {
            let from = running.map_or(pc_state, |a| a.low);
            let to = if target { if ours { target_state } else { from } } else { -1 };
            unsafe { unwind_states(m, root, from, to, |_| true) };
            // This frame's running catch blocks are over.
            while active().last().is_some_and(|a| a.is_some_and(|a| a.try_frame == frame)) {
                pop_active();
            }
        }
        return CONTINUE_SEARCH;
    }

    let r = unsafe { &*rec };
    if r.code != CXX_EXCEPTION || r.count < 3 || (r.info[0] & !0xF) != CXX_MAGIC & !0xF {
        return CONTINUE_SEARCH; // not a C++ exception: /EHs code doesn't catch these
    }
    let (object, throw_info, tbase) = (r.info[1], r.info[2], if r.count >= 4 { r.info[3] } else { 0 });
    let attrs = unsafe { u32_at(throw_info) };
    let types = tbase + unsafe { u32_at(throw_info + 12) } as u64;
    let ntypes = unsafe { i32_at(types) }.max(0) as u64;

    for t in &m.tries {
        let covers = t.low <= pc_state && pc_state <= t.high;
        let applies = match (funclet, running) {
            (Some(_), _) if own_states => covers,
            (Some(a), _) => covers && t.low > a.high && t.high <= a.catch_high,
            (None, Some(a)) => t.low < a.low && t.high >= a.high,
            (None, None) => covers,
        };
        if !applies {
            continue;
        }
        for h in &t.handlers {
            for k in 0..ntypes {
                let ct = tbase + unsafe { u32_at(types + 4 + 4 * k) } as u64;
                if !unsafe { type_matches(h, ct, tbase, attrs) } {
                    continue;
                }
                // Caught: build the parameter, then unwind to this frame
                // and run the catch block (call_catch_block).
                unsafe { build_catch_object(h, root, object, ct, tbase) };
                let mut c = Record { code: STATUS_UNWIND_CONSOLIDATE, flags: 1, record: rec, address: 0, count: 13, info: [0; 15] };
                c.info[0] = call_catch_block as *const () as u64;
                c.info[1] = root;
                c.info[2] = h.funclet;
                c.info[3] = t.low as u64;
                c.info[4] = t.high as u64;
                c.info[5] = t.catch_high as u64;
                c.info[6] = h.continuations[0];
                c.info[7] = h.continuations[1];
                c.info[8] = frame;
                c.info[9] = object;
                c.info[10] = throw_info;
                c.info[11] = tbase;
                c.info[12] = h.continuation_count as u64;
                unsafe { RtlUnwindEx(frame, dc.control_pc, &mut c, 0, ctx, dc.history_table) };
            }
        }
    }
    // noexcept: nothing may leave it.
    if m.noexcept && funclet.is_none() {
        crate::startup::terminate();
    }
    CONTINUE_SEARCH
}

/// The C++ frame handler for FH3 metadata (clang; MSVC before 2019).
#[no_mangle]
pub unsafe extern "C" fn __CxxFrameHandler3(rec: *mut Record, frame: u64, ctx: *mut u8, dc: *mut Dispatcher) -> u32 {
    let dc = unsafe { &mut *dc };
    let base = dc.image_base;
    let m = unsafe { read_fh3(base + u32_at(dc.handler_data as u64) as u64, base) };
    unsafe { frame_handler(rec, frame, ctx, dc, &m, false) }
}

/// The C++ frame handler for FH4 metadata (MSVC 2019 and later), from
/// vcruntime140_1.dll.
#[no_mangle]
pub unsafe extern "C" fn __CxxFrameHandler4(rec: *mut Record, frame: u64, ctx: *mut u8, dc: *mut Dispatcher) -> u32 {
    let dc = unsafe { &mut *dc };
    let base = dc.image_base;
    let start = unsafe { (*dc.function_entry)[0] };
    let (m, catch_funclet) = unsafe { read_fh4(base + u32_at(dc.handler_data as u64) as u64, base, start) };
    unsafe { frame_handler(rec, frame, ctx, dc, &m, catch_funclet.is_some()) }
}

#[no_mangle]
pub unsafe extern "C" fn __CxxFrameHandler(rec: *mut Record, frame: u64, ctx: *mut u8, dc: *mut Dispatcher) -> u32 {
    unsafe { __CxxFrameHandler3(rec, frame, ctx, dc) }
}

#[no_mangle]
pub unsafe extern "C" fn __CxxFrameHandler2(rec: *mut Record, frame: u64, ctx: *mut u8, dc: *mut Dispatcher) -> u32 {
    unsafe { __CxxFrameHandler3(rec, frame, ctx, dc) }
}

/// `/GS` functions with exception handlers: the cookie check is the
/// compiler's; just pass the exception on.
#[no_mangle]
pub unsafe extern "C" fn __GSHandlerCheck(_rec: *mut Record, _frame: u64, _ctx: *mut u8, _dc: *mut Dispatcher) -> u32 {
    CONTINUE_SEARCH
}

#[no_mangle]
pub unsafe extern "C" fn __GSHandlerCheck_SEH(rec: *mut Record, frame: u64, ctx: *mut u8, dc: *mut Dispatcher) -> u32 {
    unsafe { __C_specific_handler(rec, frame, ctx, dc) }
}

#[no_mangle]
pub unsafe extern "C" fn __GSHandlerCheck_EH(rec: *mut Record, frame: u64, ctx: *mut u8, dc: *mut Dispatcher) -> u32 {
    unsafe { __CxxFrameHandler3(rec, frame, ctx, dc) }
}

#[no_mangle]
pub unsafe extern "C" fn __GSHandlerCheck_EH4(rec: *mut Record, frame: u64, ctx: *mut u8, dc: *mut Dispatcher) -> u32 {
    unsafe { __CxxFrameHandler4(rec, frame, ctx, dc) }
}

// -----| msvcrt's std::exception |-----
//
// { vftable, what, owns-what } with a vftable of { scalar deleting
// destructor, what }.

#[repr(C)]
pub struct Exception {
    vftable: *const usize,
    what: *const u8,
    owned: u64,
}

/// The scalar deleting destructor (only reached through the vftable).
pub unsafe extern "C" fn exception_deleting_dtor(this: *mut Exception, flags: u32) -> *mut Exception {
    unsafe { exception_dtor(this) };
    if flags & 1 != 0 {
        crate::heap::free(this as *mut u8);
    }
    this
}

#[export_name = "?what@exception@@UEBAPEBDXZ"]
pub unsafe extern "C" fn exception_what(this: *const Exception) -> *const u8 {
    let w = unsafe { (*this).what };
    if w.is_null() { b"Unknown exception\0".as_ptr() } else { w }
}

#[repr(C)]
pub struct Vtable([unsafe extern "C" fn(); 2]);

/// `??_7exception@@6B@`: the class's vftable.
#[export_name = "??_7exception@@6B@"]
pub static EXCEPTION_VFTABLE: Vtable = Vtable(unsafe {
    [
        core::mem::transmute::<unsafe extern "C" fn(*mut Exception, u32) -> *mut Exception, unsafe extern "C" fn()>(exception_deleting_dtor),
        core::mem::transmute::<unsafe extern "C" fn(*const Exception) -> *const u8, unsafe extern "C" fn()>(exception_what),
    ]
});

fn vftable() -> *const usize {
    &EXCEPTION_VFTABLE as *const Vtable as *const usize
}

#[export_name = "??0exception@@QEAA@XZ"]
pub unsafe extern "C" fn exception_ctor(this: *mut Exception) -> *mut Exception {
    unsafe { *this = Exception { vftable: vftable(), what: core::ptr::null(), owned: 0 } };
    this
}

/// exception(const char* const&): keeps a copy of the text.
#[export_name = "??0exception@@QEAA@AEBQEBD@Z"]
pub unsafe extern "C" fn exception_ctor_text(this: *mut Exception, what: *const *const u8) -> *mut Exception {
    let text = unsafe { *what };
    let copy = if text.is_null() { core::ptr::null() } else { unsafe { crate::string::_strdup(text) as *const u8 } };
    unsafe { *this = Exception { vftable: vftable(), what: copy, owned: 1 } };
    this
}

/// exception(const char* const&, int): uses the text as it is.
#[export_name = "??0exception@@QEAA@AEBQEBDH@Z"]
pub unsafe extern "C" fn exception_ctor_noalloc(this: *mut Exception, what: *const *const u8, _unused: i32) -> *mut Exception {
    unsafe { *this = Exception { vftable: vftable(), what: *what, owned: 0 } };
    this
}

#[export_name = "??0exception@@QEAA@AEBV0@@Z"]
pub unsafe extern "C" fn exception_copy(this: *mut Exception, other: *const Exception) -> *mut Exception {
    let o = unsafe { &*other };
    let what = if o.owned != 0 && !o.what.is_null() { unsafe { crate::string::_strdup(o.what) as *const u8 } } else { o.what };
    unsafe { *this = Exception { vftable: vftable(), what, owned: o.owned } };
    this
}

#[export_name = "??4exception@@QEAAAEAV0@AEBV0@@Z"]
pub unsafe extern "C" fn exception_assign(this: *mut Exception, other: *const Exception) -> *mut Exception {
    if this as *const Exception != other {
        unsafe {
            exception_dtor(this);
            exception_copy(this, other);
        }
    }
    this
}

#[export_name = "??1exception@@UEAA@XZ"]
pub unsafe extern "C" fn exception_dtor(this: *mut Exception) {
    unsafe {
        if (*this).owned != 0 && !(*this).what.is_null() {
            crate::heap::free((*this).what as *mut u8);
        }
        (*this).what = core::ptr::null();
        (*this).owned = 0;
    }
}

#[export_name = "??1type_info@@UEAA@XZ"]
pub extern "C" fn type_info_dtor(_this: *mut u8) {}

#[export_name = "?name@type_info@@QEBAPEBDXZ"]
pub unsafe extern "C" fn type_info_name(this: *const u8) -> *const u8 {
    unsafe { this.add(16) }
}

#[export_name = "?raw_name@type_info@@QEBAPEBDXZ"]
pub unsafe extern "C" fn type_info_raw_name(this: *const u8) -> *const u8 {
    unsafe { this.add(16) }
}
