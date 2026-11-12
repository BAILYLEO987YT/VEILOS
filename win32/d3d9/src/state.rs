//! The device's state (render, texture stage and sampler states, textures,
//! transforms, lights, streams...), its defaults, and state blocks: a copy
//! of some of it, with which parts (`Mask`), captured and applied.

use alloc::boxed::Box;
use alloc::vec;
use alloc::vec::Vec;

use crate::com::*;
use crate::vtbl;

pub const RS_COUNT: usize = 256;
pub const TSS_COUNT: usize = 33;
pub const SS_COUNT: usize = 14;
/// Samplers 0-15, the displacement map sampler, vertex samplers 0-3.
pub const SAMPLERS: usize = 21;
/// VIEW, PROJECTION, TEXTURE0-7, WORLDMATRIX(0-255).
pub const TRANSFORMS: usize = 266;

/// D3DRS_*.
#[allow(dead_code)]
pub mod rs {
    pub const ZENABLE: usize = 7;
    pub const FILLMODE: usize = 8;
    pub const SHADEMODE: usize = 9;
    pub const ZWRITEENABLE: usize = 14;
    pub const ALPHATESTENABLE: usize = 15;
    pub const LASTPIXEL: usize = 16;
    pub const SRCBLEND: usize = 19;
    pub const DESTBLEND: usize = 20;
    pub const CULLMODE: usize = 22;
    pub const ZFUNC: usize = 23;
    pub const ALPHAREF: usize = 24;
    pub const ALPHAFUNC: usize = 25;
    pub const DITHERENABLE: usize = 26;
    pub const ALPHABLENDENABLE: usize = 27;
    pub const FOGENABLE: usize = 28;
    pub const SPECULARENABLE: usize = 29;
    pub const FOGCOLOR: usize = 34;
    pub const FOGTABLEMODE: usize = 35;
    pub const FOGSTART: usize = 36;
    pub const FOGEND: usize = 37;
    pub const FOGDENSITY: usize = 38;
    pub const RANGEFOGENABLE: usize = 48;
    pub const STENCILENABLE: usize = 52;
    pub const STENCILFAIL: usize = 53;
    pub const STENCILZFAIL: usize = 54;
    pub const STENCILPASS: usize = 55;
    pub const STENCILFUNC: usize = 56;
    pub const STENCILREF: usize = 57;
    pub const STENCILMASK: usize = 58;
    pub const STENCILWRITEMASK: usize = 59;
    pub const TEXTUREFACTOR: usize = 60;
    pub const CLIPPING: usize = 136;
    pub const LIGHTING: usize = 137;
    pub const AMBIENT: usize = 139;
    pub const FOGVERTEXMODE: usize = 140;
    pub const COLORVERTEX: usize = 141;
    pub const LOCALVIEWER: usize = 142;
    pub const NORMALIZENORMALS: usize = 143;
    pub const DIFFUSEMATERIALSOURCE: usize = 145;
    pub const SPECULARMATERIALSOURCE: usize = 146;
    pub const AMBIENTMATERIALSOURCE: usize = 147;
    pub const EMISSIVEMATERIALSOURCE: usize = 148;
    pub const VERTEXBLEND: usize = 151;
    pub const CLIPPLANEENABLE: usize = 152;
    pub const POINTSIZE: usize = 154;
    pub const POINTSIZE_MIN: usize = 155;
    pub const POINTSPRITEENABLE: usize = 156;
    pub const POINTSCALEENABLE: usize = 157;
    pub const POINTSCALE_A: usize = 158;
    pub const POINTSCALE_B: usize = 159;
    pub const POINTSCALE_C: usize = 160;
    pub const MULTISAMPLEANTIALIAS: usize = 161;
    pub const MULTISAMPLEMASK: usize = 162;
    pub const DEBUGMONITORTOKEN: usize = 165;
    pub const POINTSIZE_MAX: usize = 166;
    pub const COLORWRITEENABLE: usize = 168;
    pub const BLENDOP: usize = 171;
    pub const POSITIONDEGREE: usize = 172;
    pub const NORMALDEGREE: usize = 173;
    pub const SCISSORTESTENABLE: usize = 174;
    pub const SLOPESCALEDEPTHBIAS: usize = 175;
    pub const MINTESSELLATIONLEVEL: usize = 178;
    pub const MAXTESSELLATIONLEVEL: usize = 179;
    pub const ADAPTIVETESS_W: usize = 183;
    pub const TWOSIDEDSTENCILMODE: usize = 185;
    pub const CCW_STENCILFAIL: usize = 186;
    pub const CCW_STENCILZFAIL: usize = 187;
    pub const CCW_STENCILPASS: usize = 188;
    pub const CCW_STENCILFUNC: usize = 189;
    pub const COLORWRITEENABLE1: usize = 190;
    pub const COLORWRITEENABLE2: usize = 191;
    pub const COLORWRITEENABLE3: usize = 192;
    pub const BLENDFACTOR: usize = 193;
    pub const DEPTHBIAS: usize = 195;
    pub const SRCBLENDALPHA: usize = 207;
    pub const DESTBLENDALPHA: usize = 208;
    pub const BLENDOPALPHA: usize = 209;
}

/// D3DTSS_*.
pub mod tss {
    pub const COLOROP: usize = 1;
    pub const COLORARG1: usize = 2;
    pub const COLORARG2: usize = 3;
    pub const ALPHAOP: usize = 4;
    pub const ALPHAARG1: usize = 5;
    pub const ALPHAARG2: usize = 6;
    pub const TEXCOORDINDEX: usize = 11;
    pub const TEXTURETRANSFORMFLAGS: usize = 24;
    pub const COLORARG0: usize = 26;
    pub const ALPHAARG0: usize = 27;
    pub const RESULTARG: usize = 28;
    pub const CONSTANT: usize = 32;
}

/// D3DSAMP_*.
pub mod ss {
    pub const ADDRESSU: usize = 1;
    pub const ADDRESSV: usize = 2;
    pub const ADDRESSW: usize = 3;
    pub const BORDERCOLOR: usize = 4;
    pub const MAGFILTER: usize = 5;
    pub const MINFILTER: usize = 6;
    pub const MIPFILTER: usize = 7;
    pub const MIPMAPLODBIAS: usize = 8;
    pub const MAXMIPLEVEL: usize = 9;
    pub const MAXANISOTROPY: usize = 10;
}

/// D3DLIGHT9.
#[repr(C)]
#[derive(Clone, Copy, Default, PartialEq)]
pub struct Light {
    pub ltype: u32,
    pub diffuse: [f32; 4],
    pub specular: [f32; 4],
    pub ambient: [f32; 4],
    pub position: [f32; 3],
    pub direction: [f32; 3],
    pub range: f32,
    pub falloff: f32,
    pub att: [f32; 3],
    pub theta: f32,
    pub phi: f32,
}

/// D3DMATERIAL9.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Material {
    pub diffuse: [f32; 4],
    pub ambient: [f32; 4],
    pub specular: [f32; 4],
    pub emissive: [f32; 4],
    pub power: f32,
}

/// D3DVIEWPORT9.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Viewport {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
    pub min_z: f32,
    pub max_z: f32,
}

#[derive(Clone, Copy)]
pub struct Stream {
    pub buffer: *mut Com,
    pub offset: u32,
    pub stride: u32,
    pub freq: u32,
}

pub const IDENTITY: [f32; 16] = [1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0];

#[derive(Clone)]
pub struct State {
    pub rs: [u32; RS_COUNT],
    pub tss: [[u32; TSS_COUNT]; 8],
    pub ss: [[u32; SS_COUNT]; SAMPLERS],
    pub textures: [*mut Com; SAMPLERS],
    pub transforms: Vec<[f32; 16]>,
    pub viewport: Viewport,
    pub material: Material,
    /// Each light index set: the light and whether it's enabled.
    pub lights: Vec<Option<(Light, bool)>>,
    pub clip_planes: [[f32; 4]; 6],
    pub streams: [Stream; 16],
    pub indices: *mut Com,
    pub decl: *mut Com,
    pub fvf: u32,
    pub scissor: [i32; 4],
    pub vs: *mut Com,
    pub ps: *mut Com,
    pub vs_f: Vec<[f32; 4]>,
    pub vs_i: [[i32; 4]; 16],
    pub vs_b: [i32; 16],
    pub ps_f: Vec<[f32; 4]>,
    pub ps_i: [[i32; 4]; 16],
    pub ps_b: [i32; 16],
    pub npatch: f32,
}

fn f(v: f32) -> u32 {
    v.to_bits()
}

impl State {
    /// Direct3D's defaults; `depth`: the device has a depth buffer.
    pub fn new(depth: bool, w: u32, h: u32) -> State {
        let mut r = [0u32; RS_COUNT];
        r[rs::ZENABLE] = depth as u32;
        r[rs::FILLMODE] = 3;
        r[rs::SHADEMODE] = 2;
        r[rs::ZWRITEENABLE] = 1;
        r[rs::LASTPIXEL] = 1;
        r[rs::SRCBLEND] = 2;
        r[rs::DESTBLEND] = 1;
        r[rs::CULLMODE] = 3;
        r[rs::ZFUNC] = 4;
        r[rs::ALPHAFUNC] = 8;
        r[rs::FOGEND] = f(1.0);
        r[rs::FOGDENSITY] = f(1.0);
        r[rs::STENCILFAIL] = 1;
        r[rs::STENCILZFAIL] = 1;
        r[rs::STENCILPASS] = 1;
        r[rs::STENCILFUNC] = 8;
        r[rs::STENCILMASK] = 0xFFFF_FFFF;
        r[rs::STENCILWRITEMASK] = 0xFFFF_FFFF;
        r[rs::TEXTUREFACTOR] = 0xFFFF_FFFF;
        r[rs::CLIPPING] = 1;
        r[rs::LIGHTING] = 1;
        r[rs::COLORVERTEX] = 1;
        r[rs::LOCALVIEWER] = 1;
        r[rs::DIFFUSEMATERIALSOURCE] = 1;
        r[rs::SPECULARMATERIALSOURCE] = 2;
        r[rs::POINTSIZE] = f(1.0);
        r[rs::POINTSIZE_MIN] = f(1.0);
        r[rs::POINTSCALE_A] = f(1.0);
        r[rs::MULTISAMPLEANTIALIAS] = 1;
        r[rs::MULTISAMPLEMASK] = 0xFFFF_FFFF;
        r[rs::DEBUGMONITORTOKEN] = 0xBAAD_CAFE;
        r[rs::POINTSIZE_MAX] = f(64.0);
        r[rs::COLORWRITEENABLE] = 0xF;
        r[rs::BLENDOP] = 1;
        r[rs::POSITIONDEGREE] = 3;
        r[rs::NORMALDEGREE] = 1;
        r[rs::MINTESSELLATIONLEVEL] = f(1.0);
        r[rs::MAXTESSELLATIONLEVEL] = f(1.0);
        r[rs::ADAPTIVETESS_W] = f(1.0);
        r[rs::CCW_STENCILFAIL] = 1;
        r[rs::CCW_STENCILZFAIL] = 1;
        r[rs::CCW_STENCILPASS] = 1;
        r[rs::CCW_STENCILFUNC] = 8;
        r[rs::COLORWRITEENABLE1] = 0xF;
        r[rs::COLORWRITEENABLE2] = 0xF;
        r[rs::COLORWRITEENABLE3] = 0xF;
        r[rs::BLENDFACTOR] = 0xFFFF_FFFF;
        r[rs::SRCBLENDALPHA] = 2;
        r[rs::DESTBLENDALPHA] = 1;
        r[rs::BLENDOPALPHA] = 1;
        let mut t = [[0u32; TSS_COUNT]; 8];
        for (i, s) in t.iter_mut().enumerate() {
            s[tss::COLOROP] = if i == 0 { 4 } else { 1 };
            s[tss::ALPHAOP] = if i == 0 { 2 } else { 1 };
            s[tss::COLORARG1] = 2;
            s[tss::COLORARG2] = 1;
            s[tss::ALPHAARG1] = 2;
            s[tss::ALPHAARG2] = 1;
            s[tss::COLORARG0] = 1;
            s[tss::ALPHAARG0] = 1;
            s[tss::RESULTARG] = 1;
            s[tss::TEXCOORDINDEX] = i as u32;
        }
        let mut s = [[0u32; SS_COUNT]; SAMPLERS];
        for x in s.iter_mut() {
            x[ss::ADDRESSU] = 1;
            x[ss::ADDRESSV] = 1;
            x[ss::ADDRESSW] = 1;
            x[ss::MAGFILTER] = 1;
            x[ss::MINFILTER] = 1;
            x[ss::MAXANISOTROPY] = 1;
        }
        State {
            rs: r,
            tss: t,
            ss: s,
            textures: [core::ptr::null_mut(); SAMPLERS],
            transforms: vec![IDENTITY; TRANSFORMS],
            viewport: Viewport { x: 0, y: 0, w, h, min_z: 0.0, max_z: 1.0 },
            material: Material::default(),
            lights: Vec::new(),
            clip_planes: [[0.0; 4]; 6],
            streams: [Stream { buffer: core::ptr::null_mut(), offset: 0, stride: 0, freq: 1 }; 16],
            indices: core::ptr::null_mut(),
            decl: core::ptr::null_mut(),
            fvf: 0,
            scissor: [0, 0, w as i32, h as i32],
            vs: core::ptr::null_mut(),
            ps: core::ptr::null_mut(),
            vs_f: vec![[0.0; 4]; 256],
            vs_i: [[0; 4]; 16],
            vs_b: [0; 16],
            ps_f: vec![[0.0; 4]; 224],
            ps_i: [[0; 4]; 16],
            ps_b: [0; 16],
            npatch: 0.0,
        }
    }

    pub fn rsf(&self, i: usize) -> f32 {
        f32::from_bits(self.rs[i])
    }
}

/// The index into `State::transforms` of a D3DTRANSFORMSTATETYPE.
pub fn transform_index(t: u32) -> Option<usize> {
    match t {
        2 => Some(0),
        3 => Some(1),
        16..=23 => Some(2 + (t - 16) as usize),
        256..=511 => Some(10 + (t - 256) as usize),
        _ => None,
    }
}

/// The index into `State::ss` and `textures` of a sampler number.
pub fn sampler_index(s: u32) -> Option<usize> {
    match s {
        0..=15 => Some(s as usize),
        256..=260 => Some(16 + (s - 256) as usize),
        _ => None,
    }
}

// -----| masks: which parts a state block holds |-----

pub const M_RS: usize = 0;
pub const M_TSS: usize = M_RS + RS_COUNT;
pub const M_SS: usize = M_TSS + 8 * TSS_COUNT;
pub const M_TEX: usize = M_SS + SAMPLERS * SS_COUNT;
pub const M_TRANSFORM: usize = M_TEX + SAMPLERS;
pub const M_VIEWPORT: usize = M_TRANSFORM + TRANSFORMS;
pub const M_MATERIAL: usize = M_VIEWPORT + 1;
pub const M_CLIP: usize = M_MATERIAL + 1;
pub const M_STREAM: usize = M_CLIP + 6;
pub const M_INDICES: usize = M_STREAM + 16;
pub const M_DECL: usize = M_INDICES + 1;
pub const M_SCISSOR: usize = M_DECL + 1;
pub const M_VS: usize = M_SCISSOR + 1;
pub const M_PS: usize = M_VS + 1;
pub const M_VS_CONST: usize = M_PS + 1;
pub const M_PS_CONST: usize = M_VS_CONST + 1;
pub const M_NPATCH: usize = M_PS_CONST + 1;
pub const M_COUNT: usize = M_NPATCH + 1;

#[derive(Clone)]
pub struct Mask {
    pub on: Vec<bool>,
    /// Light indices (their parameters and enables).
    pub lights: Vec<usize>,
    /// Every light the device has when applied (D3DSBT_ALL and VERTEXSTATE).
    pub all_lights: bool,
}

impl Mask {
    pub fn none() -> Mask {
        Mask { on: vec![false; M_COUNT], lights: Vec::new(), all_lights: false }
    }

    pub fn set(&mut self, i: usize) {
        self.on[i] = true;
    }

    pub fn light(&mut self, i: usize) {
        if !self.lights.contains(&i) {
            self.lights.push(i);
        }
    }

    /// D3DSBT_ALL (1), D3DSBT_PIXELSTATE (2), D3DSBT_VERTEXSTATE (3).
    pub fn of_type(t: u32) -> Mask {
        let mut m = Mask::none();
        if t == 1 {
            m.on.fill(true);
            m.all_lights = true;
            return m;
        }
        const PIXEL_RS: &[usize] = &[
            7, 8, 9, 14, 15, 16, 19, 20, 23, 24, 25, 26, 36, 37, 38, 27, 195, 52, 53, 54, 55, 56, 57, 58, 59, 60, 128, 129, 130, 131, 132, 133, 134, 135, 168, 171,
            174, 175, 176, 185, 186, 187, 188, 189, 190, 191, 192, 193, 194, 198, 199, 200, 201, 202, 203, 204, 205, 206, 207, 208, 209,
        ];
        const VERTEX_RS: &[usize] = &[
            22, 28, 34, 35, 36, 37, 38, 48, 139, 141, 140, 136, 137, 143, 142, 148, 147, 145, 146, 151, 152, 154, 155, 156, 157, 158, 159, 160, 161, 162,
            163, 166, 167, 170, 172, 173, 178, 179, 180, 181, 182, 183, 184, 29,
        ];
        if t == 2 {
            for &r in PIXEL_RS {
                m.set(M_RS + r);
            }
            for s in 0..8 {
                for n in 0..TSS_COUNT {
                    if n != tss::TEXCOORDINDEX && n != tss::TEXTURETRANSFORMFLAGS {
                        m.set(M_TSS + s * TSS_COUNT + n);
                    }
                }
            }
            for s in 0..16 {
                for n in 0..SS_COUNT {
                    m.set(M_SS + s * SS_COUNT + n);
                }
            }
            m.set(M_PS);
            m.set(M_PS_CONST);
        } else if t == 3 {
            for &r in VERTEX_RS {
                m.set(M_RS + r);
            }
            for s in 0..8 {
                m.set(M_TSS + s * TSS_COUNT + tss::TEXCOORDINDEX);
                m.set(M_TSS + s * TSS_COUNT + tss::TEXTURETRANSFORMFLAGS);
            }
            for s in 16..SAMPLERS {
                m.set(M_SS + s * SS_COUNT + 13);
            }
            m.set(M_DECL);
            m.set(M_VS);
            m.set(M_VS_CONST);
            m.set(M_NPATCH);
            m.all_lights = true;
        }
        m
    }
}

/// Copy the parts of `from` that `m` names onto `to`; `counted`: `to` holds
/// references on what's bound in it (the device's state does).
pub fn copy(from: &State, to: &mut State, m: &Mask, counted: bool) {
    let put = |slot: &mut *mut Com, v: *mut Com| {
        if counted {
            unsafe { set_ref(slot, v) };
        } else {
            *slot = v;
        }
    };
    for i in 0..RS_COUNT {
        if m.on[M_RS + i] {
            to.rs[i] = from.rs[i];
        }
    }
    for s in 0..8 {
        for n in 0..TSS_COUNT {
            if m.on[M_TSS + s * TSS_COUNT + n] {
                to.tss[s][n] = from.tss[s][n];
            }
        }
    }
    for s in 0..SAMPLERS {
        for n in 0..SS_COUNT {
            if m.on[M_SS + s * SS_COUNT + n] {
                to.ss[s][n] = from.ss[s][n];
            }
        }
        if m.on[M_TEX + s] {
            put(&mut to.textures[s], from.textures[s]);
        }
    }
    for i in 0..TRANSFORMS {
        if m.on[M_TRANSFORM + i] {
            to.transforms[i] = from.transforms[i];
        }
    }
    if m.on[M_VIEWPORT] {
        to.viewport = from.viewport;
    }
    if m.on[M_MATERIAL] {
        to.material = from.material;
    }
    for i in 0..6 {
        if m.on[M_CLIP + i] {
            to.clip_planes[i] = from.clip_planes[i];
        }
    }
    for i in 0..16 {
        if m.on[M_STREAM + i] {
            put(&mut to.streams[i].buffer, from.streams[i].buffer);
            let s = from.streams[i];
            (to.streams[i].offset, to.streams[i].stride, to.streams[i].freq) = (s.offset, s.stride, s.freq);
        }
    }
    if m.on[M_INDICES] {
        put(&mut to.indices, from.indices);
    }
    if m.on[M_DECL] {
        put(&mut to.decl, from.decl);
        to.fvf = from.fvf;
    }
    if m.on[M_SCISSOR] {
        to.scissor = from.scissor;
    }
    if m.on[M_VS] {
        put(&mut to.vs, from.vs);
    }
    if m.on[M_PS] {
        put(&mut to.ps, from.ps);
    }
    if m.on[M_VS_CONST] {
        to.vs_f.clone_from(&from.vs_f);
        (to.vs_i, to.vs_b) = (from.vs_i, from.vs_b);
    }
    if m.on[M_PS_CONST] {
        to.ps_f.clone_from(&from.ps_f);
        (to.ps_i, to.ps_b) = (from.ps_i, from.ps_b);
    }
    if m.on[M_NPATCH] {
        to.npatch = from.npatch;
    }
    let lights: Vec<usize> = if m.all_lights { (0..from.lights.len().max(to.lights.len())).collect() } else { m.lights.clone() };
    for i in lights {
        if let Some(Some(l)) = from.lights.get(i) {
            if to.lights.len() <= i {
                to.lights.resize(i + 1, None);
            }
            to.lights[i] = Some(*l);
        }
    }
}

pub struct StateBlock {
    pub state: State,
    pub mask: Mask,
}

static SB_IIDS: [Guid; 1] = [IID_STATEBLOCK9];

pub fn new_block(dev: *mut Com, state: State, mask: Mask) -> *mut Com {
    Com::new(&vtbl::IDIRECT3DSTATEBLOCK9, &SB_IIDS, dev, Body::StateBlock(Box::new(StateBlock { state, mask })))
}

fn block<'a>(this: *mut Com) -> &'a mut StateBlock {
    match unsafe { &mut (*this).body } {
        Body::StateBlock(b) => b,
        _ => panic!(),
    }
}

pub unsafe extern "system" fn SB_Capture(this: *mut Com) -> i32 {
    let b = block(this);
    let d = unsafe { device((*this).device) };
    copy(&d.state, &mut b.state, &b.mask, false);
    S_OK
}

pub unsafe extern "system" fn SB_Apply(this: *mut Com) -> i32 {
    let b = block(this);
    let d = unsafe { device((*this).device) };
    let mut s = core::mem::replace(&mut d.state, State::new(false, 0, 0));
    copy(&b.state, &mut s, &b.mask, true);
    d.state = s;
    S_OK
}
