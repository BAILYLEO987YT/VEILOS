//! The console (S5): screen buffers and input, through VEIL_CONSOLE
//! (src/nt_console.rs). Output handles are screen buffers, input handles the
//! console itself; COORDs travel packed, X low and Y high, as Windows packs them.

use crate::util::wstr;
use crate::{fail, Bool, Handle};
use nt_abi::status;

#[link(name = "ntdll", kind = "raw-dylib")]
unsafe extern "system" {
    fn VeilConsole(op: u64, h: u64, a: u64, b: u64, c: u64, d: u64) -> i32;
}

const OP_WRITE: u64 = 0;
const OP_INFO: u64 = 1;
const OP_SET_CURSOR: u64 = 2;
const OP_SET_ATTR: u64 = 3;
const OP_FILL: u64 = 4;
const OP_WRITE_CELLS: u64 = 5;
const OP_READ_CELLS: u64 = 6;
const OP_SCROLL: u64 = 7;
const OP_GET_MODE: u64 = 8;
const OP_SET_MODE: u64 = 9;
const OP_READ_INPUT: u64 = 10;
const OP_WRITE_INPUT: u64 = 11;
const OP_INPUT_COUNT: u64 = 12;
const OP_FLUSH_INPUT: u64 = 13;
const OP_SET_SIZE: u64 = 14;
const OP_SET_WINDOW: u64 = 15;
const OP_NEW_BUFFER: u64 = 16;
const OP_SET_ACTIVE: u64 = 17;
const OP_GET_CURSOR_INFO: u64 = 18;
const OP_SET_CURSOR_INFO: u64 = 19;
const OP_GET_TITLE: u64 = 20;
const OP_SET_TITLE: u64 = 21;
const OP_READ_TEXT: u64 = 22;
const OP_WRITE_BLOCK: u64 = 23;
const OP_READ_BLOCK: u64 = 24;

fn call(op: u64, h: Handle, a: u64, b: u64, c: u64, d: u64) -> Bool {
    let s = unsafe { VeilConsole(op, h, a, b, c, d) };
    if s == 0 { 1 } else { fail(s) }
}

fn out_u32(p: *mut u32, v: u32) {
    if !p.is_null() {
        unsafe { *p = v };
    }
}

/// The ANSI code page is UTF-8 here: one byte in, one unit out, for ASCII;
/// anything else goes through UTF-8.
fn widen(bytes: &[u8]) -> alloc_free::Units {
    alloc_free::Units::from_utf8(bytes)
}

mod alloc_free {
    /// UTF-16 from UTF-8, in a fixed buffer (kernel32 has no allocator).
    pub struct Units {
        pub buf: [u16; 2048],
        pub len: usize,
    }
    impl Units {
        pub fn from_utf8(bytes: &[u8]) -> Units {
            let mut u = Units { buf: [0; 2048], len: 0 };
            let text = core::str::from_utf8(bytes).unwrap_or("");
            let text = if text.is_empty() && !bytes.is_empty() {
                // Not UTF-8: each byte as itself.
                for &b in bytes.iter().take(2048) {
                    u.buf[u.len] = b as u16;
                    u.len += 1;
                }
                return u;
            } else {
                text
            };
            for unit in text.encode_utf16() {
                if u.len == u.buf.len() {
                    break;
                }
                u.buf[u.len] = unit;
                u.len += 1;
            }
            u
        }
        pub fn as_slice(&self) -> &[u16] {
            &self.buf[..self.len]
        }
    }
}

// -----| output |-----

#[no_mangle]
pub unsafe extern "system" fn WriteConsoleW(h: Handle, buffer: *const u16, length: u32, written: *mut u32, _reserved: usize) -> Bool {
    let mut n = 0u32;
    let r = call(OP_WRITE, h, buffer as u64, length as u64, &mut n as *mut u32 as u64, 0);
    out_u32(written, n);
    r
}

#[no_mangle]
pub unsafe extern "system" fn WriteConsoleA(h: Handle, buffer: *const u8, length: u32, written: *mut u32, _reserved: usize) -> Bool {
    let bytes = unsafe { core::slice::from_raw_parts(buffer, length as usize) };
    // In pieces, so a long write needn't fit one buffer.
    for chunk in bytes.chunks(1024) {
        let u = widen(chunk);
        if call(OP_WRITE, h, u.buf.as_ptr() as u64, u.len as u64, 0, 0) == 0 {
            return 0;
        }
    }
    out_u32(written, length);
    1
}

/// CONSOLE_SCREEN_BUFFER_INFO: size, cursor, attributes, window, largest window.
#[no_mangle]
pub unsafe extern "system" fn GetConsoleScreenBufferInfo(h: Handle, info: *mut i16) -> Bool {
    call(OP_INFO, h, info as u64, 0, 0, 0)
}

/// CONSOLE_SCREEN_BUFFER_INFOEX: its size, then the same, then the popup
/// colours, full-screen flag and colour table.
#[no_mangle]
pub unsafe extern "system" fn GetConsoleScreenBufferInfoEx(h: Handle, info: *mut u8) -> Bool {
    if info.is_null() {
        return fail(status::INVALID_PARAMETER);
    }
    let mut base = [0i16; 11];
    if call(OP_INFO, h, base.as_mut_ptr() as u64, 0, 0, 0) == 0 {
        return 0;
    }
    unsafe {
        core::ptr::copy_nonoverlapping(base.as_ptr() as *const u8, info.add(4), 22);
        *(info.add(26) as *mut u16) = 0xF5; // popup attributes
        *(info.add(28) as *mut u32) = 0; // full screen supported
        let table: [u32; 16] = [
            0x0C0C0C, 0xDA3700, 0x0EA113, 0xDD963A, 0x1F0FC5, 0x981788, 0x009CC1, 0xCCCCCC,
            0x767676, 0xFF783B, 0x0CC616, 0xD6D661, 0x5648E7, 0x9E00B4, 0xA5F1F9, 0xF2F2F2,
        ];
        core::ptr::copy_nonoverlapping(table.as_ptr(), info.add(32) as *mut u32, 16);
    }
    1
}

fn pack(pos: [i16; 2]) -> u64 {
    (pos[0] as u16 as u64) | ((pos[1] as u16 as u64) << 16)
}

#[no_mangle]
pub extern "system" fn SetConsoleCursorPosition(h: Handle, pos: u32) -> Bool {
    call(OP_SET_CURSOR, h, pos as u64, 0, 0, 0)
}

#[no_mangle]
pub extern "system" fn SetConsoleTextAttribute(h: Handle, attr: u16) -> Bool {
    call(OP_SET_ATTR, h, attr as u64, 0, 0, 0)
}

/// CONSOLE_CURSOR_INFO: size (1-100), visible.
#[no_mangle]
pub unsafe extern "system" fn GetConsoleCursorInfo(h: Handle, info: *mut u32) -> Bool {
    call(OP_GET_CURSOR_INFO, h, info as u64, 0, 0, 0)
}

#[no_mangle]
pub unsafe extern "system" fn SetConsoleCursorInfo(h: Handle, info: *const u32) -> Bool {
    if info.is_null() {
        return fail(status::INVALID_PARAMETER);
    }
    let (size, visible) = unsafe { (*info, *info.add(1)) };
    call(OP_SET_CURSOR_INFO, h, size as u64, visible as u64, 0, 0)
}

#[no_mangle]
pub unsafe extern "system" fn FillConsoleOutputCharacterW(h: Handle, ch: u16, len: u32, pos: u32, written: *mut u32) -> Bool {
    let mut n = 0u32;
    let r = call(OP_FILL, h, ch as u64, len as u64, pos as u64, &mut n as *mut u32 as u64);
    out_u32(written, n);
    r
}

#[no_mangle]
pub unsafe extern "system" fn FillConsoleOutputCharacterA(h: Handle, ch: u8, len: u32, pos: u32, written: *mut u32) -> Bool {
    unsafe { FillConsoleOutputCharacterW(h, ch as u16, len, pos, written) }
}

#[no_mangle]
pub unsafe extern "system" fn FillConsoleOutputAttribute(h: Handle, attr: u16, len: u32, pos: u32, written: *mut u32) -> Bool {
    let mut n = 0u32;
    let r = call(OP_FILL, h, attr as u64 | 1 << 32, len as u64, pos as u64, &mut n as *mut u32 as u64);
    out_u32(written, n);
    r
}

unsafe fn cells(op: u64, h: Handle, data: u64, len: u32, pos: u32, attrs: bool, done: *mut u32) -> Bool {
    let mut n = 0u32;
    let r = call(op, h, data, len as u64, pos as u64 | (attrs as u64) << 32, &mut n as *mut u32 as u64);
    out_u32(done, n);
    r
}

#[no_mangle]
pub unsafe extern "system" fn WriteConsoleOutputCharacterW(h: Handle, text: *const u16, len: u32, pos: u32, written: *mut u32) -> Bool {
    unsafe { cells(OP_WRITE_CELLS, h, text as u64, len, pos, false, written) }
}

#[no_mangle]
pub unsafe extern "system" fn WriteConsoleOutputCharacterA(h: Handle, text: *const u8, len: u32, pos: u32, written: *mut u32) -> Bool {
    let u = widen(unsafe { core::slice::from_raw_parts(text, len as usize) });
    unsafe { cells(OP_WRITE_CELLS, h, u.buf.as_ptr() as u64, u.len as u32, pos, false, written) }
}

#[no_mangle]
pub unsafe extern "system" fn WriteConsoleOutputAttribute(h: Handle, attrs: *const u16, len: u32, pos: u32, written: *mut u32) -> Bool {
    unsafe { cells(OP_WRITE_CELLS, h, attrs as u64, len, pos, true, written) }
}

#[no_mangle]
pub unsafe extern "system" fn ReadConsoleOutputCharacterW(h: Handle, out: *mut u16, len: u32, pos: u32, read: *mut u32) -> Bool {
    unsafe { cells(OP_READ_CELLS, h, out as u64, len, pos, false, read) }
}

#[no_mangle]
pub unsafe extern "system" fn ReadConsoleOutputCharacterA(h: Handle, out: *mut u8, len: u32, pos: u32, read: *mut u32) -> Bool {
    let mut units = [0u16; 1024];
    let mut n = 0u32;
    if unsafe { cells(OP_READ_CELLS, h, units.as_mut_ptr() as u64, len.min(1024), pos, false, &mut n) } == 0 {
        return 0;
    }
    for i in 0..n as usize {
        unsafe { *out.add(i) = if units[i] < 0x100 { units[i] as u8 } else { b'?' } };
    }
    out_u32(read, n);
    1
}

#[no_mangle]
pub unsafe extern "system" fn ReadConsoleOutputAttribute(h: Handle, out: *mut u16, len: u32, pos: u32, read: *mut u32) -> Bool {
    unsafe { cells(OP_READ_CELLS, h, out as u64, len, pos, true, read) }
}

/// CHAR_INFO blocks: (buffer, its size, where in it, the rectangle of the screen).
#[no_mangle]
pub unsafe extern "system" fn WriteConsoleOutputW(h: Handle, data: *const u32, size: u32, from: u32, rect: *mut i16) -> Bool {
    call(OP_WRITE_BLOCK, h, data as u64, size as u64, from as u64, rect as u64)
}

#[no_mangle]
pub unsafe extern "system" fn ReadConsoleOutputW(h: Handle, data: *mut u32, size: u32, from: u32, rect: *mut i16) -> Bool {
    call(OP_READ_BLOCK, h, data as u64, size as u64, from as u64, rect as u64)
}

#[no_mangle]
pub unsafe extern "system" fn WriteConsoleOutputA(h: Handle, data: *const u32, size: u32, from: u32, rect: *mut i16) -> Bool {
    unsafe { WriteConsoleOutputW(h, data, size, from, rect) }
}

#[no_mangle]
pub unsafe extern "system" fn ReadConsoleOutputA(h: Handle, data: *mut u32, size: u32, from: u32, rect: *mut i16) -> Bool {
    unsafe { ReadConsoleOutputW(h, data, size, from, rect) }
}

/// (rectangle, clip rectangle or NULL, destination COORD, fill CHAR_INFO).
#[no_mangle]
pub unsafe extern "system" fn ScrollConsoleScreenBufferW(h: Handle, rect: *const i16, clip: *const i16, dest: u32, fill: *const u16) -> Bool {
    if rect.is_null() || fill.is_null() {
        return fail(status::INVALID_PARAMETER);
    }
    let mut v = [0i16; 13];
    unsafe {
        for i in 0..4 {
            v[i] = *rect.add(i);
        }
        if !clip.is_null() {
            v[4] = 1;
            for i in 0..4 {
                v[5 + i] = *clip.add(i);
            }
        }
        v[9] = dest as u16 as i16;
        v[10] = (dest >> 16) as u16 as i16;
        v[11] = *fill as i16;
        v[12] = *fill.add(1) as i16;
    }
    call(OP_SCROLL, h, v.as_ptr() as u64, 0, 0, 0)
}

#[no_mangle]
pub unsafe extern "system" fn ScrollConsoleScreenBufferA(h: Handle, rect: *const i16, clip: *const i16, dest: u32, fill: *const u16) -> Bool {
    unsafe { ScrollConsoleScreenBufferW(h, rect, clip, dest, fill) }
}

#[no_mangle]
pub extern "system" fn CreateConsoleScreenBuffer(_access: u32, _share: u32, _sa: usize, _flags: u32, _data: usize) -> Handle {
    let mut h: Handle = 0;
    let stdout = crate::GetStdHandle(-11i32 as u32);
    if call(OP_NEW_BUFFER, stdout, &mut h as *mut Handle as u64, 0, 0, 0) == 0 {
        return crate::INVALID_HANDLE_VALUE;
    }
    h
}

#[no_mangle]
pub extern "system" fn SetConsoleActiveScreenBuffer(h: Handle) -> Bool {
    call(OP_SET_ACTIVE, h, 0, 0, 0, 0)
}

#[no_mangle]
pub extern "system" fn SetConsoleScreenBufferSize(h: Handle, size: u32) -> Bool {
    call(OP_SET_SIZE, h, size as u64, 0, 0, 0)
}

#[no_mangle]
pub unsafe extern "system" fn SetConsoleWindowInfo(h: Handle, absolute: Bool, rect: *const i16) -> Bool {
    call(OP_SET_WINDOW, h, (absolute != 0) as u64, rect as u64, 0, 0)
}

/// As big as the buffer allows, here: there's no screen to measure against.
#[no_mangle]
pub extern "system" fn GetLargestConsoleWindowSize(h: Handle) -> u32 {
    let mut info = [0i16; 11];
    if call(OP_INFO, h, info.as_mut_ptr() as u64, 0, 0, 0) == 0 {
        return 0;
    }
    pack([info[0], 300]) as u32
}

// -----| modes |-----

#[no_mangle]
pub unsafe extern "system" fn GetConsoleMode(h: Handle, mode: *mut u32) -> Bool {
    let mut m = 0u32;
    if call(OP_GET_MODE, h, &mut m as *mut u32 as u64, 0, 0, 0) == 0 {
        return 0;
    }
    out_u32(mode, m);
    1
}

#[no_mangle]
pub extern "system" fn SetConsoleMode(h: Handle, mode: u32) -> Bool {
    call(OP_SET_MODE, h, mode as u64, 0, 0, 0)
}

// -----| input |-----

#[no_mangle]
pub unsafe extern "system" fn ReadConsoleW(h: Handle, buffer: *mut u16, length: u32, read: *mut u32, _control: usize) -> Bool {
    let mut n = 0u32;
    let r = call(OP_READ_TEXT, h, buffer as u64, length as u64, &mut n as *mut u32 as u64, 0);
    out_u32(read, n);
    r
}

/// As UTF-8 bytes (the code page here).
#[no_mangle]
pub unsafe extern "system" fn ReadConsoleA(h: Handle, buffer: *mut u8, length: u32, read: *mut u32, _control: usize) -> Bool {
    unsafe { crate::ReadFile(h, buffer, length, read, 0) }
}

fn read_input(h: Handle, records: *mut u8, count: u32, read: *mut u32, flags: u64) -> Bool {
    let mut n = 0u32;
    let r = call(OP_READ_INPUT, h, records as u64, count as u64, &mut n as *mut u32 as u64, flags);
    out_u32(read, n);
    r
}

#[no_mangle]
pub extern "system" fn ReadConsoleInputW(h: Handle, records: *mut u8, count: u32, read: *mut u32) -> Bool {
    read_input(h, records, count, read, 0)
}

#[no_mangle]
pub extern "system" fn ReadConsoleInputA(h: Handle, records: *mut u8, count: u32, read: *mut u32) -> Bool {
    read_input(h, records, count, read, 0)
}

#[no_mangle]
pub extern "system" fn PeekConsoleInputW(h: Handle, records: *mut u8, count: u32, read: *mut u32) -> Bool {
    read_input(h, records, count, read, 3)
}

#[no_mangle]
pub extern "system" fn PeekConsoleInputA(h: Handle, records: *mut u8, count: u32, read: *mut u32) -> Bool {
    read_input(h, records, count, read, 3)
}

/// ReadConsoleInputEx (with CONSOLE_READ_NOWAIT): what's there, maybe nothing.
#[no_mangle]
pub extern "system" fn ReadConsoleInputExW(h: Handle, records: *mut u8, count: u32, read: *mut u32, flags: u16) -> Bool {
    read_input(h, records, count, read, if flags & 2 != 0 { 2 } else { 0 })
}

#[no_mangle]
pub extern "system" fn WriteConsoleInputW(h: Handle, records: *const u8, count: u32, written: *mut u32) -> Bool {
    let mut n = 0u32;
    let r = call(OP_WRITE_INPUT, h, records as u64, count as u64, &mut n as *mut u32 as u64, 0);
    out_u32(written, n);
    r
}

#[no_mangle]
pub extern "system" fn WriteConsoleInputA(h: Handle, records: *const u8, count: u32, written: *mut u32) -> Bool {
    WriteConsoleInputW(h, records, count, written)
}

#[no_mangle]
pub unsafe extern "system" fn GetNumberOfConsoleInputEvents(h: Handle, n: *mut u32) -> Bool {
    call(OP_INPUT_COUNT, h, n as u64, 0, 0, 0)
}

#[no_mangle]
pub extern "system" fn FlushConsoleInputBuffer(h: Handle) -> Bool {
    call(OP_FLUSH_INPUT, h, 0, 0, 0, 0)
}

// -----| the console itself |-----

/// A process keeps the console it started with: FreeConsole lets it go as
/// far as programs can tell, and AllocConsole takes it back.
static DETACHED: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

#[no_mangle]
pub extern "system" fn FreeConsole() -> Bool {
    DETACHED.store(true, core::sync::atomic::Ordering::Relaxed);
    1
}

#[no_mangle]
pub extern "system" fn AllocConsole() -> Bool {
    if !DETACHED.swap(false, core::sync::atomic::Ordering::Relaxed) {
        crate::SetLastError(5); // ERROR_ACCESS_DENIED: there's one already
        return 0;
    }
    1
}

#[no_mangle]
pub extern "system" fn AttachConsole(_pid: u32) -> Bool {
    AllocConsole()
}

/// The console window has no HWND of its own (yet).
#[no_mangle]
pub extern "system" fn GetConsoleWindow() -> Handle {
    0
}

/// Ctrl+C handlers: kept nowhere yet; Ctrl+C arrives as a key.
#[no_mangle]
pub extern "system" fn SetConsoleCtrlHandler(_handler: usize, _add: Bool) -> Bool {
    1
}

#[no_mangle]
pub unsafe extern "system" fn SetConsoleTitleW(title: *const u16) -> Bool {
    let t = unsafe { wstr(title) };
    call(OP_SET_TITLE, 0, t.as_ptr() as u64, t.len() as u64, 0, 0)
}

#[no_mangle]
pub unsafe extern "system" fn SetConsoleTitleA(title: *const u8) -> Bool {
    let u = widen(unsafe { core::ffi::CStr::from_ptr(title as *const _) }.to_bytes());
    call(OP_SET_TITLE, 0, u.buf.as_ptr() as u64, u.len as u64, 0, 0)
}

/// The title's length; as much as fits copied, NUL-terminated.
#[no_mangle]
pub unsafe extern "system" fn GetConsoleTitleW(out: *mut u16, cap: u32) -> u32 {
    if out.is_null() || cap == 0 {
        return 0;
    }
    let mut len = 0u32;
    if call(OP_GET_TITLE, 0, out as u64, (cap - 1) as u64, &mut len as *mut u32 as u64, 0) == 0 {
        return 0;
    }
    unsafe { *out.add(len.min(cap - 1) as usize) = 0 };
    len
}

#[no_mangle]
pub unsafe extern "system" fn GetConsoleTitleA(out: *mut u8, cap: u32) -> u32 {
    let mut w = [0u16; 512];
    let n = unsafe { GetConsoleTitleW(w.as_mut_ptr(), 512) } as usize;
    if out.is_null() || cap == 0 {
        return n as u32;
    }
    let m = n.min(cap as usize - 1);
    for i in 0..m {
        unsafe { *out.add(i) = if w[i] < 0x100 { w[i] as u8 } else { b'?' } };
    }
    unsafe { *out.add(m) = 0 };
    n as u32
}

#[no_mangle]
pub unsafe extern "system" fn GetConsoleOriginalTitleW(out: *mut u16, cap: u32) -> u32 {
    unsafe { GetConsoleTitleW(out, cap) }
}
