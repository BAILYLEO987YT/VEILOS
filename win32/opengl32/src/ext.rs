//! Extensions, reached through wglGetProcAddress (WIN32_DESIGN.md G5c):
//! what GL 1.1 games ask for. GL_ARB_multitexture (two units),
//! GL_EXT_texture_env_add and GL_ARB_texture_env_add, the edge clamp
//! (GL_EXT_/GL_SGIS_texture_edge_clamp), GL_EXT_bgra,
//! GL_EXT_compiled_vertex_array, GL_EXT_draw_range_elements, the GL 1.0
//! extensions GL 1.1 took in (GL_EXT_vertex_array, GL_EXT_texture_object,
//! GL_EXT_polygon_offset), GL_WIN_swap_hint, and WGL_EXT_swap_control with
//! the WGL extension strings.

use alloc::vec::Vec;

use crate::more::glPolygonOffset;
use crate::*;

pub(crate) const EXTENSIONS: &[u8] = b"GL_ARB_multitexture GL_ARB_texture_env_add GL_EXT_bgra GL_EXT_compiled_vertex_array \
GL_EXT_draw_range_elements GL_EXT_polygon_offset GL_EXT_texture_edge_clamp GL_EXT_texture_env_add GL_EXT_texture_object \
GL_EXT_vertex_array GL_SGIS_texture_edge_clamp GL_WIN_swap_hint\0";

const WGL_EXTENSIONS: &[u8] = b"WGL_ARB_extensions_string WGL_EXT_extensions_string WGL_EXT_swap_control\0";

const GL_TEXTURE0: u32 = 0x84C0;

// -----| multitexture |-----

#[no_mangle]
pub extern "system" fn glActiveTextureARB(unit: u32) {
    rec!(glActiveTextureARB(unit));
    outside(|c| if (GL_TEXTURE0..GL_TEXTURE0 + 2).contains(&unit) { c.unit = (unit - GL_TEXTURE0) as usize } else { c.err(GL_INVALID_ENUM) })
}

#[no_mangle]
pub extern "system" fn glClientActiveTextureARB(unit: u32) {
    with(|c| if (GL_TEXTURE0..GL_TEXTURE0 + 2).contains(&unit) { c.client_unit = (unit - GL_TEXTURE0) as usize } else { c.err(GL_INVALID_ENUM) })
}

fn multi_tc(unit: u32, s: f32, t: f32, r: f32, q: f32) {
    rec!(multi_tc(unit, s, t, r, q));
    with(|c| if (GL_TEXTURE0..GL_TEXTURE0 + 2).contains(&unit) { c.tc[(unit - GL_TEXTURE0) as usize] = [s, t, r, q] } else { c.err(GL_INVALID_ENUM) })
}

macro_rules! multi {
    ($($name:ident($($p:ident),*): $t:ty;)*) => {
        $(pub extern "system" fn $name(u: u32, $($p: $t),*) {
            let v = [$($p as f32),*];
            let g = |i: usize| if i < v.len() { v[i] } else if i == 3 { 1.0 } else { 0.0 };
            multi_tc(u, g(0), g(1), g(2), g(3))
        })*
    };
}
macro_rules! multi_v {
    ($($name:ident($t:ty, $n:expr);)*) => {
        $(pub unsafe extern "system" fn $name(u: u32, v: *const $t) {
            let g = |i: usize| if i < $n { (unsafe { *v.add(i) }) as f32 } else if i == 3 { 1.0 } else { 0.0 };
            multi_tc(u, g(0), g(1), g(2), g(3))
        })*
    };
}
multi! {
    glMultiTexCoord1sARB(s): i16; glMultiTexCoord1iARB(s): i32; glMultiTexCoord1fARB(s): f32; glMultiTexCoord1dARB(s): f64;
    glMultiTexCoord2sARB(s, t): i16; glMultiTexCoord2iARB(s, t): i32; glMultiTexCoord2fARB(s, t): f32; glMultiTexCoord2dARB(s, t): f64;
    glMultiTexCoord3sARB(s, t, r): i16; glMultiTexCoord3iARB(s, t, r): i32; glMultiTexCoord3fARB(s, t, r): f32; glMultiTexCoord3dARB(s, t, r): f64;
    glMultiTexCoord4sARB(s, t, r, q): i16; glMultiTexCoord4iARB(s, t, r, q): i32; glMultiTexCoord4fARB(s, t, r, q): f32; glMultiTexCoord4dARB(s, t, r, q): f64;
}
multi_v! {
    glMultiTexCoord1svARB(i16, 1); glMultiTexCoord1ivARB(i32, 1); glMultiTexCoord1fvARB(f32, 1); glMultiTexCoord1dvARB(f64, 1);
    glMultiTexCoord2svARB(i16, 2); glMultiTexCoord2ivARB(i32, 2); glMultiTexCoord2fvARB(f32, 2); glMultiTexCoord2dvARB(f64, 2);
    glMultiTexCoord3svARB(i16, 3); glMultiTexCoord3ivARB(i32, 3); glMultiTexCoord3fvARB(f32, 3); glMultiTexCoord3dvARB(f64, 3);
    glMultiTexCoord4svARB(i16, 4); glMultiTexCoord4ivARB(i32, 4); glMultiTexCoord4fvARB(f32, 4); glMultiTexCoord4dvARB(f64, 4);
}

// -----| arrays |-----

/// GL_EXT_compiled_vertex_array: arrays are read when drawn anyway.
pub extern "system" fn glLockArraysEXT(_first: i32, _count: i32) {}
pub extern "system" fn glUnlockArraysEXT() {}

pub unsafe extern "system" fn glDrawRangeElementsEXT(mode: u32, _start: u32, _end: u32, count: i32, kind: u32, indices: usize) {
    unsafe { glDrawElements(mode, count, kind, indices) }
}

pub extern "system" fn glVertexPointerEXT(size: i32, kind: u32, stride: i32, _count: i32, p: usize) { glVertexPointer(size, kind, stride, p) }
pub extern "system" fn glColorPointerEXT(size: i32, kind: u32, stride: i32, _count: i32, p: usize) { glColorPointer(size, kind, stride, p) }
pub extern "system" fn glTexCoordPointerEXT(size: i32, kind: u32, stride: i32, _count: i32, p: usize) { glTexCoordPointer(size, kind, stride, p) }
pub extern "system" fn glNormalPointerEXT(kind: u32, stride: i32, _count: i32, p: usize) { glNormalPointer(kind, stride, p) }
pub extern "system" fn glIndexPointerEXT(_kind: u32, _stride: i32, _count: i32, _p: usize) {}
pub extern "system" fn glEdgeFlagPointerEXT(_stride: i32, _count: i32, _p: usize) {}

// -----| others |-----

pub extern "system" fn glPolygonOffsetEXT(factor: f32, bias: f32) {
    // The EXT's bias is in depth units, GL 1.1's units in depth steps.
    glPolygonOffset(factor, bias * 16777215.0)
}

/// GL_WIN_swap_hint: the whole frame is shown anyway.
pub extern "system" fn glAddSwapHintRectWIN(_x: i32, _y: i32, _w: i32, _h: i32) {}

pub extern "system" fn wglSwapIntervalEXT(i: i32) -> Bool {
    with(|c| {
        if i < 0 {
            unsafe { SetLastError(87) };
            return 0;
        }
        c.swap_interval = i;
        1
    })
}

pub extern "system" fn wglGetSwapIntervalEXT() -> i32 {
    with(|c| c.swap_interval)
}

pub extern "system" fn wglGetExtensionsStringARB(_hdc: u64) -> *const u8 {
    WGL_EXTENSIONS.as_ptr()
}

pub extern "system" fn wglGetExtensionsStringEXT() -> *const u8 {
    WGL_EXTENSIONS.as_ptr()
}

// -----| wglGetProcAddress |-----

macro_rules! table {
    ($($f:ident),* $(,)?) => {
        alloc::vec![$((stringify!($f), $f as *const () as usize)),*]
    };
}

fn table() -> Vec<(&'static str, usize)> {
    table!(
        glActiveTextureARB, glClientActiveTextureARB,
        glMultiTexCoord1sARB, glMultiTexCoord1iARB, glMultiTexCoord1fARB, glMultiTexCoord1dARB,
        glMultiTexCoord2sARB, glMultiTexCoord2iARB, glMultiTexCoord2fARB, glMultiTexCoord2dARB,
        glMultiTexCoord3sARB, glMultiTexCoord3iARB, glMultiTexCoord3fARB, glMultiTexCoord3dARB,
        glMultiTexCoord4sARB, glMultiTexCoord4iARB, glMultiTexCoord4fARB, glMultiTexCoord4dARB,
        glMultiTexCoord1svARB, glMultiTexCoord1ivARB, glMultiTexCoord1fvARB, glMultiTexCoord1dvARB,
        glMultiTexCoord2svARB, glMultiTexCoord2ivARB, glMultiTexCoord2fvARB, glMultiTexCoord2dvARB,
        glMultiTexCoord3svARB, glMultiTexCoord3ivARB, glMultiTexCoord3fvARB, glMultiTexCoord3dvARB,
        glMultiTexCoord4svARB, glMultiTexCoord4ivARB, glMultiTexCoord4fvARB, glMultiTexCoord4dvARB,
        glLockArraysEXT, glUnlockArraysEXT, glDrawRangeElementsEXT,
        glVertexPointerEXT, glColorPointerEXT, glTexCoordPointerEXT, glNormalPointerEXT, glIndexPointerEXT, glEdgeFlagPointerEXT,
        glPolygonOffsetEXT, glAddSwapHintRectWIN,
        wglSwapIntervalEXT, wglGetSwapIntervalEXT, wglGetExtensionsStringARB, wglGetExtensionsStringEXT,
    )
}

/// GL 1.1 calls under their EXT names: the same functions.
fn aliases() -> Vec<(&'static str, usize)> {
    alloc::vec![
        ("glArrayElementEXT", glArrayElement as *const () as usize),
        ("glDrawArraysEXT", glDrawArrays as *const () as usize),
        ("glGetPointervEXT", more::glGetPointerv as *const () as usize),
        ("glGenTexturesEXT", glGenTextures as *const () as usize),
        ("glBindTextureEXT", glBindTexture as *const () as usize),
        ("glDeleteTexturesEXT", glDeleteTextures as *const () as usize),
        ("glIsTextureEXT", glIsTexture as *const () as usize),
        ("glAreTexturesResidentEXT", more::glAreTexturesResident as *const () as usize),
        ("glPrioritizeTexturesEXT", more::glPrioritizeTextures as *const () as usize),
        ("glCopyTexImage1DEXT", more::glCopyTexImage1D as *const () as usize),
        ("glCopyTexImage2DEXT", more::glCopyTexImage2D as *const () as usize),
        ("glCopyTexSubImage1DEXT", more::glCopyTexSubImage1D as *const () as usize),
        ("glCopyTexSubImage2DEXT", more::glCopyTexSubImage2D as *const () as usize),
        ("glTexSubImage1DEXT", more::glTexSubImage1D as *const () as usize),
        ("glTexSubImage2DEXT", glTexSubImage2D as *const () as usize),
        ("glActiveTexture", glActiveTextureARB as *const () as usize),
        ("glClientActiveTexture", glClientActiveTextureARB as *const () as usize),
        ("glMultiTexCoord2f", glMultiTexCoord2fARB as *const () as usize),
        ("glMultiTexCoord2fv", glMultiTexCoord2fvARB as *const () as usize),
    ]
}

#[no_mangle]
pub extern "system" fn wglGetProcAddress(name: *const u8) -> usize {
    if name.is_null() {
        return 0;
    }
    let mut n = 0;
    while n < 256 && unsafe { *name.add(n) } != 0 {
        n += 1;
    }
    let s = unsafe { core::slice::from_raw_parts(name, n) };
    // Only for a context that's current, as on Windows.
    if current().is_none() {
        return 0;
    }
    table().into_iter().chain(aliases()).find(|(k, _)| k.as_bytes() == s).map_or(0, |e| e.1)
}
