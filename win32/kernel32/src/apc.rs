//! User APCs (QueueUserAPC): a queue per thread, kept here. A thread runs
//! its queued calls when it waits alertably (SleepEx, WaitFor...Ex with
//! alertable set), and the main thread runs them once before the program's
//! entry point, as Windows' loader does: the MSYS2/Cygwin runtime queues the
//! start of its signal thread from DllMain that way.
//!
//! An APC queued to a thread already blocked in an alertable wait runs
//! within a few milliseconds: such waits wait in short slices.

use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use crate::Handle;

const SLOTS: usize = 256;
const CURRENT_THREAD: Handle = (-2i64) as u64;
pub const WAIT_IO_COMPLETION: u32 = 0xC0;

struct Apc {
    /// The thread it's for (0: free slot), and when it was queued (FIFO).
    tid: AtomicU32,
    seq: AtomicU64,
    func: AtomicU64,
    data: AtomicU64,
}

static QUEUE: [Apc; SLOTS] = [const { Apc { tid: AtomicU32::new(0), seq: AtomicU64::new(0), func: AtomicU64::new(0), data: AtomicU64::new(0) } }; SLOTS];
static LOCK: AtomicBool = AtomicBool::new(false);
static SEQ: AtomicU64 = AtomicU64::new(1);
/// How many are queued, so waits that find none don't take the lock.
static PENDING: AtomicU32 = AtomicU32::new(0);

fn locked<R>(f: impl FnOnce() -> R) -> R {
    while LOCK.compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() {
        core::hint::spin_loop();
    }
    let r = f();
    LOCK.store(false, Ordering::Release);
    r
}

#[no_mangle]
pub extern "system" fn QueueUserAPC(func: u64, thread: Handle, data: usize) -> u32 {
    let tid = if thread == CURRENT_THREAD { crate::GetCurrentThreadId() } else { crate::process::GetThreadId(thread) };
    if tid == 0 || func == 0 {
        crate::SetLastError(6); // ERROR_INVALID_HANDLE
        return 0;
    }
    let queued = locked(|| {
        let Some(slot) = QUEUE.iter().find(|a| a.tid.load(Ordering::Relaxed) == 0) else { return false };
        slot.func.store(func, Ordering::Relaxed);
        slot.data.store(data as u64, Ordering::Relaxed);
        slot.seq.store(SEQ.fetch_add(1, Ordering::Relaxed), Ordering::Relaxed);
        slot.tid.store(tid, Ordering::Release);
        true
    });
    if !queued {
        crate::SetLastError(8); // ERROR_NOT_ENOUGH_MEMORY
        return 0;
    }
    PENDING.fetch_add(1, Ordering::AcqRel);
    1
}

/// Run this thread's queued APCs, oldest first. True if any ran.
pub fn run() -> bool {
    if PENDING.load(Ordering::Acquire) == 0 {
        return false;
    }
    let me = crate::GetCurrentThreadId();
    let mut ran = false;
    loop {
        // Take the oldest one for this thread, then call it unlocked (it may queue more).
        let next = locked(|| {
            let slot = QUEUE.iter().filter(|a| a.tid.load(Ordering::Relaxed) == me).min_by_key(|a| a.seq.load(Ordering::Relaxed))?;
            let call = (slot.func.load(Ordering::Relaxed), slot.data.load(Ordering::Relaxed));
            slot.tid.store(0, Ordering::Release);
            Some(call)
        });
        let Some((func, data)) = next else { return ran };
        PENDING.fetch_sub(1, Ordering::AcqRel);
        let f: unsafe extern "system" fn(u64) = unsafe { core::mem::transmute(func) };
        unsafe { f(data) };
        ran = true;
    }
}

/// Is an APC waiting for this thread?
fn waiting() -> bool {
    if PENDING.load(Ordering::Acquire) == 0 {
        return false;
    }
    let me = crate::GetCurrentThreadId();
    QUEUE.iter().any(|a| a.tid.load(Ordering::Acquire) == me)
}

/// The startup routine calls this before the program's entry point.
#[no_mangle]
pub extern "system" fn VeilRunApcs() {
    run();
}

/// An alertable wait: `wait(ms)` waits once (returning WAIT_TIMEOUT when the
/// time's up); this runs queued APCs first, and between slices of the wait.
pub fn alertable(ms: u32, mut wait: impl FnMut(u32) -> u32) -> u32 {
    const SLICE: u32 = 10;
    const WAIT_TIMEOUT: u32 = 258;
    let start = crate::time::GetTickCount64();
    loop {
        if run() {
            return WAIT_IO_COMPLETION;
        }
        let left = if ms == u32::MAX {
            u32::MAX
        } else {
            let gone = crate::time::GetTickCount64() - start;
            if gone >= ms as u64 { 0 } else { (ms as u64 - gone) as u32 }
        };
        let r = wait(left.min(SLICE));
        if r != WAIT_TIMEOUT || left <= SLICE {
            if r == WAIT_TIMEOUT && waiting() && run() {
                return WAIT_IO_COMPLETION;
            }
            return r;
        }
    }
}
