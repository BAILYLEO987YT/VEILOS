//! COM objects: every interface d3d11.dll (and through it dxgi.dll) hands
//! out is a `Com`: a vtable (src/vtbl.rs, generated) over a reference count
//! and a `Body`.
//!
//! An object that also answers to an interface from the other family (a
//! device is an IDXGIDevice1, a texture an IDXGISurface1 and IDXGIResource)
//! gets an alias for it on first QueryInterface: another `Com` with that
//! interface's vtable, counting on the main one, freed with it.
//!
//! References: the device holds what's bound to its context; resources,
//! views and states hold their device (as Direct3D 11 does: a device lives
//! while anything made from it does), and views their resource.

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, Ordering};

use crate::dllrt::Lock;

pub const S_OK: i32 = 0;
pub const S_FALSE: i32 = 1;
pub const E_NOTIMPL: i32 = 0x8000_4001u32 as i32;
pub const E_NOINTERFACE: i32 = 0x8000_4002u32 as i32;
pub const E_POINTER: i32 = 0x8000_4003u32 as i32;
pub const E_FAIL: i32 = 0x8000_4005u32 as i32;
pub const E_OUTOFMEMORY: i32 = 0x8007_000Eu32 as i32;
pub const E_INVALIDARG: i32 = 0x8007_0057u32 as i32;
pub const DXGI_ERROR_INVALID_CALL: i32 = 0x887A_0001u32 as i32;
pub const DXGI_ERROR_NOT_FOUND: i32 = 0x887A_0002u32 as i32;
pub const DXGI_ERROR_MORE_DATA: i32 = 0x887A_0003u32 as i32;
pub const DXGI_ERROR_UNSUPPORTED: i32 = 0x887A_0004u32 as i32;
pub const DXGI_ERROR_WAS_STILL_DRAWING: i32 = 0x887A_000Au32 as i32;
pub const DXGI_ERROR_NOT_CURRENTLY_AVAILABLE: i32 = 0x887A_0022u32 as i32;

#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Guid(pub u32, pub u16, pub u16, pub [u8; 8]);

pub const IID_IUNKNOWN: Guid = Guid(0, 0, 0, [0xC0, 0, 0, 0, 0, 0, 0, 0x46]);

/// A vtable: function pointers in the interface's order.
#[repr(C)]
pub struct Vtbl<const N: usize>(pub [*const (); N]);
unsafe impl<const N: usize> Sync for Vtbl<N> {}

/// What an object is.
pub enum Body {
    Factory,
    Adapter,
    Output,
    SwapChain(Box<crate::dxgi::SwapChain>),
    Device(Box<crate::device::Device>),
    Context,
    Resource(Box<crate::res::Resource>),
    View(Box<crate::res::View>),
    State(Box<crate::state::StateObj>),
    Shader(Box<crate::shader::ShaderObj>),
    InputLayout(Box<crate::state::InputLayout>),
    Query(u32),
    ClassLinkage,
    Deferred(Box<crate::deferred::Deferred>),
    CommandList(Box<alloc::vec::Vec<crate::deferred::Cmd>>),
    /// An alias of its `primary`.
    Alias,
}

#[repr(C)]
pub struct Com {
    vtbl: *const (),
    refs: AtomicU32,
    pub iids: &'static [Guid],
    /// An alias: the object it stands for (references count there).
    pub primary: *mut Com,
    /// GetParent / GetDevice: the factory, adapter or device (held).
    pub parent: *mut Com,
    /// SetPrivateData(Interface)'s: each GUID's bytes, or an object held.
    pub private: Vec<(Guid, Vec<u8>, *mut Com)>,
    pub aliases: Vec<*mut Com>,
    pub body: Body,
}

impl Com {
    /// A new object (one reference), holding `parent`.
    pub fn new<const N: usize>(vtbl: &'static Vtbl<N>, iids: &'static [Guid], parent: *mut Com, body: Body) -> *mut Com {
        unsafe { add_ref(parent) };
        Box::into_raw(Box::new(Com {
            vtbl: vtbl as *const Vtbl<N> as *const (),
            refs: AtomicU32::new(1),
            iids,
            primary: core::ptr::null_mut(),
            parent,
            private: Vec::new(),
            aliases: Vec::new(),
            body,
        }))
    }
}

/// The object an interface pointer stands for (itself, or its alias's).
pub fn main_of(p: *mut Com) -> *mut Com {
    if p.is_null() {
        return p;
    }
    let pr = unsafe { (*p).primary };
    if pr.is_null() { p } else { pr }
}

pub unsafe fn add_ref(this: *mut Com) -> u32 {
    let this = main_of(this);
    if this.is_null() {
        return 0;
    }
    unsafe { &*this }.refs.fetch_add(1, Ordering::AcqRel) + 1
}

pub unsafe fn release(this: *mut Com) -> u32 {
    let this = main_of(this);
    if this.is_null() {
        return 0;
    }
    let n = unsafe { &*this }.refs.fetch_sub(1, Ordering::AcqRel) - 1;
    if n == 0 {
        unsafe { free(this) };
    }
    n
}

unsafe fn free(this: *mut Com) {
    let mut b = unsafe { Box::from_raw(this) };
    for (_, _, o) in b.private.drain(..) {
        unsafe { release(o) };
    }
    for a in b.aliases.drain(..) {
        drop(unsafe { Box::from_raw(a) });
    }
    let parent = b.parent;
    drop(b);
    unsafe { release(parent) };
}

/// Swap a held reference: `slot` takes `new` (AddRef'd), the old one goes.
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

/// Hand out `this` through `out` with a reference (null stays null).
pub unsafe fn out_ref<T>(out: *mut *mut T, this: *mut Com) {
    if !out.is_null() {
        unsafe {
            add_ref(this);
            *out = this as *mut T;
        }
    }
}

/// QueryInterface on the object `this` stands for: itself, an alias it
/// has, or one made now (`crate::alias_for`).
pub unsafe fn query(this: *mut Com, iid: &Guid, out: *mut *mut Com) -> i32 {
    if out.is_null() {
        return E_POINTER;
    }
    let main = main_of(this);
    let c = unsafe { &mut *main };
    if *iid == IID_IUNKNOWN || c.iids.contains(iid) {
        unsafe { out_ref(out, main) };
        return S_OK;
    }
    for &a in &c.aliases {
        if unsafe { &*a }.iids.contains(iid) {
            unsafe { out_ref(out, a) };
            return S_OK;
        }
    }
    if let Some(a) = crate::alias_for(main, iid) {
        unsafe { (*a).primary = main };
        c.aliases.push(a);
        unsafe { out_ref(out, a) };
        return S_OK;
    }
    unsafe { *out = core::ptr::null_mut() };
    E_NOINTERFACE
}

/// An alias object (no parent, no body of its own).
pub fn alias<const N: usize>(vtbl: &'static Vtbl<N>, iids: &'static [Guid]) -> *mut Com {
    Box::into_raw(Box::new(Com {
        vtbl: vtbl as *const Vtbl<N> as *const (),
        refs: AtomicU32::new(0),
        iids,
        primary: core::ptr::null_mut(),
        parent: core::ptr::null_mut(),
        private: Vec::new(),
        aliases: Vec::new(),
        body: Body::Alias,
    }))
}

pub unsafe extern "system" fn Unk_QueryInterface(this: *mut Com, iid: *const Guid, out: *mut *mut Com) -> i32 {
    if iid.is_null() {
        return E_POINTER;
    }
    unsafe { query(this, &*iid, out) }
}

pub unsafe extern "system" fn Unk_AddRef(this: *mut Com) -> u32 {
    unsafe { add_ref(this) }
}

pub unsafe extern "system" fn Unk_Release(this: *mut Com) -> u32 {
    unsafe { release(this) }
}

// -----| private data (IDXGIObject and ID3D11DeviceChild alike) |-----

pub unsafe fn set_private(this: *mut Com, guid: *const Guid, size: u32, data: *const u8) -> i32 {
    if guid.is_null() {
        return E_INVALIDARG;
    }
    let c = unsafe { &mut *main_of(this) };
    let g = unsafe { *guid };
    if let Some(i) = c.private.iter().position(|e| e.0 == g) {
        let (_, _, o) = c.private.remove(i);
        unsafe { release(o) };
    }
    if size == 0 || data.is_null() {
        return S_OK;
    }
    c.private.push((g, unsafe { core::slice::from_raw_parts(data, size as usize) }.to_vec(), core::ptr::null_mut()));
    S_OK
}

pub unsafe fn set_private_interface(this: *mut Com, guid: *const Guid, obj: *mut Com) -> i32 {
    if guid.is_null() {
        return E_INVALIDARG;
    }
    let p = obj as usize;
    let r = unsafe { set_private(this, guid, 8, &p as *const usize as *const u8) };
    if r == S_OK && !obj.is_null() {
        unsafe { add_ref(obj) };
        let c = unsafe { &mut *main_of(this) };
        if let Some(e) = c.private.last_mut() {
            e.2 = obj;
        }
    }
    r
}

pub unsafe fn get_private(this: *mut Com, guid: *const Guid, size: *mut u32, data: *mut u8) -> i32 {
    if guid.is_null() || size.is_null() {
        return E_INVALIDARG;
    }
    let c = unsafe { &*main_of(this) };
    let Some((_, bytes, obj)) = c.private.iter().find(|e| e.0 == unsafe { *guid }) else {
        unsafe { *size = 0 };
        return DXGI_ERROR_NOT_FOUND;
    };
    let have = unsafe { *size } as usize;
    unsafe { *size = bytes.len() as u32 };
    if data.is_null() {
        return S_OK;
    }
    if have < bytes.len() {
        return DXGI_ERROR_MORE_DATA;
    }
    unsafe {
        core::ptr::copy_nonoverlapping(bytes.as_ptr(), data, bytes.len());
        add_ref(*obj);
    }
    S_OK
}

// IDXGIObject.
pub unsafe extern "system" fn DxObj_SetPrivateData(this: *mut Com, g: *const Guid, size: u32, data: *const u8) -> i32 {
    unsafe { set_private(this, g, size, data) }
}
pub unsafe extern "system" fn DxObj_SetPrivateDataInterface(this: *mut Com, g: *const Guid, o: *mut Com) -> i32 {
    unsafe { set_private_interface(this, g, o) }
}
pub unsafe extern "system" fn DxObj_GetPrivateData(this: *mut Com, g: *const Guid, size: *mut u32, data: *mut u8) -> i32 {
    unsafe { get_private(this, g, size, data) }
}
pub unsafe extern "system" fn DxObj_GetParent(this: *mut Com, iid: *const Guid, out: *mut *mut Com) -> i32 {
    let p = unsafe { (*main_of(this)).parent };
    if p.is_null() || iid.is_null() {
        return E_NOINTERFACE;
    }
    unsafe { query(p, &*iid, out) }
}
/// IDXGIDeviceSubObject::GetDevice: the device the object came from.
pub unsafe extern "system" fn DxSub_GetDevice(this: *mut Com, iid: *const Guid, out: *mut *mut Com) -> i32 {
    unsafe { DxObj_GetParent(this, iid, out) }
}

// ID3D11DeviceChild.
pub unsafe extern "system" fn Child_GetDevice(this: *mut Com, out: *mut *mut Com) {
    unsafe { out_ref(out, (*main_of(this)).parent) };
}
pub unsafe extern "system" fn Child_GetPrivateData(this: *mut Com, g: *const Guid, size: *mut u32, data: *mut u8) -> i32 {
    unsafe { get_private(this, g, size, data) }
}
pub unsafe extern "system" fn Child_SetPrivateData(this: *mut Com, g: *const Guid, size: u32, data: *const u8) -> i32 {
    unsafe { set_private(this, g, size, data) }
}
pub unsafe extern "system" fn Child_SetPrivateDataInterface(this: *mut Com, g: *const Guid, o: *mut Com) -> i32 {
    unsafe { set_private_interface(this, g, o) }
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
        OutputDebugStringA(b"d3d11: not implemented: \0".as_ptr());
        OutputDebugStringA(name.as_ptr());
        OutputDebugStringA(b"\n\0".as_ptr());
    }
}

/// The body of `p` (an interface pointer of it or an alias).
pub fn body<'a>(p: *mut Com) -> &'a mut Body {
    unsafe { &mut (*main_of(p)).body }
}
