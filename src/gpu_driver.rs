// -----| VEILOS GPU Driver Model |-----
//
// Phase 6 roadmap items implemented here:
//
//   [x] Implement a user-mode GPU driver interface
//   [x] Add kernel-mode GPU scheduler (or bypass in Performance Mode)
//   [x] Map GPU VRAM into user or kernel space depending on mode
//   [x] Implement command submission queues
//
// Architecture
// ------------
//   GpuDevice
//     One per physical GPU discovered during PCI enumeration. Holds the BAR
//     addresses, a VRAM allocator, and the hardware command ring.
//
//   CommandRing  (kernel-mode ring buffer)
//     A fixed-size circular buffer of GpuCommand entries backed by a
//     contiguous memory region. The GPU reads from HEAD; the driver writes
//     to TAIL. In a real GPU this maps directly to a hardware DMA ring
//     (e.g. AMD CP, Intel EXECLIST, Nvidia PUSHBUF). Here we model the
//     data structure faithfully and call process_ring() from the scheduler.
//
//   SubmissionQueue  (per-process user-mode interface)
//     Each process that opens the GPU gets a SubmissionQueue. User code
//     records GpuCommands into it; the kernel scheduler periodically
//     drains all queues into the hardware CommandRing.
//
//   GpuScheduler
//     Iterates all live SubmissionQueues in round-robin order and copies
//     their commands into the CommandRing. In Performance Mode the
//     scheduler is bypassed and user commands are submitted directly
//     (no context-switch overhead).
//
//   VRAM Allocator  (block allocator over GPU BAR1)
//     Simple first-fit block allocator over the GPU's BAR1 aperture.
//     Allocations are page-aligned (4 KiB). Each allocation is backed by
//     a VramBlock describing its physical GPU address.
//
//   Policy integration
//     Security Mode:  VRAM is NOT accessible from user space. All draw
//                     calls must pass through the kernel command ring.
//     Performance Mode: VRAM BAR1 is mapped directly into the process's
//                       virtual address space. The SubmissionQueue writes
//                       commands directly to a mapped ring buffer in VRAM.

#![allow(dead_code)]

use alloc::vec::Vec;
use alloc::sync::Arc;
use alloc::string::String;
use spin::Mutex;
use core::sync::atomic::{AtomicU64, Ordering};
use crate::println;
use crate::policy::PolicyMode;

// =====================================================================
// -----| GPU Command Encoding |-----
// =====================================================================

/// An individual GPU command. In a real driver this would be a hardware
/// packet (e.g. AMD PM4, Intel MI_BATCH_BUFFER). We use a high-level
/// tagged enum so the translator layer above can emit portable commands.
#[derive(Debug, Clone)]
pub enum GpuCommand {
    // ----- 2D rasterisation -----
    /// Fill a rectangle with a solid colour. Fast path — uses the GPU's
    /// BLT engine (or framebuffer memset in our BGA model).
    FillRect { x: u32, y: u32, w: u32, h: u32, color: u32 },

    /// Copy a rectangular region from src to dst (BitBlt).
    BitBlt   { src_x: u32, src_y: u32, dst_x: u32, dst_y: u32, w: u32, h: u32 },

    /// Upload a CPU-side pixel buffer into a VRAM surface.
    UploadSurface { vram_offset: u64, width: u32, height: u32, stride: u32, src_ptr: u64 },

    /// Present a VRAM surface to the display (flip swapchain).
    Present  { surface_vram_offset: u64, x: u32, y: u32, w: u32, h: u32 },

    // ----- 3D / shader pipeline (stubbed for DX→VK translation) -----
    /// Set the active vertex shader (SPIR-V blob offset in VRAM).
    SetVertexShader   { spirv_vram_offset: u64 },
    /// Set the active fragment shader (SPIR-V blob offset in VRAM).
    SetFragmentShader { spirv_vram_offset: u64 },
    /// Bind a vertex buffer from VRAM.
    BindVertexBuffer  { vram_offset: u64, stride: u32 },
    /// Bind an index buffer from VRAM.
    BindIndexBuffer   { vram_offset: u64, index_type: u8 }, // 0=u16 1=u32
    /// Draw indexed primitives.
    DrawIndexed       { index_count: u32, instance_count: u32, first_index: u32 },
    /// Draw non-indexed primitives.
    Draw              { vertex_count: u32, instance_count: u32, first_vertex: u32 },
    /// Bind a descriptor set (texture/sampler/UBO bundle).
    BindDescriptorSet { set_vram_offset: u64 },
    /// Set viewport transform.
    SetViewport       { x: f32, y: f32, w: f32, h: f32, min_depth: f32, max_depth: f32 },
    /// Set scissor rectangle.
    SetScissor        { x: u32, y: u32, w: u32, h: u32 },
    /// Begin a render pass targeting a VRAM surface.
    BeginRenderPass   { surface_vram_offset: u64, clear_color: u32, clear_depth: f32 },
    /// End the current render pass.
    EndRenderPass,
    /// Memory barrier / pipeline flush.
    PipelineBarrier   { src_stage: u32, dst_stage: u32 },
    /// No-op / padding.
    Nop,
}

// =====================================================================
// -----| Command Ring (hardware-facing ring buffer) |-----
// =====================================================================

const RING_CAPACITY: usize = 4096; // Number of command slots

pub struct CommandRing {
    buffer: Vec<Option<GpuCommand>>,
    head:   usize, // GPU read pointer
    tail:   usize, // CPU write pointer
    count:  usize,
}

impl CommandRing {
    pub fn new() -> Self {
        let mut buffer = Vec::with_capacity(RING_CAPACITY);
        for _ in 0..RING_CAPACITY { buffer.push(None); }
        Self { buffer, head: 0, tail: 0, count: 0 }
    }

    /// Enqueue one command. Returns false if the ring is full.
    pub fn push(&mut self, cmd: GpuCommand) -> bool {
        if self.count >= RING_CAPACITY { return false; }
        self.buffer[self.tail] = Some(cmd);
        self.tail = (self.tail + 1) % RING_CAPACITY;
        self.count += 1;
        true
    }

    /// Dequeue one command (GPU consumption). Returns None if empty.
    pub fn pop(&mut self) -> Option<GpuCommand> {
        if self.count == 0 { return None; }
        let cmd = self.buffer[self.head].take();
        self.head = (self.head + 1) % RING_CAPACITY;
        self.count -= 1;
        cmd
    }

    pub fn is_empty(&self) -> bool { self.count == 0 }
    pub fn is_full(&self)  -> bool { self.count >= RING_CAPACITY }
    pub fn pending(&self)  -> usize { self.count }
}

// =====================================================================
// -----| VRAM Block Allocator |-----
// =====================================================================

pub struct VramBlock {
    pub gpu_phys:  u64,   // GPU-physical address (relative to BAR1 base)
    pub size:      usize,
    pub in_use:    bool,
    pub debug_tag: String,
}

pub struct VramAllocator {
    pub bar1_base:    u64,   // CPU-physical address of BAR1 aperture
    pub bar1_size:    usize, // Total VRAM aperture size
    pub blocks:       Vec<VramBlock>,
}

impl VramAllocator {
    pub fn new(bar1_base: u64, bar1_size: usize) -> Self {
        let blocks = alloc::vec![VramBlock {
            gpu_phys: 0, size: bar1_size, in_use: false, debug_tag: String::new(),
        }];
        Self { bar1_base, bar1_size, blocks }
    }

    /// Allocate `size` bytes from VRAM. Returns the GPU-physical offset.
    pub fn alloc(&mut self, size: usize, tag: &str) -> Option<u64> {
        let size = (size + 4095) & !4095; // Round up to 4 KiB
        let idx  = self.blocks.iter().position(|b| !b.in_use && b.size >= size)?;

        let block_size = self.blocks[idx].size;
        let block_base = self.blocks[idx].gpu_phys;

        self.blocks[idx].in_use    = true;
        self.blocks[idx].size      = size;
        self.blocks[idx].debug_tag = String::from(tag);

        // Split remainder
        if block_size > size {
            self.blocks.insert(idx + 1, VramBlock {
                gpu_phys: block_base + size as u64,
                size:     block_size - size,
                in_use:   false,
                debug_tag: String::new(),
            });
        }
        Some(block_base)
    }

    /// Free a previously allocated GPU-physical region.
    pub fn free(&mut self, gpu_phys: u64) {
        if let Some(b) = self.blocks.iter_mut().find(|b| b.gpu_phys == gpu_phys && b.in_use) {
            b.in_use = false;
            b.debug_tag.clear();
        }
        self.coalesce();
    }

    /// Merge adjacent free blocks.
    fn coalesce(&mut self) {
        let mut i = 0;
        while i + 1 < self.blocks.len() {
            if !self.blocks[i].in_use && !self.blocks[i + 1].in_use {
                let merged_size = self.blocks[i].size + self.blocks[i + 1].size;
                self.blocks[i].size = merged_size;
                self.blocks.remove(i + 1);
            } else {
                i += 1;
            }
        }
    }

    pub fn free_bytes(&self) -> usize {
        self.blocks.iter().filter(|b| !b.in_use).map(|b| b.size).sum()
    }

    pub fn used_bytes(&self) -> usize {
        self.blocks.iter().filter(|b| b.in_use).map(|b| b.size).sum()
    }
}

// =====================================================================
// -----| GPU Device |-----
// =====================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpuVendor { Intel, AMD, Nvidia, Bochs, Unknown }

pub struct GpuDevice {
    pub vendor:    GpuVendor,
    pub device_id: u32,
    /// BAR0: MMIO register space (GPU control registers).
    pub bar0_phys: u64,
    pub bar0_size: usize,
    /// BAR1: Linear framebuffer / VRAM aperture.
    pub bar1_phys: u64,
    pub bar1_size: usize,
    /// Current display resolution.
    pub width:     u32,
    pub height:    u32,
    pub bpp:       u8,

    /// Hardware command ring (kernel-mode).
    pub ring:      Mutex<CommandRing>,
    /// VRAM block allocator.
    pub vram:      Mutex<VramAllocator>,
    /// Monotonically increasing fence value — incremented each flush.
    pub fence:     AtomicU64,
    /// Total commands processed since boot.
    pub cmds_processed: AtomicU64,
}

impl GpuDevice {
    pub fn new(
        vendor: GpuVendor, device_id: u32,
        bar0_phys: u64, bar0_size: usize,
        bar1_phys: u64, bar1_size: usize,
        width: u32, height: u32, bpp: u8,
    ) -> Self {
        Self {
            vendor, device_id,
            bar0_phys, bar0_size,
            bar1_phys, bar1_size,
            width, height, bpp,
            ring:  Mutex::new(CommandRing::new()),
            vram:  Mutex::new(VramAllocator::new(bar1_phys, bar1_size)),
            fence: AtomicU64::new(0),
            cmds_processed: AtomicU64::new(0),
        }
    }

    /// Submit a command directly to the hardware ring (kernel mode).
    pub fn submit(&self, cmd: GpuCommand) -> bool {
        self.ring.lock().push(cmd)
    }

    /// Flush and execute all pending ring commands.
    /// This is where we talk to real GPU hardware (or our BGA software model).
    pub fn flush(&self) {
        let mut processed = 0u64;
        loop {
            let cmd = self.ring.lock().pop();
            match cmd {
                None    => break,
                Some(c) => { self.execute_command(c); processed += 1; }
            }
        }
        if processed > 0 {
            self.cmds_processed.fetch_add(processed, Ordering::Relaxed);
            self.fence.fetch_add(1, Ordering::Release);
        }
    }

    /// Execute one command against the BGA framebuffer model.
    fn execute_command(&self, cmd: GpuCommand) {
        use crate::ui::{draw_rect_atomic, SCREEN_W};
        use crate::graphics::{Color, FRAMEBUFFER};

        match cmd {
            GpuCommand::FillRect { x, y, w, h, color } => {
                let r = ((color >> 16) & 0xFF) as u8;
                let g = ((color >>  8) & 0xFF) as u8;
                let b = ( color        & 0xFF) as u8;
                draw_rect_atomic(x as usize, y as usize, w as usize, h as usize,
                    Color { r, g, b });
            }

            GpuCommand::BitBlt { src_x, src_y, dst_x, dst_y, w, h } => {
                // Software BitBlt via back-buffer copy.
                if let Some(ref fb) = *FRAMEBUFFER.lock() {
                    let sw = SCREEN_W;
                    for row in 0..h as usize {
                        let sy = src_y as usize + row;
                        let dy = dst_y as usize + row;
                        if sy >= fb.height || dy >= fb.height { break; }
                        let src_ptr = (fb.base_addr + ((sy * fb.stride + src_x as usize) as u64 * 4)) as *const u32;
                        let dst_ptr = (fb.base_addr + ((dy * fb.stride + dst_x as usize) as u64 * 4)) as *mut u32;
                        let copy_w  = (w as usize).min(sw.saturating_sub(src_x as usize))
                                                  .min(sw.saturating_sub(dst_x as usize));
                        unsafe {
                            core::ptr::copy_nonoverlapping(src_ptr, dst_ptr, copy_w);
                        }
                    }
                }
            }

            GpuCommand::UploadSurface { vram_offset: _, width, height, stride, src_ptr } => {
                // Copy a CPU buffer straight to the framebuffer.
                if let Some(ref fb) = *FRAMEBUFFER.lock() {
                    for row in 0..height as usize {
                        let src = (src_ptr + (row * stride as usize) as u64) as *const u32;
                        let dst = (fb.base_addr + (row * fb.stride) as u64 * 4) as *mut u32;
                        unsafe { core::ptr::copy_nonoverlapping(src, dst, width as usize); }
                    }
                }
            }

            GpuCommand::Present { surface_vram_offset, x, y, w, h } => {
                // In our BGA model, "present" = copy from the VRAM aperture
                // to the scanout region. The VRAM aperture *is* the framebuffer,
                // so this is a memcpy within the framebuffer.
                let _ = (surface_vram_offset, x, y, w, h);
                // Full-screen present: hardware scanout already points at BAR1.
                // Nothing to do — the framebuffer IS the display.
            }

            // 3D commands: record fence but skip real execution.
            // These become real when a shader compiler (Phase 6 extension) exists.
            GpuCommand::BeginRenderPass { .. } |
            GpuCommand::EndRenderPass           |
            GpuCommand::SetVertexShader { .. }  |
            GpuCommand::SetFragmentShader { .. }|
            GpuCommand::BindVertexBuffer { .. } |
            GpuCommand::BindIndexBuffer { .. }  |
            GpuCommand::DrawIndexed { .. }      |
            GpuCommand::Draw { .. }             |
            GpuCommand::BindDescriptorSet { .. }|
            GpuCommand::SetViewport { .. }      |
            GpuCommand::SetScissor { .. }       |
            GpuCommand::PipelineBarrier { .. }  |
            GpuCommand::Nop => {}
        }
    }

    /// Map VRAM BAR1 into a process's address space.
    /// Security Mode: rejected — user must go through the command ring.
    /// Performance Mode: direct mapping, zero overhead.
    pub fn map_vram_for_process(
        &self,
        mode: PolicyMode,
        pid: u32,
        size: usize,
    ) -> Result<u64, &'static str> {
        match mode {
            PolicyMode::Security => {
                println!("[GPU] Security Mode: direct VRAM mapping denied for pid={}", pid);
                Err("VRAM direct mapping not permitted in Security Mode")
            }
            PolicyMode::Performance => {
                // Map BAR1 directly into process VAD.
                use crate::process::PROCESSOR;
                use crate::nt_memory::{MEM_RESERVE, MEM_COMMIT};
                use crate::nt_objects::PAGE_READWRITE;

                let map_size = size.min(self.bar1_size);
                let proc = PROCESSOR.lock().find(pid)
                    .ok_or("Process not found for VRAM mapping")?;

                // Reserve a region in the process VAD.
                let virt_base = proc.vad.lock()
                    .virtual_alloc(self.bar1_phys, map_size, MEM_RESERVE | MEM_COMMIT, PAGE_READWRITE)
                    .map_err(|_| "VAD allocation failed for VRAM")?;

                // Now wire up the physical BAR1 pages (identity-mapped in the kernel).
                use x86_64::{VirtAddr, PhysAddr, structures::paging::{Page, PhysFrame, PageTableFlags, Size4KiB, Mapper}};
                let mut mapper = unsafe { crate::memory::get_mapper() };
                let num_pages  = (map_size + 4095) / 4096;
                // Write-combining (PWT -> PAT entry 1, see smp::init_pat): VRAM is
                // a prefetchable BAR, and UC made every store its own bus write.
                let flags = PageTableFlags::PRESENT
                    | PageTableFlags::WRITABLE
                    | PageTableFlags::USER_ACCESSIBLE
                    | PageTableFlags::WRITE_THROUGH;

                for i in 0..num_pages {
                    let page  = Page::<Size4KiB>::containing_address(VirtAddr::new(virt_base + i as u64 * 4096));
                    let frame = PhysFrame::containing_address(PhysAddr::new(self.bar1_phys + i as u64 * 4096));
                    unsafe {
                        let _ = mapper.map_to(page, frame, flags, &mut crate::memory::GlobalFrameAllocator)
                            .map(|f| f.flush());
                    }
                }

                println!("[GPU] Performance Mode: VRAM ({} KiB) mapped at {:#X} for pid={}",
                    map_size / 1024, virt_base, pid);
                Ok(virt_base)
            }
        }
    }

    pub fn fence_value(&self) -> u64 { self.fence.load(Ordering::Acquire) }
    pub fn wait_fence(&self, value: u64) -> bool { self.fence_value() >= value }
}

// =====================================================================
// -----| Per-Process Submission Queue |-----
// =====================================================================

const SUBMISSION_CAPACITY: usize = 1024;

pub struct SubmissionQueue {
    pub pid:     u32,
    pub gpu_idx: usize,  // Which GPU this queue targets
    buffer:      Vec<GpuCommand>,
    pub priority: u8,    // 0=low .. 255=realtime
    pub submitted_total: AtomicU64,
}

impl SubmissionQueue {
    pub fn new(pid: u32, gpu_idx: usize) -> Self {
        Self {
            pid, gpu_idx,
            buffer:          Vec::with_capacity(SUBMISSION_CAPACITY),
            priority:        128, // Default normal priority
            submitted_total: AtomicU64::new(0),
        }
    }

    /// Record a command into the submission buffer. Does NOT send to GPU yet.
    pub fn record(&mut self, cmd: GpuCommand) -> bool {
        if self.buffer.len() >= SUBMISSION_CAPACITY { return false; }
        self.buffer.push(cmd);
        true
    }

    /// Drain all recorded commands into `ring`. Returns number transferred.
    fn flush_into_ring(&mut self, ring: &mut CommandRing) -> usize {
        let mut n = 0;
        for cmd in self.buffer.drain(..) {
            if ring.push(cmd) { n += 1; }
        }
        self.submitted_total.fetch_add(n as u64, Ordering::Relaxed);
        n
    }

    pub fn pending(&self) -> usize { self.buffer.len() }
}

// =====================================================================
// -----| GPU Scheduler |-----
// =====================================================================

pub struct GpuScheduler {
    pub queues:      Vec<SubmissionQueue>,
    pub round_robin: usize, // Last-served queue index
}

impl GpuScheduler {
    pub const fn new() -> Self {
        Self { queues: Vec::new(), round_robin: 0 }
    }

    pub fn register_process(&mut self, pid: u32, gpu_idx: usize) {
        if !self.queues.iter().any(|q| q.pid == pid) {
            self.queues.push(SubmissionQueue::new(pid, gpu_idx));
        }
    }

    pub fn deregister_process(&mut self, pid: u32) {
        self.queues.retain(|q| q.pid != pid);
    }

    pub fn queue_for(&mut self, pid: u32) -> Option<&mut SubmissionQueue> {
        self.queues.iter_mut().find(|q| q.pid == pid)
    }

    /// Drain all submission queues into the GPU ring and flush the hardware.
    /// Called by the kernel timer tick (vsync cadence).
    pub fn tick(&mut self, gpu: &GpuDevice) {
        if self.queues.is_empty() { return; }

        // Round-robin to avoid any one process starving others.
        let n = self.queues.len();
        for step in 0..n {
            let idx = (self.round_robin + step) % n;
            if self.queues[idx].gpu_idx != 0 { continue; } // Multi-GPU: match by idx
            let mut ring = gpu.ring.lock();
            self.queues[idx].flush_into_ring(&mut ring);
        }
        self.round_robin = (self.round_robin + 1) % n;

        // Execute all newly enqueued commands.
        gpu.flush();
    }

    /// Performance Mode: bypass scheduler entirely. Commands go directly
    /// to the ring and are executed immediately.
    pub fn submit_direct(gpu: &GpuDevice, cmd: GpuCommand) {
        gpu.submit(cmd);
        gpu.flush();
    }
}

// =====================================================================
// -----| Global GPU Registry |-----
// =====================================================================

pub static GPU_DEVICES:  Mutex<Vec<Arc<GpuDevice>>>  = Mutex::new(Vec::new());
pub static GPU_SCHEDULER: Mutex<GpuScheduler>        = Mutex::new(GpuScheduler::new());

/// Discover GPUs from the PCI device tree and register them.
pub fn init() {
    let mut gpus = GPU_DEVICES.lock();

    // Display controllers a HAL driver is running.
    for dev in crate::hal::bound(crate::hal::DeviceClass::Display).iter().map(|d| &d.pci) {
        // Pick apertures from the sized BARs instead of assuming fixed slots:
        //   • VRAM aperture = the largest memory BAR (on Bochs/QEMU that is the
        //     BAR0 linear framebuffer; on discrete cards the prefetchable VRAM BAR).
        //   • Register block = the smallest non-prefetchable memory BAR other
        //     than the aperture (0 if the device has none, e.g. stdvga).
        let mem_bars: Vec<(usize, crate::pci::Bar)> = dev.bars.iter().copied()
            .enumerate()
            .filter(|(_, b)| b.is_mem())
            .collect();
        let Some(&(vram_idx, vram_bar)) = mem_bars.iter().max_by_key(|(_, b)| b.size) else { continue };
        let regs_bar = mem_bars.iter()
            .filter(|(i, b)| *i != vram_idx && !b.prefetchable)
            .min_by_key(|(_, b)| b.size)
            .map(|&(_, b)| b)
            .unwrap_or(crate::pci::Bar::EMPTY);

        let vendor = match dev.vendor_id {
            0x8086 => GpuVendor::Intel,
            0x1002 => GpuVendor::AMD,
            0x10DE => GpuVendor::Nvidia,
            0x1234 => GpuVendor::Bochs, // QEMU/Bochs stdvga
            _      => GpuVendor::Unknown,
        };

        // Note: this is the size of the CPU-visible window, which on discrete
        // cards without resizable BAR can be smaller than the real VRAM.
        let vram_size = vram_bar.size as usize;
        dev.enable_decode();

        let gpu = Arc::new(GpuDevice::new(
            vendor, dev.device_id as u32,
            regs_bar.base, regs_bar.size as usize,
            vram_bar.base, vram_size,
            crate::ui::SCREEN_W as u32, crate::ui::SCREEN_H as u32, 32,
        ));

        println!("[GPU] Registered {:?} device {:#06x} regs={:#X} VRAM_base={:#X} ({} MiB)",
            vendor, dev.device_id, regs_bar.base, vram_bar.base, vram_size / (1024 * 1024));
        gpus.push(gpu);
    }

    if gpus.is_empty() {
        // Register a software fallback device so the rest of the stack always
        // has a GPU to talk to (maps to the existing BGA framebuffer).
        let sw_gpu = Arc::new(GpuDevice::new(
            GpuVendor::Bochs, 0x1234,
            0, 0,
            crate::ui::SCREEN_W as u64 * crate::ui::SCREEN_H as u64 * 4, // dummy BAR1
            crate::ui::SCREEN_W * crate::ui::SCREEN_H * 4,
            crate::ui::SCREEN_W as u32, crate::ui::SCREEN_H as u32, 32,
        ));
        println!("[GPU] No PCI GPU found — using software BGA fallback.");
        gpus.push(sw_gpu);
    }

    println!("[GPU] Scheduler ready. {} GPU(s) available.", gpus.len());
}

/// Convenience: get the primary GPU (index 0).
pub fn primary() -> Option<Arc<GpuDevice>> {
    GPU_DEVICES.lock().first().cloned()
}

/// Submit a command to the primary GPU, honouring the current policy mode.
pub fn submit(pid: u32, cmd: GpuCommand) {
    let mode = crate::policy::POLICY.lock().get_mode();
    match mode {
        PolicyMode::Performance => {
            if let Some(gpu) = primary() {
                GpuScheduler::submit_direct(&gpu, cmd);
            }
        }
        PolicyMode::Security => {
            let mut sched = GPU_SCHEDULER.lock();
            if let Some(q) = sched.queue_for(pid) {
                q.record(cmd);
            } else {
                // Auto-register the process and retry.
                drop(sched);
                GPU_SCHEDULER.lock().register_process(pid, 0);
                if let Some(q) = GPU_SCHEDULER.lock().queue_for(pid) {
                    q.record(cmd);
                }
            }
        }
    }
}

/// Allocate a VRAM surface. Returns the GPU-physical offset.
pub fn vram_alloc(size: usize, tag: &str) -> Option<u64> {
    primary()?.vram.lock().alloc(size, tag)
}

/// Free a VRAM surface.
pub fn vram_free(gpu_phys: u64) {
    if let Some(gpu) = primary() {
        gpu.vram.lock().free(gpu_phys);
    }
}
