//! The thread pool, simply: each timer and each work item gets a thread of
//! its own when it first runs, which waits on an event (with a timeout for
//! timers) and calls the callback. Also the interlocked singly-linked
//! lists (SLIST_HEADER), under one lock.

use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use crate::util::{alloc, free};
use crate::{Bool, Handle};

/// A timer or work item (what PTP_TIMER / PTP_WORK point to).
#[repr(C)]
struct Item {
    callback: u64,
    context: u64,
    /// Signalled to wake the thread (new due time, or closing).
    wake: Handle,
    /// Signalled by the thread after each callback.
    idle: Handle,
    thread: Handle,
    /// Timer: when it's due (milliseconds, GetTickCount64), 0 not set,
    /// and its period. Work: submissions waiting.
    due: AtomicU64,
    period: AtomicU32,
    pending: AtomicU32,
    running: AtomicBool,
    closing: AtomicBool,
    timer: bool,
}

fn event(manual: bool) -> Handle {
    crate::sync::CreateEventW(0, manual as Bool, 0, core::ptr::null())
}

fn new_item(callback: u64, context: u64, timer: bool) -> *mut Item {
    let p = alloc(core::mem::size_of::<Item>()) as *mut Item;
    if p.is_null() {
        return p;
    }
    unsafe {
        p.write(Item {
            callback,
            context,
            wake: event(false),
            idle: event(true),
            thread: 0,
            due: AtomicU64::new(0),
            period: AtomicU32::new(0),
            pending: AtomicU32::new(0),
            running: AtomicBool::new(false),
            closing: AtomicBool::new(false),
            timer,
        });
    }
    p
}

unsafe extern "system" fn item_thread(p: u64) -> u32 {
    let it = unsafe { &*(p as *const Item) };
    loop {
        if it.closing.load(Ordering::Acquire) {
            return 0;
        }
        let call = if it.timer {
            let due = it.due.load(Ordering::Acquire);
            let now = crate::time::GetTickCount64();
            if due != 0 && now >= due {
                let period = it.period.load(Ordering::Acquire) as u64;
                it.due.store(if period > 0 { now + period } else { 0 }, Ordering::Release);
                true
            } else {
                let wait = if due == 0 { u32::MAX } else { (due - now).min(u32::MAX as u64 - 1) as u32 };
                crate::sync::WaitForSingleObject(it.wake, wait);
                false
            }
        } else if it.pending.load(Ordering::Acquire) > 0 {
            it.pending.fetch_sub(1, Ordering::AcqRel);
            true
        } else {
            crate::sync::WaitForSingleObject(it.wake, u32::MAX);
            false
        };
        if call {
            it.running.store(true, Ordering::Release);
            crate::sync::ResetEvent(it.idle);
            // Callback(instance, context, timer/work).
            let f: unsafe extern "system" fn(u64, u64, u64) = unsafe { core::mem::transmute(it.callback) };
            unsafe { f(0, it.context, p) };
            it.running.store(false, Ordering::Release);
            crate::sync::SetEvent(it.idle);
        }
    }
}

fn start(p: *mut Item) {
    let it = unsafe { &mut *p };
    if it.thread == 0 {
        let mut tid = 0u32;
        it.thread = unsafe { crate::process::CreateThread(0, 0, item_thread as *const () as u64, p as u64, 0, &mut tid) };
    }
    crate::sync::SetEvent(it.wake);
}

/// Wait for a running callback and close the item.
fn close(p: *mut Item) {
    if p.is_null() {
        return;
    }
    let it = unsafe { &*p };
    it.closing.store(true, Ordering::Release);
    crate::sync::SetEvent(it.wake);
    if it.thread != 0 {
        crate::sync::WaitForSingleObject(it.thread, u32::MAX);
        crate::CloseHandle(it.thread);
    }
    crate::CloseHandle(it.wake);
    crate::CloseHandle(it.idle);
    free(p as *mut u8);
}

#[no_mangle]
pub extern "system" fn CreateThreadpoolTimer(callback: u64, context: u64, _env: usize) -> *mut u8 {
    new_item(callback, context, true) as *mut u8
}

/// due: a FILETIME, negative for relative (100 ns units), null to stop.
#[no_mangle]
pub unsafe extern "system" fn SetThreadpoolTimer(t: *mut u8, due: *const i64, period_ms: u32, _window: u32) {
    let p = t as *mut Item;
    if p.is_null() {
        return;
    }
    let it = unsafe { &*p };
    let now = crate::time::GetTickCount64();
    let when = if due.is_null() {
        0
    } else {
        let d = unsafe { *due };
        if d < 0 {
            now + ((-d) as u64 / 10_000)
        } else if d == 0 {
            now
        } else {
            // Absolute: as a time from now.
            let mut ft = 0u64;
            unsafe { crate::time::GetSystemTimeAsFileTime(&mut ft) };
            now + ((d - ft as i64).max(0) as u64 / 10_000)
        }
    };
    it.period.store(period_ms, Ordering::Release);
    it.due.store(when.max(if due.is_null() { 0 } else { 1 }), Ordering::Release);
    start(p);
}

#[no_mangle]
pub extern "system" fn IsThreadpoolTimerSet(t: *mut u8) -> Bool {
    (!t.is_null() && unsafe { (*(t as *const Item)).due.load(Ordering::Acquire) } != 0) as Bool
}

#[no_mangle]
pub extern "system" fn WaitForThreadpoolTimerCallbacks(t: *mut u8, cancel: Bool) {
    let p = t as *mut Item;
    if p.is_null() {
        return;
    }
    let it = unsafe { &*p };
    if cancel != 0 {
        it.due.store(0, Ordering::Release);
    }
    if it.running.load(Ordering::Acquire) {
        crate::sync::WaitForSingleObject(it.idle, u32::MAX);
    }
}

#[no_mangle]
pub extern "system" fn CloseThreadpoolTimer(t: *mut u8) {
    close(t as *mut Item);
}

#[no_mangle]
pub extern "system" fn CreateThreadpoolWork(callback: u64, context: u64, _env: usize) -> *mut u8 {
    new_item(callback, context, false) as *mut u8
}

#[no_mangle]
pub extern "system" fn SubmitThreadpoolWork(w: *mut u8) {
    let p = w as *mut Item;
    if p.is_null() {
        return;
    }
    unsafe { (*p).pending.fetch_add(1, Ordering::AcqRel) };
    start(p);
}

#[no_mangle]
pub extern "system" fn WaitForThreadpoolWorkCallbacks(w: *mut u8, cancel: Bool) {
    let p = w as *mut Item;
    if p.is_null() {
        return;
    }
    let it = unsafe { &*p };
    if cancel != 0 {
        it.pending.store(0, Ordering::Release);
    }
    while it.pending.load(Ordering::Acquire) > 0 || it.running.load(Ordering::Acquire) {
        crate::sync::WaitForSingleObject(it.idle, 10);
    }
}

#[no_mangle]
pub extern "system" fn CloseThreadpoolWork(w: *mut u8) {
    close(w as *mut Item);
}

/// A simple callback (instance, context) on a thread of its own.
#[no_mangle]
pub extern "system" fn TrySubmitThreadpoolCallback(callback: u64, context: u64, _env: usize) -> Bool {
    // On a plain thread, with the callback and context in a block.
    let block = alloc(16) as *mut u64;
    if block.is_null() {
        return 0;
    }
    unsafe {
        *block = callback;
        *block.add(1) = context;
    }
    unsafe extern "system" fn run(b: u64) -> u32 {
        let (cb, ctx) = unsafe { (*(b as *const u64), *(b as *const u64).add(1)) };
        free(b as *mut u8);
        let f: unsafe extern "system" fn(u64, u64) = unsafe { core::mem::transmute(cb) };
        unsafe { f(0, ctx) };
        0
    }
    let mut tid = 0;
    let h = unsafe { crate::process::CreateThread(0, 0, run as *const () as u64, block as u64, 0, &mut tid) };
    if h == 0 {
        return 0;
    }
    crate::CloseHandle(h);
    1
}

#[no_mangle]
pub extern "system" fn QueueUserWorkItem(f: u64, context: u64, _flags: u32) -> Bool {
    let mut tid = 0;
    let h = unsafe { crate::process::CreateThread(0, 0, f, context, 0, &mut tid) };
    if h == 0 {
        return 0;
    }
    crate::CloseHandle(h);
    1
}

// -----| interlocked singly-linked lists |-----
//
// SLIST_HEADER (16 bytes): the first entry, and a depth count.

static SLIST: AtomicBool = AtomicBool::new(false);

fn locked<R>(f: impl FnOnce() -> R) -> R {
    while SLIST.compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() {
        core::hint::spin_loop();
    }
    let r = f();
    SLIST.store(false, Ordering::Release);
    r
}

#[no_mangle]
pub unsafe extern "system" fn InitializeSListHead(h: *mut u64) {
    unsafe {
        *h = 0;
        *h.add(1) = 0;
    }
}

#[no_mangle]
pub unsafe extern "system" fn InterlockedPushEntrySList(h: *mut u64, e: *mut u64) -> *mut u64 {
    locked(|| unsafe {
        let first = *h;
        *e = first;
        *h = e as u64;
        *h.add(1) += 1;
        first as *mut u64
    })
}

#[no_mangle]
pub unsafe extern "system" fn InterlockedPopEntrySList(h: *mut u64) -> *mut u64 {
    locked(|| unsafe {
        let first = *h as *mut u64;
        if !first.is_null() {
            *h = *first;
            *h.add(1) -= 1;
        }
        first
    })
}

#[no_mangle]
pub unsafe extern "system" fn InterlockedFlushSList(h: *mut u64) -> *mut u64 {
    locked(|| unsafe {
        let first = *h as *mut u64;
        *h = 0;
        *h.add(1) = 0;
        first
    })
}

#[no_mangle]
pub unsafe extern "system" fn QueryDepthSList(h: *const u64) -> u16 {
    unsafe { *h.add(1) as u16 }
}
