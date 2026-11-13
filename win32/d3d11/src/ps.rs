//! Pixel shaders: SM4/SM5 instructions turned into the kernel's PsIns
//! (src/raster3d.rs), which run on its raster workers.
//!
//! Registers: r# are the kernel's temporaries from 0, x# arrays after
//! them, and four scratch ones at the end. Constant buffers, the
//! immediate constant buffer and the program's immediates share the
//! kernel's one constant file: each buffer the shader declares at a base,
//! the immediates after them, filled per draw. v# are the kernel's inputs
//! (planes per primitive), linked to the vertex shader's outputs by
//! semantic; SV_Position and SV_IsFrontFace are the kernel's own.
//! Texture reads go through kernel sampler slots, one per (t#, s#) pair.

use alloc::vec::Vec;

use gfx3d::raster3d::{self as k, pf, sm4, PsIns};

use crate::dxbc::{op, Ins, Opnd, Shader, SV_IS_FRONT_FACE, SV_POSITION, T_CB, T_DEPTH, T_ICB, T_IMM32, T_INDEXABLE, T_INPUT, T_NULL, T_OUTPUT, T_RESOURCE, T_SAMPLER, T_TEMP};

/// D3D9 opcodes the kernel keeps.
mod sio {
    pub const MOV: u16 = 1;
    pub const ADD: u16 = 2;
    pub const MAD: u16 = 4;
    pub const MUL: u16 = 5;
    pub const DP3: u16 = 8;
    pub const DP4: u16 = 9;
    pub const MIN: u16 = 10;
    pub const MAX: u16 = 11;
    pub const FRC: u16 = 19;
    pub const RET: u16 = 28;
    pub const ELSE: u16 = 42;
    pub const ENDIF: u16 = 43;
    pub const BREAK: u16 = 44;
    pub const TEX: u16 = 66;
    pub const DSX: u16 = 91;
    pub const DSY: u16 = 92;
    pub const TEXLDL: u16 = 95;
}

/// One kernel input's source: per component, the vertex shader output
/// (semantic, index, component within it) it comes from.
#[derive(Clone, Debug, Default)]
pub struct PsInput {
    pub comps: [Option<(Vec<u8>, u32, u8)>; 4],
    /// nointerpolation: the provoking vertex's.
    pub flat: bool,
}

pub struct PsCode {
    pub ins: Vec<PsIns>,
    pub inputs: Vec<PsInput>,
    /// Each constant buffer slot's base in the constant file and length
    /// (float4); u32::MAX: unused.
    pub cb_base: [u32; 14],
    pub cb_len: [u32; 14],
    /// The constant file after the buffers: the immediate constant buffer,
    /// then the immediates.
    pub tail_base: u32,
    pub tail: Vec<[u32; 4]>,
    pub nconst: u32,
    /// Kernel sampler slot i reads texture t# with sampler s# (None: ld
    /// and resinfo, no sampler).
    pub pairs: Vec<(u32, Option<u32>)>,
    pub writes_depth: bool,
}

struct Tr {
    out: Vec<PsIns>,
    xbase: Vec<u32>,
    scratch: u32,
    cb_base: [u32; 14],
    icb_base: u32,
    imm_base: u32,
    imms: Vec<[u32; 4]>,
    /// Each v#: the kernel operand file and index it reads.
    vmap: Vec<(u32, u32)>,
    pairs: Vec<(u32, Option<u32>)>,
    /// o# that is SV_Target0.
    target0: Option<u32>,
}

const SWZ_ID: u32 = 0xE4;

fn kop(o: u16) -> Option<u16> {
    use op::*;
    Some(match o {
        ADD => sio::ADD,
        MUL => sio::MUL,
        MAD => sio::MAD,
        MIN => sio::MIN,
        MAX => sio::MAX,
        MOV => sio::MOV,
        DP3 => sio::DP3,
        DP4 => sio::DP4,
        FRC => sio::FRC,
        DERIV_RTX | DERIV_RTX_COARSE | DERIV_RTX_FINE => sio::DSX,
        DERIV_RTY | DERIV_RTY_COARSE | DERIV_RTY_FINE => sio::DSY,
        UMAD => sm4::IMAD,
        AND | DIV | DP2 | EQ | EXP | FTOI | FTOU | GE | IADD | IEQ | IGE | ILT | IMAD | IMAX | IMIN | INE | INEG | ISHL | ISHR | ITOF | LOG | LT | MOVC | NE
        | NOT | OR | ROUND_NE | ROUND_NI | ROUND_PI | ROUND_Z | RSQ | SQRT | ULT | UGE | UMAX | UMIN | USHR | UTOF | XOR | RCP | F32TOF16 | F16TOF32
        | COUNTBITS | FIRSTBIT_HI | FIRSTBIT_LO | FIRSTBIT_SHI | UBFE | IBFE | BFREV => 0x100 + o,
        _ => return None,
    })
}

impl Tr {
    fn emit(&mut self, x: PsIns) {
        self.out.push(x);
    }

    fn ins(op: u16, dst: u32, src: [u32; 3]) -> PsIns {
        PsIns { op, ctrl: 0, sampler: 0, dst, src, pred: 0, jump: 0, rel: 0 }
    }

    fn temp_dst(t: u32, mask: u32) -> u32 {
        pf::TEMP | t << 4 | mask << 16
    }

    fn temp_src(t: u32) -> u32 {
        pf::TEMP | t << 4 | SWZ_ID << 16
    }

    /// The kernel's `rel` for an index relative to `r`.
    fn rel_of(&mut self, r: &crate::dxbc::Rel) -> Option<u32> {
        let t = match r.ty {
            T_TEMP => r.reg,
            _ => {
                // x#[n].c: copy it out first.
                let base = *self.xbase.get(r.reg as usize)?;
                let s = self.scratch + 3;
                self.emit(Self::ins(sio::MOV, Self::temp_dst(s, 15), [Self::temp_src(base + r.sub), 0, 0]));
                s
            }
        };
        Some(1 << 31 | t << 4 | (r.comp as u32) << 16)
    }

    fn imm(&mut self, v: [u32; 4]) -> u32 {
        let i = match self.imms.iter().position(|&x| x == v) {
            Some(i) => i,
            None => {
                self.imms.push(v);
                self.imms.len() - 1
            }
        };
        self.imm_base + i as u32
    }

    /// A source operand (`int`: its negation is an integer one). Copies
    /// relative or integer-negated ones to scratch `n` first.
    fn src(&mut self, o: &Opnd, int: bool, n: u32) -> Option<u32> {
        let swz = o.swz as u32;
        let (file, idx, rel) = match o.ty {
            T_TEMP => (pf::TEMP, o.idx[0], None),
            T_INDEXABLE => (pf::TEMP, *self.xbase.get(o.idx[0] as usize)? + o.idx[1], o.rel[1]),
            T_INPUT => {
                let &(f, i) = self.vmap.get(o.idx[0] as usize)?;
                if o.rel[0].is_some() {
                    return None;
                }
                (f, i, None)
            }
            T_CB => (pf::CONST, self.cb_base.get(o.idx[0] as usize).filter(|&&b| b != u32::MAX)? + o.idx[1], o.rel[1]),
            T_ICB => (pf::CONST, self.icb_base + o.idx[0], o.rel[0]),
            T_IMM32 => (pf::CONST, self.imm(o.imm), None),
            _ => return None,
        };
        let mut s = file | idx << 4;
        if let Some(r) = rel {
            // Moved out with the index applied.
            let rl = self.rel_of(&r)?;
            let t = self.scratch + n;
            let mut m = Self::ins(sio::MOV, Self::temp_dst(t, 15), [s | 1 << 28 | SWZ_ID << 16, 0, 0]);
            m.rel = rl;
            self.emit(m);
            s = Self::temp_src(t) & !(SWZ_ID << 16);
        }
        let modi = o.modi as u32;
        if int && modi != 0 {
            let t = self.scratch + n;
            self.emit(Self::ins(sm4::INEG, Self::temp_dst(t, 15), [s | swz << 16, 0, 0]));
            if modi != 1 {
                return None;
            }
            return Some(Self::temp_src(t));
        }
        let m = match modi {
            1 => 1,
            2 => 11,
            3 => 12,
            _ => 0,
        };
        Some(s | swz << 16 | m << 24)
    }

    /// A destination; None for one that goes nowhere (null, or an output
    /// the kernel doesn't keep: those go to scratch 2).
    fn dst(&mut self, o: &Opnd, sat: bool) -> Option<(u32, u32)> {
        let mask = o.mask as u32;
        let s = if sat { 1 << 20 } else { 0 };
        let (file, idx, rel) = match o.ty {
            T_TEMP => (pf::TEMP, o.idx[0], 0),
            T_INDEXABLE => {
                let b = *self.xbase.get(o.idx[0] as usize)? + o.idx[1];
                match o.rel[1] {
                    Some(r) => (pf::TEMP, b, self.rel_of(&r)?),
                    None => (pf::TEMP, b, 0),
                }
            }
            T_OUTPUT if Some(o.idx[0]) == self.target0 => (pf::COLOR, 0, 0),
            T_DEPTH => (pf::DEPTH, 0, 0),
            T_NULL => (pf::TEMP, self.scratch + 2, 0),
            _ => (pf::TEMP, self.scratch + 2, 0),
        };
        let m = if file == pf::DEPTH { 1 } else { mask };
        Some((file | idx << 4 | m << 16 | s | if rel != 0 { 1 << 28 } else { 0 }, rel))
    }

    /// The kernel sampler slot for texture `t` with sampler `s`.
    fn pair(&mut self, t: u32, s: Option<u32>) -> Option<u8> {
        let i = match self.pairs.iter().position(|&p| p == (t, s)) {
            Some(i) => i,
            None => {
                self.pairs.push((t, s));
                self.pairs.len() - 1
            }
        };
        (i < k::SAMPLERS).then_some(i as u8)
    }

    fn sample(&mut self, x: &Ins) -> Option<()> {
        use op::*;
        let ops = &x.ops;
        let res = ops.iter().find(|o| o.ty == T_RESOURCE)?;
        let smp = ops.iter().find(|o| o.ty == T_SAMPLER).map(|o| o.idx[0]);
        let slot = self.pair(res.idx[0], smp)?;
        let srcs: Vec<&Opnd> = ops[1..].iter().filter(|o| !matches!(o.ty, T_RESOURCE | T_SAMPLER)).collect();
        let c = self.src(srcs.first()?, x.op == LD || x.op == RESINFO, 0)?;
        let (kop, ctrl, src) = match x.op {
            SAMPLE => (sio::TEX, 0, [c, 0, 0]),
            SAMPLE_B | SAMPLE_L => {
                // The bias or level in the coordinates' w.
                let t = self.scratch;
                self.emit(Self::ins(sio::MOV, Self::temp_dst(t, 7), [c, 0, 0]));
                let b = self.src(srcs.get(1)?, false, 1)?;
                self.emit(Self::ins(sio::MOV, Self::temp_dst(t, 8), [(b & !(0xFF << 16)) | ((b >> 16) & 3) * 0x55 << 16, 0, 0]));
                if x.op == SAMPLE_B { (sio::TEX, 2, [Self::temp_src(t), 0, 0]) } else { (sio::TEXLDL, 0, [Self::temp_src(t), 0, 0]) }
            }
            SAMPLE_D => (sm4::SAMPLE_D, 0, [c, self.src(srcs.get(1)?, false, 1)?, self.src(srcs.get(2)?, false, 2)?]),
            SAMPLE_C | SAMPLE_C_LZ => (if x.op == SAMPLE_C { sm4::SAMPLE_C } else { sm4::SAMPLE_C_LZ }, 0, [c, self.src(srcs.get(1)?, false, 1)?, 0]),
            LD => (sm4::LD, 0, [c, 0, 0]),
            RESINFO => (sm4::RESINFO, (x.ctrl & 3) as u8, [c, 0, 0]),
            GATHER4 => (sm4::GATHER4, 0, [c, 0, 0]),
            LOD => (sm4::LOD, 0, [c, 0, 0]),
            _ => return None,
        };
        let (d, rel) = self.dst(x.dst(0), x.sat)?;
        // The resource's swizzle picks the result's components: through
        // scratch 3 when it isn't the identity.
        let swz = res.swz as u32;
        if swz == SWZ_ID {
            self.emit(PsIns { op: kop, ctrl, sampler: slot, dst: d, src, pred: 0, jump: 0, rel });
        } else {
            let t = self.scratch + 3;
            self.emit(PsIns { op: kop, ctrl, sampler: slot, dst: Self::temp_dst(t, 15), src, pred: 0, jump: 0, rel: 0 });
            let mut m = Self::ins(sio::MOV, d, [pf::TEMP | t << 4 | swz << 16, 0, 0]);
            m.rel = rel;
            self.emit(m);
        }
        Some(())
    }

    fn one(&mut self, x: &Ins) -> Option<()> {
        use op::*;
        let int = crate::vm::int_sources(x.op);
        match x.op {
            NOP | ENDIF => {
                if x.op == ENDIF {
                    self.emit(Self::ins(sio::ENDIF, 0, [0; 3]));
                }
            }
            IF | BREAKC | CONTINUEC | DISCARD | RETC => {
                let c = self.src(x.src(0), false, 0)?;
                let kop = match x.op {
                    IF => sm4::IF,
                    BREAKC => sm4::BREAKC,
                    CONTINUEC => sm4::CONTINUEC,
                    DISCARD => sm4::DISCARD,
                    _ => sm4::RETC,
                };
                let mut i = Self::ins(kop, 0, [c, 0, 0]);
                i.ctrl = x.nonzero() as u8;
                self.emit(i);
            }
            ELSE => self.emit(Self::ins(sio::ELSE, 0, [0; 3])),
            LOOP => self.emit(Self::ins(sm4::LOOP, 0, [0; 3])),
            ENDLOOP => self.emit(Self::ins(sm4::ENDLOOP, 0, [0; 3])),
            BREAK => self.emit(Self::ins(sio::BREAK, 0, [0; 3])),
            CONTINUE => self.emit(Self::ins(sm4::CONTINUE, 0, [0; 3])),
            RET => self.emit(Self::ins(sio::RET, 0, [0; 3])),
            SAMPLE | SAMPLE_B | SAMPLE_L | SAMPLE_D | SAMPLE_C | SAMPLE_C_LZ | LD | RESINFO | GATHER4 | LOD => self.sample(x)?,
            SINCOS | UDIV => {
                let a = self.src(x.src(0), int, 0)?;
                let b = if x.op == UDIV { self.src(x.src(1), int, 1)? } else { 0 };
                let (o0, o1) = if x.op == SINCOS { (sm4::SIN, sm4::COS) } else { (sm4::UDIV, sm4::UREM) };
                let first = x.dst(0).ty != T_NULL;
                let t = self.scratch + 2;
                if first {
                    self.emit(Self::ins(o0, Self::temp_dst(t, 15), [a, b, 0]));
                }
                if x.dst(1).ty != T_NULL {
                    let (d, rel) = self.dst(x.dst(1), x.sat)?;
                    let mut i = Self::ins(o1, d, [a, b, 0]);
                    i.rel = rel;
                    self.emit(i);
                }
                if first {
                    let (d, rel) = self.dst(x.dst(0), x.sat)?;
                    let mut i = Self::ins(sio::MOV, d, [Self::temp_src(t), 0, 0]);
                    i.rel = rel;
                    self.emit(i);
                }
            }
            IMUL | UMUL => {
                // Only the low half.
                if x.dst(0).ty != T_NULL {
                    return None;
                }
                let a = self.src(x.src(0), true, 0)?;
                let b = self.src(x.src(1), true, 1)?;
                let (d, rel) = self.dst(x.dst(1), false)?;
                let mut i = Self::ins(sm4::IMUL, d, [a, b, 0]);
                i.rel = rel;
                self.emit(i);
            }
            o => {
                let kop = kop(o)?;
                let n = x.ops.len() - x.ndst as usize;
                if n > 3 {
                    return None;
                }
                let mut src = [0u32; 3];
                for i in 0..n {
                    src[i] = self.src(x.src(i), int, i as u32)?;
                }
                if x.dst(0).ty == T_NULL {
                    return Some(());
                }
                let (d, rel) = self.dst(x.dst(0), x.sat)?;
                let mut i = Self::ins(kop, d, src);
                i.rel = rel;
                self.emit(i);
            }
        }
        Some(())
    }
}

/// Translate a pixel shader; None if it uses what the kernel can't run.
pub fn translate(sh: &Shader) -> Option<PsCode> {
    if sh.kind != 0 {
        return None;
    }
    let jumps = crate::dxbc::jumps(&sh.ins)?;
    // Temporaries: r#, then x#, then 4 scratch.
    let mut next = sh.temps;
    let mut xbase = Vec::new();
    for &(n, _) in &sh.xtemps {
        xbase.push(next);
        next += n;
    }
    if next + 4 > k::PS_TEMPS as u32 {
        return None;
    }
    // The constant file: declared buffers, the immediate constant buffer,
    // the immediates.
    let mut cb_base = [u32::MAX; 14];
    let mut cb_len = [0u32; 14];
    let mut at = 0u32;
    for (i, &n) in sh.cb_size.iter().enumerate() {
        if n > 0 {
            cb_base[i] = at;
            cb_len[i] = n;
            at += n;
        }
    }
    let icb_base = at;
    let icb: Vec<[u32; 4]> = sh.icb.chunks(4).map(|c| core::array::from_fn(|k| c.get(k).copied().unwrap_or(0))).collect();
    at += icb.len() as u32;
    // The inputs: v# to kernel inputs or the kernel's own values.
    let mut vmap = alloc::vec![(pf::CONST, 0u32); 32];
    let mut inputs: Vec<PsInput> = Vec::new();
    let mut pos_reg = None;
    let mut zero_regs = Vec::new();
    for d in &sh.inputs {
        let r = d.reg as usize;
        if d.special != 0 || r >= vmap.len() {
            continue;
        }
        match d.sysval {
            SV_POSITION => pos_reg = Some(r),
            SV_IS_FRONT_FACE => vmap[r] = (pf::MISC, 3),
            0 => {
                let ki = match vmap[r] {
                    (pf::INPUT, i) => i as usize,
                    _ => {
                        inputs.push(PsInput::default());
                        vmap[r] = (pf::INPUT, inputs.len() as u32 - 1);
                        inputs.len() - 1
                    }
                };
                for c in 0..4u8 {
                    if d.mask & (1 << c) == 0 {
                        continue;
                    }
                    let e = sh.isgn.iter().find(|e| e.reg == d.reg && e.mask & (1 << c) != 0)?;
                    let first = e.mask.trailing_zeros() as u8;
                    inputs[ki].comps[c as usize] = Some((e.name.clone(), e.index, c - first));
                }
                inputs[ki].flat |= d.interp == 1;
            }
            _ => zero_regs.push(r),
        }
    }
    if inputs.len() > k::PS_INPUTS {
        return None;
    }
    let target0 = sh.osgn.iter().find(|e| e.name == b"SV_TARGET" && e.index == 0).map(|e| e.reg);
    let mut tr = Tr {
        out: Vec::new(),
        xbase,
        scratch: next,
        cb_base,
        icb_base,
        imm_base: at,
        imms: Vec::new(),
        vmap,
        pairs: Vec::new(),
        target0,
    };
    for r in zero_regs {
        tr.vmap[r] = (pf::CONST, tr.imm([0; 4]));
    }
    // SV_Position: x and y the pixel's centre, z its depth, w the clip w
    // (the kernel gives 1/w).
    if let Some(r) = pos_reg {
        // A temporary of its own for the whole program; scratch moves up.
        let t = tr.scratch;
        tr.scratch += 1;
        tr.emit(Tr::ins(sio::MOV, Tr::temp_dst(t, 15), [pf::MISC | 2 << 4 | SWZ_ID << 16, 0, 0]));
        tr.emit(Tr::ins(sm4::RCP, Tr::temp_dst(t, 8), [pf::TEMP | t << 4 | 0xFF << 16, 0, 0]));
        tr.vmap[r] = (pf::TEMP, t);
        if tr.scratch + 4 > k::PS_TEMPS as u32 {
            return None;
        }
    }
    let mut main = alloc::vec![0usize; sh.ins.len()];
    for (i, x) in sh.ins.iter().enumerate() {
        tr.one(x)?;
        main[i] = tr.out.len().max(1) - 1;
    }
    // The jumps, in the kernel's terms.
    for (i, x) in sh.ins.iter().enumerate() {
        let to = main[jumps[i] as usize] as u32;
        let ki = main[i];
        match x.op {
            op::IF | op::ELSE | op::LOOP | op::BREAK | op::BREAKC | op::CONTINUE | op::CONTINUEC => tr.out[ki].jump = to,
            _ => {}
        }
    }
    if tr.out.is_empty() {
        tr.out.push(Tr::ins(sio::MOV, Tr::temp_dst(tr.scratch, 15), [Tr::temp_src(tr.scratch), 0, 0]));
    }
    let mut tail = icb;
    tail.extend_from_slice(&tr.imms);
    let nconst = icb_base + tail.len() as u32;
    if nconst as usize > k::PS_CONSTS_MAX {
        return None;
    }
    Some(PsCode {
        ins: tr.out,
        inputs,
        cb_base,
        cb_len,
        tail_base: icb_base,
        tail,
        nconst,
        pairs: tr.pairs,
        writes_depth: sh.depth_out,
    })
}
