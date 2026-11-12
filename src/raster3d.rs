// =============================================================================
// VEILOS: 3D rasterizer (WIN32_DESIGN.md G5)
// =============================================================================
//
// Primitives already transformed, clipped and mapped to the window by the
// API above (opengl32.dll and d3d9.dll do the vertex stage in user mode)
// are filled here: triangles, lines and points. Every pixel they cover goes
// through one fragment pipeline: polygon stipple, the texture stages (up to
// eight, Direct3D's combiner, sampling up to four textures with mipmaps;
// OpenGL's environments are stages too), specular, fog, the alpha test, the
// stencil test, the depth test, blending (with an operation) or a logic op,
// and the colour mask. `lgpu::execute_3d` cuts the target into bands and
// renders them on every CPU; a band only touches its own rows of the
// colour, depth and stencil buffers, and runs its primitives in order, so
// the result is the same as a serial render.
//
// Everything is integer arithmetic: the kernel is built without SSE, so
// floats would be emulated in software. Positions are in 1/256 pixel; every
// interpolated value is fixed point with 32 fraction bits ("fx32"), evaluated
// from its plane equation at the primitive's own first pixel (never the
// band's, so bands compute exactly the values a serial render would). Pixel
// centres sit at (x + 0.5, y + 0.5); a centre exactly on a triangle's edge
// belongs to it only for top and left edges.
//
// This file only uses `core` and `alloc`: the syscall side (targets and
// textures as handle objects, VEIL_GPU3D) is nt_gpu3d.rs. The same file is
// built into the Win32 graphics DLLs through win32/gfx3d, for the ABI below
// and, in their Windows-hosted copies (`--features host`), to run this
// rasterizer next to Windows' own for comparison.

use alloc::vec::Vec;

// -----| the ABI (shared with the DLLs through win32/gfx3d) |-----

/// Texture slots: each has its coordinates in every vertex and a sampler.
pub const SLOTS: usize = 4;
/// Texture stages (Direct3D's eight).
pub const STAGES: usize = 8;

/// A vertex in window space, y downwards.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Vert {
    /// Position in 1/SUB pixel.
    pub x: i32,
    pub y: i32,
    /// Depth, 0 ..= DEPTH_MAX, fx32.
    pub z: i64,
    /// 1/w scaled so the primitive's largest is at most 1, fx32.
    pub q: i64,
    /// Colour, 0 ..= 255 each, fx32.
    pub r: i64,
    pub g: i64,
    pub b: i64,
    pub a: i64,
    /// Fog, linear in the window: with the group's `fog_mode` 0, how much
    /// of the fragment's own colour is kept, 0 ..= 255, fx32 (255: no fog);
    /// otherwise the eye distance, fx32, the fog worked out per pixel.
    pub f: i64,
    /// The specular colour (F_SPECULAR), 0 ..= 255 each, fx32.
    pub spec: [i64; 3],
    /// Each texture slot's position, in texels of its level 0, times q,
    /// fx32.
    pub uv: [[i64; 2]; SLOTS],
}

/// A triangle, or a line (`v[0]`, `v[1]`; `v[2].x` is where its stipple
/// pattern starts, in pixels) or a point (`v[0]`), as the group's `kind`
/// says.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Tri {
    pub v: [Vert; 3],
}

pub const F_DEPTH_TEST: u32 = 1;
pub const F_DEPTH_WRITE: u32 = 2;
pub const F_BLEND: u32 = 4;
pub const F_ALPHA_TEST: u32 = 16;
pub const F_FOG: u32 = 32;
pub const F_STENCIL: u32 = 64;
/// Slot 0's texels with alpha 0 discard the fragment (glBitmap).
pub const F_TEX_MASK: u32 = 256;
pub const F_POLY_STIPPLE: u32 = 512;
pub const F_LINE_STIPPLE: u32 = 1024;
/// A logic op (`logic_op`) instead of blending.
pub const F_LOGIC_OP: u32 = 2048;
/// Add the specular colour after the texture stages.
pub const F_SPECULAR: u32 = 4096;
/// The colour buffer keeps alpha (its top byte): written with colour mask
/// bit 3, read by the destination-alpha blend factors. Without it,
/// destination alpha is 1.
pub const F_DST_ALPHA: u32 = 8192;
/// Colours, specular and per-vertex fog perspective-correct (Direct3D),
/// not linear in the window (Windows' software OpenGL).
pub const F_PERSPECTIVE: u32 = 16384;

/// With a per-pixel fog mode: the vertices' fog values are 1/d, which is
/// linear in the window where d (Direct3D's w) isn't.
pub const FOG_INVERSE: u32 = 0x1_0000;

pub const KIND_TRIANGLES: u32 = 0;
pub const KIND_LINES: u32 = 1;
pub const KIND_POINTS: u32 = 2;

/// Direct3D's texture operations (D3DTOP_*).
pub mod top {
    pub const DISABLE: u32 = 1;
    pub const SELECTARG1: u32 = 2;
    pub const SELECTARG2: u32 = 3;
    pub const MODULATE: u32 = 4;
    pub const MODULATE2X: u32 = 5;
    pub const MODULATE4X: u32 = 6;
    pub const ADD: u32 = 7;
    pub const ADDSIGNED: u32 = 8;
    pub const ADDSIGNED2X: u32 = 9;
    pub const SUBTRACT: u32 = 10;
    pub const ADDSMOOTH: u32 = 11;
    pub const BLENDDIFFUSEALPHA: u32 = 12;
    pub const BLENDTEXTUREALPHA: u32 = 13;
    pub const BLENDFACTORALPHA: u32 = 14;
    pub const BLENDTEXTUREALPHAPM: u32 = 15;
    pub const BLENDCURRENTALPHA: u32 = 16;
    pub const PREMODULATE: u32 = 17;
    pub const MODULATEALPHA_ADDCOLOR: u32 = 18;
    pub const MODULATECOLOR_ADDALPHA: u32 = 19;
    pub const MODULATEINVALPHA_ADDCOLOR: u32 = 20;
    pub const MODULATEINVCOLOR_ADDALPHA: u32 = 21;
    pub const DOTPRODUCT3: u32 = 24;
    pub const MULTIPLYADD: u32 = 25;
    pub const LERP: u32 = 26;
}

/// Direct3D's texture arguments (D3DTA_*), and their modifiers.
pub mod ta {
    pub const DIFFUSE: u32 = 0;
    pub const CURRENT: u32 = 1;
    pub const TEXTURE: u32 = 2;
    pub const TFACTOR: u32 = 3;
    pub const SPECULAR: u32 = 4;
    pub const TEMP: u32 = 5;
    pub const CONSTANT: u32 = 6;
    pub const COMPLEMENT: u32 = 0x10;
    pub const ALPHAREPLICATE: u32 = 0x20;
}

/// One texture stage: Direct3D's colour and alpha operations, each on up to
/// three arguments (`arg[0]` is ARG1, `arg[1]` ARG2, `arg[2]` ARG0), where
/// its result goes (0 current, 1 temp), and its constant colour.
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Default)]
pub struct Stage {
    pub color_op: u32,
    pub color_arg: [u32; 3],
    pub alpha_op: u32,
    pub alpha_arg: [u32; 3],
    pub result: u32,
    /// 0xAARRGGBB.
    pub constant: u32,
}

/// Filters: point and linear.
pub const FILTER_POINT: u32 = 1;
pub const FILTER_LINEAR: u32 = 2;
/// Address modes (D3DTADDRESS_*): wrap, mirror, clamp, border, mirror once.
pub const WRAP: u32 = 1;
pub const MIRROR: u32 = 2;
pub const CLAMP: u32 = 3;
pub const BORDER: u32 = 4;
pub const MIRRORONCE: u32 = 5;

/// How a slot samples its texture.
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Default)]
pub struct Sampler {
    /// The texture's handle; 0: none (the slot reads white).
    pub texture: u64,
    pub min: u32,
    pub mag: u32,
    /// 0 no mipmaps, else FILTER_POINT or FILTER_LINEAR between levels.
    pub mip: u32,
    pub address: [u32; 2],
    /// 0xAARRGGBB (BORDER).
    pub border: u32,
    /// Added to the level of detail, 1/256 of a level.
    pub lod_bias: i32,
    /// The most detailed level used (D3DSAMP_MAXMIPLEVEL).
    pub max_level: u32,
    /// OpenGL's rule: magnification goes up to a level of detail of 0.5,
    /// not 0, with a linear magnification over a nearest minification.
    pub gl_rules: u32,
}

/// Primitives `first .. first + count` of a batch, drawn with one state.
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Default)]
pub struct Group {
    pub first: u32,
    pub count: u32,
    pub flags: u32,
    /// KIND_TRIANGLES, KIND_LINES or KIND_POINTS.
    pub kind: u32,
    /// Line width or point size in 1/SUB pixel.
    pub size: i32,
    /// GL_NEVER (0x200) ..= GL_ALWAYS (0x207).
    pub depth_func: u32,
    pub alpha_func: u32,
    /// 0 ..= 510: twice the alpha (0 ..= 255) compared with, so a
    /// reference halfway between two alphas (0.5) is exact.
    pub alpha_ref: u32,
    /// GL blend factors (GL_CONSTANT_COLOR and the rest: `blend_color`).
    pub src_blend: u32,
    pub dst_blend: u32,
    /// D3DBLENDOP: 0 or 1 add, 2 subtract, 3 reverse subtract, 4 min, 5 max.
    pub blend_op: u32,
    /// 0xAARRGGBB.
    pub blend_color: u32,
    /// Bit 0 red, 1 green, 2 blue, 3 alpha (F_DST_ALPHA).
    pub color_mask: u32,
    /// 0x00RRGGBB (F_FOG).
    pub fog_color: u32,
    /// The stencil test (F_STENCIL): function, reference, the mask both are
    /// ANDed with, the bits written, and the operations when the stencil
    /// test fails, the depth test fails, and both pass.
    pub stencil_func: u32,
    pub stencil_ref: u32,
    pub stencil_mask: u32,
    pub stencil_write: u32,
    pub stencil_ops: [u32; 3],
    /// x0, y0, x1, y1 in pixels, y downwards: only these are drawn.
    pub scissor: [i32; 4],
    /// Fog worked out per pixel: GL_LINEAR (0x2601), GL_EXP (0x800) or
    /// GL_EXP2 (0x801), with FOG_INVERSE if the vertices hold 1/d; 0: per
    /// vertex. And its two parameters, fx32: for GL_LINEAR f = a d + b,
    /// else the density in a.
    pub fog_mode: u32,
    pub fog_params: [i64; 2],
    /// GL_CLEAR (0x1500) ..= GL_SET (0x150F) (F_LOGIC_OP).
    pub logic_op: u32,
    /// The line stipple pattern (16 bits) and its repeat factor << 16.
    pub line_stipple: u32,
    /// The polygon stipple: 32 rows from the window's bottom, bit 31 the
    /// leftmost pixel.
    pub stipple: [u32; 32],
    /// The texture stages, up to the first whose colour op is DISABLE (or 0).
    pub stages: [Stage; STAGES],
    /// Stage i samples slot i's texture.
    pub samplers: [Sampler; SLOTS],
    /// D3DRS_TEXTUREFACTOR, 0xAARRGGBB.
    pub tfactor: u32,
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
    /// GL_COLOR_BUFFER_BIT (0x4000), GL_DEPTH_BUFFER_BIT (0x100),
    /// GL_STENCIL_BUFFER_BIT (0x400).
    pub mask: u32,
    /// 0xAARRGGBB (alpha only with colour mask bit 3).
    pub color: u32,
    /// 0 ..= DEPTH_MAX.
    pub depth: u32,
    pub color_mask: u32,
    pub scissor: [i32; 4],
    pub stencil: u32,
    /// The stencil bits cleared.
    pub stencil_write: u32,
}

#[repr(C)]
pub struct ReadArgs {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    /// 0xAARRGGBB pixels (alpha as kept), rows `stride` bytes apart.
    pub dst: u64,
    pub stride: i64,
}

/// Pixels written straight into a target (op 5) or read from its depth or
/// stencil buffer (op 6), rows `stride` bytes apart, y downwards.
#[repr(C)]
pub struct PixelArgs {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    pub ptr: u64,
    pub stride: i64,
    /// 0 colour (0xAARRGGBB), 1 depth (0 ..= DEPTH_MAX), 2 stencil.
    pub which: u32,
    /// Writing colour: the colour mask (bits 0-3); stencil: the bits written.
    pub mask: u32,
    /// Writing: only inside this (x0, y0, x1, y1).
    pub scissor: [i32; 4],
}

#[repr(C)]
pub struct TexArgs {
    /// Level 0's size.
    pub w: u32,
    pub h: u32,
    /// 0xAARRGGBB texels, top row (t = 0) first: level 0, then each
    /// following level (half the size, at least 1) for `levels` levels.
    pub texels: u64,
    pub levels: u32,
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
const GL_CONSTANT_COLOR: u32 = 0x8001;
const GL_ONE_MINUS_CONSTANT_COLOR: u32 = 0x8002;
const GL_CONSTANT_ALPHA: u32 = 0x8003;
const GL_ONE_MINUS_CONSTANT_ALPHA: u32 = 0x8004;
const GL_REPLACE: u32 = 0x1E01;
const GL_KEEP: u32 = 0x1E00;
const GL_INCR: u32 = 0x1E02;
const GL_DECR: u32 = 0x1E03;
const GL_INVERT: u32 = 0x150A;
/// D3DSTENCILOP_INCR and DECR: wrapping (8 bits).
const INCR_WRAP: u32 = 0x8507;
const DECR_WRAP: u32 = 0x8508;

// -----| objects |-----

#[derive(Clone, Copy)]
pub struct Level {
    pub w: usize,
    pub h: usize,
    /// Where it starts in `Texture::texels`.
    pub at: usize,
}

/// A texture's levels; how it's sampled is each slot's own (`Sampler`).
pub struct Texture {
    pub levels: Vec<Level>,
    pub texels: Vec<u32>,
}

/// The levels of a `w` x `h` texture given `count` of them.
pub fn levels(w: u32, h: u32, count: u32) -> Vec<Level> {
    let mut out = Vec::new();
    let (mut w, mut h, mut at) = (w as usize, h as usize, 0);
    for _ in 0..count.max(1) {
        out.push(Level { w, h, at });
        at += w * h;
        if w == 1 && h == 1 {
            break;
        }
        w = (w / 2).max(1);
        h = (h / 2).max(1);
    }
    out
}

// -----| per-band rendering |-----

/// A target's buffers as raw pointers, shared by the bands (each writes only
/// its own rows). The depth and stencil buffers may be another target's
/// (Direct3D pairs any render target with any depth buffer at least as
/// big): `dw` x `dh`, rows `dw` apart.
#[derive(Clone, Copy)]
pub struct Buffers {
    pub color: *mut u32,
    pub depth: *mut u32,
    pub stencil: *mut u8,
    pub w: usize,
    pub h: usize,
    pub dw: usize,
    pub dh: usize,
}

unsafe impl Send for Buffers {}
unsafe impl Sync for Buffers {}

/// A group with its textures looked up.
pub struct Pass<'a> {
    pub g: Group,
    pub tex: [Option<&'a Texture>; SLOTS],
}

impl Pass<'_> {
    /// The slots the stages sample, as a bit set.
    fn slots_used(&self) -> u32 {
        let mut used = 0;
        for (i, s) in self.g.stages.iter().enumerate().take(SLOTS) {
            if s.color_op == 0 || s.color_op == top::DISABLE {
                break;
            }
            let uses = |a: &[u32; 3]| a.iter().any(|x| x & 0xF == ta::TEXTURE);
            if self.tex[i].is_some() && (uses(&s.color_arg) || uses(&s.alpha_arg)) {
                used |= 1 << i;
            }
        }
        used
    }
}

/// The pixel rows a primitive of `g` can touch (for banding).
pub fn prim_rows(t: &Tri, g: &Group) -> (i32, i32) {
    let n = match g.kind { KIND_TRIANGLES => 3, KIND_LINES => 2, _ => 1 };
    let y0 = t.v[..n].iter().map(|v| v.y).min().unwrap_or(0);
    let y1 = t.v[..n].iter().map(|v| v.y).max().unwrap_or(0);
    if g.kind == KIND_TRIANGLES {
        // Centres at SUB * row + HALF inside [y0, y1].
        ((y0 - HALF + SUB - 1).div_euclid(SUB), (y1 - HALF).div_euclid(SUB) + 1)
    } else {
        let s = g.size.max(SUB);
        ((y0 - s).div_euclid(SUB) - 1, (y1 + s).div_euclid(SUB) + 2)
    }
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
            g.scissor[2].min(buf.w.min(buf.dw) as i32),
            g.scissor[3].min(y1).min(buf.h.min(buf.dh) as i32),
        ];
        if clip[0] >= clip[2] || clip[1] >= clip[3] {
            continue;
        }
        let used = p.slots_used();
        for t in &tris[first..end] {
            match g.kind {
                KIND_TRIANGLES => draw_tri(buf, t, p, used, clip),
                KIND_LINES => draw_line(buf, &t.v[0], &t.v[1], t.v[2].x as i64, p, used, clip),
                _ => draw_point(buf, &t.v[0], p, used, clip),
            }
        }
    }
}

/// What a covered pixel brings to the fragment pipeline.
struct Frag {
    /// Depth, 0 ..= DEPTH_MAX.
    z: u32,
    /// Colour, 0 ..= 255 each.
    c: [i32; 4],
    /// Specular, 0 ..= 255 each.
    spec: [i32; 3],
    /// Each slot's position in level-0 texels (16 fraction bits) and level
    /// of detail (log2 of texels per pixel, 8 fraction bits).
    uv: [(i64, i64, i32); SLOTS],
    /// Fog: how much of the colour is kept, 0 ..= 255.
    f: i32,
}

fn argb(c: u32) -> [i32; 4] {
    [(c >> 16 & 0xFF) as i32, (c >> 8 & 0xFF) as i32, (c & 0xFF) as i32, (c >> 24) as i32]
}

/// The texture stages: Direct3D's combiner, starting from the diffuse
/// colour, sampling slot i in stage i.
fn stages(p: &Pass, fr: &Frag) -> Option<[i32; 4]> {
    let g = &p.g;
    let diffuse = fr.c;
    let spec = [fr.spec[0], fr.spec[1], fr.spec[2], 255];
    let mut current = diffuse;
    let mut temp = [0; 4];
    for (i, st) in g.stages.iter().enumerate() {
        if st.color_op == 0 || st.color_op == top::DISABLE {
            break;
        }
        // The texel, if this stage reads it.
        let mut texel = [255; 4];
        if i < SLOTS {
            if let Some(t) = p.tex[i] {
                let uses = |a: &[u32; 3]| a.iter().any(|x| x & 0xF == ta::TEXTURE);
                if uses(&st.color_arg) || uses(&st.alpha_arg) {
                    let (u, v, lambda) = fr.uv[i];
                    texel = sample(t, &g.samplers[i], u, v, lambda);
                    if i == 0 && g.flags & F_TEX_MASK != 0 && texel[3] == 0 {
                        return None;
                    }
                }
            }
        }
        let arg = |a: u32| -> [i32; 4] {
            let mut v = match a & 0xF {
                ta::DIFFUSE => diffuse,
                ta::CURRENT => current,
                ta::TEXTURE => texel,
                ta::TFACTOR => argb(g.tfactor),
                ta::SPECULAR => spec,
                ta::TEMP => temp,
                ta::CONSTANT => argb(st.constant),
                _ => current,
            };
            if a & ta::ALPHAREPLICATE != 0 {
                v = [v[3]; 4];
            }
            if a & ta::COMPLEMENT != 0 {
                v = v.map(|x| 255 - x);
            }
            v
        };
        let (c1, c2, c0) = (arg(st.color_arg[0]), arg(st.color_arg[1]), arg(st.color_arg[2]));
        let (a1, a2, a0) = (arg(st.alpha_arg[0]), arg(st.alpha_arg[1]), arg(st.alpha_arg[2]));
        let factor_alpha = |k: u32| match k {
            top::BLENDDIFFUSEALPHA => diffuse[3],
            top::BLENDTEXTUREALPHA => texel[3],
            top::BLENDFACTORALPHA => argb(g.tfactor)[3],
            _ => current[3],
        };
        let rgb = combine(st.color_op, c1, c2, c0, factor_alpha(st.color_op), 3);
        // An alpha op left at DISABLE keeps the current alpha.
        let alpha = if st.alpha_op == 0 || st.alpha_op == top::DISABLE {
            current[3]
        } else if st.color_op == top::DOTPRODUCT3 {
            rgb[3]
        } else {
            combine(st.alpha_op, a1, a2, a0, factor_alpha(st.alpha_op), 4)[3]
        };
        let out = [rgb[0], rgb[1], rgb[2], alpha];
        if st.result == 1 {
            temp = out;
        } else {
            current = out;
        }
    }
    Some(current)
}

/// One stage operation on channels `..n` (3: colour; 4: alpha in [3]).
fn combine(op: u32, a1: [i32; 4], a2: [i32; 4], a0: [i32; 4], fa: i32, n: usize) -> [i32; 4] {
    let mut out = [0; 4];
    let range = if n == 3 { 0..3 } else { 3..4 };
    if op == top::DOTPRODUCT3 {
        // 4 x dot(arg1 - 0.5, arg2 - 0.5), into every channel.
        let d: i32 = (0..3).map(|k| (a1[k] - 128) * (a2[k] - 128)).sum();
        return [(d / 64).clamp(0, 255); 4];
    }
    for k in range {
        let (x, y, z) = (a1[k], a2[k], a0[k]);
        out[k] = match op {
            top::SELECTARG1 => x,
            top::SELECTARG2 => y,
            top::MODULATE => mul255(x, y),
            top::MODULATE2X => (mul255(x, y) * 2).min(255),
            top::MODULATE4X => (mul255(x, y) * 4).min(255),
            top::ADD => (x + y).min(255),
            top::ADDSIGNED => (x + y - 128).clamp(0, 255),
            top::ADDSIGNED2X => ((x + y - 128) * 2).clamp(0, 255),
            top::SUBTRACT => (x - y).max(0),
            top::ADDSMOOTH => (x + y - mul255(x, y)).min(255),
            top::BLENDDIFFUSEALPHA | top::BLENDTEXTUREALPHA | top::BLENDFACTORALPHA | top::BLENDCURRENTALPHA => {
                mul255(x, fa) + mul255(y, 255 - fa)
            }
            top::BLENDTEXTUREALPHAPM => (x + mul255(y, 255 - fa)).min(255),
            top::MODULATEALPHA_ADDCOLOR => if k < 3 { (x + mul255(a1[3], y)).min(255) } else { x },
            top::MODULATECOLOR_ADDALPHA => if k < 3 { (mul255(x, y) + a1[3]).min(255) } else { x },
            top::MODULATEINVALPHA_ADDCOLOR => if k < 3 { (mul255(255 - a1[3], y) + x).min(255) } else { x },
            top::MODULATEINVCOLOR_ADDALPHA => if k < 3 { (mul255(255 - x, y) + a1[3]).min(255) } else { x },
            top::MULTIPLYADD => (z + mul255(x, y)).min(255),
            top::LERP => mul255(z, x) + mul255(255 - z, y),
            // PREMODULATE, the bump maps: the first argument.
            _ => x,
        };
    }
    out
}

/// Texturing, specular, fog, the alpha, stencil and depth tests, blending
/// and the colour mask, for pixel (x, y).
#[inline(always)]
fn fragment(buf: Buffers, p: &Pass, x: usize, y: usize, fr: &Frag) {
    let at = y * buf.w + x;
    let dat = y * buf.dw + x;
    let g = &p.g;
    let flags = g.flags;
    if flags & F_POLY_STIPPLE != 0 {
        // In window coordinates, rows counted from the bottom.
        let yb = buf.h - 1 - y;
        if (g.stipple[yb & 31] >> (31 - (x & 31))) & 1 == 0 {
            return;
        }
    }
    let dptr = unsafe { buf.depth.add(dat) };
    let stencil = flags & F_STENCIL != 0;
    let depth_ok = flags & F_DEPTH_TEST == 0 || depth_pass(g.depth_func, fr.z, unsafe { *dptr });
    // Without a stencil test a failed depth test has no other effect: stop
    // before texturing.
    if !depth_ok && !stencil {
        return;
    }
    let Some(mut c) = stages(p, fr) else { return };
    if flags & F_SPECULAR != 0 {
        for i in 0..3 {
            c[i] = (c[i] + fr.spec[i]).min(255);
        }
    }
    if flags & F_FOG != 0 {
        let fc = [(g.fog_color >> 16 & 0xFF) as i32, (g.fog_color >> 8 & 0xFF) as i32, (g.fog_color & 0xFF) as i32];
        for i in 0..3 {
            c[i] = (c[i] * fr.f + fc[i] * (255 - fr.f)) / 255;
        }
    }
    if flags & F_ALPHA_TEST != 0 && !compare(g.alpha_func, c[3] as u32 * 2, g.alpha_ref) {
        return;
    }
    if stencil {
        let sptr = unsafe { buf.stencil.add(dat) };
        let s = unsafe { *sptr } as u32;
        let pass = compare(g.stencil_func, g.stencil_ref & g.stencil_mask, s & g.stencil_mask);
        let op = if !pass { g.stencil_ops[0] } else if !depth_ok { g.stencil_ops[1] } else { g.stencil_ops[2] };
        let n = match op {
            GL_KEEP => s,
            GL_ZERO => 0,
            GL_REPLACE => g.stencil_ref & 0xFF,
            GL_INCR => (s + 1).min(255),
            GL_DECR => s.saturating_sub(1),
            INCR_WRAP => (s + 1) & 0xFF,
            DECR_WRAP => s.wrapping_sub(1) & 0xFF,
            GL_INVERT => !s & 0xFF,
            _ => s,
        };
        let m = g.stencil_write & 0xFF;
        unsafe { *sptr = ((n & m) | (s & !m)) as u8 };
        if !pass || !depth_ok {
            return;
        }
    }
    if flags & F_DEPTH_TEST != 0 && flags & F_DEPTH_WRITE != 0 {
        unsafe { *dptr = fr.z };
    }
    let cptr = unsafe { buf.color.add(at) };
    let dst = unsafe { *cptr };
    if flags & F_BLEND != 0 && flags & F_LOGIC_OP == 0 {
        c = blend(g, c, dst);
    }
    let mut src = (c[3] as u32) << 24 | (c[0] as u32) << 16 | (c[1] as u32) << 8 | c[2] as u32;
    if flags & F_LOGIC_OP != 0 {
        src = logic(g.logic_op, src, dst);
    }
    // Without destination alpha the top byte stays 0.
    let keep = if flags & F_DST_ALPHA != 0 { 0xFFFF_FFFF } else { 0xFF_FFFF };
    let m = mask_bits(g.color_mask) & keep;
    unsafe { *cptr = (src & m) | (dst & !m & keep) };
}

fn draw_tri(buf: Buffers, t: &Tri, p: &Pass, used: u32, clip: [i32; 4]) {
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
    let (ry0, ry1) = prim_rows(t, &p.g);
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
    // Equal q (no perspective, e.g. an orthographic projection): texture
    // coordinates are linear in the window, no division needed.
    let affine = v[0].q == v[1].q && v[1].q == v[2].q;
    // Perspective-correct colours: interpolated times q, divided per pixel.
    let persp = g.flags & F_PERSPECTIVE != 0 && !affine;
    let pp = |f: &dyn Fn(&Vert) -> i64| -> Plane {
        let x = |v: &Vert| if persp { ((f(v) as i128 * v.q as i128) >> 32) as i64 } else { f(v) };
        plane(x(&v[0]), x(&v[1]), x(&v[2]))
    };
    let r = pp(&|v| v.r);
    let gr = pp(&|v| v.g);
    let b = pp(&|v| v.b);
    let a = pp(&|v| v.a);
    let specular = g.flags & F_SPECULAR != 0;
    let sp: [Plane; 3] = if specular { core::array::from_fn(|k| pp(&|v| v.spec[k])) } else { Default::default() };
    let fog_persp = persp && g.fog_mode == 0;
    let fog = if g.flags & F_FOG == 0 {
        Plane::default()
    } else if fog_persp {
        pp(&|v| v.f)
    } else {
        plane(v[0].f, v[1].f, v[2].f)
    };
    let uvp: [[Plane; 2]; SLOTS] = core::array::from_fn(|i| {
        if used & (1 << i) != 0 {
            [plane(v[0].uv[i][0], v[1].uv[i][0], v[2].uv[i][0]), plane(v[0].uv[i][1], v[1].uv[i][1], v[2].uv[i][1])]
        } else {
            Default::default()
        }
    });
    let q = if affine { Plane::default() } else { plane(v[0].q, v[1].q, v[2].q) };
    let fogged = g.flags & F_FOG != 0;
    // Mipmaps: the level of detail. Linear coordinates have it constant.
    let needs_lod: [bool; SLOTS] = core::array::from_fn(|i| used & (1 << i) != 0 && {
        let s = &g.samplers[i];
        s.mip != 0 || s.min != s.mag
    });
    let affine_lambda: [i32; SLOTS] = core::array::from_fn(|i| {
        if affine && needs_lod[i] { lod(uvp[i][0].dx, uvp[i][1].dx, uvp[i][0].dy, uvp[i][1].dy) } else { 0 }
    });

    let skip = (bx0 - ox) as i64;
    for py in by0..by1 {
        let row = (py - oy) as i64;
        let mut w0 = e12.e + e12.dy * row + e12.dx * skip;
        let mut w1 = e20.e + e20.dy * row + e20.dx * skip;
        let mut w2 = e01.e + e01.dy * row + e01.dx * skip;
        for px in bx0..bx1 {
            let inside = (w0 > 0 || (w0 == 0 && e12.incl)) && (w1 > 0 || (w1 == 0 && e20.incl)) && (w2 > 0 || (w2 == 0 && e01.incl));
            if inside {
                let col = (px - ox) as i64;
                // Texture coordinates (and, F_PERSPECTIVE, colours) were
                // sent times q (1/w), to be divided by it: recip = 2^62 / q.
                let recip = if affine || (used == 0 && !persp) { 0 } else { (1i64 << 62) / q.at(col, row).max(1) };
                let pc = |x: i64| if persp { ((x as i128 * recip as i128) >> 30) as i64 } else { x };
                let mut fr = Frag {
                    z: (z.at(col, row) >> 32).clamp(0, DEPTH_MAX as i64) as u32,
                    // Colours linear in the window (as Windows' OpenGL has
                    // them) unless F_PERSPECTIVE.
                    c: [chan(pc(r.at(col, row))), chan(pc(gr.at(col, row))), chan(pc(b.at(col, row))), chan(pc(a.at(col, row)))],
                    spec: if specular { [chan(pc(sp[0].at(col, row))), chan(pc(sp[1].at(col, row))), chan(pc(sp[2].at(col, row)))] } else { [0; 3] },
                    uv: [(0, 0, 0); SLOTS],
                    f: 255,
                };
                if fogged {
                    let fv = if fog_persp { pc(fog.at(col, row)) } else { fog.at(col, row) };
                    fr.f = if g.fog_mode == 0 { chan(fv) } else { fog_per_pixel(g, fv) };
                }
                for i in 0..SLOTS {
                    if used & (1 << i) == 0 {
                        continue;
                    }
                    let [uq, vq] = &uvp[i];
                    fr.uv[i] = if affine {
                        (uq.at(col, row) >> 16, vq.at(col, row) >> 16, affine_lambda[i])
                    } else {
                        // Sent times q (1/w): one division a pixel (recip =
                        // 2^30 / q) and a multiply per coordinate.
                        let qv = q.at(col, row).max(1);
                        let (uv, vv) = (uq.at(col, row), vq.at(col, row));
                        let u = ((uv as i128 * recip as i128) >> 46) as i64;
                        let v = ((vv as i128 * recip as i128) >> 46) as i64;
                        let lambda = if needs_lod[i] {
                            // d(uq / q)/dx = (uq_x q - uq q_x) / q^2, fx32
                            // texels: divided by q twice through recip.
                            let d = |val: i64, step: i64, dq: i64| -> i64 {
                                let num = (step as i128) * (qv as i128) - (val as i128) * (dq as i128);
                                let once = (num * recip as i128) >> 62;
                                ((once * recip as i128) >> 30) as i64
                            };
                            lod(d(uv, uq.dx, q.dx), d(vv, vq.dx, q.dx), d(uv, uq.dy, q.dy), d(vv, vq.dy, q.dy))
                        } else {
                            0
                        };
                        (u, v, lambda)
                    };
                }
                fragment(buf, p, px as usize, py as usize, &fr);
            }
            w0 += e12.dx;
            w1 += e20.dx;
            w2 += e01.dx;
        }
    }
}

/// Interpolates a line's or a point's values, linearly in the window.
fn lerp_frag(a: &Vert, b: &Vert, t: i64, used: u32, g: &Group) -> Frag {
    // t: 0 ..= 2^16.
    let l = |x: i64, y: i64| x + (((y - x) as i128 * t as i128) >> 16) as i64;
    let mut fr = Frag {
        z: (l(a.z, b.z) >> 32).clamp(0, DEPTH_MAX as i64) as u32,
        c: [chan(l(a.r, b.r)), chan(l(a.g, b.g)), chan(l(a.b, b.b)), chan(l(a.a, b.a))],
        spec: core::array::from_fn(|k| chan(l(a.spec[k], b.spec[k]))),
        uv: [(0, 0, 0); SLOTS],
        f: 255,
    };
    let q = l(a.q, b.q).max(1);
    if g.flags & F_FOG != 0 {
        let fv = l(a.f, b.f);
        fr.f = if g.fog_mode != 0 { fog_per_pixel(g, fv) } else { chan(fv) };
    }
    for i in 0..SLOTS {
        if used & (1 << i) != 0 {
            fr.uv[i] = (
                (((l(a.uv[i][0], b.uv[i][0]) as i128) << 16) / q as i128) as i64,
                (((l(a.uv[i][1], b.uv[i][1]) as i128) << 16) / q as i128) as i64,
                0,
            );
        }
    }
    fr
}

/// GL's lines without antialiasing, by its diamond-exit rule: along the
/// major axis, a pixel (or a run of `size` pixels across it) wherever the
/// line leaves the pixel's diamond (|dx| + |dy| < 1/2 around its centre):
/// so not the end's pixel if the end is inside its diamond, and the start's
/// pixel, if the start lies past its centre, only if the start is inside its
/// diamond. Across it, the pixel the line is in at the centre. Values along
/// it step from the start's at the first pixel drawn, as Windows does. A
/// stipple pattern counts pixels along from `stipple_at`.
fn draw_line(buf: Buffers, a: &Vert, b: &Vert, stipple_at: i64, p: &Pass, used: u32, clip: [i32; 4]) {
    let stipple = p.g.flags & F_LINE_STIPPLE != 0;
    let (pattern, factor) = ((p.g.line_stipple & 0xFFFF) as i64, ((p.g.line_stipple >> 16) as i64).max(1));
    let (dx, dy) = (b.x as i64 - a.x as i64, b.y as i64 - a.y as i64);
    if dx == 0 && dy == 0 {
        return;
    }
    let width = (p.g.size.max(SUB) as i64 + HALF as i64) / SUB as i64;
    let xmajor = dx.abs() >= dy.abs();
    // Major-axis coordinates of the ends, the minor coordinate along it.
    let (m0, m1, n0, dm, dn) = if xmajor { (a.x as i64, b.x as i64, a.y as i64, dx, dy) } else { (a.y as i64, b.y as i64, a.x as i64, dy, dx) };
    let s = SUB as i64;
    let h = HALF as i64;
    let step = if dm > 0 { 1 } else { -1 };
    // The pixels holding the start and the end, along the major axis.
    let (ps, pe) = (m0.div_euclid(s), m1.div_euclid(s));
    // The minor coordinate where the line crosses major coordinate `c`.
    let minor_at = |c: i64| n0 + ((dn as i128 * (c - m0) as i128) / dm as i128) as i64;
    let in_diamond = |pm: i64, pn: i64, i: i64| {
        let c = i * s + h;
        let j = minor_at(c).div_euclid(s);
        (pm - c).abs() + (pn - (j * s + h)).abs() < h
    };
    let n1 = n0 + dn;
    let mut first = None;
    let mut i = ps;
    loop {
        let c = i * s + h;
        // Has the line reached this pixel's centre? Before it (the start's
        // pixel when the start is past its centre) only if it starts inside
        // the diamond; the end's pixel not if it ends inside it.
        let before = (c - m0) * step < 0;
        let skip = (before && !in_diamond(m0, n0, i)) || (i == pe && in_diamond(m1, n1, i)) || (c - m1) * step > 0;
        let i0 = first.unwrap_or(i);
        // The stipple pattern counts pixels from the first one along.
        let stippled_out = stipple && !skip && (pattern >> (((stipple_at + (i - i0).abs()) / factor) % 16)) & 1 == 0;
        if !skip && !stippled_out {
            let i0 = *first.get_or_insert(i);
            // Where along the line, 0 ..= 2^16: the first pixel drawn has
            // the start's values, each pixel after adds 1/length.
            let t = ((((i - i0).abs() * s) as i128) << 16) / dm.abs() as i128;
            let n = minor_at(c.clamp(m0.min(m1), m0.max(m1)));
            // Pixels across whose centres are within (n - width/2, n + width/2].
            let lo = n - width * s / 2;
            let j0 = (lo - h).div_euclid(s) + 1;
            let fr = lerp_frag(a, b, t as i64, used, &p.g);
            for j in j0..j0 + width {
                let (px, py) = if xmajor { (i, j) } else { (j, i) };
                if px >= clip[0] as i64 && px < clip[2] as i64 && py >= clip[1] as i64 && py < clip[3] as i64 {
                    fragment(buf, p, px as usize, py as usize, &fr);
                }
            }
        }
        if !skip {
            first.get_or_insert(i);
        }
        if i == pe {
            break;
        }
        i += step;
    }
}

/// GL's points without antialiasing: the pixels whose centres are in the
/// `size` x `size` square around the point (half-open, as lines are).
fn draw_point(buf: Buffers, v: &Vert, p: &Pass, used: u32, clip: [i32; 4]) {
    let size = (p.g.size.max(SUB) as i64 + HALF as i64) / SUB as i64;
    let s = SUB as i64;
    let first = |c: i64| (c - size * s / 2 - HALF as i64).div_euclid(s) + 1;
    let (x0, y0) = (first(v.x as i64), first(v.y as i64));
    let fr = lerp_frag(v, v, 0, used, &p.g);
    for py in y0.max(clip[1] as i64)..(y0 + size).min(clip[3] as i64) {
        for px in x0.max(clip[0] as i64)..(x0 + size).min(clip[2] as i64) {
            fragment(buf, p, px as usize, py as usize, &fr);
        }
    }
}

/// The colour mask as bits of a 0xAARRGGBB pixel.
fn mask_bits(m: u32) -> u32 {
    (if m & 1 != 0 { 0xFF_0000 } else { 0 }) | (if m & 2 != 0 { 0xFF00 } else { 0 }) | (if m & 4 != 0 { 0xFF } else { 0 }) | (if m & 8 != 0 { 0xFF00_0000 } else { 0 })
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

/// e^-x, x and the result fx32.
fn exp_neg(x: i64) -> i64 {
    if x <= 0 {
        return 1 << 32;
    }
    if x >= 48i64 << 32 {
        return 0;
    }
    // 2^-(x log2 e) = 2^-n * e^-(r ln 2), r in [0, 1).
    let y = ((x as i128 * 6_196_328_018) >> 32) as i64;
    let n = y >> 32;
    let r = ((y & 0xFFFF_FFFF) as i128 * 2_977_044_472) >> 32; // r ln 2, fx32
    let (mut term, mut sum) = (1i128 << 32, 1i128 << 32);
    for k in 1..14 {
        term = -(term * r >> 32) / k;
        sum += term;
    }
    (sum >> n) as i64
}

/// How much of a fragment's colour fog keeps (0 ..= 255) at eye distance
/// `d` (fx32), per pixel.
fn fog_per_pixel(g: &Group, d: i64) -> i32 {
    let d = if g.fog_mode & FOG_INVERSE == 0 {
        d
    } else if d <= 0 {
        i64::MAX
    } else {
        ((1i128 << 64) / d as i128).min(i64::MAX as i128) as i64
    };
    let f = match g.fog_mode & 0xFFFF {
        0x2601 => ((g.fog_params[0] as i128 * d as i128) >> 32) as i64 + g.fog_params[1],
        0x801 => {
            let x = ((g.fog_params[0] as i128 * d as i128) >> 32) as i64;
            exp_neg(((x as i128 * x as i128) >> 32) as i64)
        }
        _ => exp_neg(((g.fog_params[0] as i128 * d as i128) >> 32) as i64),
    };
    ((f.clamp(0, 1 << 32) * 255) >> 32) as i32
}

/// log2 of a value with 32 fraction bits, with 8 fraction bits, as
/// Windows' software OpenGL takes it for the level of detail: the exponent,
/// plus the mantissa's fraction as is (log2(1 + m) ~ m).
fn log2_lin(x: u128) -> i32 {
    if x == 0 {
        return i32::MIN / 2;
    }
    let msb = 127 - x.leading_zeros() as i32;
    let frac = if msb >= 8 { ((x >> (msb - 8)) & 0xFF) as i32 } else { ((x << (8 - msb)) & 0xFF) as i32 };
    (msb - 32) * 256 + frac
}

/// The level of detail from the derivatives of the texture position
/// (texels per pixel, fx32): log2 of the larger of the two lengths, as
/// half the log2 of its square.
fn lod(dudx: i64, dvdx: i64, dudy: i64, dvdy: i64) -> i32 {
    let sq = |a: i64, b: i64| ((a as i128 * a as i128 + b as i128 * b as i128) >> 32) as u128;
    (log2_lin(sq(dudx, dvdx).max(sq(dudy, dvdy))) + 1) >> 1
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

/// A texel coordinate in 0 .. n by the address mode; None: outside, the
/// border colour.
fn address(i: i64, n: usize, mode: u32) -> Option<usize> {
    let n = n as i64;
    Some(match mode {
        CLAMP => i.clamp(0, n - 1),
        BORDER => {
            if i < 0 || i >= n {
                return None;
            }
            i
        }
        MIRROR => {
            let m = i.rem_euclid(2 * n);
            if m < n { m } else { 2 * n - 1 - m }
        }
        MIRRORONCE => {
            let m = if i < 0 { -1 - i } else { i };
            m.clamp(0, n - 1)
        }
        _ => i.rem_euclid(n),
    } as usize)
}

/// One level, nearest or bilinear, at (u, v) in that level's texels with 16
/// fraction bits, as [r, g, b, a].
fn sample_level(t: &Texture, s: &Sampler, l: &Level, u: i64, v: i64, nearest: bool) -> [i32; 4] {
    let get = |x: i64, y: i64| match (address(x, l.w, s.address[0]), address(y, l.h, s.address[1])) {
        (Some(x), Some(y)) => argb(t.texels[l.at + y * l.w + x]),
        _ => argb(s.border),
    };
    if nearest {
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

/// The texel colour at (u, v) in level-0 texels (16 fraction bits), with
/// level of detail `lambda` (8 fraction bits), as the sampler says.
fn sample(t: &Texture, s: &Sampler, u: i64, v: i64, lambda: i32) -> [i32; 4] {
    let first = (s.max_level as usize).min(t.levels.len() - 1);
    let at = |n: usize, nearest: bool| {
        let n = (n + first).min(t.levels.len() - 1);
        sample_level(t, s, &t.levels[n], u >> n, v >> n, nearest)
    };
    let lambda = lambda + s.lod_bias;
    // Where magnification ends: GL's 0.5 for a linear magnification over a
    // nearest minification with mipmaps, else 0.
    let c = if s.gl_rules != 0 && s.mag == FILTER_LINEAR && s.min == FILTER_POINT && s.mip != 0 { 128 } else { 0 };
    if lambda <= c || (s.mip == 0 && s.min == s.mag) {
        return at(0, s.mag == FILTER_POINT);
    }
    let nearest = s.min == FILTER_POINT;
    match s.mip {
        0 => at(0, nearest),
        FILTER_POINT => {
            // d = ceil(lambda + 1/2) - 1 for lambda > 1/2, else 0.
            let d = if lambda <= 128 { 0 } else { ((lambda + 128 + 255) >> 8) - 1 };
            at(d as usize, nearest)
        }
        _ => {
            // Between levels floor(lambda) and the next.
            let d = (lambda >> 8) as usize;
            let w = lambda & 0xFF;
            if d + first + 1 >= t.levels.len() {
                return at(d, nearest);
            }
            let (a, b) = (at(d, nearest), at(d + 1, nearest));
            // Weighted in 255ths, truncated: as Windows' software OpenGL
            // blends the two levels.
            core::array::from_fn(|i| (a[i] * (255 - w) + b[i] * w) / 255)
        }
    }
}

/// GL_CLEAR ..= GL_SET on two pixels.
fn logic(op: u32, s: u32, d: u32) -> u32 {
    match op & 0xF {
        0x0 => 0,
        0x1 => s & d,
        0x2 => s & !d,
        0x3 => s,
        0x4 => !s & d,
        0x5 => d,
        0x6 => s ^ d,
        0x7 => s | d,
        0x8 => !(s | d),
        0x9 => !(s ^ d),
        0xA => !d,
        0xB => s | !d,
        0xC => !s,
        0xD => !s | d,
        0xE => !(s & d),
        _ => 0xFFFF_FFFF,
    }
}

/// A blend factor for channel `i` (3: alpha). Destination alpha is 1
/// unless the colour buffer keeps it (F_DST_ALPHA).
fn factor(g: &Group, f: u32, src: [i32; 4], dst: [i32; 4], i: usize) -> i32 {
    let k = argb(g.blend_color);
    match f {
        GL_ZERO => 0,
        GL_ONE => 255,
        GL_SRC_COLOR => src[i],
        GL_ONE_MINUS_SRC_COLOR => 255 - src[i],
        GL_SRC_ALPHA => src[3],
        GL_ONE_MINUS_SRC_ALPHA => 255 - src[3],
        GL_DST_ALPHA => dst[3],
        GL_ONE_MINUS_DST_ALPHA => 255 - dst[3],
        GL_DST_COLOR => dst[i],
        GL_ONE_MINUS_DST_COLOR => 255 - dst[i],
        GL_SRC_ALPHA_SATURATE => if i < 3 { src[3].min(255 - dst[3]) } else { 255 },
        GL_CONSTANT_COLOR => k[i],
        GL_ONE_MINUS_CONSTANT_COLOR => 255 - k[i],
        GL_CONSTANT_ALPHA => k[3],
        GL_ONE_MINUS_CONSTANT_ALPHA => 255 - k[3],
        _ => 255,
    }
}

fn blend(g: &Group, src: [i32; 4], dst: u32) -> [i32; 4] {
    let mut d = argb(dst);
    if g.flags & F_DST_ALPHA == 0 {
        d[3] = 255;
    }
    let mut out = src;
    // Alpha only matters if it's kept.
    let n = if g.flags & F_DST_ALPHA != 0 { 4 } else { 3 };
    for i in 0..n {
        let (s, dd) = (mul255(src[i], factor(g, g.src_blend, src, d, i)), mul255(d[i], factor(g, g.dst_blend, src, d, i)));
        out[i] = match g.blend_op {
            2 => (s - dd).max(0),
            3 => (dd - s).max(0),
            4 => src[i].min(d[i]),
            5 => src[i].max(d[i]),
            _ => (s + dd).min(255),
        };
    }
    out
}

/// How many texels TexArgs describes.
pub fn texel_count(a: &TexArgs) -> usize {
    levels(a.w, a.h, a.levels).iter().map(|l| l.w * l.h).sum()
}

/// A texture from its upload: `texels` as TexArgs describes them.
pub fn texture(a: &TexArgs, texels: Vec<u32>) -> Texture {
    Texture { levels: levels(a.w, a.h, a.levels), texels }
}

/// Write `src` (`a.w` x `a.h` values, rows packed) into the target as
/// PixelArgs says, straight (no fragment pipeline beyond the scissor and
/// masks).
pub fn write_pixels(buf: Buffers, a: &PixelArgs, src: &[u32]) {
    let m = mask_bits(a.mask);
    for row in 0..a.h.max(0) {
        let y = a.y + row;
        if y < a.scissor[1].max(0) || y >= a.scissor[3].min(buf.h as i32) {
            continue;
        }
        for col in 0..a.w.max(0) {
            let x = a.x + col;
            if x < a.scissor[0].max(0) || x >= a.scissor[2].min(buf.w as i32) {
                continue;
            }
            let at = y as usize * buf.w + x as usize;
            let v = src[(row * a.w + col) as usize];
            unsafe {
                match a.which {
                    1 => *buf.depth.add(at) = v.min(DEPTH_MAX),
                    2 => {
                        let s = buf.stencil.add(at);
                        let wm = (a.mask & 0xFF) as u8;
                        *s = (v as u8 & wm) | (*s & !wm);
                    }
                    _ => {
                        let c = buf.color.add(at);
                        *c = (v & m) | (*c & !m);
                    }
                }
            }
        }
    }
}

/// Read `a.w` x `a.h` depth (`which` 1) or stencil (2) values into `dst`,
/// rows packed; outside the target reads 0.
pub fn read_pixels(buf: Buffers, a: &PixelArgs, dst: &mut [u32]) {
    for row in 0..a.h.max(0) {
        for col in 0..a.w.max(0) {
            let (x, y) = (a.x + col, a.y + row);
            let v = if x < 0 || y < 0 || x >= buf.w as i32 || y >= buf.h as i32 {
                0
            } else {
                let at = y as usize * buf.w + x as usize;
                unsafe {
                    match a.which {
                        1 => *buf.depth.add(at),
                        2 => *buf.stencil.add(at) as u32,
                        _ => *buf.color.add(at),
                    }
                }
            };
            dst[(row * a.w + col) as usize] = v;
        }
    }
}

/// glClear on a whole target: colour inside `w` x `h`, depth and stencil
/// inside `dw` x `dh`.
pub fn clear(buf: Buffers, c: &ClearArgs) {
    let m = mask_bits(c.color_mask);
    let sm = (c.stencil_write & 0xFF) as u8;
    let span = |w: usize, h: usize| {
        let x0 = c.scissor[0].clamp(0, w as i32) as usize;
        let x1 = c.scissor[2].clamp(0, w as i32) as usize;
        let y0 = c.scissor[1].clamp(0, h as i32) as usize;
        let y1 = c.scissor[3].clamp(0, h as i32) as usize;
        (x0, x1.max(x0), y0, y1.max(y0))
    };
    if c.mask & 0x4000 != 0 {
        let (x0, x1, y0, y1) = span(buf.w, buf.h);
        for y in y0..y1 {
            let color = unsafe { core::slice::from_raw_parts_mut(buf.color.add(y * buf.w + x0), x1 - x0) };
            for p in color.iter_mut() {
                *p = (c.color & m) | (*p & !m);
            }
        }
    }
    let (x0, x1, y0, y1) = span(buf.dw, buf.dh);
    for y in y0..y1 {
        let at = y * buf.dw + x0;
        if c.mask & 0x100 != 0 {
            let depth = unsafe { core::slice::from_raw_parts_mut(buf.depth.add(at), x1 - x0) };
            depth.fill(c.depth.min(DEPTH_MAX));
        }
        if c.mask & 0x400 != 0 {
            let st = unsafe { core::slice::from_raw_parts_mut(buf.stencil.add(at), x1 - x0) };
            for s in st.iter_mut() {
                *s = (c.stencil as u8 & sm) | (*s & !sm);
            }
        }
    }
}
