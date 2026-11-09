// =============================================================================
// VEILOS: worker pool (HAL step 7B)
// =============================================================================
//
// `for_each(n, f)` runs f(0) .. f(n-1) spread over every CPU and returns once
// all of them have finished. The caller takes part, so on one CPU (or when
// every AP is busy) the work simply runs in order on the caller; nothing ever
// waits for a worker that might not come. APs pick work up from their boot
// thread (`smp::park_and_wait_for_work`), between a user process's time
// slices, and are woken by an IPI when a batch is posted.
//
// `f` may borrow from the caller's stack: `for_each` does not return until no
// other CPU can still touch the batch (it has been unlisted and its `users`
// count is back to zero), the same guarantee `std::thread::scope` gives.
//
// This is the pool the logical GPU's CPU backends run on (HAL_DESIGN.md 7C).

use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use spin::Mutex;

use crate::smp::MAX_CPUS;

struct Batch {
    /// Lifetime erased; valid until `for_each` returns (see module docs).
    f: *const (dyn Fn(usize) + Sync + 'static),
    n: usize,
    /// Next index to hand out.
    next: AtomicUsize,
    /// Indices finished.
    done: AtomicUsize,
    /// Other CPUs currently holding a pointer to this batch.
    users: AtomicUsize,
}

/// Addresses of the batches currently being worked on.
static ACTIVE: Mutex<Vec<usize>> = Mutex::new(Vec::new());

/// Items each CPU has run (diagnostics; the WORKERS command).
static ITEMS_RUN: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];

fn run_items(batch: &Batch) {
    let f = unsafe { &*batch.f };
    let cpu = crate::smp::cpu_index();
    loop {
        let i = batch.next.fetch_add(1, Ordering::AcqRel);
        if i >= batch.n {
            break;
        }
        f(i);
        ITEMS_RUN[cpu].fetch_add(1, Ordering::Relaxed);
        batch.done.fetch_add(1, Ordering::Release);
    }
}

/// Run `f(i)` for every `i` in `0..n` on all CPUs; returns when all are done.
pub fn for_each<F: Fn(usize) + Sync>(n: usize, f: &F) {
    if n == 0 {
        return;
    }
    let f_dyn: &(dyn Fn(usize) + Sync) = f;
    // SAFETY: the pointer is only dereferenced while this function is still
    // running (we wait for `users` to drain below), so `f` outlives every use.
    let f_static: *const (dyn Fn(usize) + Sync + 'static) = unsafe { core::mem::transmute(f_dyn) };
    let batch = Batch {
        f: f_static,
        n,
        next: AtomicUsize::new(0),
        done: AtomicUsize::new(0),
        users: AtomicUsize::new(0),
    };
    let addr = &batch as *const Batch as usize;

    let parallel = crate::smp::CPU_COUNT.load(Ordering::Relaxed) > 1 && n > 1;
    if parallel {
        x86_64::instructions::interrupts::without_interrupts(|| ACTIVE.lock().push(addr));
        crate::smp::wake_aps();
    }

    run_items(&batch);

    if parallel {
        // No CPU can pick the batch up after this; then wait for those that did.
        x86_64::instructions::interrupts::without_interrupts(|| ACTIVE.lock().retain(|&a| a != addr));
        while batch.done.load(Ordering::Acquire) < n || batch.users.load(Ordering::Acquire) > 0 {
            core::hint::spin_loop();
        }
    }
}

/// Take part in any posted batch. Called by idle CPUs (`smp`). Returns true if
/// it did any work.
pub fn help() -> bool {
    let claimed = x86_64::instructions::interrupts::without_interrupts(|| {
        let active = ACTIVE.lock();
        active.iter().copied().find(|&a| {
            let b = unsafe { &*(a as *const Batch) };
            b.next.load(Ordering::Acquire) < b.n
        }).map(|a| {
            // Registered while the list is locked, so `for_each` (which
            // unlists under the same lock) will wait for us.
            unsafe { &*(a as *const Batch) }.users.fetch_add(1, Ordering::AcqRel);
            a
        })
    });
    let Some(addr) = claimed else { return false };
    let batch = unsafe { &*(addr as *const Batch) };
    run_items(batch);
    batch.users.fetch_sub(1, Ordering::AcqRel);
    true
}

/// True if a posted batch still has unclaimed work.
pub fn pending() -> bool {
    x86_64::instructions::interrupts::without_interrupts(|| {
        ACTIVE.lock().iter().any(|&a| {
            let b = unsafe { &*(a as *const Batch) };
            b.next.load(Ordering::Acquire) < b.n
        })
    })
}

/// Items run per CPU since boot.
pub fn items_run() -> Vec<u64> {
    let cpus = (crate::smp::CPU_COUNT.load(Ordering::Relaxed) as usize).clamp(1, MAX_CPUS);
    (0..cpus).map(|c| ITEMS_RUN[c].load(Ordering::Relaxed)).collect()
}

/// Time `items` chunks of `work` busy-loop iterations run sequentially on this
/// CPU, then through `for_each`. Returns (sequential µs, parallel µs, items
/// each CPU ran in the parallel pass).
pub fn benchmark(items: usize, work: u64) -> (u64, u64, Vec<u64>) {
    let chunk = |i: usize| {
        let mut x = 0x9E37_79B9_7F4A_7C15u64 ^ i as u64;
        for _ in 0..work {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
        }
        core::hint::black_box(x);
    };
    let t0 = crate::time::now_us();
    for i in 0..items {
        chunk(i);
    }
    let sequential = crate::time::now_us() - t0;

    let before = items_run();
    let t1 = crate::time::now_us();
    for_each(items, &chunk);
    let parallel = crate::time::now_us() - t1;
    let per_cpu = items_run().iter().zip(before.iter().chain(core::iter::repeat(&0))).map(|(a, b)| a - b).collect();
    (sequential, parallel, per_cpu)
}
