//! Strings and paths (W7): UNICODE_STRING helpers, DOS paths to NT paths,
//! the version, NtOpenFile, and the token and process calls VEILOS answers
//! simply (no security tokens yet).

use core::ptr::read_unaligned;
use nt_abi::{layout, status, NtStatus};

#[repr(C)]
pub struct UnicodeString {
    length: u16,
    maximum_length: u16,
    buffer: *mut u16,
}

fn heap() -> *mut crate::heap::Heap {
    let peb: u64;
    unsafe {
        core::arch::asm!("mov {}, gs:[0x60]", out(reg) peb, options(nostack, readonly));
        read_unaligned((peb + 0x30) as *const u64) as *mut crate::heap::Heap
    }
}

unsafe fn wlen(p: *const u16) -> usize {
    let mut n = 0;
    while unsafe { *p.add(n) } != 0 {
        n += 1;
    }
    n
}

#[no_mangle]
pub unsafe extern "system" fn RtlInitUnicodeString(s: *mut UnicodeString, text: *const u16) {
    let n = if text.is_null() { 0 } else { unsafe { wlen(text) } };
    unsafe {
        *s = UnicodeString { length: (n * 2) as u16, maximum_length: if text.is_null() { 0 } else { (n * 2 + 2) as u16 }, buffer: text as *mut u16 };
    }
}

#[no_mangle]
pub unsafe extern "system" fn RtlInitUnicodeStringEx(s: *mut UnicodeString, text: *const u16) -> NtStatus {
    unsafe { RtlInitUnicodeString(s, text) };
    status::SUCCESS
}

/// ANSI_STRING has the same shape with bytes.
#[no_mangle]
pub unsafe extern "system" fn RtlInitAnsiString(s: *mut UnicodeString, text: *const u8) {
    let mut n = 0;
    if !text.is_null() {
        while unsafe { *text.add(n) } != 0 {
            n += 1;
        }
    }
    unsafe {
        *s = UnicodeString { length: n as u16, maximum_length: if text.is_null() { 0 } else { (n + 1) as u16 }, buffer: text as *mut u16 };
    }
}

/// A heap copy of `units` as a UNICODE_STRING (NUL-terminated).
unsafe fn make(s: *mut UnicodeString, units: impl Iterator<Item = u16> + Clone) -> bool {
    let n = units.clone().count();
    let buf = unsafe { crate::RtlAllocateHeap(heap(), 0, (n + 1) * 2) } as *mut u16;
    if buf.is_null() {
        return false;
    }
    for (i, u) in units.enumerate() {
        unsafe { *buf.add(i) = u };
    }
    unsafe {
        *buf.add(n) = 0;
        *s = UnicodeString { length: (n * 2) as u16, maximum_length: (n * 2 + 2) as u16, buffer: buf };
    }
    true
}

#[no_mangle]
pub unsafe extern "system" fn RtlCreateUnicodeString(s: *mut UnicodeString, text: *const u16) -> u8 {
    let n = unsafe { wlen(text) };
    unsafe { make(s, core::slice::from_raw_parts(text, n).iter().copied()) as u8 }
}

#[no_mangle]
pub unsafe extern "system" fn RtlCreateUnicodeStringFromAsciiz(s: *mut UnicodeString, text: *const u8) -> u8 {
    let mut n = 0;
    while unsafe { *text.add(n) } != 0 {
        n += 1;
    }
    unsafe { make(s, core::slice::from_raw_parts(text, n).iter().map(|&b| b as u16)) as u8 }
}

#[no_mangle]
pub unsafe extern "system" fn RtlFreeUnicodeString(s: *mut UnicodeString) {
    unsafe {
        if !(*s).buffer.is_null() {
            crate::RtlFreeHeap(heap(), 0, (*s).buffer as *mut u8);
        }
        *s = UnicodeString { length: 0, maximum_length: 0, buffer: core::ptr::null_mut() };
    }
}

#[no_mangle]
pub unsafe extern "system" fn RtlFreeAnsiString(s: *mut UnicodeString) {
    unsafe { RtlFreeUnicodeString(s) };
}

#[no_mangle]
pub extern "system" fn RtlFindLeastSignificantBit(set: u64) -> i8 {
    if set == 0 { -1 } else { set.trailing_zeros() as i8 }
}

#[no_mangle]
pub extern "system" fn RtlFindMostSignificantBit(set: u64) -> i8 {
    if set == 0 { -1 } else { 63 - set.leading_zeros() as i8 }
}

/// RTL_OSVERSIONINFOW: size, major, minor, build, platform, CSD.
#[no_mangle]
pub unsafe extern "system" fn RtlGetVersion(info: *mut u32) -> NtStatus {
    unsafe {
        let size = *info as usize;
        core::ptr::write_bytes((info as *mut u8).add(4), 0, size.saturating_sub(4));
        *info.add(1) = 10;
        *info.add(2) = 0;
        *info.add(3) = 19041;
        *info.add(4) = 2;
    }
    status::SUCCESS
}

#[no_mangle]
pub unsafe extern "system" fn RtlGetNtVersionNumbers(major: *mut u32, minor: *mut u32, build: *mut u32) {
    unsafe {
        if !major.is_null() {
            *major = 10;
        }
        if !minor.is_null() {
            *minor = 0;
        }
        if !build.is_null() {
            *build = 19041 | 0xF000_0000;
        }
    }
}

// -----| DOS paths to NT paths |-----

/// The current directory (with its trailing backslash), from the process
/// parameters.
fn current_directory() -> &'static [u16] {
    unsafe {
        let peb: u64;
        core::arch::asm!("mov {}, gs:[0x60]", out(reg) peb, options(nostack, readonly));
        let pp = read_unaligned((peb + layout::PEB_PROCESS_PARAMETERS as u64) as *const u64);
        let us = pp + layout::PARAMS_CURRENT_DIRECTORY as u64;
        let len = read_unaligned(us as *const u16) as usize / 2;
        let buf = read_unaligned((us + 8) as *const u64) as *const u16;
        core::slice::from_raw_parts(buf, len)
    }
}

const SLASH: u16 = b'\\' as u16;

/// `dos` made absolute and normalised ("C:\dir\file"), in `out`. Returns
/// the length and where the last component starts.
fn full_dos_path(dos: &[u16], out: &mut [u16; 1024]) -> Option<(usize, usize)> {
    let mut n = 0;
    fn push(out: &mut [u16; 1024], u: u16, n: &mut usize) {
        if *n < out.len() - 1 {
            out[*n] = if u == b'/' as u16 { SLASH } else { u };
            *n += 1;
        }
    }
    let cwd = current_directory();
    let is_sep = |u: u16| u == SLASH || u == b'/' as u16;
    // The root ("C:\") and the rest.
    let rest: &[u16] = if dos.len() >= 2 && dos[1] == b':' as u16 {
        push(out, dos[0], &mut n);
        push(out, b':' as u16, &mut n);
        push(out, SLASH, &mut n);
        if dos.len() >= 3 && is_sep(dos[2]) {
            &dos[3..]
        } else {
            // "C:name": relative to the current directory if it's on C:.
            if cwd.len() > 3 && (cwd[0] | 0x20) == (dos[0] | 0x20) {
                for &u in &cwd[3..] {
                    push(out, u, &mut n);
                }
            }
            &dos[2..]
        }
    } else if !dos.is_empty() && is_sep(dos[0]) {
        for &u in cwd.iter().take(3) {
            push(out, u, &mut n);
        }
        &dos[1..]
    } else {
        for &u in cwd {
            push(out, u, &mut n);
        }
        dos
    };
    if n > 0 && out[n - 1] != SLASH {
        push(out, SLASH, &mut n);
    }
    // Components, with "." dropped and ".." going up (not above the root).
    for part in rest.split(|&u| is_sep(u)) {
        if part.is_empty() || part == [b'.' as u16] {
            continue;
        }
        if part == [b'.' as u16, b'.' as u16] {
            if n > 3 {
                n -= 1; // the trailing backslash
                while n > 3 && out[n - 1] != SLASH {
                    n -= 1;
                }
            }
            continue;
        }
        for &u in part {
            push(out, u, &mut n);
        }
        push(out, SLASH, &mut n);
    }
    // No trailing backslash unless the path is a root or ended with one.
    let trailing = rest.last().is_some_and(|&u| is_sep(u)) || n <= 3;
    if !trailing && n > 3 {
        n -= 1;
    }
    let last = out[..n].iter().rposition(|&u| u == SLASH).map_or(0, |i| i + 1);
    Some((n, last))
}

/// RTL_RELATIVE_NAME_U: RelativeName (UNICODE_STRING), ContainingDirectory, CurDirRef.
#[repr(C)]
pub struct RelativeName {
    relative: UnicodeString,
    directory: u64,
    reference: u64,
}

unsafe fn dos_to_nt(dos: *const u16, nt: *mut UnicodeString, part: *mut *mut u16, relative: *mut RelativeName) -> NtStatus {
    if dos.is_null() || nt.is_null() {
        return status::OBJECT_NAME_INVALID;
    }
    let src = unsafe { core::slice::from_raw_parts(dos, wlen(dos)) };
    let mut full = [0u16; 1024];
    let Some((len, last)) = full_dos_path(src, &mut full) else { return status::OBJECT_NAME_INVALID };
    let prefix: [u16; 4] = [SLASH, b'?' as u16, b'?' as u16, SLASH];
    if !unsafe { make(nt, prefix.iter().copied().chain(full[..len].iter().copied())) } {
        return status::NO_MEMORY;
    }
    unsafe {
        if !part.is_null() {
            *part = if last < len { (*nt).buffer.add(4 + last) } else { core::ptr::null_mut() };
        }
        if !relative.is_null() {
            core::ptr::write_bytes(relative as *mut u8, 0, core::mem::size_of::<RelativeName>());
        }
    }
    status::SUCCESS
}

#[no_mangle]
pub unsafe extern "system" fn RtlDosPathNameToNtPathName_U(dos: *const u16, nt: *mut UnicodeString, part: *mut *mut u16, relative: *mut RelativeName) -> u8 {
    (unsafe { dos_to_nt(dos, nt, part, relative) } == status::SUCCESS) as u8
}

#[no_mangle]
pub unsafe extern "system" fn RtlDosPathNameToNtPathName_U_WithStatus(dos: *const u16, nt: *mut UnicodeString, part: *mut *mut u16, relative: *mut RelativeName) -> NtStatus {
    unsafe { dos_to_nt(dos, nt, part, relative) }
}

#[no_mangle]
pub unsafe extern "system" fn RtlDosPathNameToRelativeNtPathName_U(dos: *const u16, nt: *mut UnicodeString, part: *mut *mut u16, relative: *mut RelativeName) -> u8 {
    unsafe { RtlDosPathNameToNtPathName_U(dos, nt, part, relative) }
}

#[no_mangle]
pub unsafe extern "system" fn RtlDosPathNameToRelativeNtPathName_U_WithStatus(dos: *const u16, nt: *mut UnicodeString, part: *mut *mut u16, relative: *mut RelativeName) -> NtStatus {
    unsafe { dos_to_nt(dos, nt, part, relative) }
}

#[no_mangle]
pub extern "system" fn RtlReleaseRelativeName(_relative: *mut RelativeName) {}

/// RtlGetFullPathName_U: the absolute DOS path, in bytes written.
#[no_mangle]
pub unsafe extern "system" fn RtlGetFullPathName_U(dos: *const u16, size: u32, out: *mut u16, part: *mut *mut u16) -> u32 {
    let src = unsafe { core::slice::from_raw_parts(dos, wlen(dos)) };
    let mut full = [0u16; 1024];
    let Some((len, last)) = full_dos_path(src, &mut full) else { return 0 };
    if (len + 1) * 2 > size as usize {
        return ((len + 1) * 2) as u32;
    }
    unsafe {
        core::ptr::copy_nonoverlapping(full.as_ptr(), out, len);
        *out.add(len) = 0;
        if !part.is_null() {
            *part = if last < len { out.add(last) } else { core::ptr::null_mut() };
        }
    }
    (len * 2) as u32
}

// -----| files, tokens, process |-----

type CreateFile = unsafe extern "system" fn(*mut u64, u32, *const u8, *mut u8, *const i64, u32, u32, u32, u32, *const u8, u32) -> NtStatus;

/// NtOpenFile: NtCreateFile with FILE_OPEN.
#[no_mangle]
pub unsafe extern "system" fn NtOpenFile(handle: *mut u64, access: u32, attrs: *const u8, iosb: *mut u8, share: u32, options: u32) -> NtStatus {
    let create: CreateFile = unsafe { core::mem::transmute(crate::NtCreateFile as *const ()) };
    unsafe { create(handle, access, attrs, iosb, core::ptr::null(), 0, share, 1, options, core::ptr::null(), 0) }
}

#[no_mangle]
pub unsafe extern "system" fn ZwOpenFile(handle: *mut u64, access: u32, attrs: *const u8, iosb: *mut u8, share: u32, options: u32) -> NtStatus {
    unsafe { NtOpenFile(handle, access, attrs, iosb, share, options) }
}

const STATUS_NO_TOKEN: NtStatus = 0xC000_007Cu32 as i32;
const STATUS_INVALID_DEVICE_REQUEST: NtStatus = 0xC000_0010u32 as i32;
const STATUS_NOT_FOUND: NtStatus = 0xC000_0225u32 as i32;

/// No security tokens yet: threads have none, processes can't be opened for one.
#[no_mangle]
pub extern "system" fn NtOpenThreadToken(_thread: u64, _access: u32, _as_self: u8, token: *mut u64) -> NtStatus {
    if !token.is_null() {
        unsafe { *token = 0 };
    }
    STATUS_NO_TOKEN
}

#[no_mangle]
pub extern "system" fn NtOpenThreadTokenEx(thread: u64, access: u32, as_self: u8, _attrs: u32, token: *mut u64) -> NtStatus {
    NtOpenThreadToken(thread, access, as_self, token)
}

#[no_mangle]
pub extern "system" fn NtOpenProcessToken(_process: u64, _access: u32, token: *mut u64) -> NtStatus {
    if !token.is_null() {
        unsafe { *token = 0 };
    }
    status::ACCESS_DENIED
}

#[no_mangle]
pub extern "system" fn NtOpenProcessTokenEx(process: u64, access: u32, _attrs: u32, token: *mut u64) -> NtStatus {
    NtOpenProcessToken(process, access, token)
}

#[no_mangle]
pub extern "system" fn NtQueryInformationToken(_token: u64, _class: u32, _info: *mut u8, _len: u32, _ret: *mut u32) -> NtStatus {
    status::INVALID_HANDLE
}

/// Process settings (priority, error mode, ...) are accepted.
#[no_mangle]
pub extern "system" fn NtSetInformationProcess(_process: u64, _class: u32, _info: *const u8, _len: u32) -> NtStatus {
    status::SUCCESS
}

#[no_mangle]
pub extern "system" fn NtFsControlFile(_h: u64, _ev: u64, _apc: usize, _ctx: usize, _iosb: *mut u8, _code: u32, _in: usize, _in_len: u32, _out: usize, _out_len: u32) -> NtStatus {
    STATUS_INVALID_DEVICE_REQUEST
}

#[no_mangle]
pub extern "system" fn NtCancelSynchronousIoFile(_thread: u64, _request: *mut u8, _iosb: *mut u8) -> NtStatus {
    STATUS_NOT_FOUND
}
