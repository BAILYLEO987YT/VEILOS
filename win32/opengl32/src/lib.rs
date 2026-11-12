//! VEILOS `opengl32.dll`: OpenGL 1.1 fixed function (WIN32_DESIGN.md G5).
//!
//! The vertex stage runs here, in user mode with real floating point:
//! matrices, primitive assembly, clipping, culling and the viewport. What
//! comes out is triangles in window space, in fixed point, which go to the
//! kernel's rasterizer (src/raster3d.rs) in batches through one syscall,
//! VEIL_GPU3D; the kernel fills them on every CPU into a colour and depth
//! buffer it keeps. Frames come back with a read and are drawn on the DC with
//! SetDIBitsToDevice: at SwapBuffers for a double-buffered window, at
//! glFlush/glFinish for a single-buffered one (a DIB section included).
//!
//!   lib.rs      contexts, wgl*, state, the vertex stage and the gl* exports
//!   light.rs    lighting and fog
//!   lists.rs    display lists
//!   pixels.rs   raster position, images, bitmaps, pixel transfer, buffers,
//!               accumulation, font bitmaps
//!   more.rs     texgen, clip planes, polygon offset, stipples, logic ops,
//!               1D and copied textures, getters, every call's variants
//!   attrib.rs   the attribute stacks
//!   select.rs   selection and feedback
//!   eval.rs     evaluators
//!   ext.rs      extensions and wglGetProcAddress
//!
//! Not here: colour index mode (no pixel format has it), wglUseFontOutlines,
//! and layer planes.

#![no_std]
#![allow(non_snake_case, clippy::missing_safety_doc)]

extern crate alloc;

#[path = "../../dllrt.rs"]
mod dllrt;
mod attrib;
mod eval;
mod ext;
mod light;
mod lists;
mod more;
mod pixels;
mod select;

use pixels::unpack;

use lists::rec;

use alloc::boxed::Box;
use alloc::vec;
use alloc::vec::Vec;
use dllrt::Lock;

type Bool = i32;

#[cfg(feature = "host")]
mod host;
#[cfg(feature = "host")]
use host::{ChoosePixelFormat, DescribePixelFormat, GetPixelFormat, SetPixelFormat, VeilGdiDcSize, VeilGdiSetSwapHook};

#[allow(clippy::missing_safety_doc)]
unsafe fn VeilGpu3d(op: u64, a: u64, b: u64, c: u64) -> i32 {
    gfx3d::gpu3d(op, a, b, c)
}

unsafe fn NtClose(h: u64) -> i32 {
    gfx3d::close(h);
    0
}

#[link(name = "gdi32", kind = "raw-dylib")]
unsafe extern "system" {
    fn SetDIBitsToDevice(hdc: u64, dx: i32, dy: i32, w: u32, h: u32, sx: i32, sy: i32, start: u32, lines: u32, bits: *const u8, bmi: *const u8, usage: u32) -> i32;
}

#[cfg(not(feature = "host"))]
#[link(name = "gdi32", kind = "raw-dylib")]
unsafe extern "system" {
    fn VeilGdiDcSize(hdc: u64, out: *mut [i32; 2]) -> i32;
    fn VeilGdiSetSwapHook(f: usize);
    fn ChoosePixelFormat(hdc: u64, pfd: *const u8) -> i32;
    fn DescribePixelFormat(hdc: u64, n: i32, size: u32, out: *mut u8) -> i32;
    fn SetPixelFormat(hdc: u64, n: i32, pfd: *const u8) -> i32;
    fn GetPixelFormat(hdc: u64) -> i32;
}

#[link(name = "kernel32", kind = "raw-dylib")]
unsafe extern "system" {
    fn TlsAlloc() -> u32;
    fn TlsGetValue(index: u32) -> usize;
    fn TlsSetValue(index: u32, value: usize) -> Bool;
    fn SetLastError(e: u32);
}

// -----| the kernel's ABI (src/raster3d.rs) |-----

use gfx3d::raster3d::{
    self as k, ClearArgs, DrawArgs, Group as KGroup, PixelArgs, ReadArgs, Sampler, Stage, TexArgs, Tri as KTri, Vert as KVert, F_ALPHA_TEST, F_BLEND,
    F_DEPTH_TEST, F_DEPTH_WRITE, F_FOG, F_LINE_STIPPLE, F_LOGIC_OP, F_POLY_STIPPLE, F_STENCIL, F_TEX_MASK, KIND_LINES, KIND_POINTS,
    KIND_TRIANGLES,
};
const DEPTH_MAX: f64 = 16777215.0;
/// Subpixel steps per pixel (raster3d::SUB).
const SUB: f64 = 256.0;
const FX: f64 = 4294967296.0;

/// How the kernel samples a GL texture object.
fn sampler(t: &Tex) -> Sampler {
    let point = |f: u32| matches!(f, GL_NEAREST | GL_NEAREST_MIPMAP_NEAREST | GL_NEAREST_MIPMAP_LINEAR);
    let address = |w: u32| if w == GL_REPEAT { k::WRAP } else { k::CLAMP };
    Sampler {
        texture: t.handle,
        min: if point(t.min) { k::FILTER_POINT } else { k::FILTER_LINEAR },
        mag: if t.mag == GL_NEAREST { k::FILTER_POINT } else { k::FILTER_LINEAR },
        mip: match t.min {
            GL_NEAREST_MIPMAP_NEAREST | GL_LINEAR_MIPMAP_NEAREST => k::FILTER_POINT,
            GL_NEAREST_MIPMAP_LINEAR | GL_LINEAR_MIPMAP_LINEAR => k::FILTER_LINEAR,
            _ => 0,
        },
        address: [address(t.wrap_s), address(t.wrap_t)],
        gl_rules: 1,
        ..Default::default()
    }
}

/// A GL texture environment (0: pass the colour on) as a texture stage,
/// with GL_TEXTURE_ENV_COLOR `color` (0xAARRGGBB).
pub(crate) fn gl_stage(env: u32, color: u32) -> Stage {
    use k::{ta, top};
    let (cur, tex) = (ta::CURRENT, ta::TEXTURE);
    let (color_op, color_arg, alpha_op) = match env {
        0 => (top::SELECTARG1, [cur, 0, 0], top::SELECTARG1),
        GL_REPLACE => (top::SELECTARG1, [tex, 0, 0], top::SELECTARG1),
        GL_DECAL => (top::BLENDTEXTUREALPHA, [tex, cur, 0], top::SELECTARG2),
        // Towards the environment colour by the texel.
        GL_BLEND => (top::LERP, [ta::CONSTANT, cur, tex], top::MODULATE),
        GL_ADD => (top::ADD, [tex, cur, 0], top::MODULATE),
        _ => (top::MODULATE, [tex, cur, 0], top::MODULATE),
    };
    let alpha_arg = if env == 0 { [cur, 0, 0] } else if env == GL_REPLACE { [tex, 0, 0] } else { [tex, cur, 0] };
    Stage { color_op, color_arg, alpha_op, alpha_arg, result: 0, constant: color }
}

// -----| GL constants |-----

const GL_NO_ERROR: u32 = 0;
pub(crate) const GL_INVALID_ENUM: u32 = 0x500;
pub(crate) const GL_INVALID_VALUE: u32 = 0x501;
pub(crate) const GL_INVALID_OPERATION: u32 = 0x502;
const GL_STACK_OVERFLOW: u32 = 0x503;
const GL_STACK_UNDERFLOW: u32 = 0x504;


const GL_POINTS: u32 = 0;
const GL_LINES: u32 = 1;
const GL_LINE_LOOP: u32 = 2;
const GL_LINE_STRIP: u32 = 3;
const GL_TRIANGLES: u32 = 4;
const GL_TRIANGLE_STRIP: u32 = 5;
const GL_TRIANGLE_FAN: u32 = 6;
const GL_QUADS: u32 = 7;
const GL_QUAD_STRIP: u32 = 8;
const GL_POLYGON: u32 = 9;

const GL_MODELVIEW: u32 = 0x1700;
const GL_PROJECTION: u32 = 0x1701;
const GL_TEXTURE: u32 = 0x1702;

const GL_CULL_FACE: u32 = 0x0B44;
const GL_DEPTH_TEST: u32 = 0x0B71;
const GL_ALPHA_TEST: u32 = 0x0BC0;
const GL_BLEND: u32 = 0x0BE2;
const GL_DITHER: u32 = 0x0BD0;
const GL_SCISSOR_TEST: u32 = 0x0C11;
const GL_TEXTURE_2D: u32 = 0x0DE1;
const GL_TEXTURE_1D: u32 = 0x0DE0;
const GL_TEXTURE_GEN_S: u32 = 0x0C60;
const GL_TEXTURE_GEN_MODE: u32 = 0x2500;
const GL_OBJECT_PLANE: u32 = 0x2501;
const GL_EYE_PLANE: u32 = 0x2502;
const GL_EYE_LINEAR: u32 = 0x2400;
const GL_OBJECT_LINEAR: u32 = 0x2401;
const GL_SPHERE_MAP: u32 = 0x2402;
const GL_CLIP_PLANE0: u32 = 0x3000;
const GL_POLYGON_OFFSET_FILL: u32 = 0x8037;
const GL_POLYGON_OFFSET_LINE: u32 = 0x2A02;
const GL_POLYGON_OFFSET_POINT: u32 = 0x2A01;
const GL_POLYGON_STIPPLE: u32 = 0x0B42;
const GL_LINE_STIPPLE: u32 = 0x0B24;
const GL_COLOR_LOGIC_OP: u32 = 0x0BF2;
const GL_ADD: u32 = 0x0104;
const GL_LIGHTING: u32 = 0x0B50;
const GL_LIGHT0: u32 = 0x4000;
const GL_COLOR_MATERIAL: u32 = 0x0B57;
const GL_NORMALIZE: u32 = 0x0BA1;
const GL_FOG: u32 = 0x0B60;
const GL_STENCIL_TEST: u32 = 0x0B90;

pub(crate) const GL_AMBIENT: u32 = 0x1200;
pub(crate) const GL_DIFFUSE: u32 = 0x1201;
pub(crate) const GL_SPECULAR: u32 = 0x1202;
const GL_POSITION: u32 = 0x1203;
const GL_SPOT_DIRECTION: u32 = 0x1204;
const GL_SPOT_EXPONENT: u32 = 0x1205;
const GL_SPOT_CUTOFF: u32 = 0x1206;
const GL_CONSTANT_ATTENUATION: u32 = 0x1207;
const GL_LINEAR_ATTENUATION: u32 = 0x1208;
const GL_QUADRATIC_ATTENUATION: u32 = 0x1209;
pub(crate) const GL_EMISSION: u32 = 0x1600;
const GL_SHININESS: u32 = 0x1601;
pub(crate) const GL_AMBIENT_AND_DIFFUSE: u32 = 0x1602;
const GL_LIGHT_MODEL_LOCAL_VIEWER: u32 = 0x0B51;
const GL_LIGHT_MODEL_TWO_SIDE: u32 = 0x0B52;
const GL_LIGHT_MODEL_AMBIENT: u32 = 0x0B53;
const GL_FOG_DENSITY: u32 = 0x0B62;
const GL_FOG_START: u32 = 0x0B63;
const GL_FOG_END: u32 = 0x0B64;
const GL_FOG_MODE: u32 = 0x0B65;
const GL_FOG_COLOR: u32 = 0x0B66;
pub(crate) const GL_EXP: u32 = 0x0800;
pub(crate) const GL_EXP2: u32 = 0x0801;
/// GL_LINEAR as a fog mode.
pub(crate) const GL_LINEAR_FOG: u32 = 0x2601;
const GL_KEEP: u32 = 0x1E00;
const GL_INCR: u32 = 0x1E02;
const GL_DECR: u32 = 0x1E03;
const GL_INVERT: u32 = 0x150A;
const GL_STENCIL_BUFFER_BIT: u32 = 0x400;
const GL_POINT: u32 = 0x1B00;
const GL_LINE: u32 = 0x1B01;
const GL_FILL: u32 = 0x1B02;

pub(crate) const GL_FRONT: u32 = 0x404;
const GL_BACK: u32 = 0x405;
pub(crate) const GL_FRONT_AND_BACK: u32 = 0x408;
const GL_CW: u32 = 0x900;
const GL_CCW: u32 = 0x901;
const GL_FLAT: u32 = 0x1D00;
const GL_SMOOTH: u32 = 0x1D01;
const GL_LESS: u32 = 0x201;
const GL_ALWAYS: u32 = 0x207;
const GL_ONE: u32 = 1;
const GL_ZERO: u32 = 0;

const GL_COLOR_BUFFER_BIT: u32 = 0x4000;
const GL_DEPTH_BUFFER_BIT: u32 = 0x100;

const GL_BYTE: u32 = 0x1400;
const GL_UNSIGNED_BYTE: u32 = 0x1401;
const GL_SHORT: u32 = 0x1402;
const GL_UNSIGNED_SHORT: u32 = 0x1403;
const GL_INT: u32 = 0x1404;
const GL_UNSIGNED_INT: u32 = 0x1405;

const GL_DOUBLE: u32 = 0x140A;

const GL_ALPHA: u32 = 0x1906;
const GL_RGB: u32 = 0x1907;
const GL_RGBA: u32 = 0x1908;
const GL_LUMINANCE: u32 = 0x1909;
const GL_LUMINANCE_ALPHA: u32 = 0x190A;
const GL_BGR_EXT: u32 = 0x80E0;
const GL_BGRA_EXT: u32 = 0x80E1;

const GL_TEXTURE_MAG_FILTER: u32 = 0x2800;
const GL_TEXTURE_MIN_FILTER: u32 = 0x2801;
const GL_TEXTURE_WRAP_S: u32 = 0x2802;
const GL_TEXTURE_WRAP_T: u32 = 0x2803;
const GL_NEAREST: u32 = 0x2600;
const GL_LINEAR: u32 = 0x2601;
const GL_NEAREST_MIPMAP_NEAREST: u32 = 0x2700;
const GL_LINEAR_MIPMAP_NEAREST: u32 = 0x2701;
const GL_NEAREST_MIPMAP_LINEAR: u32 = 0x2702;
const GL_LINEAR_MIPMAP_LINEAR: u32 = 0x2703;
const GL_CLAMP: u32 = 0x2900;
const GL_REPEAT: u32 = 0x2901;
const GL_TEXTURE_ENV: u32 = 0x2300;
const GL_TEXTURE_ENV_MODE: u32 = 0x2200;
const GL_TEXTURE_ENV_COLOR: u32 = 0x2201;
const GL_MODULATE: u32 = 0x2100;
const GL_DECAL: u32 = 0x2101;
const GL_REPLACE: u32 = 0x1E01;

const GL_VERTEX_ARRAY: u32 = 0x8074;
const GL_NORMAL_ARRAY: u32 = 0x8075;
const GL_COLOR_ARRAY: u32 = 0x8076;
const GL_TEXTURE_COORD_ARRAY: u32 = 0x8078;

const GL_UNPACK_ALIGNMENT: u32 = 0x0CF5;
const GL_PACK_ALIGNMENT: u32 = 0x0D05;

const MAX_TEXTURE_SIZE: i32 = 2048;
const STACK_DEPTH: usize = 32;

// -----| maths |-----

type Mat = [f32; 16];

const IDENTITY: Mat = [1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0];

/// a × b, column-major as GL keeps them.
fn mul(a: &Mat, b: &Mat) -> Mat {
    let mut r = [0.0f32; 16];
    for c in 0..4 {
        for row in 0..4 {
            let mut s = 0.0f32;
            for k in 0..4 {
                s += a[k * 4 + row] * b[c * 4 + k];
            }
            r[c * 4 + row] = s;
        }
    }
    r
}

fn xform(m: &Mat, v: [f32; 4]) -> [f32; 4] {
    let mut r = [0.0f32; 4];
    for (row, out) in r.iter_mut().enumerate() {
        *out = m[row] * v[0] + m[4 + row] * v[1] + m[8 + row] * v[2] + m[12 + row] * v[3];
    }
    r
}

pub(crate) fn sqrt(x: f64) -> f64 {
    use core::arch::x86_64::*;
    unsafe { _mm_cvtsd_f64(_mm_sqrt_sd(_mm_set_sd(x), _mm_set_sd(x))) }
}

/// sin and cos of `x` radians.
pub(crate) fn sin_cos(x: f64) -> (f64, f64) {
    const PI: f64 = core::f64::consts::PI;
    // Down to [-pi/4, pi/4] and a quadrant.
    let k = round(x / (PI / 2.0));
    let r = x - k * (PI / 2.0);
    let r2 = r * r;
    let s = r * (1.0 + r2 * (-1.0 / 6.0 + r2 * (1.0 / 120.0 + r2 * (-1.0 / 5040.0 + r2 * (1.0 / 362880.0 + r2 * (-1.0 / 39916800.0 + r2 / 6227020800.0))))));
    let c = 1.0 + r2 * (-0.5 + r2 * (1.0 / 24.0 + r2 * (-1.0 / 720.0 + r2 * (1.0 / 40320.0 + r2 * (-1.0 / 3628800.0 + r2 / 479001600.0)))));
    match (k as i64).rem_euclid(4) {
        0 => (s, c),
        1 => (c, -s),
        2 => (-s, -c),
        _ => (-c, s),
    }
}

pub(crate) fn floor(x: f64) -> f64 {
    let i = x as i64 as f64;
    if i > x { i - 1.0 } else { i }
}

fn round(x: f64) -> f64 {
    if x >= 0.0 { (x + 0.5) as i64 as f64 } else { -((-x + 0.5) as i64 as f64) }
}

fn clamp01(x: f32) -> f32 {
    if x.is_nan() { 0.0 } else { x.clamp(0.0, 1.0) }
}

/// A vertex's distance from clip plane `plane`: 0-5 the view volume's
/// sides (in clip space), 6-11 the user's planes. Inside is >= 0.
fn plane_distance(v: &Vtx, plane: usize) -> f32 {
    if plane >= 6 {
        return v.clipd[plane - 6];
    }
    let (axis, sign) = (plane / 2, if plane % 2 == 0 { 1.0 } else { -1.0 });
    v.clip[3] + sign * v.clip[axis]
}

/// What normals are multiplied by: the inverse transpose of the upper 3x3
/// of `m` (column-major), as rows.
fn normal_matrix(m: &Mat) -> [[f32; 3]; 3] {
    let a = |r: usize, c: usize| m[c * 4 + r] as f64;
    let det = a(0, 0) * (a(1, 1) * a(2, 2) - a(1, 2) * a(2, 1)) - a(0, 1) * (a(1, 0) * a(2, 2) - a(1, 2) * a(2, 0))
        + a(0, 2) * (a(1, 0) * a(2, 1) - a(1, 1) * a(2, 0));
    if det == 0.0 {
        return [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    }
    // (M^-1)^T = cofactor matrix / det.
    let cof = |r: usize, c: usize| {
        let (r0, r1) = ([1, 0, 0][r], [2, 2, 1][r]);
        let (c0, c1) = ([1, 0, 0][c], [2, 2, 1][c]);
        let v = a(r0, c0) * a(r1, c1) - a(r0, c1) * a(r1, c0);
        if (r + c) % 2 == 0 { v } else { -v }
    };
    core::array::from_fn(|r| core::array::from_fn(|c| (cof(r, c) / det) as f32))
}

// -----| contexts |-----

#[derive(Clone, Copy)]
struct Vtx {
    clip: [f32; 4],
    /// The colour, lit for the front face if lighting is on.
    col: [f32; 4],
    /// Lit for the back face (two-sided lighting), else `col`.
    back: [f32; 4],
    /// Each texture unit's coordinates.
    tc: [[f32; 4]; 2],
    /// Signed distances from the user clip planes (inside: >= 0).
    clipd: [f32; 6],
    /// Eye distance, for fog.
    dist: f32,
}

impl Vtx {
    fn lerp(&self, b: &Vtx, t: f32) -> Vtx {
        let l = |x: [f32; 4], y: [f32; 4]| core::array::from_fn(|k| x[k] + (y[k] - x[k]) * t);
        Vtx { clip: l(self.clip, b.clip), col: l(self.col, b.col), back: l(self.back, b.back), tc: [l(self.tc[0], b.tc[0]), l(self.tc[1], b.tc[1])], clipd: core::array::from_fn(|i| self.clipd[i] + (b.clipd[i] - self.clipd[i]) * t), dist: self.dist + (b.dist - self.dist) * t }
    }
}

struct Tex {
    /// The kernel's texture, 0 until first used.
    handle: u64,
    w: u32,
    h: u32,
    /// Each level's texels, 0xAARRGGBB, top row (t = 0) first; empty for a
    /// level not given (or given at the wrong size).
    mips: Vec<Vec<u32>>,
    min: u32,
    mag: u32,
    wrap_s: u32,
    wrap_t: u32,
    dirty: bool,
}

impl Tex {
    fn new() -> Tex {
        Tex { handle: 0, w: 0, h: 0, mips: Vec::new(), min: GL_NEAREST_MIPMAP_LINEAR, mag: GL_LINEAR, wrap_s: GL_REPEAT, wrap_t: GL_REPEAT, dirty: false }
    }

    /// Level `l`'s size.
    fn level_size(&self, l: usize) -> (u32, u32) {
        ((self.w >> l).max(1), (self.h >> l).max(1))
    }

    /// How many levels a full chain has (down to 1x1).
    fn chain(&self) -> usize {
        (32 - self.w.max(self.h).leading_zeros()) as usize
    }

    /// Levels given, in a row from 0, at the right sizes.
    fn given(&self) -> usize {
        (0..self.chain()).take_while(|&l| {
            let (w, h) = self.level_size(l);
            self.mips.get(l).is_some_and(|m| m.len() == (w * h) as usize)
        }).count()
    }

    /// Usable for texturing: level 0 given, and every level if the
    /// minification filter uses mipmaps.
    fn complete(&self) -> bool {
        if self.w == 0 || self.h == 0 || self.given() == 0 {
            return false;
        }
        self.min == GL_NEAREST || self.min == GL_LINEAR || self.given() == self.chain()
    }
}

#[derive(Clone, Copy, Default)]
struct Array {
    on: bool,
    size: i32,
    kind: u32,
    stride: i32,
    ptr: usize,
}

impl Array {
    /// Element `i` as up to four floats (colours from integers normalised).
    unsafe fn get(&self, i: usize, normalize: bool) -> [f32; 4] {
        let elem = match self.kind {
            GL_BYTE | GL_UNSIGNED_BYTE => 1,
            GL_SHORT | GL_UNSIGNED_SHORT => 2,
            GL_DOUBLE => 8,
            _ => 4,
        };
        let stride = if self.stride > 0 { self.stride as usize } else { elem * self.size as usize };
        let p = self.ptr + i * stride;
        let mut out = [0.0, 0.0, 0.0, 1.0];
        for (k, o) in out.iter_mut().enumerate().take(self.size as usize) {
            let a = p + k * elem;
            *o = unsafe {
                match self.kind {
                    GL_BYTE => { let v = *(a as *const i8) as f32; if normalize { (2.0 * v + 1.0) / 255.0 } else { v } }
                    GL_UNSIGNED_BYTE => { let v = *(a as *const u8) as f32; if normalize { v / 255.0 } else { v } }
                    GL_SHORT => { let v = core::ptr::read_unaligned(a as *const i16) as f32; if normalize { (2.0 * v + 1.0) / 65535.0 } else { v } }
                    GL_UNSIGNED_SHORT => { let v = core::ptr::read_unaligned(a as *const u16) as f32; if normalize { v / 65535.0 } else { v } }
                    GL_INT => { let v = core::ptr::read_unaligned(a as *const i32) as f32; if normalize { (2.0 * v + 1.0) / 4294967295.0 } else { v } }
                    GL_UNSIGNED_INT => { let v = core::ptr::read_unaligned(a as *const u32) as f32; if normalize { v / 4294967295.0 } else { v } }
                    GL_DOUBLE => core::ptr::read_unaligned(a as *const f64) as f32,
                    _ => core::ptr::read_unaligned(a as *const f32),
                }
            };
        }
        out
    }
}

struct Ctx {
    hdc: u64,
    target: u64,
    w: i32,
    h: i32,
    double: bool,
    made_current: bool,

    matrix_mode: u32,
    /// Modelview, projection, and each texture unit's texture matrix.
    stacks: [Vec<Mat>; 4],
    viewport: [i32; 4],
    depth_range: [f32; 2],

    clear_color: [f32; 4],
    clear_depth: f32,
    color: [f32; 4],
    /// Each texture unit's current coordinates.
    tc: [[f32; 4]; 2],
    normal: [f32; 3],

    caps: Vec<u32>,
    depth_func: u32,
    depth_mask: bool,
    blend_src: u32,
    blend_dst: u32,
    alpha_func: u32,
    alpha_ref: f32,
    cull_mode: u32,
    front_face: u32,
    shade_model: u32,
    color_mask: [bool; 4],
    scissor: [i32; 4],

    textures: Vec<(u32, Tex)>,
    next_name: u32,
    /// The active texture unit (multitexture), and the one the client's
    /// texture coordinate array calls go to.
    unit: usize,
    client_unit: usize,
    /// Each unit's bound texture: [2D, 1D].
    bound: [[u32; 2]; 2],
    /// Each unit's texturing on: [2D, 1D].
    tex_on: [[bool; 2]; 2],
    tex_env: [u32; 2],
    env_color: [[f32; 4]; 2],
    /// Each unit's texture coordinate generation, for s, t, r, q.
    texgen: [[TexGen; 4]; 2],
    /// User clip planes, in eye coordinates.
    clip_planes: [[f32; 4]; 6],
    /// glPolygonOffset's factor and units.
    poly_offset: [f32; 2],
    poly_stipple: [u32; 32],
    /// glLineStipple's factor and pattern, and how far along a strip the
    /// pattern is.
    line_stipple: [u32; 2],
    stipple_count: f32,
    logic_op: u32,
    /// The current polygon's offset, while its outline or corners are drawn
    /// as lines or points.
    offset_now: f64,
    offsetting: bool,
    /// GL_PERSPECTIVE_CORRECTION_HINT is GL_NICEST. Only kept for glGet:
    /// textures are always perspective-correct (Windows' software OpenGL
    /// approximates below GL_NICEST), colours always linear in the window.
    perspective: bool,
    /// GL_FOG_HINT is GL_NICEST. Only kept for glGet: fog is always worked
    /// out per pixel, from the eye distance interpolated linearly in the
    /// window, as Windows' software OpenGL does at GL_NICEST (below it,
    /// Windows approximates differently).
    fog_nicest: bool,

    light: light::Lighting,
    fog: light::Fog,
    line_width: f32,
    point_size: f32,
    /// Front and back: GL_FILL, GL_LINE or GL_POINT.
    polygon_mode: [u32; 2],
    /// Function, reference, mask, write mask, and the three operations.
    stencil: [u32; 7],
    clear_stencil: u32,
    lists: lists::Lists,

    prim: Option<u32>,
    verts: Vec<Vtx>,
    arrays: [Array; 5],

    groups: Vec<KGroup>,
    tris: Vec<KTri>,
    error: u32,
    unpack: pixels::PixelStore,
    pack: pixels::PixelStore,
    transfer: pixels::Transfer,
    raster: pixels::Raster,
    /// Textures made for glDrawPixels and glBitmap, closed after the batch
    /// that uses them.
    temp_textures: Vec<u64>,
    /// glDrawBuffer(GL_NONE); drawing into the front buffer (shown at each
    /// glFlush); glReadBuffer of the front buffer.
    draw_none: bool,
    draw_front: bool,
    read_front: bool,
    /// The frame last shown: the front buffer, double-buffered.
    front: Vec<u32>,
    accum: Vec<[f32; 4]>,
    clear_accum: [f32; 4],
    edge_flag: bool,
    select: select::Select,
    eval: eval::Eval,
    attrib_stack: Vec<attrib::Saved>,
    client_stack: Vec<attrib::ClientSaved>,
    swap_interval: i32,
    /// Frame read back for presenting.
    frame: Vec<u32>,
}

const A_VERTEX: usize = 0;
const A_COLOR: usize = 1;
const A_TEXCOORD: usize = 2;
const A_NORMAL: usize = 3;
/// The second texture unit's coordinate array.
const A_TEXCOORD2: usize = 4;

#[derive(Clone, Copy)]
struct TexGen {
    on: bool,
    mode: u32,
    object: [f32; 4],
    eye: [f32; 4],
}

impl TexGen {
    fn new(coord: usize) -> TexGen {
        let plane = match coord {
            0 => [1.0, 0.0, 0.0, 0.0],
            1 => [0.0, 1.0, 0.0, 0.0],
            _ => [0.0; 4],
        };
        TexGen { on: false, mode: GL_EYE_LINEAR, object: plane, eye: plane }
    }
}

/// The inverse of `m` (column-major), or identity if it has none.
fn inverse(m: &Mat) -> Mat {
    let a: [f64; 16] = core::array::from_fn(|i| m[i] as f64);
    let mut inv = [0f64; 16];
    inv[0] = a[5] * a[10] * a[15] - a[5] * a[11] * a[14] - a[9] * a[6] * a[15] + a[9] * a[7] * a[14] + a[13] * a[6] * a[11] - a[13] * a[7] * a[10];
    inv[4] = -a[4] * a[10] * a[15] + a[4] * a[11] * a[14] + a[8] * a[6] * a[15] - a[8] * a[7] * a[14] - a[12] * a[6] * a[11] + a[12] * a[7] * a[10];
    inv[8] = a[4] * a[9] * a[15] - a[4] * a[11] * a[13] - a[8] * a[5] * a[15] + a[8] * a[7] * a[13] + a[12] * a[5] * a[11] - a[12] * a[7] * a[9];
    inv[12] = -a[4] * a[9] * a[14] + a[4] * a[10] * a[13] + a[8] * a[5] * a[14] - a[8] * a[6] * a[13] - a[12] * a[5] * a[10] + a[12] * a[6] * a[9];
    inv[1] = -a[1] * a[10] * a[15] + a[1] * a[11] * a[14] + a[9] * a[2] * a[15] - a[9] * a[3] * a[14] - a[13] * a[2] * a[11] + a[13] * a[3] * a[10];
    inv[5] = a[0] * a[10] * a[15] - a[0] * a[11] * a[14] - a[8] * a[2] * a[15] + a[8] * a[3] * a[14] + a[12] * a[2] * a[11] - a[12] * a[3] * a[10];
    inv[9] = -a[0] * a[9] * a[15] + a[0] * a[11] * a[13] + a[8] * a[1] * a[15] - a[8] * a[3] * a[13] - a[12] * a[1] * a[11] + a[12] * a[3] * a[9];
    inv[13] = a[0] * a[9] * a[14] - a[0] * a[10] * a[13] - a[8] * a[1] * a[14] + a[8] * a[2] * a[13] + a[12] * a[1] * a[10] - a[12] * a[2] * a[9];
    inv[2] = a[1] * a[6] * a[15] - a[1] * a[7] * a[14] - a[5] * a[2] * a[15] + a[5] * a[3] * a[14] + a[13] * a[2] * a[7] - a[13] * a[3] * a[6];
    inv[6] = -a[0] * a[6] * a[15] + a[0] * a[7] * a[14] + a[4] * a[2] * a[15] - a[4] * a[3] * a[14] - a[12] * a[2] * a[7] + a[12] * a[3] * a[6];
    inv[10] = a[0] * a[5] * a[15] - a[0] * a[7] * a[13] - a[4] * a[1] * a[15] + a[4] * a[3] * a[13] + a[12] * a[1] * a[7] - a[12] * a[3] * a[5];
    inv[14] = -a[0] * a[5] * a[14] + a[0] * a[6] * a[13] + a[4] * a[1] * a[14] - a[4] * a[2] * a[13] - a[12] * a[1] * a[6] + a[12] * a[2] * a[5];
    inv[3] = -a[1] * a[6] * a[11] + a[1] * a[7] * a[10] + a[5] * a[2] * a[11] - a[5] * a[3] * a[10] - a[9] * a[2] * a[7] + a[9] * a[3] * a[6];
    inv[7] = a[0] * a[6] * a[11] - a[0] * a[7] * a[10] - a[4] * a[2] * a[11] + a[4] * a[3] * a[10] + a[8] * a[2] * a[7] - a[8] * a[3] * a[6];
    inv[11] = -a[0] * a[5] * a[11] + a[0] * a[7] * a[9] + a[4] * a[1] * a[11] - a[4] * a[3] * a[9] - a[8] * a[1] * a[7] + a[8] * a[3] * a[5];
    inv[15] = a[0] * a[5] * a[10] - a[0] * a[6] * a[9] - a[4] * a[1] * a[10] + a[4] * a[2] * a[9] + a[8] * a[1] * a[6] - a[8] * a[2] * a[5];
    let det = a[0] * inv[0] + a[1] * inv[4] + a[2] * inv[8] + a[3] * inv[12];
    if det == 0.0 {
        return IDENTITY;
    }
    core::array::from_fn(|i| (inv[i] / det) as f32)
}

/// A plane (a row vector) carried into the space `m` maps to: p m^-1.
fn plane_through(p: [f32; 4], m: &Mat) -> [f32; 4] {
    let inv = inverse(m);
    core::array::from_fn(|c| (0..4).map(|r| p[r] * inv[c * 4 + r]).sum())
}

fn dot4(a: [f32; 4], b: [f32; 4]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2] + a[3] * b[3]
}

/// Batches are sent once this many triangles are waiting.
const BATCH_TRIS: usize = 8192;

impl Ctx {
    fn err(&mut self, e: u32) {
        if self.error == GL_NO_ERROR {
            self.error = e;
        }
    }

    fn on(&self, cap: u32) -> bool {
        match cap {
            GL_TEXTURE_2D => self.tex_on[self.unit][0],
            GL_TEXTURE_1D => self.tex_on[self.unit][1],
            c if (GL_TEXTURE_GEN_S..GL_TEXTURE_GEN_S + 4).contains(&c) => self.texgen[self.unit][(c - GL_TEXTURE_GEN_S) as usize].on,
            _ => self.caps.contains(&cap),
        }
    }

    /// The matrix stack glMatrixMode chose.
    fn stack(&self) -> usize {
        match self.matrix_mode {
            GL_PROJECTION => 1,
            GL_TEXTURE => 2 + self.unit,
            _ => 0,
        }
    }

    fn top(&mut self) -> &mut Mat {
        let i = self.stack();
        self.stacks[i].last_mut().unwrap()
    }

    fn mult(&mut self, m: &Mat) {
        let t = self.top();
        *t = mul(t, m);
    }

    fn tex(&mut self, name: u32) -> Option<&mut Tex> {
        self.textures.iter_mut().find(|t| t.0 == name).map(|t| &mut t.1)
    }

    /// Unit `unit`'s bound texture of `dim` (0 2D, 1 1D).
    fn bound_of(&mut self, unit: usize, dim: usize) -> &mut Tex {
        let n = self.bound[unit][dim];
        if self.tex(n).is_none() {
            self.textures.push((n, Tex::new()));
        }
        self.tex(n).unwrap()
    }

    /// The scissor rectangle in the kernel's terms (y downwards).
    fn scissor_box(&self) -> [i32; 4] {
        if self.on(GL_SCISSOR_TEST) {
            let [x, y, w, h] = self.scissor;
            [x, self.h - (y + h), x + w, self.h - y]
        } else {
            [0, 0, self.w, self.h]
        }
    }

    fn mask_bits(&self) -> u32 {
        if self.draw_none {
            return 0;
        }
        (self.color_mask[0] as u32) | (self.color_mask[1] as u32) << 1 | (self.color_mask[2] as u32) << 2
    }

    // -----| batches |-----

    fn flush(&mut self) {
        if self.tris.is_empty() {
            self.groups.clear();
            return;
        }
        let args = DrawArgs {
            groups: self.groups.as_ptr() as u64,
            group_count: self.groups.len() as u32,
            tri_count: self.tris.len() as u32,
            tris: self.tris.as_ptr() as u64,
            attrs: 0,
            attr_len: 0,
            _pad: 0,
        };
        unsafe { VeilGpu3d(2, self.target, &args as *const _ as u64, 0) };
        self.groups.clear();
        self.tris.clear();
        for h in self.temp_textures.drain(..) {
            unsafe { NtClose(h) };
        }
    }

    /// Unit `unit`'s texture (2D over 1D, as GL has it): how the kernel
    /// samples it (its handle included) and its size, if texturing applies.
    fn texture_for_draw(&mut self, unit: usize) -> Option<(Sampler, u32, u32)> {
        let dim = if self.tex_on[unit][0] { 0 } else if self.tex_on[unit][1] { 1 } else { return None };
        let t = self.bound_of(unit, dim);
        if !t.complete() {
            return None;
        }
        if t.handle == 0 || t.dirty {
            // Every level given, one after the other.
            let n = if t.min == GL_NEAREST || t.min == GL_LINEAR { 1 } else { t.given() };
            let texels: Vec<u32> = t.mips[..n].concat();
            let args = TexArgs {
                w: t.w,
                h: t.h,
                texels: texels.as_ptr() as u64,
                levels: n as u32,
            };
            let mut h = t.handle;
            unsafe { VeilGpu3d(4, t.handle, &args as *const _ as u64, &mut h as *mut u64 as u64) };
            t.handle = h;
            t.dirty = false;
        }
        Some((sampler(t), t.w, t.h))
    }

    fn state(&mut self, kind: u32, size: f32) -> (KGroup, [Option<(u32, u32)>; 2]) {
        let tex = self.texture_for_draw(0);
        let tex2 = self.texture_for_draw(1);
        let mut flags = 0;
        if self.on(GL_DEPTH_TEST) {
            flags |= F_DEPTH_TEST;
            if self.depth_mask {
                flags |= F_DEPTH_WRITE;
            }
        }
        if self.on(GL_BLEND) {
            flags |= F_BLEND;
        }
        if self.on(GL_ALPHA_TEST) {
            flags |= F_ALPHA_TEST;
        }
        if self.on(GL_FOG) {
            flags |= F_FOG;
        }
        if self.on(GL_STENCIL_TEST) {
            flags |= F_STENCIL;
        }
        if self.on(GL_COLOR_LOGIC_OP) {
            flags |= F_LOGIC_OP;
        }
        if kind == KIND_TRIANGLES && self.on(GL_POLYGON_STIPPLE) {
            flags |= F_POLY_STIPPLE;
        }
        if kind == KIND_LINES && self.on(GL_LINE_STIPPLE) {
            flags |= F_LINE_STIPPLE;
        }
        let ch = |v: f32| (clamp01(v) * 255.0 + 0.5) as u32;
        let s = self.stencil;
        let g = KGroup {
            first: 0,
            count: 0,
            flags,
            kind,
            size: (size.max(1.0) * SUB as f32) as i32,
            fog_color: ch(self.fog.color[0]) << 16 | ch(self.fog.color[1]) << 8 | ch(self.fog.color[2]),
            stencil_func: s[0],
            stencil_ref: s[1].min(255),
            stencil_mask: s[2] & 0xFF,
            stencil_write: s[3] & 0xFF,
            stencil_ops: [s[4], s[5], s[6]],
            fog_mode: self.fog.mode,
            fog_params: {
                let f = &self.fog;
                let p = if f.mode == GL_LINEAR_FOG && f.end != f.start {
                    [-1.0 / (f.end - f.start) as f64, f.end as f64 / (f.end - f.start) as f64]
                } else if f.mode == GL_LINEAR_FOG {
                    [0.0, 1.0]
                } else {
                    [f.density as f64, 0.0]
                };
                [(p[0] * FX) as i64, (p[1] * FX) as i64]
            },
            depth_func: self.depth_func,
            alpha_func: self.alpha_func,
            alpha_ref: (clamp01(self.alpha_ref) * 510.0 + 0.5) as u32,
            src_blend: self.blend_src,
            dst_blend: self.blend_dst,
            logic_op: self.logic_op,
            line_stipple: (self.line_stipple[1] & 0xFFFF) | self.line_stipple[0].clamp(1, 256) << 16,
            stipple: self.poly_stipple,
            color_mask: self.mask_bits(),
            scissor: self.scissor_box(),
            stages: {
                // Each unit's environment as a texture stage; unit 0 passes
                // the colour on unchanged if only unit 1 textures.
                let mut st = [Stage::default(); k::STAGES];
                let last = if tex2.is_some() { 2 } else if tex.is_some() { 1 } else { 0 };
                for (u, s) in st.iter_mut().enumerate().take(last) {
                    let on = if u == 0 { tex.is_some() } else { tex2.is_some() };
                    let c = self.env_color[u];
                    *s = gl_stage(if on { self.tex_env[u] } else { 0 }, ch(c[3]) << 24 | ch(c[0]) << 16 | ch(c[1]) << 8 | ch(c[2]));
                }
                st
            },
            samplers: {
                let mut s = [Sampler::default(); k::SAMPLERS];
                s[0] = tex.map_or(Sampler::default(), |t| t.0);
                s[1] = tex2.map_or(Sampler::default(), |t| t.0);
                s
            },
            ..Default::default()
        };
        (g, [tex.map(|t| (t.1, t.2)), tex2.map(|t| (t.1, t.2))])
    }

    fn push_tri(&mut self, g: &KGroup, t: KTri) {
        let same = self.groups.last().is_some_and(|l| {
            let mut l = *l;
            l.first = 0;
            l.count = 0;
            l == *g
        });
        if !same {
            let mut n = *g;
            n.first = self.tris.len() as u32;
            self.groups.push(n);
        }
        self.groups.last_mut().unwrap().count += 1;
        self.tris.push(t);
    }

    // -----| the vertex stage |-----

    fn vertex(&mut self, obj: [f32; 4]) {
        let mv = self.stacks[0].last().copied().unwrap();
        let eye = xform(&mv, obj);
        let clip = xform(&self.stacks[1].last().copied().unwrap(), eye);
        let lit = self.on(GL_LIGHTING);
        let sphere = (0..2).any(|u| self.texgen[u][..2].iter().any(|g| g.on && g.mode == GL_SPHERE_MAP));
        // The eye-space normal, when lighting or a sphere map needs it.
        let ne = if lit || sphere {
            let nm = normal_matrix(&mv);
            let n = self.normal;
            let mut ne = [0.0f32; 3];
            for (r, o) in ne.iter_mut().enumerate() {
                *o = nm[r][0] * n[0] + nm[r][1] * n[1] + nm[r][2] * n[2];
            }
            if self.on(GL_NORMALIZE) {
                ne = light::normalize(ne);
            }
            ne
        } else {
            [0.0; 3]
        };
        // Texture coordinates: generated where asked, then each unit's
        // texture matrix.
        let mut tc = self.tc;
        for (u, t) in tc.iter_mut().enumerate() {
            for (k, g) in self.texgen[u].iter().enumerate() {
                if !g.on {
                    continue;
                }
                t[k] = match g.mode {
                    GL_OBJECT_LINEAR => dot4(g.object, obj),
                    GL_EYE_LINEAR => dot4(g.eye, eye),
                    _ => {
                        // GL_SPHERE_MAP: from the reflection of the eye
                        // direction about the normal.
                        let e = light::normalize([eye[0], eye[1], eye[2]]);
                        let d = 2.0 * (ne[0] * e[0] + ne[1] * e[1] + ne[2] * e[2]);
                        let r = [e[0] - d * ne[0], e[1] - d * ne[1], e[2] - d * ne[2] + 1.0];
                        let m = 2.0 * sqrt((r[0] * r[0] + r[1] * r[1] + r[2] * r[2]) as f64) as f32;
                        if m == 0.0 { 0.5 } else { r[k.min(1)] / m + 0.5 }
                    }
                };
            }
            *t = xform(&self.stacks[2 + u].last().copied().unwrap(), *t);
        }
        let clipd = core::array::from_fn(|i| if self.on(GL_CLIP_PLANE0 + i as u32) { dot4(self.clip_planes[i], eye) } else { 0.0 });
        let (col, back) = if lit {
            if self.on(GL_COLOR_MATERIAL) {
                self.light.color_material(self.color);
            }
            let front = self.light.shade(eye, ne, 0);
            (front, if self.light.two_side { self.light.shade(eye, ne, 1) } else { front })
        } else {
            (self.color, self.color)
        };
        // Fog by eye distance along z.
        let dist = if eye[3] != 0.0 { (eye[2] / eye[3]).abs() } else { eye[2].abs() };
        self.verts.push(Vtx { clip, col, back, tc, clipd, dist });
    }

    /// Window coordinates (y upwards, as GL has them), depth and 1/w.
    fn window(&self, v: &Vtx) -> [f64; 4] {
        let [vx, vy, vw, vh] = self.viewport;
        let (n, f) = (self.depth_range[0] as f64, self.depth_range[1] as f64);
        let iw = 1.0 / v.clip[3] as f64;
        [
            (v.clip[0] as f64 * iw + 1.0) * vw as f64 / 2.0 + vx as f64,
            (v.clip[1] as f64 * iw + 1.0) * vh as f64 / 2.0 + vy as f64,
            (v.clip[2] as f64 * iw) * (f - n) / 2.0 + (f + n) / 2.0,
            iw,
        ]
    }

    /// The kernel's vertices for one primitive (`back`: its back face's
    /// colours), with a texture of `tw` x `th` texels.
    fn kverts(&self, poly: &[Vtx], win: &[[f64; 4]], back: bool, tex: [Option<(u32, u32)>; 2]) -> Vec<KVert> {
        let (tw, th) = tex[0].map_or((0.0, 0.0), |(w, h)| (w as f64, h as f64));
        let (tw2, th2) = tex[1].map_or((0.0, 0.0), |(w, h)| (w as f64, h as f64));
        let qmax = win.iter().map(|w| w[3]).fold(0.0f64, f64::max);
        poly.iter()
            .zip(win.iter())
            .map(|(v, w)| {
                let q = if qmax > 0.0 { w[3] / qmax } else { 1.0 };
                let col = |c: f32| (clamp01(c) as f64 * 255.0 * FX) as i64;
                let c = if back { v.back } else { v.col };
                KVert {
                    x: round(w[0] * SUB) as i32,
                    y: round((self.h as f64 - w[1]) * SUB) as i32,
                    z: (w[2].clamp(0.0, 1.0) * DEPTH_MAX * FX) as i64,
                    q: (q * FX) as i64,
                    uv: [
                        [(v.tc[0][0] as f64 / v.tc[0][3] as f64 * tw * q * FX) as i64, (v.tc[0][1] as f64 / v.tc[0][3] as f64 * th * q * FX) as i64],
                        [(v.tc[1][0] as f64 / v.tc[1][3] as f64 * tw2 * q * FX) as i64, (v.tc[1][1] as f64 / v.tc[1][3] as f64 * th2 * q * FX) as i64],
                        [0; 2],
                        [0; 2],
                    ],
                    spec: [0; 3],
                    r: col(c[0]),
                    g: col(c[1]),
                    b: col(c[2]),
                    a: col(c[3]),
                    // Fog per pixel from this, linear in the window.
                    f: (v.dist as f64 * FX) as i64,
                }
            })
            .collect()
    }

    /// Clip a line to the view volume and draw it.
    fn line(&mut self, a: Vtx, b: Vtx) {
        let mut b = b;
        let mut a = a;
        if self.shade_model == GL_FLAT {
            a.col = b.col;
            a.back = b.back;
        }
        let (mut t0, mut t1) = (0.0f32, 1.0f32);
        for plane in 0..12 {
            if plane >= 6 && !self.on(GL_CLIP_PLANE0 + plane as u32 - 6) {
                continue;
            }
            let d = |v: &Vtx| plane_distance(v, plane);
            let (da, db) = (d(&a), d(&b));
            if da < 0.0 && db < 0.0 {
                return;
            }
            if da < 0.0 {
                t0 = t0.max(da / (da - db));
            } else if db < 0.0 {
                t1 = t1.min(da / (da - db));
            }
        }
        if t0 > t1 {
            return;
        }
        let (na, nb) = (a.lerp(&b, t0), a.lerp(&b, t1));
        a = na;
        b = nb;
        if self.select.mode != select::GL_RENDER {
            let win = [self.window(&a), self.window(&b)];
            self.capture(KIND_LINES, &[a, b], &win, false);
            return;
        }
        let (g, tex) = self.state(KIND_LINES, self.line_width);
        let mut win = [self.window(&a), self.window(&b)];
        if self.on(GL_POLYGON_OFFSET_LINE) && self.offsetting {
            let o = self.offset_now;
            for w in &mut win {
                w[2] += o;
            }
        }
        let kv = self.kverts(&[a, b], &win, false, tex);
        // Where the stipple pattern is along a strip, in pixels.
        let mut third = kv[1];
        third.x = self.stipple_count as i32;
        let len = (win[1][0] - win[0][0]).abs().max((win[1][1] - win[0][1]).abs());
        self.stipple_count += len as f32;
        self.push_tri(&g, KTri { v: [kv[0], kv[1], third] });
    }

    fn point(&mut self, v: Vtx) {
        let c = v.clip;
        if (0..3).any(|k| c[k] < -c[3] || c[k] > c[3]) || (0..6).any(|i| self.on(GL_CLIP_PLANE0 + i as u32) && v.clipd[i] < 0.0) {
            return;
        }
        if self.select.mode != select::GL_RENDER {
            let win = [self.window(&v)];
            self.capture(KIND_POINTS, &[v], &win, false);
            return;
        }
        let (g, tex) = self.state(KIND_POINTS, self.point_size);
        let mut win = [self.window(&v)];
        if self.on(GL_POLYGON_OFFSET_POINT) && self.offsetting {
            win[0][2] += self.offset_now;
        }
        let kv = self.kverts(&[v], &win, false, tex);
        self.push_tri(&g, KTri { v: [kv[0], kv[0], kv[0]] });
    }

    /// Turn the vertices of one primitive (`mode`) into polygons and draw them.
    fn assemble(&mut self, mode: u32, v: &[Vtx]) {
        let n = v.len();
        match mode {
            GL_TRIANGLES => {
                for i in (0..n / 3).map(|i| i * 3) {
                    self.polygon(&[v[i], v[i + 1], v[i + 2]], 2);
                }
            }
            GL_TRIANGLE_STRIP => {
                for i in 0..n.saturating_sub(2) {
                    if i % 2 == 0 {
                        self.polygon(&[v[i], v[i + 1], v[i + 2]], 2);
                    } else {
                        self.polygon(&[v[i + 1], v[i], v[i + 2]], 2);
                    }
                }
            }
            GL_TRIANGLE_FAN => {
                for i in 1..n.saturating_sub(1) {
                    self.polygon(&[v[0], v[i], v[i + 1]], 2);
                }
            }
            GL_QUADS => {
                for i in (0..n / 4).map(|i| i * 4) {
                    self.polygon(&[v[i], v[i + 1], v[i + 2], v[i + 3]], 3);
                }
            }
            GL_QUAD_STRIP => {
                for i in 0..(n / 2).saturating_sub(1) {
                    let b = i * 2;
                    self.polygon(&[v[b], v[b + 1], v[b + 3], v[b + 2]], 2);
                }
            }
            GL_POLYGON => {
                if n >= 3 {
                    self.polygon(v, 0);
                }
            }
            GL_POINTS => {
                for p in v {
                    self.point(*p);
                }
            }
            GL_LINES => {
                for i in (0..n / 2).map(|i| i * 2) {
                    self.line(v[i], v[i + 1]);
                }
            }
            GL_LINE_STRIP | GL_LINE_LOOP => {
                for i in 0..n.saturating_sub(1) {
                    self.line(v[i], v[i + 1]);
                }
                if mode == GL_LINE_LOOP && n >= 2 {
                    self.line(v[n - 1], v[0]);
                }
            }
            _ => {}
        }
        if self.tris.len() >= BATCH_TRIS {
            self.flush();
        }
    }

    /// Clip, cull and draw one polygon; `provoking` is the vertex whose
    /// colour a flat-shaded polygon takes.
    fn polygon(&mut self, input: &[Vtx], provoking: usize) {
        let mut poly: Vec<Vtx> = input.to_vec();
        if self.shade_model == GL_FLAT {
            let (c, b) = (input[provoking].col, input[provoking].back);
            for p in &mut poly {
                p.col = c;
                p.back = b;
            }
        }
        // Against the six planes of the view volume, in clip space, then
        // the user's clip planes.
        for plane in 0..12 {
            if plane >= 6 && !self.on(GL_CLIP_PLANE0 + plane as u32 - 6) {
                continue;
            }
            let d = |v: &Vtx| plane_distance(v, plane);
            if poly.iter().all(|v| d(v) >= 0.0) {
                continue;
            }
            let mut out = Vec::with_capacity(poly.len() + 2);
            // As Windows' software OpenGL walks it: each edge as (previous,
            // current) from vertex 0, the crossing point first. Which vertex
            // the result starts with decides how it is cut into triangles,
            // and so how colours are interpolated across it.
            for i in 0..poly.len() {
                let a = poly[(i + poly.len() - 1) % poly.len()];
                let b = poly[i];
                let (da, db) = (d(&a), d(&b));
                if (da >= 0.0) != (db >= 0.0) {
                    out.push(a.lerp(&b, da / (da - db)));
                }
                if db >= 0.0 {
                    out.push(b);
                }
            }
            poly = out;
            if poly.len() < 3 {
                return;
            }
        }
        // To the window (y upwards here, as GL has it).
        let mut win: Vec<[f64; 4]> = poly.iter().map(|v| self.window(v)).collect();
        // Facing: counter-clockwise in the window is positive area.
        let mut area = 0.0;
        for i in 0..win.len() {
            let (a, b) = (win[i], win[(i + 1) % win.len()]);
            area += a[0] * b[1] - b[0] * a[1];
        }
        if area == 0.0 {
            return;
        }
        let front = (area > 0.0) == (self.front_face == GL_CCW);
        if self.on(GL_CULL_FACE) {
            let culled = match self.cull_mode {
                GL_FRONT => front,
                GL_FRONT_AND_BACK => true,
                _ => !front,
            };
            if culled {
                return;
            }
        }
        // Two-sided lighting: a back face takes its back colours.
        let back = !front && self.on(GL_LIGHTING) && self.light.two_side;
        if self.select.mode != select::GL_RENDER {
            self.capture(KIND_TRIANGLES, &poly, &win, back);
            return;
        }
        // glPolygonOffset: factor x the steepest depth slope + units x the
        // smallest depth step, for the face's fill mode.
        let mode = self.polygon_mode[!front as usize];
        let cap = match mode { GL_LINE => GL_POLYGON_OFFSET_LINE, GL_POINT => GL_POLYGON_OFFSET_POINT, _ => GL_POLYGON_OFFSET_FILL };
        self.offset_now = 0.0;
        if self.on(cap) {
            let (a, b, c) = (win[0], win[1], win[2]);
            let (e1, e2) = ([b[0] - a[0], b[1] - a[1], b[2] - a[2]], [c[0] - a[0], c[1] - a[1], c[2] - a[2]]);
            let nz = e1[0] * e2[1] - e1[1] * e2[0];
            let slope = if nz == 0.0 {
                0.0
            } else {
                let nx = e1[1] * e2[2] - e1[2] * e2[1];
                let ny = e1[2] * e2[0] - e1[0] * e2[2];
                (nx / nz).abs().max((ny / nz).abs())
            };
            self.offset_now = self.poly_offset[0] as f64 * slope + self.poly_offset[1] as f64 / DEPTH_MAX;
            if mode == GL_FILL {
                for w in &mut win {
                    w[2] += self.offset_now;
                }
            }
        }
        self.offsetting = true;
        // glPolygonMode: outlines or corners instead of filling.
        match self.polygon_mode[!front as usize] {
            GL_LINE => {
                for i in 0..input.len() {
                    let (mut a, mut b) = (input[i], input[(i + 1) % input.len()]);
                    if back {
                        a.col = a.back;
                        b.col = b.back;
                    }
                    let flat = core::mem::replace(&mut self.shade_model, GL_SMOOTH);
                    if flat == GL_FLAT {
                        a.col = poly[0].col;
                        b.col = poly[0].col;
                    }
                    self.line(a, b);
                    self.shade_model = flat;
                }
                self.offsetting = false;
                return;
            }
            GL_POINT => {
                for v in input {
                    let mut v = *v;
                    if back {
                        v.col = v.back;
                    }
                    self.point(v);
                }
                self.offsetting = false;
                return;
            }
            _ => {}
        }
        self.offsetting = false;
        let (g, tex) = self.state(KIND_TRIANGLES, 1.0);
        let kv = self.kverts(&poly, &win, back, tex);
        for i in 1..kv.len() - 1 {
            self.push_tri(&g, KTri { v: [kv[0], kv[i], kv[i + 1]] });
        }
    }

    // -----| frames |-----

    /// Read the colour buffer (top row first) into `frame`.
    fn read_frame(&mut self) {
        self.flush();
        let n = (self.w * self.h) as usize;
        self.frame.resize(n, 0);
        let r = ReadArgs { x: 0, y: 0, w: self.w, h: self.h, dst: self.frame.as_mut_ptr() as u64, stride: self.w as i64 * 4 };
        unsafe { VeilGpu3d(3, self.target, &r as *const _ as u64, 0) };
    }

    /// Draw the frame on the DC.
    fn present(&mut self) {
        self.read_frame();
        let mut bmi = [0u8; 40];
        bmi[0] = 40;
        bmi[4..8].copy_from_slice(&self.w.to_le_bytes());
        bmi[8..12].copy_from_slice(&(-self.h).to_le_bytes());
        bmi[12] = 1;
        bmi[14] = 32;
        unsafe {
            SetDIBitsToDevice(self.hdc, 0, 0, self.w as u32, self.h as u32, 0, 0, 0, self.h as u32, self.frame.as_ptr() as *const u8, bmi.as_ptr(), 0)
        };
        self.front.clone_from(&self.frame);
    }

    /// Follow the DC's size (a resized window gets a new, blank target).
    fn track_size(&mut self) {
        let mut s = [0i32; 2];
        if unsafe { VeilGdiDcSize(self.hdc, &mut s) } == 0 || s[0] <= 0 || s[1] <= 0 || (s[0] == self.w && s[1] == self.h) {
            return;
        }
        self.flush();
        let mut t = 0u64;
        if unsafe { VeilGpu3d(0, s[0] as u64, s[1] as u64, &mut t as *mut u64 as u64) } == 0 {
            unsafe { NtClose(self.target) };
            self.target = t;
            self.w = s[0];
            self.h = s[1];
        }
    }
}

// -----| the current context |-----

static TLS: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(u32::MAX);
static CONTEXTS: Lock<Vec<usize>> = Lock::new(Vec::new());

fn tls() -> u32 {
    use core::sync::atomic::Ordering;
    let i = TLS.load(Ordering::Acquire);
    if i != u32::MAX {
        return i;
    }
    let n = unsafe { TlsAlloc() };
    match TLS.compare_exchange(u32::MAX, n, Ordering::AcqRel, Ordering::Acquire) {
        Ok(_) => n,
        Err(other) => other,
    }
}

fn current() -> Option<&'static mut Ctx> {
    let p = unsafe { TlsGetValue(tls()) };
    (p != 0).then(|| unsafe { &mut *(p as *mut Ctx) })
}

/// Run `f` on the current context; without one, calls do nothing.
fn with<R: Default>(f: impl FnOnce(&mut Ctx) -> R) -> R {
    match current() {
        Some(c) => f(c),
        None => R::default(),
    }
}

/// As `with`, but an error if called between glBegin and glEnd.
fn outside<R: Default>(f: impl FnOnce(&mut Ctx) -> R) -> R {
    with(|c| {
        if c.prim.is_some() {
            c.err(GL_INVALID_OPERATION);
            return R::default();
        }
        f(c)
    })
}

#[no_mangle]
pub extern "system" fn DllMain(_module: usize, reason: u32, _reserved: usize) -> i32 {
    if reason == 1 {
        tls();
        unsafe { VeilGdiSetSwapHook(wglSwapBuffers as *const () as usize) };
    }
    1
}

// -----| wgl |-----

#[no_mangle]
pub extern "system" fn wglCreateContext(hdc: u64) -> u64 {
    let mut s = [0i32; 2];
    let kind = unsafe { VeilGdiDcSize(hdc, &mut s) };
    if kind == 0 {
        unsafe { SetLastError(6) };
        return 0;
    }
    let (w, h) = (s[0].max(1), s[1].max(1));
    let mut target = 0u64;
    if unsafe { VeilGpu3d(0, w as u64, h as u64, &mut target as *mut u64 as u64) } != 0 {
        return 0;
    }
    let format = unsafe { GetPixelFormat(hdc) };
    let ctx = Box::new(Ctx {
        hdc,
        target,
        w,
        h,
        double: format == 1,
        made_current: false,
        matrix_mode: GL_MODELVIEW,
        stacks: [vec![IDENTITY], vec![IDENTITY], vec![IDENTITY], vec![IDENTITY]],
        viewport: [0, 0, w, h],
        depth_range: [0.0, 1.0],
        clear_color: [0.0; 4],
        clear_depth: 1.0,
        color: [1.0; 4],
        tc: [[0.0, 0.0, 0.0, 1.0]; 2],
        normal: [0.0, 0.0, 1.0],
        caps: vec![GL_DITHER],
        depth_func: GL_LESS,
        depth_mask: true,
        blend_src: GL_ONE,
        blend_dst: GL_ZERO,
        alpha_func: GL_ALWAYS,
        alpha_ref: 0.0,
        cull_mode: GL_BACK,
        front_face: GL_CCW,
        shade_model: GL_SMOOTH,
        color_mask: [true; 4],
        scissor: [0, 0, w, h],
        textures: Vec::new(),
        next_name: 1,
        unit: 0,
        client_unit: 0,
        bound: [[0; 2]; 2],
        tex_on: [[false; 2]; 2],
        tex_env: [GL_MODULATE; 2],
        env_color: [[0.0; 4]; 2],
        texgen: [core::array::from_fn(TexGen::new); 2],
        clip_planes: [[0.0; 4]; 6],
        poly_offset: [0.0; 2],
        poly_stipple: [0xFFFF_FFFF; 32],
        line_stipple: [1, 0xFFFF],
        stipple_count: 0.0,
        logic_op: 0x1503,
        offset_now: 0.0,
        offsetting: false,
        perspective: false,
        fog_nicest: false,
        light: light::Lighting::new(),
        fog: light::Fog::new(),
        line_width: 1.0,
        point_size: 1.0,
        polygon_mode: [GL_FILL, GL_FILL],
        stencil: [GL_ALWAYS, 0, 0xFF, 0xFF, GL_KEEP, GL_KEEP, GL_KEEP],
        clear_stencil: 0,
        lists: lists::Lists::default(),
        prim: None,
        verts: Vec::new(),
        arrays: [Array::default(); 5],
        groups: Vec::new(),
        tris: Vec::new(),
        error: GL_NO_ERROR,
        unpack: pixels::PixelStore::new(),
        pack: pixels::PixelStore::new(),
        transfer: pixels::Transfer::new(),
        raster: pixels::Raster::new(),
        temp_textures: Vec::new(),
        draw_none: false,
        draw_front: false,
        read_front: format != 1,
        front: Vec::new(),
        accum: Vec::new(),
        clear_accum: [0.0; 4],
        edge_flag: true,
        select: select::Select::new(),
        eval: eval::Eval::new(),
        attrib_stack: Vec::new(),
        client_stack: Vec::new(),
        swap_interval: 1,
        frame: Vec::new(),
    });
    let p = Box::into_raw(ctx) as usize;
    CONTEXTS.lock().push(p);
    p as u64
}

fn valid(hglrc: u64) -> bool {
    CONTEXTS.lock().contains(&(hglrc as usize))
}

#[no_mangle]
pub extern "system" fn wglMakeCurrent(hdc: u64, hglrc: u64) -> Bool {
    if let Some(c) = current() {
        c.flush();
    }
    if hglrc == 0 {
        unsafe { TlsSetValue(tls(), 0) };
        return 1;
    }
    if !valid(hglrc) {
        unsafe { SetLastError(2000) };
        return 0;
    }
    let c = unsafe { &mut *(hglrc as *mut Ctx) };
    if hdc != 0 && hdc != c.hdc {
        c.hdc = hdc;
        c.track_size();
    }
    if !c.made_current {
        // The viewport and scissor start as the whole window.
        c.made_current = true;
        c.viewport = [0, 0, c.w, c.h];
        c.scissor = [0, 0, c.w, c.h];
    }
    unsafe { TlsSetValue(tls(), hglrc as usize) };
    1
}

#[no_mangle]
pub extern "system" fn wglDeleteContext(hglrc: u64) -> Bool {
    let mut l = CONTEXTS.lock();
    let Some(i) = l.iter().position(|&p| p == hglrc as usize) else { return 0 };
    l.remove(i);
    drop(l);
    if current().is_some_and(|c| c as *mut Ctx as u64 == hglrc) {
        unsafe { TlsSetValue(tls(), 0) };
    }
    let c = unsafe { Box::from_raw(hglrc as *mut Ctx) };
    unsafe { NtClose(c.target) };
    for (_, t) in &c.textures {
        if t.handle != 0 {
            unsafe { NtClose(t.handle) };
        }
    }
    1
}

#[no_mangle]
pub extern "system" fn wglGetCurrentContext() -> u64 {
    current().map_or(0, |c| c as *mut Ctx as u64)
}

#[no_mangle]
pub extern "system" fn wglGetCurrentDC() -> u64 {
    current().map_or(0, |c| c.hdc)
}


#[no_mangle]
pub extern "system" fn wglShareLists(_a: u64, _b: u64) -> Bool {
    1
}

#[no_mangle]
pub extern "system" fn wglSwapBuffers(hdc: u64) -> Bool {
    let Some(c) = current() else { return 0 };
    if c.hdc != hdc {
        return 0;
    }
    c.present();
    c.track_size();
    1
}

#[no_mangle]
pub extern "system" fn wglChoosePixelFormat(hdc: u64, pfd: *const u8) -> i32 {
    unsafe { ChoosePixelFormat(hdc, pfd) }
}

#[no_mangle]
pub extern "system" fn wglDescribePixelFormat(hdc: u64, n: i32, size: u32, out: *mut u8) -> i32 {
    unsafe { DescribePixelFormat(hdc, n, size, out) }
}

#[no_mangle]
pub extern "system" fn wglSetPixelFormat(hdc: u64, n: i32, pfd: *const u8) -> Bool {
    unsafe { SetPixelFormat(hdc, n, pfd) }
}

#[no_mangle]
pub extern "system" fn wglGetPixelFormat(hdc: u64) -> i32 {
    unsafe { GetPixelFormat(hdc) }
}

// -----| state |-----

#[no_mangle]
pub extern "system" fn glGetError() -> u32 {
    with(|c| core::mem::replace(&mut c.error, GL_NO_ERROR))
}

#[no_mangle]
pub extern "system" fn glGetString(name: u32) -> *const u8 {
    let s: &'static [u8] = match name {
        0x1F00 => b"VEILOS\0",
        0x1F01 => b"VEILOS software rasterizer\0",
        0x1F02 => b"1.1.0\0",
        0x1F03 => ext::EXTENSIONS,
        _ => {
            with(|c| c.err(GL_INVALID_ENUM));
            return core::ptr::null();
        }
    };
    if current().is_none() {
        return core::ptr::null();
    }
    s.as_ptr()
}

fn set_cap(cap: u32, on: bool) {
    rec!(set_cap(cap, on));
    outside(|c| {
        if (GL_LIGHT0..GL_LIGHT0 + 8).contains(&cap) {
            c.light.lights[(cap - GL_LIGHT0) as usize].on = on;
        }
        match cap {
            GL_TEXTURE_2D => return c.tex_on[c.unit][0] = on,
            GL_TEXTURE_1D => return c.tex_on[c.unit][1] = on,
            x if (GL_TEXTURE_GEN_S..GL_TEXTURE_GEN_S + 4).contains(&x) => return c.texgen[c.unit][(x - GL_TEXTURE_GEN_S) as usize].on = on,
            _ => {}
        }
        if on != c.on(cap) {
            if on {
                c.caps.push(cap);
            } else {
                c.caps.retain(|&x| x != cap);
            }
        }
    })
}

#[no_mangle]
pub extern "system" fn glEnable(cap: u32) {
    set_cap(cap, true)
}

#[no_mangle]
pub extern "system" fn glDisable(cap: u32) {
    set_cap(cap, false)
}

#[no_mangle]
pub extern "system" fn glIsEnabled(cap: u32) -> u8 {
    with(|c| c.on(cap) as u8)
}

#[no_mangle]
pub extern "system" fn glHint(target: u32, mode: u32) {
    rec!(glHint(target, mode));
    // GL_PERSPECTIVE_CORRECTION_HINT, GL_NICEST.
    outside(|c| {
        if target == 0x0C50 {
            c.perspective = mode == 0x1102;
        }
        // GL_FOG_HINT.
        if target == 0x0C54 {
            c.fog_nicest = mode == 0x1102;
        }
    })
}

#[no_mangle]
pub extern "system" fn glClearColor(r: f32, g: f32, b: f32, a: f32) {
    rec!(glClearColor(r, g, b, a));
    outside(|c| c.clear_color = [clamp01(r), clamp01(g), clamp01(b), clamp01(a)])
}

#[no_mangle]
pub extern "system" fn glClearDepth(d: f64) {
    rec!(glClearDepth(d));
    outside(|c| c.clear_depth = clamp01(d as f32))
}

#[no_mangle]
pub extern "system" fn glClear(mask: u32) {
    rec!(glClear(mask));
    outside(|c| {
        if mask & !(GL_COLOR_BUFFER_BIT | GL_DEPTH_BUFFER_BIT | 0x200 | GL_STENCIL_BUFFER_BIT) != 0 {
            c.err(GL_INVALID_VALUE);
            return;
        }
        c.flush();
        let ch = |v: f32| (v * 255.0 + 0.5) as u32;
        let args = ClearArgs {
            mask: mask & (GL_COLOR_BUFFER_BIT | GL_DEPTH_BUFFER_BIT | GL_STENCIL_BUFFER_BIT),
            color: ch(c.clear_color[0]) << 16 | ch(c.clear_color[1]) << 8 | ch(c.clear_color[2]),
            depth: (c.clear_depth as f64 * DEPTH_MAX + 0.5) as u32,
            color_mask: c.mask_bits(),
            scissor: c.scissor_box(),
            stencil: c.clear_stencil & 0xFF,
            stencil_write: c.stencil[3] & 0xFF,
        };
        unsafe { VeilGpu3d(1, c.target, &args as *const _ as u64, 0) };
        if mask & 0x200 != 0 {
            c.clear_accum_buffer();
        }
    })
}

#[no_mangle]
pub extern "system" fn glViewport(x: i32, y: i32, w: i32, h: i32) {
    rec!(glViewport(x, y, w, h));
    outside(|c| {
        if w < 0 || h < 0 {
            c.err(GL_INVALID_VALUE);
        } else {
            c.viewport = [x, y, w, h];
        }
    })
}

#[no_mangle]
pub extern "system" fn glDepthRange(n: f64, f: f64) {
    rec!(glDepthRange(n, f));
    outside(|c| c.depth_range = [clamp01(n as f32), clamp01(f as f32)])
}

#[no_mangle]
pub extern "system" fn glScissor(x: i32, y: i32, w: i32, h: i32) {
    rec!(glScissor(x, y, w, h));
    outside(|c| {
        if w < 0 || h < 0 {
            c.err(GL_INVALID_VALUE);
        } else {
            c.scissor = [x, y, w, h];
        }
    })
}

fn is_func(f: u32) -> bool {
    (0x200..=0x207).contains(&f)
}

#[no_mangle]
pub extern "system" fn glDepthFunc(f: u32) {
    rec!(glDepthFunc(f));
    outside(|c| if is_func(f) { c.depth_func = f } else { c.err(GL_INVALID_ENUM) })
}

#[no_mangle]
pub extern "system" fn glDepthMask(on: u8) {
    rec!(glDepthMask(on));
    outside(|c| c.depth_mask = on != 0)
}

#[no_mangle]
pub extern "system" fn glAlphaFunc(f: u32, r: f32) {
    rec!(glAlphaFunc(f, r));
    outside(|c| {
        if is_func(f) {
            c.alpha_func = f;
            c.alpha_ref = clamp01(r);
        } else {
            c.err(GL_INVALID_ENUM);
        }
    })
}

#[no_mangle]
pub extern "system" fn glBlendFunc(s: u32, d: u32) {
    rec!(glBlendFunc(s, d));
    let ok = |f: u32| f <= 1 || (0x300..=0x308).contains(&f);
    outside(|c| {
        // GL_SRC_ALPHA_SATURATE is a source factor only; SRC_COLOR and its
        // inverse are destination factors only, DST_COLOR's source only.
        if !ok(s) || !ok(d) || s == 0x300 || s == 0x301 || d == 0x306 || d == 0x307 || d == 0x308 {
            c.err(GL_INVALID_ENUM);
        } else {
            c.blend_src = s;
            c.blend_dst = d;
        }
    })
}

#[no_mangle]
pub extern "system" fn glCullFace(m: u32) {
    rec!(glCullFace(m));
    outside(|c| if matches!(m, GL_FRONT | GL_BACK | GL_FRONT_AND_BACK) { c.cull_mode = m } else { c.err(GL_INVALID_ENUM) })
}

#[no_mangle]
pub extern "system" fn glFrontFace(m: u32) {
    rec!(glFrontFace(m));
    outside(|c| if m == GL_CW || m == GL_CCW { c.front_face = m } else { c.err(GL_INVALID_ENUM) })
}

#[no_mangle]
pub extern "system" fn glShadeModel(m: u32) {
    rec!(glShadeModel(m));
    outside(|c| if m == GL_FLAT || m == GL_SMOOTH { c.shade_model = m } else { c.err(GL_INVALID_ENUM) })
}

#[no_mangle]
pub extern "system" fn glColorMask(r: u8, g: u8, b: u8, a: u8) {
    rec!(glColorMask(r, g, b, a));
    outside(|c| c.color_mask = [r != 0, g != 0, b != 0, a != 0])
}


#[no_mangle]
pub extern "system" fn glFlush() {
    outside(|c| {
        c.flush();
        if !c.double || c.draw_front {
            c.present();
        }
    })
}

#[no_mangle]
pub extern "system" fn glFinish() {
    glFlush()
}

#[no_mangle]
pub unsafe extern "system" fn glGetIntegerv(name: u32, out: *mut i32) {
    let mut v = [0f64; 16];
    let n = get(name, &mut v);
    for (i, x) in v.iter().enumerate().take(n) {
        unsafe { *out.add(i) = *x as i32 };
    }
}

#[no_mangle]
pub unsafe extern "system" fn glGetFloatv(name: u32, out: *mut f32) {
    let mut v = [0f64; 16];
    let n = get(name, &mut v);
    for (i, x) in v.iter().enumerate().take(n) {
        unsafe { *out.add(i) = *x as f32 };
    }
}

#[no_mangle]
pub unsafe extern "system" fn glGetDoublev(name: u32, out: *mut f64) {
    let mut v = [0f64; 16];
    let n = get(name, &mut v);
    for (i, x) in v.iter().enumerate().take(n) {
        unsafe { *out.add(i) = *x };
    }
}

#[no_mangle]
pub unsafe extern "system" fn glGetBooleanv(name: u32, out: *mut u8) {
    let mut v = [0f64; 16];
    let n = get(name, &mut v);
    for (i, x) in v.iter().enumerate().take(n) {
        unsafe { *out.add(i) = (*x != 0.0) as u8 };
    }
}

/// A state value for glGet*: how many numbers it is.
fn get(name: u32, out: &mut [f64; 16]) -> usize {
    outside(|c| {
        let one = |out: &mut [f64; 16], x: f64| {
            out[0] = x;
            1
        };
        match name {
            0x0BA2 => {
                for i in 0..4 {
                    out[i] = c.viewport[i] as f64;
                }
                4
            }
            0x0C10 => {
                for i in 0..4 {
                    out[i] = c.scissor[i] as f64;
                }
                4
            }
            0x0BA6 | 0x0BA7 | 0x0BA8 => {
                let s = if name == 0x0BA8 { 2 + c.unit } else { (name - 0x0BA6) as usize };
                let m = c.stacks[s].last().unwrap();
                for i in 0..16 {
                    out[i] = m[i] as f64;
                }
                16
            }
            0x0BA0 => one(out, c.matrix_mode as f64),
            0x0D33 => one(out, MAX_TEXTURE_SIZE as f64),
            0x0D36 | 0x0D38 | 0x0D39 => one(out, STACK_DEPTH as f64),
            0x0D52..=0x0D54 => one(out, 8.0),
            0x0D55 => one(out, 0.0),
            0x0D56 => one(out, 24.0),
            0x0D57 => one(out, 8.0),
            0x0B21 => one(out, c.line_width as f64),
            0x0B11 => one(out, c.point_size as f64),
            0x0B32 => one(out, c.lists.base as f64),
            0x0D31 => one(out, 8.0),
            // Multitexture: units, the active and client-active unit.
            0x84E2 => one(out, 2.0),
            0x84E0 => one(out, (0x84C0 + c.unit) as f64),
            0x84E1 => one(out, (0x84C0 + c.client_unit) as f64),
            0x0B31 => one(out, c.attrib_stack.len() as f64),
            0x0BB0 => one(out, c.attrib_stack.len() as f64),
            0x0BB1 => one(out, c.client_stack.len() as f64),
            0x0D35 | 0x0D3B => one(out, 16.0),
            0x0C40 => one(out, c.select.mode as f64),
            0x0B43 => one(out, c.edge_flag as u32 as f64),
            // GL_POLYGON_OFFSET_FACTOR, GL_POLYGON_OFFSET_UNITS
            0x8038 => one(out, c.poly_offset[0] as f64),
            0x2A00 => one(out, c.poly_offset[1] as f64),
            0x0B25 => one(out, c.line_stipple[1] as f64),
            0x0B26 => one(out, c.line_stipple[0] as f64),
            0x0BF0 => one(out, c.logic_op as f64),
            0x0C01 => one(out, if c.draw_none { 0.0 } else if c.draw_front || !c.double { 0x404 as f64 } else { 0x405 as f64 }),
            0x0C02 => one(out, if c.read_front || !c.double { 0x404 as f64 } else { 0x405 as f64 }),
            0x0D3A => {
                out[0] = MAX_TEXTURE_SIZE as f64;
                out[1] = MAX_TEXTURE_SIZE as f64;
                2
            }
            0x8069 => one(out, c.bound[c.unit][0] as f64),
            0x8068 => one(out, c.bound[c.unit][1] as f64),
            0x0C54 => one(out, if c.fog_nicest { 0x1102 as f64 } else { 0x1100 as f64 }),
            0x0C50 => one(out, if c.perspective { 0x1102 as f64 } else { 0x1100 as f64 }),
            0x0C32 => one(out, c.double as u32 as f64),
            0x0B74 => one(out, c.depth_func as f64),
            0x0B72 => one(out, c.depth_mask as u32 as f64),
            0x0B54 => one(out, c.shade_model as f64),
            0x0B45 => one(out, c.cull_mode as f64),
            0x0B46 => one(out, c.front_face as f64),
            GL_UNPACK_ALIGNMENT => one(out, c.unpack.align as f64),
            GL_PACK_ALIGNMENT => one(out, c.pack.align as f64),
            0x0CF0 => one(out, c.unpack.swap_bytes as u32 as f64),
            0x0CF1 => one(out, c.unpack.lsb_first as u32 as f64),
            0x0CF2 => one(out, c.unpack.row_length as f64),
            0x0CF3 => one(out, c.unpack.skip_rows as f64),
            0x0CF4 => one(out, c.unpack.skip_pixels as f64),
            0x0D00 => one(out, c.pack.swap_bytes as u32 as f64),
            0x0D01 => one(out, c.pack.lsb_first as u32 as f64),
            0x0D02 => one(out, c.pack.row_length as f64),
            0x0D03 => one(out, c.pack.skip_rows as f64),
            0x0D04 => one(out, c.pack.skip_pixels as f64),
            0x0D16 => one(out, c.transfer.zoom[0] as f64),
            0x0D17 => one(out, c.transfer.zoom[1] as f64),
            // The raster position, its validity, colour, distance, texture.
            0x0B07 => {
                let r = c.raster.win;
                out[..4].copy_from_slice(&[r[0], r[1], r[2], 1.0]);
                4
            }
            0x0B08 => one(out, c.raster.valid as u32 as f64),
            0x0B04 => {
                out[..4].copy_from_slice(&c.raster.color.map(|x| x as f64));
                4
            }
            0x0B09 => one(out, c.raster.dist as f64),
            0x0B06 => {
                out[..4].copy_from_slice(&c.raster.tc.map(|x| x as f64));
                4
            }
            // Accumulation buffer bits: 16 a channel.
            0x0D58..=0x0D5B => one(out, 16.0),
            0x0B80 => {
                out[..4].copy_from_slice(&c.clear_accum.map(|x| x as f64));
                4
            }
            0x0C22 => {
                out[..4].copy_from_slice(&c.clear_color.map(|x| x as f64));
                4
            }
            0x0B00 => {
                out[..4].copy_from_slice(&c.color.map(|x| x as f64));
                4
            }
            cap if c.on(cap) => one(out, 1.0),
            GL_DEPTH_TEST | GL_CULL_FACE | GL_BLEND | GL_ALPHA_TEST | GL_SCISSOR_TEST | GL_TEXTURE_2D | GL_DITHER => one(out, 0.0),
            _ => {
                c.err(GL_INVALID_ENUM);
                0
            }
        }
    })
}

// -----| matrices |-----

#[no_mangle]
pub extern "system" fn glMatrixMode(m: u32) {
    rec!(glMatrixMode(m));
    outside(|c| if matches!(m, GL_MODELVIEW | GL_PROJECTION | GL_TEXTURE) { c.matrix_mode = m } else { c.err(GL_INVALID_ENUM) })
}

#[no_mangle]
pub extern "system" fn glLoadIdentity() {
    rec!(glLoadIdentity());
    outside(|c| *c.top() = IDENTITY)
}

#[no_mangle]
pub unsafe extern "system" fn glLoadMatrixf(m: *const f32) {
    load_matrix(core::array::from_fn(|i| unsafe { *m.add(i) }), false)
}

fn load_matrix(m: Mat, mult: bool) {
    rec!(load_matrix(m, mult));
    outside(|c| if mult { c.mult(&m) } else { *c.top() = m })
}

#[no_mangle]
pub unsafe extern "system" fn glLoadMatrixd(m: *const f64) {
    load_matrix(core::array::from_fn(|i| unsafe { *m.add(i) } as f32), false)
}

#[no_mangle]
pub unsafe extern "system" fn glMultMatrixf(m: *const f32) {
    load_matrix(core::array::from_fn(|i| unsafe { *m.add(i) }), true)
}

#[no_mangle]
pub unsafe extern "system" fn glMultMatrixd(m: *const f64) {
    load_matrix(core::array::from_fn(|i| unsafe { *m.add(i) } as f32), true)
}

#[no_mangle]
pub extern "system" fn glPushMatrix() {
    rec!(glPushMatrix());
    outside(|c| {
        let i = c.stack();
        if c.stacks[i].len() >= STACK_DEPTH {
            c.err(GL_STACK_OVERFLOW);
        } else {
            let top = *c.stacks[i].last().unwrap();
            c.stacks[i].push(top);
        }
    })
}

#[no_mangle]
pub extern "system" fn glPopMatrix() {
    rec!(glPopMatrix());
    outside(|c| {
        let i = c.stack();
        if c.stacks[i].len() <= 1 {
            c.err(GL_STACK_UNDERFLOW);
        } else {
            c.stacks[i].pop();
        }
    })
}

#[no_mangle]
pub extern "system" fn glTranslatef(x: f32, y: f32, z: f32) {
    rec!(glTranslatef(x, y, z));
    let mut m = IDENTITY;
    m[12] = x;
    m[13] = y;
    m[14] = z;
    outside(|c| c.mult(&m))
}

#[no_mangle]
pub extern "system" fn glTranslated(x: f64, y: f64, z: f64) {
    glTranslatef(x as f32, y as f32, z as f32)
}

#[no_mangle]
pub extern "system" fn glScalef(x: f32, y: f32, z: f32) {
    rec!(glScalef(x, y, z));
    let mut m = IDENTITY;
    m[0] = x;
    m[5] = y;
    m[10] = z;
    outside(|c| c.mult(&m))
}

#[no_mangle]
pub extern "system" fn glScaled(x: f64, y: f64, z: f64) {
    glScalef(x as f32, y as f32, z as f32)
}

#[no_mangle]
pub extern "system" fn glRotatef(angle: f32, x: f32, y: f32, z: f32) {
    rec!(glRotatef(angle, x, y, z));
    let len = sqrt((x * x + y * y + z * z) as f64);
    if len == 0.0 {
        return;
    }
    let (x, y, z) = ((x as f64 / len) as f32, (y as f64 / len) as f32, (z as f64 / len) as f32);
    let (s, co) = sin_cos(angle as f64 * core::f64::consts::PI / 180.0);
    let (s, co) = (s as f32, co as f32);
    let k = 1.0 - co;
    let m: Mat = [
        x * x * k + co, y * x * k + z * s, x * z * k - y * s, 0.0,
        x * y * k - z * s, y * y * k + co, y * z * k + x * s, 0.0,
        x * z * k + y * s, y * z * k - x * s, z * z * k + co, 0.0,
        0.0, 0.0, 0.0, 1.0,
    ];
    outside(|c| c.mult(&m))
}

#[no_mangle]
pub extern "system" fn glRotated(angle: f64, x: f64, y: f64, z: f64) {
    glRotatef(angle as f32, x as f32, y as f32, z as f32)
}

#[no_mangle]
pub extern "system" fn glOrtho(l: f64, r: f64, b: f64, t: f64, n: f64, f: f64) {
    rec!(glOrtho(l, r, b, t, n, f));
    outside(|c| {
        if l == r || b == t || n == f {
            c.err(GL_INVALID_VALUE);
            return;
        }
        let mut m = IDENTITY;
        m[0] = (2.0 / (r - l)) as f32;
        m[5] = (2.0 / (t - b)) as f32;
        m[10] = (-2.0 / (f - n)) as f32;
        m[12] = (-(r + l) / (r - l)) as f32;
        m[13] = (-(t + b) / (t - b)) as f32;
        m[14] = (-(f + n) / (f - n)) as f32;
        c.mult(&m);
    })
}

#[no_mangle]
pub extern "system" fn glFrustum(l: f64, r: f64, b: f64, t: f64, n: f64, f: f64) {
    rec!(glFrustum(l, r, b, t, n, f));
    outside(|c| {
        if n <= 0.0 || f <= 0.0 || l == r || b == t || n == f {
            c.err(GL_INVALID_VALUE);
            return;
        }
        let mut m = [0.0f32; 16];
        m[0] = (2.0 * n / (r - l)) as f32;
        m[5] = (2.0 * n / (t - b)) as f32;
        m[8] = ((r + l) / (r - l)) as f32;
        m[9] = ((t + b) / (t - b)) as f32;
        m[10] = (-(f + n) / (f - n)) as f32;
        m[11] = -1.0;
        m[14] = (-2.0 * f * n / (f - n)) as f32;
        c.mult(&m);
    })
}

// -----| immediate mode |-----

#[no_mangle]
pub extern "system" fn glBegin(mode: u32) {
    rec!(glBegin(mode));
    with(|c| {
        if c.prim.is_some() {
            c.err(GL_INVALID_OPERATION);
        } else if mode > GL_POLYGON {
            c.err(GL_INVALID_ENUM);
        } else {
            c.prim = Some(mode);
            c.verts.clear();
            c.select.line_reset = true;
            c.stipple_count = 0.0;
        }
    })
}

#[no_mangle]
pub extern "system" fn glEnd() {
    rec!(glEnd());
    with(|c| {
        let Some(mode) = c.prim.take() else {
            c.err(GL_INVALID_OPERATION);
            return;
        };
        let v = core::mem::take(&mut c.verts);
        c.assemble(mode, &v);
        c.verts = v;
    })
}

fn vertex(x: f32, y: f32, z: f32, w: f32) {
    rec!(vertex(x, y, z, w));
    with(|c| {
        if c.prim.is_some() {
            c.vertex([x, y, z, w]);
        }
    })
}

#[no_mangle] pub extern "system" fn glVertex2f(x: f32, y: f32) { vertex(x, y, 0.0, 1.0) }
#[no_mangle] pub extern "system" fn glVertex2d(x: f64, y: f64) { vertex(x as f32, y as f32, 0.0, 1.0) }
#[no_mangle] pub extern "system" fn glVertex2i(x: i32, y: i32) { vertex(x as f32, y as f32, 0.0, 1.0) }
#[no_mangle] pub extern "system" fn glVertex2s(x: i16, y: i16) { vertex(x as f32, y as f32, 0.0, 1.0) }
#[no_mangle] pub extern "system" fn glVertex3f(x: f32, y: f32, z: f32) { vertex(x, y, z, 1.0) }
#[no_mangle] pub extern "system" fn glVertex3d(x: f64, y: f64, z: f64) { vertex(x as f32, y as f32, z as f32, 1.0) }
#[no_mangle] pub extern "system" fn glVertex3i(x: i32, y: i32, z: i32) { vertex(x as f32, y as f32, z as f32, 1.0) }
#[no_mangle] pub extern "system" fn glVertex3s(x: i16, y: i16, z: i16) { vertex(x as f32, y as f32, z as f32, 1.0) }
#[no_mangle] pub extern "system" fn glVertex4f(x: f32, y: f32, z: f32, w: f32) { vertex(x, y, z, w) }
#[no_mangle] pub extern "system" fn glVertex4d(x: f64, y: f64, z: f64, w: f64) { vertex(x as f32, y as f32, z as f32, w as f32) }
#[no_mangle] pub unsafe extern "system" fn glVertex2fv(v: *const f32) { unsafe { vertex(*v, *v.add(1), 0.0, 1.0) } }
#[no_mangle] pub unsafe extern "system" fn glVertex2dv(v: *const f64) { unsafe { vertex(*v as f32, *v.add(1) as f32, 0.0, 1.0) } }
#[no_mangle] pub unsafe extern "system" fn glVertex2iv(v: *const i32) { unsafe { vertex(*v as f32, *v.add(1) as f32, 0.0, 1.0) } }
#[no_mangle] pub unsafe extern "system" fn glVertex3fv(v: *const f32) { unsafe { vertex(*v, *v.add(1), *v.add(2), 1.0) } }
#[no_mangle] pub unsafe extern "system" fn glVertex3dv(v: *const f64) { unsafe { vertex(*v as f32, *v.add(1) as f32, *v.add(2) as f32, 1.0) } }
#[no_mangle] pub unsafe extern "system" fn glVertex3iv(v: *const i32) { unsafe { vertex(*v as f32, *v.add(1) as f32, *v.add(2) as f32, 1.0) } }
#[no_mangle] pub unsafe extern "system" fn glVertex4fv(v: *const f32) { unsafe { vertex(*v, *v.add(1), *v.add(2), *v.add(3)) } }

fn color(r: f32, g: f32, b: f32, a: f32) {
    rec!(color(r, g, b, a));
    with(|c| c.color = [r, g, b, a])
}

fn ub(x: u8) -> f32 {
    x as f32 / 255.0
}

#[no_mangle] pub extern "system" fn glColor3f(r: f32, g: f32, b: f32) { color(r, g, b, 1.0) }
#[no_mangle] pub extern "system" fn glColor3d(r: f64, g: f64, b: f64) { color(r as f32, g as f32, b as f32, 1.0) }
#[no_mangle] pub extern "system" fn glColor4f(r: f32, g: f32, b: f32, a: f32) { color(r, g, b, a) }
#[no_mangle] pub extern "system" fn glColor4d(r: f64, g: f64, b: f64, a: f64) { color(r as f32, g as f32, b as f32, a as f32) }
#[no_mangle] pub extern "system" fn glColor3ub(r: u8, g: u8, b: u8) { color(ub(r), ub(g), ub(b), 1.0) }
#[no_mangle] pub extern "system" fn glColor4ub(r: u8, g: u8, b: u8, a: u8) { color(ub(r), ub(g), ub(b), ub(a)) }
#[no_mangle] pub unsafe extern "system" fn glColor3fv(v: *const f32) { unsafe { color(*v, *v.add(1), *v.add(2), 1.0) } }
#[no_mangle] pub unsafe extern "system" fn glColor4fv(v: *const f32) { unsafe { color(*v, *v.add(1), *v.add(2), *v.add(3)) } }
#[no_mangle] pub unsafe extern "system" fn glColor3ubv(v: *const u8) { unsafe { color(ub(*v), ub(*v.add(1)), ub(*v.add(2)), 1.0) } }
#[no_mangle] pub unsafe extern "system" fn glColor4ubv(v: *const u8) { unsafe { color(ub(*v), ub(*v.add(1)), ub(*v.add(2)), ub(*v.add(3))) } }

fn texcoord(s: f32, t: f32, r: f32, q: f32) {
    rec!(texcoord(s, t, r, q));
    with(|c| c.tc[c.unit] = [s, t, r, q])
}

#[no_mangle] pub extern "system" fn glTexCoord1f(s: f32) { texcoord(s, 0.0, 0.0, 1.0) }
#[no_mangle] pub extern "system" fn glTexCoord2f(s: f32, t: f32) { texcoord(s, t, 0.0, 1.0) }
#[no_mangle] pub extern "system" fn glTexCoord2d(s: f64, t: f64) { texcoord(s as f32, t as f32, 0.0, 1.0) }
#[no_mangle] pub extern "system" fn glTexCoord2i(s: i32, t: i32) { texcoord(s as f32, t as f32, 0.0, 1.0) }
#[no_mangle] pub unsafe extern "system" fn glTexCoord2fv(v: *const f32) { unsafe { texcoord(*v, *v.add(1), 0.0, 1.0) } }
#[no_mangle] pub extern "system" fn glTexCoord4f(s: f32, t: f32, r: f32, q: f32) { texcoord(s, t, r, q) }

#[no_mangle]
pub extern "system" fn glNormal3f(x: f32, y: f32, z: f32) {
    rec!(glNormal3f(x, y, z));
    with(|c| c.normal = [x, y, z])
}

#[no_mangle]
pub unsafe extern "system" fn glNormal3fv(v: *const f32) {
    unsafe { glNormal3f(*v, *v.add(1), *v.add(2)) }
}

#[no_mangle]
pub extern "system" fn glNormal3d(x: f64, y: f64, z: f64) {
    glNormal3f(x as f32, y as f32, z as f32)
}

/// glRect*: a polygon in the z = 0 plane.
fn rect(x1: f32, y1: f32, x2: f32, y2: f32) {
    rec!(rect(x1, y1, x2, y2));
    outside(|c| {
        c.verts.clear();
        for (x, y) in [(x1, y1), (x2, y1), (x2, y2), (x1, y2)] {
            c.vertex([x, y, 0.0, 1.0]);
        }
        let v = core::mem::take(&mut c.verts);
        c.assemble(GL_POLYGON, &v);
        c.verts = v;
    })
}

#[no_mangle] pub extern "system" fn glRectf(x1: f32, y1: f32, x2: f32, y2: f32) { rect(x1, y1, x2, y2) }
#[no_mangle] pub extern "system" fn glRectd(x1: f64, y1: f64, x2: f64, y2: f64) { rect(x1 as f32, y1 as f32, x2 as f32, y2 as f32) }
#[no_mangle] pub extern "system" fn glRecti(x1: i32, y1: i32, x2: i32, y2: i32) { rect(x1 as f32, y1 as f32, x2 as f32, y2 as f32) }

// -----| vertex arrays |-----

fn client_state(a: u32, on: bool) {
    with(|c| {
        let i = match a {
            GL_VERTEX_ARRAY => A_VERTEX,
            GL_COLOR_ARRAY => A_COLOR,
            GL_TEXTURE_COORD_ARRAY => if c.client_unit == 1 { A_TEXCOORD2 } else { A_TEXCOORD },
            GL_NORMAL_ARRAY => A_NORMAL,
            _ => {
                // Index and edge flag arrays: accepted, never used.
                return;
            }
        };
        c.arrays[i].on = on;
    })
}

#[no_mangle]
pub extern "system" fn glEnableClientState(a: u32) {
    client_state(a, true)
}

#[no_mangle]
pub extern "system" fn glDisableClientState(a: u32) {
    client_state(a, false)
}

fn pointer(i: usize, size: i32, kind: u32, stride: i32, ptr: usize) {
    with(|c| {
        if stride < 0 || !(1..=4).contains(&size) {
            c.err(GL_INVALID_VALUE);
            return;
        }
        let a = &mut c.arrays[i];
        a.size = size;
        a.kind = kind;
        a.stride = stride;
        a.ptr = ptr;
    })
}

#[no_mangle] pub extern "system" fn glVertexPointer(size: i32, kind: u32, stride: i32, p: usize) { pointer(A_VERTEX, size, kind, stride, p) }
#[no_mangle] pub extern "system" fn glColorPointer(size: i32, kind: u32, stride: i32, p: usize) { pointer(A_COLOR, size, kind, stride, p) }
#[no_mangle] pub extern "system" fn glTexCoordPointer(size: i32, kind: u32, stride: i32, p: usize) {
    let i = if with(|c| c.client_unit) == 1 { A_TEXCOORD2 } else { A_TEXCOORD };
    pointer(i, size, kind, stride, p)
}
#[no_mangle] pub extern "system" fn glNormalPointer(kind: u32, stride: i32, p: usize) { pointer(A_NORMAL, 3, kind, stride, p) }

impl Ctx {
    /// Array element `i`: its attributes become current, then its vertex.
    fn element(&mut self, i: usize) {
        let a = self.arrays;
        unsafe {
            if a[A_COLOR].on {
                let mut col = a[A_COLOR].get(i, true);
                if a[A_COLOR].size == 3 {
                    col[3] = 1.0;
                }
                self.color = col;
            }
            if a[A_TEXCOORD].on {
                self.tc[0] = a[A_TEXCOORD].get(i, false);
            }
            if a[A_TEXCOORD2].on {
                self.tc[1] = a[A_TEXCOORD2].get(i, false);
            }
            if a[A_NORMAL].on {
                let n = a[A_NORMAL].get(i, true);
                self.normal = [n[0], n[1], n[2]];
            }
            if a[A_VERTEX].on {
                let mut p = a[A_VERTEX].get(i, false);
                if a[A_VERTEX].size < 3 {
                    p[2] = 0.0;
                }
                if a[A_VERTEX].size < 4 {
                    p[3] = 1.0;
                }
                self.vertex(p);
            }
        }
    }

    fn draw_elements(&mut self, mode: u32, idx: impl Iterator<Item = usize>) {
        if mode > GL_POLYGON {
            self.err(GL_INVALID_ENUM);
            return;
        }
        if !self.arrays[A_VERTEX].on {
            return;
        }
        if self.lists.compiling.is_some() && self.lists.replaying == 0 {
            // A list keeps what the arrays hold now.
            let a = self.arrays;
            let els: Vec<Element> = idx.map(|i| unsafe {
                Element {
                    color: a[A_COLOR].on.then(|| {
                        let mut c = a[A_COLOR].get(i, true);
                        if a[A_COLOR].size == 3 {
                            c[3] = 1.0;
                        }
                        c
                    }),
                    tc: [a[A_TEXCOORD].on.then(|| a[A_TEXCOORD].get(i, false)), a[A_TEXCOORD2].on.then(|| a[A_TEXCOORD2].get(i, false))],
                    normal: a[A_NORMAL].on.then(|| a[A_NORMAL].get(i, true)),
                    pos: {
                        let mut p = a[A_VERTEX].get(i, false);
                        if a[A_VERTEX].size < 3 {
                            p[2] = 0.0;
                        }
                        if a[A_VERTEX].size < 4 {
                            p[3] = 1.0;
                        }
                        p
                    },
                }
            }).collect();
            let _ = lists::record(alloc::boxed::Box::new(move || draw_recorded(mode, &els)));
            return;
        }
        let saved = core::mem::take(&mut self.verts);
        for i in idx {
            self.element(i);
        }
        let v = core::mem::replace(&mut self.verts, saved);
        self.assemble(mode, &v);
    }
}

/// One vertex from the arrays, as a display list keeps it.
#[derive(Clone, Copy)]
struct Element {
    color: Option<[f32; 4]>,
    tc: [Option<[f32; 4]>; 2],
    normal: Option<[f32; 4]>,
    pos: [f32; 4],
}

fn draw_recorded(mode: u32, els: &[Element]) {
    outside(|c| {
        let saved = core::mem::take(&mut c.verts);
        for e in els {
            if let Some(col) = e.color {
                c.color = col;
            }
            for u in 0..2 {
                if let Some(t) = e.tc[u] {
                    c.tc[u] = t;
                }
            }
            if let Some(n) = e.normal {
                c.normal = [n[0], n[1], n[2]];
            }
            c.vertex(e.pos);
        }
        let v = core::mem::replace(&mut c.verts, saved);
        c.assemble(mode, &v);
    })
}

#[no_mangle]
pub extern "system" fn glArrayElement(i: i32) {
    with(|c| {
        if c.prim.is_some() {
            c.element(i as usize);
        }
    })
}

#[no_mangle]
pub extern "system" fn glDrawArrays(mode: u32, first: i32, count: i32) {
    outside(|c| {
        if count < 0 {
            c.err(GL_INVALID_VALUE);
            return;
        }
        c.draw_elements(mode, (first.max(0) as usize)..(first.max(0) as usize + count as usize));
    })
}

#[no_mangle]
pub unsafe extern "system" fn glDrawElements(mode: u32, count: i32, kind: u32, indices: usize) {
    outside(|c| {
        if count < 0 {
            c.err(GL_INVALID_VALUE);
            return;
        }
        let n = count as usize;
        match kind {
            GL_UNSIGNED_BYTE => c.draw_elements(mode, (0..n).map(|i| unsafe { *(indices as *const u8).add(i) } as usize)),
            GL_UNSIGNED_SHORT => c.draw_elements(mode, (0..n).map(|i| unsafe { core::ptr::read_unaligned((indices as *const u16).add(i)) } as usize)),
            GL_UNSIGNED_INT => c.draw_elements(mode, (0..n).map(|i| unsafe { core::ptr::read_unaligned((indices as *const u32).add(i)) } as usize)),
            _ => c.err(GL_INVALID_ENUM),
        }
    })
}

// -----| textures |-----

#[no_mangle]
pub unsafe extern "system" fn glGenTextures(n: i32, out: *mut u32) {
    outside(|c| {
        if n < 0 {
            c.err(GL_INVALID_VALUE);
            return;
        }
        for i in 0..n as usize {
            while c.textures.iter().any(|t| t.0 == c.next_name) {
                c.next_name += 1;
            }
            let name = c.next_name;
            c.next_name += 1;
            c.textures.push((name, Tex::new()));
            unsafe { *out.add(i) = name };
        }
    })
}

#[no_mangle]
pub unsafe extern "system" fn glDeleteTextures(n: i32, names: *const u32) {
    outside(|c| {
        c.flush();
        for i in 0..n.max(0) as usize {
            let name = unsafe { *names.add(i) };
            if name == 0 {
                continue;
            }
            if let Some(p) = c.textures.iter().position(|t| t.0 == name) {
                let (_, t) = c.textures.remove(p);
                if t.handle != 0 {
                    unsafe { NtClose(t.handle) };
                }
            }
            for u in c.bound.iter_mut() {
                for b in u.iter_mut() {
                    if *b == name {
                        *b = 0;
                    }
                }
            }
        }
    })
}

#[no_mangle]
pub extern "system" fn glIsTexture(name: u32) -> u8 {
    with(|c| (name != 0 && c.tex(name).is_some()) as u8)
}

#[no_mangle]
pub extern "system" fn glBindTexture(target: u32, name: u32) {
    rec!(glBindTexture(target, name));
    outside(|c| {
        let dim = match target {
            GL_TEXTURE_2D => 0,
            GL_TEXTURE_1D => 1,
            _ => {
                c.err(GL_INVALID_ENUM);
                return;
            }
        };
        c.flush();
        let u = c.unit;
        c.bound[u][dim] = name;
        c.bound_of(u, dim);
    })
}

/// What the internal format keeps of a texel.
fn internal(fmt: i32, p: u32) -> u32 {
    let (a, r) = (p >> 24, p >> 16 & 0xFF);
    match fmt as u32 {
        1 | GL_LUMINANCE | 0x8040 => 0xFF00_0000 | r * 0x01_0101,
        2 | GL_LUMINANCE_ALPHA | 0x8045 => a << 24 | r * 0x01_0101,
        GL_ALPHA | 0x803C => a << 24 | 0xFF_FFFF,
        3 | GL_RGB | 0x8051 | 0x804F | 0x8050 | 0x8052 | 0x8053 | 0x8054 => p | 0xFF00_0000,
        _ => p,
    }
}

fn valid_internal(fmt: i32) -> bool {
    matches!(fmt as u32, 1..=4 | GL_ALPHA | GL_RGB | GL_RGBA | GL_LUMINANCE | GL_LUMINANCE_ALPHA | 0x803B..=0x805B)
}

#[no_mangle]
pub unsafe extern "system" fn glTexImage2D(target: u32, level: i32, ifmt: i32, w: i32, h: i32, border: i32, format: u32, kind: u32, pixels: *const u8) {
    outside(|c| {
        if target != GL_TEXTURE_2D {
            c.err(GL_INVALID_ENUM);
            return;
        }
        let pow2 = |n: i32| n > 0 && n & (n - 1) == 0;
        if level < 0 || border != 0 || !pow2(w) || !pow2(h) || w > MAX_TEXTURE_SIZE || h > MAX_TEXTURE_SIZE || !valid_internal(ifmt) {
            c.err(GL_INVALID_VALUE);
            return;
        }
        let texels = if pixels.is_null() {
            Some(vec![0u32; (w * h) as usize])
        } else {
            unsafe { unpack(c, w, h, format, kind, pixels) }
        };
        let Some(texels) = texels else {
            c.err(GL_INVALID_ENUM);
            return;
        };
        let texels: Vec<u32> = texels.into_iter().map(|p| internal(ifmt, p)).collect();
        if c.lists.compiling.is_some() && c.lists.replaying == 0 {
            let _ = lists::record(alloc::boxed::Box::new(move || tex_image(0, level, w, h, texels.clone())));
            return;
        }
        tex_image_in(c, 0, level, w, h, texels);
    })
}

/// A level's texels, already converted (from a display list).
fn tex_image(dim: usize, level: i32, w: i32, h: i32, texels: Vec<u32>) {
    outside(|c| tex_image_in(c, dim, level, w, h, texels))
}

/// The texture dimension a target names: 0 for GL_TEXTURE_2D, 1 for
/// GL_TEXTURE_1D.
fn dim_of(target: u32) -> Option<usize> {
    match target {
        GL_TEXTURE_2D => Some(0),
        GL_TEXTURE_1D => Some(1),
        _ => None,
    }
}

fn tex_image_in(c: &mut Ctx, dim: usize, level: i32, w: i32, h: i32, texels: Vec<u32>) {
    c.flush();
    let u = c.unit;
    let t = c.bound_of(u, dim);
    if level == 0 {
        t.w = w as u32;
        t.h = h as u32;
        t.mips.clear();
    }
    let l = level as usize;
    if l < 16 {
        if t.mips.len() <= l {
            t.mips.resize(l + 1, Vec::new());
        }
        // A level of the wrong size for level 0 leaves the texture
        // incomplete, as GL has it.
        t.mips[l] = if t.level_size(l) == (w as u32, h as u32) { texels } else { Vec::new() };
    }
    t.dirty = true;
}

#[no_mangle]
pub unsafe extern "system" fn glTexSubImage2D(target: u32, level: i32, x: i32, y: i32, w: i32, h: i32, format: u32, kind: u32, pixels: *const u8) {
    outside(|c| {
        if target != GL_TEXTURE_2D {
            c.err(GL_INVALID_ENUM);
            return;
        }
        let Some(src) = (unsafe { unpack(c, w.max(0), h.max(0), format, kind, pixels) }) else {
            c.err(GL_INVALID_ENUM);
            return;
        };
        if c.lists.compiling.is_some() && c.lists.replaying == 0 {
            let _ = lists::record(alloc::boxed::Box::new(move || {
                let src = src.clone();
                outside(|c| sub_image(c, 0, level, x, y, w, h, format, &src))
            }));
            return;
        }
        sub_image(c, 0, level, x, y, w, h, format, &src);
    })
}

#[allow(clippy::too_many_arguments)]
fn sub_image(c: &mut Ctx, dim: usize, level: i32, x: i32, y: i32, w: i32, h: i32, format: u32, src: &[u32]) {
    {
        c.flush();
        let u = c.unit;
        let t = c.bound_of(u, dim);
        let l = level.max(0) as usize;
        let (lw, lh) = t.level_size(l);
        if t.mips.get(l).is_none_or(|m| m.len() != (lw * lh) as usize) {
            c.err(GL_INVALID_OPERATION);
            return;
        }
        if level < 0 || x < 0 || y < 0 || x + w > lw as i32 || y + h > lh as i32 {
            c.err(GL_INVALID_VALUE);
            return;
        }
        let t = c.bound_of(u, dim);
        let m = &mut t.mips[l];
        for row in 0..h as usize {
            for col in 0..w as usize {
                let p = src[row * w as usize + col];
                let at = (y as usize + row) * lw as usize + x as usize + col;
                // Formats without alpha keep it opaque, as the image has it.
                m[at] = if m[at] >> 24 == 0xFF && format != GL_RGBA && format != GL_BGRA_EXT { p | 0xFF00_0000 } else { p };
            }
        }
        t.dirty = true;
    }
}

fn tex_param(target: u32, name: u32, v: u32) {
    rec!(tex_param(target, name, v));
    outside(|c| {
        let Some(dim) = dim_of(target) else {
            c.err(GL_INVALID_ENUM);
            return;
        };
        // GL_CLAMP_TO_EDGE (EXT/SGIS_texture_edge_clamp) is how GL_CLAMP
        // is drawn here.
        let v = if v == 0x812F { GL_CLAMP } else { v };
        let ok = match name {
            GL_TEXTURE_MIN_FILTER => (GL_NEAREST..=GL_LINEAR).contains(&v) || (0x2700..=GL_LINEAR_MIPMAP_LINEAR).contains(&v),
            GL_TEXTURE_MAG_FILTER => v == GL_NEAREST || v == GL_LINEAR,
            GL_TEXTURE_WRAP_S | GL_TEXTURE_WRAP_T => v == GL_CLAMP || v == GL_REPEAT,
            // The border colour and priority: kept nowhere yet.
            _ => return,
        };
        if !ok {
            c.err(GL_INVALID_ENUM);
            return;
        }
        c.flush();
        let u = c.unit;
        let t = c.bound_of(u, dim);
        match name {
            GL_TEXTURE_MIN_FILTER => t.min = v,
            GL_TEXTURE_MAG_FILTER => t.mag = v,
            GL_TEXTURE_WRAP_S => t.wrap_s = v,
            _ => t.wrap_t = v,
        }
        t.dirty = true;
    })
}

#[no_mangle] pub extern "system" fn glTexParameteri(t: u32, n: u32, v: i32) { tex_param(t, n, v as u32) }
#[no_mangle] pub extern "system" fn glTexParameterf(t: u32, n: u32, v: f32) { tex_param(t, n, v as u32) }
#[no_mangle] pub unsafe extern "system" fn glTexParameteriv(t: u32, n: u32, v: *const i32) { tex_param(t, n, unsafe { *v } as u32) }
#[no_mangle] pub unsafe extern "system" fn glTexParameterfv(t: u32, n: u32, v: *const f32) { tex_param(t, n, unsafe { *v } as u32) }

fn tex_env(target: u32, name: u32, v: u32) {
    rec!(tex_env(target, name, v));
    outside(|c| {
        if target != GL_TEXTURE_ENV {
            c.err(GL_INVALID_ENUM);
            return;
        }
        if name == GL_TEXTURE_ENV_MODE {
            if matches!(v, GL_MODULATE | GL_DECAL | GL_REPLACE | GL_BLEND | GL_ADD) {
                c.tex_env[c.unit] = v;
            } else {
                c.err(GL_INVALID_ENUM);
            }
        }
    })
}

#[no_mangle] pub extern "system" fn glTexEnvi(t: u32, n: u32, v: i32) { tex_env(t, n, v as u32) }
#[no_mangle] pub extern "system" fn glTexEnvf(t: u32, n: u32, v: f32) { tex_env(t, n, v as u32) }
#[no_mangle]
pub unsafe extern "system" fn glTexEnviv(t: u32, n: u32, v: *const i32) {
    if n == GL_TEXTURE_ENV_COLOR {
        let c: [f32; 4] = core::array::from_fn(|i| (unsafe { *v.add(i) } as f64 / 2147483647.0) as f32);
        return env_color(t, c);
    }
    tex_env(t, n, unsafe { *v } as u32)
}

#[no_mangle]
pub unsafe extern "system" fn glTexEnvfv(t: u32, n: u32, v: *const f32) {
    if n == GL_TEXTURE_ENV_COLOR {
        return env_color(t, core::array::from_fn(|i| unsafe { *v.add(i) }));
    }
    tex_env(t, n, unsafe { *v } as u32)
}

fn env_color(target: u32, c: [f32; 4]) {
    rec!(env_color(target, c));
    outside(|ctx| if target == GL_TEXTURE_ENV { ctx.env_color[ctx.unit] = c.map(clamp01) } else { ctx.err(GL_INVALID_ENUM) })
}

// -----| lighting and materials |-----

/// glLight*: `v` holds as many values as the parameter takes.
fn light(n: u32, name: u32, v: [f32; 4]) {
    rec!(light(n, name, v));
    outside(|c| {
        if !(GL_LIGHT0..GL_LIGHT0 + 8).contains(&n) {
            c.err(GL_INVALID_ENUM);
            return;
        }
        let mv = *c.stacks[0].last().unwrap();
        let l = &mut c.light.lights[(n - GL_LIGHT0) as usize];
        match name {
            GL_AMBIENT => l.ambient = v,
            GL_DIFFUSE => l.diffuse = v,
            GL_SPECULAR => l.specular = v,
            // Kept in eye coordinates, transformed by the modelview now.
            GL_POSITION => l.position = xform(&mv, v),
            GL_SPOT_DIRECTION => {
                let d = xform(&mv, [v[0], v[1], v[2], 0.0]);
                l.spot_dir = [d[0], d[1], d[2]];
            }
            GL_SPOT_EXPONENT if (0.0..=128.0).contains(&v[0]) => l.spot_exp = v[0],
            GL_SPOT_CUTOFF if (0.0..=90.0).contains(&v[0]) || v[0] == 180.0 => l.spot_cutoff = v[0],
            GL_CONSTANT_ATTENUATION if v[0] >= 0.0 => l.atten[0] = v[0],
            GL_LINEAR_ATTENUATION if v[0] >= 0.0 => l.atten[1] = v[0],
            GL_QUADRATIC_ATTENUATION if v[0] >= 0.0 => l.atten[2] = v[0],
            GL_SPOT_EXPONENT | GL_SPOT_CUTOFF | GL_CONSTANT_ATTENUATION | GL_LINEAR_ATTENUATION | GL_QUADRATIC_ATTENUATION => c.err(GL_INVALID_VALUE),
            _ => c.err(GL_INVALID_ENUM),
        }
    })
}

/// How many values a light, material or light-model parameter takes.
fn count(name: u32) -> usize {
    match name {
        GL_AMBIENT | GL_DIFFUSE | GL_SPECULAR | GL_POSITION | GL_EMISSION | GL_AMBIENT_AND_DIFFUSE | GL_LIGHT_MODEL_AMBIENT | GL_FOG_COLOR => 4,
        GL_SPOT_DIRECTION => 3,
        _ => 1,
    }
}

unsafe fn floats<T: Copy + Into<f64>>(name: u32, v: *const T, ints_are_colors: bool) -> [f32; 4] {
    let mut out = [0.0f32; 4];
    for (i, o) in out.iter_mut().enumerate().take(count(name)) {
        let x: f64 = unsafe { *v.add(i) }.into();
        // Integer colours map their range onto -1 ..= 1.
        *o = if ints_are_colors && count(name) == 4 && name != GL_POSITION { (x / 2147483647.0) as f32 } else { x as f32 };
    }
    out
}

#[no_mangle] pub extern "system" fn glLightf(n: u32, name: u32, v: f32) { light(n, name, [v, 0.0, 0.0, 0.0]) }
#[no_mangle] pub extern "system" fn glLighti(n: u32, name: u32, v: i32) { light(n, name, [v as f32, 0.0, 0.0, 0.0]) }
#[no_mangle] pub unsafe extern "system" fn glLightfv(n: u32, name: u32, v: *const f32) { light(n, name, unsafe { floats(name, v, false) }) }
#[no_mangle] pub unsafe extern "system" fn glLightiv(n: u32, name: u32, v: *const i32) { light(n, name, unsafe { floats(name, v, true) }) }

fn light_model(name: u32, v: [f32; 4]) {
    rec!(light_model(name, v));
    outside(|c| match name {
        GL_LIGHT_MODEL_AMBIENT => c.light.model_ambient = v,
        GL_LIGHT_MODEL_LOCAL_VIEWER => c.light.local_viewer = v[0] != 0.0,
        GL_LIGHT_MODEL_TWO_SIDE => c.light.two_side = v[0] != 0.0,
        _ => c.err(GL_INVALID_ENUM),
    })
}

#[no_mangle] pub extern "system" fn glLightModelf(name: u32, v: f32) { light_model(name, [v, 0.0, 0.0, 0.0]) }
#[no_mangle] pub extern "system" fn glLightModeli(name: u32, v: i32) { light_model(name, [v as f32, 0.0, 0.0, 0.0]) }
#[no_mangle] pub unsafe extern "system" fn glLightModelfv(name: u32, v: *const f32) { light_model(name, unsafe { floats(name, v, false) }) }
#[no_mangle] pub unsafe extern "system" fn glLightModeliv(name: u32, v: *const i32) { light_model(name, unsafe { floats(name, v, true) }) }

/// glMaterial*. (Allowed between glBegin and glEnd.)
fn material(face: u32, name: u32, v: [f32; 4]) {
    rec!(material(face, name, v));
    with(|c| {
        let faces: &[usize] = match face {
            GL_FRONT => &[0],
            GL_BACK => &[1],
            GL_FRONT_AND_BACK => &[0, 1],
            _ => {
                c.err(GL_INVALID_ENUM);
                return;
            }
        };
        for &f in faces {
            let m = &mut c.light.mat[f];
            match name {
                GL_AMBIENT => m.ambient = v,
                GL_DIFFUSE => m.diffuse = v,
                GL_SPECULAR => m.specular = v,
                GL_EMISSION => m.emission = v,
                GL_SHININESS if (0.0..=128.0).contains(&v[0]) => m.shininess = v[0],
                GL_SHININESS => c.err(GL_INVALID_VALUE),
                GL_AMBIENT_AND_DIFFUSE => {
                    m.ambient = v;
                    m.diffuse = v;
                }
                _ => c.err(GL_INVALID_ENUM),
            }
        }
    })
}

#[no_mangle] pub extern "system" fn glMaterialf(face: u32, name: u32, v: f32) { material(face, name, [v, 0.0, 0.0, 0.0]) }
#[no_mangle] pub extern "system" fn glMateriali(face: u32, name: u32, v: i32) { material(face, name, [v as f32, 0.0, 0.0, 0.0]) }
#[no_mangle] pub unsafe extern "system" fn glMaterialfv(face: u32, name: u32, v: *const f32) { material(face, name, unsafe { floats(name, v, false) }) }
#[no_mangle] pub unsafe extern "system" fn glMaterialiv(face: u32, name: u32, v: *const i32) { material(face, name, unsafe { floats(name, v, true) }) }

#[no_mangle]
pub extern "system" fn glColorMaterial(face: u32, mode: u32) {
    rec!(glColorMaterial(face, mode));
    outside(|c| {
        if !matches!(face, GL_FRONT | GL_BACK | GL_FRONT_AND_BACK) || !matches!(mode, GL_AMBIENT | GL_DIFFUSE | GL_SPECULAR | GL_EMISSION | GL_AMBIENT_AND_DIFFUSE) {
            c.err(GL_INVALID_ENUM);
        } else {
            c.light.cm_face = face;
            c.light.cm_mode = mode;
        }
    })
}

// -----| fog |-----

fn fog(name: u32, v: [f32; 4]) {
    rec!(fog(name, v));
    outside(|c| match name {
        GL_FOG_MODE => {
            let m = v[0] as u32;
            if matches!(m, GL_EXP | GL_EXP2 | GL_LINEAR_FOG) {
                c.fog.mode = m;
            } else {
                c.err(GL_INVALID_ENUM);
            }
        }
        GL_FOG_DENSITY if v[0] >= 0.0 => c.fog.density = v[0],
        GL_FOG_DENSITY => c.err(GL_INVALID_VALUE),
        GL_FOG_START => c.fog.start = v[0],
        GL_FOG_END => c.fog.end = v[0],
        GL_FOG_COLOR => c.fog.color = v.map(clamp01),
        // GL_FOG_INDEX: colour index mode isn't kept.
        0x0B61 => {}
        _ => c.err(GL_INVALID_ENUM),
    })
}

#[no_mangle] pub extern "system" fn glFogf(name: u32, v: f32) { fog(name, [v, 0.0, 0.0, 0.0]) }
#[no_mangle] pub extern "system" fn glFogi(name: u32, v: i32) { fog(name, [v as f32, 0.0, 0.0, 0.0]) }
#[no_mangle] pub unsafe extern "system" fn glFogfv(name: u32, v: *const f32) { fog(name, unsafe { floats(name, v, false) }) }
#[no_mangle] pub unsafe extern "system" fn glFogiv(name: u32, v: *const i32) { fog(name, unsafe { floats(name, v, true) }) }

// -----| lines, points, polygon modes |-----

#[no_mangle]
pub extern "system" fn glLineWidth(w: f32) {
    rec!(glLineWidth(w));
    outside(|c| if w > 0.0 { c.line_width = w.min(10.0) } else { c.err(GL_INVALID_VALUE) })
}

#[no_mangle]
pub extern "system" fn glPointSize(s: f32) {
    rec!(glPointSize(s));
    outside(|c| if s > 0.0 { c.point_size = s.min(10.0) } else { c.err(GL_INVALID_VALUE) })
}

#[no_mangle]
pub extern "system" fn glPolygonMode(face: u32, mode: u32) {
    rec!(glPolygonMode(face, mode));
    outside(|c| {
        if !matches!(mode, GL_POINT | GL_LINE | GL_FILL) {
            c.err(GL_INVALID_ENUM);
            return;
        }
        match face {
            GL_FRONT => c.polygon_mode[0] = mode,
            GL_BACK => c.polygon_mode[1] = mode,
            GL_FRONT_AND_BACK => c.polygon_mode = [mode, mode],
            _ => c.err(GL_INVALID_ENUM),
        }
    })
}

// -----| the stencil buffer |-----

#[no_mangle]
pub extern "system" fn glStencilFunc(func: u32, r: i32, mask: u32) {
    rec!(glStencilFunc(func, r, mask));
    outside(|c| {
        if is_func(func) {
            c.stencil[0] = func;
            c.stencil[1] = r.clamp(0, 255) as u32;
            c.stencil[2] = mask;
        } else {
            c.err(GL_INVALID_ENUM);
        }
    })
}

#[no_mangle]
pub extern "system" fn glStencilOp(fail: u32, zfail: u32, zpass: u32) {
    rec!(glStencilOp(fail, zfail, zpass));
    let ok = |o: u32| matches!(o, GL_KEEP | GL_ZERO | GL_REPLACE | GL_INCR | GL_DECR | GL_INVERT);
    outside(|c| {
        if ok(fail) && ok(zfail) && ok(zpass) {
            c.stencil[4] = fail;
            c.stencil[5] = zfail;
            c.stencil[6] = zpass;
        } else {
            c.err(GL_INVALID_ENUM);
        }
    })
}

#[no_mangle]
pub extern "system" fn glStencilMask(mask: u32) {
    rec!(glStencilMask(mask));
    outside(|c| c.stencil[3] = mask)
}

#[no_mangle]
pub extern "system" fn glClearStencil(s: i32) {
    rec!(glClearStencil(s));
    outside(|c| c.clear_stencil = s as u32)
}

