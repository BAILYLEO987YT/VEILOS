// The C runtime ntoskrnl exports beyond the basics in rtl.rs: bounded and
// "_s" string functions, the sprintf family (narrow and wide), sqrt and
// _purecall. Formatting is one engine (`format_core`) for every printf in
// ntos, DbgPrint included.

use alloc::{string::String, vec::Vec};

use super::rtl::{strlen, wcslen};

// -----| the printf engine |-----

/// printf as drivers use it, over a format string of `chars` and
/// arguments in an array (the Windows x64 varargs layout). `wide` is a
/// wide-character printf: plain %s and %c then mean wide, %S and %C
/// narrow (h and l/w force either). Also %Z / %wZ (counted strings).
pub fn format_core(fmt: &[char], args: *const u64, wide: bool) -> String {
    use core::fmt::Write;
    let mut out = String::new();
    let mut ai = 0usize;
    let mut next = || {
        let v = unsafe { args.add(ai).read() };
        ai += 1;
        v
    };
    let at = |k: usize| fmt.get(k).copied().unwrap_or('\0');
    let mut i = 0usize;
    while i < fmt.len() {
        let c = fmt[i];
        i += 1;
        if c != '%' {
            out.push(c);
            continue;
        }
        let (mut left, mut zero, mut plus, mut alt, mut space) = (false, false, false, false, false);
        loop {
            match at(i) {
                '-' => left = true,
                '0' => zero = true,
                '+' => plus = true,
                '#' => alt = true,
                ' ' => space = true,
                _ => break,
            }
            i += 1;
        }
        let mut width = 0usize;
        if at(i) == '*' {
            let w = next() as i32;
            if w < 0 {
                left = true;
            }
            width = w.unsigned_abs() as usize;
            i += 1;
        }
        while at(i).is_ascii_digit() {
            width = width * 10 + at(i) as usize - '0' as usize;
            i += 1;
        }
        let mut prec: Option<usize> = None;
        if at(i) == '.' {
            i += 1;
            let mut p = 0;
            if at(i) == '*' {
                p = next() as i32 as usize;
                i += 1;
            }
            while at(i).is_ascii_digit() {
                p = p * 10 + at(i) as usize - '0' as usize;
                i += 1;
            }
            prec = Some(p);
        }
        // Sizes as MSVC has them: `long` (l) is 32 bits; ll, I64, z, t, j
        // and I are 64. A single l also makes %s and %c wide.
        let (mut long, mut force_narrow, mut force_wide) = (false, false, false);
        loop {
            match at(i) {
                'l' => {
                    if at(i + 1) == 'l' {
                        long = true;
                        i += 1;
                    } else {
                        force_wide = true;
                    }
                }
                'L' | 'q' | 'j' | 'z' | 't' => long = true,
                'h' => force_narrow = true,
                'w' => force_wide = true,
                'I' => {
                    if at(i + 1) == '6' && at(i + 2) == '4' {
                        i += 2;
                        long = true;
                    } else if at(i + 1) == '3' && at(i + 2) == '2' {
                        i += 2;
                    } else {
                        long = true; // %Ix: pointer-sized
                    }
                }
                _ => break,
            }
            i += 1;
        }
        let conv = at(i);
        if conv == '\0' {
            break;
        }
        i += 1;
        let is_wide = |upper: bool| {
            if force_narrow {
                false
            } else if force_wide {
                true
            } else {
                wide != upper
            }
        };
        let num_prefix = |v: i64| if plus && v >= 0 { "+" } else if space && v >= 0 { " " } else { "" };
        let body: String = match conv {
            '%' => String::from("%"),
            'd' | 'i' => {
                let v = next();
                let v = if long { v as i64 } else { v as i32 as i64 };
                let mut s = alloc::format!("{}{}", num_prefix(v), v.unsigned_abs());
                if v < 0 {
                    s = alloc::format!("-{}", v.unsigned_abs());
                }
                if let Some(p) = prec {
                    let digits = s.trim_start_matches(['-', '+', ' ']).len();
                    if digits < p {
                        let sign_len = s.len() - digits;
                        s.insert_str(sign_len, &"0".repeat(p - digits));
                    }
                }
                s
            }
            'u' | 'x' | 'X' | 'o' => {
                let v = next();
                let v = if long { v } else { v as u32 as u64 };
                let mut s = String::new();
                let _ = match conv {
                    'u' => write!(s, "{}", v),
                    'x' => write!(s, "{}{:x}", if alt && v != 0 { "0x" } else { "" }, v),
                    'X' => write!(s, "{}{:X}", if alt && v != 0 { "0X" } else { "" }, v),
                    _ => write!(s, "{:o}", v),
                };
                if let Some(p) = prec {
                    let digits = s.trim_start_matches("0x").trim_start_matches("0X").len();
                    if digits < p {
                        let at = s.len() - digits;
                        s.insert_str(at, &"0".repeat(p - digits));
                    }
                }
                s
            }
            'p' => alloc::format!("{:016X}", next()),
            'c' | 'C' => {
                let v = next();
                let ch = if is_wide(conv == 'C') { v as u16 as u32 } else { v as u8 as u32 };
                char::from_u32(ch).map(String::from).unwrap_or_default()
            }
            's' | 'S' => {
                let p = next();
                if p == 0 {
                    String::from("(null)")
                } else if is_wide(conv == 'S') {
                    let s = p as *const u16;
                    let n = wcslen(s).min(prec.unwrap_or(usize::MAX));
                    char::decode_utf16(unsafe { core::slice::from_raw_parts(s, n) }.iter().copied()).map(|c| c.unwrap_or('?')).collect()
                } else {
                    let s = p as *const u8;
                    let n = strlen(s).min(prec.unwrap_or(usize::MAX));
                    unsafe { core::slice::from_raw_parts(s, n) }.iter().map(|&b| b as char).collect()
                }
            }
            'Z' => {
                let p = next() as *const u8;
                if p.is_null() {
                    String::from("(null)")
                } else if force_wide {
                    super::rtl::unicode_to_string(p)
                } else {
                    unsafe {
                        let n = (p as *const u16).read_unaligned() as usize;
                        let b = (p.add(8) as *const *const u8).read_unaligned();
                        core::slice::from_raw_parts(b, n).iter().map(|&b| b as char).collect()
                    }
                }
            }
            'n' => {
                let p = next() as *mut i32;
                if !p.is_null() {
                    unsafe { p.write(out.chars().count() as i32) };
                }
                String::new()
            }
            'f' | 'e' | 'g' | 'F' | 'E' | 'G' => {
                // Floating point in a kernel printf is rare; print it plainly.
                let v = f64::from_bits(next());
                alloc::format!("{:.*}", prec.unwrap_or(6), v)
            }
            other => {
                let _ = next();
                alloc::format!("%{}", other)
            }
        };
        let pad = width.saturating_sub(body.chars().count());
        if left {
            out.push_str(&body);
            out.extend(core::iter::repeat(' ').take(pad));
        } else {
            let numeric = matches!(conv, 'd' | 'i' | 'u' | 'x' | 'X' | 'o' | 'p');
            if zero && numeric && prec.is_none() {
                // Zeros go after the sign or 0x prefix.
                let sign = body.chars().take_while(|c| matches!(c, '-' | '+' | ' ')).count();
                let pre = if body[sign..].starts_with("0x") || body[sign..].starts_with("0X") { sign + 2 } else { sign };
                out.push_str(&body[..pre]);
                out.extend(core::iter::repeat('0').take(pad));
                out.push_str(&body[pre..]);
            } else {
                out.extend(core::iter::repeat(' ').take(pad));
                out.push_str(&body);
            }
        }
    }
    out
}

fn narrow_chars(p: *const u8) -> Vec<char> {
    let n = strlen(p);
    unsafe { core::slice::from_raw_parts(p, n) }.iter().map(|&b| b as char).collect()
}

fn wide_chars(p: *const u16) -> Vec<char> {
    let n = wcslen(p);
    char::decode_utf16(unsafe { core::slice::from_raw_parts(p, n) }.iter().copied()).map(|c| c.unwrap_or('?')).collect()
}

/// Write `text` to a narrow buffer of `cap` chars, at most `count` of them
/// (not counting the terminator). Returns (chars written, all fitted).
unsafe fn put_narrow(buf: *mut u8, cap: usize, count: usize, text: &str, terminate: bool) -> (usize, bool) {
    let bytes: Vec<u8> = text.chars().map(|c| if (c as u32) < 256 { c as u8 } else { b'?' }).collect();
    let room = count.min(cap);
    let n = bytes.len().min(room);
    unsafe {
        core::ptr::copy_nonoverlapping(bytes.as_ptr(), buf, n);
        if terminate && cap > 0 {
            buf.add(n.min(cap - 1)).write(0);
        } else if n < cap {
            buf.add(n).write(0);
        }
    }
    (n, bytes.len() <= room && (!terminate || bytes.len() < cap))
}

unsafe fn put_wide(buf: *mut u16, cap: usize, count: usize, text: &str, terminate: bool) -> (usize, bool) {
    let units: Vec<u16> = text.encode_utf16().collect();
    let room = count.min(cap);
    let n = units.len().min(room);
    unsafe {
        core::ptr::copy_nonoverlapping(units.as_ptr(), buf, n);
        if terminate && cap > 0 {
            buf.add(n.min(cap - 1)).write(0);
        } else if n < cap {
            buf.add(n).write(0);
        }
    }
    (n, units.len() <= room && (!terminate || units.len() < cap))
}

const TRUNCATE: usize = usize::MAX;

/// _vsnprintf(buffer, count, format, va): -1 if it didn't fit (then not
/// terminated), else the length.
pub extern "win64" fn _vsnprintf(buf: *mut u8, count: usize, fmt: *const u8, va: *const u64) -> i32 {
    let s = format_core(&narrow_chars(fmt), va, false);
    let (n, fits) = unsafe { put_narrow(buf, count, count, &s, false) };
    if fits { n as i32 } else { -1 }
}

pub extern "win64" fn _vsnwprintf(buf: *mut u16, count: usize, fmt: *const u16, va: *const u64) -> i32 {
    let s = format_core(&wide_chars(fmt), va, true);
    let (n, fits) = unsafe { put_wide(buf, count, count, &s, false) };
    if fits { n as i32 } else { -1 }
}

/// _vsnprintf_s(buffer, sizeOfBuffer, count, format, va): always
/// terminated; -1 when cut short (count = _TRUNCATE or too small).
pub extern "win64" fn _vsnprintf_s(buf: *mut u8, size: usize, count: usize, fmt: *const u8, va: *const u64) -> i32 {
    if buf.is_null() || size == 0 {
        return -1;
    }
    let s = format_core(&narrow_chars(fmt), va, false);
    let limit = if count == TRUNCATE { size - 1 } else { count.min(size - 1) };
    let (n, fits) = unsafe { put_narrow(buf, size, limit, &s, true) };
    if fits { n as i32 } else { -1 }
}

pub extern "win64" fn _vsnwprintf_s(buf: *mut u16, size: usize, count: usize, fmt: *const u16, va: *const u64) -> i32 {
    if buf.is_null() || size == 0 {
        return -1;
    }
    let s = format_core(&wide_chars(fmt), va, true);
    let limit = if count == TRUNCATE { size - 1 } else { count.min(size - 1) };
    let (n, fits) = unsafe { put_wide(buf, size, limit, &s, true) };
    if fits { n as i32 } else { -1 }
}

// The variadic ones: `varargs!` spills the register arguments into their
// home slots so every argument is one array, then calls the implementation
// with it (fixed arguments first, the variable ones after).
macro_rules! varargs {
    ($name:ident, $imp:path) => {
        #[unsafe(naked)]
        pub unsafe extern "win64" fn $name() {
            core::arch::naked_asm!(
                "mov [rsp + 8], rcx",
                "mov [rsp + 16], rdx",
                "mov [rsp + 24], r8",
                "mov [rsp + 32], r9",
                "lea rcx, [rsp + 8]",
                "sub rsp, 40",
                "call {imp}",
                "add rsp, 40",
                "ret",
                imp = sym $imp,
            )
        }
    };
}

/// sprintf_s(buffer, sizeOfBuffer, format, ...): empty and -1 if it
/// doesn't fit.
extern "win64" fn sprintf_s_impl(a: *const u64) -> i32 {
    unsafe {
        let (buf, size, fmt) = (*a as *mut u8, *a.add(1) as usize, *a.add(2) as *const u8);
        if buf.is_null() || size == 0 {
            return -1;
        }
        let s = format_core(&narrow_chars(fmt), a.add(3), false);
        if s.len() >= size {
            buf.write(0);
            return -1;
        }
        put_narrow(buf, size, size, &s, true).0 as i32
    }
}
varargs!(sprintf_s, sprintf_s_impl);

extern "win64" fn swprintf_s_impl(a: *const u64) -> i32 {
    unsafe {
        let (buf, size, fmt) = (*a as *mut u16, *a.add(1) as usize, *a.add(2) as *const u16);
        if buf.is_null() || size == 0 {
            return -1;
        }
        let s = format_core(&wide_chars(fmt), a.add(3), true);
        if s.encode_utf16().count() >= size {
            buf.write(0);
            return -1;
        }
        put_wide(buf, size, size, &s, true).0 as i32
    }
}
varargs!(swprintf_s, swprintf_s_impl);

extern "win64" fn snprintf_s_impl(a: *const u64) -> i32 {
    unsafe { _vsnprintf_s(*a as *mut u8, *a.add(1) as usize, *a.add(2) as usize, *a.add(3) as *const u8, a.add(4)) }
}
varargs!(_snprintf_s, snprintf_s_impl);

extern "win64" fn snwprintf_s_impl(a: *const u64) -> i32 {
    unsafe { _vsnwprintf_s(*a as *mut u16, *a.add(1) as usize, *a.add(2) as usize, *a.add(3) as *const u16, a.add(4)) }
}
varargs!(_snwprintf_s, snwprintf_s_impl);

extern "win64" fn snprintf_impl(a: *const u64) -> i32 {
    unsafe { _vsnprintf(*a as *mut u8, *a.add(1) as usize, *a.add(2) as *const u8, a.add(3)) }
}
varargs!(_snprintf, snprintf_impl);

extern "win64" fn snwprintf_impl(a: *const u64) -> i32 {
    unsafe { _vsnwprintf(*a as *mut u16, *a.add(1) as usize, *a.add(2) as *const u16, a.add(3)) }
}
varargs!(_snwprintf, snwprintf_impl);

// -----| strings |-----

const EINVAL: i32 = 22;
const ERANGE: i32 = 34;
const STRUNCATE: i32 = 80;

/// strcpy_s and friends over either character width.
unsafe fn copy_s<T: Copy + Default + PartialEq>(dst: *mut T, size: usize, src: *const T, max: usize, append: bool) -> i32 {
    if dst.is_null() || size == 0 {
        return EINVAL;
    }
    if src.is_null() {
        unsafe { dst.write(T::default()) };
        return EINVAL;
    }
    unsafe {
        let zero = T::default();
        let mut at = 0;
        if append {
            while at < size && *dst.add(at) != zero {
                at += 1;
            }
            if at == size {
                dst.write(zero);
                return EINVAL;
            }
        }
        let truncate = max == TRUNCATE;
        let mut k = 0;
        while (truncate || k < max) && *src.add(k) != zero {
            if at + k + 1 >= size {
                if truncate {
                    dst.add(size - 1).write(zero);
                    return STRUNCATE;
                }
                dst.write(zero);
                return ERANGE;
            }
            dst.add(at + k).write(*src.add(k));
            k += 1;
        }
        dst.add(at + k).write(zero);
    }
    0
}

pub extern "win64" fn strcpy_s(d: *mut u8, size: usize, s: *const u8) -> i32 {
    unsafe { copy_s(d, size, s, TRUNCATE - 1, false) }
}
pub extern "win64" fn strncpy_s(d: *mut u8, size: usize, s: *const u8, n: usize) -> i32 {
    unsafe { copy_s(d, size, s, n, false) }
}
pub extern "win64" fn strcat_s(d: *mut u8, size: usize, s: *const u8) -> i32 {
    unsafe { copy_s(d, size, s, TRUNCATE - 1, true) }
}
pub extern "win64" fn strncat_s(d: *mut u8, size: usize, s: *const u8, n: usize) -> i32 {
    unsafe { copy_s(d, size, s, n, true) }
}
pub extern "win64" fn wcscpy_s(d: *mut u16, size: usize, s: *const u16) -> i32 {
    unsafe { copy_s(d, size, s, TRUNCATE - 1, false) }
}
pub extern "win64" fn wcsncpy_s(d: *mut u16, size: usize, s: *const u16, n: usize) -> i32 {
    unsafe { copy_s(d, size, s, n, false) }
}
pub extern "win64" fn wcscat_s(d: *mut u16, size: usize, s: *const u16) -> i32 {
    unsafe { copy_s(d, size, s, TRUNCATE - 1, true) }
}
pub extern "win64" fn wcsncat_s(d: *mut u16, size: usize, s: *const u16, n: usize) -> i32 {
    unsafe { copy_s(d, size, s, n, true) }
}

pub extern "win64" fn strnlen(s: *const u8, max: usize) -> usize {
    (0..max).find(|&i| unsafe { s.add(i).read() } == 0).unwrap_or(max)
}
pub extern "win64" fn wcsnlen(s: *const u16, max: usize) -> usize {
    (0..max).find(|&i| unsafe { s.add(i).read() } == 0).unwrap_or(max)
}

pub extern "win64" fn wcsncmp(a: *const u16, b: *const u16, n: usize) -> i32 {
    for i in 0..n {
        let (x, y) = unsafe { (a.add(i).read(), b.add(i).read()) };
        if x != y || x == 0 {
            return x as i32 - y as i32;
        }
    }
    0
}

pub extern "win64" fn _wcsicmp(a: *const u16, b: *const u16) -> i32 {
    let low = |c: u16| if (b'A' as u16..=b'Z' as u16).contains(&c) { c + 32 } else { c };
    let mut i = 0;
    loop {
        let (x, y) = unsafe { (low(a.add(i).read()), low(b.add(i).read())) };
        if x != y || x == 0 {
            return x as i32 - y as i32;
        }
        i += 1;
    }
}

pub extern "win64" fn _wcsnicmp(a: *const u16, b: *const u16, n: usize) -> i32 {
    let low = |c: u16| if (b'A' as u16..=b'Z' as u16).contains(&c) { c + 32 } else { c };
    for i in 0..n {
        let (x, y) = unsafe { (low(a.add(i).read()), low(b.add(i).read())) };
        if x != y || x == 0 {
            return x as i32 - y as i32;
        }
    }
    0
}

pub extern "win64" fn _strnicmp(a: *const u8, b: *const u8, n: usize) -> i32 {
    for i in 0..n {
        let (x, y) = unsafe { (a.add(i).read().to_ascii_lowercase(), b.add(i).read().to_ascii_lowercase()) };
        if x != y || x == 0 {
            return x as i32 - y as i32;
        }
    }
    0
}

pub extern "win64" fn wcschr(s: *const u16, c: u16) -> *const u16 {
    let mut i = 0;
    loop {
        let v = unsafe { s.add(i).read() };
        if v == c {
            return unsafe { s.add(i) };
        }
        if v == 0 {
            return core::ptr::null();
        }
        i += 1;
    }
}

pub extern "win64" fn wcsrchr(s: *const u16, c: u16) -> *const u16 {
    let n = wcslen(s);
    (0..=n).rev().find(|&i| unsafe { s.add(i).read() } == c).map_or(core::ptr::null(), |i| unsafe { s.add(i) })
}

pub extern "win64" fn strstr(h: *const u8, n: *const u8) -> *const u8 {
    let (hl, nl) = (strlen(h), strlen(n));
    let (hs, ns) = unsafe { (core::slice::from_raw_parts(h, hl), core::slice::from_raw_parts(n, nl)) };
    if nl == 0 {
        return h;
    }
    hs.windows(nl).position(|w| w == ns).map_or(core::ptr::null(), |i| unsafe { h.add(i) })
}

pub extern "win64" fn wcsstr(h: *const u16, n: *const u16) -> *const u16 {
    let (hl, nl) = (wcslen(h), wcslen(n));
    let (hs, ns) = unsafe { (core::slice::from_raw_parts(h, hl), core::slice::from_raw_parts(n, nl)) };
    if nl == 0 {
        return h;
    }
    hs.windows(nl).position(|w| w == ns).map_or(core::ptr::null(), |i| unsafe { h.add(i) })
}

pub extern "win64" fn toupper(c: i32) -> i32 {
    if (b'a' as i32..=b'z' as i32).contains(&c) { c - 32 } else { c }
}

pub extern "win64" fn tolower(c: i32) -> i32 {
    if (b'A' as i32..=b'Z' as i32).contains(&c) { c + 32 } else { c }
}

/// sqrt(x): in XMM0 and out in XMM0, as drivers call it. (Naked: the
/// kernel's own target is soft-float, so a Rust f64 wouldn't be in XMM0.)
#[unsafe(naked)]
pub unsafe extern "win64" fn sqrt() {
    core::arch::naked_asm!("sqrtsd xmm0, xmm0", "ret")
}

/// A pure virtual function was called: R6025, a bug check in kernel mode.
pub extern "win64" fn _purecall() -> i32 {
    super::ke::KeBugCheckEx(0x8E, 0xC000_0420, 0, 0, 0) // STATUS_ASSERTION_FAILURE-ish
}
