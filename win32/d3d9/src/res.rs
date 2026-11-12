//! Resources: surfaces, textures (2D and cube), vertex and index buffers,
//! vertex declarations, and the device methods that make and copy them.
//!
//! Where pixels live: a plain surface (off-screen, or a texture's level)
//! keeps them in its own format in memory, as the program locks them. A
//! render target's are in a kernel target (VEIL_GPU3D), read back when
//! locked or copied. A depth buffer's are in a kernel target too: its own
//! at first, and then whichever render target it was last drawn with (the
//! kernel keeps colour, depth and stencil together), moved over when it
//! is used with another (`Device::bind_depth`).

use alloc::vec;
use alloc::vec::Vec;

use gfx3d::raster3d::{PixelArgs, ReadArgs, TexArgs};

use crate::com::*;
use crate::format::{self, *};
use crate::vtbl;

pub const USAGE_RENDERTARGET: u32 = 1;
pub const USAGE_DEPTHSTENCIL: u32 = 2;
pub const USAGE_AUTOGENMIPMAP: u32 = 0x400;
pub const POOL_DEFAULT: u32 = 0;
pub const POOL_MANAGED: u32 = 1;
const LOCK_READONLY: u32 = 0x10;

static SURFACE_IIDS: [Guid; 2] = [IID_RESOURCE9, IID_SURFACE9];
static TEXTURE_IIDS: [Guid; 3] = [IID_RESOURCE9, IID_BASETEXTURE9, IID_TEXTURE9];
static CUBE_IIDS: [Guid; 3] = [IID_RESOURCE9, IID_BASETEXTURE9, IID_CUBETEXTURE9];
static VB_IIDS: [Guid; 2] = [IID_RESOURCE9, IID_VERTEXBUFFER9];
static IB_IIDS: [Guid; 2] = [IID_RESOURCE9, IID_INDEXBUFFER9];
static DECL_IIDS: [Guid; 1] = [IID_DECL9];

#[repr(C)]
struct SurfaceDesc {
    format: u32,
    rtype: u32,
    usage: u32,
    pool: u32,
    multisample: u32,
    quality: u32,
    w: u32,
    h: u32,
}

#[repr(C)]
struct LockedRect {
    pitch: i32,
    bits: *mut u8,
}

// -----| surfaces |-----

pub struct Surface {
    pub format: u32,
    pub w: u32,
    pub h: u32,
    pub usage: u32,
    pub pool: u32,
    /// The pixels in `format` (a plain surface; a render target's while
    /// it's locked), rows `pitch` bytes apart.
    pub data: Vec<u8>,
    pub pitch: u32,
    /// A render target's or depth buffer's own kernel target, or 0.
    pub target: u64,
    /// A depth buffer: the kernel target its depth and stencil are in now.
    pub home: u64,
    /// Drawn into since a texture last read it (a texture's render target).
    pub drawn: bool,
    pub locked: bool,
    lock_writes: bool,
    /// Its place in a texture: level and cube face.
    pub level: u32,
    pub face: u32,
}

impl Surface {
    pub fn new(format: u32, w: u32, h: u32, usage: u32, pool: u32) -> Option<Surface> {
        let (w, h) = (w.max(1), h.max(1));
        let mut s = Surface {
            format,
            w,
            h,
            usage,
            pool,
            data: Vec::new(),
            pitch: format::pitch(format, w),
            target: 0,
            home: 0,
            drawn: false,
            locked: false,
            lock_writes: false,
            level: 0,
            face: 0,
        };
        if usage & (USAGE_RENDERTARGET | USAGE_DEPTHSTENCIL) != 0 {
            s.target = gfx3d::create_target(w, h);
            if s.target == 0 {
                return None;
            }
            s.home = s.target;
        } else {
            s.data = vec![0u8; (s.pitch * rows(format, h)) as usize];
        }
        Some(s)
    }

    pub fn is_target(&self) -> bool {
        self.usage & USAGE_RENDERTARGET != 0
    }

    pub fn is_depth(&self) -> bool {
        self.usage & USAGE_DEPTHSTENCIL != 0
    }

    /// Its pixels as 0xAARRGGBB, rows packed (a render target's from the
    /// kernel; the device's batch must have been flushed).
    pub fn read_argb(&self) -> Vec<u32> {
        if self.is_target() {
            let mut px = vec![0u32; (self.w * self.h) as usize];
            let r = ReadArgs { x: 0, y: 0, w: self.w as i32, h: self.h as i32, dst: px.as_mut_ptr() as u64, stride: self.w as i64 * 4 };
            gfx3d::gpu3d(3, self.target, &r as *const _ as u64, 0);
            if !has_alpha(self.format) {
                for p in px.iter_mut() {
                    *p |= 0xFF00_0000;
                }
            }
            return px;
        }
        if self.is_depth() {
            return vec![0; (self.w * self.h) as usize];
        }
        decode(self.format, self.w, self.h, &self.data, self.pitch)
    }

    /// Replace its pixels (0xAARRGGBB, rows packed) inside `r` (x0, y0, x1,
    /// y1); not for DXT.
    pub fn write_argb(&mut self, px: &[u32], r: [i32; 4]) {
        if self.is_target() {
            let p = PixelArgs {
                x: 0,
                y: 0,
                w: self.w as i32,
                h: self.h as i32,
                ptr: px.as_ptr() as u64,
                stride: self.w as i64 * 4,
                which: 0,
                mask: 0xF,
                scissor: r,
            };
            gfx3d::gpu3d(5, self.target, &p as *const _ as u64, 0);
            self.drawn = true;
        } else if !self.is_depth() && !is_dxt(self.format) {
            let mut cur = self.read_argb();
            for y in r[1].max(0)..r[3].min(self.h as i32) {
                for x in r[0].max(0)..r[2].min(self.w as i32) {
                    let i = (y as u32 * self.w + x as u32) as usize;
                    cur[i] = px[i];
                }
            }
            encode(self.format, self.w, self.h, &cur, &mut self.data, self.pitch);
        }
    }
}

/// Kernel handles of resources freed while draws using them may still be
/// waiting in a batch: closed at the next flush.
pub static PENDING_CLOSE: crate::dllrt::Lock<Vec<u64>> = crate::dllrt::Lock::new(Vec::new());

fn close_later(h: u64) {
    if h != 0 {
        PENDING_CLOSE.lock().push(h);
    }
}

impl Drop for Surface {
    fn drop(&mut self) {
        close_later(self.target);
    }
}

/// A new surface object (with no container).
pub fn new_surface(device: *mut Com, s: Surface) -> *mut Com {
    Com::new(&vtbl::IDIRECT3DSURFACE9, &SURFACE_IIDS, device, Body::Surface(s))
}

fn flush_device(this: *mut Com) {
    let d = unsafe { (*this).device };
    if !d.is_null() {
        unsafe { device(d) }.flush();
    }
}

/// A texture whose level this surface is: mark it to upload again.
fn touch_container(this: *mut Com) {
    let c = unsafe { (*this).container };
    if is_texture(c) {
        unsafe { texture(c) }.dirty = true;
    }
}

pub unsafe extern "system" fn Surf_GetContainer(this: *mut Com, iid: *const Guid, out: *mut *mut Com) -> i32 {
    let c = unsafe { (*this).container };
    let c = if c.is_null() { unsafe { (*this).device } } else { c };
    if c.is_null() {
        return E_NOINTERFACE;
    }
    unsafe { Unk_QueryInterface(c, iid, out) }
}

pub unsafe extern "system" fn Surf_GetDesc(this: *mut Com, out: *mut u8) -> i32 {
    let s = unsafe { surface(this) };
    if out.is_null() {
        return D3DERR_INVALIDCALL;
    }
    let d = SurfaceDesc { format: s.format, rtype: 1, usage: s.usage, pool: s.pool, multisample: 0, quality: 0, w: s.w, h: s.h };
    unsafe { core::ptr::write_unaligned(out as *mut SurfaceDesc, d) };
    S_OK
}

pub unsafe extern "system" fn Surf_LockRect(this: *mut Com, out: *mut u8, rect: *const [i32; 4], flags: u32) -> i32 {
    let s = unsafe { surface(this) };
    if out.is_null() || s.locked {
        return D3DERR_INVALIDCALL;
    }
    if s.is_depth() && s.format != D16_LOCKABLE {
        return D3DERR_INVALIDCALL;
    }
    if s.is_target() {
        // Read back into memory for as long as it's locked.
        flush_device(this);
        let px = s.read_argb();
        s.data = vec![0u8; (s.pitch * s.h) as usize];
        encode(s.format, s.w, s.h, &px, &mut s.data, s.pitch);
    }
    if s.data.is_empty() {
        return D3DERR_INVALIDCALL;
    }
    let (x, y) = if rect.is_null() { (0, 0) } else { unsafe { ((*rect)[0].max(0) as u32, (*rect)[1].max(0) as u32) } };
    // A DXT rectangle starts on a block.
    let off = if is_dxt(s.format) { (y / 4) * s.pitch + (x / 4) * bytes(s.format) } else { y * s.pitch + x * bytes(s.format) };
    if off as usize >= s.data.len() {
        return D3DERR_INVALIDCALL;
    }
    unsafe { core::ptr::write_unaligned(out as *mut LockedRect, LockedRect { pitch: s.pitch as i32, bits: s.data.as_mut_ptr().add(off as usize) }) };
    s.locked = true;
    s.lock_writes = flags & LOCK_READONLY == 0;
    S_OK
}

pub unsafe extern "system" fn Surf_UnlockRect(this: *mut Com) -> i32 {
    let s = unsafe { surface(this) };
    if !s.locked {
        return D3DERR_INVALIDCALL;
    }
    s.locked = false;
    if s.is_target() {
        if s.lock_writes {
            let px = decode(s.format, s.w, s.h, &s.data, s.pitch);
            let full = [0, 0, s.w as i32, s.h as i32];
            s.write_argb(&px, full);
        }
        s.data = Vec::new();
    }
    if s.lock_writes {
        touch_container(this);
    }
    S_OK
}

// -----| textures |-----

pub struct Texture {
    pub cube: bool,
    pub format: u32,
    pub w: u32,
    pub h: u32,
    pub levels: u32,
    pub usage: u32,
    pub pool: u32,
    /// Each face's levels, face after face.
    pub surfaces: Vec<*mut Com>,
    /// The kernel's copy (0: none yet), and whether it's out of date.
    pub handle: u64,
    pub dirty: bool,
    /// SetLOD: the most detailed level used.
    pub lod: u32,
    pub autogen_filter: u32,
}

impl Drop for Texture {
    fn drop(&mut self) {
        for &s in &self.surfaces {
            unsafe { free(s) };
        }
        close_later(self.handle);
    }
}

impl Texture {
    fn surface(&self, face: u32, level: u32) -> Option<*mut Com> {
        if level >= self.levels {
            return None;
        }
        self.surfaces.get((face * self.levels + level) as usize).copied()
    }

    /// Its kernel texture, uploaded again if it changed (face 0 of a cube
    /// map: the kernel samples 2D textures only).
    pub fn kernel(&mut self) -> u64 {
        let drawn = self.surfaces.iter().any(|&s| unsafe { surface(s) }.drawn);
        if self.handle != 0 && !self.dirty && !drawn {
            return self.handle;
        }
        let mut texels: Vec<u32> = Vec::new();
        let mut n = 0;
        for l in 0..self.levels {
            let s = unsafe { surface(self.surfaces[l as usize]) };
            s.drawn = false;
            texels.extend(s.read_argb());
            n += 1;
        }
        // Automatic mipmaps: each level from the one above, 2x2 averaged.
        if self.usage & USAGE_AUTOGENMIPMAP != 0 {
            let (mut w, mut h, mut at) = (self.w as usize, self.h as usize, 0usize);
            while w > 1 || h > 1 {
                let (nw, nh) = ((w / 2).max(1), (h / 2).max(1));
                let mut next = vec![0u32; nw * nh];
                for y in 0..nh {
                    for x in 0..nw {
                        let px = |xx: usize, yy: usize| texels[at + yy.min(h - 1) * w + xx.min(w - 1)];
                        let q = [px(2 * x, 2 * y), px(2 * x + 1, 2 * y), px(2 * x, 2 * y + 1), px(2 * x + 1, 2 * y + 1)];
                        let mut c = 0u32;
                        for sh in [0, 8, 16, 24] {
                            let sum: u32 = q.iter().map(|p| p >> sh & 0xFF).sum();
                            c |= ((sum + 2) / 4) << sh;
                        }
                        next[y * nw + x] = c;
                    }
                }
                at += w * h;
                texels.extend(next);
                n += 1;
                (w, h) = (nw, nh);
            }
        }
        let args = TexArgs { w: self.w, h: self.h, texels: texels.as_ptr() as u64, levels: n };
        let h = gfx3d::upload_texture(self.handle, &args);
        if h != 0 {
            self.handle = h;
        }
        self.dirty = false;
        self.handle
    }
}

/// How many levels a `w` x `h` texture has with `levels` asked for (0:
/// all, down to 1x1).
fn level_count(w: u32, h: u32, levels: u32) -> u32 {
    let full = 32 - w.max(h).max(1).leading_zeros();
    if levels == 0 { full } else { levels.min(full) }
}

unsafe fn make_texture(dev: *mut Com, cube: bool, w: u32, h: u32, levels: u32, usage: u32, format: u32, pool: u32, out: *mut *mut Com) -> i32 {
    if out.is_null() || w == 0 || h == 0 || w > 8192 || h > 8192 {
        return D3DERR_INVALIDCALL;
    }
    let target = usage & USAGE_RENDERTARGET != 0;
    let ok = if usage & USAGE_DEPTHSTENCIL != 0 { false } else if target { TARGET_FORMATS.contains(&format) } else { TEXTURE_FORMATS.contains(&format) };
    if !ok || (target && pool != POOL_DEFAULT) {
        return D3DERR_INVALIDCALL;
    }
    // Automatic mipmaps: one level to the program, the rest made here.
    let n = if usage & USAGE_AUTOGENMIPMAP != 0 { 1 } else { level_count(w, h, levels) };
    let mut surfaces = Vec::new();
    for face in 0..if cube { 6 } else { 1 } {
        for l in 0..n {
            // Only level 0 of a render target texture is drawn into.
            let su = if l == 0 { usage } else { usage & !USAGE_RENDERTARGET };
            let Some(mut s) = Surface::new(format, (w >> l).max(1), (h >> l).max(1), su, pool) else {
                for s in surfaces {
                    unsafe { free(s) };
                }
                return D3DERR_OUTOFVIDEOMEMORY;
            };
            (s.level, s.face) = (l, face);
            surfaces.push(new_surface(dev, s));
        }
    }
    let t = Texture { cube, format, w, h, levels: n, usage, pool, surfaces, handle: 0, dirty: true, lod: 0, autogen_filter: 2 };
    let surfaces = t.surfaces.clone();
    let obj = if cube {
        Com::new(&vtbl::IDIRECT3DCUBETEXTURE9, &CUBE_IIDS, dev, Body::Texture(t))
    } else {
        Com::new(&vtbl::IDIRECT3DTEXTURE9, &TEXTURE_IIDS, dev, Body::Texture(t))
    };
    for s in surfaces {
        unsafe { (*s).container = obj };
    }
    unsafe { *out = obj };
    S_OK
}

pub unsafe extern "system" fn Dev_CreateTexture(this: *mut Com, w: u32, h: u32, levels: u32, usage: u32, format: u32, pool: u32, out: *mut *mut Com, _shared: usize) -> i32 {
    unsafe { make_texture(this, false, w, h, levels, usage, format, pool, out) }
}

pub unsafe extern "system" fn Dev_CreateCubeTexture(this: *mut Com, size: u32, levels: u32, usage: u32, format: u32, pool: u32, out: *mut *mut Com, _shared: usize) -> i32 {
    unsafe { make_texture(this, true, size, size, levels, usage, format, pool, out) }
}

pub unsafe extern "system" fn Base_SetLOD(this: *mut Com, lod: u32) -> u32 {
    let t = unsafe { texture(this) };
    let old = t.lod;
    if t.pool == POOL_MANAGED {
        t.lod = lod.min(t.levels - 1);
    }
    old
}

pub unsafe extern "system" fn Base_GetLOD(this: *mut Com) -> u32 {
    unsafe { texture(this) }.lod
}

pub unsafe extern "system" fn Base_GetLevelCount(this: *mut Com) -> u32 {
    unsafe { texture(this) }.levels
}

pub unsafe extern "system" fn Base_SetAutoGenFilterType(this: *mut Com, f: u32) -> i32 {
    unsafe { texture(this) }.autogen_filter = f;
    S_OK
}

pub unsafe extern "system" fn Base_GetAutoGenFilterType(this: *mut Com) -> u32 {
    unsafe { texture(this) }.autogen_filter
}

pub unsafe extern "system" fn Base_GenerateMipSubLevels(this: *mut Com) {
    unsafe { texture(this) }.dirty = true;
}

unsafe fn level_desc(this: *mut Com, face: u32, level: u32, out: *mut u8) -> i32 {
    match unsafe { texture(this) }.surface(face, level) {
        Some(s) => unsafe { Surf_GetDesc(s, out) },
        None => D3DERR_INVALIDCALL,
    }
}

unsafe fn level_surface(this: *mut Com, face: u32, level: u32, out: *mut *mut Com) -> i32 {
    match unsafe { texture(this) }.surface(face, level) {
        Some(s) if !out.is_null() => unsafe { out_ref(out, s) },
        _ => D3DERR_INVALIDCALL,
    }
}

pub unsafe extern "system" fn Tex_GetLevelDesc(this: *mut Com, level: u32, out: *mut u8) -> i32 {
    unsafe { level_desc(this, 0, level, out) }
}

pub unsafe extern "system" fn Tex_GetSurfaceLevel(this: *mut Com, level: u32, out: *mut *mut Com) -> i32 {
    unsafe { level_surface(this, 0, level, out) }
}

pub unsafe extern "system" fn Tex_LockRect(this: *mut Com, level: u32, out: *mut u8, rect: *const [i32; 4], flags: u32) -> i32 {
    match unsafe { texture(this) }.surface(0, level) {
        Some(s) => unsafe { Surf_LockRect(s, out, rect, flags) },
        None => D3DERR_INVALIDCALL,
    }
}

pub unsafe extern "system" fn Tex_UnlockRect(this: *mut Com, level: u32) -> i32 {
    match unsafe { texture(this) }.surface(0, level) {
        Some(s) => unsafe { Surf_UnlockRect(s) },
        None => D3DERR_INVALIDCALL,
    }
}

pub unsafe extern "system" fn Tex_AddDirtyRect(this: *mut Com, _r: *const [i32; 4]) -> i32 {
    unsafe { texture(this) }.dirty = true;
    S_OK
}

pub unsafe extern "system" fn Cube_GetLevelDesc(this: *mut Com, level: u32, out: *mut u8) -> i32 {
    unsafe { level_desc(this, 0, level, out) }
}

pub unsafe extern "system" fn Cube_GetCubeMapSurface(this: *mut Com, face: u32, level: u32, out: *mut *mut Com) -> i32 {
    if face > 5 {
        return D3DERR_INVALIDCALL;
    }
    unsafe { level_surface(this, face, level, out) }
}

pub unsafe extern "system" fn Cube_LockRect(this: *mut Com, face: u32, level: u32, out: *mut u8, rect: *const [i32; 4], flags: u32) -> i32 {
    match unsafe { texture(this) }.surface(face.min(5), level) {
        Some(s) => unsafe { Surf_LockRect(s, out, rect, flags) },
        None => D3DERR_INVALIDCALL,
    }
}

pub unsafe extern "system" fn Cube_UnlockRect(this: *mut Com, face: u32, level: u32) -> i32 {
    match unsafe { texture(this) }.surface(face.min(5), level) {
        Some(s) => unsafe { Surf_UnlockRect(s) },
        None => D3DERR_INVALIDCALL,
    }
}

pub unsafe extern "system" fn Cube_AddDirtyRect(this: *mut Com, _face: u32, _r: *const [i32; 4]) -> i32 {
    unsafe { texture(this) }.dirty = true;
    S_OK
}

// -----| vertex and index buffers |-----

pub struct Buffer {
    pub index: bool,
    pub data: Vec<u8>,
    pub usage: u32,
    pub pool: u32,
    /// A vertex buffer's FVF; an index buffer's format (INDEX16, INDEX32).
    pub format: u32,
    pub locks: u32,
}

#[repr(C)]
struct BufferDesc {
    format: u32,
    rtype: u32,
    usage: u32,
    pool: u32,
    size: u32,
    fvf: u32,
}

unsafe fn make_buffer(dev: *mut Com, index: bool, len: u32, usage: u32, format: u32, pool: u32, out: *mut *mut Com) -> i32 {
    if out.is_null() || len == 0 || (index && format != INDEX16 && format != INDEX32) {
        return D3DERR_INVALIDCALL;
    }
    let b = Buffer { index, data: vec![0u8; len as usize], usage, pool, format, locks: 0 };
    let o = if index {
        Com::new(&vtbl::IDIRECT3DINDEXBUFFER9, &IB_IIDS, dev, Body::Buffer(b))
    } else {
        Com::new(&vtbl::IDIRECT3DVERTEXBUFFER9, &VB_IIDS, dev, Body::Buffer(b))
    };
    unsafe { *out = o };
    S_OK
}

pub unsafe extern "system" fn Dev_CreateVertexBuffer(this: *mut Com, len: u32, usage: u32, fvf: u32, pool: u32, out: *mut *mut Com, _shared: usize) -> i32 {
    unsafe { make_buffer(this, false, len, usage, fvf, pool, out) }
}

pub unsafe extern "system" fn Dev_CreateIndexBuffer(this: *mut Com, len: u32, usage: u32, format: u32, pool: u32, out: *mut *mut Com, _shared: usize) -> i32 {
    unsafe { make_buffer(this, true, len, usage, format, pool, out) }
}

/// Vertices are read when drawn, so a lock can hand out the memory itself:
/// a draw already made has what it needs.
pub unsafe extern "system" fn VB_Lock(this: *mut Com, offset: u32, size: u32, out: *mut *mut u8, _flags: u32) -> i32 {
    let b = unsafe { buffer(this) };
    if out.is_null() || offset as usize > b.data.len() || (size != 0 && (offset + size) as usize > b.data.len()) {
        return D3DERR_INVALIDCALL;
    }
    b.locks += 1;
    unsafe { *out = b.data.as_mut_ptr().add(offset as usize) };
    S_OK
}

pub unsafe extern "system" fn VB_Unlock(this: *mut Com) -> i32 {
    let b = unsafe { buffer(this) };
    b.locks = b.locks.saturating_sub(1);
    S_OK
}

pub unsafe extern "system" fn VB_GetDesc(this: *mut Com, out: *mut u8) -> i32 {
    let b = unsafe { buffer(this) };
    if out.is_null() {
        return D3DERR_INVALIDCALL;
    }
    let d = BufferDesc { format: VERTEXDATA, rtype: 6, usage: b.usage, pool: b.pool, size: b.data.len() as u32, fvf: b.format };
    unsafe { core::ptr::write_unaligned(out as *mut BufferDesc, d) };
    S_OK
}

pub unsafe extern "system" fn IB_Lock(this: *mut Com, offset: u32, size: u32, out: *mut *mut u8, flags: u32) -> i32 {
    unsafe { VB_Lock(this, offset, size, out, flags) }
}

pub unsafe extern "system" fn IB_Unlock(this: *mut Com) -> i32 {
    unsafe { VB_Unlock(this) }
}

pub unsafe extern "system" fn IB_GetDesc(this: *mut Com, out: *mut u8) -> i32 {
    let b = unsafe { buffer(this) };
    if out.is_null() {
        return D3DERR_INVALIDCALL;
    }
    // D3DINDEXBUFFER_DESC: the vertex buffer's without the FVF.
    let d = [b.format, 7, b.usage, b.pool, b.data.len() as u32];
    unsafe { core::ptr::write_unaligned(out as *mut [u32; 5], d) };
    S_OK
}

// -----| vertex declarations |-----

/// D3DVERTEXELEMENT9.
#[repr(C)]
#[derive(Clone, Copy, PartialEq)]
pub struct Element {
    pub stream: u16,
    pub offset: u16,
    pub etype: u8,
    pub method: u8,
    pub usage: u8,
    pub index: u8,
}

pub const DECL_END: Element = Element { stream: 0xFF, offset: 0, etype: 17, method: 0, usage: 0, index: 0 };

pub struct Decl {
    /// Without the end marker.
    pub elems: Vec<Element>,
    /// The FVF it was made for (SetFVF), or 0.
    pub fvf: u32,
}

pub fn new_decl(dev: *mut Com, elems: Vec<Element>, fvf: u32) -> *mut Com {
    Com::new(&vtbl::IDIRECT3DVERTEXDECLARATION9, &DECL_IIDS, dev, Body::Decl(Decl { elems, fvf }))
}

pub unsafe extern "system" fn Dev_CreateVertexDeclaration(this: *mut Com, elems: *const Element, out: *mut *mut Com) -> i32 {
    if elems.is_null() || out.is_null() {
        return D3DERR_INVALIDCALL;
    }
    let mut v = Vec::new();
    for i in 0..64 {
        let e = unsafe { core::ptr::read_unaligned(elems.add(i)) };
        if e.stream == 0xFF {
            break;
        }
        v.push(e);
    }
    unsafe { *out = new_decl(this, v, 0) };
    S_OK
}

pub unsafe extern "system" fn Decl_GetDeclaration(this: *mut Com, out: *mut Element, count: *mut u32) -> i32 {
    let Body::Decl(d) = (unsafe { &(*this).body }) else { return D3DERR_INVALIDCALL };
    if count.is_null() {
        return D3DERR_INVALIDCALL;
    }
    unsafe { *count = d.elems.len() as u32 + 1 };
    if !out.is_null() {
        for (i, e) in d.elems.iter().chain(core::iter::once(&DECL_END)).enumerate() {
            unsafe { core::ptr::write_unaligned(out.add(i), *e) };
        }
    }
    S_OK
}

// -----| surfaces made by the device, and copies |-----

unsafe fn make_surface(dev: *mut Com, w: u32, h: u32, format: u32, usage: u32, pool: u32, out: *mut *mut Com) -> i32 {
    if out.is_null() || w == 0 || h == 0 || w > 8192 || h > 8192 {
        return D3DERR_INVALIDCALL;
    }
    let ok = if usage & USAGE_DEPTHSTENCIL != 0 {
        DEPTH_FORMATS.contains(&format)
    } else if usage & USAGE_RENDERTARGET != 0 {
        TARGET_FORMATS.contains(&format)
    } else {
        TEXTURE_FORMATS.contains(&format) || TARGET_FORMATS.contains(&format) || format == R8G8B8
    };
    if !ok {
        return D3DERR_INVALIDCALL;
    }
    match Surface::new(format, w, h, usage, pool) {
        Some(s) => {
            unsafe { *out = new_surface(dev, s) };
            S_OK
        }
        None => D3DERR_OUTOFVIDEOMEMORY,
    }
}

pub unsafe extern "system" fn Dev_CreateRenderTarget(this: *mut Com, w: u32, h: u32, format: u32, _ms: u32, _q: u32, _lockable: i32, out: *mut *mut Com, _shared: usize) -> i32 {
    unsafe { make_surface(this, w, h, format, USAGE_RENDERTARGET, POOL_DEFAULT, out) }
}

pub unsafe extern "system" fn Dev_CreateDepthStencilSurface(this: *mut Com, w: u32, h: u32, format: u32, _ms: u32, _q: u32, _discard: i32, out: *mut *mut Com, _shared: usize) -> i32 {
    unsafe { make_surface(this, w, h, format, USAGE_DEPTHSTENCIL, POOL_DEFAULT, out) }
}

pub unsafe extern "system" fn Dev_CreateOffscreenPlainSurface(this: *mut Com, w: u32, h: u32, format: u32, pool: u32, out: *mut *mut Com, _shared: usize) -> i32 {
    unsafe { make_surface(this, w, h, format, 0, pool, out) }
}

/// The rectangle `r` (x0, y0, x1, y1) or the whole `w` x `h`.
fn rect_or(r: *const [i32; 4], w: u32, h: u32) -> [i32; 4] {
    if r.is_null() { [0, 0, w as i32, h as i32] } else { unsafe { *r } }
}

/// Copy `sr` of `src` onto `dr` of `dst`, scaled (nearest or, `linear`,
/// bilinear), in 0xAARRGGBB.
fn blit(src: &Surface, sr: [i32; 4], dst: &mut Surface, dr: [i32; 4], linear: bool) {
    let sp = src.read_argb();
    let mut dp = if dst.is_target() { vec![0u32; (dst.w * dst.h) as usize] } else { dst.read_argb() };
    let (sw, sh) = ((sr[2] - sr[0]).max(1), (sr[3] - sr[1]).max(1));
    let (dw, dh) = ((dr[2] - dr[0]).max(1), (dr[3] - dr[1]).max(1));
    let at = |x: i32, y: i32| sp[(y.clamp(0, src.h as i32 - 1) as u32 * src.w + x.clamp(0, src.w as i32 - 1) as u32) as usize];
    for y in dr[1].max(0)..dr[3].min(dst.h as i32) {
        for x in dr[0].max(0)..dr[2].min(dst.w as i32) {
            // The source position of this pixel's centre, 8 fraction bits.
            let fx = sr[0] * 256 + ((2 * (x - dr[0]) + 1) * sw * 256) / (2 * dw) - 128;
            let fy = sr[1] * 256 + ((2 * (y - dr[1]) + 1) * sh * 256) / (2 * dh) - 128;
            let c = if linear && (sw != dw || sh != dh) {
                let (x0, y0, ax, ay) = (fx >> 8, fy >> 8, (fx & 255) as u32, (fy & 255) as u32);
                let q = [at(x0, y0), at(x0 + 1, y0), at(x0, y0 + 1), at(x0 + 1, y0 + 1)];
                let mut c = 0;
                for sh in [0, 8, 16, 24] {
                    let ch = |p: u32| p >> sh & 0xFF;
                    let top = ch(q[0]) * (256 - ax) + ch(q[1]) * ax;
                    let bot = ch(q[2]) * (256 - ax) + ch(q[3]) * ax;
                    c |= ((top * (256 - ay) + bot * ay + 32768) >> 16) << sh;
                }
                c
            } else {
                at((fx + 128) >> 8, (fy + 128) >> 8)
            };
            dp[(y as u32 * dst.w + x as u32) as usize] = c;
        }
    }
    dst.write_argb(&dp, dr);
}

pub unsafe extern "system" fn Dev_StretchRect(this: *mut Com, src: *mut Com, sr: *const [i32; 4], dst: *mut Com, dr: *const [i32; 4], filter: u32) -> i32 {
    if !is_surface(src) || !is_surface(dst) || src == dst {
        return D3DERR_INVALIDCALL;
    }
    unsafe { device(this) }.flush();
    let (s, d) = unsafe { (surface(src), surface(dst)) };
    if s.is_depth() || d.is_depth() || is_dxt(d.format) {
        return D3DERR_INVALIDCALL;
    }
    let (r1, r2) = (rect_or(sr, s.w, s.h), rect_or(dr, d.w, d.h));
    blit(s, r1, d, r2, filter == 2);
    touch_container(dst);
    S_OK
}

pub unsafe extern "system" fn Dev_UpdateSurface(this: *mut Com, src: *mut Com, sr: *const [i32; 4], dst: *mut Com, dp: *const [i32; 2]) -> i32 {
    if !is_surface(src) || !is_surface(dst) {
        return D3DERR_INVALIDCALL;
    }
    unsafe { device(this) }.flush();
    let (s, d) = unsafe { (surface(src), surface(dst)) };
    if s.format != d.format {
        return D3DERR_INVALIDCALL;
    }
    let r = rect_or(sr, s.w, s.h);
    let p = if dp.is_null() { [r[0], r[1]] } else { unsafe { *dp } };
    let dr = [p[0], p[1], p[0] + r[2] - r[0], p[1] + r[3] - r[1]];
    if is_dxt(s.format) || (!d.is_target() && r == [0, 0, s.w as i32, s.h as i32] && dr == r && s.data.len() == d.data.len()) {
        // Same format, whole surface (or compressed: whole blocks): bytes.
        if s.data.len() == d.data.len() && !d.is_target() {
            d.data.copy_from_slice(&s.data);
        }
    } else {
        blit(s, r, d, dr, false);
    }
    touch_container(dst);
    S_OK
}

pub unsafe extern "system" fn Dev_UpdateTexture(this: *mut Com, src: *mut Com, dst: *mut Com) -> i32 {
    if !is_texture(src) || !is_texture(dst) {
        return D3DERR_INVALIDCALL;
    }
    let (s, d) = unsafe { (texture(src), texture(dst)) };
    if s.cube != d.cube || s.format != d.format {
        return D3DERR_INVALIDCALL;
    }
    // Level by level, matched by size from the top.
    let skip = (0..s.levels).find(|&l| (s.w >> l).max(1) == d.w && (s.h >> l).max(1) == d.h).unwrap_or(0);
    for face in 0..if s.cube { 6 } else { 1 } {
        for l in 0..d.levels {
            if let (Some(a), Some(b)) = (s.surface(face, l + skip), d.surface(face, l)) {
                unsafe { Dev_UpdateSurface(this, a, core::ptr::null(), b, core::ptr::null()) };
            }
        }
    }
    d.dirty = true;
    S_OK
}

pub unsafe extern "system" fn Dev_GetRenderTargetData(this: *mut Com, src: *mut Com, dst: *mut Com) -> i32 {
    if !is_surface(src) || !is_surface(dst) {
        return D3DERR_INVALIDCALL;
    }
    unsafe { device(this) }.flush();
    let (s, d) = unsafe { (surface(src), surface(dst)) };
    if s.w != d.w || s.h != d.h || d.is_target() {
        return D3DERR_INVALIDCALL;
    }
    let px = s.read_argb();
    encode(d.format, d.w, d.h, &px, &mut d.data, d.pitch);
    S_OK
}

pub unsafe extern "system" fn Dev_ColorFill(this: *mut Com, dst: *mut Com, r: *const [i32; 4], color: u32) -> i32 {
    if !is_surface(dst) {
        return D3DERR_INVALIDCALL;
    }
    unsafe { device(this) }.flush();
    let d = unsafe { surface(dst) };
    if d.is_depth() || is_dxt(d.format) {
        return D3DERR_INVALIDCALL;
    }
    let r = rect_or(r, d.w, d.h);
    let px = vec![color; (d.w * d.h) as usize];
    d.write_argb(&px, r);
    touch_container(dst);
    S_OK
}
