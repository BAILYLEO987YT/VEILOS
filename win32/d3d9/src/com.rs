//! COM objects: every interface d3d9.dll hands out is a `Com`, a vtable
//! (src/vtbl.rs, generated) over a reference count and a `Body`.
//!
//! References: an object freed at 0. A texture's level surfaces and the
//! device's own surfaces (back buffer, automatic depth buffer) have a
//! container, and count on it instead, as in Direct3D. The device holds a
//! reference on what is bound to it (textures, buffers, surfaces,
//! declarations); resources don't hold one on the device, so neither keeps
//! the other alive.

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, Ordering};

use crate::dllrt::Lock;

pub const S_OK: i32 = 0;
pub const E_NOINTERFACE: i32 = 0x8000_4002u32 as i32;
pub const E_POINTER: i32 = 0x8000_4003u32 as i32;
pub const D3DERR_INVALIDCALL: i32 = 0x8876_086Cu32 as i32;
pub const D3DERR_NOTAVAILABLE: i32 = 0x8876_086Au32 as i32;
pub const D3DERR_NOTFOUND: i32 = 0x8876_0866u32 as i32;
pub const D3DERR_MOREDATA: i32 = 0x8876_0867u32 as i32;
pub const D3DERR_OUTOFVIDEOMEMORY: i32 = 0x8876_017Cu32 as i32;

#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Guid(pub u32, pub u16, pub u16, pub [u8; 8]);

pub const IID_IUNKNOWN: Guid = Guid(0, 0, 0, [0xC0, 0, 0, 0, 0, 0, 0, 0x46]);
pub const IID_D3D9: Guid = Guid(0x81bdcbca, 0x64d4, 0x426d, [0xae, 0x8d, 0xad, 0x01, 0x47, 0xf4, 0x27, 0x5c]);
pub const IID_DEVICE9: Guid = Guid(0xd0223b96, 0xbf7a, 0x43fd, [0x92, 0xbd, 0xa4, 0x3b, 0x0d, 0x82, 0xb9, 0xeb]);
pub const IID_RESOURCE9: Guid = Guid(0x05eec05d, 0x8f7d, 0x4362, [0xb9, 0x99, 0xd1, 0xba, 0xf3, 0x57, 0xc7, 0x04]);
pub const IID_BASETEXTURE9: Guid = Guid(0x580ca87e, 0x1d3c, 0x4d54, [0x99, 0x1d, 0xb7, 0xd3, 0xe3, 0xc2, 0x98, 0xce]);
pub const IID_TEXTURE9: Guid = Guid(0x85c31227, 0x3de5, 0x4f00, [0x9b, 0x3a, 0xf1, 0x1a, 0xc3, 0x8c, 0x18, 0xb5]);
pub const IID_CUBETEXTURE9: Guid = Guid(0xfff32f81, 0xd953, 0x473a, [0x92, 0x23, 0x93, 0xd6, 0x52, 0xab, 0xa9, 0x3f]);
pub const IID_VERTEXBUFFER9: Guid = Guid(0xb64bb1b5, 0xfd70, 0x4df6, [0xbf, 0x91, 0x19, 0xd0, 0xa1, 0x24, 0x55, 0xe3]);
pub const IID_INDEXBUFFER9: Guid = Guid(0x7c9dd65e, 0xd3f7, 0x4529, [0xac, 0xee, 0x78, 0x58, 0x30, 0xac, 0xde, 0x35]);
pub const IID_SURFACE9: Guid = Guid(0x0cfbaf3a, 0x9ff6, 0x429a, [0x99, 0xb3, 0xa2, 0x79, 0x6a, 0xf8, 0xb8, 0x9b]);
pub const IID_SWAPCHAIN9: Guid = Guid(0x794950f2, 0xadfc, 0x458a, [0x90, 0x5e, 0x10, 0xa1, 0x0b, 0x0b, 0x50, 0x3b]);
pub const IID_DECL9: Guid = Guid(0xdd13c59c, 0x36fa, 0x4098, [0xa8, 0xfb, 0xc7, 0xed, 0x39, 0xdc, 0x85, 0x46]);
pub const IID_STATEBLOCK9: Guid = Guid(0xb07c4fe5, 0x310d, 0x4ba8, [0xa2, 0x3c, 0x4f, 0x0f, 0x20, 0x6f, 0x21, 0x8b]);
pub const IID_QUERY9: Guid = Guid(0xd9771460, 0xa695, 0x4f26, [0xbb, 0xd3, 0x27, 0xb8, 0x40, 0xb5, 0x41, 0xcc]);

/// A vtable: function pointers in the interface's order.
#[repr(C)]
pub struct Vtbl<const N: usize>(pub [*const (); N]);
unsafe impl<const N: usize> Sync for Vtbl<N> {}

/// What an object is.
pub enum Body {
    D3D,
    Device(Box<crate::device::Device>),
    Surface(crate::res::Surface),
    Texture(crate::res::Texture),
    Buffer(crate::res::Buffer),
    Decl(crate::res::Decl),
    StateBlock(Box<crate::state::StateBlock>),
    SwapChain,
    Query(u32),
    Shader(Box<crate::shader::Shader>),
}

#[repr(C)]
pub struct Com {
    vtbl: *const (),
    refs: AtomicU32,
    pub iids: &'static [Guid],
    /// Counted references go here instead (texture levels, the device's
    /// own surfaces).
    pub container: *mut Com,
    /// The device that made it (null for IDirect3D9 and the device).
    pub device: *mut Com,
    /// SetPrivateData's: each GUID's bytes, or a COM object it holds.
    pub private: Vec<(Guid, Vec<u8>, *mut Com)>,
    pub priority: u32,
    pub body: Body,
}

impl Com {
    pub fn new<const N: usize>(vtbl: &'static Vtbl<N>, iids: &'static [Guid], device: *mut Com, body: Body) -> *mut Com {
        Box::into_raw(Box::new(Com {
            vtbl: vtbl as *const Vtbl<N> as *const (),
            refs: AtomicU32::new(1),
            iids,
            container: core::ptr::null_mut(),
            device,
            private: Vec::new(),
            priority: 0,
            body,
        }))
    }
}

pub unsafe fn add_ref(this: *mut Com) -> u32 {
    if this.is_null() {
        return 0;
    }
    let c = unsafe { &*this };
    if !c.container.is_null() {
        return unsafe { add_ref(c.container) };
    }
    c.refs.fetch_add(1, Ordering::AcqRel) + 1
}

pub unsafe fn release(this: *mut Com) -> u32 {
    if this.is_null() {
        return 0;
    }
    let c = unsafe { &*this };
    if !c.container.is_null() {
        return unsafe { release(c.container) };
    }
    let n = c.refs.fetch_sub(1, Ordering::AcqRel) - 1;
    if n == 0 {
        unsafe { free(this) };
    }
    n
}

/// Free an object now, whatever its count (its own drop frees what it owns).
pub unsafe fn free(this: *mut Com) {
    let mut b = unsafe { Box::from_raw(this) };
    for (_, _, o) in b.private.drain(..) {
        unsafe { release(o) };
    }
    drop(b);
}

/// Swap a counted reference: `slot` takes `new` (AddRef'd), the old one is
/// released.
pub unsafe fn set_ref(slot: &mut *mut Com, new: *mut Com) {
    if *slot == new {
        return;
    }
    unsafe {
        add_ref(new);
        release(*slot);
    }
    *slot = new;
}

/// Hand out `this` through `out` with a reference.
pub unsafe fn out_ref<T>(out: *mut *mut T, this: *mut Com) -> i32 {
    if out.is_null() {
        return D3DERR_INVALIDCALL;
    }
    unsafe {
        add_ref(this);
        *out = this as *mut T;
    }
    if this.is_null() { D3DERR_NOTFOUND } else { S_OK }
}

pub unsafe extern "system" fn Unk_QueryInterface(this: *mut Com, iid: *const Guid, out: *mut *mut Com) -> i32 {
    if out.is_null() {
        return E_POINTER;
    }
    let iid = unsafe { *iid };
    let c = unsafe { &*this };
    if iid == IID_IUNKNOWN || c.iids.contains(&iid) {
        unsafe {
            add_ref(this);
            *out = this;
        }
        return S_OK;
    }
    unsafe { *out = core::ptr::null_mut() };
    E_NOINTERFACE
}

pub unsafe extern "system" fn Unk_AddRef(this: *mut Com) -> u32 {
    unsafe { add_ref(this) }
}

pub unsafe extern "system" fn Unk_Release(this: *mut Com) -> u32 {
    unsafe { release(this) }
}

// -----| IDirect3DResource9's methods, which every object has some of |-----

pub unsafe extern "system" fn Res_GetDevice(this: *mut Com, out: *mut *mut Com) -> i32 {
    unsafe { out_ref(out, (*this).device) }
}

pub unsafe extern "system" fn Res_SetPrivateData(this: *mut Com, guid: *const Guid, data: *const u8, size: u32, flags: u32) -> i32 {
    if guid.is_null() || (data.is_null() && size != 0) {
        return D3DERR_INVALIDCALL;
    }
    unsafe { Res_FreePrivateData(this, guid) };
    let c = unsafe { &mut *this };
    // D3DSPD_IUNKNOWN: the data is an object, held by reference.
    let (bytes, obj) = if flags & 1 != 0 {
        if size as usize != core::mem::size_of::<usize>() {
            return D3DERR_INVALIDCALL;
        }
        let o = unsafe { *(data as *const *mut Com) };
        unsafe { add_ref(o) };
        (unsafe { core::slice::from_raw_parts(data, size as usize) }.to_vec(), o)
    } else {
        (unsafe { core::slice::from_raw_parts(data, size as usize) }.to_vec(), core::ptr::null_mut())
    };
    c.private.push((unsafe { *guid }, bytes, obj));
    S_OK
}

pub unsafe extern "system" fn Res_GetPrivateData(this: *mut Com, guid: *const Guid, data: *mut u8, size: *mut u32) -> i32 {
    if guid.is_null() || size.is_null() {
        return D3DERR_INVALIDCALL;
    }
    let c = unsafe { &*this };
    let Some((_, bytes, obj)) = c.private.iter().find(|e| e.0 == unsafe { *guid }) else { return D3DERR_NOTFOUND };
    let have = unsafe { *size } as usize;
    unsafe { *size = bytes.len() as u32 };
    if data.is_null() {
        return S_OK;
    }
    if have < bytes.len() {
        return D3DERR_MOREDATA;
    }
    unsafe {
        core::ptr::copy_nonoverlapping(bytes.as_ptr(), data, bytes.len());
        add_ref(*obj);
    }
    S_OK
}

pub unsafe extern "system" fn Res_FreePrivateData(this: *mut Com, guid: *const Guid) -> i32 {
    let c = unsafe { &mut *this };
    let g = unsafe { *guid };
    let Some(i) = c.private.iter().position(|e| e.0 == g) else { return D3DERR_NOTFOUND };
    let (_, _, o) = c.private.remove(i);
    unsafe { release(o) };
    S_OK
}

pub unsafe extern "system" fn Res_SetPriority(this: *mut Com, p: u32) -> u32 {
    let c = unsafe { &mut *this };
    core::mem::replace(&mut c.priority, p)
}

pub unsafe extern "system" fn Res_GetPriority(this: *mut Com) -> u32 {
    unsafe { (*this).priority }
}

pub unsafe extern "system" fn Res_PreLoad(_this: *mut Com) {}

/// D3DRESOURCETYPE.
pub unsafe extern "system" fn Res_GetType(this: *mut Com) -> u32 {
    match unsafe { &(*this).body } {
        Body::Surface(_) => 1,
        Body::Texture(t) if t.cube => 5,
        Body::Texture(_) => 3,
        Body::Buffer(b) if b.index => 7,
        Body::Buffer(_) => 6,
        _ => 0,
    }
}

// -----| downcasts |-----

pub unsafe fn surface<'a>(this: *mut Com) -> &'a mut crate::res::Surface {
    match unsafe { &mut (*this).body } {
        Body::Surface(s) => s,
        _ => panic!(),
    }
}

pub unsafe fn texture<'a>(this: *mut Com) -> &'a mut crate::res::Texture {
    match unsafe { &mut (*this).body } {
        Body::Texture(t) => t,
        _ => panic!(),
    }
}

pub unsafe fn buffer<'a>(this: *mut Com) -> &'a mut crate::res::Buffer {
    match unsafe { &mut (*this).body } {
        Body::Buffer(b) => b,
        _ => panic!(),
    }
}

pub unsafe fn device<'a>(this: *mut Com) -> &'a mut crate::device::Device {
    match unsafe { &mut (*this).body } {
        Body::Device(d) => d,
        _ => panic!(),
    }
}

pub fn is_surface(this: *mut Com) -> bool {
    !this.is_null() && matches!(unsafe { &(*this).body }, Body::Surface(_))
}

pub fn is_texture(this: *mut Com) -> bool {
    !this.is_null() && matches!(unsafe { &(*this).body }, Body::Texture(_))
}

#[link(name = "kernel32", kind = "raw-dylib")]
unsafe extern "system" {
    fn OutputDebugStringA(s: *const u8);
}

static SEEN: Lock<Vec<usize>> = Lock::new(Vec::new());

/// A method not implemented was called: note it once (`name` ends in a 0).
pub fn unimplemented(name: &'static [u8]) {
    let mut s = SEEN.lock();
    if s.contains(&(name.as_ptr() as usize)) {
        return;
    }
    s.push(name.as_ptr() as usize);
    unsafe {
        OutputDebugStringA(b"d3d9: not implemented: \0".as_ptr());
        OutputDebugStringA(name.as_ptr());
        OutputDebugStringA(b"\n\0".as_ptr());
    }
}
