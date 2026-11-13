//! An SM4/SM5 interpreter for the stages that run in the DLL (vertex
//! shaders here): one invocation at a time, registers as 32-bit patterns,
//! floats in single precision as Direct3D 10 asks (denormals flushed).

use alloc::vec;
use alloc::vec::Vec;

use crate::dxbc::{op, Ins, Opnd, Shader, T_CB, T_ICB, T_IMM32, T_INDEXABLE, T_INPUT, T_OUTPUT, T_TEMP};

pub type R4 = [u32; 4];

/// What an invocation reads besides its registers.
pub trait Env {
    /// Constant buffer `slot`'s register `reg` (zero past its end).
    fn cb(&self, slot: u32, reg: i64) -> R4;
    /// A texture or buffer read (ld, sample_l and the like): `ins` the
    /// instruction, `c` its coordinates, `x` its other sources.
    fn fetch(&self, ins: &Ins, c: R4, x: [R4; 2]) -> R4;
}

pub struct Vm<'a> {
    pub sh: &'a Shader,
    jumps: Vec<u32>,
    labels: Vec<usize>,
}

fn f(u: u32) -> f32 {
    f32::from_bits(u)
}

fn u(x: f32) -> u32 {
    // Denormals to zero, keeping the sign.
    let b = x.to_bits();
    if b & 0x7F80_0000 == 0 { b & 0x8000_0000 } else { b }
}

fn mask(b: bool) -> u32 {
    if b { u32::MAX } else { 0 }
}

/// Opcodes whose sources are integers (a negation is two's complement).
pub fn int_sources(o: u16) -> bool {
    use op::*;
    matches!(
        o,
        AND | OR | XOR | NOT | IADD | INEG | IMUL | UMUL | IMAD | UMAD | IMAX | IMIN | UMAX | UMIN | ISHL | ISHR | USHR | UDIV | IEQ | INE | IGE | ILT
            | UGE | ULT | ITOF | UTOF | BFREV | COUNTBITS | FIRSTBIT_HI | FIRSTBIT_LO | FIRSTBIT_SHI | UBFE | IBFE | BFI | SWITCH | CASE | LD | RESINFO
            | F16TOF32
    )
}

pub fn half_to_f32(h: u32) -> f32 {
    let (s, e, m) = ((h >> 15) & 1, (h >> 10) & 31, h & 1023);
    let b = if e == 0 {
        if m == 0 {
            s << 31
        } else {
            let (mut e, mut m) = (127 - 15 + 1, m);
            while m & 1024 == 0 {
                m <<= 1;
                e -= 1;
            }
            s << 31 | (e as u32) << 23 | (m & 1023) << 13
        }
    } else if e == 31 {
        s << 31 | 0xFF << 23 | m << 13
    } else {
        s << 31 | (e + 127 - 15) << 23 | m << 13
    };
    f32::from_bits(b)
}

pub fn f32_to_half(x: f32) -> u32 {
    let b = x.to_bits();
    let s = (b >> 16) & 0x8000;
    let e = ((b >> 23) & 255) as i32 - 127 + 15;
    let m = b & 0x7F_FFFF;
    if (b >> 23) & 255 == 255 {
        s | 0x7C00 | if m != 0 { 0x200 } else { 0 }
    } else if e >= 31 {
        s | 0x7C00
    } else if e <= 0 {
        s
    } else {
        let r = s | (e as u32) << 10 | (m >> 13);
        if m & 0x1000 != 0 { r + 1 } else { r }
    }
}

// -----| single precision functions (no libm here) |-----

/// Below 2^23 in size, so with a fraction (not NaN or infinite).
fn fractional(x: f32) -> bool {
    (x.to_bits() & 0x7FFF_FFFF) < 0x4B00_0000
}

pub fn floor(x: f32) -> f32 {
    if !fractional(x) {
        return x;
    }
    let t = x as i32 as f32;
    if t > x { t - 1.0 } else { t }
}

fn ceil(x: f32) -> f32 {
    -floor(-x)
}

fn trunc(x: f32) -> f32 {
    if fractional(x) { x as i32 as f32 } else { x }
}

pub fn sqrt(x: f32) -> f32 {
    use core::arch::x86_64::*;
    unsafe { _mm_cvtss_f32(_mm_sqrt_ss(_mm_set_ss(x))) }
}

pub fn exp2(x: f32) -> f32 {
    if x.is_nan() {
        return x;
    }
    if x > 128.0 {
        return f32::INFINITY;
    }
    if x < -150.0 {
        return 0.0;
    }
    let n = floor(x);
    let fr = (x - n) as f64 * core::f64::consts::LN_2;
    let (mut term, mut sum) = (1.0f64, 1.0f64);
    for k in 1..16 {
        term *= fr / k as f64;
        sum += term;
    }
    (sum * f64::from_bits(((n as i64 + 1023) as u64) << 52)) as f32
}

pub fn log2(x: f32) -> f32 {
    if x.is_nan() || x < 0.0 {
        return f32::NAN;
    }
    if x == 0.0 {
        return f32::NEG_INFINITY;
    }
    if x.is_infinite() {
        return f32::INFINITY;
    }
    let bits = (x as f64).to_bits();
    let e = ((bits >> 52) & 0x7FF) as i64 - 1023;
    let m = f64::from_bits((bits & ((1 << 52) - 1)) | (1023 << 52));
    let z = (m - 1.0) / (m + 1.0);
    let (mut t, mut s) = (z, 0.0f64);
    for k in 0..20 {
        s += t / (2 * k + 1) as f64;
        t *= z * z;
    }
    (2.0 * s * core::f64::consts::LOG2_E + e as f64) as f32
}

pub fn sincos(x: f32) -> (f32, f32) {
    if !x.is_finite() {
        return (f32::NAN, f32::NAN);
    }
    let pi = core::f64::consts::PI;
    let mut x = x as f64;
    let n = x / (2.0 * pi) + 0.5;
    let n = n as i64 as f64 - if (n as i64 as f64) > n { 1.0 } else { 0.0 };
    x -= 2.0 * pi * n;
    let (mut s, mut c, mut ts, mut tc) = (0.0, 0.0, x, 1.0);
    for k in 0..14 {
        s += ts;
        c += tc;
        ts *= -x * x / ((2 * k + 2) * (2 * k + 3)) as f64;
        tc *= -x * x / ((2 * k + 1) * (2 * k + 2)) as f64;
    }
    (s as f32, c as f32)
}

fn round_ne(x: f32) -> f32 {
    if !fractional(x) {
        return x;
    }
    let lo = floor(x);
    let d = x - lo;
    if d > 0.5 || (d == 0.5 && (lo as i64) & 1 == 1) { lo + 1.0 } else { lo }
}

fn ftoi(x: f32) -> u32 {
    if x.is_nan() { 0 } else { (x as i32) as u32 }
}

fn ftou(x: f32) -> u32 {
    if x.is_nan() { 0 } else { x as u32 }
}

fn bfe(w: u32, o: u32, v: u32, signed: bool) -> u32 {
    let (w, o) = (w & 31, o & 31);
    if w == 0 {
        0
    } else if w + o < 32 {
        let l = v << (32 - w - o);
        if signed { ((l as i32) >> (32 - w)) as u32 } else { l >> (32 - w) }
    } else if signed {
        ((v as i32) >> o) as u32
    } else {
        v >> o
    }
}

pub struct Regs {
    pub r: Vec<R4>,
    pub x: Vec<Vec<R4>>,
    pub v: Vec<R4>,
    pub o: Vec<R4>,
}

impl<'a> Vm<'a> {
    pub fn new(sh: &'a Shader) -> Option<Vm<'a>> {
        let jumps = crate::dxbc::jumps(&sh.ins)?;
        let mut labels = Vec::new();
        for (i, x) in sh.ins.iter().enumerate() {
            if x.op == op::LABEL {
                let l = x.ops.first()?.idx[0] as usize;
                if labels.len() <= l {
                    labels.resize(l + 1, usize::MAX);
                }
                labels[l] = i;
            }
        }
        Some(Vm { sh, jumps, labels })
    }

    pub fn regs(&self, inputs: usize, outputs: usize) -> Regs {
        Regs {
            r: vec![[0; 4]; self.sh.temps as usize],
            x: self.sh.xtemps.iter().map(|&(n, _)| vec![[0; 4]; n as usize]).collect(),
            v: vec![[0; 4]; inputs.max(32)],
            o: vec![[0; 4]; outputs.max(32)],
        }
    }

    fn index(&self, g: &Regs, o: &Opnd, k: usize) -> i64 {
        let mut i = o.idx[k] as i64;
        if let Some(r) = o.rel[k] {
            let v = match r.ty {
                T_TEMP => g.r.get(r.reg as usize).map_or(0, |v| v[r.comp as usize]),
                _ => g.x.get(r.reg as usize).and_then(|a| a.get(r.sub as usize)).map_or(0, |v| v[r.comp as usize]),
            };
            i += v as i32 as i64;
        }
        i
    }

    fn raw(&self, g: &Regs, o: &Opnd, env: &dyn Env) -> R4 {
        let z = [0; 4];
        match o.ty {
            T_TEMP => g.r.get(self.index(g, o, 0) as usize).copied().unwrap_or(z),
            T_INPUT => g.v.get(self.index(g, o, 0) as usize).copied().unwrap_or(z),
            T_OUTPUT => g.o.get(self.index(g, o, 0) as usize).copied().unwrap_or(z),
            T_INDEXABLE => g.x.get(o.idx[0] as usize).and_then(|a| a.get(self.index(g, o, 1) as usize)).copied().unwrap_or(z),
            T_IMM32 => o.imm,
            T_CB => env.cb(o.idx[0], self.index(g, o, 1)),
            T_ICB => {
                let i = self.index(g, o, 0);
                if i < 0 {
                    z
                } else {
                    core::array::from_fn(|k| self.sh.icb.get(i as usize * 4 + k).copied().unwrap_or(0))
                }
            }
            _ => z,
        }
    }

    /// A source: swizzled, with its modifier (`int`: as an integer).
    fn read(&self, g: &Regs, o: &Opnd, env: &dyn Env, int: bool) -> R4 {
        let v = self.raw(g, o, env);
        let v: R4 = core::array::from_fn(|k| v[((o.swz >> (2 * k)) & 3) as usize]);
        match (o.modi, int) {
            (0, _) => v,
            (1, true) => v.map(|x| x.wrapping_neg()),
            (2, true) => v.map(|x| (x as i32).unsigned_abs()),
            (3, true) => v.map(|x| ((x as i32).unsigned_abs()).wrapping_neg()),
            (1, false) => v.map(|x| x ^ 0x8000_0000),
            (2, false) => v.map(|x| x & 0x7FFF_FFFF),
            (3, false) => v.map(|x| x | 0x8000_0000),
            _ => v,
        }
    }

    fn write(&self, g: &mut Regs, o: &Opnd, v: R4, sat: bool) {
        let m = o.mask;
        let i = self.index(g, o, if o.ty == T_INDEXABLE { 1 } else { 0 });
        let reg: Option<&mut R4> = match o.ty {
            T_TEMP => g.r.get_mut(i as usize),
            T_OUTPUT => g.o.get_mut(i as usize),
            T_INDEXABLE => g.x.get_mut(o.idx[0] as usize).and_then(|a| a.get_mut(i as usize)),
            _ => None,
        };
        let Some(reg) = reg else { return };
        for k in 0..4 {
            if m & (1 << k) != 0 {
                reg[k] = if sat {
                    let x = f(v[k]);
                    u(if x.is_nan() { 0.0 } else { x.max(0.0).min(1.0) })
                } else {
                    v[k]
                };
            }
        }
    }

    /// Run one invocation.
    pub fn run(&self, g: &mut Regs, env: &dyn Env) {
        use op::*;
        let ins = &self.sh.ins;
        let mut pc = 0usize;
        let mut calls: Vec<usize> = Vec::new();
        let mut steps = 0u32;
        while pc < ins.len() {
            steps += 1;
            if steps > 1 << 20 {
                return;
            }
            let x = &ins[pc];
            pc += 1;
            let int = int_sources(x.op);
            let ns = x.ops.len() - x.ndst as usize;
            let mut s = [[0u32; 4]; 4];
            for k in 0..ns.min(4) {
                let o = x.src(k);
                if !matches!(o.ty, crate::dxbc::T_RESOURCE | crate::dxbc::T_SAMPLER | crate::dxbc::T_LABEL) {
                    s[k] = self.read(g, o, env, int);
                }
            }
            let test = |v: u32| (v != 0) == x.nonzero();
            match x.op {
                IF => {
                    if !test(s[0][0]) {
                        pc = self.jumps[pc - 1] as usize + 1;
                    }
                    continue;
                }
                ELSE => {
                    pc = self.jumps[pc - 1] as usize + 1;
                    continue;
                }
                ENDIF | LOOP | CASE | DEFAULT | LABEL | NOP | ENDSWITCH => {
                    // A label reached in a straight line: the end of main.
                    if x.op == LABEL {
                        return;
                    }
                    continue;
                }
                ENDLOOP => {
                    pc = self.jumps[pc - 1] as usize + 1;
                    continue;
                }
                BREAK | CONTINUE => {
                    pc = self.jumps[pc - 1] as usize + (x.op == BREAK) as usize;
                    continue;
                }
                BREAKC | CONTINUEC => {
                    if test(s[0][0]) {
                        pc = self.jumps[pc - 1] as usize + (x.op == BREAKC) as usize;
                    }
                    continue;
                }
                SWITCH => {
                    // The case with the value, else the default, else out.
                    let end = self.jumps[pc - 1] as usize;
                    let mut to = end;
                    let mut depth = 0;
                    let mut default = None;
                    for (i, c) in ins.iter().enumerate().take(end).skip(pc) {
                        match c.op {
                            SWITCH => depth += 1,
                            ENDSWITCH => depth -= 1,
                            CASE if depth == 0 && c.ops.first().is_some_and(|o| o.imm[0] == s[0][0]) => {
                                to = i;
                                break;
                            }
                            DEFAULT if depth == 0 => default = Some(i),
                            _ => {}
                        }
                    }
                    if to == end {
                        to = default.unwrap_or(end);
                    }
                    pc = to + 1;
                    continue;
                }
                RET | RETC => {
                    if x.op == RETC && !test(s[0][0]) {
                        continue;
                    }
                    match calls.pop() {
                        Some(r) => pc = r,
                        None => return,
                    }
                    continue;
                }
                CALL | CALLC => {
                    let l = x.ops.last().map_or(0, |o| o.idx[0] as usize);
                    let go = x.op == CALL || test(s[0][0]);
                    if go && calls.len() < 32 {
                        if let Some(&t) = self.labels.get(l).filter(|&&t| t != usize::MAX) {
                            calls.push(pc);
                            pc = t + 1;
                        }
                    }
                    continue;
                }
                DISCARD | EMIT | CUT | EMITTHENCUT => continue,
                SINCOS => {
                    let a = s[0];
                    let sn = a.map(|v| u(sincos(f(v)).0));
                    let cs = a.map(|v| u(sincos(f(v)).1));
                    self.write(g, x.dst(0), sn, x.sat);
                    self.write(g, x.dst(1), cs, x.sat);
                    continue;
                }
                UDIV => {
                    let q: R4 = core::array::from_fn(|k| if s[1][k] == 0 { u32::MAX } else { s[0][k] / s[1][k] });
                    let r: R4 = core::array::from_fn(|k| if s[1][k] == 0 { u32::MAX } else { s[0][k] % s[1][k] });
                    self.write(g, x.dst(0), q, false);
                    self.write(g, x.dst(1), r, false);
                    continue;
                }
                IMUL | UMUL => {
                    let wide: [u64; 4] = core::array::from_fn(|k| {
                        if x.op == IMUL { ((s[0][k] as i32 as i64) * (s[1][k] as i32 as i64)) as u64 } else { s[0][k] as u64 * s[1][k] as u64 }
                    });
                    self.write(g, x.dst(0), wide.map(|w| (w >> 32) as u32), false);
                    self.write(g, x.dst(1), wide.map(|w| w as u32), false);
                    continue;
                }
                SWAPC => {
                    let a: R4 = core::array::from_fn(|k| if s[0][k] != 0 { s[2][k] } else { s[1][k] });
                    let b: R4 = core::array::from_fn(|k| if s[0][k] != 0 { s[1][k] } else { s[2][k] });
                    self.write(g, x.dst(0), a, false);
                    self.write(g, x.dst(1), b, false);
                    continue;
                }
                _ => {}
            }
            let (a, b, c) = (s[0], s[1], s[2]);
            let ff = |h: &dyn Fn(f32) -> f32| -> R4 { a.map(|v| u(h(f(v)))) };
            let f2 = |h: &dyn Fn(f32, f32) -> f32| -> R4 { core::array::from_fn(|k| u(h(f(a[k]), f(b[k])))) };
            let fc = |h: &dyn Fn(f32, f32) -> bool| -> R4 { core::array::from_fn(|k| mask(h(f(a[k]), f(b[k])))) };
            let i2 = |h: &dyn Fn(u32, u32) -> u32| -> R4 { core::array::from_fn(|k| h(a[k], b[k])) };
            let i3 = |h: &dyn Fn(u32, u32, u32) -> u32| -> R4 { core::array::from_fn(|k| h(a[k], b[k], c[k])) };
            let dot = |n: usize| -> R4 {
                let mut t = 0.0f32;
                for k in 0..n {
                    t += f(a[k]) * f(b[k]);
                }
                [u(t); 4]
            };
            let v: R4 = match x.op {
                MOV => a,
                ADD => f2(&|p, q| p + q),
                MUL => f2(&|p, q| p * q),
                DIV => f2(&|p, q| p / q),
                MAD => core::array::from_fn(|k| u(f(a[k]) * f(b[k]) + f(c[k]))),
                MIN => f2(&|p, q| if q.is_nan() || p < q { p } else { q }),
                MAX => f2(&|p, q| if q.is_nan() || p >= q { p } else { q }),
                DP2 => dot(2),
                DP3 => dot(3),
                DP4 => dot(4),
                EXP => ff(&|p| exp2(p)),
                LOG => ff(&|p| log2(p)),
                RSQ => ff(&|p| 1.0 / sqrt(p)),
                SQRT => ff(&sqrt),
                RCP => ff(&|p| 1.0 / p),
                FRC => ff(&|p| p - floor(p)),
                ROUND_NE => ff(&round_ne),
                ROUND_NI => ff(&floor),
                ROUND_PI => ff(&ceil),
                ROUND_Z => ff(&trunc),
                EQ => fc(&|p, q| p == q),
                NE => fc(&|p, q| p != q),
                LT => fc(&|p, q| p < q),
                GE => fc(&|p, q| p >= q),
                FTOI => a.map(|v| ftoi(f(v))),
                FTOU => a.map(|v| ftou(f(v))),
                ITOF => a.map(|v| u(v as i32 as f32)),
                UTOF => a.map(|v| u(v as f32)),
                AND => i2(&|p, q| p & q),
                OR => i2(&|p, q| p | q),
                XOR => i2(&|p, q| p ^ q),
                NOT => a.map(|v| !v),
                IADD => i2(&|p, q| p.wrapping_add(q)),
                INEG => a.map(|v| v.wrapping_neg()),
                IMAD | UMAD => i3(&|p, q, r| p.wrapping_mul(q).wrapping_add(r)),
                IMAX => i2(&|p, q| (p as i32).max(q as i32) as u32),
                IMIN => i2(&|p, q| (p as i32).min(q as i32) as u32),
                UMAX => i2(&|p, q| p.max(q)),
                UMIN => i2(&|p, q| p.min(q)),
                ISHL => i2(&|p, q| p << (q & 31)),
                ISHR => i2(&|p, q| ((p as i32) >> (q & 31)) as u32),
                USHR => i2(&|p, q| p >> (q & 31)),
                IEQ => i2(&|p, q| mask(p == q)),
                INE => i2(&|p, q| mask(p != q)),
                IGE => i2(&|p, q| mask(p as i32 >= q as i32)),
                ILT => i2(&|p, q| mask((p as i32) < q as i32)),
                UGE => i2(&|p, q| mask(p >= q)),
                ULT => i2(&|p, q| mask(p < q)),
                MOVC => i3(&|p, q, r| if p != 0 { q } else { r }),
                BFREV => a.map(|v| v.reverse_bits()),
                COUNTBITS => a.map(|v| v.count_ones()),
                FIRSTBIT_HI => a.map(|v| if v == 0 { u32::MAX } else { v.leading_zeros() }),
                FIRSTBIT_LO => a.map(|v| if v == 0 { u32::MAX } else { v.trailing_zeros() }),
                FIRSTBIT_SHI => a.map(|v| {
                    let w = if (v as i32) < 0 { !v } else { v };
                    if w == 0 { u32::MAX } else { w.leading_zeros() }
                }),
                UBFE => i3(&|w, o, v| bfe(w, o, v, false)),
                IBFE => i3(&|w, o, v| bfe(w, o, v, true)),
                BFI => core::array::from_fn(|k| {
                    let (w, o) = (a[k] & 31, b[k] & 31);
                    let m = (((1u64 << w) - 1) << o) as u32;
                    ((c[k] << o) & m) | (s[3][k] & !m)
                }),
                F32TOF16 => a.map(|v| f32_to_half(f(v))),
                F16TOF32 => a.map(|v| u(half_to_f32(v & 0xFFFF))),
                LD | SAMPLE_L | SAMPLE | SAMPLE_B | SAMPLE_D | SAMPLE_C | SAMPLE_C_LZ | RESINFO | GATHER4 => {
                    let r = env.fetch(x, a, [b, c]);
                    // The resource's swizzle picks the result's components.
                    let t = x.ops.iter().find(|o| o.ty == crate::dxbc::T_RESOURCE).map_or(0xE4, |o| o.swz);
                    core::array::from_fn(|k| r[((t >> (2 * k)) & 3) as usize])
                }
                // Derivatives are zero outside pixel shaders.
                _ => [0; 4],
            };
            self.write(g, x.dst(0), v, x.sat);
        }
    }
}
