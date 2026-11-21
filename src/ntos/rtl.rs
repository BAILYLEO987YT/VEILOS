// Run-time library: counted strings, version, the C runtime functions
// ntoskrnl exports, and DbgPrint (to the serial log as "[DRV] ...").

use alloc::string::String;

use super::{NtStatus, STATUS_SUCCESS};

// UNICODE_STRING / ANSI_STRING: Length (u16, bytes), MaximumLength (u16),
// Buffer (pointer at 8).

pub fn unicode_to_string(us: *const u8) -> String {
    if us.is_null() {
        return String::new();
    }
    unsafe {
        let len = (us as *const u16).read_unaligned() as usize / 2;
        let buf = (us.add(8) as *const *const u16).read_unaligned();
        if buf.is_null() {
            return String::new();
        }
        let units = core::slice::from_raw_parts(buf, len);
        char::decode_utf16(units.iter().copied()).map(|c| c.unwrap_or('?')).collect()
    }
}

pub extern "win64" fn RtlInitUnicodeString(us: *mut u8, src: *const u16) {
    unsafe {
        let n = if src.is_null() { 0 } else { wcslen(src) * 2 };
        (us as *mut u16).write_unaligned(n as u16);
        (us.add(2) as *mut u16).write_unaligned(if src.is_null() { 0 } else { n as u16 + 2 });
        (us.add(8) as *mut *const u16).write_unaligned(src);
    }
}

pub extern "win64" fn RtlInitAnsiString(s: *mut u8, src: *const u8) {
    unsafe {
        let n = if src.is_null() { 0 } else { strlen(src) };
        (s as *mut u16).write_unaligned(n as u16);
        (s.add(2) as *mut u16).write_unaligned(if src.is_null() { 0 } else { n as u16 + 1 });
        (s.add(8) as *mut *const u8).write_unaligned(src);
    }
}

pub extern "win64" fn RtlInitString(s: *mut u8, src: *const u8) {
    RtlInitAnsiString(s, src)
}

pub extern "win64" fn RtlCopyUnicodeString(dst: *mut u8, src: *const u8) {
    unsafe {
        if src.is_null() {
            (dst as *mut u16).write_unaligned(0);
            return;
        }
        let max = (dst.add(2) as *const u16).read_unaligned();
        let n = (src as *const u16).read_unaligned().min(max);
        let d = (dst.add(8) as *const *mut u8).read_unaligned();
        let s = (src.add(8) as *const *const u8).read_unaligned();
        core::ptr::copy(s, d, n as usize);
        (dst as *mut u16).write_unaligned(n);
        if n < max {
            (d.add(n as usize) as *mut u16).write_unaligned(0);
        }
    }
}

fn upcase(c: u16) -> u16 {
    if (b'a' as u16..=b'z' as u16).contains(&c) { c - 32 } else { c }
}

fn units(us: *const u8) -> &'static [u16] {
    unsafe {
        let len = (us as *const u16).read_unaligned() as usize / 2;
        let buf = (us.add(8) as *const *const u16).read_unaligned();
        if buf.is_null() { &[] } else { core::slice::from_raw_parts(buf, len) }
    }
}

pub extern "win64" fn RtlCompareUnicodeString(a: *const u8, b: *const u8, ci: u8) -> i32 {
    let (a, b) = (units(a), units(b));
    for (x, y) in a.iter().zip(b) {
        let (x, y) = if ci != 0 { (upcase(*x), upcase(*y)) } else { (*x, *y) };
        if x != y {
            return x as i32 - y as i32;
        }
    }
    a.len() as i32 - b.len() as i32
}

pub extern "win64" fn RtlEqualUnicodeString(a: *const u8, b: *const u8, ci: u8) -> u8 {
    (units(a).len() == units(b).len() && RtlCompareUnicodeString(a, b, ci) == 0) as u8
}

/// RTL_OSVERSIONINFOW (or EXW): what VEILOS tells drivers it is: Windows 11
/// 24H2 (10.0.26100), the version whose WDK its interfaces follow.
pub extern "win64" fn RtlGetVersion(info: *mut u8) -> NtStatus {
    unsafe {
        let size = (info as *const u32).read_unaligned();
        (info.add(4) as *mut u32).write_unaligned(10);
        (info.add(8) as *mut u32).write_unaligned(0);
        (info.add(12) as *mut u32).write_unaligned(26100);
        (info.add(16) as *mut u32).write_unaligned(2); // VER_PLATFORM_WIN32_NT
        core::ptr::write_bytes(info.add(20), 0, 256); // szCSDVersion
        if size >= 284 {
            // EX: service pack, suite mask, product type (workstation).
            core::ptr::write_bytes(info.add(276), 0, 8);
            info.add(282).write(1);
        }
    }
    STATUS_SUCCESS
}

pub extern "win64" fn RtlCompareMemory(a: *const u8, b: *const u8, n: usize) -> usize {
    (0..n).find(|&i| unsafe { a.add(i).read() != b.add(i).read() }).unwrap_or(n)
}

// -----| C runtime (ntoskrnl exports these) |-----

pub extern "win64" fn memcpy(d: *mut u8, s: *const u8, n: usize) -> *mut u8 {
    unsafe { core::ptr::copy(s, d, n) };
    d
}
pub extern "win64" fn memmove(d: *mut u8, s: *const u8, n: usize) -> *mut u8 {
    unsafe { core::ptr::copy(s, d, n) };
    d
}
pub extern "win64" fn memset(d: *mut u8, c: i32, n: usize) -> *mut u8 {
    unsafe { core::ptr::write_bytes(d, c as u8, n) };
    d
}
pub extern "win64" fn memcmp(a: *const u8, b: *const u8, n: usize) -> i32 {
    for i in 0..n {
        let (x, y) = unsafe { (a.add(i).read(), b.add(i).read()) };
        if x != y {
            return x as i32 - y as i32;
        }
    }
    0
}
pub extern "win64" fn strlen(s: *const u8) -> usize {
    let mut n = 0;
    while unsafe { s.add(n).read() } != 0 {
        n += 1;
    }
    n
}
pub extern "win64" fn wcslen(s: *const u16) -> usize {
    let mut n = 0;
    while unsafe { s.add(n).read() } != 0 {
        n += 1;
    }
    n
}
pub extern "win64" fn strcmp(a: *const u8, b: *const u8) -> i32 {
    strncmp(a, b, usize::MAX)
}
pub extern "win64" fn strncmp(a: *const u8, b: *const u8, n: usize) -> i32 {
    for i in 0..n {
        let (x, y) = unsafe { (a.add(i).read(), b.add(i).read()) };
        if x != y || x == 0 {
            return x as i32 - y as i32;
        }
    }
    0
}
pub extern "win64" fn _stricmp(a: *const u8, b: *const u8) -> i32 {
    let mut i = 0;
    loop {
        let (x, y) = unsafe { (a.add(i).read().to_ascii_lowercase(), b.add(i).read().to_ascii_lowercase()) };
        if x != y || x == 0 {
            return x as i32 - y as i32;
        }
        i += 1;
    }
}
pub extern "win64" fn wcscmp(a: *const u16, b: *const u16) -> i32 {
    let mut i = 0;
    loop {
        let (x, y) = unsafe { (a.add(i).read(), b.add(i).read()) };
        if x != y || x == 0 {
            return x as i32 - y as i32;
        }
        i += 1;
    }
}

/// __chkstk: the stack probe before a large frame. VEILOS's kernel stacks
/// are mapped in full, so there is nothing to touch; RAX (the frame size)
/// must survive, and does.
#[unsafe(naked)]
pub unsafe extern "win64" fn __chkstk() {
    core::arch::naked_asm!("ret");
}

// -----| DbgPrint |-----
//
// Variadic in the Windows x64 convention: the caller's home area holds the
// register arguments, and the rest follow it on the stack, so once RDX, R8
// and R9 are stored to their home slots the arguments are one array.

#[unsafe(naked)]
pub unsafe extern "win64" fn DbgPrint() {
    core::arch::naked_asm!(
        "mov [rsp + 16], rdx",
        "mov [rsp + 24], r8",
        "mov [rsp + 32], r9",
        "lea rdx, [rsp + 16]",
        "sub rsp, 40",
        "call {imp}",
        "add rsp, 40",
        "ret",
        imp = sym dbg_print_impl,
    );
}

/// DbgPrintEx(ComponentId, Level, Format, ...)
#[unsafe(naked)]
pub unsafe extern "win64" fn DbgPrintEx() {
    core::arch::naked_asm!(
        "mov [rsp + 32], r9",
        "mov rcx, r8",
        "lea rdx, [rsp + 32]",
        "sub rsp, 40",
        "call {imp}",
        "add rsp, 40",
        "ret",
        imp = sym dbg_print_impl,
    );
}

/// vDbgPrintEx(ComponentId, Level, Format, va_list): va_list is the array.
pub extern "win64" fn vDbgPrintEx(_component: u32, _level: u32, fmt: *const u8, args: *const u64) -> NtStatus {
    dbg_print_impl(fmt, args)
}

extern "win64" fn dbg_print_impl(fmt: *const u8, args: *const u64) -> NtStatus {
    let text = format_c(fmt, args);
    crate::serial_println!("[DRV] {}", text.trim_end_matches('\n'));
    STATUS_SUCCESS
}

/// printf (crt::format_core) over a narrow format string.
pub fn format_c(fmt: *const u8, args: *const u64) -> String {
    let n = strlen(fmt);
    let chars: alloc::vec::Vec<char> = unsafe { core::slice::from_raw_parts(fmt, n) }.iter().map(|&b| b as char).collect();
    super::crt::format_core(&chars, args, false)
}
