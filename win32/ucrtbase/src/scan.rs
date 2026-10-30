//! The scanf engine, over narrow or wide input and format.
//!
//! Options are the UCRT's `_CRT_INTERNAL_SCANF_*` bits: SECURECRT (the _s
//! functions: %s, %c and %[ take a buffer size after the pointer) and
//! LEGACY_WIDE_SPECIFIERS (in wide functions %s means a wide string).

use crate::convert::{parse_float, parse_int, signed, unsigned, utf16_to_utf8, utf8_to_utf16};
use crate::string::Ch;
use crate::{ctype, VaList};
use alloc::vec::Vec;

pub const SECURECRT: u64 = 1;
pub const LEGACY_WIDE_SPECIFIERS: u64 = 2;

/// Where scanned input comes from, a unit at a time.
pub trait In {
    /// The next unit, or None at the end.
    fn get(&mut self) -> Option<u32>;
    /// Give back the unit `get` just returned.
    fn unget(&mut self, c: u32);
}

/// Input from a string of at most `limit` units.
pub struct StrIn<C> {
    pub p: *const C,
    pub pos: usize,
    pub limit: usize,
}

impl<C: Ch> In for StrIn<C> {
    fn get(&mut self) -> Option<u32> {
        if self.pos >= self.limit {
            return None;
        }
        let c = unsafe { *self.p.add(self.pos) }.to_u32();
        if c == 0 {
            return None;
        }
        self.pos += 1;
        Some(c)
    }
    fn unget(&mut self, _c: u32) {
        self.pos -= 1;
    }
}

/// Counts what was consumed (for %n) and remembers whether input ran out.
struct Reader<'a> {
    input: &'a mut dyn In,
    consumed: usize,
    at_end: bool,
}

impl Reader<'_> {
    fn get(&mut self) -> Option<u32> {
        let c = self.input.get();
        match c {
            Some(_) => self.consumed += 1,
            None => self.at_end = true,
        }
        c
    }
    fn unget(&mut self, c: u32) {
        self.input.unget(c);
        self.consumed -= 1;
    }
    fn skip_space(&mut self) {
        while let Some(c) = self.get() {
            if !ctype::is_space(c) {
                self.unget(c);
                break;
            }
        }
    }
    /// The next unit if `ok` accepts it (and `left` > 0).
    fn take_if(&mut self, left: &mut usize, ok: impl Fn(u32) -> bool) -> Option<u32> {
        if *left == 0 {
            return None;
        }
        let c = self.get()?;
        if ok(c) {
            *left -= 1;
            Some(c)
        } else {
            self.unget(c);
            None
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Size {
    Default,
    Char,
    Short,
    Long,
    Wide64,
}

fn is_digit(c: u32, base: u32) -> bool {
    let d = match c {
        0x30..=0x39 => c - 0x30,
        0x41..=0x5A => c - 0x41 + 10,
        0x61..=0x7A => c - 0x61 + 10,
        _ => 99,
    };
    d < base
}

/// Scan `input` by `fmt`, storing through the pointers in `va`. Returns the
/// number of fields assigned, or -1 if the input ended before the first
/// conversion.
pub unsafe fn scan<C: Ch>(input: &mut dyn In, fmt: *const C, mut va: VaList, options: u64) -> i32 {
    let wide_fn = size_of::<C>() == 2;
    let secure = options & SECURECRT != 0;
    let legacy_wide = options & LEGACY_WIDE_SPECIFIERS != 0;
    let mut r = Reader { input, consumed: 0, at_end: false };
    let at = |i: usize| unsafe { (*fmt.add(i)).to_u32() };
    let mut assigned = 0;
    let mut converted = 0;
    let mut i = 0;
    let fail = |r: &Reader, assigned: i32, converted: i32| if converted == 0 && r.at_end { -1 } else { assigned };

    loop {
        let f = at(i);
        if f == 0 {
            break;
        }
        i += 1;
        if ctype::is_space(f) {
            r.skip_space();
            continue;
        }
        if f != b'%' as u32 || at(i) == b'%' as u32 {
            if f == b'%' as u32 {
                i += 1;
                r.skip_space();
            }
            match r.get() {
                Some(c) if c == f => continue,
                Some(c) => {
                    r.unget(c);
                    return assigned;
                }
                None => return fail(&r, assigned, converted),
            }
        }

        let suppress = at(i) == b'*' as u32;
        if suppress {
            i += 1;
        }
        let mut width = 0usize;
        while (0x30..=0x39).contains(&at(i)) {
            width = width * 10 + (at(i) - 0x30) as usize;
            i += 1;
        }
        let mut size = Size::Default;
        let mut narrow_arg = false;
        match at(i) {
            0x68 => {
                i += 1;
                size = Size::Short;
                narrow_arg = true;
                if at(i) == 0x68 {
                    i += 1;
                    size = Size::Char;
                }
            }
            0x6C => {
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
                i += 1;
                size = if at(i - 1) == 0x4C { Size::Long } else { Size::Wide64 };
            }
            0x49 => {
                i += 1;
                if at(i) == b'3' as u32 && at(i + 1) == b'2' as u32 {
                    i += 2;
                } else if at(i) == b'6' as u32 && at(i + 1) == b'4' as u32 {
                    i += 2;
                    size = Size::Wide64;
                } else {
                    size = Size::Wide64;
                }
            }
            _ => {}
        }
        let conv = at(i);
        i += 1;
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
        let mut left = if width == 0 { usize::MAX } else { width };

        match conv as u8 {
            b'd' | b'i' | b'u' | b'o' | b'x' | b'X' | b'p' => {
                r.skip_space();
                let mut base = match conv as u8 {
                    b'd' | b'u' => 10,
                    b'i' => 0,
                    b'o' => 8,
                    _ => 16,
                };
                let mut text: Vec<u8> = Vec::new();
                if let Some(c) = r.take_if(&mut left, |c| c == b'-' as u32 || c == b'+' as u32) {
                    text.push(c as u8);
                }
                if base == 0 || base == 16 {
                    if let Some(z) = r.take_if(&mut left, |c| c == b'0' as u32) {
                        text.push(z as u8);
                        if let Some(x) = r.take_if(&mut left, |c| c | 0x20 == b'x' as u32) {
                            text.push(x as u8);
                            base = 16;
                        } else if base == 0 {
                            base = 8;
                        }
                    } else if base == 0 {
                        base = 10;
                    }
                }
                while let Some(c) = r.take_if(&mut left, |c| is_digit(c, base)) {
                    text.push(c as u8);
                }
                if !text.iter().any(|&b| is_digit(b as u32, base)) {
                    return fail(&r, assigned, converted);
                }
                text.push(0);
                let v = unsafe { parse_int(text.as_ptr(), base) };
                converted += 1;
                if suppress {
                    continue;
                }
                let p = unsafe { va.next_u64() } as usize;
                let signed_conv = matches!(conv as u8, b'd' | b'i');
                let value = if conv == b'p' as u32 {
                    unsigned(&v, 64)
                } else if signed_conv {
                    signed(&v, if size == Size::Wide64 { 64 } else { 32 }) as u64
                } else {
                    unsigned(&v, if size == Size::Wide64 { 64 } else { 32 })
                };
                unsafe {
                    match (size, conv == b'p' as u32) {
                        (_, true) | (Size::Wide64, _) => *(p as *mut u64) = value,
                        (Size::Char, _) => *(p as *mut u8) = value as u8,
                        (Size::Short, _) => *(p as *mut u16) = value as u16,
                        _ => *(p as *mut u32) = value as u32,
                    }
                }
                assigned += 1;
            }
            b'e' | b'E' | b'f' | b'F' | b'g' | b'G' | b'a' | b'A' => {
                r.skip_space();
                let mut text: Vec<u8> = Vec::new();
                // Greedy: the characters a float can be made of, with signs
                // only at the start or after an exponent letter.
                let mut hex = false;
                loop {
                    let prev = text.last().copied().unwrap_or(0) | 0x20;
                    let Some(c) = r.take_if(&mut left, |c| {
                        let l = c | 0x20;
                        (0x30..=0x39).contains(&c)
                            || c == b'.' as u32
                            || ((c == b'+' as u32 || c == b'-' as u32) && (text.is_empty() || prev == if hex { b'p' } else { b'e' }))
                            || (l == b'x' as u32 && prev == b'0')
                            || (hex && (b'a' as u32..=b'f' as u32).contains(&l))
                            || l == if hex { b'p' as u32 } else { b'e' as u32 }
                            || (!hex && b"infatyn()".contains(&(l as u8)) && (l != b'a' as u32 || !text.is_empty()))
                    }) else {
                        break;
                    };
                    if c | 0x20 == b'x' as u32 {
                        hex = true;
                    }
                    text.push(c as u8);
                }
                text.push(0);
                let v = unsafe { parse_float(text.as_ptr(), size != Size::Long) };
                if v.end == 0 {
                    return fail(&r, assigned, converted);
                }
                converted += 1;
                if suppress {
                    continue;
                }
                let p = unsafe { va.next_u64() } as usize;
                unsafe {
                    if size == Size::Long {
                        *(p as *mut f64) = v.value;
                    } else {
                        *(p as *mut f32) = v.value as f32;
                    }
                }
                assigned += 1;
            }
            b's' | b'S' | b'c' | b'C' | b'[' => {
                let is_c = conv | 0x20 == b'c' as u32;
                let mut set: Vec<u32> = Vec::new();
                let mut negate = false;
                if conv == b'[' as u32 {
                    if at(i) == b'^' as u32 {
                        negate = true;
                        i += 1;
                    }
                    if at(i) == b']' as u32 {
                        set.push(b']' as u32);
                        i += 1;
                    }
                    while at(i) != b']' as u32 && at(i) != 0 {
                        if at(i + 1) == b'-' as u32 && at(i + 2) != b']' as u32 && at(i + 2) != 0 {
                            let (a, b) = (at(i).min(at(i + 2)), at(i).max(at(i + 2)));
                            for c in a..=b {
                                set.push(c);
                            }
                            i += 3;
                        } else {
                            set.push(at(i));
                            i += 1;
                        }
                    }
                    if at(i) == b']' as u32 {
                        i += 1;
                    }
                } else if !is_c {
                    r.skip_space();
                }
                if is_c && width == 0 {
                    left = 1;
                }
                let mut got: Vec<u32> = Vec::new();
                while let Some(c) = r.take_if(&mut left, |c| match conv as u8 {
                    b'[' => set.contains(&c) != negate,
                    b's' | b'S' => !ctype::is_space(c),
                    _ => true,
                }) {
                    got.push(c);
                }
                if got.is_empty() || (is_c && left != 0 && left != usize::MAX) {
                    return fail(&r, assigned, converted);
                }
                converted += 1;
                if suppress {
                    continue;
                }
                let p = unsafe { va.next_u64() } as usize;
                let room = if secure { (unsafe { va.next_u64() }) as u32 as usize } else { usize::MAX };
                let wide = wide_arg(conv == b'S' as u32 || conv == b'C' as u32);
                if !unsafe { store(p, room, &got, wide_fn, wide, !is_c) } {
                    return assigned;
                }
                assigned += 1;
            }
            b'n' => {
                if !suppress {
                    let p = unsafe { va.next_u64() } as usize;
                    unsafe { *(p as *mut i32) = r.consumed as i32 };
                }
            }
            _ => return assigned,
        }
    }
    assigned
}

/// Store scanned units (of the input's width) at `p` as narrow or `wide`
/// text, NUL-terminated if `terminate`, in at most `room` units.
unsafe fn store(p: usize, room: usize, got: &[u32], wide_input: bool, wide: bool, terminate: bool) -> bool {
    let mut out: Vec<u32> = Vec::new();
    match (wide_input, wide) {
        (false, true) => {
            let bytes: Vec<u8> = got.iter().map(|&c| c as u8).collect();
            utf8_to_utf16(&bytes, |u| out.push(u as u32));
        }
        (true, false) => {
            let units: Vec<u16> = got.iter().map(|&c| c as u16).collect();
            utf16_to_utf8(&units, |b| out.push(b as u32));
        }
        _ => out.extend_from_slice(got),
    }
    let need = out.len() + terminate as usize;
    if need > room {
        if room > 0 {
            unsafe {
                if wide {
                    *(p as *mut u16) = 0;
                } else {
                    *(p as *mut u8) = 0;
                }
            }
        }
        return false;
    }
    if terminate {
        out.push(0);
    }
    for (k, &c) in out.iter().enumerate() {
        unsafe {
            if wide {
                *(p as *mut u16).add(k) = c as u16;
            } else {
                *(p as *mut u8).add(k) = c as u8;
            }
        }
    }
    true
}
