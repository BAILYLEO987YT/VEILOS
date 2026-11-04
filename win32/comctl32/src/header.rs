//! The header control ("SysHeader32"): a row of column titles above a list.
//! The desktop reports clicks, not drags, so columns are resized by messages
//! and clicks report HDN_ITEMCLICK.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use crate::dllrt::Lock;
use crate::Hwnd;

const HDM_GETITEMCOUNT: u32 = 0x1200;
const HDM_INSERTITEMA: u32 = 0x1201;
const HDM_DELETEITEM: u32 = 0x1202;
const HDM_GETITEMA: u32 = 0x1203;
const HDM_SETITEMA: u32 = 0x1204;
const HDM_LAYOUT: u32 = 0x1205;
const HDM_HITTEST: u32 = 0x1206;
const HDM_GETITEMRECT: u32 = 0x1207;
const HDM_SETIMAGELIST: u32 = 0x1208;
const HDM_GETIMAGELIST: u32 = 0x1209;
const HDM_INSERTITEMW: u32 = 0x120A;
const HDM_GETITEMW: u32 = 0x120B;
const HDM_SETITEMW: u32 = 0x120C;
const HDM_ORDERTOINDEX: u32 = 0x120F;
const HDM_GETORDERARRAY: u32 = 0x1211;
const HDM_SETORDERARRAY: u32 = 0x1212;

const HDI_WIDTH: u32 = 0x1;
const HDI_TEXT: u32 = 0x2;
const HDI_FORMAT: u32 = 0x4;
const HDI_LPARAM: u32 = 0x8;
const HDI_BITMAP: u32 = 0x10;
const HDI_IMAGE: u32 = 0x20;
const HDI_ORDER: u32 = 0x80;

const HDF_RIGHT: i32 = 1;
const HDF_CENTER: i32 = 2;
const HDF_IMAGE: i32 = 0x800;
const HDF_SORTUP: i32 = 0x400;
const HDF_SORTDOWN: i32 = 0x200;

const HHT_NOWHERE: u32 = 1;
const HHT_ONHEADER: u32 = 2;

const HDN_ITEMCHANGINGW: i32 = -320;
const HDN_ITEMCHANGEDW: i32 = -321;
const HDN_ITEMCLICKW: i32 = -322;
const HDN_ITEMCHANGINGA: i32 = -300;
const HDN_ITEMCHANGEDA: i32 = -301;
const HDN_ITEMCLICKA: i32 = -302;

const HDS_BUTTONS: u32 = 2;

#[derive(Clone)]
struct Item {
    width: i32,
    text: Vec<u16>,
    fmt: i32,
    param: i64,
    bitmap: u64,
    image: i32,
    order: i32,
}

#[derive(Default)]
struct Header {
    items: Vec<Item>,
    font: u64,
    images: u64,
    /// A divider being dragged: (item, where it started, its width then).
    sizing: Option<(usize, i32, i32)>,
    /// The item a click went down on.
    pressed: Option<usize>,
}

/// Near enough to a divider to grab it.
const GRIP: i32 = 4;
const HDN_BEGINTRACKW: i32 = -326;
const HDN_ENDTRACKW: i32 = -327;
const HDN_BEGINTRACKA: i32 = -306;
const HDN_ENDTRACKA: i32 = -307;

static HEADERS: Lock<BTreeMap<Hwnd, Header>> = Lock::new(BTreeMap::new());

fn with<R>(hwnd: Hwnd, f: impl FnOnce(&mut Header) -> R) -> Option<R> {
    HEADERS.lock().get_mut(&hwnd).map(f)
}

pub fn register_class() {
    crate::register("SysHeader32", header_proc as *const () as usize, true);
}

/// The height the font wants: its line and a margin (as HDM_LAYOUT reports).
fn height(hwnd: Hwnd) -> i32 {
    crate::font_height(hwnd, with(hwnd, |h| h.font).unwrap_or(0)) + 6
}

/// Items in display order, each with its rectangle.
fn rects(hwnd: Hwnd) -> Vec<(usize, [i32; 4])> {
    let mut r = [0i32; 4];
    unsafe { crate::GetClientRect(hwnd, &mut r) };
    let Some(mut order) = with(hwnd, |h| h.items.iter().enumerate().map(|(i, it)| (i, it.order, it.width)).collect::<Vec<_>>()) else {
        return Vec::new();
    };
    order.sort_by_key(|e| e.1);
    let mut x = 0;
    order
        .into_iter()
        .map(|(i, _, w)| {
            let rr = [x, 0, x + w, r[3]];
            x += w;
            (i, rr)
        })
        .collect()
}

fn paint(hwnd: Hwnd, hdc: u64) {
    let mut r = [0i32; 4];
    unsafe { crate::GetClientRect(hwnd, &mut r) };
    unsafe { crate::FillRect(hdc, &r, crate::GetSysColorBrush(15)) };
    let Some((items, font, images)) = with(hwnd, |h| (h.items.clone(), h.font, h.images)) else { return };
    let old = unsafe { crate::SelectObject(hdc, crate::font_or_default(font)) };
    unsafe {
        crate::SetBkMode(hdc, 1);
        crate::SetTextColor(hdc, crate::GetSysColor(18));
    }
    const CDRF_SKIPDEFAULT: i64 = 4;
    const CDRF_NOTIFYITEMDRAW: i64 = 0x20;
    let pre = crate::custom_draw(hwnd, 1, hdc, r, 0, 0, 0, 0, 0, 0).0;
    if pre & CDRF_SKIPDEFAULT != 0 {
        unsafe { crate::SelectObject(hdc, old) };
        return;
    }
    for (i, rr) in rects(hwnd) {
        let it = &items[i];
        if pre & CDRF_NOTIFYITEMDRAW != 0 && crate::custom_draw(hwnd, 0x1_0001, hdc, rr, i as u64, 0, it.param, 0, 0, 0).0 & CDRF_SKIPDEFAULT != 0 {
            continue;
        }
        // BDR_RAISEDOUTER | BDR_RAISEDINNER... just the right and bottom edges,
        // Windows 10's flat look.
        unsafe {
            crate::FillRect(hdc, &[rr[2] - 1, rr[1], rr[2], rr[3]], crate::GetSysColorBrush(16));
        }
        let mut tr = [rr[0] + 6, rr[1], rr[2] - 6, rr[3]];
        if it.fmt & HDF_IMAGE != 0 && images != 0 && it.image >= 0 {
            crate::imagelist::ImageList_Draw(images, it.image, hdc, tr[0], (rr[3] - 16) / 2, 0);
            tr[0] += 18;
        }
        let align = if it.fmt & 3 == HDF_RIGHT {
            0x2
        } else if it.fmt & 3 == HDF_CENTER {
            0x1
        } else {
            0
        };
        let mut t = it.text.clone();
        // DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS | DT_NOPREFIX
        unsafe { crate::DrawTextW(hdc, t.as_mut_ptr(), t.len() as i32, &mut tr, 0x20 | 0x4 | 0x8000 | 0x800 | align) };
        if it.fmt & (HDF_SORTUP | HDF_SORTDOWN) != 0 {
            // A small triangle along the top.
            let cx = (rr[0] + rr[2]) / 2;
            let up = it.fmt & HDF_SORTUP != 0;
            for k in 0..4 {
                let y = if up { k } else { 3 - k };
                unsafe { crate::FillRect(hdc, &[cx - k, y, cx + k + 1, y + 1], crate::GetSysColorBrush(16)) };
            }
        }
    }
    unsafe { crate::SelectObject(hdc, old) };
}

fn read_item(lp: i64, unicode: bool, it: &mut Item) -> u32 {
    let p = lp as *const u8;
    let (mask, cxy, text, bm, fmt, param, image, order) = unsafe {
        (
            core::ptr::read_unaligned(p as *const u32),
            core::ptr::read_unaligned(p.add(4) as *const i32),
            core::ptr::read_unaligned(p.add(8) as *const u64),
            core::ptr::read_unaligned(p.add(16) as *const u64),
            core::ptr::read_unaligned(p.add(28) as *const i32),
            core::ptr::read_unaligned(p.add(32) as *const i64),
            core::ptr::read_unaligned(p.add(40) as *const i32),
            core::ptr::read_unaligned(p.add(44) as *const i32),
        )
    };
    if mask & HDI_WIDTH != 0 {
        it.width = cxy;
    }
    if mask & HDI_TEXT != 0 && text != u64::MAX {
        it.text = crate::text_arg(text, unicode);
    }
    if mask & HDI_BITMAP != 0 {
        it.bitmap = bm;
    }
    if mask & HDI_FORMAT != 0 {
        it.fmt = fmt;
    }
    if mask & HDI_LPARAM != 0 {
        it.param = param;
    }
    if mask & HDI_IMAGE != 0 {
        it.image = image;
    }
    if mask & HDI_ORDER != 0 {
        it.order = order;
    }
    mask
}

fn write_item(lp: i64, unicode: bool, it: &Item) {
    let p = lp as *mut u8;
    unsafe {
        let mask = core::ptr::read_unaligned(p as *const u32);
        if mask & HDI_WIDTH != 0 {
            core::ptr::write_unaligned(p.add(4) as *mut i32, it.width);
        }
        if mask & HDI_TEXT != 0 {
            let dst = core::ptr::read_unaligned(p.add(8) as *const u64);
            let cap = core::ptr::read_unaligned(p.add(24) as *const i32);
            crate::put_text(dst, cap, &it.text, unicode);
        }
        if mask & HDI_BITMAP != 0 {
            core::ptr::write_unaligned(p.add(16) as *mut u64, it.bitmap);
        }
        if mask & HDI_FORMAT != 0 {
            core::ptr::write_unaligned(p.add(28) as *mut i32, it.fmt);
        }
        if mask & HDI_LPARAM != 0 {
            core::ptr::write_unaligned(p.add(32) as *mut i64, it.param);
        }
        if mask & HDI_IMAGE != 0 {
            core::ptr::write_unaligned(p.add(40) as *mut i32, it.image);
        }
        if mask & HDI_ORDER != 0 {
            core::ptr::write_unaligned(p.add(44) as *mut i32, it.order);
        }
    }
}

/// NMHEADER after the NMHDR: item, button, pointer to an HDITEM.
fn notify(hwnd: Hwnd, code_w: i32, code_a: i32, item: i32, button: i32, hd: i64) -> i64 {
    let code = if crate::parent_unicode(hwnd) { code_w } else { code_a };
    let mut extra = [0u8; 16];
    extra[0..4].copy_from_slice(&item.to_le_bytes());
    extra[4..8].copy_from_slice(&button.to_le_bytes());
    extra[8..16].copy_from_slice(&hd.to_le_bytes());
    crate::notify(hwnd, code, &extra)
}

unsafe extern "system" fn header_proc(hwnd: Hwnd, msg: u32, wp: u64, lp: i64) -> i64 {
    match msg {
        0x0001 => {
            HEADERS.lock().insert(hwnd, Header::default());
            0
        }
        0x0082 => {
            HEADERS.lock().remove(&hwnd);
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
            with(hwnd, |h| h.font = wp);
            if lp != 0 {
                unsafe { crate::InvalidateRect(hwnd, core::ptr::null(), 1) };
            }
            0
        }
        0x0031 => with(hwnd, |h| h.font as i64).unwrap_or(0),
        0x0201 => {
            let (x, y) = ((lp & 0xFFFF) as i16 as i32, ((lp >> 16) & 0xFFFF) as i16 as i32);
            let all = rects(hwnd);
            // A divider: the last one within reach, so a zero-width column can
            // be pulled out again.
            if let Some((i, r)) = all.iter().rev().find(|(_, r)| (x - r[2]).abs() <= GRIP) {
                if notify(hwnd, HDN_BEGINTRACKW, HDN_BEGINTRACKA, *i as i32, 0, 0) == 0 {
                    with(hwnd, |h| h.sizing = Some((*i, x, r[2] - r[0])));
                    unsafe { crate::SetCapture(hwnd) };
                }
                return 0;
            }
            let hit = all.iter().find(|(_, r)| x >= r[0] && x < r[2] && y >= r[1] && y < r[3]).map(|h| h.0);
            with(hwnd, |h| h.pressed = hit);
            unsafe { crate::SetCapture(hwnd) };
            0
        }
        0x0200 => {
            let x = (lp & 0xFFFF) as i16 as i32;
            if let Some(Some((i, start, width))) = with(hwnd, |h| h.sizing) {
                let w = (width + x - start).max(0);
                // Through HDM_SETITEM, so the owner hears HDN_ITEMCHANGED.
                let mut hd = [0u8; 72];
                hd[0..4].copy_from_slice(&HDI_WIDTH.to_le_bytes());
                hd[4..8].copy_from_slice(&w.to_le_bytes());
                unsafe { crate::SendMessageW(hwnd, HDM_SETITEMW, i as u64, hd.as_ptr() as i64) };
            }
            0
        }
        0x0202 => {
            let (x, y) = ((lp & 0xFFFF) as i16 as i32, ((lp >> 16) & 0xFFFF) as i16 as i32);
            unsafe { crate::ReleaseCapture() };
            let (sizing, pressed) = with(hwnd, |h| (h.sizing.take(), h.pressed.take())).unwrap_or((None, None));
            if let Some((i, _, _)) = sizing {
                notify(hwnd, HDN_ENDTRACKW, HDN_ENDTRACKA, i as i32, 0, 0);
                return 0;
            }
            let style = unsafe { crate::GetWindowLongPtrW(hwnd, -16) } as u32;
            let hit = rects(hwnd).into_iter().find(|(_, r)| x >= r[0] && x < r[2] && y >= r[1] && y < r[3]).map(|h| h.0);
            if style & HDS_BUTTONS != 0 && hit.is_some() && hit == pressed {
                notify(hwnd, HDN_ITEMCLICKW, HDN_ITEMCLICKA, hit.unwrap() as i32, 0, 0);
            }
            0
        }
        HDM_GETITEMCOUNT => with(hwnd, |h| h.items.len() as i64).unwrap_or(-1),
        HDM_INSERTITEMW | HDM_INSERTITEMA => {
            if lp == 0 {
                return -1;
            }
            let mut it = Item { width: 0, text: Vec::new(), fmt: 0, param: 0, bitmap: 0, image: 0, order: -1 };
            read_item(lp, msg == HDM_INSERTITEMW, &mut it);
            let at = with(hwnd, |h| {
                let at = (wp as i32).clamp(0, h.items.len() as i32) as usize;
                // Display orders at and after it move along.
                let order = if it.order >= 0 && (it.order as usize) <= h.items.len() { it.order } else { at as i32 };
                for o in h.items.iter_mut() {
                    if o.order >= order {
                        o.order += 1;
                    }
                }
                it.order = order;
                h.items.insert(at, it);
                at as i64
            })
            .unwrap_or(-1);
            unsafe { crate::InvalidateRect(hwnd, core::ptr::null(), 1) };
            at
        }
        HDM_DELETEITEM => {
            let ok = with(hwnd, |h| {
                let i = wp as usize;
                if i >= h.items.len() {
                    return false;
                }
                let gone = h.items.remove(i).order;
                for o in h.items.iter_mut() {
                    if o.order > gone {
                        o.order -= 1;
                    }
                }
                true
            })
            .unwrap_or(false);
            unsafe { crate::InvalidateRect(hwnd, core::ptr::null(), 1) };
            ok as i64
        }
        HDM_GETITEMW | HDM_GETITEMA => {
            let Some(it) = with(hwnd, |h| h.items.get(wp as usize).cloned()).flatten() else { return 0 };
            if lp != 0 {
                write_item(lp, msg == HDM_GETITEMW, &it);
            }
            1
        }
        HDM_SETITEMW | HDM_SETITEMA => {
            if lp == 0 {
                return 0;
            }
            let i = wp as i32;
            if notify(hwnd, HDN_ITEMCHANGINGW, HDN_ITEMCHANGINGA, i, 0, lp) != 0 {
                return 0;
            }
            let ok = with(hwnd, |h| match h.items.get_mut(i as usize) {
                Some(it) => {
                    read_item(lp, msg == HDM_SETITEMW, it);
                    true
                }
                None => false,
            })
            .unwrap_or(false);
            if !ok {
                return 0;
            }
            unsafe { crate::InvalidateRect(hwnd, core::ptr::null(), 1) };
            notify(hwnd, HDN_ITEMCHANGEDW, HDN_ITEMCHANGEDA, i, 0, lp);
            1
        }
        // HDLAYOUT: rectangle in (the space left below comes back), WINDOWPOS out.
        HDM_LAYOUT => {
            if lp == 0 {
                return 0;
            }
            let (prc, pos) = unsafe { (*(lp as *const *mut [i32; 4]), *(lp as *const *mut u8).add(1)) };
            if prc.is_null() || pos.is_null() {
                return 0;
            }
            let r = unsafe { *prc };
            let style = unsafe { crate::GetWindowLongPtrW(hwnd, -16) } as u32;
            let h = if style & 0x8 != 0 { 0 } else { height(hwnd) }; // HDS_HIDDEN
            unsafe {
                core::ptr::write_unaligned(pos as *mut u64, hwnd);
                core::ptr::write_unaligned(pos.add(8) as *mut u64, 0);
                core::ptr::write_unaligned(pos.add(16) as *mut i32, r[0]);
                core::ptr::write_unaligned(pos.add(20) as *mut i32, r[1]);
                core::ptr::write_unaligned(pos.add(24) as *mut i32, r[2] - r[0]);
                core::ptr::write_unaligned(pos.add(28) as *mut i32, h);
                core::ptr::write_unaligned(pos.add(32) as *mut u32, 0x14); // SWP_NOZORDER | SWP_NOACTIVATE
                (*prc)[1] += h;
            }
            1
        }
        // HDHITTESTINFO: pt, flags, item.
        HDM_HITTEST => {
            if lp == 0 {
                return -1;
            }
            let p = lp as *mut i32;
            let (x, y) = unsafe { (*p, *p.add(1)) };
            let hit = rects(hwnd).into_iter().find(|(_, r)| x >= r[0] && x < r[2] && y >= r[1] && y < r[3]);
            unsafe {
                *p.add(2) = if hit.is_some() { HHT_ONHEADER } else { HHT_NOWHERE } as i32;
                *p.add(3) = hit.map_or(-1, |h| h.0 as i32);
            }
            hit.map_or(-1, |h| h.0 as i64)
        }
        HDM_GETITEMRECT => {
            if lp == 0 {
                return 0;
            }
            match rects(hwnd).into_iter().find(|(i, _)| *i == wp as usize) {
                Some((_, r)) => {
                    unsafe { *(lp as *mut [i32; 4]) = r };
                    1
                }
                None => 0,
            }
        }
        HDM_SETIMAGELIST => with(hwnd, |h| core::mem::replace(&mut h.images, lp as u64) as i64).unwrap_or(0),
        HDM_GETIMAGELIST => with(hwnd, |h| h.images as i64).unwrap_or(0),
        HDM_ORDERTOINDEX => rects(hwnd).get(wp as usize).map_or(wp as i64, |e| e.0 as i64),
        HDM_GETORDERARRAY => {
            if lp == 0 {
                return 0;
            }
            let list = rects(hwnd);
            if (wp as usize) < list.len() {
                return 0;
            }
            for (k, (i, _)) in list.iter().enumerate() {
                unsafe { *(lp as *mut i32).add(k) = *i as i32 };
            }
            1
        }
        HDM_SETORDERARRAY => {
            if lp == 0 {
                return 0;
            }
            with(hwnd, |h| {
                for k in 0..(wp as usize).min(h.items.len()) {
                    let i = unsafe { *(lp as *const i32).add(k) } as usize;
                    if let Some(it) = h.items.get_mut(i) {
                        it.order = k as i32;
                    }
                }
            });
            unsafe { crate::InvalidateRect(hwnd, core::ptr::null(), 1) };
            1
        }
        _ => unsafe { crate::DefWindowProcW(hwnd, msg, wp, lp) },
    }
}
