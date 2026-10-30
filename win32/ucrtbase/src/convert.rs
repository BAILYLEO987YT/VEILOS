//! Text <-> numbers, and narrow (UTF-8) <-> wide (UTF-16) text.
//!
//! Bytes that aren't valid UTF-8 convert as Latin-1 (each byte its own
//! character) rather than failing, so old programs' text survives.

use crate::errno::{EINVAL, ERANGE};
use crate::string::{len, Ch};
use crate::{ctype, heap, invalid_parameter, set_errno};

// -----| UTF-8 / UTF-16 |-----

/// The character starting at `s[0]` and its length in bytes.
pub fn decode_utf8(s: &[u8]) -> (u32, usize) {
    let b = s[0] as u32;
    let (n, min, init) = match b {
        0..=0x7F => return (b, 1),
        0xC2..=0xDF => (2, 0x80, b & 0x1F),
        0xE0..=0xEF => (3, 0x800, b & 0x0F),
        0xF0..=0xF4 => (4, 0x10000, b & 0x07),
        _ => return (b, 1),
    };
    if s.len() < n {
        return (b, 1);
    }
    let mut c = init;
    for &x in &s[1..n] {
        if x & 0xC0 != 0x80 {
            return (b, 1);
        }
        c = (c << 6) | (x as u32 & 0x3F);
    }
    if c < min || c > 0x10FFFF {
        return (b, 1);
    }
    (c, n)
}

/// Whether `s` is a complete (or invalid, so byte-sized) character.
fn utf8_needs(first: u8) -> usize {
    match first {
        0xC2..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xF4 => 4,
        _ => 1,
    }
}

pub fn encode_utf8(c: u32, out: &mut impl FnMut(u8)) {
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

pub fn encode_utf16(c: u32, out: &mut impl FnMut(u16)) {
    if c < 0x10000 {
        out(c as u16);
    } else {
        let c = c - 0x10000;
        out(0xD800 | (c >> 10) as u16);
        out(0xDC00 | (c & 0x3FF) as u16);
    }
}

/// UTF-8 bytes to UTF-16 units.
pub fn utf8_to_utf16(s: &[u8], mut out: impl FnMut(u16)) {
    let mut i = 0;
    while i < s.len() {
        let (c, n) = decode_utf8(&s[i..]);
        encode_utf16(c, &mut out);
        i += n;
    }
}

/// UTF-16 units to UTF-8 bytes (a lone surrogate encodes as itself).
pub fn utf16_to_utf8(s: &[u16], mut out: impl FnMut(u8)) {
    let mut i = 0;
    while i < s.len() {
        let mut c = s[i] as u32;
        i += 1;
        if (0xD800..0xDC00).contains(&c) && i < s.len() && (0xDC00..0xE000).contains(&s[i]) {
            c = 0x10000 + ((c - 0xD800) << 10) + (s[i] as u32 - 0xDC00);
            i += 1;
        }
        encode_utf8(c, &mut out);
    }
}

/// A new NUL-terminated wide copy of `s` (free with `free`).
pub fn to_wide(s: &[u8]) -> *mut u16 {
    let p = heap::malloc((s.len() + 1) * 2) as *mut u16;
    if p.is_null() {
        return p;
    }
    let mut n = 0;
    utf8_to_utf16(s, |u| {
        unsafe { *p.add(n) = u };
        n += 1;
    });
    unsafe { *p.add(n) = 0 };
    p
}

/// A new NUL-terminated UTF-8 copy of `s` (free with `free`).
pub fn to_narrow(s: &[u16]) -> *mut u8 {
    let p = heap::malloc(s.len() * 3 + 1);
    if p.is_null() {
        return p;
    }
    let mut n = 0;
    utf16_to_utf8(s, |b| {
        unsafe { *p.add(n) = b };
        n += 1;
    });
    unsafe { *p.add(n) = 0 };
    p
}

// -----| integers |-----

pub struct Int {
    pub magnitude: u64,
    pub negative: bool,
    pub overflow: bool,
    /// Units used; 0 if there was no number.
    pub end: usize,
}

fn digit(c: u32) -> u32 {
    match c {
        0x30..=0x39 => c - 0x30,
        0x41..=0x5A => c - 0x41 + 10,
        0x61..=0x7A => c - 0x61 + 10,
        _ => 99,
    }
}

/// strtol's grammar: spaces, a sign, a base prefix, digits.
pub unsafe fn parse_int<T: Ch>(s: *const T, mut base: u32) -> Int {
    let at = |i: usize| unsafe { (*s.add(i)).to_u32() };
    let mut r = Int { magnitude: 0, negative: false, overflow: false, end: 0 };
    if base == 1 || base > 36 {
        invalid_parameter();
        return r;
    }
    let mut i = 0;
    while ctype::is_space(at(i)) && at(i) != 0 {
        i += 1;
    }
    if at(i) == b'-' as u32 || at(i) == b'+' as u32 {
        r.negative = at(i) == b'-' as u32;
        i += 1;
    }
    let hex_prefix = at(i) == b'0' as u32 && (at(i + 1) | 0x20) == b'x' as u32 && digit(at(i + 2)) < 16;
    if (base == 0 || base == 16) && hex_prefix {
        base = 16;
        i += 2;
    } else if base == 0 {
        base = if at(i) == b'0' as u32 { 8 } else { 10 };
    }
    let start = i;
    while digit(at(i)) < base {
        match r.magnitude.checked_mul(base as u64).and_then(|m| m.checked_add(digit(at(i)) as u64)) {
            Some(m) => r.magnitude = m,
            None => r.overflow = true,
        }
        i += 1;
    }
    if i > start {
        r.end = i;
    }
    r
}

/// Signed result in [-2^(bits-1), 2^(bits-1)), clamped with ERANGE.
pub fn signed(r: &Int, bits: u32) -> i64 {
    let limit = 1u64 << (bits - 1);
    if r.negative {
        if r.overflow || r.magnitude > limit {
            set_errno(ERANGE);
            return (limit as i64).wrapping_neg();
        }
        (r.magnitude as i64).wrapping_neg()
    } else {
        if r.overflow || r.magnitude >= limit {
            set_errno(ERANGE);
            return (limit - 1) as i64;
        }
        r.magnitude as i64
    }
}

/// Unsigned result (negated if there was a '-', as C says), clamped.
pub fn unsigned(r: &Int, bits: u32) -> u64 {
    let max = if bits == 64 { u64::MAX } else { (1u64 << bits) - 1 };
    if r.overflow || r.magnitude > max {
        set_errno(ERANGE);
        return max;
    }
    if r.negative { r.magnitude.wrapping_neg() & max } else { r.magnitude }
}

unsafe fn set_end<T: Ch>(end: *mut *const T, s: *const T, n: usize) {
    if !end.is_null() {
        unsafe { *end = s.add(n) };
    }
}

macro_rules! strto_int {
    ($($n:ident / $w:ident -> $t:ty = $conv:ident($bits:expr);)*) => {$(
        #[no_mangle]
        pub unsafe extern "C" fn $n(s: *const u8, end: *mut *const u8, base: i32) -> $t {
            let r = unsafe { parse_int(s, base as u32) };
            unsafe { set_end(end, s, r.end) };
            $conv(&r, $bits) as $t
        }
        #[no_mangle]
        pub unsafe extern "C" fn $w(s: *const u16, end: *mut *const u16, base: i32) -> $t {
            let r = unsafe { parse_int(s, base as u32) };
            unsafe { set_end(end, s, r.end) };
            $conv(&r, $bits) as $t
        }
    )*};
}

strto_int! {
    strtol / wcstol -> i32 = signed(32);
    strtoul / wcstoul -> u32 = unsigned(32);
    strtoll / wcstoll -> i64 = signed(64);
    strtoull / wcstoull -> u64 = unsigned(64);
    _strtoi64 / _wcstoi64 -> i64 = signed(64);
    _strtoui64 / _wcstoui64 -> u64 = unsigned(64);
    strtoimax / wcstoimax -> i64 = signed(64);
    strtoumax / wcstoumax -> u64 = unsigned(64);
}

macro_rules! ato_int {
    ($($n:ident / $w:ident -> $t:ty = $bits:expr;)*) => {$(
        #[no_mangle]
        pub unsafe extern "C" fn $n(s: *const u8) -> $t {
            signed(&unsafe { parse_int(s, 10) }, $bits) as $t
        }
        #[no_mangle]
        pub unsafe extern "C" fn $w(s: *const u16) -> $t {
            signed(&unsafe { parse_int(s, 10) }, $bits) as $t
        }
    )*};
}

ato_int! {
    atoi / _wtoi -> i32 = 32;
    atol / _wtol -> i32 = 32;
    atoll / _wtoll -> i64 = 64;
    _atoi64 / _wtoi64 -> i64 = 64;
}

/// Digits of `v` in `radix` (with a '-' if `negative`) into `out`, which
/// must hold 66 units. Returns the length.
fn write_radix<T: Ch>(mut v: u64, negative: bool, radix: u32, out: *mut T) -> usize {
    let mut tmp = [0u8; 64];
    let mut n = 0;
    loop {
        let d = (v % radix as u64) as u8;
        tmp[n] = if d < 10 { b'0' + d } else { b'a' + d - 10 };
        n += 1;
        v /= radix as u64;
        if v == 0 {
            break;
        }
    }
    let mut k = 0;
    if negative {
        unsafe { *out = T::from_u32(b'-' as u32) };
        k = 1;
    }
    for i in (0..n).rev() {
        unsafe { *out.add(k) = T::from_u32(tmp[i] as u32) };
        k += 1;
    }
    unsafe { *out.add(k) = T::default() };
    k
}

/// `value` as `_itoa` treats it: negative only in base 10, else the
/// unsigned `bits`-wide pattern.
fn split(value: i64, bits: u32, radix: i32) -> (u64, bool) {
    let mask = if bits == 64 { u64::MAX } else { (1u64 << bits) - 1 };
    if radix == 10 && value < 0 { (value.unsigned_abs(), true) } else { (value as u64 & mask, false) }
}

unsafe fn to_text<T: Ch>(v: u64, negative: bool, out: *mut T, radix: i32) -> *mut T {
    if !(2..=36).contains(&radix) || out.is_null() {
        invalid_parameter();
        return out;
    }
    write_radix(v, negative, radix as u32, out);
    out
}

unsafe fn to_text_s<T: Ch>(v: u64, negative: bool, out: *mut T, size: usize, radix: i32) -> i32 {
    if !(2..=36).contains(&radix) || out.is_null() || size == 0 {
        invalid_parameter();
        return EINVAL;
    }
    let mut tmp = [T::default(); 66];
    let n = write_radix(v, negative, radix as u32, tmp.as_mut_ptr());
    if n >= size {
        unsafe { *out = T::default() };
        invalid_parameter();
        return ERANGE;
    }
    unsafe { core::ptr::copy_nonoverlapping(tmp.as_ptr(), out, n + 1) };
    0
}

macro_rules! to_a {
    ($($n:ident / $w:ident / $ns:ident / $ws:ident ($t:ty, $bits:expr);)*) => {$(
        #[no_mangle]
        pub unsafe extern "C" fn $n(v: $t, out: *mut u8, radix: i32) -> *mut u8 {
            let (m, neg) = split(v as i64, $bits, radix);
            unsafe { to_text(m, neg, out, radix) }
        }
        #[no_mangle]
        pub unsafe extern "C" fn $w(v: $t, out: *mut u16, radix: i32) -> *mut u16 {
            let (m, neg) = split(v as i64, $bits, radix);
            unsafe { to_text(m, neg, out, radix) }
        }
        #[no_mangle]
        pub unsafe extern "C" fn $ns(v: $t, out: *mut u8, size: usize, radix: i32) -> i32 {
            let (m, neg) = split(v as i64, $bits, radix);
            unsafe { to_text_s(m, neg, out, size, radix) }
        }
        #[no_mangle]
        pub unsafe extern "C" fn $ws(v: $t, out: *mut u16, size: usize, radix: i32) -> i32 {
            let (m, neg) = split(v as i64, $bits, radix);
            unsafe { to_text_s(m, neg, out, size, radix) }
        }
    )*};
}

to_a! {
    _itoa / _itow / _itoa_s / _itow_s (i32, 32);
    _ltoa / _ltow / _ltoa_s / _ltow_s (i32, 32);
    _i64toa / _i64tow / _i64toa_s / _i64tow_s (i64, 64);
}

macro_rules! to_a_unsigned {
    ($($n:ident / $w:ident / $ns:ident / $ws:ident ($t:ty);)*) => {$(
        #[no_mangle]
        pub unsafe extern "C" fn $n(v: $t, out: *mut u8, radix: i32) -> *mut u8 {
            unsafe { to_text(v as u64, false, out, radix) }
        }
        #[no_mangle]
        pub unsafe extern "C" fn $w(v: $t, out: *mut u16, radix: i32) -> *mut u16 {
            unsafe { to_text(v as u64, false, out, radix) }
        }
        #[no_mangle]
        pub unsafe extern "C" fn $ns(v: $t, out: *mut u8, size: usize, radix: i32) -> i32 {
            unsafe { to_text_s(v as u64, false, out, size, radix) }
        }
        #[no_mangle]
        pub unsafe extern "C" fn $ws(v: $t, out: *mut u16, size: usize, radix: i32) -> i32 {
            unsafe { to_text_s(v as u64, false, out, size, radix) }
        }
    )*};
}

to_a_unsigned! {
    _ultoa / _ultow / _ultoa_s / _ultow_s (u32);
    _ui64toa / _ui64tow / _ui64toa_s / _ui64tow_s (u64);
}

// -----| floating point |-----

pub struct Float {
    pub value: f64,
    /// Units used; 0 if there was no number.
    pub end: usize,
}

fn eq_ignore_case<T: Ch>(s: *const T, word: &[u8]) -> bool {
    word.iter().enumerate().all(|(i, &w)| ctype::lower(unsafe { (*s.add(i)).to_u32() }) == w as u32)
}

/// strtod's grammar: spaces, a sign, then a decimal or hex number, "inf",
/// "infinity" or "nan(...)". `single` rounds straight to float.
pub unsafe fn parse_float<T: Ch>(s: *const T, single: bool) -> Float {
    let at = |i: usize| unsafe { (*s.add(i)).to_u32() };
    let none = Float { value: 0.0, end: 0 };
    let mut i = 0;
    while ctype::is_space(at(i)) && at(i) != 0 {
        i += 1;
    }
    let sign_at = i;
    let negative = at(i) == b'-' as u32;
    if negative || at(i) == b'+' as u32 {
        i += 1;
    }
    let sign = if negative { -1.0 } else { 1.0 };
    let rest = unsafe { s.add(i) };
    if eq_ignore_case(rest, b"inf") {
        let n = if eq_ignore_case(rest, b"infinity") { 8 } else { 3 };
        return Float { value: sign * f64::INFINITY, end: i + n };
    }
    if eq_ignore_case(rest, b"nan") {
        let mut n = 3;
        if at(i + 3) == b'(' as u32 {
            let mut k = i + 4;
            while ctype::classify(at(k)) & (ctype::ALPHA | ctype::DIGIT) != 0 || at(k) == b'_' as u32 {
                k += 1;
            }
            if at(k) == b')' as u32 {
                n = k + 1 - i;
            }
        }
        return Float { value: sign * f64::NAN, end: i + n };
    }
    if at(i) == b'0' as u32 && (at(i + 1) | 0x20) == b'x' as u32 {
        if let Some((v, n)) = parse_hex_float(|k| at(i + 2 + k)) {
            return Float { value: sign * v, end: i + 2 + n };
        }
        // "0x" with no hex digits: just the 0.
        return Float { value: sign * 0.0, end: i + 1 };
    }

    // Decimal: find its extent, then let core's correctly rounded parser
    // read a byte copy.
    let mut k = i;
    let mut digits = 0;
    let mut nonzero = false;
    while digit(at(k)) < 10 {
        nonzero |= at(k) != b'0' as u32;
        k += 1;
        digits += 1;
    }
    if at(k) == b'.' as u32 {
        k += 1;
        while digit(at(k)) < 10 {
            nonzero |= at(k) != b'0' as u32;
            k += 1;
            digits += 1;
        }
    }
    if digits == 0 {
        return none;
    }
    if (at(k) | 0x20) == b'e' as u32 {
        let mut e = k + 1;
        if at(e) == b'+' as u32 || at(e) == b'-' as u32 {
            e += 1;
        }
        if digit(at(e)) < 10 {
            while digit(at(e)) < 10 {
                e += 1;
            }
            k = e;
        }
    }
    let n = k - sign_at;
    let mut small = [0u8; 128];
    let buf = if n <= small.len() { small.as_mut_ptr() } else { heap::malloc(n) };
    if buf.is_null() {
        return none;
    }
    for j in 0..n {
        unsafe { *buf.add(j) = at(sign_at + j) as u8 };
    }
    let text = unsafe { core::str::from_utf8_unchecked(core::slice::from_raw_parts(buf, n)) };
    let value = if single {
        text.parse::<f32>().map(|v| v as f64).unwrap_or(0.0)
    } else {
        text.parse::<f64>().unwrap_or(0.0)
    };
    if n > small.len() {
        heap::free(buf);
    }
    if value.is_infinite() || (value == 0.0 && nonzero) {
        set_errno(ERANGE);
    }
    Float { value, end: k }
}

/// The part of a hex float after "0x": (value, units used), or None if
/// there are no digits.
fn parse_hex_float(at: impl Fn(usize) -> u32) -> Option<(f64, usize)> {
    let mut mantissa = 0u64;
    let mut exponent = 0i64;
    let mut sticky = false;
    let mut any = false;
    let mut k = 0;
    let mut seen_point = false;
    loop {
        let c = at(k);
        if c == b'.' as u32 && !seen_point {
            seen_point = true;
        } else if digit(c) < 16 {
            any = true;
            if mantissa >> 60 == 0 {
                mantissa = (mantissa << 4) | digit(c) as u64;
                if seen_point {
                    exponent -= 4;
                }
            } else {
                sticky |= digit(c) != 0;
                if !seen_point {
                    exponent += 4;
                }
            }
        } else {
            break;
        }
        k += 1;
    }
    if !any {
        return None;
    }
    if (at(k) | 0x20) == b'p' as u32 {
        let mut e = k + 1;
        let negative = at(e) == b'-' as u32;
        if negative || at(e) == b'+' as u32 {
            e += 1;
        }
        if digit(at(e)) < 10 {
            let mut x = 0i64;
            while digit(at(e)) < 10 {
                x = (x * 10 + digit(at(e)) as i64).min(100_000);
                e += 1;
            }
            exponent += if negative { -x } else { x };
            k = e;
        }
    }
    let m = (mantissa | sticky as u64) as f64;
    let value = libm::scalbn(m, exponent.clamp(-100_000, 100_000) as i32);
    if value.is_infinite() || (value == 0.0 && mantissa != 0) {
        set_errno(ERANGE);
    }
    Some((value, k))
}

#[no_mangle]
pub unsafe extern "C" fn strtod(s: *const u8, end: *mut *const u8) -> f64 {
    let r = unsafe { parse_float(s, false) };
    unsafe { set_end(end, s, r.end) };
    r.value
}

#[no_mangle]
pub unsafe extern "C" fn wcstod(s: *const u16, end: *mut *const u16) -> f64 {
    let r = unsafe { parse_float(s, false) };
    unsafe { set_end(end, s, r.end) };
    r.value
}

#[no_mangle]
pub unsafe extern "C" fn strtof(s: *const u8, end: *mut *const u8) -> f32 {
    let r = unsafe { parse_float(s, true) };
    unsafe { set_end(end, s, r.end) };
    r.value as f32
}

#[no_mangle]
pub unsafe extern "C" fn wcstof(s: *const u16, end: *mut *const u16) -> f32 {
    let r = unsafe { parse_float(s, true) };
    unsafe { set_end(end, s, r.end) };
    r.value as f32
}

#[no_mangle]
pub unsafe extern "C" fn strtold(s: *const u8, end: *mut *const u8) -> f64 {
    unsafe { strtod(s, end) }
}

#[no_mangle]
pub unsafe extern "C" fn wcstold(s: *const u16, end: *mut *const u16) -> f64 {
    unsafe { wcstod(s, end) }
}

#[no_mangle]
pub unsafe extern "C" fn atof(s: *const u8) -> f64 {
    unsafe { parse_float(s, false) }.value
}

#[no_mangle]
pub unsafe extern "C" fn _wtof(s: *const u16) -> f64 {
    unsafe { parse_float(s, false) }.value
}

/// `_atodbl`/`_atoflt`: parse into a struct holding the value.
#[no_mangle]
pub unsafe extern "C" fn _atodbl(out: *mut f64, s: *const u8) -> i32 {
    unsafe { *out = atof(s) };
    0
}

#[no_mangle]
pub unsafe extern "C" fn _atoflt(out: *mut f32, s: *const u8) -> i32 {
    unsafe { *out = parse_float(s, true).value as f32 };
    0
}

// -----| multibyte <-> wide |-----

/// mbstowcs: UTF-8 to UTF-16; with `dst` null, the length needed.
#[no_mangle]
pub unsafe extern "C" fn mbstowcs(dst: *mut u16, src: *const u8, n: usize) -> usize {
    let s = unsafe { crate::string::slice(src) };
    let mut k = 0;
    let mut i = 0;
    while i < s.len() {
        let (c, used) = decode_utf8(&s[i..]);
        let units = if c >= 0x10000 { 2 } else { 1 };
        if !dst.is_null() {
            if k + units > n {
                return k;
            }
            encode_utf16(c, &mut |u| {
                unsafe { *dst.add(k) = u };
                k += 1;
            });
        } else {
            k += units;
        }
        i += used;
    }
    if !dst.is_null() && k < n {
        unsafe { *dst.add(k) = 0 };
    }
    k
}

/// wcstombs: UTF-16 to UTF-8, never splitting a character.
#[no_mangle]
pub unsafe extern "C" fn wcstombs(dst: *mut u8, src: *const u16, n: usize) -> usize {
    let s = unsafe { core::slice::from_raw_parts(src, len(src)) };
    let mut k = 0;
    let mut i = 0;
    while i < s.len() {
        let j = if (0xD800..0xDC00).contains(&s[i]) && i + 1 < s.len() && (0xDC00..0xE000).contains(&s[i + 1]) { i + 2 } else { i + 1 };
        let mut bytes = [0u8; 4];
        let mut m = 0;
        utf16_to_utf8(&s[i..j], |b| {
            bytes[m] = b;
            m += 1;
        });
        if !dst.is_null() {
            if k + m > n {
                return k;
            }
            unsafe { core::ptr::copy_nonoverlapping(bytes.as_ptr(), dst.add(k), m) };
        }
        k += m;
        i = j;
    }
    if !dst.is_null() && k < n {
        unsafe { *dst.add(k) = 0 };
    }
    k
}

#[no_mangle]
pub unsafe extern "C" fn mbstowcs_s(count: *mut usize, dst: *mut u16, size: usize, src: *const u8, n: usize) -> i32 {
    let need = unsafe { mbstowcs(core::ptr::null_mut(), src, 0) };
    let take = need.min(n);
    if !dst.is_null() {
        if take >= size {
            unsafe { *dst = 0 };
            invalid_parameter();
            return ERANGE;
        }
        unsafe { mbstowcs(dst, src, take) };
        unsafe { *dst.add(take) = 0 };
    }
    if !count.is_null() {
        unsafe { *count = take + 1 };
    }
    0
}

#[no_mangle]
pub unsafe extern "C" fn wcstombs_s(count: *mut usize, dst: *mut u8, size: usize, src: *const u16, n: usize) -> i32 {
    let need = unsafe { wcstombs(core::ptr::null_mut(), src, 0) };
    let take = need.min(n);
    if !dst.is_null() {
        if take >= size {
            unsafe { *dst = 0 };
            invalid_parameter();
            return ERANGE;
        }
        let k = unsafe { wcstombs(dst, src, take) };
        unsafe { *dst.add(k) = 0 };
    }
    if !count.is_null() {
        unsafe { *count = take + 1 };
    }
    0
}

/// mbtowc: one character (first unit only for characters past U+FFFF).
#[no_mangle]
pub unsafe extern "C" fn mbtowc(dst: *mut u16, s: *const u8, n: usize) -> i32 {
    if s.is_null() {
        return 0;
    }
    if n == 0 {
        return -1;
    }
    let first = unsafe { *s };
    if first == 0 {
        if !dst.is_null() {
            unsafe { *dst = 0 };
        }
        return 0;
    }
    let want = utf8_needs(first);
    if want > n {
        set_errno(crate::errno::EILSEQ);
        return -1;
    }
    let (c, used) = decode_utf8(unsafe { core::slice::from_raw_parts(s, want) });
    if !dst.is_null() {
        let mut first_unit = true;
        encode_utf16(c, &mut |u| {
            if first_unit {
                unsafe { *dst = u };
                first_unit = false;
            }
        });
    }
    used as i32
}

#[no_mangle]
pub unsafe extern "C" fn mblen(s: *const u8, n: usize) -> i32 {
    unsafe { mbtowc(core::ptr::null_mut(), s, n) }
}

#[no_mangle]
pub unsafe extern "C" fn wctomb(dst: *mut u8, c: u16) -> i32 {
    if dst.is_null() {
        return 0;
    }
    let mut k = 0;
    encode_utf8(c as u32, &mut |b| {
        unsafe { *dst.add(k) = b };
        k += 1;
    });
    k as i32
}

#[no_mangle]
pub unsafe extern "C" fn wctomb_s(count: *mut i32, dst: *mut u8, size: usize, c: u16) -> i32 {
    let mut tmp = [0u8; 4];
    let n = unsafe { wctomb(tmp.as_mut_ptr(), c) };
    if !dst.is_null() {
        if n as usize > size {
            invalid_parameter();
            return ERANGE;
        }
        unsafe { core::ptr::copy_nonoverlapping(tmp.as_ptr(), dst, n as usize) };
    }
    if !count.is_null() {
        unsafe { *count = n };
    }
    0
}

/// The restartable forms: UTF-8 needs no state between whole characters,
/// and a split one is reported as incomplete (-2).
#[no_mangle]
pub unsafe extern "C" fn mbrtowc(dst: *mut u16, s: *const u8, n: usize, _state: *mut u32) -> usize {
    if s.is_null() {
        return 0;
    }
    if n == 0 {
        return usize::MAX - 1;
    }
    let first = unsafe { *s };
    if utf8_needs(first) > n {
        return usize::MAX - 1;
    }
    unsafe { mbtowc(dst, s, n) as usize }
}

#[no_mangle]
pub unsafe extern "C" fn mbrlen(s: *const u8, n: usize, state: *mut u32) -> usize {
    unsafe { mbrtowc(core::ptr::null_mut(), s, n, state) }
}

#[no_mangle]
pub unsafe extern "C" fn wcrtomb(dst: *mut u8, c: u16, _state: *mut u32) -> usize {
    if dst.is_null() {
        return 1;
    }
    unsafe { wctomb(dst, c) as usize }
}

#[no_mangle]
pub unsafe extern "C" fn wcrtomb_s(count: *mut usize, dst: *mut u8, size: usize, c: u16, _state: *mut u32) -> i32 {
    let mut n = 0;
    let r = unsafe { wctomb_s(&mut n, dst, size, c) };
    if !count.is_null() {
        unsafe { *count = n as usize };
    }
    r
}

#[no_mangle]
pub unsafe extern "C" fn mbsrtowcs(dst: *mut u16, src: *mut *const u8, n: usize, _state: *mut u32) -> usize {
    let s = unsafe { *src };
    let k = unsafe { mbstowcs(dst, s, n) };
    if !dst.is_null() {
        // Whole string converted (terminator written): src becomes null.
        unsafe { *src = if k < n { core::ptr::null() } else { s.add(len(s).min(k)) } };
    }
    k
}

#[no_mangle]
pub unsafe extern "C" fn wcsrtombs(dst: *mut u8, src: *mut *const u16, n: usize, _state: *mut u32) -> usize {
    let s = unsafe { *src };
    let k = unsafe { wcstombs(dst, s, n) };
    if !dst.is_null() && k < n {
        unsafe { *src = core::ptr::null() };
    }
    k
}

#[no_mangle]
pub extern "C" fn mbsinit(_state: *const u32) -> i32 {
    1
}

#[no_mangle]
pub extern "C" fn btowc(c: i32) -> u16 {
    if (0..0x80).contains(&c) { c as u16 } else { 0xFFFF }
}

#[no_mangle]
pub extern "C" fn wctob(c: u16) -> i32 {
    if c < 0x80 { c as i32 } else { -1 }
}
