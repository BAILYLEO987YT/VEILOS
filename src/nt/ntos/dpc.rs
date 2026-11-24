// DPCs (deferred procedure calls) and timers.
//
// A DPC queued at PASSIVE or APC level runs at once, at DISPATCH_LEVEL, as
// Windows' would. One queued at DISPATCH or above (from a spinlock, another
// DPC, an interrupt service routine) sends this CPU an interrupt on
// `interrupts::DPC_VECTOR`, whose priority class is DISPATCH_LEVEL's: the
// local APIC holds it until CR8 drops below 2, so the queue runs the moment
// IRQL falls, as Windows' DISPATCH software interrupt does. (KeLowerIrql
// and the WDK's inline CR8 writes need nothing more.) The BSP's 1 ms tick
// (`tick`) also runs a waiting queue, as a fallback. Timers are checked on
// that tick: one that's due is signalled, queues its DPC, and is set again
// if it's periodic.
//
// KDPC (0x40): Type at 0, DeferredRoutine 0x18, DeferredContext 0x20,
// SystemArgument1/2 0x28/0x30, DpcData 0x38 (non-null while queued).
// KTIMER (0x40): dispatcher header (type 8 notification, 9 synchronization),
// DueTime 0x18, Dpc 0x30, Period 0x3C (ms). Checked in k1's layout.c.

use alloc::vec::Vec;
use spin::Mutex;

use super::ke;

const DPC_OBJECT: u8 = 19;
const TIMER_NOTIFICATION: u8 = 8;
const TIMER_SYNCHRONIZATION: u8 = 9;

const DPC_ROUTINE: usize = 0x18;
const DPC_CONTEXT: usize = 0x20;
const DPC_ARG1: usize = 0x28;
const DPC_ARG2: usize = 0x30;
const DPC_DATA: usize = 0x38;
const TIMER_DUE: usize = 0x18;
const TIMER_DPC: usize = 0x30;
const TIMER_PERIOD: usize = 0x3C;

/// Queued DPCs, oldest first.
static QUEUE: Mutex<Vec<usize>> = Mutex::new(Vec::new());
/// Set timers: (timer, due time in VEILOS milliseconds).
static TIMERS: Mutex<Vec<(usize, u64)>> = Mutex::new(Vec::new());

fn locked<R>(f: impl FnOnce() -> R) -> R {
    x86_64::instructions::interrupts::without_interrupts(f)
}

unsafe fn field(p: *mut u8, off: usize) -> *mut u64 {
    unsafe { p.add(off) as *mut u64 }
}

pub extern "win64" fn KeInitializeDpc(dpc: *mut u8, routine: u64, context: u64) {
    unsafe {
        core::ptr::write_bytes(dpc, 0, 0x40);
        dpc.write(DPC_OBJECT);
        dpc.add(1).write(1); // MediumImportance
        field(dpc, DPC_ROUTINE).write(routine);
        field(dpc, DPC_CONTEXT).write(context);
    }
}

pub extern "win64" fn KeInitializeThreadedDpc(dpc: *mut u8, routine: u64, context: u64) {
    KeInitializeDpc(dpc, routine, context);
}

pub extern "win64" fn KeSetImportanceDpc(dpc: *mut u8, importance: u32) {
    unsafe { dpc.add(1).write(importance as u8) };
}

/// FALSE if it was already queued.
pub extern "win64" fn KeInsertQueueDpc(dpc: *mut u8, arg1: u64, arg2: u64) -> u8 {
    let queued = locked(|| {
        let mut q = QUEUE.lock();
        unsafe {
            if field(dpc, DPC_DATA).read() != 0 {
                return false;
            }
            field(dpc, DPC_ARG1).write(arg1);
            field(dpc, DPC_ARG2).write(arg2);
            field(dpc, DPC_DATA).write(1);
        }
        q.push(dpc as usize);
        true
    });
    if queued {
        if ke::irql() < ke::DISPATCH_LEVEL {
            drain();
        } else {
            crate::arch::smp::self_ipi(crate::arch::interrupts::DPC_VECTOR);
        }
    }
    queued as u8
}

/// `interrupts::DPC_VECTOR`: IRQL has dropped below DISPATCH_LEVEL with
/// DPCs queued. Runs them with interrupts on (device interrupts can come
/// meanwhile, as at DISPATCH_LEVEL on Windows).
pub fn software_interrupt() {
    let on = x86_64::instructions::interrupts::are_enabled();
    x86_64::instructions::interrupts::enable();
    drain();
    if !on {
        x86_64::instructions::interrupts::disable();
    }
}

pub extern "win64" fn KeRemoveQueueDpc(dpc: *mut u8) -> u8 {
    locked(|| {
        let mut q = QUEUE.lock();
        let before = q.len();
        q.retain(|&d| d != dpc as usize);
        unsafe { field(dpc, DPC_DATA).write(0) };
        (q.len() != before) as u8
    })
}

/// Run every queued DPC now (and any they queue).
pub extern "win64" fn KeFlushQueuedDpcs() {
    drain();
}

/// Run queued DPCs at DISPATCH_LEVEL until the queue is empty.
fn drain() {
    super::with_kernel_gs(drain_inner)
}

fn drain_inner() {
    let old = ke::KfRaiseIrql(ke::DISPATCH_LEVEL);
    loop {
        let next = locked(|| {
            let mut q = QUEUE.lock();
            if q.is_empty() { None } else { Some(q.remove(0)) }
        });
        let Some(d) = next else { break };
        let dpc = d as *mut u8;
        unsafe {
            field(dpc, DPC_DATA).write(0);
            let routine: extern "win64" fn(*mut u8, u64, u64, u64) = core::mem::transmute(field(dpc, DPC_ROUTINE).read());
            routine(dpc, field(dpc, DPC_CONTEXT).read(), field(dpc, DPC_ARG1).read(), field(dpc, DPC_ARG2).read());
        }
    }
    ke::set_irql(old);
}

// -----| timers |-----

pub extern "win64" fn KeInitializeTimerEx(t: *mut u8, kind: u32) {
    unsafe {
        core::ptr::write_bytes(t, 0, 0x40);
        t.write(if kind == 1 { TIMER_SYNCHRONIZATION } else { TIMER_NOTIFICATION });
        t.add(2).write(0x10);
        let list = t.add(8) as *mut u64;
        list.write(list as u64);
        list.add(1).write(list as u64);
    }
}

pub extern "win64" fn KeInitializeTimer(t: *mut u8) {
    KeInitializeTimerEx(t, 0);
}

/// KeSetTimerEx(Timer, DueTime (100 ns, negative = relative), Period (ms),
/// Dpc): TRUE if it was already set.
pub extern "win64" fn KeSetTimerEx(t: *mut u8, due: i64, period: i32, dpc: *mut u8) -> u8 {
    let now = crate::arch::time::now_ms();
    let at = if due < 0 {
        now + ((-due) as u64).div_ceil(10_000)
    } else {
        now + (due.saturating_sub(ke::system_time()).max(0) as u64).div_ceil(10_000)
    };
    let was = locked(|| {
        let mut list = TIMERS.lock();
        let before = list.len();
        list.retain(|&(x, _)| x != t as usize);
        unsafe {
            (t.add(4) as *mut i32).write(0);
            field(t, TIMER_DUE).write(at);
            field(t, TIMER_DPC).write(dpc as u64);
            (t.add(TIMER_PERIOD) as *mut i32).write(period.max(0));
        }
        list.push((t as usize, at));
        list.len() == before
    });
    was as u8
}

pub extern "win64" fn KeSetTimer(t: *mut u8, due: i64, dpc: *mut u8) -> u8 {
    KeSetTimerEx(t, due, 0, dpc)
}

pub extern "win64" fn KeSetCoalescableTimer(t: *mut u8, due: i64, period: u32, _tolerance: u32, dpc: *mut u8) -> u8 {
    KeSetTimerEx(t, due, period as i32, dpc)
}

pub extern "win64" fn KeCancelTimer(t: *mut u8) -> u8 {
    locked(|| {
        let mut list = TIMERS.lock();
        let before = list.len();
        list.retain(|&(x, _)| x != t as usize);
        (list.len() != before) as u8
    })
}

pub extern "win64" fn KeReadStateTimer(t: *mut u8) -> u8 {
    (unsafe { (t.add(4) as *const i32).read_volatile() } != 0) as u8
}

/// From the BSP's timer interrupt: fire due timers, then run queued DPCs.
/// Skipped if a driver holds either list right now (next tick then).
pub fn tick(now: u64) {
    let mut due: Vec<(usize, u64)> = Vec::new();
    if let Some(mut list) = TIMERS.try_lock() {
        list.retain(|&(t, at)| {
            if at <= now {
                due.push((t, at));
                false
            } else {
                true
            }
        });
        for &(t, _) in &due {
            let period = unsafe { ((t as *mut u8).add(TIMER_PERIOD) as *const i32).read() };
            if period > 0 {
                list.push((t, now + period as u64));
            }
        }
    }
    for (t, _) in due {
        let t = t as *mut u8;
        unsafe {
            (t.add(4) as *mut i32).write_volatile(1);
            let dpc = field(t, TIMER_DPC).read() as *mut u8;
            if !dpc.is_null() {
                let st = ke::system_time() as u64;
                let _ = locked(|| {
                    let Some(mut q) = QUEUE.try_lock() else { return };
                    if field(dpc, DPC_DATA).read() == 0 {
                        field(dpc, DPC_ARG1).write(st & 0xFFFF_FFFF);
                        field(dpc, DPC_ARG2).write(st >> 32);
                        field(dpc, DPC_DATA).write(1);
                        q.push(dpc as usize);
                    }
                });
            }
        }
    }
    let pending = QUEUE.try_lock().is_some_and(|q| !q.is_empty());
    if pending && ke::irql() < ke::DISPATCH_LEVEL {
        drain();
    }
}
