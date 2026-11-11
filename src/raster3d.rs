// =============================================================================
// VEILOS: 3D rasterizer (WIN32_DESIGN.md G5)
// =============================================================================
//
// Triangles already transformed, clipped and mapped to the window by the
// API above (opengl32.dll does the fixed-function vertex stage in user mode)
// are filled here, per pixel: depth test, texturing, alpha test, blending,
// the colour mask. `lgpu::execute_3d` cuts the target into bands and renders
// them on every CPU; a band only touches its own rows of the colour and
// depth buffers, and runs its triangles in order, so the result is the same
// as a serial render.
//
// Everything is integer arithmetic: the kernel is built without SSE, so
// floats would be emulated in software. Positions are in 1/256 pixel; every
// interpolated value is fixed point with 32 fraction bits ("fx32"), evaluated
// from its plane equation (exact at the bounding box's first pixel, then
// stepped by gradients rounded once per triangle). Pixel centres sit at
// (x + 0.5, y + 0.5); a pixel centre exactly on an edge belongs to the
// triangle only for top and left edges.
//
// This file only uses `core` and `alloc`: the syscall side (targets and
// textures as handle objects, VEIL_GPU3D) is nt_gpu3d.rs, and the file is
// also built into a Windows copy of opengl32.dll (`--features host`) that
// runs the same rasterizer next to Windows' own OpenGL for comparison.

use alloc::vec::Vec;

// -----| the ABI (mirrored in win32/opengl32) |-----

/// A vertex in window space, y downwards.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Vert {
    /// Position in 1/SUB pixel.
    pub x: i32,
    pub y: i32,
    /// Depth, 0 ..= DEPTH_MAX, fx32.
    pub z: i64,
    /// 1/w scaled so the triangle's largest is at most 1, fx32.
    pub q: i64,
    /// Texture position in texels times q, fx32.
    pub uq: i64,
    pub vq: i64,
    /// Colour, 0 ..= 255 each, fx32.
    pub r: i64,
    pub g: i64,
    pub b: i64,
    pub a: i64,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Tri {
    pub v: [Vert; 3],
}

pub const F_DEPTH_TEST: u32 = 1;
pub const F_DEPTH_WRITE: u32 = 2;
pub const F_BLEND: u32 = 4;
pub const F_TEXTURE: u32 = 8;
pub const F_ALPHA_TEST: u32 = 16;

/// Triangles `first .. first + count` of a batch, drawn with one state.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Group {
    pub first: u32,
    pub count: u32,
    pub flags: u32,
    /// GL_NEVER (0x200) ..= GL_ALWAYS (0x207).
    pub depth_func: u32,
    pub alpha_func: u32,
    /// 0 ..= 510: twice the alpha (0 ..= 255) compared with, so a
    /// reference halfway between two alphas (0.5) is exact.
    pub alpha_ref: u32,
    /// GL blend factors.
    pub src_blend: u32,
    pub dst_blend: u32,
    /// GL_MODULATE, GL_DECAL, GL_REPLACE.
    pub tex_env: u32,
    /// Bit 0 red, 1 green, 2 blue.
    pub color_mask: u32,
    /// Texture handle (F_TEXTURE).
    pub texture: u64,
    /// x0, y0, x1, y1 in pixels, y downwards: only these are drawn.
    pub scissor: [i32; 4],
}

#[repr(C)]
pub struct DrawArgs {
    pub groups: u64,
    pub group_count: u32,
    pub tri_count: u32,
    pub tris: u64,
}

#[repr(C)]
pub struct ClearArgs {
    /// GL_COLOR_BUFFER_BIT (0x4000), GL_DEPTH_BUFFER_BIT (0x100).
    pub mask: u32,
    /// 0x00RRGGBB.
    pub color: u32,
    /// 0 ..= DEPTH_MAX.
    pub depth: u32,
    pub color_mask: u32,
    pub scissor: [i32; 4],
}

#[repr(C)]
pub struct ReadArgs {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    /// 0x00RRGGBB pixels, rows `stride` bytes apart.
    pub dst: u64,
    pub stride: i64,
}

#[repr(C)]
pub struct TexArgs {
    pub w: u32,
    pub h: u32,
    /// w * h 0xAARRGGBB texels, top row (t = 0) first.
    pub texels: u64,
    /// GL_NEAREST or GL_LINEAR (anything else: as GL_LINEAR's family says).
    pub min_filter: u32,
    pub mag_filter: u32,
    /// GL_REPEAT or GL_CLAMP.
    pub wrap_s: u32,
    pub wrap_t: u32,
}

pub const DEPTH_MAX: u32 = 0xFF_FFFF;
/// Subpixel steps per pixel. Windows' software OpenGL doesn't snap vertices
/// to a coarse grid (its coverage matches exact edges), so neither do we.
pub const SUB: i32 = 256;
const HALF: i32 = SUB / 2;

const GL_NEVER: u32 = 0x200;
const GL_LESS: u32 = 0x201;
const GL_EQUAL: u32 = 0x202;
const GL_LEQUAL: u32 = 0x203;
const GL_GREATER: u32 = 0x204;
const GL_NOTEQUAL: u32 = 0x205;
const GL_GEQUAL: u32 = 0x206;
const GL_ZERO: u32 = 0;
const GL_ONE: u32 = 1;
const GL_SRC_COLOR: u32 = 0x300;
const GL_ONE_MINUS_SRC_COLOR: u32 = 0x301;
const GL_SRC_ALPHA: u32 = 0x302;
const GL_ONE_MINUS_SRC_ALPHA: u32 = 0x303;
const GL_DST_ALPHA: u32 = 0x304;
const GL_ONE_MINUS_DST_ALPHA: u32 = 0x305;
const GL_DST_COLOR: u32 = 0x306;
const GL_ONE_MINUS_DST_COLOR: u32 = 0x307;
const GL_SRC_ALPHA_SATURATE: u32 = 0x308;
const GL_DECAL: u32 = 0x2101;
const GL_REPLACE: u32 = 0x1E01;
const GL_NEAREST: u32 = 0x2600;
const GL_NEAREST_MIPMAP_NEAREST: u32 = 0x2700;
const GL_NEAREST_MIPMAP_LINEAR: u32 = 0x2702;
const GL_REPEAT: u32 = 0x2901;

// -----| objects |-----

pub struct Texture {
    pub w: usize,
    pub h: usize,
    pub texels: Vec<u32>,
    pub nearest_min: bool,
    pub nearest_mag: bool,
    pub repeat_s: bool,
    pub repeat_t: bool,
}


// -----| per-band rendering |-----

/// A target's buffers as raw pointers, shared by the bands (each writes only
/// its own rows).
#[derive(Clone, Copy)]
pub struct Buffers {
    pub color: *mut u32,
    pub depth: *mut u32,
    pub w: usize,
    pub h: usize,
}

unsafe impl Send for Buffers {}
unsafe impl Sync for Buffers {}

/// A group with its texture looked up.
pub struct Pass<'a> {
    pub g: Group,
    pub tex: Option<&'a Texture>,
}

/// The pixel rows a triangle can touch (for banding).
pub fn tri_rows(t: &Tri) -> (i32, i32) {
    let y0 = t.v.iter().map(|v| v.y).min().unwrap_or(0);
    let y1 = t.v.iter().map(|v| v.y).max().unwrap_or(0);
    // Centres at SUB * row + HALF inside [y0, y1].
    ((y0 - HALF + SUB - 1).div_euclid(SUB), (y1 - HALF).div_euclid(SUB) + 1)
}

/// One interpolated value: its value at the first pixel and its steps.
#[derive(Clone, Copy, Default)]
struct Plane {
    base: i64,
    dx: i64,
    dy: i64,
}

impl Plane {
    #[inline(always)]
    fn at(&self, col: i64, row: i64) -> i64 {
        self.base.wrapping_add(self.dx.wrapping_mul(col)).wrapping_add(self.dy.wrapping_mul(row))
    }
}

/// Render `passes` (in order) into rows `y0 .. y1` of `buf`.
pub fn render_band(buf: Buffers, tris: &[Tri], passes: &[Pass], y0: i32, y1: i32) {
    for p in passes {
        let g = &p.g;
        let first = g.first as usize;
        let end = (first + g.count as usize).min(tris.len());
        if first >= end {
            continue;
        }
        let clip = [
            g.scissor[0].max(0),
            g.scissor[1].max(y0).max(0),
            g.scissor[2].min(buf.w as i32),
            g.scissor[3].min(y1).min(buf.h as i32),
        ];
        if clip[0] >= clip[2] || clip[1] >= clip[3] {
            continue;
        }
        for t in &tris[first..end] {
            draw_tri(buf, t, p, clip);
        }
    }
}

fn draw_tri(buf: Buffers, t: &Tri, p: &Pass, clip: [i32; 4]) {
    let mut v = t.v;
    let det = |v: &[Vert; 3]| {
        (v[1].x as i64 - v[0].x as i64) * (v[2].y as i64 - v[0].y as i64)
            - (v[2].x as i64 - v[0].x as i64) * (v[1].y as i64 - v[0].y as i64)
    };
    let mut area = det(&v);
    if area == 0 {
        return;
    }
    if area < 0 {
        v.swap(1, 2);
        area = -area;
    }
    // Bounding box in pixels, clipped.
    let minx = v.iter().map(|v| v.x).min().unwrap();
    let maxx = v.iter().map(|v| v.x).max().unwrap();
    let (ry0, ry1) = tri_rows(t);
    // Everything is evaluated from the triangle's own first pixel (ox, oy),
    // not the band's: every band then computes exactly the same values, so
    // a banded render is the same as a serial one, bit for bit.
    let ox = (minx - HALF + SUB - 1).div_euclid(SUB);
    let oy = ry0;
    let bx0 = ox.max(clip[0]);
    let bx1 = ((maxx - HALF).div_euclid(SUB) + 1).min(clip[2]);
    let by0 = ry0.max(clip[1]);
    let by1 = ry1.min(clip[3]);
    if bx0 >= bx1 || by0 >= by1 {
        return;
    }
    // Edges: E_ab(p) = (bx - ax)(py - ay) - (by - ay)(px - ax), positive inside.
    let cx = ox as i64 * SUB as i64 + HALF as i64;
    let cy = oy as i64 * SUB as i64 + HALF as i64;
    struct Edge {
        e: i64,
        dx: i64,
        dy: i64,
        incl: bool,
    }
    let edge = |a: &Vert, b: &Vert| {
        let ex = b.x as i64 - a.x as i64;
        let ey = b.y as i64 - a.y as i64;
        Edge {
            e: ex * (cy - a.y as i64) - ey * (cx - a.x as i64),
            dx: -ey * SUB as i64,
            dy: ex * SUB as i64,
            // Top (horizontal, going right) and left (going up) edges own
            // the centres on them.
            incl: (ey == 0 && ex > 0) || ey < 0,
        }
    };
    let e12 = edge(&v[1], &v[2]);
    let e20 = edge(&v[2], &v[0]);
    let e01 = edge(&v[0], &v[1]);
    // Weights at the first pixel: E12 for v0, E20 for v1, E01 for v2.
    let plane = |a0: i64, a1: i64, a2: i64| -> Plane {
        let d1 = (a1 - a0) as i128;
        let d2 = (a2 - a0) as i128;
        let ar = area as i128;
        let num = d1 * e20.e as i128 + d2 * e01.e as i128;
        Plane {
            base: a0.wrapping_add(div_round(num, ar) as i64),
            dx: div_round(d1 * e20.dx as i128 + d2 * e01.dx as i128, ar) as i64,
            dy: div_round(d1 * e20.dy as i128 + d2 * e01.dy as i128, ar) as i64,
        }
    };
    let g = &p.g;
    let z = plane(v[0].z, v[1].z, v[2].z);
    let r = plane(v[0].r, v[1].r, v[2].r);
    let gr = plane(v[0].g, v[1].g, v[2].g);
    let b = plane(v[0].b, v[1].b, v[2].b);
    let a = plane(v[0].a, v[1].a, v[2].a);
    let tex = if g.flags & F_TEXTURE != 0 { p.tex } else { None };
    let (uq, vq) = if tex.is_some() { (plane(v[0].uq, v[1].uq, v[2].uq), plane(v[0].vq, v[1].vq, v[2].vq)) } else { Default::default() };
    // Equal q (no perspective, e.g. an orthographic projection): texture
    // coordinates are linear in the window, no division needed.
    let affine = v[0].q == v[1].q && v[1].q == v[2].q;
    let q = if affine { Plane::default() } else { plane(v[0].q, v[1].q, v[2].q) };

    let skip = (bx0 - ox) as i64;
    for py in by0..by1 {
        let row = (py - oy) as i64;
        let line = py as usize * buf.w;
        let mut w0 = e12.e + e12.dy * row + e12.dx * skip;
        let mut w1 = e20.e + e20.dy * row + e20.dx * skip;
        let mut w2 = e01.e + e01.dy * row + e01.dx * skip;
        for px in bx0..bx1 {
            let inside = (w0 > 0 || (w0 == 0 && e12.incl)) && (w1 > 0 || (w1 == 0 && e20.incl)) && (w2 > 0 || (w2 == 0 && e01.incl));
            if inside {
                let col = (px - ox) as i64;
                let at = line + px as usize;
                let depth = (z.at(col, row) >> 32).clamp(0, DEPTH_MAX as i64) as u32;
                let dptr = unsafe { buf.depth.add(at) };
                if g.flags & F_DEPTH_TEST == 0 || depth_pass(g.depth_func, depth, unsafe { *dptr }) {
                    // Perspective: texture coordinates were sent times q
                    // (1/w); dividing by the interpolated q is one division
                    // a pixel (recip = 2^30 / q) and a multiply per coordinate.
                    let recip = if affine || tex.is_none() { 0 } else { (1i64 << 62) / (q.at(col, row).max(1)) };
                    let attr = |pl: &Plane| -> i64 {
                        let v = pl.at(col, row);
                        if affine { v } else { ((v as i128 * recip as i128) >> 30) as i64 }
                    };
                    // Colours are linear in the window (as Windows has them).
                    let mut c = [chan(r.at(col, row)), chan(gr.at(col, row)), chan(b.at(col, row)), chan(a.at(col, row))];
                    if let Some(tx) = tex {
                        let t = sample(tx, attr(&uq) >> 16, attr(&vq) >> 16);
                        c = tex_env(g.tex_env, c, t);
                    }
                    if g.flags & F_ALPHA_TEST == 0 || compare(g.alpha_func, c[3] as u32 * 2, g.alpha_ref) {
                        // A pixel failing the alpha test leaves depth alone.
                        if g.flags & F_DEPTH_TEST != 0 && g.flags & F_DEPTH_WRITE != 0 {
                            unsafe { *dptr = depth };
                        }
                        let cptr = unsafe { buf.color.add(at) };
                        let dst = unsafe { *cptr };
                        if g.flags & F_BLEND != 0 {
                            c = blend(g.src_blend, g.dst_blend, c, dst);
                        }
                        let src = (c[0] as u32) << 16 | (c[1] as u32) << 8 | c[2] as u32;
                        let m = mask_bits(g.color_mask);
                        unsafe { *cptr = (src & m) | (dst & !m & 0xFF_FFFF) };
                    }
                }
            }
            w0 += e12.dx;
            w1 += e20.dx;
            w2 += e01.dx;
        }
    }
}

fn mask_bits(m: u32) -> u32 {
    (if m & 1 != 0 { 0xFF_0000 } else { 0 }) | (if m & 2 != 0 { 0xFF00 } else { 0 }) | (if m & 4 != 0 { 0xFF } else { 0 })
}

/// Round to nearest, halves away from zero.
fn div_round(n: i128, d: i128) -> i128 {
    if (n < 0) == (d < 0) { (n + d / 2) / d } else { (n - d / 2) / d }
}

/// fx32 colour to 0 ..= 255, truncated (as Windows' software OpenGL does).
#[inline(always)]
fn chan(v: i64) -> i32 {
    (v >> 32).clamp(0, 255) as i32
}

fn compare(func: u32, a: u32, b: u32) -> bool {
    match func {
        GL_NEVER => false,
        GL_LESS => a < b,
        GL_EQUAL => a == b,
        GL_LEQUAL => a <= b,
        GL_GREATER => a > b,
        GL_NOTEQUAL => a != b,
        GL_GEQUAL => a >= b,
        _ => true,
    }
}

#[inline(always)]
fn depth_pass(func: u32, z: u32, stored: u32) -> bool {
    match func {
        GL_LESS => z < stored,
        GL_LEQUAL => z <= stored,
        f => compare(f, z, stored),
    }
}

/// a * b / 255, rounded.
#[inline(always)]
fn mul255(a: i32, b: i32) -> i32 {
    let t = a * b + 128;
    (t + (t >> 8)) >> 8
}

fn wrap(i: i64, n: usize, repeat: bool) -> usize {
    if repeat { i.rem_euclid(n as i64) as usize } else { i.clamp(0, n as i64 - 1) as usize }
}

/// The texel colour at (u, v), in texels with 16 fraction bits, as [r, g, b, a].
fn sample(t: &Texture, u: i64, v: i64) -> [i32; 4] {
    let get = |x: i64, y: i64| {
        let p = t.texels[wrap(y, t.h, t.repeat_t) * t.w + wrap(x, t.w, t.repeat_s)];
        [(p >> 16 & 0xFF) as i32, (p >> 8 & 0xFF) as i32, (p & 0xFF) as i32, (p >> 24) as i32]
    };
    // Without mipmaps the texture is never minified differently from
    // magnified except by the filter chosen.
    if t.nearest_mag && t.nearest_min {
        return get(u >> 16, v >> 16);
    }
    let u = u - 0x8000;
    let v = v - 0x8000;
    let (x, y) = (u >> 16, v >> 16);
    let fx = ((u >> 8) & 0xFF) as i32;
    let fy = ((v >> 8) & 0xFF) as i32;
    let (a, b, c, d) = (get(x, y), get(x + 1, y), get(x, y + 1), get(x + 1, y + 1));
    let mut out = [0; 4];
    for i in 0..4 {
        let top = a[i] * (256 - fx) + b[i] * fx;
        let bot = c[i] * (256 - fx) + d[i] * fx;
        // Truncated, as Windows' software OpenGL does.
        out[i] = (top * (256 - fy) + bot * fy) >> 16;
    }
    out
}

fn tex_env(mode: u32, c: [i32; 4], t: [i32; 4]) -> [i32; 4] {
    match mode {
        GL_REPLACE => t,
        GL_DECAL => {
            let mix = |cc: i32, tc: i32| mul255(cc, 255 - t[3]) + mul255(tc, t[3]);
            [mix(c[0], t[0]), mix(c[1], t[1]), mix(c[2], t[2]), c[3]]
        }
        // GL_MODULATE
        _ => [mul255(c[0], t[0]), mul255(c[1], t[1]), mul255(c[2], t[2]), mul255(c[3], t[3])],
    }
}

/// Destination alpha is always 1: the colour buffer has no alpha bits.
fn factor(f: u32, src: [i32; 4], dst: [i32; 3], i: usize) -> i32 {
    match f {
        GL_ZERO => 0,
        GL_ONE => 255,
        GL_SRC_COLOR => if i < 3 { src[i] } else { src[3] },
        GL_ONE_MINUS_SRC_COLOR => 255 - if i < 3 { src[i] } else { src[3] },
        GL_SRC_ALPHA => src[3],
        GL_ONE_MINUS_SRC_ALPHA => 255 - src[3],
        GL_DST_ALPHA => 255,
        GL_ONE_MINUS_DST_ALPHA => 0,
        GL_DST_COLOR => if i < 3 { dst[i] } else { 255 },
        GL_ONE_MINUS_DST_COLOR => 255 - if i < 3 { dst[i] } else { 255 },
        // min(As, 1 - Ad) = 0 with Ad = 1.
        GL_SRC_ALPHA_SATURATE => if i < 3 { 0 } else { 255 },
        _ => 255,
    }
}

fn blend(sf: u32, df: u32, src: [i32; 4], dst: u32) -> [i32; 4] {
    let d = [(dst >> 16 & 0xFF) as i32, (dst >> 8 & 0xFF) as i32, (dst & 0xFF) as i32];
    let mut out = src;
    for i in 0..3 {
        let v = mul255(src[i], factor(sf, src, d, i)) + mul255(d[i], factor(df, src, d, i));
        out[i] = v.min(255);
    }
    out
}


/// A texture from its upload: `texels` as TexArgs describes them.
pub fn texture(a: &TexArgs, texels: Vec<u32>) -> Texture {
    let nearest = |f: u32| matches!(f, GL_NEAREST | GL_NEAREST_MIPMAP_NEAREST | GL_NEAREST_MIPMAP_LINEAR);
    Texture {
        w: a.w as usize,
        h: a.h as usize,
        texels,
        nearest_min: nearest(a.min_filter),
        nearest_mag: nearest(a.mag_filter),
        repeat_s: a.wrap_s == GL_REPEAT,
        repeat_t: a.wrap_t == GL_REPEAT,
    }
}

/// glClear on a whole target.
pub fn clear(buf: Buffers, c: &ClearArgs) {
    let x0 = c.scissor[0].clamp(0, buf.w as i32) as usize;
    let x1 = c.scissor[2].clamp(0, buf.w as i32) as usize;
    let y0 = c.scissor[1].clamp(0, buf.h as i32) as usize;
    let y1 = c.scissor[3].clamp(0, buf.h as i32) as usize;
    if x0 >= x1 || y0 >= y1 {
        return;
    }
    let m = mask_bits(c.color_mask);
    for y in y0..y1 {
        let color = unsafe { core::slice::from_raw_parts_mut(buf.color.add(y * buf.w + x0), x1 - x0) };
        let depth = unsafe { core::slice::from_raw_parts_mut(buf.depth.add(y * buf.w + x0), x1 - x0) };
        if c.mask & 0x4000 != 0 {
            for p in color.iter_mut() {
                *p = (c.color & m) | (*p & !m & 0xFF_FFFF);
            }
        }
        if c.mask & 0x100 != 0 {
            depth.fill(c.depth.min(DEPTH_MAX));
        }
    }
}
