// =============================================================================
// VEILOS: kernel threads
// =============================================================================
//
// Each CPU runs a set of kernel threads, each with its own stack and saved
// register + FPU/SSE state. Thread 0 on every CPU is the code that booted it
// (the BSP's executor, an AP's park loop); it is registered on first use and
// never exits.
//
// Switching policy. A thread must never be suspended while it holds a
// spinlock: on one CPU the next thread would spin on that lock with nobody
// left to release it. So:
//   • A thread interrupted in ring 3 is preempted by the timer once its slice
//     is used up (`preempt_from_timer`). Ring-3 code holds no kernel locks,
//     and syscalls run with IF=0, so the timer can never land inside one.
//   • Kernel code switches only where it chooses to (`yield_now`), at points
//     where it holds nothing: the executor between task polls, or when idle.
//
// Limits for now: one user thread per CPU at a time, since the user long-jump
// context (`loader` EXEC_CTX), the TSS RSP0 stack and the syscall stack are
// still per CPU. Threads never move between CPUs.

use alloc::{boxed::Box, vec::Vec};
use spin::Mutex;
use x86_64::instructions::interrupts::without_interrupts;

use crate::smp::MAX_CPUS;

const STACK_SIZE: usize = 64 * 1024;

/// How long a ring-3 thread runs before the timer may switch away from it.
pub const USER_SLICE_MS: u64 = 10;

/// Saved state of a switched-out thread. `fx` must be 16-byte aligned.
#[repr(C, align(16))]
struct SavedState {
    rsp: u64,
    _pad: u64,
    fx: [u8; 512],
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum State {
    Ready,
    Running,
    Dead,
}

struct Thread {
    id: u64,
    name: &'static str,
    state: State,
    /// Lives inside the Box, so its address is stable while the thread exists.
    saved: SavedState,
    /// None for a CPU's boot thread, which runs on the stack it booted on.
    stack: Option<Box<[u8]>>,
    entry: Option<Box<dyn FnOnce() + Send>>,
}

struct CpuThreads {
    threads: Vec<Box<Thread>>,
    /// Index of the running thread in `threads`.
    current: usize,
    /// TICKS when the running thread was switched in.
    slice_start: u64,
    next_id: u64,
}

impl CpuThreads {
    const fn new() -> Self {
        Self { threads: Vec::new(), current: 0, slice_start: 0, next_id: 1 }
    }

    fn ensure_boot_thread(&mut self) {
        if self.threads.is_empty() {
            self.threads.push(Box::new(Thread {
                id: 0,
                name: "boot",
                state: State::Running,
                saved: SavedState { rsp: 0, _pad: 0, fx: [0; 512] },
                stack: None,
                entry: None,
            }));
            self.current = 0;
        }
    }

    /// Next Ready thread after the current one, round-robin.
    fn next_ready(&self) -> Option<usize> {
        let n = self.threads.len();
        (1..n).map(|k| (self.current + k) % n).find(|&i| self.threads[i].state == State::Ready)
    }
}

static CPUS: [Mutex<CpuThreads>; MAX_CPUS] = [const { Mutex::new(CpuThreads::new()) }; MAX_CPUS];

fn now_ms() -> u64 {
    crate::interrupts::TICKS.load(core::sync::atomic::Ordering::Relaxed)
}

/// Save the current thread's callee-saved registers, RFLAGS and FPU/SSE state
/// into `old`, then resume the thread whose state is in `new`.
#[unsafe(naked)]
unsafe extern "C" fn switch_context(old: *mut SavedState, new: *const SavedState) {
    core::arch::naked_asm!(
        "pushfq",
        "push rbp",
        "push rbx",
        "push r12",
        "push r13",
        "push r14",
        "push r15",
        "fxsave64 [rdi + 16]",
        "mov [rdi], rsp",
        "mov rsp, [rsi]",
        "fxrstor64 [rsi + 16]",
        "pop r15",
        "pop r14",
        "pop r13",
        "pop r12",
        "pop rbx",
        "pop rbp",
        "popfq",
        "ret",
    );
}

/// First code a new thread runs (reached by `switch_context`'s `ret`).
extern "C" fn thread_start() -> ! {
    let cpu = crate::smp::cpu_index();
    let entry = {
        let mut ct = CPUS[cpu].lock();
        let cur = ct.current;
        ct.threads[cur].entry.take()
    };
    // Switched in with IF=0 (the initial RFLAGS); the thread itself runs with
    // interrupts on.
    x86_64::instructions::interrupts::enable();
    if let Some(f) = entry {
        f();
    }
    exit_current()
}

/// Switch from the running thread to the next Ready one, leaving the running
/// one in `becomes`. Interrupts must be off. Returns false (and does nothing)
/// if no other thread is Ready.
unsafe fn reschedule(cpu: usize, becomes: State) -> bool {
    let (old, new) = {
        let mut ct = CPUS[cpu].lock();
        ct.ensure_boot_thread();
        let Some(next) = ct.next_ready() else { return false };
        let cur = ct.current;
        ct.threads[cur].state = becomes;
        ct.threads[next].state = State::Running;
        ct.current = next;
        ct.slice_start = now_ms();
        (
            &mut ct.threads[cur].saved as *mut SavedState,
            &ct.threads[next].saved as *const SavedState,
        )
    };
    unsafe { switch_context(old, new) };
    true
}

/// Start a kernel thread on the current CPU. It first runs the next time this
/// CPU switches threads.
pub fn spawn(name: &'static str, f: impl FnOnce() + Send + 'static) -> u64 {
    let mut stack = alloc::vec![0u8; STACK_SIZE].into_boxed_slice();
    let top = (stack.as_mut_ptr() as u64 + STACK_SIZE as u64) & !0xF;

    // Initial frame, in the order switch_context pops it: r15 r14 r13 r12 rbx
    // rbp, RFLAGS (IF=0), return address, then a dummy return slot so
    // thread_start sees the stack alignment of a normal call.
    let frame: [u64; 9] = [0, 0, 0, 0, 0, 0, 0x2, thread_start as usize as u64, 0];
    let rsp = top - (frame.len() * 8) as u64;
    unsafe {
        core::ptr::copy_nonoverlapping(frame.as_ptr(), rsp as *mut u64, frame.len());
    }

    let mut thread = Box::new(Thread {
        id: 0,
        name,
        state: State::Ready,
        saved: SavedState { rsp, _pad: 0, fx: [0; 512] },
        stack: Some(stack),
        entry: Some(Box::new(f)),
    });
    // Start from this CPU's current FPU/SSE control state (MXCSR, x87 CW).
    unsafe {
        core::arch::asm!("fxsave64 [{}]", in(reg) thread.saved.fx.as_mut_ptr(), options(nostack));
    }

    let cpu = crate::smp::cpu_index();
    without_interrupts(|| {
        let mut ct = CPUS[cpu].lock();
        ct.ensure_boot_thread();
        thread.id = ct.next_id;
        ct.next_id += 1;
        let id = thread.id;
        crate::serial_println!("[KTHREAD] CPU {}: spawned thread {} '{}'", cpu, id, name);
        ct.threads.push(thread);
        id
    })
}

/// End the running thread. Its stack is freed later by `reap`, from another
/// thread.
pub fn exit_current() -> ! {
    x86_64::instructions::interrupts::disable();
    let cpu = crate::smp::cpu_index();
    unsafe { reschedule(cpu, State::Dead) };
    // reschedule only fails if nothing else is Ready, but the CPU's boot
    // thread always is, unless this *is* the boot thread.
    panic!("[KTHREAD] boot thread tried to exit");
}

/// Give the CPU to the next Ready thread, if any. Call only where this thread
/// holds no spinlocks.
pub fn yield_now() {
    let cpu = crate::smp::cpu_index();
    without_interrupts(|| unsafe {
        reschedule(cpu, State::Ready);
    });
    reap(cpu);
}

/// Yield if another thread is waiting and this one has run for `slice_ms`.
pub fn yield_if_slice_used(slice_ms: u64) {
    let cpu = crate::smp::cpu_index();
    let due = without_interrupts(|| {
        let ct = CPUS[cpu].lock();
        ct.next_ready().is_some() && now_ms().wrapping_sub(ct.slice_start) >= slice_ms
    });
    if due {
        yield_now();
    }
}

/// True if another thread on this CPU is waiting to run.
pub fn others_ready() -> bool {
    let cpu = crate::smp::cpu_index();
    without_interrupts(|| CPUS[cpu].lock().next_ready().is_some())
}

/// Called from the timer ISR (interrupts off, no locks held) when it
/// interrupted ring 3: switch away once the thread's slice is used up.
pub fn preempt_from_timer() {
    let cpu = crate::smp::cpu_index();
    let due = {
        let ct = CPUS[cpu].lock();
        now_ms().wrapping_sub(ct.slice_start) >= USER_SLICE_MS
    };
    if due {
        unsafe { reschedule(cpu, State::Ready) };
    }
}

/// Free the stacks of threads that have exited on `cpu`.
fn reap(cpu: usize) {
    let dead: Vec<Box<Thread>> = without_interrupts(|| {
        let mut ct = CPUS[cpu].lock();
        if !ct.threads.iter().any(|t| t.state == State::Dead) {
            return Vec::new();
        }
        let current_id = ct.threads[ct.current].id;
        let (dead, live): (Vec<_>, Vec<_>) =
            core::mem::take(&mut ct.threads).into_iter().partition(|t| t.state == State::Dead);
        ct.threads = live;
        ct.current = ct.threads.iter().position(|t| t.id == current_id).unwrap_or(0);
        dead
    });
    for t in &dead {
        crate::serial_println!("[KTHREAD] CPU {}: reaped thread {} '{}'", cpu, t.id, t.name);
    }
    // Dropped here, with interrupts on (heap frees are IRQ-safe either way).
}
