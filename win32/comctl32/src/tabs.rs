//! The tab control ("SysTabControl32"): a row of tabs over a page area. One
//! row (TCS_MULTILINE tabs that don't fit stay on it); the owner shows the
//! pages, told by TCN_SELCHANGING / TCN_SELCHANGE.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use crate::dllrt::Lock;
use crate::Hwnd;

const TCM_FIRST: u32 = 0x1300;
const TCM_GETIMAGELIST: u32 = TCM_FIRST + 2;
const TCM_SETIMAGELIST: u32 = TCM_FIRST + 3;
const TCM_GETITEMCOUNT: u32 = TCM_FIRST + 4;
const TCM_GETITEMA: u32 = TCM_FIRST + 5;
const TCM_SETITEMA: u32 = TCM_FIRST + 6;
const TCM_INSERTITEMA: u32 = TCM_FIRST + 7;
const TCM_DELETEITEM: u32 = TCM_FIRST + 8;
const TCM_DELETEALLITEMS: u32 = TCM_FIRST + 9;
const TCM_GETITEMRECT: u32 = TCM_FIRST + 10;
const TCM_GETCURSEL: u32 = TCM_FIRST + 11;
const TCM_SETCURSEL: u32 = TCM_FIRST + 12;
const TCM_HITTEST: u32 = TCM_FIRST + 13;
const TCM_SETITEMEXTRA: u32 = TCM_FIRST + 14;
const TCM_ADJUSTRECT: u32 = TCM_FIRST + 40;
const TCM_SETITEMSIZE: u32 = TCM_FIRST + 41;
const TCM_SETPADDING: u32 = TCM_FIRST + 43;
const TCM_GETROWCOUNT: u32 = TCM_FIRST + 44;
const TCM_GETCURFOCUS: u32 = TCM_FIRST + 47;
const TCM_SETCURFOCUS: u32 = TCM_FIRST + 48;
const TCM_SETMINTABWIDTH: u32 = TCM_FIRST + 49;
const TCM_SETEXTENDEDSTYLE: u32 = TCM_FIRST + 52;
const TCM_GETEXTENDEDSTYLE: u32 = TCM_FIRST + 53;
const TCM_GETITEMW: u32 = TCM_FIRST + 60;
const TCM_SETITEMW: u32 = TCM_FIRST + 61;
const TCM_INSERTITEMW: u32 = TCM_FIRST + 62;

const TCIF_TEXT: u32 = 0x1;
const TCIF_IMAGE: u32 = 0x2;
const TCIF_PARAM: u32 = 0x8;
const TCIF_STATE: u32 = 0x10;

const TCS_FIXEDWIDTH: u32 = 0x400;

const TCN_KEYDOWN: i32 = -550;
const TCN_SELCHANGE: i32 = -551;
const TCN_SELCHANGING: i32 = -552;
const NM_CLICK: i32 = -2;
const NM_RCLICK: i32 = -5;

const TCHT_NOWHERE: u32 = 1;
const TCHT_ONITEMLABEL: u32 = 4;

#[derive(Clone)]
struct Item {
    text: Vec<u16>,
    image: i32,
    param: i64,
    state: u32,
}

#[derive(Default)]
struct Tabs {
    items: Vec<Item>,
    sel: i32,
    focus: i32,
    font: u64,
    images: u64,
    /// TCM_SETITEMSIZE, TCM_SETPADDING, TCM_SETMINTABWIDTH.
    size: (i32, i32),
    pad: (i32, i32),
    min_w: i32,
    ex: u32,
    has_focus: bool,
}

static TABS: Lock<BTreeMap<Hwnd, Tabs>> = Lock::new(BTreeMap::new());

fn with<R>(hwnd: Hwnd, f: impl FnOnce(&mut Tabs) -> R) -> Option<R> {
    TABS.lock().get_mut(&hwnd).map(f)
}

pub fn register_class() {
    crate::register("SysTabControl32", tab_proc as *const () as usize, true);
}

fn row_height(hwnd: Hwnd) -> i32 {
    let (set, font, images) = with(hwnd, |t| (t.size.1, t.font, t.images)).unwrap_or((0, 0, 0));
    if set > 0 {
        return set;
    }
    let mut h = crate::font_height(hwnd, font) + 7;
    if images != 0 {
        let (mut cx, mut cy) = (0, 0);
        unsafe { crate::imagelist::ImageList_GetIconSize(images, &mut cx, &mut cy) };
        h = h.max(cy + 4);
    }
    h
}

/// Each tab's rectangle along the top.
fn rects(hwnd: Hwnd) -> Vec<[i32; 4]> {
    let Some((items, font, images, size, pad, min_w)) = with(hwnd, |t| (t.items.clone(), t.font, t.images, t.size, t.pad, t.min_w)) else {
        return Vec::new();
    };
    let h = row_height(hwnd);
    let st = unsafe { crate::GetWindowLongPtrW(hwnd, -16) } as u32;
    let dc = unsafe { crate::GetDC(hwnd) };
    let old = unsafe { crate::SelectObject(dc, crate::font_or_default(font)) };
    let mut x = 2;
    let out = items
        .iter()
        .map(|it| {
            let w = if st & TCS_FIXEDWIDTH != 0 && size.0 > 0 {
                size.0
            } else {
                let (mut iw, mut cy) = (0, 0);
                if images != 0 && it.image >= 0 {
                    unsafe { crate::imagelist::ImageList_GetIconSize(images, &mut iw, &mut cy) };
                    iw += 3;
                }
                (crate::text_size(dc, &it.text).0 + iw + pad.0 * 2).max(min_w)
            };
            let r = [x, 2, x + w, 2 + h];
            x += w;
            r
        })
        .collect();
    unsafe {
        crate::SelectObject(dc, old);
        crate::ReleaseDC(hwnd, dc);
    }
    out
}

fn paint(hwnd: Hwnd, hdc: u64) {
    let mut c = [0i32; 4];
    unsafe { crate::GetClientRect(hwnd, &mut c) };
    let Some((items, sel, font, images, has_focus)) = with(hwnd, |t| (t.items.clone(), t.sel, t.font, t.images, t.has_focus)) else { return };
    unsafe { crate::FillRect(hdc, &c, crate::GetSysColorBrush(15)) };
    let h = row_height(hwnd);
    let top = 2 + h;
    let old = unsafe { crate::SelectObject(hdc, crate::font_or_default(font)) };
    unsafe {
        crate::SetBkMode(hdc, 1);
        crate::SetTextColor(hdc, crate::GetSysColor(18));
    }
    let (light, dark) = unsafe { (crate::GetSysColorBrush(20), crate::GetSysColorBrush(16)) };
    // The page's frame.
    unsafe {
        crate::FillRect(hdc, &[c[0], top, c[2], top + 1], light);
        crate::FillRect(hdc, &[c[0], top, c[0] + 1, c[3]], light);
        crate::FillRect(hdc, &[c[2] - 1, top, c[2], c[3]], dark);
        crate::FillRect(hdc, &[c[0], c[3] - 1, c[2], c[3]], dark);
    }
    for (i, r) in rects(hwnd).iter().enumerate() {
        let selected = i as i32 == sel;
        // The selected tab stands up and joins the page.
        let tr = if selected { [r[0] - 2, r[1] - 2, r[2] + 2, r[3] + 1] } else { *r };
        unsafe {
            crate::FillRect(hdc, &tr, crate::GetSysColorBrush(15));
            crate::FillRect(hdc, &[tr[0], tr[1], tr[2], tr[1] + 1], light);
            crate::FillRect(hdc, &[tr[0], tr[1], tr[0] + 1, tr[3]], light);
            crate::FillRect(hdc, &[tr[2] - 1, tr[1], tr[2], tr[3]], dark);
        }
        let mut x = tr[0] + 6;
        let it = &items[i];
        if images != 0 && it.image >= 0 {
            let (mut cx, mut cy) = (0, 0);
            unsafe { crate::imagelist::ImageList_GetIconSize(images, &mut cx, &mut cy) };
            crate::imagelist::ImageList_Draw(images, it.image, hdc, x, tr[1] + (tr[3] - tr[1] - cy) / 2, 0);
            x += cx + 3;
        }
        let mut t = it.text.clone();
        let mut tx = [x, tr[1], tr[2] - 4, tr[3]];
        // DT_SINGLELINE | DT_VCENTER (with mnemonics)
        unsafe { crate::DrawTextW(hdc, t.as_mut_ptr(), t.len() as i32, &mut tx, 0x20 | 0x4) };
        if selected && has_focus {
            let f = [tr[0] + 2, tr[1] + 2, tr[2] - 2, tr[3] - 2];
            unsafe { crate::DrawFocusRect(hdc, &f) };
        }
    }
    unsafe { crate::SelectObject(hdc, old) };
}

fn read_item(lp: i64, unicode: bool, it: &mut Item) {
    let p = lp as *const u8;
    unsafe {
        let mask = core::ptr::read_unaligned(p as *const u32);
        if mask & TCIF_TEXT != 0 {
            it.text = crate::text_arg(core::ptr::read_unaligned(p.add(16) as *const u64), unicode);
        }
        if mask & TCIF_IMAGE != 0 {
            it.image = core::ptr::read_unaligned(p.add(28) as *const i32);
        }
        if mask & TCIF_PARAM != 0 {
            it.param = core::ptr::read_unaligned(p.add(32) as *const i64);
        }
        if mask & TCIF_STATE != 0 {
            let m = core::ptr::read_unaligned(p.add(8) as *const u32);
            let s = core::ptr::read_unaligned(p.add(4) as *const u32);
            it.state = (it.state & !m) | (s & m);
        }
    }
}

/// A user's change of tab: TCN_SELCHANGING can refuse, TCN_SELCHANGE after.
fn choose(hwnd: Hwnd, i: i32) {
    let (sel, n) = with(hwnd, |t| (t.sel, t.items.len() as i32)).unwrap_or((-1, 0));
    if i < 0 || i >= n || i == sel {
        return;
    }
    if crate::notify(hwnd, TCN_SELCHANGING, &[]) != 0 {
        return;
    }
    with(hwnd, |t| {
        t.sel = i;
        t.focus = i;
    });
    unsafe { crate::InvalidateRect(hwnd, core::ptr::null(), 1) };
    crate::notify(hwnd, TCN_SELCHANGE, &[]);
}

unsafe extern "system" fn tab_proc(hwnd: Hwnd, msg: u32, wp: u64, lp: i64) -> i64 {
    let redraw = || unsafe { crate::InvalidateRect(hwnd, core::ptr::null(), 1) };
    match msg {
        0x0081 => {
            TABS.lock().insert(hwnd, Tabs { sel: -1, focus: -1, pad: (6, 3), ..Default::default() });
            unsafe { crate::DefWindowProcW(hwnd, msg, wp, lp) }
        }
        0x0082 => {
            TABS.lock().remove(&hwnd);
            unsafe { crate::DefWindowProcW(hwnd, msg, wp, lp) }
        }
        0x000F => {
            let mut ps = [0u8; 72];
            let hdc = unsafe { crate::BeginPaint(hwnd, ps.as_mut_ptr()) };
            paint(hwnd, hdc);
            unsafe { crate::EndPaint(hwnd, ps.as_ptr()) };
            0
        }
        0x0014 => 1,
        0x0030 => {
            with(hwnd, |t| t.font = wp);
            redraw();
            0
        }
        0x0031 => with(hwnd, |t| t.font as i64).unwrap_or(0),
        0x0007 | 0x0008 => {
            with(hwnd, |t| t.has_focus = msg == 0x0007);
            redraw();
            0
        }
        0x0087 => 0x1, // DLGC_WANTARROWS
        0x0100 => {
            let mut e = [0u8; 8];
            e[0..2].copy_from_slice(&(wp as u16).to_le_bytes());
            crate::notify(hwnd, TCN_KEYDOWN, &e);
            let sel = with(hwnd, |t| t.sel).unwrap_or(-1);
            match wp {
                0x25 => choose(hwnd, sel - 1),
                0x27 => choose(hwnd, sel + 1),
                _ => {}
            }
            0
        }
        0x0201 | 0x0204 => {
            let (x, y) = ((lp & 0xFFFF) as i16 as i32, ((lp >> 16) & 0xFFFF) as i16 as i32);
            let hit = rects(hwnd).iter().position(|r| x >= r[0] && x < r[2] && y >= r[1] - 2 && y < r[3]);
            if msg == 0x0201 {
                unsafe { crate::SetFocus(hwnd) };
                if let Some(i) = hit {
                    choose(hwnd, i as i32);
                }
                crate::notify(hwnd, NM_CLICK, &[]);
            } else {
                crate::notify(hwnd, NM_RCLICK, &[]);
            }
            0
        }
        TCM_GETIMAGELIST => with(hwnd, |t| t.images as i64).unwrap_or(0),
        TCM_SETIMAGELIST => {
            let old = with(hwnd, |t| core::mem::replace(&mut t.images, lp as u64)).unwrap_or(0);
            redraw();
            old as i64
        }
        TCM_GETITEMCOUNT => with(hwnd, |t| t.items.len() as i64).unwrap_or(0),
        TCM_GETITEMW | TCM_GETITEMA => {
            if lp == 0 {
                return 0;
            }
            let Some(it) = with(hwnd, |t| t.items.get(wp as usize).cloned()).flatten() else { return 0 };
            let p = lp as *mut u8;
            unsafe {
                let mask = core::ptr::read_unaligned(p as *const u32);
                if mask & TCIF_TEXT != 0 {
                    let dst = core::ptr::read_unaligned(p.add(16) as *const u64);
                    let cap = core::ptr::read_unaligned(p.add(24) as *const i32);
                    crate::put_text(dst, cap, &it.text, msg == TCM_GETITEMW);
                }
                if mask & TCIF_IMAGE != 0 {
                    core::ptr::write_unaligned(p.add(28) as *mut i32, it.image);
                }
                if mask & TCIF_PARAM != 0 {
                    core::ptr::write_unaligned(p.add(32) as *mut i64, it.param);
                }
                if mask & TCIF_STATE != 0 {
                    let m = core::ptr::read_unaligned(p.add(8) as *const u32);
                    core::ptr::write_unaligned(p.add(4) as *mut u32, it.state & m);
                }
            }
            1
        }
        TCM_SETITEMW | TCM_SETITEMA => {
            if lp == 0 {
                return 0;
            }
            let ok = with(hwnd, |t| match t.items.get_mut(wp as usize) {
                Some(it) => {
                    read_item(lp, msg == TCM_SETITEMW, it);
                    true
                }
                None => false,
            })
            .unwrap_or(false);
            redraw();
            ok as i64
        }
        TCM_INSERTITEMW | TCM_INSERTITEMA => {
            if lp == 0 || (wp as i32) < 0 {
                return -1;
            }
            let mut it = Item { text: Vec::new(), image: -1, param: 0, state: 0 };
            read_item(lp, msg == TCM_INSERTITEMW, &mut it);
            let at = with(hwnd, |t| {
                let at = (wp as usize).min(t.items.len());
                t.items.insert(at, it);
                // The first tab is selected; later ones keep the selection on
                // its tab.
                if t.sel < 0 {
                    t.sel = 0;
                    t.focus = 0;
                } else if at as i32 <= t.sel {
                    t.sel += 1;
                    t.focus = t.sel;
                }
                at as i64
            })
            .unwrap_or(-1);
            redraw();
            at
        }
        TCM_DELETEITEM => {
            let ok = with(hwnd, |t| {
                let i = wp as usize;
                if i >= t.items.len() {
                    return false;
                }
                t.items.remove(i);
                let i = i as i32;
                if t.sel == i {
                    t.sel = -1;
                } else if t.sel > i {
                    t.sel -= 1;
                }
                t.focus = t.sel.max(-1);
                true
            })
            .unwrap_or(false);
            redraw();
            ok as i64
        }
        TCM_DELETEALLITEMS => {
            with(hwnd, |t| {
                t.items.clear();
                t.sel = -1;
                t.focus = -1;
            });
            redraw();
            1
        }
        TCM_GETITEMRECT => {
            if lp == 0 {
                return 0;
            }
            match rects(hwnd).get(wp as usize) {
                Some(r) => {
                    unsafe { *(lp as *mut [i32; 4]) = *r };
                    1
                }
                None => 0,
            }
        }
        TCM_GETCURSEL => with(hwnd, |t| t.sel as i64).unwrap_or(-1),
        TCM_SETCURSEL => {
            let old = with(hwnd, |t| {
                if (wp as usize) >= t.items.len() {
                    return -1;
                }
                let old = t.sel;
                t.sel = wp as i32;
                t.focus = wp as i32;
                old
            })
            .unwrap_or(-1);
            redraw();
            old as i64
        }
        TCM_HITTEST => {
            if lp == 0 {
                return -1;
            }
            let p = lp as *mut i32;
            let (x, y) = unsafe { (*p, *p.add(1)) };
            let hit = rects(hwnd).iter().position(|r| x >= r[0] && x < r[2] && y >= r[1] && y < r[3]);
            unsafe { *p.add(2) = (if hit.is_some() { TCHT_ONITEMLABEL | 2 } else { TCHT_NOWHERE }) as i32 };
            hit.map_or(-1, |i| i as i64)
        }
        TCM_SETITEMEXTRA => 1,
        // fLarger: from a page rectangle to the control's; else the reverse.
        TCM_ADJUSTRECT => {
            if lp == 0 {
                return 0;
            }
            let h = row_height(hwnd) + 2;
            let r = unsafe { &mut *(lp as *mut [i32; 4]) };
            if wp != 0 {
                r[0] -= 4;
                r[1] -= h + 2;
                r[2] += 4;
                r[3] += 4;
            } else {
                r[0] += 4;
                r[1] += h + 2;
                r[2] -= 4;
                r[3] -= 4;
            }
            0
        }
        TCM_SETITEMSIZE => {
            let (w, h) = ((lp & 0xFFFF) as i32, ((lp >> 16) & 0xFFFF) as i32);
            let old = rects(hwnd).first().map_or((0, 0), |r| (r[2] - r[0], r[3] - r[1]));
            with(hwnd, |t| t.size = (w, h));
            redraw();
            ((old.1 as i64) << 16) | old.0 as i64
        }
        TCM_SETPADDING => {
            with(hwnd, |t| t.pad = ((lp & 0xFFFF) as i32, ((lp >> 16) & 0xFFFF) as i32));
            redraw();
            0
        }
        TCM_GETROWCOUNT => 1,
        TCM_GETCURFOCUS => with(hwnd, |t| t.focus as i64).unwrap_or(-1),
        TCM_SETCURFOCUS => {
            choose(hwnd, wp as i32);
            0
        }
        TCM_SETMINTABWIDTH => with(hwnd, |t| core::mem::replace(&mut t.min_w, (lp as i32).max(0)) as i64).unwrap_or(0),
        TCM_SETEXTENDEDSTYLE => {
            let mask = if wp == 0 { u32::MAX } else { wp as u32 };
            with(hwnd, |t| {
                let old = t.ex;
                t.ex = (t.ex & !mask) | (lp as u32 & mask);
                old as i64
            })
            .unwrap_or(0)
        }
        TCM_GETEXTENDEDSTYLE => with(hwnd, |t| t.ex as i64).unwrap_or(0),
        _ => unsafe { crate::DefWindowProcW(hwnd, msg, wp, lp) },
    }
}
