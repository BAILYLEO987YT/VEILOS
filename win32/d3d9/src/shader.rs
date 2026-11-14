//! Direct3D 9 shaders (WIN32_DESIGN.md G5e): the bytecode (vs_1_1 to
//! vs_3_0, ps_1_1 to ps_3_0) read into instructions, and vertex shaders
//! run here, one vertex at a time, with real floating point.
//!
//! The bytecode: a version token, then instructions until END. An
//! instruction token (bit 31 clear) has the opcode in bits 0-15, controls
//! in 16-23, and is followed by its parameters (bit 31 set): a destination
//! (register type, number, write mask, saturate, shift), an optional
//! predicate, sources (type, number, swizzle, modifier, and an address
//! register token when relatively addressed). DEF's values come raw.

use alloc::boxed::Box;
use alloc::vec::Vec;

use gfx3d::raster3d::{pf, PsArgs, PsIns, PS_INPUTS};

use crate::com::*;
use crate::vtbl;

// Opcodes (D3DSIO_*).
pub const NOP: u16 = 0;
pub const MOV: u16 = 1;
pub const ADD: u16 = 2;
pub const SUB: u16 = 3;
pub const MAD: u16 = 4;
pub const MUL: u16 = 5;
pub const RCP: u16 = 6;
pub const RSQ: u16 = 7;
pub const DP3: u16 = 8;
pub const DP4: u16 = 9;
pub const MIN: u16 = 10;
pub const MAX: u16 = 11;
pub const SLT: u16 = 12;
pub const SGE: u16 = 13;
pub const EXP: u16 = 14;
pub const LOG: u16 = 15;
pub const LIT: u16 = 16;
pub const DST: u16 = 17;
pub const LRP: u16 = 18;
pub const FRC: u16 = 19;
pub const M4X4: u16 = 20;
pub const M4X3: u16 = 21;
pub const M3X4: u16 = 22;
pub const M3X3: u16 = 23;
pub const M3X2: u16 = 24;
pub const CALL: u16 = 25;
pub const CALLNZ: u16 = 26;
pub const LOOP: u16 = 27;
pub const RET: u16 = 28;
pub const ENDLOOP: u16 = 29;
pub const LABEL: u16 = 30;
pub const DCL: u16 = 31;
pub const POW: u16 = 32;
pub const CRS: u16 = 33;
pub const SGN: u16 = 34;
pub const ABS: u16 = 35;
pub const NRM: u16 = 36;
pub const SINCOS: u16 = 37;
pub const REP: u16 = 38;
pub const ENDREP: u16 = 39;
pub const IF: u16 = 40;
pub const IFC: u16 = 41;
pub const ELSE: u16 = 42;
pub const ENDIF: u16 = 43;
pub const BREAK: u16 = 44;
pub const BREAKC: u16 = 45;
pub const MOVA: u16 = 46;
pub const DEFB: u16 = 47;
pub const DEFI: u16 = 48;
pub const TEXKILL: u16 = 65;
pub const EXPP: u16 = 78;
pub const LOGP: u16 = 79;
pub const CND: u16 = 80;
pub const DEF: u16 = 81;
pub const CMP: u16 = 88;
pub const DP2ADD: u16 = 90;
pub const SETP: u16 = 94;
pub const TEXLDL: u16 = 95;
pub const BREAKP: u16 = 96;
pub const PHASE: u16 = 0xFFFD;
pub const COMMENT: u16 = 0xFFFE;
pub const END: u16 = 0xFFFF;

// Register types (D3DSPR_*).
pub const R_TEMP: u8 = 0;
pub const R_INPUT: u8 = 1;
pub const R_CONST: u8 = 2;
/// The address register (vertex shaders) or a texture register (ps_1_x).
pub const R_ADDR: u8 = 3;
pub const R_RASTOUT: u8 = 4;
pub const R_ATTROUT: u8 = 5;
/// oTn (vs_1_1-2_x), or vs_3_0's outputs.
pub const R_OUTPUT: u8 = 6;
pub const R_CONSTINT: u8 = 7;
pub const R_COLOROUT: u8 = 8;
pub const R_DEPTHOUT: u8 = 9;
pub const R_CONSTBOOL: u8 = 14;
pub const R_LOOP: u8 = 15;
pub const R_MISC: u8 = 17;
pub const R_PREDICATE: u8 = 19;

// Usages (D3DDECLUSAGE_*).
pub const U_POSITION: u8 = 0;
pub const U_PSIZE: u8 = 4;
pub const U_TEXCOORD: u8 = 5;
pub const U_COLOR: u8 = 10;
pub const U_FOG: u8 = 11;

#[derive(Clone, Copy, Debug, Default)]
pub struct Src {
    pub rtype: u8,
    pub num: u16,
    /// Each component's source component.
    pub swz: [u8; 4],
    /// D3DSPSM_*.
    pub modifier: u8,
    /// Relatively addressed: by this register's component (a0 or aL).
    pub rel: Option<(u8, u16, u8)>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Dst {
    pub rtype: u8,
    pub num: u16,
    pub mask: u8,
    pub saturate: bool,
    /// ps_1_x's result shift: x2, x4, x8, d2, d4, d8 as -3 ..= 3.
    pub shift: i8,
    pub rel: Option<(u8, u16, u8)>,
}

#[derive(Clone, Debug)]
pub struct Ins {
    pub op: u16,
    /// Opcode-specific controls (comparison, texld's project/bias).
    pub ctrl: u8,
    pub dst: Option<Dst>,
    pub src: Vec<Src>,
    /// Predicated: by this predicate register swizzle (negated if the
    /// modifier is NOT).
    pub pred: Option<Src>,
    /// ps_1_x: runs with the one before (co-issue).
    pub coissue: bool,
    /// Where block instructions jump (filled after parsing).
    pub jump: usize,
}

/// A declared register: its usage, sampler type.
#[derive(Clone, Copy, Debug)]
pub struct Decl {
    pub rtype: u8,
    pub num: u16,
    pub usage: u8,
    pub index: u8,
    pub mask: u8,
    /// Samplers: D3DSTT_2D (2), CUBE (3), VOLUME (4).
    pub sampler: u8,
}

pub struct Shader {
    pub pixel: bool,
    pub major: u8,
    pub minor: u8,
    pub tokens: Vec<u32>,
    pub ins: Vec<Ins>,
    pub decls: Vec<Decl>,
    pub defs: Vec<(u16, [f32; 4])>,
    pub defi: Vec<(u16, [i32; 4])>,
    pub defb: Vec<(u16, bool)>,
    /// Each label's instruction.
    pub labels: Vec<(u16, usize)>,
    /// A pixel shader: the kernel's program, and its inputs' usages in
    /// the order the program numbers them.
    pub kernel: u64,
    pub varyings: Vec<(u8, u8)>,
}

impl Drop for Shader {
    fn drop(&mut self) {
        crate::res::close_later(self.kernel);
    }
}

fn src_param(t: u32, extra: Option<u32>, vs11: bool) -> Src {
    let rtype = (((t >> 28) & 7) | ((t >> 8) & 0x18)) as u8;
    let sw = (t >> 16) & 0xFF;
    let rel = if t & (1 << 13) != 0 {
        match extra {
            Some(r) => Some(((((r >> 28) & 7) | ((r >> 8) & 0x18)) as u8, (r & 0x7FF) as u16, ((r >> 16) & 3) as u8)),
            // vs_1_1: a0.x, implied.
            None if vs11 => Some((R_ADDR, 0, 0)),
            None => None,
        }
    } else {
        None
    };
    Src { rtype, num: (t & 0x7FF) as u16, swz: [(sw & 3) as u8, (sw >> 2 & 3) as u8, (sw >> 4 & 3) as u8, (sw >> 6 & 3) as u8], modifier: ((t >> 24) & 15) as u8, rel }
}

fn dst_param(t: u32, extra: Option<u32>) -> Dst {
    let shift = ((t >> 24) & 15) as i8;
    Dst {
        rtype: (((t >> 28) & 7) | ((t >> 8) & 0x18)) as u8,
        num: (t & 0x7FF) as u16,
        mask: ((t >> 16) & 15) as u8,
        saturate: (t >> 20) & 1 != 0,
        shift: if shift >= 8 { shift - 16 } else { shift },
        rel: if t & (1 << 13) != 0 { extra.map(|r| ((((r >> 28) & 7) | ((r >> 8) & 0x18)) as u8, (r & 0x7FF) as u16, ((r >> 16) & 3) as u8)) } else { None },
    }
}

/// Whether the opcode has no destination (all parameters are sources).
fn no_dest(op: u16) -> bool {
    matches!(op, CALL | CALLNZ | LOOP | RET | ENDLOOP | LABEL | REP | ENDREP | IF | IFC | ELSE | ENDIF | BREAK | BREAKC | BREAKP | NOP | PHASE)
}

/// Read a shader's bytecode. None if it isn't one we can run.
pub fn parse(tokens: &[u32]) -> Option<Shader> {
    let ver = *tokens.first()?;
    let pixel = match ver >> 16 {
        0xFFFF => true,
        0xFFFE => false,
        _ => return None,
    };
    let (major, minor) = ((ver >> 8 & 0xFF) as u8, (ver & 0xFF) as u8);
    if !(1..=3).contains(&major) {
        return None;
    }
    let vs11 = !pixel && major == 1;
    let mut s = Shader {
        pixel,
        major,
        minor,
        tokens: Vec::new(),
        ins: Vec::new(),
        decls: Vec::new(),
        defs: Vec::new(),
        defi: Vec::new(),
        defb: Vec::new(),
        labels: Vec::new(),
        kernel: 0,
        varyings: Vec::new(),
    };
    let mut i = 1;
    loop {
        let t = *tokens.get(i)?;
        let op = (t & 0xFFFF) as u16;
        i += 1;
        if op == END as u16 {
            break;
        }
        if op == COMMENT {
            i += ((t >> 16) & 0x7FFF) as usize;
            continue;
        }
        // Opcodes Direct3D 9 has (vertex shaders: none of the texture ones).
        let known = matches!(op, 0..=48 | 78..=81 | 88..=96 | PHASE) || (pixel && matches!(op, 64..=74 | 76..=77 | 82..=87));
        if !known || (!pixel && matches!(op, 80 | 88 | 89 | 91..=93)) {
            return None;
        }
        // The parameters: up to the next token with bit 31 clear (DEF's
        // values may have it either way, so they're counted).
        let n = match op {
            DEF | DEFI => 5,
            DEFB => 2,
            _ => {
                let mut n = 0;
                while tokens.get(i + n).is_some_and(|p| p >> 31 == 1) {
                    n += 1;
                }
                n
            }
        };
        let p = tokens.get(i..i + n)?;
        i += n;
        match op {
            DEF => {
                let d = dst_param(p[0], None);
                s.defs.push((d.num, core::array::from_fn(|k| f32::from_bits(p[1 + k]))));
                continue;
            }
            DEFI => {
                let d = dst_param(p[0], None);
                s.defi.push((d.num, core::array::from_fn(|k| p[1 + k] as i32)));
                continue;
            }
            DEFB => {
                let d = dst_param(p[0], None);
                s.defb.push((d.num, p[1] != 0));
                continue;
            }
            DCL => {
                let (u, d) = (p.first()?, dst_param(*p.get(1)?, None));
                s.decls.push(Decl { rtype: d.rtype, num: d.num, usage: (u & 31) as u8, index: ((u >> 16) & 15) as u8, mask: d.mask, sampler: ((u >> 27) & 15) as u8 });
                continue;
            }
            _ => {}
        }
        // Split into destination, predicate and sources; relative
        // addressing tokens follow their parameter (vs_2_0 and up).
        let mut params: Vec<(u32, Option<u32>)> = Vec::new();
        let mut k = 0;
        while k < p.len() {
            let tok = p[k];
            let rel = tok & (1 << 13) != 0 && !vs11;
            if rel && k + 1 < p.len() {
                params.push((tok, Some(p[k + 1])));
                k += 2;
            } else {
                params.push((tok, None));
                k += 1;
            }
        }
        let has_dest = !no_dest(op) && !params.is_empty();
        let predicated = t & (1 << 28) != 0;
        let mut it = params.into_iter();
        let dst = if has_dest { it.next().map(|(t, r)| dst_param(t, r)) } else { None };
        let pred = if predicated { it.next().map(|(t, r)| src_param(t, r, vs11)) } else { None };
        let src: Vec<Src> = it.map(|(t, r)| src_param(t, r, vs11)).collect();
        if op == LABEL {
            if let Some(l) = src.first() {
                s.labels.push((l.num, s.ins.len()));
            }
        }
        s.ins.push(Ins { op, ctrl: ((t >> 16) & 0xFF) as u8, dst, src, pred, coissue: t & (1 << 30) != 0, jump: 0 });
    }
    s.tokens = tokens[..i].to_vec();
    link(&mut s)?;
    Some(s)
}

/// Match up blocks: IF/ELSE/ENDIF, LOOP/ENDLOOP, REP/ENDREP; BREAK goes to
/// its loop's end.
fn link(s: &mut Shader) -> Option<()> {
    let mut stack: Vec<usize> = Vec::new();
    for i in 0..s.ins.len() {
        match s.ins[i].op {
            IF | IFC | LOOP | REP => stack.push(i),
            ELSE => {
                let o = *stack.last()?;
                s.ins[o].jump = i;
                *stack.last_mut()? = i;
            }
            ENDIF => {
                let o = stack.pop()?;
                s.ins[o].jump = i;
            }
            ENDLOOP | ENDREP => {
                let o = stack.pop()?;
                s.ins[o].jump = i;
                s.ins[i].jump = o;
            }
            BREAK | BREAKC | BREAKP => {
                // The innermost loop's end: found when it closes.
                let l = stack.iter().rev().find(|&&o| matches!(s.ins[o].op, LOOP | REP)).copied()?;
                s.ins[i].jump = l;
            }
            _ => {}
        }
    }
    // BREAK points at its loop's start; that start now knows its end.
    for i in 0..s.ins.len() {
        if matches!(s.ins[i].op, BREAK | BREAKC | BREAKP) {
            let l = s.ins[i].jump;
            s.ins[i].jump = s.ins[l].jump;
        }
    }
    stack.is_empty().then_some(())
}

// -----| the arithmetic, shared by vertex and pixel shaders |-----

pub fn compare(c: u8, a: f32, b: f32) -> bool {
    match c {
        1 => a > b,
        2 => a == b,
        3 => a >= b,
        4 => a < b,
        5 => a != b,
        6 => a <= b,
        _ => false,
    }
}

fn floor(x: f32) -> f32 {
    let t = x as i64 as f32;
    if t > x { t - 1.0 } else { t }
}

fn exp2(x: f32) -> f32 {
    if x > 128.0 {
        return f32::INFINITY;
    }
    if x < -150.0 {
        return 0.0;
    }
    let n = floor(x);
    let f = (x - n) as f64 * core::f64::consts::LN_2;
    let (mut term, mut sum) = (1.0f64, 1.0f64);
    for k in 1..16 {
        term *= f / k as f64;
        sum += term;
    }
    (sum * f64::from_bits(((n as i64 + 1023) as u64) << 52)) as f32
}

fn log2(x: f32) -> f32 {
    if x <= 0.0 {
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

fn sqrt(x: f32) -> f32 {
    if x <= 0.0 {
        return 0.0;
    }
    let x = x as f64;
    let mut y = f64::from_bits((x.to_bits() >> 1) + (1023u64 << 51));
    for _ in 0..6 {
        y = 0.5 * (y + x / y);
    }
    y as f32
}

fn sincos(x: f32) -> (f32, f32) {
    let pi = core::f64::consts::PI;
    let mut x = x as f64;
    x -= 2.0 * pi * (floor((x / (2.0 * pi) + 0.5) as f32) as f64);
    let (mut s, mut c, mut ts, mut tc) = (0.0, 0.0, x, 1.0);
    for k in 0..12 {
        s += ts;
        c += tc;
        ts *= -x * x / ((2 * k + 2) * (2 * k + 3)) as f64;
        tc *= -x * x / ((2 * k + 1) * (2 * k + 2)) as f64;
    }
    (s as f32, c as f32)
}

pub fn pow(x: f32, y: f32) -> f32 {
    exp2(y * log2(x.abs()))
}

/// Apply a source modifier.
pub fn modify(v: [f32; 4], m: u8) -> [f32; 4] {
    v.map(|x| match m {
        1 => -x,
        2 => x - 0.5,
        3 => -(x - 0.5),
        4 => 2.0 * (x - 0.5),
        5 => -2.0 * (x - 0.5),
        6 => 1.0 - x,
        7 => 2.0 * x,
        8 => -2.0 * x,
        11 => x.abs(),
        12 => -x.abs(),
        _ => x,
    })
}

/// One arithmetic instruction on its source values: the result (all four
/// components; the write mask picks), or None if it isn't arithmetic.
pub fn alu(op: u16, s: &[[f32; 4]], major: u8, vertex: bool) -> Option<[f32; 4]> {
    let a = s.first().copied().unwrap_or_default();
    let b = s.get(1).copied().unwrap_or_default();
    let c = s.get(2).copied().unwrap_or_default();
    let map2 = |f: &dyn Fn(f32, f32) -> f32| -> [f32; 4] { core::array::from_fn(|i| f(a[i], b[i])) };
    let dp3 = |a: [f32; 4], b: [f32; 4]| a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
    let dp4 = |a: [f32; 4], b: [f32; 4]| dp3(a, b) + a[3] * b[3];
    Some(match op {
        MOV => a,
        ADD => map2(&|x, y| x + y),
        SUB => map2(&|x, y| x - y),
        MUL => map2(&|x, y| x * y),
        MAD => core::array::from_fn(|i| a[i] * b[i] + c[i]),
        MIN => map2(&|x, y| if x < y { x } else { y }),
        MAX => map2(&|x, y| if x >= y { x } else { y }),
        SLT => map2(&|x, y| if x < y { 1.0 } else { 0.0 }),
        SGE => map2(&|x, y| if x >= y { 1.0 } else { 0.0 }),
        DP3 => [dp3(a, b); 4],
        DP4 => [dp4(a, b); 4],
        DP2ADD => [a[0] * b[0] + a[1] * b[1] + c[0]; 4],
        RCP => [if a[0] == 0.0 { f32::INFINITY } else { 1.0 / a[0] }; 4],
        RSQ => {
            let x = a[0].abs();
            [if x == 0.0 { f32::INFINITY } else { 1.0 / sqrt(x) }; 4]
        }
        EXP => [exp2(a[0]); 4],
        LOG => [log2(a[0].abs()); 4],
        EXPP if vertex && major == 1 => {
            let f = floor(a[0]);
            [exp2(f), a[0] - f, exp2(a[0]), 1.0]
        }
        EXPP => [exp2(a[0]); 4],
        LOGP if vertex && major == 1 => {
            let x = a[0].abs();
            if x == 0.0 {
                [f32::NEG_INFINITY, 1.0, f32::NEG_INFINITY, 1.0]
            } else {
                let e = floor(log2(x));
                [e, x / exp2(e), log2(x), 1.0]
            }
        }
        LOGP => [log2(a[0].abs()); 4],
        LIT => {
            let p = a[3].clamp(-128.0, 128.0);
            [1.0, a[0].max(0.0), if a[0] > 0.0 && a[1] > 0.0 { pow(a[1], p) } else { 0.0 }, 1.0]
        }
        DST => [1.0, a[1] * b[1], a[2], b[3]],
        LRP => core::array::from_fn(|i| a[i] * (b[i] - c[i]) + c[i]),
        FRC => a.map(|x| x - floor(x)),
        POW => [pow(a[0], b[0]); 4],
        CRS => [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0], 0.0],
        SGN => a.map(|x| if x > 0.0 { 1.0 } else if x < 0.0 { -1.0 } else { 0.0 }),
        ABS => a.map(|x| x.abs()),
        NRM => {
            let l = dp3(a, a);
            let r = if l == 0.0 { f32::INFINITY } else { 1.0 / sqrt(l) };
            a.map(|x| x * r)
        }
        // The angle is a replicated component; x gets the cosine, y the sine.
        SINCOS => {
            let (sn, cs) = sincos(a[0]);
            [cs, sn, 0.0, 0.0]
        }
        CMP => core::array::from_fn(|i| if a[i] >= 0.0 { b[i] } else { c[i] }),
        CND => core::array::from_fn(|i| if a[i] > 0.5 { b[i] } else { c[i] }),
        _ => return None,
    })
}

// -----| vertex shaders |-----

/// A vertex shader's outputs, by usage: (usage, index, value).
pub type Outputs = Vec<(u8, u8, [f32; 4])>;

/// The registers a vertex shader reads that aren't its own.
pub struct VsConsts<'a> {
    pub f: &'a [[f32; 4]],
    pub i: &'a [[i32; 4]; 16],
    pub b: &'a [i32; 16],
}

impl Shader {
    /// The usage each output register has: vs_3_0 declares them; before
    /// that they're fixed (oPos, oFog, oPts, oD0-1, oT0-7).
    fn output_usage(&self, rtype: u8, num: u16) -> Option<(u8, u8)> {
        if self.major >= 3 {
            return self.decls.iter().find(|d| d.rtype == R_OUTPUT && d.num == num).map(|d| (d.usage, d.index));
        }
        match (rtype, num) {
            (R_RASTOUT, 0) => Some((U_POSITION, 0)),
            (R_RASTOUT, 1) => Some((U_FOG, 0)),
            (R_RASTOUT, 2) => Some((U_PSIZE, 0)),
            (R_ATTROUT, n) => Some((U_COLOR, n as u8)),
            (R_OUTPUT, n) => Some((U_TEXCOORD, n as u8)),
            _ => None,
        }
    }

    /// Run as a vertex shader on `inputs` (v0-v15).
    pub fn run_vs(&self, inputs: &[[f32; 4]; 16], k: &VsConsts) -> Outputs {
        let mut r = [[0.0f32; 4]; 32];
        let mut o = [[0.0f32; 4]; 16];
        let mut written = [false; 16];
        // Output slots: vs_3_0's o-registers by number, else oPos 0, oFog 1,
        // oPts 2, oD 3-4, oT 5-12.
        let slot = |rt: u8, n: u16| -> Option<usize> {
            if self.major >= 3 {
                (rt == R_OUTPUT && n < 12).then_some(n as usize)
            } else {
                match rt {
                    R_RASTOUT if n < 3 => Some(n as usize),
                    R_ATTROUT if n < 2 => Some(3 + n as usize),
                    R_OUTPUT if n < 8 => Some(5 + n as usize),
                    _ => None,
                }
            }
        };
        let mut a0 = [0i32; 4];
        let mut al = 0i32;
        let mut p = [false; 4];
        let consts = |n: i32| -> [f32; 4] {
            if n < 0 {
                return [0.0; 4];
            }
            if let Some(d) = self.defs.iter().rev().find(|d| d.0 as i32 == n) {
                return d.1;
            }
            k.f.get(n as usize).copied().unwrap_or([0.0; 4])
        };
        let int_const = |n: u16| self.defi.iter().rev().find(|d| d.0 == n).map(|d| d.1).unwrap_or(k.i[(n as usize) & 15]);
        let bool_const = |n: u16| self.defb.iter().rev().find(|d| d.0 == n).map(|d| d.1).unwrap_or(k.b[(n as usize) & 15] != 0);
        let mut loops: Vec<(usize, i32, i32, i32)> = Vec::new(); // (start, left, aL saved, step)
        let mut calls: Vec<usize> = Vec::new();
        let mut pc = 0;
        let mut steps = 0u32;
        while pc < self.ins.len() {
            steps += 1;
            if steps > 1 << 20 {
                break;
            }
            let ins = &self.ins[pc];
            pc += 1;
            let read = |s: &Src, r: &[[f32; 4]; 32], o: &[[f32; 4]; 16], a0: &[i32; 4], al: i32, p: &[bool; 4]| -> [f32; 4] {
                let off = match s.rel {
                    Some((R_LOOP, _, _)) => al,
                    Some((_, _, c)) => a0[c as usize & 3],
                    None => 0,
                };
                let v = match s.rtype {
                    R_TEMP => r[(s.num as usize) & 31],
                    R_INPUT => inputs[((s.num as i32 + off).clamp(0, 15)) as usize],
                    R_CONST => consts(s.num as i32 + off),
                    R_ADDR => a0.map(|x| x as f32),
                    R_CONSTINT => int_const(s.num).map(|x| x as f32),
                    R_CONSTBOOL => [if bool_const(s.num) { 1.0 } else { 0.0 }; 4],
                    R_LOOP => [al as f32; 4],
                    R_PREDICATE => p.map(|x| if x { 1.0 } else { 0.0 }),
                    t => match slot(t, s.num) {
                        Some(i) => o[i],
                        None => [0.0; 4],
                    },
                };
                modify(core::array::from_fn(|i| v[s.swz[i] as usize]), s.modifier)
            };
            let srcs: Vec<[f32; 4]> = ins.src.iter().map(|s| read(s, &r, &o, &a0, al, &p)).collect();
            let pred_ok = |pr: &Option<Src>, p: &[bool; 4]| -> [bool; 4] {
                match pr {
                    Some(s) => core::array::from_fn(|i| p[s.swz[i] as usize] != (s.modifier == 13)),
                    None => [true; 4],
                }
            };
            match ins.op {
                NOP | LABEL | DCL | DEF | DEFI | DEFB | PHASE => {}
                IF => {
                    if !bool_const(ins.src[0].num) {
                        pc = self.skip_if(pc - 1);
                    }
                }
                IFC => {
                    if !compare(ins.ctrl & 7, srcs[0][0], srcs[1][0]) {
                        pc = self.skip_if(pc - 1);
                    }
                }
                ELSE => pc = ins.jump,
                ENDIF => {}
                REP => {
                    let n = int_const(ins.src[0].num)[0];
                    if n <= 0 {
                        pc = ins.jump + 1;
                    } else {
                        loops.push((pc, n, al, 0));
                    }
                }
                LOOP => {
                    let i = int_const(ins.src[1].num);
                    if i[0] <= 0 {
                        pc = ins.jump + 1;
                    } else {
                        loops.push((pc, i[0], al, i[2]));
                        al = i[1];
                    }
                }
                ENDREP | ENDLOOP => {
                    if let Some(l) = loops.last_mut() {
                        l.1 -= 1;
                        if l.1 > 0 {
                            al += l.3;
                            pc = l.0;
                        } else {
                            al = l.2;
                            loops.pop();
                        }
                    }
                }
                BREAK | BREAKC | BREAKP => {
                    let go = match ins.op {
                        BREAK => true,
                        BREAKC => compare(ins.ctrl & 7, srcs[0][0], srcs[1][0]),
                        _ => {
                            let s = &ins.src[0];
                            p[s.swz[0] as usize] != (s.modifier == 13)
                        }
                    };
                    if go {
                        if let Some(l) = loops.pop() {
                            al = l.2;
                        }
                        pc = ins.jump + 1;
                    }
                }
                CALL | CALLNZ => {
                    let go = ins.op == CALL || {
                        let s = &ins.src[1];
                        if s.rtype == R_PREDICATE { p[s.swz[0] as usize] != (s.modifier == 13) } else { bool_const(s.num) }
                    };
                    if go {
                        if let Some(&(_, at)) = self.labels.iter().find(|l| l.0 == ins.src[0].num) {
                            calls.push(pc);
                            pc = at + 1;
                        }
                    }
                }
                RET => match calls.pop() {
                    Some(back) => pc = back,
                    None => break,
                },
                _ => {
                    let Some(d) = ins.dst else { continue };
                    let ok = pred_ok(&ins.pred, &p);
                    let result = match ins.op {
                        M4X4 | M4X3 | M3X4 | M3X3 | M3X2 => {
                            let rows = match ins.op {
                                M4X4 | M3X4 => 4,
                                M4X3 | M3X3 => 3,
                                _ => 2,
                            };
                            let four = matches!(ins.op, M4X4 | M4X3);
                            let mut out = [0.0f32; 4];
                            for (row, x) in out.iter_mut().enumerate().take(rows) {
                                let mut s1 = ins.src[1];
                                s1.num += row as u16;
                                let m = read(&s1, &r, &o, &a0, al, &p);
                                let a = srcs[0];
                                *x = a[0] * m[0] + a[1] * m[1] + a[2] * m[2] + if four { a[3] * m[3] } else { 0.0 };
                            }
                            out
                        }
                        MOVA => srcs[0].map(|x| if self.major >= 2 { floor(x + 0.5) } else { floor(x) }),
                        SETP => core::array::from_fn(|i| if compare(ins.ctrl & 7, srcs[0][i], srcs[1][i]) { 1.0 } else { 0.0 }),
                        SINCOS => {
                            let (sn, cs) = sincos(srcs[0][0]);
                            [cs, sn, 0.0, 0.0]
                        }
                        // Vertex texture fetches: no vertex textures.
                        TEXLDL => [0.0, 0.0, 0.0, 0.0],
                        op => match alu(op, &srcs, self.major, true) {
                            Some(v) => v,
                            None => continue,
                        },
                    };
                    let v = if d.saturate { result.map(|x| x.clamp(0.0, 1.0)) } else { result };
                    // vs_1_1 writes a0 with mov.
                    let to_addr = d.rtype == R_ADDR;
                    for c in 0..4 {
                        if d.mask & (1 << c) == 0 || !ok[c] {
                            continue;
                        }
                        match d.rtype {
                            R_TEMP => r[(d.num as usize) & 31][c] = v[c],
                            _ if to_addr => a0[c] = if ins.op == MOVA { v[c] as i32 } else { floor(v[c]) as i32 },
                            R_PREDICATE => p[c] = v[c] != 0.0,
                            t => {
                                let off = match d.rel {
                                    Some((R_LOOP, _, _)) => al,
                                    Some((_, _, cc)) => a0[cc as usize & 3],
                                    None => 0,
                                };
                                if let Some(i) = slot(t, (d.num as i32 + off).max(0) as u16) {
                                    o[i][c] = v[c];
                                    written[i] = true;
                                }
                            }
                        }
                    }
                }
            }
        }
        let mut out = Vec::new();
        for i in 0..16 {
            if !written[i] {
                continue;
            }
            let usage = if self.major >= 3 {
                self.output_usage(R_OUTPUT, i as u16)
            } else {
                match i {
                    0..=2 => self.output_usage(R_RASTOUT, i as u16),
                    3..=4 => self.output_usage(R_ATTROUT, i as u16 - 3),
                    _ => self.output_usage(R_OUTPUT, i as u16 - 5),
                }
            };
            if let Some((u, ix)) = usage {
                out.push((u, ix, o[i]));
            }
        }
        out
    }

    /// From an IF or IFC at `at`: where execution goes when it's false.
    fn skip_if(&self, at: usize) -> usize {
        let j = self.ins[at].jump;
        // An ELSE: run what follows it; an ENDIF: carry on after it.
        j + 1
    }
}

// -----| the objects |-----

static VS_IIDS: [Guid; 1] = [Guid(0xefc5557e, 0x6265, 0x4613, [0x8a, 0x94, 0x43, 0x85, 0x78, 0x89, 0xeb, 0x36])];
static PS_IIDS: [Guid; 1] = [Guid(0x6d3bdbdc, 0x5b02, 0x4415, [0xb8, 0x52, 0xce, 0x5e, 0x8b, 0xcc, 0xb2, 0x89])];

/// The bytecode's length in tokens, up to and with END.
fn length(p: *const u32) -> usize {
    let mut n = 1;
    unsafe {
        loop {
            let t = *p.add(n);
            n += 1;
            if t & 0xFFFF == 0xFFFF {
                return n;
            }
            if t & 0xFFFF == COMMENT as u32 {
                n += ((t >> 16) & 0x7FFF) as usize;
            }
            // A constant's values could look like the end token.
            match (t & 0xFFFF) as u16 {
                DEF | DEFI => n += 5,
                DEFB => n += 2,
                _ => {}
            }
            if n > 1 << 20 {
                return n;
            }
        }
    }
}

pub unsafe extern "system" fn Dev_CreateVertexShader(this: *mut Com, code: *const u32, out: *mut *mut Com) -> i32 {
    if code.is_null() || out.is_null() {
        return D3DERR_INVALIDCALL;
    }
    let tokens = unsafe { core::slice::from_raw_parts(code, length(code)) };
    match parse(tokens) {
        Some(s) if !s.pixel => {
            unsafe { *out = Com::new(&vtbl::IDIRECT3DVERTEXSHADER9, &VS_IIDS, this, Body::Shader(Box::new(s))) };
            S_OK
        }
        _ => D3DERR_INVALIDCALL,
    }
}

/// Made into the kernel's program: ps_2_0 and later as they are, ps_1_x
/// expanded (`ps1_to_kernel`).
pub unsafe extern "system" fn Dev_CreatePixelShader(this: *mut Com, code: *const u32, out: *mut *mut Com) -> i32 {
    if code.is_null() || out.is_null() {
        return D3DERR_INVALIDCALL;
    }
    let tokens = unsafe { core::slice::from_raw_parts(code, length(code)) };
    let Some(mut s) = parse(tokens).filter(|s| s.pixel) else { return D3DERR_INVALIDCALL };
    let Some((ins, varyings)) = (if s.major == 1 { ps1_to_kernel(&s) } else { to_kernel(&s) }) else { return D3DERR_INVALIDCALL };
    let args = PsArgs { code: ins.as_ptr() as u64, count: ins.len() as u32, inputs: varyings.len() as u32 };
    let mut h = 0u64;
    if gfx3d::gpu3d(7, 0, &args as *const _ as u64, &mut h as *mut u64 as u64) != 0 {
        return D3DERR_INVALIDCALL;
    }
    (s.kernel, s.varyings) = (h, varyings);
    unsafe { *out = Com::new(&vtbl::IDIRECT3DPIXELSHADER9, &PS_IIDS, this, Body::Shader(Box::new(s))) };
    S_OK
}

/// A pixel shader as the kernel's instructions (gfx3d's PsIns), and the
/// usage of each input it numbers. None if it uses what we can't run.
pub fn to_kernel(s: &Shader) -> Option<(Vec<PsIns>, Vec<(u8, u8)>)> {
    // Inputs: ps_2_0's colours (v0-1) and texture coordinates (t0-7), or
    // ps_3_0's declared inputs.
    let mut varyings: Vec<(u8, u8)> = Vec::new();
    let mut input_of: Vec<((u8, u16), u32)> = Vec::new();
    for d in &s.decls {
        let usage = match d.rtype {
            R_INPUT if s.major >= 3 => (d.usage, d.index),
            R_INPUT => (U_COLOR, d.num as u8),
            R_ADDR => (U_TEXCOORD, d.num as u8),
            _ => continue,
        };
        if varyings.len() == PS_INPUTS {
            return None;
        }
        input_of.push(((d.rtype, d.num), varyings.len() as u32));
        varyings.push(usage);
    }
    let file = |rtype: u8, num: u16| -> Option<(u32, u32)> {
        Some(match rtype {
            R_TEMP => (pf::TEMP, num as u32),
            R_INPUT | R_ADDR => (pf::INPUT, input_of.iter().find(|e| e.0 == (rtype, num))?.1),
            R_CONST => (pf::CONST, num as u32),
            11 => (pf::CONST, num as u32 + 2048),
            R_CONSTINT => (pf::CONSTINT, num as u32),
            R_CONSTBOOL => (pf::CONSTBOOL, num as u32),
            R_LOOP => (pf::LOOP, 0),
            R_PREDICATE => (pf::PRED, 0),
            R_MISC => (pf::MISC, num as u32),
            _ => return None,
        })
    };
    let src = |x: &Src| -> Option<u32> {
        let (f, i) = file(x.rtype, x.num)?;
        let rel = match x.rel {
            None => 0,
            Some((R_LOOP, _, _)) => 1 << 28,
            Some(_) => return None,
        };
        let sw = x.swz[0] as u32 | (x.swz[1] as u32) << 2 | (x.swz[2] as u32) << 4 | (x.swz[3] as u32) << 6;
        Some(f | i << 4 | sw << 16 | (x.modifier as u32) << 24 | rel)
    };
    let dst = |d: &Dst, mask: u8| -> Option<u32> {
        let (f, i) = match d.rtype {
            R_COLOROUT if d.num == 0 => (pf::COLOR, 0),
            // Further render targets aren't kept: written to a scratch temp.
            R_COLOROUT => (pf::TEMP, 31),
            R_DEPTHOUT => (pf::DEPTH, 0),
            t => file(t, d.num)?,
        };
        let rel = if matches!(d.rel, Some((R_LOOP, _, _))) { 1 << 28 } else { 0 };
        Some(f | i << 4 | (mask as u32) << 16 | (d.saturate as u32) << 20 | ((d.shift as u32) & 15) << 24 | rel)
    };
    let pred = |p: &Option<Src>| -> u32 {
        match p {
            Some(x) => 1 << 31 | (x.swz[0] as u32 | (x.swz[1] as u32) << 2 | (x.swz[2] as u32) << 4 | (x.swz[3] as u32) << 6) << 16 | ((x.modifier == 13) as u32) << 24,
            None => 0,
        }
    };
    let mut out: Vec<PsIns> = Vec::new();
    // Where each of the shader's instructions starts in `out`.
    let mut at: Vec<usize> = Vec::with_capacity(s.ins.len() + 1);
    for x in &s.ins {
        at.push(out.len());
        let mut k = PsIns { op: x.op, ctrl: x.ctrl, sampler: 0, dst: 0, src: [0; 3], pred: pred(&x.pred), jump: 0, rel: 0 };
        match x.op {
            M4X4 | M4X3 | M3X4 | M3X3 | M3X2 => {
                // One DP4 or DP3 a row, each writing its component.
                let d = x.dst?;
                let rows = match x.op {
                    M4X4 | M3X4 => 4,
                    M4X3 | M3X3 => 3,
                    _ => 2,
                };
                for row in 0..rows {
                    let mut m = *x.src.get(1)?;
                    m.num += row as u16;
                    out.push(PsIns {
                        op: if matches!(x.op, M4X4 | M4X3) { DP4 } else { DP3 },
                        dst: dst(&d, d.mask & (1 << row))?,
                        src: [src(x.src.first()?)?, src(&m)?, 0],
                        ..k
                    });
                }
                continue;
            }
            // texkill names its register as a destination.
            TEXKILL => {
                let d = x.dst?;
                let r = Src { rtype: d.rtype, num: d.num, swz: [0, 1, 2, 3], modifier: 0, rel: None };
                k.src[0] = src(&r)?;
            }
            // texld coordinates, sampler; texldl too.
            66 | TEXLDL => {
                k.dst = dst(&x.dst?, x.dst?.mask)?;
                k.src[0] = src(x.src.first()?)?;
                k.sampler = x.src.get(1)?.num as u8;
            }
            _ => {
                if let Some(d) = x.dst {
                    k.dst = dst(&d, d.mask)?;
                }
                // SINCOS's extra sources (ps_2_0's constants) aren't needed.
                for (i, sx) in x.src.iter().enumerate().take(3) {
                    k.src[i] = src(sx)?;
                }
                // IF's boolean, REP and LOOP's integer register numbers.
                if matches!(x.op, IF | REP) {
                    k.src[0] = src(x.src.first()?)?;
                }
                if x.op == LOOP {
                    k.src[0] = src(x.src.get(1)?)?;
                }
                if matches!(x.op, CALL | CALLNZ) {
                    if x.op == CALLNZ {
                        k.src[1] = src(x.src.get(1)?)?;
                    }
                    k.src[0] = 0;
                }
                if x.op == LABEL {
                    k.src[0] = 0;
                }
            }
        }
        out.push(k);
    }
    at.push(out.len());
    // Jumps: to the new places of the shader's instructions, and labels.
    for (i, x) in s.ins.iter().enumerate() {
        let j = match x.op {
            IF | IFC | ELSE | LOOP | REP | ENDLOOP | ENDREP | BREAK | BREAKC | BREAKP => at[x.jump],
            CALL | CALLNZ => {
                let l = x.src.first()?.num;
                at[s.labels.iter().find(|e| e.0 == l)?.1]
            }
            _ => continue,
        };
        out[at[i]].jump = j as u32;
    }
    Some((out, varyings))
}

unsafe fn get_function(this: *mut Com, data: *mut u8, size: *mut u32) -> i32 {
    let Body::Shader(s) = (unsafe { &(*this).body }) else { return D3DERR_INVALIDCALL };
    if size.is_null() {
        return D3DERR_INVALIDCALL;
    }
    let n = s.tokens.len() * 4;
    if data.is_null() {
        unsafe { *size = n as u32 };
        return S_OK;
    }
    if (unsafe { *size } as usize) < n {
        return D3DERR_INVALIDCALL;
    }
    unsafe { core::ptr::copy_nonoverlapping(s.tokens.as_ptr() as *const u8, data, n) };
    S_OK
}

pub unsafe extern "system" fn VS_GetFunction(this: *mut Com, data: *mut u8, size: *mut u32) -> i32 {
    unsafe { get_function(this, data, size) }
}

pub unsafe extern "system" fn PS_GetFunction(this: *mut Com, data: *mut u8, size: *mut u32) -> i32 {
    unsafe { get_function(this, data, size) }
}

/// The shader of a shader object.
pub fn of<'a>(p: *mut Com) -> Option<&'a Shader> {
    match unsafe { p.as_ref() } {
        Some(Com { body: Body::Shader(s), .. }) => Some(s),
        _ => None,
    }
}

// -----| ps_1_x |-----
//
// ps_1_1 to ps_1_3: t0-t3 are what the texture instructions leave (kept in
// temporaries 8-11), the coordinates they read are texture coordinate
// inputs; ps_1_4: texld and texcrd read texture coordinates (tN) or
// registers (rN) into r0-r5. Each texture instruction becomes the kernel's
// arithmetic and a TEX; r0 is the colour at the end. Internal constants
// (the draw puts them in the block): c200+n each stage's bump matrix
// (00, 01, 10, 11), c204+n its luminance scale and offset, c208 ones,
// c209 zeros. Temporaries 29 and 30 are scratch.

pub const C_BUMP: u32 = 200;
pub const C_LUM: u32 = 204;
pub const C_ONE: u32 = 208;
pub const C_ZERO: u32 = 209;

const SW_ID: u32 = 0xE4;

fn ps1_to_kernel(s: &Shader) -> Option<(Vec<PsIns>, Vec<(u8, u8)>)> {
    let v14 = s.minor >= 4;
    let mut varyings: Vec<(u8, u8)> = Vec::new();
    let mut input = |usage: u8, index: u8| -> Option<u32> {
        if let Some(i) = varyings.iter().position(|v| *v == (usage, index)) {
            return Some(i as u32);
        }
        if varyings.len() == PS_INPUTS {
            return None;
        }
        varyings.push((usage, index));
        Some(varyings.len() as u32 - 1)
    };
    let op = |file: u32, idx: u32, sw: u32, m: u32| file | idx << 4 | sw << 16 | m << 24;
    let dmask = |file: u32, idx: u32, mask: u32| file | idx << 4 | mask << 16;
    let tc = |input: &mut dyn FnMut(u8, u8) -> Option<u32>, n: u16, sw: u32| -> Option<u32> { Some(op(pf::INPUT, input(U_TEXCOORD, n as u8)?, sw, 0)) };
    let mut out: Vec<PsIns> = Vec::new();
    let ins = |o: u16, dst: u32, src: [u32; 3], sampler: u8| PsIns { op: o, ctrl: 0, sampler, dst, src, pred: 0, jump: 0, rel: 0 };
    let mut pads = 0u32;
    for x in &s.ins {
        // An operand of an arithmetic instruction.
        let src = |input: &mut dyn FnMut(u8, u8) -> Option<u32>, a: &Src| -> Option<u32> {
            let (f, i) = match a.rtype {
                R_TEMP => (pf::TEMP, a.num as u32),
                R_INPUT => (pf::INPUT, input(U_COLOR, a.num as u8)?),
                R_CONST => (pf::CONST, a.num as u32),
                R_ADDR if v14 => (pf::INPUT, input(U_TEXCOORD, a.num as u8)?),
                R_ADDR => (pf::TEMP, 8 + a.num as u32),
                _ => return None,
            };
            let sw = a.swz[0] as u32 | (a.swz[1] as u32) << 2 | (a.swz[2] as u32) << 4 | (a.swz[3] as u32) << 6;
            Some(op(f, i, sw, a.modifier as u32))
        };
        let dst = |d: &Dst| -> Option<u32> {
            let (f, i) = match d.rtype {
                R_TEMP => (pf::TEMP, d.num as u32),
                R_ADDR => (pf::TEMP, 8 + d.num as u32),
                _ => return None,
            };
            Some(f | i << 4 | (d.mask as u32) << 16 | (d.saturate as u32) << 20 | ((d.shift as u32) & 15) << 24)
        };
        let d = x.dst;
        let n = d.map_or(0, |d| d.num);
        let t30 = |mask: u32| dmask(pf::TEMP, 30, mask);
        let r30 = op(pf::TEMP, 30, SW_ID, 0);
        match x.op {
            PHASE | NOP => {}
            // tex tN (1.1-1.3) / texld rN, src (1.4).
            66 => {
                let d = d?;
                if v14 {
                    out.push(ins(66, dst(&d)?, [src(&mut input, x.src.first()?)?, 0, 0], d.num as u8));
                } else {
                    out.push(ins(66, dmask(pf::TEMP, 8 + n as u32, 15), [tc(&mut input, n, SW_ID)?, 0, 0], n as u8));
                }
            }
            // texcoord tN (1.1-1.3: saturated, w 1) / texcrd rN, tM (1.4).
            64 => {
                let d = d?;
                if v14 {
                    out.push(ins(MOV, dst(&d)?, [src(&mut input, x.src.first()?)?, 0, 0], 0));
                } else {
                    out.push(ins(MOV, dmask(pf::TEMP, 8 + n as u32, 7) | 1 << 20, [tc(&mut input, n, SW_ID)?, 0, 0], 0));
                    out.push(ins(MOV, dmask(pf::TEMP, 8 + n as u32, 8), [op(pf::CONST, C_ONE, SW_ID, 0), 0, 0], 0));
                }
            }
            // texkill: the coordinates' x, y, z (1.1-1.3), or a register.
            TEXKILL => {
                let d = d?;
                let r = if !v14 || d.rtype == R_ADDR {
                    tc(&mut input, n, 0xA4)?
                } else {
                    op(pf::TEMP, n as u32, 0xA4, 0)
                };
                out.push(ins(TEXKILL, 0, [r, 0, 0], 0));
            }
            // texbem(l) tN, tM: coordinates moved by the stage's bump matrix.
            67 | 68 => {
                let m = src(&mut input, x.src.first()?)?;
                let with = |sw: u32| (m & !(0xFF << 16)) | sw << 16;
                out.push(ins(MAD, t30(3), [with(0x00), op(pf::CONST, C_BUMP + n as u32, 0x44, 0), tc(&mut input, n, SW_ID)?], 0));
                out.push(ins(MAD, t30(3), [with(0x55), op(pf::CONST, C_BUMP + n as u32, 0xEE, 0), r30], 0));
                out.push(ins(66, dmask(pf::TEMP, 8 + n as u32, 15), [r30, 0, 0], n as u8));
                if x.op == 68 {
                    out.push(ins(MAD, dmask(pf::TEMP, 29, 15), [with(0xAA), op(pf::CONST, C_LUM + n as u32, 0x00, 0), op(pf::CONST, C_LUM + n as u32, 0x55, 0)], 0));
                    out.push(ins(MUL, dmask(pf::TEMP, 8 + n as u32, 7), [op(pf::TEMP, 8 + n as u32, SW_ID, 0), op(pf::TEMP, 29, SW_ID, 0), 0], 0));
                }
            }
            // texreg2ar / gb / rgb tN, tM: tM's components as coordinates.
            69 | 70 | 82 => {
                let m = src(&mut input, x.src.first()?)?;
                let sw = match x.op {
                    69 => 0x03,
                    70 => 0xA9,
                    _ => 0xA4,
                };
                out.push(ins(66, dmask(pf::TEMP, 8 + n as u32, 15), [(m & !(0xFF << 16)) | sw << 16, 0, 0], n as u8));
            }
            // texm3x2pad, texm3x3pad: a row's dot product into scratch.
            71 | 73 => {
                let m = src(&mut input, x.src.first()?)?;
                out.push(ins(DP3, t30(1 << pads.min(2)), [tc(&mut input, n, SW_ID)?, m, 0], 0));
                pads += 1;
            }
            // The last row, then the lookup (texm3x3spec and vspec sample
            // as texm3x3tex does: cube maps aren't sampled yet).
            72 | 74 | 76 | 77 | 86 | 83 | 85 | 84 => {
                let m = src(&mut input, x.src.first()?)?;
                let row = match x.op {
                    83 | 85 => 0,
                    _ => pads.min(2),
                };
                out.push(ins(DP3, t30(1 << row), [tc(&mut input, n, SW_ID)?, m, 0], 0));
                if x.op == 83 {
                    out.push(ins(MOV, t30(2), [op(pf::CONST, C_ZERO, SW_ID, 0), 0, 0], 0));
                }
                pads = 0;
                match x.op {
                    // texdp3: the dot product, replicated.
                    85 => out.push(ins(MOV, dmask(pf::TEMP, 8 + n as u32, 15), [op(pf::TEMP, 30, 0x00, 0), 0, 0], 0)),
                    // texm3x3: the vector.
                    86 => {
                        out.push(ins(MOV, dmask(pf::TEMP, 8 + n as u32, 7), [r30, 0, 0], 0));
                        out.push(ins(MOV, dmask(pf::TEMP, 8 + n as u32, 8), [op(pf::CONST, C_ONE, SW_ID, 0), 0, 0], 0));
                    }
                    // texm3x2depth: depth = x / y.
                    84 => {
                        out.push(ins(RCP, dmask(pf::TEMP, 29, 1), [op(pf::TEMP, 30, 0x55, 0), 0, 0], 0));
                        out.push(ins(MUL, dmask(pf::DEPTH, 0, 1), [r30, op(pf::TEMP, 29, 0x00, 0), 0], 0));
                    }
                    _ => out.push(ins(66, dmask(pf::TEMP, 8 + n as u32, 15), [r30, 0, 0], n as u8)),
                }
            }
            // texdepth rN (1.4): depth = r.x / r.y.
            87 => {
                out.push(ins(RCP, dmask(pf::TEMP, 29, 1), [op(pf::TEMP, n as u32, 0x55, 0), 0, 0], 0));
                out.push(ins(MUL, dmask(pf::DEPTH, 0, 1), [op(pf::TEMP, n as u32, SW_ID, 0), op(pf::TEMP, 29, 0x00, 0), 0], 0));
            }
            // bem rD.xy, s0, s1 (1.4): s0 + the stage's bump matrix times s1.
            89 => {
                let (a, b) = (src(&mut input, x.src.first()?)?, src(&mut input, x.src.get(1)?)?);
                let with = |sw: u32| (b & !(0xFF << 16)) | sw << 16;
                out.push(ins(MAD, t30(3), [with(0x00), op(pf::CONST, C_BUMP + n as u32, 0x44, 0), a], 0));
                out.push(ins(MAD, t30(3), [with(0x55), op(pf::CONST, C_BUMP + n as u32, 0xEE, 0), r30], 0));
                out.push(ins(MOV, dst(&d?)?, [r30, 0, 0], 0));
            }
            // The arithmetic, as it is.
            o => {
                let mut k = ins(o, dst(&d?)?, [0; 3], 0);
                for (i, a) in x.src.iter().enumerate().take(3) {
                    k.src[i] = src(&mut input, a)?;
                }
                out.push(k);
            }
        }
    }
    out.push(ins(MOV, dmask(pf::COLOR, 0, 15), [op(pf::TEMP, 0, SW_ID, 0), 0, 0], 0));
    Some((out, varyings))
}
