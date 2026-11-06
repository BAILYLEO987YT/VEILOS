// -----| VEILOS Native Applications |-----
//
// Phase 9 roadmap items implemented here:
//
//   [x] File manager
//   [x] Terminal emulator
//   [x] App launcher
//   [x] Settings app (UI backed by desktop.rs SettingsApp)

#![allow(dead_code)]

use alloc::vec::Vec;
use alloc::string::String;
use spin::Mutex;
use crate::println;
use crate::vfs::VFS;
use crate::graphics::Color;
use crate::desktop::{DESKTOP_WM, notify, NotifCategory};
use crate::gui_toolkit::{Renderer, Rect, ACTIVE_THEME};

// =====================================================================
// -----| Text Editor |-----
// =====================================================================

// -----| Scrolling |-----

const BAR_W: i32 = 12;
const MIN_THUMB: i32 = 20;
const WHEEL_ROWS: i32 = 3;

/// The part of a window that scrolls, and the track beside it.
#[derive(Clone, Copy)]
struct ScrollArea {
    right: i32,
    y: i32,
    h: u32,
    /// Whole rows that fit, at least one.
    rows: usize,
}

/// An app whose content is longer than its window: the wheel, and a scroll bar
/// that can be dragged and clicked. Scrolling is in whole rows; `top` is the
/// first on show, kept in range there as the window can grow and the content
/// can shrink after scrolling.
trait Scrollable {
    fn window_id(&self) -> u32;
    fn area(&self, win: Rect) -> ScrollArea;
    fn total(&self) -> usize;
    fn raw_top(&self) -> usize;
    fn set_top(&mut self, top: usize, max: usize);
    /// The thumb being dragged: where in it the pointer took hold.
    fn grab(&self) -> Option<i32>;
    fn set_grab(&mut self, grab: Option<i32>);

    fn max_top(&self, rows: usize) -> usize {
        self.total().saturating_sub(rows)
    }
    fn top(&self, rows: usize) -> usize {
        self.raw_top().min(self.max_top(rows))
    }
    /// The thumb's top and height, if the content needs a bar.
    fn thumb(&self, a: ScrollArea) -> Option<(i32, i32)> {
        let total = self.total();
        let max = self.max_top(a.rows);
        if max == 0 {
            return None;
        }
        let track = a.h as i32;
        let h = (track as i64 * a.rows as i64 / total as i64).max(MIN_THUMB as i64).min(track as i64) as i32;
        Some((a.y + ((track - h) as i64 * self.top(a.rows) as i64 / max as i64) as i32, h))
    }
    /// Positive goes towards the end.
    fn scroll_rows(&mut self, by: i32, rows: usize) {
        let max = self.max_top(rows);
        let to = (self.top(rows) as i64 + by as i64).clamp(0, max as i64);
        self.set_top(to as usize, max);
    }
    /// The thumb's top moved to `y`.
    fn drag_thumb(&mut self, a: ScrollArea, y: i32) {
        let max = self.max_top(a.rows);
        let Some((_, h)) = self.thumb(a) else { return };
        let span = (a.h as i32 - h).max(1) as i64;
        let at = ((y - a.y) as i64).clamp(0, span);
        self.set_top(((at * max as i64 + span / 2) / span) as usize, max);
    }
    /// A click at (x, y): true if the scroll bar took it, the thumb to drag or
    /// the track to go a page.
    fn bar_click(&mut self, a: ScrollArea, x: i32, y: i32) -> bool {
        let Some((thumb_y, thumb_h)) = self.thumb(a) else { return false };
        if x < a.right - BAR_W || y < a.y || y >= a.y + a.h as i32 {
            return false;
        }
        if y >= thumb_y && y < thumb_y + thumb_h {
            self.set_grab(Some(y - thumb_y));
        } else {
            let page = a.rows as i32;
            self.scroll_rows(if y < thumb_y { -page } else { page }, a.rows);
        }
        true
    }
    fn draw_bar(&self, r: &Renderer, a: ScrollArea, track: Color, thumb: Color, held: Color) {
        let Some((y, h)) = self.thumb(a) else { return };
        let x = a.right - BAR_W;
        r.fill_rect(Rect::new(x, a.y, BAR_W as u32, a.h), track);
        r.fill_rect(Rect::new(x + 2, y, BAR_W as u32 - 4, h as u32), if self.grab().is_some() { held } else { thumb });
    }
}

fn wheel_in<T: Scrollable>(pool: &[Mutex<T>], id: u32, rect: Rect, by: i32) -> bool {
    for slot in pool {
        if let Some(mut app) = slot.try_lock() {
            if app.window_id() == id {
                let rows = app.area(rect).rows;
                app.scroll_rows(by, rows);
                return true;
            }
        }
    }
    false
}

fn drag_in<T: Scrollable>(pool: &[Mutex<T>], y: i32, left: bool) {
    for slot in pool {
        let Some(mut app) = slot.try_lock() else { continue };
        let Some(grab) = app.grab() else { continue };
        if !left {
            app.set_grab(None);
            continue;
        }
        let id = app.window_id();
        let rect = DESKTOP_WM.try_lock().and_then(|wm| wm.windows.iter().find(|w| w.id == id).map(|w| w.rect));
        if let Some(rect) = rect {
            let a = app.area(rect);
            app.drag_thumb(a, y - grab);
        }
    }
}

fn click_in<T: Scrollable>(pool: &[Mutex<T>], id: u32, rect: Rect, x: i32, y: i32) -> bool {
    for slot in pool {
        if let Some(mut app) = slot.try_lock() {
            if app.window_id() == id {
                let a = app.area(rect);
                return app.bar_click(a, x, y);
            }
        }
    }
    false
}

/// The wheel and scroll bar thumbs, from the input poll. May run in the timer
/// interrupt: try_lock only, and nothing is held across the two kinds.
pub fn scroll_pointer(x: i32, y: i32, left: bool, wheel: i32) {
    if wheel != 0 {
        let under = DESKTOP_WM.try_lock().and_then(|wm| {
            let id = wm.content_at(x, y)?;
            let rect = wm.windows.iter().find(|w| w.id == id)?.rect;
            Some((id, rect))
        });
        if let Some((id, rect)) = under {
            let by = wheel * WHEEL_ROWS;
            let _ = wheel_in(&FILE_MANAGER_POOL, id, rect, by)
                || wheel_in(&TEXT_EDITOR_POOL, id, rect, by)
                || wheel_in(&TERMINAL_POOL, id, rect, by)
                || wheel_in(&CLIPBOARD_VIEWER_POOL, id, rect, by);
        }
    }
    drag_in(&FILE_MANAGER_POOL, y, left);
    drag_in(&TEXT_EDITOR_POOL, y, left);
    drag_in(&TERMINAL_POOL, y, left);
    drag_in(&CLIPBOARD_VIEWER_POOL, y, left);
}

/// A click in a window's content: true if a scroll bar took it.
pub fn scroll_bar_click(id: u32, x: i32, y: i32) -> bool {
    let Some(rect) = DESKTOP_WM.try_lock().and_then(|wm| wm.windows.iter().find(|w| w.id == id).map(|w| w.rect)) else { return false };
    click_in(&FILE_MANAGER_POOL, id, rect, x, y)
        || click_in(&TEXT_EDITOR_POOL, id, rect, x, y)
        || click_in(&TERMINAL_POOL, id, rect, x, y)
        || click_in(&CLIPBOARD_VIEWER_POOL, id, rect, x, y)
}

pub struct TextEditorState {
    pub window_id: u32,
    pub path: String,
    pub content: String,
    pub cursor_pos: usize,
    pub scroll_y: usize,
    /// Character index where the selection starts (None = no selection).
    pub selection_start: Option<usize>,
    /// Character index where the selection ends (None = no selection).
    pub selection_end: Option<usize>,
    thumb_grab: Option<i32>,
}

const EDITOR_ROW_H: i32 = 16;
const EDITOR_HEADER_H: u32 = 28;

impl Scrollable for TextEditorState {
    fn window_id(&self) -> u32 { self.window_id }
    fn area(&self, win: Rect) -> ScrollArea {
        let h = win.h.saturating_sub(38 + EDITOR_HEADER_H);
        ScrollArea { right: win.x + win.w as i32 - 1, y: win.y + 36 + EDITOR_HEADER_H as i32, h, rows: ((h as i32 / EDITOR_ROW_H) as usize).max(1) }
    }
    fn total(&self) -> usize { self.content.bytes().filter(|&b| b == b'\n').count() + 1 }
    fn raw_top(&self) -> usize { self.scroll_y }
    fn set_top(&mut self, top: usize, _max: usize) { self.scroll_y = top; }
    fn grab(&self) -> Option<i32> { self.thumb_grab }
    fn set_grab(&mut self, grab: Option<i32>) { self.thumb_grab = grab; }
}

impl TextEditorState {
    pub const fn new_const() -> Self {
        Self { window_id: 0, path: String::new(), content: String::new(), cursor_pos: 0, scroll_y: 0, selection_start: None, selection_end: None, thumb_grab: None }
    }

    pub fn paint(&self) {
        let wm  = DESKTOP_WM.lock();
        let win = match wm.windows.iter().find(|w| w.id == self.window_id) {
            Some(w) => w, None => return,
        };
        let r     = Renderer::new(0);
        let t     = ACTIVE_THEME.lock();
        let p     = t.palette;
        drop(t);

        let cx = win.rect.x + 1;
        let cy = win.rect.y + 36;
        let cw = win.rect.w.saturating_sub(2);
        let ch = win.rect.h.saturating_sub(38);

        r.fill_rect(Rect::new(cx, cy, cw, ch), p.background);

        // Header / Save button
        r.fill_rect(Rect::new(cx, cy, cw, 28), p.surface);
        r.stroke_rect(Rect::new(cx, cy, cw, 28), p.border);
        r.draw_text(&alloc::format!("  PATH: {}", self.path), cx + 4, cy + 8, p.text_secondary);

        let save_rect = Rect::new(cx + cw as i32 - 70, cy + 4, 60, 20);
        r.fill_rect(save_rect, p.accent);
        r.draw_text(" SAVE ", cx + cw as i32 - 62, cy + 6, p.text_primary);

        // Compute selection range (normalised: lo <= hi).
        let (sel_lo, sel_hi) = match (self.selection_start, self.selection_end) {
            (Some(a), Some(b)) if a != b => {
                if a <= b { (a, b) } else { (b, a) }
            }
            _ => (usize::MAX, usize::MAX), // No active selection
        };

        // Content
        let area = self.area(win.rect);
        let text_y = area.y;
        let row_h = EDITOR_ROW_H;
        let max_rows = area.rows as i32;
        let top = self.top(area.rows) as i32;

        let mut row = 0;
        let mut col = 0;
        let mut cursor_r = 0;
        let mut cursor_c = 0;

        for (i, c) in self.content.chars().enumerate() {
            if i == self.cursor_pos { cursor_r = row; cursor_c = col; }
            if c == '\n' { row += 1; col = 0; continue; }
            if c == '\r' { continue; }
            
            if row >= top && row < top + max_rows {
                let sy = text_y + (row - top) * row_h;
                let sx = cx + 8 + col * 8;

                // Draw selection highlight behind the character.
                if i >= sel_lo && i < sel_hi {
                    r.fill_rect(Rect::new(sx, sy, 9, row_h as u32), p.selection);
                }

                let mut tmp = String::new();
                tmp.push(c);
                let text_color = if i >= sel_lo && i < sel_hi { p.text_primary } else { p.text_primary };
                r.draw_text(&tmp, sx, sy, text_color);
            }
            col += 1;
        }
        if self.cursor_pos == self.content.chars().count() {
            cursor_r = row; cursor_c = col;
        }

        if cursor_r >= top && cursor_r < top + max_rows {
            let cy_pos = text_y + (cursor_r - top) * row_h;
            let cx_pos = cx + 8 + cursor_c * 8;
            r.fill_rect(Rect::new(cx_pos, cy_pos, 2, 14), p.cursor);
        }
        self.draw_bar(&r, area, p.surface, p.border, p.accent);
    }

    pub fn on_key(&mut self, ch: char) {
        let mut chars: Vec<char> = self.content.chars().collect();
        match ch {
            '\x08' => {
                if self.cursor_pos > 0 && self.cursor_pos <= chars.len() {
                    chars.remove(self.cursor_pos - 1);
                    self.content = chars.into_iter().collect();
                    self.cursor_pos -= 1;
                }
            }
            c if (!c.is_control() || c == '\n' || c == '\r') => {
                let to_insert = if c == '\r' { '\n' } else { c };
                if self.cursor_pos <= chars.len() {
                    chars.insert(self.cursor_pos, to_insert);
                    self.content = chars.into_iter().collect();
                    self.cursor_pos += 1;
                }
            }
            _ => {}
        }
        
        // The view follows the cursor. Key handlers may hold the window list,
        // so with no answer from it the view stays as it is.
        let mut row = 0;
        for (i, c) in self.content.chars().enumerate() {
            if i == self.cursor_pos { break; }
            if c == '\n' { row += 1; }
        }
        let id = self.window_id;
        if let Some(rect) = DESKTOP_WM.try_lock().and_then(|wm| wm.windows.iter().find(|w| w.id == id).map(|w| w.rect)) {
            let rows = self.area(rect).rows;
            if row < self.scroll_y { self.scroll_y = row; }
            if row >= self.scroll_y + rows { self.scroll_y = row + 1 - rows; }
        }
    }

    pub fn handle_click(&mut self, x: i32, y: i32) {
        let wm = DESKTOP_WM.lock();
        let win = match wm.windows.iter().find(|w| w.id == self.window_id) { Some(w) => w, None => return };
        let cx = win.rect.x + 1;
        let cy = win.rect.y + 36;
        let cw = win.rect.w.saturating_sub(2);
        drop(wm);

        let save_rect = Rect::new(cx + cw as i32 - 70, cy + 4, 60, 20);
        if save_rect.contains(x, y) {
            match crate::vfs::VFS.write_file(&self.path, self.content.as_bytes()) {
                Ok(_) => { notify(NotifCategory::Success, "SAVED", &self.path); },
                Err(_) => { notify(NotifCategory::Warning, "ERROR", "Could not save file"); },
            }
        }
    }
}

pub const MAX_TEXT_EDITORS: usize = 4;
pub static TEXT_EDITOR_POOL: [Mutex<TextEditorState>; MAX_TEXT_EDITORS] = [
    Mutex::new(TextEditorState::new_const()),
    Mutex::new(TextEditorState::new_const()),
    Mutex::new(TextEditorState::new_const()),
    Mutex::new(TextEditorState::new_const()),
];

pub fn open_text_editor(x: i32, y: i32, w: u32, h: u32, z: u32, path: &str) -> (u32, usize) {
    let id = crate::desktop::DESKTOP_WM.lock().open(&alloc::format!("TEXT EDITOR - {}", path), x, y, w, h, z);
    let live_ids: alloc::vec::Vec<u32> = {
        crate::desktop::DESKTOP_WM.lock().windows.iter().map(|w| w.id).collect()
    };

    for (idx, slot) in TEXT_EDITOR_POOL.iter().enumerate() {
        if let Some(mut ed) = slot.try_lock() {
            if ed.window_id == 0 || !live_ids.contains(&ed.window_id) {
                *ed = TextEditorState::new_const();
                ed.window_id = id;
                ed.path = String::from(path);
                return (id, idx);
            }
        }
    }
    let mut ed = TEXT_EDITOR_POOL[0].lock();
    *ed = TextEditorState::new_const();
    ed.window_id = id;
    ed.path = String::from(path);
    (id, 0)
}

pub fn text_editor_slot_for_window(id: u32) -> Option<usize> {
    for (idx, slot) in TEXT_EDITOR_POOL.iter().enumerate() {
        if let Some(ed) = slot.try_lock() {
            if ed.window_id == id { return Some(idx); }
        }
    }
    None
}

// =====================================================================
// -----| Media Viewer |-----
// =====================================================================

pub struct MediaViewer {
    pub window_id:    u32,
    pub path:         String,
    pub media_data:   Vec<u8>,
    // -----| animation / video state |-----
    pub gif_frames:   Option<Vec<crate::gif_decoder::GifFrame>>,
    pub static_pixels: Option<(i32, i32, usize, Vec<u8>)>, // (w, h, channels, pixels)
    pub current_frame: u32,
    pub frame_count:   u32,
    pub frame_delay_ms: u16,   // per-frame delay (GIF) or derived from FPS (VMV)
    pub last_tick_ms:  u64,    // timestamp of last frame advance
    pub is_animated:   bool,
}

impl MediaViewer {
    pub const fn new() -> Self {
        Self {
            window_id: 0,
            path: String::new(),
            media_data: Vec::new(),
            gif_frames: None,
            static_pixels: None,
            current_frame: 0,
            frame_count: 0,
            frame_delay_ms: 100,
            last_tick_ms: 0,
            is_animated: false,
        }
    }

    /// Call this after loading media_data to pre-decode animated formats.
    pub fn prepare(&mut self) {
        let upper = self.path.to_uppercase();
        if upper.ends_with(".GIF") {
            if let Some(decoded) = crate::gif_decoder::decode_gif(&self.media_data) {
                self.frame_count = decoded.frames.len() as u32;
                self.is_animated = self.frame_count > 1;
                if self.is_animated {
                    // Use the first frame's delay; fall back to 100ms
                    self.frame_delay_ms = if decoded.frames[0].delay_ms > 0 {
                        decoded.frames[0].delay_ms
                    } else {
                        100
                    };
                }
                self.gif_frames = Some(decoded.frames);
            }
        } else if upper.ends_with(".VMV") {
            if let Some((_, _, fps, fc, _)) = crate::ui::vmv_parse_header(&self.media_data) {
                self.frame_count = fc;
                self.is_animated = fc > 1;
                self.frame_delay_ms = if fps > 0 { 1000 / fps } else { 33 }; // default ~30fps
            }
        } else if upper.ends_with(".JPG") || upper.ends_with(".JPEG") {
            self.static_pixels = crate::ui::decode_jpeg(&self.media_data);
        } else if upper.ends_with(".PNG") {
            self.static_pixels = crate::ui::decode_png(&self.media_data);
        }
    }
    
    /// Paint the current media content.  For animated media, also advances the
    /// frame if enough time has passed since the last advance.
    pub fn paint(&mut self, ms: u64) {
        let wm = crate::desktop::DESKTOP_WM.lock();
        let win = match wm.windows.iter().find(|w| w.id == self.window_id) {
            Some(w) => w, None => return,
        };
        
        let cx = win.rect.x + 1;
        let cy = win.rect.y + 36;
        let cw = win.rect.w.saturating_sub(2);
        let ch = win.rect.h.saturating_sub(38);
        drop(wm);
        
        let r = crate::gui_toolkit::Renderer::new(0);
        r.fill_rect(Rect::new(cx, cy, cw, ch), Color { r: 10, g: 10, b: 10 });

        // Advance frame for animated media
        if self.is_animated && self.frame_count > 0 {
            if ms.wrapping_sub(self.last_tick_ms) >= self.frame_delay_ms as u64 {
                self.current_frame = (self.current_frame + 1) % self.frame_count;
                self.last_tick_ms = ms;

                // Update per-frame delay for GIF (each frame can have different timing)
                if let Some(ref frames) = self.gif_frames {
                    if let Some(f) = frames.get(self.current_frame as usize) {
                        if f.delay_ms > 0 { self.frame_delay_ms = f.delay_ms; }
                    }
                }
            }
        }
        
        if !self.media_data.is_empty() {
            let target_w = Some(cw.saturating_sub(8) as i32);
            let target_h = Some(ch.saturating_sub(8) as i32);

            let upper = self.path.to_uppercase();
            if upper.ends_with(".BMP") {
                r.draw_bmp(&self.media_data, cx + 4, cy + 4, target_w, target_h);
            } else if upper.ends_with(".TGA") {
                crate::ui::draw_tga_atomic(&self.media_data, cx + 4, cy + 4, target_w, target_h);
            } else if upper.ends_with(".JPG") || upper.ends_with(".JPEG") || upper.ends_with(".PNG") {
                if let Some((w, h, channels, ref pixels)) = self.static_pixels {
                    crate::ui::draw_decoded_rgb(pixels, w, h, channels, cx + 4, cy + 4, target_w, target_h);
                } else {
                    r.draw_text("Decode failed or caching.", cx + 10, cy + 20, Color { r: 200, g: 50, b: 50 });
                }
            } else if upper.ends_with(".GIF") {
                if let Some(ref frames) = self.gif_frames {
                    let idx = self.current_frame as usize;
                    if idx < frames.len() {
                        crate::ui::draw_gif_frame_atomic(&frames[idx], cx + 4, cy + 4, target_w, target_h);
                    }
                } else {
                    r.draw_text("GIF decode failed.", cx + 10, cy + 20, Color { r: 200, g: 50, b: 50 });
                }
            } else if upper.ends_with(".VMV") {
                crate::ui::draw_vmv_frame_atomic(&self.media_data, self.current_frame, cx + 4, cy + 4, target_w, target_h);
                // Show playback info
                let info_str = alloc::format!("Frame {}/{}", self.current_frame + 1, self.frame_count);
                r.draw_text(&info_str, cx + 4, cy + ch as i32 - 16, Color { r: 150, g: 150, b: 170 });
            } else {
                r.draw_text("Unsupported media format.", cx + 10, cy + 20, Color { r: 200, g: 50, b: 50 });
            }
        } else {
            r.draw_text("Failed to load media or empty file.", cx + 10, cy + 20, Color { r: 200, g: 50, b: 50 });
        }
    }
}

pub static MEDIA_VIEWER_POOL: [Mutex<MediaViewer>; 4] = [
    Mutex::new(MediaViewer::new()),
    Mutex::new(MediaViewer::new()),
    Mutex::new(MediaViewer::new()),
    Mutex::new(MediaViewer::new()),
];

pub fn open_media_viewer(path: &str, data: Vec<u8>) -> (u32, usize) {
    let id = crate::desktop::DESKTOP_WM.lock().open(&alloc::format!("MEDIA: {}", path.split('/').last().unwrap_or("FILE")), 120, 120, 600, 480, 0);
    let live_ids: alloc::vec::Vec<u32> = {
        crate::desktop::DESKTOP_WM.lock().windows.iter().map(|w| w.id).collect()
    };

    for (idx, slot) in MEDIA_VIEWER_POOL.iter().enumerate() {
        if let Some(mut mv) = slot.try_lock() {
            if mv.window_id == 0 || !live_ids.contains(&mv.window_id) {
                *mv = MediaViewer::new();
                mv.window_id = id;
                mv.path = String::from(path);
                mv.media_data = data;
                mv.prepare();
                return (id, idx);
            }
        }
    }
    
    // Fallback if full
    let mut mv = MEDIA_VIEWER_POOL[0].lock();
    *mv = MediaViewer::new();
    mv.window_id = id;
    mv.path = String::from(path);
    mv.media_data = data;
    mv.prepare();
    (id, 0)
}

pub fn media_viewer_slot_for_window(id: u32) -> Option<usize> {
    for (idx, slot) in MEDIA_VIEWER_POOL.iter().enumerate() {
        if let Some(mv) = slot.try_lock() {
            if mv.window_id == id { return Some(idx); }
        }
    }
    None
}

/// Checks if the currently focused window is an animated media viewer that needs a new frame.
/// Set when a program writes to its console window (console.rs), so the
/// desktop repaints.
pub static TERMINAL_OUTPUT: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

pub fn needs_animation_refresh(ms: u64) -> bool {
    if TERMINAL_OUTPUT.swap(false, core::sync::atomic::Ordering::AcqRel) {
        return true;
    }
    let focused_id = crate::desktop::DESKTOP_WM.lock().focused_id;
    if let Some(fid) = focused_id {
        for slot in MEDIA_VIEWER_POOL.iter() {
            if let Some(mv) = slot.try_lock() {
                if mv.window_id == fid && mv.is_animated && mv.frame_count > 0 {
                    if ms.wrapping_sub(mv.last_tick_ms) >= mv.frame_delay_ms as u64 {
                        return true;
                    }
                }
            }
        }
    }
    false
}

// =====================================================================
// -----| File Manager |-----
// =====================================================================

pub struct FileManagerState {
    pub window_id: u32,
    pub cwd:       String,
    pub entries:   Vec<FileEntry>,
    pub selected:  Option<usize>,
    pub scroll_y:  u32,
    /// Indices of multi-selected entries (Ctrl+Click to toggle).
    pub multi_selected: Vec<usize>,
    /// Drag-and-drop state.
    pub dragging_entry: Option<usize>,
    pub drag_start_x: i32,
    pub drag_start_y: i32,
    pub is_dragging: bool,
    pub last_click_ms: u64,
    pub last_click_row: Option<usize>,
    thumb_grab: Option<i32>,
}

const FM_ROW_H: i32 = 22;
const FM_ADDRESS_H: u32 = 28;
const FM_STATUS_H: u32 = 20;

impl Scrollable for FileManagerState {
    fn window_id(&self) -> u32 { self.window_id }
    fn area(&self, win: Rect) -> ScrollArea {
        let h = win.h.saturating_sub(38 + FM_ADDRESS_H + FM_STATUS_H);
        ScrollArea { right: win.x + win.w as i32 - 1, y: win.y + 36 + FM_ADDRESS_H as i32, h, rows: ((h as i32 / FM_ROW_H) as usize).max(1) }
    }
    fn total(&self) -> usize { self.entries.len() }
    fn raw_top(&self) -> usize { self.scroll_y as usize }
    fn set_top(&mut self, top: usize, _max: usize) { self.scroll_y = top as u32; }
    fn grab(&self) -> Option<i32> { self.thumb_grab }
    fn set_grab(&mut self, grab: Option<i32>) { self.thumb_grab = grab; }
}

#[derive(Clone)]
pub struct FileEntry {
    pub name: String,
    pub size: u64,
    pub is_dir: bool,
}

impl FileManagerState {
    /// Const-safe empty state for static initialisation.
    pub const fn new_const() -> Self {
        Self {
            window_id: 0,
            cwd:       String::new(),
            entries:   Vec::new(),
            selected:  None,
            scroll_y:  0,
            multi_selected: Vec::new(),
            dragging_entry: None,
            drag_start_x: 0,
            drag_start_y: 0,
            is_dragging: false,
            last_click_ms: 0,
            last_click_row: None,
            thumb_grab: None,
        }
    }

    pub fn refresh(&mut self) {
        // Set default cwd if not yet initialised.
        if self.cwd.is_empty() { self.cwd = String::from("/"); }
        self.entries.clear();
        if self.cwd != "/" {
            self.entries.push(FileEntry { name: String::from(".."), size: 0, is_dir: true });
        }
        match VFS.list_dir(&self.cwd) {
            Ok(list) => {
                for e in list {
                    self.entries.push(FileEntry {
                        name:   e.name.clone(),
                        size:   e.size,
                        is_dir: e.file_type == crate::vfs::FileType::Directory,
                    });
                }
            }
            Err(_) => {
                self.entries.push(FileEntry {
                    name: String::from("(empty or unreadable)"), size: 0, is_dir: false
                });
            }
        }
    }

    pub fn navigate(&mut self, name: &str) {
        if name == ".." {
            if let Some(pos) = self.cwd.rfind('/') {
                if pos == 0 { self.cwd = String::from("/"); }
                else         { self.cwd.truncate(pos); }
            }
        } else if self.cwd == "/" {
            self.cwd = alloc::format!("/{}", name);
        } else {
            self.cwd = alloc::format!("{}/{}", self.cwd, name);
        }
        self.selected = None;
        self.scroll_y = 0;
        self.thumb_grab = None;
        self.refresh();
    }

    pub fn paint(&self) {
        let wm = DESKTOP_WM.lock();
        let win = match wm.windows.iter().find(|w| w.id == self.window_id) {
            Some(w) => w, None => return,
        };
        let r  = Renderer::new(0);
        let t  = ACTIVE_THEME.lock();
        let p  = t.palette;
        drop(t);

        let cx = win.rect.x + 1;
        let cy = win.rect.y + 36;
        let cw = win.rect.w.saturating_sub(2);
        let list = self.area(win.rect);

        // Address bar
        r.fill_rect(Rect::new(cx, cy, cw, FM_ADDRESS_H), p.surface);
        r.stroke_rect(Rect::new(cx, cy, cw, FM_ADDRESS_H), p.border);
        r.draw_text(&alloc::format!("  PATH: {}", self.cwd), cx + 4, cy + 8, p.text_secondary);

        // File list
        let list_y  = list.y;
        let list_h  = list.h;
        r.fill_rect(Rect::new(cx, list_y, cw, list_h), p.background);

        let row_h      = FM_ROW_H;
        let max_rows   = list.rows;
        let start      = self.top(max_rows);
        // The sizes stay clear of the bar.
        let bar_room   = if self.thumb(list).is_some() { BAR_W } else { 0 };

        for (i, entry) in self.entries.iter().enumerate().skip(start).take(max_rows) {
            let ey     = list_y + (i - start) as i32 * row_h;
            let row_bg = if self.selected == Some(i) { p.selection }
                         else if i % 2 == 0           { p.background }
                         else                          { p.surface };
            r.fill_rect(Rect::new(cx, ey, cw, row_h as u32), row_bg);

            let icon = if entry.is_dir { "[DIR] " } else { "[FILE]" };
            let tc   = if entry.is_dir { p.accent } else { p.text_primary };
            r.draw_text(icon,        cx + 8,  ey + 6, tc);
            r.draw_text(&entry.name, cx + 60, ey + 6, p.text_primary);
            if !entry.is_dir && entry.size > 0 {
                let size_str = if entry.size >= 1024*1024 {
                    alloc::format!("{} MB", entry.size / 1024 / 1024)
                } else if entry.size >= 1024 {
                    alloc::format!("{} KB", entry.size / 1024)
                } else {
                    alloc::format!("{} B", entry.size)
                };
                r.draw_text(&size_str, cx + cw as i32 - 80 - bar_room, ey + 6, p.text_secondary);
            }
        }

        // Scrollbar
        self.draw_bar(&r, list, p.surface, p.border, p.accent);

        // Status bar
        let sb_y = list_y + list_h as i32;
        r.fill_rect(Rect::new(cx, sb_y, cw, FM_STATUS_H), p.surface);
        r.draw_text(&alloc::format!("  {} items", self.entries.len()), cx + 4, sb_y + 4, p.text_secondary);
    }

    pub fn handle_click(&mut self, _x: i32, y: i32) {
        let wm  = DESKTOP_WM.lock();
        let win = match wm.windows.iter().find(|w| w.id == self.window_id) {
            Some(w) => w, None => return,
        };
        let list = self.area(win.rect);
        drop(wm);
        if y < list.y || y >= list.y + list.h as i32 { return; }

        let visible = (y - list.y) / FM_ROW_H;
        if visible as usize >= list.rows { return; }
        let row = visible as usize + self.top(list.rows);

        if row < self.entries.len() {
            let now = crate::interrupts::TICKS.load(core::sync::atomic::Ordering::Relaxed);
            if self.selected == Some(row) {
                if self.last_click_row == Some(row) && now.saturating_sub(self.last_click_ms) < 500 {
                    // Snapshot name and is_dir BEFORE navigate(), which clears entries.
                    let name   = self.entries[row].name.clone();
                    let is_dir = self.entries[row].is_dir;
                    // Snapshot path for drag-drop BEFORE any mutation.
                    let drag_path = if name != ".." {
                        Some(if self.cwd == "/" {
                            alloc::format!("/{}", name)
                        } else {
                            alloc::format!("{}/{}", self.cwd, name)
                        })
                    } else {
                        None
                    };

                    // Navigate or open — this may call refresh() and clear entries.
                    if is_dir {
                        self.navigate(&name);
                    } else {
                        self.open_file(&name);
                    }

                    // Update drag-drop state using the pre-snapshot path.
                    if let Some(path) = drag_path {
                        let state = crate::task::mouse::MOUSE_STATE.lock();
                        *GLOBAL_DRAG_DROP.lock() = Some(DragDropState {
                            source_path: path,
                            drag_start_x: state.x as i32,
                            drag_start_y: state.y as i32,
                            is_dragging: false,
                        });
                    } else {
                        *GLOBAL_DRAG_DROP.lock() = None;
                    }
                }
            } else {
                self.selected = Some(row);

                // Update drag-drop state. entries[row] is still valid here
                // because we did NOT navigate (no double-click yet).
                let name = self.entries[row].name.clone();
                if name != ".." {
                    let path = if self.cwd == "/" {
                        alloc::format!("/{}", name)
                    } else {
                        alloc::format!("{}/{}", self.cwd, name)
                    };
                    let state = crate::task::mouse::MOUSE_STATE.lock();
                    *GLOBAL_DRAG_DROP.lock() = Some(DragDropState {
                        source_path: path,
                        drag_start_x: state.x as i32,
                        drag_start_y: state.y as i32,
                        is_dragging: false,
                    });
                } else {
                    *GLOBAL_DRAG_DROP.lock() = None;
                }
            }

            self.last_click_ms  = now;
            self.last_click_row = Some(row);
        }
    }

    // -----| file viewing |-----

    /// Open a file by reading it from VFS and displaying its contents
    /// in a new terminal window.
    fn open_file(&self, name: &str) {
        // Build the full VFS path.
        let path = if self.cwd == "/" {
            alloc::format!("/{}", name)
        } else {
            alloc::format!("{}/{}", self.cwd, name)
        };
        open_file_absolute(&path, name);
    }
}

pub struct DragDropState {
    pub source_path: String,
    pub drag_start_x: i32,
    pub drag_start_y: i32,
    pub is_dragging: bool,
}

pub static GLOBAL_DRAG_DROP: Mutex<Option<DragDropState>> = Mutex::new(None);

pub struct FrequentFiles {
    pub files: alloc::vec::Vec<(String, usize)>,
}
impl FrequentFiles {
    pub const fn new() -> Self { Self { files: alloc::vec::Vec::new() } }
    pub fn record(&mut self, path: &str) {
        if let Some(entry) = self.files.iter_mut().find(|(p, _)| p == path) {
            entry.1 += 1;
        } else {
            self.files.push((String::from(path), 1));
        }
        self.files.sort_by(|a, b| b.1.cmp(&a.1));
        if self.files.len() > 10 {
            self.files.truncate(10);
        }
    }
}
pub static FREQUENT_FILES: Mutex<FrequentFiles> = Mutex::new(FrequentFiles::new());

pub fn open_file_absolute(path: &str, name: &str) {
        FREQUENT_FILES.lock().record(path);
        if name.ends_with(".exe") || name.ends_with(".EXE") {
            // It's an executable, so attempt to load and spawn it.
            let mut frame_allocator = crate::memory::GlobalFrameAllocator;
            match crate::loader::load_and_spawn_pe(path, &mut frame_allocator) {
                Ok(process) => {
                    notify(NotifCategory::Success, "LAUNCHED", name);
                    // A console program's window: its output appears here. The
                    // program hasn't run yet (it's queued on the executor this
                    // code runs on), so nothing it writes is missed.
                    if let Some(console) = &process.console {
                        let (_id, idx) = open_terminal(120, 100, 560, 400, 0);
                        let mut term = TERMINAL_POOL[idx].lock();
                        term.history.clear();
                        term.history.push(alloc::format!("=== {} ===", name));
                        term.console = Some(console.clone());
                        let (cols, rows) = term.grid_size();
                        drop(term);
                        console.attach(idx, cols, rows);
                    }
                }
                Err(e) => {
                    notify(
                        NotifCategory::Warning,
                        "EXECUTION FAILED",
                        &alloc::format!("{}: {}", name, e),
                    );
                }
            }
            return;
        }

        // Read the file contents.
        let data = match crate::vfs::VFS.read_file(path) {
            Ok(bytes) => bytes,
            Err(_) => {
                notify(
                    NotifCategory::Warning,
                    "FILE ERROR",
                    &alloc::format!("Cannot read: {}", name),
                );
                return;
            }
        };

        // Check for known media formats BEFORE the UTF-8 test.
        // PNG and some JPEG files may contain valid UTF-8 sequences in their
        // headers, so extension-based routing must happen first.
        let name_upper = name.to_uppercase();
        if name_upper.ends_with(".BMP") || name_upper.ends_with(".TGA")
        || name_upper.ends_with(".PNG") || name_upper.ends_with(".JPG")
        || name_upper.ends_with(".JPEG") || name_upper.ends_with(".GIF")
        || name_upper.ends_with(".VMV") {
            open_media_viewer(path, data);
            return;
        }

        match core::str::from_utf8(&data) {
            Ok(text) => {
                // Text file — open in native text editor.
                let (_id, idx) = open_text_editor(100, 80, 560, 400, 0, path);
                let mut editor = TEXT_EDITOR_POOL[idx].lock();
                editor.content = String::from(text);
                editor.cursor_pos = text.chars().count();
            }
            Err(_) => {

                // Show hex dump falling back to terminal.
                let (_id, idx) = open_terminal(100, 80, 560, 400, 0);
                let mut term = TERMINAL_POOL[idx].lock();
                term.history.clear();
                term.history.push(alloc::format!("=== {} ===", path));
                term.history.push(alloc::format!("SIZE: {} BYTES", data.len()));
                term.history.push(String::from("[BINARY FILE - HEX DUMP]"));
                term.history.push(String::new());
                for (i, chunk) in data.chunks(16).enumerate() {
                    if i >= 16 {
                        term.history.push(String::from("... (SHOWING FIRST 256 BYTES)"));
                        break;
                    }
                    let mut hex = String::new();
                    for b in chunk {
                        hex.push_str(&alloc::format!("{:02X} ", b));
                    }
                    term.history.push(alloc::format!("  {:04X}: {}", i * 16, hex));
                }
            }
        }

        notify(NotifCategory::Info, "OPENED", name);
}

pub static FILE_MANAGER: Mutex<FileManagerState> = Mutex::new(FileManagerState::new_const());

// =====================================================================
// -----| File Manager Pool |-----
// =====================================================================

pub const MAX_FILE_MANAGERS: usize = 4;

pub static FILE_MANAGER_POOL: [Mutex<FileManagerState>; MAX_FILE_MANAGERS] = [
    Mutex::new(FileManagerState::new_const()),
    Mutex::new(FileManagerState::new_const()),
    Mutex::new(FileManagerState::new_const()),
    Mutex::new(FileManagerState::new_const()),
];

/// Open a new File Manager window and bind it to a free pool slot.
pub fn open_file_manager(x: i32, y: i32, w: u32, h: u32, z: u32) -> (u32, usize) {
    let id = DESKTOP_WM.lock().open("FILE MANAGER", x, y, w, h, z);
    let live_ids: alloc::vec::Vec<u32> = {
        let wm = DESKTOP_WM.lock();
        wm.windows.iter().map(|win| win.id).collect()
    };
    for (idx, slot) in FILE_MANAGER_POOL.iter().enumerate() {
        if let Some(mut fm) = slot.try_lock() {
            if fm.window_id == 0 || !live_ids.contains(&fm.window_id) {
                *fm = FileManagerState::new_const();
                fm.window_id = id;
                fm.cwd = String::from("/");
                fm.refresh();
                return (id, idx);
            }
        }
    }
    // Evict slot 0.
    let mut fm = FILE_MANAGER_POOL[0].lock();
    *fm = FileManagerState::new_const();
    fm.window_id = id;
    fm.cwd = String::from("/");
    fm.refresh();
    (id, 0)
}

/// Find the pool slot whose `window_id` matches `id`.
pub fn fm_slot_for_window(id: u32) -> Option<usize> {
    for (idx, slot) in FILE_MANAGER_POOL.iter().enumerate() {
        if let Some(fm) = slot.try_lock() {
            if fm.window_id == id { return Some(idx); }
        }
    }
    None
}

// =====================================================================
// -----| Terminal Emulator |-----
// =====================================================================

pub struct TerminalEmulator {
    pub window_id:    u32,
    pub history:      Vec<String>,
    pub input_buf:    String,
    /// The first line on show when scrolled back; None follows the output.
    /// `history` only grows at the end, so a line number keeps its place.
    pub scroll_anchor: Option<usize>,
    thumb_grab:       Option<i32>,
    pub cursor_blink: bool,
    pub blink_tick:   u64,
    /// Selection state: (row, col) of start and end.
    pub selection_start: Option<(usize, usize)>,
    pub selection_end:   Option<(usize, usize)>,
    /// This terminal's TERMINAL_POOL slot.
    pub slot: usize,
    /// The console of the program running in this terminal: while it runs,
    /// typed lines go to it instead of the command line (console.rs).
    pub console: Option<alloc::sync::Arc<crate::console::Console>>,
}

const TERM_ROW_H: i32 = 16;
/// The prompt's line and the margins round the output.
const TERM_RESERVED_H: u32 = 28;

impl Scrollable for TerminalEmulator {
    fn window_id(&self) -> u32 { self.window_id }
    fn area(&self, win: Rect) -> ScrollArea {
        let h = win.h.saturating_sub(38 + TERM_RESERVED_H);
        ScrollArea { right: win.x + win.w as i32 - 1, y: win.y + 36 + 4, h, rows: ((h as i32 / TERM_ROW_H) as usize).max(1) }
    }
    fn total(&self) -> usize { self.history.len() }
    fn raw_top(&self) -> usize { self.scroll_anchor.unwrap_or(usize::MAX) }
    fn set_top(&mut self, top: usize, max: usize) { self.scroll_anchor = if top >= max { None } else { Some(top) }; }
    fn grab(&self) -> Option<i32> { self.thumb_grab }
    fn set_grab(&mut self, grab: Option<i32>) { self.thumb_grab = grab; }
}

impl TerminalEmulator {
    /// Const-safe empty state for static initialisation.
    pub const fn new_const() -> Self {
        Self {
            window_id:    0,
            history:      Vec::new(),
            input_buf:    String::new(),
            scroll_anchor: None,
            thumb_grab:   None,
            cursor_blink: true,
            blink_tick:   0,
            selection_start: None,
            selection_end: None,
            slot: 0,
            console: None,
        }
    }

    /// RUN: start a Windows program. A console program runs in this
    /// terminal (its output appears here, typed lines go to it); a GUI
    /// program opens its own window.
    fn run_program(&mut self, path: &str, args: &str) {
        let name = path.rsplit('/').next().unwrap_or(path);
        let mut frame_allocator = crate::memory::GlobalFrameAllocator;
        match crate::loader::load_and_spawn_pe_with_args(path, args, &mut frame_allocator) {
            Ok(process) => match &process.console {
                // It hasn't run yet (it's queued on the executor this runs
                // on), so nothing it writes is missed.
                Some(console) => {
                    let (cols, rows) = self.grid_size();
                    console.attach(self.slot, cols, rows);
                    self.console = Some(console.clone());
                }
                None => self.history.push(alloc::format!("  started {}", name)),
            },
            Err(e) => self.history.push(alloc::format!("  RUN {}: {}", path, e)),
        }
    }

    /// Populate the welcome message (called from init(), after heap is ready).
    pub fn populate_welcome(&mut self) {
        if self.history.is_empty() {
            self.history.push(String::from("VEILOS TERMINAL v2.0"));
            self.history.push(String::from("TYPE 'HELP' FOR COMMANDS"));
            self.history.push(String::from(""));
        }
    }

    /// Columns and rows of a console's grid in this terminal's window.
    pub fn grid_size(&self) -> (usize, usize) {
        let wm = DESKTOP_WM.lock();
        let Some(win) = wm.windows.iter().find(|w| w.id == self.window_id) else {
            return (crate::console::DEFAULT_COLUMNS, crate::console::DEFAULT_ROWS);
        };
        let cols = (win.rect.w.saturating_sub(2 + 16 + 12) / 8) as usize;
        let rows = (win.rect.h.saturating_sub(38 + 8) as i32 / TERM_ROW_H) as usize;
        (cols.max(10), rows.max(2))
    }

    /// A console program's screen: its active buffer's window, in colour,
    /// with the cursor.
    fn paint_console(&self, r: &Renderer, console: &crate::console::Console, cx: i32, cy: i32) {
        let buf = console.active();
        let b = buf.buf.lock();
        let (l, t, w, h) = b.window;
        let (x0, y0) = (cx + 8, cy + 4);
        for row in 0..h {
            let y = t + row;
            if y >= b.height {
                break;
            }
            let py = y0 + row as i32 * TERM_ROW_H;
            let mut x = 0;
            while x < w && l + x < b.width {
                let attr = b.cell(l + x, y).attr;
                let mut text = String::new();
                let mut end = x;
                while end < w && l + end < b.width && b.cell(l + end, y).attr == attr {
                    let ch = b.cell(l + end, y).ch as u32;
                    text.push(if ch < 0x20 { '?' } else { char::from_u32(ch).unwrap_or('?') });
                    end += 1;
                }
                let (fg, bg) = console_colours(attr);
                if attr & 0xF0 != 0 || attr & 0x4000 != 0 {
                    r.fill_rect(Rect::new(x0 + x as i32 * 8, py, ((end - x) * 8) as u32, TERM_ROW_H as u32), bg);
                }
                r.draw_text(&text, x0 + x as i32 * 8, py + 2, fg);
                if attr & 0x8000 != 0 {
                    r.fill_rect(Rect::new(x0 + x as i32 * 8, py + TERM_ROW_H - 2, ((end - x) * 8) as u32, 1), fg);
                }
                x = end;
            }
        }
        let (cx_, cy_) = b.cursor;
        if b.cursor_visible && self.cursor_blink && cx_ >= l && cx_ < l + w && cy_ >= t && cy_ < t + h {
            let (fg, _) = console_colours(b.attr);
            let px = x0 + (cx_ - l) as i32 * 8;
            let py = y0 + (cy_ - t) as i32 * TERM_ROW_H;
            r.fill_rect(Rect::new(px, py + TERM_ROW_H - 4, 8, 2), fg);
        }
    }

    pub fn tick(&mut self, ms: u64) {
        if ms.wrapping_sub(self.blink_tick) > 500 {
            self.cursor_blink = !self.cursor_blink;
            self.blink_tick   = ms;
        }
    }

    pub fn paint(&self) {
        let wm  = DESKTOP_WM.lock();
        let win = match wm.windows.iter().find(|w| w.id == self.window_id) {
            Some(w) => w, None => return,
        };
        let r     = Renderer::new(0);
        let t     = ACTIVE_THEME.lock();
        let p     = t.palette;
        drop(t);

        let cx = win.rect.x + 1;
        let cy = win.rect.y + 36;
        let cw = win.rect.w.saturating_sub(2);
        let ch = win.rect.h.saturating_sub(38);

        r.fill_rect(Rect::new(cx, cy, cw, ch), Color { r: 8, g: 12, b: 8 });
        if let Some(console) = self.console.as_ref().filter(|c| c.is_live()) {
            drop(wm);
            r.fill_rect(Rect::new(cx, cy, cw, ch), Color { r: 12, g: 12, b: 12 });
            self.paint_console(&r, console, cx, cy);
            return;
        }

        let row_h  = TERM_ROW_H;
        let area   = self.area(win.rect);
        let start  = self.top(area.rows);

        let text_c = Color { r: 0, g: 240, b: 60 };
        for (i, line) in self.history.iter().enumerate().skip(start).take(area.rows) {
            r.draw_text(line, cx + 8, area.y + (i - start) as i32 * row_h, text_c);
        }
        self.draw_bar(&r, area, Color { r: 10, g: 18, b: 10 }, Color { r: 0, g: 120, b: 40 }, text_c);

        // Prompt + input
        let prompt_y = cy + ch as i32 - row_h - 4;
        r.fill_rect(Rect::new(cx, prompt_y, cw, row_h as u32 + 2), Color { r: 10, g: 18, b: 10 });
        r.draw_text("> ", cx + 8, prompt_y + 2, text_c);
        r.draw_text(&self.input_buf, cx + 24, prompt_y + 2, Color { r: 255, g: 255, b: 255 });

        if self.cursor_blink {
            let cursor_x = cx + 24 + self.input_buf.len() as i32 * 8;
            r.fill_rect(Rect::new(cursor_x, prompt_y + 2, 6, 12), text_c);
        }
        let _ = p; // suppress unused warning
    }

    pub fn on_key(&mut self, ch: char) {
        // Typing brings the prompt's latest output back into view.
        self.scroll_anchor = None;
        // A program is running here: keys go to its console, which edits
        // and echoes lines itself (console.rs).
        if let Some(console) = self.console.clone().filter(|c| c.is_live()) {
            let (vk, unit) = match ch {
                '\n' | '\r' => (0x0D, 0x0D),
                '\x08' => (0x08, 0x08),
                '\t' => (0x09, 0x09),
                '\x1b' => (0x1B, 0x1B),
                ' ' => (0x20, 0x20),
                c if c.is_ascii_alphanumeric() => (c.to_ascii_uppercase() as u16, c as u16),
                c if (c as u32) < 0x10000 => (0, c as u16),
                _ => return,
            };
            console.push_input(&[
                crate::console::key_record(true, vk, unit, 0),
                crate::console::key_record(false, vk, unit, 0),
            ]);
            return;
        }
        match ch {
            '\n' | '\r' => {
                let cmd = self.input_buf.trim().to_uppercase();
                self.history.push(alloc::format!("> {}", self.input_buf));
                if cmd.starts_with("RUN ") {
                    // As typed: arguments keep their case.
                    let rest = String::from(self.input_buf.trim()[4..].trim());
                    let (path, args) = rest.split_once(' ').unwrap_or((rest.as_str(), ""));
                    self.run_program(&path.to_ascii_lowercase(), args.trim());
                } else {
                    crate::task::terminal::execute_command_pub(&cmd, &mut self.history);
                }
                self.input_buf.clear();
            }
            '\x08' => { self.input_buf.pop(); }
            c if !c.is_control() => { self.input_buf.push(c); }
            _ => {}
        }
    }
}

/// Maximum number of terminal windows that can be open simultaneously.
pub const MAX_TERMINALS: usize = 4;

/// Pool of independent terminal instances.
/// Each call to `open_terminal()` claims a free slot (or reuses the least-recently-used
/// one if all slots are taken). Instances are completely isolated — they have their own
/// history, input buffer, cursor state and window ID.
pub static TERMINAL_POOL: [Mutex<TerminalEmulator>; MAX_TERMINALS] = [
    Mutex::new(TerminalEmulator::new_const()),
    Mutex::new(TerminalEmulator::new_const()),
    Mutex::new(TerminalEmulator::new_const()),
    Mutex::new(TerminalEmulator::new_const()),
];

/// Legacy alias kept so existing callers that reference `TERMINAL_EMU` still
/// compile. They operate on slot 0, which is the initial/boot terminal.
pub static TERMINAL_EMU: Mutex<TerminalEmulator> = Mutex::new(TerminalEmulator::new_const());

/// Open a new terminal window and bind it to a free pool slot.
/// Returns the pool index so the caller can route input/paint to that slot.
pub fn open_terminal(x: i32, y: i32, w: u32, h: u32, z: u32) -> (u32, usize) {
    let id = crate::desktop::DESKTOP_WM.lock().open("TERMINAL", x, y, w, h, z);

    // Try to find a slot that has no live window (window_id == 0 or window no
    // longer exists in the WM).
    let live_ids: alloc::vec::Vec<u32> = {
        let wm = crate::desktop::DESKTOP_WM.lock();
        wm.windows.iter().map(|w| w.id).collect()
    };

    for (idx, slot) in TERMINAL_POOL.iter().enumerate() {
        if let Some(mut term) = slot.try_lock() {
            let owned = term.window_id;
            if owned == 0 || !live_ids.contains(&owned) {
                // Claim this slot for the new window.
                *term = TerminalEmulator::new_const();
                term.window_id = id;
                term.slot = idx;
                term.populate_welcome();
                return (id, idx);
            }
        }
    }

    // All slots are occupied — evict slot 0 (oldest) with a fresh instance.
    let mut term = TERMINAL_POOL[0].lock();
    *term = TerminalEmulator::new_const();
    term.window_id = id;
    term.populate_welcome();
    (id, 0)
}

/// Find the pool index whose `window_id` matches `id`, if any.
pub fn terminal_slot_for_window(id: u32) -> Option<usize> {
    for (idx, slot) in TERMINAL_POOL.iter().enumerate() {
        if let Some(term) = slot.try_lock() {
            if term.window_id == id {
                return Some(idx);
            }
        }
    }
    None
}

// =====================================================================
// -----| App Launcher |-----
// =====================================================================

pub struct AppEntry {
    pub name:        &'static str,
    pub description: &'static str,
    pub icon_color:  Color,
}

const APP_ENTRIES: &[AppEntry] = &[
    AppEntry { name: "FILE MANAGER",  description: "Browse and manage files",     icon_color: Color { r: 88,  g: 130, b: 220 } },
    AppEntry { name: "TERMINAL",      description: "Command line shell",           icon_color: Color { r: 55,  g: 185, b: 95  } },
    AppEntry { name: "POLICY TOGGLE", description: "Switch Security/Performance",  icon_color: Color { r: 220, g: 170, b: 40  } },
    AppEntry { name: "PROFILE",       description: "Performance profiler",         icon_color: Color { r: 210, g: 55,  b: 55  } },
    AppEntry { name: "PACKAGES",      description: "Package manager",              icon_color: Color { r: 140, g: 90,  b: 210 } },
    AppEntry { name: "CLIPBOARD",     description: "Clipboard History Viewer",     icon_color: Color { r: 210, g: 140, b: 30  } },
    AppEntry { name: "MESH MANAGER",  description: "VEIL-MESH Cluster Management", icon_color: Color { r: 100, g: 200, b: 250 } },
];

#[derive(PartialEq, Clone, Copy)]
pub enum QuickSettingOverlay {
    None,
    Volume,
    Brightness,
    Network,
}

pub struct AppLauncher {
    pub window_id: u32,
    pub search:    String,
    pub hovered:   Option<usize>,
    pub overlay:   QuickSettingOverlay,
}

impl AppLauncher {
    pub const fn new() -> Self {
        Self { window_id: 0, search: String::new(), hovered: None, overlay: QuickSettingOverlay::None }
    }

    pub fn paint(&self) {
        let wm  = DESKTOP_WM.lock();
        let win = match wm.windows.iter().find(|w| w.id == self.window_id) {
            Some(w) => w, None => return,
        };
        let r = Renderer::new(0);
        let t = ACTIVE_THEME.lock();
        let p = t.palette;
        drop(t);

        let cx = win.rect.x + 1;
        let cy = win.rect.y + 36;
        let cw = win.rect.w.saturating_sub(2);
        let ch = win.rect.h.saturating_sub(38);

        r.fill_rect(Rect::new(cx, cy, cw, ch), p.background);

        // Quick Settings at bottom
        let qs_y = cy + ch as i32 - 44;
        r.fill_rect(Rect::new(cx, qs_y, cw, 44), p.surface);
        r.stroke_rect(Rect::new(cx, qs_y, cw, 44), p.border);
        let qs_buttons = [
            ("POWER", p.danger),
            ("VOLUME", p.accent),
            ("BRIGHTNESS", p.warning),
            ("NETWORK", p.success),
            ("LOCK", p.text_secondary),
            ("LOGOUT", p.text_secondary),
        ];
        let qs_btn_w = cw / qs_buttons.len() as u32;
        for (i, (lbl, col)) in qs_buttons.iter().enumerate() {
            let bx = cx + (i as u32 * qs_btn_w) as i32;
            r.fill_rect(Rect::new(bx + (qs_btn_w as i32 - 20) / 2, qs_y + 4, 20, 20), *col);
            r.draw_text(lbl, bx + (qs_btn_w as i32 - lbl.len() as i32 * 8) / 2, qs_y + 28, p.text_primary);
        }

        // Search box
        r.fill_rect(Rect::new(cx + 16, cy + 12, 280, 30), p.surface);
        r.stroke_rect(Rect::new(cx + 16, cy + 12, 280, 30), p.accent);
        r.draw_text(&alloc::format!("SEARCH: {}", self.search), cx + 24, cy + 20, p.text_secondary);

        // App grid (2 columns)
        let icon_size: u32 = 48;
        let cols = 2u32;
        let cell_w = 140;
        let cell_h: u32 = 80;

        let filter_str = self.search.as_str();
        let filtered: Vec<&AppEntry> = APP_ENTRIES.iter()
            .filter(|a| filter_str.is_empty() || a.name.contains(filter_str))
            .collect();

        for (i, app) in filtered.iter().enumerate() {
            let col  = (i as u32) % cols;
            let row  = (i as u32) / cols;
            let ax   = cx + 16 + (col * cell_w) as i32;
            let ay   = cy + 56 + (row * cell_h) as i32;

            r.fill_rect(Rect::new(ax, ay, cell_w - 8, cell_h - 8), p.surface);
            r.stroke_rect(Rect::new(ax, ay, cell_w - 8, cell_h - 8), p.border);

            r.fill_rect(Rect::new(ax + 8, ay + 8, icon_size, icon_size - 16), app.icon_color);
            r.draw_text(app.name, ax + 8, ay + icon_size as i32 - 4, p.text_primary);
        }

        // Right side: Frequent files or Overlay
        let rx = cx + 320;
        let ry = cy + 12;

        match self.overlay {
            QuickSettingOverlay::None => {
                r.draw_text("FREQUENT FILES", rx, ry, p.accent);
                r.fill_rect(Rect::new(rx, ry + 20, cw.saturating_sub(336), 2), p.border);

                let freq = FREQUENT_FILES.lock();
                for (i, (path, count)) in freq.files.iter().enumerate() {
                    let fy = ry + 30 + (i as i32 * 24);
                    let mut label = path.clone();
                    if label.len() > 24 { label.truncate(21); label.push_str("..."); }
                    r.draw_text(&alloc::format!("{} ({})", label, count), rx, fy, p.text_primary);
                }
            }
            QuickSettingOverlay::Volume => {
                r.draw_text("VOLUME", rx, ry, p.accent);
                let vol = crate::audio::get_volume() as u32;
                let slider_w = cw.saturating_sub(352);
                r.fill_rect(Rect::new(rx, ry + 30, slider_w, 20), p.surface);
                r.fill_rect(Rect::new(rx, ry + 30, (vol * slider_w) / 100, 20), p.accent);
                r.draw_text(&alloc::format!("{}%", vol), rx, ry + 54, p.text_primary);
            }
            QuickSettingOverlay::Brightness => {
                r.draw_text("BRIGHTNESS", rx, ry, p.warning);
                let bri = crate::desktop::BRIGHTNESS.load(core::sync::atomic::Ordering::Relaxed) as u32;
                let slider_w = cw.saturating_sub(352);
                r.fill_rect(Rect::new(rx, ry + 30, slider_w, 20), p.surface);
                r.fill_rect(Rect::new(rx, ry + 30, (bri * slider_w) / 100, 20), p.warning);
                r.draw_text(&alloc::format!("{}%", bri), rx, ry + 54, p.text_primary);
            }
            QuickSettingOverlay::Network => {
                r.draw_text("AVAILABLE NETWORKS", rx, ry, p.success);
                r.fill_rect(Rect::new(rx, ry + 20, cw.saturating_sub(336), 2), p.border);
                let networks = [
                    ("Home_Network_5G", true),
                    ("Guest_WiFi", false),
                    ("Coffee_Shop_Free", false),
                    ("Wired Ethernet (VirtIO)", true),
                ];
                for (i, (name, secured)) in networks.iter().enumerate() {
                    let ny = ry + 30 + (i as i32 * 30);
                    r.draw_text(name, rx, ny, p.text_primary);
                    if *secured {
                        r.draw_text("[SECURE]", rx + 160, ny, p.text_secondary);
                    } else {
                        r.draw_text("[OPEN]", rx + 160, ny, p.danger);
                    }
                }
            }
        }
    }

    pub fn handle_click(&mut self, x: i32, y: i32) {
        let wm  = DESKTOP_WM.lock();
        let win = match wm.windows.iter().find(|w| w.id == self.window_id) {
            Some(w) => w, None => return,
        };
        let cx = win.rect.x + 1;
        let cy = win.rect.y + 36;
        let cw = win.rect.w.saturating_sub(2);
        let ch = win.rect.h.saturating_sub(38);
        drop(wm);

        // Check Quick Settings
        let qs_y = cy + ch as i32 - 44;
        if y >= qs_y && y < qs_y + 44 {
            let qs_btn_w = cw / 6;
            let idx = (x - cx) / qs_btn_w as i32;
            match idx {
                0 => { notify(NotifCategory::Warning, "POWER", "ACPI Shutdown requested"); }
                1 => { 
                    if self.overlay == QuickSettingOverlay::Volume { self.overlay = QuickSettingOverlay::None; }
                    else { self.overlay = QuickSettingOverlay::Volume; }
                }
                2 => {
                    if self.overlay == QuickSettingOverlay::Brightness { self.overlay = QuickSettingOverlay::None; }
                    else { self.overlay = QuickSettingOverlay::Brightness; }
                }
                3 => {
                    if self.overlay == QuickSettingOverlay::Network { self.overlay = QuickSettingOverlay::None; }
                    else { self.overlay = QuickSettingOverlay::Network; }
                }
                4 => {
                    crate::desktop::lock_desktop();
                    notify(NotifCategory::Info, "LOCKED", "Desktop locked");
                }
                5 => {
                    crate::users::SESSION.lock().logout();
                    crate::desktop::DESKTOP_PHASE.store(crate::desktop::DesktopPhase::Login as u8, core::sync::atomic::Ordering::Release);
                    notify(NotifCategory::Info, "LOGOUT", "Signed out successfully");
                }
                _ => {}
            }
            return;
        }

        // Check Overlay interactions
        let rx = cx + 320;
        let ry = cy + 12;
        let slider_w = cw.saturating_sub(352);

        match self.overlay {
            QuickSettingOverlay::None => {
                // Check frequent files
                if x >= rx && y >= ry + 30 && y < qs_y {
                    let idx = (y - (ry + 30)) / 24;
                    let freq = FREQUENT_FILES.lock();
                    if idx >= 0 && (idx as usize) < freq.files.len() {
                        let path = freq.files[idx as usize].0.clone();
                        drop(freq);
                        let name = path.split('/').last().unwrap_or("FILE");
                        open_file_absolute(&path, name);
                        return;
                    }
                }
            }
            QuickSettingOverlay::Volume => {
                if x >= rx && x <= rx + slider_w as i32 && y >= ry + 30 && y <= ry + 50 {
                    let mut pct = ((x - rx) * 100) / slider_w as i32;
                    if pct < 0 { pct = 0; }
                    if pct > 100 { pct = 100; }
                    crate::audio::set_volume(pct as u8);
                    return;
                }
            }
            QuickSettingOverlay::Brightness => {
                if x >= rx && x <= rx + slider_w as i32 && y >= ry + 30 && y <= ry + 50 {
                    let mut pct = ((x - rx) * 100) / slider_w as i32;
                    if pct < 0 { pct = 0; }
                    if pct > 100 { pct = 100; }
                    crate::desktop::BRIGHTNESS.store(pct, core::sync::atomic::Ordering::Relaxed);
                    return;
                }
            }
            QuickSettingOverlay::Network => {
                if x >= rx && y >= ry + 30 && y < qs_y {
                    let idx = (y - (ry + 30)) / 30;
                    if idx >= 0 && idx < 4 {
                        notify(NotifCategory::Success, "NETWORK", "Connected to network");
                        self.overlay = QuickSettingOverlay::None;
                        return;
                    }
                }
            }
        }

        // Check App grid
        let ax_start = cx + 16;
        let ay_start = cy + 56;
        let cell_w = 140i32;
        let cell_h = 80i32;
        let cols = 2i32;

        if x >= ax_start && x < ax_start + cols * cell_w && y >= ay_start && y < qs_y {
            let col = (x - ax_start) / cell_w;
            let row = (y - ay_start) / cell_h;
            let idx = (row * cols + col) as usize;
            
            let filter_str = self.search.as_str();
            let filtered: Vec<&AppEntry> = APP_ENTRIES.iter()
                .filter(|a| filter_str.is_empty() || a.name.contains(filter_str))
                .collect();
                
            if idx < filtered.len() {
                match filtered[idx].name {
                    "FILE MANAGER" => {
                        open_file_manager(100, 60, 520, 400, 0);
                        notify(NotifCategory::Info, "File Manager", "Opened");
                    }
                    "TERMINAL" => {
                        open_terminal(80, 80, 550, 380, 0);
                        notify(NotifCategory::Info, "Terminal", "Opened");
                    }
                    "POLICY TOGGLE" => {
                        let mut pol = crate::policy::POLICY.lock();
                        match pol.get_mode() {
                            crate::policy::PolicyMode::Security => {
                                pol.set_mode(crate::policy::PolicyMode::Performance);
                                drop(pol);
                                notify(NotifCategory::Warning, "Mode Changed", "Switched to Performance");
                            }
                            crate::policy::PolicyMode::Performance => {
                                pol.set_mode(crate::policy::PolicyMode::Security);
                                drop(pol);
                                notify(NotifCategory::Success, "Mode Changed", "Switched to Security");
                            }
                        }
                    }
                    "PROFILE" => {
                        let mut prof = crate::debugger::PROFILER.lock();
                        prof.enabled = !prof.enabled;
                        let state = prof.enabled;
                        drop(prof);
                        notify(NotifCategory::Info, "Profiler", if state { "Started" } else { "Stopped" });
                    }
                    "PACKAGES" => {
                        let reg = crate::pkgmgr::PACKAGE_REGISTRY.lock();
                        let n   = reg.list_installed().len();
                        drop(reg);
                        notify(NotifCategory::Info, "Packages", &alloc::format!("{} installed", n));
                    }
                    "CLIPBOARD" => {
                        open_clipboard_viewer(200, 150, 400, 500, 0);
                        notify(NotifCategory::Info, "Clipboard", "Opened Viewer");
                    }
                    "MESH MANAGER" => {
                        open_mesh_manager(150, 100, 600, 450, 0);
                        notify(NotifCategory::Info, "Mesh Manager", "Opened");
                    }
                    _ => {}
                }
            }
        }
    }

    pub fn handle_key(&mut self, ch: char) {
        match ch {
            '\x08' => { self.search.pop(); }
            c if !c.is_control() => { self.search.push(c.to_ascii_uppercase()); }
            _ => {}
        }
    }
}

pub static APP_LAUNCHER: Mutex<AppLauncher> = Mutex::new(AppLauncher::new());

// =====================================================================
// -----| App Launcher Pool |-----
// =====================================================================

pub const MAX_LAUNCHERS: usize = 4;

pub static APP_LAUNCHER_POOL: [Mutex<AppLauncher>; MAX_LAUNCHERS] = [
    Mutex::new(AppLauncher::new()),
    Mutex::new(AppLauncher::new()),
    Mutex::new(AppLauncher::new()),
    Mutex::new(AppLauncher::new()),
];

pub fn open_app_launcher(x: i32, y: i32, w: u32, h: u32, z: u32) -> (u32, usize) {
    let id = DESKTOP_WM.lock().open("APP LAUNCHER", x, y, w, h, z);
    let live_ids: alloc::vec::Vec<u32> = {
        let wm = DESKTOP_WM.lock();
        wm.windows.iter().map(|win| win.id).collect()
    };
    for (idx, slot) in APP_LAUNCHER_POOL.iter().enumerate() {
        if let Some(mut launcher) = slot.try_lock() {
            if launcher.window_id == 0 || !live_ids.contains(&launcher.window_id) {
                *launcher = AppLauncher::new();
                launcher.window_id = id;
                return (id, idx);
            }
        }
    }
    let mut launcher = APP_LAUNCHER_POOL[0].lock();
    *launcher = AppLauncher::new();
    launcher.window_id = id;
    (id, 0)
}

pub fn launcher_slot_for_window(id: u32) -> Option<usize> {
    for (idx, slot) in APP_LAUNCHER_POOL.iter().enumerate() {
        if let Some(launcher) = slot.try_lock() {
            if launcher.window_id == id { return Some(idx); }
        }
    }
    None
}

// =====================================================================
// -----| Clipboard Viewer |-----
// =====================================================================

pub struct ClipboardViewerApp {
    pub window_id: u32,
    pub scroll_y: usize,
    thumb_grab: Option<i32>,
}

/// Scrolling is by this much; the heading and cards are taller.
const CLIP_ROW_H: i32 = 20;
const CLIP_HEADING_H: i32 = 28;
const CLIP_CARD_H: i32 = 40;

impl Scrollable for ClipboardViewerApp {
    fn window_id(&self) -> u32 { self.window_id }
    fn area(&self, win: Rect) -> ScrollArea {
        let h = win.h.saturating_sub(38);
        ScrollArea { right: win.x + win.w as i32 - 1, y: win.y + 36, h, rows: ((h as i32 / CLIP_ROW_H) as usize).max(1) }
    }
    /// With the clipboard busy this answers as if empty, and `scroll_rows`
    /// leaves the view alone.
    fn total(&self) -> usize {
        let Some(clip) = crate::clipboard::CLIPBOARD.try_lock() else { return 0 };
        let cards = clip.history.iter().filter(|e| !e.consumed).count() as i32;
        ((CLIP_HEADING_H + cards * CLIP_CARD_H + CLIP_ROW_H - 1) / CLIP_ROW_H) as usize
    }
    fn raw_top(&self) -> usize { self.scroll_y }
    fn set_top(&mut self, top: usize, _max: usize) { self.scroll_y = top; }
    fn grab(&self) -> Option<i32> { self.thumb_grab }
    fn set_grab(&mut self, grab: Option<i32>) { self.thumb_grab = grab; }
}

impl ClipboardViewerApp {
    pub const fn new() -> Self {
        Self { window_id: 0, scroll_y: 0, thumb_grab: None }
    }

    pub fn paint(&self) {
        let wm  = DESKTOP_WM.lock();
        let win = match wm.windows.iter().find(|w| w.id == self.window_id) {
            Some(w) => w, None => return,
        };
        let r = Renderer::new(0);
        let t = ACTIVE_THEME.lock();
        let p = t.palette;
        drop(t);

        let cx = win.rect.x + 1;
        let cy = win.rect.y + 36;
        let cw = win.rect.w.saturating_sub(2);
        let ch = win.rect.h.saturating_sub(38);

        r.fill_rect(Rect::new(cx, cy, cw, ch), p.background);

        // Before taking the clipboard: these ask it too.
        let area = self.area(win.rect);
        let top = self.top(area.rows);
        let bar_room = if self.thumb(area).is_some() { BAR_W as u32 } else { 0 };
        self.draw_bar(&r, area, p.surface, p.border, p.accent);

        let clip = crate::clipboard::CLIPBOARD.lock();
        let mut y = cy + 4 - (top as i32 * CLIP_ROW_H);

        r.draw_text("CLIPBOARD HISTORY", cx + 8, y, p.text_secondary);
        y += 24;

        if clip.history.is_empty() {
            r.draw_text("No items in clipboard.", cx + 8, y, p.text_primary);
            return;
        }

        for entry in &clip.history {
            if entry.consumed { continue; }
            if y > cy + ch as i32 { break; }
            if y + 40 > cy {
                r.fill_rect(Rect::new(cx + 4, y, cw - 8 - bar_room, 36), p.surface);
                r.stroke_rect(Rect::new(cx + 4, y, cw - 8 - bar_room, 36), p.border);
                
                let chan_color = match entry.channel {
                    crate::clipboard::ClipboardChannel::Public => p.text_primary,
                    crate::clipboard::ClipboardChannel::Private => p.accent,
                    crate::clipboard::ClipboardChannel::Sensitive => Color { r: 255, g: 100, b: 100 },
                    crate::clipboard::ClipboardChannel::AppRestricted => Color { r: 200, g: 100, b: 255 },
                };
                
                r.draw_text(entry.channel.as_str(), cx + 8, y + 4, chan_color);
                
                let preview = entry.data.preview(30);
                r.draw_text(&preview, cx + 8, y + 20, p.text_primary);
            }
            y += 40;
        }
    }
}

pub const MAX_CLIPBOARD_VIEWERS: usize = 2;
pub static CLIPBOARD_VIEWER_POOL: [Mutex<ClipboardViewerApp>; MAX_CLIPBOARD_VIEWERS] = [
    Mutex::new(ClipboardViewerApp::new()),
    Mutex::new(ClipboardViewerApp::new()),
];

pub fn open_clipboard_viewer(x: i32, y: i32, w: u32, h: u32, z: u32) -> (u32, usize) {
    let id = DESKTOP_WM.lock().open("CLIPBOARD", x, y, w, h, z);
    let live_ids: alloc::vec::Vec<u32> = {
        let wm = DESKTOP_WM.lock();
        wm.windows.iter().map(|win| win.id).collect()
    };
    for (idx, slot) in CLIPBOARD_VIEWER_POOL.iter().enumerate() {
        if let Some(mut app) = slot.try_lock() {
            if app.window_id == 0 || !live_ids.contains(&app.window_id) {
                *app = ClipboardViewerApp::new();
                app.window_id = id;
                return (id, idx);
            }
        }
    }
    let mut app = CLIPBOARD_VIEWER_POOL[0].lock();
    *app = ClipboardViewerApp::new();
    app.window_id = id;
    (id, 0)
}

pub fn clipboard_viewer_slot_for_window(id: u32) -> Option<usize> {
    for (idx, slot) in CLIPBOARD_VIEWER_POOL.iter().enumerate() {
        if let Some(app) = slot.try_lock() {
            if app.window_id == id { return Some(idx); }
        }
    }
    None
}

// =====================================================================
// -----| Mesh Manager |-----
// =====================================================================

pub struct MeshManagerState {
    pub window_id: u32,
    pub selected_node: Option<crate::distributed::NodeId>,
}

impl MeshManagerState {
    pub const fn new() -> Self {
        Self { window_id: 0, selected_node: None }
    }

    pub fn paint(&self) {
        let wm  = DESKTOP_WM.lock();
        let win = match wm.windows.iter().find(|w| w.id == self.window_id) {
            Some(w) => w, None => return,
        };
        let r = Renderer::new(0);
        let p = ACTIVE_THEME.lock().palette;

        let cx = win.rect.x + 1;
        let cy = win.rect.y + 36;
        let cw = win.rect.w.saturating_sub(2);
        let ch = win.rect.h.saturating_sub(38);

        r.fill_rect(Rect::new(cx, cy, cw, ch), p.background);

        // Sidebar with buttons
        let sb_w = 120i32;
        r.fill_rect(Rect::new(cx, cy, sb_w as u32, ch), p.surface);
        r.stroke_rect(Rect::new(cx, cy, sb_w as u32, ch), p.border);

        // Buttons
        let btn_scan = Rect::new(cx + 10, cy + 10, 100, 24);
        r.fill_rect(btn_scan, p.accent);
        r.draw_text("SCAN", btn_scan.x + 30, btn_scan.y + 8, p.text_primary);

        let btn_disc = Rect::new(cx + 10, cy + 44, 100, 24);
        r.fill_rect(btn_disc, p.accent);
        r.draw_text("DISCONNECT", btn_disc.x + 10, btn_disc.y + 8, p.text_primary);

        let btn_type_c = Rect::new(cx + 10, cy + 78, 100, 24);
        r.fill_rect(btn_type_c, p.accent);
        r.draw_text("SET COMPUTE", btn_type_c.x + 5, btn_type_c.y + 8, p.text_primary);

        let btn_type_s = Rect::new(cx + 10, cy + 112, 100, 24);
        r.fill_rect(btn_type_s, p.accent);
        r.draw_text("SET STORAGE", btn_type_s.x + 5, btn_type_s.y + 8, p.text_primary);

        let btn_type_d = Rect::new(cx + 10, cy + 146, 100, 24);
        r.fill_rect(btn_type_d, p.accent);
        r.draw_text("SET DELEGATE", btn_type_d.x + 2, btn_type_d.y + 8, p.text_primary);

        let btn_type_ctrl = Rect::new(cx + 10, cy + 180, 100, 24);
        r.fill_rect(btn_type_ctrl, p.accent);
        r.draw_text("REQUEST PROMO", btn_type_ctrl.x + 2, btn_type_ctrl.y + 8, p.text_primary);

        let btn_type_t = Rect::new(cx + 10, cy + 214, 100, 24);
        r.fill_rect(btn_type_t, p.accent);
        r.draw_text("SET TENANT", btn_type_t.x + 5, btn_type_t.y + 8, p.text_primary);

        let is_controller = crate::distributed::NODE_MESH.lock().local_type == crate::distributed::NodeType::Controller;
        let mode_str = if is_controller { "MODE: TARGET PEER" } else { "MODE: SELF ONLY" };
        r.draw_text(mode_str, cx + 10, cy + 244, p.text_secondary);

        // Main content area - Node Visualization
        let mc_x = cx + sb_w;
        let mc_y = cy;

        r.draw_text("VEIL-MESH NETWORK", mc_x + 10, mc_y + 10, p.text_primary);

        let mesh = crate::distributed::NODE_MESH.lock();
        
        // Draw Local Node
        let l_id = mesh.local_id.to_hex();
        let local_type_str = match mesh.local_type {
            crate::distributed::NodeType::Delegate => "Delegate",
            crate::distributed::NodeType::Compute  => "Compute",
            crate::distributed::NodeType::Offload  => "Offload",
            crate::distributed::NodeType::Storage  => "Storage",
            crate::distributed::NodeType::Controller => "Controller",
            crate::distributed::NodeType::Hub      => "Hub",
            crate::distributed::NodeType::Tenant   => "Tenant",
            crate::distributed::NodeType::Unknown  => "Unknown",
        };
        r.fill_rect(Rect::new(mc_x + 10, mc_y + 40, 150, 40), p.surface);
        r.stroke_rect(Rect::new(mc_x + 10, mc_y + 40, 150, 40), p.border);
        r.draw_text(&alloc::format!("LOCAL: {}", local_type_str), mc_x + 15, mc_y + 45, p.text_primary);
        r.draw_text(&l_id, mc_x + 15, mc_y + 60, p.text_secondary);

        // Draw Peers
        let mut py = mc_y + 100;
        for (id, peer) in mesh.peers.iter() {
            let pr = Rect::new(mc_x + 10, py, 200, 50);
            
            // Highlight selected
            if Some(*id) == self.selected_node {
                r.fill_rect(pr, p.selection);
            } else {
                r.fill_rect(pr, p.surface);
            }
            r.stroke_rect(pr, p.border);

            let type_str = match peer.node_type {
                crate::distributed::NodeType::Delegate => "Delegate",
                crate::distributed::NodeType::Compute  => "Compute",
                crate::distributed::NodeType::Offload  => "Offload",
                crate::distributed::NodeType::Storage  => "Storage",
                crate::distributed::NodeType::Controller => "Controller",
                crate::distributed::NodeType::Hub      => "Hub",
                crate::distributed::NodeType::Tenant   => "Tenant",
                crate::distributed::NodeType::Unknown  => "Unknown",
            };

            r.draw_text(type_str, pr.x + 5, pr.y + 5, p.text_primary);
            r.draw_text(&id.to_hex(), pr.x + 5, pr.y + 20, p.text_secondary);
            r.draw_text(&alloc::format!("CAPS: {:#010x}", peer.capabilities), pr.x + 5, pr.y + 35, p.text_secondary);
            
            py += 60;
        }
    }

    pub fn handle_click(&mut self, mx: i32, my: i32) {
        let wm = DESKTOP_WM.lock();
        let win = match wm.windows.iter().find(|w| w.id == self.window_id) { Some(w) => w, None => return };
        let cx = win.rect.x + 1;
        let cy = win.rect.y + 36;
        drop(wm);

        let btn_scan = Rect::new(cx + 10, cy + 10, 100, 24);
        let btn_disc = Rect::new(cx + 10, cy + 44, 100, 24);
        let btn_type_c = Rect::new(cx + 10, cy + 78, 100, 24);
        let btn_type_s = Rect::new(cx + 10, cy + 112, 100, 24);
        let btn_type_d = Rect::new(cx + 10, cy + 146, 100, 24);
        let btn_type_ctrl = Rect::new(cx + 10, cy + 180, 100, 24);
        let btn_type_t = Rect::new(cx + 10, cy + 214, 100, 24);

        if btn_scan.contains(mx, my) {
            let count = crate::distributed::NODE_MESH.lock().peer_count();
            crate::distributed::NODE_MESH.lock().search_cluster();
            notify(NotifCategory::Info, "MESH", &alloc::format!("Scanning... {} peers known", count));
            return;
        }

        if btn_disc.contains(mx, my) {
            if let Some(target) = self.selected_node {
                crate::distributed::NODE_MESH.lock().send_cluster_disconnect(&target);
                notify(NotifCategory::Info, "MESH", "Disconnect sent.");
            } else {
                notify(NotifCategory::Warning, "MESH", "No node selected.");
            }
            return;
        }

        // Role change buttons. Behavior depends on our own current role:
        //  - Controller: acts on the selected peer, via the existing
        //    request/confirm flow (the peer must accept).
        //  - Anyone else: acts on ourselves immediately (no confirmation
        //    needed — it's your own role), regardless of what's selected.
        //    A non-Controller can never target another node's role directly;
        //    the only lever it has over the Controller role specifically is
        //    REQUEST PROMOTION below.
        let is_controller = crate::distributed::NODE_MESH.lock().local_type == crate::distributed::NodeType::Controller;

        if btn_type_c.contains(mx, my) {
            if is_controller {
                if let Some(target) = self.selected_node {
                    crate::distributed::NODE_MESH.lock().send_cluster_role_request(&target, crate::distributed::NodeType::Compute);
                    notify(NotifCategory::Info, "MESH", "Sent role request: Compute");
                } else { notify(NotifCategory::Warning, "MESH", "No node selected."); }
            } else {
                crate::distributed::NODE_MESH.lock().set_local_type_immediate(crate::distributed::NodeType::Compute);
                notify(NotifCategory::Success, "MESH", "You are now Compute");
            }
            return;
        }
        if btn_type_s.contains(mx, my) {
            if is_controller {
                if let Some(target) = self.selected_node {
                    crate::distributed::NODE_MESH.lock().send_cluster_role_request(&target, crate::distributed::NodeType::Storage);
                    notify(NotifCategory::Info, "MESH", "Sent role request: Storage");
                } else { notify(NotifCategory::Warning, "MESH", "No node selected."); }
            } else {
                crate::distributed::NODE_MESH.lock().set_local_type_immediate(crate::distributed::NodeType::Storage);
                notify(NotifCategory::Success, "MESH", "You are now Storage");
            }
            return;
        }
        if btn_type_d.contains(mx, my) {
            if is_controller {
                if let Some(target) = self.selected_node {
                    crate::distributed::NODE_MESH.lock().send_cluster_role_request(&target, crate::distributed::NodeType::Delegate);
                    notify(NotifCategory::Info, "MESH", "Sent role request: Delegate");
                } else { notify(NotifCategory::Warning, "MESH", "No node selected."); }
            } else {
                crate::distributed::NODE_MESH.lock().set_local_type_immediate(crate::distributed::NodeType::Delegate);
                notify(NotifCategory::Success, "MESH", "You are now Delegate");
            }
            return;
        }

        // REQUEST PROMOTION — always self-directed. This is the only way a
        // non-Controller node can ever change who holds the Controller role;
        // there is deliberately no "SET CONTROLLER" that targets some other
        // selected peer, since that would let a controlled node dethrone the
        // controller in favor of a third node with nobody left in charge.
        if btn_type_ctrl.contains(mx, my) {
            crate::distributed::NODE_MESH.lock().send_promotion_request();
            return;
        }

        if btn_type_t.contains(mx, my) {
            if is_controller {
                if let Some(target) = self.selected_node {
                    crate::distributed::NODE_MESH.lock().send_cluster_role_request(&target, crate::distributed::NodeType::Tenant);
                    notify(NotifCategory::Info, "MESH", "Sent role request: Tenant");
                } else { notify(NotifCategory::Warning, "MESH", "No node selected."); }
            } else {
                crate::distributed::NODE_MESH.lock().set_local_type_immediate(crate::distributed::NodeType::Tenant);
                notify(NotifCategory::Success, "MESH", "You are now Tenant");
            }
            return;
        }

        // Selection logic
        let sb_w = 120;
        let mc_x = cx + sb_w;
        let mc_y = cy;
        
        let mesh = crate::distributed::NODE_MESH.lock();
        let mut py = mc_y + 100;
        for (id, _) in mesh.peers.iter() {
            let pr = Rect::new(mc_x + 10, py, 200, 50);
            if pr.contains(mx, my) {
                self.selected_node = Some(*id);
                return;
            }
            py += 60;
        }
    }
}

pub const MAX_MESH_MANAGERS: usize = 2;
pub static MESH_MANAGER_POOL: [Mutex<MeshManagerState>; MAX_MESH_MANAGERS] = [
    Mutex::new(MeshManagerState::new()),
    Mutex::new(MeshManagerState::new()),
];

pub fn open_mesh_manager(x: i32, y: i32, w: u32, h: u32, z: u32) -> (u32, usize) {
    let id = DESKTOP_WM.lock().open("MESH MANAGER", x, y, w, h, z);
    let live_ids: alloc::vec::Vec<u32> = {
        let wm = DESKTOP_WM.lock();
        wm.windows.iter().map(|win| win.id).collect()
    };
    for (idx, slot) in MESH_MANAGER_POOL.iter().enumerate() {
        if let Some(mut app) = slot.try_lock() {
            if app.window_id == 0 || !live_ids.contains(&app.window_id) {
                *app = MeshManagerState::new();
                app.window_id = id;
                return (id, idx);
            }
        }
    }
    let mut app = MESH_MANAGER_POOL[0].lock();
    *app = MeshManagerState::new();
    app.window_id = id;
    (id, 0)
}

pub fn mesh_manager_slot_for_window(id: u32) -> Option<usize> {
    for (idx, slot) in MESH_MANAGER_POOL.iter().enumerate() {
        if let Some(app) = slot.try_lock() {
            if app.window_id == id { return Some(idx); }
        }
    }
    None
}

/// Initialise all native applications and open the launcher.
pub fn init() {
    // NOTE: /config.vsh is NOT loaded here because apps::init() runs before
    // the AHCI disk is probed and the VFS has no mounts yet.
    // The config is loaded lazily on the first Desktop phase tick in
    // gui_refresh_task() (main.rs) once the filesystem is confirmed live.
    println!("[APPS] Native applications initialised.");
}

/// Load and execute /config.vsh, then fire the 'start' event.
/// Called exactly once from gui_refresh_task() after the VFS is mounted.
pub fn load_config_vsh() {
    let config_bytes = crate::vfs::VFS.read_file("/config.vsh");
    match config_bytes {
        Ok(bytes) => {
            match core::str::from_utf8(&bytes) {
                Ok(script) => {
                    let mut eng = crate::vscript::GLOBAL_ENGINE.lock();
                    let errs = eng.execute_named("/config.vsh", script);
                    for e in &errs {
                        crate::serial_println!("[VSCRIPT] {}", e);
                    }
                    let errs2 = eng.trigger_event("start", None);
                    for e in &errs2 {
                        crate::serial_println!("[VSCRIPT] start: {}", e);
                    }
                    crate::serial_println!("[VSCRIPT] /config.vsh loaded and 'start' event fired.");
                    crate::println!("[VSCRIPT] Desktop configured from /config.vsh");
                }
                Err(e) => crate::serial_println!("[VSCRIPT] /config.vsh is not valid UTF-8: {}", e),
            }
        }
        Err(_) => {
            crate::serial_println!("[VSCRIPT] /config.vsh not found — using defaults.");
        }
    }
}

/// A console attribute's foreground and background, in the colours Windows'
/// terminal shows (its Campbell scheme). COMMON_LVB_REVERSE_VIDEO swaps them.
fn console_colours(attr: u16) -> (Color, Color) {
    const P: [(u8, u8, u8); 16] = [
        (12, 12, 12), (0, 55, 218), (19, 161, 14), (58, 150, 221), (197, 15, 31), (136, 23, 152), (193, 156, 0), (204, 204, 204),
        (118, 118, 118), (59, 120, 255), (22, 198, 12), (97, 214, 214), (231, 72, 86), (180, 0, 158), (249, 241, 165), (242, 242, 242),
    ];
    let c = |i: u16| { let (r, g, b) = P[(i & 0xF) as usize]; Color { r, g, b } };
    let (fg, bg) = (c(attr), c(attr >> 4));
    if attr & 0x4000 != 0 { (bg, fg) } else { (fg, bg) }
}
