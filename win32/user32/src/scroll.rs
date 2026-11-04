//! Scroll bars: a window's standard ones (WS_HSCROLL, WS_VSCROLL: part of
//! its non-client area, drawn by WM_NCPAINT's DefWindowProc and clicked
//! through WM_NCLBUTTONDOWN) and the "ScrollBar" control (SB_CTL). Both
//! keep a SCROLLINFO and send WM_HSCROLL/WM_VSCROLL. (The desktop reports
//! clicks, not drags, so the thumb is moved by clicking the track.)

use crate::message::send;
use crate::window::{SCROLL_SIZE, WS_HSCROLL, WS_VSCROLL};
use crate::{state, Bool, Hwnd, ScrollBar};

pub const WM_HSCROLL: u32 = 0x0114;
pub const WM_VSCROLL: u32 = 0x0115;
const SB_HORZ: i32 = 0;
const SB_VERT: i32 = 1;
const SB_CTL: i32 = 2;
const SB_BOTH: i32 = 3;

const SB_LINEUP: u32 = 0;
const SB_LINEDOWN: u32 = 1;
const SB_PAGEUP: u32 = 2;
const SB_PAGEDOWN: u32 = 3;
const SB_THUMBPOSITION: u32 = 4;
const SB_THUMBTRACK: u32 = 5;
const SB_TOP: u32 = 6;
const SB_BOTTOM: u32 = 7;
const SB_ENDSCROLL: u32 = 8;

const SIF_RANGE: u32 = 1;
const SIF_PAGE: u32 = 2;
const SIF_POS: u32 = 4;
const SIF_DISABLENOSCROLL: u32 = 8;
const SIF_TRACKPOS: u32 = 0x10;

pub const HTHSCROLL: u64 = 6;
pub const HTVSCROLL: u64 = 7;

/// SBS_VERT: a vertical scroll bar control.
const SBS_VERT: u32 = 1;

/// Which scroll bar `bar` (SB_HORZ, SB_VERT, SB_CTL) of `hwnd` is: the
/// window and the index into its `scroll`. A control keeps its own in
/// slot 0.
fn slot(hwnd: Hwnd, bar: i32) -> Option<usize> {
    match bar {
        SB_HORZ | SB_CTL => Some(0),
        SB_VERT => Some(1),
        _ => {
            let _ = hwnd;
            None
        }
    }
}

fn clamp(sb: &mut ScrollBar) {
    let top = if sb.page > 0 { sb.max - sb.page as i32 + 1 } else { sb.max };
    sb.pos = sb.pos.clamp(sb.min, top.max(sb.min));
}

/// After a change: redraw the scroll bar.
fn redraw(hwnd: Hwnd, bar: i32) {
    if bar == SB_CTL {
        crate::paint::invalidate(hwnd, None, false);
    } else {
        crate::paint::paint_frame(hwnd);
    }
}

/// SCROLLINFO: cbSize, fMask, nMin, nMax, nPage, nPos, nTrackPos.
#[no_mangle]
pub unsafe extern "system" fn SetScrollInfo(hwnd: Hwnd, bar: i32, si: *const u32, draw: Bool) -> i32 {
    let Some(i) = slot(hwnd, bar) else { return 0 };
    if si.is_null() {
        return 0;
    }
    let f = unsafe { core::slice::from_raw_parts(si, 7) };
    let mask = f[1];
    let pos = {
        let mut s = state();
        let Some(w) = s.get_mut(hwnd) else { return 0 };
        let sb = &mut w.scroll[i];
        if mask & SIF_RANGE != 0 {
            sb.min = f[2] as i32;
            sb.max = f[3] as i32;
        }
        if mask & SIF_PAGE != 0 {
            sb.page = f[4];
        }
        if mask & SIF_POS != 0 {
            sb.pos = f[5] as i32;
        }
        clamp(sb);
        sb.pos
    };
    if draw != 0 {
        redraw(hwnd, bar);
    }
    pos
}

#[no_mangle]
pub unsafe extern "system" fn GetScrollInfo(hwnd: Hwnd, bar: i32, si: *mut u32) -> Bool {
    let Some(i) = slot(hwnd, bar) else { return 0 };
    let Some(sb) = state().get(hwnd).map(|w| w.scroll[i]) else { return 0 };
    if si.is_null() {
        return 0;
    }
    let f = unsafe { core::slice::from_raw_parts_mut(si, 7) };
    let mask = f[1];
    if mask & SIF_RANGE != 0 {
        f[2] = sb.min as u32;
        f[3] = sb.max as u32;
    }
    if mask & SIF_PAGE != 0 {
        f[4] = sb.page;
    }
    if mask & SIF_POS != 0 {
        f[5] = sb.pos as u32;
    }
    if mask & SIF_TRACKPOS != 0 {
        f[6] = (if sb.tracking { sb.track } else { sb.pos }) as u32;
    }
    let _ = SIF_DISABLENOSCROLL;
    1
}

#[no_mangle]
pub extern "system" fn SetScrollPos(hwnd: Hwnd, bar: i32, pos: i32, draw: Bool) -> i32 {
    let mut si = [28u32, SIF_POS, 0, 0, 0, pos as u32, 0];
    let old = GetScrollPos(hwnd, bar);
    unsafe { SetScrollInfo(hwnd, bar, si.as_mut_ptr(), draw) };
    old
}

#[no_mangle]
pub extern "system" fn GetScrollPos(hwnd: Hwnd, bar: i32) -> i32 {
    slot(hwnd, bar).and_then(|i| state().get(hwnd).map(|w| w.scroll[i].pos)).unwrap_or(0)
}

#[no_mangle]
pub extern "system" fn SetScrollRange(hwnd: Hwnd, bar: i32, min: i32, max: i32, draw: Bool) -> Bool {
    let si = [28u32, SIF_RANGE, min as u32, max as u32, 0, 0, 0];
    unsafe { SetScrollInfo(hwnd, bar, si.as_ptr(), draw) };
    1
}

#[no_mangle]
pub extern "system" fn GetScrollRange(hwnd: Hwnd, bar: i32, min: *mut i32, max: *mut i32) -> Bool {
    let Some(sb) = slot(hwnd, bar).and_then(|i| state().get(hwnd).map(|w| w.scroll[i])) else { return 0 };
    unsafe {
        if !min.is_null() {
            *min = sb.min;
        }
        if !max.is_null() {
            *max = sb.max;
        }
    }
    1
}

/// Show or hide standard scroll bars: their style bits, and the window's
/// non-client area.
#[no_mangle]
pub extern "system" fn ShowScrollBar(hwnd: Hwnd, bar: i32, show: Bool) -> Bool {
    let bits = match bar {
        SB_HORZ => WS_HSCROLL,
        SB_VERT => WS_VSCROLL,
        SB_BOTH => WS_HSCROLL | WS_VSCROLL,
        SB_CTL => {
            crate::window::ShowWindow(hwnd, if show != 0 { 5 } else { 0 });
            return 1;
        }
        _ => return 0,
    };
    let changed = {
        let mut s = state();
        let Some(w) = s.get_mut(hwnd) else { return 0 };
        let old = w.style;
        if show != 0 { w.style |= bits } else { w.style &= !bits }
        old != w.style
    };
    if changed {
        crate::window::set_window_pos(hwnd, 0, 0, 0, 0, 0, 0x0020 | 0x1 | 0x2 | 0x4 | 0x10);
    }
    1
}

/// ESB_DISABLE_*: which arrows are disabled.
#[no_mangle]
pub extern "system" fn EnableScrollBar(hwnd: Hwnd, bar: u32, arrows: u32) -> Bool {
    let bars: &[usize] = match bar as i32 {
        SB_HORZ | SB_CTL => &[0],
        SB_VERT => &[1],
        SB_BOTH => &[0, 1],
        _ => return 0,
    };
    if let Some(w) = state().get_mut(hwnd) {
        for &b in bars {
            w.scroll[b].disabled = arrows;
        }
    }
    redraw(hwnd, bar as i32);
    1
}

// -----| drawing |-----

/// Draw a scroll bar in `r` (DC coordinates).
pub fn draw(hdc: u64, r: [i32; 4], vertical: bool, sb: &ScrollBar, enabled: bool) {
    let face = crate::paint::GetSysColorBrush(15);
    let track = crate::paint::GetSysColorBrush(0);
    let dark = crate::paint::GetSysColorBrush(16);
    let light = crate::paint::GetSysColorBrush(20);
    let arrow = crate::paint::GetSysColorBrush(if enabled && sb.disabled != 3 { 18 } else { 17 });
    crate::paint::FillRect(hdc, &r, track);
    let (len, thick) = if vertical { (r[3] - r[1], r[2] - r[0]) } else { (r[2] - r[0], r[3] - r[1]) };
    let btn = thick.min(len / 2);
    let at = |a: i32, b: i32| if vertical { [r[0], r[1] + a, r[2], r[1] + b] } else { [r[0] + a, r[1], r[0] + b, r[3]] };
    let button = |rr: [i32; 4]| {
        crate::paint::FillRect(hdc, &rr, face);
        crate::paint::FillRect(hdc, &[rr[0], rr[1], rr[2], rr[1] + 1], light);
        crate::paint::FillRect(hdc, &[rr[0], rr[1], rr[0] + 1, rr[3]], light);
        crate::paint::FillRect(hdc, &[rr[0], rr[3] - 1, rr[2], rr[3]], dark);
        crate::paint::FillRect(hdc, &[rr[2] - 1, rr[1], rr[2], rr[3]], dark);
    };
    // Arrows: a small triangle in each end button.
    for (k, rr) in [at(0, btn), at(len - btn, len)].into_iter().enumerate() {
        button(rr);
        let (cx, cy) = ((rr[0] + rr[2]) / 2, (rr[1] + rr[3]) / 2);
        for i in 0..4 {
            let line = match (vertical, k) {
                (true, 0) => [cx - i, cy - 2 + i, cx + i + 1, cy - 1 + i],
                (true, _) => [cx - i, cy + 1 - i, cx + i + 1, cy + 2 - i],
                (false, 0) => [cx - 2 + i, cy - i, cx - 1 + i, cy + i + 1],
                (false, _) => [cx + 1 - i, cy - i, cx + 2 - i, cy + i + 1],
            };
            crate::paint::FillRect(hdc, &line, arrow);
        }
    }
    // The thumb, if there's room and range.
    let range = sb.max - sb.min + 1;
    let track_len = len - 2 * btn;
    if enabled && range > 0 && track_len > 8 && (sb.page == 0 || (sb.page as i32) < range) {
        let size = if sb.page > 0 { (track_len * sb.page as i32 / range).max(8) } else { thick.max(8) };
        let span = (range - (sb.page as i32).max(1)).max(1);
        let shown = if sb.tracking { sb.track } else { sb.pos };
        let off = btn + (track_len - size) * (shown - sb.min) / span;
        button(at(off, off + size));
    }
}

/// The standard scroll bars of `hwnd` in window coordinates (horizontal,
/// vertical), where it has them.
pub fn bar_rects(hwnd: Hwnd) -> [Option<[i32; 4]>; 2] {
    let s = state();
    let Some(w) = s.get(hwnd) else { return [None, None] };
    let f = crate::window::frame_margins(w.style, w.ex_style, w.parent == 0);
    let m = w.margins;
    let v = (w.style & WS_VSCROLL != 0).then(|| [w.w - f[2] - SCROLL_SIZE, m[1], w.w - f[2], w.h - m[3]]);
    let h = (w.style & WS_HSCROLL != 0).then(|| [m[0], w.h - f[3] - SCROLL_SIZE, w.w - m[2], w.h - f[3]]);
    [h, v]
}

/// WM_NCPAINT: draw `hwnd`'s standard scroll bars on `hdc` (its window DC).
pub fn paint_bars(hwnd: Hwnd, hdc: u64) {
    let rects = bar_rects(hwnd);
    let (bars, enabled) = match state().get(hwnd) {
        Some(w) => (w.scroll, w.enabled),
        None => return,
    };
    for (i, r) in rects.iter().enumerate() {
        if let Some(r) = r {
            draw(hdc, *r, i == 1, &bars[i], enabled);
        }
    }
    // The square between two bars.
    if let (Some(h), Some(v)) = (rects[0], rects[1]) {
        crate::paint::FillRect(hdc, &[v[0], h[1], v[2], h[3]], crate::paint::GetSysColorBrush(15));
    }
}

/// What a click at `along` (pixels from the bar's start) does: the
/// scroll code.
fn click_code(len: i32, thick: i32, sb: &ScrollBar, along: i32) -> u32 {
    let btn = thick.min(len / 2);
    if along < btn {
        return SB_LINEUP;
    }
    if along >= len - btn {
        return SB_LINEDOWN;
    }
    let range = sb.max - sb.min + 1;
    let track_len = len - 2 * btn;
    let size = if sb.page > 0 { (track_len * sb.page as i32 / range.max(1)).max(8) } else { thick.max(8) };
    let span = (range - (sb.page as i32).max(1)).max(1);
    let off = btn + (track_len - size) * (sb.pos - sb.min) / span;
    if along < off {
        SB_PAGEUP
    } else if along >= off + size {
        SB_PAGEDOWN
    } else {
        SB_THUMBPOSITION
    }
}

/// A click on a standard scroll bar of `hwnd` at screen (sx, sy): the
/// WM_HSCROLL/WM_VSCROLL it means, then SB_ENDSCROLL.
pub fn nc_click(hwnd: Hwnd, vertical: bool, sx: i32, sy: i32) {
    let mut wr = [0i32; 4];
    crate::window::GetWindowRect(hwnd, &mut wr);
    let i = vertical as usize;
    let Some(r) = bar_rects(hwnd)[i] else { return };
    let (x, y) = (sx - wr[0] - r[0], sy - wr[1] - r[1]);
    let (len, thick, along) = if vertical { (r[3] - r[1], r[2] - r[0], y) } else { (r[2] - r[0], r[3] - r[1], x) };
    let bar = if vertical { SB_VERT } else { SB_HORZ };
    track(hwnd, bar, Bar { target: hwnd, ctl: 0, msg: if vertical { WM_VSCROLL } else { WM_HSCROLL }, vertical, origin: (wr[0] + r[0], wr[1] + r[1]), len, thick }, along);
}

/// A scroll bar being pressed.
struct Bar {
    /// Who hears, and the control to name (0 for a window's own bar).
    target: Hwnd,
    ctl: i64,
    msg: u32,
    vertical: bool,
    /// Its start on the screen, and its size.
    origin: (i32, i32),
    len: i32,
    thick: i32,
}

fn bar_info(hwnd: Hwnd, bar: i32) -> ScrollBar {
    match slot(hwnd, bar) {
        Some(i) => state().get(hwnd).map(|w| w.scroll[i]).unwrap_or_default(),
        None => ScrollBar::default(),
    }
}

fn set_tracking(hwnd: Hwnd, bar: i32, tracking: bool, pos: i32) {
    if let Some(i) = slot(hwnd, bar) {
        if let Some(w) = state().get_mut(hwnd) {
            w.scroll[i].tracking = tracking;
            w.scroll[i].track = pos;
        }
    }
    redraw(hwnd, bar);
}

/// Hold the button down on a scroll bar until it's released: the thumb
/// follows the mouse (SB_THUMBTRACK, then SB_THUMBPOSITION), the arrows and
/// the track repeat. Other messages go on being handled meanwhile.
fn track(hwnd: Hwnd, bar: i32, b: Bar, along: i32) {
    let sb = bar_info(hwnd, bar);
    let code = click_code(b.len, b.thick, &sb, along);
    let geometry = |sb: &ScrollBar| {
        let btn = b.thick.min(b.len / 2);
        let range = sb.max - sb.min + 1;
        let track_len = b.len - 2 * btn;
        let size = if sb.page > 0 { (track_len * sb.page as i32 / range.max(1)).max(8) } else { b.thick.max(8) };
        let span = (range - (sb.page as i32).max(1)).max(1);
        (btn, track_len - size, span)
    };
    crate::window::SetCapture(hwnd);
    let mut m = nt_abi::Msg::default();
    if code == SB_THUMBPOSITION {
        let (btn, room, span) = geometry(&sb);
        let start = btn + room * (sb.pos - sb.min) / span;
        let grab = along - start;
        let mut pos = sb.pos;
        set_tracking(hwnd, bar, true, pos);
        loop {
            if crate::message::GetMessageW(&mut m, 0, 0, 0) == 0 {
                crate::message::PostQuitMessage(m.wparam as i32);
                break;
            }
            match m.message {
                0x0200 | 0x00A0 => {
                    let at = if b.vertical { m.pt_y - b.origin.1 } else { m.pt_x - b.origin.0 };
                    let cur = bar_info(hwnd, bar);
                    let (btn, room, span) = geometry(&cur);
                    let new = if room > 0 { cur.min + (((at - grab - btn) as i64 * span as i64 + room as i64 / 2) / room as i64) as i32 } else { cur.min };
                    let top = if cur.page > 0 { cur.max - cur.page as i32 + 1 } else { cur.max };
                    let new = new.clamp(cur.min, top.max(cur.min));
                    if new != pos {
                        pos = new;
                        set_tracking(hwnd, bar, true, pos);
                        send(b.target, b.msg, (pos as u16 as u64) << 16 | SB_THUMBTRACK as u64, b.ctl);
                    }
                }
                0x0202 | 0x00A2 => break,
                _ => {
                    crate::message::TranslateMessage(&m);
                    crate::message::DispatchMessageW(&m);
                }
            }
        }
        set_tracking(hwnd, bar, false, pos);
        send(b.target, b.msg, (pos as u16 as u64) << 16 | SB_THUMBPOSITION as u64, b.ctl);
    } else {
        send(b.target, b.msg, code as u64, b.ctl);
        // As Windows: a pause, then a steady repeat while it's held.
        let mut next = unsafe { GetTickCount() }.wrapping_add(350);
        'held: loop {
            while crate::message::PeekMessageW(&mut m, 0, 0, 0, 1) != 0 {
                match m.message {
                    0x0202 | 0x00A2 => break 'held,
                    0x0200 | 0x00A0 => {}
                    0x0012 => {
                        crate::message::PostQuitMessage(m.wparam as i32);
                        break 'held;
                    }
                    _ => {
                        crate::message::TranslateMessage(&m);
                        crate::message::DispatchMessageW(&m);
                    }
                }
            }
            let now = unsafe { GetTickCount() };
            if now.wrapping_sub(next) as i32 >= 0 {
                next = now.wrapping_add(50);
                // Paging stops when the thumb reaches the pointer.
                let mut p = [0i32; 2];
                crate::paint::GetCursorPos(&mut p);
                let at = if b.vertical { p[1] - b.origin.1 } else { p[0] - b.origin.0 };
                if click_code(b.len, b.thick, &bar_info(hwnd, bar), at) == code {
                    send(b.target, b.msg, code as u64, b.ctl);
                }
            }
            unsafe { Sleep(10) };
        }
    }
    crate::window::ReleaseCapture();
    send(b.target, b.msg, SB_ENDSCROLL as u64, b.ctl);
}

#[link(name = "kernel32", kind = "raw-dylib")]
unsafe extern "system" {
    fn GetTickCount() -> u32;
    fn Sleep(ms: u32);
}

// -----| the ScrollBar control |-----

pub unsafe extern "system" fn scrollbar_proc(hwnd: Hwnd, msg: u32, wp: u64, lp: i64) -> i64 {
    const WM_PAINT: u32 = 0x000F;
    const WM_LBUTTONDOWN: u32 = 0x0201;
    const WM_KEYDOWN: u32 = 0x0100;
    const SBM_SETPOS: u32 = 0xE0;
    const SBM_GETPOS: u32 = 0xE1;
    const SBM_SETRANGE: u32 = 0xE2;
    const SBM_GETRANGE: u32 = 0xE3;
    const SBM_SETRANGEREDRAW: u32 = 0xE6;
    const SBM_SETSCROLLINFO: u32 = 0xE9;
    const SBM_GETSCROLLINFO: u32 = 0xEA;
    let Some((style, parent, enabled)) = state().get(hwnd).map(|w| (w.style, w.parent, w.enabled)) else { return 0 };
    let vertical = style & SBS_VERT != 0;
    match msg {
        WM_PAINT => {
            let mut ps = [0u8; 72];
            let hdc = crate::paint::BeginPaint(hwnd, ps.as_mut_ptr());
            let mut r = [0i32; 4];
            crate::window::GetClientRect(hwnd, &mut r);
            let sb = state().get(hwnd).map(|w| w.scroll[0]).unwrap_or_default();
            draw(hdc, r, vertical, &sb, enabled);
            crate::paint::EndPaint(hwnd, ps.as_ptr());
            0
        }
        WM_LBUTTONDOWN => {
            let mut r = [0i32; 4];
            crate::window::GetClientRect(hwnd, &mut r);
            let (x, y) = ((lp & 0xFFFF) as i16 as i32, ((lp >> 16) & 0xFFFF) as i16 as i32);
            let (len, thick, along) = if vertical { (r[3], r[2], y) } else { (r[2], r[3], x) };
            let mut o = [[0i32, 0i32]];
            crate::window::MapWindowPoints(hwnd, 0, o.as_mut_ptr(), 1);
            let m = if vertical { WM_VSCROLL } else { WM_HSCROLL };
            track(hwnd, SB_CTL, Bar { target: parent, ctl: hwnd as i64, msg: m, vertical, origin: (o[0][0], o[0][1]), len, thick }, along);
            0
        }
        WM_KEYDOWN => {
            let code = match wp as u32 {
                0x25 | 0x26 => Some(SB_LINEUP),
                0x27 | 0x28 => Some(SB_LINEDOWN),
                0x21 => Some(SB_PAGEUP),
                0x22 => Some(SB_PAGEDOWN),
                0x24 => Some(SB_TOP),
                0x23 => Some(SB_BOTTOM),
                _ => None,
            };
            if let Some(c) = code {
                let m = if vertical { WM_VSCROLL } else { WM_HSCROLL };
                send(parent, m, c as u64, hwnd as i64);
                send(parent, m, SB_ENDSCROLL as u64, hwnd as i64);
            }
            0
        }
        SBM_SETPOS => SetScrollPos(hwnd, SB_CTL, wp as i32, lp as i32) as i64,
        SBM_GETPOS => GetScrollPos(hwnd, SB_CTL) as i64,
        SBM_SETRANGE | SBM_SETRANGEREDRAW => {
            SetScrollRange(hwnd, SB_CTL, wp as i32, lp as i32, (msg == SBM_SETRANGEREDRAW) as Bool);
            0
        }
        SBM_GETRANGE => {
            GetScrollRange(hwnd, SB_CTL, wp as *mut i32, lp as *mut i32);
            0
        }
        SBM_SETSCROLLINFO => (unsafe { SetScrollInfo(hwnd, SB_CTL, lp as *const u32, wp as Bool) }) as i64,
        SBM_GETSCROLLINFO => (unsafe { GetScrollInfo(hwnd, SB_CTL, lp as *mut u32) }) as i64,
        // WM_GETDLGCODE: arrow keys.
        0x0087 => 0x0001,
        _ => crate::message::def_window_proc(hwnd, msg, wp, lp, true),
    }
}
