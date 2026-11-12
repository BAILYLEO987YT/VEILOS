// -----| VEILOS Adaptive Scheduler |-----
//
// Phase 11 roadmap items implemented here:
//
//   [x] Lock-free kernel queues       — PriorityQueues: one ArrayQueue per priority level
//   [x] Per-core scheduling domains   — SchedDomain: per-core affinity mask + ready queues
//   [x] Dynamic quantum scaling       — QuantumConfig: base quantum × per-priority multiplier
//                                       + Performance-mode bonus applied at runtime
//   [x] Heuristic/adaptive scheduler  — ProfileStore: usage history persisted to FAT32.
//                                       On launch the engine pre-selects priority and
//                                       quantum multiplier from past sessions. On exit it
//                                       updates the rolling averages and marks the store
//                                       dirty so the next flush writes to disk.
//
// Architecture
// ------------
//   Timer ISR (1 kHz)
//     └─ scheduler_tick(tick)          <- thin try_lock wrapper, never blocks
//          └─ SchedDomain::tick()      <- decrements quantum_remaining; sets QUANTUM_EXPIRED
//               └─ (heavy work skipped in ISR — handled on process launch/exit)
//
//   Kernel code-paths
//     AdaptiveScheduler::on_process_launch(pid, name, tick)
//       -> looks up AppProfile, enqueues pid at suggested priority,
//          arms quantum from suggested multiplier
//     AdaptiveScheduler::on_process_exit(pid, final_mem, tick)
//       -> finalises session stats, updates rolling averages in ProfileStore,
//          marks store dirty (flushed on next flush_profiles() call or shutdown)
//
// Quantum expiry
//   QUANTUM_EXPIRED is an AtomicBool set by the ISR. The async Executor
//   (and any future preemptive path) checks this after each task poll and
//   calls AdaptiveScheduler::handle_expiry(tick) to pick the next pid
//   and rearm. The actual register-level context switch is the next step;
//   for now we expose the full infrastructure.

#![allow(dead_code)]

use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use spin::Mutex;
use crossbeam_queue::ArrayQueue;
use lazy_static::lazy_static;
use core::sync::atomic::{AtomicBool, Ordering};

// =====================================================================
// -----| Part 1 - Priority & Affinity |-----
// =====================================================================

/// Five scheduling priority levels, highest to lowest.
/// Stored as u8 in AppProfile for serialisation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum SchedPriority {
    RealTime = 0,
    High     = 1,
    Normal   = 2,
    Low      = 3,
    Idle     = 4,
}

const PRIORITY_LEVELS: usize = 5;

impl SchedPriority {
    pub fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::RealTime,
            1 => Self::High,
            2 => Self::Normal,
            3 => Self::Low,
            _ => Self::Idle,
        }
    }
}

/// A bitmask of CPU cores this process is allowed to run on.
/// Bit N = core N. ALL-ones means no restriction.
#[derive(Debug, Clone, Copy)]
pub struct CpuAffinityMask(pub u64);

impl CpuAffinityMask {
    /// Unrestricted: allowed on every core.
    pub const ANY:   Self = Self(u64::MAX);
    /// Core 0 only (QEMU single-core default).
    pub const CORE0: Self = Self(1 << 0);

    pub fn allows(&self, core: u8) -> bool {
        self.0 & (1u64 << core) != 0
    }
}

// =====================================================================
// -----| Part 2 - Lock-free Per-Priority Ready Queues |-----
// =====================================================================

/// Capacity of each per-priority ready queue.
const QUEUE_CAPACITY: usize = 256;

/// Five lock-free FIFO queues, one per SchedPriority level.
/// Each queue holds PIDs (u32). ArrayQueue is backed by crossbeam's
/// lock-free ring buffer — safe to push from an ISR and pop from the
/// scheduler without holding a spinlock.
pub struct PriorityQueues {
    queues: [ArrayQueue<u32>; PRIORITY_LEVELS],
}

impl PriorityQueues {
    fn new() -> Self {
        Self {
            queues: [
                ArrayQueue::new(QUEUE_CAPACITY), // RealTime
                ArrayQueue::new(QUEUE_CAPACITY), // High
                ArrayQueue::new(QUEUE_CAPACITY), // Normal
                ArrayQueue::new(QUEUE_CAPACITY), // Low
                ArrayQueue::new(QUEUE_CAPACITY), // Idle
            ],
        }
    }

    /// Enqueue a PID at the given priority.
    /// Returns false if the queue for that level is full.
    pub fn enqueue(&self, priority: SchedPriority, pid: u32) -> bool {
        self.queues[priority as usize].push(pid).is_ok()
    }

    /// Dequeue from the highest-priority non-empty queue.
    /// Returns (priority, pid) or None if all queues are empty.
    pub fn dequeue_highest(&self) -> Option<(SchedPriority, u32)> {
        for level in 0..PRIORITY_LEVELS {
            if let Some(pid) = self.queues[level].pop() {
                return Some((SchedPriority::from_u8(level as u8), pid));
            }
        }
        None
    }

    /// Number of PIDs waiting at a given priority.
    pub fn len_at(&self, priority: SchedPriority) -> usize {
        self.queues[priority as usize].len()
    }
}

// =====================================================================
// -----| Part 3 - Per-Core Scheduling Domain |-----
// =====================================================================

/// One scheduling domain = one logical CPU core.
///
/// Currently VEILOS uses exactly one domain (Core 0). Adding SMP later
/// means creating one SchedDomain per core and routing processes to
/// domains based on affinity masks.
pub struct SchedDomain {
    /// Which physical core this domain manages.
    pub core_id: u8,
    /// Affinity mask: which processes are allowed to run on this core.
    pub affinity: CpuAffinityMask,
    /// Lock-free per-priority ready queues.
    pub ready: PriorityQueues,
    /// PID currently occupying this core, if any.
    pub running_pid: Option<u32>,
    /// Ticks remaining in the running process's current quantum.
    /// Decremented by tick(). Reaches 0 -> QUANTUM_EXPIRED is set.
    pub quantum_remaining: u32,
    /// The priority level the running process was scheduled at.
    pub running_priority: SchedPriority,
}

impl SchedDomain {
    fn new(core_id: u8) -> Self {
        Self {
            core_id,
            affinity: CpuAffinityMask::ANY,
            ready: PriorityQueues::new(),
            running_pid: None,
            quantum_remaining: 0,
            running_priority: SchedPriority::Normal,
        }
    }

    /// Called every timer tick (ISR hot path). Returns true on expiry.
    #[inline(always)]
    pub fn tick(&mut self) -> bool {
        if self.running_pid.is_none()  { return false; }
        if self.quantum_remaining == 0 { return false; }
        self.quantum_remaining -= 1;
        self.quantum_remaining == 0
    }

    /// Arm a fresh quantum for the given process.
    pub fn arm_quantum(&mut self, pid: u32, priority: SchedPriority, ticks: u32) {
        self.running_pid       = Some(pid);
        self.running_priority  = priority;
        self.quantum_remaining = ticks;
    }

    /// Yield the running slot — used when the process blocks or exits.
    pub fn yield_slot(&mut self) {
        self.running_pid       = None;
        self.quantum_remaining = 0;
    }
}

// =====================================================================
// -----| Part 4 - Dynamic Quantum Configuration |-----
// =====================================================================

/// Quantum = base_ticks x priority_multiplier [x perf_bonus in Perf mode]
///
/// Multiplier rationale:
///   RealTime — minimal quantum; pre-empted fast to keep latency low.
///   High     — moderate.
///   Normal   — 30 ms at 1 kHz (comfortable for most apps).
///   Low      — large quantum; avoids churning the scheduler for background work.
///   Idle     — only runs when nothing else is ready.
///   PerfBonus — Performance Mode gives the active game an extra 2x on top of
///               its normal multiplier to minimise pre-emptions mid-frame.
pub struct QuantumConfig {
    pub base_ticks:        u32, // 10 ticks = 10 ms at 1 kHz
    pub rt_multiplier:     u32,
    pub high_multiplier:   u32,
    pub normal_multiplier: u32,
    pub low_multiplier:    u32,
    pub idle_multiplier:   u32,
    pub perf_mode_bonus:   u32,
}

impl QuantumConfig {
    const fn default_config() -> Self {
        Self {
            base_ticks:        10,
            rt_multiplier:     1,  // 10 ms
            high_multiplier:   2,  // 20 ms
            normal_multiplier: 3,  // 30 ms
            low_multiplier:    5,  // 50 ms
            idle_multiplier:   8,  // 80 ms
            perf_mode_bonus:   2,  // +20 ms in Performance Mode
        }
    }

    /// Compute quantum in ticks for a given priority and policy mode.
    pub fn quantum_ticks(&self, priority: SchedPriority, perf_mode: bool) -> u32 {
        let mult = match priority {
            SchedPriority::RealTime => self.rt_multiplier,
            SchedPriority::High     => self.high_multiplier,
            SchedPriority::Normal   => self.normal_multiplier,
            SchedPriority::Low      => self.low_multiplier,
            SchedPriority::Idle     => self.idle_multiplier,
        };
        let bonus = if perf_mode { self.perf_mode_bonus } else { 0 };
        self.base_ticks * (mult + bonus)
    }
}

// =====================================================================
// -----| Part 5 - Live Session Statistics |-----
// =====================================================================

/// Tracks resource usage for the currently running process this session.
/// Created on launch, finalised on exit, merged into AppProfile.
#[derive(Debug, Clone)]
pub struct SessionStats {
    pub pid:                u32,
    pub name:               [u8; 32],
    /// CPU ticks this process has held the core this session.
    pub cpu_ticks:          u64,
    /// Highest observed memory footprint this session (bytes).
    pub peak_mem_bytes:     u64,
    /// Tick counter at session start.
    pub session_start_tick: u64,
}

impl SessionStats {
    fn new(pid: u32, name: &[u8; 32], start_tick: u64) -> Self {
        Self {
            pid,
            name: *name,
            cpu_ticks: 0,
            peak_mem_bytes: 0,
            session_start_tick: start_tick,
        }
    }

    fn session_duration(&self, current_tick: u64) -> u64 {
        current_tick.saturating_sub(self.session_start_tick)
    }
}

// =====================================================================
// -----| Part 6 - App Profile (serialisable, fixed-size) |-----
// =====================================================================

/// Persistent per-application resource-usage profile.
///
/// Serialised as raw bytes (repr(C)) into /SCHEDPRO.BIN on FAT32.
///
/// How the adaptive decisions work
/// ---------------------------------
///   suggested_priority    — derived from avg_cpu_ticks relative to simple
///                           thresholds. CPU-heavy apps rise to High;
///                           lightweight background apps sink to Low/Idle.
///   suggested_quantum_mult — long-session apps (games) get a bigger
///                            multiplier so they are not constantly chopped
///                            mid-frame by the timer interrupt.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct AppProfile {
    /// Null-padded app name, max 31 chars + NUL.
    pub name:                         [u8; 32],
    /// Number of recorded sessions.
    pub run_count:                     u32,
    /// Rolling average CPU ticks consumed per session.
    pub avg_cpu_ticks_per_session:     u64,
    /// Rolling average memory footprint (bytes) per session.
    pub avg_mem_bytes:                 u64,
    /// Lifetime peak CPU ticks in a single session.
    pub peak_cpu_ticks:                u64,
    /// Lifetime peak memory (bytes) in a single session.
    pub peak_mem_bytes:                u64,
    /// Rolling average session duration in ticks.
    pub avg_session_duration_ticks:    u64,
    /// Suggested SchedPriority as u8 (0=RealTime, 4=Idle).
    pub suggested_priority:            u8,
    /// Suggested quantum multiplier (1-8).
    pub suggested_quantum_mult:        u8,
    /// Reserved. Keeps the struct 84 bytes.
    _pad:                              [u8; 6],
}

// repr(C) layout (x86-64):
//   name[32] + run_count u32 + 4-byte align-pad + 5×u64 + 2×u8 + _pad[6] = 88 bytes
const _PROFILE_SIZE_CHECK: () = assert!(core::mem::size_of::<AppProfile>() == 88);

impl AppProfile {
    fn default_for(name: &[u8; 32]) -> Self {
        Self {
            name:                         *name,
            run_count:                     0,
            avg_cpu_ticks_per_session:     0,
            avg_mem_bytes:                 0,
            peak_cpu_ticks:                0,
            peak_mem_bytes:                0,
            avg_session_duration_ticks:    0,
            suggested_priority:            SchedPriority::Normal as u8,
            suggested_quantum_mult:        3,
            _pad:                          [0; 6],
        }
    }

    /// Merge a completed session into this profile using a rolling average.
    /// Uses integer arithmetic to stay no_std-compatible.
    pub fn record_session(&mut self, cpu_ticks: u64, mem_bytes: u64, session_ticks: u64) {
        let n = self.run_count.saturating_add(1);
        self.run_count = n;

        // Rolling average: new_avg = old_avg + (new_val - old_avg) / n
        let delta = (cpu_ticks as i128) - (self.avg_cpu_ticks_per_session as i128);
        self.avg_cpu_ticks_per_session =
            ((self.avg_cpu_ticks_per_session as i128) + delta / n as i128).max(0) as u64;

        let delta = (mem_bytes as i128) - (self.avg_mem_bytes as i128);
        self.avg_mem_bytes =
            ((self.avg_mem_bytes as i128) + delta / n as i128).max(0) as u64;

        let delta = (session_ticks as i128) - (self.avg_session_duration_ticks as i128);
        self.avg_session_duration_ticks =
            ((self.avg_session_duration_ticks as i128) + delta / n as i128).max(0) as u64;

        if cpu_ticks   > self.peak_cpu_ticks { self.peak_cpu_ticks = cpu_ticks; }
        if mem_bytes   > self.peak_mem_bytes  { self.peak_mem_bytes  = mem_bytes; }

        self.suggested_priority    = Self::compute_priority(self.avg_cpu_ticks_per_session) as u8;
        self.suggested_quantum_mult = Self::compute_quantum_mult(self.avg_session_duration_ticks);
    }

    /// Heuristic: map average CPU ticks/session -> priority level.
    ///
    /// Thresholds (1 kHz PIT):
    ///   > 30 000 ticks = app has been using 30+ s of CPU per session  -> High
    ///   > 5 000  ticks = moderate CPU use                              -> Normal
    ///   > 0                                                            -> Low
    ///   0 (no history)                                                 -> Normal
    fn compute_priority(avg_cpu_ticks: u64) -> SchedPriority {
        if avg_cpu_ticks > 30_000 {
            SchedPriority::High
        } else if avg_cpu_ticks > 5_000 {
            SchedPriority::Normal
        } else if avg_cpu_ticks > 0 {
            SchedPriority::Low
        } else {
            SchedPriority::Normal
        }
    }

    /// Heuristic: map average session duration -> quantum multiplier.
    ///
    /// Long-running apps (games) get a bigger quantum to avoid choppy
    /// pre-emptions mid-frame.
    ///   > 60 000 ticks (60 s sessions) -> mult 6 = 60 ms quantum
    ///   > 10 000 ticks (10 s)          -> mult 4 = 40 ms
    ///   otherwise                       -> mult 3 = 30 ms (default)
    fn compute_quantum_mult(avg_session_dur: u64) -> u8 {
        if avg_session_dur > 60_000 { 6 }
        else if avg_session_dur > 10_000 { 4 }
        else { 3 }
    }
}

// =====================================================================
// -----| Part 7 - Profile Store (load / save via FAT32) |-----
// =====================================================================

/// Root-level FAT32 path where profiles are persisted.
/// Must be an 8.3 filename so write_file can locate it without
/// long-name support.
const PROFILES_PATH: &str = "/SCHEDPRO.BIN";

const PROFILE_MAGIC:   [u8; 4] = [b'V', b'L', b'S', b'P'];
const PROFILE_VERSION: u8      = 1;
const PROFILE_SIZE:    usize   = core::mem::size_of::<AppProfile>(); // 84

pub struct ProfileStore {
    profiles: BTreeMap<[u8; 32], AppProfile>,
    /// True when in-memory data is ahead of what is on disk.
    dirty: bool,
}

impl ProfileStore {
    fn new() -> Self { Self { profiles: BTreeMap::new(), dirty: false } }

    /// Try to load profiles from the VFS. Returns an empty store on
    /// first boot or if the file is corrupt — both are safe.
    fn load_from_vfs() -> Self {
        match crate::vfs::VFS.read_file(PROFILES_PATH) {
            Ok(bytes) => {
                let profiles = Self::deserialise(&bytes);
                let count    = profiles.len();
                let store    = Self { profiles, dirty: false };
                crate::serial_println!("[SCHED] Loaded {} app profile(s) from {}.", count, PROFILES_PATH);
                store
            }
            Err(_) => {
                crate::println!("[SCHED] No profile store found - starting fresh.");
                Self::new()
            }
        }
    }

    /// Write dirty profiles to FAT32. No-op if nothing has changed.
    pub fn save_to_vfs(&mut self) {
        if !self.dirty { return; }
        let bytes = Self::serialise(&self.profiles);
        match crate::vfs::VFS.write_file(PROFILES_PATH, &bytes) {
            Ok(_) => {
                self.dirty = false;
                crate::serial_println!(
                    "[SCHED] Flushed {} profile(s) to {}.",
                    self.profiles.len(), PROFILES_PATH
                );
            }
            Err(e) => {
                crate::serial_println!("[SCHED] WARNING: Could not flush profiles: {:?}", e);
            }
        }
    }

    pub fn get(&self, name: &[u8; 32]) -> Option<&AppProfile> {
        self.profiles.get(name)
    }

    pub fn upsert(&mut self, profile: AppProfile) {
        self.profiles.insert(profile.name, profile);
        self.dirty = true;
    }

    fn get_or_insert(&mut self, name: &[u8; 32]) -> &mut AppProfile {
        self.profiles
            .entry(*name)
            .or_insert_with(|| AppProfile::default_for(name))
    }

    /// Update rolling stats after a session ends.
    pub fn record_session(
        &mut self, name: &[u8; 32],
        cpu_ticks: u64, mem_bytes: u64, session_ticks: u64,
    ) {
        let profile = self.get_or_insert(name);
        profile.record_session(cpu_ticks, mem_bytes, session_ticks);
        self.dirty = true;
    }

    // ---- Binary serialisation ----------------------------------------

    fn serialise(profiles: &BTreeMap<[u8; 32], AppProfile>) -> Vec<u8> {
        let count = profiles.len().min(u16::MAX as usize) as u16;
        let mut out = Vec::with_capacity(7 + (count as usize) * PROFILE_SIZE);
        out.extend_from_slice(&PROFILE_MAGIC);
        out.push(PROFILE_VERSION);
        out.extend_from_slice(&count.to_le_bytes());
        for profile in profiles.values().take(count as usize) {
            // SAFETY: AppProfile is repr(C); no padding in active fields.
            let bytes = unsafe {
                core::slice::from_raw_parts(
                    profile as *const AppProfile as *const u8,
                    PROFILE_SIZE,
                )
            };
            out.extend_from_slice(bytes);
        }
        out
    }

    fn deserialise(data: &[u8]) -> BTreeMap<[u8; 32], AppProfile> {
        let mut map = BTreeMap::new();
        if data.len() < 7 { return map; }
        if data[0..4] != PROFILE_MAGIC { return map; }
        if data[4] != PROFILE_VERSION  { return map; }
        let count    = u16::from_le_bytes([data[5], data[6]]) as usize;
        let required = 7 + count * PROFILE_SIZE;
        if data.len() < required { return map; }
        for i in 0..count {
            let offset = 7 + i * PROFILE_SIZE;
            // SAFETY: bounds checked above; repr(C) guarantees layout.
            let profile: AppProfile = unsafe {
                core::ptr::read_unaligned(
                    data.as_ptr().add(offset) as *const AppProfile
                )
            };
            map.insert(profile.name, profile);
        }
        map
    }
}

// =====================================================================
// -----| Part 8 - Adaptive Scheduler |-----
// =====================================================================

/// Top-level scheduler: per-core domain + quantum config + live session
/// stats + persistent profile store.
pub struct AdaptiveScheduler {
    /// Single domain (Core 0). Extend to Vec<SchedDomain> for SMP.
    pub domain:   SchedDomain,
    /// Quantum sizing rules.
    pub quantum:  QuantumConfig,
    /// Live stats for the currently tracked session, if any.
    pub session:  Option<SessionStats>,
    /// Persistent app profiles loaded from / saved to FAT32.
    pub profiles: ProfileStore,
}

impl AdaptiveScheduler {
    /// One-time init called via lazy_static after heap + VFS are live.
    pub fn init() -> Self {
        Self {
            domain:   SchedDomain::new(0),
            quantum:  QuantumConfig::default_config(),
            session:  None,
            profiles: ProfileStore::new(), // read once the disk is up (load_profiles)
        }
    }

    // ---- Process lifecycle hooks -------------------------------------

    /// Call when a new process is about to be first scheduled.
    /// Looks up its history, picks priority + quantum, enqueues the PID.
    ///
    /// Wire this into: `process::ProcessManager::set_active()`
    pub fn on_process_launch(&mut self, pid: u32, name: &str, current_tick: u64) {
        let name83 = name_to_fixed32(name);
        let (priority, quantum_mult) = match self.profiles.get(&name83) {
            Some(p) => (SchedPriority::from_u8(p.suggested_priority), p.suggested_quantum_mult),
            None    => (SchedPriority::Normal, 3u8),
        };

        let is_perf = crate::policy::POLICY.lock().get_mode()
            == crate::policy::PolicyMode::Performance;
        let ticks = self.quantum.base_ticks
            * (quantum_mult as u32 + if is_perf { self.quantum.perf_mode_bonus } else { 0 });

        self.domain.ready.enqueue(priority, pid);
        if let Some((p, next_pid)) = self.domain.ready.dequeue_highest() {
            self.domain.arm_quantum(next_pid, p, ticks);
        }

        self.session = Some(SessionStats::new(pid, &name83, current_tick));

        crate::println!(
            "[SCHED] Launch pid={} \"{}\" -> {:?}, quantum={}ms",
            pid, name, priority, ticks
        );
    }

    /// Call when a process exits or is terminated.
    /// Finalises session stats and writes them into the rolling profile.
    ///
    /// Wire this into: `process::Process::terminate()`
    pub fn on_process_exit(&mut self, pid: u32, final_mem_bytes: u64, current_tick: u64) {
        if let Some(mut s) = self.session.take() {
            if s.pid == pid {
                s.peak_mem_bytes = s.peak_mem_bytes.max(final_mem_bytes);
                let duration = s.session_duration(current_tick);
                self.profiles.record_session(
                    &s.name, s.cpu_ticks, s.peak_mem_bytes, duration,
                );
                crate::println!(
                    "[SCHED] Exit pid={}: cpu_ticks={} peak_mem={}B duration={}ms",
                    pid, s.cpu_ticks, s.peak_mem_bytes, duration
                );
            } else {
                // A different pid exited — put the session back.
                self.session = Some(s);
            }
        }
        self.domain.yield_slot();
    }

    // ---- Timer ISR hot path -----------------------------------------

    /// Called once per tick (1 kHz). Must be extremely fast.
    /// 1. Decrements quantum counter.
    /// 2. Sets QUANTUM_EXPIRED on expiry.
    /// 3. Increments session CPU-tick counter.
    #[inline(always)]
    pub fn tick(&mut self, _current_tick: u64) {
        if self.domain.tick() {
            QUANTUM_EXPIRED.store(true, Ordering::Release);
        }
        if let Some(ref mut s) = self.session {
            s.cpu_ticks = s.cpu_ticks.saturating_add(1);
        }
    }

    // ---- Quantum expiry handler -------------------------------------

    /// Called by the executor when it observes QUANTUM_EXPIRED == true.
    /// Re-queues the current process and picks the next one.
    pub fn handle_expiry(&mut self, current_tick: u64) {
        QUANTUM_EXPIRED.store(false, Ordering::Release);

        let is_perf = crate::policy::POLICY.lock().get_mode()
            == crate::policy::PolicyMode::Performance;

        // Re-queue the current process.
        if let Some(pid) = self.domain.running_pid {
            let priority = self.domain.running_priority;
            self.domain.ready.enqueue(priority, pid);
        }

        // Pick the highest-priority waiting process.
        if let Some((priority, next_pid)) = self.domain.ready.dequeue_highest() {
            let mult  = self.quantum_mult_for(next_pid);
            let ticks = self.quantum.base_ticks
                * (mult + if is_perf { self.quantum.perf_mode_bonus } else { 0 });
            self.domain.arm_quantum(next_pid, priority, ticks);
        }

        let _ = current_tick;
    }

    // ---- Profile flush ----------------------------------------------

    /// Flush dirty profiles to FAT32.
    /// Call on graceful shutdown, or periodically from a background task.
    pub fn flush_profiles(&mut self) {
        self.profiles.save_to_vfs();
    }

    // ---- Query helpers ----------------------------------------------

    /// Priority a named app would be given on its next launch.
    pub fn peek_priority(&self, name: &str) -> SchedPriority {
        let name83 = name_to_fixed32(name);
        self.profiles
            .get(&name83)
            .map(|p| SchedPriority::from_u8(p.suggested_priority))
            .unwrap_or(SchedPriority::Normal)
    }

    // ---- Mesh Distribution / Offloading (OffloadBroker delegation) ------

    /// Submit a TaskCapsule for offloading.  Routes through the global
    /// OffloadBroker which handles policy, node selection, tracking, and
    /// the full retry/failure path.
    pub fn offload_task(
        &mut self,
        target_node: crate::distributed::NodeId,
        task_data:   &[u8],
    ) {
        let capsule = crate::task_capsule::TaskCapsule::new(
            crate::task_capsule::alloc_job_id(),
            0, 0, 0, true,
            task_data.to_vec(),
            alloc::vec::Vec::new(),
            30_000,
            crate::distributed::CAP_EXEC as u64,
        );
        OFFLOAD_BROKER.lock().submit_to(capsule, target_node);
    }

    /// Dispatch a batch of independent capsules across multiple nodes.
    pub fn dispatch_batch(
        &mut self,
        nodes:      &[crate::distributed::NodeId],
        batch_data: &[u8],
    ) {
        crate::println!("[SCHED] Dispatching batch job to {} nodes", nodes.len());
        for (i, node) in nodes.iter().enumerate() {
            let capsule = crate::task_capsule::TaskCapsule::new(
                crate::task_capsule::alloc_job_id(),
                0, 0, i as u32, true,
                batch_data.to_vec(),
                alloc::vec::Vec::new(),
                30_000,
                crate::distributed::CAP_EXEC as u64,
            );
            OFFLOAD_BROKER.lock().submit_to(capsule, *node);
        }
    }

    // ---- Private helpers --------------------------------------------

    fn quantum_mult_for(&self, pid: u32) -> u32 {
        let pm = crate::process::PROCESSOR.lock();
        if let Some(proc) = pm.find(pid) {
            let name83 = name_to_fixed32(proc.name);
            if let Some(profile) = self.profiles.get(&name83) {
                return profile.suggested_quantum_mult as u32;
            }
        }
        3 // Normal default
    }
}

// =====================================================================
// -----| Utilities |-----
// =====================================================================

/// Convert a UTF-8 process name to the fixed 32-byte profile key.
/// Truncated at 31 chars, NUL-padded, ASCII upper-cased for
/// case-insensitive lookup.
pub fn name_to_fixed32(name: &str) -> [u8; 32] {
    let mut out = [0u8; 32];
    for (i, b) in name.bytes().take(31).enumerate() {
        out[i] = b.to_ascii_uppercase();
    }
    out
}

// =====================================================================
// -----| Globals & ISR Entry Point |-----
// =====================================================================

/// Set by the timer ISR when the running process's quantum has expired.
/// The executor checks this after each task poll and calls
/// `SCHEDULER.lock().handle_expiry(tick)` to pick the next process.
pub static QUANTUM_EXPIRED: AtomicBool = AtomicBool::new(false);

lazy_static! {
    /// Global adaptive scheduler. Lazily initialised on first access,
    /// which must happen after the heap allocator is online.
    pub static ref SCHEDULER: Mutex<AdaptiveScheduler> =
        Mutex::new(AdaptiveScheduler::init());
}

/// Set to true by `init()` once the heap is live and the lazy_static
/// has been safely constructed. The timer ISR checks this before
/// touching SCHEDULER so it never fires into an uninitialised heap.
static SCHEDULER_READY: AtomicBool = AtomicBool::new(false);

/// Call this from kernel_main AFTER `allocator::init_heap()` returns.
/// It forces the lazy_static to construct (allocating the ArrayQueues
/// and BTreeMap on the now-live heap) and arms the ISR guard.
pub fn init() {
    // Touch SCHEDULER to force lazy construction now, on a live heap.
    let _ = SCHEDULER.lock().domain.core_id;
    SCHEDULER_READY.store(true, Ordering::Release);
    crate::println!("[SCHED] Adaptive scheduler online.");
}

/// Read the saved app profiles. Call once the disk is mounted: `init` runs
/// before storage binds, when there's nothing to read yet.
pub fn load_profiles() {
    let store = ProfileStore::load_from_vfs();
    let mut s = SCHEDULER.lock();
    if !s.profiles.dirty {
        s.profiles = store;
    }
}

/// Thin shim called from the timer interrupt handler in interrupts.rs.
///
/// Uses try_lock so it NEVER spins inside an ISR context. Missing one
/// quantum decrement is harmless — the counter catches up next tick.
/// Also reaps zombie processes every 1000 ticks (~1 s) so the process
/// list never accumulates terminated entries indefinitely.
#[inline(always)]
pub fn scheduler_tick(current_tick: u64) {
    // Guard: do nothing until init() has been called post-heap-init.
    if !SCHEDULER_READY.load(Ordering::Acquire) { return; }
    if let Some(mut sched) = SCHEDULER.try_lock() {
        sched.tick(current_tick);
    }
}

/// Reap dead processes and finished threads once a second.
///
/// This used to run inside the timer ISR, where tearing down a process
/// (unmapping pages, freeing frames, heap frees, WIN32_USER cleanup) ran with
/// interrupts off on top of whatever kernel code the tick interrupted. As an
/// executor task it runs in normal kernel context. It doesn't run while a
/// user process owns the BSP; dead processes simply wait for the next pass.
pub async fn reaper_task() {
    loop {
        crate::interrupts::SleepFuture::new(1000).await;
        crate::process::PROCESSOR.lock().reap_zombies();
        crate::process::THREAD_MANAGER.lock().reap_finished();
    }
}

// =====================================================================
// -----| Offload Broker |-----
// =====================================================================
//
// The OffloadBroker is the central coordinator for all mesh task
// offloading.  It owns the full lifecycle of an in-flight job:
//
//   1. Policy check   — reads MESH_OFFLOAD_POLICY from the VScript engine.
//                       "disabled" → run locally.
//                       "ask"      → hold in waiting_for_approval, surface
//                                    a desktop notification.
//                       "always"   → dispatch immediately.
//
//   2. Node selection — picks the best available peer that advertises
//                       CAP_EXEC and is not overloaded with in-flight jobs.
//
//   3. Dispatch       — calls distributed::remote_exec_capsule() to send
//                       the capsule over TCP and collect the TaskResult.
//
//   4. Result delivery — marks the thread Finished and delivers output.
//
//   5. Failure & retry —
//        attempt 1-2: retry on the same node.
//        attempt 3-4: reassign to a different node.
//        attempt  >4: notify the user via desktop::notify(); give up.
//
// Win32 threads: no checkpoint is written, and failure handling stops
// at user notification (never retried — the thread may be in an
// inconsistent state after a partial remote execution).

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OffloadPolicy {
    Disabled,
    Ask,
    Always,
}

/// One tracked in-flight job.
#[derive(Clone)]
pub struct OffloadJob {
    pub job_id:               u64,
    pub capsule:              crate::task_capsule::TaskCapsule,
    pub assigned_node:        crate::distributed::NodeId,
    /// 1-4: attempt counter (2 on first node, 2 on second).
    pub attempt:              u8,
    /// Highest step for which a checkpoint exists (0 = none).
    pub last_checkpoint_step: u32,
    /// Tick at which the job was submitted.
    pub submitted_tick:       u64,
}

/// A capsule held pending user approval (policy == "ask").
struct PendingApproval {
    capsule:       crate::task_capsule::TaskCapsule,
    preferred_node: crate::distributed::NodeId,
}

pub struct OffloadBroker {
    /// Jobs currently dispatched to a remote node.
    pub in_flight:       BTreeMap<u64, OffloadJob>,
    /// Completed results waiting to be consumed by the originating thread.
    pub pending_results: BTreeMap<u64, crate::task_capsule::TaskResult>,
    /// Capsules held for user approval.
    waiting_for_approval: Vec<PendingApproval>,
}

impl OffloadBroker {
    pub const fn new() -> Self {
        Self {
            in_flight:            BTreeMap::new(),
            pending_results:      BTreeMap::new(),
            waiting_for_approval: Vec::new(),
        }
    }

    // ----------------------------------------------------------------
    // Policy
    // ----------------------------------------------------------------

    /// Read MESH_OFFLOAD_POLICY from the VScript engine.
    /// Defaults to Ask when the variable is absent or unrecognised.
    pub fn read_policy() -> OffloadPolicy {
        let eng = crate::vscript::GLOBAL_ENGINE.lock();
        match eng.variables.get("MESH_OFFLOAD_POLICY") {
            Some(crate::vscript::VValue::String(s)) => match s.to_uppercase().as_str() {
                "DISABLED" => OffloadPolicy::Disabled,
                "ALWAYS"   => OffloadPolicy::Always,
                _          => OffloadPolicy::Ask,
            },
            _ => OffloadPolicy::Ask,
        }
    }

    // ----------------------------------------------------------------
    // Node selection
    // ----------------------------------------------------------------

    /// Find the best available peer node for `cap_required`.
    ///
    /// Selection criteria (in priority order):
    ///   1. Node must advertise every bit in `cap_required`.
    ///   2. Prefer nodes carrying fewer in-flight jobs (load balancing).
    ///   3. Among equal-load nodes, prefer by NodeId ascending (stable).
    ///
    /// Returns `None` when no suitable peer is available.
    pub fn select_node(
        &self,
        cap_required: u32,
        exclude: Option<&crate::distributed::NodeId>,
    ) -> Option<crate::distributed::NodeId> {
        let mesh = crate::distributed::NODE_MESH.lock();
        let mut best: Option<(crate::distributed::NodeId, usize)> = None;

        for (id, peer) in mesh.peers.iter() {
            if peer.capabilities & cap_required != cap_required { continue; }
            if exclude == Some(id) { continue; }

            let load = self.in_flight.values()
                .filter(|j| &j.assigned_node == id)
                .count();

            let better = match best {
                None           => true,
                Some((_, bl))  => load < bl,
            };
            if better { best = Some((*id, load)); }
        }

        best.map(|(id, _)| id)
    }

    // ----------------------------------------------------------------
    // Submit
    // ----------------------------------------------------------------

    /// Submit a capsule with automatic policy check and node selection.
    /// This is the primary entry point for the OffloadBroker.
    pub fn submit(&mut self, capsule: crate::task_capsule::TaskCapsule) {
        let policy = Self::read_policy();
        let cap_req = capsule.capability_requirements as u32
            | crate::distributed::CAP_EXEC;

        match policy {
            OffloadPolicy::Disabled => {
                // Run locally via the terminal executor.
                self.run_locally(capsule);
            }

            OffloadPolicy::Always => {
                match self.select_node(cap_req, None) {
                    Some(node) => self.dispatch(capsule, node, 1),
                    None => {
                        crate::desktop::notify(
                            crate::desktop::NotifCategory::Warning,
                            "MESH OFFLOAD",
                            "No capable peer available — running locally",
                        );
                        self.run_locally(capsule);
                    }
                }
            }

            OffloadPolicy::Ask => {
                match self.select_node(cap_req, None) {
                    Some(node) => {
                        // Surface a desktop notification and hold the capsule.
                        let thread_name = crate::process::THREAD_MANAGER
                            .lock()
                            .find_thread(capsule.thread_id)
                            .map(|t| alloc::string::String::from(t.name_str()))
                            .unwrap_or_else(|| alloc::format!("tid={}", capsule.thread_id));

                        crate::desktop::notify(
                            crate::desktop::NotifCategory::Info,
                            "MESH OFFLOAD",
                            &alloc::format!(
                                "Offload '{}' to {}?  MESH_OFFLOAD_APPROVE {:#x} / DENY",
                                thread_name, node.to_hex(), capsule.job_id
                            ),
                        );
                        self.waiting_for_approval.push(PendingApproval {
                            capsule,
                            preferred_node: node,
                        });
                    }
                    None => {
                        crate::desktop::notify(
                            crate::desktop::NotifCategory::Warning,
                            "MESH OFFLOAD",
                            "No capable peer — running locally",
                        );
                        self.run_locally(capsule);
                    }
                }
            }
        }
    }

    /// Submit directly to a specific node, bypassing node selection and
    /// policy (used by the legacy `offload_task` / `dispatch_batch` shims
    /// and by the approval path after the user confirms).
    pub fn submit_to(
        &mut self,
        capsule: crate::task_capsule::TaskCapsule,
        node:    crate::distributed::NodeId,
    ) {
        self.dispatch(capsule, node, 1);
    }

    /// Approve a pending offload (user typed MESH_OFFLOAD_APPROVE <job_id>).
    pub fn approve(&mut self, job_id: u64) {
        let pos = self.waiting_for_approval
            .iter()
            .position(|p| p.capsule.job_id == job_id);

        if let Some(idx) = pos {
            let pending = self.waiting_for_approval.remove(idx);
            crate::println!("[BROKER] Offload {:#x} approved, dispatching", job_id);
            self.dispatch(pending.capsule, pending.preferred_node, 1);
        } else {
            crate::println!("[BROKER] No pending offload with job_id {:#x}", job_id);
        }
    }

    /// Deny a pending offload (user typed MESH_OFFLOAD_DENY <job_id>).
    pub fn deny(&mut self, job_id: u64) {
        let pos = self.waiting_for_approval
            .iter()
            .position(|p| p.capsule.job_id == job_id);

        if let Some(idx) = pos {
            let pending = self.waiting_for_approval.remove(idx);
            crate::println!("[BROKER] Offload {:#x} denied, running locally", job_id);
            self.run_locally(pending.capsule);
        }
    }

    // ----------------------------------------------------------------
    // Dispatch
    // ----------------------------------------------------------------

    /// Send the capsule to `node`, record the job as in-flight, and
    /// collect the result synchronously (our TCP layer is synchronous).
    ///
    /// On success  → `on_result()`.
    /// On failure  → `on_failure()` which implements the retry logic.
    fn dispatch(
        &mut self,
        capsule: crate::task_capsule::TaskCapsule,
        node:    crate::distributed::NodeId,
        attempt: u8,
    ) {
        let job_id     = capsule.job_id;
        let step_index = capsule.step_index;
        let is_native  = capsule.is_native;
        let now        = crate::interrupts::TICKS.load(core::sync::atomic::Ordering::Relaxed);

        // Find the last checkpoint for this job (for resume on retry).
        let last_ckpt = if is_native {
            crate::checkpoint::CheckpointManager::find_latest_checkpoint_local(job_id)
                .unwrap_or(0)
        } else {
            0
        };

        // Record the job as in-flight before sending so `select_node`
        // can account for the load even if the send is instantaneous.
        self.in_flight.insert(job_id, OffloadJob {
            job_id,
            capsule:              capsule.clone(),
            assigned_node:        node,
            attempt,
            last_checkpoint_step: last_ckpt,
            submitted_tick:       now,
        });

        crate::println!(
            "[BROKER] Dispatch job={:#x} step={} to {} attempt={}",
            job_id, step_index, node.to_hex(), attempt
        );

        // Perform the remote call.
        match crate::distributed::remote_exec_capsule(&node, &capsule) {
            Ok(result) => self.on_result(result),
            Err(e) => {
                crate::serial_println!(
                    "[BROKER] Dispatch failed job={:#x}: {:?}", job_id, e
                );
                self.on_failure(job_id, is_native);
            }
        }
    }

    // ----------------------------------------------------------------
    // Result handling
    // ----------------------------------------------------------------

    /// Called when a remote node returns a successful or failed TaskResult.
    pub fn on_result(&mut self, result: crate::task_capsule::TaskResult) {
        let job_id = result.job_id;
        crate::println!(
            "[BROKER] Result job={:#x} step={} success={}",
            job_id, result.step_index, result.success
        );

        // Remove from in-flight.
        if let Some(job) = self.in_flight.remove(&job_id) {
            // Mark the owning thread as Finished.
            if let Some(t) = crate::process::THREAD_MANAGER
                .lock()
                .find_thread_mut(job.capsule.thread_id) {
                t.state = crate::process::ThreadState::Finished;
                t.job_id = 0;
            }

            // Purge checkpoints for completed native jobs.
            if job.capsule.is_native && result.success {
                crate::checkpoint::CheckpointManager::purge_checkpoints(job_id);
            }
        }

        // Store for the caller to retrieve.
        self.pending_results.insert(job_id, result);
    }

    // ----------------------------------------------------------------
    // Failure & retry
    // ----------------------------------------------------------------

    /// Called when a remote call returns an error (network failure, remote
    /// node refused, timeout, etc.).
    ///
    /// Retry logic:
    ///   attempt 1  → retry same node  (attempt 2)
    ///   attempt 2  → retry same node  (attempt 2, last same-node try)
    ///   attempt 3  → switch node, retry  (attempt 3)
    ///   attempt 4  → last try on new node (attempt 4)
    ///   attempt >4 → notify user, store a failure result, give up.
    ///
    /// Win32 threads skip retry entirely — one attempt, then notify.
    fn on_failure(&mut self, job_id: u64, is_native: bool) {
        let job = match self.in_flight.remove(&job_id) {
            Some(j) => j,
            None    => return,
        };

        let next_attempt = job.attempt + 1;

        // Win32 threads: no retry, just notify.
        if !is_native {
            crate::desktop::notify(
                crate::desktop::NotifCategory::Warning,
                "MESH OFFLOAD",
                &alloc::format!(
                    "Win32 offload failed (job {:#x}) — thread not retried",
                    job_id
                ),
            );
            self.pending_results.insert(job_id,
                crate::task_capsule::TaskResult::err(
                    job_id, job.capsule.step_index, "win32 offload failed",
                ),
            );
            return;
        }

        if next_attempt <= 2 {
            // Retry on the same node.
            crate::println!(
                "[BROKER] Retry job={:#x} on same node {} (attempt {})",
                job_id, job.assigned_node.to_hex(), next_attempt
            );
            self.dispatch(job.capsule, job.assigned_node, next_attempt);

        } else if next_attempt <= 4 {
            // Switch to a different node.
            let cap_req = job.capsule.capability_requirements as u32
                | crate::distributed::CAP_EXEC;
            match self.select_node(cap_req, Some(&job.assigned_node)) {
                Some(new_node) => {
                    crate::println!(
                        "[BROKER] Retry job={:#x} on NEW node {} (attempt {})",
                        job_id, new_node.to_hex(), next_attempt
                    );
                    self.dispatch(job.capsule, new_node, next_attempt);
                }
                None => {
                    // No alternative node — give up now.
                    self.notify_failure(job_id, job.capsule.step_index, "no alternative node");
                }
            }

        } else {
            // Exhausted all attempts.
            self.notify_failure(
                job_id, job.capsule.step_index,
                "all retry attempts exhausted",
            );
        }
    }

    fn notify_failure(&mut self, job_id: u64, step: u32, reason: &str) {
        crate::desktop::notify(
            crate::desktop::NotifCategory::Error,
            "MESH OFFLOAD FAILED",
            &alloc::format!("job={:#x} step={}: {}", job_id, step, reason),
        );
        self.pending_results.insert(
            job_id,
            crate::task_capsule::TaskResult::err(job_id, step, reason),
        );
    }

    // ----------------------------------------------------------------
    // Local execution fallback
    // ----------------------------------------------------------------

    /// Execute a capsule locally via the terminal executor (policy=disabled
    /// or no peer available).
    fn run_locally(&mut self, capsule: crate::task_capsule::TaskCapsule) {
        let job_id     = capsule.job_id;
        let step_index = capsule.step_index;
        let is_native  = capsule.is_native;

        crate::println!("[BROKER] Running job={:#x} locally", job_id);

        let command_str = if capsule.input_data.is_empty() {
            alloc::string::String::from("ECHO [local: no command]")
        } else {
            let end = capsule.input_data.iter().take(256)
                .position(|&b| b == 0 || b == b'\n')
                .unwrap_or(capsule.input_data.len().min(256));
            core::str::from_utf8(&capsule.input_data[..end])
                .unwrap_or("ECHO [bad utf8]")
                .trim()
                .to_uppercase()
                .into()
        };

        let mut output: Vec<alloc::string::String> = Vec::new();
        crate::task::terminal::execute_command_pub(&command_str, &mut output);
        let stdout = output.join("\n");

        if is_native {
            crate::checkpoint::CheckpointManager::write_checkpoint(
                job_id, step_index, stdout.as_bytes(),
            );
        }

        let result = crate::task_capsule::TaskResult::ok(
            job_id, step_index, stdout.into_bytes(),
        );
        self.on_result(result);
    }

    // ----------------------------------------------------------------
    // Status / diagnostics
    // ----------------------------------------------------------------

    /// Return a list of human-readable status lines for BROKER STATUS.
    pub fn status_lines(&self) -> Vec<alloc::string::String> {
        let mut out = Vec::new();
        if self.in_flight.is_empty() && self.waiting_for_approval.is_empty() {
            out.push(alloc::string::String::from("  (no active jobs)"));
            return out;
        }

        for job in self.in_flight.values() {
            out.push(alloc::format!(
                "  [INFLIGHT] job={:#x} step={} node={} attempt={} ckpt_step={} native={}",
                job.job_id,
                job.capsule.step_index,
                job.assigned_node.to_hex(),
                job.attempt,
                job.last_checkpoint_step,
                job.capsule.is_native,
            ));
        }

        for pending in &self.waiting_for_approval {
            out.push(alloc::format!(
                "  [PENDING APPROVAL] job={:#x} preferred_node={}",
                pending.capsule.job_id,
                pending.preferred_node.to_hex(),
            ));
        }

        out
    }
}

lazy_static! {
    /// Global OffloadBroker instance.
    pub static ref OFFLOAD_BROKER: Mutex<OffloadBroker> =
        Mutex::new(OffloadBroker::new());
}
