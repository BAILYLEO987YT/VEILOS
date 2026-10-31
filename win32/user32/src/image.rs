//! Bitmaps, icons and cursors: from a module's resources (RT_BITMAP,
//! RT_GROUP_ICON/RT_ICON, RT_GROUP_CURSOR/RT_CURSOR), from .bmp/.ico/.cur
//! files, and the standard ones (IDI_*, IDC_*), drawn here.
//!
//! An icon or cursor is kept as 32bpp straight-alpha pixels; one without
//! an alpha channel gets its alpha from its AND mask. DrawIconEx blends it
//! through gdi32's GdiAlphaBlend. (The desktop draws its own pointer, so a
//! cursor is a handle programs can query and draw, not yet the pointer.)

use alloc::vec::Vec;

use crate::dllrt::Lock;
use crate::Bool;

#[link(name = "kernel32", kind = "raw-dylib")]
unsafe extern "system" {
    fn FindResourceW(module: u64, name: *const u16, kind: *const u16) -> u64;
    fn LoadResource(module: u64, res: u64) -> u64;
    fn LockResource(data: u64) -> *const u8;
    fn SizeofResource(module: u64, res: u64) -> u32;
    fn CreateFileW(name: *const u16, access: u32, share: u32, sa: usize, disposition: u32, flags: u32, template: u64) -> u64;
    fn GetFileSizeEx(file: u64, size: *mut i64) -> i32;
    fn ReadFile(file: u64, buf: *mut u8, n: u32, read: *mut u32, ov: usize) -> i32;
    fn CloseHandle(h: u64) -> i32;
    fn GetModuleHandleW(name: *const u16) -> u64;
}

#[link(name = "gdi32", kind = "raw-dylib")]
unsafe extern "system" {
    fn CreateDIBSection(hdc: u64, bmi: *const u8, usage: u32, bits: *mut *mut u8, section: u64, offset: u32) -> u64;
    fn CreateCompatibleBitmap(hdc: u64, w: i32, h: i32) -> u64;
    fn CreateBitmap(w: i32, h: i32, planes: u32, bpp: u32, bits: *const u8) -> u64;
    fn SetDIBits(hdc: u64, hbm: u64, start: u32, lines: u32, bits: *const u8, bmi: *const u8, usage: u32) -> i32;
    fn GetDIBits(hdc: u64, hbm: u64, start: u32, lines: u32, bits: *mut u8, bmi: *mut u8, usage: u32) -> i32;
    fn CreateCompatibleDC(hdc: u64) -> u64;
    fn DeleteObject(h: u64) -> Bool;
    fn GetObjectW(h: u64, size: i32, out: *mut u8) -> i32;
    fn GdiAlphaBlend(dst: u64, x: i32, y: i32, w: i32, h: i32, src: u64, sx: i32, sy: i32, sw: i32, sh: i32, blend: u32) -> Bool;
    fn StretchBlt(dst: u64, x: i32, y: i32, w: i32, h: i32, src: u64, sx: i32, sy: i32, sw: i32, sh: i32, rop: u32) -> Bool;
}

const RT_CURSOR: u16 = 1;
const RT_BITMAP: u16 = 2;
const RT_ICON: u16 = 3;
const RT_GROUP_CURSOR: u16 = 12;
const RT_GROUP_ICON: u16 = 14;

const IMAGE_BITMAP: u32 = 0;
const IMAGE_ICON: u32 = 1;
const IMAGE_CURSOR: u32 = 2;
const LR_LOADFROMFILE: u32 = 0x10;
const LR_DEFAULTSIZE: u32 = 0x40;
const LR_CREATEDIBSECTION: u32 = 0x2000;

pub struct Icon {
    pub w: i32,
    pub h: i32,
    pub hotspot: (i32, i32),
    pub cursor: bool,
    /// Top-down 0xAARRGGBB, straight alpha.
    pub argb: Vec<u32>,
    /// Shared (standard and LR_SHARED images): DestroyIcon leaves it.
    pub shared: bool,
}

static ICONS: Lock<Vec<Option<Icon>>> = Lock::new(Vec::new());
/// Standard icons and cursors made so far: (id, cursor, handle).
static STANDARD: Lock<Vec<(u16, bool, u64)>> = Lock::new(Vec::new());

const ICON_BASE: u64 = 0x0004_0000;

fn add(icon: Icon) -> u64 {
    let mut v = ICONS.lock();
    let i = match v.iter().position(|e| e.is_none()) {
        Some(i) => i,
        None => {
            v.push(None);
            v.len() - 1
        }
    };
    v[i] = Some(icon);
    ICON_BASE + 4 * i as u64
}

fn with_icon<R>(h: u64, f: impl FnOnce(&Icon) -> R) -> Option<R> {
    let v = ICONS.lock();
    let i = (h.checked_sub(ICON_BASE)? / 4) as usize;
    v.get(i)?.as_ref().map(f)
}

// -----| resources and files |-----

fn res_bytes(module: u64, name: *const u16, kind: u16) -> Option<&'static [u8]> {
    let module = if module == 0 { unsafe { GetModuleHandleW(core::ptr::null()) } } else { module };
    let res = unsafe { FindResourceW(module, name, kind as usize as *const u16) };
    if res == 0 {
        return None;
    }
    let size = unsafe { SizeofResource(module, res) } as usize;
    let p = unsafe { LockResource(LoadResource(module, res)) };
    (!p.is_null()).then(|| unsafe { core::slice::from_raw_parts(p, size) })
}

fn read_file(path: *const u16) -> Option<Vec<u8>> {
    let h = unsafe { CreateFileW(path, 0x8000_0000, 1, 0, 3, 0, 0) };
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

fn u16_at(d: &[u8], o: usize) -> u16 {
    d.get(o..o + 2).map_or(0, |b| u16::from_le_bytes([b[0], b[1]]))
}

fn u32_at(d: &[u8], o: usize) -> u32 {
    d.get(o..o + 4).map_or(0, |b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

/// A name argument as UTF-16 (or the resource id it is).
unsafe fn name_a(p: *const u8) -> (Vec<u16>, *const u16) {
    if (p as usize) >> 16 == 0 {
        return (Vec::new(), p as *const u16);
    }
    let mut w = unsafe { crate::from_ansi(p, -1) };
    w.push(0);
    let ptr = w.as_ptr();
    (w, ptr)
}

// -----| decoding images |-----

/// A BITMAPINFOHEADER image (an icon's XOR part, `rows` high) at `d[at..]`
/// as top-down 0xAARRGGBB, and where its pixels end.
fn decode_dib(d: &[u8], rows: i32) -> Option<(i32, i32, Vec<u32>, usize, bool)> {
    let size = u32_at(d, 0) as usize;
    if size < 40 {
        return None;
    }
    let w = u32_at(d, 4) as i32;
    let h = rows;
    let bpp = u16_at(d, 14) as usize;
    let used = u32_at(d, 32) as usize;
    if w <= 0 || h <= 0 || w > 1024 || h > 1024 || !matches!(bpp, 1 | 4 | 8 | 24 | 32) {
        return None;
    }
    let colors = if bpp <= 8 { if used == 0 { 1 << bpp } else { used } } else { 0 };
    let pal_at = size + if u32_at(d, 16) == 3 { 12 } else { 0 };
    let pal: Vec<u32> = (0..colors).map(|i| u32_at(d, pal_at + i * 4) & 0x00FF_FFFF).collect();
    let bits = pal_at + colors * 4;
    let stride = (w as usize * bpp).div_ceil(32) * 4;
    if d.len() < bits + stride * h as usize {
        return None;
    }
    let mut out = alloc::vec![0u32; (w * h) as usize];
    let mut any_alpha = false;
    for y in 0..h as usize {
        let row = &d[bits + (h as usize - 1 - y) * stride..];
        for x in 0..w as usize {
            let p = match bpp {
                1 => pal.get(((row[x / 8] >> (7 - x % 8)) & 1) as usize).copied().unwrap_or(0) | 0xFF00_0000,
                4 => pal.get(((row[x / 2] >> if x % 2 == 0 { 4 } else { 0 }) & 0xF) as usize).copied().unwrap_or(0) | 0xFF00_0000,
                8 => pal.get(row[x] as usize).copied().unwrap_or(0) | 0xFF00_0000,
                24 => 0xFF00_0000 | (row[x * 3 + 2] as u32) << 16 | (row[x * 3 + 1] as u32) << 8 | row[x * 3] as u32,
                _ => {
                    let v = u32::from_le_bytes([row[x * 4], row[x * 4 + 1], row[x * 4 + 2], row[x * 4 + 3]]);
                    any_alpha |= v >> 24 != 0;
                    v
                }
            };
            out[y * w as usize + x] = p;
        }
    }
    Some((w, h, out, bits + stride * h as usize, bpp == 32 && any_alpha))
}

fn decode_png(d: &[u8]) -> Option<(i32, i32, Vec<u32>)> {
    use zune_png::zune_core::bytestream::ZCursor;
    use zune_png::zune_core::colorspace::ColorSpace;
    use zune_png::PngDecoder;
    let mut dec = PngDecoder::new(ZCursor::new(d));
    let px = dec.decode_raw().ok()?;
    let (w, h) = dec.dimensions()?;
    let ch = match dec.colorspace()? {
        ColorSpace::RGB => 3,
        ColorSpace::RGBA => 4,
        ColorSpace::Luma => 1,
        ColorSpace::LumaA => 2,
        _ => return None,
    };
    if px.len() < w * h * ch {
        return None;
    }
    let out = px
        .chunks(ch)
        .take(w * h)
        .map(|c| {
            let (r, g, b, a) = match ch {
                1 => (c[0], c[0], c[0], 255),
                2 => (c[0], c[0], c[0], c[1]),
                3 => (c[0], c[1], c[2], 255),
                _ => (c[0], c[1], c[2], c[3]),
            };
            (a as u32) << 24 | (r as u32) << 16 | (g as u32) << 8 | b as u32
        })
        .collect();
    Some((w as i32, h as i32, out))
}

/// One icon or cursor image (an RT_ICON / RT_CURSOR resource, or the
/// data an .ico entry points at).
fn decode_icon_image(d: &[u8], cursor: bool) -> Option<Icon> {
    let (hot, d) = if cursor && d.len() > 4 && &d[4..8] != b"\x89PNG" {
        ((u16_at(d, 0) as i32, u16_at(d, 2) as i32), &d[4..])
    } else {
        ((0, 0), d)
    };
    if d.starts_with(b"\x89PNG") {
        let (w, h, argb) = decode_png(d)?;
        return Some(Icon { w, h, hotspot: hot, cursor, argb, shared: false });
    }
    // XOR image (half the header's height), then the AND mask.
    let h = (u32_at(d, 8) as i32) / 2;
    let (w, h, mut argb, end, alpha) = decode_dib(d, h)?;
    if !alpha {
        let stride = (w as usize).div_ceil(32) * 4;
        for y in 0..h as usize {
            let row = end + (h as usize - 1 - y) * stride;
            for x in 0..w as usize {
                let masked = d.get(row + x / 8).is_some_and(|b| b & (0x80 >> (x % 8)) != 0);
                let p = &mut argb[y * w as usize + x];
                *p = if masked { 0 } else { *p | 0xFF00_0000 };
            }
        }
    }
    Some(Icon { w, h, hotspot: hot, cursor, argb, shared: false })
}

/// The entry of an icon/cursor directory (resource group or .ico file)
/// best for `cx` x `cy`: its index.
fn best_entry(sizes: &[(i32, i32, u16)], cx: i32, cy: i32) -> usize {
    let (cx, cy) = (if cx == 0 { 32 } else { cx }, if cy == 0 { 32 } else { cy });
    (0..sizes.len())
        .min_by_key(|&i| {
            let (w, h, bpp) = sizes[i];
            // Exact size first (a larger one shrinks better than a smaller
            // one grows), then the most colors.
            let d = (w - cx).abs() + (h - cy).abs();
            let smaller = (w < cx) as i32 * 1000;
            (d * 4 + smaller) * 64 - bpp as i32
        })
        .unwrap_or(0)
}

fn dir_size(b: u8) -> i32 {
    if b == 0 { 256 } else { b as i32 }
}

/// An icon or cursor from a module's resource group.
fn load_resource_icon(module: u64, name: *const u16, cursor: bool, cx: i32, cy: i32) -> Option<Icon> {
    let group = res_bytes(module, name, if cursor { RT_GROUP_CURSOR } else { RT_GROUP_ICON })?;
    let n = u16_at(group, 4) as usize;
    // GRPICONDIRENTRY (14 bytes): width, height, colors, reserved, planes,
    // bit count, size, id. A cursor's width/height are u16s, height doubled.
    let sizes: Vec<(i32, i32, u16)> = (0..n)
        .map(|i| {
            let e = 6 + i * 14;
            if cursor {
                (u16_at(group, e) as i32, u16_at(group, e + 2) as i32 / 2, u16_at(group, e + 6))
            } else {
                (dir_size(group[e]), dir_size(group[e + 1]), u16_at(group, e + 6))
            }
        })
        .collect();
    let e = 6 + best_entry(&sizes, cx, cy) * 14;
    let id = u16_at(group, e + 12);
    let data = res_bytes(module, id as usize as *const u16, if cursor { RT_CURSOR } else { RT_ICON })?;
    decode_icon_image(data, cursor)
}

/// An icon or cursor from an .ico / .cur file's data.
fn load_file_icon(d: &[u8], cursor: bool, cx: i32, cy: i32) -> Option<Icon> {
    let n = u16_at(d, 4) as usize;
    let sizes: Vec<(i32, i32, u16)> = (0..n).map(|i| (dir_size(d[6 + i * 16]), dir_size(d[7 + i * 16]), u16_at(d, 6 + i * 16 + 6))).collect();
    let e = 6 + best_entry(&sizes, cx, cy) * 16;
    let (size, at) = (u32_at(d, e + 8) as usize, u32_at(d, e + 12) as usize);
    let img = d.get(at..at + size)?;
    let mut icon = decode_icon_image(img, false)?;
    if cursor {
        icon.cursor = true;
        icon.hotspot = (u16_at(d, e + 4) as i32, u16_at(d, e + 6) as i32);
    }
    Some(icon)
}

// -----| the standard icons and cursors |-----

/// Draw a standard icon (IDI_*) or cursor (IDC_*) image, 32 x 32.
fn standard(id: u16, cursor: bool) -> Icon {
    let mut px = alloc::vec![0u32; 32 * 32];
    let mut put = |x: i32, y: i32, c: u32| {
        if (0..32).contains(&x) && (0..32).contains(&y) {
            px[(y * 32 + x) as usize] = c;
        }
    };
    let mut hot = (0, 0);
    if cursor {
        // An arrow (every cursor looks like one for now).
        for y in 0..21i32 {
            for x in 0..=y.min(12) {
                let edge = x == 0 || x == y.min(12) || y == 20;
                put(x, y, if edge { 0xFF00_0000 } else { 0xFFFF_FFFF });
            }
        }
        if matches!(id, 32513 | 32515) {
            hot = (8, 8);
        }
    } else {
        let disc = |put: &mut dyn FnMut(i32, i32, u32), c: u32| {
            for y in 0..32 {
                for x in 0..32 {
                    let (dx, dy) = (x * 2 - 31, y * 2 - 31);
                    if dx * dx + dy * dy <= 30 * 30 {
                        put(x, y, c);
                    }
                }
            }
        };
        match id {
            // IDI_ERROR: a red disc with a white cross.
            32513 => {
                disc(&mut put, 0xFFD8_3B3B);
                for i in 9..23 {
                    for t in 0..3 {
                        put(i + t - 1, i, 0xFFFF_FFFF);
                        put(31 - i - t + 1, i, 0xFFFF_FFFF);
                    }
                }
            }
            // IDI_INFORMATION: a blue disc with an "i".
            32516 => {
                disc(&mut put, 0xFF1E_6FD9);
                for (y0, y1) in [(7, 11), (13, 25)] {
                    for y in y0..y1 {
                        for x in 14..18 {
                            put(x, y, 0xFFFF_FFFF);
                        }
                    }
                }
            }
            // IDI_QUESTION: a blue disc with a "?".
            32514 => {
                disc(&mut put, 0xFF1E_6FD9);
                let strokes = [(11, 21, 6, 9), (18, 21, 6, 15), (14, 21, 12, 15), (14, 18, 12, 20), (14, 18, 22, 26)];
                for (x0, x1, y0, y1) in strokes {
                    for y in y0..y1 {
                        for x in x0..x1 {
                            put(x, y, 0xFFFF_FFFF);
                        }
                    }
                }
            }
            // IDI_WARNING: a yellow triangle with a black mark.
            32515 => {
                for y in 2..30 {
                    let half = (y - 2) / 2 + 1;
                    for x in 16 - half..16 + half {
                        put(x, y, 0xFFF5_C518);
                    }
                }
                for y in 10..22 {
                    for x in 14..18 {
                        put(x, y, 0xFF00_0000);
                    }
                }
                for y in 24..27 {
                    for x in 14..18 {
                        put(x, y, 0xFF00_0000);
                    }
                }
            }
            // IDI_APPLICATION and the rest: a window.
            _ => {
                for y in 3..29 {
                    for x in 2..30 {
                        let border = x == 2 || x == 29 || y == 3 || y == 28;
                        put(x, y, if border { 0xFF40_4040 } else if y < 9 { 0xFF1E_6FD9 } else { 0xFFFF_FFFF });
                    }
                }
            }
        }
    }
    Icon { w: 32, h: 32, hotspot: hot, cursor, argb: px, shared: true }
}

fn standard_handle(id: u16, cursor: bool) -> u64 {
    // Aliases: IDI_HAND = IDI_ERROR, and so on, are the same numbers.
    if let Some(&(_, _, h)) = STANDARD.lock().iter().find(|e| e.0 == id && e.1 == cursor) {
        return h;
    }
    let h = add(standard(id, cursor));
    STANDARD.lock().push((id, cursor, h));
    h
}

// -----| loading |-----

fn load_icon(module: u64, name: *const u16, cursor: bool, cx: i32, cy: i32) -> u64 {
    if module == 0 && (name as usize) >> 16 == 0 {
        return standard_handle(name as usize as u16, cursor);
    }
    match load_resource_icon(module, name, cursor, cx, cy) {
        Some(mut icon) => {
            icon.shared = true;
            add(icon)
        }
        None => {
            crate::set_error(1813); // ERROR_RESOURCE_TYPE_NOT_FOUND
            0
        }
    }
}

#[no_mangle]
pub extern "system" fn LoadIconW(module: u64, name: *const u16) -> u64 {
    load_icon(module, name, false, 32, 32)
}

#[no_mangle]
pub unsafe extern "system" fn LoadIconA(module: u64, name: *const u8) -> u64 {
    let (_keep, w) = unsafe { name_a(name) };
    load_icon(module, w, false, 32, 32)
}

#[no_mangle]
pub extern "system" fn LoadCursorW(module: u64, name: *const u16) -> u64 {
    load_icon(module, name, true, 32, 32)
}

#[no_mangle]
pub unsafe extern "system" fn LoadCursorA(module: u64, name: *const u8) -> u64 {
    let (_keep, w) = unsafe { name_a(name) };
    load_icon(module, w, true, 32, 32)
}

#[no_mangle]
pub extern "system" fn LoadCursorFromFileW(path: *const u16) -> u64 {
    read_file(path).and_then(|d| load_file_icon(&d, true, 32, 32)).map_or(0, add)
}

/// A packed DIB (BITMAPINFO then bits) as a bitmap: a DDB, or a DIB
/// section in the DIB's own format.
fn bitmap_from_dib(d: &[u8], dib_section: bool) -> u64 {
    let size = u32_at(d, 0) as usize;
    let bpp = u16_at(d, 14) as usize;
    let used = u32_at(d, 32) as usize;
    let colors = if bpp <= 8 { if used == 0 { 1 << bpp } else { used } } else { 0 };
    let masks = if size == 40 && u32_at(d, 16) == 3 { 12 } else { 0 };
    let bits_at = size + masks + colors * 4;
    if size < 12 || d.len() < bits_at {
        return 0;
    }
    let (w, h) = (u32_at(d, 4) as i32, u32_at(d, 8) as i32);
    unsafe {
        if dib_section {
            let mut bits: *mut u8 = core::ptr::null_mut();
            let hbm = CreateDIBSection(0, d.as_ptr(), 0, &mut bits, 0, 0);
            if hbm != 0 && !bits.is_null() {
                let stride = (w as usize * bpp).div_ceil(32) * 4;
                let n = (stride * h.unsigned_abs() as usize).min(d.len() - bits_at);
                core::ptr::copy_nonoverlapping(d.as_ptr().add(bits_at), bits, n);
            }
            hbm
        } else {
            let hbm = CreateCompatibleBitmap(0, w, h.abs());
            SetDIBits(0, hbm, 0, h.unsigned_abs(), d.as_ptr().add(bits_at), d.as_ptr(), 0);
            hbm
        }
    }
}

#[no_mangle]
pub extern "system" fn LoadBitmapW(module: u64, name: *const u16) -> u64 {
    if module == 0 {
        // OBM_* system bitmaps aren't drawn yet.
        return 0;
    }
    res_bytes(module, name, RT_BITMAP).map_or(0, |d| bitmap_from_dib(d, false))
}

#[no_mangle]
pub unsafe extern "system" fn LoadBitmapA(module: u64, name: *const u8) -> u64 {
    let (_keep, w) = unsafe { name_a(name) };
    LoadBitmapW(module, w)
}

#[no_mangle]
pub extern "system" fn LoadImageW(module: u64, name: *const u16, kind: u32, cx: i32, cy: i32, flags: u32) -> u64 {
    let (cx, cy) = if (cx == 0 || cy == 0) && flags & LR_DEFAULTSIZE != 0 && kind != IMAGE_BITMAP { (32, 32) } else { (cx, cy) };
    if flags & LR_LOADFROMFILE != 0 {
        let Some(d) = read_file(name) else { return 0 };
        return match kind {
            IMAGE_BITMAP if d.starts_with(b"BM") => bitmap_from_dib(&d[14..], flags & LR_CREATEDIBSECTION != 0),
            IMAGE_ICON | IMAGE_CURSOR => load_file_icon(&d, kind == IMAGE_CURSOR, cx, cy).map_or(0, add),
            _ => 0,
        };
    }
    match kind {
        IMAGE_BITMAP => {
            if module == 0 {
                return 0;
            }
            res_bytes(module, name, RT_BITMAP).map_or(0, |d| bitmap_from_dib(d, flags & LR_CREATEDIBSECTION != 0))
        }
        IMAGE_ICON | IMAGE_CURSOR => load_icon(module, name, kind == IMAGE_CURSOR, cx, cy),
        _ => 0,
    }
}

#[no_mangle]
pub unsafe extern "system" fn LoadImageA(module: u64, name: *const u8, kind: u32, cx: i32, cy: i32, flags: u32) -> u64 {
    let (_keep, w) = unsafe { name_a(name) };
    LoadImageW(module, w, kind, cx, cy, flags)
}

#[no_mangle]
pub extern "system" fn CreateIconFromResourceEx(bits: *const u8, size: u32, icon: Bool, _ver: u32, _cx: i32, _cy: i32, _flags: u32) -> u64 {
    if bits.is_null() {
        return 0;
    }
    let d = unsafe { core::slice::from_raw_parts(bits, size as usize) };
    decode_icon_image(d, icon == 0).map_or(0, add)
}

#[no_mangle]
pub extern "system" fn CreateIconFromResource(bits: *const u8, size: u32, icon: Bool, ver: u32) -> u64 {
    CreateIconFromResourceEx(bits, size, icon, ver, 0, 0, 0)
}

/// The id of the best image in an RT_GROUP_ICON / RT_GROUP_CURSOR resource.
#[no_mangle]
pub extern "system" fn LookupIconIdFromDirectoryEx(dir: *const u8, icon: Bool, cx: i32, cy: i32, _flags: u32) -> i32 {
    if dir.is_null() {
        return 0;
    }
    let n = unsafe { core::ptr::read_unaligned(dir.add(4) as *const u16) } as usize;
    let g = unsafe { core::slice::from_raw_parts(dir, 6 + n * 14) };
    let sizes: Vec<(i32, i32, u16)> = (0..n)
        .map(|i| {
            let e = 6 + i * 14;
            if icon == 0 { (u16_at(g, e) as i32, u16_at(g, e + 2) as i32 / 2, u16_at(g, e + 6)) } else { (dir_size(g[e]), dir_size(g[e + 1]), u16_at(g, e + 6)) }
        })
        .collect();
    u16_at(g, 6 + best_entry(&sizes, cx, cy) * 14 + 12) as i32
}

#[no_mangle]
pub extern "system" fn LookupIconIdFromDirectory(dir: *const u8, icon: Bool) -> i32 {
    LookupIconIdFromDirectoryEx(dir, icon, 0, 0, 0)
}

// -----| ICONINFO |-----

/// The pixels of 32bpp bitmap `hbm`, top-down, with its alpha if it's a
/// DIB section (a DDB has none).
fn bitmap_argb(hbm: u64) -> Option<(i32, i32, Vec<u32>)> {
    let mut ds = [0u8; 104];
    let n = unsafe { GetObjectW(hbm, 104, ds.as_mut_ptr()) };
    if n < 32 {
        return None;
    }
    let (w, h, bpp) = (u32_at(&ds, 4) as i32, u32_at(&ds, 8) as i32, u16_at(&ds, 18));
    let bits = u64::from_le_bytes(ds[24..32].try_into().unwrap());
    if n == 104 && bpp == 32 && bits != 0 {
        let bottom_up = (u32_at(&ds, 40) as i32) > 0;
        let src = unsafe { core::slice::from_raw_parts(bits as *const u32, (w * h) as usize) };
        let mut out = alloc::vec![0u32; (w * h) as usize];
        for y in 0..h {
            let sy = if bottom_up { h - 1 - y } else { y };
            out[(y * w) as usize..((y + 1) * w) as usize].copy_from_slice(&src[(sy * w) as usize..((sy + 1) * w) as usize]);
        }
        return Some((w, h, out));
    }
    // Through GetDIBits as 32bpp top-down.
    let mut bmi = [0u8; 44];
    bmi[0..4].copy_from_slice(&40u32.to_le_bytes());
    bmi[4..8].copy_from_slice(&w.to_le_bytes());
    bmi[8..12].copy_from_slice(&(-h).to_le_bytes());
    bmi[12..14].copy_from_slice(&1u16.to_le_bytes());
    bmi[14..16].copy_from_slice(&32u16.to_le_bytes());
    let mut out = alloc::vec![0u32; (w * h) as usize];
    unsafe { GetDIBits(0, hbm, 0, h as u32, out.as_mut_ptr() as *mut u8, bmi.as_mut_ptr(), 0) };
    Some((w, h, out))
}

/// ICONINFO: fIcon, xHotspot, yHotspot, hbmMask, hbmColor.
#[no_mangle]
pub unsafe extern "system" fn CreateIconIndirect(info: *const u8) -> u64 {
    if info.is_null() {
        return 0;
    }
    let d = unsafe { core::slice::from_raw_parts(info, 32) };
    let (is_icon, hx, hy) = (u32_at(d, 0) != 0, u32_at(d, 4) as i32, u32_at(d, 8) as i32);
    let (mask, color) = (u64::from_le_bytes(d[16..24].try_into().unwrap()), u64::from_le_bytes(d[24..32].try_into().unwrap()));
    let Some((mw, mh, mpx)) = bitmap_argb(mask) else { return 0 };
    let (w, h, mut argb) = match bitmap_argb(color).filter(|_| color != 0) {
        Some(c) => c,
        // Monochrome: the mask is AND over XOR, twice as tall.
        None => (mw, mh / 2, mpx[(mw * mh / 2) as usize..].to_vec()),
    };
    let alpha = argb.iter().any(|p| p >> 24 != 0);
    if !alpha {
        for (i, p) in argb.iter_mut().enumerate() {
            let masked = mpx.get(i).is_some_and(|m| m & 0xFF_FFFF != 0);
            *p = if masked { 0 } else { *p | 0xFF00_0000 };
        }
    }
    add(Icon { w, h, hotspot: if is_icon { (w / 2, h / 2) } else { (hx, hy) }, cursor: !is_icon, argb, shared: false })
}

#[no_mangle]
pub unsafe extern "system" fn GetIconInfo(hicon: u64, out: *mut u8) -> Bool {
    let Some((w, h, hot, cursor, argb)) = with_icon(hicon, |i| (i.w, i.h, i.hotspot, i.cursor, i.argb.clone())) else { return 0 };
    // Color: a 32bpp DIB section with the alpha; mask: 1bpp, set where
    // the icon is transparent.
    let mut bmi = [0u8; 40];
    bmi[0..4].copy_from_slice(&40u32.to_le_bytes());
    bmi[4..8].copy_from_slice(&w.to_le_bytes());
    bmi[8..12].copy_from_slice(&(-h).to_le_bytes());
    bmi[12..14].copy_from_slice(&1u16.to_le_bytes());
    bmi[14..16].copy_from_slice(&32u16.to_le_bytes());
    let mut bits: *mut u8 = core::ptr::null_mut();
    let color = unsafe { CreateDIBSection(0, bmi.as_ptr(), 0, &mut bits, 0, 0) };
    if !bits.is_null() {
        unsafe { core::ptr::copy_nonoverlapping(argb.as_ptr(), bits as *mut u32, argb.len()) };
    }
    let row = (w as usize).div_ceil(16) * 2;
    let mut mask_bits = alloc::vec![0u8; row * h as usize];
    for y in 0..h as usize {
        for x in 0..w as usize {
            if argb[y * w as usize + x] >> 24 == 0 {
                mask_bits[y * row + x / 8] |= 0x80 >> (x % 8);
            }
        }
    }
    let mask = unsafe { CreateBitmap(w, h, 1, 1, mask_bits.as_ptr()) };
    let mut info = [0u8; 32];
    info[0..4].copy_from_slice(&(!cursor as u32).to_le_bytes());
    info[4..8].copy_from_slice(&(hot.0 as u32).to_le_bytes());
    info[8..12].copy_from_slice(&(hot.1 as u32).to_le_bytes());
    info[16..24].copy_from_slice(&mask.to_le_bytes());
    info[24..32].copy_from_slice(&color.to_le_bytes());
    unsafe { core::ptr::copy_nonoverlapping(info.as_ptr(), out, 32) };
    1
}

#[no_mangle]
pub extern "system" fn CopyIcon(hicon: u64) -> u64 {
    with_icon(hicon, |i| Icon { w: i.w, h: i.h, hotspot: i.hotspot, cursor: i.cursor, argb: i.argb.clone(), shared: false }).map_or(0, add)
}

#[no_mangle]
pub extern "system" fn DestroyIcon(hicon: u64) -> Bool {
    let mut v = ICONS.lock();
    let Some(i) = hicon.checked_sub(ICON_BASE).map(|d| (d / 4) as usize) else { return 0 };
    match v.get_mut(i) {
        Some(slot @ Some(_)) => {
            if !slot.as_ref().unwrap().shared {
                *slot = None;
            }
            1
        }
        _ => 0,
    }
}

#[no_mangle]
pub extern "system" fn DestroyCursor(h: u64) -> Bool {
    DestroyIcon(h)
}

#[no_mangle]
pub extern "system" fn CopyImage(h: u64, kind: u32, cx: i32, cy: i32, _flags: u32) -> u64 {
    match kind {
        IMAGE_ICON | IMAGE_CURSOR => CopyIcon(h),
        IMAGE_BITMAP => {
            let mut bm = [0u8; 32];
            if unsafe { GetObjectW(h, 32, bm.as_mut_ptr()) } == 0 {
                return 0;
            }
            let (w, hh) = (u32_at(&bm, 4) as i32, u32_at(&bm, 8) as i32);
            let (nw, nh) = (if cx == 0 { w } else { cx }, if cy == 0 { hh } else { cy });
            let copy = unsafe { CreateCompatibleBitmap(0, nw, nh) };
            unsafe {
                let (a, b) = (CreateCompatibleDC(0), CreateCompatibleDC(0));
                let (oa, ob) = (crate::SelectObject(a, h), crate::SelectObject(b, copy));
                StretchBlt(b, 0, 0, nw, nh, a, 0, 0, w, hh, 0x00CC_0020);
                crate::SelectObject(a, oa);
                crate::SelectObject(b, ob);
                crate::DeleteDC(a);
                crate::DeleteDC(b);
            }
            copy
        }
        _ => 0,
    }
}

// -----| drawing |-----

const DI_MASK: u32 = 1;
const DI_IMAGE: u32 = 2;
const DI_DEFAULTSIZE: u32 = 8;

#[no_mangle]
pub extern "system" fn DrawIconEx(hdc: u64, x: i32, y: i32, hicon: u64, cx: i32, cy: i32, _step: u32, brush: u64, flags: u32) -> Bool {
    let Some((w, h, argb)) = with_icon(hicon, |i| (i.w, i.h, i.argb.clone())) else { return 0 };
    let cx = if cx == 0 { if flags & DI_DEFAULTSIZE != 0 { 32 } else { w } } else { cx };
    let cy = if cy == 0 { if flags & DI_DEFAULTSIZE != 0 { 32 } else { h } } else { cy };
    if brush != 0 {
        let r = [x, y, x + cx, y + cy];
        crate::paint::FillRect(hdc, &r, brush);
    }
    // The icon as a premultiplied 32bpp DIB section in a memory DC.
    let mut bmi = [0u8; 40];
    bmi[0..4].copy_from_slice(&40u32.to_le_bytes());
    bmi[4..8].copy_from_slice(&w.to_le_bytes());
    bmi[8..12].copy_from_slice(&(-h).to_le_bytes());
    bmi[12..14].copy_from_slice(&1u16.to_le_bytes());
    bmi[14..16].copy_from_slice(&32u16.to_le_bytes());
    let mut bits: *mut u8 = core::ptr::null_mut();
    let dib = unsafe { CreateDIBSection(0, bmi.as_ptr(), 0, &mut bits, 0, 0) };
    if dib == 0 || bits.is_null() {
        return 0;
    }
    let out = unsafe { core::slice::from_raw_parts_mut(bits as *mut u32, (w * h) as usize) };
    let what = flags & (DI_MASK | DI_IMAGE);
    for (o, &p) in out.iter_mut().zip(&argb) {
        let a = p >> 24;
        *o = match what {
            // The mask alone: black where opaque, white where not.
            DI_MASK => if a == 0 { 0xFFFF_FFFF } else { 0xFF00_0000 },
            // The image alone: every pixel opaque.
            DI_IMAGE => p | 0xFF00_0000,
            _ => {
                let pm = |s: u32| ((p >> s) & 0xFF) * a / 255;
                a << 24 | pm(16) << 16 | pm(8) << 8 | pm(0)
            }
        };
    }
    let mem = unsafe { CreateCompatibleDC(hdc) };
    let old = unsafe { crate::SelectObject(mem, dib) };
    let r = unsafe { GdiAlphaBlend(hdc, x, y, cx, cy, mem, 0, 0, w, h, 0x01FF_0000) };
    unsafe {
        crate::SelectObject(mem, old);
        crate::DeleteDC(mem);
        DeleteObject(dib);
    }
    r
}

#[no_mangle]
pub extern "system" fn DrawIcon(hdc: u64, x: i32, y: i32, hicon: u64) -> Bool {
    DrawIconEx(hdc, x, y, hicon, 0, 0, 0, 0, 3 | DI_DEFAULTSIZE)
}

// -----| the pointer |-----

static CURSOR: Lock<(u64, i32)> = Lock::new((0, 0));

#[no_mangle]
pub extern "system" fn SetCursor(c: u64) -> u64 {
    core::mem::replace(&mut CURSOR.lock().0, c)
}

#[no_mangle]
pub extern "system" fn GetCursor() -> u64 {
    let c = CURSOR.lock().0;
    if c == 0 { standard_handle(32512, true) } else { c }
}

#[no_mangle]
pub extern "system" fn ShowCursor(show: Bool) -> i32 {
    let mut c = CURSOR.lock();
    c.1 += if show != 0 { 1 } else { -1 };
    c.1
}

