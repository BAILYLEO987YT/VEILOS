//! user32's character and string helpers: CharNext/Prev/Upper/Lower,
//! IsChar*, and wsprintf/wvsprintf (Windows' own small printf: no floating
//! point, at most 1024 characters).

use alloc::vec::Vec;

use crate::Bool;

fn up(c: u16) -> u16 {
    match c {
        0x61..=0x7A | 0xE0..=0xFE if c != 0xF7 => c - 32,
        _ => c,
    }
}

fn low(c: u16) -> u16 {
    match c {
        0x41..=0x5A | 0xC0..=0xDE if c != 0xD7 => c + 32,
        _ => c,
    }
}

/// A string, or (high word zero) one character, to convert in place.
unsafe fn convert_w(p: *mut u16, f: fn(u16) -> u16) -> *mut u16 {
    if (p as usize) >> 16 == 0 {
        return f(p as usize as u16) as usize as *mut u16;
    }
    let mut q = p;
    unsafe {
        while *q != 0 {
            *q = f(*q);
            q = q.add(1);
        }
    }
    p
}

unsafe fn convert_a(p: *mut u8, f: fn(u16) -> u16) -> *mut u8 {
    if (p as usize) >> 16 == 0 {
        return f(p as usize as u8 as u16) as u8 as usize as *mut u8;
    }
    let mut q = p;
    unsafe {
        while *q != 0 {
            if *q < 0x80 {
                *q = f(*q as u16) as u8;
            }
            q = q.add(1);
        }
    }
    p
}

#[no_mangle]
pub unsafe extern "system" fn CharUpperW(p: *mut u16) -> *mut u16 {
    unsafe { convert_w(p, up) }
}

#[no_mangle]
pub unsafe extern "system" fn CharLowerW(p: *mut u16) -> *mut u16 {
    unsafe { convert_w(p, low) }
}

#[no_mangle]
pub unsafe extern "system" fn CharUpperA(p: *mut u8) -> *mut u8 {
    unsafe { convert_a(p, up) }
}

#[no_mangle]
pub unsafe extern "system" fn CharLowerA(p: *mut u8) -> *mut u8 {
    unsafe { convert_a(p, low) }
}

#[no_mangle]
pub unsafe extern "system" fn CharUpperBuffW(p: *mut u16, n: u32) -> u32 {
    for i in 0..n as usize {
        unsafe { *p.add(i) = up(*p.add(i)) };
    }
    n
}

#[no_mangle]
pub unsafe extern "system" fn CharLowerBuffW(p: *mut u16, n: u32) -> u32 {
    for i in 0..n as usize {
        unsafe { *p.add(i) = low(*p.add(i)) };
    }
    n
}

#[no_mangle]
pub unsafe extern "system" fn CharUpperBuffA(p: *mut u8, n: u32) -> u32 {
    for i in 0..n as usize {
        unsafe { *p.add(i) = up(*p.add(i) as u16) as u8 };
    }
    n
}

#[no_mangle]
pub unsafe extern "system" fn CharLowerBuffA(p: *mut u8, n: u32) -> u32 {
    for i in 0..n as usize {
        unsafe { *p.add(i) = low(*p.add(i) as u16) as u8 };
    }
    n
}

/// The next character (surrogate pairs stay together); the NUL stays.
#[no_mangle]
pub unsafe extern "system" fn CharNextW(p: *const u16) -> *const u16 {
    unsafe {
        if *p == 0 {
            return p;
        }
        if (0xD800..0xDC00).contains(&*p) && (0xDC00..0xE000).contains(&*p.add(1)) {
            return p.add(2);
        }
        p.add(1)
    }
}

/// The next character of UTF-8 text.
#[no_mangle]
pub unsafe extern "system" fn CharNextA(p: *const u8) -> *const u8 {
    unsafe {
        if *p == 0 {
            return p;
        }
        let mut q = p.add(1);
        while *q & 0xC0 == 0x80 {
            q = q.add(1);
        }
        q
    }
}

#[no_mangle]
pub unsafe extern "system" fn CharPrevW(start: *const u16, p: *const u16) -> *const u16 {
    if p <= start {
        return start;
    }
    unsafe {
        let q = p.sub(1);
        if q > start && (0xDC00..0xE000).contains(&*q) && (0xD800..0xDC00).contains(&*q.sub(1)) {
            return q.sub(1);
        }
        q
    }
}

#[no_mangle]
pub unsafe extern "system" fn CharPrevA(start: *const u8, p: *const u8) -> *const u8 {
    if p <= start {
        return start;
    }
    unsafe {
        let mut q = p.sub(1);
        while q > start && *q & 0xC0 == 0x80 {
            q = q.sub(1);
        }
        q
    }
}

fn alpha(c: u16) -> bool {
    up(c) != low(c) || (0x41..=0x5A).contains(&c) || (0x61..=0x7A).contains(&c)
}

#[no_mangle]
pub extern "system" fn IsCharAlphaW(c: u16) -> Bool {
    alpha(c) as Bool
}

#[no_mangle]
pub extern "system" fn IsCharAlphaA(c: u8) -> Bool {
    alpha(c as u16) as Bool
}

#[no_mangle]
pub extern "system" fn IsCharAlphaNumericW(c: u16) -> Bool {
    (alpha(c) || (0x30..=0x39).contains(&c)) as Bool
}

#[no_mangle]
pub extern "system" fn IsCharAlphaNumericA(c: u8) -> Bool {
    IsCharAlphaNumericW(c as u16)
}

#[no_mangle]
pub extern "system" fn IsCharUpperW(c: u16) -> Bool {
    (alpha(c) && low(c) != c) as Bool
}

#[no_mangle]
pub extern "system" fn IsCharUpperA(c: u8) -> Bool {
    IsCharUpperW(c as u16)
}

#[no_mangle]
pub extern "system" fn IsCharLowerW(c: u16) -> Bool {
    (alpha(c) && up(c) != c) as Bool
}

#[no_mangle]
pub extern "system" fn IsCharLowerA(c: u8) -> Bool {
    IsCharLowerW(c as u16)
}

// -----| wsprintf |-----

/// The arguments after the format: a pointer walking them, 8 bytes each
/// (the x64 va_list).
struct Args(*const u64);

impl Args {
    fn next(&mut self) -> u64 {
        let v = unsafe { *self.0 };
        self.0 = unsafe { self.0.add(1) };
        v
    }
}

/// Format into units (wide or narrow source strings for %s per `wide`).
fn format(fmt: &[u16], args: &mut Args, wide: bool) -> Vec<u16> {
    let mut out: Vec<u16> = Vec::new();
    let mut i = 0;
    while i < fmt.len() && out.len() < 1024 {
        let c = fmt[i];
        i += 1;
        if c != b'%' as u16 {
            out.push(c);
            continue;
        }
        let (mut left, mut zero, mut alt) = (false, false, false);
        while i < fmt.len() {
            match fmt[i] {
                0x2D => left = true,
                0x30 => zero = true,
                0x23 => alt = true,
                _ => break,
            }
            i += 1;
        }
        let mut width = 0usize;
        while i < fmt.len() && (0x30..=0x39).contains(&fmt[i]) {
            width = width * 10 + (fmt[i] - 0x30) as usize;
            i += 1;
        }
        let mut prec: Option<usize> = None;
        if i < fmt.len() && fmt[i] == b'.' as u16 {
            i += 1;
            let mut p = 0;
            while i < fmt.len() && (0x30..=0x39).contains(&fmt[i]) {
                p = p * 10 + (fmt[i] - 0x30) as usize;
                i += 1;
            }
            prec = Some(p);
        }
        // Size: l (long, or wide for s/c), h (narrow for s/c), I64.
        let (mut long, mut short, mut wide64) = (false, false, false);
        loop {
            match fmt.get(i) {
                Some(0x6C) => long = true,
                Some(0x68) => short = true,
                Some(0x49) if fmt.get(i + 1) == Some(&0x36) && fmt.get(i + 2) == Some(&0x34) => {
                    wide64 = true;
                    i += 2;
                }
                _ => break,
            }
            i += 1;
        }
        let Some(&conv) = fmt.get(i) else { break };
        i += 1;
        let mut field: Vec<u16> = Vec::new();
        let mut numeric = true;
        match conv as u8 {
            b'%' => {
                out.push(conv);
                continue;
            }
            b'd' | b'i' | b'u' | b'x' | b'X' => {
                let raw = args.next();
                let (neg, mag) = match conv as u8 {
                    b'd' | b'i' => {
                        let v = if wide64 { raw as i64 } else { raw as u32 as i32 as i64 };
                        (v < 0, v.unsigned_abs())
                    }
                    _ => (false, if wide64 { raw } else { raw as u32 as u64 }),
                };
                let base = if matches!(conv as u8, b'x' | b'X') { 16 } else { 10 };
                let digits = if conv == b'X' as u16 { b"0123456789ABCDEF" } else { b"0123456789abcdef" };
                let mut tmp = Vec::new();
                let mut m = mag;
                loop {
                    tmp.push(digits[(m % base) as usize] as u16);
                    m /= base;
                    if m == 0 {
                        break;
                    }
                }
                while tmp.len() < prec.unwrap_or(0) {
                    tmp.push(b'0' as u16);
                }
                if alt && base == 16 && mag != 0 {
                    tmp.push(conv);
                    tmp.push(b'0' as u16);
                }
                if neg {
                    tmp.push(b'-' as u16);
                }
                tmp.reverse();
                field = tmp;
                let _ = long;
            }
            b'c' | b'C' => {
                numeric = false;
                field.push(args.next() as u16);
            }
            b's' | b'S' => {
                numeric = false;
                let p = args.next();
                // %s is the call's own kind; %S, %hs, %ls name the other.
                let w = if short { false } else if long { true } else if conv == b'S' as u16 { !wide } else { wide };
                let text: Vec<u16> = if p == 0 {
                    "(null)".encode_utf16().collect()
                } else if w {
                    unsafe { crate::wstr(p as *const u16) }
                } else {
                    unsafe { crate::from_ansi(p as *const u8, -1) }
                };
                field = match prec {
                    Some(n) => text[..n.min(text.len())].to_vec(),
                    None => text,
                };
            }
            _ => {
                out.push(conv);
                continue;
            }
        }
        let pad = width.saturating_sub(field.len());
        if left {
            out.extend(field);
            out.extend(core::iter::repeat_n(b' ' as u16, pad));
        } else if zero && numeric {
            let sign = field.first().is_some_and(|&c| c == b'-' as u16);
            if sign {
                out.push(field.remove(0));
            }
            out.extend(core::iter::repeat_n(b'0' as u16, pad));
            out.extend(field);
        } else {
            out.extend(core::iter::repeat_n(b' ' as u16, pad));
            out.extend(field);
        }
    }
    out.truncate(1023);
    out
}

#[no_mangle]
pub unsafe extern "system" fn wvsprintfW(out: *mut u16, fmt: *const u16, args: *const u64) -> i32 {
    let f = unsafe { crate::wstr(fmt) };
    let r = format(&f, &mut Args(args), true);
    unsafe {
        core::ptr::copy_nonoverlapping(r.as_ptr(), out, r.len());
        *out.add(r.len()) = 0;
    }
    r.len() as i32
}

#[no_mangle]
pub unsafe extern "system" fn wvsprintfA(out: *mut u8, fmt: *const u8, args: *const u64) -> i32 {
    let f = unsafe { crate::from_ansi(fmt, -1) };
    let r = crate::to_ansi(&format(&f, &mut Args(args), false));
    let n = r.len().min(1023);
    unsafe {
        core::ptr::copy_nonoverlapping(r.as_ptr(), out, n);
        *out.add(n) = 0;
    }
    n as i32
}

/// wsprintf(out, fmt, ...): the register arguments go to their home
/// slots above the return address, so they and the stack ones make one
/// list for wvsprintf.
#[unsafe(naked)]
#[no_mangle]
pub unsafe extern "C" fn wsprintfW() {
    core::arch::naked_asm!(
        "mov [rsp + 8], rcx",
        "mov [rsp + 16], rdx",
        "mov [rsp + 24], r8",
        "mov [rsp + 32], r9",
        "lea r8, [rsp + 24]",
        "jmp {f}",
        f = sym wvsprintfW,
    )
}

#[unsafe(naked)]
#[no_mangle]
pub unsafe extern "C" fn wsprintfA() {
    core::arch::naked_asm!(
        "mov [rsp + 8], rcx",
        "mov [rsp + 16], rdx",
        "mov [rsp + 24], r8",
        "mov [rsp + 32], r9",
        "lea r8, [rsp + 24]",
        "jmp {f}",
        f = sym wvsprintfA,
    )
}
