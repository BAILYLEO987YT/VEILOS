// -----| The crash screen and the saved crash log |-----
//
// When the kernel panics (main.rs, the panic handler calls `on_panic`):
//
//   1. Every other CPU is stopped with an NMI (smp::stop_others; they halt
//      in interrupts.rs's NMI handler), so nothing changes under us.
//   2. A report is put together in a fixed buffer: no allocation, as the
//      heap may be what broke.
//   3. The screen is painted straight onto the framebuffer: what happened,
//      where, and that the details were saved.
//   4. The report is written to /var/log/crash.log, through sectors found
//      when the system started (`prepare`): the file is made then, 64 KiB
//      of zeros, and its sectors kept. At the panic they're written
//      directly (block::Disk::panic_write), past the filesystem and the
//      cache, as Windows writes its dump to space it set aside.
//
// At the next start `prepare` finds a report in crash.log, keeps it as
// /var/log/crash-N.log, says so in a notification, and clears crash.log
// for next time. CRASH TEST in the terminal (administrators) panics on
// purpose. Any key on the crash screen restarts (power.rs; main.rs's panic
// handler waits for it).

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::fmt::Write;
use core::sync::atomic::{AtomicBool, Ordering};
use spin::Mutex;

const LOG: &str = "/var/log/crash.log";
const LOG_BYTES: usize = 64 * 1024;
const MAGIC: &[u8] = b"VEILOS CRASH REPORT\n";

static STOPPING: AtomicBool = AtomicBool::new(false);
static TARGET: Mutex<Option<(Arc<crate::block::Disk>, Vec<u64>)>> = Mutex::new(None);
/// When this boot started, as text ("2026-11-17 18:50:02"), for the report.
static BOOTED: Mutex<[u8; 20]> = Mutex::new([0; 20]);

/// True once a panic has begun: other CPUs halt on their NMI.
pub fn stopping() -> bool {
    STOPPING.load(Ordering::Acquire)
}

/// After the disks mount: keep last time's report, if any, and set up
/// crash.log for this boot.
pub fn prepare() {
    let vfs = &crate::vfs::VFS;
    let _ = vfs.create_dir("/var");
    let _ = vfs.create_dir("/var/log");
    {
        let t = crate::shell::wall_clock();
        let mut s = Buf::<20>::new();
        let _ = write!(s, "{:04}-{:02}-{:02} {:02}:{:02}:{:02}", t.year, t.month, t.day, t.hour, t.minute, t.second);
        BOOTED.lock()[..s.len].copy_from_slice(s.bytes());
    }
    if let Ok(old) = vfs.read_file(LOG) {
        if old.starts_with(MAGIC) {
            let end = old.iter().position(|&b| b == 0).unwrap_or(old.len());
            let names: Vec<alloc::string::String> = vfs.list_dir("/var/log").map(|v| v.into_iter().map(|e| e.name.to_ascii_lowercase()).collect()).unwrap_or_default();
            let n = (1..).find(|n| !names.contains(&alloc::format!("crash-{}.log", n))).unwrap_or(1);
            let kept = alloc::format!("/var/log/crash-{}.log", n);
            if vfs.write_file(&kept, &old[..end]).is_ok() {
                crate::serial_println!("[CRASH] the last start ended in a crash: report kept as {}", kept);
                crate::desktop::notify(
                    crate::desktop::NotifCategory::Warning,
                    "VEILOS",
                    &alloc::format!("VEILOS stopped unexpectedly last time. The report is in {}", kept),
                );
            }
        }
    }
    let zeros = alloc::vec![0u8; LOG_BYTES];
    if vfs.write_file(LOG, &zeros).is_err() {
        crate::serial_println!("[CRASH] {} couldn't be made: a crash won't be saved", LOG);
        return;
    }
    // The file's place must be on the disk before it can be written behind
    // the filesystem's back (Performance Mode holds writes back).
    crate::block::flush_all();
    match vfs.file_sectors(LOG) {
        Some((disk, lbas)) if lbas.len() == LOG_BYTES / 512 => {
            crate::serial_println!("[CRASH] a crash will be saved to {} ({} sectors from {})", LOG, lbas.len(), lbas[0]);
            *TARGET.lock() = Some((disk, lbas));
        }
        _ => crate::serial_println!("[CRASH] {}'s sectors aren't known: a crash won't be saved", LOG),
    }
}

/// A fixed-size text buffer (no allocation).
struct Buf<const N: usize> {
    b: [u8; N],
    len: usize,
}

impl<const N: usize> Buf<N> {
    const fn new() -> Self {
        Buf { b: [0; N], len: 0 }
    }
    fn bytes(&self) -> &[u8] {
        &self.b[..self.len]
    }
}

impl<const N: usize> Write for Buf<N> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let n = s.len().min(N - self.len);
        self.b[self.len..self.len + n].copy_from_slice(&s.as_bytes()[..n]);
        self.len += n;
        Ok(())
    }
}

/// The report. Static: a panic may have little stack to spare.
static mut REPORT: Buf<8192> = Buf::new();

/// From the panic handler. Returns when the screen and the log are done
/// (the handler then halts this CPU).
pub fn on_panic(info: &core::panic::PanicInfo) {
    // A panic while reporting one: leave what's done.
    if STOPPING.swap(true, Ordering::AcqRel) {
        return;
    }
    crate::smp::stop_others();

    #[allow(static_mut_refs)]
    let r = unsafe { &mut REPORT };
    r.len = 0;
    let booted = match BOOTED.try_lock() {
        Some(b) => *b,
        None => [0; 20],
    };
    let booted = core::str::from_utf8(&booted).unwrap_or("").trim_end_matches('\0');
    let _ = r.write_str(core::str::from_utf8(MAGIC).unwrap_or(""));
    let _ = write!(r, "Started {} (local time); stopped {} s later.\n", booted, crate::time::now_ms() / 1000);
    let _ = write!(r, "CPU {}, mode {}.\n", crate::smp::cpu_index(), if crate::policy::performance() { "Performance" } else { "Security" });
    if let Some(p) = crate::process::PROCESSOR.try_lock().and_then(|p| p.get_active()) {
        let _ = write!(r, "Last process: pid {}.\n", p.pid);
    }
    // "panicked at FILE:LINE:COL:" and the message.
    let _ = write!(r, "\n{}\n", info);

    paint(
        b"VEILOS RAN INTO A PROBLEM AND STOPPED.",
        &[b"THE REPORT BELOW WAS SAVED TO /VAR/LOG/CRASH.LOG AND IS KEPT AT THE NEXT START.", b"PRESS ANY KEY TO RESTART."],
        r.bytes(),
    );

    // Pad to whole sectors; write the runs of consecutive sectors.
    if let Some(guard) = TARGET.try_lock() {
        if let Some((disk, lbas)) = guard.as_ref() {
            let used = r.len.div_ceil(512).min(lbas.len());
            static mut SECTORS: [u8; 8192] = [0; 8192];
            #[allow(static_mut_refs)]
            let s = unsafe { &mut SECTORS };
            s[..r.len].copy_from_slice(r.bytes());
            s[r.len..].fill(0);
            let mut i = 0;
            let mut ok = true;
            while i < used {
                let mut n = 1;
                while i + n < used && lbas[i + n] == lbas[i] + n as u64 {
                    n += 1;
                }
                ok &= disk.panic_write(lbas[i], &s[i * 512..(i + n) * 512]);
                i += n;
            }
            crate::serial_println!("[CRASH] report {} to {}", if ok { "saved" } else { "NOT saved" }, LOG);
        }
    }
}

/// A screen VEILOS stops at (main.rs: no system volume), on every display.
/// `body`'s lines are shown below `notes`.
pub fn paint_stop(headline: &[u8], notes: &[&[u8]], body: &[u8]) {
    let mut report: Vec<u8> = Vec::from(&b"\n"[..]);
    report.extend_from_slice(body);
    paint(headline, notes, &report);
}

/// The crash screen, straight onto every display. The report's first line
/// (its magic) is not shown.
fn paint(headline: &[u8], notes: &[&[u8]], report: &[u8]) {
    const BG: u32 = 0x0010_2A43;
    const FG: u32 = 0x00FF_FFFF;
    const DIM: u32 = 0x00A8_C0D8;
    let heads = match crate::graphics::HEADS.try_lock() {
        Some(h) => h,
        None => {
            unsafe { crate::graphics::HEADS.force_unlock() };
            match crate::graphics::HEADS.try_lock() {
                Some(h) => h,
                None => return,
            }
        }
    };
    for head in heads.iter() {
        let fb = &head.fb;
        let px = |x: usize, y: usize, c: u32| {
            if x < fb.width && y < fb.height {
                unsafe { *((fb.base_addr + (y * fb.stride + x) as u64 * 4) as *mut u32) = c };
            }
        };
        for y in 0..fb.height {
            for x in 0..fb.width {
                px(x, y, BG);
            }
        }
        // Text at twice the font's size for the headline, 1x for the rest.
        let text = |s: &[u8], x0: usize, y0: usize, scale: usize, c: u32| {
            let mut x = x0;
            for &ch in s {
                let g = crate::ui::glyph(ch as char);
                for (dy, row) in g.iter().enumerate() {
                    for dx in 0..8 {
                        if row & (0x80 >> dx) != 0 {
                            for sy in 0..scale {
                                for sx in 0..scale {
                                    px(x + dx * scale + sx, y0 + dy * scale + sy, c);
                                }
                            }
                        }
                    }
                }
                x += 9 * scale;
            }
        };
        let left = 48;
        text(b":(", left, 48, 6, FG);
        text(headline, left, 120, 2, FG);
        let mut y = 150;
        for note in notes {
            text(note, left, y, 1, DIM);
            y += 14;
        }
        // The report, wrapped to the screen.
        let cols = ((fb.width.saturating_sub(2 * left)) / 9).max(20);
        y += 18;
        for line in report.split(|&b| b == b'\n').skip(1) {
            let mut rest = line;
            loop {
                let n = rest.len().min(cols);
                text(&rest[..n], left, y, 1, FG);
                y += 12;
                rest = &rest[n..];
                if rest.is_empty() || y + 12 > fb.height {
                    break;
                }
            }
            if y + 12 > fb.height {
                break;
            }
        }
    }
}
