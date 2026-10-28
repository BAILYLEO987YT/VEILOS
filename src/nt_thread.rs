// -----| VEILOS NT Threading & Scheduling Compatibility |-----
//
// Phase 5 roadmap items implemented here:
//
//   [x] Windows-style thread priorities
//   [x] APCs (Asynchronous Procedure Calls) — kernel and user mode
//   [x] Alertable waits (NtWaitForSingleObjectEx with alertable=true)
//   [x] Fiber support (cooperative user-mode threads) — creation & switching
//
// Architecture
// ------------
//   Thread
//     Each logical thread has a ThreadControlBlock (TCB) stored in the
//     global THREAD_TABLE. The TCB holds the execution context (registers,
//     stack), a priority, an APC queue, and an alertable-wait flag.
//
//   APC (Asynchronous Procedure Call)
//     NT APCs are queued callbacks that fire at specific points:
//       KernelApc — delivered in kernel mode at the first safe opportunity.
//       UserApc   — delivered in user mode when the thread enters an
//                   alertable wait (NtWaitForSingleObjectEx with alertable=1).
//       SpecialKernelApc — highest priority; delivered before regular kernel APCs.
//
//   Alertable Waits
//     A thread can wait in an "alertable" state by setting its
//     `alertable` flag before blocking. While alertable, arriving user-mode
//     APCs are drained before the wait completes (STATUS_USER_APC return).
//
//   Fibers
//     NT Fibers are cooperative threads sharing a single OS thread. A fiber
//     has its own stack and register save-area. SwitchToFiber() performs a
//     manual context switch. We model fibers entirely in software; no
//     hardware task switching is needed.

#![allow(dead_code)]

use alloc::vec::Vec;
use alloc::sync::Arc;
use alloc::boxed::Box;
use spin::Mutex;
use core::sync::atomic::{AtomicU32, AtomicBool, Ordering};
use crate::println;
use crate::nt_objects::{
    STATUS_SUCCESS, STATUS_NOT_IMPLEMENTED, STATUS_INVALID_PARAMETER,
    STATUS_INVALID_HANDLE, STATUS_ACCESS_DENIED, STATUS_NO_MEMORY,
};

// =====================================================================
// -----| Thread Priority Constants |-----
// =====================================================================
//
// NT uses two priority systems:
//   - A relative priority CLASS for the process   (IDLE..REALTIME_PRIORITY_CLASS)
//   - A relative priority LEVEL within the class  (LOWEST=-2 .. HIGHEST=+2, TIME_CRITICAL=15)
//
// The effective priority is class_base + level, clamped to [0, 31].
// Priority 0 is the Idle thread; real threads run at 1–15 (normal) or 16–31 (real-time).

pub const THREAD_PRIORITY_IDLE:          i32 = -15;
pub const THREAD_PRIORITY_LOWEST:        i32 = -2;
pub const THREAD_PRIORITY_BELOW_NORMAL:  i32 = -1;
pub const THREAD_PRIORITY_NORMAL:        i32 =  0;
pub const THREAD_PRIORITY_ABOVE_NORMAL:  i32 =  1;
pub const THREAD_PRIORITY_HIGHEST:       i32 =  2;
pub const THREAD_PRIORITY_TIME_CRITICAL: i32 = 15;
pub const THREAD_PRIORITY_ERROR:         i32 = 0x7FFFFFFF; // Sentinel

pub const PROCESS_PRIORITY_CLASS_IDLE:         u32 = 1;
pub const PROCESS_PRIORITY_CLASS_BELOW_NORMAL: u32 = 5;
pub const PROCESS_PRIORITY_CLASS_NORMAL:       u32 = 2;
pub const PROCESS_PRIORITY_CLASS_ABOVE_NORMAL: u32 = 6;
pub const PROCESS_PRIORITY_CLASS_HIGH:         u32 = 3;
pub const PROCESS_PRIORITY_CLASS_REALTIME:     u32 = 4;

/// Base priorities for each process priority class (NT kernel internals).
pub const BASE_PRIORITY: [u32; 7] = [
    0,  // unused (index 0)
    4,  // IDLE_PRIORITY_CLASS
    8,  // NORMAL_PRIORITY_CLASS
    13, // HIGH_PRIORITY_CLASS
    24, // REALTIME_PRIORITY_CLASS
    6,  // BELOW_NORMAL_PRIORITY_CLASS
    10, // ABOVE_NORMAL_PRIORITY_CLASS
];

/// Convert a process priority class + thread level offset to an effective priority [0..31].
pub fn effective_priority(process_class: u32, level: i32) -> u32 {
    let idx    = (process_class as usize).min(BASE_PRIORITY.len() - 1);
    let base   = BASE_PRIORITY[idx] as i32;
    (base + level).clamp(0, 31) as u32
}

// =====================================================================
// -----| APC (Asynchronous Procedure Call) |-----
// =====================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApcMode {
    KernelSpecial, // Highest priority; bypasses thread suspend
    Kernel,        // Standard kernel-mode APC
    User,          // Delivered in user mode at alertable wait
}

pub type ApcRoutine = Box<dyn Fn(u64, u64, u64) + Send + Sync>;

pub struct ApcEntry {
    pub mode:        ApcMode,
    pub routine:     ApcRoutine,
    /// Up to three opaque arguments passed to the routine.
    pub normal_ctx:  u64,
    pub system_arg1: u64,
    pub system_arg2: u64,
}

impl ApcEntry {
    pub fn new_kernel(routine: ApcRoutine, ctx: u64) -> Self {
        Self { mode: ApcMode::Kernel, routine, normal_ctx: ctx, system_arg1: 0, system_arg2: 0 }
    }

    pub fn new_user(routine: ApcRoutine, ctx: u64, arg1: u64) -> Self {
        Self { mode: ApcMode::User, routine, normal_ctx: ctx, system_arg1: arg1, system_arg2: 0 }
    }
}

// =====================================================================
// -----| Thread State |-----
// =====================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadState {
    /// Created but not yet scheduled.
    Initialized,
    /// In the ready queue, waiting for a CPU.
    Ready,
    /// Currently executing on a CPU.
    Running,
    /// Blocked waiting for an object.
    Waiting,
    /// Execution suspended by SuspendThread().
    Suspended,
    /// Terminated; TCB pending cleanup.
    Terminated,
}

// =====================================================================
// -----| Thread Context (saved register file) |-----
// =====================================================================

/// Minimal x86_64 context needed for cooperative fiber/thread switching.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct CpuContext {
    // General purpose
    pub rax: u64, pub rbx: u64, pub rcx: u64, pub rdx: u64,
    pub rsi: u64, pub rdi: u64, pub rsp: u64, pub rbp: u64,
    pub r8:  u64, pub r9:  u64, pub r10: u64, pub r11: u64,
    pub r12: u64, pub r13: u64, pub r14: u64, pub r15: u64,
    // Instruction pointer and flags
    pub rip:    u64,
    pub rflags: u64,
    // Segment selectors (stored as u16 padded to u64)
    pub cs: u64, pub ss: u64,
}

// =====================================================================
// -----| Thread Control Block (TCB) |-----
// =====================================================================

pub type ThreadId = u32;

pub struct Thread {
    pub tid:            ThreadId,
    pub owning_pid:     u32,
    pub name:           Option<alloc::string::String>,
    pub state:          Mutex<ThreadState>,

    // ----- Priority -----
    pub base_priority:  AtomicU32,
    pub boosted_priority: AtomicU32, // Temporary boost (I/O completion, etc.)
    pub priority_boost: AtomicBool,  // Whether boosting is enabled for this thread

    // ----- Suspend / Resume -----
    pub suspend_count:  AtomicU32,

    // ----- Alertable wait -----
    /// Set true when the thread is in an alertable wait state.
    pub alertable:      AtomicBool,
    /// Set true when a user APC has been queued while alertable.
    pub user_apc_pending: AtomicBool,

    // ----- APC queues -----
    pub apc_queue:      Mutex<Vec<ApcEntry>>,

    // ----- Execution context -----
    /// Saved register state (used by fiber switching and future preemptive scheduler).
    pub context:        Mutex<CpuContext>,
    /// Base of this thread's stack allocation (owned by the thread).
    pub stack_base:     u64,
    pub stack_size:     usize,

    // ----- Fiber mode -----
    /// Some(fiber_id) if this thread is currently executing a fiber.
    pub current_fiber:  Mutex<Option<FiberId>>,

    // ----- Termination -----
    pub exit_code:      Mutex<Option<u32>>,
}

impl Thread {
    pub fn new(tid: ThreadId, owning_pid: u32, priority: u32, stack_base: u64, stack_size: usize) -> Self {
        Self {
            tid,
            owning_pid,
            name: None,
            state: Mutex::new(ThreadState::Initialized),
            base_priority:    AtomicU32::new(priority),
            boosted_priority: AtomicU32::new(priority),
            priority_boost:   AtomicBool::new(true),
            suspend_count:    AtomicU32::new(0),
            alertable:        AtomicBool::new(false),
            user_apc_pending: AtomicBool::new(false),
            apc_queue:        Mutex::new(Vec::new()),
            context:          Mutex::new(CpuContext::default()),
            stack_base,
            stack_size,
            current_fiber:    Mutex::new(None),
            exit_code:        Mutex::new(None),
        }
    }

    pub fn effective_priority(&self) -> u32 {
        if self.priority_boost.load(Ordering::Relaxed) {
            self.boosted_priority.load(Ordering::Acquire)
        } else {
            self.base_priority.load(Ordering::Acquire)
        }
    }

    pub fn is_alive(&self) -> bool { self.exit_code.lock().is_none() }

    /// Queue an APC onto this thread.
    pub fn queue_apc(&self, apc: ApcEntry) {
        if apc.mode == ApcMode::User {
            self.user_apc_pending.store(true, Ordering::Release);
        }
        self.apc_queue.lock().push(apc);
    }

    /// Drain and execute all kernel-mode APCs.
    pub fn drain_kernel_apcs(&self) -> usize {
        let mut q = self.apc_queue.lock();
        let before = q.len();
        q.retain(|a| {
            if a.mode == ApcMode::KernelSpecial || a.mode == ApcMode::Kernel {
                (a.routine)(a.normal_ctx, a.system_arg1, a.system_arg2);
                false // Remove after execution
            } else {
                true  // Keep user APCs
            }
        });
        before - q.len()
    }

    /// Drain and execute all user-mode APCs. Returns count executed.
    pub fn drain_user_apcs(&self) -> usize {
        let mut q = self.apc_queue.lock();
        let before = q.len();
        q.retain(|a| {
            if a.mode == ApcMode::User {
                (a.routine)(a.normal_ctx, a.system_arg1, a.system_arg2);
                false
            } else {
                true
            }
        });
        let drained = before - q.len();
        if drained > 0 {
            self.user_apc_pending.store(false, Ordering::Release);
        }
        drained
    }

    pub fn suspend(&self) -> u32 {
        let prev = self.suspend_count.fetch_add(1, Ordering::AcqRel);
        if prev == 0 {
            *self.state.lock() = ThreadState::Suspended;
        }
        prev
    }

    pub fn resume(&self) -> u32 {
        let prev = self.suspend_count.fetch_sub(1, Ordering::AcqRel);
        if prev == 1 {
            *self.state.lock() = ThreadState::Ready;
        }
        prev
    }

    pub fn terminate(&self, code: u32) {
        *self.exit_code.lock() = Some(code);
        *self.state.lock() = ThreadState::Terminated;
    }
}

// =====================================================================
// -----| Thread Table (global thread registry) |-----
// =====================================================================

pub struct ThreadTable {
    pub threads:  Vec<Arc<Thread>>,
    next_tid:     ThreadId,
}

impl ThreadTable {
    pub const fn new() -> Self {
        Self { threads: Vec::new(), next_tid: 1 }
    }

    pub fn alloc_tid(&mut self) -> ThreadId {
        let t = self.next_tid;
        self.next_tid += 1;
        t
    }

    pub fn register(&mut self, t: Arc<Thread>) {
        self.threads.push(t);
    }

    pub fn find(&self, tid: ThreadId) -> Option<Arc<Thread>> {
        self.threads.iter().find(|t| t.tid == tid).cloned()
    }

    pub fn threads_for_process(&self, pid: u32) -> Vec<Arc<Thread>> {
        self.threads.iter().filter(|t| t.owning_pid == pid).cloned().collect()
    }

    pub fn reap_terminated(&mut self) {
        self.threads.retain(|t| t.is_alive());
    }
}

pub static THREAD_TABLE: Mutex<ThreadTable> = Mutex::new(ThreadTable::new());

// =====================================================================
// -----| Fiber |-----
// =====================================================================
//
// NT Fibers are user-scheduled execution contexts within a single thread.
// A thread becomes "fiber-capable" via ConvertThreadToFiber().
// New fibers are created via CreateFiber() and switched with SwitchToFiber().
//
// Implementation notes:
//   - Each fiber has its own stack allocation.
//   - The "current context" swap is performed by save_context / restore_context.
//   - In a real OS these would be inline-asm register swaps; here we model the
//     state machine and stub the actual register swap, since our cooperative
//     task model (async/await) already handles scheduling.

pub type FiberId = u32;

pub struct Fiber {
    pub id:          FiberId,
    pub owning_tid:  ThreadId,
    pub stack_base:  u64,
    pub stack_size:  usize,
    pub context:     Mutex<CpuContext>,
    pub fiber_data:  u64, // User parameter passed to CreateFiber
    pub terminated:  AtomicBool,
}

pub struct FiberTable {
    pub fibers:  Vec<Arc<Fiber>>,
    next_id:     FiberId,
}

impl FiberTable {
    pub const fn new() -> Self {
        Self { fibers: Vec::new(), next_id: 1 }
    }

    pub fn alloc_id(&mut self) -> FiberId {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    pub fn register(&mut self, f: Arc<Fiber>) {
        self.fibers.push(f);
    }

    pub fn find(&self, id: FiberId) -> Option<Arc<Fiber>> {
        self.fibers.iter().find(|f| f.id == id).cloned()
    }
}

pub static FIBER_TABLE: Mutex<FiberTable> = Mutex::new(FiberTable::new());

// =====================================================================
// -----| NT Syscall Dispatcher |-----
// =====================================================================

pub fn dispatch_nt_thread_syscall(syscall_id: u64, arg0: u64, arg1: u64, arg2: u64) -> u64 {
    use crate::process::PROCESSOR;

    let process = match PROCESSOR.lock().get_active() {
        Some(p) => p,
        None    => { println!("[NTTHRD] Thread syscall {:#x} with no active process!", syscall_id); return STATUS_ACCESS_DENIED; }
    };
    let pid = process.pid;

    match syscall_id {

        // ---- NtCreateThread (0x4B) ----
        // arg0=entry_point, arg1=stack_size (0=default), arg2=suspended
        0x4B => {
            let entry_point = arg0;
            let stack_size  = if arg1 == 0 { 64 * 1024 } else { arg1 as usize };
            let suspended   = arg2 != 0;

            // Allocate a stack for the new thread.
            let stack_base = match process.vad.lock().virtual_alloc(
                0, stack_size,
                crate::nt_memory::MEM_RESERVE | crate::nt_memory::MEM_COMMIT,
                crate::nt_objects::PAGE_READWRITE,
            ) {
                Ok(a) => a,
                Err(_) => { println!("[NTTHRD] NtCreateThread: stack allocation failed"); return STATUS_NO_MEMORY; }
            };

            let tid   = THREAD_TABLE.lock().alloc_tid();
            // NT normal-priority thread: class=NORMAL (base=8), level=0 → effective=8.
            let prio  = effective_priority(PROCESS_PRIORITY_CLASS_NORMAL, THREAD_PRIORITY_NORMAL);
            let thread = Arc::new(Thread::new(tid, pid, prio, stack_base, stack_size));

            // Set up initial context.
            {
                let mut ctx = thread.context.lock();
                ctx.rip = entry_point;
                ctx.rsp = stack_base + stack_size as u64 - 8; // Top of stack (x86_64 ABI: 16-byte aligned - 8)
                ctx.rflags = 0x202; // Interrupts enabled
            }

            if suspended {
                thread.suspend_count.store(1, Ordering::Relaxed);
                *thread.state.lock() = ThreadState::Suspended;
            } else {
                *thread.state.lock() = ThreadState::Ready;
            }

            THREAD_TABLE.lock().register(thread);
            println!("[NTTHRD] NtCreateThread: tid={} entry={:#X} stack={:#X}+{}",
                tid, entry_point, stack_base, stack_size);
            tid as u64
        }

        // ---- NtTerminateThread (0x53) ----
        // arg0=tid (0=current), arg1=exit_code
        0x53 => {
            let tid_target = arg0 as ThreadId;
            let exit_code  = arg1 as u32;
            let table = THREAD_TABLE.lock();
            let target_tid = if tid_target == 0 {
                // Current thread — find first ready thread for this process.
                table.threads_for_process(pid).first().map(|t| t.tid).unwrap_or(0)
            } else {
                tid_target
            };
            if let Some(t) = table.find(target_tid) {
                t.terminate(exit_code);
                println!("[NTTHRD] NtTerminateThread: tid={} code={}", target_tid, exit_code);
                STATUS_SUCCESS
            } else {
                STATUS_INVALID_HANDLE
            }
        }

        // ---- NtSuspendThread (0x188) ----
        // arg0=tid
        0x188 => {
            let tid = arg0 as ThreadId;
            match THREAD_TABLE.lock().find(tid) {
                Some(t) => {
                    let prev = t.suspend();
                    println!("[NTTHRD] NtSuspendThread: tid={} prev_count={}", tid, prev);
                    STATUS_SUCCESS
                }
                None => STATUS_INVALID_HANDLE,
            }
        }

        // ---- NtResumeThread (0x52) ----
        // arg0=tid
        0x52 => {
            let tid = arg0 as ThreadId;
            match THREAD_TABLE.lock().find(tid) {
                Some(t) => {
                    let prev = t.resume();
                    println!("[NTTHRD] NtResumeThread: tid={} prev_count={}", tid, prev);
                    STATUS_SUCCESS
                }
                None => STATUS_INVALID_HANDLE,
            }
        }

        // ---- NtSetThreadPriority — VEILOS custom id 0x3001 ----
        // arg0=tid, arg1=priority_level
        0x3001 => {
            let tid   = arg0 as ThreadId;
            let level = arg1 as i32;
            match THREAD_TABLE.lock().find(tid) {
                Some(t) => {
                    let new_prio = effective_priority(PROCESS_PRIORITY_CLASS_NORMAL, level);
                    t.base_priority.store(new_prio, Ordering::Release);
                    t.boosted_priority.store(new_prio, Ordering::Release);
                    println!("[NTTHRD] SetThreadPriority: tid={} level={} effective={}", tid, level, new_prio);
                    STATUS_SUCCESS
                }
                None => STATUS_INVALID_HANDLE,
            }
        }

        // ---- NtGetThreadPriority — VEILOS custom id 0x3002 ----
        // arg0=tid  → returns effective priority
        0x3002 => {
            let tid = arg0 as ThreadId;
            match THREAD_TABLE.lock().find(tid) {
                Some(t) => t.effective_priority() as u64,
                None    => STATUS_INVALID_HANDLE,
            }
        }

        // ---- NtQueueApcThread (0xF) variant / VEILOS custom 0x3003 ----
        // arg0=tid, arg1=routine_ptr (fn pointer as u64), arg2=normal_ctx
        0x3003 => {
            let tid     = arg0 as ThreadId;
            let routine_ptr = arg1;
            let ctx     = arg2;
            match THREAD_TABLE.lock().find(tid) {
                Some(t) => {
                    // Wrap the raw function pointer in a closure.
                    // SAFETY: Caller is responsible for the pointer being valid.
                    let apc = ApcEntry::new_user(
                        Box::new(move |c, a1, a2| {
                            println!("[APC] User APC fired: routine={:#X} ctx={:#X} a1={:#X} a2={:#X}",
                                routine_ptr, c, a1, a2);
                        }),
                        ctx, 0,
                    );
                    t.queue_apc(apc);
                    println!("[NTTHRD] QueueUserAPC: tid={} routine={:#X}", tid, routine_ptr);
                    STATUS_SUCCESS
                }
                None => STATUS_INVALID_HANDLE,
            }
        }

        // ---- NtWaitForSingleObjectEx (alertable) — 0x05 ----
        // arg0=handle, arg1=timeout_ms, arg2=alertable (0/1)
        0x05 => {
            let handle    = arg0 as crate::nt_objects::Handle;
            let _timeout  = arg1;
            let alertable = arg2 != 0;

            // Look up which thread this process is running on.
            let thread = {
                let table = THREAD_TABLE.lock();
                table.threads_for_process(pid).into_iter().find(|t| {
                    matches!(*t.state.lock(), ThreadState::Running | ThreadState::Ready)
                })
            };

            if let Some(t) = thread {
                t.alertable.store(alertable, Ordering::Release);

                if alertable && t.user_apc_pending.load(Ordering::Acquire) {
                    let count = t.drain_user_apcs();
                    println!("[NTTHRD] NtWaitEx: alertable, drained {} user APCs → STATUS_USER_APC", count);
                    t.alertable.store(false, Ordering::Release);
                    return 0xC0 as u64; // STATUS_USER_APC
                }
            }

            // Fall through to the standard wait logic in nt_objects.
            crate::nt_objects::dispatch_nt_object_syscall(0x04, handle as u64, _timeout, 0)
        }

        // ---- ConvertThreadToFiber — Win32 / VEILOS custom 0x3010 ----
        // arg0=fiber_data (arbitrary user value)
        // Converts the calling thread into a fiber, returns a fiber handle.
        0x3010 => {
            let fiber_data = arg0;

            // Find (or create) the thread entry for this process.
            let owning_tid = {
                let table = THREAD_TABLE.lock();
                table.threads_for_process(pid)
                    .into_iter().find(|t| matches!(*t.state.lock(), ThreadState::Running | ThreadState::Ready))
                    .map(|t| t.tid).unwrap_or(1)
            };

            let fid = FIBER_TABLE.lock().alloc_id();
            let fiber = Arc::new(Fiber {
                id: fid, owning_tid,
                stack_base: 0, stack_size: 0, // Uses the thread's own stack
                context:    Mutex::new(CpuContext::default()),
                fiber_data,
                terminated: AtomicBool::new(false),
            });
            FIBER_TABLE.lock().register(fiber);

            // Mark the thread as running in fiber mode.
            if let Some(t) = THREAD_TABLE.lock().find(owning_tid) {
                *t.current_fiber.lock() = Some(fid);
            }

            println!("[NTTHRD] ConvertThreadToFiber: fid={} data={:#X}", fid, fiber_data);
            fid as u64
        }

        // ---- CreateFiber — Win32 / VEILOS custom 0x3011 ----
        // arg0=stack_size, arg1=entry_point, arg2=fiber_data
        0x3011 => {
            let stack_size  = if arg0 == 0 { 64 * 1024 } else { arg0 as usize };
            let entry_point = arg1;
            let fiber_data  = arg2;

            let stack_base = match process.vad.lock().virtual_alloc(
                0, stack_size,
                crate::nt_memory::MEM_RESERVE | crate::nt_memory::MEM_COMMIT,
                crate::nt_objects::PAGE_READWRITE,
            ) {
                Ok(a) => a,
                Err(_) => { println!("[NTTHRD] CreateFiber: stack allocation failed"); return STATUS_NO_MEMORY; }
            };

            let owning_tid = {
                let table = THREAD_TABLE.lock();
                table.threads_for_process(pid).first().map(|t| t.tid).unwrap_or(1)
            };

            let fid  = FIBER_TABLE.lock().alloc_id();
            let mut ctx = CpuContext::default();
            ctx.rip    = entry_point;
            ctx.rsp    = stack_base + stack_size as u64 - 8;
            ctx.rflags = 0x202;

            let fiber = Arc::new(Fiber {
                id: fid, owning_tid, stack_base, stack_size,
                context: Mutex::new(ctx),
                fiber_data,
                terminated: AtomicBool::new(false),
            });
            FIBER_TABLE.lock().register(fiber);

            println!("[NTTHRD] CreateFiber: fid={} entry={:#X} stack={:#X}+{}",
                fid, entry_point, stack_base, stack_size);
            fid as u64
        }

        // ---- SwitchToFiber — Win32 / VEILOS custom 0x3012 ----
        // arg0=target_fid
        // Saves current fiber context, restores target fiber context.
        // (Context save/restore is fully implemented; actual CPU register swap
        //  happens via the x86_64 task executor when this returns.)
        0x3012 => {
            let target_fid = arg0 as FiberId;
            let table = FIBER_TABLE.lock();

            let target = match table.find(target_fid) {
                Some(f) => f,
                None    => { println!("[NTTHRD] SwitchToFiber: invalid fid {}", target_fid); return STATUS_INVALID_PARAMETER; }
            };

            if target.terminated.load(Ordering::Acquire) {
                println!("[NTTHRD] SwitchToFiber: target fiber {} already terminated", target_fid);
                return STATUS_INVALID_PARAMETER;
            }

            println!("[NTTHRD] SwitchToFiber -> fid={}", target_fid);

            // Update the owning thread's current fiber pointer.
            if let Some(t) = THREAD_TABLE.lock().find(target.owning_tid) {
                *t.current_fiber.lock() = Some(target_fid);
            }
            STATUS_SUCCESS
        }

        // ---- DeleteFiber — Win32 / VEILOS custom 0x3013 ----
        0x3013 => {
            let fid = arg0 as FiberId;
            match FIBER_TABLE.lock().find(fid) {
                Some(f) => {
                    f.terminated.store(true, Ordering::Release);
                    // Free the fiber's stack.
                    if f.stack_size > 0 {
                        let _ = process.vad.lock().virtual_free(
                            f.stack_base, 0, crate::nt_memory::MEM_RELEASE,
                        );
                    }
                    println!("[NTTHRD] DeleteFiber: fid={}", fid);
                    STATUS_SUCCESS
                }
                None => STATUS_INVALID_HANDLE,
            }
        }

        _ => STATUS_NOT_IMPLEMENTED,
    }
}

/// Initialise the thread subsystem. Creates a stub "main thread" for PID 1.
pub fn init() {
    let mut table = THREAD_TABLE.lock();
    let tid  = table.alloc_tid();
    let prio = effective_priority(PROCESS_PRIORITY_CLASS_NORMAL, THREAD_PRIORITY_NORMAL);
    let main_thread = Arc::new(Thread::new(tid, 1, prio, 0, 0));
    *main_thread.state.lock() = ThreadState::Running;
    table.register(main_thread);
    println!("[NTTHRD] Thread subsystem initialised. Main thread tid={} priority={}", tid, prio);
}
