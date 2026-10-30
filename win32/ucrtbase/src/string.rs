//! String functions, narrow and wide. Most are written once over `Ch` (a
//! byte or a UTF-16 unit) and exported under both names.
//!
//! memcpy, memmove, memset and memcmp are compiler_builtins' (exported by
//! build.rs): writing them here would let the optimiser turn their loops
//! back into calls to themselves.

use crate::errno::{EINVAL, ERANGE, STRUNCATE};
use crate::{ctype, heap, invalid_parameter, set_errno};

/// A character unit: `u8` for narrow strings, `u16` for wide ones.
pub trait Ch: Copy + Eq + Default + 'static {
    fn to_u32(self) -> u32;
    fn from_u32(c: u32) -> Self;
}

impl Ch for u8 {
    fn to_u32(self) -> u32 {
        self as u32
    }
    fn from_u32(c: u32) -> Self {
        c as u8
    }
}

impl Ch for u16 {
    fn to_u32(self) -> u32 {
        self as u32
    }
    fn from_u32(c: u32) -> Self {
        c as u16
    }
}

pub unsafe fn len<T: Ch>(s: *const T) -> usize {
    let mut n = 0;
    while unsafe { *s.add(n) } != T::default() {
        n += 1;
    }
    n
}

pub unsafe fn nlen<T: Ch>(s: *const T, max: usize) -> usize {
    let mut n = 0;
    while n < max && unsafe { *s.add(n) } != T::default() {
        n += 1;
    }
    n
}

/// The string at `s` (without its terminator) as a slice.
pub unsafe fn slice<'a, T: Ch>(s: *const T) -> &'a [T] {
    unsafe { core::slice::from_raw_parts(s, len(s)) }
}

unsafe fn copy<T: Ch>(dst: *mut T, src: *const T) -> *mut T {
    let mut i = 0;
    loop {
        let c = unsafe { *src.add(i) };
        unsafe { *dst.add(i) = c };
        if c == T::default() {
            return dst;
        }
        i += 1;
    }
}

unsafe fn ncopy<T: Ch>(dst: *mut T, src: *const T, n: usize) -> *mut T {
    let mut i = 0;
    while i < n && unsafe { *src.add(i) } != T::default() {
        unsafe { *dst.add(i) = *src.add(i) };
        i += 1;
    }
    while i < n {
        unsafe { *dst.add(i) = T::default() };
        i += 1;
    }
    dst
}

unsafe fn cat<T: Ch>(dst: *mut T, src: *const T) -> *mut T {
    unsafe { copy(dst.add(len(dst)), src) };
    dst
}

unsafe fn ncat<T: Ch>(dst: *mut T, src: *const T, n: usize) -> *mut T {
    let end = unsafe { dst.add(len(dst)) };
    let k = unsafe { nlen(src, n) };
    unsafe {
        core::ptr::copy_nonoverlapping(src, end, k);
        *end.add(k) = T::default();
    }
    dst
}

/// Compare up to `n` units, after `fold` (identity or lowercase).
unsafe fn compare<T: Ch>(a: *const T, b: *const T, n: usize, fold: fn(u32) -> u32) -> i32 {
    for i in 0..n {
        let (x, y) = unsafe { (fold((*a.add(i)).to_u32()), fold((*b.add(i)).to_u32())) };
        if x != y {
            return if x < y { -1 } else { 1 };
        }
        if x == 0 {
            break;
        }
    }
    0
}

fn same(c: u32) -> u32 {
    c
}

unsafe fn find<T: Ch>(s: *const T, c: T) -> *mut T {
    let mut p = s;
    loop {
        if unsafe { *p } == c {
            return p as *mut T;
        }
        if unsafe { *p } == T::default() {
            return core::ptr::null_mut();
        }
        p = unsafe { p.add(1) };
    }
}

unsafe fn rfind<T: Ch>(s: *const T, c: T) -> *mut T {
    let mut found = core::ptr::null_mut();
    let mut p = s;
    loop {
        if unsafe { *p } == c {
            found = p as *mut T;
        }
        if unsafe { *p } == T::default() {
            return found;
        }
        p = unsafe { p.add(1) };
    }
}

unsafe fn substring<T: Ch>(hay: *const T, needle: *const T) -> *mut T {
    let n = unsafe { len(needle) };
    if n == 0 {
        return hay as *mut T;
    }
    let mut p = hay;
    while unsafe { *p } != T::default() {
        if unsafe { compare(p, needle, n, same) } == 0 {
            return p as *mut T;
        }
        p = unsafe { p.add(1) };
    }
    core::ptr::null_mut()
}

/// Length of the prefix of `s` whose units are (`inside`) or aren't in `set`.
unsafe fn span<T: Ch>(s: *const T, set: *const T, inside: bool) -> usize {
    let set = unsafe { slice(set) };
    let mut n = 0;
    loop {
        let c = unsafe { *s.add(n) };
        if c == T::default() || set.contains(&c) != inside {
            return n;
        }
        n += 1;
    }
}

unsafe fn tokenize<T: Ch>(s: *mut T, delim: *const T, state: *mut *mut T) -> *mut T {
    let mut p = if s.is_null() { unsafe { *state } } else { s };
    if p.is_null() {
        return p;
    }
    p = unsafe { p.add(span(p, delim, true)) };
    if unsafe { *p } == T::default() {
        unsafe { *state = p };
        return core::ptr::null_mut();
    }
    let end = unsafe { p.add(span(p, delim, false)) };
    if unsafe { *end } == T::default() {
        unsafe { *state = end };
    } else {
        unsafe {
            *end = T::default();
            *state = end.add(1);
        }
    }
    p
}

unsafe fn dup<T: Ch>(s: *const T) -> *mut T {
    if s.is_null() {
        return core::ptr::null_mut();
    }
    let n = unsafe { len(s) } + 1;
    let p = heap::malloc(n * size_of::<T>()) as *mut T;
    if !p.is_null() {
        unsafe { core::ptr::copy_nonoverlapping(s, p, n) };
    }
    p
}

unsafe fn map_in_place<T: Ch>(s: *mut T, f: fn(u32) -> u32) -> *mut T {
    let mut p = s;
    while unsafe { *p } != T::default() {
        unsafe { *p = T::from_u32(f((*p).to_u32())) };
        p = unsafe { p.add(1) };
    }
    s
}

unsafe fn reverse<T: Ch>(s: *mut T) -> *mut T {
    unsafe { core::slice::from_raw_parts_mut(s, len(s)) }.reverse();
    s
}

const TRUNCATE: usize = usize::MAX;

/// strcpy_s / wcscpy_s.
unsafe fn copy_s<T: Ch>(dst: *mut T, size: usize, src: *const T) -> i32 {
    unsafe { ncopy_s(dst, size, src, usize::MAX - 1) }
}

/// strncpy_s / wcsncpy_s: at most `n` units (all that fit with _TRUNCATE).
unsafe fn ncopy_s<T: Ch>(dst: *mut T, size: usize, src: *const T, n: usize) -> i32 {
    if dst.is_null() || size == 0 {
        invalid_parameter();
        return EINVAL;
    }
    if src.is_null() {
        unsafe { *dst = T::default() };
        invalid_parameter();
        return EINVAL;
    }
    let want = unsafe { nlen(src, if n == TRUNCATE { usize::MAX } else { n }) };
    if want < size {
        unsafe {
            core::ptr::copy_nonoverlapping(src, dst, want);
            *dst.add(want) = T::default();
        }
        return 0;
    }
    if n == TRUNCATE {
        unsafe {
            core::ptr::copy_nonoverlapping(src, dst, size - 1);
            *dst.add(size - 1) = T::default();
        }
        return STRUNCATE;
    }
    unsafe { *dst = T::default() };
    set_errno(ERANGE);
    invalid_parameter();
    ERANGE
}

/// strcat_s / strncat_s and their wide forms.
unsafe fn ncat_s<T: Ch>(dst: *mut T, size: usize, src: *const T, n: usize) -> i32 {
    if dst.is_null() || size == 0 {
        invalid_parameter();
        return EINVAL;
    }
    let have = unsafe { nlen(dst, size) };
    if have == size {
        unsafe { *dst = T::default() };
        invalid_parameter();
        return EINVAL;
    }
    let r = unsafe { ncopy_s(dst.add(have), size - have, src, n) };
    if r != 0 && r != STRUNCATE {
        unsafe { *dst = T::default() };
    }
    r
}

macro_rules! both {
    ($(fn $n:ident / $w:ident ($($a:ident: $t:ident),*) -> $r:ident = $f:expr;)*) => {$(
        #[no_mangle]
        pub unsafe extern "C" fn $n($($a: both!(@t u8, $t)),*) -> both!(@t u8, $r) { unsafe { $f($($a),*) } }
        #[no_mangle]
        pub unsafe extern "C" fn $w($($a: both!(@t u16, $t)),*) -> both!(@t u16, $r) { unsafe { $f($($a),*) } }
    )*};
    (@t $c:ty, S) => { *const $c };
    (@t $c:ty, M) => { *mut $c };
    (@t $c:ty, C) => { $c };
    (@t $c:ty, MM) => { *mut *mut $c };
    (@t $c:ty, usize) => { usize };
    (@t $c:ty, i32) => { i32 };
}

both! {
    fn strlen / wcslen(s: S) -> usize = len;
    fn strnlen / wcsnlen(s: S, n: usize) -> usize = nlen;
    fn strcpy / wcscpy(d: M, s: S) -> M = copy;
    fn strncpy / wcsncpy(d: M, s: S, n: usize) -> M = ncopy;
    fn strcat / wcscat(d: M, s: S) -> M = cat;
    fn strncat / wcsncat(d: M, s: S, n: usize) -> M = ncat;
    fn strstr / wcsstr(h: S, n: S) -> M = substring;
    fn strspn / wcsspn(s: S, set: S) -> usize = |s, set| span(s, set, true);
    fn strcspn / wcscspn(s: S, set: S) -> usize = |s, set| span(s, set, false);
    fn _strdup / _wcsdup(s: S) -> M = dup;
    fn _strrev / _wcsrev(s: M) -> M = reverse;
    fn strcpy_s / wcscpy_s(d: M, size: usize, s: S) -> i32 = copy_s;
    fn strncpy_s / wcsncpy_s(d: M, size: usize, s: S, n: usize) -> i32 = ncopy_s;
    fn strcat_s / wcscat_s(d: M, size: usize, s: S) -> i32 = |d, size, s| ncat_s(d, size, s, usize::MAX - 1);
    fn strncat_s / wcsncat_s(d: M, size: usize, s: S, n: usize) -> i32 = ncat_s;
    fn strtok_s / wcstok_s(s: M, d: S, ctx: MM) -> M = tokenize;
}

#[no_mangle]
pub unsafe extern "C" fn strcmp(a: *const u8, b: *const u8) -> i32 {
    unsafe { compare(a, b, usize::MAX, same) }
}

#[no_mangle]
pub unsafe extern "C" fn wcscmp(a: *const u16, b: *const u16) -> i32 {
    unsafe { compare(a, b, usize::MAX, same) }
}

#[no_mangle]
pub unsafe extern "C" fn strncmp(a: *const u8, b: *const u8, n: usize) -> i32 {
    unsafe { compare(a, b, n, same) }
}

#[no_mangle]
pub unsafe extern "C" fn wcsncmp(a: *const u16, b: *const u16, n: usize) -> i32 {
    unsafe { compare(a, b, n, same) }
}

#[no_mangle]
pub unsafe extern "C" fn strcoll(a: *const u8, b: *const u8) -> i32 {
    unsafe { strcmp(a, b) }
}

#[no_mangle]
pub unsafe extern "C" fn wcscoll(a: *const u16, b: *const u16) -> i32 {
    unsafe { wcscmp(a, b) }
}

#[no_mangle]
pub unsafe extern "C" fn _stricmp(a: *const u8, b: *const u8) -> i32 {
    unsafe { compare(a, b, usize::MAX, ctype::lower) }
}

#[no_mangle]
pub unsafe extern "C" fn _strcmpi(a: *const u8, b: *const u8) -> i32 {
    unsafe { _stricmp(a, b) }
}

#[no_mangle]
pub unsafe extern "C" fn _wcsicmp(a: *const u16, b: *const u16) -> i32 {
    unsafe { compare(a, b, usize::MAX, ctype::lower) }
}

#[no_mangle]
pub unsafe extern "C" fn _strnicmp(a: *const u8, b: *const u8, n: usize) -> i32 {
    unsafe { compare(a, b, n, ctype::lower) }
}

#[no_mangle]
pub unsafe extern "C" fn _wcsnicmp(a: *const u16, b: *const u16, n: usize) -> i32 {
    unsafe { compare(a, b, n, ctype::lower) }
}

#[no_mangle]
pub unsafe extern "C" fn _stricoll(a: *const u8, b: *const u8) -> i32 {
    unsafe { _stricmp(a, b) }
}

#[no_mangle]
pub unsafe extern "C" fn _wcsicoll(a: *const u16, b: *const u16) -> i32 {
    unsafe { _wcsicmp(a, b) }
}

#[no_mangle]
pub unsafe extern "C" fn strchr(s: *const u8, c: i32) -> *mut u8 {
    unsafe { find(s, c as u8) }
}

#[no_mangle]
pub unsafe extern "C" fn wcschr(s: *const u16, c: u16) -> *mut u16 {
    unsafe { find(s, c) }
}

#[no_mangle]
pub unsafe extern "C" fn strrchr(s: *const u8, c: i32) -> *mut u8 {
    unsafe { rfind(s, c as u8) }
}

#[no_mangle]
pub unsafe extern "C" fn wcsrchr(s: *const u16, c: u16) -> *mut u16 {
    unsafe { rfind(s, c) }
}

#[no_mangle]
pub unsafe extern "C" fn strpbrk(s: *const u8, set: *const u8) -> *mut u8 {
    let p = unsafe { s.add(span(s, set, false)) };
    if unsafe { *p } == 0 { core::ptr::null_mut() } else { p as *mut u8 }
}

#[no_mangle]
pub unsafe extern "C" fn wcspbrk(s: *const u16, set: *const u16) -> *mut u16 {
    let p = unsafe { s.add(span(s, set, false)) };
    if unsafe { *p } == 0 { core::ptr::null_mut() } else { p as *mut u16 }
}


#[no_mangle]
pub unsafe extern "C" fn strtok(s: *mut u8, delim: *const u8) -> *mut u8 {
    unsafe { tokenize(s, delim, &mut crate::threads::this().strtok) }
}

/// The two-argument wcstok (msvcrt's, and UCRT's `_wcstok`).
#[no_mangle]
pub unsafe extern "C" fn _wcstok(s: *mut u16, delim: *const u16) -> *mut u16 {
    unsafe { tokenize(s, delim, &mut crate::threads::this().wcstok) }
}

/// UCRT's wcstok takes a context (C11's); msvcrt.dll's wcstok forwards to
/// `_wcstok` instead.
#[no_mangle]
pub unsafe extern "C" fn wcstok(s: *mut u16, delim: *const u16, ctx: *mut *mut u16) -> *mut u16 {
    let ctx = if ctx.is_null() { &raw mut crate::threads::this().wcstok } else { ctx };
    unsafe { tokenize(s, delim, ctx) }
}

#[no_mangle]
pub unsafe extern "C" fn _strlwr(s: *mut u8) -> *mut u8 {
    unsafe { map_in_place(s, ctype::lower) }
}

#[no_mangle]
pub unsafe extern "C" fn _strupr(s: *mut u8) -> *mut u8 {
    unsafe { map_in_place(s, ctype::upper) }
}

#[no_mangle]
pub unsafe extern "C" fn _wcslwr(s: *mut u16) -> *mut u16 {
    unsafe { map_in_place(s, ctype::lower) }
}

#[no_mangle]
pub unsafe extern "C" fn _wcsupr(s: *mut u16) -> *mut u16 {
    unsafe { map_in_place(s, ctype::upper) }
}

#[no_mangle]
pub unsafe extern "C" fn _strlwr_s(s: *mut u8, _size: usize) -> i32 {
    unsafe { map_in_place(s, ctype::lower) };
    0
}

#[no_mangle]
pub unsafe extern "C" fn _strupr_s(s: *mut u8, _size: usize) -> i32 {
    unsafe { map_in_place(s, ctype::upper) };
    0
}

#[no_mangle]
pub unsafe extern "C" fn _wcslwr_s(s: *mut u16, _size: usize) -> i32 {
    unsafe { map_in_place(s, ctype::lower) };
    0
}

#[no_mangle]
pub unsafe extern "C" fn _wcsupr_s(s: *mut u16, _size: usize) -> i32 {
    unsafe { map_in_place(s, ctype::upper) };
    0
}

#[no_mangle]
pub unsafe extern "C" fn _strset(s: *mut u8, c: i32) -> *mut u8 {
    unsafe { core::ptr::write_bytes(s, c as u8, len(s)) };
    s
}

#[no_mangle]
pub unsafe extern "C" fn strxfrm(dst: *mut u8, src: *const u8, n: usize) -> usize {
    let k = unsafe { len(src) };
    if k < n {
        unsafe { copy(dst, src) };
    }
    k
}

#[no_mangle]
pub unsafe extern "C" fn wcsxfrm(dst: *mut u16, src: *const u16, n: usize) -> usize {
    let k = unsafe { len(src) };
    if k < n {
        unsafe { copy(dst, src) };
    }
    k
}

#[no_mangle]
pub unsafe extern "C" fn memchr(s: *const u8, c: i32, n: usize) -> *mut u8 {
    for i in 0..n {
        if unsafe { *s.add(i) } == c as u8 {
            return unsafe { s.add(i) } as *mut u8;
        }
    }
    core::ptr::null_mut()
}

#[no_mangle]
pub unsafe extern "C" fn _memicmp(a: *const u8, b: *const u8, n: usize) -> i32 {
    for i in 0..n {
        let (x, y) = unsafe { (ctype::lower(*a.add(i) as u32), ctype::lower(*b.add(i) as u32)) };
        if x != y {
            return if x < y { -1 } else { 1 };
        }
    }
    0
}

#[no_mangle]
pub unsafe extern "C" fn _memccpy(dst: *mut u8, src: *const u8, c: i32, n: usize) -> *mut u8 {
    for i in 0..n {
        let b = unsafe { *src.add(i) };
        unsafe { *dst.add(i) = b };
        if b == c as u8 {
            return unsafe { dst.add(i + 1) };
        }
    }
    core::ptr::null_mut()
}

#[no_mangle]
pub unsafe extern "C" fn memcpy_s(dst: *mut u8, size: usize, src: *const u8, n: usize) -> i32 {
    if n == 0 {
        return 0;
    }
    if dst.is_null() {
        invalid_parameter();
        return EINVAL;
    }
    if src.is_null() || size < n {
        unsafe { core::ptr::write_bytes(dst, 0, size) };
        invalid_parameter();
        return if src.is_null() { EINVAL } else { ERANGE };
    }
    unsafe { core::ptr::copy_nonoverlapping(src, dst, n) };
    0
}

#[no_mangle]
pub unsafe extern "C" fn memmove_s(dst: *mut u8, size: usize, src: *const u8, n: usize) -> i32 {
    if n == 0 {
        return 0;
    }
    if dst.is_null() || src.is_null() || size < n {
        invalid_parameter();
        return if size < n { ERANGE } else { EINVAL };
    }
    unsafe { core::ptr::copy(src, dst, n) };
    0
}

#[no_mangle]
pub unsafe extern "C" fn wmemcpy_s(dst: *mut u16, size: usize, src: *const u16, n: usize) -> i32 {
    unsafe { memcpy_s(dst as *mut u8, size * 2, src as *const u8, n * 2) }
}

#[no_mangle]
pub unsafe extern "C" fn wmemmove_s(dst: *mut u16, size: usize, src: *const u16, n: usize) -> i32 {
    unsafe { memmove_s(dst as *mut u8, size * 2, src as *const u8, n * 2) }
}

// -----| strerror |-----

const MESSAGES: [&str; 43] = [
    "No error\0",
    "Operation not permitted\0",
    "No such file or directory\0",
    "No such process\0",
    "Interrupted function call\0",
    "Input/output error\0",
    "No such device or address\0",
    "Arg list too long\0",
    "Exec format error\0",
    "Bad file descriptor\0",
    "No child processes\0",
    "Resource temporarily unavailable\0",
    "Not enough space\0",
    "Permission denied\0",
    "Bad address\0",
    "Unknown error\0",
    "Resource device\0",
    "File exists\0",
    "Improper link\0",
    "No such device\0",
    "Not a directory\0",
    "Is a directory\0",
    "Invalid argument\0",
    "Too many open files in system\0",
    "Too many open files\0",
    "Inappropriate I/O control operation\0",
    "Unknown error\0",
    "File too large\0",
    "No space left on device\0",
    "Invalid seek\0",
    "Read-only file system\0",
    "Too many links\0",
    "Broken pipe\0",
    "Domain error\0",
    "Result too large\0",
    "Unknown error\0",
    "Resource deadlock avoided\0",
    "Unknown error\0",
    "Filename too long\0",
    "No locks available\0",
    "Function not implemented\0",
    "Directory not empty\0",
    "Illegal byte sequence\0",
];

pub fn message(e: i32) -> &'static str {
    MESSAGES.get(e as usize).copied().unwrap_or("Unknown error\0")
}

#[no_mangle]
pub extern "C" fn strerror(e: i32) -> *const u8 {
    message(e).as_ptr()
}

#[no_mangle]
pub unsafe extern "C" fn strerror_s(buf: *mut u8, size: usize, e: i32) -> i32 {
    unsafe { ncopy_s(buf, size, message(e).as_ptr(), TRUNCATE) };
    0
}

static mut WIDE_MESSAGE: [u16; 64] = [0; 64];

#[no_mangle]
pub unsafe extern "C" fn _wcserror(e: i32) -> *const u16 {
    let buf = unsafe { &mut *(&raw mut WIDE_MESSAGE) };
    for (i, b) in message(e).bytes().enumerate() {
        buf[i] = b as u16;
    }
    buf.as_ptr()
}

#[no_mangle]
pub unsafe extern "C" fn _wcserror_s(buf: *mut u16, size: usize, e: i32) -> i32 {
    unsafe { ncopy_s(buf, size, _wcserror(e), TRUNCATE) }
}
