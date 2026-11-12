//! Lighting (GL 1.1's per-vertex formula) and fog's state (the fog itself is
//! worked out per pixel by the kernel, raster3d.rs), and the few
//! transcendental functions lighting needs (no libm here).

use crate::{GL_AMBIENT, GL_AMBIENT_AND_DIFFUSE, GL_DIFFUSE, GL_EMISSION, GL_EXP, GL_FRONT, GL_FRONT_AND_BACK, GL_SPECULAR};

// -----| maths |-----

/// log2 of a positive finite x.
pub fn log2(x: f64) -> f64 {
    if x <= 0.0 {
        return f64::NEG_INFINITY;
    }
    let bits = x.to_bits();
    let mut e = ((bits >> 52) & 0x7FF) as i64 - 1023;
    let mut m = f64::from_bits((bits & !(0x7FFu64 << 52)) | (1023u64 << 52));
    if e == -1023 {
        // Subnormal: scale up first.
        return log2(x * (1u64 << 60) as f64) - 60.0;
    }
    if m > core::f64::consts::SQRT_2 {
        m /= 2.0;
        e += 1;
    }
    // ln(m) = 2 atanh(y), y = (m - 1)/(m + 1), |y| <= 0.172.
    let y = (m - 1.0) / (m + 1.0);
    let y2 = y * y;
    let mut term = y;
    let mut sum = 0.0;
    for k in 0..12 {
        sum += term / (2 * k + 1) as f64;
        term *= y2;
    }
    e as f64 + 2.0 * sum * core::f64::consts::LOG2_E
}

/// 2^x.
pub fn exp2(x: f64) -> f64 {
    if x < -1022.0 {
        return 0.0;
    }
    if x > 1023.0 {
        return f64::INFINITY;
    }
    let mut n = x as i64;
    if n as f64 > x {
        n -= 1;
    }
    let f = (x - n as f64) * core::f64::consts::LN_2;
    // e^f, 0 <= f < ln 2.
    let mut term = 1.0;
    let mut sum = 1.0;
    for k in 1..16 {
        term *= f / k as f64;
        sum += term;
    }
    sum * f64::from_bits(((n + 1023) as u64) << 52)
}

/// x^e for x >= 0.
pub fn pow(x: f64, e: f64) -> f64 {
    if e == 0.0 {
        return 1.0;
    }
    if x <= 0.0 {
        return 0.0;
    }
    exp2(e * log2(x))
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

pub fn normalize(v: [f32; 3]) -> [f32; 3] {
    let l = crate::sqrt(dot(v, v) as f64) as f32;
    if l == 0.0 { v } else { [v[0] / l, v[1] / l, v[2] / l] }
}

// -----| state |-----

#[derive(Clone, Copy)]
pub struct Light {
    pub ambient: [f32; 4],
    pub diffuse: [f32; 4],
    pub specular: [f32; 4],
    /// In eye coordinates (transformed when set).
    pub position: [f32; 4],
    pub spot_dir: [f32; 3],
    pub spot_exp: f32,
    pub spot_cutoff: f32,
    /// Constant, linear, quadratic.
    pub atten: [f32; 3],
    pub on: bool,
}

impl Light {
    pub fn new(i: usize) -> Light {
        let one = if i == 0 { [1.0, 1.0, 1.0, 1.0] } else { [0.0, 0.0, 0.0, 1.0] };
        Light {
            ambient: [0.0, 0.0, 0.0, 1.0],
            diffuse: one,
            specular: one,
            position: [0.0, 0.0, 1.0, 0.0],
            spot_dir: [0.0, 0.0, -1.0],
            spot_exp: 0.0,
            spot_cutoff: 180.0,
            atten: [1.0, 0.0, 0.0],
            on: false,
        }
    }
}

#[derive(Clone, Copy)]
pub struct Material {
    pub ambient: [f32; 4],
    pub diffuse: [f32; 4],
    pub specular: [f32; 4],
    pub emission: [f32; 4],
    pub shininess: f32,
}

impl Material {
    pub const fn new() -> Material {
        Material {
            ambient: [0.2, 0.2, 0.2, 1.0],
            diffuse: [0.8, 0.8, 0.8, 1.0],
            specular: [0.0, 0.0, 0.0, 1.0],
            emission: [0.0, 0.0, 0.0, 1.0],
            shininess: 0.0,
        }
    }
}

#[derive(Clone)]
pub struct Lighting {
    pub lights: [Light; 8],
    /// Front and back.
    pub mat: [Material; 2],
    pub model_ambient: [f32; 4],
    pub local_viewer: bool,
    pub two_side: bool,
    pub cm_face: u32,
    pub cm_mode: u32,
}

impl Lighting {
    pub fn new() -> Lighting {
        Lighting {
            lights: core::array::from_fn(Light::new),
            mat: [Material::new(); 2],
            model_ambient: [0.2, 0.2, 0.2, 1.0],
            local_viewer: false,
            two_side: false,
            cm_face: GL_FRONT_AND_BACK,
            cm_mode: GL_AMBIENT_AND_DIFFUSE,
        }
    }

    /// glColorMaterial: the current colour becomes the chosen property.
    pub fn color_material(&mut self, c: [f32; 4]) {
        for (i, m) in self.mat.iter_mut().enumerate() {
            let face_ok = self.cm_face == GL_FRONT_AND_BACK || (self.cm_face == GL_FRONT) == (i == 0);
            if !face_ok {
                continue;
            }
            match self.cm_mode {
                GL_AMBIENT => m.ambient = c,
                GL_DIFFUSE => m.diffuse = c,
                GL_SPECULAR => m.specular = c,
                GL_EMISSION => m.emission = c,
                GL_AMBIENT_AND_DIFFUSE => {
                    m.ambient = c;
                    m.diffuse = c;
                }
                _ => {}
            }
        }
    }

    /// The lit colour of a vertex at `eye` with normal `n` (eye space), for
    /// face `face` (0 front, 1 back, whose normal is flipped).
    pub fn shade(&self, eye: [f32; 4], n: [f32; 3], face: usize) -> [f32; 4] {
        let m = &self.mat[face];
        let n = if face == 1 { [-n[0], -n[1], -n[2]] } else { n };
        let mut c = [0.0f32; 3];
        for k in 0..3 {
            c[k] = m.emission[k] + m.ambient[k] * self.model_ambient[k];
        }
        let v = if eye[3] != 0.0 { [eye[0] / eye[3], eye[1] / eye[3], eye[2] / eye[3]] } else { [eye[0], eye[1], eye[2]] };
        for l in self.lights.iter().filter(|l| l.on) {
            let (vp, att) = if l.position[3] == 0.0 {
                (normalize([l.position[0], l.position[1], l.position[2]]), 1.0)
            } else {
                let p = [l.position[0] / l.position[3], l.position[1] / l.position[3], l.position[2] / l.position[3]];
                let d = [p[0] - v[0], p[1] - v[1], p[2] - v[2]];
                let dist = crate::sqrt(dot(d, d) as f64) as f32;
                let a = l.atten[0] + l.atten[1] * dist + l.atten[2] * dist * dist;
                (normalize(d), if a > 0.0 { 1.0 / a } else { 1.0 })
            };
            let mut spot = 1.0f32;
            if l.spot_cutoff != 180.0 {
                let s = dot([-vp[0], -vp[1], -vp[2]], normalize(l.spot_dir));
                let (_, cos) = crate::sin_cos(l.spot_cutoff as f64 * core::f64::consts::PI / 180.0);
                spot = if (s as f64) < cos { 0.0 } else { pow(s.max(0.0) as f64, l.spot_exp as f64) as f32 };
            }
            let ndl = dot(n, vp);
            let diff = ndl.max(0.0);
            let mut spec = 0.0f32;
            if ndl > 0.0 {
                let eye_dir = if self.local_viewer { normalize([-v[0], -v[1], -v[2]]) } else { [0.0, 0.0, 1.0] };
                let h = normalize([vp[0] + eye_dir[0], vp[1] + eye_dir[1], vp[2] + eye_dir[2]]);
                let ndh = dot(n, h).max(0.0);
                spec = pow(ndh as f64, m.shininess as f64) as f32;
            }
            let k = att * spot;
            for i in 0..3 {
                c[i] += k * (l.ambient[i] * m.ambient[i] + diff * l.diffuse[i] * m.diffuse[i] + spec * l.specular[i] * m.specular[i]);
            }
        }
        [c[0].clamp(0.0, 1.0), c[1].clamp(0.0, 1.0), c[2].clamp(0.0, 1.0), m.diffuse[3].clamp(0.0, 1.0)]
    }
}

// -----| fog |-----

#[derive(Clone)]
pub struct Fog {
    pub mode: u32,
    pub density: f32,
    pub start: f32,
    pub end: f32,
    pub color: [f32; 4],
}

impl Fog {
    pub fn new() -> Fog {
        Fog { mode: GL_EXP, density: 1.0, start: 0.0, end: 1.0, color: [0.0; 4] }
    }

}
