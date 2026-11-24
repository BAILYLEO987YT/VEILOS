// =============================================================================
// VEILOS: the kernel log, kept in memory
// =============================================================================
//
// Everything `serial_println!` writes is also kept here, the last 128 KiB of
// it, so it can be read on a machine with no serial port: the terminal's
// `DMESG [TEXT]` shows the last lines (those containing TEXT). On a real PC
// that is how a driver's start-up gets reported back.
//
// Written with try_lock from wherever serial output comes from (interrupt
// handlers too): a line that finds the log busy is only missed here, never
// on the serial port.

use alloc::{string::String, vec::Vec};
use spin::Mutex;

const SIZE: usize = 128 * 1024;

pub struct Ring {
    buf: [u8; SIZE],
    /// Next byte written.
    head: usize,
    /// The buffer has wrapped at least once.
    full: bool,
}

pub static KLOG: Mutex<Ring> = Mutex::new(Ring { buf: [0; SIZE], head: 0, full: false });

impl core::fmt::Write for Ring {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        for &b in s.as_bytes() {
            self.buf[self.head] = b;
            self.head += 1;
            if self.head == SIZE {
                self.head = 0;
                self.full = true;
            }
        }
        Ok(())
    }
}

/// From `_serial_print`.
pub fn record(args: core::fmt::Arguments) {
    use core::fmt::Write;
    if let Some(mut ring) = KLOG.try_lock() {
        let _ = ring.write_fmt(args);
    }
}

/// The last `max` lines containing `filter` (any case; empty: every line).
pub fn lines(filter: &str, max: usize) -> Vec<String> {
    let text = {
        let ring = KLOG.lock();
        let mut bytes = Vec::with_capacity(SIZE);
        if ring.full {
            bytes.extend_from_slice(&ring.buf[ring.head..]);
        }
        bytes.extend_from_slice(&ring.buf[..ring.head]);
        String::from_utf8_lossy(&bytes).into_owned()
    };
    let filter = filter.to_ascii_lowercase();
    let mut out: Vec<String> = text
        .lines()
        .map(|l| l.trim_end_matches('\r'))
        .filter(|l| !l.trim().is_empty() && (filter.is_empty() || l.to_ascii_lowercase().contains(&filter)))
        .map(String::from)
        .collect();
    if out.len() > max {
        out.drain(..out.len() - max);
    }
    out
}
