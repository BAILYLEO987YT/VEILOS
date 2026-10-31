//! File mapping, emulated: a mapping holds its own duplicate of the file
//! handle (or nothing, for memory backed by the paging file) and one buffer
//! with the contents, read once; every view of the mapping is that buffer,
//! so views agree with each other. Writable mappings are written back to
//! the file when a view is flushed or unmapped. (Two processes mapping
//! the same file each get their own copy.)
//!
//! The mapping handle is a kernel event, so CloseHandle and DuplicateHandle
//! work on it; this table says what it maps.

use core::sync::atomic::{AtomicBool, Ordering};

use crate::{Bool, Handle, INVALID_HANDLE_VALUE};

const PAGE_READWRITE: u32 = 0x04;
const PAGE_WRITECOPY: u32 = 0x08;
const PAGE_EXECUTE_READWRITE: u32 = 0x40;
const FILE_MAP_WRITE: u32 = 0x0002;
const ERROR_INVALID_PARAMETER: u32 = 87;
const ERROR_NOT_ENOUGH_MEMORY: u32 = 8;
const ERROR_FILE_INVALID: u32 = 1006;

#[derive(Clone, Copy)]
struct Mapping {
    /// The mapping's handle (0: free slot).
    handle: Handle,
    /// Its own handle to the file (0: memory only).
    file: Handle,
    size: u64,
    writable: bool,
    /// The contents (VirtualAlloc'd), and how many views are open.
    base: usize,
    views: u32,
}

const EMPTY: Mapping = Mapping { handle: 0, file: 0, size: 0, writable: false, base: 0, views: 0 };
static mut TABLE: [Mapping; 128] = [EMPTY; 128];
static LOCK: AtomicBool = AtomicBool::new(false);

fn with<R>(f: impl FnOnce(&mut [Mapping; 128]) -> R) -> R {
    while LOCK.compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() {
        core::hint::spin_loop();
    }
    let r = f(unsafe { &mut *core::ptr::addr_of_mut!(TABLE) });
    LOCK.store(false, Ordering::Release);
    r
}

fn file_size(h: Handle) -> u64 {
    let mut size = 0i64;
    unsafe { crate::file::GetFileSizeEx(h, &mut size) };
    size.max(0) as u64
}

#[no_mangle]
pub unsafe extern "system" fn CreateFileMappingW(file: Handle, _sa: usize, protect: u32, hi: u32, lo: u32, _name: *const u16) -> Handle {
    let writable = matches!(protect & 0xFF, PAGE_READWRITE | PAGE_WRITECOPY | PAGE_EXECUTE_READWRITE);
    let requested = (hi as u64) << 32 | lo as u64;
    let paged = file == INVALID_HANDLE_VALUE || file == 0;
    if paged && requested == 0 {
        crate::SetLastError(ERROR_INVALID_PARAMETER);
        return 0;
    }
    // Our own handle to the file, so closing the program's is fine.
    let mut own: Handle = 0;
    if !paged {
        let me: Handle = u64::MAX; // GetCurrentProcess()
        if unsafe { crate::process::DuplicateHandle(me, file, me, &mut own, 0, 0, 2) } == 0 {
            return 0;
        }
    }
    let size = if requested != 0 { requested } else { file_size(own) };
    if size == 0 {
        crate::SetLastError(ERROR_FILE_INVALID);
        if own != 0 {
            crate::CloseHandle(own);
        }
        return 0;
    }
    let mut handle: Handle = 0;
    if unsafe { crate::NtCreateEvent(&mut handle, 0x1F0003, core::ptr::null(), 0, 0) } != 0 {
        return 0;
    }
    let ok = with(|t| match t.iter_mut().find(|m| m.handle == 0) {
        Some(m) => {
            *m = Mapping { handle, file: own, size, writable, base: 0, views: 0 };
            true
        }
        None => false,
    });
    if !ok {
        crate::CloseHandle(handle);
        crate::SetLastError(ERROR_NOT_ENOUGH_MEMORY);
        return 0;
    }
    handle
}

#[no_mangle]
pub unsafe extern "system" fn CreateFileMappingA(file: Handle, sa: usize, protect: u32, hi: u32, lo: u32, _name: *const u8) -> Handle {
    unsafe { CreateFileMappingW(file, sa, protect, hi, lo, core::ptr::null()) }
}

/// Named mappings aren't kept: opening one finds nothing.
#[no_mangle]
pub extern "system" fn OpenFileMappingW(_access: u32, _inherit: Bool, _name: *const u16) -> Handle {
    crate::SetLastError(2);
    0
}

/// The mapping's contents, read the first time a view asks.
fn contents(i: usize) -> Option<usize> {
    let m = with(|t| t[i]);
    if m.base != 0 {
        return Some(m.base);
    }
    let base = unsafe { crate::mem::VirtualAlloc(0, m.size as usize, 0x3000, 0x04) };
    if base == 0 {
        return None;
    }
    if m.file != 0 {
        let mut pos = 0i64;
        unsafe { crate::file::SetFilePointerEx(m.file, 0, &mut pos, 0) };
        let want = m.size.min(file_size(m.file)) as usize;
        let mut done = 0usize;
        while done < want {
            let mut got = 0u32;
            let chunk = (want - done).min(1 << 30) as u32;
            if unsafe { crate::ReadFile(m.file, (base + done) as *mut u8, chunk, &mut got, 0) } == 0 || got == 0 {
                break;
            }
            done += got as usize;
        }
    }
    with(|t| t[i].base = base);
    Some(base)
}

#[no_mangle]
pub unsafe extern "system" fn MapViewOfFileEx(h: Handle, access: u32, hi: u32, lo: u32, bytes: usize, _at: usize) -> usize {
    let Some(i) = with(|t| t.iter().position(|m| m.handle == h && h != 0)) else {
        crate::SetLastError(6);
        return 0;
    };
    let m = with(|t| t[i]);
    let offset = (hi as u64) << 32 | lo as u64;
    if offset > m.size || (access & FILE_MAP_WRITE != 0 && !m.writable) || offset + bytes as u64 > m.size {
        crate::SetLastError(if access & FILE_MAP_WRITE != 0 && !m.writable { 5 } else { ERROR_INVALID_PARAMETER });
        return 0;
    }
    let Some(base) = contents(i) else {
        crate::SetLastError(ERROR_NOT_ENOUGH_MEMORY);
        return 0;
    };
    with(|t| t[i].views += 1);
    base + offset as usize
}

#[no_mangle]
pub unsafe extern "system" fn MapViewOfFile(h: Handle, access: u32, hi: u32, lo: u32, bytes: usize) -> usize {
    unsafe { MapViewOfFileEx(h, access, hi, lo, bytes, 0) }
}

/// The mapping whose contents hold `addr`.
fn holding(addr: usize) -> Option<usize> {
    with(|t| t.iter().position(|m| m.handle != 0 && m.base != 0 && addr >= m.base && addr < m.base + m.size as usize))
}

/// Write a writable file mapping's contents back to its file.
fn write_back(i: usize) {
    let m = with(|t| t[i]);
    if !m.writable || m.file == 0 || m.base == 0 {
        return;
    }
    let mut pos = 0i64;
    unsafe { crate::file::SetFilePointerEx(m.file, 0, &mut pos, 0) };
    let mut done = 0usize;
    while done < m.size as usize {
        let mut put = 0u32;
        let chunk = (m.size as usize - done).min(1 << 30) as u32;
        if unsafe { crate::WriteFile(m.file, (m.base + done) as *const u8, chunk, &mut put, 0) } == 0 || put == 0 {
            break;
        }
        done += put as usize;
    }
}

#[no_mangle]
pub extern "system" fn FlushViewOfFile(addr: usize, _bytes: usize) -> Bool {
    match holding(addr) {
        Some(i) => {
            write_back(i);
            1
        }
        None => 0,
    }
}

#[no_mangle]
pub extern "system" fn UnmapViewOfFile(addr: usize) -> Bool {
    let Some(i) = holding(addr) else {
        crate::SetLastError(487); // ERROR_INVALID_ADDRESS
        return 0;
    };
    write_back(i);
    with(|t| t[i].views = t[i].views.saturating_sub(1));
    1
}

#[no_mangle]
pub extern "system" fn UnmapViewOfFileEx(addr: usize, _flags: u32) -> Bool {
    UnmapViewOfFile(addr)
}

/// CloseHandle on a mapping: its file handle goes, and its contents once
/// no view is open.
pub fn closed(h: Handle) {
    let Some(i) = with(|t| t.iter().position(|m| m.handle == h && h != 0)) else { return };
    let m = with(|t| t[i]);
    write_back(i);
    if m.file != 0 {
        crate::CloseHandle(m.file);
    }
    if m.base != 0 && m.views == 0 {
        unsafe { crate::mem::VirtualFree(m.base, 0, 0x8000) };
    }
    with(|t| t[i] = EMPTY);
}
