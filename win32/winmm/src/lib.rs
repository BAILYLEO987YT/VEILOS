//! VEILOS `winmm.dll`: sound and the multimedia timer (V1_CHECKLIST.md
//! audio; WIN32_DESIGN.md).
//!
//! Sound goes to the kernel's mixer (src/audio.rs) through VEIL_SOUND
//! streams (src/nt_sound.rs), one per open waveOut device and one for
//! PlaySound. The kernel takes 8- or 16-bit PCM, mono or stereo, at any rate
//! from 1 to 192 kHz; other PCM (24/32-bit, float, more channels) is
//! converted here first (`fmt.rs`). There is one output device and no input.

#![no_std]

extern crate alloc;

#[path = "../../dllrt.rs"]
mod dllrt;

mod fmt;
mod play;
mod wave;

use nt_abi::NtStatus;

pub(crate) type Bool = i32;
pub(crate) type Handle = u64;

#[link(name = "ntdll", kind = "raw-dylib")]
unsafe extern "system" {
    pub(crate) fn VeilSound(op: u64, a: u64, b: u64, c: u64) -> NtStatus;
}

#[link(name = "kernel32", kind = "raw-dylib")]
unsafe extern "system" {
    pub(crate) fn CloseHandle(h: Handle) -> Bool;
    pub(crate) fn CreateEventW(sa: usize, manual: Bool, initial: Bool, name: *const u16) -> Handle;
    pub(crate) fn SetEvent(h: Handle) -> Bool;
    pub(crate) fn WaitForSingleObject(h: Handle, ms: u32) -> u32;
    pub(crate) fn CreateThread(sa: usize, stack: usize, start: unsafe extern "system" fn(usize) -> u32, param: usize, flags: u32, tid: *mut u32) -> Handle;
    pub(crate) fn GetCurrentThreadId() -> u32;
    pub(crate) fn Sleep(ms: u32);
    pub(crate) fn GetTickCount() -> u32;
    pub(crate) fn CreateFileW(name: *const u16, access: u32, share: u32, sa: usize, disposition: u32, flags: u32, template: Handle) -> Handle;
    pub(crate) fn GetFileSize(h: Handle, high: *mut u32) -> u32;
    pub(crate) fn ReadFile(h: Handle, buf: *mut u8, len: u32, read: *mut u32, ov: usize) -> Bool;
    pub(crate) fn FindResourceW(module: Handle, name: *const u16, kind: *const u16) -> Handle;
    pub(crate) fn LoadResource(module: Handle, res: Handle) -> Handle;
    pub(crate) fn LockResource(data: Handle) -> *const u8;
    pub(crate) fn SizeofResource(module: Handle, res: Handle) -> u32;
    pub(crate) fn GetModuleHandleW(name: *const u16) -> Handle;
    pub(crate) fn RegGetValueW(key: Handle, sub: *const u16, name: *const u16, flags: u32, kind: *mut u32, data: *mut u8, size: *mut u32) -> u32;
    pub(crate) fn MultiByteToWideChar(cp: u32, flags: u32, s: *const u8, n: i32, w: *mut u16, wn: i32) -> i32;
}

#[link(name = "user32", kind = "raw-dylib")]
unsafe extern "system" {
    pub(crate) fn PostMessageW(hwnd: u64, msg: u32, w: usize, l: isize) -> Bool;
    pub(crate) fn PostThreadMessageW(tid: u32, msg: u32, w: usize, l: isize) -> Bool;
}

#[no_mangle]
pub extern "system" fn DllMain(_module: usize, _reason: u32, _reserved: usize) -> i32 {
    1
}

// -----| MMRESULT values |-----

pub(crate) const MMSYSERR_NOERROR: u32 = 0;
pub(crate) const MMSYSERR_ERROR: u32 = 1;
pub(crate) const MMSYSERR_BADDEVICEID: u32 = 2;
pub(crate) const MMSYSERR_INVALHANDLE: u32 = 5;
pub(crate) const MMSYSERR_NODRIVER: u32 = 6;
pub(crate) const MMSYSERR_NOMEM: u32 = 7;
pub(crate) const MMSYSERR_NOTSUPPORTED: u32 = 8;
pub(crate) const MMSYSERR_INVALFLAG: u32 = 10;
pub(crate) const MMSYSERR_INVALPARAM: u32 = 11;
pub(crate) const WAVERR_BADFORMAT: u32 = 32;
pub(crate) const WAVERR_STILLPLAYING: u32 = 33;
pub(crate) const WAVERR_UNPREPARED: u32 = 34;
pub(crate) const TIMERR_NOCANDO: u32 = 97;

/// A NUL-terminated UTF-16 string as a slice (without the NUL).
pub(crate) unsafe fn wstr<'a>(p: *const u16) -> &'a [u16] {
    let mut n = 0;
    while unsafe { *p.add(n) } != 0 {
        n += 1;
    }
    unsafe { core::slice::from_raw_parts(p, n) }
}

/// An ANSI string (the ANSI code page is UTF-8 here) as NUL-terminated UTF-16.
pub(crate) unsafe fn widen(p: *const u8) -> alloc::vec::Vec<u16> {
    let n = unsafe { MultiByteToWideChar(0, 0, p, -1, core::ptr::null_mut(), 0) }.max(1);
    let mut w = alloc::vec![0u16; n as usize];
    unsafe { MultiByteToWideChar(0, 0, p, -1, w.as_mut_ptr(), n) };
    w
}

// -----| the multimedia timer |-----

#[no_mangle]
pub extern "system" fn timeGetTime() -> u32 {
    unsafe { GetTickCount() }
}

/// The timer already ticks every millisecond.
#[no_mangle]
pub extern "system" fn timeBeginPeriod(period: u32) -> u32 {
    if period == 0 { TIMERR_NOCANDO } else { MMSYSERR_NOERROR }
}

#[no_mangle]
pub extern "system" fn timeEndPeriod(period: u32) -> u32 {
    if period == 0 { TIMERR_NOCANDO } else { MMSYSERR_NOERROR }
}

/// TIMECAPS { wPeriodMin, wPeriodMax }.
#[no_mangle]
pub unsafe extern "system" fn timeGetDevCaps(caps: *mut u32, size: u32) -> u32 {
    if caps.is_null() || size < 8 {
        return TIMERR_NOCANDO;
    }
    unsafe {
        *caps = 1;
        *caps.add(1) = 1_000_000;
    }
    MMSYSERR_NOERROR
}

/// MMTIME with TIME_MS.
#[no_mangle]
pub unsafe extern "system" fn timeGetSystemTime(t: *mut u32, size: u32) -> u32 {
    if t.is_null() || size < 8 {
        return MMSYSERR_INVALPARAM;
    }
    unsafe {
        *t = 1; // TIME_MS
        *t.add(1) = GetTickCount();
    }
    MMSYSERR_NOERROR
}

// -----| devices that aren't here |-----

#[no_mangle]
pub extern "system" fn waveInGetNumDevs() -> u32 {
    0
}

macro_rules! no_device {
    ($($name:ident($($arg:ident),*) => $ret:expr;)*) => {$(
        #[no_mangle]
        pub extern "system" fn $name($($arg: usize),*) -> u32 {
            $(let _ = $arg;)*
            $ret
        }
    )*};
}

no_device! {
    waveInOpen(a, b, c, d, e, f) => MMSYSERR_BADDEVICEID;
    waveInGetDevCapsA(a, b, c) => MMSYSERR_BADDEVICEID;
    waveInGetDevCapsW(a, b, c) => MMSYSERR_BADDEVICEID;
    waveInClose(a) => MMSYSERR_INVALHANDLE;
    waveInPrepareHeader(a, b, c) => MMSYSERR_INVALHANDLE;
    waveInUnprepareHeader(a, b, c) => MMSYSERR_INVALHANDLE;
    waveInAddBuffer(a, b, c) => MMSYSERR_INVALHANDLE;
    waveInStart(a) => MMSYSERR_INVALHANDLE;
    waveInStop(a) => MMSYSERR_INVALHANDLE;
    waveInReset(a) => MMSYSERR_INVALHANDLE;
    waveInGetPosition(a, b, c) => MMSYSERR_INVALHANDLE;
    midiOutOpen(a, b, c, d, e) => MMSYSERR_BADDEVICEID;
    midiOutGetDevCapsA(a, b, c) => MMSYSERR_BADDEVICEID;
    midiOutGetDevCapsW(a, b, c) => MMSYSERR_BADDEVICEID;
    midiInOpen(a, b, c, d, e) => MMSYSERR_BADDEVICEID;
    auxGetDevCapsA(a, b, c) => MMSYSERR_BADDEVICEID;
    auxGetDevCapsW(a, b, c) => MMSYSERR_BADDEVICEID;
    mixerOpen(a, b, c, d, e) => MMSYSERR_BADDEVICEID;
}

#[no_mangle]
pub extern "system" fn midiOutGetNumDevs() -> u32 {
    0
}

#[no_mangle]
pub extern "system" fn midiInGetNumDevs() -> u32 {
    0
}

#[no_mangle]
pub extern "system" fn auxGetNumDevs() -> u32 {
    0
}

#[no_mangle]
pub extern "system" fn mixerGetNumDevs() -> u32 {
    0
}
