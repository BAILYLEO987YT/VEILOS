//! Pixel formats and SwapBuffers: gdi32's side of OpenGL (WIN32_DESIGN.md
//! G5). Two formats, both drawn by opengl32.dll's software rasterizer:
//!
//!   1  a window, double-buffered (SwapBuffers shows the frame)
//!   2  a window or a bitmap (a DIB section in a memory DC), single-buffered
//!
//! 32-bit colour with no alpha bits, a 24-bit depth buffer, an 8-bit stencil
//! buffer. SwapBuffers is
//! opengl32's: it hands it over with `VeilGdiSetSwapHook` when loaded.

use crate::{Handle, Obj, Target};
use crate::dllrt::Lock;
use alloc::vec::Vec;

const PFD_DOUBLEBUFFER: u32 = 1;
const PFD_DRAW_TO_WINDOW: u32 = 4;
const PFD_DRAW_TO_BITMAP: u32 = 8;
const PFD_SUPPORT_GDI: u32 = 0x10;
const PFD_SUPPORT_OPENGL: u32 = 0x20;
const PFD_GENERIC_FORMAT: u32 = 0x40;

const FORMATS: [u32; 2] = [
    PFD_DRAW_TO_WINDOW | PFD_SUPPORT_OPENGL | PFD_DOUBLEBUFFER | PFD_GENERIC_FORMAT,
    PFD_DRAW_TO_WINDOW | PFD_DRAW_TO_BITMAP | PFD_SUPPORT_GDI | PFD_SUPPORT_OPENGL | PFD_GENERIC_FORMAT,
];

/// The format set on each DC.
static SET: Lock<Vec<(Handle, i32)>> = Lock::new(Vec::new());
static SWAP: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

/// Fill a PIXELFORMATDESCRIPTOR (40 bytes).
fn describe(n: i32, out: *mut u8) {
    let mut d = [0u8; 40];
    d[0] = 40;
    d[2] = 1;
    d[4..8].copy_from_slice(&FORMATS[n as usize - 1].to_le_bytes());
    d[9] = 32; // cColorBits
    (d[10], d[11], d[12], d[13], d[14], d[15]) = (8, 16, 8, 8, 8, 0);
    d[23] = 24; // cDepthBits
    d[24] = 8; // cStencilBits
    unsafe { core::ptr::copy_nonoverlapping(d.as_ptr(), out, 40) };
}

#[no_mangle]
pub extern "system" fn ChoosePixelFormat(_hdc: Handle, pfd: *const u8) -> i32 {
    if pfd.is_null() {
        return 0;
    }
    let flags = unsafe { core::ptr::read_unaligned(pfd.add(4) as *const u32) };
    if flags & PFD_DRAW_TO_BITMAP != 0 || flags & PFD_DOUBLEBUFFER == 0 { 2 } else { 1 }
}

#[no_mangle]
pub extern "system" fn DescribePixelFormat(_hdc: Handle, n: i32, size: u32, out: *mut u8) -> i32 {
    // With no descriptor to fill, it just answers how many there are.
    if !out.is_null() {
        if n < 1 || n > FORMATS.len() as i32 {
            return 0;
        }
        if size >= 40 {
            describe(n, out);
        }
    }
    FORMATS.len() as i32
}

#[no_mangle]
pub extern "system" fn SetPixelFormat(hdc: Handle, n: i32, _pfd: *const u8) -> i32 {
    if n < 1 || n > FORMATS.len() as i32 || crate::table().dc(hdc).is_none() {
        return 0;
    }
    let mut s = SET.lock();
    s.retain(|e| e.0 != hdc);
    s.push((hdc, n));
    1
}

#[no_mangle]
pub extern "system" fn GetPixelFormat(hdc: Handle) -> i32 {
    SET.lock().iter().find(|e| e.0 == hdc).map_or(0, |e| e.1)
}

#[no_mangle]
pub extern "system" fn SwapBuffers(hdc: Handle) -> i32 {
    let f = SWAP.load(core::sync::atomic::Ordering::Acquire);
    if f == 0 {
        return 0;
    }
    let f: extern "system" fn(Handle) -> i32 = unsafe { core::mem::transmute(f) };
    f(hdc)
}

/// opengl32.dll's SwapBuffers.
#[no_mangle]
pub extern "system" fn VeilGdiSetSwapHook(f: usize) {
    SWAP.store(f, core::sync::atomic::Ordering::Release);
}

/// The size of what `hdc` draws on (a window's client area, or the memory
/// DC's bitmap) into `out`; returns 1 for a window, 2 for a memory DC, 0 if
/// `hdc` isn't a DC.
#[no_mangle]
pub extern "system" fn VeilGdiDcSize(hdc: Handle, out: *mut [i32; 2]) -> i32 {
    let mut t = crate::table();
    let Some(dc) = t.dc(hdc) else { return 0 };
    let (bx0, by0, bx1, by1) = dc.bounds;
    let r = match dc.target {
        Target::Surface { w, h, .. } => ([bx1.min(w) - bx0.max(0), by1.min(h) - by0.max(0)], 1),
        Target::Bitmap(b) => match t.get(b) {
            Some(Obj::Bitmap(bm)) => ([bm.w, bm.h.abs()], 2),
            _ => return 0,
        },
    };
    if !out.is_null() {
        unsafe { *out = [r.0[0].max(0), r.0[1].max(0)] };
    }
    r.1
}
