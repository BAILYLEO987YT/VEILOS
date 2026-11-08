//! Waits kept by kernel32 itself, each on a thread of its own as the thread
//! pool's are (pool.rs): RegisterWaitForSingleObject, and waitable timers
//! (an event the timer's thread signals when it's due).

use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use crate::util::{alloc, free};
use crate::{Bool, Handle};

const WAIT_TIMEOUT: u32 = 258;
const INVALID_HANDLE_VALUE: Handle = usize::MAX as Handle;
const WT_EXECUTEONLYONCE: u32 = 8;

fn event(manual: bool) -> Handle {
    crate::sync::CreateEventW(0, manual as Bool, 0, core::ptr::null())
}

fn thread(start: unsafe extern "system" fn(u64) -> u32, param: u64, tid: &mut u32) -> Handle {
    unsafe { crate::process::CreateThread(0, 0, start as *const () as u64, param, 0, tid) }
}


// -----| RegisterWaitForSingleObject |-----

#[repr(C)]
struct Wait {
    object: Handle,
    callback: u64,
    context: u64,
    ms: u32,
    flags: u32,
    cancel: Handle,
    thread: Handle,
    tid: u32,
}

unsafe extern "system" fn wait_thread(p: u64) -> u32 {
    let w = unsafe { &*(p as *const Wait) };
    let handles = [w.cancel, w.object];
    loop {
        let r = unsafe { crate::process::WaitForMultipleObjects(2, handles.as_ptr(), 0, w.ms) };
        if r != 1 && r != WAIT_TIMEOUT {
            return 0; // cancelled, or the object went away
        }
        // WAITORTIMERCALLBACK(context, timed_out)
        let f: unsafe extern "system" fn(u64, u8) = unsafe { core::mem::transmute(w.callback) };
        unsafe { f(w.context, (r == WAIT_TIMEOUT) as u8) };
        if w.flags & WT_EXECUTEONLYONCE != 0 {
            return 0;
        }
    }
}

#[no_mangle]
pub unsafe extern "system" fn RegisterWaitForSingleObject(out: *mut Handle, object: Handle, callback: u64, context: u64, ms: u32, flags: u32) -> Bool {
    let p = alloc(core::mem::size_of::<Wait>()) as *mut Wait;
    if p.is_null() || out.is_null() {
        crate::SetLastError(8); // ERROR_NOT_ENOUGH_MEMORY
        return 0;
    }
    unsafe {
        p.write(Wait { object, callback, context, ms, flags, cancel: event(true), thread: 0, tid: 0 });
        let mut tid = 0;
        (*p).thread = thread(wait_thread, p as u64, &mut tid);
        (*p).tid = tid;
        *out = p as Handle;
    }
    1
}

/// UnregisterWaitEx(wait, completion): INVALID_HANDLE_VALUE waits for a
/// running callback (unless called from it); an event is set once it's done.
#[no_mangle]
pub unsafe extern "system" fn UnregisterWaitEx(wait: Handle, completion: Handle) -> Bool {
    let p = wait as *mut Wait;
    if p.is_null() {
        crate::SetLastError(6); // ERROR_INVALID_HANDLE
        return 0;
    }
    let w = unsafe { &*p };
    crate::sync::SetEvent(w.cancel);
    let mine = w.tid == crate::GetCurrentThreadId();
    if completion == 0 || mine {
        // Not waited for: the thread ends by itself; its few bytes stay.
        return 1;
    }
    crate::sync::WaitForSingleObject(w.thread, u32::MAX);
    if completion != INVALID_HANDLE_VALUE {
        crate::sync::SetEvent(completion);
    }
    crate::CloseHandle(w.thread);
    crate::CloseHandle(w.cancel);
    free(p as *mut u8);
    1
}

#[no_mangle]
pub unsafe extern "system" fn UnregisterWait(wait: Handle) -> Bool {
    unsafe { UnregisterWaitEx(wait, 0) }
}

// -----| waitable timers |-----

#[repr(C)]
struct Timer {
    event: Handle,
    wake: Handle,
    /// When it's due (GetTickCount64 milliseconds), 0 if not set.
    due: AtomicU64,
    period: AtomicU32,
}

/// Timers by their event handle.
const MAX_TIMERS: usize = 256;
static TIMERS: [AtomicU64; MAX_TIMERS] = [const { AtomicU64::new(0) }; MAX_TIMERS];

fn timer(h: Handle) -> Option<&'static Timer> {
    TIMERS.iter().map(|t| t.load(Ordering::Acquire)).filter(|&p| p != 0).map(|p| unsafe { &*(p as *const Timer) }).find(|t| t.event == h)
}

unsafe extern "system" fn timer_thread(p: u64) -> u32 {
    let t = unsafe { &*(p as *const Timer) };
    loop {
        let due = t.due.load(Ordering::Acquire);
        let now = crate::time::GetTickCount64();
        if due != 0 && now >= due {
            let period = t.period.load(Ordering::Acquire) as u64;
            // Unless SetWaitableTimer moved it meanwhile.
            let next = if period > 0 { due + period } else { 0 };
            if t.due.compare_exchange(due, next, Ordering::AcqRel, Ordering::Acquire).is_ok() {
                crate::sync::SetEvent(t.event);
            }
            continue;
        }
        let ms = if due == 0 { u32::MAX } else { (due - now).min(u32::MAX as u64 - 1) as u32 };
        crate::sync::WaitForSingleObject(t.wake, ms);
    }
}

/// CREATE_WAITABLE_TIMER_MANUAL_RESET (1) in `flags`; high resolution (2)
/// asks nothing more of a millisecond timer.
#[no_mangle]
pub extern "system" fn CreateWaitableTimerExW(_sa: usize, _name: *const u16, flags: u32, _access: u32) -> Handle {
    let p = alloc(core::mem::size_of::<Timer>()) as *mut Timer;
    if p.is_null() {
        crate::SetLastError(8);
        return 0;
    }
    let Some(slot) = TIMERS.iter().find(|t| t.compare_exchange(0, p as u64, Ordering::AcqRel, Ordering::Acquire).is_ok()) else {
        free(p as *mut u8);
        crate::SetLastError(8);
        return 0;
    };
    let h = event(flags & 1 != 0);
    unsafe { p.write(Timer { event: h, wake: event(false), due: AtomicU64::new(0), period: AtomicU32::new(0) }) };
    let _ = slot;
    let mut tid = 0;
    let th = thread(timer_thread, p as u64, &mut tid);
    crate::CloseHandle(th);
    h
}

#[no_mangle]
pub extern "system" fn CreateWaitableTimerW(sa: usize, manual: Bool, name: *const u16) -> Handle {
    CreateWaitableTimerExW(sa, name, (manual != 0) as u32, 0)
}

/// due: a FILETIME, negative for relative (100 ns units). Completion
/// routines (APCs) aren't run.
#[no_mangle]
pub unsafe extern "system" fn SetWaitableTimer(h: Handle, due: *const i64, period_ms: i32, _apc: u64, _arg: u64, _resume: Bool) -> Bool {
    let Some(t) = timer(h) else {
        crate::SetLastError(6);
        return 0;
    };
    if due.is_null() {
        crate::SetLastError(87);
        return 0;
    }
    let now = crate::time::GetTickCount64();
    let d = unsafe { *due };
    let when = if d < 0 {
        now + ((-d) as u64).div_ceil(10_000)
    } else {
        let mut ft = 0u64;
        unsafe { crate::time::GetSystemTimeAsFileTime(&mut ft) };
        now + ((d - ft as i64).max(0) as u64).div_ceil(10_000)
    };
    crate::sync::ResetEvent(h);
    t.period.store(period_ms.max(0) as u32, Ordering::Release);
    t.due.store(when.max(1), Ordering::Release);
    crate::sync::SetEvent(t.wake);
    1
}

#[no_mangle]
pub unsafe extern "system" fn SetWaitableTimerEx(h: Handle, due: *const i64, period_ms: i32, apc: u64, arg: u64, _context: usize, _delay: u32) -> Bool {
    unsafe { SetWaitableTimer(h, due, period_ms, apc, arg, 0) }
}

#[no_mangle]
pub extern "system" fn CancelWaitableTimer(h: Handle) -> Bool {
    let Some(t) = timer(h) else {
        crate::SetLastError(6);
        return 0;
    };
    t.due.store(0, Ordering::Release);
    crate::sync::SetEvent(t.wake);
    1
}
