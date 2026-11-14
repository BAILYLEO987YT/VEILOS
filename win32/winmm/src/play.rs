//! PlaySound and sndPlaySound: one sound at a time per process, from a file,
//! a resource, memory or an alias (the registry's sound scheme). The system
//! aliases fall back to VEILOS's own short tones when the scheme names no
//! file, since Windows' sound files aren't here.

use alloc::boxed::Box;
use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};

use crate::dllrt::Lock;
use crate::fmt::{parse_wav, Fmt};
use crate::wave::{k_info, k_open, k_write};
use crate::*;

const SND_ASYNC: u32 = 0x0001;
const SND_NODEFAULT: u32 = 0x0002;
const SND_MEMORY: u32 = 0x0004;
const SND_LOOP: u32 = 0x0008;
const SND_NOSTOP: u32 = 0x0010;
const SND_PURGE: u32 = 0x0040;
const SND_ALIAS: u32 = 0x0001_0000;
const SND_ALIAS_ID: u32 = 0x0011_0000;
const SND_FILENAME: u32 = 0x0002_0000;
const SND_RESOURCE: u32 = 0x0004_0004;

/// The sound playing now: its stop flag, its thread (0 when synchronous),
/// and whether it has finished.
struct Current {
    stop: &'static AtomicBool,
    done: &'static AtomicBool,
    thread: Handle,
}

static CURRENT: Lock<Option<Current>> = Lock::new(None);

/// Stop what's playing and wait for its thread.
fn stop_current() {
    let cur = CURRENT.lock().take();
    if let Some(c) = cur {
        c.stop.store(true, Ordering::Relaxed);
        if c.thread != 0 {
            unsafe {
                WaitForSingleObject(c.thread, 2000);
                CloseHandle(c.thread);
            }
        } else {
            // A synchronous PlaySound on another thread: it sees the flag.
            let t = unsafe { GetTickCount() };
            while !c.done.load(Ordering::Relaxed) && unsafe { GetTickCount() }.wrapping_sub(t) < 2000 {
                unsafe { Sleep(5) };
            }
        }
    }
}

fn playing() -> bool {
    CURRENT.lock().as_ref().is_some_and(|c| !c.done.load(Ordering::Relaxed))
}

/// Samples ready for the kernel (already converted).
struct Sound {
    fmt: Fmt,
    data: Vec<u8>,
}

/// Play `s` to the end (or forever when `looped`) unless `stop` is set.
fn play(s: &Sound, looped: bool, stop: &AtomicBool) -> bool {
    let Some(h) = k_open(&s.fmt) else { return false };
    let mut written: u64 = 0;
    'outer: loop {
        let mut at = 0;
        while at < s.data.len() {
            if stop.load(Ordering::Relaxed) {
                break 'outer;
            }
            let n = k_write(h, &s.data[at..]);
            at += n;
            written += n as u64;
            if at < s.data.len() {
                unsafe { Sleep(10) };
            }
        }
        if !looped || s.data.is_empty() {
            break;
        }
    }
    while !stop.load(Ordering::Relaxed) && k_info(h).consumed < written {
        unsafe { Sleep(5) };
    }
    unsafe { CloseHandle(h) };
    true
}

struct Job {
    sound: Sound,
    looped: bool,
    stop: &'static AtomicBool,
    done: &'static AtomicBool,
}

unsafe extern "system" fn player(p: usize) -> u32 {
    let job = unsafe { Box::from_raw(p as *mut Job) };
    play(&job.sound, job.looped, job.stop);
    job.done.store(true, Ordering::Relaxed);
    0
}

fn decode(wav: &[u8]) -> Option<Sound> {
    let (fmt, samples) = parse_wav(wav)?;
    let mut data = Vec::with_capacity(samples.len());
    fmt.convert(samples, &mut data);
    let out = Fmt { float: false, channels: fmt.out_channels(), rate: fmt.rate, bits: fmt.out_bits(), block: fmt.out_block() };
    Some(Sound { fmt: out, data })
}

fn read_file(name: &[u16]) -> Option<Vec<u8>> {
    let mut z: Vec<u16> = name.to_vec();
    z.push(0);
    let h = unsafe { CreateFileW(z.as_ptr(), 0x8000_0000, 1, 0, 3, 0, 0) };
    if h == u64::MAX {
        return None;
    }
    let size = unsafe { GetFileSize(h, core::ptr::null_mut()) };
    let mut buf = vec![0u8; size as usize];
    let mut got = 0u32;
    let ok = size != u32::MAX && unsafe { ReadFile(h, buf.as_mut_ptr(), size, &mut got, 0) } != 0;
    unsafe { CloseHandle(h) };
    if !ok {
        return None;
    }
    buf.truncate(got as usize);
    Some(buf)
}

// -----| aliases |-----

/// The system sounds: alias, its key in the sound scheme, and VEILOS's own
/// tone for it (notes in Hz, 0 ending the list).
const SYSTEM: &[(&str, &str, [u32; 3])] = &[
    ("SystemDefault", ".Default", [660, 0, 0]),
    (".Default", ".Default", [660, 0, 0]),
    ("SystemAsterisk", "SystemAsterisk", [880, 1320, 0]),
    ("SystemExclamation", "SystemExclamation", [660, 880, 0]),
    ("SystemHand", "SystemHand", [440, 330, 0]),
    ("SystemQuestion", "SystemQuestion", [660, 990, 0]),
    ("SystemNotification", "SystemNotification", [990, 1320, 0]),
    ("SystemStart", "SystemStart", [523, 659, 784]),
    ("SystemWelcome", "SystemWelcome", [523, 659, 784]),
    ("SystemExit", "SystemExit", [784, 659, 523]),
];

fn eq_ignore_case(a: &[u16], b: &str) -> bool {
    a.len() == b.len() && a.iter().zip(b.bytes()).all(|(&x, y)| {
        let x = if (b'A' as u16..=b'Z' as u16).contains(&x) { x + 32 } else { x };
        x == y.to_ascii_lowercase() as u16
    })
}

/// The scheme's file for an alias: HKCU\AppEvents\Schemes\Apps\.Default\<key>\.Current.
fn scheme_file(key: &str) -> Option<Vec<u16>> {
    let mut sub: Vec<u16> = "AppEvents\\Schemes\\Apps\\.Default\\".encode_utf16().collect();
    sub.extend(key.encode_utf16());
    sub.extend("\\.Current".encode_utf16());
    sub.push(0);
    let mut buf = [0u16; 260];
    let mut size = (buf.len() * 2) as u32;
    // RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ: expanded.
    let e = unsafe { RegGetValueW(0x8000_0001, sub.as_ptr(), core::ptr::null(), 0x6, core::ptr::null_mut(), buf.as_mut_ptr() as *mut u8, &mut size) };
    if e != 0 {
        return None;
    }
    let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    (n > 0).then(|| buf[..n].to_vec())
}

/// A tone: each note 110 ms of a sine with a falling envelope, 48 kHz mono.
fn tone(notes: &[u32]) -> Sound {
    const RATE: u32 = 48_000;
    let per = (RATE / 1000 * 110) as usize;
    let mut data = Vec::new();
    for &hz in notes.iter().take_while(|&&n| n != 0) {
        let mut phase = 0.0f32;
        let step = hz as f32 / RATE as f32;
        for i in 0..per {
            let x = phase * 2.0 - 1.0; // -1..1 over one period
            // A parabola close to a sine: 4x(1-|x|), with its sign.
            let s = 4.0 * x * (1.0 - if x < 0.0 { -x } else { x });
            let attack = (i as f32 / 240.0).min(1.0);
            let decay = 1.0 - i as f32 / per as f32;
            let v = (s * attack * decay * 0.3 * 32767.0) as i16;
            data.extend_from_slice(&(-v).to_le_bytes());
            phase += step;
            if phase >= 1.0 {
                phase -= 1.0;
            }
        }
    }
    Sound { fmt: Fmt { float: false, channels: 1, rate: RATE, bits: 16, block: 2 }, data }
}

/// An alias as a sound: the scheme's file, else our tone for a system alias.
fn alias(name: &[u16]) -> Option<Sound> {
    let sys = SYSTEM.iter().find(|(a, _, _)| eq_ignore_case(name, a));
    let key_owned: Vec<u16>;
    let file = match sys {
        Some((_, key, _)) => scheme_file(key),
        None => {
            key_owned = name.to_vec();
            let k = alloc::string::String::from_utf16_lossy(&key_owned);
            scheme_file(&k)
        }
    };
    if let Some(s) = file.and_then(|f| read_file(&f)).and_then(|w| decode(&w)) {
        return Some(s);
    }
    sys.map(|(_, _, notes)| tone(notes))
}

/// SND_ALIAS_ID: 'S' and a second character.
fn alias_id(id: usize) -> Option<&'static str> {
    if id & 0xFF != b'S' as usize {
        return None;
    }
    Some(match (id >> 8) as u8 {
        b'*' => "SystemAsterisk",
        b'?' => "SystemQuestion",
        b'H' => "SystemHand",
        b'E' => "SystemExit",
        b'S' => "SystemStart",
        b'W' => "SystemWelcome",
        b'!' => "SystemExclamation",
        b'D' => "SystemDefault",
        _ => return None,
    })
}

// -----| PlaySound |-----

unsafe fn find(sound: *const u16, module: Handle, flags: u32) -> Option<Sound> {
    if flags & SND_ALIAS_ID == SND_ALIAS_ID {
        let name: Vec<u16> = alias_id(sound as usize)?.encode_utf16().collect();
        return alias(&name);
    }
    if flags & SND_MEMORY != 0 && flags & SND_RESOURCE != SND_RESOURCE {
        // The image's own RIFF length says how long it is.
        let p = sound as *const u8;
        let head = unsafe { core::slice::from_raw_parts(p, 8) };
        if &head[0..4] != b"RIFF" {
            return None;
        }
        let len = u32::from_le_bytes([head[4], head[5], head[6], head[7]]) as usize + 8;
        return decode(unsafe { core::slice::from_raw_parts(p, len) });
    }
    if flags & SND_RESOURCE == SND_RESOURCE {
        let m = if module == 0 { unsafe { GetModuleHandleW(core::ptr::null()) } } else { module };
        let kind: Vec<u16> = "WAVE\0".encode_utf16().collect();
        let r = unsafe { FindResourceW(m, sound, kind.as_ptr()) };
        if r == 0 {
            return None;
        }
        let p = unsafe { LockResource(LoadResource(m, r)) };
        let n = unsafe { SizeofResource(m, r) } as usize;
        if p.is_null() {
            return None;
        }
        return decode(unsafe { core::slice::from_raw_parts(p, n) });
    }
    let name = unsafe { wstr(sound) };
    if flags & SND_FILENAME != 0 {
        return read_file(name).and_then(|w| decode(&w));
    }
    if flags & SND_ALIAS != 0 {
        return alias(name);
    }
    // Neither said: an alias if there is one, else a file.
    alias(name).or_else(|| read_file(name).and_then(|w| decode(&w)))
}

#[no_mangle]
pub unsafe extern "system" fn PlaySoundW(sound: *const u16, module: Handle, flags: u32) -> Bool {
    if sound.is_null() || flags & SND_PURGE != 0 && flags & (SND_ALIAS | SND_FILENAME | SND_MEMORY) == 0 {
        stop_current();
        return 1;
    }
    if flags & SND_NOSTOP != 0 && playing() {
        return 0;
    }
    let found = unsafe { find(sound, module, flags) };
    let s = match found {
        Some(s) => s,
        None if flags & SND_NODEFAULT == 0 => match alias(&"SystemDefault".encode_utf16().collect::<Vec<u16>>()) {
            Some(s) => s,
            None => return 0,
        },
        None => return 0,
    };
    stop_current();
    if !wave::device_present() {
        return 0;
    }
    let stop: &'static AtomicBool = Box::leak(Box::new(AtomicBool::new(false)));
    let done: &'static AtomicBool = Box::leak(Box::new(AtomicBool::new(false)));
    let looped = flags & SND_LOOP != 0;
    if flags & SND_ASYNC != 0 {
        let job = Box::into_raw(Box::new(Job { sound: s, looped, stop, done }));
        let th = unsafe { CreateThread(0, 0, player, job as usize, 0, core::ptr::null_mut()) };
        *CURRENT.lock() = Some(Current { stop, done, thread: th });
        1
    } else {
        // SND_LOOP needs SND_ASYNC; without it Windows plays the sound once.
        *CURRENT.lock() = Some(Current { stop, done, thread: 0 });
        let ok = play(&s, false, stop);
        done.store(true, Ordering::Relaxed);
        ok as Bool
    }
}

/// The ANSI forms take a name; with SND_RESOURCE it may be a number, with
/// SND_MEMORY or SND_ALIAS_ID it's not a string at all.
unsafe fn ansi_arg(sound: *const u8, flags: u32) -> Option<Vec<u16>> {
    let not_text = sound.is_null()
        || flags & SND_ALIAS_ID == SND_ALIAS_ID
        || flags & SND_MEMORY != 0 && flags & SND_RESOURCE != SND_RESOURCE
        || (sound as usize) >> 16 == 0;
    if not_text { None } else { Some(unsafe { widen(sound) }) }
}

#[no_mangle]
pub unsafe extern "system" fn PlaySoundA(sound: *const u8, module: Handle, flags: u32) -> Bool {
    match unsafe { ansi_arg(sound, flags) } {
        Some(w) => unsafe { PlaySoundW(w.as_ptr(), module, flags) },
        None => unsafe { PlaySoundW(sound as *const u16, module, flags) },
    }
}

#[no_mangle]
pub unsafe extern "system" fn PlaySound(sound: *const u8, module: Handle, flags: u32) -> Bool {
    unsafe { PlaySoundA(sound, module, flags) }
}

/// sndPlaySound's flags are PlaySound's low ones; the name is an alias or
/// a file, never a resource.
#[no_mangle]
pub unsafe extern "system" fn sndPlaySoundW(sound: *const u16, flags: u32) -> Bool {
    unsafe { PlaySoundW(sound, 0, flags & 0xFF) }
}

#[no_mangle]
pub unsafe extern "system" fn sndPlaySoundA(sound: *const u8, flags: u32) -> Bool {
    unsafe { PlaySoundA(sound, 0, flags & 0xFF) }
}
