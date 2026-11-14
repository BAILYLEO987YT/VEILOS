//! Evaluators (WIN32_DESIGN.md G5c): glMap1 and glMap2 (Bezier curves and
//! surfaces of colour, normal, texture coordinates and vertices), the grids,
//! glEvalCoord, glEvalPoint and glEvalMesh, GL_AUTO_NORMAL, glGetMap.

use alloc::vec::Vec;

use crate::*;

const MAP1: u32 = 0x0D90;
const MAP2: u32 = 0x0DB0;
const GL_AUTO_NORMAL: u32 = 0x0D80;
const GL_COEFF: u32 = 0x0A00;
const GL_ORDER: u32 = 0x0A01;
const GL_DOMAIN: u32 = 0x0A02;

/// Components of each map target: COLOR_4, INDEX, NORMAL, TEXTURE_COORD_1
/// .. 4, VERTEX_3, VERTEX_4.
const DIMS: [usize; 9] = [4, 1, 3, 1, 2, 3, 4, 3, 4];

#[derive(Clone)]
pub(crate) struct Map {
    /// The domain (u1, u2), and (v1, v2) for a surface.
    pub u: [f32; 2],
    pub v: [f32; 2],
    pub uorder: usize,
    pub vorder: usize,
    /// Control points, packed: uorder x vorder of DIMS components.
    pub points: Vec<f32>,
}

#[derive(Clone)]
pub(crate) struct Eval {
    pub map1: [Option<Map>; 9],
    pub map2: [Option<Map>; 9],
    /// glMapGrid1: n, u1, u2; glMapGrid2: un, u1, u2, vn, v1, v2.
    pub grid1: (i32, f32, f32),
    pub grid2: (i32, f32, f32, i32, f32, f32),
}

impl Eval {
    pub fn new() -> Eval {
        Eval { map1: Default::default(), map2: Default::default(), grid1: (1, 0.0, 1.0), grid2: (1, 0.0, 1.0, 1, 0.0, 1.0) }
    }
}

/// The Bernstein weights of `order` control points at t (0 ..= 1), and
/// their derivatives.
fn bernstein(order: usize, t: f32) -> (Vec<f32>, Vec<f32>) {
    let n = order - 1;
    let binom = |k: usize| -> f32 {
        let mut r = 1.0f64;
        for i in 0..k {
            r = r * (n - i) as f64 / (i + 1) as f64;
        }
        r as f32
    };
    let pw = |x: f32, e: i32| -> f32 { if e <= 0 { 1.0 } else { (0..e).fold(1.0, |a, _| a * x) } };
    let b = (0..order).map(|k| binom(k) * pw(t, k as i32) * pw(1.0 - t, (n - k) as i32)).collect();
    let d = (0..order)
        .map(|k| {
            let a = if k > 0 { k as f32 * pw(t, k as i32 - 1) * pw(1.0 - t, (n - k) as i32) } else { 0.0 };
            let c = if k < n { (n - k) as f32 * pw(t, k as i32) * pw(1.0 - t, (n - k) as i32 - 1) } else { 0.0 };
            binom(k) * (a - c)
        })
        .collect();
    (b, d)
}

fn eval1(m: &Map, dim: usize, u: f32) -> [f32; 4] {
    let t = if m.u[1] != m.u[0] { (u - m.u[0]) / (m.u[1] - m.u[0]) } else { 0.0 };
    let (b, _) = bernstein(m.uorder, t);
    let mut out = [0.0, 0.0, 0.0, 1.0];
    for (k, o) in out.iter_mut().enumerate().take(dim) {
        *o = (0..m.uorder).map(|i| b[i] * m.points[i * dim + k]).sum();
    }
    out
}

/// A surface's value at (u, v), and its derivatives along u and v.
fn eval2(m: &Map, dim: usize, u: f32, v: f32) -> ([f32; 4], [f32; 4], [f32; 4]) {
    let tu = if m.u[1] != m.u[0] { (u - m.u[0]) / (m.u[1] - m.u[0]) } else { 0.0 };
    let tv = if m.v[1] != m.v[0] { (v - m.v[0]) / (m.v[1] - m.v[0]) } else { 0.0 };
    let (bu, du) = bernstein(m.uorder, tu);
    let (bv, dv) = bernstein(m.vorder, tv);
    let mut p = [0.0, 0.0, 0.0, 1.0];
    let mut pu = [0.0; 4];
    let mut pv = [0.0; 4];
    for k in 0..dim {
        let (mut a, mut b, mut c) = (0.0, 0.0, 0.0);
        for i in 0..m.uorder {
            for j in 0..m.vorder {
                let x = m.points[(i * m.vorder + j) * dim + k];
                a += bu[i] * bv[j] * x;
                b += du[i] * bv[j] * x;
                c += bu[i] * dv[j] * x;
            }
        }
        p[k] = a;
        pu[k] = b;
        pv[k] = c;
    }
    (p, pu, pv)
}

fn map(dims: usize, target: u32, u: [f32; 2], v: [f32; 2], ustride: usize, uorder: usize, vstride: usize, vorder: usize, get: &dyn Fn(usize) -> f32) {
    let base = if dims == 1 { MAP1 } else { MAP2 };
    with(|c| {
        if !(base..base + 9).contains(&target) {
            return c.err(GL_INVALID_ENUM);
        }
        let i = (target - base) as usize;
        let dim = DIMS[i];
        if uorder < 1 || uorder > 8 || vorder < 1 || vorder > 8 || ustride < dim || (dims == 2 && vstride < dim) || u[0] == u[1] || (dims == 2 && v[0] == v[1]) {
            return c.err(GL_INVALID_VALUE);
        }
        let mut points = Vec::with_capacity(uorder * vorder * dim);
        for a in 0..uorder {
            for b in 0..vorder {
                for k in 0..dim {
                    points.push(get(a * ustride + b * vstride + k));
                }
            }
        }
        let m = Map { u, v, uorder, vorder, points };
        if dims == 1 { c.eval.map1[i] = Some(m) } else { c.eval.map2[i] = Some(m) }
    })
}

fn set_map(dims: usize, target: u32, m: Map) {
    let m2 = m.clone();
    rec!(set_map(dims, target, m2.clone()));
    with(|c| {
        let base = if dims == 1 { MAP1 } else { MAP2 };
        let i = (target - base) as usize;
        if dims == 1 { c.eval.map1[i] = Some(m) } else { c.eval.map2[i] = Some(m) }
    })
}

/// glMap*: read the points now (a display list keeps them), then set.
fn map_and_record(dims: usize, target: u32, u: [f32; 2], v: [f32; 2], ustride: usize, uorder: usize, vstride: usize, vorder: usize, get: &dyn Fn(usize) -> f32) {
    map(dims, target, u, v, ustride, uorder, vstride, vorder, get);
    let base = if dims == 1 { MAP1 } else { MAP2 };
    if !(base..base + 9).contains(&target) {
        return;
    }
    let m = with(|c| if dims == 1 { c.eval.map1[(target - base) as usize].clone() } else { c.eval.map2[(target - base) as usize].clone() });
    if let Some(m) = m {
        set_map(dims, target, m);
    }
}

#[no_mangle]
pub unsafe extern "system" fn glMap1f(target: u32, u1: f32, u2: f32, stride: i32, order: i32, p: *const f32) {
    map_and_record(1, target, [u1, u2], [0.0, 1.0], stride.max(0) as usize, order.max(0) as usize, 0, 1, &|i| unsafe { *p.add(i) })
}

#[no_mangle]
pub unsafe extern "system" fn glMap1d(target: u32, u1: f64, u2: f64, stride: i32, order: i32, p: *const f64) {
    map_and_record(1, target, [u1 as f32, u2 as f32], [0.0, 1.0], stride.max(0) as usize, order.max(0) as usize, 0, 1, &|i| unsafe { *p.add(i) } as f32)
}

#[no_mangle]
pub unsafe extern "system" fn glMap2f(target: u32, u1: f32, u2: f32, us: i32, uo: i32, v1: f32, v2: f32, vs: i32, vo: i32, p: *const f32) {
    map_and_record(2, target, [u1, u2], [v1, v2], us.max(0) as usize, uo.max(0) as usize, vs.max(0) as usize, vo.max(0) as usize, &|i| unsafe { *p.add(i) })
}

#[no_mangle]
pub unsafe extern "system" fn glMap2d(target: u32, u1: f64, u2: f64, us: i32, uo: i32, v1: f64, v2: f64, vs: i32, vo: i32, p: *const f64) {
    map_and_record(2, target, [u1 as f32, u2 as f32], [v1 as f32, v2 as f32], us.max(0) as usize, uo.max(0) as usize, vs.max(0) as usize, vo.max(0) as usize, &|i| unsafe { *p.add(i) } as f32)
}

/// Evaluate the enabled 1D maps at u, and make the vertex.
fn eval_coord1(u: f32) {
    rec!(eval_coord1(u));
    with(|c| {
        let saved = (c.color, c.normal, c.tc);
        let ev = c.eval.clone();
        let ons: [bool; 9] = core::array::from_fn(|i| c.on(MAP1 + i as u32));
        let on = |i: usize| ons[i];
        let get = |i: usize| ev.map1[i].as_ref().map(|m| eval1(m, DIMS[i], u));
        if on(0) {
            if let Some(v) = get(0) {
                c.color = v;
            }
        }
        if on(2) {
            if let Some(v) = get(2) {
                c.normal = [v[0], v[1], v[2]];
            }
        }
        for (i, n) in [(6usize, 4usize), (5, 3), (4, 2), (3, 1)] {
            if on(i) {
                if let Some(v) = get(i) {
                    c.tc[c.unit] = [v[0], if n > 1 { v[1] } else { 0.0 }, if n > 2 { v[2] } else { 0.0 }, if n > 3 { v[3] } else { 1.0 }];
                    break;
                }
            }
        }
        let pos = if on(8) { get(8) } else { None }.or_else(|| if on(7) { get(7).map(|v| [v[0], v[1], v[2], 1.0]) } else { None });
        if let (Some(p), Some(_)) = (pos, c.prim) {
            c.vertex(p);
        }
        (c.color, c.normal, c.tc) = saved;
    })
}

/// Evaluate the enabled 2D maps at (u, v), and make the vertex.
fn eval_coord2(u: f32, v: f32) {
    rec!(eval_coord2(u, v));
    with(|c| {
        let saved = (c.color, c.normal, c.tc);
        let ev = c.eval.clone();
        let auto = c.on(GL_AUTO_NORMAL);
        let ons: [bool; 9] = core::array::from_fn(|i| c.on(MAP2 + i as u32));
        let on = |i: usize| ons[i];
        let get = |i: usize| ev.map2[i].as_ref().map(|m| eval2(m, DIMS[i], u, v));
        if on(0) {
            if let Some((p, _, _)) = get(0) {
                c.color = p;
            }
        }
        if on(2) {
            if let Some((p, _, _)) = get(2) {
                c.normal = [p[0], p[1], p[2]];
            }
        }
        for (i, n) in [(6usize, 4usize), (5, 3), (4, 2), (3, 1)] {
            if on(i) {
                if let Some((p, _, _)) = get(i) {
                    c.tc[c.unit] = [p[0], if n > 1 { p[1] } else { 0.0 }, if n > 2 { p[2] } else { 0.0 }, if n > 3 { p[3] } else { 1.0 }];
                    break;
                }
            }
        }
        let vert = if on(8) { get(8) } else { None }.or_else(|| if on(7) { get(7).map(|(p, a, b)| ([p[0], p[1], p[2], 1.0], a, b)) } else { None });
        if let Some((p, pu, pv)) = vert {
            if auto {
                // The surface's normal: dP/du x dP/dv.
                c.normal = light::normalize([pu[1] * pv[2] - pu[2] * pv[1], pu[2] * pv[0] - pu[0] * pv[2], pu[0] * pv[1] - pu[1] * pv[0]]);
            }
            if c.prim.is_some() {
                c.vertex(p);
            }
        }
        (c.color, c.normal, c.tc) = saved;
    })
}

#[no_mangle] pub extern "system" fn glEvalCoord1f(u: f32) { eval_coord1(u) }
#[no_mangle] pub extern "system" fn glEvalCoord1d(u: f64) { eval_coord1(u as f32) }
#[no_mangle] pub unsafe extern "system" fn glEvalCoord1fv(u: *const f32) { eval_coord1(unsafe { *u }) }
#[no_mangle] pub unsafe extern "system" fn glEvalCoord1dv(u: *const f64) { eval_coord1(unsafe { *u } as f32) }
#[no_mangle] pub extern "system" fn glEvalCoord2f(u: f32, v: f32) { eval_coord2(u, v) }
#[no_mangle] pub extern "system" fn glEvalCoord2d(u: f64, v: f64) { eval_coord2(u as f32, v as f32) }
#[no_mangle] pub unsafe extern "system" fn glEvalCoord2fv(u: *const f32) { unsafe { eval_coord2(*u, *u.add(1)) } }
#[no_mangle] pub unsafe extern "system" fn glEvalCoord2dv(u: *const f64) { unsafe { eval_coord2(*u as f32, *u.add(1) as f32) } }

#[no_mangle]
pub extern "system" fn glMapGrid1f(n: i32, u1: f32, u2: f32) {
    rec!(glMapGrid1f(n, u1, u2));
    outside(|c| if n > 0 { c.eval.grid1 = (n, u1, u2) } else { c.err(GL_INVALID_VALUE) })
}

#[no_mangle]
pub extern "system" fn glMapGrid1d(n: i32, u1: f64, u2: f64) {
    glMapGrid1f(n, u1 as f32, u2 as f32)
}

#[no_mangle]
pub extern "system" fn glMapGrid2f(un: i32, u1: f32, u2: f32, vn: i32, v1: f32, v2: f32) {
    rec!(glMapGrid2f(un, u1, u2, vn, v1, v2));
    outside(|c| if un > 0 && vn > 0 { c.eval.grid2 = (un, u1, u2, vn, v1, v2) } else { c.err(GL_INVALID_VALUE) })
}

#[no_mangle]
pub extern "system" fn glMapGrid2d(un: i32, u1: f64, u2: f64, vn: i32, v1: f64, v2: f64) {
    glMapGrid2f(un, u1 as f32, u2 as f32, vn, v1 as f32, v2 as f32)
}

fn grid1_u(i: i32) -> f32 {
    with(|c| {
        let (n, a, b) = c.eval.grid1;
        if i == n { b } else { a + i as f32 * (b - a) / n as f32 }
    })
}

fn grid2_uv(i: i32, j: i32) -> (f32, f32) {
    with(|c| {
        let (un, u1, u2, vn, v1, v2) = c.eval.grid2;
        (if i == un { u2 } else { u1 + i as f32 * (u2 - u1) / un as f32 }, if j == vn { v2 } else { v1 + j as f32 * (v2 - v1) / vn as f32 })
    })
}

#[no_mangle]
pub extern "system" fn glEvalPoint1(i: i32) {
    eval_coord1(grid1_u(i))
}

#[no_mangle]
pub extern "system" fn glEvalPoint2(i: i32, j: i32) {
    let (u, v) = grid2_uv(i, j);
    eval_coord2(u, v)
}

#[no_mangle]
pub extern "system" fn glEvalMesh1(mode: u32, i1: i32, i2: i32) {
    let prim = match mode {
        GL_POINT => GL_POINTS,
        GL_LINE => GL_LINE_STRIP,
        _ => return with(|c| c.err(GL_INVALID_ENUM)),
    };
    glBegin(prim);
    for i in i1..=i2 {
        glEvalPoint1(i);
    }
    glEnd();
}

#[no_mangle]
pub extern "system" fn glEvalMesh2(mode: u32, i1: i32, i2: i32, j1: i32, j2: i32) {
    match mode {
        GL_FILL => {
            for j in j1..j2 {
                glBegin(GL_QUAD_STRIP);
                for i in i1..=i2 {
                    glEvalPoint2(i, j);
                    glEvalPoint2(i, j + 1);
                }
                glEnd();
            }
        }
        GL_LINE => {
            for j in j1..=j2 {
                glBegin(GL_LINE_STRIP);
                for i in i1..=i2 {
                    glEvalPoint2(i, j);
                }
                glEnd();
            }
            for i in i1..=i2 {
                glBegin(GL_LINE_STRIP);
                for j in j1..=j2 {
                    glEvalPoint2(i, j);
                }
                glEnd();
            }
        }
        GL_POINT => {
            glBegin(GL_POINTS);
            for j in j1..=j2 {
                for i in i1..=i2 {
                    glEvalPoint2(i, j);
                }
            }
            glEnd();
        }
        _ => with(|c| c.err(GL_INVALID_ENUM)),
    }
}

fn get_map(target: u32, query: u32) -> Vec<f64> {
    with(|c| {
        let (m, dims) = if (MAP1..MAP1 + 9).contains(&target) {
            (c.eval.map1[(target - MAP1) as usize].clone(), 1)
        } else if (MAP2..MAP2 + 9).contains(&target) {
            (c.eval.map2[(target - MAP2) as usize].clone(), 2)
        } else {
            c.err(GL_INVALID_ENUM);
            return Vec::new();
        };
        let m = m.unwrap_or(Map { u: [0.0, 1.0], v: [0.0, 1.0], uorder: 1, vorder: 1, points: Vec::new() });
        match query {
            GL_COEFF => m.points.iter().map(|&x| x as f64).collect(),
            GL_ORDER => if dims == 1 { alloc::vec![m.uorder as f64] } else { alloc::vec![m.uorder as f64, m.vorder as f64] },
            GL_DOMAIN => if dims == 1 { alloc::vec![m.u[0] as f64, m.u[1] as f64] } else { alloc::vec![m.u[0] as f64, m.u[1] as f64, m.v[0] as f64, m.v[1] as f64] },
            _ => {
                c.err(GL_INVALID_ENUM);
                Vec::new()
            }
        }
    })
}

#[no_mangle] pub unsafe extern "system" fn glGetMapfv(t: u32, q: u32, out: *mut f32) { let v = get_map(t, q); more::put(out, &v, v.len()) }
#[no_mangle] pub unsafe extern "system" fn glGetMapdv(t: u32, q: u32, out: *mut f64) { let v = get_map(t, q); more::put(out, &v, v.len()) }
#[no_mangle] pub unsafe extern "system" fn glGetMapiv(t: u32, q: u32, out: *mut i32) { let v = get_map(t, q); more::put(out, &v, v.len()) }
