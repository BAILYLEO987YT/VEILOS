// IRQL, spinlocks, dispatcher objects (events, semaphores, mutexes) and
// waits, time, bug checks.
//
// IRQL is CR8, the local APIC's task priority, exactly as on Windows x64:
// the WDK's KeGetCurrentIrql / KeRaiseIrql / KeLowerIrql are inline CR8
// reads and writes, so drivers change it without calling in. CR8 = n masks
// interrupt vectors whose top nibble is n or less: at DISPATCH_LEVEL (2),
// VEILOS's PIT tick (0x20) and PS/2 IRQs wait until it drops; USB (0x50) and
// the AP timer (0x40) still come.
//
// Waits poll the object between pauses (`pause`: another thread's turn, or
// a sleep until the next interrupt); nothing in W1 needs a wait list.

use core::sync::atomic::{AtomicI32, AtomicU64, Ordering};

use super::{NtStatus, STATUS_SUCCESS, STATUS_TIMEOUT};

pub const PASSIVE_LEVEL: u8 = 0;
pub const DISPATCH_LEVEL: u8 = 2;

pub fn irql() -> u8 {
    let v: u64;
    unsafe { core::arch::asm!("mov {}, cr8", out(reg) v, options(nomem, nostack, preserves_flags)) };
    v as u8
}

pub fn set_irql(l: u8) {
    unsafe { core::arch::asm!("mov cr8, {}", in(reg) l as u64, options(nomem, nostack, preserves_flags)) };
}

pub extern "win64" fn KeGetCurrentIrql() -> u8 {
    irql()
}

pub extern "win64" fn KfRaiseIrql(new: u8) -> u8 {
    let old = irql();
    if new > old {
        set_irql(new);
    }
    old
}

pub extern "win64" fn KeLowerIrql(new: u8) {
    set_irql(new);
}

// -----| spinlocks (KSPIN_LOCK: one pointer-sized word) |-----

fn spin_acquire(lock: *mut u64) {
    let a = unsafe { &*(lock as *const AtomicU64) };
    while a.compare_exchange_weak(0, 1, Ordering::Acquire, Ordering::Relaxed).is_err() {
        core::hint::spin_loop();
    }
}

fn spin_release(lock: *mut u64) {
    unsafe { &*(lock as *const AtomicU64) }.store(0, Ordering::Release);
}

pub extern "win64" fn KeInitializeSpinLock(lock: *mut u64) {
    unsafe { lock.write(0) };
}

pub extern "win64" fn KeAcquireSpinLockRaiseToDpc(lock: *mut u64) -> u8 {
    let old = KfRaiseIrql(DISPATCH_LEVEL);
    spin_acquire(lock);
    old
}

pub extern "win64" fn KeReleaseSpinLock(lock: *mut u64, new: u8) {
    spin_release(lock);
    set_irql(new);
}

pub extern "win64" fn KeAcquireSpinLockAtDpcLevel(lock: *mut u64) {
    spin_acquire(lock);
}

pub extern "win64" fn KeReleaseSpinLockFromDpcLevel(lock: *mut u64) {
    spin_release(lock);
}

pub extern "win64" fn KeTryToAcquireSpinLockAtDpcLevel(lock: *mut u64) -> u8 {
    let a = unsafe { &*(lock as *const AtomicU64) };
    a.compare_exchange(0, 1, Ordering::Acquire, Ordering::Relaxed).is_ok() as u8
}

/// KLOCK_QUEUE_HANDLE: LockQueue { Next, Lock } (16 bytes), then OldIrql.
/// VEILOS keeps the lock as a plain spinlock: the queue is Windows' way to be
/// fair, and drivers never look inside it.
pub extern "win64" fn KeAcquireInStackQueuedSpinLock(lock: *mut u64, handle: *mut u8) {
    let old = KfRaiseIrql(DISPATCH_LEVEL);
    spin_acquire(lock);
    unsafe {
        (handle as *mut u64).write(0);
        (handle as *mut *mut u64).add(1).write(lock);
        handle.add(16).write(old);
    }
}

pub extern "win64" fn KeReleaseInStackQueuedSpinLock(handle: *mut u8) {
    unsafe {
        let lock = (handle as *mut *mut u64).add(1).read();
        let old = handle.add(16).read();
        spin_release(lock);
        set_irql(old);
    }
}

pub extern "win64" fn KeAcquireInStackQueuedSpinLockAtDpcLevel(lock: *mut u64, handle: *mut u8) {
    spin_acquire(lock);
    unsafe { (handle as *mut *mut u64).add(1).write(lock) };
}

pub extern "win64" fn KeReleaseInStackQueuedSpinLockFromDpcLevel(handle: *mut u8) {
    spin_release(unsafe { (handle as *mut *mut u64).add(1).read() });
}

// -----| dispatcher objects |-----
//
// DISPATCHER_HEADER (0x18 bytes): Type (u8) at 0, Size (u8, in dwords) at
// 2, SignalState (i32) at 4, WaitListHead (LIST_ENTRY) at 8.

const NOTIFICATION_EVENT: u8 = 0;
const SYNCHRONIZATION_EVENT: u8 = 1;
const MUTANT: u8 = 2;
const SEMAPHORE: u8 = 5;
const SYNCHRONIZATION_TIMER: u8 = 9;

fn header_init(obj: *mut u8, kind: u8, size_bytes: usize, state: i32) {
    unsafe {
        core::ptr::write_bytes(obj, 0, 0x18);
        obj.write(kind);
        obj.add(2).write((size_bytes / 4) as u8);
        (obj.add(4) as *mut i32).write(state);
        let list = obj.add(8) as *mut u64;
        list.write(list as u64);
        list.add(1).write(list as u64);
    }
}

fn signal(obj: *mut u8) -> &'static AtomicI32 {
    unsafe { &*(obj.add(4) as *const AtomicI32) }
}

pub extern "win64" fn KeInitializeEvent(ev: *mut u8, kind: u32, state: u8) {
    header_init(ev, kind as u8, 0x18, state as i32);
}

pub extern "win64" fn KeSetEvent(ev: *mut u8, _increment: i32, _wait: u8) -> i32 {
    signal(ev).swap(1, Ordering::AcqRel)
}

pub extern "win64" fn KeResetEvent(ev: *mut u8) -> i32 {
    signal(ev).swap(0, Ordering::AcqRel)
}

pub extern "win64" fn KeClearEvent(ev: *mut u8) {
    signal(ev).store(0, Ordering::Release);
}

pub extern "win64" fn KeReadStateEvent(ev: *mut u8) -> i32 {
    signal(ev).load(Ordering::Acquire)
}

/// KSEMAPHORE: header, then Limit (i32) at 0x18.
pub extern "win64" fn KeInitializeSemaphore(sem: *mut u8, count: i32, limit: i32) {
    header_init(sem, SEMAPHORE, 0x20, count);
    unsafe { (sem.add(0x18) as *mut i32).write(limit) };
}

pub extern "win64" fn KeReleaseSemaphore(sem: *mut u8, _increment: i32, adjustment: i32, _wait: u8) -> i32 {
    let limit = unsafe { (sem.add(0x18) as *const i32).read() };
    let s = signal(sem);
    let old = s.fetch_add(adjustment, Ordering::AcqRel);
    if old + adjustment > limit {
        s.fetch_sub(adjustment, Ordering::AcqRel);
    }
    old
}

/// KMUTANT: header, then MutantListEntry, OwnerThread (at 0x28), ...
/// VEILOS's version: SignalState 1 = free, 0 = owned (no recursion count).
pub extern "win64" fn KeInitializeMutex(m: *mut u8, _level: u32) {
    header_init(m, MUTANT, 0x38, 1);
}

pub extern "win64" fn KeReleaseMutex(m: *mut u8, _wait: u8) -> i32 {
    signal(m).swap(1, Ordering::AcqRel)
}

/// Take the object if it's signalled; true if this wait is satisfied.
fn try_take(obj: *mut u8) -> bool {
    let kind = unsafe { obj.read() };
    let s = signal(obj);
    match kind {
        NOTIFICATION_EVENT => s.load(Ordering::Acquire) > 0,
        // Satisfying a wait on these consumes a signal.
        SYNCHRONIZATION_EVENT | MUTANT | SYNCHRONIZATION_TIMER => s.compare_exchange(1, 0, Ordering::AcqRel, Ordering::Relaxed).is_ok(),
        SEMAPHORE => {
            let mut v = s.load(Ordering::Acquire);
            while v > 0 {
                match s.compare_exchange(v, v - 1, Ordering::AcqRel, Ordering::Relaxed) {
                    Ok(_) => return true,
                    Err(now) => v = now,
                }
            }
            false
        }
        _ => s.load(Ordering::Acquire) > 0,
    }
}

/// A Windows timeout (100 ns units; negative = relative, positive =
/// absolute system time) as a deadline in TSC-calibrated milliseconds,
/// None for "wait forever".
fn deadline(timeout: *const i64) -> Option<u64> {
    if timeout.is_null() {
        return None;
    }
    let t = unsafe { timeout.read() };
    let now = crate::arch::time::now_ms();
    Some(if t < 0 {
        now + ((-t) as u64).div_ceil(10_000)
    } else {
        let sys = system_time();
        now + (t.saturating_sub(sys).max(0) as u64).div_ceil(10_000)
    })
}

/// Wait until `done()` (another part of ntos's own wait: KeRemoveQueue).
pub fn wait_for(done: impl FnMut() -> bool, timeout: *const i64) -> NtStatus {
    wait_until(done, timeout)
}

/// While spinning on a lock or a busy object: pause below DISPATCH_LEVEL,
/// just spin at or above it.
pub fn pause_at_current_irql() {
    if irql() < DISPATCH_LEVEL {
        pause();
    } else {
        core::hint::spin_loop();
    }
}

fn wait_until(mut done: impl FnMut() -> bool, timeout: *const i64) -> NtStatus {
    let end = deadline(timeout);
    loop {
        if done() {
            return STATUS_SUCCESS;
        }
        if end.is_some_and(|e| crate::arch::time::now_ms() >= e) {
            return STATUS_TIMEOUT;
        }
        if irql() < DISPATCH_LEVEL {
            pause();
        } else {
            core::hint::spin_loop();
        }
    }
}

/// Between looks at what a wait waits for, below DISPATCH_LEVEL: another
/// thread on this CPU if one is ready (a driver's own system thread or work
/// item, say), else sleep until the next interrupt. A program's system call
/// runs with interrupts off, and a driver's IOCTL waits inside one: then
/// interrupts are on only for the HLT, as VEILOS's own waits
/// (nt_syscalls::wait_a_little) do it, so the tick that fires timers and
/// DPCs still comes. Elsewhere they're left as they were.
fn pause() {
    use x86_64::instructions::interrupts;
    if crate::sched::kthread::others_ready() {
        crate::sched::kthread::yield_now();
        return;
    }
    let were_on = interrupts::are_enabled();
    interrupts::enable_and_hlt();
    if !were_on {
        interrupts::disable();
    }
}

pub extern "win64" fn KeWaitForSingleObject(obj: *mut u8, _reason: u32, _mode: u8, _alertable: u8, timeout: *const i64) -> NtStatus {
    wait_until(|| try_take(obj), timeout)
}

/// WaitAny (1): the index of the object that ended the wait; WaitAll (0):
/// STATUS_SUCCESS once every one could be taken (taken one at a time).
pub extern "win64" fn KeWaitForMultipleObjects(
    count: u32,
    objs: *const *mut u8,
    wait_type: u32,
    _reason: u32,
    _mode: u8,
    _alertable: u8,
    timeout: *const i64,
    _blocks: *mut u8,
) -> NtStatus {
    let objs = unsafe { core::slice::from_raw_parts(objs, count as usize) };
    if wait_type == 1 {
        let mut which = 0;
        let st = wait_until(
            || {
                for (i, &o) in objs.iter().enumerate() {
                    if try_take(o) {
                        which = i;
                        return true;
                    }
                }
                false
            },
            timeout,
        );
        if st == STATUS_SUCCESS { which as NtStatus } else { st }
    } else {
        let mut taken = alloc::vec![false; objs.len()];
        wait_until(
            || {
                for (i, &o) in objs.iter().enumerate() {
                    if !taken[i] && try_take(o) {
                        taken[i] = true;
                    }
                }
                taken.iter().all(|&t| t)
            },
            timeout,
        )
    }
}

pub extern "win64" fn KeDelayExecutionThread(_mode: u8, _alertable: u8, interval: *const i64) -> NtStatus {
    let _ = wait_until(|| false, interval);
    STATUS_SUCCESS
}

// -----| time |-----

/// Windows system time: 100 ns units since 1601-01-01 UTC.
pub fn system_time() -> i64 {
    const UNIX_TO_1601_US: i64 = 11_644_473_600_000_000;
    (crate::net::network::wall_clock_us() + UNIX_TO_1601_US) * 10
}

pub extern "win64" fn KeQuerySystemTimePrecise(out: *mut i64) {
    unsafe { out.write(system_time()) };
}

pub extern "win64" fn KeQuerySystemTime(out: *mut i64) {
    unsafe { out.write(system_time()) };
}

pub extern "win64" fn ExSystemTimeToLocalTime(sys: *const i64, local: *mut i64) {
    unsafe { local.write(sys.read()) };
}

/// The clock interrupt's period in 100 ns units: VEILOS's tick is 1 ms.
pub extern "win64" fn KeQueryTimeIncrement() -> u32 {
    10_000
}

/// hal.dll: a counter (the TSC) and its frequency.
pub extern "win64" fn KeQueryPerformanceCounter(freq: *mut i64) -> i64 {
    if !freq.is_null() {
        unsafe { freq.write(crate::arch::time::tsc_hz() as i64) };
    }
    unsafe { core::arch::x86_64::_rdtsc() as i64 }
}

pub extern "win64" fn KeStallExecutionProcessor(us: u32) {
    crate::arch::time::spin_until(us as u64, || false);
}

pub extern "win64" fn KeQueryInterruptTime() -> u64 {
    crate::arch::time::now_ms() * 10_000
}

// -----| processors |-----

pub extern "win64" fn KeQueryActiveProcessorCount(affinity: *mut u64) -> u32 {
    let n = crate::arch::smp::CPU_ONLINE.load(Ordering::Relaxed).count_ones();
    if !affinity.is_null() {
        unsafe { affinity.write(if n >= 64 { u64::MAX } else { (1u64 << n) - 1 }) };
    }
    n
}

pub extern "win64" fn KeGetCurrentProcessorNumberEx(number: *mut u8) -> u32 {
    let cpu = crate::arch::smp::cpu_index() as u32;
    if !number.is_null() {
        unsafe {
            (number as *mut u16).write(0); // group
            number.add(2).write(cpu as u8);
            number.add(3).write(0);
        }
    }
    cpu
}

// -----| critical regions (APC disabling): nothing to do without APCs |-----

pub extern "win64" fn KeEnterCriticalRegion() {}
pub extern "win64" fn KeLeaveCriticalRegion() {}

// -----| bug checks |-----

pub extern "win64" fn KeBugCheckEx(code: u32, p1: u64, p2: u64, p3: u64, p4: u64) -> ! {
    panic!("driver bug check {:#010x} ({:#x}, {:#x}, {:#x}, {:#x})", code, p1, p2, p3, p4);
}

pub extern "win64" fn KeBugCheck(code: u32) -> ! {
    KeBugCheckEx(code, 0, 0, 0, 0)
}
