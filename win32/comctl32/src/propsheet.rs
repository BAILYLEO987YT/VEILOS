//! Property sheets and wizards: `PropertySheet`, `CreatePropertySheetPage`.
//! The sheet is a dialog with a tab control (or, for a wizard, Back / Next /
//! Finish) and its pages are child dialogs made from each page's template,
//! created the first time they show. PSN_* notifications go to the pages
//! through WM_NOTIFY and come back through DWLP_MSGRESULT, as on Windows.
//! Not yet: wizard watermarks and header bitmaps.

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use crate::dllrt::Lock;
use crate::{Bool, Hwnd};

const PSH_PROPTITLE: u32 = 0x1;
const PSH_PROPSHEETPAGE: u32 = 0x8;
const PSH_WIZARD: u32 = 0x20;
const PSH_USEPSTARTPAGE: u32 = 0x40;
const PSH_NOAPPLYNOW: u32 = 0x80;
const PSH_USECALLBACK: u32 = 0x100;
const PSH_HASHELP: u32 = 0x200;
const PSH_MODELESS: u32 = 0x400;
const PSH_WIZARD97: u32 = 0x0100_0000;
const PSH_WIZARD_LITE: u32 = 0x0040_0000;
const PSH_AEROWIZARD: u32 = 0x4000;

const PSP_DLGINDIRECT: u32 = 0x1;
const PSP_USETITLE: u32 = 0x8;
const PSP_USECALLBACK: u32 = 0x80;

const PSCB_INITIALIZED: u32 = 1;
const PSCB_BUTTONPRESSED: u32 = 3;
const PSPCB_RELEASE: u32 = 1;
const PSPCB_CREATE: u32 = 2;
const PSPCB_ADDREF: u32 = 0;

const PSN_SETACTIVE: i32 = -200;
const PSN_KILLACTIVE: i32 = -201;
const PSN_APPLY: i32 = -202;
const PSN_RESET: i32 = -203;
const PSN_WIZBACK: i32 = -206;
const PSN_WIZNEXT: i32 = -207;
const PSN_WIZFINISH: i32 = -208;
const PSN_QUERYCANCEL: i32 = -209;

const WM_USER: u32 = 0x0400;
const PSM_SETCURSEL: u32 = WM_USER + 101;
const PSM_REMOVEPAGE: u32 = WM_USER + 102;
const PSM_ADDPAGE: u32 = WM_USER + 103;
const PSM_CHANGED: u32 = WM_USER + 104;
const PSM_RESTARTWINDOWS: u32 = WM_USER + 105;
const PSM_REBOOTSYSTEM: u32 = WM_USER + 106;
const PSM_CANCELTOCLOSE: u32 = WM_USER + 107;
const PSM_QUERYSIBLINGS: u32 = WM_USER + 108;
const PSM_UNCHANGED: u32 = WM_USER + 109;
const PSM_APPLY: u32 = WM_USER + 110;
const PSM_SETTITLEA: u32 = WM_USER + 111;
const PSM_SETWIZBUTTONS: u32 = WM_USER + 112;
const PSM_PRESSBUTTON: u32 = WM_USER + 113;
const PSM_SETCURSELID: u32 = WM_USER + 114;
const PSM_SETFINISHTEXTA: u32 = WM_USER + 115;
const PSM_GETTABCONTROL: u32 = WM_USER + 116;
const PSM_ISDIALOGMESSAGE: u32 = WM_USER + 117;
const PSM_GETCURRENTPAGEHWND: u32 = WM_USER + 118;
const PSM_INSERTPAGE: u32 = WM_USER + 119;
const PSM_SETTITLEW: u32 = WM_USER + 120;
const PSM_SETFINISHTEXTW: u32 = WM_USER + 121;
const PSM_HWNDTOINDEX: u32 = WM_USER + 129;
const PSM_INDEXTOHWND: u32 = WM_USER + 130;
const PSM_PAGETOINDEX: u32 = WM_USER + 131;
const PSM_INDEXTOPAGE: u32 = WM_USER + 132;
const PSM_IDTOINDEX: u32 = WM_USER + 133;
const PSM_INDEXTOID: u32 = WM_USER + 134;
const PSM_GETRESULT: u32 = WM_USER + 135;

const PSWIZB_BACK: u32 = 1;
const PSWIZB_NEXT: u32 = 2;
const PSWIZB_FINISH: u32 = 4;
const PSWIZB_DISABLEDFINISH: u32 = 8;

const PSBTN_BACK: u64 = 0;
const PSBTN_NEXT: u64 = 1;
const PSBTN_FINISH: u64 = 2;
const PSBTN_OK: u64 = 3;
const PSBTN_APPLYNOW: u64 = 4;
const PSBTN_CANCEL: u64 = 5;

const IDOK: u16 = 1;
const IDCANCEL: u16 = 2;
const IDHELP: u16 = 9;
const IDD_APPLY: u16 = 0x3021;
const IDD_BACK: u16 = 0x3023;
const IDD_NEXT: u16 = 0x3024;
const IDD_FINISH: u16 = 0x3025;
const IDD_TAB: u16 = 0x3020;

const TCM_INSERTITEMW: u32 = 0x133E;
const TCM_DELETEITEM: u32 = 0x1308;
const TCM_SETCURSEL: u32 = 0x130C;
const TCM_GETCURSEL: u32 = 0x130B;
const TCM_ADJUSTRECT: u32 = 0x1328;
const TCN_SELCHANGE: i32 = -551;
const TCN_SELCHANGING: i32 = -552;

const WM_NOTIFY: u32 = 0x004E;
const WM_COMMAND: u32 = 0x0111;
const WM_INITDIALOG: u32 = 0x0110;
const WM_CLOSE: u32 = 0x0010;

const DWLP_MSGRESULT: i32 = 0;
const RT_DIALOG: *const u16 = 5 as *const u16;

/// A page: a copy of its PROPSHEETPAGE (programs read their lParam from it in
/// WM_INITDIALOG).
struct Page {
    psp: Vec<u8>,
    unicode: bool,
}

impl Page {
    fn q(&self, off: usize) -> u64 {
        self.psp.get(off..off + 8).map_or(0, |b| u64::from_le_bytes(b.try_into().unwrap()))
    }
    fn flags(&self) -> u32 {
        u32::from_le_bytes(self.psp[4..8].try_into().unwrap())
    }
    fn inst(&self) -> u64 {
        self.q(8)
    }
    fn template(&self) -> u64 {
        self.q(16)
    }
    fn title(&self) -> Vec<u16> {
        let p = self.q(32);
        if self.flags() & PSP_USETITLE == 0 || p == 0 {
            // The dialog template's own caption.
            return template_of(self).map_or(Vec::new(), template_caption);
        }
        if p >> 16 == 0 {
            let mut buf = [0u16; 128];
            let n = unsafe { crate::LoadStringW(self.inst(), p as u32, buf.as_mut_ptr(), 128) }.max(0) as usize;
            return buf[..n].to_vec();
        }
        crate::text_arg(p, self.unicode)
    }
    fn dlg_proc(&self) -> usize {
        self.q(40) as usize
    }
    fn callback(&self) -> usize {
        if self.flags() & PSP_USECALLBACK != 0 { self.q(56) as usize } else { 0 }
    }
    /// PFNPROPSHEETCALLBACK(hwnd, msg, ppsp)
    fn call(&self, hwnd: Hwnd, msg: u32) -> u32 {
        let cb = self.callback();
        if cb == 0 {
            return 1;
        }
        let f: unsafe extern "system" fn(Hwnd, u32, *const u8) -> u32 = unsafe { core::mem::transmute(cb) };
        unsafe { f(hwnd, msg, self.psp.as_ptr()) }
    }
}

fn read_page(p: *const u8, unicode: bool) -> u64 {
    let size = unsafe { core::ptr::read_unaligned(p as *const u32) }.clamp(64, 4096) as usize;
    let psp = unsafe { core::slice::from_raw_parts(p, size) }.to_vec();
    let page = Box::new(Page { psp, unicode });
    page.call(0, PSPCB_ADDREF);
    Box::into_raw(page) as u64
}

fn page_of<'a>(h: u64) -> Option<&'a Page> {
    if h == 0 { None } else { Some(unsafe { &*(h as *const Page) }) }
}

#[no_mangle]
pub unsafe extern "system" fn CreatePropertySheetPageW(p: *const u8) -> u64 {
    if p.is_null() { 0 } else { read_page(p, true) }
}

#[no_mangle]
pub unsafe extern "system" fn CreatePropertySheetPageA(p: *const u8) -> u64 {
    if p.is_null() { 0 } else { read_page(p, false) }
}

#[no_mangle]
pub extern "system" fn DestroyPropertySheetPage(h: u64) -> Bool {
    let Some(page) = page_of(h) else { return 0 };
    page.call(0, PSPCB_RELEASE);
    drop(unsafe { Box::from_raw(h as *mut Page) });
    1
}

// -----| the sheet |-----

struct PageSlot {
    page: u64,
    hwnd: Hwnd,
}

struct Sheet {
    pages: Vec<PageSlot>,
    current: i32,
    wizard: bool,
    modeless: bool,
    changed: Vec<Hwnd>,
    /// PropertySheet's return value when it ends.
    result: i64,
    applied: bool,
    /// The largest page, in pixels, and where pages go.
    page_rect: [i32; 4],
    callback: usize,
    no_apply: bool,
    title_prop: bool,
    title: Vec<u16>,
    closed: bool,
    start: i32,
}

static SHEETS: Lock<BTreeMap<Hwnd, Sheet>> = Lock::new(BTreeMap::new());

fn with<R>(hwnd: Hwnd, f: impl FnOnce(&mut Sheet) -> R) -> Option<R> {
    SHEETS.lock().get_mut(&hwnd).map(f)
}

/// A page's template bytes.
fn template_of(page: &Page) -> Option<&'static [u8]> {
    let t = page.template();
    if page.flags() & PSP_DLGINDIRECT != 0 {
        if t == 0 {
            return None;
        }
        // A template in memory: its size isn't given; parse enough for a copy.
        let size = template_len(t as *const u8);
        return Some(unsafe { core::slice::from_raw_parts(t as *const u8, size) });
    }
    unsafe {
        let res = crate::FindResourceW(page.inst(), t as *const u16, RT_DIALOG);
        if res == 0 {
            return None;
        }
        let h = crate::LoadResource(page.inst(), res);
        let p = crate::LockResource(h);
        if p.is_null() {
            return None;
        }
        Some(core::slice::from_raw_parts(p, crate::SizeofResource(page.inst(), res) as usize))
    }
}

/// Skip a template string or ordinal: the offset after it.
fn skip_sz(p: *const u8, mut at: usize) -> usize {
    unsafe {
        let first = core::ptr::read_unaligned(p.add(at) as *const u16);
        if first == 0xFFFF {
            return at + 4;
        }
        while core::ptr::read_unaligned(p.add(at) as *const u16) != 0 {
            at += 2;
        }
        at + 2
    }
}

/// The length of a dialog template in memory.
fn template_len(p: *const u8) -> usize {
    unsafe {
        let d = |o: usize| core::ptr::read_unaligned(p.add(o) as *const u32);
        let w = |o: usize| core::ptr::read_unaligned(p.add(o) as *const u16);
        let ex = w(0) == 1 && w(2) == 0xFFFF;
        let (style, count, mut at) = if ex { (d(12), w(16) as usize, 26) } else { (d(0), w(8) as usize, 18) };
        at = skip_sz(p, at); // menu
        at = skip_sz(p, at); // class
        at = skip_sz(p, at); // title
        if style & 0x40 != 0 {
            at += if ex { 6 } else { 2 };
            at = skip_sz(p, at);
        }
        for _ in 0..count {
            at = (at + 3) & !3;
            at += if ex { 24 } else { 18 };
            at = skip_sz(p, at); // class
            at = skip_sz(p, at); // title
            let extra = w(at) as usize;
            at += 2 + extra;
        }
        at
    }
}

/// A template's caption.
fn template_caption(t: &[u8]) -> Vec<u16> {
    if t.len() < 18 {
        return Vec::new();
    }
    let p = t.as_ptr();
    let ex = t[0] == 1 && t[1] == 0 && t[2] == 0xFF && t[3] == 0xFF;
    let mut at = if ex { 26 } else { 18 };
    at = skip_sz(p, at); // menu
    at = skip_sz(p, at); // class
    crate::wstr(unsafe { p.add(at) } as *const u16)
}

/// (style, cx, cy in dialog units) of a template.
fn template_size(t: &[u8]) -> (i32, i32) {
    if t.len() < 18 {
        return (212, 188);
    }
    let w = |o: usize| i16::from_le_bytes([t[o], t[o + 1]]) as i32;
    let ex = w(0) == 1 && w(2) == -1;
    if ex { (w(22), w(24)) } else { (w(14), w(16)) }
}

/// A copy of the page's template that makes a child window.
fn child_template(t: &[u8]) -> Vec<u8> {
    let mut c = t.to_vec();
    if c.len() < 16 {
        return c;
    }
    let ex = c[0] == 1 && c[1] == 0 && c[2] == 0xFF && c[3] == 0xFF;
    let at = if ex { 12 } else { 0 };
    let mut style = u32::from_le_bytes(c[at..at + 4].try_into().unwrap());
    // Off: WS_POPUP, WS_CAPTION, WS_SYSMENU, WS_THICKFRAME, DS_MODALFRAME,
    // WS_VISIBLE, WS_BORDER, DS_CENTER. On: WS_CHILD, DS_CONTROL.
    style &= !(0x8000_0000 | 0x00C0_0000 | 0x0008_0000 | 0x0004_0000 | 0x80 | 0x1000_0000 | 0x800);
    style |= 0x4000_0000 | 0x400;
    c[at..at + 4].copy_from_slice(&style.to_le_bytes());
    // And WS_EX_CONTROLPARENT, so Tab reaches its controls.
    let ex_at = if ex { 8 } else { 4 };
    let exs = u32::from_le_bytes(c[ex_at..ex_at + 4].try_into().unwrap()) | 0x0001_0000;
    c[ex_at..ex_at + 4].copy_from_slice(&exs.to_le_bytes());
    c
}

/// The page's dialog, made the first time it's wanted.
fn ensure_page(sheet: Hwnd, i: usize) -> Hwnd {
    let Some(Some((existing, h))) = with(sheet, |s| s.pages.get(i).map(|p| (p.hwnd, p.page))) else { return 0 };
    if existing != 0 {
        return existing;
    }
    let Some(page) = page_of(h) else { return 0 };
    if page.call(0, PSPCB_CREATE) == 0 {
        return 0;
    }
    let Some(t) = template_of(page) else { return 0 };
    let c = child_template(t);
    let param = page.psp.as_ptr() as i64;
    let hwnd = unsafe {
        if page.unicode {
            crate::CreateDialogIndirectParamW(page.inst(), c.as_ptr(), sheet, page.dlg_proc(), param)
        } else {
            crate::CreateDialogIndirectParamA(page.inst(), c.as_ptr(), sheet, page.dlg_proc(), param)
        }
    };
    if hwnd == 0 {
        return 0;
    }
    let r = with(sheet, |s| {
        if let Some(p) = s.pages.get_mut(i) {
            p.hwnd = hwnd;
        }
        s.page_rect
    })
    .unwrap_or([0; 4]);
    // SWP_NOZORDER | SWP_NOACTIVATE
    unsafe { crate::SetWindowPos(hwnd, 0, r[0], r[1], r[2] - r[0], r[3] - r[1], 0x14) };
    hwnd
}

/// Tell a page something: its answer through DWLP_MSGRESULT.
fn send_psn(page: Hwnd, sheet: Hwnd, code: i32, lp: i64) -> i64 {
    let id = unsafe { crate::GetDlgCtrlID(page) } as u64;
    let mut n = [0u8; 32];
    n[0..8].copy_from_slice(&sheet.to_le_bytes());
    n[8..16].copy_from_slice(&id.to_le_bytes());
    n[16..20].copy_from_slice(&(code as u32).to_le_bytes());
    n[24..32].copy_from_slice(&lp.to_le_bytes());
    unsafe { crate::SetWindowLongPtrW(page, DWLP_MSGRESULT, 0) };
    unsafe { crate::SendMessageW(page, WM_NOTIFY, id, n.as_mut_ptr() as i64) };
    unsafe { crate::GetWindowLongPtrW(page, DWLP_MSGRESULT) }
}

fn current_page(sheet: Hwnd) -> Hwnd {
    with(sheet, |s| s.pages.get(s.current.max(0) as usize).map_or(0, |p| p.hwnd)).unwrap_or(0)
}

fn dlg_ids(sheet: Hwnd, id: u16) -> Hwnd {
    unsafe { crate::GetDlgItem(sheet, id as i32) }
}

/// Move to page `i`. `check`: the current page may refuse (PSN_KILLACTIVE).
/// A page refusing PSN_SETACTIVE with -1 is skipped in `dir`; another value
/// is the dialog id to go to instead.
fn go_to(sheet: Hwnd, i: i32, check: bool, dir: i32) -> bool {
    let n = with(sheet, |s| s.pages.len() as i32).unwrap_or(0);
    if i < 0 || i >= n {
        return false;
    }
    let old = current_page(sheet);
    if check && old != 0 && send_psn(old, sheet, PSN_KILLACTIVE, 0) != 0 {
        return false;
    }
    let mut at = i;
    let page = loop {
        let p = ensure_page(sheet, at as usize);
        if p == 0 {
            return false;
        }
        match send_psn(p, sheet, PSN_SETACTIVE, 0) {
            0 => break p,
            -1 => {
                at += dir.signum().max(if dir == 0 { 1 } else { -1 });
                if at < 0 || at >= n {
                    return false;
                }
            }
            id => {
                let Some(k) = find_id(sheet, id) else { return false };
                at = k;
            }
        }
    };
    if old != 0 && old != page {
        unsafe { crate::ShowWindow(old, 0) };
    }
    with(sheet, |s| s.current = at);
    unsafe { crate::ShowWindow(page, 5) };
    let tab = dlg_ids(sheet, IDD_TAB);
    if tab != 0 {
        unsafe { crate::SendMessageW(tab, TCM_SETCURSEL, at as u64, 0) };
    }
    // The title follows the page in a wizard.
    let wizard = with(sheet, |s| s.wizard).unwrap_or(false);
    if wizard {
        if let Some(pg) = with(sheet, |s| s.pages[at as usize].page).and_then(page_of) {
            let mut t = pg.title();
            if !t.is_empty() {
                t.push(0);
                unsafe { crate::SetWindowTextW(sheet, t.as_ptr()) };
            }
        }
    }
    true
}

/// The index of the page whose template is dialog `id`.
fn find_id(sheet: Hwnd, id: i64) -> Option<i32> {
    with(sheet, |s| {
        s.pages.iter().position(|p| page_of(p.page).is_some_and(|g| g.template() == id as u64)).map(|i| i as i32)
    })
    .flatten()
}

/// OK / Apply: every made page gets PSN_APPLY. False if one wants to stay.
fn apply(sheet: Hwnd, closing: bool) -> bool {
    let cur = current_page(sheet);
    if cur != 0 && send_psn(cur, sheet, PSN_KILLACTIVE, 0) != 0 {
        return false;
    }
    let pages: Vec<Hwnd> = with(sheet, |s| s.pages.iter().map(|p| p.hwnd).filter(|&h| h != 0).collect()).unwrap_or_default();
    for p in pages {
        // PSN_APPLY's lParam: whether this is OK rather than Apply.
        match send_psn(p, sheet, PSN_APPLY, closing as i64) {
            0 | 3 => {}
            // PSNRET_INVALID (1) stays where it is; INVALID_NOCHANGEPAGE (2) too.
            _ => return false,
        }
    }
    with(sheet, |s| {
        s.changed.clear();
        s.applied = true;
    });
    let a = dlg_ids(sheet, IDD_APPLY);
    if a != 0 {
        unsafe { crate::EnableWindow(a, 0) };
    }
    if !closing && cur != 0 {
        send_psn(cur, sheet, PSN_SETACTIVE, 0);
    }
    true
}

fn finish(sheet: Hwnd, result: i64) {
    let modeless = with(sheet, |s| {
        s.result = result;
        s.closed = true;
        s.modeless
    })
    .unwrap_or(false);
    if !modeless {
        unsafe { crate::EndDialog(sheet, result) };
    }
}

fn cancel(sheet: Hwnd) {
    let cur = current_page(sheet);
    if cur != 0 && send_psn(cur, sheet, PSN_QUERYCANCEL, 0) != 0 {
        return;
    }
    let pages: Vec<Hwnd> = with(sheet, |s| s.pages.iter().map(|p| p.hwnd).filter(|&h| h != 0).collect()).unwrap_or_default();
    for p in pages {
        send_psn(p, sheet, PSN_RESET, 0);
    }
    let applied = with(sheet, |s| s.applied).unwrap_or(false);
    finish(sheet, applied as i64);
}

/// Back, Next and Finish in a wizard: the page may name where to go.
fn wizard_step(sheet: Hwnd, code: i32, dir: i32) {
    let cur = current_page(sheet);
    let r = if cur != 0 { send_psn(cur, sheet, code, 0) } else { 0 };
    if r == -1 {
        return;
    }
    let at = with(sheet, |s| s.current).unwrap_or(0);
    let to = if r != 0 { find_id(sheet, r).unwrap_or(at + dir) } else { at + dir };
    go_to(sheet, to, true, dir);
}

fn press(sheet: Hwnd, button: u64) {
    let id = match button {
        PSBTN_BACK => IDD_BACK,
        PSBTN_NEXT => IDD_NEXT,
        PSBTN_FINISH => IDD_FINISH,
        PSBTN_OK => IDOK,
        PSBTN_APPLYNOW => IDD_APPLY,
        PSBTN_CANCEL => IDCANCEL,
        _ => return,
    };
    unsafe { crate::SendMessageW(sheet, WM_COMMAND, id as u64, dlg_ids(sheet, id) as i64) };
}

fn add_tab(sheet: Hwnd, i: usize, title: &[u16]) {
    let tab = dlg_ids(sheet, IDD_TAB);
    if tab == 0 {
        return;
    }
    let mut t = title.to_vec();
    t.push(0);
    let mut item = [0u8; 40];
    item[0..4].copy_from_slice(&1u32.to_le_bytes()); // TCIF_TEXT
    item[16..24].copy_from_slice(&(t.as_ptr() as u64).to_le_bytes());
    unsafe { crate::SendMessageW(tab, TCM_INSERTITEMW, i as u64, item.as_ptr() as i64) };
}

/// Title the sheet from the caption, "Properties for X" with PSH_PROPTITLE.
fn set_title(sheet: Hwnd, text: &[u16], prop: bool) {
    let mut t: Vec<u16> = Vec::new();
    if prop {
        t.extend(text);
        t.extend(" Properties".encode_utf16());
    } else {
        t.extend(text);
    }
    t.push(0);
    unsafe { crate::SetWindowTextW(sheet, t.as_ptr()) };
}

// -----| the sheet's dialog procedure |-----

unsafe extern "system" fn sheet_proc(hwnd: Hwnd, msg: u32, wp: u64, lp: i64) -> i64 {
    match msg {
        WM_INITDIALOG => {
            let sheet = unsafe { Box::from_raw(lp as *mut Sheet) };
            let (start, wizard, callback, no_apply, titles, title, prop) = (
                sheet.start,
                sheet.wizard,
                sheet.callback,
                sheet.no_apply,
                sheet.pages.iter().map(|p| page_of(p.page).map_or(Vec::new(), |g| g.title())).collect::<Vec<_>>(),
                sheet.title.clone(),
                sheet.title_prop,
            );
            SHEETS.lock().insert(hwnd, *sheet);
            set_title(hwnd, &title, prop);
            for (i, t) in titles.iter().enumerate() {
                add_tab(hwnd, i, t);
            }
            // Where pages go.
            let mut r = [0i32; 4];
            let tab = dlg_ids(hwnd, IDD_TAB);
            if !wizard {
                unsafe {
                    crate::GetClientRect(tab, &mut r);
                    crate::SendMessageW(tab, TCM_ADJUSTRECT, 0, r.as_mut_ptr() as i64);
                    let mut pts = [[r[0], r[1]], [r[2], r[3]]];
                    crate::MapWindowPoints(tab, hwnd, pts.as_mut_ptr(), 2);
                    r = [pts[0][0], pts[0][1], pts[1][0], pts[1][1]];
                }
            } else {
                // Above the wizard's buttons.
                let mut c = [0i32; 4];
                unsafe { crate::GetClientRect(hwnd, &mut c) };
                let mut d = [0, 0, 4, 28];
                unsafe { crate::MapDialogRect(hwnd, &mut d) };
                r = [0, 0, c[2], c[3] - d[3]];
            }
            with(hwnd, |s| s.page_rect = r);
            if wizard {
                // Back and Next until a page says; Finish waits behind Next.
                let f = dlg_ids(hwnd, IDD_FINISH);
                unsafe {
                    crate::ShowWindow(tab, 0);
                    crate::ShowWindow(f, 0);
                }
            }
            if no_apply {
                let a = dlg_ids(hwnd, IDD_APPLY);
                if a != 0 {
                    unsafe { crate::ShowWindow(a, 0) };
                }
            }
            if callback != 0 {
                let f: unsafe extern "system" fn(Hwnd, u32, i64) -> i32 = unsafe { core::mem::transmute(callback) };
                unsafe { f(hwnd, PSCB_INITIALIZED, 0) };
            }
            go_to(hwnd, start, false, 1);
            // The page chose its own focus.
            0
        }
        WM_NOTIFY => {
            let code = unsafe { *((lp as *const u8).add(16) as *const i32) };
            let from = unsafe { *((lp as *const u8).add(8) as *const u64) };
            if from == IDD_TAB as u64 {
                let tab = dlg_ids(hwnd, IDD_TAB);
                match code {
                    TCN_SELCHANGING => {
                        // The current page may refuse to be left.
                        let cur = current_page(hwnd);
                        let stay = cur != 0 && send_psn(cur, hwnd, PSN_KILLACTIVE, 0) != 0;
                        unsafe { crate::SetWindowLongPtrW(hwnd, DWLP_MSGRESULT, stay as i64) };
                        return 1;
                    }
                    TCN_SELCHANGE => {
                        let i = unsafe { crate::SendMessageW(tab, TCM_GETCURSEL, 0, 0) } as i32;
                        go_to(hwnd, i, false, 1);
                        return 1;
                    }
                    _ => {}
                }
            }
            0
        }
        WM_COMMAND => {
            let id = (wp & 0xFFFF) as u16;
            let callback = with(hwnd, |s| s.callback).unwrap_or(0);
            let report = |b: i64| {
                if callback != 0 {
                    let f: unsafe extern "system" fn(Hwnd, u32, i64) -> i32 = unsafe { core::mem::transmute(callback) };
                    unsafe { f(hwnd, PSCB_BUTTONPRESSED, b) };
                }
            };
            match id {
                IDOK => {
                    report(PSBTN_OK as i64);
                    if apply(hwnd, true) {
                        finish(hwnd, 1);
                    }
                }
                IDCANCEL => {
                    report(PSBTN_CANCEL as i64);
                    cancel(hwnd);
                }
                IDD_APPLY => {
                    report(PSBTN_APPLYNOW as i64);
                    apply(hwnd, false);
                }
                IDD_BACK => wizard_step(hwnd, PSN_WIZBACK, -1),
                IDD_NEXT => wizard_step(hwnd, PSN_WIZNEXT, 1),
                IDD_FINISH => {
                    let cur = current_page(hwnd);
                    let r = if cur != 0 { send_psn(cur, hwnd, PSN_WIZFINISH, 0) } else { 0 };
                    if r == 0 {
                        finish(hwnd, 1);
                    }
                }
                IDHELP => {
                    let cur = current_page(hwnd);
                    if cur != 0 {
                        send_psn(cur, hwnd, -205, 0);
                    }
                }
                _ => return 0,
            }
            1
        }
        WM_CLOSE => {
            cancel(hwnd);
            1
        }
        PSM_SETCURSEL => {
            // By index, or by page handle.
            let i = if lp != 0 {
                with(hwnd, |s| s.pages.iter().position(|p| p.page == lp as u64).map_or(-1, |i| i as i32)).unwrap_or(-1)
            } else {
                wp as i32
            };
            let ok = go_to(hwnd, i, true, 1);
            answer(hwnd, ok as i64)
        }
        PSM_SETCURSELID => {
            let ok = find_id(hwnd, lp).is_some_and(|i| go_to(hwnd, i, true, 1));
            answer(hwnd, ok as i64)
        }
        PSM_ADDPAGE | PSM_INSERTPAGE => {
            let page = lp as u64;
            let Some(pg) = page_of(page) else { return answer(hwnd, 0) };
            let title = pg.title();
            let at = with(hwnd, |s| {
                let at = if msg == PSM_INSERTPAGE { (wp as usize).min(s.pages.len()) } else { s.pages.len() };
                s.pages.insert(at, PageSlot { page, hwnd: 0 });
                if msg == PSM_INSERTPAGE && (at as i32) <= s.current {
                    s.current += 1;
                }
                at
            })
            .unwrap_or(0);
            add_tab(hwnd, at, &title);
            answer(hwnd, 1)
        }
        PSM_REMOVEPAGE => {
            let i = if lp != 0 {
                with(hwnd, |s| s.pages.iter().position(|p| p.page == lp as u64).map_or(-1, |i| i as i32)).unwrap_or(-1)
            } else {
                wp as i32
            };
            let Some(Some(slot)) = with(hwnd, |s| {
                if i < 0 || i as usize >= s.pages.len() {
                    return None;
                }
                Some(s.pages.remove(i as usize))
            }) else {
                return answer(hwnd, 0);
            };
            if slot.hwnd != 0 {
                unsafe { crate::DestroyWindow(slot.hwnd) };
            }
            DestroyPropertySheetPage(slot.page);
            let tab = dlg_ids(hwnd, IDD_TAB);
            if tab != 0 {
                unsafe { crate::SendMessageW(tab, TCM_DELETEITEM, i as u64, 0) };
            }
            let (cur, n) = with(hwnd, |s| (s.current, s.pages.len() as i32)).unwrap_or((0, 0));
            if cur == i && n > 0 {
                go_to(hwnd, i.min(n - 1), false, 1);
            } else if cur > i {
                with(hwnd, |s| s.current -= 1);
            }
            answer(hwnd, 1)
        }
        PSM_CHANGED => {
            with(hwnd, |s| {
                if !s.changed.contains(&wp) {
                    s.changed.push(wp);
                }
            });
            let a = dlg_ids(hwnd, IDD_APPLY);
            if a != 0 {
                unsafe { crate::EnableWindow(a, 1) };
            }
            answer(hwnd, 1)
        }
        PSM_UNCHANGED => {
            let none = with(hwnd, |s| {
                s.changed.retain(|&h| h != wp);
                s.changed.is_empty()
            })
            .unwrap_or(true);
            let a = dlg_ids(hwnd, IDD_APPLY);
            if a != 0 && none {
                unsafe { crate::EnableWindow(a, 0) };
            }
            answer(hwnd, 1)
        }
        PSM_RESTARTWINDOWS | PSM_REBOOTSYSTEM => {
            with(hwnd, |s| s.result = if msg == PSM_RESTARTWINDOWS { 1 } else { 2 });
            answer(hwnd, 1)
        }
        PSM_CANCELTOCLOSE => {
            let c = dlg_ids(hwnd, IDCANCEL);
            let ok = dlg_ids(hwnd, IDOK);
            if c != 0 {
                let t = crate::wide("Close");
                unsafe { crate::SetWindowTextW(c, t.as_ptr()) };
            }
            if ok != 0 {
                unsafe { crate::EnableWindow(ok, 0) };
            }
            answer(hwnd, 1)
        }
        PSM_QUERYSIBLINGS => {
            let pages: Vec<Hwnd> = with(hwnd, |s| s.pages.iter().map(|p| p.hwnd).filter(|&h| h != 0).collect()).unwrap_or_default();
            for p in pages {
                let r = unsafe { crate::SendMessageW(p, msg, wp, lp) };
                if r != 0 {
                    return answer(hwnd, r);
                }
            }
            answer(hwnd, 0)
        }
        PSM_APPLY => answer(hwnd, apply(hwnd, false) as i64),
        PSM_SETTITLEW | PSM_SETTITLEA => {
            let t = crate::text_arg(lp as u64, msg == PSM_SETTITLEW);
            set_title(hwnd, &t, wp as u32 & PSH_PROPTITLE != 0);
            answer(hwnd, 1)
        }
        PSM_SETWIZBUTTONS => {
            set_wiz_buttons(hwnd, lp as u32);
            answer(hwnd, 1)
        }
        PSM_PRESSBUTTON => {
            press(hwnd, wp);
            answer(hwnd, 1)
        }
        PSM_SETFINISHTEXTW | PSM_SETFINISHTEXTA => {
            let mut t = crate::text_arg(lp as u64, msg == PSM_SETFINISHTEXTW);
            t.push(0);
            let f = dlg_ids(hwnd, IDD_FINISH);
            let (back, next) = (dlg_ids(hwnd, IDD_BACK), dlg_ids(hwnd, IDD_NEXT));
            unsafe {
                crate::SetWindowTextW(f, t.as_ptr());
                crate::ShowWindow(f, 5);
                crate::EnableWindow(f, 1);
                crate::ShowWindow(back, 0);
                crate::ShowWindow(next, 0);
            }
            answer(hwnd, 1)
        }
        PSM_GETTABCONTROL => answer(hwnd, dlg_ids(hwnd, IDD_TAB) as i64),
        PSM_ISDIALOGMESSAGE => {
            let r = unsafe { crate::IsDialogMessageW(hwnd, lp as *const u8) };
            answer(hwnd, r as i64)
        }
        // A modeless sheet's owner destroys it when this says NULL.
        PSM_GETCURRENTPAGEHWND => {
            let closed = with(hwnd, |s| s.closed).unwrap_or(false);
            answer(hwnd, if closed { 0 } else { current_page(hwnd) as i64 })
        }
        PSM_HWNDTOINDEX => {
            let r = with(hwnd, |s| s.pages.iter().position(|p| p.hwnd == wp).map_or(-1, |i| i as i64)).unwrap_or(-1);
            answer(hwnd, r)
        }
        PSM_INDEXTOHWND => {
            let r = with(hwnd, |s| s.pages.get(wp as usize).map_or(0, |p| p.hwnd as i64)).unwrap_or(0);
            answer(hwnd, r)
        }
        PSM_PAGETOINDEX => {
            let r = with(hwnd, |s| s.pages.iter().position(|p| p.page == lp as u64).map_or(-1, |i| i as i64)).unwrap_or(-1);
            answer(hwnd, r)
        }
        PSM_INDEXTOPAGE => {
            let r = with(hwnd, |s| s.pages.get(wp as usize).map_or(0, |p| p.page as i64)).unwrap_or(0);
            answer(hwnd, r)
        }
        PSM_IDTOINDEX => {
            let r = find_id(hwnd, lp).map_or(-1, |i| i as i64);
            answer(hwnd, r)
        }
        PSM_INDEXTOID => {
            let r = with(hwnd, |s| s.pages.get(wp as usize).and_then(|p| page_of(p.page)).map_or(0, |g| g.template() as i64)).unwrap_or(0);
            answer(hwnd, r)
        }
        PSM_GETRESULT => {
            let r = with(hwnd, |s| if s.closed { s.result } else { -1 }).unwrap_or(-1);
            answer(hwnd, r)
        }
        // WM_NCDESTROY: the pages go with it.
        0x0082 => {
            if let Some(s) = SHEETS.lock().remove(&hwnd) {
                for p in s.pages {
                    DestroyPropertySheetPage(p.page);
                }
            }
            0
        }
        _ => 0,
    }
}

/// A dialog procedure's answer to a PSM_ message.
fn answer(hwnd: Hwnd, v: i64) -> i64 {
    unsafe { crate::SetWindowLongPtrW(hwnd, DWLP_MSGRESULT, v) };
    1
}

fn set_wiz_buttons(sheet: Hwnd, flags: u32) {
    let (back, next, fin) = (dlg_ids(sheet, IDD_BACK), dlg_ids(sheet, IDD_NEXT), dlg_ids(sheet, IDD_FINISH));
    unsafe {
        crate::EnableWindow(back, (flags & PSWIZB_BACK != 0) as Bool);
        let finishing = flags & (PSWIZB_FINISH | PSWIZB_DISABLEDFINISH) != 0;
        crate::ShowWindow(next, if finishing { 0 } else { 5 });
        crate::ShowWindow(fin, if finishing { 5 } else { 0 });
        crate::EnableWindow(next, (flags & PSWIZB_NEXT != 0) as Bool);
        crate::EnableWindow(fin, (flags & PSWIZB_FINISH != 0) as Bool);
    }
}

// -----| building the sheet's dialog |-----

struct Tmpl(Vec<u8>);

impl Tmpl {
    fn w(&mut self, v: u16) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn d(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn s(&mut self, t: &str) {
        for c in t.encode_utf16() {
            self.w(c);
        }
        self.w(0);
    }
    fn item(&mut self, class: &str, id: u16, style: u32, r: [i32; 4], text: &str) {
        while self.0.len() % 4 != 0 {
            self.0.push(0);
        }
        self.d(style | 0x5000_0000); // WS_CHILD | WS_VISIBLE
        self.d(0);
        for v in r {
            self.w(v as i16 as u16);
        }
        self.w(id);
        self.s(class);
        self.s(text);
        self.w(0);
    }
}

fn build(wizard: bool, no_apply: bool, help: bool, pw: i32, ph: i32) -> Vec<u8> {
    let mut t = Tmpl(Vec::new());
    let (cx, cy) = if wizard { (pw, ph + 28) } else { (pw + 16, ph + 22 + 4 + 4 + 14 + 6) };
    let count = if wizard { 5 } else { 3 + !no_apply as u16 + help as u16 };
    // WS_POPUP | WS_CAPTION | WS_SYSMENU | DS_MODALFRAME | DS_SETFONT | DS_CENTER,
    // WS_EX_CONTROLPARENT.
    t.d(0x80C8_0000 | 0x80 | 0x40 | 0x800);
    t.d(0x0001_0000);
    t.w(count);
    for v in [0, 0, cx, cy] {
        t.w(v as i16 as u16);
    }
    t.w(0);
    t.w(0);
    t.w(0);
    t.w(8);
    t.s("MS Shell Dlg");
    if wizard {
        let y = cy - 18;
        t.item("Button", IDD_BACK, 0x1_0000, [cx - 164, y, 50, 14], "< &Back");
        t.item("Button", IDD_NEXT, 0x1_0001, [cx - 114, y, 50, 14], "&Next >");
        t.item("Button", IDD_FINISH, 0x1_0000, [cx - 114, y, 50, 14], "Finish");
        t.item("Button", IDCANCEL, 0x1_0000, [cx - 57, y, 50, 14], "Cancel");
        t.item("SysTabControl32", IDD_TAB, 0x0400_0000, [0, 0, cx, cy - 28], "");
    } else {
        t.item("SysTabControl32", IDD_TAB, 0x1_0000 | 0x0400_0000, [4, 4, pw + 8, ph + 22], "");
        let y = cy - 20;
        let mut x = cx - 4 - 50;
        if help {
            t.item("Button", IDHELP, 0x1_0000, [x, y, 50, 14], "Help");
            x -= 54;
        }
        if !no_apply {
            t.item("Button", IDD_APPLY, 0x1_0000 | 0x0800_0000, [x, y, 50, 14], "&Apply");
            x -= 54;
        }
        t.item("Button", IDCANCEL, 0x1_0000, [x, y, 50, 14], "Cancel");
        x -= 54;
        t.item("Button", IDOK, 0x1_0001, [x, y, 50, 14], "OK");
    }
    t.0
}

unsafe fn property_sheet(h: *const u8, unicode: bool) -> i64 {
    if h.is_null() {
        return -1;
    }
    let (flags, parent, inst, caption, n, start, pages_ptr, callback) = unsafe {
        let d = |o: usize| core::ptr::read_unaligned(h.add(o) as *const u32);
        let q = |o: usize| core::ptr::read_unaligned(h.add(o) as *const u64);
        (d(4), q(8), q(16), q(32), d(40) as usize, q(48), q(56), q(64))
    };
    // The pages: an array of PROPSHEETPAGE or of page handles.
    let mut pages: Vec<PageSlot> = Vec::new();
    if flags & PSH_PROPSHEETPAGE != 0 {
        let mut at = pages_ptr as *const u8;
        for _ in 0..n {
            let size = unsafe { core::ptr::read_unaligned(at as *const u32) } as usize;
            pages.push(PageSlot { page: read_page(at, unicode), hwnd: 0 });
            at = unsafe { at.add(size) };
        }
    } else {
        for i in 0..n {
            let page = unsafe { *(pages_ptr as *const u64).add(i) };
            pages.push(PageSlot { page, hwnd: 0 });
        }
    }
    // The first page: an index, or (PSH_USEPSTARTPAGE) a title or template name.
    let first = if flags & PSH_USEPSTARTPAGE != 0 {
        let want = crate::text_arg(start, unicode);
        pages.iter().position(|p| page_of(p.page).is_some_and(|g| g.title() == want)).unwrap_or(0) as i32
    } else {
        start as u32 as i32
    };
    // Big enough for the largest page.
    let (mut pw, mut ph) = (0, 0);
    for p in &pages {
        if let Some(t) = page_of(p.page).and_then(template_of) {
            let (w, h) = template_size(t);
            pw = pw.max(w);
            ph = ph.max(h);
        }
    }
    let wizard = flags & (PSH_WIZARD | PSH_WIZARD97 | PSH_WIZARD_LITE | PSH_AEROWIZARD) != 0;
    let no_apply = flags & PSH_NOAPPLYNOW != 0;
    let title = if caption >> 16 == 0 {
        let mut buf = [0u16; 128];
        let k = unsafe { crate::LoadStringW(inst, caption as u32, buf.as_mut_ptr(), 128) }.max(0) as usize;
        buf[..k].to_vec()
    } else {
        crate::text_arg(caption, unicode)
    };
    let modeless = flags & PSH_MODELESS != 0;
    let sheet = Box::new(Sheet {
        pages,
        current: -1,
        wizard,
        modeless,
        changed: Vec::new(),
        result: 0,
        applied: false,
        page_rect: [0; 4],
        callback: if flags & PSH_USECALLBACK != 0 { callback as usize } else { 0 },
        no_apply,
        title_prop: flags & PSH_PROPTITLE != 0,
        title,
        closed: false,
        start: first,
    });
    let t = build(wizard, no_apply, flags & PSH_HASHELP != 0, pw.max(100), ph.max(50));
    let param = Box::into_raw(sheet) as i64;
    let proc = sheet_proc as *const () as usize;
    if modeless {
        unsafe { crate::CreateDialogIndirectParamW(inst, t.as_ptr(), parent, proc, param) as i64 }
    } else {
        unsafe { crate::DialogBoxIndirectParamW(inst, t.as_ptr(), parent, proc, param) }
    }
}

#[no_mangle]
pub unsafe extern "system" fn PropertySheetW(h: *const u8) -> i64 {
    unsafe { property_sheet(h, true) }
}

#[no_mangle]
pub unsafe extern "system" fn PropertySheetA(h: *const u8) -> i64 {
    unsafe { property_sheet(h, false) }
}
