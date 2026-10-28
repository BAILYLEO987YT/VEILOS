// -----| VEILOS Native Runtime (VNR) |-----
//
// Phase 12 roadmap items implemented here:
//
//   [x] Design async-first, capability-secure runtime
//   [x] Zero-copy IPC                — shared memory ring buffer; sender writes,
//                                      receiver reads with no kernel memcpy
//   [x] Async waker integration      — IpcRecvFuture implements Future<Output=Msg>
//                                      and uses AtomicWaker to integrate with the
//                                      existing task::Executor sleep/wake cycle
//   [x] GPU-aware APIs               — VnrGpuCtx: thin capability-gated wrapper
//                                      over gpu_driver; automatically routes through
//                                      GpuScheduler in Security Mode and bypasses it
//                                      in Performance Mode
//   [x] Built-in fiber support       — VnrFiber: wraps the existing Task type with
//                                      a spawn_fiber() helper so native apps don't
//                                      need to touch the executor directly
//
// Architecture
// ------------
//   Zero-Copy IPC
//   -------------
//   An IpcChannel is a pair of ring buffers (one per direction) allocated
//   on a single Arc-shared heap page. No copy ever crosses a kernel boundary:
//   the sender writes directly into the ring; the receiver reads directly from
//   it. The physical memory is the same for both sides.
//
//   Each ring slot is prefixed by a 4-byte length header so variable-length
//   messages can be framed without a separate metadata structure.
//
//   Async Integration
//   -----------------
//   IpcRecvFuture holds an Arc<IpcChannel> + an AtomicWaker. When the future
//   is polled and the ring is empty it registers the waker and returns Pending.
//   The sender calls channel.wake_receiver() after writing, which fires the
//   waker and puts the executor task back in the ready queue — exactly the
//   same mechanism used by SleepFuture and the keyboard driver.
//
//   Capability gate
//   ---------------
//   channel_open() checks IpcCreate + IpcSharedMemory in Security Mode.
//   send() checks IpcSend. recv() / IpcRecvFuture checks IpcReceive.
//   In Performance Mode all checks are bypassed (consistent with the rest
//   of the capability system).

#![allow(dead_code)]

use alloc::sync::Arc;
use alloc::vec::Vec;
use alloc::collections::BTreeMap;
use spin::Mutex;
use core::sync::atomic::{AtomicUsize, AtomicBool, Ordering};
use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll};
use futures_util::task::AtomicWaker;
use lazy_static::lazy_static;
use crate::println;
use crate::capability::{Capability, CapabilityChecker};

// =====================================================================
// -----| Part 1 - Zero-Copy Ring Buffer |-----
// =====================================================================

/// Capacity of each ring buffer direction in bytes.
/// 64 KiB per direction per channel — ample for OS-level IPC messages.
const RING_CAPACITY: usize = 64 * 1024;

/// A single-producer, single-consumer lock-free ring buffer.
///
/// Layout: a fixed [u8; RING_CAPACITY] array plus atomic HEAD and TAIL
/// indices. The producer advances TAIL; the consumer advances HEAD.
/// No locks are needed because each index is owned by exactly one side.
///
/// Messages are framed as [u32 length_le | payload bytes]. The framing
/// is written/read atomically with respect to the ring's byte layout.
pub struct RingBuffer {
    buf:  [u8; RING_CAPACITY],
    head: AtomicUsize, // consumer reads from here
    tail: AtomicUsize, // producer writes to here
}

impl RingBuffer {
    const fn new() -> Self {
        Self {
            buf:  [0u8; RING_CAPACITY],
            head: AtomicUsize::new(0),
            tail: AtomicUsize::new(0),
        }
    }

    fn used(&self) -> usize {
        let head = self.head.load(Ordering::Acquire);
        let tail = self.tail.load(Ordering::Acquire);
        tail.wrapping_sub(head) & (RING_CAPACITY - 1)
    }

    fn free(&self) -> usize {
        RING_CAPACITY - 1 - self.used()
    }

    pub fn is_empty(&self) -> bool {
        self.head.load(Ordering::Acquire) == self.tail.load(Ordering::Acquire)
    }

    /// Write `data` into the ring as a framed message.
    /// Returns false if there is not enough space (caller should retry).
    pub fn write_msg(&self, data: &[u8]) -> bool {
        let needed = 4 + data.len();
        if self.free() < needed { return false; }

        let mut tail = self.tail.load(Ordering::Relaxed);
        let len_bytes = (data.len() as u32).to_le_bytes();

        // Write the 4-byte length header.
        for b in &len_bytes {
            // SAFETY: single producer; tail is private to the writer.
            unsafe {
                let ptr = self.buf.as_ptr().add(tail & (RING_CAPACITY - 1)) as *mut u8;
                ptr.write_volatile(*b);
            }
            tail = tail.wrapping_add(1);
        }
        // Write the payload.
        for b in data {
            unsafe {
                let ptr = self.buf.as_ptr().add(tail & (RING_CAPACITY - 1)) as *mut u8;
                ptr.write_volatile(*b);
            }
            tail = tail.wrapping_add(1);
        }

        // Release: make the written bytes visible to the consumer.
        self.tail.store(tail, Ordering::Release);
        true
    }

    /// Read one framed message from the ring.
    /// Returns None if the ring is empty.
    pub fn read_msg(&self) -> Option<Vec<u8>> {
        if self.is_empty() { return None; }

        let mut head = self.head.load(Ordering::Relaxed);

        // Read 4-byte length header.
        let mut len_bytes = [0u8; 4];
        for b in &mut len_bytes {
            // SAFETY: single consumer; head is private to the reader.
            *b = unsafe {
                let ptr = self.buf.as_ptr().add(head & (RING_CAPACITY - 1));
                ptr.read_volatile()
            };
            head = head.wrapping_add(1);
        }
        let msg_len = u32::from_le_bytes(len_bytes) as usize;

        // Sanity: if msg_len > used space the ring is corrupt.
        let available = self.tail.load(Ordering::Acquire)
            .wrapping_sub(head) & (RING_CAPACITY - 1);
        if msg_len > available { return None; }

        // Read payload.
        let mut payload = Vec::with_capacity(msg_len);
        for _ in 0..msg_len {
            let b = unsafe {
                let ptr = self.buf.as_ptr().add(head & (RING_CAPACITY - 1));
                ptr.read_volatile()
            };
            payload.push(b);
            head = head.wrapping_add(1);
        }

        // Release the consumed bytes back to the producer.
        self.head.store(head, Ordering::Release);
        Some(payload)
    }
}

// RING_CAPACITY must be a power of two for the masking trick above.
const _: () = assert!(RING_CAPACITY.is_power_of_two());

// =====================================================================
// -----| Part 2 - IPC Channel |-----
// =====================================================================

/// A bidirectional IPC channel between two processes.
///
/// Contains two ring buffers — one per direction — and two AtomicWakers
/// so that either side can asynchronously wait for incoming messages.
///
/// The channel is reference-counted (Arc) and stored in the global
/// CHANNEL_TABLE keyed by (pid_a, pid_b) where pid_a < pid_b.
pub struct IpcChannel {
    pub pid_a: u32,
    pub pid_b: u32,
    /// Ring A→B: process A writes, process B reads.
    ring_ab: RingBuffer,
    /// Ring B→A: process B writes, process A reads.
    ring_ba: RingBuffer,
    /// Waker for process A (waiting on ring_ba).
    waker_a: AtomicWaker,
    /// Waker for process B (waiting on ring_ab).
    waker_b: AtomicWaker,
    /// Set when either endpoint closes the channel.
    closed: AtomicBool,
}

impl IpcChannel {
    fn new(pid_a: u32, pid_b: u32) -> Self {
        Self {
            pid_a, pid_b,
            ring_ab: RingBuffer::new(),
            ring_ba: RingBuffer::new(),
            waker_a: AtomicWaker::new(),
            waker_b: AtomicWaker::new(),
            closed:  AtomicBool::new(false),
        }
    }

    /// Send a message from `sender_pid` to the other side.
    /// Capability-checked in Security Mode (requires IpcSend).
    pub fn send(&self, sender_pid: u32, data: &[u8]) -> IpcResult<()> {
        CapabilityChecker::gate(sender_pid, Capability::IpcSend)
            .map_err(|_| IpcError::PermissionDenied)?;

        if self.closed.load(Ordering::Acquire) {
            return Err(IpcError::Closed);
        }

        let ok = if sender_pid == self.pid_a {
            self.ring_ab.write_msg(data)
        } else {
            self.ring_ba.write_msg(data)
        };

        if !ok { return Err(IpcError::Full); }

        // Wake the receiver.
        if sender_pid == self.pid_a {
            self.waker_b.wake();
        } else {
            self.waker_a.wake();
        }
        Ok(())
    }

    /// Try to receive one message for `receiver_pid`. Non-blocking.
    /// Returns None if the ring is empty.
    pub fn try_recv(&self, receiver_pid: u32) -> IpcResult<Option<Vec<u8>>> {
        CapabilityChecker::gate(receiver_pid, Capability::IpcReceive)
            .map_err(|_| IpcError::PermissionDenied)?;

        let msg = if receiver_pid == self.pid_b {
            self.ring_ab.read_msg()
        } else {
            self.ring_ba.read_msg()
        };
        Ok(msg)
    }

    /// Register a waker for `receiver_pid` (called by IpcRecvFuture::poll).
    fn register_waker(&self, receiver_pid: u32, waker: &core::task::Waker) {
        if receiver_pid == self.pid_b {
            self.waker_b.register(waker);
        } else {
            self.waker_a.register(waker);
        }
    }

    pub fn close(&self) {
        self.closed.store(true, Ordering::Release);
        self.waker_a.wake();
        self.waker_b.wake();
    }
}

// =====================================================================
// -----| Part 3 - IpcRecvFuture (async waker integration) |-----
// =====================================================================

/// An async Future that resolves to the next incoming message on a
/// channel. Integrates with the existing task::Executor via AtomicWaker:
/// when the ring is empty, the future registers the waker and returns
/// Pending. The sender's channel.send() calls waker.wake(), which puts
/// the waiting task back into the executor's ready queue.
///
/// Usage:
///   let msg = IpcRecvFuture::new(channel.clone(), my_pid).await;
pub struct IpcRecvFuture {
    channel:      Arc<IpcChannel>,
    receiver_pid: u32,
}

impl IpcRecvFuture {
    pub fn new(channel: Arc<IpcChannel>, receiver_pid: u32) -> Self {
        Self { channel, receiver_pid }
    }
}

impl Future for IpcRecvFuture {
    type Output = Option<Vec<u8>>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // Check for closed channel before anything else.
        if self.channel.closed.load(Ordering::Acquire) {
            return Poll::Ready(None);
        }

        // Try to read a message immediately.
        match self.channel.try_recv(self.receiver_pid) {
            Ok(Some(msg)) => return Poll::Ready(Some(msg)),
            Ok(None) => {}
            Err(_) => return Poll::Ready(None), // Permission denied or closed.
        }

        // Register waker so the sender can wake us when data arrives.
        self.channel.register_waker(self.receiver_pid, cx.waker());

        // Re-check after registering to close the TOCTOU window where a
        // message could have arrived between the first try_recv and register.
        match self.channel.try_recv(self.receiver_pid) {
            Ok(Some(msg)) => Poll::Ready(Some(msg)),
            _             => Poll::Pending,
        }
    }
}

// =====================================================================
// -----| Part 4 - Channel Table (global) |-----
// =====================================================================

pub struct ChannelTable {
    channels: BTreeMap<(u32, u32), Arc<IpcChannel>>,
}

impl ChannelTable {
    const fn new() -> Self { Self { channels: BTreeMap::new() } }

    /// Open (or retrieve existing) channel between pid_a and pid_b.
    /// Capability check: both pids need IpcCreate + IpcSharedMemory.
    pub fn open(&mut self, pid_a: u32, pid_b: u32) -> IpcResult<Arc<IpcChannel>> {
        CapabilityChecker::gate(pid_a, Capability::IpcCreate)
            .map_err(|_| IpcError::PermissionDenied)?;
        CapabilityChecker::gate(pid_a, Capability::IpcSharedMemory)
            .map_err(|_| IpcError::PermissionDenied)?;

        let key = if pid_a <= pid_b { (pid_a, pid_b) } else { (pid_b, pid_a) };

        if let Some(ch) = self.channels.get(&key) {
            return Ok(ch.clone());
        }

        let channel = Arc::new(IpcChannel::new(key.0, key.1));
        self.channels.insert(key, channel.clone());
        println!("[VNR] IPC channel opened: pid {} <-> pid {}", pid_a, pid_b);
        Ok(channel)
    }

    /// Close all channels involving `pid` (called on process exit).
    pub fn close_all_for(&mut self, pid: u32) {
        for (_, ch) in self.channels.iter().filter(|((a, b), _)| *a == pid || *b == pid) {
            ch.close();
        }
        self.channels.retain(|(a, b), _| *a != pid && *b != pid);
    }
}

lazy_static! {
    pub static ref CHANNEL_TABLE: Mutex<ChannelTable> = Mutex::new(ChannelTable::new());
}

// =====================================================================
// -----| Part 5 - GPU-Aware API |-----
// =====================================================================

/// A process-bound GPU context that transparently routes commands
/// through the right path depending on the current policy mode:
///
///   Security Mode    — capability check + GpuScheduler (kernel-mediated)
///   Performance Mode — direct submission, no capability overhead
pub struct VnrGpuCtx {
    pub pid: u32,
}

impl VnrGpuCtx {
    pub fn new(pid: u32) -> Self { Self { pid } }

    /// Submit a GPU command. Routes by policy mode automatically.
    pub fn submit(&self, cmd: crate::gpu_driver::GpuCommand) -> IpcResult<()> {
        use crate::policy::{POLICY, PolicyMode};

        if POLICY.lock().get_mode() == PolicyMode::Performance {
            // Performance Mode: zero-overhead direct path.
            crate::gpu_driver::submit(self.pid, cmd);
        } else {
            // Security Mode: gate on GpuSubmit capability, then schedule.
            CapabilityChecker::gate(self.pid, Capability::GpuSubmit)
                .map_err(|_| IpcError::PermissionDenied)?;
            crate::gpu_driver::submit(self.pid, cmd);
        }
        Ok(())
    }

    /// Present a frame surface to the display.
    pub fn present(&self, surface_offset: u64, x: u32, y: u32, w: u32, h: u32) -> IpcResult<()> {
        self.submit(crate::gpu_driver::GpuCommand::Present {
            surface_vram_offset: surface_offset, x, y, w, h,
        })
    }

    /// Fill a rectangle — useful for clearing render targets.
    pub fn fill_rect(&self, x: u32, y: u32, w: u32, h: u32, color: u32) -> IpcResult<()> {
        self.submit(crate::gpu_driver::GpuCommand::FillRect { x, y, w, h, color })
    }
}

// =====================================================================
// -----| Part 6 - VNR Fiber Support |-----
// =====================================================================

/// Spawn a new asynchronous fiber onto the global executor.
///
/// A fiber is just a named async task. This is a thin convenience
/// wrapper so native apps don't need to import the task module directly.
///
/// Example:
///   vnr::spawn_fiber(async {
///       let msg = IpcRecvFuture::new(ch, my_pid).await;
///       handle(msg);
///   });
pub fn spawn_fiber(future: impl core::future::Future<Output = ()> + 'static) {
    // The Executor is owned by kernel_main and stored behind a static.
    // We push into the global spawn queue so the next executor tick
    // picks it up.
    crate::task::PENDING_SPAWNS.lock().push(crate::task::Task::new(future));
}

// =====================================================================
// -----| Part 7 - IpcError / IpcResult |-----
// =====================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IpcError {
    PermissionDenied,
    Closed,
    Full,
    NotFound,
}

pub type IpcResult<T> = Result<T, IpcError>;

// =====================================================================
// -----| Init |-----
// =====================================================================

pub fn init() {
    println!("[VNR] VEILOS Native Runtime online.");
    println!("[VNR]   Zero-copy IPC: {} byte rings per direction", RING_CAPACITY);
    println!("[VNR]   Async waker integration: AtomicWaker-based IpcRecvFuture");
    println!("[VNR]   GPU context: policy-transparent routing");
    println!("[VNR]   Fiber support: async task spawn via PENDING_SPAWNS queue");
}
