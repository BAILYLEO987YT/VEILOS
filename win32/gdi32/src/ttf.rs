//! TrueType fonts: the kernel's font engine (src/font.rs, compiled in) over
//! the .ttf files in C:\FONTS, and which face and size a LOGFONT gets.
//!
//! Windows' own fonts aren't here, so a request is matched by family name
//! if a face has it, else by kind: fixed-pitch requests (FIXED_PITCH,
//! FF_MODERN, Courier, Consolas, ...) get a monospaced face, the rest a
//! proportional one. "System", "Fixedsys" and "Terminal" stay the built-in
//! bitmap font. Bold is synthesized (drawn twice, a pixel apart) when the
//! face isn't bold itself.

use alloc::string::String;
use alloc::vec::Vec;

use crate::dllrt::Lock;
use crate::font::{Font as Face, GlyphCache};

#[link(name = "kernel32", kind = "raw-dylib")]
unsafe extern "system" {
    fn FindFirstFileW(name: *const u16, data: *mut u8) -> u64;
    fn FindNextFileW(find: u64, data: *mut u8) -> i32;
    fn FindClose(find: u64) -> i32;
    fn CreateFileW(name: *const u16, access: u32, share: u32, sa: usize, disposition: u32, flags: u32, template: u64) -> u64;
    fn GetFileSizeEx(file: u64, size: *mut i64) -> i32;
    fn ReadFile(file: u64, buf: *mut u8, n: u32, read: *mut u32, ov: usize) -> i32;
    fn CloseHandle(h: u64) -> i32;
}

struct Library {
    loaded: bool,
    faces: Vec<Face>,
    /// Each face is monospaced.
    fixed: Vec<bool>,
}

static LIB: Lock<Library> = Lock::new(Library { loaded: false, faces: Vec::new(), fixed: Vec::new() });
static GLYPHS: Lock<GlyphCache> = Lock::new(GlyphCache::new());

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(core::iter::once(0)).collect()
}

fn read_file(path: &[u16]) -> Option<Vec<u8>> {
    let h = unsafe { CreateFileW(path.as_ptr(), 0x8000_0000, 1, 0, 3, 0, 0) };
    if h == u64::MAX {
        return None;
    }
    let mut size = 0i64;
    unsafe { GetFileSizeEx(h, &mut size) };
    let mut data = alloc::vec![0u8; size.clamp(0, 64 << 20) as usize];
    let mut got = 0u32;
    let ok = unsafe { ReadFile(h, data.as_mut_ptr(), data.len() as u32, &mut got, 0) } != 0;
    unsafe { CloseHandle(h) };
    (ok && got as usize == data.len()).then_some(data)
}

impl Library {
    fn load(&mut self) {
        if self.loaded {
            return;
        }
        self.loaded = true;
        let mut find = [0u8; 592];
        let h = unsafe { FindFirstFileW(wide("C:\\FONTS\\*.TTF").as_ptr(), find.as_mut_ptr()) };
        if h == u64::MAX {
            return;
        }
        loop {
            let name: Vec<u16> = (0..260).map(|i| u16::from_le_bytes([find[44 + 2 * i], find[45 + 2 * i]])).take_while(|&c| c != 0).collect();
            let mut path = wide("C:\\FONTS\\");
            path.pop();
            path.extend_from_slice(&name);
            path.push(0);
            if let Some(face) = read_file(&path).and_then(|d| Face::parse(d).ok()) {
                let fixed = face.advance(face.glyph_index('i')) == face.advance(face.glyph_index('W'));
                self.faces.push(face);
                self.fixed.push(fixed);
            }
            if unsafe { FindNextFileW(h, find.as_mut_ptr()) } == 0 {
                break;
            }
        }
        unsafe { FindClose(h) };
    }
}

/// A LOGFONT made real: the face and size it draws with.
#[derive(Clone)]
pub struct Realized {
    face: usize,
    pub size_q6: u32,
    pub em_px: i32,
    pub ascent: i32,
    pub descent: i32,
    pub line_gap: i32,
    pub weight: i32,
    /// Bold drawn by doubling: every glyph a pixel wider.
    pub embolden: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
    pub fixed: bool,
    pub family: u8,
    pub avg_w: i32,
    pub max_w: i32,
    pub x_height: i32,
}

fn lower(s: &str) -> String {
    s.chars().map(|c| c.to_ascii_lowercase()).collect()
}

const FIXED_NAMES: [&str; 6] = ["courier", "consolas", "lucida console", "mono", "lucida sans typewriter", "ocr"];

/// The face and size for LOGFONTW `log`, or None for the bitmap font.
pub fn realize(log: &[u8; 92]) -> Option<Realized> {
    let height = i32::from_le_bytes(log[0..4].try_into().unwrap());
    let weight = i32::from_le_bytes(log[16..20].try_into().unwrap());
    let (italic, underline, strike) = (log[20] != 0, log[21] != 0, log[22] != 0);
    let pitch_family = log[27];
    let name: String = char::decode_utf16((0..32).map(|i| u16::from_le_bytes([log[28 + 2 * i], log[29 + 2 * i]])).take_while(|&c| c != 0))
        .map(|c| c.unwrap_or('?'))
        .collect();
    let name = lower(&name);
    if matches!(name.as_str(), "system" | "fixedsys" | "terminal") {
        return None;
    }
    let mut lib = LIB.lock();
    lib.load();
    if lib.faces.is_empty() {
        return None;
    }
    let want_fixed = pitch_family & 3 == 1 || pitch_family & 0xF0 == 0x30 || FIXED_NAMES.iter().any(|n| name.contains(n));
    let want_weight = if weight == 0 { 400 } else { weight };
    // A face of the family asked for, else one of the right kind; the
    // closest weight and slant among them.
    let named: Vec<usize> = (0..lib.faces.len()).filter(|&i| !name.is_empty() && lower(&lib.faces[i].family) == name).collect();
    let pool: Vec<usize> = if !named.is_empty() {
        named
    } else {
        let kind: Vec<usize> = (0..lib.faces.len()).filter(|&i| lib.fixed[i] == want_fixed).collect();
        if kind.is_empty() { (0..lib.faces.len()).collect() } else { kind }
    };
    let face = *pool
        .iter()
        .min_by_key(|&&i| {
            let f = &lib.faces[i];
            (f.weight as i32 - want_weight).abs() + if f.italic != italic { 1000 } else { 0 }
        })
        .unwrap();
    let f = &lib.faces[face];
    let upem = f.units_per_em.max(1) as i64;
    let cell_fu = (f.ascent as i64 - f.descent as i64).max(1);
    // Negative: the em height; positive: the cell height (ascent + descent).
    let size_q6 = match height {
        h if h < 0 => (-h as i64 * 64) as u32,
        0 => (16 * 64 * upem / cell_fu) as u32,
        h => (h as i64 * 64 * upem / cell_fu) as u32,
    }
    .clamp(64, 64 * 255);
    let m = f.metrics(size_q6);
    let px = |q6: i32| (q6 + 63).div_euclid(64);
    let embolden = want_weight >= 600 && f.weight < 600;
    let adv = |c: char| px(f.scale_q6(f.advance(f.glyph_index(c)) as i64, size_q6)) + embolden as i32;
    let max_w = (0x20u8..0x7F).map(|c| adv(c as char)).max().unwrap_or(0);
    Some(Realized {
        face,
        size_q6,
        em_px: (size_q6 as i32 + 32) / 64,
        ascent: px(m.ascent_q6),
        descent: px(m.descent_q6),
        line_gap: (m.line_gap_q6 + 32).div_euclid(64),
        weight: want_weight,
        embolden,
        italic,
        underline,
        strike,
        fixed: lib.fixed[face],
        family: if lib.fixed[face] { 0x30 } else { 0x20 },
        avg_w: adv('x'),
        max_w,
        x_height: px(f.scale_q6(f.x_height as i64, size_q6)),
    })
}

/// Where each character's glyph goes (q6 pixels from the start) and the
/// total advance, with kerning; or with the caller's advances (pixels).
pub fn layout(r: &Realized, text: &[u16], advances: Option<&[i32]>) -> (Vec<(u16, i32)>, i32) {
    let lib = LIB.lock();
    let f = &lib.faces[r.face];
    let chars: Vec<char> = char::decode_utf16(text.iter().copied()).map(|c| c.unwrap_or('\u{FFFD}')).collect();
    let mut out = Vec::with_capacity(chars.len());
    let mut pen_fu: i64 = 0;
    let mut extra = 0i32;
    let mut prev: Option<u16> = None;
    let mut given = 0i32;
    for (i, &ch) in chars.iter().enumerate() {
        let ch = if ch == '\t' { ' ' } else { ch };
        let gid = f.glyph_index(ch);
        if let Some(a) = advances {
            out.push((gid, given * 64));
            given += a.get(i).copied().unwrap_or(0);
            continue;
        }
        if let Some(p) = prev {
            pen_fu += f.kern(p, gid) as i64;
        }
        out.push((gid, f.scale_q6(pen_fu, r.size_q6) + extra));
        pen_fu += f.advance(gid) as i64;
        if r.embolden {
            extra += 64;
        }
        prev = Some(gid);
    }
    let total = if advances.is_some() { given * 64 } else { f.scale_q6(pen_fu, r.size_q6) + extra };
    (out, total)
}

/// Width in pixels of `q6`.
pub fn px(q6: i32) -> i32 {
    (q6 + 63).div_euclid(64)
}

/// Draw glyph `gid` with its origin at device q6 x `x_q6` on `baseline`,
/// blending `color` (a pixel) by its coverage.
pub fn draw_glyph(c: &crate::draw::Canvas, r: &Realized, gid: u16, x_q6: i32, baseline: i32, color: u32) {
    let lib = LIB.lock();
    let f = &lib.faces[r.face];
    let mut cache = GLYPHS.lock();
    let ix = x_q6.div_euclid(64);
    let sub = (x_q6.rem_euclid(64) >> 4) as u8;
    let Some(g) = cache.get(f, gid, r.size_q6, sub) else { return };
    let passes = if r.embolden { 2 } else { 1 };
    for pass in 0..passes {
        for gy in 0..g.h {
            let y = baseline + g.top + gy;
            for gx in 0..g.w {
                let cov = g.data[(gy * g.w + gx) as usize] as u32;
                // A slant for italic: rows above the baseline shift right.
                let lean = if r.italic { (-(g.top + gy)).max(0) / 4 } else { 0 };
                let x = ix + g.left + gx + pass + lean;
                if cov == 0 || !c.contains(x, y) {
                    continue;
                }
                let at = c.at(x, y);
                let d = unsafe { *at };
                let mut out = 0u32;
                for s in [0, 8, 16] {
                    let (fv, dv) = ((color >> s) & 0xFF, (d >> s) & 0xFF);
                    out |= ((fv * cov + dv * (255 - cov)) / 255) << s;
                }
                unsafe { *at = out };
            }
        }
    }
}

/// Every face: (family, fixed pitch, weight, italic).
pub fn families() -> Vec<(String, bool, i32, bool)> {
    let mut lib = LIB.lock();
    lib.load();
    lib.faces.iter().zip(&lib.fixed).map(|(f, &fx)| (f.family.clone(), fx, f.weight as i32, f.italic)).collect()
}

/// The family name of a realized font's face.
pub fn face_name(r: &Realized) -> String {
    LIB.lock().faces[r.face].family.clone()
}

