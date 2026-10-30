// =============================================================================
// VEILOS: files for Windows programs (WIN32_DESIGN.md, W4)
// =============================================================================
//
// NtCreateFile and friends over the VFS. Paths arrive as NT paths
// ("\??\C:\dir\file.txt", made by kernel32 from DOS paths): any drive letter
// is the VFS root, backslashes become slashes.
//
// The VFS reads and writes whole files, so an open file is a buffer: the
// file's contents are read on open (unless it's being replaced), reads and
// writes work on the buffer at the handle's position, and a changed buffer
// is written back when the last handle to it closes (or on a flush). Two
// separate opens of one file each have their own buffer; the last to close
// wins.

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};
use nt_abi::{file, status, IoStatusBlock, NtStatus, ObjectAttributes, UnicodeString};
use spin::Mutex;

use crate::vfs::{FileType, VFS};

pub struct FileObject {
    /// VFS path ("/dir/file.txt").
    path: String,
    is_dir: bool,
    access: u32,
    data: Mutex<Vec<u8>>,
    position: Mutex<u64>,
    dirty: AtomicBool,
    delete_on_close: AtomicBool,
    /// Directory listing for NtQueryDirectoryFile: entries and next index.
    listing: Mutex<Option<(Vec<(String, u64, bool)>, usize)>>,
}

impl FileObject {
    fn flush(&self) -> NtStatus {
        if self.is_dir || !self.dirty.swap(false, Ordering::AcqRel) {
            return status::SUCCESS;
        }
        match VFS.write_file(&self.path, &self.data.lock()) {
            Ok(()) => status::SUCCESS,
            Err(_) => status::ACCESS_DENIED,
        }
    }
}

impl Drop for FileObject {
    fn drop(&mut self) {
        if self.delete_on_close.load(Ordering::Acquire) {
            let _ = VFS.delete_file(&self.path);
            return;
        }
        if self.flush() != status::SUCCESS {
            crate::serial_println!("[FILE] {}: could not write back", self.path);
        }
    }
}

// -----| user memory |-----

pub fn ptr_ok(p: u64, len: usize) -> bool {
    len == 0 || crate::memory::user_ptr_ok(p, len)
}

/// A UNICODE_STRING at `us` (user memory).
fn read_unicode(us: u64) -> Option<String> {
    if !ptr_ok(us, core::mem::size_of::<UnicodeString>()) {
        return None;
    }
    let s = unsafe { core::ptr::read_unaligned(us as *const UnicodeString) };
    let units = s.length as usize / 2;
    if !ptr_ok(s.buffer as u64, units * 2) {
        return None;
    }
    let slice = unsafe { core::slice::from_raw_parts(s.buffer, units) };
    Some(char::decode_utf16(slice.iter().copied()).map(|c| c.unwrap_or('?')).collect())
}

/// The name in OBJECT_ATTRIBUTES at `oa` (user memory).
fn read_object_name(oa: u64) -> Option<String> {
    if !ptr_ok(oa, core::mem::size_of::<ObjectAttributes>()) {
        return None;
    }
    let attrs = unsafe { &*(oa as *const ObjectAttributes) };
    read_unicode(attrs.object_name as u64)
}

/// NT path → VFS path: "\??\C:\a\b" → "/a/b". None if it isn't a drive path.
pub fn vfs_path(nt: &str) -> Option<String> {
    let p = nt.strip_prefix("\\??\\").or_else(|| nt.strip_prefix("\\\\?\\")).unwrap_or(nt);
    let b = p.as_bytes();
    if b.len() < 2 || b[1] != b':' || !b[0].is_ascii_alphabetic() {
        return None;
    }
    let mut out = String::from("/");
    for part in p[2..].split(|c| c == '\\' || c == '/').filter(|s| !s.is_empty()) {
        match part {
            "." => {}
            ".." => {
                if let Some(i) = out.trim_end_matches('/').rfind('/') {
                    out.truncate(i + 1);
                }
            }
            _ => {
                out.push_str(part);
                out.push('/');
            }
        }
    }
    if out.len() > 1 {
        out.pop();
    }
    Some(out)
}

/// What's at `path`: (is a directory, size).
fn stat(path: &str) -> Option<(bool, u64)> {
    if path == "/" {
        return Some((true, 0));
    }
    let (dir, name) = path.rsplit_once('/')?;
    let dir = if dir.is_empty() { "/" } else { dir };
    VFS.list_dir(dir).ok()?.into_iter()
        .find(|e| e.name.eq_ignore_ascii_case(name))
        .map(|e| (e.file_type == FileType::Directory, e.size))
}

fn parent_exists(path: &str) -> bool {
    match path.rsplit_once('/') {
        Some(("", _)) | None => true,
        Some((dir, _)) => stat(dir).is_some_and(|(d, _)| d),
    }
}

// -----| services |-----

fn write_iosb(iosb: u64, status: NtStatus, information: u64) {
    if ptr_ok(iosb, 16) {
        unsafe { *(iosb as *mut IoStatusBlock) = IoStatusBlock { status: status as u32 as u64, information } };
    }
}

/// NtCreateFile(FileHandle*, DesiredAccess, ObjectAttributes*, IoStatusBlock*,
/// AllocationSize*, FileAttributes, ShareAccess, CreateDisposition,
/// CreateOptions, EaBuffer, EaLength).
pub fn create_file(proc: &crate::process::Process, out: u64, access: u64, oa: u64, iosb: u64) -> NtStatus {
    let (Some(disposition), Some(options)) = (crate::interrupts::syscall_stack_arg(7), crate::interrupts::syscall_stack_arg(8)) else {
        return status::ACCESS_VIOLATION;
    };
    let (disposition, options, access) = (disposition as u32, options as u32, access as u32);
    if !ptr_ok(out, 8) {
        return status::ACCESS_VIOLATION;
    }
    let Some(name) = read_object_name(oa) else { return status::ACCESS_VIOLATION };
    let Some(path) = vfs_path(&name) else { return status::OBJECT_NAME_INVALID };
    let existing = stat(&path);
    let want_dir = options & file::DIRECTORY_FILE != 0;

    let (is_dir, data, info) = match (existing, disposition) {
        (Some(_), file::CREATE) => return status::OBJECT_NAME_COLLISION,
        (None, file::OPEN | file::OVERWRITE) => {
            return if parent_exists(&path) { status::OBJECT_NAME_NOT_FOUND } else { status::OBJECT_PATH_NOT_FOUND };
        }
        (Some((true, _)), _) if options & file::NON_DIRECTORY_FILE != 0 => return status::FILE_IS_A_DIRECTORY,
        (Some((false, _)), _) if want_dir => return status::NOT_A_DIRECTORY,
        (Some((true, _)), _) => (true, Vec::new(), file::OPENED),
        (Some((false, _)), file::OPEN | file::OPEN_IF) => {
            let data = VFS.read_file(&path).unwrap_or_default();
            (false, data, file::OPENED)
        }
        (Some((false, _)), _) => {
            // SUPERSEDE / OVERWRITE / OVERWRITE_IF: start empty.
            if VFS.write_file(&path, &[]).is_err() {
                return status::ACCESS_DENIED;
            }
            (false, Vec::new(), if disposition == file::SUPERSEDE { file::SUPERSEDED } else { file::OVERWRITTEN })
        }
        (None, _) => {
            if !parent_exists(&path) {
                return status::OBJECT_PATH_NOT_FOUND;
            }
            let made = if want_dir { VFS.create_dir(&path) } else { VFS.write_file(&path, &[]) };
            if made.is_err() {
                return status::ACCESS_DENIED;
            }
            (want_dir, Vec::new(), file::CREATED)
        }
    };
    let obj = Arc::new(FileObject {
        path,
        is_dir,
        access,
        data: Mutex::new(data),
        position: Mutex::new(0),
        dirty: AtomicBool::new(false),
        delete_on_close: AtomicBool::new(options & file::DELETE_ON_CLOSE != 0),
        listing: Mutex::new(None),
    });
    let h = proc.handles.lock().allocate_ref(crate::nt_objects::KernelObjectRef::File(obj), access, 0);
    unsafe { *(out as *mut u64) = h as u64 };
    write_iosb(iosb, status::SUCCESS, info);
    status::SUCCESS
}

fn can_write(f: &FileObject) -> bool {
    f.access & (file::WRITE_DATA | file::APPEND_DATA | file::GENERIC_WRITE | 0x1000_0000) != 0
}

/// The offset an I/O starts at: ByteOffset if given (-1 = end of file,
/// -2 = current position), else the handle's position.
fn start_offset(f: &FileObject, offset_ptr: u64, len: u64) -> u64 {
    let given = (offset_ptr != 0 && ptr_ok(offset_ptr, 8)).then(|| unsafe { *(offset_ptr as *const i64) });
    match given {
        Some(-1) => len,
        Some(o) if o >= 0 => o as u64,
        _ if f.access & file::APPEND_DATA != 0 && f.access & file::WRITE_DATA == 0 => len,
        _ => *f.position.lock(),
    }
}

pub fn read(f: &FileObject, iosb: u64, buffer: u64, length: usize, offset_ptr: u64) -> NtStatus {
    if f.is_dir {
        return status::FILE_IS_A_DIRECTORY;
    }
    let data = f.data.lock();
    let start = start_offset(f, offset_ptr, data.len() as u64) as usize;
    if start >= data.len() && length > 0 {
        write_iosb(iosb, status::END_OF_FILE, 0);
        return status::END_OF_FILE;
    }
    let n = length.min(data.len().saturating_sub(start));
    unsafe { core::ptr::copy_nonoverlapping(data[start..].as_ptr(), buffer as *mut u8, n) };
    *f.position.lock() = (start + n) as u64;
    write_iosb(iosb, status::SUCCESS, n as u64);
    status::SUCCESS
}

pub fn write(f: &FileObject, iosb: u64, buffer: u64, length: usize, offset_ptr: u64) -> NtStatus {
    if f.is_dir {
        return status::FILE_IS_A_DIRECTORY;
    }
    if !can_write(f) {
        return status::ACCESS_DENIED;
    }
    let mut data = f.data.lock();
    let start = start_offset(f, offset_ptr, data.len() as u64) as usize;
    if data.len() < start + length {
        data.resize(start + length, 0);
    }
    unsafe { core::ptr::copy_nonoverlapping(buffer as *const u8, data[start..].as_mut_ptr(), length) };
    *f.position.lock() = (start + length) as u64;
    f.dirty.store(true, Ordering::Release);
    write_iosb(iosb, status::SUCCESS, length as u64);
    status::SUCCESS
}

fn attributes(is_dir: bool) -> u32 {
    if is_dir { file::ATTRIBUTE_DIRECTORY } else { file::ATTRIBUTE_ARCHIVE }
}

fn now_filetime() -> i64 {
    let k = nt_abi::kusd::ADDRESS as u64 + nt_abi::kusd::SYSTEM_TIME as u64;
    // Read through the process's own mapping of the shared page.
    unsafe { core::ptr::read_unaligned(k as *const i64) }
}

/// FILE_BASIC_INFORMATION (40 bytes) at `out`.
fn put_basic(out: u64, is_dir: bool) {
    let t = now_filetime();
    unsafe {
        for i in 0..4 {
            core::ptr::write_unaligned((out + i * 8) as *mut i64, t);
        }
        core::ptr::write_unaligned((out + 32) as *mut u32, attributes(is_dir));
        core::ptr::write_unaligned((out + 36) as *mut u32, 0);
    }
}

/// NtQueryInformationFile(FileHandle, IoStatusBlock*, FileInformation*, Length, Class).
pub fn query_information(f: &FileObject, iosb: u64, out: u64, length: usize, class: u32) -> NtStatus {
    let need = match class {
        file::BASIC_INFORMATION => 40,
        file::STANDARD_INFORMATION => 24,
        file::POSITION_INFORMATION => 8,
        _ => return status::INVALID_INFO_CLASS,
    };
    if length < need {
        return status::BUFFER_TOO_SMALL;
    }
    if !ptr_ok(out, need) {
        return status::ACCESS_VIOLATION;
    }
    let size = f.data.lock().len() as u64;
    unsafe {
        match class {
            file::BASIC_INFORMATION => put_basic(out, f.is_dir),
            file::STANDARD_INFORMATION => {
                core::ptr::write_unaligned(out as *mut u64, (size + 4095) & !4095); // AllocationSize
                core::ptr::write_unaligned((out + 8) as *mut u64, size); // EndOfFile
                core::ptr::write_unaligned((out + 16) as *mut u32, 1); // NumberOfLinks
                *((out + 20) as *mut u8) = f.delete_on_close.load(Ordering::Relaxed) as u8;
                *((out + 21) as *mut u8) = f.is_dir as u8;
                core::ptr::write_unaligned((out + 22) as *mut u16, 0);
            }
            _ => core::ptr::write_unaligned(out as *mut u64, *f.position.lock()),
        }
    }
    write_iosb(iosb, status::SUCCESS, need as u64);
    status::SUCCESS
}

/// NtSetInformationFile(FileHandle, IoStatusBlock*, FileInformation*, Length, Class).
pub fn set_information(f: &FileObject, iosb: u64, info: u64, length: usize, class: u32) -> NtStatus {
    if !ptr_ok(info, length) || length == 0 {
        return status::ACCESS_VIOLATION;
    }
    match class {
        file::POSITION_INFORMATION if length >= 8 => *f.position.lock() = unsafe { *(info as *const u64) },
        file::END_OF_FILE_INFORMATION if length >= 8 => {
            if !can_write(f) {
                return status::ACCESS_DENIED;
            }
            f.data.lock().resize(unsafe { *(info as *const u64) } as usize, 0);
            f.dirty.store(true, Ordering::Release);
        }
        file::DISPOSITION_INFORMATION => f.delete_on_close.store(unsafe { *(info as *const u8) } != 0, Ordering::Release),
        // Times and attributes aren't stored by the VFS; preallocation needs nothing.
        file::BASIC_INFORMATION | file::ALLOCATION_INFORMATION => {}
        _ => return status::INVALID_INFO_CLASS,
    }
    write_iosb(iosb, status::SUCCESS, 0);
    status::SUCCESS
}

/// `name` matches a DOS wildcard `mask` (`*`, `?`), ignoring case.
fn wildcard(mask: &[char], name: &[char]) -> bool {
    match mask.first() {
        None => name.is_empty(),
        Some('*') => (0..=name.len()).any(|i| wildcard(&mask[1..], &name[i..])),
        Some('?') => !name.is_empty() && wildcard(&mask[1..], &name[1..]),
        Some(&c) => name.first().is_some_and(|&n| n.eq_ignore_ascii_case(&c)) && wildcard(&mask[1..], &name[1..]),
    }
}

/// NtQueryDirectoryFile(FileHandle, Event, ApcRoutine, ApcContext,
/// IoStatusBlock*, FileInformation*, Length, FileInformationClass,
/// ReturnSingleEntry, FileName*, RestartScan): one FILE_DIRECTORY_INFORMATION
/// entry per call. The mask (default "*") is taken on the first call.
pub fn query_directory(f: &FileObject) -> NtStatus {
    // Arguments 4-10: IoStatusBlock, FileInformation, Length, Class,
    // ReturnSingleEntry (always treated as TRUE), FileName, RestartScan.
    let args: Vec<u64> = (4..11).map(|i| crate::interrupts::syscall_stack_arg(i).unwrap_or(0)).collect();
    let (iosb, out, length, class, mask_ptr, restart) = (args[0], args[1], args[2] as u32 as usize, args[3] as u32, args[5], args[6] as u8 != 0);
    if !f.is_dir {
        return status::INVALID_PARAMETER;
    }
    if class != file::DIRECTORY_INFORMATION {
        return status::INVALID_INFO_CLASS;
    }
    let mut listing = f.listing.lock();
    if listing.is_none() || restart {
        let mask: Vec<char> = if mask_ptr != 0 { read_unicode(mask_ptr).unwrap_or_default().chars().collect() } else { Vec::new() };
        let mask = if mask.is_empty() { alloc::vec!['*'] } else { mask };
        let mut entries: Vec<(String, u64, bool)> = Vec::new();
        if f.path != "/" {
            entries.push((String::from("."), 0, true));
            entries.push((String::from(".."), 0, true));
        }
        for e in VFS.list_dir(&f.path).unwrap_or_default() {
            entries.push((e.name, e.size, e.file_type == FileType::Directory));
        }
        entries.retain(|(n, _, _)| wildcard(&mask, &n.chars().collect::<Vec<_>>()));
        *listing = Some((entries, 0));
    }
    let (entries, next) = listing.as_mut().unwrap();
    let Some((name, size, is_dir)) = entries.get(*next).cloned() else {
        write_iosb(iosb, status::NO_MORE_FILES, 0);
        return status::NO_MORE_FILES;
    };
    let units: Vec<u16> = name.encode_utf16().collect();
    let need = file::DIRECTORY_INFORMATION_SIZE + units.len() * 2;
    if length < need {
        return status::BUFFER_TOO_SMALL;
    }
    if !ptr_ok(out, need) {
        return status::ACCESS_VIOLATION;
    }
    *next += 1;
    unsafe {
        core::ptr::write_bytes(out as *mut u8, 0, file::DIRECTORY_INFORMATION_SIZE);
        put_basic(out + 8, is_dir); // the four times, then (overwritten below) attributes
        core::ptr::write_unaligned((out + 40) as *mut u64, size); // EndOfFile
        core::ptr::write_unaligned((out + 48) as *mut u64, (size + 4095) & !4095); // AllocationSize
        core::ptr::write_unaligned((out + 56) as *mut u32, attributes(is_dir));
        core::ptr::write_unaligned((out + 60) as *mut u32, (units.len() * 2) as u32);
        core::ptr::copy_nonoverlapping(units.as_ptr(), (out + 64) as *mut u16, units.len());
    }
    write_iosb(iosb, status::SUCCESS, need as u64);
    status::SUCCESS
}

/// NtQueryAttributesFile(ObjectAttributes*, FILE_BASIC_INFORMATION*).
pub fn query_attributes(oa: u64, out: u64) -> NtStatus {
    let Some(name) = read_object_name(oa) else { return status::ACCESS_VIOLATION };
    let Some(path) = vfs_path(&name) else { return status::OBJECT_NAME_INVALID };
    let Some((is_dir, _)) = stat(&path) else {
        return if parent_exists(&path) { status::OBJECT_NAME_NOT_FOUND } else { status::OBJECT_PATH_NOT_FOUND };
    };
    if !ptr_ok(out, 40) {
        return status::ACCESS_VIOLATION;
    }
    put_basic(out, is_dir);
    status::SUCCESS
}

/// NtDeleteFile(ObjectAttributes*): a file, or an empty directory.
pub fn delete(oa: u64) -> NtStatus {
    let Some(name) = read_object_name(oa) else { return status::ACCESS_VIOLATION };
    let Some(path) = vfs_path(&name) else { return status::OBJECT_NAME_INVALID };
    match stat(&path) {
        None => status::OBJECT_NAME_NOT_FOUND,
        Some((true, _)) if !VFS.list_dir(&path).unwrap_or_default().is_empty() => status::DIRECTORY_NOT_EMPTY,
        Some(_) => match VFS.delete_file(&path) {
            Ok(()) => status::SUCCESS,
            Err(_) => status::ACCESS_DENIED,
        },
    }
}

pub fn flush(f: &FileObject, iosb: u64) -> NtStatus {
    let s = f.flush();
    write_iosb(iosb, s, 0);
    s
}
