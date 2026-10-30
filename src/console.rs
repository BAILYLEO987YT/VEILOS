// =============================================================================
// VEILOS: consoles for Windows console programs (WIN32_DESIGN.md, W2-W3)
// =============================================================================
//
// A console-subsystem program gets one `Console`. Its standard input, output
// and error handles all refer to it (handle-table entries, so they can be
// closed and passed around like any handle), and a terminal window shows it:
// a window of its own, or the terminal it was started from with RUN.
//
//   Output: what the program writes appears in the terminal (lines joined
//   across writes) and in the serial log, a line at a time, with its pid.
//   Input: while the program runs, the terminal's input line is the
//   console's: Enter echoes the line into the output (as a Windows console
//   does) and queues it, with "\r\n", for the program's reads.
//   Closing the window ends the program (`TerminalEmulator::console`).

use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use spin::Mutex;

use crate::apps::TerminalEmulator;

/// No terminal attached (yet).
const NO_TERMINAL: usize = usize::MAX;

pub struct Console {
    pid: u32,
    /// TERMINAL_POOL slot showing this console.
    terminal: AtomicUsize,
    /// True while the terminal's last line is unfinished (no newline yet),
    /// so the next output continues it.
    line_open: Mutex<bool>,
    /// The serial log's unfinished line.
    serial_line: Mutex<String>,
    /// Typed lines not yet read, as bytes ("...\r\n" each).
    input: Mutex<VecDeque<u8>>,
    /// The program has exited: no more input or output.
    finished: AtomicBool,
}

impl Console {
    pub fn new(pid: u32) -> Self {
        Console {
            pid,
            terminal: AtomicUsize::new(NO_TERMINAL),
            line_open: Mutex::new(false),
            serial_line: Mutex::new(String::new()),
            input: Mutex::new(VecDeque::new()),
            finished: AtomicBool::new(false),
        }
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// Show this console in terminal pool slot `slot`. The caller also sets
    /// that terminal's `console` (it may already hold the terminal's lock).
    pub fn attach(&self, slot: usize) {
        self.terminal.store(slot, Ordering::Release);
    }

    pub fn is_live(&self) -> bool {
        !self.finished.load(Ordering::Acquire)
    }

    /// Program output: bytes as written (UTF-8; anything else shows as U+FFFD).
    pub fn write(&self, bytes: &[u8]) {
        let text = String::from_utf8_lossy(bytes);
        self.to_serial(&text);
        if let Some(term) = crate::apps::TERMINAL_POOL.get(self.terminal.load(Ordering::Acquire)) {
            self.append(&mut term.lock(), &text);
        }
    }

    /// Add `text` to `term`'s lines: a newline ends the current line, and at
    /// the start of a line it is an empty line of its own.
    fn append(&self, term: &mut TerminalEmulator, text: &str) {
        // A form feed clears the terminal (kernel32's FillConsoleOutputCharacter
        // sends one for `cls`).
        let text = match text.rfind('\x0c') {
            Some(i) => {
                term.history.clear();
                *self.line_open.lock() = false;
                &text[i + 1..]
            }
            None => text,
        };
        let mut open = self.line_open.lock();
        for (i, part) in text.split('\n').enumerate() {
            if i > 0 {
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

    /// A line typed in the console's terminal (`term`, already locked by the
    /// caller): echo it and queue it for the program.
    pub fn typed_line(&self, term: &mut TerminalEmulator, line: &str) {
        let mut echoed = String::from(line);
        echoed.push('\n');
        self.append(term, &echoed);
        self.to_serial(&echoed);
        let mut input = self.input.lock();
        input.extend(line.bytes());
        input.extend(b"\r\n");
    }

    /// Up to `max` bytes of the oldest typed line (all of it, through its
    /// "\r\n", if it fits). None until a whole line has been typed.
    pub fn read_line(&self, max: usize) -> Option<Vec<u8>> {
        let mut input = self.input.lock();
        let end = input.iter().position(|&b| b == b'\n')? + 1;
        Some(input.drain(..end.min(max)).collect())
    }

    /// The program has exited: say so in its window, detach from the
    /// terminal, and flush the log line.
    pub fn finish(&self, code: Option<i32>) {
        self.finished.store(true, Ordering::Release);
        if !self.serial_line.lock().is_empty() {
            self.to_serial("\n");
        }
        let line = match code {
            Some(c) => alloc::format!("[exited with code {}]", c),
            None => String::from("[exited]"),
        };
        if let Some(term) = crate::apps::TERMINAL_POOL.get(self.terminal.load(Ordering::Acquire)) {
            let mut term = term.lock();
            if *self.line_open.lock() {
                term.history.push(String::new());
            }
            term.history.push(line);
            term.history.push(String::new());
            if term.console.as_ref().is_some_and(|c| c.pid == self.pid) {
                term.console = None;
            }
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
