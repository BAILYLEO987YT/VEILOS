//! File descriptors, FILE streams, and the printf / scanf entry points.
//!
//! Descriptors (`_open`, `_read`, ...) wrap Win32 handles; text mode turns
//! "\n" into "\r\n" on the way out and back on the way in.
//!
//! A FILE starts with msvcrt's 48-byte layout, so `_iob[i]`, `_fileno` and
//! old programs' getc/putc/feof macros work. `_cnt` stays 0, which sends
//! those macros to `_filbuf` / `_flsbuf`; the buffers themselves live in
//! `STATE`, beside the FILEs. Streams on a console are written out at the
//! end of each call.

use crate::errno::{EBADF, EINVAL, EMFILE};
use crate::format::{self, Out};
use crate::scan::{self, In, StrIn};
use crate::string::{len, Ch};
use crate::{convert, errno_from_win32, heap, invalid_parameter, set_errno, variadic, VaList};
use crate::{CloseHandle, CreateFileW, FlushFileBuffers, GetFileSizeEx, GetFileType, GetStdHandle, ReadFile, SetEndOfFile, SetFilePointerEx, WriteFile};

const EOF: i32 = -1;
const INVALID: u64 = u64::MAX;

// -----| file descriptors |-----

pub const O_WRONLY: i32 = 0x1;
pub const O_RDWR: i32 = 0x2;
pub const O_APPEND: i32 = 0x8;
pub const O_CREAT: i32 = 0x100;
pub const O_TRUNC: i32 = 0x200;
pub const O_EXCL: i32 = 0x400;
pub const O_TEXT: i32 = 0x4000;
pub const O_BINARY: i32 = 0x8000;
pub const O_WTEXT: i32 = 0x10000;
pub const O_U16TEXT: i32 = 0x20000;
pub const O_U8TEXT: i32 = 0x40000;
const TEXT_MODES: i32 = O_TEXT | O_BINARY | O_WTEXT | O_U16TEXT | O_U8TEXT;

const MAX_FDS: usize = 256;

#[derive(Clone, Copy)]
struct Fd {
    handle: u64,
    open: bool,
    /// O_TEXT, O_BINARY, O_WTEXT, O_U16TEXT or O_U8TEXT.
    mode: i32,
    append: bool,
    device: bool,
    eof: bool,
    /// A byte read ahead while looking for "\r\n".
    peek: Option<u8>,
}

const CLOSED: Fd = Fd { handle: INVALID, open: false, mode: O_TEXT, append: false, device: false, eof: false, peek: None };
static mut FDS: [Fd; MAX_FDS] = [CLOSED; MAX_FDS];

fn fds() -> &'static mut [Fd; MAX_FDS] {
    unsafe { &mut *(&raw mut FDS) }
}

fn fd_entry(fd: i32) -> Option<&'static mut Fd> {
    let e = fds().get_mut(usize::try_from(fd).ok()?)?;
    if e.open { Some(e) } else { None }
}

fn bad_fd() -> i32 {
    set_errno(EBADF);
    -1
}

/// Descriptors 0-2 are the process's standard handles too (as msvcrt
/// keeps them): a child started with inherited handles gets the same ones.
fn set_std(fd: usize, handle: u64) {
    if fd < 3 {
        unsafe { crate::SetStdHandle(0xFFFF_FFF6 - fd as u32, handle) };
    }
}

fn new_fd(handle: u64, mode: i32, append: bool) -> i32 {
    let Some(i) = fds().iter().position(|f| !f.open) else {
        set_errno(EMFILE);
        return -1;
    };
    let mode = if mode & TEXT_MODES == 0 { default_mode() } else { mode & TEXT_MODES };
    fds()[i] = Fd {
        handle,
        open: true,
        mode,
        append,
        device: unsafe { GetFileType(handle) } == 2,
        eof: false,
        peek: None,
    };
    set_std(i, handle);
    i as i32
}

/// The descriptor's bytes, untranslated.
fn raw_read(f: &mut Fd, buf: *mut u8, n: usize) -> i32 {
    let mut got = 0;
    let mut dst = buf;
    let mut n = n;
    if n > 0 {
        if let Some(b) = f.peek.take() {
            unsafe { *dst = b };
            dst = unsafe { dst.add(1) };
            n -= 1;
            got = 1;
        }
    }
    if n == 0 {
        return got as i32;
    }
    let mut read = 0u32;
    if unsafe { ReadFile(f.handle, dst, n.min(u32::MAX as usize) as u32, &mut read, 0) } == 0 {
        // A closed pipe is the end of the input, not an error.
        if unsafe { crate::GetLastError() } == 109 {
            return got as i32;
        }
        errno_from_win32();
        return if got > 0 { got as i32 } else { -1 };
    }
    got as i32 + read as i32
}

fn raw_write(f: &mut Fd, buf: *const u8, n: usize) -> bool {
    if f.append {
        unsafe { SetFilePointerEx(f.handle, 0, core::ptr::null_mut(), 2) };
    }
    let mut done = 0;
    while done < n {
        let mut written = 0u32;
        let chunk = (n - done).min(1 << 30) as u32;
        if unsafe { WriteFile(f.handle, buf.add(done), chunk, &mut written, 0) } == 0 || written == 0 {
            errno_from_win32();
            return false;
        }
        done += written as usize;
    }
    true
}

/// Wide (UTF-16) text for a descriptor in a wide mode: a console or an
/// O_U8TEXT descriptor takes UTF-8, a file O_U16TEXT takes it as it is.
fn write_wide_mode(f: &mut Fd, buf: *const u8, n: usize) -> bool {
    if f.mode == O_U8TEXT || f.device {
        let units = unsafe { core::slice::from_raw_parts(buf as *const u16, n / 2) };
        let mut out = alloc::vec::Vec::with_capacity(units.len());
        convert::utf16_to_utf8(units, |b| out.push(b));
        return raw_write(f, out.as_ptr(), out.len());
    }
    raw_write(f, buf, n)
}

#[no_mangle]
pub unsafe extern "C" fn _write(fd: i32, buf: *const u8, n: u32) -> i32 {
    let Some(f) = fd_entry(fd) else { return bad_fd() };
    let n = n as usize;
    if f.mode & (O_WTEXT | O_U16TEXT | O_U8TEXT) != 0 {
        return if write_wide_mode(f, buf, n) { n as i32 } else { -1 };
    }
    if f.mode != O_TEXT {
        return if raw_write(f, buf, n) { n as i32 } else { -1 };
    }
    // Text: "\n" becomes "\r\n".
    let mut tmp = [0u8; 1024];
    let mut k = 0;
    for i in 0..n {
        let b = unsafe { *buf.add(i) };
        if b == b'\n' {
            tmp[k] = b'\r';
            k += 1;
        }
        tmp[k] = b;
        k += 1;
        if k >= tmp.len() - 1 {
            if !raw_write(f, tmp.as_ptr(), k) {
                return -1;
            }
            k = 0;
        }
    }
    if k > 0 && !raw_write(f, tmp.as_ptr(), k) {
        return -1;
    }
    n as i32
}

#[no_mangle]
pub unsafe extern "C" fn _read(fd: i32, buf: *mut u8, n: u32) -> i32 {
    let Some(f) = fd_entry(fd) else { return bad_fd() };
    if f.eof && !f.device {
        return 0;
    }
    let got = raw_read(f, buf, n as usize);
    if got <= 0 || f.mode != O_TEXT {
        return got;
    }
    // Text: "\r\n" becomes "\n"; Ctrl+Z ends a file.
    let data = unsafe { core::slice::from_raw_parts_mut(buf, got as usize) };
    let mut k = 0;
    let mut i = 0;
    while i < data.len() {
        let b = data[i];
        if b == 0x1A && !f.device {
            f.eof = true;
            break;
        }
        if b == b'\r' {
            if i + 1 < data.len() {
                if data[i + 1] == b'\n' {
                    i += 1;
                    continue;
                }
            } else {
                let mut next = 0u8;
                if raw_read(f, &mut next, 1) == 1 {
                    if next == b'\n' {
                        data[k] = b'\n';
                        k += 1;
                        break;
                    }
                    f.peek = Some(next);
                }
            }
        }
        data[k] = b;
        k += 1;
        i += 1;
    }
    k as i32
}

/// UTF-16 path to a handle, as _open's flags say.
unsafe fn open_handle(path: *const u16, flags: i32) -> u64 {
    let access = match flags & 3 {
        0 => 0x8000_0000,
        1 => 0x4000_0000,
        _ => 0xC000_0000,
    };
    let creation = match (flags & O_CREAT != 0, flags & O_EXCL != 0, flags & O_TRUNC != 0) {
        (true, true, _) => 1,      // CREATE_NEW
        (true, false, true) => 2,  // CREATE_ALWAYS
        (true, false, false) => 4, // OPEN_ALWAYS
        (false, _, true) => 5,     // TRUNCATE_EXISTING
        _ => 3,                    // OPEN_EXISTING
    };
    unsafe { CreateFileW(path, access, 7, 0, creation, 0x80, 0) }
}

#[no_mangle]
pub unsafe extern "C" fn _wopen(path: *const u16, flags: i32, _mode: i32) -> i32 {
    let h = unsafe { open_handle(path, flags) };
    if h == INVALID {
        errno_from_win32();
        return -1;
    }
    let fd = new_fd(h, flags, flags & O_APPEND != 0);
    if fd < 0 {
        unsafe { CloseHandle(h) };
    }
    fd
}

#[no_mangle]
pub unsafe extern "C" fn _open(path: *const u8, flags: i32, mode: i32) -> i32 {
    let w = convert::to_wide(unsafe { crate::string::slice(path) });
    let fd = unsafe { _wopen(w, flags, mode) };
    heap::free(w as *mut u8);
    fd
}

#[no_mangle]
pub unsafe extern "C" fn _sopen(path: *const u8, flags: i32, _share: i32, mode: i32) -> i32 {
    unsafe { _open(path, flags, mode) }
}

#[no_mangle]
pub unsafe extern "C" fn _wsopen(path: *const u16, flags: i32, _share: i32, mode: i32) -> i32 {
    unsafe { _wopen(path, flags, mode) }
}

#[no_mangle]
pub unsafe extern "C" fn _sopen_s(fd: *mut i32, path: *const u8, flags: i32, _share: i32, mode: i32) -> i32 {
    let r = unsafe { _open(path, flags, mode) };
    unsafe { *fd = r };
    if r < 0 { unsafe { *crate::_errno() } } else { 0 }
}

#[no_mangle]
pub unsafe extern "C" fn _wsopen_s(fd: *mut i32, path: *const u16, flags: i32, _share: i32, mode: i32) -> i32 {
    let r = unsafe { _wopen(path, flags, mode) };
    unsafe { *fd = r };
    if r < 0 { unsafe { *crate::_errno() } } else { 0 }
}

#[no_mangle]
pub unsafe extern "C" fn _close(fd: i32) -> i32 {
    let Some(f) = fd_entry(fd) else { return bad_fd() };
    let h = f.handle;
    *f = CLOSED;
    set_std(fd as usize, 0);
    // Descriptors made by _dup share the handle: close it with the last.
    if !fds().iter().any(|o| o.open && o.handle == h) {
        unsafe { CloseHandle(h) };
    }
    0
}

#[no_mangle]
pub unsafe extern "C" fn _dup(fd: i32) -> i32 {
    let Some(f) = fd_entry(fd) else { return bad_fd() };
    let copy = *f;
    let Some(i) = fds().iter().position(|f| !f.open) else {
        set_errno(EMFILE);
        return -1;
    };
    fds()[i] = Fd { peek: None, ..copy };
    i as i32
}

#[no_mangle]
pub unsafe extern "C" fn _dup2(fd: i32, target: i32) -> i32 {
    let Some(f) = fd_entry(fd) else { return bad_fd() };
    let copy = *f;
    if !(0..MAX_FDS as i32).contains(&target) {
        return bad_fd();
    }
    if target == fd {
        return 0;
    }
    if fds()[target as usize].open {
        unsafe { _close(target) };
    }
    fds()[target as usize] = Fd { peek: None, ..copy };
    set_std(target as usize, copy.handle);
    0
}

#[no_mangle]
pub extern "C" fn _get_osfhandle(fd: i32) -> isize {
    match fd_entry(fd) {
        Some(f) => f.handle as isize,
        None => {
            bad_fd();
            -1
        }
    }
}

#[no_mangle]
pub extern "C" fn _open_osfhandle(handle: isize, flags: i32) -> i32 {
    new_fd(handle as u64, flags, flags & O_APPEND != 0)
}

#[no_mangle]
pub extern "C" fn _setmode(fd: i32, mode: i32) -> i32 {
    let Some(f) = fd_entry(fd) else { return bad_fd() };
    if mode & TEXT_MODES == 0 {
        invalid_parameter();
        return -1;
    }
    core::mem::replace(&mut f.mode, mode & TEXT_MODES)
}

#[no_mangle]
pub extern "C" fn _isatty(fd: i32) -> i32 {
    fd_entry(fd).map_or(0, |f| f.device as i32)
}

#[no_mangle]
pub extern "C" fn _lseeki64(fd: i32, offset: i64, whence: i32) -> i64 {
    let Some(f) = fd_entry(fd) else { return bad_fd() as i64 };
    let mut pos = 0i64;
    let offset = if whence == 1 && f.peek.is_some() { offset - 1 } else { offset };
    f.peek = None;
    if unsafe { SetFilePointerEx(f.handle, offset, &mut pos, whence as u32) } == 0 {
        errno_from_win32();
        return -1;
    }
    f.eof = false;
    pos
}

#[no_mangle]
pub extern "C" fn _lseek(fd: i32, offset: i32, whence: i32) -> i32 {
    _lseeki64(fd, offset as i64, whence) as i32
}

#[no_mangle]
pub extern "C" fn _telli64(fd: i32) -> i64 {
    _lseeki64(fd, 0, 1)
}

#[no_mangle]
pub extern "C" fn _tell(fd: i32) -> i32 {
    _lseeki64(fd, 0, 1) as i32
}

#[no_mangle]
pub extern "C" fn _filelengthi64(fd: i32) -> i64 {
    let Some(f) = fd_entry(fd) else { return bad_fd() as i64 };
    let mut size = 0i64;
    if unsafe { GetFileSizeEx(f.handle, &mut size) } == 0 {
        errno_from_win32();
        return -1;
    }
    size
}

#[no_mangle]
pub extern "C" fn _filelength(fd: i32) -> i32 {
    _filelengthi64(fd) as i32
}

#[no_mangle]
pub extern "C" fn _eof(fd: i32) -> i32 {
    let Some(f) = fd_entry(fd) else { return bad_fd() };
    if f.eof {
        return 1;
    }
    let here = _telli64(fd);
    (here >= _filelengthi64(fd)) as i32
}

#[no_mangle]
pub extern "C" fn _chsize_s(fd: i32, size: i64) -> i32 {
    let Some(f) = fd_entry(fd) else { return EBADF };
    let h = f.handle;
    let here = _telli64(fd);
    unsafe {
        SetFilePointerEx(h, size, core::ptr::null_mut(), 0);
        let ok = SetEndOfFile(h);
        SetFilePointerEx(h, here.min(size), core::ptr::null_mut(), 0);
        if ok == 0 {
            errno_from_win32();
            return *crate::_errno();
        }
    }
    0
}

#[no_mangle]
pub extern "C" fn _chsize(fd: i32, size: i32) -> i32 {
    if _chsize_s(fd, size as i64) == 0 { 0 } else { -1 }
}

#[no_mangle]
pub extern "C" fn _commit(fd: i32) -> i32 {
    let Some(f) = fd_entry(fd) else { return bad_fd() };
    unsafe { FlushFileBuffers(f.handle) };
    0
}

#[no_mangle]
pub extern "C" fn _locking(_fd: i32, _mode: i32, _n: i32) -> i32 {
    0
}

// -----| _fmode, _commode |-----

#[no_mangle]
pub static mut _fmode: i32 = 0;
#[no_mangle]
pub static mut _commode: i32 = 0;

/// The text/binary mode for a descriptor opened without one.
fn default_mode() -> i32 {
    if unsafe { _fmode } == O_BINARY { O_BINARY } else { O_TEXT }
}

#[no_mangle]
pub extern "C" fn __p__fmode() -> *mut i32 {
    &raw mut _fmode
}

#[no_mangle]
pub extern "C" fn __p__commode() -> *mut i32 {
    &raw mut _commode
}

#[no_mangle]
pub extern "C" fn _set_fmode(mode: i32) -> i32 {
    if mode != O_TEXT && mode != O_BINARY && mode != 0 {
        invalid_parameter();
        return EINVAL;
    }
    unsafe { _fmode = mode };
    0
}

#[no_mangle]
pub unsafe extern "C" fn _get_fmode(mode: *mut i32) -> i32 {
    unsafe { *mode = _fmode };
    0
}

// -----| streams |-----

/// msvcrt's FILE.
#[repr(C)]
pub struct File {
    ptr: *mut u8,
    cnt: i32,
    base: *mut u8,
    flag: i32,
    file: i32,
    charbuf: i32,
    bufsiz: i32,
    tmpfname: *mut u8,
}

const IOREAD: i32 = 0x1;
const IOWRT: i32 = 0x2;
const IOEOF: i32 = 0x10;
const IOERR: i32 = 0x20;
const IORW: i32 = 0x80;

const MAX_STREAMS: usize = 512;
const BUF_SIZE: usize = 4096;

/// What a stream's buffer holds.
#[derive(Clone, Copy, PartialEq)]
enum Pending {
    Nothing,
    Reading,
    Writing,
}

#[derive(Clone, Copy)]
struct State {
    in_use: bool,
    buf: *mut u8,
    pos: usize,
    end: usize,
    pending: Pending,
    unbuffered: bool,
    text: bool,
    /// Pushed back by ungetc (most recent last).
    ungot: [u32; 2],
    ungot_n: usize,
    /// The second half of a surrogate pair fgetwc read.
    low_surrogate: Option<u16>,
}

const FREE: State = State {
    in_use: false,
    buf: core::ptr::null_mut(),
    pos: 0,
    end: 0,
    pending: Pending::Nothing,
    unbuffered: false,
    text: true,
    ungot: [0; 2],
    ungot_n: 0,
    low_surrogate: None,
};

const NO_FILE: File = File {
    ptr: core::ptr::null_mut(),
    cnt: 0,
    base: core::ptr::null_mut(),
    flag: 0,
    file: -1,
    charbuf: 0,
    bufsiz: 0,
    tmpfname: core::ptr::null_mut(),
};

/// The streams: msvcrt programs index `_iob` directly for stdin, stdout and
/// stderr.
#[no_mangle]
pub static mut _iob: [File; MAX_STREAMS] = [NO_FILE; MAX_STREAMS];
static mut STATE: [State; MAX_STREAMS] = [FREE; MAX_STREAMS];

fn index(f: *mut File) -> Option<usize> {
    let base = &raw mut _iob as usize;
    let off = (f as usize).wrapping_sub(base);
    if f.is_null() || off % size_of::<File>() != 0 || off / size_of::<File>() >= MAX_STREAMS {
        return None;
    }
    let i = off / size_of::<File>();
    if unsafe { STATE[i].in_use } { Some(i) } else { None }
}

fn stream(i: usize) -> *mut File {
    unsafe { (&raw mut _iob as *mut File).add(i) }
}

/// A stream, with the stdio lock held (threads.rs).
struct S {
    f: &'static mut File,
    s: &'static mut State,
    _lock: crate::threads::Locked,
}

fn get(f: *mut File) -> Option<S> {
    let lock = crate::threads::Locked::new();
    let i = index(f)?;
    Some(S { f: unsafe { &mut *stream(i) }, s: unsafe { &mut (*(&raw mut STATE))[i] }, _lock: lock })
}

/// Descriptors a parent C runtime passed in STARTUPINFO.lpReserved2:
/// a count, a flags byte each, then a handle each.
fn inherit_descriptors() {
    let mut si = [0u8; 104];
    unsafe { crate::GetStartupInfoW(si.as_mut_ptr()) };
    let size = u16::from_le_bytes([si[66], si[67]]) as usize;
    let data = u64::from_le_bytes(si[72..80].try_into().unwrap());
    if size < 4 || data == 0 {
        return;
    }
    let count = (unsafe { core::ptr::read_unaligned(data as *const i32) }.max(0) as usize).min(MAX_FDS).min((size - 4) / 9);
    for i in 0..count {
        let flags = unsafe { *((data + 4 + i as u64) as *const u8) };
        let h = unsafe { core::ptr::read_unaligned((data + 4 + count as u64 + 8 * i as u64) as *const u64) };
        // FOPEN, and not a pipe or device we'd mistake: take it.
        if flags & 0x01 == 0 || h == 0 || h == INVALID {
            continue;
        }
        fds()[i] = Fd {
            handle: h,
            open: true,
            mode: if flags & 0x80 != 0 { O_TEXT } else { O_BINARY },
            append: flags & 0x20 != 0,
            device: unsafe { GetFileType(h) } == 2,
            eof: false,
            peek: None,
        };
    }
}

pub fn init() {
    inherit_descriptors();
    for (i, which) in [(0, 0xFFFF_FFF6u32), (1, 0xFFFF_FFF5), (2, 0xFFFF_FFF4)] {
        if fds()[i].open {
            unsafe {
                STATE[i] = State { in_use: true, unbuffered: i == 2, text: fds()[i].mode != O_BINARY, ..FREE };
                _iob[i] = File { file: i as i32, flag: if i == 0 { IOREAD } else { IOWRT }, ..NO_FILE };
            }
            continue;
        }
        let h = unsafe { GetStdHandle(which) };
        fds()[i] = Fd {
            handle: h,
            open: h != 0 && h != INVALID,
            mode: O_TEXT,
            append: false,
            device: unsafe { GetFileType(h) } == 2,
            eof: false,
            peek: None,
        };
        unsafe {
            STATE[i] = State { in_use: true, unbuffered: i == 2, ..FREE };
            _iob[i] = File { file: i as i32, flag: if i == 0 { IOREAD } else { IOWRT }, ..NO_FILE };
        }
    }
}

impl S {
    fn fd(&mut self) -> Option<&'static mut Fd> {
        fd_entry(self.f.file)
    }

    fn buffer(&mut self) -> *mut u8 {
        if self.s.buf.is_null() {
            self.s.buf = heap::malloc(BUF_SIZE);
        }
        self.s.buf
    }

    /// Write out what's buffered; drop read-ahead (moving the descriptor
    /// back over it).
    fn flush(&mut self) -> bool {
        match self.s.pending {
            Pending::Writing => {
                let (buf, n) = (self.s.buf, self.s.pos);
                self.s.pos = 0;
                self.s.end = 0;
                self.s.pending = Pending::Nothing;
                let Some(fd) = self.fd() else { return false };
                if n > 0 && !raw_write(fd, buf, n) {
                    self.f.flag |= IOERR;
                    return false;
                }
            }
            Pending::Reading => {
                let unread = (self.s.end - self.s.pos) as i64;
                self.s.pos = 0;
                self.s.end = 0;
                self.s.pending = Pending::Nothing;
                if unread > 0 && !self.fd().is_some_and(|f| f.device) {
                    _lseeki64(self.f.file, -unread, 1);
                }
            }
            Pending::Nothing => {}
        }
        true
    }

    /// End of a write call: a console sees it now.
    fn done_writing(&mut self) {
        if self.s.unbuffered || self.fd().is_some_and(|f| f.device) {
            self.flush();
        }
    }

    fn put_raw(&mut self, b: u8) -> bool {
        if self.s.pending == Pending::Reading {
            self.flush();
        }
        let buf = self.buffer();
        if buf.is_null() {
            return false;
        }
        self.s.pending = Pending::Writing;
        unsafe { *buf.add(self.s.pos) = b };
        self.s.pos += 1;
        if self.s.pos == BUF_SIZE {
            return self.flush();
        }
        true
    }

    /// One byte out, "\n" as "\r\n" in text mode.
    fn put(&mut self, b: u8) -> bool {
        if self.f.flag & (IOWRT | IORW) == 0 {
            self.f.flag |= IOERR;
            set_errno(EBADF);
            return false;
        }
        if b == b'\n' && self.s.text && !self.put_raw(b'\r') {
            return false;
        }
        self.put_raw(b)
    }

    fn fill(&mut self) -> bool {
        if self.s.pending == Pending::Writing {
            self.flush();
        }
        if self.f.flag & IOEOF != 0 && !self.fd().is_some_and(|f| f.device) {
            return false;
        }
        let buf = self.buffer();
        let Some(fd) = self.fd() else { return false };
        if buf.is_null() {
            return false;
        }
        let n = raw_read(fd, buf, BUF_SIZE);
        if n <= 0 {
            self.f.flag |= if n < 0 { IOERR } else { IOEOF };
            return false;
        }
        self.s.pos = 0;
        self.s.end = n as usize;
        self.s.pending = Pending::Reading;
        true
    }

    fn get_raw(&mut self) -> Option<u8> {
        if self.s.pending != Pending::Reading || self.s.pos == self.s.end {
            if !self.fill() {
                return None;
            }
        }
        let b = unsafe { *self.s.buf.add(self.s.pos) };
        self.s.pos += 1;
        Some(b)
    }

    /// One byte in, "\r\n" as "\n" in text mode.
    fn get(&mut self) -> Option<u8> {
        if self.s.ungot_n > 0 {
            self.s.ungot_n -= 1;
            return Some(self.s.ungot[self.s.ungot_n] as u8);
        }
        if self.f.flag & (IOREAD | IORW) == 0 {
            self.f.flag |= IOERR;
            return None;
        }
        let b = self.get_raw()?;
        if self.s.text {
            if b == 0x1A && !self.fd().is_some_and(|f| f.device) {
                self.s.pos -= 1;
                self.f.flag |= IOEOF;
                return None;
            }
            if b == b'\r' {
                match self.get_raw() {
                    Some(b'\n') => return Some(b'\n'),
                    Some(_) => self.s.pos -= 1,
                    None => {}
                }
            }
        }
        Some(b)
    }

    /// Is the descriptor in a mode that stores UTF-16 as it is?
    fn utf16_file(&mut self) -> bool {
        self.fd().is_some_and(|f| f.mode & (O_WTEXT | O_U16TEXT) != 0 && !f.device)
    }

    fn put_wide(&mut self, c: u16) -> bool {
        if self.utf16_file() {
            return self.put_raw(c as u8) && self.put_raw((c >> 8) as u8);
        }
        // A lone high surrogate waits for its partner.
        if (0xD800..0xDC00).contains(&c) {
            self.s.low_surrogate = Some(c);
            return true;
        }
        let code = match self.s.low_surrogate.take() {
            Some(hi) if (0xDC00..0xE000).contains(&c) => 0x10000 + ((hi as u32 - 0xD800) << 10) + (c as u32 - 0xDC00),
            _ => c as u32,
        };
        let mut ok = true;
        convert::encode_utf8(code, &mut |b| ok &= self.put(b));
        ok
    }

    fn get_wide(&mut self) -> Option<u16> {
        if let Some(lo) = self.s.low_surrogate.take() {
            return Some(lo);
        }
        if self.utf16_file() {
            let lo = self.get_raw()?;
            let hi = self.get_raw()?;
            return Some(u16::from_le_bytes([lo, hi]));
        }
        let first = self.get()?;
        let need = match first {
            0xC2..=0xDF => 2,
            0xE0..=0xEF => 3,
            0xF0..=0xF4 => 4,
            _ => 1,
        };
        let mut bytes = [first, 0, 0, 0];
        for k in 1..need {
            bytes[k] = self.get()?;
        }
        let (c, _) = convert::decode_utf8(&bytes[..need]);
        let mut units = [0u16; 2];
        let mut n = 0;
        convert::encode_utf16(c, &mut |u| {
            units[n] = u;
            n += 1;
        });
        if n == 2 {
            self.s.low_surrogate = Some(units[1]);
        }
        Some(units[0])
    }
}

/// Every stream's buffered output, written out (exit does this).
pub fn flush_all() -> i32 {
    let mut n = 0;
    for i in 0..MAX_STREAMS {
        if let Some(mut s) = get(stream(i)) {
            s.flush();
            n += 1;
        }
    }
    n
}

#[no_mangle]
pub extern "C" fn __acrt_iob_func(i: u32) -> *mut File {
    stream(i as usize)
}

#[no_mangle]
pub extern "C" fn __iob_func() -> *mut File {
    stream(0)
}

pub fn stdout() -> *mut File {
    stream(1)
}

pub fn stderr() -> *mut File {
    stream(2)
}

fn stdin() -> *mut File {
    stream(0)
}

/// fopen's mode string as _open flags (with text or binary), or None.
unsafe fn parse_mode<T: Ch>(mode: *const T) -> Option<(i32, i32)> {
    let mut i = 0;
    let at = |i: usize| unsafe { (*mode.add(i)).to_u32() };
    let (mut flags, mut stream_flag) = match at(0) as u8 {
        b'r' => (0, IOREAD),
        b'w' => (O_WRONLY | O_CREAT | O_TRUNC, IOWRT),
        b'a' => (O_WRONLY | O_CREAT | O_APPEND, IOWRT),
        _ => return None,
    };
    i += 1;
    loop {
        match at(i) as u8 {
            0 | b',' => break,
            b'+' => {
                flags = (flags & !3) | O_RDWR;
                stream_flag = IORW;
            }
            b'b' => flags |= O_BINARY,
            b't' => flags |= O_TEXT,
            b'x' => flags |= O_EXCL,
            _ => {}
        }
        i += 1;
    }
    Some((flags, stream_flag))
}

fn free_slot() -> Option<usize> {
    (3..MAX_STREAMS).find(|&i| unsafe { !STATE[i].in_use })
}

/// Make stream `i` read/write descriptor `fd`.
fn attach(i: usize, fd: i32, stream_flag: i32) -> *mut File {
    let text = fd_entry(fd).map_or(true, |f| f.mode != O_BINARY);
    unsafe {
        STATE[i] = State { in_use: true, text, ..FREE };
        _iob[i] = File { file: fd, flag: stream_flag, ..NO_FILE };
    }
    stream(i)
}

unsafe fn open_stream<T: Ch>(path: *const u16, mode: *const T, slot: Option<usize>) -> *mut File {
    let Some((flags, stream_flag)) = (unsafe { parse_mode(mode) }) else {
        invalid_parameter();
        return core::ptr::null_mut();
    };
    let Some(i) = slot.or_else(free_slot) else {
        set_errno(EMFILE);
        return core::ptr::null_mut();
    };
    let fd = unsafe { _wopen(path, flags, 0o666) };
    if fd < 0 {
        return core::ptr::null_mut();
    }
    attach(i, fd, stream_flag)
}

#[no_mangle]
pub unsafe extern "C" fn _wfopen(path: *const u16, mode: *const u16) -> *mut File {
    unsafe { open_stream(path, mode, None) }
}

#[no_mangle]
pub unsafe extern "C" fn fopen(path: *const u8, mode: *const u8) -> *mut File {
    let w = convert::to_wide(unsafe { crate::string::slice(path) });
    let f = unsafe { open_stream(w, mode, None) };
    heap::free(w as *mut u8);
    f
}

#[no_mangle]
pub unsafe extern "C" fn _fsopen(path: *const u8, mode: *const u8, _share: i32) -> *mut File {
    unsafe { fopen(path, mode) }
}

#[no_mangle]
pub unsafe extern "C" fn _wfsopen(path: *const u16, mode: *const u16, _share: i32) -> *mut File {
    unsafe { _wfopen(path, mode) }
}

#[no_mangle]
pub unsafe extern "C" fn fopen_s(out: *mut *mut File, path: *const u8, mode: *const u8) -> i32 {
    let f = unsafe { fopen(path, mode) };
    unsafe { *out = f };
    if f.is_null() { unsafe { *crate::_errno() } } else { 0 }
}

#[no_mangle]
pub unsafe extern "C" fn _wfopen_s(out: *mut *mut File, path: *const u16, mode: *const u16) -> i32 {
    let f = unsafe { _wfopen(path, mode) };
    unsafe { *out = f };
    if f.is_null() { unsafe { *crate::_errno() } } else { 0 }
}

#[no_mangle]
pub unsafe extern "C" fn _wfreopen(path: *const u16, mode: *const u16, f: *mut File) -> *mut File {
    let Some(i) = index(f) else { return core::ptr::null_mut() };
    unsafe { fclose(f) };
    unsafe { open_stream(path, mode, Some(i)) }
}

#[no_mangle]
pub unsafe extern "C" fn freopen(path: *const u8, mode: *const u8, f: *mut File) -> *mut File {
    let Some(i) = index(f) else { return core::ptr::null_mut() };
    unsafe { fclose(f) };
    let w = convert::to_wide(unsafe { crate::string::slice(path) });
    let r = unsafe { open_stream(w, mode, Some(i)) };
    heap::free(w as *mut u8);
    r
}

#[no_mangle]
pub unsafe extern "C" fn freopen_s(out: *mut *mut File, path: *const u8, mode: *const u8, f: *mut File) -> i32 {
    let r = unsafe { freopen(path, mode, f) };
    unsafe { *out = r };
    if r.is_null() { unsafe { *crate::_errno() } } else { 0 }
}

#[no_mangle]
pub unsafe extern "C" fn _fdopen(fd: i32, mode: *const u8) -> *mut File {
    let Some((_, stream_flag)) = (unsafe { parse_mode(mode) }) else {
        invalid_parameter();
        return core::ptr::null_mut();
    };
    if fd_entry(fd).is_none() {
        bad_fd();
        return core::ptr::null_mut();
    }
    let Some(i) = free_slot() else {
        set_errno(EMFILE);
        return core::ptr::null_mut();
    };
    attach(i, fd, stream_flag)
}

#[no_mangle]
pub unsafe extern "C" fn _wfdopen(fd: i32, mode: *const u16) -> *mut File {
    let mut narrow = [0u8; 16];
    for k in 0..15 {
        let c = unsafe { *mode.add(k) };
        narrow[k] = c as u8;
        if c == 0 {
            break;
        }
    }
    unsafe { _fdopen(fd, narrow.as_ptr()) }
}

#[no_mangle]
pub unsafe extern "C" fn fclose(f: *mut File) -> i32 {
    let Some(i) = index(f) else {
        invalid_parameter();
        return EOF;
    };
    let mut s = get(f).unwrap();
    let ok = s.flush();
    let fd = s.f.file;
    if !s.s.buf.is_null() {
        heap::free(s.s.buf);
    }
    unsafe {
        STATE[i] = FREE;
        _iob[i] = NO_FILE;
    }
    let closed = unsafe { _close(fd) } == 0;
    if ok && closed { 0 } else { EOF }
}

#[no_mangle]
pub unsafe extern "C" fn _fcloseall() -> i32 {
    let mut n = 0;
    for i in 3..MAX_STREAMS {
        if unsafe { STATE[i].in_use } {
            unsafe { fclose(stream(i)) };
            n += 1;
        }
    }
    n
}

#[no_mangle]
pub unsafe extern "C" fn fflush(f: *mut File) -> i32 {
    if f.is_null() {
        flush_all();
        return 0;
    }
    match get(f) {
        Some(mut s) => {
            if s.flush() { 0 } else { EOF }
        }
        None => EOF,
    }
}

#[no_mangle]
pub extern "C" fn _flushall() -> i32 {
    flush_all()
}

#[no_mangle]
pub unsafe extern "C" fn setvbuf(f: *mut File, _buf: *mut u8, mode: i32, _size: usize) -> i32 {
    let Some(mut s) = get(f) else { return -1 };
    s.flush();
    s.s.unbuffered = mode == 4; // _IONBF
    0
}

#[no_mangle]
pub unsafe extern "C" fn setbuf(f: *mut File, buf: *mut u8) {
    unsafe { setvbuf(f, buf, if buf.is_null() { 4 } else { 0 }, 512) };
}

#[no_mangle]
pub unsafe extern "C" fn fputc(c: i32, f: *mut File) -> i32 {
    let Some(mut s) = get(f) else { return EOF };
    let ok = s.put(c as u8);
    s.done_writing();
    if ok { c & 0xFF } else { EOF }
}

#[no_mangle]
pub unsafe extern "C" fn putc(c: i32, f: *mut File) -> i32 {
    unsafe { fputc(c, f) }
}

#[no_mangle]
pub unsafe extern "C" fn _fputc_nolock(c: i32, f: *mut File) -> i32 {
    unsafe { fputc(c, f) }
}

#[no_mangle]
pub unsafe extern "C" fn _putc_nolock(c: i32, f: *mut File) -> i32 {
    unsafe { fputc(c, f) }
}

#[no_mangle]
pub unsafe extern "C" fn putchar(c: i32) -> i32 {
    unsafe { fputc(c, stdout()) }
}

#[no_mangle]
pub unsafe extern "C" fn _fputchar(c: i32) -> i32 {
    unsafe { fputc(c, stdout()) }
}

/// msvcrt's putc macro, when `_cnt` runs out (always).
#[no_mangle]
pub unsafe extern "C" fn _flsbuf(c: i32, f: *mut File) -> i32 {
    unsafe { fputc(c, f) }
}

#[no_mangle]
pub unsafe extern "C" fn fgetc(f: *mut File) -> i32 {
    let Some(mut s) = get(f) else { return EOF };
    s.get().map_or(EOF, |b| b as i32)
}

#[no_mangle]
pub unsafe extern "C" fn getc(f: *mut File) -> i32 {
    unsafe { fgetc(f) }
}

#[no_mangle]
pub unsafe extern "C" fn _fgetc_nolock(f: *mut File) -> i32 {
    unsafe { fgetc(f) }
}

#[no_mangle]
pub unsafe extern "C" fn _getc_nolock(f: *mut File) -> i32 {
    unsafe { fgetc(f) }
}

#[no_mangle]
pub unsafe extern "C" fn getchar() -> i32 {
    unsafe { fgetc(stdin()) }
}

#[no_mangle]
pub unsafe extern "C" fn _fgetchar() -> i32 {
    unsafe { fgetc(stdin()) }
}

/// msvcrt's getc macro, when `_cnt` runs out (always).
#[no_mangle]
pub unsafe extern "C" fn _filbuf(f: *mut File) -> i32 {
    unsafe { fgetc(f) }
}

#[no_mangle]
pub unsafe extern "C" fn ungetc(c: i32, f: *mut File) -> i32 {
    let Some(s) = get(f) else { return EOF };
    if c == EOF || s.s.ungot_n == 2 {
        return EOF;
    }
    s.s.ungot[s.s.ungot_n] = c as u8 as u32;
    s.s.ungot_n += 1;
    s.f.flag &= !IOEOF;
    c & 0xFF
}

#[no_mangle]
pub unsafe extern "C" fn _ungetc_nolock(c: i32, f: *mut File) -> i32 {
    unsafe { ungetc(c, f) }
}

#[no_mangle]
pub unsafe extern "C" fn fputwc(c: u16, f: *mut File) -> u16 {
    let Some(mut s) = get(f) else { return 0xFFFF };
    let ok = s.put_wide(c);
    s.done_writing();
    if ok { c } else { 0xFFFF }
}

#[no_mangle]
pub unsafe extern "C" fn putwc(c: u16, f: *mut File) -> u16 {
    unsafe { fputwc(c, f) }
}

#[no_mangle]
pub unsafe extern "C" fn _fputwc_nolock(c: u16, f: *mut File) -> u16 {
    unsafe { fputwc(c, f) }
}

#[no_mangle]
pub unsafe extern "C" fn putwchar(c: u16) -> u16 {
    unsafe { fputwc(c, stdout()) }
}

#[no_mangle]
pub unsafe extern "C" fn _fputwchar(c: u16) -> u16 {
    unsafe { fputwc(c, stdout()) }
}

#[no_mangle]
pub unsafe extern "C" fn fgetwc(f: *mut File) -> u16 {
    let Some(mut s) = get(f) else { return 0xFFFF };
    s.get_wide().unwrap_or(0xFFFF)
}

#[no_mangle]
pub unsafe extern "C" fn getwc(f: *mut File) -> u16 {
    unsafe { fgetwc(f) }
}

#[no_mangle]
pub unsafe extern "C" fn _fgetwc_nolock(f: *mut File) -> u16 {
    unsafe { fgetwc(f) }
}

#[no_mangle]
pub unsafe extern "C" fn getwchar() -> u16 {
    unsafe { fgetwc(stdin()) }
}

#[no_mangle]
pub unsafe extern "C" fn _fgetwchar() -> u16 {
    unsafe { fgetwc(stdin()) }
}

#[no_mangle]
pub unsafe extern "C" fn ungetwc(c: u16, f: *mut File) -> u16 {
    let Some(s) = get(f) else { return 0xFFFF };
    if c == 0xFFFF || s.s.low_surrogate.is_some() {
        return 0xFFFF;
    }
    s.s.low_surrogate = Some(c);
    s.f.flag &= !IOEOF;
    c
}

#[no_mangle]
pub unsafe extern "C" fn fputs(text: *const u8, f: *mut File) -> i32 {
    let Some(mut s) = get(f) else { return EOF };
    let mut ok = true;
    for &b in unsafe { crate::string::slice(text) } {
        ok &= s.put(b);
    }
    s.done_writing();
    if ok { 0 } else { EOF }
}

#[no_mangle]
pub unsafe extern "C" fn fputws(text: *const u16, f: *mut File) -> i32 {
    let Some(mut s) = get(f) else { return EOF };
    let mut ok = true;
    for &c in unsafe { crate::string::slice(text) } {
        ok &= s.put_wide(c);
    }
    s.done_writing();
    if ok { 0 } else { EOF }
}

#[no_mangle]
pub unsafe extern "C" fn puts(text: *const u8) -> i32 {
    let Some(mut s) = get(stdout()) else { return EOF };
    let mut ok = true;
    for &b in unsafe { crate::string::slice(text) } {
        ok &= s.put(b);
    }
    ok &= s.put(b'\n');
    s.done_writing();
    if ok { 0 } else { EOF }
}

#[no_mangle]
pub unsafe extern "C" fn _putws(text: *const u16) -> i32 {
    let Some(mut s) = get(stdout()) else { return EOF };
    let mut ok = true;
    for &c in unsafe { crate::string::slice(text) } {
        ok &= s.put_wide(c);
    }
    ok &= s.put(b'\n');
    s.done_writing();
    if ok { 0 } else { EOF }
}

#[no_mangle]
pub unsafe extern "C" fn fgets(buf: *mut u8, n: i32, f: *mut File) -> *mut u8 {
    let Some(mut s) = get(f) else { return core::ptr::null_mut() };
    if n <= 0 {
        return core::ptr::null_mut();
    }
    let mut k = 0;
    while k + 1 < n as usize {
        let Some(b) = s.get() else { break };
        unsafe { *buf.add(k) = b };
        k += 1;
        if b == b'\n' {
            break;
        }
    }
    if k == 0 {
        return core::ptr::null_mut();
    }
    unsafe { *buf.add(k) = 0 };
    buf
}

#[no_mangle]
pub unsafe extern "C" fn fgetws(buf: *mut u16, n: i32, f: *mut File) -> *mut u16 {
    let Some(mut s) = get(f) else { return core::ptr::null_mut() };
    if n <= 0 {
        return core::ptr::null_mut();
    }
    let mut k = 0;
    while k + 1 < n as usize {
        let Some(c) = s.get_wide() else { break };
        unsafe { *buf.add(k) = c };
        k += 1;
        if c == b'\n' as u16 {
            break;
        }
    }
    if k == 0 {
        return core::ptr::null_mut();
    }
    unsafe { *buf.add(k) = 0 };
    buf
}

/// gets_s: a line from stdin without its newline.
#[no_mangle]
pub unsafe extern "C" fn gets_s(buf: *mut u8, size: usize) -> *mut u8 {
    let Some(mut s) = get(stdin()) else { return core::ptr::null_mut() };
    let mut k = 0;
    let mut any = false;
    while let Some(b) = s.get() {
        any = true;
        if b == b'\n' {
            break;
        }
        if k + 1 < size {
            unsafe { *buf.add(k) = b };
            k += 1;
        }
    }
    if !any {
        return core::ptr::null_mut();
    }
    unsafe { *buf.add(k) = 0 };
    buf
}

#[no_mangle]
pub unsafe extern "C" fn _getws_s(buf: *mut u16, size: usize) -> *mut u16 {
    let Some(mut s) = get(stdin()) else { return core::ptr::null_mut() };
    let mut k = 0;
    let mut any = false;
    while let Some(c) = s.get_wide() {
        any = true;
        if c == b'\n' as u16 {
            break;
        }
        if k + 1 < size {
            unsafe { *buf.add(k) = c };
            k += 1;
        }
    }
    if !any {
        return core::ptr::null_mut();
    }
    unsafe { *buf.add(k) = 0 };
    buf
}

#[no_mangle]
pub unsafe extern "C" fn fread(buf: *mut u8, size: usize, count: usize, f: *mut File) -> usize {
    let Some(mut s) = get(f) else { return 0 };
    let Some(total) = size.checked_mul(count) else { return 0 };
    if total == 0 {
        return 0;
    }
    let mut k = 0;
    if s.s.text {
        while k < total {
            let Some(b) = s.get() else { break };
            unsafe { *buf.add(k) = b };
            k += 1;
        }
    } else {
        while s.s.ungot_n > 0 && k < total {
            s.s.ungot_n -= 1;
            unsafe { *buf.add(k) = s.s.ungot[s.s.ungot_n] as u8 };
            k += 1;
        }
        while k < total {
            if s.s.pending != Pending::Reading || s.s.pos == s.s.end {
                if !s.fill() {
                    break;
                }
            }
            let n = (s.s.end - s.s.pos).min(total - k);
            unsafe { core::ptr::copy_nonoverlapping(s.s.buf.add(s.s.pos), buf.add(k), n) };
            s.s.pos += n;
            k += n;
        }
    }
    k / size
}

#[no_mangle]
pub unsafe extern "C" fn _fread_nolock(buf: *mut u8, size: usize, count: usize, f: *mut File) -> usize {
    unsafe { fread(buf, size, count, f) }
}

#[no_mangle]
pub unsafe extern "C" fn fwrite(buf: *const u8, size: usize, count: usize, f: *mut File) -> usize {
    let Some(mut s) = get(f) else { return 0 };
    let Some(total) = size.checked_mul(count) else { return 0 };
    let mut k = 0;
    while k < total && s.put(unsafe { *buf.add(k) }) {
        k += 1;
    }
    s.done_writing();
    if size == 0 { 0 } else { k / size }
}

#[no_mangle]
pub unsafe extern "C" fn _fwrite_nolock(buf: *const u8, size: usize, count: usize, f: *mut File) -> usize {
    unsafe { fwrite(buf, size, count, f) }
}

#[no_mangle]
pub unsafe extern "C" fn _fseeki64(f: *mut File, offset: i64, whence: i32) -> i32 {
    let Some(mut s) = get(f) else {
        invalid_parameter();
        return -1;
    };
    if !(0..=2).contains(&whence) {
        invalid_parameter();
        return -1;
    }
    let offset = if whence == 1 { offset - s.s.ungot_n as i64 } else { offset };
    s.s.ungot_n = 0;
    s.s.low_surrogate = None;
    s.flush();
    s.f.flag &= !IOEOF;
    if _lseeki64(s.f.file, offset, whence) < 0 { -1 } else { 0 }
}

#[no_mangle]
pub unsafe extern "C" fn fseek(f: *mut File, offset: i32, whence: i32) -> i32 {
    unsafe { _fseeki64(f, offset as i64, whence) }
}

#[no_mangle]
pub unsafe extern "C" fn _ftelli64(f: *mut File) -> i64 {
    let Some(s) = get(f) else { return -1 };
    let here = _telli64(s.f.file);
    if here < 0 {
        return -1;
    }
    let adjust = match s.s.pending {
        Pending::Reading => -((s.s.end - s.s.pos) as i64),
        Pending::Writing => s.s.pos as i64,
        Pending::Nothing => 0,
    };
    here + adjust - s.s.ungot_n as i64
}

#[no_mangle]
pub unsafe extern "C" fn ftell(f: *mut File) -> i32 {
    unsafe { _ftelli64(f) as i32 }
}

#[no_mangle]
pub unsafe extern "C" fn rewind(f: *mut File) {
    unsafe { _fseeki64(f, 0, 0) };
    if let Some(s) = get(f) {
        s.f.flag &= !(IOEOF | IOERR);
    }
}

#[no_mangle]
pub unsafe extern "C" fn fgetpos(f: *mut File, pos: *mut i64) -> i32 {
    let p = unsafe { _ftelli64(f) };
    unsafe { *pos = p };
    if p < 0 { -1 } else { 0 }
}

#[no_mangle]
pub unsafe extern "C" fn fsetpos(f: *mut File, pos: *const i64) -> i32 {
    unsafe { _fseeki64(f, *pos, 0) }
}

#[no_mangle]
pub unsafe extern "C" fn feof(f: *mut File) -> i32 {
    get(f).map_or(0, |s| s.f.flag & IOEOF)
}

#[no_mangle]
pub unsafe extern "C" fn ferror(f: *mut File) -> i32 {
    get(f).map_or(0, |s| s.f.flag & IOERR)
}

#[no_mangle]
pub unsafe extern "C" fn clearerr(f: *mut File) {
    if let Some(s) = get(f) {
        s.f.flag &= !(IOEOF | IOERR);
    }
}

#[no_mangle]
pub unsafe extern "C" fn clearerr_s(f: *mut File) -> i32 {
    unsafe { clearerr(f) };
    0
}

#[no_mangle]
pub unsafe extern "C" fn _fileno(f: *mut File) -> i32 {
    match get(f) {
        Some(s) => s.f.file,
        None => {
            invalid_parameter();
            -1
        }
    }
}

#[no_mangle]
pub extern "C" fn _lock_file(_f: *mut File) {
    crate::threads::lock();
}

#[no_mangle]
pub extern "C" fn _unlock_file(_f: *mut File) {
    crate::threads::unlock();
}

#[no_mangle]
pub extern "C" fn _getmaxstdio() -> i32 {
    MAX_STREAMS as i32
}

#[no_mangle]
pub extern "C" fn _setmaxstdio(n: i32) -> i32 {
    if n as usize <= MAX_STREAMS { n } else { -1 }
}

#[no_mangle]
pub unsafe extern "C" fn _get_stream_buffer_pointers(f: *mut File, base: *mut *mut *mut u8, ptr: *mut *mut *mut u8, cnt: *mut *mut i32) -> i32 {
    unsafe {
        if !base.is_null() {
            *base = &raw mut (*f).base;
        }
        if !ptr.is_null() {
            *ptr = &raw mut (*f).ptr;
        }
        if !cnt.is_null() {
            *cnt = &raw mut (*f).cnt;
        }
    }
    0
}

#[no_mangle]
pub unsafe extern "C" fn perror(text: *const u8) {
    let e = unsafe { *crate::_errno() };
    let err = stderr();
    unsafe {
        if !text.is_null() && *text != 0 {
            fputs(text, err);
            fputs(b": \0".as_ptr(), err);
        }
        fputs(crate::string::strerror(e), err);
        fputc(b'\n' as i32, err);
    }
}

#[no_mangle]
pub unsafe extern "C" fn _wperror(text: *const u16) {
    let err = stderr();
    unsafe {
        if !text.is_null() && *text != 0 {
            fputws(text, err);
            fputs(b": \0".as_ptr(), err);
        }
        fputs(crate::string::strerror(*crate::_errno()), err);
        fputc(b'\n' as i32, err);
    }
}

// -----| files by name |-----

#[no_mangle]
pub unsafe extern "C" fn _wremove(path: *const u16) -> i32 {
    if unsafe { crate::DeleteFileW(path) } == 0 {
        errno_from_win32();
        return -1;
    }
    0
}

#[no_mangle]
pub unsafe extern "C" fn remove(path: *const u8) -> i32 {
    let w = convert::to_wide(unsafe { crate::string::slice(path) });
    let r = unsafe { _wremove(w) };
    heap::free(w as *mut u8);
    r
}

#[no_mangle]
pub unsafe extern "C" fn _unlink(path: *const u8) -> i32 {
    unsafe { remove(path) }
}

#[no_mangle]
pub unsafe extern "C" fn _wunlink(path: *const u16) -> i32 {
    unsafe { _wremove(path) }
}

// -----| the console |-----

/// No raw keyboard yet: the next character of the typed line.
#[no_mangle]
pub unsafe extern "C" fn _getch() -> i32 {
    let mut b = 0u8;
    match fd_entry(0) {
        Some(f) => if raw_read(f, &mut b, 1) == 1 { b as i32 } else { EOF },
        None => EOF,
    }
}

#[no_mangle]
pub unsafe extern "C" fn _getche() -> i32 {
    unsafe { _getch() }
}

#[no_mangle]
pub unsafe extern "C" fn _getwch() -> u16 {
    unsafe { _getch() as u16 }
}

#[no_mangle]
pub extern "C" fn _kbhit() -> i32 {
    0
}

#[no_mangle]
pub unsafe extern "C" fn _putch(c: i32) -> i32 {
    let b = c as u8;
    match fd_entry(1) {
        Some(f) => if raw_write(f, &b, 1) { c } else { EOF },
        None => EOF,
    }
}

#[no_mangle]
pub unsafe extern "C" fn _putwch(c: u16) -> u16 {
    let mut bytes = [0u8; 4];
    let mut n = 0;
    convert::encode_utf8(c as u32, &mut |b| {
        bytes[n] = b;
        n += 1;
    });
    match fd_entry(1) {
        Some(f) => if raw_write(f, bytes.as_ptr(), n) { c } else { 0xFFFF },
        None => 0xFFFF,
    }
}

#[no_mangle]
pub unsafe extern "C" fn _cputs(text: *const u8) -> i32 {
    match fd_entry(1) {
        Some(f) => if raw_write(f, text, unsafe { len(text) }) { 0 } else { -1 },
        None => -1,
    }
}

// -----| printf |-----

const MSVCRT_PRINTF: u64 = format::LEGACY_WIDE_SPECIFIERS | 8 | format::LEGACY_THREE_DIGIT_EXPONENTS;
const STANDARD_SNPRINTF: u64 = 2;
const LEGACY_NULL_TERMINATION: u64 = 1;

struct StreamOut(S, bool);

impl Out<u8> for StreamOut {
    fn put(&mut self, c: u8) {
        self.1 &= self.0.put(c);
    }
}

impl Out<u16> for StreamOut {
    fn put(&mut self, c: u16) {
        self.1 &= self.0.put_wide(c);
    }
}

/// Into a buffer of `cap` units, counting all of them.
struct BufOut<C> {
    p: *mut C,
    cap: usize,
    n: usize,
}

impl<C: Ch> Out<C> for BufOut<C> {
    fn put(&mut self, c: C) {
        if self.n < self.cap {
            unsafe { *self.p.add(self.n) = c };
        }
        self.n += 1;
    }
}

unsafe fn to_stream<C: Ch>(f: *mut File, fmt: *const C, va: VaList, options: u64) -> i32
where
    StreamOut: Out<C>,
{
    let Some(s) = get(f) else {
        invalid_parameter();
        return -1;
    };
    let mut out = StreamOut(s, true);
    let n = unsafe { format::format::<C>(&mut out, fmt, va, options) };
    out.0.done_writing();
    if out.1 { n } else { -1 }
}

/// UCRT's sprintf family: `count` is the buffer's size in units.
unsafe fn to_buffer<C: Ch>(buf: *mut C, count: usize, fmt: *const C, va: VaList, options: u64) -> i32 {
    if fmt.is_null() || (buf.is_null() && count != 0) {
        invalid_parameter();
        return -1;
    }
    let mut out = BufOut { p: buf, cap: count, n: 0 };
    let n = unsafe { format::format(&mut out, fmt, va, options) } as usize;
    if buf.is_null() {
        return n as i32;
    }
    if n < count {
        unsafe { *buf.add(n) = C::default() };
        return n as i32;
    }
    if options & STANDARD_SNPRINTF != 0 {
        if count > 0 {
            unsafe { *buf.add(count - 1) = C::default() };
        }
        return n as i32;
    }
    if n == count && options & LEGACY_NULL_TERMINATION != 0 {
        return n as i32;
    }
    if options & LEGACY_NULL_TERMINATION == 0 && count > 0 {
        unsafe { *buf.add(count - 1) = C::default() };
    }
    -1
}

/// The _s forms: output that doesn't fit is an error (empty result).
unsafe fn to_buffer_s<C: Ch>(buf: *mut C, size: usize, max: usize, fmt: *const C, va: VaList, options: u64) -> i32 {
    if buf.is_null() || size == 0 || fmt.is_null() {
        invalid_parameter();
        return -1;
    }
    let mut out = BufOut { p: buf, cap: size, n: 0 };
    let n = unsafe { format::format(&mut out, fmt, va, options) } as usize;
    let limit = if max == usize::MAX { size - 1 } else { max };
    if n <= limit && n < size {
        unsafe { *buf.add(n) = C::default() };
        return n as i32;
    }
    if max == usize::MAX || limit < size {
        // _TRUNCATE (or a count that fits the buffer): truncate.
        let k = limit.min(size - 1);
        unsafe { *buf.add(k) = C::default() };
        return -1;
    }
    unsafe { *buf = C::default() };
    set_errno(crate::errno::ERANGE);
    invalid_parameter();
    -1
}

// UCRT's entry points (options, destination, format, locale, va_list).

#[no_mangle]
pub unsafe extern "C" fn __stdio_common_vfprintf(options: u64, f: *mut File, fmt: *const u8, _l: usize, va: VaList) -> i32 {
    unsafe { to_stream(f, fmt, va, options) }
}

#[no_mangle]
pub unsafe extern "C" fn __stdio_common_vfprintf_s(options: u64, f: *mut File, fmt: *const u8, _l: usize, va: VaList) -> i32 {
    unsafe { to_stream(f, fmt, va, options) }
}

#[no_mangle]
pub unsafe extern "C" fn __stdio_common_vfprintf_p(options: u64, f: *mut File, fmt: *const u8, _l: usize, va: VaList) -> i32 {
    unsafe { to_stream(f, fmt, va, options) }
}

#[no_mangle]
pub unsafe extern "C" fn __stdio_common_vfwprintf(options: u64, f: *mut File, fmt: *const u16, _l: usize, va: VaList) -> i32 {
    unsafe { to_stream(f, fmt, va, options) }
}

#[no_mangle]
pub unsafe extern "C" fn __stdio_common_vfwprintf_s(options: u64, f: *mut File, fmt: *const u16, _l: usize, va: VaList) -> i32 {
    unsafe { to_stream(f, fmt, va, options) }
}

#[no_mangle]
pub unsafe extern "C" fn __stdio_common_vfwprintf_p(options: u64, f: *mut File, fmt: *const u16, _l: usize, va: VaList) -> i32 {
    unsafe { to_stream(f, fmt, va, options) }
}

#[no_mangle]
pub unsafe extern "C" fn __stdio_common_vsprintf(options: u64, buf: *mut u8, count: usize, fmt: *const u8, _l: usize, va: VaList) -> i32 {
    unsafe { to_buffer(buf, count, fmt, va, options) }
}

#[no_mangle]
pub unsafe extern "C" fn __stdio_common_vsprintf_p(options: u64, buf: *mut u8, count: usize, fmt: *const u8, _l: usize, va: VaList) -> i32 {
    unsafe { to_buffer(buf, count, fmt, va, options) }
}

#[no_mangle]
pub unsafe extern "C" fn __stdio_common_vsprintf_s(options: u64, buf: *mut u8, size: usize, fmt: *const u8, _l: usize, va: VaList) -> i32 {
    unsafe { to_buffer_s(buf, size, size, fmt, va, options) }
}

#[no_mangle]
pub unsafe extern "C" fn __stdio_common_vsnprintf_s(options: u64, buf: *mut u8, size: usize, max: usize, fmt: *const u8, _l: usize, va: VaList) -> i32 {
    unsafe { to_buffer_s(buf, size, max, fmt, va, options) }
}

#[no_mangle]
pub unsafe extern "C" fn __stdio_common_vswprintf(options: u64, buf: *mut u16, count: usize, fmt: *const u16, _l: usize, va: VaList) -> i32 {
    unsafe { to_buffer(buf, count, fmt, va, options) }
}

#[no_mangle]
pub unsafe extern "C" fn __stdio_common_vswprintf_p(options: u64, buf: *mut u16, count: usize, fmt: *const u16, _l: usize, va: VaList) -> i32 {
    unsafe { to_buffer(buf, count, fmt, va, options) }
}

#[no_mangle]
pub unsafe extern "C" fn __stdio_common_vswprintf_s(options: u64, buf: *mut u16, size: usize, fmt: *const u16, _l: usize, va: VaList) -> i32 {
    unsafe { to_buffer_s(buf, size, size, fmt, va, options) }
}

#[no_mangle]
pub unsafe extern "C" fn __stdio_common_vsnwprintf_s(options: u64, buf: *mut u16, size: usize, max: usize, fmt: *const u16, _l: usize, va: VaList) -> i32 {
    unsafe { to_buffer_s(buf, size, max, fmt, va, options) }
}

// msvcrt's (classic) entry points.

#[no_mangle]
pub unsafe extern "C" fn vfprintf(f: *mut File, fmt: *const u8, va: VaList) -> i32 {
    unsafe { to_stream(f, fmt, va, MSVCRT_PRINTF) }
}

unsafe extern "C" fn vprintf_impl(fmt: *const u8, va: VaList) -> i32 {
    unsafe { to_stream(stdout(), fmt, va, MSVCRT_PRINTF) }
}

unsafe extern "C" fn vsprintf_impl(buf: *mut u8, fmt: *const u8, va: VaList) -> i32 {
    unsafe { to_buffer(buf, usize::MAX / 2, fmt, va, MSVCRT_PRINTF) }
}

unsafe extern "C" fn vsnprintf_legacy(buf: *mut u8, n: usize, fmt: *const u8, va: VaList) -> i32 {
    unsafe { to_buffer(buf, n, fmt, va, MSVCRT_PRINTF | LEGACY_NULL_TERMINATION) }
}

unsafe extern "C" fn vsnprintf_impl(buf: *mut u8, n: usize, fmt: *const u8, va: VaList) -> i32 {
    unsafe { to_buffer(buf, n, fmt, va, STANDARD_SNPRINTF | format::LEGACY_WIDE_SPECIFIERS) }
}

unsafe extern "C" fn vscprintf_impl(fmt: *const u8, va: VaList) -> i32 {
    unsafe { to_buffer(core::ptr::null_mut(), 0, fmt, va, MSVCRT_PRINTF) }
}

unsafe extern "C" fn vsprintf_s_impl(buf: *mut u8, size: usize, fmt: *const u8, va: VaList) -> i32 {
    unsafe { to_buffer_s(buf, size, size, fmt, va, MSVCRT_PRINTF) }
}

unsafe extern "C" fn vsnprintf_s_impl(buf: *mut u8, size: usize, max: usize, fmt: *const u8, va: VaList) -> i32 {
    unsafe { to_buffer_s(buf, size, max, fmt, va, MSVCRT_PRINTF) }
}

unsafe extern "C" fn vfwprintf_impl(f: *mut File, fmt: *const u16, va: VaList) -> i32 {
    unsafe { to_stream(f, fmt, va, MSVCRT_PRINTF) }
}

unsafe extern "C" fn vwprintf_impl(fmt: *const u16, va: VaList) -> i32 {
    unsafe { to_stream(stdout(), fmt, va, MSVCRT_PRINTF) }
}

/// msvcrt's swprintf has no count.
unsafe extern "C" fn vswprintf_legacy(buf: *mut u16, fmt: *const u16, va: VaList) -> i32 {
    unsafe { to_buffer(buf, usize::MAX / 4, fmt, va, MSVCRT_PRINTF) }
}

unsafe extern "C" fn vsnwprintf_legacy(buf: *mut u16, n: usize, fmt: *const u16, va: VaList) -> i32 {
    unsafe { to_buffer(buf, n, fmt, va, MSVCRT_PRINTF | LEGACY_NULL_TERMINATION) }
}

unsafe extern "C" fn vscwprintf_impl(fmt: *const u16, va: VaList) -> i32 {
    unsafe { to_buffer(core::ptr::null_mut::<u16>(), 0, fmt, va, MSVCRT_PRINTF) }
}

unsafe extern "C" fn vswprintf_s_impl(buf: *mut u16, size: usize, fmt: *const u16, va: VaList) -> i32 {
    unsafe { to_buffer_s(buf, size, size, fmt, va, MSVCRT_PRINTF) }
}

unsafe extern "C" fn vsnwprintf_s_impl(buf: *mut u16, size: usize, max: usize, fmt: *const u16, va: VaList) -> i32 {
    unsafe { to_buffer_s(buf, size, max, fmt, va, MSVCRT_PRINTF) }
}

unsafe extern "C" fn vcprintf_impl(fmt: *const u8, va: VaList) -> i32 {
    let mut text = alloc::vec::Vec::new();
    struct VecOut<'a>(&'a mut alloc::vec::Vec<u8>);
    impl Out<u8> for VecOut<'_> {
        fn put(&mut self, c: u8) {
            self.0.push(c);
        }
    }
    let n = unsafe { format::format(&mut VecOut(&mut text), fmt, va, MSVCRT_PRINTF) };
    if let Some(f) = fd_entry(1) {
        raw_write(f, text.as_ptr(), text.len());
    }
    n
}

macro_rules! exported_v {
    ($($name:ident = $impl:ident($($a:ident: $t:ty),*);)*) => {$(
        #[no_mangle]
        pub unsafe extern "C" fn $name($($a: $t),*, va: VaList) -> i32 {
            unsafe { $impl($($a),*, va) }
        }
    )*};
}

exported_v! {
    vprintf = vprintf_impl(fmt: *const u8);
    vsprintf = vsprintf_impl(buf: *mut u8, fmt: *const u8);
    _vsnprintf = vsnprintf_legacy(buf: *mut u8, n: usize, fmt: *const u8);
    vsnprintf = vsnprintf_impl(buf: *mut u8, n: usize, fmt: *const u8);
    _vscprintf = vscprintf_impl(fmt: *const u8);
    vsprintf_s = vsprintf_s_impl(buf: *mut u8, size: usize, fmt: *const u8);
    _vsnprintf_s = vsnprintf_s_impl(buf: *mut u8, size: usize, max: usize, fmt: *const u8);
    vfwprintf = vfwprintf_impl(f: *mut File, fmt: *const u16);
    vwprintf = vwprintf_impl(fmt: *const u16);
    vswprintf = vswprintf_legacy(buf: *mut u16, fmt: *const u16);
    _vswprintf = vswprintf_legacy(buf: *mut u16, fmt: *const u16);
    _vsnwprintf = vsnwprintf_legacy(buf: *mut u16, n: usize, fmt: *const u16);
    _vscwprintf = vscwprintf_impl(fmt: *const u16);
    vswprintf_s = vswprintf_s_impl(buf: *mut u16, size: usize, fmt: *const u16);
    _vsnwprintf_s = vsnwprintf_s_impl(buf: *mut u16, size: usize, max: usize, fmt: *const u16);
    _vcprintf = vcprintf_impl(fmt: *const u8);
}

variadic!(printf => vprintf_impl, 1);
variadic!(fprintf => vfprintf, 2);
variadic!(sprintf => vsprintf_impl, 2);
variadic!(_snprintf => vsnprintf_legacy, 3);
variadic!(snprintf => vsnprintf_impl, 3);
variadic!(_scprintf => vscprintf_impl, 1);
variadic!(sprintf_s => vsprintf_s_impl, 3);
variadic!(_snprintf_s => vsnprintf_s_impl, 4);
variadic!(wprintf => vwprintf_impl, 1);
variadic!(fwprintf => vfwprintf_impl, 2);
variadic!(swprintf => vswprintf_legacy, 2);
variadic!(_swprintf => vswprintf_legacy, 2);
variadic!(_snwprintf => vsnwprintf_legacy, 3);
variadic!(_scwprintf => vscwprintf_impl, 1);
variadic!(swprintf_s => vswprintf_s_impl, 3);
variadic!(_snwprintf_s => vsnwprintf_s_impl, 4);
variadic!(_cprintf => vcprintf_impl, 1);
variadic!(printf_s => vprintf_impl, 1);
variadic!(fprintf_s => vfprintf, 2);
variadic!(wprintf_s => vwprintf_impl, 1);
variadic!(fwprintf_s => vfwprintf_impl, 2);

// -----| scanf |-----

const MSVCRT_SCANF: u64 = scan::LEGACY_WIDE_SPECIFIERS | 4;

struct StreamIn(S);

impl In for StreamIn {
    fn get(&mut self) -> Option<u32> {
        self.0.get().map(|b| b as u32)
    }
    fn unget(&mut self, c: u32) {
        let s = &mut self.0.s;
        if s.ungot_n < 2 {
            s.ungot[s.ungot_n] = c;
            s.ungot_n += 1;
        }
    }
}

struct WideStreamIn(S);

impl In for WideStreamIn {
    fn get(&mut self) -> Option<u32> {
        self.0.get_wide().map(|c| c as u32)
    }
    fn unget(&mut self, c: u32) {
        self.0.s.low_surrogate = Some(c as u16);
    }
}

unsafe fn from_string<C: Ch>(text: *const C, count: usize, fmt: *const C, va: VaList, options: u64) -> i32 {
    if text.is_null() || fmt.is_null() {
        invalid_parameter();
        return -1;
    }
    let mut input = StrIn { p: text, pos: 0, limit: count };
    unsafe { scan::scan(&mut input, fmt, va, options) }
}

#[no_mangle]
pub unsafe extern "C" fn __stdio_common_vsscanf(options: u64, text: *const u8, count: usize, fmt: *const u8, _l: usize, va: VaList) -> i32 {
    unsafe { from_string(text, count, fmt, va, options) }
}

#[no_mangle]
pub unsafe extern "C" fn __stdio_common_vswscanf(options: u64, text: *const u16, count: usize, fmt: *const u16, _l: usize, va: VaList) -> i32 {
    unsafe { from_string(text, count, fmt, va, options) }
}

#[no_mangle]
pub unsafe extern "C" fn __stdio_common_vfscanf(options: u64, f: *mut File, fmt: *const u8, _l: usize, va: VaList) -> i32 {
    let Some(s) = get(f) else {
        invalid_parameter();
        return -1;
    };
    unsafe { scan::scan(&mut StreamIn(s), fmt, va, options) }
}

#[no_mangle]
pub unsafe extern "C" fn __stdio_common_vfwscanf(options: u64, f: *mut File, fmt: *const u16, _l: usize, va: VaList) -> i32 {
    let Some(s) = get(f) else {
        invalid_parameter();
        return -1;
    };
    unsafe { scan::scan(&mut WideStreamIn(s), fmt, va, options) }
}

unsafe extern "C" fn vsscanf_impl(text: *const u8, fmt: *const u8, va: VaList) -> i32 {
    unsafe { from_string(text, usize::MAX, fmt, va, MSVCRT_SCANF) }
}

unsafe extern "C" fn vsscanf_s_impl(text: *const u8, fmt: *const u8, va: VaList) -> i32 {
    unsafe { from_string(text, usize::MAX, fmt, va, MSVCRT_SCANF | scan::SECURECRT) }
}

unsafe extern "C" fn vswscanf_impl(text: *const u16, fmt: *const u16, va: VaList) -> i32 {
    unsafe { from_string(text, usize::MAX, fmt, va, MSVCRT_SCANF) }
}

unsafe extern "C" fn vswscanf_s_impl(text: *const u16, fmt: *const u16, va: VaList) -> i32 {
    unsafe { from_string(text, usize::MAX, fmt, va, MSVCRT_SCANF | scan::SECURECRT) }
}

unsafe extern "C" fn vfscanf_impl(f: *mut File, fmt: *const u8, va: VaList) -> i32 {
    unsafe { __stdio_common_vfscanf(MSVCRT_SCANF, f, fmt, 0, va) }
}

unsafe extern "C" fn vfscanf_s_impl(f: *mut File, fmt: *const u8, va: VaList) -> i32 {
    unsafe { __stdio_common_vfscanf(MSVCRT_SCANF | scan::SECURECRT, f, fmt, 0, va) }
}

unsafe extern "C" fn vscanf_impl(fmt: *const u8, va: VaList) -> i32 {
    unsafe { vfscanf_impl(stdin(), fmt, va) }
}

unsafe extern "C" fn vscanf_s_impl(fmt: *const u8, va: VaList) -> i32 {
    unsafe { vfscanf_s_impl(stdin(), fmt, va) }
}

unsafe extern "C" fn vfwscanf_impl(f: *mut File, fmt: *const u16, va: VaList) -> i32 {
    unsafe { __stdio_common_vfwscanf(MSVCRT_SCANF, f, fmt, 0, va) }
}

unsafe extern "C" fn vwscanf_impl(fmt: *const u16, va: VaList) -> i32 {
    unsafe { vfwscanf_impl(stdin(), fmt, va) }
}

exported_v! {
    vsscanf = vsscanf_impl(text: *const u8, fmt: *const u8);
    vswscanf = vswscanf_impl(text: *const u16, fmt: *const u16);
    vfscanf = vfscanf_impl(f: *mut File, fmt: *const u8);
    vscanf = vscanf_impl(fmt: *const u8);
    vfwscanf = vfwscanf_impl(f: *mut File, fmt: *const u16);
    vwscanf = vwscanf_impl(fmt: *const u16);
}

variadic!(sscanf => vsscanf_impl, 2);
variadic!(sscanf_s => vsscanf_s_impl, 2);
variadic!(swscanf => vswscanf_impl, 2);
variadic!(swscanf_s => vswscanf_s_impl, 2);
variadic!(fscanf => vfscanf_impl, 2);
variadic!(fscanf_s => vfscanf_s_impl, 2);
variadic!(scanf => vscanf_impl, 1);
variadic!(scanf_s => vscanf_s_impl, 1);
variadic!(fwscanf => vfwscanf_impl, 2);
variadic!(wscanf => vwscanf_impl, 1);
