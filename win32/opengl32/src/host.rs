//! `--features host`: this opengl32.dll built to run on Windows, next to a
//! test program (an app-local opengl32.dll wins over System32's), so its
//! pixels can be compared with Windows' own software OpenGL in seconds
//! instead of a VM boot. VEIL_GPU3D is answered here, in-process, by the
//! kernel's own rasterizer source (src/raster3d.rs), split into bands as
//! lgpu does; the gdi32 calls VEILOS adds are made from Windows' gdi32.
//! Windows' gdi32 asks opengl32 for pixel formats, so those live here too.

#[path = "../../../src/raster3d.rs"]
#[allow(dead_code)]
mod raster3d;

use crate::dllrt::Lock;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use raster3d::*;

enum Obj {
    Target { w: usize, h: usize, color: Vec<u32>, depth: Vec<u32>, stencil: Vec<u8> },
    Texture(Arc<Texture>),
}

static OBJS: Lock<Vec<Option<Obj>>> = Lock::new(Vec::new());

/// Rows per band: small, so band edges are exercised as on many CPUs.
const BAND: i32 = 7;
// (Set to the target height to compare a serial render.)

fn add(o: Obj, out: u64) -> i32 {
    let mut l = OBJS.lock();
    l.push(Some(o));
    unsafe { *(out as *mut u64) = l.len() as u64 };
    0
}

pub unsafe extern "system" fn VeilGpu3d(op: u64, a: u64, b: u64, c: u64) -> i32 {
    let mut l = OBJS.lock();
    match op {
        0 => {
            drop(l);
            let n = (a * b) as usize;
            add(Obj::Target { w: a as usize, h: b as usize, color: vec![0; n], depth: vec![DEPTH_MAX; n], stencil: vec![0; n] }, c)
        }
        5 | 6 => {
            let Some(Some(Obj::Target { w, h, color, depth, stencil })) = l.get_mut(a as usize - 1) else { return -1 };
            let buf = Buffers { color: color.as_mut_ptr(), depth: depth.as_mut_ptr(), stencil: stencil.as_mut_ptr(), w: *w, h: *h };
            let pa = unsafe { &*(b as *const PixelArgs) };
            if pa.w <= 0 || pa.h <= 0 {
                return 0;
            }
            let mut vals = vec![0u32; (pa.w * pa.h) as usize];
            for r in 0..pa.h as usize {
                let p = (pa.ptr as i64 + r as i64 * pa.stride) as *mut u32;
                let row = &mut vals[r * pa.w as usize..(r + 1) * pa.w as usize];
                if op == 5 {
                    unsafe { core::ptr::copy_nonoverlapping(p, row.as_mut_ptr(), pa.w as usize) };
                }
            }
            if op == 5 {
                write_pixels(buf, pa, &vals);
            } else {
                read_pixels(buf, pa, &mut vals);
                for r in 0..pa.h as usize {
                    let p = (pa.ptr as i64 + r as i64 * pa.stride) as *mut u32;
                    unsafe { core::ptr::copy_nonoverlapping(vals.as_ptr().add(r * pa.w as usize), p, pa.w as usize) };
                }
            }
            0
        }
        1 | 2 | 3 => {
            let Some(Some(Obj::Target { w, h, color, depth, stencil })) = l.get_mut(a as usize - 1) else { return -1 };
            let buf = Buffers { color: color.as_mut_ptr(), depth: depth.as_mut_ptr(), stencil: stencil.as_mut_ptr(), w: *w, h: *h };
            match op {
                1 => clear(buf, unsafe { &*(b as *const ClearArgs) }),
                2 => {
                    let d = unsafe { &*(b as *const DrawArgs) };
                    let groups = unsafe { core::slice::from_raw_parts(d.groups as *const Group, d.group_count as usize) };
                    let tris = unsafe { core::slice::from_raw_parts(d.tris as *const Tri, d.tri_count as usize) };
                    let h = *h;
                    let tex_of = |on: bool, h: u64| match l.get(h.wrapping_sub(1) as usize) {
                        Some(Some(Obj::Texture(t))) if on => Some(t.clone()),
                        _ => None,
                    };
                    let texs: Vec<_> = groups.iter().map(|g| (tex_of(g.flags & F_TEXTURE != 0, g.texture), tex_of(g.flags & F_TEXTURE2 != 0, g.texture2))).collect();
                    let passes: Vec<Pass> = groups
                        .iter()
                        .zip(texs.iter())
                        .map(|(g, (t1, t2))| {
                            let mut g = *g;
                            if t1.is_none() {
                                g.flags &= !F_TEXTURE;
                            }
                            if t2.is_none() {
                                g.flags &= !F_TEXTURE2;
                            }
                            Pass { g, tex: t1.as_deref(), tex2: t2.as_deref() }
                        })
                        .collect();
                    let mut y = 0;
                    while y < h as i32 {
                        render_band(buf, tris, &passes, y, (y + BAND).min(h as i32));
                        y += BAND;
                    }
                }
                _ => {
                    let r = unsafe { &*(b as *const ReadArgs) };
                    for row in 0..r.h {
                        let yy = r.y + row;
                        if yy < 0 || yy >= *h as i32 {
                            continue;
                        }
                        for x in 0..r.w {
                            let xx = r.x + x;
                            if xx >= 0 && xx < *w as i32 {
                                unsafe { *((r.dst as i64 + row as i64 * r.stride) as *mut u32).add(x as usize) = color[yy as usize * *w + xx as usize] };
                            }
                        }
                    }
                }
            }
            0
        }
        4 => {
            let ta = unsafe { &*(b as *const TexArgs) };
            let texels = unsafe { core::slice::from_raw_parts(ta.texels as *const u32, texel_count(ta)) }.to_vec();
            let t = Arc::new(texture(ta, texels));
            if a == 0 {
                drop(l);
                add(Obj::Texture(t), c)
            } else {
                l[a as usize - 1] = Some(Obj::Texture(t));
                0
            }
        }
        _ => -1,
    }
}

pub unsafe extern "system" fn NtClose(h: u64) -> i32 {
    if let Some(s) = OBJS.lock().get_mut(h.wrapping_sub(1) as usize) {
        *s = None;
    }
    0
}

#[link(name = "gdi32", kind = "raw-dylib")]
unsafe extern "system" {
    fn GetCurrentObject(hdc: u64, kind: u32) -> u64;
    fn GetObjectW(h: u64, size: i32, out: *mut u8) -> i32;
    fn GetObjectType(h: u64) -> u32;
}

#[link(name = "user32", kind = "raw-dylib")]
unsafe extern "system" {
    fn WindowFromDC(hdc: u64) -> u64;
    fn GetClientRect(hwnd: u64, r: *mut [i32; 4]) -> i32;
}

pub unsafe extern "system" fn VeilGdiDcSize(hdc: u64, out: *mut [i32; 2]) -> i32 {
    unsafe {
        if GetObjectType(hdc) == 10 {
            let mut bm = [0u8; 32];
            if GetObjectW(GetCurrentObject(hdc, 7), 32, bm.as_mut_ptr()) == 0 {
                return 0;
            }
            let w = i32::from_le_bytes(bm[4..8].try_into().unwrap());
            let h = i32::from_le_bytes(bm[8..12].try_into().unwrap());
            *out = [w, h.abs()];
            return 2;
        }
        let mut r = [0i32; 4];
        GetClientRect(WindowFromDC(hdc), &mut r);
        *out = [r[2], r[3]];
        1
    }
}

pub unsafe extern "system" fn VeilGdiSetSwapHook(_f: usize) {}

// Pixel formats: the same two as VEILOS's gdi32 (win32/gdi32/src/pixfmt.rs).

const FORMATS: [u32; 2] = [0x1 | 0x4 | 0x20 | 0x40, 0x4 | 0x8 | 0x10 | 0x20 | 0x40];
static SET: Lock<Vec<(u64, i32)>> = Lock::new(Vec::new());

pub unsafe extern "system" fn ChoosePixelFormat(_hdc: u64, pfd: *const u8) -> i32 {
    let flags = unsafe { core::ptr::read_unaligned(pfd.add(4) as *const u32) };
    if flags & 8 != 0 || flags & 1 == 0 { 2 } else { 1 }
}

pub unsafe extern "system" fn DescribePixelFormat(_hdc: u64, n: i32, size: u32, out: *mut u8) -> i32 {
    if !out.is_null() {
        if n < 1 || n > 2 {
            return 0;
        }
        if size >= 40 {
            let mut d = [0u8; 40];
            d[0] = 40;
            d[2] = 1;
            d[4..8].copy_from_slice(&FORMATS[n as usize - 1].to_le_bytes());
            d[9] = 32;
            (d[10], d[11], d[12], d[13], d[14], d[15]) = (8, 16, 8, 8, 8, 0);
            d[23] = 24;
            d[24] = 8;
            (d[18], d[19], d[20], d[21], d[22]) = (64, 16, 16, 16, 16);
            unsafe { core::ptr::copy_nonoverlapping(d.as_ptr(), out, 40) };
        }
    }
    2
}

pub unsafe extern "system" fn SetPixelFormat(hdc: u64, n: i32, _pfd: *const u8) -> i32 {
    let mut s = SET.lock();
    s.retain(|e| e.0 != hdc);
    s.push((hdc, n));
    1
}

pub unsafe extern "system" fn GetPixelFormat(hdc: u64) -> i32 {
    SET.lock().iter().find(|e| e.0 == hdc).map_or(0, |e| e.1)
}
