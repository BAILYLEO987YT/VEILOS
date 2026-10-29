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
//     (e.g. AMD CP, Intel EXECLIST, Nvidia PUSHBUF). Here `flush` drains it
//     as one batch and runs the batch on the logical GPU (`lgpu`, HAL step
//     7C), which splits the pixel work over every CPU.
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
//     a VramBlock describing its physical GPU address. The visible
//     framebuffer (scanout) is the start of the aperture and is reserved.
//
//   Command semantics (software model)
//     FillRect, BitBlt and Present draw on the scanout in screen pixels;
//     UploadSurface copies CPU memory into VRAM at `vram_offset` (rows of
//     `width` pixels read `stride` bytes apart); Present copies a VRAM
//     surface (`w` x `h`, packed) to the screen at (x, y). 3D commands are
//     accepted and ignored until a shader compiler exists.
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
    /// Kernel address of the VRAM aperture, or 0 when it isn't mapped (the
    /// software fallback device): surface commands are then skipped.
    pub vram_virt: u64,
}

impl GpuDevice {
    pub fn new(
        vendor: GpuVendor, device_id: u32,
        bar0_phys: u64, bar0_size: usize,
        bar1_phys: u64, bar1_size: usize,
        width: u32, height: u32, bpp: u8,
        vram_virt: u64,
    ) -> Self {
        Self {
            vendor, device_id,
            bar0_phys, bar0_size,
            bar1_phys, bar1_size,
            width, height, bpp,
            vram_virt,
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

    /// Flush and execute all pending ring commands as one batch.
    pub fn flush(&self) {
        let mut batch = Vec::new();
        {
            let mut ring = self.ring.lock();
            while let Some(c) = ring.pop() {
                batch.push(c);
            }
        }
        if batch.is_empty() {
            return;
        }
        self.execute_batch(&batch);
        self.cmds_processed.fetch_add(batch.len() as u64, Ordering::Relaxed);
        self.fence.fetch_add(1, Ordering::Release);
    }

    /// Run a batch on the logical GPU: the 2D commands become `lgpu` ops
    /// rendered together, banded over every CPU. A screen-to-screen BitBlt
    /// reads pixels earlier commands write, and an upload overwrites a surface
    /// an earlier Present reads, so each of those first renders the ops
    /// collected so far and then runs on its own.
    fn execute_batch(&self, batch: &[GpuCommand]) {
        use crate::lgpu::{execute, Op};
        let Some(screen) = scanout() else { return };
        let mut ops: Vec<Op> = Vec::new();
        for cmd in batch {
            match *cmd {
                GpuCommand::FillRect { x, y, w, h, color } => {
                    ops.push(Op::Fill { rect: screen_rect(&screen, x, y, w, h), color: color & 0x00FF_FFFF });
                }
                GpuCommand::Present { surface_vram_offset, x, y, w, h } => {
                    if let Some(src) = self.vram_surface(surface_vram_offset, w, h) {
                        ops.push(Op::Copy { src, src_x: 0, src_y: 0, dst: screen_rect(&screen, x, y, w, h), transparent: None });
                    }
                }
                GpuCommand::BitBlt { src_x, src_y, dst_x, dst_y, w, h } => {
                    execute(&ops, &screen);
                    ops.clear();
                    blit_within(&screen, src_x as usize, src_y as usize, dst_x as usize, dst_y as usize, w as usize, h as usize);
                }
                GpuCommand::UploadSurface { vram_offset, width, height, stride, src_ptr } => {
                    execute(&ops, &screen);
                    ops.clear();
                    self.upload(vram_offset, width, height, stride, src_ptr);
                }
                // 3D commands: accepted, not executed until a shader
                // compiler (Phase 6 extension) exists.
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
        execute(&ops, &screen);
    }

    /// A packed `w` x `h` surface at `offset` in VRAM, if it fits the aperture.
    fn vram_surface(&self, offset: u64, w: u32, h: u32) -> Option<crate::lgpu::Surface> {
        let bytes = (w as u64) * (h as u64) * 4;
        if self.vram_virt == 0 || bytes == 0 || offset.checked_add(bytes)? > self.bar1_size as u64 {
            return None;
        }
        Some(crate::lgpu::Surface { ptr: (self.vram_virt + offset) as *mut u32, width: w as usize, height: h as usize, stride: w as usize })
    }

    /// UploadSurface: `height` rows of `width` pixels, `stride` bytes apart
    /// at `src_ptr` (kernel memory or the current address space), into the
    /// packed VRAM surface at `vram_offset`.
    fn upload(&self, vram_offset: u64, width: u32, height: u32, stride: u32, src_ptr: u64) {
        let Some(dst) = self.vram_surface(vram_offset, width, height) else { return };
        if src_ptr == 0 || (stride as usize) < width as usize * 4 && height > 1 {
            return;
        }
        for row in 0..dst.height {
            let src = (src_ptr + (row * stride as usize) as u64) as *const u32;
            // SAFETY: the destination was bounds-checked against the aperture;
            // the source is the submitter's buffer (unchecked, as before).
            unsafe { core::ptr::copy_nonoverlapping(src, dst.ptr.add(row * dst.stride), dst.width); }
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

    /// Drain all submission queues into the GPU ring (`tick` then runs it).
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

/// The visible framebuffer as a logical-GPU target.
fn scanout() -> Option<crate::lgpu::Surface> {
    let fb = crate::graphics::FRAMEBUFFER.lock();
    let fb = fb.as_ref()?;
    Some(crate::lgpu::Surface { ptr: fb.base_addr as *mut u32, width: fb.width, height: fb.height, stride: fb.stride })
}

/// A command's rectangle in screen pixels, cut to the screen.
fn screen_rect(s: &crate::lgpu::Surface, x: u32, y: u32, w: u32, h: u32) -> crate::lgpu::Rect {
    let x0 = (x as usize).min(s.width);
    let y0 = (y as usize).min(s.height);
    crate::lgpu::Rect::new(x0, y0, (x as usize + w as usize).min(s.width), (y as usize + h as usize).min(s.height))
}

/// Screen-to-screen copy with memmove semantics: overlapping source and
/// destination (scrolling) come out right.
fn blit_within(s: &crate::lgpu::Surface, sx: usize, sy: usize, dx: usize, dy: usize, w: usize, h: usize) {
    let w = w.min(s.width.saturating_sub(sx)).min(s.width.saturating_sub(dx));
    let h = h.min(s.height.saturating_sub(sy)).min(s.height.saturating_sub(dy));
    if w == 0 || h == 0 {
        return;
    }
    let copy_row = |row: usize| unsafe {
        let src = s.ptr.add((sy + row) * s.stride + sx);
        let dst = s.ptr.add((dy + row) * s.stride + dx);
        core::ptr::copy(src, dst, w);
    };
    // Moving down: copy the bottom row first so no source row is overwritten early.
    if dy > sy {
        (0..h).rev().for_each(copy_row);
    } else {
        (0..h).for_each(copy_row);
    }
}

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

        // Surfaces need the aperture mapped; the display driver maps it.
        let vram_virt = match crate::graphics::vram_mapping(vram_bar.base) {
            Some(size) if size >= vram_size => vram_bar.base,
            _ => 0,
        };
        let gpu = Arc::new(GpuDevice::new(
            vendor, dev.device_id as u32,
            regs_bar.base, regs_bar.size as usize,
            vram_bar.base, vram_size,
            crate::ui::screen_w() as u32, crate::ui::screen_h() as u32, 32,
            vram_virt,
        ));
        // The aperture starts with the visible framebuffer: never hand it out.
        let scanout_bytes = crate::graphics::scanout_bytes(vram_bar.base);
        if scanout_bytes > 0 {
            gpu.vram.lock().alloc(scanout_bytes, "scanout");
        }

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
            crate::ui::screen_w() as u64 * crate::ui::screen_h() as u64 * 4, // dummy BAR1
            crate::ui::screen_w() * crate::ui::screen_h() * 4,
            crate::ui::screen_w() as u32, crate::ui::screen_h() as u32, 32,
            0,
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
            // An upload reads the submitter's memory, which is not mapped when
            // the queued batch runs (from the GUI loop, in another address
            // space), so it is done now. A Present still queued from the same
            // surface would then show the new pixels.
            if let GpuCommand::UploadSurface { vram_offset, width, height, stride, src_ptr } = cmd {
                if let Some(gpu) = primary() {
                    gpu.upload(vram_offset, width, height, stride, src_ptr);
                }
                return;
            }
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

/// Run what processes queued since the last call (Security Mode) as one
/// batch. Performance Mode runs each command as it is submitted. Called once
/// per GUI loop iteration.
pub fn tick() {
    let Some(gpu) = primary() else { return };
    GPU_SCHEDULER.lock().tick(&gpu);
    gpu.flush();
}

/// `GPUTEST`: one batch through the ring (a full-screen fill, an upload, a
/// present, an overlapping blit, another fill), the pixels it leaves checked,
/// and the batch timed. Draws on the screen; the desktop paints over it on
/// its next frame.
pub fn self_test() -> Vec<String> {
    use alloc::format;
    let mut out = Vec::new();
    let (Some(gpu), Some(screen)) = (primary(), scanout()) else {
        out.push(String::from("gputest: no GPU or no framebuffer"));
        return out;
    };
    let (iw, ih) = (256u32, 256u32);
    let img_px = |x: u32, y: u32| (x << 16) | (y << 8) | 0x40;
    let img: Vec<u32> = (0..iw * ih).map(|i| img_px(i % iw, i / iw)).collect();
    let Some(surf) = gpu.vram.lock().alloc((iw * ih * 4) as usize, "gputest") else {
        out.push(String::from("gputest: no free VRAM"));
        return out;
    };
    let (sw, sh) = (screen.width as u32, screen.height as u32);
    let batch = [
        GpuCommand::FillRect { x: 0, y: 0, w: sw, h: sh, color: 0x20_30_40 },
        GpuCommand::UploadSurface { vram_offset: surf, width: iw, height: ih, stride: iw * 4, src_ptr: img.as_ptr() as u64 },
        GpuCommand::Present { surface_vram_offset: surf, x: 100, y: 100, w: iw, h: ih },
        // Overlaps its own source: the image moves down 50 rows.
        GpuCommand::BitBlt { src_x: 100, src_y: 100, dst_x: 100, dst_y: 150, w: iw, h: ih },
        GpuCommand::FillRect { x: 600, y: 100, w: 200, h: 200, color: 0xFF_00_00 },
    ];
    let t0 = crate::time::rdtsc();
    for c in batch.iter().cloned() {
        gpu.submit(c);
    }
    gpu.flush();
    let cycles = crate::time::rdtsc() - t0;
    let px = |x: usize, y: usize| unsafe { *screen.ptr.add(y * screen.stride + x) } & 0x00FF_FFFF;
    let checks = [
        ("fill", px(5, 5), 0x20_30_40),
        ("present", px(110, 110), img_px(10, 10)),
        ("blit", px(110, 160), img_px(10, 10)),
        ("blit end", px(110, 350), img_px(10, 200)),
        ("fill 2", px(700, 200), 0xFF_00_00),
    ];
    gpu.vram.lock().free(surf);
    let failed: Vec<_> = checks.iter().filter(|c| c.1 != c.2).collect();
    out.push(format!("gputest: {} commands in one batch, {}.{} Mcycles on {} CPUs",
        batch.len(), cycles / 1_000_000, cycles / 100_000 % 10,
        crate::smp::CPU_COUNT.load(Ordering::Relaxed)));
    if failed.is_empty() {
        out.push(format!("gputest: all {} pixel checks passed", checks.len()));
    }
    for (name, got, want) in failed {
        out.push(format!("gputest: {} FAILED: got {:06x}, want {:06x}", name, got, want));
    }
    out
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
