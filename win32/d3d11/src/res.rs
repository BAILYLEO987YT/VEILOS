//! Resources (buffers; 1D, 2D and 3D textures with their mips and array
//! slices) and views; Map, UpdateSubresource, copies and clears.
//!
//! Where the bytes live: every resource keeps all its subresources in
//! memory, in its own format. A subresource drawn into (a render target or
//! depth buffer) also has a kernel target (VEIL_GPU3D), the truth while it's
//! `drawn`: read back when the program needs the bytes. Sampling uploads a
//! texture's slice (its mips, as 0xAARRGGBB) to a kernel texture, again
//! when it changed.

use alloc::boxed::Box;
use alloc::vec;
use alloc::vec::Vec;

use gfx3d::raster3d::{ClearArgs, PixelArgs, ReadArgs, TexArgs, DEPTH_MAX};

use crate::com::*;
use crate::format::{self, *};
use crate::vtbl::*;

pub const DIM_BUFFER: u32 = 1;
pub const DIM_TEXTURE1D: u32 = 2;
pub const DIM_TEXTURE2D: u32 = 3;
pub const DIM_TEXTURE3D: u32 = 4;

pub const BIND_VERTEX_BUFFER: u32 = 1;
pub const BIND_INDEX_BUFFER: u32 = 2;
pub const BIND_CONSTANT_BUFFER: u32 = 4;
pub const BIND_SHADER_RESOURCE: u32 = 8;
pub const BIND_RENDER_TARGET: u32 = 0x20;
pub const BIND_DEPTH_STENCIL: u32 = 0x40;
pub const MISC_GENERATE_MIPS: u32 = 1;
pub const MISC_TEXTURECUBE: u32 = 4;

/// D3D11_TEXTURE2D_DESC (the others are converted to it).
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Tex2DDesc {
    pub w: u32,
    pub h: u32,
    pub mips: u32,
    pub array: u32,
    pub format: u32,
    pub sample: [u32; 2],
    pub usage: u32,
    pub bind: u32,
    pub cpu: u32,
    pub misc: u32,
}

/// D3D11_BUFFER_DESC.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct BufferDesc {
    pub size: u32,
    pub usage: u32,
    pub bind: u32,
    pub cpu: u32,
    pub misc: u32,
    pub stride: u32,
}

/// D3D11_SUBRESOURCE_DATA.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct SubData {
    pub mem: *const u8,
    pub pitch: u32,
    pub slice: u32,
}

/// One subresource's place in `data`.
#[derive(Clone, Copy)]
pub struct Sub {
    pub at: usize,
    pub w: u32,
    pub h: u32,
    pub d: u32,
    pub pitch: u32,
    pub slice: u32,
}

pub struct Resource {
    pub dim: u32,
    pub format: u32,
    pub w: u32,
    pub h: u32,
    /// Depth (3D) or array size.
    pub d: u32,
    pub mips: u32,
    pub usage: u32,
    pub bind: u32,
    pub cpu: u32,
    pub misc: u32,
    pub stride: u32,
    pub data: Vec<u8>,
    /// Mip-major within each array slice: index = mip + slice * mips.
    pub subs: Vec<Sub>,
    /// Each subresource's kernel target (0: none yet), and whether it's
    /// newer than `data`.
    pub targets: Vec<u64>,
    pub drawn: Vec<bool>,
    /// Each array slice's kernel texture (0: none yet), and whether
    /// `data` changed since.
    pub textures: Vec<u64>,
    pub dirty: Vec<bool>,
    pub mapped: Vec<bool>,
    pub priority: u32,
    /// The device, for flushing its waiting draws.
    pub device: *mut Com,
    /// Stream output: the bytes written (where the next write goes, and
    /// what DrawAuto draws).
    pub so_filled: u32,
}

impl Drop for Resource {
    fn drop(&mut self) {
        for &h in self.targets.iter().chain(self.textures.iter()) {
            close_later(h);
        }
    }
}

/// Kernel handles of resources freed while draws using them may still be
/// waiting: closed at the next flush.
pub static PENDING_CLOSE: crate::dllrt::Lock<Vec<u64>> = crate::dllrt::Lock::new(Vec::new());

pub fn close_later(h: u64) {
    if h != 0 {
        PENDING_CLOSE.lock().push(h);
    }
}

pub fn resource<'a>(p: *mut Com) -> &'a mut Resource {
    match body(p) {
        Body::Resource(r) => r,
        _ => panic!(),
    }
}

pub fn is_resource(p: *mut Com) -> bool {
    !p.is_null() && matches!(body(p), Body::Resource(_))
}

impl Resource {
    /// A depth buffer (typeless ones too): its kernel target's truth is
    /// in the depth and stencil planes.
    pub fn holds_depth(&self) -> bool {
        is_depth(self.format) || self.bind & BIND_DEPTH_STENCIL != 0
    }

    fn flush_device(&self) {
        if !self.device.is_null() {
            crate::device::of(self.device).flush();
        }
    }

    /// A subresource as 0xAARRGGBB texels, rows packed (from the kernel if
    /// drawn there; the device's waiting draws go first).
    pub fn read_argb(&mut self, s: usize) -> Vec<u32> {
        let sub = self.subs[s];
        if self.drawn[s] && !self.holds_depth() {
            self.flush_device();
            let mut px = vec![0u32; (sub.w * sub.h) as usize];
            let r = ReadArgs { x: 0, y: 0, w: sub.w as i32, h: sub.h as i32, dst: px.as_mut_ptr() as u64, stride: sub.w as i64 * 4 };
            gfx3d::gpu3d(3, self.targets[s], &r as *const _ as u64, 0);
            if !has_alpha(self.format) {
                for p in px.iter_mut() {
                    *p |= 0xFF00_0000;
                }
            }
            return px;
        }
        decode(self.format, sub.w, sub.h * sub.d, &self.data[sub.at..], sub.pitch)
    }

    /// Bring `data` up to date with what was drawn.
    pub fn sync(&mut self, s: usize) {
        if !self.drawn[s] {
            return;
        }
        let sub = self.subs[s];
        if self.holds_depth() {
            self.flush_device();
            let mut z = vec![0u32; (sub.w * sub.h) as usize];
            let mut st = vec![0u32; (sub.w * sub.h) as usize];
            for (which, buf) in [(1u32, &mut z), (2, &mut st)] {
                let p = PixelArgs { x: 0, y: 0, w: sub.w as i32, h: sub.h as i32, ptr: buf.as_mut_ptr() as u64, stride: sub.w as i64 * 4, which, mask: 0, scissor: [0; 4] };
                gfx3d::gpu3d(6, self.targets[s], &p as *const _ as u64, 0);
            }
            let bpp = bytes(self.format).unwrap_or(4) as usize;
            for y in 0..sub.h as usize {
                for x in 0..sub.w as usize {
                    let i = y * sub.w as usize + x;
                    let at = sub.at + y * sub.pitch as usize + x * bpp;
                    let zf = z[i] as f64 / DEPTH_MAX as f64;
                    let v: u32 = match self.format {
                        D24_UNORM_S8_UINT | R24G8_TYPELESS => z[i] | st[i] << 24,
                        D16_UNORM | R16_TYPELESS => (zf * 65535.0 + 0.5) as u32,
                        _ => (zf as f32).to_bits(),
                    };
                    let b = v.to_le_bytes();
                    self.data[at..at + bpp].copy_from_slice(&b[..bpp.min(4)]);
                }
            }
        } else {
            let px = self.read_argb(s);
            encode(self.format, sub.w, sub.h, &px, &mut self.data[sub.at..], sub.pitch);
        }
        self.drawn[s] = false;
    }

    /// The kernel target of subresource `s` (made, from `data`, if new).
    pub fn target(&mut self, s: usize) -> u64 {
        if self.targets[s] != 0 {
            return self.targets[s];
        }
        let sub = self.subs[s];
        let t = gfx3d::create_target(sub.w, sub.h);
        self.targets[s] = t;
        if t != 0 {
            self.push(s);
        }
        t
    }

    /// Write `data`'s subresource `s` into its kernel target, if it has one.
    fn push(&mut self, s: usize) {
        let t = self.targets[s];
        if t == 0 {
            return;
        }
        let sub = self.subs[s];
        let full = [0, 0, sub.w as i32, sub.h as i32];
        if self.holds_depth() {
            let bpp = bytes(self.format).unwrap_or(4) as usize;
            let mut z = vec![0u32; (sub.w * sub.h) as usize];
            let mut st = vec![0u32; (sub.w * sub.h) as usize];
            for y in 0..sub.h as usize {
                for x in 0..sub.w as usize {
                    let at = sub.at + y * sub.pitch as usize + x * bpp;
                    let mut b = [0u8; 4];
                    b[..bpp.min(4)].copy_from_slice(&self.data[at..at + bpp.min(4)]);
                    let v = u32::from_le_bytes(b);
                    let i = y * sub.w as usize + x;
                    (z[i], st[i]) = match self.format {
                        D24_UNORM_S8_UINT | R24G8_TYPELESS => (v & 0xFF_FFFF, v >> 24),
                        D16_UNORM | R16_TYPELESS => ((v as u64 * DEPTH_MAX as u64 / 65535) as u32, 0),
                        _ => ((f32::from_bits(v).clamp(0.0, 1.0) as f64 * DEPTH_MAX as f64) as u32, 0),
                    };
                }
            }
            for (which, buf, mask) in [(1u32, &z, 0xF), (2, &st, 0xFF)] {
                let p = PixelArgs { x: 0, y: 0, w: sub.w as i32, h: sub.h as i32, ptr: buf.as_ptr() as u64, stride: sub.w as i64 * 4, which, mask, scissor: full };
                gfx3d::gpu3d(5, t, &p as *const _ as u64, 0);
            }
        } else {
            let px = decode(self.format, sub.w, sub.h, &self.data[sub.at..], sub.pitch);
            let p = PixelArgs { x: 0, y: 0, w: sub.w as i32, h: sub.h as i32, ptr: px.as_ptr() as u64, stride: sub.w as i64 * 4, which: 0, mask: 0xF, scissor: full };
            gfx3d::gpu3d(5, t, &p as *const _ as u64, 0);
        }
        self.drawn[s] = false;
    }

    /// `data` changed in subresource `s` (by the program).
    pub fn changed(&mut self, s: usize) {
        self.flush_device();
        self.push(s);
        let slice = s / self.mips.max(1) as usize;
        if let Some(d) = self.dirty.get_mut(slice) {
            *d = true;
        }
    }

    /// The whole resource as one kernel texture (every array slice with
    /// all its mips; a 3D texture's slices), uploaded again if it changed
    /// or was drawn into. A depth buffer's texels hold its depth (24 bits)
    /// across red, green and blue.
    pub fn kernel_texture(&mut self) -> u64 {
        if self.textures.is_empty() || self.dim == DIM_BUFFER {
            return 0;
        }
        let drawn = self.drawn.iter().any(|&d| d);
        if self.textures[0] != 0 && !self.dirty.iter().any(|&d| d) && !drawn {
            return self.textures[0];
        }
        let depth = self.holds_depth();
        let bpp = bytes(self.format).unwrap_or(4) as usize;
        let mut texels = Vec::new();
        for s in 0..self.subs.len() {
            if !depth {
                texels.extend(self.read_argb(s));
                continue;
            }
            self.sync(s);
            let sub = self.subs[s];
            for y in 0..(sub.h * sub.d) as usize {
                for x in 0..sub.w as usize {
                    let at = sub.at + y * sub.pitch as usize + x * bpp;
                    let mut b = [0u8; 4];
                    b[..bpp.min(4)].copy_from_slice(&self.data[at..at + bpp.min(4)]);
                    let v = u32::from_le_bytes(b);
                    let z = match self.format {
                        D24_UNORM_S8_UINT | R24G8_TYPELESS => v & 0xFF_FFFF,
                        D16_UNORM | R16_TYPELESS => (v as u64 * DEPTH_MAX as u64 / 65535) as u32,
                        _ => (f32::from_bits(v).clamp(0.0, 1.0) as f64 * DEPTH_MAX as f64) as u32,
                    };
                    texels.push(0xFF00_0000 | z);
                }
            }
        }
        use gfx3d::raster3d::{TEX_1D_ARRAY, TEX_2D, TEX_2D_ARRAY, TEX_3D};
        let (layers, kind, depth3) = match self.dim {
            DIM_TEXTURE3D => (1, TEX_3D, self.d),
            DIM_TEXTURE1D if self.d > 1 => (self.d, TEX_1D_ARRAY, 0),
            _ if self.d > 1 => (self.d, TEX_2D_ARRAY, 0),
            _ => (1, TEX_2D, 0),
        };
        let args = TexArgs {
            w: self.w,
            h: if self.dim == DIM_TEXTURE1D { 1 } else { self.h },
            texels: texels.as_ptr() as u64,
            levels: self.mips,
            depth: depth3,
            layers,
            kind,
        };
        let h = gfx3d::upload_texture(self.textures[0], &args);
        if h != 0 {
            self.textures[0] = h;
        }
        self.dirty.iter_mut().for_each(|d| *d = false);
        self.textures[0]
    }
}

/// Lay out a resource's subresources in `data`.
fn layout(dim: u32, format: u32, w: u32, h: u32, d: u32, mips: u32, array: u32) -> (Vec<Sub>, usize) {
    let mut subs = Vec::new();
    let mut at = 0usize;
    for _ in 0..array {
        for l in 0..mips {
            let (mw, mh) = ((w >> l).max(1), (h >> l).max(1));
            let md = if dim == DIM_TEXTURE3D { (d >> l).max(1) } else { 1 };
            let pitch = format::pitch(format, mw);
            let slice = pitch * format::rows(format, mh);
            subs.push(Sub { at, w: mw, h: mh, d: md, pitch, slice });
            at += (slice * md) as usize;
        }
    }
    (subs, at)
}

fn mip_count(w: u32, h: u32, d: u32, mips: u32) -> u32 {
    let full = 32 - w.max(h).max(d).max(1).leading_zeros();
    if mips == 0 { full } else { mips.min(full) }
}

static BUFFER_IIDS: [Guid; 3] = [IID_ID3D11DEVICECHILD, IID_ID3D11RESOURCE, IID_ID3D11BUFFER];
static TEX1D_IIDS: [Guid; 3] = [IID_ID3D11DEVICECHILD, IID_ID3D11RESOURCE, IID_ID3D11TEXTURE1D];
static TEX2D_IIDS: [Guid; 3] = [IID_ID3D11DEVICECHILD, IID_ID3D11RESOURCE, IID_ID3D11TEXTURE2D];
static TEX3D_IIDS: [Guid; 3] = [IID_ID3D11DEVICECHILD, IID_ID3D11RESOURCE, IID_ID3D11TEXTURE3D];

/// A texture of any dimension from its description and initial data.
fn make_texture(dev: *mut Com, dim: u32, td: &Tex2DDesc, depth: u32, init: *const SubData) -> Result<*mut Com, i32> {
    let dev = main_of(dev);
    if bytes(td.format).is_none() || td.w == 0 || td.h == 0 || td.w > 16384 || td.h > 16384 || td.array == 0 {
        return Err(E_INVALIDARG);
    }
    if td.sample[0] > 1 {
        return Err(E_INVALIDARG);
    }
    // Automatic mips need a target that's also a shader resource.
    if td.misc & MISC_GENERATE_MIPS != 0 && td.bind & (BIND_RENDER_TARGET | BIND_SHADER_RESOURCE) != BIND_RENDER_TARGET | BIND_SHADER_RESOURCE {
        return Err(E_INVALIDARG);
    }
    // A depth buffer needs a depth format (a staging copy of one may have it
    // too); a depth format can't be a render target or shader resource.
    if (td.bind & BIND_DEPTH_STENCIL != 0 && !format::can_be_depth(td.format)) || (is_depth(td.format) && td.bind & (BIND_RENDER_TARGET | BIND_SHADER_RESOURCE) != 0) {
        return Err(E_INVALIDARG);
    }
    let mips = mip_count(td.w, if dim == DIM_TEXTURE1D { 1 } else { td.h }, depth, td.mips);
    let array = if dim == DIM_TEXTURE3D { 1 } else { td.array };
    let (subs, size) = layout(dim, td.format, td.w, td.h, depth, mips, array);
    let n = subs.len();
    let mut r = Resource {
        dim,
        format: td.format,
        w: td.w,
        h: td.h,
        d: if dim == DIM_TEXTURE3D { depth } else { array },
        mips,
        usage: td.usage,
        bind: td.bind,
        cpu: td.cpu,
        misc: td.misc,
        stride: 0,
        data: vec![0u8; size],
        subs,
        targets: vec![0; n],
        drawn: vec![false; n],
        textures: vec![0; array as usize],
        dirty: vec![true; array as usize],
        mapped: vec![false; n],
        priority: 0,
        so_filled: 0,
        device: dev,
    };
    if !init.is_null() {
        for (i, sub) in r.subs.clone().iter().enumerate() {
            let s = unsafe { *init.add(i) };
            if s.mem.is_null() {
                return Err(E_INVALIDARG);
            }
            let rows = format::rows(td.format, sub.h) as usize;
            for z in 0..sub.d as usize {
                for y in 0..rows {
                    let src = unsafe { s.mem.add(z * s.slice as usize + y * s.pitch as usize) };
                    let at = sub.at + z * sub.slice as usize + y * sub.pitch as usize;
                    unsafe { core::ptr::copy_nonoverlapping(src, r.data.as_mut_ptr().add(at), sub.pitch as usize) };
                }
            }
        }
    }
    let (vt, iids): (*const (), &'static [Guid]) = match dim {
        DIM_TEXTURE1D => (&ID3D11TEXTURE1D as *const _ as *const (), &TEX1D_IIDS),
        DIM_TEXTURE3D => (&ID3D11TEXTURE3D as *const _ as *const (), &TEX3D_IIDS),
        _ => (&ID3D11TEXTURE2D as *const _ as *const (), &TEX2D_IIDS),
    };
    let _ = vt;
    let o = match dim {
        DIM_TEXTURE1D => Com::new(&ID3D11TEXTURE1D, iids, dev, Body::Resource(Box::new(r))),
        DIM_TEXTURE3D => Com::new(&ID3D11TEXTURE3D, iids, dev, Body::Resource(Box::new(r))),
        _ => Com::new(&ID3D11TEXTURE2D, iids, dev, Body::Resource(Box::new(r))),
    };
    Ok(o)
}

pub fn make_texture2d(dev: *mut Com, td: &Tex2DDesc, init: *const SubData) -> Result<*mut Com, i32> {
    make_texture(dev, DIM_TEXTURE2D, td, 1, init)
}

pub unsafe extern "system" fn Dev_CreateTexture2D(this: *mut Com, desc: *const Tex2DDesc, init: *const SubData, out: *mut *mut Com) -> i32 {
    if desc.is_null() {
        return E_INVALIDARG;
    }
    match make_texture2d(this, unsafe { &*desc }, init) {
        Ok(o) if !out.is_null() => {
            unsafe { *out = o };
            S_OK
        }
        Ok(o) => {
            unsafe { release(o) };
            S_FALSE
        }
        Err(e) => e,
    }
}

/// D3D11_TEXTURE1D_DESC.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Tex1DDesc {
    pub w: u32,
    pub mips: u32,
    pub array: u32,
    pub format: u32,
    pub usage: u32,
    pub bind: u32,
    pub cpu: u32,
    pub misc: u32,
}

/// D3D11_TEXTURE3D_DESC.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Tex3DDesc {
    pub w: u32,
    pub h: u32,
    pub d: u32,
    pub mips: u32,
    pub format: u32,
    pub usage: u32,
    pub bind: u32,
    pub cpu: u32,
    pub misc: u32,
}

pub unsafe extern "system" fn Dev_CreateTexture1D(this: *mut Com, desc: *const Tex1DDesc, init: *const SubData, out: *mut *mut Com) -> i32 {
    if desc.is_null() {
        return E_INVALIDARG;
    }
    let d = unsafe { *desc };
    let td = Tex2DDesc { w: d.w, h: 1, mips: d.mips, array: d.array, format: d.format, sample: [1, 0], usage: d.usage, bind: d.bind, cpu: d.cpu, misc: d.misc };
    match make_texture(this, DIM_TEXTURE1D, &td, 1, init) {
        Ok(o) if !out.is_null() => {
            unsafe { *out = o };
            S_OK
        }
        Ok(o) => {
            unsafe { release(o) };
            S_FALSE
        }
        Err(e) => e,
    }
}

pub unsafe extern "system" fn Dev_CreateTexture3D(this: *mut Com, desc: *const Tex3DDesc, init: *const SubData, out: *mut *mut Com) -> i32 {
    if desc.is_null() {
        return E_INVALIDARG;
    }
    let d = unsafe { *desc };
    let td = Tex2DDesc { w: d.w, h: d.h, mips: d.mips, array: 1, format: d.format, sample: [1, 0], usage: d.usage, bind: d.bind, cpu: d.cpu, misc: d.misc };
    match make_texture(this, DIM_TEXTURE3D, &td, d.d.max(1), init) {
        Ok(o) if !out.is_null() => {
            unsafe { *out = o };
            S_OK
        }
        Ok(o) => {
            unsafe { release(o) };
            S_FALSE
        }
        Err(e) => e,
    }
}

pub unsafe extern "system" fn Dev_CreateBuffer(this: *mut Com, desc: *const BufferDesc, init: *const SubData, out: *mut *mut Com) -> i32 {
    if desc.is_null() {
        return E_INVALIDARG;
    }
    let d = unsafe { *desc };
    if d.size == 0 || d.size > 1 << 30 || (d.bind & BIND_CONSTANT_BUFFER != 0 && (d.size % 16 != 0 || d.size > 65536)) {
        return E_INVALIDARG;
    }
    let mut data = vec![0u8; d.size as usize];
    if !init.is_null() {
        let s = unsafe { *init };
        if s.mem.is_null() {
            return E_INVALIDARG;
        }
        unsafe { core::ptr::copy_nonoverlapping(s.mem, data.as_mut_ptr(), d.size as usize) };
    }
    let r = Resource {
        dim: DIM_BUFFER,
        format: 0,
        w: d.size,
        h: 1,
        d: 1,
        mips: 1,
        usage: d.usage,
        bind: d.bind,
        cpu: d.cpu,
        misc: d.misc,
        stride: d.stride,
        data,
        subs: vec![Sub { at: 0, w: d.size, h: 1, d: 1, pitch: d.size, slice: d.size }],
        targets: vec![0],
        drawn: vec![false],
        textures: Vec::new(),
        dirty: Vec::new(),
        mapped: vec![false],
        priority: 0,
        so_filled: 0,
        device: main_of(this),
    };
    if out.is_null() {
        return S_FALSE;
    }
    unsafe { *out = Com::new(&ID3D11BUFFER, &BUFFER_IIDS, main_of(this), Body::Resource(Box::new(r))) };
    S_OK
}

// -----| resource interfaces |-----

pub unsafe extern "system" fn Res_GetType(this: *mut Com, out: *mut u32) {
    if !out.is_null() {
        unsafe { *out = resource(this).dim };
    }
}

pub unsafe extern "system" fn Res_SetEvictionPriority(this: *mut Com, p: u32) {
    resource(this).priority = p;
}

pub unsafe extern "system" fn Res_GetEvictionPriority(this: *mut Com) -> u32 {
    resource(this).priority
}

pub unsafe extern "system" fn Buf_GetDesc(this: *mut Com, out: *mut BufferDesc) {
    let r = resource(this);
    if !out.is_null() {
        unsafe { *out = BufferDesc { size: r.w, usage: r.usage, bind: r.bind, cpu: r.cpu, misc: r.misc, stride: r.stride } };
    }
}

pub unsafe extern "system" fn T2_GetDesc(this: *mut Com, out: *mut Tex2DDesc) {
    let r = resource(this);
    if !out.is_null() {
        unsafe { *out = Tex2DDesc { w: r.w, h: r.h, mips: r.mips, array: r.d, format: r.format, sample: [1, 0], usage: r.usage, bind: r.bind, cpu: r.cpu, misc: r.misc } };
    }
}

pub unsafe extern "system" fn T1_GetDesc(this: *mut Com, out: *mut Tex1DDesc) {
    let r = resource(this);
    if !out.is_null() {
        unsafe { *out = Tex1DDesc { w: r.w, mips: r.mips, array: r.d, format: r.format, usage: r.usage, bind: r.bind, cpu: r.cpu, misc: r.misc } };
    }
}

pub unsafe extern "system" fn T3_GetDesc(this: *mut Com, out: *mut Tex3DDesc) {
    let r = resource(this);
    if !out.is_null() {
        unsafe { *out = Tex3DDesc { w: r.w, h: r.h, d: r.d, mips: r.mips, format: r.format, usage: r.usage, bind: r.bind, cpu: r.cpu, misc: r.misc } };
    }
}

// The DXGI side of a resource.

pub unsafe extern "system" fn DxRes_GetSharedHandle(_this: *mut Com, out: *mut usize) -> i32 {
    if !out.is_null() {
        unsafe { *out = 0 };
    }
    E_NOTIMPL
}

pub unsafe extern "system" fn DxRes_GetUsage(_this: *mut Com, out: *mut u32) -> i32 {
    if !out.is_null() {
        // DXGI_USAGE_SHADER_INPUT | RENDER_TARGET_OUTPUT.
        unsafe { *out = 0x10 | 0x20 };
    }
    S_OK
}

pub unsafe extern "system" fn DxRes_SetEvictionPriority(this: *mut Com, p: u32) -> i32 {
    resource(this).priority = p;
    S_OK
}

pub unsafe extern "system" fn DxRes_GetEvictionPriority(this: *mut Com, out: *mut u32) -> i32 {
    if !out.is_null() {
        unsafe { *out = resource(this).priority };
    }
    S_OK
}

pub unsafe extern "system" fn Surf_GetDesc(this: *mut Com, out: *mut [u32; 5]) -> i32 {
    let r = resource(this);
    if out.is_null() {
        return E_INVALIDARG;
    }
    unsafe { *out = [r.w, r.h, r.format, 1, 0] };
    S_OK
}

/// DXGI_MAPPED_RECT for subresource 0.
pub unsafe extern "system" fn Surf_Map(this: *mut Com, out: *mut (i32, *mut u8), _flags: u32) -> i32 {
    let r = resource(this);
    if out.is_null() {
        return E_INVALIDARG;
    }
    r.sync(0);
    r.mapped[0] = true;
    unsafe { *out = (r.subs[0].pitch as i32, r.data.as_mut_ptr()) };
    S_OK
}

pub unsafe extern "system" fn Surf_Unmap(this: *mut Com) -> i32 {
    let r = resource(this);
    if r.mapped[0] {
        r.mapped[0] = false;
        r.changed(0);
    }
    S_OK
}

// -----| views |-----

pub const VIEW_SRV: u32 = 0;
pub const VIEW_RTV: u32 = 1;
pub const VIEW_DSV: u32 = 2;
pub const VIEW_UAV: u32 = 3;

pub struct View {
    pub kind: u32,
    /// The resource (held).
    pub res: *mut Com,
    pub format: u32,
    /// The view dimension (its own enumeration per kind).
    pub dim: u32,
    pub mip: u32,
    pub mips: u32,
    pub first: u32,
    pub slices: u32,
    pub flags: u32,
    /// GetDesc's answer.
    pub desc: [u32; 6],
}

impl Drop for View {
    fn drop(&mut self) {
        unsafe { release(self.res) };
    }
}

pub fn view<'a>(p: *mut Com) -> Option<&'a mut View> {
    match unsafe { p.as_ref() } {
        Some(_) => match body(p) {
            Body::View(v) => Some(v),
            _ => None,
        },
        None => None,
    }
}

static SRV_IIDS: [Guid; 3] = [IID_ID3D11DEVICECHILD, IID_ID3D11VIEW, IID_ID3D11SHADERRESOURCEVIEW];
static RTV_IIDS: [Guid; 3] = [IID_ID3D11DEVICECHILD, IID_ID3D11VIEW, IID_ID3D11RENDERTARGETVIEW];
static DSV_IIDS: [Guid; 3] = [IID_ID3D11DEVICECHILD, IID_ID3D11VIEW, IID_ID3D11DEPTHSTENCILVIEW];
static UAV_IIDS: [Guid; 3] = [IID_ID3D11DEVICECHILD, IID_ID3D11VIEW, IID_ID3D11UNORDEREDACCESSVIEW];

/// A view of `res`: its description (6 words: format, dimension, then the
/// dimension's fields; a DSV's flags at 2 and its fields from 3) or the
/// resource's whole.
unsafe fn make_view(dev: *mut Com, kind: u32, res: *mut Com, desc: *const [u32; 6], out: *mut *mut Com) -> i32 {
    if !is_resource(res) {
        return E_INVALIDARG;
    }
    let r = resource(res);
    let need = match kind {
        VIEW_SRV => BIND_SHADER_RESOURCE,
        VIEW_RTV => BIND_RENDER_TARGET,
        VIEW_DSV => BIND_DEPTH_STENCIL,
        _ => 0x80,
    };
    if r.bind & need == 0 {
        return E_INVALIDARG;
    }
    // The defaults: the whole resource, its format.
    let array = r.dim != DIM_TEXTURE3D && r.d > 1;
    let cube = r.misc & MISC_TEXTURECUBE != 0;
    let default_dim = match (kind, r.dim) {
        (_, DIM_BUFFER) => 1,
        (VIEW_DSV, DIM_TEXTURE1D) => if array { 2 } else { 1 },
        (VIEW_DSV, _) => if array { 4 } else { 3 },
        (_, DIM_TEXTURE1D) => if array { 3 } else { 2 },
        (VIEW_SRV, DIM_TEXTURE2D) if cube => if r.d > 6 { 10 } else { 9 },
        (_, DIM_TEXTURE2D) => if array { 5 } else { 4 },
        _ => 8,
    };
    let mut d: [u32; 6] = if desc.is_null() {
        let mut d = [r.format, default_dim, 0, 0, 0, 0];
        match kind {
            VIEW_SRV => {
                (d[2], d[3]) = (0, r.mips);
                if array && !cube {
                    (d[4], d[5]) = (0, r.d);
                }
                if r.dim == DIM_BUFFER {
                    (d[2], d[3]) = (0, r.w / r.stride.max(1));
                }
            }
            VIEW_DSV => {
                if array {
                    (d[4], d[5]) = (0, r.d);
                }
            }
            _ => {
                if array {
                    (d[3], d[4]) = (0, r.d);
                }
                if r.dim == DIM_TEXTURE3D {
                    (d[3], d[4]) = (0, r.d);
                }
            }
        }
        d
    } else {
        unsafe { *desc }
    };
    if d[0] == 0 {
        d[0] = r.format;
    }
    // A typeless resource's view names a format.
    let format = d[0];
    let (mip, mips, first, slices, flags) = match kind {
        VIEW_SRV => match d[1] {
            5 | 3 | 10 => (d[2], d[3], d[4], d[5]),
            _ => (d[2], d[3], 0, if cube { 6 } else { 1 }),
        }
        .into_flags(0),
        VIEW_DSV => match d[1] {
            2 | 4 => (d[3], 1, d[4], d[5], d[2]),
            _ => (d[3], 1, 0, 1, d[2]),
        },
        _ => match d[1] {
            3 | 5 | 8 => (d[2], 1, d[3], d[4], 0),
            _ => (d[2], 1, 0, 1, 0),
        },
    };
    let mips = if mips == u32::MAX { r.mips - mip.min(r.mips - 1) } else { mips };
    if r.dim != DIM_BUFFER && mip >= r.mips {
        return E_INVALIDARG;
    }
    if out.is_null() {
        return S_FALSE;
    }
    unsafe { add_ref(res) };
    let v = View { kind, res: main_of(res), format, dim: d[1], mip, mips, first, slices: slices.max(1), flags, desc: d };
    let (iids, o): (&'static [Guid], *mut Com) = match kind {
        VIEW_SRV => (&SRV_IIDS, core::ptr::null_mut()),
        VIEW_RTV => (&RTV_IIDS, core::ptr::null_mut()),
        VIEW_DSV => (&DSV_IIDS, core::ptr::null_mut()),
        _ => (&UAV_IIDS, core::ptr::null_mut()),
    };
    let _ = o;
    let dev = main_of(dev);
    let obj = match kind {
        VIEW_SRV => Com::new(&ID3D11SHADERRESOURCEVIEW, iids, dev, Body::View(Box::new(v))),
        VIEW_RTV => Com::new(&ID3D11RENDERTARGETVIEW, iids, dev, Body::View(Box::new(v))),
        VIEW_DSV => Com::new(&ID3D11DEPTHSTENCILVIEW, iids, dev, Body::View(Box::new(v))),
        _ => Com::new(&ID3D11UNORDEREDACCESSVIEW, iids, dev, Body::View(Box::new(v))),
    };
    unsafe { *out = obj };
    S_OK
}

trait IntoFlags {
    fn into_flags(self, f: u32) -> (u32, u32, u32, u32, u32);
}

impl IntoFlags for (u32, u32, u32, u32) {
    fn into_flags(self, f: u32) -> (u32, u32, u32, u32, u32) {
        (self.0, self.1, self.2, self.3, f)
    }
}

pub unsafe extern "system" fn Dev_CreateShaderResourceView(this: *mut Com, res: *mut Com, desc: *const [u32; 6], out: *mut *mut Com) -> i32 {
    unsafe { make_view(this, VIEW_SRV, res, desc, out) }
}

pub unsafe extern "system" fn Dev_CreateRenderTargetView(this: *mut Com, res: *mut Com, desc: *const [u32; 6], out: *mut *mut Com) -> i32 {
    unsafe { make_view(this, VIEW_RTV, res, desc, out) }
}

pub unsafe extern "system" fn Dev_CreateDepthStencilView(this: *mut Com, res: *mut Com, desc: *const [u32; 6], out: *mut *mut Com) -> i32 {
    unsafe { make_view(this, VIEW_DSV, res, desc, out) }
}

pub unsafe extern "system" fn Dev_CreateUnorderedAccessView(this: *mut Com, res: *mut Com, desc: *const [u32; 6], out: *mut *mut Com) -> i32 {
    unsafe { make_view(this, VIEW_UAV, res, desc, out) }
}

pub unsafe extern "system" fn View_GetResource(this: *mut Com, out: *mut *mut Com) {
    if let Some(v) = view(this) {
        unsafe { out_ref(out, v.res) };
    }
}

unsafe fn view_desc(this: *mut Com, out: *mut u32, words: usize) {
    if let (Some(v), false) = (view(this), out.is_null()) {
        for i in 0..words {
            unsafe { *out.add(i) = v.desc[i] };
        }
    }
}

pub unsafe extern "system" fn SRV_GetDesc(this: *mut Com, out: *mut u32) {
    unsafe { view_desc(this, out, 6) }
}
pub unsafe extern "system" fn RTV_GetDesc(this: *mut Com, out: *mut u32) {
    unsafe { view_desc(this, out, 5) }
}
pub unsafe extern "system" fn DSV_GetDesc(this: *mut Com, out: *mut u32) {
    unsafe { view_desc(this, out, 6) }
}
pub unsafe extern "system" fn UAV_GetDesc(this: *mut Com, out: *mut u32) {
    unsafe { view_desc(this, out, 5) }
}

impl View {
    /// The subresource a render target or depth view draws into.
    pub fn sub(&self) -> usize {
        let r = resource(self.res);
        (self.mip + self.first * r.mips) as usize
    }
}

// -----| Map, updates, copies, clears (the context's) |-----

/// D3D11_MAPPED_SUBRESOURCE.
#[repr(C)]
pub struct Mapped {
    pub data: *mut u8,
    pub row: u32,
    pub depth: u32,
}

pub unsafe extern "system" fn Ctx_Map(_this: *mut Com, res: *mut Com, sub: u32, kind: u32, _flags: u32, out: *mut Mapped) -> i32 {
    if !is_resource(res) {
        return E_INVALIDARG;
    }
    let r = resource(res);
    let s = sub as usize;
    if s >= r.subs.len() || !(1..=5).contains(&kind) {
        return E_INVALIDARG;
    }
    // Reading what was drawn: the draws go first. Draws already waiting
    // read their vertices and constants when made, so writing is free.
    if kind == 1 || kind == 3 {
        r.sync(s);
    }
    let su = r.subs[s];
    r.mapped[s] = kind != 1;
    if !out.is_null() {
        unsafe { *out = Mapped { data: r.data.as_mut_ptr().add(su.at), row: su.pitch, depth: su.slice } };
    }
    S_OK
}

pub unsafe extern "system" fn Ctx_Unmap(_this: *mut Com, res: *mut Com, sub: u32) {
    if !is_resource(res) {
        return;
    }
    let r = resource(res);
    let s = sub as usize;
    if s < r.mapped.len() && r.mapped[s] {
        r.mapped[s] = false;
        r.changed(s);
    }
}

/// D3D11_BOX.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Box3 {
    pub left: u32,
    pub top: u32,
    pub front: u32,
    pub right: u32,
    pub bottom: u32,
    pub back: u32,
}

pub unsafe extern "system" fn Ctx_UpdateSubresource(_this: *mut Com, res: *mut Com, sub: u32, b: *const Box3, src: *const u8, pitch: u32, depth_pitch: u32) {
    if !is_resource(res) || src.is_null() {
        return;
    }
    let r = resource(res);
    let s = sub as usize;
    if s >= r.subs.len() {
        return;
    }
    r.sync(s);
    let su = r.subs[s];
    let bx = if b.is_null() { Box3 { left: 0, top: 0, front: 0, right: su.w, bottom: su.h, back: su.d } } else { unsafe { *b } };
    if r.dim == DIM_BUFFER {
        let (a, e) = (bx.left as usize, (bx.right as usize).min(r.data.len()));
        if a < e {
            unsafe { core::ptr::copy_nonoverlapping(src, r.data.as_mut_ptr().add(a), e - a) };
        }
    } else {
        let bpp = bytes(r.format).unwrap_or(4) as usize;
        let bc = is_bc(r.format);
        let (x0, y0, x1, y1) = if bc { (bx.left / 4, bx.top / 4, bx.right.div_ceil(4), bx.bottom.div_ceil(4)) } else { (bx.left, bx.top, bx.right, bx.bottom) };
        let row = (x1.saturating_sub(x0)) as usize * bpp;
        for z in bx.front..bx.back.min(su.d) {
            for y in y0..y1 {
                let from = unsafe { src.add((z - bx.front) as usize * depth_pitch as usize + (y - y0) as usize * pitch as usize) };
                let at = su.at + z as usize * su.slice as usize + y as usize * su.pitch as usize + x0 as usize * bpp;
                if at + row <= r.data.len() {
                    unsafe { core::ptr::copy_nonoverlapping(from, r.data.as_mut_ptr().add(at), row) };
                }
            }
        }
    }
    r.changed(s);
}

/// Copy subresource `ss` of `src` (the box `b`, or all) to (x, y, z) of
/// subresource `ds` of `dst`.
fn copy_sub(dst: *mut Com, ds: usize, (x, y, z): (u32, u32, u32), src: *mut Com, ss: usize, b: Option<Box3>) {
    let same = main_of(dst) == main_of(src);
    let (d, s) = (resource(dst), resource(src));
    if ds >= d.subs.len() || ss >= s.subs.len() {
        return;
    }
    if d.dim == DIM_BUFFER && s.dim == DIM_BUFFER {
        let bx = b.unwrap_or(Box3 { left: 0, top: 0, front: 0, right: s.w, bottom: 1, back: 1 });
        let n = (bx.right.saturating_sub(bx.left) as usize).min(s.data.len().saturating_sub(bx.left as usize)).min(d.data.len().saturating_sub(x as usize));
        if same {
            d.data.copy_within(bx.left as usize..bx.left as usize + n, x as usize);
        } else {
            d.data[x as usize..x as usize + n].copy_from_slice(&s.data[bx.left as usize..bx.left as usize + n]);
        }
        d.changed(0);
        return;
    }
    s.sync(ss);
    d.sync(ds);
    let (dsu, ssu) = (d.subs[ds], s.subs[ss]);
    let bx = b.unwrap_or(Box3 { left: 0, top: 0, front: 0, right: ssu.w, bottom: ssu.h, back: ssu.d });
    let bpp = bytes(s.format).unwrap_or(4) as usize;
    let bc = is_bc(s.format);
    let (sx, sy, ex, ey, dx, dy) = if bc {
        (bx.left / 4, bx.top / 4, bx.right.div_ceil(4), bx.bottom.div_ceil(4), x / 4, y / 4)
    } else {
        (bx.left, bx.top, bx.right, bx.bottom, x, y)
    };
    let row = ex.saturating_sub(sx) as usize * bpp;
    let src_bytes: Vec<u8> = if same { s.data.clone() } else { Vec::new() };
    for zz in 0..bx.back.saturating_sub(bx.front).min(dsu.d.saturating_sub(z)) {
        for yy in 0..ey.saturating_sub(sy) {
            let from = ssu.at + (bx.front + zz) as usize * ssu.slice as usize + (sy + yy) as usize * ssu.pitch as usize + sx as usize * bpp;
            let to = dsu.at + (z + zz) as usize * dsu.slice as usize + (dy + yy) as usize * dsu.pitch as usize + dx as usize * bpp;
            if to + row > d.data.len() {
                continue;
            }
            if same {
                d.data[to..to + row].copy_from_slice(&src_bytes[from..from + row]);
            } else if from + row <= s.data.len() {
                d.data[to..to + row].copy_from_slice(&s.data[from..from + row]);
            }
        }
    }
    let _ = ex;
    d.changed(ds);
}

pub unsafe extern "system" fn Ctx_CopyResource(_this: *mut Com, dst: *mut Com, src: *mut Com) {
    if !is_resource(dst) || !is_resource(src) {
        return;
    }
    let n = resource(src).subs.len().min(resource(dst).subs.len());
    for i in 0..n {
        copy_sub(dst, i, (0, 0, 0), src, i, None);
    }
}

pub unsafe extern "system" fn Ctx_CopySubresourceRegion(_this: *mut Com, dst: *mut Com, ds: u32, x: u32, y: u32, z: u32, src: *mut Com, ss: u32, b: *const Box3) {
    if !is_resource(dst) || !is_resource(src) {
        return;
    }
    copy_sub(dst, ds as usize, (x, y, z), src, ss as usize, if b.is_null() { None } else { Some(unsafe { *b }) });
}

/// 0 ..= 1 to a byte as Direct3D converts: x 255 in single precision,
/// rounded to the nearest (halves to even).
fn unorm(x: f32) -> u32 {
    let x = if x > 0.0 { x } else { 0.0 };
    let x = if x < 1.0 { x } else { 1.0 };
    let v = x * 255.0;
    let r = (v + 0.5) as u32;
    if r as f32 - v == 0.5 && r & 1 == 1 { r - 1 } else { r }
}

pub unsafe extern "system" fn Ctx_ClearRenderTargetView(this: *mut Com, rtv: *mut Com, color: *const [f32; 4]) {
    let (Some(v), false) = (view(rtv), color.is_null()) else { return };
    let c = unsafe { *color };
    crate::device::of(main_of(this)).flush();
    let r = resource(v.res);
    let s = v.sub();
    let su = r.subs[s];
    let t = r.target(s);
    let argb = unorm(c[3]) << 24 | unorm(c[0]) << 16 | unorm(c[1]) << 8 | unorm(c[2]);
    let args = ClearArgs { mask: 0x4000, color: argb, depth: 0, color_mask: 0xF, scissor: [0, 0, su.w as i32, su.h as i32], stencil: 0, stencil_write: 0 };
    gfx3d::gpu3d(1, t, &args as *const _ as u64, 0);
    r.drawn[s] = true;
}

pub unsafe extern "system" fn Ctx_ClearDepthStencilView(this: *mut Com, dsv: *mut Com, flags: u32, depth: f32, stencil: u8) {
    let Some(v) = view(dsv) else { return };
    crate::device::of(main_of(this)).flush();
    let r = resource(v.res);
    let s = v.sub();
    let su = r.subs[s];
    let t = r.target(s);
    let mut mask = 0;
    if flags & 1 != 0 {
        mask |= 0x100;
    }
    if flags & 2 != 0 {
        mask |= 0x400;
    }
    let args = ClearArgs {
        mask,
        color: 0,
        depth: (depth.clamp(0.0, 1.0) as f64 * DEPTH_MAX as f64 + 0.5) as u32,
        color_mask: 0,
        scissor: [0, 0, su.w as i32, su.h as i32],
        stencil: stencil as u32,
        stencil_write: 0xFF,
    };
    gfx3d::gpu3d(1, t, &args as *const _ as u64, 0);
    r.drawn[s] = true;
}

/// Each mip from the one above, 2x2 averaged.
pub unsafe extern "system" fn Ctx_GenerateMips(_this: *mut Com, srv: *mut Com) {
    let Some(v) = view(srv) else { return };
    let r = resource(v.res);
    if r.dim != DIM_TEXTURE2D || is_bc(r.format) {
        return;
    }
    let m = r.mips as usize;
    for slice in 0..r.d as usize {
        for l in 1..m {
            let above = r.read_argb(slice * m + l - 1);
            let (a, b) = (r.subs[slice * m + l - 1], r.subs[slice * m + l]);
            let mut next = vec![0u32; (b.w * b.h) as usize];
            for y in 0..b.h as usize {
                for x in 0..b.w as usize {
                    let px = |xx: usize, yy: usize| above[yy.min(a.h as usize - 1) * a.w as usize + xx.min(a.w as usize - 1)];
                    let q = [px(2 * x, 2 * y), px(2 * x + 1, 2 * y), px(2 * x, 2 * y + 1), px(2 * x + 1, 2 * y + 1)];
                    let mut c = 0u32;
                    for sh in [0, 8, 16, 24] {
                        // The mean, rounded with halves to even.
                        let sum: u32 = q.iter().map(|p| p >> sh & 0xFF).sum();
                        let (m, rem) = (sum / 4, sum % 4);
                        let m = if rem > 2 || (rem == 2 && m & 1 == 1) { m + 1 } else { m };
                        c |= m << sh;
                    }
                    next[y * b.w as usize + x] = c;
                }
            }
            let s = slice * m + l;
            r.drawn[s] = false;
            encode(r.format, b.w, b.h, &next, &mut r.data[b.at..], b.pitch);
            r.changed(s);
        }
    }
}
