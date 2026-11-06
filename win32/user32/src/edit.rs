//! The Edit control: single-line and multiline (wrapping at word
//! boundaries unless ES_AUTOHSCROLL), selection, the caret, text limits,
//! password characters, read-only, ES_NUMBER/UPPERCASE/LOWERCASE, the EM_*
//! messages and EN_* notifications in Windows' order. Its text is the
//! window's text (WM_GETTEXT reads it).

use alloc::vec::Vec;

use crate::controls::{self, notify, Kind, DLGC_HASSETSEL, DLGC_WANTALLKEYS, DLGC_WANTARROWS, DLGC_WANTCHARS};
use crate::message::def_window_proc;
use crate::{state, Hwnd};

const ES_MULTILINE: u32 = 0x0004;
const ES_UPPERCASE: u32 = 0x0008;
const ES_LOWERCASE: u32 = 0x0010;
const ES_PASSWORD: u32 = 0x0020;
const ES_AUTOHSCROLL: u32 = 0x0080;
const ES_NOHIDESEL: u32 = 0x0100;
const ES_READONLY: u32 = 0x0800;
const ES_WANTRETURN: u32 = 0x1000;
const ES_NUMBER: u32 = 0x2000;

const EN_SETFOCUS: u32 = 0x0100;
const EN_KILLFOCUS: u32 = 0x0200;
const EN_CHANGE: u32 = 0x0300;
const EN_UPDATE: u32 = 0x0400;
const EN_MAXTEXT: u32 = 0x0501;

#[link(name = "kernel32", kind = "raw-dylib")]
unsafe extern "system" {
    fn LocalAlloc(flags: u32, size: usize) -> u64;
    fn LocalReAlloc(h: u64, size: usize, flags: u32) -> u64;
    fn LocalFree(p: u64) -> u64;
    fn LocalLock(h: u64) -> *mut u16;
    fn LocalUnlock(h: u64) -> i32;
    fn LocalSize(h: u64) -> usize;
}

const CR: u16 = 13;
const LF: u16 = 10;

pub struct Edit {
    /// Selection: anchor and caret (character indices).
    pub anchor: usize,
    pub caret: usize,
    pub limit: usize,
    pub modified: bool,
    pub password: u16,
    pub focused: bool,
    /// Scrolled: pixels (single-line) and the first visible line.
    pub scroll_x: i32,
    pub top: usize,
    /// EM_GETHANDLE's block (LocalAlloc'd), and the text before the last
    /// change (EM_UNDO).
    pub local: u64,
    pub undo: Option<(Vec<u16>, usize, usize)>,
    /// EM_SETEXTENDEDSTYLE's flags and EM_SETENDOFLINE's line ending.
    pub ex_style: u32,
    pub eol: u32,
    /// EM_SETWORDBREAKPROC's procedure (0: the default), kept for
    /// EM_GETWORDBREAKPROC.
    pub word_break: u64,
}

fn get<R>(hwnd: Hwnd, f: impl FnOnce(&mut Edit) -> R) -> Option<R> {
    controls::with(hwnd, |c| match &mut c.kind {
        Kind::Edit(e) => Some(f(e)),
        _ => None,
    })
    .flatten()
}

fn text(hwnd: Hwnd) -> Vec<u16> {
    controls::text(hwnd)
}

fn sel(hwnd: Hwnd) -> (usize, usize) {
    get(hwnd, |e| (e.anchor.min(e.caret), e.anchor.max(e.caret))).unwrap_or((0, 0))
}

/// A DC on the edit with its font: for measuring.
fn measure_dc(hwnd: Hwnd) -> (u64, u64) {
    let hdc = crate::paint::GetDC(hwnd);
    (hdc, controls::select_font(hwnd, hdc))
}

fn release_dc(hwnd: Hwnd, dc: (u64, u64)) {
    if dc.1 != 0 {
        unsafe { crate::SelectObject(dc.0, dc.1) };
    }
    crate::paint::ReleaseDC(hwnd, dc.0);
}

fn width(hdc: u64, s: &[u16]) -> i32 {
    let mut sz = [0i32; 2];
    unsafe { crate::GetTextExtentPoint32W(hdc, s.as_ptr(), s.len() as i32, &mut sz) };
    sz[0]
}

fn line_height(hdc: u64) -> i32 {
    let mut tm = [0u8; 60];
    unsafe { crate::GetTextMetricsW(hdc, tm.as_mut_ptr()) };
    i32::from_le_bytes(tm[0..4].try_into().unwrap()).max(1)
}

/// What's shown for `t`: the password character in its place.
fn shown(t: &[u16], password: u16) -> Vec<u16> {
    if password != 0 { alloc::vec![password; t.len()] } else { t.to_vec() }
}

/// The visual lines: (start, end) indices, end before any line break.
fn lines(hwnd: Hwnd, t: &[u16], hdc: u64) -> Vec<(usize, usize)> {
    let style = controls::style(hwnd);
    if style & ES_MULTILINE == 0 {
        return alloc::vec![(0, t.len())];
    }
    let wrap = style & ES_AUTOHSCROLL == 0;
    let max = controls::client(hwnd)[2] - 2;
    let mut out = Vec::new();
    let mut start = 0;
    loop {
        // The paragraph to the next line break.
        let mut end = start;
        while end < t.len() && t[end] != CR && t[end] != LF {
            end += 1;
        }
        // Wrapped at spaces where it doesn't fit.
        let mut s = start;
        while wrap && s < end && width(hdc, &t[s..end]) > max {
            let mut cut = s;
            let mut i = s;
            while i < end {
                if t[i] == b' ' as u16 {
                    if width(hdc, &t[s..i]) > max {
                        break;
                    }
                    cut = i + 1;
                }
                i += 1;
            }
            if cut == s {
                // One long word: as much of it as fits.
                cut = s + 1;
                while cut < end && width(hdc, &t[s..cut + 1]) <= max {
                    cut += 1;
                }
            }
            out.push((s, cut));
            s = cut;
        }
        out.push((s, end));
        if end >= t.len() {
            break;
        }
        // Past "\r\n" (or a lone \r or \n).
        start = if t[end] == CR && end + 1 < t.len() && t[end + 1] == LF { end + 2 } else { end + 1 };
        if start > t.len() {
            break;
        }
    }
    out
}

/// The lines between line breaks (no wrapping): Windows 10's "file lines".
fn file_lines(t: &[u16]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut start = 0;
    loop {
        let mut end = start;
        while end < t.len() && t[end] != CR && t[end] != LF {
            end += 1;
        }
        out.push((start, end));
        if end >= t.len() {
            return out;
        }
        start = if t[end] == CR && end + 1 < t.len() && t[end + 1] == LF { end + 2 } else { end + 1 };
    }
}

const EC_ENDOFLINE_CRLF: u32 = 1;
const LMEM_MOVEABLE: u32 = 0x2;

fn line_of(ls: &[(usize, usize)], i: usize) -> usize {
    ls.iter().rposition(|l| l.0 <= i).unwrap_or(0)
}

/// Put the text, keeping the selection inside it.
fn set_text_raw(hwnd: Hwnd, t: Vec<u16>) {
    let n = t.len();
    crate::window::set_text(hwnd, t);
    get(hwnd, |e| {
        e.anchor = e.anchor.min(n);
        e.caret = e.caret.min(n);
    });
}

/// A change by the user (or EM_REPLACESEL): replace the selection with
/// `ins`, within the limit, and say so.
fn replace_sel(hwnd: Hwnd, ins: &[u16], user: bool) -> bool {
    let t = text(hwnd);
    let (a, b) = sel(hwnd);
    let limit = get(hwnd, |e| e.limit).unwrap_or(usize::MAX);
    let room = limit.saturating_sub(t.len() - (b - a));
    let ins = if ins.len() > room {
        notify(hwnd, EN_MAXTEXT);
        if user && room == 0 {
            return false;
        }
        &ins[..room]
    } else {
        ins
    };
    if ins.is_empty() && a == b {
        return false;
    }
    let mut n = t[..a].to_vec();
    n.extend_from_slice(ins);
    n.extend_from_slice(&t[b..]);
    get(hwnd, |e| e.undo = Some((t.clone(), a, b)));
    set_text_raw(hwnd, n);
    let at = a + ins.len();
    get(hwnd, |e| {
        e.anchor = at;
        e.caret = at;
        e.modified = true;
    });
    changed(hwnd);
    true
}

fn changed(hwnd: Hwnd) {
    scroll_to_caret(hwnd);
    crate::paint::invalidate(hwnd, None, true);
    notify(hwnd, EN_UPDATE);
    notify(hwnd, EN_CHANGE);
}

/// Where the caret is drawn (client coordinates).
fn caret_pos(hwnd: Hwnd) -> (i32, i32, i32) {
    let t = text(hwnd);
    let dc = measure_dc(hwnd);
    let ls = lines(hwnd, &t, dc.0);
    let lh = line_height(dc.0);
    let (caret, password, scroll_x, top) = get(hwnd, |e| (e.caret, e.password, e.scroll_x, e.top)).unwrap_or((0, 0, 0, 0));
    let li = line_of(&ls, caret);
    let (s, _) = ls[li];
    let x = 1 + width(dc.0, &shown(&t[s..caret.min(t.len())], password)) - scroll_x;
    let y = 1 + (li as i32 - top as i32) * lh;
    release_dc(hwnd, dc);
    (x, y, lh)
}

/// Scroll so the caret shows, then move the caret there.
fn scroll_to_caret(hwnd: Hwnd) {
    let r = controls::client(hwnd);
    let t = text(hwnd);
    let dc = measure_dc(hwnd);
    let ls = lines(hwnd, &t, dc.0);
    let lh = line_height(dc.0);
    let (caret, password) = get(hwnd, |e| (e.caret, e.password)).unwrap_or((0, 0));
    let li = line_of(&ls, caret);
    let x = width(dc.0, &shown(&t[ls[li].0..caret.min(t.len())], password));
    release_dc(hwnd, dc);
    let visible = ((r[3] - 2) / lh).max(1) as usize;
    get(hwnd, |e| {
        if x - e.scroll_x > r[2] - 3 {
            e.scroll_x = x - (r[2] - 3);
        }
        if x < e.scroll_x {
            e.scroll_x = (x - r[2] / 3).max(0);
        }
        if li < e.top {
            e.top = li;
        }
        if li >= e.top + visible {
            e.top = li + 1 - visible;
        }
    });
    update_scrollbar(hwnd, ls.len(), visible);
    place_caret(hwnd);
}

fn update_scrollbar(hwnd: Hwnd, lines: usize, visible: usize) {
    if controls::style(hwnd) & crate::window::WS_VSCROLL != 0 {
        let top = get(hwnd, |e| e.top).unwrap_or(0);
        let si = [28u32, 7, 0, (lines.max(1) - 1) as u32, visible as u32, top as u32, 0];
        unsafe { crate::scroll::SetScrollInfo(hwnd, 1, si.as_ptr(), 1) };
    }
}

fn place_caret(hwnd: Hwnd) {
    if get(hwnd, |e| e.focused) == Some(true) {
        let (x, y, _) = caret_pos(hwnd);
        crate::keyboard::SetCaretPos(x, y);
    }
}

/// The character nearest client point (x, y).
fn char_at(hwnd: Hwnd, x: i32, y: i32) -> usize {
    let t = text(hwnd);
    let dc = measure_dc(hwnd);
    let ls = lines(hwnd, &t, dc.0);
    let lh = line_height(dc.0);
    let (password, scroll_x, top) = get(hwnd, |e| (e.password, e.scroll_x, e.top)).unwrap_or((0, 0, 0));
    let li = (top as i32 + (y - 1).max(0) / lh).clamp(0, ls.len() as i32 - 1) as usize;
    let (s, e) = ls[li];
    let px = x - 1 + scroll_x;
    let mut best = s;
    for i in s..=e {
        let w = width(dc.0, &shown(&t[s..i], password));
        if w <= px {
            best = i;
        }
        let next = if i < e { width(dc.0, &shown(&t[s..i + 1], password)) } else { w };
        if px < (w + next) / 2 {
            best = i;
            break;
        }
    }
    release_dc(hwnd, dc);
    best
}

fn paint(hwnd: Hwnd, hdc: u64) {
    let (style, enabled) = state().get(hwnd).map_or((0, true), |w| (w.style, w.enabled));
    let msg = if style & ES_READONLY != 0 || !enabled { controls::WM_CTLCOLORSTATIC } else { controls::WM_CTLCOLOREDIT };
    let old_font = controls::select_font(hwnd, hdc);
    let brush = controls::ctl_color(hwnd, hdc, msg);
    let r = controls::client(hwnd);
    crate::paint::FillRect(hdc, &r, brush);
    let t = text(hwnd);
    let ls = lines(hwnd, &t, hdc);
    let lh = line_height(hdc);
    let (a, b) = sel(hwnd);
    let (password, scroll_x, top, focused) = get(hwnd, |e| (e.password, e.scroll_x, e.top, e.focused)).unwrap_or((0, 0, 0, false));
    let show_sel = a != b && (focused || style & ES_NOHIDESEL != 0);
    if !enabled {
        unsafe { crate::SetTextColor(hdc, crate::paint::GetSysColor(17)) };
    }
    unsafe { crate::SetBkMode(hdc, 1) };
    for (i, &(s, e)) in ls.iter().enumerate().skip(top) {
        let y = 1 + (i - top) as i32 * lh;
        if y > r[3] {
            break;
        }
        let line = shown(&t[s..e], password);
        let x0 = 1 - scroll_x;
        unsafe { crate::TextOutW(hdc, x0, y, line.as_ptr(), line.len() as i32) };
        if show_sel && b > s && a < e.max(s + 1) {
            let (sa, sb) = (a.max(s), b.min(e));
            let xa = x0 + width(hdc, &shown(&t[s..sa], password));
            let xb = x0 + width(hdc, &shown(&t[s..sb], password));
            let hl = [xa, y, xb.max(xa + if b > e { 4 } else { 0 }), y + lh];
            crate::paint::FillRect(hdc, &hl, crate::paint::GetSysColorBrush(13));
            let part = shown(&t[sa..sb], password);
            let old = unsafe { crate::SetTextColor(hdc, crate::paint::GetSysColor(14)) };
            unsafe { crate::TextOutW(hdc, xa, y, part.as_ptr(), part.len() as i32) };
            unsafe { crate::SetTextColor(hdc, old) };
        }
    }
    if old_font != 0 {
        unsafe { crate::SelectObject(hdc, old_font) };
    }
}

/// Characters a typed `ch` puts in (none: rejected).
fn filter(hwnd: Hwnd, ch: u16) -> Option<Vec<u16>> {
    let style = controls::style(hwnd);
    let multi = style & ES_MULTILINE != 0;
    match ch {
        CR if multi => Some(alloc::vec![CR, LF]),
        9 if multi => Some(alloc::vec![9]),
        0..=31 => None,
        _ if style & ES_NUMBER != 0 && !(b'0' as u16..=b'9' as u16).contains(&ch) => None,
        _ if style & ES_UPPERCASE != 0 && (b'a' as u16..=b'z' as u16).contains(&ch) => Some(alloc::vec![ch - 32]),
        _ if style & ES_LOWERCASE != 0 && (b'A' as u16..=b'Z' as u16).contains(&ch) => Some(alloc::vec![ch + 32]),
        _ => Some(alloc::vec![ch]),
    }
}

/// Move the caret to `to` (extending the selection with Shift).
fn move_caret(hwnd: Hwnd, to: usize, extend: bool) {
    get(hwnd, |e| {
        e.caret = to;
        if !extend {
            e.anchor = to;
        }
    });
    scroll_to_caret(hwnd);
    crate::paint::invalidate(hwnd, None, true);
}

fn key_down(hwnd: Hwnd, vk: u32) {
    let shift = crate::keyboard::GetKeyState(0x10) < 0;
    let ctrl = crate::keyboard::GetKeyState(0x11) < 0;
    let t = text(hwnd);
    let (caret, anchor) = get(hwnd, |e| (e.caret, e.anchor)).unwrap_or((0, 0));
    let dc = measure_dc(hwnd);
    let ls = lines(hwnd, &t, dc.0);
    release_dc(hwnd, dc);
    let li = line_of(&ls, caret);
    let word = |mut i: usize, fwd: bool| {
        if fwd {
            while i < t.len() && t[i] != b' ' as u16 {
                i += 1;
            }
            while i < t.len() && t[i] == b' ' as u16 {
                i += 1;
            }
        } else {
            while i > 0 && t[i - 1] == b' ' as u16 {
                i -= 1;
            }
            while i > 0 && t[i - 1] != b' ' as u16 {
                i -= 1;
            }
        }
        i
    };
    // A line break's two characters move as one.
    let left = |i: usize| if i >= 2 && t[i - 1] == LF && t[i - 2] == CR { i - 2 } else { i.saturating_sub(1) };
    let right = |i: usize| if i + 1 < t.len() && t[i] == CR && t[i + 1] == LF { i + 2 } else { (i + 1).min(t.len()) };
    match vk {
        0x25 => {
            let to = if !shift && anchor != caret { anchor.min(caret) } else if ctrl { word(caret, false) } else { left(caret) };
            move_caret(hwnd, to, shift);
        }
        0x27 => {
            let to = if !shift && anchor != caret { anchor.max(caret) } else if ctrl { word(caret, true) } else { right(caret) };
            move_caret(hwnd, to, shift);
        }
        0x24 => move_caret(hwnd, if ctrl { 0 } else { ls[li].0 }, shift),
        0x23 => move_caret(hwnd, if ctrl { t.len() } else { ls[li].1 }, shift),
        0x26 | 0x28 if ls.len() > 1 => {
            let target = if vk == 0x26 { li.checked_sub(1) } else { (li + 1 < ls.len()).then_some(li + 1) };
            if let Some(nl) = target {
                let col = caret - ls[li].0;
                move_caret(hwnd, (ls[nl].0 + col).min(ls[nl].1), shift);
            }
        }
        0x2E => {
            // Delete: the selection, or the next character.
            if controls::style(hwnd) & ES_READONLY != 0 {
                return;
            }
            if anchor == caret {
                get(hwnd, |e| e.caret = right(caret));
            }
            replace_sel(hwnd, &[], true);
        }
        _ => {}
    }
}

pub unsafe extern "system" fn edit_proc(hwnd: Hwnd, msg: u32, wp: u64, lp: i64) -> i64 {
    if let Some(r) = controls::common(hwnd, msg, wp, lp) {
        if msg == controls::WM_SETFONT {
            scroll_to_caret(hwnd);
        }
        return r;
    }
    let style = controls::style(hwnd);
    let multi = style & ES_MULTILINE != 0;
    match msg {
        0x0081 => {
            // WM_NCCREATE
            controls::border_to_edge(hwnd);
            let password = if style & ES_PASSWORD != 0 && !multi { b'*' as u16 } else { 0 };
            controls::add(hwnd, Kind::Edit(Edit { anchor: 0, caret: 0, limit: 0x7FFF_FFFE, modified: false, password, focused: false, scroll_x: 0, top: 0, local: 0, undo: None, ex_style: 0, eol: EC_ENDOFLINE_CRLF, word_break: 0 }));
            def_window_proc(hwnd, msg, wp, lp, true)
        }
        0x000F => {
            let mut ps = [0u8; 72];
            let hdc = crate::paint::BeginPaint(hwnd, ps.as_mut_ptr());
            paint(hwnd, hdc);
            crate::paint::EndPaint(hwnd, ps.as_ptr());
            0
        }
        0x0014 => 1,
        0x0005 => {
            scroll_to_caret(hwnd);
            0
        }
        0x000C => {
            // WM_SETTEXT: no undo, unmodified, caret at the start. A
            // single-line edit says it changed.
            let r = def_window_proc(hwnd, msg, wp, lp, true);
            get(hwnd, |e| {
                e.anchor = 0;
                e.caret = 0;
                e.modified = false;
                e.scroll_x = 0;
                e.top = 0;
            });
            if multi {
                scroll_to_caret(hwnd);
                crate::paint::invalidate(hwnd, None, true);
            } else {
                changed(hwnd);
            }
            r
        }
        0x0102 => {
            // WM_CHAR
            let ch = wp as u16;
            if style & ES_READONLY != 0 {
                return 0;
            }
            if ch == 8 {
                let (a, b) = sel(hwnd);
                if a == b {
                    let t = text(hwnd);
                    let back = if a >= 2 && t[a - 1] == LF && t[a - 2] == CR { 2 } else { 1 };
                    if a == 0 {
                        return 0;
                    }
                    get(hwnd, |e| e.anchor = a - back);
                }
                replace_sel(hwnd, &[], true);
                return 0;
            }
            if let Some(ins) = filter(hwnd, ch) {
                replace_sel(hwnd, &ins, true);
            }
            0
        }
        0x0100 => {
            key_down(hwnd, wp as u32);
            0
        }
        0x0201 => {
            crate::window::SetFocus(hwnd);
            let (x, y) = ((lp & 0xFFFF) as i16 as i32, ((lp >> 16) & 0xFFFF) as i16 as i32);
            let i = char_at(hwnd, x, y);
            move_caret(hwnd, i, crate::keyboard::GetKeyState(0x10) < 0);
            crate::window::SetCapture(hwnd);
            0
        }
        // A double click takes the word.
        0x0203 => {
            let (x, y) = ((lp & 0xFFFF) as i16 as i32, ((lp >> 16) & 0xFFFF) as i16 as i32);
            let i = char_at(hwnd, x, y);
            let t = text(hwnd);
            let is_word = |c: u16| c != b' ' as u16 && c != b'\t' as u16 && c != CR && c != LF;
            let mut a = i.min(t.len());
            while a > 0 && is_word(t[a - 1]) {
                a -= 1;
            }
            let mut b = i.min(t.len());
            while b < t.len() && is_word(t[b]) {
                b += 1;
            }
            get(hwnd, |e| e.anchor = a);
            move_caret(hwnd, b, true);
            0
        }
        // Dragging extends the selection.
        0x0200 => {
            if crate::window::GetCapture() == hwnd {
                let (x, y) = ((lp & 0xFFFF) as i16 as i32, ((lp >> 16) & 0xFFFF) as i16 as i32);
                let i = char_at(hwnd, x, y);
                if get(hwnd, |e| e.caret) != Some(i) {
                    move_caret(hwnd, i, true);
                }
            }
            0
        }
        0x0202 => {
            if crate::window::GetCapture() == hwnd {
                crate::window::ReleaseCapture();
            }
            0
        }
        // The wheel: three lines a notch.
        0x020A if multi => {
            let delta = ((wp >> 16) & 0xFFFF) as i16 as i32;
            let lines = delta.abs() * 3 / 120;
            for _ in 0..lines.max(1) {
                crate::message::send(hwnd, 0x0115, if delta > 0 { 0 } else { 1 }, 0);
            }
            0
        }
        0x0007 => {
            // WM_SETFOCUS
            get(hwnd, |e| e.focused = true);
            let (_, _, lh) = caret_pos(hwnd);
            crate::keyboard::CreateCaret(hwnd, 0, 1, lh);
            place_caret(hwnd);
            crate::keyboard::ShowCaret(hwnd);
            crate::paint::invalidate(hwnd, None, true);
            notify(hwnd, EN_SETFOCUS);
            0
        }
        0x0008 => {
            get(hwnd, |e| e.focused = false);
            crate::keyboard::DestroyCaret();
            crate::paint::invalidate(hwnd, None, true);
            notify(hwnd, EN_KILLFOCUS);
            0
        }
        0x000A => {
            crate::paint::invalidate(hwnd, None, true);
            0
        }
        controls::WM_GETDLGCODE => {
            let mut code = DLGC_WANTCHARS | DLGC_HASSETSEL | DLGC_WANTARROWS;
            if multi {
                code |= DLGC_WANTALLKEYS;
                // Return goes to the dialog unless ES_WANTRETURN.
                if style & ES_WANTRETURN == 0 && lp != 0 {
                    let m = unsafe { &*(lp as *const nt_abi::Msg) };
                    if m.message == 0x0100 && m.wparam == 0x0D {
                        code &= !DLGC_WANTALLKEYS;
                    }
                }
            }
            code
        }
        0x0115 => {
            // WM_VSCROLL
            let dc = measure_dc(hwnd);
            let t = text(hwnd);
            let n = lines(hwnd, &t, dc.0).len();
            let lh = line_height(dc.0);
            release_dc(hwnd, dc);
            let visible = ((controls::client(hwnd)[3] - 2) / lh).max(1) as usize;
            let top = get(hwnd, |e| e.top).unwrap_or(0);
            let new = match wp & 0xFFFF {
                0 => top.saturating_sub(1),
                1 => top + 1,
                2 => top.saturating_sub(visible),
                3 => top + visible,
                4 | 5 => ((wp >> 16) & 0xFFFF) as usize,
                _ => top,
            }
            .min(n.saturating_sub(visible));
            get(hwnd, |e| e.top = new);
            update_scrollbar(hwnd, n, visible);
            place_caret(hwnd);
            crate::paint::invalidate(hwnd, None, true);
            0
        }
        // EM_GETSEL
        0x00B0 => {
            let (a, b) = sel(hwnd);
            unsafe {
                if wp != 0 {
                    *(wp as *mut u32) = a as u32;
                }
                if lp != 0 {
                    *(lp as *mut u32) = b as u32;
                }
            }
            ((b.min(0xFFFF) as i64) << 16) | a.min(0xFFFF) as i64
        }
        // EM_SETSEL: start -1 deselects; end -1 is the end.
        0x00B1 => {
            let n = text(hwnd).len();
            let (s, e) = (wp as i32, lp as i32);
            get(hwnd, |ed| {
                if s < 0 {
                    ed.anchor = ed.caret;
                } else {
                    ed.anchor = (s as usize).min(n);
                    ed.caret = if e < 0 { n } else { (e as usize).min(n) };
                }
            });
            scroll_to_caret(hwnd);
            crate::paint::invalidate(hwnd, None, true);
            0
        }
        // EM_GETRECT
        0x00B2 => {
            if lp != 0 {
                unsafe { *(lp as *mut [i32; 4]) = controls::client(hwnd) };
            }
            0
        }
        // EM_SCROLLCARET
        0x00B7 => {
            scroll_to_caret(hwnd);
            1
        }
        0x00B8 => get(hwnd, |e| e.modified as i64).unwrap_or(0),
        0x00B9 => {
            get(hwnd, |e| e.modified = wp != 0);
            0
        }
        // EM_GETLINECOUNT
        0x00BA => {
            let dc = measure_dc(hwnd);
            let n = lines(hwnd, &text(hwnd), dc.0).len();
            release_dc(hwnd, dc);
            n as i64
        }
        // EM_LINEINDEX: line -1 is the caret's.
        0x00BB => {
            let dc = measure_dc(hwnd);
            let t = text(hwnd);
            let ls = lines(hwnd, &t, dc.0);
            release_dc(hwnd, dc);
            let line = if wp as i32 == -1 { line_of(&ls, get(hwnd, |e| e.caret).unwrap_or(0)) } else { wp as usize };
            ls.get(line).map_or(-1, |l| l.0 as i64)
        }
        // EM_LINELENGTH: of the line holding character `wp`.
        0x00C1 => {
            let dc = measure_dc(hwnd);
            let t = text(hwnd);
            let ls = lines(hwnd, &t, dc.0);
            release_dc(hwnd, dc);
            if !multi {
                return t.len() as i64;
            }
            let i = if wp as i32 == -1 { get(hwnd, |e| e.caret).unwrap_or(0) } else { wp as usize };
            let l = ls[line_of(&ls, i.min(t.len()))];
            (l.1 - l.0) as i64
        }
        // EM_REPLACESEL
        0x00C2 => {
            let ins = if lp == 0 { Vec::new() } else { unsafe { crate::wstr(lp as *const u16) } };
            replace_sel(hwnd, &ins, false);
            0
        }
        // EM_GETLINE: the line into a buffer whose first word is its size.
        0x00C4 => {
            if lp == 0 {
                return 0;
            }
            let dc = measure_dc(hwnd);
            let t = text(hwnd);
            let ls = lines(hwnd, &t, dc.0);
            release_dc(hwnd, dc);
            let Some(&(s, e)) = ls.get(if multi { wp as usize } else { 0 }) else { return 0 };
            let cap = unsafe { *(lp as *const u16) } as usize;
            let n = (e - s).min(cap);
            unsafe { core::ptr::copy_nonoverlapping(t[s..].as_ptr(), lp as *mut u16, n) };
            n as i64
        }
        // EM_LIMITTEXT / EM_SETLIMITTEXT (0: the most)
        0x00C5 => {
            get(hwnd, |e| e.limit = if wp == 0 { 0x7FFF_FFFE } else { wp as usize });
            0
        }
        // EM_LINEFROMCHAR
        0x00C9 => {
            let dc = measure_dc(hwnd);
            let t = text(hwnd);
            let ls = lines(hwnd, &t, dc.0);
            release_dc(hwnd, dc);
            let i = if wp as i32 == -1 { get(hwnd, |e| e.caret).unwrap_or(0) } else { wp as usize };
            line_of(&ls, i.min(t.len())) as i64
        }
        // EM_SETPASSWORDCHAR, EM_GETPASSWORDCHAR
        0x00CC => {
            get(hwnd, |e| e.password = wp as u16);
            crate::paint::invalidate(hwnd, None, true);
            1
        }
        0x00D2 => get(hwnd, |e| e.password as i64).unwrap_or(0),
        // EM_SETWORDBREAKPROC, EM_GETWORDBREAKPROC
        0x00D0 => {
            get(hwnd, |e| e.word_break = lp as u64);
            0
        }
        0x00D1 => get(hwnd, |e| e.word_break as i64).unwrap_or(0),
        // EM_GETFIRSTVISIBLELINE
        0x00CE => get(hwnd, |e| e.top as i64).unwrap_or(0),
        // EM_SETREADONLY
        0x00CF => {
            if let Some(w) = state().get_mut(hwnd) {
                if wp != 0 { w.style |= ES_READONLY } else { w.style &= !ES_READONLY }
            }
            crate::paint::invalidate(hwnd, None, true);
            1
        }
        // EM_SETMARGINS, EM_GETMARGINS
        0x00D3 | 0x00D4 => 0,
        // EM_GETLIMITTEXT
        0x00D5 => get(hwnd, |e| e.limit as i64).unwrap_or(0),
        // EM_POSFROMCHAR
        0x00D6 => {
            let old = get(hwnd, |e| core::mem::replace(&mut e.caret, wp as usize)).unwrap_or(0);
            let (x, y, _) = caret_pos(hwnd);
            get(hwnd, |e| e.caret = old);
            ((y as u16 as i64) << 16) | x as u16 as i64
        }
        // EM_CHARFROMPOS
        0x00D7 => {
            let i = char_at(hwnd, (lp & 0xFFFF) as i16 as i32, ((lp >> 16) & 0xFFFF) as i16 as i32);
            let dc = measure_dc(hwnd);
            let line = line_of(&lines(hwnd, &text(hwnd), dc.0), i);
            release_dc(hwnd, dc);
            ((line as i64) << 16) | i as i64
        }
        // WM_CUT, WM_COPY (not from a password field), WM_PASTE, WM_CLEAR
        0x0300 | 0x0301 => {
            let (a, b) = sel(hwnd);
            let password = get(hwnd, |e| e.password).unwrap_or(0);
            if a < b && password == 0 {
                crate::clipboard::copy_text(hwnd, &text(hwnd)[a..b]);
            }
            if msg == 0x0300 && style & ES_READONLY == 0 {
                replace_sel(hwnd, &[], true);
            }
            0
        }
        0x0302 => {
            if style & ES_READONLY == 0 {
                let mut t = crate::clipboard::paste_text();
                if !multi {
                    // One line only: to the first line break.
                    if let Some(i) = t.iter().position(|&c| c == CR || c == LF) {
                        t.truncate(i);
                    }
                }
                replace_sel(hwnd, &t, true);
            }
            0
        }
        0x0303 => {
            if style & ES_READONLY == 0 {
                replace_sel(hwnd, &[], true);
            }
            0
        }
        // EM_UNDO / WM_UNDO: back to the text before the last change.
        0x00C7 | 0x0304 => {
            let Some((old, a, b)) = get(hwnd, |e| e.undo.take()).flatten() else { return 0 };
            let now = text(hwnd);
            set_text_raw(hwnd, old);
            get(hwnd, |e| {
                e.anchor = a;
                e.caret = b;
                e.modified = true;
                e.undo = Some((now, a, a));
            });
            changed(hwnd);
            1
        }
        0x00C6 => get(hwnd, |e| e.undo.is_some() as i64).unwrap_or(0),
        0x00CD => {
            get(hwnd, |e| e.undo = None);
            0
        }
        // EM_GETHANDLE: the text in the edit's moveable block (the same
        // handle each time; programs grow it and hand it back).
        0x00BD => {
            let t = text(hwnd);
            let size = (t.len() + 1) * 2;
            let old = get(hwnd, |e| e.local).unwrap_or(0);
            let h = if old != 0 && unsafe { LocalReAlloc(old, size, LMEM_MOVEABLE) } != 0 { old } else { unsafe { LocalAlloc(LMEM_MOVEABLE, size) } };
            let p = if h != 0 { unsafe { LocalLock(h) } } else { core::ptr::null_mut() };
            if !p.is_null() {
                unsafe {
                    core::ptr::copy_nonoverlapping(t.as_ptr(), p, t.len());
                    *p.add(t.len()) = 0;
                    LocalUnlock(h);
                }
            }
            get(hwnd, |e| e.local = h);
            h as i64
        }
        // EM_SETHANDLE: the text from a block the edit now owns.
        0x00BC => {
            let p = wp;
            // Up to the NUL, within the block.
            let mut t = Vec::new();
            let data = if p == 0 { core::ptr::null_mut() } else { unsafe { LocalLock(p) } };
            if !data.is_null() {
                let n = unsafe { LocalSize(p) } / 2;
                let s = unsafe { core::slice::from_raw_parts(data, n) };
                t = s[..s.iter().position(|&c| c == 0).unwrap_or(n)].to_vec();
                unsafe { LocalUnlock(p) };
            }
            let old = get(hwnd, |e| core::mem::replace(&mut e.local, p)).unwrap_or(0);
            if old != 0 && old != p {
                unsafe { LocalFree(old) };
            }
            set_text_raw(hwnd, t);
            get(hwnd, |e| {
                e.anchor = 0;
                e.caret = 0;
                e.modified = false;
                e.undo = None;
                e.top = 0;
                e.scroll_x = 0;
            });
            scroll_to_caret(hwnd);
            crate::paint::invalidate(hwnd, None, true);
            0
        }
        // EM_SETEXTENDEDSTYLE (wp the mask, 0 for all), EM_GETEXTENDEDSTYLE
        0x150A => {
            let mask = if wp == 0 { u32::MAX } else { wp as u32 };
            get(hwnd, |e| e.ex_style = (e.ex_style & !mask) | (lp as u32 & mask));
            0
        }
        0x150B => get(hwnd, |e| e.ex_style as i64).unwrap_or(0),
        // EM_SETENDOFLINE, EM_GETENDOFLINE (0 detect, 1 CRLF, 2 CR, 3 LF)
        0x150C => {
            get(hwnd, |e| e.eol = if wp == 0 { EC_ENDOFLINE_CRLF } else { wp as u32 });
            0
        }
        0x150D => get(hwnd, |e| e.eol as i64).unwrap_or(EC_ENDOFLINE_CRLF as i64),
        // EM_ENABLESEARCHWEB, EM_SEARCHWEB: no web search.
        0x150E | 0x150F => 0,
        // EM_SETCARETINDEX, EM_GETCARETINDEX
        0x1511 => {
            let n = text(hwnd).len();
            move_caret(hwnd, (wp as usize).min(n), false);
            1
        }
        0x1512 => get(hwnd, |e| e.caret as i64).unwrap_or(0),
        // EM_FILELINEFROMCHAR, EM_FILELINEINDEX, EM_FILELINELENGTH: lines
        // between line breaks, whatever the wrapping (-1: the caret's).
        0x1513 | 0x1514 | 0x1515 => {
            let t = text(hwnd);
            let ls = file_lines(&t);
            let caret = get(hwnd, |e| e.caret).unwrap_or(0);
            let at = |i: usize| line_of(&ls, i.min(t.len()));
            match msg {
                0x1513 => at(if wp as i64 == -1 { caret } else { wp as usize }) as i64,
                0x1514 => {
                    let line = if wp as i64 == -1 { at(caret) } else { wp as usize };
                    ls.get(line).map_or(-1, |l| l.0 as i64)
                }
                _ => {
                    let l = ls[at(if wp as i64 == -1 { caret } else { wp as usize })];
                    (l.1 - l.0) as i64
                }
            }
        }
        // EM_GETFILELINE: line `wp` into a buffer whose first word is its size.
        0x1516 => {
            if lp == 0 {
                return 0;
            }
            let t = text(hwnd);
            let Some(&(s, e)) = file_lines(&t).get(wp as usize) else { return 0 };
            let cap = unsafe { *(lp as *const u16) } as usize;
            let n = (e - s).min(cap);
            unsafe { core::ptr::copy_nonoverlapping(t[s..].as_ptr(), lp as *mut u16, n) };
            n as i64
        }
        // EM_GETFILELINECOUNT
        0x1517 => file_lines(&text(hwnd)).len() as i64,
        // EM_GETZOOM (always 1:1), EM_SETZOOM (no zooming yet)
        0x04E0 => {
            for p in [wp, lp as u64] {
                if p != 0 {
                    unsafe { *(p as *mut i32) = 1 };
                }
            }
            1
        }
        0x04E1 => 0,
        // EM_FMTLINES, EM_SETTABSTOPS: accepted.
        0x00C8 | 0x00CB => 1,
        _ => def_window_proc(hwnd, msg, wp, lp, true),
    }
}
