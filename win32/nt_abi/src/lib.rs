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
    pub const TIMEOUT: NtStatus = 0x0000_0102;
    pub const NO_MORE_FILES: NtStatus = 0x8000_0006u32 as i32;
    pub const NOT_IMPLEMENTED: NtStatus = 0xC000_0002u32 as i32;
    pub const INVALID_INFO_CLASS: NtStatus = 0xC000_0003u32 as i32;
    pub const ACCESS_VIOLATION: NtStatus = 0xC000_0005u32 as i32;
    pub const INVALID_HANDLE: NtStatus = 0xC000_0008u32 as i32;
    pub const INVALID_PARAMETER: NtStatus = 0xC000_000Du32 as i32;
    pub const END_OF_FILE: NtStatus = 0xC000_0011u32 as i32;
    pub const NO_MEMORY: NtStatus = 0xC000_0017u32 as i32;
    pub const ACCESS_DENIED: NtStatus = 0xC000_0022u32 as i32;
    pub const BUFFER_TOO_SMALL: NtStatus = 0xC000_0023u32 as i32;
    pub const OBJECT_TYPE_MISMATCH: NtStatus = 0xC000_0024u32 as i32;
    pub const OBJECT_NAME_INVALID: NtStatus = 0xC000_0033u32 as i32;
    pub const OBJECT_NAME_NOT_FOUND: NtStatus = 0xC000_0034u32 as i32;
    pub const OBJECT_NAME_COLLISION: NtStatus = 0xC000_0035u32 as i32;
    pub const OBJECT_PATH_NOT_FOUND: NtStatus = 0xC000_003Au32 as i32;
    pub const FILE_IS_A_DIRECTORY: NtStatus = 0xC000_00BAu32 as i32;
    pub const DIRECTORY_NOT_EMPTY: NtStatus = 0xC000_0101u32 as i32;
    pub const NOT_A_DIRECTORY: NtStatus = 0xC000_0103u32 as i32;
    pub const DLL_NOT_FOUND: NtStatus = 0xC000_0135u32 as i32;
    pub const PROCEDURE_NOT_FOUND: NtStatus = 0xC000_007Au32 as i32;

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
    pub const NT_DELAY_EXECUTION: u32 = BASE + 4;
    pub const NT_ALLOCATE_VIRTUAL_MEMORY: u32 = BASE + 5;
    pub const NT_FREE_VIRTUAL_MEMORY: u32 = BASE + 6;
    pub const NT_PROTECT_VIRTUAL_MEMORY: u32 = BASE + 7;
    pub const NT_QUERY_VIRTUAL_MEMORY: u32 = BASE + 8;
    pub const NT_CREATE_FILE: u32 = BASE + 9;
    pub const NT_QUERY_INFORMATION_FILE: u32 = BASE + 10;
    pub const NT_SET_INFORMATION_FILE: u32 = BASE + 11;
    pub const NT_QUERY_DIRECTORY_FILE: u32 = BASE + 12;
    pub const NT_QUERY_ATTRIBUTES_FILE: u32 = BASE + 13;
    pub const NT_DELETE_FILE: u32 = BASE + 14;
    pub const NT_FLUSH_BUFFERS_FILE: u32 = BASE + 15;
    pub const NT_CREATE_EVENT: u32 = BASE + 16;
    pub const NT_SET_EVENT: u32 = BASE + 17;
    pub const NT_RESET_EVENT: u32 = BASE + 18;
    pub const NT_WAIT_FOR_SINGLE_OBJECT: u32 = BASE + 19;
    pub const NT_YIELD_EXECUTION: u32 = BASE + 20;
    /// VEILOS-specific: load a DLL into the calling process
    /// (name: *const u16, name_chars, base_out: *mut u64,
    ///  inits: *mut InitCall, inits_capacity, inits_count_out: *mut u32).
    /// The caller runs the returned init calls (TLS callbacks, DllMain).
    pub const VEIL_LOAD_DLL: u32 = BASE + 21;
    /// VEILOS-specific: an export of a loaded module, following forwarders
    /// and falling back to the kernel's built-ins
    /// (module_base, name: *const u8 or an ordinal below 0x10000, address_out: *mut u64).
    pub const VEIL_GET_PROC_ADDRESS: u32 = BASE + 22;
    /// One past the last number in use.
    pub const END: u32 = BASE + 23;
}

/// The pseudo-handle for "this process" (NtCurrentProcess()).
pub const CURRENT_PROCESS: u64 = u64::MAX;

/// IO_STATUS_BLOCK: the outcome of an I/O call.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct IoStatusBlock {
    /// NTSTATUS (a union with a pointer, hence 8 bytes).
    pub status: u64,
    /// Bytes transferred, or a FILE_* disposition result.
    pub information: u64,
}

/// One init call returned by VEIL_LOAD_DLL: `routine(module, DLL_PROCESS_ATTACH, 0)`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct InitCall {
    pub routine: u64,
    pub module: u64,
}

/// UNICODE_STRING (x64).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct UnicodeString {
    /// Bytes, without a terminator.
    pub length: u16,
    pub maximum_length: u16,
    pub buffer: *const u16,
}

/// OBJECT_ATTRIBUTES (x64).
#[repr(C)]
pub struct ObjectAttributes {
    pub length: u32,
    pub root_directory: u64,
    pub object_name: *const UnicodeString,
    pub attributes: u32,
    pub security_descriptor: usize,
    pub security_quality_of_service: usize,
}

/// NtCreateFile values.
pub mod file {
    // CreateDisposition
    pub const SUPERSEDE: u32 = 0;
    pub const OPEN: u32 = 1;
    pub const CREATE: u32 = 2;
    pub const OPEN_IF: u32 = 3;
    pub const OVERWRITE: u32 = 4;
    pub const OVERWRITE_IF: u32 = 5;
    // CreateOptions
    pub const DIRECTORY_FILE: u32 = 0x0001;
    pub const NON_DIRECTORY_FILE: u32 = 0x0040;
    pub const DELETE_ON_CLOSE: u32 = 0x1000;
    // IO_STATUS_BLOCK.Information after a create
    pub const SUPERSEDED: u64 = 0;
    pub const OPENED: u64 = 1;
    pub const CREATED: u64 = 2;
    pub const OVERWRITTEN: u64 = 3;
    // Access rights the kernel looks at
    pub const READ_DATA: u32 = 0x0001;
    pub const WRITE_DATA: u32 = 0x0002;
    pub const APPEND_DATA: u32 = 0x0004;
    pub const GENERIC_READ: u32 = 0x8000_0000;
    pub const GENERIC_WRITE: u32 = 0x4000_0000;
    pub const DELETE: u32 = 0x0001_0000;
    // FILE_ATTRIBUTE_*
    pub const ATTRIBUTE_READONLY: u32 = 0x01;
    pub const ATTRIBUTE_DIRECTORY: u32 = 0x10;
    pub const ATTRIBUTE_ARCHIVE: u32 = 0x20;
    pub const ATTRIBUTE_NORMAL: u32 = 0x80;
    // FILE_INFORMATION_CLASS values served
    pub const DIRECTORY_INFORMATION: u32 = 1;
    pub const BASIC_INFORMATION: u32 = 4;
    pub const STANDARD_INFORMATION: u32 = 5;
    pub const DISPOSITION_INFORMATION: u32 = 13;
    pub const POSITION_INFORMATION: u32 = 14;
    pub const ALLOCATION_INFORMATION: u32 = 19;
    pub const END_OF_FILE_INFORMATION: u32 = 20;
    /// FILE_DIRECTORY_INFORMATION's fixed part (the name follows at 64).
    pub const DIRECTORY_INFORMATION_SIZE: usize = 64;
}

/// Field offsets in the NT structures the loader fills and the DLLs read.
pub mod layout {
    /// TEB (two pages): ClientId, the PEB pointer, LastErrorValue, TLS slots.
    pub const TEB_SIZE: usize = 0x2000;
    pub const TEB_PROCESS_ID: usize = 0x40;
    pub const TEB_THREAD_ID: usize = 0x48;
    pub const TEB_PEB: usize = 0x60;
    pub const TEB_LAST_ERROR: usize = 0x68;
    pub const TEB_TLS_SLOTS: usize = 0x1480;
    pub const TLS_MINIMUM_AVAILABLE: usize = 64;
    /// PEB: image base, loader data, process parameters, process heap.
    pub const PEB_IMAGE_BASE: usize = 0x10;
    pub const PEB_LDR: usize = 0x18;
    pub const PEB_PROCESS_PARAMETERS: usize = 0x20;
    pub const PEB_PROCESS_HEAP: usize = 0x30;
    /// RTL_USER_PROCESS_PARAMETERS.
    pub const PARAMS_STD_INPUT: usize = 0x20;
    pub const PARAMS_STD_OUTPUT: usize = 0x28;
    pub const PARAMS_STD_ERROR: usize = 0x30;
    /// CURDIR: UNICODE_STRING DosPath ("C:\dir\", with the trailing
    /// backslash; buffer room for MAX_PATH) then a handle.
    pub const PARAMS_CURRENT_DIRECTORY: usize = 0x38;
    pub const PARAMS_IMAGE_PATH_NAME: usize = 0x60;
    pub const PARAMS_COMMAND_LINE: usize = 0x70;
    /// Pointer to the environment block: "NAME=value\0" UTF-16 strings, then "\0".
    pub const PARAMS_ENVIRONMENT: usize = 0x80;
    pub const PARAMS_ENVIRONMENT_SIZE: usize = 0x3F0;
    pub const PARAMS_SIZE: usize = 0x440;
    /// PEB_LDR_DATA: the module list in load order.
    pub const LDR_IN_LOAD_ORDER: usize = 0x10;
    /// LDR_DATA_TABLE_ENTRY: DllBase, FullDllName, BaseDllName (InLoadOrderLinks at 0).
    pub const LDR_ENTRY_DLL_BASE: usize = 0x30;
    pub const LDR_ENTRY_FULL_NAME: usize = 0x48;
    pub const LDR_ENTRY_BASE_NAME: usize = 0x58;
}

/// KUSER_SHARED_DATA (mapped read-only at 0x7FFE0000 in every process):
/// field offsets. The KSYSTEM_TIME fields are (LowPart u32, High1Time i32,
/// High2Time i32); read until High1Time == High2Time.
pub mod kusd {
    pub const ADDRESS: usize = 0x7FFE_0000;
    pub const TICK_COUNT_LOW_DEPRECATED: usize = 0x000;
    /// Fixed 1 << 24 on VEILOS: TICK_COUNT is in milliseconds.
    pub const TICK_COUNT_MULTIPLIER: usize = 0x004;
    /// 100 ns units since boot.
    pub const INTERRUPT_TIME: usize = 0x008;
    /// FILETIME (100 ns since 1601-01-01), UTC.
    pub const SYSTEM_TIME: usize = 0x014;
    pub const TIME_ZONE_BIAS: usize = 0x020;
    pub const NT_BUILD_NUMBER: usize = 0x260;
    pub const NT_PRODUCT_TYPE: usize = 0x264;
    pub const PRODUCT_TYPE_IS_VALID: usize = 0x268;
    pub const NATIVE_PROCESSOR_ARCHITECTURE: usize = 0x26A;
    pub const NT_MAJOR_VERSION: usize = 0x26C;
    pub const NT_MINOR_VERSION: usize = 0x270;
    /// Ticks per second of the performance counter (the TSC on VEILOS).
    pub const QPC_FREQUENCY: usize = 0x300;
    /// Milliseconds since boot.
    pub const TICK_COUNT: usize = 0x320;
    pub const COOKIE: usize = 0x330;
}
