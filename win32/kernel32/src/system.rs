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
const MOVEFILE_COPY_ALLOWED: u32 = 2;
const STATUS_NOT_SAME_DEVICE: NtStatus = 0xC000_00D4u32 as NtStatus;

/// No rename in the file system yet: a file moves by copy and delete.
#[no_mangle]
pub unsafe extern "system" fn MoveFileExW(from: *const u16, to: *const u16, flags: u32) -> Bool {
    let attrs = unsafe { crate::file::GetFileAttributesW(from) };
    if attrs == u32::MAX {
        return 0;
    }
    if to.is_null() {
        return unsafe { crate::file::DeleteFileW(from) };
    }
    let replace = flags & MOVEFILE_REPLACE_EXISTING != 0;
    let s = crate::file::rename_path(unsafe { wstr(from) }, unsafe { wstr(to) }, replace);
    // Between volumes a file is copied, if the caller allows it.
    if s == STATUS_NOT_SAME_DEVICE && attrs & 0x10 == 0 && flags & MOVEFILE_COPY_ALLOWED != 0 {
        if unsafe { CopyFileW(from, to, !replace as Bool) } == 0 {
            return 0;
        }
        return unsafe { crate::file::DeleteFileW(from) };
    }
    check(s)
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

/// One time is kept per file: the last write.
#[no_mangle]
pub unsafe extern "system" fn SetFileTime(h: Handle, _c: *const u64, _a: *const u64, w: *const u64) -> Bool {
    let mut basic = [0u8; 40];
    if !w.is_null() {
        basic[16..24].copy_from_slice(&unsafe { *w }.to_le_bytes());
    }
    let mut iosb = nt_abi::IoStatusBlock::default();
    check(unsafe { crate::NtSetInformationFile(h, &mut iosb, basic.as_ptr(), 40, 4) })
}

#[no_mangle]
pub unsafe extern "system" fn GetFileTime(h: Handle, c: *mut u64, a: *mut u64, w: *mut u64) -> Bool {
    let mut basic = [0u8; 40];
    let mut iosb = nt_abi::IoStatusBlock::default();
    let r = check(unsafe { crate::NtQueryInformationFile(h, &mut iosb, basic.as_mut_ptr(), 40, 4) });
    if r == 0 {
        return 0;
    }
    for (p, at) in [(c, 0usize), (a, 8), (w, 16)] {
        if !p.is_null() {
            unsafe { *p = u64::from_le_bytes(basic[at..at + 8].try_into().unwrap()) };
        }
    }
    1
}

/// FILE_INFO_BY_HANDLE_CLASS: basic (0), rename (3 and 22), disposition (4 and
/// 21), end of file (6). The Ex forms start with flags where the plain ones
/// start with a boolean.
#[no_mangle]
pub unsafe extern "system" fn SetFileInformationByHandle(h: Handle, class: u32, info: *const u8, size: u32) -> Bool {
    let mut iosb = nt_abi::IoStatusBlock::default();
    let set = |class: u32, p: *const u8, n: u32, iosb: &mut nt_abi::IoStatusBlock| check(unsafe { crate::NtSetInformationFile(h, iosb, p, n, class) });
    match class {
        0 => set(4, info, size, &mut iosb),
        3 | 22 => {
            // Flag 1 is FILE_RENAME_FLAG_REPLACE_IF_EXISTS, in the same place.
            let replace = unsafe { *info } & 1;
            let bytes = unsafe { core::ptr::read_unaligned(info.add(16) as *const u32) } as usize;
            let name = unsafe { core::slice::from_raw_parts(info.add(20) as *const u16, bytes / 2) };
            let s = crate::file::rename_handle(h, name, replace != 0);
            check(s)
        }
        4 | 21 => {
            // Flag 1 is FILE_DISPOSITION_FLAG_DELETE, in the same place.
            let on = (unsafe { *info } & 1) as u8;
            set(13, &on, 1, &mut iosb)
        }
        6 => set(20, info, 8, &mut iosb),
        _ => {
            crate::SetLastError(ERROR_NOT_SUPPORTED);
            0
        }
    }
}

/// FILE_ID_BOTH_DIR_INFO entries, as many as fit, from NtQueryDirectoryFile.
fn directory_info(h: Handle, out: *mut u8, size: u32, restart: bool) -> Bool {
    const NAME_AT: usize = 104;
    let mut at = 0usize;
    let mut previous: Option<usize> = None;
    let mut restart = restart as u8;
    loop {
        // FILE_DIRECTORY_INFORMATION: times at 8, sizes at 40, attributes at
        // 56, name length at 60, name at 64.
        let mut entry = [0u8; 64 + 520];
        let mut iosb = nt_abi::IoStatusBlock::default();
        let s = unsafe {
            crate::NtQueryDirectoryFile(h, 0, 0, 0, &mut iosb, entry.as_mut_ptr(), entry.len() as u32, 1, 1, core::ptr::null(), restart)
        };
        restart = 0;
        if s == status::NO_MORE_FILES {
            if at != 0 {
                return 1;
            }
            crate::SetLastError(18); // ERROR_NO_MORE_FILES
            return 0;
        }
        if s != status::SUCCESS {
            return fail(s);
        }
        let bytes = u32::from_le_bytes(entry[60..64].try_into().unwrap()) as usize;
        let need = (NAME_AT + bytes + 7) & !7;
        if at + need > size as usize {
            // Doesn't fit: this one is lost to the position unless it's rewound,
            // so a buffer is only ever filled to what's certain to fit.
            if at == 0 {
                crate::SetLastError(122); // ERROR_INSUFFICIENT_BUFFER
                return 0;
            }
            return 1;
        }
        unsafe {
            let e = out.add(at);
            core::ptr::write_bytes(e, 0, NAME_AT);
            core::ptr::copy_nonoverlapping(entry.as_ptr().add(8), e.add(8), 52 + 4 - 4);
            core::ptr::write_unaligned(e.add(60) as *mut u32, bytes as u32);
            core::ptr::copy_nonoverlapping(entry.as_ptr().add(64), e.add(NAME_AT), bytes);
            if let Some(p) = previous {
                core::ptr::write_unaligned(out.add(p) as *mut u32, (at - p) as u32);
            }
        }
        previous = Some(at);
        at += need;
        // A longest name must still fit, so nothing is ever read and dropped.
        if at + ((NAME_AT + 520 + 7) & !7) > size as usize {
            return 1;
        }
    }
}

/// FILE_INFO_BY_HANDLE_CLASS: FileBasicInfo (0) and FileStandardInfo (1),
/// from the NT information classes they mirror.
#[no_mangle]
pub unsafe extern "system" fn GetFileInformationByHandleEx(h: Handle, class: u32, out: *mut u8, size: u32) -> Bool {
    let nt_class = match class {
        0 => 4, // FileBasicInformation
        1 => 5, // FileStandardInformation
        2 => 9, // FileNameInformation
        9 => {
            // FILE_ATTRIBUTE_TAG_INFO: no reparse points here.
            let mut basic = [0u8; 40];
            let mut iosb = nt_abi::IoStatusBlock::default();
            let r = check(unsafe { crate::NtQueryInformationFile(h, &mut iosb, basic.as_mut_ptr(), 40, 4) });
            if r != 0 {
                unsafe {
                    core::ptr::copy_nonoverlapping(basic.as_ptr().add(32), out, 4);
                    core::ptr::write_bytes(out.add(4), 0, 4);
                }
            }
            return r;
        }
        10 => return directory_info(h, out, size, false),
        11 => return directory_info(h, out, size, true),
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
// Owned per thread, and recursive: the kernel keeps the count.

#[no_mangle]
pub unsafe extern "system" fn CreateMutexExW(_sa: usize, name: *const u16, flags: u32, access: u32) -> Handle {
    if let Some(found) = crate::named_existing(name, crate::KIND_MUTEX) {
        return found;
    }
    let mut h: Handle = 0;
    let _ = access;
    let owned = flags & 1 != 0; // CREATE_MUTEX_INITIAL_OWNER
    let s = unsafe { crate::VeilMutex(0, owned as u64, &mut h as *mut Handle as u64) };
    if s != status::SUCCESS {
        fail(s);
        return 0;
    }
    crate::name_new(h, name, crate::KIND_MUTEX);
    h
}

#[no_mangle]
pub unsafe extern "system" fn CreateMutexW(sa: usize, owned: Bool, name: *const u16) -> Handle {
    unsafe { CreateMutexExW(sa, name, (owned != 0) as u32, 0) }
}

#[no_mangle]
pub unsafe extern "system" fn CreateMutexA(sa: usize, owned: Bool, name: *const u8) -> Handle {
    if name.is_null() {
        return unsafe { CreateMutexExW(sa, core::ptr::null(), (owned != 0) as u32, 0) };
    }
    let w = unsafe { crate::util::widen(name) };
    unsafe { CreateMutexExW(sa, w.as_ptr(), (owned != 0) as u32, 0) }
}

#[no_mangle]
pub extern "system" fn ReleaseMutex(h: Handle) -> Bool {
    check(unsafe { crate::VeilMutex(1, h, 0) })
}

#[no_mangle]
pub unsafe extern "system" fn OpenMutexW(_access: u32, _inherit: Bool, name: *const u16) -> Handle {
    crate::open_named(name, crate::KIND_MUTEX)
}

#[no_mangle]
pub unsafe extern "system" fn OpenMutexA(access: u32, inherit: Bool, name: *const u8) -> Handle {
    let w = unsafe { crate::util::widen(name) };
    unsafe { OpenMutexW(access, inherit, w.as_ptr()) }
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

/// Every process is in the one processor group, group 0.
#[no_mangle]
pub unsafe extern "system" fn GetProcessGroupAffinity(_process: Handle, count: *mut u16, groups: *mut u16) -> Bool {
    if count.is_null() {
        crate::SetLastError(87); // ERROR_INVALID_PARAMETER
        return 0;
    }
    let room = unsafe { *count };
    unsafe { *count = 1 };
    if room < 1 || groups.is_null() {
        crate::SetLastError(122); // ERROR_INSUFFICIENT_BUFFER
        return 0;
    }
    unsafe { *groups = 0 };
    1
}

/// One group, one NUMA node, one package, and each CPU a core of its own, as
/// far as VEILOS knows. Relations: 0 cores, 1 NUMA, 3 packages, 4 groups,
/// 0xFFFF all; no caches are described.
#[no_mangle]
pub unsafe extern "system" fn GetLogicalProcessorInformationEx(relation: u32, out: *mut u8, length: *mut u32) -> Bool {
    const ERROR_INSUFFICIENT_BUFFER: u32 = 122;
    const ERROR_INVALID_PARAMETER: u32 = 87;
    if length.is_null() {
        crate::SetLastError(ERROR_INVALID_PARAMETER);
        return 0;
    }
    let mut info = [0u8; 48];
    unsafe { crate::mem::GetSystemInfo(info.as_mut_ptr()) };
    let cpus = u32::from_le_bytes(info[32..36].try_into().unwrap()).clamp(1, 64) as usize;
    let all_cpus = if cpus == 64 { u64::MAX } else { (1u64 << cpus) - 1 };
    let want = |r: u32| relation == r || relation == 0xFFFF;
    const AFFINITY_ENTRY: usize = 48;
    const GROUP_ENTRY: usize = 80;
    let need = want(0) as usize * cpus * AFFINITY_ENTRY + (want(1) as usize + want(3) as usize) * AFFINITY_ENTRY + want(4) as usize * GROUP_ENTRY;
    let room = unsafe { *length } as usize;
    unsafe { *length = need as u32 };
    if out.is_null() || room < need {
        crate::SetLastError(ERROR_INSUFFICIENT_BUFFER);
        return 0;
    }
    unsafe { core::ptr::write_bytes(out, 0, need) };
    let mut at = out;
    // Relationship, Size, then PROCESSOR_RELATIONSHIP or NUMA_NODE_RELATIONSHIP:
    // both have GroupCount at 30 and their GROUP_AFFINITY at 32.
    let mut entry = |kind: u32, mask: u64| unsafe {
        (at as *mut u32).write_unaligned(kind);
        (at.add(4) as *mut u32).write_unaligned(AFFINITY_ENTRY as u32);
        (at.add(30) as *mut u16).write_unaligned(1);
        (at.add(32) as *mut u64).write_unaligned(mask);
        at = at.add(AFFINITY_ENTRY);
    };
    if want(0) {
        for i in 0..cpus {
            entry(0, 1u64 << i);
        }
    }
    if want(1) {
        entry(1, all_cpus);
    }
    if want(3) {
        entry(3, all_cpus);
    }
    if want(4) {
        // GROUP_RELATIONSHIP: maximum and active group counts, then at 32 a
        // PROCESSOR_GROUP_INFO: maximum and active CPUs, and at 40 their mask.
        unsafe {
            (at as *mut u32).write_unaligned(4);
            (at.add(4) as *mut u32).write_unaligned(GROUP_ENTRY as u32);
            (at.add(8) as *mut u16).write_unaligned(1);
            (at.add(10) as *mut u16).write_unaligned(1);
            *at.add(32) = cpus as u8;
            *at.add(33) = cpus as u8;
            (at.add(72) as *mut u64).write_unaligned(all_cpus);
        }
    }
    1
}

#[no_mangle]
pub unsafe extern "system" fn ReadProcessMemory(p: Handle, addr: usize, buf: *mut u8, n: usize, read: *mut usize) -> Bool {
    #[link(name = "ntdll", kind = "raw-dylib")]
    unsafe extern "system" {
        fn NtReadVirtualMemory(process: Handle, address: usize, buffer: *mut u8, size: usize, read: *mut usize) -> NtStatus;
    }
    let mut done = 0usize;
    let s = unsafe { NtReadVirtualMemory(p, addr, buf, n, &mut done) };
    if !read.is_null() {
        unsafe { *read = done };
    }
    // Windows fails a partial read too (ERROR_PARTIAL_COPY), saying how much it got.
    if s == nt_abi::status::SUCCESS { 1 } else { crate::fail(s) }
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
