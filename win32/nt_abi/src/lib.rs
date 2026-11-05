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
    pub const DISK_FULL: NtStatus = 0xC000_007Fu32 as i32;
    pub const OBJECT_PATH_NOT_FOUND: NtStatus = 0xC000_003Au32 as i32;
    pub const FILE_IS_A_DIRECTORY: NtStatus = 0xC000_00BAu32 as i32;
    pub const DIRECTORY_NOT_EMPTY: NtStatus = 0xC000_0101u32 as i32;
    pub const NOT_A_DIRECTORY: NtStatus = 0xC000_0103u32 as i32;
    pub const DLL_NOT_FOUND: NtStatus = 0xC000_0135u32 as i32;
    pub const PROCEDURE_NOT_FOUND: NtStatus = 0xC000_007Au32 as i32;
    pub const PENDING: NtStatus = 0x0000_0103;
    pub const PIPE_BROKEN: NtStatus = 0xC000_014Bu32 as i32;
    pub const INVALID_IMAGE_FORMAT: NtStatus = 0xC000_007Bu32 as i32;

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
    pub const NT_TERMINATE_THREAD: u32 = BASE + 23;
    /// NtWaitForMultipleObjects(count, handles*, wait_type (0 all, 1 any),
    /// alertable, timeout*).
    pub const NT_WAIT_FOR_MULTIPLE_OBJECTS: u32 = BASE + 24;
    /// NtQueryInformationProcess(handle, class (0: basic), info*, length, return_length*).
    pub const NT_QUERY_INFORMATION_PROCESS: u32 = BASE + 25;
    /// NtQueryInformationThread(handle, class (0: basic), info*, length, return_length*).
    pub const NT_QUERY_INFORMATION_THREAD: u32 = BASE + 26;
    /// NtResumeThread(handle, previous_count*).
    pub const NT_RESUME_THREAD: u32 = BASE + 27;
    /// NtDuplicateObject(source_process, source_handle, target_process,
    /// target_handle*, access, attributes, options).
    pub const NT_DUPLICATE_OBJECT: u32 = BASE + 28;
    /// VEILOS-specific: start a thread in this process at ntdll's
    /// RtlUserThreadStart(start, parameter)
    /// (start, parameter, flags (CREATE_SUSPENDED), result: *mut ThreadResult).
    pub const VEIL_CREATE_THREAD: u32 = BASE + 29;
    /// VEILOS-specific: start a program (params: *const ProcessParams,
    /// result: *mut ProcessResult).
    pub const VEIL_CREATE_PROCESS: u32 = BASE + 30;
    /// VEILOS-specific: an anonymous pipe (read_out*, write_out*, buffer_size).
    pub const VEIL_CREATE_PIPE: u32 = BASE + 31;
    /// NtQueryVolumeInformationFile(handle, iosb*, info*, length, class
    /// (4: FileFsDeviceInformation)).
    pub const NT_QUERY_VOLUME_INFORMATION_FILE: u32 = BASE + 32;
    /// VEILOS-specific: OutputDebugString's text to the kernel log (text: *const u8, length).
    pub const VEIL_DEBUG_PRINT: u32 = BASE + 33;
    /// NtSetInformationObject(handle, class (4: ObjectHandleFlagInformation), info*, length).
    pub const NT_SET_INFORMATION_OBJECT: u32 = BASE + 34;
    /// NtQueryObject(handle, class (4: ObjectHandleFlagInformation), info*, length, return_length*).
    pub const NT_QUERY_OBJECT: u32 = BASE + 35;
    // -----| windows (user32, G1) |-----
    /// VEIL_USER_CREATE_WINDOW(params: *const WindowParams, hwnd_out: *mut u64).
    pub const VEIL_USER_CREATE_WINDOW: u32 = BASE + 36;
    /// VEIL_USER_DESTROY_WINDOW(hwnd).
    pub const VEIL_USER_DESTROY_WINDOW: u32 = BASE + 37;
    /// VEIL_USER_SHOW_WINDOW(hwnd, show: 0 hide, 1 show, 2 minimize, 3 maximize, 4 show without focus).
    pub const VEIL_USER_SHOW_WINDOW: u32 = BASE + 38;
    /// VEIL_USER_SET_TEXT(hwnd, title: *const u16).
    pub const VEIL_USER_SET_TEXT: u32 = BASE + 39;
    /// VEIL_USER_SET_POS(hwnd, place: *const WindowPlace, flags: 1 move, 2 size).
    pub const VEIL_USER_SET_POS: u32 = BASE + 40;
    /// VEIL_USER_GET_POS(hwnd, place_out: *mut WindowPlace): client origin (screen) and size.
    pub const VEIL_USER_GET_POS: u32 = BASE + 41;
    /// VEIL_USER_PRESENT(hwnd, pixels: *const u32, stride_pixels, rect: *const [i32; 4] x, y, w, h).
    pub const VEIL_USER_PRESENT: u32 = BASE + 42;
    /// VEIL_USER_POST(hwnd, message, wparam, lparam, thread_id (when hwnd is 0)).
    pub const VEIL_USER_POST: u32 = BASE + 43;
    /// VEIL_USER_GET_MESSAGE(msg_out: *mut Msg, timeout_ms (u32::MAX: forever),
    /// flags (1 remove, 2 no timers), hwnd filter, min, max).
    pub const VEIL_USER_GET_MESSAGE: u32 = BASE + 44;
    /// VEIL_USER_TIMER(hwnd, id, interval_ms (0: kill)).
    pub const VEIL_USER_TIMER: u32 = BASE + 45;
    /// VEIL_USER_QUERY(what: 0 screen size, 1 cursor position, 2 | vk << 8 key
    /// state [down, toggled], out: *mut [i32; 2]).
    pub const VEIL_USER_QUERY: u32 = BASE + 46;
    /// NtCreateSemaphore(handle_out*, access, attributes*, initial, maximum).
    pub const NT_CREATE_SEMAPHORE: u32 = BASE + 47;
    /// NtReleaseSemaphore(handle, count, previous*).
    pub const NT_RELEASE_SEMAPHORE: u32 = BASE + 48;
    /// VEIL_CLIP_SET(text: *const u16, chars): the clipboard's text.
    pub const VEIL_CLIP_SET: u32 = BASE + 49;
    /// VEIL_CLIP_GET(buf: *mut u16, cap, chars_out*, sequence_out*): the
    /// clipboard's latest text (buf null: just its length and sequence).
    pub const VEIL_CLIP_GET: u32 = BASE + 50;
    /// VEIL_REG(call: *mut reg::RegCall): the registry; returns a Win32 error.
    pub const VEIL_REG: u32 = BASE + 51;
    /// VEIL_MAP_VIEW(file, offset, size, out*, flags): a view of a file brought
    /// in as it's touched. Flags: 1 private copy, writable; 2 executable.
    pub const VEIL_MAP_VIEW: u32 = BASE + 52;
    /// VEIL_UNMAP_VIEW(addr).
    pub const VEIL_UNMAP_VIEW: u32 = BASE + 53;
    /// VEIL_NAME_OBJECT(handle, name*, kind). Kinds: 1 event, 2 mutex, 3 semaphore.
    pub const VEIL_NAME_OBJECT: u32 = BASE + 54;
    /// VEIL_OPEN_NAMED(name*, kind, out*).
    pub const VEIL_OPEN_NAMED: u32 = BASE + 55;
    /// VEIL_WAIT_ADDRESS(addr, compare*, size, ms).
    pub const VEIL_WAIT_ADDRESS: u32 = BASE + 56;
    /// VEIL_WAKE_ADDRESS(addr).
    pub const VEIL_WAKE_ADDRESS: u32 = BASE + 57;
    /// VEIL_PIPE(op, a, b, c): named pipes (src/nt_ipc.rs).
    pub const VEIL_PIPE: u32 = BASE + 58;
    /// VEIL_MUTEX(op, a, b): create (owned, out*) or release (handle).
    pub const VEIL_MUTEX: u32 = BASE + 59;
    /// VEIL_SYSINFO(op, out*, size). Op 0: random bytes. Op 1: [total, free] memory.
    pub const VEIL_SYSINFO: u32 = BASE + 60;
    /// NtOpenProcess(handle*, access, attributes*, client_id*).
    pub const NT_OPEN_PROCESS: u32 = BASE + 61;
    /// VEIL_SOCKET(op, a, b, c, [d, e]): Winsock's sockets (src/nt_socket.rs).
    pub const VEIL_SOCKET: u32 = BASE + 62;
    /// One past the last number in use.
    pub const END: u32 = BASE + 63;
}

/// The pseudo-handle for "this process" (NtCurrentProcess()).
pub const CURRENT_PROCESS: u64 = u64::MAX;
/// The pseudo-handle for "this thread" (NtCurrentThread()).
pub const CURRENT_THREAD: u64 = u64::MAX - 1;

/// VEIL_CREATE_THREAD's result.
#[repr(C)]
#[derive(Default)]
pub struct ThreadResult {
    pub handle: u64,
    pub tid: u64,
}

/// VEIL_CREATE_PROCESS's parameters. Strings are NUL-terminated UTF-16
/// (0: none).
#[repr(C)]
pub struct ProcessParams {
    /// The program, as a DOS path ("C:\dir\x.exe").
    pub image: u64,
    /// The whole command line the program will see.
    pub command_line: u64,
    /// Its current directory (0: the caller's).
    pub current_directory: u64,
    /// Its environment block (0: a copy of the caller's).
    pub environment: u64,
    /// CreateProcess's creation flags (CREATE_SUSPENDED, CREATE_NEW_CONSOLE,
    /// DETACHED_PROCESS).
    pub flags: u64,
    /// If `use_std_handles`, the caller's handles to give the program as its
    /// standard input, output and error (0: none).
    pub std_handles: [u64; 3],
    pub use_std_handles: u64,
    /// bInheritHandles: the program gets a copy of the caller's handles,
    /// with the same values.
    pub inherit_handles: u64,
    /// STARTUPINFO's dwFlags, and its lpReserved2/cbReserved2 (the C
    /// runtime's inherited file descriptors), kept for GetStartupInfo.
    pub startup_flags: u64,
    pub runtime_data: u64,
    pub runtime_size: u64,
}

/// VEIL_USER_CREATE_WINDOW's parameters: the window's title, screen
/// position (of its frame) and client size, and `WINDOW_*` flags.
#[repr(C)]
pub struct WindowParams {
    pub title: u64,
    pub x: i32,
    pub y: i32,
    pub client_w: u32,
    pub client_h: u32,
    pub flags: u32,
}

/// Shown when created.
pub const WINDOW_VISIBLE: u32 = 1;
/// No desktop frame (a popup: a menu, a drop-down list): its position is
/// its client area's.
pub const WINDOW_POPUP: u32 = 2;
/// Owned by another window (a dialog): no taskbar button.
pub const WINDOW_OWNED: u32 = 4;

/// The registry's one syscall: a request and what comes back.
pub mod reg {
    pub const OPEN: u32 = 1;
    pub const CREATE: u32 = 2;
    pub const CLOSE: u32 = 3;
    pub const QUERY_VALUE: u32 = 4;
    pub const SET_VALUE: u32 = 5;
    pub const DELETE_VALUE: u32 = 6;
    pub const DELETE_KEY: u32 = 7;
    pub const ENUM_KEY: u32 = 8;
    pub const ENUM_VALUE: u32 = 9;
    pub const INFO: u32 = 10;
    pub const FLUSH: u32 = 11;
    pub const RENAME: u32 = 12;

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    pub struct RegCall {
        pub op: u32,
        /// DELETE_KEY: 1 to take what's under it. CREATE returns 1 created, 2 opened.
        pub flags: u32,
        pub key: u64,
        /// A key path or value name, not NUL-terminated.
        pub name: u64,
        pub name_len: u32,
        pub kind: u32,
        /// In or out, with its capacity going in and its size coming back.
        /// RENAME: the new name, in characters.
        pub data: u64,
        pub data_len: u32,
        pub index: u32,
        pub out_key: u64,
        /// ENUM_*: where the name goes (characters, with room for a NUL).
        pub out_name: u64,
        pub out_name_cap: u32,
        pub out_name_len: u32,
        /// INFO: subkeys, longest subkey name, values, longest value name,
        /// largest data.
        pub info: [u32; 6],
    }
}

/// In a WM_CHAR / WM_SYSCHAR the desktop typed (lParam bit 28, reserved on
/// Windows): user32 delivers it only if its key-down was translated, as
/// TranslateMessage would have made it, and clears the bit.
pub const CHAR_TYPED: u64 = 1 << 28;

/// A window's client area: its origin on the screen and its size.
#[repr(C)]
#[derive(Default, Clone, Copy)]
pub struct WindowPlace {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

/// MSG, as Windows lays it out.
#[repr(C)]
#[derive(Default, Clone, Copy)]
pub struct Msg {
    pub hwnd: u64,
    pub message: u32,
    pub wparam: u64,
    pub lparam: u64,
    pub time: u32,
    pub pt_x: i32,
    pub pt_y: i32,
    pub private: u32,
}

/// VEIL_CREATE_PROCESS's result.
#[repr(C)]
#[derive(Default)]
pub struct ProcessResult {
    pub process: u64,
    pub thread: u64,
    pub pid: u64,
    pub tid: u64,
}

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
    pub const FULL_DIRECTORY_INFORMATION: u32 = 2;
    pub const BOTH_DIRECTORY_INFORMATION: u32 = 3;
    pub const ID_BOTH_DIRECTORY_INFORMATION: u32 = 37;
    pub const ID_FULL_DIRECTORY_INFORMATION: u32 = 38;
    pub const BASIC_INFORMATION: u32 = 4;
    pub const STANDARD_INFORMATION: u32 = 5;
    pub const INTERNAL_INFORMATION: u32 = 6;
    pub const NAME_INFORMATION: u32 = 9;
    pub const RENAME_INFORMATION: u32 = 10;
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
    /// The first thread's TLS array, past the end of a real PEB: that thread
    /// starts before anything could note it, and a DLL loaded later needs it.
    pub const PEB_MAIN_TLS_ARRAY: usize = 0x7F0;
    /// Slots in every thread's ThreadLocalStoragePointer array. TLS indexes are
    /// never reused, and every crate's proc macro DLL takes one.
    pub const TLS_ARRAY_SLOTS: usize = 256;
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
    pub const PARAMS_WINDOW_FLAGS: usize = 0xA4;
    /// UNICODE_STRING-shaped: STARTUPINFO's cbReserved2/lpReserved2.
    pub const PARAMS_RUNTIME_DATA: usize = 0xE0;
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
