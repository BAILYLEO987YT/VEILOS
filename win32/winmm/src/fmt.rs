//! PCM formats: WAVEFORMATEX in, what the kernel takes out.

use alloc::vec::Vec;

const WAVE_FORMAT_PCM: u16 = 1;
const WAVE_FORMAT_IEEE_FLOAT: u16 = 3;
const WAVE_FORMAT_EXTENSIBLE: u16 = 0xFFFE;

#[derive(Clone, Copy)]
pub struct Fmt {
    pub float: bool,
    pub channels: u16,
    pub rate: u32,
    pub bits: u16,
    /// Bytes per frame of the source.
    pub block: usize,
}

impl Fmt {
    /// From a WAVEFORMATEX (or WAVEFORMATEXTENSIBLE) of `len` bytes, if it's
    /// PCM or float PCM the kernel can play once converted.
    pub unsafe fn parse(p: *const u8, len: usize) -> Option<Fmt> {
        if p.is_null() || len < 14 {
            return None;
        }
        let u16_at = |o: usize| unsafe { u16::from_le_bytes([*p.add(o), *p.add(o + 1)]) };
        let u32_at = |o: usize| unsafe { u32::from_le_bytes([*p.add(o), *p.add(o + 1), *p.add(o + 2), *p.add(o + 3)]) };
        let mut tag = u16_at(0);
        let channels = u16_at(2);
        let rate = u32_at(4);
        let block = u16_at(12) as usize;
        // PCMWAVEFORMAT (16 bytes) has the bits; WAVEFORMAT (14) doesn't.
        let bits = if len >= 16 { u16_at(14) } else { 8 };
        if tag == WAVE_FORMAT_EXTENSIBLE {
            if len < 40 || u16_at(16) < 22 {
                return None;
            }
            // The SubFormat GUID's first field is the format tag.
            tag = u32_at(24) as u16;
        }
        let float = match tag {
            WAVE_FORMAT_PCM => false,
            WAVE_FORMAT_IEEE_FLOAT => true,
            _ => return None,
        };
        let ok_bits = if float { bits == 32 } else { matches!(bits, 8 | 16 | 24 | 32) };
        if !ok_bits || !(1..=8).contains(&channels) || !(1000..=192_000).contains(&rate) {
            return None;
        }
        if block != channels as usize * bits as usize / 8 {
            return None;
        }
        Some(Fmt { float, channels, rate, bits, block })
    }

    /// The kernel takes it as it is.
    pub fn direct(&self) -> bool {
        !self.float && (self.bits == 8 || self.bits == 16) && self.channels <= 2
    }
    pub fn out_bits(&self) -> u16 {
        if self.direct() { self.bits } else { 16 }
    }
    pub fn out_channels(&self) -> u16 {
        self.channels.min(2)
    }
    /// Bytes per frame as the kernel gets it.
    pub fn out_block(&self) -> usize {
        self.out_channels() as usize * self.out_bits() as usize / 8
    }

    fn sample(&self, f: &[u8], ch: usize) -> i16 {
        let b = self.bits as usize / 8;
        let s = &f[ch * b..ch * b + b];
        if self.float {
            let v = f32::from_le_bytes([s[0], s[1], s[2], s[3]]);
            let v = if v.is_nan() { 0.0 } else { v.clamp(-1.0, 1.0) };
            return (v * 32767.0) as i16;
        }
        match b {
            1 => ((s[0] as i16) - 128) << 8,
            2 => i16::from_le_bytes([s[0], s[1]]),
            3 => i16::from_le_bytes([s[1], s[2]]),
            _ => i16::from_le_bytes([s[2], s[3]]),
        }
    }

    /// Whole frames of `src` as the kernel takes them, appended to `out`.
    /// More than two channels keep the front left and right.
    pub fn convert(&self, src: &[u8], out: &mut Vec<u8>) {
        if self.direct() {
            out.extend_from_slice(&src[..src.len() / self.block * self.block]);
            return;
        }
        let oc = self.out_channels() as usize;
        for f in src.chunks_exact(self.block) {
            for c in 0..oc {
                out.extend_from_slice(&self.sample(f, c).to_le_bytes());
            }
        }
    }
}

/// A RIFF WAVE image: its format and its sample bytes.
pub fn parse_wav(data: &[u8]) -> Option<(Fmt, &[u8])> {
    if data.len() < 12 || &data[0..4] != b"RIFF" || &data[8..12] != b"WAVE" {
        return None;
    }
    let mut fmt = None;
    let mut at = 12;
    while at + 8 <= data.len() {
        let id = &data[at..at + 4];
        let len = u32::from_le_bytes([data[at + 4], data[at + 5], data[at + 6], data[at + 7]]) as usize;
        let body = at + 8;
        let end = body.saturating_add(len).min(data.len());
        if id == b"fmt " {
            fmt = unsafe { Fmt::parse(data[body..end].as_ptr(), end - body) };
        } else if id == b"data" {
            return fmt.map(|f| (f, &data[body..end]));
        }
        at = body.saturating_add(len + (len & 1));
    }
    None
}
