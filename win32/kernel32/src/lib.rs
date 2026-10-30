//! VEILOS `kernel32.dll`: the Win32 base API, built on `ntdll.dll`.
//!
//! Functions not here yet are still provided by the kernel's built-in
//! kernel32 (the loader falls back to it for any name this DLL doesn't
//! export), so this grows one area at a time (WIN32_DESIGN.md).

#![no_std]

use core::arch::asm;
use nt_abi::{layout, IoStatusBlock, NtStatus, CURRENT_PROCESS};

type Handle = u64;
type Bool = i32;

const INVALID_HANDLE_VALUE: Handle = u64::MAX;
/// GetStdHandle's arguments: (DWORD)-10, -11, -12.
const STD_INPUT_HANDLE: u32 = -10i32 as u32;
const STD_OUTPUT_HANDLE: u32 = -11i32 as u32;
const STD_ERROR_HANDLE: u32 = -12i32 as u32;

#[link(name = "ntdll", kind = "raw-dylib")]
unsafe extern "system" {
    fn NtTerminateProcess(process: Handle, status: NtStatus) -> NtStatus;
    fn NtWriteFile(file: Handle, event: Handle, apc: usize, apc_context: usize, iosb: *mut IoStatusBlock,
                   buffer: *const u8, length: u32, offset: *const i64, key: *const u32) -> NtStatus;
    fn NtClose(handle: Handle) -> NtStatus;
    fn RtlNtStatusToDosError(status: NtStatus) -> u32;
}

/// This thread's TEB.
fn teb() -> *mut u8 {
    let t: *mut u8;
    unsafe { asm!("mov {}, gs:[0x30]", out(reg) t, options(nostack, readonly, preserves_flags)) };
    t
}

fn process_parameters() -> *mut u8 {
    unsafe {
        let peb = *(teb().add(layout::TEB_PEB) as *const *mut u8);
        *(peb.add(layout::PEB_PROCESS_PARAMETERS) as *const *mut u8)
    }
}

/// Record a failed NTSTATUS as this thread's last error; returns FALSE.
fn fail(status: NtStatus) -> Bool {
    SetLastError(unsafe { RtlNtStatusToDosError(status) });
    0
}

#[no_mangle]
pub extern "system" fn GetLastError() -> u32 {
    unsafe { *(teb().add(layout::TEB_LAST_ERROR) as *const u32) }
}

#[no_mangle]
pub extern "system" fn SetLastError(code: u32) {
    unsafe { *(teb().add(layout::TEB_LAST_ERROR) as *mut u32) = code }
}

#[no_mangle]
pub extern "system" fn GetCurrentProcess() -> Handle {
    CURRENT_PROCESS
}

#[no_mangle]
pub extern "system" fn GetCurrentProcessId() -> u32 {
    unsafe { *(teb().add(layout::TEB_PROCESS_ID) as *const u64) as u32 }
}

#[no_mangle]
pub extern "system" fn GetStdHandle(which: u32) -> Handle {
    let offset = match which {
        STD_INPUT_HANDLE => layout::PARAMS_STD_INPUT,
        STD_OUTPUT_HANDLE => layout::PARAMS_STD_OUTPUT,
        STD_ERROR_HANDLE => layout::PARAMS_STD_ERROR,
        _ => {
            SetLastError(6); // ERROR_INVALID_HANDLE
            return INVALID_HANDLE_VALUE;
        }
    };
    unsafe { *(process_parameters().add(offset) as *const Handle) }
}

#[no_mangle]
pub unsafe extern "system" fn WriteFile(file: Handle, buffer: *const u8, length: u32, written: *mut u32, overlapped: usize) -> Bool {
    if !written.is_null() {
        unsafe { *written = 0 };
    }
    if overlapped != 0 {
        return fail(nt_abi::status::NOT_IMPLEMENTED); // overlapped I/O comes later
    }
    let mut iosb = IoStatusBlock::default();
    let s = unsafe { NtWriteFile(file, 0, 0, 0, &mut iosb, buffer, length, core::ptr::null(), core::ptr::null()) };
    if !nt_abi::status::is_success(s) {
        return fail(s);
    }
    if !written.is_null() {
        unsafe { *written = iosb.information as u32 };
    }
    1
}

#[no_mangle]
pub unsafe extern "system" fn WriteConsoleA(console: Handle, buffer: *const u8, length: u32, written: *mut u32, _reserved: usize) -> Bool {
    unsafe { WriteFile(console, buffer, length, written, 0) }
}

/// UTF-16 in, UTF-8 to the console; `written` counts UTF-16 units.
#[no_mangle]
pub unsafe extern "system" fn WriteConsoleW(console: Handle, buffer: *const u16, length: u32, written: *mut u32, _reserved: usize) -> Bool {
    let units = unsafe { core::slice::from_raw_parts(buffer, length as usize) };
    let mut out = [0u8; 512];
    let mut n = 0;
    for c in char::decode_utf16(units.iter().copied()) {
        let c = c.unwrap_or(char::REPLACEMENT_CHARACTER);
        if n + 4 > out.len() {
            if unsafe { WriteFile(console, out.as_ptr(), n as u32, core::ptr::null_mut(), 0) } == 0 {
                return 0;
            }
            n = 0;
        }
        n += c.encode_utf8(&mut out[n..]).len();
    }
    if n > 0 && unsafe { WriteFile(console, out.as_ptr(), n as u32, core::ptr::null_mut(), 0) } == 0 {
        return 0;
    }
    if !written.is_null() {
        unsafe { *written = length };
    }
    1
}

#[no_mangle]
pub extern "system" fn CloseHandle(handle: Handle) -> Bool {
    let s = unsafe { NtClose(handle) };
    if nt_abi::status::is_success(s) { 1 } else { fail(s) }
}

#[no_mangle]
pub extern "system" fn ExitProcess(code: u32) -> ! {
    unsafe { NtTerminateProcess(CURRENT_PROCESS, code as NtStatus) };
    loop {}
}

#[no_mangle]
pub extern "system" fn DllMain(_module: usize, _reason: u32, _reserved: usize) -> i32 {
    1
}

/// Normally from the MSVC C runtime: marks that floating point is used.
#[no_mangle]
static _fltused: i32 = 0;

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    ExitProcess(0xDEAD)
}
