//! Compute shaders (feature level 11_0): Dispatch runs the shader's thread
//! groups on the interpreter (vm.rs), spread over a few threads of this
//! DLL. A group's threads run one after another up to each group sync,
//! sharing its group shared memory; shader resource and unordered access
//! views are the resources' own bytes (atomics on them are real atomics,
//! groups running at once).

use alloc::boxed::Box;
use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, Ordering};

use crate::com::*;
use crate::context::STAGE_CS;
use crate::device::Device;
use crate::dxbc::{Ins, T_RESOURCE, T_UAV};
use crate::format::*;
use crate::res::{resource, view, Resource, View, DIM_BUFFER, DIM_TEXTURE1D, DIM_TEXTURE3D};
use crate::vm::{Env, Mem, Regs, Vm, R4};

#[link(name = "kernel32", kind = "raw-dylib")]
unsafe extern "system" {
    fn CreateThread(attrs: usize, stack: usize, start: unsafe extern "system" fn(*mut u8) -> u32, param: *mut u8, flags: u32, id: *mut u32) -> usize;
    fn WaitForSingleObject(h: usize, ms: u32) -> u32;
    fn CloseHandle(h: usize) -> i32;
    fn GetSystemInfo(info: *mut [u8; 64]);
}

/// The memory a view gives a shader.
fn view_mem(v: &View) -> Option<Mem> {
    let r = resource(v.res);
    let base = r.data.as_mut_ptr();
    if r.dim == DIM_BUFFER {
        // First element, count, flags (raw: 1).
        let d = &v.desc;
        let raw = d[1] == 11 && d[4] & 1 != 0 || (v.kind == crate::res::VIEW_UAV && d[4] & 1 != 0);
        let (size, format, stride) = if raw {
            (4usize, 0, 0)
        } else if v.format == 0 {
            (r.stride.max(1) as usize, 0, r.stride)
        } else {
            (bytes(v.format)? as usize, v.format, 0)
        };
        let first = d[2] as usize * size;
        let len = (d[3] as usize * size).min(r.data.len().saturating_sub(first));
        return Some(Mem { ptr: unsafe { base.add(first.min(r.data.len())) }, len, stride, format, w: d[3], h: 1, d: 1, row: 0, slice: 0 });
    }
    // A texture: the view's level, from its first slice.
    let mips = r.mips as usize;
    let s = v.mip as usize + v.first as usize * mips;
    let sub = *r.subs.get(s)?;
    let layer = if r.subs.len() > mips { r.subs[mips].at - r.subs[0].at } else { 0 };
    let (h, d, row, slice) = match r.dim {
        DIM_TEXTURE1D => (v.slices, 1, layer, 0),
        DIM_TEXTURE3D => (sub.h, sub.d, sub.pitch as usize, sub.slice as usize),
        _ => (sub.h, v.slices, sub.pitch as usize, layer),
    };
    let len = r.data.len().saturating_sub(sub.at);
    Some(Mem { ptr: unsafe { base.add(sub.at) }, len, stride: 0, format: v.format, w: sub.w, h, d, row, slice })
}

struct CsEnv {
    cbs: [(*const u8, usize); 14],
    t: Vec<Option<Mem>>,
    u: Vec<Option<Mem>>,
}

unsafe impl Sync for CsEnv {}

impl Env for CsEnv {
    fn cb(&self, slot: u32, reg: i64) -> R4 {
        let Some(&(p, len)) = self.cbs.get(slot as usize) else { return [0; 4] };
        if reg < 0 || p.is_null() || (reg as usize + 1) * 16 > len {
            return [0; 4];
        }
        let s = unsafe { core::slice::from_raw_parts(p.add(reg as usize * 16), 16) };
        core::array::from_fn(|k| u32::from_le_bytes(s[k * 4..k * 4 + 4].try_into().unwrap()))
    }
    fn fetch(&self, _ins: &Ins, _c: R4, _x: [R4; 2]) -> R4 {
        // Filtered sampling in compute shaders: not yet.
        [0; 4]
    }
    fn mem(&self, ty: u8, reg: u32) -> Option<Mem> {
        match ty {
            T_RESOURCE => *self.t.get(reg as usize)?,
            T_UAV => *self.u.get(reg as usize)?,
            _ => None,
        }
    }
}

struct Job<'a> {
    vm: &'a Vm<'a>,
    env: &'a CsEnv,
    groups: [u32; 3],
    next: AtomicU32,
}

impl Job<'_> {
    fn work(&self) {
        let [gx, gy, gz] = self.groups;
        let total = gx * gy * gz;
        loop {
            let i = self.next.fetch_add(1, Ordering::Relaxed);
            if i >= total {
                return;
            }
            self.group([i % gx, (i / gx) % gy, i / (gx * gy)]);
        }
    }

    /// One thread group: its threads in turn, each up to the next sync.
    fn group(&self, gid: [u32; 3]) {
        let sh = self.vm.sh;
        let [tx, ty, tz] = sh.group;
        let n = (tx * ty * tz) as usize;
        let mut tgsm: Vec<Vec<u8>> = sh.tgsm.iter().map(|&(_, bytes, _)| vec![0u8; bytes as usize]).collect();
        let mut regs: Vec<Regs> = (0..n)
            .map(|i| {
                let mut r = self.vm.regs(0, 0);
                let l = [i as u32 % tx, (i as u32 / tx) % ty, i as u32 / (tx * ty)];
                r.ids = [
                    [gid[0] * tx + l[0], gid[1] * ty + l[1], gid[2] * tz + l[2], 0],
                    [gid[0], gid[1], gid[2], 0],
                    [l[0], l[1], l[2], 0],
                    [i as u32; 4],
                ];
                r
            })
            .collect();
        let tp: *mut Vec<Vec<u8>> = &mut tgsm;
        for r in regs.iter_mut() {
            r.tgsm = tp;
        }
        let mut pcs: Vec<Option<usize>> = vec![Some(0); n];
        while pcs.iter().any(|p| p.is_some()) {
            for i in 0..n {
                if let Some(p) = pcs[i] {
                    pcs[i] = self.vm.run_from(&mut regs[i], self.env, p);
                }
            }
        }
    }
}

unsafe extern "system" fn worker(p: *mut u8) -> u32 {
    let job = unsafe { &*(p as *const Job) };
    job.work();
    0
}

fn cpus() -> u32 {
    let mut info = [0u8; 64];
    unsafe { GetSystemInfo(&mut info) };
    u32::from_le_bytes(info[32..36].try_into().unwrap()).clamp(1, 16)
}

/// Bring every subresource drawn into back into the bytes.
fn synced(r: &mut Resource) {
    for s in 0..r.subs.len() {
        r.sync(s);
    }
}

fn dispatch(d: &mut Device, groups: [u32; 3]) {
    let Some(cs) = crate::shader::of(d.state.stages[STAGE_CS].shader) else { return };
    if cs.sh.kind != 5 || groups.iter().any(|&g| g == 0) || groups.iter().any(|&g| g > 65535) {
        return;
    }
    let Some(vm) = Vm::new(&cs.sh) else { return };
    // The views: their resources' bytes up to date.
    let st = &d.state.stages[STAGE_CS];
    let srvs: Vec<*mut Com> = st.srvs.to_vec();
    let uavs: Vec<*mut Com> = d.state.cs_uavs.to_vec();
    for &v in srvs.iter().chain(uavs.iter()) {
        if let Some(v) = view(v) {
            synced(resource(v.res));
        }
    }
    let cbs: [(*const u8, usize); 14] = core::array::from_fn(|i| {
        let b = st.cbs[i];
        if crate::res::is_resource(b) {
            let r = resource(b);
            (r.data.as_ptr(), r.data.len())
        } else {
            (core::ptr::null(), 0)
        }
    });
    let env = CsEnv { cbs, t: srvs.iter().map(|&v| view(v).and_then(|v| view_mem(v))).collect(), u: uavs.iter().map(|&v| view(v).and_then(|v| view_mem(v))).collect() };
    let job = Job { vm: &vm, env: &env, groups, next: AtomicU32::new(0) };
    // Workers, and this thread too.
    let total = groups[0] * groups[1] * groups[2];
    let n = cpus().min(total).saturating_sub(1);
    let mut threads = Vec::new();
    for _ in 0..n {
        let h = unsafe { CreateThread(0, 0, worker, &job as *const Job as *mut u8, 0, core::ptr::null_mut()) };
        if h != 0 {
            threads.push(h);
        }
    }
    job.work();
    for h in threads {
        unsafe {
            WaitForSingleObject(h, 0xFFFF_FFFF);
            CloseHandle(h);
        }
    }
    // What the shader wrote: the resources changed.
    for &v in &uavs {
        if let Some(v) = view(v) {
            let r = resource(v.res);
            for s in 0..r.subs.len() {
                r.changed(s);
            }
        }
    }
}

pub unsafe extern "system" fn Ctx_Dispatch(this: *mut Com, x: u32, y: u32, z: u32) {
    dispatch(crate::device::of(main_of(this)), [x, y, z]);
}

pub unsafe extern "system" fn Ctx_DispatchIndirect(this: *mut Com, args: *mut Com, offset: u32) {
    if !crate::res::is_resource(args) {
        return;
    }
    let r = resource(args);
    let o = offset as usize;
    let Some(b) = r.data.get(o..o + 12) else { return };
    let g: [u32; 3] = core::array::from_fn(|k| u32::from_le_bytes(b[k * 4..k * 4 + 4].try_into().unwrap()));
    dispatch(crate::device::of(main_of(this)), g);
}

static CS_IIDS: [Guid; 2] = [crate::vtbl::IID_ID3D11DEVICECHILD, crate::vtbl::IID_ID3D11COMPUTESHADER];

pub unsafe extern "system" fn Dev_CreateComputeShader(this: *mut Com, code: *const u8, len: usize, _link: *mut Com, out: *mut *mut Com) -> i32 {
    if code.is_null() || len < 32 {
        return E_INVALIDARG;
    }
    let bytes = unsafe { core::slice::from_raw_parts(code, len) }.to_vec();
    let Some(sh) = crate::dxbc::parse(&bytes) else { return E_INVALIDARG };
    let dev = main_of(this);
    if sh.kind != 5 || crate::device::of(dev).level < 0xB000 || crate::vm::Vm::new(&sh).is_none() {
        return E_INVALIDARG;
    }
    let [x, y, z] = sh.group;
    if x * y * z == 0 || x * y * z > 1024 || z > 64 {
        return E_INVALIDARG;
    }
    if out.is_null() {
        return S_FALSE;
    }
    let obj = crate::shader::ShaderObj { code: bytes, sh, ps: None, kernel: 0 };
    unsafe { *out = Com::new(&crate::vtbl::ID3D11COMPUTESHADER, &CS_IIDS, dev, Body::Shader(Box::new(obj))) };
    S_OK
}
