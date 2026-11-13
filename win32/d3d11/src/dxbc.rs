//! DXBC: the container fxc writes and Direct3D 10/11 take, and the SM4/SM5
//! token stream in it, parsed into instructions with their operands and
//! the declarations a stage needs (signatures, registers, buffers,
//! resources, samplers). Its tables name the whole format, used or not.
#![allow(dead_code)]

use alloc::vec::Vec;

// Operand types.
pub const T_TEMP: u8 = 0;
pub const T_INPUT: u8 = 1;
pub const T_OUTPUT: u8 = 2;
pub const T_INDEXABLE: u8 = 3;
pub const T_IMM32: u8 = 4;
pub const T_IMM64: u8 = 5;
pub const T_SAMPLER: u8 = 6;
pub const T_RESOURCE: u8 = 7;
pub const T_CB: u8 = 8;
pub const T_ICB: u8 = 9;
pub const T_LABEL: u8 = 10;
pub const T_PRIMITIVEID: u8 = 11;
pub const T_DEPTH: u8 = 12;
pub const T_NULL: u8 = 13;
pub const T_COVERAGE_MASK: u8 = 15;
pub const T_INPUT_COVERAGE: u8 = 0x23;
pub const T_THREAD_ID: u8 = 0x20;
pub const T_GROUP_ID: u8 = 0x21;
pub const T_THREAD_IN_GROUP: u8 = 0x22;
pub const T_THREAD_IN_GROUP_FLAT: u8 = 0x24;

// Opcodes.
pub mod op {
    pub const ADD: u16 = 0x00;
    pub const AND: u16 = 0x01;
    pub const BREAK: u16 = 0x02;
    pub const BREAKC: u16 = 0x03;
    pub const CALL: u16 = 0x04;
    pub const CALLC: u16 = 0x05;
    pub const CASE: u16 = 0x06;
    pub const CONTINUE: u16 = 0x07;
    pub const CONTINUEC: u16 = 0x08;
    pub const CUT: u16 = 0x09;
    pub const DEFAULT: u16 = 0x0a;
    pub const DERIV_RTX: u16 = 0x0b;
    pub const DERIV_RTY: u16 = 0x0c;
    pub const DISCARD: u16 = 0x0d;
    pub const DIV: u16 = 0x0e;
    pub const DP2: u16 = 0x0f;
    pub const DP3: u16 = 0x10;
    pub const DP4: u16 = 0x11;
    pub const ELSE: u16 = 0x12;
    pub const EMIT: u16 = 0x13;
    pub const EMITTHENCUT: u16 = 0x14;
    pub const ENDIF: u16 = 0x15;
    pub const ENDLOOP: u16 = 0x16;
    pub const ENDSWITCH: u16 = 0x17;
    pub const EQ: u16 = 0x18;
    pub const EXP: u16 = 0x19;
    pub const FRC: u16 = 0x1a;
    pub const FTOI: u16 = 0x1b;
    pub const FTOU: u16 = 0x1c;
    pub const GE: u16 = 0x1d;
    pub const IADD: u16 = 0x1e;
    pub const IF: u16 = 0x1f;
    pub const IEQ: u16 = 0x20;
    pub const IGE: u16 = 0x21;
    pub const ILT: u16 = 0x22;
    pub const IMAD: u16 = 0x23;
    pub const IMAX: u16 = 0x24;
    pub const IMIN: u16 = 0x25;
    pub const IMUL: u16 = 0x26;
    pub const INE: u16 = 0x27;
    pub const INEG: u16 = 0x28;
    pub const ISHL: u16 = 0x29;
    pub const ISHR: u16 = 0x2a;
    pub const ITOF: u16 = 0x2b;
    pub const LABEL: u16 = 0x2c;
    pub const LD: u16 = 0x2d;
    pub const LD_MS: u16 = 0x2e;
    pub const LOG: u16 = 0x2f;
    pub const LOOP: u16 = 0x30;
    pub const LT: u16 = 0x31;
    pub const MAD: u16 = 0x32;
    pub const MIN: u16 = 0x33;
    pub const MAX: u16 = 0x34;
    pub const CUSTOMDATA: u16 = 0x35;
    pub const MOV: u16 = 0x36;
    pub const MOVC: u16 = 0x37;
    pub const MUL: u16 = 0x38;
    pub const NE: u16 = 0x39;
    pub const NOP: u16 = 0x3a;
    pub const NOT: u16 = 0x3b;
    pub const OR: u16 = 0x3c;
    pub const RESINFO: u16 = 0x3d;
    pub const RET: u16 = 0x3e;
    pub const RETC: u16 = 0x3f;
    pub const ROUND_NE: u16 = 0x40;
    pub const ROUND_NI: u16 = 0x41;
    pub const ROUND_PI: u16 = 0x42;
    pub const ROUND_Z: u16 = 0x43;
    pub const RSQ: u16 = 0x44;
    pub const SAMPLE: u16 = 0x45;
    pub const SAMPLE_C: u16 = 0x46;
    pub const SAMPLE_C_LZ: u16 = 0x47;
    pub const SAMPLE_L: u16 = 0x48;
    pub const SAMPLE_D: u16 = 0x49;
    pub const SAMPLE_B: u16 = 0x4a;
    pub const SQRT: u16 = 0x4b;
    pub const SWITCH: u16 = 0x4c;
    pub const SINCOS: u16 = 0x4d;
    pub const UDIV: u16 = 0x4e;
    pub const ULT: u16 = 0x4f;
    pub const UGE: u16 = 0x50;
    pub const UMUL: u16 = 0x51;
    pub const UMAD: u16 = 0x52;
    pub const UMAX: u16 = 0x53;
    pub const UMIN: u16 = 0x54;
    pub const USHR: u16 = 0x55;
    pub const UTOF: u16 = 0x56;
    pub const XOR: u16 = 0x57;
    pub const DCL_RESOURCE: u16 = 0x58;
    pub const DCL_CONSTANT_BUFFER: u16 = 0x59;
    pub const DCL_SAMPLER: u16 = 0x5a;
    pub const DCL_INPUT: u16 = 0x5f;
    pub const DCL_INPUT_SGV: u16 = 0x60;
    pub const DCL_INPUT_SIV: u16 = 0x61;
    pub const DCL_INPUT_PS: u16 = 0x62;
    pub const DCL_INPUT_PS_SGV: u16 = 0x63;
    pub const DCL_INPUT_PS_SIV: u16 = 0x64;
    pub const DCL_OUTPUT: u16 = 0x65;
    pub const DCL_OUTPUT_SGV: u16 = 0x66;
    pub const DCL_OUTPUT_SIV: u16 = 0x67;
    pub const DCL_TEMPS: u16 = 0x68;
    pub const DCL_INDEXABLE_TEMP: u16 = 0x69;
    pub const DCL_GLOBAL_FLAGS: u16 = 0x6a;
    pub const LOD: u16 = 0x6c;
    pub const GATHER4: u16 = 0x6d;
    pub const SAMPLE_POS: u16 = 0x6e;
    pub const SAMPLE_INFO: u16 = 0x6f;
    pub const DERIV_RTX_COARSE: u16 = 0x7a;
    pub const DERIV_RTX_FINE: u16 = 0x7b;
    pub const DERIV_RTY_COARSE: u16 = 0x7c;
    pub const DERIV_RTY_FINE: u16 = 0x7d;
    pub const RCP: u16 = 0x81;
    pub const F32TOF16: u16 = 0x82;
    pub const F16TOF32: u16 = 0x83;
    pub const COUNTBITS: u16 = 0x86;
    pub const FIRSTBIT_HI: u16 = 0x87;
    pub const FIRSTBIT_LO: u16 = 0x88;
    pub const FIRSTBIT_SHI: u16 = 0x89;
    pub const UBFE: u16 = 0x8a;
    pub const IBFE: u16 = 0x8b;
    pub const BFI: u16 = 0x8c;
    pub const BFREV: u16 = 0x8d;
    pub const SWAPC: u16 = 0x8e;
}

// System values (D3D_NAME).
pub const SV_POSITION: u32 = 1;
pub const SV_CLIP_DISTANCE: u32 = 2;
pub const SV_CULL_DISTANCE: u32 = 3;
pub const SV_VERTEX_ID: u32 = 6;
pub const SV_PRIMITIVE_ID: u32 = 7;
pub const SV_INSTANCE_ID: u32 = 8;
pub const SV_IS_FRONT_FACE: u32 = 9;
pub const SV_SAMPLE_INDEX: u32 = 10;
pub const SV_TARGET: u32 = 64;
pub const SV_DEPTH: u32 = 65;

/// A register an index is relative to: r#.c or x#[n].c.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rel {
    pub ty: u8,
    pub reg: u32,
    /// x#'s own (immediate) index.
    pub sub: u32,
    pub comp: u8,
}

#[derive(Clone, Copy, Debug)]
pub struct Opnd {
    pub ty: u8,
    /// Components: 0, 1 or 4.
    pub comps: u8,
    /// A destination's write mask (bit per component).
    pub mask: u8,
    /// A source's swizzle: component k reads (swz >> 2k) & 3.
    pub swz: u8,
    /// 0 none, 1 negated, 2 absolute, 3 both (-|x|).
    pub modi: u8,
    pub nidx: u8,
    pub idx: [u32; 3],
    pub rel: [Option<Rel>; 3],
    pub imm: [u32; 4],
}

impl Opnd {
    pub const NULL: Opnd = Opnd { ty: T_NULL, comps: 0, mask: 0, swz: 0xE4, modi: 0, nidx: 0, idx: [0; 3], rel: [None; 3], imm: [0; 4] };

    /// The register number (the first index; x#'s array).
    pub fn reg(&self) -> u32 {
        self.idx[0]
    }
}

#[derive(Clone, Debug)]
pub struct Ins {
    pub op: u16,
    /// The opcode token's bits 11-23.
    pub ctrl: u16,
    pub sat: bool,
    /// Texel offsets (aoffimmi), u v w.
    pub offset: [i8; 3],
    /// How many of `ops` are destinations.
    pub ndst: u8,
    pub ops: Vec<Opnd>,
}

impl Ins {
    /// The conditional ones' test: true when they act on non-zero.
    pub fn nonzero(&self) -> bool {
        self.ctrl & (1 << 7) != 0
    }
    pub fn dst(&self, i: usize) -> &Opnd {
        &self.ops[i]
    }
    pub fn src(&self, i: usize) -> &Opnd {
        &self.ops[self.ndst as usize + i]
    }
}

#[derive(Clone, Debug, Default)]
pub struct SigElem {
    pub name: Vec<u8>,
    pub index: u32,
    pub sysval: u32,
    /// 1 uint, 2 sint, 3 float.
    pub ctype: u32,
    pub reg: u32,
    pub mask: u8,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct InDecl {
    pub reg: u32,
    pub mask: u8,
    pub sysval: u32,
    /// Pixel shaders: 1 constant, 2 linear, 3 centroid, 4 no perspective,
    /// 5-7 the others.
    pub interp: u8,
    /// v# is SV_Position and the like given by name, not interpolated.
    pub special: u8,
}

pub struct Shader {
    /// 0 pixel, 1 vertex, 2 geometry, 3 hull, 4 domain, 5 compute.
    pub kind: u32,
    pub major: u32,
    pub minor: u32,
    pub ins: Vec<Ins>,
    pub temps: u32,
    /// x#: size and components.
    pub xtemps: Vec<(u32, u32)>,
    /// Each constant buffer's size in float4 (0: not declared).
    pub cb_size: [u32; 14],
    pub icb: Vec<u32>,
    pub isgn: Vec<SigElem>,
    pub osgn: Vec<SigElem>,
    pub inputs: Vec<InDecl>,
    pub outputs: Vec<InDecl>,
    /// t#: dimension (1 buffer, 2 1D, 3 2D, 4 2DMS, 5 3D, 6 cube, 7 1D
    /// array, 8 2D array, 10 cube array) and return type (1 unorm, 2 snorm,
    /// 3 sint, 4 uint, 5 float).
    pub res: Vec<(u32, u32, u32)>,
    /// s#: 0 default, 1 comparison.
    pub samplers: Vec<(u32, u32)>,
    /// Writes SV_Depth (oDepth).
    pub depth_out: bool,
}

struct Rd<'a> {
    t: &'a [u32],
    at: usize,
}

impl Rd<'_> {
    fn next(&mut self) -> Option<u32> {
        let v = *self.t.get(self.at)?;
        self.at += 1;
        Some(v)
    }
}

fn swizzle_of(tok: u32, comps: u32) -> (u8, u8) {
    // (mask, swizzle)
    if comps == 1 {
        return (1, 0);
    }
    if comps != 2 {
        return (0, 0xE4);
    }
    match (tok >> 2) & 3 {
        0 => (((tok >> 4) & 15) as u8, 0xE4),
        1 => (15, ((tok >> 4) & 0xFF) as u8),
        _ => {
            let c = ((tok >> 4) & 3) as u8;
            (15, c | c << 2 | c << 4 | c << 6)
        }
    }
}

fn operand(r: &mut Rd) -> Option<Opnd> {
    let t = r.next()?;
    let comps = t & 3;
    let (mask, swz) = swizzle_of(t, comps);
    let ty = ((t >> 12) & 0xFF) as u8;
    let nidx = (t >> 20) & 3;
    let mut o = Opnd { ty, comps: [0, 1, 4, 4][comps as usize], mask, swz, modi: 0, nidx: nidx as u8, idx: [0; 3], rel: [None; 3], imm: [0; 4] };
    let mut ext = t >> 31 != 0;
    while ext {
        let e = r.next()?;
        ext = e >> 31 != 0;
        if e & 0x3F == 1 {
            o.modi = ((e >> 6) & 0xFF) as u8;
        }
    }
    if ty == T_IMM32 {
        let n = if comps == 1 { 1 } else { 4 };
        for k in 0..n {
            o.imm[k] = r.next()?;
        }
        if n == 1 {
            o.imm = [o.imm[0]; 4];
        }
        o.swz = 0xE4;
    } else if ty == T_IMM64 {
        let n = if comps == 1 { 1 } else { 2 };
        for k in 0..n {
            o.imm[k] = r.next()?;
            r.next()?;
        }
    }
    for k in 0..nidx as usize {
        let rep = (t >> (22 + 3 * k)) & 7;
        match rep {
            0 => o.idx[k] = r.next()?,
            1 => {
                o.idx[k] = r.next()?;
                r.next()?;
            }
            2 | 3 => {
                if rep == 3 {
                    o.idx[k] = r.next()?;
                }
                let ro = operand(r)?;
                let comp = ro.swz & 3;
                o.rel[k] = Some(Rel { ty: ro.ty, reg: ro.idx[0], sub: ro.idx[1], comp });
                if !matches!(ro.ty, T_TEMP | T_INDEXABLE) {
                    return None;
                }
            }
            _ => return None,
        }
    }
    Some(o)
}

/// How many destinations an instruction has.
fn dsts(o: u16) -> u8 {
    use op::*;
    match o {
        SINCOS | UDIV | IMUL | UMUL | SWAPC => 2,
        BREAK | BREAKC | CALL | CALLC | CASE | CONTINUE | CONTINUEC | CUT | DEFAULT | DISCARD | ELSE | EMIT | EMITTHENCUT | ENDIF | ENDLOOP | ENDSWITCH | IF
        | LABEL | LOOP | NOP | RET | RETC | SWITCH => 0,
        _ => 1,
    }
}

fn chunk<'a>(b: &'a [u8], tag: &[u8; 4]) -> Option<&'a [u8]> {
    if b.len() < 32 || &b[..4] != b"DXBC" {
        return None;
    }
    let n = u32::from_le_bytes(b[28..32].try_into().ok()?) as usize;
    for i in 0..n {
        let at = 32 + 4 * i;
        let off = u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?) as usize;
        let h = b.get(off..off + 8)?;
        if &h[..4] == tag {
            let len = u32::from_le_bytes(h[4..8].try_into().ok()?) as usize;
            return b.get(off + 8..off + 8 + len);
        }
    }
    None
}

fn signature(b: &[u8], tag: &[u8; 4], wide: bool) -> Option<Vec<SigElem>> {
    let c = chunk(b, tag)?;
    let rd = |at: usize| -> Option<u32> { Some(u32::from_le_bytes(c.get(at..at + 4)?.try_into().ok()?)) };
    let n = rd(0)? as usize;
    let size = if wide { 32 } else { 24 };
    let mut v = Vec::new();
    for i in 0..n {
        let e = 8 + size * i + if wide { 4 } else { 0 };
        let name_at = rd(e)? as usize;
        let name: Vec<u8> = c.get(name_at..)?.iter().take_while(|&&x| x != 0).map(|x| x.to_ascii_uppercase()).collect();
        v.push(SigElem { name, index: rd(e + 4)?, sysval: rd(e + 8)?, ctype: rd(e + 12)?, reg: rd(e + 16)?, mask: *c.get(e + 20)? });
    }
    Some(v)
}

/// The signature `tag` (ISGN or OSGN), or its SM5 forms.
fn any_signature(b: &[u8], input: bool) -> Vec<SigElem> {
    let (a, w, w5) = if input { (b"ISGN", b"ISG1", b"ISG1") } else { (b"OSGN", b"OSG1", b"OSG5") };
    signature(b, a, false).or_else(|| signature(b, w, true)).or_else(|| signature(b, w5, true)).unwrap_or_default()
}

/// Parse a shader's bytecode; None if it isn't DXBC we understand.
pub fn parse(b: &[u8]) -> Option<Shader> {
    let code = chunk(b, b"SHEX").or_else(|| chunk(b, b"SHDR"))?;
    let toks: Vec<u32> = code.chunks_exact(4).map(|c| u32::from_le_bytes(c.try_into().unwrap())).collect();
    let ver = *toks.first()?;
    let len = (*toks.get(1)? as usize).min(toks.len());
    let mut sh = Shader {
        kind: ver >> 16,
        major: (ver >> 4) & 15,
        minor: ver & 15,
        ins: Vec::new(),
        temps: 0,
        xtemps: Vec::new(),
        cb_size: [0; 14],
        icb: Vec::new(),
        isgn: any_signature(b, true),
        osgn: any_signature(b, false),
        inputs: Vec::new(),
        outputs: Vec::new(),
        res: Vec::new(),
        samplers: Vec::new(),
        depth_out: false,
    };
    let mut at = 2;
    while at < len {
        let t = toks[at];
        let o = (t & 0x7FF) as u16;
        if o == op::CUSTOMDATA {
            let n = *toks.get(at + 1)? as usize;
            if n < 2 {
                return None;
            }
            // The immediate constant buffer.
            if t >> 11 == 3 {
                sh.icb = toks.get(at + 2..at + n)?.to_vec();
            }
            at += n;
            continue;
        }
        let n = ((t >> 24) & 0x7F) as usize;
        if n == 0 || at + n > len {
            return None;
        }
        let end = at + n;
        let mut r = Rd { t: &toks[..end], at: at + 1 };
        let mut offset = [0i8; 3];
        let mut ext = t >> 31 != 0;
        while ext {
            let e = r.next()?;
            ext = e >> 31 != 0;
            if e & 0x3F == 1 {
                let s = |b: u32| ((((e >> b) & 15) as i8) << 4) >> 4;
                offset = [s(9), s(13), s(17)];
            }
        }
        let ctrl = ((t >> 11) & 0x1FFF) as u16;
        match o {
            op::DCL_TEMPS => sh.temps = r.next()?,
            op::DCL_INDEXABLE_TEMP => {
                let reg = r.next()? as usize;
                let size = r.next()?;
                let comps = r.next()?;
                if sh.xtemps.len() <= reg {
                    sh.xtemps.resize(reg + 1, (0, 0));
                }
                sh.xtemps[reg] = (size, comps);
            }
            op::DCL_CONSTANT_BUFFER => {
                let x = operand(&mut r)?;
                if let Some(s) = sh.cb_size.get_mut(x.idx[0] as usize) {
                    *s = x.idx[1];
                }
            }
            op::DCL_SAMPLER => {
                let x = operand(&mut r)?;
                sh.samplers.push((x.idx[0], (ctrl as u32) & 15));
            }
            op::DCL_RESOURCE => {
                let x = operand(&mut r)?;
                let rt = r.next()?;
                sh.res.push((x.idx[0], (ctrl as u32) & 31, rt & 15));
            }
            op::DCL_INPUT..=op::DCL_INPUT_PS_SIV => {
                let x = operand(&mut r)?;
                let sysval = if matches!(o, op::DCL_INPUT_SGV | op::DCL_INPUT_SIV | op::DCL_INPUT_PS_SGV | op::DCL_INPUT_PS_SIV) { r.next()? } else { 0 };
                let interp = if matches!(o, op::DCL_INPUT_PS | op::DCL_INPUT_PS_SGV | op::DCL_INPUT_PS_SIV) { (ctrl & 15) as u8 } else { 0 };
                let special = if x.ty == T_INPUT { 0 } else { x.ty };
                sh.inputs.push(InDecl { reg: x.idx[0], mask: x.mask, sysval, interp, special });
            }
            op::DCL_OUTPUT..=op::DCL_OUTPUT_SIV => {
                let x = operand(&mut r)?;
                let sysval = if o != op::DCL_OUTPUT { r.next()? } else { 0 };
                if x.ty == T_DEPTH {
                    sh.depth_out = true;
                }
                sh.outputs.push(InDecl { reg: x.idx[0], mask: x.mask, sysval, interp: 0, special: if x.ty == T_OUTPUT { 0 } else { x.ty } });
            }
            o if o < op::DCL_RESOURCE || matches!(o, op::LOD | op::GATHER4 | op::SAMPLE_POS | op::SAMPLE_INFO) || (0x7a..=0x8e).contains(&o) => {
                let mut ops = Vec::new();
                while r.at < end {
                    ops.push(operand(&mut r)?);
                }
                let nd = dsts(o);
                if ops.len() < nd as usize {
                    return None;
                }
                sh.ins.push(Ins { op: o, ctrl, sat: ctrl & (1 << 2) != 0, offset, ndst: nd, ops });
            }
            // Declarations and instructions we don't need, or know.
            _ => {
                if o >= 0x100 {
                    return None;
                }
            }
        }
        at = end;
    }
    Some(sh)
}

/// Every instruction's partner: for IF its ELSE or ENDIF; ELSE its ENDIF;
/// LOOP its ENDLOOP and the other way round; BREAK* the end of the loop or
/// switch it leaves; CONTINUE* its loop's ENDLOOP; SWITCH its ENDSWITCH;
/// CASE and DEFAULT their SWITCH. None if the nesting is wrong.
pub fn jumps(ins: &[Ins]) -> Option<Vec<u32>> {
    use op::*;
    let mut j = alloc::vec![0u32; ins.len()];
    let mut ifs: Vec<usize> = Vec::new();
    // Loops and switches: (start, kind), and each one's breaks to patch.
    let mut blocks: Vec<(usize, u16, Vec<usize>)> = Vec::new();
    for (i, x) in ins.iter().enumerate() {
        match x.op {
            IF => ifs.push(i),
            ELSE => {
                let s = *ifs.last()?;
                j[s] = i as u32;
                *ifs.last_mut()? = i;
            }
            ENDIF => {
                let s = ifs.pop()?;
                j[s] = i as u32;
            }
            LOOP | SWITCH => blocks.push((i, x.op, Vec::new())),
            ENDLOOP | ENDSWITCH => {
                let (s, k, brk) = blocks.pop()?;
                if (k == LOOP) != (x.op == ENDLOOP) {
                    return None;
                }
                j[s] = i as u32;
                j[i] = s as u32;
                for b in brk {
                    j[b] = i as u32;
                }
            }
            BREAK | BREAKC => blocks.last_mut()?.2.push(i),
            CONTINUE | CONTINUEC => {
                // The innermost loop (switches let continue through).
                let l = blocks.iter_mut().rev().find(|b| b.1 == LOOP)?;
                l.2.push(i);
            }
            CASE | DEFAULT => j[i] = blocks.last().filter(|b| b.1 == SWITCH)?.0 as u32,
            _ => {}
        }
    }
    if !ifs.is_empty() || !blocks.is_empty() {
        return None;
    }
    Some(j)
}
