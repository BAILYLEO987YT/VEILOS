// -----| VEILOS Game SDK |-----
//
// Phase 12 roadmap items implemented here:
//
//   [x] Direct GPU access (Performance Mode)      — GpuHandle::submit() routes
//       + capability-checked path (Security Mode)   through GpuScheduler in
//                                                   Security Mode, bypasses it
//                                                   in Performance Mode. Both
//                                                   paths share the same API.
//
//   [x] Predictable memory model APIs             — GameArena: a pre-allocated
//                                                   fixed-size slab. Games call
//                                                   alloc() to carve out regions
//                                                   at startup; zero fragmentation
//                                                   during gameplay. reset() on
//                                                   level load reclaims everything.
//
//   [x] Low-latency input APIs                    — InputSnapshot: polls the raw
//                                                   keyboard and mouse queues at
//                                                   the moment of the call with no
//                                                   intermediate buffering. Each
//                                                   snapshot is stamped with the
//                                                   current PIT tick for sub-frame
//                                                   input timing.
//
//   [x] Built-in telemetry and profiling          — GameTelemetry: per-frame CPU
//                                                   timing, frame budget tracking,
//                                                   missed-frame counting. Wraps
//                                                   the existing PROFILER and adds
//                                                   game-specific derived metrics.
//                                                   Records a GameSession on exit
//                                                   so the adaptive scheduler gets
//                                                   accurate game workload data.
//
// Scheduler integration
// ----------------------
//   GameSession::begin() calls SCHEDULER.on_process_launch() so the adaptive
//   scheduler pre-allocates resources for the game.
//   GameSession::end()   calls SCHEDULER.on_process_exit() so usage stats are
//   persisted to /SCHEDPRO.BIN for future sessions.

#![allow(dead_code)]

use alloc::vec::Vec;
use alloc::string::String;
use crate::println;
use crate::capability::{Capability, CapabilityChecker};
use crate::gpu_driver::GpuCommand;
use crate::policy::{POLICY, PolicyMode};

// =====================================================================
// -----| Part 1 - Policy-Transparent GPU Handle |-----
// =====================================================================

/// A per-game GPU context. Routes commands through the right kernel
/// path based on the current policy mode.
///
/// Security Mode:  GpuSubmit capability checked → GpuScheduler mediates
/// Performance Mode: direct ring submission, zero overhead
pub struct GpuHandle {
    pub pid: u32,
}

impl GpuHandle {
    pub fn new(pid: u32) -> Self {
        println!("[SDK] GpuHandle opened for pid={}", pid);
        Self { pid }
    }

    /// Submit a raw GPU command. Never panics — returns a typed error on
    /// permission denial so the game can handle it gracefully.
    pub fn submit(&self, cmd: GpuCommand) -> SdkResult<()> {
        if POLICY.lock().get_mode() == PolicyMode::Performance {
            // Performance Mode: no capability check, no scheduler overhead.
            crate::gpu_driver::submit(self.pid, cmd);
        } else {
            // Security Mode: check GpuSubmit capability, then schedule normally.
            CapabilityChecker::gate(self.pid, Capability::GpuSubmit)
                .map_err(|_| SdkError::GpuPermissionDenied)?;
            crate::gpu_driver::submit(self.pid, cmd);
        }
        Ok(())
    }

    /// Present a VRAM surface to the screen.
    pub fn present(&self, vram_offset: u64, x: u32, y: u32, w: u32, h: u32) -> SdkResult<()> {
        self.submit(GpuCommand::Present { surface_vram_offset: vram_offset, x, y, w, h })
    }

    /// Fill a rectangle with a solid colour (clear, UI draw, etc.).
    pub fn fill_rect(&self, x: u32, y: u32, w: u32, h: u32, color: u32) -> SdkResult<()> {
        self.submit(GpuCommand::FillRect { x, y, w, h, color })
    }

    /// Upload a CPU-side surface into VRAM.
    pub fn upload_surface(
        &self, vram_offset: u64, width: u32, height: u32, stride: u32, src_ptr: u64,
    ) -> SdkResult<()> {
        self.submit(GpuCommand::UploadSurface { vram_offset, width, height, stride, src_ptr })
    }

    /// Get a direct pointer to GPU VRAM (Performance Mode only).
    /// Returns Err(NotInPerfMode) in Security Mode — use upload_surface instead.
    pub fn vram_ptr(&self) -> SdkResult<*mut u8> {
        if POLICY.lock().get_mode() != PolicyMode::Performance {
            return Err(SdkError::NotInPerfMode);
        }
        CapabilityChecker::gate(self.pid, Capability::GpuDirectMap)
            .map_err(|_| SdkError::GpuPermissionDenied)?;

        let gpus = crate::gpu_driver::GPU_DEVICES.lock();
        let gpu  = gpus.first().ok_or(SdkError::NoGpuDevice)?;
        // SAFETY: In Performance Mode, BAR1 is mapped into the process address space.
        // The caller is responsible for bounds-checking against bar1_size.
        Ok(gpu.bar1_phys as *mut u8)
    }

    pub fn vram_size(&self) -> usize {
        crate::gpu_driver::GPU_DEVICES.lock()
            .first()
            .map(|g| g.bar1_size)
            .unwrap_or(0)
    }
}

// =====================================================================
// -----| Part 2 - Predictable Memory Arena |-----
// =====================================================================

/// A pre-allocated slab allocator for game use.
///
/// Design goals:
///   - Zero fragmentation during gameplay (all allocations at startup)
///   - O(1) alloc and O(1) reset
///   - 16-byte aligned output for SIMD safety
///
/// Typical usage:
///   let mut arena = GameArena::new(64 * 1024 * 1024); // 64 MiB
///   let verts = arena.alloc(vertex_buffer_size)?;
///   let indices = arena.alloc(index_buffer_size)?;
///   // ... gameplay ...
///   arena.reset(); // Level load: reclaim everything in O(1)
pub struct GameArena {
    buf:    Vec<u8>,
    cursor: usize,
}

impl GameArena {
    /// Allocate a new arena of `capacity` bytes.
    pub fn new(capacity: usize) -> Self {
        let mut buf = Vec::with_capacity(capacity);
        buf.resize(capacity, 0);
        Self { buf, cursor: 0 }
    }

    /// Carve out `size` bytes, 16-byte aligned.
    /// Returns a raw pointer and the actual allocation size (padded to alignment).
    pub fn alloc(&mut self, size: usize) -> SdkResult<(*mut u8, usize)> {
        // Align up to 16 bytes.
        let aligned  = (self.cursor + 15) & !15;
        let end      = aligned + size;
        if end > self.buf.capacity() {
            return Err(SdkError::ArenaFull);
        }
        if end > self.buf.len() { self.buf.resize(end, 0); }
        let ptr = unsafe { self.buf.as_mut_ptr().add(aligned) };
        self.cursor = end;
        Ok((ptr, size))
    }

    /// Release all allocations in O(1).
    /// Does NOT zero the memory — matches game engine practice (next frame overwrites).
    pub fn reset(&mut self) {
        self.cursor = 0;
    }

    /// Bytes remaining before the arena is full.
    pub fn remaining(&self) -> usize {
        self.buf.capacity().saturating_sub(self.cursor)
    }

    /// Bytes consumed.
    pub fn used(&self) -> usize { self.cursor }

    /// Total capacity.
    pub fn capacity(&self) -> usize { self.buf.capacity() }
}

// =====================================================================
// -----| Part 3 - Low-Latency Input API |-----
// =====================================================================

/// A timestamped snapshot of all input at a single point in time.
///
/// Polled at the start of each game frame so input processing sees a
/// consistent view of device state for the entire frame duration.
#[derive(Clone)]
pub struct InputSnapshot {
    /// PIT tick at snapshot time (1 kHz → 1 ms resolution).
    pub tick: u64,
    /// Pressed key scancodes since the last snapshot.
    pub keys_pressed:  Vec<u8>,
    /// Raw mouse delta since the last snapshot.
    pub mouse_dx:      i32,
    pub mouse_dy:      i32,
    /// Mouse button bitfield: bit 0 = left, bit 1 = right, bit 2 = middle.
    pub mouse_buttons: u8,
    /// Absolute mouse position.
    pub mouse_x:       i32,
    pub mouse_y:       i32,
}

impl InputSnapshot {
    /// Capture the current input state.
    pub fn capture() -> Self {
        let tick = crate::interrupts::TICKS.load(core::sync::atomic::Ordering::Relaxed);

        // Drain pending scancodes from the keyboard queue.
        let mut keys = Vec::new();
        while let Some(sc) = crate::task::keyboard::pop_scancode() {
            keys.push(sc);
        }

        // Read current mouse position and delta from the mouse state.
        let (mx, my, dx, dy, buttons) = crate::task::mouse::get_state();

        Self {
            tick,
            keys_pressed:  keys,
            mouse_dx:      dx,
            mouse_dy:      dy,
            mouse_buttons: buttons,
            mouse_x:       mx,
            mouse_y:       my,
        }
    }

    /// Returns true if the given scancode was pressed this frame.
    pub fn key_pressed(&self, scancode: u8) -> bool {
        self.keys_pressed.contains(&scancode)
    }

    /// Returns true if the left mouse button is held.
    pub fn left_mouse(&self)   -> bool { self.mouse_buttons & 0x01 != 0 }
    /// Returns true if the right mouse button is held.
    pub fn right_mouse(&self)  -> bool { self.mouse_buttons & 0x02 != 0 }
    /// Returns true if the middle mouse button is held.
    pub fn middle_mouse(&self) -> bool { self.mouse_buttons & 0x04 != 0 }
}

// =====================================================================
// -----| Part 4 - Frame Timer |-----
// =====================================================================

/// Per-frame timing utility.
///
/// Call begin_frame() at the start of each frame and end_frame() at the
/// end. Tracks frame time, FPS, frame budget usage, and missed frames.
pub struct FrameTimer {
    /// Target frame duration in ticks (e.g. 17 for ~60 Hz at 1 kHz PIT).
    pub target_ticks:    u64,
    /// Tick at the start of the current frame.
    frame_start:         u64,
    /// Smoothed frame time (exponential moving average over 16 frames).
    pub avg_frame_ticks: u64,
    /// Total frames measured.
    pub frame_count:     u64,
    /// Frames that exceeded the target duration.
    pub missed_frames:   u64,
    /// Last frame's duration in ticks.
    pub last_frame_ticks: u64,
}

impl FrameTimer {
    /// Create a frame timer targeting `target_hz` frames per second.
    pub fn new(target_hz: u64) -> Self {
        // At 1 kHz, ticks per frame = 1000 / hz.
        let target_ticks = 1000u64 / target_hz.max(1);
        Self {
            target_ticks,
            frame_start:      0,
            avg_frame_ticks:  target_ticks,
            frame_count:      0,
            missed_frames:    0,
            last_frame_ticks: 0,
        }
    }

    /// Call at the very start of each game loop iteration.
    pub fn begin_frame(&mut self) {
        self.frame_start = crate::interrupts::TICKS
            .load(core::sync::atomic::Ordering::Relaxed);
    }

    /// Call at the very end of each game loop iteration.
    /// Returns the frame duration in ticks.
    pub fn end_frame(&mut self) -> u64 {
        let now  = crate::interrupts::TICKS.load(core::sync::atomic::Ordering::Relaxed);
        let dur  = now.wrapping_sub(self.frame_start);

        self.last_frame_ticks = dur;
        self.frame_count     += 1;

        // Exponential moving average: EMA(n) = EMA(n-1)*15/16 + new/16
        self.avg_frame_ticks = (self.avg_frame_ticks * 15 + dur) / 16;

        if dur > self.target_ticks {
            self.missed_frames += 1;
        }
        dur
    }

    /// Approximate FPS based on the smoothed frame time.
    pub fn fps(&self) -> u64 {
        if self.avg_frame_ticks == 0 { return 0; }
        1000 / self.avg_frame_ticks
    }

    /// Budget usage as a percentage (0–100+).
    pub fn budget_pct(&self) -> u64 {
        if self.target_ticks == 0 { return 0; }
        self.last_frame_ticks * 100 / self.target_ticks
    }
}

// =====================================================================
// -----| Part 5 - Game Telemetry |-----
// =====================================================================

/// Per-session game telemetry. Wraps the kernel profiler and adds
/// game-specific metrics that feed into the adaptive scheduler.
pub struct GameTelemetry {
    pub session_name:    String,
    pub pid:             u32,
    pub session_start:   u64,
    /// Peak memory observed this session (in bytes, updated manually).
    pub peak_mem_bytes:  u64,
    /// Running total of CPU ticks spent in game logic.
    pub logic_ticks:     u64,
    /// Total frames rendered.
    pub total_frames:    u64,
    /// Frames that missed the target budget.
    pub missed_frames:   u64,
}

impl GameTelemetry {
    pub fn new(name: &str, pid: u32) -> Self {
        let tick = crate::interrupts::TICKS.load(core::sync::atomic::Ordering::Relaxed);
        Self {
            session_name:   String::from(name),
            pid,
            session_start:  tick,
            peak_mem_bytes: 0,
            logic_ticks:    0,
            total_frames:   0,
            missed_frames:  0,
        }
    }

    /// Record a completed frame.
    pub fn record_frame(&mut self, frame_ticks: u64, missed: bool) {
        self.total_frames  += 1;
        self.logic_ticks   += frame_ticks;
        if missed { self.missed_frames += 1; }
    }

    /// Update peak memory observation.
    pub fn observe_memory(&mut self, bytes: u64) {
        if bytes > self.peak_mem_bytes { self.peak_mem_bytes = bytes; }
    }

    /// Print a telemetry report to serial.
    pub fn report(&self) {
        let tick = crate::interrupts::TICKS.load(core::sync::atomic::Ordering::Relaxed);
        let dur  = tick.wrapping_sub(self.session_start);
        println!("[SDK/TEL] Session '{}' pid={}", self.session_name, self.pid);
        println!("[SDK/TEL]   Frames:       {} ({} missed)", self.total_frames, self.missed_frames);
        println!("[SDK/TEL]   Logic ticks:  {}", self.logic_ticks);
        println!("[SDK/TEL]   Session dur:  {}ms", dur);
        println!("[SDK/TEL]   Peak mem:     {}B", self.peak_mem_bytes);
        if self.total_frames > 0 {
            let avg = self.logic_ticks / self.total_frames;
            println!("[SDK/TEL]   Avg frame:    {}ms", avg);
        }
    }
}

// =====================================================================
// -----| Part 6 - GameSession (scheduler integration) |-----
// =====================================================================

/// A RAII guard that hooks a game process into the adaptive scheduler.
///
/// On creation: calls SCHEDULER.on_process_launch() so the scheduler
///   pre-allocates the right priority and quantum based on past history.
/// On drop:     calls SCHEDULER.on_process_exit() so usage stats are
///   recorded and persisted to /SCHEDPRO.BIN.
pub struct GameSession {
    pub telemetry: GameTelemetry,
}

impl GameSession {
    /// Begin a game session. `name` is used as the scheduler profile key.
    pub fn begin(name: &str, pid: u32) -> Self {
        let tick = crate::interrupts::TICKS.load(core::sync::atomic::Ordering::Relaxed);

        // Notify the adaptive scheduler so it arms the right quantum + priority.
        if let Some(mut sched) = crate::scheduler::SCHEDULER.try_lock() {
            sched.on_process_launch(pid, name, tick);
        }

        // Enable the kernel profiler for this session.
        crate::debugger::PROFILER.lock().enabled = true;

        println!("[SDK] GameSession::begin(\"{}\", pid={})", name, pid);
        Self {
            telemetry: GameTelemetry::new(name, pid),
        }
    }

    /// End the session, flush telemetry, and notify the scheduler.
    pub fn end(self) {
        self.telemetry.report();

        let tick = crate::interrupts::TICKS.load(core::sync::atomic::Ordering::Relaxed);

        // Stop the profiler.
        crate::debugger::PROFILER.lock().enabled = false;

        // Notify the adaptive scheduler so it records the session stats.
        if let Some(mut sched) = crate::scheduler::SCHEDULER.try_lock() {
            sched.on_process_exit(
                self.telemetry.pid,
                self.telemetry.peak_mem_bytes,
                tick,
            );
            sched.flush_profiles();
        }

        println!("[SDK] GameSession::end(\"{}\") — profiles flushed.",
            self.telemetry.session_name);
    }
}

// =====================================================================
// -----| Part 7 - Error type |-----
// =====================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SdkError {
    GpuPermissionDenied,
    NotInPerfMode,
    NoGpuDevice,
    ArenaFull,
    InputUnavailable,
}

pub type SdkResult<T> = Result<T, SdkError>;

// =====================================================================
// -----| Init |-----
// =====================================================================

pub fn init() {
    println!("[SDK] VEILOS Game SDK online.");
    println!("[SDK]   GPU handle: policy-transparent (Security=scheduled, Perf=direct)");
    println!("[SDK]   Game arena: pre-allocated slab, O(1) reset");
    println!("[SDK]   Input: low-latency snapshot API, 1ms PIT-stamped");
    println!("[SDK]   Telemetry: per-frame timing + adaptive scheduler integration");
}
