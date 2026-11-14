//! DXGI formats: their sizes, and their texels to and from the kernel's
//! 0xAARRGGBB (8 bits a channel: floating-point and 16-bit formats are
//! clamped and rounded on the way). Block-compressed BC1-5 are decoded
//! when uploaded.

use alloc::vec;
use alloc::vec::Vec;

pub const R32G32B32A32_TYPELESS: u32 = 1;
pub const R32G32B32A32_FLOAT: u32 = 2;
pub const R32G32B32_FLOAT: u32 = 6;
pub const R16G16B16A16_TYPELESS: u32 = 9;
pub const R16G16B16A16_FLOAT: u32 = 10;
pub const R16G16B16A16_UNORM: u32 = 11;
pub const R32G32_FLOAT: u32 = 16;
pub const R32G8X24_TYPELESS: u32 = 19;
pub const D32_FLOAT_S8X24_UINT: u32 = 20;
pub const R10G10B10A2_TYPELESS: u32 = 23;
pub const R10G10B10A2_UNORM: u32 = 24;
pub const R11G11B10_FLOAT: u32 = 26;
pub const R8G8B8A8_TYPELESS: u32 = 27;
pub const R8G8B8A8_UNORM: u32 = 28;
pub const R8G8B8A8_UNORM_SRGB: u32 = 29;
pub const R8G8B8A8_UINT: u32 = 30;
pub const R8G8B8A8_SNORM: u32 = 31;
pub const R16G16_TYPELESS: u32 = 33;
pub const R16G16_FLOAT: u32 = 34;
pub const R16G16_UNORM: u32 = 35;
pub const R32_TYPELESS: u32 = 39;
pub const D32_FLOAT: u32 = 40;
pub const R32_FLOAT: u32 = 41;
pub const R32_UINT: u32 = 42;
pub const R24G8_TYPELESS: u32 = 44;
pub const D24_UNORM_S8_UINT: u32 = 45;
pub const R24_UNORM_X8_TYPELESS: u32 = 46;
pub const R8G8_TYPELESS: u32 = 48;
pub const R8G8_UNORM: u32 = 49;
pub const R16_TYPELESS: u32 = 53;
pub const R16_FLOAT: u32 = 54;
pub const D16_UNORM: u32 = 55;
pub const R16_UNORM: u32 = 56;
pub const R16_UINT: u32 = 57;
pub const R8_TYPELESS: u32 = 60;
pub const R8_UNORM: u32 = 61;
pub const A8_UNORM: u32 = 65;
pub const BC1_TYPELESS: u32 = 70;
pub const BC1_UNORM: u32 = 71;
pub const BC1_UNORM_SRGB: u32 = 72;
pub const BC2_TYPELESS: u32 = 73;
pub const BC2_UNORM: u32 = 74;
pub const BC2_UNORM_SRGB: u32 = 75;
pub const BC3_TYPELESS: u32 = 76;
pub const BC3_UNORM: u32 = 77;
pub const BC3_UNORM_SRGB: u32 = 78;
pub const BC4_TYPELESS: u32 = 79;
pub const BC4_UNORM: u32 = 80;
pub const BC5_TYPELESS: u32 = 82;
pub const BC5_UNORM: u32 = 83;
pub const B5G6R5_UNORM: u32 = 85;
pub const B5G5R5A1_UNORM: u32 = 86;
pub const B8G8R8A8_UNORM: u32 = 87;
pub const B8G8R8X8_UNORM: u32 = 88;
pub const B8G8R8A8_TYPELESS: u32 = 90;
pub const B8G8R8A8_UNORM_SRGB: u32 = 91;
pub const B8G8R8X8_TYPELESS: u32 = 92;
pub const B8G8R8X8_UNORM_SRGB: u32 = 93;
pub const B4G4R4A4_UNORM: u32 = 115;

/// Formats a texture can have here, and how many bytes a texel (a 4x4
/// block for BC) takes.
pub fn bytes(f: u32) -> Option<u32> {
    Some(match f {
        R32G32B32A32_TYPELESS | R32G32B32A32_FLOAT => 16,
        R32G32B32_FLOAT => 12,
        R16G16B16A16_TYPELESS | R16G16B16A16_FLOAT | R16G16B16A16_UNORM | R32G32_FLOAT | R32G8X24_TYPELESS | D32_FLOAT_S8X24_UINT => 8,
        R10G10B10A2_TYPELESS | R10G10B10A2_UNORM | R11G11B10_FLOAT | R8G8B8A8_TYPELESS | R8G8B8A8_UNORM | R8G8B8A8_UNORM_SRGB | R8G8B8A8_UINT
        | R8G8B8A8_SNORM | R16G16_TYPELESS | R16G16_FLOAT | R16G16_UNORM | R32_TYPELESS | D32_FLOAT | R32_FLOAT | R32_UINT | R24G8_TYPELESS
        | D24_UNORM_S8_UINT | R24_UNORM_X8_TYPELESS | B8G8R8A8_UNORM | B8G8R8X8_UNORM | B8G8R8A8_TYPELESS | B8G8R8A8_UNORM_SRGB
        | B8G8R8X8_TYPELESS | B8G8R8X8_UNORM_SRGB => 4,
        R8G8_TYPELESS | R8G8_UNORM | R16_TYPELESS | R16_FLOAT | D16_UNORM | R16_UNORM | R16_UINT | B5G6R5_UNORM | B5G5R5A1_UNORM | B4G4R4A4_UNORM => 2,
        R8_TYPELESS | R8_UNORM | A8_UNORM => 1,
        BC1_TYPELESS | BC1_UNORM | BC1_UNORM_SRGB | BC4_TYPELESS | BC4_UNORM => 8,
        BC2_TYPELESS | BC2_UNORM | BC2_UNORM_SRGB | BC3_TYPELESS | BC3_UNORM | BC3_UNORM_SRGB | BC5_TYPELESS | BC5_UNORM => 16,
        _ => return None,
    })
}

pub fn is_bc(f: u32) -> bool {
    (70..=84).contains(&f)
}

pub fn is_depth(f: u32) -> bool {
    matches!(f, D32_FLOAT | D24_UNORM_S8_UINT | D16_UNORM | D32_FLOAT_S8X24_UINT)
}

/// Typeless and depth formats that can be bound as depth.
pub fn can_be_depth(f: u32) -> bool {
    is_depth(f) || matches!(f, R24G8_TYPELESS | R32_TYPELESS | R16_TYPELESS | R32G8X24_TYPELESS)
}

/// A render target format keeps alpha.
pub fn has_alpha(f: u32) -> bool {
    !matches!(f, B8G8R8X8_UNORM | B8G8R8X8_TYPELESS | B8G8R8X8_UNORM_SRGB | B5G6R5_UNORM | R11G11B10_FLOAT | R8G8_UNORM | R8_UNORM | R16_FLOAT | R32_FLOAT)
}

/// Bytes a row (of blocks) takes, `w` texels wide.
pub fn pitch(f: u32, w: u32) -> u32 {
    let b = bytes(f).unwrap_or(4);
    if is_bc(f) { w.div_ceil(4).max(1) * b } else { w * b }
}

/// Rows (of blocks) `h` texels high takes.
pub fn rows(f: u32, h: u32) -> u32 {
    if is_bc(f) { h.div_ceil(4).max(1) } else { h }
}

fn half(h: u16) -> f32 {
    let (s, e, m) = ((h >> 15) as u32, ((h >> 10) & 31) as u32, (h & 1023) as u32);
    let bits = if e == 0 {
        if m == 0 {
            s << 31
        } else {
            let (mut e, mut m) = (127 - 15 + 1, m);
            while m & 1024 == 0 {
                m <<= 1;
                e -= 1;
            }
            s << 31 | (e as u32) << 23 | (m & 1023) << 13
        }
    } else if e == 31 {
        s << 31 | 0xFF << 23 | m << 13
    } else {
        s << 31 | (e + 127 - 15) << 23 | m << 13
    };
    f32::from_bits(bits)
}

fn to_half(f: f32) -> u16 {
    let b = f.to_bits();
    let s = (b >> 16 & 0x8000) as u16;
    let e = ((b >> 23) & 255) as i32 - 127 + 15;
    if e <= 0 {
        s
    } else if e >= 31 {
        s | 0x7C00
    } else {
        s | (e as u16) << 10 | ((b >> 13) & 0x3FF) as u16
    }
}

/// 0 ..= 1 to a byte, rounded.
fn unorm(x: f32) -> u32 {
    let x = if x > 0.0 { x } else { 0.0 };
    let x = if x < 1.0 { x } else { 1.0 };
    (x * 255.0 + 0.5) as u32
}

fn f11(v: u32, mbits: u32) -> f32 {
    let e = v >> mbits;
    let m = v & ((1 << mbits) - 1);
    if e == 0 {
        m as f32 / (1 << mbits) as f32 * f32::from_bits((127 - 14) << 23)
    } else {
        f32::from_bits((e + 127 - 15) << 23 | m << (23 - mbits))
    }
}

/// One texel (its bytes `p`) as 0xAARRGGBB.
pub fn to_argb(f: u32, p: &[u8]) -> u32 {
    let w16 = |i: usize| u16::from_le_bytes([p[2 * i], p[2 * i + 1]]) as u32;
    let w32 = |i: usize| u32::from_le_bytes([p[4 * i], p[4 * i + 1], p[4 * i + 2], p[4 * i + 3]]);
    let f32_ = |i: usize| f32::from_bits(w32(i));
    let argb = |a: u32, r: u32, g: u32, b: u32| a << 24 | r << 16 | g << 8 | b;
    let x5 = |v: u32| (v << 3) | (v >> 2);
    let x6 = |v: u32| (v << 2) | (v >> 4);
    match f {
        R8G8B8A8_TYPELESS | R8G8B8A8_UNORM | R8G8B8A8_UNORM_SRGB | R8G8B8A8_UINT => argb(p[3] as u32, p[0] as u32, p[1] as u32, p[2] as u32),
        R8G8B8A8_SNORM => {
            let s = |x: u8| unorm((x as i8).max(-127) as f32 / 127.0);
            argb(s(p[3]), s(p[0]), s(p[1]), s(p[2]))
        }
        B8G8R8A8_UNORM | B8G8R8A8_TYPELESS | B8G8R8A8_UNORM_SRGB => w32(0),
        B8G8R8X8_UNORM | B8G8R8X8_TYPELESS | B8G8R8X8_UNORM_SRGB => w32(0) | 0xFF00_0000,
        R10G10B10A2_TYPELESS | R10G10B10A2_UNORM => {
            let v = w32(0);
            argb((v >> 30) * 85, (v & 1023) >> 2, (v >> 10 & 1023) >> 2, (v >> 20 & 1023) >> 2)
        }
        R11G11B10_FLOAT => {
            let v = w32(0);
            argb(255, unorm(f11(v & 0x7FF, 6)), unorm(f11(v >> 11 & 0x7FF, 6)), unorm(f11(v >> 22, 5)))
        }
        R16G16B16A16_FLOAT | R16G16B16A16_TYPELESS => argb(unorm(half(w16(3) as u16)), unorm(half(w16(0) as u16)), unorm(half(w16(1) as u16)), unorm(half(w16(2) as u16))),
        R16G16B16A16_UNORM => argb(w16(3) >> 8, w16(0) >> 8, w16(1) >> 8, w16(2) >> 8),
        R32G32B32A32_FLOAT | R32G32B32A32_TYPELESS => argb(unorm(f32_(3)), unorm(f32_(0)), unorm(f32_(1)), unorm(f32_(2))),
        R32G32B32_FLOAT => argb(255, unorm(f32_(0)), unorm(f32_(1)), unorm(f32_(2))),
        R32G32_FLOAT => argb(255, unorm(f32_(0)), unorm(f32_(1)), 0),
        R16G16_FLOAT | R16G16_TYPELESS => argb(255, unorm(half(w16(0) as u16)), unorm(half(w16(1) as u16)), 0),
        R16G16_UNORM => argb(255, w16(0) >> 8, w16(1) >> 8, 0),
        R32_FLOAT | R32_TYPELESS | D32_FLOAT => argb(255, unorm(f32_(0)), 0, 0),
        R32_UINT => argb(255, w32(0).min(255), 0, 0),
        R16_FLOAT => argb(255, unorm(half(w16(0) as u16)), 0, 0),
        R16_UNORM | R16_TYPELESS | D16_UNORM | R16_UINT => argb(255, w16(0) >> 8, 0, 0),
        R8G8_UNORM | R8G8_TYPELESS => argb(255, p[0] as u32, p[1] as u32, 0),
        R8_UNORM | R8_TYPELESS => argb(255, p[0] as u32, 0, 0),
        A8_UNORM => argb(p[0] as u32, 0, 0, 0),
        B5G6R5_UNORM => {
            let v = w16(0);
            argb(255, x5(v >> 11), x6(v >> 5 & 63), x5(v & 31))
        }
        B5G5R5A1_UNORM => {
            let v = w16(0);
            argb(if v & 0x8000 != 0 { 255 } else { 0 }, x5(v >> 10 & 31), x5(v >> 5 & 31), x5(v & 31))
        }
        B4G4R4A4_UNORM => {
            let v = w16(0);
            argb((v >> 12) * 17, (v >> 8 & 15) * 17, (v >> 4 & 15) * 17, (v & 15) * 17)
        }
        R24G8_TYPELESS | D24_UNORM_S8_UINT | R24_UNORM_X8_TYPELESS => argb(255, (w32(0) & 0xFF_FFFF) >> 16, 0, 0),
        _ => 0,
    }
}

/// 0xAARRGGBB as one texel of `f` into `p`.
pub fn from_argb(f: u32, c: u32, p: &mut [u8]) {
    let (a, r, g, b) = (c >> 24, c >> 16 & 0xFF, c >> 8 & 0xFF, c & 0xFF);
    let n = |x: u32| x as f32 / 255.0;
    let put16 = |p: &mut [u8], i: usize, v: u32| p[2 * i..2 * i + 2].copy_from_slice(&(v as u16).to_le_bytes());
    let put32 = |p: &mut [u8], i: usize, v: u32| p[4 * i..4 * i + 4].copy_from_slice(&v.to_le_bytes());
    match f {
        R8G8B8A8_TYPELESS | R8G8B8A8_UNORM | R8G8B8A8_UNORM_SRGB | R8G8B8A8_UINT => p[..4].copy_from_slice(&[r as u8, g as u8, b as u8, a as u8]),
        B8G8R8A8_UNORM | B8G8R8A8_TYPELESS | B8G8R8A8_UNORM_SRGB | B8G8R8X8_UNORM | B8G8R8X8_TYPELESS | B8G8R8X8_UNORM_SRGB => put32(p, 0, c),
        R10G10B10A2_TYPELESS | R10G10B10A2_UNORM => put32(p, 0, (a >> 6) << 30 | (b << 2 | b >> 6) << 20 | (g << 2 | g >> 6) << 10 | (r << 2 | r >> 6)),
        R16G16B16A16_FLOAT | R16G16B16A16_TYPELESS => {
            for (i, x) in [r, g, b, a].into_iter().enumerate() {
                put16(p, i, to_half(n(x)) as u32);
            }
        }
        R16G16B16A16_UNORM => {
            for (i, x) in [r, g, b, a].into_iter().enumerate() {
                put16(p, i, x << 8 | x);
            }
        }
        R32G32B32A32_FLOAT | R32G32B32A32_TYPELESS => {
            for (i, x) in [r, g, b, a].into_iter().enumerate() {
                put32(p, i, n(x).to_bits());
            }
        }
        R32G32_FLOAT => {
            put32(p, 0, n(r).to_bits());
            put32(p, 1, n(g).to_bits());
        }
        R16G16_FLOAT | R16G16_TYPELESS => {
            put16(p, 0, to_half(n(r)) as u32);
            put16(p, 1, to_half(n(g)) as u32);
        }
        R32_FLOAT | R32_TYPELESS => put32(p, 0, n(r).to_bits()),
        R16_FLOAT => put16(p, 0, to_half(n(r)) as u32),
        R16_UNORM | R16_TYPELESS => put16(p, 0, r << 8 | r),
        R8G8_UNORM | R8G8_TYPELESS => p[..2].copy_from_slice(&[r as u8, g as u8]),
        R8_UNORM | R8_TYPELESS => p[0] = r as u8,
        A8_UNORM => p[0] = a as u8,
        B5G6R5_UNORM => put16(p, 0, (r >> 3) << 11 | (g >> 2) << 5 | b >> 3),
        B5G5R5A1_UNORM => put16(p, 0, (a >> 7) << 15 | (r >> 3) << 10 | (g >> 3) << 5 | b >> 3),
        B4G4R4A4_UNORM => put16(p, 0, (a >> 4) << 12 | (r >> 4) << 8 | (g >> 4) << 4 | b >> 4),
        R11G11B10_FLOAT => {
            // Positive values only: the exponent and the top mantissa bits.
            let pack = |x: u32, mb: u32| -> u32 {
                let h = to_half(n(x)) as u32;
                (h >> (10 - mb)) & ((1 << (5 + mb)) - 1)
            };
            put32(p, 0, pack(r, 6) | pack(g, 6) << 11 | pack(b, 5) << 22)
        }
        _ => {}
    }
}

/// A `w` x `h` image of format `f` (rows `pitch` bytes apart) as
/// 0xAARRGGBB texels, rows packed.
pub fn decode(f: u32, w: u32, h: u32, src: &[u8], pitch: u32) -> Vec<u32> {
    let (w, h) = (w as usize, h as usize);
    let mut out = vec![0u32; w * h];
    let Some(bpp) = bytes(f) else { return out };
    let bpp = bpp as usize;
    if is_bc(f) {
        for by in 0..h.div_ceil(4) {
            for bx in 0..w.div_ceil(4) {
                let at = by * pitch as usize + bx * bpp;
                let Some(b) = src.get(at..at + bpp) else { continue };
                let block = bc_block(f, b);
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
    for y in 0..h {
        for x in 0..w {
            let at = y * pitch as usize + x * bpp;
            if let Some(p) = src.get(at..at + bpp) {
                out[y * w + x] = to_argb(f, p);
            }
        }
    }
    out
}

/// The opposite of `decode` (not for BC).
pub fn encode(f: u32, w: u32, h: u32, src: &[u32], dst: &mut [u8], pitch: u32) {
    let Some(bpp) = bytes(f) else { return };
    let bpp = bpp as usize;
    for y in 0..h as usize {
        for x in 0..w as usize {
            let at = y * pitch as usize + x * bpp;
            if let Some(p) = dst.get_mut(at..at + bpp) {
                from_argb(f, src[y * w as usize + x], p);
            }
        }
    }
}

fn x5(v: u32) -> u32 {
    (v << 3) | (v >> 2)
}
fn x6(v: u32) -> u32 {
    (v << 2) | (v >> 4)
}

/// BC4's or BC5's one channel: 8 values interpolated between two.
fn bc_channel(b: &[u8]) -> [u32; 16] {
    let (a0, a1) = (b[0] as u32, b[1] as u32);
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
    idx[..6].copy_from_slice(&b[2..8]);
    let ab = u64::from_le_bytes(idx);
    core::array::from_fn(|i| pa[(ab >> (i * 3)) as usize & 7])
}

/// The 16 texels of one BC1-5 block, rows first.
fn bc_block(f: u32, b: &[u8]) -> [u32; 16] {
    match f {
        BC4_TYPELESS | BC4_UNORM => return bc_channel(&b[..8]).map(|r| 0xFF00_0000 | r << 16),
        BC5_TYPELESS | BC5_UNORM => {
            let (r, g) = (bc_channel(&b[..8]), bc_channel(&b[8..16]));
            return core::array::from_fn(|i| 0xFF00_0000 | r[i] << 16 | g[i] << 8);
        }
        _ => {}
    }
    let bc1 = matches!(f, BC1_TYPELESS | BC1_UNORM | BC1_UNORM_SRGB);
    let color = if bc1 { &b[0..8] } else { &b[8..16] };
    let c0 = u16::from_le_bytes([color[0], color[1]]) as u32;
    let c1 = u16::from_le_bytes([color[2], color[3]]) as u32;
    let rgb = |c: u32| [x5(c >> 11), x6(c >> 5 & 63), x5(c & 31)];
    let (p0, p1) = (rgb(c0), rgb(c1));
    let mix = |a: [u32; 3], b: [u32; 3], wa: u32, wb: u32| -> [u32; 3] { core::array::from_fn(|i| (a[i] * wa + b[i] * wb) / (wa + wb)) };
    let four = !bc1 || c0 > c1;
    let pal: [[u32; 4]; 4] = if four {
        let (p2, p3) = (mix(p0, p1, 2, 1), mix(p0, p1, 1, 2));
        [[p0[0], p0[1], p0[2], 255], [p1[0], p1[1], p1[2], 255], [p2[0], p2[1], p2[2], 255], [p3[0], p3[1], p3[2], 255]]
    } else {
        let p2 = mix(p0, p1, 1, 1);
        [[p0[0], p0[1], p0[2], 255], [p1[0], p1[1], p1[2], 255], [p2[0], p2[1], p2[2], 255], [0, 0, 0, 0]]
    };
    let bits = u32::from_le_bytes([color[4], color[5], color[6], color[7]]);
    let alphas: [u32; 16] = match f {
        BC2_TYPELESS | BC2_UNORM | BC2_UNORM_SRGB => core::array::from_fn(|i| ((u64::from_le_bytes(b[0..8].try_into().unwrap()) >> (i * 4)) as u32 & 15) * 17),
        BC3_TYPELESS | BC3_UNORM | BC3_UNORM_SRGB => bc_channel(&b[0..8]),
        _ => [255; 16],
    };
    core::array::from_fn(|i| {
        let p = pal[(bits >> (i * 2)) as usize & 3];
        let a = if bc1 { p[3] } else { alphas[i] };
        a << 24 | p[0] << 16 | p[1] << 8 | p[2]
    })
}

// -----| shader registers as elements |-----

fn unorm_bits(v: u32, bits: u32) -> u32 {
    (v as f32 / ((1u64 << bits) - 1) as f32).to_bits()
}

fn snorm(v: u32, bits: u32) -> u32 {
    let s = ((v << (32 - bits)) as i32) >> (32 - bits);
    (s as f32 / ((1u32 << (bits - 1)) - 1) as f32).max(-1.0).to_bits()
}

fn sext(v: u32, bits: u32) -> u32 {
    (((v << (32 - bits)) as i32) >> (32 - bits)) as u32
}

/// An element of format `fmt` at `p` as a shader register: floats for the float
/// and normalized formats, integers for the others; missing components 0,
/// and w 1.
pub fn load_typed(fmt: u32, p: &[u8]) -> Option<[u32; 4]> {
    let w = |i: usize| -> u32 { u32::from_le_bytes(p[i * 4..i * 4 + 4].try_into().unwrap()) };
    let h = |i: usize| -> u32 { u16::from_le_bytes(p[i * 2..i * 2 + 2].try_into().unwrap()) as u32 };
    let b = |i: usize| -> u32 { p[i] as u32 };
    let one_f = 1.0f32.to_bits();
    let half = |v: u32| crate::vm::half_to_f32(v).to_bits();
    let need = bytes(fmt)? as usize;
    if p.len() < need {
        return None;
    }
    Some(match fmt {
        2 | 3 | 4 => [w(0), w(1), w(2), w(3)],
        6 => [w(0), w(1), w(2), one_f],
        7 | 8 => [w(0), w(1), w(2), 1],
        10 => [half(h(0)), half(h(1)), half(h(2)), half(h(3))],
        11 => [unorm_bits(h(0), 16), unorm_bits(h(1), 16), unorm_bits(h(2), 16), unorm_bits(h(3), 16)],
        12 => [h(0), h(1), h(2), h(3)],
        13 => [snorm(h(0), 16), snorm(h(1), 16), snorm(h(2), 16), snorm(h(3), 16)],
        14 => [sext(h(0), 16), sext(h(1), 16), sext(h(2), 16), sext(h(3), 16)],
        16 => [w(0), w(1), 0, one_f],
        17 | 18 => [w(0), w(1), 0, 1],
        24 => {
            let v = w(0);
            [unorm_bits(v & 1023, 10), unorm_bits((v >> 10) & 1023, 10), unorm_bits((v >> 20) & 1023, 10), unorm_bits(v >> 30, 2)]
        }
        25 => {
            let v = w(0);
            [v & 1023, (v >> 10) & 1023, (v >> 20) & 1023, v >> 30]
        }
        28 | 29 => [unorm_bits(b(0), 8), unorm_bits(b(1), 8), unorm_bits(b(2), 8), unorm_bits(b(3), 8)],
        30 => [b(0), b(1), b(2), b(3)],
        31 => [snorm(b(0), 8), snorm(b(1), 8), snorm(b(2), 8), snorm(b(3), 8)],
        32 => [sext(b(0), 8), sext(b(1), 8), sext(b(2), 8), sext(b(3), 8)],
        34 => [half(h(0)), half(h(1)), 0, one_f],
        35 => [unorm_bits(h(0), 16), unorm_bits(h(1), 16), 0, one_f],
        36 => [h(0), h(1), 0, 1],
        37 => [snorm(h(0), 16), snorm(h(1), 16), 0, one_f],
        38 => [sext(h(0), 16), sext(h(1), 16), 0, 1],
        41 => [w(0), 0, 0, one_f],
        42 | 43 => [w(0), 0, 0, 1],
        49 => [unorm_bits(b(0), 8), unorm_bits(b(1), 8), 0, one_f],
        50 => [b(0), b(1), 0, 1],
        51 => [snorm(b(0), 8), snorm(b(1), 8), 0, one_f],
        52 => [sext(b(0), 8), sext(b(1), 8), 0, 1],
        54 => [half(h(0)), 0, 0, one_f],
        56 => [unorm_bits(h(0), 16), 0, 0, one_f],
        57 => [h(0), 0, 0, 1],
        58 => [snorm(h(0), 16), 0, 0, one_f],
        59 => [sext(h(0), 16), 0, 0, 1],
        61 => [unorm_bits(b(0), 8), 0, 0, one_f],
        62 => [b(0), 0, 0, 1],
        63 => [snorm(b(0), 8), 0, 0, one_f],
        64 => [sext(b(0), 8), 0, 0, 1],
        87 => [unorm_bits(b(2), 8), unorm_bits(b(1), 8), unorm_bits(b(0), 8), unorm_bits(b(3), 8)],
        88 => [unorm_bits(b(2), 8), unorm_bits(b(1), 8), unorm_bits(b(0), 8), one_f],
        _ => return None,
    })
}


/// Round to nearest, halves to even (as Direct3D converts to unorm).
fn rne(x: f32) -> u32 {
    let f = x as u32 as f32;
    let d = x - f;
    let r = x as u32;
    if d > 0.5 || (d == 0.5 && r & 1 == 1) { r + 1 } else { r }
}

fn to_unorm(v: u32, bits: u32) -> u32 {
    let x = f32::from_bits(v);
    let x = if x > 0.0 { x.min(1.0) } else { 0.0 };
    rne(x * ((1u64 << bits) - 1) as f32)
}

fn to_snorm(v: u32, bits: u32) -> u32 {
    let x = f32::from_bits(v);
    let x = if x > -1.0 { x.min(1.0) } else { -1.0 };
    let m = ((1u32 << (bits - 1)) - 1) as f32;
    let s = if x < 0.0 { -(rne(-x * m) as i32) } else { rne(x * m) as i32 };
    (s as u32) & ((1u64 << bits) - 1) as u32
}

/// A shader register stored as an element of format `fmt` at `p` (as
/// `load_typed` reads it); false if the format isn't one we write.
pub fn store_typed(fmt: u32, v: [u32; 4], p: &mut [u8]) -> bool {
    let Some(n) = bytes(fmt) else { return false };
    if p.len() < n as usize {
        return false;
    }
    let mut w = |i: usize, x: u32| p[i * 4..i * 4 + 4].copy_from_slice(&x.to_le_bytes());
    let hf = |x: u32| to_half(f32::from_bits(x)) as u32;
    match fmt {
        2..=4 => (0..4).for_each(|i| w(i, v[i])),
        6..=8 => (0..3).for_each(|i| w(i, v[i])),
        16..=18 => (0..2).for_each(|i| w(i, v[i])),
        41..=43 => w(0, v[0]),
        _ => {
            let (a, b, c, d) = (v[0], v[1], v[2], v[3]);
            let h = |p: &mut [u8], i: usize, x: u32| p[i * 2..i * 2 + 2].copy_from_slice(&(x as u16).to_le_bytes());
            match fmt {
                10 => [a, b, c, d].iter().enumerate().for_each(|(i, &x)| h(p, i, hf(x))),
                11 => [a, b, c, d].iter().enumerate().for_each(|(i, &x)| h(p, i, to_unorm(x, 16))),
                12 | 14 => [a, b, c, d].iter().enumerate().for_each(|(i, &x)| h(p, i, x)),
                13 => [a, b, c, d].iter().enumerate().for_each(|(i, &x)| h(p, i, to_snorm(x, 16))),
                24 => p[..4].copy_from_slice(&(to_unorm(a, 10) | to_unorm(b, 10) << 10 | to_unorm(c, 10) << 20 | to_unorm(d, 2) << 30).to_le_bytes()),
                25 => p[..4].copy_from_slice(&((a & 1023) | (b & 1023) << 10 | (c & 1023) << 20 | (d & 3) << 30).to_le_bytes()),
                28 | 29 => [a, b, c, d].iter().enumerate().for_each(|(i, &x)| p[i] = to_unorm(x, 8) as u8),
                30 | 32 => [a, b, c, d].iter().enumerate().for_each(|(i, &x)| p[i] = x as u8),
                31 => [a, b, c, d].iter().enumerate().for_each(|(i, &x)| p[i] = to_snorm(x, 8) as u8),
                34 => [a, b].iter().enumerate().for_each(|(i, &x)| h(p, i, hf(x))),
                35 => [a, b].iter().enumerate().for_each(|(i, &x)| h(p, i, to_unorm(x, 16))),
                36 | 38 => [a, b].iter().enumerate().for_each(|(i, &x)| h(p, i, x)),
                37 => [a, b].iter().enumerate().for_each(|(i, &x)| h(p, i, to_snorm(x, 16))),
                49 => [a, b].iter().enumerate().for_each(|(i, &x)| p[i] = to_unorm(x, 8) as u8),
                50 | 52 => [a, b].iter().enumerate().for_each(|(i, &x)| p[i] = x as u8),
                51 => [a, b].iter().enumerate().for_each(|(i, &x)| p[i] = to_snorm(x, 8) as u8),
                54 => h(p, 0, hf(a)),
                56 => h(p, 0, to_unorm(a, 16)),
                57 | 59 => h(p, 0, a),
                58 => h(p, 0, to_snorm(a, 16)),
                61 => p[0] = to_unorm(a, 8) as u8,
                62 | 64 => p[0] = a as u8,
                63 => p[0] = to_snorm(a, 8) as u8,
                87 | 88 => {
                    p[0] = to_unorm(c, 8) as u8;
                    p[1] = to_unorm(b, 8) as u8;
                    p[2] = to_unorm(a, 8) as u8;
                    p[3] = if fmt == 87 { to_unorm(d, 8) as u8 } else { 255 };
                }
                _ => return false,
            }
        }
    }
    true
}
