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

const OP_FREE: u64 = 27;
const OP_ALLOC: u64 = 28;
const OP_PTY_CREATE: u64 = 29;
const OP_PTY_RESIZE: u64 = 30;
const OP_PTY_CLOSE: u64 = 31;

/// Whether `h` is a console handle (input or a screen buffer).
pub(crate) fn is_console_handle(h: Handle) -> bool {
    let mut m = 0u32;
    h != 0 && unsafe { VeilConsole(OP_GET_MODE, h, &mut m as *mut u32 as u64, 0, 0, 0) } == 0
}

fn hresult(s: i32) -> i32 {
    match s as u32 {
        0 => 0,
        0xC000_000D => 0x8007_0057u32 as i32, // E_INVALIDARG
        0xC000_0008 => 0x8007_0006u32 as i32, // E_HANDLE
        _ => 0x8000_4005u32 as i32,           // E_FAIL
    }
}

// -----| pseudo-consoles (src/pty.rs) |-----

/// CreatePseudoConsole(size, input pipe's read end, output pipe's write end,
/// flags, HPCON out*): an HPCON is a handle to the console.
#[no_mangle]
pub unsafe extern "system" fn CreatePseudoConsole(size: u32, input: Handle, output: Handle, _flags: u32, out: *mut Handle) -> i32 {
    if out.is_null() {
        return 0x8007_0057u32 as i32;
    }
    let mut h: Handle = 0;
    let s = unsafe { VeilConsole(OP_PTY_CREATE, input, output, size as u64, &mut h as *mut Handle as u64, 0) };
    if s == 0 {
        unsafe { *out = h };
    }
    hresult(s)
}

#[no_mangle]
pub extern "system" fn ResizePseudoConsole(pc: Handle, size: u32) -> i32 {
    hresult(unsafe { VeilConsole(OP_PTY_RESIZE, pc, size as u64, 0, 0, 0) })
}

/// The host's reads end once it has the rest; programs in it end.
#[no_mangle]
pub extern "system" fn ClosePseudoConsole(pc: Handle) {
    unsafe { VeilConsole(OP_PTY_CLOSE, pc, 0, 0, 0, 0) };
}

#[no_mangle]
pub extern "system" fn FreeConsole() -> Bool {
    call(OP_FREE, 0, 0, 0, 0, 0)
}

/// A console of the process's own, not shown anywhere: standard handles that
/// were console handles move to it; redirected ones stay (as on Windows).
#[no_mangle]
pub extern "system" fn AllocConsole() -> Bool {
    let (mut hi, mut ho): (Handle, Handle) = (0, 0);
    let s = unsafe { VeilConsole(OP_ALLOC, 0, &mut hi as *mut Handle as u64, &mut ho as *mut Handle as u64, 0, 0) };
    if s != 0 {
        return fail(s);
    }
    for (which, h) in [(-10i32 as u32, hi), (-11i32 as u32, ho), (-12i32 as u32, ho)] {
        let old = crate::GetStdHandle(which);
        let mut m = 0u32;
        let is_console = unsafe { VeilConsole(OP_GET_MODE, old, &mut m as *mut u32 as u64, 0, 0, 0) } == 0;
        if old == 0 || old == crate::INVALID_HANDLE_VALUE || is_console {
            crate::SetStdHandle(which, h);
        }
    }
    1
}

/// Only back to the console the process started with (the parent's, as a
/// rule), whichever process is asked for.
#[no_mangle]
pub extern "system" fn AttachConsole(_pid: u32) -> Bool {
    call(OP_FREE, 0, 1, 0, 0, 0)
}

/// The console window has no HWND of its own (yet).
#[no_mangle]
pub extern "system" fn GetConsoleWindow() -> Handle {
    0
}

// -----| Ctrl+C and Ctrl+Break |-----

const OP_SET_CTRL_ROUTINE: u64 = 25;
const OP_GENERATE_CTRL: u64 = 26;

/// The handlers, oldest first; the newest runs first. A spin lock guards
/// them: a control event runs them on a thread of its own.
struct Handlers {
    list: [usize; 64],
    count: usize,
}
static HANDLERS: SpinCell<Handlers> = SpinCell::new(Handlers { list: [0; 64], count: 0 });
struct SpinCell<T> {
    locked: core::sync::atomic::AtomicBool,
    value: core::cell::UnsafeCell<T>,
}
unsafe impl<T> Sync for SpinCell<T> {}
impl<T> SpinCell<T> {
    const fn new(value: T) -> Self {
        SpinCell { locked: core::sync::atomic::AtomicBool::new(false), value: core::cell::UnsafeCell::new(value) }
    }
    fn with<R>(&self, f: impl FnOnce(&mut T) -> R) -> R {
        use core::sync::atomic::Ordering;
        while self.locked.compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() {
            core::hint::spin_loop();
        }
        let r = f(unsafe { &mut *self.value.get() });
        self.locked.store(false, Ordering::Release);
        r
    }
}
/// SetConsoleCtrlHandler(NULL, TRUE): Ctrl+C runs no handlers.
static IGNORE_CTRL_C: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

#[no_mangle]
pub extern "system" fn SetConsoleCtrlHandler(handler: usize, add: Bool) -> Bool {
    if handler == 0 {
        IGNORE_CTRL_C.store(add != 0, core::sync::atomic::Ordering::Relaxed);
        return 1;
    }
    let ok = HANDLERS.with(|h| {
        if add != 0 {
            if h.count == h.list.len() {
                return false;
            }
            h.list[h.count] = handler;
            h.count += 1;
            true
        } else {
            match h.list[..h.count].iter().rposition(|&x| x == handler) {
                Some(i) => {
                    h.list.copy_within(i + 1..h.count, i);
                    h.count -= 1;
                    true
                }
                None => false,
            }
        }
    });
    if ok {
        1
    } else {
        crate::SetLastError(87); // ERROR_INVALID_PARAMETER
        0
    }
}

/// Where a control event starts: the newest handler first, until one says
/// it's handled; if none does, the program ends, as on Windows.
extern "system" fn ctrl_routine(event: usize) -> u32 {
    let event = event as u32;
    if event == 0 && IGNORE_CTRL_C.load(core::sync::atomic::Ordering::Relaxed) {
        return 0;
    }
    let (list, count) = HANDLERS.with(|h| (h.list, h.count));
    for &f in list[..count].iter().rev() {
        let handler: extern "system" fn(u32) -> Bool = unsafe { core::mem::transmute(f) };
        // Close, logoff, shutdown: the program ends whatever its handler says.
        if handler(event) != 0 && event < 2 {
            return 0;
        }
    }
    crate::ExitProcess(0xC000_013A) // STATUS_CONTROL_C_EXIT
}

/// Tell the kernel where control events go (kernel32's DllMain).
pub(crate) fn register_ctrl_routine() {
    unsafe { VeilConsole(OP_SET_CTRL_ROUTINE, 0, ctrl_routine as *const () as u64, 0, 0, 0) };
}

#[no_mangle]
pub extern "system" fn GenerateConsoleCtrlEvent(event: u32, group: u32) -> Bool {
    let s = unsafe { VeilConsole(OP_GENERATE_CTRL, 0, event as u64, group as u64, 0, 0) };
    match s as u32 {
        0 => 1,
        // A group that isn't on this console: Windows says ERROR_BAD_FORMAT.
        0xC000_000B => {
            crate::SetLastError(11);
            0
        }
        _ => fail(s),
    }
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
