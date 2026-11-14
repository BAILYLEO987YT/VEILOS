//! ID3D11DeviceContext's bindings: what each pipeline stage has set, kept
//! (with a reference each) for the draws, and given back by the getters.

use alloc::vec::Vec;

use crate::com::*;

pub const STAGE_VS: usize = 0;
pub const STAGE_HS: usize = 1;
pub const STAGE_DS: usize = 2;
pub const STAGE_GS: usize = 3;
pub const STAGE_PS: usize = 4;
pub const STAGE_CS: usize = 5;

const NULL: *mut Com = core::ptr::null_mut();

#[derive(Clone)]
pub struct Stage {
    pub shader: *mut Com,
    pub cbs: [*mut Com; 14],
    pub srvs: [*mut Com; 128],
    pub samplers: [*mut Com; 16],
}

impl Default for Stage {
    fn default() -> Stage {
        Stage { shader: NULL, cbs: [NULL; 14], srvs: [NULL; 128], samplers: [NULL; 16] }
    }
}

#[derive(Clone)]
pub struct Bindings {
    pub layout: *mut Com,
    /// Each slot: buffer, stride, offset.
    pub vbs: [(*mut Com, u32, u32); 32],
    pub ib: (*mut Com, u32, u32),
    pub topology: u32,
    pub stages: [Stage; 6],
    pub rs: *mut Com,
    pub viewports: Vec<[f32; 6]>,
    pub scissors: Vec<[i32; 4]>,
    pub rtvs: [*mut Com; 8],
    pub dsv: *mut Com,
    pub uavs: [*mut Com; 8],
    /// The compute stage's own UAVs.
    pub cs_uavs: [*mut Com; 8],
    pub blend: *mut Com,
    pub blend_factor: [f32; 4],
    pub sample_mask: u32,
    pub dss: *mut Com,
    pub stencil_ref: u32,
    pub so: [(*mut Com, u32); 4],
    pub predicate: (*mut Com, i32),
}

impl Default for Bindings {
    fn default() -> Bindings {
        Bindings {
            layout: NULL,
            vbs: [(NULL, 0, 0); 32],
            ib: (NULL, 0, 0),
            topology: 0,
            stages: Default::default(),
            rs: NULL,
            viewports: Vec::new(),
            scissors: Vec::new(),
            rtvs: [NULL; 8],
            dsv: NULL,
            uavs: [NULL; 8],
            cs_uavs: [NULL; 8],
            blend: NULL,
            blend_factor: [1.0; 4],
            sample_mask: 0xFFFF_FFFF,
            dss: NULL,
            stencil_ref: 0,
            so: [(NULL, 0); 4],
            predicate: (NULL, 0),
        }
    }
}

impl Bindings {
    /// A copy holding references of its own.
    pub fn snapshot(&self) -> Bindings {
        let s = self.clone();
        for p in s.refs() {
            unsafe { add_ref(p) };
        }
        s
    }

    fn refs(&self) -> Vec<*mut Com> {
        let mut all: Vec<*mut Com> = Vec::new();
        all.push(self.layout);
        all.extend(self.vbs.iter().map(|v| v.0));
        all.push(self.ib.0);
        for s in &self.stages {
            all.push(s.shader);
            all.extend(s.cbs);
            all.extend(s.srvs);
            all.extend(s.samplers);
        }
        all.push(self.rs);
        all.extend(self.rtvs);
        all.push(self.dsv);
        all.extend(self.uavs);
        all.extend(self.cs_uavs);
        all.push(self.blend);
        all.push(self.dss);
        all.extend(self.so.iter().map(|s| s.0));
        all.push(self.predicate.0);
        all.retain(|p| !p.is_null());
        all
    }

    /// Every reference let go, every state back to its default.
    pub fn clear(&mut self) {
        let mut all: Vec<*mut Com> = Vec::new();
        all.push(self.layout);
        all.extend(self.vbs.iter().map(|v| v.0));
        all.push(self.ib.0);
        for s in &self.stages {
            all.push(s.shader);
            all.extend(s.cbs);
            all.extend(s.srvs);
            all.extend(s.samplers);
        }
        all.push(self.rs);
        all.extend(self.rtvs);
        all.push(self.dsv);
        all.extend(self.uavs);
        all.extend(self.cs_uavs);
        all.push(self.blend);
        all.push(self.dss);
        all.extend(self.so.iter().map(|s| s.0));
        all.push(self.predicate.0);
        *self = Bindings::default();
        for p in all {
            unsafe { release(p) };
        }
    }
}

fn bind<'a>(this: *mut Com) -> &'a mut Bindings {
    match crate::deferred::of(this) {
        Some(d) => &mut d.state,
        None => &mut crate::device::of(main_of(this)).state,
    }
}

/// The device behind an immediate context (None: a deferred one).
pub fn immediate<'a>(this: *mut Com) -> Option<&'a mut crate::device::Device> {
    match body(this) {
        Body::Device(d) => Some(d),
        _ => None,
    }
}

/// Set `n` slots from `start` of `slots` to `src` (null: unbind).
unsafe fn set_slots(slots: &mut [*mut Com], start: u32, n: u32, src: *const *mut Com) {
    for i in 0..n as usize {
        let at = start as usize + i;
        if at >= slots.len() {
            break;
        }
        let v = if src.is_null() { NULL } else { main_of(unsafe { *src.add(i) }) };
        unsafe { set_ref(&mut slots[at], v) };
    }
}

unsafe fn get_slots(slots: &[*mut Com], start: u32, n: u32, out: *mut *mut Com) {
    if out.is_null() {
        return;
    }
    for i in 0..n as usize {
        let v = slots.get(start as usize + i).copied().unwrap_or(NULL);
        unsafe {
            *out.add(i) = NULL;
            out_ref(out.add(i), v);
        }
    }
}

macro_rules! stage {
    ($st:expr, $set_shader:ident, $get_shader:ident, $set_cb:ident, $get_cb:ident, $set_srv:ident, $get_srv:ident, $set_samp:ident, $get_samp:ident) => {
        pub unsafe extern "system" fn $set_shader(this: *mut Com, s: *mut Com, _ci: *const *mut Com, _n: u32) {
            if let Some(d) = immediate(this) {
                d.flush_if_bound();
            }
            unsafe { set_ref(&mut bind(this).stages[$st].shader, main_of(s)) };
        }
        pub unsafe extern "system" fn $get_shader(this: *mut Com, out: *mut *mut Com, ci: *mut *mut Com, n: *mut u32) {
            if !out.is_null() {
                unsafe {
                    *out = NULL;
                    out_ref(out, bind(this).stages[$st].shader);
                }
            }
            if !n.is_null() {
                unsafe { *n = 0 };
            }
            let _ = ci;
        }
        pub unsafe extern "system" fn $set_cb(this: *mut Com, start: u32, n: u32, b: *const *mut Com) {
            unsafe { set_slots(&mut bind(this).stages[$st].cbs, start, n, b) };
        }
        pub unsafe extern "system" fn $get_cb(this: *mut Com, start: u32, n: u32, out: *mut *mut Com) {
            unsafe { get_slots(&bind(this).stages[$st].cbs, start, n, out) };
        }
        pub unsafe extern "system" fn $set_srv(this: *mut Com, start: u32, n: u32, v: *const *mut Com) {
            unsafe { set_slots(&mut bind(this).stages[$st].srvs, start, n, v) };
        }
        pub unsafe extern "system" fn $get_srv(this: *mut Com, start: u32, n: u32, out: *mut *mut Com) {
            unsafe { get_slots(&bind(this).stages[$st].srvs, start, n, out) };
        }
        pub unsafe extern "system" fn $set_samp(this: *mut Com, start: u32, n: u32, v: *const *mut Com) {
            unsafe { set_slots(&mut bind(this).stages[$st].samplers, start, n, v) };
        }
        pub unsafe extern "system" fn $get_samp(this: *mut Com, start: u32, n: u32, out: *mut *mut Com) {
            unsafe { get_slots(&bind(this).stages[$st].samplers, start, n, out) };
        }
    };
}

stage!(STAGE_VS, Ctx_VSSetShader, Ctx_VSGetShader, Ctx_VSSetConstantBuffers, Ctx_VSGetConstantBuffers, Ctx_VSSetShaderResources, Ctx_VSGetShaderResources, Ctx_VSSetSamplers, Ctx_VSGetSamplers);
stage!(STAGE_HS, Ctx_HSSetShader, Ctx_HSGetShader, Ctx_HSSetConstantBuffers, Ctx_HSGetConstantBuffers, Ctx_HSSetShaderResources, Ctx_HSGetShaderResources, Ctx_HSSetSamplers, Ctx_HSGetSamplers);
stage!(STAGE_DS, Ctx_DSSetShader, Ctx_DSGetShader, Ctx_DSSetConstantBuffers, Ctx_DSGetConstantBuffers, Ctx_DSSetShaderResources, Ctx_DSGetShaderResources, Ctx_DSSetSamplers, Ctx_DSGetSamplers);
stage!(STAGE_GS, Ctx_GSSetShader, Ctx_GSGetShader, Ctx_GSSetConstantBuffers, Ctx_GSGetConstantBuffers, Ctx_GSSetShaderResources, Ctx_GSGetShaderResources, Ctx_GSSetSamplers, Ctx_GSGetSamplers);
stage!(STAGE_PS, Ctx_PSSetShader, Ctx_PSGetShader, Ctx_PSSetConstantBuffers, Ctx_PSGetConstantBuffers, Ctx_PSSetShaderResources, Ctx_PSGetShaderResources, Ctx_PSSetSamplers, Ctx_PSGetSamplers);
stage!(STAGE_CS, Ctx_CSSetShader, Ctx_CSGetShader, Ctx_CSSetConstantBuffers, Ctx_CSGetConstantBuffers, Ctx_CSSetShaderResources, Ctx_CSGetShaderResources, Ctx_CSSetSamplers, Ctx_CSGetSamplers);

impl crate::device::Device {
    /// Nothing to do: draws already waiting hold what they need.
    pub fn flush_if_bound(&mut self) {}
}

// -----| input assembler |-----

pub unsafe extern "system" fn Ctx_IASetInputLayout(this: *mut Com, l: *mut Com) {
    unsafe { set_ref(&mut bind(this).layout, main_of(l)) };
}

pub unsafe extern "system" fn Ctx_IAGetInputLayout(this: *mut Com, out: *mut *mut Com) {
    if !out.is_null() {
        unsafe {
            *out = NULL;
            out_ref(out, bind(this).layout);
        }
    }
}

pub unsafe extern "system" fn Ctx_IASetVertexBuffers(this: *mut Com, start: u32, n: u32, b: *const *mut Com, strides: *const u32, offsets: *const u32) {
    let s = bind(this);
    for i in 0..n as usize {
        let at = start as usize + i;
        if at >= s.vbs.len() {
            break;
        }
        let v = if b.is_null() { NULL } else { main_of(unsafe { *b.add(i) }) };
        unsafe { set_ref(&mut s.vbs[at].0, v) };
        s.vbs[at].1 = if strides.is_null() { 0 } else { unsafe { *strides.add(i) } };
        s.vbs[at].2 = if offsets.is_null() { 0 } else { unsafe { *offsets.add(i) } };
    }
}

pub unsafe extern "system" fn Ctx_IAGetVertexBuffers(this: *mut Com, start: u32, n: u32, out: *mut *mut Com, strides: *mut u32, offsets: *mut u32) {
    let s = bind(this);
    for i in 0..n as usize {
        let v = s.vbs.get(start as usize + i).copied().unwrap_or((NULL, 0, 0));
        unsafe {
            if !out.is_null() {
                *out.add(i) = NULL;
                out_ref(out.add(i), v.0);
            }
            if !strides.is_null() {
                *strides.add(i) = v.1;
            }
            if !offsets.is_null() {
                *offsets.add(i) = v.2;
            }
        }
    }
}

pub unsafe extern "system" fn Ctx_IASetIndexBuffer(this: *mut Com, b: *mut Com, format: u32, offset: u32) {
    let s = bind(this);
    unsafe { set_ref(&mut s.ib.0, main_of(b)) };
    (s.ib.1, s.ib.2) = (format, offset);
}

pub unsafe extern "system" fn Ctx_IAGetIndexBuffer(this: *mut Com, out: *mut *mut Com, format: *mut u32, offset: *mut u32) {
    let s = bind(this);
    unsafe {
        if !out.is_null() {
            *out = NULL;
            out_ref(out, s.ib.0);
        }
        if !format.is_null() {
            *format = s.ib.1;
        }
        if !offset.is_null() {
            *offset = s.ib.2;
        }
    }
}

pub unsafe extern "system" fn Ctx_IASetPrimitiveTopology(this: *mut Com, t: u32) {
    bind(this).topology = t;
}

pub unsafe extern "system" fn Ctx_IAGetPrimitiveTopology(this: *mut Com, out: *mut u32) {
    if !out.is_null() {
        unsafe { *out = bind(this).topology };
    }
}

// -----| rasterizer |-----

pub unsafe extern "system" fn Ctx_RSSetState(this: *mut Com, s: *mut Com) {
    unsafe { set_ref(&mut bind(this).rs, main_of(s)) };
}

pub unsafe extern "system" fn Ctx_RSGetState(this: *mut Com, out: *mut *mut Com) {
    if !out.is_null() {
        unsafe {
            *out = NULL;
            out_ref(out, bind(this).rs);
        }
    }
}

pub unsafe extern "system" fn Ctx_RSSetViewports(this: *mut Com, n: u32, v: *const [f32; 6]) {
    let s = bind(this);
    s.viewports.clear();
    for i in 0..n.min(16) as usize {
        if !v.is_null() {
            s.viewports.push(unsafe { *v.add(i) });
        }
    }
}

pub unsafe extern "system" fn Ctx_RSGetViewports(this: *mut Com, n: *mut u32, out: *mut [f32; 6]) {
    let s = bind(this);
    if n.is_null() {
        return;
    }
    if out.is_null() {
        unsafe { *n = s.viewports.len() as u32 };
        return;
    }
    let want = unsafe { *n } as usize;
    for i in 0..want {
        unsafe { *out.add(i) = s.viewports.get(i).copied().unwrap_or([0.0; 6]) };
    }
    unsafe { *n = s.viewports.len().min(want) as u32 };
}

pub unsafe extern "system" fn Ctx_RSSetScissorRects(this: *mut Com, n: u32, r: *const [i32; 4]) {
    let s = bind(this);
    s.scissors.clear();
    for i in 0..n.min(16) as usize {
        if !r.is_null() {
            s.scissors.push(unsafe { *r.add(i) });
        }
    }
}

pub unsafe extern "system" fn Ctx_RSGetScissorRects(this: *mut Com, n: *mut u32, out: *mut [i32; 4]) {
    let s = bind(this);
    if n.is_null() {
        return;
    }
    if out.is_null() {
        unsafe { *n = s.scissors.len() as u32 };
        return;
    }
    let want = unsafe { *n } as usize;
    for i in 0..want {
        unsafe { *out.add(i) = s.scissors.get(i).copied().unwrap_or([0; 4]) };
    }
    unsafe { *n = s.scissors.len().min(want) as u32 };
}

// -----| output merger |-----

pub unsafe extern "system" fn Ctx_OMSetRenderTargets(this: *mut Com, n: u32, rtvs: *const *mut Com, dsv: *mut Com) {
    let s = bind(this);
    for i in 0..8 {
        let v = if (i as u32) < n && !rtvs.is_null() { main_of(unsafe { *rtvs.add(i) }) } else { NULL };
        unsafe { set_ref(&mut s.rtvs[i], v) };
    }
    unsafe { set_ref(&mut s.dsv, main_of(dsv)) };
}

pub unsafe extern "system" fn Ctx_OMGetRenderTargets(this: *mut Com, n: u32, rtvs: *mut *mut Com, dsv: *mut *mut Com) {
    let s = bind(this);
    if !rtvs.is_null() {
        unsafe { get_slots(&s.rtvs, 0, n, rtvs) };
    }
    if !dsv.is_null() {
        unsafe {
            *dsv = NULL;
            out_ref(dsv, s.dsv);
        }
    }
}

pub unsafe extern "system" fn Ctx_OMSetRenderTargetsAndUnorderedAccessViews(this: *mut Com, n: u32, rtvs: *const *mut Com, dsv: *mut Com, ustart: u32, nu: u32, uavs: *const *mut Com, _counts: *const u32) {
    // D3D11_KEEP_RENDER_TARGETS_AND_DEPTH_STENCIL: leave them.
    if n != 0xFFFF_FFFF {
        unsafe { Ctx_OMSetRenderTargets(this, n, rtvs, dsv) };
    }
    if nu != 0xFFFF_FFFF {
        unsafe { set_slots(&mut bind(this).uavs, ustart, nu, uavs) };
    }
}

pub unsafe extern "system" fn Ctx_OMGetRenderTargetsAndUnorderedAccessViews(this: *mut Com, n: u32, rtvs: *mut *mut Com, dsv: *mut *mut Com, ustart: u32, nu: u32, uavs: *mut *mut Com) {
    unsafe {
        Ctx_OMGetRenderTargets(this, n, rtvs, dsv);
        get_slots(&bind(this).uavs, ustart, nu, uavs);
    }
}

pub unsafe extern "system" fn Ctx_OMSetBlendState(this: *mut Com, b: *mut Com, factor: *const [f32; 4], mask: u32) {
    let s = bind(this);
    unsafe { set_ref(&mut s.blend, main_of(b)) };
    s.blend_factor = if factor.is_null() { [1.0; 4] } else { unsafe { *factor } };
    s.sample_mask = mask;
}

pub unsafe extern "system" fn Ctx_OMGetBlendState(this: *mut Com, out: *mut *mut Com, factor: *mut [f32; 4], mask: *mut u32) {
    let s = bind(this);
    unsafe {
        if !out.is_null() {
            *out = NULL;
            out_ref(out, s.blend);
        }
        if !factor.is_null() {
            *factor = s.blend_factor;
        }
        if !mask.is_null() {
            *mask = s.sample_mask;
        }
    }
}

pub unsafe extern "system" fn Ctx_OMSetDepthStencilState(this: *mut Com, d: *mut Com, r: u32) {
    let s = bind(this);
    unsafe { set_ref(&mut s.dss, main_of(d)) };
    s.stencil_ref = r;
}

pub unsafe extern "system" fn Ctx_OMGetDepthStencilState(this: *mut Com, out: *mut *mut Com, r: *mut u32) {
    let s = bind(this);
    unsafe {
        if !out.is_null() {
            *out = NULL;
            out_ref(out, s.dss);
        }
        if !r.is_null() {
            *r = s.stencil_ref;
        }
    }
}

// -----| stream output, predication, compute views |-----

pub unsafe extern "system" fn Ctx_SOSetTargets(this: *mut Com, n: u32, b: *const *mut Com, offsets: *const u32) {
    let s = bind(this);
    for i in 0..4 {
        let v = if (i as u32) < n && !b.is_null() { main_of(unsafe { *b.add(i) }) } else { NULL };
        unsafe { set_ref(&mut s.so[i].0, v) };
        s.so[i].1 = if (i as u32) < n && !offsets.is_null() { unsafe { *offsets.add(i) } } else { 0 };
        if crate::res::is_resource(v) && s.so[i].1 != u32::MAX {
            crate::res::resource(v).so_filled = s.so[i].1;
        }
    }
}

pub unsafe extern "system" fn Ctx_SOGetTargets(this: *mut Com, n: u32, out: *mut *mut Com) {
    let s = bind(this);
    let v: Vec<*mut Com> = s.so.iter().map(|x| x.0).collect();
    unsafe { get_slots(&v, 0, n, out) };
}

pub unsafe extern "system" fn Ctx_SetPredication(this: *mut Com, p: *mut Com, value: i32) {
    let s = bind(this);
    unsafe { set_ref(&mut s.predicate.0, main_of(p)) };
    s.predicate.1 = value;
}

pub unsafe extern "system" fn Ctx_GetPredication(this: *mut Com, out: *mut *mut Com, value: *mut i32) {
    let s = bind(this);
    unsafe {
        if !out.is_null() {
            *out = NULL;
            out_ref(out, s.predicate.0);
        }
        if !value.is_null() {
            *value = s.predicate.1;
        }
    }
}

pub unsafe extern "system" fn Ctx_CSSetUnorderedAccessViews(this: *mut Com, start: u32, n: u32, v: *const *mut Com, _counts: *const u32) {
    unsafe { set_slots(&mut bind(this).cs_uavs, start, n, v) };
}

pub unsafe extern "system" fn Ctx_CSGetUnorderedAccessViews(this: *mut Com, start: u32, n: u32, out: *mut *mut Com) {
    unsafe { get_slots(&bind(this).cs_uavs, start, n, out) };
}

pub unsafe extern "system" fn Ctx_ClearState(this: *mut Com) {
    if let Some(d) = crate::deferred::of(this) {
        d.state.clear();
        return;
    }
    let d = crate::device::of(main_of(this));
    d.flush();
    d.state.clear();
}

pub unsafe extern "system" fn Ctx_SetResourceMinLOD(_this: *mut Com, _r: *mut Com, _lod: f32) {}

pub unsafe extern "system" fn Ctx_GetResourceMinLOD(_this: *mut Com, _r: *mut Com) -> f32 {
    0.0
}
