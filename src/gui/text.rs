// =====================================================================
// -----| Text for the kernel's own screens (ACCOUNTS_DESIGN.md, section 10) |-----
// =====================================================================
//
// One draw / measure pair for the login and lock screens, the panel,
// notifications and the kernel's apps, in the TrueType fonts from /FONTS
// (gfx::render2d_kernel), instead of the 8x8 bitmap placeholder.
//
//   Face::Ui    the theme's font (THEME[font_family], THEME[font_size]):
//               proportional, so layouts measure text instead of counting
//               8 or 9 pixels a character.
//   Face::Mono  a monospaced face (Cascadia Mono when present) sized so one
//               character is exactly MONO_CELL_W pixels wide: the terminal
//               and the text editor are character grids.
//
// Text is placed so its capitals are centred where the 8-pixel bitmap glyphs
// were (on `y + 4`), so the existing layouts keep their vertical alignment.
//
// The bitmap font stays the fallback: before the fonts are loaded, when
// /FONTS has none, when the font library is busy, and for early boot and
// the panic screen (which call ui::draw_string_atomic directly).

use alloc::string::String;
use core::sync::atomic::{AtomicU32, Ordering};
use spin::Mutex;

use crate::gfx::font::{self, Font, TextStyle};
use crate::gfx::graphics::Color;
use crate::gfx::render2d_kernel::{with_text, FONTS};

/// Which face.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Face {
    Ui,
    Mono,
}

/// The width of a character cell on the grids (terminal, text editor), in
/// pixels: the bitmap font's pitch, kept so the grids' arithmetic holds
/// whichever font draws them.
pub const MONO_CELL_W: i32 = 9;

/// Width of a bitmap character (the fallback), in pixels.
const BITMAP_PITCH: i32 = 9;

/// The UI font: family ("default": the first face loaded) and size in px.
static UI_FAMILY: Mutex<String> = Mutex::new(String::new());
static UI_SIZE_PX: AtomicU32 = AtomicU32::new(13);

/// The theme's font (vsh/vscript.rs, `set THEME`). Sizes outside 9..=20 px
/// are clamped: the kernel's layouts are built around a small text size.
pub fn set_ui_font(family: &str, size_px: u32) {
    *UI_FAMILY.lock() = if family.eq_ignore_ascii_case("default") { String::new() } else { String::from(family) };
    UI_SIZE_PX.store(size_px.clamp(9, 20), Ordering::Relaxed);
}

fn ui_font<'a>(lib: &'a crate::gfx::font::FontLibrary) -> Option<&'a Font> {
    let fam = UI_FAMILY.lock().clone();
    if !fam.is_empty() {
        if let Some(f) = lib.pick(&fam, 400, false) {
            return Some(f);
        }
    }
    // The first upright regular face that isn't monospaced, else any.
    lib.faces().iter().find(|f| !is_mono(f) && !f.italic && f.weight <= 500)
        .or_else(|| lib.faces().first())
}

fn is_mono(f: &Font) -> bool {
    let fam = f.family.to_ascii_lowercase();
    if fam.contains("mono") || fam.contains("code") || fam.contains("courier") {
        return true;
    }
    // Same advance for a narrow and a wide letter.
    f.advance(f.glyph_index('i')) == f.advance(f.glyph_index('M')) && f.advance(f.glyph_index('M')) > 0
}

fn mono_font(lib: &crate::gfx::font::FontLibrary) -> Option<&Font> {
    lib.faces().iter().find(|f| is_mono(f) && !f.italic && f.weight <= 500)
        .or_else(|| lib.faces().iter().find(|f| is_mono(f)))
}

/// The face and style for `face`, or None (use the bitmap font).
fn pick<'a>(lib: &'a crate::gfx::font::FontLibrary, face: Face) -> Option<(&'a Font, TextStyle)> {
    match face {
        Face::Ui => ui_font(lib).map(|f| (f, TextStyle::px(UI_SIZE_PX.load(Ordering::Relaxed)))),
        Face::Mono => mono_font(lib).and_then(|f| {
            // The size whose advance is exactly one cell.
            let adv = f.advance(f.glyph_index('M')) as u32;
            if adv == 0 {
                return None;
            }
            let size_q6 = (MONO_CELL_W as u32 * 64 * f.units_per_em as u32) / adv;
            Some((f, TextStyle::q6(size_q6)))
        }),
    }
}

/// Draw `s` at (x, y), y being where the top of an 8-pixel bitmap line
/// would be. Returns its width in pixels.
pub fn draw(face: Face, s: &str, x: i32, y: i32, color: Color) -> i32 {
    if s.is_empty() {
        return 0;
    }
    let drawn = with_text(|canvas, glyphs, lib| {
        let (f, style) = pick(lib, face)?;
        let baseline = f.baseline_for_centre(y + 4, style.size_q6);
        let c = crate::gfx::render2d::Color32::rgb(color.r, color.g, color.b);
        let adv = font::draw_text(canvas, glyphs, f, s, x, baseline, c, 255, &style);
        Some((adv + 63) / 64)
    }).flatten();
    match drawn {
        Some(w) => w,
        None => {
            if x >= 0 && y >= 0 {
                crate::gui::ui::draw_string_atomic(s, x as usize, y as usize, color);
            }
            s.chars().count() as i32 * BITMAP_PITCH
        }
    }
}

/// The width `draw` would give `s`, in pixels.
pub fn measure(face: Face, s: &str) -> i32 {
    if face == Face::Mono {
        return s.chars().count() as i32 * MONO_CELL_W;
    }
    let measured = FONTS.try_lock().and_then(|lib| {
        let (f, style) = pick(&lib, face)?;
        Some((font::measure(f, s, &style) + 63) / 64)
    });
    measured.unwrap_or(s.chars().count() as i32 * BITMAP_PITCH)
}

/// `s` shortened with an ellipsis to fit in `max_w` pixels.
pub fn fit(face: Face, s: &str, max_w: i32) -> String {
    if measure(face, s) <= max_w {
        return String::from(s);
    }
    let fitted = FONTS.try_lock().and_then(|lib| {
        let (f, style) = pick(&lib, face)?;
        Some(font::fit_with_ellipsis(f, s, &style, max_w * 64))
    });
    fitted.unwrap_or_else(|| {
        let n = (max_w / BITMAP_PITCH).max(0) as usize;
        s.chars().take(n).collect()
    })
}

/// Draw `s` centred on `centre_x`.
pub fn draw_centred(face: Face, s: &str, centre_x: i32, y: i32, color: Color) {
    let w = measure(face, s);
    draw(face, s, centre_x - w / 2, y, color);
}
