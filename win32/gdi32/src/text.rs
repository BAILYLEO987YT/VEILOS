//! Text: TrueType faces (ttf.rs) for the fonts programs create, and the
//! built-in 8x16 bitmap font (font_data.rs, from Cascadia Mono) for the
//! stock "System" fonts, drawn at a whole-number scale from the height.

use alloc::vec::Vec;

use crate::draw::{pixel, Canvas, R2_COPYPEN};
use crate::font_data::GLYPHS;
use crate::ttf::{self, Realized};
use crate::{Bool, Handle, Obj, Table};

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

/// How a DC's font draws.
#[derive(Clone)]
enum Face {
    /// The bitmap font at this scale, with underline/strike-out.
    Bitmap { scale: i32, underline: bool, strike: bool },
    TrueType(Realized),
}

impl Face {
    fn ascent(&self) -> i32 {
        match self {
            Face::Bitmap { scale, .. } => ASCENT * scale,
            Face::TrueType(r) => r.ascent,
        }
    }
    fn height(&self) -> i32 {
        match self {
            Face::Bitmap { scale, .. } => CELL_H * scale,
            Face::TrueType(r) => r.ascent + r.descent,
        }
    }
    /// Where a strike-out line goes, above the baseline.
    fn strike_rise(&self) -> i32 {
        match self {
            Face::Bitmap { scale, .. } => 4 * scale,
            Face::TrueType(r) => (r.x_height / 2).max(1),
        }
    }
    fn lines(&self) -> (bool, bool) {
        match self {
            Face::Bitmap { underline, strike, .. } => (*underline, *strike),
            Face::TrueType(r) => (r.underline, r.strike),
        }
    }
}

/// The DC's font, realized (once per font object).
fn face(t: &mut Table, hdc: Handle) -> Face {
    let Some(f) = t.dc(hdc).map(|dc| dc.s.font) else { return Face::Bitmap { scale: 1, underline: false, strike: false } };
    let Some(Obj::Font(font)) = t.get(f) else { return Face::Bitmap { scale: 1, underline: false, strike: false } };
    if font.tt.is_none() {
        font.tt = Some(ttf::realize(&font.log));
    }
    match font.tt.as_ref().unwrap() {
        Some(r) => Face::TrueType(r.clone()),
        None => {
            let h = i32::from_le_bytes(font.log[0..4].try_into().unwrap()).abs();
            Face::Bitmap { scale: if h == 0 { 1 } else { ((h + 8) / 16).clamp(1, 8) }, underline: font.log[21] != 0, strike: font.log[22] != 0 }
        }
    }
}

fn glyph(c: u16) -> &'static [u8; 16] {
    let i = match c {
        0x20..=0x7E => c as usize - 0x20,
        0xA0..=0xFF => c as usize - 0xA0 + 95,
        _ => '?' as usize - 0x20,
    };
    &GLYPHS[i]
}

/// Each character's x (q6 pixels from the start) and the total advance.
fn layout(face: &Face, text: &[u16], advances: Option<&[i32]>) -> (Vec<(u16, i32)>, i32) {
    match face {
        Face::TrueType(r) => ttf::layout(r, text, advances),
        Face::Bitmap { scale, .. } => {
            let mut x = 0;
            let mut out = Vec::with_capacity(text.len());
            for (i, &ch) in text.iter().enumerate() {
                out.push((ch, x * 64));
                x += advances.and_then(|a| a.get(i).copied()).unwrap_or(CELL_W * scale);
            }
            (out, x * 64)
        }
    }
}

/// Draw `text` at logical (x, y) by the DC's text alignment.
fn draw(hdc: Handle, x: i32, y: i32, text: &[u16], opts: u32, rect: Option<[i32; 4]>, advances: Option<&[i32]>) -> Bool {
    let mut t = crate::table();
    if !t.is_dc(hdc) {
        return 0;
    }
    let face = face(&mut t, hdc);
    let (fg, bg, mode, align, cp) = crate::dc_text_state(&mut t, hdc).unwrap();
    let (glyphs, total_q6) = layout(&face, text, advances);
    let width = ttf::px(total_q6);
    let height = face.height();
    // The reference point: the current position, or (x, y).
    let (rx, ry) = if align & TA_UPDATECP != 0 { cp } else { (x, y) };
    let (dx, dy) = crate::to_device(&mut t, hdc, rx, ry);
    let left = match align & TA_CENTER {
        TA_CENTER => dx - width / 2,
        TA_RIGHT => dx - width,
        _ => dx,
    };
    let top = match align & TA_BASELINE {
        TA_BASELINE => dy - face.ascent(),
        TA_BOTTOM => dy - height,
        _ => dy,
    };
    if align & TA_UPDATECP != 0 {
        let nx = if align & TA_CENTER == TA_RIGHT { rx - width } else { rx + width };
        crate::set_pos(&mut t, hdc, (nx, ry));
    }
    let Some(mut c) = t.canvas(hdc) else { return 1 };
    let rect = rect.map(|r| {
        let (x0, y0) = crate::to_device(&mut t, hdc, r[0], r[1]);
        let (x1, y1) = crate::to_device(&mut t, hdc, r[2], r[3]);
        (x0, y0, x1, y1)
    });
    if let Some((x0, y0, x1, y1)) = rect {
        if opts & ETO_OPAQUE != 0 {
            c.fill(x0, y0, x1, y1, pixel(bg), R2_COPYPEN);
        }
        if opts & ETO_CLIPPED != 0 {
            c.x0 = c.x0.max(x0);
            c.y0 = c.y0.max(y0);
            c.x1 = c.x1.min(x1);
            c.y1 = c.y1.min(y1);
        }
    }
    let (fg, bg) = (pixel(fg), pixel(bg));
    if mode == 2 {
        c.fill(left, top, left + width, top + height, bg, R2_COPYPEN);
    }
    let baseline = top + face.ascent();
    for &(g, x_q6) in &glyphs {
        match &face {
            Face::TrueType(r) => ttf::draw_glyph(&c, r, g, left * 64 + x_q6, baseline, fg),
            Face::Bitmap { scale, .. } => bitmap_glyph(&c, g, left + x_q6 / 64, top, *scale, fg),
        }
    }
    let (underline, strike) = face.lines();
    let thick = (height / 16).max(1);
    if underline {
        c.fill(left, baseline + thick, left + width, baseline + 2 * thick, fg, R2_COPYPEN);
    }
    if strike {
        let mid = baseline - face.strike_rise();
        c.fill(left, mid, left + width, mid + thick, fg, R2_COPYPEN);
    }
    t.done(hdc);
    1
}

fn bitmap_glyph(c: &Canvas, ch: u16, x: i32, top: i32, scale: i32, fg: u32) {
    for (row, bits) in glyph(ch).iter().enumerate() {
        if *bits == 0 {
            continue;
        }
        for col in 0..8 {
            if bits & (0x80 >> col) != 0 {
                let (px, py) = (x + col * scale, top + row as i32 * scale);
                c.fill(px, py, px + scale, py + scale, fg, R2_COPYPEN);
            }
        }
    }
}

unsafe fn wide(s: *const u16, n: i32) -> Vec<u16> {
    if s.is_null() || n <= 0 {
        return Vec::new();
    }
    unsafe { core::slice::from_raw_parts(s, n as usize) }.to_vec()
}

/// ANSI text (UTF-8, VEILOS's ANSI code page) as UTF-16.
unsafe fn ansi(s: *const u8, n: i32) -> Vec<u16> {
    if s.is_null() || n <= 0 {
        return Vec::new();
    }
    let bytes = unsafe { core::slice::from_raw_parts(s, n as usize) };
    match core::str::from_utf8(bytes) {
        Ok(text) => text.encode_utf16().collect(),
        Err(_) => bytes.iter().map(|&b| b as u16).collect(),
    }
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

/// Width and height of `text`; an empty string measures 0 x 0, as on
/// Windows.
fn extent(hdc: Handle, text: &[u16], out: *mut [i32; 2]) -> Bool {
    let mut t = crate::table();
    if !t.is_dc(hdc) || out.is_null() {
        return 0;
    }
    let face = face(&mut t, hdc);
    let (_, total) = layout(&face, text, None);
    unsafe { *out = if text.is_empty() { [0, 0] } else { [ttf::px(total), face.height()] } };
    1
}

#[no_mangle]
pub unsafe extern "system" fn GetTextExtentPoint32W(hdc: Handle, s: *const u16, n: i32, out: *mut [i32; 2]) -> Bool {
    extent(hdc, &unsafe { wide(s, n) }, out)
}

#[no_mangle]
pub unsafe extern "system" fn GetTextExtentPoint32A(hdc: Handle, s: *const u8, n: i32, out: *mut [i32; 2]) -> Bool {
    extent(hdc, &unsafe { ansi(s, n) }, out)
}

#[no_mangle]
pub unsafe extern "system" fn GetTextExtentPointW(hdc: Handle, s: *const u16, n: i32, out: *mut [i32; 2]) -> Bool {
    extent(hdc, &unsafe { wide(s, n) }, out)
}

#[no_mangle]
pub unsafe extern "system" fn GetTextExtentPointA(hdc: Handle, s: *const u8, n: i32, out: *mut [i32; 2]) -> Bool {
    extent(hdc, &unsafe { ansi(s, n) }, out)
}

/// How many characters fit in `max` pixels, and where each one ends.
fn extent_ex(hdc: Handle, text: &[u16], max: i32, fit: *mut i32, dx: *mut i32, size: *mut [i32; 2]) -> Bool {
    let mut t = crate::table();
    if !t.is_dc(hdc) {
        return 0;
    }
    let face = face(&mut t, hdc);
    drop(t);
    let (glyphs, total) = layout(&face, text, None);
    let ends: Vec<i32> = (0..glyphs.len()).map(|i| ttf::px(glyphs.get(i + 1).map_or(total, |g| g.1))).collect();
    if !fit.is_null() {
        unsafe { *fit = ends.iter().take_while(|&&e| e <= max).count() as i32 };
    }
    if !dx.is_null() {
        for (i, e) in ends.iter().enumerate() {
            unsafe { *dx.add(i) = *e };
        }
    }
    if !size.is_null() {
        extent(hdc, text, size);
    }
    1
}

#[no_mangle]
pub unsafe extern "system" fn GetTextExtentExPointW(hdc: Handle, s: *const u16, n: i32, max: i32, fit: *mut i32, dx: *mut i32, size: *mut [i32; 2]) -> Bool {
    extent_ex(hdc, &unsafe { wide(s, n) }, max, fit, dx, size)
}

#[no_mangle]
pub unsafe extern "system" fn GetTextExtentExPointA(hdc: Handle, s: *const u8, n: i32, max: i32, fit: *mut i32, dx: *mut i32, size: *mut [i32; 2]) -> Bool {
    extent_ex(hdc, &unsafe { ansi(s, n) }, max, fit, dx, size)
}

/// TEXTMETRICW (`wide`) or TEXTMETRICA.
fn metrics(hdc: Handle, out: *mut u8, wide: bool) -> Bool {
    let mut t = crate::table();
    if !t.is_dc(hdc) || out.is_null() {
        return 0;
    }
    let face = face(&mut t, hdc);
    // Height, ascent, descent, internal and external leading, average and
    // maximum width, weight, overhang, digitized aspect; then first, last,
    // default and break characters; italic, underlined, struck out, pitch
    // and family, charset.
    let (longs, chars, flags): ([i32; 11], [u16; 4], [u8; 5]) = match &face {
        Face::Bitmap { scale: s, underline, strike } => (
            [CELL_H * s, ASCENT * s, (CELL_H - ASCENT) * s, 0, 0, CELL_W * s, CELL_W * s, 400, 0, 96, 96],
            [0x20, 0xFF, 0x3F, 0x20],
            [0, *underline as u8, *strike as u8, 0x30, 0],
        ),
        Face::TrueType(r) => {
            let height = r.ascent + r.descent;
            // TMPF_FIXED_PITCH set means variable pitch; TMPF_VECTOR, TMPF_TRUETYPE.
            let pitch = (!r.fixed) as u8 | 2 | 4 | r.family;
            (
                [height, r.ascent, r.descent, (height - r.em_px).max(0), r.line_gap, r.avg_w, r.max_w, r.weight, 0, 96, 96],
                [0x20, 0xFFFC, 0x1F, 0x20],
                [r.italic as u8 * 255, r.underline as u8, r.strike as u8, pitch, 0],
            )
        }
    };
    for (i, v) in longs.iter().enumerate() {
        unsafe { *(out.add(i * 4) as *mut i32) = *v };
    }
    let rest_at = if wide {
        for (i, c) in chars.iter().enumerate() {
            unsafe { *(out.add(44 + i * 2) as *mut u16) = *c };
        }
        52
    } else {
        for (i, c) in chars.iter().enumerate() {
            unsafe { *out.add(44 + i) = (*c).min(0xFF) as u8 };
        }
        48
    };
    for (i, b) in flags.iter().enumerate() {
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

/// The face name the program asked for (what it would get on Windows),
/// or the face drawing it if it asked for none.
fn face_name(hdc: Handle) -> Vec<u16> {
    let mut t = crate::table();
    let Some(f) = t.dc(hdc).map(|dc| dc.s.font) else { return Vec::new() };
    let asked: Vec<u16> = match t.get(f) {
        Some(Obj::Font(font)) => (0..32).map(|i| u16::from_le_bytes([font.log[28 + 2 * i], font.log[29 + 2 * i]])).take_while(|&c| c != 0).collect(),
        _ => Vec::new(),
    };
    if !asked.is_empty() {
        return asked;
    }
    match face(&mut t, hdc) {
        Face::TrueType(r) => ttf::face_name(&r).encode_utf16().collect(),
        Face::Bitmap { .. } => "System".encode_utf16().collect(),
    }
}

#[no_mangle]
pub extern "system" fn GetTextFaceW(hdc: Handle, n: i32, out: *mut u16) -> i32 {
    let mut name = face_name(hdc);
    name.push(0);
    if out.is_null() {
        return name.len() as i32;
    }
    let k = (n.max(0) as usize).min(name.len());
    if k == 0 {
        return 0;
    }
    unsafe { core::ptr::copy_nonoverlapping(name.as_ptr(), out, k) };
    unsafe { *out.add(k - 1) = 0 };
    k as i32 - 1
}

#[no_mangle]
pub extern "system" fn GetTextFaceA(hdc: Handle, n: i32, out: *mut u8) -> i32 {
    let name: Vec<u8> = alloc::string::String::from_utf16_lossy(&face_name(hdc)).into_bytes().into_iter().chain(core::iter::once(0)).collect();
    if out.is_null() {
        return name.len() as i32;
    }
    let k = (n.max(0) as usize).min(name.len());
    if k == 0 {
        return 0;
    }
    unsafe { core::ptr::copy_nonoverlapping(name.as_ptr(), out, k) };
    unsafe { *out.add(k - 1) = 0 };
    k as i32 - 1
}

// -----| enumerating fonts |-----

/// Call `proc(ENUMLOGFONTEXW*, NEWTEXTMETRICEXW*, FontType, lParam)` for
/// each family (or each face of `family`): the installed TrueType faces,
/// then the bitmap "System". Stops when it returns 0; that's the result.
fn enumerate(family: Option<alloc::vec::Vec<u16>>, proc: usize, lp: i64, wide: bool) -> i32 {
    if proc == 0 {
        return 0;
    }
    let f: unsafe extern "system" fn(*const u8, *const u8, u32, i64) -> i32 = unsafe { core::mem::transmute(proc) };
    let mut faces = ttf::families();
    faces.push((alloc::string::String::from("System"), false, 400, false));
    let lower = |s: &str| s.chars().map(|c| c.to_ascii_lowercase()).collect::<alloc::string::String>();
    let want = family.map(|w| lower(&alloc::string::String::from_utf16_lossy(&w)));
    let mut last = 1;
    let mut seen: alloc::vec::Vec<alloc::string::String> = alloc::vec::Vec::new();
    for (name, fixed, weight, italic) in faces {
        if want.as_ref().is_some_and(|w| *w != lower(&name)) {
            continue;
        }
        // One call per family unless a family was asked for.
        if want.is_none() && seen.contains(&lower(&name)) {
            continue;
        }
        seen.push(lower(&name));
        let tt = name != "System";
        // ENUMLOGFONTEXW: LOGFONTW, full name, style, script (A: bytes).
        let mut lf = [0u8; 420];
        lf[0..4].copy_from_slice(&(if tt { -16i32 } else { 16 }).to_le_bytes());
        lf[16..20].copy_from_slice(&weight.to_le_bytes());
        lf[20] = italic as u8;
        lf[27] = if fixed { 0x31 } else { 0x22 };
        let style = match (weight >= 600, italic) {
            (false, false) => "Regular",
            (true, false) => "Bold",
            (false, true) => "Italic",
            (true, true) => "Bold Italic",
        };
        let put = |buf: &mut [u8], at: usize, s: &str, w: bool| {
            for (i, c) in s.encode_utf16().enumerate() {
                if w {
                    buf[at + 2 * i..at + 2 * i + 2].copy_from_slice(&c.to_le_bytes());
                } else {
                    buf[at + i] = c as u8;
                }
            }
        };
        let (face_at, full_at, style_at, script_at) = if wide { (28, 92, 220, 284) } else { (28, 60, 124, 156) };
        put(&mut lf, face_at, &name, wide);
        put(&mut lf, full_at, &name, wide);
        put(&mut lf, style_at, style, wide);
        put(&mut lf, script_at, "Western", wide);
        // NEWTEXTMETRICEXW: the metrics at 16 pixels.
        let mut tm = [0u8; 120];
        let longs = [16i32, 12, 4, 0, 0, 8, 8, weight, 0, 96, 96];
        for (i, v) in longs.iter().enumerate() {
            tm[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
        }
        let kind = if tt { 4 } else { 0 }; // TRUETYPE_FONTTYPE
        last = unsafe { f(lf.as_ptr(), tm.as_ptr(), kind, lp) };
        if last == 0 {
            break;
        }
    }
    last
}

fn family_arg(p: *const u8, wide: bool, at_logfont: bool) -> Option<alloc::vec::Vec<u16>> {
    if p.is_null() {
        return None;
    }
    let p = if at_logfont { unsafe { p.add(28) } } else { p };
    let v: alloc::vec::Vec<u16> = if wide {
        (0..32).map(|i| unsafe { *(p as *const u16).add(i) }).take_while(|&c| c != 0).collect()
    } else {
        (0..32).map(|i| unsafe { *p.add(i) } as u16).take_while(|&c| c != 0).collect()
    };
    (!v.is_empty()).then_some(v)
}

#[no_mangle]
pub extern "system" fn EnumFontFamiliesExW(_hdc: Handle, lf: *const u8, proc: usize, lp: i64, _flags: u32) -> i32 {
    enumerate(family_arg(lf, true, true), proc, lp, true)
}

#[no_mangle]
pub extern "system" fn EnumFontFamiliesExA(_hdc: Handle, lf: *const u8, proc: usize, lp: i64, _flags: u32) -> i32 {
    enumerate(family_arg(lf, false, true), proc, lp, false)
}

#[no_mangle]
pub extern "system" fn EnumFontFamiliesW(_hdc: Handle, family: *const u8, proc: usize, lp: i64) -> i32 {
    enumerate(family_arg(family, true, false), proc, lp, true)
}

#[no_mangle]
pub extern "system" fn EnumFontFamiliesA(_hdc: Handle, family: *const u8, proc: usize, lp: i64) -> i32 {
    enumerate(family_arg(family, false, false), proc, lp, false)
}

#[no_mangle]
pub extern "system" fn EnumFontsW(_hdc: Handle, face: *const u8, proc: usize, lp: i64) -> i32 {
    enumerate(family_arg(face, true, false), proc, lp, true)
}

#[no_mangle]
pub extern "system" fn EnumFontsA(_hdc: Handle, face: *const u8, proc: usize, lp: i64) -> i32 {
    enumerate(family_arg(face, false, false), proc, lp, false)
}

/// Fonts can't be added at run time yet.
#[no_mangle]
pub extern "system" fn AddFontResourceExW(_name: *const u16, _flags: u32, _r: usize) -> i32 {
    0
}

#[no_mangle]
pub extern "system" fn RemoveFontResourceExW(_name: *const u16, _flags: u32, _r: usize) -> Bool {
    0
}

#[no_mangle]
pub extern "system" fn GetTextCharset(_hdc: Handle) -> i32 {
    0
}

#[no_mangle]
pub extern "system" fn GetTextCharsetInfo(_hdc: Handle, sig: *mut u8, _flags: u32) -> i32 {
    if !sig.is_null() {
        unsafe { core::ptr::write_bytes(sig, 0, 24) };
    }
    0
}

/// Advance widths of characters `first..=last`.
fn char_widths(hdc: Handle, first: u32, last: u32, out: *mut i32, abc: bool) -> Bool {
    let mut t = crate::table();
    if !t.is_dc(hdc) || out.is_null() || last < first || last - first > 0x10000 {
        return 0;
    }
    let face = face(&mut t, hdc);
    drop(t);
    for (i, ch) in (first..=last).enumerate() {
        let (_, w) = layout(&face, &[ch as u16], None);
        let w = ttf::px(w);
        unsafe {
            if abc {
                // ABC { A, B, C }: no overhangs.
                *out.add(i * 3) = 0;
                *(out.add(i * 3 + 1) as *mut u32) = w as u32;
                *out.add(i * 3 + 2) = 0;
            } else {
                *out.add(i) = w;
            }
        }
    }
    1
}

#[no_mangle]
pub extern "system" fn GetCharWidth32W(hdc: Handle, first: u32, last: u32, out: *mut i32) -> Bool {
    char_widths(hdc, first, last, out, false)
}

#[no_mangle]
pub extern "system" fn GetCharWidth32A(hdc: Handle, first: u32, last: u32, out: *mut i32) -> Bool {
    char_widths(hdc, first, last, out, false)
}

#[no_mangle]
pub extern "system" fn GetCharWidthW(hdc: Handle, first: u32, last: u32, out: *mut i32) -> Bool {
    char_widths(hdc, first, last, out, false)
}

#[no_mangle]
pub extern "system" fn GetCharWidthA(hdc: Handle, first: u32, last: u32, out: *mut i32) -> Bool {
    char_widths(hdc, first, last, out, false)
}

#[no_mangle]
pub extern "system" fn GetCharABCWidthsW(hdc: Handle, first: u32, last: u32, out: *mut i32) -> Bool {
    char_widths(hdc, first, last, out, true)
}

#[no_mangle]
pub extern "system" fn GetCharABCWidthsA(hdc: Handle, first: u32, last: u32, out: *mut i32) -> Bool {
    char_widths(hdc, first, last, out, true)
}
