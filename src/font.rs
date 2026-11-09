// -----| VEILOS font engine |-----
//
// TrueType (glyf) font loading, anti-aliased glyph rasterising, kerning, text
// layout and a font library. Pure `alloc` code with NO kernel dependencies and
// no floating point, like render2d.rs; the kernel glue is in render2d_kernel.rs.
//
//   Font::parse(bytes)            parse a .ttf (never panics on corrupt data)
//   rasterize(font, gid, size)    exact-area anti-aliased coverage bitmap
//   GlyphCache                    bitmaps cached per (font, glyph, size, subpixel)
//   layout / measure / draw_text  advances + kerning + optional tabular digits
//   FontLibrary                   pick a face by family / weight / italic
//
// Supported: TrueType outlines (.ttf, TrueType collections use the first font),
// cmap formats 4/6/12, composite glyphs, `kern` table and GPOS pair kerning.
// Not supported (reported, never drawn wrongly): CFF outlines (.otf "OTTO") and
// variable fonts' variation data (the default instance is used). TrueType
// hinting instructions are ignored: glyphs are rendered unhinted.
//
// Units: font units scale to pixels as fu * size / units_per_em. Sizes are in
// 1/64 px ("q6"), positions and advances in q6, bitmap coverage 0..=255.
// Glyphs get 4 horizontal sub-pixel phases (0, 1/4, 1/2, 3/4 px) so spacing
// stays even at small sizes.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, Ordering};

use crate::render2d::{Canvas, Color32};

/// Largest glyph bitmap side we will rasterise (pixels).
const MAX_BITMAP_SIDE: i32 = 512;
/// Smallest / largest supported size, in q6 (1 px .. 256 px).
const MIN_SIZE_Q6: u32 = 64;
const MAX_SIZE_Q6: u32 = 64 * 256;
/// Safety limits while reading outlines from untrusted data.
const MAX_POINTS: usize = 16384;
const MAX_COMPOSITE_DEPTH: u32 = 8;
const MAX_COMPONENTS: u32 = 256;

static NEXT_FONT_ID: AtomicU32 = AtomicU32::new(1);

// =====================================================================
// -----| bounds-checked big-endian readers |-----
// =====================================================================

#[inline]
fn rd16(d: &[u8], o: usize) -> Option<u16> {
    let b = d.get(o..o.checked_add(2)?)?;
    Some(((b[0] as u16) << 8) | b[1] as u16)
}

#[inline]
fn rd16s(d: &[u8], o: usize) -> Option<i16> {
    rd16(d, o).map(|v| v as i16)
}

#[inline]
fn rd32(d: &[u8], o: usize) -> Option<u32> {
    let b = d.get(o..o.checked_add(4)?)?;
    Some(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
}

#[inline]
fn rd8(d: &[u8], o: usize) -> Option<u8> {
    d.get(o).copied()
}

// =====================================================================
// -----| errors |-----
// =====================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FontError {
    TooShort,
    BadMagic,
    /// OpenType with CFF outlines (.otf): only TrueType `glyf` outlines are supported.
    CffOutlines,
    MissingTable(&'static str),
    Corrupt,
}

impl FontError {
    pub fn describe(&self) -> &'static str {
        match self {
            FontError::TooShort => "file too short",
            FontError::BadMagic => "not a TrueType font",
            FontError::CffOutlines => "CFF-outline OpenType (.otf) is not supported, use a TrueType .ttf",
            FontError::MissingTable(_) => "required font table missing",
            FontError::Corrupt => "corrupt font data",
        }
    }
}

// =====================================================================
// -----| font |-----
// =====================================================================

#[derive(Debug, Clone, Copy)]
struct CmapSel {
    off: usize,
    format: u16,
}

pub struct Font {
    data: Vec<u8>,
    id: u32,
    pub units_per_em: u16,
    num_glyphs: u16,
    num_hmetrics: u16,
    loca_long: bool,
    glyf: (usize, usize),
    loca: (usize, usize),
    hmtx: (usize, usize),
    cmap: CmapSel,
    kern: Option<(usize, usize)>,
    /// Absolute offsets of GPOS PairPos subtables reached through the `kern` feature.
    gpos_kern: Vec<usize>,
    /// Ascent / descent (negative) / line gap in font units.
    pub ascent: i16,
    pub descent: i16,
    pub line_gap: i16,
    pub weight: u16,
    pub italic: bool,
    pub family: String,
    pub subfamily: String,
    /// Height of capital letters / lowercase x in font units (OS/2, else measured from 'H' / 'x').
    pub cap_height: i16,
    pub x_height: i16,
    digit_adv: u16,
}

/// Raw outline in font units: points as (x, y, on_curve) plus contour end indices.
pub struct Outline {
    pub pts: Vec<(i32, i32, bool)>,
    pub ends: Vec<usize>,
}

fn find_table(d: &[u8], base: usize, num: usize, tag: &[u8; 4]) -> Option<(usize, usize)> {
    for i in 0..num {
        let r = base.checked_add(12)?.checked_add(i.checked_mul(16)?)?;
        let t = d.get(r..r.checked_add(4)?)?;
        if t == tag {
            let off = rd32(d, r + 8)? as usize;
            let len = rd32(d, r + 12)? as usize;
            return if off.checked_add(len)? <= d.len() { Some((off, len)) } else { None };
        }
    }
    None
}

fn decode_name(d: &[u8], name: (usize, usize), id: u16) -> Option<String> {
    let (t, tlen) = name;
    let count = rd16(d, t + 2)? as usize;
    let str_off = rd16(d, t + 4)? as usize;
    let mut best: Option<(u32, usize, usize, u16)> = None; // (score, off, len, platform)
    for i in 0..count.min(512) {
        let r = t + 6 + i * 12;
        let platform = rd16(d, r)?;
        let lang = rd16(d, r + 4)?;
        let nid = rd16(d, r + 6)?;
        if nid != id {
            continue;
        }
        let len = rd16(d, r + 8)? as usize;
        let off = rd16(d, r + 10)? as usize;
        let score = match (platform, lang) {
            (3, 0x409) => 3,
            (3, _) => 2,
            (1, _) => 1,
            _ => 0,
        };
        if score == 0 || off + len > tlen.saturating_sub(str_off) {
            continue;
        }
        if best.map_or(true, |b| score > b.0) {
            best = Some((score, t + str_off + off, len, platform));
        }
    }
    let (_, off, len, platform) = best?;
    let bytes = d.get(off..off + len)?;
    if platform == 3 {
        let units: Vec<u16> = bytes.chunks_exact(2).map(|c| ((c[0] as u16) << 8) | c[1] as u16).collect();
        Some(core::char::decode_utf16(units).map(|r| r.unwrap_or('?')).collect())
    } else {
        Some(bytes.iter().map(|&b| if b < 128 { b as char } else { '?' }).collect())
    }
}

impl Font {
    /// Parse a TrueType font. Takes ownership of the file bytes.
    pub fn parse(data: Vec<u8>) -> Result<Font, FontError> {
        let d = &data[..];
        if d.len() < 12 {
            return Err(FontError::TooShort);
        }
        let mut base = 0usize;
        if rd32(d, 0) == Some(0x7474_6366) {
            // 'ttcf': a collection, use the first font.
            base = rd32(d, 12).ok_or(FontError::TooShort)? as usize;
        }
        match rd32(d, base).ok_or(FontError::TooShort)? {
            0x0001_0000 | 0x7472_7565 => {}
            0x4F54_544F => return Err(FontError::CffOutlines),
            _ => return Err(FontError::BadMagic),
        }
        let num = rd16(d, base + 4).ok_or(FontError::TooShort)? as usize;
        if num == 0 || num > 256 {
            return Err(FontError::Corrupt);
        }
        let tab = |tag: &'static [u8; 4], name: &'static str| find_table(d, base, num, tag).ok_or(FontError::MissingTable(name));

        let head = tab(b"head", "head")?;
        let maxp = tab(b"maxp", "maxp")?;
        let hhea = tab(b"hhea", "hhea")?;
        let hmtx = tab(b"hmtx", "hmtx")?;
        let cmap = tab(b"cmap", "cmap")?;
        if find_table(d, base, num, b"CFF ").is_some() && find_table(d, base, num, b"glyf").is_none() {
            return Err(FontError::CffOutlines);
        }
        let loca = tab(b"loca", "loca")?;
        let glyf = tab(b"glyf", "glyf")?;

        if head.1 < 54 || maxp.1 < 6 || hhea.1 < 36 {
            return Err(FontError::Corrupt);
        }
        let units_per_em = rd16(d, head.0 + 18).ok_or(FontError::Corrupt)?;
        if !(16..=16384).contains(&units_per_em) {
            return Err(FontError::Corrupt);
        }
        let loca_long = rd16s(d, head.0 + 50).ok_or(FontError::Corrupt)? != 0;
        let mac_style = rd16(d, head.0 + 44).unwrap_or(0);
        let num_glyphs = rd16(d, maxp.0 + 4).ok_or(FontError::Corrupt)?;
        if num_glyphs == 0 {
            return Err(FontError::Corrupt);
        }
        let need_loca = (num_glyphs as usize + 1) * if loca_long { 4 } else { 2 };
        if loca.1 < need_loca {
            return Err(FontError::Corrupt);
        }
        let num_hmetrics = rd16(d, hhea.0 + 34).ok_or(FontError::Corrupt)?;
        if num_hmetrics == 0 || hmtx.1 < num_hmetrics as usize * 4 {
            return Err(FontError::Corrupt);
        }

        // Vertical metrics: OS/2 typo metrics when the font asks for them, else hhea.
        let mut ascent = rd16s(d, hhea.0 + 4).ok_or(FontError::Corrupt)?;
        let mut descent = rd16s(d, hhea.0 + 6).ok_or(FontError::Corrupt)?;
        let mut line_gap = rd16s(d, hhea.0 + 8).ok_or(FontError::Corrupt)?;
        let mut weight = 400u16;
        let (mut cap_height, mut x_height) = (0i16, 0i16);
        let mut italic = mac_style & 2 != 0;
        if let Some((o, l)) = find_table(d, base, num, b"OS/2") {
            if l >= 64 {
                weight = rd16(d, o + 4).unwrap_or(400).clamp(1, 1000);
                let sel = rd16(d, o + 62).unwrap_or(0);
                italic = italic || sel & 1 != 0;
                if rd16(d, o).unwrap_or(0) >= 2 && l >= 90 {
                    x_height = rd16s(d, o + 86).unwrap_or(0).max(0);
                    cap_height = rd16s(d, o + 88).unwrap_or(0).max(0);
                }
                if l >= 74 && sel & 128 != 0 {
                    let (ta, td, tg) = (rd16s(d, o + 68).unwrap_or(0), rd16s(d, o + 70).unwrap_or(0), rd16s(d, o + 72).unwrap_or(0));
                    if ta != 0 || td != 0 {
                        ascent = ta;
                        descent = td;
                        line_gap = tg;
                    }
                }
            }
        }
        if descent > 0 {
            descent = -descent;
        }

        let (mut family, mut subfamily) = (String::new(), String::new());
        if let Some(nt) = find_table(d, base, num, b"name") {
            family = decode_name(d, nt, 16).or_else(|| decode_name(d, nt, 1)).unwrap_or_default();
            subfamily = decode_name(d, nt, 17).or_else(|| decode_name(d, nt, 2)).unwrap_or_default();
        }

        let cmap_sel = Self::select_cmap(d, cmap).ok_or(FontError::MissingTable("cmap (usable subtable)"))?;
        let kern = find_table(d, base, num, b"kern");
        let gpos_kern = match find_table(d, base, num, b"GPOS") {
            Some(g) => collect_gpos_kern(d, g.0).unwrap_or_default(),
            None => Vec::new(),
        };

        let mut font = Font {
            data: Vec::new(),
            id: NEXT_FONT_ID.fetch_add(1, Ordering::Relaxed),
            units_per_em,
            num_glyphs,
            num_hmetrics,
            loca_long,
            glyf,
            loca,
            hmtx,
            cmap: cmap_sel,
            kern,
            gpos_kern,
            ascent,
            descent,
            line_gap,
            weight,
            italic,
            family,
            subfamily,
            cap_height,
            x_height,
            digit_adv: 0,
        };
        // Move the bytes in last: everything above borrowed `d` from them.
        font.data = data;
        let mut max_digit = 0u16;
        for c in '0'..='9' {
            let g = font.glyph_index(c);
            max_digit = max_digit.max(font.advance(g));
        }
        font.digit_adv = max_digit;
        // Fonts without OS/2 v2+ heights: measure them from the outlines of 'H' and 'x'.
        let top_of = |font: &Font, c: char| -> i16 {
            font.outline(font.glyph_index(c))
                .and_then(|o| o.pts.iter().map(|p| p.1).max())
                .unwrap_or(0)
                .clamp(0, i16::MAX as i32) as i16
        };
        if font.cap_height == 0 {
            font.cap_height = top_of(&font, 'H');
        }
        if font.x_height == 0 {
            font.x_height = top_of(&font, 'x');
        }
        Ok(font)
    }

    fn select_cmap(d: &[u8], cmap: (usize, usize)) -> Option<CmapSel> {
        let n = rd16(d, cmap.0 + 2)? as usize;
        let mut best: Option<(u32, CmapSel)> = None;
        for i in 0..n.min(64) {
            let r = cmap.0 + 4 + i * 8;
            let (platform, enc) = (rd16(d, r)?, rd16(d, r + 2)?);
            let off = cmap.0 + rd32(d, r + 4)? as usize;
            let format = match rd16(d, off) {
                Some(f) => f,
                None => continue,
            };
            let supported = matches!(format, 4 | 6 | 12);
            let score = match (platform, enc, format) {
                (3, 10, 12) => 5,
                (0, _, 12) => 4,
                (3, 1, 4) => 4,
                (0, _, 4) => 3,
                (3, 0, 4) => 2,
                (_, _, 4) | (_, _, 6) | (_, _, 12) => 1,
                _ => 0,
            };
            if supported && score > 0 && best.map_or(true, |b| score > b.0) {
                best = Some((score, CmapSel { off, format }));
            }
        }
        best.map(|b| b.1)
    }

    /// Process-unique id (cache key).
    pub fn id(&self) -> u32 {
        self.id
    }

    pub fn num_glyphs(&self) -> u16 {
        self.num_glyphs
    }

    pub fn has_glyph(&self, c: char) -> bool {
        self.glyph_index(c) != 0
    }

    /// Glyph id for a character; 0 (.notdef) if the font has none.
    pub fn glyph_index(&self, c: char) -> u16 {
        let cp = c as u32;
        let d = &self.data[..];
        let o = self.cmap.off;
        let gid = match self.cmap.format {
            4 => self.cmap4(d, o, cp),
            6 => self.cmap6(d, o, cp),
            12 => self.cmap12(d, o, cp),
            _ => None,
        };
        match gid {
            Some(g) if g < self.num_glyphs => g,
            _ => 0,
        }
    }

    fn cmap4(&self, d: &[u8], o: usize, cp: u32) -> Option<u16> {
        if cp > 0xFFFF {
            return None;
        }
        let c = cp as u16;
        let seg2 = rd16(d, o + 6)? as usize;
        let segs = seg2 / 2;
        let end_codes = o + 14;
        let start_codes = o + 16 + seg2;
        let id_deltas = o + 16 + 2 * seg2;
        let id_range_offs = o + 16 + 3 * seg2;
        // first segment whose endCode >= c
        let (mut lo, mut hi) = (0usize, segs);
        while lo < hi {
            let mid = (lo + hi) / 2;
            if rd16(d, end_codes + mid * 2)? < c {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        if lo >= segs {
            return None;
        }
        let start = rd16(d, start_codes + lo * 2)?;
        if c < start {
            return None;
        }
        let delta = rd16(d, id_deltas + lo * 2)?;
        let range = rd16(d, id_range_offs + lo * 2)? as usize;
        if range == 0 {
            return Some(c.wrapping_add(delta));
        }
        let addr = id_range_offs + lo * 2 + range + 2 * (c - start) as usize;
        let g = rd16(d, addr)?;
        if g == 0 { Some(0) } else { Some(g.wrapping_add(delta)) }
    }

    fn cmap6(&self, d: &[u8], o: usize, cp: u32) -> Option<u16> {
        let first = rd16(d, o + 6)? as u32;
        let count = rd16(d, o + 8)? as u32;
        if cp < first || cp >= first + count {
            return None;
        }
        rd16(d, o + 10 + 2 * (cp - first) as usize)
    }

    fn cmap12(&self, d: &[u8], o: usize, cp: u32) -> Option<u16> {
        let n = rd32(d, o + 12)? as usize;
        let (mut lo, mut hi) = (0usize, n.min(1 << 20));
        while lo < hi {
            let mid = (lo + hi) / 2;
            let g = o + 16 + mid * 12;
            let (s, e) = (rd32(d, g)?, rd32(d, g + 4)?);
            if cp < s {
                hi = mid;
            } else if cp > e {
                lo = mid + 1;
            } else {
                let sg = rd32(d, g + 8)?;
                return Some((sg.wrapping_add(cp - s)) as u16);
            }
        }
        None
    }

    /// Horizontal advance in font units (the last metric repeats for later glyphs).
    pub fn advance(&self, gid: u16) -> u16 {
        let i = (gid as usize).min(self.num_hmetrics as usize - 1);
        rd16(&self.data, self.hmtx.0 + i * 4).unwrap_or(0)
    }

    /// Kerning between two glyphs in font units (negative = closer). GPOS `kern`
    /// wins over the legacy `kern` table.
    pub fn kern(&self, left: u16, right: u16) -> i32 {
        let d = &self.data[..];
        if !self.gpos_kern.is_empty() {
            for &sub in &self.gpos_kern {
                if let Some(v) = pairpos_value(d, sub, left, right) {
                    return v;
                }
            }
            return 0;
        }
        match self.kern {
            Some((o, l)) => kern_table_value(d, o, l, left, right).unwrap_or(0),
            None => 0,
        }
    }

    pub fn has_kerning(&self) -> bool {
        !self.gpos_kern.is_empty() || self.kern.is_some()
    }

    /// Scale font units to q6 pixels for `size_q6`.
    #[inline]
    pub fn scale_q6(&self, fu: i64, size_q6: u32) -> i32 {
        let upem = self.units_per_em as i64;
        ((fu * size_q6 as i64 + upem / 2).div_euclid(upem)) as i32
    }
}

// =====================================================================
// -----| kerning: GPOS pair adjustment + legacy kern table |-----
// =====================================================================

fn collect_gpos_kern(d: &[u8], gpos: usize) -> Option<Vec<usize>> {
    let feature_list = gpos + rd16(d, gpos + 6)? as usize;
    let lookup_list = gpos + rd16(d, gpos + 8)? as usize;
    let mut wanted: BTreeSet<u16> = BTreeSet::new();
    let nfeat = rd16(d, feature_list)? as usize;
    for i in 0..nfeat.min(256) {
        let rec = feature_list + 2 + i * 6;
        if d.get(rec..rec + 4)? == b"kern" {
            let ft = feature_list + rd16(d, rec + 4)? as usize;
            let n = rd16(d, ft + 2)? as usize;
            for j in 0..n.min(256) {
                wanted.insert(rd16(d, ft + 4 + 2 * j)?);
            }
        }
    }
    let nlook = rd16(d, lookup_list)?;
    let mut out = Vec::new();
    for idx in wanted {
        if idx >= nlook {
            continue;
        }
        let lk = lookup_list + rd16(d, lookup_list + 2 + 2 * idx as usize)? as usize;
        let ltype = rd16(d, lk)?;
        let nsub = rd16(d, lk + 4)? as usize;
        for s in 0..nsub.min(64) {
            let sub = lk + rd16(d, lk + 6 + 2 * s)? as usize;
            match ltype {
                2 => out.push(sub),
                9 => {
                    // Extension lookup wrapping a PairPos subtable.
                    if rd16(d, sub)? == 1 && rd16(d, sub + 2)? == 2 {
                        out.push(sub + rd32(d, sub + 4)? as usize);
                    }
                }
                _ => {}
            }
            if out.len() > 512 {
                return Some(out);
            }
        }
    }
    Some(out)
}

fn coverage_index(d: &[u8], off: usize, gid: u16) -> Option<usize> {
    match rd16(d, off)? {
        1 => {
            let n = rd16(d, off + 2)? as usize;
            let (mut lo, mut hi) = (0usize, n);
            while lo < hi {
                let mid = (lo + hi) / 2;
                let g = rd16(d, off + 4 + mid * 2)?;
                if g == gid {
                    return Some(mid);
                } else if g < gid {
                    lo = mid + 1;
                } else {
                    hi = mid;
                }
            }
            None
        }
        2 => {
            let n = rd16(d, off + 2)? as usize;
            let (mut lo, mut hi) = (0usize, n);
            while lo < hi {
                let mid = (lo + hi) / 2;
                let r = off + 4 + mid * 6;
                let (s, e) = (rd16(d, r)?, rd16(d, r + 2)?);
                if gid < s {
                    hi = mid;
                } else if gid > e {
                    lo = mid + 1;
                } else {
                    return Some(rd16(d, r + 4)? as usize + (gid - s) as usize);
                }
            }
            None
        }
        _ => None,
    }
}

fn class_of(d: &[u8], off: usize, gid: u16) -> Option<u16> {
    match rd16(d, off)? {
        1 => {
            let start = rd16(d, off + 2)?;
            let n = rd16(d, off + 4)?;
            if gid >= start && gid - start < n {
                rd16(d, off + 6 + 2 * (gid - start) as usize)
            } else {
                Some(0)
            }
        }
        2 => {
            let n = rd16(d, off + 2)? as usize;
            let (mut lo, mut hi) = (0usize, n);
            while lo < hi {
                let mid = (lo + hi) / 2;
                let r = off + 4 + mid * 6;
                let (s, e) = (rd16(d, r)?, rd16(d, r + 2)?);
                if gid < s {
                    hi = mid;
                } else if gid > e {
                    lo = mid + 1;
                } else {
                    return rd16(d, r + 4);
                }
            }
            Some(0)
        }
        _ => None,
    }
}

/// X advance (font units) from a ValueRecord whose layout is given by `fmt`.
fn xadvance_of(d: &[u8], rec: usize, fmt: u16) -> Option<i32> {
    if fmt & 0x0004 == 0 {
        return Some(0);
    }
    let skip = (fmt & 0x0003).count_ones() as usize; // XPlacement, YPlacement come first
    Some(rd16s(d, rec + 2 * skip)? as i32)
}

/// Kerning from one PairPos subtable. None = this subtable does not apply to the pair.
fn pairpos_value(d: &[u8], sub: usize, left: u16, right: u16) -> Option<i32> {
    let format = rd16(d, sub)?;
    let cov = sub + rd16(d, sub + 2)? as usize;
    let (vf1, vf2) = (rd16(d, sub + 4)?, rd16(d, sub + 6)?);
    let (s1, s2) = ((vf1.count_ones() * 2) as usize, (vf2.count_ones() * 2) as usize);
    let ci = coverage_index(d, cov, left)?;
    match format {
        1 => {
            let npairsets = rd16(d, sub + 8)? as usize;
            if ci >= npairsets {
                return None;
            }
            let set = sub + rd16(d, sub + 10 + 2 * ci)? as usize;
            let n = rd16(d, set)? as usize;
            let rec_len = 2 + s1 + s2;
            let (mut lo, mut hi) = (0usize, n);
            while lo < hi {
                let mid = (lo + hi) / 2;
                let r = set + 2 + mid * rec_len;
                let g = rd16(d, r)?;
                if g == right {
                    return xadvance_of(d, r + 2, vf1);
                } else if g < right {
                    lo = mid + 1;
                } else {
                    hi = mid;
                }
            }
            None
        }
        2 => {
            let cd1 = sub + rd16(d, sub + 8)? as usize;
            let cd2 = sub + rd16(d, sub + 10)? as usize;
            let n1 = rd16(d, sub + 12)? as usize;
            let n2 = rd16(d, sub + 14)? as usize;
            let c1 = class_of(d, cd1, left)? as usize;
            let c2 = class_of(d, cd2, right)? as usize;
            if c1 >= n1 || c2 >= n2 {
                return None;
            }
            let rec = sub + 16 + (c1 * n2 + c2) * (s1 + s2);
            xadvance_of(d, rec, vf1)
        }
        _ => None,
    }
}

fn kern_table_value(d: &[u8], off: usize, len: usize, left: u16, right: u16) -> Option<i32> {
    let ntables = rd16(d, off + 2)? as usize;
    let mut p = off + 4;
    let end = off + len;
    for _ in 0..ntables.min(16) {
        if p + 6 > end {
            break;
        }
        let sublen = rd16(d, p + 2)? as usize;
        let coverage = rd16(d, p + 4)?;
        let format = coverage >> 8;
        // horizontal (bit 0), not minimum (bit 1), not cross-stream (bit 2)
        if format == 0 && coverage & 1 == 1 && coverage & 6 == 0 {
            let npairs = rd16(d, p + 6)? as usize;
            let key = ((left as u32) << 16) | right as u32;
            let (mut lo, mut hi) = (0usize, npairs);
            while lo < hi {
                let mid = (lo + hi) / 2;
                let r = p + 14 + mid * 6;
                let k = rd32(d, r)?;
                if k == key {
                    return Some(rd16s(d, r + 4)? as i32);
                } else if k < key {
                    lo = mid + 1;
                } else {
                    hi = mid;
                }
            }
        }
        if sublen == 0 {
            break;
        }
        p += sublen;
    }
    None
}

// =====================================================================
// -----| glyph outlines |-----
// =====================================================================

impl Font {
    /// Absolute byte range of a glyph's data; None for empty / invalid glyphs.
    fn glyph_range(&self, gid: u16) -> Option<(usize, usize)> {
        if gid >= self.num_glyphs {
            return None;
        }
        let d = &self.data[..];
        let g = gid as usize;
        let (a, b) = if self.loca_long {
            (rd32(d, self.loca.0 + g * 4)? as usize, rd32(d, self.loca.0 + g * 4 + 4)? as usize)
        } else {
            (rd16(d, self.loca.0 + g * 2)? as usize * 2, rd16(d, self.loca.0 + g * 2 + 2)? as usize * 2)
        };
        if b <= a || b > self.glyf.1 {
            return None;
        }
        Some((self.glyf.0 + a, self.glyf.0 + b))
    }

    /// The glyph's contours in font units, composites resolved. `Some` with no
    /// points for blank glyphs such as space; None only for corrupt data.
    pub fn outline(&self, gid: u16) -> Option<Outline> {
        let mut out = Outline { pts: Vec::new(), ends: Vec::new() };
        let mut comps = MAX_COMPONENTS;
        self.outline_into(gid, 0, &mut out, &mut comps)?;
        Some(out)
    }

    fn outline_into(&self, gid: u16, depth: u32, out: &mut Outline, comps: &mut u32) -> Option<()> {
        if depth > MAX_COMPOSITE_DEPTH {
            return None;
        }
        let (start, end) = match self.glyph_range(gid) {
            Some(r) => r,
            None => return Some(()),
        };
        let d = &self.data[start..end];
        let nc = rd16s(d, 0)?;
        if nc >= 0 {
            self.simple_glyph(d, nc as usize, out)
        } else {
            self.composite_glyph(d, depth, out, comps)
        }
    }

    fn simple_glyph(&self, d: &[u8], nc: usize, out: &mut Outline) -> Option<()> {
        if nc == 0 {
            return Some(());
        }
        let mut ends: Vec<usize> = Vec::with_capacity(nc);
        let mut prev: i64 = -1;
        for i in 0..nc {
            let e = rd16(d, 10 + 2 * i)? as usize;
            if e as i64 <= prev {
                return None;
            }
            ends.push(e);
            prev = e as i64;
        }
        let npts = ends[nc - 1] + 1;
        if npts > MAX_POINTS || out.pts.len() + npts > MAX_POINTS {
            return None;
        }
        let mut p = 10 + 2 * nc;
        let ilen = rd16(d, p)? as usize;
        p += 2 + ilen;
        let mut flags: Vec<u8> = Vec::with_capacity(npts);
        while flags.len() < npts {
            let f = rd8(d, p)?;
            p += 1;
            flags.push(f);
            if f & 8 != 0 {
                let r = rd8(d, p)? as usize;
                p += 1;
                for _ in 0..r {
                    if flags.len() >= npts {
                        break;
                    }
                    flags.push(f);
                }
            }
        }
        let mut xs: Vec<i32> = Vec::with_capacity(npts);
        let mut x = 0i32;
        for &f in &flags {
            if f & 2 != 0 {
                let v = rd8(d, p)? as i32;
                p += 1;
                x += if f & 16 != 0 { v } else { -v };
            } else if f & 16 == 0 {
                x += rd16s(d, p)? as i32;
                p += 2;
            }
            xs.push(x);
        }
        let mut ys: Vec<i32> = Vec::with_capacity(npts);
        let mut y = 0i32;
        for &f in &flags {
            if f & 4 != 0 {
                let v = rd8(d, p)? as i32;
                p += 1;
                y += if f & 32 != 0 { v } else { -v };
            } else if f & 32 == 0 {
                y += rd16s(d, p)? as i32;
                p += 2;
            }
            ys.push(y);
        }
        let base = out.pts.len();
        for i in 0..npts {
            out.pts.push((xs[i], ys[i], flags[i] & 1 != 0));
        }
        for e in ends {
            out.ends.push(base + e);
        }
        Some(())
    }

    fn composite_glyph(&self, d: &[u8], depth: u32, out: &mut Outline, comps: &mut u32) -> Option<()> {
        let mut p = 10usize;
        loop {
            if *comps == 0 {
                return None;
            }
            *comps -= 1;
            let flags = rd16(d, p)?;
            let gi = rd16(d, p + 2)?;
            p += 4;
            let (a1, a2) = if flags & 0x0001 != 0 {
                let v = (rd16s(d, p)? as i32, rd16s(d, p + 2)? as i32);
                p += 4;
                v
            } else {
                let v = (rd8(d, p)? as i8 as i32, rd8(d, p + 1)? as i8 as i32);
                p += 2;
                v
            };
            // Point-matching components (flag 0x0002 clear) are not supported: offset 0.
            let (mut dx, mut dy) = if flags & 0x0002 != 0 { (a1, a2) } else { (0, 0) };
            // x' = xx*x + yx*y ; y' = xy*x + yy*y   (F2Dot14)
            let (mut xx, mut xy, mut yx, mut yy) = (0x4000i64, 0i64, 0i64, 0x4000i64);
            if flags & 0x0008 != 0 {
                let s = rd16s(d, p)? as i64;
                p += 2;
                xx = s;
                yy = s;
            } else if flags & 0x0040 != 0 {
                xx = rd16s(d, p)? as i64;
                yy = rd16s(d, p + 2)? as i64;
                p += 4;
            } else if flags & 0x0080 != 0 {
                xx = rd16s(d, p)? as i64;
                xy = rd16s(d, p + 2)? as i64;
                yx = rd16s(d, p + 4)? as i64;
                yy = rd16s(d, p + 6)? as i64;
                p += 8;
            }
            let first = out.pts.len();
            self.outline_into(gi, depth + 1, out, comps)?;
            if flags & 0x0800 != 0 && flags & 0x1000 == 0 {
                // SCALED_COMPONENT_OFFSET: scale the offset by the matrix column lengths.
                let mx = isqrt_i64(xx * xx + xy * xy);
                let my = isqrt_i64(yy * yy + yx * yx);
                dx = ((dx as i64 * mx + 0x2000) >> 14) as i32;
                dy = ((dy as i64 * my + 0x2000) >> 14) as i32;
            }
            for i in first..out.pts.len() {
                let (x, y, on) = out.pts[i];
                let nx = ((xx * x as i64 + yx * y as i64 + 0x2000) >> 14) as i32 + dx;
                let ny = ((xy * x as i64 + yy * y as i64 + 0x2000) >> 14) as i32 + dy;
                out.pts[i] = (nx, ny, on);
            }
            if flags & 0x0020 == 0 {
                break;
            }
        }
        Some(())
    }
}

fn isqrt_i64(n: i64) -> i64 {
    crate::render2d::isqrt_u64(n.max(0) as u64) as i64
}

// =====================================================================
// -----| rasteriser: exact-area anti-aliased coverage |-----
// =====================================================================

/// A rendered glyph: 8-bit coverage, `left`/`top` give the bitmap's top-left
/// relative to the pen position on the baseline (y grows downward, so `top` is
/// negative above the baseline).
pub struct Glyph {
    pub w: i32,
    pub h: i32,
    pub left: i32,
    pub top: i32,
    pub data: Vec<u8>,
}

impl Glyph {
    fn empty() -> Glyph {
        Glyph { w: 0, h: 0, left: 0, top: 0, data: Vec::new() }
    }
}

/// Add one row's signed coverage of a line piece (xa..xb, Q8) with signed height d (Q8).
/// Accumulators are Q16: summing a row left to right yields coverage (65536 = 1.0).
fn acc_row(acc: &mut [i32], row_off: usize, xa: i64, xb: i64, d: i64) {
    let (x0, x1) = if xa < xb { (xa, xb) } else { (xb, xa) };
    let x0i = (x0 >> 8).max(0) as usize;
    let x1i = ((x1 + 255) >> 8).max(0) as usize;
    let mut add = |i: usize, frac_q16: i64| {
        if let Some(c) = acc.get_mut(row_off + i) {
            *c = c.wrapping_add(((d * frac_q16) >> 8) as i32);
        }
    };
    if x1i <= x0i + 1 {
        // The piece stays within one pixel column.
        let xmf = (((xa + xb) >> 1) - ((x0i as i64) << 8)).clamp(0, 256);
        add(x0i, (256 - xmf) << 8);
        add(x0i + 1, xmf << 8);
    } else {
        let w = (x1 - x0).max(1); // Q8
        let x0f = x0 - ((x0i as i64) << 8);
        let u = 256 - x0f;
        let a0 = (u * u * 128) / w;
        let x1f = x1 - ((x1i as i64) << 8) + 256;
        let am = (x1f * x1f * 128) / w;
        let s = (1i64 << 24) / w;
        add(x0i, a0);
        if x1i == x0i + 2 {
            add(x0i + 1, 65536 - a0 - am);
        } else {
            let a1 = ((384 - x0f) << 16) / w;
            add(x0i + 1, a1 - a0);
            let mut xi = x0i + 2;
            while xi + 1 < x1i {
                add(xi, s);
                xi += 1;
            }
            let a2 = a1 + (x1i - x0i - 3) as i64 * s;
            add(x1i - 1, 65536 - a2 - am);
        }
        add(x1i, am);
    }
}

fn draw_line(acc: &mut [i32], stride: usize, h: usize, p0: (i32, i32), p1: (i32, i32)) {
    if p0.1 == p1.1 {
        return;
    }
    let (dir, a, b) = if p0.1 < p1.1 { (1i64, p0, p1) } else { (-1i64, p1, p0) };
    let (ax, ay, bx, by) = (a.0 as i64, a.1 as i64, b.0 as i64, b.1 as i64);
    let dy = by - ay;
    let dx = bx - ax;
    let mut row = (ay >> 8).max(0);
    let last = ((by - 1) >> 8).min(h as i64 - 1);
    while row <= last {
        let top = row << 8;
        let bot = top + 256;
        let ya = ay.max(top);
        let yb = by.min(bot);
        let xa = ax + (ya - ay) * dx / dy;
        let xb = ax + (yb - ay) * dx / dy;
        acc_row(acc, row as usize * stride, xa, xb, dir * (yb - ya));
        row += 1;
    }
}

fn quad(p0: (i32, i32), c: (i32, i32), p1: (i32, i32), line: &mut dyn FnMut((i32, i32), (i32, i32))) {
    let dx = (p0.0 as i64 - 2 * c.0 as i64 + p1.0 as i64).abs();
    let dy = (p0.1 as i64 - 2 * c.1 as i64 + p1.1 as i64).abs();
    // Flattening error <= dev / (4 n^2); keep it under 1/32 px (8 in Q8).
    let n = (crate::render2d::isqrt_u64(((dx + dy) / 32) as u64) as i64 + 1).clamp(1, 24);
    let mut prev = p0;
    for i in 1..=n {
        let (a, b, cc) = ((n - i) * (n - i), 2 * (n - i) * i, i * i);
        let nn = n * n;
        let x = (a * p0.0 as i64 + b * c.0 as i64 + cc * p1.0 as i64 + nn / 2).div_euclid(nn) as i32;
        let y = (a * p0.1 as i64 + b * c.1 as i64 + cc * p1.1 as i64 + nn / 2).div_euclid(nn) as i32;
        line(prev, (x, y));
        prev = (x, y);
    }
}

/// Walk one TrueType contour (on/off-curve points, implied on-curve midpoints)
/// emitting straight pieces. Coordinates are shifted by (-ox, -oy).
fn flatten_contour(c: &[(i32, i32, bool)], ox: i32, oy: i32, line: &mut dyn FnMut((i32, i32), (i32, i32))) {
    let n = c.len();
    if n < 2 {
        return;
    }
    let rel = |p: (i32, i32, bool)| (p.0 - ox, p.1 - oy);
    let (start, begin, end);
    if c[0].2 {
        start = rel(c[0]);
        begin = 1;
        end = n;
    } else if c[n - 1].2 {
        start = rel(c[n - 1]);
        begin = 0;
        end = n - 1;
    } else {
        let (a, b) = (rel(c[0]), rel(c[n - 1]));
        start = ((a.0 + b.0) >> 1, (a.1 + b.1) >> 1);
        begin = 0;
        end = n;
    }
    let mut cur = start;
    let mut ctrl: Option<(i32, i32)> = None;
    for i in begin..end {
        let p = c[i];
        let q = rel(p);
        if p.2 {
            match ctrl.take() {
                Some(cc) => quad(cur, cc, q, line),
                None => line(cur, q),
            }
            cur = q;
        } else {
            match ctrl {
                Some(cc) => {
                    let m = ((cc.0 + q.0) >> 1, (cc.1 + q.1) >> 1);
                    quad(cur, cc, m, line);
                    cur = m;
                    ctrl = Some(q);
                }
                None => ctrl = Some(q),
            }
        }
    }
    match ctrl {
        Some(cc) => quad(cur, cc, start, line),
        None => line(cur, start),
    }
}

/// Render one glyph at `size_q6` (1/64 px) with a horizontal sub-pixel shift of
/// `sub_q8` (1/256 px). None if the size is unsupported or the glyph is corrupt/huge.
pub fn rasterize(font: &Font, gid: u16, size_q6: u32, sub_q8: i32) -> Option<Glyph> {
    if !(MIN_SIZE_Q6..=MAX_SIZE_Q6).contains(&size_q6) {
        return None;
    }
    let outline = font.outline(gid)?;
    if outline.pts.is_empty() {
        return Some(Glyph::empty());
    }
    let q8 = size_q6 * 4; // font units -> Q8 pixels via scale_q6
    let pts: Vec<(i32, i32, bool)> = outline
        .pts
        .iter()
        .map(|&(x, y, on)| (font.scale_q6(x as i64, q8) + sub_q8, -font.scale_q6(y as i64, q8), on))
        .collect();
    let (mut minx, mut maxx, mut miny, mut maxy) = (i32::MAX, i32::MIN, i32::MAX, i32::MIN);
    for &(x, y, _) in &pts {
        minx = minx.min(x);
        maxx = maxx.max(x);
        miny = miny.min(y);
        maxy = maxy.max(y);
    }
    let x0 = minx.div_euclid(256);
    let y0 = miny.div_euclid(256);
    let x1 = (maxx + 255).div_euclid(256);
    let y1 = (maxy + 255).div_euclid(256);
    let (w, h) = (x1 - x0, y1 - y0);
    if w <= 0 || h <= 0 {
        return Some(Glyph::empty());
    }
    if w > MAX_BITMAP_SIDE || h > MAX_BITMAP_SIDE {
        return None;
    }
    let stride = w as usize + 2;
    let mut acc = vec![0i32; stride * h as usize];
    {
        let mut line = |a: (i32, i32), b: (i32, i32)| draw_line(&mut acc, stride, h as usize, a, b);
        let mut start = 0usize;
        for &e in &outline.ends {
            if e >= pts.len() || e < start {
                return None;
            }
            flatten_contour(&pts[start..=e], x0 * 256, y0 * 256, &mut line);
            start = e + 1;
        }
    }
    let mut data = vec![0u8; (w * h) as usize];
    for row in 0..h as usize {
        let mut sum: i64 = 0;
        for x in 0..w as usize {
            sum += acc[row * stride + x] as i64;
            let v = sum.abs().min(65536);
            data[row * w as usize + x] = ((v * 255 + 32768) >> 16) as u8;
        }
    }
    Some(Glyph { w, h, left: x0, top: y0, data })
}

// =====================================================================
// -----| glyph cache |-----
// =====================================================================

const GLYPH_CACHE_BYTES: usize = 2 * 1024 * 1024;

type GKey = (u32, u16, u32, u8);

struct GEntry {
    last: u64,
    glyph: Glyph,
}

/// Rendered glyphs keyed by (font, glyph, size, sub-pixel phase). `const fn new`
/// so it can live in a static.
pub struct GlyphCache {
    map: BTreeMap<GKey, GEntry>,
    bytes: usize,
    tick: u64,
    pub hits: u64,
    pub misses: u64,
}

impl GlyphCache {
    pub const fn new() -> GlyphCache {
        GlyphCache { map: BTreeMap::new(), bytes: 0, tick: 0, hits: 0, misses: 0 }
    }

    pub fn bytes(&self) -> usize {
        self.bytes
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn clear(&mut self) {
        self.map.clear();
        self.bytes = 0;
    }

    /// The glyph bitmap, rendering it on a miss. `sub` is the quarter-pixel phase (0..=3).
    pub fn get(&mut self, font: &Font, gid: u16, size_q6: u32, sub: u8) -> Option<&Glyph> {
        let sub = sub & 3;
        let key: GKey = (font.id(), gid, size_q6, sub);
        self.tick += 1;
        let tick = self.tick;
        if self.map.contains_key(&key) {
            self.hits += 1;
        } else {
            self.misses += 1;
            let g = rasterize(font, gid, size_q6, sub as i32 * 64)?;
            self.bytes += g.data.len() + 64;
            while self.bytes > GLYPH_CACHE_BYTES && self.map.len() > 1 {
                let victim = self.map.iter().min_by_key(|(_, e)| e.last).map(|(k, _)| *k);
                match victim {
                    Some(k) => {
                        if let Some(e) = self.map.remove(&k) {
                            self.bytes -= e.glyph.data.len() + 64;
                        }
                    }
                    None => break,
                }
            }
            self.map.insert(key, GEntry { last: tick, glyph: g });
        }
        let e = self.map.get_mut(&key)?;
        e.last = tick;
        Some(&e.glyph)
    }
}

// =====================================================================
// -----| text layout and drawing |-----
// =====================================================================

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextStyle {
    /// Font size in 1/64 pixel.
    pub size_q6: u32,
    /// Give every digit the same advance (clocks, counters) and centre it.
    pub tabular_digits: bool,
    /// Extra space after every glyph, in 1/64 px.
    pub letter_spacing_q6: i32,
}

impl TextStyle {
    pub const fn px(px: u32) -> TextStyle {
        TextStyle { size_q6: px * 64, tabular_digits: false, letter_spacing_q6: 0 }
    }
    pub const fn q6(size_q6: u32) -> TextStyle {
        TextStyle { size_q6, tabular_digits: false, letter_spacing_q6: 0 }
    }
    pub const fn tabular(mut self) -> TextStyle {
        self.tabular_digits = true;
        self
    }
    pub const fn spacing(mut self, q6: i32) -> TextStyle {
        self.letter_spacing_q6 = q6;
        self
    }
}

/// Vertical metrics for a size, in q6 pixels (ascent and descent both positive).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LineMetrics {
    pub ascent_q6: i32,
    pub descent_q6: i32,
    pub line_gap_q6: i32,
}

impl LineMetrics {
    pub fn height_q6(&self) -> i32 {
        self.ascent_q6 + self.descent_q6 + self.line_gap_q6
    }
}

impl Font {
    /// Baseline pixel row that visually centres a line of capital letters on `centre_y`.
    pub fn baseline_for_centre(&self, centre_y: i32, size_q6: u32) -> i32 {
        let cap_q6 = self.scale_q6(self.cap_height as i64, size_q6);
        centre_y + (cap_q6 / 2 + 32).div_euclid(64)
    }

    pub fn metrics(&self, size_q6: u32) -> LineMetrics {
        LineMetrics {
            ascent_q6: self.scale_q6(self.ascent as i64, size_q6),
            descent_q6: self.scale_q6(-(self.descent as i64), size_q6),
            line_gap_q6: self.scale_q6(self.line_gap as i64, size_q6),
        }
    }
}

/// Lay `text` out on one line, calling `emit(glyph, pen_x_q6)` for each glyph
/// (pen relative to the start of the line). Returns the total advance in q6.
/// Newlines are skipped and tabs become spaces; there is no shaping beyond
/// kerning (no ligatures, no right-to-left).
pub fn layout<F: FnMut(u16, i32)>(font: &Font, text: &str, style: &TextStyle, mut emit: F) -> i32 {
    let mut pen_fu: i64 = 0;
    let mut spacing: i64 = 0;
    let mut prev: Option<u16> = None;
    for ch in text.chars() {
        if ch == '\n' || ch == '\r' {
            continue;
        }
        let ch = if ch == '\t' { ' ' } else { ch };
        let gid = font.glyph_index(ch);
        let mut adv = font.advance(gid) as i64;
        let mut centre = 0i64;
        if style.tabular_digits && ch.is_ascii_digit() {
            let m = font.digit_adv as i64;
            centre = (m - adv) / 2;
            adv = m;
        }
        if let Some(p) = prev {
            pen_fu += font.kern(p, gid) as i64;
        }
        let x = font.scale_q6(pen_fu + centre, style.size_q6) as i64 + spacing;
        emit(gid, x as i32);
        pen_fu += adv;
        spacing += style.letter_spacing_q6 as i64;
        prev = Some(gid);
    }
    (font.scale_q6(pen_fu, style.size_q6) as i64 + spacing) as i32
}

/// Width of `text` in q6 pixels.
pub fn measure(font: &Font, text: &str, style: &TextStyle) -> i32 {
    layout(font, text, style, |_, _| {})
}

/// Draw one line of text with its baseline at `baseline_y` and its origin at
/// pixel `x`. Returns the advance in q6 pixels.
#[allow(clippy::too_many_arguments)]
pub fn draw_text(
    canvas: &mut Canvas,
    cache: &mut GlyphCache,
    font: &Font,
    text: &str,
    x: i32,
    baseline_y: i32,
    color: Color32,
    opacity: u8,
    style: &TextStyle,
) -> i32 {
    let st = TextStyle { size_q6: style.size_q6.clamp(MIN_SIZE_Q6, MAX_SIZE_Q6), ..*style };
    layout(font, text, &st, |gid, pen_q6| {
        let xq = x as i64 * 64 + pen_q6 as i64;
        let ix = xq.div_euclid(64) as i32;
        let sub = (xq.rem_euclid(64) >> 4) as u8;
        if let Some(g) = cache.get(font, gid, st.size_q6, sub) {
            if g.w > 0 {
                canvas.blit_mask(&g.data, g.w, g.h, ix + g.left, baseline_y + g.top, color, opacity);
            }
        }
    })
}

/// `text` shortened with an ellipsis so it fits in `max_q6`. Unchanged if it already fits.
pub fn fit_with_ellipsis(font: &Font, text: &str, style: &TextStyle, max_q6: i32) -> String {
    if measure(font, text, style) <= max_q6 {
        return String::from(text);
    }
    let ell = if font.has_glyph('\u{2026}') { "\u{2026}" } else { "..." };
    if measure(font, ell, style) > max_q6 {
        return String::new();
    }
    let chars: Vec<char> = text.chars().collect();
    let (mut lo, mut hi) = (0usize, chars.len());
    while lo < hi {
        let mid = (lo + hi + 1) / 2;
        let mut s: String = chars[..mid].iter().collect();
        s.push_str(ell);
        if measure(font, &s, style) <= max_q6 {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    let mut s: String = chars[..lo].iter().collect();
    while s.ends_with(' ') {
        s.pop();
    }
    s.push_str(ell);
    s
}

// =====================================================================
// -----| font library |-----
// =====================================================================

/// The loaded faces. `pick` resolves a CSS-like (family, weight, italic) request.
pub struct FontLibrary {
    faces: Vec<Font>,
    default_family: String,
}

impl FontLibrary {
    pub const fn new() -> FontLibrary {
        FontLibrary { faces: Vec::new(), default_family: String::new() }
    }

    pub fn add(&mut self, font: Font) -> usize {
        if self.default_family.is_empty() {
            self.default_family = font.family.clone();
        }
        self.faces.push(font);
        self.faces.len() - 1
    }

    pub fn len(&self) -> usize {
        self.faces.len()
    }

    pub fn is_empty(&self) -> bool {
        self.faces.is_empty()
    }

    pub fn faces(&self) -> &[Font] {
        &self.faces
    }

    pub fn clear(&mut self) {
        self.faces.clear();
    }

    /// The family used for "", "default", "system" and "sans" and as the fallback.
    pub fn set_default_family(&mut self, family: &str) {
        self.default_family = String::from(family);
    }

    pub fn default_family(&self) -> &str {
        &self.default_family
    }

    /// Best face for the request: matching family (case-insensitive; unknown or
    /// generic names fall back to the default family, then to any face), nearest
    /// weight, italic preferred only when asked for.
    pub fn pick(&self, family: &str, weight: u16, italic: bool) -> Option<&Font> {
        if self.faces.is_empty() {
            return None;
        }
        let want = family.trim();
        let generic = want.is_empty()
            || ["default", "system", "sans", "sans-serif", "ui"].iter().any(|g| g.eq_ignore_ascii_case(want));
        let matches = |name: &str| self.faces.iter().filter(|f| f.family.eq_ignore_ascii_case(name)).count();
        let name: &str = if !generic && matches(want) > 0 {
            want
        } else if matches(&self.default_family) > 0 {
            &self.default_family
        } else {
            ""
        };
        let score = |f: &Font| -> i32 {
            (f.weight as i32 - weight as i32).abs() + if f.italic != italic { 1000 } else { 0 }
        };
        self.faces
            .iter()
            .filter(|f| name.is_empty() || f.family.eq_ignore_ascii_case(name))
            .min_by_key(|f| score(f))
    }
}


// =====================================================================
// -----| demo: text overlay for the render2d test card |-----
// =====================================================================

pub mod demo {
    use super::*;

    /// Draw labels over `render2d::demo::draw_test_card` (laid out for 1024x768):
    /// corner-radius captions, a glass-card title, the taskbar clock (tabular digits)
    /// and a specimen line. Returns false if the library has no fonts.
    pub fn draw_text_overlay(c: &mut Canvas, glyphs: &mut GlyphCache, lib: &FontLibrary) -> bool {
        let (regular, medium, semibold, bold) = match (
            lib.pick("default", 400, false),
            lib.pick("default", 500, false),
            lib.pick("default", 600, false),
            lib.pick("default", 700, false),
        ) {
            (Some(a), Some(b), Some(c2), Some(d)) => (a, b, c2, d),
            _ => return false,
        };
        let centred = |c: &mut Canvas, g: &mut GlyphCache, f: &Font, s: &str, cx: i32, cy: i32, st: &TextStyle, col: Color32| {
            let w = measure(f, s, st) / 64;
            draw_text(c, g, f, s, cx - w / 2, f.baseline_for_centre(cy, st.size_q6), col, 255, st);
        };
        // Band 1: one caption per box (6 boxes, 146 px wide, 166 px pitch, y 20..90).
        let labels = ["0 px", "6 px", "16 px", "28 px", "pill", "mixed"];
        for (i, l) in labels.iter().enumerate() {
            centred(c, glyphs, semibold, l, 24 + i as i32 * 166 + 73, 55, &TextStyle::px(15), Color32::WHITE);
        }
        // Band 4: the glass card (155 wide at x = 635, y 378..472).
        centred(c, glyphs, bold, "Glass", 635 + 77, 425, &TextStyle::px(20), Color32::WHITE);
        // Taskbar clock in the tray pill (88x24 at 690,718): tabular digits keep it from jittering.
        centred(c, glyphs, medium, "12:34:56", 690 + 44, 730, &TextStyle::px(13).tabular(), Color32::rgb(235, 238, 250));
        // Specimen line in the gap between bands 5 and 6.
        draw_text(c, glyphs, regular, "Poppins: the quick brown fox jumps over the lazy dog 0123456789", 24, 604, Color32::rgba(255, 255, 255, 210), 255, &TextStyle::px(12));
        true
    }
}
