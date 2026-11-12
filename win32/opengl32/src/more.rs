//! The rest of GL 1.1's entry points (WIN32_DESIGN.md G5c): texture
//! coordinate generation, user clip planes, polygon offset, stipples, logic
//! ops, 1D textures and copying the framebuffer into textures, the getters,
//! interleaved arrays, edge flags, colour-index calls (there are no colour
//! index pixel formats, so they have nothing to act on), every variant of
//! the vertex, colour, normal, texture coordinate and rectangle calls, and
//! Windows' layer-plane, metafile and debug entry points.

use alloc::vec;
use alloc::vec::Vec;

use crate::ext::wglGetProcAddress;
use crate::*;

// -----| texture coordinate generation |-----

const GL_S: u32 = 0x2000;

fn tex_gen(coord: u32, name: u32, v: [f32; 4]) {
    rec!(tex_gen(coord, name, v));
    outside(|c| {
        if !(GL_S..GL_S + 4).contains(&coord) {
            return c.err(GL_INVALID_ENUM);
        }
        let k = (coord - GL_S) as usize;
        let u = c.unit;
        let mv = *c.stacks[0].last().unwrap();
        let g = &mut c.texgen[u][k];
        match name {
            GL_TEXTURE_GEN_MODE => {
                let m = v[0] as u32;
                // A sphere map makes only s and t.
                if matches!(m, GL_OBJECT_LINEAR | GL_EYE_LINEAR) || (m == GL_SPHERE_MAP && k < 2) {
                    g.mode = m;
                } else {
                    c.err(GL_INVALID_ENUM);
                }
            }
            GL_OBJECT_PLANE => g.object = v,
            // Kept in eye coordinates: through the modelview's inverse now.
            GL_EYE_PLANE => g.eye = plane_through(v, &mv),
            _ => c.err(GL_INVALID_ENUM),
        }
    })
}

unsafe fn vals<T: Copy + Into<f64>>(name: u32, v: *const T) -> [f32; 4] {
    let n = if name == GL_TEXTURE_GEN_MODE { 1 } else { 4 };
    core::array::from_fn(|i| if i < n { (unsafe { *v.add(i) }).into() as f32 } else { 0.0 })
}

#[no_mangle] pub extern "system" fn glTexGeni(c: u32, n: u32, v: i32) { tex_gen(c, n, [v as f32, 0.0, 0.0, 0.0]) }
#[no_mangle] pub extern "system" fn glTexGenf(c: u32, n: u32, v: f32) { tex_gen(c, n, [v, 0.0, 0.0, 0.0]) }
#[no_mangle] pub extern "system" fn glTexGend(c: u32, n: u32, v: f64) { tex_gen(c, n, [v as f32, 0.0, 0.0, 0.0]) }
#[no_mangle] pub unsafe extern "system" fn glTexGeniv(c: u32, n: u32, v: *const i32) { tex_gen(c, n, unsafe { vals(n, v) }) }
#[no_mangle] pub unsafe extern "system" fn glTexGenfv(c: u32, n: u32, v: *const f32) { tex_gen(c, n, unsafe { vals(n, v) }) }
#[no_mangle] pub unsafe extern "system" fn glTexGendv(c: u32, n: u32, v: *const f64) { tex_gen(c, n, unsafe { vals(n, v) }) }

fn get_tex_gen(coord: u32, name: u32) -> [f64; 4] {
    with(|c| {
        if !(GL_S..GL_S + 4).contains(&coord) {
            c.err(GL_INVALID_ENUM);
            return [0.0; 4];
        }
        let g = c.texgen[c.unit][(coord - GL_S) as usize];
        match name {
            GL_TEXTURE_GEN_MODE => [g.mode as f64, 0.0, 0.0, 0.0],
            GL_OBJECT_PLANE => g.object.map(|x| x as f64),
            GL_EYE_PLANE => g.eye.map(|x| x as f64),
            _ => {
                c.err(GL_INVALID_ENUM);
                [0.0; 4]
            }
        }
    })
}

#[no_mangle] pub unsafe extern "system" fn glGetTexGeniv(c: u32, n: u32, out: *mut i32) { put(out, &get_tex_gen(c, n), if n == GL_TEXTURE_GEN_MODE { 1 } else { 4 }) }
#[no_mangle] pub unsafe extern "system" fn glGetTexGenfv(c: u32, n: u32, out: *mut f32) { put(out, &get_tex_gen(c, n), if n == GL_TEXTURE_GEN_MODE { 1 } else { 4 }) }
#[no_mangle] pub unsafe extern "system" fn glGetTexGendv(c: u32, n: u32, out: *mut f64) { put(out, &get_tex_gen(c, n), if n == GL_TEXTURE_GEN_MODE { 1 } else { 4 }) }

/// Something glGet* can write: f64 converted to it.
pub(crate) trait Out: Copy {
    fn from(v: f64) -> Self;
}
impl Out for i32 { fn from(v: f64) -> i32 { v as i32 } }
impl Out for f32 { fn from(v: f64) -> f32 { v as f32 } }
impl Out for f64 { fn from(v: f64) -> f64 { v } }
impl Out for u8 { fn from(v: f64) -> u8 { (v != 0.0) as u8 } }

pub(crate) fn put<T: Out>(out: *mut T, v: &[f64], n: usize) {
    if out.is_null() {
        return;
    }
    for (i, x) in v.iter().enumerate().take(n) {
        unsafe { *out.add(i) = T::from(*x) };
    }
}

// -----| clip planes, polygon offset, stipples, logic ops |-----

fn clip_plane(p: u32, eq: [f32; 4]) {
    rec!(clip_plane(p, eq));
    outside(|c| {
        if !(GL_CLIP_PLANE0..GL_CLIP_PLANE0 + 6).contains(&p) {
            return c.err(GL_INVALID_ENUM);
        }
        let mv = *c.stacks[0].last().unwrap();
        c.clip_planes[(p - GL_CLIP_PLANE0) as usize] = plane_through(eq, &mv);
    })
}

#[no_mangle]
pub unsafe extern "system" fn glClipPlane(p: u32, eq: *const f64) {
    clip_plane(p, core::array::from_fn(|i| unsafe { *eq.add(i) } as f32))
}

#[no_mangle]
pub unsafe extern "system" fn glGetClipPlane(p: u32, out: *mut f64) {
    let v = with(|c| {
        if !(GL_CLIP_PLANE0..GL_CLIP_PLANE0 + 6).contains(&p) {
            c.err(GL_INVALID_ENUM);
            return [0.0; 4];
        }
        c.clip_planes[(p - GL_CLIP_PLANE0) as usize].map(|x| x as f64)
    });
    put(out, &v, 4)
}

#[no_mangle]
pub extern "system" fn glPolygonOffset(factor: f32, units: f32) {
    rec!(glPolygonOffset(factor, units));
    outside(|c| c.poly_offset = [factor, units])
}

#[no_mangle]
pub unsafe extern "system" fn glPolygonStipple(mask: *const u8) {
    let st = with(|c| c.unpack);
    let rows: [u32; 32] = core::array::from_fn(|y| {
        let mut r = 0u32;
        for x in 0..32 {
            let bit = x + st.skip_pixels as usize;
            let len = if st.row_length > 0 { st.row_length as usize } else { 32 };
            let stride = len.div_ceil(8).div_ceil(st.align as usize) * st.align as usize;
            let byte = unsafe { *mask.add((y + st.skip_rows as usize) * stride + bit / 8) };
            let set = if st.lsb_first { byte >> (bit % 8) } else { byte >> (7 - bit % 8) } & 1;
            r |= (set as u32) << (31 - x);
        }
        r
    });
    polygon_stipple(rows)
}

fn polygon_stipple(rows: [u32; 32]) {
    rec!(polygon_stipple(rows));
    outside(|c| c.poly_stipple = rows)
}

#[no_mangle]
pub unsafe extern "system" fn glGetPolygonStipple(out: *mut u8) {
    let rows = with(|c| c.poly_stipple);
    for (y, r) in rows.iter().enumerate() {
        for b in 0..4 {
            unsafe { *out.add(y * 4 + b) = (r >> (24 - 8 * b)) as u8 };
        }
    }
}

#[no_mangle]
pub extern "system" fn glLineStipple(factor: i32, pattern: u16) {
    rec!(glLineStipple(factor, pattern));
    outside(|c| c.line_stipple = [factor.clamp(1, 256) as u32, pattern as u32])
}

#[no_mangle]
pub extern "system" fn glLogicOp(op: u32) {
    rec!(glLogicOp(op));
    outside(|c| if (0x1500..=0x150F).contains(&op) { c.logic_op = op } else { c.err(GL_INVALID_ENUM) })
}

// -----| 1D textures, copying into textures |-----

#[no_mangle]
pub unsafe extern "system" fn glTexImage1D(target: u32, level: i32, ifmt: i32, w: i32, border: i32, format: u32, kind: u32, pixels: *const u8) {
    outside(|c| {
        if target != GL_TEXTURE_1D {
            return c.err(GL_INVALID_ENUM);
        }
        let pow2 = |n: i32| n > 0 && n & (n - 1) == 0;
        if level < 0 || border != 0 || !pow2(w) || w > MAX_TEXTURE_SIZE || !valid_internal(ifmt) {
            return c.err(GL_INVALID_VALUE);
        }
        let texels = if pixels.is_null() { Some(vec![0u32; w as usize]) } else { unsafe { unpack(c, w, 1, format, kind, pixels) } };
        let Some(texels) = texels else { return c.err(GL_INVALID_ENUM) };
        let texels: Vec<u32> = texels.into_iter().map(|p| internal(ifmt, p)).collect();
        if c.lists.compiling.is_some() && c.lists.replaying == 0 {
            let _ = lists::record(alloc::boxed::Box::new(move || tex_image(1, level, w, 1, texels.clone())));
            return;
        }
        tex_image_in(c, 1, level, w, 1, texels);
    })
}

#[no_mangle]
pub unsafe extern "system" fn glTexSubImage1D(target: u32, level: i32, x: i32, w: i32, format: u32, kind: u32, pixels: *const u8) {
    outside(|c| {
        if target != GL_TEXTURE_1D {
            return c.err(GL_INVALID_ENUM);
        }
        let Some(src) = (unsafe { unpack(c, w.max(0), 1, format, kind, pixels) }) else { return c.err(GL_INVALID_ENUM) };
        if c.lists.compiling.is_some() && c.lists.replaying == 0 {
            let _ = lists::record(alloc::boxed::Box::new(move || {
                let src = src.clone();
                outside(|c| sub_image(c, 1, level, x, 0, w, 1, format, &src))
            }));
            return;
        }
        sub_image(c, 1, level, x, 0, w, 1, format, &src);
    })
}

/// The read buffer's pixels (x, y, w, h) as texels, the bottom row first
/// (a texture's t = 0), alpha 1.
fn framebuffer_texels(c: &mut Ctx, x: i32, y: i32, w: i32, h: i32, ifmt: i32) -> Vec<u32> {
    c.read_color(x, y, w, h).into_iter().map(|p| internal(ifmt, p | 0xFF00_0000)).collect()
}

fn copy_tex_image(dim: usize, level: i32, ifmt: i32, x: i32, y: i32, w: i32, h: i32, border: i32) {
    rec!(copy_tex_image(dim, level, ifmt, x, y, w, h, border));
    outside(|c| {
        let pow2 = |n: i32| n > 0 && n & (n - 1) == 0;
        if level < 0 || border != 0 || !pow2(w) || !pow2(h) || !valid_internal(ifmt) {
            return c.err(GL_INVALID_VALUE);
        }
        let texels = framebuffer_texels(c, x, y, w, h, ifmt);
        tex_image_in(c, dim, level, w, h, texels);
    })
}

#[no_mangle]
pub extern "system" fn glCopyTexImage2D(target: u32, level: i32, ifmt: u32, x: i32, y: i32, w: i32, h: i32, border: i32) {
    if target != GL_TEXTURE_2D {
        return with(|c| c.err(GL_INVALID_ENUM));
    }
    copy_tex_image(0, level, ifmt as i32, x, y, w, h, border)
}

#[no_mangle]
pub extern "system" fn glCopyTexImage1D(target: u32, level: i32, ifmt: u32, x: i32, y: i32, w: i32, border: i32) {
    if target != GL_TEXTURE_1D {
        return with(|c| c.err(GL_INVALID_ENUM));
    }
    copy_tex_image(1, level, ifmt as i32, x, y, w, 1, border)
}

fn copy_sub_image(dim: usize, level: i32, xo: i32, yo: i32, x: i32, y: i32, w: i32, h: i32) {
    rec!(copy_sub_image(dim, level, xo, yo, x, y, w, h));
    outside(|c| {
        let texels = framebuffer_texels(c, x, y, w, h, GL_RGBA as i32);
        // The texture's own format decides about alpha (sub_image keeps it).
        sub_image(c, dim, level, xo, yo, w, h, GL_RGB, &texels);
    })
}

#[no_mangle]
pub extern "system" fn glCopyTexSubImage2D(target: u32, level: i32, xo: i32, yo: i32, x: i32, y: i32, w: i32, h: i32) {
    if target != GL_TEXTURE_2D {
        return with(|c| c.err(GL_INVALID_ENUM));
    }
    copy_sub_image(0, level, xo, yo, x, y, w, h)
}

#[no_mangle]
pub extern "system" fn glCopyTexSubImage1D(target: u32, level: i32, xo: i32, x: i32, y: i32, w: i32) {
    if target != GL_TEXTURE_1D {
        return with(|c| c.err(GL_INVALID_ENUM));
    }
    copy_sub_image(1, level, xo, 0, x, y, w, 1)
}

// -----| texture getters |-----

#[no_mangle]
pub unsafe extern "system" fn glGetTexImage(target: u32, level: i32, format: u32, kind: u32, out: *mut u8) {
    let Some(dim) = dim_of(target) else { return with(|c| c.err(GL_INVALID_ENUM)) };
    let (texels, w, h) = with(|c| {
        let u = c.unit;
        let t = c.bound_of(u, dim);
        let l = level.max(0) as usize;
        let (w, h) = t.level_size(l);
        (t.mips.get(l).cloned().unwrap_or_default(), w as i32, h as i32)
    });
    if texels.len() != (w * h) as usize || out.is_null() {
        return;
    }
    // Written as glReadPixels writes, from 0xAARRGGBB texels.
    let comps = match format {
        GL_RGB | GL_BGR_EXT => 3,
        GL_RGBA | GL_BGRA_EXT => 4,
        GL_LUMINANCE_ALPHA => 2,
        _ => 1,
    };
    if kind != GL_UNSIGNED_BYTE && kind != 0x1406 {
        return with(|c| c.err(GL_INVALID_ENUM));
    }
    let st = with(|c| c.pack);
    let size = if kind == GL_UNSIGNED_BYTE { 1 } else { 4 };
    let row = ((w as usize * comps * size).div_ceil(st.align as usize)) * st.align as usize;
    for y in 0..h as usize {
        for x in 0..w as usize {
            let p = texels[y * w as usize + x];
            let f = [(p >> 16 & 0xFF) as f32 / 255.0, (p >> 8 & 0xFF) as f32 / 255.0, (p & 0xFF) as f32 / 255.0, (p >> 24) as f32 / 255.0];
            let v: [f32; 4] = match format {
                GL_BGR_EXT | GL_BGRA_EXT => [f[2], f[1], f[0], f[3]],
                GL_LUMINANCE => [f[0], 0.0, 0.0, 0.0],
                GL_LUMINANCE_ALPHA => [f[0], f[3], 0.0, 0.0],
                GL_ALPHA => [f[3], 0.0, 0.0, 0.0],
                0x1903 => [f[0], 0.0, 0.0, 0.0],
                0x1904 => [f[1], 0.0, 0.0, 0.0],
                0x1905 => [f[2], 0.0, 0.0, 0.0],
                _ => f,
            };
            for k in 0..comps {
                let at = y * row + (x * comps + k) * size;
                unsafe {
                    if size == 1 {
                        *out.add(at) = (v[k] * 255.0 + 0.5) as u8;
                    } else {
                        core::ptr::write_unaligned(out.add(at) as *mut f32, v[k]);
                    }
                }
            }
        }
    }
}

fn tex_parameter(target: u32, name: u32) -> [f64; 4] {
    let Some(dim) = dim_of(target) else {
        with(|c| c.err(GL_INVALID_ENUM));
        return [0.0; 4];
    };
    with(|c| {
        let u = c.unit;
        let t = c.bound_of(u, dim);
        match name {
            GL_TEXTURE_MIN_FILTER => [t.min as f64, 0.0, 0.0, 0.0],
            GL_TEXTURE_MAG_FILTER => [t.mag as f64, 0.0, 0.0, 0.0],
            GL_TEXTURE_WRAP_S => [t.wrap_s as f64, 0.0, 0.0, 0.0],
            GL_TEXTURE_WRAP_T => [t.wrap_t as f64, 0.0, 0.0, 0.0],
            // GL_TEXTURE_BORDER_COLOR, GL_TEXTURE_PRIORITY, GL_TEXTURE_RESIDENT
            0x1004 => [0.0; 4],
            0x8066 | 0x8067 => [1.0, 0.0, 0.0, 0.0],
            _ => {
                c.err(GL_INVALID_ENUM);
                [0.0; 4]
            }
        }
    })
}

#[no_mangle] pub unsafe extern "system" fn glGetTexParameteriv(t: u32, n: u32, out: *mut i32) { put(out, &tex_parameter(t, n), if n == 0x1004 { 4 } else { 1 }) }
#[no_mangle] pub unsafe extern "system" fn glGetTexParameterfv(t: u32, n: u32, out: *mut f32) { put(out, &tex_parameter(t, n), if n == 0x1004 { 4 } else { 1 }) }

fn tex_level(target: u32, level: i32, name: u32) -> f64 {
    let Some(dim) = dim_of(target) else {
        with(|c| c.err(GL_INVALID_ENUM));
        return 0.0;
    };
    with(|c| {
        let u = c.unit;
        let t = c.bound_of(u, dim);
        let l = level.max(0) as usize;
        let given = t.mips.get(l).is_some_and(|m| !m.is_empty());
        let (w, h) = if given { t.level_size(l) } else { (0, 0) };
        let alpha = given && t.mips[l].iter().any(|p| p >> 24 != 0xFF);
        match name {
            // WIDTH, HEIGHT, INTERNAL_FORMAT (COMPONENTS), BORDER
            0x1000 => w as f64,
            0x1001 => h as f64,
            0x1003 => if !given { 1.0 } else if alpha { GL_RGBA as f64 } else { GL_RGB as f64 },
            0x1005 => 0.0,
            // RED_SIZE .. ALPHA_SIZE, LUMINANCE_SIZE, INTENSITY_SIZE
            0x805C..=0x805E => if given { 8.0 } else { 0.0 },
            0x805F => if alpha { 8.0 } else { 0.0 },
            0x8060 | 0x8061 => 0.0,
            _ => {
                c.err(GL_INVALID_ENUM);
                0.0
            }
        }
    })
}

#[no_mangle] pub unsafe extern "system" fn glGetTexLevelParameteriv(t: u32, l: i32, n: u32, out: *mut i32) { put(out, &[tex_level(t, l, n)], 1) }
#[no_mangle] pub unsafe extern "system" fn glGetTexLevelParameterfv(t: u32, l: i32, n: u32, out: *mut f32) { put(out, &[tex_level(t, l, n)], 1) }

fn tex_env_get(target: u32, name: u32) -> [f64; 4] {
    with(|c| {
        if target != GL_TEXTURE_ENV {
            c.err(GL_INVALID_ENUM);
            return [0.0; 4];
        }
        match name {
            GL_TEXTURE_ENV_MODE => [c.tex_env[c.unit] as f64, 0.0, 0.0, 0.0],
            GL_TEXTURE_ENV_COLOR => c.env_color[c.unit].map(|x| x as f64),
            _ => {
                c.err(GL_INVALID_ENUM);
                [0.0; 4]
            }
        }
    })
}

#[no_mangle] pub unsafe extern "system" fn glGetTexEnviv(t: u32, n: u32, out: *mut i32) { put(out, &tex_env_get(t, n), if n == GL_TEXTURE_ENV_COLOR { 4 } else { 1 }) }
#[no_mangle] pub unsafe extern "system" fn glGetTexEnvfv(t: u32, n: u32, out: *mut f32) { put(out, &tex_env_get(t, n), if n == GL_TEXTURE_ENV_COLOR { 4 } else { 1 }) }

#[no_mangle]
pub unsafe extern "system" fn glPrioritizeTextures(_n: i32, _names: *const u32, _prio: *const f32) {}

/// Every texture is always resident.
#[no_mangle]
pub unsafe extern "system" fn glAreTexturesResident(n: i32, names: *const u32, out: *mut u8) -> u8 {
    with(|c| {
        for i in 0..n.max(0) as usize {
            let name = unsafe { *names.add(i) };
            if name == 0 || c.tex(name).is_none() {
                c.err(GL_INVALID_VALUE);
                return 0;
            }
        }
        for i in 0..n.max(0) as usize {
            unsafe { *out.add(i) = 1 };
        }
        1
    })
}

// -----| light and material getters |-----

fn light_get(n: u32, name: u32) -> [f64; 4] {
    with(|c| {
        if !(GL_LIGHT0..GL_LIGHT0 + 8).contains(&n) {
            c.err(GL_INVALID_ENUM);
            return [0.0; 4];
        }
        let l = c.light.lights[(n - GL_LIGHT0) as usize];
        let v4 = |v: [f32; 4]| v.map(|x| x as f64);
        let v1 = |x: f32| [x as f64, 0.0, 0.0, 0.0];
        match name {
            GL_AMBIENT => v4(l.ambient),
            GL_DIFFUSE => v4(l.diffuse),
            GL_SPECULAR => v4(l.specular),
            GL_POSITION => v4(l.position),
            GL_SPOT_DIRECTION => [l.spot_dir[0] as f64, l.spot_dir[1] as f64, l.spot_dir[2] as f64, 0.0],
            GL_SPOT_EXPONENT => v1(l.spot_exp),
            GL_SPOT_CUTOFF => v1(l.spot_cutoff),
            GL_CONSTANT_ATTENUATION => v1(l.atten[0]),
            GL_LINEAR_ATTENUATION => v1(l.atten[1]),
            GL_QUADRATIC_ATTENUATION => v1(l.atten[2]),
            _ => {
                c.err(GL_INVALID_ENUM);
                [0.0; 4]
            }
        }
    })
}

fn count_of(name: u32) -> usize {
    match name {
        GL_SPOT_DIRECTION => 3,
        GL_AMBIENT | GL_DIFFUSE | GL_SPECULAR | GL_POSITION | GL_EMISSION => 4,
        _ => 1,
    }
}

#[no_mangle] pub unsafe extern "system" fn glGetLightfv(l: u32, n: u32, out: *mut f32) { put(out, &light_get(l, n), count_of(n)) }
#[no_mangle] pub unsafe extern "system" fn glGetLightiv(l: u32, n: u32, out: *mut i32) { put(out, &light_get(l, n), count_of(n)) }

fn material_get(face: u32, name: u32) -> [f64; 4] {
    with(|c| {
        let f = match face {
            GL_FRONT => 0,
            GL_BACK => 1,
            _ => {
                c.err(GL_INVALID_ENUM);
                return [0.0; 4];
            }
        };
        let m = c.light.mat[f];
        let v4 = |v: [f32; 4]| v.map(|x| x as f64);
        match name {
            GL_AMBIENT => v4(m.ambient),
            GL_DIFFUSE => v4(m.diffuse),
            GL_SPECULAR => v4(m.specular),
            GL_EMISSION => v4(m.emission),
            GL_SHININESS => [m.shininess as f64, 0.0, 0.0, 0.0],
            // GL_COLOR_INDEXES
            0x1603 => [0.0, 1.0, 1.0, 0.0],
            _ => {
                c.err(GL_INVALID_ENUM);
                [0.0; 4]
            }
        }
    })
}

#[no_mangle] pub unsafe extern "system" fn glGetMaterialfv(f: u32, n: u32, out: *mut f32) { put(out, &material_get(f, n), if n == 0x1603 { 3 } else { count_of(n) }) }
#[no_mangle] pub unsafe extern "system" fn glGetMaterialiv(f: u32, n: u32, out: *mut i32) { put(out, &material_get(f, n), if n == 0x1603 { 3 } else { count_of(n) }) }

#[no_mangle]
pub unsafe extern "system" fn glGetPointerv(name: u32, out: *mut usize) {
    let p = with(|c| match name {
        0x808E => c.arrays[A_VERTEX].ptr,
        0x808F => c.arrays[A_NORMAL].ptr,
        0x8090 => c.arrays[A_COLOR].ptr,
        0x8092 => c.arrays[if c.client_unit == 1 { A_TEXCOORD2 } else { A_TEXCOORD }].ptr,
        // Index and edge flag arrays aren't kept; selection and feedback
        // buffers are.
        0x0DF0 => c.select.feedback_ptr,
        0x0DF3 => c.select.select_ptr,
        0x8091 | 0x8093 => 0,
        _ => {
            c.err(GL_INVALID_ENUM);
            0
        }
    });
    if !out.is_null() {
        unsafe { *out = p };
    }
}

// -----| interleaved arrays, edge flags |-----

#[no_mangle]
pub extern "system" fn glInterleavedArrays(format: u32, stride: i32, ptr: usize) {
    // (texture size, colour size and type, normal, vertex size) per format.
    let (t, c, ct, n, v): (i32, i32, u32, bool, i32) = match format {
        0x2A20 => (0, 0, 0, false, 2),                 // GL_V2F
        0x2A21 => (0, 0, 0, false, 3),                 // GL_V3F
        0x2A22 => (0, 4, GL_UNSIGNED_BYTE, false, 2),  // GL_C4UB_V2F
        0x2A23 => (0, 4, GL_UNSIGNED_BYTE, false, 3),  // GL_C4UB_V3F
        0x2A24 => (0, 3, 0x1406, false, 3),            // GL_C3F_V3F
        0x2A25 => (0, 0, 0, true, 3),                  // GL_N3F_V3F
        0x2A26 => (0, 4, 0x1406, true, 3),             // GL_C4F_N3F_V3F
        0x2A27 => (2, 0, 0, false, 3),                 // GL_T2F_V3F
        0x2A28 => (4, 0, 0, false, 4),                 // GL_T4F_V4F
        0x2A29 => (2, 4, GL_UNSIGNED_BYTE, false, 3),  // GL_T2F_C4UB_V3F
        0x2A2A => (2, 3, 0x1406, false, 3),            // GL_T2F_C3F_V3F
        0x2A2B => (2, 0, 0, true, 3),                  // GL_T2F_N3F_V3F
        0x2A2C => (2, 4, 0x1406, true, 3),             // GL_T2F_C4F_N3F_V3F
        0x2A2D => (4, 4, 0x1406, true, 4),             // GL_T4F_C4F_N3F_V4F
        _ => return with(|c| c.err(GL_INVALID_ENUM)),
    };
    let csize = if ct == GL_UNSIGNED_BYTE { 4 } else { c * 4 };
    let size = t * 4 + if c > 0 { csize } else { 0 } + if n { 12 } else { 0 } + v * 4;
    let stride = if stride == 0 { size } else { stride };
    with(|ctx| {
        let mut off = 0usize;
        let set = |a: &mut Array, on: bool, sz: i32, kind: u32, at: usize| {
            a.on = on;
            if on {
                a.size = sz;
                a.kind = kind;
                a.stride = stride;
                a.ptr = ptr + at;
            }
        };
        let tu = if ctx.client_unit == 1 { A_TEXCOORD2 } else { A_TEXCOORD };
        set(&mut ctx.arrays[tu], t > 0, t, 0x1406, off);
        off += (t * 4) as usize;
        set(&mut ctx.arrays[A_COLOR], c > 0, c, ct, off);
        off += if c > 0 { csize as usize } else { 0 };
        set(&mut ctx.arrays[A_NORMAL], n, 3, 0x1406, off);
        off += if n { 12 } else { 0 };
        set(&mut ctx.arrays[A_VERTEX], true, v, 0x1406, off);
    })
}

/// Edge flags only matter to polygons drawn as outlines; they're kept for
/// glGet but every edge is drawn.
#[no_mangle]
pub extern "system" fn glEdgeFlag(f: u8) {
    rec!(glEdgeFlag(f));
    with(|c| c.edge_flag = f != 0)
}

#[no_mangle]
pub unsafe extern "system" fn glEdgeFlagv(f: *const u8) {
    glEdgeFlag(unsafe { *f })
}

#[no_mangle]
pub extern "system" fn glEdgeFlagPointer(_stride: i32, _ptr: usize) {}

// -----| colour index mode: no pixel format has it |-----

#[no_mangle] pub extern "system" fn glIndexMask(_m: u32) {}
#[no_mangle] pub extern "system" fn glClearIndex(_c: f32) {}
#[no_mangle] pub extern "system" fn glIndexPointer(_kind: u32, _stride: i32, _ptr: usize) {}
#[no_mangle] pub extern "system" fn glIndexd(_c: f64) {}
#[no_mangle] pub extern "system" fn glIndexf(_c: f32) {}
#[no_mangle] pub extern "system" fn glIndexi(_c: i32) {}
#[no_mangle] pub extern "system" fn glIndexs(_c: i16) {}
#[no_mangle] pub extern "system" fn glIndexub(_c: u8) {}
#[no_mangle] pub extern "system" fn glIndexdv(_c: *const f64) {}
#[no_mangle] pub extern "system" fn glIndexfv(_c: *const f32) {}
#[no_mangle] pub extern "system" fn glIndexiv(_c: *const i32) {}
#[no_mangle] pub extern "system" fn glIndexsv(_c: *const i16) {}
#[no_mangle] pub extern "system" fn glIndexubv(_c: *const u8) {}

// -----| the variants |-----

fn b(x: i8) -> f32 { (2.0 * x as f32 + 1.0) / 255.0 }
fn us(x: u16) -> f32 { x as f32 / 65535.0 }
fn s(x: i16) -> f32 { (2.0 * x as f32 + 1.0) / 65535.0 }
fn ui(x: u32) -> f32 { (x as f64 / 4294967295.0) as f32 }
fn si(x: i32) -> f32 { ((2.0 * x as f64 + 1.0) / 4294967295.0) as f32 }

macro_rules! color_variants {
    ($($n3:ident, $n4:ident, $n3v:ident, $n4v:ident, $t:ty, $f:expr;)*) => {
        $(
            #[no_mangle] pub extern "system" fn $n3(r: $t, g: $t, b: $t) { let f = $f; glColor4f(f(r), f(g), f(b), 1.0) }
            #[no_mangle] pub extern "system" fn $n4(r: $t, g: $t, b: $t, a: $t) { let f = $f; glColor4f(f(r), f(g), f(b), f(a)) }
            #[no_mangle] pub unsafe extern "system" fn $n3v(v: *const $t) { let f = $f; unsafe { glColor4f(f(*v), f(*v.add(1)), f(*v.add(2)), 1.0) } }
            #[no_mangle] pub unsafe extern "system" fn $n4v(v: *const $t) { let f = $f; unsafe { glColor4f(f(*v), f(*v.add(1)), f(*v.add(2)), f(*v.add(3))) } }
        )*
    };
}
color_variants! {
    glColor3b, glColor4b, glColor3bv, glColor4bv, i8, b;
    glColor3s, glColor4s, glColor3sv, glColor4sv, i16, s;
    glColor3i, glColor4i, glColor3iv, glColor4iv, i32, si;
    glColor3us, glColor4us, glColor3usv, glColor4usv, u16, us;
    glColor3ui, glColor4ui, glColor3uiv, glColor4uiv, u32, ui;
}

#[no_mangle] pub unsafe extern "system" fn glColor3dv(v: *const f64) { unsafe { glColor4f(*v as f32, *v.add(1) as f32, *v.add(2) as f32, 1.0) } }
#[no_mangle] pub unsafe extern "system" fn glColor4dv(v: *const f64) { unsafe { glColor4f(*v as f32, *v.add(1) as f32, *v.add(2) as f32, *v.add(3) as f32) } }

macro_rules! normal_variants {
    ($($n:ident, $nv:ident, $t:ty, $f:expr;)*) => {
        $(
            #[no_mangle] pub extern "system" fn $n(x: $t, y: $t, z: $t) { let f = $f; glNormal3f(f(x), f(y), f(z)) }
            #[no_mangle] pub unsafe extern "system" fn $nv(v: *const $t) { let f = $f; unsafe { glNormal3f(f(*v), f(*v.add(1)), f(*v.add(2))) } }
        )*
    };
}
normal_variants! {
    glNormal3b, glNormal3bv, i8, b;
    glNormal3s, glNormal3sv, i16, s;
    glNormal3i, glNormal3iv, i32, si;
}

#[no_mangle] pub unsafe extern "system" fn glNormal3dv(v: *const f64) { unsafe { glNormal3f(*v as f32, *v.add(1) as f32, *v.add(2) as f32) } }

macro_rules! texcoord_variants {
    ($($name:ident($($p:ident),*): $t:ty;)*) => {
        $(#[no_mangle] pub extern "system" fn $name($($p: $t),*) {
            let v = [$($p as f32),*];
            let g = |i: usize| if i < v.len() { v[i] } else if i == 3 { 1.0 } else { 0.0 };
            glTexCoord4f(g(0), g(1), g(2), g(3))
        })*
    };
}
texcoord_variants! {
    glTexCoord1d(s0): f64; glTexCoord1i(s0): i32; glTexCoord1s(s0): i16;
    glTexCoord2s(s0, t0): i16;
    glTexCoord3d(s0, t0, r0): f64; glTexCoord3f(s0, t0, r0): f32; glTexCoord3i(s0, t0, r0): i32; glTexCoord3s(s0, t0, r0): i16;
    glTexCoord4d(s0, t0, r0, q0): f64; glTexCoord4i(s0, t0, r0, q0): i32; glTexCoord4s(s0, t0, r0, q0): i16;
}

macro_rules! texcoord_v {
    ($($name:ident($t:ty, $n:expr);)*) => {
        $(#[no_mangle] pub unsafe extern "system" fn $name(v: *const $t) {
            let g = |i: usize| if i < $n { (unsafe { *v.add(i) }) as f32 } else if i == 3 { 1.0 } else { 0.0 };
            glTexCoord4f(g(0), g(1), g(2), g(3))
        })*
    };
}
texcoord_v! {
    glTexCoord1dv(f64, 1); glTexCoord1fv(f32, 1); glTexCoord1iv(i32, 1); glTexCoord1sv(i16, 1);
    glTexCoord2dv(f64, 2); glTexCoord2iv(i32, 2); glTexCoord2sv(i16, 2);
    glTexCoord3dv(f64, 3); glTexCoord3fv(f32, 3); glTexCoord3iv(i32, 3); glTexCoord3sv(i16, 3);
    glTexCoord4dv(f64, 4); glTexCoord4fv(f32, 4); glTexCoord4iv(i32, 4); glTexCoord4sv(i16, 4);
}

macro_rules! vertex_v {
    ($($name:ident($t:ty, $n:expr);)*) => {
        $(#[no_mangle] pub unsafe extern "system" fn $name(v: *const $t) {
            let g = |i: usize| if i < $n { (unsafe { *v.add(i) }) as f32 } else if i == 3 { 1.0 } else { 0.0 };
            glVertex4f(g(0), g(1), g(2), g(3))
        })*
    };
}
vertex_v! {
    glVertex2sv(i16, 2); glVertex3sv(i16, 3); glVertex4dv(f64, 4); glVertex4iv(i32, 4); glVertex4sv(i16, 4);
}

#[no_mangle] pub extern "system" fn glVertex4i(x: i32, y: i32, z: i32, w: i32) { glVertex4f(x as f32, y as f32, z as f32, w as f32) }
#[no_mangle] pub extern "system" fn glVertex4s(x: i16, y: i16, z: i16, w: i16) { glVertex4f(x as f32, y as f32, z as f32, w as f32) }

#[no_mangle] pub extern "system" fn glRects(a: i16, b0: i16, c: i16, d: i16) { glRectf(a as f32, b0 as f32, c as f32, d as f32) }
#[no_mangle] pub unsafe extern "system" fn glRectfv(a: *const f32, b0: *const f32) { unsafe { glRectf(*a, *a.add(1), *b0, *b0.add(1)) } }
#[no_mangle] pub unsafe extern "system" fn glRectdv(a: *const f64, b0: *const f64) { unsafe { glRectf(*a as f32, *a.add(1) as f32, *b0 as f32, *b0.add(1) as f32) } }
#[no_mangle] pub unsafe extern "system" fn glRectiv(a: *const i32, b0: *const i32) { unsafe { glRectf(*a as f32, *a.add(1) as f32, *b0 as f32, *b0.add(1) as f32) } }
#[no_mangle] pub unsafe extern "system" fn glRectsv(a: *const i16, b0: *const i16) { unsafe { glRectf(*a as f32, *a.add(1) as f32, *b0 as f32, *b0.add(1) as f32) } }

// -----| Windows' own entry points |-----

/// Layer planes: no pixel format has any.
#[no_mangle] pub extern "system" fn wglCreateLayerContext(hdc: u64, layer: i32) -> u64 { if layer == 0 { wglCreateContext(hdc) } else { 0 } }
#[no_mangle] pub extern "system" fn wglDescribeLayerPlane(_hdc: u64, _fmt: i32, _layer: i32, _size: u32, _out: *mut u8) -> Bool { 0 }
#[no_mangle] pub extern "system" fn wglSetLayerPaletteEntries(_hdc: u64, _layer: i32, _start: i32, _n: i32, _c: *const u32) -> i32 { 0 }
#[no_mangle] pub extern "system" fn wglGetLayerPaletteEntries(_hdc: u64, _layer: i32, _start: i32, _n: i32, _c: *mut u32) -> i32 { 0 }
#[no_mangle] pub extern "system" fn wglRealizeLayerPalette(_hdc: u64, _layer: i32, _realize: Bool) -> Bool { 0 }

/// The main plane (WGL_SWAP_MAIN_PLANE, bit 0) is swapped.
#[no_mangle]
pub extern "system" fn wglSwapLayerBuffers(hdc: u64, planes: u32) -> Bool {
    if planes & 1 != 0 { wglSwapBuffers(hdc) } else { 0 }
}

/// WGLSWAP { hdc, flags } entries.
#[no_mangle]
pub unsafe extern "system" fn wglSwapMultipleBuffers(n: u32, swaps: *const [u64; 2]) -> u32 {
    let mut done = 0;
    for i in 0..n as usize {
        let s = unsafe { *swaps.add(i) };
        if wglSwapLayerBuffers(s[0], s[1] as u32) != 0 {
            done |= 1 << i;
        }
    }
    done
}

/// Copies the state groups `mask` names from one context into another.
#[no_mangle]
pub extern "system" fn wglCopyContext(src: u64, dst: u64, mask: u32) -> Bool {
    if !valid(src) || !valid(dst) || src == dst {
        return 0;
    }
    let (s, d) = unsafe { (&*(src as *const Ctx), &mut *(dst as *mut Ctx)) };
    crate::attrib::copy(s, d, mask);
    1
}

#[no_mangle]
pub extern "system" fn wglGetDefaultProcAddress(name: *const u8) -> usize {
    wglGetProcAddress(name)
}

/// OpenGL metafile playback (GLS records in EMF files): none here.
#[no_mangle] pub extern "system" fn GlmfInitPlayback(_hdc: u64, _emf: u64, _rect: *const [i32; 4]) -> Bool { 0 }
#[no_mangle] pub extern "system" fn GlmfBeginGlsBlock(_hdc: u64) -> Bool { 0 }
#[no_mangle] pub extern "system" fn GlmfPlayGlsRecord(_hdc: u64, _op: u32, _n: u32, _rec: *const u8) -> Bool { 0 }
#[no_mangle] pub extern "system" fn GlmfEndGlsBlock(_hdc: u64) -> Bool { 0 }
#[no_mangle] pub extern "system" fn GlmfEndPlayback(_hdc: u64) -> Bool { 0 }
#[no_mangle] pub extern "system" fn GlmfCloseMetaFile(_hdc: u64) -> Bool { 0 }

#[no_mangle]
pub extern "system" fn glDebugEntry(_a: u32, _b: u32) -> Bool {
    1
}
