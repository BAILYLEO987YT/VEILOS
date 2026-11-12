// =============================================================================
// VEILOS: files for Windows programs (WIN32_DESIGN.md, W4)
// =============================================================================
//
// NtCreateFile and friends over the VFS. Paths arrive as NT paths
// ("\??\C:\dir\file.txt", made by kernel32 from DOS paths): any drive letter
// is the VFS root, backslashes become slashes.
//
// Each file open anywhere has one `SharedFile` (found by path in OPEN_FILES),
// so every handle to it sees the others' writes and its deletion waits for the
// last of them. Reads and writes go to the VFS at the handle's position; they
// don't pass through a copy of the file.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use nt_abi::{file, status, IoStatusBlock, NtStatus, ObjectAttributes, UnicodeString};
use spin::Mutex;

use crate::vfs::{FileType, VfsFile, VFS};

/// A file with at least one handle open, or mapped as an image.
pub struct SharedFile {
    file: VfsFile,
    /// VFS path; follows a rename.
    path: Mutex<String>,
    delete_pending: AtomicBool,
    /// Images running from it: it can't be deleted or emptied meanwhile.
    images: AtomicUsize,
}

impl SharedFile {
    pub fn read_at(&self, off: u64, buf: &mut [u8]) -> Result<usize, crate::vfs::VfsError> {
        self.file.read_at(off, buf)
    }
    pub fn len(&self) -> u64 {
        self.file.len()
    }
}

/// The file is running as an image for as long as this lives.
pub struct ImageHold(Arc<SharedFile>);

impl ImageHold {
    pub fn new(file: Arc<SharedFile>) -> Self {
        file.images.fetch_add(1, Ordering::AcqRel);
        Self(file)
    }

    pub fn file(&self) -> &SharedFile {
        &self.0
    }
}

impl Drop for ImageHold {
    fn drop(&mut self) {
        self.0.images.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Open `path` to map it.
pub fn open_mapped(path: &str) -> Result<Arc<SharedFile>, crate::vfs::VfsError> {
    open_shared(path)
}

/// Open files by lower-case VFS path.
static OPEN_FILES: Mutex<BTreeMap<String, Weak<SharedFile>>> = Mutex::new(BTreeMap::new());

fn open_key(path: &str) -> String {
    path.to_ascii_lowercase()
}

fn find_open(path: &str) -> Option<Arc<SharedFile>> {
    OPEN_FILES.lock().get(&open_key(path)).and_then(Weak::upgrade)
}

fn open_shared(path: &str) -> Result<Arc<SharedFile>, crate::vfs::VfsError> {
    if let Some(s) = find_open(path) {
        return Ok(s);
    }
    let shared = Arc::new(SharedFile {
        file: VFS.open(path)?,
        path: Mutex::new(String::from(path)),
        delete_pending: AtomicBool::new(false),
        images: AtomicUsize::new(0),
    });
    OPEN_FILES.lock().insert(open_key(path), Arc::downgrade(&shared));
    Ok(shared)
}

impl Drop for SharedFile {
    fn drop(&mut self) {
        let path = self.path.lock().clone();
        OPEN_FILES.lock().remove(&open_key(&path));
        if self.delete_pending.load(Ordering::Acquire) {
            let _ = VFS.delete_file(&path);
        } else if self.file.sync().is_err() {
            crate::serial_println!("[FILE] {}: could not write back", path);
        }
    }
}

const STATUS_NOT_SAME_DEVICE: NtStatus = 0xC000_00D4u32 as NtStatus;

pub struct FileObject {
    /// VFS path ("/dir/file.txt"), as opened.
    path: Mutex<String>,
    is_dir: bool,
    access: u32,
    /// None for a directory.
    shared: Option<Arc<SharedFile>>,
    position: Mutex<u64>,
    /// FILE_DELETE_ON_CLOSE, or a directory's disposition.
    delete_on_close: AtomicBool,
    /// Directory listing for NtQueryDirectoryFile: entries and next index.
    listing: Mutex<Option<(Vec<ListEntry>, usize)>>,
}

#[derive(Clone)]
struct ListEntry {
    name: String,
    size: u64,
    is_dir: bool,
    mtime: i64,
}

impl FileObject {
    fn flush(&self) -> NtStatus {
        match &self.shared {
            Some(s) if s.file.sync().is_err() => status::ACCESS_DENIED,
            _ => status::SUCCESS,
        }
    }

    fn file(&self) -> Option<&SharedFile> {
        self.shared.as_deref()
    }

    /// Where it is now.
    pub fn current_path(&self) -> String {
        match &self.shared {
            Some(s) => s.path.lock().clone(),
            None => self.path.lock().clone(),
        }
    }

    /// The open file, for mapping.
    pub fn shared(&self) -> Option<Arc<SharedFile>> {
        self.shared.clone()
    }
}

impl Drop for FileObject {
    fn drop(&mut self) {
        if !self.delete_on_close.load(Ordering::Acquire) {
            return;
        }
        match &self.shared {
            Some(s) => s.delete_pending.store(true, Ordering::Release),
            None => {
                let _ = VFS.delete_file(&self.path.lock());
            }
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
pub fn read_object_name(oa: u64) -> Option<String> {
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
    // Characters no Windows name has: ERROR_INVALID_NAME, which cmd relies on
    // to tell a quoted command from a file's name.
    if p[2..].chars().any(|c| matches!(c, '"' | '<' | '>' | '|' | '*' | '?') || (c as u32) < 0x20) {
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

/// `name` (backslashes, no drive) inside the directory at `dir`.
fn relative_path(dir: &str, name: &str) -> String {
    let mut out = String::from(dir.trim_end_matches('/'));
    for part in name.split(|c| c == '\\' || c == '/').filter(|s| !s.is_empty() && *s != ".") {
        out.push('/');
        out.push_str(part);
    }
    if out.is_empty() {
        out.push('/');
    }
    out
}

/// What's at `path`: (is a directory, size). An open file's size is the live one.
fn stat(path: &str) -> Option<(bool, u64)> {
    if path == "/" {
        return Some((true, 0));
    }
    if let Some(s) = find_open(path) {
        return (!s.delete_pending.load(Ordering::Acquire)).then(|| (false, s.file.len()));
    }
    VFS.stat(path).map(|s| (s.is_dir, s.size))
}

/// The last-write time at `path`; an unknown time reads as now.
fn mtime_of(path: &str) -> i64 {
    let t = match find_open(path) {
        Some(s) => s.file.mtime(),
        None => VFS.stat(path).map_or(0, |s| s.mtime),
    };
    if t != 0 { t } else { now_filetime() }
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
    // OBJECT_ATTRIBUTES.RootDirectory: the name is relative to that directory.
    let root = unsafe { (*(oa as *const ObjectAttributes)).root_directory } as u64;
    let path = if root != 0 {
        let Some(crate::nt_objects::KernelObjectRef::File(dir)) = crate::nt_process::object(proc, root) else { return status::INVALID_HANDLE };
        relative_path(&dir.current_path(), &name)
    } else {
        match vfs_path(&name) {
            Some(p) => p,
            None => return status::OBJECT_NAME_INVALID,
        }
    };
    let existing = stat(&path);
    let want_dir = options & file::DIRECTORY_FILE != 0;

    // Security Mode (security.rs): reading needs fs.read; writing, creating,
    // replacing or truncating fs.write; deleting fs.modify.
    {
        use crate::capability::Capability as C;
        let creates = matches!(disposition, file::SUPERSEDE | file::CREATE | file::OVERWRITE | file::OVERWRITE_IF)
            || (disposition == file::OPEN_IF && existing.is_none());
        // Write data or attributes, append, GENERIC_WRITE, GENERIC_ALL.
        let writes = access & (file::WRITE_DATA | file::APPEND_DATA | 0x0000_0110 | file::GENERIC_WRITE | 0x1000_0000) != 0;
        let deletes = access & file::DELETE != 0 || options & file::DELETE_ON_CLOSE != 0;
        let checks = [
            (true, C::FsRead, "reading files"),
            (creates || writes, C::FsWrite, "writing files"),
            (deletes, C::FsModify, "deleting files"),
        ];
        for (needed, cap, what) in checks {
            if needed {
                if let Err(s) = crate::security::require(proc, cap, what) {
                    return s;
                }
            }
        }
    }

    // A deleted file's name is gone at once (as on Windows 11), though its
    // handles go on working. It can't be made again until they've closed.
    if existing.is_none() && find_open(&path).is_some() && disposition != file::OPEN && disposition != file::OVERWRITE {
        return status::ACCESS_DENIED;
    }
    let open = |p: &str| open_shared(p).map_err(|_| status::ACCESS_DENIED);
    let (is_dir, shared, info) = match (existing, disposition) {
        (Some(_), file::CREATE) => return status::OBJECT_NAME_COLLISION,
        (None, file::OPEN | file::OVERWRITE) => {
            return if parent_exists(&path) { status::OBJECT_NAME_NOT_FOUND } else { status::OBJECT_PATH_NOT_FOUND };
        }
        (Some((true, _)), _) if options & file::NON_DIRECTORY_FILE != 0 => return status::FILE_IS_A_DIRECTORY,
        (Some((false, _)), _) if want_dir => return status::NOT_A_DIRECTORY,
        (Some((true, _)), _) => (true, None, file::OPENED),
        (Some((false, _)), file::OPEN | file::OPEN_IF) => match open(&path) {
            Ok(s) => (false, Some(s), file::OPENED),
            Err(e) => return e,
        },
        (Some((false, _)), _) => {
            // SUPERSEDE / OVERWRITE / OVERWRITE_IF: start empty.
            let s = match open(&path) {
                Ok(s) => s,
                Err(e) => return e,
            };
            if s.images.load(Ordering::Acquire) != 0 || s.file.set_len(0).is_err() {
                return status::ACCESS_DENIED;
            }
            (false, Some(s), if disposition == file::SUPERSEDE { file::SUPERSEDED } else { file::OVERWRITTEN })
        }
        (None, _) => {
            if !parent_exists(&path) {
                return status::OBJECT_PATH_NOT_FOUND;
            }
            let made = if want_dir { VFS.create_dir(&path) } else { VFS.write_file(&path, &[]) };
            if made.is_err() {
                return status::ACCESS_DENIED;
            }
            let shared = if want_dir {
                None
            } else {
                match open(&path) {
                    Ok(s) => Some(s),
                    Err(e) => return e,
                }
            };
            (want_dir, shared, file::CREATED)
        }
    };
    let obj = Arc::new(FileObject {
        path: Mutex::new(path),
        is_dir,
        access,
        shared,
        position: Mutex::new(0),
        delete_on_close: AtomicBool::new(options & file::DELETE_ON_CLOSE != 0),
        listing: Mutex::new(None),
    });
    let inherit = unsafe { (*(oa as *const ObjectAttributes)).attributes } & 2 != 0; // OBJ_INHERIT
    let kept = if inherit { 0 } else { crate::nt_objects::HANDLE_NO_INHERIT };
    let h = proc.handles.lock().allocate_ref(crate::nt_objects::KernelObjectRef::File(obj), access, kept);
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
    let Some(shared) = f.file() else { return status::FILE_IS_A_DIRECTORY };
    let size = shared.file.len();
    let start = start_offset(f, offset_ptr, size);
    if start >= size && length > 0 {
        write_iosb(iosb, status::END_OF_FILE, 0);
        return status::END_OF_FILE;
    }
    let buf = unsafe { core::slice::from_raw_parts_mut(buffer as *mut u8, length) };
    let Ok(n) = shared.file.read_at(start, buf) else {
        write_iosb(iosb, status::ACCESS_DENIED, 0);
        return status::ACCESS_DENIED;
    };
    *f.position.lock() = start + n as u64;
    write_iosb(iosb, status::SUCCESS, n as u64);
    status::SUCCESS
}

pub fn write(f: &FileObject, iosb: u64, buffer: u64, length: usize, offset_ptr: u64) -> NtStatus {
    let Some(shared) = f.file() else { return status::FILE_IS_A_DIRECTORY };
    if !can_write(f) {
        return status::ACCESS_DENIED;
    }
    let start = start_offset(f, offset_ptr, shared.file.len());
    let data = unsafe { core::slice::from_raw_parts(buffer as *const u8, length) };
    if shared.file.write_at(start, data).is_err() {
        write_iosb(iosb, status::DISK_FULL, 0);
        return status::DISK_FULL;
    }
    *f.position.lock() = start + length as u64;
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

/// FILE_BASIC_INFORMATION (40 bytes) at `out`. One time is kept per file.
fn put_basic(out: u64, is_dir: bool, t: i64) {
    unsafe {
        for i in 0..4 {
            core::ptr::write_unaligned((out + i * 8) as *mut i64, t);
        }
        core::ptr::write_unaligned((out + 32) as *mut u32, attributes(is_dir));
        core::ptr::write_unaligned((out + 36) as *mut u32, 0);
    }
}

/// An access mask as Windows keeps it: generic rights mapped to the file
/// rights they stand for (MSYS2 checks FILE_WRITE_DATA on its std handles).
fn specific_access(access: u32) -> u32 {
    let mut a = access & 0x00FF_FFFF;
    if access & 0x8000_0000 != 0 { a |= 0x0012_0089; } // GENERIC_READ
    if access & 0x4000_0000 != 0 { a |= 0x0012_0116; } // GENERIC_WRITE
    if access & 0x2000_0000 != 0 { a |= 0x0012_00A0; } // GENERIC_EXECUTE
    if access & 0x1200_0000 != 0 { a |= 0x001F_01FF; } // GENERIC_ALL, MAXIMUM_ALLOWED
    a
}

/// NtQueryInformationFile(FileHandle, IoStatusBlock*, FileInformation*, Length, Class).
/// Basic, standard, internal, EA, access, name, position, mode, alignment,
/// all of those at once (FileAllInformation), network-open and
/// attribute-tag information.
pub fn query_information(f: &FileObject, iosb: u64, out: u64, length: usize, class: u32) -> NtStatus {
    const EA: u32 = 7;
    const ACCESS: u32 = 8;
    const MODE: u32 = 16;
    const ALIGNMENT: u32 = 17;
    const ALL: u32 = 18;
    const NETWORK_OPEN: u32 = 34;
    const ATTRIBUTE_TAG: u32 = 35;
    const STATUS_BUFFER_OVERFLOW: NtStatus = 0x8000_0005u32 as NtStatus;

    let size = f.file().map_or(0, |s| s.file.len());
    let mtime = match f.file() {
        Some(s) if s.file.mtime() != 0 => s.file.mtime(),
        Some(_) => now_filetime(),
        None => mtime_of(&f.path.lock()),
    };
    let pending = f.delete_on_close.load(Ordering::Relaxed) || f.file().is_some_and(|s| s.delete_pending.load(Ordering::Relaxed));
    // A number for the file: its path, which is what two opens share.
    let id = {
        let mut h = 0xCBF2_9CE4_8422_2325u64;
        for b in open_key(&f.current_path()).bytes() {
            h = (h ^ b as u64).wrapping_mul(0x0100_0000_01B3);
        }
        h
    };
    let mut basic = [0u8; 40];
    put_basic(basic.as_mut_ptr() as u64, f.is_dir, mtime);
    let mut standard = [0u8; 24];
    standard[0..8].copy_from_slice(&((size + 4095) & !4095).to_le_bytes()); // AllocationSize
    standard[8..16].copy_from_slice(&size.to_le_bytes()); // EndOfFile
    standard[16..20].copy_from_slice(&1u32.to_le_bytes()); // NumberOfLinks
    standard[20] = pending as u8;
    standard[21] = f.is_dir as u8;
    let position = (*f.position.lock()).to_le_bytes();

    let mut info: Vec<u8> = Vec::new();
    let mut with_name = false;
    match class {
        file::BASIC_INFORMATION => info.extend_from_slice(&basic),
        file::STANDARD_INFORMATION => info.extend_from_slice(&standard),
        file::INTERNAL_INFORMATION => info.extend_from_slice(&id.to_le_bytes()),
        EA | MODE | ALIGNMENT => info.extend_from_slice(&0u32.to_le_bytes()),
        ACCESS => info.extend_from_slice(&specific_access(f.access).to_le_bytes()),
        file::POSITION_INFORMATION => info.extend_from_slice(&position),
        file::NAME_INFORMATION => with_name = true,
        ALL => {
            info.extend_from_slice(&basic);
            info.extend_from_slice(&standard);
            info.extend_from_slice(&id.to_le_bytes());
            info.extend_from_slice(&0u32.to_le_bytes()); // EaSize
            info.extend_from_slice(&specific_access(f.access).to_le_bytes());
            info.extend_from_slice(&position);
            info.extend_from_slice(&0u32.to_le_bytes()); // Mode
            info.extend_from_slice(&0u32.to_le_bytes()); // AlignmentRequirement
            with_name = true;
        }
        NETWORK_OPEN => {
            info.extend_from_slice(&basic[..32]); // the four times
            info.extend_from_slice(&standard[..16]); // AllocationSize, EndOfFile
            info.extend_from_slice(&basic[32..36]); // FileAttributes
            info.extend_from_slice(&0u32.to_le_bytes());
        }
        ATTRIBUTE_TAG => {
            info.extend_from_slice(&basic[32..36]);
            info.extend_from_slice(&0u32.to_le_bytes()); // ReparseTag
        }
        _ => return status::INVALID_INFO_CLASS,
    }
    let mut st = status::SUCCESS;
    if with_name {
        // FileNameLength, then the name (as much as fits: STATUS_BUFFER_OVERFLOW).
        let path = f.current_path();
        let units: Vec<u16> = path.chars().map(|c| if c == '/' { '\\' } else { c }).collect::<String>().encode_utf16().collect();
        info.extend_from_slice(&((units.len() * 2) as u32).to_le_bytes());
        let fixed = info.len();
        if length < fixed {
            return status::BUFFER_TOO_SMALL;
        }
        for u in &units {
            info.extend_from_slice(&u.to_le_bytes());
        }
        if info.len() > length {
            info.truncate(length);
            st = STATUS_BUFFER_OVERFLOW;
        }
    } else if length < info.len() {
        return status::BUFFER_TOO_SMALL;
    }
    if !ptr_ok(out, info.len()) {
        return status::ACCESS_VIOLATION;
    }
    unsafe { core::ptr::copy_nonoverlapping(info.as_ptr(), out as *mut u8, info.len()) };
    write_iosb(iosb, st, info.len() as u64);
    st
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
            let Some(shared) = f.file() else { return status::INVALID_PARAMETER };
            if shared.file.set_len(unsafe { *(info as *const u64) }).is_err() {
                return status::DISK_FULL;
            }
        }
        file::DISPOSITION_INFORMATION => {
            let on = unsafe { *(info as *const u8) } != 0;
            match f.file() {
                Some(s) if on && s.images.load(Ordering::Acquire) != 0 => return status::ACCESS_DENIED,
                Some(s) => s.delete_pending.store(on, Ordering::Release),
                None => {
                    let path = f.path.lock();
                    if on && !VFS.list_dir(&path).unwrap_or_default().is_empty() {
                        return status::DIRECTORY_NOT_EMPTY;
                    }
                    f.delete_on_close.store(on, Ordering::Release);
                }
            }
        }
        file::RENAME_INFORMATION if length >= 20 => {
            let s = rename(f, info, length);
            if s != status::SUCCESS {
                return s;
            }
        }
        file::BASIC_INFORMATION if length >= 40 => {
            // Times of 0 and -1 leave a time as it is. Attributes aren't kept.
            let t = unsafe { core::ptr::read_unaligned((info + 16) as *const i64) };
            if t > 0 {
                if let Some(s) = f.file() {
                    let _ = s.file.set_mtime(t);
                }
            }
        }
        // Preallocation needs nothing.
        file::BASIC_INFORMATION | file::ALLOCATION_INFORMATION => {}
        _ => return status::INVALID_INFO_CLASS,
    }
    write_iosb(iosb, status::SUCCESS, 0);
    status::SUCCESS
}

/// FILE_RENAME_INFORMATION: ReplaceIfExists, RootDirectory, FileNameLength,
/// FileName. The name is a whole NT path.
fn rename(f: &FileObject, info: u64, length: usize) -> NtStatus {
    let replace = unsafe { *(info as *const u8) } != 0;
    let bytes = unsafe { core::ptr::read_unaligned((info + 16) as *const u32) } as usize;
    if bytes % 2 != 0 || 20 + bytes > length {
        return status::INVALID_PARAMETER;
    }
    let units: Vec<u16> = (0..bytes / 2).map(|i| unsafe { core::ptr::read_unaligned((info + 20 + i as u64 * 2) as *const u16) }).collect();
    let name: String = char::decode_utf16(units).map(|c| c.unwrap_or('?')).collect();
    let Some(to) = vfs_path(&name) else { return status::OBJECT_NAME_INVALID };
    let from = match f.file() {
        Some(s) => s.path.lock().clone(),
        None => f.path.lock().clone(),
    };
    let same = from.eq_ignore_ascii_case(&to);
    if !same {
        if !parent_exists(&to) {
            return status::OBJECT_PATH_NOT_FOUND;
        }
        match stat(&to) {
            None => {}
            Some(_) if !replace => return status::OBJECT_NAME_COLLISION,
            Some((true, _)) => return status::ACCESS_DENIED,
            Some(_) if find_open(&to).is_some() => return status::ACCESS_DENIED,
            Some(_) => {
                if VFS.delete_file(&to).is_err() {
                    return status::ACCESS_DENIED;
                }
            }
        }
    }
    if let Some(s) = f.file() {
        let _ = s.file.sync();
    }
    // Windows reports a move between volumes this way; callers then copy.
    match VFS.rename(&from, &to) {
        Ok(()) => {}
        Err(crate::vfs::VfsError::GenericError) => return STATUS_NOT_SAME_DEVICE,
        Err(_) => return status::ACCESS_DENIED,
    }
    // Whatever is open at or under the old path follows it.
    let prefix = alloc::format!("{}/", open_key(&from));
    let mut table = OPEN_FILES.lock();
    let moved: Vec<String> = table.keys().filter(|k| **k == open_key(&from) || k.starts_with(&prefix)).cloned().collect();
    for key in moved {
        let Some(weak) = table.remove(&key) else { continue };
        let Some(shared) = weak.upgrade() else { continue };
        let new = alloc::format!("{}{}", to, &key[from.len()..]);
        // Files under a moved directory keep their entries.
        if key.len() == from.len() {
            let _ = shared.file.moved(&new);
        }
        *shared.path.lock() = new.clone();
        table.insert(open_key(&new), weak);
    }
    drop(table);
    *f.path.lock() = to;
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
    // All start the same; the name's place is what differs. (The rest of what
    // the longer ones add stays zero: no EAs, no short names, no file ids.)
    let name_at = match class {
        file::DIRECTORY_INFORMATION => file::DIRECTORY_INFORMATION_SIZE,
        file::FULL_DIRECTORY_INFORMATION => 68,
        file::BOTH_DIRECTORY_INFORMATION => 94,
        file::ID_FULL_DIRECTORY_INFORMATION => 80,
        file::ID_BOTH_DIRECTORY_INFORMATION => 104,
        _ => return status::INVALID_INFO_CLASS,
    };
    let mut listing = f.listing.lock();
    if listing.is_none() || restart {
        let mask: Vec<char> = if mask_ptr != 0 { read_unicode(mask_ptr).unwrap_or_default().chars().collect() } else { Vec::new() };
        let mask = if mask.is_empty() { alloc::vec!['*'] } else { mask };
        let path = f.path.lock().clone();
        let now = now_filetime();
        let mut entries: Vec<ListEntry> = Vec::new();
        if path != "/" {
            for dot in [".", ".."] {
                entries.push(ListEntry { name: String::from(dot), size: 0, is_dir: true, mtime: now });
            }
        }
        for e in VFS.list_dir(&path).unwrap_or_default() {
            let is_dir = e.file_type == FileType::Directory;
            entries.push(ListEntry { name: e.name, size: e.size, is_dir, mtime: if e.mtime != 0 { e.mtime } else { now } });
        }
        entries.retain(|e| wildcard(&mask, &e.name.chars().collect::<Vec<_>>()));
        *listing = Some((entries, 0));
    }
    let (entries, next) = listing.as_mut().unwrap();
    let Some(ListEntry { name, size, is_dir, mtime }) = entries.get(*next).cloned() else {
        write_iosb(iosb, status::NO_MORE_FILES, 0);
        return status::NO_MORE_FILES;
    };
    let units: Vec<u16> = name.encode_utf16().collect();
    let need = name_at + units.len() * 2;
    if length < need {
        return status::BUFFER_TOO_SMALL;
    }
    if !ptr_ok(out, need) {
        return status::ACCESS_VIOLATION;
    }
    *next += 1;
    unsafe {
        core::ptr::write_bytes(out as *mut u8, 0, name_at);
        put_basic(out + 8, is_dir, mtime); // the four times, then (overwritten below) attributes
        core::ptr::write_unaligned((out + 40) as *mut u64, size); // EndOfFile
        core::ptr::write_unaligned((out + 48) as *mut u64, (size + 4095) & !4095); // AllocationSize
        core::ptr::write_unaligned((out + 56) as *mut u32, attributes(is_dir));
        core::ptr::write_unaligned((out + 60) as *mut u32, (units.len() * 2) as u32);
        core::ptr::copy_nonoverlapping(units.as_ptr(), (out as usize + name_at) as *mut u16, units.len());
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
    put_basic(out, is_dir, mtime_of(&path));
    status::SUCCESS
}

/// NtDeleteFile(ObjectAttributes*): a file, or an empty directory.
pub fn delete(oa: u64) -> NtStatus {
    let Some(name) = read_object_name(oa) else { return status::ACCESS_VIOLATION };
    let Some(path) = vfs_path(&name) else { return status::OBJECT_NAME_INVALID };
    // Open somewhere: it goes when the last handle closes.
    if let Some(s) = find_open(&path) {
        if s.images.load(Ordering::Acquire) != 0 {
            return status::ACCESS_DENIED;
        }
        s.delete_pending.store(true, Ordering::Release);
        return status::SUCCESS;
    }
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
    // FlushFileBuffers means on the disk, write-back cache or not.
    crate::block::flush_all();
    write_iosb(iosb, s, 0);
    s
}
