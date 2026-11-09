// =============================================================================
// VEILOS: named objects, address waits and named pipes (WIN32_DESIGN.md, S1)
// =============================================================================
//
// Named objects: one table of (name, kind) -> object, shared by every process.
// An entry holds a reference of its own, and goes once it's the only one left.
//
// Address waits (WaitOnAddress): a table of sequence numbers hashed by address.
// A waiter reads the number, compares the memory, and sleeps until the number
// moves; a wake moves it. Colliding addresses just wake early, which the API
// allows callers to see anyway.
//
// Named pipes: an instance is two of nt_process's byte pipes, one each way. Its
// server end exists from creation, and a client takes the oldest instance
// nobody has connected to. Messages aren't kept apart: pipes are byte streams.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use nt_abi::{status, NtStatus};
use spin::Mutex;

use crate::nt_file::ptr_ok;
use crate::nt_objects::KernelObjectRef;
use crate::nt_process::{new_pipe, pipe_available, Pipe, PipeEnd};
use crate::process::Process;

const STATUS_OBJECT_NAME_NOT_FOUND: NtStatus = status::OBJECT_NAME_NOT_FOUND;
const STATUS_OBJECT_TYPE_MISMATCH: NtStatus = 0xC000_0024u32 as NtStatus;
const STATUS_PIPE_NOT_AVAILABLE: NtStatus = 0xC000_00ACu32 as NtStatus;
const STATUS_PIPE_LISTENING: NtStatus = 0xC000_00B3u32 as NtStatus;
const STATUS_PIPE_CONNECTED: NtStatus = 0xC000_00B2u32 as NtStatus;
const STATUS_IO_TIMEOUT: NtStatus = 0xC000_00B5u32 as NtStatus;
const STATUS_TIMEOUT: NtStatus = 0x0000_0102;
const NAME_MAX: usize = 520;

/// A NUL-terminated UTF-16 string from the caller.
fn read_name(p: u64) -> Option<String> {
    let mut units = Vec::new();
    while units.len() < NAME_MAX {
        let a = p + units.len() as u64 * 2;
        if !ptr_ok(a, 2) {
            return None;
        }
        let u = unsafe { core::ptr::read_unaligned(a as *const u16) };
        if u == 0 {
            return Some(char::decode_utf16(units).map(|c| c.unwrap_or('?')).collect());
        }
        units.push(u);
    }
    None
}

// -----| small questions |-----

/// VEIL_SYSINFO(op, out*, size).
pub fn sys_info(op: u64, out: u64, size: u64) -> NtStatus {
    if !ptr_ok(out, size as usize) {
        return status::ACCESS_VIOLATION;
    }
    match op {
        0 => {
            fill_random(unsafe { core::slice::from_raw_parts_mut(out as *mut u8, size as usize) });
            status::SUCCESS
        }
        1 if size >= 16 => {
            let free = crate::memory::free_frame_count() as u64 * 4096;
            let total = crate::memory::total_frame_count() as u64 * 4096;
            unsafe {
                *(out as *mut u64) = total.max(free);
                *(out as *mut u64).add(1) = free;
            }
            status::SUCCESS
        }
        // Missing calls return 0 for this process (VEILOS_MISSING_CALLS=zero).
        2 => match crate::process::PROCESSOR.lock().get_active() {
            Some(p) => {
                p.missing_calls_return_zero.store(true, core::sync::atomic::Ordering::Relaxed);
                status::SUCCESS
            }
            None => status::INVALID_HANDLE,
        },
        // Log this process's system calls (VEILOS_TRACE_SYSCALLS=1).
        3 => match crate::process::PROCESSOR.lock().get_active() {
            Some(p) => {
                p.trace_syscalls.store(true, core::sync::atomic::Ordering::Relaxed);
                status::SUCCESS
            }
            None => status::INVALID_HANDLE,
        },
        // Log only its socket calls and waits (VEILOS_TRACE_NET=1).
        4 => match crate::process::PROCESSOR.lock().get_active() {
            Some(p) => {
                p.trace_net.store(true, core::sync::atomic::Ordering::Relaxed);
                status::SUCCESS
            }
            None => status::INVALID_HANDLE,
        },
        _ => status::INVALID_PARAMETER,
    }
}

/// Random bytes: RDRAND where the CPU has it. Elsewhere (QEMU's default CPU)
/// this is the time stamp counter stirred by a generator, good enough for hash
/// tables and temp names but NOT for keys.
fn fill_random(out: &mut [u8]) {
    static STATE: AtomicU64 = AtomicU64::new(0x9E37_79B9_7F4A_7C15);
    let has_rdrand = unsafe { core::arch::x86_64::__cpuid(1) }.ecx & (1 << 30) != 0;
    for chunk in out.chunks_mut(8) {
        let mut v = 0u64;
        let got = has_rdrand && unsafe { core::arch::x86_64::_rdrand64_step(&mut v) } == 1;
        if !got {
            // SplitMix64, with the clock mixed in on every step.
            let mut z = STATE.fetch_add(0x9E37_79B9_7F4A_7C15 ^ crate::time::rdtsc(), Ordering::Relaxed);
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            v = z ^ (z >> 31);
        }
        chunk.copy_from_slice(&v.to_le_bytes()[..chunk.len()]);
    }
}

// -----| named objects |-----

static NAMED: Mutex<BTreeMap<String, (u32, KernelObjectRef)>> = Mutex::new(BTreeMap::new());

fn still_held(o: &KernelObjectRef) -> bool {
    match o {
        KernelObjectRef::Event(a) => Arc::strong_count(a) > 1,
        KernelObjectRef::Semaphore(a) => Arc::strong_count(a) > 1,
        KernelObjectRef::Mutex(a) => Arc::strong_count(a) > 1,
        KernelObjectRef::Section(a) => Arc::strong_count(a) > 1,
        KernelObjectRef::Directory(a) => Arc::strong_count(a) > 1,
        KernelObjectRef::Timer(a) => Arc::strong_count(a) > 1,
        KernelObjectRef::Link(a) => Arc::strong_count(a) > 1,
        _ => false,
    }
}

// Kinds in the table (kernel32 uses the first three by number).
pub const KIND_EVENT: u32 = 1;
pub const KIND_MUTEX: u32 = 2;
pub const KIND_SEMAPHORE: u32 = 3;
pub const KIND_SECTION: u32 = 4;
pub const KIND_DIRECTORY: u32 = 5;
pub const KIND_TIMER: u32 = 6;
pub const KIND_LINK: u32 = 7;

const STATUS_OBJECT_NAME_EXISTS: NtStatus = 0x4000_0000;
const STATUS_OBJECT_NAME_COLLISION: NtStatus = 0xC000_0035u32 as NtStatus;
const OBJ_OPENIF: u32 = 0x80;

/// One namespace, one session: Win32's Global\ and Local\, NT's
/// \BaseNamedObjects and each session's, and \Sessions\BNOLINKS\N all lead
/// to the same place. The name without any of them.
pub fn canonical(name: &str) -> String {
    fn strip<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
        (s.len() >= prefix.len() && s.is_char_boundary(prefix.len()) && s[..prefix.len()].eq_ignore_ascii_case(prefix)).then(|| &s[prefix.len()..])
    }
    fn digits(s: &str) -> &str {
        s.trim_start_matches(|c: char| c.is_ascii_digit())
    }
    let mut n = name;
    loop {
        let before = n.len();
        if let Some(r) = strip(n, "\\Sessions\\BNOLINKS\\") {
            n = digits(r);
        } else if let Some(r) = strip(n, "\\Sessions\\") {
            let r = digits(r);
            n = strip(r, "\\BaseNamedObjects").unwrap_or(r);
        } else if let Some(r) = strip(n, "\\BaseNamedObjects") {
            n = r;
        } else if let Some(r) = strip(n, "Global\\").or_else(|| strip(n, "Local\\")) {
            n = r;
        }
        n = n.trim_start_matches('\\');
        if n.len() == before {
            return String::from(n);
        }
    }
}

/// A UNICODE_STRING (Length, MaximumLength, Buffer at 8) from the caller.
fn read_unicode_string(us: u64) -> Option<String> {
    if !ptr_ok(us, 16) {
        return None;
    }
    let len = unsafe { core::ptr::read_unaligned(us as *const u16) } as usize / 2;
    let buf = unsafe { core::ptr::read_unaligned((us + 8) as *const u64) };
    if len > NAME_MAX || (len > 0 && !ptr_ok(buf, len * 2)) {
        return None;
    }
    let units = unsafe { core::slice::from_raw_parts(buf as *const u16, len) };
    Some(char::decode_utf16(units.iter().copied()).map(|c| c.unwrap_or('?')).collect())
}

/// The name OBJECT_ATTRIBUTES give (ObjectName under RootDirectory, if a
/// directory object), canonical; and its Attributes. None: no name.
pub fn attr_name(proc: &Process, attrs: u64) -> Result<(Option<String>, u32), NtStatus> {
    if attrs == 0 {
        return Ok((None, 0));
    }
    if !ptr_ok(attrs, 48) {
        return Err(status::ACCESS_VIOLATION);
    }
    let (root, name_ptr, flags) = unsafe {
        (
            core::ptr::read_unaligned((attrs + 8) as *const u64),
            core::ptr::read_unaligned((attrs + 16) as *const u64),
            core::ptr::read_unaligned((attrs + 24) as *const u32),
        )
    };
    if name_ptr == 0 {
        return Ok((None, flags));
    }
    let name = read_unicode_string(name_ptr).ok_or(status::ACCESS_VIOLATION)?;
    let full = if root != 0 {
        match crate::nt_process::object(proc, root) {
            Some(KernelObjectRef::Directory(d)) if d.name.is_empty() => name,
            Some(KernelObjectRef::Directory(d)) => alloc::format!("{}\\{}", d.name, name.trim_start_matches('\\')),
            Some(_) => return Err(STATUS_OBJECT_TYPE_MISMATCH),
            None => return Err(status::INVALID_HANDLE),
        }
    } else {
        name
    };
    Ok((Some(canonical(&full)), flags))
}

/// Create an object of `kind` (made by `make`), named as `attrs` say. A name
/// in use: with OBJ_OPENIF the object there (STATUS_OBJECT_NAME_EXISTS),
/// otherwise STATUS_OBJECT_NAME_COLLISION.
pub fn create_named(proc: &Process, out: u64, access: u64, attrs: u64, kind: u32, make: impl FnOnce() -> KernelObjectRef) -> NtStatus {
    if !ptr_ok(out, 8) {
        return status::ACCESS_VIOLATION;
    }
    let (name, flags) = match attr_name(proc, attrs) {
        Ok(n) => n,
        Err(s) => return s,
    };
    let mut table = NAMED.lock();
    let (obj, st) = match name {
        Some(name) if !name.is_empty() => {
            table.retain(|_, (_, o)| still_held(o));
            match table.get(&name) {
                Some((k, o)) if *k == kind && flags & OBJ_OPENIF != 0 => (o.clone(), STATUS_OBJECT_NAME_EXISTS),
                Some(_) => return STATUS_OBJECT_NAME_COLLISION,
                None => {
                    let o = make();
                    table.insert(name, (kind, o.clone()));
                    (o, status::SUCCESS)
                }
            }
        }
        _ => (make(), status::SUCCESS),
    };
    drop(table);
    let h = proc.handles.lock().allocate_ref(obj, access as u32, 0);
    unsafe { *(out as *mut u64) = h as u64 };
    if proc.trace_syscalls.load(Ordering::Relaxed) {
        let name = attr_name(proc, attrs).ok().and_then(|(n, _)| n).unwrap_or_default();
        // Who asked: code addresses among the caller's first stack words.
        let rsp = crate::smp::current_syscall_user_rsp();
        let mut trail = String::new();
        for i in 0..40u64 {
            let a = rsp + i * 8;
            if !ptr_ok(a, 8) {
                break;
            }
            let v = unsafe { core::ptr::read_unaligned(a as *const u64) };
            if (0x1_0000_0000..0x3_0000_0000).contains(&v) {
                trail.push_str(&alloc::format!(" {:x}", v));
            }
        }
        crate::serial_println!("[OBJ] pid {}: kind {} {:#x} {:?} -> {:#x} from{}", proc.pid, kind, h, name, st as u32, trail);
    }
    st
}

/// NtOpen{Event,Mutant,Semaphore,Section,DirectoryObject}(out*, access, attrs*).
/// The namespace's root directory always exists.
pub fn open_by_attrs(proc: &Process, out: u64, access: u64, attrs: u64, kind: u32) -> NtStatus {
    if !ptr_ok(out, 8) {
        return status::ACCESS_VIOLATION;
    }
    let name = match attr_name(proc, attrs) {
        Ok((Some(n), _)) => n,
        Ok((None, _)) => return 0xC000_0033u32 as NtStatus, // STATUS_OBJECT_NAME_INVALID
        Err(s) => return s,
    };
    // A device, not a link: how Cygwin finds out that a device exists.
    if kind == KIND_LINK && name.eq_ignore_ascii_case("Device\\Null") {
        return STATUS_OBJECT_TYPE_MISMATCH;
    }
    let obj = if kind == KIND_DIRECTORY && name.is_empty() {
        KernelObjectRef::Directory(Arc::new(crate::nt_objects::DirectoryObject { name }))
    } else {
        let mut table = NAMED.lock();
        table.retain(|_, (_, o)| still_held(o));
        match table.get(&name) {
            None => return STATUS_OBJECT_NAME_NOT_FOUND,
            Some((k, _)) if *k != kind => return STATUS_OBJECT_TYPE_MISMATCH,
            Some((_, o)) => o.clone(),
        }
    };
    let h = proc.handles.lock().allocate_ref(obj, access as u32, 0);
    unsafe { *(out as *mut u64) = h as u64 };
    status::SUCCESS
}

/// NtCreateSymbolicLinkObject(out*, access, attrs*, target*).
pub fn create_link(proc: &Process, out: u64, access: u64, attrs: u64, target: u64) -> NtStatus {
    if !ptr_ok(target, 16) {
        return status::ACCESS_VIOLATION;
    }
    let len = unsafe { core::ptr::read_unaligned(target as *const u16) } as usize / 2;
    let buf = unsafe { core::ptr::read_unaligned((target + 8) as *const u64) };
    if len > NAME_MAX || (len > 0 && !ptr_ok(buf, len * 2)) {
        return status::ACCESS_VIOLATION;
    }
    let units = unsafe { core::slice::from_raw_parts(buf as *const u16, len) }.to_vec();
    create_named(proc, out, access, attrs, KIND_LINK, move || {
        KernelObjectRef::Link(Arc::new(crate::nt_objects::LinkObject { target: units }))
    })
}

/// NtQuerySymbolicLinkObject(link, target* (a UNICODE_STRING to fill), length*).
pub fn query_link(proc: &Process, h: u64, target: u64, ret: u64) -> NtStatus {
    let Some(KernelObjectRef::Link(link)) = crate::nt_process::object(proc, h) else { return status::INVALID_HANDLE };
    if !ptr_ok(target, 16) {
        return status::ACCESS_VIOLATION;
    }
    let bytes = link.target.len() * 2;
    if ptr_ok(ret, 4) {
        unsafe { core::ptr::write_unaligned(ret as *mut u32, bytes as u32) };
    }
    let max = unsafe { core::ptr::read_unaligned((target + 2) as *const u16) } as usize;
    let buf = unsafe { core::ptr::read_unaligned((target + 8) as *const u64) };
    if max < bytes || !ptr_ok(buf, bytes) {
        return status::BUFFER_TOO_SMALL;
    }
    unsafe {
        core::ptr::copy_nonoverlapping(link.target.as_ptr(), buf as *mut u16, link.target.len());
        core::ptr::write_unaligned(target as *mut u16, bytes as u16);
    }
    status::SUCCESS
}

/// NtCreateDirectoryObject(out*, access, attrs*).
pub fn create_directory(proc: &Process, out: u64, access: u64, attrs: u64) -> NtStatus {
    let name = match attr_name(proc, attrs) {
        Ok((n, _)) => n.unwrap_or_default(),
        Err(s) => return s,
    };
    create_named(proc, out, access, attrs, KIND_DIRECTORY, move || {
        KernelObjectRef::Directory(Arc::new(crate::nt_objects::DirectoryObject { name }))
    })
}

/// VEIL_NAME_OBJECT(handle, name*, kind): give the object at `handle` a name.
pub fn name_object(proc: &Process, h: u64, name: u64, kind: u64) -> NtStatus {
    let Some(name) = read_name(name).map(|n| canonical(&n)) else { return status::ACCESS_VIOLATION };
    let Some(obj) = crate::nt_process::object(proc, h) else { return status::INVALID_HANDLE };
    let mut table = NAMED.lock();
    table.retain(|_, (_, o)| still_held(o));
    table.insert(name, (kind as u32, obj));
    status::SUCCESS
}

/// VEIL_OPEN_NAMED(name*, kind, out*). A name held by another kind of object
/// is STATUS_OBJECT_TYPE_MISMATCH.
pub fn open_named(proc: &Process, name: u64, kind: u64, out: u64) -> NtStatus {
    if !ptr_ok(out, 8) {
        return status::ACCESS_VIOLATION;
    }
    let Some(name) = read_name(name).map(|n| canonical(&n)) else { return status::ACCESS_VIOLATION };
    let found = {
        let mut table = NAMED.lock();
        table.retain(|_, (_, o)| still_held(o));
        table.get(&name).cloned()
    };
    match found {
        None => STATUS_OBJECT_NAME_NOT_FOUND,
        Some((k, _)) if k != kind as u32 => STATUS_OBJECT_TYPE_MISMATCH,
        Some((_, obj)) => {
            let h = proc.handles.lock().allocate_ref(obj, 0x1F_0003, 0);
            unsafe { *(out as *mut u64) = h as u64 };
            status::SUCCESS
        }
    }
}

// -----| NT-style creation, queries and timers |-----

/// NtCreateMutant(out*, access, attrs*, initially_owned).
pub fn create_mutant(proc: &Process, out: u64, access: u64, attrs: u64, owned: u64) -> NtStatus {
    use crate::nt_objects::MutexObject;
    crate::nt_ipc::create_named(proc, out, access, attrs, KIND_MUTEX, || {
        let m = Arc::new(MutexObject::new(None, false));
        if owned as u8 != 0 {
            m.try_acquire(MutexObject::current_owner());
        }
        KernelObjectRef::Mutex(m)
    })
}

/// NtCreateEvent(out*, access, attrs*, type, initial): type 0 notification
/// (manual reset), 1 synchronization.
pub fn create_event(proc: &Process, out: u64, access: u64, attrs: u64, kind: u64, initial: u64) -> NtStatus {
    create_named(proc, out, access, attrs, KIND_EVENT, || {
        KernelObjectRef::Event(Arc::new(crate::nt_objects::EventObject::new(None, kind as u32 == 1, initial as u8 != 0)))
    })
}

/// NtCreateSemaphore(out*, access, attrs*, initial, maximum).
pub fn create_semaphore(proc: &Process, out: u64, access: u64, attrs: u64, initial: i32, max: i32) -> NtStatus {
    if max <= 0 || initial < 0 || initial > max {
        return status::INVALID_PARAMETER;
    }
    create_named(proc, out, access, attrs, KIND_SEMAPHORE, || {
        KernelObjectRef::Semaphore(Arc::new(crate::nt_objects::SemaphoreObject::new(None, initial, max)))
    })
}

/// Write an 8-byte basic-information answer (two i32s) for a query.
fn answer(out: u64, length: u64, ret: u64, a: i32, b: i32) -> NtStatus {
    if length < 8 {
        return 0xC000_0004u32 as NtStatus; // STATUS_INFO_LENGTH_MISMATCH
    }
    if !ptr_ok(out, 8) {
        return status::ACCESS_VIOLATION;
    }
    unsafe {
        core::ptr::write_unaligned(out as *mut i32, a);
        core::ptr::write_unaligned((out + 4) as *mut i32, b);
    }
    if ptr_ok(ret, 4) {
        unsafe { core::ptr::write_unaligned(ret as *mut u32, 8) };
    }
    status::SUCCESS
}

/// NtQueryEvent(h, EventBasicInformation, info*, length, ret*): type, state.
pub fn query_event(proc: &Process, h: u64, class: u64, out: u64, length: u64, ret: u64) -> NtStatus {
    let Some(KernelObjectRef::Event(e)) = crate::nt_process::object(proc, h) else { return status::INVALID_HANDLE };
    if class != 0 {
        return status::INVALID_INFO_CLASS;
    }
    answer(out, length, ret, e.auto_reset as i32, e.signaled.load(Ordering::Acquire) as i32)
}

/// NtQuerySemaphore(h, SemaphoreBasicInformation, info*, length, ret*): count, maximum.
pub fn query_semaphore(proc: &Process, h: u64, class: u64, out: u64, length: u64, ret: u64) -> NtStatus {
    let Some(KernelObjectRef::Semaphore(sem)) = crate::nt_process::object(proc, h) else { return status::INVALID_HANDLE };
    if class != 0 {
        return status::INVALID_INFO_CLASS;
    }
    answer(out, length, ret, sem.current_count(), sem.max_count)
}

/// NtCreateTimer(out*, access, attrs*, type): 0 notification, 1 synchronization.
pub fn create_timer(proc: &Process, out: u64, access: u64, attrs: u64, kind: u64) -> NtStatus {
    create_named(proc, out, access, attrs, KIND_TIMER, || {
        KernelObjectRef::Timer(Arc::new(crate::nt_objects::TimerObject::new(kind as u32 == 0)))
    })
}

fn timer(proc: &Process, h: u64) -> Option<Arc<crate::nt_objects::TimerObject>> {
    match crate::nt_process::object(proc, h)? {
        KernelObjectRef::Timer(t) => Some(t),
        _ => None,
    }
}

/// NtSetTimer(h, due*, apc, context, resume, period_ms, previous*). Due
/// times in 100 ns units, negative relative, else a FILETIME. APC routines
/// aren't run.
pub fn set_timer(proc: &Process, h: u64, due: u64) -> NtStatus {
    let period = crate::interrupts::syscall_stack_arg(5).unwrap_or(0) as u32 as i32;
    let prev = crate::interrupts::syscall_stack_arg(6).unwrap_or(0);
    let Some(t) = timer(proc, h) else { return status::INVALID_HANDLE };
    if !ptr_ok(due, 8) {
        return status::ACCESS_VIOLATION;
    }
    let d = unsafe { core::ptr::read_unaligned(due as *const i64) };
    let now = crate::interrupts::TICKS.load(Ordering::Relaxed);
    let ms = if d < 0 {
        d.unsigned_abs().div_ceil(10_000)
    } else {
        (d as u64).saturating_sub(crate::kusd::system_time()).div_ceil(10_000)
    };
    let was = t.fired.swap(false, Ordering::AcqRel);
    t.period.store(period.max(0) as u32, Ordering::Release);
    t.due.store((now + ms).max(1), Ordering::Release);
    if ptr_ok(prev, 1) {
        unsafe { *(prev as *mut u8) = was as u8 };
    }
    status::SUCCESS
}

/// NtCancelTimer(h, current*).
pub fn cancel_timer(proc: &Process, h: u64, current: u64) -> NtStatus {
    let Some(t) = timer(proc, h) else { return status::INVALID_HANDLE };
    let was = t.check();
    t.due.store(0, Ordering::Release);
    if ptr_ok(current, 1) {
        unsafe { *(current as *mut u8) = was as u8 };
    }
    status::SUCCESS
}

/// NtQueryTimer(h, TimerBasicInformation, info*, length, ret*): the time
/// left (100 ns units), and whether it's signalled.
pub fn query_timer(proc: &Process, h: u64, class: u64, out: u64, length: u64, ret: u64) -> NtStatus {
    let Some(t) = timer(proc, h) else { return status::INVALID_HANDLE };
    if class != 0 {
        return status::INVALID_INFO_CLASS;
    }
    if length < 16 {
        return 0xC000_0004u32 as NtStatus;
    }
    if !ptr_ok(out, 16) {
        return status::ACCESS_VIOLATION;
    }
    let state = t.check();
    let due = t.due.load(Ordering::Acquire);
    let now = crate::interrupts::TICKS.load(Ordering::Relaxed);
    let left = if due > now { (due - now) as i64 * 10_000 } else { 0 };
    unsafe {
        core::ptr::write_unaligned(out as *mut i64, left);
        core::ptr::write_unaligned((out + 8) as *mut u8, state as u8);
    }
    if ptr_ok(ret, 4) {
        unsafe { core::ptr::write_unaligned(ret as *mut u32, 16) };
    }
    status::SUCCESS
}

/// VEIL_MUTEX(op, a, b). Op 0, create: initially owned, out*. Op 1, release: handle.
pub fn mutex_call(proc: &Process, op: u64, a: u64, b: u64) -> NtStatus {
    use crate::nt_objects::MutexObject;
    match op {
        0 => {
            if !ptr_ok(b, 8) {
                return status::ACCESS_VIOLATION;
            }
            let m = Arc::new(MutexObject::new(None, false));
            if a != 0 {
                m.try_acquire(MutexObject::current_owner());
            }
            let h = proc.handles.lock().allocate_mutex(m, 0x1F_0001);
            unsafe { *(b as *mut u64) = h as u64 };
            status::SUCCESS
        }
        1 => match crate::nt_process::object(proc, a) {
            Some(KernelObjectRef::Mutex(m)) => match m.release(MutexObject::current_owner()) {
                Ok(_) => status::SUCCESS,
                Err(_) => 0xC000_0046u32 as NtStatus, // STATUS_MUTANT_NOT_OWNED
            },
            _ => status::INVALID_HANDLE,
        },
        _ => status::INVALID_PARAMETER,
    }
}

// -----| address waits |-----

const WAKE_SLOTS: usize = 256;
static WAKES: [AtomicU64; WAKE_SLOTS] = [const { AtomicU64::new(0) }; WAKE_SLOTS];

fn slot(addr: u64) -> &'static AtomicU64 {
    &WAKES[((addr >> 3) ^ (addr >> 11)) as usize % WAKE_SLOTS]
}

/// VEIL_WAIT_ADDRESS(addr, compare*, size, ms; u64::MAX: forever).
pub fn wait_address(addr: u64, compare: u64, size: u64, ms: u64) -> NtStatus {
    let size = size as usize;
    if !matches!(size, 1 | 2 | 4 | 8) {
        return status::INVALID_PARAMETER;
    }
    if !ptr_ok(addr, size) || !ptr_ok(compare, size) {
        return status::ACCESS_VIOLATION;
    }
    let seq = slot(addr);
    let seen = seq.load(Ordering::SeqCst);
    let same = unsafe {
        core::slice::from_raw_parts(addr as *const u8, size) == core::slice::from_raw_parts(compare as *const u8, size)
    };
    if !same {
        return status::SUCCESS;
    }
    let until = (ms != u64::MAX).then(|| crate::time::now_ms().saturating_add(ms));
    let mut note = crate::nt_syscalls::WaitNote::new("WaitOnAddress");
    while seq.load(Ordering::SeqCst) == seen {
        if until.is_some_and(|t| crate::time::now_ms() >= t) {
            return STATUS_TIMEOUT;
        }
        crate::loader::kill_if_requested();
        note.tick();
        crate::nt_syscalls::wait_a_little();
    }
    status::SUCCESS
}

/// VEIL_WAKE_ADDRESS(addr).
pub fn wake_address(addr: u64) -> NtStatus {
    slot(addr).fetch_add(1, Ordering::SeqCst);
    status::SUCCESS
}

// -----| named pipes |-----

/// Both directions of one connection.
struct Stream {
    client_to_server: Arc<Pipe>,
    server_to_client: Arc<Pipe>,
}

impl Stream {
    fn new(capacity: u64) -> Self {
        Self { client_to_server: new_pipe(capacity), server_to_client: new_pipe(capacity) }
    }
}

const LISTENING: u8 = 0;
const CONNECTED: u8 = 1;
const DISCONNECTED: u8 = 2;

pub struct Instance {
    name: String,
    capacity: u64,
    stream: Mutex<Stream>,
    state: AtomicU8,
}

/// A handle to one side of an instance.
pub struct DuplexEnd {
    instance: Arc<Instance>,
    is_server: bool,
    /// (read, write).
    ends: Mutex<(Arc<PipeEnd>, Arc<PipeEnd>)>,
}

impl DuplexEnd {
    fn server(instance: Arc<Instance>) -> Self {
        let ends = {
            let s = instance.stream.lock();
            (PipeEnd::reader(s.client_to_server.clone()), PipeEnd::writer(s.server_to_client.clone()))
        };
        Self { instance, is_server: true, ends: Mutex::new(ends) }
    }

    fn client(instance: Arc<Instance>) -> Self {
        let ends = {
            let s = instance.stream.lock();
            (PipeEnd::reader(s.server_to_client.clone()), PipeEnd::writer(s.client_to_server.clone()))
        };
        Self { instance, is_server: false, ends: Mutex::new(ends) }
    }

    /// Read or write this side: (status, bytes).
    pub fn io(&self, buffer: u64, length: usize, writing: bool) -> (NtStatus, usize) {
        if self.is_server && self.instance.state.load(Ordering::Acquire) != CONNECTED {
            return (STATUS_PIPE_LISTENING, 0);
        }
        let (read, write) = self.ends.lock().clone();
        crate::nt_process::pipe_io(if writing { &write } else { &read }, buffer, length, writing)
    }

    fn available(&self) -> usize {
        pipe_available(&self.ends.lock().0.pipe)
    }
}

static PIPES: Mutex<Vec<alloc::sync::Weak<Instance>>> = Mutex::new(Vec::new());

fn find_listening(name: &str) -> (bool, Option<Arc<Instance>>) {
    let mut pipes = PIPES.lock();
    pipes.retain(|w| w.strong_count() != 0);
    let mut exists = false;
    for w in pipes.iter() {
        let Some(i) = w.upgrade() else { continue };
        if !i.name.eq_ignore_ascii_case(name) {
            continue;
        }
        exists = true;
        if i.state.load(Ordering::Acquire) == LISTENING {
            return (true, Some(i));
        }
    }
    (exists, None)
}

fn put_end(proc: &Process, end: DuplexEnd, out: u64) {
    let h = proc.handles.lock().allocate_ref(KernelObjectRef::Duplex(Arc::new(end)), 0x0012_019F, 0);
    unsafe { *(out as *mut u64) = h as u64 };
}

/// NtQueryInformationFile on a pipe end (named or anonymous): pipe
/// information (23), local pipe information (24: quotas, data waiting, free
/// space; the MSYS2 runtime checks its signal pipe's space so), access (8)
/// and standard information (5). None: not a pipe.
pub fn pipe_query(proc: &Process, h: u64, iosb: u64, out: u64, length: usize, class: u32) -> Option<NtStatus> {
    // (bytes to read, read capacity, bytes queued to write, write capacity, server?, connected?, readable, writable)
    // ..., and whether the other end is gone (FILE_PIPE_CLOSING_STATE).
    let (avail, rcap, queued, wcap, server, connected, readable, writable, closing) = match crate::nt_process::object(proc, h)? {
        KernelObjectRef::Duplex(d) => {
            let (r, w) = { let e = d.ends.lock(); (e.0.clone(), e.1.clone()) };
            let (a, rc) = r.pipe.fill();
            let (q, wc) = w.pipe.fill();
            let closing = r.pipe.one_side_gone() || w.pipe.one_side_gone();
            (a, rc, q, wc, d.is_server, d.instance.state.load(Ordering::Acquire) == CONNECTED, true, true, closing)
        }
        KernelObjectRef::Pipe(end) => {
            let (n, cap) = end.pipe.fill();
            let closing = end.pipe.one_side_gone();
            if end.write { (0, cap, n, cap, false, true, false, true, closing) } else { (n, cap, 0, cap, true, true, true, false, closing) }
        }
        _ => return None,
    };
    let mut info: Vec<u8> = Vec::new();
    let mut put = |v: u32| info.extend_from_slice(&v.to_le_bytes());
    match class {
        23 => {
            put(0); // ReadMode: byte stream
            put(0); // CompletionMode: queue operation
        }
        24 => {
            put(0); // NamedPipeType: byte stream
            put(2); // NamedPipeConfiguration: full duplex
            put(1); // MaximumInstances
            put(1); // CurrentInstances
            put(rcap as u32); // InboundQuota
            put(avail as u32); // ReadDataAvailable
            put(wcap as u32); // OutboundQuota
            put(wcap.saturating_sub(queued) as u32); // WriteQuotaAvailable
            put(if closing { 4 } else if connected { 3 } else { 2 }); // NamedPipeState: closing / connected / listening
            put(server as u32); // NamedPipeEnd
        }
        8 => {
            let mut a = 0x0010_0000u32 | 0x0002_0000; // SYNCHRONIZE | READ_CONTROL
            if readable { a |= 0x0012_0089; }
            if writable { a |= 0x0012_0116; }
            put(a);
        }
        5 => {
            info.extend_from_slice(&(rcap as u64).to_le_bytes()); // AllocationSize
            info.extend_from_slice(&(avail as u64).to_le_bytes()); // EndOfFile
            info.extend_from_slice(&1u32.to_le_bytes()); // NumberOfLinks
            info.extend_from_slice(&[0, 0, 0, 0]); // DeletePending, Directory
        }
        _ => return Some(status::INVALID_INFO_CLASS),
    }
    if length < info.len() {
        return Some(status::BUFFER_TOO_SMALL);
    }
    if !ptr_ok(out, info.len()) {
        return Some(status::ACCESS_VIOLATION);
    }
    unsafe { core::ptr::copy_nonoverlapping(info.as_ptr(), out as *mut u8, info.len()) };
    if ptr_ok(iosb, 16) {
        unsafe { *(iosb as *mut nt_abi::IoStatusBlock) = nt_abi::IoStatusBlock { status: 0, information: info.len() as u64 } };
    }
    Some(status::SUCCESS)
}

fn duplex(proc: &Process, h: u64) -> Option<Arc<DuplexEnd>> {
    match crate::nt_process::object(proc, h) {
        Some(KernelObjectRef::Duplex(d)) => Some(d),
        _ => None,
    }
}

/// VEIL_PIPE(op, a, b, c).
pub fn pipe_call(proc: &Process, op: u64, a: u64, b: u64, c: u64) -> NtStatus {
    match op {
        // Create a server end: name* (0: nameless), out*, buffer size.
        0 => {
            if !ptr_ok(b, 8) {
                return status::ACCESS_VIOLATION;
            }
            let name = if a == 0 {
                String::new()
            } else {
                match read_name(a) {
                    Some(n) => n,
                    None => return status::ACCESS_VIOLATION,
                }
            };
            let capacity = c.max(4096);
            let instance = Arc::new(Instance { name, capacity, stream: Mutex::new(Stream::new(capacity)), state: AtomicU8::new(LISTENING) });
            if a != 0 {
                PIPES.lock().push(Arc::downgrade(&instance));
            }
            put_end(proc, DuplexEnd::server(instance), b);
            status::SUCCESS
        }
        // Connect as a client by name: name*, out*.
        1 => {
            if !ptr_ok(b, 8) {
                return status::ACCESS_VIOLATION;
            }
            let Some(name) = read_name(a) else { return status::ACCESS_VIOLATION };
            match find_listening(&name) {
                (false, _) => STATUS_OBJECT_NAME_NOT_FOUND,
                (true, None) => STATUS_PIPE_NOT_AVAILABLE,
                (true, Some(i)) => {
                    i.state.store(CONNECTED, Ordering::Release);
                    put_end(proc, DuplexEnd::client(i), b);
                    status::SUCCESS
                }
            }
        }
        // ConnectNamedPipe(server): waits for a client.
        2 => {
            let Some(d) = duplex(proc, a) else { return status::INVALID_HANDLE };
            let _ = d.instance.state.compare_exchange(DISCONNECTED, LISTENING, Ordering::AcqRel, Ordering::Acquire);
            if d.instance.state.load(Ordering::Acquire) == CONNECTED {
                return STATUS_PIPE_CONNECTED;
            }
            while d.instance.state.load(Ordering::Acquire) != CONNECTED {
                crate::loader::kill_if_requested();
                crate::nt_syscalls::wait_a_little();
            }
            status::SUCCESS
        }
        // ConnectNamedPipe without the wait: has a client come?
        9 => {
            let Some(d) = duplex(proc, a) else { return status::INVALID_HANDLE };
            let _ = d.instance.state.compare_exchange(DISCONNECTED, LISTENING, Ordering::AcqRel, Ordering::Acquire);
            if d.instance.state.load(Ordering::Acquire) == CONNECTED { STATUS_PIPE_CONNECTED } else { STATUS_PIPE_LISTENING }
        }
        // DisconnectNamedPipe(server): new streams; no client until ConnectNamedPipe.
        3 => {
            let Some(d) = duplex(proc, a) else { return status::INVALID_HANDLE };
            if !d.is_server {
                return status::INVALID_PARAMETER;
            }
            let ends = {
                let mut s = d.instance.stream.lock();
                *s = Stream::new(d.instance.capacity);
                (PipeEnd::reader(s.client_to_server.clone()), PipeEnd::writer(s.server_to_client.clone()))
            };
            *d.ends.lock() = ends;
            d.instance.state.store(DISCONNECTED, Ordering::Release);
            status::SUCCESS
        }
        // PeekNamedPipe(handle, available*).
        4 => {
            if !ptr_ok(b, 4) {
                return status::ACCESS_VIOLATION;
            }
            let n = match crate::nt_process::object(proc, a) {
                Some(KernelObjectRef::Duplex(d)) => d.available(),
                Some(KernelObjectRef::Pipe(e)) => pipe_available(&e.pipe),
                _ => return status::INVALID_HANDLE,
            };
            unsafe { *(b as *mut u32) = n as u32 };
            status::SUCCESS
        }
        // WaitNamedPipe(name*, ms): for an instance to be free.
        5 => {
            let Some(name) = read_name(a) else { return status::ACCESS_VIOLATION };
            let until = crate::time::now_ms().saturating_add(b);
            loop {
                match find_listening(&name) {
                    (false, _) => return STATUS_OBJECT_NAME_NOT_FOUND,
                    (true, Some(_)) => return status::SUCCESS,
                    (true, None) => {}
                }
                if crate::time::now_ms() >= until {
                    return STATUS_IO_TIMEOUT;
                }
                crate::loader::kill_if_requested();
                crate::nt_syscalls::wait_a_little();
            }
        }
        // A new handle to this process's console, redirected or not: out*,
        // then 0 for its input (CONIN$) or 1 for its active screen buffer (CONOUT$).
        8 => {
            if !ptr_ok(a, 8) {
                return status::ACCESS_VIOLATION;
            }
            let Some(console) = proc.current_console() else { return status::INVALID_HANDLE };
            let object = if b == 1 { KernelObjectRef::ConsoleBuffer(console.active()) } else { KernelObjectRef::Console(console) };
            let h = proc.handles.lock().allocate_ref(object, 0x0012_019F, 0);
            unsafe { *(a as *mut u64) = h as u64 };
            status::SUCCESS
        }
        // A handle to the NUL device: out*.
        7 => {
            if !ptr_ok(a, 8) {
                return status::ACCESS_VIOLATION;
            }
            let h = proc.handles.lock().allocate_ref(KernelObjectRef::Null, 0x0012_019F, 0);
            unsafe { *(a as *mut u64) = h as u64 };
            status::SUCCESS
        }
        // Connect as a client to the instance of a server handle: server, out*.
        6 => {
            if !ptr_ok(b, 8) {
                return status::ACCESS_VIOLATION;
            }
            let Some(d) = duplex(proc, a) else { return status::INVALID_HANDLE };
            if d.instance.state.swap(CONNECTED, Ordering::AcqRel) != LISTENING {
                return STATUS_PIPE_NOT_AVAILABLE;
            }
            put_end(proc, DuplexEnd::client(d.instance.clone()), b);
            status::SUCCESS
        }
        _ => status::INVALID_PARAMETER,
    }
}
