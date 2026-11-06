// =============================================================================
// VEILOS: consoles for Windows console programs (WIN32_DESIGN.md, W2-W3, S5)
// =============================================================================
//
// A console-subsystem program gets one `Console` (its children share it).
// Like Windows', it has two sides, each with its own handles:
//
//   Input (CONIN$, standard input): a queue of input records, key events
//   above all, and a mode. In line mode a read takes keys until Enter,
//   editing the line as it goes (Backspace) and echoing it into the active
//   screen buffer, as Windows' console host does, only while a read waits.
//   Without it, a read takes the characters typed so far; with
//   ENABLE_VIRTUAL_TERMINAL_INPUT, keys like the arrows come as the escape
//   sequences a terminal sends.
//
//   Output (CONOUT$, standard output and error): screen buffers (conbuf.rs),
//   one of them active: the one a terminal window shows. Programs can make
//   more and switch between them.
//
// What goes to the active buffer is also copied to the serial log, a line at
// a time with the program's pid (the tests read it there).

use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use spin::Mutex;

use crate::conbuf::ScreenBuffer;

/// No terminal attached (yet).
const NO_TERMINAL: usize = usize::MAX;

pub const ENABLE_PROCESSED_INPUT: u32 = 0x1;
pub const ENABLE_LINE_INPUT: u32 = 0x2;
pub const ENABLE_ECHO_INPUT: u32 = 0x4;
pub const ENABLE_VIRTUAL_TERMINAL_INPUT: u32 = 0x200;
const DEFAULT_INPUT_MODE: u32 = 0x1F7;

/// A console's size before a terminal shows it: Windows' default window,
/// with rows of history above.
pub const DEFAULT_COLUMNS: usize = 120;
pub const DEFAULT_ROWS: usize = 30;
pub const HISTORY_ROWS: usize = 1000;

/// INPUT_RECORD: EventType (u16, padded to 4), then the event (16 bytes).
pub type InputRecord = [u8; 20];

pub const KEY_EVENT: u16 = 1;
pub const WINDOW_BUFFER_SIZE_EVENT: u16 = 4;

/// A KEY_EVENT record.
pub fn key_record(down: bool, vk: u16, ch: u16, state: u32) -> InputRecord {
    let mut r = [0u8; 20];
    r[0..2].copy_from_slice(&KEY_EVENT.to_le_bytes());
    r[4..8].copy_from_slice(&(down as u32).to_le_bytes());
    r[8..10].copy_from_slice(&1u16.to_le_bytes()); // repeat count
    r[10..12].copy_from_slice(&vk.to_le_bytes());
    r[14..16].copy_from_slice(&ch.to_le_bytes());
    r[16..20].copy_from_slice(&state.to_le_bytes());
    r
}

/// A key event's (down, virtual key, character, control state).
fn key_of(r: &InputRecord) -> Option<(bool, u16, u16, u32)> {
    if u16::from_le_bytes([r[0], r[1]]) != KEY_EVENT {
        return None;
    }
    Some((
        u32::from_le_bytes(r[4..8].try_into().unwrap()) != 0,
        u16::from_le_bytes([r[10], r[11]]),
        u16::from_le_bytes([r[14], r[15]]),
        u32::from_le_bytes(r[16..20].try_into().unwrap()),
    ))
}

/// A screen buffer, as handles to it see it.
pub struct BufferObj {
    pub buf: Mutex<ScreenBuffer>,
    pub console: Weak<Console>,
}

impl BufferObj {
    /// Text into this buffer. Shown (and logged) if it's the active one.
    pub fn write_units(self: &Arc<Self>, units: &[u16]) {
        let title = {
            let mut b = self.buf.lock();
            b.write(units);
            b.title.take()
        };
        if let Some(console) = self.console.upgrade() {
            if let Some(t) = title {
                *console.title.lock() = t;
            }
            if console.is_active(self) {
                console.to_serial(&String::from_utf16_lossy(units));
                crate::apps::TERMINAL_OUTPUT.store(true, Ordering::Release);
            }
        }
    }

    pub fn write_utf8(self: &Arc<Self>, bytes: &[u8]) {
        let units: Vec<u16> = String::from_utf8_lossy(bytes).encode_utf16().collect();
        self.write_units(&units);
    }
}

pub struct Console {
    pid: u32,
    /// TERMINAL_POOL slot showing this console.
    terminal: AtomicUsize,
    /// The serial log's unfinished line.
    serial_line: Mutex<String>,
    /// The program has exited: no more input or output.
    finished: AtomicBool,
    me: Weak<Console>,
    active: Mutex<Arc<BufferObj>>,
    input: Mutex<VecDeque<InputRecord>>,
    pub input_mode: AtomicU32,
    /// Line mode: the line being typed, and typed lines (with "\r\n") not
    /// yet read.
    editing: Mutex<Vec<u16>>,
    ready: Mutex<VecDeque<u16>>,
    pub title: Mutex<Vec<u16>>,
}

impl Console {
    pub fn new(pid: u32) -> Arc<Self> {
        Arc::new_cyclic(|me: &Weak<Console>| Console {
            pid,
            terminal: AtomicUsize::new(NO_TERMINAL),
            serial_line: Mutex::new(String::new()),
            finished: AtomicBool::new(false),
            me: me.clone(),
            active: Mutex::new(Arc::new(BufferObj {
                buf: Mutex::new(ScreenBuffer::new(DEFAULT_COLUMNS, HISTORY_ROWS, DEFAULT_ROWS)),
                console: me.clone(),
            })),
            input: Mutex::new(VecDeque::new()),
            input_mode: AtomicU32::new(DEFAULT_INPUT_MODE),
            editing: Mutex::new(Vec::new()),
            ready: Mutex::new(VecDeque::new()),
            title: Mutex::new(Vec::new()),
        })
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }

    pub fn is_live(&self) -> bool {
        !self.finished.load(Ordering::Acquire)
    }

    // -----| screen buffers |-----

    pub fn active(&self) -> Arc<BufferObj> {
        self.active.lock().clone()
    }

    pub fn is_active(&self, b: &Arc<BufferObj>) -> bool {
        Arc::ptr_eq(&self.active.lock(), b)
    }

    /// CreateConsoleScreenBuffer: the size of the active one.
    pub fn new_buffer(&self) -> Arc<BufferObj> {
        let (w, h, rows) = {
            let a = self.active();
            let b = a.buf.lock();
            (b.width, b.height, b.window.3)
        };
        Arc::new(BufferObj { buf: Mutex::new(ScreenBuffer::new(w, h, rows)), console: self.me.clone() })
    }

    pub fn set_active(&self, b: Arc<BufferObj>) {
        *self.active.lock() = b;
        crate::apps::TERMINAL_OUTPUT.store(true, Ordering::Release);
    }

    /// Show this console in terminal pool slot `slot`, `columns` by `rows`:
    /// the buffer takes the terminal's width and the window its height. The
    /// caller also sets that terminal's `console`.
    pub fn attach(&self, slot: usize, columns: usize, rows: usize) {
        self.terminal.store(slot, Ordering::Release);
        self.resize(columns, rows);
    }

    /// The terminal's size changed: the active buffer follows, and the
    /// program hears of it (WINDOW_BUFFER_SIZE_EVENT).
    pub fn resize(&self, columns: usize, rows: usize) {
        let (columns, rows) = (columns.max(10), rows.max(2));
        let a = self.active();
        let changed = {
            let mut b = a.buf.lock();
            let changed = b.width != columns || b.window.3 != rows;
            if b.width != columns {
                let h = b.height;
                b.resize(columns, h);
            }
            let (l, t, _, _) = b.window;
            let top = t.min(b.height - rows);
            b.window = (l.min(columns - 1), top, columns.min(b.width), rows.min(b.height));
            // Keep the cursor in view.
            let (_, y) = b.cursor;
            if y >= b.window.1 + rows {
                b.window.1 = y + 1 - rows;
            }
            changed
        };
        if changed {
            let mut r = [0u8; 20];
            r[0..2].copy_from_slice(&WINDOW_BUFFER_SIZE_EVENT.to_le_bytes());
            r[4..6].copy_from_slice(&(columns as i16).to_le_bytes());
            r[6..8].copy_from_slice(&(HISTORY_ROWS as i16).to_le_bytes());
            self.input.lock().push_back(r);
        }
    }

    // -----| output |-----

    /// Program output through WriteFile: UTF-8 (anything else shows as U+FFFD).
    pub fn write(&self, bytes: &[u8]) {
        self.active().write_utf8(bytes);
    }

    // -----| input |-----

    /// Input records from the terminal (or WriteConsoleInput).
    pub fn push_input(&self, records: &[InputRecord]) {
        self.input.lock().extend(records.iter().copied());
    }

    pub fn input_count(&self) -> usize {
        self.input.lock().len()
    }

    pub fn flush_input(&self) {
        self.input.lock().clear();
        self.editing.lock().clear();
        self.ready.lock().clear();
    }

    /// Up to `max` records, removed unless `peek`.
    pub fn read_input(&self, max: usize, peek: bool) -> Vec<InputRecord> {
        let mut q = self.input.lock();
        let n = q.len().min(max);
        if peek {
            q.iter().take(n).copied().collect()
        } else {
            q.drain(..n).collect()
        }
    }

    /// Text for ReadConsole/ReadFile, up to `max` units: None while there's
    /// nothing to give yet (the caller waits and asks again).
    pub fn read_text(&self, max: usize) -> Option<Vec<u16>> {
        let mode = self.input_mode.load(Ordering::Relaxed);
        let max = max.max(1);
        // A typed line not all read yet comes first.
        {
            let mut ready = self.ready.lock();
            if !ready.is_empty() {
                let n = ready.len().min(max);
                return Some(ready.drain(..n).collect());
            }
        }
        if mode & ENABLE_LINE_INPUT != 0 {
            self.edit_line(mode);
            let mut ready = self.ready.lock();
            if ready.is_empty() {
                return None;
            }
            let n = ready.len().min(max);
            return Some(ready.drain(..n).collect());
        }
        // Characters as they come: what's there now, at least one.
        let mut out = Vec::new();
        let mut q = self.input.lock();
        while out.len() < max {
            let Some(r) = q.pop_front() else { break };
            let Some((down, vk, ch, state)) = key_of(&r) else { continue };
            if !down {
                continue;
            }
            out.extend(key_text(vk, ch, state, mode));
        }
        if out.is_empty() { None } else { Some(out) }
    }

    /// Line mode: take the keys waiting into the line being typed; Enter
    /// finishes it. Echoed into the active buffer, if ENABLE_ECHO_INPUT.
    fn edit_line(&self, mode: u32) {
        let echo = mode & ENABLE_ECHO_INPUT != 0;
        loop {
            let Some(r) = self.input.lock().pop_front() else { return };
            let Some((down, vk, ch, state)) = key_of(&r) else { continue };
            if !down {
                continue;
            }
            match ch {
                0x0D => {
                    let mut line = core::mem::take(&mut *self.editing.lock());
                    line.extend([0x0D, 0x0A]);
                    if echo {
                        self.active().write_units(&[0x0D, 0x0A]);
                    }
                    self.ready.lock().extend(line);
                    return;
                }
                0x08 => {
                    if self.editing.lock().pop().is_some() && echo {
                        self.active().write_units(&[0x08, 0x20, 0x08]);
                    }
                }
                0 => {
                    let _ = (vk, state);
                }
                c => {
                    self.editing.lock().push(c);
                    if echo {
                        self.active().write_units(&[c]);
                    }
                }
            }
        }
    }

    /// The program has exited: the terminal keeps what it showed, as lines,
    /// and goes back to its prompt.
    pub fn finish(&self, code: Option<i32>) {
        self.finished.store(true, Ordering::Release);
        if !self.serial_line.lock().is_empty() {
            self.to_serial("\n");
        }
        let line = match code {
            Some(c) => alloc::format!("[exited with code {}]", c),
            None => String::from("[exited]"),
        };
        let shown = self.shown_lines();
        if let Some(term) = crate::apps::TERMINAL_POOL.get(self.terminal.load(Ordering::Acquire)) {
            let mut term = term.lock();
            term.history.extend(shown);
            term.history.push(line);
            term.history.push(String::new());
            if term.console.as_ref().is_some_and(|c| c.pid == self.pid) {
                term.console = None;
            }
            crate::apps::TERMINAL_OUTPUT.store(true, Ordering::Release);
        }
    }

    /// The active buffer's rows as text, from the first written to the
    /// cursor's (or the last written, if later).
    fn shown_lines(&self) -> Vec<String> {
        let a = self.active();
        let b = a.buf.lock();
        let text = |y: usize| -> String {
            let units: Vec<u16> = (0..b.width).map(|x| b.cell(x, y).ch).collect();
            String::from_utf16_lossy(&units).trim_end().into()
        };
        let mut last = b.cursor.1;
        for y in (0..b.height).rev() {
            if !text(y).is_empty() {
                last = last.max(y);
                break;
            }
        }
        let first = (0..=last).find(|&y| !text(y).is_empty()).unwrap_or(last);
        let mut lines: Vec<String> = (first..=last).map(text).collect();
        while lines.last().is_some_and(|l| l.is_empty()) {
            lines.pop();
        }
        lines
    }

    pub fn to_serial(&self, text: &str) {
        let mut pending = self.serial_line.lock();
        for c in text.chars() {
            if c == '\n' {
                crate::serial_println!("[CONSOLE {}] {}", self.pid, pending);
                pending.clear();
            } else if c != '\r' {
                pending.push(c);
            }
        }
    }
}

const SHIFT_PRESSED: u32 = 0x10;
const LEFT_CTRL_PRESSED: u32 = 0x08;
const RIGHT_CTRL_PRESSED: u32 = 0x04;

/// A key, as characters: its own, or with ENABLE_VIRTUAL_TERMINAL_INPUT the
/// escape sequence a terminal sends for it.
fn key_text(vk: u16, ch: u16, state: u32, mode: u32) -> Vec<u16> {
    if mode & ENABLE_VIRTUAL_TERMINAL_INPUT != 0 && ch == 0 {
        let ctrl = state & (LEFT_CTRL_PRESSED | RIGHT_CTRL_PRESSED) != 0;
        let shift = state & SHIFT_PRESSED != 0;
        // xterm's modifier parameter: 1 + shift + 2*alt + 4*ctrl.
        let modifier = 1 + shift as u32 + 4 * ctrl as u32;
        let csi = |end: &str| -> Vec<u16> {
            let s = if modifier > 1 { alloc::format!("\x1b[1;{}{}", modifier, end) } else { alloc::format!("\x1b[{}", end) };
            s.encode_utf16().collect()
        };
        let tilde = |n: u32| -> Vec<u16> {
            let s = if modifier > 1 { alloc::format!("\x1b[{};{}~", n, modifier) } else { alloc::format!("\x1b[{}~", n) };
            s.encode_utf16().collect()
        };
        return match vk {
            0x26 => csi("A"),
            0x28 => csi("B"),
            0x27 => csi("C"),
            0x25 => csi("D"),
            0x24 => csi("H"),
            0x23 => csi("F"),
            0x2D => tilde(2),
            0x2E => tilde(3),
            0x21 => tilde(5),
            0x22 => tilde(6),
            0x70..=0x73 => alloc::format!("\x1bO{}", ['P', 'Q', 'R', 'S'][(vk - 0x70) as usize]).encode_utf16().collect(),
            0x74 => tilde(15),
            0x75..=0x77 => tilde(17 + (vk - 0x75) as u32),
            0x78..=0x79 => tilde(20 + (vk - 0x78) as u32),
            0x7A..=0x7B => tilde(23 + (vk - 0x7A) as u32),
            _ => Vec::new(),
        };
    }
    if ch == 0 { Vec::new() } else { alloc::vec![ch] }
}
