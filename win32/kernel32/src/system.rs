//! What cmd.exe and other console programs ask of the system (W7): the
//! console as a screen buffer, the registry, volumes and drives, the
//! Windows version, file copy/move, mutexes, delay-loaded imports, and the
//! security and topology calls VEILOS answers simply.

use nt_abi::{status, NtStatus};

use crate::util::{wstr, WBuf};
use crate::{check, fail, Bool, Handle};

const ERROR_FILE_NOT_FOUND: u32 = 2;
const ERROR_ACCESS_DENIED: u32 = 5;
const ERROR_INVALID_HANDLE: u32 = 6;
const ERROR_NOT_SUPPORTED: u32 = 50;

// -----| the console as a screen buffer |-----
//
// The console is a scrolling terminal: there are no cells to read back.
// It reports a 120 x 30 window; cursor and colour changes are accepted
// and ignored, and filling the whole screen with spaces from the top left
// (what `cls` does) clears the terminal.

const CONSOLE_COLUMNS: i16 = 120;
const CONSOLE_ROWS: i16 = 30;

fn is_console(h: Handle) -> bool {
    let mut mode = 0u32;
    unsafe { crate::GetConsoleMode(h, &mut mode) != 0 }
}

/// CONSOLE_SCREEN_BUFFER_INFO.
#[no_mangle]
pub unsafe extern "system" fn GetConsoleScreenBufferInfo(h: Handle, info: *mut i16) -> Bool {
    if !is_console(h) {
        crate::SetLastError(ERROR_INVALID_HANDLE);
        return 0;
    }
    let values: [i16; 11] = [
        CONSOLE_COLUMNS, 9001,      // dwSize
        0, 0,                       // dwCursorPosition
        0x07,                       // wAttributes (light grey on black)
        0, 0, CONSOLE_COLUMNS - 1, CONSOLE_ROWS - 1, // srWindow
        CONSOLE_COLUMNS, CONSOLE_ROWS, // dwMaximumWindowSize
    ];
    unsafe { core::ptr::copy_nonoverlapping(values.as_ptr(), info, values.len()) };
    1
}

/// CONSOLE_SCREEN_BUFFER_INFOEX: cbSize, then the fields above, popup
/// attributes, full-screen flag and a colour table.
#[no_mangle]
pub unsafe extern "system" fn GetConsoleScreenBufferInfoEx(h: Handle, info: *mut u8) -> Bool {
    unsafe {
        let size = *(info as *const u32) as usize;
        core::ptr::write_bytes(info.add(4), 0, size.saturating_sub(4));
        GetConsoleScreenBufferInfo(h, info.add(4) as *mut i16)
    }
}

#[no_mangle]
pub extern "system" fn SetConsoleCursorPosition(h: Handle, _pos: u32) -> Bool {
    is_console(h) as Bool
}

#[no_mangle]
pub extern "system" fn SetConsoleTextAttribute(h: Handle, _attr: u16) -> Bool {
    is_console(h) as Bool
}

#[no_mangle]
pub unsafe extern "system" fn GetConsoleCursorInfo(_h: Handle, info: *mut u32) -> Bool {
    unsafe {
        *info = 25; // dwSize
        *info.add(1) = 1; // bVisible
    }
    1
}

#[no_mangle]
pub extern "system" fn SetConsoleCursorInfo(_h: Handle, _info: *const u32) -> Bool {
    1
}

/// `cls`: a fill of the whole screen from (0, 0) clears the terminal (a
/// form feed tells the console to).
#[no_mangle]
pub unsafe extern "system" fn FillConsoleOutputCharacterW(h: Handle, ch: u16, len: u32, pos: u32, written: *mut u32) -> Bool {
    if !is_console(h) {
        crate::SetLastError(ERROR_INVALID_HANDLE);
        return 0;
    }
    if ch == b' ' as u16 && pos == 0 && len >= (CONSOLE_COLUMNS as u32 * CONSOLE_ROWS as u32) {
        let mut n = 0u32;
        unsafe { crate::WriteFile(h, b"\x0c".as_ptr(), 1, &mut n, 0) };
    }
    if !written.is_null() {
        unsafe { *written = len };
    }
    1
}

#[no_mangle]
pub unsafe extern "system" fn FillConsoleOutputCharacterA(h: Handle, ch: u8, len: u32, pos: u32, written: *mut u32) -> Bool {
    unsafe { FillConsoleOutputCharacterW(h, ch as u16, len, pos, written) }
}

#[no_mangle]
pub unsafe extern "system" fn FillConsoleOutputAttribute(_h: Handle, _attr: u16, len: u32, _pos: u32, written: *mut u32) -> Bool {
    if !written.is_null() {
        unsafe { *written = len };
    }
    1
}

#[no_mangle]
pub extern "system" fn ScrollConsoleScreenBufferW(_h: Handle, _rect: usize, _clip: usize, _dest: u32, _fill: usize) -> Bool {
    1
}

#[no_mangle]
pub extern "system" fn FlushConsoleInputBuffer(_h: Handle) -> Bool {
    1
}

#[no_mangle]
pub unsafe extern "system" fn GetNumberOfConsoleInputEvents(_h: Handle, n: *mut u32) -> Bool {
    unsafe { *n = 0 };
    1
}

#[no_mangle]
pub extern "system" fn GetLargestConsoleWindowSize(_h: Handle) -> u32 {
    (CONSOLE_COLUMNS as u32) | ((CONSOLE_ROWS as u32) << 16)
}

#[no_mangle]
pub extern "system" fn SetConsoleCtrlHandler(_handler: usize, _add: Bool) -> Bool {
    1
}

/// No console has a window of its own (it's shown in a terminal).
#[no_mangle]
pub extern "system" fn GetConsoleWindow() -> Handle {
    0
}

static mut TITLE: [u16; 256] = [0; 256];

#[no_mangle]
pub unsafe extern "system" fn SetConsoleTitleW(title: *const u16) -> Bool {
    let t = unsafe { wstr(title) };
    let n = t.len().min(255);
    unsafe {
        let buf = &mut *(&raw mut TITLE);
        buf[..n].copy_from_slice(&t[..n]);
        buf[n] = 0;
    }
    1
}

#[no_mangle]
pub unsafe extern "system" fn SetConsoleTitleA(title: *const u8) -> Bool {
    let w = unsafe { crate::util::widen(title) };
    unsafe { SetConsoleTitleW(w.as_ptr()) }
}

#[no_mangle]
pub unsafe extern "system" fn GetConsoleTitleW(out: *mut u16, cap: u32) -> u32 {
    let t = unsafe { wstr((&raw const TITLE) as *const u16) };
    if cap == 0 {
        return t.len() as u32;
    }
    let n = t.len().min(cap as usize - 1);
    unsafe {
        core::ptr::copy_nonoverlapping(t.as_ptr(), out, n);
        *out.add(n) = 0;
    }
    n as u32
}

#[no_mangle]
pub unsafe extern "system" fn GetConsoleTitleA(out: *mut u8, cap: u32) -> u32 {
    let t = unsafe { wstr((&raw const TITLE) as *const u16) };
    unsafe { crate::util::narrow_out(t, out, cap) }
}

#[no_mangle]
pub unsafe extern "system" fn GetConsoleOriginalTitleW(out: *mut u16, cap: u32) -> u32 {
    unsafe { GetConsoleTitleW(out, cap) }
}

// -----| volumes and drives |-----

/// Is `root` drive C (or none, meaning the current one)?
unsafe fn is_drive_c(root: *const u16) -> bool {
    root.is_null() || {
        let r = unsafe { wstr(root) };
        r.is_empty() || (r[0] | 0x20) == b'c' as u16 || r[0] == b'\\' as u16
    }
}

#[no_mangle]
pub unsafe extern "system" fn GetDriveTypeW(root: *const u16) -> u32 {
    if unsafe { is_drive_c(root) } { 3 } else { 1 } // DRIVE_FIXED, DRIVE_NO_ROOT_DIR
}

#[no_mangle]
pub unsafe extern "system" fn GetDriveTypeA(root: *const u8) -> u32 {
    let w = unsafe { crate::util::widen(root) };
    unsafe { GetDriveTypeW(if root.is_null() { core::ptr::null() } else { w.as_ptr() }) }
}

#[no_mangle]
pub extern "system" fn GetLogicalDrives() -> u32 {
    1 << 2 // C:
}

const VOLUME_SERIAL: u32 = 0x5645_494C; // "VEIL"

unsafe fn put_wide(s: &str, out: *mut u16, cap: u32) {
    if out.is_null() || cap == 0 {
        return;
    }
    let mut n = 0;
    for u in s.encode_utf16() {
        if n + 1 >= cap as usize {
            break;
        }
        unsafe { *out.add(n) = u };
        n += 1;
    }
    unsafe { *out.add(n) = 0 };
}

#[no_mangle]
pub unsafe extern "system" fn GetVolumeInformationW(
    root: *const u16,
    name: *mut u16,
    name_len: u32,
    serial: *mut u32,
    max_component: *mut u32,
    flags: *mut u32,
    fs_name: *mut u16,
    fs_len: u32,
) -> Bool {
    if !unsafe { is_drive_c(root) } {
        crate::SetLastError(3);
        return 0;
    }
    unsafe {
        put_wide("VEILOS", name, name_len);
        put_wide("FAT32", fs_name, fs_len);
        if !serial.is_null() {
            *serial = VOLUME_SERIAL;
        }
        if !max_component.is_null() {
            *max_component = 255;
        }
        if !flags.is_null() {
            *flags = 0x2 | 0x4; // case-preserved names, Unicode on disk
        }
    }
    1
}

#[no_mangle]
pub unsafe extern "system" fn GetVolumeInformationByHandleW(_h: Handle, name: *mut u16, name_len: u32, serial: *mut u32, max_component: *mut u32, flags: *mut u32, fs_name: *mut u16, fs_len: u32) -> Bool {
    unsafe { GetVolumeInformationW(core::ptr::null(), name, name_len, serial, max_component, flags, fs_name, fs_len) }
}

/// Free space: VEILOS doesn't track it per volume yet, so a fixed answer
/// (1 GiB free of 4 GiB).
#[no_mangle]
pub unsafe extern "system" fn GetDiskFreeSpaceExW(_root: *const u16, avail: *mut u64, total: *mut u64, free: *mut u64) -> Bool {
    unsafe {
        if !avail.is_null() {
            *avail = 1 << 30;
        }
        if !total.is_null() {
            *total = 4 << 30;
        }
        if !free.is_null() {
            *free = 1 << 30;
        }
    }
    1
}

#[no_mangle]
pub unsafe extern "system" fn GetDiskFreeSpaceW(_root: *const u16, spc: *mut u32, bps: *mut u32, free: *mut u32, total: *mut u32) -> Bool {
    unsafe {
        if !spc.is_null() {
            *spc = 8;
        }
        if !bps.is_null() {
            *bps = 512;
        }
        if !free.is_null() {
            *free = (1 << 30) / 4096;
        }
        if !total.is_null() {
            *total = (4u32 << 30) / 4096;
        }
    }
    1
}

#[no_mangle]
pub unsafe extern "system" fn GetVolumePathNameW(_path: *const u16, out: *mut u16, cap: u32) -> Bool {
    unsafe { put_wide("C:\\", out, cap) };
    1
}

#[no_mangle]
pub unsafe extern "system" fn GetWindowsDirectoryW(out: *mut u16, cap: u32) -> u32 {
    if cap < 4 {
        return 4;
    }
    unsafe { put_wide("C:\\", out, cap) };
    3
}

#[no_mangle]
pub unsafe extern "system" fn GetSystemWindowsDirectoryW(out: *mut u16, cap: u32) -> u32 {
    unsafe { GetWindowsDirectoryW(out, cap) }
}

#[no_mangle]
pub unsafe extern "system" fn GetSystemDirectoryW(out: *mut u16, cap: u32) -> u32 {
    if cap < 10 {
        return 10;
    }
    unsafe { put_wide("C:\\SYSTEM", out, cap) };
    9
}

#[no_mangle]
pub unsafe extern "system" fn GetWindowsDirectoryA(out: *mut u8, cap: u32) -> u32 {
    let w: [u16; 3] = [b'C' as u16, b':' as u16, b'\\' as u16];
    unsafe { crate::util::narrow_out(&w, out, cap) }
}

#[no_mangle]
pub unsafe extern "system" fn GetSystemDirectoryA(out: *mut u8, cap: u32) -> u32 {
    let w: [u16; 9] = [b'C' as u16, b':' as u16, b'\\' as u16, b'S' as u16, b'Y' as u16, b'S' as u16, b'T' as u16, b'E' as u16, b'M' as u16];
    unsafe { crate::util::narrow_out(&w, out, cap) }
}

// -----| the version |-----

/// Windows 10, build 19041.
#[no_mangle]
pub extern "system" fn GetVersion() -> u32 {
    (19041 << 16) | 10
}

/// OSVERSIONINFO(EX)W: size, major, minor, build, platform, CSD version, ...
#[no_mangle]
pub unsafe extern "system" fn GetVersionExW(info: *mut u32) -> Bool {
    unsafe {
        let size = *info as usize;
        core::ptr::write_bytes((info as *mut u8).add(4), 0, size.saturating_sub(4));
        *info.add(1) = 10;
        *info.add(2) = 0;
        *info.add(3) = 19041;
        *info.add(4) = 2; // VER_PLATFORM_WIN32_NT
    }
    1
}

#[no_mangle]
pub unsafe extern "system" fn GetVersionExA(info: *mut u32) -> Bool {
    unsafe { GetVersionExW(info) }
}

// -----| files: copy, move, links |-----

const COPY_CHUNK: usize = 64 * 1024;

#[no_mangle]
pub unsafe extern "system" fn CopyFileW(from: *const u16, to: *const u16, fail_if_exists: Bool) -> Bool {
    let src = unsafe { crate::file::CreateFileW(from, 0x8000_0000, 1, 0, 3, 0, 0) };
    if src == crate::INVALID_HANDLE_VALUE {
        return 0;
    }
    let attrs = unsafe { crate::file::GetFileAttributesW(from) };
    let disposition = if fail_if_exists != 0 { 1 } else { 2 }; // CREATE_NEW, CREATE_ALWAYS
    let dst = unsafe { crate::file::CreateFileW(to, 0x4000_0000, 0, 0, disposition, attrs & !0x10, 0) };
    if dst == crate::INVALID_HANDLE_VALUE {
        crate::CloseHandle(src);
        return 0;
    }
    let buf = crate::util::alloc(COPY_CHUNK);
    let mut ok = true;
    loop {
        let mut n = 0u32;
        if unsafe { crate::ReadFile(src, buf, COPY_CHUNK as u32, &mut n, 0) } == 0 {
            ok = false;
            break;
        }
        if n == 0 {
            break;
        }
        let mut w = 0u32;
        if unsafe { crate::WriteFile(dst, buf, n, &mut w, 0) } == 0 || w != n {
            ok = false;
            break;
        }
    }
    crate::util::free(buf);
    crate::CloseHandle(src);
    crate::CloseHandle(dst);
    ok as Bool
}

#[no_mangle]
pub unsafe extern "system" fn CopyFileA(from: *const u8, to: *const u8, fail_if_exists: Bool) -> Bool {
    let (f, t) = unsafe { (crate::util::widen(from), crate::util::widen(to)) };
    unsafe { CopyFileW(f.as_ptr(), t.as_ptr(), fail_if_exists) }
}

#[no_mangle]
pub unsafe extern "system" fn CopyFileExW(from: *const u16, to: *const u16, _progress: usize, _data: usize, _cancel: *mut Bool, flags: u32) -> Bool {
    unsafe { CopyFileW(from, to, (flags & 1) as Bool) } // COPY_FILE_FAIL_IF_EXISTS
}

const MOVEFILE_REPLACE_EXISTING: u32 = 1;

/// No rename in the file system yet: a file moves by copy and delete.
#[no_mangle]
pub unsafe extern "system" fn MoveFileExW(from: *const u16, to: *const u16, flags: u32) -> Bool {
    let attrs = unsafe { crate::file::GetFileAttributesW(from) };
    if attrs == u32::MAX {
        return 0;
    }
    if attrs & 0x10 != 0 {
        crate::SetLastError(ERROR_ACCESS_DENIED); // directories can't move yet
        return 0;
    }
    if to.is_null() {
        return unsafe { crate::file::DeleteFileW(from) };
    }
    if unsafe { CopyFileW(from, to, (flags & MOVEFILE_REPLACE_EXISTING == 0) as Bool) } == 0 {
        return 0;
    }
    unsafe { crate::file::DeleteFileW(from) }
}

#[no_mangle]
pub unsafe extern "system" fn MoveFileW(from: *const u16, to: *const u16) -> Bool {
    unsafe { MoveFileExW(from, to, 0) }
}

#[no_mangle]
pub unsafe extern "system" fn MoveFileA(from: *const u8, to: *const u8) -> Bool {
    let (f, t) = unsafe { (crate::util::widen(from), crate::util::widen(to)) };
    unsafe { MoveFileExW(f.as_ptr(), t.as_ptr(), 0) }
}

#[no_mangle]
pub unsafe extern "system" fn MoveFileExA(from: *const u8, to: *const u8, flags: u32) -> Bool {
    let (f, t) = unsafe { (crate::util::widen(from), crate::util::widen(to)) };
    unsafe { MoveFileExW(f.as_ptr(), if to.is_null() { core::ptr::null() } else { t.as_ptr() }, flags) }
}

#[no_mangle]
pub unsafe extern "system" fn MoveFileWithProgressW(from: *const u16, to: *const u16, _p: usize, _d: usize, flags: u32) -> Bool {
    unsafe { MoveFileExW(from, to, flags) }
}

#[no_mangle]
pub extern "system" fn CreateHardLinkW(_new: *const u16, _existing: *const u16, _sa: usize) -> Bool {
    crate::SetLastError(ERROR_NOT_SUPPORTED);
    0
}

#[no_mangle]
pub extern "system" fn CreateSymbolicLinkW(_link: *const u16, _target: *const u16, _flags: u32) -> u8 {
    crate::SetLastError(ERROR_NOT_SUPPORTED);
    0
}

/// File times aren't kept yet: accepted.
#[no_mangle]
pub extern "system" fn SetFileTime(_h: Handle, _c: *const u64, _a: *const u64, _w: *const u64) -> Bool {
    1
}

#[no_mangle]
pub unsafe extern "system" fn GetFileTime(_h: Handle, c: *mut u64, a: *mut u64, w: *mut u64) -> Bool {
    for p in [c, a, w] {
        if !p.is_null() {
            unsafe { *p = 0 };
        }
    }
    1
}

/// FILE_INFO_BY_HANDLE_CLASS: FileBasicInfo (0) and FileStandardInfo (1),
/// from the NT information classes they mirror.
#[no_mangle]
pub unsafe extern "system" fn GetFileInformationByHandleEx(h: Handle, class: u32, out: *mut u8, size: u32) -> Bool {
    let nt_class = match class {
        0 => 4, // FileBasicInformation
        1 => 5, // FileStandardInformation
        _ => {
            crate::SetLastError(ERROR_NOT_SUPPORTED);
            return 0;
        }
    };
    let mut iosb = nt_abi::IoStatusBlock::default();
    check(unsafe { crate::NtQueryInformationFile(h, &mut iosb, out, size, nt_class) })
}

#[no_mangle]
pub extern "system" fn DeviceIoControl(_h: Handle, _code: u32, _in: usize, _in_len: u32, _out: usize, _out_len: u32, _ret: *mut u32, _ov: usize) -> Bool {
    crate::SetLastError(1); // ERROR_INVALID_FUNCTION
    0
}

/// Look for `file` (with `ext` if it has none) in `path`, else the
/// application's folder, the current directory, C:\SYSTEM and PATH.
#[no_mangle]
pub unsafe extern "system" fn SearchPathW(path: *const u16, file: *const u16, ext: *const u16, cap: u32, out: *mut u16, part: *mut *mut u16) -> u32 {
    let name = unsafe { wstr(file) };
    let mut with_ext = WBuf::new();
    with_ext.extend(name);
    let has_ext = name.iter().rposition(|&c| c == b'.' as u16).is_some_and(|d| !name[d..].contains(&(b'\\' as u16)));
    if !has_ext && !ext.is_null() {
        with_ext.extend(unsafe { wstr(ext) });
    }
    let mut dirs: [WBuf; 4] = [WBuf::new(), WBuf::new(), WBuf::new(), WBuf::new()];
    if !path.is_null() {
        dirs[0].extend(unsafe { wstr(path) });
    } else {
        let mut exe = [0u16; 520];
        let n = unsafe { crate::module::GetModuleFileNameW(0, exe.as_mut_ptr(), 500) } as usize;
        if let Some(s) = exe[..n].iter().rposition(|&c| c == b'\\' as u16) {
            dirs[0].extend(&exe[..s]);
        }
        let n = unsafe { crate::file::GetCurrentDirectoryW(dirs[1].buf.len() as u32, dirs[1].buf.as_mut_ptr()) };
        dirs[1].len = n as usize;
        dirs[2].push_str("C:\\SYSTEM");
        let key: [u16; 5] = [b'P' as u16, b'A' as u16, b'T' as u16, b'H' as u16, 0];
        let n = unsafe { crate::env::GetEnvironmentVariableW(key.as_ptr(), dirs[3].buf.as_mut_ptr(), dirs[3].buf.len() as u32) };
        dirs[3].len = (n as usize).min(dirs[3].buf.len() - 1);
    }
    for d in dirs.iter() {
        for dir in d.as_slice().split(|&c| c == b';' as u16).filter(|x| !x.is_empty()) {
            let mut candidate = WBuf::new();
            candidate.extend(dir);
            if dir[dir.len() - 1] != b'\\' as u16 {
                candidate.push(b'\\' as u16);
            }
            candidate.extend(with_ext.as_slice());
            let attrs = unsafe { crate::file::GetFileAttributesW(candidate.as_ptr()) };
            if attrs != u32::MAX && attrs & 0x10 == 0 {
                let n = unsafe { crate::file::GetFullPathNameW(candidate.as_ptr(), cap, out, part) };
                return n;
            }
        }
    }
    crate::SetLastError(ERROR_FILE_NOT_FOUND);
    0
}

/// Replace the whole environment with `block` (cmd's endlocal).
#[no_mangle]
pub unsafe extern "system" fn SetEnvironmentStringsW(block: *const u16) -> Bool {
    // Remove what's there now, then set each "NAME=value".
    let current = crate::env::GetEnvironmentStringsW();
    let mut names: alloc_free::Names = alloc_free::Names::new();
    let mut p = current as *const u16;
    unsafe {
        while *p != 0 {
            let s = wstr(p);
            if let Some(eq) = s.iter().skip(1).position(|&c| c == b'=' as u16) {
                names.push(&s[..eq + 1]);
            }
            p = p.add(s.len() + 1);
        }
    }
    crate::env::FreeEnvironmentStringsW(current);
    for name in names.iter() {
        unsafe { crate::env::SetEnvironmentVariableW(name.as_ptr(), core::ptr::null()) };
    }
    let mut p = block;
    unsafe {
        while *p != 0 {
            let s = wstr(p);
            if let Some(eq) = s.iter().skip(1).position(|&c| c == b'=' as u16) {
                let mut name = WBuf::new();
                name.extend(&s[..eq + 1]);
                let mut value = WBuf::new();
                value.extend(&s[eq + 2..]);
                crate::env::SetEnvironmentVariableW(name.as_ptr(), value.as_ptr());
            }
            p = p.add(s.len() + 1);
        }
    }
    1
}

mod alloc_free {
    use crate::util::WBuf;
    /// Up to 256 variable names.
    pub struct Names {
        list: [*mut WBuf; 256],
        n: usize,
    }
    impl Names {
        pub fn new() -> Self {
            Names { list: [core::ptr::null_mut(); 256], n: 0 }
        }
        pub fn push(&mut self, name: &[u16]) {
            if self.n < self.list.len() {
                let b = crate::util::alloc(core::mem::size_of::<WBuf>()) as *mut WBuf;
                unsafe {
                    b.write(WBuf::new());
                    (*b).extend(name);
                }
                self.list[self.n] = b;
                self.n += 1;
            }
        }
        pub fn iter(&self) -> impl Iterator<Item = &WBuf> {
            self.list[..self.n].iter().map(|&b| unsafe { &*b })
        }
    }
    impl Drop for Names {
        fn drop(&mut self) {
            for &b in &self.list[..self.n] {
                crate::util::free(b as *mut u8);
            }
        }
    }
}

// -----| mutexes (on auto-reset events) and semaphores |-----
//
// A mutex is an auto-reset event: signalled = free. Waiting takes it,
// ReleaseMutex frees it. (No owner tracking or recursion yet.)

#[no_mangle]
pub unsafe extern "system" fn CreateMutexExW(_sa: usize, _name: *const u16, flags: u32, access: u32) -> Handle {
    let mut h: Handle = 0;
    let owned = flags & 1 != 0; // CREATE_MUTEX_INITIAL_OWNER
    let s = unsafe { crate::NtCreateEvent(&mut h, access | 0x1F0003, core::ptr::null(), 1, (!owned) as u8) };
    if s != status::SUCCESS {
        fail(s);
        return 0;
    }
    h
}

#[no_mangle]
pub unsafe extern "system" fn CreateMutexW(sa: usize, owned: Bool, name: *const u16) -> Handle {
    unsafe { CreateMutexExW(sa, name, (owned != 0) as u32, 0) }
}

#[no_mangle]
pub unsafe extern "system" fn CreateMutexA(sa: usize, owned: Bool, _name: *const u8) -> Handle {
    unsafe { CreateMutexExW(sa, core::ptr::null(), (owned != 0) as u32, 0) }
}

#[no_mangle]
pub extern "system" fn ReleaseMutex(h: Handle) -> Bool {
    check(unsafe { crate::NtSetEvent(h, core::ptr::null_mut()) })
}

/// Named objects aren't kept: opening one finds nothing.
#[no_mangle]
pub extern "system" fn OpenMutexW(_access: u32, _inherit: Bool, _name: *const u16) -> Handle {
    crate::SetLastError(ERROR_FILE_NOT_FOUND);
    0
}

#[no_mangle]
pub extern "system" fn OpenSemaphoreW(_access: u32, _inherit: Bool, _name: *const u16) -> Handle {
    crate::SetLastError(ERROR_FILE_NOT_FOUND);
    0
}

// -----| delay-loaded imports |-----

/// IMAGE_DELAYLOAD_DESCRIPTOR: Attributes, DllNameRVA, ModuleHandleRVA,
/// ImportAddressTableRVA, ImportNameTableRVA, ...
#[repr(C)]
pub struct DelayDescriptor {
    attributes: u32,
    dll_name: u32,
    module_handle: u32,
    iat: u32,
    int: u32,
    bound_iat: u32,
    unload_iat: u32,
    timestamp: u32,
}

type FailureHook = Option<unsafe extern "system" fn(u32, *mut u8) -> u64>;

/// Load a delay-loaded import on first call: find the DLL (loading it if
/// need be), look up the name, patch the IAT slot and return the address.
#[no_mangle]
pub unsafe extern "system" fn ResolveDelayLoadedAPI(
    parent: u64,
    descriptor: *const DelayDescriptor,
    _failure_dll_hook: FailureHook,
    _failure_system_hook: FailureHook,
    thunk: *mut u64,
    _flags: u32,
) -> u64 {
    let d = unsafe { &*descriptor };
    let dll = (parent + d.dll_name as u64) as *const u8;
    let handle_slot = (parent + d.module_handle as u64) as *mut u64;
    let mut module = unsafe { *handle_slot };
    if module == 0 {
        module = unsafe { crate::module::LoadLibraryA(dll) };
        unsafe { *handle_slot = module };
    }
    let index = (thunk as u64 - (parent + d.iat as u64)) / 8;
    let name_entry = unsafe { *((parent + d.int as u64 + index * 8) as *const u64) };
    let address = if module == 0 {
        0
    } else if name_entry & (1 << 63) != 0 {
        unsafe { crate::module::GetProcAddress(module, (name_entry & 0xFFFF) as *const u8) }
    } else {
        unsafe { crate::module::GetProcAddress(module, (parent + (name_entry & 0x7FFF_FFFF) + 2) as *const u8) }
    };
    // A function we can't find returns 0 (as a stub would).
    let address = if address == 0 { delay_load_missing as *const () as u64 } else { address as u64 };
    unsafe { *thunk = address };
    address
}

extern "system" fn delay_load_missing() -> u64 {
    crate::SetLastError(127); // ERROR_PROC_NOT_FOUND
    0
}

#[no_mangle]
pub extern "system" fn DelayLoadFailureHook(_dll: *const u8, _proc: *const u8) -> u64 {
    delay_load_missing as *const () as u64
}

/// API sets that are present: only the ones this system provides, which
/// isn't known here, so "no" (programs then skip optional features).
#[no_mangle]
pub unsafe extern "system" fn ApiSetQueryApiSetPresence(_name: *const u8, present: *mut u8) -> NtStatus {
    unsafe { *present = 0 };
    status::SUCCESS
}

// -----| security, debugging, topology: simple answers |-----

#[no_mangle]
pub extern "system" fn GetFileSecurityW(_name: *const u16, _info: u32, _sd: *mut u8, _len: u32, _needed: *mut u32) -> Bool {
    crate::SetLastError(ERROR_NOT_SUPPORTED);
    0
}

#[no_mangle]
pub extern "system" fn GetSecurityDescriptorOwner(_sd: *const u8, _owner: *mut usize, _defaulted: *mut Bool) -> Bool {
    crate::SetLastError(ERROR_NOT_SUPPORTED);
    0
}

#[no_mangle]
pub extern "system" fn RevertToSelf() -> Bool {
    1
}

#[no_mangle]
pub extern "system" fn OpenThread(_access: u32, _inherit: Bool, _tid: u32) -> Handle {
    crate::SetLastError(ERROR_ACCESS_DENIED);
    0
}

#[no_mangle]
pub extern "system" fn OpenProcess(_access: u32, _inherit: Bool, _pid: u32) -> Handle {
    crate::SetLastError(ERROR_ACCESS_DENIED);
    0
}

#[no_mangle]
pub extern "system" fn ReadProcessMemory(_p: Handle, _addr: usize, _buf: *mut u8, _n: usize, _read: *mut usize) -> Bool {
    crate::SetLastError(ERROR_ACCESS_DENIED);
    0
}

#[no_mangle]
pub unsafe extern "system" fn CreateProcessAsUserW(
    _token: Handle,
    application: *const u16,
    command_line: *mut u16,
    psa: usize,
    tsa: usize,
    inherit: Bool,
    flags: u32,
    environment: *const u8,
    current_dir: *const u16,
    startup: *const u8,
    info: *mut u8,
) -> Bool {
    unsafe { crate::process::CreateProcessW(application, command_line, psa, tsa, inherit, flags, environment, current_dir, startup, info) }
}

#[no_mangle]
pub extern "system" fn SetLocalTime(_st: *const u16) -> Bool {
    crate::SetLastError(1314); // ERROR_PRIVILEGE_NOT_HELD
    0
}

#[no_mangle]
pub extern "system" fn DebugBreak() {
    unsafe { core::arch::asm!("int3") };
}

#[no_mangle]
pub unsafe extern "system" fn GetNumaHighestNodeNumber(node: *mut u32) -> Bool {
    unsafe { *node = 0 };
    1
}

/// GROUP_AFFINITY: Mask, Group, Reserved[3].
#[no_mangle]
pub unsafe extern "system" fn GetNumaNodeProcessorMaskEx(_node: u16, affinity: *mut u64) -> Bool {
    unsafe {
        *affinity = all_processors();
        *affinity.add(1) = 0;
    }
    1
}

#[no_mangle]
pub unsafe extern "system" fn GetThreadGroupAffinity(_thread: Handle, affinity: *mut u64) -> Bool {
    unsafe { GetNumaNodeProcessorMaskEx(0, affinity) }
}

fn all_processors() -> u64 {
    let n = unsafe { *(crate::peb().add(0xB8) as *const u32) }.clamp(1, 64);
    if n == 64 { u64::MAX } else { (1u64 << n) - 1 }
}

#[no_mangle]
pub unsafe extern "system" fn GetProcessAffinityMask(_p: Handle, process: *mut u64, system: *mut u64) -> Bool {
    unsafe {
        *process = all_processors();
        *system = all_processors();
    }
    1
}

#[no_mangle]
pub extern "system" fn SetProcessAffinityMask(_p: Handle, _mask: u64) -> Bool {
    1
}
