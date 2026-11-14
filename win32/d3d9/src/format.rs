//! Surface formats (D3DFORMAT): their sizes, and conversion of their pixels
//! to and from the kernel's 0xAARRGGBB. Compressed (DXT1-5) textures are
//! decompressed when uploaded.

use alloc::vec;
use alloc::vec::Vec;

pub const R8G8B8: u32 = 20;
pub const A8R8G8B8: u32 = 21;
pub const X8R8G8B8: u32 = 22;
pub const R5G6B5: u32 = 23;
pub const X1R5G5B5: u32 = 24;
pub const A1R5G5B5: u32 = 25;
pub const A4R4G4B4: u32 = 26;
pub const R3G3B2: u32 = 27;
pub const A8: u32 = 28;
pub const X4R4G4B4: u32 = 30;
pub const A2B10G10R10: u32 = 31;
pub const A8B8G8R8: u32 = 32;
pub const X8B8G8R8: u32 = 33;
pub const A2R10G10B10: u32 = 35;
pub const L8: u32 = 50;
pub const A8L8: u32 = 51;
pub const A4L4: u32 = 52;
pub const D16_LOCKABLE: u32 = 70;
pub const D32: u32 = 71;
pub const D15S1: u32 = 73;
pub const D24S8: u32 = 75;
pub const D24X8: u32 = 77;
pub const D24X4S4: u32 = 79;
pub const D16: u32 = 80;
pub const L16: u32 = 81;
pub const VERTEXDATA: u32 = 100;
pub const INDEX16: u32 = 101;
pub const INDEX32: u32 = 102;

const fn fourcc(s: &[u8; 4]) -> u32 {
    s[0] as u32 | (s[1] as u32) << 8 | (s[2] as u32) << 16 | (s[3] as u32) << 24
}
pub const DXT1: u32 = fourcc(b"DXT1");
pub const DXT2: u32 = fourcc(b"DXT2");
pub const DXT3: u32 = fourcc(b"DXT3");
pub const DXT4: u32 = fourcc(b"DXT4");
pub const DXT5: u32 = fourcc(b"DXT5");

/// Formats a texture or an off-screen surface can have.
pub const TEXTURE_FORMATS: &[u32] = &[
    A8R8G8B8, X8R8G8B8, R5G6B5, X1R5G5B5, A1R5G5B5, A4R4G4B4, X4R4G4B4, R3G3B2, A8, A8B8G8R8, X8B8G8R8, A2B10G10R10, A2R10G10B10, L8, A8L8,
    A4L4, L16, DXT1, DXT2, DXT3, DXT4, DXT5,
];
/// Formats a render target can have (kept as 0xAARRGGBB by the kernel).
pub const TARGET_FORMATS: &[u32] = &[A8R8G8B8, X8R8G8B8, R5G6B5, X1R5G5B5, A1R5G5B5, A4R4G4B4, X4R4G4B4, A8B8G8R8, X8B8G8R8];
/// Depth and stencil formats (kept as 24-bit depth and 8-bit stencil).
pub const DEPTH_FORMATS: &[u32] = &[D16, D16_LOCKABLE, D24S8, D24X8, D24X4S4, D15S1, D32];

pub fn is_dxt(f: u32) -> bool {
    matches!(f, DXT1 | DXT2 | DXT3 | DXT4 | DXT5)
}

/// Does a render target in this format keep alpha?
pub fn has_alpha(f: u32) -> bool {
    matches!(f, A8R8G8B8 | A1R5G5B5 | A4R4G4B4 | A8B8G8R8 | A8 | A8L8 | A4L4 | A2B10G10R10 | A2R10G10B10 | DXT1 | DXT2 | DXT3 | DXT4 | DXT5)
}

/// Bytes per pixel (per 4x4 block for DXT: 8 or 16).
pub fn bytes(f: u32) -> u32 {
    match f {
        A8 | L8 | A4L4 | R3G3B2 => 1,
        R5G6B5 | X1R5G5B5 | A1R5G5B5 | A4R4G4B4 | X4R4G4B4 | A8L8 | L16 | D16 | D16_LOCKABLE | D15S1 => 2,
        R8G8B8 => 3,
        DXT1 => 8,
        DXT2 | DXT3 | DXT4 | DXT5 => 16,
        _ => 4,
    }
}

/// Bytes per row of a `w` pixels wide surface (per row of blocks for DXT).
pub fn pitch(f: u32, w: u32) -> u32 {
    if is_dxt(f) { w.div_ceil(4).max(1) * bytes(f) } else { w * bytes(f) }
}

/// Rows of a `h` pixels high surface as stored (rows of blocks for DXT).
pub fn rows(f: u32, h: u32) -> u32 {
    if is_dxt(f) { h.div_ceil(4).max(1) } else { h }
}

fn x5(v: u32) -> u32 {
    (v << 3) | (v >> 2)
}
fn x6(v: u32) -> u32 {
    (v << 2) | (v >> 4)
}
fn x4(v: u32) -> u32 {
    v * 17
}

/// One pixel of format `f` (its bytes from `p`) as 0xAARRGGBB.
fn to_argb(f: u32, p: &[u8]) -> u32 {
    let w16 = || u16::from_le_bytes([p[0], p[1]]) as u32;
    let w32 = || u32::from_le_bytes([p[0], p[1], p[2], p[3]]);
    let argb = |a: u32, r: u32, g: u32, b: u32| a << 24 | r << 16 | g << 8 | b;
    match f {
        A8R8G8B8 => w32(),
        X8R8G8B8 => w32() | 0xFF00_0000,
        A8B8G8R8 => {
            let v = w32();
            (v & 0xFF00_FF00) | (v >> 16 & 0xFF) | (v & 0xFF) << 16
        }
        X8B8G8R8 => {
            let v = w32();
            0xFF00_0000 | (v & 0xFF00) | (v >> 16 & 0xFF) | (v & 0xFF) << 16
        }
        R8G8B8 => argb(255, p[2] as u32, p[1] as u32, p[0] as u32),
        R5G6B5 => {
            let v = w16();
            argb(255, x5(v >> 11), x6(v >> 5 & 63), x5(v & 31))
        }
        X1R5G5B5 | A1R5G5B5 => {
            let v = w16();
            let a = if f == X1R5G5B5 || v & 0x8000 != 0 { 255 } else { 0 };
            argb(a, x5(v >> 10 & 31), x5(v >> 5 & 31), x5(v & 31))
        }
        A4R4G4B4 | X4R4G4B4 => {
            let v = w16();
            let a = if f == X4R4G4B4 { 255 } else { x4(v >> 12) };
            argb(a, x4(v >> 8 & 15), x4(v >> 4 & 15), x4(v & 15))
        }
        R3G3B2 => {
            let v = p[0] as u32;
            argb(255, (v >> 5) * 255 / 7, (v >> 2 & 7) * 255 / 7, (v & 3) * 85)
        }
        A2R10G10B10 | A2B10G10R10 => {
            let v = w32();
            let (r, g, b) = ((v >> 20 & 1023) >> 2, (v >> 10 & 1023) >> 2, (v & 1023) >> 2);
            let (r, b) = if f == A2B10G10R10 { (b, r) } else { (r, b) };
            argb((v >> 30) * 85, r, g, b)
        }
        A8 => argb(p[0] as u32, 0, 0, 0),
        L8 => {
            let l = p[0] as u32;
            argb(255, l, l, l)
        }
        A8L8 => {
            let l = p[0] as u32;
            argb(p[1] as u32, l, l, l)
        }
        A4L4 => {
            let l = x4(p[0] as u32 & 15);
            argb(x4(p[0] as u32 >> 4), l, l, l)
        }
        L16 => {
            let l = w16() >> 8;
            argb(255, l, l, l)
        }
        _ => 0,
    }
}

/// 0xAARRGGBB as one pixel of format `f`, into `p`.
fn from_argb(f: u32, c: u32, p: &mut [u8]) {
    let (a, r, g, b) = (c >> 24, c >> 16 & 0xFF, c >> 8 & 0xFF, c & 0xFF);
    let put16 = |p: &mut [u8], v: u32| p[..2].copy_from_slice(&(v as u16).to_le_bytes());
    let put32 = |p: &mut [u8], v: u32| p[..4].copy_from_slice(&v.to_le_bytes());
    match f {
        A8R8G8B8 | X8R8G8B8 => put32(p, c),
        A8B8G8R8 | X8B8G8R8 => put32(p, a << 24 | b << 16 | g << 8 | r),
        R8G8B8 => p[..3].copy_from_slice(&[b as u8, g as u8, r as u8]),
        R5G6B5 => put16(p, (r >> 3) << 11 | (g >> 2) << 5 | b >> 3),
        X1R5G5B5 | A1R5G5B5 => put16(p, (a >> 7) << 15 | (r >> 3) << 10 | (g >> 3) << 5 | b >> 3),
        A4R4G4B4 | X4R4G4B4 => put16(p, (a >> 4) << 12 | (r >> 4) << 8 | (g >> 4) << 4 | b >> 4),
        R3G3B2 => p[0] = ((r >> 5) << 5 | (g >> 5) << 2 | b >> 6) as u8,
        A2R10G10B10 => put32(p, (a >> 6) << 30 | (r << 2 | r >> 6) << 20 | (g << 2 | g >> 6) << 10 | (b << 2 | b >> 6)),
        A2B10G10R10 => put32(p, (a >> 6) << 30 | (b << 2 | b >> 6) << 20 | (g << 2 | g >> 6) << 10 | (r << 2 | r >> 6)),
        A8 => p[0] = a as u8,
        L8 => p[0] = r as u8,
        A8L8 => p[..2].copy_from_slice(&[r as u8, a as u8]),
        A4L4 => p[0] = ((a >> 4) << 4 | r >> 4) as u8,
        L16 => put16(p, r << 8 | r),
        _ => {}
    }
}

/// A `w` x `h` image stored in format `f` (rows `pitch` bytes apart) as
/// 0xAARRGGBB pixels, rows packed.
pub fn decode(f: u32, w: u32, h: u32, src: &[u8], pitch: u32) -> Vec<u32> {
    let (w, h) = (w as usize, h as usize);
    let mut out = vec![0u32; w * h];
    if is_dxt(f) {
        let bw = w.div_ceil(4);
        for by in 0..h.div_ceil(4) {
            for bx in 0..bw {
                let at = by * pitch as usize + bx * bytes(f) as usize;
                let block = dxt_block(f, &src[at..at + bytes(f) as usize]);
                for y in 0..4 {
                    for x in 0..4 {
                        let (px, py) = (bx * 4 + x, by * 4 + y);
                        if px < w && py < h {
                            out[py * w + px] = block[y * 4 + x];
                        }
                    }
                }
            }
        }
        return out;
    }
    let bpp = bytes(f) as usize;
    for y in 0..h {
        let row = &src[y * pitch as usize..];
        for x in 0..w {
            out[y * w + x] = to_argb(f, &row[x * bpp..x * bpp + bpp]);
        }
    }
    out
}

/// The opposite of `decode` (not for DXT).
pub fn encode(f: u32, w: u32, h: u32, src: &[u32], dst: &mut [u8], pitch: u32) {
    let bpp = bytes(f) as usize;
    for y in 0..h as usize {
        for x in 0..w as usize {
            let at = y * pitch as usize + x * bpp;
            from_argb(f, src[y * w as usize + x], &mut dst[at..at + bpp]);
        }
    }
}

/// The 16 pixels of one DXT block, rows first.
fn dxt_block(f: u32, b: &[u8]) -> [u32; 16] {
    let (alpha, color) = if f == DXT1 { (&b[0..0], &b[0..8]) } else { (&b[0..8], &b[8..16]) };
    let c0 = u16::from_le_bytes([color[0], color[1]]) as u32;
    let c1 = u16::from_le_bytes([color[2], color[3]]) as u32;
    let rgb = |c: u32| [x5(c >> 11), x6(c >> 5 & 63), x5(c & 31)];
    let (p0, p1) = (rgb(c0), rgb(c1));
    let mix = |a: [u32; 3], b: [u32; 3], wa: u32, wb: u32| -> [u32; 3] {
        core::array::from_fn(|i| (a[i] * wa + b[i] * wb) / (wa + wb))
    };
    // DXT1 with c0 <= c1: three colours and transparent black.
    let four = f != DXT1 || c0 > c1;
    let pal: [[u32; 4]; 4] = if four {
        let (p2, p3) = (mix(p0, p1, 2, 1), mix(p0, p1, 1, 2));
        [[p0[0], p0[1], p0[2], 255], [p1[0], p1[1], p1[2], 255], [p2[0], p2[1], p2[2], 255], [p3[0], p3[1], p3[2], 255]]
    } else {
        let p2 = mix(p0, p1, 1, 1);
        [[p0[0], p0[1], p0[2], 255], [p1[0], p1[1], p1[2], 255], [p2[0], p2[1], p2[2], 255], [0, 0, 0, 0]]
    };
    let bits = u32::from_le_bytes([color[4], color[5], color[6], color[7]]);
    // Alpha: DXT2/3 four bits a pixel, DXT4/5 interpolated.
    let alphas: [u32; 16] = match f {
        DXT2 | DXT3 => core::array::from_fn(|i| x4((u64::from_le_bytes(alpha.try_into().unwrap()) >> (i * 4)) as u32 & 15)),
        DXT4 | DXT5 => {
            let (a0, a1) = (alpha[0] as u32, alpha[1] as u32);
            let mut pa = [a0, a1, 0, 0, 0, 0, 0, 0];
            if a0 > a1 {
                for i in 1..7 {
                    pa[i + 1] = (a0 * (7 - i as u32) + a1 * i as u32) / 7;
                }
            } else {
                for i in 1..5 {
                    pa[i + 1] = (a0 * (5 - i as u32) + a1 * i as u32) / 5;
                }
                pa[6] = 0;
                pa[7] = 255;
            }
            let mut idx = [0u8; 8];
            idx[..6].copy_from_slice(&alpha[2..8]);
            let ab = u64::from_le_bytes(idx);
            core::array::from_fn(|i| pa[(ab >> (i * 3)) as usize & 7])
        }
        _ => [255; 16],
    };
    core::array::from_fn(|i| {
        let p = pal[(bits >> (i * 2)) as usize & 3];
        let a = if f == DXT1 { p[3] } else { alphas[i] };
        a << 24 | p[0] << 16 | p[1] << 8 | p[2]
    })
}
