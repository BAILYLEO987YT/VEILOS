// -----| Random numbers for keys |-----
//
// A SHA-256 pool. Seeded at boot from RDSEED/RDRAND when the CPU has them,
// the time stamp counter, the PIT and the real-time clock, and stirred
// afterwards with the timing of interrupts (timer, keyboard, mouse): when
// they arrive in TSC cycles is the part nobody outside can predict.
// `fill` hashes the pool with a counter and RDRAND (when there is one) for
// each block and then moves the pool on, so earlier output can't be rebuilt
// from a later pool (forward secrecy).
//
// Used for the key ledger's keys (encryption.rs) and for Windows programs'
// ProcessPrng / RtlGenRandom / BCryptGenRandom (VEIL_SYSINFO op 0).

use core::sync::atomic::{AtomicU64, Ordering};
use sha2::{Digest, Sha256};
use spin::Mutex;

static POOL: Mutex<[u8; 32]> = Mutex::new([0; 32]);
static COUNTER: AtomicU64 = AtomicU64::new(0);
/// Interrupt timings not yet mixed in (folded cheaply in the interrupt).
static PENDING: AtomicU64 = AtomicU64::new(0);
static PENDING_COUNT: AtomicU64 = AtomicU64::new(0);

fn has_rdrand() -> bool {
    unsafe { core::arch::x86_64::__cpuid(1) }.ecx & (1 << 30) != 0
}

fn has_rdseed() -> bool {
    unsafe { core::arch::x86_64::__cpuid_count(7, 0) }.ebx & (1 << 18) != 0
}

fn hw_random() -> Option<u64> {
    let mut v = 0u64;
    if has_rdseed() && unsafe { core::arch::x86_64::_rdseed64_step(&mut v) } == 1 {
        return Some(v);
    }
    if has_rdrand() && unsafe { core::arch::x86_64::_rdrand64_step(&mut v) } == 1 {
        return Some(v);
    }
    None
}

fn mix(pool: &mut [u8; 32], data: &[u8]) {
    let mut h = Sha256::new();
    h.update(*pool);
    h.update(data);
    pool.copy_from_slice(&h.finalize());
}

/// Seed the pool. Call once, early in boot.
pub fn init() {
    let mut pool = POOL.lock();
    for _ in 0..8 {
        if let Some(v) = hw_random() {
            mix(&mut pool, &v.to_le_bytes());
        }
    }
    // The RTC's date and time, the PIT's count and TSC jitter between them.
    for reg in [0x00u8, 0x02, 0x04, 0x07, 0x08, 0x09] {
        let v = unsafe {
            use x86_64::instructions::port::Port;
            Port::<u8>::new(0x70).write(reg);
            Port::<u8>::new(0x71).read()
        };
        let t = crate::time::rdtsc();
        mix(&mut pool, &[v]);
        mix(&mut pool, &t.to_le_bytes());
    }
    for _ in 0..64 {
        let pit: u8 = unsafe { x86_64::instructions::port::Port::<u8>::new(0x40).read() };
        let t = crate::time::rdtsc();
        mix(&mut pool, &[pit]);
        mix(&mut pool, &t.to_le_bytes());
    }
    crate::serial_println!("[ENTROPY] pool seeded (RDSEED {}, RDRAND {})", has_rdseed(), has_rdrand());
}

/// From an interrupt handler: the moment it ran, and anything else it knows.
/// Cheap: a fold into an atomic, hashed into the pool every 64 events.
#[inline]
pub fn stir(extra: u64) {
    let t = crate::time::rdtsc();
    let v = t.rotate_left((extra & 63) as u32) ^ extra;
    let old = PENDING.load(Ordering::Relaxed);
    PENDING.store(old.rotate_left(7) ^ v, Ordering::Relaxed);
    if PENDING_COUNT.fetch_add(1, Ordering::Relaxed) % 64 == 63 {
        if let Some(mut pool) = POOL.try_lock() {
            let p = PENDING.swap(0, Ordering::Relaxed);
            mix(&mut pool, &p.to_le_bytes());
        }
    }
}

/// Random bytes, good for keys.
pub fn fill(out: &mut [u8]) {
    x86_64::instructions::interrupts::without_interrupts(|| {
        let mut pool = POOL.lock();
        let p = PENDING.swap(0, Ordering::Relaxed);
        mix(&mut pool, &p.to_le_bytes());
        for chunk in out.chunks_mut(32) {
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let mut h = Sha256::new();
            h.update(*pool);
            h.update(b"out");
            h.update(n.to_le_bytes());
            h.update(crate::time::rdtsc().to_le_bytes());
            if let Some(v) = hw_random() {
                h.update(v.to_le_bytes());
            }
            let block = h.finalize();
            chunk.copy_from_slice(&block[..chunk.len()]);
        }
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        mix(&mut pool, &n.to_le_bytes());
        mix(&mut pool, b"next");
    });
}
