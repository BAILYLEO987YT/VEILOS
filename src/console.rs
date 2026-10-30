// =============================================================================
// VEILOS: consoles for Windows console programs (WIN32_DESIGN.md, step W2)
// =============================================================================
//
// A console-subsystem program gets one `Console`. Its standard input, output
// and error handles all refer to it (handle-table entries, so they can be
// closed and passed around like any handle), and the terminal window opened
// for the program shows what it writes. Output is also copied to the serial
// log a line at a time, prefixed with the program's pid.
//
// Keyboard input to the window (stdin) is W3.

use alloc::string::String;
use core::sync::atomic::{AtomicUsize, Ordering};
use spin::Mutex;

/// No terminal attached (yet).
const NO_TERMINAL: usize = usize::MAX;

pub struct Console {
    pid: u32,
    /// TERMINAL_POOL slot showing this console.
    terminal: AtomicUsize,
    /// True while the terminal's last line is unfinished (no newline yet),
    /// so the next write continues it.
    line_open: Mutex<bool>,
    /// The serial log's unfinished line.
    serial_line: Mutex<String>,
}

impl Console {
    pub fn new(pid: u32) -> Self {
        Console { pid, terminal: AtomicUsize::new(NO_TERMINAL), line_open: Mutex::new(false), serial_line: Mutex::new(String::new()) }
    }

    /// Show this console in terminal pool slot `slot`.
    pub fn attach(&self, slot: usize) {
        self.terminal.store(slot, Ordering::Release);
    }

    /// Program output: bytes as written (UTF-8; anything else shows as U+FFFD).
    pub fn write(&self, bytes: &[u8]) {
        let text = String::from_utf8_lossy(bytes);
        self.to_serial(&text);
        let slot = self.terminal.load(Ordering::Acquire);
        if slot == NO_TERMINAL {
            return;
        }
        let Some(term) = crate::apps::TERMINAL_POOL.get(slot) else { return };
        let mut term = term.lock();
        let mut open = self.line_open.lock();
        for (i, part) in text.split('\n').enumerate() {
            if i > 0 {
                // A newline ends the current line; at the start of a line it
                // is an empty line of its own.
                if !*open {
                    term.history.push(String::new());
                }
                *open = false;
            }
            let part: String = part.chars().filter(|&c| c != '\r').map(|c| if c == '\t' { ' ' } else { c }).collect();
            if part.is_empty() {
                continue;
            }
            match term.history.last_mut() {
                Some(last) if *open => last.push_str(&part),
                _ => term.history.push(part),
            }
            *open = true;
        }
        crate::apps::TERMINAL_OUTPUT.store(true, Ordering::Release);
    }

    /// The program has exited: say so in its window and flush the log line.
    pub fn finish(&self, code: Option<i32>) {
        if !self.serial_line.lock().is_empty() {
            self.to_serial("\n");
        }
        let line = match code {
            Some(c) => alloc::format!("[exited with code {}]", c),
            None => String::from("[exited]"),
        };
        let slot = self.terminal.load(Ordering::Acquire);
        if let Some(term) = crate::apps::TERMINAL_POOL.get(slot) {
            let mut term = term.lock();
            term.history.push(String::new());
            term.history.push(line);
            crate::apps::TERMINAL_OUTPUT.store(true, Ordering::Release);
        }
    }

    fn to_serial(&self, text: &str) {
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
