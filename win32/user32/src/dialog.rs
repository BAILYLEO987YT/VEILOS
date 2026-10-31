//! Dialogs: DLGTEMPLATE / DLGTEMPLATEEX (from resources or memory), the
//! "#32770" class and DefDlgProc, modeless (CreateDialog*) and modal
//! (DialogBox*, EndDialog) dialogs, the dialog helpers, keyboard
//! navigation (IsDialogMessage: Tab, arrows in groups, Return for the
//! default button, Escape for Cancel) and MessageBox, which builds a
//! dialog of its own.

use alloc::vec::Vec;

use crate::controls::{self, WM_COMMAND, WM_GETDLGCODE, WM_SETFONT};
use crate::message::{call, def_window_proc, send};
use crate::window::{WS_CHILD, WS_VISIBLE};
use crate::{state, Bool, Hwnd};

const WM_INITDIALOG: u32 = 0x0110;
const WM_NEXTDLGCTL: u32 = 0x0028;
const DM_GETDEFID: u32 = 0x0400;
const DM_SETDEFID: u32 = 0x0401;
const DC_HASDEFID: i64 = 0x534B;

const DS_ABSALIGN: u32 = 0x01;
const DS_SETFONT: u32 = 0x40;
const DS_MODALFRAME: u32 = 0x80;
const DS_CONTROL: u32 = 0x400;
const DS_CENTER: u32 = 0x800;
const DS_CENTERMOUSE: u32 = 0x1000;

const WS_GROUP: u32 = 0x0002_0000;
const WS_TABSTOP: u32 = 0x0001_0000;
const WS_DISABLED: u32 = 0x0800_0000;

const DWLP_MSGRESULT: i32 = 0;
const DWLP_DLGPROC: i32 = 8;

/// Each dialog's dialog-manager state.
struct Dlg {
    hwnd: Hwnd,
    /// The procedure takes ANSI messages (CreateDialog*A).
    ansi: bool,
    font: u64,
    base: (i32, i32),
    def_id: u32,
    /// The control that had the focus when the dialog lost it.
    focus: Hwnd,
    /// EndDialog was called: the result.
    ended: Option<i64>,
}

static DIALOGS: crate::dllrt::Lock<Vec<Dlg>> = crate::dllrt::Lock::new(Vec::new());

fn with_dlg<R>(hwnd: Hwnd, f: impl FnOnce(&mut Dlg) -> R) -> Option<R> {
    DIALOGS.lock().iter_mut().find(|d| d.hwnd == hwnd).map(f)
}

// -----| templates |-----

struct Reader<'a> {
    d: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn u16(&mut self) -> u16 {
        let v = self.d.get(self.at..self.at + 2).map_or(0, |b| u16::from_le_bytes([b[0], b[1]]));
        self.at += 2;
        v
    }
    fn u32(&mut self) -> u32 {
        let v = self.d.get(self.at..self.at + 4).map_or(0, |b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
        self.at += 4;
        v
    }
    fn i16(&mut self) -> i32 {
        self.u16() as i16 as i32
    }
    fn align4(&mut self) {
        self.at = (self.at + 3) & !3;
    }
    /// A string or an ordinal (0xFFFF then the number); none: empty.
    fn sz_or_ord(&mut self) -> Name {
        let first = self.u16();
        match first {
            0 => Name::None,
            0xFFFF => Name::Ord(self.u16()),
            c => {
                let mut s = alloc::vec![c];
                loop {
                    let c = self.u16();
                    if c == 0 || self.at > self.d.len() {
                        break;
                    }
                    s.push(c);
                }
                Name::Text(s)
            }
        }
    }
    fn string(&mut self) -> Vec<u16> {
        let mut s = Vec::new();
        loop {
            let c = self.u16();
            if c == 0 || self.at > self.d.len() {
                return s;
            }
            s.push(c);
        }
    }
}

#[derive(Clone)]
enum Name {
    None,
    Ord(u16),
    Text(Vec<u16>),
}

struct Item {
    style: u32,
    ex: u32,
    x: i32,
    y: i32,
    cx: i32,
    cy: i32,
    id: u32,
    class: Name,
    title: Name,
    extra: Vec<u8>,
}

struct Template {
    style: u32,
    ex: u32,
    x: i32,
    y: i32,
    cx: i32,
    cy: i32,
    menu: Name,
    class: Name,
    title: Vec<u16>,
    font: Option<(i32, i32, bool, Vec<u16>)>,
    items: Vec<Item>,
}

fn parse(d: &[u8]) -> Option<Template> {
    let mut r = Reader { d, at: 0 };
    let ex_form = d.len() >= 4 && u16::from_le_bytes([d[0], d[1]]) == 1 && u16::from_le_bytes([d[2], d[3]]) == 0xFFFF;
    let (style, ex, count);
    if ex_form {
        r.at = 4;
        let _help = r.u32();
        ex = r.u32();
        style = r.u32();
    } else {
        style = r.u32();
        ex = r.u32();
    }
    count = r.u16() as usize;
    let (x, y, cx, cy) = (r.i16(), r.i16(), r.i16(), r.i16());
    let menu = r.sz_or_ord();
    let class = r.sz_or_ord();
    let title = r.string();
    let font = if style & DS_SETFONT != 0 {
        let pt = r.u16() as i32;
        let (weight, italic) = if ex_form {
            let w = r.u16() as i32;
            let it = r.d.get(r.at).copied().unwrap_or(0) != 0;
            r.at += 2;
            (w, it)
        } else {
            (400, false)
        };
        Some((pt, weight, italic, r.string()))
    } else {
        None
    };
    let mut items = Vec::with_capacity(count);
    for _ in 0..count {
        r.align4();
        let (style, ex, id);
        if ex_form {
            let _help = r.u32();
            ex = r.u32();
            style = r.u32();
        } else {
            style = r.u32();
            ex = r.u32();
        }
        let (x, y, cx, cy) = (r.i16(), r.i16(), r.i16(), r.i16());
        id = if ex_form { r.u32() } else { r.u16() as u32 };
        let class = r.sz_or_ord();
        let title = r.sz_or_ord();
        let n = r.u16() as usize;
        let extra = d.get(r.at..r.at + n).map(|e| e.to_vec()).unwrap_or_default();
        r.at += n;
        items.push(Item { style, ex, x, y, cx, cy, id, class, title, extra });
        if r.at > d.len() {
            return None;
        }
    }
    Some(Template { style, ex, x, y, cx, cy, menu, class, title, font, items })
}

/// The class a template's ordinal names.
fn class_name(n: &Name) -> Vec<u16> {
    let s = match n {
        Name::Ord(0x80) => "Button",
        Name::Ord(0x81) => "Edit",
        Name::Ord(0x82) => "Static",
        Name::Ord(0x83) => "ListBox",
        Name::Ord(0x84) => "ScrollBar",
        Name::Ord(0x85) => "ComboBox",
        Name::Text(t) => return t.clone(),
        _ => "Static",
    };
    s.encode_utf16().collect()
}

fn z(mut s: Vec<u16>) -> Vec<u16> {
    s.push(0);
    s
}

/// Dialog base units for a font: its average character width (of the
/// alphabet, as Windows measures it) and height.
fn base_units(font: u64) -> (i32, i32) {
    if font == 0 {
        return (8, 16);
    }
    let hdc = unsafe { crate::CreateCompatibleDC(0) };
    let old = unsafe { crate::SelectObject(hdc, font) };
    let abc: Vec<u16> = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz".encode_utf16().collect();
    let mut sz = [0i32; 2];
    unsafe { crate::GetTextExtentPoint32W(hdc, abc.as_ptr(), 52, &mut sz) };
    let mut tm = [0u8; 60];
    unsafe { crate::GetTextMetricsW(hdc, tm.as_mut_ptr()) };
    unsafe {
        crate::SelectObject(hdc, old);
        crate::DeleteDC(hdc);
    }
    ((sz[0] / 26 + 1) / 2, i32::from_le_bytes(tm[0..4].try_into().unwrap()))
}

fn du(v: i32, base: i32, div: i32) -> i32 {
    (v * base + div / 2) / div
}

/// Create a dialog from template `d`: the window, its font, its controls,
/// WM_INITDIALOG, the focus.
fn create(inst: u64, d: &[u8], owner: Hwnd, proc: usize, param: i64, ansi: bool) -> Hwnd {
    let Some(t) = parse(d) else {
        crate::set_error(1812); // ERROR_RESOURCE_DATA_NOT_FOUND
        return 0;
    };
    // The font, and the units it makes.
    let font = match &t.font {
        Some((pt, weight, italic, face)) => {
            let h = -(pt * 96 + 36) / 72;
            let face = z(face.clone());
            unsafe { crate::CreateFontW(h, 0, 0, 0, *weight, *italic as u32, 0, 0, 1, 0, 0, 0, 0, face.as_ptr()) }
        }
        None => 0,
    };
    let base = base_units(font);
    let child = t.style & WS_CHILD != 0;
    let mut style = t.style & !WS_VISIBLE;
    let mut ex = t.ex;
    if t.style & DS_MODALFRAME != 0 {
        ex |= 0x1; // WS_EX_DLGMODALFRAME
    }
    if t.style & DS_CONTROL != 0 {
        ex |= 0x0001_0000; // WS_EX_CONTROLPARENT
        style |= WS_CHILD;
    }
    // Client size from dialog units, the window around it.
    let (cw, ch) = (du(t.cx, base.0, 4), du(t.cy, base.1, 8));
    let mut r = [0, 0, cw, ch];
    let has_menu = !matches!(t.menu, Name::None);
    crate::window::AdjustWindowRectEx(&mut r, style, has_menu as i32, ex);
    let (w, h) = (r[2] - r[0], r[3] - r[1]);
    let (mut x, mut y) = (du(t.x, base.0, 4), du(t.y, base.1, 8));
    if t.style & (DS_CENTER | DS_CENTERMOUSE) != 0 && !child {
        let (sw, sh) = crate::screen_size();
        x = (sw - w) / 2;
        y = (sh - h) / 2;
    } else if !child && owner != 0 && t.style & DS_ABSALIGN == 0 {
        let mut p = [0i32; 2];
        crate::window::ClientToScreen(owner, &mut p);
        x += p[0];
        y += p[1];
    }
    let class = match &t.class {
        Name::None => z("#32770".encode_utf16().collect()),
        Name::Text(s) => z(s.clone()),
        Name::Ord(o) => {
            let mut v = Vec::new();
            v.push(*o);
            v
        }
    };
    let class_ptr = if let Name::Ord(o) = &t.class { *o as usize as *const u16 } else { class.as_ptr() };
    let title = z(t.title.clone());
    let hwnd = unsafe { crate::window::CreateWindowExW(ex, class_ptr, title.as_ptr(), style, x, y, w, h, owner, 0, inst, param as u64) };
    if hwnd == 0 {
        return 0;
    }
    DIALOGS.lock().push(Dlg { hwnd, ansi, font, base, def_id: 1, focus: 0, ended: None });
    crate::window::SetWindowLongPtrW(hwnd, DWLP_DLGPROC, proc as i64);
    if font != 0 {
        send(hwnd, WM_SETFONT, font, 0);
    }
    // The controls, in template order.
    for it in &t.items {
        let cls = z(class_name(&it.class));
        let text: Vec<u16> = match &it.title {
            Name::Text(s) => z(s.clone()),
            Name::Ord(o) => alloc::vec![0xFFFF, *o, 0],
            Name::None => alloc::vec![0],
        };
        let extra = if it.extra.is_empty() { 0 } else { it.extra.as_ptr() as u64 };
        let c = unsafe {
            crate::window::CreateWindowExW(
                it.ex | 0x4, // WS_EX_NOPARENTNOTIFY
                cls.as_ptr(),
                text.as_ptr(),
                it.style | WS_CHILD,
                du(it.x, base.0, 4),
                du(it.y, base.1, 8),
                du(it.cx, base.0, 4),
                du(it.cy, base.1, 8),
                hwnd,
                it.id as u64,
                inst,
                extra,
            )
        };
        if c != 0 && font != 0 {
            send(c, WM_SETFONT, font, 0);
        }
        // The default push button.
        if c != 0 && it.style & 0x0F == 1 && matches!(&it.class, Name::Ord(0x80)) {
            with_dlg(hwnd, |dl| dl.def_id = it.id);
        }
    }
    let first = GetNextDlgTabItem(hwnd, 0, 0);
    if call_dlg(hwnd, WM_INITDIALOG, first, param) != 0 && first != 0 {
        set_dlg_focus(hwnd, first);
    }
    if t.style & WS_VISIBLE != 0 && crate::window::IsWindow(hwnd) != 0 {
        crate::window::ShowWindow(hwnd, 1);
    }
    hwnd
}

/// Give a dialog control the focus as the dialog manager does: an edit's
/// text is selected.
fn set_dlg_focus(dlg: Hwnd, c: Hwnd) {
    let code = send(c, WM_GETDLGCODE, 0, 0);
    if code & controls::DLGC_HASSETSEL != 0 {
        send(c, 0x00B1, 0, -1);
    }
    crate::window::SetFocus(c);
    with_dlg(dlg, |d| d.focus = c);
}

/// The dialog procedure's answer to a message (0 if it has none).
fn call_dlg(hwnd: Hwnd, msg: u32, wp: u64, lp: i64) -> i64 {
    let proc = crate::window::GetWindowLongPtrW(hwnd, DWLP_DLGPROC);
    if proc == 0 {
        return 0;
    }
    let ansi = with_dlg(hwnd, |d| d.ansi).unwrap_or(false);
    crate::message::call_proc(proc as usize, !ansi, hwnd, msg, wp, lp, true)
}

fn template(inst: u64, name: *const u16) -> Option<&'static [u8]> {
    #[link(name = "kernel32", kind = "raw-dylib")]
    unsafe extern "system" {
        fn FindResourceW(module: u64, name: *const u16, kind: *const u16) -> u64;
        fn LoadResource(module: u64, res: u64) -> u64;
        fn LockResource(data: u64) -> *const u8;
        fn SizeofResource(module: u64, res: u64) -> u32;
        fn GetModuleHandleW(name: *const u16) -> u64;
    }
    let inst = if inst == 0 { unsafe { GetModuleHandleW(core::ptr::null()) } } else { inst };
    let res = unsafe { FindResourceW(inst, name, 5 as *const u16) }; // RT_DIALOG
    if res == 0 {
        return None;
    }
    let size = unsafe { SizeofResource(inst, res) } as usize;
    let p = unsafe { LockResource(LoadResource(inst, res)) };
    (!p.is_null()).then(|| unsafe { core::slice::from_raw_parts(p, size) })
}

unsafe fn name_arg_a(p: *const u8) -> (Vec<u16>, *const u16) {
    if (p as usize) >> 16 == 0 {
        return (Vec::new(), p as *const u16);
    }
    let w = z(unsafe { crate::from_ansi(p, -1) });
    let ptr = w.as_ptr();
    (w, ptr)
}

fn tmpl_len(t: *const u8) -> usize {
    // In-memory templates have no size: generous, parsing stops at the end.
    if t.is_null() { 0 } else { 0x10000 }
}

#[no_mangle]
pub unsafe extern "system" fn CreateDialogParamW(inst: u64, name: *const u16, owner: Hwnd, proc: usize, param: i64) -> Hwnd {
    match template(inst, name) {
        Some(d) => create(inst, d, owner, proc, param, false),
        None => 0,
    }
}

#[no_mangle]
pub unsafe extern "system" fn CreateDialogParamA(inst: u64, name: *const u8, owner: Hwnd, proc: usize, param: i64) -> Hwnd {
    let (_k, w) = unsafe { name_arg_a(name) };
    match template(inst, w) {
        Some(d) => create(inst, d, owner, proc, param, true),
        None => 0,
    }
}

#[no_mangle]
pub unsafe extern "system" fn CreateDialogIndirectParamW(inst: u64, t: *const u8, owner: Hwnd, proc: usize, param: i64) -> Hwnd {
    if t.is_null() {
        return 0;
    }
    create(inst, unsafe { core::slice::from_raw_parts(t, tmpl_len(t)) }, owner, proc, param, false)
}

#[no_mangle]
pub unsafe extern "system" fn CreateDialogIndirectParamA(inst: u64, t: *const u8, owner: Hwnd, proc: usize, param: i64) -> Hwnd {
    if t.is_null() {
        return 0;
    }
    create(inst, unsafe { core::slice::from_raw_parts(t, tmpl_len(t)) }, owner, proc, param, true)
}

/// The modal loop: run until EndDialog, then the result.
fn modal(dlg: Hwnd, owner: Hwnd) -> i64 {
    if dlg == 0 {
        return -1;
    }
    let owner_enabled = owner != 0 && crate::window::IsWindowEnabled(owner) != 0;
    if owner_enabled {
        crate::window::EnableWindow(owner, 0);
    }
    if with_dlg(dlg, |d| d.ended.is_none()).unwrap_or(false) && crate::window::IsWindowVisible(dlg) == 0 {
        crate::window::ShowWindow(dlg, 1);
    }
    let mut msg = nt_abi::Msg::default();
    let result = loop {
        if let Some(r) = with_dlg(dlg, |d| d.ended).flatten() {
            break r;
        }
        if crate::window::IsWindow(dlg) == 0 {
            break -1;
        }
        if crate::message::GetMessageW(&mut msg, 0, 0, 0) == 0 {
            // WM_QUIT ends the dialog and goes on to the outer loop.
            crate::message::PostQuitMessage(msg.wparam as i32);
            break 0;
        }
        if IsDialogMessageW(dlg, &msg) == 0 {
            crate::message::TranslateMessage(&msg);
            crate::message::DispatchMessageW(&msg);
        }
    };
    if owner_enabled {
        crate::window::EnableWindow(owner, 1);
    }
    crate::window::DestroyWindow(dlg);
    DIALOGS.lock().retain(|d| d.hwnd != dlg);
    if owner != 0 {
        crate::window::SetActiveWindow(owner);
    }
    result
}

#[no_mangle]
pub unsafe extern "system" fn DialogBoxParamW(inst: u64, name: *const u16, owner: Hwnd, proc: usize, param: i64) -> i64 {
    let dlg = unsafe { CreateDialogParamW(inst, name, owner, proc, param) };
    modal(dlg, owner)
}

#[no_mangle]
pub unsafe extern "system" fn DialogBoxParamA(inst: u64, name: *const u8, owner: Hwnd, proc: usize, param: i64) -> i64 {
    let dlg = unsafe { CreateDialogParamA(inst, name, owner, proc, param) };
    modal(dlg, owner)
}

#[no_mangle]
pub unsafe extern "system" fn DialogBoxIndirectParamW(inst: u64, t: *const u8, owner: Hwnd, proc: usize, param: i64) -> i64 {
    let dlg = unsafe { CreateDialogIndirectParamW(inst, t, owner, proc, param) };
    modal(dlg, owner)
}

#[no_mangle]
pub unsafe extern "system" fn DialogBoxIndirectParamA(inst: u64, t: *const u8, owner: Hwnd, proc: usize, param: i64) -> i64 {
    let dlg = unsafe { CreateDialogIndirectParamA(inst, t, owner, proc, param) };
    modal(dlg, owner)
}

#[no_mangle]
pub extern "system" fn EndDialog(dlg: Hwnd, result: i64) -> Bool {
    let found = with_dlg(dlg, |d| d.ended = Some(result)).is_some();
    if found {
        crate::window::ShowWindow(dlg, 0);
    }
    found as Bool
}

// -----| DefDlgProc |-----

/// Messages whose answer is the dialog procedure's return value itself,
/// not DWLP_MSGRESULT.
fn direct_answer(msg: u32) -> bool {
    matches!(msg, 0x0132..=0x0138 | 0x0039 | 0x002E | 0x002F | 0x0037 | WM_INITDIALOG | 0x004E)
}

pub fn def_dlg_proc(hwnd: Hwnd, msg: u32, wp: u64, lp: i64, unicode: bool) -> i64 {
    // The dialog procedure first.
    let proc = crate::window::GetWindowLongPtrW(hwnd, DWLP_DLGPROC);
    if proc != 0 && msg != 0x0081 && msg != 0x0083 {
        crate::window::SetWindowLongPtrW(hwnd, DWLP_MSGRESULT, 0);
        let ansi = with_dlg(hwnd, |d| d.ansi).unwrap_or(!unicode);
        let r = crate::message::call_proc(proc as usize, !ansi, hwnd, msg, wp, lp, unicode);
        if r != 0 {
            return if direct_answer(msg) { r } else { crate::window::GetWindowLongPtrW(hwnd, DWLP_MSGRESULT) };
        }
    }
    match msg {
        0x0010 => {
            // WM_CLOSE: Cancel.
            let cancel = GetDlgItem(hwnd, 2);
            if cancel == 0 || crate::window::IsWindowEnabled(cancel) != 0 {
                send(hwnd, WM_COMMAND, 2, cancel as i64);
            }
            0
        }
        0x0014 => {
            // WM_ERASEBKGND: the WM_CTLCOLORDLG brush.
            let brush = {
                let b = send(hwnd, controls::WM_CTLCOLORDLG, wp, hwnd as i64) as u64;
                if b != 0 { b } else { controls::default_ctl_color(wp, controls::WM_CTLCOLORDLG) }
            };
            let mut r = [0i32; 4];
            unsafe { crate::GetClipBox(wp, &mut r) };
            crate::paint::FillRect(wp, &r, brush);
            1
        }
        0x0132..=0x0138 => controls::default_ctl_color(wp, msg) as i64,
        DM_GETDEFID => {
            let id = with_dlg(hwnd, |d| d.def_id).unwrap_or(0);
            if id != 0 && GetDlgItem(hwnd, id as i32) != 0 { DC_HASDEFID << 16 | id as i64 } else { 0 }
        }
        DM_SETDEFID => {
            with_dlg(hwnd, |d| d.def_id = wp as u32);
            1
        }
        WM_NEXTDLGCTL => {
            let target = if lp & 0xFFFF != 0 {
                wp
            } else {
                let cur = crate::window::GetFocus();
                GetNextDlgTabItem(hwnd, cur, (wp != 0) as Bool)
            };
            if target != 0 {
                set_dlg_focus(hwnd, target);
            }
            0
        }
        0x0007 => {
            // WM_SETFOCUS: back to the control that had it.
            let f = with_dlg(hwnd, |d| d.focus).unwrap_or(0);
            if f != 0 && crate::window::IsWindow(f) != 0 {
                crate::window::SetFocus(f);
            }
            0
        }
        0x0002 => {
            // WM_DESTROY: the dialog's font goes too.
            let font = with_dlg(hwnd, |d| core::mem::replace(&mut d.font, 0)).unwrap_or(0);
            if font != 0 {
                unsafe { crate::DeleteObject(font) };
            }
            0
        }
        0x0082 => {
            DIALOGS.lock().retain(|d| d.hwnd != hwnd || d.ended.is_some());
            def_window_proc(hwnd, msg, wp, lp, unicode)
        }
        WM_SETFONT => {
            with_dlg(hwnd, |d| d.font = wp);
            0
        }
        0x0031 => with_dlg(hwnd, |d| d.font as i64).unwrap_or(0),
        _ => def_window_proc(hwnd, msg, wp, lp, unicode),
    }
}

pub unsafe extern "system" fn def_dlg_proc_w(hwnd: Hwnd, msg: u32, wp: u64, lp: i64) -> i64 {
    def_dlg_proc(hwnd, msg, wp, lp, true)
}

#[no_mangle]
pub extern "system" fn DefDlgProcW(hwnd: Hwnd, msg: u32, wp: u64, lp: i64) -> i64 {
    def_dlg_proc(hwnd, msg, wp, lp, true)
}

#[no_mangle]
pub extern "system" fn DefDlgProcA(hwnd: Hwnd, msg: u32, wp: u64, lp: i64) -> i64 {
    def_dlg_proc(hwnd, msg, wp, lp, false)
}

// -----| helpers |-----

#[no_mangle]
pub extern "system" fn GetDlgItem(dlg: Hwnd, id: i32) -> Hwnd {
    state().windows.iter().find(|w| w.parent == dlg && w.id as u32 as i32 == id && !w.destroying).map_or(0, |w| w.hwnd)
}

#[no_mangle]
pub extern "system" fn SendDlgItemMessageW(dlg: Hwnd, id: i32, msg: u32, wp: u64, lp: i64) -> i64 {
    call(GetDlgItem(dlg, id), msg, wp, lp, true)
}

#[no_mangle]
pub extern "system" fn SendDlgItemMessageA(dlg: Hwnd, id: i32, msg: u32, wp: u64, lp: i64) -> i64 {
    call(GetDlgItem(dlg, id), msg, wp, lp, false)
}

#[no_mangle]
pub unsafe extern "system" fn SetDlgItemTextW(dlg: Hwnd, id: i32, s: *const u16) -> Bool {
    unsafe { crate::window::SetWindowTextW(GetDlgItem(dlg, id), s) }
}

#[no_mangle]
pub unsafe extern "system" fn SetDlgItemTextA(dlg: Hwnd, id: i32, s: *const u8) -> Bool {
    unsafe { crate::window::SetWindowTextA(GetDlgItem(dlg, id), s) }
}

#[no_mangle]
pub unsafe extern "system" fn GetDlgItemTextW(dlg: Hwnd, id: i32, out: *mut u16, cap: i32) -> u32 {
    unsafe { crate::window::GetWindowTextW(GetDlgItem(dlg, id), out, cap) as u32 }
}

#[no_mangle]
pub unsafe extern "system" fn GetDlgItemTextA(dlg: Hwnd, id: i32, out: *mut u8, cap: i32) -> u32 {
    unsafe { crate::window::GetWindowTextA(GetDlgItem(dlg, id), out, cap) as u32 }
}

#[no_mangle]
pub extern "system" fn SetDlgItemInt(dlg: Hwnd, id: i32, v: u32, signed: Bool) -> Bool {
    let s = if signed != 0 { alloc::format!("{}", v as i32) } else { alloc::format!("{}", v) };
    let w = z(s.encode_utf16().collect());
    unsafe { crate::window::SetWindowTextW(GetDlgItem(dlg, id), w.as_ptr()) }
}

#[no_mangle]
pub unsafe extern "system" fn GetDlgItemInt(dlg: Hwnd, id: i32, ok: *mut Bool, signed: Bool) -> u32 {
    let mut buf = [0u16; 64];
    let n = unsafe { crate::window::GetWindowTextW(GetDlgItem(dlg, id), buf.as_mut_ptr(), 64) } as usize;
    let s: alloc::string::String = char::decode_utf16(buf[..n].iter().copied()).map(|c| c.unwrap_or('?')).collect();
    let t = s.trim();
    let v: Option<i64> = t.parse::<i64>().ok().filter(|v| if signed != 0 { *v >= i32::MIN as i64 && *v <= i32::MAX as i64 } else { *v >= 0 && *v <= u32::MAX as i64 });
    if !ok.is_null() {
        unsafe { *ok = v.is_some() as Bool };
    }
    v.unwrap_or(0) as u32
}

#[no_mangle]
pub extern "system" fn CheckDlgButton(dlg: Hwnd, id: i32, check: u32) -> Bool {
    send(GetDlgItem(dlg, id), 0x00F1, check as u64, 0);
    1
}

#[no_mangle]
pub extern "system" fn IsDlgButtonChecked(dlg: Hwnd, id: i32) -> u32 {
    send(GetDlgItem(dlg, id), 0x00F0, 0, 0) as u32
}

#[no_mangle]
pub extern "system" fn CheckRadioButton(dlg: Hwnd, first: i32, last: i32, check: i32) -> Bool {
    for id in first..=last {
        let c = GetDlgItem(dlg, id);
        if c != 0 {
            send(c, 0x00F1, (id == check) as u64, 0);
        }
    }
    1
}

#[no_mangle]
pub extern "system" fn GetDialogBaseUnits() -> i32 {
    // The system font's.
    16 << 16 | 8
}

#[no_mangle]
pub extern "system" fn MapDialogRect(dlg: Hwnd, r: *mut [i32; 4]) -> Bool {
    let Some(b) = with_dlg(dlg, |d| d.base) else { return 0 };
    if r.is_null() {
        return 0;
    }
    unsafe {
        let v = *r;
        *r = [du(v[0], b.0, 4), du(v[1], b.1, 8), du(v[2], b.0, 4), du(v[3], b.1, 8)];
    }
    1
}

/// The dialog's controls in order, with their styles.
fn controls_of(dlg: Hwnd) -> Vec<(Hwnd, u32)> {
    state().windows.iter().filter(|w| w.parent == dlg && !w.destroying).map(|w| (w.hwnd, w.style)).collect()
}

fn usable(style: u32) -> bool {
    style & WS_VISIBLE != 0 && style & WS_DISABLED == 0
}

/// The next (or previous) control with WS_TABSTOP after `from`, wrapping.
#[no_mangle]
pub extern "system" fn GetNextDlgTabItem(dlg: Hwnd, from: Hwnd, prev: Bool) -> Hwnd {
    let cs = controls_of(dlg);
    let n = cs.len();
    if n == 0 {
        return 0;
    }
    let start = cs.iter().position(|c| c.0 == from);
    for k in 1..=n {
        let i = match (start, prev != 0) {
            (Some(s), false) => (s + k) % n,
            (Some(s), true) => (s + n - k) % n,
            (None, false) => k - 1,
            (None, true) => n - k,
        };
        let (h, st) = cs[i];
        if usable(st) && st & WS_TABSTOP != 0 {
            return h;
        }
    }
    from
}

/// The next (or previous) usable control in `from`'s group, wrapping.
#[no_mangle]
pub extern "system" fn GetNextDlgGroupItem(dlg: Hwnd, from: Hwnd, prev: Bool) -> Hwnd {
    let cs = controls_of(dlg);
    let Some(i) = cs.iter().position(|c| c.0 == from) else { return from };
    let mut start = i;
    while start > 0 && cs[start].1 & WS_GROUP == 0 {
        start -= 1;
    }
    let mut end = i + 1;
    while end < cs.len() && cs[end].1 & WS_GROUP == 0 {
        end += 1;
    }
    let group = &cs[start..end];
    let n = group.len();
    let at = i - start;
    for k in 1..n {
        let j = if prev != 0 { (at + n - k) % n } else { (at + k) % n };
        if usable(group[j].1) {
            return group[j].0;
        }
    }
    from
}

/// The control of `dlg` that is `h` or contains it.
fn dlg_child(dlg: Hwnd, mut h: Hwnd) -> Hwnd {
    let s = state();
    while let Some(w) = s.get(h) {
        if w.parent == dlg {
            return h;
        }
        h = w.parent;
    }
    0
}

/// Keyboard navigation for messages to `dlg` or its controls.
#[no_mangle]
pub extern "system" fn IsDialogMessageW(dlg: Hwnd, m: *const nt_abi::Msg) -> Bool {
    if m.is_null() || dlg == 0 {
        return 0;
    }
    let msg = unsafe { *m };
    if msg.hwnd != dlg && dlg_child(dlg, msg.hwnd) == 0 {
        return 0;
    }
    if msg.message == 0x0100 {
        let focus = crate::window::GetFocus();
        let ctl = dlg_child(dlg, focus);
        let code = if ctl != 0 { send(ctl, WM_GETDLGCODE, msg.wparam, m as i64) } else { 0 };
        let wants = |bits: i64| code & bits != 0;
        match msg.wparam as u32 {
            0x09 if !wants(controls::DLGC_WANTTAB | controls::DLGC_WANTALLKEYS) => {
                let back = crate::keyboard::GetKeyState(0x10) < 0;
                let next = GetNextDlgTabItem(dlg, ctl, back as Bool);
                if next != 0 {
                    set_dlg_focus(dlg, next);
                }
                return 1;
            }
            0x25..=0x28 if !wants(controls::DLGC_WANTARROWS | controls::DLGC_WANTALLKEYS) && ctl != 0 => {
                let back = matches!(msg.wparam, 0x25 | 0x26);
                let next = GetNextDlgGroupItem(dlg, ctl, back as Bool);
                if next != 0 && next != ctl {
                    let ncode = send(next, WM_GETDLGCODE, 0, 0);
                    crate::window::SetFocus(next);
                    with_dlg(dlg, |d| d.focus = next);
                    // An auto radio button is clicked by arriving at it.
                    if ncode & controls::DLGC_RADIOBUTTON != 0 && controls::style(next) & 0x0F == 9 {
                        send(next, 0x00F5, 0, 0);
                    }
                }
                return 1;
            }
            0x0D if !wants(controls::DLGC_WANTALLKEYS) => {
                // The focused push button, or the default one.
                let (id, target) = if code & (controls::DLGC_DEFPUSHBUTTON | controls::DLGC_UNDEFPUSHBUTTON) != 0 && ctl != 0 {
                    (crate::window::GetDlgCtrlID(ctl) as u32, ctl)
                } else {
                    let d = send(dlg, DM_GETDEFID, 0, 0);
                    let id = if d >> 16 == DC_HASDEFID { d as u32 & 0xFFFF } else { 1 };
                    (id, GetDlgItem(dlg, id as i32))
                };
                if target == 0 || crate::window::IsWindowEnabled(target) != 0 {
                    send(dlg, WM_COMMAND, id as u64, target as i64);
                }
                return 1;
            }
            0x1B if !wants(controls::DLGC_WANTALLKEYS) => {
                let cancel = GetDlgItem(dlg, 2);
                send(dlg, WM_COMMAND, 2, cancel as i64);
                return 1;
            }
            _ => {}
        }
    }
    crate::message::TranslateMessage(m);
    crate::message::DispatchMessageW(m);
    1
}

#[no_mangle]
pub extern "system" fn IsDialogMessageA(dlg: Hwnd, m: *const nt_abi::Msg) -> Bool {
    IsDialogMessageW(dlg, m)
}

// -----| MessageBox |-----

/// MessageBox's buttons for each MB_ type: (id, label).
fn mb_buttons(kind: u32) -> &'static [(u32, &'static str)] {
    match kind & 0xF {
        1 => &[(1, "OK"), (2, "Cancel")],
        2 => &[(3, "&Abort"), (4, "&Retry"), (5, "&Ignore")],
        3 => &[(6, "&Yes"), (7, "&No"), (2, "Cancel")],
        4 => &[(6, "&Yes"), (7, "&No")],
        5 => &[(4, "&Retry"), (2, "Cancel")],
        6 => &[(2, "Cancel"), (10, "&Try Again"), (11, "&Continue")],
        _ => &[(1, "OK")],
    }
}

/// An in-memory DLGTEMPLATE for a message box: icon, text and buttons,
/// sized for the text.
fn mb_template(text: &[u16], caption: &[u16], kind: u32) -> Vec<u8> {
    let buttons = mb_buttons(kind);
    let icon = match kind & 0xF0 {
        0x10 => 32513,
        0x20 => 32514,
        0x30 => 32515,
        0x40 => 32516,
        _ => 0,
    };
    // Lay out in dialog units of an 8-point "MS Shell Dlg": measure the
    // text with that font, wrapping at about 2/3 of the screen.
    let font = unsafe {
        let f: Vec<u16> = "MS Shell Dlg\0".encode_utf16().collect();
        crate::CreateFontW(-11, 0, 0, 0, 400, 0, 0, 0, 1, 0, 0, 0, 0, f.as_ptr())
    };
    let base = base_units(font);
    let hdc = unsafe { crate::CreateCompatibleDC(0) };
    let old = unsafe { crate::SelectObject(hdc, font) };
    let (sw, _) = crate::screen_size();
    let mut r = [0, 0, sw * 2 / 3, 0];
    let mut t = text.to_vec();
    unsafe { crate::paint::DrawTextW(hdc, t.as_mut_ptr(), t.len() as i32, &mut r, 0x410 | 0x800) };
    unsafe {
        crate::SelectObject(hdc, old);
        crate::DeleteDC(hdc);
        crate::DeleteObject(font);
    }
    let to_dx = |px: i32| (px * 4 + base.0 - 1) / base.0;
    let to_dy = |px: i32| (px * 8 + base.1 - 1) / base.1;
    let (text_w, text_h) = (to_dx(r[2]).max(40), to_dy(r[3]).max(10));
    let icon_w = if icon != 0 { 32 } else { 0 };
    let bw = 50;
    let buttons_w = buttons.len() as i32 * (bw + 4);
    let width = (14 + icon_w + text_w + 14).max(buttons_w + 14);
    let body = text_h.max(if icon != 0 { 20 } else { 0 });
    let height = 14 + body + 14 + 14 + 7;
    // DLGTEMPLATE, then items.
    let mut d: Vec<u8> = Vec::new();
    let w16 = |d: &mut Vec<u8>, v: u16| d.extend_from_slice(&v.to_le_bytes());
    let w32 = |d: &mut Vec<u8>, v: u32| d.extend_from_slice(&v.to_le_bytes());
    let wstr = |d: &mut Vec<u8>, s: &[u16]| {
        for c in s {
            d.extend_from_slice(&c.to_le_bytes());
        }
        d.extend_from_slice(&[0, 0]);
    };
    let count = buttons.len() + 1 + (icon != 0) as usize;
    w32(&mut d, 0x80C8_0000 | DS_MODALFRAME | DS_CENTER | DS_SETFONT); // WS_POPUP | WS_CAPTION | WS_SYSMENU
    w32(&mut d, 0x0001_0000); // WS_EX_TOPMOST-ish: WS_EX_CONTROLPARENT
    w16(&mut d, count as u16);
    for v in [0, 0, width, height] {
        w16(&mut d, v as i16 as u16);
    }
    w16(&mut d, 0);
    w16(&mut d, 0);
    wstr(&mut d, caption);
    w16(&mut d, 8);
    wstr(&mut d, &"MS Shell Dlg".encode_utf16().collect::<Vec<u16>>());
    let item = |d: &mut Vec<u8>, style: u32, x: i32, y: i32, cx: i32, cy: i32, id: u16, class: u16, title: &[u16], ord_title: Option<u16>| {
        while d.len() % 4 != 0 {
            d.push(0);
        }
        w32(d, style | WS_VISIBLE | WS_CHILD);
        w32(d, 0);
        for v in [x, y, cx, cy] {
            w16(d, v as i16 as u16);
        }
        w16(d, id);
        w16(d, 0xFFFF);
        w16(d, class);
        match ord_title {
            Some(o) => {
                w16(d, 0xFFFF);
                w16(d, o);
            }
            None => wstr(d, title),
        }
        w16(d, 0);
    };
    if icon != 0 {
        item(&mut d, 0x3, 14, 14, 21, 20, 20, 0x82, &[], Some(icon)); // SS_ICON
    }
    item(&mut d, 0x0080, 14 + icon_w, 14, text_w, text_h, 0xFFFF, 0x82, text, None); // SS_NOPREFIX
    let default = ((kind >> 8) & 0xF) as usize;
    let mut x = width - 7 - buttons_w + 4;
    for (i, (id, label)) in buttons.iter().enumerate() {
        let def = i == default.min(buttons.len() - 1);
        let style = WS_TABSTOP | if def { 1 } else { 0 } | if i == 0 { WS_GROUP } else { 0 };
        item(&mut d, style, x, height - 21, bw, 14, *id as u16, 0x80, &label.encode_utf16().collect::<Vec<u16>>(), None);
        x += bw + 4;
    }
    d
}

static MB_CANCEL: crate::dllrt::Lock<Vec<(Hwnd, u32)>> = crate::dllrt::Lock::new(Vec::new());

unsafe extern "system" fn mb_proc(hwnd: Hwnd, msg: u32, wp: u64, _lp: i64) -> i64 {
    match msg {
        WM_INITDIALOG => {
            // The default button gets the focus.
            let d = send(hwnd, DM_GETDEFID, 0, 0);
            let id = if d >> 16 == DC_HASDEFID { d as i32 & 0xFFFF } else { 1 };
            let b = GetDlgItem(hwnd, id);
            if b != 0 {
                crate::window::SetFocus(b);
            }
            0
        }
        WM_COMMAND => {
            let id = (wp & 0xFFFF) as i32;
            // Escape (IDCANCEL) means Cancel, or OK in an OK-only box.
            let id = if id == 2 && GetDlgItem(hwnd, 2) == 0 {
                match MB_CANCEL.lock().iter().find(|e| e.0 == hwnd).map(|e| e.1) {
                    Some(0) => 1,
                    _ => return 1,
                }
            } else {
                id
            };
            EndDialog(hwnd, id as i64);
            1
        }
        _ => 0,
    }
}

fn message_box(owner: Hwnd, text: &[u16], caption: &[u16], kind: u32) -> i32 {
    let caption: Vec<u16> = if caption.is_empty() { "Error".encode_utf16().collect() } else { caption.to_vec() };
    let t = mb_template(text, &caption, kind);
    let dlg = create(0, &t, owner, mb_proc as *const () as usize, 0, false);
    MB_CANCEL.lock().push((dlg, kind & 0xF));
    let r = modal(dlg, owner);
    MB_CANCEL.lock().retain(|e| e.0 != dlg);
    r as i32
}

#[no_mangle]
pub unsafe extern "system" fn MessageBoxW(owner: Hwnd, text: *const u16, caption: *const u16, kind: u32) -> i32 {
    message_box(owner, &unsafe { crate::wstr(text) }, &unsafe { crate::wstr(caption) }, kind)
}

#[no_mangle]
pub unsafe extern "system" fn MessageBoxA(owner: Hwnd, text: *const u8, caption: *const u8, kind: u32) -> i32 {
    message_box(owner, &unsafe { crate::from_ansi(text, -1) }, &unsafe { crate::from_ansi(caption, -1) }, kind)
}

#[no_mangle]
pub unsafe extern "system" fn MessageBoxExW(owner: Hwnd, text: *const u16, caption: *const u16, kind: u32, _lang: u16) -> i32 {
    unsafe { MessageBoxW(owner, text, caption, kind) }
}

#[no_mangle]
pub unsafe extern "system" fn MessageBoxExA(owner: Hwnd, text: *const u8, caption: *const u8, kind: u32, _lang: u16) -> i32 {
    unsafe { MessageBoxA(owner, text, caption, kind) }
}
