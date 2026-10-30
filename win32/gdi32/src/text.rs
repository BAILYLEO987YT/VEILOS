//! Text in the built-in 8x16 font (font_data.rs, from Cascadia Mono), drawn
//! at a whole-number scale picked from the selected font's height. Every
//! font is this font until G2 brings TrueType.

use alloc::vec::Vec;

use crate::draw::pixel;
use crate::font_data::GLYPHS;
use crate::{Bool, Handle};

const CELL_W: i32 = 8;
const CELL_H: i32 = 16;
const ASCENT: i32 = 12;

const TA_UPDATECP: u32 = 1;
const TA_RIGHT: u32 = 2;
const TA_CENTER: u32 = 6;
const TA_BOTTOM: u32 = 8;
const TA_BASELINE: u32 = 24;

const ETO_OPAQUE: u32 = 2;
const ETO_CLIPPED: u32 = 4;

fn glyph(c: u16) -> &'static [u8; 16] {
    let i = match c {
        0x20..=0x7E => c as usize - 0x20,
        0xA0..=0xFF => c as usize - 0xA0 + 95,
        _ => '?' as usize - 0x20,
    };
    &GLYPHS[i]
}

/// Draw `text` with its cells' top-left at device (x, y); `advances`
/// (device pixels) if the caller gave its own spacing.
fn draw(hdc: Handle, x: i32, y: i32, text: &[u16], opts: u32, rect: Option<[i32; 4]>, advances: Option<&[i32]>) -> Bool {
    let mut t = crate::table();
    if !crate::is_dc(&mut t, hdc) {
        return 0;
    }
    let scale = crate::font_scale(&mut t, hdc);
    let (fg, bg, mode, align, cp) = crate::dc_text_state(&mut t, hdc).unwrap();
    let advance = |i: usize| advances.and_then(|a| a.get(i).copied()).unwrap_or(CELL_W * scale);
    let width: i32 = (0..text.len()).map(advance).sum();
    let height = CELL_H * scale;
    // The reference point: the current position, or (x, y).
    let (rx, ry) = if align & TA_UPDATECP != 0 { cp } else { (x, y) };
    let (dx, dy) = crate::to_device(&mut t, hdc, rx, ry);
    let left = match align & TA_CENTER {
        TA_CENTER => dx - width / 2,
        TA_RIGHT => dx - width,
        _ => dx,
    };
    let top = match align & TA_BASELINE {
        TA_BASELINE => dy - ASCENT * scale,
        TA_BOTTOM => dy - height,
        _ => dy,
    };
    if align & TA_UPDATECP != 0 {
        let nx = if align & TA_CENTER == TA_RIGHT { rx - width } else { rx + width };
        crate::set_pos(&mut t, hdc, (nx, ry));
    }
    let Some(mut c) = crate::canvas(&mut t, hdc) else { return 1 };
    let rect = rect.map(|r| {
        let (x0, y0) = crate::to_device(&mut t, hdc, r[0], r[1]);
        let (x1, y1) = crate::to_device(&mut t, hdc, r[2], r[3]);
        (x0, y0, x1, y1)
    });
    if let Some((x0, y0, x1, y1)) = rect {
        if opts & ETO_OPAQUE != 0 {
            c.fill(x0, y0, x1, y1, pixel(bg), crate::draw::R2_COPYPEN);
        }
        if opts & ETO_CLIPPED != 0 {
            c.x0 = c.x0.max(x0);
            c.y0 = c.y0.max(y0);
            c.x1 = c.x1.min(x1);
            c.y1 = c.y1.min(y1);
        }
    }
    let (fg, bg) = (pixel(fg), pixel(bg));
    let mut pen_x = left;
    for (i, &ch) in text.iter().enumerate() {
        let g = glyph(ch);
        let adv = advance(i);
        if mode == 2 {
            c.fill(pen_x, top, pen_x + adv, top + height, bg, crate::draw::R2_COPYPEN);
        }
        for (row, bits) in g.iter().enumerate() {
            if *bits == 0 {
                continue;
            }
            for col in 0..8 {
                if bits & (0x80 >> col) != 0 {
                    let (px, py) = (pen_x + col * scale, top + row as i32 * scale);
                    c.fill(px, py, px + scale, py + scale, fg, crate::draw::R2_COPYPEN);
                }
            }
        }
        pen_x += adv;
    }
    1
}

unsafe fn wide(s: *const u16, n: i32) -> Vec<u16> {
    if s.is_null() || n <= 0 {
        return Vec::new();
    }
    unsafe { core::slice::from_raw_parts(s, n as usize) }.to_vec()
}

/// ANSI text as Latin-1 (the code page is 1252 in name).
unsafe fn ansi(s: *const u8, n: i32) -> Vec<u16> {
    if s.is_null() || n <= 0 {
        return Vec::new();
    }
    unsafe { core::slice::from_raw_parts(s, n as usize) }.iter().map(|&b| b as u16).collect()
}

#[no_mangle]
pub unsafe extern "system" fn TextOutW(hdc: Handle, x: i32, y: i32, s: *const u16, n: i32) -> Bool {
    draw(hdc, x, y, &unsafe { wide(s, n) }, 0, None, None)
}

#[no_mangle]
pub unsafe extern "system" fn TextOutA(hdc: Handle, x: i32, y: i32, s: *const u8, n: i32) -> Bool {
    draw(hdc, x, y, &unsafe { ansi(s, n) }, 0, None, None)
}

unsafe fn ext(hdc: Handle, x: i32, y: i32, opts: u32, rect: *const [i32; 4], text: &[u16], dx: *const i32) -> Bool {
    let rect = (!rect.is_null()).then(|| unsafe { *rect });
    let advances = (!dx.is_null()).then(|| unsafe { core::slice::from_raw_parts(dx, text.len()) });
    draw(hdc, x, y, text, opts, rect, advances)
}

#[no_mangle]
pub unsafe extern "system" fn ExtTextOutW(hdc: Handle, x: i32, y: i32, opts: u32, rect: *const [i32; 4], s: *const u16, n: u32, dx: *const i32) -> Bool {
    unsafe { ext(hdc, x, y, opts, rect, &wide(s, n as i32), dx) }
}

#[no_mangle]
pub unsafe extern "system" fn ExtTextOutA(hdc: Handle, x: i32, y: i32, opts: u32, rect: *const [i32; 4], s: *const u8, n: u32, dx: *const i32) -> Bool {
    unsafe { ext(hdc, x, y, opts, rect, &ansi(s, n as i32), dx) }
}

fn extent(hdc: Handle, n: i32, out: *mut [i32; 2]) -> Bool {
    let mut t = crate::table();
    if !crate::is_dc(&mut t, hdc) || out.is_null() {
        return 0;
    }
    let scale = crate::font_scale(&mut t, hdc);
    // An empty string measures 0 x 0, as on Windows.
    let n = n.max(0);
    unsafe { *out = [n * CELL_W * scale, if n == 0 { 0 } else { CELL_H * scale }] };
    1
}

#[no_mangle]
pub extern "system" fn GetTextExtentPoint32W(hdc: Handle, _s: *const u16, n: i32, out: *mut [i32; 2]) -> Bool {
    extent(hdc, n, out)
}

#[no_mangle]
pub extern "system" fn GetTextExtentPoint32A(hdc: Handle, _s: *const u8, n: i32, out: *mut [i32; 2]) -> Bool {
    extent(hdc, n, out)
}

#[no_mangle]
pub extern "system" fn GetTextExtentPointW(hdc: Handle, _s: *const u16, n: i32, out: *mut [i32; 2]) -> Bool {
    extent(hdc, n, out)
}

#[no_mangle]
pub extern "system" fn GetTextExtentPointA(hdc: Handle, _s: *const u8, n: i32, out: *mut [i32; 2]) -> Bool {
    extent(hdc, n, out)
}

/// How many of `n` characters fit in `max` pixels, and each one's end.
#[no_mangle]
pub extern "system" fn GetTextExtentExPointW(hdc: Handle, _s: *const u16, n: i32, max: i32, fit: *mut i32, dx: *mut i32, size: *mut [i32; 2]) -> Bool {
    let mut t = crate::table();
    if !crate::is_dc(&mut t, hdc) {
        return 0;
    }
    let w = CELL_W * crate::font_scale(&mut t, hdc);
    drop(t);
    let n = n.max(0);
    if !fit.is_null() {
        unsafe { *fit = (max / w).min(n) };
    }
    if !dx.is_null() {
        for i in 0..n as usize {
            unsafe { *dx.add(i) = (i as i32 + 1) * w };
        }
    }
    if !size.is_null() {
        extent(hdc, n, size);
    }
    1
}

/// TEXTMETRICW (`wide`) or TEXTMETRICA.
fn metrics(hdc: Handle, out: *mut u8, wide: bool) -> Bool {
    let mut t = crate::table();
    if !crate::is_dc(&mut t, hdc) || out.is_null() {
        return 0;
    }
    let s = crate::font_scale(&mut t, hdc);
    let longs = [CELL_H * s, ASCENT * s, (CELL_H - ASCENT) * s, 0, 0, CELL_W * s, CELL_W * s, 400, 0, 96, 96];
    for (i, v) in longs.iter().enumerate() {
        unsafe { *(out.add(i * 4) as *mut i32) = *v };
    }
    // First, last, default and break characters, then italic, underlined,
    // struck out, pitch and family (fixed pitch, FF_MODERN), charset.
    let rest_at = if wide {
        for (i, c) in [0x20u16, 0xFF, 0x3F, 0x20].iter().enumerate() {
            unsafe { *(out.add(44 + i * 2) as *mut u16) = *c };
        }
        52
    } else {
        for (i, c) in [0x20u8, 0xFF, 0x3F, 0x20].iter().enumerate() {
            unsafe { *out.add(44 + i) = *c };
        }
        48
    };
    for (i, b) in [0u8, 0, 0, 0x30, 0].iter().enumerate() {
        unsafe { *out.add(rest_at + i) = *b };
    }
    1
}

#[no_mangle]
pub extern "system" fn GetTextMetricsW(hdc: Handle, out: *mut u8) -> Bool {
    metrics(hdc, out, true)
}

#[no_mangle]
pub extern "system" fn GetTextMetricsA(hdc: Handle, out: *mut u8) -> Bool {
    metrics(hdc, out, false)
}

#[no_mangle]
pub extern "system" fn GetTextFaceW(_hdc: Handle, n: i32, out: *mut u16) -> i32 {
    let face: Vec<u16> = "System".encode_utf16().chain(core::iter::once(0)).collect();
    if out.is_null() {
        return face.len() as i32;
    }
    let k = (n.max(0) as usize).min(face.len());
    if k == 0 {
        return 0;
    }
    unsafe { core::ptr::copy_nonoverlapping(face.as_ptr(), out, k) };
    unsafe { *out.add(k - 1) = 0 };
    k as i32 - 1
}

#[no_mangle]
pub extern "system" fn GetTextFaceA(_hdc: Handle, n: i32, out: *mut u8) -> i32 {
    let face = b"System\0";
    if out.is_null() {
        return face.len() as i32;
    }
    let k = (n.max(0) as usize).min(face.len());
    if k == 0 {
        return 0;
    }
    unsafe { core::ptr::copy_nonoverlapping(face.as_ptr(), out, k) };
    unsafe { *out.add(k - 1) = 0 };
    k as i32 - 1
}

#[no_mangle]
pub extern "system" fn GetCharWidth32W(hdc: Handle, first: u32, last: u32, out: *mut i32) -> Bool {
    let mut t = crate::table();
    if !crate::is_dc(&mut t, hdc) || out.is_null() || last < first {
        return 0;
    }
    let w = CELL_W * crate::font_scale(&mut t, hdc);
    for i in 0..=(last - first) as usize {
        unsafe { *out.add(i) = w };
    }
    1
}
