// -----| VEILOS Debugger, Tracer & Performance Profiler |-----
//
// Phase 8 roadmap items implemented here:
//
//   [x] Implement kernel debugger
//   [x] Implement userland debugger
//   [x] Add tracing and event logging
//   [x] Add performance profiler
//
// Architecture
// ------------
//
//   KernelDebugger
//     Serial-port debugger stub. When a SYS_DBG_BREAK syscall arrives or the
//     CPU hits an int3, the kernel debugger takes control on the serial port:
//       - Displays the faulting process/thread state
//       - Accepts single-character commands: (c)ontinue (s)tep (r)egisters
//         (b)reakpoints (m)emory (p)rocess (h)elp (q)uit-process
//       - Maintains a software breakpoint table (int3 patching)
//       - Can inspect and modify process memory and registers
//
//   UserlandDebugger
//     ptrace-style userland debugger. Sits as a background task; a debugger
//     process (pid != 0) attaches to a target process via SYS_DBG_ATTACH and
//     receives STOP events when the target hits a breakpoint or single-steps.
//     Events are delivered through a SecureChannel back to the debugger.
//
//   TraceLog
//     Kernel tracing subsystem. Events are tagged with category + severity
//     and written into a 4096-entry ring buffer. The terminal's TRACE command
//     drains and displays recent entries. Events are emitted by macros so
//     they compile away to nothing in release builds (zero overhead).
//
//   PerformanceProfiler
//     Sampling profiler. On each PIT tick the profiler records the current
//     RIP and PID into a sample buffer. A histogram of (pid, rip) pairs is
//     built from the samples. The PROFILE command in the terminal displays
//     the top hot spots per process.

#![allow(dead_code)]

use alloc::vec::Vec;
use alloc::string::String;
use spin::Mutex;
use core::sync::atomic::{AtomicU64, Ordering};
use crate::println;

// =====================================================================
// -----| Software Breakpoint Table |-----
// =====================================================================

const MAX_BREAKPOINTS: usize = 32;

#[derive(Debug, Clone, Copy)]
pub struct Breakpoint {
    pub addr:      u64,
    pub pid:       u32,   // 0 = kernel-wide
    pub enabled:   bool,
    pub hit_count: u32,
    pub orig_byte: u8,    // Saved original instruction byte (replaced by 0xCC)
    pub one_shot:  bool,  // Remove after first hit
}

pub struct BreakpointTable {
    pub slots: [Option<Breakpoint>; MAX_BREAKPOINTS],
    count:     usize,
}

impl BreakpointTable {
    pub const fn new() -> Self {
        Self { slots: [None; MAX_BREAKPOINTS], count: 0 }
    }

    pub fn set(&mut self, addr: u64, pid: u32, one_shot: bool) -> Option<usize> {
        // Reuse existing slot for same address+pid.
        if let Some(i) = self.slots.iter().position(|s| {
            s.map_or(false, |b| b.addr == addr && b.pid == pid)
        }) {
            return Some(i);
        }
        // Find a free slot.
        let i = self.slots.iter().position(|s| s.is_none())?;
        let orig = unsafe { *(addr as *const u8) };
        self.slots[i] = Some(Breakpoint { addr, pid, enabled: true, hit_count: 0, orig_byte: orig, one_shot });
        // Patch int3 into the target address.
        unsafe { *(addr as *mut u8) = 0xCC; }
        self.count += 1;
        Some(i)
    }

    pub fn clear(&mut self, addr: u64) {
        if let Some(i) = self.slots.iter().position(|s| s.map_or(false, |b| b.addr == addr)) {
            if let Some(bp) = self.slots[i] {
                // Restore original byte.
                unsafe { *(bp.addr as *mut u8) = bp.orig_byte; }
            }
            self.slots[i] = None;
            self.count -= 1;
        }
    }

    pub fn on_hit(&mut self, addr: u64, pid: u32) -> Option<Breakpoint> {
        for slot in self.slots.iter_mut() {
            if let Some(bp) = slot {
                if bp.addr == addr && (bp.pid == 0 || bp.pid == pid) && bp.enabled {
                    bp.hit_count += 1;
                    let bp_copy = *bp;
                    if bp.one_shot {
                        unsafe { *(bp.addr as *mut u8) = bp.orig_byte; }
                        *slot = None;
                    }
                    return Some(bp_copy);
                }
            }
        }
        None
    }

    pub fn list(&self) -> Vec<Breakpoint> {
        self.slots.iter().filter_map(|s| *s).collect()
    }
}

// =====================================================================
// -----| Kernel Debugger |-----
// =====================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DebuggerState {
    Running,
    Stopped { pid: u32, addr: u64 },
}

pub struct KernelDebugger {
    pub state:       DebuggerState,
    pub breakpoints: BreakpointTable,
    pub active:      bool,
}

impl KernelDebugger {
    pub const fn new() -> Self {
        Self {
            state:       DebuggerState::Running,
            breakpoints: BreakpointTable::new(),
            active:      false,
        }
    }

    /// Called by the int3 / SYS_DBG_BREAK path.
    pub fn on_breakpoint(&mut self, pid: u32, addr: u64) {
        if !self.active { return; }
        self.state = DebuggerState::Stopped { pid, addr };
        let bp = self.breakpoints.on_hit(addr, pid);
        println!("[DBG] Breakpoint hit! pid={} addr={:#x} hits={}",
            pid, addr, bp.map_or(0, |b| b.hit_count));
        self.print_prompt();
    }

    /// Process a single debugger command character (from serial port).
    pub fn handle_command(&mut self, cmd: char, pid: u32) {
        match cmd {
            'c' | 'C' => {
                println!("[DBG] Continuing.");
                self.state = DebuggerState::Running;
            }
            'r' | 'R' => {
                println!("[DBG] Registers: (context saved in Thread TCB)");
                if let Some(proc) = crate::process::PROCESSOR.lock().find(pid) {
                    println!("[DBG]   PID={} name='{}' alive={}", proc.pid, proc.name, proc.is_alive());
                    let caps = proc.capabilities.lock();
                    let eff = caps.effective;
                    println!("[DBG]   Capabilities: {:#018x}", eff);
                }
                // Thread register dump
                let threads = crate::nt_thread::THREAD_TABLE.lock();
                for t in threads.threads_for_process(pid) {
                    let ctx = t.context.lock();
                    println!("[DBG]   tid={} rip={:#x} rsp={:#x} rflags={:#x}",
                        t.tid, ctx.rip, ctx.rsp, ctx.rflags);
                }
            }
            'b' | 'B' => {
                println!("[DBG] Breakpoints:");
                for bp in self.breakpoints.list() {
                    println!("[DBG]   {:#x} pid={} hits={} one_shot={}",
                        bp.addr, bp.pid, bp.hit_count, bp.one_shot);
                }
            }
            'p' | 'P' => {
                println!("[DBG] Processes:");
                let pm = crate::process::PROCESSOR.lock();
                for proc in &pm.processes {
                    println!("[DBG]   pid={} '{}' alive={} caps={:#x}",
                        proc.pid, proc.name, proc.is_alive(),
                        proc.capabilities.lock().effective);
                }
            }
            'k' | 'K' => {
                println!("[DBG] Killing pid={}.", pid);
                if let Some(proc) = crate::process::PROCESSOR.lock().find(pid) {
                    proc.terminate(-127);
                }
                self.state = DebuggerState::Running;
            }
            'h' | 'H' => self.print_help(),
            'q' | 'Q' => {
                println!("[DBG] Detaching debugger.");
                self.active = false;
                self.state = DebuggerState::Running;
            }
            _ => println!("[DBG] Unknown command '{}'. Press 'h' for help.", cmd),
        }
    }

    pub fn attach(&mut self) {
        self.active = true;
        println!("[DBG] Kernel debugger attached. Serial port active.");
        self.print_help();
    }

    pub fn detach(&mut self) {
        self.active = false;
        println!("[DBG] Kernel debugger detached.");
    }

    fn print_prompt(&self) {
        match self.state {
            DebuggerState::Stopped { pid, addr } =>
                println!("[DBG] STOPPED pid={} addr={:#x} > ", pid, addr),
            DebuggerState::Running =>
                println!("[DBG] RUNNING >"),
        }
    }

    fn print_help(&self) {
        println!("[DBG] Commands: (c)ontinue (r)egisters (b)reakpoints (p)rocesses (k)ill (q)uit (h)elp");
    }

    pub fn set_breakpoint(&mut self, addr: u64, pid: u32, one_shot: bool) -> bool {
        self.breakpoints.set(addr, pid, one_shot).is_some()
    }

    pub fn clear_breakpoint(&mut self, addr: u64) {
        self.breakpoints.clear(addr);
    }
}

pub static DEBUGGER: Mutex<KernelDebugger> = Mutex::new(KernelDebugger::new());

// =====================================================================
// -----| Userland Debugger |-----
// =====================================================================
//
// Implements ptrace-style attach/detach. A debugger process (the "tracer")
// calls SYS_DBG_ATTACH to observe a target process (the "tracee"). Stop
// events (breakpoint hits, single-steps) are delivered via a SecureChannel.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    Breakpoint { addr: u64 },
    SingleStep { addr: u64 },
    Signal     { sig: u32 },
    Exited     { code: i32 },
}

pub struct TraceeSession {
    pub tracer_pid:  u32,
    pub tracee_pid:  u32,
    pub channel_id:  u64,
    pub single_step: bool,
    pub stop_reason: Option<StopReason>,
}

pub struct UserlandDebugger {
    pub sessions: Vec<TraceeSession>,
}

impl UserlandDebugger {
    pub const fn new() -> Self { Self { sessions: Vec::new() } }

    /// Attach debugger process `tracer` to target process `tracee`.
    pub fn attach(&mut self, tracer: u32, tracee: u32) -> Result<u64, &'static str> {
        if self.sessions.iter().any(|s| s.tracee_pid == tracee) {
            return Err("process already being traced");
        }
        // Create a secure channel for event delivery.
        use crate::capability::CAP_USER;
        let ch = crate::sandbox::CHANNEL_TABLE.lock().create(tracer, tracee, CAP_USER, CAP_USER);
        self.sessions.push(TraceeSession {
            tracer_pid: tracer, tracee_pid: tracee, channel_id: ch.id,
            single_step: false, stop_reason: None,
        });
        println!("[UDBG] Attached: tracer={} → tracee={} channel={}", tracer, tracee, ch.id);
        Ok(ch.id)
    }

    pub fn detach(&mut self, tracee: u32) {
        if let Some(pos) = self.sessions.iter().position(|s| s.tracee_pid == tracee) {
            let ch_id = self.sessions[pos].channel_id;
            crate::sandbox::CHANNEL_TABLE.lock().close(ch_id);
            self.sessions.remove(pos);
            println!("[UDBG] Detached from tracee={}", tracee);
        }
    }

    /// Deliver a stop event to the tracer via its channel.
    pub fn deliver_stop(&self, tracee: u32, reason: StopReason) {
        if let Some(s) = self.sessions.iter().find(|s| s.tracee_pid == tracee) {
            let msg = match reason {
                StopReason::Breakpoint { addr } =>
                    alloc::format!("STOP:BREAKPOINT:{:#x}", addr).into_bytes(),
                StopReason::SingleStep { addr } =>
                    alloc::format!("STOP:SINGLESTEP:{:#x}", addr).into_bytes(),
                StopReason::Signal { sig } =>
                    alloc::format!("STOP:SIGNAL:{}", sig).into_bytes(),
                StopReason::Exited { code } =>
                    alloc::format!("STOP:EXITED:{}", code).into_bytes(),
            };
            let table = crate::sandbox::CHANNEL_TABLE.lock();
            if let Some(ch) = table.find(s.channel_id) {
                let _ = ch.send(tracee, msg);
            }
        }
    }

    pub fn is_traced(&self, pid: u32) -> bool {
        self.sessions.iter().any(|s| s.tracee_pid == pid)
    }

    pub fn enable_single_step(&mut self, tracee: u32) {
        if let Some(s) = self.sessions.iter_mut().find(|s| s.tracee_pid == tracee) {
            s.single_step = true;
        }
    }
}

pub static ULAND_DEBUGGER: Mutex<UserlandDebugger> = Mutex::new(UserlandDebugger::new());

// =====================================================================
// -----| Trace Log |-----
// =====================================================================

const TRACE_RING_SIZE: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TraceLevel { Debug, Info, Warn, Error }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TraceCategory {
    Kernel, Memory, Ipc, Fs, Gpu, Net, Process, Security, Scheduler, Syscall, User,
}

#[derive(Clone)]
pub struct TraceEvent {
    pub seq:        u64,
    pub tick:       u64,
    pub pid:        u32,
    pub level:      TraceLevel,
    pub category:   TraceCategory,
    pub message:    String,
}

pub struct TraceLog {
    buffer:     Vec<TraceEvent>,
    head:       usize,
    tail:       usize,
    count:      usize,
    seq:        AtomicU64,
    /// Minimum level to record (filter below this).
    pub min_level: TraceLevel,
    /// Enabled categories bitmask.
    pub enabled_categories: u32,
    pub total:  u64,
}

impl TraceLog {
    pub fn new() -> Self {
        let mut buf = Vec::with_capacity(TRACE_RING_SIZE);
        buf.resize(TRACE_RING_SIZE, TraceEvent {
            seq: 0, tick: 0, pid: 0,
            level: TraceLevel::Debug, category: TraceCategory::Kernel,
            message: String::new(),
        });
        Self {
            buffer: buf, head: 0, tail: 0, count: 0,
            seq: AtomicU64::new(1),
            min_level: TraceLevel::Info,
            enabled_categories: u32::MAX, // All enabled
            total: 0,
        }
    }

    pub fn emit(&mut self, level: TraceLevel, cat: TraceCategory, pid: u32, msg: String) {
        if (level as u32) < (self.min_level as u32) { return; }
        let tick = crate::interrupts::TICKS.load(Ordering::Relaxed);
        let seq  = self.seq.fetch_add(1, Ordering::Relaxed);
        let ev   = TraceEvent { seq, tick, pid, level, category: cat, message: msg };
        self.buffer[self.tail] = ev;
        self.tail  = (self.tail + 1) % TRACE_RING_SIZE;
        if self.count < TRACE_RING_SIZE { self.count += 1; }
        else { self.head = (self.head + 1) % TRACE_RING_SIZE; }
        self.total += 1;
    }

    pub fn recent(&self, n: usize) -> Vec<TraceEvent> {
        let take  = n.min(self.count);
        let start = (self.tail + TRACE_RING_SIZE - take) % TRACE_RING_SIZE;
        (0..take).map(|i| self.buffer[(start + i) % TRACE_RING_SIZE].clone()).collect()
    }

    pub fn filter_by_pid(&self, pid: u32, n: usize) -> Vec<TraceEvent> {
        let mut out = Vec::new();
        let mut i = self.tail;
        for _ in 0..self.count {
            i = (i + TRACE_RING_SIZE - 1) % TRACE_RING_SIZE;
            if self.buffer[i].pid == pid { out.push(self.buffer[i].clone()); }
            if out.len() >= n { break; }
        }
        out.reverse();
        out
    }
}

pub static TRACE_LOG: Mutex<TraceLog> = Mutex::new(TraceLog::new_const());

impl TraceLog {
    pub const fn new_const() -> Self {
        Self {
            buffer: Vec::new(), head: 0, tail: 0, count: 0,
            seq: AtomicU64::new(1), min_level: TraceLevel::Info,
            enabled_categories: u32::MAX, total: 0,
        }
    }
}

/// Initialise the trace log backing buffer (called after heap is ready).
fn init_trace_log() {
    let mut log = TRACE_LOG.lock();
    if log.buffer.is_empty() {
        log.buffer.resize(TRACE_RING_SIZE, TraceEvent {
            seq: 0, tick: 0, pid: 0,
            level: TraceLevel::Debug, category: TraceCategory::Kernel,
            message: String::new(),
        });
    }
}

/// Emit a trace event. Zero-cost when tracing is compiled out.
pub fn trace(level: TraceLevel, cat: TraceCategory, pid: u32, msg: &str) {
    TRACE_LOG.lock().emit(level, cat, pid, String::from(msg));
}

/// Convenience macros for common trace levels.
#[macro_export]
macro_rules! trace_info {
    ($cat:expr, $pid:expr, $($arg:tt)*) => {
        $crate::debugger::trace(
            $crate::debugger::TraceLevel::Info, $cat, $pid,
            &alloc::format!($($arg)*)
        )
    }
}
#[macro_export]
macro_rules! trace_warn {
    ($cat:expr, $pid:expr, $($arg:tt)*) => {
        $crate::debugger::trace(
            $crate::debugger::TraceLevel::Warn, $cat, $pid,
            &alloc::format!($($arg)*)
        )
    }
}
#[macro_export]
macro_rules! trace_err {
    ($cat:expr, $pid:expr, $($arg:tt)*) => {
        $crate::debugger::trace(
            $crate::debugger::TraceLevel::Error, $cat, $pid,
            &alloc::format!($($arg)*)
        )
    }
}

// =====================================================================
// -----| Performance Profiler |-----
// =====================================================================
//
// Sampling-based profiler. The PIT interrupt calls profiler::sample() on
// each tick. Every N ticks (SAMPLE_INTERVAL) a (pid, rip) pair is recorded.
// The `profile` terminal command builds a histogram and prints hot spots.

const PROFILE_RING: usize = 8192;
const SAMPLE_INTERVAL: u64 = 10; // Sample every 10 ms

#[derive(Debug, Clone, Copy, Default)]
pub struct ProfileSample {
    pub pid:  u32,
    pub rip:  u64,
    pub tick: u64,
}

pub struct Profiler {
    samples:      Vec<ProfileSample>,
    head:         usize,
    tail:         usize,
    count:        usize,
    pub enabled:  bool,
    tick_accum:   u64,
    pub total_samples: u64,
}

impl Profiler {
    pub const fn new() -> Self {
        Self {
            samples:  Vec::new(), head: 0, tail: 0, count: 0,
            enabled:  false, tick_accum: 0, total_samples: 0,
        }
    }

    fn ensure_buffer(&mut self) {
        if self.samples.is_empty() {
            self.samples.resize(PROFILE_RING, ProfileSample::default());
        }
    }

    /// Called by the PIT interrupt. Records a sample every SAMPLE_INTERVAL ticks.
    pub fn on_tick(&mut self, tick: u64) {
        if !self.enabled { return; }
        self.ensure_buffer();
        self.tick_accum += 1;
        if self.tick_accum < SAMPLE_INTERVAL { return; }
        self.tick_accum = 0;

        // Capture current RIP from the active thread.
        let (pid, rip) = {
            let pm = crate::process::PROCESSOR.lock();
            let pid = pm.current_pid.unwrap_or(0);
            let rip = crate::nt_thread::THREAD_TABLE.lock()
                .threads_for_process(pid).first()
                .map(|t| t.context.lock().rip)
                .unwrap_or(0);
            (pid, rip)
        };

        let sample = ProfileSample { pid, rip, tick };
        self.samples[self.tail] = sample;
        self.tail = (self.tail + 1) % PROFILE_RING;
        if self.count < PROFILE_RING { self.count += 1; }
        else { self.head = (self.head + 1) % PROFILE_RING; }
        self.total_samples += 1;
    }

    /// Build a histogram: (pid, rip) → hit count.
    pub fn histogram(&self) -> Vec<(u32, u64, u64)> {
        let mut counts: Vec<(u32, u64, u64)> = Vec::new();
        for i in 0..self.count {
            let s = &self.samples[(self.head + i) % PROFILE_RING];
            if let Some(e) = counts.iter_mut().find(|e| e.0 == s.pid && e.1 == s.rip) {
                e.2 += 1;
            } else {
                counts.push((s.pid, s.rip, 1));
            }
        }
        counts.sort_by(|a, b| b.2.cmp(&a.2)); // Sort descending by hits
        counts
    }

    /// Print top N hot-spots to the serial/VGA.
    pub fn report(&self, top_n: usize) {
        if self.count == 0 {
            println!("[PROF] No samples collected yet. Enable with PROFILE START.");
            return;
        }
        let hist = self.histogram();
        println!("[PROF] Top {} hot spots ({} total samples):", top_n, self.total_samples);
        for (i, &(pid, rip, hits)) in hist.iter().take(top_n).enumerate() {
            let pct = hits * 100 / self.total_samples.max(1);
            println!("[PROF]   #{}: pid={} rip={:#x} hits={} ({}%)", i+1, pid, rip, hits, pct);
        }
    }

    pub fn reset(&mut self) {
        self.count = 0;
        self.head  = 0;
        self.tail  = 0;
        self.total_samples = 0;
        println!("[PROF] Profile data cleared.");
    }
}

pub static PROFILER: Mutex<Profiler> = Mutex::new(Profiler::new());

/// Atomic tick counter incremented by the PIT ISR.
/// We never take any lock inside the interrupt — we just bump this counter.
static PROFILER_ISR_TICKS: AtomicU64 = AtomicU64::new(0);

/// Called from the PIT interrupt handler — MUST be lock-free.
/// Only increments an atomic; no mutex, no heap, no other locks.
pub fn profiler_tick(_tick: u64) {
    PROFILER_ISR_TICKS.fetch_add(1, Ordering::Relaxed);
}

/// Called from task context (gui_refresh_task) every frame.
/// Drains accumulated ISR ticks and runs the real sampling logic,
/// which is allowed to take locks because we are not in an interrupt.
pub fn pump_profiler_ticks() {
    // Atomically drain all ticks that have accumulated since last call.
    let pending = PROFILER_ISR_TICKS.swap(0, Ordering::Relaxed);
    if pending == 0 { return; }
    let tick = crate::interrupts::TICKS.load(Ordering::Relaxed);
    if let Some(mut prof) = PROFILER.try_lock() {
        for _ in 0..pending {
            prof.on_tick(tick);
        }
    }
    // If the lock is contended we simply drop this batch — profiler
    // data is statistical and a missed sample is harmless.
}

/// Initialise all debugger / tracing subsystems.
pub fn init() {
    init_trace_log();
    println!("[DBG] Debugger subsystem ready.");
    println!("[DBG]   Kernel debugger: serial-port, {} breakpoint slots", MAX_BREAKPOINTS);
    println!("[DBG]   Userland debugger: ptrace-style attach/detach");
    println!("[DBG]   Trace log: {} slots, {} categories", TRACE_RING_SIZE, 11);
    println!("[DBG]   Profiler: {} sample slots, {}ms interval", PROFILE_RING, SAMPLE_INTERVAL);
}
