//! The syscall interface between the VEILOS kernel and VEILOS's own
//! `ntdll.dll` (WIN32_DESIGN.md). Both sides build from these numbers and
//! layouts, so they can't drift apart.
//!
//! Calling convention (as NT's): `mov r10, rcx; mov eax, ID; syscall; ret`,
//! arguments 1-4 in r10/rdx/r8/r9, the rest on the caller's stack from
//! `[rsp + 0x28]` at the `syscall`; the NTSTATUS comes back in rax.

#![no_std]

/// NTSTATUS values used on both sides.
pub type NtStatus = i32;

pub mod status {
    use super::NtStatus;
    pub const SUCCESS: NtStatus = 0;
    pub const NOT_IMPLEMENTED: NtStatus = 0xC000_0002u32 as i32;
    pub const INVALID_HANDLE: NtStatus = 0xC000_0008u32 as i32;
    pub const INVALID_PARAMETER: NtStatus = 0xC000_000Du32 as i32;
    pub const ACCESS_VIOLATION: NtStatus = 0xC000_0005u32 as i32;
    pub const OBJECT_TYPE_MISMATCH: NtStatus = 0xC000_0024u32 as i32;
    pub const END_OF_FILE: NtStatus = 0xC000_0011u32 as i32;

    /// True for success and informational codes.
    pub const fn is_success(s: NtStatus) -> bool {
        s >= 0
    }
}

/// Syscall numbers. They sit above the kernel's built-in Win32 functions
/// (below 0x1_0000) and the loader's unresolved-import stubs (0x0100_0000+).
pub mod sys {
    pub const BASE: u32 = 0x0200_0000;
    pub const NT_TERMINATE_PROCESS: u32 = BASE;
    pub const NT_WRITE_FILE: u32 = BASE + 1;
    pub const NT_CLOSE: u32 = BASE + 2;
    pub const NT_READ_FILE: u32 = BASE + 3;
    /// One past the last number in use.
    pub const END: u32 = BASE + 4;
}

/// The pseudo-handle for "this process" (NtCurrentProcess()).
pub const CURRENT_PROCESS: u64 = u64::MAX;

/// IO_STATUS_BLOCK: the outcome of an I/O call.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct IoStatusBlock {
    /// NTSTATUS (a union with a pointer, hence 8 bytes).
    pub status: u64,
    /// Bytes transferred.
    pub information: u64,
}

/// Field offsets in the NT structures the loader fills and the DLLs read.
pub mod layout {
    /// TEB: ClientId.UniqueProcess, the PEB pointer, LastErrorValue.
    pub const TEB_PROCESS_ID: usize = 0x40;
    pub const TEB_PEB: usize = 0x60;
    pub const TEB_LAST_ERROR: usize = 0x68;
    /// PEB: the RTL_USER_PROCESS_PARAMETERS pointer.
    pub const PEB_PROCESS_PARAMETERS: usize = 0x20;
    /// RTL_USER_PROCESS_PARAMETERS: the standard handles.
    pub const PARAMS_STD_INPUT: usize = 0x20;
    pub const PARAMS_STD_OUTPUT: usize = 0x28;
    pub const PARAMS_STD_ERROR: usize = 0x30;
}
