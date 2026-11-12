// -----| Time spent in each NT service |-----
//
// SYSPROF START / STOP / DUMP (terminal): calls and TSC cycles per VEIL/NT
// syscall number while on, so the cost of a mode (security.rs) or of a
// service can be measured from the kernel's side. The sampler can't see
// syscall time: interrupts are off for a whole call.

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

const N: usize = 128;
static ON: AtomicBool = AtomicBool::new(false);
static CALLS: [AtomicU64; N] = [const { AtomicU64::new(0) }; N];
static CYCLES: [AtomicU64; N] = [const { AtomicU64::new(0) }; N];

#[inline]
pub fn on() -> bool {
    ON.load(Ordering::Relaxed)
}

pub fn record(index: usize, cycles: u64) {
    if index < N {
        CALLS[index].fetch_add(1, Ordering::Relaxed);
        CYCLES[index].fetch_add(cycles, Ordering::Relaxed);
    }
}

pub fn start() {
    for i in 0..N {
        CALLS[i].store(0, Ordering::Relaxed);
        CYCLES[i].store(0, Ordering::Relaxed);
    }
    ON.store(true, Ordering::Relaxed);
}

pub fn stop() {
    ON.store(false, Ordering::Relaxed);
}

/// Each service used, by total time, to the serial log.
pub fn dump() {
    let hz = crate::time::tsc_hz().max(1);
    let mut rows: alloc::vec::Vec<(u64, u64, usize)> = (0..N)
        .map(|i| (CYCLES[i].load(Ordering::Relaxed), CALLS[i].load(Ordering::Relaxed), i))
        .filter(|r| r.1 > 0)
        .collect();
    rows.sort_by(|a, b| b.0.cmp(&a.0));
    crate::serial_println!("[SYSPROF] BEGIN (BASE+n, calls, total ms, us per call)");
    for (cyc, calls, i) in rows {
        let us = cyc as u128 * 1_000_000 / hz as u128;
        crate::serial_println!("[SYSPROF] {:3} {:8} {:8}.{:03} {:8}.{:02}", i, calls, us / 1000, us % 1000,
            us / calls as u128, (us * 100 / calls as u128) % 100);
    }
    crate::serial_println!("[SYSPROF] END");
}
