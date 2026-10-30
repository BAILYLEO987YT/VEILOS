//! Critical sections, SRW locks, condition variables, one-time init, events,
//! waits and TLS slots.
//!
//! Locks are user-mode spin-then-yield (NtYieldExecution): a thread that
//! can't take a lock lets others run instead of burning its slice. One
//! thread per process for now (threads are W6), so they are rarely contended.

use core::sync::atomic::{AtomicU64, Ordering};
use nt_abi::{layout, status};

use crate::{check, fail, Bool, Handle};

const WAIT_OBJECT_0: u32 = 0;
const WAIT_TIMEOUT: u32 = 258;
const WAIT_FAILED: u32 = u32::MAX;
const EVENT_ALL_ACCESS: u32 = 0x001F_0003;

fn yield_cpu() {
    unsafe { crate::NtYieldExecution() };
}

// -----| critical sections |-----

/// RTL_CRITICAL_SECTION (40 bytes): DebugInfo, LockCount, RecursionCount,
/// OwningThread, LockSemaphore, SpinCount. OwningThread is the lock word.
#[repr(C)]
pub struct CriticalSection {
    debug_info: usize,
    lock_count: i32,
    recursion_count: i32,
    owning_thread: AtomicU64,
    lock_semaphore: usize,
    spin_count: usize,
}

fn me() -> u64 {
    crate::GetCurrentThreadId() as u64
}

#[no_mangle]
pub unsafe extern "system" fn InitializeCriticalSection(cs: *mut CriticalSection) {
    unsafe {
        cs.write(CriticalSection {
            debug_info: 0,
            lock_count: -1,
            recursion_count: 0,
            owning_thread: AtomicU64::new(0),
            lock_semaphore: 0,
            spin_count: 0,
        })
    };
}

#[no_mangle]
pub unsafe extern "system" fn InitializeCriticalSectionAndSpinCount(cs: *mut CriticalSection, spin: u32) -> Bool {
    unsafe {
        InitializeCriticalSection(cs);
        (*cs).spin_count = spin as usize;
    }
    1
}

#[no_mangle]
pub unsafe extern "system" fn InitializeCriticalSectionEx(cs: *mut CriticalSection, spin: u32, _flags: u32) -> Bool {
    unsafe { InitializeCriticalSectionAndSpinCount(cs, spin) }
}

#[no_mangle]
pub unsafe extern "system" fn TryEnterCriticalSection(cs: *mut CriticalSection) -> Bool {
    let cs = unsafe { &mut *cs };
    let me = me();
    if cs.owning_thread.load(Ordering::Acquire) == me {
        cs.recursion_count += 1;
        return 1;
    }
    if cs.owning_thread.compare_exchange(0, me, Ordering::Acquire, Ordering::Relaxed).is_ok() {
        cs.recursion_count = 1;
        cs.lock_count = 0;
        return 1;
    }
    0
}

#[no_mangle]
pub unsafe extern "system" fn EnterCriticalSection(cs: *mut CriticalSection) {
    let mut spins = 0u32;
    while unsafe { TryEnterCriticalSection(cs) } == 0 {
        spins += 1;
        if spins > 64 {
            yield_cpu();
        } else {
            core::hint::spin_loop();
        }
    }
}

#[no_mangle]
pub unsafe extern "system" fn LeaveCriticalSection(cs: *mut CriticalSection) {
    let cs = unsafe { &mut *cs };
    cs.recursion_count -= 1;
    if cs.recursion_count <= 0 {
        cs.recursion_count = 0;
        cs.lock_count = -1;
        cs.owning_thread.store(0, Ordering::Release);
    }
}

#[no_mangle]
pub extern "system" fn DeleteCriticalSection(_cs: *mut CriticalSection) {}

// -----| SRW locks |-----

/// The lock word: WRITER while held exclusively, else the reader count.
const WRITER: u64 = 1 << 63;

unsafe fn srw<'a>(lock: *mut u64) -> &'a AtomicU64 {
    unsafe { AtomicU64::from_ptr(lock) }
}

#[no_mangle]
pub unsafe extern "system" fn InitializeSRWLock(lock: *mut u64) {
    unsafe { *lock = 0 };
}

#[no_mangle]
pub unsafe extern "system" fn TryAcquireSRWLockExclusive(lock: *mut u64) -> u8 {
    unsafe { srw(lock) }.compare_exchange(0, WRITER, Ordering::Acquire, Ordering::Relaxed).is_ok() as u8
}

#[no_mangle]
pub unsafe extern "system" fn AcquireSRWLockExclusive(lock: *mut u64) {
    while unsafe { TryAcquireSRWLockExclusive(lock) } == 0 {
        yield_cpu();
    }
}

#[no_mangle]
pub unsafe extern "system" fn ReleaseSRWLockExclusive(lock: *mut u64) {
    unsafe { srw(lock) }.store(0, Ordering::Release);
}

#[no_mangle]
pub unsafe extern "system" fn TryAcquireSRWLockShared(lock: *mut u64) -> u8 {
    let l = unsafe { srw(lock) };
    let v = l.load(Ordering::Relaxed);
    (v & WRITER == 0 && l.compare_exchange(v, v + 1, Ordering::Acquire, Ordering::Relaxed).is_ok()) as u8
}

#[no_mangle]
pub unsafe extern "system" fn AcquireSRWLockShared(lock: *mut u64) {
    while unsafe { TryAcquireSRWLockShared(lock) } == 0 {
        yield_cpu();
    }
}

#[no_mangle]
pub unsafe extern "system" fn ReleaseSRWLockShared(lock: *mut u64) {
    unsafe { srw(lock) }.fetch_sub(1, Ordering::Release);
}

// -----| condition variables |-----
//
// The variable is a generation counter: waking bumps it, a sleeper waits
// (yielding) until it changes or its timeout passes.

unsafe fn generation<'a>(cv: *mut u64) -> &'a AtomicU64 {
    unsafe { AtomicU64::from_ptr(cv) }
}

#[no_mangle]
pub unsafe extern "system" fn InitializeConditionVariable(cv: *mut u64) {
    unsafe { *cv = 0 };
}

fn wait_for_change(cv: *mut u64, start: u64, ms: u32) -> bool {
    let until = crate::time::GetTickCount64().saturating_add(ms as u64);
    while unsafe { generation(cv) }.load(Ordering::Acquire) == start {
        if ms != u32::MAX && crate::time::GetTickCount64() >= until {
            return false;
        }
        crate::time::Sleep(1);
    }
    true
}

#[no_mangle]
pub unsafe extern "system" fn SleepConditionVariableCS(cv: *mut u64, cs: *mut CriticalSection, ms: u32) -> Bool {
    let start = unsafe { generation(cv) }.load(Ordering::Acquire);
    unsafe { LeaveCriticalSection(cs) };
    let woken = wait_for_change(cv, start, ms);
    unsafe { EnterCriticalSection(cs) };
    if woken { 1 } else { fail(status::TIMEOUT) }
}

#[no_mangle]
pub unsafe extern "system" fn SleepConditionVariableSRW(cv: *mut u64, lock: *mut u64, ms: u32, flags: u32) -> Bool {
    let shared = flags & 1 != 0; // CONDITION_VARIABLE_LOCKMODE_SHARED
    let start = unsafe { generation(cv) }.load(Ordering::Acquire);
    unsafe { if shared { ReleaseSRWLockShared(lock) } else { ReleaseSRWLockExclusive(lock) } };
    let woken = wait_for_change(cv, start, ms);
    unsafe { if shared { AcquireSRWLockShared(lock) } else { AcquireSRWLockExclusive(lock) } };
    if woken { 1 } else { fail(status::TIMEOUT) }
}

#[no_mangle]
pub unsafe extern "system" fn WakeConditionVariable(cv: *mut u64) {
    unsafe { generation(cv) }.fetch_add(1, Ordering::Release);
}

#[no_mangle]
pub unsafe extern "system" fn WakeAllConditionVariable(cv: *mut u64) {
    unsafe { generation(cv) }.fetch_add(1, Ordering::Release);
}

// -----| one-time initialisation |-----

type InitOnceFn = unsafe extern "system" fn(*mut u64, usize, *mut usize) -> Bool;

/// INIT_ONCE: 0 = not run, 1 = running, 2 = done.
#[no_mangle]
pub unsafe extern "system" fn InitOnceExecuteOnce(once: *mut u64, f: InitOnceFn, param: usize, context: *mut usize) -> Bool {
    let state = unsafe { AtomicU64::from_ptr(once) };
    loop {
        match state.compare_exchange(0, 1, Ordering::Acquire, Ordering::Acquire) {
            Ok(_) => {
                let ok = unsafe { f(once, param, context) };
                state.store(if ok != 0 { 2 } else { 0 }, Ordering::Release);
                return ok;
            }
            Err(2) => return 1,
            Err(_) => yield_cpu(),
        }
    }
}

#[no_mangle]
pub unsafe extern "system" fn InitOnceInitialize(once: *mut u64) {
    unsafe { *once = 0 };
}

// -----| events and waits |-----

#[no_mangle]
pub extern "system" fn CreateEventW(_security: usize, manual_reset: Bool, initial: Bool, _name: *const u16) -> Handle {
    let mut h: Handle = 0;
    let s = unsafe { crate::NtCreateEvent(&mut h, EVENT_ALL_ACCESS, core::ptr::null(), if manual_reset != 0 { 0 } else { 1 }, (initial != 0) as u8) };
    if status::is_success(s) {
        h
    } else {
        fail(s);
        0
    }
}

#[no_mangle]
pub extern "system" fn CreateEventA(security: usize, manual_reset: Bool, initial: Bool, _name: *const u8) -> Handle {
    CreateEventW(security, manual_reset, initial, core::ptr::null())
}

/// Flags: CREATE_EVENT_MANUAL_RESET (1), CREATE_EVENT_INITIAL_SET (2).
#[no_mangle]
pub extern "system" fn CreateEventExW(security: usize, _name: *const u16, flags: u32, _access: u32) -> Handle {
    CreateEventW(security, (flags & 1 != 0) as Bool, (flags & 2 != 0) as Bool, core::ptr::null())
}

#[no_mangle]
pub extern "system" fn CreateEventExA(security: usize, _name: *const u8, flags: u32, access: u32) -> Handle {
    CreateEventExW(security, core::ptr::null(), flags, access)
}

#[no_mangle]
pub extern "system" fn SetEvent(h: Handle) -> Bool {
    check(unsafe { crate::NtSetEvent(h, core::ptr::null_mut()) })
}

#[no_mangle]
pub extern "system" fn ResetEvent(h: Handle) -> Bool {
    check(unsafe { crate::NtResetEvent(h, core::ptr::null_mut()) })
}

#[no_mangle]
pub extern "system" fn WaitForSingleObjectEx(h: Handle, ms: u32, _alertable: Bool) -> u32 {
    let timeout: i64 = -(ms as i64) * 10_000;
    let t = if ms == u32::MAX { core::ptr::null() } else { &timeout as *const i64 };
    match unsafe { crate::NtWaitForSingleObject(h, 0, t) } {
        status::SUCCESS => WAIT_OBJECT_0,
        status::TIMEOUT => WAIT_TIMEOUT,
        s => {
            fail(s);
            WAIT_FAILED
        }
    }
}

#[no_mangle]
pub extern "system" fn WaitForSingleObject(h: Handle, ms: u32) -> u32 {
    WaitForSingleObjectEx(h, ms, 0)
}

#[no_mangle]
pub extern "system" fn SwitchToThread() -> Bool {
    unsafe { crate::NtYieldExecution() };
    1
}

// -----| TLS slots |-----
//
// TlsGetValue and friends use the TEB's 64 TlsSlots; which are taken is
// kept here (per process: each process has its own copy of this DLL).

static TLS_USED: AtomicU64 = AtomicU64::new(0);
const TLS_OUT_OF_INDEXES: u32 = u32::MAX;

fn slot(index: u32) -> *mut usize {
    unsafe { crate::teb().add(layout::TEB_TLS_SLOTS + index as usize * 8) as *mut usize }
}

#[no_mangle]
pub extern "system" fn TlsAlloc() -> u32 {
    loop {
        let used = TLS_USED.load(Ordering::Acquire);
        if used == u64::MAX {
            crate::SetLastError(8);
            return TLS_OUT_OF_INDEXES;
        }
        let i = (!used).trailing_zeros();
        if TLS_USED.compare_exchange(used, used | (1 << i), Ordering::AcqRel, Ordering::Acquire).is_ok() {
            unsafe { *slot(i) = 0 };
            return i;
        }
    }
}

#[no_mangle]
pub extern "system" fn TlsFree(index: u32) -> Bool {
    if index as usize >= layout::TLS_MINIMUM_AVAILABLE {
        return fail(status::INVALID_PARAMETER);
    }
    TLS_USED.fetch_and(!(1 << index), Ordering::AcqRel);
    1
}

#[no_mangle]
pub extern "system" fn TlsGetValue(index: u32) -> usize {
    if index as usize >= layout::TLS_MINIMUM_AVAILABLE {
        fail(status::INVALID_PARAMETER);
        return 0;
    }
    crate::SetLastError(0);
    unsafe { *slot(index) }
}

#[no_mangle]
pub extern "system" fn TlsSetValue(index: u32, value: usize) -> Bool {
    if index as usize >= layout::TLS_MINIMUM_AVAILABLE {
        return fail(status::INVALID_PARAMETER);
    }
    unsafe { *slot(index) = value };
    1
}

// Fiber-local storage is thread-local storage while there are no fibers
// (the cleanup callback isn't run).

#[no_mangle]
pub extern "system" fn FlsAlloc(_callback: usize) -> u32 {
    TlsAlloc()
}

#[no_mangle]
pub extern "system" fn FlsFree(index: u32) -> Bool {
    TlsFree(index)
}

#[no_mangle]
pub extern "system" fn FlsGetValue(index: u32) -> usize {
    TlsGetValue(index)
}

#[no_mangle]
pub extern "system" fn FlsSetValue(index: u32, value: usize) -> Bool {
    TlsSetValue(index, value)
}
