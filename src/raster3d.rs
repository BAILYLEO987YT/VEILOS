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
/// Samplers a pixel shader can use (the first SLOTS serve the stages too).
pub const SAMPLERS: usize = 16;

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
/// A pixel shader (`ps`) colours the fragment instead of the texture
/// stages, from per-primitive planes in DrawArgs' `attrs` (`PsProgram`).
pub const F_SHADER: u32 = 32768;

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
    /// Pixel shaders' comparison sampling (sample_c): D3D11_COMPARISON
    /// (1 never ..= 8 always); 0: none.
    pub compare: u32,
    /// The texture holds 24-bit depth across red, green and blue (a depth
    /// buffer as a shader resource): sampled as that depth in red.
    pub packed_depth: u32,
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
    /// Stage i samples slot i's texture; a pixel shader any of them.
    pub samplers: [Sampler; SAMPLERS],
    /// D3DRS_TEXTUREFACTOR, 0xAARRGGBB.
    pub tfactor: u32,
    /// F_SHADER: the pixel shader's handle, how many float4 inputs it has,
    /// where in `attrs` this group's first primitive's planes are, and its
    /// constants (224 float4, then 16 int4 as raw bits), and its boolean
    /// constants (bit i: b_i).
    pub ps: u64,
    pub ps_inputs: u32,
    pub attr_first: u32,
    pub const_first: u32,
    pub ps_bools: u32,
    /// How many float4 constants the block has (0: Direct3D 9's 224).
    pub ps_consts: u32,
}

#[repr(C)]
pub struct DrawArgs {
    pub groups: u64,
    pub group_count: u32,
    pub tri_count: u32,
    pub tris: u64,
    /// Pixel shader data (f32): each F_SHADER group's primitives' planes
    /// (`PsProgram`), and its constants.
    pub attrs: u64,
    pub attr_len: u32,
    pub _pad: u32,
}

/// VEIL_GPU3D op 7: a pixel shader program, `count` PsIns at `code`.
#[repr(C)]
pub struct PsArgs {
    pub code: u64,
    pub count: u32,
    /// How many float4 inputs it reads (at most PS_INPUTS).
    pub inputs: u32,
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

/// A group with its textures (and pixel shader, and the batch's shader
/// data) looked up.
pub struct Pass<'a> {
    pub g: Group,
    pub tex: [Option<&'a Texture>; SAMPLERS],
    pub ps: Option<&'a PsProgram>,
    pub attrs: &'a [f32],
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
        let used = if g.flags & F_SHADER != 0 { 0 } else { p.slots_used() };
        // A pixel shader's primitives each have their planes.
        let shader = p.ps.filter(|_| g.flags & F_SHADER != 0);
        let rec_len = PS_HEADER + 12 * g.ps_inputs as usize;
        let _sse = shader.map(|_| SseGuard::new());
        for (i, t) in tris[first..end].iter().enumerate() {
            let rec = shader.and_then(|_| {
                let at = g.attr_first as usize + i * rec_len;
                p.attrs.get(at..at + rec_len)
            });
            // A shaded primitive without its planes isn't drawn.
            if shader.is_some() && rec.is_none() {
                continue;
            }
            match g.kind {
                KIND_TRIANGLES => draw_tri(buf, t, p, used, clip, rec),
                KIND_LINES => draw_line(buf, &t.v[0], &t.v[1], t.v[2].x as i64, p, used, clip, rec),
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
                    texel = sample(t, &g.samplers[i], u, v, lambda, false);
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

/// Texturing (or the pixel shader, with the primitive's planes `rec`),
/// specular, fog, the alpha, stencil and depth tests, blending and the
/// colour mask, for pixel (x, y).
#[inline(always)]
fn fragment(buf: Buffers, p: &Pass, x: usize, y: usize, fr: &Frag, rec: Option<&[f32]>) {
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
    let shader = match (p.ps, rec) {
        (Some(ps), Some(rec)) if flags & F_SHADER != 0 => Some((ps, rec)),
        _ => None,
    };
    // A shader that writes depth runs before the depth test.
    let mut z = fr.z;
    let mut shaded = None;
    if let Some((ps, rec)) = shader {
        if ps.writes_depth {
            let Some((c, d)) = ps.run(p, rec, x, y) else { return };
            z = d.unwrap_or(z);
            shaded = Some(c);
        }
    }
    let depth_ok = flags & F_DEPTH_TEST == 0 || depth_pass(g.depth_func, z, unsafe { *dptr });
    // Without a stencil test a failed depth test has no other effect: stop
    // before texturing.
    if !depth_ok && !stencil {
        return;
    }
    let color = match (shader, shaded) {
        (_, Some(c)) => Some(c),
        (Some((ps, rec)), None) => ps.run(p, rec, x, y).map(|r| r.0),
        _ => stages(p, fr),
    };
    let Some(mut c) = color else { return };
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
        unsafe { *dptr = z };
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

fn draw_tri(buf: Buffers, t: &Tri, p: &Pass, used: u32, clip: [i32; 4], rec: Option<&[f32]>) {
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
                fragment(buf, p, px as usize, py as usize, &fr, rec);
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
fn draw_line(buf: Buffers, a: &Vert, b: &Vert, stipple_at: i64, p: &Pass, used: u32, clip: [i32; 4], rec: Option<&[f32]>) {
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
                    fragment(buf, p, px as usize, py as usize, &fr, rec);
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
            fragment(buf, p, px as usize, py as usize, &fr, None);
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
/// `hp`: with 8 more fraction bits (0 ..= 255 x 256), for pixel shaders,
/// whose texels may become coordinates.
fn sample_level(t: &Texture, s: &Sampler, l: &Level, u: i64, v: i64, nearest: bool, hp: bool) -> [i32; 4] {
    let get = |x: i64, y: i64| match (address(x, l.w, s.address[0]), address(y, l.h, s.address[1])) {
        (Some(x), Some(y)) => argb(t.texels[l.at + y * l.w + x]),
        _ => argb(s.border),
    };
    if nearest {
        let c = get(u >> 16, v >> 16);
        return if hp { c.map(|x| x << 8) } else { c };
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
        out[i] = (top * (256 - fy) + bot * fy) >> if hp { 8 } else { 16 };
    }
    out
}

/// The texel colour at (u, v) in level-0 texels (16 fraction bits), with
/// level of detail `lambda` (8 fraction bits), as the sampler says.
fn sample(t: &Texture, s: &Sampler, u: i64, v: i64, lambda: i32, hp: bool) -> [i32; 4] {
    let first = (s.max_level as usize).min(t.levels.len() - 1);
    let at = |n: usize, nearest: bool| {
        let n = (n + first).min(t.levels.len() - 1);
        sample_level(t, s, &t.levels[n], u >> n, v >> n, nearest, hp)
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

// -----| pixel shaders (WIN32_DESIGN.md G5e2) |-----
//
// d3d9.dll turns a pixel shader's bytecode into PsIns (op 7 makes a
// program of them, checked here once), and gives each shaded primitive a
// record of planes in DrawArgs' `attrs`: [x0, y0, face, Qa, Qb, Qc], then
// per input four components' a, then b, then c (12 floats). At pixel
// centre (X, Y) (the rasterizer's coordinates, +0.5) with dx = X - x0,
// dy = Y - y0: Q = Qa + Qb dx + Qc dy, and an input is (a + b dx + c dy) / Q,
// so perspective-correct. A group's constants follow at `const_first`: 224
// float4, then 16 int4 (raw bits).
//
// The arithmetic runs on SSE, in inline assembly (the kernel is built
// soft-float, so Rust floats would be emulated); the transcendental
// functions are scalar. `SseGuard` saves and restores the FPU and SSE
// state around a band's shading: on the calling CPU it's the program's.

/// Inputs a pixel shader can have (ps_3_0's ten, colours, texcoords).
pub const PS_INPUTS: usize = 12;
pub const PS_CONSTS: usize = 224;
/// Floats before the inputs in a primitive's record: [x0, y0, face, Q
/// plane, Z plane] (Z, the window depth, linear in the window).
pub const PS_HEADER: usize = 9;
/// The most float4 constants a block can have.
pub const PS_CONSTS_MAX: usize = 4096;
/// Temporaries (Direct3D 11's indexable ones are the last of them).
pub const PS_TEMPS: usize = 64;

/// PsIns register files (bits 0-3 of an operand).
pub mod pf {
    pub const TEMP: u32 = 0;
    pub const INPUT: u32 = 1;
    pub const CONST: u32 = 2;
    pub const CONSTINT: u32 = 3;
    pub const CONSTBOOL: u32 = 4;
    pub const LOOP: u32 = 5;
    pub const PRED: u32 = 6;
    pub const COLOR: u32 = 7;
    pub const DEPTH: u32 = 8;
    /// vPos (0), vFace (1), SV_Position (2: centre x, y, window z, 1/w),
    /// SV_IsFrontFace (3: all ones or zero).
    pub const MISC: u32 = 9;
}

/// Direct3D 10/11 (SM4/SM5) operations the kernel runs, beyond Direct3D 9's
/// (which keep their numbers): 0x100 + the SM4 opcode where there is one,
/// on 32-bit patterns (integers, masks) or per-component floats.
pub mod sm4 {
    pub const AND: u16 = 0x101;
    pub const CONTINUE: u16 = 0x107;
    pub const DIV: u16 = 0x10E;
    pub const DP2: u16 = 0x10F;
    pub const EQ: u16 = 0x118;
    pub const EXP: u16 = 0x119;
    pub const FTOI: u16 = 0x11B;
    pub const FTOU: u16 = 0x11C;
    pub const GE: u16 = 0x11D;
    pub const IADD: u16 = 0x11E;
    pub const IEQ: u16 = 0x120;
    pub const IGE: u16 = 0x121;
    pub const ILT: u16 = 0x122;
    pub const IMAD: u16 = 0x123;
    pub const IMAX: u16 = 0x124;
    pub const IMIN: u16 = 0x125;
    pub const IMUL: u16 = 0x126;
    pub const INE: u16 = 0x127;
    pub const INEG: u16 = 0x128;
    pub const ISHL: u16 = 0x129;
    pub const ISHR: u16 = 0x12A;
    pub const ITOF: u16 = 0x12B;
    pub const LD: u16 = 0x12D;
    pub const LOG: u16 = 0x12F;
    pub const LT: u16 = 0x131;
    pub const MOVC: u16 = 0x137;
    pub const NE: u16 = 0x139;
    pub const NOT: u16 = 0x13B;
    pub const OR: u16 = 0x13C;
    pub const RESINFO: u16 = 0x13D;
    pub const ROUND_NE: u16 = 0x140;
    pub const ROUND_NI: u16 = 0x141;
    pub const ROUND_PI: u16 = 0x142;
    pub const ROUND_Z: u16 = 0x143;
    pub const RSQ: u16 = 0x144;
    pub const SAMPLE_C: u16 = 0x146;
    pub const SAMPLE_C_LZ: u16 = 0x147;
    pub const SAMPLE_D: u16 = 0x149;
    pub const SQRT: u16 = 0x14B;
    pub const UDIV: u16 = 0x14E;
    pub const ULT: u16 = 0x14F;
    pub const UGE: u16 = 0x150;
    pub const UMUL: u16 = 0x151;
    pub const UMAX: u16 = 0x153;
    pub const UMIN: u16 = 0x154;
    pub const USHR: u16 = 0x155;
    pub const UTOF: u16 = 0x156;
    pub const XOR: u16 = 0x157;
    pub const LOD: u16 = 0x16C;
    pub const GATHER4: u16 = 0x16D;
    pub const RCP: u16 = 0x181;
    pub const F32TOF16: u16 = 0x182;
    pub const F16TOF32: u16 = 0x183;
    pub const COUNTBITS: u16 = 0x186;
    pub const FIRSTBIT_HI: u16 = 0x187;
    pub const FIRSTBIT_LO: u16 = 0x188;
    pub const FIRSTBIT_SHI: u16 = 0x189;
    pub const UBFE: u16 = 0x18A;
    pub const IBFE: u16 = 0x18B;
    pub const BFREV: u16 = 0x18D;
    // Ours: sine and cosine apart, the remainder, and flow control tested
    // on a register (ctrl 1: when non-zero, 0: when zero).
    pub const SIN: u16 = 0x1F0;
    pub const COS: u16 = 0x1F1;
    pub const UREM: u16 = 0x1F2;
    pub const IF: u16 = 0x1F3;
    pub const BREAKC: u16 = 0x1F4;
    pub const CONTINUEC: u16 = 0x1F5;
    /// A loop without a count (and its end): left by BREAK or BREAKC.
    pub const LOOP: u16 = 0x1F6;
    pub const ENDLOOP: u16 = 0x1F7;
    pub const DISCARD: u16 = 0x1F8;
    pub const RETC: u16 = 0x1F9;
}

/// One pixel shader instruction. `op` is Direct3D's opcode (D3DSIO_*;
/// the matrix ones lowered to DP3/DP4 by the DLL; TEX is texld, ctrl 1
/// projected, 2 biased). Operands:
///   source: file 0-3, index 4-15, swizzle 16-23, modifier 24-27 (D3DSPSM),
///           bit 28 indexed by aL
///   dest:   file 0-3, index 4-15, mask 16-19, saturate 20, shift 24-27
///           (signed), bit 28 indexed by aL
///   pred:   0 none, else bit 31, swizzle 16-23, bit 24 negated
/// `jump`: for IF/IFC/ELSE/LOOP/REP/BREAK*, the matching ELSE/ENDIF or
/// loop end; for loop ends, the loop start; for CALL/CALLNZ, the label.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct PsIns {
    pub op: u16,
    pub ctrl: u8,
    pub sampler: u8,
    pub dst: u32,
    pub src: [u32; 3],
    pub pred: u32,
    pub jump: u32,
    /// Operands with bit 28 are indexed by this: 0, aL (Direct3D 9);
    /// else bit 31, a temporary (bits 4-15) and its component (16-17).
    pub rel: u32,
}

pub struct PsProgram {
    pub ins: Vec<PsIns>,
    pub inputs: u32,
    pub writes_depth: bool,
    /// Inputs a TEX reads its coordinates straight from (bit i): their
    /// derivatives give the level of detail.
    deriv: u32,
    /// Some TEX reads coordinates it worked out, or DSX/DSY asks: carry
    /// every register's derivatives along (forward differentiation).
    dual: bool,
}

mod sio {
    pub const MOV: u16 = 1;
    pub const ADD: u16 = 2;
    pub const SUB: u16 = 3;
    pub const MAD: u16 = 4;
    pub const MUL: u16 = 5;
    pub const RCP: u16 = 6;
    pub const RSQ: u16 = 7;
    pub const DP3: u16 = 8;
    pub const DP4: u16 = 9;
    pub const MIN: u16 = 10;
    pub const MAX: u16 = 11;
    pub const SLT: u16 = 12;
    pub const SGE: u16 = 13;
    pub const EXP: u16 = 14;
    pub const LOG: u16 = 15;
    pub const LIT: u16 = 16;
    pub const DST: u16 = 17;
    pub const LRP: u16 = 18;
    pub const FRC: u16 = 19;
    pub const CALL: u16 = 25;
    pub const CALLNZ: u16 = 26;
    pub const LOOP: u16 = 27;
    pub const RET: u16 = 28;
    pub const ENDLOOP: u16 = 29;
    pub const LABEL: u16 = 30;
    pub const POW: u16 = 32;
    pub const CRS: u16 = 33;
    pub const SGN: u16 = 34;
    pub const ABS: u16 = 35;
    pub const NRM: u16 = 36;
    pub const SINCOS: u16 = 37;
    pub const REP: u16 = 38;
    pub const ENDREP: u16 = 39;
    pub const IF: u16 = 40;
    pub const IFC: u16 = 41;
    pub const ELSE: u16 = 42;
    pub const ENDIF: u16 = 43;
    pub const BREAK: u16 = 44;
    pub const BREAKC: u16 = 45;
    pub const TEXKILL: u16 = 65;
    pub const TEX: u16 = 66;
    pub const CND: u16 = 80;
    pub const CMP: u16 = 88;
    pub const DP2ADD: u16 = 90;
    pub const DSX: u16 = 91;
    pub const DSY: u16 = 92;
    pub const SETP: u16 = 94;
    pub const TEXLDL: u16 = 95;
    pub const BREAKP: u16 = 96;
}

fn sources(op: u16) -> usize {
    use sio::*;
    if op >= 0x100 {
        use sm4::*;
        return match op {
            IMAD | MOVC | SAMPLE_D | UBFE | IBFE => 3,
            AND | OR | XOR | DIV | DP2 | EQ | NE | LT | GE | IADD | IMUL | IMAX | IMIN | UMAX | UMIN | ISHL | ISHR | USHR | UDIV | UREM | IEQ | INE | IGE
            | ILT | ULT | UGE | UMUL | SAMPLE_C | SAMPLE_C_LZ => 2,
            CONTINUE | LOOP | ENDLOOP => 0,
            _ => 1,
        };
    }
    match op {
        MAD | LRP | CND | CMP | DP2ADD => 3,
        ADD | SUB | MUL | DP3 | DP4 | MIN | MAX | SLT | SGE | DST | POW | CRS | TEX | TEXLDL | SETP | IFC | BREAKC | CALLNZ => 2,
        MOV | RCP | RSQ | EXP | LOG | LIT | FRC | SGN | ABS | NRM | SINCOS | TEXKILL | DSX | DSY | REP | IF | LOOP | BREAKP => 1,
        _ => 0,
    }
}

/// Check a pixel shader's instructions; None if they could do harm or
/// aren't ours to run.
pub fn ps_program(ins: &[PsIns], inputs: u32) -> Option<PsProgram> {
    use sio::*;
    if inputs as usize > PS_INPUTS || ins.is_empty() || ins.len() > 1 << 14 {
        return None;
    }
    let ok_src = |s: u32| {
        let (f, i) = (s & 15, (s >> 4) & 0xFFF);
        match f {
            pf::TEMP => i < PS_TEMPS as u32,
            pf::INPUT => i < inputs,
            pf::CONST => (i as usize) < PS_CONSTS_MAX,
            pf::CONSTINT | pf::CONSTBOOL => i < 16,
            pf::LOOP | pf::PRED => i == 0,
            pf::MISC => i < 4,
            _ => false,
        }
    };
    let mut deriv = 0;
    let mut dual = false;
    let mut writes_depth = false;
    for x in ins {
        let known = matches!(
            x.op,
            0 | MOV..=FRC | CALL..=LABEL | POW..=BREAKC | TEXKILL | TEX | CND | CMP | DP2ADD | DSX | DSY | SETP | TEXLDL | BREAKP
        ) || matches!(
            x.op,
            0x101 | 0x107 | 0x10E | 0x10F | 0x118..=0x12B | 0x12D | 0x12F | 0x131 | 0x137 | 0x139 | 0x13B..=0x13D | 0x140..=0x144
                | 0x146 | 0x147 | 0x149 | 0x14B | 0x14E..=0x151 | 0x153..=0x157 | 0x16C | 0x16D | 0x181..=0x183 | 0x186..=0x18B | 0x18D
                | 0x1F0..=0x1F9
        );
        if x.rel != 0 && (x.rel >> 31 == 0 || (x.rel >> 4) & 0xFFF >= PS_TEMPS as u32) {
            return None;
        }
        if !known || x.jump as usize > ins.len() || x.sampler as usize >= SAMPLERS {
            return None;
        }
        if !x.src[..sources(x.op)].iter().all(|&s| ok_src(s)) {
            return None;
        }
        let has_dst = !matches!(x.op, 0 | CALL | CALLNZ | LOOP | RET | ENDLOOP | LABEL | REP | ENDREP | IF | IFC | ELSE | ENDIF | BREAK | BREAKC | BREAKP | TEXKILL)
            && !matches!(x.op, sm4::CONTINUE | sm4::IF | sm4::BREAKC | sm4::CONTINUEC | sm4::LOOP | sm4::ENDLOOP | sm4::DISCARD | sm4::RETC);
        if has_dst {
            let (f, i) = (x.dst & 15, (x.dst >> 4) & 0xFFF);
            let ok = match f {
                pf::TEMP => i < PS_TEMPS as u32,
                pf::COLOR | pf::DEPTH | pf::PRED => i == 0,
                _ => false,
            };
            if !ok {
                return None;
            }
            writes_depth |= f == pf::DEPTH;
        }
        if matches!(x.op, TEX | sm4::SAMPLE_C | sm4::LOD | sm4::GATHER4) {
            if x.src[0] & 15 == pf::INPUT && x.src[0] & (1 << 28) == 0 && (x.src[0] >> 24) & 15 == 0 {
                deriv |= 1 << ((x.src[0] >> 4) & 0xFFF);
            } else {
                dual = true;
            }
        }
        dual |= matches!(x.op, DSX | DSY);
    }
    if dual {
        deriv = (1 << inputs) - 1;
    }
    Some(PsProgram { ins: ins.to_vec(), inputs, writes_depth, deriv, dual })
}

// -----| SSE |-----

type F4 = [f32; 4];

macro_rules! sse2 {
    ($op:literal, $a:expr, $b:expr) => {{
        let (a, b): (&F4, &F4) = ($a, $b);
        let mut o: F4 = [0.0; 4];
        unsafe {
            core::arch::asm!(
                "movups xmm0, [{a}]",
                "movups xmm1, [{b}]",
                concat!($op, " xmm0, xmm1"),
                "movups [{o}], xmm0",
                a = in(reg) a.as_ptr(),
                b = in(reg) b.as_ptr(),
                o = in(reg) o.as_mut_ptr(),
                out("xmm0") _,
                out("xmm1") _,
                options(nostack)
            );
        }
        o
    }};
}

fn add(a: &F4, b: &F4) -> F4 {
    sse2!("addps", a, b)
}
fn sub(a: &F4, b: &F4) -> F4 {
    sse2!("subps", a, b)
}
fn mul(a: &F4, b: &F4) -> F4 {
    sse2!("mulps", a, b)
}
fn div(a: &F4, b: &F4) -> F4 {
    sse2!("divps", a, b)
}
fn min4(a: &F4, b: &F4) -> F4 {
    sse2!("minps", a, b)
}
fn max4(a: &F4, b: &F4) -> F4 {
    sse2!("maxps", a, b)
}
fn sqrt4(a: &F4) -> F4 {
    sse2!("sqrtps", a, a)
}
fn and4(a: &F4, b: &F4) -> F4 {
    sse2!("andps", a, b)
}
/// All-ones where a < b.
fn lt(a: &F4, b: &F4) -> F4 {
    sse2!("cmpltps", a, b)
}
/// All-ones where a <= b.
fn le(a: &F4, b: &F4) -> F4 {
    sse2!("cmpleps", a, b)
}

fn splat(x: f32) -> F4 {
    [x; 4]
}

const ONE: F4 = [1.0; 4];
const ZERO: F4 = [0.0; 4];

/// The sum of a's components, in every component.
fn hsum(a: &F4) -> F4 {
    let mut o: F4 = [0.0; 4];
    unsafe {
        core::arch::asm!(
            "movups xmm0, [{a}]",
            "movaps xmm1, xmm0",
            "shufps xmm1, xmm1, 0x4E",
            "addps xmm0, xmm1",
            "movaps xmm1, xmm0",
            "shufps xmm1, xmm1, 0xB1",
            "addps xmm0, xmm1",
            "movups [{o}], xmm0",
            a = in(reg) a.as_ptr(),
            o = in(reg) o.as_mut_ptr(),
            out("xmm0") _,
            out("xmm1") _,
            options(nostack)
        );
    }
    o
}

fn dp4(a: &F4, b: &F4) -> F4 {
    hsum(&mul(a, b))
}

fn dp3(a: &F4, b: &F4) -> F4 {
    let mut m = mul(a, b);
    m[3] = 0.0;
    hsum(&m)
}

fn floor4(a: &F4) -> F4 {
    let mut o: F4 = [0.0; 4];
    unsafe {
        core::arch::asm!(
            "movups xmm0, [{a}]",
            "cvttps2dq xmm1, xmm0",
            "cvtdq2ps xmm1, xmm1",
            // Truncation went up for negatives: take one off there.
            "movaps xmm2, xmm0",
            "cmpltps xmm2, xmm1",
            "movups xmm3, [{one}]",
            "andps xmm2, xmm3",
            "subps xmm1, xmm2",
            "movups [{o}], xmm1",
            a = in(reg) a.as_ptr(),
            one = in(reg) ONE.as_ptr(),
            o = in(reg) o.as_mut_ptr(),
            out("xmm0") _,
            out("xmm1") _,
            out("xmm2") _,
            out("xmm3") _,
            options(nostack)
        );
    }
    o
}

/// Where `mask` is all ones, b; else c.
fn select(mask: &F4, b: &F4, c: &F4) -> F4 {
    core::array::from_fn(|i| if mask[i].to_bits() != 0 { b[i] } else { c[i] })
}

/// 0 ..= 1 to 0 ..= 255, rounded.
fn to_bytes(a: &F4) -> [i32; 4] {
    let mut o = [0i32; 4];
    let k255: F4 = [255.0; 4];
    let half: F4 = [0.5; 4];
    unsafe {
        core::arch::asm!(
            "movups xmm0, [{a}]",
            "movups xmm1, [{one}]",
            "minps xmm0, xmm1",
            "xorps xmm1, xmm1",
            "maxps xmm0, xmm1",
            "movups xmm1, [{k}]",
            "mulps xmm0, xmm1",
            "movups xmm1, [{h}]",
            "addps xmm0, xmm1",
            "cvttps2dq xmm0, xmm0",
            "movdqu [{o}], xmm0",
            a = in(reg) a.as_ptr(),
            one = in(reg) ONE.as_ptr(),
            k = in(reg) k255.as_ptr(),
            h = in(reg) half.as_ptr(),
            o = in(reg) o.as_mut_ptr(),
            out("xmm0") _,
            out("xmm1") _,
            options(nostack)
        );
    }
    o
}

/// 4 x i32 to floats.
fn from_ints(a: &[i32; 4]) -> F4 {
    let mut o: F4 = [0.0; 4];
    unsafe {
        core::arch::asm!(
            "movdqu xmm0, [{a}]",
            "cvtdq2ps xmm0, xmm0",
            "movups [{o}], xmm0",
            a = in(reg) a.as_ptr(),
            o = in(reg) o.as_mut_ptr(),
            out("xmm0") _,
            options(nostack)
        );
    }
    o
}

/// Truncated to integers (fixed point by scaling first).
fn to_ints(a: &F4) -> [i32; 4] {
    let mut o = [0i32; 4];
    unsafe {
        core::arch::asm!(
            "movups xmm0, [{a}]",
            "cvttps2dq xmm0, xmm0",
            "movdqu [{o}], xmm0",
            a = in(reg) a.as_ptr(),
            o = in(reg) o.as_mut_ptr(),
            out("xmm0") _,
            options(nostack)
        );
    }
    o
}

/// The FPU and SSE state saved (and MXCSR set: every exception masked,
/// round to nearest) while it lives.
pub struct SseGuard {
    area: alloc::boxed::Box<FxArea>,
}

#[repr(C, align(16))]
struct FxArea([u8; 512]);

impl SseGuard {
    pub fn new() -> SseGuard {
        let mut area = alloc::boxed::Box::new(FxArea([0; 512]));
        let csr: u32 = 0x1F80;
        unsafe {
            core::arch::asm!("fxsave64 [{a}]", "ldmxcsr [{c}]", a = in(reg) area.0.as_mut_ptr(), c = in(reg) &csr, options(nostack));
        }
        SseGuard { area }
    }
}

impl Drop for SseGuard {
    fn drop(&mut self) {
        unsafe { core::arch::asm!("fxrstor64 [{a}]", a = in(reg) self.area.0.as_ptr(), options(nostack)) };
    }
}

// -----| scalar functions |-----

fn fl(x: f32) -> f32 {
    floor4(&splat(x))[0]
}

fn exp2f(x: f32) -> f32 {
    if x > 128.0 {
        return f32::INFINITY;
    }
    if x < -150.0 {
        return 0.0;
    }
    let n = fl(x);
    let f = (x - n) * core::f32::consts::LN_2;
    let (mut term, mut sum) = (1.0f32, 1.0f32);
    for k in 1..12 {
        term = term * f / k as f32;
        sum += term;
    }
    sum * f32::from_bits(((n as i32 + 127).clamp(1, 254) as u32) << 23)
}

fn log2f(x: f32) -> f32 {
    if !(x > 0.0) {
        return f32::NEG_INFINITY;
    }
    if x == f32::INFINITY {
        return x;
    }
    let bits = x.to_bits();
    let e = ((bits >> 23) & 0xFF) as i32 - 127;
    let m = f32::from_bits((bits & 0x7F_FFFF) | (127 << 23));
    let z = (m - 1.0) / (m + 1.0);
    let (mut t, mut s) = (z, 0.0f32);
    for k in 0..10 {
        s += t / (2 * k + 1) as f32;
        t = t * z * z;
    }
    2.0 * s * core::f32::consts::LOG2_E + e as f32
}

fn sincosf(x: f32) -> (f32, f32) {
    let tau = 2.0 * core::f32::consts::PI;
    let x = x - tau * fl(x / tau + 0.5);
    let (mut s, mut c, mut ts, mut tc) = (0.0f32, 0.0f32, x, 1.0f32);
    for k in 0..9 {
        s += ts;
        c += tc;
        ts = ts * -x * x / ((2 * k + 2) * (2 * k + 3)) as f32;
        tc = tc * -x * x / ((2 * k + 1) * (2 * k + 2)) as f32;
    }
    (s, c)
}

fn cmp_scalar(c: u8, a: f32, b: f32) -> bool {
    match c {
        1 => a > b,
        2 => a == b,
        3 => a >= b,
        4 => a < b,
        5 => a != b,
        6 => a <= b,
        7 => true,
        _ => false,
    }
}

// -----| running |-----

impl PsProgram {
    /// Run for pixel (x, y) of the primitive whose planes are `rec`: its
    /// colour (0 ..= 255) and depth (0 ..= DEPTH_MAX) if it writes one, or
    /// None if TEXKILL discards it.
    pub fn run(&self, p: &Pass, rec: &[f32], x: usize, y: usize) -> Option<([i32; 4], Option<u32>)> {
        use sio::*;
        let g = &p.g;
        let cf = g.const_first as usize;
        let nconst = if g.ps_consts == 0 { PS_CONSTS } else { (g.ps_consts as usize).min(PS_CONSTS_MAX) };
        let block = p.attrs.get(cf..cf + nconst * 4 + 64)?;
        let consts = &block[..nconst * 4];
        let ints = &block[nconst * 4..];
        // The inputs, perspective-correct, and the derivatives a TEX needs.
        let pos = from_ints(&[x as i32, y as i32, 0, 0]);
        let dx = pos[0] + 0.5 - rec[0];
        let dy = pos[1] + 0.5 - rec[1];
        let (vdx, vdy) = (splat(dx), splat(dy));
        let q = rec[3] + rec[4] * dx + rec[5] * dy;
        let iq = splat(if q != 0.0 { 1.0 / q } else { 0.0 });
        // Derivatives are taken at the centre of the pixel's 2x2 quad, as a
        // GPU's differences across the quad give them.
        let qpos = from_ints(&[(x & !1) as i32 + 1, (y & !1) as i32 + 1, 0, 0]);
        let (qdx, qdy) = (splat(qpos[0] - rec[0]), splat(qpos[1] - rec[1]));
        let qq = rec[3] + rec[4] * qdx[0] + rec[5] * qdy[0];
        let iqq = splat(if qq != 0.0 { 1.0 / qq } else { 0.0 });
        let mut inp = [ZERO; PS_INPUTS];
        let mut dd = [(ZERO, ZERO); PS_INPUTS];
        for i in 0..self.inputs as usize {
            let at = PS_HEADER + 12 * i;
            let pa: &F4 = rec[at..at + 4].try_into().ok()?;
            let pb: &F4 = rec[at + 4..at + 8].try_into().ok()?;
            let pc: &F4 = rec[at + 8..at + 12].try_into().ok()?;
            let v = mul(&add(&add(pa, &mul(pb, &vdx)), &mul(pc, &vdy)), &iq);
            if self.deriv & (1 << i) != 0 {
                let vq = mul(&add(&add(pa, &mul(pb, &qdx)), &mul(pc, &qdy)), &iqq);
                dd[i] = (mul(&sub(pb, &mul(&vq, &splat(rec[4]))), &iqq), mul(&sub(pc, &mul(&vq, &splat(rec[5]))), &iqq));
            }
            inp[i] = v;
        }
        let z = rec[6] + rec[7] * dx + rec[8] * dy;
        let front = f32::from_bits(if rec[2] >= 0.0 { 0xFFFF_FFFF } else { 0 });
        let misc = [[pos[0], pos[1], 0.0, 0.0], [rec[2]; 4], [pos[0] + 0.5, pos[1] + 0.5, z, q], [front; 4]];
        let mut r = [ZERO; PS_TEMPS];
        // Derivatives only for programs that need them.
        let mut rd_store;
        let rd: &mut [(F4, F4)] = if self.dual {
            rd_store = [(ZERO, ZERO); PS_TEMPS];
            &mut rd_store
        } else {
            &mut []
        };
        let mut color: F4 = [0.0, 0.0, 0.0, 1.0];
        let mut depth = None;
        let mut pred = [false; 4];
        let mut al = 0i32;
        let mut loops: [(usize, i32, i32, i32); 8] = [(0, 0, 0, 0); 8];
        let mut nl = 0;
        let mut calls = [0usize; 8];
        let mut nc = 0;
        let int_const = |n: u32| -> [i32; 4] { core::array::from_fn(|k| ints.get((n as usize & 15) * 4 + k).map_or(0, |f| f.to_bits() as i32)) };
        let bool_const = |n: u32| g.ps_bools & (1 << (n & 15)) != 0;
        let mut pc = 0usize;
        let mut steps = 0u32;
        while pc < self.ins.len() {
            steps += 1;
            if steps > 1 << 16 {
                break;
            }
            let x = &self.ins[pc];
            pc += 1;
            // A relative index: aL, or a temporary's component.
            let off = if x.rel != 0 { r[((x.rel >> 4) & 63) as usize][((x.rel >> 16) & 3) as usize].to_bits() as i32 } else { al };
            let read = |s: u32, r: &[F4; PS_TEMPS], pred: &[bool; 4], al: i32| -> F4 {
                let (f, mut i) = (s & 15, ((s >> 4) & 0xFFF) as i32);
                if s & (1 << 28) != 0 {
                    i = i.wrapping_add(al);
                }
                let v: F4 = match f {
                    pf::TEMP => r[(i as usize) & 63],
                    pf::INPUT => inp[(i.max(0) as usize).min(PS_INPUTS - 1)],
                    pf::CONST => {
                        // Out of range reads 0, as Direct3D 10's do.
                        if i < 0 || i as usize >= nconst {
                            ZERO
                        } else {
                            let i = i as usize * 4;
                            [consts[i], consts[i + 1], consts[i + 2], consts[i + 3]]
                        }
                    }
                    pf::CONSTINT => from_ints(&int_const(i as u32)),
                    pf::CONSTBOOL => splat(if bool_const(i as u32) { 1.0 } else { 0.0 }),
                    pf::LOOP => from_ints(&[al; 4]),
                    pf::PRED => pred.map(|b| if b { 1.0 } else { 0.0 }),
                    pf::MISC => misc[(i as usize) & 3],
                    _ => ZERO,
                };
                let sw = s >> 16;
                let v: F4 = core::array::from_fn(|k| v[((sw >> (2 * k)) & 3) as usize]);
                match (s >> 24) & 15 {
                    1 => v.map(|x| f32::from_bits(x.to_bits() ^ 0x8000_0000)),
                    2 => sub(&v, &splat(0.5)),
                    3 => sub(&splat(0.5), &v),
                    4 => mul(&sub(&v, &splat(0.5)), &splat(2.0)),
                    5 => mul(&sub(&v, &splat(0.5)), &splat(-2.0)),
                    6 => sub(&ONE, &v),
                    7 => add(&v, &v),
                    8 => sub(&ZERO, &add(&v, &v)),
                    9 => div(&v, &splat(v[2])),
                    10 => div(&v, &splat(v[3])),
                    11 => v.map(|x| f32::from_bits(x.to_bits() & 0x7FFF_FFFF)),
                    12 => v.map(|x| f32::from_bits(x.to_bits() | 0x8000_0000)),
                    _ => v,
                }
            };
            let n = sources(x.op);
            let mut s = [ZERO; 3];
            for k in 0..n {
                s[k] = read(x.src[k], &r, &pred, off);
            }
            // Their derivatives (dual programs only).
            let mut sd = [(ZERO, ZERO); 3];
            if self.dual {
                for k in 0..n {
                    sd[k] = read_d(x.src[k], &s[k], rd, &dd, off);
                }
            }
            match x.op {
                0 | LABEL => continue,
                IF => {
                    if !bool_const((x.src[0] >> 4) & 0xFFF) {
                        pc = x.jump as usize + 1;
                    }
                    continue;
                }
                IFC => {
                    if !cmp_scalar(x.ctrl & 7, s[0][0], s[1][0]) {
                        pc = x.jump as usize + 1;
                    }
                    continue;
                }
                ELSE => {
                    pc = x.jump as usize;
                    continue;
                }
                ENDIF => continue,
                REP | LOOP => {
                    let c = int_const((x.src[0] >> 4) & 0xFFF);
                    if c[0] <= 0 || nl == loops.len() {
                        pc = x.jump as usize + 1;
                    } else {
                        loops[nl] = (pc, c[0], al, if x.op == LOOP { c[2] } else { 0 });
                        nl += 1;
                        if x.op == LOOP {
                            al = c[1];
                        }
                    }
                    continue;
                }
                ENDREP | ENDLOOP => {
                    if nl > 0 {
                        let l = &mut loops[nl - 1];
                        l.1 -= 1;
                        if l.1 > 0 {
                            al += l.3;
                            pc = l.0;
                        } else {
                            al = l.2;
                            nl -= 1;
                        }
                    }
                    continue;
                }
                BREAK | BREAKC | BREAKP => {
                    let go = match x.op {
                        BREAK => true,
                        BREAKC => cmp_scalar(x.ctrl & 7, s[0][0], s[1][0]),
                        _ => pred[((x.src[0] >> 16) & 3) as usize] != ((x.src[0] >> 24) & 15 == 13),
                    };
                    if go && nl > 0 {
                        nl -= 1;
                        al = loops[nl].2;
                        pc = x.jump as usize + 1;
                    }
                    continue;
                }
                CALL | CALLNZ => {
                    let go = x.op == CALL || {
                        let b = x.src[1];
                        if b & 15 == pf::PRED { pred[((b >> 16) & 3) as usize] != ((b >> 24) & 15 == 13) } else { bool_const((b >> 4) & 0xFFF) }
                    };
                    if go && nc < calls.len() {
                        calls[nc] = pc;
                        nc += 1;
                        pc = x.jump as usize + 1;
                    }
                    continue;
                }
                RET => {
                    if nc == 0 {
                        break;
                    }
                    nc -= 1;
                    pc = calls[nc];
                    continue;
                }
                TEXKILL => {
                    if s[0].iter().any(|&c| c < 0.0) {
                        return None;
                    }
                    continue;
                }
                sm4::IF => {
                    if (s[0][0].to_bits() != 0) != (x.ctrl & 1 != 0) {
                        pc = x.jump as usize + 1;
                    }
                    continue;
                }
                sm4::LOOP => {
                    if nl == loops.len() {
                        pc = x.jump as usize + 1;
                    } else {
                        loops[nl] = (pc, i32::MAX, al, 0);
                        nl += 1;
                    }
                    continue;
                }
                sm4::ENDLOOP => {
                    if nl > 0 {
                        pc = loops[nl - 1].0;
                    }
                    continue;
                }
                sm4::CONTINUE | sm4::CONTINUEC => {
                    if x.op == sm4::CONTINUE || (s[0][0].to_bits() != 0) == (x.ctrl & 1 != 0) {
                        pc = x.jump as usize;
                    }
                    continue;
                }
                sm4::BREAKC => {
                    if (s[0][0].to_bits() != 0) == (x.ctrl & 1 != 0) && nl > 0 {
                        nl -= 1;
                        al = loops[nl].2;
                        pc = x.jump as usize + 1;
                    }
                    continue;
                }
                sm4::DISCARD => {
                    if (s[0][0].to_bits() != 0) == (x.ctrl & 1 != 0) {
                        return None;
                    }
                    continue;
                }
                sm4::RETC => {
                    if (s[0][0].to_bits() != 0) == (x.ctrl & 1 != 0) {
                        if nc == 0 {
                            break;
                        }
                        nc -= 1;
                        pc = calls[nc];
                    }
                    continue;
                }
                _ => {}
            }
            let (a, b, c) = (&s[0], &s[1], &s[2]);
            let v: F4 = match x.op {
                MOV => *a,
                ADD => add(a, b),
                SUB => sub(a, b),
                MUL => mul(a, b),
                MAD => add(&mul(a, b), c),
                MIN => min4(a, b),
                MAX => max4(b, a),
                SLT => and4(&lt(a, b), &ONE),
                SGE => and4(&le(b, a), &ONE),
                DP3 => dp3(a, b),
                DP4 => dp4(a, b),
                DP2ADD => splat(a[0] * b[0] + a[1] * b[1] + c[0]),
                RCP => div(&ONE, &splat(a[0])),
                RSQ => div(&ONE, &sqrt4(&splat(f32::from_bits(a[0].to_bits() & 0x7FFF_FFFF)))),
                EXP => splat(exp2f(a[0])),
                LOG => splat(log2f(f32::from_bits(a[0].to_bits() & 0x7FFF_FFFF))),
                POW => splat(exp2f(b[0] * log2f(f32::from_bits(a[0].to_bits() & 0x7FFF_FFFF)))),
                LIT => {
                    let p = a[3].clamp(-128.0, 128.0);
                    [1.0, a[0].max(0.0), if a[0] > 0.0 && a[1] > 0.0 { exp2f(p * log2f(a[1])) } else { 0.0 }, 1.0]
                }
                DST => [1.0, a[1] * b[1], a[2], b[3]],
                LRP => add(&mul(a, &sub(b, c)), c),
                FRC => sub(a, &floor4(a)),
                CRS => [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0], 0.0],
                SGN => a.map(|x| if x > 0.0 { 1.0 } else if x < 0.0 { -1.0 } else { 0.0 }),
                ABS => a.map(|x| f32::from_bits(x.to_bits() & 0x7FFF_FFFF)),
                NRM => {
                    let l = dp3(a, a);
                    mul(a, &div(&ONE, &sqrt4(&l)))
                }
                SINCOS => {
                    let (sn, cs) = sincosf(a[0]);
                    [cs, sn, 0.0, 0.0]
                }
                CMP => select(&le(&ZERO, a), b, c),
                CND => select(&lt(&splat(0.5), a), b, c),
                SETP => core::array::from_fn(|k| if cmp_scalar(x.ctrl & 7, a[k], b[k]) { 1.0 } else { 0.0 }),
                DSX => sd[0].0,
                DSY => sd[0].1,
                TEX | TEXLDL => self.texld(p, x, a, &dd, &sd[0]),
                op if op >= 0x100 => self.sm4_op(p, x, &s, &sd, &dd),
                _ => ZERO,
            };
            let vd = if self.dual { derivative(x.op, &s, &sd, &v) } else { (ZERO, ZERO) };
            // The destination: shift, saturate, masked and predicated.
            let d = x.dst;
            let shift = (((d >> 24) & 15) as i32) << 28 >> 28;
            let scale = splat(if shift > 0 { (1 << shift) as f32 } else { 1.0 / (1 << -shift) as f32 });
            let mut v = if shift != 0 { mul(&v, &scale) } else { v };
            let mut vd = if shift != 0 { (mul(&vd.0, &scale), mul(&vd.1, &scale)) } else { vd };
            if d & (1 << 20) != 0 {
                // Clamped: no slope where it was cut.
                let inside = and4(&le(&ZERO, &v), &le(&v, &ONE));
                vd = (select(&inside, &vd.0, &ZERO), select(&inside, &vd.1, &ZERO));
                v = max4(&min4(&v, &ONE), &ZERO);
            }
            let mut mask = (d >> 16) & 15;
            if x.pred != 0 {
                for k in 0..4 {
                    if pred[((x.pred >> (16 + 2 * k)) & 3) as usize] == ((x.pred >> 24) & 1 != 0) {
                        mask &= !(1 << k);
                    }
                }
            }
            let (f, mut i) = (d & 15, ((d >> 4) & 0xFFF) as i32);
            if d & (1 << 28) != 0 {
                i = i.wrapping_add(off);
            }
            for k in 0..4 {
                if mask & (1 << k) == 0 {
                    continue;
                }
                match f {
                    pf::TEMP => {
                        r[(i as usize) & 63][k] = v[k];
                        if let Some(e) = rd.get_mut((i as usize) & 63) {
                            e.0[k] = vd.0[k];
                            e.1[k] = vd.1[k];
                        }
                    }
                    pf::COLOR => color[k] = v[k],
                    pf::DEPTH => depth = Some(v[0]),
                    pf::PRED => pred[k] = v[k] != 0.0,
                    _ => {}
                }
            }
        }
        let depth = depth.map(|z| (z.clamp(0.0, 1.0) * DEPTH_MAX as f32) as u32);
        Some((to_bytes(&color), depth))
    }

    /// texld (TEX: projected, biased) and texldl: the sampler's texel at
    /// the coordinates `c`, 0 ..= 1.
    fn texld(&self, p: &Pass, x: &PsIns, c: &F4, dd: &[(F4, F4); PS_INPUTS], cd: &(F4, F4)) -> F4 {
        let s = x.sampler as usize;
        let Some(t) = p.tex[s] else { return [0.0, 0.0, 0.0, 1.0] };
        let l0 = &t.levels[0];
        let (tw, th) = (l0.w as f32, l0.h as f32);
        let mut c = *c;
        let mut cd = *cd;
        if x.op == sio::TEX && x.ctrl & 1 != 0 && c[3] != 0.0 {
            // Projected: (c / w)' = (c' - (c / w) w') / w.
            let iw = splat(1.0 / c[3]);
            c = mul(&c, &iw);
            cd = (mul(&sub(&cd.0, &mul(&c, &splat(cd.0[3]))), &iw), mul(&sub(&cd.1, &mul(&c, &splat(cd.1[3]))), &iw));
        }
        // Level of detail: from the input's derivatives (texels per pixel),
        // or texldl's w.
        let lambda = if x.op == sio::TEXLDL {
            (c[3] * 256.0) as i32
        } else {
            let src = x.src[0];
            let direct = src & 15 == pf::INPUT && src & (1 << 28) == 0 && (src >> 24) & 15 == 0 && x.ctrl & 1 == 0;
            let (ddx, ddy) = if direct {
                let (ddx, ddy) = dd[(((src >> 4) & 0xFFF) as usize).min(PS_INPUTS - 1)];
                let sw = src >> 16;
                let pick = |v: &F4| -> F4 { core::array::from_fn(|k| v[((sw >> (2 * k)) & 3) as usize]) };
                (pick(&ddx), pick(&ddy))
            } else {
                cd
            };
            // log2 of the longer of the two steps across the texture, as
            // GPUs work it out.
            let (ux, vx, uy, vy) = (ddx[0] * tw, ddx[1] * th, ddy[0] * tw, ddy[1] * th);
            let len2 = (ux * ux + vx * vx).max(uy * uy + vy * vy);
            let base = if len2 > 0.0 { (0.5 * log2f(len2) * 256.0).max(-32768.0) as i32 } else { -32768 };
            base + if x.ctrl & 2 != 0 { (c[3] * 256.0) as i32 } else { 0 }
        };
        let smp = &p.g.samplers[s];
        if smp.packed_depth != 0 {
            // A depth buffer read as a texture: its depth, unfiltered.
            let k = to_ints(&[c[0] * tw, c[1] * th, 0.0, 0.0]);
            return [packed_depth(texel_at(t, smp, 0, k[0] as i64, k[1] as i64)), 0.0, 0.0, 1.0];
        }
        let u = to_ints(&[c[0] * tw * 256.0, c[1] * th * 256.0, 0.0, 0.0]);
        let px = sample(t, smp, (u[0] as i64) << 8, (u[1] as i64) << 8, lambda, true);
        let f = from_ints(&px);
        mul(&f, &splat(1.0 / (255.0 * 256.0)))
    }
}

/// A source's derivatives (d/dx, d/dy): the register's, swizzled, with the
/// modifier's slope.
fn read_d(s: u32, val: &F4, rd: &[(F4, F4)], dd: &[(F4, F4); PS_INPUTS], al: i32) -> (F4, F4) {
    let (f, mut i) = (s & 15, ((s >> 4) & 0xFFF) as i32);
    if s & (1 << 28) != 0 {
        i = i.wrapping_add(al);
    }
    let (dx, dy) = match f {
        pf::TEMP => rd.get((i as usize) & 63).copied().unwrap_or((ZERO, ZERO)),
        pf::INPUT => dd[(i.max(0) as usize).min(PS_INPUTS - 1)],
        _ => return (ZERO, ZERO),
    };
    let sw = s >> 16;
    let pick = |v: &F4| -> F4 { core::array::from_fn(|k| v[((sw >> (2 * k)) & 3) as usize]) };
    let (dx, dy) = (pick(&dx), pick(&dy));
    let k = match (s >> 24) & 15 {
        1 | 3 | 6 => -1.0,
        4 | 7 => 2.0,
        5 | 8 => -2.0,
        11 => return (val_sign(val, &dx), val_sign(val, &dy)),
        12 => {
            let (a, b) = (val_sign(val, &dx), val_sign(val, &dy));
            return (mul(&a, &splat(-1.0)), mul(&b, &splat(-1.0)));
        }
        _ => 1.0,
    };
    if k == 1.0 { (dx, dy) } else { (mul(&dx, &splat(k)), mul(&dy, &splat(k))) }
}

/// d|x| = sign(x) dx (val is already |x|; the sign is the source's).
fn val_sign(_val: &F4, d: &F4) -> F4 {
    *d
}

/// An instruction's result's derivatives from its sources' (s, sd) and
/// its result v; 0 for what doesn't vary smoothly.
fn derivative(op: u16, s: &[F4; 3], sd: &[(F4, F4); 3], v: &F4) -> (F4, F4) {
    use sio::*;
    let (a, b, c) = (&s[0], &s[1], &s[2]);
    let each = |f: &dyn Fn(&F4, &F4, &F4) -> F4| (f(&sd[0].0, &sd[1].0, &sd[2].0), f(&sd[0].1, &sd[1].1, &sd[2].1));
    match op {
        MOV | FRC => sd[0],
        ADD => each(&|da, db, _| add(da, db)),
        SUB => each(&|da, db, _| sub(da, db)),
        MUL => each(&|da, db, _| add(&mul(da, b), &mul(a, db))),
        MAD => each(&|da, db, dc| add(&add(&mul(da, b), &mul(a, db)), dc)),
        DP3 => each(&|da, db, _| add(&dp3(da, b), &dp3(a, db))),
        DP4 => each(&|da, db, _| add(&dp4(da, b), &dp4(a, db))),
        DP2ADD => each(&|da, db, dc| splat(da[0] * b[0] + a[0] * db[0] + da[1] * b[1] + a[1] * db[1] + dc[0])),
        LRP => each(&|da, db, dc| add(&add(&mul(da, &sub(b, c)), &mul(a, &sub(db, dc))), dc)),
        MIN | MAX => {
            let first = if op == MIN { le(a, b) } else { le(b, a) };
            (select(&first, &sd[0].0, &sd[1].0), select(&first, &sd[0].1, &sd[1].1))
        }
        CMP => {
            let m = le(&ZERO, a);
            (select(&m, &sd[1].0, &sd[2].0), select(&m, &sd[1].1, &sd[2].1))
        }
        CND => {
            let m = lt(&splat(0.5), a);
            (select(&m, &sd[1].0, &sd[2].0), select(&m, &sd[1].1, &sd[2].1))
        }
        ABS => {
            let neg = lt(a, &ZERO);
            let f = |d: &F4| select(&neg, &mul(d, &splat(-1.0)), d);
            (f(&sd[0].0), f(&sd[0].1))
        }
        // (1/x)' = -x' / x^2; (x^-1/2)' = -x' / (2 x^3/2); scalar.
        RCP => {
            let k = -v[0] * v[0];
            (splat(k * sd[0].0[0]), splat(k * sd[0].1[0]))
        }
        RSQ => {
            let k = -0.5 * v[0] * v[0] * v[0];
            (splat(k * sd[0].0[0]), splat(k * sd[0].1[0]))
        }
        EXP => {
            let k = v[0] * core::f32::consts::LN_2;
            (splat(k * sd[0].0[0]), splat(k * sd[0].1[0]))
        }
        LOG => {
            let k = if a[0] != 0.0 { core::f32::consts::LOG2_E / a[0] } else { 0.0 };
            (splat(k * sd[0].0[0]), splat(k * sd[0].1[0]))
        }
        POW => {
            // (a^b)' = a^b (b' ln a + b a' / a).
            let la = if a[0] > 0.0 { log2f(a[0]) * core::f32::consts::LN_2 } else { 0.0 };
            let ia = if a[0] != 0.0 { b[0] / a[0] } else { 0.0 };
            let f = |da: &F4, db: &F4| splat(v[0] * (db[0] * la + ia * da[0]));
            (f(&sd[0].0, &sd[1].0), f(&sd[0].1, &sd[1].1))
        }
        SINCOS => {
            let f = |d: &F4| [-v[1] * d[0], v[0] * d[0], 0.0, 0.0];
            (f(&sd[0].0), f(&sd[0].1))
        }
        NRM => {
            // (a / |a|)' = a' / |a| - a (a . a') / |a|^3.
            let il = div(&ONE, &sqrt4(&dp3(a, a)));
            let f = |d: &F4| {
                let k = mul(&dp3(a, d), &mul(&il, &il));
                mul(&sub(d, &mul(a, &k)), &il)
            };
            (f(&sd[0].0), f(&sd[0].1))
        }
        // Direct3D 10's, per component.
        sm4::DIV => {
            // (a / b)' = (a' - v b') / b.
            let f = |da: &F4, db: &F4| div(&sub(da, &mul(v, db)), b);
            (f(&sd[0].0, &sd[1].0), f(&sd[0].1, &sd[1].1))
        }
        sm4::DP2 => {
            let f = |da: &F4, db: &F4| splat(da[0] * b[0] + a[0] * db[0] + da[1] * b[1] + a[1] * db[1]);
            (f(&sd[0].0, &sd[1].0), f(&sd[0].1, &sd[1].1))
        }
        sm4::MOVC => {
            let m: F4 = core::array::from_fn(|k| f32::from_bits(if a[k].to_bits() != 0 { 0xFFFF_FFFF } else { 0 }));
            (select(&m, &sd[1].0, &sd[2].0), select(&m, &sd[1].1, &sd[2].1))
        }
        sm4::SQRT | sm4::RSQ | sm4::RCP | sm4::EXP | sm4::LOG | sm4::SIN | sm4::COS => {
            // The slope of each component's function at its source.
            let k: F4 = core::array::from_fn(|i| match op {
                sm4::SQRT => if v[i] != 0.0 { 0.5 / v[i] } else { 0.0 },
                sm4::RSQ => -0.5 * v[i] * v[i] * v[i],
                sm4::RCP => -v[i] * v[i],
                sm4::EXP => v[i] * core::f32::consts::LN_2,
                sm4::LOG => if a[i] != 0.0 { core::f32::consts::LOG2_E / a[i] } else { 0.0 },
                sm4::SIN => sincosf(a[i]).1,
                _ => -sincosf(a[i]).0,
            });
            (mul(&k, &sd[0].0), mul(&k, &sd[0].1))
        }
        _ => (ZERO, ZERO),
    }
}

// -----| Direct3D 10/11's operations |-----

fn bits(v: &F4) -> [u32; 4] {
    v.map(|f| f.to_bits())
}

fn from_bits(u: [u32; 4]) -> F4 {
    u.map(f32::from_bits)
}

fn mask(b: bool) -> u32 {
    if b { 0xFFFF_FFFF } else { 0 }
}

/// |f| below 2^23 (so it has a fraction), and not infinite or NaN.
fn has_fraction(f: f32) -> bool {
    (f.to_bits() & 0x7FFF_FFFF) < 0x4B00_0000
}

fn half_to_f32(h: u32) -> f32 {
    let (s, e, m) = ((h >> 15) & 1, (h >> 10) & 31, h & 1023);
    let b = if e == 0 {
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
    f32::from_bits(b)
}

fn f32_to_half(f: f32) -> u32 {
    let b = f.to_bits();
    let s = (b >> 16) & 0x8000;
    let e = ((b >> 23) & 255) as i32 - 127 + 15;
    let m = b & 0x7F_FFFF;
    if (b >> 23) & 255 == 255 {
        s | 0x7C00 | if m != 0 { 0x200 } else { 0 }
    } else if e >= 31 {
        s | 0x7C00
    } else if e <= 0 {
        s
    } else {
        // Rounded to nearest.
        let r = s | (e as u32) << 10 | (m >> 13);
        if m & 0x1000 != 0 { r + 1 } else { r }
    }
}

/// A texel of level `n`, as the sampler addresses it, 0xAARRGGBB.
fn texel_at(t: &Texture, s: &Sampler, n: usize, x: i64, y: i64) -> u32 {
    let l = &t.levels[n.min(t.levels.len() - 1)];
    match (address(x, l.w, s.address[0]), address(y, l.h, s.address[1])) {
        (Some(x), Some(y)) => t.texels[l.at + y * l.w + x],
        _ => s.border,
    }
}

/// 24-bit depth packed into red, green and blue.
fn packed_depth(v: u32) -> f32 {
    (v & 0xFF_FFFF) as f32 / 16_777_215.0
}

fn argb_f(v: u32) -> F4 {
    let k = 1.0 / 255.0;
    [(v >> 16 & 0xFF) as f32 * k, (v >> 8 & 0xFF) as f32 * k, (v & 0xFF) as f32 * k, (v >> 24) as f32 * k]
}

/// The level of detail of gradients `gx`, `gy` on a `tw` x `th` level, 8
/// fraction bits.
fn lambda_of(gx: &F4, gy: &F4, tw: f32, th: f32) -> i32 {
    let (ux, vx, uy, vy) = (gx[0] * tw, gx[1] * th, gy[0] * tw, gy[1] * th);
    let len2 = (ux * ux + vx * vx).max(uy * uy + vy * vy);
    if len2 > 0.0 { (0.5 * log2f(len2) * 256.0).max(-32768.0) as i32 } else { -32768 }
}

/// D3D11_COMPARISON_FUNC (1 never ..= 8 always) as cmp_scalar's: the
/// reference compared with the texel.
fn d3d_func(f: u32) -> u8 {
    match f {
        2 => 4, // less
        3 => 2, // equal
        4 => 6, // less or equal
        5 => 1, // greater
        6 => 5, // not equal
        7 => 3, // greater or equal
        8 => 7, // always
        _ => 0, // never
    }
}

impl PsProgram {
    /// The derivatives of coordinates source `src` (`cd`, or the input's
    /// when they come straight from one).
    fn coord_derivs(&self, src: u32, cd: &(F4, F4), dd: &[(F4, F4); PS_INPUTS]) -> (F4, F4) {
        let direct = src & 15 == pf::INPUT && src & (1 << 28) == 0 && (src >> 24) & 15 == 0;
        if direct {
            let (ddx, ddy) = dd[(((src >> 4) & 0xFFF) as usize).min(PS_INPUTS - 1)];
            let sw = src >> 16;
            let pick = |v: &F4| -> F4 { core::array::from_fn(|k| v[((sw >> (2 * k)) & 3) as usize]) };
            (pick(&ddx), pick(&ddy))
        } else {
            *cd
        }
    }

    /// One SM4 operation's result (`s`: its sources, `sd` their derivatives).
    fn sm4_op(&self, p: &Pass, x: &PsIns, s: &[F4; 3], sd: &[(F4, F4); 3], dd: &[(F4, F4); PS_INPUTS]) -> F4 {
        use sm4::*;
        let (a, b, c) = (bits(&s[0]), bits(&s[1]), bits(&s[2]));
        let (fa, fb) = (&s[0], &s[1]);
        let each = |f: &dyn Fn(u32, u32, u32) -> u32| from_bits(core::array::from_fn(|k| f(a[k], b[k], c[k])));
        let fcmp = |f: &dyn Fn(f32, f32) -> bool| from_bits(core::array::from_fn(|k| mask(f(fa[k], fb[k]))));
        match x.op {
            AND => each(&|a, b, _| a & b),
            OR => each(&|a, b, _| a | b),
            XOR => each(&|a, b, _| a ^ b),
            NOT => each(&|a, _, _| !a),
            IADD => each(&|a, b, _| a.wrapping_add(b)),
            INEG => each(&|a, _, _| a.wrapping_neg()),
            IMUL | UMUL => each(&|a, b, _| a.wrapping_mul(b)),
            IMAD => each(&|a, b, c| a.wrapping_mul(b).wrapping_add(c)),
            IMAX => each(&|a, b, _| (a as i32).max(b as i32) as u32),
            IMIN => each(&|a, b, _| (a as i32).min(b as i32) as u32),
            UMAX => each(&|a, b, _| a.max(b)),
            UMIN => each(&|a, b, _| a.min(b)),
            ISHL => each(&|a, b, _| a << (b & 31)),
            ISHR => each(&|a, b, _| ((a as i32) >> (b & 31)) as u32),
            USHR => each(&|a, b, _| a >> (b & 31)),
            UDIV => each(&|a, b, _| if b == 0 { u32::MAX } else { a / b }),
            UREM => each(&|a, b, _| if b == 0 { u32::MAX } else { a % b }),
            IEQ => each(&|a, b, _| mask(a == b)),
            INE => each(&|a, b, _| mask(a != b)),
            IGE => each(&|a, b, _| mask(a as i32 >= b as i32)),
            ILT => each(&|a, b, _| mask((a as i32) < b as i32)),
            UGE => each(&|a, b, _| mask(a >= b)),
            ULT => each(&|a, b, _| mask(a < b)),
            MOVC => each(&|a, b, c| if a != 0 { b } else { c }),
            EQ => fcmp(&|a, b| a == b),
            NE => fcmp(&|a, b| a != b),
            LT => fcmp(&|a, b| a < b),
            GE => fcmp(&|a, b| a >= b),
            FTOI => from_bits(fa.map(|f| (f as i32) as u32)),
            FTOU => from_bits(fa.map(|f| f as u32)),
            ITOF => from_ints(&a.map(|v| v as i32)),
            UTOF => a.map(|v| v as f32),
            DIV => div(fa, fb),
            DP2 => splat(fa[0] * fb[0] + fa[1] * fb[1]),
            EXP => fa.map(exp2f),
            LOG => fa.map(log2f),
            RSQ => div(&ONE, &sqrt4(fa)),
            SQRT => sqrt4(fa),
            RCP => div(&ONE, fa),
            ROUND_NI => floor4(fa),
            ROUND_PI => sub(&ZERO, &floor4(&sub(&ZERO, fa))),
            ROUND_Z => fa.map(|f| if has_fraction(f) { (f as i32) as f32 } else { f }),
            ROUND_NE => fa.map(|f| {
                if !has_fraction(f) {
                    return f;
                }
                let lo = fl(f);
                let d = f - lo;
                if d > 0.5 || (d == 0.5 && (lo as i32) & 1 == 1) { lo + 1.0 } else { lo }
            }),
            SIN => fa.map(|f| sincosf(f).0),
            COS => fa.map(|f| sincosf(f).1),
            BFREV => each(&|a, _, _| a.reverse_bits()),
            COUNTBITS => each(&|a, _, _| a.count_ones()),
            FIRSTBIT_HI => each(&|a, _, _| if a == 0 { u32::MAX } else { a.leading_zeros() }),
            FIRSTBIT_LO => each(&|a, _, _| if a == 0 { u32::MAX } else { a.trailing_zeros() }),
            FIRSTBIT_SHI => each(&|a, _, _| {
                let v = if (a as i32) < 0 { !a } else { a };
                if v == 0 { u32::MAX } else { v.leading_zeros() }
            }),
            UBFE | IBFE => {
                let signed = x.op == IBFE;
                each(&|w, o, v| {
                    let (w, o) = (w & 31, o & 31);
                    if w == 0 {
                        0
                    } else if w + o < 32 {
                        let l = v << (32 - w - o);
                        if signed { ((l as i32) >> (32 - w)) as u32 } else { l >> (32 - w) }
                    } else if signed {
                        ((v as i32) >> o) as u32
                    } else {
                        v >> o
                    }
                })
            }
            F32TOF16 => from_bits(fa.map(f32_to_half)),
            F16TOF32 => a.map(|h| half_to_f32(h & 0xFFFF)),
            SAMPLE_D | SAMPLE_C | SAMPLE_C_LZ | LD | RESINFO | GATHER4 | LOD => self.sample_op(p, x, s, &sd[0], dd),
            _ => ZERO,
        }
    }

    /// The SM4 sampling operations.
    fn sample_op(&self, p: &Pass, x: &PsIns, s: &[F4; 3], cd: &(F4, F4), dd: &[(F4, F4); PS_INPUTS]) -> F4 {
        use sm4::*;
        let si = (x.sampler as usize).min(SAMPLERS - 1);
        let smp = &p.g.samplers[si];
        let Some(t) = p.tex[si] else { return ZERO };
        let first = (smp.max_level as usize).min(t.levels.len() - 1);
        let l0 = &t.levels[0];
        let (tw, th) = (l0.w as f32, l0.h as f32);
        let c = &s[0];
        match x.op {
            RESINFO => {
                let m = (s[0][0].to_bits() as usize).saturating_add(first);
                let (w, h) = t.levels.get(m).map_or((0, 0), |l| (l.w as u32, l.h as u32));
                let n = (t.levels.len() - first) as u32;
                match x.ctrl & 3 {
                    2 => from_bits([w, h, 1, n]),
                    1 => [if w > 0 { 1.0 / w as f32 } else { 0.0 }, if h > 0 { 1.0 / h as f32 } else { 0.0 }, 1.0, n as f32],
                    _ => [w as f32, h as f32, 1.0, n as f32],
                }
            }
            LD => {
                let i = bits(c);
                let n = (i[3] as usize).saturating_add(first);
                if n >= t.levels.len() {
                    return ZERO;
                }
                let raw = Sampler { address: [BORDER, BORDER], border: 0, ..*smp };
                let v = texel_at(t, &raw, n, i[0] as i32 as i64, i[1] as i32 as i64);
                if smp.packed_depth != 0 { [packed_depth(v), 0.0, 0.0, 1.0] } else { argb_f(v) }
            }
            LOD => {
                let (gx, gy) = self.coord_derivs(x.src[0], cd, dd);
                let l = lambda_of(&gx, &gy, tw, th) as f32 / 256.0;
                let top = (t.levels.len() - 1 - first) as f32;
                [l.max(0.0).min(top), l, 0.0, 0.0]
            }
            GATHER4 | SAMPLE_C | SAMPLE_C_LZ => {
                // The four texels of the first level's bilinear footprint,
                // and where in it the coordinates fall.
                let l = &t.levels[first];
                let k = to_ints(&[c[0] * l.w as f32 * 256.0 - 128.0, c[1] * l.h as f32 * 256.0 - 128.0, 0.0, 0.0]);
                let (x0, y0) = ((k[0] >> 8) as i64, (k[1] >> 8) as i64);
                let (fx, fy) = ((k[0] & 255) as f32 / 256.0, (k[1] & 255) as f32 / 256.0);
                let q = [
                    texel_at(t, smp, first, x0, y0 + 1),
                    texel_at(t, smp, first, x0 + 1, y0 + 1),
                    texel_at(t, smp, first, x0 + 1, y0),
                    texel_at(t, smp, first, x0, y0),
                ];
                let val = |v: u32| if smp.packed_depth != 0 { packed_depth(v) } else { argb_f(v)[0] };
                if x.op == GATHER4 {
                    return q.map(val);
                }
                // Comparison: each texel against the reference, the results
                // filtered (percentage closer) or the nearest one taken.
                let r = s[1][0];
                let f = d3d_func(smp.compare);
                let pass = |v: u32| -> f32 { if cmp_scalar(f, r, val(v)) { 1.0 } else { 0.0 } };
                if smp.mag == FILTER_LINEAR {
                    let top = pass(q[3]) * (1.0 - fx) + pass(q[2]) * fx;
                    let bot = pass(q[0]) * (1.0 - fx) + pass(q[1]) * fx;
                    splat(top * (1.0 - fy) + bot * fy)
                } else {
                    let n = to_ints(&[c[0] * l.w as f32, c[1] * l.h as f32, 0.0, 0.0]);
                    splat(pass(texel_at(t, smp, first, n[0] as i64, n[1] as i64)))
                }
            }
            _ => {
                // sample_d: the level of detail from the gradients given.
                let lambda = lambda_of(&s[1], &s[2], tw, th);
                let u = to_ints(&[c[0] * tw * 256.0, c[1] * th * 256.0, 0.0, 0.0]);
                let px = sample(t, smp, (u[0] as i64) << 8, (u[1] as i64) << 8, lambda, true);
                mul(&from_ints(&px), &splat(1.0 / (255.0 * 256.0)))
            }
        }
    }
}
