// -----| Sound streams for Win32 programs |-----
//
// VEIL_SOUND(op, a, b, c): what winmm.dll (waveOut, PlaySound) and
// kernel32's Beep stand on. A stream is a handle object; closing its last
// handle (or the process ending) takes it out of the mixer (audio.rs).

use alloc::sync::Arc;

use nt_abi::{status, NtStatus};

use crate::nt_file::ptr_ok;
use crate::nt_objects::KernelObjectRef;
use crate::process::Process;

/// The handle object. Dropped with its last handle.
pub struct SoundHandle(pub Arc<crate::audio::Stream>);

impl Drop for SoundHandle {
    fn drop(&mut self) {
        crate::audio::close_stream(&self.0);
    }
}

/// STATUS_DEVICE_DOES_NOT_EXIST: no sound device.
const NO_DEVICE: NtStatus = 0xC000_00C0u32 as i32;

/// VEIL_SOUND op 1's argument.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct SoundWrite {
    pub data: u64,
    pub len: u32,
    /// Out: the bytes queued (whole frames; less than `len` when the queue
    /// is full).
    pub written: u32,
}

/// VEIL_SOUND op 2's result.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct SoundInfo {
    /// Bytes the mixer has taken since the stream opened.
    pub consumed: u64,
    pub queued: u32,
    pub free: u32,
}

/// VEIL_SOUND op 4's result.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct SoundDevice {
    pub present: u32,
    /// 0-100.
    pub volume: u32,
}

fn stream(proc: &Process, h: u64) -> Option<Arc<SoundHandle>> {
    match crate::nt_process::object(proc, h) {
        Some(KernelObjectRef::Sound(s)) => Some(s),
        _ => None,
    }
}

/// VEIL_SOUND(op, a, b, c): see nt_abi.
pub fn call(proc: &Process, op: u64, a: u64, b: u64, c: u64) -> NtStatus {
    match op {
        // Open: a = rate, b = bits << 8 | channels, c = handle out*.
        0 => {
            if !crate::audio::present() {
                return NO_DEVICE;
            }
            if !ptr_ok(c, 8) {
                return status::ACCESS_VIOLATION;
            }
            let Some(s) = crate::audio::open_stream(a as u32, (b & 0xFF) as u16, ((b >> 8) & 0xFF) as u16) else {
                return status::INVALID_PARAMETER;
            };
            let h = proc.handles.lock().allocate_ref(KernelObjectRef::Sound(Arc::new(SoundHandle(s))), 0x001F_0003, 0);
            unsafe { *(c as *mut u64) = h as u64 };
            status::SUCCESS
        }
        // Write: a = stream, b = SoundWrite*.
        1 => {
            let Some(s) = stream(proc, a) else { return status::INVALID_HANDLE };
            if !ptr_ok(b, core::mem::size_of::<SoundWrite>()) {
                return status::ACCESS_VIOLATION;
            }
            let mut w = unsafe { core::ptr::read_unaligned(b as *const SoundWrite) };
            if !ptr_ok(w.data, w.len as usize) {
                return status::ACCESS_VIOLATION;
            }
            let data = unsafe { core::slice::from_raw_parts(w.data as *const u8, w.len as usize) };
            // In pieces, so interrupts are never off for long.
            let mut done = 0usize;
            while done < data.len() {
                let piece = &data[done..data.len().min(done + 4096)];
                let n = crate::audio::with_stream(&s.0, |st| st.push(piece));
                done += n;
                if n < piece.len() {
                    break;
                }
            }
            w.written = done as u32;
            unsafe { core::ptr::write_unaligned(b as *mut SoundWrite, w) };
            status::SUCCESS
        }
        // Info: a = stream, b = SoundInfo* out.
        2 => {
            let Some(s) = stream(proc, a) else { return status::INVALID_HANDLE };
            if !ptr_ok(b, core::mem::size_of::<SoundInfo>()) {
                return status::ACCESS_VIOLATION;
            }
            let i = crate::audio::with_stream(&s.0, |st| SoundInfo {
                consumed: st.consumed,
                queued: st.queued() as u32,
                free: st.free() as u32,
            });
            unsafe { core::ptr::write_unaligned(b as *mut SoundInfo, i) };
            status::SUCCESS
        }
        // Control: a = stream, b = what, c = value. 0 pause, 1 resume,
        // 2 flush the queue, 3 volume (c = left | right << 16, 0-0xFFFF).
        3 => {
            let Some(s) = stream(proc, a) else { return status::INVALID_HANDLE };
            crate::audio::with_stream(&s.0, |st| match b {
                0 => st.paused = true,
                1 => st.paused = false,
                2 => st.flush(),
                3 => {
                    let g = |v: u64| -> i64 { let v = (v & 0xFFFF) as i64; if v == 0xFFFF { 0x10000 } else { v } };
                    st.gain = [g(c), g(c >> 16)];
                }
                _ => {}
            });
            if b > 3 { status::INVALID_PARAMETER } else { status::SUCCESS }
        }
        // Device: a = SoundDevice* out.
        4 => {
            if !ptr_ok(a, core::mem::size_of::<SoundDevice>()) {
                return status::ACCESS_VIOLATION;
            }
            let d = SoundDevice { present: crate::audio::present() as u32, volume: crate::audio::get_volume() as u32 };
            unsafe { core::ptr::write_unaligned(a as *mut SoundDevice, d) };
            status::SUCCESS
        }
        _ => status::INVALID_PARAMETER,
    }
}
