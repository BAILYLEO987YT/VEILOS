pub use crate::graphics::{FRAMEBUFFER, Color};
use spin::Mutex;

// -----| screen dimensions |-----
// These match the resolution set by the display driver (graphics::probe_display).
// Update both here and there if you change the resolution.
/// Current screen size in pixels. Set from the display mode by the display
/// driver (`set_screen_size`) before the back buffer is allocated; 1024×768
/// until then. These were compile-time constants, which tied every window,
/// clip and buffer to one resolution (HAL step 7D).
static SCREEN_W_RT: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(1024);
static SCREEN_H_RT: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(768);

/// Screen width in pixels.
#[inline]
pub fn screen_w() -> usize {
    SCREEN_W_RT.load(core::sync::atomic::Ordering::Relaxed)
}

/// Screen height in pixels.
#[inline]
pub fn screen_h() -> usize {
    SCREEN_H_RT.load(core::sync::atomic::Ordering::Relaxed)
}

/// Adopt a new screen size: called by the display driver once the mode is
/// set, before `init_back_buffer`. Also resets the clip to the whole screen.
pub fn set_screen_size(w: usize, h: usize) {
    SCREEN_W_RT.store(w, core::sync::atomic::Ordering::Relaxed);
    SCREEN_H_RT.store(h, core::sync::atomic::Ordering::Relaxed);
    clip_reset();
}

// -----| primitive drawing |-----

pub fn draw_rect(x: usize, y: usize, w: usize, h: usize, color: Color) {
    let mut fb_lock = FRAMEBUFFER.lock();
    if let Some(ref mut fb) = *fb_lock {
        let pixel_val = (color.b as u32) | ((color.g as u32) << 8) | ((color.r as u32) << 16);
        
        for dy in 0..h {
            let py = y + dy;
            if py >= fb.height { break; }
            
            // Optimization: Get a direct pointer to the start of the row segment
            let row_start = (fb.base_addr + (py * fb.stride + x) as u64 * 4) as *mut u32;
            
            // Determine how many pixels we can fit in this row within bounds
            let draw_w = if x + w > fb.width { fb.width.saturating_sub(x) } else { w };
            
            unsafe {
                // High-performance slice fill (unrolled by compiler)
                let row_slice = core::slice::from_raw_parts_mut(row_start, draw_w);
                row_slice.fill(pixel_val);
            }
        }
    }
}

// -----| bitmap font (Standard 8x8 MSB-First) |-----
//
// Complete printable-ASCII font covering chars 32 (' ') through 127 (DEL).
// 96 glyphs * 8 bytes each = 768 bytes total.
// Bit 7 of each byte = leftmost pixel of that row (MSB-first / standard convention).
// Lowercase letters map to their uppercase equivalents in get_font_offset().
const FONT: [u8; 768] = [
    0x00,0x00,0x00,0x00,0x00,0x00,0x00,0x00, // ' '  (0x20)
    0x18,0x18,0x18,0x18,0x18,0x00,0x18,0x00, // '!'  (0x21)
    0x6C,0x6C,0x6C,0x00,0x00,0x00,0x00,0x00, // '"'  (0x22)
    0x6C,0x6C,0xFE,0x6C,0xFE,0x6C,0x6C,0x00, // '#'  (0x23)
    0x18,0x7E,0xC0,0x7E,0x06,0x7E,0x18,0x00, // '$'  (0x24)
    0x00,0xC6,0xCC,0x18,0x30,0x66,0xC6,0x00, // '%'  (0x25)
    0x38,0x6C,0x38,0x76,0xDC,0xCC,0x76,0x00, // '&'  (0x26)
    0x30,0x30,0x60,0x00,0x00,0x00,0x00,0x00, // '''  (0x27)
    0x18,0x30,0x60,0x60,0x60,0x30,0x18,0x00, // '('  (0x28)
    0x60,0x30,0x18,0x18,0x18,0x30,0x60,0x00, // ')'  (0x29)
    0x00,0x66,0x3C,0xFF,0x3C,0x66,0x00,0x00, // '*'  (0x2A)
    0x00,0x18,0x18,0x7E,0x18,0x18,0x00,0x00, // '+'  (0x2B)
    0x00,0x00,0x00,0x00,0x00,0x18,0x18,0x30, // ','  (0x2C)
    0x00,0x00,0x00,0x7E,0x00,0x00,0x00,0x00, // '-'  (0x2D)
    0x00,0x00,0x00,0x00,0x00,0x18,0x18,0x00, // '.'  (0x2E)
    0x00,0x06,0x0C,0x18,0x30,0x60,0x00,0x00, // '/'  (0x2F)
    0x3C,0x66,0xCE,0xD6,0xE6,0x66,0x3C,0x00, // '0'  (0x30)
    0x18,0x38,0x18,0x18,0x18,0x18,0x7E,0x00, // '1'  (0x31)
    0x3C,0x66,0x06,0x1C,0x30,0x60,0x7E,0x00, // '2'  (0x32)
    0x3C,0x66,0x06,0x1C,0x06,0x66,0x3C,0x00, // '3'  (0x33)
    0x0C,0x1C,0x3C,0x6C,0x7E,0x0C,0x0C,0x00, // '4'  (0x34)
    0x7E,0x60,0x7C,0x06,0x06,0x66,0x3C,0x00, // '5'  (0x35)
    0x3C,0x60,0x7C,0x66,0x66,0x66,0x3C,0x00, // '6'  (0x36)
    0x7E,0x06,0x0C,0x18,0x30,0x30,0x30,0x00, // '7'  (0x37)
    0x3C,0x66,0x66,0x3C,0x66,0x66,0x3C,0x00, // '8'  (0x38)
    0x3C,0x66,0x66,0x66,0x3E,0x06,0x3C,0x00, // '9'  (0x39)
    0x00,0x18,0x18,0x00,0x00,0x18,0x18,0x00, // ':'  (0x3A)
    0x00,0x18,0x18,0x00,0x00,0x18,0x18,0x30, // ';'  (0x3B)
    0x00,0x18,0x30,0x60,0x30,0x18,0x00,0x00, // '<'  (0x3C)
    0x00,0x00,0x7E,0x00,0x7E,0x00,0x00,0x00, // '='  (0x3D)
    0x00,0x18,0x0C,0x06,0x0C,0x18,0x00,0x00, // '>'  (0x3E)
    0x3C,0x66,0x06,0x0C,0x18,0x00,0x18,0x00, // '?'  (0x3F)
    0x3C,0x66,0x6E,0x6E,0x60,0x3E,0x00,0x00, // '@'  (0x40)
    0x18,0x3C,0x66,0x7E,0x66,0x66,0x66,0x00, // 'A'  (0x41)
    0x7C,0x66,0x66,0x7C,0x66,0x66,0x7C,0x00, // 'B'  (0x42)
    0x3C,0x66,0x60,0x60,0x60,0x66,0x3C,0x00, // 'C'  (0x43)
    0x78,0x6C,0x66,0x66,0x66,0x6C,0x78,0x00, // 'D'  (0x44)
    0x7E,0x60,0x60,0x7C,0x60,0x60,0x7E,0x00, // 'E'  (0x45)
    0x7E,0x60,0x60,0x7C,0x60,0x60,0x60,0x00, // 'F'  (0x46)
    0x3C,0x66,0x60,0x6E,0x66,0x66,0x3C,0x00, // 'G'  (0x47)
    0x66,0x66,0x66,0x7E,0x66,0x66,0x66,0x00, // 'H'  (0x48)
    0x3C,0x18,0x18,0x18,0x18,0x18,0x3C,0x00, // 'I'  (0x49)
    0x0E,0x06,0x06,0x06,0x06,0x66,0x3C,0x00, // 'J'  (0x4A)
    0x66,0x6C,0x78,0x70,0x78,0x6C,0x66,0x00, // 'K'  (0x4B)
    0x60,0x60,0x60,0x60,0x60,0x60,0x7E,0x00, // 'L'  (0x4C)
    0xC3,0xE7,0xFF,0xFF,0xDB,0xC3,0xC3,0x00, // 'M'  (0x4D)
    0xC3,0xE3,0xF3,0xDB,0xCF,0xC7,0xC3,0x00, // 'N'  (0x4E)
    0x3C,0x66,0x66,0x66,0x66,0x66,0x3C,0x00, // 'O'  (0x4F)
    0x7C,0x66,0x66,0x7C,0x60,0x60,0x60,0x00, // 'P'  (0x50)
    0x3C,0x66,0x66,0x66,0x6E,0x3C,0x0E,0x00, // 'Q'  (0x51)
    0x7C,0x66,0x66,0x7C,0x6C,0x66,0x66,0x00, // 'R'  (0x52)
    0x3C,0x66,0x60,0x3C,0x06,0x66,0x3C,0x00, // 'S'  (0x53)
    0x7E,0x18,0x18,0x18,0x18,0x18,0x18,0x00, // 'T'  (0x54)
    0x66,0x66,0x66,0x66,0x66,0x66,0x3C,0x00, // 'U'  (0x55)
    0x66,0x66,0x66,0x66,0x66,0x3C,0x18,0x00, // 'V'  (0x56)
    0xC3,0xC3,0xDB,0xFF,0xFF,0xE7,0xC3,0x00, // 'W'  (0x57)
    0xC3,0x66,0x3C,0x18,0x3C,0x66,0xC3,0x00, // 'X'  (0x58)
    0x66,0x66,0x66,0x3C,0x18,0x18,0x18,0x00, // 'Y'  (0x59)
    0x7E,0x06,0x0C,0x18,0x30,0x60,0x7E,0x00, // 'Z'  (0x5A)
    0x3C,0x30,0x30,0x30,0x30,0x30,0x3C,0x00, // '['  (0x5B)
    0x00,0x60,0x30,0x18,0x0C,0x06,0x00,0x00, // '\\' (0x5C)
    0x3C,0x0C,0x0C,0x0C,0x0C,0x0C,0x3C,0x00, // ']'  (0x5D)
    0x18,0x3C,0x66,0x00,0x00,0x00,0x00,0x00, // '^'  (0x5E)
    0x00,0x00,0x00,0x00,0x00,0x00,0xFF,0x00, // '_'  (0x5F)
    0x30,0x18,0x00,0x00,0x00,0x00,0x00,0x00, // '`'  (0x60)
    0x18,0x3C,0x66,0x7E,0x66,0x66,0x66,0x00, // 'a' => A
    0x7C,0x66,0x66,0x7C,0x66,0x66,0x7C,0x00, // 'b' => B
    0x3C,0x66,0x60,0x60,0x60,0x66,0x3C,0x00, // 'c' => C
    0x78,0x6C,0x66,0x66,0x66,0x6C,0x78,0x00, // 'd' => D
    0x7E,0x60,0x60,0x7C,0x60,0x60,0x7E,0x00, // 'e' => E
    0x7E,0x60,0x60,0x7C,0x60,0x60,0x60,0x00, // 'f' => F
    0x3C,0x66,0x60,0x6E,0x66,0x66,0x3C,0x00, // 'g' => G
    0x66,0x66,0x66,0x7E,0x66,0x66,0x66,0x00, // 'h' => H
    0x3C,0x18,0x18,0x18,0x18,0x18,0x3C,0x00, // 'i' => I
    0x0E,0x06,0x06,0x06,0x06,0x66,0x3C,0x00, // 'j' => J
    0x66,0x6C,0x78,0x70,0x78,0x6C,0x66,0x00, // 'k' => K
    0x60,0x60,0x60,0x60,0x60,0x60,0x7E,0x00, // 'l' => L
    0xC3,0xE7,0xFF,0xFF,0xDB,0xC3,0xC3,0x00, // 'm' => M
    0xC3,0xE3,0xF3,0xDB,0xCF,0xC7,0xC3,0x00, // 'n' => N
    0x3C,0x66,0x66,0x66,0x66,0x66,0x3C,0x00, // 'o' => O
    0x7C,0x66,0x66,0x7C,0x60,0x60,0x60,0x00, // 'p' => P
    0x3C,0x66,0x66,0x66,0x6E,0x3C,0x0E,0x00, // 'q' => Q
    0x7C,0x66,0x66,0x7C,0x6C,0x66,0x66,0x00, // 'r' => R
    0x3C,0x66,0x60,0x3C,0x06,0x66,0x3C,0x00, // 's' => S
    0x7E,0x18,0x18,0x18,0x18,0x18,0x18,0x00, // 't' => T
    0x66,0x66,0x66,0x66,0x66,0x66,0x3C,0x00, // 'u' => U
    0x66,0x66,0x66,0x66,0x66,0x3C,0x18,0x00, // 'v' => V
    0xC3,0xC3,0xDB,0xFF,0xFF,0xE7,0xC3,0x00, // 'w' => W
    0xC3,0x66,0x3C,0x18,0x3C,0x66,0xC3,0x00, // 'x' => X
    0x66,0x66,0x66,0x3C,0x18,0x18,0x18,0x00, // 'y' => Y
    0x7E,0x06,0x0C,0x18,0x30,0x60,0x7E,0x00, // 'z' => Z
    0x1C,0x30,0x30,0x60,0x30,0x30,0x1C,0x00, // '{'  (0x7B)
    0x18,0x18,0x18,0x00,0x18,0x18,0x18,0x00, // '|'  (0x7C)
    0x38,0x0C,0x0C,0x06,0x0C,0x0C,0x38,0x00, // '}'  (0x7D)
    0x34,0x58,0x00,0x00,0x00,0x00,0x00,0x00, // '~'  (0x7E)
    0x00,0x00,0x00,0x00,0x00,0x00,0x00,0x00, // DEL  (0x7F)
];

fn get_font_offset(c: char) -> usize {
    let c = if c.is_ascii_lowercase() { c.to_ascii_uppercase() } else { c };
    let code = c as usize;
    if code >= 32 && code <= 127 {
        (code - 32) * 8
    } else {
        0 // Unknown chars render as blank (space glyph at index 0)
    }
}

pub fn draw_char(c: char, x: usize, y: usize, color: Color) {
    let mut fb_lock = FRAMEBUFFER.lock();
    if let Some(ref mut fb) = *fb_lock {
        draw_char_raw(fb, c, x, y, color);
    }
}

pub fn draw_char_raw(fb: &mut crate::graphics::Framebuffer, c: char, x: usize, y: usize, color: Color) {
    let offset = get_font_offset(c);
    if offset + 7 >= FONT.len() { return; }
    
    for dy in 0..8 {
        let row = FONT[offset + dy];
        for dx in 0..8 {
            if (row & (0x80 >> dx)) != 0 {
                plot_pixel_raw(fb, x + dx, y + dy, color);
            }
        }
    }
}

pub fn draw_string(s: &str, x: usize, y: usize, color: Color) {
    let mut fb_lock = FRAMEBUFFER.lock();
    if let Some(ref mut fb) = *fb_lock {
        draw_string_raw(fb, s, x, y, color);
    }
}

pub fn draw_string_raw(fb: &mut crate::graphics::Framebuffer, s: &str, x: usize, y: usize, color: Color) {
    let mut offset = 0;
    for c in s.chars() {
        draw_char_raw(fb, c, x + offset, y, color);
        offset += 9;
    }
}

pub fn clear_rect(x: usize, y: usize, w: usize, h: usize) {
    draw_rect(x, y, w, h, Color { r: 20, g: 20, b: 35 });
}

// -----| cursor subsystem |-----

const CURSOR_SIZE: usize = 14;
static CURSOR_BACKING: Mutex<[Color; CURSOR_SIZE * CURSOR_SIZE]> = Mutex::new([Color { r: 0, g: 0, b: 0 }; CURSOR_SIZE * CURSOR_SIZE]);
// static CURSOR_LAST_POS removed

pub fn draw_cursor(x: usize, y: usize) {
    let white = Color { r: 255, g: 255, b: 255 };
    let black = Color { r: 0, g: 0, b: 0 };
    
    let mut fb_lock = FRAMEBUFFER.lock();
    if let Some(ref mut fb) = *fb_lock {
        // 1. First, save the backing of the NEW position before we draw the cursor there
        // Note: In a real system, you'd usually have a "BackingStore" struct
        let mut backing = CURSOR_BACKING.lock();
        for dy in 0..CURSOR_SIZE {
            for dx in 0..CURSOR_SIZE {
                let px = x + dx;
                let py = y + dy;
                if px < fb.width && py < fb.height {
                    let pixel_offset = py * fb.stride + px;
                    let p_ptr = (fb.base_addr + (pixel_offset as u64 * 4)) as *mut u32;
                    let val = unsafe { *p_ptr };
                    backing[dy * CURSOR_SIZE + dx] = Color {
                        b: (val & 0xFF) as u8,
                        g: ((val >> 8) & 0xFF) as u8,
                        r: ((val >> 16) & 0xFF) as u8,
                    };
                }
            }
        }

        // 2. Draw procedural cursor
        for dy in 0..12 {
            for dx in 0..12 {
                let px = x + dx;
                let py = y + dy;
                if px >= fb.width || py >= fb.height { continue; }

                let is_border = dx == 0 || dx == dy || (dy == 11 && dx < 11);
                let is_fill = dx > 0 && dx < dy && dy < 11;

                if is_border {
                    plot_pixel_raw(fb, px, py, black);
                } else if is_fill {
                    plot_pixel_raw(fb, px, py, white);
                }
            }
        }
    }
}

pub fn erase_cursor(x: usize, y: usize) {
    let mut fb_lock = FRAMEBUFFER.lock();
    if let Some(ref mut fb) = *fb_lock {
        let backing = CURSOR_BACKING.lock();
        
        // Restore the pixels exactly as they were captured in the PREVIOUS draw_cursor call
        for dy in 0..CURSOR_SIZE {
            for dx in 0..CURSOR_SIZE {
                let px = x + dx;
                let py = y + dy;
                if px < fb.width && py < fb.height {
                    plot_pixel_raw(fb, px, py, backing[dy * CURSOR_SIZE + dx]);
                }
            }
        }
    }
}

// =====================================================================
// -----| Global Clip Rectangle |-----
// =====================================================================
//
// All back-buffer draw calls (draw_rect_atomic, draw_string_atomic) clip
// their output to this region.  Set it to a window's content area before
// painting app content, then clear it (set to full screen) afterwards so
// chrome drawing (title bars, panel, notifications) is never clipped.
//
// Uses four AtomicI32 values so no Mutex is needed — the renderer is
// always single-threaded (called from the single gui_refresh_task).

use core::sync::atomic::{AtomicBool, AtomicI32, AtomicPtr, Ordering as AtomicOrd};

pub static CLIP_X:  AtomicI32 = AtomicI32::new(0);
pub static CLIP_Y:  AtomicI32 = AtomicI32::new(0);
pub static CLIP_X2: AtomicI32 = AtomicI32::new(1024); // exclusive right edge
pub static CLIP_Y2: AtomicI32 = AtomicI32::new(768);  // exclusive bottom edge

/// Set the clip rectangle.  All subsequent draw_*_atomic calls will be
/// constrained to this region until clip_reset() is called.
pub fn clip_set(x: i32, y: i32, w: u32, h: u32) {
    CLIP_X .store(x,                    AtomicOrd::Relaxed);
    CLIP_Y .store(y,                    AtomicOrd::Relaxed);
    CLIP_X2.store(x + w as i32,         AtomicOrd::Relaxed);
    CLIP_Y2.store(y + h as i32,         AtomicOrd::Relaxed);
}

/// Reset the clip rectangle to the full screen (no clipping).
pub fn clip_reset() {
    CLIP_X .store(0,              AtomicOrd::Relaxed);
    CLIP_Y .store(0,              AtomicOrd::Relaxed);
    CLIP_X2.store(screen_w() as i32, AtomicOrd::Relaxed);
    CLIP_Y2.store(screen_h() as i32, AtomicOrd::Relaxed);
}

// -----| double buffering subsystem |-----

/// The 3.1 MB Off-Screen Memory Buffer for Atomic Rendering.
/// Using a Vec<u32> to match a 1024x768 BGRX screen.
pub static BACK_BUFFER: Mutex<Option<alloc::vec::Vec<u32>>> = Mutex::new(None);

// -----| BackBufferGuard — frame-level lock batching |-----
//
// The problem: draw_rect_atomic and draw_string_atomic each acquire and
// release BACK_BUFFER for every single draw call. A typical desktop frame
// (background + panel + several windows + notifications) makes 20-40 of
// these lock round-trips on the hot render path.
//
// The fix: BackBufferGuard holds BACK_BUFFER for the lifetime of an entire
// frame. While it is live, draw_rect_atomic, draw_string_atomic, and
// commit_dirty_rect all bypass the Mutex entirely by writing/reading through
// a raw pointer stored in PAINTER_PTR. No signature changes are needed at
// the call sites — all existing code (including desktop.rs) automatically
// gets the fast path while a guard is active.
//
// Safety: VEILOS is a single-core cooperative kernel. While BackBufferGuard
// is held by a task, no other task can run (cooperative scheduling). If the
// timer ISR fires it also takes the fast path through PAINTER_PTR, so it
// never tries to re-acquire the Mutex and cannot deadlock.

/// Set to true while a BackBufferGuard is live (or while the timer ISR is
/// painting). draw_rect_atomic / draw_string_atomic / commit_dirty_rect check
/// this to bypass the per-call Mutex acquisition.
pub static PAINTER_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Raw pointer into the Vec<u32> inside BACK_BUFFER.
/// Valid only while PAINTER_ACTIVE is true and the owning guard/ISR is live.
pub static PAINTER_PTR: AtomicPtr<u32> = AtomicPtr::new(core::ptr::null_mut());

/// RAII guard that holds the back-buffer Mutex for an entire frame.
///
/// Acquire with `BackBufferGuard::acquire()` before starting a frame's draw
/// calls. While the guard is live, every call to `draw_rect_atomic`,
/// `draw_string_atomic`, and `commit_dirty_rect`/`commit_back_buffer` skips
/// the per-call Mutex and writes directly through the guard's raw pointer.
/// Drop the guard (or let it go out of scope) to release the Mutex and
/// restore normal per-call locking.
pub struct BackBufferGuard {
    // The MutexGuard keeps BACK_BUFFER locked for the duration.
    // The field is never accessed directly — it exists solely to
    // release the lock when the guard is dropped.
    _guard: spin::MutexGuard<'static, Option<alloc::vec::Vec<u32>>>,
}

impl BackBufferGuard {
    /// Lock the back buffer for the current frame.
    /// Returns `None` if `init_back_buffer()` has not yet been called.
    pub fn acquire() -> Option<Self> {
        let guard = BACK_BUFFER.lock();
        let ptr = match &*guard {
            Some(bb) => bb.as_ptr() as *mut u32,
            None     => return None,
        };
        // Publish the raw pointer BEFORE setting the flag so that any
        // code observing PAINTER_ACTIVE = true always sees a valid pointer.
        PAINTER_PTR.store(ptr, AtomicOrd::Release);
        PAINTER_ACTIVE.store(true, AtomicOrd::Release);
        // Record this frame's drawing into the logical GPU's display list.
        crate::lgpu::dl_begin(crate::lgpu::Surface { ptr, width: screen_w(), height: screen_h(), stride: screen_w() });
        Some(Self { _guard: guard })
    }
}

impl Drop for BackBufferGuard {
    fn drop(&mut self) {
        // Render anything still recorded while the buffer is still ours.
        crate::lgpu::dl_end();
        // Clear the flag first so that after the MutexGuard releases the
        // lock, no code can observe PAINTER_ACTIVE = true with a stale ptr.
        PAINTER_ACTIVE.store(false, AtomicOrd::Release);
        PAINTER_PTR.store(core::ptr::null_mut(), AtomicOrd::Relaxed);
        // _guard drops here, releasing the spin::Mutex.
    }
}

pub fn init_back_buffer() {
    let mut bb = BACK_BUFFER.lock();
    let mut vec = alloc::vec::Vec::new();
    vec.reserve_exact(screen_w() * screen_h());
    unsafe {
        core::ptr::write_bytes(vec.as_mut_ptr(), 0, screen_w() * screen_h());
        vec.set_len(screen_w() * screen_h());
    }
    *bb = Some(vec);
}

/// Commits the ENTIRE off-screen buffer to the real hardware.
pub fn commit_back_buffer() {
    commit_dirty_rect(0, 0, screen_w(), screen_h());
}

/// The back buffer's raw pointer for code that writes pixels itself (image
/// decoders, `render2d`), after rendering everything the frame's display list
/// has recorded so far, so those writes land on top in the right order.
pub fn painter_ptr_flushed() -> *mut u32 {
    crate::lgpu::dl_flush();
    PAINTER_PTR.load(AtomicOrd::Relaxed)
}

/// The back buffer as a logical-GPU target, while a `BackBufferGuard` is held
/// (the painter fast path). None otherwise.
pub fn painter_surface() -> Option<crate::lgpu::Surface> {
    if !PAINTER_ACTIVE.load(AtomicOrd::Acquire) {
        return None;
    }
    let ptr = PAINTER_PTR.load(AtomicOrd::Relaxed);
    (!ptr.is_null()).then_some(crate::lgpu::Surface { ptr, width: screen_w(), height: screen_h(), stride: screen_w() })
}

/// Commits only a specific portion of the off-screen buffer to the real hardware.
/// The back buffer is the logical display; each head takes the part of the
/// rectangle that lies on it (graphics::HEADS).
pub fn commit_dirty_rect(x: usize, y: usize, w: usize, h: usize) {
    // Fast path: if a BackBufferGuard is active, read through the raw pointer
    // so we never try to re-acquire BACK_BUFFER (which would deadlock since
    // the guard already holds it).
    if PAINTER_ACTIVE.load(AtomicOrd::Acquire) {
        let bb_ptr = painter_ptr_flushed();
        if !bb_ptr.is_null() {
            commit_from(bb_ptr, x, y, w, h);
            return;
        }
    }

    // Normal path: hold the back buffer while copying from it.
    let mut bb_lock = BACK_BUFFER.lock();
    if let Some(bb) = bb_lock.as_mut() {
        commit_from(bb.as_mut_ptr(), x, y, w, h);
    }
}

/// Back buffer -> every head the rectangle touches, through the logical GPU:
/// split into bands and copied on every CPU (lgpu, HAL step 7C).
fn commit_from(bb_ptr: *mut u32, x: usize, y: usize, w: usize, h: usize) {
    let src = crate::lgpu::Surface { ptr: bb_ptr, width: screen_w(), height: screen_h(), stride: screen_w() };
    let (x1, y1) = ((x + w).min(screen_w()), (y + h).min(screen_h()));
    for head in crate::graphics::HEADS.lock().iter() {
        let fb = &head.fb;
        let (hx0, hx1) = (x.max(head.x), x1.min(head.x + fb.width));
        let hy1 = y1.min(fb.height);
        if hx0 >= hx1 || y >= hy1 {
            continue;
        }
        let target = crate::lgpu::Surface { ptr: fb.base_addr as *mut u32, width: fb.width, height: fb.height, stride: fb.stride };
        let dst = crate::lgpu::Rect::new(hx0 - head.x, y, hx1 - head.x, hy1);
        crate::lgpu::execute(&[crate::lgpu::Op::Copy { src, src_x: hx0, src_y: y, dst, transparent: None }], &target);
    }
}

/// Commits a dirty rect AND redraws the cursor on hardware if it overlaps the region.
///
/// App tasks should use this instead of bare `commit_dirty_rect` so that
/// updating a window's content never leaves a cursor-shaped hole.
pub fn commit_dirty_rect_and_cursor(x: usize, y: usize, w: usize, h: usize) {
    commit_dirty_rect(x, y, w, h);

    // Check whether the cursor overlaps the committed region and needs redrawing.
    let wm = WINDOW_MANAGER.lock();
    let cx = wm.cursor_x;
    let cy = wm.cursor_y;
    const CW: usize = 8;
    const CH: usize = 8;
    let overlaps = !(cx >= x + w || cx + CW <= x || cy >= y + h || cy + CH <= y);
    if overlaps {
        wm.draw_cursor_hw();
    }
}

/// Raw draw that targets the BACKBUFFER instead of the hardware.
pub fn draw_rect_atomic(x: usize, y: usize, w: usize, h: usize, color: Color) {
    // Apply the global clip rectangle.
    let cx  = CLIP_X .load(AtomicOrd::Relaxed).max(0) as usize;
    let cy  = CLIP_Y .load(AtomicOrd::Relaxed).max(0) as usize;
    let cx2 = CLIP_X2.load(AtomicOrd::Relaxed).max(0) as usize;
    let cy2 = CLIP_Y2.load(AtomicOrd::Relaxed).max(0) as usize;

    // Intersect the requested rect with the clip.
    let rx  = x.max(cx);
    let ry  = y.max(cy);
    let rx2 = (x + w).min(cx2).min(screen_w());
    let ry2 = (y + h).min(cy2).min(screen_h());
    if rx >= rx2 || ry >= ry2 { return; }
    let dw = rx2 - rx;
    let dh = ry2 - ry;

    let pixel_val = (color.b as u32) | ((color.g as u32) << 8) | ((color.r as u32) << 16);

    // Fast path: if a BackBufferGuard is active, write directly through its
    // raw pointer — no Mutex acquisition needed.
    if PAINTER_ACTIVE.load(AtomicOrd::Acquire) {
        let bb_ptr = PAINTER_PTR.load(AtomicOrd::Relaxed);
        if !bb_ptr.is_null() {
            // Through the logical GPU (HAL step 7C): large fills (window and
            // panel backgrounds) are banded over every CPU, small ones run here.
            // Each fill completes before this returns, so drawing order is kept.
            let target = crate::lgpu::Surface { ptr: bb_ptr, width: screen_w(), height: screen_h(), stride: screen_w() };
            let op = crate::lgpu::Op::Fill { rect: crate::lgpu::Rect::new(rx, ry, rx + dw, ry + dh), color: pixel_val };
            if !crate::lgpu::dl_record(op) {
                crate::lgpu::execute(&[op], &target);
            }
            return;
        }
    }

    // Normal path: per-call Mutex lock.
    let mut bb_lock = BACK_BUFFER.lock();
    if let Some(ref mut bb) = *bb_lock {
        for dy in 0..dh {
            let py = ry + dy;
            let start = py * screen_w() + rx;
            bb[start..start + dw].fill(pixel_val);
        }
    }
}

/// Utility for drawing directly to a framebuffer without locking (Internal)
fn plot_pixel_raw(fb: &mut crate::graphics::Framebuffer, x: usize, y: usize, color: Color) {
    let pixel_offset = y * fb.stride + x;
    let p_ptr = (fb.base_addr + (pixel_offset as u64 * 4)) as *mut u32;
    let pixel_val = (color.b as u32) | ((color.g as u32) << 8) | ((color.r as u32) << 16);
    unsafe { *p_ptr = pixel_val; }
}

/// Fills a rectangle directly on the hardware framebuffer (does NOT touch the back buffer).
/// Used exclusively for cursor rendering so the back buffer stays cursor-free.
/// `x`, `y` are logical-display coordinates; each head draws its part.
pub fn plot_pixels_hw(x: usize, y: usize, w: usize, h: usize, color: Color) {
    let pixel_val = (color.b as u32) | ((color.g as u32) << 8) | ((color.r as u32) << 16);
    for head in crate::graphics::HEADS.lock().iter() {
        let fb = &head.fb;
        let x0 = x.max(head.x);
        let x1 = (x + w).min(head.x + fb.width);
        if x0 >= x1 { continue; }
        for py in y..(y + h).min(fb.height) {
            let row_ptr = (fb.base_addr + (py * fb.stride + x0 - head.x) as u64 * 4) as *mut u32;
            unsafe {
                core::slice::from_raw_parts_mut(row_ptr, x1 - x0).fill(pixel_val);
            }
        }
    }
}

pub fn draw_string_atomic(s: &str, x: usize, y: usize, color: Color) {
    // Clip check: skip the whole string if the baseline is outside the clip.
    let cy  = CLIP_Y .load(AtomicOrd::Relaxed).max(0) as usize;
    let cy2 = CLIP_Y2.load(AtomicOrd::Relaxed).max(0) as usize;
    let cx  = CLIP_X .load(AtomicOrd::Relaxed).max(0) as usize;
    let cx2 = CLIP_X2.load(AtomicOrd::Relaxed).max(0) as usize;
    if y + 8 <= cy || y >= cy2 { return; } // entirely above or below clip

    // Fast path: write directly through the guard's raw pointer.
    if PAINTER_ACTIVE.load(AtomicOrd::Acquire) {
        let bb_ptr = PAINTER_PTR.load(AtomicOrd::Relaxed);
        if !bb_ptr.is_null() {
            let pixel_val = (color.b as u32) | ((color.g as u32) << 8) | ((color.r as u32) << 16);
            let clip = crate::lgpu::Rect::new(cx, cy, cx2.min(screen_w()), cy2.min(screen_h()));
            let mut offset = 0;
            for c in s.chars() {
                let char_x = x + offset;
                if char_x + 8 > cx && char_x < cx2 {
                    // Recorded into the frame's display list (glyph rows are static font data).
                    let font_at = get_font_offset(c);
                    let recorded = font_at + 8 <= FONT.len() && crate::lgpu::dl_record(crate::lgpu::Op::Glyph8 {
                        bits: &FONT[font_at..font_at + 8], x: char_x, y, color: pixel_val, clip,
                    });
                    if !recorded {
                        unsafe { draw_char_raw_ptr(bb_ptr, c, char_x, y, color, cx, cy, cx2, cy2); }
                    }
                }
                offset += 9;
                if char_x >= cx2 { break; }
            }
            return;
        }
    }

    // Normal path: per-call Mutex lock.
    let mut bb_lock = BACK_BUFFER.lock();
    if let Some(ref mut bb) = *bb_lock {
        let mut offset = 0;
        for c in s.chars() {
            let char_x = x + offset;
            // Skip characters entirely outside horizontal clip.
            if char_x + 8 > cx && char_x < cx2 {
                draw_char_atomic_raw(bb, c, char_x, y, color, cx, cy, cx2, cy2);
            }
            offset += 9;
            // Stop once we've gone past the right edge of the clip.
            if char_x >= cx2 { break; }
        }
    }
}

/// Fast-path character renderer: writes directly to a `*mut u32` pixel buffer
/// without going through a Vec borrow. Used by the BackBufferGuard fast path
/// in draw_string_atomic.
///
/// # Safety
/// `bb_ptr` must point to a valid screen_w() × screen_h() pixel buffer. Caller
/// guarantees this by only calling this when PAINTER_PTR is non-null.
unsafe fn draw_char_raw_ptr(bb_ptr: *mut u32, c: char, x: usize, y: usize, color: Color,
                             cx: usize, cy: usize, cx2: usize, cy2: usize) {
    let offset = get_font_offset(c);
    if offset + 7 >= FONT.len() { return; }
    let pixel_val = (color.b as u32) | ((color.g as u32) << 8) | ((color.r as u32) << 16);
    for dy in 0..8 {
        let py = y + dy;
        if py < cy || py >= cy2 || py >= screen_h() { continue; }
        let row_bits = FONT[offset + dy];
        for dx in 0..8 {
            if (row_bits & (0x80 >> dx)) != 0 {
                let px = x + dx;
                if px >= cx && px < cx2 && px < screen_w() {
                    unsafe { *bb_ptr.add(py * screen_w() + px) = pixel_val; }
                }
            }
        }
    }
}

fn draw_char_atomic_raw(bb: &mut alloc::vec::Vec<u32>, c: char, x: usize, y: usize, color: Color,
                        cx: usize, cy: usize, cx2: usize, cy2: usize) {
    let offset = get_font_offset(c);
    if offset + 7 >= FONT.len() { return; }
    let pixel_val = (color.b as u32) | ((color.g as u32) << 8) | ((color.r as u32) << 16);

    for dy in 0..8 {
        let py = y + dy;
        if py < cy || py >= cy2 || py >= screen_h() { continue; }
        let row_bits = FONT[offset + dy];
        for dx in 0..8 {
            if (row_bits & (0x80 >> dx)) != 0 {
                let px = x + dx;
                if px >= cx && px < cx2 && px < screen_w() {
                    bb[py * screen_w() + px] = pixel_val;
                }
            }
        }
    }
}

/// Copy `cw` x `ch` pixels (0x00RRGGBB, `w` per row) to the back buffer at
/// (x, y), within the clip: a Windows program's window content (nt_user).
pub fn draw_pixels_atomic(pixels: &[u32], w: u32, h: u32, x: i32, y: i32, cw: u32, ch: u32) {
    let (w, cw, ch) = (w as usize, cw.min(w) as i32, ch.min(h) as i32);
    if pixels.len() < w * h as usize { return; }
    let x0 = x.max(CLIP_X.load(AtomicOrd::Relaxed).max(0));
    let y0 = y.max(CLIP_Y.load(AtomicOrd::Relaxed).max(0));
    let x1 = (x + cw).min(CLIP_X2.load(AtomicOrd::Relaxed)).min(screen_w() as i32);
    let y1 = (y + ch).min(CLIP_Y2.load(AtomicOrd::Relaxed)).min(screen_h() as i32);
    if x0 >= x1 || y0 >= y1 { return; }
    let (sx, n) = ((x0 - x) as usize, (x1 - x0) as usize);
    let direct = PAINTER_ACTIVE.load(AtomicOrd::Acquire);
    let bb_ptr = if direct { painter_ptr_flushed() } else { core::ptr::null_mut() };
    let mut bb_lock = if !direct { Some(BACK_BUFFER.lock()) } else { None };
    let mut fallback = bb_lock.as_mut().and_then(|g| (**g).as_mut());
    for py in y0..y1 {
        let src = &pixels[(py - y) as usize * w + sx..][..n];
        let at = py as usize * screen_w() + x0 as usize;
        if !bb_ptr.is_null() {
            unsafe { core::ptr::copy_nonoverlapping(src.as_ptr(), bb_ptr.add(at), n) };
        } else if let Some(ref mut bb) = fallback {
            bb[at..at + n].copy_from_slice(src);
        }
    }
}

pub fn draw_bmp_atomic(raw: &[u8], x: i32, y: i32, target_w: Option<i32>, target_h: Option<i32>) {
    if raw.len() < 54 { return; }
    if raw[0] != b'B' || raw[1] != b'M' { return; }

    let offset = u32::from_le_bytes(raw[10..14].try_into().unwrap_or([0; 4])) as usize;
    let width = i32::from_le_bytes(raw[18..22].try_into().unwrap_or([0; 4]));
    let height = i32::from_le_bytes(raw[22..26].try_into().unwrap_or([0; 4]));
    let bpp = u16::from_le_bytes(raw[28..30].try_into().unwrap_or([0; 2]));
    let compress = u32::from_le_bytes(raw[30..34].try_into().unwrap_or([0; 4]));

    if compress != 0 || (bpp != 24 && bpp != 32) || width <= 0 { return; }

    let (tw, th) = if let (Some(tgt_w), Some(tgt_h)) = (target_w, target_h) {
        let mut final_w = tgt_w;
        let mut final_h = (tgt_w as i64 * height.abs() as i64 / width as i64) as i32;
        if final_h > tgt_h {
            final_h = tgt_h;
            final_w = (tgt_h as i64 * width as i64 / height.abs() as i64) as i32;
        }
        (final_w, final_h)
    } else {
        (width, height.abs())
    };

    if tw <= 0 || th <= 0 { return; }

    let cx = CLIP_X.load(AtomicOrd::Relaxed).max(0) as i32;
    let cy = CLIP_Y.load(AtomicOrd::Relaxed).max(0) as i32;
    let cx2 = CLIP_X2.load(AtomicOrd::Relaxed).max(0) as i32;
    let cy2 = CLIP_Y2.load(AtomicOrd::Relaxed).max(0) as i32;

    let row_stride = ((width * (bpp as i32) + 31) / 32) * 4;
    let h_abs = height.abs();
    let bottom_up = height > 0;

    let direct = PAINTER_ACTIVE.load(AtomicOrd::Acquire);
    let bb_ptr = if direct { painter_ptr_flushed() } else { core::ptr::null_mut() };

    let mut bb_lock_opt = if !direct { Some(BACK_BUFFER.lock()) } else { None };
    let mut fallback_bb = if let Some(ref mut guard) = bb_lock_opt { (**guard).as_mut() } else { None };

    for r in 0..th {
        let py = y + r;
        if py < cy || py >= cy2 || py >= screen_h() as i32 { continue; }

        let src_r = (r as i64 * h_abs as i64 / th as i64) as i32;
        let src_y = if bottom_up { h_abs - 1 - src_r } else { src_r };
        let row_offset = offset + (src_y as usize) * (row_stride as usize);
        if row_offset + (width as usize) * (bpp as usize / 8) > raw.len() { continue; }

        for c in 0..tw {
            let px = x + c;
            if px < cx || px >= cx2 || px >= screen_w() as i32 { continue; }

            let src_x = (c as i64 * width as i64 / tw as i64) as usize;
            let px_off = row_offset + src_x * (bpp as usize / 8);
            if px_off + 2 >= raw.len() { continue; }
            let b = raw[px_off];
            let g = raw[px_off + 1];
            let rd = raw[px_off + 2];
            let a = if bpp == 32 && px_off + 3 < raw.len() { raw[px_off + 3] } else { 255 };

            if a < 128 { continue; }

            let pixel_val = (b as u32) | ((g as u32) << 8) | ((rd as u32) << 16);

            if direct && !bb_ptr.is_null() {
                unsafe { *bb_ptr.add((py as usize) * screen_w() + (px as usize)) = pixel_val; }
            } else if let Some(ref mut vec) = fallback_bb {
                vec[(py as usize) * screen_w() + (px as usize)] = pixel_val;
            }
        }
    }
}

pub fn draw_tga_atomic(raw: &[u8], x: i32, y: i32, target_w: Option<i32>, target_h: Option<i32>) {
    if raw.len() < 18 { return; }

    let id_length = raw[0] as usize;
    let color_map_type = raw[1];
    let image_type = raw[2];
    
    if color_map_type != 0 || image_type != 2 { return; } // Only uncompressed true-color

    let width = u16::from_le_bytes([raw[12], raw[13]]) as i32;
    let height = u16::from_le_bytes([raw[14], raw[15]]) as i32;
    let bpp = raw[16];
    let descriptor = raw[17];

    if width <= 0 || height <= 0 || (bpp != 24 && bpp != 32) { return; }

    let (tw, th) = if let (Some(tgt_w), Some(tgt_h)) = (target_w, target_h) {
        let mut final_w = tgt_w;
        let mut final_h = (tgt_w as i64 * height as i64 / width as i64) as i32;
        if final_h > tgt_h {
            final_h = tgt_h;
            final_w = (tgt_h as i64 * width as i64 / height as i64) as i32;
        }
        (final_w, final_h)
    } else {
        (width, height)
    };

    if tw <= 0 || th <= 0 { return; }

    let top_down = (descriptor & 0x20) != 0;
    
    let offset = 18 + id_length;
    if offset >= raw.len() { return; }

    let bytes_per_pixel = (bpp / 8) as usize;
    let row_stride = (width as usize) * bytes_per_pixel;

    let cx = CLIP_X.load(AtomicOrd::Relaxed).max(0) as i32;
    let cy = CLIP_Y.load(AtomicOrd::Relaxed).max(0) as i32;
    let cx2 = CLIP_X2.load(AtomicOrd::Relaxed).max(0) as i32;
    let cy2 = CLIP_Y2.load(AtomicOrd::Relaxed).max(0) as i32;

    let direct = PAINTER_ACTIVE.load(AtomicOrd::Acquire);
    let bb_ptr = if direct { painter_ptr_flushed() } else { core::ptr::null_mut() };
    let mut bb_lock_opt = if !direct { Some(BACK_BUFFER.lock()) } else { None };
    let mut fallback_bb = if let Some(ref mut guard) = bb_lock_opt { (**guard).as_mut() } else { None };

    for r in 0..th {
        let py = y + r;
        if py < cy || py >= cy2 || py >= screen_h() as i32 { continue; }

        let src_r = (r as i64 * height as i64 / th as i64) as i32;
        let src_y = if top_down { src_r } else { height - 1 - src_r };
        let row_offset = offset + (src_y as usize) * row_stride;
        if row_offset + row_stride > raw.len() { continue; }

        for c in 0..tw {
            let px = x + c;
            if px < cx || px >= cx2 || px >= screen_w() as i32 { continue; }

            let src_x = (c as i64 * width as i64 / tw as i64) as usize;
            let px_off = row_offset + src_x * bytes_per_pixel;
            if px_off + 2 >= raw.len() { continue; }
            
            // TGA is also typically BGR or BGRA
            let b = raw[px_off];
            let g = raw[px_off + 1];
            let rd = raw[px_off + 2];
            let a = if bpp == 32 && px_off + 3 < raw.len() { raw[px_off + 3] } else { 255 };

            if a < 128 { continue; } // threshold alpha

            let pixel_val = (b as u32) | ((g as u32) << 8) | ((rd as u32) << 16);

            if direct && !bb_ptr.is_null() {
                unsafe { *bb_ptr.add((py as usize) * screen_w() + (px as usize)) = pixel_val; }
            } else if let Some(ref mut vec) = fallback_bb {
                vec[(py as usize) * screen_w() + (px as usize)] = pixel_val;
            }
        }
    }
}

// =====================================================================
// -----| Decoded RGB Pixel Buffer Blitter |-----
// =====================================================================
//
// Shared helper used by draw_jpeg_atomic and draw_png_atomic.
// Takes an already-decoded pixel buffer in RGB or RGBA format and
// blits it to the back buffer with clip awareness.

/// Blit a raw decoded pixel buffer (RGB8 or RGBA8, top-down, no padding) to the back buffer.
///  - `pixels`: the decoded pixel data, left-to-right, top-to-bottom
///  - `width`, `height`: dimensions of the decoded image
///  - `channels`: 3 for RGB, 4 for RGBA
///  - `x`, `y`: destination position on screen
pub fn draw_decoded_rgb(pixels: &[u8], width: i32, height: i32, channels: usize, x: i32, y: i32, target_w: Option<i32>, target_h: Option<i32>) {
    if width <= 0 || height <= 0 || channels < 3 { return; }

    let (tw, th) = if let (Some(tgt_w), Some(tgt_h)) = (target_w, target_h) {
        let mut final_w = tgt_w;
        let mut final_h = (tgt_w as i64 * height as i64 / width as i64) as i32;
        if final_h > tgt_h {
            final_h = tgt_h;
            final_w = (tgt_h as i64 * width as i64 / height as i64) as i32;
        }
        (final_w, final_h)
    } else {
        (width, height)
    };

    draw_decoded_rgb_ext(pixels, width, height, channels, x, y, tw, th);
}

// -----| cached pre-scaled images (wallpaper fast path) |-----
//
// `draw_decoded_rgb_ext` used to resample the source image to the target size on EVERY call:
// a 64-bit divide per pixel plus bounds and overflow checks. The desktop repaints the wallpaper
// on every full frame, so a debug build under emulation spent most of each frame right here.
// Large draws are now resampled ONCE into a pre-scaled u32 copy, and later frames just copy rows.
// The original per-pixel loop below is kept as the fallback (small images, cache busy, no memory).
// An image is only cached the SECOND time the same draw is seen: a draw that is never repeated
// (e.g. one animated-GIF frame) would otherwise pay for the resample AND the copy.

/// Pixel value in a cached image meaning "transparent: leave the back buffer alone".
/// Real pixels are BGRX with a zero top byte, so this can never collide with one.
const SCALED_TRANSPARENT: u32 = 0xFFFF_FFFF;

/// Only draws of at least this many target pixels are cached. Small images are cheap anyway,
/// and caching them could let unrelated images evict the wallpaper.
const SCALE_CACHE_MIN_PIXELS: usize = 256 * 256;
/// Never cache more than this many target pixels (16 MiB of u32).
const SCALE_CACHE_MAX_PIXELS: usize = 4 * 1024 * 1024;
/// Cached images kept; the least recently used one is replaced. (Keep in sync with the
/// `[None, None]` initialiser of `SCALE_CACHE`.)
const SCALE_CACHE_SLOTS: usize = 2;

#[derive(Clone, Copy, PartialEq, Eq)]
struct ScaleKey {
    ptr: usize,
    len: usize,
    width: i32,
    height: i32,
    channels: usize,
    tw: i32,
    th: i32,
    /// FNV-1a over ~256 bytes sampled across the source. Guards against a *different* image
    /// being decoded into the same allocation (same address and size) after the old one is freed.
    sample: u64,
}

struct ScaledImage {
    key: ScaleKey,
    /// Higher = more recently used.
    stamp: u64,
    /// True when no pixel is transparent, so whole rows can be block-copied.
    opaque: bool,
    /// tw * th pixels, row-major.
    px: alloc::vec::Vec<u32>,
}

struct ScaleCache {
    slots: [Option<ScaledImage>; SCALE_CACHE_SLOTS],
    clock: u64,
    /// Keys seen once but not yet cached (see the policy note above). Two entries, round-robin.
    seen: [Option<ScaleKey>; 2],
    seen_next: usize,
}

static SCALE_CACHE: Mutex<ScaleCache> =
    Mutex::new(ScaleCache { slots: [None, None], clock: 0, seen: [None, None], seen_next: 0 });

fn scale_key(pixels: &[u8], width: i32, height: i32, channels: usize, tw: i32, th: i32) -> ScaleKey {
    let n = pixels.len();
    let step = (n / 256).max(1);
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut i = 0usize;
    while i < n {
        h = (h ^ pixels[i] as u64).wrapping_mul(0x0000_0100_0000_01b3);
        i += step;
    }
    ScaleKey { ptr: pixels.as_ptr() as usize, len: n, width, height, channels, tw, th, sample: h }
}

/// Resample `pixels` to `tw` x `th` exactly like the per-pixel path of `draw_decoded_rgb_ext`
/// (nearest neighbour, alpha < 128 skipped). Returns `(pixels, fully_opaque)`, or `None` if the
/// target is too large or memory is short.
fn build_scaled(pixels: &[u8], width: i32, height: i32, channels: usize, tw: i32, th: i32)
    -> Option<(alloc::vec::Vec<u32>, bool)>
{
    let tw_u = tw as usize;
    let th_u = th as usize;
    let total = tw_u.checked_mul(th_u)?;
    if total > SCALE_CACHE_MAX_PIXELS { return None; }

    let mut px: alloc::vec::Vec<u32> = alloc::vec::Vec::new();
    px.try_reserve_exact(total).ok()?;

    // Source column for each target column: one divide per column instead of one per pixel.
    let mut xmap: alloc::vec::Vec<usize> = alloc::vec::Vec::new();
    xmap.try_reserve_exact(tw_u).ok()?;
    for c in 0..tw_u {
        xmap.push((c as i64 * width as i64 / tw as i64) as usize);
    }

    let row_bytes = (width as usize) * channels;
    let mut opaque = true;
    for r in 0..th_u {
        let src_y = (r as i64 * height as i64 / th as i64) as usize;
        let row_offset = src_y * row_bytes;
        // A source row that runs past the buffer ends the image (the original loop `break`s):
        // this row and every later one stay transparent.
        if row_offset + row_bytes > pixels.len() {
            for _ in r..th_u {
                for _ in 0..tw_u { px.push(SCALED_TRANSPARENT); }
            }
            opaque = false;
            break;
        }
        for c in 0..tw_u {
            let off = row_offset + xmap[c] * channels;
            if off + 2 >= pixels.len() { px.push(SCALED_TRANSPARENT); opaque = false; continue; }
            let a = if channels >= 4 && off + 3 < pixels.len() { pixels[off + 3] } else { 255 };
            if a < 128 { px.push(SCALED_TRANSPARENT); opaque = false; continue; }
            // Framebuffer is BGRX.
            px.push((pixels[off + 2] as u32) | ((pixels[off + 1] as u32) << 8) | ((pixels[off] as u32) << 16));
        }
    }
    Some((px, opaque))
}

/// Copy the visible part of a cached image into `dst` (a screen_w() x screen_h() back buffer),
/// honouring the clip rectangle exactly like the per-pixel path.
fn blit_scaled(img: &ScaledImage, x: i32, y: i32, tw: i32, th: i32,
               cx: i32, cy: i32, cx2: i32, cy2: i32, dst: &mut [u32]) {
    let left  = cx.max(x).max(0);
    let right = cx2.min(x + tw).min(screen_w() as i32);
    if left >= right { return; }
    let c0 = (left - x) as usize;
    let n  = (right - left) as usize;
    for r in 0..th {
        let py = y + r;
        if py < cy || py >= cy2 || py >= screen_h() as i32 { continue; }
        let s0 = (r as usize) * (tw as usize) + c0;
        let src = &img.px[s0..s0 + n];
        let d0 = (py as usize) * screen_w() + left as usize;
        let d = &mut dst[d0..d0 + n];
        if img.opaque {
            d.copy_from_slice(src);
        } else {
            for (dp, &sp) in d.iter_mut().zip(src.iter()) {
                if sp != SCALED_TRANSPARENT { *dp = sp; }
            }
        }
    }
}

/// Fast path for `draw_decoded_rgb_ext`. Returns `true` if the draw was fully handled.
fn draw_scaled_cached(pixels: &[u8], width: i32, height: i32, channels: usize,
                      x: i32, y: i32, tw: i32, th: i32,
                      cx: i32, cy: i32, cx2: i32, cy2: i32) -> bool {
    let total = (tw as usize).saturating_mul(th as usize);
    if total < SCALE_CACHE_MIN_PIXELS || total > SCALE_CACHE_MAX_PIXELS { return false; }

    // Never block: this can be reached from interrupt context while the interrupted code
    // holds the cache lock. If it is busy, the caller falls back to the per-pixel path.
    let mut cache = match SCALE_CACHE.try_lock() {
        Some(c) => c,
        None => return false,
    };

    let key = scale_key(pixels, width, height, channels, tw, th);
    cache.clock += 1;
    let stamp = cache.clock;

    let mut found: Option<usize> = None;
    for i in 0..SCALE_CACHE_SLOTS {
        if let Some(img) = &cache.slots[i] {
            if img.key == key { found = Some(i); break; }
        }
    }

    let idx = match found {
        Some(i) => i,
        None => {
            // First sighting: remember it and let the per-pixel path draw this one.
            let mut seen_before = false;
            for s in cache.seen.iter() {
                if *s == Some(key) { seen_before = true; }
            }
            if !seen_before {
                let n = cache.seen_next;
                cache.seen[n] = Some(key);
                cache.seen_next = (n + 1) % 2;
                return false;
            }
            // A recorded copy may still point into the slot about to be replaced.
            crate::lgpu::dl_flush();
            let (px, opaque) = match build_scaled(pixels, width, height, channels, tw, th) {
                Some(v) => v,
                None => return false,
            };
            // Use an empty slot, otherwise evict the least recently used one.
            let mut victim = 0usize;
            let mut oldest = u64::MAX;
            for i in 0..SCALE_CACHE_SLOTS {
                match &cache.slots[i] {
                    None => { victim = i; break; }
                    Some(img) => { if img.stamp < oldest { oldest = img.stamp; victim = i; } }
                }
            }
            cache.slots[victim] = Some(ScaledImage { key, stamp, opaque, px });
            victim
        }
    };

    if let Some(img) = cache.slots[idx].as_mut() { img.stamp = stamp; }
    let img = match cache.slots[idx].as_ref() {
        Some(i) => i,
        None => return false,
    };

    // Same destination selection as the per-pixel path.
    let direct = PAINTER_ACTIVE.load(AtomicOrd::Acquire);
    if direct {
        let Some(target) = painter_surface() else { return false };
        // Through the logical GPU: banded over every CPU (lgpu, HAL step 7C).
        // Same clipping as blit_scaled: the draw rect, the clip, the screen.
        let x0 = cx.max(x).max(0);
        let x1 = cx2.min(x + tw).min(screen_w() as i32);
        let y0 = cy.max(y).max(0);
        let y1 = cy2.min(y + th).min(screen_h() as i32);
        if x0 < x1 && y0 < y1 {
            let src = crate::lgpu::Surface {
                ptr: img.px.as_ptr() as *mut u32, width: tw as usize, height: th as usize, stride: tw as usize,
            };
            let op = crate::lgpu::Op::Copy {
                src,
                src_x: (x0 - x) as usize,
                src_y: (y0 - y) as usize,
                dst: crate::lgpu::Rect::new(x0 as usize, y0 as usize, x1 as usize, y1 as usize),
                transparent: if img.opaque { None } else { Some(SCALED_TRANSPARENT) },
            };
            if !crate::lgpu::dl_record(op) {
                crate::lgpu::execute(&[op], &target);
            }
        }
        return true;
    }
    let mut bb_lock = BACK_BUFFER.lock();
    if let Some(ref mut v) = *bb_lock {
        if v.len() >= screen_w() * screen_h() {
            blit_scaled(img, x, y, tw, th, cx, cy, cx2, cy2, &mut v[..]);
            return true;
        }
    }
    false
}

pub fn draw_decoded_rgb_ext(pixels: &[u8], width: i32, height: i32, channels: usize, x: i32, y: i32, tw: i32, th: i32) {
    if width <= 0 || height <= 0 || channels < 3 || tw <= 0 || th <= 0 { return; }

    let cx  = CLIP_X .load(AtomicOrd::Relaxed).max(0) as i32;
    let cy  = CLIP_Y .load(AtomicOrd::Relaxed).max(0) as i32;
    let cx2 = CLIP_X2.load(AtomicOrd::Relaxed).max(0) as i32;
    let cy2 = CLIP_Y2.load(AtomicOrd::Relaxed).max(0) as i32;

    // Large draws that repeat (the wallpaper): blit from the cached pre-scaled copy instead of
    // re-resampling every pixel on every frame. Falls through to the per-pixel path below when
    // not applicable (first sighting, small image, cache busy, no memory).
    if draw_scaled_cached(pixels, width, height, channels, x, y, tw, th, cx, cy, cx2, cy2) {
        return;
    }

    let direct = PAINTER_ACTIVE.load(AtomicOrd::Acquire);
    let bb_ptr = if direct { painter_ptr_flushed() } else { core::ptr::null_mut() };
    let mut bb_lock_opt = if !direct { Some(BACK_BUFFER.lock()) } else { None };
    let mut fallback_bb = if let Some(ref mut guard) = bb_lock_opt { (**guard).as_mut() } else { None };

    let row_bytes = (width as usize) * channels;

    for r in 0..th {
        let py = y + r;
        if py < cy || py >= cy2 || py >= screen_h() as i32 { continue; }

        let src_y = (r as i64 * height as i64 / th as i64) as usize;
        let row_offset = src_y * row_bytes;
        if row_offset + row_bytes > pixels.len() { break; }

        for c in 0..tw {
            let px = x + c;
            if px < cx || px >= cx2 || px >= screen_w() as i32 { continue; }

            let src_x = (c as i64 * width as i64 / tw as i64) as usize;
            let off = row_offset + src_x * channels;
            if off + 2 >= pixels.len() { continue; }

            let rd = pixels[off];
            let g  = pixels[off + 1];
            let b  = pixels[off + 2];
            let a  = if channels >= 4 && off + 3 < pixels.len() { pixels[off + 3] } else { 255 };

            if a < 128 { continue; }

            // Framebuffer is BGRX
            let pixel_val = (b as u32) | ((g as u32) << 8) | ((rd as u32) << 16);

            if direct && !bb_ptr.is_null() {
                unsafe { *bb_ptr.add((py as usize) * screen_w() + (px as usize)) = pixel_val; }
            } else if let Some(ref mut vec) = fallback_bb {
                vec[(py as usize) * screen_w() + (px as usize)] = pixel_val;
            }
        }
    }
}

// -----| JPEG decoder (zune-jpeg, no_std) |-----

pub fn decode_jpeg(raw: &[u8]) -> Option<(i32, i32, usize, alloc::vec::Vec<u8>)> {
    use zune_jpeg::JpegDecoder;
    use zune_jpeg::zune_core::bytestream::ZCursor;

    let cursor = ZCursor::new(raw);
    let mut decoder = JpegDecoder::new(cursor);
    let pixels = decoder.decode().ok()?;
    let (w, h) = decoder.dimensions()?;
    
    // JPEG always decodes to RGB (3 channels) by default
    Some((w as i32, h as i32, 3, pixels))
}

/// Decode and blit a JPEG file to the back buffer.
pub fn draw_jpeg_atomic(raw: &[u8], x: i32, y: i32, target_w: Option<i32>, target_h: Option<i32>) {
    if let Some((w, h, channels, pixels)) = decode_jpeg(raw) {
        draw_decoded_rgb(&pixels, w, h, channels, x, y, target_w, target_h);
    }
}

// -----| PNG decoder (zune-png, no_std) |-----

pub fn decode_png(raw: &[u8]) -> Option<(i32, i32, usize, alloc::vec::Vec<u8>)> {
    use zune_png::PngDecoder;
    use zune_png::zune_core::bytestream::ZCursor;
    use zune_png::zune_core::colorspace::ColorSpace;

    let cursor = ZCursor::new(raw);
    let mut decoder = PngDecoder::new(cursor);
    let pixels = decoder.decode_raw().ok()?;
    let (w, h) = decoder.dimensions()?;
    let cs = decoder.colorspace()?;

    match cs {
        ColorSpace::RGB => Some((w as i32, h as i32, 3, pixels)),
        ColorSpace::RGBA => Some((w as i32, h as i32, 4, pixels)),
        ColorSpace::Luma => {
            let mut rgb = alloc::vec::Vec::with_capacity(pixels.len() * 3);
            for &v in &pixels { rgb.push(v); rgb.push(v); rgb.push(v); }
            Some((w as i32, h as i32, 3, rgb))
        }
        ColorSpace::LumaA => {
            let mut rgba = alloc::vec::Vec::with_capacity(pixels.len() * 2);
            for chunk in pixels.chunks(2) {
                if chunk.len() < 2 { break; }
                let v = chunk[0]; let a = chunk[1];
                rgba.push(v); rgba.push(v); rgba.push(v); rgba.push(a);
            }
            Some((w as i32, h as i32, 4, rgba))
        }
        _ => None,
    }
}

/// Decode and blit a PNG file to the back buffer.
pub fn draw_png_atomic(raw: &[u8], x: i32, y: i32, target_w: Option<i32>, target_h: Option<i32>) {
    if let Some((w, h, channels, pixels)) = decode_png(raw) {
        draw_decoded_rgb(&pixels, w, h, channels, x, y, target_w, target_h);
    }
}

// -----| GIF frame renderer |-----

/// Draw a single frame from a decoded GIF. The frame pixels are RGBA (already composited).
pub fn draw_gif_frame_atomic(frame: &crate::gif_decoder::GifFrame, x: i32, y: i32, target_w: Option<i32>, target_h: Option<i32>) {
    draw_decoded_rgb(&frame.pixels, frame.width as i32, frame.height as i32, 4, x, y, target_w, target_h);
}

pub fn draw_gif_frame_atomic_ext(frame: &crate::gif_decoder::GifFrame, x: i32, y: i32, tw: i32, th: i32) {
    draw_decoded_rgb_ext(&frame.pixels, frame.width as i32, frame.height as i32, 4, x, y, tw, th);
}

// -----| VMV (VEILOS Media Video) frame renderer |-----

/// VMV Format:
///   [4 bytes] Magic: "VMV\0"
///   [2 bytes] Width (u16 LE)
///   [2 bytes] Height (u16 LE)
///   [2 bytes] FPS (u16 LE)
///   [4 bytes] Frame count (u32 LE)
///   For each frame:
///     [4 bytes] Frame data length (u32 LE)
///     [N bytes] JPEG-encoded frame data

/// Parse a VMV header, returning (width, height, fps, frame_count, data_offset).
pub fn vmv_parse_header(raw: &[u8]) -> Option<(u16, u16, u16, u32, usize)> {
    if raw.len() < 14 { return None; }
    if &raw[0..4] != b"VMV\0" { return None; }
    let width = u16::from_le_bytes([raw[4], raw[5]]);
    let height = u16::from_le_bytes([raw[6], raw[7]]);
    let fps = u16::from_le_bytes([raw[8], raw[9]]);
    let frame_count = u32::from_le_bytes([raw[10], raw[11], raw[12], raw[13]]);
    Some((width, height, fps, frame_count, 14))
}

/// Get the byte offset and length of frame N in a VMV file.
pub fn vmv_frame_offset(raw: &[u8], frame_idx: u32) -> Option<(usize, usize)> {
    let (_, _, _, frame_count, mut pos) = vmv_parse_header(raw)?;
    if frame_idx >= frame_count { return None; }

    for _ in 0..frame_idx {
        if pos + 4 > raw.len() { return None; }
        let len = u32::from_le_bytes([raw[pos], raw[pos+1], raw[pos+2], raw[pos+3]]) as usize;
        pos += 4 + len;
    }

    if pos + 4 > raw.len() { return None; }
    let len = u32::from_le_bytes([raw[pos], raw[pos+1], raw[pos+2], raw[pos+3]]) as usize;
    pos += 4;
    if pos + len > raw.len() { return None; }
    Some((pos, len))
}

/// Decode and blit VMV frame N (which is internally JPEG-encoded).
pub fn draw_vmv_frame_atomic(raw: &[u8], frame_idx: u32, x: i32, y: i32, target_w: Option<i32>, target_h: Option<i32>) {
    if let Some((offset, len)) = vmv_frame_offset(raw, frame_idx) {
        draw_jpeg_atomic(&raw[offset..offset + len], x, y, target_w, target_h);
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct WindowState {
    pub id: usize,
    pub title: alloc::string::String,
    pub x: usize,
    pub y: usize,
    pub w: usize,
    pub h: usize,
    pub visible: bool,
    pub minimized: bool,
    pub backing: Option<alloc::vec::Vec<u32>>,
}

impl WindowState {
    /// Takes a current snapshot of the window's UI state (Chrome + Content)
    /// to allow for zero-cost rapid movement (blitting).
    pub fn update_snapshot(&mut self) {
        let size = self.w * self.h;
        if self.backing.is_none() || self.backing.as_ref().unwrap().len() != size {
            let mut vec = alloc::vec::Vec::new();
            vec.reserve_exact(size);
            unsafe {
                core::ptr::write_bytes(vec.as_mut_ptr(), 0, size);
                vec.set_len(size);
            }
            self.backing = Some(vec);
        }
        
        if let Some(ref mut bb) = self.backing {
            let border = Color { r: 80, g: 80, b: 120 };
            let title_bar = Color { r: 60, g: 60, b: 100 };
            let bg = Color { r: 30, g: 30, b: 45 };
            let text = Color { r: 255, g: 255, b: 255 };

            draw_rect_buffer(bb, self.w, self.h, 0, 0, self.w, self.h, border);
            draw_rect_buffer(bb, self.w, self.h, 2, 2, self.w - 4, 30, title_bar);
            draw_string_buffer(bb, self.w, self.h, &self.title, 10, 10, text);
            
            // Buttons
            draw_rect_buffer(bb, self.w, self.h, self.w - 30, 5, 22, 22, Color { r: 200, g: 50, b: 50 });
            draw_string_buffer(bb, self.w, self.h, "X", self.w - 23, 12, text);
            draw_rect_buffer(bb, self.w, self.h, self.w - 55, 5, 22, 22, Color { r: 50, g: 150, b: 50 });
            draw_string_buffer(bb, self.w, self.h, "+", self.w - 48, 12, text);
            draw_rect_buffer(bb, self.w, self.h, self.w - 80, 5, 22, 22, Color { r: 150, g: 150, b: 50 });
            draw_string_buffer(bb, self.w, self.h, "-", self.w - 73, 12, text);
            
            draw_rect_buffer(bb, self.w, self.h, 2, 32, self.w - 4, self.h - 34, bg);
        }
    }
}

pub struct WindowManager {
    pub windows: alloc::vec::Vec<WindowState>,
    pub dragging_id: Option<usize>,
    pub drag_offset_x: usize,
    pub drag_offset_y: usize,
    pub cursor_x: usize,
    pub cursor_y: usize,
}

impl WindowManager {
    pub const fn new() -> Self {
        Self {
            windows: alloc::vec::Vec::new(),
            dragging_id: None,
            drag_offset_x: 0,
            drag_offset_y: 0,
            cursor_x: 0,
            cursor_y: 0,
        }
    }

    pub fn register(&mut self, id: usize, title: &str, x: usize, y: usize, w: usize, h: usize) {
        let mut win = WindowState {
            id,
            title: alloc::string::String::from(title),
            x, y, w, h,
            visible: true,
            minimized: false,
            backing: None,
        };
        win.update_snapshot(); // Pre-draw the window cache
        self.windows.push(win);
    }

    pub fn get_window_mut(&mut self, id: usize) -> Option<&mut WindowState> {
        self.windows.iter_mut().find(|w| w.id == id)
    }

    pub fn redraw_all(&self) {
        // Compose scene into back buffer (NO cursor — back buffer is always cursor-free).
        self.draw_to_backbuffer_full();
        // Push back buffer to hardware, then overdraw cursor directly on hardware.
        commit_back_buffer();
        self.draw_cursor_hw();
    }

    /// Composite all visible windows into the back buffer without committing
    /// to hardware or drawing the cursor.  Called from gui_refresh_task as
    /// part of the single-composite-per-frame pipeline.
    pub fn draw_to_backbuffer(&self) {
        self.draw_to_backbuffer_full();
    }

    fn draw_to_backbuffer_full(&self) {
        // The cursor is deliberately NOT drawn here. The back buffer is the
        // clean scene; the cursor lives only on the hardware surface.
        draw_rect_atomic(0, 0, screen_w(), screen_h(), Color { r: 20, g: 20, b: 35 });
        self.draw_status_bar_atomic();
        for win in &self.windows {
            if win.visible && !win.minimized {
                blit_window(win);
            }
        }
    }

    pub fn redraw_move(&self, old_x: usize, old_y: usize, old_w: usize, old_h: usize, current_id: usize) {
        // Repaint the back buffer for the affected regions (no cursor involved).
        draw_rect_atomic(old_x, old_y, old_w, old_h, Color { r: 20, g: 20, b: 35 });
        if old_x + old_w >= 800 && old_y <= 50 {
            self.draw_status_bar_atomic();
        }
        for win in &self.windows {
            if win.id != current_id && win.visible && !win.minimized {
                if !(win.x >= old_x + old_w || win.x + win.w <= old_x || 
                     win.y >= old_y + old_h || win.y + win.h <= old_y) {
                    blit_window(win);
                }
            }
        }
        if let Some(win) = self.windows.iter().find(|w| w.id == current_id) {
            if win.visible && !win.minimized {
                blit_window(win);
            }
        }

        // Commit the dirty regions to hardware, then redraw cursor on top.
        commit_dirty_rect(old_x, old_y, old_w, old_h);
        if let Some(win) = self.windows.iter().find(|w| w.id == current_id) {
            commit_dirty_rect(win.x, win.y, win.w, win.h);
        }
        self.draw_cursor_hw();
    }

    fn draw_status_bar_atomic(&self) {
        let x = 800; let y = 10; let w = 210; let h = 40;
        let mode = crate::policy::POLICY.lock().get_mode();
        let mode_color = match mode {
            crate::policy::PolicyMode::Security => Color { r: 50, g: 200, b: 50 },
            crate::policy::PolicyMode::Performance => Color { r: 250, g: 50, b: 50 },
        };
        draw_rect_atomic(x, y, w, h, Color { r: 100, g: 100, b: 100 });
        draw_rect_atomic(x + 2, y + 2, w - 4, h - 4, Color { r: 0, g: 0, b: 0 });
        let label = match mode {
            crate::policy::PolicyMode::Security => "POLICY: SECURITY",
            crate::policy::PolicyMode::Performance => "POLICY: PERFORMANCE",
        };
        draw_string_atomic(label, x + 10, y + 12, mode_color);
    }

    /// Draws the cursor directly onto the hardware framebuffer (bypasses back buffer).
    ///
    /// The back buffer is always kept cursor-free. The cursor is the very last
    /// thing written to hardware after every commit, so it always sits on top
    /// of everything without corrupting the composited scene.
    pub fn draw_cursor_hw(&self) {
        let white = Color { r: 255, g: 255, b: 255 };
        let black = Color { r: 0,   g: 0,   b: 0   };
        plot_pixels_hw(self.cursor_x, self.cursor_y, 8, 8, white);
        plot_pixels_hw(self.cursor_x + 2, self.cursor_y + 2, 4, 4, black);
    }
}

pub fn blit_window(win: &WindowState) {
    let mut bb_lock = BACK_BUFFER.lock();
    if let (Some(bb), Some(src)) = (&mut *bb_lock, &win.backing) {
        for dy in 0..win.h {
            let py = win.y + dy;
            if py >= screen_h() { break; }
            let dst_start = py * screen_w() + win.x;
            let src_start = dy * win.w;
            let draw_w = if win.x + win.w > screen_w() { screen_w().saturating_sub(win.x) } else { win.w };
            unsafe {
                core::ptr::copy_nonoverlapping(
                    src.as_ptr().add(src_start),
                    bb.as_mut_ptr().add(dst_start),
                    draw_w,
                );
            }
        }
    }
}

fn draw_rect_buffer(buf: &mut [u32], bw: usize, bh: usize, x: usize, y: usize, w: usize, h: usize, color: Color) {
    let px_val = (color.b as u32) | ((color.g as u32) << 8) | ((color.r as u32) << 16);
    for dy in 0..h {
        let py = y + dy;
        if py >= bh { break; }
        let start = py * bw + x;
        let draw_w = if x + w > bw { bw.saturating_sub(x) } else { w };
        buf[start..start + draw_w].fill(px_val);
    }
}

fn draw_string_buffer(buf: &mut [u32], bw: usize, bh: usize, s: &str, x: usize, y: usize, color: Color) {
    // Pre-compute the packed pixel value once — it's the same for every pixel
    // in this string, so there is no reason to recompute it inside the inner loop.
    let px_val = (color.b as u32) | ((color.g as u32) << 8) | ((color.r as u32) << 16);
    let mut offset = 0;
    for c in s.chars() {
        let goff = get_font_offset(c);
        if goff + 7 >= FONT.len() { offset += 9; continue; }
        for dy in 0..8 {
            let row_bits = FONT[goff + dy];
            let py = y + dy;
            if py >= bh { continue; }
            for dx in 0..8 {
                if (row_bits & (0x80 >> dx)) != 0 {
                    let px = x + dx + offset;
                    if px < bw { buf[py * bw + px] = px_val; }
                }
            }
        }
        offset += 9;
    }
}

pub fn draw_rect_raw(fb: &mut crate::graphics::Framebuffer, x: usize, y: usize, w: usize, h: usize, color: Color) {
    let pixel_val = (color.b as u32) | ((color.g as u32) << 8) | ((color.r as u32) << 16);
    for dy in 0..h {
        let py = y + dy;
        if py >= fb.height { break; }
        let row_start = (fb.base_addr + (py * fb.stride + x) as u64 * 4) as *mut u32;
        let draw_w = if x + w > fb.width { fb.width.saturating_sub(x) } else { w };
        unsafe {
            core::slice::from_raw_parts_mut(row_start, draw_w).fill(pixel_val);
        }
    }
}

pub static WINDOW_MANAGER: Mutex<WindowManager> = Mutex::new(WindowManager::new());

// -----| focus / scheduling flags |-----
//
// One AtomicBool per window slot. The focus manager task is the ONLY writer.
// App tasks are readers — a single `load(Relaxed)` per loop iteration, no mutex.
//
// MAX_WINDOWS must be >= the number of windows you register.
pub const MAX_WINDOWS: usize = 8;

use core::sync::atomic::{Ordering};

/// Per-window "allowed to run" flags. Index == window id.
pub static WINDOW_ACTIVE: [AtomicBool; MAX_WINDOWS] = {
    const INIT: AtomicBool = AtomicBool::new(false);
    [INIT; MAX_WINDOWS]
};

/// Set to true by the WM whenever z-order, visibility, minimise state, or
/// policy mode changes. Cleared by the focus manager after it recomputes.
pub static FOCUS_DIRTY: AtomicBool = AtomicBool::new(false);

/// Mark that the focus manager needs to recompute who is active.
/// Call this from any WM operation that changes window or policy state.
pub fn mark_focus_dirty() {
    FOCUS_DIRTY.store(true, Ordering::Release);
}

/// O(1) check for app tasks — one atomic load, no mutex.
pub fn is_window_active(id: usize) -> bool {
    if id >= MAX_WINDOWS { return false; }
    WINDOW_ACTIVE[id].load(Ordering::Acquire)
}

/// Syncs a window's backing store from the current back buffer content.
///
/// The core bug this solves: app tasks draw directly into the global back buffer,
/// but `win.backing` only has the chrome from `update_snapshot()`. Any call to
/// `redraw_all()` rebuilds the scene from `blit_window()` which reads `win.backing`,
/// wiping all app-drawn content. Calling this at the end of every `redraw_*`
/// function keeps the backing in sync so `redraw_all()` restores the full scene.
pub fn sync_window_backing_from_bb(id: usize) {
    let (wx, wy, ww, wh) = {
        let wm = WINDOW_MANAGER.lock();
        match wm.windows.iter().find(|w| w.id == id) {
            Some(win) => (win.x, win.y, win.w, win.h),
            None => return,
        }
    }; // WM lock dropped here

    let mut tmp = alloc::vec::Vec::new();
    tmp.reserve_exact(ww * wh);
    unsafe {
        core::ptr::write_bytes(tmp.as_mut_ptr(), 0, ww * wh);
        tmp.set_len(ww * wh);
    }
    {
        let bb_lock = BACK_BUFFER.lock();
        if let Some(ref bb) = *bb_lock {
            for dy in 0..wh {
                let py = wy + dy;
                if py >= screen_h() { break; }
                let copy_w = (wx + ww).min(screen_w()).saturating_sub(wx);
                if copy_w == 0 { continue; }
                let src = py * screen_w() + wx;
                let dst = dy * ww;
                tmp[dst..dst + copy_w].copy_from_slice(&bb[src..src + copy_w]);
            }
        }
    } // BB lock dropped here

    let mut wm = WINDOW_MANAGER.lock();
    if let Some(win) = wm.get_window_mut(id) {
        win.backing = Some(tmp);
    }
}

/// Returns true if the window with the given id is currently the topmost
/// (focused) visible window. App tasks must call this before processing any
/// mouse click so that clicks on background windows are not acted on — the
/// WM handles raising the window to front, and the app sees the *next* click.
pub fn is_top_window(id: usize) -> bool {
    let wm = WINDOW_MANAGER.lock();
    wm.windows.iter().rev()
        .find(|w| w.visible && !w.minimized)
        .map(|w| w.id == id)
        .unwrap_or(false)
}

// -----| ui components |-----
//
// Every function with the `_atomic` suffix writes to the BACK BUFFER.
// Always call one of these from within async app tasks, then call
// `commit_dirty_rect(win_x, win_y, win_w, win_h)` once at the end.
//
// The non-`_atomic` variants write directly to the hardware framebuffer and
// are kept for the initial boot splash sequence only (before the WM starts).

/// Draws a full window chrome (border, title bar, buttons, content area bg)
/// into the BACK BUFFER. Apps must call this before drawing their own content.
pub fn draw_window_atomic(title: &str, x: usize, y: usize, w: usize, h: usize) {
    let bg       = Color { r: 30,  g: 30,  b: 45  };
    let border   = Color { r: 80,  g: 80,  b: 120 };
    let titlebar = Color { r: 60,  g: 60,  b: 100 };
    let text     = Color { r: 255, g: 255, b: 255 };

    draw_rect_atomic(x,       y,      w,     h,      border);
    draw_rect_atomic(x + 2,   y + 2,  w - 4, 30,     titlebar);
    draw_string_atomic(title, x + 10, y + 10, text);

    // Close [X]
    draw_rect_atomic(x + w - 30, y + 5, 22, 22, Color { r: 200, g: 50, b: 50 });
    draw_string_atomic("X", x + w - 23, y + 12, text);
    // Maximise [+]
    draw_rect_atomic(x + w - 55, y + 5, 22, 22, Color { r: 50, g: 150, b: 50 });
    draw_string_atomic("+", x + w - 48, y + 12, text);
    // Minimise [-]
    draw_rect_atomic(x + w - 80, y + 5, 22, 22, Color { r: 150, g: 150, b: 50 });
    draw_string_atomic("-", x + w - 73, y + 12, text);

    // Content area
    draw_rect_atomic(x + 2, y + 32, w - 4, h - 34, bg);
}

/// Draws a button into the BACK BUFFER.
pub fn draw_button_atomic(label: &str, x: usize, y: usize, w: usize, h: usize, pressed: bool) {
    let bg     = if pressed { Color { r: 100, g: 100, b: 150 } }
                 else       { Color { r: 60,  g: 60,  b: 90  } };
    let border = Color { r: 150, g: 150, b: 200 };
    let text   = Color { r: 255, g: 255, b: 255 };

    draw_rect_atomic(x,     y,     w,     h,     border);
    draw_rect_atomic(x + 1, y + 1, w - 2, h - 2, bg);
    draw_string_atomic(label, x + (w / 2).saturating_sub(label.len() * 4), y + (h / 2).saturating_sub(4), text);
}

// -----| hardware-direct primitives (boot/splash only) |-----

pub fn draw_window(title: &str, x: usize, y: usize, w: usize, h: usize) {
    let bg = Color { r: 30, g: 30, b: 45 };
    let border = Color { r: 80, g: 80, b: 120 };
    let title_bar = Color { r: 60, g: 60, b: 100 };
    let text = Color { r: 255, g: 255, b: 255 };

    // Window shadow/border
    draw_rect(x, y, w, h, border);
    // Title bar
    draw_rect(x + 2, y + 2, w - 4, 30, title_bar);
    draw_string(title, x + 10, y + 10, text);

    // Minimize [-], Maximize [+], Close [X] Buttons
    // Close [X]
    draw_rect(x + w - 30, y + 5, 22, 22, Color { r: 200, g: 50, b: 50 });
    draw_string("X", x + w - 23, y + 12, text);
    
    // Maximize [+]
    draw_rect(x + w - 55, y + 5, 22, 22, Color { r: 50, g: 150, b: 50 });
    draw_string("+", x + w - 48, y + 12, text);

    // Minimize [-]
    draw_rect(x + w - 80, y + 5, 22, 22, Color { r: 150, g: 150, b: 50 });
    draw_string("-", x + w - 73, y + 12, text);

    // Content area
    draw_rect(x + 2, y + 32, w - 4, h - 34, bg);
}

pub fn draw_button(label: &str, x: usize, y: usize, w: usize, h: usize, pressed: bool) {
    let bg = if pressed { Color { r: 100, g: 100, b: 150 } } else { Color { r: 60, g: 60, b: 90 } };
    let border = Color { r: 150, g: 150, b: 200 };
    let text = Color { r: 255, g: 255, b: 255 };

    draw_rect(x, y, w, h, border);
    draw_rect(x + 1, y + 1, w - 2, h - 2, bg);
    draw_string(label, x + (w / 2) - (label.len() * 4), y + (h / 2) - 4, text);
}

pub fn draw_status_bar() {
    // Top-right corner
    let x = 800;
    let y = 10;
    let w = 210;
    let h = 40;
    
    // Lock once — avoids a potential stale-read if the mode were to change
    // between two separate lock() calls.
    let mode = crate::policy::POLICY.lock().get_mode();

    let mode_color = match mode {
        crate::policy::PolicyMode::Security => Color { r: 50, g: 200, b: 50 },
        crate::policy::PolicyMode::Performance => Color { r: 250, g: 50, b: 50 },
    };

    draw_rect(x, y, w, h, Color { r: 100, g: 100, b: 100 });
    draw_rect(x + 2, y + 2, w - 4, h - 4, Color { r: 0, g: 0, b: 0 });
    
    let label = match mode {
        crate::policy::PolicyMode::Security => "POLICY: SECURITY",
        crate::policy::PolicyMode::Performance => "POLICY: PERFORMANCE",
    };
    draw_string(label, x + 10, y + 12, mode_color);
}
