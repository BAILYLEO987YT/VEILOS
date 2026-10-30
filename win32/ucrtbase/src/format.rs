//! The printf engine, for narrow (`u8`) and wide (`u16`) output alike.
//!
//! It follows the UCRT's output: `%p` as 16 uppercase hex digits, infinity
//! and NaN as "inf" / "nan" / "-nan(ind)", `%a` with all 13 hex digits, and
//! correctly rounded decimals (core's float formatting is exact). `%n` is
//! not supported (the UCRT disables it too). Options are the UCRT's
//! `_CRT_INTERNAL_PRINTF_*` bits; msvcrt's entry points pass legacy ones.

use crate::convert::{utf16_to_utf8, utf8_to_utf16};
use crate::string::{nlen, Ch};
use crate::{heap, VaList};
use core::fmt::Write;

/// In wide functions, %s and %c take wide arguments (msvcrt's meaning).
pub const LEGACY_WIDE_SPECIFIERS: u64 = 4;
/// Exponents have at least three digits ("1e+005").
pub const LEGACY_THREE_DIGIT_EXPONENTS: u64 = 16;

/// Where formatted output goes, a unit at a time.
pub trait Out<C> {
    fn put(&mut self, c: C);
}

/// A growable ASCII buffer for numbers: inline for the usual sizes, on the
/// heap for very long ones (%.500f).
pub struct Text {
    inline: [u8; 400],
    heap: *mut u8,
    cap: usize,
    pub len: usize,
}

impl Text {
    pub fn new() -> Self {
        Text { inline: [0; 400], heap: core::ptr::null_mut(), cap: 400, len: 0 }
    }
    pub fn bytes(&self) -> &[u8] {
        let p = if self.heap.is_null() { self.inline.as_ptr() } else { self.heap };
        unsafe { core::slice::from_raw_parts(p, self.len) }
    }
    pub fn push(&mut self, b: u8) {
        let _ = self.write_str(unsafe { core::str::from_utf8_unchecked(&[b]) });
    }
}

impl Write for Text {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        if self.len + s.len() > self.cap {
            let cap = (self.len + s.len()) * 2;
            let p = heap::malloc(cap);
            if p.is_null() {
                return Err(core::fmt::Error);
            }
            unsafe { core::ptr::copy_nonoverlapping(self.bytes().as_ptr(), p, self.len) };
            if !self.heap.is_null() {
                heap::free(self.heap);
            }
            self.heap = p;
            self.cap = cap;
        }
        let base = if self.heap.is_null() { self.inline.as_mut_ptr() } else { self.heap };
        unsafe { core::ptr::copy_nonoverlapping(s.as_ptr(), base.add(self.len), s.len()) };
        self.len += s.len();
        Ok(())
    }
}

impl Drop for Text {
    fn drop(&mut self) {
        if !self.heap.is_null() {
            heap::free(self.heap);
        }
    }
}

#[derive(Default, Clone, Copy)]
struct Spec {
    left: bool,
    plus: bool,
    space: bool,
    alt: bool,
    zero: bool,
    width: usize,
    precision: Option<usize>,
}

#[derive(Clone, Copy, PartialEq)]
enum Size {
    Default,
    Char,  // hh
    Short, // h
    Long,  // l, w
    Wide64, // ll, I64, j, z, t, I, L
}

struct Writer<'a, C> {
    out: &'a mut dyn Out<C>,
    count: usize,
}

impl<C: Ch> Writer<'_, C> {
    fn put(&mut self, c: C) {
        self.out.put(c);
        self.count += 1;
    }
    fn ascii(&mut self, s: &[u8]) {
        for &b in s {
            self.put(C::from_u32(b as u32));
        }
    }
    fn repeat(&mut self, b: u8, n: usize) {
        for _ in 0..n {
            self.put(C::from_u32(b as u32));
        }
    }

    /// `prefix` (sign, "0x") then `body`, padded to the width.
    fn padded(&mut self, spec: &Spec, prefix: &[u8], body: &[u8], zero_ok: bool) {
        let n = prefix.len() + body.len();
        let pad = spec.width.saturating_sub(n);
        if spec.left {
            self.ascii(prefix);
            self.ascii(body);
            self.repeat(b' ', pad);
        } else if spec.zero && zero_ok {
            self.ascii(prefix);
            self.repeat(b'0', pad);
            self.ascii(body);
        } else {
            self.repeat(b' ', pad);
            self.ascii(prefix);
            self.ascii(body);
        }
    }

    /// Units of another width converted to ours (UTF-8 <-> UTF-16).
    fn units<S: Ch>(&mut self, s: &[S]) {
        if size_of::<S>() == size_of::<C>() {
            for &c in s {
                self.put(C::from_u32(c.to_u32()));
            }
        } else if size_of::<S>() == 1 {
            let s = unsafe { core::slice::from_raw_parts(s.as_ptr() as *const u8, s.len()) };
            utf8_to_utf16(s, |u| self.put(C::from_u32(u as u32)));
        } else {
            let s = unsafe { core::slice::from_raw_parts(s.as_ptr() as *const u16, s.len()) };
            utf16_to_utf8(s, |b| self.put(C::from_u32(b as u32)));
        }
    }

    fn string<S: Ch>(&mut self, spec: &Spec, s: &[S]) {
        // Width counts output units.
        let n = if size_of::<S>() == size_of::<C>() {
            s.len()
        } else {
            let mut counter = Writer::<C> { out: &mut Discard, count: 0 };
            counter.units(s);
            counter.count
        };
        let pad = spec.width.saturating_sub(n);
        if !spec.left {
            self.repeat(b' ', pad);
        }
        self.units(s);
        if spec.left {
            self.repeat(b' ', pad);
        }
    }
}

struct Discard;

impl<C> Out<C> for Discard {
    fn put(&mut self, _: C) {}
}

const NULL_TEXT: &[u8] = b"(null)";
const NULL_WIDE: &[u16] = &[0x28, 0x6E, 0x75, 0x6C, 0x6C, 0x29];

/// Format `fmt` with the arguments in `va` into `out`. Returns the number
/// of units written.
pub unsafe fn format<C: Ch>(out: &mut dyn Out<C>, fmt: *const C, mut va: VaList, options: u64) -> i32 {
    let wide_fn = size_of::<C>() == 2;
    let legacy_wide = options & LEGACY_WIDE_SPECIFIERS != 0;
    let three_digits = options & LEGACY_THREE_DIGIT_EXPONENTS != 0;
    let mut w = Writer { out, count: 0 };
    let at = |i: usize| unsafe { (*fmt.add(i)).to_u32() };
    let mut i = 0;
    loop {
        let c = at(i);
        if c == 0 {
            break;
        }
        i += 1;
        if c != b'%' as u32 {
            w.put(C::from_u32(c));
            continue;
        }
        let start = i - 1;

        let mut spec = Spec::default();
        loop {
            match at(i) {
                0x2D => spec.left = true,  // '-'
                0x2B => spec.plus = true,  // '+'
                0x20 => spec.space = true, // ' '
                0x23 => spec.alt = true,   // '#'
                0x30 => spec.zero = true,  // '0'
                _ => break,
            }
            i += 1;
        }
        if at(i) == b'*' as u32 {
            let v = unsafe { va.next_u64() } as i32;
            if v < 0 {
                spec.left = true;
            }
            spec.width = v.unsigned_abs() as usize;
            i += 1;
        } else {
            while (0x30..=0x39).contains(&at(i)) {
                spec.width = spec.width.saturating_mul(10).saturating_add((at(i) - 0x30) as usize);
                i += 1;
            }
        }
        if at(i) == b'.' as u32 {
            i += 1;
            if at(i) == b'*' as u32 {
                let v = unsafe { va.next_u64() } as i32;
                spec.precision = if v < 0 { None } else { Some(v as usize) };
                i += 1;
            } else {
                let mut p = 0usize;
                while (0x30..=0x39).contains(&at(i)) {
                    p = p.saturating_mul(10).saturating_add((at(i) - 0x30) as usize);
                    i += 1;
                }
                spec.precision = Some(p);
            }
        }

        let mut size = Size::Default;
        let mut narrow_arg = false; // h on c/s
        match at(i) {
            0x68 => {
                // h, hh
                i += 1;
                size = Size::Short;
                narrow_arg = true;
                if at(i) == 0x68 {
                    i += 1;
                    size = Size::Char;
                }
            }
            0x6C => {
                // l, ll
                i += 1;
                size = Size::Long;
                if at(i) == 0x6C {
                    i += 1;
                    size = Size::Wide64;
                }
            }
            0x77 => {
                i += 1;
                size = Size::Long;
            }
            0x4C | 0x6A | 0x7A | 0x74 => {
                // L j z t
                i += 1;
                size = Size::Wide64;
            }
            0x49 => {
                // I, I32, I64
                i += 1;
                if at(i) == b'3' as u32 && at(i + 1) == b'2' as u32 {
                    i += 2;
                    size = Size::Default;
                } else if at(i) == b'6' as u32 && at(i + 1) == b'4' as u32 {
                    i += 2;
                    size = Size::Wide64;
                } else {
                    size = Size::Wide64;
                }
            }
            0x54 => i += 1, // T: the function's own width
            _ => {}
        }

        let conv = at(i);
        i += 1;
        // Does %s / %c take a wide argument here?
        let wide_arg = |upper_form: bool| {
            if size == Size::Long {
                true
            } else if narrow_arg {
                false
            } else if wide_fn && legacy_wide {
                !upper_form
            } else {
                upper_form
            }
        };

        match conv as u8 {
            b'd' | b'i' => {
                let raw = unsafe { va.next_u64() };
                let v = match size {
                    Size::Char => raw as i8 as i64,
                    Size::Short => raw as i16 as i64,
                    Size::Default | Size::Long => raw as i32 as i64,
                    Size::Wide64 => raw as i64,
                };
                let sign: &[u8] = if v < 0 {
                    b"-"
                } else if spec.plus {
                    b"+"
                } else if spec.space {
                    b" "
                } else {
                    b""
                };
                integer(&mut w, &spec, v.unsigned_abs(), 10, false, sign);
            }
            b'u' | b'o' | b'x' | b'X' => {
                let raw = unsafe { va.next_u64() };
                let v = match size {
                    Size::Char => raw as u8 as u64,
                    Size::Short => raw as u16 as u64,
                    Size::Default | Size::Long => raw as u32 as u64,
                    Size::Wide64 => raw,
                };
                let (base, upper) = match conv as u8 {
                    b'u' => (10, false),
                    b'o' => (8, false),
                    b'x' => (16, false),
                    _ => (16, true),
                };
                let prefix: &[u8] = match (spec.alt && v != 0, conv as u8) {
                    (true, b'x') => b"0x",
                    (true, b'X') => b"0X",
                    _ => b"",
                };
                integer(&mut w, &spec, v, base, upper, prefix);
            }
            b'p' => {
                let v = unsafe { va.next_u64() };
                let mut s = spec;
                s.precision = Some(16);
                integer(&mut w, &s, v, 16, true, b"");
            }
            b'c' | b'C' => {
                let v = unsafe { va.next_u64() };
                let spec = Spec { precision: None, ..spec };
                if wide_arg(conv == b'C' as u32) {
                    w.string(&spec, &[v as u16]);
                } else {
                    let b = v as u8;
                    if b >= 0x80 && wide_fn {
                        w.string(&spec, &[b as u16]); // a lone byte: Latin-1
                    } else {
                        w.string(&spec, &[b]);
                    }
                }
            }
            b's' | b'S' => {
                let p = unsafe { va.next_u64() } as usize;
                let max = spec.precision.unwrap_or(usize::MAX);
                if wide_arg(conv == b'S' as u32) {
                    let s: &[u16] = if p == 0 {
                        NULL_WIDE
                    } else {
                        unsafe { core::slice::from_raw_parts(p as *const u16, nlen(p as *const u16, max)) }
                    };
                    w.string(&spec, &s[..s.len().min(max)]);
                } else {
                    let s: &[u8] = if p == 0 {
                        NULL_TEXT
                    } else {
                        unsafe { core::slice::from_raw_parts(p as *const u8, nlen(p as *const u8, max)) }
                    };
                    w.string(&spec, &s[..s.len().min(max)]);
                }
            }
            b'Z' => {
                // ANSI_STRING / UNICODE_STRING: u16 byte length, u16 max, buffer.
                let p = unsafe { va.next_u64() } as usize;
                let wide = wide_arg(false);
                if p == 0 || unsafe { *((p + 8) as *const usize) } == 0 {
                    w.string(&spec, NULL_TEXT);
                } else {
                    let bytes = unsafe { *(p as *const u16) } as usize;
                    let buf = unsafe { *((p + 8) as *const usize) };
                    if wide {
                        w.string(&spec, unsafe { core::slice::from_raw_parts(buf as *const u16, bytes / 2) });
                    } else {
                        w.string(&spec, unsafe { core::slice::from_raw_parts(buf as *const u8, bytes) });
                    }
                }
            }
            b'e' | b'E' | b'f' | b'F' | b'g' | b'G' | b'a' | b'A' => {
                let v = unsafe { va.next_f64() };
                float(&mut w, &spec, v, conv as u8, three_digits);
            }
            b'n' => {
                let _ = unsafe { va.next_u64() };
            }
            b'%' if i == start + 2 => w.put(C::from_u32(b'%' as u32)),
            _ => {
                // Not a conversion: print it as written.
                for k in start..i {
                    if at(k) == 0 {
                        break;
                    }
                    w.put(C::from_u32(at(k)));
                }
                if conv == 0 {
                    break;
                }
            }
        }
    }
    w.count as i32
}

fn integer<C: Ch>(w: &mut Writer<C>, spec: &Spec, mut v: u64, base: u64, upper: bool, prefix: &[u8]) {
    let mut digits = [0u8; 64];
    let mut n = 0;
    while v != 0 {
        let d = (v % base) as u8;
        digits[63 - n] = if d < 10 {
            b'0' + d
        } else if upper {
            b'A' + d - 10
        } else {
            b'a' + d - 10
        };
        n += 1;
        v /= base;
    }
    let want = spec.precision.unwrap_or(1);
    let mut body = [b'0'; 600];
    let zeros = want.min(500).saturating_sub(n);
    let mut len = zeros;
    body[len..len + n].copy_from_slice(&digits[64 - n..]);
    len += n;
    // '#' with octal: make sure it starts with 0.
    if base == 8 && spec.alt && (len == 0 || body[0] != b'0') {
        body.copy_within(0..len, 1);
        body[0] = b'0';
        len += 1;
    }
    w.padded(spec, prefix, &body[..len], spec.precision.is_none());
}

/// `{:.*e}` as C writes it: "d.ddde+XX" (or three exponent digits).
fn exponent_form(t: &mut Text, v: f64, precision: usize, upper: bool, alt: bool, three: bool) {
    let mut raw = Text::new();
    let _ = write!(raw, "{:.*e}", precision, v);
    let bytes = raw.bytes();
    let e = bytes.iter().position(|&b| b == b'e').unwrap_or(bytes.len());
    for &b in &bytes[..e] {
        t.push(b);
    }
    if alt && precision == 0 {
        t.push(b'.');
    }
    let exp: i32 = core::str::from_utf8(&bytes[e + 1..]).ok().and_then(|s| s.parse().ok()).unwrap_or(0);
    t.push(if upper { b'E' } else { b'e' });
    t.push(if exp < 0 { b'-' } else { b'+' });
    let _ = if three { write!(t, "{:03}", exp.unsigned_abs()) } else { write!(t, "{:02}", exp.unsigned_abs()) };
}

/// The decimal exponent %e would print for `v` at `precision`.
fn decimal_exponent(v: f64, precision: usize) -> i32 {
    if v == 0.0 {
        return 0;
    }
    let mut raw = Text::new();
    let _ = write!(raw, "{:.*e}", precision, v);
    let bytes = raw.bytes();
    let e = bytes.iter().position(|&b| b == b'e').unwrap_or(0);
    core::str::from_utf8(&bytes[e + 1..]).ok().and_then(|s| s.parse().ok()).unwrap_or(0)
}

/// Drop a fraction's trailing zeros (and a bare '.') before any exponent.
fn strip_zeros(t: &mut Text) {
    let bytes = t.bytes();
    let Some(dot) = bytes.iter().position(|&b| b == b'.') else { return };
    let e = bytes.iter().position(|&b| b == b'e' || b == b'E').unwrap_or(bytes.len());
    let mut end = e;
    while end > dot + 1 && bytes[end - 1] == b'0' {
        end -= 1;
    }
    if end == dot + 1 {
        end = dot;
    }
    let mut out = Text::new();
    let _ = out.write_str(unsafe { core::str::from_utf8_unchecked(&bytes[..end]) });
    let _ = out.write_str(unsafe { core::str::from_utf8_unchecked(&bytes[e..]) });
    *t = out;
}

fn hex_form(t: &mut Text, v: f64, precision: Option<usize>, upper: bool, alt: bool) {
    let bits = v.to_bits();
    let raw_exp = ((bits >> 52) & 0x7FF) as i32;
    let mut frac = bits & ((1 << 52) - 1);
    let mut lead = if raw_exp == 0 { 0u64 } else { 1 };
    let exp = if raw_exp == 0 { if frac == 0 { 0 } else { -1022 } } else { raw_exp - 1023 };
    let p = precision.unwrap_or(13);
    if p < 13 {
        let shift = 52 - 4 * p as u32;
        let rem = frac & ((1u64 << shift) - 1);
        let half = 1u64 << (shift - 1);
        frac >>= shift;
        if rem > half || (rem == half && frac & 1 == 1) {
            frac += 1;
            if frac >> (4 * p) != 0 {
                frac &= (1u64 << (4 * p)) - 1;
                lead += 1;
            }
        }
    }
    let _ = write!(t, "{}", lead);
    if p > 0 || alt {
        t.push(b'.');
    }
    let shown = p.min(13);
    for k in 0..shown {
        let d = ((frac >> (4 * (shown - 1 - k))) & 0xF) as u8;
        t.push(if d < 10 { b'0' + d } else if upper { b'A' + d - 10 } else { b'a' + d - 10 });
    }
    for _ in shown..p {
        t.push(b'0');
    }
    t.push(if upper { b'P' } else { b'p' });
    let _ = write!(t, "{}{}", if exp < 0 { '-' } else { '+' }, exp.unsigned_abs());
}

fn float<C: Ch>(w: &mut Writer<C>, spec: &Spec, v: f64, conv: u8, three: bool) {
    let upper = conv.is_ascii_uppercase();
    let negative = v.is_sign_negative();
    let sign: &[u8] = if negative {
        b"-"
    } else if spec.plus {
        b"+"
    } else if spec.space {
        b" "
    } else {
        b""
    };
    if !v.is_finite() {
        let body: &[u8] = if v.is_infinite() {
            if upper { b"INF" } else { b"inf" }
        } else {
            let quiet = v.to_bits() & (1 << 51) != 0;
            let payload = v.to_bits() & ((1 << 51) - 1);
            match (quiet, negative && payload == 0) {
                (false, _) => if upper { b"NAN(SNAN)" } else { b"nan(snan)" },
                (true, true) => if upper { b"NAN(IND)" } else { b"nan(ind)" },
                _ => if upper { b"NAN" } else { b"nan" },
            }
        };
        w.padded(spec, sign, body, false);
        return;
    }
    let a = v.abs();
    let mut t = Text::new();
    match conv {
        b'f' | b'F' => {
            let p = spec.precision.unwrap_or(6);
            let _ = write!(t, "{:.*}", p, a);
            if spec.alt && p == 0 {
                t.push(b'.');
            }
        }
        b'e' | b'E' => exponent_form(&mut t, a, spec.precision.unwrap_or(6), upper, spec.alt, three),
        b'g' | b'G' => {
            let p = spec.precision.unwrap_or(6).max(1);
            let x = decimal_exponent(a, p - 1);
            if x < p as i32 && x >= -4 {
                let _ = write!(t, "{:.*}", (p as i32 - 1 - x) as usize, a);
                if spec.alt && !t.bytes().contains(&b'.') {
                    t.push(b'.');
                }
            } else {
                exponent_form(&mut t, a, p - 1, upper, spec.alt, three);
            }
            if !spec.alt {
                strip_zeros(&mut t);
            }
        }
        _ => {
            hex_form(&mut t, a, spec.precision, upper, spec.alt);
            let prefix: &[u8] = if upper { b"0X" } else { b"0x" };
            let mut full = [0u8; 3];
            full[..sign.len()].copy_from_slice(sign);
            full[sign.len()..sign.len() + 2].copy_from_slice(prefix);
            w.padded(spec, &full[..sign.len() + 2], t.bytes(), true);
            return;
        }
    }
    w.padded(spec, sign, t.bytes(), true);
}
