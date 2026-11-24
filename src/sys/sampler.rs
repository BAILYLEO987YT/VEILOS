// -----| VEILOS PC sampler |-----
//
// A real sampling profiler for the kernel. The timer ISR records the instruction
// pointer of whatever code it interrupted (the CPU pushed it on the interrupt
// stack frame), so samples land in the GUI loop, the image decoders, the
// renderer, the idle `hlt` loop... wherever time actually goes.
//
// (The older debugger::Profiler reads a saved NT-thread context instead, which
// is not the code the CPU was running, so it cannot see kernel async tasks.)
//
// Usage from the terminal:  SAMPLE START ... do the slow thing ... SAMPLE STOP
//                           SAMPLE DUMP      (prints a histogram to the serial log)
// Then on the host:         python3 symbolize.py --elf <kernel ELF> --log serial.txt
//
// ISR path: one relaxed atomic load when off; one fetch_add + one store when on.
// No locks, no allocation, no panics.
//
// Limits: samples can only land where interrupts are enabled, so code that runs
// with IF=0 is under-counted. 1 sample = 1 timer tick (1 ms at 1 kHz).

use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

/// Ring capacity: 16384 samples = 16 s at 1 kHz.
const RING: usize = 16384;
/// Histogram bucket = 16 bytes of code. The host script maps buckets to functions.
const BUCKET_SHIFT: u32 = 4;
/// Most buckets printed by `dump`.
const MAX_LINES: usize = 600;

const ZERO: AtomicU64 = AtomicU64::new(0);
static RIPS: [AtomicU64; RING] = [ZERO; RING];
static WRITTEN: AtomicUsize = AtomicUsize::new(0);
static ON: AtomicBool = AtomicBool::new(false);
/// Ticks seen while sampling was on (== samples taken, even after the ring wraps).
static TOTAL: AtomicU64 = AtomicU64::new(0);

/// Call from the timer ISR with the interrupted RIP. Cheap and lock-free.
#[inline]
pub fn sample(rip: u64) {
    if !ON.load(Ordering::Relaxed) {
        return;
    }
    let i = WRITTEN.fetch_add(1, Ordering::Relaxed);
    RIPS[i % RING].store(rip, Ordering::Relaxed);
    TOTAL.fetch_add(1, Ordering::Relaxed);
}

pub fn start() {
    WRITTEN.store(0, Ordering::Relaxed);
    TOTAL.store(0, Ordering::Relaxed);
    ON.store(true, Ordering::Release);
}

pub fn stop() {
    ON.store(false, Ordering::Release);
}

pub fn running() -> bool {
    ON.load(Ordering::Relaxed)
}

/// Samples currently held (at most RING).
pub fn count() -> usize {
    WRITTEN.load(Ordering::Relaxed).min(RING)
}

/// Total samples taken since `start` (may exceed what the ring still holds).
pub fn total() -> u64 {
    TOTAL.load(Ordering::Relaxed)
}

/// (bucket start address, hits), most-hit first. Pure: also used by the host tests.
pub fn histogram() -> Vec<(u64, u32)> {
    let n = count();
    let mut buckets: Vec<u64> = Vec::new();
    if buckets.try_reserve_exact(n).is_err() {
        return Vec::new();
    }
    for i in 0..n {
        buckets.push(RIPS[i].load(Ordering::Relaxed) >> BUCKET_SHIFT);
    }
    buckets.sort_unstable();
    let mut out: Vec<(u64, u32)> = Vec::new();
    let mut i = 0;
    while i < buckets.len() {
        let b = buckets[i];
        let mut j = i;
        while j < buckets.len() && buckets[j] == b {
            j += 1;
        }
        out.push((b << BUCKET_SHIFT, (j - i) as u32));
        i = j;
    }
    out.sort_unstable_by(|a, b| b.1.cmp(&a.1));
    out
}

/// Print the histogram to the serial log in the format `symbolize.py` reads.
pub fn dump() {
    let hist = histogram();
    crate::serial_println!("[SAMPLE] BEGIN samples={} total={} bucket_bytes={}", count(), total(), 1u32 << BUCKET_SHIFT);
    for (addr, hits) in hist.iter().take(MAX_LINES) {
        crate::serial_println!("[SAMPLE] {:#x} {}", addr, hits);
    }
    crate::serial_println!("[SAMPLE] END");
}
