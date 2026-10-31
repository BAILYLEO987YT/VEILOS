//! Device-independent bitmaps: the formats a BITMAPINFO describes (1, 4,
//! 8, 16, 24 and 32 bits per pixel, bottom-up or top-down, color tables and
//! bit masks), DIB sections, copying DIBs to and from bitmaps and DCs, and
//! blending (GdiAlphaBlend, GdiTransparentBlt; msimg32 forwards to them).
//!
//! A 32bpp DIB section is drawn on in place. Other formats are read into
//! the bitmap's 32bpp pixels before GDI uses them and written back after
//! GDI draws on them.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use crate::draw::{colorref, mix3, pixel, source, span, step, Canvas};
use crate::{Bool, Handle, Obj, Table};

const BI_RGB: u32 = 0;
const BI_BITFIELDS: u32 = 3;
const DIB_RGB_COLORS: u32 = 0;

#[derive(Clone)]
pub struct Format {
    pub bpp: u16,
    pub w: i32,
    /// Rows (positive).
    pub h: i32,
    pub bottom_up: bool,
    /// Bytes per row.
    pub stride: usize,
    /// Red, green and blue masks (16 and 32 bits per pixel).
    pub masks: [u32; 3],
    /// Color table as pixels (0x00RRGGBB), for 1, 4 and 8 bits.
    pub palette: Vec<u32>,
}

/// The default 256-color palette for writing 8-bit DIBs from bitmaps that
/// have none: the 20 system colors, a 6x6x6 cube, then grays.
fn default_palette(n: usize) -> Vec<u32> {
    let mut p: Vec<u32> = Vec::with_capacity(256);
    if n <= 2 {
        return alloc::vec![0x000000, 0xFFFFFF][..n.max(1)].to_vec();
    }
    if n <= 16 {
        return [
            0x000000, 0x800000, 0x008000, 0x808000, 0x000080, 0x800080, 0x008080, 0xC0C0C0, 0x808080, 0xFF0000, 0x00FF00, 0xFFFF00,
            0x0000FF, 0xFF00FF, 0x00FFFF, 0xFFFFFF,
        ][..n]
            .to_vec();
    }
    for r in 0..6u32 {
        for g in 0..6u32 {
            for b in 0..6u32 {
                p.push((r * 51) << 16 | (g * 51) << 8 | b * 51);
            }
        }
    }
    let mut i = 0u32;
    while p.len() < 256 {
        let v = i * 255 / 39;
        p.push(v << 16 | v << 8 | v);
        i += 1;
    }
    p.truncate(n);
    p
}

fn shift_of(m: u32) -> u32 {
    if m == 0 { 0 } else { m.trailing_zeros() }
}

/// A channel through a mask, scaled to 0..=255.
fn channel(v: u32, m: u32) -> u32 {
    if m == 0 {
        return 0;
    }
    let max = m >> shift_of(m);
    ((v & m) >> shift_of(m)) * 255 / max
}

fn unchannel(c: u32, m: u32) -> u32 {
    if m == 0 {
        return 0;
    }
    let max = m >> shift_of(m);
    ((c * max + 127) / 255) << shift_of(m) & m
}

impl Format {
    /// The format a BITMAPINFO describes (`usage` DIB_PAL_COLORS: indices
    /// into the default palette).
    pub unsafe fn parse(bmi: *const u8, usage: u32) -> Option<Format> {
        if bmi.is_null() {
            return None;
        }
        let rd32 = |o: usize| unsafe { core::ptr::read_unaligned(bmi.add(o) as *const u32) };
        let rd16 = |o: usize| unsafe { core::ptr::read_unaligned(bmi.add(o) as *const u16) };
        let size = rd32(0) as usize;
        let (w, h, bpp, compression, used, header) = if size == 12 {
            // BITMAPCOREHEADER
            (rd16(4) as i32, rd16(6) as i16 as i32, rd16(10), BI_RGB, 0, 12)
        } else if size >= 40 {
            (rd32(4) as i32, rd32(8) as i32, rd16(14), rd32(16), rd32(32) as usize, size)
        } else {
            return None;
        };
        if w <= 0 || h == 0 || !matches!(bpp, 1 | 4 | 8 | 16 | 24 | 32) || w > 32768 || h.abs() > 32768 {
            return None;
        }
        let mut f = Format {
            bpp,
            w,
            h: h.abs(),
            bottom_up: h > 0,
            stride: ((w as usize * bpp as usize + 31) / 32) * 4,
            masks: [0; 3],
            palette: Vec::new(),
        };
        let table_at = header;
        match (bpp, compression) {
            (16, BI_RGB) => f.masks = [0x7C00, 0x03E0, 0x001F],
            (32, BI_RGB) | (24, _) => f.masks = [0xFF_0000, 0xFF00, 0xFF],
            (16 | 32, BI_BITFIELDS) => {
                if size >= 52 {
                    f.masks = [rd32(40), rd32(44), rd32(48)];
                } else {
                    f.masks = [rd32(header), rd32(header + 4), rd32(header + 8)];
                }
            }
            (1 | 4 | 8, BI_RGB) => {
                let n = if used == 0 || used > 1 << bpp { 1usize << bpp } else { used };
                if usage == DIB_RGB_COLORS {
                    let entry = if size == 12 { 3 } else { 4 };
                    for i in 0..n {
                        let q = unsafe { bmi.add(table_at + i * entry) };
                        let (b, g, r) = unsafe { (*q as u32, *q.add(1) as u32, *q.add(2) as u32) };
                        f.palette.push(r << 16 | g << 8 | b);
                    }
                } else {
                    let def = default_palette(256);
                    for i in 0..n {
                        let idx = unsafe { core::ptr::read_unaligned(bmi.add(table_at + i * 2) as *const u16) } as usize;
                        f.palette.push(def[idx & 0xFF]);
                    }
                }
            }
            // Compressed DIBs (RLE, JPEG, PNG) aren't read.
            _ => return None,
        }
        Some(f)
    }

    pub fn size(&self) -> usize {
        self.stride * self.h as usize
    }

    /// Scan line `y` counted from the top.
    pub fn row(&self, bits: *const u8, y: i32) -> *const u8 {
        let scan = if self.bottom_up { self.h - 1 - y } else { y };
        unsafe { bits.add(scan as usize * self.stride) }
    }

    pub fn get(&self, row: *const u8, x: i32) -> u32 {
        let x = x as usize;
        unsafe {
            match self.bpp {
                1 => self.pal((*row.add(x / 8) >> (7 - x % 8)) & 1),
                4 => self.pal((*row.add(x / 2) >> if x % 2 == 0 { 4 } else { 0 }) & 0xF),
                8 => self.pal(*row.add(x)),
                16 => {
                    let v = core::ptr::read_unaligned(row.add(x * 2) as *const u16) as u32;
                    channel(v, self.masks[0]) << 16 | channel(v, self.masks[1]) << 8 | channel(v, self.masks[2])
                }
                24 => {
                    let q = row.add(x * 3);
                    (*q.add(2) as u32) << 16 | (*q.add(1) as u32) << 8 | *q as u32
                }
                _ => {
                    let v = core::ptr::read_unaligned(row.add(x * 4) as *const u32);
                    if self.masks == [0xFF_0000, 0xFF00, 0xFF] {
                        v & 0x00FF_FFFF
                    } else {
                        channel(v, self.masks[0]) << 16 | channel(v, self.masks[1]) << 8 | channel(v, self.masks[2])
                    }
                }
            }
        }
    }

    fn pal(&self, i: u8) -> u32 {
        self.palette.get(i as usize).copied().unwrap_or(0)
    }

    /// The color table entry nearest pixel `p`.
    pub fn nearest(&self, p: u32, cache: &mut BTreeMap<u32, u8>) -> u8 {
        if let Some(&i) = cache.get(&p) {
            return i;
        }
        let d = |q: u32| {
            let dr = ((p >> 16) & 0xFF) as i32 - ((q >> 16) & 0xFF) as i32;
            let dg = ((p >> 8) & 0xFF) as i32 - ((q >> 8) & 0xFF) as i32;
            let db = (p & 0xFF) as i32 - (q & 0xFF) as i32;
            dr * dr + dg * dg + db * db
        };
        let i = self.palette.iter().enumerate().min_by_key(|(_, q)| d(**q)).map_or(0, |(i, _)| i as u8);
        cache.insert(p, i);
        i
    }

    pub fn put(&self, row: *mut u8, x: i32, p: u32, cache: &mut BTreeMap<u32, u8>) {
        let x = x as usize;
        let (r, g, b) = ((p >> 16) & 0xFF, (p >> 8) & 0xFF, p & 0xFF);
        unsafe {
            match self.bpp {
                1 => {
                    let bit = 0x80u8 >> (x % 8);
                    let q = row.add(x / 8);
                    if self.nearest(p, cache) & 1 != 0 { *q |= bit } else { *q &= !bit }
                }
                4 => {
                    let i = self.nearest(p, cache) & 0xF;
                    let q = row.add(x / 2);
                    *q = if x % 2 == 0 { (*q & 0x0F) | i << 4 } else { (*q & 0xF0) | i };
                }
                8 => *row.add(x) = self.nearest(p, cache),
                16 => {
                    let v = unchannel(r, self.masks[0]) | unchannel(g, self.masks[1]) | unchannel(b, self.masks[2]);
                    core::ptr::write_unaligned(row.add(x * 2) as *mut u16, v as u16);
                }
                24 => {
                    let q = row.add(x * 3);
                    *q = b as u8;
                    *q.add(1) = g as u8;
                    *q.add(2) = r as u8;
                }
                _ => {
                    let v = unchannel(r, self.masks[0]) | unchannel(g, self.masks[1]) | unchannel(b, self.masks[2]);
                    core::ptr::write_unaligned(row.add(x * 4) as *mut u32, v);
                }
            }
        }
    }

    /// 32 bits, standard masks: GDI's own pixel layout.
    pub fn direct(&self) -> bool {
        self.bpp == 32 && self.masks == [0xFF_0000, 0xFF00, 0xFF]
    }
}

/// A DIB section's bits (owned here, written by the program too).
pub struct Section {
    pub mem: Vec<u32>,
    pub fmt: Format,
}

impl Section {
    pub fn ptr(&self) -> *mut u8 {
        self.mem.as_ptr() as *mut u8
    }

    /// Bits into 32bpp top-down pixels.
    pub fn read_into(&self, out: &mut [u32]) {
        let f = &self.fmt;
        for y in 0..f.h {
            let row = f.row(self.ptr(), y);
            for x in 0..f.w {
                out[(y * f.w + x) as usize] = f.get(row, x);
            }
        }
    }

    /// 32bpp top-down pixels into the bits.
    pub fn write_from(&mut self, src: &[u32]) {
        let f = self.fmt.clone();
        let mut cache = BTreeMap::new();
        for y in 0..f.h {
            let row = f.row(self.ptr(), y) as *mut u8;
            for x in 0..f.w {
                f.put(row, x, src[(y * f.w + x) as usize], &mut cache);
            }
        }
    }
}

// -----| DIB sections |-----

#[no_mangle]
pub unsafe extern "system" fn CreateDIBSection(_hdc: Handle, bmi: *const u8, usage: u32, out: *mut *mut u8, _section: u64, _offset: u32) -> Handle {
    let Some(fmt) = (unsafe { Format::parse(bmi, usage) }) else {
        if !out.is_null() {
            unsafe { *out = core::ptr::null_mut() };
        }
        return 0;
    };
    let words = (fmt.size() + 3) / 4;
    let sec = Section { mem: alloc::vec![0; words], fmt };
    let ptr = sec.ptr();
    let (w, h, bpp) = (sec.fmt.w, sec.fmt.h, sec.fmt.bpp);
    let direct = sec.fmt.direct();
    let bm = crate::Bitmap {
        w,
        h,
        bpp,
        // Pixels for GDI only when the bits aren't GDI's own layout.
        bits: if direct { Vec::new() } else { alloc::vec![0; (w * h) as usize] },
        dc: 0,
        dib: Some(alloc::boxed::Box::new(sec)),
    };
    if !out.is_null() {
        unsafe { *out = ptr };
    }
    crate::table().add(Obj::Bitmap(bm))
}

#[no_mangle]
pub unsafe extern "system" fn GetDIBColorTable(hdc: Handle, start: u32, n: u32, out: *mut u8) -> u32 {
    let mut t = crate::table();
    let Some(b) = t.selected_bitmap(hdc) else { return 0 };
    let Some(Obj::Bitmap(bm)) = t.get(b) else { return 0 };
    let Some(sec) = bm.dib.as_ref() else { return 0 };
    let pal = &sec.fmt.palette;
    let mut k = 0;
    for i in start as usize..(start as usize + n as usize).min(pal.len()) {
        let p = pal[i];
        unsafe { *(out.add(k * 4) as *mut u32) = p & 0x00FF_FFFF };
        k += 1;
    }
    k as u32
}

#[no_mangle]
pub unsafe extern "system" fn SetDIBColorTable(hdc: Handle, start: u32, n: u32, colors: *const u8) -> u32 {
    let mut t = crate::table();
    let Some(b) = t.selected_bitmap(hdc) else { return 0 };
    let Some(Obj::Bitmap(bm)) = t.get(b) else { return 0 };
    let Some(sec) = bm.dib.as_mut() else { return 0 };
    let pal = &mut sec.fmt.palette;
    let mut k = 0;
    for i in start as usize..(start as usize + n as usize).min(pal.len()) {
        pal[i] = unsafe { *(colors.add(k * 4) as *const u32) } & 0x00FF_FFFF;
        k += 1;
    }
    k as u32
}

// -----| DIBs to and from bitmaps |-----

/// Copy DIB scan lines `start..start + lines` (counted from the bottom
/// for a bottom-up DIB) into bitmap `hbm`.
#[no_mangle]
pub unsafe extern "system" fn SetDIBits(_hdc: Handle, hbm: Handle, start: u32, lines: u32, bits: *const u8, bmi: *const u8, usage: u32) -> i32 {
    let Some(f) = (unsafe { Format::parse(bmi, usage) }) else { return 0 };
    if bits.is_null() {
        return 0;
    }
    let mut t = crate::table();
    let Some(Obj::Bitmap(bm)) = t.get(hbm) else { return 0 };
    let (ptr, stride) = bm.pixels();
    let c = Canvas { ptr, stride, x0: 0, y0: 0, x1: bm.w, y1: bm.h };
    let mut done = 0;
    for scan in start..(start + lines).min(f.h as u32) {
        let row = unsafe { bits.add((scan - start) as usize * f.stride) };
        let y = if f.bottom_up { f.h - 1 - scan as i32 } else { scan as i32 };
        for x in 0..f.w.min(bm.w) {
            c.put(x, y, f.get(row, x), crate::draw::R2_COPYPEN);
        }
        done += 1;
    }
    bm.written();
    done
}

/// Read bitmap `hbm` as a DIB. With `bits` null, only fill in the header
/// (its size, and with biBitCount 0 its format too).
#[no_mangle]
pub unsafe extern "system" fn GetDIBits(_hdc: Handle, hbm: Handle, start: u32, lines: u32, bits: *mut u8, bmi: *mut u8, usage: u32) -> i32 {
    if bmi.is_null() {
        return 0;
    }
    let mut t = crate::table();
    let Some(Obj::Bitmap(bm)) = t.get(hbm) else { return 0 };
    let wr32 = |o: usize, v: u32| unsafe { core::ptr::write_unaligned(bmi.add(o) as *mut u32, v) };
    let rd16 = |o: usize| unsafe { core::ptr::read_unaligned(bmi.add(o) as *const u16) };
    if bits.is_null() && rd16(14) == 0 {
        // The bitmap's own format.
        let bpp = if bm.bpp <= 8 && bm.dib.is_none() { 32 } else { bm.bpp };
        wr32(4, bm.w as u32);
        wr32(8, bm.h as u32);
        unsafe { core::ptr::write_unaligned(bmi.add(12) as *mut u16, 1) };
        unsafe { core::ptr::write_unaligned(bmi.add(14) as *mut u16, bpp) };
        // A 32bpp bitmap reports its masks, as Windows' do.
        if bpp == 32 {
            wr32(16, BI_BITFIELDS);
            for (i, m) in [0xFF_0000u32, 0xFF00, 0xFF].iter().enumerate() {
                wr32(40 + i * 4, *m);
            }
        } else {
            wr32(16, BI_RGB);
        }
        wr32(20, (((bm.w as u32 * bpp as u32 + 31) / 32) * 4) * bm.h as u32);
        return bm.h;
    }
    // The bitmap's color table, or the default one, for 8 bits and fewer.
    let bpp = rd16(14);
    if bpp <= 8 && matches!(bpp, 1 | 4 | 8) {
        let pal = match bm.dib.as_ref() {
            Some(s) if s.fmt.bpp == bpp => s.fmt.palette.clone(),
            _ => default_palette(1 << bpp),
        };
        for (i, p) in pal.iter().enumerate() {
            wr32(40 + i * 4, *p & 0x00FF_FFFF);
        }
        wr32(32, 0);
    }
    let Some(mut f) = (unsafe { Format::parse(bmi, usage) }) else { return 0 };
    wr32(20, f.size() as u32);
    if bits.is_null() {
        return bm.h;
    }
    f.h = f.h.min(bm.h);
    let (ptr, stride) = bm.pixels();
    let c = Canvas { ptr, stride, x0: 0, y0: 0, x1: bm.w, y1: bm.h };
    let mut cache = BTreeMap::new();
    let mut done = 0;
    for scan in start..(start + lines).min(f.h as u32) {
        let row = unsafe { bits.add((scan - start) as usize * f.stride) };
        let y = if f.bottom_up { f.h - 1 - scan as i32 } else { scan as i32 };
        for x in 0..f.w.min(bm.w) {
            f.put(row, x, c.get(x, y).unwrap_or(0), &mut cache);
        }
        done += 1;
    }
    done
}

#[no_mangle]
pub unsafe extern "system" fn CreateDIBitmap(hdc: Handle, bih: *const u8, init: u32, bits: *const u8, bmi: *const u8, usage: u32) -> Handle {
    if bih.is_null() {
        return 0;
    }
    let w = unsafe { core::ptr::read_unaligned(bih.add(4) as *const i32) };
    let h = unsafe { core::ptr::read_unaligned(bih.add(8) as *const i32) };
    let hbm = crate::CreateCompatibleBitmap(hdc, w, h.abs());
    if init & 4 != 0 && !bits.is_null() {
        unsafe { SetDIBits(hdc, hbm, 0, h.unsigned_abs(), bits, bmi, usage) };
    }
    hbm
}

/// GetBitmapBits/SetBitmapBits: the bitmap's rows as 32bpp, top-down.
#[no_mangle]
pub unsafe extern "system" fn GetBitmapBits(hbm: Handle, size: i32, out: *mut u8) -> i32 {
    let mut t = crate::table();
    let Some(Obj::Bitmap(bm)) = t.get(hbm) else { return 0 };
    let (ptr, stride) = bm.pixels();
    let c = Canvas { ptr, stride, x0: 0, y0: 0, x1: bm.w, y1: bm.h };
    let n = (size.max(0) as usize / 4).min((bm.w * bm.h) as usize);
    for i in 0..n {
        let (x, y) = (i as i32 % bm.w, i as i32 / bm.w);
        unsafe { *(out.add(i * 4) as *mut u32) = c.get(x, y).unwrap_or(0) };
    }
    (n * 4) as i32
}

#[no_mangle]
pub unsafe extern "system" fn SetBitmapBits(hbm: Handle, size: u32, src: *const u8) -> i32 {
    let mut t = crate::table();
    let Some(Obj::Bitmap(bm)) = t.get(hbm) else { return 0 };
    let (ptr, stride) = bm.pixels();
    let c = Canvas { ptr, stride, x0: 0, y0: 0, x1: bm.w, y1: bm.h };
    let n = (size as usize / 4).min((bm.w * bm.h) as usize);
    for i in 0..n {
        let (x, y) = (i as i32 % bm.w, i as i32 / bm.w);
        c.put(x, y, unsafe { *(src.add(i * 4) as *const u32) } & 0x00FF_FFFF, crate::draw::R2_COPYPEN);
    }
    bm.written();
    (n * 4) as i32
}

// -----| DIBs to DCs |-----

/// Draw a DIB's source rectangle (top-down coordinates) stretched onto a
/// DC's destination rectangle (negative sizes mirror).
#[allow(clippy::too_many_arguments)]
fn dib_to_dc(t: &mut Table, hdc: Handle, f: &Format, src: impl Fn(i32, i32) -> Option<u32>,
             dx: i32, dy: i32, dw: i32, dh: i32, sx: i32, sy: i32, sw: i32, sh: i32, rop: u32) -> bool {
    let Some(c) = t.canvas(hdc) else { return t.is_dc(hdc) };
    let (x0, y0) = crate::to_device(t, hdc, dx, dy);
    let pat = t.dc_brush(hdc).unwrap_or(0);
    let code = (rop >> 16) as u8;
    let ((xa, xb), (ya, yb)) = (span(x0, dw), span(y0, dh));
    for y in ya.max(c.y0)..yb.min(c.y1) {
        let syy = source(sy, sh, dh, step(y0, dh, y));
        if syy < 0 || syy >= f.h {
            continue;
        }
        for x in xa.max(c.x0)..xb.min(c.x1) {
            let sxx = source(sx, sw, dw, step(x0, dw, x));
            if sxx < 0 || sxx >= f.w {
                continue;
            }
            let Some(s) = src(sxx, syy) else { continue };
            let at = c.at(x, y);
            unsafe { *at = mix3(code, pat, s, *at) };
        }
    }
    t.done(hdc);
    true
}

#[no_mangle]
pub unsafe extern "system" fn SetDIBitsToDevice(hdc: Handle, dx: i32, dy: i32, w: u32, h: u32, sx: i32, sy: i32,
                                                start: u32, lines: u32, bits: *const u8, bmi: *const u8, usage: u32) -> i32 {
    let Some(f) = (unsafe { Format::parse(bmi, usage) }) else { return 0 };
    if bits.is_null() {
        return 0;
    }
    let (start, lines) = (start as i32, lines as i32);
    // Pixel (x, top-down y) of the DIB, if its scan line was given.
    let src = |x: i32, y: i32| {
        let scan = if f.bottom_up { f.h - 1 - y } else { y };
        if scan < start || scan >= start + lines {
            return None;
        }
        let row = unsafe { bits.add((scan - start) as usize * f.stride) };
        Some(f.get(row, x))
    };
    // A bottom-up DIB's source origin is its lower-left corner.
    let top = if f.bottom_up { f.h - sy - h as i32 } else { sy };
    let mut t = crate::table();
    if !dib_to_dc(&mut t, hdc, &f, src, dx, dy, w as i32, h as i32, sx, top, w as i32, h as i32, 0x00CC_0020) {
        return 0;
    }
    lines
}

#[no_mangle]
pub unsafe extern "system" fn StretchDIBits(hdc: Handle, dx: i32, dy: i32, dw: i32, dh: i32, sx: i32, sy: i32, sw: i32, sh: i32,
                                            bits: *const u8, bmi: *const u8, usage: u32, rop: u32) -> i32 {
    let Some(f) = (unsafe { Format::parse(bmi, usage) }) else { return 0 };
    if bits.is_null() {
        return 0;
    }
    let src = |x: i32, y: i32| Some(f.get(f.row(bits, y), x));
    let top = if f.bottom_up { f.h - sy - sh.abs() } else { sy };
    let (top, sh) = if sh < 0 { (top + sh.abs() - 1, sh) } else { (top, sh) };
    let mut t = crate::table();
    if !dib_to_dc(&mut t, hdc, &f, src, dx, dy, dw, dh, sx, top, sw, sh, rop) {
        return 0;
    }
    // The scan lines copied.
    sh.abs()
}

// -----| blending |-----

/// AlphaBlend: BLENDFUNCTION { op, flags, SourceConstantAlpha, AlphaFormat }
/// packed in a DWORD; AC_SRC_ALPHA uses the source's (premultiplied) alpha.
#[no_mangle]
pub extern "system" fn GdiAlphaBlend(dst: Handle, dx: i32, dy: i32, dw: i32, dh: i32, src: Handle, sx: i32, sy: i32, sw: i32, sh: i32, blend: u32) -> Bool {
    let constant = (blend >> 16) & 0xFF;
    let per_pixel = (blend >> 24) & 1 != 0;
    let mut t = crate::table();
    let (Some(dc), Some(sc)) = (t.canvas(dst), t.canvas(src)) else { return 0 };
    if dw <= 0 || dh <= 0 || sw <= 0 || sh <= 0 {
        return 0;
    }
    let (x0, y0) = crate::to_device(&mut t, dst, dx, dy);
    let (sx0, sy0) = crate::to_device(&mut t, src, sx, sy);
    for y in y0.max(dc.y0)..(y0 + dh).min(dc.y1) {
        let ry = sy0 + (y - y0) * sh / dh;
        for x in x0.max(dc.x0)..(x0 + dw).min(dc.x1) {
            let rx = sx0 + (x - x0) * sw / dw;
            if !sc.contains(rx, ry) {
                continue;
            }
            let s = unsafe { *sc.at(rx, ry) };
            let d = unsafe { *dc.at(x, y) };
            let sa = if per_pixel { (s >> 24) * constant / 255 } else { constant };
            let mut out = 0u32;
            for sh_ in [0, 8, 16, 24] {
                let sv = if sh_ == 24 && !per_pixel { constant } else { ((s >> sh_) & 0xFF) * constant / 255 };
                let dv = (d >> sh_) & 0xFF;
                out |= (sv + dv * (255 - sa) / 255).min(255) << sh_;
            }
            unsafe { *dc.at(x, y) = out };
        }
    }
    t.done(dst);
    1
}

/// TransparentBlt: copy (stretching) all but one color.
#[no_mangle]
pub extern "system" fn GdiTransparentBlt(dst: Handle, dx: i32, dy: i32, dw: i32, dh: i32, src: Handle, sx: i32, sy: i32, sw: i32, sh: i32, key: u32) -> Bool {
    let mut t = crate::table();
    let (Some(dc), Some(sc)) = (t.canvas(dst), t.canvas(src)) else { return 0 };
    if dw <= 0 || dh <= 0 || sw <= 0 || sh <= 0 {
        return 0;
    }
    let key = pixel(key);
    let (x0, y0) = crate::to_device(&mut t, dst, dx, dy);
    let (sx0, sy0) = crate::to_device(&mut t, src, sx, sy);
    for y in y0.max(dc.y0)..(y0 + dh).min(dc.y1) {
        let ry = sy0 + (y - y0) * sh / dh;
        for x in x0.max(dc.x0)..(x0 + dw).min(dc.x1) {
            let rx = sx0 + (x - x0) * sw / dw;
            if let Some(p) = sc.get(rx, ry) {
                if p != key {
                    unsafe { *dc.at(x, y) = p };
                }
            }
        }
    }
    t.done(dst);
    1
}

// -----| palettes (8-bit displays are emulated: a palette is a color list) |-----

/// LOGPALETTE: version, count, then PALETTEENTRY { r, g, b, flags }.
#[no_mangle]
pub unsafe extern "system" fn CreatePalette(lp: *const u8) -> Handle {
    if lp.is_null() {
        return 0;
    }
    let n = unsafe { core::ptr::read_unaligned(lp.add(2) as *const u16) } as usize;
    let mut colors = Vec::with_capacity(n);
    for i in 0..n {
        let e = unsafe { lp.add(4 + i * 4) };
        let (r, g, b) = unsafe { (*e as u32, *e.add(1) as u32, *e.add(2) as u32) };
        colors.push(r << 16 | g << 8 | b);
    }
    crate::table().add(Obj::Palette(colors))
}

#[no_mangle]
pub extern "system" fn SelectPalette(hdc: Handle, pal: Handle, _background: Bool) -> Handle {
    let mut t = crate::table();
    if t.get(pal).is_none() {
        return 0;
    }
    t.dc_palette(hdc, pal)
}

#[no_mangle]
pub extern "system" fn RealizePalette(_hdc: Handle) -> u32 {
    0
}

fn palette_entries(h: Handle, start: u32, n: u32, out: *mut u8, set: Option<*const u8>) -> u32 {
    let mut t = crate::table();
    let Some(Obj::Palette(colors)) = t.get(h) else { return 0 };
    if out.is_null() && set.is_none() {
        return colors.len() as u32;
    }
    let mut k = 0u32;
    for i in start as usize..(start as usize + n as usize).min(colors.len()) {
        unsafe {
            match set {
                Some(src) => {
                    let e = src.add(k as usize * 4);
                    colors[i] = (*e as u32) << 16 | (*e.add(1) as u32) << 8 | *e.add(2) as u32;
                }
                None => {
                    let e = out.add(k as usize * 4);
                    let c = colors[i];
                    *e = (c >> 16) as u8;
                    *e.add(1) = (c >> 8) as u8;
                    *e.add(2) = c as u8;
                    *e.add(3) = 0;
                }
            }
        }
        k += 1;
    }
    k
}

#[no_mangle]
pub extern "system" fn GetPaletteEntries(pal: Handle, start: u32, n: u32, out: *mut u8) -> u32 {
    palette_entries(pal, start, n, out, None)
}

#[no_mangle]
pub extern "system" fn SetPaletteEntries(pal: Handle, start: u32, n: u32, src: *const u8) -> u32 {
    palette_entries(pal, start, n, core::ptr::null_mut(), Some(src))
}

#[no_mangle]
pub extern "system" fn GetSystemPaletteEntries(_hdc: Handle, start: u32, n: u32, out: *mut u8) -> u32 {
    let def = default_palette(256);
    let mut k = 0;
    for i in start as usize..(start as usize + n as usize).min(256) {
        if !out.is_null() {
            let c = def[i];
            unsafe {
                *out.add(k * 4) = (c >> 16) as u8;
                *out.add(k * 4 + 1) = (c >> 8) as u8;
                *out.add(k * 4 + 2) = c as u8;
                *out.add(k * 4 + 3) = 0;
            }
        }
        k += 1;
    }
    k as u32
}

#[no_mangle]
pub extern "system" fn GetNearestPaletteIndex(pal: Handle, color: u32) -> u32 {
    let mut t = crate::table();
    let Some(Obj::Palette(colors)) = t.get(pal) else { return 0xFFFF_FFFF };
    let f = Format { bpp: 8, w: 1, h: 1, bottom_up: false, stride: 4, masks: [0; 3], palette: colors.clone() };
    f.nearest(pixel(color), &mut BTreeMap::new()) as u32
}

/// A true-color display: every color is available.
#[no_mangle]
pub extern "system" fn GetNearestColor(_hdc: Handle, color: u32) -> u32 {
    colorref(pixel(color))
}

pub fn stock_palette() -> Vec<u32> {
    default_palette(16)
}
