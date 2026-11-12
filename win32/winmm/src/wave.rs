//! waveOut: one device, the kernel's mixer.
//!
//! An HWAVEOUT is a `Dev`. Headers written are queued in order; a feeder
//! thread per device converts them into the kernel stream as it has room,
//! and marks each one done (WHDR_DONE, WOM_DONE) once the mixer has taken
//! its last byte. Positions count what the mixer has taken.

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::vec::Vec;

use crate::dllrt::Lock;
use crate::fmt::Fmt;
use crate::*;

// -----| the kernel's streams |-----

#[repr(C)]
struct SoundWrite {
    data: u64,
    len: u32,
    written: u32,
}

#[repr(C)]
#[derive(Default)]
pub(crate) struct SoundInfo {
    pub consumed: u64,
    pub queued: u32,
    pub free: u32,
}

#[repr(C)]
#[derive(Default)]
struct SoundDevice {
    present: u32,
    volume: u32,
}

pub(crate) fn device_present() -> bool {
    let mut d = SoundDevice::default();
    unsafe { VeilSound(4, &mut d as *mut _ as u64, 0, 0) == 0 && d.present != 0 }
}

pub(crate) fn k_open(f: &Fmt) -> Option<Handle> {
    let mut h: Handle = 0;
    let s = unsafe { VeilSound(0, f.rate as u64, ((f.out_bits() as u64) << 8) | f.out_channels() as u64, &mut h as *mut _ as u64) };
    (s == 0).then_some(h)
}

pub(crate) fn k_write(h: Handle, data: &[u8]) -> usize {
    let mut w = SoundWrite { data: data.as_ptr() as u64, len: data.len() as u32, written: 0 };
    unsafe { VeilSound(1, h, &mut w as *mut _ as u64, 0) };
    w.written as usize
}

pub(crate) fn k_info(h: Handle) -> SoundInfo {
    let mut i = SoundInfo::default();
    unsafe { VeilSound(2, h, &mut i as *mut _ as u64, 0) };
    i
}

pub(crate) fn k_control(h: Handle, what: u64, value: u64) {
    unsafe { VeilSound(3, h, what, value) };
}

// -----| WAVEHDR |-----

#[repr(C)]
pub struct WaveHdr {
    data: *mut u8,
    len: u32,
    recorded: u32,
    user: usize,
    flags: u32,
    loops: u32,
    next: *mut WaveHdr,
    reserved: usize,
}

const WHDR_DONE: u32 = 1;
const WHDR_PREPARED: u32 = 2;
const WHDR_BEGINLOOP: u32 = 4;
const WHDR_ENDLOOP: u32 = 8;
const WHDR_INQUEUE: u32 = 0x10;

const WOM_OPEN: u32 = 0x3BB;
const WOM_CLOSE: u32 = 0x3BC;
const WOM_DONE: u32 = 0x3BD;

const CALLBACK_TYPEMASK: u32 = 0x0007_0000;
const CALLBACK_NULL: u32 = 0;
const CALLBACK_WINDOW: u32 = 0x0001_0000;
const CALLBACK_THREAD: u32 = 0x0002_0000;
const CALLBACK_FUNCTION: u32 = 0x0003_0000;
const CALLBACK_EVENT: u32 = 0x0005_0000;
const WAVE_FORMAT_QUERY: u32 = 0x1;
const WAVE_MAPPER: usize = 0xFFFF_FFFF;

// -----| devices |-----

const MAGIC: u32 = 0x5645_574F; // "VEWO"

struct State {
    stream: Handle,
    fmt: Fmt,
    queue: VecDeque<*mut WaveHdr>,
    /// How many headers at the front of `queue` are wholly in the kernel,
    /// and the kernel byte count at which each of them ends.
    ends: VecDeque<u64>,
    /// Bytes of the next header already given to the kernel.
    offset: usize,
    /// Loops left for the header being fed (WHDR_BEGINLOOP|WHDR_ENDLOOP).
    loops_left: u32,
    /// Bytes given to the kernel since the stream opened.
    written: u64,
    /// The kernel's consumed count when the position was last reset.
    base: u64,
    paused: bool,
    closing: bool,
    volume: u32,
    scratch: Vec<u8>,
}

unsafe impl Send for State {}

pub struct Dev {
    magic: u32,
    state: Lock<State>,
    wake: Handle,
    thread: Handle,
    thread_id: u32,
    cb_kind: u32,
    cb: usize,
    instance: usize,
}

/// Open devices, to tell a real HWAVEOUT from anything else.
static OPEN: Lock<Vec<usize>> = Lock::new(Vec::new());
/// waveOutSetVolume on the device rather than a handle: new streams start
/// with it.
static DEVICE_VOLUME: Lock<u32> = Lock::new(0xFFFF_FFFF);

fn dev(h: usize) -> Option<&'static Dev> {
    if h == 0 || !OPEN.lock().contains(&h) {
        return None;
    }
    let d = unsafe { &*(h as *const Dev) };
    (d.magic == MAGIC).then_some(d)
}

fn notify(d: &Dev, msg: u32, p1: usize) {
    let h = d as *const Dev as usize;
    unsafe {
        match d.cb_kind {
            CALLBACK_FUNCTION if d.cb != 0 => {
                let f: unsafe extern "system" fn(usize, u32, usize, usize, usize) = core::mem::transmute(d.cb);
                f(h, msg, d.instance, p1, 0);
            }
            CALLBACK_WINDOW if d.cb != 0 => {
                PostMessageW(d.cb as u64, msg, h, p1 as isize);
            }
            CALLBACK_THREAD if d.cb != 0 => {
                PostThreadMessageW(d.cb as u32, msg, h, p1 as isize);
            }
            CALLBACK_EVENT if d.cb != 0 => {
                SetEvent(d.cb as u64);
            }
            _ => {}
        }
    }
}

fn volume_value(v: u32) -> u64 {
    v as u64
}

impl State {
    /// Give the kernel what it has room for, and take the headers it has
    /// finished off the queue (returned, to be reported done).
    fn pump(&mut self) -> Vec<*mut WaveHdr> {
        let mut done = Vec::new();
        if !self.paused {
            let mut info = k_info(self.stream);
            let block = self.fmt.block;
            let out_block = self.fmt.out_block();
            loop {
                let fed = self.ends.len();
                if fed >= self.queue.len() {
                    break;
                }
                let hdr = unsafe { &mut *self.queue[fed] };
                if self.offset == 0 && hdr.flags & (WHDR_BEGINLOOP | WHDR_ENDLOOP) == (WHDR_BEGINLOOP | WHDR_ENDLOOP) && self.loops_left == 0 {
                    self.loops_left = hdr.loops.max(1);
                }
                let total = hdr.len as usize / block * block;
                let room = info.free as usize / out_block;
                let frames = ((total - self.offset) / block).min(room);
                if frames == 0 && self.offset < total {
                    break; // the kernel is full
                }
                if frames > 0 {
                    let src = unsafe { core::slice::from_raw_parts(hdr.data.add(self.offset), frames * block) };
                    let n = if self.fmt.direct() {
                        k_write(self.stream, src)
                    } else {
                        self.scratch.clear();
                        self.fmt.convert(src, &mut self.scratch);
                        k_write(self.stream, &self.scratch)
                    };
                    let took = n / out_block;
                    self.offset += took * block;
                    self.written += (took * out_block) as u64;
                    info.free = info.free.saturating_sub(n as u32);
                    if took < frames {
                        break;
                    }
                }
                if self.offset >= total {
                    self.offset = 0;
                    if self.loops_left > 1 {
                        self.loops_left -= 1;
                        continue;
                    }
                    self.loops_left = 0;
                    self.ends.push_back(self.written);
                }
            }
            let consumed = k_info(self.stream).consumed;
            while let Some(&end) = self.ends.front() {
                if end > consumed {
                    break;
                }
                self.ends.pop_front();
                done.push(self.queue.pop_front().unwrap());
            }
        }
        done
    }
}

fn finish(d: &Dev, done: Vec<*mut WaveHdr>) {
    for h in done {
        unsafe {
            (*h).flags = ((*h).flags & !WHDR_INQUEUE) | WHDR_DONE;
        }
        notify(d, WOM_DONE, h as usize);
    }
}

unsafe extern "system" fn feeder(p: usize) -> u32 {
    let d = unsafe { &*(p as *const Dev) };
    loop {
        let (done, exit) = {
            let mut s = d.state.lock();
            if s.closing {
                (Vec::new(), true)
            } else {
                (s.pump(), false)
            }
        };
        finish(d, done);
        if exit {
            return 0;
        }
        unsafe { WaitForSingleObject(d.wake, 10) };
    }
}

// -----| the API |-----

#[no_mangle]
pub extern "system" fn waveOutGetNumDevs() -> u32 {
    device_present() as u32
}

fn device_id_ok(id: usize) -> bool {
    device_present() && (id == 0 || id == WAVE_MAPPER || id as u32 == WAVE_MAPPER as u32 || dev(id).is_some())
}

/// WAVEOUTCAPSW / WAVEOUTCAPSA.
unsafe fn caps(id: usize, out: *mut u8, size: u32, wide: bool) -> u32 {
    if !device_id_ok(id) {
        return MMSYSERR_BADDEVICEID;
    }
    if out.is_null() {
        return MMSYSERR_INVALPARAM;
    }
    let name = "Speakers (VEILOS HD Audio)";
    let mut c = [0u8; 84];
    c[0..2].copy_from_slice(&1u16.to_le_bytes()); // MM_MICROSOFT
    c[2..4].copy_from_slice(&100u16.to_le_bytes());
    c[4..8].copy_from_slice(&0x0001_0000u32.to_le_bytes());
    let name_len = if wide { 64 } else { 32 };
    for (i, ch) in name.bytes().take(31).enumerate() {
        if wide {
            c[8 + 2 * i] = ch;
        } else {
            c[8 + i] = ch;
        }
    }
    let rest = 8 + name_len;
    c[rest..rest + 4].copy_from_slice(&0x000F_FFFFu32.to_le_bytes()); // every WAVE_FORMAT_*
    c[rest + 4..rest + 6].copy_from_slice(&2u16.to_le_bytes());
    // WAVECAPS_VOLUME | WAVECAPS_LRVOLUME | WAVECAPS_SAMPLEACCURATE
    c[rest + 8..rest + 12].copy_from_slice(&0x2Cu32.to_le_bytes());
    let total = rest + 12;
    unsafe { core::ptr::copy_nonoverlapping(c.as_ptr(), out, total.min(size as usize)) };
    MMSYSERR_NOERROR
}

#[no_mangle]
pub unsafe extern "system" fn waveOutGetDevCapsW(id: usize, out: *mut u8, size: u32) -> u32 {
    unsafe { caps(id, out, size, true) }
}

#[no_mangle]
pub unsafe extern "system" fn waveOutGetDevCapsA(id: usize, out: *mut u8, size: u32) -> u32 {
    unsafe { caps(id, out, size, false) }
}

#[no_mangle]
pub unsafe extern "system" fn waveOutOpen(out: *mut usize, id: u32, wfx: *const u8, cb: usize, instance: usize, flags: u32) -> u32 {
    if !device_present() || !(id == 0 || id == WAVE_MAPPER as u32) {
        return MMSYSERR_BADDEVICEID;
    }
    if wfx.is_null() {
        return MMSYSERR_INVALPARAM;
    }
    // cbSize says how long a non-PCM WAVEFORMATEX is.
    let tag = unsafe { u16::from_le_bytes([*wfx, *wfx.add(1)]) };
    let len = if tag == 1 { 16 } else { 18 + unsafe { u16::from_le_bytes([*wfx.add(16), *wfx.add(17)]) } as usize };
    let Some(fmt) = (unsafe { Fmt::parse(wfx, len) }) else { return WAVERR_BADFORMAT };
    if flags & WAVE_FORMAT_QUERY != 0 {
        return MMSYSERR_NOERROR;
    }
    let cb_kind = flags & CALLBACK_TYPEMASK;
    if !matches!(cb_kind, CALLBACK_NULL | CALLBACK_WINDOW | CALLBACK_THREAD | CALLBACK_FUNCTION | CALLBACK_EVENT) {
        return MMSYSERR_INVALFLAG;
    }
    if out.is_null() {
        return MMSYSERR_INVALPARAM;
    }
    let Some(stream) = k_open(&fmt) else { return MMSYSERR_NODRIVER };
    let volume = *DEVICE_VOLUME.lock();
    if volume != 0xFFFF_FFFF {
        k_control(stream, 3, volume_value(volume));
    }
    let wake = unsafe { CreateEventW(0, 0, 0, core::ptr::null()) };
    let d = Box::into_raw(Box::new(Dev {
        magic: MAGIC,
        state: Lock::new(State {
            stream,
            fmt,
            queue: VecDeque::new(),
            ends: VecDeque::new(),
            offset: 0,
            loops_left: 0,
            written: 0,
            base: 0,
            paused: false,
            closing: false,
            volume,
            scratch: Vec::new(),
        }),
        wake,
        thread: 0,
        thread_id: 0,
        cb_kind,
        cb,
        instance,
    }));
    let mut tid = 0u32;
    let th = unsafe { CreateThread(0, 0, feeder, d as usize, 0, &mut tid) };
    unsafe {
        (*d).thread = th;
        (*d).thread_id = tid;
    }
    OPEN.lock().push(d as usize);
    unsafe { *out = d as usize };
    notify(unsafe { &*d }, WOM_OPEN, 0);
    MMSYSERR_NOERROR
}

#[no_mangle]
pub extern "system" fn waveOutClose(h: usize) -> u32 {
    let Some(d) = dev(h) else { return MMSYSERR_INVALHANDLE };
    {
        let mut s = d.state.lock();
        if !s.queue.is_empty() {
            return WAVERR_STILLPLAYING;
        }
        s.closing = true;
    }
    OPEN.lock().retain(|&x| x != h);
    unsafe {
        SetEvent(d.wake);
        if GetCurrentThreadId() != d.thread_id {
            WaitForSingleObject(d.thread, 0xFFFF_FFFF);
        }
        CloseHandle(d.thread);
        CloseHandle(d.wake);
        CloseHandle(d.state.lock().stream);
    }
    notify(d, WOM_CLOSE, 0);
    // Left allocated when closed from its own callback: the feeder thread
    // is still on its way out.
    if unsafe { GetCurrentThreadId() } != d.thread_id {
        drop(unsafe { Box::from_raw(h as *mut Dev) });
    }
    MMSYSERR_NOERROR
}

#[no_mangle]
pub unsafe extern "system" fn waveOutPrepareHeader(h: usize, hdr: *mut WaveHdr, size: u32) -> u32 {
    if dev(h).is_none() {
        return MMSYSERR_INVALHANDLE;
    }
    if hdr.is_null() || (size as usize) < core::mem::size_of::<WaveHdr>() {
        return MMSYSERR_INVALPARAM;
    }
    unsafe {
        if (*hdr).flags & WHDR_INQUEUE != 0 {
            return WAVERR_STILLPLAYING;
        }
        (*hdr).flags |= WHDR_PREPARED;
    }
    MMSYSERR_NOERROR
}

#[no_mangle]
pub unsafe extern "system" fn waveOutUnprepareHeader(h: usize, hdr: *mut WaveHdr, size: u32) -> u32 {
    if dev(h).is_none() {
        return MMSYSERR_INVALHANDLE;
    }
    if hdr.is_null() || (size as usize) < core::mem::size_of::<WaveHdr>() {
        return MMSYSERR_INVALPARAM;
    }
    unsafe {
        if (*hdr).flags & WHDR_INQUEUE != 0 {
            return WAVERR_STILLPLAYING;
        }
        (*hdr).flags &= !WHDR_PREPARED;
    }
    MMSYSERR_NOERROR
}

#[no_mangle]
pub unsafe extern "system" fn waveOutWrite(h: usize, hdr: *mut WaveHdr, size: u32) -> u32 {
    let Some(d) = dev(h) else { return MMSYSERR_INVALHANDLE };
    if hdr.is_null() || (size as usize) < core::mem::size_of::<WaveHdr>() {
        return MMSYSERR_INVALPARAM;
    }
    unsafe {
        if (*hdr).flags & WHDR_PREPARED == 0 {
            return WAVERR_UNPREPARED;
        }
        if (*hdr).flags & WHDR_INQUEUE != 0 {
            return WAVERR_STILLPLAYING;
        }
        (*hdr).flags = ((*hdr).flags & !WHDR_DONE) | WHDR_INQUEUE;
    }
    let done = {
        let mut s = d.state.lock();
        s.queue.push_back(hdr);
        s.pump()
    };
    finish(d, done);
    unsafe { SetEvent(d.wake) };
    MMSYSERR_NOERROR
}

#[no_mangle]
pub extern "system" fn waveOutPause(h: usize) -> u32 {
    let Some(d) = dev(h) else { return MMSYSERR_INVALHANDLE };
    let mut s = d.state.lock();
    s.paused = true;
    k_control(s.stream, 0, 0);
    MMSYSERR_NOERROR
}

#[no_mangle]
pub extern "system" fn waveOutRestart(h: usize) -> u32 {
    let Some(d) = dev(h) else { return MMSYSERR_INVALHANDLE };
    {
        let mut s = d.state.lock();
        s.paused = false;
        k_control(s.stream, 1, 0);
    }
    unsafe { SetEvent(d.wake) };
    MMSYSERR_NOERROR
}

/// Stop, give every header back as done, and start the position at 0.
#[no_mangle]
pub extern "system" fn waveOutReset(h: usize) -> u32 {
    let Some(d) = dev(h) else { return MMSYSERR_INVALHANDLE };
    let done: Vec<*mut WaveHdr> = {
        let mut s = d.state.lock();
        k_control(s.stream, 2, 0);
        let consumed = k_info(s.stream).consumed;
        s.written = consumed;
        s.base = consumed;
        s.offset = 0;
        s.loops_left = 0;
        s.ends.clear();
        s.queue.drain(..).collect()
    };
    finish(d, done);
    MMSYSERR_NOERROR
}

/// End the loop being played after its current pass.
#[no_mangle]
pub extern "system" fn waveOutBreakLoop(h: usize) -> u32 {
    let Some(d) = dev(h) else { return MMSYSERR_INVALHANDLE };
    let mut s = d.state.lock();
    if s.loops_left > 1 {
        s.loops_left = 1;
    }
    MMSYSERR_NOERROR
}

/// MMTIME { wType, union }: TIME_MS 1, TIME_SAMPLES 2, TIME_BYTES 4; any
/// other type is answered in bytes, as Windows does.
#[no_mangle]
pub unsafe extern "system" fn waveOutGetPosition(h: usize, t: *mut u32, size: u32) -> u32 {
    let Some(d) = dev(h) else { return MMSYSERR_INVALHANDLE };
    if t.is_null() || size < 12 {
        return MMSYSERR_INVALPARAM;
    }
    let (frames, fmt) = {
        let s = d.state.lock();
        let played = k_info(s.stream).consumed.saturating_sub(s.base);
        (played / s.fmt.out_block() as u64, s.fmt)
    };
    unsafe {
        match *t {
            1 => *t.add(1) = (frames * 1000 / fmt.rate as u64) as u32,
            2 => *t.add(1) = frames as u32,
            _ => {
                *t = 4;
                *t.add(1) = (frames * fmt.block as u64) as u32;
            }
        }
    }
    MMSYSERR_NOERROR
}

#[no_mangle]
pub unsafe extern "system" fn waveOutGetVolume(h: usize, out: *mut u32) -> u32 {
    if out.is_null() {
        return MMSYSERR_INVALPARAM;
    }
    let v = match dev(h) {
        Some(d) => d.state.lock().volume,
        None if device_id_ok(h) => *DEVICE_VOLUME.lock(),
        None => return MMSYSERR_INVALHANDLE,
    };
    unsafe { *out = v };
    MMSYSERR_NOERROR
}

#[no_mangle]
pub extern "system" fn waveOutSetVolume(h: usize, v: u32) -> u32 {
    match dev(h) {
        Some(d) => {
            let mut s = d.state.lock();
            s.volume = v;
            k_control(s.stream, 3, volume_value(v));
        }
        None if device_id_ok(h) => *DEVICE_VOLUME.lock() = v,
        None => return MMSYSERR_INVALHANDLE,
    }
    MMSYSERR_NOERROR
}

#[no_mangle]
pub unsafe extern "system" fn waveOutGetID(h: usize, out: *mut u32) -> u32 {
    if dev(h).is_none() {
        return MMSYSERR_INVALHANDLE;
    }
    if out.is_null() {
        return MMSYSERR_INVALPARAM;
    }
    unsafe { *out = 0 };
    MMSYSERR_NOERROR
}

#[no_mangle]
pub extern "system" fn waveOutGetPitch(h: usize, _out: *mut u32) -> u32 {
    if dev(h).is_none() { MMSYSERR_INVALHANDLE } else { MMSYSERR_NOTSUPPORTED }
}

#[no_mangle]
pub extern "system" fn waveOutSetPitch(h: usize, _v: u32) -> u32 {
    if dev(h).is_none() { MMSYSERR_INVALHANDLE } else { MMSYSERR_NOTSUPPORTED }
}

#[no_mangle]
pub extern "system" fn waveOutGetPlaybackRate(h: usize, _out: *mut u32) -> u32 {
    if dev(h).is_none() { MMSYSERR_INVALHANDLE } else { MMSYSERR_NOTSUPPORTED }
}

#[no_mangle]
pub extern "system" fn waveOutSetPlaybackRate(h: usize, _v: u32) -> u32 {
    if dev(h).is_none() { MMSYSERR_INVALHANDLE } else { MMSYSERR_NOTSUPPORTED }
}

#[no_mangle]
pub extern "system" fn waveOutMessage(_h: usize, _msg: u32, _a: usize, _b: usize) -> u32 {
    MMSYSERR_NOTSUPPORTED
}

fn error_text(e: u32) -> &'static str {
    match e {
        MMSYSERR_NOERROR => "The command was successful.",
        MMSYSERR_ERROR => "Undefined external error.",
        MMSYSERR_BADDEVICEID => "A device ID has been used that is out of range for your system.",
        MMSYSERR_INVALHANDLE => "An invalid handle was passed to a system function.",
        MMSYSERR_NODRIVER => "There is no driver installed on your system.",
        MMSYSERR_NOMEM => "There is not enough memory available for this task.",
        MMSYSERR_NOTSUPPORTED => "This function is not supported.",
        MMSYSERR_INVALFLAG => "An invalid flag was passed to a system function.",
        MMSYSERR_INVALPARAM => "An invalid parameter was passed to a system function.",
        WAVERR_BADFORMAT => "The specified format is not supported or cannot be translated.",
        WAVERR_STILLPLAYING => "Cannot perform this operation while media data is still playing.",
        WAVERR_UNPREPARED => "The wave header was not prepared.",
        _ => "",
    }
}

#[no_mangle]
pub unsafe extern "system" fn waveOutGetErrorTextW(e: u32, out: *mut u16, n: u32) -> u32 {
    let t = error_text(e);
    if t.is_empty() {
        return MMSYSERR_BADERRNUM;
    }
    if out.is_null() || n == 0 {
        return MMSYSERR_INVALPARAM;
    }
    let mut i = 0;
    for b in t.bytes().take(n as usize - 1) {
        unsafe { *out.add(i) = b as u16 };
        i += 1;
    }
    unsafe { *out.add(i) = 0 };
    MMSYSERR_NOERROR
}

#[no_mangle]
pub unsafe extern "system" fn waveOutGetErrorTextA(e: u32, out: *mut u8, n: u32) -> u32 {
    let t = error_text(e);
    if t.is_empty() {
        return MMSYSERR_BADERRNUM;
    }
    if out.is_null() || n == 0 {
        return MMSYSERR_INVALPARAM;
    }
    let mut i = 0;
    for b in t.bytes().take(n as usize - 1) {
        unsafe { *out.add(i) = b };
        i += 1;
    }
    unsafe { *out.add(i) = 0 };
    MMSYSERR_NOERROR
}

const MMSYSERR_BADERRNUM: u32 = 9;
