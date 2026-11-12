//! Drawing: Direct3D 9's fixed-function pipeline in user mode, with real
//! floating point (vertex fetch through declarations and FVFs, transform,
//! lighting, texture coordinate generation and transforms, fog, clipping,
//! culling, primitive assembly), down to window-space primitives in fixed
//! point for the kernel's rasterizer (src/raster3d.rs), whose texture stages
//! are Direct3D's own. Clear and Present too.
//!
//! Direct3D 9 puts pixel centres on integer coordinates; the rasterizer
//! has them at +0.5, so window positions move by half a pixel on the way.

use alloc::vec;
use alloc::vec::Vec;

use gfx3d::raster3d::{self as k, ClearArgs, Group, Sampler, Stage, Tri, Vert};

use crate::com::*;
use crate::device::Device;
use crate::format;
use crate::res::Element;
use crate::state::*;

const FX: f64 = 4294967296.0;
const SUB: f64 = 256.0;
const DEPTH_MAX: f64 = 16777215.0;

#[link(name = "user32", kind = "raw-dylib")]
unsafe extern "system" {
    fn GetDC(hwnd: usize) -> usize;
    fn ReleaseDC(hwnd: usize, hdc: usize) -> i32;
    fn GetClientRect(hwnd: usize, r: *mut [i32; 4]) -> i32;
}

#[link(name = "gdi32", kind = "raw-dylib")]
unsafe extern "system" {
    fn StretchDIBits(hdc: usize, dx: i32, dy: i32, dw: i32, dh: i32, sx: i32, sy: i32, sw: i32, sh: i32, bits: *const u8, bmi: *const u8, usage: u32, rop: u32) -> i32;
}

// -----| maths |-----

/// a * b, row-major (Direct3D's row vectors: v * a * b).
pub fn mat_mul(a: &[f32; 16], b: &[f32; 16]) -> [f32; 16] {
    core::array::from_fn(|i| {
        let (r, c) = (i / 4, i % 4);
        (0..4).map(|k| a[r * 4 + k] * b[k * 4 + c]).sum()
    })
}

fn xform(v: [f32; 4], m: &[f32; 16]) -> [f32; 4] {
    core::array::from_fn(|c| v[0] * m[c] + v[1] * m[4 + c] + v[2] * m[8 + c] + v[3] * m[12 + c])
}

fn xform3(v: [f32; 3], m: &[f32; 16]) -> [f32; 3] {
    core::array::from_fn(|c| v[0] * m[c] + v[1] * m[4 + c] + v[2] * m[8 + c])
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn sqrt(x: f32) -> f32 {
    libm_sqrt(x as f64) as f32
}

fn libm_sqrt(x: f64) -> f64 {
    if x <= 0.0 {
        return 0.0;
    }
    // Newton from a bit-level guess: a few steps reach full precision.
    let mut y = f64::from_bits((x.to_bits() >> 1) + (1023u64 << 51));
    for _ in 0..6 {
        y = 0.5 * (y + x / y);
    }
    y
}

fn normalize(v: [f32; 3]) -> [f32; 3] {
    let l = sqrt(dot(v, v));
    if l == 0.0 { v } else { [v[0] / l, v[1] / l, v[2] / l] }
}

/// e^x for x <= 0 (fog).
fn exp_neg(x: f32) -> f32 {
    let x = x as f64;
    if x < -700.0 {
        return 0.0;
    }
    // e^x = 2^(x log2 e) = 2^n 2^f.
    let y = x * core::f64::consts::LOG2_E;
    let n = floor(y);
    let f = (y - n) * core::f64::consts::LN_2;
    let (mut term, mut sum) = (1.0f64, 1.0f64);
    for k in 1..20 {
        term *= f / k as f64;
        sum += term;
    }
    (sum * f64::from_bits(((n as i64 + 1023) as u64) << 52)) as f32
}

fn floor(x: f64) -> f64 {
    let t = x as i64 as f64;
    if t > x { t - 1.0 } else { t }
}

fn pow(x: f32, p: f32) -> f32 {
    if x <= 0.0 {
        return 0.0;
    }
    if p == 0.0 {
        return 1.0;
    }
    // x^p = e^(p ln x); ln by atanh series around 1 after scaling by 2^k.
    let bits = (x as f64).to_bits();
    let e = ((bits >> 52) & 0x7FF) as i64 - 1023;
    let m = f64::from_bits((bits & ((1 << 52) - 1)) | (1023 << 52));
    let z = (m - 1.0) / (m + 1.0);
    let (mut t, mut s) = (z, 0.0f64);
    for k in 0..30 {
        s += t / (2 * k + 1) as f64;
        t *= z * z;
    }
    let ln = 2.0 * s + e as f64 * core::f64::consts::LN_2;
    let v = p as f64 * ln;
    if v > 0.0 { 1.0 / exp_neg(-v as f32) } else { exp_neg(v as f32) }
}

fn round(x: f64) -> f64 {
    floor(x + 0.5)
}

fn color(c: u32) -> [f32; 4] {
    [(c >> 16 & 0xFF) as f32 / 255.0, (c >> 8 & 0xFF) as f32 / 255.0, (c & 0xFF) as f32 / 255.0, (c >> 24) as f32 / 255.0]
}

// -----| vertex declarations from FVFs, and fetching |-----

const T_FLOAT1: u8 = 0;
const T_FLOAT3: u8 = 2;
const T_FLOAT4: u8 = 3;
const T_COLOR: u8 = 4;
const T_UBYTE4: u8 = 5;

const U_POSITION: u8 = 0;
const U_BLENDWEIGHT: u8 = 1;
const U_BLENDINDICES: u8 = 2;
const U_NORMAL: u8 = 3;
const U_PSIZE: u8 = 4;
const U_TEXCOORD: u8 = 5;
const U_POSITIONT: u8 = 9;
const U_COLOR: u8 = 10;

/// The declaration an FVF stands for.
pub fn fvf_elements(fvf: u32) -> Vec<Element> {
    let mut v = Vec::new();
    let mut off = 0u16;
    let mut add = |t: u8, usage: u8, index: u8, size: u16| {
        v.push(Element { stream: 0, offset: off, etype: t, method: 0, usage, index });
        off += size;
    };
    match fvf & 0x400E {
        0x002 => add(T_FLOAT3, U_POSITION, 0, 12),
        0x004 => add(T_FLOAT4, U_POSITIONT, 0, 16),
        0x4002 => add(T_FLOAT4, U_POSITION, 0, 16),
        p @ 0x006..=0x00E => {
            add(T_FLOAT3, U_POSITION, 0, 12);
            let betas = ((p - 4) / 2) as u16;
            let indexed = fvf & (0x1000 | 0x8000) != 0;
            let weights = if indexed { betas - 1 } else { betas };
            if weights > 0 {
                add(T_FLOAT1 + weights as u8 - 1, U_BLENDWEIGHT, 0, 4 * weights);
            }
            if indexed {
                add(if fvf & 0x8000 != 0 { T_COLOR } else { T_UBYTE4 }, U_BLENDINDICES, 0, 4);
            }
        }
        _ => {}
    }
    if fvf & 0x10 != 0 {
        add(T_FLOAT3, U_NORMAL, 0, 12);
    }
    if fvf & 0x20 != 0 {
        add(T_FLOAT1, U_PSIZE, 0, 4);
    }
    if fvf & 0x40 != 0 {
        add(T_COLOR, U_COLOR, 0, 4);
    }
    if fvf & 0x80 != 0 {
        add(T_COLOR, U_COLOR, 1, 4);
    }
    for i in 0..((fvf >> 8) & 0xF).min(8) {
        let n = match (fvf >> (16 + 2 * i)) & 3 {
            0 => 2,
            1 => 3,
            2 => 4,
            _ => 1,
        };
        add(T_FLOAT1 + n as u8 - 1, U_TEXCOORD, i as u8, 4 * n);
    }
    v
}

fn half(h: u16) -> f32 {
    let (s, e, m) = ((h >> 15) as u32, ((h >> 10) & 31) as u32, (h & 1023) as u32);
    let bits = if e == 0 {
        if m == 0 {
            s << 31
        } else {
            // Subnormal: normalize.
            let mut e = 127 - 15 + 1;
            let mut m = m;
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

/// One element's value (missing components 0, w 1).
fn fetch(t: u8, p: &[u8]) -> ([f32; 4], usize) {
    let f = |i: usize| f32::from_le_bytes([p[4 * i], p[4 * i + 1], p[4 * i + 2], p[4 * i + 3]]);
    let s = |i: usize| i16::from_le_bytes([p[2 * i], p[2 * i + 1]]) as f32;
    let us = |i: usize| u16::from_le_bytes([p[2 * i], p[2 * i + 1]]) as f32;
    let h = |i: usize| half(u16::from_le_bytes([p[2 * i], p[2 * i + 1]]));
    match t {
        0 => ([f(0), 0.0, 0.0, 1.0], 1),
        1 => ([f(0), f(1), 0.0, 1.0], 2),
        2 => ([f(0), f(1), f(2), 1.0], 3),
        3 => ([f(0), f(1), f(2), f(3)], 4),
        4 => ([p[2] as f32 / 255.0, p[1] as f32 / 255.0, p[0] as f32 / 255.0, p[3] as f32 / 255.0], 4),
        5 => ([p[0] as f32, p[1] as f32, p[2] as f32, p[3] as f32], 4),
        6 => ([s(0), s(1), 0.0, 1.0], 2),
        7 => ([s(0), s(1), s(2), s(3)], 4),
        8 => ([p[0] as f32 / 255.0, p[1] as f32 / 255.0, p[2] as f32 / 255.0, p[3] as f32 / 255.0], 4),
        9 => ([(s(0) / 32767.0).max(-1.0), (s(1) / 32767.0).max(-1.0), 0.0, 1.0], 2),
        10 => (core::array::from_fn(|i| (s(i) / 32767.0).max(-1.0)), 4),
        11 => ([us(0) / 65535.0, us(1) / 65535.0, 0.0, 1.0], 2),
        12 => (core::array::from_fn(|i| us(i) / 65535.0), 4),
        13 | 14 => {
            let v = u32::from_le_bytes([p[0], p[1], p[2], p[3]]);
            let c = |i: u32| {
                let x = (v >> (10 * i)) & 1023;
                if t == 13 { x as f32 } else { (((x << 22) as i32 >> 22) as f32 / 511.0).max(-1.0) }
            };
            ([c(0), c(1), c(2), 1.0], 3)
        }
        15 => ([h(0), h(1), 0.0, 1.0], 2),
        16 => ([h(0), h(1), h(2), h(3)], 4),
        _ => ([0.0, 0.0, 0.0, 1.0], 0),
    }
}

/// Element sizes in bytes, by type.
fn type_size(t: u8) -> usize {
    match t {
        0 => 4,
        1 => 8,
        2 => 12,
        3 => 16,
        4 | 5 | 8 | 13 | 14 | 9 | 11 | 15 | 6 => 4,
        7 | 10 | 12 | 16 => 8,
        _ => 0,
    }
}

/// A vertex as fetched.
#[derive(Clone, Copy)]
struct In {
    pos: [f32; 4],
    pretransformed: bool,
    normal: Option<[f32; 3]>,
    psize: Option<f32>,
    diffuse: Option<[f32; 4]>,
    specular: Option<[f32; 4]>,
    tex: [[f32; 4]; 8],
    dims: [u8; 8],
}

/// Where each stream's vertices are this draw: bytes and stride.
#[derive(Clone, Copy)]
struct Source {
    ptr: *const u8,
    len: usize,
    stride: usize,
}

/// One element of vertex `n`, if its stream has it.
fn fetch_elem(e: &Element, src: &[Source; 16], n: usize) -> Option<[f32; 4]> {
    let s = &src[(e.stream as usize).min(15)];
    let at = n * s.stride + e.offset as usize;
    let size = type_size(e.etype);
    if s.ptr.is_null() || size == 0 || at + size > s.len {
        return None;
    }
    Some(fetch(e.etype, unsafe { core::slice::from_raw_parts(s.ptr.add(at), size) }).0)
}

/// A vertex through a vertex shader: its inputs by the shader's
/// declarations, its outputs taken as the fixed-function pipeline's
/// (position, colours, texture coordinates, fog, point size).
fn vs_vertex(sh: &crate::shader::Shader, ctx: &Ctx, elems: &[Element], src: &[Source; 16], n: usize, k: &crate::shader::VsConsts, planes: &[(usize, [f32; 4])], vary: &[(u8, u8)], clamp_colors: bool) -> V {
    use crate::shader as s;
    let mut inputs = [[0.0, 0.0, 0.0, 1.0f32]; 16];
    for dc in sh.decls.iter().filter(|d| d.rtype == s::R_INPUT && d.num < 16) {
        if let Some(e) = elems.iter().find(|e| e.usage == dc.usage && e.index == dc.index) {
            if let Some(v) = fetch_elem(e, src, n) {
                inputs[dc.num as usize] = v;
            }
        }
    }
    let outs = sh.run_vs(&inputs, k);
    let get = |u: u8, i: u8| outs.iter().find(|o| o.0 == u && o.1 == i).map(|o| o.2);
    let mut o = V::default();
    o.c = get(s::U_POSITION, 0).unwrap_or([0.0, 0.0, 0.0, 1.0]);
    o.d = get(s::U_COLOR, 0).unwrap_or([1.0; 4]).map(|x| x.clamp(0.0, 1.0));
    o.s = get(s::U_COLOR, 1).unwrap_or([0.0; 4]).map(|x| x.clamp(0.0, 1.0));
    for (i, slot) in ctx.slots.iter().enumerate() {
        let Some((_, flags, _)) = slot else { continue };
        let c = get(s::U_TEXCOORD, i as u8).unwrap_or([0.0, 0.0, 0.0, 1.0]);
        let count = (flags & 0xFF) as usize;
        let q = if flags & 0x100 != 0 && count >= 2 { c[count - 1] } else { 1.0 };
        o.t[i] = [c[0], c[1], q];
    }
    o.fog = match ctx.fog {
        Fog::Table => o.c[3],
        _ => get(s::U_FOG, 0).map_or(1.0, |f| f[0]),
    };
    o.psize = get(s::U_PSIZE, 0).map_or(ctx.point_size, |p| p[0]).clamp(ctx.point_min_max[0], ctx.point_min_max[1].max(ctx.point_min_max[0]));
    // With a vertex shader, user clip planes are in clip space.
    for &(i, p) in planes {
        o.u[i] = p[0] * o.c[0] + p[1] * o.c[1] + p[2] * o.c[2] + p[3] * o.c[3];
    }
    // A pixel shader's inputs, by usage (ps_2_0's colours saturated).
    for (k, &(u, i)) in vary.iter().enumerate() {
        let v = get(u, i).unwrap_or([0.0, 0.0, 0.0, 1.0]);
        o.outs[k] = if u == s::U_COLOR && clamp_colors { v.map(|x| x.clamp(0.0, 1.0)) } else { v };
    }
    o
}

/// A fixed-function vertex's values as a pixel shader's inputs.
fn ff_outs(o: &mut V, vary: &[(u8, u8)]) {
    use crate::shader as s;
    for (k, &(u, i)) in vary.iter().enumerate() {
        o.outs[k] = match (u, i) {
            (s::U_COLOR, 0) => o.d,
            (s::U_COLOR, 1) => o.s,
            (s::U_TEXCOORD, n) if (n as usize) < k::SLOTS => {
                let t = o.t[n as usize];
                [t[0], t[1], 0.0, t[2]]
            }
            (s::U_FOG, 0) => [o.fog; 4],
            _ => [0.0, 0.0, 0.0, 1.0],
        };
    }
}

fn fetch_vertex(elems: &[Element], src: &[Source; 16], n: usize) -> In {
    let mut v = In { pos: [0.0, 0.0, 0.0, 1.0], pretransformed: false, normal: None, psize: None, diffuse: None, specular: None, tex: [[0.0, 0.0, 0.0, 1.0]; 8], dims: [0; 8] };
    for e in elems {
        let s = &src[(e.stream as usize).min(15)];
        let at = n * s.stride + e.offset as usize;
        let size = type_size(e.etype);
        if s.ptr.is_null() || size == 0 || at + size > s.len {
            continue;
        }
        let bytes = unsafe { core::slice::from_raw_parts(s.ptr.add(at), size) };
        let (val, dims) = fetch(e.etype, bytes);
        match (e.usage, e.index) {
            (U_POSITION, 0) => v.pos = val,
            (U_POSITIONT, 0) => {
                v.pos = val;
                v.pretransformed = true;
            }
            (U_NORMAL, 0) => v.normal = Some([val[0], val[1], val[2]]),
            (U_PSIZE, 0) => v.psize = Some(val[0]),
            (U_COLOR, 0) => v.diffuse = Some(val),
            (U_COLOR, 1) => v.specular = Some(val),
            (U_TEXCOORD, i) if i < 8 => {
                v.tex[i as usize] = val;
                v.dims[i as usize] = dims as u8;
            }
            _ => {}
        }
    }
    v
}

// -----| per-vertex processing |-----

/// A vertex in clip space (or, pretransformed, in the window: x, y, z, 1/rhw).
#[derive(Clone, Copy, Default)]
struct V {
    c: [f32; 4],
    d: [f32; 4],
    s: [f32; 4],
    /// Each texture slot's (s, t, q): its position is (s/q, t/q).
    t: [[f32; 3]; k::SLOTS],
    /// Fog: the factor (vertex fog), or the distance (per pixel).
    fog: f32,
    /// Distances from the user clip planes.
    u: [f32; 6],
    psize: f32,
    /// A pixel shader's inputs, in the order its program numbers them.
    outs: [[f32; 4]; k::PS_INPUTS],
}

fn lerp_v(a: &V, b: &V, t: f32) -> V {
    let l = |x: f32, y: f32| x + (y - x) * t;
    let l4 = |x: [f32; 4], y: [f32; 4]| core::array::from_fn(|i| l(x[i], y[i]));
    V {
        c: l4(a.c, b.c),
        d: l4(a.d, b.d),
        s: l4(a.s, b.s),
        t: core::array::from_fn(|i| core::array::from_fn(|j| l(a.t[i][j], b.t[i][j]))),
        fog: l(a.fog, b.fog),
        u: core::array::from_fn(|i| l(a.u[i], b.u[i])),
        psize: l(a.psize, b.psize),
        outs: core::array::from_fn(|i| l4(a.outs[i], b.outs[i])),
    }
}

struct EyeLight {
    l: Light,
    /// Position and direction in eye space.
    pos: [f32; 3],
    dir: [f32; 3],
}

/// Fog: per pixel (TABLE), per vertex, or from the specular alpha.
#[derive(Clone, Copy, PartialEq)]
enum Fog {
    None,
    Table,
    Vertex,
    Specular,
}

/// What every vertex of a draw is processed with.
struct Ctx {
    world_view: [f32; 16],
    proj: [f32; 16],
    world: [f32; 16],
    normal_m: [f32; 16],
    lighting: bool,
    lights: Vec<EyeLight>,
    ambient: [f32; 4],
    material: Material,
    /// Each material colour's source: 0 material, 1 diffuse, 2 specular.
    sources: [u32; 4],
    specular: bool,
    local_viewer: bool,
    normalize: bool,
    fog: Fog,
    fog_mode: u32,
    fog_params: [f32; 3],
    range_fog: bool,
    /// Slots textured: the stage's coordinate index, transform flags and
    /// matrix.
    slots: [Option<(u32, u32, [f32; 16])>; k::SLOTS],
    clip_planes: Vec<(usize, [f32; 4])>,
    point_size: f32,
    point_scale: Option<[f32; 3]>,
    point_min_max: [f32; 2],
    viewport_h: f32,
}

fn fog_factor(mode: u32, p: [f32; 3], d: f32) -> f32 {
    let f = match mode {
        1 => exp_neg(-(d * p[2])),
        2 => exp_neg(-(d * p[2]) * (d * p[2])),
        3 => {
            if p[1] == p[0] {
                1.0
            } else {
                (p[1] - d) / (p[1] - p[0])
            }
        }
        _ => 1.0,
    };
    // Not clamped here: GPUs interpolate the factor and clamp per pixel
    // (linear fog past the end goes below 0).
    f
}

/// Inverse transpose of the upper 3x3 (for normals).
fn normal_matrix(m: &[f32; 16]) -> [f32; 16] {
    let a = |r: usize, c: usize| m[r * 4 + c] as f64;
    let det = a(0, 0) * (a(1, 1) * a(2, 2) - a(1, 2) * a(2, 1)) - a(0, 1) * (a(1, 0) * a(2, 2) - a(1, 2) * a(2, 0)) + a(0, 2) * (a(1, 0) * a(2, 1) - a(1, 1) * a(2, 0));
    let id = if det == 0.0 { 0.0 } else { 1.0 / det };
    // Cofactor matrix / det = the inverse's transpose.
    let cof = |r: usize, c: usize| {
        let (r1, r2) = ((r + 1) % 3, (r + 2) % 3);
        let (c1, c2) = ((c + 1) % 3, (c + 2) % 3);
        ((a(r1, c1) * a(r2, c2) - a(r1, c2) * a(r2, c1)) * id) as f32
    };
    let mut out = IDENTITY;
    for r in 0..3 {
        for c in 0..3 {
            out[r * 4 + c] = cof(r, c);
        }
    }
    out
}

impl Ctx {
    fn new(d: &Device) -> Ctx {
        let s = &d.state;
        let world = s.transforms[10];
        let view = s.transforms[0];
        let proj = s.transforms[1];
        let world_view = mat_mul(&world, &view);
        let lights = s
            .lights
            .iter()
            .flatten()
            .filter(|l| l.1)
            .take(8)
            .map(|(l, _)| {
                let p = xform([l.position[0], l.position[1], l.position[2], 1.0], &view);
                EyeLight { l: *l, pos: [p[0], p[1], p[2]], dir: normalize(xform3(l.direction, &view)) }
            })
            .collect();
        let fog = if s.rs[rs::FOGENABLE] == 0 {
            Fog::None
        } else if s.rs[rs::FOGTABLEMODE] != 0 {
            Fog::Table
        } else if s.rs[rs::FOGVERTEXMODE] != 0 {
            Fog::Vertex
        } else {
            Fog::Specular
        };
        let fog_mode = if fog == Fog::Table { s.rs[rs::FOGTABLEMODE] } else { s.rs[rs::FOGVERTEXMODE] };
        let mut slots = [None; k::SLOTS];
        for (i, slot) in slots.iter_mut().enumerate() {
            if s.tss[i][tss::COLOROP] <= 1 {
                break;
            }
            if !s.textures[i].is_null() {
                *slot = Some((s.tss[i][tss::TEXCOORDINDEX], s.tss[i][tss::TEXTURETRANSFORMFLAGS], s.transforms[2 + i]));
            }
        }
        let cv = s.rs[rs::COLORVERTEX] != 0;
        let src = |r: usize| if cv { s.rs[r] } else { 0 };
        Ctx {
            world_view,
            proj,
            world,
            normal_m: normal_matrix(&world_view),
            lighting: s.rs[rs::LIGHTING] != 0,
            lights,
            ambient: color(s.rs[rs::AMBIENT]),
            material: s.material,
            sources: [src(rs::DIFFUSEMATERIALSOURCE), src(rs::AMBIENTMATERIALSOURCE), src(rs::SPECULARMATERIALSOURCE), src(rs::EMISSIVEMATERIALSOURCE)],
            specular: s.rs[rs::SPECULARENABLE] != 0,
            local_viewer: s.rs[rs::LOCALVIEWER] != 0,
            normalize: s.rs[rs::NORMALIZENORMALS] != 0,
            fog,
            fog_mode,
            fog_params: [s.rsf(rs::FOGSTART), s.rsf(rs::FOGEND), s.rsf(rs::FOGDENSITY)],
            range_fog: s.rs[rs::RANGEFOGENABLE] != 0,
            slots,
            clip_planes: (0..6).filter(|&i| s.rs[rs::CLIPPLANEENABLE] & (1 << i) != 0).map(|i| (i, s.clip_planes[i])).collect(),
            point_size: s.rsf(rs::POINTSIZE),
            point_scale: (s.rs[rs::POINTSCALEENABLE] != 0).then(|| [s.rsf(rs::POINTSCALE_A), s.rsf(rs::POINTSCALE_B), s.rsf(rs::POINTSCALE_C)]),
            point_min_max: [s.rsf(rs::POINTSIZE_MIN), s.rsf(rs::POINTSIZE_MAX)],
            viewport_h: s.viewport.h as f32,
        }
    }

    /// Direct3D's lighting at eye position `e` with eye normal `n`: the
    /// diffuse and specular colours.
    fn light(&self, e: [f32; 3], n: [f32; 3], vd: Option<[f32; 4]>, vs: Option<[f32; 4]>) -> ([f32; 4], [f32; 4]) {
        let m = &self.material;
        let pick = |src: u32, mat: [f32; 4]| match src {
            1 => vd.unwrap_or(mat),
            2 => vs.unwrap_or(mat),
            _ => mat,
        };
        let (md, ma, ms, me) = (pick(self.sources[0], m.diffuse), pick(self.sources[1], m.ambient), pick(self.sources[2], m.specular), pick(self.sources[3], m.emissive));
        let mut amb = self.ambient;
        let mut dif = [0.0f32; 3];
        let mut spe = [0.0f32; 3];
        let view_dir = if self.local_viewer { normalize([-e[0], -e[1], -e[2]]) } else { [0.0, 0.0, -1.0] };
        for li in &self.lights {
            let l = &li.l;
            let (ldir, atten) = if l.ltype == 3 {
                ([-li.dir[0], -li.dir[1], -li.dir[2]], 1.0)
            } else {
                let to = [li.pos[0] - e[0], li.pos[1] - e[1], li.pos[2] - e[2]];
                let dist = sqrt(dot(to, to));
                if dist > l.range {
                    continue;
                }
                let a = l.att[0] + l.att[1] * dist + l.att[2] * dist * dist;
                (normalize(to), if a > 0.0 { 1.0 / a } else { 1.0 })
            };
            let mut f = atten;
            if l.ltype == 2 {
                let rho = -dot(ldir, li.dir);
                let (ct, cp) = (cos(l.theta / 2.0), cos(l.phi / 2.0));
                f *= if rho > ct {
                    1.0
                } else if rho <= cp {
                    0.0
                } else {
                    pow((rho - cp) / (ct - cp), l.falloff)
                };
            }
            if f <= 0.0 {
                continue;
            }
            for i in 0..3 {
                amb[i] += l.ambient[i] * f;
            }
            let ndl = dot(n, ldir);
            if ndl > 0.0 {
                for i in 0..3 {
                    dif[i] += l.diffuse[i] * ndl * f;
                }
                if self.specular {
                    let h = normalize([ldir[0] + view_dir[0], ldir[1] + view_dir[1], ldir[2] + view_dir[2]]);
                    let ndh = dot(n, h);
                    if ndh > 0.0 {
                        let p = pow(ndh, m.power) * f;
                        for i in 0..3 {
                            spe[i] += l.specular[i] * p;
                        }
                    }
                }
            }
        }
        let d = [me[0] + ma[0] * amb[0] + md[0] * dif[0], me[1] + ma[1] * amb[1] + md[1] * dif[1], me[2] + ma[2] * amb[2] + md[2] * dif[2], md[3]];
        let s = [ms[0] * spe[0], ms[1] * spe[1], ms[2] * spe[2], ms[3]];
        (d.map(|x| x.clamp(0.0, 1.0)), s.map(|x| x.clamp(0.0, 1.0)))
    }

    fn process(&self, v: &In) -> V {
        let mut o = V::default();
        if v.pretransformed {
            let rhw = if v.pos[3] == 0.0 { 1.0 } else { v.pos[3] };
            o.c = [v.pos[0], v.pos[1], v.pos[2], 1.0 / rhw];
            o.d = v.diffuse.unwrap_or([1.0; 4]);
            o.s = v.specular.unwrap_or([0.0; 4]);
            o.fog = match self.fog {
                Fog::Table => v.pos[2],
                _ => o.s[3],
            };
            self.texcoords(v, &mut o, [0.0; 3], [0.0; 3]);
            o.psize = v.psize.unwrap_or(self.point_size);
            return o;
        }
        let pos = if v.pos[3] == 0.0 { [v.pos[0], v.pos[1], v.pos[2], 1.0] } else { v.pos };
        let eye4 = xform(pos, &self.world_view);
        let e = [eye4[0], eye4[1], eye4[2]];
        o.c = xform(eye4, &self.proj);
        let n = v.normal.map(|n| {
            let n = xform3(n, &self.normal_m);
            if self.normalize { normalize(n) } else { n }
        });
        if self.lighting {
            let (d, s) = self.light(e, n.unwrap_or([0.0; 3]), v.diffuse, v.specular);
            (o.d, o.s) = (d, s);
        } else {
            o.d = v.diffuse.unwrap_or([1.0; 4]);
            o.s = v.specular.unwrap_or([0.0; 4]);
        }
        let dist = if self.range_fog { sqrt(dot(e, e)) } else { e[2].abs() };
        o.fog = match self.fog {
            Fog::Vertex => fog_factor(self.fog_mode, self.fog_params, dist),
            // Per pixel by eye depth (w), or the window's z.
            Fog::Table => o.c[3],
            Fog::Specular => o.s[3],
            Fog::None => 1.0,
        };
        if !self.clip_planes.is_empty() {
            let wp = xform(pos, &self.world);
            for &(i, p) in &self.clip_planes {
                o.u[i] = p[0] * wp[0] + p[1] * wp[1] + p[2] * wp[2] + p[3] * wp[3];
            }
        }
        self.texcoords(v, &mut o, e, n.map(normalize).unwrap_or([0.0; 3]));
        o.psize = v.psize.unwrap_or(self.point_size);
        if let Some([a, b, c]) = self.point_scale {
            let d = sqrt(dot(e, e));
            let q = a + b * d + c * d * d;
            o.psize = self.viewport_h * o.psize * if q > 0.0 { sqrt(1.0 / q) } else { 1.0 };
        }
        o.psize = o.psize.clamp(self.point_min_max[0], self.point_min_max[1].max(self.point_min_max[0]));
        o
    }

    /// Each textured slot's coordinates: picked or generated, transformed,
    /// and how they're projected.
    fn texcoords(&self, v: &In, o: &mut V, e: [f32; 3], n: [f32; 3]) {
        for (i, slot) in self.slots.iter().enumerate() {
            let Some((index, flags, m)) = slot else { continue };
            let refl = || {
                let ev = if self.local_viewer { normalize(e) } else { [0.0, 0.0, 1.0] };
                let d = 2.0 * dot(ev, n);
                [ev[0] - d * n[0], ev[1] - d * n[1], ev[2] - d * n[2]]
            };
            let (mut c, dims) = match index & 0xFFFF_0000 {
                0x1_0000 => ([n[0], n[1], n[2], 1.0], 3),
                0x2_0000 => ([e[0], e[1], e[2], 1.0], 3),
                0x3_0000 => {
                    let r = refl();
                    ([r[0], r[1], r[2], 1.0], 3)
                }
                0x4_0000 => {
                    let r = refl();
                    let mm = 2.0 * sqrt(r[0] * r[0] + r[1] * r[1] + (r[2] + 1.0) * (r[2] + 1.0));
                    let f = |x: f32| if mm == 0.0 { 0.5 } else { x / mm + 0.5 };
                    ([f(r[0]), f(r[1]), 1.0, 0.0], 2)
                }
                _ => {
                    let j = (index & 7) as usize;
                    (v.tex[j], v.dims[j] as usize)
                }
            };
            let count = (flags & 0xFF) as usize;
            if count != 0 {
                // Missing components: 1 just after the last, 0 beyond.
                let mut x = c;
                for (k, xk) in x.iter_mut().enumerate().skip(dims) {
                    *xk = if k == dims { 1.0 } else { 0.0 };
                }
                c = xform(x, m);
            }
            let q = if flags & 0x100 != 0 && count >= 2 { c[count - 1] } else { 1.0 };
            o.t[i] = [c[0], c[1], q];
        }
    }
}

fn cos(x: f32) -> f32 {
    // Reduced to [-pi, pi], then a Taylor series.
    let pi = core::f64::consts::PI;
    let mut x = x as f64;
    x -= 2.0 * pi * floor(x / (2.0 * pi) + 0.5);
    let (mut term, mut sum) = (1.0f64, 1.0f64);
    for k in 1..12 {
        term *= -x * x / ((2 * k - 1) * (2 * k)) as f64;
        sum += term;
    }
    sum as f32
}

// -----| clipping |-----

/// Clip a polygon (clip space) to the view volume (0 <= z <= w) and the
/// user planes `planes` (bits).
fn clip_poly(mut p: Vec<V>, planes: u32) -> Vec<V> {
    let dist = |v: &V, i: u32| -> f32 {
        match i {
            0 => v.c[3] + v.c[0],
            1 => v.c[3] - v.c[0],
            2 => v.c[3] + v.c[1],
            3 => v.c[3] - v.c[1],
            4 => v.c[2],
            5 => v.c[3] - v.c[2],
            _ => v.u[(i - 6) as usize],
        }
    };
    for i in 0..12 {
        if i >= 6 && planes & (1 << (i - 6)) == 0 {
            continue;
        }
        if p.iter().all(|v| dist(v, i) >= 0.0) {
            continue;
        }
        let mut out = Vec::with_capacity(p.len() + 2);
        for j in 0..p.len() {
            let (a, b) = (&p[j], &p[(j + 1) % p.len()]);
            let (da, db) = (dist(a, i), dist(b, i));
            if da >= 0.0 {
                out.push(*a);
            }
            if (da >= 0.0) != (db >= 0.0) {
                out.push(lerp_v(a, b, da / (da - db)));
            }
        }
        p = out;
        if p.len() < 3 {
            return Vec::new();
        }
    }
    p
}

fn clip_line(a: V, b: V, planes: u32) -> Option<(V, V)> {
    let dist = |v: &V, i: u32| -> f32 {
        match i {
            0 => v.c[3] + v.c[0],
            1 => v.c[3] - v.c[0],
            2 => v.c[3] + v.c[1],
            3 => v.c[3] - v.c[1],
            4 => v.c[2],
            5 => v.c[3] - v.c[2],
            _ => v.u[(i - 6) as usize],
        }
    };
    let (mut a, mut b) = (a, b);
    for i in 0..12 {
        if i >= 6 && planes & (1 << (i - 6)) == 0 {
            continue;
        }
        let (da, db) = (dist(&a, i), dist(&b, i));
        if da < 0.0 && db < 0.0 {
            return None;
        }
        if da < 0.0 {
            a = lerp_v(&a, &b, da / (da - db));
        } else if db < 0.0 {
            b = lerp_v(&a, &b, da / (da - db));
        }
    }
    Some((a, b))
}

// -----| window space and the kernel |-----

/// A vertex in the window (Direct3D's coordinates, y down).
#[derive(Clone, Copy)]
struct W {
    x: f64,
    y: f64,
    z: f64,
    /// 1/w.
    q: f64,
    v: V,
}

/// Everything about the draw that the kernel needs per primitive.
struct Out {
    group: Group,
    /// The group for counter-clockwise triangles (two-sided stencil).
    group_ccw: Option<Group>,
    pre: bool,
    viewport: Viewport,
    /// Each slot's texture size (0: none).
    tex_size: [(f64, f64); k::SLOTS],
    cull: u32,
    fill: u32,
    flat: bool,
    bias: (f64, f64),
    planes: u32,
    sprite: bool,
    /// A pixel shader: its constant block, and its inputs' usages.
    ps_consts: Option<Vec<f32>>,
    vary: Vec<(u8, u8)>,
    /// Nothing pushed yet: a shaded draw starts a group of its own, since
    /// its constants may differ from the last draw's.
    fresh: core::cell::Cell<bool>,
}

impl Out {
    fn window(&self, v: &V) -> W {
        if self.pre {
            return W { x: v.c[0] as f64, y: v.c[1] as f64, z: v.c[2] as f64, q: 1.0 / v.c[3] as f64, v: *v };
        }
        let vp = &self.viewport;
        let iw = 1.0 / v.c[3] as f64;
        W {
            x: vp.x as f64 + (1.0 + v.c[0] as f64 * iw) * vp.w as f64 / 2.0,
            y: vp.y as f64 + (1.0 - v.c[1] as f64 * iw) * vp.h as f64 / 2.0,
            z: vp.min_z as f64 + v.c[2] as f64 * iw * (vp.max_z - vp.min_z) as f64,
            q: iw,
            v: *v,
        }
    }

    /// The kernel's vertices for `ws` (q relative to the largest).
    fn kverts<const N: usize>(&self, ws: &[W; N], zbias: f64) -> [Vert; N] {
        let qmax = ws.iter().map(|w| w.q).fold(0.0f64, f64::max);
        let fx = |x: f64| (x * FX) as i64;
        let ch = |c: f32| fx(c.clamp(0.0, 1.0) as f64 * 255.0 + 0.5);
        core::array::from_fn(|i| {
            let w = &ws[i];
            let q = if qmax > 0.0 { w.q / qmax } else { 1.0 };
            let v = &w.v;
            Vert {
                x: round((w.x + 0.5) * SUB).clamp(-8e6 * SUB, 8e6 * SUB) as i32,
                y: round((w.y + 0.5) * SUB).clamp(-8e6 * SUB, 8e6 * SUB) as i32,
                z: fx(((w.z + zbias).clamp(0.0, 1.0)) * DEPTH_MAX),
                q: fx(q),
                r: ch(v.d[0]),
                g: ch(v.d[1]),
                b: ch(v.d[2]),
                a: ch(v.d[3]),
                // Vertex fog: the factor; per pixel: 1/w, or the depth.
                f: if self.group.fog_mode == 0 {
                    fx(v.fog as f64 * 255.0 + 0.5)
                } else if self.group.fog_mode & k::FOG_INVERSE != 0 {
                    fx(1.0 / (v.fog as f64).max(1e-9))
                } else {
                    fx(w.z)
                },
                spec: [ch(v.s[0]), ch(v.s[1]), ch(v.s[2])],
                uv: core::array::from_fn(|s| {
                    let (tw, th) = self.tex_size[s];
                    let t = v.t[s];
                    let iq = if t[2] == 0.0 { 0.0 } else { 1.0 / t[2] as f64 };
                    [fx(t[0] as f64 * iq * tw * q), fx(t[1] as f64 * iq * th * q)]
                }),
            }
        })
    }
}

fn push(d: &mut Device, g: &Group, t: Tri, o: &Out, rec: Option<&[f32]>) {
    let b = &mut d.batch;
    let fresh = o.fresh.replace(false) && o.ps_consts.is_some();
    let same = !fresh && b.groups.last().is_some_and(|l| {
        let mut l = *l;
        (l.first, l.count, l.attr_first, l.const_first) = (0, 0, g.attr_first, g.const_first);
        l == *g
    });
    if !same {
        let mut n = *g;
        n.first = b.tris.len() as u32;
        // A shaded group: its constants, then its primitives' planes.
        if let Some(c) = &o.ps_consts {
            n.const_first = b.attrs.len() as u32;
            b.attrs.extend_from_slice(c);
            n.attr_first = b.attrs.len() as u32;
        }
        b.groups.push(n);
    }
    if let Some(r) = rec {
        b.attrs.extend_from_slice(r);
    }
    b.groups.last_mut().unwrap().count += 1;
    b.tris.push(t);
    if b.tris.len() >= 1 << 16 || b.attrs.len() >= 1 << 22 {
        d.flush();
    }
}

/// A triangle in clip space: clipped, culled, and filled (or outlined, or
/// its corners) as the state says.
fn triangle(d: &mut Device, o: &Out, a: &V, b: &V, c: &V, provoking: &V) {
    let (mut a, mut b, mut c) = (*a, *b, *c);
    if o.flat {
        for v in [&mut a, &mut b, &mut c] {
            (v.d, v.s) = (provoking.d, provoking.s);
        }
    }
    let poly = if o.pre { vec![a, b, c] } else { clip_poly(vec![a, b, c], o.planes) };
    if poly.len() < 3 {
        return;
    }
    let ws: Vec<W> = poly.iter().map(|v| o.window(v)).collect();
    // Orientation in the window (y down): positive is clockwise.
    let mut area = 0.0;
    for i in 0..ws.len() {
        let (p, q) = (&ws[i], &ws[(i + 1) % ws.len()]);
        area += p.x * q.y - q.x * p.y;
    }
    let cw = area > 0.0;
    if (o.cull == 2 && cw) || (o.cull == 3 && !cw) || area == 0.0 {
        return;
    }
    let g = if !cw { o.group_ccw.as_ref().unwrap_or(&o.group) } else { &o.group };
    // Depth bias: constant, plus slope-scaled by the plane's steepest z.
    let zb = if o.bias != (0.0, 0.0) {
        let (p0, p1, p2) = (&ws[0], &ws[1], &ws[2]);
        let det = (p1.x - p0.x) * (p2.y - p0.y) - (p2.x - p0.x) * (p1.y - p0.y);
        let (dzdx, dzdy) = if det == 0.0 {
            (0.0, 0.0)
        } else {
            (((p1.z - p0.z) * (p2.y - p0.y) - (p2.z - p0.z) * (p1.y - p0.y)) / det, ((p2.z - p0.z) * (p1.x - p0.x) - (p1.z - p0.z) * (p2.x - p0.x)) / det)
        };
        o.bias.0 + o.bias.1 * dzdx.abs().max(dzdy.abs())
    } else {
        0.0
    };
    match o.fill {
        1 => {
            for w in &ws {
                point_at(d, o, w);
            }
        }
        2 => {
            let mut lg = *g;
            (lg.kind, lg.size) = (k::KIND_LINES, k::SUB);
            for i in 0..ws.len() {
                let edge = [ws[i], ws[(i + 1) % ws.len()]];
                let kv = o.kverts(&edge, zb);
                let rec = o.ps_consts.as_ref().map(|_| record(o, &kv, &edge, if cw { 1.0 } else { -1.0 }));
                push(d, &lg, Tri { v: [kv[0], kv[1], Vert::default()] }, o, rec.as_deref());
            }
        }
        _ => {
            for i in 1..ws.len() - 1 {
                let tri = [ws[0], ws[i], ws[i + 1]];
                let kv = o.kverts(&tri, zb);
                let rec = o.ps_consts.as_ref().map(|_| record(o, &kv, &tri, if cw { 1.0 } else { -1.0 }));
                push(d, g, Tri { v: kv }, o, rec.as_deref());
            }
        }
    }
}

fn line(d: &mut Device, o: &Out, a: &V, b: &V) {
    let (a, mut b) = (*a, *b);
    if o.flat {
        (b.d, b.s) = (a.d, a.s);
    }
    let Some((a, b)) = (if o.pre { Some((a, b)) } else { clip_line(a, b, o.planes) }) else { return };
    let mut g = o.group;
    (g.kind, g.size) = (k::KIND_LINES, k::SUB);
    let ends = [o.window(&a), o.window(&b)];
    let kv = o.kverts(&ends, 0.0);
    let rec = o.ps_consts.as_ref().map(|_| record(o, &kv, &ends, 1.0));
    push(d, &g, Tri { v: [kv[0], kv[1], Vert::default()] }, o, rec.as_deref());
}

/// A point: a square `psize` across, as two triangles (point sprites
/// spread the texture over it).
fn point_at(d: &mut Device, o: &Out, w: &W) {
    let h = (w.v.psize.max(1.0) / 2.0) as f64;
    let corner = |dx: f64, dy: f64, s: f32, t: f32| {
        let mut c = *w;
        c.x += dx;
        c.y += dy;
        if o.sprite {
            for tc in c.v.t.iter_mut() {
                *tc = [s, t, 1.0];
            }
            for (k, &(u, _)) in o.vary.iter().enumerate() {
                if u == crate::shader::U_TEXCOORD {
                    c.v.outs[k] = [s, t, 0.0, 1.0];
                }
            }
        }
        c
    };
    let q = [corner(-h, -h, 0.0, 0.0), corner(h, -h, 1.0, 0.0), corner(h, h, 1.0, 1.0), corner(-h, h, 0.0, 1.0)];
    let mut g = o.group;
    g.kind = k::KIND_TRIANGLES;
    let kv = o.kverts(&q, 0.0);
    for (a, b, c) in [(0, 1, 2), (0, 2, 3)] {
        let rec = o.ps_consts.as_ref().map(|_| record(o, &[kv[a], kv[b], kv[c]], &[q[a], q[b], q[c]], 1.0));
        push(d, &g, Tri { v: [kv[a], kv[b], kv[c]] }, o, rec.as_deref());
    }
}

fn point(d: &mut Device, o: &Out, v: &V) {
    if !o.pre && (v.c[3] <= 0.0 || v.c[0].abs() > v.c[3] || v.c[1].abs() > v.c[3] || v.c[2] < 0.0 || v.c[2] > v.c[3]) {
        return;
    }
    point_at(d, o, &o.window(v));
}

// -----| the state, as the kernel's group |-----

fn cmp(f: u32) -> u32 {
    0x200 + f.clamp(1, 8) - 1
}

fn blend_factor(f: u32) -> u32 {
    match f {
        1 => 0,
        2 => 1,
        3 => 0x300,
        4 => 0x301,
        5 | 12 => 0x302,
        6 | 13 => 0x303,
        7 => 0x304,
        8 => 0x305,
        9 => 0x306,
        10 => 0x307,
        11 => 0x308,
        14 => 0x8001,
        15 => 0x8002,
        _ => 1,
    }
}

fn stencil_op(o: u32) -> u32 {
    match o {
        2 => 0,
        3 => 0x1E01,
        4 => 0x1E02,
        5 => 0x1E03,
        6 => 0x150A,
        7 => 0x8507,
        8 => 0x8508,
        _ => 0x1E00,
    }
}

fn filter(f: u32) -> u32 {
    if f >= 2 { k::FILTER_LINEAR } else { k::FILTER_POINT }
}

/// The kernel's group for this draw, and each slot's texture size.
fn group(d: &mut Device) -> (Group, Option<Group>, [(f64, f64); k::SLOTS]) {
    let s = &d.state;
    let r = &s.rs;
    let rt = unsafe { surface(d.rt) };
    let has_ds = !d.ds.is_null();
    let mut flags = k::F_PERSPECTIVE;
    if has_ds && r[rs::ZENABLE] != 0 {
        flags |= k::F_DEPTH_TEST;
        if r[rs::ZWRITEENABLE] != 0 {
            flags |= k::F_DEPTH_WRITE;
        }
    }
    if r[rs::ALPHABLENDENABLE] != 0 {
        flags |= k::F_BLEND;
    }
    if r[rs::ALPHATESTENABLE] != 0 {
        flags |= k::F_ALPHA_TEST;
    }
    if r[rs::FOGENABLE] != 0 {
        flags |= k::F_FOG;
    }
    if has_ds && r[rs::STENCILENABLE] != 0 {
        flags |= k::F_STENCIL;
    }
    if r[rs::SPECULARENABLE] != 0 {
        flags |= k::F_SPECULAR;
    }
    if format::has_alpha(rt.format) {
        flags |= k::F_DST_ALPHA;
    }
    // The viewport, and the scissor rectangle.
    let vp = s.viewport;
    let mut sc = [vp.x as i32, vp.y as i32, (vp.x + vp.w) as i32, (vp.y + vp.h) as i32];
    if r[rs::SCISSORTESTENABLE] != 0 {
        sc = [sc[0].max(s.scissor[0]), sc[1].max(s.scissor[1]), sc[2].min(s.scissor[2]), sc[3].min(s.scissor[3])];
    }
    let table = r[rs::FOGENABLE] != 0 && r[rs::FOGTABLEMODE] != 0;
    let (start, end, density) = (s.rsf(rs::FOGSTART) as f64, s.rsf(rs::FOGEND) as f64, s.rsf(rs::FOGDENSITY) as f64);
    // By eye depth (w) with a perspective projection, else by z.
    let proj = &s.transforms[1];
    let w_fog = proj[11] != 0.0 || proj[15] != 1.0;
    let fog_mode = if !table {
        0
    } else {
        (match r[rs::FOGTABLEMODE] {
            1 => 0x800,
            2 => 0x801,
            _ => 0x2601,
        }) | if w_fog { k::FOG_INVERSE } else { 0 }
    };
    let fog_params = if fog_mode == 0x2601 {
        if end != start { [-1.0 / (end - start), end / (end - start)] } else { [0.0, 1.0] }
    } else {
        [density, 0.0]
    };
    let mut stages = [Stage::default(); k::STAGES];
    for (i, st) in stages.iter_mut().enumerate() {
        let t = &s.tss[i];
        if t[tss::COLOROP] <= 1 {
            break;
        }
        *st = Stage {
            color_op: t[tss::COLOROP],
            color_arg: [t[tss::COLORARG1], t[tss::COLORARG2], t[tss::COLORARG0]],
            alpha_op: t[tss::ALPHAOP],
            alpha_arg: [t[tss::ALPHAARG1], t[tss::ALPHAARG2], t[tss::ALPHAARG0]],
            result: (t[tss::RESULTARG] == 5) as u32,
            constant: t[tss::CONSTANT],
        };
    }
    let ps = crate::shader::of(s.ps).filter(|p| p.pixel);
    let mut samplers = [Sampler::default(); k::SAMPLERS];
    let mut sizes = [(0.0, 0.0); k::SAMPLERS];
    let mut need_flush = false;
    for i in 0..if ps.is_some() { k::SAMPLERS } else { k::SLOTS } {
        if ps.is_none() && stages[i].color_op <= 1 {
            break;
        }
        let tex = s.textures[i];
        if !is_texture(tex) {
            continue;
        }
        let t = unsafe { texture(tex) };
        if t.cube {
            continue;
        }
        // A texture drawn into is read back after the draws waiting; and
        // re-uploading one the waiting draws use would change them too.
        if t.surfaces.iter().any(|&l| unsafe { surface(l) }.drawn) || (t.handle != 0 && t.dirty) {
            need_flush = true;
        }
        let ss = &s.ss[i];
        let levels = if t.usage & crate::res::USAGE_AUTOGENMIPMAP != 0 { 2 } else { t.levels };
        samplers[i] = Sampler {
            texture: 0,
            min: filter(ss[ss::MINFILTER]),
            mag: filter(ss[ss::MAGFILTER]),
            mip: if levels > 1 { ss[ss::MIPFILTER].min(2) } else { 0 },
            address: [ss[ss::ADDRESSU].clamp(1, 5), ss[ss::ADDRESSV].clamp(1, 5)],
            border: ss[ss::BORDERCOLOR],
            lod_bias: (f32::from_bits(ss[ss::MIPMAPLODBIAS]) * 256.0) as i32,
            max_level: ss[ss::MAXMIPLEVEL].max(t.lod),
            gl_rules: 0,
        };
        sizes[i] = (t.w as f64, t.h as f64);
    }
    let g = Group {
        flags,
        kind: k::KIND_TRIANGLES,
        size: k::SUB,
        depth_func: cmp(r[rs::ZFUNC]),
        alpha_func: cmp(r[rs::ALPHAFUNC]),
        alpha_ref: (r[rs::ALPHAREF] & 0xFF) * 2,
        src_blend: blend_factor(r[rs::SRCBLEND]),
        dst_blend: blend_factor(match r[rs::SRCBLEND] {
            12 => 6,
            13 => 5,
            _ => r[rs::DESTBLEND],
        }),
        blend_op: r[rs::BLENDOP],
        blend_color: r[rs::BLENDFACTOR],
        color_mask: r[rs::COLORWRITEENABLE] & 0xF,
        fog_color: r[rs::FOGCOLOR] & 0xFF_FFFF,
        stencil_func: cmp(r[rs::STENCILFUNC]),
        stencil_ref: r[rs::STENCILREF] & 0xFF,
        stencil_mask: r[rs::STENCILMASK] & 0xFF,
        stencil_write: r[rs::STENCILWRITEMASK] & 0xFF,
        stencil_ops: [stencil_op(r[rs::STENCILFAIL]), stencil_op(r[rs::STENCILZFAIL]), stencil_op(r[rs::STENCILPASS])],
        scissor: sc,
        fog_mode,
        fog_params: fog_params.map(|x| (x * FX) as i64),
        stages,
        samplers,
        tfactor: r[rs::TEXTUREFACTOR],
        ..Default::default()
    };
    let ccw = (flags & k::F_STENCIL != 0 && r[rs::TWOSIDEDSTENCILMODE] != 0).then(|| {
        let mut c = g;
        c.stencil_func = cmp(r[rs::CCW_STENCILFUNC]);
        c.stencil_ops = [stencil_op(r[rs::CCW_STENCILFAIL]), stencil_op(r[rs::CCW_STENCILZFAIL]), stencil_op(r[rs::CCW_STENCILPASS])];
        c
    });
    if need_flush {
        d.flush();
    }
    // The textures, uploaded if they changed.
    let mut g = g;
    for i in 0..k::SAMPLERS {
        if sizes[i].0 > 0.0 {
            g.samplers[i].texture = unsafe { texture(d.state.textures[i]) }.kernel();
        }
    }
    // A pixel shader instead of the stages: specular and (ps_3_0) fog are
    // its own business.
    if let Some(p) = crate::shader::of(d.state.ps).filter(|p| p.pixel) {
        g.flags = (g.flags | k::F_SHADER) & !k::F_SPECULAR;
        if p.major >= 3 {
            g.flags &= !k::F_FOG;
        }
        g.ps = p.kernel;
        g.ps_inputs = p.varyings.len() as u32;
        g.stages = Default::default();
        let mut bools = 0u32;
        for (i, &b) in d.state.ps_b.iter().enumerate() {
            if b != 0 {
                bools |= 1 << i;
            }
        }
        for &(n, b) in &p.defb {
            bools = (bools & !(1 << (n & 15))) | ((b as u32) << (n & 15));
        }
        g.ps_bools = bools;
    }
    let ccw = ccw.map(|mut c| {
        c.samplers = g.samplers;
        c
    });
    (g, ccw, core::array::from_fn(|i| sizes[i]))
}

// -----| draw calls |-----

/// How many vertices `count` primitives of type `t` use.
fn vertex_count(t: u32, count: u32) -> u32 {
    match t {
        1 => count,
        2 => count * 2,
        3 => count + 1,
        4 => count * 3,
        5 | 6 => count + 2,
        _ => 0,
    }
}

/// Draw `count` primitives of type `t`, vertex i being `index(i)` of the
/// streams `src`.
fn draw(d: &mut Device, t: u32, count: u32, src: &[Source; 16], index: &dyn Fn(usize) -> Option<usize>) -> i32 {
    if !(1..=6).contains(&t) || d.rt.is_null() {
        return D3DERR_INVALIDCALL;
    }
    let n = vertex_count(t, count) as usize;
    if count == 0 {
        return S_OK;
    }
    let elems = match unsafe { d.state.decl.as_ref() } {
        Some(Com { body: Body::Decl(dc), .. }) => dc.elems.clone(),
        _ => return D3DERR_INVALIDCALL,
    };
    // The target, and which depth buffer goes with it.
    let target = unsafe { surface(d.rt) }.target;
    let depth = if d.ds.is_null() { 0 } else { unsafe { surface(d.ds) }.target };
    if d.batch.target != target || d.batch.depth != depth {
        d.flush();
        (d.batch.target, d.batch.depth) = (target, depth);
    }
    unsafe { surface(d.rt) }.drawn = true;
    let ctx = Ctx::new(d);
    let (group, group_ccw, tex_size) = group(d);
    let s = &d.state;
    let vs = crate::shader::of(s.vs).filter(|v| !v.pixel);
    let pre = vs.is_none() && elems.iter().any(|e| e.usage == U_POSITIONT);
    // Pretransformed vertices fog by z.
    let mut group = group;
    if pre {
        group.fog_mode &= !k::FOG_INVERSE;
    }
    let group_ccw = group_ccw.map(|mut g| {
        g.fog_mode = group.fog_mode;
        g
    });
    let psh = crate::shader::of(s.ps).filter(|p| p.pixel);
    let vary: Vec<(u8, u8)> = psh.map(|p| p.varyings.clone()).unwrap_or_default();
    let ps_consts = psh.map(|p| {
        let mut c: Vec<f32> = s.ps_f.iter().flatten().copied().collect();
        c.resize(k::PS_CONSTS * 4, 0.0);
        for &(n, v) in &p.defs {
            if (n as usize) < k::PS_CONSTS {
                c[n as usize * 4..n as usize * 4 + 4].copy_from_slice(&v);
            }
        }
        let mut ints = s.ps_i;
        for &(n, v) in &p.defi {
            ints[(n as usize) & 15] = v;
        }
        c.extend(ints.iter().flatten().map(|&i| f32::from_bits(i as u32)));
        c
    });
    let clamp_colors = psh.is_none_or(|p| p.major < 3);
    let o = Out {
        group,
        group_ccw,
        pre,
        viewport: s.viewport,
        tex_size,
        cull: s.rs[rs::CULLMODE],
        fill: s.rs[rs::FILLMODE],
        flat: s.rs[rs::SHADEMODE] == 1,
        bias: (s.rsf(rs::DEPTHBIAS) as f64, s.rsf(rs::SLOPESCALEDEPTHBIAS) as f64),
        planes: if pre { 0 } else { s.rs[rs::CLIPPLANEENABLE] & 63 },
        sprite: s.rs[rs::POINTSPRITEENABLE] != 0,
        ps_consts,
        vary,
        fresh: core::cell::Cell::new(true),
    };
    let consts = crate::shader::VsConsts { f: &s.vs_f, i: &s.vs_i, b: &s.vs_b };
    // Each vertex processed once.
    let mut cache: Vec<(usize, V)> = Vec::new();
    let mut verts: Vec<V> = Vec::with_capacity(n);
    for i in 0..n {
        let Some(vi) = index(i) else { return D3DERR_INVALIDCALL };
        let v = match cache.binary_search_by_key(&vi, |e| e.0) {
            Ok(j) => cache[j].1,
            Err(j) => {
                let v = match vs {
                    Some(sh) => vs_vertex(sh, &ctx, &elems, src, vi, &consts, &ctx.clip_planes, &o.vary, clamp_colors),
                    None => {
                        let mut v = ctx.process(&fetch_vertex(&elems, src, vi));
                        ff_outs(&mut v, &o.vary);
                        v
                    }
                };
                if cache.len() < 1 << 16 {
                    cache.insert(j, (vi, v));
                }
                v
            }
        };
        verts.push(v);
    }
    match t {
        1 => {
            for v in &verts {
                point(d, &o, v);
            }
        }
        2 | 3 => {
            let step = if t == 2 { 2 } else { 1 };
            let mut i = 0;
            while i + 1 < n {
                line(d, &o, &verts[i], &verts[i + 1]);
                i += step;
            }
        }
        4 => {
            for p in verts.chunks_exact(3) {
                triangle(d, &o, &p[0], &p[1], &p[2], &p[0]);
            }
        }
        5 => {
            for i in 0..count as usize {
                let (a, b, c) = (&verts[i], &verts[i + 1], &verts[i + 2]);
                if i % 2 == 0 {
                    triangle(d, &o, a, b, c, a);
                } else {
                    triangle(d, &o, b, a, c, a);
                }
            }
        }
        _ => {
            for i in 0..count as usize {
                triangle(d, &o, &verts[0], &verts[i + 1], &verts[i + 2], &verts[i + 1]);
            }
        }
    }
    S_OK
}

fn dev<'a>(this: *mut Com) -> &'a mut Device {
    unsafe { device(this) }
}

/// The bound streams.
fn sources(d: &Device) -> [Source; 16] {
    core::array::from_fn(|i| {
        let s = &d.state.streams[i];
        match unsafe { s.buffer.as_ref() } {
            Some(Com { body: Body::Buffer(b), .. }) if (s.offset as usize) <= b.data.len() => Source {
                ptr: unsafe { b.data.as_ptr().add(s.offset as usize) },
                len: b.data.len() - s.offset as usize,
                stride: s.stride as usize,
            },
            _ => Source { ptr: core::ptr::null(), len: 0, stride: 0 },
        }
    })
}

pub unsafe extern "system" fn Dev_DrawPrimitive(this: *mut Com, t: u32, start: u32, count: u32) -> i32 {
    let d = dev(this);
    let src = sources(d);
    draw(d, t, count, &src, &|i| Some(start as usize + i))
}

/// The indices of the bound index buffer, from `start`.
fn indices(d: &Device) -> Option<(*const u8, usize, bool)> {
    match unsafe { d.state.indices.as_ref() } {
        Some(Com { body: Body::Buffer(b), .. }) => Some((b.data.as_ptr(), b.data.len(), b.format == format::INDEX32)),
        _ => None,
    }
}

fn read_index(p: *const u8, len: usize, wide: bool, i: usize) -> Option<usize> {
    let size = if wide { 4 } else { 2 };
    if (i + 1) * size > len {
        return None;
    }
    Some(unsafe {
        if wide { core::ptr::read_unaligned(p.add(i * 4) as *const u32) as usize } else { core::ptr::read_unaligned(p.add(i * 2) as *const u16) as usize }
    })
}

pub unsafe extern "system" fn Dev_DrawIndexedPrimitive(this: *mut Com, t: u32, base: i32, _min: u32, _nv: u32, start: u32, count: u32) -> i32 {
    let d = dev(this);
    let Some((p, len, wide)) = indices(d) else { return D3DERR_INVALIDCALL };
    let src = sources(d);
    draw(d, t, count, &src, &|i| read_index(p, len, wide, start as usize + i).map(|x| (x as i64 + base as i64).max(0) as usize))
}

/// The *UP draws leave stream 0 and the indices unset, as Direct3D does.
fn after_up(d: &mut Device, indexed: bool) {
    unsafe { set_ref(&mut d.state.streams[0].buffer, core::ptr::null_mut()) };
    (d.state.streams[0].offset, d.state.streams[0].stride) = (0, 0);
    if indexed {
        unsafe { set_ref(&mut d.state.indices, core::ptr::null_mut()) };
    }
}

pub unsafe extern "system" fn Dev_DrawPrimitiveUP(this: *mut Com, t: u32, count: u32, data: *const u8, stride: u32) -> i32 {
    let d = dev(this);
    if data.is_null() {
        return D3DERR_INVALIDCALL;
    }
    let mut src = sources(d);
    src[0] = Source { ptr: data, len: vertex_count(t, count) as usize * stride as usize, stride: stride as usize };
    let r = draw(d, t, count, &src, &|i| Some(i));
    after_up(d, false);
    r
}

pub unsafe extern "system" fn Dev_DrawIndexedPrimitiveUP(this: *mut Com, t: u32, _min: u32, nv: u32, count: u32, idx: *const u8, ifmt: u32, data: *const u8, stride: u32) -> i32 {
    let d = dev(this);
    if data.is_null() || idx.is_null() {
        return D3DERR_INVALIDCALL;
    }
    let wide = ifmt == format::INDEX32;
    let n = vertex_count(t, count) as usize;
    let mut src = sources(d);
    // The vertices the indices reach.
    let top = (0..n).filter_map(|i| read_index(idx, usize::MAX / 8, wide, i)).max().unwrap_or(0).max(nv as usize);
    src[0] = Source { ptr: data, len: (top + 1) * stride as usize, stride: stride as usize };
    let r = draw(d, t, count, &src, &|i| read_index(idx, usize::MAX / 8, wide, i));
    after_up(d, true);
    r
}

// -----| Clear and Present |-----

pub unsafe extern "system" fn Dev_Clear(this: *mut Com, count: u32, rects: *const [i32; 4], flags: u32, color: u32, z: f32, stencil: u32) -> i32 {
    let d = dev(this);
    if d.rt.is_null() || (flags & 6 != 0 && d.ds.is_null()) {
        return D3DERR_INVALIDCALL;
    }
    d.flush();
    let s = &d.state;
    let vp = s.viewport;
    let mut area = [vp.x as i32, vp.y as i32, (vp.x + vp.w) as i32, (vp.y + vp.h) as i32];
    if s.rs[rs::SCISSORTESTENABLE] != 0 {
        area = [area[0].max(s.scissor[0]), area[1].max(s.scissor[1]), area[2].min(s.scissor[2]), area[3].min(s.scissor[3])];
    }
    let list: Vec<[i32; 4]> = if count == 0 || rects.is_null() {
        vec![area]
    } else {
        (0..count as usize)
            .map(|i| {
                let r = unsafe { core::ptr::read_unaligned(rects.add(i)) };
                [r[0].max(area[0]), r[1].max(area[1]), r[2].min(area[2]), r[3].min(area[3])]
            })
            .collect()
    };
    let rt = unsafe { surface(d.rt) };
    rt.drawn = true;
    let target = rt.target;
    let depth = if d.ds.is_null() { 0 } else { unsafe { surface(d.ds) }.target };
    let mut mask = 0;
    if flags & 1 != 0 {
        mask |= 0x4000;
    }
    if flags & 2 != 0 {
        mask |= 0x100;
    }
    if flags & 4 != 0 {
        mask |= 0x400;
    }
    let color = if format::has_alpha(rt.format) { color } else { color & 0xFF_FFFF };
    for r in list {
        let args = ClearArgs {
            mask,
            color,
            depth: (z.clamp(0.0, 1.0) as f64 * DEPTH_MAX + 0.5) as u32,
            color_mask: 0xF,
            scissor: r,
            stencil: stencil & 0xFF,
            stencil_write: 0xFF,
        };
        gfx3d::gpu3d(1, target, &args as *const _ as u64, depth);
    }
    S_OK
}

pub unsafe extern "system" fn Dev_Present(this: *mut Com, src: *const [i32; 4], dst: *const [i32; 4], hwnd: usize, _dirty: usize) -> i32 {
    let d = dev(this);
    d.flush();
    let bb = unsafe { surface(d.backbuffer) };
    let px = bb.read_argb();
    let win = if hwnd != 0 {
        hwnd
    } else if d.params.hwnd != 0 {
        d.params.hwnd
    } else {
        d.focus
    };
    let sr = if src.is_null() { [0, 0, bb.w as i32, bb.h as i32] } else { unsafe { *src } };
    let dr = if dst.is_null() {
        let mut r = [0i32; 4];
        unsafe { GetClientRect(win, &mut r) };
        r
    } else {
        unsafe { *dst }
    };
    let mut bmi = [0u8; 40];
    bmi[0] = 40;
    bmi[4..8].copy_from_slice(&(bb.w as i32).to_le_bytes());
    bmi[8..12].copy_from_slice(&(-(bb.h as i32)).to_le_bytes());
    bmi[12] = 1;
    bmi[14] = 32;
    unsafe {
        let hdc = GetDC(win);
        StretchDIBits(hdc, dr[0], dr[1], dr[2] - dr[0], dr[3] - dr[1], sr[0], sr[1], sr[2] - sr[0], sr[3] - sr[1], px.as_ptr() as *const u8, bmi.as_ptr(), 0, 0x00CC_0020);
        ReleaseDC(win, hdc);
    }
    S_OK
}

/// A shaded primitive's record for the kernel: [x0, y0, face, Q planes],
/// then each input's planes (a, b, c over four components), each value v
/// as v q, so the kernel's (v q) / q is perspective-correct. For a line,
/// the gradient runs along it.
fn record(o: &Out, kv: &[Vert], ws: &[W], face: f32) -> Vec<f32> {
    let n = o.vary.len();
    let mut r = Vec::with_capacity(k::PS_HEADER + 12 * n);
    let pos = |i: usize| (kv[i].x as f64 / SUB, kv[i].y as f64 / SUB);
    let q = |i: usize| kv[i].q as f64 / FX;
    let (x0, y0) = pos(0);
    // The plane through (0, 0, f0), (x1, y1, f1), (x2, y2, f2), relative to
    // vertex 0.
    let solve: alloc::boxed::Box<dyn Fn(&[f64]) -> [f64; 3]> = if kv.len() >= 3 {
        let ((ax, ay), (bx, by)) = (pos(1), pos(2));
        let (x1, y1, x2, y2) = (ax - x0, ay - y0, bx - x0, by - y0);
        let det = x1 * y2 - x2 * y1;
        alloc::boxed::Box::new(move |f: &[f64]| {
            if det == 0.0 {
                return [f[0], 0.0, 0.0];
            }
            let (d1, d2) = (f[1] - f[0], f[2] - f[0]);
            [f[0], (d1 * y2 - d2 * y1) / det, (d2 * x1 - d1 * x2) / det]
        })
    } else {
        let (ax, ay) = pos(1);
        let (dx, dy) = (ax - x0, ay - y0);
        let l2 = dx * dx + dy * dy;
        alloc::boxed::Box::new(move |f: &[f64]| {
            let d = f[1] - f[0];
            if l2 == 0.0 { [f[0], 0.0, 0.0] } else { [f[0], d * dx / l2, d * dy / l2] }
        })
    };
    let qs: Vec<f64> = (0..kv.len()).map(q).collect();
    let qp = solve(&qs);
    r.extend_from_slice(&[x0 as f32, y0 as f32, face, qp[0] as f32, qp[1] as f32, qp[2] as f32]);
    for i in 0..n {
        let planes: [[f64; 3]; 4] = core::array::from_fn(|c| {
            let f: Vec<f64> = (0..kv.len()).map(|v| ws[v].v.outs[i][c] as f64 * qs[v]).collect();
            solve(&f)
        });
        for part in 0..3 {
            for c in 0..4 {
                r.push(planes[c][part] as f32);
            }
        }
    }
    r
}
