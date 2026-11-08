//! Calls claude.exe (a Bun build) imports that need little here: answers
//! about things VEILOS doesn't keep (jobs, sessions, priorities, volumes,
//! resource updates, process lists) and a few easy ones.

use crate::util::wide_out;
use crate::{Bool, Handle};

const ERROR_NOT_SUPPORTED: u32 = 50;
const INVALID_HANDLE_VALUE: Handle = usize::MAX as Handle;

fn unsupported() -> Bool {
    crate::SetLastError(ERROR_NOT_SUPPORTED);
    0
}

fn teb_u64(offset: usize) -> u64 {
    unsafe { core::ptr::read_unaligned(crate::teb().add(offset) as *const u64) }
}

// -----| threads and processes |-----

/// The thread's whole stack: TEB.DeallocationStack (or StackLimit) to StackBase.
#[no_mangle]
pub unsafe extern "system" fn GetCurrentThreadStackLimits(low: *mut u64, high: *mut u64) {
    let dealloc = teb_u64(0x1478);
    unsafe {
        *low = if dealloc != 0 { dealloc } else { teb_u64(0x10) };
        *high = teb_u64(0x08);
    }
}

/// x86 keeps instruction caches coherent.
#[no_mangle]
pub extern "system" fn FlushInstructionCache(_process: Handle, _base: usize, _size: usize) -> Bool {
    1
}

/// Threads have no descriptions kept: an empty one (LocalFree'd by the caller).
#[no_mangle]
pub unsafe extern "system" fn GetThreadDescription(_thread: Handle, out: *mut *mut u16) -> i32 {
    let p = crate::mem::LocalAlloc(0x40, 2) as *mut u16;
    if p.is_null() {
        return 0x8007_000Eu32 as i32; // E_OUTOFMEMORY
    }
    unsafe { *out = p };
    0
}

/// Register contents of another thread: not kept (threads can't be suspended yet).
#[no_mangle]
pub extern "system" fn GetThreadContext(_thread: Handle, _context: *mut u8) -> Bool {
    unsupported()
}

#[no_mangle]
pub extern "system" fn GetPriorityClass(_process: Handle) -> u32 {
    0x20 // NORMAL_PRIORITY_CLASS
}

#[no_mangle]
pub extern "system" fn SetPriorityClass(_process: Handle, _class: u32) -> Bool {
    1
}

/// Everything runs in session 1 (the interactive one).
#[no_mangle]
pub unsafe extern "system" fn ProcessIdToSessionId(_pid: u32, session: *mut u32) -> Bool {
    if !session.is_null() {
        unsafe { *session = 1 };
    }
    1
}

/// IO_COUNTERS: not counted, so all zero.
#[no_mangle]
pub unsafe extern "system" fn GetProcessIoCounters(_process: Handle, counters: *mut u8) -> Bool {
    if !counters.is_null() {
        unsafe { core::ptr::write_bytes(counters, 0, 48) };
    }
    1
}

#[no_mangle]
pub unsafe extern "system" fn IsProcessInJob(_process: Handle, _job: Handle, result: *mut Bool) -> Bool {
    if !result.is_null() {
        unsafe { *result = 0 };
    }
    1
}

#[no_mangle]
pub extern "system" fn CreateJobObjectA(sa: usize, _name: *const u8) -> Handle {
    crate::toolchain::CreateJobObjectW(sa, core::ptr::null())
}

#[no_mangle]
pub extern "system" fn CreateMemoryResourceNotification(_kind: u32) -> Handle {
    unsupported();
    0
}

// -----| process and thread lists: not offered |-----

#[no_mangle]
pub extern "system" fn CreateToolhelp32Snapshot(_flags: u32, _pid: u32) -> Handle {
    unsupported();
    INVALID_HANDLE_VALUE
}

fn no_more() -> Bool {
    crate::SetLastError(18); // ERROR_NO_MORE_FILES
    0
}

#[no_mangle]
pub extern "system" fn Process32FirstW(_snap: Handle, _entry: *mut u8) -> Bool {
    no_more()
}

#[no_mangle]
pub extern "system" fn Process32NextW(_snap: Handle, _entry: *mut u8) -> Bool {
    no_more()
}

#[no_mangle]
pub extern "system" fn Thread32First(_snap: Handle, _entry: *mut u8) -> Bool {
    no_more()
}

#[no_mangle]
pub extern "system" fn Thread32Next(_snap: Handle, _entry: *mut u8) -> Bool {
    no_more()
}

/// The modules of this process: its EXE (DLLs aren't listed yet).
#[no_mangle]
pub unsafe extern "system" fn K32EnumProcessModules(_process: Handle, modules: *mut Handle, bytes: u32, needed: *mut u32) -> Bool {
    let exe = unsafe { crate::module::GetModuleHandleW(core::ptr::null()) };
    if !needed.is_null() {
        unsafe { *needed = 8 };
    }
    if bytes >= 8 && !modules.is_null() {
        unsafe { *modules = exe };
    }
    1
}

/// A module's file name without its folder.
#[no_mangle]
pub unsafe extern "system" fn K32GetModuleBaseNameW(_process: Handle, module: Handle, out: *mut u16, size: u32) -> u32 {
    let mut path = [0u16; 520];
    let n = unsafe { crate::module::GetModuleFileNameW(module, path.as_mut_ptr(), path.len() as u32) } as usize;
    if n == 0 {
        return 0;
    }
    let start = path[..n].iter().rposition(|&c| c == b'\\' as u16 || c == b'/' as u16).map_or(0, |i| i + 1);
    let name = &path[start..n];
    let take = name.len().min(size.saturating_sub(1) as usize);
    if size > 0 && !out.is_null() {
        unsafe {
            core::ptr::copy_nonoverlapping(name.as_ptr(), out, take);
            *out.add(take) = 0;
        }
    }
    take as u32
}

// -----| names |-----

/// The computer's name in each form; no DNS domain.
#[no_mangle]
pub unsafe extern "system" fn GetComputerNameExW(format: u32, out: *mut u16, size: *mut u32) -> Bool {
    // ComputerNameDnsDomain (2), ComputerNamePhysicalDnsDomain (6): none.
    if format == 2 || format == 6 {
        if size.is_null() {
            crate::SetLastError(87);
            return 0;
        }
        unsafe {
            if out.is_null() || *size == 0 {
                *size = 1;
                crate::SetLastError(234); // ERROR_MORE_DATA
                return 0;
            }
            *out = 0;
            *size = 0;
        }
        return 1;
    }
    unsafe { crate::legacy::GetComputerNameW(out, size) }
}

// -----| files and volumes |-----

#[no_mangle]
pub unsafe extern "system" fn CreateDirectoryExW(_template: *const u16, name: *const u16, sa: usize) -> Bool {
    unsafe { crate::file::CreateDirectoryW(name, sa) }
}

/// Watching folders (fs.watch): not yet.
#[no_mangle]
pub extern "system" fn ReadDirectoryChangesW(_dir: Handle, _buf: *mut u8, _len: u32, _subtree: Bool, _filter: u32, _ret: *mut u32, _ov: *mut u8, _routine: u64) -> Bool {
    unsupported()
}

#[no_mangle]
pub extern "system" fn OpenFileById(_volume: Handle, _id: *const u8, _access: u32, _share: u32, _sa: usize, _flags: u32) -> Handle {
    unsupported();
    INVALID_HANDLE_VALUE
}

/// I/O completing through APCs isn't offered (overlapped I/O with events
/// and completion ports is).
#[no_mangle]
pub extern "system" fn ReadFileEx(_h: Handle, _buf: *mut u8, _len: u32, _ov: *mut u8, _routine: u64) -> Bool {
    unsupported()
}

#[no_mangle]
pub extern "system" fn WriteFileEx(_h: Handle, _buf: *const u8, _len: u32, _ov: *mut u8, _routine: u64) -> Bool {
    unsupported()
}

#[no_mangle]
pub extern "system" fn QueryDosDeviceW(_name: *const u16, _out: *mut u16, _size: u32) -> u32 {
    unsupported();
    0
}

#[no_mangle]
pub extern "system" fn FindFirstVolumeW(_out: *mut u16, _size: u32) -> Handle {
    unsupported();
    INVALID_HANDLE_VALUE
}

#[no_mangle]
pub extern "system" fn FindNextVolumeW(_find: Handle, _out: *mut u16, _size: u32) -> Bool {
    no_more()
}

#[no_mangle]
pub extern "system" fn FindVolumeClose(_find: Handle) -> Bool {
    1
}

/// Pages are never swapped out, so there's nothing to unlock.
#[no_mangle]
pub extern "system" fn VirtualUnlock(_base: usize, _size: usize) -> Bool {
    1
}

// -----| named pipes: what isn't kept |-----

#[no_mangle]
pub extern "system" fn GetNamedPipeClientProcessId(_pipe: Handle, _pid: *mut u32) -> Bool {
    unsupported()
}

#[no_mangle]
pub extern "system" fn GetNamedPipeServerProcessId(_pipe: Handle, _pid: *mut u32) -> Bool {
    unsupported()
}

#[no_mangle]
pub extern "system" fn GetNamedPipeHandleStateA(_pipe: Handle, _state: *mut u32, _instances: *mut u32, _count: *mut u32, _timeout: *mut u32, _user: *mut u8, _size: u32) -> Bool {
    unsupported()
}

// -----| resources: no updating, no enumerating |-----

#[no_mangle]
pub extern "system" fn BeginUpdateResourceW(_file: *const u16, _delete: Bool) -> Handle {
    unsupported();
    0
}

#[no_mangle]
pub extern "system" fn UpdateResourceW(_update: Handle, _kind: *const u16, _name: *const u16, _lang: u16, _data: *const u8, _size: u32) -> Bool {
    unsupported()
}

#[no_mangle]
pub extern "system" fn EndUpdateResourceW(_update: Handle, _discard: Bool) -> Bool {
    unsupported()
}

#[no_mangle]
pub extern "system" fn EnumResourceNamesW(_module: Handle, _kind: *const u16, _f: u64, _param: isize) -> Bool {
    crate::SetLastError(1813); // ERROR_RESOURCE_TYPE_NOT_FOUND
    0
}

#[no_mangle]
pub extern "system" fn EnumResourceLanguagesW(_module: Handle, _kind: *const u16, _name: *const u16, _f: u64, _param: isize) -> Bool {
    crate::SetLastError(1813);
    0
}

// -----| locales: one, en-US |-----

/// EnumSystemLocalesW(callback, flags): the one locale, as "00000409".
#[no_mangle]
pub unsafe extern "system" fn EnumSystemLocalesW(callback: u64, _flags: u32) -> Bool {
    if callback == 0 {
        crate::SetLastError(87);
        return 0;
    }
    let id: [u16; 9] = [b'0' as u16, b'0' as u16, b'0' as u16, b'0' as u16, b'0' as u16, b'4' as u16, b'0' as u16, b'9' as u16, 0];
    let f: unsafe extern "system" fn(*const u16) -> Bool = unsafe { core::mem::transmute(callback) };
    unsafe { f(id.as_ptr()) };
    1
}

/// ResolveLocaleName(name, out, size): every name resolves to en-US.
#[no_mangle]
pub unsafe extern "system" fn ResolveLocaleName(_name: *const u16, out: *mut u16, size: i32) -> i32 {
    let en: [u16; 5] = [b'e' as u16, b'n' as u16, b'-' as u16, b'U' as u16, b'S' as u16];
    if out.is_null() || size <= 0 {
        return en.len() as i32 + 1;
    }
    if (size as usize) <= en.len() {
        crate::SetLastError(122); // ERROR_INSUFFICIENT_BUFFER
        return 0;
    }
    unsafe { wide_out(&en, out, size as u32) };
    en.len() as i32 + 1
}

#[no_mangle]
pub extern "system" fn GetNumberFormatEx(_locale: *const u16, _flags: u32, _value: *const u16, _format: *const u8, _out: *mut u16, _size: i32) -> i32 {
    unsupported();
    0
}

#[no_mangle]
pub extern "system" fn GetCurrencyFormatEx(_locale: *const u16, _flags: u32, _value: *const u16, _format: *const u8, _out: *mut u16, _size: i32) -> i32 {
    unsupported();
    0
}

#[no_mangle]
pub extern "system" fn GetGeoInfoW(_geo: i32, _kind: u32, _out: *mut u16, _size: i32, _lang: u16) -> i32 {
    unsupported();
    0
}

/// The United States (GEOID 244), matching the locale.
#[no_mangle]
pub extern "system" fn GetUserGeoID(_class: u32) -> i32 {
    244
}

/// The one time zone, whatever the year.
#[no_mangle]
pub unsafe extern "system" fn GetTimeZoneInformationForYear(_year: u16, _dynamic: *const u8, info: *mut u8) -> Bool {
    let r = unsafe { crate::time::GetTimeZoneInformation(info) };
    (r != u32::MAX) as Bool
}
