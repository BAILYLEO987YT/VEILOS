//! The tessellator: from a patch's tess factors, the domain points (where
//! the domain shader runs) and the primitives joining them.
//!
//! Triangles and quads are tessellated in rings: the outer ring's edges
//! split by their own factors, the inner rings by the inside factor(s),
//! shrinking toward the middle, each ring stitched to the next. Integer
//! and power-of-two partitioning space points evenly; the fractional ones
//! put the two short segments in the middle of each edge, as Direct3D's
//! do.

use alloc::vec;
use alloc::vec::Vec;

pub struct Tessellation {
    /// Domain locations: (u, v, w) for triangles, (u, v, 0) otherwise.
    pub points: Vec<[f32; 3]>,
    /// Primitives: 1, 2 or 3 points each.
    pub prims: Vec<Vec<u32>>,
}

/// A tess factor as the partitioning rounds it, and the factor its points
/// are spaced by (the fractional ones keep theirs).
fn factor(f: f32, part: u32, max: f32) -> Option<(u32, f32)> {
    if !(f > 0.0) {
        return None;
    }
    let max = max.clamp(1.0, 64.0);
    let ceil = |x: f32| -> u32 {
        let t = x as u32;
        if (t as f32) < x { t + 1 } else { t }
    };
    Some(match part {
        2 => {
            let n = ceil(f.clamp(1.0, max));
            (n.next_power_of_two().min(64), n.next_power_of_two().min(64) as f32)
        }
        3 => {
            let f = f.clamp(1.0, max.max(1.0));
            let n = ceil(f);
            (if n % 2 == 0 { n + 1 } else { n }, f)
        }
        4 => {
            let f = f.clamp(2.0, max.max(2.0));
            let n = ceil(f);
            (if n % 2 == 1 { n + 1 } else { n }, f)
        }
        _ => {
            let n = ceil(f.clamp(1.0, max));
            (n, n as f32)
        }
    })
}

/// Where the points on an edge split `n` ways by factor `f` fall, 0 ..= 1.
fn split(n: u32, f: f32) -> Vec<f32> {
    if n == 0 {
        return vec![0.0];
    }
    if n < 2 || (n as f32 - f).abs() < 1e-6 || f <= 0.0 {
        return (0..=n).map(|i| i as f32 / n as f32).collect();
    }
    // Fractional: n - 2 segments of 1/f, two short ones in the middle.
    let long = 1.0 / f;
    let short = ((1.0 - (n - 2) as f32 * long) / 2.0).max(0.0);
    let half = (n - 2) / 2;
    let mut lens = vec![long; n as usize];
    lens[half as usize] = short;
    lens[(n - 1 - half) as usize] = short;
    let mut out = vec![0.0];
    let mut t = 0.0;
    for l in lens {
        t += l;
        out.push(t.min(1.0));
    }
    *out.last_mut().unwrap() = 1.0;
    out
}

struct Builder {
    points: Vec<[f32; 3]>,
    prims: Vec<Vec<u32>>,
}

impl Builder {
    fn point(&mut self, p: [f32; 3]) -> u32 {
        self.points.push(p);
        self.points.len() as u32 - 1
    }

    /// Join an outer polyline `a` and an inner one `b` (same direction)
    /// with triangles, outer first. An outer edge two segments longer
    /// than the inner is Direct3D's regular trapezoid: a triangle at each
    /// corner and quads between, their diagonals mirrored about the
    /// middle. Other edges (uneven factors) are zipped up evenly, not
    /// quite as Direct3D's transition tables do.
    fn stitch(&mut self, a: &[u32], b: &[u32]) {
        let (m, p) = (a.len() - 1, b.len() - 1);
        if p >= 1 && m == p + 2 {
            self.prims.push(vec![a[0], a[1], b[0]]);
            for q in 0..p {
                // The first half (the middle quad with it) lean one way.
                let x = q < (p + 1) / 2;
                let (o1, o2, i0, i1) = (a[q + 1], a[q + 2], b[q], b[q + 1]);
                if x {
                    self.prims.push(vec![o1, o2, i1]);
                    self.prims.push(vec![o1, i1, i0]);
                } else {
                    self.prims.push(vec![o1, o2, i0]);
                    self.prims.push(vec![o2, i1, i0]);
                }
            }
            self.prims.push(vec![a[m - 1], a[m], b[p]]);
            return;
        }
        let (mut i, mut j) = (0, 0);
        while i < m || j < p {
            // Advance on whichever side is further behind.
            let ta = if m > 0 { (i as f32 + 1.0) / m as f32 } else { 2.0 };
            let tb = if p > 0 { (j as f32 + 1.0) / p as f32 } else { 2.0 };
            if j >= p || (i < m && ta <= tb) {
                self.prims.push(vec![a[i], a[i + 1], b[j]]);
                i += 1;
            } else {
                self.prims.push(vec![a[i], b[j + 1], b[j]]);
                j += 1;
            }
        }
    }
}

fn lerp3(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    core::array::from_fn(|k| a[k] + (b[k] - a[k]) * t)
}

/// Tessellate: `tf` the patch's factors (triangle: 3 edges then inside;
/// quad: 4 edges then 2 inside; isoline: detail, density). None culls the
/// patch.
pub fn tessellate(domain: u32, part: u32, max: f32, tf: &[f32]) -> Option<Tessellation> {
    let mut b = Builder { points: Vec::new(), prims: Vec::new() };
    match domain {
        2 => {
            // Corners: u, v, w = 1. Edge i lies opposite corner i.
            let corner = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
            let e: Vec<(u32, f32)> = (0..3).map(|i| factor(tf[i], part, max)).collect::<Option<_>>()?;
            let (mut n, nf) = factor(tf.get(3).copied().unwrap_or(1.0), part, max)?;
            if n <= 1 && e.iter().all(|x| x.0 <= 1) {
                let v: Vec<u32> = corner.iter().map(|&c| b.point(c)).collect();
                b.prims.push(v);
                return Some(Tessellation { points: b.points, prims: b.prims });
            }
            if n <= 1 {
                n = if part == 3 { 3 } else { 2 };
            }
            let c = [1.0 / 3.0; 3];
            let ring_corner = |k: u32, i: usize| -> [f32; 3] { lerp3(c, corner[i], (n as f32 - 2.0 * k as f32) / n as f32) };
            // The outer ring: edge i from corner (i+1) to (i+2).
            let mut outer: Vec<Vec<u32>> = Vec::new();
            let corners: Vec<u32> = (0..3).map(|i| b.point(corner[i])).collect();
            for i in 0..3 {
                let (from, to) = ((i + 1) % 3, (i + 2) % 3);
                let s = split(e[i].0, e[i].1);
                let mut pts = vec![corners[from]];
                for &t in &s[1..s.len() - 1] {
                    pts.push(b.point(lerp3(corner[from], corner[to], t)));
                }
                pts.push(corners[to]);
                outer.push(pts);
            }
            // Inner rings, each stitched to the one around it.
            let mut k = 1;
            loop {
                let segs = n as i32 - 2 * k as i32;
                if segs <= 0 {
                    // The middle point.
                    let m = b.point(c);
                    for edge in &outer {
                        b.stitch(edge, &[m]);
                    }
                    break;
                }
                let rc: Vec<[f32; 3]> = (0..3).map(|i| ring_corner(k, i)).collect();
                let rcp: Vec<u32> = rc.iter().map(|&p| b.point(p)).collect();
                let s = split(segs as u32, if part >= 3 { nf - 2.0 * k as f32 } else { segs as f32 });
                let mut inner: Vec<Vec<u32>> = Vec::new();
                for i in 0..3 {
                    let (from, to) = ((i + 1) % 3, (i + 2) % 3);
                    let mut pts = vec![rcp[from]];
                    for &t in &s[1..s.len() - 1] {
                        pts.push(b.point(lerp3(rc[from], rc[to], t)));
                    }
                    pts.push(rcp[to]);
                    inner.push(pts);
                }
                for i in 0..3 {
                    b.stitch(&outer[i], &inner[i]);
                }
                if segs == 1 {
                    // The last ring is a triangle of its own.
                    b.prims.push(vec![rcp[0], rcp[1], rcp[2]]);
                    break;
                }
                outer = inner;
                k += 1;
            }
        }
        3 => {
            // Corners (u, v): (0,0) (1,0) (1,1) (0,1). Edges: u=0, v=0,
            // u=1, v=1.
            let e: Vec<(u32, f32)> = (0..4).map(|i| factor(tf[i], part, max)).collect::<Option<_>>()?;
            let (mut nu, nuf) = factor(tf.get(4).copied().unwrap_or(1.0), part, max)?;
            let (mut nv, nvf) = factor(tf.get(5).copied().unwrap_or(1.0), part, max)?;
            if nu <= 1 && nv <= 1 && e.iter().all(|x| x.0 <= 1) {
                let q: Vec<u32> = [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [1.0, 1.0, 0.0], [0.0, 1.0, 0.0]].iter().map(|&p| b.point(p)).collect();
                b.prims.push(vec![q[0], q[1], q[2]]);
                b.prims.push(vec![q[0], q[2], q[3]]);
                return Some(Tessellation { points: b.points, prims: b.prims });
            }
            nu = nu.max(2);
            nv = nv.max(2);
            let cu = |k: u32| -> [f32; 2] { [k as f32 / nu as f32, 1.0 - k as f32 / nu as f32] };
            let cv = |k: u32| -> [f32; 2] { [k as f32 / nv as f32, 1.0 - k as f32 / nv as f32] };
            // A ring's four edges, going round: v=lo (u up), u=hi (v up),
            // v=hi (u down), u=lo (v down).
            let ring = |b: &mut Builder, k: u32, outer: bool| -> Vec<Vec<u32>> {
                let ([u0, u1], [v0, v1]) = (cu(k), cv(k));
                let c = [[u0, v0, 0.0], [u1, v0, 0.0], [u1, v1, 0.0], [u0, v1, 0.0]];
                let cp: Vec<u32> = c.iter().map(|&p| b.point(p)).collect();
                // Which factor each edge takes: v=0 is edge 1, u=1 edge 2,
                // v=1 edge 3, u=0 edge 0.
                let which = [1usize, 2, 3, 0];
                let mut out = Vec::new();
                for s in 0..4 {
                    let (n, f) = if outer {
                        e[which[s]]
                    } else if s % 2 == 0 {
                        let segs = nu - 2 * k;
                        (segs, if part >= 3 { nuf - 2.0 * k as f32 } else { segs as f32 })
                    } else {
                        let segs = nv - 2 * k;
                        (segs, if part >= 3 { nvf - 2.0 * k as f32 } else { segs as f32 })
                    };
                    let t = split(n, f);
                    let (a, z) = (s, (s + 1) % 4);
                    let mut pts = vec![cp[a]];
                    for &x in &t[1..t.len() - 1] {
                        pts.push(b.point(lerp3(c[a], c[z], x)));
                    }
                    pts.push(cp[z]);
                    out.push(pts);
                }
                out
            };
            let mut outer = ring(&mut b, 0, true);
            let mut k = 1;
            loop {
                let (su, sv) = (nu as i32 - 2 * k as i32, nv as i32 - 2 * k as i32);
                if su <= 0 || sv <= 0 {
                    // What's left in the middle: a line of points (or one).
                    let ([u0, u1], [v0, v1]) = if su <= 0 && sv <= 0 { ([0.5, 0.5], [0.5, 0.5]) } else if su <= 0 { ([0.5, 0.5], cv(k)) } else { (cu(k), [0.5, 0.5]) };
                    let segs = (su.max(sv).max(0)) as u32;
                    let ts = split(segs, segs as f32);
                    let line: Vec<u32> = ts.iter().map(|&t| b.point([u0 + (u1 - u0) * t, v0 + (v1 - v0) * t, 0.0])).collect();
                    let rev: Vec<u32> = line.iter().rev().copied().collect();
                    let first = [line[0]];
                    let last = [*line.last().unwrap()];
                    if su <= 0 && sv > 0 {
                        // A vertical line: v=lo edge to its bottom, u=hi up it,
                        // v=hi to its top, u=lo down it.
                        b.stitch(&outer[0], &first);
                        b.stitch(&outer[1], &line);
                        b.stitch(&outer[2], &last);
                        b.stitch(&outer[3], &rev);
                    } else if sv <= 0 && su > 0 {
                        b.stitch(&outer[0], &line);
                        b.stitch(&outer[1], &last);
                        b.stitch(&outer[2], &rev);
                        b.stitch(&outer[3], &first);
                    } else {
                        for edge in &outer {
                            b.stitch(edge, &first);
                        }
                    }
                    break;
                }
                let inner = ring(&mut b, k, false);
                for s in 0..4 {
                    b.stitch(&outer[s], &inner[s]);
                }
                if su == 1 && sv == 1 {
                    // The middle cell: two triangles.
                    let c: Vec<u32> = inner.iter().map(|e| e[0]).collect();
                    b.prims.push(vec![c[0], c[1], c[3]]);
                    b.prims.push(vec![c[1], c[2], c[3]]);
                    break;
                }
                outer = inner;
                k += 1;
            }
        }
        1 => {
            // Isolines: `density` lines (v), each split `detail` ways (u).
            let (detail, df) = factor(tf[0], part, max)?;
            let (density, _) = factor(tf.get(1).copied().unwrap_or(1.0), 1, max)?;
            let us = split(detail, df);
            for j in 0..density {
                let v = j as f32 / density as f32;
                let pts: Vec<u32> = us.iter().map(|&u| b.point([u, v, 0.0])).collect();
                for w in pts.windows(2) {
                    b.prims.push(vec![w[0], w[1]]);
                }
            }
        }
        _ => return None,
    }
    Some(Tessellation { points: b.points, prims: b.prims })
}
