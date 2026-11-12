//! What VEILOS's 3D DLLs (opengl32, d3d9) share (WIN32_DESIGN.md G5): the
//! kernel rasterizer's ABI, from the kernel's own source (src/raster3d.rs,
//! included by path, so the two can't drift apart), and VEIL_GPU3D.
//!
//! With `--features host` the DLLs run on Windows, next to its own OpenGL or
//! Direct3D, and VEIL_GPU3D is answered in-process by that same rasterizer
//! (host.rs), so their pixels can be compared without booting VEILOS.

#![no_std]
#![allow(non_snake_case)]

extern crate alloc;

#[path = "../../../src/raster3d.rs"]
#[allow(dead_code)]
pub mod raster3d;

#[cfg(feature = "host")]
mod host;

#[cfg(not(feature = "host"))]
#[link(name = "ntdll", kind = "raw-dylib")]
unsafe extern "system" {
    fn VeilGpu3d(op: u64, a: u64, b: u64, c: u64) -> i32;
    fn NtClose(h: u64) -> i32;
}

#[cfg(feature = "host")]
use host::{NtClose, VeilGpu3d};

/// VEIL_GPU3D(op, a, b, c): see nt_abi.
pub fn gpu3d(op: u64, a: u64, b: u64, c: u64) -> i32 {
    unsafe { VeilGpu3d(op, a, b, c) }
}

/// Close a target or texture handle.
pub fn close(h: u64) {
    unsafe { NtClose(h) };
}

/// Create a target `w` x `h`; its handle, or 0.
pub fn create_target(w: u32, h: u32) -> u64 {
    let mut out = 0u64;
    if gpu3d(0, w as u64, h as u64, &mut out as *mut u64 as u64) != 0 {
        return 0;
    }
    out
}

/// Create (`handle` 0) or replace a texture; its handle, or 0.
pub fn upload_texture(handle: u64, args: &raster3d::TexArgs) -> u64 {
    let mut out = handle;
    if gpu3d(4, handle, args as *const _ as u64, &mut out as *mut u64 as u64) != 0 {
        return 0;
    }
    out
}
