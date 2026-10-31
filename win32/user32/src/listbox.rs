//! ListBox and ComboBox. A list box keeps items (text and data), sorted
//! with LBS_SORT, single or multiple selection, scrolling (its scroll bar
//! shows only when the items overflow, as on Windows), owner-drawn items,
//! and the LB_* messages and LBN_* notifications. A combo box is a closed
//! field plus, when dropped down, a "ComboLBox" popup window with its list
//! (a frameless kernel window that never takes the focus).

use alloc::vec::Vec;

use crate::controls::{self, notify, Kind, DLGC_WANTARROWS, DLGC_WANTCHARS};
use crate::message::{def_window_proc, send};
use crate::{state, Hwnd};

const LB_ERR: i64 = -1;

const LBS_NOTIFY: u32 = 0x0001;
const LBS_SORT: u32 = 0x0002;
const LBS_MULTIPLESEL: u32 = 0x0008;
const LBS_OWNERDRAWFIXED: u32 = 0x0010;
const LBS_OWNERDRAWVARIABLE: u32 = 0x0020;
const LBS_HASSTRINGS: u32 = 0x0040;
const LBS_DISABLENOSCROLL: u32 = 0x1000;
const LBS_EXTENDEDSEL: u32 = 0x0800;

const LBN_SELCHANGE: u32 = 1;
const LBN_DBLCLK: u32 = 2;
const LBN_SETFOCUS: u32 = 4;
const LBN_KILLFOCUS: u32 = 5;

/// Items, selection, scrolling.
pub struct List {
    pub items: Vec<(Vec<u16>, u64)>,
    pub sel: Option<usize>,
    pub selected: Vec<bool>,
    pub caret: usize,
    pub top: usize,
    pub item_h: i32,
    pub focused: bool,
    /// It was created with WS_VSCROLL (shown when the items overflow).
    pub wants_vscroll: bool,
}

impl List {
    fn new(wants_vscroll: bool) -> Self {
        List { items: Vec::new(), sel: None, selected: Vec::new(), caret: 0, top: 0, item_h: 16, focused: false, wants_vscroll }
    }
}

fn lower(c: u16) -> u16 {
    if (b'A' as u16..=b'Z' as u16).contains(&c) { c + 32 } else { c }
}

fn cmp_ci(a: &[u16], b: &[u16]) -> core::cmp::Ordering {
    a.iter().map(|&c| lower(c)).cmp(b.iter().map(|&c| lower(c)))
}

fn starts_ci(s: &[u16], prefix: &[u16]) -> bool {
    s.len() >= prefix.len() && s.iter().zip(prefix).all(|(&a, &b)| lower(a) == lower(b))
}

/// The item text a string argument means (A or W already W here).
fn arg_text(lp: i64, has_strings: bool) -> (Vec<u16>, u64) {
    if has_strings && lp != 0 { (unsafe { crate::wstr(lp as *const u16) }, 0) } else { (Vec::new(), lp as u64) }
}

/// Item operations common to list and combo boxes, on `items`/`sel`.
mod items {
    use super::*;

    pub fn add(items: &mut Vec<(Vec<u16>, u64)>, sel: &mut Option<usize>, item: (Vec<u16>, u64), sorted: bool, at: Option<usize>) -> i64 {
        let i = match at {
            Some(i) if i <= items.len() => i,
            Some(_) => return LB_ERR,
            None if sorted => items.iter().position(|e| cmp_ci(&e.0, &item.0) == core::cmp::Ordering::Greater).unwrap_or(items.len()),
            None => items.len(),
        };
        items.insert(i, item);
        if let Some(s) = sel.as_mut() {
            if *s >= i {
                *s += 1;
            }
        }
        i as i64
    }

    pub fn delete(items: &mut Vec<(Vec<u16>, u64)>, sel: &mut Option<usize>, i: usize) -> i64 {
        if i >= items.len() {
            return LB_ERR;
        }
        items.remove(i);
        *sel = match *sel {
            Some(s) if s == i => None,
            Some(s) if s > i => Some(s - 1),
            other => other,
        };
        items.len() as i64
    }

    /// First item after `start` (-1: from the beginning), wrapping, whose
    /// text starts with (or is, `exact`) `s`, case-insensitively.
    pub fn find(items: &[(Vec<u16>, u64)], start: i64, s: &[u16], exact: bool) -> i64 {
        let n = items.len();
        if n == 0 {
            return LB_ERR;
        }
        let first = if start < 0 || start as usize >= n { 0 } else { start as usize + 1 };
        for k in 0..n {
            let i = (first + k) % n;
            let t = &items[i].0;
            let hit = if exact { cmp_ci(t, s) == core::cmp::Ordering::Equal } else { starts_ci(t, s) };
            if hit {
                return i as i64;
            }
        }
        LB_ERR
    }

    pub fn text(items: &[(Vec<u16>, u64)], i: usize, out: i64) -> i64 {
        let Some(item) = items.get(i) else { return LB_ERR };
        if out != 0 {
            unsafe {
                core::ptr::copy_nonoverlapping(item.0.as_ptr(), out as *mut u16, item.0.len());
                *(out as *mut u16).add(item.0.len()) = 0;
            }
        }
        item.0.len() as i64
    }
}

// -----| ListBox |-----

fn get<R>(hwnd: Hwnd, f: impl FnOnce(&mut List) -> R) -> Option<R> {
    controls::with(hwnd, |c| match &mut c.kind {
        Kind::List(l) => Some(f(l)),
        _ => None,
    })
    .flatten()
}

fn font_height(hwnd: Hwnd) -> i32 {
    let hdc = crate::paint::GetDC(hwnd);
    let old = controls::select_font(hwnd, hdc);
    let mut tm = [0u8; 60];
    unsafe { crate::GetTextMetricsW(hdc, tm.as_mut_ptr()) };
    if old != 0 {
        unsafe { crate::SelectObject(hdc, old) };
    }
    crate::paint::ReleaseDC(hwnd, hdc);
    i32::from_le_bytes(tm[0..4].try_into().unwrap()).max(1)
}

fn visible_rows(hwnd: Hwnd) -> usize {
    let h = get(hwnd, |l| l.item_h).unwrap_or(16);
    (controls::client(hwnd)[3] / h.max(1)).max(1) as usize
}

/// Show the scroll bar if the items overflow, and set its range.
fn update_scroll(hwnd: Hwnd) {
    let Some((n, top, wants)) = get(hwnd, |l| (l.items.len(), l.top, l.wants_vscroll)) else { return };
    if !wants {
        return;
    }
    let rows = visible_rows(hwnd);
    let need = n > rows || controls::style(hwnd) & LBS_DISABLENOSCROLL != 0;
    let has = controls::style(hwnd) & crate::window::WS_VSCROLL != 0;
    if need != has {
        crate::scroll::ShowScrollBar(hwnd, 1, need as i32);
    }
    if need {
        let si = [28u32, 7, 0, n.saturating_sub(1) as u32, visible_rows(hwnd) as u32, top as u32, 0];
        unsafe { crate::scroll::SetScrollInfo(hwnd, 1, si.as_ptr(), 1) };
    }
}

fn changed(hwnd: Hwnd) {
    update_scroll(hwnd);
    crate::paint::invalidate(hwnd, None, true);
}

fn ensure_visible(hwnd: Hwnd, i: usize) {
    let rows = visible_rows(hwnd);
    get(hwnd, |l| {
        if i < l.top {
            l.top = i;
        } else if i >= l.top + rows {
            l.top = i + 1 - rows;
        }
    });
}

fn multi(style: u32) -> bool {
    style & (LBS_MULTIPLESEL | LBS_EXTENDEDSEL) != 0
}

fn paint_list(hwnd: Hwnd, hdc: u64) {
    let style = controls::style(hwnd);
    let enabled = state().get(hwnd).is_some_and(|w| w.enabled);
    let old = controls::select_font(hwnd, hdc);
    let brush = controls::ctl_color(hwnd, hdc, controls::WM_CTLCOLORLISTBOX);
    let r = controls::client(hwnd);
    crate::paint::FillRect(hdc, &r, brush);
    let Some((items, sel, selected, top, h, focused, caret)) =
        get(hwnd, |l| (l.items.clone(), l.sel, l.selected.clone(), l.top, l.item_h, l.focused, l.caret))
    else {
        return;
    };
    let owner = style & (LBS_OWNERDRAWFIXED | LBS_OWNERDRAWVARIABLE) != 0;
    unsafe { crate::SetBkMode(hdc, 1) };
    for (i, (text, data)) in items.iter().enumerate().skip(top) {
        let y = (i - top) as i32 * h;
        if y >= r[3] {
            break;
        }
        let row = [0, y, r[2], y + h];
        let is_sel = if multi(style) { selected.get(i).copied().unwrap_or(false) } else { sel == Some(i) };
        if owner {
            draw_item(hwnd, hdc, 2, i, *data, row, is_sel, focused && caret == i);
            continue;
        }
        if is_sel {
            crate::paint::FillRect(hdc, &row, crate::paint::GetSysColorBrush(13));
        }
        let color = if !enabled { 17 } else if is_sel { 14 } else { 8 };
        let old_c = unsafe { crate::SetTextColor(hdc, crate::paint::GetSysColor(color)) };
        unsafe { crate::TextOutW(hdc, 2, y, text.as_ptr(), text.len() as i32) };
        unsafe { crate::SetTextColor(hdc, old_c) };
        if focused && caret == i {
            controls::DrawFocusRect(hdc, &row);
        }
    }
    if old != 0 {
        unsafe { crate::SelectObject(hdc, old) };
    }
}

/// WM_DRAWITEM for an owner-drawn list item (ctl_type: ODT_LISTBOX 2,
/// ODT_COMBOBOX 3).
#[allow(clippy::too_many_arguments)]
fn draw_item(hwnd: Hwnd, hdc: u64, ctl_type: u32, i: usize, data: u64, r: [i32; 4], selected: bool, focus: bool) {
    let (parent, id) = state().get(hwnd).map_or((0, 0), |w| (w.parent, w.id));
    let mut dis = [0u8; 64];
    dis[0..4].copy_from_slice(&ctl_type.to_le_bytes());
    dis[4..8].copy_from_slice(&(id as u32).to_le_bytes());
    dis[8..12].copy_from_slice(&(i as u32).to_le_bytes());
    dis[12..16].copy_from_slice(&1u32.to_le_bytes());
    dis[16..20].copy_from_slice(&((selected as u32) | (focus as u32) << 4).to_le_bytes());
    dis[24..32].copy_from_slice(&hwnd.to_le_bytes());
    dis[32..40].copy_from_slice(&hdc.to_le_bytes());
    for (k, v) in r.iter().enumerate() {
        dis[40 + k * 4..44 + k * 4].copy_from_slice(&v.to_le_bytes());
    }
    dis[56..64].copy_from_slice(&data.to_le_bytes());
    send(parent, 0x002B, id, dis.as_mut_ptr() as i64);
}

/// The user picked item `i` (click or key): select it and tell the
/// parent if it changed.
fn pick(hwnd: Hwnd, i: usize, toggle: bool) {
    let style = controls::style(hwnd);
    let changed_sel = get(hwnd, |l| {
        if i >= l.items.len() {
            return false;
        }
        l.caret = i;
        if multi(style) {
            l.selected.resize(l.items.len(), false);
            if toggle || style & LBS_MULTIPLESEL != 0 {
                l.selected[i] = !l.selected[i];
            } else {
                l.selected.iter_mut().for_each(|s| *s = false);
                l.selected[i] = true;
            }
            true
        } else {
            let c = l.sel != Some(i);
            l.sel = Some(i);
            c
        }
    })
    .unwrap_or(false);
    ensure_visible(hwnd, i);
    changed(hwnd);
    if changed_sel && style & LBS_NOTIFY != 0 {
        notify(hwnd, LBN_SELCHANGE);
    }
}

pub unsafe extern "system" fn listbox_proc(hwnd: Hwnd, msg: u32, wp: u64, lp: i64) -> i64 {
    if let Some(r) = controls::common(hwnd, msg, wp, lp) {
        if msg == controls::WM_SETFONT {
            let h = font_height(hwnd);
            get(hwnd, |l| l.item_h = h);
            update_scroll(hwnd);
        }
        return r;
    }
    let style = controls::style(hwnd);
    let has_strings = style & (LBS_OWNERDRAWFIXED | LBS_OWNERDRAWVARIABLE) == 0 || style & LBS_HASSTRINGS != 0;
    let sorted = style & LBS_SORT != 0;
    match msg {
        0x0081 => {
            // WM_NCCREATE: the scroll bar waits until it's needed.
            controls::border_to_edge(hwnd);
            let wants = style & crate::window::WS_VSCROLL != 0;
            if let Some(w) = state().get_mut(hwnd) {
                if style & LBS_DISABLENOSCROLL == 0 {
                    w.style &= !crate::window::WS_VSCROLL;
                }
            }
            controls::add(hwnd, Kind::List(List::new(wants)));
            let h = font_height(hwnd);
            get(hwnd, |l| l.item_h = h);
            def_window_proc(hwnd, msg, wp, lp, true)
        }
        0x000F => {
            let mut ps = [0u8; 72];
            let hdc = crate::paint::BeginPaint(hwnd, ps.as_mut_ptr());
            paint_list(hwnd, hdc);
            crate::paint::EndPaint(hwnd, ps.as_ptr());
            0
        }
        0x0014 => 1,
        0x0005 => {
            update_scroll(hwnd);
            0
        }
        controls::WM_GETDLGCODE => DLGC_WANTARROWS | DLGC_WANTCHARS,
        0x0201 | 0x0203 => {
            crate::window::SetFocus(hwnd);
            let y = ((lp >> 16) & 0xFFFF) as i16 as i32;
            let Some((top, h, n)) = get(hwnd, |l| (l.top, l.item_h, l.items.len())) else { return 0 };
            let i = top + (y / h.max(1)) as usize;
            if i < n {
                pick(hwnd, i, crate::keyboard::GetKeyState(0x11) < 0);
                if msg == 0x0203 && style & LBS_NOTIFY != 0 {
                    notify(hwnd, LBN_DBLCLK);
                }
            }
            0
        }
        0x0100 => {
            let Some((caret, n, sel)) = get(hwnd, |l| (l.caret, l.items.len(), l.sel)) else { return 0 };
            if n == 0 {
                return 0;
            }
            let cur = if multi(style) { caret } else { sel.unwrap_or(0) };
            let rows = visible_rows(hwnd);
            let to = match wp {
                0x26 => cur.saturating_sub(1),
                0x28 => (cur + 1).min(n - 1),
                0x21 => cur.saturating_sub(rows),
                0x22 => (cur + rows).min(n - 1),
                0x24 => 0,
                0x23 => n - 1,
                0x20 if multi(style) => {
                    pick(hwnd, caret, true);
                    return 0;
                }
                _ => return 0,
            };
            if multi(style) {
                get(hwnd, |l| l.caret = to);
                ensure_visible(hwnd, to);
                changed(hwnd);
            } else if sel != Some(to) || sel.is_none() {
                pick(hwnd, to, false);
            }
            0
        }
        0x0102 => {
            // A character selects the next item starting with it.
            let Some((items, sel)) = get(hwnd, |l| (l.items.clone(), l.sel)) else { return 0 };
            let i = items::find(&items, sel.map_or(-1, |s| s as i64), &[wp as u16], false);
            if i >= 0 && !multi(style) {
                pick(hwnd, i as usize, false);
            }
            0
        }
        0x0007 | 0x0008 => {
            get(hwnd, |l| l.focused = msg == 0x0007);
            crate::paint::invalidate(hwnd, None, true);
            if style & LBS_NOTIFY != 0 {
                notify(hwnd, if msg == 0x0007 { LBN_SETFOCUS } else { LBN_KILLFOCUS });
            }
            0
        }
        0x0115 => {
            let rows = visible_rows(hwnd);
            let Some((top, n)) = get(hwnd, |l| (l.top, l.items.len())) else { return 0 };
            let new = match wp & 0xFFFF {
                0 => top.saturating_sub(1),
                1 => top + 1,
                2 => top.saturating_sub(rows),
                3 => top + rows,
                4 | 5 => ((wp >> 16) & 0xFFFF) as usize,
                6 => 0,
                7 => n,
                _ => top,
            }
            .min(n.saturating_sub(rows));
            get(hwnd, |l| l.top = new);
            changed(hwnd);
            0
        }
        // LB_ADDSTRING, LB_INSERTSTRING
        0x0180 | 0x0181 => {
            let item = arg_text(lp, has_strings);
            let at = if msg == 0x0181 { Some(if wp as i32 == -1 { usize::MAX } else { wp as usize }) } else { None };
            let r = get(hwnd, |l| {
                let at = at.map(|a| a.min(l.items.len()));
                let i = items::add(&mut l.items, &mut l.sel, item, sorted && at.is_none(), at);
                if i >= 0 {
                    l.selected.insert((i as usize).min(l.selected.len()), false);
                }
                i
            })
            .unwrap_or(LB_ERR);
            changed(hwnd);
            r
        }
        // LB_DELETESTRING
        0x0182 => {
            let r = get(hwnd, |l| {
                if (wp as usize) < l.selected.len() {
                    l.selected.remove(wp as usize);
                }
                items::delete(&mut l.items, &mut l.sel, wp as usize)
            })
            .unwrap_or(LB_ERR);
            changed(hwnd);
            r
        }
        // LB_RESETCONTENT
        0x0184 => {
            get(hwnd, |l| {
                l.items.clear();
                l.selected.clear();
                l.sel = None;
                l.top = 0;
                l.caret = 0;
            });
            changed(hwnd);
            0
        }
        // LB_SETSEL (multiple selection): wp select, lp index or -1.
        0x0185 => {
            let r = get(hwnd, |l| {
                l.selected.resize(l.items.len(), false);
                if lp as i32 == -1 {
                    l.selected.iter_mut().for_each(|s| *s = wp != 0);
                    0
                } else if (lp as usize) < l.items.len() {
                    l.selected[lp as usize] = wp != 0;
                    0
                } else {
                    LB_ERR
                }
            })
            .unwrap_or(LB_ERR);
            changed(hwnd);
            r
        }
        // LB_SETCURSEL: -1 clears (and answers LB_ERR).
        0x0186 => {
            let r = get(hwnd, |l| {
                if wp as i32 == -1 {
                    l.sel = None;
                    LB_ERR
                } else if (wp as usize) < l.items.len() {
                    l.sel = Some(wp as usize);
                    l.caret = wp as usize;
                    wp as i64
                } else {
                    LB_ERR
                }
            })
            .unwrap_or(LB_ERR);
            if r >= 0 {
                ensure_visible(hwnd, r as usize);
            }
            changed(hwnd);
            r
        }
        // LB_GETSEL
        0x0187 => get(hwnd, |l| {
            if wp as usize >= l.items.len() {
                LB_ERR
            } else if multi(style) {
                l.selected.get(wp as usize).copied().unwrap_or(false) as i64
            } else {
                (l.sel == Some(wp as usize)) as i64
            }
        })
        .unwrap_or(LB_ERR),
        // LB_GETCURSEL
        0x0188 => get(hwnd, |l| if multi(style) { l.caret as i64 } else { l.sel.map_or(LB_ERR, |s| s as i64) }).unwrap_or(LB_ERR),
        // LB_GETTEXT, LB_GETTEXTLEN
        0x0189 => get(hwnd, |l| items::text(&l.items, wp as usize, lp)).unwrap_or(LB_ERR),
        0x018A => get(hwnd, |l| l.items.get(wp as usize).map_or(LB_ERR, |i| i.0.len() as i64)).unwrap_or(LB_ERR),
        // LB_GETCOUNT
        0x018B => get(hwnd, |l| l.items.len() as i64).unwrap_or(0),
        // LB_SELECTSTRING
        0x018C => {
            let s = unsafe { crate::wstr(lp as *const u16) };
            let i = get(hwnd, |l| items::find(&l.items, wp as i32 as i64, &s, false)).unwrap_or(LB_ERR);
            if i >= 0 {
                get(hwnd, |l| {
                    l.sel = Some(i as usize);
                    l.caret = i as usize;
                });
                ensure_visible(hwnd, i as usize);
                changed(hwnd);
            }
            i
        }
        // LB_GETTOPINDEX, LB_SETTOPINDEX
        0x018E => get(hwnd, |l| l.top as i64).unwrap_or(0),
        0x0197 => {
            get(hwnd, |l| l.top = (wp as usize).min(l.items.len().saturating_sub(1)));
            changed(hwnd);
            0
        }
        // LB_FINDSTRING, LB_FINDSTRINGEXACT
        0x018F | 0x01A2 => {
            let s = unsafe { crate::wstr(lp as *const u16) };
            get(hwnd, |l| items::find(&l.items, wp as i32 as i64, &s, msg == 0x01A2)).unwrap_or(LB_ERR)
        }
        // LB_GETSELCOUNT, LB_GETSELITEMS
        0x0190 => {
            if !multi(style) {
                return LB_ERR;
            }
            get(hwnd, |l| l.selected.iter().filter(|&&s| s).count() as i64).unwrap_or(0)
        }
        0x0191 => {
            if !multi(style) {
                return LB_ERR;
            }
            let sel: Vec<usize> = get(hwnd, |l| l.selected.iter().enumerate().filter(|e| *e.1).map(|e| e.0).collect()).unwrap_or_default();
            let n = sel.len().min(wp as usize);
            for (k, i) in sel.iter().take(n).enumerate() {
                unsafe { *(lp as *mut i32).add(k) = *i as i32 };
            }
            n as i64
        }
        // LB_GETITEMRECT
        0x0198 => {
            let Some((top, h, n)) = get(hwnd, |l| (l.top, l.item_h, l.items.len())) else { return LB_ERR };
            if wp as usize >= n.max(1) {
                return LB_ERR;
            }
            let y = (wp as i32 - top as i32) * h;
            unsafe { *(lp as *mut [i32; 4]) = [0, y, controls::client(hwnd)[2], y + h] };
            1
        }
        // LB_GETITEMDATA, LB_SETITEMDATA
        0x0199 => get(hwnd, |l| l.items.get(wp as usize).map_or(LB_ERR, |i| i.1 as i64)).unwrap_or(LB_ERR),
        0x019A => get(hwnd, |l| match l.items.get_mut(wp as usize) {
            Some(i) => {
                i.1 = lp as u64;
                0
            }
            None => LB_ERR,
        })
        .unwrap_or(LB_ERR),
        // LB_SETCARETINDEX, LB_GETCARETINDEX
        0x019E => {
            get(hwnd, |l| l.caret = wp as usize);
            changed(hwnd);
            0
        }
        0x019F => get(hwnd, |l| l.caret as i64).unwrap_or(0),
        // LB_SETITEMHEIGHT, LB_GETITEMHEIGHT
        0x01A0 => {
            get(hwnd, |l| l.item_h = (lp as i32).max(1));
            changed(hwnd);
            0
        }
        0x01A1 => get(hwnd, |l| l.item_h as i64).unwrap_or(LB_ERR),
        // LB_INITSTORAGE
        0x01A8 => get(hwnd, |l| l.items.len() as i64).unwrap_or(0),
        // LB_ITEMFROMPOINT: index, and whether the point is outside.
        0x01A9 => {
            let y = ((lp >> 16) & 0xFFFF) as i16 as i32;
            let Some((top, h, n)) = get(hwnd, |l| (l.top, l.item_h, l.items.len())) else { return 0 };
            let i = top as i64 + (y / h.max(1)) as i64;
            if i < n as i64 { i } else { (1 << 16) | (n.saturating_sub(1) as i64) }
        }
        _ => def_window_proc(hwnd, msg, wp, lp, true),
    }
}

// -----| ComboBox |-----

const CBS_SIMPLE: u32 = 1;
const CBS_DROPDOWN: u32 = 2;
const CBS_DROPDOWNLIST: u32 = 3;
const CBS_OWNERDRAWFIXED: u32 = 0x10;
const CBS_OWNERDRAWVARIABLE: u32 = 0x20;
const CBS_SORT: u32 = 0x100;
const CBS_HASSTRINGS: u32 = 0x200;

const CB_ERR: i64 = -1;
const CBN_SELCHANGE: u32 = 1;
const CBN_SETFOCUS: u32 = 3;
const CBN_KILLFOCUS: u32 = 4;
const CBN_EDITCHANGE: u32 = 5;
const CBN_EDITUPDATE: u32 = 6;
const CBN_DROPDOWN: u32 = 7;
const CBN_CLOSEUP: u32 = 8;
const CBN_SELENDOK: u32 = 9;
const CBN_SELENDCANCEL: u32 = 10;

pub struct Combo {
    pub items: Vec<(Vec<u16>, u64)>,
    pub sel: Option<usize>,
    /// The drop-down list's popup window, while dropped.
    pub list: Hwnd,
    /// The edit field (CBS_SIMPLE, CBS_DROPDOWN).
    pub edit: Hwnd,
    pub focused: bool,
    pub item_h: i32,
    /// The height asked for at creation: closed field plus list.
    pub full_h: i32,
}

fn cget<R>(hwnd: Hwnd, f: impl FnOnce(&mut Combo) -> R) -> Option<R> {
    controls::with(hwnd, |c| match &mut c.kind {
        Kind::Combo(cb) => Some(f(cb)),
        _ => None,
    })
    .flatten()
}

fn combo_kind(style: u32) -> u32 {
    style & 3
}

/// The closed field's height for the font.
fn field_h(hwnd: Hwnd) -> i32 {
    font_height(hwnd) + 8
}

fn paint_combo(hwnd: Hwnd, hdc: u64) {
    let style = controls::style(hwnd);
    let enabled = state().get(hwnd).is_some_and(|w| w.enabled);
    let r = controls::client(hwnd);
    let old = controls::select_font(hwnd, hdc);
    let brush = controls::ctl_color(hwnd, hdc, controls::WM_CTLCOLOREDIT);
    crate::paint::FillRect(hdc, &r, brush);
    controls::edge(hdc, r, true);
    let btn = crate::window::SCROLL_SIZE;
    if combo_kind(style) != CBS_SIMPLE {
        let b = [r[2] - 2 - btn, r[1] + 2, r[2] - 2, r[3] - 2];
        controls::fill(hdc, b, 15);
        controls::edge(hdc, b, false);
        let (cx, cy) = ((b[0] + b[2]) / 2, (b[1] + b[3]) / 2);
        for i in 0..4 {
            controls::fill(hdc, [cx - 3 + i, cy - 1 + i, cx + 4 - i, cy + i], if enabled { 18 } else { 17 });
        }
    }
    if combo_kind(style) == CBS_DROPDOWNLIST {
        let Some((items, sel, focused)) = cget(hwnd, |c| (c.items.clone(), c.sel, c.focused)) else { return };
        let field = [r[0] + 3, r[1] + 3, r[2] - 3 - btn, r[3] - 3];
        if let Some(s) = sel {
            if style & (CBS_OWNERDRAWFIXED | CBS_OWNERDRAWVARIABLE) != 0 {
                draw_item(hwnd, hdc, 3, s, items[s].1, field, focused, focused);
            } else {
                if focused {
                    crate::paint::FillRect(hdc, &field, crate::paint::GetSysColorBrush(13));
                }
                let color = if !enabled { 17 } else if focused { 14 } else { 8 };
                let old_c = unsafe { crate::SetTextColor(hdc, crate::paint::GetSysColor(color)) };
                unsafe { crate::SetBkMode(hdc, 1) };
                let t = &items[s].0;
                let h = font_height(hwnd);
                unsafe { crate::TextOutW(hdc, field[0] + 1, (field[1] + field[3] - h) / 2, t.as_ptr(), t.len() as i32) };
                unsafe { crate::SetTextColor(hdc, old_c) };
            }
        }
        if focused {
            controls::DrawFocusRect(hdc, &field);
        }
    }
    if old != 0 {
        unsafe { crate::SelectObject(hdc, old) };
    }
}

/// Show the list. CBN_DROPDOWN first.
fn drop_down(hwnd: Hwnd) {
    if cget(hwnd, |c| c.list != 0) != Some(false) {
        return;
    }
    notify(hwnd, CBN_DROPDOWN);
    let mut wr = [0i32; 4];
    crate::window::GetWindowRect(hwnd, &mut wr);
    let Some((n, h, full)) = cget(hwnd, |c| (c.items.len(), c.item_h, c.full_h)) else { return };
    let list_h = ((n.max(1) as i32) * h + 2).min((full - (wr[3] - wr[1])).max(h + 2));
    let class: Vec<u16> = "ComboLBox\0".encode_utf16().collect();
    let list = unsafe {
        crate::window::CreateWindowExW(0, class.as_ptr(), core::ptr::null(), 0x8080_0000, wr[0], wr[3], wr[2] - wr[0], list_h, hwnd, 0, 0, hwnd)
    };
    cget(hwnd, |c| c.list = list);
    LIST_OWNER.lock().push((list, hwnd));
    crate::window::ShowWindow(list, 8);
}

/// Hide the list: CBN_SELENDOK or CBN_SELENDCANCEL, then CBN_CLOSEUP.
fn close_up(hwnd: Hwnd, ok: bool) {
    let list = cget(hwnd, |c| core::mem::replace(&mut c.list, 0)).unwrap_or(0);
    if list == 0 {
        return;
    }
    notify(hwnd, if ok { CBN_SELENDOK } else { CBN_SELENDCANCEL });
    LIST_OWNER.lock().retain(|e| e.0 != list);
    crate::window::DestroyWindow(list);
    notify(hwnd, CBN_CLOSEUP);
}

/// Select item `i` as the user did.
fn combo_pick(hwnd: Hwnd, i: usize) {
    let changed = cget(hwnd, |c| {
        let ch = c.sel != Some(i) && i < c.items.len();
        if i < c.items.len() {
            c.sel = Some(i);
        }
        ch
    })
    .unwrap_or(false);
    sync_edit(hwnd);
    crate::paint::invalidate(hwnd, None, true);
    if changed {
        notify(hwnd, CBN_SELCHANGE);
    }
}

/// The edit field shows the selected item.
fn sync_edit(hwnd: Hwnd) {
    let Some((edit, text)) = cget(hwnd, |c| (c.edit, c.sel.and_then(|s| c.items.get(s)).map(|i| i.0.clone()))) else { return };
    if edit != 0 {
        if let Some(mut t) = text {
            t.push(0);
            unsafe { crate::window::SetWindowTextW(edit, t.as_ptr()) };
        }
    }
}

/// The popup lists and the combo boxes they belong to.
static LIST_OWNER: crate::dllrt::Lock<Vec<(Hwnd, Hwnd)>> = crate::dllrt::Lock::new(Vec::new());

pub unsafe extern "system" fn combo_proc(hwnd: Hwnd, msg: u32, wp: u64, lp: i64) -> i64 {
    if let Some(r) = controls::common(hwnd, msg, wp, lp) {
        if msg == controls::WM_SETFONT {
            let h = font_height(hwnd);
            cget(hwnd, |c| c.item_h = h);
            let edit = cget(hwnd, |c| c.edit).unwrap_or(0);
            if edit != 0 {
                send(edit, controls::WM_SETFONT, wp, lp);
            }
        }
        return r;
    }
    let style = controls::style(hwnd);
    let kind = combo_kind(style);
    let has_strings = style & (CBS_OWNERDRAWFIXED | CBS_OWNERDRAWVARIABLE) == 0 || style & CBS_HASSTRINGS != 0;
    match msg {
        0x0081 => {
            // WM_NCCREATE: its scroll bars belong to the list; it has
            // strings unless owner-drawn.
            if let Some(w) = state().get_mut(hwnd) {
                w.style &= !(crate::window::WS_VSCROLL | crate::window::WS_HSCROLL);
                if w.style & (CBS_OWNERDRAWFIXED | CBS_OWNERDRAWVARIABLE) == 0 {
                    w.style |= CBS_HASSTRINGS;
                }
            }
            let full = if lp != 0 { unsafe { *((lp as usize + 32) as *const i32) } } else { 100 };
            controls::add(hwnd, Kind::Combo(Combo { items: Vec::new(), sel: None, list: 0, edit: 0, focused: false, item_h: 16, full_h: full }));
            let h = font_height(hwnd);
            cget(hwnd, |c| c.item_h = h);
            def_window_proc(hwnd, msg, wp, lp, true)
        }
        0x0001 => {
            // WM_CREATE: the closed field's height; an edit field.
            if kind != CBS_SIMPLE {
                let w = controls::client(hwnd)[2];
                // SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOREDRAW
                crate::window::set_window_pos(hwnd, 0, 0, 0, w, field_h(hwnd), 0x2 | 0x4 | 0x10 | 0x8);
            }
            if kind == CBS_DROPDOWN || kind == CBS_SIMPLE {
                let r = controls::client(hwnd);
                let class: Vec<u16> = "Edit\0".encode_utf16().collect();
                let edit = unsafe {
                    crate::window::CreateWindowExW(4, class.as_ptr(), core::ptr::null(), 0x5000_0080, 3, 3, r[2] - 6 - crate::window::SCROLL_SIZE, field_h(hwnd) - 6, hwnd, 1001, 0, 0)
                };
                cget(hwnd, |c| c.edit = edit);
            }
            0
        }
        0x000F => {
            let mut ps = [0u8; 72];
            let hdc = crate::paint::BeginPaint(hwnd, ps.as_mut_ptr());
            paint_combo(hwnd, hdc);
            crate::paint::EndPaint(hwnd, ps.as_ptr());
            0
        }
        0x0014 => 1,
        controls::WM_GETDLGCODE => {
            // While dropped, Return and Escape close the list.
            let dropped = cget(hwnd, |c| c.list != 0).unwrap_or(false);
            DLGC_WANTARROWS | DLGC_WANTCHARS | if dropped { controls::DLGC_WANTALLKEYS } else { 0 }
        }
        0x0201 | 0x0203 => {
            crate::window::SetFocus(hwnd);
            if cget(hwnd, |c| c.list != 0).unwrap_or(false) {
                close_up(hwnd, false);
            } else {
                drop_down(hwnd);
            }
            0
        }
        0x0100 => {
            let Some((sel, n, dropped)) = cget(hwnd, |c| (c.sel, c.items.len(), c.list != 0)) else { return 0 };
            match wp {
                // F4: the list.
                0x73 => {
                    if dropped { close_up(hwnd, true) } else { drop_down(hwnd) }
                }
                0x0D if dropped => close_up(hwnd, true),
                0x1B if dropped => close_up(hwnd, false),
                0x26 | 0x28 | 0x24 | 0x23 if n > 0 => {
                    let cur = sel.unwrap_or(0);
                    let to = match wp {
                        0x26 => cur.saturating_sub(if sel.is_some() { 1 } else { 0 }),
                        0x28 => if sel.is_some() { (cur + 1).min(n - 1) } else { 0 },
                        0x24 => 0,
                        _ => n - 1,
                    };
                    combo_pick(hwnd, to);
                    let list = cget(hwnd, |c| c.list).unwrap_or(0);
                    if list != 0 {
                        crate::paint::invalidate(list, None, true);
                    }
                }
                _ => {}
            }
            0
        }
        0x0104 if wp == 0x28 || wp == 0x26 => {
            // Alt+Down / Alt+Up: the list.
            if cget(hwnd, |c| c.list != 0).unwrap_or(false) { close_up(hwnd, true) } else { drop_down(hwnd) }
            0
        }
        0x0102 => {
            if kind == CBS_DROPDOWNLIST {
                let Some((items, sel)) = cget(hwnd, |c| (c.items.clone(), c.sel)) else { return 0 };
                let i = items::find(&items, sel.map_or(-1, |s| s as i64), &[wp as u16], false);
                if i >= 0 {
                    combo_pick(hwnd, i as usize);
                }
            }
            0
        }
        0x0007 => {
            cget(hwnd, |c| c.focused = true);
            let edit = cget(hwnd, |c| c.edit).unwrap_or(0);
            if edit != 0 {
                crate::window::SetFocus(edit);
            }
            crate::paint::invalidate(hwnd, None, true);
            notify(hwnd, CBN_SETFOCUS);
            0
        }
        0x0008 => {
            cget(hwnd, |c| c.focused = false);
            close_up(hwnd, false);
            crate::paint::invalidate(hwnd, None, true);
            notify(hwnd, CBN_KILLFOCUS);
            0
        }
        0x0111 => {
            // From the edit field.
            match (wp >> 16) as u32 {
                0x0300 => notify(hwnd, CBN_EDITCHANGE),
                0x0400 => notify(hwnd, CBN_EDITUPDATE),
                _ => {}
            }
            0
        }
        0x000D | 0x000E => {
            // WM_GETTEXT / WM_GETTEXTLENGTH: the selection (or the field).
            let (edit, text) = cget(hwnd, |c| (c.edit, c.sel.and_then(|s| c.items.get(s)).map(|i| i.0.clone()).unwrap_or_default())).unwrap_or_default();
            if edit != 0 {
                return send(edit, msg, wp, lp);
            }
            if msg == 0x000E {
                return text.len() as i64;
            }
            unsafe { crate::copy_out(&text, lp as *mut u16, wp as usize) as i64 }
        }
        0x000C => {
            let edit = cget(hwnd, |c| c.edit).unwrap_or(0);
            if edit != 0 {
                return send(edit, msg, wp, lp);
            }
            let s = if lp == 0 { Vec::new() } else { unsafe { crate::wstr(lp as *const u16) } };
            let i = cget(hwnd, |c| items::find(&c.items, -1, &s, true)).unwrap_or(CB_ERR);
            if i >= 0 {
                cget(hwnd, |c| c.sel = Some(i as usize));
                crate::paint::invalidate(hwnd, None, true);
                1
            } else {
                CB_ERR
            }
        }
        // CB_GETEDITSEL, CB_LIMITTEXT, CB_SETEDITSEL: to the edit field.
        0x0140 | 0x0141 | 0x0142 => {
            let edit = cget(hwnd, |c| c.edit).unwrap_or(0);
            if edit == 0 {
                return CB_ERR;
            }
            let m = match msg {
                0x0140 => 0x00B0,
                0x0141 => 0x00C5,
                _ => 0x00B1,
            };
            let (w, l) = if msg == 0x0142 { ((lp & 0xFFFF) as i16 as u64, (lp >> 16) as i16 as i64) } else { (wp, lp) };
            send(edit, m, w, l);
            1
        }
        // CB_ADDSTRING, CB_INSERTSTRING
        0x0143 | 0x014A => {
            let item = arg_text(lp, has_strings);
            let at = if msg == 0x014A { Some(if wp as i32 == -1 { usize::MAX } else { wp as usize }) } else { None };
            cget(hwnd, |c| {
                let at = at.map(|a| a.min(c.items.len()));
                items::add(&mut c.items, &mut c.sel, item, style & CBS_SORT != 0 && at.is_none(), at)
            })
            .unwrap_or(CB_ERR)
        }
        // CB_DELETESTRING
        0x0144 => {
            let r = cget(hwnd, |c| items::delete(&mut c.items, &mut c.sel, wp as usize)).unwrap_or(CB_ERR);
            crate::paint::invalidate(hwnd, None, true);
            r
        }
        // CB_GETCOUNT, CB_GETCURSEL
        0x0146 => cget(hwnd, |c| c.items.len() as i64).unwrap_or(0),
        0x0147 => cget(hwnd, |c| c.sel.map_or(CB_ERR, |s| s as i64)).unwrap_or(CB_ERR),
        // CB_GETLBTEXT, CB_GETLBTEXTLEN
        0x0148 => cget(hwnd, |c| items::text(&c.items, wp as usize, lp)).unwrap_or(CB_ERR),
        0x0149 => cget(hwnd, |c| c.items.get(wp as usize).map_or(CB_ERR, |i| i.0.len() as i64)).unwrap_or(CB_ERR),
        // CB_RESETCONTENT
        0x014B => {
            cget(hwnd, |c| {
                c.items.clear();
                c.sel = None;
            });
            crate::paint::invalidate(hwnd, None, true);
            0
        }
        // CB_FINDSTRING, CB_FINDSTRINGEXACT
        0x014C | 0x0158 => {
            let s = unsafe { crate::wstr(lp as *const u16) };
            cget(hwnd, |c| items::find(&c.items, wp as i32 as i64, &s, msg == 0x0158)).unwrap_or(CB_ERR)
        }
        // CB_SELECTSTRING
        0x014D => {
            let s = unsafe { crate::wstr(lp as *const u16) };
            let i = cget(hwnd, |c| items::find(&c.items, wp as i32 as i64, &s, false)).unwrap_or(CB_ERR);
            if i >= 0 {
                cget(hwnd, |c| c.sel = Some(i as usize));
                sync_edit(hwnd);
                crate::paint::invalidate(hwnd, None, true);
            }
            i
        }
        // CB_SETCURSEL: -1 clears (and answers CB_ERR).
        0x014E => {
            let r = cget(hwnd, |c| {
                if (wp as i32) < 0 || wp as usize >= c.items.len() {
                    c.sel = None;
                    CB_ERR
                } else {
                    c.sel = Some(wp as usize);
                    wp as i64
                }
            })
            .unwrap_or(CB_ERR);
            sync_edit(hwnd);
            crate::paint::invalidate(hwnd, None, true);
            r
        }
        // CB_SHOWDROPDOWN
        0x014F => {
            if wp != 0 { drop_down(hwnd) } else { close_up(hwnd, false) }
            1
        }
        // CB_GETITEMDATA, CB_SETITEMDATA
        0x0150 => cget(hwnd, |c| c.items.get(wp as usize).map_or(CB_ERR, |i| i.1 as i64)).unwrap_or(CB_ERR),
        0x0151 => cget(hwnd, |c| match c.items.get_mut(wp as usize) {
            Some(i) => {
                i.1 = lp as u64;
                0
            }
            None => CB_ERR,
        })
        .unwrap_or(CB_ERR),
        // CB_GETDROPPEDCONTROLRECT
        0x0152 => {
            let mut wr = [0i32; 4];
            crate::window::GetWindowRect(hwnd, &mut wr);
            let full = cget(hwnd, |c| c.full_h).unwrap_or(0);
            unsafe { *(lp as *mut [i32; 4]) = [wr[0], wr[1], wr[2], wr[1] + full] };
            1
        }
        // CB_SETITEMHEIGHT, CB_GETITEMHEIGHT
        0x0153 => {
            cget(hwnd, |c| c.item_h = (lp as i32).max(1));
            0
        }
        0x0154 => cget(hwnd, |c| c.item_h as i64).unwrap_or(CB_ERR),
        // CB_GETDROPPEDSTATE
        0x0157 => cget(hwnd, |c| (c.list != 0) as i64).unwrap_or(0),
        // CB_SETEXTENDEDUI, CB_GETEXTENDEDUI, CB_INITSTORAGE
        0x0155 | 0x0156 => 0,
        0x0161 => cget(hwnd, |c| c.items.len() as i64).unwrap_or(0),
        _ => def_window_proc(hwnd, msg, wp, lp, true),
    }
}

// -----| the drop-down list |-----

fn owner_of(list: Hwnd) -> Hwnd {
    LIST_OWNER.lock().iter().find(|e| e.0 == list).map_or(0, |e| e.1)
}

pub unsafe extern "system" fn combo_list_proc(hwnd: Hwnd, msg: u32, wp: u64, lp: i64) -> i64 {
    let combo = owner_of(hwnd);
    match msg {
        0x000F => {
            let mut ps = [0u8; 72];
            let hdc = crate::paint::BeginPaint(hwnd, ps.as_mut_ptr());
            let r = controls::client(hwnd);
            controls::fill(hdc, r, 5);
            let font = controls::font(combo);
            let old = if font != 0 { unsafe { crate::SelectObject(hdc, font) } } else { 0 };
            if let Some((items, sel, h)) = cget(combo, |c| (c.items.clone(), c.sel, c.item_h)) {
                unsafe { crate::SetBkMode(hdc, 1) };
                for (i, (t, _)) in items.iter().enumerate() {
                    let y = i as i32 * h;
                    let row = [0, y, r[2], y + h];
                    let color = if sel == Some(i) {
                        crate::paint::FillRect(hdc, &row, crate::paint::GetSysColorBrush(13));
                        14
                    } else {
                        8
                    };
                    let oc = unsafe { crate::SetTextColor(hdc, crate::paint::GetSysColor(color)) };
                    unsafe { crate::TextOutW(hdc, 2, y, t.as_ptr(), t.len() as i32) };
                    unsafe { crate::SetTextColor(hdc, oc) };
                }
            }
            if old != 0 {
                unsafe { crate::SelectObject(hdc, old) };
            }
            crate::paint::EndPaint(hwnd, ps.as_ptr());
            0
        }
        0x0201 => {
            let y = ((lp >> 16) & 0xFFFF) as i16 as i32;
            let h = cget(combo, |c| c.item_h).unwrap_or(16);
            combo_pick(combo, (y / h.max(1)) as usize);
            close_up(combo, true);
            0
        }
        _ => def_window_proc(hwnd, msg, wp, lp, true),
    }
}
