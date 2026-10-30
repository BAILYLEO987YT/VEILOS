//! Everything else: sorting and searching, rand, directories and paths,
//! file status, locale, setjmp/longjmp, operator new/delete.

use crate::convert::{to_narrow, to_wide};
use crate::errno::{EACCES, ENOENT, ERANGE};
use crate::string::{len, slice};
use crate::{errno_from_win32, heap, invalid_parameter, set_errno};

// -----| sorting and searching |-----

type Compare = unsafe extern "C" fn(*const u8, *const u8) -> i32;
type CompareS = unsafe extern "C" fn(*mut u8, *const u8, *const u8) -> i32;

unsafe fn swap(a: *mut u8, b: *mut u8, size: usize) {
    if a != b {
        unsafe { core::ptr::swap_nonoverlapping(a, b, size) };
    }
}

/// Quicksort (median of three, insertion sort for short runs), smaller
/// side first so the recursion stays shallow.
unsafe fn sort(base: *mut u8, n: usize, size: usize, cmp: &dyn Fn(*const u8, *const u8) -> i32) {
    let at = |i: usize| unsafe { base.add(i * size) };
    if n < 2 {
        return;
    }
    if n <= 8 {
        for i in 1..n {
            let mut j = i;
            while j > 0 && cmp(at(j - 1), at(j)) > 0 {
                unsafe { swap(at(j - 1), at(j), size) };
                j -= 1;
            }
        }
        return;
    }
    let mid = n / 2;
    unsafe {
        if cmp(at(mid), at(0)) < 0 {
            swap(at(mid), at(0), size);
        }
        if cmp(at(n - 1), at(0)) < 0 {
            swap(at(n - 1), at(0), size);
        }
        if cmp(at(n - 1), at(mid)) < 0 {
            swap(at(n - 1), at(mid), size);
        }
        // Pivot to the end, then partition.
        swap(at(mid), at(n - 1), size);
    }
    let pivot = at(n - 1);
    let mut store = 0;
    for i in 0..n - 1 {
        if cmp(at(i), pivot) < 0 {
            unsafe { swap(at(i), at(store), size) };
            store += 1;
        }
    }
    unsafe { swap(at(store), pivot, size) };
    unsafe {
        sort(base, store, size, cmp);
        sort(at(store + 1), n - store - 1, size, cmp);
    }
}

#[no_mangle]
pub unsafe extern "C" fn qsort(base: *mut u8, n: usize, size: usize, cmp: Compare) {
    unsafe { sort(base, n, size, &|a, b| cmp(a, b)) };
}

#[no_mangle]
pub unsafe extern "C" fn qsort_s(base: *mut u8, n: usize, size: usize, cmp: CompareS, ctx: *mut u8) {
    unsafe { sort(base, n, size, &|a, b| cmp(ctx, a, b)) };
}

#[no_mangle]
pub unsafe extern "C" fn bsearch(key: *const u8, base: *const u8, n: usize, size: usize, cmp: Compare) -> *mut u8 {
    let (mut lo, mut hi) = (0, n);
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        let p = unsafe { base.add(mid * size) };
        match unsafe { cmp(key, p) } {
            0 => return p as *mut u8,
            c if c < 0 => hi = mid,
            _ => lo = mid + 1,
        }
    }
    core::ptr::null_mut()
}

#[no_mangle]
pub unsafe extern "C" fn bsearch_s(key: *const u8, base: *const u8, n: usize, size: usize, cmp: CompareS, ctx: *mut u8) -> *mut u8 {
    let (mut lo, mut hi) = (0, n);
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        let p = unsafe { base.add(mid * size) };
        match unsafe { cmp(ctx, key, p) } {
            0 => return p as *mut u8,
            c if c < 0 => hi = mid,
            _ => lo = mid + 1,
        }
    }
    core::ptr::null_mut()
}

#[no_mangle]
pub unsafe extern "C" fn _lfind(key: *const u8, base: *const u8, n: *const u32, size: u32, cmp: Compare) -> *mut u8 {
    for i in 0..unsafe { *n } as usize {
        let p = unsafe { base.add(i * size as usize) };
        if unsafe { cmp(key, p) } == 0 {
            return p as *mut u8;
        }
    }
    core::ptr::null_mut()
}

#[no_mangle]
pub unsafe extern "C" fn _lsearch(key: *const u8, base: *mut u8, n: *mut u32, size: u32, cmp: Compare) -> *mut u8 {
    let found = unsafe { _lfind(key, base, n, size, cmp) };
    if !found.is_null() {
        return found;
    }
    let p = unsafe { base.add(*n as usize * size as usize) };
    unsafe {
        core::ptr::copy_nonoverlapping(key, p, size as usize);
        *n += 1;
    }
    p
}

// -----| rand |-----

static mut SEED: u32 = 1;

/// Microsoft's generator, so seeded sequences match Windows.
#[no_mangle]
pub extern "C" fn rand() -> i32 {
    unsafe {
        SEED = SEED.wrapping_mul(214_013).wrapping_add(2_531_011);
        ((SEED >> 16) & 0x7FFF) as i32
    }
}

#[no_mangle]
pub extern "C" fn srand(seed: u32) {
    unsafe { SEED = seed };
}

/// rand_s: "cryptographic" randomness from a mix of the clock and a
/// counter (an entropy source is future work).
#[no_mangle]
pub unsafe extern "C" fn rand_s(out: *mut u32) -> i32 {
    static mut STATE: u64 = 0x9E37_79B9_7F4A_7C15;
    unsafe {
        STATE ^= crate::GetTickCount64();
        STATE = STATE.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        *out = (STATE >> 32) as u32;
    }
    0
}

// -----| processes |-----

/// No command processor yet (cmd.exe is W7).
#[no_mangle]
pub unsafe extern "C" fn system(cmd: *const u8) -> i32 {
    if cmd.is_null() {
        return 0;
    }
    set_errno(ENOENT);
    -1
}

#[no_mangle]
pub unsafe extern "C" fn _wsystem(cmd: *const u16) -> i32 {
    unsafe { system(cmd as *const u8) }
}

#[no_mangle]
pub extern "C" fn _getpid() -> i32 {
    unsafe { crate::GetCurrentProcessId() as i32 }
}

#[no_mangle]
pub extern "C" fn _sleep(ms: u32) {
    unsafe { crate::Sleep(ms) };
}

// -----| directories |-----

/// A wide result as the caller's narrow buffer (or a new one if `buf` is
/// null).
unsafe fn give_narrow(wide: &[u16], buf: *mut u8, size: usize) -> *mut u8 {
    let s = to_narrow(wide);
    let n = unsafe { len(s) };
    if buf.is_null() {
        if size > n + 1 {
            let p = heap::realloc(s, size);
            return p;
        }
        return s;
    }
    if n + 1 > size {
        heap::free(s);
        set_errno(ERANGE);
        return core::ptr::null_mut();
    }
    unsafe { core::ptr::copy_nonoverlapping(s, buf, n + 1) };
    heap::free(s);
    buf
}

unsafe fn give_wide(wide: &[u16], buf: *mut u16, size: usize) -> *mut u16 {
    let n = wide.len();
    let (out, cap) = if buf.is_null() {
        let cap = size.max(n + 1);
        (heap::malloc(cap * 2) as *mut u16, cap)
    } else {
        (buf, size)
    };
    if out.is_null() {
        return out;
    }
    if n + 1 > cap {
        set_errno(ERANGE);
        return core::ptr::null_mut();
    }
    unsafe {
        core::ptr::copy_nonoverlapping(wide.as_ptr(), out, n);
        *out.add(n) = 0;
    }
    out
}

fn current_directory(out: &mut [u16; 520]) -> usize {
    unsafe { crate::GetCurrentDirectoryW(out.len() as u32, out.as_mut_ptr()) as usize }
}

#[no_mangle]
pub unsafe extern "C" fn _wgetcwd(buf: *mut u16, size: i32) -> *mut u16 {
    let mut d = [0u16; 520];
    let n = current_directory(&mut d);
    unsafe { give_wide(&d[..n], buf, size.max(0) as usize) }
}

#[no_mangle]
pub unsafe extern "C" fn _getcwd(buf: *mut u8, size: i32) -> *mut u8 {
    let mut d = [0u16; 520];
    let n = current_directory(&mut d);
    unsafe { give_narrow(&d[..n], buf, size.max(0) as usize) }
}

#[no_mangle]
pub unsafe extern "C" fn _getdcwd(_drive: i32, buf: *mut u8, size: i32) -> *mut u8 {
    unsafe { _getcwd(buf, size) }
}

#[no_mangle]
pub unsafe extern "C" fn _wgetdcwd(_drive: i32, buf: *mut u16, size: i32) -> *mut u16 {
    unsafe { _wgetcwd(buf, size) }
}

#[no_mangle]
pub extern "C" fn _getdrive() -> i32 {
    3 // C:
}

/// Run `f` on a narrow path converted to wide.
unsafe fn with_wide(path: *const u8, f: impl FnOnce(*const u16) -> i32) -> i32 {
    if path.is_null() {
        invalid_parameter();
        return -1;
    }
    let w = to_wide(unsafe { slice(path) });
    let r = f(w);
    heap::free(w as *mut u8);
    r
}

fn win32_result(ok: i32) -> i32 {
    if ok == 0 {
        errno_from_win32();
        -1
    } else {
        0
    }
}

#[no_mangle]
pub unsafe extern "C" fn _wchdir(path: *const u16) -> i32 {
    win32_result(unsafe { crate::SetCurrentDirectoryW(path) })
}

#[no_mangle]
pub unsafe extern "C" fn _chdir(path: *const u8) -> i32 {
    unsafe { with_wide(path, |w| _wchdir(w)) }
}

#[no_mangle]
pub unsafe extern "C" fn _wmkdir(path: *const u16) -> i32 {
    win32_result(unsafe { crate::CreateDirectoryW(path, 0) })
}

#[no_mangle]
pub unsafe extern "C" fn _mkdir(path: *const u8) -> i32 {
    unsafe { with_wide(path, |w| _wmkdir(w)) }
}

#[no_mangle]
pub unsafe extern "C" fn _wrmdir(path: *const u16) -> i32 {
    win32_result(unsafe { crate::RemoveDirectoryW(path) })
}

#[no_mangle]
pub unsafe extern "C" fn _rmdir(path: *const u8) -> i32 {
    unsafe { with_wide(path, |w| _wrmdir(w)) }
}

#[no_mangle]
pub unsafe extern "C" fn _waccess(path: *const u16, mode: i32) -> i32 {
    let attrs = unsafe { crate::GetFileAttributesW(path) };
    if attrs == u32::MAX {
        errno_from_win32();
        return -1;
    }
    // Asking to write a read-only file.
    if mode & 2 != 0 && attrs & 1 != 0 && attrs & 0x10 == 0 {
        set_errno(EACCES);
        return -1;
    }
    0
}

#[no_mangle]
pub unsafe extern "C" fn _access(path: *const u8, mode: i32) -> i32 {
    unsafe { with_wide(path, |w| _waccess(w, mode)) }
}

#[no_mangle]
pub unsafe extern "C" fn _access_s(path: *const u8, mode: i32) -> i32 {
    if unsafe { _access(path, mode) } == 0 { 0 } else { unsafe { *crate::_errno() } }
}

#[no_mangle]
pub unsafe extern "C" fn _waccess_s(path: *const u16, mode: i32) -> i32 {
    if unsafe { _waccess(path, mode) } == 0 { 0 } else { unsafe { *crate::_errno() } }
}

/// Not in our kernel32 yet: renaming needs a kernel rename operation.
#[no_mangle]
pub unsafe extern "C" fn rename(_from: *const u8, _to: *const u8) -> i32 {
    set_errno(EACCES);
    -1
}

#[no_mangle]
pub unsafe extern "C" fn _wrename(_from: *const u16, _to: *const u16) -> i32 {
    set_errno(EACCES);
    -1
}

#[no_mangle]
pub unsafe extern "C" fn _wfullpath(buf: *mut u16, path: *const u16, size: usize) -> *mut u16 {
    let mut full = [0u16; 520];
    let n = unsafe { crate::GetFullPathNameW(path, full.len() as u32, full.as_mut_ptr(), core::ptr::null_mut()) } as usize;
    if n == 0 || n >= full.len() {
        errno_from_win32();
        return core::ptr::null_mut();
    }
    unsafe { give_wide(&full[..n], buf, if buf.is_null() { 0 } else { size }) }
}

#[no_mangle]
pub unsafe extern "C" fn _fullpath(buf: *mut u8, path: *const u8, size: usize) -> *mut u8 {
    let w = to_wide(unsafe { slice(path) });
    let full = unsafe { _wfullpath(core::ptr::null_mut(), w, 0) };
    heap::free(w as *mut u8);
    if full.is_null() {
        return core::ptr::null_mut();
    }
    let r = unsafe { give_narrow(slice(full), buf, if buf.is_null() { 0 } else { size }) };
    heap::free(full as *mut u8);
    r
}

/// Copy `part` into `out` (if given) with a terminator.
unsafe fn put_part<T: crate::string::Ch>(out: *mut T, part: &[T]) {
    if !out.is_null() {
        unsafe {
            core::ptr::copy_nonoverlapping(part.as_ptr(), out, part.len());
            *out.add(part.len()) = T::default();
        }
    }
}

unsafe fn split_path<T: crate::string::Ch>(path: *const T, drive: *mut T, dir: *mut T, name: *mut T, ext: *mut T) {
    let s = unsafe { slice(path) };
    let is = |c: T, b: u8| c.to_u32() == b as u32;
    let mut rest = s;
    if s.len() >= 2 && is(s[1], b':') {
        unsafe { put_part(drive, &s[..2]) };
        rest = &s[2..];
    } else {
        unsafe { put_part(drive, &[]) };
    }
    let slash = rest.iter().rposition(|&c| is(c, b'\\') || is(c, b'/')).map_or(0, |p| p + 1);
    unsafe { put_part(dir, &rest[..slash]) };
    let file = &rest[slash..];
    let dot = file.iter().rposition(|&c| is(c, b'.')).unwrap_or(file.len());
    unsafe {
        put_part(name, &file[..dot]);
        put_part(ext, &file[dot..]);
    }
}

#[no_mangle]
pub unsafe extern "C" fn _splitpath(path: *const u8, drive: *mut u8, dir: *mut u8, name: *mut u8, ext: *mut u8) {
    unsafe { split_path(path, drive, dir, name, ext) };
}

#[no_mangle]
pub unsafe extern "C" fn _wsplitpath(path: *const u16, drive: *mut u16, dir: *mut u16, name: *mut u16, ext: *mut u16) {
    unsafe { split_path(path, drive, dir, name, ext) };
}

#[no_mangle]
pub unsafe extern "C" fn _splitpath_s(path: *const u8, drive: *mut u8, _dn: usize, dir: *mut u8, _dirn: usize, name: *mut u8, _nn: usize, ext: *mut u8, _en: usize) -> i32 {
    unsafe { split_path(path, drive, dir, name, ext) };
    0
}

#[no_mangle]
pub unsafe extern "C" fn _wsplitpath_s(path: *const u16, drive: *mut u16, _dn: usize, dir: *mut u16, _dirn: usize, name: *mut u16, _nn: usize, ext: *mut u16, _en: usize) -> i32 {
    unsafe { split_path(path, drive, dir, name, ext) };
    0
}

unsafe fn make_path<T: crate::string::Ch>(out: *mut T, drive: *const T, dir: *const T, name: *const T, ext: *const T) {
    let mut n = 0;
    let mut push = |c: T| {
        unsafe { *out.add(n) = c };
        n += 1;
    };
    let is = |c: T, b: u8| c.to_u32() == b as u32;
    if !drive.is_null() {
        let d = unsafe { slice(drive) };
        if !d.is_empty() {
            push(d[0]);
            push(T::from_u32(b':' as u32));
        }
    }
    if !dir.is_null() {
        let d = unsafe { slice(dir) };
        d.iter().for_each(|&c| push(c));
        if let Some(&last) = d.last() {
            if !is(last, b'\\') && !is(last, b'/') {
                push(T::from_u32(b'\\' as u32));
            }
        }
    }
    if !name.is_null() {
        unsafe { slice(name) }.iter().for_each(|&c| push(c));
    }
    if !ext.is_null() {
        let e = unsafe { slice(ext) };
        if !e.is_empty() && !is(e[0], b'.') {
            push(T::from_u32(b'.' as u32));
        }
        e.iter().for_each(|&c| push(c));
    }
    push(T::default());
}

#[no_mangle]
pub unsafe extern "C" fn _makepath(out: *mut u8, drive: *const u8, dir: *const u8, name: *const u8, ext: *const u8) {
    unsafe { make_path(out, drive, dir, name, ext) };
}

#[no_mangle]
pub unsafe extern "C" fn _wmakepath(out: *mut u16, drive: *const u16, dir: *const u16, name: *const u16, ext: *const u16) {
    unsafe { make_path(out, drive, dir, name, ext) };
}

#[no_mangle]
pub unsafe extern "C" fn _makepath_s(out: *mut u8, _size: usize, drive: *const u8, dir: *const u8, name: *const u8, ext: *const u8) -> i32 {
    unsafe { make_path(out, drive, dir, name, ext) };
    0
}

#[no_mangle]
pub unsafe extern "C" fn _wmakepath_s(out: *mut u16, _size: usize, drive: *const u16, dir: *const u16, name: *const u16, ext: *const u16) -> i32 {
    unsafe { make_path(out, drive, dir, name, ext) };
    0
}

// -----| file status |-----

#[derive(Default)]
struct Status {
    mode: u16,
    size: i64,
    mtime: i64,
}

const S_IFDIR: u16 = 0x4000;
const S_IFCHR: u16 = 0x2000;
const S_IFREG: u16 = 0x8000;

fn filetime_to_unix(ft: u64) -> i64 {
    if ft == 0 { 0 } else { (ft.saturating_sub(116_444_736_000_000_000) / 10_000_000) as i64 }
}

unsafe fn status_of(path: *const u16) -> Option<Status> {
    let mut data = [0u8; 600]; // WIN32_FIND_DATAW
    let h = unsafe { crate::FindFirstFileW(path, data.as_mut_ptr()) };
    if h == u64::MAX {
        // FindFirstFile doesn't find roots like "C:\": fall back.
        let attrs = unsafe { crate::GetFileAttributesW(path) };
        if attrs == u32::MAX {
            errno_from_win32();
            return None;
        }
        return Some(Status { mode: if attrs & 0x10 != 0 { S_IFDIR | 0o555 } else { S_IFREG | 0o444 }, ..Status::default() });
    }
    unsafe { crate::FindClose(h) };
    let u32_at = |o: usize| u32::from_le_bytes(data[o..o + 4].try_into().unwrap());
    let attrs = u32_at(0);
    let write = u32_at(20) as u64 | (u32_at(24) as u64) << 32;
    let size = (u32_at(28) as i64) << 32 | u32_at(32) as i64;
    let mut mode = if attrs & 0x10 != 0 { S_IFDIR | 0o111 } else { S_IFREG };
    mode |= if attrs & 1 != 0 { 0o444 } else { 0o666 };
    Some(Status { mode, size, mtime: filetime_to_unix(write) })
}

/// `struct _stat64` (with 64-bit size) or `_stat64i32` (32-bit size).
unsafe fn fill(out: *mut u8, st: &Status, big: bool) {
    unsafe {
        core::ptr::write_bytes(out, 0, if big { 56 } else { 48 });
        *(out.add(4) as *mut u16) = 0; // st_ino
        *(out.add(6) as *mut u16) = st.mode;
        *(out.add(8) as *mut i16) = 1; // st_nlink
        *(out.add(16) as *mut u32) = 2; // st_rdev: drive C
        *(out as *mut u32) = 2; // st_dev
        let times = if big {
            *(out.add(24) as *mut i64) = st.size;
            32
        } else {
            *(out.add(20) as *mut i32) = st.size as i32;
            24
        };
        for k in 0..3 {
            *(out.add(times + 8 * k) as *mut i64) = st.mtime;
        }
    }
}

macro_rules! stat_fns {
    ($($n:ident / $w:ident = $big:expr;)*) => {$(
        #[no_mangle]
        pub unsafe extern "C" fn $w(path: *const u16, out: *mut u8) -> i32 {
            match unsafe { status_of(path) } {
                Some(st) => {
                    unsafe { fill(out, &st, $big) };
                    0
                }
                None => -1,
            }
        }
        #[no_mangle]
        pub unsafe extern "C" fn $n(path: *const u8, out: *mut u8) -> i32 {
            unsafe { with_wide(path, |w| $w(w, out)) }
        }
    )*};
}

stat_fns! {
    _stat64 / _wstat64 = true;
    _stat64i32 / _wstat64i32 = false;
    _stat / _wstat = false;
    _stati64 / _wstati64 = true;
}

macro_rules! fstat_fns {
    ($($n:ident = $big:expr;)*) => {$(
        #[no_mangle]
        pub unsafe extern "C" fn $n(fd: i32, out: *mut u8) -> i32 {
            let h = crate::stdio::_get_osfhandle(fd);
            if h == -1 {
                return -1;
            }
            let device = unsafe { crate::GetFileType(h as u64) } != 1;
            let st = if device {
                Status { mode: S_IFCHR | 0o666, ..Status::default() }
            } else {
                let mut size = 0i64;
                unsafe { crate::GetFileSizeEx(h as u64, &mut size) };
                Status { mode: S_IFREG | 0o666, size, mtime: 0 }
            };
            unsafe { fill(out, &st, $big) };
            0
        }
    )*};
}

fstat_fns! {
    _fstat64 = true;
    _fstat64i32 = false;
    _fstat = false;
    _fstati64 = true;
}

#[no_mangle]
pub extern "C" fn _umask(_mode: i32) -> i32 {
    0
}

#[no_mangle]
pub unsafe extern "C" fn _umask_s(_mode: i32, old: *mut i32) -> i32 {
    unsafe { *old = 0 };
    0
}

#[no_mangle]
pub unsafe extern "C" fn _chmod(path: *const u8, _mode: i32) -> i32 {
    unsafe { _access(path, 0) }
}

#[no_mangle]
pub unsafe extern "C" fn _wchmod(path: *const u16, _mode: i32) -> i32 {
    unsafe { _waccess(path, 0) }
}

// -----| locale: always "C" |-----

static C_LOCALE: [u8; 2] = *b"C\0";
static C_LOCALE_WIDE: [u16; 2] = [b'C' as u16, 0];

#[no_mangle]
pub extern "C" fn setlocale(_category: i32, _locale: *const u8) -> *const u8 {
    C_LOCALE.as_ptr()
}

#[no_mangle]
pub extern "C" fn _wsetlocale(_category: i32, _locale: *const u16) -> *const u16 {
    C_LOCALE_WIDE.as_ptr()
}

#[repr(C)]
pub struct Lconv {
    strings: [*const u8; 10],
    chars: [u8; 8],
    wide: [*const u16; 10],
}

static DOT: [u8; 2] = *b".\0";
static EMPTY: [u8; 1] = [0];
static WIDE_DOT: [u16; 2] = [b'.' as u16, 0];
static WIDE_EMPTY: [u16; 1] = [0];

static mut LCONV: Lconv = Lconv { strings: [core::ptr::null(); 10], chars: [127; 8], wide: [core::ptr::null(); 10] };

#[no_mangle]
pub extern "C" fn localeconv() -> *mut Lconv {
    unsafe {
        let l = &mut *(&raw mut LCONV);
        for (i, s) in l.strings.iter_mut().enumerate() {
            *s = if i == 0 { DOT.as_ptr() } else { EMPTY.as_ptr() };
        }
        for (i, s) in l.wide.iter_mut().enumerate() {
            *s = if i == 0 { WIDE_DOT.as_ptr() } else { WIDE_EMPTY.as_ptr() };
        }
        l
    }
}

#[no_mangle]
pub extern "C" fn ___lc_codepage_func() -> u32 {
    65001
}

#[no_mangle]
pub extern "C" fn ___lc_collate_cp_func() -> u32 {
    65001
}

#[no_mangle]
pub extern "C" fn ___lc_locale_name_func() -> *const *const u16 {
    static NAMES: [usize; 6] = [0; 6];
    NAMES.as_ptr() as *const *const u16
}

#[no_mangle]
pub extern "C" fn _create_locale(_category: i32, _locale: *const u8) -> usize {
    1
}

#[no_mangle]
pub extern "C" fn _wcreate_locale(_category: i32, _locale: *const u16) -> usize {
    1
}

#[no_mangle]
pub extern "C" fn _free_locale(_l: usize) {}

#[no_mangle]
pub extern "C" fn _get_current_locale() -> usize {
    1
}

#[no_mangle]
pub extern "C" fn _getmbcp() -> i32 {
    65001
}

#[no_mangle]
pub extern "C" fn _setmbcp(_cp: i32) -> i32 {
    0
}

// -----| setjmp / longjmp |-----
//
// The x64 _JUMP_BUFFER: Frame, Rbx, Rsp, Rbp, Rsi, Rdi, R12-R15, Rip,
// MxCsr, FpCsr, Spare, Xmm6-Xmm15. longjmp restores registers directly
// (no unwinding through frames between: that's W5b).

#[unsafe(naked)]
#[no_mangle]
pub unsafe extern "C" fn _setjmp() {
    core::arch::naked_asm!(
        "mov [rcx], rdx",
        "mov [rcx + 8], rbx",
        "lea rax, [rsp + 8]",
        "mov [rcx + 16], rax",
        "mov [rcx + 24], rbp",
        "mov [rcx + 32], rsi",
        "mov [rcx + 40], rdi",
        "mov [rcx + 48], r12",
        "mov [rcx + 56], r13",
        "mov [rcx + 64], r14",
        "mov [rcx + 72], r15",
        "mov rax, [rsp]",
        "mov [rcx + 80], rax",
        "stmxcsr [rcx + 88]",
        "fnstcw [rcx + 92]",
        "movdqu [rcx + 96], xmm6",
        "movdqu [rcx + 112], xmm7",
        "movdqu [rcx + 128], xmm8",
        "movdqu [rcx + 144], xmm9",
        "movdqu [rcx + 160], xmm10",
        "movdqu [rcx + 176], xmm11",
        "movdqu [rcx + 192], xmm12",
        "movdqu [rcx + 208], xmm13",
        "movdqu [rcx + 224], xmm14",
        "movdqu [rcx + 240], xmm15",
        "xor eax, eax",
        "ret",
    )
}

#[unsafe(naked)]
#[no_mangle]
pub unsafe extern "C" fn _setjmpex() {
    core::arch::naked_asm!("jmp {s}", s = sym _setjmp)
}

/// What MSVC's setjmp compiles to (vcruntime's names).
#[unsafe(naked)]
#[no_mangle]
pub unsafe extern "C" fn __intrinsic_setjmp() {
    core::arch::naked_asm!("jmp {s}", s = sym _setjmp)
}

#[unsafe(naked)]
#[no_mangle]
pub unsafe extern "C" fn __intrinsic_setjmpex() {
    core::arch::naked_asm!("jmp {s}", s = sym _setjmp)
}

#[unsafe(naked)]
#[no_mangle]
pub unsafe extern "C" fn longjmp() {
    core::arch::naked_asm!(
        "mov rbx, [rcx + 8]",
        "mov rbp, [rcx + 24]",
        "mov rsi, [rcx + 32]",
        "mov rdi, [rcx + 40]",
        "mov r12, [rcx + 48]",
        "mov r13, [rcx + 56]",
        "mov r14, [rcx + 64]",
        "mov r15, [rcx + 72]",
        "ldmxcsr [rcx + 88]",
        "fldcw [rcx + 92]",
        "movdqu xmm6, [rcx + 96]",
        "movdqu xmm7, [rcx + 112]",
        "movdqu xmm8, [rcx + 128]",
        "movdqu xmm9, [rcx + 144]",
        "movdqu xmm10, [rcx + 160]",
        "movdqu xmm11, [rcx + 176]",
        "movdqu xmm12, [rcx + 192]",
        "movdqu xmm13, [rcx + 208]",
        "movdqu xmm14, [rcx + 224]",
        "movdqu xmm15, [rcx + 240]",
        "mov eax, edx",
        "test eax, eax",
        "jnz 2f",
        "mov eax, 1",
        "2:",
        "mov rsp, [rcx + 16]",
        "jmp qword ptr [rcx + 80]",
    )
}

// -----| operator new / delete (msvcrt's) |-----

#[export_name = "??2@YAPEAX_K@Z"]
pub extern "C" fn operator_new(size: usize) -> *mut u8 {
    let p = heap::malloc(size);
    if p.is_null() {
        crate::startup::fatal("out of memory (operator new)", 3);
    }
    p
}

#[export_name = "??_U@YAPEAX_K@Z"]
pub extern "C" fn operator_new_array(size: usize) -> *mut u8 {
    operator_new(size)
}

#[export_name = "??3@YAXPEAX@Z"]
pub extern "C" fn operator_delete(p: *mut u8) {
    heap::free(p);
}

#[export_name = "??_V@YAXPEAX@Z"]
pub extern "C" fn operator_delete_array(p: *mut u8) {
    heap::free(p);
}

#[export_name = "??3@YAXPEAX_K@Z"]
pub extern "C" fn operator_delete_sized(p: *mut u8, _size: usize) {
    heap::free(p);
}

#[export_name = "??_V@YAXPEAX_K@Z"]
pub extern "C" fn operator_delete_array_sized(p: *mut u8, _size: usize) {
    heap::free(p);
}
