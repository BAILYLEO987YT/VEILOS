//! IDirect3D9: one adapter (the logical GPU), its display modes and
//! formats, its capabilities (D3DCAPS9), and CreateDevice.

use crate::com::*;
use crate::device::{self, Params};
use crate::format::*;

#[link(name = "user32", kind = "raw-dylib")]
unsafe extern "system" {
    fn GetSystemMetrics(i: i32) -> i32;
    fn GetClientRect(hwnd: usize, r: *mut [i32; 4]) -> i32;
    fn MonitorFromWindow(hwnd: usize, flags: u32) -> usize;
}

/// The desktop's mode: D3DDISPLAYMODE (width, height, refresh, format).
pub fn desktop_mode() -> [u32; 4] {
    let (w, h) = unsafe { (GetSystemMetrics(0), GetSystemMetrics(1)) };
    [w.max(640) as u32, h.max(480) as u32, 60, X8R8G8B8]
}

/// The modes offered: the common sizes up to the desktop's, and it.
fn modes() -> alloc::vec::Vec<(u32, u32)> {
    let [dw, dh, _, _] = desktop_mode();
    let mut v: alloc::vec::Vec<(u32, u32)> = [(640, 480), (800, 600), (1024, 768), (1280, 720), (1280, 1024), (1600, 900), (1920, 1080)]
        .into_iter()
        .filter(|&(w, h)| w <= dw && h <= dh)
        .collect();
    if !v.contains(&(dw, dh)) {
        v.push((dw, dh));
    }
    v
}

fn display_format(f: u32) -> bool {
    f == X8R8G8B8 || f == R5G6B5
}

pub unsafe extern "system" fn D3D_RegisterSoftwareDevice(_this: *mut Com, _f: usize) -> i32 {
    S_OK
}

pub unsafe extern "system" fn D3D_GetAdapterCount(_this: *mut Com) -> u32 {
    1
}

pub unsafe extern "system" fn D3D_GetAdapterIdentifier(_this: *mut Com, adapter: u32, _flags: u32, out: *mut u8) -> i32 {
    if adapter != 0 || out.is_null() {
        return D3DERR_INVALIDCALL;
    }
    let mut d = [0u8; 1104];
    let put = |d: &mut [u8; 1104], at: usize, s: &[u8]| d[at..at + s.len()].copy_from_slice(s);
    put(&mut d, 0, b"veilos_lgpu.dll");
    put(&mut d, 512, b"VEILOS logical GPU");
    put(&mut d, 1024, b"\\\\.\\DISPLAY1");
    // DriverVersion: 9.0.1.0.
    d[1056..1064].copy_from_slice(&(9u64 << 48 | 1 << 16).to_le_bytes());
    // DeviceIdentifier, and WHQLLevel 1.
    d[1080..1096].copy_from_slice(&[0x56, 0x45, 0x49, 0x4C, 0x4F, 0x53, 0x47, 0x50, 0x55, 0x30, 0x31, 0x00, 0x00, 0x00, 0x00, 0x01]);
    d[1096..1100].copy_from_slice(&1u32.to_le_bytes());
    unsafe { core::ptr::copy_nonoverlapping(d.as_ptr(), out, d.len()) };
    S_OK
}

pub unsafe extern "system" fn D3D_GetAdapterModeCount(_this: *mut Com, adapter: u32, format: u32) -> u32 {
    if adapter != 0 || !display_format(format) { 0 } else { modes().len() as u32 }
}

pub unsafe extern "system" fn D3D_EnumAdapterModes(_this: *mut Com, adapter: u32, format: u32, i: u32, out: *mut [u32; 4]) -> i32 {
    let m = modes();
    if adapter != 0 || !display_format(format) || out.is_null() {
        return D3DERR_INVALIDCALL;
    }
    let Some(&(w, h)) = m.get(i as usize) else { return D3DERR_INVALIDCALL };
    unsafe { *out = [w, h, 60, format] };
    S_OK
}

pub unsafe extern "system" fn D3D_GetAdapterDisplayMode(_this: *mut Com, adapter: u32, out: *mut [u32; 4]) -> i32 {
    if adapter != 0 || out.is_null() {
        return D3DERR_INVALIDCALL;
    }
    unsafe { *out = desktop_mode() };
    S_OK
}

pub unsafe extern "system" fn D3D_CheckDeviceType(_this: *mut Com, adapter: u32, _t: u32, display: u32, back: u32, windowed: i32) -> i32 {
    let ok = adapter == 0 && (display_format(display) || (windowed != 0 && display == 0)) && (back == 0 || TARGET_FORMATS.contains(&back));
    if ok { S_OK } else { D3DERR_NOTAVAILABLE }
}

pub unsafe extern "system" fn D3D_CheckDeviceFormat(_this: *mut Com, adapter: u32, _t: u32, _display: u32, usage: u32, rtype: u32, format: u32) -> i32 {
    if adapter != 0 {
        return D3DERR_INVALIDCALL;
    }
    // Not yet: volume textures, vertex textures, sRGB, bump maps.
    const NO: u32 = 0x8000 | 0x10000 | 0x40000 | 0x100000 | 0x200000;
    if usage & NO != 0 || rtype == 4 {
        return D3DERR_NOTAVAILABLE;
    }
    let ok = match rtype {
        6 | 7 => true,
        _ if usage & 2 != 0 => DEPTH_FORMATS.contains(&format) && rtype == 1,
        _ if usage & 1 != 0 => TARGET_FORMATS.contains(&format),
        _ => TEXTURE_FORMATS.contains(&format) || (rtype == 1 && TARGET_FORMATS.contains(&format)),
    };
    if ok { S_OK } else { D3DERR_NOTAVAILABLE }
}

pub unsafe extern "system" fn D3D_CheckDeviceMultiSampleType(_this: *mut Com, _a: u32, _t: u32, _f: u32, _w: i32, ms: u32, quality: *mut u32) -> i32 {
    if ms != 0 {
        return D3DERR_NOTAVAILABLE;
    }
    if !quality.is_null() {
        unsafe { *quality = 1 };
    }
    S_OK
}

pub unsafe extern "system" fn D3D_CheckDepthStencilMatch(_this: *mut Com, _a: u32, _t: u32, _d: u32, _rt: u32, ds: u32) -> i32 {
    if DEPTH_FORMATS.contains(&ds) { S_OK } else { D3DERR_NOTAVAILABLE }
}

pub unsafe extern "system" fn D3D_CheckDeviceFormatConversion(_this: *mut Com, _a: u32, _t: u32, src: u32, dst: u32) -> i32 {
    if TARGET_FORMATS.contains(&src) && TARGET_FORMATS.contains(&dst) { S_OK } else { D3DERR_NOTAVAILABLE }
}

pub unsafe extern "system" fn D3D_GetDeviceCaps(_this: *mut Com, adapter: u32, t: u32, out: *mut u8) -> i32 {
    if adapter != 0 || out.is_null() {
        return D3DERR_INVALIDCALL;
    }
    caps(t, out);
    S_OK
}

pub unsafe extern "system" fn D3D_GetAdapterMonitor(_this: *mut Com, _adapter: u32) -> usize {
    unsafe { MonitorFromWindow(0, 1) }
}

/// Fill in what a program may leave out of its D3DPRESENT_PARAMETERS: the
/// size of the window, the desktop's format. False if they can't work.
pub fn complete_params(p: &mut Params, focus: usize) -> bool {
    let hwnd = if p.hwnd != 0 { p.hwnd } else { focus };
    if p.w == 0 || p.h == 0 {
        let mut r = [0i32; 4];
        unsafe { GetClientRect(hwnd, &mut r) };
        if p.w == 0 {
            p.w = (r[2] - r[0]).max(1) as u32;
        }
        if p.h == 0 {
            p.h = (r[3] - r[1]).max(1) as u32;
        }
    }
    if p.format == 0 {
        p.format = X8R8G8B8;
    }
    p.count = p.count.max(1);
    TARGET_FORMATS.contains(&p.format) && (p.auto_depth == 0 || DEPTH_FORMATS.contains(&p.depth_format)) && p.w <= 8192 && p.h <= 8192
}

pub unsafe extern "system" fn D3D_CreateDevice(this: *mut Com, adapter: u32, t: u32, focus: usize, flags: u32, params: *mut Params, out: *mut *mut Com) -> i32 {
    if adapter != 0 || params.is_null() || out.is_null() || !(1..=3).contains(&t) {
        return D3DERR_INVALIDCALL;
    }
    let mut p = unsafe { core::ptr::read_unaligned(params) };
    if !complete_params(&mut p, focus) {
        return D3DERR_INVALIDCALL;
    }
    unsafe { core::ptr::write_unaligned(params, p) };
    match device::create(this, t, focus, flags, p) {
        Some(d) => {
            unsafe { *out = d };
            S_OK
        }
        None => D3DERR_OUTOFVIDEOMEMORY,
    }
}

/// D3DCAPS9 (304 bytes) for device type `t`.
pub fn caps(t: u32, out: *mut u8) {
    let mut c = [0u8; 304];
    let mut u = |at: usize, v: u32| c[at..at + 4].copy_from_slice(&v.to_le_bytes());
    let f = |v: f32| v.to_bits();
    u(0, t);
    u(12, 0x2000_0000 | 0x2_0000); // Caps2: DYNAMICTEXTURES, FULLSCREENGAMMA
    u(16, 0x20 | 0x100 | 0x200); // Caps3: alpha flip/discard, copies to video and system memory
    u(20, 0x8000_0000 | 1); // PresentationIntervals: IMMEDIATE, ONE
    // DevCaps: execute and TL vertices from system and video memory,
    // textures in both, DrawPrimitives2(Ex), hardware T&L and rasterization.
    u(28, 0x10 | 0x20 | 0x40 | 0x80 | 0x100 | 0x200 | 0x400 | 0x800 | 0x1000 | 0x2000 | 0x8000 | 0x1_0000 | 0x2_0000 | 0x8_0000);
    // PrimitiveMiscCaps: MASKZ, CULLNONE/CW/CCW, COLORWRITEENABLE,
    // CLIPPLANESCALEDPOINTS, TSSARGTEMP, BLENDOP.
    u(32, 2 | 0x10 | 0x20 | 0x40 | 0x80 | 0x100 | 0x400 | 0x800);
    // RasterCaps: DITHER, ZTEST, FOGVERTEX, FOGTABLE, MIPMAPLODBIAS,
    // FOGRANGE, WFOG, ZFOG, SCISSORTEST, SLOPESCALEDEPTHBIAS, DEPTHBIAS.
    u(36, 1 | 0x10 | 0x80 | 0x100 | 0x2000 | 0x1_0000 | 0x10_0000 | 0x20_0000 | 0x100_0000 | 0x200_0000 | 0x400_0000);
    u(40, 0xFF); // ZCmpCaps
    u(44, 0x3FFF); // SrcBlendCaps
    u(48, 0x3FFF); // DestBlendCaps
    u(52, 0xFF); // AlphaCmpCaps
    u(56, 8 | 0x200 | 0x4000 | 0x8_0000); // ShadeCaps: Gouraud colour, specular, alpha, fog
    u(60, 1 | 4 | 0x400 | 0x4000); // TextureCaps: PERSPECTIVE, ALPHA, PROJECTED, MIPMAP
    u(64, 0x100 | 0x200 | 0x1_0000 | 0x2_0000 | 0x100_0000 | 0x200_0000); // TextureFilterCaps
    u(76, 1 | 2 | 4 | 8 | 0x10 | 0x20); // TextureAddressCaps
    u(84, 1 | 2 | 4 | 8 | 0x10); // LineCaps
    u(88, 8192); // MaxTextureWidth
    u(92, 8192); // MaxTextureHeight
    u(100, 8192); // MaxTextureRepeat
    u(104, 8192); // MaxTextureAspectRatio
    u(108, 1); // MaxAnisotropy
    u(112, f(1e10)); // MaxVertexW
    u(116, f(-32768.0));
    u(120, f(-32768.0));
    u(124, f(32767.0));
    u(128, f(32767.0));
    u(136, 0x1FF); // StencilCaps
    u(140, 8 | 0x10_0000); // FVFCaps: 8 texture coordinates, PSIZE
    // TextureOpCaps: every operation but PREMODULATE and the bump maps.
    u(144, 0x3FF_FFFF & !(0x1_0000 | 0x20_0000 | 0x40_0000));
    u(148, 8); // MaxTextureBlendStages
    u(152, 4); // MaxSimultaneousTextures
    u(156, 1 | 2 | 8 | 0x10 | 0x20); // VertexProcessingCaps
    u(160, 8); // MaxActiveLights
    u(164, 6); // MaxUserClipPlanes
    u(168, 1); // MaxVertexBlendMatrices
    u(176, f(256.0)); // MaxPointSize
    u(180, 0xF_FFFF); // MaxPrimitiveCount
    u(184, 0xFF_FFFF); // MaxVertexIndex
    u(188, 16); // MaxStreams
    u(192, 508); // MaxStreamStride
    // Shaders: none yet (G5e).
    u(196, 0xFFFE_0000);
    u(204, 0xFFFF_0000);
    u(212, 1 | 0x40); // DevCaps2: STREAMOFFSET, VERTEXELEMENTSCANSHARESTREAMOFFSET
    u(232, 1); // NumberOfAdaptersInGroup
    u(236, 0x3FF); // DeclTypes
    u(240, 1); // NumSimultaneousRTs
    u(244, 0x100 | 0x200 | 0x100_0000 | 0x200_0000); // StretchRectFilterCaps
    unsafe { core::ptr::copy_nonoverlapping(c.as_ptr(), out, c.len()) };
}
