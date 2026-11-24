// The Zw* calls drivers make that aren't about the registry (cm.rs): files
// on VEILOS's VFS, devices, events, symbolic links, registry enumeration,
// system information, driver loading, and the ones VEILOS doesn't back yet.
//
// Paths drivers open: \??\C:\x, \DosDevices\C:\x and \GLOBAL??\C:\x (the
// VFS: drive letters are the root), \SystemRoot\x (/system/x), or a
// device's name (\Device\X, or a link to one), which opens the device.
//
// The calls VEILOS can't do yet (process virtual memory, sections, process
// and token queries, power information) return STATUS_NOT_SUPPORTED and
// say so in the log the first time each is called, so a driver that
// depends on one shows where it stopped.

use alloc::{collections::BTreeMap, format, string::String, sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicU64, Ordering};
use spin::Mutex;

use super::{cm, image, io, ke, ob, pool, rtl, NtStatus, STATUS_INVALID_PARAMETER, STATUS_NO_MEMORY, STATUS_OBJECT_NAME_NOT_FOUND, STATUS_SUCCESS};

const STATUS_NOT_SUPPORTED: NtStatus = 0xC000_00BBu32 as i32;
const STATUS_INVALID_HANDLE: NtStatus = 0xC000_0008u32 as i32;
const STATUS_OBJECT_TYPE_MISMATCH: NtStatus = 0xC000_0024u32 as i32;
const STATUS_INVALID_INFO_CLASS: NtStatus = 0xC000_0003u32 as i32;
const STATUS_END_OF_FILE: NtStatus = 0xC000_0011u32 as i32;
const STATUS_BUFFER_TOO_SMALL: NtStatus = 0xC000_0023u32 as i32;
const STATUS_NO_MORE_ENTRIES: NtStatus = 0x8000_001Au32 as i32;
const STATUS_ACCESS_DENIED: NtStatus = 0xC000_0022u32 as i32;
const STATUS_OBJECT_NAME_COLLISION: NtStatus = 0xC000_0035u32 as i32;

unsafe fn rd<T: Copy>(p: *const u8, off: usize) -> T {
    unsafe { (p.add(off) as *const T).read_unaligned() }
}
unsafe fn wr<T>(p: *mut u8, off: usize, v: T) {
    unsafe { (p.add(off) as *mut T).write_unaligned(v) }
}

fn iosb(p: *mut u8, status: NtStatus, info: u64) {
    if !p.is_null() {
        unsafe {
            wr::<u64>(p, 0, status as u32 as u64);
            wr::<u64>(p, 8, info);
        }
    }
}

/// OBJECT_ATTRIBUTES.ObjectName as text.
fn oa_name(oa: *const u8) -> String {
    if oa.is_null() {
        return String::new();
    }
    unsafe { rtl::unicode_to_string(rd::<*const u8>(oa, 0x10)) }
}

/// Say once that a driver called something VEILOS doesn't do yet.
fn unsupported(name: &str) -> NtStatus {
    static SEEN: Mutex<Vec<String>> = Mutex::new(Vec::new());
    let first = x86_64::instructions::interrupts::without_interrupts(|| {
        let mut s = SEEN.lock();
        if s.iter().any(|n| n == name) {
            false
        } else {
            s.push(String::from(name));
            true
        }
    });
    if first {
        crate::serial_println!("[NTOS] {}: not supported yet (STATUS_NOT_SUPPORTED)", name);
    }
    STATUS_NOT_SUPPORTED
}

// -----| files |-----

/// A driver's open file: the VFS file and its position.
pub struct KFile {
    pub path: String,
    file: crate::fs::vfs::VfsFile,
    pos: Mutex<u64>,
}

/// A driver's NT path as a VFS path, or None (not a file path).
pub fn kernel_vfs_path(nt: &str) -> Option<String> {
    let lower = nt.to_ascii_lowercase();
    if let Some(rest) = lower.strip_prefix(r"\systemroot") {
        let rest = &nt[nt.len() - rest.len()..];
        return Some(format!("/system{}", rest.replace('\\', "/")));
    }
    for p in [r"\dosdevices\", r"\global??\"] {
        if let Some(rest) = lower.strip_prefix(p) {
            return crate::nt::file::vfs_path(&format!(r"\??\{}", &nt[nt.len() - rest.len()..]));
        }
    }
    crate::nt::file::vfs_path(nt)
}

const FILE_SUPERSEDE: u32 = 0;
const FILE_OPEN: u32 = 1;
const FILE_CREATE: u32 = 2;
const FILE_OPEN_IF: u32 = 3;
const FILE_OVERWRITE: u32 = 4;
const FILE_OVERWRITE_IF: u32 = 5;
const FILE_DIRECTORY_FILE: u32 = 1;

fn open_path(name: &str, disposition: u32, options: u32) -> Result<(ob::Object, u64), NtStatus> {
    if let Some(r) = io::open_device(name) {
        return r.map(|d| (ob::Object::Device(Arc::new(d)), 1));
    }
    let path = kernel_vfs_path(name).ok_or(STATUS_OBJECT_NAME_NOT_FOUND)?;
    let exists = crate::fs::vfs::VFS.stat(&path);
    if options & FILE_DIRECTORY_FILE != 0 {
        return match (exists, disposition) {
            (Some(s), _) if s.is_dir => Ok((ob::Object::Directory(path), 1)),
            (None, FILE_CREATE | FILE_OPEN_IF) => {
                crate::fs::vfs::VFS.create_dir(&path).map_err(|_| STATUS_ACCESS_DENIED)?;
                Ok((ob::Object::Directory(path), 2))
            }
            _ => Err(STATUS_OBJECT_NAME_NOT_FOUND),
        };
    }
    // FILE_SUPERSEDED 0, OPENED 1, CREATED 2, OVERWRITTEN 3.
    let info = match (exists.is_some(), disposition) {
        (true, FILE_CREATE) => return Err(STATUS_OBJECT_NAME_COLLISION),
        (false, FILE_OPEN | FILE_OVERWRITE) => return Err(STATUS_OBJECT_NAME_NOT_FOUND),
        (true, FILE_OPEN | FILE_OPEN_IF) => 1,
        (true, FILE_OVERWRITE | FILE_OVERWRITE_IF) => {
            crate::fs::vfs::VFS.write_file(&path, &[]).map_err(|_| STATUS_ACCESS_DENIED)?;
            3
        }
        (true, FILE_SUPERSEDE) => {
            crate::fs::vfs::VFS.write_file(&path, &[]).map_err(|_| STATUS_ACCESS_DENIED)?;
            0
        }
        (false, _) => {
            crate::fs::vfs::VFS.write_file(&path, &[]).map_err(|_| STATUS_ACCESS_DENIED)?;
            2
        }
        _ => return Err(STATUS_INVALID_PARAMETER),
    };
    let file = crate::fs::vfs::VFS.open(&path).map_err(|_| STATUS_OBJECT_NAME_NOT_FOUND)?;
    Ok((ob::Object::File(Arc::new(KFile { path, file, pos: Mutex::new(0) })), info))
}

/// ZwCreateFile(FileHandle*, DesiredAccess, ObjectAttributes*,
/// IoStatusBlock*, AllocationSize*, FileAttributes, ShareAccess,
/// CreateDisposition, CreateOptions, EaBuffer, EaLength).
pub extern "win64" fn ZwCreateFile(
    out: *mut u64,
    _access: u32,
    oa: *const u8,
    io_status: *mut u8,
    _alloc: *const i64,
    _attrs: u32,
    _share: u32,
    disposition: u32,
    options: u32,
    _ea: *const u8,
    _ea_len: u32,
) -> NtStatus {
    match open_path(&oa_name(oa), disposition, options) {
        Ok((obj, info)) => {
            unsafe { out.write(ob::insert(obj)) };
            iosb(io_status, STATUS_SUCCESS, info);
            STATUS_SUCCESS
        }
        Err(s) => {
            iosb(io_status, s, 0);
            s
        }
    }
}

/// ZwOpenFile(FileHandle*, DesiredAccess, ObjectAttributes*,
/// IoStatusBlock*, ShareAccess, OpenOptions).
pub extern "win64" fn ZwOpenFile(out: *mut u64, access: u32, oa: *const u8, io_status: *mut u8, share: u32, options: u32) -> NtStatus {
    ZwCreateFile(out, access, oa, io_status, core::ptr::null(), 0, share, FILE_OPEN, options, core::ptr::null(), 0)
}

fn file_of(h: u64) -> Result<Arc<KFile>, NtStatus> {
    match ob::get(h) {
        Some(ob::Object::File(f)) => Ok(f),
        Some(_) => Err(STATUS_OBJECT_TYPE_MISMATCH),
        None => Err(STATUS_INVALID_HANDLE),
    }
}

/// ZwReadFile(FileHandle, Event, ApcRoutine, ApcContext, IoStatusBlock*,
/// Buffer, Length, ByteOffset*, Key): at ByteOffset, else the position.
pub extern "win64" fn ZwReadFile(h: u64, _ev: u64, _apc: u64, _ctx: u64, io_status: *mut u8, buf: *mut u8, len: u32, offset: *const i64, _key: *const u32) -> NtStatus {
    let f = match file_of(h) {
        Ok(f) => f,
        Err(s) => return s,
    };
    let mut pos = f.pos.lock();
    let at = if offset.is_null() || unsafe { offset.read() } < 0 { *pos } else { (unsafe { offset.read() }) as u64 };
    if at >= f.file.len() && len > 0 {
        iosb(io_status, STATUS_END_OF_FILE, 0);
        return STATUS_END_OF_FILE;
    }
    let slice = unsafe { core::slice::from_raw_parts_mut(buf, len as usize) };
    match f.file.read_at(at, slice) {
        Ok(n) => {
            *pos = at + n as u64;
            iosb(io_status, STATUS_SUCCESS, n as u64);
            STATUS_SUCCESS
        }
        Err(_) => {
            iosb(io_status, STATUS_ACCESS_DENIED, 0);
            STATUS_ACCESS_DENIED
        }
    }
}

pub extern "win64" fn ZwWriteFile(h: u64, _ev: u64, _apc: u64, _ctx: u64, io_status: *mut u8, buf: *const u8, len: u32, offset: *const i64, _key: *const u32) -> NtStatus {
    let f = match file_of(h) {
        Ok(f) => f,
        Err(s) => return s,
    };
    let mut pos = f.pos.lock();
    let at = if offset.is_null() || unsafe { offset.read() } < 0 { *pos } else { (unsafe { offset.read() }) as u64 };
    let data = unsafe { core::slice::from_raw_parts(buf, len as usize) };
    match f.file.write_at(at, data) {
        Ok(()) => {
            *pos = at + len as u64;
            iosb(io_status, STATUS_SUCCESS, len as u64);
            STATUS_SUCCESS
        }
        Err(_) => {
            iosb(io_status, STATUS_ACCESS_DENIED, 0);
            STATUS_ACCESS_DENIED
        }
    }
}

/// ZwQueryInformationFile(FileHandle, IoStatusBlock*, Info, Length,
/// Class): FileBasicInformation (4), FileStandardInformation (5),
/// FilePositionInformation (14).
pub extern "win64" fn ZwQueryInformationFile(h: u64, io_status: *mut u8, info: *mut u8, len: u32, class: u32) -> NtStatus {
    let f = match file_of(h) {
        Ok(f) => f,
        Err(s) => return s,
    };
    let size = f.file.len();
    let mut out = Vec::new();
    match class {
        4 => {
            let t = f.file.mtime();
            for _ in 0..4 {
                out.extend_from_slice(&t.to_le_bytes());
            }
            out.extend_from_slice(&0x80u32.to_le_bytes()); // FILE_ATTRIBUTE_NORMAL
            out.extend_from_slice(&0u32.to_le_bytes());
        }
        5 => {
            out.extend_from_slice(&size.next_multiple_of(4096).to_le_bytes());
            out.extend_from_slice(&size.to_le_bytes());
            out.extend_from_slice(&1u32.to_le_bytes());
            out.extend_from_slice(&[0, 0, 0, 0]);
        }
        14 => out.extend_from_slice(&f.pos.lock().to_le_bytes()),
        _ => return STATUS_INVALID_INFO_CLASS,
    }
    if (len as usize) < out.len() {
        return STATUS_BUFFER_TOO_SMALL;
    }
    unsafe { core::ptr::copy_nonoverlapping(out.as_ptr(), info, out.len()) };
    iosb(io_status, STATUS_SUCCESS, out.len() as u64);
    STATUS_SUCCESS
}

/// ZwDeviceIoControlFile(FileHandle, Event, ApcRoutine, ApcContext,
/// IoStatusBlock*, IoControlCode, InputBuffer, InputLength, OutputBuffer,
/// OutputLength): synchronous.
pub extern "win64" fn ZwDeviceIoControlFile(
    h: u64,
    _ev: u64,
    _apc: u64,
    _ctx: u64,
    io_status: *mut u8,
    code: u32,
    input: u64,
    in_len: u32,
    output: u64,
    out_len: u32,
) -> NtStatus {
    let Some(ob::Object::Device(d)) = ob::get(h) else { return STATUS_INVALID_HANDLE };
    let (st, info) = io::kernel_device_control(&d, code, input, in_len as usize, output, out_len as usize);
    iosb(io_status, st, info);
    st
}

// -----| events |-----

/// Named kernel events (\BaseNamedObjects\X, \KernelObjects\X, ...): the
/// event and how many handles are open to it. As in Windows the name goes
/// away with the last handle, so the next ZwCreateEvent makes a new event.
/// The event itself stays allocated: ObfDereferenceObject doesn't count
/// references yet, so a driver may still hold the pointer.
static NAMED: Mutex<BTreeMap<String, (usize, u32)>> = Mutex::new(BTreeMap::new());

/// Takes a reference for a new handle to the event called `name`, if it exists.
fn open_named(name: &str) -> Option<usize> {
    x86_64::instructions::interrupts::without_interrupts(|| {
        NAMED.lock().get_mut(name).map(|(e, n)| {
            *n += 1;
            *e
        })
    })
}

/// A handle to a dispatcher object was closed (ZwClose).
pub fn dispatcher_closed(p: usize) {
    x86_64::instructions::interrupts::without_interrupts(|| {
        let mut named = NAMED.lock();
        let Some(name) = named.iter().find(|(_, (e, _))| *e == p).map(|(k, _)| k.clone()) else { return };
        if let Some((_, n)) = named.get_mut(&name) {
            *n -= 1;
            if *n == 0 {
                named.remove(&name);
            }
        }
    })
}

/// ZwCreateEvent(EventHandle*, DesiredAccess, ObjectAttributes*,
/// EventType, InitialState): an event in pool; a named one can be opened
/// again (ZwOpenEvent).
pub extern "win64" fn ZwCreateEvent(out: *mut u64, _access: u32, oa: *const u8, kind: u32, initial: u8) -> NtStatus {
    let name = oa_name(oa).to_ascii_lowercase();
    let existing = if name.is_empty() { None } else { open_named(&name) };
    let ev = match existing {
        Some(e) => e,
        None => {
            let e = pool::alloc_tracked(0x18);
            if e.is_null() {
                return STATUS_NO_MEMORY;
            }
            ke::KeInitializeEvent(e, kind, initial);
            if !name.is_empty() {
                x86_64::instructions::interrupts::without_interrupts(|| NAMED.lock().insert(name, (e as usize, 1)));
            }
            e as usize
        }
    };
    unsafe { out.write(ob::insert(ob::Object::Dispatcher(ev))) };
    STATUS_SUCCESS
}

pub extern "win64" fn ZwOpenEvent(out: *mut u64, _access: u32, oa: *const u8) -> NtStatus {
    let name = oa_name(oa).to_ascii_lowercase();
    match open_named(&name) {
        Some(e) => {
            unsafe { out.write(ob::insert(ob::Object::Dispatcher(e))) };
            STATUS_SUCCESS
        }
        None => STATUS_OBJECT_NAME_NOT_FOUND,
    }
}

fn dispatcher(h: u64) -> Result<*mut u8, NtStatus> {
    match ob::get(h) {
        Some(ob::Object::Dispatcher(p)) => Ok(p as *mut u8),
        Some(_) => Err(STATUS_OBJECT_TYPE_MISMATCH),
        None => Err(STATUS_INVALID_HANDLE),
    }
}

pub extern "win64" fn ZwSetEvent(h: u64, prev: *mut i32) -> NtStatus {
    match dispatcher(h) {
        Ok(e) => {
            let p = ke::KeSetEvent(e, 0, 0);
            if !prev.is_null() {
                unsafe { prev.write(p) };
            }
            STATUS_SUCCESS
        }
        Err(s) => s,
    }
}

pub extern "win64" fn ZwResetEvent(h: u64, prev: *mut i32) -> NtStatus {
    match dispatcher(h) {
        Ok(e) => {
            let p = ke::KeResetEvent(e);
            if !prev.is_null() {
                unsafe { prev.write(p) };
            }
            STATUS_SUCCESS
        }
        Err(s) => s,
    }
}

pub extern "win64" fn ZwClearEvent(h: u64) -> NtStatus {
    ZwResetEvent(h, core::ptr::null_mut())
}

pub extern "win64" fn ZwWaitForSingleObject(h: u64, _alertable: u8, timeout: *const i64) -> NtStatus {
    match dispatcher(h) {
        Ok(o) => ke::KeWaitForSingleObject(o, 0, 0, 0, timeout),
        Err(s) => s,
    }
}

// -----| symbolic links, handles, LUIDs |-----

pub extern "win64" fn ZwOpenSymbolicLinkObject(out: *mut u64, _access: u32, oa: *const u8) -> NtStatus {
    match io::link_target(&oa_name(oa)) {
        Some(t) => {
            unsafe { out.write(ob::insert(ob::Object::Link(t))) };
            STATUS_SUCCESS
        }
        None => STATUS_OBJECT_NAME_NOT_FOUND,
    }
}

/// ZwQuerySymbolicLinkObject(LinkHandle, LinkTarget (UNICODE_STRING*),
/// ReturnedLength*): the target into the string's own buffer.
pub extern "win64" fn ZwQuerySymbolicLinkObject(h: u64, target: *mut u8, ret: *mut u32) -> NtStatus {
    let Some(ob::Object::Link(t)) = ob::get(h) else { return STATUS_INVALID_HANDLE };
    let units: Vec<u16> = t.encode_utf16().collect();
    let bytes = units.len() * 2;
    if !ret.is_null() {
        unsafe { ret.write(bytes as u32) };
    }
    unsafe {
        let max = rd::<u16>(target, 2) as usize;
        if bytes > max {
            return STATUS_BUFFER_TOO_SMALL;
        }
        let b: *mut u16 = rd(target, 8);
        core::ptr::copy_nonoverlapping(units.as_ptr(), b, units.len());
        wr::<u16>(target, 0, bytes as u16);
    }
    STATUS_SUCCESS
}

/// ZwDuplicateObject(SourceProcess, SourceHandle, TargetProcess,
/// TargetHandle*, DesiredAccess, HandleAttributes, Options): within the
/// kernel's handles (DUPLICATE_CLOSE_SOURCE: 1).
pub extern "win64" fn ZwDuplicateObject(_sp: u64, src: u64, _tp: u64, out: *mut u64, _access: u32, _attrs: u32, options: u32) -> NtStatus {
    let Some(o) = ob::get(src) else { return STATUS_INVALID_HANDLE };
    if !out.is_null() {
        unsafe { out.write(ob::insert(o)) };
    }
    if options & 1 != 0 {
        ob::ZwClose(src);
    }
    STATUS_SUCCESS
}

static NEXT_LUID: AtomicU64 = AtomicU64::new(0x1000);

pub extern "win64" fn ZwAllocateLocallyUniqueId(luid: *mut u64) -> NtStatus {
    unsafe { luid.write(NEXT_LUID.fetch_add(1, Ordering::Relaxed)) };
    STATUS_SUCCESS
}

// -----| registry enumeration |-----

/// ZwEnumerateValueKey(KeyHandle, Index, Class, Info, Length,
/// ResultLength*).
pub extern "win64" fn ZwEnumerateValueKey(h: u64, index: u32, class: u32, info: *mut u8, len: u32, result: *mut u32) -> NtStatus {
    let path = match cm::key_path(h) {
        Ok(p) => p,
        Err(s) => return s,
    };
    let values = crate::nt::registry::kernel_values(&path).unwrap_or_default();
    match values.get(index as usize) {
        Some((name, kind, data)) => cm::value_info(name, *kind, data, class, info, len, result),
        None => STATUS_NO_MORE_ENTRIES,
    }
}

/// ZwEnumerateKey(KeyHandle, Index, Class, Info, Length, ResultLength*):
/// KeyBasicInformation (0) only.
pub extern "win64" fn ZwEnumerateKey(h: u64, index: u32, class: u32, info: *mut u8, len: u32, result: *mut u32) -> NtStatus {
    let path = match cm::key_path(h) {
        Ok(p) => p,
        Err(s) => return s,
    };
    if class != 0 {
        return STATUS_INVALID_PARAMETER;
    }
    let subs = crate::nt::registry::kernel_subkeys(&path).unwrap_or_default();
    let Some(name) = subs.get(index as usize) else { return STATUS_NO_MORE_ENTRIES };
    cm::put_info(&key_basic(name), 16, info, len, result)
}

/// KEY_BASIC_INFORMATION { LastWriteTime, TitleIndex, NameLength, Name }.
fn key_basic(name: &str) -> Vec<u8> {
    let n: Vec<u8> = name.encode_utf16().flat_map(|c| c.to_le_bytes()).collect();
    let mut out = Vec::new();
    out.extend_from_slice(&0i64.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&(n.len() as u32).to_le_bytes());
    out.extend_from_slice(&n);
    out
}

/// ZwQueryKey(KeyHandle, Class, Info, Length, ResultLength*):
/// KeyBasicInformation (0) and KeyFullInformation (2).
pub extern "win64" fn ZwQueryKey(h: u64, class: u32, info: *mut u8, len: u32, result: *mut u32) -> NtStatus {
    let path = match cm::key_path(h) {
        Ok(p) => p,
        Err(s) => return s,
    };
    match class {
        0 => cm::put_info(&key_basic(path.rsplit('\\').next().unwrap_or("")), 16, info, len, result),
        2 => {
            let subs = crate::nt::registry::kernel_subkeys(&path).unwrap_or_default();
            let vals = crate::nt::registry::kernel_values(&path).unwrap_or_default();
            let mut out = Vec::new();
            let mut push = |x: u32| out.extend_from_slice(&x.to_le_bytes());
            push(0);
            push(0); // LastWriteTime
            push(0); // TitleIndex
            push(0x2C); // ClassOffset (no class)
            push(0); // ClassLength
            push(subs.len() as u32);
            push(subs.iter().map(|s| s.len() as u32 * 2).max().unwrap_or(0));
            push(0); // MaxClassLen
            push(vals.len() as u32);
            push(vals.iter().map(|v| v.0.len() as u32 * 2).max().unwrap_or(0));
            push(vals.iter().map(|v| v.2.len() as u32).max().unwrap_or(0));
            cm::put_info(&out, 0x2C, info, len, result)
        }
        _ => STATUS_INVALID_PARAMETER,
    }
}

// -----| system information |-----

/// ZwQuerySystemInformation(Class, Info, Length, ReturnLength*):
/// SystemBasicInformation (0) and SystemModuleInformation (11: the kernel
/// and the loaded drivers).
pub extern "win64" fn ZwQuerySystemInformation(class: u32, info: *mut u8, len: u32, result: *mut u32) -> NtStatus {
    let mut out = Vec::new();
    match class {
        0 => {
            let cpus = crate::arch::smp::CPU_ONLINE.load(Ordering::Relaxed);
            let pages = crate::mm::memory::total_frame_count() as u32;
            for v in [0u32, 10_000, 4096, pages, 1, pages, 65536, 0] {
                out.extend_from_slice(&v.to_le_bytes());
            }
            out.extend_from_slice(&0x10000u64.to_le_bytes());
            out.extend_from_slice(&0x7FFF_FFFE_FFFFu64.to_le_bytes());
            out.extend_from_slice(&cpus.to_le_bytes());
            out.push(cpus.count_ones() as u8);
            out.resize(0x40, 0);
        }
        11 => {
            let mut mods: Vec<(u64, u32, String)> = Vec::new();
            mods.push((0xFFFF_FFFF_8000_0000, 0x0100_0000, String::from(r"\SystemRoot\system32\ntoskrnl.exe")));
            for d in image::DRIVERS.lock().iter() {
                mods.push((d.base, d.size as u32, format!(r"\SystemRoot\system32\drivers\{}.sys", d.name)));
            }
            out.extend_from_slice(&(mods.len() as u32).to_le_bytes());
            out.resize(8, 0);
            for (i, (base, size, path)) in mods.iter().enumerate() {
                let mut m = [0u8; 0x128];
                m[0x10..0x18].copy_from_slice(&base.to_le_bytes());
                m[0x18..0x1C].copy_from_slice(&size.to_le_bytes());
                m[0x20..0x22].copy_from_slice(&(i as u16).to_le_bytes());
                m[0x22..0x24].copy_from_slice(&(i as u16).to_le_bytes());
                m[0x24..0x26].copy_from_slice(&1u16.to_le_bytes());
                let file_at = path.rfind('\\').map_or(0, |p| p + 1) as u16;
                m[0x26..0x28].copy_from_slice(&file_at.to_le_bytes());
                let b = path.as_bytes();
                m[0x28..0x28 + b.len().min(255)].copy_from_slice(&b[..b.len().min(255)]);
                out.extend_from_slice(&m);
            }
        }
        _ => return STATUS_INVALID_INFO_CLASS,
    }
    if !result.is_null() {
        unsafe { result.write(out.len() as u32) };
    }
    if (len as usize) < out.len() {
        return 0xC000_0004u32 as i32; // STATUS_INFO_LENGTH_MISMATCH
    }
    unsafe { core::ptr::copy_nonoverlapping(out.as_ptr(), info, out.len()) };
    STATUS_SUCCESS
}

// -----| loading drivers |-----

/// ZwLoadDriver(DriverServiceName): the driver its Services key's
/// ImagePath names (\SystemRoot\..., \??\C:\..., or relative to
/// /system), loaded and started.
pub extern "win64" fn ZwLoadDriver(service: *const u8) -> NtStatus {
    let nt = rtl::unicode_to_string(service);
    let Some(key) = cm::hive_path(&nt) else { return STATUS_OBJECT_NAME_NOT_FOUND };
    let Some((_, data)) = crate::nt::registry::kernel_get(&key, "ImagePath") else { return STATUS_OBJECT_NAME_NOT_FOUND };
    let units: Vec<u16> = data.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).take_while(|&u| u != 0).collect();
    let image_path = String::from_utf16_lossy(&units);
    let vfs = if image_path.starts_with('\\') {
        kernel_vfs_path(&image_path)
    } else {
        Some(format!("/system/{}", image_path.replace('\\', "/")))
    };
    let Some(vfs) = vfs else { return STATUS_OBJECT_NAME_NOT_FOUND };
    match image::load(&vfs.to_ascii_lowercase()) {
        Ok((_, st)) => st,
        Err(e) => {
            crate::serial_println!("[NTOS] ZwLoadDriver {}: {}", vfs, e);
            0xC000_0034u32 as i32
        }
    }
}

pub extern "win64" fn ZwUnloadDriver(service: *const u8) -> NtStatus {
    let nt = rtl::unicode_to_string(service);
    let name = nt.rsplit('\\').next().unwrap_or("").to_ascii_lowercase();
    match image::unload(&name) {
        Ok(()) => STATUS_SUCCESS,
        Err(_) => STATUS_INVALID_PARAMETER,
    }
}

// -----| not backed yet |-----

pub extern "win64" fn ZwAllocateVirtualMemory(_p: u64, _base: *mut u64, _zero: u64, _size: *mut u64, _kind: u32, _protect: u32) -> NtStatus {
    unsupported("ZwAllocateVirtualMemory")
}
pub extern "win64" fn ZwFreeVirtualMemory(_p: u64, _base: *mut u64, _size: *mut u64, _kind: u32) -> NtStatus {
    unsupported("ZwFreeVirtualMemory")
}
pub extern "win64" fn ZwQueryVirtualMemory(_p: u64, _base: u64, _class: u32, _info: *mut u8, _len: usize, _ret: *mut usize) -> NtStatus {
    unsupported("ZwQueryVirtualMemory")
}
pub extern "win64" fn ZwCreateSection(_out: *mut u64, _access: u32, _oa: *const u8, _max: *const i64, _protect: u32, _attrs: u32, _file: u64) -> NtStatus {
    unsupported("ZwCreateSection")
}
pub extern "win64" fn ZwOpenSection(_out: *mut u64, _access: u32, _oa: *const u8) -> NtStatus {
    unsupported("ZwOpenSection")
}
pub extern "win64" fn ZwMapViewOfSection(_s: u64, _p: u64, _base: *mut u64, _zero: u64, _commit: u64, _off: *mut i64, _size: *mut u64, _inherit: u32, _kind: u32, _protect: u32) -> NtStatus {
    unsupported("ZwMapViewOfSection")
}
pub extern "win64" fn ZwUnmapViewOfSection(_p: u64, _base: u64) -> NtStatus {
    unsupported("ZwUnmapViewOfSection")
}
pub extern "win64" fn ZwOpenProcess(_out: *mut u64, _access: u32, _oa: *const u8, _cid: *const u8) -> NtStatus {
    unsupported("ZwOpenProcess")
}
pub extern "win64" fn ZwQueryInformationProcess(_p: u64, _class: u32, _info: *mut u8, _len: u32, _ret: *mut u32) -> NtStatus {
    unsupported("ZwQueryInformationProcess")
}
pub extern "win64" fn ZwOpenProcessTokenEx(_p: u64, _access: u32, _attrs: u32, _out: *mut u64) -> NtStatus {
    unsupported("ZwOpenProcessTokenEx")
}
pub extern "win64" fn ZwQueryInformationToken(_t: u64, _class: u32, _info: *mut u8, _len: u32, _ret: *mut u32) -> NtStatus {
    unsupported("ZwQueryInformationToken")
}
pub extern "win64" fn ZwSetSecurityObject(_h: u64, _info: u32, _sd: *const u8) -> NtStatus {
    unsupported("ZwSetSecurityObject")
}
pub extern "win64" fn ZwQueryInformationThread(_t: u64, _class: u32, _info: *mut u8, _len: u32, _ret: *mut u32) -> NtStatus {
    unsupported("ZwQueryInformationThread")
}
pub extern "win64" fn ZwPowerInformation(_level: u32, _in: *const u8, _in_len: u32, _out: *mut u8, _out_len: u32) -> NtStatus {
    unsupported("ZwPowerInformation")
}

/// ZwSetInformationThread: priorities and "hide from debugger" are what
/// drivers set; VEILOS's kernel threads have neither, so it succeeds.
pub extern "win64" fn ZwSetInformationThread(_t: u64, _class: u32, _info: *const u8, _len: u32) -> NtStatus {
    STATUS_SUCCESS
}
