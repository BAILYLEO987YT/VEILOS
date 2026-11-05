//! VEILOS `ucrtbase.dll`: the C runtime (WIN32_DESIGN.md, W5).
//!
//! One implementation serves every C runtime a program may import:
//! `ucrtbase.dll` and its API sets (`api-ms-win-crt-*`) directly, and
//! `msvcrt.dll` / `vcruntime140.dll`, which forward to it (their own
//! crates), so a program sees one stdout, one heap and one atexit list
//! whichever names it uses. Names only msvcrt has (`__getmainargs`, `_iob`,
//! `printf` as a real function) are exported here too.
//!
//!   heap.rs     malloc and friends (the process heap)
//!   string.rs   mem*, str*, wcs* and their _s variants
//!   ctype.rs    character classes and case
//!   convert.rs  number <-> text, multibyte <-> wide
//!   format.rs   the printf engine (narrow and wide)
//!   scan.rs     the scanf engine
//!   stdio.rs    FILE streams, file descriptors, the printf/scanf entry points
//!   startup.rs  argv, environment, initialisers, atexit, exit, errno
//!   time.rs     time, clock, struct tm
//!   math.rs     the math library (libm)
//!   misc.rs     qsort, rand, getenv, setjmp/longjmp, directories, locale
//!
//! Exceptions (SEH, C++) are W5b; until then their entry points end the
//! program with a message.
//!
//! Narrow strings are UTF-8 (the console's code page is 65001).

#![no_std]
// Don't let the optimiser turn our loops into calls to strlen/memcpy, which
// may be the very functions being compiled.
#![no_builtins]

extern crate alloc;

mod convert;
mod ctype;
mod eh;
mod format;
mod heap;
mod math;
mod misc;
mod scan;
mod startup;
mod stdio;
mod string;
mod threads;
mod time;

type Handle = u64;

#[allow(dead_code)]
#[link(name = "kernel32", kind = "raw-dylib")]
unsafe extern "system" {
    fn GetProcessHeap() -> Handle;
    fn HeapAlloc(heap: Handle, flags: u32, size: usize) -> *mut u8;
    fn HeapFree(heap: Handle, flags: u32, p: *mut u8) -> i32;
    fn HeapReAlloc(heap: Handle, flags: u32, p: *mut u8, size: usize) -> *mut u8;
    fn HeapSize(heap: Handle, flags: u32, p: *mut u8) -> usize;
    fn GetStdHandle(which: u32) -> Handle;
    fn SetStdHandle(which: u32, h: Handle) -> i32;
    fn WriteFile(h: Handle, buf: *const u8, len: u32, written: *mut u32, overlapped: usize) -> i32;
    fn ReadFile(h: Handle, buf: *mut u8, len: u32, read: *mut u32, overlapped: usize) -> i32;
    fn CreateFileW(name: *const u16, access: u32, share: u32, sa: usize, creation: u32, flags: u32, template: Handle) -> Handle;
    fn CloseHandle(h: Handle) -> i32;
    fn DuplicateHandle(src_process: Handle, src: Handle, dst_process: Handle, dst: *mut Handle, access: u32, inherit: i32, options: u32) -> i32;
    fn SetFilePointerEx(h: Handle, distance: i64, new: *mut i64, method: u32) -> i32;
    fn SetEndOfFile(h: Handle) -> i32;
    fn FlushFileBuffers(h: Handle) -> i32;
    fn GetFileType(h: Handle) -> u32;
    fn GetFileSizeEx(h: Handle, size: *mut i64) -> i32;
    fn ExitProcess(code: u32) -> !;
    fn GetCommandLineW() -> *const u16;
    pub(crate) fn GetCommandLineA() -> *mut u8;
    fn GetEnvironmentStringsW() -> *mut u16;
    fn FreeEnvironmentStringsW(p: *mut u16) -> i32;
    fn SetEnvironmentVariableW(name: *const u16, value: *const u16) -> i32;
    fn GetSystemTimeAsFileTime(ft: *mut u64);
    fn GetTickCount64() -> u64;
    fn GetLastError() -> u32;
    fn DeleteFileW(name: *const u16) -> i32;
    fn CreateDirectoryW(name: *const u16, sa: usize) -> i32;
    fn RemoveDirectoryW(name: *const u16) -> i32;
    fn GetCurrentDirectoryW(len: u32, buf: *mut u16) -> u32;
    fn SetCurrentDirectoryW(name: *const u16) -> i32;
    fn GetFileAttributesW(name: *const u16) -> u32;
    fn GetFullPathNameW(name: *const u16, len: u32, buf: *mut u16, part: *mut *mut u16) -> u32;
    fn FindFirstFileW(pattern: *const u16, data: *mut u8) -> Handle;
    fn FindNextFileW(h: Handle, data: *mut u8) -> i32;
    fn FindClose(h: Handle) -> i32;
    fn GetModuleFileNameW(module: Handle, buf: *mut u16, len: u32) -> u32;
    fn GetCurrentProcessId() -> u32;
    fn GetStartupInfoW(info: *mut u8);
    fn Sleep(ms: u32);
}

// -----| errno |-----

pub mod errno {
    pub const EPERM: i32 = 1;
    pub const ENOENT: i32 = 2;
    pub const EBADF: i32 = 9;
    pub const ENOMEM: i32 = 12;
    pub const EACCES: i32 = 13;
    pub const EEXIST: i32 = 17;
    pub const ENOTDIR: i32 = 20;
    pub const EISDIR: i32 = 21;
    pub const EINVAL: i32 = 22;
    pub const EMFILE: i32 = 24;
    pub const ERANGE: i32 = 34;
    pub const EILSEQ: i32 = 42;
    pub const ENOTEMPTY: i32 = 41;
    pub const STRUNCATE: i32 = 80;
}

pub fn set_errno(e: i32) {
    threads::this().errno = e;
}

/// errno from the last Win32 error, as the CRT maps them.
pub fn errno_from_win32() {
    let e = unsafe { GetLastError() };
    threads::this().doserrno = e;
    set_errno(match e {
        2 | 3 | 15 | 18 | 53 | 161 => errno::ENOENT,
        5 | 19 | 32 | 33 | 82 => errno::EACCES,
        6 => errno::EBADF,
        8 | 14 => errno::ENOMEM,
        80 | 183 => errno::EEXIST,
        145 => errno::ENOTEMPTY,
        267 => errno::ENOTDIR,
        _ => errno::EINVAL,
    });
}

#[no_mangle]
pub extern "C" fn _errno() -> *mut i32 {
    &mut threads::this().errno
}

#[no_mangle]
pub extern "C" fn __doserrno() -> *mut u32 {
    &mut threads::this().doserrno
}

#[no_mangle]
pub unsafe extern "C" fn _get_errno(value: *mut i32) -> i32 {
    unsafe { *value = threads::this().errno };
    0
}

#[no_mangle]
pub extern "C" fn _set_errno(value: i32) -> i32 {
    set_errno(value);
    0
}

// -----| invalid parameters |-----

type InvalidParameterHandler = Option<unsafe extern "C" fn(*const u16, *const u16, *const u16, u32, usize)>;
static mut INVALID_PARAMETER_HANDLER: InvalidParameterHandler = None;

#[no_mangle]
pub unsafe extern "C" fn _set_invalid_parameter_handler(new: InvalidParameterHandler) -> InvalidParameterHandler {
    unsafe { core::mem::replace(&mut *(&raw mut INVALID_PARAMETER_HANDLER), new) }
}

#[no_mangle]
pub unsafe extern "C" fn _get_invalid_parameter_handler() -> InvalidParameterHandler {
    unsafe { INVALID_PARAMETER_HANDLER }
}

/// An invalid parameter: the program's handler if it set one, else (as the
/// release CRT does) set EINVAL and let the function return its error.
pub fn invalid_parameter() {
    set_errno(errno::EINVAL);
    if let Some(h) = unsafe { INVALID_PARAMETER_HANDLER } {
        unsafe { h(core::ptr::null(), core::ptr::null(), core::ptr::null(), 0, 0) };
    }
}

#[no_mangle]
pub extern "C" fn _invalid_parameter_noinfo() {
    invalid_parameter();
}

#[no_mangle]
pub extern "C" fn _invalid_parameter_noinfo_noreturn() -> ! {
    invalid_parameter();
    startup::fatal("invalid parameter", 0xC000_0409)
}

#[no_mangle]
pub extern "C" fn _invalid_parameter(_e: *const u16, _f: *const u16, _file: *const u16, _line: u32, _r: usize) {
    invalid_parameter();
}

// -----| variadic entry points |-----
//
// printf-style functions are C variadic. On Windows x64 every argument has
// an 8-byte slot (the first four in registers, with "home" slots the caller
// reserves just above the return address), so a wrapper can spill the
// register arguments into their home slots and pass a pointer to the first
// variadic one as a va_list. It calls (not jumps to) the implementation, so
// the implementation's own home slots don't overwrite the spilled arguments.

/// `variadic!(name => target, fixed)`: `name(a1..a_fixed, ...)` calls
/// `target(a1..a_fixed, va)`, where `va` points at the first variadic slot.
macro_rules! variadic {
    ($name:ident => $target:path, 1) => { variadic!(@emit $name, $target, "rdx", 56); };
    ($name:ident => $target:path, 2) => { variadic!(@emit $name, $target, "r8", 64); };
    ($name:ident => $target:path, 3) => { variadic!(@emit $name, $target, "r9", 72); };
    // Four fixed arguments: the va_list is the fifth, on the stack.
    ($name:ident => $target:path, 4) => {
        #[unsafe(naked)]
        #[no_mangle]
        pub unsafe extern "C" fn $name() {
            core::arch::naked_asm!(
                "mov [rsp + 8], rcx",
                "mov [rsp + 16], rdx",
                "mov [rsp + 24], r8",
                "mov [rsp + 32], r9",
                "sub rsp, 56",
                "lea rax, [rsp + 96]",
                "mov [rsp + 32], rax",
                "call {t}",
                "add rsp, 56",
                "ret",
                t = sym $target,
            )
        }
    };
    (@emit $name:ident, $target:path, $reg:literal, $off:literal) => {
        #[unsafe(naked)]
        #[no_mangle]
        pub unsafe extern "C" fn $name() {
            core::arch::naked_asm!(
                "mov [rsp + 8], rcx",
                "mov [rsp + 16], rdx",
                "mov [rsp + 24], r8",
                "mov [rsp + 32], r9",
                "sub rsp, 40",
                concat!("lea ", $reg, ", [rsp + ", $off, "]"),
                "call {t}",
                "add rsp, 40",
                "ret",
                t = sym $target,
            )
        }
    };
}
pub(crate) use variadic;

/// A va_list: the next 8-byte argument slot.
#[derive(Clone, Copy)]
#[repr(transparent)]
pub struct VaList(pub *const u64);

impl VaList {
    pub unsafe fn next_u64(&mut self) -> u64 {
        let v = unsafe { *self.0 };
        self.0 = unsafe { self.0.add(1) };
        v
    }
    pub unsafe fn next_f64(&mut self) -> f64 {
        f64::from_bits(unsafe { self.next_u64() })
    }
    pub unsafe fn next_ptr<T>(&mut self) -> *mut T {
        unsafe { self.next_u64() as usize as *mut T }
    }
}

// -----| DLL plumbing |-----

/// Rust's own allocations (Vec in the scanf engine and the like) come from
/// the process heap too.
struct ProcessHeap;

unsafe impl core::alloc::GlobalAlloc for ProcessHeap {
    unsafe fn alloc(&self, layout: core::alloc::Layout) -> *mut u8 {
        heap::_aligned_malloc(layout.size(), layout.align())
    }
    unsafe fn dealloc(&self, p: *mut u8, _layout: core::alloc::Layout) {
        heap::_aligned_free(p)
    }
}

#[global_allocator]
static ALLOCATOR: ProcessHeap = ProcessHeap;

#[no_mangle]
pub extern "system" fn DllMain(_module: usize, reason: u32, _reserved: usize) -> i32 {
    match reason {
        1 => {
            threads::init();
            stdio::init();
            time::init();
        }
        3 => threads::detach(), // DLL_THREAD_DETACH
        _ => {}
    }
    1
}

/// Stack probe (see kernel32's).
#[unsafe(naked)]
#[no_mangle]
pub unsafe extern "C" fn __chkstk() {
    core::arch::naked_asm!(
        "mov r10, rax",
        "lea r11, [rsp + 8]",
        "2:",
        "cmp r10, 0x1000",
        "jb 3f",
        "sub r11, 0x1000",
        "test [r11], r11",
        "sub r10, 0x1000",
        "jmp 2b",
        "3:",
        "sub r11, r10",
        "test [r11], r11",
        "ret",
    )
}

/// Marks that floating point is used (the C runtime's own symbol).
#[no_mangle]
pub static _fltused: i32 = 0x9875;

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    startup::fatal("internal error in the C runtime", 0xDEAD)
}
