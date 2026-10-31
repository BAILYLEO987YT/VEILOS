//! Image lists: images of one size kept as 32bpp straight-alpha pixels
//! (from icons, or from bitmaps with a mask or a mask color), drawn by
//! blending through gdi32's GdiAlphaBlend.

use alloc::boxed::Box;
use alloc::vec::Vec;

use crate::Bool;

const CLR_NONE: u32 = 0xFFFF_FFFF;
const ILD_TRANSPARENT: u32 = 0x0001;
const ILD_MASK: u32 = 0x0010;

pub struct ImageList {
    cx: i32,
    cy: i32,
    bk: u32,
    images: Vec<Vec<u32>>,
}

fn get<'a>(h: u64) -> Option<&'a mut ImageList> {
    (h != 0).then(|| unsafe { &mut *(h as *mut ImageList) })
}

#[no_mangle]
pub extern "system" fn ImageList_Create(cx: i32, cy: i32, _flags: u32, initial: i32, _grow: i32) -> u64 {
    if cx <= 0 || cy <= 0 {
        return 0;
    }
    let il = Box::new(ImageList { cx, cy, bk: CLR_NONE, images: Vec::with_capacity(initial.max(0) as usize) });
    Box::into_raw(il) as u64
}

#[no_mangle]
pub extern "system" fn ImageList_Destroy(h: u64) -> Bool {
    if h == 0 {
        return 0;
    }
    drop(unsafe { Box::from_raw(h as *mut ImageList) });
    1
}

#[no_mangle]
pub extern "system" fn ImageList_GetImageCount(h: u64) -> i32 {
    get(h).map_or(0, |il| il.images.len() as i32)
}

#[no_mangle]
pub extern "system" fn ImageList_SetImageCount(h: u64, n: u32) -> Bool {
    let Some(il) = get(h) else { return 0 };
    let blank = alloc::vec![0u32; (il.cx * il.cy) as usize];
    il.images.resize(n as usize, blank);
    1
}

#[no_mangle]
pub unsafe extern "system" fn ImageList_GetIconSize(h: u64, cx: *mut i32, cy: *mut i32) -> Bool {
    let Some(il) = get(h) else { return 0 };
    unsafe {
        if !cx.is_null() {
            *cx = il.cx;
        }
        if !cy.is_null() {
            *cy = il.cy;
        }
    }
    1
}

#[no_mangle]
pub extern "system" fn ImageList_SetIconSize(h: u64, cx: i32, cy: i32) -> Bool {
    let Some(il) = get(h) else { return 0 };
    il.cx = cx.max(1);
    il.cy = cy.max(1);
    il.images.clear();
    1
}

#[no_mangle]
pub extern "system" fn ImageList_SetBkColor(h: u64, color: u32) -> u32 {
    get(h).map_or(CLR_NONE, |il| core::mem::replace(&mut il.bk, color))
}

#[no_mangle]
pub extern "system" fn ImageList_GetBkColor(h: u64) -> u32 {
    get(h).map_or(CLR_NONE, |il| il.bk)
}

/// A bitmap's pixels, top-down 32bpp, with its alpha if it's a 32bpp DIB
/// section (a DDB has none: all clear).
fn bitmap_pixels(hbm: u64) -> Option<(i32, i32, Vec<u32>)> {
    let mut ds = [0u8; 104];
    let n = unsafe { crate::GetObjectW(hbm, 104, ds.as_mut_ptr()) };
    if n < 32 {
        return None;
    }
    let rd = |o: usize| i32::from_le_bytes(ds[o..o + 4].try_into().unwrap());
    let (w, h, bpp) = (rd(4), rd(8), u16::from_le_bytes([ds[18], ds[19]]));
    let bits = u64::from_le_bytes(ds[24..32].try_into().unwrap());
    if n == 104 && bpp == 32 && bits != 0 {
        let bottom_up = rd(40) > 0;
        let src = unsafe { core::slice::from_raw_parts(bits as *const u32, (w * h) as usize) };
        let mut out = alloc::vec![0u32; (w * h) as usize];
        for y in 0..h {
            let sy = if bottom_up { h - 1 - y } else { y };
            out[(y * w) as usize..((y + 1) * w) as usize].copy_from_slice(&src[(sy * w) as usize..((sy + 1) * w) as usize]);
        }
        return Some((w, h, out));
    }
    let mut bmi = [0u8; 52];
    bmi[0..4].copy_from_slice(&40u32.to_le_bytes());
    bmi[4..8].copy_from_slice(&w.to_le_bytes());
    bmi[8..12].copy_from_slice(&(-h).to_le_bytes());
    bmi[12..14].copy_from_slice(&1u16.to_le_bytes());
    bmi[14..16].copy_from_slice(&32u16.to_le_bytes());
    let mut out = alloc::vec![0u32; (w * h) as usize];
    unsafe { crate::GetDIBits(0, hbm, 0, h as u32, out.as_mut_ptr() as *mut u8, bmi.as_mut_ptr(), 0) };
    Some((w, h, out))
}

/// Split a strip of images `cx` wide into the list.
fn add_strip(il: &mut ImageList, w: i32, h: i32, px: &[u32]) -> i32 {
    let first = il.images.len() as i32;
    let n = (w / il.cx).max(1);
    for k in 0..n {
        let mut img = alloc::vec![0u32; (il.cx * il.cy) as usize];
        for y in 0..il.cy.min(h) {
            for x in 0..il.cx {
                let sx = k * il.cx + x;
                if sx < w {
                    img[(y * il.cx + x) as usize] = px[(y * w + sx) as usize];
                }
            }
        }
        il.images.push(img);
    }
    first
}

#[no_mangle]
pub extern "system" fn ImageList_Add(h: u64, image: u64, mask: u64) -> i32 {
    let Some(il) = get(h) else { return -1 };
    let Some((w, hh, mut px)) = bitmap_pixels(image) else { return -1 };
    let has_alpha = px.iter().any(|p| p >> 24 != 0);
    if !has_alpha {
        // Opaque, except where a mask bitmap is white.
        let m = if mask != 0 { bitmap_pixels(mask).map(|m| m.2) } else { None };
        for (i, p) in px.iter_mut().enumerate() {
            let clear = m.as_ref().is_some_and(|m| m.get(i).is_some_and(|v| v & 0xFF_FFFF != 0));
            *p = if clear { 0 } else { *p | 0xFF00_0000 };
        }
    }
    add_strip(il, w, hh, &px)
}

/// Pixels of `mask` color (a COLORREF) become transparent.
#[no_mangle]
pub extern "system" fn ImageList_AddMasked(h: u64, image: u64, mask: u32) -> i32 {
    let Some(il) = get(h) else { return -1 };
    let Some((w, hh, mut px)) = bitmap_pixels(image) else { return -1 };
    let key = (mask & 0xFF) << 16 | (mask & 0xFF00) | (mask >> 16) & 0xFF;
    for p in px.iter_mut() {
        *p = if *p & 0xFF_FFFF == key { 0 } else { *p | 0xFF00_0000 };
    }
    add_strip(il, w, hh, &px)
}

/// An icon's pixels, scaled to the list's size.
fn icon_pixels(il: &ImageList, icon: u64) -> Option<Vec<u32>> {
    let mut info = [0u8; 32];
    if unsafe { crate::GetIconInfo(icon, info.as_mut_ptr()) } == 0 {
        return None;
    }
    let mask = u64::from_le_bytes(info[16..24].try_into().unwrap());
    let color = u64::from_le_bytes(info[24..32].try_into().unwrap());
    let (w, h, px) = bitmap_pixels(color)?;
    unsafe {
        crate::DeleteObject(mask);
        crate::DeleteObject(color);
    }
    let mut out = alloc::vec![0u32; (il.cx * il.cy) as usize];
    for y in 0..il.cy {
        for x in 0..il.cx {
            out[(y * il.cx + x) as usize] = px[((y * h / il.cy) * w + x * w / il.cx) as usize];
        }
    }
    Some(out)
}

/// Replace image `i` with an icon (-1: add it).
#[no_mangle]
pub extern "system" fn ImageList_ReplaceIcon(h: u64, i: i32, icon: u64) -> i32 {
    let Some(il) = get(h) else { return -1 };
    let Some(px) = icon_pixels(il, icon) else { return -1 };
    if i < 0 || i as usize >= il.images.len() {
        il.images.push(px);
        il.images.len() as i32 - 1
    } else {
        il.images[i as usize] = px;
        i
    }
}

#[no_mangle]
pub extern "system" fn ImageList_Replace(h: u64, i: i32, image: u64, mask: u64) -> Bool {
    let Some(il) = get(h) else { return 0 };
    if i < 0 || i as usize >= il.images.len() {
        return 0;
    }
    let n = il.images.len();
    if ImageList_Add(h, image, mask) < 0 {
        return 0;
    }
    let il = get(h).unwrap();
    let new = il.images.remove(n);
    il.images.truncate(n);
    il.images[i as usize] = new;
    1
}

#[no_mangle]
pub extern "system" fn ImageList_Remove(h: u64, i: i32) -> Bool {
    let Some(il) = get(h) else { return 0 };
    if i < 0 {
        il.images.clear();
        return 1;
    }
    if i as usize >= il.images.len() {
        return 0;
    }
    il.images.remove(i as usize);
    1
}

/// Image `i` as a new icon.
#[no_mangle]
pub extern "system" fn ImageList_GetIcon(h: u64, i: i32, _flags: u32) -> u64 {
    let Some(il) = get(h) else { return 0 };
    let Some(img) = il.images.get(i.max(0) as usize) else { return 0 };
    let (w, hh) = (il.cx, il.cy);
    let mut bmi = [0u8; 40];
    bmi[0..4].copy_from_slice(&40u32.to_le_bytes());
    bmi[4..8].copy_from_slice(&w.to_le_bytes());
    bmi[8..12].copy_from_slice(&(-hh).to_le_bytes());
    bmi[12..14].copy_from_slice(&1u16.to_le_bytes());
    bmi[14..16].copy_from_slice(&32u16.to_le_bytes());
    let mut bits: *mut u8 = core::ptr::null_mut();
    let color = unsafe { crate::CreateDIBSection(0, bmi.as_ptr(), 0, &mut bits, 0, 0) };
    if color == 0 || bits.is_null() {
        return 0;
    }
    unsafe { core::ptr::copy_nonoverlapping(img.as_ptr(), bits as *mut u32, img.len()) };
    let row = (w as usize).div_ceil(16) * 2;
    let mask_bits = alloc::vec![0u8; row * hh as usize];
    let mask = unsafe { crate::CreateBitmap(w, hh, 1, 1, mask_bits.as_ptr()) };
    let mut info = [0u8; 32];
    info[0..4].copy_from_slice(&1u32.to_le_bytes());
    info[16..24].copy_from_slice(&mask.to_le_bytes());
    info[24..32].copy_from_slice(&color.to_le_bytes());
    let icon = unsafe { crate::CreateIconIndirect(info.as_ptr()) };
    unsafe {
        crate::DeleteObject(mask);
        crate::DeleteObject(color);
    }
    icon
}

/// Draw image `i` at (x, y), `cx` x `cy` (0: its size).
fn draw(h: u64, i: i32, hdc: u64, x: i32, y: i32, cx: i32, cy: i32, bk: u32, style: u32) -> Bool {
    let Some(il) = get(h) else { return 0 };
    let Some(img) = il.images.get(i.max(0) as usize) else { return 0 };
    let (w, hh) = (il.cx, il.cy);
    let (cx, cy) = (if cx == 0 { w } else { cx }, if cy == 0 { hh } else { cy });
    let bk = if bk == 0xFF00_0000 { il.bk } else { bk };
    if bk != CLR_NONE && style & ILD_TRANSPARENT == 0 {
        let b = unsafe { crate::CreateSolidBrush(bk) };
        unsafe { crate::FillRect(hdc, &[x, y, x + cx, y + cy], b) };
        unsafe { crate::DeleteObject(b) };
    }
    let mut bmi = [0u8; 40];
    bmi[0..4].copy_from_slice(&40u32.to_le_bytes());
    bmi[4..8].copy_from_slice(&w.to_le_bytes());
    bmi[8..12].copy_from_slice(&(-hh).to_le_bytes());
    bmi[12..14].copy_from_slice(&1u16.to_le_bytes());
    bmi[14..16].copy_from_slice(&32u16.to_le_bytes());
    let mut bits: *mut u8 = core::ptr::null_mut();
    let dib = unsafe { crate::CreateDIBSection(0, bmi.as_ptr(), 0, &mut bits, 0, 0) };
    if dib == 0 || bits.is_null() {
        return 0;
    }
    let out = unsafe { core::slice::from_raw_parts_mut(bits as *mut u32, img.len()) };
    for (o, &p) in out.iter_mut().zip(img) {
        let a = p >> 24;
        *o = if style & ILD_MASK != 0 {
            if a == 0 { 0 } else { 0xFF00_0000 }
        } else {
            let pm = |s: u32| ((p >> s) & 0xFF) * a / 255;
            a << 24 | pm(16) << 16 | pm(8) << 8 | pm(0)
        };
    }
    let mem = unsafe { crate::CreateCompatibleDC(hdc) };
    let old = unsafe { crate::SelectObject(mem, dib) };
    unsafe { crate::GdiAlphaBlend(hdc, x, y, cx, cy, mem, 0, 0, w, hh, 0x01FF_0000) };
    unsafe {
        crate::SelectObject(mem, old);
        crate::DeleteDC(mem);
        crate::DeleteObject(dib);
    }
    1
}

#[no_mangle]
pub extern "system" fn ImageList_Draw(h: u64, i: i32, hdc: u64, x: i32, y: i32, style: u32) -> Bool {
    draw(h, i, hdc, x, y, 0, 0, 0xFF00_0000, style)
}

#[no_mangle]
pub extern "system" fn ImageList_DrawEx(h: u64, i: i32, hdc: u64, x: i32, y: i32, cx: i32, cy: i32, bk: u32, _fg: u32, style: u32) -> Bool {
    draw(h, i, hdc, x, y, cx, cy, bk, style)
}

/// A bitmap or icon strip from a resource or file, split `cx` wide.
#[no_mangle]
pub unsafe extern "system" fn ImageList_LoadImageW(inst: u64, name: *const u16, cx: i32, _grow: i32, mask: u32, kind: u32, flags: u32) -> u64 {
    let img = unsafe { crate::LoadImageW(inst, name, kind, 0, 0, flags | 0x2000) };
    if img == 0 {
        return 0;
    }
    if kind == 1 {
        let il = ImageList_Create(cx, cx, 0x21, 1, 1);
        ImageList_ReplaceIcon(il, -1, img);
        return il;
    }
    let Some((_, h, _)) = bitmap_pixels(img) else { return 0 };
    let il = ImageList_Create(cx, h, 0x21, 4, 4);
    if mask == CLR_NONE { ImageList_Add(il, img, 0) } else { ImageList_AddMasked(il, img, mask) };
    unsafe { crate::DeleteObject(img) };
    il
}
