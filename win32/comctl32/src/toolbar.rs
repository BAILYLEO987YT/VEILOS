//! The toolbar ("ToolbarWindow32"): buttons with images and text, separators,
//! check and group buttons, drop-downs. A click sends WM_COMMAND to the
//! owner. Bitmaps added with TB_ADDBITMAP land in one image list per toolbar
//! (Windows' own HINST_COMMCTRL pictures take their slots but draw nothing).
//! Not yet: customizing, tooltips on hover (the desktop reports clicks, not
//! movement), hot tracking.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use crate::dllrt::Lock;
use crate::Hwnd;

const WM_USER: u32 = 0x0400;
const TB_ENABLEBUTTON: u32 = WM_USER + 1;
const TB_CHECKBUTTON: u32 = WM_USER + 2;
const TB_PRESSBUTTON: u32 = WM_USER + 3;
const TB_HIDEBUTTON: u32 = WM_USER + 4;
const TB_INDETERMINATE: u32 = WM_USER + 5;
const TB_MARKBUTTON: u32 = WM_USER + 6;
const TB_ISBUTTONENABLED: u32 = WM_USER + 9;
const TB_ISBUTTONCHECKED: u32 = WM_USER + 10;
const TB_ISBUTTONPRESSED: u32 = WM_USER + 11;
const TB_ISBUTTONHIDDEN: u32 = WM_USER + 12;
const TB_ISBUTTONINDETERMINATE: u32 = WM_USER + 13;
const TB_ISBUTTONHIGHLIGHTED: u32 = WM_USER + 14;
const TB_SETSTATE: u32 = WM_USER + 17;
const TB_GETSTATE: u32 = WM_USER + 18;
const TB_ADDBITMAP: u32 = WM_USER + 19;
const TB_ADDBUTTONSA: u32 = WM_USER + 20;
const TB_INSERTBUTTONA: u32 = WM_USER + 21;
const TB_DELETEBUTTON: u32 = WM_USER + 22;
const TB_GETBUTTON: u32 = WM_USER + 23;
const TB_BUTTONCOUNT: u32 = WM_USER + 24;
const TB_COMMANDTOINDEX: u32 = WM_USER + 25;
const TB_ADDSTRINGA: u32 = WM_USER + 28;
const TB_GETITEMRECT: u32 = WM_USER + 29;
const TB_BUTTONSTRUCTSIZE: u32 = WM_USER + 30;
const TB_SETBUTTONSIZE: u32 = WM_USER + 31;
const TB_SETBITMAPSIZE: u32 = WM_USER + 32;
const TB_AUTOSIZE: u32 = WM_USER + 33;
const TB_GETTOOLTIPS: u32 = WM_USER + 35;
const TB_SETTOOLTIPS: u32 = WM_USER + 36;
const TB_SETPARENT: u32 = WM_USER + 37;
const TB_SETROWS: u32 = WM_USER + 39;
const TB_GETROWS: u32 = WM_USER + 40;
const TB_SETCMDID: u32 = WM_USER + 42;
const TB_CHANGEBITMAP: u32 = WM_USER + 43;
const TB_GETBITMAP: u32 = WM_USER + 44;
const TB_GETBUTTONTEXTA: u32 = WM_USER + 45;
const TB_SETINDENT: u32 = WM_USER + 47;
const TB_SETIMAGELIST: u32 = WM_USER + 48;
const TB_GETIMAGELIST: u32 = WM_USER + 49;
const TB_LOADIMAGES: u32 = WM_USER + 50;
const TB_GETRECT: u32 = WM_USER + 51;
const TB_SETHOTIMAGELIST: u32 = WM_USER + 52;
const TB_GETHOTIMAGELIST: u32 = WM_USER + 53;
const TB_SETDISABLEDIMAGELIST: u32 = WM_USER + 54;
const TB_GETDISABLEDIMAGELIST: u32 = WM_USER + 55;
const TB_SETSTYLE: u32 = WM_USER + 56;
const TB_GETSTYLE: u32 = WM_USER + 57;
const TB_GETBUTTONSIZE: u32 = WM_USER + 58;
const TB_SETBUTTONWIDTH: u32 = WM_USER + 59;
const TB_SETMAXTEXTROWS: u32 = WM_USER + 60;
const TB_GETTEXTROWS: u32 = WM_USER + 61;
const TB_GETBUTTONINFOW: u32 = WM_USER + 63;
const TB_SETBUTTONINFOW: u32 = WM_USER + 64;
const TB_GETBUTTONINFOA: u32 = WM_USER + 65;
const TB_SETBUTTONINFOA: u32 = WM_USER + 66;
const TB_INSERTBUTTONW: u32 = WM_USER + 67;
const TB_ADDBUTTONSW: u32 = WM_USER + 68;
const TB_HITTEST: u32 = WM_USER + 69;
const TB_SETDRAWTEXTFLAGS: u32 = WM_USER + 70;
const TB_GETHOTITEM: u32 = WM_USER + 71;
const TB_SETHOTITEM: u32 = WM_USER + 72;
const TB_GETBUTTONTEXTW: u32 = WM_USER + 75;
const TB_ADDSTRINGW: u32 = WM_USER + 77;
const TB_SETEXTENDEDSTYLE: u32 = WM_USER + 84;
const TB_GETEXTENDEDSTYLE: u32 = WM_USER + 85;
const TB_GETPADDING: u32 = WM_USER + 86;
const TB_SETPADDING: u32 = WM_USER + 87;

const TBSTATE_CHECKED: u8 = 0x01;
const TBSTATE_PRESSED: u8 = 0x02;
const TBSTATE_ENABLED: u8 = 0x04;
const TBSTATE_HIDDEN: u8 = 0x08;
const TBSTATE_INDETERMINATE: u8 = 0x10;
const TBSTATE_MARKED: u8 = 0x80;

const BTNS_SEP: u8 = 0x01;
const BTNS_CHECK: u8 = 0x02;
const BTNS_GROUP: u8 = 0x04;
const BTNS_DROPDOWN: u8 = 0x08;
const BTNS_AUTOSIZE: u8 = 0x10;
const BTNS_SHOWTEXT: u8 = 0x40;

const TBSTYLE_FLAT: u32 = 0x800;
const TBSTYLE_LIST: u32 = 0x1000;
const CCS_NORESIZE: u32 = 0x4;
const CCS_NOPARENTALIGN: u32 = 0x8;
const CCS_BOTTOM: u32 = 0x3;

const TBIF_IMAGE: u32 = 0x1;
const TBIF_TEXT: u32 = 0x2;
const TBIF_STATE: u32 = 0x4;
const TBIF_STYLE: u32 = 0x8;
const TBIF_LPARAM: u32 = 0x10;
const TBIF_COMMAND: u32 = 0x20;
const TBIF_SIZE: u32 = 0x40;
const TBIF_BYINDEX: u32 = 0x8000_0000;

const TBN_DROPDOWN: i32 = -710;
const NM_CLICK: i32 = -2;
const NM_RCLICK: i32 = -5;

const HINST_COMMCTRL: u64 = u64::MAX;

#[derive(Clone)]
struct Button {
    bitmap: i32,
    id: i32,
    state: u8,
    style: u8,
    data: u64,
    /// An index into the string pool, or -1.
    string: i32,
    /// Its own text (a string pointer given to it, or TB_SETBUTTONINFO).
    text: Option<Vec<u16>>,
    cx: i32,
}

struct Bar {
    buttons: Vec<Button>,
    strings: Vec<Vec<u16>>,
    font: u64,
    bitmap_size: (i32, i32),
    button_size: (i32, i32),
    images: u64,
    hot_images: u64,
    disabled_images: u64,
    /// Pictures from HINST_COMMCTRL: slots with no image.
    bitmaps: i32,
    tooltips: Hwnd,
    owner: Hwnd,
    indent: i32,
    ex: u32,
    padding: (i32, i32),
    text_rows: i32,
    struct_size: u32,
    /// The button the mouse went down on.
    pressed: Option<usize>,
}

static BARS: Lock<BTreeMap<Hwnd, Bar>> = Lock::new(BTreeMap::new());

fn with<R>(hwnd: Hwnd, f: impl FnOnce(&mut Bar) -> R) -> Option<R> {
    BARS.lock().get_mut(&hwnd).map(f)
}

pub fn register_class() {
    crate::register("ToolbarWindow32", toolbar_proc as *const () as usize, true);
}

fn style(hwnd: Hwnd) -> u32 {
    unsafe { crate::GetWindowLongPtrW(hwnd, -16) as u32 }
}

fn redraw(hwnd: Hwnd) {
    unsafe { crate::InvalidateRect(hwnd, core::ptr::null(), 1) };
}

fn index_of(hwnd: Hwnd, id: u64) -> Option<usize> {
    with(hwnd, |b| b.buttons.iter().position(|x| x.id == id as i32)).flatten()
}

fn button_text(b: &Bar, x: &Button) -> Vec<u16> {
    if let Some(t) = &x.text {
        return t.clone();
    }
    if x.string >= 0 {
        return b.strings.get(x.string as usize).cloned().unwrap_or_default();
    }
    Vec::new()
}

/// The button size when not set: the bitmap, room for the text, a margin.
fn size_of(hwnd: Hwnd) -> (i32, i32) {
    let Some((set, bm, font, buttons_text, rows)) = with(hwnd, |b| {
        let t: Vec<Vec<u16>> = b.buttons.iter().map(|x| button_text(b, x)).collect();
        (b.button_size, b.bitmap_size, b.font, t, b.text_rows)
    }) else {
        return (24, 22);
    };
    let any_text = rows > 0 && buttons_text.iter().any(|t| !t.is_empty());
    if !any_text {
        return (set.0.max(bm.0 + 7), set.1.max(bm.1 + 7));
    }
    let list = style(hwnd) & TBSTYLE_LIST != 0;
    let dc = unsafe { crate::GetDC(hwnd) };
    let old = unsafe { crate::SelectObject(dc, crate::font_or_default(font)) };
    let (mut tw, mut th) = (0, 0);
    for t in &buttons_text {
        let (w, h) = crate::text_size(dc, t);
        tw = tw.max(w);
        th = th.max(h);
    }
    unsafe {
        crate::SelectObject(dc, old);
        crate::ReleaseDC(hwnd, dc);
    }
    if list {
        (set.0.max(bm.0 + tw + 11), set.1.max(bm.1.max(th) + 7))
    } else {
        (set.0.max(bm.0.max(tw) + 7), set.1.max(bm.1 + th + 7))
    }
}

/// Each button's rectangle (hidden ones empty).
fn rects(hwnd: Hwnd) -> Vec<[i32; 4]> {
    let (bw, bh) = size_of(hwnd);
    let Some((buttons, indent, font, has)) = with(hwnd, |b| {
        let info: Vec<(u8, u8, i32, Vec<u16>)> = b.buttons.iter().map(|x| (x.state, x.style, x.cx, button_text(b, x))).collect();
        (info, b.indent, b.font, b.images != 0)
    }) else {
        return Vec::new();
    };
    let mut x = indent + 2;
    let mut c = [0i32; 4];
    unsafe { crate::GetClientRect(hwnd, &mut c) };
    let dc = unsafe { crate::GetDC(hwnd) };
    let old = unsafe { crate::SelectObject(dc, crate::font_or_default(font)) };
    let wrap = style(hwnd) & 0x200 != 0; // TBSTYLE_WRAPABLE
    let mut y = 2;
    let out = buttons
        .iter()
        .map(|(state, st, cx, text)| {
            if state & TBSTATE_HIDDEN != 0 {
                return [x, y, x, y];
            }
            let w = if st & BTNS_SEP != 0 {
                if *cx > 0 { *cx } else { 8 }
            } else if *cx > 0 {
                *cx
            } else if st & BTNS_AUTOSIZE != 0 {
                let tw = if text.is_empty() { 0 } else { crate::text_size(dc, text).0 + 6 };
                (if has { 16 + 7 } else { 7 }) + tw
            } else {
                bw
            };
            let w = w + if st & BTNS_DROPDOWN != 0 && st & BTNS_SEP == 0 { 12 } else { 0 };
            if wrap && x > 2 && x + w > c[2] && c[2] > 0 {
                x = indent + 2;
                y += bh + 2;
            }
            let r = [x, y, x + w, y + bh];
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

/// Fit the parent's width, across the top (or bottom).
fn autosize(hwnd: Hwnd) {
    let st = style(hwnd);
    if st & (CCS_NORESIZE | CCS_NOPARENTALIGN) != 0 {
        return;
    }
    let parent = unsafe { crate::GetParent(hwnd) };
    let mut r = [0i32; 4];
    unsafe { crate::GetClientRect(parent, &mut r) };
    let (_, bh) = size_of(hwnd);
    let bottom = rects(hwnd).iter().map(|x| x[3]).max().unwrap_or(bh + 2).max(bh + 2);
    let h = bottom + 4;
    let y = if st & 0x3 == CCS_BOTTOM { r[3] - h } else { 0 };
    unsafe { crate::SetWindowPos(hwnd, 0, 0, y, r[2], h, 0x14) };
}

fn paint(hwnd: Hwnd, hdc: u64) {
    let mut c = [0i32; 4];
    unsafe { crate::GetClientRect(hwnd, &mut c) };
    unsafe { crate::FillRect(hdc, &c, crate::GetSysColorBrush(15)) };
    let Some((buttons, font, images, disabled, bm, rows)) = with(hwnd, |b| {
        let info: Vec<(Button, Vec<u16>)> = b.buttons.iter().map(|x| (x.clone(), button_text(b, x))).collect();
        (info, b.font, b.images, b.disabled_images, b.bitmap_size, b.text_rows)
    }) else {
        return;
    };
    let st = style(hwnd);
    let flat = st & TBSTYLE_FLAT != 0;
    let list = st & TBSTYLE_LIST != 0;
    let old = unsafe { crate::SelectObject(hdc, crate::font_or_default(font)) };
    unsafe { crate::SetBkMode(hdc, 1) };
    let (light, dark) = unsafe { (crate::GetSysColorBrush(20), crate::GetSysColorBrush(16)) };
    for (i, r) in rects(hwnd).iter().enumerate() {
        let (b, text) = &buttons[i];
        if b.state & TBSTATE_HIDDEN != 0 {
            continue;
        }
        if b.style & BTNS_SEP != 0 {
            if flat {
                let mid = (r[0] + r[2]) / 2;
                unsafe {
                    crate::FillRect(hdc, &[mid, r[1] + 1, mid + 1, r[3] - 1], dark);
                    crate::FillRect(hdc, &[mid + 1, r[1] + 1, mid + 2, r[3] - 1], light);
                }
            }
            continue;
        }
        let down = b.state & (TBSTATE_PRESSED | TBSTATE_CHECKED) != 0;
        let enabled = b.state & TBSTATE_ENABLED != 0;
        if down || !flat {
            let (tl, br) = if down { (dark, light) } else { (light, dark) };
            unsafe {
                crate::FillRect(hdc, &[r[0], r[1], r[2], r[1] + 1], tl);
                crate::FillRect(hdc, &[r[0], r[1], r[0] + 1, r[3]], tl);
                crate::FillRect(hdc, &[r[0], r[3] - 1, r[2], r[3]], br);
                crate::FillRect(hdc, &[r[2] - 1, r[1], r[2], r[3]], br);
            }
        }
        let off = down as i32;
        let body_w = r[2] - r[0] - if b.style & BTNS_DROPDOWN != 0 { 12 } else { 0 };
        // TBSTYLE_EX_MIXEDBUTTONS: only BTNS_SHOWTEXT buttons show text.
        let mixed = with(hwnd, |x| x.ex & 8 != 0).unwrap_or(false);
        let show_text = rows > 0 && !text.is_empty() && (!mixed || b.style & BTNS_SHOWTEXT != 0);
        let il = if !enabled && disabled != 0 { disabled } else { images };
        let image_x = if list { r[0] + 3 + off } else { r[0] + (body_w - bm.0) / 2 + off };
        let image_y = if list || !show_text { r[1] + (r[3] - r[1] - bm.1) / 2 + off } else { r[1] + 3 + off };
        if il != 0 && b.bitmap >= 0 {
            crate::imagelist::ImageList_Draw(il, b.bitmap & 0xFFFF, hdc, image_x, image_y, 0);
        }
        if show_text {
            unsafe { crate::SetTextColor(hdc, crate::GetSysColor(if enabled { 18 } else { 17 })) };
            let mut t = text.clone();
            let mut tr = if list {
                [image_x + bm.0 + 3, r[1], r[0] + body_w, r[3]]
            } else {
                [r[0], image_y + bm.1, r[0] + body_w, r[3]]
            };
            // DT_SINGLELINE | DT_VCENTER | DT_CENTER unless beside the image.
            let flags = if list { 0x20 | 0x4 } else { 0x20 | 0x4 | 0x1 };
            unsafe { crate::DrawTextW(hdc, t.as_mut_ptr(), t.len() as i32, &mut tr, flags) };
        }
        if b.style & BTNS_DROPDOWN != 0 {
            let ax = r[2] - 9;
            let ay = (r[1] + r[3]) / 2 - 1;
            for k in 0..3 {
                unsafe { crate::FillRect(hdc, &[ax + k, ay + k, ax + 5 - k, ay + k + 1], crate::GetSysColorBrush(18)) };
            }
        }
    }
    unsafe { crate::SelectObject(hdc, old) };
}

/// TBBUTTON array into the bar at `at`.
fn add_buttons(hwnd: Hwnd, count: u64, p: i64, at: Option<usize>, unicode: bool) -> bool {
    if p == 0 {
        return false;
    }
    let size = with(hwnd, |b| if b.struct_size >= 32 { b.struct_size } else { 32 }).unwrap_or(32) as usize;
    let mut list = Vec::new();
    for k in 0..count as usize {
        let e = unsafe { (p as *const u8).add(k * size) };
        let (bitmap, id, state, st, data, string) = unsafe {
            (
                core::ptr::read_unaligned(e as *const i32),
                core::ptr::read_unaligned(e.add(4) as *const i32),
                *e.add(8),
                *e.add(9),
                core::ptr::read_unaligned(e.add(16) as *const u64),
                core::ptr::read_unaligned(e.add(24) as *const i64),
            )
        };
        // iString: an index, or a pointer to the text.
        let (string, text) = if string > 0xFFFF { (-1, Some(crate::text_arg(string as u64, unicode))) } else { (string as i32, None) };
        list.push(Button { bitmap, id, state, style: st, data, string, text, cx: 0 });
    }
    with(hwnd, |b| {
        let mut where_ = at.unwrap_or(b.buttons.len()).min(b.buttons.len());
        for x in list {
            b.buttons.insert(where_, x);
            where_ += 1;
        }
    });
    redraw(hwnd);
    true
}

/// A double-NUL list of strings, or one string resource: the first new index.
fn add_strings(hwnd: Hwnd, wp: u64, lp: i64, unicode: bool) -> i64 {
    let mut new: Vec<Vec<u16>> = Vec::new();
    if wp != 0 && lp as u64 >> 16 == 0 {
        // From a string resource: its first character separates the strings.
        let mut buf = [0u16; 512];
        let n = unsafe { crate::LoadStringW(wp, lp as u32, buf.as_mut_ptr(), 512) } as usize;
        if n > 0 {
            let sep = buf[n - 1];
            new.extend(buf[..n].split(|&c| c == sep).map(|s| s.to_vec()));
            if new.last().is_some_and(|l| l.is_empty()) {
                new.pop();
            }
        }
    } else if lp != 0 {
        let mut at = lp as u64;
        loop {
            let s = crate::text_arg(at, unicode);
            if s.is_empty() {
                break;
            }
            if unicode {
                at += (s.len() as u64 + 1) * 2;
            } else {
                let mut n = 0;
                while unsafe { *(at as *const u8).add(n) } != 0 {
                    n += 1;
                }
                at += n as u64 + 1;
            }
            new.push(s);
        }
    }
    with(hwnd, |b| {
        let first = b.strings.len() as i64;
        if new.is_empty() {
            return -1;
        }
        b.strings.extend(new);
        first
    })
    .unwrap_or(-1)
}

/// TB_ADDBITMAP: `n` pictures from a TBADDBITMAP.
fn add_bitmap(hwnd: Hwnd, n: u64, lp: i64) -> i64 {
    if lp == 0 {
        return -1;
    }
    let (inst, id) = unsafe { (*(lp as *const u64), *(lp as *const u64).add(1)) };
    let first = with(hwnd, |b| b.bitmaps).unwrap_or(0);
    with(hwnd, |b| b.bitmaps += n as i32);
    if inst == HINST_COMMCTRL {
        return first as i64;
    }
    let (bw, bh) = with(hwnd, |b| b.bitmap_size).unwrap_or((16, 15));
    let existing = with(hwnd, |b| b.images).unwrap_or(0);
    let il = if existing != 0 {
        existing
    } else {
        let il = crate::imagelist::ImageList_Create(bw, bh, 0x20, n as i32, 4);
        with(hwnd, |b| b.images = il);
        il
    };
    // From a resource, or a bitmap handle when `inst` is 0.
    let (bitmap, ours) = if inst != 0 {
        (unsafe { crate::LoadImageW(inst, id as *const u16, 0, 0, 0, 0) }, true)
    } else {
        (id, false)
    };
    if bitmap != 0 {
        // Light gray stands for transparent, as in classic toolbar pictures.
        crate::imagelist::ImageList_AddMasked(il, bitmap, 0x00C0_C0C0);
        if ours {
            unsafe { crate::DeleteObject(bitmap) };
        }
    }
    first as i64
}

/// TBBUTTONINFO: find by command or by index.
fn info_index(hwnd: Hwnd, wp: u64, lp: i64) -> Option<usize> {
    if lp == 0 {
        return None;
    }
    let mask = unsafe { core::ptr::read_unaligned((lp as *const u8).add(4) as *const u32) };
    if mask & TBIF_BYINDEX != 0 {
        let i = wp as usize;
        with(hwnd, |b| (i < b.buttons.len()).then_some(i)).flatten()
    } else {
        index_of(hwnd, wp)
    }
}

/// A click on button `i`: group and check behaviour, WM_COMMAND to the owner.
fn click(hwnd: Hwnd, i: usize) {
    let Some(b) = with(hwnd, |b| b.buttons.get(i).cloned()).flatten() else { return };
    if b.style & BTNS_SEP != 0 || b.state & TBSTATE_ENABLED == 0 {
        return;
    }
    if b.style & BTNS_CHECK != 0 {
        with(hwnd, |bar| {
            if b.style & BTNS_GROUP != 0 {
                // The neighbours in its group go up; it stays down.
                let mut k = i;
                while k > 0 && bar.buttons[k - 1].style & BTNS_GROUP != 0 {
                    k -= 1;
                    bar.buttons[k].state &= !TBSTATE_CHECKED;
                }
                let mut k = i + 1;
                while k < bar.buttons.len() && bar.buttons[k].style & BTNS_GROUP != 0 {
                    bar.buttons[k].state &= !TBSTATE_CHECKED;
                    k += 1;
                }
                bar.buttons[i].state |= TBSTATE_CHECKED;
            } else {
                bar.buttons[i].state ^= TBSTATE_CHECKED;
            }
        });
    }
    redraw(hwnd);
    let owner = with(hwnd, |b| b.owner).unwrap_or(0);
    unsafe { crate::SendMessageW(owner, 0x0111, b.id as u16 as u64, hwnd as i64) };
}

unsafe extern "system" fn toolbar_proc(hwnd: Hwnd, msg: u32, wp: u64, lp: i64) -> i64 {
    match msg {
        0x0081 => {
            let owner = unsafe { crate::GetParent(hwnd) };
            BARS.lock().insert(
                hwnd,
                Bar {
                    buttons: Vec::new(),
                    strings: Vec::new(),
                    font: 0,
                    bitmap_size: (16, 15),
                    button_size: (24, 22),
                    images: 0,
                    hot_images: 0,
                    disabled_images: 0,
                    bitmaps: 0,
                    tooltips: 0,
                    owner,
                    indent: 0,
                    ex: 0,
                    padding: (7, 6),
                    text_rows: 1,
                    struct_size: 0,
                    pressed: None,
                },
            );
            unsafe { crate::DefWindowProcW(hwnd, msg, wp, lp) }
        }
        0x0082 => {
            BARS.lock().remove(&hwnd);
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
        0x0005 => {
            redraw(hwnd);
            0
        }
        0x0030 => {
            with(hwnd, |b| b.font = wp);
            redraw(hwnd);
            0
        }
        0x0031 => with(hwnd, |b| b.font as i64).unwrap_or(0),
        0x0201 | 0x0203 | 0x0204 => {
            let (x, y) = ((lp & 0xFFFF) as i16 as i32, ((lp >> 16) & 0xFFFF) as i16 as i32);
            let at = rects(hwnd).iter().position(|r| x >= r[0] && x < r[2] && y >= r[1] && y < r[3]);
            if msg == 0x0204 {
                crate::notify(hwnd, NM_RCLICK, &[]);
                return 0;
            }
            if let Some(i) = at {
                let (st, id) = with(hwnd, |b| (b.buttons[i].style, b.buttons[i].id)).unwrap_or((0, 0));
                if st & BTNS_DROPDOWN != 0 && x >= rects(hwnd)[i][2] - 12 {
                    let mut e = [0u8; 72];
                    e[0..4].copy_from_slice(&id.to_le_bytes());
                    crate::notify(hwnd, TBN_DROPDOWN, &e);
                } else {
                    with(hwnd, |b| b.pressed = Some(i));
                }
            }
            0
        }
        0x0202 => {
            let (x, y) = ((lp & 0xFFFF) as i16 as i32, ((lp >> 16) & 0xFFFF) as i16 as i32);
            let at = rects(hwnd).iter().position(|r| x >= r[0] && x < r[2] && y >= r[1] && y < r[3]);
            let was = with(hwnd, |b| b.pressed.take()).flatten();
            crate::notify(hwnd, NM_CLICK, &[]);
            if let (Some(i), true) = (at, at == was) {
                click(hwnd, i);
            }
            0
        }

        TB_BUTTONSTRUCTSIZE => {
            with(hwnd, |b| b.struct_size = wp as u32);
            0
        }
        TB_ADDBUTTONSW | TB_ADDBUTTONSA => {
            let r = add_buttons(hwnd, wp, lp, None, msg == TB_ADDBUTTONSW);
            autosize(hwnd);
            r as i64
        }
        TB_INSERTBUTTONW | TB_INSERTBUTTONA => add_buttons(hwnd, 1, lp, Some(wp as usize), msg == TB_INSERTBUTTONW) as i64,
        TB_DELETEBUTTON => {
            let ok = with(hwnd, |b| {
                if (wp as usize) < b.buttons.len() {
                    b.buttons.remove(wp as usize);
                    true
                } else {
                    false
                }
            })
            .unwrap_or(false);
            redraw(hwnd);
            ok as i64
        }
        TB_BUTTONCOUNT => with(hwnd, |b| b.buttons.len() as i64).unwrap_or(0),
        TB_COMMANDTOINDEX => index_of(hwnd, wp).map_or(-1, |i| i as i64),
        TB_GETBUTTON => {
            if lp == 0 {
                return 0;
            }
            let Some(b) = with(hwnd, |b| b.buttons.get(wp as usize).cloned()).flatten() else { return 0 };
            let e = lp as *mut u8;
            unsafe {
                core::ptr::write_unaligned(e as *mut i32, b.bitmap);
                core::ptr::write_unaligned(e.add(4) as *mut i32, b.id);
                *e.add(8) = b.state;
                *e.add(9) = b.style;
                core::ptr::write_unaligned(e.add(16) as *mut u64, b.data);
                core::ptr::write_unaligned(e.add(24) as *mut i64, b.string as i64);
            }
            1
        }
        TB_ENABLEBUTTON | TB_CHECKBUTTON | TB_PRESSBUTTON | TB_HIDEBUTTON | TB_INDETERMINATE | TB_MARKBUTTON => {
            let Some(i) = index_of(hwnd, wp) else { return 0 };
            let bit = match msg {
                TB_ENABLEBUTTON => TBSTATE_ENABLED,
                TB_CHECKBUTTON => TBSTATE_CHECKED,
                TB_PRESSBUTTON => TBSTATE_PRESSED,
                TB_HIDEBUTTON => TBSTATE_HIDDEN,
                TB_INDETERMINATE => TBSTATE_INDETERMINATE,
                _ => TBSTATE_MARKED,
            };
            let on = lp & 0xFFFF != 0;
            with(hwnd, |b| {
                if on {
                    b.buttons[i].state |= bit;
                } else {
                    b.buttons[i].state &= !bit;
                }
            });
            redraw(hwnd);
            1
        }
        TB_ISBUTTONENABLED | TB_ISBUTTONCHECKED | TB_ISBUTTONPRESSED | TB_ISBUTTONHIDDEN | TB_ISBUTTONINDETERMINATE | TB_ISBUTTONHIGHLIGHTED => {
            let Some(i) = index_of(hwnd, wp) else { return -1 };
            let bit = match msg {
                TB_ISBUTTONENABLED => TBSTATE_ENABLED,
                TB_ISBUTTONCHECKED => TBSTATE_CHECKED,
                TB_ISBUTTONPRESSED => TBSTATE_PRESSED,
                TB_ISBUTTONHIDDEN => TBSTATE_HIDDEN,
                TB_ISBUTTONINDETERMINATE => TBSTATE_INDETERMINATE,
                _ => TBSTATE_MARKED,
            };
            with(hwnd, |b| (b.buttons[i].state & bit) as i64).unwrap_or(0)
        }
        TB_SETSTATE => {
            let Some(i) = index_of(hwnd, wp) else { return 0 };
            with(hwnd, |b| b.buttons[i].state = lp as u8);
            redraw(hwnd);
            1
        }
        TB_GETSTATE => index_of(hwnd, wp).and_then(|i| with(hwnd, |b| b.buttons[i].state as i64)).unwrap_or(-1),
        TB_ADDBITMAP => add_bitmap(hwnd, wp, lp),
        TB_LOADIMAGES => {
            // Standard pictures: slots only (TBADDBITMAP from HINST_COMMCTRL).
            let n = match wp {
                0 | 1 => 15,
                4 | 5 => 12,
                8 | 9 => 5,
                _ => 0,
            };
            with(hwnd, |b| {
                let first = b.bitmaps;
                b.bitmaps += n;
                first as i64
            })
            .unwrap_or(-1)
        }
        TB_ADDSTRINGW | TB_ADDSTRINGA => add_strings(hwnd, wp, lp, msg == TB_ADDSTRINGW),
        TB_GETITEMRECT => {
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
        TB_GETRECT => {
            if lp == 0 {
                return 0;
            }
            let Some(i) = index_of(hwnd, wp) else { return 0 };
            unsafe { *(lp as *mut [i32; 4]) = rects(hwnd)[i] };
            1
        }
        TB_SETBUTTONSIZE => {
            with(hwnd, |b| b.button_size = ((lp & 0xFFFF) as i32, ((lp >> 16) & 0xFFFF) as i32));
            redraw(hwnd);
            1
        }
        TB_SETBITMAPSIZE => {
            with(hwnd, |b| b.bitmap_size = ((lp & 0xFFFF) as i32, ((lp >> 16) & 0xFFFF) as i32));
            redraw(hwnd);
            1
        }
        TB_GETBUTTONSIZE => {
            let (w, h) = size_of(hwnd);
            ((h as i64) << 16) | w as i64
        }
        TB_AUTOSIZE => {
            autosize(hwnd);
            0
        }
        TB_GETTOOLTIPS => with(hwnd, |b| b.tooltips as i64).unwrap_or(0),
        TB_SETTOOLTIPS => {
            with(hwnd, |b| b.tooltips = wp);
            0
        }
        TB_SETPARENT => with(hwnd, |b| core::mem::replace(&mut b.owner, wp) as i64).unwrap_or(0),
        TB_SETROWS => 0,
        TB_GETROWS => {
            let mut tops: Vec<i32> = rects(hwnd).iter().map(|r| r[1]).collect();
            tops.dedup();
            tops.len().max(1) as i64
        }
        TB_SETCMDID => {
            let ok = with(hwnd, |b| match b.buttons.get_mut(wp as usize) {
                Some(x) => {
                    x.id = lp as i32;
                    true
                }
                None => false,
            })
            .unwrap_or(false);
            ok as i64
        }
        TB_CHANGEBITMAP => {
            let Some(i) = index_of(hwnd, wp) else { return 0 };
            with(hwnd, |b| b.buttons[i].bitmap = lp as i32);
            redraw(hwnd);
            1
        }
        TB_GETBITMAP => index_of(hwnd, wp).and_then(|i| with(hwnd, |b| b.buttons[i].bitmap as i64)).unwrap_or(0),
        TB_GETBUTTONTEXTW | TB_GETBUTTONTEXTA => {
            let Some(i) = index_of(hwnd, wp) else { return -1 };
            let Some(t) = with(hwnd, |b| {
                let x = b.buttons[i].clone();
                button_text(b, &x)
            }) else {
                return -1;
            };
            if lp != 0 {
                crate::put_text(lp as u64, i32::MAX, &t, msg == TB_GETBUTTONTEXTW);
            }
            t.len() as i64
        }
        TB_SETINDENT => {
            with(hwnd, |b| b.indent = wp as i32);
            redraw(hwnd);
            1
        }
        TB_SETIMAGELIST => {
            let old = with(hwnd, |b| core::mem::replace(&mut b.images, lp as u64)).unwrap_or(0);
            if lp != 0 {
                let (mut cx, mut cy) = (0, 0);
                unsafe { crate::imagelist::ImageList_GetIconSize(lp as u64, &mut cx, &mut cy) };
                with(hwnd, |b| b.bitmap_size = (cx, cy));
            }
            redraw(hwnd);
            old as i64
        }
        TB_GETIMAGELIST => with(hwnd, |b| b.images as i64).unwrap_or(0),
        TB_SETHOTIMAGELIST => with(hwnd, |b| core::mem::replace(&mut b.hot_images, lp as u64) as i64).unwrap_or(0),
        TB_GETHOTIMAGELIST => with(hwnd, |b| b.hot_images as i64).unwrap_or(0),
        TB_SETDISABLEDIMAGELIST => with(hwnd, |b| core::mem::replace(&mut b.disabled_images, lp as u64) as i64).unwrap_or(0),
        TB_GETDISABLEDIMAGELIST => with(hwnd, |b| b.disabled_images as i64).unwrap_or(0),
        TB_SETSTYLE => {
            unsafe { crate::SetWindowLongPtrW(hwnd, -16, lp) };
            redraw(hwnd);
            0
        }
        TB_GETSTYLE => style(hwnd) as i64,
        TB_SETBUTTONWIDTH => 1,
        TB_SETMAXTEXTROWS => {
            with(hwnd, |b| b.text_rows = wp as i32);
            redraw(hwnd);
            1
        }
        TB_GETTEXTROWS => with(hwnd, |b| b.text_rows as i64).unwrap_or(0),
        TB_GETBUTTONINFOW | TB_GETBUTTONINFOA => {
            let Some(i) = info_index(hwnd, wp, lp) else { return -1 };
            let Some((x, text)) = with(hwnd, |b| {
                let x = b.buttons[i].clone();
                let t = button_text(b, &x);
                (x, t)
            }) else {
                return -1;
            };
            let p = lp as *mut u8;
            unsafe {
                let mask = core::ptr::read_unaligned(p.add(4) as *const u32);
                if mask & TBIF_COMMAND != 0 {
                    core::ptr::write_unaligned(p.add(8) as *mut i32, x.id);
                }
                if mask & TBIF_IMAGE != 0 {
                    core::ptr::write_unaligned(p.add(12) as *mut i32, x.bitmap);
                }
                if mask & TBIF_STATE != 0 {
                    *p.add(16) = x.state;
                }
                if mask & TBIF_STYLE != 0 {
                    *p.add(17) = x.style;
                }
                if mask & TBIF_SIZE != 0 {
                    let w = rects(hwnd).get(i).map_or(0, |r| r[2] - r[0]);
                    core::ptr::write_unaligned(p.add(18) as *mut u16, w as u16);
                }
                if mask & TBIF_LPARAM != 0 {
                    core::ptr::write_unaligned(p.add(24) as *mut u64, x.data);
                }
                if mask & TBIF_TEXT != 0 {
                    let dst = core::ptr::read_unaligned(p.add(32) as *const u64);
                    let cap = core::ptr::read_unaligned(p.add(40) as *const i32);
                    crate::put_text(dst, cap, &text, msg == TB_GETBUTTONINFOW);
                }
            }
            i as i64
        }
        TB_SETBUTTONINFOW | TB_SETBUTTONINFOA => {
            let Some(i) = info_index(hwnd, wp, lp) else { return 0 };
            let p = lp as *const u8;
            let (mask, id, image, state, st, cx, data, text) = unsafe {
                (
                    core::ptr::read_unaligned(p.add(4) as *const u32),
                    core::ptr::read_unaligned(p.add(8) as *const i32),
                    core::ptr::read_unaligned(p.add(12) as *const i32),
                    *p.add(16),
                    *p.add(17),
                    core::ptr::read_unaligned(p.add(18) as *const u16),
                    core::ptr::read_unaligned(p.add(24) as *const u64),
                    core::ptr::read_unaligned(p.add(32) as *const u64),
                )
            };
            let new_text = (mask & TBIF_TEXT != 0).then(|| crate::text_arg(text, msg == TB_SETBUTTONINFOW));
            with(hwnd, |b| {
                let x = &mut b.buttons[i];
                if mask & TBIF_COMMAND != 0 {
                    x.id = id;
                }
                if mask & TBIF_IMAGE != 0 {
                    x.bitmap = image;
                }
                if mask & TBIF_STATE != 0 {
                    x.state = state;
                }
                if mask & TBIF_STYLE != 0 {
                    x.style = st;
                }
                if mask & TBIF_SIZE != 0 {
                    x.cx = cx as i32;
                }
                if mask & TBIF_LPARAM != 0 {
                    x.data = data;
                }
                if let Some(t) = new_text {
                    x.text = Some(t);
                }
            });
            redraw(hwnd);
            1
        }
        TB_HITTEST => {
            if lp == 0 {
                return -1;
            }
            let (x, y) = unsafe { (*(lp as *const i32), *(lp as *const i32).add(1)) };
            rects(hwnd).iter().position(|r| x >= r[0] && x < r[2] && y >= r[1] && y < r[3]).map_or(-1, |i| i as i64)
        }
        TB_SETDRAWTEXTFLAGS => 0,
        TB_GETHOTITEM => -1,
        TB_SETHOTITEM => -1,
        TB_SETEXTENDEDSTYLE => with(hwnd, |b| core::mem::replace(&mut b.ex, lp as u32) as i64).unwrap_or(0),
        TB_GETEXTENDEDSTYLE => with(hwnd, |b| b.ex as i64).unwrap_or(0),
        TB_GETPADDING => with(hwnd, |b| ((b.padding.1 as i64) << 16) | b.padding.0 as i64).unwrap_or(0),
        TB_SETPADDING => with(hwnd, |b| {
            let old = b.padding;
            b.padding = ((lp & 0xFFFF) as i32, ((lp >> 16) & 0xFFFF) as i32);
            ((old.1 as i64) << 16) | old.0 as i64
        })
        .unwrap_or(0),
        _ => unsafe { crate::DefWindowProcW(hwnd, msg, wp, lp) },
    }
}

/// `CreateToolbarEx(parent, style, id, bitmaps, inst, bitmap id, buttons, n,
/// button w, h, bitmap w, h, struct size)`.
#[no_mangle]
pub unsafe extern "system" fn CreateToolbarEx(parent: Hwnd, st: u32, id: u32, bitmaps: i32, inst: u64, bmp: u64, buttons: *const u8, n: i32,
                                              bw: i32, bh: i32, bmw: i32, bmh: i32, size: u32) -> Hwnd {
    let class = crate::wide("ToolbarWindow32");
    let tb = unsafe { crate::CreateWindowExW(0, class.as_ptr(), core::ptr::null(), st | 0x4000_0000, 0, 0, 0, 0, parent, id as u64, inst, 0) };
    if tb == 0 {
        return 0;
    }
    unsafe {
        crate::SendMessageW(tb, TB_BUTTONSTRUCTSIZE, size as u64, 0);
        if bmw > 0 && bmh > 0 {
            crate::SendMessageW(tb, TB_SETBITMAPSIZE, 0, ((bmh as i64) << 16) | bmw as i64);
        }
        if bw > 0 && bh > 0 {
            crate::SendMessageW(tb, TB_SETBUTTONSIZE, 0, ((bh as i64) << 16) | bw as i64);
        }
        if bitmaps > 0 {
            let add = [inst, bmp];
            crate::SendMessageW(tb, TB_ADDBITMAP, bitmaps as u64, add.as_ptr() as i64);
        }
        if !buttons.is_null() && n > 0 {
            crate::SendMessageW(tb, TB_ADDBUTTONSW, n as u64, buttons as i64);
        }
    }
    tb
}
