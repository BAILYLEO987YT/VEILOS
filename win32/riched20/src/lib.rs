//! VEILOS `riched20.dll` (also installed as `msftedit.dll` and `riched32.dll`):
//! rich edit controls as plain edit controls. The classes pass everything to
//! EDIT and answer the rich edit messages over it: text, selection, ranges,
//! finding, streaming in and out as text. Formatting is accepted and not
//! shown (one font, one colour), and RTF is not parsed.

#![no_std]

extern crate alloc;

#[path = "../../dllrt.rs"]
#[allow(dead_code)]
mod dllrt;

use alloc::vec::Vec;
use core::sync::atomic::{AtomicUsize, Ordering};

type Hwnd = u64;

#[link(name = "user32", kind = "raw-dylib")]
unsafe extern "system" {
    fn RegisterClassExW(wc: *const u8) -> u16;
    fn GetClassInfoExW(inst: u64, name: *const u16, wc: *mut u8) -> i32;
    fn CallWindowProcW(proc: usize, hwnd: Hwnd, msg: u32, wp: u64, lp: i64) -> i64;
}

#[link(name = "kernel32", kind = "raw-dylib")]
unsafe extern "system" {
    fn WideCharToMultiByte(cp: u32, flags: u32, s: *const u16, n: i32, out: *mut u8, cap: i32, def: usize, used: usize) -> i32;
    fn MultiByteToWideChar(cp: u32, flags: u32, s: *const u8, n: i32, out: *mut u16, cap: i32) -> i32;
}

const WM_USER: u32 = 0x0400;
const WM_GETTEXT: u32 = 0x000D;
const WM_GETTEXTLENGTH: u32 = 0x000E;
const WM_SETTEXT: u32 = 0x000C;
const EM_GETSEL: u32 = 0x00B0;
const EM_SETSEL: u32 = 0x00B1;
const EM_REPLACESEL: u32 = 0x00C2;
const EM_LINEFROMCHAR: u32 = 0x00C9;
const EM_CANPASTE: u32 = WM_USER + 50;
const EM_EXGETSEL: u32 = WM_USER + 52;
const EM_EXLIMITTEXT: u32 = WM_USER + 53;
const EM_EXLINEFROMCHAR: u32 = WM_USER + 54;
const EM_EXSETSEL: u32 = WM_USER + 55;
const EM_FINDTEXT: u32 = WM_USER + 56;
const EM_GETCHARFORMAT: u32 = WM_USER + 58;
const EM_GETEVENTMASK: u32 = WM_USER + 59;
const EM_GETOLEINTERFACE: u32 = WM_USER + 60;
const EM_GETPARAFORMAT: u32 = WM_USER + 61;
const EM_GETSELTEXT: u32 = WM_USER + 62;
const EM_HIDESELECTION: u32 = WM_USER + 63;
const EM_SELECTIONTYPE: u32 = WM_USER + 66;
const EM_SETBKGNDCOLOR: u32 = WM_USER + 67;
const EM_SETCHARFORMAT: u32 = WM_USER + 68;
const EM_SETEVENTMASK: u32 = WM_USER + 69;
const EM_SETOLECALLBACK: u32 = WM_USER + 70;
const EM_SETPARAFORMAT: u32 = WM_USER + 71;
const EM_SETTARGETDEVICE: u32 = WM_USER + 72;
const EM_STREAMIN: u32 = WM_USER + 73;
const EM_STREAMOUT: u32 = WM_USER + 74;
const EM_GETTEXTRANGE: u32 = WM_USER + 75;
const EM_FINDTEXTEX: u32 = WM_USER + 79;
const EM_SETUNDOLIMIT: u32 = WM_USER + 82;
const EM_REDO: u32 = WM_USER + 84;
const EM_CANREDO: u32 = WM_USER + 85;
const EM_SETOPTIONS: u32 = WM_USER + 77;
const EM_GETOPTIONS: u32 = WM_USER + 78;
const EM_AUTOURLDETECT: u32 = WM_USER + 91;
const EM_GETTEXTEX: u32 = WM_USER + 94;
const EM_GETTEXTLENGTHEX: u32 = WM_USER + 95;
const EM_SETTEXTEX: u32 = WM_USER + 97;
const EM_FINDTEXTW: u32 = WM_USER + 123;
const EM_FINDTEXTEXW: u32 = WM_USER + 124;
const EM_SETTEXTMODE: u32 = WM_USER + 89;
const EM_GETTEXTMODE: u32 = WM_USER + 90;
const EM_SETLANGOPTIONS: u32 = WM_USER + 120;
const EM_GETLANGOPTIONS: u32 = WM_USER + 121;
const EM_SETEDITSTYLE: u32 = WM_USER + 204;
const EM_GETEDITSTYLE: u32 = WM_USER + 205;

const SF_TEXT: u64 = 1;
const SF_UNICODE: u64 = 0x10;
const SFF_SELECTION: u64 = 0x8000;
const FR_DOWN: u64 = 1;
const FR_MATCHCASE: u64 = 4;
const GT_SELECTION: u32 = 2;

static EDIT_PROC: AtomicUsize = AtomicUsize::new(0);

fn edit(hwnd: Hwnd, msg: u32, wp: u64, lp: i64) -> i64 {
    unsafe { CallWindowProcW(EDIT_PROC.load(Ordering::Relaxed), hwnd, msg, wp, lp) }
}

fn all_text(hwnd: Hwnd) -> Vec<u16> {
    let n = edit(hwnd, WM_GETTEXTLENGTH, 0, 0).max(0) as usize;
    let mut v = alloc::vec![0u16; n + 1];
    edit(hwnd, WM_GETTEXT, n as u64 + 1, v.as_mut_ptr() as i64);
    v.truncate(n);
    v
}

fn selection(hwnd: Hwnd) -> (i32, i32) {
    let (mut s, mut e) = (0u32, 0u32);
    edit(hwnd, EM_GETSEL, &mut s as *mut u32 as u64, &mut e as *mut u32 as i64);
    (s as i32, e as i32)
}

fn put(dst: u64, s: &[u16], unicode: bool) -> usize {
    if dst == 0 {
        return 0;
    }
    if unicode {
        unsafe {
            core::ptr::copy_nonoverlapping(s.as_ptr(), dst as *mut u16, s.len());
            *(dst as *mut u16).add(s.len()) = 0;
        }
        s.len()
    } else {
        let n = unsafe { WideCharToMultiByte(0, 0, s.as_ptr(), s.len() as i32, dst as *mut u8, s.len() as i32 * 2 + 1, 0, 0) }.max(0) as usize;
        unsafe { *(dst as *mut u8).add(n) = 0 };
        n
    }
}

fn from_ansi(p: *const u8, n: i32) -> Vec<u16> {
    let k = unsafe { MultiByteToWideChar(0, 0, p, n, core::ptr::null_mut(), 0) };
    if k <= 0 {
        return Vec::new();
    }
    let mut v = alloc::vec![0u16; k as usize];
    unsafe { MultiByteToWideChar(0, 0, p, n, v.as_mut_ptr(), k) };
    if n < 0 {
        v.pop();
    }
    v
}

fn wstr(p: *const u16) -> Vec<u16> {
    if p.is_null() {
        return Vec::new();
    }
    (0..).map(|i| unsafe { *p.add(i) }).take_while(|&c| c != 0).collect()
}

fn lower(c: u16) -> u16 {
    if (b'A' as u16..=b'Z' as u16).contains(&c) { c + 32 } else { c }
}

/// FINDTEXT { CHARRANGE chrg; LPCTSTR lpstrText }, then (Ex) chrgText.
fn find(hwnd: Hwnd, flags: u64, lp: i64, unicode: bool, ex: bool) -> i64 {
    if lp == 0 {
        return -1;
    }
    let p = lp as *mut u8;
    let (min, max, text) = unsafe { (*(p as *const i32), *(p as *const i32).add(1), core::ptr::read_unaligned(p.add(8) as *const u64)) };
    let needle = if unicode { wstr(text as *const u16) } else { from_ansi(text as *const u8, -1) };
    let hay = all_text(hwnd);
    if needle.is_empty() {
        return -1;
    }
    let case = flags & FR_MATCHCASE != 0;
    let at = |i: usize| hay[i..i + needle.len()].iter().zip(&needle).all(|(a, b)| if case { a == b } else { lower(*a) == lower(*b) });
    let end = if max < 0 { hay.len() } else { (max as usize).min(hay.len()) };
    let start = (min.max(0) as usize).min(hay.len());
    let hit = if flags & FR_DOWN != 0 {
        (start..end.saturating_sub(needle.len() - 1)).find(|&i| at(i))
    } else {
        // Up, from `min` towards `max` (the range is given high to low).
        let (lo, hi) = (end.min(start), start.max(end));
        (lo..hi.saturating_sub(needle.len() - 1)).rev().find(|&i| at(i))
    };
    let Some(h) = hit else { return -1 };
    if ex {
        unsafe {
            *(p.add(16) as *mut i32) = h as i32;
            *(p.add(20) as *mut i32) = (h + needle.len()) as i32;
        }
    }
    h as i64
}

/// EDITSTREAM { dwCookie, dwError, pfnCallback } (pack 4: the callback is at
/// 12): EDITSTREAMCALLBACK(cookie, buffer, cb, *pcb).
fn stream_args(es: *mut u8) -> (u64, u64) {
    unsafe { (core::ptr::read_unaligned(es as *const u64), core::ptr::read_unaligned(es.add(12) as *const u64)) }
}

fn stream_in(hwnd: Hwnd, format: u64, lp: i64) -> i64 {
    let es = lp as *mut u8;
    if es.is_null() {
        return 0;
    }
    let (cookie, cb) = stream_args(es);
    let f: unsafe extern "system" fn(u64, *mut u8, i32, *mut i32) -> u32 = unsafe { core::mem::transmute(cb) };
    let mut bytes: Vec<u8> = Vec::new();
    let mut chunk = alloc::vec![0u8; 4096];
    loop {
        let mut got = 0i32;
        let e = unsafe { f(cookie, chunk.as_mut_ptr(), 4096, &mut got) };
        if e != 0 {
            unsafe { core::ptr::write_unaligned(es.add(8) as *mut u32, e) };
            break;
        }
        if got <= 0 {
            break;
        }
        bytes.extend_from_slice(&chunk[..got as usize]);
    }
    let text: Vec<u16> = if format & SF_UNICODE != 0 {
        bytes.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect()
    } else if bytes.is_empty() {
        Vec::new()
    } else {
        from_ansi(bytes.as_ptr(), bytes.len() as i32)
    };
    let mut z = text.clone();
    z.push(0);
    if format & SFF_SELECTION != 0 {
        edit(hwnd, EM_REPLACESEL, 0, z.as_ptr() as i64);
    } else {
        edit(hwnd, WM_SETTEXT, 0, z.as_ptr() as i64);
    }
    text.len() as i64
}

fn stream_out(hwnd: Hwnd, format: u64, lp: i64) -> i64 {
    let es = lp as *mut u8;
    if es.is_null() {
        return 0;
    }
    let (cookie, cb) = stream_args(es);
    let f: unsafe extern "system" fn(u64, *mut u8, i32, *mut i32) -> u32 = unsafe { core::mem::transmute(cb) };
    let mut text = all_text(hwnd);
    if format & SFF_SELECTION != 0 {
        let (s, e) = selection(hwnd);
        text = text[(s as usize).min(text.len())..(e as usize).min(text.len())].to_vec();
    }
    let mut bytes: Vec<u8> = if format & SF_UNICODE != 0 {
        text.iter().flat_map(|c| c.to_le_bytes()).collect()
    } else {
        let mut b = alloc::vec![0u8; text.len() * 2 + 1];
        let n = put(b.as_mut_ptr() as u64, &text, false);
        b.truncate(n);
        b
    };
    let total = bytes.len();
    let mut at = 0;
    while at < total {
        let mut wrote = 0i32;
        let e = unsafe { f(cookie, bytes[at..].as_mut_ptr(), (total - at).min(4096) as i32, &mut wrote) };
        if e != 0 {
            unsafe { core::ptr::write_unaligned(es.add(8) as *mut u32, e) };
            break;
        }
        if wrote <= 0 {
            break;
        }
        at += wrote as usize;
    }
    let _ = SF_TEXT;
    (at / if format & SF_UNICODE != 0 { 2 } else { 1 }) as i64
}

unsafe extern "system" fn rich_proc(hwnd: Hwnd, msg: u32, wp: u64, lp: i64) -> i64 {
    match msg {
        // CHARRANGE { min, max }
        EM_EXGETSEL => {
            if lp != 0 {
                let (s, e) = selection(hwnd);
                unsafe { *(lp as *mut [i32; 2]) = [s, e] };
            }
            0
        }
        EM_EXSETSEL => {
            if lp == 0 {
                return 0;
            }
            let [s, e] = unsafe { *(lp as *const [i32; 2]) };
            edit(hwnd, EM_SETSEL, s as i64 as u64, e as i64);
            selection(hwnd).1 as i64
        }
        EM_EXLIMITTEXT => {
            edit(hwnd, 0x00C5, lp as u64, 0);
            0
        }
        EM_EXLINEFROMCHAR => edit(hwnd, EM_LINEFROMCHAR, lp as u64, 0),
        // TEXTRANGE { CHARRANGE chrg; LPTSTR lpstrText }
        EM_GETTEXTRANGE => {
            if lp == 0 {
                return 0;
            }
            let p = lp as *const u8;
            let (min, max, dst) = unsafe { (*(p as *const i32), *(p as *const i32).add(1), core::ptr::read_unaligned(p.add(8) as *const u64)) };
            let t = all_text(hwnd);
            let lo = (min.max(0) as usize).min(t.len());
            let hi = if max < 0 { t.len() } else { (max as usize).min(t.len()) };
            put(dst, &t[lo..hi.max(lo)], true) as i64
        }
        EM_GETSELTEXT => {
            let (s, e) = selection(hwnd);
            let t = all_text(hwnd);
            put(lp as u64, &t[(s as usize).min(t.len())..(e as usize).min(t.len())], true) as i64
        }
        EM_FINDTEXTW => find(hwnd, wp, lp, true, false),
        EM_FINDTEXTEXW => find(hwnd, wp, lp, true, true),
        EM_FINDTEXT => find(hwnd, wp, lp, false, false),
        EM_FINDTEXTEX => find(hwnd, wp, lp, false, true),
        EM_STREAMIN => stream_in(hwnd, wp, lp),
        EM_STREAMOUT => stream_out(hwnd, wp, lp),
        // GETTEXTEX { cb, flags, codepage, lpDefaultChar, lpUsedDefChar }
        EM_GETTEXTEX => {
            if wp == 0 {
                return 0;
            }
            let p = wp as *const u32;
            let (cb, flags, cp) = unsafe { (*p, *p.add(1), *p.add(2)) };
            let mut t = all_text(hwnd);
            if flags & GT_SELECTION != 0 {
                let (s, e) = selection(hwnd);
                t = t[(s as usize).min(t.len())..(e as usize).min(t.len())].to_vec();
            }
            if cp == 1200 {
                t.truncate((cb as usize / 2).saturating_sub(1));
                put(lp as u64, &t, true) as i64
            } else {
                t.truncate((cb as usize).saturating_sub(1));
                put(lp as u64, &t, false) as i64
            }
        }
        // GETTEXTLENGTHEX { flags, codepage }
        EM_GETTEXTLENGTHEX => all_text(hwnd).len() as i64,
        // SETTEXTEX { flags (ST_SELECTION 2), codepage }
        EM_SETTEXTEX => {
            if wp == 0 {
                return 0;
            }
            let (flags, cp) = unsafe { (*(wp as *const u32), *(wp as *const u32).add(1)) };
            let mut t = if cp == 1200 { wstr(lp as *const u16) } else { from_ansi(lp as *const u8, -1) };
            let n = t.len();
            t.push(0);
            if flags & 2 != 0 {
                edit(hwnd, EM_REPLACESEL, 0, t.as_ptr() as i64);
            } else {
                edit(hwnd, WM_SETTEXT, 0, t.as_ptr() as i64);
            }
            let _ = n;
            1
        }
        // Formats are accepted and not shown. A CHARFORMAT asked for reports
        // nothing beyond what the caller zeroed.
        EM_SETCHARFORMAT | EM_SETPARAFORMAT => 1,
        EM_GETCHARFORMAT | EM_GETPARAFORMAT => 0,
        EM_SETBKGNDCOLOR => 0x00FF_FFFF,
        EM_SETTARGETDEVICE | EM_SETUNDOLIMIT | EM_SETOLECALLBACK | EM_HIDESELECTION | EM_AUTOURLDETECT | EM_SETLANGOPTIONS | EM_SETEDITSTYLE => 1,
        EM_GETLANGOPTIONS | EM_GETEDITSTYLE | EM_GETOLEINTERFACE => 0,
        EM_SETEVENTMASK | EM_SETOPTIONS | EM_GETEVENTMASK | EM_GETOPTIONS | EM_SETTEXTMODE => 0,
        // TM_PLAINTEXT | TM_MULTILEVELUNDO
        EM_GETTEXTMODE => 1 | 8,
        EM_CANPASTE => 1,
        EM_CANREDO | EM_REDO => 0,
        // SEL_EMPTY 0, SEL_TEXT 1, with SEL_MULTICHAR 4 for more than one.
        EM_SELECTIONTYPE => {
            let (s, e) = selection(hwnd);
            match e - s {
                0 => 0,
                1 => 1,
                _ => 1 | 4,
            }
        }
        // Rich edit controls take Tab and Return.
        0x0087 => edit(hwnd, msg, wp, lp) | 0x2 | 0x4,
        _ => edit(hwnd, msg, wp, lp),
    }
}

#[no_mangle]
pub extern "system" fn DllMain(_module: usize, reason: u32, _reserved: usize) -> i32 {
    if reason == 1 {
        // WNDCLASSEXW: size, style, proc, extra, instance, icon, cursor,
        // brush, menu, name, small icon.
        let mut wc = [0u8; 80];
        wc[0..4].copy_from_slice(&80u32.to_le_bytes());
        let edit_name: Vec<u16> = "EDIT\0".encode_utf16().collect();
        if unsafe { GetClassInfoExW(0, edit_name.as_ptr(), wc.as_mut_ptr()) } == 0 {
            return 1;
        }
        EDIT_PROC.store(usize::from_le_bytes(wc[8..16].try_into().unwrap()), Ordering::Relaxed);
        wc[4..8].copy_from_slice(&0x400Bu32.to_le_bytes()); // CS_GLOBALCLASS | ...
        wc[8..16].copy_from_slice(&(rich_proc as *const () as u64).to_le_bytes());
        for name in ["RichEdit20W\0", "RichEdit20A\0", "RICHEDIT50W\0", "RichEdit\0"] {
            let n: Vec<u16> = name.encode_utf16().collect();
            wc[64..72].copy_from_slice(&(n.as_ptr() as u64).to_le_bytes());
            unsafe { RegisterClassExW(wc.as_ptr()) };
        }
    }
    1
}

/// riched20's only named export besides the classes.
#[no_mangle]
pub extern "system" fn RichEditANSIWndProc(hwnd: Hwnd, msg: u32, wp: u64, lp: i64) -> i64 {
    unsafe { rich_proc(hwnd, msg, wp, lp) }
}
