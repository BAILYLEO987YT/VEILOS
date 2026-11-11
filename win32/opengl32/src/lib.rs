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
//!   lib.rs   contexts, wgl*, state, the vertex stage and the gl* exports
//!
//! Not here yet (later G5 slices): lighting, fog, points and lines, stencil,
//! display lists, mipmaps beyond level 0, accumulation.

#![no_std]
#![allow(non_snake_case, clippy::missing_safety_doc)]

extern crate alloc;

#[path = "../../dllrt.rs"]
mod dllrt;

use alloc::boxed::Box;
use alloc::vec;
use alloc::vec::Vec;
use dllrt::Lock;

type Bool = i32;

#[cfg(feature = "host")]
mod host;
#[cfg(feature = "host")]
use host::{ChoosePixelFormat, DescribePixelFormat, GetPixelFormat, NtClose, SetPixelFormat, VeilGdiDcSize, VeilGdiSetSwapHook, VeilGpu3d};

#[cfg(not(feature = "host"))]
#[link(name = "ntdll", kind = "raw-dylib")]
unsafe extern "system" {
    fn VeilGpu3d(op: u64, a: u64, b: u64, c: u64) -> i32;
    fn NtClose(h: u64) -> i32;
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

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct KVert {
    x: i32,
    y: i32,
    z: i64,
    q: i64,
    uq: i64,
    vq: i64,
    r: i64,
    g: i64,
    b: i64,
    a: i64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct KTri {
    v: [KVert; 3],
}

#[repr(C)]
#[derive(Clone, Copy, PartialEq)]
struct KGroup {
    first: u32,
    count: u32,
    flags: u32,
    depth_func: u32,
    alpha_func: u32,
    alpha_ref: u32,
    src_blend: u32,
    dst_blend: u32,
    tex_env: u32,
    color_mask: u32,
    texture: u64,
    scissor: [i32; 4],
}

#[repr(C)]
struct DrawArgs {
    groups: u64,
    group_count: u32,
    tri_count: u32,
    tris: u64,
}

#[repr(C)]
struct ClearArgs {
    mask: u32,
    color: u32,
    depth: u32,
    color_mask: u32,
    scissor: [i32; 4],
}

#[repr(C)]
struct ReadArgs {
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    dst: u64,
    stride: i64,
}

#[repr(C)]
struct TexArgs {
    w: u32,
    h: u32,
    texels: u64,
    min_filter: u32,
    mag_filter: u32,
    wrap_s: u32,
    wrap_t: u32,
}

const F_DEPTH_TEST: u32 = 1;
const F_DEPTH_WRITE: u32 = 2;
const F_BLEND: u32 = 4;
const F_TEXTURE: u32 = 8;
const F_ALPHA_TEST: u32 = 16;
const DEPTH_MAX: f64 = 16777215.0;
/// Subpixel steps per pixel (raster3d::SUB).
const SUB: f64 = 256.0;
const FX: f64 = 4294967296.0;

// -----| GL constants |-----

const GL_NO_ERROR: u32 = 0;
const GL_INVALID_ENUM: u32 = 0x500;
const GL_INVALID_VALUE: u32 = 0x501;
const GL_INVALID_OPERATION: u32 = 0x502;
const GL_STACK_OVERFLOW: u32 = 0x503;
const GL_STACK_UNDERFLOW: u32 = 0x504;


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

const GL_FRONT: u32 = 0x404;
const GL_BACK: u32 = 0x405;
const GL_FRONT_AND_BACK: u32 = 0x408;
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
const GL_NEAREST_MIPMAP_LINEAR: u32 = 0x2702;
const GL_LINEAR_MIPMAP_LINEAR: u32 = 0x2703;
const GL_CLAMP: u32 = 0x2900;
const GL_REPEAT: u32 = 0x2901;
const GL_TEXTURE_ENV: u32 = 0x2300;
const GL_TEXTURE_ENV_MODE: u32 = 0x2200;
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

fn sqrt(x: f64) -> f64 {
    use core::arch::x86_64::*;
    unsafe { _mm_cvtsd_f64(_mm_sqrt_sd(_mm_set_sd(x), _mm_set_sd(x))) }
}

/// sin and cos of `x` radians.
fn sin_cos(x: f64) -> (f64, f64) {
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

fn round(x: f64) -> f64 {
    if x >= 0.0 { (x + 0.5) as i64 as f64 } else { -((-x + 0.5) as i64 as f64) }
}

fn clamp01(x: f32) -> f32 {
    if x.is_nan() { 0.0 } else { x.clamp(0.0, 1.0) }
}

// -----| contexts |-----

#[derive(Clone, Copy)]
struct Vtx {
    clip: [f32; 4],
    col: [f32; 4],
    tc: [f32; 4],
}

struct Tex {
    /// The kernel's texture, 0 until first used.
    handle: u64,
    w: u32,
    h: u32,
    /// Level 0, 0xAARRGGBB, top row (t = 0) first.
    texels: Vec<u32>,
    /// Which mipmap levels have been given (bit n: level n).
    levels: u32,
    min: u32,
    mag: u32,
    wrap_s: u32,
    wrap_t: u32,
    dirty: bool,
}

impl Tex {
    fn new() -> Tex {
        Tex { handle: 0, w: 0, h: 0, texels: Vec::new(), levels: 0, min: GL_NEAREST_MIPMAP_LINEAR, mag: GL_LINEAR, wrap_s: GL_REPEAT, wrap_t: GL_REPEAT, dirty: false }
    }

    /// Usable for texturing: level 0 given, and every level if the
    /// minification filter uses mipmaps.
    fn complete(&self) -> bool {
        if self.w == 0 || self.h == 0 {
            return false;
        }
        if self.min == GL_NEAREST || self.min == GL_LINEAR {
            return true;
        }
        let n = 32 - self.w.max(self.h).leading_zeros();
        self.levels & ((1u32 << n) - 1) == (1u32 << n) - 1
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
    stacks: [Vec<Mat>; 3],
    viewport: [i32; 4],
    depth_range: [f32; 2],

    clear_color: [f32; 4],
    clear_depth: f32,
    color: [f32; 4],
    tc: [f32; 4],
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
    bound: u32,
    tex_env: u32,
    /// GL_PERSPECTIVE_CORRECTION_HINT is GL_NICEST. Only kept for glGet:
    /// colours and textures are always perspective-correct. (Windows'
    /// software OpenGL does colours so always, textures only with GL_NICEST;
    /// below that it approximates.)
    perspective: bool,

    prim: Option<u32>,
    verts: Vec<Vtx>,
    arrays: [Array; 4],

    groups: Vec<KGroup>,
    tris: Vec<KTri>,
    error: u32,
    unpack_align: i32,
    pack_align: i32,
    /// Frame read back for presenting.
    frame: Vec<u32>,
}

const A_VERTEX: usize = 0;
const A_COLOR: usize = 1;
const A_TEXCOORD: usize = 2;
const A_NORMAL: usize = 3;

/// Batches are sent once this many triangles are waiting.
const BATCH_TRIS: usize = 8192;

impl Ctx {
    fn err(&mut self, e: u32) {
        if self.error == GL_NO_ERROR {
            self.error = e;
        }
    }

    fn on(&self, cap: u32) -> bool {
        self.caps.contains(&cap)
    }

    fn top(&mut self) -> &mut Mat {
        let i = match self.matrix_mode {
            GL_PROJECTION => 1,
            GL_TEXTURE => 2,
            _ => 0,
        };
        self.stacks[i].last_mut().unwrap()
    }

    fn mult(&mut self, m: &Mat) {
        let t = self.top();
        *t = mul(t, m);
    }

    fn tex(&mut self, name: u32) -> Option<&mut Tex> {
        self.textures.iter_mut().find(|t| t.0 == name).map(|t| &mut t.1)
    }

    fn bound_tex(&mut self) -> &mut Tex {
        let n = self.bound;
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
        };
        unsafe { VeilGpu3d(2, self.target, &args as *const _ as u64, 0) };
        self.groups.clear();
        self.tris.clear();
    }

    /// The bound texture's kernel handle and size, if texturing applies.
    fn texture_for_draw(&mut self) -> Option<(u64, u32, u32)> {
        if !self.on(GL_TEXTURE_2D) {
            return None;
        }
        let t = self.bound_tex();
        if !t.complete() {
            return None;
        }
        if t.handle == 0 || t.dirty {
            let args = TexArgs {
                w: t.w,
                h: t.h,
                texels: t.texels.as_ptr() as u64,
                min_filter: t.min,
                mag_filter: t.mag,
                wrap_s: t.wrap_s,
                wrap_t: t.wrap_t,
            };
            let mut h = t.handle;
            unsafe { VeilGpu3d(4, t.handle, &args as *const _ as u64, &mut h as *mut u64 as u64) };
            t.handle = h;
            t.dirty = false;
        }
        Some((t.handle, t.w, t.h))
    }

    fn state(&mut self) -> (KGroup, Option<(u32, u32)>) {
        let tex = self.texture_for_draw();
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
        if tex.is_some() {
            flags |= F_TEXTURE;
        }
        if self.on(GL_ALPHA_TEST) {
            flags |= F_ALPHA_TEST;
        }
        let g = KGroup {
            first: 0,
            count: 0,
            flags,
            depth_func: self.depth_func,
            alpha_func: self.alpha_func,
            alpha_ref: (clamp01(self.alpha_ref) * 510.0 + 0.5) as u32,
            src_blend: self.blend_src,
            dst_blend: self.blend_dst,
            tex_env: self.tex_env,
            color_mask: self.mask_bits(),
            texture: tex.map_or(0, |t| t.0),
            scissor: self.scissor_box(),
        };
        (g, tex.map(|t| (t.1, t.2)))
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
        let eye = xform(&self.stacks[0].last().copied().unwrap(), obj);
        let clip = xform(&self.stacks[1].last().copied().unwrap(), eye);
        let tc = xform(&self.stacks[2].last().copied().unwrap(), self.tc);
        self.verts.push(Vtx { clip, col: self.color, tc });
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
            // Points and lines aren't drawn yet.
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
            let c = input[provoking].col;
            for p in &mut poly {
                p.col = c;
            }
        }
        // Against the six planes of the view volume, in clip space.
        for plane in 0..6 {
            let d = |v: &Vtx| {
                let (axis, sign) = (plane / 2, if plane % 2 == 0 { 1.0 } else { -1.0 });
                v.clip[3] + sign * v.clip[axis]
            };
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
                    let t = da / (da - db);
                    let lerp = |x: [f32; 4], y: [f32; 4]| core::array::from_fn(|k| x[k] + (y[k] - x[k]) * t);
                    out.push(Vtx { clip: lerp(a.clip, b.clip), col: lerp(a.col, b.col), tc: lerp(a.tc, b.tc) });
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
        let [vx, vy, vw, vh] = self.viewport;
        let (n, f) = (self.depth_range[0] as f64, self.depth_range[1] as f64);
        let win: Vec<[f64; 4]> = poly
            .iter()
            .map(|v| {
                let w = v.clip[3] as f64;
                let iw = 1.0 / w;
                [
                    (v.clip[0] as f64 * iw + 1.0) * vw as f64 / 2.0 + vx as f64,
                    (v.clip[1] as f64 * iw + 1.0) * vh as f64 / 2.0 + vy as f64,
                    (v.clip[2] as f64 * iw) * (f - n) / 2.0 + (f + n) / 2.0,
                    iw,
                ]
            })
            .collect();
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
        let (g, tex) = self.state();
        let (tw, th) = tex.map_or((0.0, 0.0), |(w, h)| (w as f64, h as f64));
        let qmax = win.iter().map(|w| w[3]).fold(0.0f64, f64::max);
        let kv: Vec<KVert> = poly
            .iter()
            .zip(win.iter())
            .map(|(v, w)| {
                let q = if qmax > 0.0 { w[3] / qmax } else { 1.0 };
                let col = |c: f32| (clamp01(c) as f64 * 255.0 * FX) as i64;
                KVert {
                    x: round(w[0] * SUB) as i32,
                    y: round((self.h as f64 - w[1]) * SUB) as i32,
                    z: (w[2].clamp(0.0, 1.0) * DEPTH_MAX * FX) as i64,
                    q: (q * FX) as i64,
                    uq: (v.tc[0] as f64 / v.tc[3] as f64 * tw * q * FX) as i64,
                    vq: (v.tc[1] as f64 / v.tc[3] as f64 * th * q * FX) as i64,
                    r: col(v.col[0]),
                    g: col(v.col[1]),
                    b: col(v.col[2]),
                    a: col(v.col[3]),
                }
            })
            .collect();
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
        stacks: [vec![IDENTITY], vec![IDENTITY], vec![IDENTITY]],
        viewport: [0, 0, w, h],
        depth_range: [0.0, 1.0],
        clear_color: [0.0; 4],
        clear_depth: 1.0,
        color: [1.0; 4],
        tc: [0.0, 0.0, 0.0, 1.0],
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
        bound: 0,
        tex_env: GL_MODULATE,
        perspective: false,
        prim: None,
        verts: Vec::new(),
        arrays: [Array::default(); 4],
        groups: Vec::new(),
        tris: Vec::new(),
        error: GL_NO_ERROR,
        unpack_align: 4,
        pack_align: 4,
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

/// No extensions yet.
#[no_mangle]
pub extern "system" fn wglGetProcAddress(_name: *const u8) -> usize {
    0
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
        0x1F03 => b"GL_EXT_bgra\0",
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
    outside(|c| {
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
    // GL_PERSPECTIVE_CORRECTION_HINT, GL_NICEST.
    outside(|c| {
        if target == 0x0C50 {
            c.perspective = mode == 0x1102;
        }
    })
}

#[no_mangle]
pub extern "system" fn glClearColor(r: f32, g: f32, b: f32, a: f32) {
    outside(|c| c.clear_color = [clamp01(r), clamp01(g), clamp01(b), clamp01(a)])
}

#[no_mangle]
pub extern "system" fn glClearDepth(d: f64) {
    outside(|c| c.clear_depth = clamp01(d as f32))
}

#[no_mangle]
pub extern "system" fn glClear(mask: u32) {
    outside(|c| {
        if mask & !(GL_COLOR_BUFFER_BIT | GL_DEPTH_BUFFER_BIT | 0x200 | 0x400) != 0 {
            c.err(GL_INVALID_VALUE);
            return;
        }
        c.flush();
        let ch = |v: f32| (v * 255.0 + 0.5) as u32;
        let args = ClearArgs {
            mask: mask & (GL_COLOR_BUFFER_BIT | GL_DEPTH_BUFFER_BIT),
            color: ch(c.clear_color[0]) << 16 | ch(c.clear_color[1]) << 8 | ch(c.clear_color[2]),
            depth: (c.clear_depth as f64 * DEPTH_MAX + 0.5) as u32,
            color_mask: c.mask_bits(),
            scissor: c.scissor_box(),
        };
        unsafe { VeilGpu3d(1, c.target, &args as *const _ as u64, 0) };
    })
}

#[no_mangle]
pub extern "system" fn glViewport(x: i32, y: i32, w: i32, h: i32) {
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
    outside(|c| c.depth_range = [clamp01(n as f32), clamp01(f as f32)])
}

#[no_mangle]
pub extern "system" fn glScissor(x: i32, y: i32, w: i32, h: i32) {
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
    outside(|c| if is_func(f) { c.depth_func = f } else { c.err(GL_INVALID_ENUM) })
}

#[no_mangle]
pub extern "system" fn glDepthMask(on: u8) {
    outside(|c| c.depth_mask = on != 0)
}

#[no_mangle]
pub extern "system" fn glAlphaFunc(f: u32, r: f32) {
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
    outside(|c| if matches!(m, GL_FRONT | GL_BACK | GL_FRONT_AND_BACK) { c.cull_mode = m } else { c.err(GL_INVALID_ENUM) })
}

#[no_mangle]
pub extern "system" fn glFrontFace(m: u32) {
    outside(|c| if m == GL_CW || m == GL_CCW { c.front_face = m } else { c.err(GL_INVALID_ENUM) })
}

#[no_mangle]
pub extern "system" fn glShadeModel(m: u32) {
    outside(|c| if m == GL_FLAT || m == GL_SMOOTH { c.shade_model = m } else { c.err(GL_INVALID_ENUM) })
}

#[no_mangle]
pub extern "system" fn glColorMask(r: u8, g: u8, b: u8, a: u8) {
    outside(|c| c.color_mask = [r != 0, g != 0, b != 0, a != 0])
}

#[no_mangle]
pub extern "system" fn glPixelStorei(name: u32, v: i32) {
    outside(|c| {
        if !matches!(v, 1 | 2 | 4 | 8) {
            c.err(GL_INVALID_VALUE);
            return;
        }
        match name {
            GL_UNPACK_ALIGNMENT => c.unpack_align = v,
            GL_PACK_ALIGNMENT => c.pack_align = v,
            _ => {}
        }
    })
}

#[no_mangle]
pub extern "system" fn glFlush() {
    outside(|c| {
        c.flush();
        if !c.double {
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
                let m = c.stacks[(name - 0x0BA6) as usize].last().unwrap();
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
            0x0D57 => one(out, 0.0),
            0x0D3A => {
                out[0] = MAX_TEXTURE_SIZE as f64;
                out[1] = MAX_TEXTURE_SIZE as f64;
                2
            }
            0x8069 => one(out, c.bound as f64),
            0x0C50 => one(out, if c.perspective { 0x1102 as f64 } else { 0x1100 as f64 }),
            0x0C32 => one(out, c.double as u32 as f64),
            0x0B74 => one(out, c.depth_func as f64),
            0x0B72 => one(out, c.depth_mask as u32 as f64),
            0x0B54 => one(out, c.shade_model as f64),
            0x0B45 => one(out, c.cull_mode as f64),
            0x0B46 => one(out, c.front_face as f64),
            GL_UNPACK_ALIGNMENT => one(out, c.unpack_align as f64),
            GL_PACK_ALIGNMENT => one(out, c.pack_align as f64),
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
    outside(|c| if matches!(m, GL_MODELVIEW | GL_PROJECTION | GL_TEXTURE) { c.matrix_mode = m } else { c.err(GL_INVALID_ENUM) })
}

#[no_mangle]
pub extern "system" fn glLoadIdentity() {
    outside(|c| *c.top() = IDENTITY)
}

#[no_mangle]
pub unsafe extern "system" fn glLoadMatrixf(m: *const f32) {
    let m: Mat = core::array::from_fn(|i| unsafe { *m.add(i) });
    outside(|c| *c.top() = m)
}

#[no_mangle]
pub unsafe extern "system" fn glLoadMatrixd(m: *const f64) {
    let m: Mat = core::array::from_fn(|i| unsafe { *m.add(i) } as f32);
    outside(|c| *c.top() = m)
}

#[no_mangle]
pub unsafe extern "system" fn glMultMatrixf(m: *const f32) {
    let m: Mat = core::array::from_fn(|i| unsafe { *m.add(i) });
    outside(|c| c.mult(&m))
}

#[no_mangle]
pub unsafe extern "system" fn glMultMatrixd(m: *const f64) {
    let m: Mat = core::array::from_fn(|i| unsafe { *m.add(i) } as f32);
    outside(|c| c.mult(&m))
}

#[no_mangle]
pub extern "system" fn glPushMatrix() {
    outside(|c| {
        let i = match c.matrix_mode { GL_PROJECTION => 1, GL_TEXTURE => 2, _ => 0 };
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
    outside(|c| {
        let i = match c.matrix_mode { GL_PROJECTION => 1, GL_TEXTURE => 2, _ => 0 };
        if c.stacks[i].len() <= 1 {
            c.err(GL_STACK_UNDERFLOW);
        } else {
            c.stacks[i].pop();
        }
    })
}

#[no_mangle]
pub extern "system" fn glTranslatef(x: f32, y: f32, z: f32) {
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
    with(|c| {
        if c.prim.is_some() {
            c.err(GL_INVALID_OPERATION);
        } else if mode > GL_POLYGON {
            c.err(GL_INVALID_ENUM);
        } else {
            c.prim = Some(mode);
            c.verts.clear();
        }
    })
}

#[no_mangle]
pub extern "system" fn glEnd() {
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
    with(|c| c.tc = [s, t, r, q])
}

#[no_mangle] pub extern "system" fn glTexCoord1f(s: f32) { texcoord(s, 0.0, 0.0, 1.0) }
#[no_mangle] pub extern "system" fn glTexCoord2f(s: f32, t: f32) { texcoord(s, t, 0.0, 1.0) }
#[no_mangle] pub extern "system" fn glTexCoord2d(s: f64, t: f64) { texcoord(s as f32, t as f32, 0.0, 1.0) }
#[no_mangle] pub extern "system" fn glTexCoord2i(s: i32, t: i32) { texcoord(s as f32, t as f32, 0.0, 1.0) }
#[no_mangle] pub unsafe extern "system" fn glTexCoord2fv(v: *const f32) { unsafe { texcoord(*v, *v.add(1), 0.0, 1.0) } }
#[no_mangle] pub extern "system" fn glTexCoord4f(s: f32, t: f32, r: f32, q: f32) { texcoord(s, t, r, q) }

#[no_mangle]
pub extern "system" fn glNormal3f(x: f32, y: f32, z: f32) {
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
            GL_TEXTURE_COORD_ARRAY => A_TEXCOORD,
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
#[no_mangle] pub extern "system" fn glTexCoordPointer(size: i32, kind: u32, stride: i32, p: usize) { pointer(A_TEXCOORD, size, kind, stride, p) }
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
                self.tc = a[A_TEXCOORD].get(i, false);
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
        let saved = core::mem::take(&mut self.verts);
        for i in idx {
            self.element(i);
        }
        let v = core::mem::replace(&mut self.verts, saved);
        self.assemble(mode, &v);
    }
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
            if c.bound == name {
                c.bound = 0;
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
    outside(|c| {
        if target != GL_TEXTURE_2D {
            if target != 0x0DE0 {
                c.err(GL_INVALID_ENUM);
            }
            return;
        }
        c.bound = name;
        c.bound_tex();
    })
}

/// Texels from client memory as 0xAARRGGBB, rows of `w`.
unsafe fn unpack(c: &Ctx, w: i32, h: i32, format: u32, kind: u32, pixels: *const u8) -> Option<Vec<u32>> {
    let comps = match format {
        GL_RGB | GL_BGR_EXT => 3,
        GL_RGBA | GL_BGRA_EXT => 4,
        GL_LUMINANCE | GL_ALPHA | 0x1903 => 1,
        GL_LUMINANCE_ALPHA => 2,
        _ => return None,
    };
    if kind != GL_UNSIGNED_BYTE {
        return None;
    }
    let align = c.unpack_align as usize;
    let row = (w as usize * comps).div_ceil(align) * align;
    let mut out = Vec::with_capacity((w * h) as usize);
    for y in 0..h as usize {
        for x in 0..w as usize {
            let p = unsafe { pixels.add(y * row + x * comps) };
            let b = |i: usize| unsafe { *p.add(i) } as u32;
            let (r, g, bl, a) = match format {
                GL_RGB => (b(0), b(1), b(2), 255),
                GL_BGR_EXT => (b(2), b(1), b(0), 255),
                GL_RGBA => (b(0), b(1), b(2), b(3)),
                GL_BGRA_EXT => (b(2), b(1), b(0), b(3)),
                GL_LUMINANCE => (b(0), b(0), b(0), 255),
                GL_LUMINANCE_ALPHA => (b(0), b(0), b(0), b(1)),
                GL_ALPHA => (0, 0, 0, b(0)),
                // GL_RED
                _ => (b(0), 0, 0, 255),
            };
            out.push(a << 24 | r << 16 | g << 8 | bl);
        }
    }
    Some(out)
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
        c.flush();
        let t = c.bound_tex();
        if level == 0 {
            t.w = w as u32;
            t.h = h as u32;
            t.texels = texels.into_iter().map(|p| internal(ifmt, p)).collect();
            t.levels = 1;
            t.dirty = true;
        } else if level < 32 {
            t.levels |= 1 << level;
        }
    })
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
        c.flush();
        let t = c.bound_tex();
        if level != 0 {
            return;
        }
        if x < 0 || y < 0 || x + w > t.w as i32 || y + h > t.h as i32 {
            c.err(GL_INVALID_VALUE);
            return;
        }
        for row in 0..h as usize {
            for col in 0..w as usize {
                let p = src[row * w as usize + col];
                let at = (y as usize + row) * t.w as usize + x as usize + col;
                // Formats without alpha keep it opaque, as the image has it.
                t.texels[at] = if t.texels[at] >> 24 == 0xFF && format != GL_RGBA && format != GL_BGRA_EXT { p | 0xFF00_0000 } else { p };
            }
        }
        t.dirty = true;
    })
}

fn tex_param(target: u32, name: u32, v: u32) {
    outside(|c| {
        if target != GL_TEXTURE_2D {
            c.err(GL_INVALID_ENUM);
            return;
        }
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
        let t = c.bound_tex();
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
    outside(|c| {
        if target != GL_TEXTURE_ENV {
            c.err(GL_INVALID_ENUM);
            return;
        }
        if name == GL_TEXTURE_ENV_MODE {
            if matches!(v, GL_MODULATE | GL_DECAL | GL_REPLACE) {
                c.tex_env = v;
            } else {
                c.err(GL_INVALID_ENUM);
            }
        }
    })
}

#[no_mangle] pub extern "system" fn glTexEnvi(t: u32, n: u32, v: i32) { tex_env(t, n, v as u32) }
#[no_mangle] pub extern "system" fn glTexEnvf(t: u32, n: u32, v: f32) { tex_env(t, n, v as u32) }
#[no_mangle] pub unsafe extern "system" fn glTexEnviv(t: u32, n: u32, v: *const i32) { tex_env(t, n, unsafe { *v } as u32) }
#[no_mangle] pub unsafe extern "system" fn glTexEnvfv(t: u32, n: u32, v: *const f32) { tex_env(t, n, unsafe { *v } as u32) }

// -----| reading pixels |-----

#[no_mangle]
pub unsafe extern "system" fn glReadPixels(x: i32, y: i32, w: i32, h: i32, format: u32, kind: u32, out: *mut u8) {
    outside(|c| {
        if w < 0 || h < 0 {
            c.err(GL_INVALID_VALUE);
            return;
        }
        let comps = match format {
            GL_RGB | GL_BGR_EXT => 3,
            GL_RGBA | GL_BGRA_EXT => 4,
            _ => {
                c.err(GL_INVALID_ENUM);
                return;
            }
        };
        if kind != GL_UNSIGNED_BYTE {
            c.err(GL_INVALID_ENUM);
            return;
        }
        if w == 0 || h == 0 {
            return;
        }
        c.flush();
        // The rows asked for, top one first (GL counts rows from the bottom).
        let mut buf = vec![0u32; (w * h) as usize];
        let r = ReadArgs { x, y: c.h - (y + h), w, h, dst: buf.as_mut_ptr() as u64, stride: w as i64 * 4 };
        unsafe { VeilGpu3d(3, c.target, &r as *const _ as u64, 0) };
        let align = c.pack_align as usize;
        let row = (w as usize * comps).div_ceil(align) * align;
        for gl_row in 0..h as usize {
            let src = &buf[(h as usize - 1 - gl_row) * w as usize..][..w as usize];
            for (i, &p) in src.iter().enumerate() {
                let (r, g, b) = ((p >> 16) as u8, (p >> 8) as u8, p as u8);
                let d = unsafe { out.add(gl_row * row + i * comps) };
                let px: [u8; 4] = match format {
                    GL_BGR_EXT | GL_BGRA_EXT => [b, g, r, 255],
                    _ => [r, g, b, 255],
                };
                unsafe { core::ptr::copy_nonoverlapping(px.as_ptr(), d, comps) };
            }
        }
    })
}
