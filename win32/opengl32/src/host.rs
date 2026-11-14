//! `--features host`: this opengl32.dll built to run on Windows, next to a
//! test program (an app-local opengl32.dll wins over System32's), so its
//! pixels can be compared with Windows' own software OpenGL in seconds
//! instead of a VM boot. VEIL_GPU3D is answered in-process by gfx3d's
//! host build; the gdi32 calls VEILOS adds are made here from Windows' gdi32.
//! Windows' gdi32 asks opengl32 for pixel formats, so those live here too.

use crate::dllrt::Lock;
use alloc::vec::Vec;

#[link(name = "gdi32", kind = "raw-dylib")]
unsafe extern "system" {
    fn GetCurrentObject(hdc: u64, kind: u32) -> u64;
    fn GetObjectW(h: u64, size: i32, out: *mut u8) -> i32;
    fn GetObjectType(h: u64) -> u32;
}

#[link(name = "user32", kind = "raw-dylib")]
unsafe extern "system" {
    fn WindowFromDC(hdc: u64) -> u64;
    fn GetClientRect(hwnd: u64, r: *mut [i32; 4]) -> i32;
}

pub unsafe extern "system" fn VeilGdiDcSize(hdc: u64, out: *mut [i32; 2]) -> i32 {
    unsafe {
        if GetObjectType(hdc) == 10 {
            let mut bm = [0u8; 32];
            if GetObjectW(GetCurrentObject(hdc, 7), 32, bm.as_mut_ptr()) == 0 {
                return 0;
            }
            let w = i32::from_le_bytes(bm[4..8].try_into().unwrap());
            let h = i32::from_le_bytes(bm[8..12].try_into().unwrap());
            *out = [w, h.abs()];
            return 2;
        }
        let mut r = [0i32; 4];
        GetClientRect(WindowFromDC(hdc), &mut r);
        *out = [r[2], r[3]];
        1
    }
}

pub unsafe extern "system" fn VeilGdiSetSwapHook(_f: usize) {}

// Pixel formats: the same two as VEILOS's gdi32 (win32/gdi32/src/pixfmt.rs).

const FORMATS: [u32; 2] = [0x1 | 0x4 | 0x20 | 0x40, 0x4 | 0x8 | 0x10 | 0x20 | 0x40];
static SET: Lock<Vec<(u64, i32)>> = Lock::new(Vec::new());

pub unsafe extern "system" fn ChoosePixelFormat(_hdc: u64, pfd: *const u8) -> i32 {
    let flags = unsafe { core::ptr::read_unaligned(pfd.add(4) as *const u32) };
    if flags & 8 != 0 || flags & 1 == 0 { 2 } else { 1 }
}

pub unsafe extern "system" fn DescribePixelFormat(_hdc: u64, n: i32, size: u32, out: *mut u8) -> i32 {
    if !out.is_null() {
        if n < 1 || n > 2 {
            return 0;
        }
        if size >= 40 {
            let mut d = [0u8; 40];
            d[0] = 40;
            d[2] = 1;
            d[4..8].copy_from_slice(&FORMATS[n as usize - 1].to_le_bytes());
            d[9] = 32;
            (d[10], d[11], d[12], d[13], d[14], d[15]) = (8, 16, 8, 8, 8, 0);
            d[23] = 24;
            d[24] = 8;
            (d[18], d[19], d[20], d[21], d[22]) = (64, 16, 16, 16, 16);
            unsafe { core::ptr::copy_nonoverlapping(d.as_ptr(), out, 40) };
        }
    }
    2
}

pub unsafe extern "system" fn SetPixelFormat(hdc: u64, n: i32, _pfd: *const u8) -> i32 {
    let mut s = SET.lock();
    s.retain(|e| e.0 != hdc);
    s.push((hdc, n));
    1
}

pub unsafe extern "system" fn GetPixelFormat(hdc: u64) -> i32 {
    SET.lock().iter().find(|e| e.0 == hdc).map_or(0, |e| e.1)
}
