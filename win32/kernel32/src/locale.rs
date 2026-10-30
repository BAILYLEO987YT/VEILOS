//! Code pages, the locale (en-US), dates and times as text, and string
//! comparison (W7).
//!
//! The ANSI and OEM code pages are UTF-8 (65001), like the console's; 1252,
//! 437 and the ISO Latin-1 pages convert byte for byte.

use crate::time::SystemTime;
use crate::util::wstr;
use crate::Bool;

const ERROR_INSUFFICIENT_BUFFER: u32 = 122;
const ERROR_INVALID_PARAMETER: u32 = 87;
const CP_UTF8: u32 = 65001;
const LOCALE_EN_US: u32 = 0x409;

/// Code pages handled as UTF-8 (the "ANSI", "OEM", thread and UTF-8 pages).
fn is_utf8(cp: u32) -> bool {
    matches!(cp, 0 | 1 | 2 | 3 | CP_UTF8)
}

#[no_mangle]
pub extern "system" fn GetACP() -> u32 {
    CP_UTF8
}

#[no_mangle]
pub extern "system" fn GetOEMCP() -> u32 {
    CP_UTF8
}

#[no_mangle]
pub extern "system" fn IsValidCodePage(cp: u32) -> Bool {
    (is_utf8(cp) || matches!(cp, 437 | 850 | 1252 | 28591 | 20127 | 1200)) as Bool
}

/// CPINFO: MaxCharSize, DefaultChar[2], LeadByte[12].
#[no_mangle]
pub unsafe extern "system" fn GetCPInfo(cp: u32, info: *mut u8) -> Bool {
    if info.is_null() || IsValidCodePage(cp) == 0 {
        crate::SetLastError(ERROR_INVALID_PARAMETER);
        return 0;
    }
    unsafe {
        core::ptr::write_bytes(info, 0, 20);
        *(info as *mut u32) = if is_utf8(cp) { 4 } else { 1 };
        *info.add(4) = b'?';
    }
    1
}

/// CPINFOEXW: CPINFO, then UnicodeDefaultChar, CodePage, CodePageName[260].
#[no_mangle]
pub unsafe extern "system" fn GetCPInfoExW(cp: u32, _flags: u32, info: *mut u8) -> Bool {
    if unsafe { GetCPInfo(cp, info) } == 0 {
        return 0;
    }
    unsafe {
        core::ptr::write_bytes(info.add(20), 0, 4 + 520);
        *(info.add(20) as *mut u16) = b'?' as u16;
        *(info.add(24) as *mut u32) = if is_utf8(cp) { CP_UTF8 } else { cp };
    }
    1
}

/// Decode `src` (bytes of code page `cp`) into UTF-16, calling `out` per unit.
fn decode(cp: u32, src: &[u8], mut out: impl FnMut(u16)) {
    if !is_utf8(cp) {
        for &b in src {
            out(b as u16);
        }
        return;
    }
    let mut i = 0;
    while i < src.len() {
        let b = src[i] as u32;
        let (len, init) = match b {
            0..=0x7F => (1, b),
            0xC2..=0xDF => (2, b & 0x1F),
            0xE0..=0xEF => (3, b & 0x0F),
            0xF0..=0xF4 => (4, b & 0x07),
            _ => (0, 0),
        };
        if len == 0 || i + len > src.len() || src[i + 1..i + len].iter().any(|&c| c & 0xC0 != 0x80) {
            out(0xFFFD);
            i += 1;
            continue;
        }
        let mut c = init;
        for &x in &src[i + 1..i + len] {
            c = (c << 6) | (x as u32 & 0x3F);
        }
        if c >= 0x10000 {
            let c = c - 0x10000;
            out(0xD800 | (c >> 10) as u16);
            out(0xDC00 | (c & 0x3FF) as u16);
        } else {
            out(c as u16);
        }
        i += len;
    }
}

/// Encode UTF-16 `src` into code page `cp`, calling `out` per byte.
/// Returns true if a character had no mapping.
fn encode(cp: u32, src: &[u16], mut out: impl FnMut(u8)) -> bool {
    let mut lost = false;
    let mut i = 0;
    while i < src.len() {
        let mut c = src[i] as u32;
        i += 1;
        if (0xD800..0xDC00).contains(&c) && i < src.len() && (0xDC00..0xE000).contains(&src[i]) {
            c = 0x10000 + ((c - 0xD800) << 10) + (src[i] as u32 - 0xDC00);
            i += 1;
        }
        if !is_utf8(cp) {
            if c < 0x100 {
                out(c as u8);
            } else {
                out(b'?');
                lost = true;
            }
            continue;
        }
        match c {
            0..=0x7F => out(c as u8),
            0x80..=0x7FF => {
                out(0xC0 | (c >> 6) as u8);
                out(0x80 | (c & 0x3F) as u8);
            }
            0x800..=0xFFFF => {
                out(0xE0 | (c >> 12) as u8);
                out(0x80 | ((c >> 6) & 0x3F) as u8);
                out(0x80 | (c & 0x3F) as u8);
            }
            _ => {
                out(0xF0 | (c >> 18) as u8);
                out(0x80 | ((c >> 12) & 0x3F) as u8);
                out(0x80 | ((c >> 6) & 0x3F) as u8);
                out(0x80 | (c & 0x3F) as u8);
            }
        }
    }
    lost
}

/// One UTF-16 unit as UTF-8 (a lone surrogate encodes as itself).
pub(crate) fn encode_one(u: u16, out: &mut [u8; 4]) -> usize {
    let mut n = 0;
    encode(CP_UTF8, &[u], |b| {
        out[n] = b;
        n += 1;
    });
    n
}

#[no_mangle]
pub unsafe extern "system" fn MultiByteToWideChar(cp: u32, _flags: u32, src: *const u8, len: i32, dst: *mut u16, cap: i32) -> i32 {
    if src.is_null() || len == 0 || cap < 0 {
        crate::SetLastError(ERROR_INVALID_PARAMETER);
        return 0;
    }
    // -1: the string and its terminator.
    let n = if len < 0 { (unsafe { crate::util::alen(src) }) + 1 } else { len as usize };
    let bytes = unsafe { core::slice::from_raw_parts(src, n) };
    let mut count = 0usize;
    let mut overflow = false;
    decode(cp, bytes, |u| {
        if cap > 0 {
            if count < cap as usize {
                unsafe { *dst.add(count) = u };
            } else {
                overflow = true;
            }
        }
        count += 1;
    });
    if overflow {
        crate::SetLastError(ERROR_INSUFFICIENT_BUFFER);
        return 0;
    }
    count as i32
}

#[no_mangle]
pub unsafe extern "system" fn WideCharToMultiByte(
    cp: u32,
    _flags: u32,
    src: *const u16,
    len: i32,
    dst: *mut u8,
    cap: i32,
    _default: *const u8,
    used_default: *mut Bool,
) -> i32 {
    if src.is_null() || len == 0 || cap < 0 {
        crate::SetLastError(ERROR_INVALID_PARAMETER);
        return 0;
    }
    let n = if len < 0 { (unsafe { crate::util::wlen(src) }) + 1 } else { len as usize };
    let units = unsafe { core::slice::from_raw_parts(src, n) };
    let mut count = 0usize;
    let mut overflow = false;
    let lost = encode(cp, units, |b| {
        if cap > 0 {
            if count < cap as usize {
                unsafe { *dst.add(count) = b };
            } else {
                overflow = true;
            }
        }
        count += 1;
    });
    if !used_default.is_null() {
        unsafe { *used_default = lost as Bool };
    }
    if overflow {
        crate::SetLastError(ERROR_INSUFFICIENT_BUFFER);
        return 0;
    }
    count as i32
}

// -----| the locale |-----

#[no_mangle]
pub extern "system" fn GetUserDefaultLCID() -> u32 {
    LOCALE_EN_US
}

#[no_mangle]
pub extern "system" fn GetSystemDefaultLCID() -> u32 {
    LOCALE_EN_US
}

#[no_mangle]
pub extern "system" fn GetUserDefaultLangID() -> u16 {
    LOCALE_EN_US as u16
}

#[no_mangle]
pub extern "system" fn GetSystemDefaultLangID() -> u16 {
    LOCALE_EN_US as u16
}

#[no_mangle]
pub extern "system" fn GetUserDefaultUILanguage() -> u16 {
    LOCALE_EN_US as u16
}

#[no_mangle]
pub extern "system" fn GetSystemDefaultUILanguage() -> u16 {
    LOCALE_EN_US as u16
}

#[no_mangle]
pub extern "system" fn GetThreadLocale() -> u32 {
    LOCALE_EN_US
}

#[no_mangle]
pub extern "system" fn SetThreadLocale(_lcid: u32) -> Bool {
    1
}

#[no_mangle]
pub extern "system" fn GetThreadUILanguage() -> u16 {
    LOCALE_EN_US as u16
}

#[no_mangle]
pub extern "system" fn SetThreadUILanguage(_lang: u16) -> u16 {
    LOCALE_EN_US as u16
}

#[no_mangle]
pub extern "system" fn IsValidLocale(lcid: u32, _flags: u32) -> Bool {
    (lcid == LOCALE_EN_US || lcid == 0x400 || lcid == 0x800 || lcid == 0) as Bool
}

const DAYS: [&str; 7] = ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"];
const MONTHS: [&str; 12] = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];

/// en-US's value for LCTYPE `t` (flags removed).
fn locale_value(t: u32) -> Option<&'static str> {
    Some(match t {
        0x01 => "0409",                     // LOCALE_ILANGUAGE
        0x02 => "English (United States)",  // LOCALE_SLANGUAGE
        0x03 => "ENU",                      // LOCALE_SABBREVLANGNAME
        0x04 => "English",                  // LOCALE_SNATIVELANGNAME
        0x05 => "1",                        // LOCALE_ICOUNTRY
        0x06 => "United States",            // LOCALE_SCOUNTRY
        0x07 => "USA",                      // LOCALE_SABBREVCTRYNAME
        0x0B => "437",                      // LOCALE_IDEFAULTCODEPAGE
        0x0C => ",",                        // LOCALE_SLIST
        0x0D => "1",                        // LOCALE_IMEASURE
        0x0E => ".",                        // LOCALE_SDECIMAL
        0x0F => ",",                        // LOCALE_STHOUSAND
        0x10 => "3;0",                      // LOCALE_SGROUPING
        0x11 => "2",                        // LOCALE_IDIGITS
        0x12 => "1",                        // LOCALE_ILZERO
        0x13 => "0123456789",               // LOCALE_SNATIVEDIGITS
        0x14 => "$",                        // LOCALE_SCURRENCY
        0x15 => "USD",                      // LOCALE_SINTLSYMBOL
        0x16 => ".",                        // LOCALE_SMONDECIMALSEP
        0x17 => ",",                        // LOCALE_SMONTHOUSANDSEP
        0x18 => "3;0",                      // LOCALE_SMONGROUPING
        0x19 => "2",                        // LOCALE_ICURRDIGITS
        0x1B => "0",                        // LOCALE_ICURRENCY
        0x1C => "0",                        // LOCALE_INEGCURR
        0x1D => "/",                        // LOCALE_SDATE
        0x1E => ":",                        // LOCALE_STIME
        0x1F => "M/d/yyyy",                 // LOCALE_SSHORTDATE
        0x20 => "dddd, MMMM d, yyyy",       // LOCALE_SLONGDATE
        0x21 => "0",                        // LOCALE_IDATE (M/D/Y)
        0x22 => "0",                        // LOCALE_ILDATE
        0x23 => "0",                        // LOCALE_ITIME (12 hour)
        0x24 => "1",                        // LOCALE_ICENTURY
        0x25 => "0",                        // LOCALE_ITLZERO
        0x26 => "0",                        // LOCALE_IDAYLZERO
        0x27 => "0",                        // LOCALE_IMONLZERO
        0x28 => "AM",                       // LOCALE_S1159
        0x29 => "PM",                       // LOCALE_S2359
        0x2A..=0x30 => DAYS[(t - 0x2A) as usize],
        0x31..=0x37 => &DAYS[(t - 0x31) as usize][..3],
        0x38..=0x43 => MONTHS[(t - 0x38) as usize],
        0x44..=0x4F => &MONTHS[(t - 0x44) as usize][..3],
        0x50 => "",                         // LOCALE_SPOSITIVESIGN
        0x51 => "-",                        // LOCALE_SNEGATIVESIGN
        0x59 => "en",                       // LOCALE_SISO639LANGNAME
        0x5A => "US",                       // LOCALE_SISO3166CTRYNAME
        0x5C => "en-US",                    // LOCALE_SNAME
        0x79 => "h:mm tt",                  // LOCALE_SSHORTTIME
        0x1001 => "English",                // LOCALE_SENGLANGUAGE
        0x1002 => "United States",          // LOCALE_SENGCOUNTRY
        0x1003 => "h:mm:ss tt",             // LOCALE_STIMEFORMAT
        0x1004 => "65001",                  // LOCALE_IDEFAULTANSICODEPAGE
        0x1009 => "0",                      // LOCALE_IFIRSTWEEKOFYEAR
        0x100C => "6",                      // LOCALE_IFIRSTDAYOFWEEK (Sunday)
        0x1014 => "1",                      // LOCALE_IREADINGLAYOUT... approximate
        _ => return None,
    })
}

const LOCALE_RETURN_NUMBER: u32 = 0x2000_0000;

unsafe fn locale_out(value: &str, number: bool, out: *mut u16, cap: i32) -> i32 {
    if number {
        // A DWORD in place of the text.
        if cap >= 2 && !out.is_null() {
            unsafe { core::ptr::write_unaligned(out as *mut u32, value.parse().unwrap_or(0)) };
        }
        return 2;
    }
    let units: alloc_free::Units = alloc_free::Units::from(value);
    let n = units.len + 1;
    if cap == 0 {
        return n as i32;
    }
    if (cap as usize) < n || out.is_null() {
        crate::SetLastError(ERROR_INSUFFICIENT_BUFFER);
        return 0;
    }
    unsafe {
        core::ptr::copy_nonoverlapping(units.buf.as_ptr(), out, units.len);
        *out.add(units.len) = 0;
    }
    n as i32
}

/// A short str as UTF-16 without allocating.
mod alloc_free {
    pub struct Units {
        pub buf: [u16; 64],
        pub len: usize,
    }
    impl From<&str> for Units {
        fn from(s: &str) -> Self {
            let mut u = Units { buf: [0; 64], len: 0 };
            for c in s.encode_utf16().take(63) {
                u.buf[u.len] = c;
                u.len += 1;
            }
            u
        }
    }
}

#[no_mangle]
pub unsafe extern "system" fn GetLocaleInfoW(_lcid: u32, kind: u32, out: *mut u16, cap: i32) -> i32 {
    match locale_value(kind & 0x0FFF_FFFF) {
        Some(v) => unsafe { locale_out(v, kind & LOCALE_RETURN_NUMBER != 0, out, cap) },
        None => {
            crate::SetLastError(1004); // ERROR_INVALID_FLAGS
            0
        }
    }
}

#[no_mangle]
pub unsafe extern "system" fn GetLocaleInfoEx(_name: *const u16, kind: u32, out: *mut u16, cap: i32) -> i32 {
    unsafe { GetLocaleInfoW(LOCALE_EN_US, kind, out, cap) }
}

#[no_mangle]
pub unsafe extern "system" fn GetLocaleInfoA(lcid: u32, kind: u32, out: *mut u8, cap: i32) -> i32 {
    let mut w = [0u16; 128];
    let n = unsafe { GetLocaleInfoW(lcid, kind, w.as_mut_ptr(), w.len() as i32) };
    if n <= 0 {
        return n;
    }
    if kind & LOCALE_RETURN_NUMBER != 0 {
        if cap >= 4 && !out.is_null() {
            unsafe { core::ptr::copy_nonoverlapping(w.as_ptr() as *const u8, out, 4) };
        }
        return 4;
    }
    if cap == 0 {
        return n;
    }
    if cap < n || out.is_null() {
        crate::SetLastError(ERROR_INSUFFICIENT_BUFFER);
        return 0;
    }
    for i in 0..n as usize {
        unsafe { *out.add(i) = w[i] as u8 };
    }
    n
}

#[no_mangle]
pub unsafe extern "system" fn GetUserDefaultLocaleName(out: *mut u16, cap: i32) -> i32 {
    unsafe { locale_out("en-US", false, out, cap) }
}

#[no_mangle]
pub unsafe extern "system" fn LCIDToLocaleName(_lcid: u32, out: *mut u16, cap: i32, _flags: u32) -> i32 {
    unsafe { locale_out("en-US", false, out, cap) }
}

#[no_mangle]
pub extern "system" fn LocaleNameToLCID(_name: *const u16, _flags: u32) -> u32 {
    LOCALE_EN_US
}

// -----| dates and times as text |-----

/// Expand a date or time picture ("M/d/yyyy", "h:mm tt") for `st`.
fn picture(fmt: &[u16], st: &SystemTime, out: &mut [u16; 128], n: &mut usize) {
    let put = |s: &str, out: &mut [u16; 128], n: &mut usize| {
        for u in s.encode_utf16() {
            if *n < out.len() - 1 {
                out[*n] = u;
                *n += 1;
            }
        }
    };
    let mut i = 0;
    let dow = (st.day_of_week as usize + 6) % 7; // SYSTEMTIME: Sunday = 0
    while i < fmt.len() {
        let c = fmt[i];
        if c == b'\'' as u16 {
            // Quoted text ('' is a quote).
            i += 1;
            while i < fmt.len() {
                if fmt[i] == b'\'' as u16 {
                    if i + 1 < fmt.len() && fmt[i + 1] == b'\'' as u16 {
                        put("'", out, n);
                        i += 2;
                        continue;
                    }
                    break;
                }
                if *n < out.len() - 1 {
                    out[*n] = fmt[i];
                    *n += 1;
                }
                i += 1;
            }
            i += 1;
            continue;
        }
        let mut run = 1;
        while i + run < fmt.len() && fmt[i + run] == c {
            run += 1;
        }
        let num = |v: u16, width: usize, out: &mut [u16; 128], n: &mut usize| {
            let mut digits = [0u8; 6];
            let mut k = 0;
            let mut v = v as u32;
            loop {
                digits[k] = b'0' + (v % 10) as u8;
                k += 1;
                v /= 10;
                if v == 0 {
                    break;
                }
            }
            for _ in k..width {
                put("0", out, n);
            }
            for d in digits[..k].iter().rev() {
                put(core::str::from_utf8(core::slice::from_ref(d)).unwrap(), out, n);
            }
        };
        let h12 = if st.hour % 12 == 0 { 12 } else { st.hour % 12 };
        match c as u8 {
            b'd' => match run {
                1 => num(st.day, 1, out, n),
                2 => num(st.day, 2, out, n),
                3 => put(&DAYS[dow][..3], out, n),
                _ => put(DAYS[dow], out, n),
            },
            b'M' => match run {
                1 => num(st.month, 1, out, n),
                2 => num(st.month, 2, out, n),
                3 => put(&MONTHS[(st.month as usize).clamp(1, 12) - 1][..3], out, n),
                _ => put(MONTHS[(st.month as usize).clamp(1, 12) - 1], out, n),
            },
            b'y' => match run {
                1 => num(st.year % 100, 1, out, n),
                2 => num(st.year % 100, 2, out, n),
                _ => num(st.year, 4, out, n),
            },
            b'g' => put("A.D.", out, n),
            b'h' => num(h12, run.min(2), out, n),
            b'H' => num(st.hour, run.min(2), out, n),
            b'm' => num(st.minute, run.min(2), out, n),
            b's' => num(st.second, run.min(2), out, n),
            b't' => put(if st.hour < 12 { if run == 1 { "A" } else { "AM" } } else if run == 1 { "P" } else { "PM" }, out, n),
            _ => {
                for _ in 0..run {
                    if *n < out.len() - 1 {
                        out[*n] = c;
                        *n += 1;
                    }
                }
            }
        }
        i += run;
    }
}

unsafe fn time_or_now(st: *const SystemTime) -> SystemTime {
    if st.is_null() {
        let mut now = core::mem::MaybeUninit::<SystemTime>::uninit();
        unsafe {
            crate::time::GetLocalTime(now.as_mut_ptr());
            now.assume_init()
        }
    } else {
        unsafe { core::ptr::read_unaligned(st) }
    }
}

unsafe fn picture_out(text: &[u16], out: *mut u16, cap: i32) -> i32 {
    let n = text.len() + 1;
    if cap == 0 {
        return n as i32;
    }
    if (cap as usize) < n || out.is_null() {
        crate::SetLastError(ERROR_INSUFFICIENT_BUFFER);
        return 0;
    }
    unsafe {
        core::ptr::copy_nonoverlapping(text.as_ptr(), out, text.len());
        *out.add(text.len()) = 0;
    }
    n as i32
}

const DATE_LONGDATE: u32 = 2;

#[no_mangle]
pub unsafe extern "system" fn GetDateFormatW(_lcid: u32, flags: u32, st: *const SystemTime, format: *const u16, out: *mut u16, cap: i32) -> i32 {
    let st = unsafe { time_or_now(st) };
    let default = alloc_free::Units::from(if flags & DATE_LONGDATE != 0 { "dddd, MMMM d, yyyy" } else { "M/d/yyyy" });
    let fmt = if format.is_null() { &default.buf[..default.len] } else { unsafe { wstr(format) } };
    let mut text = [0u16; 128];
    let mut n = 0;
    picture(fmt, &st, &mut text, &mut n);
    unsafe { picture_out(&text[..n], out, cap) }
}

#[no_mangle]
pub unsafe extern "system" fn GetDateFormatEx(_name: *const u16, flags: u32, st: *const SystemTime, format: *const u16, out: *mut u16, cap: i32, _cal: *const u16) -> i32 {
    unsafe { GetDateFormatW(LOCALE_EN_US, flags, st, format, out, cap) }
}

const TIME_NOMINUTESORSECONDS: u32 = 1;
const TIME_NOSECONDS: u32 = 2;
const TIME_NOTIMEMARKER: u32 = 4;
const TIME_FORCE24HOURFORMAT: u32 = 8;

#[no_mangle]
pub unsafe extern "system" fn GetTimeFormatW(_lcid: u32, flags: u32, st: *const SystemTime, format: *const u16, out: *mut u16, cap: i32) -> i32 {
    let st = unsafe { time_or_now(st) };
    let mut base = alloc_free::Units::from("h:mm:ss tt");
    if !format.is_null() {
        let f = unsafe { wstr(format) };
        base.len = f.len().min(63);
        base.buf[..base.len].copy_from_slice(&f[..base.len]);
    }
    // Drop what the flags say to, with the separator before it.
    let mut fmt = [0u16; 64];
    let mut k = 0;
    let src = &base.buf[..base.len];
    let mut i = 0;
    while i < src.len() {
        let c = src[i] as u8;
        let skip = (c == b's' && flags & (TIME_NOSECONDS | TIME_NOMINUTESORSECONDS) != 0)
            || (c == b'm' && flags & TIME_NOMINUTESORSECONDS != 0)
            || (c == b't' && flags & TIME_NOTIMEMARKER != 0);
        if skip {
            while k > 0 && !matches!(fmt[k - 1] as u8, b'h' | b'H' | b'm' | b's' | b't') {
                k -= 1;
            }
            while i < src.len() && src[i] == c as u16 {
                i += 1;
            }
            continue;
        }
        fmt[k] = if c == b'h' && flags & TIME_FORCE24HOURFORMAT != 0 { b'H' as u16 } else { src[i] };
        k += 1;
        i += 1;
    }
    let mut text = [0u16; 128];
    let mut n = 0;
    picture(&fmt[..k], &st, &mut text, &mut n);
    unsafe { picture_out(&text[..n], out, cap) }
}

#[no_mangle]
pub unsafe extern "system" fn GetTimeFormatEx(_name: *const u16, flags: u32, st: *const SystemTime, format: *const u16, out: *mut u16, cap: i32) -> i32 {
    unsafe { GetTimeFormatW(LOCALE_EN_US, flags, st, format, out, cap) }
}

// -----| comparing strings |-----

fn fold(u: u16) -> u16 {
    if (b'a' as u16..=b'z' as u16).contains(&u) { u - 32 } else { u }
}

fn compare(a: &[u16], b: &[u16], ignore_case: bool) -> i32 {
    for (&x, &y) in a.iter().zip(b) {
        let (x, y) = if ignore_case { (fold(x), fold(y)) } else { (x, y) };
        if x != y {
            return if x < y { -1 } else { 1 };
        }
    }
    (a.len() as i32 - b.len() as i32).signum()
}

#[no_mangle]
pub unsafe extern "system" fn lstrcmpW(a: *const u16, b: *const u16) -> i32 {
    compare(unsafe { wstr(a) }, unsafe { wstr(b) }, false)
}

#[no_mangle]
pub unsafe extern "system" fn lstrcmpiW(a: *const u16, b: *const u16) -> i32 {
    compare(unsafe { wstr(a) }, unsafe { wstr(b) }, true)
}

#[no_mangle]
pub unsafe extern "system" fn lstrlenW(s: *const u16) -> i32 {
    if s.is_null() { 0 } else { unsafe { crate::util::wlen(s) as i32 } }
}

#[no_mangle]
pub unsafe extern "system" fn lstrlenA(s: *const u8) -> i32 {
    if s.is_null() { 0 } else { unsafe { crate::util::alen(s) as i32 } }
}

const NORM_IGNORECASE: u32 = 1;
const LINGUISTIC_IGNORECASE: u32 = 0x10;

/// CompareString's result: CSTR_LESS_THAN 1, CSTR_EQUAL 2, CSTR_GREATER_THAN 3.
unsafe fn compare_string(flags: u32, a: *const u16, alen: i32, b: *const u16, blen: i32) -> i32 {
    let a = if alen < 0 { unsafe { wstr(a) } } else { unsafe { core::slice::from_raw_parts(a, alen as usize) } };
    let b = if blen < 0 { unsafe { wstr(b) } } else { unsafe { core::slice::from_raw_parts(b, blen as usize) } };
    compare(a, b, flags & (NORM_IGNORECASE | LINGUISTIC_IGNORECASE) != 0) + 2
}

#[no_mangle]
pub unsafe extern "system" fn CompareStringW(_lcid: u32, flags: u32, a: *const u16, alen: i32, b: *const u16, blen: i32) -> i32 {
    unsafe { compare_string(flags, a, alen, b, blen) }
}

#[no_mangle]
pub unsafe extern "system" fn CompareStringEx(_name: *const u16, flags: u32, a: *const u16, alen: i32, b: *const u16, blen: i32, _v: usize, _r: usize, _p: usize) -> i32 {
    unsafe { compare_string(flags, a, alen, b, blen) }
}

#[no_mangle]
pub unsafe extern "system" fn CompareStringOrdinal(a: *const u16, alen: i32, b: *const u16, blen: i32, ignore_case: Bool) -> i32 {
    unsafe { compare_string(if ignore_case != 0 { NORM_IGNORECASE } else { 0 }, a, alen, b, blen) }
}

#[no_mangle]
pub unsafe extern "system" fn CompareFileTime(a: *const u64, b: *const u64) -> i32 {
    let (a, b) = unsafe { (core::ptr::read_unaligned(a), core::ptr::read_unaligned(b)) };
    (a as i64).cmp(&(b as i64)) as i32
}

/// LCMapString for case mapping (LCMAP_LOWERCASE 0x100, LCMAP_UPPERCASE 0x200).
#[no_mangle]
pub unsafe extern "system" fn LCMapStringW(_lcid: u32, flags: u32, src: *const u16, len: i32, dst: *mut u16, cap: i32) -> i32 {
    let s = if len < 0 { unsafe { core::slice::from_raw_parts(src, crate::util::wlen(src) + 1) } } else { unsafe { core::slice::from_raw_parts(src, len as usize) } };
    if cap == 0 {
        return s.len() as i32;
    }
    if (cap as usize) < s.len() {
        crate::SetLastError(ERROR_INSUFFICIENT_BUFFER);
        return 0;
    }
    for (i, &u) in s.iter().enumerate() {
        let mapped = if flags & 0x200 != 0 {
            fold(u)
        } else if flags & 0x100 != 0 && (b'A' as u16..=b'Z' as u16).contains(&u) {
            u + 32
        } else {
            u
        };
        unsafe { *dst.add(i) = mapped };
    }
    s.len() as i32
}

#[no_mangle]
pub unsafe extern "system" fn LCMapStringEx(_name: *const u16, flags: u32, src: *const u16, len: i32, dst: *mut u16, cap: i32, _v: usize, _r: usize, _p: usize) -> i32 {
    unsafe { LCMapStringW(LOCALE_EN_US, flags, src, len, dst, cap) }
}
