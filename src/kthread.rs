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
// User threads. A thread running a Windows process carries what used to be
// per CPU (`UserCtx`): the kernel stack that syscalls and ring-3 interrupts
// enter on, the long-jump context back to its launch point, its process and
// a kill flag. A switch installs the next thread's kernel stack in the TSS
// (RSP0) and the per-CPU block (syscall entry), and swaps GS_BASE and
// KERNEL_GS_BASE, which hold the user TEB on one side or the other. So any
// number of processes can share a CPU. Threads never move between CPUs.

use alloc::{boxed::Box, vec::Vec};
use spin::Mutex;
use x86_64::instructions::interrupts::without_interrupts;

use crate::smp::MAX_CPUS;

const STACK_SIZE: usize = 64 * 1024;
/// A user thread's stack is also its kernel stack for syscalls and
/// interrupts from ring 3.
const USER_STACK_SIZE: usize = 256 * 1024;

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

/// Per-thread state of a thread that runs a Windows process.
pub struct UserCtx {
    /// Long-jump context saved by `loader::launch_into_usermode`.
    pub jump: [u64; 8],
    pub jump_valid: bool,
    /// Kernel stack top for syscalls and ring-3 interrupts (0: not a user thread).
    pub kernel_rsp: u64,
    pub pid: u32,
    /// Set by `kill_pid`; acted on in ring 3 (timer) or by a blocking syscall.
    pub kill: bool,
}

impl UserCtx {
    const fn new(pid: u32) -> Self {
        UserCtx { jump: [0; 8], jump_valid: false, kernel_rsp: 0, pid, kill: false }
    }
}

struct Thread {
    id: u64,
    name: &'static str,
    state: State,
    user: UserCtx,
    /// GS_BASE and KERNEL_GS_BASE while switched out (one holds the user TEB
    /// while the thread is in user mode or a syscall).
    gs_base: u64,
    kernel_gs_base: u64,
    /// Lives inside the Box, so its address is stable while the thread exists.
    saved: SavedState,
    /// None for a CPU's boot thread, which runs on the stack it booted on.
    stack: Option<Box<[u8]>>,
    entry: Option<Box<dyn FnOnce() + Send>>,
    /// Page tables this thread runs on (address_space); 0 = the kernel's.
    cr3: u64,
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
                user: UserCtx::new(0),
                gs_base: 0,
                kernel_gs_base: 0,
                saved: SavedState { rsp: 0, _pad: 0, fx: [0; 512] },
                stack: None,
                entry: None,
                cr3: 0,
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
        // Kernel memory (including every thread's stack) is mapped in every
        // address space, so switching tables before the stack is safe.
        crate::address_space::activate(ct.threads[next].cr3);
        unsafe {
            use x86_64::registers::model_specific::Msr;
            let (mut gs, mut kgs) = (Msr::new(0xC000_0101), Msr::new(0xC000_0102));
            ct.threads[cur].gs_base = gs.read();
            ct.threads[cur].kernel_gs_base = kgs.read();
            gs.write(ct.threads[next].gs_base);
            kgs.write(ct.threads[next].kernel_gs_base);
        }
        let krsp = ct.threads[next].user.kernel_rsp;
        if krsp != 0 {
            crate::gdt::set_rsp0(cpu, krsp);
            crate::smp::set_syscall_stack(cpu, krsp);
        }
        (
            &mut ct.threads[cur].saved as *mut SavedState,
            &ct.threads[next].saved as *const SavedState,
        )
    };
    unsafe { switch_context(old, new) };
    true
}

/// Start a kernel thread on the current CPU, on the kernel's page tables. It
/// first runs the next time this CPU switches threads.
pub fn spawn(name: &'static str, f: impl FnOnce() + Send + 'static) -> u64 {
    spawn_in(name, 0, f)
}

/// Like `spawn`, but the thread runs on the page tables at `cr3` (a process's
/// address space; 0 = the kernel's).
pub fn spawn_in(name: &'static str, cr3: u64, f: impl FnOnce() + Send + 'static) -> u64 {
    spawn_with(name, cr3, 0, STACK_SIZE, f)
}

/// A thread for process `pid`, on its page tables, with a stack big enough
/// to also serve its syscalls and interrupts from ring 3.
pub fn spawn_user(name: &'static str, cr3: u64, pid: u32, f: impl FnOnce() + Send + 'static) -> u64 {
    spawn_with(name, cr3, pid, USER_STACK_SIZE, f)
}

fn spawn_with(name: &'static str, cr3: u64, pid: u32, stack_size: usize, f: impl FnOnce() + Send + 'static) -> u64 {
    let mut stack = alloc::vec![0u8; stack_size].into_boxed_slice();
    let top = (stack.as_mut_ptr() as u64 + stack_size as u64) & !0xF;

    // Initial frame, in the order switch_context pops it: r15 r14 r13 r12 rbx
    // rbp, RFLAGS (IF=0), return address, then a dummy return slot so
    // thread_start sees the stack alignment of a normal call.
    let frame: [u64; 9] = [0, 0, 0, 0, 0, 0, 0x2, thread_start as usize as u64, 0];
    let rsp = top - (frame.len() * 8) as u64;
    unsafe {
        core::ptr::copy_nonoverlapping(frame.as_ptr(), rsp as *mut u64, frame.len());
    }

    let cpu = crate::smp::cpu_index();
    let mut thread = Box::new(Thread {
        id: 0,
        name,
        state: State::Ready,
        user: UserCtx::new(pid),
        gs_base: 0,
        kernel_gs_base: crate::smp::percpu_base(cpu),
        saved: SavedState { rsp, _pad: 0, fx: [0; 512] },
        stack: Some(stack),
        entry: Some(Box::new(f)),
        cr3,
    });
    // Start from this CPU's current FPU/SSE control state (MXCSR, x87 CW).
    unsafe {
        core::arch::asm!("fxsave64 [{}]", in(reg) thread.saved.fx.as_mut_ptr(), options(nostack));
    }

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

/// Record that the running thread now uses the page tables at `cr3` (0 = the
/// kernel's). The caller loads CR3 itself.
pub fn set_current_cr3(cr3: u64) {
    let cpu = crate::smp::cpu_index();
    without_interrupts(|| {
        let mut ct = CPUS[cpu].lock();
        ct.ensure_boot_thread();
        let cur = ct.current;
        ct.threads[cur].cr3 = cr3;
    });
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

/// The running thread's user state. The pointer stays valid while the
/// thread exists (threads are boxed); only that thread itself uses it.
pub fn current_user() -> *mut UserCtx {
    let cpu = crate::smp::cpu_index();
    without_interrupts(|| {
        let mut ct = CPUS[cpu].lock();
        ct.ensure_boot_thread();
        let cur = ct.current;
        &mut ct.threads[cur].user as *mut UserCtx
    })
}

/// The process the running thread belongs to (0: a kernel thread).
pub fn current_pid() -> u32 {
    unsafe { (*current_user()).pid }
}

/// Set the running thread's kernel stack for syscalls and ring-3 interrupts,
/// and install it now.
pub fn set_kernel_entry_stack(top: u64) {
    let cpu = crate::smp::cpu_index();
    without_interrupts(|| {
        unsafe { (*current_user()).kernel_rsp = top };
        crate::gdt::set_rsp0(cpu, top);
        crate::smp::set_syscall_stack(cpu, top);
    });
}

/// Ask every thread of process `pid`, on any CPU, to end it.
pub fn kill_pid(pid: u32) {
    for cpu in CPUS.iter() {
        without_interrupts(|| {
            for t in cpu.lock().threads.iter_mut() {
                if t.user.pid == pid {
                    t.user.kill = true;
                }
            }
        });
    }
}

/// True (once) if the running thread has been asked to end its process.
pub fn take_kill() -> bool {
    let u = current_user();
    unsafe { core::mem::replace(&mut (*u).kill, false) }
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
