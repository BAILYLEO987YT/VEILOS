//! What the Rust toolchain and git ask of the system, beyond the rest: pointer
//! encoding, randomness, memory figures, job objects, file locks (nothing else
//! holds one on this machine, so they're granted), and final paths.

use nt_abi::IoStatusBlock;

use crate::util::{widen, wstr};
use crate::{check, Bool, Handle};

const ERROR_NOT_SUPPORTED: u32 = 50;
const ERROR_INSUFFICIENT_BUFFER: u32 = 122;

// -----| small answers |-----

#[no_mangle]
pub extern "system" fn EncodePointer(p: usize) -> usize {
    p
}

#[no_mangle]
pub extern "system" fn DecodePointer(p: usize) -> usize {
    p
}

#[no_mangle]
pub extern "system" fn AreFileApisANSI() -> Bool {
    1
}

#[no_mangle]
pub unsafe extern "system" fn SetThreadErrorMode(_mode: u32, old: *mut u32) -> Bool {
    if !old.is_null() {
        unsafe { *old = 0 };
    }
    1
}

/// Guard pages aren't kept: the thread's stack is all committed anyway.
#[no_mangle]
pub unsafe extern "system" fn SetThreadStackGuarantee(size: *mut u32) -> Bool {
    if !size.is_null() {
        unsafe { *size = 0 };
    }
    1
}

#[no_mangle]
pub extern "system" fn GetCurrentProcessorNumber() -> u32 {
    0
}

#[no_mangle]
pub extern "system" fn FreeConsole() -> Bool {
    1
}

#[no_mangle]
pub unsafe extern "system" fn IsWow64Process(_process: Handle, wow: *mut Bool) -> Bool {
    unsafe { *wow = 0 };
    1
}

#[no_mangle]
pub extern "system" fn FreeLibraryAndExitThread(_module: Handle, code: u32) -> ! {
    unsafe { crate::process::ExitThread(code) };
    #[allow(clippy::empty_loop)]
    loop {}
}

#[no_mangle]
pub extern "system" fn HeapCompact(_heap: Handle, _flags: u32) -> usize {
    1 << 20
}

#[no_mangle]
pub extern "system" fn HeapQueryInformation(_heap: Handle, _class: u32, _info: usize, _size: usize, _ret: usize) -> Bool {
    crate::SetLastError(ERROR_NOT_SUPPORTED);
    0
}

#[no_mangle]
pub unsafe extern "system" fn GetSystemTimes(idle: *mut u64, kernel: *mut u64, user: *mut u64) -> Bool {
    for p in [idle, kernel, user] {
        if !p.is_null() {
            unsafe { *p = 0 };
        }
    }
    1
}

/// DYNAMIC_TIME_ZONE_INFORMATION: all zero is UTC with no daylight saving.
#[no_mangle]
pub unsafe extern "system" fn GetDynamicTimeZoneInformation(info: *mut u8) -> u32 {
    unsafe { core::ptr::write_bytes(info, 0, 432) };
    0
}

#[no_mangle]
pub unsafe extern "system" fn SystemTimeToTzSpecificLocalTime(_zone: usize, utc: *const [u16; 8], local: *mut [u16; 8]) -> Bool {
    unsafe { *local = *utc };
    1
}

/// Windows 10 passes every version check a program can ask of it.
#[no_mangle]
pub extern "system" fn VerSetConditionMask(mask: u64, _type_bit: u32, _condition: u8) -> u64 {
    mask
}

#[no_mangle]
pub extern "system" fn VerifyVersionInfoW(_info: usize, _types: u32, _mask: u64) -> Bool {
    1
}

#[no_mangle]
pub extern "system" fn VerifyVersionInfoA(_info: usize, _types: u32, _mask: u64) -> Bool {
    1
}

/// CT_CTYPE1 for ASCII; everything beyond counts as a letter.
#[no_mangle]
pub unsafe extern "system" fn GetStringTypeW(kind: u32, s: *const u16, len: i32, out: *mut u16) -> Bool {
    const CT_CTYPE1: u32 = 1;
    if kind != CT_CTYPE1 {
        crate::SetLastError(87);
        return 0;
    }
    let chars = if len < 0 { unsafe { wstr(s) } } else { unsafe { core::slice::from_raw_parts(s, len as usize) } };
    for (i, &c) in chars.iter().enumerate() {
        let bits: u16 = if c >= 0x80 {
            0x100
        } else {
            let b = c as u8;
            let mut v = 0;
            if b.is_ascii_uppercase() { v |= 0x001 | 0x100; }
            if b.is_ascii_lowercase() { v |= 0x002 | 0x100; }
            if b.is_ascii_digit() { v |= 0x004; }
            if b.is_ascii_whitespace() || b == 0x0B { v |= 0x008; }
            if b.is_ascii_punctuation() { v |= 0x010; }
            if b.is_ascii_control() { v |= 0x020; }
            if b == b' ' || b == b'\t' { v |= 0x040; }
            if b.is_ascii_hexdigit() { v |= 0x080; }
            v
        };
        unsafe { *out.add(i) = bits };
    }
    1
}

// -----| memory |-----

/// (total, free) bytes of memory, from the kernel.
fn memory() -> (u64, u64) {
    let mut m = [0u64; 2];
    unsafe { crate::VeilSysInfo(1, m.as_mut_ptr() as u64, 16) };
    (m[0], m[1])
}

/// MEMORYSTATUSEX: dwLength, load, then physical, page file, virtual pairs.
#[no_mangle]
pub unsafe extern "system" fn GlobalMemoryStatusEx(out: *mut u64) -> Bool {
    let (total, free) = memory();
    let address_space = 1u64 << 47;
    unsafe {
        let load = if total == 0 { 0 } else { 100 - free.saturating_mul(100) / total };
        *(out as *mut u32).add(1) = load as u32;
        *out.add(1) = total;
        *out.add(2) = free;
        *out.add(3) = total;
        *out.add(4) = free;
        *out.add(5) = address_space;
        *out.add(6) = address_space - total.min(address_space);
        *out.add(7) = 0;
    }
    1
}

/// PROCESS_MEMORY_COUNTERS: nothing is counted per process yet.
#[no_mangle]
pub unsafe extern "system" fn K32GetProcessMemoryInfo(_process: Handle, out: *mut u32, size: u32) -> Bool {
    unsafe {
        core::ptr::write_bytes(out as *mut u8, 0, size as usize);
        *out = size;
    }
    1
}

#[no_mangle]
pub unsafe extern "system" fn GetProcessMemoryInfo(process: Handle, out: *mut u32, size: u32) -> Bool {
    unsafe { K32GetProcessMemoryInfo(process, out, size) }
}

// -----| randomness |-----

fn random(out: *mut u8, len: usize) -> bool {
    unsafe { crate::VeilSysInfo(0, out as u64, len as u64) == 0 }
}

#[no_mangle]
pub extern "system" fn ProcessPrng(out: *mut u8, len: usize) -> Bool {
    random(out, len) as Bool
}

/// Called RtlGenRandom in the documentation.
#[no_mangle]
pub extern "system" fn SystemFunction036(out: *mut u8, len: u32) -> u8 {
    random(out, len as usize) as u8
}

// -----| crypto that isn't here |-----
//
// What a missing import answers is 0, which for CNG is STATUS_SUCCESS: libssh2
// (in curl) opened its algorithms, believed it, and went on with no handles.
// These say no instead, each in its family's way. Forwarders in bcrypt,
// crypt32, advapi32, wldap32 and iphlpapi point their missing functions here.

/// CNG (bcrypt): STATUS_NOT_SUPPORTED.
#[no_mangle]
pub extern "system" fn VeilCngUnsupported() -> i32 {
    0xC000_00BBu32 as i32
}

/// CryptoAPI and certificate functions: FALSE or NULL, ERROR_NOT_SUPPORTED.
#[no_mangle]
pub extern "system" fn VeilCryptUnsupported() -> usize {
    crate::SetLastError(50);
    0
}

/// CryptoAPI keys: none to import, encrypt with or destroy.
#[no_mangle]
pub extern "system" fn CryptImportKey(_prov: usize, _data: *const u8, _len: u32, _pub: usize, _flags: u32, key: *mut usize) -> i32 {
    if !key.is_null() {
        unsafe { *key = 0 };
    }
    VeilCryptUnsupported() as i32
}

#[no_mangle]
pub extern "system" fn CryptEncrypt(_key: usize, _hash: usize, _last: i32, _flags: u32, _data: *mut u8, _len: *mut u32, _size: u32) -> i32 {
    VeilCryptUnsupported() as i32
}

#[no_mangle]
pub extern "system" fn CryptDestroyKey(_key: usize) -> i32 {
    VeilCryptUnsupported() as i32
}

/// LDAP calls that answer an error number: LDAP_NOT_SUPPORTED.
#[no_mangle]
pub extern "system" fn VeilLdapUnsupported() -> u32 {
    0x5C
}

#[no_mangle]
pub extern "system" fn BCryptGenRandom(_algorithm: usize, out: *mut u8, len: u32, _flags: u32) -> i32 {
    if random(out, len as usize) { 0 } else { 0xC000_0001u32 as i32 }
}

// -----| jobs: no limits are kept, so a job is just a handle |-----

#[no_mangle]
pub extern "system" fn CreateJobObjectW(_sa: usize, _name: *const u16) -> Handle {
    let mut h: Handle = 0;
    unsafe { crate::NtCreateEvent(&mut h, 0x1F0003, core::ptr::null(), 0, 0) };
    h
}

#[no_mangle]
pub extern "system" fn SetInformationJobObject(_job: Handle, _class: u32, _info: usize, _size: u32) -> Bool {
    1
}

#[no_mangle]
pub extern "system" fn AssignProcessToJobObject(_job: Handle, _process: Handle) -> Bool {
    1
}

// -----| files |-----

#[no_mangle]
pub extern "system" fn LockFile(_h: Handle, _lo: u32, _hi: u32, _n_lo: u32, _n_hi: u32) -> Bool {
    1
}

#[no_mangle]
pub extern "system" fn UnlockFile(_h: Handle, _lo: u32, _hi: u32, _n_lo: u32, _n_hi: u32) -> Bool {
    1
}

#[no_mangle]
pub extern "system" fn LockFileEx(h: Handle, _flags: u32, _reserved: u32, _n_lo: u32, _n_hi: u32, overlapped: usize) -> Bool {
    crate::iocp::complete(h, overlapped, 0, 0, true);
    1
}

#[no_mangle]
pub extern "system" fn UnlockFileEx(h: Handle, _reserved: u32, _n_lo: u32, _n_hi: u32, overlapped: usize) -> Bool {
    crate::iocp::complete(h, overlapped, 0, 0, true);
    1
}

/// Another handle to the same open file.
#[no_mangle]
pub unsafe extern "system" fn ReOpenFile(h: Handle, _access: u32, _share: u32, _flags: u32) -> Handle {
    let me: Handle = u64::MAX;
    let mut out: Handle = 0;
    if unsafe { crate::process::DuplicateHandle(me, h, me, &mut out, 0, 0, 2) } == 0 {
        return crate::INVALID_HANDLE_VALUE;
    }
    out
}

/// "\\?\C:\path" for VOLUME_NAME_DOS, what `fs::canonicalize` asks for. There's
/// one volume, so the other forms are the same path without a drive.
#[no_mangle]
pub unsafe extern "system" fn GetFinalPathNameByHandleW(h: Handle, out: *mut u16, cap: u32, flags: u32) -> u32 {
    const VOLUME_NAME_NONE: u32 = 4;
    // FILE_NAME_INFORMATION: length, then the name.
    let mut info = [0u8; 4 + 2 * 1024];
    let mut iosb = IoStatusBlock::default();
    let s = unsafe { crate::NtQueryInformationFile(h, &mut iosb, info.as_mut_ptr(), info.len() as u32, nt_abi::file::NAME_INFORMATION) };
    if check(s) == 0 {
        return 0;
    }
    let bytes = u32::from_le_bytes(info[0..4].try_into().unwrap()) as usize;
    let name = unsafe { core::slice::from_raw_parts(info.as_ptr().add(4) as *const u16, bytes / 2) };
    const PREFIX: [u16; 6] = [b'\\' as u16, b'\\' as u16, b'?' as u16, b'\\' as u16, b'C' as u16, b':' as u16];
    let prefix: &[u16] = if flags & VOLUME_NAME_NONE != 0 { &[] } else { &PREFIX };
    let total = prefix.len() + name.len();
    if cap as usize <= total {
        crate::SetLastError(ERROR_INSUFFICIENT_BUFFER);
        return total as u32 + 1;
    }
    unsafe {
        core::ptr::copy_nonoverlapping(prefix.as_ptr(), out, prefix.len());
        core::ptr::copy_nonoverlapping(name.as_ptr(), out.add(prefix.len()), name.len());
        *out.add(total) = 0;
    }
    total as u32
}

#[no_mangle]
pub unsafe extern "system" fn GetDiskFreeSpaceA(root: *const u8, spc: *mut u32, bps: *mut u32, free: *mut u32, total: *mut u32) -> Bool {
    let w = unsafe { widen(root) };
    unsafe { crate::system::GetDiskFreeSpaceW(if root.is_null() { core::ptr::null() } else { w.as_ptr() }, spc, bps, free, total) }
}

#[no_mangle]
pub unsafe extern "system" fn GetDiskFreeSpaceExA(root: *const u8, avail: *mut u64, total: *mut u64, free: *mut u64) -> Bool {
    let w = unsafe { widen(root) };
    unsafe { crate::system::GetDiskFreeSpaceExW(if root.is_null() { core::ptr::null() } else { w.as_ptr() }, avail, total, free) }
}

// -----| named-pipe queries |-----

#[no_mangle]
pub unsafe extern "system" fn GetNamedPipeInfo(_h: Handle, flags: *mut u32, out_size: *mut u32, in_size: *mut u32, max: *mut u32) -> Bool {
    for (p, v) in [(flags, 0), (out_size, 65536), (in_size, 65536), (max, 1)] {
        if !p.is_null() {
            unsafe { *p = v };
        }
    }
    1
}

// -----| networking and security, until there are sockets (S4) |-----

/// Enough for a library that sets up networking whether or not it uses it.
/// WSADATA: wVersion, wHighVersion, then text.
#[no_mangle]
pub unsafe extern "system" fn WSAStartup(version: u16, data: *mut u8) -> i32 {
    if !data.is_null() {
        unsafe {
            core::ptr::write_bytes(data, 0, 408);
            *(data as *mut u16) = version.min(0x0202);
            *(data as *mut u16).add(1) = 0x0202;
        }
    }
    0
}

#[no_mangle]
pub extern "system" fn WSACleanup() -> i32 {
    0
}

static WSA_ERROR: core::sync::atomic::AtomicI32 = core::sync::atomic::AtomicI32::new(0);

#[no_mangle]
pub extern "system" fn WSAGetLastError() -> i32 {
    WSA_ERROR.load(core::sync::atomic::Ordering::Relaxed)
}

#[no_mangle]
pub extern "system" fn WSASetLastError(e: i32) {
    WSA_ERROR.store(e, core::sync::atomic::Ordering::Relaxed);
}

// -----| the profile folder |-----

/// userenv: the one user's home.
#[no_mangle]
pub unsafe extern "system" fn GetUserProfileDirectoryW(_token: Handle, out: *mut u16, size: *mut u32) -> Bool {
    const HOME: &[u8] = b"C:\\home";
    let need = HOME.len() as u32 + 1;
    let cap = unsafe { *size };
    unsafe { *size = need };
    if out.is_null() || cap < need {
        crate::SetLastError(ERROR_INSUFFICIENT_BUFFER);
        return 0;
    }
    for (i, &b) in HOME.iter().enumerate() {
        unsafe { *out.add(i) = b as u16 };
    }
    unsafe { *out.add(HOME.len()) = 0 };
    1
}

// -----| oleaut32: BSTRs, and no error objects |-----

#[no_mangle]
pub unsafe extern "system" fn SysAllocStringLen(s: *const u16, len: u32) -> *mut u16 {
    let block = unsafe { crate::mem::HeapAlloc(crate::mem::GetProcessHeap(), 0, 4 + (len as usize + 1) * 2) };
    if block.is_null() {
        return core::ptr::null_mut();
    }
    unsafe {
        *(block as *mut u32) = len * 2;
        let out = block.add(4) as *mut u16;
        if !s.is_null() {
            core::ptr::copy_nonoverlapping(s, out, len as usize);
        }
        *out.add(len as usize) = 0;
        out
    }
}

#[no_mangle]
pub unsafe extern "system" fn SysFreeString(s: *mut u16) {
    if !s.is_null() {
        unsafe { crate::mem::HeapFree(crate::mem::GetProcessHeap(), 0, (s as *mut u8).sub(4)) };
    }
}

#[no_mangle]
pub unsafe extern "system" fn SysStringLen(s: *const u16) -> u32 {
    if s.is_null() {
        return 0;
    }
    unsafe { *((s as *const u8).sub(4) as *const u32) / 2 }
}

#[no_mangle]
pub unsafe extern "system" fn GetErrorInfo(_reserved: u32, out: *mut usize) -> i32 {
    unsafe { *out = 0 };
    1 // S_FALSE: there isn't one.
}

#[no_mangle]
pub extern "system" fn SetErrorInfo(_reserved: u32, _info: usize) -> i32 {
    0
}
