// -----| the console calls |-----
//
// VEIL_CONSOLE(op, handle, a, b, [c, d]): what kernel32's console functions
// stand on (console.rs, conbuf.rs). Output handles are screen buffers; input
// handles the console itself. Coordinates packed in a u32 are X in the low
// 16 bits and Y in the high, as Windows' COORD.

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::Ordering;

use nt_abi::{status, NtStatus};

use crate::conbuf::Cell;
use crate::console::{BufferObj, Console, InputRecord};
use crate::interrupts::syscall_stack_arg as stack;
use crate::nt_file::ptr_ok;
use crate::nt_objects::KernelObjectRef;
use crate::process::Process;

pub const OP_WRITE: u64 = 0;
pub const OP_INFO: u64 = 1;
pub const OP_SET_CURSOR: u64 = 2;
pub const OP_SET_ATTR: u64 = 3;
pub const OP_FILL: u64 = 4;
pub const OP_WRITE_CELLS: u64 = 5;
pub const OP_READ_CELLS: u64 = 6;
pub const OP_SCROLL: u64 = 7;
pub const OP_GET_MODE: u64 = 8;
pub const OP_SET_MODE: u64 = 9;
pub const OP_READ_INPUT: u64 = 10;
pub const OP_WRITE_INPUT: u64 = 11;
pub const OP_INPUT_COUNT: u64 = 12;
pub const OP_FLUSH_INPUT: u64 = 13;
pub const OP_SET_SIZE: u64 = 14;
pub const OP_SET_WINDOW: u64 = 15;
pub const OP_NEW_BUFFER: u64 = 16;
pub const OP_SET_ACTIVE: u64 = 17;
pub const OP_GET_CURSOR_INFO: u64 = 18;
pub const OP_SET_CURSOR_INFO: u64 = 19;
pub const OP_GET_TITLE: u64 = 20;
pub const OP_SET_TITLE: u64 = 21;
pub const OP_READ_TEXT: u64 = 22;
pub const OP_WRITE_BLOCK: u64 = 23;
pub const OP_READ_BLOCK: u64 = 24;
pub const OP_SET_CTRL_ROUTINE: u64 = 25;
pub const OP_GENERATE_CTRL: u64 = 26;
pub const OP_FREE: u64 = 27;
pub const OP_ALLOC: u64 = 28;
pub const OP_PTY_CREATE: u64 = 29;
pub const OP_PTY_RESIZE: u64 = 30;
pub const OP_PTY_CLOSE: u64 = 31;

enum End {
    Input(Arc<Console>),
    Output(Arc<BufferObj>),
}

fn end_of(proc: &Process, h: u64) -> Option<End> {
    match crate::nt_process::object(proc, h)? {
        KernelObjectRef::Console(c) => Some(End::Input(c)),
        KernelObjectRef::ConsoleBuffer(b) => Some(End::Output(b)),
        _ => None,
    }
}

fn output(proc: &Process, h: u64) -> Result<Arc<BufferObj>, NtStatus> {
    match end_of(proc, h) {
        Some(End::Output(b)) => Ok(b),
        // The console itself, where an output handle is wanted: its active buffer.
        Some(End::Input(c)) => Ok(c.active()),
        None => Err(status::INVALID_HANDLE),
    }
}

fn input(proc: &Process, h: u64) -> Result<Arc<Console>, NtStatus> {
    match end_of(proc, h) {
        Some(End::Input(c)) => Ok(c),
        Some(End::Output(b)) => b.console.upgrade().ok_or(status::INVALID_HANDLE),
        None => Err(status::INVALID_HANDLE),
    }
}

fn put<T: Copy>(p: u64, v: T) -> Result<(), NtStatus> {
    if p == 0 {
        return Ok(());
    }
    if !ptr_ok(p, core::mem::size_of::<T>()) {
        return Err(status::ACCESS_VIOLATION);
    }
    unsafe { core::ptr::write_unaligned(p as *mut T, v) };
    Ok(())
}

fn coord(v: u64) -> (usize, usize) {
    ((v & 0xFFFF) as u16 as usize, ((v >> 16) & 0xFFFF) as u16 as usize)
}

pub fn call(proc: &Process, op: u64, h: u64, a: u64, b: u64) -> NtStatus {
    let c = stack(4).unwrap_or(0);
    let d = stack(5).unwrap_or(0);
    let r = run(proc, op, h, a, b, c, d);
    // On a pseudo-console, the host sees whatever that did to the screen.
    if let Some(con) = proc.current_console() {
        con.render();
    }
    match r {
        Ok(()) => status::SUCCESS,
        Err(s) => s,
    }
}

/// A handle to a pipe end, reading or writing.
fn pipe_end(proc: &Process, h: u64, write: bool) -> Result<Arc<crate::nt_process::PipeEnd>, NtStatus> {
    match crate::nt_process::object(proc, h) {
        Some(KernelObjectRef::Pipe(end)) if end.write == write => Ok(end),
        Some(_) => Err(status::INVALID_PARAMETER),
        None => Err(status::INVALID_HANDLE),
    }
}

fn run(proc: &Process, op: u64, h: u64, a: u64, b: u64, c: u64, d: u64) -> Result<(), NtStatus> {
    match op {
        // (h, units*, count, written*)
        OP_WRITE => {
            let buf = output(proc, h)?;
            let n = b as usize;
            if n > 0 && !ptr_ok(a, n * 2) {
                return Err(status::ACCESS_VIOLATION);
            }
            let units: Vec<u16> = (0..n).map(|i| unsafe { core::ptr::read_unaligned((a as *const u16).add(i)) }).collect();
            buf.write_units(&units);
            put(c, n as u32)
        }
        // (h, CONSOLE_SCREEN_BUFFER_INFO*): size, cursor, attribute, window, largest window.
        OP_INFO => {
            let buf = output(proc, h)?;
            let s = buf.buf.lock();
            let (l, t, w, wh) = s.window;
            let info: [i16; 11] = [
                s.width as i16, s.height as i16, s.cursor.0 as i16, s.cursor.1 as i16, s.attr as i16,
                l as i16, t as i16, (l + w - 1) as i16, (t + wh - 1) as i16, s.width as i16, (s.height.min(wh.max(1) * 4)) as i16,
            ];
            put(a, info)
        }
        // (h, coord)
        OP_SET_CURSOR => {
            let buf = output(proc, h)?;
            let (x, y) = coord(a);
            let mut s = buf.buf.lock();
            if x >= s.width || y >= s.height {
                return Err(status::INVALID_PARAMETER);
            }
            s.cursor = (x, y);
            s.wrap_pending = false;
            drop(s);
            crate::apps::TERMINAL_OUTPUT.store(true, Ordering::Release);
            Ok(())
        }
        OP_SET_ATTR => {
            output(proc, h)?.buf.lock().set_attribute(a as u16);
            Ok(())
        }
        // (h, value | kind << 32, count, coord, written*): characters (kind 0)
        // or attributes (1), along the rows from coord.
        OP_FILL => {
            let buf = output(proc, h)?;
            let mut s = buf.buf.lock();
            let (x, y) = coord(c);
            let start = y * s.width + x;
            if x >= s.width || y >= s.height {
                return put(d, 0u32);
            }
            let end = (start + b as usize).min(s.cells.len());
            for cell in &mut s.cells[start..end] {
                if a >> 32 == 1 {
                    cell.attr = a as u16;
                } else {
                    cell.ch = a as u16;
                }
            }
            drop(s);
            crate::apps::TERMINAL_OUTPUT.store(true, Ordering::Release);
            put(d, (end - start) as u32)
        }
        // (h, data*, count, coord | kind << 32, written*): characters (kind 0) or attributes (1).
        OP_WRITE_CELLS | OP_READ_CELLS => {
            let buf = output(proc, h)?;
            let mut s = buf.buf.lock();
            let (x, y) = coord(c & 0xFFFF_FFFF);
            let attrs = c >> 32 == 1;
            if x >= s.width || y >= s.height {
                return put(d, 0u32);
            }
            let start = y * s.width + x;
            let n = (b as usize).min(s.cells.len() - start);
            if n > 0 && !ptr_ok(a, n * 2) {
                return Err(status::ACCESS_VIOLATION);
            }
            for i in 0..n {
                let p = unsafe { (a as *mut u16).add(i) };
                let cell = &mut s.cells[start + i];
                match (op == OP_WRITE_CELLS, attrs) {
                    (true, false) => cell.ch = unsafe { core::ptr::read_unaligned(p) },
                    (true, true) => cell.attr = unsafe { core::ptr::read_unaligned(p) },
                    (false, false) => unsafe { core::ptr::write_unaligned(p, cell.ch) },
                    (false, true) => unsafe { core::ptr::write_unaligned(p, cell.attr) },
                }
            }
            drop(s);
            if op == OP_WRITE_CELLS {
                crate::apps::TERMINAL_OUTPUT.store(true, Ordering::Release);
            }
            put(d, n as u32)
        }
        // (h, args*): source rect, has clip, clip rect, destination x and y,
        // fill character, fill attribute (13 i16s).
        OP_SCROLL => {
            let buf = output(proc, h)?;
            if !ptr_ok(a, 26) {
                return Err(status::ACCESS_VIOLATION);
            }
            let v: [i16; 13] = unsafe { core::ptr::read_unaligned(a as *const [i16; 13]) };
            scroll(&mut buf.buf.lock(), v);
            crate::apps::TERMINAL_OUTPUT.store(true, Ordering::Release);
            Ok(())
        }
        OP_GET_MODE => match end_of(proc, h).ok_or(status::INVALID_HANDLE)? {
            End::Input(c) => put(a, c.input_mode.load(Ordering::Relaxed)),
            End::Output(b) => put(a, b.buf.lock().mode),
        },
        OP_SET_MODE => match end_of(proc, h).ok_or(status::INVALID_HANDLE)? {
            End::Input(c) => {
                c.input_mode.store(a as u32, Ordering::Relaxed);
                Ok(())
            }
            End::Output(b) => {
                b.buf.lock().mode = a as u32;
                Ok(())
            }
        },
        // (h, records*, count, read*, flags: 1 peek, 2 don't wait)
        OP_READ_INPUT => {
            let con = input(proc, h)?;
            let peek = d & 1 != 0;
            let wait = d & 2 == 0;
            if b > 0 && !ptr_ok(a, b as usize * 20) {
                return Err(status::ACCESS_VIOLATION);
            }
            let records = loop {
                let r = con.read_input(b as usize, peek);
                if !r.is_empty() || !wait || b == 0 || !con.is_live() {
                    break r;
                }
                crate::loader::kill_if_requested();
                crate::nt_syscalls::wait_a_little();
            };
            for (i, r) in records.iter().enumerate() {
                unsafe { core::ptr::copy_nonoverlapping(r.as_ptr(), (a as *mut u8).add(i * 20), 20) };
            }
            put(c, records.len() as u32)
        }
        OP_WRITE_INPUT => {
            let con = input(proc, h)?;
            if b > 0 && !ptr_ok(a, b as usize * 20) {
                return Err(status::ACCESS_VIOLATION);
            }
            let records: Vec<InputRecord> = (0..b as usize).map(|i| unsafe { core::ptr::read_unaligned((a as *const InputRecord).add(i)) }).collect();
            con.push_input(&records);
            put(c, b as u32)
        }
        OP_INPUT_COUNT => put(a, input(proc, h)?.input_count() as u32),
        OP_FLUSH_INPUT => {
            input(proc, h)?.flush_input();
            Ok(())
        }
        // (h, coord: the new size). Not smaller than the window.
        OP_SET_SIZE => {
            let buf = output(proc, h)?;
            let (w, hgt) = coord(a);
            let mut s = buf.buf.lock();
            if w == 0 || hgt == 0 || w < s.window.2 || hgt < s.window.3 {
                return Err(status::INVALID_PARAMETER);
            }
            let changed = (s.width, s.height) != (w, hgt);
            s.resize(w, hgt);
            drop(s);
            if changed {
                if let Some(con) = buf.console.upgrade().filter(|c| c.is_active(&buf)) {
                    con.size_event(w, hgt);
                }
            }
            Ok(())
        }
        // (h, absolute, SMALL_RECT*): the window, within the buffer.
        OP_SET_WINDOW => {
            let buf = output(proc, h)?;
            if !ptr_ok(b, 8) {
                return Err(status::ACCESS_VIOLATION);
            }
            let r: [i16; 4] = unsafe { core::ptr::read_unaligned(b as *const [i16; 4]) };
            let mut s = buf.buf.lock();
            let (l, t, w, wh) = s.window;
            let (left, top, right, bottom) = if a != 0 {
                (r[0] as i64, r[1] as i64, r[2] as i64, r[3] as i64)
            } else {
                (l as i64 + r[0] as i64, t as i64 + r[1] as i64, (l + w - 1) as i64 + r[2] as i64, (t + wh - 1) as i64 + r[3] as i64)
            };
            if left < 0 || top < 0 || right < left || bottom < top || right as usize >= s.width || bottom as usize >= s.height {
                return Err(status::INVALID_PARAMETER);
            }
            let window = (left as usize, top as usize, (right - left + 1) as usize, (bottom - top + 1) as usize);
            // A buffer just the size of its window, with the window shrinking
            // from the corner, shrinks with it (as Windows' does).
            let follows = (s.window.0, s.window.1) == (0, 0) && (s.width, s.height) == (s.window.2, s.window.3)
                && (window.0, window.1) == (0, 0);
            s.window = window;
            if follows && (window.2, window.3) != (s.width, s.height) {
                s.resize(window.2, window.3);
                drop(s);
                if let Some(con) = buf.console.upgrade().filter(|c| c.is_active(&buf)) {
                    con.size_event(window.2, window.3);
                }
            }
            Ok(())
        }
        // (_, CtrlRoutine): where control events start a thread in this process.
        OP_SET_CTRL_ROUTINE => {
            proc.ctrl_routine.store(a, Ordering::Release);
            Ok(())
        }
        // (_, event, process group): CTRL_C_EVENT or CTRL_BREAK_EVENT to the
        // processes on the caller's console; a group names one of them.
        OP_GENERATE_CTRL => {
            if a > 1 {
                return Err(status::INVALID_PARAMETER);
            }
            let con = proc.current_console().ok_or(status::INVALID_HANDLE)?;
            if !con.ctrl_event(a as u32, b as u32) {
                return Err(0xC000_000Bu32 as NtStatus); // STATUS_INVALID_CID
            }
            Ok(())
        }
        // CreatePseudoConsole (_, input pipe's read end, output pipe's write
        // end, size coord, HPCON out*): the console, by a handle of the host's.
        OP_PTY_CREATE => {
            let (w, hgt) = coord(b);
            if w == 0 || hgt == 0 {
                return Err(status::INVALID_PARAMETER);
            }
            if !ptr_ok(c, 8) {
                return Err(status::ACCESS_VIOLATION);
            }
            let input = pipe_end(proc, h, false)?;
            let output = pipe_end(proc, a, true)?;
            // Ends of its own: the host may close its handles.
            let con = Console::new_pty(
                crate::nt_process::PipeEnd::reader(input.pipe.clone()),
                crate::nt_process::PipeEnd::writer(output.pipe.clone()),
                w,
                hgt,
            );
            let handle = proc.handles.lock().allocate_ref(KernelObjectRef::Console(con), 0x0012_019F, crate::nt_objects::HANDLE_NO_INHERIT);
            put(c, handle as u64)
        }
        OP_PTY_RESIZE | OP_PTY_CLOSE => {
            let con = input(proc, h)?;
            if con.pty().is_none() {
                return Err(status::INVALID_HANDLE);
            }
            if op == OP_PTY_CLOSE {
                con.close_pty();
                proc.handles.lock().close(h as u32);
                return Ok(());
            }
            let (w, hgt) = coord(a);
            if w == 0 || hgt == 0 {
                return Err(status::INVALID_PARAMETER);
            }
            con.resize_pty(w, hgt);
            Ok(())
        }
        // FreeConsole: the process leaves its console (its handles to it
        // stay). (_, 1): AttachConsole, back to the console it started with.
        OP_FREE if a == 1 => {
            if proc.current_console().is_some() {
                return Err(status::ACCESS_DENIED);
            }
            if proc.console.is_none() {
                return Err(status::INVALID_HANDLE);
            }
            *proc.console_now.lock() = None;
            Ok(())
        }
        OP_FREE => {
            if proc.current_console().is_none() {
                return Err(status::INVALID_PARAMETER);
            }
            *proc.console_now.lock() = Some(None);
            Ok(())
        }
        // AllocConsole (_, input handle out*, output handle out*): a console of
        // the process's own, not shown in a terminal; only if it has none.
        OP_ALLOC => {
            if proc.current_console().is_some() {
                return Err(status::ACCESS_DENIED);
            }
            if !ptr_ok(a, 8) || !ptr_ok(b, 8) {
                return Err(status::ACCESS_VIOLATION);
            }
            let con = Console::new(proc.pid);
            let (hi, ho) = {
                let mut handles = proc.handles.lock();
                let hi = handles.allocate_ref(KernelObjectRef::Console(con.clone()), 0x0012_019F, 0);
                let ho = handles.allocate_ref(KernelObjectRef::ConsoleBuffer(con.active()), 0x0012_019F, 0);
                (hi, ho)
            };
            *proc.console_now.lock() = Some(Some(con));
            put(a, hi as u64)?;
            put(b, ho as u64)
        }
        // (any console handle, out*)
        OP_NEW_BUFFER => {
            let con = match end_of(proc, h) {
                Some(_) => input(proc, h)?,
                None => proc.current_console().ok_or(status::INVALID_HANDLE)?,
            };
            let buf = con.new_buffer();
            let handle = proc.handles.lock().allocate_ref(KernelObjectRef::ConsoleBuffer(buf), 0x001F_01FF, 0);
            put(a, handle as u64)
        }
        OP_SET_ACTIVE => {
            let buf = output(proc, h)?;
            let con = buf.console.upgrade().ok_or(status::INVALID_HANDLE)?;
            con.set_active(buf);
            Ok(())
        }
        // CONSOLE_CURSOR_INFO: size, visible.
        OP_GET_CURSOR_INFO => {
            let buf = output(proc, h)?;
            let s = buf.buf.lock();
            put(a, [s.cursor_size, s.cursor_visible as u32])
        }
        OP_SET_CURSOR_INFO => {
            let buf = output(proc, h)?;
            if !(1..=100).contains(&a) {
                return Err(status::INVALID_PARAMETER);
            }
            let mut s = buf.buf.lock();
            s.cursor_size = a as u32;
            s.cursor_visible = b != 0;
            Ok(())
        }
        // (console, out*, capacity, length*)
        OP_GET_TITLE => {
            let con = proc.current_console().ok_or(status::INVALID_HANDLE)?;
            let t = con.title.lock().clone();
            let n = t.len().min(b as usize);
            if n > 0 && !ptr_ok(a, n * 2) {
                return Err(status::ACCESS_VIOLATION);
            }
            unsafe { core::ptr::copy_nonoverlapping(t.as_ptr(), a as *mut u16, n) };
            put(c, t.len() as u32)
        }
        OP_SET_TITLE => {
            let con = proc.current_console().ok_or(status::INVALID_HANDLE)?;
            if b > 0 && !ptr_ok(a, b as usize * 2) {
                return Err(status::ACCESS_VIOLATION);
            }
            let t: Vec<u16> = (0..b as usize).map(|i| unsafe { core::ptr::read_unaligned((a as *const u16).add(i)) }).collect();
            *con.title.lock() = t;
            Ok(())
        }
        // (h, units*, capacity, read*): ReadConsole, waiting for text.
        OP_READ_TEXT => {
            let con = input(proc, h)?;
            if b > 0 && !ptr_ok(a, b as usize * 2) {
                return Err(status::ACCESS_VIOLATION);
            }
            let text = loop {
                if let Some(t) = con.read_text(b as usize) {
                    break t;
                }
                if !con.is_live() {
                    break Vec::new();
                }
                crate::loader::kill_if_requested();
                crate::nt_syscalls::wait_a_little();
            };
            unsafe { core::ptr::copy_nonoverlapping(text.as_ptr(), a as *mut u16, text.len()) };
            put(c, text.len() as u32)
        }
        // WriteConsoleOutput / ReadConsoleOutput: (h, CHAR_INFO*, size coord, from coord, SMALL_RECT* in/out)
        OP_WRITE_BLOCK | OP_READ_BLOCK => {
            let buf = output(proc, h)?;
            let (bw, bh) = coord(b);
            let (fx, fy) = coord(c);
            if !ptr_ok(d, 8) || !ptr_ok(a, bw * bh * 4) {
                return Err(status::ACCESS_VIOLATION);
            }
            let r: [i16; 4] = unsafe { core::ptr::read_unaligned(d as *const [i16; 4]) };
            let mut s = buf.buf.lock();
            let left = (r[0].max(0) as usize).min(s.width - 1);
            let top = (r[1].max(0) as usize).min(s.height - 1);
            let right = (r[2].max(0) as usize).min(s.width - 1).min(left + bw.saturating_sub(fx).saturating_sub(1));
            let bottom = (r[3].max(0) as usize).min(s.height - 1).min(top + bh.saturating_sub(fy).saturating_sub(1));
            for y in top..=bottom {
                for x in left..=right {
                    let i = (fy + y - top) * bw + (fx + x - left);
                    let p = unsafe { (a as *mut u16).add(i * 2) };
                    if op == OP_WRITE_BLOCK {
                        let ch = unsafe { core::ptr::read_unaligned(p) };
                        let attr = unsafe { core::ptr::read_unaligned(p.add(1)) };
                        let w = s.width;
                        s.cells[y * w + x] = Cell { ch, attr, bold: false };
                    } else {
                        let cell = s.cells[y * s.width + x];
                        unsafe {
                            core::ptr::write_unaligned(p, cell.ch);
                            core::ptr::write_unaligned(p.add(1), cell.attr);
                        }
                    }
                }
            }
            drop(s);
            if op == OP_WRITE_BLOCK {
                crate::apps::TERMINAL_OUTPUT.store(true, Ordering::Release);
            }
            put(d, [left as i16, top as i16, right as i16, bottom as i16])
        }
        _ => Err(status::INVALID_PARAMETER),
    }
}

/// ScrollConsoleScreenBuffer: the source rectangle's cells move to the
/// destination; what they leave, inside the clip, takes the fill. `v`: source
/// (4), has clip, clip (4), destination x and y, fill character, fill attribute.
fn scroll(s: &mut crate::conbuf::ScreenBuffer, v: [i16; 13]) {
    let (w, h) = (s.width as i64, s.height as i64);
    let clamp = |r: [i64; 4]| [r[0].max(0), r[1].max(0), r[2].min(w - 1), r[3].min(h - 1)];
    let src = clamp([v[0] as i64, v[1] as i64, v[2] as i64, v[3] as i64]);
    let clip = if v[4] != 0 { clamp([v[5] as i64, v[6] as i64, v[7] as i64, v[8] as i64]) } else { [0, 0, w - 1, h - 1] };
    let (dx, dy) = (v[9] as i64, v[10] as i64);
    let fill = Cell { ch: v[11] as u16, attr: v[12] as u16, bold: false };
    if src[0] > src[2] || src[1] > src[3] {
        return;
    }
    let old = s.cells.clone();
    let inside = |x: i64, y: i64, r: [i64; 4]| x >= r[0] && x <= r[2] && y >= r[1] && y <= r[3];
    // The fill first, over the source (within the clip), then the moved cells.
    for y in src[1]..=src[3] {
        for x in src[0]..=src[2] {
            if inside(x, y, clip) {
                s.cells[(y * w + x) as usize] = fill;
            }
        }
    }
    for y in src[1]..=src[3] {
        for x in src[0]..=src[2] {
            let (tx, ty) = (dx + x - src[0], dy + y - src[1]);
            if tx >= 0 && ty >= 0 && tx < w && ty < h && inside(tx, ty, clip) {
                s.cells[(ty * w + tx) as usize] = old[(y * w + x) as usize];
            }
        }
    }
}
