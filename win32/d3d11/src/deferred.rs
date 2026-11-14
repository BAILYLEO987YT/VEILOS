//! Deferred contexts and command lists: a deferred context keeps its own
//! bindings, and what would act (draws, dispatches, clears, copies,
//! updates, maps) is recorded with the bindings it would act with.
//! FinishCommandList hands the record over as a command list;
//! ExecuteCommandList replays it on the immediate context, each command
//! under its own bindings, and then puts the immediate context's back (or
//! clears them).

use alloc::boxed::Box;
use alloc::vec::Vec;

use crate::com::*;
use crate::context::Bindings;
use crate::res::{is_resource, resource, Box3};
use crate::vtbl::*;

/// A command, holding a reference on every object it names.
pub enum Op {
    Draw(u32, u32, u32, u32),
    DrawIndexed(u32, u32, u32, i32, u32),
    DrawAuto,
    DrawIndirect(bool, *mut Com, u32),
    Dispatch(u32, u32, u32),
    DispatchIndirect(*mut Com, u32),
    ClearRtv(*mut Com, [f32; 4]),
    ClearDsv(*mut Com, u32, f32, u8),
    ClearUavUint(*mut Com, [u32; 4]),
    ClearUavFloat(*mut Com, [f32; 4]),
    Copy(*mut Com, *mut Com),
    CopyRegion(*mut Com, u32, [u32; 3], *mut Com, u32, Option<Box3>),
    Update(*mut Com, u32, Option<Box3>, Vec<u8>, u32, u32),
    Write(*mut Com, u32, Vec<u8>),
    GenerateMips(*mut Com),
    Resolve(*mut Com, u32, *mut Com, u32, u32),
    Begin(*mut Com),
    End(*mut Com),
    Execute(*mut Com, bool),
    ClearState,
}

impl Op {
    fn refs(&self) -> Vec<*mut Com> {
        use Op::*;
        match self {
            DrawIndirect(_, a, _) | DispatchIndirect(a, _) | ClearRtv(a, _) | ClearDsv(a, ..) | ClearUavUint(a, _) | ClearUavFloat(a, _) | Update(a, ..) | Write(a, ..) | GenerateMips(a) | Begin(a) | End(a) | Execute(a, _) => alloc::vec![*a],
            Copy(a, b) | CopyRegion(a, _, _, b, ..) | Resolve(a, _, b, ..) => alloc::vec![*a, *b],
            _ => Vec::new(),
        }
    }
}

pub struct Cmd {
    /// The bindings it acts with (draws and dispatches).
    pub state: Option<Bindings>,
    pub op: Op,
}

impl Drop for Cmd {
    fn drop(&mut self) {
        if let Some(s) = &mut self.state {
            s.clear();
        }
        for p in self.op.refs() {
            unsafe { release(p) };
        }
    }
}

pub struct Deferred {
    pub state: Bindings,
    pub cmds: Vec<Cmd>,
    /// Subresources mapped here: their bytes, written at Unmap.
    pub maps: Vec<(*mut Com, u32, Vec<u8>)>,
    pub device: *mut Com,
}

impl Drop for Deferred {
    fn drop(&mut self) {
        self.state.clear();
    }
}

pub fn of<'a>(this: *mut Com) -> Option<&'a mut Deferred> {
    if this.is_null() {
        return None;
    }
    match body(this) {
        Body::Deferred(d) => Some(d),
        _ => None,
    }
}

impl Deferred {
    /// Record `op`, with a snapshot of the bindings if it draws or
    /// dispatches.
    pub fn record(&mut self, op: Op, with_state: bool) {
        for p in op.refs() {
            unsafe { add_ref(p) };
        }
        let state = with_state.then(|| self.state.snapshot());
        self.cmds.push(Cmd { state, op });
    }
}

static DC_IIDS: [Guid; 2] = [IID_ID3D11DEVICECHILD, IID_ID3D11DEVICECONTEXT];
static CL_IIDS: [Guid; 2] = [IID_ID3D11DEVICECHILD, IID_ID3D11COMMANDLIST];

pub unsafe extern "system" fn Dev_CreateDeferredContext(this: *mut Com, flags: u32, out: *mut *mut Com) -> i32 {
    if flags != 0 || out.is_null() {
        return E_INVALIDARG;
    }
    let dev = main_of(this);
    let d = Deferred { state: Bindings::default(), cmds: Vec::new(), maps: Vec::new(), device: dev };
    unsafe { *out = Com::new(&ID3D11DEVICECONTEXT, &DC_IIDS, dev, Body::Deferred(Box::new(d))) };
    S_OK
}

pub unsafe extern "system" fn Ctx_FinishCommandList(this: *mut Com, restore: i32, out: *mut *mut Com) -> i32 {
    let Some(d) = of(this) else { return DXGI_ERROR_INVALID_CALL };
    let cmds = core::mem::take(&mut d.cmds);
    if restore == 0 {
        d.state.clear();
    }
    if out.is_null() {
        return S_OK;
    }
    unsafe { *out = Com::new(&ID3D11COMMANDLIST, &CL_IIDS, d.device, Body::CommandList(Box::new(cmds))) };
    S_OK
}

pub unsafe extern "system" fn CL_GetContextFlags(_this: *mut Com) -> u32 {
    0
}

pub unsafe extern "system" fn Ctx_ExecuteCommandList(this: *mut Com, list: *mut Com, restore: i32) {
    if list.is_null() {
        return;
    }
    if let Some(d) = of(this) {
        d.record(Op::Execute(main_of(list), restore != 0), false);
        return;
    }
    let Body::CommandList(cmds) = body(list) else { return };
    let cmds: &Vec<Cmd> = unsafe { &*(&**cmds as *const Vec<Cmd>) };
    let dev = crate::device::of(main_of(this));
    let saved = (restore != 0).then(|| dev.state.snapshot());
    for c in cmds {
        if let Some(s) = &c.state {
            let mut old = core::mem::replace(&mut crate::device::of(main_of(this)).state, s.snapshot());
            old.clear();
        }
        unsafe { replay(this, &c.op) };
    }
    match saved {
        Some(s) => {
            let mut old = core::mem::replace(&mut crate::device::of(main_of(this)).state, s);
            old.clear();
        }
        None => unsafe { crate::context::Ctx_ClearState(this) },
    }
}

/// One command, on the immediate context `this`.
unsafe fn replay(this: *mut Com, op: &Op) {
    use Op::*;
    unsafe {
        match *op {
            Draw(n, inst, start, si) => crate::draw::Ctx_DrawInstanced(this, n, inst, start, si),
            DrawIndexed(n, inst, start, base, si) => crate::draw::Ctx_DrawIndexedInstanced(this, n, inst, start, base, si),
            DrawAuto => crate::draw::Ctx_DrawAuto(this),
            DrawIndirect(indexed, b, off) => {
                if indexed {
                    crate::draw::Ctx_DrawIndexedInstancedIndirect(this, b, off)
                } else {
                    crate::draw::Ctx_DrawInstancedIndirect(this, b, off)
                }
            }
            Dispatch(x, y, z) => crate::compute::Ctx_Dispatch(this, x, y, z),
            DispatchIndirect(b, off) => crate::compute::Ctx_DispatchIndirect(this, b, off),
            ClearRtv(v, c) => crate::res::Ctx_ClearRenderTargetView(this, v, &c),
            ClearDsv(v, f, z, s) => crate::res::Ctx_ClearDepthStencilView(this, v, f, z, s),
            ClearUavUint(v, c) => crate::res::Ctx_ClearUnorderedAccessViewUint(this, v, &c),
            ClearUavFloat(v, c) => crate::res::Ctx_ClearUnorderedAccessViewFloat(this, v, &c),
            Copy(d, s) => crate::res::Ctx_CopyResource(this, d, s),
            CopyRegion(d, ds, [x, y, z], s, ss, b) => crate::res::Ctx_CopySubresourceRegion(this, d, ds, x, y, z, s, ss, b.as_ref().map_or(core::ptr::null(), |b| b as *const Box3)),
            Update(r, sub, b, ref data, pitch, dp) => crate::res::Ctx_UpdateSubresource(this, r, sub, b.as_ref().map_or(core::ptr::null(), |b| b as *const Box3), data.as_ptr(), pitch, dp),
            Write(r, sub, ref data) => {
                if is_resource(r) {
                    let res = resource(r);
                    if let Some(s) = res.subs.get(sub as usize).copied() {
                        let n = data.len().min(res.data.len().saturating_sub(s.at));
                        res.data[s.at..s.at + n].copy_from_slice(&data[..n]);
                        res.changed(sub as usize);
                    }
                }
            }
            GenerateMips(v) => crate::res::Ctx_GenerateMips(this, v),
            Resolve(d, ds, s, ss, f) => crate::res::Ctx_ResolveSubresource(this, d, ds, s, ss, f),
            Begin(q) => crate::state::Ctx_Begin(this, q),
            End(q) => crate::state::Ctx_End(this, q),
            Execute(l, r) => Ctx_ExecuteCommandList(this, l, r as i32),
            ClearState => crate::context::Ctx_ClearState(this),
        }
    }
}

/// Map on a deferred context: only discarding (or not overwriting) a
/// dynamic resource; the bytes are this context's until Unmap records
/// them.
pub fn map(d: &mut Deferred, res: *mut Com, sub: u32, kind: u32) -> Option<(*mut u8, u32, u32)> {
    if !is_resource(res) || !(4..=5).contains(&kind) {
        return None;
    }
    let r = resource(res);
    let s = *r.subs.get(sub as usize)?;
    let size = if r.dim == crate::res::DIM_BUFFER { r.data.len() } else { (s.slice * s.d) as usize };
    let res = main_of(res);
    // No overwrite: what this context last wrote, else the resource's.
    let prev = d.maps.iter().rposition(|m| m.0 == res && m.1 == sub);
    let bytes = match prev {
        Some(i) if kind == 5 => d.maps.remove(i).2,
        _ => r.data.get(s.at..s.at + size).map_or_else(|| alloc::vec![0u8; size], |b| b.to_vec()),
    };
    d.maps.push((res, sub, bytes));
    let m = d.maps.last_mut()?;
    Some((m.2.as_mut_ptr(), s.pitch, s.slice))
}

pub fn unmap(d: &mut Deferred, res: *mut Com, sub: u32) {
    let res = main_of(res);
    if let Some(i) = d.maps.iter().rposition(|m| m.0 == res && m.1 == sub) {
        let data = d.maps[i].2.clone();
        d.record(Op::Write(res, sub, data), false);
    }
}

/// How many bytes UpdateSubresource reads from its source.
pub fn update_size(res: *mut Com, sub: u32, b: Option<&Box3>, pitch: u32, depth_pitch: u32) -> usize {
    if !is_resource(res) {
        return 0;
    }
    let r = resource(res);
    let Some(s) = r.subs.get(sub as usize).copied() else { return 0 };
    if r.dim == crate::res::DIM_BUFFER {
        return b.map_or(r.data.len(), |b| b.right.saturating_sub(b.left) as usize);
    }
    let (w, h, d) = match b {
        Some(b) => (b.right.saturating_sub(b.left), b.bottom.saturating_sub(b.top), b.back.saturating_sub(b.front)),
        None => (s.w, s.h, s.d),
    };
    if w == 0 || h == 0 || d == 0 {
        return 0;
    }
    let rows = crate::format::rows(r.format, h) as usize;
    let row = crate::format::pitch(r.format, w) as usize;
    (d as usize - 1) * depth_pitch as usize + (rows - 1) * pitch as usize + row
}
