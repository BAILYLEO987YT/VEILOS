//! glPushAttrib/glPopAttrib, glPushClientAttrib/glPopClientAttrib and
//! wglCopyContext (WIN32_DESIGN.md G5c). A push saves all the state the
//! groups could need; a pop puts back only the groups it was pushed with.

use alloc::vec::Vec;

use crate::*;

const CURRENT: u32 = 0x1;
const POINT: u32 = 0x2;
const LINE: u32 = 0x4;
const POLYGON: u32 = 0x8;
const POLYGON_STIPPLE: u32 = 0x10;
const PIXEL_MODE: u32 = 0x20;
const LIGHTING: u32 = 0x40;
const FOG: u32 = 0x80;
const DEPTH_BUFFER: u32 = 0x100;
const ACCUM: u32 = 0x200;
const STENCIL: u32 = 0x400;
const VIEWPORT: u32 = 0x800;
const TRANSFORM: u32 = 0x1000;
const ENABLE: u32 = 0x2000;
const COLOR_BUFFER: u32 = 0x4000;
const HINT: u32 = 0x8000;
const EVAL: u32 = 0x10000;
const LIST: u32 = 0x20000;
const TEXTURE: u32 = 0x40000;
const SCISSOR: u32 = 0x80000;

/// GL's minimum depth for both attribute stacks.
const DEPTH: usize = 16;

/// Everything an attribute group holds, copied whole.
#[derive(Clone)]
pub(crate) struct Saved {
    mask: u32,
    color: [f32; 4],
    normal: [f32; 3],
    tc: [[f32; 4]; 2],
    raster: pixels::Raster,
    edge_flag: bool,
    point_size: f32,
    line_width: f32,
    line_stipple: [u32; 2],
    cull_mode: u32,
    front_face: u32,
    polygon_mode: [u32; 2],
    poly_offset: [f32; 2],
    poly_stipple: [u32; 32],
    transfer: pixels::Transfer,
    read_front: bool,
    light: light::Lighting,
    shade_model: u32,
    fog: light::Fog,
    depth_func: u32,
    depth_mask: bool,
    clear_depth: f32,
    clear_accum: [f32; 4],
    stencil: [u32; 7],
    clear_stencil: u32,
    viewport: [i32; 4],
    depth_range: [f32; 2],
    matrix_mode: u32,
    clip_planes: [[f32; 4]; 6],
    caps: Vec<u32>,
    tex_on: [[bool; 2]; 2],
    texgen: [[TexGen; 4]; 2],
    alpha_func: u32,
    alpha_ref: f32,
    blend_src: u32,
    blend_dst: u32,
    logic_op: u32,
    draw_none: bool,
    draw_front: bool,
    clear_color: [f32; 4],
    color_mask: [bool; 4],
    perspective: bool,
    fog_nicest: bool,
    eval: eval::Eval,
    list_base: u32,
    unit: usize,
    bound: [[u32; 2]; 2],
    tex_env: [u32; 2],
    env_color: [[f32; 4]; 2],
    scissor: [i32; 4],
}

fn save(c: &Ctx, mask: u32) -> Saved {
    Saved {
        mask,
        color: c.color,
        normal: c.normal,
        tc: c.tc,
        raster: c.raster,
        edge_flag: c.edge_flag,
        point_size: c.point_size,
        line_width: c.line_width,
        line_stipple: c.line_stipple,
        cull_mode: c.cull_mode,
        front_face: c.front_face,
        polygon_mode: c.polygon_mode,
        poly_offset: c.poly_offset,
        poly_stipple: c.poly_stipple,
        transfer: c.transfer.clone(),
        read_front: c.read_front,
        light: c.light.clone(),
        shade_model: c.shade_model,
        fog: c.fog.clone(),
        depth_func: c.depth_func,
        depth_mask: c.depth_mask,
        clear_depth: c.clear_depth,
        clear_accum: c.clear_accum,
        stencil: c.stencil,
        clear_stencil: c.clear_stencil,
        viewport: c.viewport,
        depth_range: c.depth_range,
        matrix_mode: c.matrix_mode,
        clip_planes: c.clip_planes,
        caps: c.caps.clone(),
        tex_on: c.tex_on,
        texgen: c.texgen,
        alpha_func: c.alpha_func,
        alpha_ref: c.alpha_ref,
        blend_src: c.blend_src,
        blend_dst: c.blend_dst,
        logic_op: c.logic_op,
        draw_none: c.draw_none,
        draw_front: c.draw_front,
        clear_color: c.clear_color,
        color_mask: c.color_mask,
        perspective: c.perspective,
        fog_nicest: c.fog_nicest,
        eval: c.eval.clone(),
        list_base: c.lists.base,
        unit: c.unit,
        bound: c.bound,
        tex_env: c.tex_env,
        env_color: c.env_color,
        scissor: c.scissor,
    }
}

/// Caps each group restores (besides those ENABLE restores: all of them).
fn group_caps(group: u32) -> &'static [u32] {
    match group {
        LINE => &[GL_LINE_STIPPLE, 0x0B20],
        POLYGON => &[GL_CULL_FACE, GL_POLYGON_STIPPLE, GL_POLYGON_OFFSET_FILL, GL_POLYGON_OFFSET_LINE, GL_POLYGON_OFFSET_POINT, 0x0B41],
        LIGHTING => &[GL_LIGHTING, GL_COLOR_MATERIAL, GL_LIGHT0, GL_LIGHT0 + 1, GL_LIGHT0 + 2, GL_LIGHT0 + 3, GL_LIGHT0 + 4, GL_LIGHT0 + 5, GL_LIGHT0 + 6, GL_LIGHT0 + 7],
        FOG => &[GL_FOG],
        DEPTH_BUFFER => &[GL_DEPTH_TEST],
        STENCIL => &[GL_STENCIL_TEST],
        TRANSFORM => &[GL_NORMALIZE, GL_CLIP_PLANE0, GL_CLIP_PLANE0 + 1, GL_CLIP_PLANE0 + 2, GL_CLIP_PLANE0 + 3, GL_CLIP_PLANE0 + 4, GL_CLIP_PLANE0 + 5],
        COLOR_BUFFER => &[GL_ALPHA_TEST, GL_BLEND, GL_DITHER, GL_COLOR_LOGIC_OP],
        SCISSOR => &[GL_SCISSOR_TEST],
        _ => &[],
    }
}

fn set_caps(c: &mut Ctx, from: &[u32], caps: &[u32]) {
    for &cap in caps {
        let on = from.contains(&cap);
        c.caps.retain(|&x| x != cap);
        if on {
            c.caps.push(cap);
        }
        if (GL_LIGHT0..GL_LIGHT0 + 8).contains(&cap) {
            c.light.lights[(cap - GL_LIGHT0) as usize].on = on;
        }
    }
}

/// Put back the groups of `s.mask` (and only those) into `c`.
fn restore(c: &mut Ctx, s: &Saved) {
    let m = s.mask;
    for g in 0..20 {
        let group = 1u32 << g;
        if m & group != 0 {
            set_caps(c, &s.caps, group_caps(group));
        }
    }
    if m & CURRENT != 0 {
        c.color = s.color;
        c.normal = s.normal;
        c.tc = s.tc;
        c.raster = s.raster;
        c.edge_flag = s.edge_flag;
    }
    if m & POINT != 0 {
        c.point_size = s.point_size;
    }
    if m & LINE != 0 {
        c.line_width = s.line_width;
        c.line_stipple = s.line_stipple;
    }
    if m & POLYGON != 0 {
        c.cull_mode = s.cull_mode;
        c.front_face = s.front_face;
        c.polygon_mode = s.polygon_mode;
        c.poly_offset = s.poly_offset;
    }
    if m & POLYGON_STIPPLE != 0 {
        c.poly_stipple = s.poly_stipple;
    }
    if m & PIXEL_MODE != 0 {
        c.transfer = s.transfer.clone();
        c.read_front = s.read_front;
    }
    if m & LIGHTING != 0 {
        let ons: [bool; 8] = core::array::from_fn(|i| c.light.lights[i].on);
        c.light = s.light.clone();
        for (i, on) in ons.iter().enumerate() {
            c.light.lights[i].on = *on;
        }
        c.shade_model = s.shade_model;
    }
    if m & FOG != 0 {
        c.fog = s.fog.clone();
    }
    if m & DEPTH_BUFFER != 0 {
        c.depth_func = s.depth_func;
        c.depth_mask = s.depth_mask;
        c.clear_depth = s.clear_depth;
    }
    if m & ACCUM != 0 {
        c.clear_accum = s.clear_accum;
    }
    if m & STENCIL != 0 {
        c.stencil = s.stencil;
        c.clear_stencil = s.clear_stencil;
    }
    if m & VIEWPORT != 0 {
        c.viewport = s.viewport;
        c.depth_range = s.depth_range;
    }
    if m & TRANSFORM != 0 {
        c.matrix_mode = s.matrix_mode;
        c.clip_planes = s.clip_planes;
    }
    if m & ENABLE != 0 {
        c.caps = s.caps.clone();
        for i in 0..8 {
            c.light.lights[i].on = s.caps.contains(&(GL_LIGHT0 + i as u32));
        }
        c.tex_on = s.tex_on;
        for u in 0..2 {
            for k in 0..4 {
                c.texgen[u][k].on = s.texgen[u][k].on;
            }
        }
    }
    if m & COLOR_BUFFER != 0 {
        c.alpha_func = s.alpha_func;
        c.alpha_ref = s.alpha_ref;
        c.blend_src = s.blend_src;
        c.blend_dst = s.blend_dst;
        c.logic_op = s.logic_op;
        c.draw_none = s.draw_none;
        c.draw_front = s.draw_front;
        c.clear_color = s.clear_color;
        c.color_mask = s.color_mask;
    }
    if m & HINT != 0 {
        c.perspective = s.perspective;
        c.fog_nicest = s.fog_nicest;
    }
    if m & EVAL != 0 {
        c.eval = s.eval.clone();
    }
    if m & LIST != 0 {
        c.lists.base = s.list_base;
    }
    if m & TEXTURE != 0 {
        c.unit = s.unit;
        c.bound = s.bound;
        c.tex_env = s.tex_env;
        c.env_color = s.env_color;
        c.tex_on = s.tex_on;
        c.texgen = s.texgen;
    }
    if m & SCISSOR != 0 {
        c.scissor = s.scissor;
    }
}

/// wglCopyContext: `mask`'s groups from `src` into `dst`.
pub(crate) fn copy(src: &Ctx, dst: &mut Ctx, mask: u32) {
    dst.flush();
    restore(dst, &save(src, mask));
}

#[no_mangle]
pub extern "system" fn glPushAttrib(mask: u32) {
    rec!(glPushAttrib(mask));
    outside(|c| {
        if c.attrib_stack.len() >= DEPTH {
            return c.err(GL_STACK_OVERFLOW);
        }
        let s = save(c, mask);
        c.attrib_stack.push(s);
    })
}

#[no_mangle]
pub extern "system" fn glPopAttrib() {
    rec!(glPopAttrib());
    outside(|c| {
        let Some(s) = c.attrib_stack.pop() else { return c.err(GL_STACK_UNDERFLOW) };
        c.flush();
        restore(c, &s);
    })
}

/// The client state: pixel storage (bit 1) and vertex arrays (bit 2).
#[derive(Clone)]
pub(crate) struct ClientSaved {
    mask: u32,
    unpack: pixels::PixelStore,
    pack: pixels::PixelStore,
    arrays: [Array; 5],
    client_unit: usize,
}

#[no_mangle]
pub extern "system" fn glPushClientAttrib(mask: u32) {
    outside(|c| {
        if c.client_stack.len() >= DEPTH {
            return c.err(GL_STACK_OVERFLOW);
        }
        let s = ClientSaved { mask, unpack: c.unpack, pack: c.pack, arrays: c.arrays, client_unit: c.client_unit };
        c.client_stack.push(s);
    })
}

#[no_mangle]
pub extern "system" fn glPopClientAttrib() {
    outside(|c| {
        let Some(s) = c.client_stack.pop() else { return c.err(GL_STACK_UNDERFLOW) };
        if s.mask & 1 != 0 {
            c.unpack = s.unpack;
            c.pack = s.pack;
        }
        if s.mask & 2 != 0 {
            c.arrays = s.arrays;
            c.client_unit = s.client_unit;
        }
    })
}
