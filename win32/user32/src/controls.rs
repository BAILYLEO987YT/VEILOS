//! The standard controls' shared parts (their state, font, notifications,
//! colors and registration), and the Button and Static classes. Edit is in
//! edit.rs, ListBox and ComboBox in listbox.rs, ScrollBar in scroll.rs.
//!
//! They draw in Windows' classic (unthemed) look, which is what programs
//! without a common-controls 6 manifest get on Windows too.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use crate::dllrt::Lock;
use crate::message::{def_window_proc, send};
use crate::{state, Bool, Hwnd};

pub const WM_SETFONT: u32 = 0x0030;
pub const WM_GETFONT: u32 = 0x0031;
pub const WM_COMMAND: u32 = 0x0111;
pub const WM_GETDLGCODE: u32 = 0x0087;
pub const WM_CTLCOLOREDIT: u32 = 0x0133;
pub const WM_CTLCOLORLISTBOX: u32 = 0x0134;
pub const WM_CTLCOLORDLG: u32 = 0x0136;
pub const WM_CTLCOLORSTATIC: u32 = 0x0138;

pub const DLGC_WANTARROWS: i64 = 0x0001;
pub const DLGC_WANTTAB: i64 = 0x0002;
pub const DLGC_WANTALLKEYS: i64 = 0x0004;
pub const DLGC_HASSETSEL: i64 = 0x0008;
pub const DLGC_DEFPUSHBUTTON: i64 = 0x0010;
pub const DLGC_UNDEFPUSHBUTTON: i64 = 0x0020;
pub const DLGC_RADIOBUTTON: i64 = 0x0040;
pub const DLGC_WANTCHARS: i64 = 0x0080;
pub const DLGC_STATIC: i64 = 0x0100;
pub const DLGC_BUTTON: i64 = 0x2000;

/// Per-control data the classes keep: the font they draw with, and their
/// own state.
pub struct Control {
    pub font: u64,
    pub kind: Kind,
}

pub enum Kind {
    Button { check: u32, pushed: bool, focused: bool, image: u64 },
    Static { image: u64 },
    Edit(crate::edit::Edit),
    List(crate::listbox::List),
    Combo(crate::listbox::Combo),
}

static CONTROLS: Lock<BTreeMap<Hwnd, Control>> = Lock::new(BTreeMap::new());

/// Run `f` on `hwnd`'s control data, if it has any (never while calling
/// a window procedure).
pub fn with<R>(hwnd: Hwnd, f: impl FnOnce(&mut Control) -> R) -> Option<R> {
    CONTROLS.lock().get_mut(&hwnd).map(f)
}

pub fn add(hwnd: Hwnd, kind: Kind) {
    CONTROLS.lock().insert(hwnd, Control { font: 0, kind });
}

pub fn remove(hwnd: Hwnd) {
    CONTROLS.lock().remove(&hwnd);
}

pub fn font(hwnd: Hwnd) -> u64 {
    with(hwnd, |c| c.font).unwrap_or(0)
}

/// WM_COMMAND to the parent: (id, code), from `hwnd`.
pub fn notify(hwnd: Hwnd, code: u32) {
    let Some((parent, id)) = state().get(hwnd).map(|w| (w.parent, w.id)) else { return };
    if parent != 0 {
        send(parent, WM_COMMAND, (code as u64) << 16 | (id & 0xFFFF), hwnd as i64);
    }
}

/// A list or combo box whose items are strings (not owner-drawn without
/// LBS_HASSTRINGS / CBS_HASSTRINGS).
pub fn takes_strings(hwnd: Hwnd) -> bool {
    let st = style(hwnd);
    with(hwnd, |c| match c.kind {
        Kind::List(_) => st & 0x30 == 0 || st & 0x40 != 0,
        Kind::Combo(_) => st & 0x30 == 0 || st & 0x200 != 0,
        _ => false,
    })
    .unwrap_or(false)
}

/// Edit and list box controls draw a client edge instead of WS_BORDER
/// (as Windows': the style bit goes, WS_EX_CLIENTEDGE comes).
pub fn border_to_edge(hwnd: Hwnd) {
    if let Some(w) = state().get_mut(hwnd) {
        if w.style & crate::window::WS_BORDER != 0 {
            w.style &= !crate::window::WS_BORDER;
            w.ex_style |= crate::window::WS_EX_CLIENTEDGE;
        }
    }
}

pub fn style(hwnd: Hwnd) -> u32 {
    state().get(hwnd).map_or(0, |w| w.style)
}

/// The parent's WM_CTLCOLOR* answer for a control painting on `hdc`: its
/// background brush (it may also have set the DC's colors).
pub fn ctl_color(hwnd: Hwnd, hdc: u64, msg: u32) -> u64 {
    let parent = state().get(hwnd).map_or(0, |w| w.parent);
    let b = if parent != 0 { send(parent, msg, hdc, hwnd as i64) as u64 } else { 0 };
    if b != 0 { b } else { default_ctl_color(hdc, msg) }
}

/// DefWindowProc's WM_CTLCOLOR*: window colors for edits and list boxes,
/// 3D face for the rest.
pub fn default_ctl_color(hdc: u64, msg: u32) -> u64 {
    unsafe {
        crate::SetTextColor(hdc, crate::paint::GetSysColor(8));
        let bg = if matches!(msg, WM_CTLCOLOREDIT | WM_CTLCOLORLISTBOX) { 5 } else { 15 };
        crate::SetBkColor(hdc, crate::paint::GetSysColor(bg));
        crate::paint::GetSysColorBrush(bg)
    }
}

/// Select the control's font into `hdc` (the system font if it has none).
pub fn select_font(hwnd: Hwnd, hdc: u64) -> u64 {
    let f = font(hwnd);
    if f == 0 { 0 } else { unsafe { crate::SelectObject(hdc, f) } }
}

/// The common messages: font, and the control data's end.
pub fn common(hwnd: Hwnd, msg: u32, wp: u64, lp: i64) -> Option<i64> {
    match msg {
        WM_SETFONT => {
            with(hwnd, |c| c.font = wp);
            if lp != 0 {
                crate::paint::invalidate(hwnd, None, true);
            }
            Some(0)
        }
        WM_GETFONT => Some(font(hwnd) as i64),
        0x0082 => {
            // WM_NCDESTROY
            remove(hwnd);
            Some(def_window_proc(hwnd, msg, wp, lp, true))
        }
        _ => None,
    }
}

pub fn text(hwnd: Hwnd) -> Vec<u16> {
    state().get(hwnd).map(|w| w.text.clone()).unwrap_or_default()
}

pub fn client(hwnd: Hwnd) -> [i32; 4] {
    let mut r = [0i32; 4];
    crate::window::GetClientRect(hwnd, &mut r);
    r
}

pub fn fill(hdc: u64, r: [i32; 4], color: i32) {
    crate::paint::FillRect(hdc, &r, crate::paint::GetSysColorBrush(color));
}

/// A 3D edge: raised (button) or sunken (pressed).
pub fn edge(hdc: u64, r: [i32; 4], sunken: bool) {
    let (outer_tl, outer_br, inner_tl, inner_br) = if sunken { (16, 20, 21, 22) } else { (20, 21, 22, 16) };
    fill(hdc, [r[0], r[1], r[2] - 1, r[1] + 1], outer_tl);
    fill(hdc, [r[0], r[1], r[0] + 1, r[3] - 1], outer_tl);
    fill(hdc, [r[0], r[3] - 1, r[2], r[3]], outer_br);
    fill(hdc, [r[2] - 1, r[1], r[2], r[3]], outer_br);
    fill(hdc, [r[0] + 1, r[1] + 1, r[2] - 2, r[1] + 2], inner_tl);
    fill(hdc, [r[0] + 1, r[1] + 1, r[0] + 2, r[3] - 2], inner_tl);
    fill(hdc, [r[0] + 1, r[3] - 2, r[2] - 1, r[3] - 1], inner_br);
    fill(hdc, [r[2] - 2, r[1] + 1, r[2] - 1, r[3] - 1], inner_br);
}

const BF_LEFT: u32 = 0x1;
const BF_TOP: u32 = 0x2;
const BF_RIGHT: u32 = 0x4;
const BF_BOTTOM: u32 = 0x8;
const BF_MIDDLE: u32 = 0x800;
const BF_ADJUST: u32 = 0x2000;
const BF_FLAT: u32 = 0x4000;
const BF_MONO: u32 = 0x8000;

/// One pair of borders: top/left and bottom/right colours.
fn edge_ring(hdc: u64, r: &mut [i32; 4], flags: u32, tl: i32, br: i32) {
    let (tl, br) = (crate::paint::GetSysColorBrush(tl), crate::paint::GetSysColorBrush(br));
    if flags & BF_TOP != 0 {
        crate::paint::FillRect(hdc, &[r[0], r[1], r[2], r[1] + 1], tl);
        r[1] += 1;
    }
    if flags & BF_LEFT != 0 {
        crate::paint::FillRect(hdc, &[r[0], r[1], r[0] + 1, r[3]], tl);
        r[0] += 1;
    }
    if flags & BF_BOTTOM != 0 {
        crate::paint::FillRect(hdc, &[r[0], r[3] - 1, r[2], r[3]], br);
        r[3] -= 1;
    }
    if flags & BF_RIGHT != 0 {
        crate::paint::FillRect(hdc, &[r[2] - 1, r[1], r[2], r[3]], br);
        r[2] -= 1;
    }
}

/// `edge`: BDR_RAISEDOUTER 1, BDR_SUNKENOUTER 2, BDR_RAISEDINNER 4,
/// BDR_SUNKENINNER 8.
#[no_mangle]
pub unsafe extern "system" fn DrawEdge(hdc: u64, r: *mut [i32; 4], edge: u32, flags: u32) -> Bool {
    if r.is_null() {
        return 0;
    }
    let mut c = unsafe { *r };
    if flags & BF_FLAT != 0 || flags & BF_MONO != 0 {
        let color = if flags & BF_MONO != 0 { 8 } else { 16 };
        edge_ring(hdc, &mut c, flags, color, color);
        if edge & 0xC != 0 {
            edge_ring(hdc, &mut c, flags, 15, 15);
        }
    } else {
        // The outer ring, then the inner.
        match edge & 3 {
            1 => edge_ring(hdc, &mut c, flags, 22, 21),
            2 => edge_ring(hdc, &mut c, flags, 16, 20),
            _ => {}
        }
        match edge & 0xC {
            4 => edge_ring(hdc, &mut c, flags, 20, 16),
            8 => edge_ring(hdc, &mut c, flags, 21, 22),
            _ => {}
        }
    }
    if flags & BF_MIDDLE != 0 {
        crate::paint::FillRect(hdc, &c, crate::paint::GetSysColorBrush(15));
    }
    if flags & BF_ADJUST != 0 {
        unsafe { *r = c };
    }
    1
}

/// `kind`: DFC_CAPTION 1, DFC_MENU 2, DFC_SCROLL 3, DFC_BUTTON 4.
#[no_mangle]
pub unsafe extern "system" fn DrawFrameControl(hdc: u64, r: *mut [i32; 4], kind: u32, state: u32) -> Bool {
    if r.is_null() {
        return 0;
    }
    let mut c = unsafe { *r };
    let ink = crate::paint::GetSysColorBrush(if state & 0x100 != 0 { 17 } else { 18 });
    let pushed = state & 0x200 != 0;
    let checked = state & 0x400 != 0;
    let (cx, cy) = ((c[0] + c[2]) / 2, (c[1] + c[3]) / 2);
    match (kind, state & 0xFF) {
        // DFCS_BUTTONCHECK, DFCS_BUTTON3STATE: a box.
        (4, 0) | (4, 8) => {
            let b = [c[0], c[1], c[0] + 13, c[1] + 13];
            crate::paint::FillRect(hdc, &b, crate::paint::GetSysColorBrush(5));
            let mut e = b;
            edge_ring(hdc, &mut e, 0xF, 16, 16);
            if checked {
                crate::paint::FillRect(hdc, &[b[0] + 3, b[1] + 3, b[2] - 3, b[3] - 3], ink);
            }
        }
        // DFCS_BUTTONRADIO
        (4, 4) => {
            let b = [c[0] + 1, c[1] + 1, c[0] + 12, c[1] + 12];
            crate::paint::FillRect(hdc, &b, crate::paint::GetSysColorBrush(5));
            let mut e = b;
            edge_ring(hdc, &mut e, 0xF, 16, 16);
            if checked {
                crate::paint::FillRect(hdc, &[b[0] + 3, b[1] + 3, b[2] - 3, b[3] - 3], ink);
            }
        }
        _ => {
            // A button face, with what it shows.
            crate::paint::FillRect(hdc, &c, crate::paint::GetSysColorBrush(15));
            if pushed {
                edge_ring(hdc, &mut c, 0xF, 16, 20);
            } else {
                edge_ring(hdc, &mut c, 0xF, 20, 16);
            }
            let tri = |down: Option<bool>, right: Option<bool>| {
                for k in 0..4 {
                    let a = if let Some(d) = down {
                        let y = if d { cy - 2 + k } else { cy + 1 - k };
                        [cx - 3 + k, y, cx + 4 - k, y + 1]
                    } else {
                        let x = if right == Some(true) { cx - 2 + k } else { cx + 1 - k };
                        [x, cy - 3 + k, x + 1, cy + 4 - k]
                    };
                    crate::paint::FillRect(hdc, &a, ink);
                }
            };
            match (kind, state & 0xFF) {
                (3, 0) => tri(Some(false), None),
                (3, 1) | (3, 5) => tri(Some(true), None),
                (3, 2) => tri(None, Some(false)),
                (3, 3) => tri(None, Some(true)),
                // DFCS_CAPTIONCLOSE: a cross.
                (1, 0) => {
                    for k in -3..4 {
                        crate::paint::FillRect(hdc, &[cx + k, cy + k, cx + k + 1, cy + k + 1], ink);
                        crate::paint::FillRect(hdc, &[cx + k, cy - k, cx + k + 1, cy - k + 1], ink);
                    }
                }
                (1, 1) => {
                    crate::paint::FillRect(hdc, &[cx - 3, cy + 2, cx + 3, cy + 4], ink);
                }
                (1, 2) | (1, 3) => {
                    crate::paint::FillRect(hdc, &[cx - 4, cy - 4, cx + 4, cy - 2], ink);
                    crate::paint::FillRect(hdc, &[cx - 4, cy + 3, cx + 4, cy + 4], ink);
                    crate::paint::FillRect(hdc, &[cx - 4, cy - 4, cx - 3, cy + 4], ink);
                    crate::paint::FillRect(hdc, &[cx + 3, cy - 4, cx + 4, cy + 4], ink);
                }
                _ => {}
            }
        }
    }
    1
}

/// A dotted focus rectangle (DrawFocusRect).
#[no_mangle]
pub extern "system" fn DrawFocusRect(hdc: u64, r: *const [i32; 4]) -> Bool {
    if r.is_null() {
        return 0;
    }
    let r = unsafe { *r };
    for x in (r[0]..r[2]).step_by(2) {
        unsafe {
            crate::PatBlt(hdc, x, r[1], 1, 1, 0x0055_0009);
            crate::PatBlt(hdc, x, r[3] - 1, 1, 1, 0x0055_0009);
        }
    }
    for y in (r[1] + 2..r[3] - 1).step_by(2) {
        unsafe {
            crate::PatBlt(hdc, r[0], y, 1, 1, 0x0055_0009);
            crate::PatBlt(hdc, r[2] - 1, y, 1, 1, 0x0055_0009);
        }
    }
    1
}

/// Draw `text` in `r` with DrawText `flags`, grayed if disabled.
pub fn draw_label(hdc: u64, text: &[u16], r: [i32; 4], flags: u32, enabled: bool) {
    let mut rr = r;
    let mut t = text.to_vec();
    unsafe { crate::SetBkMode(hdc, 1) };
    if !enabled {
        let old = unsafe { crate::SetTextColor(hdc, crate::paint::GetSysColor(17)) };
        unsafe { crate::paint::DrawTextW(hdc, t.as_mut_ptr(), t.len() as i32, &mut rr, flags) };
        unsafe { crate::SetTextColor(hdc, old) };
    } else {
        unsafe { crate::paint::DrawTextW(hdc, t.as_mut_ptr(), t.len() as i32, &mut rr, flags) };
    }
}

// -----| registration |-----

/// The standard classes, registered when user32 loads.
pub fn register_classes() {
    type P = unsafe extern "system" fn(Hwnd, u32, u64, i64) -> i64;
    let classes: [(&str, P, u32, u64, i32); 8] = [
        ("#32768", crate::menu::popup_proc, 0x0808, 0, 0),
        ("Button", button_proc, 0x0B, 0, 0),
        ("Static", static_proc, 0x08, 0, 0),
        ("Edit", crate::edit::edit_proc, 0x0B, 0, 0),
        ("ListBox", crate::listbox::listbox_proc, 0x0B, 0, 0),
        ("ComboBox", crate::listbox::combo_proc, 0x0B, 0, 0),
        ("ComboLBox", crate::listbox::combo_list_proc, 0x0B, 0, 0),
        ("ScrollBar", crate::scroll::scrollbar_proc, 0x0B, 0, 0),
    ];
    let mut s = state();
    for (name, proc, style, brush, extra) in classes {
        let atom = s.next_atom;
        s.next_atom += 1;
        s.classes.push(crate::Class {
            name: name.encode_utf16().collect(),
            atom,
            style,
            proc: proc as *const () as usize,
            unicode: true,
            cls_extra: Vec::new(),
            wnd_extra: extra,
            instance: 0,
            icon: 0,
            icon_sm: 0,
            cursor: 0,
            brush,
            menu_name: 0,
        });
    }
    // The dialog class, with room for DWLP_MSGRESULT, DWLP_DLGPROC and
    // DWLP_USER (DLGWINDOWEXTRA).
    let atom = s.next_atom;
    s.next_atom += 1;
    s.classes.push(crate::Class {
        name: "#32770".encode_utf16().collect(),
        atom,
        style: 0x0808,
        proc: crate::dialog::def_dlg_proc_w as *const () as usize,
        unicode: true,
        cls_extra: Vec::new(),
        wnd_extra: 30,
        instance: 0,
        icon: 0,
        icon_sm: 0,
        cursor: 0,
        brush: 0,
        menu_name: 0,
    });
}

// -----| Button |-----

const BS_TYPEMASK: u32 = 0x0F;
const BS_PUSHBUTTON: u32 = 0;
const BS_DEFPUSHBUTTON: u32 = 1;
const BS_CHECKBOX: u32 = 2;
const BS_AUTOCHECKBOX: u32 = 3;
const BS_RADIOBUTTON: u32 = 4;
const BS_3STATE: u32 = 5;
const BS_AUTO3STATE: u32 = 6;
const BS_GROUPBOX: u32 = 7;
const BS_AUTORADIOBUTTON: u32 = 9;
const BS_OWNERDRAW: u32 = 0x0B;
const BS_LEFTTEXT: u32 = 0x20;
const BS_ICON: u32 = 0x40;
const BS_BITMAP: u32 = 0x80;
const BS_NOTIFY: u32 = 0x4000;

const BM_GETCHECK: u32 = 0x00F0;
const BM_SETCHECK: u32 = 0x00F1;
const BM_GETSTATE: u32 = 0x00F2;
const BM_SETSTATE: u32 = 0x00F3;
const BM_SETSTYLE: u32 = 0x00F4;
const BM_CLICK: u32 = 0x00F5;
const BM_GETIMAGE: u32 = 0x00F6;
const BM_SETIMAGE: u32 = 0x00F7;

pub const BN_CLICKED: u32 = 0;
const BN_SETFOCUS: u32 = 6;
const BN_KILLFOCUS: u32 = 7;

fn button_state(hwnd: Hwnd) -> (u32, bool, bool, u64) {
    with(hwnd, |c| match c.kind {
        Kind::Button { check, pushed, focused, image } => (check, pushed, focused, image),
        _ => (0, false, false, 0),
    })
    .unwrap_or((0, false, false, 0))
}

fn set_button(hwnd: Hwnd, f: impl FnOnce(&mut u32, &mut bool, &mut bool)) {
    with(hwnd, |c| {
        if let Kind::Button { check, pushed, focused, .. } = &mut c.kind {
            f(check, pushed, focused);
        }
    });
}

/// The buttons of `hwnd`'s group (from the WS_GROUP before it to the next).
pub fn group_of(hwnd: Hwnd) -> Vec<Hwnd> {
    let s = state();
    let Some(parent) = s.get(hwnd).map(|w| w.parent) else { return Vec::new() };
    let sib: Vec<(Hwnd, u32)> = s.windows.iter().filter(|w| w.parent == parent && !w.destroying).map(|w| (w.hwnd, w.style)).collect();
    let Some(i) = sib.iter().position(|e| e.0 == hwnd) else { return Vec::new() };
    let mut start = i;
    while start > 0 && sib[start].1 & 0x0002_0000 == 0 {
        start -= 1;
    }
    let mut end = i + 1;
    while end < sib.len() && sib[end].1 & 0x0002_0000 == 0 {
        end += 1;
    }
    sib[start..end].iter().map(|e| e.0).collect()
}

fn paint_button(hwnd: Hwnd, hdc: u64) {
    let (style, enabled) = state().get(hwnd).map_or((0, true), |w| (w.style, w.enabled));
    let kind = style & BS_TYPEMASK;
    let r = client(hwnd);
    let text = text(hwnd);
    let (check, pushed, focused, image) = button_state(hwnd);
    let old_font = select_font(hwnd, hdc);
    match kind {
        BS_OWNERDRAW => {
            // DRAWITEMSTRUCT: CtlType, CtlID, itemID, itemAction, itemState,
            // hwndItem, hDC, rcItem, itemData.
            let (parent, id) = state().get(hwnd).map_or((0, 0), |w| (w.parent, w.id));
            let mut dis = [0u8; 64];
            dis[0..4].copy_from_slice(&4u32.to_le_bytes()); // ODT_BUTTON
            dis[4..8].copy_from_slice(&(id as u32).to_le_bytes());
            dis[12..16].copy_from_slice(&1u32.to_le_bytes()); // ODA_DRAWENTIRE
            let st = (pushed as u32) | (focused as u32) << 4 | (!enabled as u32) << 2;
            dis[16..20].copy_from_slice(&st.to_le_bytes());
            dis[24..32].copy_from_slice(&hwnd.to_le_bytes());
            dis[32..40].copy_from_slice(&hdc.to_le_bytes());
            for (i, v) in r.iter().enumerate() {
                dis[40 + i * 4..44 + i * 4].copy_from_slice(&v.to_le_bytes());
            }
            send(parent, 0x002B, id, dis.as_mut_ptr() as i64); // WM_DRAWITEM
        }
        BS_GROUPBOX => {
            let brush = ctl_color(hwnd, hdc, WM_CTLCOLORSTATIC);
            crate::paint::FillRect(hdc, &r, brush);
            let mut sz = [0i32; 2];
            unsafe { crate::GetTextExtentPoint32W(hdc, [b'X' as u16].as_ptr(), 1, &mut sz) };
            let top = sz[1] / 2;
            fill(hdc, [r[0], r[1] + top, r[2] - 1, r[1] + top + 1], 16);
            fill(hdc, [r[0], r[1] + top, r[0] + 1, r[3] - 1], 16);
            fill(hdc, [r[0], r[3] - 2, r[2] - 1, r[3] - 1], 16);
            fill(hdc, [r[2] - 2, r[1] + top, r[2] - 1, r[3] - 1], 16);
            fill(hdc, [r[0] + 1, r[1] + top + 1, r[2] - 2, r[1] + top + 2], 20);
            fill(hdc, [r[0], r[3] - 1, r[2], r[3]], 20);
            fill(hdc, [r[2] - 1, r[1] + top, r[2], r[3]], 20);
            if !text.is_empty() {
                let mut tr = [r[0] + 8, r[1], r[2] - 8, r[1] + sz[1]];
                let mut t = text.clone();
                unsafe { crate::paint::DrawTextW(hdc, t.as_mut_ptr(), t.len() as i32, &mut tr, 0x420) };
                crate::paint::FillRect(hdc, &[tr[0] - 2, tr[1], tr[2] + 2, tr[3]], brush);
                draw_label(hdc, &text, [r[0] + 8, r[1], r[2] - 8, r[1] + sz[1]], 0x20, enabled);
            }
        }
        BS_CHECKBOX | BS_AUTOCHECKBOX | BS_3STATE | BS_AUTO3STATE | BS_RADIOBUTTON | BS_AUTORADIOBUTTON => {
            let brush = ctl_color(hwnd, hdc, WM_CTLCOLORSTATIC);
            crate::paint::FillRect(hdc, &r, brush);
            let radio = matches!(kind, BS_RADIOBUTTON | BS_AUTORADIOBUTTON);
            let size = 13;
            let left_text = style & BS_LEFTTEXT != 0;
            let bx = if left_text { r[2] - size } else { r[0] };
            let by = (r[1] + r[3] - size) / 2;
            let b = [bx, by, bx + size, by + size];
            if radio {
                // A round box: a sunken circle, filled, with a dot.
                for y in 0..size {
                    for x in 0..size {
                        let (dx, dy) = (x * 2 - 12, y * 2 - 12);
                        let d = dx * dx + dy * dy;
                        let c = if d > 144 { None } else if d > 100 { Some(if dx + dy < 0 { 16 } else { 20 }) } else { Some(if pushed { 15 } else { 5 }) };
                        if let Some(c) = c {
                            fill(hdc, [bx + x, by + y, bx + x + 1, by + y + 1], c);
                        }
                        if check == 1 && d <= 16 {
                            fill(hdc, [bx + x, by + y, bx + x + 1, by + y + 1], 8);
                        }
                    }
                }
            } else {
                fill(hdc, b, if pushed || check == 2 { 15 } else { 5 });
                edge(hdc, b, true);
                if check != 0 {
                    // A check mark.
                    let c = if check == 2 { 16 } else { 8 };
                    for i in 0..7 {
                        let (x, y) = (bx + 3 + i, if i < 2 { by + 6 + i } else { by + 9 - (i - 2) });
                        fill(hdc, [x, y - 1, x + 1, y + 2], c);
                    }
                }
            }
            let tr = if left_text { [r[0], r[1], bx - 4, r[3]] } else { [bx + size + 4, r[1], r[2], r[3]] };
            if image != 0 && style & BS_ICON != 0 {
                crate::image::DrawIconEx(hdc, tr[0], (tr[1] + tr[3] - 16) / 2, image, 16, 16, 0, 0, 3);
            } else {
                draw_label(hdc, &text, tr, 0x24 | if style & 0x2000 != 0 { 0x10 } else { 0 }, enabled);
            }
            if focused {
                let mut fr = tr;
                let mut t = text.clone();
                unsafe { crate::paint::DrawTextW(hdc, t.as_mut_ptr(), t.len() as i32, &mut fr, 0x424) };
                let h = fr[3] - fr[1];
                let top = (tr[1] + tr[3] - h) / 2;
                let f = [fr[0] - 1, top - 1, fr[2] + 1, top + h + 1];
                DrawFocusRect(hdc, &f);
            }
        }
        _ => {
            // A push button.
            fill(hdc, r, 15);
            let default = kind == BS_DEFPUSHBUTTON;
            let mut b = r;
            if default {
                crate::paint::FrameRect(hdc, &r, crate::paint::GetSysColorBrush(21));
                b = [r[0] + 1, r[1] + 1, r[2] - 1, r[3] - 1];
            }
            if pushed {
                crate::paint::FrameRect(hdc, &b, crate::paint::GetSysColorBrush(16));
            } else {
                edge(hdc, b, false);
            }
            let off = pushed as i32;
            if image != 0 && style & (BS_ICON | BS_BITMAP) != 0 {
                if style & BS_ICON != 0 {
                    crate::image::DrawIconEx(hdc, (r[0] + r[2] - 16) / 2 + off, (r[1] + r[3] - 16) / 2 + off, image, 16, 16, 0, 0, 3);
                }
            } else {
                draw_label(hdc, &text, [r[0] + 2 + off, r[1] + 2 + off, r[2] - 2 + off, r[3] - 2 + off], 0x25, enabled);
            }
            if focused {
                let f = [b[0] + 3, b[1] + 3, b[2] - 3, b[3] - 3];
                DrawFocusRect(hdc, &f);
            }
        }
    }
    if old_font != 0 {
        unsafe { crate::SelectObject(hdc, old_font) };
    }
}

/// A completed click: the auto styles change their check, then BN_CLICKED.
fn clicked(hwnd: Hwnd) {
    let kind = style(hwnd) & BS_TYPEMASK;
    match kind {
        BS_AUTOCHECKBOX => set_button(hwnd, |c, _, _| *c = (*c == 0) as u32),
        BS_AUTO3STATE => set_button(hwnd, |c, _, _| *c = (*c + 1) % 3),
        BS_AUTORADIOBUTTON => {
            for b in group_of(hwnd) {
                if b != hwnd && style(b) & BS_TYPEMASK == BS_AUTORADIOBUTTON {
                    send(b, BM_SETCHECK, 0, 0);
                }
            }
            set_button(hwnd, |c, _, _| *c = 1);
        }
        _ => {}
    }
    crate::paint::invalidate(hwnd, None, true);
    notify(hwnd, BN_CLICKED);
}

pub unsafe extern "system" fn button_proc(hwnd: Hwnd, msg: u32, wp: u64, lp: i64) -> i64 {
    if let Some(r) = common(hwnd, msg, wp, lp) {
        return r;
    }
    let kind = style(hwnd) & BS_TYPEMASK;
    match msg {
        0x0001 => {
            // WM_CREATE
            add(hwnd, Kind::Button { check: 0, pushed: false, focused: false, image: 0 });
            0
        }
        0x000F => {
            let mut ps = [0u8; 72];
            let hdc = crate::paint::BeginPaint(hwnd, ps.as_mut_ptr());
            paint_button(hwnd, hdc);
            crate::paint::EndPaint(hwnd, ps.as_ptr());
            0
        }
        0x0014 => 1, // WM_ERASEBKGND: painted with the button
        WM_GETDLGCODE => match kind {
            BS_PUSHBUTTON => DLGC_BUTTON | DLGC_UNDEFPUSHBUTTON,
            BS_DEFPUSHBUTTON => DLGC_BUTTON | DLGC_DEFPUSHBUTTON,
            BS_RADIOBUTTON | BS_AUTORADIOBUTTON => DLGC_BUTTON | DLGC_RADIOBUTTON,
            BS_GROUPBOX => DLGC_STATIC,
            _ => DLGC_BUTTON,
        },
        0x0201 | 0x0203 => {
            // WM_LBUTTONDOWN (and double-click): pressed, with the focus.
            if kind != BS_GROUPBOX {
                set_button(hwnd, |_, p, _| *p = true);
                crate::window::SetFocus(hwnd);
                crate::window::SetCapture(hwnd);
                crate::paint::invalidate(hwnd, None, true);
            }
            0
        }
        0x0202 => {
            // WM_LBUTTONUP: a click if it was pressed.
            let (_, pushed, _, _) = button_state(hwnd);
            crate::window::ReleaseCapture();
            if pushed {
                set_button(hwnd, |_, p, _| *p = false);
                clicked(hwnd);
            }
            0
        }
        0x0100 if wp == 0x20 => {
            // Space down: pressed.
            set_button(hwnd, |_, p, _| *p = true);
            crate::paint::invalidate(hwnd, None, true);
            0
        }
        0x0101 if wp == 0x20 => {
            let (_, pushed, _, _) = button_state(hwnd);
            if pushed {
                set_button(hwnd, |_, p, _| *p = false);
                clicked(hwnd);
            }
            0
        }
        0x0007 => {
            // WM_SETFOCUS. A radio button getting the focus by keyboard tells
            // its parent it was clicked, unless it's checked already.
            set_button(hwnd, |_, _, f| *f = true);
            let (check, pushed, _, _) = button_state(hwnd);
            if style(hwnd) & BS_NOTIFY != 0 {
                notify(hwnd, BN_SETFOCUS);
            }
            if matches!(kind, BS_RADIOBUTTON | BS_AUTORADIOBUTTON) && check == 0 && !pushed {
                notify(hwnd, BN_CLICKED);
            }
            crate::paint::invalidate(hwnd, None, true);
            0
        }
        0x0008 => {
            set_button(hwnd, |_, p, f| {
                *f = false;
                *p = false;
            });
            if style(hwnd) & BS_NOTIFY != 0 {
                notify(hwnd, BN_KILLFOCUS);
            }
            crate::paint::invalidate(hwnd, None, true);
            0
        }
        0x000A | 0x000C => {
            // WM_ENABLE, WM_SETTEXT
            let r = def_window_proc(hwnd, msg, wp, lp, true);
            crate::paint::invalidate(hwnd, None, true);
            r
        }
        BM_GETCHECK => button_state(hwnd).0 as i64,
        BM_SETCHECK => {
            let v = (wp as u32).min(if matches!(kind, BS_3STATE | BS_AUTO3STATE) { 2 } else { 1 });
            set_button(hwnd, |c, _, _| *c = v);
            crate::paint::invalidate(hwnd, None, true);
            0
        }
        BM_GETSTATE => {
            let (check, pushed, focused, _) = button_state(hwnd);
            (check | (pushed as u32) << 2 | (focused as u32) << 3) as i64
        }
        BM_SETSTATE => {
            set_button(hwnd, |_, p, _| *p = wp != 0);
            crate::paint::invalidate(hwnd, None, true);
            0
        }
        BM_SETSTYLE => {
            if let Some(w) = state().get_mut(hwnd) {
                w.style = (w.style & 0xFFFF_0000) | (wp as u32 & 0xFFFF);
            }
            if lp != 0 {
                crate::paint::invalidate(hwnd, None, true);
            }
            0
        }
        BM_CLICK => {
            // As a mouse click.
            send(hwnd, 0x0201, 1, 0);
            send(hwnd, 0x0202, 0, 0);
            0
        }
        BM_GETIMAGE => button_state(hwnd).3 as i64,
        BM_SETIMAGE => {
            let old = with(hwnd, |c| match &mut c.kind {
                Kind::Button { image, .. } => core::mem::replace(image, lp as u64),
                _ => 0,
            });
            crate::paint::invalidate(hwnd, None, true);
            old.unwrap_or(0) as i64
        }
        _ => def_window_proc(hwnd, msg, wp, lp, true),
    }
}

// -----| Static |-----

const SS_TYPEMASK: u32 = 0x1F;
const SS_CENTER: u32 = 1;
const SS_RIGHT: u32 = 2;
const SS_ICON: u32 = 3;
const SS_BLACKRECT: u32 = 4;
const SS_GRAYRECT: u32 = 5;
const SS_WHITERECT: u32 = 6;
const SS_BLACKFRAME: u32 = 7;
const SS_GRAYFRAME: u32 = 8;
const SS_WHITEFRAME: u32 = 9;
const SS_SIMPLE: u32 = 0xB;
const SS_LEFTNOWORDWRAP: u32 = 0xC;
const SS_OWNERDRAW: u32 = 0xD;
const SS_BITMAP: u32 = 0xE;
const SS_ETCHEDHORZ: u32 = 0x10;
const SS_ETCHEDVERT: u32 = 0x11;
const SS_ETCHEDFRAME: u32 = 0x12;
const SS_NOPREFIX: u32 = 0x80;
const SS_NOTIFY: u32 = 0x100;
const SS_CENTERIMAGE: u32 = 0x200;

const STM_SETICON: u32 = 0x170;
const STM_GETICON: u32 = 0x171;
const STM_SETIMAGE: u32 = 0x172;
const STM_GETIMAGE: u32 = 0x173;

fn static_image(hwnd: Hwnd) -> u64 {
    with(hwnd, |c| match c.kind {
        Kind::Static { image } => image,
        _ => 0,
    })
    .unwrap_or(0)
}

fn paint_static(hwnd: Hwnd, hdc: u64) {
    let (style, enabled) = state().get(hwnd).map_or((0, true), |w| (w.style, w.enabled));
    let kind = style & SS_TYPEMASK;
    let r = client(hwnd);
    let old_font = select_font(hwnd, hdc);
    match kind {
        SS_BLACKRECT => fill(hdc, r, 21),
        SS_GRAYRECT => fill(hdc, r, 16),
        SS_WHITERECT => fill(hdc, r, 20),
        SS_BLACKFRAME => {
            crate::paint::FrameRect(hdc, &r, crate::paint::GetSysColorBrush(21));
        }
        SS_GRAYFRAME => {
            crate::paint::FrameRect(hdc, &r, crate::paint::GetSysColorBrush(16));
        }
        SS_WHITEFRAME => {
            crate::paint::FrameRect(hdc, &r, crate::paint::GetSysColorBrush(20));
        }
        SS_ETCHEDHORZ => {
            fill(hdc, [r[0], r[1], r[2], r[1] + 1], 16);
            fill(hdc, [r[0], r[1] + 1, r[2], r[1] + 2], 20);
        }
        SS_ETCHEDVERT => {
            fill(hdc, [r[0], r[1], r[0] + 1, r[3]], 16);
            fill(hdc, [r[0] + 1, r[1], r[0] + 2, r[3]], 20);
        }
        SS_ETCHEDFRAME => {
            crate::paint::FrameRect(hdc, &r, crate::paint::GetSysColorBrush(16));
            let i = [r[0] + 1, r[1] + 1, r[2] + 1, r[3] + 1];
            crate::paint::FrameRect(hdc, &i, crate::paint::GetSysColorBrush(20));
        }
        SS_OWNERDRAW => {
            let (parent, id) = state().get(hwnd).map_or((0, 0), |w| (w.parent, w.id));
            let mut dis = [0u8; 64];
            dis[0..4].copy_from_slice(&5u32.to_le_bytes()); // ODT_STATIC
            dis[4..8].copy_from_slice(&(id as u32).to_le_bytes());
            dis[12..16].copy_from_slice(&1u32.to_le_bytes());
            dis[24..32].copy_from_slice(&hwnd.to_le_bytes());
            dis[32..40].copy_from_slice(&hdc.to_le_bytes());
            for (i, v) in r.iter().enumerate() {
                dis[40 + i * 4..44 + i * 4].copy_from_slice(&v.to_le_bytes());
            }
            send(parent, 0x002B, id, dis.as_mut_ptr() as i64);
        }
        SS_ICON | SS_BITMAP => {
            let brush = ctl_color(hwnd, hdc, WM_CTLCOLORSTATIC);
            crate::paint::FillRect(hdc, &r, brush);
            let img = static_image(hwnd);
            if img != 0 && kind == SS_ICON {
                let (x, y) = if style & SS_CENTERIMAGE != 0 { ((r[2] - 32) / 2, (r[3] - 32) / 2) } else { (0, 0) };
                crate::image::DrawIconEx(hdc, x, y, img, 0, 0, 0, 0, 3);
            } else if img != 0 {
                let mut bm = [0u8; 32];
                if unsafe { crate::GetObjectW(img, 32, bm.as_mut_ptr()) } != 0 {
                    let (w, h) = (i32::from_le_bytes(bm[4..8].try_into().unwrap()), i32::from_le_bytes(bm[8..12].try_into().unwrap()));
                    unsafe {
                        let mem = crate::CreateCompatibleDC(hdc);
                        let old = crate::SelectObject(mem, img);
                        crate::BitBlt(hdc, 0, 0, w, h, mem, 0, 0, 0x00CC_0020);
                        crate::SelectObject(mem, old);
                        crate::DeleteDC(mem);
                    }
                }
            }
        }
        _ => {
            let brush = ctl_color(hwnd, hdc, WM_CTLCOLORSTATIC);
            crate::paint::FillRect(hdc, &r, brush);
            let mut flags = match kind {
                SS_CENTER => 0x11,
                SS_RIGHT => 0x12,
                SS_SIMPLE | SS_LEFTNOWORDWRAP => 0x20,
                _ => 0x10,
            };
            if style & SS_NOPREFIX != 0 {
                flags |= 0x800;
            }
            if style & SS_CENTERIMAGE != 0 {
                flags |= 0x24;
            }
            draw_label(hdc, &text(hwnd), r, flags | 0x2000, enabled);
        }
    }
    if old_font != 0 {
        unsafe { crate::SelectObject(hdc, old_font) };
    }
}

pub unsafe extern "system" fn static_proc(hwnd: Hwnd, msg: u32, wp: u64, lp: i64) -> i64 {
    if let Some(r) = common(hwnd, msg, wp, lp) {
        return r;
    }
    let style = style(hwnd);
    match msg {
        0x0081 => {
            // WM_NCCREATE: an icon or bitmap static's text is its resource.
            add(hwnd, Kind::Static { image: 0 });
            let r = def_window_proc(hwnd, msg, wp, lp, true);
            if matches!(style & SS_TYPEMASK, SS_ICON | SS_BITMAP) && lp != 0 {
                let name = unsafe { *((lp as usize + 56) as *const u64) };
                let inst = unsafe { *((lp as usize + 8) as *const u64) };
                let res = if name >> 16 == 0 { name } else if unsafe { *(name as *const u16) } == 0xFFFF { (unsafe { *((name + 2) as *const u16) }) as u64 } else { name };
                if res != 0 {
                    let img = if style & SS_TYPEMASK == SS_ICON {
                        crate::image::LoadIconW(inst, res as *const u16)
                    } else {
                        crate::image::LoadBitmapW(inst, res as *const u16)
                    };
                    with(hwnd, |c| c.kind = Kind::Static { image: img });
                }
                crate::window::set_text(hwnd, Vec::new());
            }
            r
        }
        0x000F => {
            let mut ps = [0u8; 72];
            let hdc = crate::paint::BeginPaint(hwnd, ps.as_mut_ptr());
            paint_static(hwnd, hdc);
            crate::paint::EndPaint(hwnd, ps.as_ptr());
            0
        }
        0x0014 => 1,
        WM_GETDLGCODE => DLGC_STATIC,
        0x0084 => {
            // WM_NCHITTEST: transparent unless SS_NOTIFY.
            if style & SS_NOTIFY != 0 { 1 } else { -1 }
        }
        0x0201 if style & SS_NOTIFY != 0 => {
            notify(hwnd, 0); // STN_CLICKED
            0
        }
        0x000A | 0x000C => {
            let r = def_window_proc(hwnd, msg, wp, lp, true);
            crate::paint::invalidate(hwnd, None, true);
            r
        }
        STM_SETICON | STM_SETIMAGE => {
            let img = if msg == STM_SETICON { wp } else { lp as u64 };
            let old = with(hwnd, |c| match &mut c.kind {
                Kind::Static { image } => core::mem::replace(image, img),
                _ => 0,
            });
            crate::paint::invalidate(hwnd, None, true);
            old.unwrap_or(0) as i64
        }
        STM_GETICON | STM_GETIMAGE => static_image(hwnd) as i64,
        _ => def_window_proc(hwnd, msg, wp, lp, true),
    }
}
