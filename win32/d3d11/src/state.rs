//! State objects (blend, depth-stencil, rasterizer, sampler), input
//! layouts and queries.

use alloc::boxed::Box;
use alloc::ffi::CString;
use alloc::vec::Vec;

use crate::com::*;
use crate::vtbl::*;

/// D3D11_RENDER_TARGET_BLEND_DESC.
#[repr(C)]
#[derive(Clone, Copy, Default, PartialEq)]
pub struct TargetBlend {
    pub enable: i32,
    pub src: u32,
    pub dst: u32,
    pub op: u32,
    pub src_a: u32,
    pub dst_a: u32,
    pub op_a: u32,
    pub mask: u8,
    _pad: [u8; 3],
}

/// D3D11_BLEND_DESC.
#[repr(C)]
#[derive(Clone, Copy, Default, PartialEq)]
pub struct BlendDesc {
    pub alpha_to_coverage: i32,
    pub independent: i32,
    pub rt: [TargetBlend; 8],
}

/// D3D11_DEPTH_STENCILOP_DESC.
#[repr(C)]
#[derive(Clone, Copy, Default, PartialEq)]
pub struct StencilOps {
    pub fail: u32,
    pub depth_fail: u32,
    pub pass: u32,
    pub func: u32,
}

/// D3D11_DEPTH_STENCIL_DESC.
#[repr(C)]
#[derive(Clone, Copy, Default, PartialEq)]
pub struct DepthStencilDesc {
    pub depth: i32,
    pub write_mask: u32,
    pub func: u32,
    pub stencil: i32,
    pub read: u8,
    pub write: u8,
    _pad: [u8; 2],
    pub front: StencilOps,
    pub back: StencilOps,
}

/// D3D11_RASTERIZER_DESC.
#[repr(C)]
#[derive(Clone, Copy, Default, PartialEq)]
pub struct RasterizerDesc {
    pub fill: u32,
    pub cull: u32,
    pub front_ccw: i32,
    pub bias: i32,
    pub bias_clamp: f32,
    pub slope_bias: f32,
    pub depth_clip: i32,
    pub scissor: i32,
    pub multisample: i32,
    pub aa_lines: i32,
}

/// D3D11_SAMPLER_DESC.
#[repr(C)]
#[derive(Clone, Copy, Default, PartialEq)]
pub struct SamplerDesc {
    pub filter: u32,
    pub address: [u32; 3],
    pub lod_bias: f32,
    pub anisotropy: u32,
    pub compare: u32,
    pub border: [f32; 4],
    pub min_lod: f32,
    pub max_lod: f32,
}

pub enum StateObj {
    Blend(BlendDesc),
    DepthStencil(DepthStencilDesc),
    Rasterizer(RasterizerDesc),
    Sampler(SamplerDesc),
}

impl BlendDesc {
    /// Direct3D's default: no blending, every channel written.
    pub fn default_state() -> BlendDesc {
        let rt = TargetBlend { enable: 0, src: 2, dst: 1, op: 1, src_a: 2, dst_a: 1, op_a: 1, mask: 0xF, _pad: [0; 3] };
        BlendDesc { alpha_to_coverage: 0, independent: 0, rt: [rt; 8] }
    }
}

impl DepthStencilDesc {
    pub fn default_state() -> DepthStencilDesc {
        let ops = StencilOps { fail: 1, depth_fail: 1, pass: 1, func: 8 };
        DepthStencilDesc { depth: 1, write_mask: 1, func: 2, stencil: 0, read: 0xFF, write: 0xFF, _pad: [0; 2], front: ops, back: ops }
    }
}

impl RasterizerDesc {
    pub fn default_state() -> RasterizerDesc {
        RasterizerDesc { fill: 3, cull: 3, depth_clip: 1, ..Default::default() }
    }
}

impl SamplerDesc {
    pub fn default_state() -> SamplerDesc {
        SamplerDesc { filter: 0x15, address: [3; 3], lod_bias: 0.0, anisotropy: 1, compare: 1, border: [1.0; 4], min_lod: -f32::MAX, max_lod: f32::MAX }
    }
}

pub fn state<'a>(p: *mut Com) -> Option<&'a StateObj> {
    if p.is_null() {
        return None;
    }
    match body(p) {
        Body::State(s) => Some(s),
        _ => None,
    }
}

static BS_IIDS: [Guid; 2] = [IID_ID3D11DEVICECHILD, IID_ID3D11BLENDSTATE];
static DSS_IIDS: [Guid; 2] = [IID_ID3D11DEVICECHILD, IID_ID3D11DEPTHSTENCILSTATE];
static RS_IIDS: [Guid; 2] = [IID_ID3D11DEVICECHILD, IID_ID3D11RASTERIZERSTATE];
static SAMP_IIDS: [Guid; 2] = [IID_ID3D11DEVICECHILD, IID_ID3D11SAMPLERSTATE];

pub unsafe extern "system" fn Dev_CreateBlendState(this: *mut Com, d: *const BlendDesc, out: *mut *mut Com) -> i32 {
    if d.is_null() {
        return E_INVALIDARG;
    }
    let mut d = unsafe { *d };
    // Without independent blending every target blends as the first.
    if d.independent == 0 {
        d.rt = [d.rt[0]; 8];
    }
    if out.is_null() {
        return S_FALSE;
    }
    unsafe { *out = Com::new(&ID3D11BLENDSTATE, &BS_IIDS, main_of(this), Body::State(Box::new(StateObj::Blend(d)))) };
    S_OK
}

pub unsafe extern "system" fn Dev_CreateDepthStencilState(this: *mut Com, d: *const DepthStencilDesc, out: *mut *mut Com) -> i32 {
    if d.is_null() {
        return E_INVALIDARG;
    }
    if out.is_null() {
        return S_FALSE;
    }
    unsafe { *out = Com::new(&ID3D11DEPTHSTENCILSTATE, &DSS_IIDS, main_of(this), Body::State(Box::new(StateObj::DepthStencil(*d)))) };
    S_OK
}

pub unsafe extern "system" fn Dev_CreateRasterizerState(this: *mut Com, d: *const RasterizerDesc, out: *mut *mut Com) -> i32 {
    if d.is_null() {
        return E_INVALIDARG;
    }
    let r = unsafe { *d };
    if !(2..=3).contains(&r.fill) || !(1..=3).contains(&r.cull) {
        return E_INVALIDARG;
    }
    if out.is_null() {
        return S_FALSE;
    }
    unsafe { *out = Com::new(&ID3D11RASTERIZERSTATE, &RS_IIDS, main_of(this), Body::State(Box::new(StateObj::Rasterizer(r)))) };
    S_OK
}

pub unsafe extern "system" fn Dev_CreateSamplerState(this: *mut Com, d: *const SamplerDesc, out: *mut *mut Com) -> i32 {
    if d.is_null() {
        return E_INVALIDARG;
    }
    if out.is_null() {
        return S_FALSE;
    }
    unsafe { *out = Com::new(&ID3D11SAMPLERSTATE, &SAMP_IIDS, main_of(this), Body::State(Box::new(StateObj::Sampler(*d)))) };
    S_OK
}

pub unsafe extern "system" fn BS_GetDesc(this: *mut Com, out: *mut BlendDesc) {
    if let (Some(StateObj::Blend(d)), false) = (state(this), out.is_null()) {
        unsafe { *out = *d };
    }
}

pub unsafe extern "system" fn DSS_GetDesc(this: *mut Com, out: *mut DepthStencilDesc) {
    if let (Some(StateObj::DepthStencil(d)), false) = (state(this), out.is_null()) {
        unsafe { *out = *d };
    }
}

pub unsafe extern "system" fn RS_GetDesc(this: *mut Com, out: *mut RasterizerDesc) {
    if let (Some(StateObj::Rasterizer(d)), false) = (state(this), out.is_null()) {
        unsafe { *out = *d };
    }
}

pub unsafe extern "system" fn Samp_GetDesc(this: *mut Com, out: *mut SamplerDesc) {
    if let (Some(StateObj::Sampler(d)), false) = (state(this), out.is_null()) {
        unsafe { *out = *d };
    }
}

// -----| input layouts |-----

/// D3D11_INPUT_ELEMENT_DESC.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ElementDesc {
    pub name: *const u8,
    pub index: u32,
    pub format: u32,
    pub slot: u32,
    pub offset: u32,
    pub class: u32,
    pub step: u32,
}

#[derive(Clone)]
pub struct Element {
    /// The semantic, upper case.
    pub name: Vec<u8>,
    pub index: u32,
    pub format: u32,
    pub slot: u32,
    pub offset: u32,
    pub instanced: bool,
    pub step: u32,
}

pub struct InputLayout {
    pub elems: Vec<Element>,
}

static IL_IIDS: [Guid; 2] = [IID_ID3D11DEVICECHILD, IID_ID3D11INPUTLAYOUT];

/// D3D11_APPEND_ALIGNED_ELEMENT offsets follow the slot's last element.
pub unsafe extern "system" fn Dev_CreateInputLayout(this: *mut Com, descs: *const ElementDesc, n: u32, _code: *const u8, _len: usize, out: *mut *mut Com) -> i32 {
    if (descs.is_null() && n != 0) || n > 32 {
        return E_INVALIDARG;
    }
    let mut elems: Vec<Element> = Vec::new();
    let mut next = [0u32; 32];
    for i in 0..n as usize {
        let d = unsafe { *descs.add(i) };
        if d.name.is_null() || d.slot >= 32 {
            return E_INVALIDARG;
        }
        let name = unsafe { core::ffi::CStr::from_ptr(d.name as *const core::ffi::c_char) }.to_bytes().to_ascii_uppercase();
        let Some(size) = crate::format::bytes(d.format) else { return E_INVALIDARG };
        let offset = if d.offset == 0xFFFF_FFFF { next[d.slot as usize] } else { d.offset };
        next[d.slot as usize] = offset + size;
        elems.push(Element { name, index: d.index, format: d.format, slot: d.slot, offset, instanced: d.class == 1, step: d.step });
    }
    let _ = CString::default();
    if out.is_null() {
        return S_FALSE;
    }
    unsafe { *out = Com::new(&ID3D11INPUTLAYOUT, &IL_IIDS, main_of(this), Body::InputLayout(Box::new(InputLayout { elems }))) };
    S_OK
}

// -----| queries |-----

static QUERY_IIDS: [Guid; 3] = [IID_ID3D11DEVICECHILD, IID_ID3D11ASYNCHRONOUS, IID_ID3D11QUERY];
static PRED_IIDS: [Guid; 4] = [IID_ID3D11DEVICECHILD, IID_ID3D11ASYNCHRONOUS, IID_ID3D11QUERY, IID_ID3D11PREDICATE];

/// D3D11_QUERY: event, occlusion, timestamps, pipeline and stream-output
/// statistics, the predicates.
fn data_size(q: u32) -> u32 {
    match q {
        0 | 5 | 7 => 4,
        1 | 2 => 8,
        3 => 16,
        4 => 88,
        6 => 16,
        _ => 0,
    }
}

pub unsafe extern "system" fn Dev_CreateQuery(this: *mut Com, d: *const [u32; 2], out: *mut *mut Com) -> i32 {
    if d.is_null() {
        return E_INVALIDARG;
    }
    let q = unsafe { (*d)[0] };
    if q > 7 {
        return E_INVALIDARG;
    }
    if out.is_null() {
        return S_FALSE;
    }
    unsafe { *out = Com::new(&ID3D11QUERY, &QUERY_IIDS, main_of(this), Body::Query(q)) };
    S_OK
}

pub unsafe extern "system" fn Dev_CreatePredicate(this: *mut Com, d: *const [u32; 2], out: *mut *mut Com) -> i32 {
    if d.is_null() {
        return E_INVALIDARG;
    }
    let q = unsafe { (*d)[0] };
    if q != 5 && q != 7 {
        return E_INVALIDARG;
    }
    if out.is_null() {
        return S_FALSE;
    }
    unsafe { *out = Com::new(&ID3D11PREDICATE, &PRED_IIDS, main_of(this), Body::Query(q)) };
    S_OK
}

fn query_kind(p: *mut Com) -> u32 {
    match body(p) {
        Body::Query(q) => *q,
        _ => 0,
    }
}

pub unsafe extern "system" fn Async_GetDataSize(this: *mut Com) -> u32 {
    data_size(query_kind(this))
}

pub unsafe extern "system" fn Query_GetDesc(this: *mut Com, out: *mut [u32; 2]) {
    if !out.is_null() {
        unsafe { *out = [query_kind(this), 0] };
    }
}

pub unsafe extern "system" fn Ctx_Begin(_this: *mut Com, _q: *mut Com) {}

pub unsafe extern "system" fn Ctx_End(this: *mut Com, _q: *mut Com) {
    crate::device::of(main_of(this)).flush();
}

/// Everything is done once flushed: events have happened, occlusion
/// counts as visible, predicates pass, timestamps count microseconds.
pub unsafe extern "system" fn Ctx_GetData(_this: *mut Com, q: *mut Com, data: *mut u8, size: u32, _flags: u32) -> i32 {
    if q.is_null() {
        return E_INVALIDARG;
    }
    let k = query_kind(q);
    if data.is_null() || size == 0 {
        return S_OK;
    }
    if size < data_size(k) {
        return E_INVALIDARG;
    }
    let mut b = [0u8; 88];
    match k {
        0 | 5 => b[..4].copy_from_slice(&1u32.to_le_bytes()),
        1 => b[..8].copy_from_slice(&1_000_000u64.to_le_bytes()),
        3 => b[..8].copy_from_slice(&1_000_000u64.to_le_bytes()),
        _ => {}
    }
    unsafe { core::ptr::copy_nonoverlapping(b.as_ptr(), data, data_size(k) as usize) };
    S_OK
}
