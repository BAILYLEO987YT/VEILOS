// -----| render2d kernel glue |-----
//
// Connects the pure renderer (render2d.rs) to the kernel back buffer.
//
//   with_backbuffer(|canvas, cache| ...)  draw into the screen back buffer
//   draw_and_commit(|canvas, cache| ...)  same, then commit the touched area
//                                         (only for use OUTSIDE a frame; a
//                                         frame commits once at its end)
//   GFXTEST                               on-screen test card + cycle counts
//   load_fonts() / with_text(|c, glyphs, fonts| ...)   fonts from /FONTS, text drawing
//                                         (8.3 file names: the FAT driver has no LFN support)
//
// Back buffer access mirrors ui::draw_rect_atomic exactly: when a
// BackBufferGuard is active its raw pointer is used (no lock), otherwise the
// BACK_BUFFER mutex is taken. The global clip rect (CLIP_*) is honoured.

use core::sync::atomic::{AtomicU64, Ordering as AtomicOrd};
use spin::Mutex;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::font::{Font, FontLibrary, GlyphCache};
use crate::render2d::{Cache, Canvas, IRect};
use crate::ui::{BACK_BUFFER, CLIP_X, CLIP_X2, CLIP_Y, CLIP_Y2, PAINTER_ACTIVE, SCREEN_H, SCREEN_W};

/// Shared corner-mask and shadow-mask cache. Keeping it across frames is what
/// makes shadows cheap: a mask is built once, then blitted.
pub static GFX_CACHE: Mutex<Cache> = Mutex::new(Cache::new());

fn current_clip() -> IRect {
    let x = CLIP_X.load(AtomicOrd::Relaxed);
    let y = CLIP_Y.load(AtomicOrd::Relaxed);
    let x2 = CLIP_X2.load(AtomicOrd::Relaxed);
    let y2 = CLIP_Y2.load(AtomicOrd::Relaxed);
    IRect::new(x, y, (x2 - x).max(0), (y2 - y).max(0))
}

/// Run `f` with a canvas over the screen back buffer. Returns None if there is
/// no back buffer yet. Never blocks on the shared cache: if another caller
/// holds it, `f` gets a temporary empty cache (correct, just not cached).
pub fn with_backbuffer<R>(f: impl FnOnce(&mut Canvas, &mut Cache) -> R) -> Option<R> {
    let clip = current_clip();
    let mut shared = GFX_CACHE.try_lock();
    let mut temp = Cache::new();
    let cache: &mut Cache = match shared.as_mut() {
        Some(g) => &mut **g,
        None => &mut temp,
    };

    // Fast path: a BackBufferGuard is active, write through its pointer.
    if PAINTER_ACTIVE.load(AtomicOrd::Acquire) {
        // Renders the frame's recorded display list first (see ui::painter_ptr_flushed).
        let ptr = crate::ui::painter_ptr_flushed();
        if !ptr.is_null() {
            // SAFETY: the guard keeps SCREEN_W * SCREEN_H pixels alive at `ptr`
            // for the duration of the frame (same assumption as draw_rect_atomic).
            let px = unsafe { core::slice::from_raw_parts_mut(ptr, SCREEN_W * SCREEN_H) };
            let mut canvas = Canvas::new_opaque(px, SCREEN_W as i32, SCREEN_H as i32)?;
            canvas.set_clip(clip);
            // Recording is off while the canvas holds the raw buffer.
            return Some(crate::lgpu::suspended(|| f(&mut canvas, cache)));
        }
    }

    // Normal path: take the back buffer mutex for the duration of the call.
    let mut bb = BACK_BUFFER.lock();
    let buf = bb.as_mut()?;
    let mut canvas = Canvas::new_opaque(&mut buf[..], SCREEN_W as i32, SCREEN_H as i32)?;
    canvas.set_clip(clip);
    Some(f(&mut canvas, cache))
}

/// Draw, then commit exactly the area that was touched. Do NOT call this from
/// inside a desktop frame: frames commit once at the end (gui_refresh_task).
pub fn draw_and_commit(f: impl FnOnce(&mut Canvas, &mut Cache)) {
    let dirty = with_backbuffer(|c, cache| {
        f(c, cache);
        c.take_dirty()
    });
    if let Some(d) = dirty {
        if !d.is_empty() {
            crate::ui::commit_dirty_rect(d.x as usize, d.y as usize, d.w as usize, d.h as usize);
        }
    }
}

// =====================================================================
// -----| fonts |-----
// =====================================================================

/// Loaded faces. `pick("Century Gothic", 400, false)` etc. resolve through this.
pub static FONTS: Mutex<FontLibrary> = Mutex::new(FontLibrary::new());
/// Shared glyph bitmap cache.
pub static GLYPHS: Mutex<GlyphCache> = Mutex::new(GlyphCache::new());
/// Bumped whenever the font set changes, so cached text layers re-render.
pub static FONTS_GEN: AtomicU64 = AtomicU64::new(0);

/// Directory scanned for TrueType fonts. Use 8.3 file names (POPPINS.TTF,
/// GOTHIC.TTF): the FAT driver has no long-filename support.
pub const FONT_DIR: &str = "/FONTS";
const MAX_FONT_BYTES: u64 = 8 * 1024 * 1024;

/// (Re)load every `.ttf` in `FONT_DIR`. Replaces what was loaded before, so it
/// is safe to call again after copying new fonts onto the disk. Family, weight
/// and italic come from each font's own tables. Returns (loaded, failed) and
/// logs one serial line per file.
pub fn load_fonts() -> (usize, usize) {
    let entries = {
        let vfs = &crate::vfs::VFS;
        vfs.list_dir(FONT_DIR)
    };
    let entries = match entries {
        Ok(e) => e,
        Err(_) => {
            crate::serial_println!("[FONT] no {} directory: copy .TTF files into it (8.3 names)", FONT_DIR);
            return (0, 0);
        }
    };
    let (mut loaded, mut failed) = (0usize, 0usize);
    let mut faces: Vec<Font> = Vec::new();
    for e in entries {
        let upper: String = e.name.chars().map(|c| c.to_ascii_uppercase()).collect();
        if e.file_type != crate::vfs::FileType::File || !upper.ends_with(".TTF") {
            continue;
        }
        if e.size > MAX_FONT_BYTES {
            crate::serial_println!("[FONT] {}: skipped, {} bytes is larger than the {} byte limit", e.name, e.size, MAX_FONT_BYTES);
            failed += 1;
            continue;
        }
        let path = format!("{}/{}", FONT_DIR, e.name);
        let bytes = {
            let vfs = &crate::vfs::VFS;
            vfs.read_file(&path)
        };
        let bytes = match bytes {
            Ok(b) => b,
            Err(_) => {
                crate::serial_println!("[FONT] {}: could not be read", e.name);
                failed += 1;
                continue;
            }
        };
        match Font::parse(bytes) {
            Ok(f) => {
                crate::serial_println!("[FONT] {}: {} {} (weight {}{}), {} glyphs{}", e.name, f.family, f.subfamily, f.weight,
                    if f.italic { ", italic" } else { "" }, f.num_glyphs(), if f.has_kerning() { ", kerning" } else { "" });
                faces.push(f);
                loaded += 1;
            }
            Err(err) => {
                crate::serial_println!("[FONT] {}: {}", e.name, err.describe());
                failed += 1;
            }
        }
    }
    {
        let mut lib = FONTS.lock();
        lib.clear();
        for f in faces {
            lib.add(f);
        }
    }
    if let Some(mut g) = GLYPHS.try_lock() {
        g.clear();
    }
    FONTS_GEN.fetch_add(1, AtomicOrd::Relaxed);
    FONTS_TRIED.store(true, AtomicOrd::Relaxed);
    (loaded, failed)
}

/// Load the fonts once if nothing has tried yet (test card, shell start).
pub fn ensure_fonts() {
    if !FONTS_TRIED.load(AtomicOrd::Relaxed) {
        let (ok, bad) = load_fonts();
        crate::serial_println!("[GFX] fonts: {} loaded, {} failed", ok, bad);
    }
}

/// Run `f` with the glyph cache and font library. Never blocks: returns None if
/// the library is busy or empty; a busy glyph cache is replaced by a throwaway one.
pub fn with_text<R>(f: impl FnOnce(&mut Canvas, &mut GlyphCache, &FontLibrary) -> R) -> Option<R> {
    let lib = FONTS.try_lock()?;
    if lib.is_empty() {
        return None;
    }
    let mut shared = GLYPHS.try_lock();
    let mut temp = GlyphCache::new();
    let glyphs: &mut GlyphCache = match shared.as_mut() {
        Some(g) => &mut **g,
        None => &mut temp,
    };
    with_backbuffer(|c, _| f(c, glyphs, &lib))
}

// =====================================================================
// -----| GFXTEST: on-screen test card |-----
// =====================================================================

static TEST_CARD_UNTIL_MS: AtomicU64 = AtomicU64::new(0);
static TEST_CARD_FRAMES: AtomicU64 = AtomicU64::new(0);
static FONTS_TRIED: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// Show the renderer test card for `seconds` (1..=60). It is drawn at the end
/// of every desktop frame, so it appears on the next repaint (<= 0.5 s).
pub fn show_test_card(seconds: u64) {
    // Load fonts once, here (terminal context) rather than inside a frame.
    ensure_fonts();
    let now = crate::interrupts::TICKS.load(AtomicOrd::Relaxed);
    TEST_CARD_FRAMES.store(0, AtomicOrd::Relaxed);
    TEST_CARD_UNTIL_MS.store(now + seconds.clamp(1, 60) * 1000, AtomicOrd::Relaxed);
}

/// Hide the test card immediately (instead of waiting for it to time out).
pub fn hide_test_card() {
    TEST_CARD_UNTIL_MS.store(0, AtomicOrd::Relaxed);
}

pub fn test_card_active() -> bool {
    crate::interrupts::TICKS.load(AtomicOrd::Relaxed) < TEST_CARD_UNTIL_MS.load(AtomicOrd::Relaxed)
}

/// Call at the end of `paint_desktop`'s Desktop phase, before the frame commit.
pub fn paint_test_card_if_enabled() {
    if !test_card_active() {
        return;
    }
    let n = TEST_CARD_FRAMES.fetch_add(1, AtomicOrd::Relaxed);
    // SAFETY: rdtsc has no preconditions (same use as gui_refresh_task diagnostics).
    let rdtsc = || unsafe { core::arch::x86_64::_rdtsc() };

    // Per-band cycle counts of the uncached card (a deliberate stress test:
    // a full-screen gradient plus ~60 elements, drawn from scratch).
    let mut marks: [(&'static str, u64); 12] = [("", 0); 12];
    let mut count = 0usize;
    let t_start = rdtsc();
    let mut last = t_start;
    let drew = with_backbuffer(|c, cache| {
        crate::render2d::demo::draw_test_card_timed(c, cache, &mut |label| {
            let now = rdtsc();
            if count < marks.len() {
                marks[count] = (label, now.wrapping_sub(last));
                count += 1;
            }
            last = now;
        });
    })
    .is_some();
    let total = rdtsc().wrapping_sub(t_start);
    if n < 2 {
        crate::serial_println!("[GFX] test card frame {} (uncached, drawn from scratch, drew={}): {} Mcycles", n, drew, total / 1_000_000);
        for i in 0..count {
            crate::serial_println!("[GFX]   {:>6} Mcycles  {}", marks[i].1 / 1_000_000, marks[i].0);
        }
    }
    // Text overlay (needs fonts in /FONTS; skipped silently otherwise).
    let t_text = rdtsc();
    let overlay = with_text(|c, glyphs, lib| crate::font::demo::draw_text_overlay(c, glyphs, lib)).unwrap_or(false);
    let text_cycles = rdtsc().wrapping_sub(t_text);
    if n < 2 {
        if overlay {
            let (hits, misses) = GLYPHS.try_lock().map(|g| (g.hits, g.misses)).unwrap_or((0, 0));
            crate::serial_println!("[GFX] text overlay: {} Mcycles (glyph cache: {} hits, {} misses)", text_cycles / 1_000_000, hits, misses);
        } else {
            crate::serial_println!("[GFX] text overlay skipped: no fonts loaded (copy .TTF files to {})", FONT_DIR);
        }
    }
    if n == 0 {
        bench_cached_taskbar();
    }
}

/// The "draw once, redraw on dirty" comparison: build a taskbar-sized layer once,
/// then composite it. A retained UI pays `build` only when the element changes
/// and `composite` on every frame.
fn bench_cached_taskbar() {
    use crate::render2d::{demo, Layer};
    // SAFETY: rdtsc has no preconditions.
    let rdtsc = || unsafe { core::arch::x86_64::_rdtsc() };
    let (bx, by) = demo::TASKBAR_IN_LAYER;
    let mut layer = match Layer::new(demo::TASKBAR_LAYER_W, demo::TASKBAR_LAYER_H) {
        Some(l) => l,
        None => return,
    };
    let t0 = rdtsc();
    {
        let mut shared = GFX_CACHE.try_lock();
        let mut temp = Cache::new();
        let cache: &mut Cache = match shared.as_mut() {
            Some(g) => &mut **g,
            None => &mut temp,
        };
        let mut lc = Canvas::from_layer(&mut layer);
        demo::draw_taskbar(&mut lc, cache, bx, by);
    }
    let build = rdtsc().wrapping_sub(t0);
    let t1 = rdtsc();
    let _ = with_backbuffer(|c, _| {
        for _ in 0..10 {
            c.draw_layer(&layer, 232 - bx, 704 - by, 255);
        }
    });
    let composite = rdtsc().wrapping_sub(t1) / 10;
    crate::serial_println!(
        "[GFX] taskbar cached as a {}x{} layer: build {} Mcycles (only when it changes), composite {} Mcycles per frame",
        demo::TASKBAR_LAYER_W, demo::TASKBAR_LAYER_H, build / 1_000_000, composite / 1_000_000
    );
}
