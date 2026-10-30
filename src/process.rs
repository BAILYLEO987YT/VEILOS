use crate::subsystem::Subsystem;
use crate::policy::PolicyMode;
use crate::nt_objects::HandleTable;
use crate::nt_memory::VadSpace;
use crate::capability::CapabilitySet;
use crate::sandbox::SyscallFilter;
use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec::Vec;
use spin::Mutex;
use core::sync::atomic::{AtomicU32, Ordering};

pub struct Registers {
    pub rax: u64, pub rbx: u64, pub rcx: u64, pub rdx: u64,
    pub rsi: u64, pub rdi: u64, pub rbp: u64, pub r8:  u64,
    pub r9:  u64, pub r10: u64, pub r11: u64, pub r12: u64,
    pub r13: u64, pub r14: u64, pub r15: u64,
    pub rip: u64, pub rsp: u64, pub rflags: u64,
}

impl Registers {
    pub const fn zeroed() -> Self {
        Self {
            rax: 0, rbx: 0, rcx: 0, rdx: 0,
            rsi: 0, rdi: 0, rbp: 0, r8:  0,
            r9:  0, r10: 0, r11: 0, r12: 0,
            r13: 0, r14: 0, r15: 0,
            rip: 0, rsp: 0, rflags: 0x202, // IF bit set
        }
    }
}

// =====================================================================
// -----| Process Control Block (PCB) |-----
// =====================================================================

pub struct Process {
    pub pid:         u32,
    pub name:        &'static str,
    pub subsystem:   Box<dyn Subsystem>,
    pub policy_mode: PolicyMode,

    /// Per-process handle table (NT kernel objects).
    pub handles: Mutex<HandleTable>,
    /// Virtual Address Descriptor space (NT memory manager).
    pub vad: Mutex<VadSpace>,
    /// Capability set — defines what this process is allowed to do.
    pub capabilities: Mutex<CapabilitySet>,
    /// Syscall filter — per-syscall allow/deny/audit rules.
    pub syscall_filter: Mutex<SyscallFilter>,

    // -----| Hardware Context |-----
    pub cr3:       u64,
    pub entry_point: u64,
    pub stack_top: u64,
    /// Low address of the user stack (stack_top is the launch RSP).
    pub stack_base: u64,
    /// Where this image was actually mapped (may differ from the PE's
    /// preferred image base — see the rebasing in loader.rs).
    pub image_base: u64,
    pub registers: Mutex<Registers>,
    pub mapped_pages: Mutex<Vec<u64>>,
    /// Per-process NT environment (TEB/PEB/_initterm trampolines).
    /// These used to be `static mut` globals shared by every process.
    pub nt_env: Mutex<crate::loader::NtEnv>,
    /// "dll!name" of each import nothing provided (modules.rs), and whether
    /// a call to it has been logged yet.
    pub unresolved_imports: Mutex<Vec<(alloc::string::String, bool)>>,
    /// The console of a console-subsystem program (its std handles refer to it).
    pub console: Option<alloc::sync::Arc<crate::console::Console>>,

    // -----| Lifecycle |-----
    pub exit_code: Mutex<Option<i32>>,
}

impl Process {
    pub fn new(pid: u32, name: &'static str, subsystem: Box<dyn Subsystem>) -> Self {
        // The loader builds every process with its new address space active
        // (address_space::with), so the active CR3 is the process's own.
        use x86_64::registers::control::Cr3;
        let (l4_frame, _) = Cr3::read();

        // Default: Win32 capabilities + permissive syscall filter.
        let caps   = CapabilitySet::new(crate::capability::CAP_WIN32);
        let filter = SyscallFilter::permissive();

        Process {
            pid,
            name,
            subsystem,
            policy_mode: crate::policy::POLICY.lock().get_mode(),
            handles:        Mutex::new(HandleTable::new()),
            vad:            Mutex::new(VadSpace::new()),
            capabilities:   Mutex::new(caps),
            syscall_filter: Mutex::new(filter),
            cr3:         l4_frame.start_address().as_u64(),
            entry_point: 0,
            stack_top:   0,
            stack_base:  0,
            image_base:  0,
            registers:   Mutex::new(Registers::zeroed()),
            mapped_pages: Mutex::new(alloc::vec::Vec::new()),
            nt_env:      Mutex::new(crate::loader::NtEnv::default()),
            unresolved_imports: Mutex::new(alloc::vec::Vec::new()),
            console:     None,
            exit_code:   Mutex::new(None),
        }
    }

    /// Run a syscall through the filter, then the subsystem handler.
    pub fn handle_syscall(&self, id: u64, a0: u64, a1: u64, a2: u64, a3: u64) -> u64 {
        use crate::sandbox::FilterAction;
        use crate::nt_objects::STATUS_ACCESS_DENIED;

        let action = self.syscall_filter.lock().evaluate(id);
        match action {
            FilterAction::Deny  => return STATUS_ACCESS_DENIED,
            FilterAction::Kill  => {
                self.terminate(-1);
                return STATUS_ACCESS_DENIED;
            }
            FilterAction::Audit => {
                crate::sandbox::audit(self.pid, 0, 0, 0,
                    crate::sandbox::AuditAction::Send, id);
            }
            FilterAction::Allow => {}
        }
        self.subsystem.handle_syscall(id, a0, a1, a2, a3)
    }

    pub fn is_alive(&self) -> bool { self.exit_code.lock().is_none() }
    pub fn terminate(&self, code: i32) { *self.exit_code.lock() = Some(code); }
}

// =====================================================================
// -----| Process Manager |-----
// =====================================================================

pub struct ProcessManager {
    pub processes:   Vec<Arc<Process>>,
    pub current_pid: Option<u32>,
    next_pid:        u32,
}

impl ProcessManager {
    pub const fn new() -> Self {
        Self { processes: Vec::new(), current_pid: None, next_pid: 1 }
    }

    /// Register the kernel/init process (PID 0) with full capabilities.
    pub fn register_kernel(&mut self) {
        use alloc::sync::Arc;
        use crate::capability::CAP_KERNEL;
        use crate::subsystem::NoopSubsystem;
        
        let kernel_process = Arc::new(Process {
            pid: 0,
            name: "kernel",
            subsystem: Box::new(NoopSubsystem),
            policy_mode: crate::policy::POLICY.lock().get_mode(),
            handles: Mutex::new(HandleTable::new()),
            vad: Mutex::new(VadSpace::new()),
            capabilities: Mutex::new(CapabilitySet::new(CAP_KERNEL)),
            syscall_filter: Mutex::new(SyscallFilter::permissive()),
            cr3: 0,
            entry_point: 0,
            stack_top: 0,
            stack_base: 0,
            image_base: 0,
            registers: Mutex::new(Registers::zeroed()),
            mapped_pages: Mutex::new(alloc::vec::Vec::new()),
            nt_env: Mutex::new(crate::loader::NtEnv::default()),
            unresolved_imports: Mutex::new(alloc::vec::Vec::new()),
            console: None,
            exit_code: Mutex::new(None),
        });
        self.processes.push(kernel_process);
        crate::println!("[PROC] Kernel process (PID 0) registered with CAP_KERNEL");
    }

    pub fn alloc_pid(&mut self) -> u32 {
        let pid = self.next_pid;
        self.next_pid += 1;
        pid
    }

    pub fn register(&mut self, process: Arc<Process>) -> u32 {
        let pid = process.pid;
        self.processes.push(process);
        pid
    }

    pub fn set_active(&mut self, process: Arc<Process>) {
        let pid = process.pid;
        if !self.processes.iter().any(|p| p.pid == pid) {
            self.processes.push(process);
        }
        self.current_pid = Some(pid);
    }

    pub fn get_active(&self) -> Option<Arc<Process>> {
        let pid = self.current_pid?;
        self.find(pid)
    }

    pub fn find(&self, pid: u32) -> Option<Arc<Process>> {
        self.processes.iter().find(|p| p.pid == pid).cloned()
    }

    pub fn reap_zombies(&mut self) {
        // Revoke all tokens for terminated processes before removing them.
        for p in self.processes.iter().filter(|p| !p.is_alive()) {
            crate::capability::TOKEN_REGISTRY.lock().revoke_all_for(p.pid);

            // Drop any worker threads still attributed to this pid, otherwise
            // THREAD_MANAGER keeps handing stale tids to the Win32 layer.
            if let Some(mut tm) = THREAD_MANAGER.try_lock() {
                tm.remove_threads_for_pid(p.pid);
            }

            // Close NT handles so the object manager can drop refcounts.
            if let Some(mut h) = p.handles.try_lock() { h.close_all(); }

            // Pages and page tables are released by loader::on_process_exit,
            // which runs on the process's own CPU with its CR3 still active.
            // This reaper runs on the kernel's page tables, where the
            // process's pages were never mapped.
        }
        self.processes.retain(|p| p.is_alive());
        if let Some(pid) = self.current_pid {
            if self.find(pid).is_none() { self.current_pid = None; }
        }
    }

    pub fn live_count(&self) -> usize {
        self.processes.iter().filter(|p| p.is_alive()).count()
    }
}

pub static PROCESSOR: Mutex<ProcessManager> = Mutex::new(ProcessManager::new());

// =====================================================================
// -----| Worker Thread Model |-----
// =====================================================================
//
// A WorkerThread is a first-class kernel object distinct from a Process.
// It represents a unit of parallelisable work that the OffloadBroker may
// elect to dispatch to a remote mesh node, subject to:
//   - is_native == true  : has MeshOffload capability; full offload path
//                          including checkpointing and retry.
//   - is_native == false : Win32 subsystem thread; best-effort offload
//                          only — no checkpoint, no retry beyond notification.
//   - offload_eligible   : set at spawn time and re-evaluated when the
//                          subsystem is Win32 (see ThreadManager::spawn_worker).

/// Which subsystem owns this thread, used to gate offload behaviour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ThreadSubsystem {
    /// Native VEILOS thread — designed for offloading from the ground up.
    Native = 0,
    /// Win32 subsystem thread — best-effort offload only.
    Win32 = 1,
}

/// The lifecycle state of a worker thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ThreadState {
    /// Waiting to be scheduled.
    Ready = 0,
    /// Currently executing (locally or on a remote node).
    Running = 1,
    /// Paused at a step boundary; a checkpoint may exist.
    Suspended = 2,
    /// Offloaded to a remote node; awaiting result.
    Offloaded = 3,
    /// Execution complete; result available.
    Finished = 4,
    /// Terminated with error.
    Failed = 5,
}

/// A worker thread — an independently schedulable unit of work owned by
/// a parent process.  Multiple workers can be spawned from the same process
/// and are eligible for mesh offloading based on the flags below.
pub struct WorkerThread {
    /// Unique thread ID across the whole system.
    pub thread_id:          u32,
    /// PID of the owning process.
    pub owner_pid:          u32,
    /// Human-readable name, null-padded to 32 bytes.
    pub name:               [u8; 32],
    /// Whether this thread may be offloaded to a remote mesh node.
    /// For Win32 threads this is evaluated at spawn; for native threads
    /// it is always true unless explicitly cleared.
    pub offload_eligible:   bool,
    /// Which subsystem created this thread.
    pub subsystem:          ThreadSubsystem,
    /// Entry-point offset inside the owning process's virtual address space.
    pub entry_vram_offset:  u64,
    /// Stack size in bytes (rounded up to page boundary by the caller).
    pub stack_size:         u32,
    /// Current lifecycle state.
    pub state:              ThreadState,
    /// Capability requirements the remote node must satisfy to accept this thread.
    /// Encoded as a bitfield matching the distributed::CAP_* constants.
    pub required_node_caps: u32,
    /// The current step index (incremented as the thread completes steps and
    /// the CheckpointManager writes a checkpoint).
    pub step_index:         u32,
    /// Job ID assigned by the OffloadBroker when this thread is in flight.
    /// Zero means the thread has not been submitted to the broker yet.
    pub job_id:             u64,
}

impl WorkerThread {
    /// Returns true if this thread is a candidate for mesh offloading.
    /// For native threads: always true (they were designed for it).
    /// For Win32 threads: true only when IpcSharedMemory is NOT in
    /// `required_node_caps` — shared-memory semantics can't cross a
    /// network boundary safely (see Task 7).
    pub fn is_offload_candidate(&self) -> bool {
        if !self.offload_eligible { return false; }
        if self.subsystem == ThreadSubsystem::Win32 {
            // IpcSharedMemory (bit 10 in the process capability space) maps to
            // the node's resource-sharing model; a Win32 thread that relies on
            // shared memory sections is ineligible for cross-node offload.
            const SHARED_MEM_BIT: u32 = 1 << 10;
            return self.required_node_caps & SHARED_MEM_BIT == 0;
        }
        true
    }

    /// Name as a UTF-8 string slice (trims trailing NULs).
    pub fn name_str(&self) -> &str {
        let end = self.name.iter().position(|&b| b == 0).unwrap_or(32);
        core::str::from_utf8(&self.name[..end]).unwrap_or("?")
    }
}

// =====================================================================
// -----| Thread Manager |-----
// =====================================================================

static NEXT_THREAD_ID: AtomicU32 = AtomicU32::new(1);

pub struct ThreadManager {
    threads: Vec<WorkerThread>,
}

impl ThreadManager {
    pub const fn new() -> Self {
        Self { threads: Vec::new() }
    }

    /// Spawn a new worker thread for `owner_pid`.
    ///
    /// `name`            — human-readable label (truncated to 31 chars).
    /// `subsystem`       — Native or Win32.
    /// `entry`           — entry-point VA offset.
    /// `stack_size`      — requested stack in bytes.
    /// `node_caps`       — node capability requirements (distributed::CAP_*).
    ///
    /// Offload eligibility rules:
    ///   Native threads: eligible by default.
    ///   Win32 threads:  eligible only when `node_caps` does not include
    ///                   the IpcSharedMemory bit (0x400).
    pub fn spawn_worker(
        &mut self,
        owner_pid:   u32,
        name:        &str,
        subsystem:   ThreadSubsystem,
        entry:       u64,
        stack_size:  u32,
        node_caps:   u32,
    ) -> u32 {
        let tid = NEXT_THREAD_ID.fetch_add(1, Ordering::Relaxed);

        let mut name_buf = [0u8; 32];
        for (i, b) in name.bytes().take(31).enumerate() {
            name_buf[i] = b;
        }

        // Win32: disqualify threads that need IpcSharedMemory (bit 10).
        const SHARED_MEM_BIT: u32 = 1 << 10;
        let eligible = match subsystem {
            ThreadSubsystem::Native => true,
            ThreadSubsystem::Win32  => node_caps & SHARED_MEM_BIT == 0,
        };

        self.threads.push(WorkerThread {
            thread_id:         tid,
            owner_pid,
            name:              name_buf,
            offload_eligible:  eligible,
            subsystem,
            entry_vram_offset: entry,
            stack_size,
            state:             ThreadState::Ready,
            required_node_caps: node_caps,
            step_index:        0,
            job_id:            0,
        });

        crate::println!(
            "[THREAD] Spawned tid={} owner={} name={} sub={:?} eligible={}",
            tid, owner_pid, name, subsystem, eligible
        );
        tid
    }

    /// Find a thread by its ID.
    pub fn find_thread(&self, tid: u32) -> Option<&WorkerThread> {
        self.threads.iter().find(|t| t.thread_id == tid)
    }

    /// Find a thread mutably by its ID.
    pub fn find_thread_mut(&mut self, tid: u32) -> Option<&mut WorkerThread> {
        self.threads.iter_mut().find(|t| t.thread_id == tid)
    }

    /// Mark a thread as offload-eligible (or not) after spawn.
    pub fn mark_offload_eligible(&mut self, tid: u32, eligible: bool) {
        if let Some(t) = self.find_thread_mut(tid) {
            t.offload_eligible = eligible;
        }
    }

    /// Remove all finished/failed threads (called periodically by the scheduler).
    pub fn reap_finished(&mut self) {
        self.threads.retain(|t| {
            let dead = matches!(t.state, ThreadState::Finished | ThreadState::Failed);
            if dead && t.subsystem == ThreadSubsystem::Win32 {
                // try_lock: a syscall handler on another CPU may hold
                // WIN32_USER; skip the cleanup rather than stall the reaper.
                if let Some(mut w) = crate::win32_user::WIN32_USER.try_lock() {
                    w.cleanup_thread(t.thread_id);
                }
            }
            !dead
        });
    }

    /// Remove every thread belonging to `pid` (called on process teardown).
    ///
    /// Without this, a closed process's threads stayed in the table forever
    /// and `threads_for_pid()` kept returning tids the Win32 layer then used
    /// to look up windows that no longer existed.
    pub fn remove_threads_for_pid(&mut self, pid: u32) {
        let mut removed = 0usize;
        self.threads.retain(|t| {
            if t.owner_pid == pid {
                if t.subsystem == ThreadSubsystem::Win32 {
                    if let Some(mut w) = crate::win32_user::WIN32_USER.try_lock() {
                        w.cleanup_thread(t.thread_id);
                    }
                }
                removed += 1;
                false
            } else {
                true
            }
        });
        if removed > 0 {
            crate::serial_println!("[THREAD] Removed {} thread(s) for pid {}", removed, pid);
        }
    }

    /// All live threads for a given PID.
    pub fn threads_for_pid(&self, pid: u32) -> impl Iterator<Item = &WorkerThread> {
        self.threads.iter().filter(move |t| t.owner_pid == pid)
    }

    /// All offload-eligible threads across all processes.
    pub fn eligible_threads(&self) -> impl Iterator<Item = &WorkerThread> {
        self.threads.iter().filter(|t| t.is_offload_candidate())
    }

    pub fn thread_count(&self) -> usize { self.threads.len() }
}

pub static THREAD_MANAGER: Mutex<ThreadManager> = Mutex::new(ThreadManager::new());
