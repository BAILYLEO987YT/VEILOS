//! "ComboBoxEx32": a combo box with extended items (images, indent, lParam).
//! The combo inside is user32's ComboBox, owner-drawn by this window, and CB_*
//! messages go to it.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use crate::dllrt::Lock;
use crate::Hwnd;

const WM_USER: u32 = 0x0400;
const CBEM_INSERTITEMA: u32 = WM_USER + 1;
const CBEM_SETIMAGELIST: u32 = WM_USER + 2;
const CBEM_GETIMAGELIST: u32 = WM_USER + 3;
const CBEM_GETITEMA: u32 = WM_USER + 4;
const CBEM_SETITEMA: u32 = WM_USER + 5;
const CBEM_GETCOMBOCONTROL: u32 = WM_USER + 6;
const CBEM_GETEDITCONTROL: u32 = WM_USER + 7;
const CBEM_SETEXSTYLE: u32 = WM_USER + 8;
const CBEM_GETEXTENDEDSTYLE: u32 = WM_USER + 9;
const CBEM_HASEDITCHANGED: u32 = WM_USER + 10;
const CBEM_INSERTITEMW: u32 = WM_USER + 11;
const CBEM_SETITEMW: u32 = WM_USER + 12;
const CBEM_GETITEMW: u32 = WM_USER + 13;
const CBEM_SETEXTENDEDSTYLE: u32 = WM_USER + 14;

const CB_DELETESTRING: u32 = 0x0144;
const CB_INSERTSTRING: u32 = 0x014A;
const CB_RESETCONTENT: u32 = 0x014B;
const CB_GETCOUNT: u32 = 0x0146;
const CB_GETLBTEXT: u32 = 0x0148;
const CB_GETLBTEXTLEN: u32 = 0x0149;
const CB_SETITEMDATA: u32 = 0x0151;
const CB_GETCURSEL: u32 = 0x0147;

const CBEIF_TEXT: u32 = 1;
const CBEIF_IMAGE: u32 = 2;
const CBEIF_SELECTEDIMAGE: u32 = 4;
const CBEIF_OVERLAY: u32 = 8;
const CBEIF_INDENT: u32 = 0x10;
const CBEIF_LPARAM: u32 = 0x20;

const CBEN_INSERTITEM: i32 = -801;
const CBEN_DELETEITEM: i32 = -802;

#[derive(Clone, Copy, Default)]
struct Extra {
    image: i32,
    selected: i32,
    overlay: i32,
    indent: i32,
    param: i64,
}

#[derive(Default)]
struct ComboEx {
    combo: Hwnd,
    items: Vec<Extra>,
    images: u64,
    ex: u32,
    font: u64,
}

static COMBOS: Lock<BTreeMap<Hwnd, ComboEx>> = Lock::new(BTreeMap::new());

fn with<R>(hwnd: Hwnd, f: impl FnOnce(&mut ComboEx) -> R) -> Option<R> {
    COMBOS.lock().get_mut(&hwnd).map(f)
}

pub fn register_class() {
    crate::register("ComboBoxEx32", comboex_proc as *const () as usize, true);
}

fn combo(hwnd: Hwnd) -> Hwnd {
    with(hwnd, |c| c.combo).unwrap_or(0)
}

/// This window as tall as the closed combo, whatever it was given.
fn fit(hwnd: Hwnd) {
    let inner = combo(hwnd);
    let (mut c, mut r) = ([0i32; 4], [0i32; 4]);
    unsafe {
        crate::GetClientRect(hwnd, &mut c);
        crate::GetWindowRect(inner, &mut r);
    }
    let h = r[3] - r[1];
    // SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE
    unsafe { crate::SetWindowPos(inner, 0, 0, 0, c[2], h, 0x2 | 0x4 | 0x10) };
    if c[3] != h {
        unsafe { crate::SetWindowPos(hwnd, 0, 0, 0, c[2], h, 0x2 | 0x4 | 0x10) };
    }
}

/// DRAWITEMSTRUCT from the combo: picture and text, indented in the list.
fn draw_item(hwnd: Hwnd, p: *const u8) {
    let (item, state, hdc, rc) = unsafe {
        (
            core::ptr::read_unaligned(p.add(8) as *const i32),
            core::ptr::read_unaligned(p.add(16) as *const u32),
            core::ptr::read_unaligned(p.add(32) as *const u64),
            core::ptr::read_unaligned(p.add(40) as *const [i32; 4]),
        )
    };
    const ODS_SELECTED: u32 = 1;
    const ODS_COMBOBOXEDIT: u32 = 0x1000;
    let selected = state & ODS_SELECTED != 0;
    let (bg, fg) = if selected { (13, 14) } else { (5, 8) };
    unsafe {
        crate::FillRect(hdc, &rc, crate::GetSysColorBrush(bg));
        crate::SetBkMode(hdc, 1);
        crate::SetTextColor(hdc, crate::GetSysColor(fg));
    }
    if item < 0 {
        return;
    }
    let Some((e, images, font)) = with(hwnd, |x| (x.items.get(item as usize).copied(), x.images, x.font)) else { return };
    let e = e.unwrap_or_default();
    let mut x = rc[0] + 2;
    if state & ODS_COMBOBOXEDIT == 0 {
        x += e.indent * 10;
    }
    if images != 0 {
        let (mut cx, mut cy) = (0, 0);
        unsafe { crate::imagelist::ImageList_GetIconSize(images, &mut cx, &mut cy) };
        let pic = if selected { e.selected } else { e.image };
        if pic >= 0 {
            unsafe { crate::imagelist::ImageList_Draw(images, pic, hdc, x, (rc[1] + rc[3] - cy) / 2, 0) };
        }
        x += cx + 4;
    }
    let inner = combo(hwnd);
    let n = unsafe { crate::SendMessageW(inner, CB_GETLBTEXTLEN, item as u64, 0) }.max(0) as usize;
    let mut t = alloc::vec![0u16; n + 1];
    unsafe { crate::SendMessageW(inner, CB_GETLBTEXT, item as u64, t.as_mut_ptr() as i64) };
    t.truncate(n);
    let old = unsafe { crate::SelectObject(hdc, crate::font_or_default(font)) };
    let mut r = [x, rc[1], rc[2], rc[3]];
    // DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX
    unsafe {
        crate::DrawTextW(hdc, t.as_mut_ptr(), t.len() as i32, &mut r, 0x20 | 0x4 | 0x800);
        crate::SelectObject(hdc, old);
    }
}

/// NMCOMBOBOXEX: the item after the NMHDR.
fn item_notify(hwnd: Hwnd, code: i32, item: i64, e: &Extra) {
    let mut n = [0u8; 64];
    n[0..4].copy_from_slice(&(CBEIF_IMAGE | CBEIF_SELECTEDIMAGE | CBEIF_OVERLAY | CBEIF_INDENT | CBEIF_LPARAM).to_le_bytes());
    n[8..16].copy_from_slice(&item.to_le_bytes());
    n[28..32].copy_from_slice(&e.image.to_le_bytes());
    n[32..36].copy_from_slice(&e.selected.to_le_bytes());
    n[36..40].copy_from_slice(&e.overlay.to_le_bytes());
    n[40..44].copy_from_slice(&e.indent.to_le_bytes());
    n[48..56].copy_from_slice(&e.param.to_le_bytes());
    crate::notify(hwnd, code, &n);
}

/// A field of COMBOBOXEXITEM.
fn read_extra(p: *const u8, mask: u32, e: &mut Extra) {
    unsafe {
        let d = |o: usize| core::ptr::read_unaligned(p.add(o) as *const i32);
        if mask & CBEIF_IMAGE != 0 {
            e.image = d(28);
        }
        if mask & CBEIF_SELECTEDIMAGE != 0 {
            e.selected = d(32);
        }
        if mask & CBEIF_OVERLAY != 0 {
            e.overlay = d(36);
        }
        if mask & CBEIF_INDENT != 0 {
            e.indent = d(40);
        }
        if mask & CBEIF_LPARAM != 0 {
            e.param = core::ptr::read_unaligned(p.add(48) as *const i64);
        }
    }
}

unsafe extern "system" fn comboex_proc(hwnd: Hwnd, msg: u32, wp: u64, lp: i64) -> i64 {
    match msg {
        0x0081 => {
            COMBOS.lock().insert(hwnd, ComboEx::default());
            unsafe { crate::DefWindowProcW(hwnd, msg, wp, lp) }
        }
        0x0001 => {
            // The combo inside, in the style asked for.
            let st = unsafe { crate::GetWindowLongPtrW(hwnd, -16) } as u32 & 0xFFFF;
            let mut c = [0i32; 4];
            unsafe { crate::GetClientRect(hwnd, &mut c) };
            let class = crate::wide("ComboBox");
            // WS_CHILD | WS_VISIBLE | WS_VSCROLL | WS_TABSTOP
            // CBS_OWNERDRAWFIXED | CBS_HASSTRINGS: we draw the images.
            let h = unsafe { crate::CreateWindowExW(0, class.as_ptr(), core::ptr::null(), 0x5021_0000 | 0x210 | st, 0, 0, c[2], c[3].max(120), hwnd, 1001, 0, 0) };
            with(hwnd, |x| x.combo = h);
            fit(hwnd);
            0
        }
        0x0082 => {
            COMBOS.lock().remove(&hwnd);
            unsafe { crate::DefWindowProcW(hwnd, msg, wp, lp) }
        }
        0x0005 => {
            let _ = lp;
            fit(hwnd);
            0
        }
        0x0007 => {
            unsafe { crate::SetFocus(combo(hwnd)) };
            0
        }
        0x0030 => {
            with(hwnd, |x| x.font = wp);
            unsafe { crate::SendMessageW(combo(hwnd), msg, wp, lp) }
        }
        0x0031 => with(hwnd, |x| x.font as i64).unwrap_or(0),
        // WM_DRAWITEM from the combo.
        0x002B => {
            draw_item(hwnd, lp as *const u8);
            1
        }
        // From the combo: to our parent, under our id.
        0x0111 => {
            let parent = unsafe { crate::GetParent(hwnd) };
            let id = unsafe { crate::GetDlgCtrlID(hwnd) } as u64;
            unsafe { crate::SendMessageW(parent, msg, (wp & 0xFFFF_0000) | id, hwnd as i64) }
        }
        CBEM_INSERTITEMW | CBEM_INSERTITEMA => {
            if lp == 0 {
                return -1;
            }
            let p = lp as *const u8;
            let mask = unsafe { core::ptr::read_unaligned(p as *const u32) };
            let at = unsafe { core::ptr::read_unaligned(p.add(8) as *const i64) };
            let text = if mask & CBEIF_TEXT != 0 { crate::text_arg(unsafe { core::ptr::read_unaligned(p.add(16) as *const u64) }, msg == CBEM_INSERTITEMW) } else { Vec::new() };
            let mut z = text;
            z.push(0);
            let i = unsafe { crate::SendMessageW(combo(hwnd), CB_INSERTSTRING, at as u64, z.as_ptr() as i64) };
            if i < 0 {
                return -1;
            }
            let mut e = Extra::default();
            read_extra(p, mask, &mut e);
            with(hwnd, |x| {
                let k = (i as usize).min(x.items.len());
                x.items.insert(k, e);
            });
            item_notify(hwnd, CBEN_INSERTITEM, i, &e);
            i
        }
        CBEM_GETITEMW | CBEM_GETITEMA => {
            if lp == 0 {
                return 0;
            }
            let p = lp as *mut u8;
            let (mut mask, mut i) = unsafe { (core::ptr::read_unaligned(p as *const u32), core::ptr::read_unaligned(p.add(8) as *const i64)) };
            if i == -1 {
                // The text in the edit box; a drop list hasn't one.
                i = unsafe { crate::SendMessageW(combo(hwnd), CB_GETCURSEL, 0, 0) };
                let st = unsafe { crate::GetWindowLongPtrW(hwnd, -16) } as u32 & 3;
                if mask & CBEIF_TEXT != 0 {
                    mask &= !CBEIF_TEXT;
                    if st != 3 {
                        let (dst, cap) = unsafe { (core::ptr::read_unaligned(p.add(16) as *const u64), core::ptr::read_unaligned(p.add(24) as *const i32)) };
                        unsafe { crate::SendMessageW(combo(hwnd), 0x000D, cap as u64, dst as i64) };
                    }
                }
                if i < 0 {
                    return 1;
                }
            }
            let Some(Some(e)) = with(hwnd, |x| x.items.get(i as usize).copied()) else { return 0 };
            unsafe {
                if mask & CBEIF_TEXT != 0 {
                    let n = crate::SendMessageW(combo(hwnd), CB_GETLBTEXTLEN, i as u64, 0).max(0) as usize;
                    let mut t = alloc::vec![0u16; n + 1];
                    crate::SendMessageW(combo(hwnd), CB_GETLBTEXT, i as u64, t.as_mut_ptr() as i64);
                    t.truncate(n);
                    crate::put_text(core::ptr::read_unaligned(p.add(16) as *const u64), core::ptr::read_unaligned(p.add(24) as *const i32), &t, msg == CBEM_GETITEMW);
                }
                let w = |o: usize, v: i32| core::ptr::write_unaligned(p.add(o) as *mut i32, v);
                if mask & CBEIF_IMAGE != 0 {
                    w(28, e.image);
                }
                if mask & CBEIF_SELECTEDIMAGE != 0 {
                    w(32, e.selected);
                }
                if mask & CBEIF_OVERLAY != 0 {
                    w(36, e.overlay);
                }
                if mask & CBEIF_INDENT != 0 {
                    w(40, e.indent);
                }
                if mask & CBEIF_LPARAM != 0 {
                    core::ptr::write_unaligned(p.add(48) as *mut i64, e.param);
                }
            }
            1
        }
        CBEM_SETITEMW | CBEM_SETITEMA => {
            if lp == 0 {
                return 0;
            }
            let p = lp as *const u8;
            let (mask, i) = unsafe { (core::ptr::read_unaligned(p as *const u32), core::ptr::read_unaligned(p.add(8) as *const i64)) };
            if with(hwnd, |x| x.items.get(i as usize).is_none()).unwrap_or(true) {
                return 0;
            }
            if mask & CBEIF_TEXT != 0 {
                let mut z = crate::text_arg(unsafe { core::ptr::read_unaligned(p.add(16) as *const u64) }, msg == CBEM_SETITEMW);
                z.push(0);
                let sel = unsafe { crate::SendMessageW(combo(hwnd), CB_GETCURSEL, 0, 0) };
                unsafe {
                    crate::SendMessageW(combo(hwnd), CB_DELETESTRING, i as u64, 0);
                    crate::SendMessageW(combo(hwnd), CB_INSERTSTRING, i as u64, z.as_ptr() as i64);
                    crate::SendMessageW(combo(hwnd), 0x014E, sel as u64, 0); // CB_SETCURSEL
                }
            }
            with(hwnd, |x| read_extra(p, mask, &mut x.items[i as usize]));
            1
        }
        CB_DELETESTRING => {
            let r = unsafe { crate::SendMessageW(combo(hwnd), msg, wp, lp) };
            if r >= 0 {
                let e = with(hwnd, |x| ((wp as usize) < x.items.len()).then(|| x.items.remove(wp as usize))).flatten();
                if let Some(e) = e {
                    item_notify(hwnd, CBEN_DELETEITEM, wp as i64, &e);
                }
            }
            r
        }
        CB_RESETCONTENT => {
            with(hwnd, |x| x.items.clear());
            unsafe { crate::SendMessageW(combo(hwnd), msg, wp, lp) }
        }
        CBEM_SETIMAGELIST => {
            let old = with(hwnd, |x| core::mem::replace(&mut x.images, lp as u64) as i64).unwrap_or(0);
            // Rows tall enough for the pictures.
            let (mut cx, mut cy) = (0, 0);
            if lp != 0 && unsafe { crate::imagelist::ImageList_GetIconSize(lp as u64, &mut cx, &mut cy) } != 0 {
                let h = (crate::font_height(hwnd, with(hwnd, |x| x.font).unwrap_or(0)) + 2).max(cy + 2);
                unsafe { crate::SendMessageW(combo(hwnd), 0x0153, 0, h as i64) };
            }
            unsafe { crate::InvalidateRect(combo(hwnd), core::ptr::null(), 1) };
            old
        }
        CBEM_GETIMAGELIST => with(hwnd, |x| x.images as i64).unwrap_or(0),
        CBEM_GETCOMBOCONTROL => combo(hwnd) as i64,
        CBEM_GETEDITCONTROL => unsafe { crate::GetWindow(combo(hwnd), 5) as i64 }, // GW_CHILD
        CBEM_HASEDITCHANGED => 0,
        CBEM_SETEXSTYLE | CBEM_SETEXTENDEDSTYLE => {
            let mask = if msg == CBEM_SETEXSTYLE || wp == 0 { u32::MAX } else { wp as u32 };
            with(hwnd, |x| {
                let old = x.ex;
                x.ex = (x.ex & !mask) | (lp as u32 & mask);
                old as i64
            })
            .unwrap_or(0)
        }
        CBEM_GETEXTENDEDSTYLE => with(hwnd, |x| x.ex as i64).unwrap_or(0),
        // The rest of the CB_ messages are the combo's.
        CB_GETCOUNT | CB_GETLBTEXT | CB_GETLBTEXTLEN | CB_SETITEMDATA | 0x0140..=0x0167 => unsafe { crate::SendMessageW(combo(hwnd), msg, wp, lp) },
        _ => unsafe { crate::DefWindowProcW(hwnd, msg, wp, lp) },
    }
}
