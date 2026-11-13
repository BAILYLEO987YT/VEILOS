//! Shader objects: the bytecode parsed (dxbc.rs); a pixel shader also
//! translated to the kernel's program (ps.rs) and made there.

use alloc::boxed::Box;
use alloc::vec::Vec;

use gfx3d::raster3d::PsArgs;

use crate::com::*;
use crate::vtbl::*;

pub struct ShaderObj {
    pub code: Vec<u8>,
    pub sh: crate::dxbc::Shader,
    /// A pixel shader's translation and its kernel program.
    pub ps: Option<crate::ps::PsCode>,
    pub kernel: u64,
}

impl Drop for ShaderObj {
    fn drop(&mut self) {
        crate::res::close_later(self.kernel);
    }
}

pub fn of<'a>(p: *mut Com) -> Option<&'a ShaderObj> {
    if p.is_null() {
        return None;
    }
    match body(p) {
        Body::Shader(s) => Some(s),
        _ => None,
    }
}

static VS_IIDS: [Guid; 2] = [IID_ID3D11DEVICECHILD, IID_ID3D11VERTEXSHADER];
static GS_IIDS: [Guid; 2] = [IID_ID3D11DEVICECHILD, IID_ID3D11GEOMETRYSHADER];
static PS_IIDS: [Guid; 2] = [IID_ID3D11DEVICECHILD, IID_ID3D11PIXELSHADER];

/// Make a shader of `kind` (0 pixel, 1 vertex, 2 geometry) from bytecode.
unsafe fn make(this: *mut Com, code: *const u8, len: usize, kind: u32, out: *mut *mut Com) -> i32 {
    if code.is_null() || len < 32 {
        return E_INVALIDARG;
    }
    let bytes = unsafe { core::slice::from_raw_parts(code, len) }.to_vec();
    let Some(sh) = crate::dxbc::parse(&bytes) else { return E_INVALIDARG };
    if sh.kind != kind {
        return E_INVALIDARG;
    }
    // Shader model 5 needs feature level 11_0.
    let dev = main_of(this);
    if sh.major >= 5 && crate::device::of(dev).level < 0xB000 {
        return E_INVALIDARG;
    }
    if kind == 1 && crate::vm::Vm::new(&sh).is_none() {
        return E_INVALIDARG;
    }
    let mut ps = None;
    let mut kernel = 0u64;
    if kind == 0 {
        let Some(code) = crate::ps::translate(&sh) else {
            crate::com::unimplemented(b"pixel shader the kernel can't run\0");
            return E_INVALIDARG;
        };
        let args = PsArgs { code: code.ins.as_ptr() as u64, count: code.ins.len() as u32, inputs: code.inputs.len() as u32 };
        if gfx3d::gpu3d(7, 0, &args as *const _ as u64, &mut kernel as *mut u64 as u64) != 0 {
            crate::com::unimplemented(b"pixel shader the kernel refused\0");
            return E_INVALIDARG;
        }
        ps = Some(code);
    }
    if out.is_null() {
        crate::res::close_later(kernel);
        return S_FALSE;
    }
    let (vt, iids): (&'static Vtbl<7>, &'static [Guid]) = match kind {
        0 => (&ID3D11PIXELSHADER, &PS_IIDS),
        1 => (&ID3D11VERTEXSHADER, &VS_IIDS),
        _ => (&ID3D11GEOMETRYSHADER, &GS_IIDS),
    };
    unsafe { *out = Com::new(vt, iids, dev, Body::Shader(Box::new(ShaderObj { code: bytes, sh, ps, kernel }))) };
    S_OK
}

pub unsafe extern "system" fn Dev_CreateVertexShader(this: *mut Com, code: *const u8, len: usize, _link: *mut Com, out: *mut *mut Com) -> i32 {
    unsafe { make(this, code, len, 1, out) }
}

pub unsafe extern "system" fn Dev_CreatePixelShader(this: *mut Com, code: *const u8, len: usize, _link: *mut Com, out: *mut *mut Com) -> i32 {
    unsafe { make(this, code, len, 0, out) }
}

/// Geometry shaders are made (and kept) but don't run yet (G5f2).
pub unsafe extern "system" fn Dev_CreateGeometryShader(this: *mut Com, code: *const u8, len: usize, _link: *mut Com, out: *mut *mut Com) -> i32 {
    unsafe { make(this, code, len, 2, out) }
}
