//! DXGI: a factory, one adapter (the logical GPU), its one output (the
//! desktop), and swap chains: a window's back buffer, a Direct3D 11
//! texture, shown with StretchDIBits at Present.

use alloc::boxed::Box;
use alloc::vec::Vec;

use crate::com::*;
use crate::format::*;
use crate::vtbl::*;

#[link(name = "user32", kind = "raw-dylib")]
unsafe extern "system" {
    fn GetSystemMetrics(i: i32) -> i32;
    fn GetClientRect(hwnd: usize, r: *mut [i32; 4]) -> i32;
    fn MonitorFromWindow(hwnd: usize, flags: u32) -> usize;
    fn GetDC(hwnd: usize) -> usize;
    fn ReleaseDC(hwnd: usize, hdc: usize) -> i32;
}

#[link(name = "gdi32", kind = "raw-dylib")]
unsafe extern "system" {
    fn StretchDIBits(hdc: usize, dx: i32, dy: i32, dw: i32, dh: i32, sx: i32, sy: i32, sw: i32, sh: i32, bits: *const u8, bmi: *const u8, usage: u32, rop: u32) -> i32;
}

static FACTORY_IIDS: [Guid; 4] = [IID_IDXGIOBJECT, IID_IDXGIFACTORY, IID_IDXGIFACTORY1, IID_IDXGIFACTORY2];
static ADAPTER_IIDS: [Guid; 3] = [IID_IDXGIOBJECT, IID_IDXGIADAPTER, IID_IDXGIADAPTER1];
static OUTPUT_IIDS: [Guid; 2] = [IID_IDXGIOBJECT, IID_IDXGIOUTPUT];
static SWAP_IIDS: [Guid; 4] = [IID_IDXGIOBJECT, IID_IDXGIDEVICESUBOBJECT, IID_IDXGISWAPCHAIN, IID_IDXGISWAPCHAIN1];

/// DXGI_MODE_DESC.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct ModeDesc {
    pub w: u32,
    pub h: u32,
    pub refresh: [u32; 2],
    pub format: u32,
    pub scanline: u32,
    pub scaling: u32,
}

/// DXGI_SWAP_CHAIN_DESC.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct SwapChainDesc {
    pub mode: ModeDesc,
    pub sample: [u32; 2],
    pub usage: u32,
    pub count: u32,
    pub hwnd: usize,
    pub windowed: i32,
    pub effect: u32,
    pub flags: u32,
}

/// DXGI_SWAP_CHAIN_DESC1.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct SwapChainDesc1 {
    pub w: u32,
    pub h: u32,
    pub format: u32,
    pub stereo: i32,
    pub sample: [u32; 2],
    pub usage: u32,
    pub count: u32,
    pub scaling: u32,
    pub effect: u32,
    pub alpha: u32,
    pub flags: u32,
}

/// The desktop's size.
pub fn desktop() -> (u32, u32) {
    let (w, h) = unsafe { (GetSystemMetrics(0), GetSystemMetrics(1)) };
    (w.max(640) as u32, h.max(480) as u32)
}

/// The one factory object's children: made on demand, each its own.
pub unsafe fn create_factory(iid: *const Guid, out: *mut *mut Com) -> i32 {
    if out.is_null() || iid.is_null() {
        return E_INVALIDARG;
    }
    let f = Com::new(&IDXGIFACTORY2, &FACTORY_IIDS, core::ptr::null_mut(), Body::Factory);
    let r = unsafe { query(f, &*iid, out) };
    unsafe { release(f) };
    r
}

/// The adapter, its parent `factory` (a new object each time, as cheap as
/// the real ones are equal).
pub fn adapter(factory: *mut Com) -> *mut Com {
    Com::new(&IDXGIADAPTER1, &ADAPTER_IIDS, factory, Body::Adapter)
}

// -----| IDXGIFactory, 1, 2 |-----

pub unsafe extern "system" fn Fac_EnumAdapters(this: *mut Com, i: u32, out: *mut *mut Com) -> i32 {
    if out.is_null() {
        return E_INVALIDARG;
    }
    if i != 0 {
        unsafe { *out = core::ptr::null_mut() };
        return DXGI_ERROR_NOT_FOUND;
    }
    unsafe { *out = adapter(main_of(this)) };
    S_OK
}

pub unsafe extern "system" fn Fac1_EnumAdapters1(this: *mut Com, i: u32, out: *mut *mut Com) -> i32 {
    unsafe { Fac_EnumAdapters(this, i, out) }
}

pub unsafe extern "system" fn Fac1_IsCurrent(_this: *mut Com) -> i32 {
    1
}

pub unsafe extern "system" fn Fac_MakeWindowAssociation(_this: *mut Com, _hwnd: usize, _flags: u32) -> i32 {
    S_OK
}

pub unsafe extern "system" fn Fac_GetWindowAssociation(_this: *mut Com, out: *mut usize) -> i32 {
    if !out.is_null() {
        unsafe { *out = 0 };
    }
    S_OK
}

pub unsafe extern "system" fn Fac_CreateSoftwareAdapter(_this: *mut Com, _module: usize, out: *mut *mut Com) -> i32 {
    if !out.is_null() {
        unsafe { *out = core::ptr::null_mut() };
    }
    DXGI_ERROR_UNSUPPORTED
}

pub unsafe extern "system" fn Fac_CreateSwapChain(_this: *mut Com, device: *mut Com, desc: *const SwapChainDesc, out: *mut *mut Com) -> i32 {
    if device.is_null() || desc.is_null() || out.is_null() {
        return DXGI_ERROR_INVALID_CALL;
    }
    unsafe { swap_chain_for(device, &*desc, out) }
}

pub unsafe extern "system" fn Fac2_CreateSwapChainForHwnd(_this: *mut Com, device: *mut Com, hwnd: usize, desc: *const SwapChainDesc1, _fs: *const u8, _output: *mut Com, out: *mut *mut Com) -> i32 {
    if device.is_null() || desc.is_null() || out.is_null() || hwnd == 0 {
        return DXGI_ERROR_INVALID_CALL;
    }
    unsafe { make_swap_chain(device, hwnd, *desc, true, out) }
}

pub unsafe extern "system" fn Fac2_IsWindowedStereoEnabled(_this: *mut Com) -> i32 {
    0
}

pub unsafe extern "system" fn Fac2_GetSharedResourceAdapterLuid(_this: *mut Com, _h: usize, luid: *mut [u32; 2]) -> i32 {
    if !luid.is_null() {
        unsafe { *luid = LUID };
    }
    S_OK
}

pub unsafe extern "system" fn Fac2_RegisterOcclusionStatusWindow(_this: *mut Com, _hwnd: usize, _msg: u32, cookie: *mut u32) -> i32 {
    if !cookie.is_null() {
        unsafe { *cookie = 1 };
    }
    S_OK
}

pub unsafe extern "system" fn Fac2_UnregisterOcclusionStatus(_this: *mut Com, _cookie: u32) {}

// -----| IDXGIAdapter, 1 |-----

const LUID: [u32; 2] = [0x5645_494C, 0];

fn adapter_desc(out: *mut u8, one: bool) {
    let mut d = [0u8; 312];
    for (i, c) in "VEILOS logical GPU".encode_utf16().enumerate() {
        d[2 * i..2 * i + 2].copy_from_slice(&c.to_le_bytes());
    }
    // Vendor, device, subsystem, revision.
    d[256..260].copy_from_slice(&0x1414u32.to_le_bytes());
    d[260..264].copy_from_slice(&0x0001u32.to_le_bytes());
    // Video memory: 512 MiB dedicated, 1 GiB shared.
    d[272..280].copy_from_slice(&(512u64 << 20).to_le_bytes());
    d[288..296].copy_from_slice(&(1024u64 << 20).to_le_bytes());
    d[296..300].copy_from_slice(&LUID[0].to_le_bytes());
    unsafe { core::ptr::copy_nonoverlapping(d.as_ptr(), out, if one { 312 } else { 304 }) };
}

pub unsafe extern "system" fn Adp_GetDesc(_this: *mut Com, out: *mut u8) -> i32 {
    if out.is_null() {
        return E_INVALIDARG;
    }
    adapter_desc(out, false);
    S_OK
}

pub unsafe extern "system" fn Adp1_GetDesc1(_this: *mut Com, out: *mut u8) -> i32 {
    if out.is_null() {
        return E_INVALIDARG;
    }
    adapter_desc(out, true);
    S_OK
}

pub unsafe extern "system" fn Adp_EnumOutputs(this: *mut Com, i: u32, out: *mut *mut Com) -> i32 {
    if out.is_null() {
        return E_INVALIDARG;
    }
    if i != 0 {
        unsafe { *out = core::ptr::null_mut() };
        return DXGI_ERROR_NOT_FOUND;
    }
    unsafe { *out = Com::new(&IDXGIOUTPUT, &OUTPUT_IIDS, main_of(this), Body::Output) };
    S_OK
}

/// Direct3D 10 and 11 are supported (UMD version 9.0.1.0).
pub unsafe extern "system" fn Adp_CheckInterfaceSupport(_this: *mut Com, _iid: *const Guid, umd: *mut u64) -> i32 {
    if !umd.is_null() {
        unsafe { *umd = 9u64 << 48 | 1 << 16 };
    }
    S_OK
}

// -----| IDXGIOutput |-----

pub unsafe extern "system" fn Out_GetDesc(_this: *mut Com, out: *mut u8) -> i32 {
    if out.is_null() {
        return E_INVALIDARG;
    }
    let mut d = [0u8; 96];
    for (i, c) in "\\\\.\\DISPLAY1".encode_utf16().enumerate() {
        d[2 * i..2 * i + 2].copy_from_slice(&c.to_le_bytes());
    }
    let (w, h) = desktop();
    d[72..76].copy_from_slice(&w.to_le_bytes());
    d[76..80].copy_from_slice(&h.to_le_bytes());
    d[80..84].copy_from_slice(&1u32.to_le_bytes());
    d[84..88].copy_from_slice(&1u32.to_le_bytes());
    d[88..96].copy_from_slice(&(unsafe { MonitorFromWindow(0, 1) } as u64).to_le_bytes());
    unsafe { core::ptr::copy_nonoverlapping(d.as_ptr(), out, 96) };
    S_OK
}

fn modes(format: u32) -> Vec<ModeDesc> {
    if !matches!(format, R8G8B8A8_UNORM | R8G8B8A8_UNORM_SRGB | B8G8R8A8_UNORM | B8G8R8A8_UNORM_SRGB) {
        return Vec::new();
    }
    let (dw, dh) = desktop();
    let mut v: Vec<(u32, u32)> = [(640, 480), (800, 600), (1024, 768), (1280, 720), (1280, 1024), (1600, 900), (1920, 1080)]
        .into_iter()
        .filter(|&(w, h)| w <= dw && h <= dh)
        .collect();
    if !v.contains(&(dw, dh)) {
        v.push((dw, dh));
    }
    v.into_iter().map(|(w, h)| ModeDesc { w, h, refresh: [60, 1], format, scanline: 1, scaling: 0 }).collect()
}

pub unsafe extern "system" fn Out_GetDisplayModeList(_this: *mut Com, format: u32, _flags: u32, count: *mut u32, out: *mut ModeDesc) -> i32 {
    if count.is_null() {
        return DXGI_ERROR_INVALID_CALL;
    }
    let m = modes(format);
    if out.is_null() {
        unsafe { *count = m.len() as u32 };
        return S_OK;
    }
    let n = (unsafe { *count } as usize).min(m.len());
    for (i, x) in m.iter().take(n).enumerate() {
        unsafe { *out.add(i) = *x };
    }
    unsafe { *count = n as u32 };
    if n < m.len() { DXGI_ERROR_MORE_DATA } else { S_OK }
}

pub unsafe extern "system" fn Out_FindClosestMatchingMode(_this: *mut Com, want: *const ModeDesc, out: *mut ModeDesc, _dev: *mut Com) -> i32 {
    if want.is_null() || out.is_null() {
        return DXGI_ERROR_INVALID_CALL;
    }
    let w = unsafe { *want };
    let f = if w.format == 0 { R8G8B8A8_UNORM } else { w.format };
    let m = modes(f);
    let best = m.iter().min_by_key(|x| (x.w as i64 - w.w as i64).abs() + (x.h as i64 - w.h as i64).abs());
    match best {
        Some(b) => {
            unsafe { *out = *b };
            S_OK
        }
        None => DXGI_ERROR_NOT_FOUND,
    }
}

pub unsafe extern "system" fn Out_WaitForVBlank(_this: *mut Com) -> i32 {
    S_OK
}

pub unsafe extern "system" fn Out_TakeOwnership(_this: *mut Com, _dev: *mut Com, _excl: i32) -> i32 {
    S_OK
}

pub unsafe extern "system" fn Out_ReleaseOwnership(_this: *mut Com) {}

// -----| swap chains |-----

pub struct SwapChain {
    pub device: *mut Com,
    pub hwnd: usize,
    pub desc: SwapChainDesc1,
    pub windowed: bool,
    /// The back buffer (a texture), held.
    pub buffer: *mut Com,
    pub presents: u32,
}

impl Drop for SwapChain {
    fn drop(&mut self) {
        unsafe {
            release(self.buffer);
            release(self.device);
        }
    }
}

/// A swap chain from DXGI_SWAP_CHAIN_DESC.
pub unsafe fn swap_chain_for(device: *mut Com, d: &SwapChainDesc, out: *mut *mut Com) -> i32 {
    let d1 = SwapChainDesc1 {
        w: d.mode.w,
        h: d.mode.h,
        format: d.mode.format,
        stereo: 0,
        sample: d.sample,
        usage: d.usage,
        count: d.count,
        scaling: 0,
        effect: d.effect,
        alpha: 0,
        flags: d.flags,
    };
    unsafe { make_swap_chain(device, d.hwnd, d1, d.windowed != 0, out) }
}

/// The back buffer for `d` (its size filled in from the window).
fn make_buffer(device: *mut Com, hwnd: usize, d: &mut SwapChainDesc1) -> Result<*mut Com, i32> {
    if d.w == 0 || d.h == 0 {
        let mut r = [0i32; 4];
        unsafe { GetClientRect(hwnd, &mut r) };
        if d.w == 0 {
            d.w = (r[2] - r[0]).max(8) as u32;
        }
        if d.h == 0 {
            d.h = (r[3] - r[1]).max(8) as u32;
        }
    }
    let td = crate::res::Tex2DDesc {
        w: d.w,
        h: d.h,
        mips: 1,
        array: 1,
        format: d.format,
        sample: [1, 0],
        usage: 0,
        bind: crate::res::BIND_RENDER_TARGET | crate::res::BIND_SHADER_RESOURCE,
        cpu: 0,
        misc: 0,
    };
    crate::res::make_texture2d(device, &td, core::ptr::null())
}

unsafe fn make_swap_chain(device: *mut Com, hwnd: usize, mut d: SwapChainDesc1, windowed: bool, out: *mut *mut Com) -> i32 {
    // The device it's for, as a Direct3D 11 device.
    let dev = main_of(device);
    if !matches!(body(dev), Body::Device(_)) {
        return DXGI_ERROR_INVALID_CALL;
    }
    if !matches!(d.format, R8G8B8A8_UNORM | R8G8B8A8_UNORM_SRGB | B8G8R8A8_UNORM | B8G8R8A8_UNORM_SRGB | R10G10B10A2_UNORM | R16G16B16A16_FLOAT) {
        return DXGI_ERROR_INVALID_CALL;
    }
    let buffer = match make_buffer(dev, hwnd, &mut d) {
        Ok(b) => b,
        Err(e) => return e,
    };
    unsafe { add_ref(dev) };
    let sc = SwapChain { device: dev, hwnd, desc: d, windowed, buffer, presents: 0 };
    // Its parent: a factory.
    let mut f: *mut Com = core::ptr::null_mut();
    unsafe { create_factory(&IID_IDXGIFACTORY2, &mut f) };
    unsafe { *out = Com::new(&IDXGISWAPCHAIN1, &SWAP_IIDS, f, Body::SwapChain(Box::new(sc))) };
    unsafe { release(f) };
    S_OK
}

fn chain<'a>(this: *mut Com) -> &'a mut SwapChain {
    match body(this) {
        Body::SwapChain(s) => s,
        _ => panic!(),
    }
}

pub unsafe extern "system" fn SC_Present(this: *mut Com, _sync: u32, flags: u32) -> i32 {
    let s = chain(this);
    // DXGI_PRESENT_TEST: nothing shown.
    if flags & 1 != 0 {
        return S_OK;
    }
    crate::device::of(s.device).flush();
    let r = crate::res::resource(s.buffer);
    let px = r.read_argb(0);
    let (w, h) = (s.desc.w as i32, s.desc.h as i32);
    let mut cr = [0i32; 4];
    unsafe { GetClientRect(s.hwnd, &mut cr) };
    let mut bmi = [0u8; 40];
    bmi[0] = 40;
    bmi[4..8].copy_from_slice(&w.to_le_bytes());
    bmi[8..12].copy_from_slice(&(-h).to_le_bytes());
    bmi[12] = 1;
    bmi[14] = 32;
    unsafe {
        let hdc = GetDC(s.hwnd);
        StretchDIBits(hdc, 0, 0, cr[2] - cr[0], cr[3] - cr[1], 0, 0, w, h, px.as_ptr() as *const u8, bmi.as_ptr(), 0, 0x00CC_0020);
        ReleaseDC(s.hwnd, hdc);
    }
    s.presents += 1;
    S_OK
}

pub unsafe extern "system" fn SC1_Present1(this: *mut Com, sync: u32, flags: u32, _params: *const u8) -> i32 {
    unsafe { SC_Present(this, sync, flags) }
}

/// Every buffer is the one back buffer (copied to the window at Present).
pub unsafe extern "system" fn SC_GetBuffer(this: *mut Com, _i: u32, iid: *const Guid, out: *mut *mut Com) -> i32 {
    if iid.is_null() || out.is_null() {
        return E_INVALIDARG;
    }
    unsafe { query(chain(this).buffer, &*iid, out) }
}

pub unsafe extern "system" fn SC_SetFullscreenState(this: *mut Com, full: i32, _target: *mut Com) -> i32 {
    chain(this).windowed = full == 0;
    S_OK
}

pub unsafe extern "system" fn SC_GetFullscreenState(this: *mut Com, full: *mut i32, target: *mut *mut Com) -> i32 {
    if !full.is_null() {
        unsafe { *full = !chain(this).windowed as i32 };
    }
    if !target.is_null() {
        unsafe { *target = core::ptr::null_mut() };
    }
    S_OK
}

pub unsafe extern "system" fn SC_GetDesc(this: *mut Com, out: *mut SwapChainDesc) -> i32 {
    if out.is_null() {
        return E_INVALIDARG;
    }
    let s = chain(this);
    let d = &s.desc;
    unsafe {
        *out = SwapChainDesc {
            mode: ModeDesc { w: d.w, h: d.h, refresh: [60, 1], format: d.format, scanline: 0, scaling: 0 },
            sample: d.sample,
            usage: d.usage,
            count: d.count,
            hwnd: s.hwnd,
            windowed: s.windowed as i32,
            effect: d.effect,
            flags: d.flags,
        }
    };
    S_OK
}

pub unsafe extern "system" fn SC1_GetDesc1(this: *mut Com, out: *mut SwapChainDesc1) -> i32 {
    if out.is_null() {
        return E_INVALIDARG;
    }
    unsafe { *out = chain(this).desc };
    S_OK
}

pub unsafe extern "system" fn SC1_GetHwnd(this: *mut Com, out: *mut usize) -> i32 {
    if out.is_null() {
        return E_INVALIDARG;
    }
    unsafe { *out = chain(this).hwnd };
    S_OK
}

/// A new back buffer; the old one must have no references but ours.
pub unsafe extern "system" fn SC_ResizeBuffers(this: *mut Com, count: u32, w: u32, h: u32, format: u32, flags: u32) -> i32 {
    let s = chain(this);
    let mut d = s.desc;
    if count != 0 {
        d.count = count;
    }
    (d.w, d.h) = (w, h);
    if format != 0 {
        d.format = format;
    }
    d.flags = flags;
    crate::device::of(s.device).flush();
    match make_buffer(s.device, s.hwnd, &mut d) {
        Ok(b) => {
            unsafe { release(s.buffer) };
            s.buffer = b;
            s.desc = d;
            S_OK
        }
        Err(e) => e,
    }
}

pub unsafe extern "system" fn SC_ResizeTarget(_this: *mut Com, _mode: *const ModeDesc) -> i32 {
    S_OK
}

pub unsafe extern "system" fn SC_GetContainingOutput(this: *mut Com, out: *mut *mut Com) -> i32 {
    if out.is_null() {
        return E_INVALIDARG;
    }
    let a = adapter(core::ptr::null_mut());
    let _ = this;
    let r = unsafe { Adp_EnumOutputs(a, 0, out) };
    unsafe { release(a) };
    r
}

pub unsafe extern "system" fn SC_GetLastPresentCount(this: *mut Com, out: *mut u32) -> i32 {
    if out.is_null() {
        return E_INVALIDARG;
    }
    unsafe { *out = chain(this).presents };
    S_OK
}

pub unsafe extern "system" fn SC1_IsTemporaryMonoSupported(_this: *mut Com) -> i32 {
    0
}

pub unsafe extern "system" fn SC1_SetBackgroundColor(_this: *mut Com, _c: *const [f32; 4]) -> i32 {
    S_OK
}

pub unsafe extern "system" fn SC1_SetRotation(_this: *mut Com, _r: u32) -> i32 {
    S_OK
}

pub unsafe extern "system" fn SC1_GetRotation(_this: *mut Com, r: *mut u32) -> i32 {
    if !r.is_null() {
        unsafe { *r = 1 };
    }
    S_OK
}
