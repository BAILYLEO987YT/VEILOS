// -----| VEILOS Desktop Environment |-----
//
// Phase 9 roadmap items implemented here:
//
//   [x] Implement window manager
//   [x] Implement panel/taskbar
//   [x] Implement notification system
//   [x] Implement settings UI
//
// Architecture
// ------------
//
//   DesktopWindowManager
//     Manages a stack of DesktopWindows. Each window wraps a Widget tree
//     root. The DWM handles Z-order, focus, move/resize, minimize/maximize,
//     and renders the complete scene on each vsync tick.
//
//   Panel (taskbar)
//     A 40px bar anchored to the top of the screen containing:
//       - VEILOS logo / launcher button (left)
//       - Pinned taskbar buttons (center-left)
//       - Running windows list (center)
//       - Notification tray (right)
//       - Clock (right)
//       - Security/Performance policy toggle (far right)
//
//   NotificationSystem
//     Notifications are displayed as toast pop-ups in the bottom-right.
//     Each notification has a category (Info/Warning/Error/Success), title,
//     body, and an expiry tick. The system queues up to 5 simultaneous
//     toasts and auto-dismisses them after their TTL.
//
//   SettingsUI
//     The "SYSTEM CUSTOMISATION HUB" window now has real tabs built from the
//     widget toolkit: Appearance (theme), Display, Security, About.

#![allow(dead_code)]

use alloc::vec::Vec;
use alloc::string::String;
use alloc::boxed::Box;
use spin::Mutex;
use core::sync::atomic::{AtomicU32, AtomicU8, AtomicBool, Ordering};
use crate::println;
use crate::ui::{screen_w, screen_h};
use crate::graphics::Color;
use crate::gui_toolkit::{Widget, Renderer, Rect, ThemeKind, ACTIVE_THEME};
use crate::distributed::NodeId;
use smoltcp::wire::Ipv4Address;

// Tracking for input state when the executor is frozen.
static LAST_MB: AtomicBool = AtomicBool::new(false);
static LAST_RMB: AtomicBool = AtomicBool::new(false);

/// Screen brightness, 0-100 (the Quick Settings slider). With no backlight to
/// drive, below 100 the picture is dimmed on its way to the screen
/// (`ui::commit_dirty_rect`), never below 10%.
pub static BRIGHTNESS: core::sync::atomic::AtomicI32 = core::sync::atomic::AtomicI32::new(100);
static LAST_MX: AtomicU32  = AtomicU32::new(0);
static LAST_MY: AtomicU32  = AtomicU32::new(0);

// Pending close_silent requests from destroy_window() when DESKTOP_WM was busy.
static PENDING_CLOSE: Mutex<Vec<u32>> = Mutex::new(Vec::new());

/// Schedule a close_silent for the next time poll_input() drains the queue.
/// Only called when DESKTOP_WM is locked and cannot be acquired immediately.
pub fn schedule_close_silent(veilos_id: u32) {
    PENDING_CLOSE.lock().push(veilos_id);
}

// Pending heavy clicks (desktop icon opens, app launcher) deferred from the
// timer ISR.  These operations involve massive heap allocations (PNG decode,
// PE loading, text file reads) and MUST NOT run inside the timer ISR where
// re-entrant heap access would corrupt the linked-list allocator, causing a
// null-pointer dereference page fault.
//
// poll_input() queues clicks here instead of dispatching them immediately.
// drain_deferred_clicks() is called from gui_refresh_task (safe executor
// context) every frame to actually execute them.
static DEFERRED_CLICKS: Mutex<Vec<PendingClick>> = Mutex::new(Vec::new());

/// Drain and dispatch any deferred desktop-icon / app-launcher clicks.
/// MUST be called from the executor (gui_refresh_task), never from the ISR.
pub fn drain_deferred_clicks() {
    let clicks: Vec<PendingClick> = {
        let mut q = DEFERRED_CLICKS.lock();
        if q.is_empty() { return; }
        core::mem::take(&mut *q)
    };
    for click in clicks {
        dispatch_pending_click(click);
    }
}

// =====================================================================
// -----| Desktop Window |-----
// =====================================================================

static NEXT_WIN_ID: AtomicU32 = AtomicU32::new(100);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowState { Normal, Minimized, Maximized, Closing }

pub struct DesktopWindow {
    pub id:      u32,
    pub title:   String,
    pub rect:    Rect,           // Current position and size
    pub saved_rect: Rect,        // Saved normal rect (for restore from maximize)
    pub state:   WindowState,
    pub z_order: i32,
    pub resizable: bool,
    pub owner_pid: u32,
    pub root:    Option<Box<dyn Widget>>,  // Widget tree root
    pub dirty:   bool,
    /// No frame, title bar or taskbar button (a Windows program's popup).
    pub frameless: bool,
    /// A taskbar button (not for frameless or owned windows).
    pub taskbar: bool,
}

impl DesktopWindow {
    pub fn new(id: u32, title: &str, x: i32, y: i32, w: u32, h: u32, pid: u32) -> Self {
        let rect = Rect::new(x, y, w, h);
        Self {
            id, title: String::from(title), rect, saved_rect: rect,
            state: WindowState::Normal, z_order: 0, resizable: true,
            owner_pid: pid, root: None, dirty: true, frameless: false, taskbar: true,
        }
    }

    pub fn set_content(&mut self, widget: Box<dyn Widget>) {
        self.root = Some(widget);
        self.dirty = true;
    }

    pub fn paint(&self, r: &Renderer) {
        if self.state == WindowState::Minimized { return; }
        let t = ACTIVE_THEME.lock();
        let p = t.palette;
        let title_h = t.titlebar_height;
        drop(t);

        if self.frameless {
            crate::nt_user::set_content_rect(self.id, self.rect.x, self.rect.y, self.rect.w, self.rect.h);
            if let Some(ref root) = self.root { root.paint(r); }
            return;
        }

        // Shadow
        r.fill_rect(Rect::new(self.rect.x + 4, self.rect.y + 4, self.rect.w, self.rect.h),
            Color { r: 0, g: 0, b: 0 });

        // Frame border
        r.stroke_rect(self.rect, p.border);

        // Title bar
        let tb = Rect::new(self.rect.x, self.rect.y, self.rect.w, title_h);
        r.fill_rect(tb, p.titlebar);
        r.draw_text(&self.title, self.rect.x + 10, self.rect.y + (title_h as i32 - 8) / 2, p.titlebar_text);

        // Window control buttons
        let btn_y   = self.rect.y + (title_h as i32 - 20) / 2;
        let close_x = self.rect.x + self.rect.w as i32 - 28;
        let max_x   = close_x - 26;
        let min_x   = max_x   - 26;

        // Close [×]
        r.fill_rect(Rect::new(close_x, btn_y, 20, 20), p.danger);
        r.draw_text("x", close_x + 6, btn_y + 5, p.text_primary);
        // Maximize [+] or restore [↓]
        r.fill_rect(Rect::new(max_x, btn_y, 20, 20), p.success);
        let max_lbl = if self.state == WindowState::Maximized { "v" } else { "^" };
        r.draw_text(max_lbl, max_x + 6, btn_y + 5, p.text_primary);
        // Minimize [−]
        r.fill_rect(Rect::new(min_x, btn_y, 20, 20), p.warning);
        r.draw_text("-", min_x + 7, btn_y + 5, p.text_primary);

        // Content area
        let content_y = self.rect.y + title_h as i32;
        let content_h = self.rect.h.saturating_sub(title_h);
        r.fill_rect(Rect::new(self.rect.x + 1, content_y, self.rect.w - 2, content_h), p.background);

        // Paint widget tree
        crate::nt_user::set_content_rect(self.id, self.rect.x + 1, content_y, self.rect.w - 2, content_h);
        if let Some(ref root) = self.root { root.paint(r); }
    }

    pub fn hit_test(&self, x: i32, y: i32) -> WindowHit {
        if self.frameless {
            return if self.rect.contains(x, y) { WindowHit::Content } else { WindowHit::None };
        }
        if !self.rect.contains(x, y) {
            if self.resizable && self.state != WindowState::Minimized && self.state != WindowState::Maximized {
                let rx = self.rect.x;
                let ry = self.rect.y;
                let rw = self.rect.w as i32;
                let rh = self.rect.h as i32;
                let margin = 6;
                let on_left = x >= rx - margin && x <= rx + margin && y >= ry - margin && y <= ry + rh + margin;
                let on_right = x >= rx + rw - margin && x <= rx + rw + margin && y >= ry - margin && y <= ry + rh + margin;
                let on_top = y >= ry - margin && y <= ry + margin && x >= rx - margin && x <= rx + rw + margin;
                let on_bottom = y >= ry + rh - margin && y <= ry + rh + margin && x >= rx - margin && x <= rx + rw + margin;
                if on_left && on_top { return WindowHit::ResizeTopLeft; }
                if on_right && on_top { return WindowHit::ResizeTopRight; }
                if on_left && on_bottom { return WindowHit::ResizeBottomLeft; }
                if on_right && on_bottom { return WindowHit::ResizeBottomRight; }
                if on_left { return WindowHit::ResizeLeft; }
                if on_right { return WindowHit::ResizeRight; }
                if on_top { return WindowHit::ResizeTop; }
                if on_bottom { return WindowHit::ResizeBottom; }
            }
            return WindowHit::None;
        }
        let t = ACTIVE_THEME.lock();
        let title_h = t.titlebar_height as i32;
        drop(t);
        let btn_y   = self.rect.y + (title_h - 20) / 2;
        let close_x = self.rect.x + self.rect.w as i32 - 28;
        let max_x   = close_x - 26;
        let min_x   = max_x   - 26;

        if y >= btn_y && y < btn_y + 20 {
            if x >= close_x && x < close_x + 20 { return WindowHit::Close; }
            if x >= max_x   && x < max_x + 20   { return WindowHit::MaximizeRestore; }
            if x >= min_x   && x < min_x + 20   { return WindowHit::Minimize; }
        }

        if self.resizable && self.state != WindowState::Minimized && self.state != WindowState::Maximized {
            let margin = 6;
            let rx = self.rect.x;
            let ry = self.rect.y;
            let rw = self.rect.w as i32;
            let rh = self.rect.h as i32;
            let near_left = x >= rx && x <= rx + margin;
            let near_right = x >= rx + rw - margin && x <= rx + rw;
            let near_top = y >= ry && y <= ry + margin;
            let near_bottom = y >= ry + rh - margin && y <= ry + rh;

            if near_left && near_top { return WindowHit::ResizeTopLeft; }
            if near_right && near_top { return WindowHit::ResizeTopRight; }
            if near_left && near_bottom { return WindowHit::ResizeBottomLeft; }
            if near_right && near_bottom { return WindowHit::ResizeBottomRight; }
            if near_left { return WindowHit::ResizeLeft; }
            if near_right { return WindowHit::ResizeRight; }
            if near_top { return WindowHit::ResizeTop; }
            if near_bottom { return WindowHit::ResizeBottom; }
        }

        if y >= self.rect.y && y < self.rect.y + title_h { return WindowHit::TitleBar; }
        WindowHit::Content
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowHit { None, TitleBar, Close, Minimize, MaximizeRestore, Content, ResizeLeft, ResizeRight, ResizeTop, ResizeBottom, ResizeTopLeft, ResizeTopRight, ResizeBottomLeft, ResizeBottomRight }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowSnapMode { Magnetic, Sticky, Slippery, Off }

/// Returned by on_mouse_down() when a content-area click needs to be
/// forwarded to a native app.  The caller must dispatch this AFTER
/// releasing the DESKTOP_WM lock to avoid re-entrant deadlocks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PendingClick {
    None,
    AppContent { window_id: u32, x: i32, y: i32 },
    RightClick { window_id: u32, x: i32, y: i32 },
    OpenAppLauncher,
    DesktopIcon(String),
    DesktopIconRightClick(String),
    DesktopRightClick,
}

// =====================================================================
// -----| Desktop Window Manager |-----
// =====================================================================

pub struct DesktopWindowManager {
    pub windows:       Vec<DesktopWindow>,
    pub focused_id:    Option<u32>,
    pub dragging_id:   Option<u32>,
    pub drag_offset_x: i32,
    pub drag_offset_y: i32,
    pub renderer:      Renderer,
    /// At 60 Hz the vast majority of frames have no Z-order change, so we
    /// skip the O(N log N) sort on almost every frame.
    pub z_order_dirty: bool,
    pub window_tiles: (u32, u32),
    pub window_tile_size_ratio_x: Vec<u32>,
    pub window_tile_size_ratio_y: Vec<u32>,
    pub window_snap: WindowSnapMode,
    pub window_stack: bool,
    pub resizing_id: Option<u32>,
    pub resize_hit: WindowHit,
    pub resize_start_rect: Rect,
    pub resize_start_mx: i32,
    pub resize_start_my: i32,
}

impl DesktopWindowManager {
    pub const fn new() -> Self {
        Self {
            windows:       Vec::new(),
            focused_id:    None,
            dragging_id:   None,
            drag_offset_x: 0,
            drag_offset_y: 0,
            renderer:      Renderer { pid: 0 },
            z_order_dirty: false,
            window_tiles: (1, 1),
            window_tile_size_ratio_x: Vec::new(),
            window_tile_size_ratio_y: Vec::new(),
            window_snap: WindowSnapMode::Off,
            window_stack: true,
            resizing_id: None,
            resize_hit: WindowHit::None,
            resize_start_rect: Rect::new(0, 0, 0, 0),
            resize_start_mx: 0,
            resize_start_my: 0,
        }
    }

    pub fn get_grid_lines(&self) -> (Vec<i32>, Vec<i32>) {
        let mut x_lines = Vec::new();
        let mut y_lines = Vec::new();
        
        let screen_w = crate::ui::screen_w() as i32;
        let (top, bottom) = work_area();

        x_lines.push(0);
        
        let tiles_x = self.window_tiles.0.max(1) as i32;
        let tiles_y = self.window_tiles.1.max(1) as i32;
        
        let mut sum_x: u32 = self.window_tile_size_ratio_x.iter().sum();
        if sum_x == 0 { sum_x = tiles_x as u32; }
        let mut cur_x = 0;
        for i in 0..tiles_x {
            let ratio = *self.window_tile_size_ratio_x.get(i as usize).unwrap_or(&1);
            cur_x += (screen_w * ratio as i32) / sum_x as i32;
            if i < tiles_x - 1 { x_lines.push(cur_x); }
        }
        x_lines.push(screen_w);
        
        y_lines.push(top);

        let mut sum_y: u32 = self.window_tile_size_ratio_y.iter().sum();
        if sum_y == 0 { sum_y = tiles_y as u32; }
        let mut cur_y = top;
        for i in 0..tiles_y {
            let ratio = *self.window_tile_size_ratio_y.get(i as usize).unwrap_or(&1);
            cur_y += ((bottom - top) * ratio as i32) / sum_y as i32;
            if i < tiles_y - 1 { y_lines.push(cur_y); }
        }
        y_lines.push(bottom);
        
        (x_lines, y_lines)
    }

    pub fn get_snap_dist(&self) -> i32 {
        match self.window_snap {
            WindowSnapMode::Magnetic => 40,
            WindowSnapMode::Sticky => 20,
            WindowSnapMode::Slippery => 10,
            WindowSnapMode::Off => 0,
        }
    }

    pub fn snap_value(val: i32, lines: &[i32], snap_dist: i32) -> i32 {
        for &line in lines {
            if (val - line).abs() < snap_dist {
                return line;
            }
        }
        val
    }

    pub fn open(&mut self, title: &str, mut x: i32, mut y: i32, mut w: u32, mut h: u32, pid: u32) -> u32 {
        if !self.window_stack && self.window_tiles != (1, 1) {
            let (x_lines, y_lines) = self.get_grid_lines();
            let mut found = false;
            for iy in 0..self.window_tiles.1 as usize {
                for ix in 0..self.window_tiles.0 as usize {
                    if ix + 1 >= x_lines.len() || iy + 1 >= y_lines.len() { continue; }
                    let tx = x_lines[ix];
                    let ty = y_lines[iy];
                    let tw = x_lines[ix+1] - tx;
                    let th = y_lines[iy+1] - ty;
                    
                    let mut occupied = false;
                    for win in &self.windows {
                        if win.state != WindowState::Minimized {
                            let cx = win.rect.x + (win.rect.w / 2) as i32;
                            let cy = win.rect.y + (win.rect.h / 2) as i32;
                            if cx >= tx && cx < tx + tw && cy >= ty && cy < ty + th {
                                occupied = true;
                                break;
                            }
                        }
                    }
                    if !occupied {
                        x = tx; y = ty;
                        w = tw as u32; h = th as u32;
                        found = true;
                        break;
                    }
                }
                if found { break; }
            }
        }

        let id = NEXT_WIN_ID.fetch_add(1, Ordering::Relaxed);
        let max_z = self.windows.iter().map(|w| w.z_order).max().unwrap_or(0);
        let mut win = DesktopWindow::new(id, title, x, y, w, h, pid);
        win.z_order = max_z + 1;
        self.windows.push(win);
        self.focused_id = Some(id);
        self.z_order_dirty = true;
        id
    }

    pub fn close(&mut self, id: u32) {
        // A Windows program's window (nt_user): WM_CLOSE, and the program
        // decides; DestroyWindow removes it.
        if crate::nt_user::deliver(id, crate::nt_user::WM_CLOSE, 0, 0) {
            // If it then has no windows but keeps running, it's ended
            // (win32_user::check_close_watch).
            if let Some(win) = self.windows.iter().find(|w| w.id == id) {
                crate::win32_user::watch_close(win.owner_pid);
            }
            return;
        }
        if let Some(win) = self.windows.iter().find(|w| w.id == id) {
            let pid = win.owner_pid;

            // Look up Win32 hwnd in one lock acquisition. Tracked: close() runs
            // from poll_input(), which the timer ISR calls directly whenever a
            // process is (or was recently) active — an abandoned WIN32_USER
            // guard here is exactly as reachable as from a syscall handler.
            let hwnd = crate::loader::lock_guard::lock_tracked(&crate::win32_user::WIN32_USER)
                .find_hwnd_by_veilos_id(id);
            if let Some(hwnd) = hwnd {
                // WM_CLOSE only, as on Windows: the app decides (DefWindowProc
                // destroys the window and its WM_DESTROY handler posts WM_QUIT,
                // or it asks to save, or it hides to the tray). Forcing WM_QUIT
                // as well used to end apps that meant to stay open.
                let _ = hwnd;
                crate::win32_user::dispatch_veilos_input(id, crate::win32_user::WM_CLOSE, 0, 0);
                // If the app then has no windows but keeps running, it's ended
                // (win32_user::check_close_watch).
                crate::win32_user::watch_close(pid);
            } else if let Some(console) = crate::apps::TERMINAL_POOL.iter()
                .filter_map(|t| t.try_lock())
                .find(|t| t.window_id == id)
                .and_then(|t| t.console.clone())
                .filter(|c| c.is_live())
            {
                // A console window: closing it ends its program, as on Windows
                // (exit code STATUS_CONTROL_C_EXIT).
                if let Some(proc) = crate::process::PROCESSOR.lock().find(console.pid()) {
                    proc.terminate(0xC000_013Au32 as i32);
                    crate::loader::request_kill(console.pid());
                }
            } else {
                // Non-Win32 window: just terminate the owning process.
                if pid > 0 {
                    if let Some(proc) = crate::process::PROCESSOR.lock().find(pid) {
                        proc.terminate(0);
                    }
                }
            }
        }

        // Remove the DesktopWindow in all cases.
        // For Win32 windows, destroy_window() above already called close_silent()
        // via try_lock — but since we're about to call retain() anyway, any
        // duplicate removal is harmless (retain with no match is a no-op).
        self.windows.retain(|w| w.id != id);
        if self.focused_id == Some(id) {
            self.focused_id = self.windows.last().map(|w| w.id);
        }
        self.z_order_dirty = true;
    }

    /// Remove a desktop window without sending WM_CLOSE again.
    /// Called by win32_user::destroy_window() after the Win32 side is already
    /// torn down, so we only need to drop the compositor frame.
    pub fn close_silent(&mut self, id: u32) {
        self.windows.retain(|w| w.id != id);
        if self.focused_id == Some(id) {
            self.focused_id = self.windows.last().map(|w| w.id);
        }
        self.z_order_dirty = true;
    }

    pub fn focus(&mut self, id: u32) {
        if self.focused_id == Some(id) { return; } // already focused, no Z-change needed
        self.focused_id = Some(id);
        let max_z = self.windows.iter().map(|w| w.z_order).max().unwrap_or(0);

        // Normalise Z-order if it would overflow i32 (after ~2 billion clicks, but defensive).
        // Keep values in 0..N by reassigning sequential ranks before bumping the new focus.
        if max_z > 100_000 {
            self.windows.sort_by_key(|w| w.z_order);
            for (rank, w) in self.windows.iter_mut().enumerate() {
                w.z_order = rank as i32;
            }
        }
        let new_max = self.windows.iter().map(|w| w.z_order).max().unwrap_or(0) + 1;
        if let Some(w) = self.windows.iter_mut().find(|w| w.id == id) {
            w.z_order = new_max;
        }
        self.z_order_dirty = true;
    }

    pub fn minimize(&mut self, id: u32) {
        if let Some(w) = self.windows.iter_mut().find(|w| w.id == id) {
            w.state = WindowState::Minimized;
        }
    }

    pub fn maximize_restore(&mut self, id: u32) {
        if let Some(w) = self.windows.iter_mut().find(|w| w.id == id) {
            match w.state {
                WindowState::Maximized => {
                    w.rect  = w.saved_rect;
                    w.state = WindowState::Normal;
                }
                WindowState::Normal => {
                    w.saved_rect = w.rect;
                    let (top, bottom) = work_area();
                    w.rect  = Rect::new(0, top, screen_w() as u32, (bottom - top) as u32);
                    w.state = WindowState::Maximized;
                }
                _ => {}
            }
        }
    }

    /// The window whose content is at (x, y), if the topmost there is one.
    pub fn content_at(&self, x: i32, y: i32) -> Option<u32> {
        self.windows.iter().rev().find_map(|w| match w.hit_test(x, y) {
            WindowHit::None => None,
            WindowHit::Content => Some(Some(w.id)),
            _ => Some(None),
        }).flatten()
    }

    pub fn on_mouse_down(&mut self, x: i32, y: i32) -> PendingClick {
        // Hit-test in reverse Z order (topmost first).
        // windows is kept sorted by repaint_all(), so we just reverse-iterate.
        let n = self.windows.len();
        for rev_i in 0..n {
            let i = n - 1 - rev_i; // highest Z first
            let hit = self.windows[i].hit_test(x, y);
            let id  = self.windows[i].id;
            match hit {
                WindowHit::None => continue,
                WindowHit::Close             => { self.close(id); return PendingClick::None; }
                WindowHit::Minimize          => { self.minimize(id); return PendingClick::None; }
                WindowHit::MaximizeRestore   => { self.maximize_restore(id); return PendingClick::None; }
                WindowHit::TitleBar => {
                    // Read drag offsets BEFORE calling self.focus() which mutates z_order.
                    let rect_x = self.windows[i].rect.x;
                    let rect_y = self.windows[i].rect.y;
                    self.focus(id);
                    self.dragging_id   = Some(id);
                    self.drag_offset_x = x - rect_x;
                    self.drag_offset_y = y - rect_y;
                    return PendingClick::None;
                }
                WindowHit::ResizeLeft | WindowHit::ResizeRight | WindowHit::ResizeTop | WindowHit::ResizeBottom |
                WindowHit::ResizeTopLeft | WindowHit::ResizeTopRight | WindowHit::ResizeBottomLeft | WindowHit::ResizeBottomRight => {
                    self.focus(id);
                    self.resizing_id = Some(id);
                    self.resize_hit = hit;
                    self.resize_start_rect = self.windows[i].rect.clone();
                    self.resize_start_mx = x;
                    self.resize_start_my = y;
                    return PendingClick::None;
                }
                WindowHit::Content => {
                    self.focus(id);
                    // Return the pending click — caller MUST dispatch this
                    // after releasing the DESKTOP_WM lock to avoid deadlock,
                    // since app handle_click() methods re-lock DESKTOP_WM.
                    return PendingClick::AppContent { window_id: id, x, y };
                }
            }
        }
        PendingClick::None
    }

    pub fn on_mouse_up(&mut self) {
        self.dragging_id = None;
        self.resizing_id = None;
    }

    pub fn handle_arrow_navigation(&mut self, key: pc_keyboard::KeyCode) -> bool {
        let is_arrow = matches!(key, pc_keyboard::KeyCode::ArrowUp | pc_keyboard::KeyCode::ArrowDown | pc_keyboard::KeyCode::ArrowLeft | pc_keyboard::KeyCode::ArrowRight);
        if !is_arrow { return false; }

        let id = match self.focused_id {
            Some(i) => i,
            None => return false,
        };
        let (x_lines, y_lines) = self.get_grid_lines();
        if x_lines.len() < 2 || y_lines.len() < 2 { return true; }

        let w_rect = {
            if let Some(w) = self.windows.iter().find(|w| w.id == id) {
                if w.state == WindowState::Minimized || w.state == WindowState::Maximized { return true; }
                w.rect.clone()
            } else { return true; }
        };

        let cx = w_rect.x + (w_rect.w / 2) as i32;
        let cy = w_rect.y + (w_rect.h / 2) as i32;
        
        let mut t_x = 0;
        for i in 0..x_lines.len()-1 {
            if cx >= x_lines[i] && cx < x_lines[i+1] { t_x = i; break; }
        }
        let mut t_y = 0;
        for i in 0..y_lines.len()-1 {
            if cy >= y_lines[i] && cy < y_lines[i+1] { t_y = i; break; }
        }

        let old_tx = x_lines[t_x];
        let old_ty = y_lines[t_y];
        let old_tw = (x_lines[t_x+1] - old_tx) as u32;
        let old_th = (y_lines[t_y+1] - old_ty) as u32;

        let orig_tx = t_x;
        let orig_ty = t_y;

        match key {
            pc_keyboard::KeyCode::ArrowUp => if t_y > 0 { t_y -= 1; },
            pc_keyboard::KeyCode::ArrowDown => if t_y + 1 < y_lines.len() - 1 { t_y += 1; },
            pc_keyboard::KeyCode::ArrowLeft => if t_x > 0 { t_x -= 1; },
            pc_keyboard::KeyCode::ArrowRight => if t_x + 1 < x_lines.len() - 1 { t_x += 1; },
            _ => return true,
        }

        if orig_tx == t_x && orig_ty == t_y { return true; }

        let tx = x_lines[t_x];
        let ty = y_lines[t_y];
        let tw = (x_lines[t_x+1] - tx) as u32;
        let th = (y_lines[t_y+1] - ty) as u32;

        if !self.window_stack {
            let mut swap_id = None;
            for w in &self.windows {
                if w.id != id && w.state != WindowState::Minimized && w.state != WindowState::Maximized {
                    let wcx = w.rect.x + (w.rect.w / 2) as i32;
                    let wcy = w.rect.y + (w.rect.h / 2) as i32;
                    if wcx >= tx && wcx < tx + tw as i32 && wcy >= ty && wcy < ty + th as i32 {
                        swap_id = Some(w.id);
                        break;
                    }
                }
            }
            if let Some(sid) = swap_id {
                if let Some(sw) = self.windows.iter_mut().find(|w| w.id == sid) {
                    sw.rect.x = old_tx;
                    sw.rect.y = old_ty;
                    sw.rect.w = old_tw;
                    sw.rect.h = old_th;
                    sw.dirty = true;
                }
            }
        }

        if let Some(w) = self.windows.iter_mut().find(|w| w.id == id) {
            w.rect.x = tx;
            w.rect.y = ty;
            w.rect.w = tw;
            w.rect.h = th;
            w.dirty = true;
        }
        
        true
    }

    pub fn on_mouse_move(&mut self, x: i32, y: i32) {
        let snap_dist = self.get_snap_dist();
        let (x_lines, y_lines) = if snap_dist > 0 { self.get_grid_lines() } else { (Vec::new(), Vec::new()) };

        if let Some(id) = self.dragging_id {
            let ox = self.drag_offset_x;
            let oy = self.drag_offset_y;
            if let Some(w) = self.windows.iter_mut().find(|w| w.id == id) {
                let mut nx = x - ox;
                let top = work_area().0;
                let mut ny = (y - oy).max(top);
                
                if snap_dist > 0 {
                    // Snap left edge
                    let snapped_x = Self::snap_value(nx, &x_lines, snap_dist);
                    if snapped_x != nx { nx = snapped_x; }
                    else {
                        // Snap right edge
                        let snapped_right = Self::snap_value(nx + w.rect.w as i32, &x_lines, snap_dist);
                        if snapped_right != nx + w.rect.w as i32 {
                            nx = snapped_right - w.rect.w as i32;
                        }
                    }
                    
                    // Snap top edge
                    let snapped_y = Self::snap_value(ny, &y_lines, snap_dist);
                    if snapped_y != ny { ny = snapped_y; }
                    else {
                        // Snap bottom edge
                        let snapped_bottom = Self::snap_value(ny + w.rect.h as i32, &y_lines, snap_dist);
                        if snapped_bottom != ny + w.rect.h as i32 {
                            ny = snapped_bottom - w.rect.h as i32;
                        }
                    }
                }
                
                w.rect.x = nx;
                w.rect.y = ny.max(top);
                w.dirty  = true;
            }
        } else if let Some(id) = self.resizing_id {
            let resize_hit = self.resize_hit;
            let resize_start_mx = self.resize_start_mx;
            let resize_start_my = self.resize_start_my;
            let resize_start_rect = self.resize_start_rect;
            
            if let Some(w) = self.windows.iter_mut().find(|w| w.id == id) {
                let dx = x - resize_start_mx;
                let dy = y - resize_start_my;
                let mut nx = resize_start_rect.x;
                let mut ny = resize_start_rect.y;
                let mut nw = resize_start_rect.w as i32;
                let mut nh = resize_start_rect.h as i32;

                match resize_hit {
                    WindowHit::ResizeLeft | WindowHit::ResizeTopLeft | WindowHit::ResizeBottomLeft => {
                        nx += dx; nw -= dx;
                    }
                    WindowHit::ResizeRight | WindowHit::ResizeTopRight | WindowHit::ResizeBottomRight => {
                        nw += dx;
                    }
                    _ => {}
                }
                match resize_hit {
                    WindowHit::ResizeTop | WindowHit::ResizeTopLeft | WindowHit::ResizeTopRight => {
                        ny += dy; nh -= dy;
                    }
                    WindowHit::ResizeBottom | WindowHit::ResizeBottomLeft | WindowHit::ResizeBottomRight => {
                        nh += dy;
                    }
                    _ => {}
                }

                // Minimum size constraints
                if nw < 100 {
                    if resize_hit == WindowHit::ResizeLeft || resize_hit == WindowHit::ResizeTopLeft || resize_hit == WindowHit::ResizeBottomLeft {
                        nx -= 100 - nw;
                    }
                    nw = 100;
                }
                if nh < 50 {
                    if resize_hit == WindowHit::ResizeTop || resize_hit == WindowHit::ResizeTopLeft || resize_hit == WindowHit::ResizeTopRight {
                        ny -= 50 - nh;
                    }
                    nh = 50;
                }

                if snap_dist > 0 {
                    match resize_hit {
                        WindowHit::ResizeLeft | WindowHit::ResizeTopLeft | WindowHit::ResizeBottomLeft => {
                            let snapped_nx = Self::snap_value(nx, &x_lines, snap_dist);
                            nw += nx - snapped_nx;
                            nx = snapped_nx;
                        }
                        WindowHit::ResizeRight | WindowHit::ResizeTopRight | WindowHit::ResizeBottomRight => {
                            nw = Self::snap_value(nx + nw, &x_lines, snap_dist) - nx;
                        }
                        _ => {}
                    }
                    match resize_hit {
                        WindowHit::ResizeTop | WindowHit::ResizeTopLeft | WindowHit::ResizeTopRight => {
                            let snapped_ny = Self::snap_value(ny, &y_lines, snap_dist);
                            nh += ny - snapped_ny;
                            ny = snapped_ny;
                        }
                        WindowHit::ResizeBottom | WindowHit::ResizeBottomLeft | WindowHit::ResizeBottomRight => {
                            nh = Self::snap_value(ny + nh, &y_lines, snap_dist) - ny;
                        }
                        _ => {}
                    }
                }

                if nw < 100 { nw = 100; }
                if nh < 50 { nh = 50; }

                w.rect.x = nx;
                w.rect.y = ny.max(work_area().0);
                w.rect.w = nw as u32;
                w.rect.h = nh as u32;
                w.dirty = true;
            }
        }
    }

    /// Repaint all visible windows, sorted by Z-order.
    ///
    /// Sort is skipped unless z_order_dirty is set, which only happens on
    /// open/close/focus. In typical steady-state rendering (no focus change)
    /// this saves an O(N log N) sort on every 60 Hz frame.
    pub fn repaint_all(&mut self) {
        if self.z_order_dirty {
            self.windows.sort_by_key(|w| w.z_order);
            self.z_order_dirty = false;
        }
        for win in &self.windows {
            if win.state != WindowState::Minimized {
                win.paint(&self.renderer);
            }
        }
    }

    pub fn window_mut(&mut self, id: u32) -> Option<&mut DesktopWindow> {
        self.windows.iter_mut().find(|w| w.id == id)
    }
}

pub static DESKTOP_WM: Mutex<DesktopWindowManager> = Mutex::new(DesktopWindowManager::new());

// =====================================================================
// -----| Desktop Background |-----
// =====================================================================

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum WallpaperPosition {
    Stretch,
    Center,
    Tile,
    Crop,
    Basic,
    WindowFit,
    WindowCenter,
}

pub struct DesktopBackground {
    pub desktop_dir: Option<String>,
    pub entries: Vec<crate::apps::FileEntry>,
    pub last_refresh: u64,

    // Wallpaper state
    pub wallpaper_path: Option<String>,
    pub gif_frames: Option<Vec<crate::gif_decoder::GifFrame>>,
    pub static_pixels: Option<(i32, i32, usize, Vec<u8>)>,
    pub current_frame: u32,
    pub frame_count: u32,
    pub frame_delay_ms: u16,
    pub last_tick_ms: u64,
    pub is_animated: bool,
    pub pending_wallpaper_path: Option<String>,
    /// A request that was replaced before it was ever tried. If the newer
    /// path fails to load, this one gets its turn instead of being lost.
    pub fallback_wallpaper_path: Option<String>,
    pub wallpaper_position: WallpaperPosition,
}

impl DesktopBackground {
    pub const fn new() -> Self {
        Self {
            desktop_dir: None,
            entries: Vec::new(),
            last_refresh: 0,
            wallpaper_path: None,
            gif_frames: None,
            static_pixels: None,
            current_frame: 0,
            frame_count: 0,
            frame_delay_ms: 100,
            last_tick_ms: 0,
            is_animated: false,
            pending_wallpaper_path: None,
            fallback_wallpaper_path: None,
            wallpaper_position: WallpaperPosition::Stretch,
        }
    }

    pub fn set_desktop_dir(&mut self, path: &str) {
        self.desktop_dir = Some(String::from(path));
        self.last_refresh = 0;
    }

    pub fn get_desktop_dir(&self) -> String {
        self.desktop_dir.clone().unwrap_or_else(|| String::from("/desktop"))
    }

    pub fn set_wallpaper(&mut self, path: &str) {
        let mut p = path.replace("\\", "/");
        if p.starts_with("C:") || p.starts_with("D:") || p.starts_with("c:") || p.starts_with("d:") {
            p = alloc::string::String::from(&p[2..]);
        }
        if !p.starts_with('/') {
            p = alloc::format!("/{}", p);
        }
        if let Some(old) = self.pending_wallpaper_path.replace(p) {
            self.fallback_wallpaper_path = Some(old);
        }
    }

    pub fn set_wallpaper_position(&mut self, pos: &str) {
        self.wallpaper_position = match pos.to_uppercase().as_str() {
            "CENTER" => WallpaperPosition::Center,
            "TILE" => WallpaperPosition::Tile,
            "CROP" => WallpaperPosition::Crop,
            "BASIC" => WallpaperPosition::Basic,
            "WINDOW_FIT" => WallpaperPosition::WindowFit,
            "WINDOW_CENTER" => WallpaperPosition::WindowCenter,
            _ => WallpaperPosition::Stretch,
        };
    }

    pub fn refresh(&mut self, ms: u64) {
        // Deferred wallpaper loading (prevents hang during boot phase)
        if let Some(path) = self.pending_wallpaper_path.take() {
            crate::serial_println!("[DESKTOP] Attempting to load wallpaper from: {}", path);
            match crate::vfs::VFS.read_file(&path) {
                Ok(data) => {
                    crate::serial_println!("[DESKTOP] Successfully read wallpaper file, {} bytes.", data.len());
                    let upper = path.to_uppercase();
                    if upper.ends_with(".GIF") {
                        if let Some(decoded) = crate::gif_decoder::decode_gif(&data) {
                            self.wallpaper_path = Some(path.clone());
                            self.frame_count = decoded.frames.len() as u32;
                            self.is_animated = self.frame_count > 1;
                            self.frame_delay_ms = if self.is_animated && decoded.frames[0].delay_ms > 0 {
                                decoded.frames[0].delay_ms
                            } else {
                                100
                            };
                            self.gif_frames = Some(decoded.frames);
                            self.static_pixels = None;
                            self.current_frame = 0;
                            self.last_tick_ms = ms;
                            crate::serial_println!("[DESKTOP] Loaded GIF wallpaper ({} frames).", self.frame_count);
                        } else {
                            crate::serial_println!("[DESKTOP] Failed to decode GIF wallpaper.");
                        }
                    } else if upper.ends_with(".JPG") || upper.ends_with(".JPEG") {
                        self.static_pixels = crate::ui::decode_jpeg(&data);
                        if self.static_pixels.is_some() {
                            self.wallpaper_path = Some(path.clone());
                            self.gif_frames = None;
                            self.is_animated = false;
                            crate::serial_println!("[DESKTOP] Loaded JPEG wallpaper.");
                        } else {
                            crate::serial_println!("[DESKTOP] Failed to decode JPEG wallpaper.");
                        }
                    } else if upper.ends_with(".PNG") {
                        self.static_pixels = crate::ui::decode_png(&data);
                        if self.static_pixels.is_some() {
                            self.wallpaper_path = Some(path.clone());
                            self.gif_frames = None;
                            self.is_animated = false;
                            crate::serial_println!("[DESKTOP] Loaded PNG wallpaper.");
                        } else {
                            crate::serial_println!("[DESKTOP] Failed to decode PNG wallpaper.");
                        }
                    } else {
                        crate::serial_println!("[DESKTOP] Unsupported wallpaper format: {}", upper);
                    }
                }
                Err(e) => {
                    crate::serial_println!("[DESKTOP] Failed to read wallpaper file: {:?}", e);
                }
            }
            if self.wallpaper_path.as_deref() == Some(path.as_str()) {
                self.fallback_wallpaper_path = None;
            } else {
                notify(
                    NotifCategory::Warning,
                    "Wallpaper",
                    &alloc::format!("could not load {} (8.3 names only, JPG/PNG/GIF)", path),
                );
                // Keep the current wallpaper; with none, try the request this one replaced.
                if self.wallpaper_path.is_none() {
                    self.pending_wallpaper_path = self.fallback_wallpaper_path.take();
                }
            }
        }

        // Animation tick for wallpaper (Low priority: we only advance if enough time has passed)
        if self.is_animated && self.frame_count > 0 {
            if ms.wrapping_sub(self.last_tick_ms) >= self.frame_delay_ms as u64 {
                self.current_frame = (self.current_frame + 1) % self.frame_count;
                self.last_tick_ms = ms;

                if let Some(ref frames) = self.gif_frames {
                    if let Some(f) = frames.get(self.current_frame as usize) {
                        if f.delay_ms > 0 { self.frame_delay_ms = f.delay_ms; }
                    }
                }
            }
        }

        if ms - self.last_refresh < 2000 { return; } // refresh file list every 2 seconds
        self.last_refresh = ms;
        
        let vfs = &crate::vfs::VFS;
        self.entries.clear();
        let target_dir = self.get_desktop_dir();
        
        let list = match vfs.list_dir(&target_dir) {
            Ok(l) => l,
            Err(_) => {
                if target_dir != "/desktop" {
                    vfs.list_dir("/desktop").unwrap_or_default()
                } else {
                    Vec::new()
                }
            }
        };

        for e in list {
            if e.name == "." || e.name == ".." {
                continue;
            }
            self.entries.push(crate::apps::FileEntry {
                name: e.name, size: e.size, is_dir: e.file_type == crate::vfs::FileType::Directory
            });
        }
    }

    pub fn paint(&self) {
        let r = Renderer::new(0);

        // 1. Draw wallpaper if set
        if self.wallpaper_path.is_some() {
            let (img_w, img_h) = if let Some(ref frames) = self.gif_frames {
                if !frames.is_empty() { (frames[0].width as i32, frames[0].height as i32) } else { (0, 0) }
            } else if let Some((w, h, _, _)) = self.static_pixels {
                (w, h)
            } else {
                (0, 0)
            };

            if img_w > 0 && img_h > 0 {
                let sw = crate::ui::screen_w() as i32;
                let sh = crate::ui::screen_h() as i32;
                
                let draw = |x: i32, y: i32, tw: i32, th: i32| {
                    if let Some(ref frames) = self.gif_frames {
                        let idx = self.current_frame as usize;
                        if idx < frames.len() {
                            crate::ui::draw_gif_frame_atomic_ext(&frames[idx], x, y, tw, th);
                        }
                    } else if let Some((w, h, channels, ref pixels)) = self.static_pixels {
                        crate::ui::draw_decoded_rgb_ext(pixels, w, h, channels, x, y, tw, th);
                    }
                };

                match self.wallpaper_position {
                    WallpaperPosition::Stretch => {
                        draw(0, 0, sw, sh);
                    }
                    WallpaperPosition::Center | WallpaperPosition::WindowCenter => {
                        draw((sw - img_w) / 2, (sh - img_h) / 2, img_w, img_h);
                    }
                    WallpaperPosition::Basic => {
                        draw(0, 0, img_w, img_h);
                    }
                    WallpaperPosition::Tile => {
                        let mut x = 0;
                        while x < sw {
                            let mut y = 0;
                            while y < sh {
                                draw(x, y, img_w, img_h);
                                y += img_h;
                            }
                            x += img_w;
                        }
                    }
                    WallpaperPosition::WindowFit => {
                        let mut tw = sw;
                        let mut th = (sw as i64 * img_h as i64 / img_w as i64) as i32;
                        if th > sh {
                            th = sh;
                            tw = (sh as i64 * img_w as i64 / img_h as i64) as i32;
                        }
                        draw((sw - tw) / 2, (sh - th) / 2, tw, th);
                    }
                    WallpaperPosition::Crop => {
                        let mut tw = sw;
                        let mut th = (sw as i64 * img_h as i64 / img_w as i64) as i32;
                        if th < sh {
                            th = sh;
                            tw = (sh as i64 * img_w as i64 / img_h as i64) as i32;
                        }
                        draw((sw - tw) / 2, (sh - th) / 2, tw, th);
                    }
                }
            }
        }

        let t = ACTIVE_THEME.lock();
        let p = t.palette;
        drop(t);

        let icon_size = 48;
        let spacing_x = 80;
        let spacing_y = 100;
        let (top, bottom) = work_area();
        let mut x = 20;
        let mut y = top + 20;

        for entry in &self.entries {
            // Draw icon background/shape
            let icon_col = if entry.is_dir { p.accent } else { p.surface };
            r.fill_rect(Rect::new(x + (spacing_x - icon_size) / 2, y, icon_size as u32, icon_size as u32), icon_col);
            if !entry.is_dir {
                r.stroke_rect(Rect::new(x + (spacing_x - icon_size) / 2, y, icon_size as u32, icon_size as u32), p.border);
            }
            
            // Text truncation so it doesn't overlap
            let mut label = entry.name.clone();
            if label.len() > 10 {
                label.truncate(8);
                label.push_str("..");
            }
            // Draw text
            r.draw_text(&label, x, y + icon_size as i32 + 4, p.text_primary);

            y += spacing_y;
            if y + spacing_y > bottom {
                y = top + 20;
                x += spacing_x;
            }
        }
    }

    pub fn hit_test(&self, mx: i32, my: i32) -> Option<String> {
        let spacing_x = 80;
        let spacing_y = 100;
        let (top, bottom) = work_area();
        let mut x = 20;
        let mut y = top + 20;

        for entry in &self.entries {
            let rect = Rect::new(x, y, spacing_x as u32, spacing_y as u32);
            if rect.contains(mx, my) {
                return Some(entry.name.clone());
            }

            y += spacing_y;
            if y + spacing_y > bottom {
                y = top + 20;
                x += spacing_x;
            }
        }
        None
    }
}
pub static DESKTOP_BG: Mutex<DesktopBackground> = Mutex::new(DesktopBackground::new());

// =====================================================================
// -----| Panel / Taskbar |-----
// =====================================================================

pub const PANEL_HEIGHT: usize = 40;
/// Height of the VSH shell's taskbar (`Taskbar` in SHELL.VSH), pinned to the bottom.
pub const SHELL_TASKBAR_HEIGHT: usize = 32;

/// Screen rows windows may occupy: (top, bottom). The built-in panel owns the
/// top strip; a running VSH shell replaces it with its own bar at the bottom.
pub fn work_area() -> (i32, i32) {
    if crate::shell::is_active() {
        (0, (crate::ui::screen_h() - SHELL_TASKBAR_HEIGHT) as i32)
    } else {
        (PANEL_HEIGHT as i32, crate::ui::screen_h() as i32)
    }
}

pub struct TaskbarButton {
    pub window_id: u32,
    pub label:     String,
    pub active:    bool,
}

pub struct Panel {
    pub taskbar_buttons: Vec<TaskbarButton>,
    pub clock_tick:      u64,
}

impl Panel {
    pub const fn new() -> Self { Self { taskbar_buttons: Vec::new(), clock_tick: 0 } }

    pub fn sync_windows(&mut self, wm: &DesktopWindowManager) {
        let mut needs_sync = false;
        let listed: Vec<&DesktopWindow> = wm.windows.iter().filter(|w| !w.frameless && w.taskbar).collect();
        if self.taskbar_buttons.len() != listed.len() {
            needs_sync = true;
        } else {
            for (b, w) in self.taskbar_buttons.iter().zip(listed.iter()) {
                if b.window_id != w.id || b.active != (wm.focused_id == Some(w.id)) {
                    needs_sync = true;
                    break;
                }
            }
        }
        if !needs_sync { return; }

        self.taskbar_buttons.clear();
        for win in listed {
            self.taskbar_buttons.push(TaskbarButton {
                window_id: win.id,
                label:     win.title[..win.title.len().min(14)].to_uppercase(),
                active:    wm.focused_id == Some(win.id),
            });
        }
    }

    pub fn paint(&self) {
        let r = Renderer::new(0);

        // Acquire the theme lock ONCE and copy out every colour value we need.
        // Previously this function took the spinlock 5–6 separate times per frame
        // (300–360 lock roundtrips/sec at 60 Hz). One acquisition amortises all
        // of them — the guard is dropped before any drawing begins.
        let (titlebar_c, border_c, accent_c, surface_c, text_primary_c, text_secondary_c) = {
            let t = ACTIVE_THEME.lock();
            let p = t.palette;
            (p.titlebar, p.border, p.accent, p.surface, p.text_primary, p.text_secondary)
        };
        let mode = crate::policy::POLICY.lock().get_mode();

        // Background bar
        r.fill_rect(Rect::new(0, 0, screen_w() as u32, PANEL_HEIGHT as u32), titlebar_c);
        // Bottom border
        r.fill_rect(Rect::new(0, PANEL_HEIGHT as i32 - 1, screen_w() as u32, 1), border_c);

        // VEILOS logo / launcher button
        r.fill_rect(Rect::new(0, 0, PANEL_HEIGHT as u32, PANEL_HEIGHT as u32), accent_c);
        r.draw_text("VL", 10, 12, text_primary_c);

        // Taskbar buttons
        let mut bx = PANEL_HEIGHT as i32 + 8;
        for btn in &self.taskbar_buttons {
            let bw = (btn.label.len() as i32 * 8 + 20).min(140) as u32;
            let bg = if btn.active { accent_c } else { surface_c };
            r.fill_rect(Rect::new(bx, 4, bw, PANEL_HEIGHT as u32 - 8), bg);
            r.stroke_rect(Rect::new(bx, 4, bw, PANEL_HEIGHT as u32 - 8), border_c);
            r.draw_text(&btn.label, bx + 6, 14, text_primary_c);
            bx += bw as i32 + 4;
        }

        // Clock (right side, simple tick counter)
        let ticks = self.clock_tick;
        let secs  = ticks / 1000;
        let mins  = secs / 60 % 60;
        let hrs   = secs / 3600 % 24;
        let clock = alloc::format!("{:02}:{:02}:{:02}", hrs, mins, secs % 60);
        r.draw_text(&clock, screen_w() as i32 - 80, 14, text_secondary_c);

        // Policy mode indicator (far right)
        let (mode_str, mode_col) = match mode {
            crate::policy::PolicyMode::Security    => ("SEC", Color { r: 55, g: 185, b: 95  }),
            crate::policy::PolicyMode::Performance => ("PRF", Color { r: 220, g: 55, b: 55 }),
        };
        r.fill_rect(Rect::new(screen_w() as i32 - 120, 6, 36, PANEL_HEIGHT as u32 - 12), mode_col);
        r.draw_text(mode_str, screen_w() as i32 - 114, 14, text_primary_c);
    }

    /// Hit-test a click at (x, y) against the panel.
    /// Returns the window_id of the taskbar button that was clicked, if any.
    pub fn hit_test_taskbar(&self, x: i32, y: i32) -> Option<u32> {
        if y < 0 || y >= PANEL_HEIGHT as i32 { return None; }
        // VEILOS logo button occupies [0, PANEL_HEIGHT).
        let mut bx = PANEL_HEIGHT as i32 + 8;
        for btn in &self.taskbar_buttons {
            let bw = (btn.label.len() as i32 * 8 + 20).min(140);
            if x >= bx && x < bx + bw {
                return Some(btn.window_id);
            }
            bx += bw + 4;
        }
        None
    }

    /// Called from the PIT tick — update clock and sync.
    pub fn tick(&mut self, ms: u64) {
        self.clock_tick = ms;
    }
}

pub static PANEL: Mutex<Panel> = Mutex::new(Panel::new());

// =====================================================================
// -----| Notification System |-----
// =====================================================================

const MAX_TOASTS: usize = 5;
const TOAST_W:    u32   = 320;
const TOAST_H:    u32   = 60;
const TOAST_TTL_MS: u64 = 4000; // 4 seconds

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotifCategory { Info, Warning, Error, Success }

pub struct Notification {
    pub id:       u64,
    pub category: NotifCategory,
    pub title:    String,
    pub body:     String,
    pub expires:  u64, // Absolute tick
    pub dismissed: bool,
}

pub struct NotificationSystem {
    toasts:   Vec<Notification>,
    next_id:  u64,
}

impl NotificationSystem {
    pub const fn new() -> Self { Self { toasts: Vec::new(), next_id: 1 } }

    pub fn push(&mut self, cat: NotifCategory, title: &str, body: &str, now_ms: u64) -> u64 {
        let id = self.next_id; self.next_id += 1;

        // Discard oldest if at capacity.
        if self.toasts.len() >= MAX_TOASTS {
            self.toasts.remove(0);
        }

        self.toasts.push(Notification {
            id, category: cat, title: String::from(title), body: String::from(body),
            expires: now_ms + TOAST_TTL_MS, dismissed: false,
        });
        id
    }

    pub fn dismiss(&mut self, id: u64) {
        if let Some(n) = self.toasts.iter_mut().find(|n| n.id == id) {
            n.dismissed = true;
        }
    }

    /// Remove expired and dismissed notifications.
    pub fn tick(&mut self, now_ms: u64) {
        self.toasts.retain(|n| !n.dismissed && n.expires > now_ms);
    }

    pub fn paint(&self) {
        let r = Renderer::new(0);
        let mut y = screen_h() as i32 - TOAST_H as i32 - 8;
        let x     = screen_w() as i32 - TOAST_W as i32 - 8;

        for toast in self.toasts.iter().rev() {
            let t  = ACTIVE_THEME.lock();
            let bg = match toast.category {
                NotifCategory::Info    => t.palette.surface,
                NotifCategory::Warning => Color { r: 70, g: 55, b: 10 },
                NotifCategory::Error   => Color { r: 70, g: 15, b: 15 },
                NotifCategory::Success => Color { r: 15, g: 60, b: 30 },
            };
            let accent = match toast.category {
                NotifCategory::Info    => t.palette.accent,
                NotifCategory::Warning => t.palette.warning,
                NotifCategory::Error   => t.palette.danger,
                NotifCategory::Success => t.palette.success,
            };
            let tp  = t.palette.text_primary;
            let ts  = t.palette.text_secondary;
            let bd  = t.palette.border;
            drop(t);

            // Toast background
            r.fill_rect(Rect::new(x, y, TOAST_W, TOAST_H), bg);
            r.stroke_rect(Rect::new(x, y, TOAST_W, TOAST_H), bd);
            // Left accent stripe
            r.fill_rect(Rect::new(x, y, 4, TOAST_H), accent);
            // Text
            r.draw_text(&toast.title, x + 12, y + 10, tp);
            r.draw_text(&toast.body,  x + 12, y + 28, ts);

            y -= TOAST_H as i32 + 6;
        }
    }
}

pub static NOTIF_SYS: Mutex<NotificationSystem> = Mutex::new(NotificationSystem::new());

/// Push a notification.
pub fn notify(cat: NotifCategory, title: &str, body: &str) -> u64 {
    let now = crate::interrupts::TICKS.load(Ordering::Relaxed);
    NOTIF_SYS.lock().push(cat, title, body, now)
}

// =====================================================================
// -----| Context Menu |-----
// =====================================================================

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContextAction {
    Copy, Cut, Paste, SelectAll, Delete,
    Open, OpenWith, Rename,
    NewFolder, NewFile, Properties,
    ChannelSelect(crate::clipboard::ClipboardChannel),
    PasteFromHistory(usize),
    CopyToDesktop(alloc::string::String),
    MoveToDesktop(alloc::string::String),
}

pub struct ContextMenuItem {
    pub label: String,
    pub action: ContextAction,
    pub enabled: bool,
}

pub struct ContextMenu {
    pub x: i32,
    pub y: i32,
    pub items: Vec<ContextMenuItem>,
    pub visible: bool,
    pub hovered_idx: Option<usize>,
    pub source_window: Option<u32>,
    pub channel_dropdown_idx: Option<usize>,
    pub history_dropdown_open: bool,
}

impl ContextMenu {
    pub const fn new() -> Self {
        Self {
            x: 0, y: 0,
            items: Vec::new(),
            visible: false,
            hovered_idx: None,
            source_window: None,
            channel_dropdown_idx: None,
            history_dropdown_open: false,
        }
    }

    pub fn paint(&self) {
        if !self.visible { return; }
        let r = Renderer::new(0);
        let t = ACTIVE_THEME.lock();
        let p = t.palette;
        drop(t);

        let item_h = 24i32;
        let padding = 4i32;
        let menu_w = 160i32;
        let menu_h = padding * 2 + (self.items.len() as i32 * item_h);

        // Main menu
        r.fill_rect(Rect::new(self.x, self.y, menu_w as u32, menu_h as u32), p.surface);
        r.stroke_rect(Rect::new(self.x, self.y, menu_w as u32, menu_h as u32), p.border);

        for (i, item) in self.items.iter().enumerate() {
            let item_y = self.y + padding + (i as i32 * item_h);
            let color = if item.enabled { p.text_primary } else { p.text_secondary };
            
            if Some(i) == self.hovered_idx && item.enabled {
                r.fill_rect(Rect::new(self.x + 2, item_y, (menu_w - 4) as u32, item_h as u32), p.selection);
            }

            r.draw_text(&item.label, self.x + 12, item_y + 4, color);

            // Draw dropdown arrow if it's the Copy, Cut, or Paste item
            if item.action == ContextAction::Copy || item.action == ContextAction::Cut || item.action == ContextAction::Paste {
                r.draw_text(">", self.x + menu_w - 20, item_y + 4, color);
            }
        }

        // Sub-menu for channels
        if let Some(idx) = self.channel_dropdown_idx {
            let sub_x = self.x + menu_w;
            let sub_y = self.y + padding + (idx as i32 * item_h);
            let sub_w = 140i32;
            let sub_h = padding * 2 + (4 * item_h); // 4 channels

            r.fill_rect(Rect::new(sub_x, sub_y, sub_w as u32, sub_h as u32), p.surface);
            r.stroke_rect(Rect::new(sub_x, sub_y, sub_w as u32, sub_h as u32), p.border);

            let channels = ["PUBLIC", "PRIVATE", "SENSITIVE", "APP_RESTRICTED"];
            let active = crate::clipboard::CLIPBOARD.lock().active_channel.as_str();

            for (i, ch) in channels.iter().enumerate() {
                let sy = sub_y + padding + (i as i32 * item_h);
                if *ch == active {
                    r.fill_rect(Rect::new(sub_x + 2, sy, (sub_w - 4) as u32, item_h as u32), p.selection);
                }
                r.draw_text(ch, sub_x + 12, sy + 4, p.text_primary);
            }
        }

        // Sub-menu for history
        if self.history_dropdown_open {
            let paste_idx = self.items.iter().position(|it| it.action == ContextAction::Paste).unwrap_or(0);
            let sub_x = self.x + menu_w;
            let sub_y = self.y + padding + (paste_idx as i32 * item_h);
            
            let cb = crate::clipboard::CLIPBOARD.lock();
            let history_count = cb.history.iter().filter(|e| !e.consumed).count().min(5);
            let history_count = history_count.max(1); // show at least "(empty)"
            let sub_w = 200i32;
            let sub_h = padding * 2 + (history_count as i32 * item_h);

            r.fill_rect(Rect::new(sub_x, sub_y, sub_w as u32, sub_h as u32), p.surface);
            r.stroke_rect(Rect::new(sub_x, sub_y, sub_w as u32, sub_h as u32), p.border);

            let mut drawn = 0;
            for (_i, entry) in cb.history.iter().enumerate() {
                if entry.consumed { continue; }
                let sy = sub_y + padding + (drawn as i32 * item_h);
                let preview = entry.data.preview(20);
                r.draw_text(&preview, sub_x + 12, sy + 4, p.text_primary);
                drawn += 1;
                if drawn >= 5 { break; }
            }
            if drawn == 0 {
                r.draw_text("(empty)", sub_x + 12, sub_y + padding + 4, p.text_secondary);
            }
        }
    }

    pub fn handle_click(&mut self, mx: i32, my: i32) -> Option<ContextAction> {
        if !self.visible { return None; }
        
        let item_h = 24i32;
        let padding = 4i32;
        let menu_w = 160i32;
        let menu_h = padding * 2 + (self.items.len() as i32 * item_h);

        // Check channel submenu click
        if let Some(idx) = self.channel_dropdown_idx {
            let sub_x = self.x + menu_w;
            let sub_y = self.y + padding + (idx as i32 * item_h);
            let sub_w = 140i32;
            let sub_h = padding * 2 + (4 * item_h);

            if mx >= sub_x && mx < sub_x + sub_w && my >= sub_y && my < sub_y + sub_h {
                let rel_y = my - sub_y - padding;
                let ch_idx = rel_y / item_h;
                if ch_idx >= 0 && ch_idx < 4 {
                    use crate::clipboard::ClipboardChannel::*;
                    let ch = match ch_idx { 0 => Public, 1 => Private, 2 => Sensitive, _ => AppRestricted };
                    self.visible = false;
                    crate::clipboard::CLIPBOARD.lock().active_channel = ch;
                    // Trigger the action (Copy or Cut) that was hovered
                    if let Some(item) = self.items.get(idx) {
                        return Some(item.action.clone());
                    }
                    return None;
                }
            }
        }

        // Check history submenu click
        if self.history_dropdown_open {
            let paste_idx = self.items.iter().position(|it| it.action == ContextAction::Paste).unwrap_or(0);
            let sub_x = self.x + menu_w;
            let sub_y = self.y + padding + (paste_idx as i32 * item_h);
            
            let cb = crate::clipboard::CLIPBOARD.lock();
            let history_count = cb.history.iter().filter(|e| !e.consumed).count().min(5);
            let history_count = history_count.max(1);
            let sub_w = 200i32;
            let sub_h = padding * 2 + (history_count as i32 * item_h);

            if mx >= sub_x && mx < sub_x + sub_w && my >= sub_y && my < sub_y + sub_h {
                let rel_y = my - sub_y - padding;
                let hist_idx = rel_y / item_h;
                if hist_idx >= 0 && hist_idx < history_count as i32 {
                    let mut drawn = 0;
                    for (i, entry) in cb.history.iter().enumerate() {
                        if entry.consumed { continue; }
                        if drawn == hist_idx {
                            self.visible = false;
                            return Some(ContextAction::PasteFromHistory(i));
                        }
                        drawn += 1;
                        if drawn >= 5 { break; }
                    }
                }
                return None; // clicked on "(empty)" or invalid
            }
        }

        // Check main menu click
        if mx >= self.x && mx < self.x + menu_w && my >= self.y && my < self.y + menu_h {
            let rel_y = my - self.y - padding;
            let idx = rel_y / item_h;
            if idx >= 0 && (idx as usize) < self.items.len() {
                let item = &self.items[idx as usize];
                if item.enabled {
                    if item.action == ContextAction::Paste {
                        self.visible = false;
                        return Some(ContextAction::Paste);
                    } else if item.action == ContextAction::Copy || item.action == ContextAction::Cut {
                        self.visible = false;
                        return Some(item.action.clone());
                    } else {
                        self.visible = false;
                        return Some(item.action.clone());
                    }
                }
            }
            return None; // Clicked disabled item or padding
        }

        // Clicked outside -> close menu
        self.visible = false;
        None
    }

    pub fn handle_hover(&mut self, mx: i32, my: i32) {
        if !self.visible { return; }
        let item_h = 24i32;
        let padding = 4i32;
        let menu_w = 160i32;
        let menu_h = padding * 2 + (self.items.len() as i32 * item_h);

        if mx >= self.x && mx < self.x + menu_w && my >= self.y && my < self.y + menu_h {
            let rel_y = my - self.y - padding;
            let idx = rel_y / item_h;
            if idx >= 0 && (idx as usize) < self.items.len() {
                self.hovered_idx = Some(idx as usize);
                let item = &self.items[idx as usize];
                if item.action == ContextAction::Copy || item.action == ContextAction::Cut {
                    self.channel_dropdown_idx = Some(idx as usize);
                    self.history_dropdown_open = false;
                } else if item.action == ContextAction::Paste {
                    self.history_dropdown_open = true;
                    self.channel_dropdown_idx = None;
                } else {
                    self.channel_dropdown_idx = None;
                    self.history_dropdown_open = false;
                }
            } else {
                self.hovered_idx = None;
            }
        } else {
            // Check submenu hover so they stay open
            if let Some(idx) = self.channel_dropdown_idx {
                let sub_x = self.x + menu_w;
                let sub_y = self.y + padding + (idx as i32 * item_h);
                let sub_w = 140i32;
                let sub_h = padding * 2 + (4 * item_h);
                if mx >= sub_x && mx < sub_x + sub_w && my >= sub_y && my < sub_y + sub_h {
                    return; // Hovering channel submenu
                }
            }
            if self.history_dropdown_open {
                let paste_idx = self.items.iter().position(|it| it.action == ContextAction::Paste).unwrap_or(0);
                let sub_x = self.x + menu_w;
                let sub_y = self.y + padding + (paste_idx as i32 * item_h);
                let history_count = crate::clipboard::CLIPBOARD.lock().history.iter().filter(|e| !e.consumed).count().min(5).max(1);
                let sub_w = 200i32;
                let sub_h = padding * 2 + (history_count as i32 * item_h);
                if mx >= sub_x && mx < sub_x + sub_w && my >= sub_y && my < sub_y + sub_h {
                    return; // Hovering history submenu
                }
            }
            
            self.hovered_idx = None;
            self.channel_dropdown_idx = None;
            self.history_dropdown_open = false;
        }
    }
}

pub static CONTEXT_MENU: Mutex<ContextMenu> = Mutex::new(ContextMenu::new());

// =====================================================================
// -----| Pending Role Dialog (Mesh Confirmation) |-----
// =====================================================================

/// A pending role assignment request waiting for confirmation.
pub struct PendingRoleDialog {
    pub visible: bool,
    /// The ID of the node that initiated the request.
    pub requestor_id: NodeId,
    /// The IP address the response should be sent to.
    pub target_ip: Ipv4Address,
    /// The role type being requested.
    pub proposed_type: crate::distributed::NodeType,
    /// When the request expires (tick in ms). 10 minute timeout.
    pub expires: u64,
}

impl PendingRoleDialog {
    pub const fn new() -> Self {
        Self {
            visible: false,
            requestor_id: crate::distributed::NodeId([0; 8]),
            target_ip: Ipv4Address::new(0, 0, 0, 0),
            proposed_type: crate::distributed::NodeType::Unknown,
            expires: 0,
        }
    }

    pub fn show_request(&mut self, requestor: crate::distributed::NodeId, responder_ip: Ipv4Address, proposed: crate::distributed::NodeType) {
        self.visible = true;
        self.requestor_id = requestor;
        self.target_ip = responder_ip;
        self.proposed_type = proposed;
        // 10 minute timeout (600000 ms)
        self.expires = crate::interrupts::TICKS.load(Ordering::Relaxed) + 600_000;
        crate::println!("[MESH] Role request dialog shown for {:?}", proposed);
    }

    pub fn confirm(&mut self) {
        if !self.visible { return; }
        self.visible = false;

        if self.proposed_type == crate::distributed::NodeType::Controller {
            // This is a promotion request: the requestor wants to become
            // Controller, and by confirming we're the one ceding the role.
            // Only meaningful if we actually still are the Controller —
            // otherwise (e.g. we already got demoted by someone else since
            // this dialog opened) confirming would create two Controllers.
            let mut mesh = crate::distributed::NODE_MESH.lock();
            if mesh.local_type != crate::distributed::NodeType::Controller {
                drop(mesh);
                crate::distributed::NODE_MESH.lock().send_cluster_role_response(self.target_ip, &self.requestor_id, false);
                crate::desktop::notify(NotifCategory::Warning, "MESH", "Can't concede — you're not the Controller anymore.");
                return;
            }
            mesh.local_type = crate::distributed::NodeType::Tenant;
            mesh.update_capabilities_for_type();
            drop(mesh);
            crate::distributed::NODE_MESH.lock().send_cluster_role_response(self.target_ip, &self.requestor_id, true);
            crate::distributed::NODE_MESH.lock().send_lan_announcement();
            crate::desktop::notify(NotifCategory::Info, "MESH", "Ceded Controller role — demoted to Tenant.");
            return;
        }

        // Normal case: we're being asked to adopt a role ourselves.
        crate::distributed::NODE_MESH.lock().send_cluster_role_response(
            self.target_ip,
            &self.requestor_id,
            true,
        );
        crate::distributed::NODE_MESH.lock().local_type = self.proposed_type;
        crate::distributed::NODE_MESH.lock().update_capabilities_for_type();
        crate::distributed::NODE_MESH.lock().send_lan_announcement();
        crate::desktop::notify(NotifCategory::Success, "MESH", "Role accepted");
    }

    pub fn deny(&mut self) {
        if !self.visible { return; }
        self.visible = false;
        // Send denial to the requester
        crate::distributed::NODE_MESH.lock().send_cluster_role_response(
            self.target_ip,
            &self.requestor_id,
            false,
        );
        crate::desktop::notify(NotifCategory::Info, "MESH", "Role request denied");
    }

    /// Kept for symmetry/cleanup of any stray visible dialog matching this
    /// requestor. Promotion acceptance itself is handled directly in
    /// distributed.rs's ClusterRoleResponse handler via `pending_promotion`,
    /// since the original requester never has a dialog open for their own
    /// outbound request (show_request is only invoked on the receiving side).
    pub fn confirm_by_peer(&mut self, requestor: &crate::distributed::NodeId) {
        if !self.visible || &self.requestor_id != requestor { return; }
        self.visible = false;
        crate::println!("[MESH] Role request confirmed by peer");
    }

    pub fn deny_by_peer(&mut self, requestor: &crate::distributed::NodeId) {
        if !self.visible || &self.requestor_id != requestor { return; }
        self.visible = false;
        crate::desktop::notify(NotifCategory::Warning, "MESH", "Peer denied role change");
    }

    pub fn check_timeout(&mut self) {
        let now = crate::interrupts::TICKS.load(Ordering::Relaxed);
        if self.visible && now > self.expires {
            self.visible = false;
            crate::desktop::notify(NotifCategory::Warning, "MESH", "Role request timed out");
        }
    }

    pub fn paint(&self) {
        if !self.visible { return; }
        
        use crate::ui::{draw_rect_atomic, draw_string_atomic, commit_dirty_rect};
        let sw = crate::ui::screen_w();
        let sh = crate::ui::screen_h();
        
        let t = ACTIVE_THEME.lock();
        let p = t.palette;
        drop(t);

        // Dialog centered on screen
        let dlg_w: usize = 300;
        let dlg_h: usize = 120;
        let cx = (sw.saturating_sub(dlg_w)) / 2;
        let cy = (sh.saturating_sub(dlg_h)) / 2;

        // Background overlay (dark semi-transparent)
        draw_rect_atomic(0, 0, sw, sh, Color { r: 0, g: 0, b: 0 });
        
        // Dialog background with border
        draw_rect_atomic(cx, cy, dlg_w, dlg_h, p.surface);
        // Border (accent color) - top and bottom edges
        draw_rect_atomic(cx, cy, dlg_w, 2, p.accent);
        draw_rect_atomic(cx, cy + dlg_h - 2, dlg_w, 2, p.accent);
        // Border - left and right edges
        draw_rect_atomic(cx, cy + 2, 2, dlg_h - 4, p.accent);
        draw_rect_atomic(cx + dlg_w - 2, cy + 2, 2, dlg_h - 4, p.accent);

        // Title
        let type_str = match self.proposed_type {
            crate::distributed::NodeType::Delegate => "Delegate",
            crate::distributed::NodeType::Compute => "Compute",
            crate::distributed::NodeType::Offload => "Offload",
            crate::distributed::NodeType::Storage => "Storage",
            crate::distributed::NodeType::Controller => "Controller",
            crate::distributed::NodeType::Hub => "Hub",
            crate::distributed::NodeType::Tenant => "Tenant",
            crate::distributed::NodeType::Unknown => "Unknown",
        };
        draw_string_atomic("ROLE REQUEST", cx + 12, cy + 12, p.accent);
        if self.proposed_type == crate::distributed::NodeType::Controller {
            draw_string_atomic("Wants to become Controller", cx + 16, cy + 32, p.text_primary);
            draw_string_atomic("(you would become Tenant)", cx + 16, cy + 48, p.text_secondary);
        } else {
            draw_string_atomic(&alloc::format!("Wants you to become: {}", type_str), cx + 16, cy + 32, p.text_primary);
            draw_string_atomic(&alloc::format!("From: {}", self.requestor_id.to_hex()), cx + 16, cy + 48, p.text_secondary);
        }

        // Buttons
        let btn_w: usize = 80;
        let btn_h: usize = 24;
        let accept_y = cy + dlg_h - 36;
        let deny_y = cy + dlg_h - 36;
        draw_rect_atomic(cx + dlg_w - btn_w - 30, accept_y, btn_w, btn_h, p.success);
        draw_rect_atomic(cx + dlg_w - btn_w * 2 - 40, deny_y, btn_w, btn_h, p.danger);
        
        // Button borders
        draw_rect_atomic(cx + dlg_w - btn_w - 30, accept_y, btn_w, 1, p.text_primary);
        draw_rect_atomic(cx + dlg_w - btn_w - 30, accept_y + btn_h - 1, btn_w, 1, p.text_primary);
        draw_rect_atomic(cx + dlg_w - btn_w * 2 - 40, deny_y, btn_w, 1, p.text_primary);
        draw_rect_atomic(cx + dlg_w - btn_w * 2 - 40, deny_y + btn_h - 1, btn_w, 1, p.text_primary);
        
        draw_string_atomic("ACCEPT", cx + dlg_w - btn_w - 10, cy + dlg_h - 30, p.text_primary);
        draw_string_atomic("DENY", cx + dlg_w - btn_w * 2 - 24, cy + dlg_h - 30, p.text_primary);
        
        commit_dirty_rect(0, 0, sw, sh);
    }

    pub fn handle_click(&mut self, mx: i32, my: i32) -> bool {
        if !self.visible { return false; }
        let dlg_w: i32 = 300;
        let dlg_h: i32 = 120;
        let cx = (crate::ui::screen_w() as i32 - dlg_w) / 2;
        let cy = (crate::ui::screen_h() as i32 - dlg_h) / 2;

        let btn_w: i32 = 80;
        let btn_h: i32 = 24;
        let accept_btn = Rect::new(cx + dlg_w - btn_w - 30, cy + dlg_h - 36, btn_w as u32, btn_h as u32);
        let deny_btn = Rect::new(cx + dlg_w - btn_w * 2 - 40, cy + dlg_h - 36, btn_w as u32, btn_h as u32);

        if accept_btn.contains(mx, my) {
            self.confirm();
            return true;
        }
        if deny_btn.contains(mx, my) {
            self.deny();
            return true;
        }
        false
    }
}

pub static PENDING_ROLE_DIALOG: Mutex<PendingRoleDialog> = Mutex::new(PendingRoleDialog::new());

// =====================================================================
// -----| Login Screen (Phase 18) |-----
// =====================================================================

/// Which field has keyboard focus in the login form.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum LoginFocus { Username, Password }

pub struct LoginScreen {
    pub username_buf:   String,
    pub password_buf:   String,  // never displayed — shown as '●' chars
    pub focus:          LoginFocus,
    pub error_msg:      String,
    pub failed_count:   u32,
    pub locked_until_ms: u64,    // 0 = not locked
    pub last_username:  String,  // remembered across reboots
}

impl LoginScreen {
    pub const fn new() -> Self {
        Self {
            username_buf:    String::new(),
            password_buf:    String::new(),
            focus:           LoginFocus::Username,
            error_msg:       String::new(),
            failed_count:    0,
            locked_until_ms: 0,
            last_username:   String::new(),
        }
    }

    /// Load the last-used username hint from the VFS.
    pub fn load_last_user(&mut self) {
        let vfs = &crate::vfs::VFS;
        if let Ok(bytes) = vfs.read_file(crate::users::LAST_USER_PATH) {
            if let Ok(s) = core::str::from_utf8(&bytes) {
                let s = s.trim();
                if !s.is_empty() {
                    self.last_username = String::from(s);
                    self.username_buf  = String::from(s);
                }
            }
        }
    }

    /// Handle a character from the keyboard.
    pub fn on_key(&mut self, ch: char) {
        let now_ms = crate::interrupts::TICKS.load(Ordering::Relaxed);
        if self.locked_until_ms > now_ms {
            return; // Still in lockout period.
        }
        match ch {
            '\n' | '\r' => {
                if self.focus == LoginFocus::Username {
                    self.focus = LoginFocus::Password;
                } else {
                    self.attempt_login();
                }
            }
            '\t' => {
                self.focus = match self.focus {
                    LoginFocus::Username => LoginFocus::Password,
                    LoginFocus::Password => LoginFocus::Username,
                };
            }
            '\x08' => {
                match self.focus {
                    LoginFocus::Username => { self.username_buf.pop(); }
                    LoginFocus::Password => { self.password_buf.pop(); }
                }
            }
            c if !c.is_control() => {
                match self.focus {
                    LoginFocus::Username => {
                        if self.username_buf.len() < 32 {
                            self.username_buf.push(c);
                        }
                    }
                    LoginFocus::Password => {
                        if self.password_buf.len() < 128 {
                            self.password_buf.push(c);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    /// Attempt to log in with the current username/password buffers.
    fn attempt_login(&mut self) {
        let now_ms = crate::interrupts::TICKS.load(Ordering::Relaxed);
        if self.locked_until_ms > now_ms {
            self.error_msg = alloc::format!(
                "Too many attempts. Wait {} s.",
                (self.locked_until_ms - now_ms) / 1_000
            );
            return;
        }

        // Guest session shortcut.
        if self.username_buf.eq_ignore_ascii_case("guest") {
            crate::users::SESSION.lock().login("guest");
            DESKTOP_PHASE.store(DesktopPhase::Desktop as u8, Ordering::Release);
            self.clear_buffers();
            notify(NotifCategory::Info, "Welcome", "Logged in as guest");
            return;
        }

        let result = {
            let mut db = crate::users::USER_DB.lock();
            let user = db.find_mut(&self.username_buf);
            match user {
                None => Err("Unknown username"),
                Some(u) => u.authenticate(&self.password_buf),
            }
        };

        match result {
            Ok(()) => {
                let username = self.username_buf.clone();
                crate::users::SESSION.lock().login(&username);
                // The user's saved keys (encryption.rs): unlocked by this password.
                if let Err(e) = crate::encryption::cen_unlock_user(&username, &self.password_buf) {
                    crate::serial_println!("[CEN] {}: keys not unlocked: {}", username, e);
                }
                DESKTOP_PHASE.store(DesktopPhase::Desktop as u8, Ordering::Release);
                self.clear_buffers();
                notify(NotifCategory::Success, "Welcome",
                    &alloc::format!("Logged in as {}", username));
                crate::users::USER_DB.lock().save();
            }
            Err(msg) => {
                self.failed_count += 1;
                self.password_buf.clear();
                self.error_msg = String::from(msg);
                // Exponential back-off: lock for (2^attempts - 1) seconds.
                if self.failed_count >= 3 {
                    let wait_secs: u64 = (1u64 << (self.failed_count - 2)).min(60);
                    self.locked_until_ms = now_ms + wait_secs * 1_000;
                }
                println!("[LOGIN] Failed attempt {} for '{}'", self.failed_count, self.username_buf);
            }
        }
    }

    fn clear_buffers(&mut self) {
        self.username_buf.clear();
        self.password_buf.clear();
        self.error_msg.clear();
        self.failed_count = 0;
        self.locked_until_ms = 0;
        self.focus = LoginFocus::Username;
    }

    /// Paint the login screen over the full framebuffer.
    pub fn paint(&self) {
        use crate::ui::{draw_rect_atomic, draw_string_atomic, commit_dirty_rect};

        let sw = screen_w();
        let sh = screen_h();

        // Background — deep navy.
        draw_rect_atomic(0, 0, sw, sh, Color { r: 8, g: 12, b: 28 });

        // Subtle grid pattern — thin lines every 48px.
        let grid = Color { r: 16, g: 22, b: 44 };
        let mut gx = 0;
        while gx < sw { draw_rect_atomic(gx, 0, 1, sh, grid); gx += 48; }
        let mut gy = 0;
        while gy < sh { draw_rect_atomic(0, gy, sw, 1, grid); gy += 48; }

        // Card.
        let card_w: usize = 380;
        let card_h: usize = 300;
        let card_x = (sw - card_w) / 2;
        let card_y = (sh - card_h) / 2;
        // Shadow (slightly offset block).
        draw_rect_atomic(card_x + 6, card_y + 6, card_w, card_h, Color { r: 0, g: 0, b: 0 });
        // Card background.
        draw_rect_atomic(card_x, card_y, card_w, card_h, Color { r: 18, g: 24, b: 48 });
        // Card border.
        draw_rect_atomic(card_x, card_y, card_w, 1, Color { r: 60, g: 100, b: 200 });
        draw_rect_atomic(card_x, card_y + card_h - 1, card_w, 1, Color { r: 40, g: 70, b: 140 });
        draw_rect_atomic(card_x, card_y, 1, card_h, Color { r: 40, g: 70, b: 140 });
        draw_rect_atomic(card_x + card_w - 1, card_y, 1, card_h, Color { r: 40, g: 70, b: 140 });

        // Logo + title.
        draw_string_atomic("VEILOS", card_x + 140, card_y + 22, Color { r: 80, g: 140, b: 255 });
        draw_string_atomic("v0.1.0", card_x + 245, card_y + 22, Color { r: 60, g: 80, b: 130 });
        draw_string_atomic("Sign in to continue", card_x + 100, card_y + 44, Color { r: 120, g: 140, b: 180 });

        // Divider.
        draw_rect_atomic(card_x + 24, card_y + 62, card_w - 48, 1, Color { r: 40, g: 55, b: 100 });

        // Username field.
        let field_x = card_x + 24;
        let field_w = card_w - 48;
        let u_y = card_y + 76;
        draw_string_atomic("Username", field_x, u_y, Color { r: 100, g: 130, b: 190 });
        let u_field_y = u_y + 16;
        let u_focused = self.focus == LoginFocus::Username;
        let field_bg = Color { r: 12, g: 18, b: 38 };
        let border_active = Color { r: 80, g: 140, b: 255 };
        let border_idle   = Color { r: 40, g: 55, b: 100 };
        draw_rect_atomic(field_x, u_field_y, field_w, 28, field_bg);
        draw_rect_atomic(field_x, u_field_y + 27, field_w, 1,
            if u_focused { border_active } else { border_idle });
        draw_string_atomic(&self.username_buf, field_x + 6, u_field_y + 8,
            Color { r: 220, g: 230, b: 255 });
        if u_focused {
            let cx = field_x + 6 + self.username_buf.len() * 8;
            draw_rect_atomic(cx, u_field_y + 6, 2, 16, border_active);
        }

        // Password field.
        let p_y = u_field_y + 44;
        draw_string_atomic("Password", field_x, p_y, Color { r: 100, g: 130, b: 190 });
        let p_field_y = p_y + 16;
        let p_focused = self.focus == LoginFocus::Password;
        draw_rect_atomic(field_x, p_field_y, field_w, 28, field_bg);
        draw_rect_atomic(field_x, p_field_y + 27, field_w, 1,
            if p_focused { border_active } else { border_idle });
        // Show bullets for each password character.
        let bullets: String = self.password_buf.chars().map(|_| '*').collect();
        draw_string_atomic(&bullets, field_x + 6, p_field_y + 8,
            Color { r: 220, g: 230, b: 255 });
        if p_focused {
            let cx = field_x + 6 + self.password_buf.len() * 8;
            draw_rect_atomic(cx, p_field_y + 6, 2, 16, border_active);
        }

        // Error message.
        if !self.error_msg.is_empty() {
            draw_string_atomic(&self.error_msg, field_x, p_field_y + 36,
                Color { r: 255, g: 80, b: 80 });
        }

        // Sign-in button.
        let btn_y = p_field_y + 56;
        let btn_x = field_x;
        let btn_w = field_w;
        draw_rect_atomic(btn_x, btn_y, btn_w, 32, Color { r: 40, g: 80, b: 200 });
        draw_string_atomic("SIGN IN  (Enter)", btn_x + btn_w / 2 - 64, btn_y + 10,
            Color { r: 255, g: 255, b: 255 });

        // Guest session hint.
        draw_string_atomic("Guest session: type 'guest' as username",
            card_x + 24, card_y + card_h - 20,
            Color { r: 60, g: 80, b: 130 });

        commit_dirty_rect(0, 0, sw, sh);
    }
}

pub static LOGIN_SCREEN: Mutex<LoginScreen> = Mutex::new(LoginScreen::new());

// =====================================================================
// -----| Lock Screen (Phase 18) |-----
// =====================================================================

pub struct LockScreen {
    pub password_buf: String,
    pub error_msg:    String,
    pub show_tick:    u64,  // tick when the lock screen appeared
}

impl LockScreen {
    pub const fn new() -> Self {
        Self {
            password_buf: String::new(),
            error_msg:    String::new(),
            show_tick:    0,
        }
    }

    pub fn on_show(&mut self) {
        self.password_buf.clear();
        self.error_msg.clear();
        self.show_tick = crate::interrupts::TICKS.load(Ordering::Relaxed);
    }

    pub fn on_key(&mut self, ch: char) {
        match ch {
            '\n' | '\r' => self.attempt_unlock(),
            '\x08' => { self.password_buf.pop(); }
            c if !c.is_control() => {
                if self.password_buf.len() < 128 {
                    self.password_buf.push(c);
                }
            }
            _ => {}
        }
    }

    fn attempt_unlock(&mut self) {
        let pwd = self.password_buf.clone();

        // LOCK ORDER: verify password with USER_DB locked first (no SESSION
        // lock held), then re-lock SESSION to flip the flag.  This matches
        // the USER_DB → SESSION ordering used everywhere else and avoids the
        // previous inversion (SESSION locked → USER_DB acquired inside).
        let result = {
            let session = crate::users::SESSION.lock();
            session.verify_unlock_password(&pwd)
        };  // SESSION lock released here before we re-acquire below.

        match result {
            Ok(()) => {
                self.password_buf.clear();
                self.error_msg.clear();
                crate::users::SESSION.lock().complete_unlock();
                DESKTOP_PHASE.store(DesktopPhase::Desktop as u8, Ordering::Release);
                notify(NotifCategory::Success, "Unlocked", "Welcome back");
            }
            Err(msg) => {
                self.password_buf.clear();
                self.error_msg = String::from(msg);
            }
        }
    }

    /// Paint the lock screen over the full framebuffer.
    pub fn paint(&self, ms: u64) {
        use crate::ui::{draw_rect_atomic, draw_string_atomic, commit_dirty_rect};

        let sw = screen_w();
        let sh = screen_h();

        // Dark overlay.
        draw_rect_atomic(0, 0, sw, sh, Color { r: 4, g: 6, b: 16 });

        // Clock — compute seconds since epoch as HH:MM:SS from TICKS.
        let total_secs = ms / 1_000;
        let hh = (total_secs / 3_600) % 24;
        let mm = (total_secs / 60) % 60;
        let ss = total_secs % 60;
        let time_str = alloc::format!("{:02}:{:02}:{:02}", hh, mm, ss);
        // Draw big clock — 3x scaled by drawing each glyph as 3×3 blocks.
        let clock_x = sw / 2 - time_str.len() * 12;
        let clock_y = sh / 2 - 100;
        draw_string_atomic(&time_str, clock_x, clock_y, Color { r: 200, g: 220, b: 255 });

        // Locked indicator.
        draw_string_atomic("SCREEN LOCKED", sw / 2 - 52, clock_y + 24,
            Color { r: 80, g: 100, b: 160 });

        // Get current username.
        let username = crate::users::SESSION.lock().username.clone();
        draw_string_atomic(
            &alloc::format!("Signed in as: {}", username),
            sw / 2 - 80, clock_y + 48,
            Color { r: 100, g: 120, b: 180 },
        );

        // Password box.
        let box_w: usize = 320;
        let box_x = (sw - box_w) / 2;
        let box_y = sh / 2 + 20;
        draw_rect_atomic(box_x, box_y, box_w, 36, Color { r: 12, g: 16, b: 36 });
        draw_rect_atomic(box_x, box_y + 35, box_w, 1, Color { r: 80, g: 120, b: 220 });
        let bullets: String = self.password_buf.chars().map(|_| '*').collect();
        let hint = if self.password_buf.is_empty() { "Enter password to unlock" } else { &bullets };
        let hint_col = if self.password_buf.is_empty() {
            Color { r: 60, g: 75, b: 120 }
        } else {
            Color { r: 200, g: 210, b: 255 }
        };
        draw_string_atomic(hint, box_x + 12, box_y + 10, hint_col);
        // Cursor blink.
        if (ms / 500) % 2 == 0 && !self.password_buf.is_empty() {
            let cx = box_x + 12 + self.password_buf.len() * 8;
            draw_rect_atomic(cx, box_y + 8, 2, 18, Color { r: 80, g: 140, b: 255 });
        }

        if !self.error_msg.is_empty() {
            draw_string_atomic(&self.error_msg, box_x, box_y + 44,
                Color { r: 255, g: 80, b: 80 });
        }

        draw_string_atomic("Press Enter to unlock  |  Super+L to lock",
            sw / 2 - 168, sh - 30, Color { r: 40, g: 55, b: 100 });

        commit_dirty_rect(0, 0, sw, sh);
    }
}

pub static LOCK_SCREEN: Mutex<LockScreen> = Mutex::new(LockScreen::new());

// =====================================================================
// -----| Desktop Phase state machine |-----
// =====================================================================
//
//  Boot → LoginScreen → Desktop ↔ LockScreen
//
//  Stored as an atomic u8 so the gui_refresh_task can read it without
//  holding any Mutex.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum DesktopPhase {
    Login   = 0,
    Desktop = 1,
    Locked  = 2,
}

impl DesktopPhase {
    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Desktop,
            2 => Self::Locked,
            _ => Self::Login,
        }
    }
}

pub static DESKTOP_PHASE: AtomicU8 = AtomicU8::new(0); // 0 = Login

/// Lock the desktop from anywhere (e.g. Super+L shortcut).
pub fn lock_desktop() {
    let logged_in = crate::users::SESSION.lock().logged_in;
    if logged_in {
        crate::users::SESSION.lock().lock();
        LOCK_SCREEN.lock().on_show();
        DESKTOP_PHASE.store(DesktopPhase::Locked as u8, Ordering::Release);
    }
}

/// Dispatch a pending content-area click to the owning native app.
///
/// MUST be called after the DESKTOP_WM lock has been released — every app's
/// handle_click() re-locks DESKTOP_WM to read its window rect, so calling
/// this while the WM is held would deadlock.
///
/// The typical call site in gui_refresh_task is:
///
///   let pending = { let mut wm = DESKTOP_WM.lock(); wm.on_mouse_down(x, y) };
///   dispatch_pending_click(pending);  // WM lock already released
/// Set the global draw clip to the content area of the window with `window_id`.
///
/// The content area is the region inside the title bar and frame border — the
/// area that belongs exclusively to the app, not the WM chrome.  Any draw call
/// made after this (and before clip_reset) will be silently clipped to this
/// rect, preventing apps from drawing outside their own window or over each
/// other's content.
///
/// Call `crate::ui::clip_reset()` after the app finishes painting.
fn clip_to_window(window_id: u32) {
    let wm = DESKTOP_WM.lock();
    if let Some(win) = wm.windows.iter().find(|w| w.id == window_id) {
        if win.state == WindowState::Minimized { return; }
        let t = crate::gui_toolkit::ACTIVE_THEME.lock();
        let title_h = t.titlebar_height as i32;
        drop(t);
        // Content rect: inset 1px for frame, below title bar.
        let cx = win.rect.x + 1;
        let cy = win.rect.y + title_h;
        let cw = win.rect.w.saturating_sub(2);
        let ch = win.rect.h.saturating_sub(title_h as u32);
        drop(wm);
        crate::ui::clip_set(cx, cy, cw, ch);
    }
}

pub fn dispatch_pending_click(click: PendingClick) {
    let (window_id, x, y, is_right) = match click {
        PendingClick::None => return,
        PendingClick::OpenAppLauncher => {
            crate::apps::open_app_launcher(0, crate::desktop::PANEL_HEIGHT as i32, 600, 480, 0);
            return;
        }
        PendingClick::DesktopIcon(name) => {
            let dir = crate::desktop::DESKTOP_BG.lock().get_desktop_dir();
            let path = if dir.ends_with("/") {
                alloc::format!("{}{}", dir, name)
            } else {
                alloc::format!("{}/{}", dir, name)
            };
            crate::apps::open_file_absolute(&path, &name);
            return;
        }
        PendingClick::DesktopIconRightClick(_name) => {
            // Future: File-specific context menu
            return;
        }
        PendingClick::DesktopRightClick => {
            // Future: Desktop context menu
            return;
        }
        PendingClick::AppContent { window_id, x, y } => (window_id, x, y, false),
        PendingClick::RightClick { window_id, x, y } => (window_id, x, y, true),
    };

    let button = if is_right { crate::nt_user::WM_RBUTTONDOWN } else { crate::nt_user::WM_LBUTTONDOWN };
    if crate::nt_user::deliver(window_id, button, 0, (y as u16 as u64) << 16 | x as u16 as u64) {
        return;
    }

    if is_right {
        // Build the basic context menu with Copy, Cut, Paste and channel selection for now.
        let mut cm = CONTEXT_MENU.lock();
        cm.items.clear();
        cm.items.push(ContextMenuItem { label: String::from("Copy"), action: ContextAction::Copy, enabled: true });
        cm.items.push(ContextMenuItem { label: String::from("Cut"), action: ContextAction::Cut, enabled: true });
        cm.items.push(ContextMenuItem { label: String::from("Paste"), action: ContextAction::Paste, enabled: true });
        
        let state = crate::task::mouse::MOUSE_STATE.lock();
        cm.x = state.x as i32;
        cm.y = state.y as i32;
        cm.visible = true;
        cm.source_window = Some(window_id);
        cm.channel_dropdown_idx = None;
        cm.history_dropdown_open = false;
        cm.hovered_idx = None;
        return;
    }

    if crate::apps::scroll_bar_click(window_id, x, y) {
        return;
    }

    // Each app type is pooled — look up the owning slot and dispatch there.
    // No singleton window_id comparisons; every instance is independent.
    if let Some(idx) = crate::apps::fm_slot_for_window(window_id) {
        crate::apps::FILE_MANAGER_POOL[idx].lock().handle_click(x, y);
    } else if let Some(idx) = crate::apps::text_editor_slot_for_window(window_id) {
        crate::apps::TEXT_EDITOR_POOL[idx].lock().handle_click(x, y);
    } else if let Some(idx) = crate::apps::launcher_slot_for_window(window_id) {
        crate::apps::APP_LAUNCHER_POOL[idx].lock().handle_click(x, y);
    } else if let Some(idx) = crate::apps::mesh_manager_slot_for_window(window_id) {
        crate::apps::MESH_MANAGER_POOL[idx].lock().handle_click(x, y);
    } else {
        // Check if it's a Win32 window.
        crate::win32_user::dispatch_veilos_input(
            window_id, 
            crate::win32_user::WM_LBUTTONDOWN, 
            0, 
            (y as u64) << 16 | (x as u64)
        );
    }
    // Terminals are keyboard-driven; a content click just transfers focus,
    // which on_mouse_down already handled before returning the PendingClick.
    // However, we should also send a message to ensure the app knows it was clicked.
    if let Some(_idx) = crate::apps::terminal_slot_for_window(window_id) {
        crate::win32_user::dispatch_veilos_input(window_id, crate::win32_user::WM_LBUTTONDOWN, 0, (y as u64) << 16 | (x as u64));
    }
}

/// Polls mouse input and updates the window manager state.
/// This is called from the timer ISR so it works even when the async executor is blocked.
pub fn poll_input() {
    // Drain any deferred close_silent requests from destroy_window().
    // These are queued when DESKTOP_WM was busy at the time DestroyWindow fired.
    {
        let mut pending = PENDING_CLOSE.lock();
        if !pending.is_empty() {
            if let Some(mut wm) = DESKTOP_WM.try_lock() {
                for vid in pending.drain(..) {
                    wm.close_silent(vid);
                }
            }
        }
    }

    let (mx, my, mb, rmb, wheel) = {
        let mut state = crate::task::mouse::MOUSE_STATE.lock();
        (state.x as usize, state.y as usize, state.left_button, state.right_button, core::mem::replace(&mut state.wheel, 0))
    };
    // Windows programs' windows follow the pointer themselves.
    crate::nt_user::mouse_tick(mx as i32, my as i32, mb, rmb, wheel);
    crate::apps::scroll_pointer(mx as i32, my as i32, mb, wheel);

    let last_mb = LAST_MB.load(Ordering::Relaxed);
    let last_rmb = LAST_RMB.load(Ordering::Relaxed);
    let last_mx = LAST_MX.load(Ordering::Relaxed);
    let last_my = LAST_MY.load(Ordering::Relaxed);

    // Track state for the next call.
    LAST_MB.store(mb, Ordering::Relaxed);
    LAST_RMB.store(rmb, Ordering::Relaxed);
    LAST_MX.store(mx as u32, Ordering::Relaxed);
    LAST_MY.store(my as u32, Ordering::Relaxed);

    // Context menu intercept
    {
        let mut cm = CONTEXT_MENU.lock();
        if cm.visible {
            cm.handle_hover(mx as i32, my as i32);
            if mb && !last_mb {
                if let Some(action) = cm.handle_click(mx as i32, my as i32) {
                    drop(cm);
                    match action {
                        ContextAction::ChannelSelect(ch) => {
                            crate::clipboard::CLIPBOARD.lock().active_channel = ch;
                        }
                        ContextAction::Copy => {
                            crate::clipboard::clipboard_copy();
                        }
                        ContextAction::Cut => {
                            crate::clipboard::clipboard_cut();
                        }
                        ContextAction::Paste => {
                            crate::clipboard::clipboard_paste();
                        }
                        ContextAction::PasteFromHistory(idx) => {
                            crate::clipboard::clipboard_paste_at(idx);
                        }
                        ContextAction::CopyToDesktop(src) => {
                            let file_name = src.split('/').last().unwrap_or("file");
                            let dest = alloc::format!("/Desktop/{}", file_name);
                            if let Ok(data) = crate::vfs::VFS.read_file(&src) {
                                let _ = crate::vfs::VFS.write_file(&dest, &data);
                            }
                        }
                        ContextAction::MoveToDesktop(src) => {
                            let file_name = src.split('/').last().unwrap_or("file");
                            let dest = alloc::format!("/Desktop/{}", file_name);
                            let vfs = &crate::vfs::VFS;
                            if let Ok(data) = vfs.read_file(&src) {
                                if vfs.write_file(&dest, &data).is_ok() {
                                    let _ = vfs.delete_file(&src);
                                }
                            }
                        }
                        _ => {}
                    }
                }
                return; // Swallow left click
            }
            if rmb && !last_rmb {
                cm.visible = false;
                return; // Swallow right click
            }
        }
    }

    // VSH shell elements sit above windows, so they see the pointer first.
    // ISR-safe: try_lock only, no allocation, handlers run later in pump().
    if crate::shell::pointer_hook(mx as i32, my as i32, mb, last_mb) {
        return;
    }

    // 1. Clicks and window buttons.
    let pending_click = if rmb && !last_rmb {
        if let Some(mut wm) = DESKTOP_WM.try_lock() {
            let hit = wm.on_mouse_down(mx as i32, my as i32);
            match hit {
                PendingClick::AppContent { window_id, x, y } => PendingClick::RightClick { window_id, x, y },
                PendingClick::None => {
                    if let Some(bg) = DESKTOP_BG.try_lock() {
                        if let Some(name) = bg.hit_test(mx as i32, my as i32) {
                            PendingClick::DesktopIconRightClick(name)
                        } else {
                            PendingClick::DesktopRightClick
                        }
                    } else {
                        PendingClick::DesktopRightClick
                    }
                }
                _ => PendingClick::None
            }
        } else {
            PendingClick::None
        }
    } else if mb && !last_mb {
            // Check pending role dialog first (it captures all clicks when visible)
            if PENDING_ROLE_DIALOG.lock().visible {
                if let Some(mut dlg) = PENDING_ROLE_DIALOG.try_lock() {
                    if dlg.handle_click(mx as i32, my as i32) {
                        return; // Dialog handled the click
                    }
                }
            }

            // Check if the click is in the panel strip first (panel is always on top).
            // A running VSH shell replaces the panel, so the strip is ordinary desktop.
            let in_panel = !crate::shell::is_active() && (my as i32) < PANEL_HEIGHT as i32;
            if in_panel {
            if (mx as i32) < PANEL_HEIGHT as i32 {
                let is_open = {
                    let live_ids: alloc::vec::Vec<u32> = {
                        DESKTOP_WM.try_lock()
                            .map(|wm| wm.windows.iter().map(|w| w.id).collect())
                            .unwrap_or_default()
                    };
                    let mut found = None;
                    for slot in crate::apps::APP_LAUNCHER_POOL.iter() {
                        if let Some(launcher) = slot.try_lock() {
                            if launcher.window_id != 0 && live_ids.contains(&launcher.window_id) {
                                found = Some(launcher.window_id);
                                break;
                            }
                        }
                    }
                    found
                };
                if let Some(id) = is_open {
                    if let Some(mut wm) = DESKTOP_WM.try_lock() {
                        if let Some(w) = wm.windows.iter_mut().find(|w| w.id == id) {
                            if w.state == WindowState::Minimized {
                                w.state = WindowState::Normal;
                            }
                        }
                        wm.focus(id);
                    }
                    PendingClick::None
                } else {
                    PendingClick::OpenAppLauncher
                }
            } else {
                // Hit-test the taskbar buttons.  We need both PANEL (for layout) and
                // DESKTOP_WM (to restore/focus the window) — acquire in that order.
                let clicked_id = PANEL.try_lock().and_then(|p| p.hit_test_taskbar(mx as i32, my as i32));
                if let Some(win_id) = clicked_id {
                    if let Some(mut wm) = DESKTOP_WM.try_lock() {
                        if let Some(w) = wm.windows.iter_mut().find(|w| w.id == win_id) {
                            // Restore if minimized, otherwise just focus.
                            if w.state == WindowState::Minimized {
                                w.state = WindowState::Normal;
                            }
                        }
                        wm.focus(win_id);
                    }
                }
                PendingClick::None
            }
        } else if let Some(mut wm) = DESKTOP_WM.try_lock() {
            let hit = wm.on_mouse_down(mx as i32, my as i32);
            if hit == PendingClick::None {
                // Clicked on empty desktop background
                if let Some(bg) = DESKTOP_BG.try_lock() {
                    if let Some(name) = bg.hit_test(mx as i32, my as i32) {
                        PendingClick::DesktopIcon(name)
                    } else {
                        PendingClick::None
                    }
                } else {
                    PendingClick::None
                }
            } else {
                hit
            }
        } else {
            PendingClick::None
        }
    } else {
        if !mb && last_mb {
            let mut dd = crate::apps::GLOBAL_DRAG_DROP.lock();
            if let Some(state) = dd.take() {
                if state.is_dragging {
                    handle_drop(state.source_path, mx as i32, my as i32);
                }
            }
            if let Some(mut wm) = DESKTOP_WM.try_lock() {
                wm.on_mouse_up();
            }
        }
        PendingClick::None
    };

    // 2. Drag movement
    if mx as u32 != last_mx || my as u32 != last_my {
        if mb {
            let mut dd = crate::apps::GLOBAL_DRAG_DROP.lock();
            if let Some(state) = dd.as_mut() {
                if !state.is_dragging {
                    let dx = (mx as i32 - state.drag_start_x).abs();
                    let dy = (my as i32 - state.drag_start_y).abs();
                    if dx > 4 || dy > 4 {
                        state.is_dragging = true;
                    }
                }
            }
        }
        if let Some(mut wm) = DESKTOP_WM.try_lock() {
            wm.on_mouse_move(mx as i32, my as i32);
        }
    }

    // 3. Dispatch content clicks (must be done OUTSIDE DESKTOP_WM lock).
    //
    // IMPORTANT: DesktopIcon and OpenAppLauncher clicks trigger heavy heap
    // allocations (PNG decode, PE loading, VFS reads).  If poll_input() is
    // being called from the timer ISR (while a user process is running),
    // those allocations can corrupt the linked-list allocator because the
    // executor may have been mid-allocation when the ISR fired.  We defer
    // them to DEFERRED_CLICKS for the executor to drain safely.
    //
    // AppContent clicks are lightweight (just forwarding a click event to
    // an already-open app's handler) and are safe to dispatch inline.
    if pending_click != PendingClick::None {
        match pending_click {
            PendingClick::DesktopIcon(_) | PendingClick::OpenAppLauncher => {
                if let Some(mut q) = DEFERRED_CLICKS.try_lock() {
                    q.push(pending_click);
                }
                // If try_lock fails the click is silently dropped — the user
                // can click again. This is preferable to a page fault.
            }
            _ => {
                dispatch_pending_click(pending_click);
            }
        }
    }
}

pub fn handle_drop(source_path: alloc::string::String, x: i32, y: i32) {
    let wm = DESKTOP_WM.lock();
    let target_win_id = wm.windows.iter().rev().find(|w| {
        x >= w.rect.x && x < w.rect.x + w.rect.w as i32 &&
        y >= w.rect.y && y < w.rect.y + w.rect.h as i32
    }).map(|w| w.id);
    drop(wm);

    if let Some(id) = target_win_id {
        if let Some(idx) = crate::apps::fm_slot_for_window(id) {
            let fm = crate::apps::FILE_MANAGER_POOL[idx].lock();
            let target_cwd = fm.cwd.clone();
            drop(fm);
            
            let source_filename = match source_path.rfind('/') {
                Some(pos) => &source_path[pos + 1..],
                None => &source_path,
            };
            
            let target_path = if target_cwd == "/" {
                alloc::format!("/{}", source_filename)
            } else {
                alloc::format!("{}/{}", target_cwd, source_filename)
            };
            
            if target_path != source_path {
                let vfs = &crate::vfs::VFS;
                if let Ok(data) = vfs.read_file(&source_path) {
                    if vfs.write_file(&target_path, &data).is_ok() {
                        notify(NotifCategory::Success, "COPIED", &target_path);
                        crate::apps::FILE_MANAGER_POOL[idx].lock().refresh();
                    }
                }
            }
        }
    } else {
        // Dropped on the desktop background
        let mut cm = CONTEXT_MENU.lock();
        cm.items.clear();
        cm.items.push(ContextMenuItem { 
            label: alloc::string::String::from("Copy to Desktop"), 
            action: ContextAction::CopyToDesktop(source_path.clone()), 
            enabled: true 
        });
        cm.items.push(ContextMenuItem { 
            label: alloc::string::String::from("Move to Desktop"), 
            action: ContextAction::MoveToDesktop(source_path), 
            enabled: true 
        });
        cm.x = x;
        cm.y = y;
        cm.visible = true;
        cm.source_window = None;
        cm.channel_dropdown_idx = None;
        cm.history_dropdown_open = false;
        cm.hovered_idx = None;
    }
}
/// Deliver a key character to whichever phase owns the keyboard right now.
pub fn dispatch_raw_key(key: pc_keyboard::KeyCode) {
    match DesktopPhase::from_u8(DESKTOP_PHASE.load(Ordering::Acquire)) {
        DesktopPhase::Desktop => {
            let mut wm = DESKTOP_WM.lock();
            // A Windows program's window had the key itself (nt_user::key_event).
            if wm.focused_id.is_some_and(crate::nt_user::owns) {
                return;
            }
            let _ = wm.handle_arrow_navigation(key);
        }
        _ => {}
    }
}

pub fn dispatch_key(ch: char) {
    // Record activity for idle timeout regardless.
    crate::users::SESSION.lock().touch();

    match DesktopPhase::from_u8(DESKTOP_PHASE.load(Ordering::Acquire)) {
        DesktopPhase::Login  => LOGIN_SCREEN.lock().on_key(ch),
        DesktopPhase::Locked => LOCK_SCREEN.lock().on_key(ch),
        DesktopPhase::Desktop => {
            let focused_id = DESKTOP_WM.lock().focused_id;

            if let Some(id) = focused_id {
                // Pool lookup order: terminal → text editor → launcher → settings → Win32.
                if let Some(idx) = crate::apps::terminal_slot_for_window(id) {
                    crate::apps::TERMINAL_POOL[idx].lock().on_key(ch);
                    return;
                }
                if let Some(idx) = crate::apps::text_editor_slot_for_window(id) {
                    crate::apps::TEXT_EDITOR_POOL[idx].lock().on_key(ch);
                    return;
                }
                if let Some(idx) = crate::apps::launcher_slot_for_window(id) {
                    crate::apps::APP_LAUNCHER_POOL[idx].lock().handle_key(ch);
                    return;
                }
                if crate::nt_user::deliver(id, crate::nt_user::WM_CHAR, ch as u64, 0) {
                    return;
                }
                // Check whether this is a live Win32 window before dispatching.
                // If the EXE has exited and its veilos window ID is stale (the
                // window was removed from WIN32_USER but focused_id hasn't been
                // updated yet), find_hwnd_by_veilos_id() returns None and we fall
                // through to the terminal instead of silently dropping the key.
                let is_live_win32 = crate::loader::lock_guard::lock_tracked(&crate::win32_user::WIN32_USER)
                    .find_hwnd_by_veilos_id(id)
                    .is_some();
                if is_live_win32 {
                    // Send to Win32 message queue and return — don't double-route.
                    crate::win32_user::dispatch_veilos_input(id, crate::win32_user::WM_CHAR, ch as u64, 0);
                    return;
                }
                // focused_id points at a window that no longer exists in any known
                // pool.  Clear stale focus so the next interaction refocuses naturally,
                // then route to the boot terminal.
                DESKTOP_WM.lock().focused_id = None;
            }
            // Fallback: send to pool slot 0 (the boot terminal).
            crate::apps::TERMINAL_POOL[0].lock().on_key(ch);
        }
    }
}



/// Initialise the desktop environment.
pub fn init() {

    // Load the last-used username hint for the login screen.
    LOGIN_SCREEN.lock().load_last_user();

    // Boot always starts at the login screen.
    DESKTOP_PHASE.store(DesktopPhase::Login as u8, Ordering::Release);

    println!("[DE] Desktop environment initialised.");
    println!("[DE]   Boot phase: LoginScreen");
    println!("[DE]   Window manager: drag, resize, minimize, maximize, Z-order");
    println!("[DE]   Panel: clock, taskbar, policy toggle, launcher");
    println!("[DE]   Notifications: {} toast slots, {}ms TTL", MAX_TOASTS, TOAST_TTL_MS);
    println!("[DE]   Settings UI: Appearance / Display / Security / About tabs");
}

/// Called from the vsync tick — repaint the full desktop scene.
/// Cheap per-second clock refresh.
///
/// Only the panel strip (top `PANEL_HEIGHT` rows) is repainted and flushed.
/// This avoids the full 1024×768 back-buffer clear + window repaint cycle
/// that `paint_desktop` triggers, which was the source of the once-per-second
/// visible stutter.
///
/// Must only be called when the desktop phase is `Desktop` and no other
/// `ui_dirty` event is pending (gui_refresh_task handles the guard).
pub fn repaint_panel_only(ms: u64) {
    use crate::ui::{screen_w, commit_dirty_rect};
    // Only meaningful on the desktop.
    let phase = DesktopPhase::from_u8(DESKTOP_PHASE.load(Ordering::Acquire));
    if phase != DesktopPhase::Desktop { return; }
    // The VSH shell replaces the panel and refreshes its own clock in pump().
    if crate::shell::is_active() { return; }

    // Re-tick the panel (updates the clock field) then repaint just its strip.
    let mut panel = PANEL.lock();
    panel.tick(ms);
    let wm = DESKTOP_WM.lock();
    panel.sync_windows(&wm);
    drop(wm);
    panel.paint(); // writes into the back-buffer, clipped to the panel strip

    // Flush only the panel rows to hardware — ~40 rows × 1024 px.
    commit_dirty_rect(0, 0, screen_w(), PANEL_HEIGHT);
    // Re-draw cursor if it overlaps the panel area.
    {
        let wm = crate::ui::WINDOW_MANAGER.lock();
        if wm.cursor_y < PANEL_HEIGHT + 16 {
            wm.draw_cursor_hw();
        }
    }
}

pub fn needs_wallpaper_refresh(ms: u64) -> bool {
    if let Some(bg) = DESKTOP_BG.try_lock() {
        if bg.pending_wallpaper_path.is_some() {
            return true;
        }
        if bg.is_animated && bg.frame_count > 0 {
            if ms.wrapping_sub(bg.last_tick_ms) >= bg.frame_delay_ms as u64 {
                return true;
            }
        }
    }
    false
}

pub fn paint_desktop(ms: u64) {
    let phase = DesktopPhase::from_u8(DESKTOP_PHASE.load(Ordering::Acquire));

    // Check idle timeout — may transition Desktop → Locked.
    if phase == DesktopPhase::Desktop {
        let should_lock = crate::users::SESSION.lock().tick();
        if should_lock {
            LOCK_SCREEN.lock().on_show();
            DESKTOP_PHASE.store(DesktopPhase::Locked as u8, Ordering::Release);
            LOCK_SCREEN.lock().paint(ms);
            return;
        }
    }

    match phase {
        DesktopPhase::Login => {
            LOGIN_SCREEN.lock().paint();
            // Still show pending role dialog during login (on top)
            PENDING_ROLE_DIALOG.lock().check_timeout();
            PENDING_ROLE_DIALOG.lock().paint();
        }
        DesktopPhase::Locked => {
            LOCK_SCREEN.lock().paint(ms);
            // Still show pending role dialog during lock screen (on top)
            PENDING_ROLE_DIALOG.lock().check_timeout();
            PENDING_ROLE_DIALOG.lock().paint();
        }
        DesktopPhase::Desktop => {
            // Poll input before painting — handle clicks that might change Z-order or close windows.
            poll_input();

            // Background is already cleared by ui::WINDOW_MANAGER.draw_to_backbuffer()
            // which runs before paint_desktop in gui_refresh_task.
            // Root shell elements are the desktop surface: under the wallpaper.
            crate::shell::paint_desktop_layer();
            {
                let mut bg = DESKTOP_BG.lock();
                bg.refresh(ms);
                bg.paint();
            }

            // Panel tick + paint (writes into back buffer via Renderer).
            // Skipped while a VSH shell is running: the shell draws its own taskbar.
            if !crate::shell::is_active() {
                let mut panel = PANEL.lock();
                panel.tick(ms);
                let wm = DESKTOP_WM.lock();
                panel.sync_windows(&wm);
                drop(wm);
                panel.paint();
            }

            // Paint all windows in Z-order: for each window, draw its WM
            // chrome (title bar, border, background) then immediately draw
            // the owning app's content clipped to the content area.
            // This ensures a higher-Z window's content always appears above
            // a lower-Z window's content — not just its chrome.
            {
                // Collect window ids sorted by z-order (lowest first = back to front).
                let sorted_ids: Vec<u32> = {
                    let mut wm = DESKTOP_WM.lock();
                    wm.windows.sort_by_key(|w| w.z_order);
                    wm.windows.iter()
                        .filter(|w| w.state != WindowState::Minimized)
                        .map(|w| w.id)
                        .collect()
                };

                // Snapshot all live pool slots for each app type.
                // Using try_lock + collect means we never hold a lock across the paint loop.
                let term_slots: alloc::vec::Vec<(u32, usize)> = crate::apps::TERMINAL_POOL
                    .iter().enumerate()
                    .filter_map(|(idx, slot)| slot.try_lock().and_then(|t|
                        if t.window_id != 0 { Some((t.window_id, idx)) } else { None }))
                    .collect();
                let fm_slots: alloc::vec::Vec<(u32, usize)> = crate::apps::FILE_MANAGER_POOL
                    .iter().enumerate()
                    .filter_map(|(idx, slot)| slot.try_lock().and_then(|t|
                        if t.window_id != 0 { Some((t.window_id, idx)) } else { None }))
                    .collect();
                let text_editor_slots: alloc::vec::Vec<(u32, usize)> = crate::apps::TEXT_EDITOR_POOL
                    .iter().enumerate()
                    .filter_map(|(idx, slot)| slot.try_lock().and_then(|t|
                        if t.window_id != 0 { Some((t.window_id, idx)) } else { None }))
                    .collect();
                let launcher_slots: alloc::vec::Vec<(u32, usize)> = crate::apps::APP_LAUNCHER_POOL
                    .iter().enumerate()
                    .filter_map(|(idx, slot)| slot.try_lock().and_then(|t|
                        if t.window_id != 0 { Some((t.window_id, idx)) } else { None }))
                    .collect();
                let media_slots: alloc::vec::Vec<(u32, usize)> = crate::apps::MEDIA_VIEWER_POOL
                    .iter().enumerate()
                    .filter_map(|(idx, slot)| slot.try_lock().and_then(|t|
                        if t.window_id != 0 { Some((t.window_id, idx)) } else { None }))
                    .collect();
                let clip_slots: alloc::vec::Vec<(u32, usize)> = crate::apps::CLIPBOARD_VIEWER_POOL
                    .iter().enumerate()
                    .filter_map(|(idx, slot)| slot.try_lock().and_then(|t|
                        if t.window_id != 0 { Some((t.window_id, idx)) } else { None }))
                    .collect();
                let mesh_slots: alloc::vec::Vec<(u32, usize)> = crate::apps::MESH_MANAGER_POOL
                    .iter().enumerate()
                    .filter_map(|(idx, slot)| slot.try_lock().and_then(|t|
                        if t.window_id != 0 { Some((t.window_id, idx)) } else { None }))
                    .collect();

                for win_id in sorted_ids {
                    // 1. Draw window chrome (frame, title bar, background).
                    {
                        let wm = DESKTOP_WM.lock();
                        if let Some(win) = wm.windows.iter().find(|w| w.id == win_id) {
                            win.paint(&wm.renderer);
                        }
                    }

                    // 2. Draw app content clipped to the window's content area.
                    clip_to_window(win_id);
                    if let Some(&(_, idx)) = term_slots.iter().find(|(id, _)| *id == win_id) {
                        crate::apps::TERMINAL_POOL[idx].lock().paint();
                    } else if let Some(&(_, idx)) = text_editor_slots.iter().find(|(id, _)| *id == win_id) {
                        crate::apps::TEXT_EDITOR_POOL[idx].lock().paint();
                    } else if let Some(&(_, idx)) = fm_slots.iter().find(|(id, _)| *id == win_id) {
                        crate::apps::FILE_MANAGER_POOL[idx].lock().paint();
                    } else if let Some(&(_, idx)) = launcher_slots.iter().find(|(id, _)| *id == win_id) {
                        crate::apps::APP_LAUNCHER_POOL[idx].lock().paint();
                    } else if let Some(&(_, idx)) = media_slots.iter().find(|(id, _)| *id == win_id) {
                        crate::apps::MEDIA_VIEWER_POOL[idx].lock().paint(ms);
                    } else if let Some(&(_, idx)) = clip_slots.iter().find(|(id, _)| *id == win_id) {
                        crate::apps::CLIPBOARD_VIEWER_POOL[idx].lock().paint();
                    } else if let Some(&(_, idx)) = mesh_slots.iter().find(|(id, _)| *id == win_id) {
                        crate::apps::MESH_MANAGER_POOL[idx].lock().paint();
                    }
                    crate::ui::clip_reset();
                }
            }
            // Everything else in the shell: above windows, below notifications/menus.
            crate::shell::paint_overlay_layer();
            // Notifications (topmost layer, writes into back buffer).
            {
                let mut ns = NOTIF_SYS.lock();
                ns.tick(ms);
                ns.paint();
            }

            // Context Menu (absolute topmost)
            {
                CONTEXT_MENU.lock().paint();
            }

            // Pending Role Dialog (mesh confirmation, above drag indicator)
            {
                PENDING_ROLE_DIALOG.lock().check_timeout();
                PENDING_ROLE_DIALOG.lock().paint();
            }

            // Drag and drop visual indicator
            {
                let dd = crate::apps::GLOBAL_DRAG_DROP.lock();
                if let Some(state) = &*dd {
                    if state.is_dragging {
                        let state_mouse = crate::task::mouse::MOUSE_STATE.lock();
                        let mx = state_mouse.x as i32;
                        let my = state_mouse.y as i32;
                        drop(state_mouse);
                        let r = crate::gui_toolkit::Renderer::new(0);
                        let filename = match state.source_path.rfind('/') {
                            Some(pos) => &state.source_path[pos + 1..],
                            None => &state.source_path,
                        };
                        let w = (filename.len() * 8 + 16) as u32;
                        r.fill_rect(crate::gui_toolkit::Rect::new(mx + 10, my + 10, w, 24), crate::graphics::Color { r: 60, g: 100, b: 200 });
                        r.stroke_rect(crate::gui_toolkit::Rect::new(mx + 10, my + 10, w, 24), crate::graphics::Color { r: 255, g: 255, b: 255 });
                        r.draw_text(filename, mx + 18, my + 16, crate::graphics::Color { r: 255, g: 255, b: 255 });
                    }
                }
            }
            crate::render2d_kernel::paint_test_card_if_enabled();
            // NOTE: no commit_back_buffer() or draw_cursor_hw() here.
            // gui_refresh_task does ONE commit + cursor draw after this returns.
        }
    }
}


// =====================================================================
// -----| Bug Condition Exploration Tests — Property 1 |-----
//
// Task 1: Write bug condition exploration test
// Spec:   .kiro/specs/win32-input-restore
//
// These tests encode the EXPECTED behavior after the fix.
// They FAIL on unfixed code (bare `return` in Settings branch, no
// liveness check in Win32 branch) and PASS on the fixed code.
//
// To run: call `dispatch_key_bug_tests::run()` from a kernel test boot.
//
// Validates: Requirements 2.1, 2.2, 2.3
// =====================================================================

pub mod dispatch_key_bug_tests {
    use core::sync::atomic::Ordering;
    use crate::desktop::{
        DESKTOP_WM, DESKTOP_PHASE, DesktopPhase,
    };
    use crate::apps::TERMINAL_POOL;
    use crate::win32_user::WIN32_USER;

    // ------------------------------------------------------------------
    // Helpers
    // ------------------------------------------------------------------

    /// Prepare the desktop for a test case:
    ///   - Set phase to Desktop so dispatch_key routes to the WM branch.
    ///   - Clear all terminal input buffers to get a clean baseline.
    ///   - Reset focused_id.
    fn setup_desktop_phase() {
        DESKTOP_PHASE.store(DesktopPhase::Desktop as u8, Ordering::Release);
        // Clear all terminal input buffers used as the observable "character
        // arrived" indicator.  Printable chars are appended to input_buf by
        // TerminalEmulator::on_key — NOT to history — so we measure input_buf.
        for slot in TERMINAL_POOL.iter() {
            if let Some(mut t) = slot.try_lock() {
                t.input_buf.clear();
            }
        }
        // Reset focused_id to a clean state.
        if let Some(mut wm) = DESKTOP_WM.try_lock() {
            wm.focused_id = None;
        }
    }
}

// =====================================================================
// Property 2: Preservation — Correct-Focus Dispatch Unchanged
// =====================================================================
//
// These tests verify that every routing path that was correct BEFORE the
// bugfix is still correct AFTER the bugfix.  They iterate over many
// combinations of characters and live-focus states to give property-based
// coverage.
//
// Three properties are checked:
//
//   P2a — Live terminal routing: for all printable chars and all live
//         terminal `focused_id` values, the char arrives in exactly the
//         correct pool slot's input_buf — no other slot is affected.
//
//   P2b — None fallback: for all printable chars with `focused_id = None`,
//         the char arrives in TERMINAL_POOL[0].input_buf.
//
//   P2c — Live Win32 routing: for all printable chars with a live Win32
//         `focused_id`, a WM_CHAR message with the correct w_param is
//         posted to the owning thread's queue.
//
// Expected outcome: ALL tests PASS (confirms preserved baseline behavior).
//
// Validates: Requirements 3.1, 3.2, 3.3
// =====================================================================

pub mod dispatch_key_preservation_tests {
    use core::sync::atomic::Ordering;
    use crate::desktop::{DESKTOP_WM, DESKTOP_PHASE, DesktopPhase};
    use crate::apps::TERMINAL_POOL;
    use crate::win32_user::{WIN32_USER, WM_CHAR, WindowEntry, Msg};

    // ------------------------------------------------------------------
    // Helpers
    // ------------------------------------------------------------------

    /// Printable ASCII characters used as the test "alphabet" for all
    /// property sweeps.  Covers lower/upper letters, digits, and a
    /// representative selection of punctuation.
    const PRINTABLE_CHARS: &[char] = &[
        'a', 'b', 'z', 'A', 'Z', '0', '9',
        ' ', '!', '@', '#', '$', '%', '^', '&', '*',
        '(', ')', '-', '+', '=', '[', ']', '\\', '/',
        '.', ',', '?', '<', '>',
    ];

    /// Prepare the desktop for a test case.
    /// - Phase = Desktop so dispatch_key enters the WM routing branch.
    /// - All terminal input_buf fields cleared for a clean baseline.
    /// - focused_id reset to None.
    fn setup() {
        DESKTOP_PHASE.store(DesktopPhase::Desktop as u8, Ordering::Release);
        for slot in TERMINAL_POOL.iter() {
            if let Some(mut t) = slot.try_lock() {
                t.input_buf.clear();
            }
        }
        if let Some(mut wm) = DESKTOP_WM.try_lock() {
            wm.focused_id = None;
        }
    }

    /// Return the current input_buf length for every terminal pool slot.
    fn terminal_buf_lens() -> [usize; crate::apps::MAX_TERMINALS] {
        let mut lens = [0usize; crate::apps::MAX_TERMINALS];
        for (i, slot) in TERMINAL_POOL.iter().enumerate() {
            if let Some(t) = slot.try_lock() {
                lens[i] = t.input_buf.len();
            }
        }
        lens
    }

    /// Register a minimal Win32 window directly into WIN32_USER for tests.
    ///
    /// Uses a fixed-range hwnd (0xF000 + window_count) to avoid collisions
    /// with real allocations, and a veilos_win_id opened through the real
    /// WM so focused_id lookup works correctly.
    ///
    /// Returns (hwnd, veilos_win_id).
    fn register_test_win32_window(tid: u32) -> (u32, u32) {
        // Open a real DESKTOP_WM window so focused_id routing resolves it.
        let vid = DESKTOP_WM.lock().open("WIN32 TEST WINDOW", 50, 50, 320, 240, 0);

        // Inject a WindowEntry into WIN32_USER so find_hwnd_by_veilos_id
        // returns Some for this veilos_win_id.  We derive a hwnd from
        // window_count() to avoid touching the private next_hwnd field.
        let hwnd = {
            let mut state = WIN32_USER.lock();
            // Use 0xF000 + current count as a scratch hwnd that won't
            // collide with the sequential allocator (which starts at 1).
            let hwnd = 0xF000_u32 + state.window_count() as u32;
            state.ensure_queue(tid);
            state.windows.push(WindowEntry {
                hwnd,
                owning_tid: tid,
                owning_pid: 0xDEAD,
                class_name: alloc::string::String::from("TESTCLASS"),
                title:      alloc::string::String::from("WIN32 TEST WINDOW"),
                x: 50, y: 50, w: 320, h: 240,
                visible: true, enabled: true,
                wnd_proc: 0,
                style: 0, ex_style: 0,
                user_data: 0,
                veilos_win_id: vid,
            });
            hwnd
        };

        (hwnd, vid)
    }

    /// Tear down a test Win32 window registered by register_test_win32_window.
    fn remove_test_win32_window(hwnd: u32, vid: u32) {
        {
            let mut state = WIN32_USER.lock();
            state.windows.retain(|w| w.hwnd != hwnd);
        }
        DESKTOP_WM.lock().close_silent(vid);
    }

    /// Drain the message queue for `tid` (destructive), return how many
    /// WM_CHAR messages with w_param == `w_param` were found, and re-post
    /// all non-WM_CHAR messages so other code is not disturbed.
    fn drain_and_count_wm_char(tid: u32, w_param: u64) -> usize {
        let mut saved: alloc::vec::Vec<Msg> = alloc::vec::Vec::new();
        let mut wm_char_count = 0usize;

        // Drain everything from the queue.
        loop {
            let msg = WIN32_USER.lock().get_message(tid, true);
            match msg {
                Some(m) => {
                    if m.message == WM_CHAR && m.w_param == w_param {
                        wm_char_count += 1;
                    } else {
                        saved.push(m);
                    }
                }
                None => break,
            }
        }

        // Re-post messages that were not the target WM_CHAR.
        for m in saved {
            WIN32_USER.lock().post(tid, m);
        }

        wm_char_count
    }

    // ------------------------------------------------------------------
    // P2a — Live Terminal Routing Preservation
    //
    // For every printable character and every terminal pool slot that has
    // a registered window, dispatch_key must route the character to that
    // exact pool slot's input_buf and MUST NOT affect any other slot.
    // ------------------------------------------------------------------
    pub fn test_p2a_live_terminal_routing() {
        for slot_idx in 0..crate::apps::MAX_TERMINALS {
            // Open a WM window and bind it to this pool slot.
            setup();
            let win_id = DESKTOP_WM.lock().open(
                "TERMINAL PRESERVATION TEST", 10, 10, 400, 300, 0,
            );
            {
                let mut t = TERMINAL_POOL[slot_idx].lock();
                t.window_id = win_id;
                t.input_buf.clear();
            }

            // Confirm terminal_slot_for_window recognises the window.
            assert!(
                crate::apps::terminal_slot_for_window(win_id).is_some(),
                "SETUP ERROR: terminal_slot_for_window did not find slot {} window_id={}",
                slot_idx, win_id
            );

            // Point focus at this terminal window.
            DESKTOP_WM.lock().focused_id = Some(win_id);

            // Sweep all printable characters.
            for &ch in PRINTABLE_CHARS {
                // Capture baseline lengths for ALL slots.
                let pre = terminal_buf_lens();

                crate::desktop::dispatch_key(ch);

                let post = terminal_buf_lens();

                // The target slot MUST have grown by exactly 1.
                assert!(
                    post[slot_idx] == pre[slot_idx] + 1,
                    "P2a FAIL: dispatch_key({:?}) with terminal slot {} focused \
                     did NOT deliver to the correct pool slot. \
                     pre={}, post={}. Routing is broken.",
                    ch, slot_idx, pre[slot_idx], post[slot_idx]
                );

                // No other slot must have grown.
                for other in 0..crate::apps::MAX_TERMINALS {
                    if other != slot_idx {
                        assert_eq!(
                            post[other], pre[other],
                            "P2a FAIL: dispatch_key({:?}) with terminal slot {} focused \
                             also wrote to slot {}. Character was double-routed. \
                             pre={}, post={}.",
                            ch, slot_idx, other, pre[other], post[other]
                        );
                    }
                }
            }

            // Cleanup.
            DESKTOP_WM.lock().close_silent(win_id);
            TERMINAL_POOL[slot_idx].lock().window_id = 0;
            TERMINAL_POOL[slot_idx].lock().input_buf.clear();
        }
    }

    // ------------------------------------------------------------------
    // P2b — None Fallback Preservation
    //
    // For every printable character with focused_id = None, dispatch_key
    // must route the character to TERMINAL_POOL[0].input_buf.  This path
    // was correct before the fix and must remain correct after it.
    // ------------------------------------------------------------------
    pub fn test_p2b_none_fallback() {
        setup();

        // Make sure TERMINAL_POOL[0] has no registered window so
        // terminal_slot_for_window(0) returns None and we truly exercise
        // the fallback path (focused_id == None → bottom of dispatch_key).
        // We save and restore window_id in case the boot terminal is set up.
        let saved_win_id = TERMINAL_POOL[0].lock().window_id;
        TERMINAL_POOL[0].lock().window_id = 0;

        // Ensure focused_id is None.
        DESKTOP_WM.lock().focused_id = None;

        for &ch in PRINTABLE_CHARS {
            let pre = TERMINAL_POOL[0].lock().input_buf.len();

            crate::desktop::dispatch_key(ch);

            let post = TERMINAL_POOL[0].lock().input_buf.len();

            assert!(
                post > pre,
                "P2b FAIL: dispatch_key({:?}) with focused_id=None did NOT \
                 route character to TERMINAL_POOL[0].input_buf. \
                 pre={}, post={}. Fallback path is broken.",
                ch, pre, post
            );
        }

        // Restore.
        TERMINAL_POOL[0].lock().window_id = saved_win_id;
    }

    // ------------------------------------------------------------------
    // P2c — Live Win32 Routing Preservation
    //
    // For every printable character with a live Win32 focused_id (hwnd
    // present in WIN32_USER), dispatch_key must post a WM_CHAR message
    // with the correct w_param to the owning thread's queue.  No
    // TERMINAL_POOL slot must grow (the key must NOT double-route).
    // ------------------------------------------------------------------
    pub fn test_p2c_live_win32_routing() {
        // Use a dedicated test thread ID that won't conflict with the main
        // kernel thread (tid 1) or any real process thread.
        let test_tid: u32 = 0xBEEF_1234;

        setup();

        let (hwnd, vid) = register_test_win32_window(test_tid);

        // Confirm the liveness check will find this window.
        assert!(
            WIN32_USER.lock().find_hwnd_by_veilos_id(vid).is_some(),
            "SETUP ERROR: find_hwnd_by_veilos_id returned None for just-registered \
             test Win32 window vid={}", vid
        );

        // Point focus at the Win32 window.
        DESKTOP_WM.lock().focused_id = Some(vid);

        // Drain any stale messages in the test thread queue from setup.
        while WIN32_USER.lock().get_message(test_tid, true).is_some() {}

        for &ch in PRINTABLE_CHARS {
            // Capture terminal baseline — no terminal should grow.
            let pre_terms = terminal_buf_lens();

            // Re-affirm focus before each dispatch (dispatch_key doesn't
            // mutate focused_id for the live Win32 case, but we're explicit).
            DESKTOP_WM.lock().focused_id = Some(vid);
            crate::desktop::dispatch_key(ch);

            let post_terms = terminal_buf_lens();

            // Drain and count WM_CHAR messages for this character.
            let wm_char_count = drain_and_count_wm_char(test_tid, ch as u64);

            // The WM_CHAR must have been posted exactly once.
            assert!(
                wm_char_count >= 1,
                "P2c FAIL: dispatch_key({:?}) with live Win32 focused_id={} \
                 did NOT post WM_CHAR to thread queue tid={}. \
                 count={}. Win32 routing is broken.",
                ch, vid, test_tid, wm_char_count
            );

            // No terminal slot must have grown (key must not double-route).
            for (i, (&pre, &post)) in pre_terms.iter().zip(post_terms.iter()).enumerate() {
                assert_eq!(
                    post, pre,
                    "P2c FAIL: dispatch_key({:?}) with live Win32 focused_id={} \
                     also wrote to TERMINAL_POOL[{}]. Character was double-routed. \
                     pre={}, post={}.",
                    ch, vid, i, pre, post
                );
            }
        }

        // Cleanup.
        remove_test_win32_window(hwnd, vid);
    }

    // ------------------------------------------------------------------
    // Entry point — call from kernel test boot
    // ------------------------------------------------------------------

    /// Run all preservation property tests.
    ///
    /// ALL tests are expected to PASS on both unfixed and fixed code
    /// (these paths were already correct before the bugfix).
    /// A failure here means a regression was introduced.
    ///
    /// Call after dispatch_key_bug_tests::run() to verify no regressions.
    pub fn run() {
        crate::println!("[TEST] dispatch_key preservation — START");

        crate::println!("[TEST]   P2a: Live terminal routing preservation ...");
        test_p2a_live_terminal_routing();
        crate::println!("[TEST]   P2a: PASS — chars routed to correct terminal slot only");

        crate::println!("[TEST]   P2b: None fallback preservation ...");
        test_p2b_none_fallback();
        crate::println!("[TEST]   P2b: PASS — chars with focused_id=None reached TERMINAL_POOL[0]");

        crate::println!("[TEST]   P2c: Live Win32 routing preservation ...");
        test_p2c_live_win32_routing();
        crate::println!("[TEST]   P2c: PASS — WM_CHAR posted to correct thread queue; no terminal double-route");

        crate::println!("[TEST] dispatch_key preservation — ALL PASS");
        crate::println!("[TEST] All three preservation properties hold:");
        crate::println!("[TEST]   Terminal routing:  correct slot only, no spillover");
        crate::println!("[TEST]   None fallback:     always reaches TERMINAL_POOL[0]");
        crate::println!("[TEST]   Win32 routing:     WM_CHAR queued, no double-route");
    }
}
