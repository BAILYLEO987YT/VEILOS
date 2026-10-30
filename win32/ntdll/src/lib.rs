//! VEILOS `ntdll.dll`: the bottom of the user-mode Windows stack.
//!
//! The `Nt*`/`Zw*` exports are system call stubs written exactly like
//! Windows' own (`mov r10, rcx; mov eax, ID; syscall; ret`), so arguments past
//! the fourth are where the kernel expects them on the caller's stack. The
//! numbers come from `nt_abi`. `Rtl*` helpers run entirely in user mode.

#![no_std]

use core::arch::naked_asm;
use nt_abi::{status, sys, NtStatus};

/// Export `$nt` and `$zw` as stubs for syscall `$id`.
macro_rules! syscall_stubs {
    ($($nt:ident, $zw:ident => $id:path;)*) => {$(
        #[unsafe(naked)]
        #[no_mangle]
        pub unsafe extern "system" fn $nt() {
            naked_asm!("mov r10, rcx", "mov eax, {id}", "syscall", "ret", id = const $id)
        }
        #[unsafe(naked)]
        #[no_mangle]
        pub unsafe extern "system" fn $zw() {
            naked_asm!("mov r10, rcx", "mov eax, {id}", "syscall", "ret", id = const $id)
        }
    )*};
}

syscall_stubs! {
    NtTerminateProcess, ZwTerminateProcess => sys::NT_TERMINATE_PROCESS;
    NtWriteFile, ZwWriteFile => sys::NT_WRITE_FILE;
    NtClose, ZwClose => sys::NT_CLOSE;
}

/// The Win32 error code for an NTSTATUS (the common ones; anything else maps
/// to ERROR_MR_MID_NOT_FOUND, as Windows does for codes it doesn't know).
#[no_mangle]
pub extern "system" fn RtlNtStatusToDosError(s: NtStatus) -> u32 {
    match s {
        status::SUCCESS => 0,              // ERROR_SUCCESS
        status::NOT_IMPLEMENTED => 1,      // ERROR_INVALID_FUNCTION
        status::INVALID_HANDLE => 6,       // ERROR_INVALID_HANDLE
        status::OBJECT_TYPE_MISMATCH => 6, // ERROR_INVALID_HANDLE
        status::INVALID_PARAMETER => 87,   // ERROR_INVALID_PARAMETER
        status::ACCESS_VIOLATION => 998,   // ERROR_NOACCESS
        _ => 317,                          // ERROR_MR_MID_NOT_FOUND
    }
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
    loop {}
}
