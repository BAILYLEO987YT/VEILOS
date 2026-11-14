//! Pixels (WIN32_DESIGN.md G5c): the raster position, glDrawPixels,
//! glBitmap, glCopyPixels and glReadPixels in every format and type GL 1.1
//! has, pixel storage, transfer (scale, bias, maps) and zoom, the front and
//! back buffers, the accumulation buffer, and wglUseFontBitmaps.
//!
//! An image or bitmap is drawn as a window-aligned quad textured with a
//! temporary texture of exactly its pixels (nearest, one texel a pixel), so
//! its fragments go through the whole fragment pipeline as GL has them:
//! depth, stencil, alpha test, blending, fog, the colour mask. A bitmap's
//! texels are white with alpha 1 where its bits are set and 0 elsewhere,
//! modulated by the raster colour, with F_TEX_MASK discarding the rest.

use alloc::vec;
use alloc::vec::Vec;

use crate::*;

// -----| pixel storage |-----

#[derive(Clone, Copy)]
pub(crate) struct PixelStore {
    pub align: i32,
    pub row_length: i32,
    pub skip_rows: i32,
    pub skip_pixels: i32,
    pub lsb_first: bool,
    pub swap_bytes: bool,
}

impl Default for PixelStore {
    fn default() -> PixelStore {
        PixelStore::new()
    }
}

impl PixelStore {
    pub const fn new() -> PixelStore {
        PixelStore { align: 4, row_length: 0, skip_rows: 0, skip_pixels: 0, lsb_first: false, swap_bytes: false }
    }

    /// Where row `y`, pixel `x` of a `w`-wide image of `size`-byte
    /// components, `comps` of them a pixel, starts.
    fn offset(&self, w: i32, comps: usize, size: usize, x: i32, y: i32) -> usize {
        let len = if self.row_length > 0 { self.row_length } else { w } as usize;
        let row = (len * comps * size).div_ceil(self.align as usize) * self.align as usize;
        (y + self.skip_rows) as usize * row + (x + self.skip_pixels) as usize * comps * size
    }
}

/// The pixel transfer state: scale and bias for each of red, green, blue,
/// alpha (then depth), and the RGBA maps.
#[derive(Clone)]
pub(crate) struct Transfer {
    pub scale: [f32; 5],
    pub bias: [f32; 5],
    pub map_color: bool,
    /// GL_PIXEL_MAP_I_TO_I .. GL_PIXEL_MAP_A_TO_A (0x0C70 .. 0x0C79).
    pub maps: [Vec<f32>; 10],
    pub zoom: [f32; 2],
}

impl Transfer {
    pub fn new() -> Transfer {
        Transfer { scale: [1.0; 5], bias: [0.0; 5], map_color: false, maps: core::array::from_fn(|_| vec![0.0]), zoom: [1.0, 1.0] }
    }

    fn identity(&self) -> bool {
        self.scale[..4].iter().all(|&s| s == 1.0) && self.bias[..4].iter().all(|&b| b == 0.0) && !self.map_color
    }

    /// Scale, bias and the RGBA maps on one colour (0 ..= 1 each).
    fn apply(&self, c: [f32; 4]) -> [f32; 4] {
        core::array::from_fn(|i| {
            let v = clamp01(c[i] * self.scale[i] + self.bias[i]);
            if self.map_color {
                let m = &self.maps[6 + i];
                m[((v * (m.len() - 1) as f32) + 0.5) as usize]
            } else {
                v
            }
        })
    }
}

/// The raster position (glRasterPos, glBitmap's moves).
#[derive(Clone, Copy)]
pub(crate) struct Raster {
    pub valid: bool,
    /// Window x, y (y upwards), depth 0 ..= 1.
    pub win: [f64; 3],
    pub color: [f32; 4],
    pub tc: [f32; 4],
    pub dist: f32,
}

impl Raster {
    pub const fn new() -> Raster {
        Raster { valid: true, win: [0.0; 3], color: [1.0; 4], tc: [0.0, 0.0, 0.0, 1.0], dist: 0.0 }
    }
}

// -----| formats and types |-----

const GL_COLOR_INDEX: u32 = 0x1900;
const GL_STENCIL_INDEX: u32 = 0x1901;
const GL_DEPTH_COMPONENT: u32 = 0x1902;
const GL_RED: u32 = 0x1903;
const GL_GREEN: u32 = 0x1904;
const GL_BLUE: u32 = 0x1905;
const GL_BITMAP: u32 = 0x1A00;

/// Components a pixel of `format` has.
fn components(format: u32) -> Option<usize> {
    Some(match format {
        GL_RGB | GL_BGR_EXT => 3,
        GL_RGBA | GL_BGRA_EXT => 4,
        GL_LUMINANCE_ALPHA => 2,
        GL_LUMINANCE | GL_ALPHA | GL_RED | GL_GREEN | GL_BLUE | GL_COLOR_INDEX | GL_STENCIL_INDEX | GL_DEPTH_COMPONENT => 1,
        _ => return None,
    })
}

/// Bytes a component of `kind` takes.
fn type_size(kind: u32) -> Option<usize> {
    Some(match kind {
        GL_BYTE | GL_UNSIGNED_BYTE => 1,
        GL_SHORT | GL_UNSIGNED_SHORT => 2,
        GL_INT | GL_UNSIGNED_INT | 0x1406 => 4,
        _ => return None,
    })
}

/// Component `i` of a pixel at `p`, as GL converts it: unsigned types to
/// 0 ..= 1, signed to -1 ..= 1, floats as they are; `raw` keeps integers
/// as integers (indices).
unsafe fn read_comp(p: *const u8, kind: u32, swap: bool, raw: bool) -> f64 {
    unsafe {
        let b2 = |p: *const u8| {
            let v = [*p, *p.add(1)];
            if swap { [v[1], v[0]] } else { v }
        };
        let b4 = |p: *const u8| {
            let v = [*p, *p.add(1), *p.add(2), *p.add(3)];
            if swap { [v[3], v[2], v[1], v[0]] } else { v }
        };
        match kind {
            GL_UNSIGNED_BYTE => if raw { *p as f64 } else { *p as f64 / 255.0 },
            GL_BYTE => if raw { *(p as *const i8) as f64 } else { (2.0 * *(p as *const i8) as f64 + 1.0) / 255.0 },
            GL_UNSIGNED_SHORT => { let v = u16::from_le_bytes(b2(p)) as f64; if raw { v } else { v / 65535.0 } }
            GL_SHORT => { let v = i16::from_le_bytes(b2(p)) as f64; if raw { v } else { (2.0 * v + 1.0) / 65535.0 } }
            GL_UNSIGNED_INT => { let v = u32::from_le_bytes(b4(p)) as f64; if raw { v } else { v / 4294967295.0 } }
            GL_INT => { let v = i32::from_le_bytes(b4(p)) as f64; if raw { v } else { (2.0 * v + 1.0) / 4294967295.0 } }
            _ => f32::from_le_bytes(b4(p)) as f64,
        }
    }
}

/// Write component `v` (0 ..= 1, or an integer if `raw`) at `p`.
unsafe fn write_comp(p: *mut u8, kind: u32, v: f64, swap: bool, raw: bool) {
    unsafe {
        let put = |bytes: &[u8]| {
            for (i, b) in bytes.iter().enumerate() {
                *p.add(if swap { bytes.len() - 1 - i } else { i }) = *b;
            }
        };
        let u = |max: f64| if raw { v.clamp(0.0, max) } else { floor(v.clamp(0.0, 1.0) * max + 0.5) };
        let s = |max: f64| if raw { v.clamp(-max - 1.0, max) } else { floor((v.clamp(-1.0, 1.0) * (2.0 * max + 1.0) - 1.0) / 2.0 + 0.5) };
        match kind {
            GL_UNSIGNED_BYTE => *p = u(255.0) as u8,
            GL_BYTE => *p = s(127.0) as i8 as u8,
            GL_UNSIGNED_SHORT => put(&(u(65535.0) as u16).to_le_bytes()),
            GL_SHORT => put(&(s(32767.0) as i16).to_le_bytes()),
            GL_UNSIGNED_INT => put(&(u(4294967295.0) as u32).to_le_bytes()),
            GL_INT => put(&(s(2147483647.0) as i32).to_le_bytes()),
            _ => put(&(v as f32).to_le_bytes()),
        }
    }
}

/// A colour image from client memory as 0xAARRGGBB, rows of `w` from the
/// first in memory, with the pixel store and transfer state applied.
pub(crate) unsafe fn unpack(c: &Ctx, w: i32, h: i32, format: u32, kind: u32, pixels: *const u8) -> Option<Vec<u32>> {
    let comps = components(format)?;
    if matches!(format, GL_STENCIL_INDEX | GL_DEPTH_COMPONENT | GL_COLOR_INDEX) {
        return None;
    }
    let size = type_size(kind)?;
    let st = &c.unpack;
    let tr = &c.transfer;
    let identity = tr.identity();
    let mut out = Vec::with_capacity((w.max(0) * h.max(0)) as usize);
    for y in 0..h.max(0) {
        let row = unsafe { pixels.add(st.offset(w, comps, size, 0, y)) };
        for x in 0..w.max(0) {
            let p = unsafe { row.add(x as usize * comps * size) };
            let comp = |i: usize| unsafe { read_comp(p.add(i * size), kind, st.swap_bytes, false) as f32 };
            let rgba: [f32; 4] = match format {
                GL_RGB => [comp(0), comp(1), comp(2), 1.0],
                GL_BGR_EXT => [comp(2), comp(1), comp(0), 1.0],
                GL_RGBA => [comp(0), comp(1), comp(2), comp(3)],
                GL_BGRA_EXT => [comp(2), comp(1), comp(0), comp(3)],
                GL_LUMINANCE => [comp(0), comp(0), comp(0), 1.0],
                GL_LUMINANCE_ALPHA => [comp(0), comp(0), comp(0), comp(1)],
                GL_ALPHA => [0.0, 0.0, 0.0, comp(0)],
                GL_RED => [comp(0), 0.0, 0.0, 1.0],
                GL_GREEN => [0.0, comp(0), 0.0, 1.0],
                _ => [0.0, 0.0, comp(0), 1.0],
            };
            let rgba = if identity { rgba.map(clamp01) } else { tr.apply(rgba) };
            let ch = |v: f32| (v * 255.0 + 0.5) as u32;
            out.push(ch(rgba[3]) << 24 | ch(rgba[0]) << 16 | ch(rgba[1]) << 8 | ch(rgba[2]));
        }
    }
    Some(out)
}

/// A one-bit-a-pixel image (glBitmap, GL_BITMAP stencil), rows from the
/// first in memory, as 0/1.
unsafe fn unpack_bits(st: &PixelStore, w: i32, h: i32, bits: *const u8) -> Vec<u8> {
    let len = if st.row_length > 0 { st.row_length } else { w } as usize;
    let row = len.div_ceil(8).div_ceil(st.align as usize) * st.align as usize;
    let mut out = Vec::with_capacity((w.max(0) * h.max(0)) as usize);
    for y in 0..h.max(0) as usize {
        for x in 0..w.max(0) as usize {
            let bit = x + st.skip_pixels as usize;
            let byte = unsafe { *bits.add((y + st.skip_rows as usize) * row + bit / 8) };
            let set = if st.lsb_first { byte >> (bit % 8) } else { byte >> (7 - bit % 8) } & 1;
            out.push(set);
        }
    }
    out
}

// -----| drawing images |-----

impl Ctx {
    /// Draw `img` (`w` x `h`, the first row the bottom one, 0xAARRGGBB)
    /// with its lower-left corner at window (x, y), each pixel zoomed to
    /// zx x zy; `mask`: a bitmap, coloured `mask`.
    pub(crate) fn draw_image(&mut self, x: f64, y: f64, w: i32, h: i32, img: &[u32], zoom: [f32; 2], mask: Option<[f32; 4]>) {
        if w <= 0 || h <= 0 {
            return;
        }
        let args = TexArgs {
            w: w as u32,
            h: h as u32,
            texels: img.as_ptr() as u64,
            levels: 1,
            depth: 0,
            layers: 0,
            kind: k::TEX_2D,
        };
        let mut handle = 0u64;
        if unsafe { VeilGpu3d(4, 0, &args as *const _ as u64, &mut handle as *mut u64 as u64) } != 0 {
            return;
        }
        self.temp_textures.push(handle);
        let (mut g, _) = self.state(KIND_TRIANGLES, 1.0);
        g.flags &= !F_POLY_STIPPLE;
        g.stages = Default::default();
        g.samplers = Default::default();
        g.stages[0] = crate::gl_stage(if mask.is_some() { GL_MODULATE } else { GL_REPLACE }, 0);
        g.samplers[0] = Sampler { texture: handle, min: k::FILTER_POINT, mag: k::FILTER_POINT, address: [k::CLAMP; 2], ..Default::default() };
        if mask.is_some() {
            g.flags |= F_TEX_MASK;
        }
        let r = self.raster;
        let col = mask.unwrap_or([1.0; 4]);
        let ch = |v: f32| (clamp01(v) as f64 * 255.0 * FX) as i64;
        let corner = |cx: i32, cy: i32| KVert {
            x: round((x + (cx as f64) * w as f64 * zoom[0] as f64) * SUB) as i32,
            y: round((self.h as f64 - (y + (cy as f64) * h as f64 * zoom[1] as f64)) * SUB) as i32,
            z: (r.win[2].clamp(0.0, 1.0) * DEPTH_MAX * FX) as i64,
            q: FX as i64,
            uv: [[((cx * w) as f64 * FX) as i64, ((cy * h) as f64 * FX) as i64], [0; 2], [0; 2], [0; 2]],
            spec: [0; 3],
            r: ch(col[0]),
            g: ch(col[1]),
            b: ch(col[2]),
            a: ch(col[3]),
            f: (r.dist as f64 * FX) as i64,
        };
        let (c00, c10, c11, c01) = (corner(0, 0), corner(1, 0), corner(1, 1), corner(0, 1));
        self.push_tri(&g, KTri { v: [c00, c10, c11] });
        self.push_tri(&g, KTri { v: [c00, c11, c01] });
    }

    /// The colour buffer read from (glReadBuffer): `w` x `h` pixels from
    /// window (x, y), the first row the bottom one, 0x00RRGGBB.
    pub(crate) fn read_color(&mut self, x: i32, y: i32, w: i32, h: i32) -> Vec<u32> {
        self.flush();
        let mut buf = vec![0u32; (w.max(0) * h.max(0)) as usize];
        if w <= 0 || h <= 0 {
            return buf;
        }
        if self.read_front && self.double {
            // The front buffer is the frame last shown.
            for row in 0..h {
                let wy = self.h - 1 - (y + row);
                for col in 0..w {
                    let wx = x + col;
                    if wx >= 0 && wy >= 0 && wx < self.w && wy < self.h && !self.front.is_empty() {
                        buf[(row * w + col) as usize] = self.front[(wy * self.w + wx) as usize];
                    }
                }
            }
            return buf;
        }
        let mut top = vec![0u32; (w * h) as usize];
        let r = ReadArgs { x, y: self.h - (y + h), w, h, dst: top.as_mut_ptr() as u64, stride: w as i64 * 4 };
        unsafe { VeilGpu3d(3, self.target, &r as *const _ as u64, 0) };
        for row in 0..h as usize {
            buf[row * w as usize..(row + 1) * w as usize].copy_from_slice(&top[(h as usize - 1 - row) * w as usize..][..w as usize]);
        }
        buf
    }

    /// Depth (`which` 1, 0 ..= DEPTH_MAX) or stencil (2) values, rows from
    /// the bottom.
    pub(crate) fn read_other(&mut self, which: u32, x: i32, y: i32, w: i32, h: i32) -> Vec<u32> {
        self.flush();
        let mut top = vec![0u32; (w.max(0) * h.max(0)) as usize];
        if w <= 0 || h <= 0 {
            return top;
        }
        let a = PixelArgs { x, y: self.h - (y + h), w, h, ptr: top.as_mut_ptr() as u64, stride: w as i64 * 4, which, mask: 0, scissor: [0; 4] };
        unsafe { VeilGpu3d(6, self.target, &a as *const _ as u64, 0) };
        let mut out = vec![0u32; top.len()];
        for row in 0..h as usize {
            out[row * w as usize..(row + 1) * w as usize].copy_from_slice(&top[(h as usize - 1 - row) * w as usize..][..w as usize]);
        }
        out
    }

    /// Write depth or stencil values (rows from the bottom) straight in at
    /// window (x, y), inside the scissor box.
    pub(crate) fn write_other(&mut self, which: u32, x: i32, y: i32, w: i32, h: i32, vals: &[u32], mask: u32) {
        self.flush();
        if w <= 0 || h <= 0 {
            return;
        }
        let mut top = vec![0u32; vals.len()];
        for row in 0..h as usize {
            top[row * w as usize..(row + 1) * w as usize].copy_from_slice(&vals[(h as usize - 1 - row) * w as usize..][..w as usize]);
        }
        let a = PixelArgs { x, y: self.h - (y + h), w, h, ptr: top.as_ptr() as u64, stride: w as i64 * 4, which, mask, scissor: self.scissor_box() };
        unsafe { VeilGpu3d(5, self.target, &a as *const _ as u64, 0) };
    }

    /// Where an image drawn at the raster position starts, in whole pixels
    /// when not zoomed (GL rounds it so).
    fn image_origin(&self) -> (f64, f64) {
        (self.raster.win[0], self.raster.win[1])
    }
}

// -----| the raster position |-----

pub(crate) fn raster_pos(x: f32, y: f32, z: f32, w: f32) {
    rec!(raster_pos(x, y, z, w));
    outside(|c| {
        let mv = *c.stacks[0].last().unwrap();
        let eye = xform(&mv, [x, y, z, w]);
        let clip = xform(c.stacks[1].last().unwrap(), eye);
        let inside = (0..3).all(|k| clip[k] >= -clip[3] && clip[k] <= clip[3]) && clip[3] > 0.0
            && (0..6).all(|i| !c.on(GL_CLIP_PLANE0 + i as u32) || dot4(c.clip_planes[i], eye) >= 0.0);
        // The current colour (lit if lighting is on) and texture position
        // go with it, as a vertex's would.
        let saved = core::mem::take(&mut c.verts);
        c.vertex([x, y, z, w]);
        let v = c.verts.pop();
        c.verts = saved;
        let Some(v) = v else { return };
        let win = c.window(&v);
        c.raster = Raster { valid: inside, win: [win[0], win[1], win[2]], color: v.col.map(clamp01), tc: v.tc[0], dist: v.dist };
    })
}

macro_rules! raster_variants {
    ($($name:ident($($p:ident: $t:ty),*) => ($x:expr, $y:expr, $z:expr, $w:expr);)*) => {
        $(#[no_mangle] pub extern "system" fn $name($($p: $t),*) { raster_pos($x as f32, $y as f32, $z as f32, $w as f32) })*
    };
}
macro_rules! raster_variants_v {
    ($($name:ident($t:ty, $n:expr);)*) => {
        $(#[no_mangle] pub unsafe extern "system" fn $name(v: *const $t) {
            let g = |i: usize| if i < $n { (unsafe { *v.add(i) }) as f32 } else if i == 3 { 1.0 } else { 0.0 };
            raster_pos(g(0), g(1), g(2), g(3))
        })*
    };
}
raster_variants! {
    glRasterPos2s(x: i16, y: i16) => (x, y, 0, 1);
    glRasterPos2i(x: i32, y: i32) => (x, y, 0, 1);
    glRasterPos2f(x: f32, y: f32) => (x, y, 0, 1);
    glRasterPos2d(x: f64, y: f64) => (x, y, 0, 1);
    glRasterPos3s(x: i16, y: i16, z: i16) => (x, y, z, 1);
    glRasterPos3i(x: i32, y: i32, z: i32) => (x, y, z, 1);
    glRasterPos3f(x: f32, y: f32, z: f32) => (x, y, z, 1);
    glRasterPos3d(x: f64, y: f64, z: f64) => (x, y, z, 1);
    glRasterPos4s(x: i16, y: i16, z: i16, w: i16) => (x, y, z, w);
    glRasterPos4i(x: i32, y: i32, z: i32, w: i32) => (x, y, z, w);
    glRasterPos4f(x: f32, y: f32, z: f32, w: f32) => (x, y, z, w);
    glRasterPos4d(x: f64, y: f64, z: f64, w: f64) => (x, y, z, w);
}
raster_variants_v! {
    glRasterPos2sv(i16, 2); glRasterPos2iv(i32, 2); glRasterPos2fv(f32, 2); glRasterPos2dv(f64, 2);
    glRasterPos3sv(i16, 3); glRasterPos3iv(i32, 3); glRasterPos3fv(f32, 3); glRasterPos3dv(f64, 3);
    glRasterPos4sv(i16, 4); glRasterPos4iv(i32, 4); glRasterPos4fv(f32, 4); glRasterPos4dv(f64, 4);
}

// -----| glBitmap, glDrawPixels, glCopyPixels |-----

#[no_mangle]
pub unsafe extern "system" fn glBitmap(w: i32, h: i32, xo: f32, yo: f32, xm: f32, ym: f32, bits: *const u8) {
    if w < 0 || h < 0 {
        return with(|c| c.err(GL_INVALID_VALUE));
    }
    let mask = if bits.is_null() || w == 0 || h == 0 { Vec::new() } else { unsafe { unpack_bits(&with(|c| c.unpack), w, h, bits) } };
    bitmap(w, h, xo, yo, xm, ym, mask);
}

fn bitmap(w: i32, h: i32, xo: f32, yo: f32, xm: f32, ym: f32, mask: Vec<u8>) {
    let m2 = mask.clone();
    rec!(bitmap(w, h, xo, yo, xm, ym, m2.clone()));
    outside(|c| {
        if !c.raster.valid {
            return;
        }
        if c.capture_pixels(0) {
            c.raster.win[0] += xm as f64;
            c.raster.win[1] += ym as f64;
            return;
        }
        if !mask.is_empty() {
            let img: Vec<u32> = mask.iter().map(|&b| if b != 0 { 0xFFFF_FFFF } else { 0 }).collect();
            // GL: the lower-left corner at floor(raster - origin).
            let x = floor(c.raster.win[0] - xo as f64);
            let y = floor(c.raster.win[1] - yo as f64);
            let col = c.raster.color;
            c.draw_image(x, y, w, h, &img, [1.0, 1.0], Some(col));
        }
        c.raster.win[0] += xm as f64;
        c.raster.win[1] += ym as f64;
    })
}

#[no_mangle]
pub unsafe extern "system" fn glDrawPixels(w: i32, h: i32, format: u32, kind: u32, pixels: *const u8) {
    if w < 0 || h < 0 {
        return with(|c| c.err(GL_INVALID_VALUE));
    }
    if components(format).is_none() || (type_size(kind).is_none() && kind != GL_BITMAP) {
        return with(|c| c.err(GL_INVALID_ENUM));
    }
    let Some(data) = with(|c| -> Option<(u32, Vec<u32>)> {
        match format {
            GL_DEPTH_COMPONENT | GL_STENCIL_INDEX | GL_COLOR_INDEX => {
                if kind == GL_BITMAP {
                    if format != GL_STENCIL_INDEX && format != GL_COLOR_INDEX {
                        return None;
                    }
                    let b = unsafe { unpack_bits(&c.unpack, w, h, pixels) };
                    return Some((format, b.into_iter().map(|v| v as u32).collect()));
                }
                let size = type_size(kind)?;
                let st = c.unpack;
                let mut out = Vec::with_capacity((w * h) as usize);
                for y in 0..h {
                    for x in 0..w {
                        let p = unsafe { pixels.add(st.offset(w, 1, size, x, y)) };
                        let raw = format != GL_DEPTH_COMPONENT;
                        let v = unsafe { read_comp(p, kind, st.swap_bytes, raw) };
                        out.push(if raw { v as u32 } else {
                            let d = (v * c.transfer.scale[4] as f64 + c.transfer.bias[4] as f64).clamp(0.0, 1.0);
                            (d * DEPTH_MAX + 0.5) as u32
                        });
                    }
                }
                Some((format, out))
            }
            _ => unsafe { unpack(c, w, h, format, kind, pixels) }.map(|v| (GL_RGBA, v)),
        }
    }) else {
        return with(|c| c.err(GL_INVALID_ENUM));
    };
    draw_pixels(w, h, data.0, data.1);
}

fn draw_pixels(w: i32, h: i32, format: u32, data: Vec<u32>) {
    let d2 = data.clone();
    rec!(draw_pixels(w, h, format, d2.clone()));
    outside(|c| {
        if !c.raster.valid || c.capture_pixels(1) {
            return;
        }
        let (x, y) = c.image_origin();
        match format {
            GL_DEPTH_COMPONENT => c.write_other(1, x as i32, y as i32, w, h, &data, 0),
            GL_STENCIL_INDEX => {
                let m = c.stencil[3];
                c.write_other(2, x as i32, y as i32, w, h, &data, m)
            }
            // No colour-index pixel formats: indices have no colour to draw.
            GL_COLOR_INDEX => {}
            _ => {
                let z = c.transfer.zoom;
                c.draw_image(x, y, w, h, &data, z, None);
            }
        }
    })
}

#[no_mangle]
pub extern "system" fn glCopyPixels(x: i32, y: i32, w: i32, h: i32, kind: u32) {
    rec!(glCopyPixels(x, y, w, h, kind));
    outside(|c| {
        if w < 0 || h < 0 {
            return c.err(GL_INVALID_VALUE);
        }
        if !c.raster.valid || c.capture_pixels(2) {
            return;
        }
        let (rx, ry) = c.image_origin();
        match kind {
            0x1800 => {
                let img: Vec<u32> = c.read_color(x, y, w, h).into_iter().map(|p| {
                    let tr = &c.transfer;
                    if tr.identity() {
                        p | 0xFF00_0000
                    } else {
                        let f = [(p >> 16 & 0xFF) as f32 / 255.0, (p >> 8 & 0xFF) as f32 / 255.0, (p & 0xFF) as f32 / 255.0, 1.0];
                        let o = tr.apply(f);
                        let ch = |v: f32| (v * 255.0 + 0.5) as u32;
                        ch(o[3]) << 24 | ch(o[0]) << 16 | ch(o[1]) << 8 | ch(o[2])
                    }
                }).collect();
                let z = c.transfer.zoom;
                c.draw_image(rx, ry, w, h, &img, z, None);
            }
            0x1801 => {
                let d = c.read_other(1, x, y, w, h);
                c.write_other(1, rx as i32, ry as i32, w, h, &d, 0);
            }
            0x1802 => {
                let s = c.read_other(2, x, y, w, h);
                let m = c.stencil[3];
                c.write_other(2, rx as i32, ry as i32, w, h, &s, m);
            }
            _ => c.err(GL_INVALID_ENUM),
        }
    })
}

#[no_mangle]
pub extern "system" fn glPixelZoom(x: f32, y: f32) {
    rec!(glPixelZoom(x, y));
    outside(|c| c.transfer.zoom = [x, y])
}

// -----| pixel storage and transfer |-----

fn pixel_store(name: u32, v: f64) {
    outside(|c| {
        let (st, what) = match name {
            0x0CF0..=0x0CF5 => (&mut c.unpack, name - 0x0CF0),
            0x0D00..=0x0D05 => (&mut c.pack, name - 0x0D00),
            _ => return c.err(GL_INVALID_ENUM),
        };
        let i = v as i32;
        match what {
            0 => st.swap_bytes = v != 0.0,
            1 => st.lsb_first = v != 0.0,
            5 if matches!(i, 1 | 2 | 4 | 8) => st.align = i,
            2..=4 if i >= 0 => match what {
                2 => st.row_length = i,
                3 => st.skip_rows = i,
                _ => st.skip_pixels = i,
            },
            _ => c.err(GL_INVALID_VALUE),
        }
    })
}

#[no_mangle]
pub extern "system" fn glPixelStorei(name: u32, v: i32) {
    pixel_store(name, v as f64)
}

#[no_mangle]
pub extern "system" fn glPixelStoref(name: u32, v: f32) {
    pixel_store(name, v as f64)
}

fn pixel_transfer(name: u32, v: f32) {
    rec!(pixel_transfer(name, v));
    outside(|c| {
        let t = &mut c.transfer;
        match name {
            // GL_MAP_COLOR, GL_MAP_STENCIL (no index maps kept).
            0x0D10 => t.map_color = v != 0.0,
            0x0D11 | 0x0D12 | 0x0D13 => {}
            // RED_SCALE, RED_BIAS; (ZOOM_X, ZOOM_Y); GREEN_, BLUE_, ALPHA_
            // SCALE and BIAS.
            0x0D14 => t.scale[0] = v,
            0x0D15 => t.bias[0] = v,
            0x0D18 => t.scale[1] = v,
            0x0D19 => t.bias[1] = v,
            0x0D1A => t.scale[2] = v,
            0x0D1B => t.bias[2] = v,
            0x0D1C => t.scale[3] = v,
            0x0D1D => t.bias[3] = v,
            // DEPTH_SCALE, DEPTH_BIAS
            0x0D1E => t.scale[4] = v,
            0x0D1F => t.bias[4] = v,
            _ => c.err(GL_INVALID_ENUM),
        }
    })
}

#[no_mangle]
pub extern "system" fn glPixelTransferf(name: u32, v: f32) {
    pixel_transfer(name, v)
}

#[no_mangle]
pub extern "system" fn glPixelTransferi(name: u32, v: i32) {
    pixel_transfer(name, v as f32)
}

fn pixel_map(map: u32, vals: Vec<f32>) {
    let v2 = vals.clone();
    rec!(pixel_map(map, v2.clone()));
    outside(|c| {
        if !(0x0C70..=0x0C79).contains(&map) {
            return c.err(GL_INVALID_ENUM);
        }
        let n = vals.len();
        if n == 0 || n > 256 || (map <= 0x0C75 && n & (n - 1) != 0) {
            return c.err(GL_INVALID_VALUE);
        }
        c.transfer.maps[(map - 0x0C70) as usize] = vals;
    })
}

#[no_mangle]
pub unsafe extern "system" fn glPixelMapfv(map: u32, n: i32, v: *const f32) {
    pixel_map(map, (0..n.max(0) as usize).map(|i| unsafe { *v.add(i) }).collect())
}

#[no_mangle]
pub unsafe extern "system" fn glPixelMapuiv(map: u32, n: i32, v: *const u32) {
    // Colour maps take unsigned integers as fractions of their range.
    let to = |x: u32| if map >= 0x0C72 { (x as f64 / 4294967295.0) as f32 } else { x as f32 };
    pixel_map(map, (0..n.max(0) as usize).map(|i| to(unsafe { *v.add(i) })).collect())
}

#[no_mangle]
pub unsafe extern "system" fn glPixelMapusv(map: u32, n: i32, v: *const u16) {
    let to = |x: u16| if map >= 0x0C72 { x as f32 / 65535.0 } else { x as f32 };
    pixel_map(map, (0..n.max(0) as usize).map(|i| to(unsafe { *v.add(i) })).collect())
}

fn get_map(map: u32) -> Vec<f32> {
    with(|c| if (0x0C70..=0x0C79).contains(&map) { c.transfer.maps[(map - 0x0C70) as usize].clone() } else { c.err(GL_INVALID_ENUM); Vec::new() })
}

#[no_mangle]
pub unsafe extern "system" fn glGetPixelMapfv(map: u32, out: *mut f32) {
    for (i, v) in get_map(map).into_iter().enumerate() {
        unsafe { *out.add(i) = v };
    }
}

#[no_mangle]
pub unsafe extern "system" fn glGetPixelMapuiv(map: u32, out: *mut u32) {
    for (i, v) in get_map(map).into_iter().enumerate() {
        unsafe { *out.add(i) = if map >= 0x0C72 { (v as f64 * 4294967295.0) as u32 } else { v as u32 } };
    }
}

#[no_mangle]
pub unsafe extern "system" fn glGetPixelMapusv(map: u32, out: *mut u16) {
    for (i, v) in get_map(map).into_iter().enumerate() {
        unsafe { *out.add(i) = if map >= 0x0C72 { (v * 65535.0) as u16 } else { v as u16 } };
    }
}

// -----| reading pixels |-----

#[no_mangle]
pub unsafe extern "system" fn glReadPixels(x: i32, y: i32, w: i32, h: i32, format: u32, kind: u32, out: *mut u8) {
    outside(|c| {
        if w < 0 || h < 0 {
            return c.err(GL_INVALID_VALUE);
        }
        let (Some(comps), Some(size)) = (components(format), type_size(kind)) else {
            return c.err(GL_INVALID_ENUM);
        };
        if w == 0 || h == 0 {
            return;
        }
        let st = c.pack;
        if matches!(format, GL_DEPTH_COMPONENT | GL_STENCIL_INDEX) {
            let which = if format == GL_DEPTH_COMPONENT { 1 } else { 2 };
            let vals = c.read_other(which, x, y, w, h);
            for row in 0..h {
                for col in 0..w {
                    let v = vals[(row * w + col) as usize];
                    let p = unsafe { out.add(st.offset(w, 1, size, col, row)) };
                    if which == 1 {
                        let d = (v as f64 / DEPTH_MAX * c.transfer.scale[4] as f64 + c.transfer.bias[4] as f64).clamp(0.0, 1.0);
                        unsafe { write_comp(p, kind, d, st.swap_bytes, false) };
                    } else {
                        unsafe { write_comp(p, kind, v as f64, st.swap_bytes, true) };
                    }
                }
            }
            return;
        }
        if format == GL_COLOR_INDEX {
            return c.err(GL_INVALID_OPERATION);
        }
        let pixels = c.read_color(x, y, w, h);
        let tr = c.transfer.clone();
        let identity = tr.identity();
        for row in 0..h {
            for col in 0..w {
                let p = pixels[(row * w + col) as usize];
                // The colour buffer has no alpha bits: alpha reads as 1.
                let mut f = [(p >> 16 & 0xFF) as f32 / 255.0, (p >> 8 & 0xFF) as f32 / 255.0, (p & 0xFF) as f32 / 255.0, 1.0];
                if !identity {
                    f = tr.apply(f);
                }
                let vals: [f32; 4] = match format {
                    GL_BGR_EXT | GL_BGRA_EXT => [f[2], f[1], f[0], f[3]],
                    GL_LUMINANCE | GL_LUMINANCE_ALPHA => [clamp01(f[0] + f[1] + f[2]), f[3], 0.0, 0.0],
                    GL_ALPHA => [f[3], 0.0, 0.0, 0.0],
                    GL_RED => [f[0], 0.0, 0.0, 0.0],
                    GL_GREEN => [f[1], 0.0, 0.0, 0.0],
                    GL_BLUE => [f[2], 0.0, 0.0, 0.0],
                    _ => f,
                };
                let base = st.offset(w, comps, size, col, row);
                for (k, v) in vals.iter().enumerate().take(comps) {
                    unsafe { write_comp(out.add(base + k * size), kind, *v as f64, st.swap_bytes, false) };
                }
            }
        }
    })
}

// -----| buffers |-----

fn buffer_ok(b: u32) -> bool {
    matches!(b, 0 | 0x400..=0x40C)
}

#[no_mangle]
pub extern "system" fn glDrawBuffer(b: u32) {
    rec!(glDrawBuffer(b));
    outside(|c| {
        if !buffer_ok(b) {
            return c.err(GL_INVALID_ENUM);
        }
        c.flush();
        // GL_NONE, and the front buffer (FRONT, FRONT_LEFT, LEFT, ...).
        c.draw_none = b == 0;
        c.draw_front = c.double && matches!(b, 0x400 | 0x401 | 0x404 | 0x406 | 0x407 | 0x408);
    })
}

#[no_mangle]
pub extern "system" fn glReadBuffer(b: u32) {
    rec!(glReadBuffer(b));
    outside(|c| {
        if !buffer_ok(b) || b == 0 {
            return c.err(GL_INVALID_ENUM);
        }
        c.read_front = matches!(b, 0x400 | 0x401 | 0x404 | 0x406 | 0x407);
    })
}

// -----| the accumulation buffer |-----

#[no_mangle]
pub extern "system" fn glClearAccum(r: f32, g: f32, b: f32, a: f32) {
    rec!(glClearAccum(r, g, b, a));
    outside(|c| c.clear_accum = [r, g, b, a].map(|v| v.clamp(-1.0, 1.0)))
}

impl Ctx {
    /// glClear's GL_ACCUM_BUFFER_BIT: the scissor box to the clear value.
    pub(crate) fn clear_accum_buffer(&mut self) {
        let n = (self.w * self.h) as usize;
        if self.accum.len() != n {
            self.accum = vec![[0.0; 4]; n];
        }
        let [x0, y0, x1, y1] = self.scissor_box();
        for y in y0.max(0)..y1.min(self.h) {
            for x in x0.max(0)..x1.min(self.w) {
                self.accum[(y * self.w + x) as usize] = self.clear_accum;
            }
        }
    }
}

#[no_mangle]
pub extern "system" fn glAccum(op: u32, value: f32) {
    rec!(glAccum(op, value));
    outside(|c| {
        let n = (c.w * c.h) as usize;
        if c.accum.len() != n {
            c.accum = vec![[0.0; 4]; n];
        }
        let (w, h) = (c.w, c.h);
        let [sx0, sy0, sx1, sy1] = c.scissor_box();
        let inside = |x: i32, y: i32| x >= sx0 && x < sx1 && y >= sy0 && y < sy1;
        match op {
            // GL_ACCUM, GL_LOAD
            0x0100 | 0x0101 => {
                let px = c.read_color(0, 0, w, h);
                for y in 0..h {
                    for x in 0..w {
                        // `px` has the bottom row first; the accumulation
                        // buffer is kept top row first, as the kernel's.
                        let p = px[((h - 1 - y) * w + x) as usize];
                        if !inside(x, y) {
                            continue;
                        }
                        let f = [(p >> 16 & 0xFF) as f32 / 255.0, (p >> 8 & 0xFF) as f32 / 255.0, (p & 0xFF) as f32 / 255.0, 1.0];
                        let a = &mut c.accum[(y * w + x) as usize];
                        for k in 0..4 {
                            a[k] = if op == 0x0101 { value * f[k] } else { a[k] + value * f[k] };
                        }
                    }
                }
            }
            // GL_RETURN: value x the buffer into the colour buffer.
            0x0102 => {
                let vals: Vec<u32> = c.accum.iter().map(|a| {
                    let ch = |v: f32| (clamp01(v * value) * 255.0 + 0.5) as u32;
                    ch(a[0]) << 16 | ch(a[1]) << 8 | ch(a[2])
                }).collect();
                c.flush();
                let pa = PixelArgs { x: 0, y: 0, w, h, ptr: vals.as_ptr() as u64, stride: w as i64 * 4, which: 0, mask: c.mask_bits(), scissor: c.scissor_box() };
                unsafe { VeilGpu3d(5, c.target, &pa as *const _ as u64, 0) };
            }
            // GL_MULT, GL_ADD
            0x0103 | 0x0104 => {
                for y in 0..h {
                    for x in 0..w {
                        if inside(x, y) {
                            for v in c.accum[(y * w + x) as usize].iter_mut() {
                                *v = if op == 0x0103 { *v * value } else { *v + value };
                            }
                        }
                    }
                }
            }
            _ => c.err(GL_INVALID_ENUM),
        }
    })
}

// -----| fonts |-----

#[link(name = "gdi32", kind = "raw-dylib")]
unsafe extern "system" {
    fn CreateCompatibleDC(hdc: u64) -> u64;
    fn DeleteDC(hdc: u64) -> i32;
    fn CreateDIBSection(hdc: u64, bmi: *const u8, usage: u32, bits: *mut *mut u32, section: u64, offset: u32) -> u64;
    fn SelectObject(hdc: u64, obj: u64) -> u64;
    fn DeleteObject(obj: u64) -> i32;
    fn GetCurrentObject(hdc: u64, kind: u32) -> u64;
    fn GetTextMetricsW(hdc: u64, tm: *mut u8) -> i32;
    fn GetTextExtentPoint32W(hdc: u64, s: *const u16, n: i32, size: *mut [i32; 2]) -> i32;
    fn TextOutW(hdc: u64, x: i32, y: i32, s: *const u16, n: i32) -> i32;
    fn SetTextColor(hdc: u64, c: u32) -> u32;
    fn SetBkMode(hdc: u64, mode: i32) -> i32;
}

/// wglUseFontBitmaps: a display list for each of `count` characters from
/// `first`, drawing it with glBitmap in the DC's font (its cell: the
/// advance wide, the font's height tall, the baseline at the descent).
fn use_font_bitmaps(hdc: u64, first: u32, count: u32, base: u32) -> Bool {
    unsafe {
        let mut tm = [0u8; 64];
        let font = GetCurrentObject(hdc, 6);
        let mem = CreateCompatibleDC(hdc);
        if mem == 0 {
            return 0;
        }
        SelectObject(mem, font);
        GetTextMetricsW(mem, tm.as_mut_ptr());
        let height = i32::from_le_bytes(tm[0..4].try_into().unwrap()).max(1);
        let descent = i32::from_le_bytes(tm[8..12].try_into().unwrap());
        for i in 0..count {
            let ch = [(first + i) as u16];
            let mut size = [0i32; 2];
            GetTextExtentPoint32W(mem, ch.as_ptr(), 1, &mut size);
            let w = size[0].max(1);
            let mut bmi = [0u8; 40];
            bmi[0] = 40;
            bmi[4..8].copy_from_slice(&w.to_le_bytes());
            bmi[8..12].copy_from_slice(&(-height).to_le_bytes());
            bmi[12] = 1;
            bmi[14] = 32;
            let mut bits: *mut u32 = core::ptr::null_mut();
            let bmp = CreateDIBSection(mem, bmi.as_ptr(), 0, &mut bits, 0, 0);
            if bmp == 0 || bits.is_null() {
                continue;
            }
            let old = SelectObject(mem, bmp);
            SetBkMode(mem, 1);
            SetTextColor(mem, 0xFF_FFFF);
            TextOutW(mem, 0, 0, ch.as_ptr(), 1);
            // Rows from the bottom, a bit a pixel.
            let mut mask = Vec::with_capacity((w * height) as usize);
            for y in (0..height).rev() {
                for x in 0..w {
                    mask.push((*bits.add((y * w + x) as usize) & 0xFF_FFFF != 0) as u8);
                }
            }
            SelectObject(mem, old);
            DeleteObject(bmp);
            crate::lists::glNewList(base + i, 0x1300);
            bitmap(w, height, 0.0, descent as f32, w as f32, 0.0, mask);
            crate::lists::glEndList();
        }
        DeleteDC(mem);
        1
    }
}

#[no_mangle]
pub extern "system" fn wglUseFontBitmapsW(hdc: u64, first: u32, count: u32, base: u32) -> Bool {
    use_font_bitmaps(hdc, first, count, base)
}

#[no_mangle]
pub extern "system" fn wglUseFontBitmapsA(hdc: u64, first: u32, count: u32, base: u32) -> Bool {
    use_font_bitmaps(hdc, first, count, base)
}

/// Outlines need the glyphs' curves, which gdi32 doesn't give yet.
#[no_mangle]
pub extern "system" fn wglUseFontOutlinesW(_hdc: u64, _first: u32, _count: u32, _base: u32, _dev: f32, _ext: f32, _fmt: i32, _gmf: *mut u8) -> Bool {
    unsafe { SetLastError(50) };
    0
}

#[no_mangle]
pub extern "system" fn wglUseFontOutlinesA(hdc: u64, first: u32, count: u32, base: u32, dev: f32, ext: f32, fmt: i32, gmf: *mut u8) -> Bool {
    wglUseFontOutlinesW(hdc, first, count, base, dev, ext, fmt, gmf)
}
