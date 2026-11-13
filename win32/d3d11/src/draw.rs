//! Drawing: the input assembler (vertex buffers through the input layout),
//! vertex shaders run here (vm.rs), clipping, the viewport (Direct3D 10's
//! pixel centres at +0.5, as the kernel's), the rasterizer, depth-stencil
//! and blend states as the kernel's group, and the pixel shader's planes
//! per primitive (src/raster3d.rs runs it).

use alloc::vec;
use alloc::vec::Vec;

use gfx3d::raster3d::{self as k, Group, Sampler, Tri, Vert};

use crate::com::*;
use crate::context::{STAGE_PS, STAGE_VS};
use crate::device::Device;
use crate::dxbc::{Ins, SV_INSTANCE_ID, SV_POSITION, SV_VERTEX_ID};
use crate::format::*;
use crate::res::{resource, view};
use crate::state::{state, BlendDesc, DepthStencilDesc, RasterizerDesc, SamplerDesc, StateObj};
use crate::vm::{Env, Vm, R4};

const FX: f64 = 4294967296.0;
const SUB: f64 = 256.0;

// -----| fetching vertices |-----

fn f(u: u32) -> f32 {
    f32::from_bits(u)
}

fn unorm(v: u32, bits: u32) -> u32 {
    (v as f32 / ((1u64 << bits) - 1) as f32).to_bits()
}

fn snorm(v: u32, bits: u32) -> u32 {
    let s = ((v << (32 - bits)) as i32) >> (32 - bits);
    (s as f32 / ((1u32 << (bits - 1)) - 1) as f32).max(-1.0).to_bits()
}

fn sext(v: u32, bits: u32) -> u32 {
    (((v << (32 - bits)) as i32) >> (32 - bits)) as u32
}

/// An element of format `fmt` at `p` as a register: floats for the float
/// and normalized formats, integers for the others; missing components 0,
/// and w 1.
fn fetch(fmt: u32, p: &[u8]) -> Option<R4> {
    let w = |i: usize| -> u32 { u32::from_le_bytes(p[i * 4..i * 4 + 4].try_into().unwrap()) };
    let h = |i: usize| -> u32 { u16::from_le_bytes(p[i * 2..i * 2 + 2].try_into().unwrap()) as u32 };
    let b = |i: usize| -> u32 { p[i] as u32 };
    let one_f = 1.0f32.to_bits();
    let half = |v: u32| crate::vm::half_to_f32(v).to_bits();
    let need = bytes(fmt)? as usize;
    if p.len() < need {
        return None;
    }
    Some(match fmt {
        2 | 3 | 4 => [w(0), w(1), w(2), w(3)],
        6 => [w(0), w(1), w(2), one_f],
        7 | 8 => [w(0), w(1), w(2), 1],
        10 => [half(h(0)), half(h(1)), half(h(2)), half(h(3))],
        11 => [unorm(h(0), 16), unorm(h(1), 16), unorm(h(2), 16), unorm(h(3), 16)],
        12 => [h(0), h(1), h(2), h(3)],
        13 => [snorm(h(0), 16), snorm(h(1), 16), snorm(h(2), 16), snorm(h(3), 16)],
        14 => [sext(h(0), 16), sext(h(1), 16), sext(h(2), 16), sext(h(3), 16)],
        16 => [w(0), w(1), 0, one_f],
        17 | 18 => [w(0), w(1), 0, 1],
        24 => {
            let v = w(0);
            [unorm(v & 1023, 10), unorm((v >> 10) & 1023, 10), unorm((v >> 20) & 1023, 10), unorm(v >> 30, 2)]
        }
        25 => {
            let v = w(0);
            [v & 1023, (v >> 10) & 1023, (v >> 20) & 1023, v >> 30]
        }
        28 | 29 => [unorm(b(0), 8), unorm(b(1), 8), unorm(b(2), 8), unorm(b(3), 8)],
        30 => [b(0), b(1), b(2), b(3)],
        31 => [snorm(b(0), 8), snorm(b(1), 8), snorm(b(2), 8), snorm(b(3), 8)],
        32 => [sext(b(0), 8), sext(b(1), 8), sext(b(2), 8), sext(b(3), 8)],
        34 => [half(h(0)), half(h(1)), 0, one_f],
        35 => [unorm(h(0), 16), unorm(h(1), 16), 0, one_f],
        36 => [h(0), h(1), 0, 1],
        37 => [snorm(h(0), 16), snorm(h(1), 16), 0, one_f],
        38 => [sext(h(0), 16), sext(h(1), 16), 0, 1],
        41 => [w(0), 0, 0, one_f],
        42 | 43 => [w(0), 0, 0, 1],
        49 => [unorm(b(0), 8), unorm(b(1), 8), 0, one_f],
        50 => [b(0), b(1), 0, 1],
        51 => [snorm(b(0), 8), snorm(b(1), 8), 0, one_f],
        52 => [sext(b(0), 8), sext(b(1), 8), 0, 1],
        54 => [half(h(0)), 0, 0, one_f],
        56 => [unorm(h(0), 16), 0, 0, one_f],
        57 => [h(0), 0, 0, 1],
        58 => [snorm(h(0), 16), 0, 0, one_f],
        59 => [sext(h(0), 16), 0, 0, 1],
        61 => [unorm(b(0), 8), 0, 0, one_f],
        62 => [b(0), 0, 0, 1],
        63 => [snorm(b(0), 8), 0, 0, one_f],
        64 => [sext(b(0), 8), 0, 0, 1],
        87 => [unorm(b(2), 8), unorm(b(1), 8), unorm(b(0), 8), unorm(b(3), 8)],
        88 => [unorm(b(2), 8), unorm(b(1), 8), unorm(b(0), 8), one_f],
        _ => return None,
    })
}

/// Where each of the vertex shader's inputs comes from.
enum Src {
    /// Vertex buffer slot, byte offset in a vertex, format, per instance
    /// (step rate).
    Elem { slot: usize, offset: usize, format: u32, step: Option<u32> },
    VertexId,
    InstanceId,
}

struct Inputs {
    regs: Vec<(usize, u8, Src)>,
    /// Each slot: its bytes from the binding's offset, and stride.
    vbs: Vec<(*const u8, usize, usize)>,
}

impl Inputs {
    fn fill(&self, v: &mut [R4], vertex: usize, instance: usize, start_instance: usize) {
        for (reg, mask, src) in &self.regs {
            let val = match src {
                Src::VertexId => [vertex as u32; 4],
                Src::InstanceId => [instance as u32; 4],
                Src::Elem { slot, offset, format, step } => {
                    let (p, len, stride) = self.vbs[*slot];
                    let i = match step {
                        None => vertex,
                        Some(0) => start_instance,
                        Some(s) => start_instance + instance / *s as usize,
                    };
                    let at = i * stride + offset;
                    if p.is_null() || at >= len {
                        [0; 4]
                    } else {
                        fetch(*format, unsafe { core::slice::from_raw_parts(p.add(at), len - at) }).unwrap_or([0; 4])
                    }
                }
            };
            if let Some(r) = v.get_mut(*reg) {
                for c in 0..4 {
                    if mask & (1 << c) != 0 {
                        r[c] = val[c];
                    }
                }
            }
        }
    }
}

struct StageEnv<'a> {
    cbs: &'a [(*const u8, usize); 14],
}

impl Env for StageEnv<'_> {
    fn cb(&self, slot: u32, reg: i64) -> R4 {
        let Some(&(p, len)) = self.cbs.get(slot as usize) else { return [0; 4] };
        if reg < 0 || p.is_null() || (reg as usize + 1) * 16 > len {
            return [0; 4];
        }
        let s = unsafe { core::slice::from_raw_parts(p.add(reg as usize * 16), 16) };
        core::array::from_fn(|k| u32::from_le_bytes(s[k * 4..k * 4 + 4].try_into().unwrap()))
    }
    fn fetch(&self, _ins: &Ins, _c: R4, _x: [R4; 2]) -> R4 {
        // Vertex texture fetch: G5f2.
        [0; 4]
    }
}

fn stage_cbs(d: &Device, st: usize) -> [(*const u8, usize); 14] {
    core::array::from_fn(|i| {
        let b = d.state.stages[st].cbs[i];
        if crate::res::is_resource(b) {
            let r = resource(b);
            (r.data.as_ptr(), r.data.len())
        } else {
            (core::ptr::null(), 0)
        }
    })
}

// -----| vertices in clip space |-----

#[derive(Clone, Copy)]
struct V {
    c: [f32; 4],
    /// The pixel shader's inputs.
    outs: [[f32; 4]; k::PS_INPUTS],
}

fn lerp_v(a: &V, b: &V, t: f32) -> V {
    let l4 = |x: [f32; 4], y: [f32; 4]| core::array::from_fn(|i| x[i] + (y[i] - x[i]) * t);
    V { c: l4(a.c, b.c), outs: core::array::from_fn(|i| l4(a.outs[i], b.outs[i])) }
}

fn dist(v: &V, i: u32) -> f32 {
    match i {
        0 => v.c[3] + v.c[0],
        1 => v.c[3] - v.c[0],
        2 => v.c[3] + v.c[1],
        3 => v.c[3] - v.c[1],
        4 => v.c[2],
        _ => v.c[3] - v.c[2],
    }
}

fn clip_poly(mut p: Vec<V>, zclip: bool) -> Vec<V> {
    for i in 0..6 {
        if i >= 4 && !zclip {
            // Without depth clipping only w > 0 matters.
            if p.iter().all(|v| v.c[3] > 0.0) {
                continue;
            }
        }
        let d = |v: &V| if i >= 4 && !zclip { v.c[3] - 1e-6 } else { dist(v, i) };
        if p.iter().all(|v| d(v) >= 0.0) {
            continue;
        }
        let mut out = Vec::with_capacity(p.len() + 2);
        for j in 0..p.len() {
            let (a, b) = (&p[j], &p[(j + 1) % p.len()]);
            let (da, db) = (d(a), d(b));
            if da >= 0.0 {
                out.push(*a);
            }
            if (da >= 0.0) != (db >= 0.0) {
                out.push(lerp_v(a, b, da / (da - db)));
            }
        }
        p = out;
        if p.len() < 3 {
            return Vec::new();
        }
    }
    p
}

fn clip_line(a: V, b: V, zclip: bool) -> Option<(V, V)> {
    let (mut a, mut b) = (a, b);
    for i in 0..if zclip { 6 } else { 4 } {
        let (da, db) = (dist(&a, i), dist(&b, i));
        if da < 0.0 && db < 0.0 {
            return None;
        }
        if da < 0.0 {
            a = lerp_v(&a, &b, da / (da - db));
        } else if db < 0.0 {
            b = lerp_v(&a, &b, da / (da - db));
        }
    }
    Some((a, b))
}

// -----| window space and the kernel |-----

#[derive(Clone, Copy)]
struct W {
    x: f64,
    y: f64,
    z: f64,
    /// 1/w.
    q: f64,
    v: V,
}

struct Out {
    front: Group,
    back: Group,
    vp: [f32; 6],
    cull: u32,
    front_ccw: bool,
    wire: bool,
    zclip: bool,
    /// Depth bias: constant (in depth), slope-scaled, clamp.
    bias: (f64, f64, f64),
    /// The pixel shader's constants (None: no pixel shader), and its
    /// inputs that take the provoking vertex's value.
    consts: Option<Vec<f32>>,
    flat: u32,
    ninputs: usize,
    fresh: core::cell::Cell<bool>,
}

fn round(x: f64) -> f64 {
    let t = (x + 0.5) as i64 as f64;
    if t > x + 0.5 { t - 1.0 } else { t }
}

impl Out {
    fn window(&self, v: &V) -> W {
        let vp = &self.vp;
        let iw = 1.0 / v.c[3] as f64;
        W {
            x: vp[0] as f64 + (1.0 + v.c[0] as f64 * iw) * vp[2] as f64 / 2.0,
            y: vp[1] as f64 + (1.0 - v.c[1] as f64 * iw) * vp[3] as f64 / 2.0,
            z: vp[4] as f64 + v.c[2] as f64 * iw * (vp[5] - vp[4]) as f64,
            q: iw,
            v: *v,
        }
    }

    fn kverts(&self, ws: &[W], zbias: f64) -> Vec<Vert> {
        let qmax = ws.iter().map(|w| w.q).fold(0.0f64, f64::max);
        ws.iter()
            .map(|w| Vert {
                x: round(w.x * SUB).clamp(-8e6 * SUB, 8e6 * SUB) as i32,
                y: round(w.y * SUB).clamp(-8e6 * SUB, 8e6 * SUB) as i32,
                z: (((w.z + zbias).clamp(0.0, 1.0)) * k::DEPTH_MAX as f64 * FX) as i64,
                q: ((if qmax > 0.0 { w.q / qmax } else { 1.0 }) * FX) as i64,
                r: (255.0 * FX) as i64,
                g: (255.0 * FX) as i64,
                b: (255.0 * FX) as i64,
                a: (255.0 * FX) as i64,
                f: (255.0 * FX) as i64,
                ..Default::default()
            })
            .collect()
    }

    /// The pixel shader's planes for a primitive: [x0, y0, face, Q plane,
    /// Z plane], then each input's (as v q, so the kernel's (v q) / Q is
    /// perspective-correct; flat ones as the provoking vertex's v times Q).
    fn record(&self, kv: &[Vert], ws: &[W], face: f32, provoking: &V) -> Vec<f32> {
        let n = self.ninputs;
        let mut r = Vec::with_capacity(k::PS_HEADER + 12 * n);
        let pos = |i: usize| (kv[i].x as f64 / SUB, kv[i].y as f64 / SUB);
        let (x0, y0) = pos(0);
        let tri = kv.len() >= 3;
        let (x1, y1) = pos(1);
        let (x2, y2) = if tri { pos(2) } else { (0.0, 0.0) };
        let (ax, ay, bx, by) = (x1 - x0, y1 - y0, x2 - x0, y2 - y0);
        let det = ax * by - bx * ay;
        let l2 = ax * ax + ay * ay;
        let solve = |f: &[f64]| -> [f64; 3] {
            if tri {
                if det == 0.0 {
                    return [f[0], 0.0, 0.0];
                }
                let (d1, d2) = (f[1] - f[0], f[2] - f[0]);
                [f[0], (d1 * by - d2 * ay) / det, (d2 * ax - d1 * bx) / det]
            } else {
                let d = f[1] - f[0];
                if l2 == 0.0 { [f[0], 0.0, 0.0] } else { [f[0], d * ax / l2, d * ay / l2] }
            }
        };
        let qs: Vec<f64> = ws.iter().map(|w| w.q).collect();
        let qp = solve(&qs);
        let zs: Vec<f64> = kv.iter().map(|v| v.z as f64 / (FX * k::DEPTH_MAX as f64)).collect();
        let zp = solve(&zs);
        r.extend_from_slice(&[x0 as f32, y0 as f32, face, qp[0] as f32, qp[1] as f32, qp[2] as f32]);
        r.extend_from_slice(&[zp[0] as f32, zp[1] as f32, zp[2] as f32]);
        for i in 0..n {
            let planes: [[f64; 3]; 4] = core::array::from_fn(|c| {
                if self.flat & (1 << i) != 0 {
                    let v = provoking.outs[i][c] as f64;
                    [qp[0] * v, qp[1] * v, qp[2] * v]
                } else {
                    let f: Vec<f64> = ws.iter().map(|w| w.v.outs[i][c] as f64 * w.q).collect();
                    solve(&f)
                }
            });
            for part in 0..3 {
                for c in 0..4 {
                    r.push(planes[c][part] as f32);
                }
            }
        }
        r
    }
}

fn push(d: &mut Device, g: &Group, t: Tri, o: &Out, rec: Option<&[f32]>) {
    let b = &mut d.batch;
    let fresh = o.fresh.replace(false) && o.consts.is_some();
    let same = !fresh
        && b.groups.last().is_some_and(|l| {
            let mut l = *l;
            (l.first, l.count, l.attr_first, l.const_first) = (0, 0, g.attr_first, g.const_first);
            l == *g
        });
    if !same {
        let mut n = *g;
        n.first = b.tris.len() as u32;
        if let Some(c) = &o.consts {
            n.const_first = b.attrs.len() as u32;
            b.attrs.extend_from_slice(c);
            n.attr_first = b.attrs.len() as u32;
        }
        b.groups.push(n);
    }
    if let Some(r) = rec {
        b.attrs.extend_from_slice(r);
    }
    b.groups.last_mut().unwrap().count += 1;
    b.tris.push(t);
    if b.tris.len() >= 1 << 16 || b.attrs.len() >= 1 << 22 {
        d.flush();
    }
}

fn triangle(d: &mut Device, o: &Out, a: &V, b: &V, c: &V) {
    let poly = clip_poly(vec![*a, *b, *c], o.zclip);
    if poly.len() < 3 {
        return;
    }
    let ws: Vec<W> = poly.iter().map(|v| o.window(v)).collect();
    // Orientation in the window (y down): positive is clockwise.
    let mut area = 0.0;
    for i in 0..ws.len() {
        let (p, q) = (&ws[i], &ws[(i + 1) % ws.len()]);
        area += p.x * q.y - q.x * p.y;
    }
    if area == 0.0 {
        return;
    }
    let front = (area > 0.0) != o.front_ccw;
    if (o.cull == 2 && front) || (o.cull == 3 && !front) {
        return;
    }
    let g = if front { &o.front } else { &o.back };
    let face = if front { 1.0 } else { -1.0 };
    let zb = if o.bias != (0.0, 0.0, 0.0) {
        let (p0, p1, p2) = (&ws[0], &ws[1], &ws[2]);
        let det = (p1.x - p0.x) * (p2.y - p0.y) - (p2.x - p0.x) * (p1.y - p0.y);
        let (dzdx, dzdy) = if det == 0.0 {
            (0.0, 0.0)
        } else {
            (((p1.z - p0.z) * (p2.y - p0.y) - (p2.z - p0.z) * (p1.y - p0.y)) / det, ((p2.z - p0.z) * (p1.x - p0.x) - (p1.z - p0.z) * (p2.x - p0.x)) / det)
        };
        let b = o.bias.0 + o.bias.1 * dzdx.abs().max(dzdy.abs());
        if o.bias.2 > 0.0 {
            b.min(o.bias.2)
        } else if o.bias.2 < 0.0 {
            b.max(o.bias.2)
        } else {
            b
        }
    } else {
        0.0
    };
    if o.wire {
        let mut lg = *g;
        (lg.kind, lg.size) = (k::KIND_LINES, k::SUB);
        for i in 0..ws.len() {
            let edge = [ws[i], ws[(i + 1) % ws.len()]];
            let kv = o.kverts(&edge, zb);
            let rec = o.consts.as_ref().map(|_| o.record(&kv, &edge, face, a));
            push(d, &lg, Tri { v: [kv[0], kv[1], Vert::default()] }, o, rec.as_deref());
        }
        return;
    }
    for i in 1..ws.len() - 1 {
        let tri = [ws[0], ws[i], ws[i + 1]];
        let kv = o.kverts(&tri, zb);
        let rec = o.consts.as_ref().map(|_| o.record(&kv, &tri, face, a));
        push(d, g, Tri { v: [kv[0], kv[1], kv[2]] }, o, rec.as_deref());
    }
}

fn line(d: &mut Device, o: &Out, a: &V, b: &V) {
    let Some((ca, cb)) = clip_line(*a, *b, o.zclip) else { return };
    let mut g = o.front;
    (g.kind, g.size) = (k::KIND_LINES, k::SUB);
    let ends = [o.window(&ca), o.window(&cb)];
    let kv = o.kverts(&ends, 0.0);
    let rec = o.consts.as_ref().map(|_| o.record(&kv, &ends, 1.0, a));
    push(d, &g, Tri { v: [kv[0], kv[1], Vert::default()] }, o, rec.as_deref());
}

/// A point: the pixel square around it.
fn point(d: &mut Device, o: &Out, v: &V) {
    if v.c[3] <= 0.0 || v.c[0].abs() > v.c[3] || v.c[1].abs() > v.c[3] || (o.zclip && (v.c[2] < 0.0 || v.c[2] > v.c[3])) {
        return;
    }
    let w = o.window(v);
    let corner = |dx: f64, dy: f64| W { x: w.x + dx, y: w.y + dy, ..w };
    let q = [corner(-0.5, -0.5), corner(0.5, -0.5), corner(0.5, 0.5), corner(-0.5, 0.5)];
    let kv = o.kverts(&q, 0.0);
    let mut g = o.front;
    g.kind = k::KIND_TRIANGLES;
    for (a, b, c) in [(0, 1, 2), (0, 2, 3)] {
        let rec = o.consts.as_ref().map(|_| o.record(&[kv[a], kv[b], kv[c]], &[q[a], q[b], q[c]], 1.0, v));
        push(d, &g, Tri { v: [kv[a], kv[b], kv[c]] }, o, rec.as_deref());
    }
}

// -----| the state, as the kernel's group |-----

fn cmp(f: u32) -> u32 {
    0x200 + f.clamp(1, 8) - 1
}

fn blend_factor(f: u32) -> u32 {
    match f {
        1 => 0,
        2 => 1,
        3 => 0x300,
        4 => 0x301,
        5 => 0x302,
        6 => 0x303,
        7 => 0x304,
        8 => 0x305,
        9 => 0x306,
        10 => 0x307,
        11 => 0x308,
        14 => 0x8001,
        15 => 0x8002,
        _ => 1,
    }
}

fn stencil_op(o: u32) -> u32 {
    match o {
        2 => 0,
        3 => 0x1E01,
        4 => 0x1E02,
        5 => 0x1E03,
        6 => 0x150A,
        7 => 0x8507,
        8 => 0x8508,
        _ => 0x1E00,
    }
}

fn argb(c: [f32; 4]) -> u32 {
    let b = |x: f32| (x.max(0.0).min(1.0) * 255.0 + 0.5) as u32;
    b(c[3]) << 24 | b(c[0]) << 16 | b(c[1]) << 8 | b(c[2])
}

/// A kernel sampler from a D3D11 sampler state and a view's first level.
fn sampler(s: &SamplerDesc, first: u32, levels: u32) -> Sampler {
    let fl = s.filter;
    let lin = |b: u32| if fl & 0x40 != 0 || (fl >> b) & 1 != 0 { k::FILTER_LINEAR } else { k::FILTER_POINT };
    let min_lod = if s.min_lod > 0.0 { s.min_lod as u32 } else { 0 };
    Sampler {
        texture: 0,
        min: lin(4),
        mag: lin(2),
        mip: if levels > 1 && s.max_lod > 0.0 { lin(0) } else { 0 },
        address: [s.address[0].clamp(1, 5), s.address[1].clamp(1, 5)],
        border: argb(s.border),
        lod_bias: (s.lod_bias * 256.0) as i32,
        max_level: first + min_lod,
        gl_rules: 0,
        compare: if fl & 0x80 != 0 { s.compare } else { 0 },
        packed_depth: 0,
    }
}

/// The draw's state; None if it draws nothing.
fn setup(d: &mut Device) -> Option<(Out, Vm<'static>, Inputs, Vec<[Option<(usize, usize)>; 4]>, usize)> {
    let vs = crate::shader::of(d.state.stages[STAGE_VS].shader)?;
    // SAFETY: the shader object is held by the binding for the draw.
    let vs: &'static crate::shader::ShaderObj = unsafe { &*(vs as *const _) };
    let ps = crate::shader::of(d.state.stages[STAGE_PS].shader).filter(|p| p.ps.is_some());
    // The targets: the first render target and the depth-stencil view.
    let rtv = view(d.state.rtvs[0]);
    let dsv = view(d.state.dsv);
    let sub_of = |v: &crate::res::View| -> usize {
        let r = resource(v.res);
        (v.mip + v.first * r.mips) as usize
    };
    let (color, csub) = match &rtv {
        Some(v) => (Some(v.res), sub_of(v)),
        None => (None, 0),
    };
    let (depth, dsub) = match &dsv {
        Some(v) => (Some(v.res), sub_of(v)),
        None => (None, 0),
    };
    if color.is_none() && depth.is_none() {
        return None;
    }
    let target = match color {
        Some(c) => resource(c).target(csub),
        None => {
            // Depth only: a colour target of the depth's size, never shown.
            let r = resource(depth?);
            let s = r.subs[dsub];
            if d.scratch.0 != (s.w, s.h) {
                crate::res::close_later(d.scratch.1);
                d.scratch = ((s.w, s.h), gfx3d::create_target(s.w, s.h));
            }
            d.scratch.1
        }
    };
    let dtarget = depth.map_or(0, |z| resource(z).target(dsub));
    if target == 0 {
        return None;
    }
    if d.batch.target != target || d.batch.depth != dtarget {
        d.flush();
        d.batch.target = target;
        d.batch.depth = dtarget;
    }
    if let Some(c) = color {
        resource(c).drawn[csub] = true;
    }
    if let Some(z) = depth {
        resource(z).drawn[dsub] = true;
    }
    let cformat = color.map_or(B8G8R8A8_UNORM, |c| resource(c).format);
    // The states, Direct3D's defaults where none is set.
    let bs = match state(d.state.blend) {
        Some(StateObj::Blend(b)) => *b,
        _ => BlendDesc::default_state(),
    };
    let ds = match state(d.state.dss) {
        Some(StateObj::DepthStencil(s)) => *s,
        _ => DepthStencilDesc::default_state(),
    };
    let rs = match state(d.state.rs) {
        Some(StateObj::Rasterizer(r)) => *r,
        _ => RasterizerDesc::default_state(),
    };
    let vp = *d.state.viewports.first()?;
    let mut flags = k::F_PERSPECTIVE;
    if depth.is_some() && ds.depth != 0 {
        flags |= k::F_DEPTH_TEST;
        if ds.write_mask != 0 {
            flags |= k::F_DEPTH_WRITE;
        }
    }
    if depth.is_some() && ds.stencil != 0 {
        flags |= k::F_STENCIL;
    }
    let rb = bs.rt[0];
    if rb.enable != 0 {
        flags |= k::F_BLEND;
    }
    if has_alpha(cformat) {
        flags |= k::F_DST_ALPHA;
    }
    let mut sc = [floor(vp[0]) as i32, floor(vp[1]) as i32, ceil(vp[0] + vp[2]) as i32, ceil(vp[1] + vp[3]) as i32];
    if rs.scissor != 0 {
        let s = d.state.scissors.first().copied().unwrap_or([0; 4]);
        sc = [sc[0].max(s[0]), sc[1].max(s[1]), sc[2].min(s[2]), sc[3].min(s[3])];
    }
    let bf = d.state.blend_factor;
    let mut g = Group {
        flags,
        kind: k::KIND_TRIANGLES,
        size: k::SUB,
        depth_func: cmp(ds.func),
        alpha_func: 0x207,
        src_blend: blend_factor(rb.src),
        dst_blend: blend_factor(rb.dst),
        blend_op: rb.op,
        blend_color: argb(bf),
        color_mask: if color.is_some() && ps.is_some() { rb.mask as u32 & 0xF } else { 0 },
        stencil_func: cmp(ds.front.func),
        stencil_ref: d.state.stencil_ref & 0xFF,
        stencil_mask: ds.read as u32,
        stencil_write: ds.write as u32,
        stencil_ops: [stencil_op(ds.front.fail), stencil_op(ds.front.depth_fail), stencil_op(ds.front.pass)],
        scissor: sc,
        ..Default::default()
    };
    // The pixel shader: its program, samplers and constants.
    let mut consts = None;
    let mut flat = 0;
    let mut ninputs = 0;
    let mut links = Vec::new();
    if let Some(p) = ps {
        let code = p.ps.as_ref()?;
        g.flags |= k::F_SHADER;
        g.ps = p.kernel;
        g.ps_inputs = code.inputs.len() as u32;
        ninputs = code.inputs.len();
        // Inputs from the vertex shader's outputs, by semantic.
        for (i, inp) in code.inputs.iter().enumerate() {
            if inp.flat {
                flat |= 1 << i;
            }
            links.push(core::array::from_fn(|c| {
                let (name, idx, comp) = inp.comps[c].as_ref()?;
                let e = vs.sh.osgn.iter().find(|e| &e.name == name && e.index == *idx)?;
                Some((e.reg as usize, e.mask.trailing_zeros() as usize + *comp as usize))
            }));
        }
        // Textures and samplers, per (t#, s#) pair.
        let mut need_flush = false;
        for (slot, &(t, s)) in code.pairs.iter().enumerate() {
            let srv = d.state.stages[STAGE_PS].srvs.get(t as usize).copied().unwrap_or(core::ptr::null_mut());
            let Some(v) = view(srv) else { continue };
            let r = resource(v.res);
            if r.dim == crate::res::DIM_BUFFER {
                continue;
            }
            let sd = match s.map(|s| d.state.stages[STAGE_PS].samplers.get(s as usize).copied().unwrap_or(core::ptr::null_mut())).and_then(state) {
                Some(StateObj::Sampler(sd)) => *sd,
                _ => SamplerDesc::default_state(),
            };
            let mut smp = sampler(&sd, v.mip, v.mips.min(r.mips - v.mip.min(r.mips)));
            if s.is_none() {
                smp.mip = 0;
                (smp.min, smp.mag) = (k::FILTER_POINT, k::FILTER_POINT);
            }
            if r.drawn.iter().any(|&x| x) || r.dirty.iter().any(|&x| x) {
                need_flush = true;
            }
            g.samplers[slot] = smp;
        }
        if need_flush {
            d.flush();
        }
        for (slot, &(t, _)) in code.pairs.iter().enumerate() {
            let srv = d.state.stages[STAGE_PS].srvs.get(t as usize).copied().unwrap_or(core::ptr::null_mut());
            let Some(v) = view(srv) else { continue };
            let r = resource(v.res);
            if r.dim == crate::res::DIM_BUFFER {
                continue;
            }
            if r.bind & crate::res::BIND_DEPTH_STENCIL != 0 {
                g.samplers[slot].texture = r.depth_texture(v.first as usize);
                g.samplers[slot].packed_depth = 1;
            } else {
                g.samplers[slot].texture = r.kernel_texture(v.first as usize);
            }
        }
        // The constant file: the buffers, the immediate constant buffer
        // and immediates, then the kernel's 16 int4 (unused here).
        let cbs = stage_cbs(d, STAGE_PS);
        let mut c: Vec<f32> = vec![0.0; code.nconst.max(1) as usize * 4 + 64];
        for slot in 0..14 {
            let base = code.cb_base[slot];
            if base == u32::MAX {
                continue;
            }
            let (ptr, len) = cbs[slot];
            let n = (code.cb_len[slot] as usize * 16).min(len);
            for i in 0..n / 4 {
                let b = unsafe { core::slice::from_raw_parts(ptr.add(i * 4), 4) };
                c[base as usize * 4 + i] = f32::from_bits(u32::from_le_bytes(b.try_into().unwrap()));
            }
        }
        for (i, v) in code.tail.iter().enumerate() {
            for k in 0..4 {
                c[(code.tail_base as usize + i) * 4 + k] = f32::from_bits(v[k]);
            }
        }
        g.ps_consts = code.nconst.max(1);
        consts = Some(c);
    }
    let mut back = g;
    back.stencil_func = cmp(ds.back.func);
    back.stencil_ops = [stencil_op(ds.back.fail), stencil_op(ds.back.depth_fail), stencil_op(ds.back.pass)];
    // Depth bias in depth units: 2^-24 for 24-bit depth, 2^-16 for 16.
    let unit = match depth.map(|z| resource(z).format) {
        Some(D16_UNORM) | Some(R16_TYPELESS) => 1.0 / 65536.0,
        _ => 1.0 / 16_777_216.0,
    };
    let out = Out {
        front: g,
        back,
        vp,
        cull: rs.cull,
        front_ccw: rs.front_ccw != 0,
        wire: rs.fill == 2,
        zclip: rs.depth_clip != 0,
        bias: (rs.bias as f64 * unit, rs.slope_bias as f64, rs.bias_clamp as f64),
        consts,
        flat,
        ninputs,
        fresh: core::cell::Cell::new(true),
    };
    // The input assembler.
    let vm = Vm::new(&vs.sh)?;
    let layout = match unsafe { d.state.layout.as_ref() } {
        Some(_) => match body(d.state.layout) {
            Body::InputLayout(l) => Some(&**l),
            _ => None,
        },
        None => None,
    };
    let mut regs = Vec::new();
    for e in &vs.sh.inputs {
        match e.sysval {
            SV_VERTEX_ID => regs.push((e.reg as usize, e.mask, Src::VertexId)),
            SV_INSTANCE_ID => regs.push((e.reg as usize, e.mask, Src::InstanceId)),
            _ => {
                // Each signature element in the register: its layout element.
                for s in vs.sh.isgn.iter().filter(|s| s.reg == e.reg && s.sysval == 0) {
                    let el = layout.and_then(|l| l.elems.iter().find(|x| x.name == s.name && x.index == s.index))?;
                    regs.push((
                        e.reg as usize,
                        s.mask,
                        Src::Elem { slot: el.slot as usize, offset: el.offset as usize, format: el.format, step: el.instanced.then_some(el.step) },
                    ));
                }
            }
        }
    }
    let vbs = d
        .state
        .vbs
        .iter()
        .map(|&(b, stride, off)| {
            if crate::res::is_resource(b) {
                let r = resource(b);
                let off = (off as usize).min(r.data.len());
                (unsafe { r.data.as_ptr().add(off) }, r.data.len() - off, stride as usize)
            } else {
                (core::ptr::null(), 0, 0)
            }
        })
        .collect();
    let pos = vs.sh.osgn.iter().find(|e| e.sysval == SV_POSITION || e.name == b"SV_POSITION").map(|e| e.reg as usize)?;
    Some((out, vm, Inputs { regs, vbs }, links, pos))
}

fn floor(x: f32) -> f32 {
    crate::vm::floor(x)
}

fn ceil(x: f32) -> f32 {
    -crate::vm::floor(-x)
}

/// Draw `count` vertices of the bound topology, vertex i being `index(i)`
/// (None: a strip cut), for each instance.
fn draw(this: *mut Com, count: u32, instances: u32, start_instance: u32, index: &dyn Fn(usize) -> Option<Option<usize>>) {
    let d = crate::device::of(main_of(this));
    let topo = d.state.topology;
    let Some((o, vm, inputs, links, pos)) = setup(d) else { return };
    let cbs = stage_cbs(d, STAGE_VS);
    let env = StageEnv { cbs: &cbs };
    let mut regs = vm.regs(32, 32);
    let mut cache: alloc::collections::BTreeMap<usize, V> = alloc::collections::BTreeMap::new();
    for inst in 0..instances.max(1) as usize {
        if instances == 0 {
            break;
        }
        cache.clear();
        let mut shade = |vi: usize| -> V {
            if let Some(v) = cache.get(&vi) {
                return *v;
            }
            for r in regs.o.iter_mut() {
                *r = [0; 4];
            }
            for r in regs.r.iter_mut() {
                *r = [0; 4];
            }
            inputs.fill(&mut regs.v, vi, inst, start_instance as usize);
            vm.run(&mut regs, &env);
            let p = regs.o[pos].map(f);
            let v = V {
                c: p,
                outs: core::array::from_fn(|i| match links.get(i) {
                    Some(l) => core::array::from_fn(|c| match l[c] {
                        Some((r, cc)) => regs.o.get(r).map_or(0.0, |x| f(x[cc & 3])),
                        None => 0.0,
                    }),
                    None => [0.0; 4],
                }),
            };
            cache.insert(vi, v);
            v
        };
        // The vertices in order, with strip cuts.
        let mut verts: Vec<Option<V>> = Vec::with_capacity(count as usize);
        for i in 0..count as usize {
            match index(i) {
                Some(Some(vi)) => verts.push(Some(shade(vi))),
                Some(None) => verts.push(None),
                None => break,
            }
        }
        match topo {
            1 => {
                for v in verts.iter().flatten() {
                    point(d, &o, v);
                }
            }
            2 => {
                for p in verts.chunks_exact(2) {
                    if let (Some(a), Some(b)) = (&p[0], &p[1]) {
                        line(d, &o, a, b);
                    }
                }
            }
            3 => {
                for p in verts.windows(2) {
                    if let (Some(a), Some(b)) = (&p[0], &p[1]) {
                        line(d, &o, a, b);
                    }
                }
            }
            4 => {
                for t in verts.chunks_exact(3) {
                    if let (Some(a), Some(b), Some(c)) = (&t[0], &t[1], &t[2]) {
                        triangle(d, &o, a, b, c);
                    }
                }
            }
            5 => {
                // Strips restart after a cut; every other triangle reversed.
                let mut run: Vec<V> = Vec::new();
                for v in verts.iter().chain(core::iter::once(&None)) {
                    match v {
                        Some(v) => run.push(*v),
                        None => {
                            for i in 2..run.len() {
                                let (a, b, c) = if i % 2 == 0 { (&run[i - 2], &run[i - 1], &run[i]) } else { (&run[i - 1], &run[i - 2], &run[i]) };
                                triangle(d, &o, a, b, c);
                            }
                            run.clear();
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

pub unsafe extern "system" fn Ctx_Draw(this: *mut Com, count: u32, start: u32) {
    draw(this, count, 1, 0, &|i| Some(Some(start as usize + i)));
}

pub unsafe extern "system" fn Ctx_DrawInstanced(this: *mut Com, count: u32, instances: u32, start: u32, start_instance: u32) {
    draw(this, count, instances, start_instance, &|i| Some(Some(start as usize + i)));
}

/// The bound index buffer: its bytes from the offset, and whether 32-bit.
fn indices(this: *mut Com) -> Option<(*const u8, usize, bool)> {
    let d = crate::device::of(main_of(this));
    let (b, fmt, off) = d.state.ib;
    if !crate::res::is_resource(b) {
        return None;
    }
    let r = resource(b);
    let off = (off as usize).min(r.data.len());
    Some((unsafe { r.data.as_ptr().add(off) }, r.data.len() - off, fmt == R32_UINT))
}

fn read_index(ib: (*const u8, usize, bool), i: usize, base: i32) -> Option<Option<usize>> {
    let (p, len, wide) = ib;
    let size = if wide { 4 } else { 2 };
    if (i + 1) * size > len {
        return None;
    }
    let v = unsafe {
        if wide {
            u32::from_le_bytes(core::slice::from_raw_parts(p.add(i * 4), 4).try_into().unwrap())
        } else {
            u16::from_le_bytes(core::slice::from_raw_parts(p.add(i * 2), 2).try_into().unwrap()) as u32
        }
    };
    if (wide && v == u32::MAX) || (!wide && v == 0xFFFF) {
        return Some(None);
    }
    Some(Some((v as i64 + base as i64).max(0) as usize))
}

pub unsafe extern "system" fn Ctx_DrawIndexed(this: *mut Com, count: u32, start: u32, base: i32) {
    let Some(ib) = indices(this) else { return };
    draw(this, count, 1, 0, &|i| read_index(ib, start as usize + i, base));
}

pub unsafe extern "system" fn Ctx_DrawIndexedInstanced(this: *mut Com, count: u32, instances: u32, start: u32, base: i32, start_instance: u32) {
    let Some(ib) = indices(this) else { return };
    draw(this, count, instances, start_instance, &|i| read_index(ib, start as usize + i, base));
}

/// Stream output isn't here yet (G5f2): nothing to draw from.
pub unsafe extern "system" fn Ctx_DrawAuto(_this: *mut Com) {}
