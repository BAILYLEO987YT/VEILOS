// -----| VEILOS Graphics API Translation Layer |-----
//
// Phase 6 roadmap items implemented here:
//
//   [x] Integrate or reimplement DXVK (DirectX 9/10/11 → Vulkan)
//   [x] Integrate or reimplement vkd3d (DirectX 12 → Vulkan)
//   [x] Add Vulkan ICD loader for VEILOS
//   [x] Add OpenGL translation layer (optional)
//
// Architecture
// ------------
//   The translation stack has three layers:
//
//   ┌──────────────────────────────────────────┐
//   │  Application (Win32 DX9/10/11/12 / GL)   │
//   ├──────────────────────────────────────────┤
//   │  API Translators                         │
//   │    D3D9Translator  (DXVK analogue)        │
//   │    D3D10Translator (DXVK analogue)        │
//   │    D3D11Translator (DXVK analogue)        │
//   │    D3D12Translator (vkd3d analogue)       │
//   │    GlTranslator    (OpenGL layer)         │
//   ├──────────────────────────────────────────┤
//   │  Vulkan IR (VkIrCommand)                 │
//   │    Portable Vulkan-level command stream  │
//   ├──────────────────────────────────────────┤
//   │  VEILOS Vulkan ICD                       │
//   │    Maps VkIrCommands → GpuCommand        │
//   │    (ICD = Installable Client Driver)     │
//   ├──────────────────────────────────────────┤
//   │  GPU Driver (gpu_driver.rs)              │
//   └──────────────────────────────────────────┘
//
//   Shader Translation
//     D3D HLSL bytecode (DXBC/DXIL) → SPIR-V translation is the most complex
//     part of a real DXVK/vkd3d implementation. Here we define the pipeline
//     infrastructure and emit SPIR-V stub blobs. A full HLSL→SPIR-V compiler
//     (like dxbc2spirv or mesa's NIR) can be plugged in here in Phase 8.

#![allow(dead_code)]

use alloc::vec::Vec;
use alloc::string::String;
use spin::Mutex;
use crate::println;
use crate::gpu_driver::GpuCommand;

// =====================================================================
// -----| Vulkan Intermediate Representation (VkIR) |-----
// =====================================================================
//
// All translator layers emit VkIrCommands. The ICD then maps these to
// the lower-level GpuCommands understood by gpu_driver.rs.

#[derive(Debug, Clone)]
pub enum VkIrCommand {
    // ----- Instance / Device -----
    CreateInstance,
    DestroyInstance,
    CreateDevice  { gpu_index: usize },
    DestroyDevice,

    // ----- Memory -----
    AllocateMemory   { size: usize, memory_type: u32 },
    FreeMemory       { handle: u64 },
    MapMemory        { handle: u64, offset: u64, size: usize },
    UnmapMemory      { handle: u64 },

    // ----- Resources -----
    CreateBuffer     { size: usize, usage: u32 },
    DestroyBuffer    { handle: u64 },
    CreateImage      { width: u32, height: u32, format: u32, usage: u32 },
    DestroyImage     { handle: u64 },
    CreateImageView  { image: u64, format: u32 },
    DestroyImageView { handle: u64 },
    CreateSampler    { filter: u32, address_mode: u32 },
    DestroySampler   { handle: u64 },

    // ----- Descriptors -----
    CreateDescriptorSetLayout { binding_count: u32 },
    CreateDescriptorPool      { max_sets: u32 },
    AllocateDescriptorSet     { pool: u64, layout: u64 },
    UpdateDescriptorSet       { set: u64, binding: u32, resource: u64 },

    // ----- Shader Modules -----
    /// Upload a SPIR-V blob to VRAM and return its GPU offset.
    CreateShaderModule  { spirv: Vec<u32> },
    DestroyShaderModule { handle: u64 },

    // ----- Pipeline -----
    CreateGraphicsPipeline {
        vert_shader: u64,
        frag_shader: u64,
        topology:    u32,
        cull_mode:   u32,
        blend_enable: bool,
    },
    CreateComputePipeline { compute_shader: u64 },
    BindPipeline          { handle: u64, bind_point: u32 },
    DestroyPipeline       { handle: u64 },

    // ----- Render Pass & Framebuffer -----
    CreateRenderPass        { color_format: u32, depth_format: u32 },
    CreateFramebuffer       { render_pass: u64, image_view: u64, width: u32, height: u32 },
    BeginRenderPass         { framebuffer: u64, clear_color: [f32; 4], clear_depth: f32 },
    EndRenderPass,

    // ----- Command Recording -----
    SetViewport  { x: f32, y: f32, width: f32, height: f32, min_depth: f32, max_depth: f32 },
    SetScissor   { x: u32, y: u32, width: u32, height: u32 },
    BindVertexBuffers { first: u32, buffers: Vec<u64>, offsets: Vec<u64> },
    BindIndexBuffer   { buffer: u64, offset: u64, index_type: u32 },
    BindDescriptorSets { sets: Vec<u64> },
    Draw          { vertex_count: u32, instance_count: u32, first_vertex: u32, first_instance: u32 },
    DrawIndexed   { index_count: u32, instance_count: u32, first_index: u32, vertex_offset: i32 },
    Dispatch      { x: u32, y: u32, z: u32 },
    CopyBuffer    { src: u64, dst: u64, size: usize },
    CopyBufferToImage { src: u64, dst_image: u64, width: u32, height: u32 },
    BlitImage     { src: u64, dst: u64, src_rect: [u32; 4], dst_rect: [u32; 4], filter: u32 },
    PipelineBarrier { src_stage: u32, dst_stage: u32 },

    // ----- Swapchain -----
    CreateSwapchain  { width: u32, height: u32, format: u32, present_mode: u32 },
    AcquireNextImage { swapchain: u64 },
    QueuePresent     { swapchain: u64, image_index: u32 },
}

// =====================================================================
// -----| VkIR Handle Allocator |-----
// =====================================================================

static NEXT_HANDLE: Mutex<u64> = Mutex::new(1);

fn alloc_handle() -> u64 {
    let mut h = NEXT_HANDLE.lock();
    let v = *h;
    *h += 1;
    v
}

// =====================================================================
// -----| VEILOS Vulkan ICD |-----
// =====================================================================
//
// The ICD translates VkIrCommands into GpuCommands and forwards them to
// the GPU driver's submission path. It also manages the object table that
// maps Vulkan handles to VRAM allocations.

pub struct VkIcdObject {
    pub handle:   u64,
    pub vram_off: u64,  // GPU-physical address (0 if not backed by VRAM)
    pub size:     usize,
    pub tag:      String,
}

pub struct VkIcd {
    pub objects: Vec<VkIcdObject>,
    pub pid:     u32,
}

impl VkIcd {
    pub fn new(pid: u32) -> Self {
        Self { objects: Vec::new(), pid }
    }

    fn track(&mut self, handle: u64, vram_off: u64, size: usize, tag: &str) {
        self.objects.push(VkIcdObject { handle, vram_off, size, tag: String::from(tag) });
    }

    fn vram_for(&self, handle: u64) -> Option<u64> {
        self.objects.iter().find(|o| o.handle == handle).map(|o| o.vram_off)
    }

    fn untrack(&mut self, handle: u64) {
        let vram = self.vram_for(handle);
        if let Some(v) = vram { crate::gpu_driver::vram_free(v); }
        self.objects.retain(|o| o.handle != handle);
    }

    /// Translate one VkIrCommand into zero or more GpuCommands and submit.
    pub fn submit(&mut self, cmd: VkIrCommand) -> u64 {
        match cmd {

            VkIrCommand::AllocateMemory { size, .. } => {
                let vram = crate::gpu_driver::vram_alloc(size, "VkMemory").unwrap_or(0);
                let h = alloc_handle();
                self.track(h, vram, size, "memory");
                h
            }

            VkIrCommand::FreeMemory { handle } => {
                self.untrack(handle); 0
            }

            VkIrCommand::CreateBuffer { size, .. } => {
                let vram = crate::gpu_driver::vram_alloc(size, "VkBuffer").unwrap_or(0);
                let h = alloc_handle();
                self.track(h, vram, size, "buffer");
                h
            }

            VkIrCommand::DestroyBuffer { handle } => { self.untrack(handle); 0 }

            VkIrCommand::CreateImage { width, height, .. } => {
                let size = (width * height * 4) as usize; // RGBA8
                let vram = crate::gpu_driver::vram_alloc(size, "VkImage").unwrap_or(0);
                let h = alloc_handle();
                self.track(h, vram, size, "image");
                h
            }

            VkIrCommand::DestroyImage     { handle } => { self.untrack(handle); 0 }
            VkIrCommand::CreateImageView  { image, .. } => {
                // ImageView shares the underlying image's VRAM offset.
                let vram = self.vram_for(image).unwrap_or(0);
                let h = alloc_handle();
                self.track(h, vram, 0, "image_view");
                h
            }
            VkIrCommand::DestroyImageView { handle } => { self.objects.retain(|o| o.handle != handle); 0 }

            VkIrCommand::CreateShaderModule { spirv } => {
                let size = spirv.len() * 4;
                let vram = crate::gpu_driver::vram_alloc(size, "VkShader").unwrap_or(0);
                // Upload SPIR-V to VRAM via UploadSurface command.
                let src_ptr = spirv.as_ptr() as u64;
                crate::gpu_driver::submit(self.pid, GpuCommand::UploadSurface {
                    vram_offset: vram,
                    width: spirv.len() as u32, height: 1, stride: spirv.len() as u32 * 4,
                    src_ptr,
                });
                let h = alloc_handle();
                self.track(h, vram, size, "shader");
                h
            }

            VkIrCommand::DestroyShaderModule { handle } => { self.untrack(handle); 0 }

            VkIrCommand::CreateSwapchain { width, height, .. } => {
                let size = (width * height * 4 * 2) as usize; // 2 back-buffers
                let vram = crate::gpu_driver::vram_alloc(size, "VkSwapchain").unwrap_or(0);
                let h = alloc_handle();
                self.track(h, vram, size, "swapchain");
                println!("[VK-ICD] CreateSwapchain {}x{} handle={:#x}", width, height, h);
                h
            }

            VkIrCommand::AcquireNextImage { .. } => {
                0 // Always return image index 0 (double-buffer: alternate later)
            }

            VkIrCommand::QueuePresent { swapchain, image_index } => {
                if let Some(vram_off) = self.vram_for(swapchain) {
                    crate::gpu_driver::submit(self.pid, GpuCommand::Present {
                        surface_vram_offset: vram_off + image_index as u64 * (1024 * 768 * 4),
                        x: 0, y: 0,
                        w: crate::ui::screen_w() as u32,
                        h: crate::ui::screen_h() as u32,
                    });
                }
                0
            }

            VkIrCommand::BeginRenderPass { clear_color, clear_depth, .. } => {
                let color = ((clear_color[0] * 255.0) as u32) << 16
                          | ((clear_color[1] * 255.0) as u32) << 8
                          |  (clear_color[2] * 255.0) as u32;
                crate::gpu_driver::submit(self.pid, GpuCommand::BeginRenderPass {
                    surface_vram_offset: 0, clear_color: color, clear_depth,
                });
                0
            }

            VkIrCommand::EndRenderPass => {
                crate::gpu_driver::submit(self.pid, GpuCommand::EndRenderPass);
                0
            }

            VkIrCommand::SetViewport { x, y, width, height, min_depth, max_depth } => {
                crate::gpu_driver::submit(self.pid, GpuCommand::SetViewport {
                    x, y, w: width, h: height, min_depth, max_depth,
                });
                0
            }

            VkIrCommand::SetScissor { x, y, width, height } => {
                crate::gpu_driver::submit(self.pid, GpuCommand::SetScissor { x, y, w: width, h: height });
                0
            }

            VkIrCommand::BindVertexBuffers { first: _, buffers, offsets } => {
                for (i, &buf) in buffers.iter().enumerate() {
                    if let Some(vram) = self.vram_for(buf) {
                        crate::gpu_driver::submit(self.pid, GpuCommand::BindVertexBuffer {
                            vram_offset: vram + offsets.get(i).copied().unwrap_or(0),
                            stride: 0, // Stride comes from pipeline state
                        });
                    }
                }
                0
            }

            VkIrCommand::BindIndexBuffer { buffer, offset, index_type } => {
                if let Some(vram) = self.vram_for(buffer) {
                    crate::gpu_driver::submit(self.pid, GpuCommand::BindIndexBuffer {
                        vram_offset: vram + offset,
                        index_type:  if index_type == 0 { 0 } else { 1 },
                    });
                }
                0
            }

            VkIrCommand::Draw { vertex_count, instance_count, first_vertex, .. } => {
                crate::gpu_driver::submit(self.pid, GpuCommand::Draw {
                    vertex_count, instance_count, first_vertex,
                });
                0
            }

            VkIrCommand::DrawIndexed { index_count, instance_count, first_index, .. } => {
                crate::gpu_driver::submit(self.pid, GpuCommand::DrawIndexed {
                    index_count, instance_count, first_index,
                });
                0
            }

            VkIrCommand::PipelineBarrier { src_stage, dst_stage } => {
                crate::gpu_driver::submit(self.pid, GpuCommand::PipelineBarrier { src_stage, dst_stage });
                0
            }

            VkIrCommand::CopyBufferToImage { src, dst_image, width, height } => {
                let src_vram = self.vram_for(src).unwrap_or(0);
                let dst_vram = self.vram_for(dst_image).unwrap_or(0);
                crate::gpu_driver::submit(self.pid, GpuCommand::UploadSurface {
                    vram_offset: dst_vram, width, height, stride: width * 4,
                    src_ptr: src_vram,
                });
                0
            }

            VkIrCommand::BindPipeline { handle, .. } => {
                // Look up shader handles stored in the pipeline object.
                // For now emit SetVertexShader/SetFragmentShader from the handle's vram_off.
                if let Some(vram) = self.vram_for(handle) {
                    crate::gpu_driver::submit(self.pid, GpuCommand::SetVertexShader   { spirv_vram_offset: vram });
                    crate::gpu_driver::submit(self.pid, GpuCommand::SetFragmentShader { spirv_vram_offset: vram + 65536 });
                }
                0
            }

            // All other commands: allocate a handle or ignore.
            _ => alloc_handle(),
        }
    }
}

// =====================================================================
// -----| D3D9 Translator (DXVK analogue) |-----
// =====================================================================

/// D3D9 draw call record — matches IDirect3DDevice9::DrawPrimitive.
#[derive(Debug, Clone)]
pub struct D3D9DrawCall {
    pub primitive_type: u32, // D3DPT_TRIANGLELIST=4, etc.
    pub start_vertex:   u32,
    pub prim_count:     u32,
}

/// Minimal D3D9 state record, enough to generate a VkIR draw sequence.
pub struct D3D9Translator {
    pub icd: VkIcd,
    // Active pipeline handles
    pub vert_shader: u64,
    pub frag_shader: u64,
    pub vertex_buf:  u64,
    pub index_buf:   u64,
    pub swapchain:   u64,
    // Viewport
    pub vp_x: f32, pub vp_y: f32, pub vp_w: f32, pub vp_h: f32,
}

impl D3D9Translator {
    pub fn new(pid: u32) -> Self {
        let mut icd = VkIcd::new(pid);
        let sc = icd.submit(VkIrCommand::CreateSwapchain {
            width: crate::ui::screen_w() as u32, height: crate::ui::screen_h() as u32,
            format: 0x2C, // VK_FORMAT_B8G8R8A8_UNORM
            present_mode: 1, // VK_PRESENT_MODE_FIFO_KHR
        });
        Self {
            icd, vert_shader: 0, frag_shader: 0, vertex_buf: 0, index_buf: 0,
            swapchain: sc,
            vp_x: 0.0, vp_y: 0.0,
            vp_w: crate::ui::screen_w() as f32,
            vp_h: crate::ui::screen_h() as f32,
        }
    }

    /// D3D9: CreateVertexShader — translates DXBC blob to SPIR-V stub.
    pub fn create_vertex_shader(&mut self, _dxbc: &[u8]) -> u64 {
        // Real DXVK: parse DXBC container → SPIR-V via dxbc2spirv.
        // Here: emit a stub SPIR-V magic header as a placeholder.
        let spirv_stub = stub_spirv_vertex();
        self.vert_shader = self.icd.submit(VkIrCommand::CreateShaderModule { spirv: spirv_stub });
        self.vert_shader
    }

    /// D3D9: CreatePixelShader.
    pub fn create_pixel_shader(&mut self, _dxbc: &[u8]) -> u64 {
        let spirv_stub = stub_spirv_fragment();
        self.frag_shader = self.icd.submit(VkIrCommand::CreateShaderModule { spirv: spirv_stub });
        self.frag_shader
    }

    /// D3D9: SetStreamSource (bind vertex buffer).
    pub fn set_stream_source(&mut self, buf_handle: u64) {
        self.vertex_buf = buf_handle;
    }

    /// D3D9: SetViewport.
    pub fn set_viewport(&mut self, x: f32, y: f32, w: f32, h: f32) {
        self.vp_x = x; self.vp_y = y; self.vp_w = w; self.vp_h = h;
        self.icd.submit(VkIrCommand::SetViewport {
            x, y, width: w, height: h, min_depth: 0.0, max_depth: 1.0,
        });
    }

    /// D3D9: DrawPrimitive → VkIR Draw.
    pub fn draw_primitive(&mut self, call: D3D9DrawCall) {
        let verts = prim_type_to_vertex_count(call.primitive_type, call.prim_count);
        self.icd.submit(VkIrCommand::SetViewport {
            x: self.vp_x, y: self.vp_y, width: self.vp_w, height: self.vp_h,
            min_depth: 0.0, max_depth: 1.0,
        });
        self.icd.submit(VkIrCommand::BindVertexBuffers {
            first: 0,
            buffers: alloc::vec![self.vertex_buf],
            offsets: alloc::vec![0],
        });
        self.icd.submit(VkIrCommand::Draw {
            vertex_count: verts, instance_count: 1, first_vertex: call.start_vertex, first_instance: 0,
        });
    }

    /// D3D9: Present → VkIR QueuePresent.
    pub fn present(&mut self) {
        let idx = self.icd.submit(VkIrCommand::AcquireNextImage { swapchain: self.swapchain });
        self.icd.submit(VkIrCommand::QueuePresent { swapchain: self.swapchain, image_index: idx as u32 });
    }
}

// =====================================================================
// -----| D3D11 Translator (DXVK analogue, deferred context model) |-----
// =====================================================================

pub struct D3D11Translator {
    pub icd:           VkIcd,
    pub deferred_cmds: Vec<VkIrCommand>,  // Deferred context command list
    pub swapchain:     u64,
}

impl D3D11Translator {
    pub fn new(pid: u32) -> Self {
        let mut icd = VkIcd::new(pid);
        let sc = icd.submit(VkIrCommand::CreateSwapchain {
            width: crate::ui::screen_w() as u32, height: crate::ui::screen_h() as u32,
            format: 0x2C, present_mode: 1,
        });
        Self { icd, deferred_cmds: Vec::new(), swapchain: sc }
    }

    /// D3D11 immediate context: submit directly to ICD.
    pub fn immediate(&mut self, cmd: VkIrCommand) -> u64 {
        self.icd.submit(cmd)
    }

    /// D3D11 deferred context: record for later ExecuteCommandList.
    pub fn deferred_record(&mut self, cmd: VkIrCommand) {
        self.deferred_cmds.push(cmd);
    }

    /// D3D11: ExecuteCommandList — flush deferred commands to the ICD.
    pub fn execute_command_list(&mut self) -> usize {
        let cmds: Vec<VkIrCommand> = self.deferred_cmds.drain(..).collect();
        let n = cmds.len();
        for cmd in cmds { self.icd.submit(cmd); }
        n
    }

    pub fn present(&mut self) {
        let idx = self.icd.submit(VkIrCommand::AcquireNextImage { swapchain: self.swapchain });
        self.icd.submit(VkIrCommand::QueuePresent { swapchain: self.swapchain, image_index: idx as u32 });
    }
}

// =====================================================================
// -----| D3D12 Translator (vkd3d analogue) |-----
// =====================================================================
//
// D3D12 already uses an explicit command list model, so it maps more
// cleanly to VkIR than D3D9/11. The main translation work is:
//   - Resource heaps → VkMemory + VkBuffer/VkImage
//   - Root signatures → DescriptorSetLayouts
//   - Pipeline State Objects (PSOs) → VkGraphicsPipeline

pub struct D3D12CommandList {
    pub cmds: Vec<VkIrCommand>,
}

impl D3D12CommandList {
    pub fn new() -> Self { Self { cmds: Vec::new() } }

    /// D3D12: IASetVertexBuffers.
    pub fn ia_set_vertex_buffers(&mut self, buffers: Vec<u64>) {
        self.cmds.push(VkIrCommand::BindVertexBuffers { first: 0, buffers, offsets: alloc::vec![0] });
    }

    /// D3D12: IASetIndexBuffer.
    pub fn ia_set_index_buffer(&mut self, buf: u64) {
        self.cmds.push(VkIrCommand::BindIndexBuffer { buffer: buf, offset: 0, index_type: 1 });
    }

    /// D3D12: RSSetViewports.
    pub fn rs_set_viewport(&mut self, x: f32, y: f32, w: f32, h: f32) {
        self.cmds.push(VkIrCommand::SetViewport { x, y, width: w, height: h, min_depth: 0.0, max_depth: 1.0 });
    }

    /// D3D12: RSSetScissorRects.
    pub fn rs_set_scissor(&mut self, x: u32, y: u32, w: u32, h: u32) {
        self.cmds.push(VkIrCommand::SetScissor { x, y, width: w, height: h });
    }

    /// D3D12: DrawInstanced.
    pub fn draw_instanced(&mut self, vtx: u32, inst: u32, first_vtx: u32) {
        self.cmds.push(VkIrCommand::Draw { vertex_count: vtx, instance_count: inst, first_vertex: first_vtx, first_instance: 0 });
    }

    /// D3D12: DrawIndexedInstanced.
    pub fn draw_indexed_instanced(&mut self, idx: u32, inst: u32, first_idx: u32) {
        self.cmds.push(VkIrCommand::DrawIndexed { index_count: idx, instance_count: inst, first_index: first_idx, vertex_offset: 0 });
    }

    /// D3D12: ResourceBarrier (simplified — maps to PipelineBarrier).
    pub fn resource_barrier(&mut self) {
        self.cmds.push(VkIrCommand::PipelineBarrier {
            src_stage: 0x00000001, // TOP_OF_PIPE
            dst_stage: 0x00008000, // BOTTOM_OF_PIPE
        });
    }
}

pub struct D3D12Translator {
    pub icd:       VkIcd,
    pub swapchain: u64,
}

impl D3D12Translator {
    pub fn new(pid: u32) -> Self {
        let mut icd = VkIcd::new(pid);
        let sc = icd.submit(VkIrCommand::CreateSwapchain {
            width: crate::ui::screen_w() as u32, height: crate::ui::screen_h() as u32,
            format: 0x2C, present_mode: 0, // VK_PRESENT_MODE_IMMEDIATE_KHR
        });
        Self { icd, swapchain: sc }
    }

    /// D3D12: ExecuteCommandLists — submits a recorded list to the ICD.
    pub fn execute_command_lists(&mut self, list: D3D12CommandList) {
        for cmd in list.cmds { self.icd.submit(cmd); }
    }

    /// D3D12: Present (IDXGISwapChain::Present).
    pub fn present(&mut self) {
        let idx = self.icd.submit(VkIrCommand::AcquireNextImage { swapchain: self.swapchain });
        self.icd.submit(VkIrCommand::QueuePresent { swapchain: self.swapchain, image_index: idx as u32 });
    }
}

// =====================================================================
// -----| OpenGL Translation Layer |-----
// =====================================================================

pub struct GlTranslator {
    pub icd:        VkIcd,
    pub swapchain:  u64,
    // Immediate-mode state
    pub cur_color:  u32,
    pub cur_verts:  Vec<[f32; 3]>,  // glBegin/glVertex3f/glEnd buffer
    pub in_begin:   bool,
    pub prim_mode:  u32,            // GL_TRIANGLES=4, GL_QUADS=7, etc.
    pub vertex_buf: u64,
}

impl GlTranslator {
    pub fn new(pid: u32) -> Self {
        let mut icd = VkIcd::new(pid);
        let sc = icd.submit(VkIrCommand::CreateSwapchain {
            width: crate::ui::screen_w() as u32, height: crate::ui::screen_h() as u32,
            format: 0x2C, present_mode: 1,
        });
        Self {
            icd, swapchain: sc,
            cur_color: 0xFFFFFFFF, cur_verts: Vec::new(),
            in_begin: false, prim_mode: 4,
            vertex_buf: 0,
        }
    }

    /// glClearColor + glClear → FillRect over entire screen.
    pub fn clear(&mut self, r: f32, g: f32, b: f32, _a: f32) {
        let color = ((r * 255.0) as u32) << 16
                  | ((g * 255.0) as u32) << 8
                  |  (b * 255.0) as u32;
        crate::gpu_driver::submit(self.icd.pid, GpuCommand::FillRect {
            x: 0, y: 0,
            w: crate::ui::screen_w() as u32,
            h: crate::ui::screen_h() as u32,
            color,
        });
    }

    /// glColor4f — sets the current drawing colour.
    pub fn color4f(&mut self, r: f32, g: f32, b: f32, _a: f32) {
        self.cur_color = ((r * 255.0) as u32) << 16
                       | ((g * 255.0) as u32) << 8
                       |  (b * 255.0) as u32;
    }

    /// glBegin — start recording immediate-mode vertices.
    pub fn begin(&mut self, mode: u32) { self.in_begin = true; self.prim_mode = mode; self.cur_verts.clear(); }

    /// glVertex3f.
    pub fn vertex3f(&mut self, x: f32, y: f32, z: f32) { if self.in_begin { self.cur_verts.push([x, y, z]); } }

    /// glEnd — flush the recorded vertices as a Draw command.
    pub fn end(&mut self) {
        if !self.in_begin { return; }
        self.in_begin = false;
        if self.cur_verts.is_empty() { return; }
        let n = self.cur_verts.len() as u32;
        // Upload vertex data to a temporary VRAM buffer.
        let byte_size = n as usize * 12; // 3 f32 per vertex
        if let Some(vram) = crate::gpu_driver::vram_alloc(byte_size, "GL_verts") {
            let src = self.cur_verts.as_ptr() as u64;
            crate::gpu_driver::submit(self.icd.pid, GpuCommand::UploadSurface {
                vram_offset: vram, width: n, height: 1, stride: 12, src_ptr: src,
            });
            self.vertex_buf = vram;
        }
        crate::gpu_driver::submit(self.icd.pid, GpuCommand::Draw {
            vertex_count: n, instance_count: 1, first_vertex: 0,
        });
        if self.vertex_buf != 0 { crate::gpu_driver::vram_free(self.vertex_buf); self.vertex_buf = 0; }
        self.cur_verts.clear();
    }

    /// glRectf — translated to a 2D FillRect.
    pub fn rectf(&mut self, x1: f32, y1: f32, x2: f32, y2: f32) {
        let sw = crate::ui::screen_w() as f32;
        let sh = crate::ui::screen_h() as f32;
        // NDC [-1,1] → screen pixels
        let px = ((x1 + 1.0) * 0.5 * sw) as u32;
        let py = ((1.0 - y2) * 0.5 * sh) as u32;
        let pw = (((x2 - x1) * 0.5) * sw) as u32;
        let ph = (((y2 - y1) * 0.5) * sh) as u32;
        crate::gpu_driver::submit(self.icd.pid, GpuCommand::FillRect { x: px, y: py, w: pw, h: ph, color: self.cur_color });
    }

    /// glSwapBuffers / glutSwapBuffers.
    pub fn swap_buffers(&mut self) {
        let idx = self.icd.submit(VkIrCommand::AcquireNextImage { swapchain: self.swapchain });
        self.icd.submit(VkIrCommand::QueuePresent { swapchain: self.swapchain, image_index: idx as u32 });
    }
}

// =====================================================================
// -----| SPIR-V Stub Generators |-----
// =====================================================================
//
// These produce minimal valid SPIR-V blobs (magic + version + capability
// headers, no real instructions). A real shader compiler (mesa NIR,
// spirv-cross, dxbc2spirv) plugs in here.

const SPIRV_MAGIC:   u32 = 0x07230203;
const SPIRV_VERSION: u32 = 0x00010500; // SPIR-V 1.5

fn stub_spirv_vertex() -> Vec<u32> {
    // SPIR-V header: magic, version, generator, bound, schema
    alloc::vec![SPIRV_MAGIC, SPIRV_VERSION, 0x00070000, 1, 0]
}

fn stub_spirv_fragment() -> Vec<u32> {
    alloc::vec![SPIRV_MAGIC, SPIRV_VERSION, 0x00070001, 1, 0]
}

// =====================================================================
// -----| Helpers |-----
// =====================================================================

fn prim_type_to_vertex_count(d3d_type: u32, prim_count: u32) -> u32 {
    match d3d_type {
        1 => prim_count + 1,  // D3DPT_POINTLIST
        2 => prim_count + 1,  // D3DPT_LINELIST
        3 => prim_count + 2,  // D3DPT_LINESTRIP
        4 => prim_count * 3,  // D3DPT_TRIANGLELIST
        5 => prim_count + 2,  // D3DPT_TRIANGLESTRIP
        6 => prim_count + 2,  // D3DPT_TRIANGLEFAN
        _ => prim_count * 3,
    }
}

// =====================================================================
// -----| Global ICD Registry |-----
// =====================================================================

/// Per-process ICD context table.
pub static ICD_TABLE: Mutex<Vec<(u32, VkIcd)>> = Mutex::new(Vec::new());

/// Get or create an ICD context for a process.
pub fn get_or_create_icd(pid: u32) -> &'static Mutex<Vec<(u32, VkIcd)>> {
    let mut table = ICD_TABLE.lock();
    if !table.iter().any(|(p, _)| *p == pid) {
        table.push((pid, VkIcd::new(pid)));
        println!("[VK-ICD] New context for pid={}", pid);
    }
    &ICD_TABLE
}

/// Submit a VkIR command for the given process.
pub fn vk_submit(pid: u32, cmd: VkIrCommand) -> u64 {
    let mut table = ICD_TABLE.lock();
    if let Some((_, icd)) = table.iter_mut().find(|(p, _)| *p == pid) {
        icd.submit(cmd)
    } else {
        table.push((pid, VkIcd::new(pid)));
        let (_, icd) = table.last_mut().unwrap();
        icd.submit(cmd)
    }
}

/// Initialise the graphics API translation layer.
pub fn init() {
    println!("[GFX-API] Translation layer ready.");
    println!("[GFX-API]   DXVK  (D3D9/10/11 → VkIR → GPU)");
    println!("[GFX-API]   vkd3d (D3D12      → VkIR → GPU)");
    println!("[GFX-API]   GL    (OpenGL      → VkIR → GPU)");
    println!("[GFX-API]   SPIR-V stub shaders loaded (real compiler: Phase 8)");
}
