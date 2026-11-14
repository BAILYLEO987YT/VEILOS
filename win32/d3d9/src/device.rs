//! IDirect3DDevice9: the device, its state's setters and getters, render
//! targets, state blocks, the swap chain and queries. Drawing, clearing
//! and presenting are draw.rs's.

use alloc::boxed::Box;
use alloc::vec::Vec;

use gfx3d::raster3d::{Group, Tri};

use crate::com::*;
use crate::res::{self, Surface, USAGE_DEPTHSTENCIL, USAGE_RENDERTARGET};
use crate::state::*;
use crate::vtbl;

/// D3DPRESENT_PARAMETERS.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Params {
    pub w: u32,
    pub h: u32,
    pub format: u32,
    pub count: u32,
    pub multisample: u32,
    pub quality: u32,
    pub swap_effect: u32,
    _pad: u32,
    pub hwnd: usize,
    pub windowed: i32,
    pub auto_depth: i32,
    pub depth_format: u32,
    pub flags: u32,
    pub refresh: u32,
    pub interval: u32,
}

/// Primitives waiting to go to the kernel in one VEIL_GPU3D draw.
#[derive(Default)]
pub struct Batch {
    pub tris: Vec<Tri>,
    pub groups: Vec<Group>,
    /// Pixel shader planes and constants (raster3d's DrawArgs::attrs).
    pub attrs: Vec<f32>,
    /// The render target and depth target they're for.
    pub target: u64,
    pub depth: u64,
}

pub struct Device {
    pub this: *mut Com,
    pub d3d: *mut Com,
    pub params: Params,
    pub devtype: u32,
    pub focus: usize,
    pub flags: u32,
    pub swap: *mut Com,
    /// The device's own surfaces (their container is the device).
    pub backbuffer: *mut Com,
    pub auto_depth: *mut Com,
    /// What's drawn into (held, unless the device's own).
    pub rt: *mut Com,
    pub ds: *mut Com,
    pub state: State,
    /// Between BeginStateBlock and EndStateBlock: what's set goes here.
    pub recording: Option<Box<StateBlock>>,
    pub batch: Batch,
    /// The declarations made for FVFs (SetFVF).
    pub fvf_decls: Vec<(u32, *mut Com)>,
    pub gamma: [u16; 768],
    pub sw_vp: bool,
}

impl Device {
    /// Send the waiting primitives to the kernel.
    pub fn flush(&mut self) {
        let b = &mut self.batch;
        if !b.tris.is_empty() {
            let args = gfx3d::raster3d::DrawArgs {
                groups: b.groups.as_ptr() as u64,
                group_count: b.groups.len() as u32,
                tri_count: b.tris.len() as u32,
                tris: b.tris.as_ptr() as u64,
                attrs: b.attrs.as_ptr() as u64,
                attr_len: b.attrs.len() as u32,
                _pad: 0,
            };
            gfx3d::gpu3d(2, b.target, &args as *const _ as u64, b.depth);
        }
        b.tris.clear();
        b.groups.clear();
        b.attrs.clear();
        for h in core::mem::take(&mut *res::PENDING_CLOSE.lock()) {
            gfx3d::close(h);
        }
    }

    /// Set state through `f`: into the state block being recorded (marking
    /// `m`), or the device's own (`true`: it holds references).
    pub fn record(&mut self, m: usize, f: impl FnOnce(&mut State, bool)) {
        match &mut self.recording {
            Some(b) => {
                b.mask.set(m);
                f(&mut b.state, false)
            }
            None => f(&mut self.state, true),
        }
    }

    /// Set light `i` through `f`, as `record` does.
    pub fn record_light(&mut self, i: usize, f: impl FnOnce(&mut State)) {
        match &mut self.recording {
            Some(b) => {
                b.mask.light(i);
                f(&mut b.state)
            }
            None => f(&mut self.state),
        }
    }

    /// A surface bound as render target or depth buffer: held unless it's
    /// the device's own (which would hold the device itself).
    fn bind(&self, slot: *mut *mut Com, new: *mut Com) {
        let own = |p: *mut Com| p.is_null() || unsafe { (*p).container } == self.this;
        unsafe {
            if !own(new) {
                add_ref(new);
            }
            if !own(*slot) {
                release(*slot);
            }
            *slot = new;
        }
    }

    /// Release everything bound (the device is going, or being reset).
    fn unbind_all(&mut self) {
        self.flush();
        let (rt, ds) = (&mut self.rt as *mut _, &mut self.ds as *mut _);
        self.bind(rt, core::ptr::null_mut());
        self.bind(ds, core::ptr::null_mut());
        let s = &mut self.state;
        for t in s.textures.iter_mut() {
            unsafe { set_ref(t, core::ptr::null_mut()) };
        }
        for st in s.streams.iter_mut() {
            unsafe { set_ref(&mut st.buffer, core::ptr::null_mut()) };
        }
        unsafe {
            set_ref(&mut s.indices, core::ptr::null_mut());
            set_ref(&mut s.decl, core::ptr::null_mut());
            set_ref(&mut s.vs, core::ptr::null_mut());
            set_ref(&mut s.ps, core::ptr::null_mut());
        }
    }

    /// Make the back buffer and depth buffer for `params`, and reset the
    /// state to the defaults.
    pub fn setup(&mut self) -> bool {
        let p = self.params;
        let Some(bb) = Surface::new(p.format, p.w, p.h, USAGE_RENDERTARGET, 0) else { return false };
        let bb = res::new_surface(self.this, bb);
        unsafe { (*bb).container = self.this };
        self.backbuffer = bb;
        self.auto_depth = core::ptr::null_mut();
        if p.auto_depth != 0 {
            let Some(ds) = Surface::new(p.depth_format, p.w, p.h, USAGE_DEPTHSTENCIL, 0) else { return false };
            let ds = res::new_surface(self.this, ds);
            unsafe { (*ds).container = self.this };
            self.auto_depth = ds;
        }
        self.rt = self.backbuffer;
        self.ds = self.auto_depth;
        self.state = State::new(p.auto_depth != 0, p.w, p.h);
        true
    }

    fn teardown(&mut self) {
        self.unbind_all();
        for p in [self.backbuffer, self.auto_depth] {
            if !p.is_null() {
                unsafe { free(p) };
            }
        }
        self.backbuffer = core::ptr::null_mut();
        self.auto_depth = core::ptr::null_mut();
        for (_, d) in self.fvf_decls.drain(..) {
            unsafe { release(d) };
        }
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        self.teardown();
        unsafe {
            free(self.swap);
            release(self.d3d);
        }
    }
}

static DEVICE_IIDS: [Guid; 1] = [IID_DEVICE9];
static SWAP_IIDS: [Guid; 1] = [IID_SWAPCHAIN9];
static QUERY_IIDS: [Guid; 1] = [IID_QUERY9];

/// A new device for `params` (already completed by CreateDevice).
pub fn create(d3d: *mut Com, devtype: u32, focus: usize, flags: u32, params: Params) -> Option<*mut Com> {
    let d = Device {
        this: core::ptr::null_mut(),
        d3d,
        params,
        devtype,
        focus,
        flags,
        swap: core::ptr::null_mut(),
        backbuffer: core::ptr::null_mut(),
        auto_depth: core::ptr::null_mut(),
        rt: core::ptr::null_mut(),
        ds: core::ptr::null_mut(),
        state: State::new(false, 0, 0),
        recording: None,
        batch: Batch::default(),
        fvf_decls: Vec::new(),
        gamma: core::array::from_fn(|i| ((i % 256) * 257) as u16),
        sw_vp: flags & 0x20 != 0,
    };
    let obj = Com::new(&vtbl::IDIRECT3DDEVICE9, &DEVICE_IIDS, core::ptr::null_mut(), Body::Device(Box::new(d)));
    let dev = unsafe { device(obj) };
    dev.this = obj;
    // The swap chain answers for the device (it has no count of its own).
    let sc = Com::new(&vtbl::IDIRECT3DSWAPCHAIN9, &SWAP_IIDS, obj, Body::SwapChain);
    unsafe { (*sc).container = obj };
    dev.swap = sc;
    unsafe { add_ref(d3d) };
    if !dev.setup() {
        unsafe { release(obj) };
        return None;
    }
    Some(obj)
}

fn dev<'a>(this: *mut Com) -> &'a mut Device {
    unsafe { device(this) }
}

fn put<T>(out: *mut T, v: T) -> i32 {
    if out.is_null() {
        return D3DERR_INVALIDCALL;
    }
    unsafe { core::ptr::write_unaligned(out, v) };
    S_OK
}

fn get<T: Copy>(p: *const T) -> Option<T> {
    if p.is_null() { None } else { Some(unsafe { core::ptr::read_unaligned(p) }) }
}

// -----| the device itself |-----

pub unsafe extern "system" fn Dev_TestCooperativeLevel(_this: *mut Com) -> i32 {
    S_OK
}

pub unsafe extern "system" fn Dev_GetAvailableTextureMem(_this: *mut Com) -> u32 {
    512 << 20
}

pub unsafe extern "system" fn Dev_EvictManagedResources(_this: *mut Com) -> i32 {
    S_OK
}

pub unsafe extern "system" fn Dev_GetDirect3D(this: *mut Com, out: *mut *mut Com) -> i32 {
    unsafe { out_ref(out, dev(this).d3d) }
}

pub unsafe extern "system" fn Dev_GetDeviceCaps(this: *mut Com, out: *mut u8) -> i32 {
    if out.is_null() {
        return D3DERR_INVALIDCALL;
    }
    crate::d3d::caps(dev(this).devtype, out);
    S_OK
}

pub unsafe extern "system" fn Dev_GetDisplayMode(_this: *mut Com, _swap: u32, out: *mut [u32; 4]) -> i32 {
    put(out, crate::d3d::desktop_mode())
}

#[repr(C)]
struct CreationParams {
    adapter: u32,
    devtype: u32,
    focus: usize,
    flags: u32,
}

pub unsafe extern "system" fn Dev_GetCreationParameters(this: *mut Com, out: *mut u8) -> i32 {
    let d = dev(this);
    put(out as *mut CreationParams, CreationParams { adapter: 0, devtype: d.devtype, focus: d.focus, flags: d.flags })
}

pub unsafe extern "system" fn Dev_SetCursorProperties(_this: *mut Com, _x: u32, _y: u32, _s: *mut Com) -> i32 {
    S_OK
}

pub unsafe extern "system" fn Dev_SetCursorPosition(_this: *mut Com, _x: i32, _y: i32, _flags: u32) {}

pub unsafe extern "system" fn Dev_ShowCursor(_this: *mut Com, _show: i32) -> i32 {
    0
}

pub unsafe extern "system" fn Dev_GetSwapChain(this: *mut Com, i: u32, out: *mut *mut Com) -> i32 {
    if i != 0 {
        return D3DERR_INVALIDCALL;
    }
    unsafe { out_ref(out, dev(this).swap) }
}

pub unsafe extern "system" fn Dev_GetNumberOfSwapChains(_this: *mut Com) -> u32 {
    1
}

pub unsafe extern "system" fn Dev_Reset(this: *mut Com, params: *mut Params) -> i32 {
    let Some(mut p) = get(params) else { return D3DERR_INVALIDCALL };
    let d = dev(this);
    if !crate::d3d::complete_params(&mut p, d.focus) {
        return D3DERR_INVALIDCALL;
    }
    unsafe { *params = p };
    d.recording = None;
    d.teardown();
    d.params = p;
    if !d.setup() {
        return D3DERR_INVALIDCALL;
    }
    S_OK
}

pub unsafe extern "system" fn Dev_GetBackBuffer(this: *mut Com, swap: u32, i: u32, _t: u32, out: *mut *mut Com) -> i32 {
    if swap != 0 || i != 0 {
        return D3DERR_INVALIDCALL;
    }
    unsafe { out_ref(out, dev(this).backbuffer) }
}

pub unsafe extern "system" fn Dev_GetRasterStatus(_this: *mut Com, _swap: u32, out: *mut [u32; 2]) -> i32 {
    put(out, [0, 0])
}

pub unsafe extern "system" fn Dev_SetDialogBoxMode(_this: *mut Com, _on: i32) -> i32 {
    S_OK
}

pub unsafe extern "system" fn Dev_SetGammaRamp(this: *mut Com, _swap: u32, _flags: u32, ramp: *const [u16; 768]) {
    if let Some(r) = get(ramp) {
        dev(this).gamma = r;
    }
}

pub unsafe extern "system" fn Dev_GetGammaRamp(this: *mut Com, _swap: u32, out: *mut [u16; 768]) {
    put(out, dev(this).gamma);
}

pub unsafe extern "system" fn Dev_GetFrontBufferData(this: *mut Com, _swap: u32, dst: *mut Com) -> i32 {
    // The last frame shown is the back buffer's (copied, not flipped).
    let bb = dev(this).backbuffer;
    unsafe { res::Dev_GetRenderTargetData(this, bb, dst) }
}

// -----| render targets |-----

pub unsafe extern "system" fn Dev_SetRenderTarget(this: *mut Com, i: u32, s: *mut Com) -> i32 {
    if i != 0 {
        return if i < 4 && s.is_null() { S_OK } else { D3DERR_INVALIDCALL };
    }
    if !is_surface(s) || !unsafe { surface(s) }.is_target() {
        return D3DERR_INVALIDCALL;
    }
    let d = dev(this);
    d.flush();
    let slot = &mut d.rt as *mut _;
    d.bind(slot, s);
    // The viewport (and scissor rectangle) become the whole target.
    let su = unsafe { surface(s) };
    d.state.viewport = Viewport { x: 0, y: 0, w: su.w, h: su.h, min_z: 0.0, max_z: 1.0 };
    d.state.scissor = [0, 0, su.w as i32, su.h as i32];
    S_OK
}

pub unsafe extern "system" fn Dev_GetRenderTarget(this: *mut Com, i: u32, out: *mut *mut Com) -> i32 {
    if i != 0 {
        if !out.is_null() {
            unsafe { *out = core::ptr::null_mut() };
        }
        return D3DERR_NOTFOUND;
    }
    unsafe { out_ref(out, dev(this).rt) }
}

pub unsafe extern "system" fn Dev_SetDepthStencilSurface(this: *mut Com, s: *mut Com) -> i32 {
    if !s.is_null() && (!is_surface(s) || !unsafe { surface(s) }.is_depth()) {
        return D3DERR_INVALIDCALL;
    }
    let d = dev(this);
    d.flush();
    let slot = &mut d.ds as *mut _;
    d.bind(slot, s);
    S_OK
}

pub unsafe extern "system" fn Dev_GetDepthStencilSurface(this: *mut Com, out: *mut *mut Com) -> i32 {
    unsafe { out_ref(out, dev(this).ds) }
}

pub unsafe extern "system" fn Dev_BeginScene(_this: *mut Com) -> i32 {
    S_OK
}

pub unsafe extern "system" fn Dev_EndScene(this: *mut Com) -> i32 {
    dev(this).flush();
    S_OK
}

// -----| transforms, viewport, material, lights, clip planes |-----

pub unsafe extern "system" fn Dev_SetTransform(this: *mut Com, t: u32, m: *const [f32; 16]) -> i32 {
    let (Some(i), Some(m)) = (transform_index(t), get(m)) else { return D3DERR_INVALIDCALL };
    dev(this).record(M_TRANSFORM + i, |s, _| s.transforms[i] = m);
    S_OK
}

pub unsafe extern "system" fn Dev_GetTransform(this: *mut Com, t: u32, out: *mut [f32; 16]) -> i32 {
    let Some(i) = transform_index(t) else { return D3DERR_INVALIDCALL };
    put(out, dev(this).state.transforms[i])
}

pub unsafe extern "system" fn Dev_MultiplyTransform(this: *mut Com, t: u32, m: *const [f32; 16]) -> i32 {
    let (Some(i), Some(m)) = (transform_index(t), get(m)) else { return D3DERR_INVALIDCALL };
    let cur = dev(this).state.transforms[i];
    let r = crate::draw::mat_mul(&m, &cur);
    dev(this).record(M_TRANSFORM + i, |s, _| s.transforms[i] = r);
    S_OK
}

pub unsafe extern "system" fn Dev_SetViewport(this: *mut Com, v: *const Viewport) -> i32 {
    let Some(v) = get(v) else { return D3DERR_INVALIDCALL };
    dev(this).record(M_VIEWPORT, |s, _| s.viewport = v);
    S_OK
}

pub unsafe extern "system" fn Dev_GetViewport(this: *mut Com, out: *mut Viewport) -> i32 {
    put(out, dev(this).state.viewport)
}

pub unsafe extern "system" fn Dev_SetMaterial(this: *mut Com, m: *const Material) -> i32 {
    let Some(m) = get(m) else { return D3DERR_INVALIDCALL };
    dev(this).record(M_MATERIAL, |s, _| s.material = m);
    S_OK
}

pub unsafe extern "system" fn Dev_GetMaterial(this: *mut Com, out: *mut Material) -> i32 {
    put(out, dev(this).state.material)
}

fn light_slot(s: &mut State, i: usize) -> &mut (Light, bool) {
    if s.lights.len() <= i {
        s.lights.resize(i + 1, None);
    }
    // A light enabled before it's set has the default (a white
    // directional light along +z).
    s.lights[i].get_or_insert((
        Light { ltype: 3, diffuse: [1.0, 1.0, 1.0, 0.0], direction: [0.0, 0.0, 1.0], ..Default::default() },
        false,
    ))
}

pub unsafe extern "system" fn Dev_SetLight(this: *mut Com, i: u32, l: *const Light) -> i32 {
    let Some(l) = get(l) else { return D3DERR_INVALIDCALL };
    // Windows takes any type (a bad one just doesn't light).
    if i > 0xFFFF {
        return D3DERR_INVALIDCALL;
    }
    dev(this).record_light(i as usize, |s| light_slot(s, i as usize).0 = l);
    S_OK
}

pub unsafe extern "system" fn Dev_GetLight(this: *mut Com, i: u32, out: *mut Light) -> i32 {
    match dev(this).state.lights.get(i as usize) {
        Some(Some(l)) => put(out, l.0),
        _ => D3DERR_INVALIDCALL,
    }
}

pub unsafe extern "system" fn Dev_LightEnable(this: *mut Com, i: u32, on: i32) -> i32 {
    if i > 0xFFFF {
        return D3DERR_INVALIDCALL;
    }
    dev(this).record_light(i as usize, |s| light_slot(s, i as usize).1 = on != 0);
    S_OK
}

pub unsafe extern "system" fn Dev_GetLightEnable(this: *mut Com, i: u32, out: *mut i32) -> i32 {
    match dev(this).state.lights.get(i as usize) {
        // Direct3D answers 128 for "enabled".
        Some(Some(l)) => put(out, if l.1 { 128 } else { 0 }),
        _ => D3DERR_INVALIDCALL,
    }
}

pub unsafe extern "system" fn Dev_SetClipPlane(this: *mut Com, i: u32, p: *const [f32; 4]) -> i32 {
    let Some(p) = get(p) else { return D3DERR_INVALIDCALL };
    if i >= 6 {
        return D3DERR_INVALIDCALL;
    }
    dev(this).record(M_CLIP + i as usize, |s, _| s.clip_planes[i as usize] = p);
    S_OK
}

pub unsafe extern "system" fn Dev_GetClipPlane(this: *mut Com, i: u32, out: *mut [f32; 4]) -> i32 {
    if i >= 6 {
        return D3DERR_INVALIDCALL;
    }
    put(out, dev(this).state.clip_planes[i as usize])
}

// -----| render, texture stage and sampler states, textures |-----

pub unsafe extern "system" fn Dev_SetRenderState(this: *mut Com, n: u32, v: u32) -> i32 {
    if n as usize >= RS_COUNT {
        return D3DERR_INVALIDCALL;
    }
    dev(this).record(M_RS + n as usize, |s, _| s.rs[n as usize] = v);
    S_OK
}

pub unsafe extern "system" fn Dev_GetRenderState(this: *mut Com, n: u32, out: *mut u32) -> i32 {
    if n as usize >= RS_COUNT {
        return D3DERR_INVALIDCALL;
    }
    put(out, dev(this).state.rs[n as usize])
}

pub unsafe extern "system" fn Dev_SetTextureStageState(this: *mut Com, stage: u32, n: u32, v: u32) -> i32 {
    if stage >= 8 || n as usize >= TSS_COUNT {
        return D3DERR_INVALIDCALL;
    }
    let (st, n) = (stage as usize, n as usize);
    dev(this).record(M_TSS + st * TSS_COUNT + n, |s, _| s.tss[st][n] = v);
    S_OK
}

pub unsafe extern "system" fn Dev_GetTextureStageState(this: *mut Com, stage: u32, n: u32, out: *mut u32) -> i32 {
    if stage >= 8 || n as usize >= TSS_COUNT {
        return D3DERR_INVALIDCALL;
    }
    put(out, dev(this).state.tss[stage as usize][n as usize])
}

pub unsafe extern "system" fn Dev_SetSamplerState(this: *mut Com, sampler: u32, n: u32, v: u32) -> i32 {
    let Some(i) = sampler_index(sampler) else { return D3DERR_INVALIDCALL };
    if n as usize >= SS_COUNT {
        return D3DERR_INVALIDCALL;
    }
    let n = n as usize;
    dev(this).record(M_SS + i * SS_COUNT + n, |s, _| s.ss[i][n] = v);
    S_OK
}

pub unsafe extern "system" fn Dev_GetSamplerState(this: *mut Com, sampler: u32, n: u32, out: *mut u32) -> i32 {
    let Some(i) = sampler_index(sampler) else { return D3DERR_INVALIDCALL };
    if n as usize >= SS_COUNT {
        return D3DERR_INVALIDCALL;
    }
    put(out, dev(this).state.ss[i][n as usize])
}

pub unsafe extern "system" fn Dev_SetTexture(this: *mut Com, sampler: u32, t: *mut Com) -> i32 {
    let Some(i) = sampler_index(sampler) else { return D3DERR_INVALIDCALL };
    if !t.is_null() && !is_texture(t) {
        return D3DERR_INVALIDCALL;
    }
    dev(this).record(M_TEX + i, |s, counted| {
        if counted {
            unsafe { set_ref(&mut s.textures[i], t) };
        } else {
            s.textures[i] = t;
        }
    });
    S_OK
}

pub unsafe extern "system" fn Dev_GetTexture(this: *mut Com, sampler: u32, out: *mut *mut Com) -> i32 {
    let Some(i) = sampler_index(sampler) else { return D3DERR_INVALIDCALL };
    let t = dev(this).state.textures[i];
    let r = unsafe { out_ref(out, t) };
    if t.is_null() { S_OK } else { r }
}

pub unsafe extern "system" fn Dev_ValidateDevice(_this: *mut Com, passes: *mut u32) -> i32 {
    put(passes, 1)
}

pub unsafe extern "system" fn Dev_SetScissorRect(this: *mut Com, r: *const [i32; 4]) -> i32 {
    let Some(r) = get(r) else { return D3DERR_INVALIDCALL };
    dev(this).record(M_SCISSOR, |s, _| s.scissor = r);
    S_OK
}

pub unsafe extern "system" fn Dev_GetScissorRect(this: *mut Com, out: *mut [i32; 4]) -> i32 {
    put(out, dev(this).state.scissor)
}

pub unsafe extern "system" fn Dev_SetSoftwareVertexProcessing(this: *mut Com, on: i32) -> i32 {
    dev(this).sw_vp = on != 0;
    S_OK
}

pub unsafe extern "system" fn Dev_GetSoftwareVertexProcessing(this: *mut Com) -> i32 {
    dev(this).sw_vp as i32
}

pub unsafe extern "system" fn Dev_SetNPatchMode(this: *mut Com, n: f32) -> i32 {
    dev(this).record(M_NPATCH, |s, _| s.npatch = n);
    S_OK
}

pub unsafe extern "system" fn Dev_GetNPatchMode(this: *mut Com) -> f32 {
    dev(this).state.npatch
}

pub unsafe extern "system" fn Dev_SetClipStatus(_this: *mut Com, _s: *const [u32; 2]) -> i32 {
    S_OK
}

pub unsafe extern "system" fn Dev_GetClipStatus(_this: *mut Com, out: *mut [u32; 2]) -> i32 {
    put(out, [0, 0])
}

// -----| state blocks |-----

pub unsafe extern "system" fn Dev_CreateStateBlock(this: *mut Com, t: u32, out: *mut *mut Com) -> i32 {
    if out.is_null() || !(1..=3).contains(&t) {
        return D3DERR_INVALIDCALL;
    }
    let d = dev(this);
    let mask = Mask::of_type(t);
    let mut state = State::new(false, 0, 0);
    copy(&d.state, &mut state, &mask, false);
    unsafe { *out = new_block(this, state, mask) };
    S_OK
}

pub unsafe extern "system" fn Dev_BeginStateBlock(this: *mut Com) -> i32 {
    let d = dev(this);
    if d.recording.is_some() {
        return D3DERR_INVALIDCALL;
    }
    // What isn't set while recording keeps the device's values.
    d.recording = Some(Box::new(StateBlock { state: d.state.clone(), mask: Mask::none() }));
    S_OK
}

pub unsafe extern "system" fn Dev_EndStateBlock(this: *mut Com, out: *mut *mut Com) -> i32 {
    let d = dev(this);
    let Some(b) = d.recording.take() else { return D3DERR_INVALIDCALL };
    if out.is_null() {
        return D3DERR_INVALIDCALL;
    }
    unsafe { *out = new_block(this, b.state, b.mask) };
    S_OK
}

// -----| vertex input |-----

pub unsafe extern "system" fn Dev_SetVertexDeclaration(this: *mut Com, decl: *mut Com) -> i32 {
    dev(this).record(M_DECL, |s, counted| {
        if counted {
            unsafe { set_ref(&mut s.decl, decl) };
        } else {
            s.decl = decl;
        }
        s.fvf = match unsafe { decl.as_ref() } {
            Some(Com { body: Body::Decl(d), .. }) => d.fvf,
            _ => 0,
        };
    });
    S_OK
}

pub unsafe extern "system" fn Dev_GetVertexDeclaration(this: *mut Com, out: *mut *mut Com) -> i32 {
    let d = dev(this).state.decl;
    let r = unsafe { out_ref(out, d) };
    if d.is_null() { S_OK } else { r }
}

pub unsafe extern "system" fn Dev_SetFVF(this: *mut Com, fvf: u32) -> i32 {
    if fvf == 0 {
        return S_OK;
    }
    let d = dev(this);
    let decl = match d.fvf_decls.iter().find(|e| e.0 == fvf) {
        Some(e) => e.1,
        None => {
            let decl = res::new_decl(this, crate::draw::fvf_elements(fvf), fvf);
            d.fvf_decls.push((fvf, decl));
            decl
        }
    };
    unsafe { Dev_SetVertexDeclaration(this, decl) }
}

pub unsafe extern "system" fn Dev_GetFVF(this: *mut Com, out: *mut u32) -> i32 {
    put(out, dev(this).state.fvf)
}

pub unsafe extern "system" fn Dev_SetStreamSource(this: *mut Com, i: u32, vb: *mut Com, offset: u32, stride: u32) -> i32 {
    if i >= 16 || (!vb.is_null() && !matches!(unsafe { &(*vb).body }, Body::Buffer(b) if !b.index)) {
        return D3DERR_INVALIDCALL;
    }
    let i = i as usize;
    dev(this).record(M_STREAM + i, |s, counted| {
        if counted {
            unsafe { set_ref(&mut s.streams[i].buffer, vb) };
        } else {
            s.streams[i].buffer = vb;
        }
        (s.streams[i].offset, s.streams[i].stride) = (offset, stride);
    });
    S_OK
}

pub unsafe extern "system" fn Dev_GetStreamSource(this: *mut Com, i: u32, out: *mut *mut Com, offset: *mut u32, stride: *mut u32) -> i32 {
    if i >= 16 {
        return D3DERR_INVALIDCALL;
    }
    let s = dev(this).state.streams[i as usize];
    put(offset, s.offset);
    put(stride, s.stride);
    let r = unsafe { out_ref(out, s.buffer) };
    if s.buffer.is_null() { S_OK } else { r }
}

pub unsafe extern "system" fn Dev_SetStreamSourceFreq(this: *mut Com, i: u32, f: u32) -> i32 {
    if i >= 16 {
        return D3DERR_INVALIDCALL;
    }
    dev(this).record(M_STREAM + i as usize, |s, _| s.streams[i as usize].freq = f);
    S_OK
}

pub unsafe extern "system" fn Dev_GetStreamSourceFreq(this: *mut Com, i: u32, out: *mut u32) -> i32 {
    if i >= 16 {
        return D3DERR_INVALIDCALL;
    }
    put(out, dev(this).state.streams[i as usize].freq)
}

pub unsafe extern "system" fn Dev_SetIndices(this: *mut Com, ib: *mut Com) -> i32 {
    if !ib.is_null() && !matches!(unsafe { &(*ib).body }, Body::Buffer(b) if b.index) {
        return D3DERR_INVALIDCALL;
    }
    dev(this).record(M_INDICES, |s, counted| {
        if counted {
            unsafe { set_ref(&mut s.indices, ib) };
        } else {
            s.indices = ib;
        }
    });
    S_OK
}

pub unsafe extern "system" fn Dev_GetIndices(this: *mut Com, out: *mut *mut Com) -> i32 {
    let i = dev(this).state.indices;
    let r = unsafe { out_ref(out, i) };
    if i.is_null() { S_OK } else { r }
}

// -----| shaders: vertex shaders (shader.rs); pixel shaders not yet (G5e2) |-----

pub unsafe extern "system" fn Dev_SetVertexShader(this: *mut Com, s: *mut Com) -> i32 {
    if !s.is_null() && !crate::shader::of(s).is_some_and(|s| !s.pixel) {
        return D3DERR_INVALIDCALL;
    }
    dev(this).record(M_VS, |st, counted| {
        if counted {
            unsafe { set_ref(&mut st.vs, s) };
        } else {
            st.vs = s;
        }
    });
    S_OK
}

pub unsafe extern "system" fn Dev_GetVertexShader(this: *mut Com, out: *mut *mut Com) -> i32 {
    let s = dev(this).state.vs;
    let r = unsafe { out_ref(out, s) };
    if s.is_null() { S_OK } else { r }
}

pub unsafe extern "system" fn Dev_SetPixelShader(this: *mut Com, s: *mut Com) -> i32 {
    if !s.is_null() && !crate::shader::of(s).is_some_and(|s| s.pixel) {
        return D3DERR_INVALIDCALL;
    }
    dev(this).record(M_PS, |st, counted| {
        if counted {
            unsafe { set_ref(&mut st.ps, s) };
        } else {
            st.ps = s;
        }
    });
    S_OK
}

pub unsafe extern "system" fn Dev_GetPixelShader(this: *mut Com, out: *mut *mut Com) -> i32 {
    let s = dev(this).state.ps;
    let r = unsafe { out_ref(out, s) };
    if s.is_null() { S_OK } else { r }
}

fn set_consts<T: Copy>(dst: &mut [T], start: u32, src: *const T, n: u32) -> i32 {
    let (s, n) = (start as usize, n as usize);
    if src.is_null() || s + n > dst.len() {
        return D3DERR_INVALIDCALL;
    }
    for i in 0..n {
        dst[s + i] = unsafe { core::ptr::read_unaligned(src.add(i)) };
    }
    S_OK
}

fn get_consts<T: Copy>(src: &[T], start: u32, dst: *mut T, n: u32) -> i32 {
    let (s, n) = (start as usize, n as usize);
    if dst.is_null() || s + n > src.len() {
        return D3DERR_INVALIDCALL;
    }
    for i in 0..n {
        unsafe { core::ptr::write_unaligned(dst.add(i), src[s + i]) };
    }
    S_OK
}

macro_rules! consts {
    ($set:ident, $get:ident, $field:ident, $t:ty, $m:expr) => {
        pub unsafe extern "system" fn $set(this: *mut Com, start: u32, v: *const $t, n: u32) -> i32 {
            let mut r = S_OK;
            dev(this).record($m, |s, _| r = set_consts(&mut s.$field, start, v, n));
            r
        }
        pub unsafe extern "system" fn $get(this: *mut Com, start: u32, out: *mut $t, n: u32) -> i32 {
            get_consts(&dev(this).state.$field, start, out, n)
        }
    };
}

consts!(Dev_SetVertexShaderConstantF, Dev_GetVertexShaderConstantF, vs_f, [f32; 4], M_VS_CONST);
consts!(Dev_SetVertexShaderConstantI, Dev_GetVertexShaderConstantI, vs_i, [i32; 4], M_VS_CONST);
consts!(Dev_SetVertexShaderConstantB, Dev_GetVertexShaderConstantB, vs_b, i32, M_VS_CONST);
consts!(Dev_SetPixelShaderConstantF, Dev_GetPixelShaderConstantF, ps_f, [f32; 4], M_PS_CONST);
consts!(Dev_SetPixelShaderConstantI, Dev_GetPixelShaderConstantI, ps_i, [i32; 4], M_PS_CONST);
consts!(Dev_SetPixelShaderConstantB, Dev_GetPixelShaderConstantB, ps_b, i32, M_PS_CONST);

// -----| queries |-----

/// D3DQUERYTYPE: EVENT (8) and OCCLUSION (9).
pub unsafe extern "system" fn Dev_CreateQuery(this: *mut Com, t: u32, out: *mut *mut Com) -> i32 {
    if t != 8 && t != 9 {
        return D3DERR_NOTAVAILABLE;
    }
    if out.is_null() {
        return S_OK;
    }
    unsafe { *out = Com::new(&vtbl::IDIRECT3DQUERY9, &QUERY_IIDS, this, Body::Query(t)) };
    S_OK
}

pub unsafe extern "system" fn Query_GetType(this: *mut Com) -> u32 {
    match unsafe { &(*this).body } {
        Body::Query(t) => *t,
        _ => 0,
    }
}

pub unsafe extern "system" fn Query_GetDataSize(this: *mut Com) -> u32 {
    if unsafe { Query_GetType(this) } == 8 { 4 } else { 4 }
}

pub unsafe extern "system" fn Query_Issue(this: *mut Com, _flags: u32) -> i32 {
    dev(unsafe { (*this).device }).flush();
    S_OK
}

/// Everything is done once flushed: an event has happened; an occlusion
/// query (not counted yet) answers that much was drawn.
pub unsafe extern "system" fn Query_GetData(this: *mut Com, out: *mut u8, size: u32, _flags: u32) -> i32 {
    if !out.is_null() && size >= 4 {
        let v: u32 = if unsafe { Query_GetType(this) } == 8 { 1 } else { 0x7FFF_FFFF };
        unsafe { core::ptr::write_unaligned(out as *mut u32, v) };
    }
    S_OK
}

// -----| the swap chain |-----

pub unsafe extern "system" fn SC_Present(this: *mut Com, src: *const [i32; 4], dst: *const [i32; 4], hwnd: usize, dirty: usize, _flags: u32) -> i32 {
    unsafe { crate::draw::Dev_Present((*this).device, src, dst, hwnd, dirty) }
}

pub unsafe extern "system" fn SC_GetFrontBufferData(this: *mut Com, dst: *mut Com) -> i32 {
    unsafe { Dev_GetFrontBufferData((*this).device, 0, dst) }
}

pub unsafe extern "system" fn SC_GetBackBuffer(this: *mut Com, i: u32, t: u32, out: *mut *mut Com) -> i32 {
    unsafe { Dev_GetBackBuffer((*this).device, 0, i, t, out) }
}

pub unsafe extern "system" fn SC_GetRasterStatus(this: *mut Com, out: *mut [u32; 2]) -> i32 {
    unsafe { Dev_GetRasterStatus((*this).device, 0, out) }
}

pub unsafe extern "system" fn SC_GetDisplayMode(this: *mut Com, out: *mut [u32; 4]) -> i32 {
    unsafe { Dev_GetDisplayMode((*this).device, 0, out) }
}

pub unsafe extern "system" fn SC_GetPresentParameters(this: *mut Com, out: *mut Params) -> i32 {
    put(out, dev(unsafe { (*this).device }).params)
}
