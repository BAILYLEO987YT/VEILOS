// =============================================================================
// VEILOS: pseudo-consoles (WIN32_DESIGN.md, S5c)
// =============================================================================
//
// CreatePseudoConsole gives a host two pipes' worth of terminal: what programs
// in the console show comes out of one as text and escape sequences, and what
// the host writes into the other (a terminal's keys: text, control characters,
// escape sequences) comes to the programs as key events.
//
// Like Windows' console host, the console keeps its screen buffer as ever
// (conbuf.rs: programs may write escape sequences or call the Win32 console
// API, and read the cells back), and what goes out is the buffer's picture:
// after each change, the rows that differ from what was last sent, with the
// cursor and colours. A scroll goes out as newlines at the bottom, so the
// host's terminal keeps the lines in its own history.

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::fmt::Write;
use spin::Mutex;

use crate::conbuf::{Cell, ScreenBuffer};
use crate::console::{key_record_scan, Console, InputRecord};
use crate::nt_process::PipeEnd;

pub struct Pty {
    input: Arc<PipeEnd>,
    output: Mutex<Option<Arc<PipeEnd>>>,
    sent: Mutex<Sent>,
    /// Input bytes not yet a whole key (an escape sequence cut short).
    pending: Mutex<Vec<u8>>,
}

/// What the host's terminal shows, as far as we've told it.
struct Sent {
    width: usize,
    height: usize,
    cells: Vec<Cell>,
    cursor: (usize, usize),
    cursor_visible: bool,
    /// The SGR in force there: (attribute, bold).
    attr: (u16, bool),
    title: Vec<u16>,
}

impl Sent {
    fn new(width: usize, height: usize) -> Sent {
        Sent {
            width,
            height,
            cells: vec![Cell { ch: b' ' as u16, attr: 0x07, bold: false }; width * height],
            cursor: (0, 0),
            cursor_visible: true,
            attr: (0x07, false),
            title: Vec::new(),
        }
    }
}

impl Pty {
    pub fn new(input: Arc<PipeEnd>, output: Arc<PipeEnd>, width: usize, height: usize) -> Arc<Pty> {
        Arc::new(Pty {
            input,
            output: Mutex::new(Some(output)),
            sent: Mutex::new(Sent::new(width, height)),
            pending: Mutex::new(Vec::new()),
        })
    }

    fn send(&self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        let out = self.output.lock().clone();
        if let Some(out) = out {
            let _ = crate::nt_process::pipe_io(&out, bytes.as_ptr() as u64, bytes.len(), true);
        }
    }

    /// The host's terminal is now `width` by `height` (ResizePseudoConsole):
    /// it keeps what it showed, as far as it fits.
    pub fn resized(&self, width: usize, height: usize) {
        let mut s = self.sent.lock();
        let mut cells = vec![Cell { ch: b' ' as u16, attr: 0x07, bold: false }; width * height];
        for y in 0..height.min(s.height) {
            for x in 0..width.min(s.width) {
                cells[y * width + x] = s.cells[y * s.width + x];
            }
        }
        s.cells = cells;
        s.width = width;
        s.height = height;
        s.cursor = (s.cursor.0.min(width - 1), s.cursor.1.min(height - 1));
    }

    /// Closing: the host hears no more (its reads end once it has read what
    /// was sent).
    pub fn close(&self) {
        self.output.lock().take();
    }

    // -----| output: the buffer's picture |-----

    /// Bring the host's terminal up to date with the active buffer.
    pub fn render(&self, console: &Console) {
        if self.output.lock().is_none() {
            return;
        }
        let buf = console.active();
        let mut out = String::new();
        {
            let mut b = buf.buf.lock();
            let mut s = self.sent.lock();
            let scrolled = core::mem::take(&mut b.scrolled);
            draw(&b, &mut s, scrolled, &mut out);
        }
        let title = console.title.lock().clone();
        {
            let mut s = self.sent.lock();
            if title != s.title {
                let _ = write!(out, "\x1b]0;{}\x07", String::from_utf16_lossy(&title));
                s.title = title;
            }
        }
        self.send(out.as_bytes());
    }

    // -----| input: a terminal's bytes as keys |-----

    /// Take what the host has written: key events for the console.
    pub fn pump(&self, console: &Console) {
        let n = crate::nt_process::pipe_available(&self.input.pipe);
        let mut pending = self.pending.lock();
        if n > 0 {
            let start = pending.len();
            pending.resize(start + n, 0);
            let (_, got) = crate::nt_process::pipe_io(&self.input, pending[start..].as_mut_ptr() as u64, n, false);
            pending.truncate(start + got);
        }
        if pending.is_empty() {
            return;
        }
        let (records, used) = parse_input(&pending);
        pending.drain(..used);
        drop(pending);
        if !records.is_empty() {
            console.typed(&records);
        }
    }
}

/// Console colour (blue 1, green 2, red 4) to ANSI colour (red 1, green 2, blue 4).
const ANSI: [u8; 8] = [0, 4, 2, 6, 1, 5, 3, 7];

fn sgr(attr: u16, bold: bool, out: &mut String) {
    out.push_str("\x1b[0");
    let fg = attr & 0x0F;
    let bg = (attr >> 4) & 0x0F;
    if bold {
        let _ = write!(out, ";1;{}", 30 + ANSI[(fg & 7) as usize] as u32);
    } else if fg != 0x07 {
        let _ = write!(out, ";{}", if fg & 8 != 0 { 90 } else { 30 } + ANSI[(fg & 7) as usize] as u32);
    }
    if bg != 0 {
        let _ = write!(out, ";{}", if bg & 8 != 0 { 100 } else { 40 } + ANSI[(bg & 7) as usize] as u32);
    }
    if attr & 0x4000 != 0 {
        out.push_str(";7");
    }
    if attr & 0x8000 != 0 {
        out.push_str(";4");
    }
    out.push('m');
}

fn draw(b: &ScreenBuffer, s: &mut Sent, scrolled: usize, out: &mut String) {
    let (l, t, w, h) = b.window;
    if (w, h) != (s.width, s.height) {
        // (A resize the host hasn't told us of: start again.)
        *s = Sent::new(w, h);
        out.push_str("\x1b[0m\x1b[H\x1b[2J");
    }
    // The buffer scrolled: newlines at the bottom scroll the host's screen
    // the same way, and its history keeps the lines.
    if scrolled > 0 {
        let n = scrolled.min(h);
        let _ = write!(out, "\x1b[{};1H", h);
        for _ in 0..n {
            out.push('\n');
        }
        s.cells.drain(..n * w);
        s.cells.extend(core::iter::repeat_n(Cell { ch: b' ' as u16, attr: 0x07, bold: false }, n * w));
        s.cursor = (0, h - 1);
    }
    let default = (b.default_attr, false);
    let (cx, cy) = b.cursor;
    let cursor = (cx.saturating_sub(l).min(w - 1), cy.saturating_sub(t).min(h - 1));
    let mut moved = false;
    for row in 0..h {
        let y = t + row;
        let differs = |x: usize| {
            let c = b.cell(l + x, y);
            let o = s.cells[row * w + x];
            c.ch != o.ch || c.attr != o.attr || c.bold != o.bold
        };
        let Some(first) = (0..w).find(|&x| differs(x)) else { continue };
        let mut last = (0..w).rev().find(|&x| differs(x)).unwrap_or(first);
        // On the cursor's row, blanks up to the cursor go out as spaces, as
        // the program wrote them ("name? "), rather than as a cursor move.
        if row == cursor.1 && cursor.0 > last + 1 && cursor.0 - last <= 16 {
            last = cursor.0 - 1;
        }
        // Blank to the end of the row: erased, not written.
        let mut end = w;
        while end > first && {
            let c = b.cell(l + end - 1, y);
            c.ch == b' ' as u16 && (c.attr, c.bold) == default
        } {
            end -= 1;
        }
        if row == cursor.1 {
            end = end.max((last + 1).min(cursor.0));
        }
        let _ = write!(out, "\x1b[{};{}H", row + 1, first + 1);
        moved = true;
        for x in first..end.max(first).min(last + 1) {
            let c = b.cell(l + x, y);
            // A space shows only its background: no colour change for it
            // alone (as Windows' console host does).
            let looks_same = c.ch == b' ' as u16 && c.attr & 0xC0F0 == s.attr.0 & 0xC0F0;
            if (c.attr, c.bold) != s.attr && !looks_same {
                sgr(c.attr, c.bold, out);
                s.attr = (c.attr, c.bold);
            }
            let ch = char::from_u32(c.ch as u32).filter(|&ch| ch >= ' ').unwrap_or(' ');
            out.push(ch);
            s.cells[row * w + x] = c;
        }
        if end <= last {
            if s.attr != default {
                sgr(default.0, false, out);
                s.attr = default;
            }
            let from = end.max(first);
            if from > first {
                let _ = write!(out, "\x1b[{};{}H", row + 1, from + 1);
            }
            out.push_str("\x1b[K");
            for x in from..w {
                s.cells[row * w + x] = b.cell(l + x, y);
            }
        }
    }
    if b.cursor_visible != s.cursor_visible {
        out.push_str(if b.cursor_visible { "\x1b[?25h" } else { "\x1b[?25l" });
        s.cursor_visible = b.cursor_visible;
    }
    if moved || cursor != s.cursor {
        let _ = write!(out, "\x1b[{};{}H", cursor.1 + 1, cursor.0 + 1);
        s.cursor = cursor;
    }
}

// -----| input |-----

const SHIFT: u32 = 0x10;
const LEFT_CTRL: u32 = 0x08;
const LEFT_ALT: u32 = 0x02;
const ENHANCED: u32 = 0x100;

fn key(records: &mut Vec<InputRecord>, vk: u16, scan: u16, ch: u16, state: u32) {
    records.push(key_record_scan(true, vk, scan, ch, state));
    records.push(key_record_scan(false, vk, scan, ch, state));
}

/// A character's key on a US keyboard: (virtual key, scan code, Shift).
fn char_key(c: char) -> (u16, u16, bool) {
    const ROW: &[(char, char, u16, u16)] = &[
        ('1', '!', 0x31, 0x02), ('2', '@', 0x32, 0x03), ('3', '#', 0x33, 0x04), ('4', '$', 0x34, 0x05),
        ('5', '%', 0x35, 0x06), ('6', '^', 0x36, 0x07), ('7', '&', 0x37, 0x08), ('8', '*', 0x38, 0x09),
        ('9', '(', 0x39, 0x0A), ('0', ')', 0x30, 0x0B), ('-', '_', 0xBD, 0x0C), ('=', '+', 0xBB, 0x0D),
        ('[', '{', 0xDB, 0x1A), (']', '}', 0xDD, 0x1B), (';', ':', 0xBA, 0x27), ('\'', '"', 0xDE, 0x28),
        ('`', '~', 0xC0, 0x29), ('\\', '|', 0xDC, 0x2B), (',', '<', 0xBC, 0x33), ('.', '>', 0xBE, 0x34),
        ('/', '?', 0xBF, 0x35), (' ', ' ', 0x20, 0x39),
    ];
    const LETTERS: &[u16; 26] = &[
        0x1E, 0x30, 0x2E, 0x20, 0x12, 0x21, 0x22, 0x23, 0x17, 0x24, 0x25, 0x26, 0x32, 0x31, 0x18, 0x19, 0x10, 0x13,
        0x1F, 0x14, 0x16, 0x2F, 0x11, 0x2D, 0x15, 0x2C,
    ];
    if c.is_ascii_lowercase() {
        return (c.to_ascii_uppercase() as u16, LETTERS[(c as u8 - b'a') as usize], false);
    }
    if c.is_ascii_uppercase() {
        return (c as u16, LETTERS[(c as u8 - b'A') as usize], true);
    }
    for &(plain, shifted, vk, scan) in ROW {
        if c == plain {
            return (vk, scan, false);
        }
        if c == shifted {
            return (vk, scan, true);
        }
    }
    (0, 0, false)
}

/// Key events from a terminal's bytes, and how many bytes they took (an
/// escape sequence cut short waits for the rest).
fn parse_input(b: &[u8]) -> (Vec<InputRecord>, usize) {
    let mut records = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        match c {
            0x1B => {
                match parse_escape(&b[i..], &mut records) {
                    Some(n) => i += n,
                    None => break, // the rest of it hasn't come yet
                }
            }
            b'\r' => {
                key(&mut records, 0x0D, 0x1C, 0x0D, 0);
                i += 1;
            }
            b'\n' => {
                key(&mut records, 0x0D, 0x1C, 0x0A, LEFT_CTRL);
                i += 1;
            }
            b'\t' => {
                key(&mut records, 0x09, 0x0F, 0x09, 0);
                i += 1;
            }
            0x7F | 0x08 => {
                key(&mut records, 0x08, 0x0E, 0x08, 0);
                i += 1;
            }
            0x00 => {
                key(&mut records, 0x20, 0x39, 0, LEFT_CTRL);
                i += 1;
            }
            0x01..=0x1A => {
                let letter = (c - 1 + b'A') as char;
                let (vk, scan, _) = char_key(letter);
                key(&mut records, vk, scan, c as u16, LEFT_CTRL);
                i += 1;
            }
            0x1C..=0x1F => {
                key(&mut records, 0, 0, c as u16, LEFT_CTRL);
                i += 1;
            }
            _ => {
                // A UTF-8 character.
                let len = match c {
                    0xC0..=0xDF => 2,
                    0xE0..=0xEF => 3,
                    0xF0..=0xF7 => 4,
                    _ => 1,
                };
                if i + len > b.len() {
                    break;
                }
                let s = core::str::from_utf8(&b[i..i + len]).unwrap_or("\u{FFFD}");
                for ch in s.chars() {
                    let (vk, scan, shift) = char_key(ch);
                    let mut units = [0u16; 2];
                    for &u in ch.encode_utf16(&mut units).iter() {
                        key(&mut records, vk, scan, u, if shift { SHIFT } else { 0 });
                    }
                }
                i += len;
            }
        }
    }
    (records, i)
}

/// One escape sequence at the start of `b`: its keys, and its length; None
/// if it's cut short. ESC on its own (nothing after it yet) is the Escape key.
fn parse_escape(b: &[u8], records: &mut Vec<InputRecord>) -> Option<usize> {
    if b.len() == 1 {
        key(records, 0x1B, 0x01, 0x1B, 0);
        return Some(1);
    }
    match b[1] {
        b'[' => {
            // CSI: parameters, then the final byte.
            let mut j = 2;
            while j < b.len() && (b[j].is_ascii_digit() || b[j] == b';' || b[j] == b'?' || b[j] == b'<') {
                j += 1;
            }
            if j >= b.len() {
                return None;
            }
            let fin = b[j];
            let params: Vec<u32> = core::str::from_utf8(&b[2..j])
                .unwrap_or("")
                .trim_start_matches(['?', '<'])
                .split(';')
                .map(|p| p.parse().unwrap_or(0))
                .collect();
            let p = |k: usize| params.get(k).copied().unwrap_or(0);
            // Modifiers as xterm sends them: 1 + Shift 1, Alt 2, Ctrl 4.
            let mods = |m: u32| -> u32 {
                let m = m.saturating_sub(1);
                (if m & 1 != 0 { SHIFT } else { 0 }) | (if m & 2 != 0 { LEFT_ALT } else { 0 }) | (if m & 4 != 0 { LEFT_CTRL } else { 0 })
            };
            match fin {
                // win32-input-mode: Vk;Sc;Uc;Kd;Cs;Rc, a whole key record.
                b'_' => {
                    let rec = key_record_scan(p(3) != 0, p(0) as u16, p(1) as u16, p(2) as u16, p(4));
                    for _ in 0..p(5).max(1) {
                        records.push(rec);
                    }
                }
                b'A' | b'B' | b'C' | b'D' | b'H' | b'F' => {
                    let (vk, scan) = match fin {
                        b'A' => (0x26, 0x48),
                        b'B' => (0x28, 0x50),
                        b'C' => (0x27, 0x4D),
                        b'D' => (0x25, 0x4B),
                        b'H' => (0x24, 0x47),
                        _ => (0x23, 0x4F),
                    };
                    key(records, vk, scan, 0, ENHANCED | mods(p(1)));
                }
                b'Z' => key(records, 0x09, 0x0F, 0x09, SHIFT),
                b'~' => {
                    let (vk, scan, ext) = match p(0) {
                        1 => (0x24, 0x47, true),
                        2 => (0x2D, 0x52, true),
                        3 => (0x2E, 0x53, true),
                        4 => (0x23, 0x4F, true),
                        5 => (0x21, 0x49, true),
                        6 => (0x22, 0x51, true),
                        15 => (0x74, 0x3F, false),
                        17 => (0x75, 0x40, false),
                        18 => (0x76, 0x41, false),
                        19 => (0x77, 0x42, false),
                        20 => (0x78, 0x43, false),
                        21 => (0x79, 0x44, false),
                        23 => (0x7A, 0x57, false),
                        24 => (0x7B, 0x58, false),
                        _ => (0, 0, false),
                    };
                    if vk != 0 {
                        key(records, vk, scan, 0, if ext { ENHANCED } else { 0 } | mods(p(1)));
                    }
                }
                // Focus in and out, cursor position reports: not keys.
                _ => {}
            }
            Some(j + 1)
        }
        b'O' => {
            if b.len() < 3 {
                return None;
            }
            let (vk, scan) = match b[2] {
                b'P' => (0x70, 0x3B),
                b'Q' => (0x71, 0x3C),
                b'R' => (0x72, 0x3D),
                b'S' => (0x73, 0x3E),
                b'A' => (0x26, 0x48),
                b'B' => (0x28, 0x50),
                b'C' => (0x27, 0x4D),
                b'D' => (0x25, 0x4B),
                b'H' => (0x24, 0x47),
                b'F' => (0x23, 0x4F),
                _ => (0, 0),
            };
            if vk != 0 {
                key(records, vk, scan, 0, if vk < 0x70 { ENHANCED } else { 0 });
            }
            Some(3)
        }
        // Alt with a character.
        c if c >= 0x20 && c < 0x7F => {
            let (vk, scan, shift) = char_key(c as char);
            key(records, vk, scan, c as u16, LEFT_ALT | if shift { SHIFT } else { 0 });
            Some(2)
        }
        _ => {
            key(records, 0x1B, 0x01, 0x1B, 0);
            Some(1)
        }
    }
}
