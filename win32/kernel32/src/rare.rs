//! Calls the MSYS2/Cygwin runtime (Git Bash's msys-2.0.dll) imports that
//! VEILOS answers simply: serial ports and tape drives (none), volumes and
//! drives (one, C:), IDN names (ASCII ones pass), another process's memory
//! (through the NT calls), and a few process and console questions.

use crate::{Bool, Handle};
use nt_abi::status;

const ERROR_NOT_SUPPORTED: u32 = 50;

#[link(name = "ntdll", kind = "raw-dylib")]
unsafe extern "system" {
    fn NtWriteVirtualMemory(process: Handle, address: usize, buffer: *const u8, size: usize, written: *mut usize) -> i32;
}

fn unsupported() -> Bool {
    crate::SetLastError(ERROR_NOT_SUPPORTED);
    0
}

/// Calls that answer a Win32 error number themselves (the tape functions).
fn unsupported_code() -> u32 {
    ERROR_NOT_SUPPORTED
}

// -----| serial ports: none |-----

#[no_mangle]
pub extern "system" fn ClearCommBreak(_h: Handle) -> Bool { unsupported() }
#[no_mangle]
pub extern "system" fn ClearCommError(_h: Handle, _errors: *mut u32, _stat: *mut u8) -> Bool { unsupported() }
#[no_mangle]
pub extern "system" fn EscapeCommFunction(_h: Handle, _f: u32) -> Bool { unsupported() }
#[no_mangle]
pub extern "system" fn GetCommModemStatus(_h: Handle, _stat: *mut u32) -> Bool { unsupported() }
#[no_mangle]
pub extern "system" fn GetCommState(_h: Handle, _dcb: *mut u8) -> Bool { unsupported() }
#[no_mangle]
pub extern "system" fn PurgeComm(_h: Handle, _flags: u32) -> Bool { unsupported() }
#[no_mangle]
pub extern "system" fn SetCommBreak(_h: Handle) -> Bool { unsupported() }
#[no_mangle]
pub extern "system" fn SetCommMask(_h: Handle, _mask: u32) -> Bool { unsupported() }
#[no_mangle]
pub extern "system" fn SetCommState(_h: Handle, _dcb: *const u8) -> Bool { unsupported() }
#[no_mangle]
pub extern "system" fn SetCommTimeouts(_h: Handle, _t: *const u8) -> Bool { unsupported() }
#[no_mangle]
pub extern "system" fn TransmitCommChar(_h: Handle, _c: u8) -> Bool { unsupported() }
#[no_mangle]
pub extern "system" fn WaitCommEvent(_h: Handle, _mask: *mut u32, _ov: *mut u8) -> Bool { unsupported() }

// -----| tape drives: none |-----

#[no_mangle]
pub extern "system" fn CreateTapePartition(_h: Handle, _method: u32, _count: u32, _size: u32) -> u32 { unsupported_code() }
#[no_mangle]
pub extern "system" fn EraseTape(_h: Handle, _kind: u32, _immediate: Bool) -> u32 { unsupported_code() }
#[no_mangle]
pub extern "system" fn GetTapeParameters(_h: Handle, _op: u32, _size: *mut u32, _info: *mut u8) -> u32 { unsupported_code() }
#[no_mangle]
pub extern "system" fn GetTapePosition(_h: Handle, _kind: u32, _part: *mut u32, _lo: *mut u32, _hi: *mut u32) -> u32 { unsupported_code() }
#[no_mangle]
pub extern "system" fn GetTapeStatus(_h: Handle) -> u32 { unsupported_code() }
#[no_mangle]
pub extern "system" fn PrepareTape(_h: Handle, _op: u32, _immediate: Bool) -> u32 { unsupported_code() }
#[no_mangle]
pub extern "system" fn SetTapeParameters(_h: Handle, _op: u32, _info: *const u8) -> u32 { unsupported_code() }
#[no_mangle]
pub extern "system" fn SetTapePosition(_h: Handle, _method: u32, _part: u32, _lo: u32, _hi: u32, _immediate: Bool) -> u32 { unsupported_code() }
#[no_mangle]
pub extern "system" fn WriteTapemark(_h: Handle, _kind: u32, _count: u32, _immediate: Bool) -> u32 { unsupported_code() }

// -----| drives and volumes: C: |-----

/// "C:\" and the final NUL: the length without that NUL, or what's needed.
#[no_mangle]
pub unsafe extern "system" fn GetLogicalDriveStringsW(size: u32, out: *mut u16) -> u32 {
    let s: [u16; 5] = [b'C' as u16, b':' as u16, b'\\' as u16, 0, 0];
    if (size as usize) < s.len() || out.is_null() {
        return s.len() as u32;
    }
    unsafe { core::ptr::copy_nonoverlapping(s.as_ptr(), out, s.len()) };
    4
}

#[no_mangle]
pub unsafe extern "system" fn GetLogicalDriveStringsA(size: u32, out: *mut u8) -> u32 {
    let s = *b"C:\\\0\0";
    if (size as usize) < s.len() || out.is_null() {
        return s.len() as u32;
    }
    unsafe { core::ptr::copy_nonoverlapping(s.as_ptr(), out, s.len()) };
    4
}

#[no_mangle]
pub extern "system" fn GetVolumeNameForVolumeMountPointW(_mount: *const u16, _out: *mut u16, _size: u32) -> Bool { unsupported() }
#[no_mangle]
pub extern "system" fn GetVolumePathNamesForVolumeNameW(_volume: *const u16, _out: *mut u16, _size: u32, _ret: *mut u32) -> Bool { unsupported() }

// -----| IDN names: ASCII ones are already themselves |-----

unsafe fn idn_copy(input: *const u16, len: i32, out: *mut u16, size: i32) -> i32 {
    let n = if len < 0 { (unsafe { crate::util::wlen(input) }) as i32 + 1 } else { len };
    let src = unsafe { core::slice::from_raw_parts(input, n as usize) };
    if src.iter().any(|&c| c >= 0x80) {
        crate::SetLastError(1113); // ERROR_NO_UNICODE_TRANSLATION
        return 0;
    }
    if size == 0 || out.is_null() {
        return n;
    }
    if size < n {
        crate::SetLastError(122);
        return 0;
    }
    unsafe { core::ptr::copy_nonoverlapping(src.as_ptr(), out, n as usize) };
    n
}

#[no_mangle]
pub unsafe extern "system" fn IdnToAscii(_flags: u32, input: *const u16, len: i32, out: *mut u16, size: i32) -> i32 {
    unsafe { idn_copy(input, len, out, size) }
}

#[no_mangle]
pub unsafe extern "system" fn IdnToUnicode(_flags: u32, input: *const u16, len: i32, out: *mut u16, size: i32) -> i32 {
    unsafe { idn_copy(input, len, out, size) }
}

// -----| locales |-----

/// EnumSystemLocalesEx(callback, flags, lparam, reserved): the one locale.
#[no_mangle]
pub unsafe extern "system" fn EnumSystemLocalesEx(callback: u64, flags: u32, lparam: isize, _reserved: usize) -> Bool {
    if callback == 0 {
        crate::SetLastError(87);
        return 0;
    }
    let name: [u16; 6] = [b'e' as u16, b'n' as u16, b'-' as u16, b'U' as u16, b'S' as u16, 0];
    let f: unsafe extern "system" fn(*const u16, u32, isize) -> Bool = unsafe { core::mem::transmute(callback) };
    unsafe { f(name.as_ptr(), flags, lparam) };
    1
}

// -----| processes and memory |-----

#[no_mangle]
pub unsafe extern "system" fn WriteProcessMemory(p: Handle, addr: usize, buf: *const u8, n: usize, written: *mut usize) -> Bool {
    let mut done = 0usize;
    let s = unsafe { NtWriteVirtualMemory(p, addr, buf, n, &mut done) };
    if !written.is_null() {
        unsafe { *written = done };
    }
    if s == status::SUCCESS { 1 } else { crate::fail(s) }
}

#[no_mangle]
pub unsafe extern "system" fn VirtualQueryEx(p: Handle, address: usize, info: *mut u8, length: usize) -> usize {
    let mut ret = 0usize;
    let s = unsafe { crate::NtQueryVirtualMemory(p, address, 0, info, length, &mut ret) };
    if status::is_success(s) {
        ret
    } else {
        crate::fail(s);
        0
    }
}

#[no_mangle]
pub unsafe extern "system" fn VirtualProtectEx(p: Handle, address: usize, size: usize, protect: u32, old: *mut u32) -> Bool {
    let (mut base, mut len) = (address, size);
    let mut was = 0u32;
    let s = unsafe { crate::NtProtectVirtualMemory(p, &mut base, &mut len, protect, &mut was) };
    if !old.is_null() {
        unsafe { *old = was };
    }
    crate::check(s)
}

/// Every page is already where it's going to be.
#[no_mangle]
pub extern "system" fn PrefetchVirtualMemory(_p: Handle, _count: usize, _ranges: *const u8, _flags: u32) -> Bool {
    1
}

/// MODULEINFO: base, SizeOfImage, entry point (from the module's headers).
#[no_mangle]
pub unsafe extern "system" fn K32GetModuleInformation(_p: Handle, module: Handle, info: *mut u8, size: u32) -> Bool {
    if size < 24 || info.is_null() {
        crate::SetLastError(122);
        return 0;
    }
    let base = if module == 0 { unsafe { crate::module::GetModuleHandleW(core::ptr::null()) } } else { module } as usize;
    unsafe {
        let nt = base + core::ptr::read_unaligned((base + 0x3C) as *const u32) as usize;
        let image_size = core::ptr::read_unaligned((nt + 0x18 + 56) as *const u32);
        let entry = core::ptr::read_unaligned((nt + 0x18 + 16) as *const u32);
        core::ptr::write_unaligned(info as *mut u64, base as u64);
        core::ptr::write_unaligned(info.add(8) as *mut u32, image_size);
        core::ptr::write_unaligned(info.add(16) as *mut u64, if entry != 0 { (base + entry as usize) as u64 } else { 0 });
    }
    1
}

#[no_mangle]
pub unsafe extern "system" fn GetProcessWorkingSetSize(_p: Handle, min: *mut usize, max: *mut usize) -> Bool {
    unsafe {
        *min = 200 * 4096;
        *max = 1380 * 4096;
    }
    1
}

#[no_mangle]
pub extern "system" fn SetProcessWorkingSetSize(_p: Handle, _min: usize, _max: usize) -> Bool {
    1
}

#[no_mangle]
pub extern "system" fn QueryInformationJobObject(_job: Handle, _class: u32, _info: *mut u8, _len: u32, _ret: *mut u32) -> Bool {
    unsupported()
}

#[no_mangle]
pub extern "system" fn Process32First(_snap: Handle, _entry: *mut u8) -> Bool {
    crate::SetLastError(18);
    0
}

#[no_mangle]
pub extern "system" fn Process32Next(_snap: Handle, _entry: *mut u8) -> Bool {
    crate::SetLastError(18);
    0
}

/// The processes on this console: here, this one.
#[no_mangle]
pub unsafe extern "system" fn GetConsoleProcessList(list: *mut u32, count: u32) -> u32 {
    if count >= 1 && !list.is_null() {
        unsafe { *list = crate::GetCurrentProcessId() };
    }
    1
}

/// CONSOLE_FONT_INFOEX: an 8x16 font.
#[no_mangle]
pub unsafe extern "system" fn GetCurrentConsoleFontEx(_h: Handle, _max: Bool, info: *mut u8) -> Bool {
    if info.is_null() {
        crate::SetLastError(87);
        return 0;
    }
    unsafe {
        let size = core::ptr::read_unaligned(info as *const u32) as usize;
        core::ptr::write_bytes(info.add(4), 0, size.saturating_sub(4).min(80));
        core::ptr::write_unaligned(info.add(8) as *mut i16, 8);
        core::ptr::write_unaligned(info.add(10) as *mut i16, 16);
        core::ptr::write_unaligned(info.add(16) as *mut u32, 400); // FontWeight
    }
    1
}

/// PROCESSOR_NUMBER: group 0, this CPU.
#[no_mangle]
pub unsafe extern "system" fn GetCurrentProcessorNumberEx(out: *mut u8) {
    unsafe {
        core::ptr::write_unaligned(out as *mut u16, 0);
        *out.add(2) = crate::toolchain::GetCurrentProcessorNumber() as u8;
        *out.add(3) = 0;
    }
}

#[no_mangle]
pub extern "system" fn SetThreadGroupAffinity(_thread: Handle, _new: *const u8, _old: *mut u8) -> Bool {
    1
}

/// Interrupt time not counting sleep: there is no sleep, so the tick count.
#[no_mangle]
pub unsafe extern "system" fn QueryUnbiasedInterruptTime(out: *mut u64) -> Bool {
    unsafe { *out = crate::time::GetTickCount64() * 10_000 };
    1
}

#[no_mangle]
pub extern "system" fn GetBinaryTypeW(_name: *const u16, _kind: *mut u32) -> Bool { unsupported() }
#[no_mangle]
pub extern "system" fn CallNamedPipeA(_name: *const u8, _in: *const u8, _in_len: u32, _out: *mut u8, _out_len: u32, _read: *mut u32, _timeout: u32) -> Bool { unsupported() }
#[no_mangle]
pub extern "system" fn SetComputerNameExW(_format: u32, _name: *const u16) -> Bool { unsupported() }
#[no_mangle]
pub extern "system" fn SetSystemTime(_time: *const u8) -> Bool {
    crate::SetLastError(1314); // ERROR_PRIVILEGE_NOT_HELD
    0
}

/// Changing another thread's registers isn't offered (threads can't be suspended yet).
#[no_mangle]
pub extern "system" fn SetThreadContext(_thread: Handle, _context: *const u8) -> Bool { unsupported() }


// -----| what Cygwin loads from KernelBase by name |-----

/// Interrupt time in 100 ns units, from the performance counter.
fn interrupt_time() -> u64 {
    let (mut count, mut freq) = (0i64, 0i64);
    unsafe {
        crate::time::QueryPerformanceCounter(&mut count);
        crate::time::QueryPerformanceFrequency(&mut freq);
    }
    if freq <= 0 {
        return crate::time::GetTickCount64() * 10_000;
    }
    ((count as u128) * 10_000_000 / freq as u128) as u64
}

#[no_mangle]
pub unsafe extern "system" fn QueryInterruptTime(out: *mut u64) {
    unsafe { *out = interrupt_time() };
}

#[no_mangle]
pub unsafe extern "system" fn QueryInterruptTimePrecise(out: *mut u64) {
    unsafe { *out = interrupt_time() };
}

#[no_mangle]
pub unsafe extern "system" fn QueryUnbiasedInterruptTimePrecise(out: *mut u64) {
    unsafe { *out = interrupt_time() };
}

/// VirtualAlloc2(process, base, size, type, protect, params, count): where
/// it may go, and its alignment, come from the extended parameters.
#[no_mangle]
pub unsafe extern "system" fn VirtualAlloc2(process: Handle, address: usize, size: usize, kind: u32, protect: u32, params: *const u8, count: u32) -> usize {
    #[link(name = "ntdll", kind = "raw-dylib")]
    unsafe extern "system" {
        fn NtAllocateVirtualMemoryEx(process: Handle, base: *mut usize, size: *mut usize, kind: u32, protect: u32, params: *const u8, count: u32) -> i32;
    }
    let (mut base, mut len) = (address, size);
    let p = if process == 0 { u64::MAX } else { process };
    let s = unsafe { NtAllocateVirtualMemoryEx(p, &mut base, &mut len, kind, protect, params, count) };
    if status::is_success(s) {
        base
    } else {
        crate::fail(s);
        0
    }
}

/// Discarded pages keep their contents here; that's allowed.
#[no_mangle]
pub extern "system" fn DiscardVirtualMemory(_base: usize, _size: usize) -> u32 {
    0
}

#[no_mangle]
pub extern "system" fn GetProcessInformation(_p: Handle, _class: u32, _info: *mut u8, _size: u32) -> Bool {
    unsupported()
}

/// Not running under WOW64: the process machine is "unknown", the native one x64.
#[no_mangle]
pub unsafe extern "system" fn IsWow64Process2(_p: Handle, process_machine: *mut u16, native_machine: *mut u16) -> Bool {
    unsafe {
        *process_machine = 0;
        if !native_machine.is_null() {
            *native_machine = 0x8664;
        }
    }
    1
}

#[no_mangle]
pub extern "system" fn ContinueDebugEvent(_pid: u32, _tid: u32, _status: u32) -> Bool { unsupported() }
#[no_mangle]
pub extern "system" fn ImpersonateLoggedOnUser(_token: Handle) -> Bool { unsupported() }
#[no_mangle]
pub extern "system" fn ImpersonateNamedPipeClient(_pipe: Handle) -> Bool { unsupported() }

/// Registry calls answering an error number.
#[no_mangle]
pub extern "system" fn RegGetKeySecurity(_key: Handle, _info: u32, _sd: *mut u8, _len: *mut u32) -> u32 { ERROR_NOT_SUPPORTED }
#[no_mangle]
pub extern "system" fn RegOpenUserClassesRoot(_token: Handle, _options: u32, _access: u32, _out: *mut Handle) -> u32 { ERROR_NOT_SUPPORTED }

// -----| iphlpapi: what Cygwin asks for |-----

/// FIXED_INFO: the host name (as GetComputerName), no domain, and QEMU's
/// DNS server. ERROR_BUFFER_OVERFLOW with the size if it doesn't fit.
#[no_mangle]
pub unsafe extern "system" fn GetNetworkParams(info: *mut u8, len: *mut u32) -> u32 {
    const SIZE: usize = 600;
    if len.is_null() {
        return 87;
    }
    if info.is_null() || (unsafe { *len } as usize) < SIZE {
        unsafe { *len = SIZE as u32 };
        return 111; // ERROR_BUFFER_OVERFLOW
    }
    unsafe {
        core::ptr::write_bytes(info, 0, SIZE);
        let mut name = [0u16; 32];
        let mut n = name.len() as u32;
        if crate::legacy::GetComputerNameW(name.as_mut_ptr(), &mut n) != 0 {
            for i in 0..(n as usize).min(131) {
                *info.add(i) = name[i] as u8;
            }
        }
        // CurrentDnsServer points at DnsServerList (at 272): IpAddress at +8.
        core::ptr::write_unaligned(info.add(264) as *mut u64, info.add(272) as u64);
        let dns = b"10.0.2.3";
        core::ptr::copy_nonoverlapping(dns.as_ptr(), info.add(280), dns.len());
        core::ptr::write_unaligned(info.add(320) as *mut u32, 1); // NodeType: broadcast
    }
    0
}
