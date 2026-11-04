//! Paths, files, directories, attributes and FindFirstFile.
//!
//! Paths are made absolute here (the current directory lives in the process
//! parameters, always with a trailing backslash) and handed to ntdll as NT
//! paths ("\??\C:\dir\file").

use nt_abi::{file as nf, layout, status, IoStatusBlock, NtStatus, ObjectAttributes, UnicodeString};

use crate::util::{narrow_out, wide_out, widen, wstr, WBuf};
use crate::{check, fail, Bool, Handle, INVALID_HANDLE_VALUE};

pub const FILE_TYPE_UNKNOWN: u32 = 0;
pub const FILE_TYPE_DISK: u32 = 1;
pub const FILE_TYPE_CHAR: u32 = 2;

const INVALID_FILE_ATTRIBUTES: u32 = u32::MAX;
const INVALID_SET_FILE_POINTER: u32 = u32::MAX;
const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
const FILE_FLAG_DELETE_ON_CLOSE: u32 = 0x0400_0000;
const OBJ_CASE_INSENSITIVE: u32 = 0x40;
const SYNCHRONIZE: u32 = 0x0010_0000;

fn is_sep(u: u16) -> bool {
    u == b'\\' as u16 || u == b'/' as u16
}

/// The current directory, "C:\dir\".
fn current_dir() -> &'static [u16] {
    unsafe {
        let us = &*(crate::process_parameters().add(layout::PARAMS_CURRENT_DIRECTORY) as *const UnicodeString);
        core::slice::from_raw_parts(us.buffer, us.length as usize / 2)
    }
}

/// `name` as a normalised absolute DOS path ("C:\a\b"; the root is "C:\").
pub fn full_path(name: &[u16]) -> WBuf {
    let mut name = name;
    // "\\?\C:\..." is already absolute.
    if name.len() >= 4 && is_sep(name[0]) && is_sep(name[1]) && name[2] == b'?' as u16 && is_sep(name[3]) {
        name = &name[4..];
    }
    let cwd = current_dir();
    let mut joined = WBuf::new();
    if name.len() >= 2 && name[1] == b':' as u16 {
        joined.extend(&name[..2]);
        joined.push(b'\\' as u16);
        joined.extend(&name[2..]);
    } else if name.first().is_some_and(|&u| is_sep(u)) {
        joined.extend(&cwd[..2.min(cwd.len())]);
        joined.extend(name);
    } else {
        joined.extend(cwd);
        joined.extend(name);
    }
    // Resolve "." and ".." and repeated separators.
    let src = joined.as_slice();
    let mut out = WBuf::new();
    out.extend(&src[..2]);
    let mut parts: [(usize, usize); 128] = [(0, 0); 128];
    let mut n: usize = 0;
    let mut i = 2;
    while i < src.len() {
        while i < src.len() && is_sep(src[i]) {
            i += 1;
        }
        let start = i;
        while i < src.len() && !is_sep(src[i]) {
            i += 1;
        }
        let part = &src[start..i];
        if part.is_empty() || part == [b'.' as u16] {
            continue;
        }
        if part == [b'.' as u16, b'.' as u16] {
            n = n.saturating_sub(1);
        } else if n < parts.len() {
            parts[n] = (start, i);
            n += 1;
        }
    }
    if n == 0 {
        out.push(b'\\' as u16);
    }
    for &(s, e) in &parts[..n] {
        out.push(b'\\' as u16);
        out.extend(&src[s..e]);
    }
    out
}

/// Run `f` with OBJECT_ATTRIBUTES naming the NT path of DOS path `name`.
fn with_name<R>(name: &[u16], f: impl FnOnce(&ObjectAttributes) -> R) -> R {
    let full = full_path(name);
    let mut nt = WBuf::new();
    nt.push_str("\\??\\");
    nt.extend(full.as_slice());
    let us = UnicodeString { length: (nt.len * 2) as u16, maximum_length: (nt.len * 2 + 2) as u16, buffer: nt.as_ptr() };
    let oa = ObjectAttributes {
        length: core::mem::size_of::<ObjectAttributes>() as u32,
        root_directory: 0,
        object_name: &us,
        attributes: OBJ_CASE_INSENSITIVE,
        security_descriptor: 0,
        security_quality_of_service: 0,
    };
    f(&oa)
}

/// NtCreateFile on a DOS path: (handle, IO_STATUS_BLOCK.Information).
fn open(name: &[u16], access: u32, disposition: u32, options: u32) -> Result<(Handle, u64), NtStatus> {
    with_name(name, |oa| {
        let mut h: Handle = 0;
        let mut iosb = IoStatusBlock::default();
        let s = unsafe {
            crate::NtCreateFile(&mut h, access | SYNCHRONIZE, oa, &mut iosb, core::ptr::null(), 0, 7, disposition, options, 0, 0)
        };
        if status::is_success(s) { Ok((h, iosb.information)) } else { Err(s) }
    })
}

/// FILE_RENAME_INFORMATION on an open handle: `to` as a DOS path.
pub fn rename_handle(h: Handle, to: &[u16], replace: bool) -> NtStatus {
    let full = full_path(to);
    let mut nt = WBuf::new();
    nt.push_str("\\??\\");
    nt.extend(full.as_slice());
    // ReplaceIfExists at 0, RootDirectory at 8, FileNameLength at 16, FileName at 20.
    let mut info = [0u8; 20 + 2 * 520];
    let bytes = nt.len * 2;
    if 20 + bytes > info.len() {
        return status::OBJECT_NAME_INVALID;
    }
    info[0] = replace as u8;
    info[16..20].copy_from_slice(&(bytes as u32).to_le_bytes());
    for (i, u) in nt.as_slice().iter().enumerate() {
        info[20 + i * 2..22 + i * 2].copy_from_slice(&u.to_le_bytes());
    }
    let mut iosb = IoStatusBlock::default();
    unsafe { crate::NtSetInformationFile(h, &mut iosb, info.as_ptr(), (20 + bytes) as u32, nf::RENAME_INFORMATION) }
}

/// Move what's at `from` (a file or directory) to `to`.
pub fn rename_path(from: &[u16], to: &[u16], replace: bool) -> NtStatus {
    match open(from, nf::DELETE, nf::OPEN, 0) {
        Ok((h, _)) => {
            let s = rename_handle(h, to, replace);
            unsafe { crate::NtClose(h) };
            s
        }
        Err(s) => s,
    }
}

/// `name` is the ASCII string `s`, ignoring case.
fn is_ascii_name(name: &[u16], s: &str) -> bool {
    name.len() == s.len() && name.iter().zip(s.bytes()).all(|(&u, b)| u < 0x80 && (u as u8).eq_ignore_ascii_case(&b))
}

/// The console's own names open its standard handles.
fn console_name(name: &[u16]) -> Option<u32> {
    if is_ascii_name(name, "CONOUT$") {
        Some(-11i32 as u32)
    } else if is_ascii_name(name, "CONIN$") || is_ascii_name(name, "CON") {
        Some(-10i32 as u32)
    } else {
        None
    }
}

#[no_mangle]
pub unsafe extern "system" fn CreateFileW(name: *const u16, access: u32, _share: u32, _security: usize, creation: u32, flags: u32, _template: Handle) -> Handle {
    let name = unsafe { wstr(name) };
    if let Some(which) = console_name(name) {
        return crate::GetStdHandle(which);
    }
    if crate::fiber::pipe_name(name) {
        return crate::fiber::pipe_client(name.as_ptr());
    }
    let disposition = match creation {
        1 => nf::CREATE,       // CREATE_NEW
        2 => nf::OVERWRITE_IF, // CREATE_ALWAYS
        3 => nf::OPEN,         // OPEN_EXISTING
        4 => nf::OPEN_IF,      // OPEN_ALWAYS
        5 => nf::OVERWRITE,    // TRUNCATE_EXISTING
        _ => {
            fail(status::INVALID_PARAMETER);
            return INVALID_HANDLE_VALUE;
        }
    };
    let mut options = if flags & FILE_FLAG_BACKUP_SEMANTICS != 0 { 0 } else { nf::NON_DIRECTORY_FILE };
    if flags & FILE_FLAG_DELETE_ON_CLOSE != 0 {
        options |= nf::DELETE_ON_CLOSE;
    }
    match open(name, access, disposition, options) {
        Ok((h, info)) => {
            // CREATE_ALWAYS / OPEN_ALWAYS on an existing file: ERROR_ALREADY_EXISTS.
            let existed = (creation == 2 || creation == 4) && (info == nf::OPENED || info == nf::OVERWRITTEN);
            crate::SetLastError(if existed { 183 } else { 0 });
            h
        }
        // CreateFile reports an existing file as ERROR_FILE_EXISTS (80), not
        // the ERROR_ALREADY_EXISTS (183) CreateDirectory uses.
        Err(status::OBJECT_NAME_COLLISION) => {
            crate::SetLastError(80);
            INVALID_HANDLE_VALUE
        }
        Err(s) => {
            fail(s);
            INVALID_HANDLE_VALUE
        }
    }
}

#[no_mangle]
pub unsafe extern "system" fn CreateFileA(name: *const u8, access: u32, share: u32, security: usize, creation: u32, flags: u32, template: Handle) -> Handle {
    let w = unsafe { widen(name) };
    unsafe { CreateFileW(w.as_ptr(), access, share, security, creation, flags, template) }
}

/// FILE_STANDARD_INFORMATION: (AllocationSize, EndOfFile, links, delete pending, directory).
fn standard_info(h: Handle) -> Result<[u8; 24], NtStatus> {
    let mut info = [0u8; 24];
    let mut iosb = IoStatusBlock::default();
    let s = unsafe { crate::NtQueryInformationFile(h, &mut iosb, info.as_mut_ptr(), 24, nf::STANDARD_INFORMATION) };
    if status::is_success(s) { Ok(info) } else { Err(s) }
}

fn file_size(h: Handle) -> Result<u64, NtStatus> {
    standard_info(h).map(|i| u64::from_le_bytes(i[8..16].try_into().unwrap()))
}

fn position(h: Handle) -> Result<u64, NtStatus> {
    let mut pos = 0u64;
    let mut iosb = IoStatusBlock::default();
    let s = unsafe { crate::NtQueryInformationFile(h, &mut iosb, &mut pos as *mut u64 as *mut u8, 8, nf::POSITION_INFORMATION) };
    if status::is_success(s) { Ok(pos) } else { Err(s) }
}

fn set_info(h: Handle, class: u32, value: u64) -> NtStatus {
    let mut iosb = IoStatusBlock::default();
    unsafe { crate::NtSetInformationFile(h, &mut iosb, &value as *const u64 as *const u8, 8, class) }
}

#[no_mangle]
pub extern "system" fn GetFileType(h: Handle) -> u32 {
    // FileFsDeviceInformation: the device type.
    let mut info = [0u32; 2];
    let mut iosb = IoStatusBlock::default();
    match unsafe { NtQueryVolumeInformationFile(h, &mut iosb, info.as_mut_ptr() as *mut u8, 8, 4) } {
        status::SUCCESS => match info[0] {
            0x07 | 0x08 => FILE_TYPE_DISK,
            0x11 => 3, // FILE_TYPE_PIPE
            0x50 | 0x14 => FILE_TYPE_CHAR,
            _ => FILE_TYPE_UNKNOWN,
        },
        s => {
            fail(s);
            FILE_TYPE_UNKNOWN
        }
    }
}

#[link(name = "ntdll", kind = "raw-dylib")]
unsafe extern "system" {
    fn NtQueryVolumeInformationFile(h: Handle, iosb: *mut IoStatusBlock, info: *mut u8, length: u32, class: u32) -> NtStatus;
}

#[no_mangle]
pub unsafe extern "system" fn GetFileSizeEx(h: Handle, size: *mut i64) -> Bool {
    match file_size(h) {
        Ok(n) => {
            unsafe { *size = n as i64 };
            1
        }
        Err(s) => fail(s),
    }
}

#[no_mangle]
pub unsafe extern "system" fn GetFileSize(h: Handle, high: *mut u32) -> u32 {
    match file_size(h) {
        Ok(n) => {
            if !high.is_null() {
                unsafe { *high = (n >> 32) as u32 };
            }
            crate::SetLastError(0);
            n as u32
        }
        Err(s) => {
            fail(s);
            u32::MAX
        }
    }
}

#[no_mangle]
pub unsafe extern "system" fn SetFilePointerEx(h: Handle, distance: i64, new: *mut i64, method: u32) -> Bool {
    let base = match method {
        0 => Ok(0),           // FILE_BEGIN
        1 => position(h),     // FILE_CURRENT
        2 => file_size(h),    // FILE_END
        _ => Err(status::INVALID_PARAMETER),
    };
    let base = match base {
        Ok(b) => b as i64,
        Err(s) => return fail(s),
    };
    let target = base + distance;
    if target < 0 {
        crate::SetLastError(131); // ERROR_NEGATIVE_SEEK
        return 0;
    }
    let s = set_info(h, nf::POSITION_INFORMATION, target as u64);
    if !status::is_success(s) {
        return fail(s);
    }
    if !new.is_null() {
        unsafe { *new = target };
    }
    1
}

#[no_mangle]
pub unsafe extern "system" fn SetFilePointer(h: Handle, low: i32, high: *mut i32, method: u32) -> u32 {
    let distance = if high.is_null() { low as i64 } else { ((unsafe { *high } as i64) << 32) | (low as u32 as i64) };
    let mut new = 0i64;
    if unsafe { SetFilePointerEx(h, distance, &mut new, method) } == 0 {
        return INVALID_SET_FILE_POINTER;
    }
    if !high.is_null() {
        unsafe { *high = (new >> 32) as i32 };
    }
    crate::SetLastError(0);
    new as u32
}

#[no_mangle]
pub extern "system" fn SetEndOfFile(h: Handle) -> Bool {
    match position(h) {
        Ok(p) => check(set_info(h, nf::END_OF_FILE_INFORMATION, p)),
        Err(s) => fail(s),
    }
}

#[no_mangle]
pub extern "system" fn FlushFileBuffers(h: Handle) -> Bool {
    let mut iosb = IoStatusBlock::default();
    check(unsafe { crate::NtFlushBuffersFile(h, &mut iosb) })
}

/// FILE_BASIC_INFORMATION for a path: (attributes, last write time).
fn basic(name: &[u16]) -> Result<(u32, [u8; 40]), NtStatus> {
    let mut info = [0u8; 40];
    let s = with_name(name, |oa| unsafe { crate::NtQueryAttributesFile(oa, info.as_mut_ptr()) });
    if status::is_success(s) {
        Ok((u32::from_le_bytes(info[32..36].try_into().unwrap()), info))
    } else {
        Err(s)
    }
}

#[no_mangle]
pub unsafe extern "system" fn GetFileAttributesW(name: *const u16) -> u32 {
    match basic(unsafe { wstr(name) }) {
        Ok((attrs, _)) => attrs,
        Err(s) => {
            fail(s);
            INVALID_FILE_ATTRIBUTES
        }
    }
}

#[no_mangle]
pub unsafe extern "system" fn GetFileAttributesA(name: *const u8) -> u32 {
    let w = unsafe { widen(name) };
    unsafe { GetFileAttributesW(w.as_ptr()) }
}

/// WIN32_FILE_ATTRIBUTE_DATA (36 bytes): attributes, three FILETIMEs, size.
#[no_mangle]
pub unsafe extern "system" fn GetFileAttributesExW(name: *const u16, _level: u32, data: *mut u8) -> Bool {
    let name = unsafe { wstr(name) };
    let (attrs, info) = match basic(name) {
        Ok(b) => b,
        Err(s) => return fail(s),
    };
    let size = if attrs & nf::ATTRIBUTE_DIRECTORY != 0 {
        0
    } else {
        match open(name, nf::READ_DATA, nf::OPEN, nf::NON_DIRECTORY_FILE) {
            Ok((h, _)) => {
                let n = file_size(h).unwrap_or(0);
                unsafe { crate::NtClose(h) };
                n
            }
            Err(_) => 0,
        }
    };
    unsafe {
        core::ptr::write_unaligned(data as *mut u32, attrs);
        core::ptr::copy_nonoverlapping(info.as_ptr(), data.add(4), 8); // creation
        core::ptr::copy_nonoverlapping(info.as_ptr().add(8), data.add(12), 8); // last access
        core::ptr::copy_nonoverlapping(info.as_ptr().add(16), data.add(20), 8); // last write
        core::ptr::write_unaligned(data.add(28) as *mut u32, (size >> 32) as u32);
        core::ptr::write_unaligned(data.add(32) as *mut u32, size as u32);
    }
    1
}

#[no_mangle]
pub unsafe extern "system" fn GetFileAttributesExA(name: *const u8, level: u32, data: *mut u8) -> Bool {
    let w = unsafe { widen(name) };
    unsafe { GetFileAttributesExW(w.as_ptr(), level, data) }
}

/// Attributes aren't stored by the VFS: accepted if the file exists.
#[no_mangle]
pub unsafe extern "system" fn SetFileAttributesW(name: *const u16, _attrs: u32) -> Bool {
    match basic(unsafe { wstr(name) }) {
        Ok(_) => 1,
        Err(s) => fail(s),
    }
}

#[no_mangle]
pub unsafe extern "system" fn SetFileAttributesA(name: *const u8, attrs: u32) -> Bool {
    let w = unsafe { widen(name) };
    unsafe { SetFileAttributesW(w.as_ptr(), attrs) }
}

#[no_mangle]
pub unsafe extern "system" fn DeleteFileW(name: *const u16) -> Bool {
    check(with_name(unsafe { wstr(name) }, |oa| unsafe { crate::NtDeleteFile(oa) }))
}

#[no_mangle]
pub unsafe extern "system" fn DeleteFileA(name: *const u8) -> Bool {
    let w = unsafe { widen(name) };
    unsafe { DeleteFileW(w.as_ptr()) }
}

#[no_mangle]
pub unsafe extern "system" fn RemoveDirectoryW(name: *const u16) -> Bool {
    unsafe { DeleteFileW(name) }
}

#[no_mangle]
pub unsafe extern "system" fn RemoveDirectoryA(name: *const u8) -> Bool {
    unsafe { DeleteFileA(name) }
}

#[no_mangle]
pub unsafe extern "system" fn CreateDirectoryW(name: *const u16, _security: usize) -> Bool {
    match open(unsafe { wstr(name) }, nf::READ_DATA, nf::CREATE, nf::DIRECTORY_FILE) {
        Ok((h, _)) => {
            unsafe { crate::NtClose(h) };
            1
        }
        Err(s) => fail(s),
    }
}

#[no_mangle]
pub unsafe extern "system" fn CreateDirectoryA(name: *const u8, security: usize) -> Bool {
    let w = unsafe { widen(name) };
    unsafe { CreateDirectoryW(w.as_ptr(), security) }
}

/// The current directory as Win32 shows it: no trailing backslash except
/// on a root ("C:\").
fn current_dir_display() -> &'static [u16] {
    let cwd = current_dir();
    if cwd.len() > 3 && is_sep(cwd[cwd.len() - 1]) { &cwd[..cwd.len() - 1] } else { cwd }
}

#[no_mangle]
pub unsafe extern "system" fn GetCurrentDirectoryW(size: u32, buffer: *mut u16) -> u32 {
    unsafe { wide_out(current_dir_display(), buffer, size) }
}

#[no_mangle]
pub unsafe extern "system" fn GetCurrentDirectoryA(size: u32, buffer: *mut u8) -> u32 {
    unsafe { narrow_out(current_dir_display(), buffer, size) }
}

#[no_mangle]
pub unsafe extern "system" fn SetCurrentDirectoryW(name: *const u16) -> Bool {
    let full = full_path(unsafe { wstr(name) });
    match basic(full.as_slice()) {
        Ok((attrs, _)) if attrs & nf::ATTRIBUTE_DIRECTORY != 0 => {}
        Ok(_) => return fail(status::NOT_A_DIRECTORY),
        Err(s) => return fail(s),
    }
    unsafe {
        let us = &mut *(crate::process_parameters().add(layout::PARAMS_CURRENT_DIRECTORY) as *mut UnicodeString);
        let cap = us.maximum_length as usize / 2;
        let mut path = WBuf::new();
        path.extend(full.as_slice());
        if !is_sep(path.buf[path.len - 1]) {
            path.push(b'\\' as u16);
        }
        if path.len + 1 > cap {
            crate::SetLastError(206); // ERROR_FILENAME_EXCED_RANGE
            return 0;
        }
        core::ptr::copy_nonoverlapping(path.as_ptr(), us.buffer as *mut u16, path.len + 1);
        us.length = (path.len * 2) as u16;
    }
    1
}

#[no_mangle]
pub unsafe extern "system" fn SetCurrentDirectoryA(name: *const u8) -> Bool {
    let w = unsafe { widen(name) };
    unsafe { SetCurrentDirectoryW(w.as_ptr()) }
}

#[no_mangle]
pub unsafe extern "system" fn GetFullPathNameW(name: *const u16, size: u32, buffer: *mut u16, file_part: *mut *mut u16) -> u32 {
    let full = full_path(unsafe { wstr(name) });
    let n = unsafe { wide_out(full.as_slice(), buffer, size) };
    if !file_part.is_null() && (n as usize) == full.len {
        let last = full.as_slice().iter().rposition(|&u| is_sep(u)).map_or(0, |i| i + 1);
        unsafe { *file_part = if last < full.len { buffer.add(last) } else { core::ptr::null_mut() } };
    }
    n
}

#[no_mangle]
pub unsafe extern "system" fn GetFullPathNameA(name: *const u8, size: u32, buffer: *mut u8, file_part: *mut *mut u8) -> u32 {
    let full = full_path(unsafe { widen(name) }.as_slice());
    let n = unsafe { narrow_out(full.as_slice(), buffer, size) };
    if !file_part.is_null() && (n as usize) == full.len {
        let last = full.as_slice().iter().rposition(|&u| is_sep(u)).map_or(0, |i| i + 1);
        unsafe { *file_part = if last < full.len { buffer.add(last) } else { core::ptr::null_mut() } };
    }
    n
}

const TEMP_PATH: &str = "C:\\tmp\\";

#[no_mangle]
pub unsafe extern "system" fn GetTempPathW(size: u32, buffer: *mut u16) -> u32 {
    let mut w = WBuf::new();
    w.push_str(TEMP_PATH);
    unsafe { wide_out(w.as_slice(), buffer, size) }
}

#[no_mangle]
pub unsafe extern "system" fn GetTempPathA(size: u32, buffer: *mut u8) -> u32 {
    let mut w = WBuf::new();
    w.push_str(TEMP_PATH);
    unsafe { narrow_out(w.as_slice(), buffer, size) }
}

// -----| FindFirstFile |-----

/// The next directory entry matching the handle's mask, as raw
/// FILE_DIRECTORY_INFORMATION (name at byte 64).
fn next_entry(h: Handle, mask: Option<&[u16]>, restart: bool, info: &mut [u8; 64 + 1040]) -> NtStatus {
    let us = mask.map(|m| UnicodeString { length: (m.len() * 2) as u16, maximum_length: (m.len() * 2) as u16, buffer: m.as_ptr() });
    let mut iosb = IoStatusBlock::default();
    unsafe {
        crate::NtQueryDirectoryFile(h, 0, 0, 0, &mut iosb, info.as_mut_ptr(), info.len() as u32, nf::DIRECTORY_INFORMATION, 1,
            us.as_ref().map_or(core::ptr::null(), |u| u as *const UnicodeString), restart as u8)
    }
}

/// WIN32_FIND_DATAW (592 bytes) from a directory entry.
unsafe fn fill_find_w(info: &[u8; 64 + 1040], data: *mut u8) {
    let name_len = (u32::from_le_bytes(info[60..64].try_into().unwrap()) / 2).min(259) as usize;
    let size = u64::from_le_bytes(info[40..48].try_into().unwrap());
    unsafe {
        core::ptr::write_bytes(data, 0, 592);
        core::ptr::copy_nonoverlapping(info.as_ptr().add(56), data, 4); // attributes
        core::ptr::copy_nonoverlapping(info.as_ptr().add(8), data.add(4), 24); // creation, access, write times
        core::ptr::write_unaligned(data.add(28) as *mut u32, (size >> 32) as u32);
        core::ptr::write_unaligned(data.add(32) as *mut u32, size as u32);
        core::ptr::copy_nonoverlapping(info.as_ptr().add(64), data.add(44), name_len * 2);
    }
}

/// WIN32_FIND_DATAA (320 bytes) from the wide form.
unsafe fn narrow_find(w: &[u8; 592], data: *mut u8) {
    unsafe {
        core::ptr::write_bytes(data, 0, 320);
        core::ptr::copy_nonoverlapping(w.as_ptr(), data, 44);
        for i in 0..259 {
            let u = u16::from_le_bytes([w[44 + i * 2], w[45 + i * 2]]);
            if u == 0 {
                break;
            }
            *data.add(44 + i) = if u < 0x100 { u as u8 } else { b'?' };
        }
    }
}

#[no_mangle]
pub unsafe extern "system" fn FindFirstFileW(pattern: *const u16, data: *mut u8) -> Handle {
    let full = full_path(unsafe { wstr(pattern) });
    let s = full.as_slice();
    let split = s.iter().rposition(|&u| is_sep(u)).unwrap_or(2);
    let dir = &s[..(split + 1).max(3).min(s.len())];
    let mask = &s[(split + 1).min(s.len())..];
    let h = match open(dir, nf::READ_DATA, nf::OPEN, nf::DIRECTORY_FILE) {
        Ok((h, _)) => h,
        Err(st) => {
            fail(st);
            return INVALID_HANDLE_VALUE;
        }
    };
    let mut info = [0u8; 64 + 1040];
    let st = next_entry(h, if mask.is_empty() { None } else { Some(mask) }, true, &mut info);
    if !status::is_success(st) || st == status::NO_MORE_FILES {
        unsafe { crate::NtClose(h) };
        crate::SetLastError(2); // ERROR_FILE_NOT_FOUND
        return INVALID_HANDLE_VALUE;
    }
    unsafe { fill_find_w(&info, data) };
    h
}

#[no_mangle]
pub unsafe extern "system" fn FindNextFileW(h: Handle, data: *mut u8) -> Bool {
    let mut info = [0u8; 64 + 1040];
    let st = next_entry(h, None, false, &mut info);
    if st == status::NO_MORE_FILES || !status::is_success(st) {
        return fail(st);
    }
    unsafe { fill_find_w(&info, data) };
    1
}

#[no_mangle]
pub unsafe extern "system" fn FindFirstFileExW(pattern: *const u16, _level: u32, data: *mut u8, _op: u32, _filter: usize, _flags: u32) -> Handle {
    unsafe { FindFirstFileW(pattern, data) }
}

#[no_mangle]
pub unsafe extern "system" fn FindFirstFileA(pattern: *const u8, data: *mut u8) -> Handle {
    let w = unsafe { widen(pattern) };
    let mut wide = [0u8; 592];
    let h = unsafe { FindFirstFileW(w.as_ptr(), wide.as_mut_ptr()) };
    if h != INVALID_HANDLE_VALUE {
        unsafe { narrow_find(&wide, data) };
    }
    h
}

#[no_mangle]
pub unsafe extern "system" fn FindNextFileA(h: Handle, data: *mut u8) -> Bool {
    let mut wide = [0u8; 592];
    if unsafe { FindNextFileW(h, wide.as_mut_ptr()) } == 0 {
        return 0;
    }
    unsafe { narrow_find(&wide, data) };
    1
}

#[no_mangle]
pub extern "system" fn FindClose(h: Handle) -> Bool {
    check(unsafe { crate::NtClose(h) })
}
