//! ID3D11Device: creation (feature levels up to 10_1), what it supports,
//! and the batch of primitives waiting for the kernel. Its immediate
//! context is an alias of it (one object, one count); so is its
//! IDXGIDevice1.

use alloc::boxed::Box;
use alloc::vec::Vec;

use gfx3d::raster3d::{Group, Tri};

use crate::com::*;
use crate::format::*;
use crate::vtbl::*;

pub const LEVEL_9_1: u32 = 0x9100;
pub const LEVEL_10_0: u32 = 0xA000;
pub const LEVEL_10_1: u32 = 0xA100;
pub const LEVEL_11_0: u32 = 0xB000;
/// The highest feature level here (G5f2).
pub const LEVEL_MAX: u32 = LEVEL_11_0;

/// Primitives waiting to go to the kernel in one VEIL_GPU3D draw.
#[derive(Default)]
pub struct Batch {
    pub tris: Vec<Tri>,
    pub groups: Vec<Group>,
    pub attrs: Vec<f32>,
    pub target: u64,
    pub depth: u64,
}

pub struct Device {
    pub level: u32,
    pub flags: u32,
    pub ctx: *mut Com,
    pub state: crate::context::Bindings,
    pub batch: Batch,
    /// The colour target drawn into when only depth is bound: its size
    /// and handle.
    pub scratch: ((u32, u32), u64),
}

pub fn of<'a>(dev: *mut Com) -> &'a mut Device {
    match body(dev) {
        Body::Device(d) => d,
        _ => panic!(),
    }
}

impl Device {
    /// Send the waiting primitives to the kernel.
    pub fn flush(&mut self) {
        let b = &mut self.batch;
        if !b.tris.is_empty() {
            let args = gfx3d::raster3d::DrawArgs {
                groups: b.groups.as_ptr() as u64,
                group_count: b.groups.len() as u32,
                tri_count: b.tris.len() as u32,
                tris: b.tris.as_ptr() as u64,
                attrs: b.attrs.as_ptr() as u64,
                attr_len: b.attrs.len() as u32,
                _pad: 0,
            };
            gfx3d::gpu3d(2, b.target, &args as *const _ as u64, b.depth);
        }
        b.tris.clear();
        b.groups.clear();
        b.attrs.clear();
        for h in core::mem::take(&mut *crate::res::PENDING_CLOSE.lock()) {
            gfx3d::close(h);
        }
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        self.state.clear();
        self.flush();
        if self.scratch.1 != 0 {
            gfx3d::close(self.scratch.1);
        }
    }
}

static DEVICE_IIDS: [Guid; 1] = [IID_ID3D11DEVICE];
static CONTEXT_IIDS: [Guid; 2] = [IID_ID3D11DEVICECHILD, IID_ID3D11DEVICECONTEXT];

/// D3D11CreateDevice: hardware, WARP, reference and software all mean the
/// logical GPU; NULL makes nothing.
pub unsafe fn create(
    adapter: *mut Com,
    driver: u32,
    _software: usize,
    flags: u32,
    levels: *const u32,
    nlevels: u32,
    _sdk: u32,
    out: *mut *mut Com,
    level: *mut u32,
    ctx: *mut *mut Com,
) -> i32 {
    // An adapter given means driver type UNKNOWN (0), and the other way round.
    if (adapter.is_null()) == (driver == 0) {
        return E_INVALIDARG;
    }
    let wanted: Vec<u32> = if levels.is_null() || nlevels == 0 {
        alloc::vec![0xB000, LEVEL_10_1, LEVEL_10_0, 0x9300, 0x9200, LEVEL_9_1]
    } else {
        (0..nlevels as usize).map(|i| unsafe { *levels.add(i) }).collect()
    };
    let Some(&lv) = wanted.iter().find(|&&l| (LEVEL_9_1..=LEVEL_MAX).contains(&l)) else { return E_INVALIDARG };
    if !level.is_null() {
        unsafe { *level = lv };
    }
    if out.is_null() && ctx.is_null() {
        return S_FALSE;
    }
    // Its parent: the adapter (a new one if none was given).
    let ad = if adapter.is_null() { crate::dxgi::adapter(core::ptr::null_mut()) } else { main_of(adapter) };
    if !adapter.is_null() {
        unsafe { add_ref(ad) };
    }
    let d = Device { level: lv, flags, ctx: core::ptr::null_mut(), state: Default::default(), batch: Batch::default(), scratch: ((0, 0), 0) };
    let dev = Com::new(&ID3D11DEVICE, &DEVICE_IIDS, ad, Body::Device(Box::new(d)));
    unsafe { release(ad) };
    let c = alias(&ID3D11DEVICECONTEXT, &CONTEXT_IIDS);
    unsafe {
        (*c).primary = dev;
        (*dev).aliases.push(c);
    }
    of(dev).ctx = c;
    if !ctx.is_null() {
        unsafe { out_ref(ctx, c) };
    }
    if out.is_null() {
        unsafe { release(dev) };
    } else {
        unsafe { *out = dev };
    }
    S_OK
}

pub unsafe extern "system" fn Dev_GetFeatureLevel(this: *mut Com) -> u32 {
    of(main_of(this)).level
}

pub unsafe extern "system" fn Dev_GetCreationFlags(this: *mut Com) -> u32 {
    of(main_of(this)).flags
}

pub unsafe extern "system" fn Dev_GetDeviceRemovedReason(_this: *mut Com) -> i32 {
    S_OK
}

pub unsafe extern "system" fn Dev_GetImmediateContext(this: *mut Com, out: *mut *mut Com) {
    unsafe { out_ref(out, of(main_of(this)).ctx) };
}

pub unsafe extern "system" fn Dev_SetExceptionMode(_this: *mut Com, _f: u32) -> i32 {
    S_OK
}

pub unsafe extern "system" fn Dev_GetExceptionMode(_this: *mut Com) -> u32 {
    0
}

pub unsafe extern "system" fn Dev_GetPrivateData(this: *mut Com, g: *const Guid, size: *mut u32, data: *mut u8) -> i32 {
    unsafe { get_private(this, g, size, data) }
}

pub unsafe extern "system" fn Dev_SetPrivateData(this: *mut Com, g: *const Guid, size: u32, data: *const u8) -> i32 {
    unsafe { set_private(this, g, size, data) }
}

pub unsafe extern "system" fn Dev_SetPrivateDataInterface(this: *mut Com, g: *const Guid, o: *mut Com) -> i32 {
    unsafe { set_private_interface(this, g, o) }
}

/// D3D11_FORMAT_SUPPORT bits for a format.
fn support(f: u32) -> u32 {
    const BUFFER: u32 = 1;
    const IA_VERTEX: u32 = 2;
    const IA_INDEX: u32 = 4;
    const T1: u32 = 0x10;
    const T2: u32 = 0x20;
    const T3: u32 = 0x40;
    const CUBE: u32 = 0x80;
    const LOAD: u32 = 0x100;
    const SAMPLE: u32 = 0x200;
    const MIP: u32 = 0x1000;
    const AUTOGEN: u32 = 0x2000;
    const RT: u32 = 0x4000;
    const BLEND: u32 = 0x8000;
    const DEPTH: u32 = 0x10000;
    const CPU: u32 = 0x20000;
    const DISPLAY: u32 = 0x80000;
    match f {
        D24_UNORM_S8_UINT | D32_FLOAT | D16_UNORM | D32_FLOAT_S8X24_UINT => T1 | T2 | DEPTH,
        R16_UINT | R32_UINT => BUFFER | IA_INDEX | IA_VERTEX | T1 | T2 | T3 | LOAD | CPU,
        _ if is_bc(f) => T2 | T3 | CUBE | LOAD | SAMPLE | MIP | CPU,
        R8G8B8A8_UNORM | R8G8B8A8_UNORM_SRGB | B8G8R8A8_UNORM | B8G8R8A8_UNORM_SRGB | R10G10B10A2_UNORM | R16G16B16A16_FLOAT => {
            BUFFER | IA_VERTEX | T1 | T2 | T3 | CUBE | LOAD | SAMPLE | MIP | AUTOGEN | RT | BLEND | CPU | DISPLAY
        }
        R32G32B32A32_FLOAT | R32G32B32_FLOAT | R32G32_FLOAT | R32_FLOAT | R16G16_FLOAT | R16_FLOAT => BUFFER | IA_VERTEX | T1 | T2 | T3 | LOAD | SAMPLE | MIP | RT | BLEND | CPU,
        _ if bytes(f).is_some() => BUFFER | IA_VERTEX | T1 | T2 | T3 | LOAD | SAMPLE | MIP | RT | BLEND | CPU,
        _ => 0,
    }
}

pub unsafe extern "system" fn Dev_CheckFormatSupport(_this: *mut Com, f: u32, out: *mut u32) -> i32 {
    if out.is_null() {
        return E_INVALIDARG;
    }
    let s = support(f);
    unsafe { *out = s };
    if s == 0 { E_FAIL } else { S_OK }
}

pub unsafe extern "system" fn Dev_CheckMultisampleQualityLevels(_this: *mut Com, f: u32, count: u32, out: *mut u32) -> i32 {
    if out.is_null() {
        return E_INVALIDARG;
    }
    unsafe { *out = if count == 1 && support(f) != 0 { 1 } else { 0 } };
    S_OK
}

/// Nothing optional: threading, doubles, the 10.x compute options, 11's
/// options all report none; format support as CheckFormatSupport.
pub unsafe extern "system" fn Dev_CheckFeatureSupport(_this: *mut Com, feature: u32, data: *mut u8, size: u32) -> i32 {
    if data.is_null() {
        return E_INVALIDARG;
    }
    match feature {
        2 if size >= 8 => {
            let f = unsafe { core::ptr::read_unaligned(data as *const u32) };
            unsafe { core::ptr::write_unaligned(data.add(4) as *mut u32, support(f)) };
            S_OK
        }
        0..=1 | 3..=20 => {
            unsafe { core::ptr::write_bytes(data, 0, size as usize) };
            if feature == 3 && size >= 8 {
                // FORMAT_SUPPORT2: keep the format.
            }
            S_OK
        }
        _ => E_INVALIDARG,
    }
}

pub unsafe extern "system" fn Dev_CheckCounterInfo(_this: *mut Com, out: *mut [u32; 3]) {
    if !out.is_null() {
        unsafe { *out = [0; 3] };
    }
}

// -----| the IDXGIDevice side |-----

pub unsafe extern "system" fn DxDev_GetAdapter(this: *mut Com, out: *mut *mut Com) -> i32 {
    if out.is_null() {
        return E_INVALIDARG;
    }
    unsafe { out_ref(out, (*main_of(this)).parent) };
    S_OK
}

pub unsafe extern "system" fn DxDev_SetGPUThreadPriority(_this: *mut Com, _p: i32) -> i32 {
    S_OK
}

pub unsafe extern "system" fn DxDev_GetGPUThreadPriority(_this: *mut Com, p: *mut i32) -> i32 {
    if !p.is_null() {
        unsafe { *p = 0 };
    }
    S_OK
}

pub unsafe extern "system" fn DxDev1_SetMaximumFrameLatency(_this: *mut Com, _n: u32) -> i32 {
    S_OK
}

pub unsafe extern "system" fn DxDev1_GetMaximumFrameLatency(_this: *mut Com, n: *mut u32) -> i32 {
    if !n.is_null() {
        unsafe { *n = 3 };
    }
    S_OK
}

// -----| the context as a device child |-----

pub unsafe extern "system" fn Ctx_GetDevice(this: *mut Com, out: *mut *mut Com) {
    match crate::deferred::of(this) {
        Some(d) => unsafe { out_ref(out, d.device) },
        None => unsafe { out_ref(out, main_of(this)) },
    }
}

/// 0 immediate, 1 deferred.
pub unsafe extern "system" fn Ctx_GetType(this: *mut Com) -> u32 {
    crate::deferred::of(this).is_some() as u32
}

pub unsafe extern "system" fn Ctx_GetContextFlags(_this: *mut Com) -> u32 {
    0
}

pub unsafe extern "system" fn Ctx_Flush(this: *mut Com) {
    if let Some(d) = crate::context::immediate(this) {
        d.flush();
    }
}
