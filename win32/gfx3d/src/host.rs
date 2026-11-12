//! VEIL_GPU3D answered in-process on Windows (`--features host`), by the
//! kernel's own rasterizer source, split into bands as lgpu does.

use crate::raster3d::*;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, Ordering};

struct Lock<T> {
    held: AtomicBool,
    v: UnsafeCell<T>,
}

unsafe impl<T> Sync for Lock<T> {}

struct Guard<'a, T>(&'a Lock<T>);

impl<T> Lock<T> {
    const fn new(v: T) -> Self {
        Lock { held: AtomicBool::new(false), v: UnsafeCell::new(v) }
    }
    fn lock(&self) -> Guard<'_, T> {
        while self.held.compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() {
            core::hint::spin_loop();
        }
        Guard(self)
    }
}

impl<T> core::ops::Deref for Guard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        unsafe { &*self.0.v.get() }
    }
}

impl<T> core::ops::DerefMut for Guard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut *self.0.v.get() }
    }
}

impl<T> Drop for Guard<'_, T> {
    fn drop(&mut self) {
        self.0.held.store(false, Ordering::Release);
    }
}

enum Obj {
    Target { w: usize, h: usize, color: Vec<u32>, depth: Vec<u32>, stencil: Vec<u8> },
    Texture(Arc<Texture>),
}

static OBJS: Lock<Vec<Option<Obj>>> = Lock::new(Vec::new());

/// Rows per band: small, so band edges are exercised as on many CPUs.
const BAND: i32 = 7;
// (Set to the target height to compare a serial render.)

fn add(l: &mut Vec<Option<Obj>>, o: Obj, out: u64) -> i32 {
    l.push(Some(o));
    unsafe { *(out as *mut u64) = l.len() as u64 };
    0
}

pub unsafe extern "system" fn VeilGpu3d(op: u64, a: u64, b: u64, c: u64) -> i32 {
    let mut l = OBJS.lock();
    match op {
        0 => {
            let n = (a * b) as usize;
            add(&mut l, Obj::Target { w: a as usize, h: b as usize, color: vec![0; n], depth: vec![DEPTH_MAX; n], stencil: vec![0; n] }, c)
        }
        4 => {
            let ta = unsafe { &*(b as *const TexArgs) };
            let texels = unsafe { core::slice::from_raw_parts(ta.texels as *const u32, texel_count(ta)) }.to_vec();
            let t = Arc::new(texture(ta, texels));
            if a == 0 {
                add(&mut l, Obj::Texture(t), c)
            } else {
                l[a as usize - 1] = Some(Obj::Texture(t));
                0
            }
        }
        1 | 2 | 3 | 5 | 6 => {
            // A draw's textures first (shared), then the target (mutable).
            let texs: Vec<[Option<Arc<Texture>>; SLOTS]> = if op == 2 {
                let d = unsafe { &*(b as *const DrawArgs) };
                let groups = unsafe { core::slice::from_raw_parts(d.groups as *const Group, d.group_count as usize) };
                groups
                    .iter()
                    .map(|g| {
                        core::array::from_fn(|i| match l.get(g.samplers[i].texture.wrapping_sub(1) as usize) {
                            Some(Some(Obj::Texture(t))) => Some(t.clone()),
                            _ => None,
                        })
                    })
                    .collect()
            } else {
                Vec::new()
            };
            // Ops 1 and 2: another target's depth and stencil buffers (c).
            let lend = if (op == 1 || op == 2) && c != 0 && c != a {
                match l.get_mut((c as usize).wrapping_sub(1)) {
                    Some(Some(Obj::Target { w, h, depth, stencil, .. })) => Some((depth.as_mut_ptr(), stencil.as_mut_ptr(), *w, *h)),
                    _ => return -1,
                }
            } else {
                None
            };
            let Some(Some(Obj::Target { w, h, color, depth, stencil })) = l.get_mut((a as usize).wrapping_sub(1)) else { return -1 };
            let mut buf = Buffers { color: color.as_mut_ptr(), depth: depth.as_mut_ptr(), stencil: stencil.as_mut_ptr(), w: *w, h: *h, dw: *w, dh: *h };
            if let Some((d, s, dw, dh)) = lend {
                (buf.depth, buf.stencil, buf.dw, buf.dh) = (d, s, dw, dh);
            }
            match op {
                1 => clear(buf, unsafe { &*(b as *const ClearArgs) }),
                2 => {
                    let d = unsafe { &*(b as *const DrawArgs) };
                    let groups = unsafe { core::slice::from_raw_parts(d.groups as *const Group, d.group_count as usize) };
                    let tris = unsafe { core::slice::from_raw_parts(d.tris as *const Tri, d.tri_count as usize) };
                    let passes: Vec<Pass> = groups
                        .iter()
                        .zip(texs.iter())
                        .map(|(g, ts)| Pass { g: *g, tex: core::array::from_fn(|i| ts[i].as_deref()) })
                        .collect();
                    let mut y = 0;
                    while y < *h as i32 {
                        render_band(buf, tris, &passes, y, (y + BAND).min(*h as i32));
                        y += BAND;
                    }
                }
                3 => {
                    let r = unsafe { &*(b as *const ReadArgs) };
                    for row in 0..r.h {
                        let yy = r.y + row;
                        if yy < 0 || yy >= *h as i32 {
                            continue;
                        }
                        for x in 0..r.w {
                            let xx = r.x + x;
                            if xx >= 0 && xx < *w as i32 {
                                unsafe { *((r.dst as i64 + row as i64 * r.stride) as *mut u32).add(x as usize) = color[yy as usize * *w + xx as usize] };
                            }
                        }
                    }
                }
                _ => {
                    let pa = unsafe { &*(b as *const PixelArgs) };
                    if pa.w <= 0 || pa.h <= 0 {
                        return 0;
                    }
                    let mut vals = vec![0u32; (pa.w * pa.h) as usize];
                    if op == 5 {
                        for r in 0..pa.h as usize {
                            let p = (pa.ptr as i64 + r as i64 * pa.stride) as *const u32;
                            unsafe { core::ptr::copy_nonoverlapping(p, vals.as_mut_ptr().add(r * pa.w as usize), pa.w as usize) };
                        }
                        write_pixels(buf, pa, &vals);
                    } else {
                        read_pixels(buf, pa, &mut vals);
                        for r in 0..pa.h as usize {
                            let p = (pa.ptr as i64 + r as i64 * pa.stride) as *mut u32;
                            unsafe { core::ptr::copy_nonoverlapping(vals.as_ptr().add(r * pa.w as usize), p, pa.w as usize) };
                        }
                    }
                }
            }
            0
        }
        _ => -1,
    }
}

pub unsafe extern "system" fn NtClose(h: u64) -> i32 {
    if let Some(s) = OBJS.lock().get_mut(h.wrapping_sub(1) as usize) {
        *s = None;
    }
    0
}
