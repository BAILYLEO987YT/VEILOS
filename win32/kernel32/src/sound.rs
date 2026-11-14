//! Beep: a tone of the given pitch, through the sound device (VEIL_SOUND),
//! as Windows 7 and later play it. It returns when the tone has played.

use crate::{Bool, Handle};

#[link(name = "ntdll", kind = "raw-dylib")]
unsafe extern "system" {
    fn VeilSound(op: u64, a: u64, b: u64, c: u64) -> i32;
}

#[repr(C)]
struct SoundWrite {
    data: u64,
    len: u32,
    written: u32,
}

#[repr(C)]
#[derive(Default)]
struct SoundInfo {
    consumed: u64,
    queued: u32,
    free: u32,
}

const RATE: u32 = 48_000;

#[no_mangle]
pub extern "system" fn Beep(freq: u32, ms: u32) -> Bool {
    let mut h: Handle = 0;
    // Windows answers TRUE for a pitch out of range (37-32767 Hz) too, and
    // plays nothing then. 48 kHz, 16-bit mono.
    if !(0x25..=0x7FFF).contains(&freq) || unsafe { VeilSound(0, RATE as u64, (16 << 8) | 1, &mut h as *mut Handle as u64) } != 0 {
        // No sound device: take the time anyway.
        crate::time::Sleep(ms);
        return 1;
    }
    let total = RATE as u64 * ms as u64 / 1000;
    // Phase in 1/2^32 of a period per sample.
    let step = ((freq as u64) << 32) / RATE as u64;
    let fade = (RATE as u64 / 200).min(total / 2).max(1); // 5 ms in and out
    let mut phase: u32 = 0;
    let mut made: u64 = 0;
    let mut buf = [0i16; 2048];
    while made < total {
        let n = (total - made).min(buf.len() as u64) as usize;
        for (i, s) in buf[..n].iter_mut().enumerate() {
            let k = made + i as u64;
            // A parabola close to a sine: x in -32768..32767 over a period.
            let x = (phase >> 16) as i64 - 32768;
            let y = 4 * x * (32768 - x.abs()) / 32768; // -32768..32768
            let env = (k.min(total - 1 - k).min(fade) * 1024 / fade) as i64; // 0..1024
            *s = (-(y * env / 1024) * 3 / 10) as i16;
            phase = phase.wrapping_add(step as u32);
        }
        let mut at = 0;
        while at < n * 2 {
            let mut w = SoundWrite { data: buf.as_ptr() as u64 + at as u64, len: (n * 2 - at) as u32, written: 0 };
            unsafe { VeilSound(1, h, &mut w as *mut SoundWrite as u64, 0) };
            at += w.written as usize;
            if at < n * 2 {
                crate::time::Sleep(5);
            }
        }
        made += n as u64;
    }
    // Until the mixer has taken it all.
    loop {
        let mut i = SoundInfo::default();
        if unsafe { VeilSound(2, h, &mut i as *mut SoundInfo as u64, 0) } != 0 || i.consumed >= total * 2 {
            break;
        }
        crate::time::Sleep(5);
    }
    crate::CloseHandle(h);
    1
}
