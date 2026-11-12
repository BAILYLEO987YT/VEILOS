// -----| 3D targets and textures for Win32 programs |-----
//
// VEIL_GPU3D(op, a, b, c): what opengl32.dll stands on (WIN32_DESIGN.md
// G5). Render targets (colour, depth and stencil buffers) and textures are kernel
// objects in the process's handle table; batches of window-space primitives
// are copied in and filled by raster3d.rs on every CPU (`lgpu::execute_3d`),
// and pixels are read back out. Kept in kernel memory because the CPUs that
// render can't see the calling process's memory.

use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use spin::Mutex;

use nt_abi::{status, NtStatus};

use crate::nt_file::ptr_ok;
use crate::nt_objects::KernelObjectRef;
use crate::process::Process;
use crate::raster3d::*;

/// A colour buffer (0x00RRGGBB, top row first), a depth buffer and an
/// 8-bit stencil buffer.
pub struct Target {
    pub w: usize,
    pub h: usize,
    pub color: Vec<u32>,
    pub depth: Vec<u32>,
    pub stencil: Vec<u8>,
}

impl Target {
    fn buffers(&mut self) -> Buffers {
        Buffers { color: self.color.as_mut_ptr(), depth: self.depth.as_mut_ptr(), stencil: self.stencil.as_mut_ptr(), w: self.w, h: self.h }
    }
}

pub enum Gpu3d {
    Target(Mutex<Target>),
    /// Replaced whole on update, so a draw holds the version it started with.
    Texture(Mutex<Arc<Texture>>),
}

fn obj(proc: &Process, h: u64) -> Option<Arc<Gpu3d>> {
    match crate::nt_process::object(proc, h) {
        Some(KernelObjectRef::Gpu3d(o)) => Some(o),
        _ => None,
    }
}

fn read<T>(p: u64) -> Option<T> {
    ptr_ok(p, core::mem::size_of::<T>()).then(|| unsafe { core::ptr::read_unaligned(p as *const T) })
}

fn new_handle(proc: &Process, o: Gpu3d, out: u64) -> NtStatus {
    if !ptr_ok(out, 8) {
        return status::ACCESS_VIOLATION;
    }
    let h = proc.handles.lock().allocate_ref(KernelObjectRef::Gpu3d(Arc::new(o)), 0x001F_0003, 0);
    unsafe { *(out as *mut u64) = h as u64 };
    status::SUCCESS
}

/// The biggest target or texture side accepted.
const MAX_SIDE: u32 = 8192;

/// VEIL_GPU3D(op, a, b, c): see nt_abi.
pub fn call(proc: &Process, op: u64, a: u64, b: u64, c: u64) -> NtStatus {
    match op {
        // Create a target: a = width, b = height, c = handle out.
        0 => {
            let (w, h) = (a as u32, b as u32);
            if w == 0 || h == 0 || w > MAX_SIDE || h > MAX_SIDE {
                return status::INVALID_PARAMETER;
            }
            let n = w as usize * h as usize;
            let t = Target { w: w as usize, h: h as usize, color: vec![0; n], depth: vec![DEPTH_MAX; n], stencil: vec![0; n] };
            new_handle(proc, Gpu3d::Target(Mutex::new(t)), c)
        }
        // Clear: a = target, b = ClearArgs*.
        1 => {
            let Some(o) = obj(proc, a) else { return status::INVALID_HANDLE };
            let Gpu3d::Target(t) = &*o else { return status::INVALID_HANDLE };
            let Some(args) = read::<ClearArgs>(b) else { return status::ACCESS_VIOLATION };
            clear(t.lock().buffers(), &args);
            status::SUCCESS
        }
        // Draw: a = target, b = DrawArgs*.
        2 => {
            let Some(o) = obj(proc, a) else { return status::INVALID_HANDLE };
            let Gpu3d::Target(t) = &*o else { return status::INVALID_HANDLE };
            let Some(args) = read::<DrawArgs>(b) else { return status::ACCESS_VIOLATION };
            let (gn, tn) = (args.group_count as usize, args.tri_count as usize);
            if gn > 1 << 20 || tn > 1 << 22
                || !ptr_ok(args.groups, gn * core::mem::size_of::<Group>())
                || !ptr_ok(args.tris, tn * core::mem::size_of::<Tri>())
            {
                return status::ACCESS_VIOLATION;
            }
            // Copied in: the bands run on CPUs that can't see this process.
            let groups: Vec<Group> = (0..gn).map(|i| unsafe { core::ptr::read_unaligned((args.groups as *const Group).add(i)) }).collect();
            let mut tris: Vec<Tri> = Vec::with_capacity(tn);
            unsafe {
                core::ptr::copy_nonoverlapping(args.tris as *const Tri, tris.as_mut_ptr(), tn);
                tris.set_len(tn);
            }
            let tex_of = |on: bool, h: u64| -> Option<Arc<Texture>> {
                if !on {
                    return None;
                }
                match obj(proc, h).as_deref() {
                    Some(Gpu3d::Texture(tx)) => Some(tx.lock().clone()),
                    _ => None,
                }
            };
            let texs: Vec<(Option<Arc<Texture>>, Option<Arc<Texture>>)> = groups
                .iter()
                .map(|g| (tex_of(g.flags & F_TEXTURE != 0, g.texture), tex_of(g.flags & F_TEXTURE2 != 0, g.texture2)))
                .collect();
            let passes: Vec<Pass> = groups.iter().zip(texs.iter()).map(|(g, (t1, t2))| {
                let mut g = *g;
                if t1.is_none() {
                    g.flags &= !F_TEXTURE;
                }
                if t2.is_none() {
                    g.flags &= !F_TEXTURE2;
                }
                Pass { g, tex: t1.as_deref(), tex2: t2.as_deref() }
            }).collect();
            let mut t = t.lock();
            crate::lgpu::execute_3d(t.buffers(), &tris, &passes);
            status::SUCCESS
        }
        // Read pixels: a = target, b = ReadArgs*.
        3 => {
            let Some(o) = obj(proc, a) else { return status::INVALID_HANDLE };
            let Gpu3d::Target(t) = &*o else { return status::INVALID_HANDLE };
            let Some(r) = read::<ReadArgs>(b) else { return status::ACCESS_VIOLATION };
            let t = t.lock();
            let x0 = r.x.max(0) as usize;
            let x1 = (r.x.max(0) as i64 + r.w.max(0) as i64).min(t.w as i64) as usize;
            if r.w <= 0 || r.h <= 0 || x0 >= x1 {
                return status::SUCCESS;
            }
            for row in 0..r.h as i64 {
                let y = r.y as i64 + row;
                if y < 0 || y >= t.h as i64 {
                    continue;
                }
                let dst = (r.dst as i64 + row * r.stride) as u64 + ((x0 as i64 - r.x as i64) * 4) as u64;
                if !ptr_ok(dst, (x1 - x0) * 4) {
                    return status::ACCESS_VIOLATION;
                }
                let src = &t.color[y as usize * t.w + x0..y as usize * t.w + x1];
                unsafe { core::ptr::copy_nonoverlapping(src.as_ptr(), dst as *mut u32, x1 - x0) };
            }
            status::SUCCESS
        }
        // Create (a = 0, c = handle out) or replace (a = texture) a texture's
        // contents: b = TexArgs*.
        4 => {
            let Some(ta) = read::<TexArgs>(b) else { return status::ACCESS_VIOLATION };
            if ta.w == 0 || ta.h == 0 || ta.w > MAX_SIDE || ta.h > MAX_SIDE || ta.levels > 16 {
                return status::INVALID_PARAMETER;
            }
            let n = texel_count(&ta);
            if !ptr_ok(ta.texels, n * 4) {
                return status::ACCESS_VIOLATION;
            }
            let mut texels = vec![0u32; n];
            unsafe { core::ptr::copy_nonoverlapping(ta.texels as *const u32, texels.as_mut_ptr(), n) };
            let tex = Arc::new(texture(&ta, texels));
            if a == 0 {
                new_handle(proc, Gpu3d::Texture(Mutex::new(tex)), c)
            } else {
                match obj(proc, a).as_deref() {
                    Some(Gpu3d::Texture(t)) => {
                        *t.lock() = tex;
                        status::SUCCESS
                    }
                    _ => status::INVALID_HANDLE,
                }
            }
        }
        // Write pixels straight in (5) or read depth or stencil out (6):
        // a = target, b = PixelArgs*.
        5 | 6 => {
            let Some(o) = obj(proc, a) else { return status::INVALID_HANDLE };
            let Gpu3d::Target(t) = &*o else { return status::INVALID_HANDLE };
            let Some(pa) = read::<PixelArgs>(b) else { return status::ACCESS_VIOLATION };
            if pa.w <= 0 || pa.h <= 0 {
                return status::SUCCESS;
            }
            if pa.w > MAX_SIDE as i32 || pa.h > MAX_SIDE as i32 {
                return status::INVALID_PARAMETER;
            }
            let row = pa.w as usize * 4;
            for r in 0..pa.h as i64 {
                if !ptr_ok((pa.ptr as i64 + r * pa.stride) as u64, row) {
                    return status::ACCESS_VIOLATION;
                }
            }
            let mut vals = vec![0u32; (pa.w * pa.h) as usize];
            let mut t = t.lock();
            let buf = t.buffers();
            if op == 5 {
                for r in 0..pa.h as usize {
                    let src = (pa.ptr as i64 + r as i64 * pa.stride) as *const u32;
                    unsafe { core::ptr::copy_nonoverlapping(src, vals.as_mut_ptr().add(r * pa.w as usize), pa.w as usize) };
                }
                write_pixels(buf, &pa, &vals);
            } else {
                read_pixels(buf, &pa, &mut vals);
                for r in 0..pa.h as usize {
                    let dst = (pa.ptr as i64 + r as i64 * pa.stride) as *mut u32;
                    unsafe { core::ptr::copy_nonoverlapping(vals.as_ptr().add(r * pa.w as usize), dst, pa.w as usize) };
                }
            }
            status::SUCCESS
        }
        _ => status::INVALID_PARAMETER,
    }
}
