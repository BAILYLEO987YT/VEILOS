//! Smaller common controls: the progress bar ("msctls_progress32"), the
//! trackbar ("msctls_trackbar32"), the up-down control
//! ("msctls_updown32") and tooltips ("tooltips_class32", which keep their
//! tools but don't pop up yet: the desktop reports no mouse movement).

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use crate::dllrt::Lock;
use crate::Hwnd;

#[derive(Clone, Copy, Default)]
struct Range {
    min: i32,
    max: i32,
    pos: i32,
    step: i32,
    page: i32,
    line: i32,
    tick_freq: i32,
    buddy: Hwnd,
    base: u32,
}

static STATE: Lock<BTreeMap<Hwnd, Range>> = Lock::new(BTreeMap::new());

fn get(hwnd: Hwnd) -> Range {
    STATE.lock().get(&hwnd).copied().unwrap_or_default()
}

fn set(hwnd: Hwnd, f: impl FnOnce(&mut Range)) {
    let mut s = STATE.lock();
    let r = s.entry(hwnd).or_default();
    f(r);
}

fn style(hwnd: Hwnd) -> u32 {
    unsafe { crate::GetWindowLongPtrW(hwnd, -16) as u32 }
}

fn redraw(hwnd: Hwnd) {
    unsafe { crate::InvalidateRect(hwnd, core::ptr::null(), 1) };
}

fn client(hwnd: Hwnd) -> [i32; 4] {
    let mut r = [0i32; 4];
    unsafe { crate::GetClientRect(hwnd, &mut r) };
    r
}

fn fill(hdc: u64, r: [i32; 4], color: i32) {
    unsafe { crate::FillRect(hdc, &r, crate::GetSysColorBrush(color)) };
}

fn sunken(hdc: u64, r: [i32; 4]) {
    fill(hdc, [r[0], r[1], r[2], r[1] + 1], 16);
    fill(hdc, [r[0], r[1], r[0] + 1, r[3]], 16);
    fill(hdc, [r[0], r[3] - 1, r[2], r[3]], 20);
    fill(hdc, [r[2] - 1, r[1], r[2], r[3]], 20);
}

fn raised(hdc: u64, r: [i32; 4]) {
    fill(hdc, r, 15);
    fill(hdc, [r[0], r[1], r[2], r[1] + 1], 20);
    fill(hdc, [r[0], r[1], r[0] + 1, r[3]], 20);
    fill(hdc, [r[0], r[3] - 1, r[2], r[3]], 21);
    fill(hdc, [r[2] - 1, r[1], r[2], r[3]], 21);
}

fn paint_with(hwnd: Hwnd, f: impl FnOnce(u64)) {
    let mut ps = [0u8; 72];
    let hdc = unsafe { crate::BeginPaint(hwnd, ps.as_mut_ptr()) };
    f(hdc);
    unsafe { crate::EndPaint(hwnd, ps.as_ptr()) };
}

pub fn register_classes() {
    crate::register("msctls_progress32", progress_proc as *const () as usize, true);
    crate::register("msctls_trackbar32", trackbar_proc as *const () as usize, true);
    crate::register("msctls_updown32", updown_proc as *const () as usize, true);
    crate::register("tooltips_class32", tooltip_proc as *const () as usize, true);
}

fn lohi(v: i64) -> (i32, i32) {
    ((v & 0xFFFF) as i16 as i32, ((v >> 16) & 0xFFFF) as i16 as i32)
}

// -----| progress bar |-----

const PBS_VERTICAL: u32 = 0x04;

unsafe extern "system" fn progress_proc(hwnd: Hwnd, msg: u32, wp: u64, lp: i64) -> i64 {
    match msg {
        0x0001 => {
            set(hwnd, |r| {
                r.max = 100;
                r.step = 10;
            });
            0
        }
        0x0082 => {
            STATE.lock().remove(&hwnd);
            unsafe { crate::DefWindowProcW(hwnd, msg, wp, lp) }
        }
        0x000F => {
            paint_with(hwnd, |hdc| {
                let c = client(hwnd);
                fill(hdc, c, 15);
                sunken(hdc, c);
                let r = get(hwnd);
                let span = (r.max - r.min).max(1) as i64;
                let done = (r.pos - r.min).clamp(0, r.max - r.min) as i64;
                // Chunks of the highlight color.
                if style(hwnd) & PBS_VERTICAL != 0 {
                    let h = ((c[3] - 4) as i64 * done / span) as i32;
                    let mut y = c[3] - 2;
                    while y > c[3] - 2 - h {
                        fill(hdc, [2, (y - 6).max(c[3] - 2 - h), c[2] - 2, y], 13);
                        y -= 8;
                    }
                } else {
                    let w = ((c[2] - 4) as i64 * done / span) as i32;
                    let mut x = 2;
                    while x < 2 + w {
                        fill(hdc, [x, 2, (x + 6).min(2 + w), c[3] - 2], 13);
                        x += 8;
                    }
                }
            });
            0
        }
        0x0014 => 1,
        // PBM_SETRANGE: the old range.
        0x0401 => {
            let old = get(hwnd);
            let (lo, hi) = lohi(lp);
            set(hwnd, |r| {
                r.min = lo;
                r.max = hi;
                r.pos = r.pos.clamp(lo, hi.max(lo));
            });
            redraw(hwnd);
            ((old.max as u16 as i64) << 16) | old.min as u16 as i64
        }
        // PBM_SETPOS, PBM_DELTAPOS, PBM_STEPIT: the old position.
        0x0402 | 0x0403 | 0x0405 => {
            let old = get(hwnd);
            set(hwnd, |r| {
                let p = match msg {
                    0x0402 => wp as i32,
                    0x0403 => r.pos + wp as i32,
                    _ => {
                        let p = r.pos + r.step;
                        // Stepping past the end starts over.
                        if p > r.max { r.min + (p - r.max) } else { p }
                    }
                };
                r.pos = p.clamp(r.min, r.max.max(r.min));
            });
            redraw(hwnd);
            old.pos as i64
        }
        // PBM_SETSTEP: the old step.
        0x0404 => {
            let old = get(hwnd).step;
            set(hwnd, |r| r.step = wp as i32);
            old as i64
        }
        // PBM_SETRANGE32
        0x0406 => {
            let old = get(hwnd);
            set(hwnd, |r| {
                r.min = wp as i32;
                r.max = lp as i32;
                r.pos = r.pos.clamp(r.min, r.max.max(r.min));
            });
            redraw(hwnd);
            ((old.max as u16 as i64) << 16) | old.min as u16 as i64
        }
        // PBM_GETRANGE: (low ? min : max), and PBRANGE.
        0x0407 => {
            let r = get(hwnd);
            if lp != 0 {
                unsafe { *(lp as *mut [i32; 2]) = [r.min, r.max] };
            }
            (if wp != 0 { r.min } else { r.max }) as i64
        }
        0x0408 => get(hwnd).pos as i64,
        // PBM_SETBARCOLOR, PBM_SETBKCOLOR, PBM_SETMARQUEE, PBM_SETSTATE
        0x0409 | 0x2001 | 0x040A | 0x0410 => 0,
        _ => unsafe { crate::DefWindowProcW(hwnd, msg, wp, lp) },
    }
}

// -----| trackbar |-----

const TBS_VERT: u32 = 0x02;
const TBS_NOTICKS: u32 = 0x10;

/// The thumb's rectangle and the channel's ends (along the bar).
fn track_geometry(hwnd: Hwnd) -> ([i32; 4], i32, i32) {
    let c = client(hwnd);
    let r = get(hwnd);
    let vert = style(hwnd) & TBS_VERT != 0;
    let (len, thick) = if vert { (c[3], c[2]) } else { (c[2], c[3]) };
    let (a, b) = (8, len - 8);
    let span = (r.max - r.min).max(1) as i64;
    let at = a + ((b - a) as i64 * (r.pos - r.min) as i64 / span) as i32;
    let thumb = if vert { [thick / 2 - 10, at - 5, thick / 2 + 10, at + 5] } else { [at - 5, thick / 2 - 10, at + 5, thick / 2 + 10] };
    (thumb, a, b)
}

fn track_notify(hwnd: Hwnd, code: u32) {
    let vert = style(hwnd) & TBS_VERT != 0;
    let parent = unsafe { crate::GetParent(hwnd) };
    let pos = get(hwnd).pos;
    let msg = if vert { 0x0115 } else { 0x0114 };
    let wp = if code == 4 || code == 5 { (pos as u16 as u64) << 16 | code as u64 } else { code as u64 };
    unsafe { crate::SendMessageW(parent, msg, wp, hwnd as i64) };
}

fn track_move(hwnd: Hwnd, to: i32, code: u32) {
    set(hwnd, |r| r.pos = to.clamp(r.min, r.max.max(r.min)));
    redraw(hwnd);
    track_notify(hwnd, code);
    track_notify(hwnd, 8); // TB_ENDTRACK
}

unsafe extern "system" fn trackbar_proc(hwnd: Hwnd, msg: u32, wp: u64, lp: i64) -> i64 {
    match msg {
        0x0001 => {
            set(hwnd, |r| {
                r.max = 100;
                r.page = 20;
                r.line = 1;
                r.tick_freq = 1;
            });
            0
        }
        0x0082 => {
            STATE.lock().remove(&hwnd);
            unsafe { crate::DefWindowProcW(hwnd, msg, wp, lp) }
        }
        0x000F => {
            paint_with(hwnd, |hdc| {
                let c = client(hwnd);
                fill(hdc, c, 15);
                let vert = style(hwnd) & TBS_VERT != 0;
                let (thumb, a, b) = track_geometry(hwnd);
                let channel = if vert { [c[2] / 2 - 2, a, c[2] / 2 + 2, b] } else { [a, c[3] / 2 - 2, b, c[3] / 2 + 2] };
                fill(hdc, channel, 20);
                sunken(hdc, channel);
                let r = get(hwnd);
                if style(hwnd) & TBS_NOTICKS == 0 && r.tick_freq > 0 && r.max > r.min {
                    let span = (r.max - r.min) as i64;
                    let mut v = r.min;
                    while v <= r.max {
                        let at = a + ((b - a) as i64 * (v - r.min) as i64 / span) as i32;
                        let t = if vert { [c[2] / 2 + 12, at, c[2] / 2 + 15, at + 1] } else { [at, c[3] / 2 + 12, at + 1, c[3] / 2 + 15] };
                        fill(hdc, t, 21);
                        v += r.tick_freq.max((r.max - r.min) / 100).max(1);
                    }
                }
                raised(hdc, thumb);
            });
            0
        }
        0x0014 => 1,
        0x0087 => 0x0001, // WM_GETDLGCODE: arrows
        0x0201 => {
            unsafe { crate::SetFocus(hwnd) };
            let (x, y) = lohi(lp);
            let (thumb, _, _) = track_geometry(hwnd);
            let vert = style(hwnd) & TBS_VERT != 0;
            let (p, lo, hi) = if vert { (y, thumb[1], thumb[3]) } else { (x, thumb[0], thumb[2]) };
            let r = get(hwnd);
            if p < lo {
                track_move(hwnd, r.pos - r.page, 2);
            } else if p >= hi {
                track_move(hwnd, r.pos + r.page, 3);
            } else {
                // The thumb: it follows the mouse until released.
                unsafe { crate::SetCapture(hwnd) };
            }
            0
        }
        0x0200 => {
            if unsafe { crate::GetCapture() } == hwnd {
                let (x, y) = lohi(lp);
                let vert = style(hwnd) & TBS_VERT != 0;
                let (_, a, b) = track_geometry(hwnd);
                let r = get(hwnd);
                let at = (if vert { y } else { x }).clamp(a, b);
                let span = (b - a).max(1) as i64;
                let to = r.min + (((at - a) as i64 * (r.max - r.min) as i64 + span / 2) / span) as i32;
                if to != r.pos {
                    set(hwnd, |t| t.pos = to);
                    redraw(hwnd);
                    track_notify(hwnd, 5); // TB_THUMBTRACK
                }
            }
            0
        }
        0x0202 => {
            if unsafe { crate::GetCapture() } == hwnd {
                unsafe { crate::ReleaseCapture() };
                track_notify(hwnd, 4); // TB_THUMBPOSITION
                track_notify(hwnd, 8); // TB_ENDTRACK
            }
            0
        }
        0x0100 => {
            let r = get(hwnd);
            match wp {
                0x25 | 0x26 => track_move(hwnd, r.pos - r.line, 0),
                0x27 | 0x28 => track_move(hwnd, r.pos + r.line, 1),
                0x21 => track_move(hwnd, r.pos - r.page, 2),
                0x22 => track_move(hwnd, r.pos + r.page, 3),
                0x24 => track_move(hwnd, r.min, 6),
                0x23 => track_move(hwnd, r.max, 7),
                _ => {}
            }
            0
        }
        0x0400 => get(hwnd).pos as i64,
        0x0401 => get(hwnd).min as i64,
        0x0402 => get(hwnd).max as i64,
        // TBM_SETPOS (redraw, position)
        0x0405 => {
            set(hwnd, |r| r.pos = (lp as i32).clamp(r.min, r.max.max(r.min)));
            if wp != 0 {
                redraw(hwnd);
            }
            0
        }
        // TBM_SETRANGE, TBM_SETRANGEMIN, TBM_SETRANGEMAX
        0x0406 | 0x0407 | 0x0408 => {
            set(hwnd, |r| {
                match msg {
                    0x0406 => {
                        let (lo, hi) = lohi(lp);
                        r.min = lo;
                        r.max = hi;
                    }
                    0x0407 => r.min = lp as i32,
                    _ => r.max = lp as i32,
                }
                r.pos = r.pos.clamp(r.min, r.max.max(r.min));
            });
            if wp != 0 {
                redraw(hwnd);
            }
            0
        }
        // TBM_SETTICFREQ
        0x0414 => {
            set(hwnd, |r| r.tick_freq = wp as i32);
            redraw(hwnd);
            0
        }
        // TBM_SETPAGESIZE, TBM_GETPAGESIZE, TBM_SETLINESIZE, TBM_GETLINESIZE
        0x0415 => {
            let old = get(hwnd).page;
            set(hwnd, |r| r.page = lp as i32);
            old as i64
        }
        0x0416 => get(hwnd).page as i64,
        0x0417 => {
            let old = get(hwnd).line;
            set(hwnd, |r| r.line = lp as i32);
            old as i64
        }
        0x0418 => get(hwnd).line as i64,
        // TBM_CLEARTICS, TBM_SETTIC, TBM_SETSEL*, TBM_SETBUDDY ...
        0x0409 | 0x0404 | 0x040A | 0x040B | 0x040C | 0x0420 => 0,
        _ => unsafe { crate::DefWindowProcW(hwnd, msg, wp, lp) },
    }
}

// -----| up-down control |-----

const UDS_WRAP: u32 = 0x01;
const UDS_SETBUDDYINT: u32 = 0x02;
const UDS_ALIGNRIGHT: u32 = 0x04;
const UDS_ALIGNLEFT: u32 = 0x08;
const UDS_AUTOBUDDY: u32 = 0x10;
const UDS_HORZ: u32 = 0x40;
const UDS_NOTHOUSANDS: u32 = 0x80;

/// Show the position in the buddy window (UDS_SETBUDDYINT).
fn update_buddy(hwnd: Hwnd) {
    let r = get(hwnd);
    if r.buddy == 0 || style(hwnd) & UDS_SETBUDDYINT == 0 {
        return;
    }
    let s = if r.base == 16 {
        alloc::format!("0x{:X}", r.pos)
    } else if style(hwnd) & UDS_NOTHOUSANDS == 0 && r.pos.unsigned_abs() >= 1000 {
        // Thousands separators.
        let digits = alloc::format!("{}", r.pos.unsigned_abs());
        let mut out = alloc::string::String::new();
        for (i, ch) in digits.chars().enumerate() {
            if i > 0 && (digits.len() - i) % 3 == 0 {
                out.push(',');
            }
            out.push(ch);
        }
        if r.pos < 0 { alloc::format!("-{}", out) } else { out }
    } else {
        alloc::format!("{}", r.pos)
    };
    let w: Vec<u16> = s.encode_utf16().chain(core::iter::once(0)).collect();
    unsafe { crate::SetWindowTextW(r.buddy, w.as_ptr()) };
}

/// Sit next to the buddy (UDS_ALIGNRIGHT / UDS_ALIGNLEFT), narrowing it.
fn align_to_buddy(hwnd: Hwnd) {
    let s = style(hwnd);
    let buddy = get(hwnd).buddy;
    if buddy == 0 || s & (UDS_ALIGNRIGHT | UDS_ALIGNLEFT) == 0 {
        return;
    }
    let parent = unsafe { crate::GetParent(hwnd) };
    let mut br = [0i32; 4];
    unsafe { crate::GetWindowRect(buddy, &mut br) };
    unsafe { crate::MapWindowPoints(0, parent, br.as_mut_ptr() as *mut [i32; 2], 2) };
    let w = 17;
    let (bx, ux) = if s & UDS_ALIGNRIGHT != 0 { (br[0], br[2] - w) } else { (br[0] + w, br[0]) };
    unsafe {
        crate::SetWindowPos(buddy, 0, bx, br[1], br[2] - br[0] - w, br[3] - br[1], 0x4 | 0x10);
        crate::SetWindowPos(hwnd, 0, ux, br[1], w, br[3] - br[1], 0x4 | 0x10);
    }
}

/// One step up (+1) or down: UDN_DELTAPOS first (the parent may refuse),
/// then the new position and WM_VSCROLL / WM_HSCROLL.
fn step(hwnd: Hwnd, delta: i32) {
    let r = get(hwnd);
    let mut extra = [0u8; 8];
    extra[0..4].copy_from_slice(&r.pos.to_le_bytes());
    extra[4..8].copy_from_slice(&delta.to_le_bytes());
    if crate::notify(hwnd, -722, &extra) != 0 {
        return;
    }
    let (lo, hi) = (r.min.min(r.max), r.min.max(r.max));
    // Up means towards max, whichever way the range runs.
    let d = if r.min > r.max { -delta } else { delta };
    let mut p = r.pos + d;
    if p > hi {
        p = if style(hwnd) & UDS_WRAP != 0 { lo } else { hi };
    } else if p < lo {
        p = if style(hwnd) & UDS_WRAP != 0 { hi } else { lo };
    }
    set(hwnd, |r| r.pos = p);
    update_buddy(hwnd);
    let parent = unsafe { crate::GetParent(hwnd) };
    let msg = if style(hwnd) & UDS_HORZ != 0 { 0x0114 } else { 0x0115 };
    unsafe {
        crate::SendMessageW(parent, msg, (p as u16 as u64) << 16 | 4, hwnd as i64);
        crate::SendMessageW(parent, msg, 8, hwnd as i64);
    }
}

unsafe extern "system" fn updown_proc(hwnd: Hwnd, msg: u32, wp: u64, lp: i64) -> i64 {
    match msg {
        0x0001 => {
            set(hwnd, |r| {
                r.min = 100;
                r.max = 0;
                r.base = 10;
            });
            // The window before it is its buddy.
            if style(hwnd) & UDS_AUTOBUDDY != 0 {
                let prev = unsafe { crate::GetWindow(hwnd, 3) };
                set(hwnd, |r| r.buddy = prev);
                align_to_buddy(hwnd);
                update_buddy(hwnd);
            }
            0
        }
        0x0082 => {
            STATE.lock().remove(&hwnd);
            unsafe { crate::DefWindowProcW(hwnd, msg, wp, lp) }
        }
        0x000F => {
            paint_with(hwnd, |hdc| {
                let c = client(hwnd);
                let horz = style(hwnd) & UDS_HORZ != 0;
                let (a, b) = if horz { ([0, 0, c[2] / 2, c[3]], [c[2] / 2, 0, c[2], c[3]]) } else { ([0, 0, c[2], c[3] / 2], [0, c[3] / 2, c[2], c[3]]) };
                for (k, r) in [a, b].iter().enumerate() {
                    raised(hdc, *r);
                    let (cx, cy) = ((r[0] + r[2]) / 2, (r[1] + r[3]) / 2);
                    for i in 0..3 {
                        let line = match (horz, k) {
                            (false, 0) => [cx - i, cy - 1 + i, cx + i + 1, cy + i],
                            (false, _) => [cx - i, cy + 1 - i, cx + i + 1, cy + 2 - i],
                            (true, 0) => [cx - 1 + i, cy - i, cx + i, cy + i + 1],
                            (true, _) => [cx + 1 - i, cy - i, cx + 2 - i, cy + i + 1],
                        };
                        fill(hdc, line, 18);
                    }
                }
            });
            0
        }
        0x0014 => 1,
        0x0201 | 0x0203 => {
            let (x, y) = lohi(lp);
            let c = client(hwnd);
            // The upper (or right) half steps up.
            let up = if style(hwnd) & UDS_HORZ != 0 { x >= c[2] / 2 } else { y < c[3] / 2 };
            step(hwnd, if up { 1 } else { -1 });
            0
        }
        // UDM_SETRANGE (MAKELONG(max, min)), UDM_GETRANGE
        0x0465 => {
            let (max, min) = lohi(lp);
            set(hwnd, |r| {
                r.min = min;
                r.max = max;
            });
            0
        }
        0x0466 => {
            let r = get(hwnd);
            ((r.min as u16 as i64) << 16) | r.max as u16 as i64
        }
        // UDM_SETPOS (old position), UDM_GETPOS (position, error flag above)
        0x0467 | 0x0471 => {
            let old = get(hwnd).pos;
            let p = if msg == 0x0467 { lp as i16 as i32 } else { lp as i32 };
            set(hwnd, |r| r.pos = p.clamp(r.min.min(r.max), r.min.max(r.max)));
            update_buddy(hwnd);
            old as i64
        }
        0x0468 | 0x0472 => {
            let r = get(hwnd);
            if msg == 0x0472 {
                if lp != 0 {
                    unsafe { *(lp as *mut i32) = 0 };
                }
                return r.pos as i64;
            }
            r.pos as u16 as i64
        }
        // UDM_SETBUDDY (old buddy), UDM_GETBUDDY
        0x0469 => {
            let old = get(hwnd).buddy;
            set(hwnd, |r| r.buddy = wp);
            align_to_buddy(hwnd);
            update_buddy(hwnd);
            old as i64
        }
        0x046A => get(hwnd).buddy as i64,
        // UDM_SETBASE (old base), UDM_GETBASE
        0x046D => {
            let old = get(hwnd).base;
            if wp == 10 || wp == 16 {
                set(hwnd, |r| r.base = wp as u32);
                update_buddy(hwnd);
            }
            old as i64
        }
        0x046E => get(hwnd).base as i64,
        // UDM_SETRANGE32, UDM_GETRANGE32
        0x046F => {
            set(hwnd, |r| {
                r.min = wp as i32;
                r.max = lp as i32;
            });
            0
        }
        0x0470 => {
            let r = get(hwnd);
            unsafe {
                if wp != 0 {
                    *(wp as *mut i32) = r.min;
                }
                if lp != 0 {
                    *(lp as *mut i32) = r.max;
                }
            }
            0
        }
        // UDM_SETACCEL, UDM_GETACCEL
        0x046B | 0x046C => 0,
        _ => unsafe { crate::DefWindowProcW(hwnd, msg, wp, lp) },
    }
}

#[no_mangle]
pub extern "system" fn CreateUpDownControl(style: u32, x: i32, y: i32, cx: i32, cy: i32, parent: Hwnd, id: i32, inst: u64,
                                           buddy: Hwnd, upper: i32, lower: i32, pos: i32) -> Hwnd {
    let class: Vec<u16> = "msctls_updown32\0".encode_utf16().collect();
    let h = unsafe { crate::CreateWindowExW(0, class.as_ptr(), core::ptr::null(), style, x, y, cx, cy, parent, id as u64, inst, 0) };
    if h != 0 {
        unsafe {
            crate::SendMessageW(h, 0x046F, lower as u64, upper as i64);
            if buddy != 0 {
                crate::SendMessageW(h, 0x0469, buddy, 0);
            }
            crate::SendMessageW(h, 0x0471, 0, pos as i64);
        }
    }
    h
}

// -----| tooltips |-----

/// Tools by (window, id): kept, counted, never shown.
static TOOLS: Lock<Vec<(Hwnd, Hwnd, u64)>> = Lock::new(Vec::new());

unsafe extern "system" fn tooltip_proc(hwnd: Hwnd, msg: u32, wp: u64, lp: i64) -> i64 {
    let key = |lp: i64| -> (Hwnd, u64) {
        // TOOLINFO: cbSize, uFlags, hwnd, uId, ...
        if lp == 0 { (0, 0) } else { unsafe { (*((lp as usize + 8) as *const u64), *((lp as usize + 16) as *const u64)) } }
    };
    match msg {
        // TTM_ADDTOOLA / W
        0x0404 | 0x0432 => {
            let (w, id) = key(lp);
            TOOLS.lock().push((hwnd, w, id));
            1
        }
        // TTM_DELTOOLA / W
        0x0405 | 0x0433 => {
            let (w, id) = key(lp);
            TOOLS.lock().retain(|t| !(t.0 == hwnd && t.1 == w && t.2 == id));
            0
        }
        // TTM_GETTOOLCOUNT
        0x040D => TOOLS.lock().iter().filter(|t| t.0 == hwnd).count() as i64,
        0x0082 => {
            TOOLS.lock().retain(|t| t.0 != hwnd);
            unsafe { crate::DefWindowProcW(hwnd, msg, wp, lp) }
        }
        // Everything else a tooltip is told: accepted.
        0x0400..=0x0450 => 0,
        _ => unsafe { crate::DefWindowProcW(hwnd, msg, wp, lp) },
    }
}
