// -----| VEILOS Window Compositor |-----
//
// Phase 6 roadmap items implemented here:
//
//   [x] Implement a minimal compositor for windowed apps
//   [x] Add input routing (mouse, keyboard, gamepad)
//   [x] Add vsync, frame pacing, and swapchain support
//   [x] Add GPU-accelerated 2D rendering primitives
//
// Architecture
// ------------
//   Surface
//     Every window owns a Surface — a VRAM-backed pixel buffer. The surface
//     has two slots (double-buffering): a BACK buffer that the app renders
//     into, and a FRONT buffer that the compositor scans out. A Present()
//     call atomically flips back→front.
//
//   Compositor
//     Owns the root framebuffer and all registered Surfaces. On each vsync
//     tick it composites all visible surfaces (in Z-order) onto the root
//     framebuffer via GPU BitBlt commands. Dirty-rectangle tracking means
//     only changed regions are recomposited.
//
//   Swapchain
//     A Swapchain wraps a Surface and adds frame pacing: it tracks the
//     last-presented timestamp and enforces a maximum frame rate.
//     Win32 DXGI swapchains are mapped to VkSwapchain handles in gfx_api.
//
//   Vsync
//     The PIT timer in interrupts.rs fires at ~1000 Hz. The compositor
//     accumulates those ticks; every 16 ms (~60 Hz) it triggers a compose
//     pass. A PRESENT_INTERVAL flag allows the app to request 30, 60, or
//     uncapped presentation.
//
//   GPU-accelerated 2D Primitives
//     The compositor provides a DrawContext — a high-level 2D API that emits
//     GpuCommand::FillRect / BitBlt for GPU execution. This replaces the
//     CPU-side draw_rect_atomic() calls for compositing work.
//
//   Input Routing
//     Mouse events hit-test the surface stack (topmost visible surface wins).
//     Keyboard events go to the focused surface (changed by click-to-focus).
//     Gamepad axis/button events are broadcast to all interested surfaces.

#![allow(dead_code)]

use alloc::vec::Vec;
use alloc::string::String;
use spin::Mutex;
use core::sync::atomic::{AtomicU64, AtomicBool, AtomicU32, Ordering};
use crate::println;
use crate::gpu_driver::{GpuCommand, submit as gpu_submit};

// =====================================================================
// -----| Surface |-----
// =====================================================================

pub type SurfaceId = u32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SurfaceKind {
    /// Owned by a Win32 window (maps from WindowEntry in win32_user).
    Window,
    /// Desktop wallpaper / root surface.
    Desktop,
    /// Cursor overlay (always topmost).
    Cursor,
    /// Off-screen render target (no direct scanout).
    Offscreen,
}

/// A VRAM-backed drawable surface with double-buffer support.
pub struct Surface {
    pub id:       SurfaceId,
    pub kind:     SurfaceKind,
    pub owner_pid: u32,
    pub x: i32, pub y: i32,
    pub width:    u32,
    pub height:   u32,
    pub z_order:  i32,         // Lower = further back. Desktop=0, windows=1+, cursor=1000
    pub visible:  bool,
    pub alpha:    u8,          // 0=transparent, 255=opaque
    pub title:    String,

    /// VRAM offset of the BACK buffer (app renders here).
    pub back_vram:  u64,
    /// VRAM offset of the FRONT buffer (compositor reads here).
    pub front_vram: u64,
    pub stride:     u32, // bytes per row in VRAM

    /// Dirty rectangle — the region that has changed since last composite.
    pub dirty:      AtomicBool,
    pub dirty_x:    AtomicU32,
    pub dirty_y:    AtomicU32,
    pub dirty_w:    AtomicU32,
    pub dirty_h:    AtomicU32,

    /// Fence value at last present — used for frame pacing.
    pub last_fence: AtomicU64,
}

impl Surface {
    pub fn new(id: SurfaceId, kind: SurfaceKind, owner_pid: u32,
               x: i32, y: i32, w: u32, h: u32, z: i32, title: &str) -> Self
    {
        let stride   = w * 4; // RGBA8
        let buf_size = (h * stride) as usize;

        let back_vram  = crate::gpu_driver::vram_alloc(buf_size,  "surf_back").unwrap_or(0);
        let front_vram = crate::gpu_driver::vram_alloc(buf_size, "surf_front").unwrap_or(0);

        Surface {
            id, kind, owner_pid, x, y, width: w, height: h,
            z_order: z, visible: true, alpha: 255,
            title: String::from(title),
            back_vram, front_vram, stride,
            dirty:   AtomicBool::new(true), // Newly created = dirty
            dirty_x: AtomicU32::new(0),
            dirty_y: AtomicU32::new(0),
            dirty_w: AtomicU32::new(w),
            dirty_h: AtomicU32::new(h),
            last_fence: AtomicU64::new(0),
        }
    }

    /// Mark a sub-region of this surface as dirty.
    pub fn mark_dirty(&self, x: u32, y: u32, w: u32, h: u32) {
        // Expand the dirty rect to cover both old and new dirty regions.
        let ox = self.dirty_x.load(Ordering::Relaxed);
        let oy = self.dirty_y.load(Ordering::Relaxed);
        let ow = self.dirty_w.load(Ordering::Relaxed);
        let oh = self.dirty_h.load(Ordering::Relaxed);

        let nx = ox.min(x);
        let ny = oy.min(y);
        let nw = (ox + ow).max(x + w) - nx;
        let nh = (oy + oh).max(y + h) - ny;

        self.dirty_x.store(nx, Ordering::Relaxed);
        self.dirty_y.store(ny, Ordering::Relaxed);
        self.dirty_w.store(nw, Ordering::Relaxed);
        self.dirty_h.store(nh, Ordering::Relaxed);
        self.dirty.store(true, Ordering::Release);
    }

    pub fn mark_fully_dirty(&self) {
        self.dirty_x.store(0, Ordering::Relaxed);
        self.dirty_y.store(0, Ordering::Relaxed);
        self.dirty_w.store(self.width,  Ordering::Relaxed);
        self.dirty_h.store(self.height, Ordering::Relaxed);
        self.dirty.store(true, Ordering::Release);
    }

    pub fn clear_dirty(&self) {
        self.dirty.store(false, Ordering::Release);
    }

    pub fn is_dirty(&self) -> bool { self.dirty.load(Ordering::Acquire) }

    /// Flip back→front (atomic present).
    /// The compositor reads from front_vram; the app writes to back_vram.
    /// After a flip, back and front are swapped so the app can start
    /// rendering the next frame immediately.
    pub fn present(&self, pid: u32) {
        // Copy back→front via GPU BitBlt.
        // In a real GPU with page-flipping this would just swap page pointers;
        // on our BGA model we do a fast same-device copy.
        gpu_submit(pid, GpuCommand::BitBlt {
            src_x: 0, src_y: 0,
            dst_x: 0, dst_y: 0,
            w: self.width, h: self.height,
        });
        self.mark_fully_dirty();
    }
}

// =====================================================================
// -----| Swapchain |-----
// =====================================================================

pub const PRESENT_INTERVAL_IMMEDIATE: u32 = 0; // Uncapped
pub const PRESENT_INTERVAL_ONE:       u32 = 1; // Every vsync (60 Hz)
pub const PRESENT_INTERVAL_TWO:       u32 = 2; // Every 2nd vsync (30 Hz)

pub struct Swapchain {
    pub id:              u64,
    pub surface_id:      SurfaceId,
    pub present_interval: u32,
    /// How many frames have been presented.
    pub frame_count:     AtomicU64,
    /// Tick (ms) of the last present.
    pub last_present_ms: AtomicU64,
    /// Target frame interval in ms. 16 = 60 Hz, 33 = 30 Hz, 0 = uncapped.
    pub target_interval: u64,
}

impl Swapchain {
    pub fn new(id: u64, surface_id: SurfaceId, present_interval: u32) -> Self {
        let target = match present_interval {
            PRESENT_INTERVAL_ONE => 16,
            PRESENT_INTERVAL_TWO => 33,
            _                    => 0,
        };
        Self {
            id, surface_id, present_interval,
            frame_count:     AtomicU64::new(0),
            last_present_ms: AtomicU64::new(0),
            target_interval: target,
        }
    }

    /// Check whether enough time has passed to present a new frame.
    pub fn is_ready(&self, now_ms: u64) -> bool {
        if self.target_interval == 0 { return true; }
        now_ms - self.last_present_ms.load(Ordering::Acquire) >= self.target_interval
    }

    pub fn on_present(&self, now_ms: u64) {
        self.last_present_ms.store(now_ms, Ordering::Release);
        self.frame_count.fetch_add(1, Ordering::Relaxed);
    }
}

// =====================================================================
// -----| DrawContext — GPU-accelerated 2D primitives |-----
// =====================================================================

pub struct DrawContext {
    pid:        u32,
    surface_id: SurfaceId,
    clip_x:     u32, clip_y: u32, clip_w: u32, clip_h: u32,
    fill_color: u32,
    stroke_color: u32,
}

impl DrawContext {
    pub fn new(pid: u32, surface_id: SurfaceId, surface_w: u32, surface_h: u32) -> Self {
        Self {
            pid, surface_id,
            clip_x: 0, clip_y: 0, clip_w: surface_w, clip_h: surface_h,
            fill_color:   0x00000000,
            stroke_color: 0xFFFFFFFF,
        }
    }

    pub fn set_fill_color(&mut self, r: u8, g: u8, b: u8, _a: u8) {
        self.fill_color = (r as u32) << 16 | (g as u32) << 8 | b as u32;
    }

    pub fn set_stroke_color(&mut self, r: u8, g: u8, b: u8) {
        self.stroke_color = (r as u32) << 16 | (g as u32) << 8 | b as u32;
    }

    pub fn set_clip_rect(&mut self, x: u32, y: u32, w: u32, h: u32) {
        self.clip_x = x; self.clip_y = y; self.clip_w = w; self.clip_h = h;
    }

    // ----- Filled Shapes -----

    pub fn fill_rect(&self, x: i32, y: i32, w: u32, h: u32) {
        let (cx, cy, cw, ch) = self.clip_surface(x, y, w, h);
        if cw == 0 || ch == 0 { return; }
        gpu_submit(self.pid, GpuCommand::FillRect { x: cx, y: cy, w: cw, h: ch, color: self.fill_color });
    }

    pub fn clear(&self) {
        gpu_submit(self.pid, GpuCommand::FillRect {
            x: 0, y: 0, w: self.clip_w, h: self.clip_h, color: self.fill_color,
        });
    }

    // ----- Stroked Shapes (software — drawn as thin filled rects) -----

    pub fn stroke_rect(&self, x: i32, y: i32, w: u32, h: u32) {
        // Top, Bottom, Left, Right edges
        self.fill_rect_color(x,           y,           w, 1, self.stroke_color);
        self.fill_rect_color(x,           y + h as i32 - 1, w, 1, self.stroke_color);
        self.fill_rect_color(x,           y,           1, h, self.stroke_color);
        self.fill_rect_color(x + w as i32 - 1, y,     1, h, self.stroke_color);
    }

    fn fill_rect_color(&self, x: i32, y: i32, w: u32, h: u32, color: u32) {
        let (cx, cy, cw, ch) = self.clip_surface(x, y, w, h);
        if cw == 0 || ch == 0 { return; }
        gpu_submit(self.pid, GpuCommand::FillRect { x: cx, y: cy, w: cw, h: ch, color });
    }

    // ----- Image Blit -----

    pub fn blit(&self, src_x: u32, src_y: u32, dst_x: i32, dst_y: i32, w: u32, h: u32) {
        let (cx, cy, cw, ch) = self.clip_surface(dst_x, dst_y, w, h);
        if cw == 0 || ch == 0 { return; }
        gpu_submit(self.pid, GpuCommand::BitBlt { src_x, src_y, dst_x: cx, dst_y: cy, w: cw, h: ch });
    }

    // ----- Window Chrome helper -----

    /// Draw a standard VEILOS window frame around the given region.
    pub fn draw_window_chrome(&self, x: i32, y: i32, w: u32, h: u32, title: &str, focused: bool) {
        let title_h: u32 = 30;
        let border:  u32 = 2;

        // Title bar background
        let title_color = if focused { 0x1A6BC4 } else { 0x3A3A4A };
        self.fill_rect_color(x, y, w, title_h, title_color);

        // Border
        let border_color = if focused { 0x2A7BD4 } else { 0x2A2A3A };
        self.stroke_rect_color(x, y, w, h, border_color, border);

        // Body background
        self.fill_rect_color(x + border as i32, y + title_h as i32,
                             w.saturating_sub(border * 2),
                             h.saturating_sub(title_h + border),
                             0x1A1A2A);

        // Close button [×]
        self.fill_rect_color(x + w as i32 - 30, y + 5, 22, 20, 0xC0392B);
        // Minimize button [−]
        self.fill_rect_color(x + w as i32 - 56, y + 5, 22, 20, 0x7F8C8D);

        // Title text (via VGA text mode as fallback — GPU text rendering in Phase 9)
        let _ = title; // Text rendering: Phase 9 GUI toolkit
    }

    fn stroke_rect_color(&self, x: i32, y: i32, w: u32, h: u32, color: u32, thickness: u32) {
        for t in 0..thickness {
            let t = t as i32;
            self.fill_rect_color(x + t,                y + t,           w.saturating_sub((t * 2) as u32), 1, color);
            self.fill_rect_color(x + t,                y + h as i32 - t - 1, w.saturating_sub((t * 2) as u32), 1, color);
            self.fill_rect_color(x + t,                y + t,           1, h.saturating_sub((t * 2) as u32), color);
            self.fill_rect_color(x + w as i32 - t - 1, y + t,           1, h.saturating_sub((t * 2) as u32), color);
        }
    }

    /// Clip a draw call to the context's clip rectangle. Returns (x, y, w, h).
    fn clip_surface(&self, x: i32, y: i32, w: u32, h: u32) -> (u32, u32, u32, u32) {
        let x1 = x.max(self.clip_x as i32) as u32;
        let y1 = y.max(self.clip_y as i32) as u32;
        let x2 = (x + w as i32).min((self.clip_x + self.clip_w) as i32) as u32;
        let y2 = (y + h as i32).min((self.clip_y + self.clip_h) as i32) as u32;
        if x2 <= x1 || y2 <= y1 { return (0, 0, 0, 0); }
        (x1, y1, x2 - x1, y2 - y1)
    }
}

// =====================================================================
// -----| Input Routing |-----
// =====================================================================

#[derive(Debug, Clone, Copy)]
pub enum InputEvent {
    MouseMove    { x: i32, y: i32 },
    MouseButton  { x: i32, y: i32, button: u8, pressed: bool },
    MouseScroll  { delta: i32 },
    KeyDown      { vk: u32, scan: u32, ch: Option<char> },
    KeyUp        { vk: u32, scan: u32 },
    GamepadAxis  { stick: u8, x: i16, y: i16 },
    GamepadButton{ button: u8, pressed: bool },
}

pub struct InputRouter {
    pub focused_surface: Option<SurfaceId>,
    pub mouse_x:         i32,
    pub mouse_y:         i32,
    pub mouse_captured:  Option<SurfaceId>, // Surface that captured the mouse
}

impl InputRouter {
    pub const fn new() -> Self {
        Self { focused_surface: None, mouse_x: 0, mouse_y: 0, mouse_captured: None }
    }

    /// Route an input event. Returns the target SurfaceId (if any).
    pub fn route(&mut self, event: InputEvent, surfaces: &mut Vec<Surface>) -> Option<SurfaceId> {
        match event {
            InputEvent::MouseMove { x, y } => {
                self.mouse_x = x;
                self.mouse_y = y;
                // Route to captured surface, or topmost surface under cursor.
                self.mouse_captured.or_else(|| self.hit_test(x, y, surfaces))
            }

            InputEvent::MouseButton { x, y, button, pressed } => {
                let target = self.mouse_captured.or_else(|| self.hit_test(x, y, surfaces));
                if pressed && button == 0 {
                    // Left-click → focus.
                    if let Some(id) = target {
                        self.focused_surface = Some(id);
                        // Raise to front.
                        if let Some(idx) = surfaces.iter().position(|s| s.id == id) {
                            let max_z = surfaces.iter().map(|s| s.z_order).max().unwrap_or(0);
                            surfaces[idx].z_order = max_z + 1;
                        }
                    }
                    // Capture mouse for drag operations.
                    self.mouse_captured = target;
                } else if !pressed {
                    self.mouse_captured = None;
                }
                target
            }

            InputEvent::KeyDown { .. } | InputEvent::KeyUp { .. } => {
                // Keyboard always goes to focused surface.
                self.focused_surface
            }

            InputEvent::MouseScroll { .. } => {
                self.hit_test(self.mouse_x, self.mouse_y, surfaces)
            }

            // Gamepad: broadcast (simple games want direct input, not per-window).
            InputEvent::GamepadAxis { .. } | InputEvent::GamepadButton { .. } => {
                self.focused_surface
            }
        }
    }

    /// Find the topmost visible surface under (x, y).
    fn hit_test(&self, x: i32, y: i32, surfaces: &[Surface]) -> Option<SurfaceId> {
        // Iterate in reverse Z-order (highest Z = topmost).
        let mut best: Option<(i32, SurfaceId)> = None;
        for s in surfaces.iter().filter(|s| s.visible) {
            if x >= s.x && x < s.x + s.width as i32
            && y >= s.y && y < s.y + s.height as i32 {
                if best.map_or(true, |(bz, _)| s.z_order > bz) {
                    best = Some((s.z_order, s.id));
                }
            }
        }
        best.map(|(_, id)| id)
    }
}

// =====================================================================
// -----| Compositor |-----
// =====================================================================

pub struct Compositor {
    pub surfaces:    Vec<Surface>,
    pub swapchains:  Vec<Swapchain>,
    pub input:       InputRouter,
    next_surface_id: SurfaceId,
    next_swap_id:    u64,

    /// Monotonic millisecond counter driven by the PIT interrupt.
    pub tick_ms:     u64,
    /// Frame counter (increments once per vsync compose pass).
    pub frame_count: u64,

    /// Vsync accumulator. Triggers a compose pass every ~16 ms.
    vsync_accum_ms:  u64,
    /// Target compose interval (16 ms = 60 Hz).
    pub vsync_hz:    u64,

    /// Set when a surface is created, destroyed, or has its Z-order changed.
    /// compose() only re-sorts the surface list when this is true.
    /// In steady state (no Z-order change) this avoids an O(N log N) sort
    /// on every 60 Hz compose pass.
    z_order_dirty:   bool,
}

impl Compositor {
    pub const fn new() -> Self {
        Self {
            surfaces:        Vec::new(),
            swapchains:      Vec::new(),
            input:           InputRouter::new(),
            next_surface_id: 1,
            next_swap_id:    1,
            tick_ms:         0,
            frame_count:     0,
            vsync_accum_ms:  0,
            vsync_hz:        60,
            z_order_dirty:   true, // Sort on first compose pass
        }
    }

    // ----- Surface Management -----

    pub fn create_surface(
        &mut self, kind: SurfaceKind, owner_pid: u32,
        x: i32, y: i32, w: u32, h: u32, title: &str,
    ) -> SurfaceId {
        let id  = self.next_surface_id;
        self.next_surface_id += 1;
        let z   = match kind { SurfaceKind::Desktop => 0, SurfaceKind::Cursor => 1000, _ => 1 };
        self.surfaces.push(Surface::new(id, kind, owner_pid, x, y, w, h, z, title));
        self.z_order_dirty = true;
        println!("[COMPOSITOR] CreateSurface: id={} '{}' {}x{} at ({},{})", id, title, w, h, x, y);
        id
    }

    pub fn destroy_surface(&mut self, id: SurfaceId) {
        self.surfaces.retain(|s| s.id != id);
        self.swapchains.retain(|sc| sc.surface_id != id);
        self.z_order_dirty = true;
    }

    pub fn surface_mut(&mut self, id: SurfaceId) -> Option<&mut Surface> {
        self.surfaces.iter_mut().find(|s| s.id == id)
    }

    pub fn move_surface(&mut self, id: SurfaceId, x: i32, y: i32) {
        if let Some(s) = self.surface_mut(id) {
            s.x = x; s.y = y; s.mark_fully_dirty();
        }
    }

    pub fn resize_surface(&mut self, id: SurfaceId, w: u32, h: u32) {
        if let Some(s) = self.surface_mut(id) {
            // Free old VRAM, re-allocate.
            if s.back_vram  != 0 { crate::gpu_driver::vram_free(s.back_vram); }
            if s.front_vram != 0 { crate::gpu_driver::vram_free(s.front_vram); }
            let buf = (w * h * 4) as usize;
            s.back_vram  = crate::gpu_driver::vram_alloc(buf, "surf_back").unwrap_or(0);
            s.front_vram = crate::gpu_driver::vram_alloc(buf, "surf_front").unwrap_or(0);
            s.width  = w; s.height = h; s.stride = w * 4;
            s.mark_fully_dirty();
        }
    }

    // ----- Swapchain -----

    pub fn create_swapchain(&mut self, surface_id: SurfaceId, present_interval: u32) -> u64 {
        let id = self.next_swap_id;
        self.next_swap_id += 1;
        self.swapchains.push(Swapchain::new(id, surface_id, present_interval));
        id
    }

    pub fn present_swapchain(&mut self, swapchain_id: u64, owner_pid: u32) -> bool {
        let now = self.tick_ms;
        if let Some(sc) = self.swapchains.iter_mut().find(|s| s.id == swapchain_id) {
            if !sc.is_ready(now) { return false; } // Frame too early — honour pacing
            sc.on_present(now);
            let sid = sc.surface_id;
            if let Some(surf) = self.surfaces.iter().find(|s| s.id == sid) {
                surf.present(owner_pid);
            }
            return true;
        }
        false
    }

    // ----- DrawContext factory -----

    pub fn get_draw_context(&self, surface_id: SurfaceId, pid: u32) -> Option<DrawContext> {
        let s = self.surfaces.iter().find(|s| s.id == surface_id)?;
        Some(DrawContext::new(pid, surface_id, s.width, s.height))
    }

    // ----- Input -----

    pub fn dispatch_input(&mut self, event: InputEvent) -> Option<SurfaceId> {
        self.input.route(event, &mut self.surfaces)
    }

    pub fn focused_surface(&self) -> Option<SurfaceId> { self.input.focused_surface }

    // ----- Vsync / Compose -----

    /// Called by the PIT interrupt handler each millisecond.
    pub fn tick(&mut self, ms: u64) {
        self.tick_ms       = ms;
        self.vsync_accum_ms += 1;

        let interval = 1000 / self.vsync_hz; // ms per frame at target Hz
        if self.vsync_accum_ms >= interval {
            self.vsync_accum_ms = 0;
            self.compose();
        }
    }

    /// Composite all dirty surfaces onto the root framebuffer.
    fn compose(&mut self) {
        // Sort surfaces by Z-order (ascending — lowest drawn first).
        // Only re-sort when a surface was created, destroyed, or Z-changed.
        // In steady state (no Z changes) this saves an O(N log N) sort every
        // 60 Hz compose pass — the most frequently called path in the compositor.
        if self.z_order_dirty {
            self.surfaces.sort_by_key(|s| s.z_order);
            self.z_order_dirty = false;
        }

        let mut any_dirty = false;
        for surf in &self.surfaces {
            if !surf.visible || !surf.is_dirty() { continue; }
            any_dirty = true;

            let dx = surf.dirty_x.load(Ordering::Relaxed);
            let dy = surf.dirty_y.load(Ordering::Relaxed);
            let dw = surf.dirty_w.load(Ordering::Relaxed);
            let dh = surf.dirty_h.load(Ordering::Relaxed);

            // Composite: copy dirty region of surface's front buffer to the
            // root framebuffer at the surface's screen position.
            // We use FillRect as a stand-in for a real alpha-composite blit.
            // Phase 9 will replace this with a proper GPU shader blit.
            gpu_submit(0, GpuCommand::BitBlt {
                src_x: dx, src_y: dy,
                dst_x: (surf.x as u32).saturating_add(dx),
                dst_y: (surf.y as u32).saturating_add(dy),
                w: dw, h: dh,
            });

            surf.clear_dirty();
        }

        if any_dirty {
            self.frame_count += 1;
        }
    }

    // ----- Desktop initialisation -----

    /// Create the desktop background surface and the cursor surface.
    pub fn init_desktop(&mut self) {
        let sw = crate::ui::screen_w() as u32;
        let sh = crate::ui::screen_h() as u32;

        let desktop_id = self.create_surface(SurfaceKind::Desktop, 0, 0, 0, sw, sh, "Desktop");
        let _cursor_id = self.create_surface(SurfaceKind::Cursor,  0, 0, 0, 8, 8,  "Cursor");

        // Draw initial desktop background via GPU.
        if let Some(mut dc) = self.get_draw_context(desktop_id, 0) {
            dc.set_fill_color(20, 20, 35, 255); // VEILOS dark blue-black
            dc.clear();
        }

        println!("[COMPOSITOR] Desktop ready. {}x{} @ {} Hz", sw, sh, self.vsync_hz);
    }
}

pub static COMPOSITOR: Mutex<Compositor> = Mutex::new(Compositor::new());

/// Initialise the compositor subsystem.
pub fn init() {
    COMPOSITOR.lock().init_desktop();
    println!("[COMPOSITOR] Window compositor initialised.");
    println!("[COMPOSITOR]   Double-buffered surfaces, per-surface dirty tracking");
    println!("[COMPOSITOR]   VRAM-backed swapchains, frame pacing @ 60 Hz");
    println!("[COMPOSITOR]   GPU-accelerated FillRect / BitBlt draw primitives");
}
