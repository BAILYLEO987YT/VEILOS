//! Character classes ("C" locale for bytes; Unicode for wide characters)
//! and the class table the CRT's macros index.

pub const UPPER: u16 = 0x1;
pub const LOWER: u16 = 0x2;
pub const DIGIT: u16 = 0x4;
pub const SPACE: u16 = 0x8;
pub const PUNCT: u16 = 0x10;
pub const CONTROL: u16 = 0x20;
pub const BLANK: u16 = 0x40;
pub const HEX: u16 = 0x80;
pub const ALPHA: u16 = 0x100;

const fn class(c: u8) -> u16 {
    match c {
        b'A'..=b'F' => UPPER | ALPHA | HEX,
        b'G'..=b'Z' => UPPER | ALPHA,
        b'a'..=b'f' => LOWER | ALPHA | HEX,
        b'g'..=b'z' => LOWER | ALPHA,
        b'0'..=b'9' => DIGIT | HEX,
        b' ' => SPACE | BLANK,
        b'\t' => CONTROL | SPACE | BLANK,
        b'\n' | 0x0B | 0x0C | b'\r' => CONTROL | SPACE,
        0..=0x1F | 0x7F => CONTROL,
        0x21..=0x7E => PUNCT,
        _ => 0,
    }
}

/// The class table, from EOF (-1) to 255: `_pctype` points at entry 1.
static TABLE: [u16; 257] = {
    let mut t = [0; 257];
    let mut i = 0;
    while i < 256 {
        t[i + 1] = class(i as u8);
        i += 1;
    }
    t
};

/// msvcrt's `_pctype` and `_pwctype` data exports.
#[no_mangle]
pub static mut _pctype: *const u16 = unsafe { TABLE.as_ptr().add(1) };
#[no_mangle]
pub static mut _pwctype: *const u16 = unsafe { TABLE.as_ptr().add(1) };
#[no_mangle]
pub static mut __mb_cur_max: i32 = 1;

#[no_mangle]
pub extern "C" fn __pctype_func() -> *const u16 {
    unsafe { TABLE.as_ptr().add(1) }
}

#[no_mangle]
pub extern "C" fn __pwctype_func() -> *const u16 {
    __pctype_func()
}

#[no_mangle]
pub extern "C" fn __p__pctype() -> *const *const u16 {
    &raw const _pctype
}

#[no_mangle]
pub extern "C" fn __p__pwctype() -> *const *const u16 {
    &raw const _pwctype
}

#[no_mangle]
pub extern "C" fn ___mb_cur_max_func() -> i32 {
    1
}

#[no_mangle]
pub extern "C" fn __p___mb_cur_max() -> *const i32 {
    &raw const __mb_cur_max
}

/// The class bits of `c` (a byte value, EOF, or a wide character).
pub fn classify(c: u32) -> u16 {
    if c < 0x80 {
        return TABLE[c as usize + 1];
    }
    if c == u32::MAX {
        return 0;
    }
    let Some(ch) = char::from_u32(c) else { return 0 };
    let mut bits = 0;
    if ch.is_alphabetic() {
        bits |= ALPHA;
    }
    if ch.is_uppercase() {
        bits |= UPPER;
    }
    if ch.is_lowercase() {
        bits |= LOWER;
    }
    if ch.is_whitespace() {
        bits |= SPACE;
        if c == 0xA0 || c == 0x3000 || (0x2000..=0x200A).contains(&c) {
            bits |= BLANK;
        }
    }
    if ch.is_control() {
        bits |= CONTROL;
    }
    if bits == 0 && !ch.is_numeric() {
        bits |= PUNCT;
    }
    if ch.is_numeric() && !ch.is_alphabetic() {
        bits |= PUNCT; // Windows counts digits other than 0-9 as punctuation
    }
    bits
}

/// Narrow characters past ASCII have no class in the "C" locale.
fn narrow(c: i32) -> u32 {
    if (0..0x80).contains(&c) { c as u32 } else { u32::MAX }
}

/// Case mapping in the "C" locale: ASCII letters only (as the UCRT does,
/// for wide characters too).
pub fn lower(c: u32) -> u32 {
    if c < 0x80 { (c as u8).to_ascii_lowercase() as u32 } else { c }
}

pub fn upper(c: u32) -> u32 {
    if c < 0x80 { (c as u8).to_ascii_uppercase() as u32 } else { c }
}

pub fn is_space(c: u32) -> bool {
    classify(c) & SPACE != 0
}

macro_rules! classes {
    ($($n:ident / $w:ident = $mask:expr;)*) => {$(
        #[no_mangle]
        pub extern "C" fn $n(c: i32) -> i32 {
            (classify(narrow(c)) & ($mask)) as i32
        }
        #[no_mangle]
        pub extern "C" fn $w(c: u16) -> i32 {
            (classify(c as u32) & ($mask)) as i32
        }
    )*};
}

classes! {
    isalpha / iswalpha = ALPHA;
    isupper / iswupper = UPPER;
    islower / iswlower = LOWER;
    isdigit / iswdigit = DIGIT;
    isxdigit / iswxdigit = HEX;
    isspace / iswspace = SPACE;
    ispunct / iswpunct = PUNCT;
    isalnum / iswalnum = ALPHA | DIGIT;
    isgraph / iswgraph = ALPHA | DIGIT | PUNCT;
    iscntrl / iswcntrl = CONTROL;
    isblank / iswblank = BLANK;
}

/// Printable: graphic characters and the space (but not tab).
#[no_mangle]
pub extern "C" fn isprint(c: i32) -> i32 {
    (isgraph(c) != 0 || c == b' ' as i32) as i32
}

#[no_mangle]
pub extern "C" fn iswprint(c: u16) -> i32 {
    (iswgraph(c) != 0 || c == b' ' as u16) as i32
}

#[no_mangle]
pub extern "C" fn _isctype(c: i32, mask: i32) -> i32 {
    (classify(narrow(c)) & mask as u16) as i32
}

#[no_mangle]
pub extern "C" fn _isctype_l(c: i32, mask: i32, _locale: usize) -> i32 {
    _isctype(c, mask)
}

#[no_mangle]
pub extern "C" fn iswctype(c: u16, mask: u16) -> i32 {
    (classify(c as u32) & mask) as i32
}

#[no_mangle]
pub extern "C" fn is_wctype(c: u16, mask: u16) -> i32 {
    iswctype(c, mask)
}

#[no_mangle]
pub extern "C" fn isascii(c: i32) -> i32 {
    ((c as u32) < 0x80) as i32
}

#[no_mangle]
pub extern "C" fn __isascii(c: i32) -> i32 {
    isascii(c)
}

#[no_mangle]
pub extern "C" fn iswascii(c: u16) -> i32 {
    (c < 0x80) as i32
}

#[no_mangle]
pub extern "C" fn __iscsym(c: i32) -> i32 {
    (isalnum(c) != 0 || c == b'_' as i32) as i32
}

#[no_mangle]
pub extern "C" fn __iscsymf(c: i32) -> i32 {
    (isalpha(c) != 0 || c == b'_' as i32) as i32
}

#[no_mangle]
pub extern "C" fn toascii(c: i32) -> i32 {
    c & 0x7F
}

#[no_mangle]
pub extern "C" fn __toascii(c: i32) -> i32 {
    c & 0x7F
}

#[no_mangle]
pub extern "C" fn tolower(c: i32) -> i32 {
    if (0..0x80).contains(&c) { lower(c as u32) as i32 } else { c }
}

#[no_mangle]
pub extern "C" fn toupper(c: i32) -> i32 {
    if (0..0x80).contains(&c) { upper(c as u32) as i32 } else { c }
}

#[no_mangle]
pub extern "C" fn _tolower(c: i32) -> i32 {
    c - b'A' as i32 + b'a' as i32
}

#[no_mangle]
pub extern "C" fn _toupper(c: i32) -> i32 {
    c - b'a' as i32 + b'A' as i32
}

#[no_mangle]
pub extern "C" fn towlower(c: u16) -> u16 {
    lower(c as u32) as u16
}

#[no_mangle]
pub extern "C" fn towupper(c: u16) -> u16 {
    upper(c as u32) as u16
}

#[no_mangle]
pub extern "C" fn _tolower_l(c: i32, _l: usize) -> i32 {
    tolower(c)
}

#[no_mangle]
pub extern "C" fn _toupper_l(c: i32, _l: usize) -> i32 {
    toupper(c)
}

#[no_mangle]
pub extern "C" fn _towlower_l(c: u16, _l: usize) -> u16 {
    towlower(c)
}

#[no_mangle]
pub extern "C" fn _towupper_l(c: u16, _l: usize) -> u16 {
    towupper(c)
}

/// `wctype("alpha")` and friends: the mask iswctype takes.
#[no_mangle]
pub unsafe extern "C" fn wctype(name: *const u8) -> u16 {
    let name = unsafe { crate::string::slice(name) };
    match name {
        b"alnum" => ALPHA | DIGIT,
        b"alpha" => ALPHA,
        b"blank" => BLANK,
        b"cntrl" => CONTROL,
        b"digit" => DIGIT,
        b"graph" => ALPHA | DIGIT | PUNCT,
        b"lower" => LOWER,
        b"print" => ALPHA | DIGIT | PUNCT,
        b"punct" => PUNCT,
        b"space" => SPACE,
        b"upper" => UPPER,
        b"xdigit" => HEX,
        _ => 0,
    }
}

#[no_mangle]
pub extern "C" fn isleadbyte(_c: i32) -> i32 {
    0
}
