//! VEILOS `comdlg32.dll`: the common dialogs (WIN32_DESIGN.md, G4). Open and
//! Save As (a folder list, the filter, a name box), Find and Replace
//! (modeless, reporting through the registered "commdlg_FindReplace"
//! message), and a font chooser, all built as in-memory dialog templates
//! for user32. There are no printers: the print dialogs report
//! PDERR_NODEFAULTPRN. Hooks and custom templates are ignored.

#![no_std]

extern crate alloc;

#[path = "../../dllrt.rs"]
mod dllrt;

use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, Ordering};

type Hwnd = u64;
type Bool = i32;

const WM_INITDIALOG: u32 = 0x0110;
const WM_COMMAND: u32 = 0x0111;
const WM_CLOSE: u32 = 0x0010;
const IDOK: u16 = 1;
const IDCANCEL: u16 = 2;
const BM_GETCHECK: u32 = 0x00F0;
const BM_SETCHECK: u32 = 0x00F1;
const LB_ADDSTRING: u32 = 0x0180;
const LB_RESETCONTENT: u32 = 0x0184;
const LB_SETCURSEL: u32 = 0x0186;
const LB_GETCURSEL: u32 = 0x0188;
const LB_GETTEXT: u32 = 0x0189;
const LB_GETTEXTLEN: u32 = 0x018A;
const LB_FINDSTRINGEXACT: u32 = 0x01A2;
const LBN_SELCHANGE: u16 = 1;
const LBN_DBLCLK: u16 = 2;

const WS_CHILD: u32 = 0x4000_0000;
const WS_VISIBLE: u32 = 0x1000_0000;
const WS_TABSTOP: u32 = 0x0001_0000;
const WS_GROUP: u32 = 0x0002_0000;
const WS_BORDER: u32 = 0x0080_0000;
const WS_VSCROLL: u32 = 0x0020_0000;
const DS_SETFONT: u32 = 0x40;
const DS_MODALFRAME: u32 = 0x80;
const DS_CENTER: u32 = 0x800;

const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
const INVALID_ATTRIBUTES: u32 = 0xFFFF_FFFF;

// CommDlgExtendedError codes.
const CDERR_INITIALIZATION: u32 = 0x0002;
const FNERR_BUFFERTOOSMALL: u32 = 0x3003;
const PDERR_NODEFAULTPRN: u32 = 0x1008;

#[link(name = "kernel32", kind = "raw-dylib")]
unsafe extern "system" {
    fn FindFirstFileW(pattern: *const u16, data: *mut u8) -> u64;
    fn FindNextFileW(h: u64, data: *mut u8) -> Bool;
    fn FindClose(h: u64) -> Bool;
    fn GetCurrentDirectoryW(len: u32, buf: *mut u16) -> u32;
    fn GetFileAttributesW(name: *const u16) -> u32;
    fn GetFullPathNameW(name: *const u16, len: u32, buf: *mut u16, part: *mut *mut u16) -> u32;
    fn MulDiv(a: i32, b: i32, c: i32) -> i32;
}

#[link(name = "user32", kind = "raw-dylib")]
unsafe extern "system" {
    fn DialogBoxIndirectParamW(inst: u64, t: *const u8, owner: Hwnd, proc: DlgProc, param: isize) -> isize;
    fn CreateDialogIndirectParamW(inst: u64, t: *const u8, owner: Hwnd, proc: DlgProc, param: isize) -> Hwnd;
    fn EndDialog(hwnd: Hwnd, result: isize) -> Bool;
    fn DestroyWindow(hwnd: Hwnd) -> Bool;
    fn SendMessageW(hwnd: Hwnd, msg: u32, wp: usize, lp: isize) -> isize;
    fn SendDlgItemMessageW(hwnd: Hwnd, id: i32, msg: u32, wp: usize, lp: isize) -> isize;
    fn GetDlgItemTextW(hwnd: Hwnd, id: i32, buf: *mut u16, cap: i32) -> u32;
    fn SetDlgItemTextW(hwnd: Hwnd, id: i32, text: *const u16) -> Bool;
    fn MessageBoxW(hwnd: Hwnd, text: *const u16, caption: *const u16, kind: u32) -> i32;
    fn RegisterWindowMessageW(name: *const u16) -> u32;
    fn SetPropW(hwnd: Hwnd, name: *const u16, data: isize) -> Bool;
    fn SetFocus(hwnd: Hwnd) -> Hwnd;
    fn GetDlgItem(hwnd: Hwnd, id: i32) -> Hwnd;
    fn GetPropW(hwnd: Hwnd, name: *const u16) -> isize;
    fn RemovePropW(hwnd: Hwnd, name: *const u16) -> isize;
    fn GetDC(hwnd: Hwnd) -> u64;
    fn ReleaseDC(hwnd: Hwnd, hdc: u64) -> i32;
}

#[link(name = "gdi32", kind = "raw-dylib")]
unsafe extern "system" {
    fn EnumFontFamiliesExW(hdc: u64, lf: *const u8, proc: FontProc, lp: isize, flags: u32) -> i32;
}

type DlgProc = unsafe extern "system" fn(Hwnd, u32, usize, isize) -> isize;
type FontProc = unsafe extern "system" fn(*const u8, *const u8, u32, isize) -> i32;

static LAST_ERROR: AtomicU32 = AtomicU32::new(0);

/// The folder the last file dialog ended in: where the next one starts
/// when the program names none.
static LAST_FOLDER: dllrt::Lock<Vec<u16>> = dllrt::Lock::new(Vec::new());

fn fail(code: u32) -> Bool {
    LAST_ERROR.store(code, Ordering::Relaxed);
    0
}

#[no_mangle]
pub extern "system" fn DllMain(_module: usize, _reason: u32, _reserved: usize) -> i32 {
    1
}

#[no_mangle]
pub extern "system" fn CommDlgExtendedError() -> u32 {
    LAST_ERROR.load(Ordering::Relaxed)
}

// -----| strings |-----

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(core::iter::once(0)).collect()
}

/// A NUL-terminated wide string, without the NUL.
unsafe fn wstr(p: *const u16) -> Vec<u16> {
    if p.is_null() {
        return Vec::new();
    }
    (0..).map(|i| unsafe { *p.add(i) }).take_while(|&c| c != 0).collect()
}

unsafe fn astr(p: *const u8) -> Vec<u16> {
    if p.is_null() {
        return Vec::new();
    }
    let bytes: Vec<u8> = (0..).map(|i| unsafe { *p.add(i) }).take_while(|&c| c != 0).collect();
    alloc::string::String::from_utf8_lossy(&bytes).encode_utf16().collect()
}

fn z(s: &[u16]) -> Vec<u16> {
    let mut v = s.to_vec();
    v.push(0);
    v
}

fn lower(c: u16) -> u16 {
    if (b'A' as u16..=b'Z' as u16).contains(&c) { c + 32 } else { c }
}

/// `*` and `?` wildcard match, ignoring case.
fn matches(pat: &[u16], name: &[u16]) -> bool {
    match pat.first() {
        None => name.is_empty(),
        Some(&0x2A) => (0..=name.len()).any(|i| matches(&pat[1..], &name[i..])),
        Some(&0x3F) => !name.is_empty() && matches(&pat[1..], &name[1..]),
        Some(&c) => !name.is_empty() && lower(c) == lower(name[0]) && matches(&pat[1..], &name[1..]),
    }
}

/// The name part of a path: after the last `\`, `/` or `:`.
fn name_start(path: &[u16]) -> usize {
    path.iter().rposition(|&c| c == b'\\' as u16 || c == b'/' as u16 || c == b':' as u16).map_or(0, |i| i + 1)
}

fn dlg_text(hwnd: Hwnd, id: i32) -> Vec<u16> {
    let mut buf = [0u16; 520];
    let n = unsafe { GetDlgItemTextW(hwnd, id, buf.as_mut_ptr(), buf.len() as i32) } as usize;
    buf[..n.min(buf.len())].to_vec()
}

fn set_text(hwnd: Hwnd, id: i32, s: &[u16]) {
    unsafe { SetDlgItemTextW(hwnd, id, z(s).as_ptr()) };
}

fn list_text(hwnd: Hwnd, id: i32, index: isize) -> Vec<u16> {
    let len = unsafe { SendDlgItemMessageW(hwnd, id, LB_GETTEXTLEN, index as usize, 0) };
    if len < 0 {
        return Vec::new();
    }
    let mut buf = alloc::vec![0u16; len as usize + 1];
    unsafe { SendDlgItemMessageW(hwnd, id, LB_GETTEXT, index as usize, buf.as_mut_ptr() as isize) };
    buf.truncate(len as usize);
    buf
}

fn checked(hwnd: Hwnd, id: i32) -> bool {
    unsafe { SendDlgItemMessageW(hwnd, id, BM_GETCHECK, 0, 0) == 1 }
}

fn check(hwnd: Hwnd, id: i32, on: bool) {
    unsafe { SendDlgItemMessageW(hwnd, id, BM_SETCHECK, on as usize, 0) };
}

/// The focus to an edit box, its text selected (WM_INITDIALOG then
/// returns FALSE).
fn focus_text(hwnd: Hwnd, id: i32) {
    unsafe {
        SetFocus(GetDlgItem(hwnd, id));
        SendDlgItemMessageW(hwnd, id, 0x00B1, 0, -1); // EM_SETSEL
    }
}

/// The dialog's state pointer, kept as a window property.
fn state_prop() -> Vec<u16> {
    wide("VeilCommDlg")
}

// -----| dialog templates |-----

/// A DLGTEMPLATE being built: the header, then items added one by one.
struct Template(Vec<u8>);

impl Template {
    fn new(caption: &[u16], cx: i16, cy: i16, count: u16) -> Self {
        let mut t = Template(Vec::new());
        t.u32(0x80C8_0000 | DS_MODALFRAME | DS_CENTER | DS_SETFONT); // WS_POPUP | WS_CAPTION | WS_SYSMENU
        t.u32(0x0001_0000); // WS_EX_CONTROLPARENT
        t.u16(count);
        for v in [0, 0, cx, cy] {
            t.u16(v as u16);
        }
        t.u16(0);
        t.u16(0);
        t.str(caption);
        t.u16(8);
        t.str(&"MS Shell Dlg".encode_utf16().collect::<Vec<u16>>());
        t
    }

    fn u16(&mut self, v: u16) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }

    fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }

    fn str(&mut self, s: &[u16]) {
        for &c in s {
            self.u16(c);
        }
        self.u16(0);
    }

    /// One control: a predefined class by ordinal (0x80 button, 0x81 edit,
    /// 0x82 static, 0x83 list box).
    fn item(&mut self, class: u16, id: u16, style: u32, rect: [i16; 4], title: &str) -> &mut Self {
        while self.0.len() % 4 != 0 {
            self.0.push(0);
        }
        self.u32(style | WS_CHILD | WS_VISIBLE);
        self.u32(0);
        for v in rect {
            self.u16(v as u16);
        }
        self.u16(id);
        self.u16(0xFFFF);
        self.u16(class);
        self.str(&title.encode_utf16().collect::<Vec<u16>>());
        self.u16(0);
        self
    }
}

fn button(t: &mut Template, id: u16, default: bool, rect: [i16; 4], title: &str) {
    t.item(0x80, id, WS_TABSTOP | default as u32, rect, title);
}

fn label(t: &mut Template, rect: [i16; 4], title: &str) {
    t.item(0x82, 0xFFFF, 0, rect, title);
}

// -----| Open and Save As |-----

const ID_FOLDER: i32 = 1091; // stc4
const ID_LIST: i32 = 1120; // lst1
const ID_NAME: i32 = 1152; // edt1

const OFN_OVERWRITEPROMPT: u32 = 0x2;
const OFN_FILEMUSTEXIST: u32 = 0x1000;

/// OPENFILENAME, read into one form for both character sets.
struct FileDialog {
    save: bool,
    owner: Hwnd,
    title: Vec<u16>,
    /// (description, patterns) pairs.
    filters: Vec<(Vec<u16>, Vec<u16>)>,
    filter_index: usize,
    flags: u32,
    def_ext: Vec<u16>,
    folder: Vec<u16>,
    initial_name: Vec<u16>,
    /// The chosen full path.
    result: Vec<u16>,
}

/// Reads a double-NUL-terminated filter list of (description, pattern).
unsafe fn read_filters(p: *const u8, unicode: bool) -> Vec<(Vec<u16>, Vec<u16>)> {
    let mut parts: Vec<Vec<u16>> = Vec::new();
    if p.is_null() {
        return Vec::new();
    }
    let mut at = p;
    loop {
        let s = unsafe { if unicode { wstr(at as *const u16) } else { astr(at) } };
        if s.is_empty() {
            break;
        }
        let bytes = if unicode { (s.len() + 1) * 2 } else { unsafe { (0..).take_while(|&i| *at.add(i) != 0).count() + 1 } };
        at = unsafe { at.add(bytes) };
        parts.push(s);
    }
    parts.chunks(2).map(|c| (c[0].clone(), c.get(1).cloned().unwrap_or_default())).collect()
}

impl FileDialog {
    /// The OPENFILENAME at `ofn` (W or A: the layout is the same, the
    /// strings differ).
    unsafe fn read(ofn: *const u8, unicode: bool, save: bool) -> Self {
        let q = |off: usize| unsafe { core::ptr::read_unaligned(ofn.add(off) as *const u64) };
        let d = |off: usize| unsafe { core::ptr::read_unaligned(ofn.add(off) as *const u32) };
        let s = |p: u64| unsafe { if unicode { wstr(p as *const u16) } else { astr(p as *const u8) } };
        let file = s(q(48));
        let mut folder = s(q(80));
        let start = name_start(&file);
        if folder.is_empty() && start > 0 {
            folder = file[..start - 1].to_vec();
        }
        if folder.is_empty() {
            folder = LAST_FOLDER.lock().clone();
        }
        if folder.is_empty() {
            let mut buf = [0u16; 260];
            let n = unsafe { GetCurrentDirectoryW(260, buf.as_mut_ptr()) } as usize;
            folder = buf[..n.min(260)].to_vec();
        }
        FileDialog {
            save,
            owner: q(8),
            title: s(q(88)),
            filters: unsafe { read_filters(q(24) as *const u8, unicode) },
            filter_index: d(44) as usize,
            flags: d(96),
            def_ext: s(q(104)),
            folder: full_path(&folder),
            initial_name: file[start..].to_vec(),
            result: Vec::new(),
        }
    }

    fn patterns(&self) -> Vec<Vec<u16>> {
        let i = self.filter_index.max(1) - 1;
        match self.filters.get(i).or(self.filters.first()) {
            Some((_, p)) => p.split(|&c| c == b';' as u16).map(|s| s.iter().copied().filter(|&c| c != b' ' as u16).collect()).collect(),
            None => alloc::vec![alloc::vec![b'*' as u16]],
        }
    }

    /// Fills the list: `[..]` and `[folder]` entries, then matching files.
    fn fill(&self, hwnd: Hwnd) {
        unsafe { SendDlgItemMessageW(hwnd, ID_LIST, LB_RESETCONTENT, 0, 0) };
        set_text(hwnd, ID_FOLDER, &self.folder);
        let mut dirs = Vec::new();
        let mut files = Vec::new();
        let mut pattern = self.folder.clone();
        if pattern.last() != Some(&(b'\\' as u16)) {
            pattern.push(b'\\' as u16);
        }
        pattern.extend("*".encode_utf16());
        let mut data = [0u8; 592];
        let h = unsafe { FindFirstFileW(z(&pattern).as_ptr(), data.as_mut_ptr()) };
        if h != u64::MAX {
            let patterns = self.patterns();
            loop {
                let name = unsafe { wstr(data.as_ptr().add(44) as *const u16) };
                let attrs = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
                if name != [b'.' as u16] && name != [b'.' as u16, b'.' as u16] {
                    if attrs & FILE_ATTRIBUTE_DIRECTORY != 0 {
                        dirs.push(name);
                    } else if patterns.iter().any(|p| matches(p, &name)) {
                        files.push(name);
                    }
                }
                if unsafe { FindNextFileW(h, data.as_mut_ptr()) } == 0 {
                    break;
                }
            }
            unsafe { FindClose(h) };
        }
        let key = |s: &Vec<u16>| s.iter().map(|&c| lower(c)).collect::<Vec<u16>>();
        dirs.sort_by_key(key);
        files.sort_by_key(key);
        let add = |s: &[u16]| unsafe { SendDlgItemMessageW(hwnd, ID_LIST, LB_ADDSTRING, 0, z(s).as_ptr() as isize) };
        if self.folder.len() > 3 {
            add(&"[..]".encode_utf16().collect::<Vec<u16>>());
        }
        for d in &dirs {
            let mut s = alloc::vec![b'[' as u16];
            s.extend_from_slice(d);
            s.push(b']' as u16);
            add(&s);
        }
        for f in &files {
            add(f);
        }
    }

    /// `name` (absolute, or relative to the folder) made a full path.
    fn resolve(&self, name: &[u16]) -> Vec<u16> {
        let absolute = name.len() >= 2 && (name[1] == b':' as u16 || name[0] == b'\\' as u16);
        if absolute {
            return full_path(name);
        }
        let mut p = self.folder.clone();
        if p.last() != Some(&(b'\\' as u16)) {
            p.push(b'\\' as u16);
        }
        p.extend_from_slice(name);
        full_path(&p)
    }

    /// The name box or a list entry was chosen: enter a folder, or accept
    /// a file. True when the dialog should end.
    fn choose(&mut self, hwnd: Hwnd, name: &[u16]) -> bool {
        if name.is_empty() {
            return false;
        }
        let name = if name.len() > 2 && name[0] == b'[' as u16 && name[name.len() - 1] == b']' as u16 { &name[1..name.len() - 1] } else { name };
        let path = self.resolve(name);
        let attrs = unsafe { GetFileAttributesW(z(&path).as_ptr()) };
        if attrs != INVALID_ATTRIBUTES && attrs & FILE_ATTRIBUTE_DIRECTORY != 0 {
            self.folder = path;
            self.fill(hwnd);
            set_text(hwnd, ID_NAME, &[]);
            return false;
        }
        // Wildcards set the filter.
        if name.iter().any(|&c| c == b'*' as u16 || c == b'?' as u16) {
            self.filters = alloc::vec![(Vec::new(), name.to_vec())];
            self.filter_index = 1;
            self.fill(hwnd);
            return false;
        }
        let mut path = path;
        let base = &path[name_start(&path)..];
        if self.save && !self.def_ext.is_empty() && !base.contains(&(b'.' as u16)) {
            path.push(b'.' as u16);
            path.extend_from_slice(&self.def_ext);
        }
        let exists = unsafe { GetFileAttributesW(z(&path).as_ptr()) } != INVALID_ATTRIBUTES;
        let caption = if self.title.is_empty() { wide(if self.save { "Save As" } else { "Open" }) } else { z(&self.title) };
        let mut msg = path.clone();
        if !self.save && !exists && self.flags & OFN_FILEMUSTEXIST != 0 {
            msg.extend("\nFile not found.\nCheck the file name and try again.".encode_utf16());
            unsafe { MessageBoxW(hwnd, z(&msg).as_ptr(), caption.as_ptr(), 0x30) };
            return false;
        }
        if self.save && exists && self.flags & OFN_OVERWRITEPROMPT != 0 {
            msg.extend(" already exists.\nDo you want to replace it?".encode_utf16());
            if unsafe { MessageBoxW(hwnd, z(&msg).as_ptr(), caption.as_ptr(), 0x34) } != 6 {
                return false;
            }
        }
        self.result = path;
        true
    }
}

fn full_path(p: &[u16]) -> Vec<u16> {
    let mut buf = [0u16; 520];
    let n = unsafe { GetFullPathNameW(z(p).as_ptr(), 520, buf.as_mut_ptr(), core::ptr::null_mut()) } as usize;
    if n == 0 || n >= 520 {
        return p.to_vec();
    }
    let mut v = buf[..n].to_vec();
    if v.len() > 3 && v.last() == Some(&(b'\\' as u16)) {
        v.pop();
    }
    v
}

unsafe extern "system" fn file_proc(hwnd: Hwnd, msg: u32, wp: usize, lp: isize) -> isize {
    let prop = state_prop();
    match msg {
        WM_INITDIALOG => {
            unsafe { SetPropW(hwnd, prop.as_ptr(), lp) };
            let fd = unsafe { &*(lp as *const FileDialog) };
            fd.fill(hwnd);
            set_text(hwnd, ID_NAME, &fd.initial_name);
            focus_text(hwnd, ID_NAME);
            0
        }
        WM_COMMAND => {
            let fd = unsafe { GetPropW(hwnd, prop.as_ptr()) } as *mut FileDialog;
            if fd.is_null() {
                return 0;
            }
            let fd = unsafe { &mut *fd };
            let (id, code) = ((wp & 0xFFFF) as u16, (wp >> 16) as u16);
            match (id as i32, code) {
                (1, _) => {
                    let mut name = dlg_text(hwnd, ID_NAME);
                    if name.is_empty() {
                        let sel = unsafe { SendDlgItemMessageW(hwnd, ID_LIST, LB_GETCURSEL, 0, 0) };
                        if sel >= 0 {
                            name = list_text(hwnd, ID_LIST, sel);
                        }
                    }
                    if fd.choose(hwnd, &name) {
                        unsafe { EndDialog(hwnd, 1) };
                    }
                }
                (2, _) => unsafe {
                    EndDialog(hwnd, 0);
                },
                (ID_LIST, LBN_SELCHANGE) => {
                    let sel = unsafe { SendDlgItemMessageW(hwnd, ID_LIST, LB_GETCURSEL, 0, 0) };
                    let name = list_text(hwnd, ID_LIST, sel);
                    if !name.is_empty() && name[0] != b'[' as u16 {
                        set_text(hwnd, ID_NAME, &name);
                    }
                }
                (ID_LIST, LBN_DBLCLK) => {
                    let sel = unsafe { SendDlgItemMessageW(hwnd, ID_LIST, LB_GETCURSEL, 0, 0) };
                    let name = list_text(hwnd, ID_LIST, sel);
                    if fd.choose(hwnd, &name) {
                        unsafe { EndDialog(hwnd, 1) };
                    }
                }
                _ => return 0,
            }
            1
        }
        WM_CLOSE => {
            unsafe { EndDialog(hwnd, 0) };
            1
        }
        0x0002 => {
            unsafe { RemovePropW(hwnd, prop.as_ptr()) };
            0
        }
        _ => 0,
    }
}

/// Runs the dialog and writes the result back into the OPENFILENAME.
unsafe fn file_dialog(ofn: *mut u8, unicode: bool, save: bool) -> Bool {
    LAST_ERROR.store(0, Ordering::Relaxed);
    if ofn.is_null() {
        return fail(CDERR_INITIALIZATION);
    }
    let mut fd = unsafe { FileDialog::read(ofn, unicode, save) };
    let caption = if fd.title.is_empty() { (if save { "Save As" } else { "Open" }).encode_utf16().collect() } else { fd.title.clone() };
    let mut t = Template::new(&caption, 280, 170, 7);
    label(&mut t, [7, 7, 40, 10], "Look in:");
    t.item(0x82, ID_FOLDER as u16, 0x80, [50, 7, 223, 10], ""); // SS_NOPREFIX
    t.item(0x83, ID_LIST as u16, 0x1 | WS_BORDER | WS_VSCROLL | WS_TABSTOP, [7, 20, 266, 100], ""); // LBS_NOTIFY
    label(&mut t, [7, 130, 40, 10], "File name:");
    t.item(0x81, ID_NAME as u16, 0x80 | WS_BORDER | WS_TABSTOP, [50, 128, 153, 12], ""); // ES_AUTOHSCROLL
    button(&mut t, IDOK, true, [213, 127, 60, 14], if save { "&Save" } else { "&Open" });
    button(&mut t, IDCANCEL, false, [213, 147, 60, 14], "Cancel");
    let r = unsafe { DialogBoxIndirectParamW(0, t.0.as_ptr(), fd.owner, file_proc, &mut fd as *mut FileDialog as isize) };
    if r != 1 {
        return 0;
    }
    // lpstrFile and nMaxFile, nFileOffset and nFileExtension, lpstrFileTitle.
    let q = |off: usize| unsafe { core::ptr::read_unaligned(ofn.add(off) as *const u64) };
    let d = |off: usize| unsafe { core::ptr::read_unaligned(ofn.add(off) as *const u32) };
    let path = &fd.result;
    let start = name_start(path);
    // The chosen file's folder ("C:\\" keeps its separator).
    *LAST_FOLDER.lock() = path[..if start <= 3 { start } else { start - 1 }].to_vec();
    let ext = path[start..].iter().rposition(|&c| c == b'.' as u16).map_or(0, |i| start + i + 1);
    let encoded: Vec<u8> = if unicode { Vec::new() } else { alloc::string::String::from_utf16_lossy(path).into_bytes() };
    let need = if unicode { path.len() + 1 } else { encoded.len() + 1 };
    let (file, max) = (q(48), d(56) as usize);
    if file == 0 || need > max {
        if file != 0 && max >= 2 {
            unsafe { core::ptr::write_unaligned(file as *mut u16, need as u16) };
        }
        return fail(FNERR_BUFFERTOOSMALL);
    }
    unsafe {
        write_string(file, path, unicode);
        core::ptr::write_unaligned(ofn.add(100) as *mut u16, start as u16);
        core::ptr::write_unaligned(ofn.add(102) as *mut u16, if ext == 0 { path.len() as u16 } else { ext as u16 });
        // The filter index the user ended up with.
        core::ptr::write_unaligned(ofn.add(44) as *mut u32, fd.filter_index as u32);
        let (title, title_max) = (q(64), d(72) as usize);
        if title != 0 && title_max > path.len() - start {
            write_string(title, &path[start..], unicode);
        }
    }
    1
}

/// Writes `s` and a NUL as UTF-16 or (lossy) 8-bit text.
unsafe fn write_string(dst: u64, s: &[u16], unicode: bool) {
    if unicode {
        unsafe {
            core::ptr::copy_nonoverlapping(s.as_ptr(), dst as *mut u16, s.len());
            *(dst as *mut u16).add(s.len()) = 0;
        }
    } else {
        let bytes = alloc::string::String::from_utf16_lossy(s).into_bytes();
        unsafe {
            core::ptr::copy_nonoverlapping(bytes.as_ptr(), dst as *mut u8, bytes.len());
            *(dst as *mut u8).add(bytes.len()) = 0;
        }
    }
}

#[no_mangle]
pub unsafe extern "system" fn GetOpenFileNameW(ofn: *mut u8) -> Bool {
    unsafe { file_dialog(ofn, true, false) }
}

#[no_mangle]
pub unsafe extern "system" fn GetOpenFileNameA(ofn: *mut u8) -> Bool {
    unsafe { file_dialog(ofn, false, false) }
}

#[no_mangle]
pub unsafe extern "system" fn GetSaveFileNameW(ofn: *mut u8) -> Bool {
    unsafe { file_dialog(ofn, true, true) }
}

#[no_mangle]
pub unsafe extern "system" fn GetSaveFileNameA(ofn: *mut u8) -> Bool {
    unsafe { file_dialog(ofn, false, true) }
}

/// The name part of `file`: 0 when copied, else the size it needs
/// (characters, with the NUL); -1 for a bad name.
unsafe fn file_title(name: &[u16], buf: u64, cap: u16, unicode: bool) -> i16 {
    if name.is_empty() || name.iter().any(|&c| c == b'*' as u16 || c == b'?' as u16) {
        return -1;
    }
    // A folder's title is empty.
    let title = &name[name_start(name)..];
    if buf == 0 || (cap as usize) <= title.len() {
        return title.len() as i16 + 1;
    }
    unsafe { write_string(buf, title, unicode) };
    0
}

#[no_mangle]
pub unsafe extern "system" fn GetFileTitleW(file: *const u16, buf: *mut u16, cap: u16) -> i16 {
    unsafe { file_title(&wstr(file), buf as u64, cap, true) }
}

#[no_mangle]
pub unsafe extern "system" fn GetFileTitleA(file: *const u8, buf: *mut u8, cap: u16) -> i16 {
    unsafe { file_title(&astr(file), buf as u64, cap, false) }
}

// -----| Find and Replace |-----

const FR_DOWN: u32 = 0x1;
const FR_MATCHCASE: u32 = 0x4;
const FR_FINDNEXT: u32 = 0x8;
const FR_REPLACE: u32 = 0x10;
const FR_REPLACEALL: u32 = 0x20;
const FR_DIALOGTERM: u32 = 0x40;
const FR_HIDEUPDOWN: u32 = 0x4000;
const FR_HIDEMATCHCASE: u32 = 0x8000;

const ID_FIND: i32 = 1152; // edt1
const ID_REPLACE: i32 = 1153; // edt2
const ID_CASE: i32 = 1041; // chx2
const ID_UP: i32 = 1056; // rad1
const ID_DOWN: i32 = 1057; // rad2
const ID_REPLACE_ONE: u16 = 1024; // psh1
const ID_REPLACE_ALL: u16 = 1025; // psh2

/// FINDREPLACEW: lStructSize, hwndOwner, hInstance, Flags, lpstrFindWhat,
/// lpstrReplaceWith, wFindWhatLen, wReplaceWithLen, ...
#[repr(C)]
struct FindReplace {
    size: u32,
    owner: Hwnd,
    instance: u64,
    flags: u32,
    find: *mut u16,
    replace: *mut u16,
    find_len: u16,
    replace_len: u16,
}

fn find_message() -> u32 {
    unsafe { RegisterWindowMessageW(wide("commdlg_FindReplace").as_ptr()) }
}

unsafe fn put(dst: *mut u16, cap: u16, s: &[u16]) {
    if dst.is_null() || cap == 0 {
        return;
    }
    let n = s.len().min(cap as usize - 1);
    unsafe {
        core::ptr::copy_nonoverlapping(s.as_ptr(), dst, n);
        *dst.add(n) = 0;
    }
}

unsafe extern "system" fn find_proc(hwnd: Hwnd, msg: u32, wp: usize, lp: isize) -> isize {
    let prop = state_prop();
    let fr = unsafe { GetPropW(hwnd, prop.as_ptr()) } as *mut FindReplace;
    match msg {
        WM_INITDIALOG => {
            unsafe { SetPropW(hwnd, prop.as_ptr(), lp) };
            let fr = unsafe { &*(lp as *const FindReplace) };
            set_text(hwnd, ID_FIND, unsafe { &wstr(fr.find) });
            if !fr.replace.is_null() {
                set_text(hwnd, ID_REPLACE, unsafe { &wstr(fr.replace) });
            }
            check(hwnd, ID_CASE, fr.flags & FR_MATCHCASE != 0);
            check(hwnd, ID_DOWN, fr.flags & FR_DOWN != 0);
            check(hwnd, ID_UP, fr.flags & FR_DOWN == 0);
            focus_text(hwnd, ID_FIND);
            0
        }
        WM_COMMAND if !fr.is_null() => {
            let fr = unsafe { &mut *fr };
            let action = match (wp & 0xFFFF) as u16 {
                IDOK => FR_FINDNEXT,
                ID_REPLACE_ONE => FR_REPLACE,
                ID_REPLACE_ALL => FR_REPLACEALL,
                IDCANCEL => FR_DIALOGTERM,
                _ => return 0,
            };
            report(hwnd, fr, action);
            1
        }
        WM_CLOSE if !fr.is_null() => {
            report(hwnd, unsafe { &mut *fr }, FR_DIALOGTERM);
            1
        }
        0x0002 => {
            unsafe { RemovePropW(hwnd, prop.as_ptr()) };
            0
        }
        _ => 0,
    }
}

/// Copies the dialog's settings into the FINDREPLACE and tells the owner;
/// closing destroys the dialog afterwards.
fn report(hwnd: Hwnd, fr: &mut FindReplace, action: u32) {
    let mut flags = fr.flags & !(FR_FINDNEXT | FR_REPLACE | FR_REPLACEALL | FR_DIALOGTERM | FR_MATCHCASE | FR_DOWN);
    if fr.flags & FR_HIDEMATCHCASE != 0 {
        flags |= fr.flags & FR_MATCHCASE;
    } else if checked(hwnd, ID_CASE) {
        flags |= FR_MATCHCASE;
    }
    // Replace always searches down.
    if !fr.replace.is_null() {
        flags |= FR_DOWN;
    } else if fr.flags & FR_HIDEUPDOWN != 0 {
        flags |= fr.flags & FR_DOWN;
    } else if checked(hwnd, ID_DOWN) {
        flags |= FR_DOWN;
    }
    if action != FR_DIALOGTERM {
        unsafe { put(fr.find, fr.find_len, &dlg_text(hwnd, ID_FIND)) };
        if !fr.replace.is_null() {
            unsafe { put(fr.replace, fr.replace_len, &dlg_text(hwnd, ID_REPLACE)) };
        }
    }
    fr.flags = flags | action;
    unsafe { SendMessageW(fr.owner, find_message(), 0, fr as *mut FindReplace as isize) };
    if action == FR_DIALOGTERM {
        unsafe { DestroyWindow(hwnd) };
    }
}

unsafe fn find_dialog(fr: *mut FindReplace, replace: bool) -> Hwnd {
    LAST_ERROR.store(0, Ordering::Relaxed);
    if fr.is_null() || unsafe { (*fr).find.is_null() || (*fr).find_len == 0 || (replace && (*fr).replace.is_null()) } {
        fail(CDERR_INITIALIZATION);
        return 0;
    }
    let mut t;
    if replace {
        t = Template::new(&"Replace".encode_utf16().collect::<Vec<u16>>(), 230, 94, 9);
        label(&mut t, [4, 9, 48, 8], "Fi&nd what:");
        t.item(0x81, ID_FIND as u16, 0x80 | WS_BORDER | WS_TABSTOP | WS_GROUP, [54, 7, 114, 12], "");
        label(&mut t, [4, 26, 48, 8], "Re&place with:");
        t.item(0x81, ID_REPLACE as u16, 0x80 | WS_BORDER | WS_TABSTOP | WS_GROUP, [54, 24, 114, 12], "");
        t.item(0x80, ID_CASE as u16, 0x3 | WS_TABSTOP | WS_GROUP, [5, 62, 59, 12], "Match &case"); // BS_AUTOCHECKBOX
        button(&mut t, IDOK, true, [174, 4, 50, 14], "&Find Next");
        button(&mut t, ID_REPLACE_ONE, false, [174, 21, 50, 14], "&Replace");
        button(&mut t, ID_REPLACE_ALL, false, [174, 38, 50, 14], "Replace &All");
        button(&mut t, IDCANCEL, false, [174, 55, 50, 14], "Cancel");
    } else {
        t = Template::new(&"Find".encode_utf16().collect::<Vec<u16>>(), 236, 62, 8);
        label(&mut t, [6, 8, 42, 8], "Fi&nd what:");
        t.item(0x81, ID_FIND as u16, 0x80 | WS_BORDER | WS_TABSTOP | WS_GROUP, [47, 7, 128, 12], "");
        t.item(0x80, ID_CASE as u16, 0x3 | WS_TABSTOP | WS_GROUP, [6, 42, 64, 12], "Match &case");
        t.item(0x80, 0xFFFF, 0x7, [107, 26, 68, 28], "Direction"); // BS_GROUPBOX
        t.item(0x80, ID_UP as u16, 0x9 | WS_GROUP | WS_TABSTOP, [111, 38, 25, 12], "&Up"); // BS_AUTORADIOBUTTON
        t.item(0x80, ID_DOWN as u16, 0x9, [138, 38, 35, 12], "&Down");
        button(&mut t, IDOK, true, [182, 5, 50, 14], "&Find Next");
        button(&mut t, IDCANCEL, false, [182, 23, 50, 14], "Cancel");
    }
    // Modeless: shown by its template.
    t.0[3] |= (WS_VISIBLE >> 24) as u8;
    unsafe { CreateDialogIndirectParamW(0, t.0.as_ptr(), (*fr).owner, find_proc, fr as isize) }
}

#[no_mangle]
pub unsafe extern "system" fn FindTextW(fr: *mut u8) -> Hwnd {
    unsafe { find_dialog(fr as *mut FindReplace, false) }
}

#[no_mangle]
pub unsafe extern "system" fn ReplaceTextW(fr: *mut u8) -> Hwnd {
    unsafe { find_dialog(fr as *mut FindReplace, true) }
}

/// The ANSI forms aren't there yet.
#[no_mangle]
pub extern "system" fn FindTextA(_fr: *mut u8) -> Hwnd {
    fail(CDERR_INITIALIZATION);
    0
}

#[no_mangle]
pub extern "system" fn ReplaceTextA(_fr: *mut u8) -> Hwnd {
    fail(CDERR_INITIALIZATION);
    0
}

// -----| fonts |-----

const ID_FACES: i32 = 1136; // lst1
const ID_STYLES: i32 = 1137; // lst2
const ID_SIZES: i32 = 1138; // lst3
const ID_SIZE: i32 = 1152; // edt1
const CF_INITTOLOGFONTSTRUCT: u32 = 0x40;

const STYLES: [&str; 4] = ["Regular", "Italic", "Bold", "Bold Italic"];
const SIZES: [i32; 16] = [8, 9, 10, 11, 12, 14, 16, 18, 20, 22, 24, 26, 28, 36, 48, 72];

/// What the font dialog edits: a LOGFONTW (92 bytes) and the point size.
struct FontDialog {
    logfont: [u8; 92],
    points: i32,
}

unsafe extern "system" fn add_face(lf: *const u8, _tm: *const u8, _kind: u32, lp: isize) -> i32 {
    let hwnd = lp as Hwnd;
    let name = unsafe { wstr(lf.add(28) as *const u16) };
    let z = z(&name);
    if unsafe { SendDlgItemMessageW(hwnd, ID_FACES, LB_FINDSTRINGEXACT, usize::MAX, z.as_ptr() as isize) } < 0 {
        unsafe { SendDlgItemMessageW(hwnd, ID_FACES, LB_ADDSTRING, 0, z.as_ptr() as isize) };
    }
    1
}

fn select(hwnd: Hwnd, id: i32, s: &[u16]) {
    let i = unsafe { SendDlgItemMessageW(hwnd, id, LB_FINDSTRINGEXACT, usize::MAX, z(s).as_ptr() as isize) };
    unsafe { SendDlgItemMessageW(hwnd, id, LB_SETCURSEL, i.max(0) as usize, 0) };
}

fn number(s: &[u16]) -> i32 {
    s.iter().take_while(|c| (b'0' as u16..=b'9' as u16).contains(c)).fold(0, |n, &c| n * 10 + (c - b'0' as u16) as i32)
}

fn digits(n: i32) -> Vec<u16> {
    alloc::format!("{}", n).encode_utf16().collect()
}

unsafe extern "system" fn font_proc(hwnd: Hwnd, msg: u32, wp: usize, lp: isize) -> isize {
    let prop = state_prop();
    match msg {
        WM_INITDIALOG => {
            unsafe { SetPropW(hwnd, prop.as_ptr(), lp) };
            let fd = unsafe { &*(lp as *const FontDialog) };
            let hdc = unsafe { GetDC(0) };
            let mut query = [0u8; 92];
            query[23] = 1; // DEFAULT_CHARSET
            unsafe { EnumFontFamiliesExW(hdc, query.as_ptr(), add_face, hwnd as isize, 0) };
            unsafe { ReleaseDC(0, hdc) };
            for s in STYLES {
                unsafe { SendDlgItemMessageW(hwnd, ID_STYLES, LB_ADDSTRING, 0, wide(s).as_ptr() as isize) };
            }
            for s in SIZES {
                unsafe { SendDlgItemMessageW(hwnd, ID_SIZES, LB_ADDSTRING, 0, z(&digits(s)).as_ptr() as isize) };
            }
            let face = unsafe { wstr(fd.logfont.as_ptr().add(28) as *const u16) };
            select(hwnd, ID_FACES, &face);
            let weight = i32::from_le_bytes([fd.logfont[16], fd.logfont[17], fd.logfont[18], fd.logfont[19]]);
            let style = (fd.logfont[20] != 0) as usize + if weight >= 600 { 2 } else { 0 };
            unsafe { SendDlgItemMessageW(hwnd, ID_STYLES, LB_SETCURSEL, style, 0) };
            select(hwnd, ID_SIZES, &digits(fd.points));
            set_text(hwnd, ID_SIZE, &digits(fd.points));
            1
        }
        WM_COMMAND => {
            let fd = unsafe { GetPropW(hwnd, prop.as_ptr()) } as *mut FontDialog;
            if fd.is_null() {
                return 0;
            }
            let fd = unsafe { &mut *fd };
            let (id, code) = ((wp & 0xFFFF) as u16, (wp >> 16) as u16);
            match (id as i32, code) {
                (1, _) => {
                    let points = number(&dlg_text(hwnd, ID_SIZE));
                    if !(1..=1638).contains(&points) {
                        unsafe { MessageBoxW(hwnd, wide("Size must be a number.").as_ptr(), wide("Font").as_ptr(), 0x30) };
                        return 1;
                    }
                    let face_sel = unsafe { SendDlgItemMessageW(hwnd, ID_FACES, LB_GETCURSEL, 0, 0) };
                    if face_sel >= 0 {
                        let mut face = list_text(hwnd, ID_FACES, face_sel);
                        face.truncate(31);
                        fd.logfont[28..92].fill(0);
                        for (i, c) in face.iter().enumerate() {
                            fd.logfont[28 + i * 2..30 + i * 2].copy_from_slice(&c.to_le_bytes());
                        }
                    }
                    let style = unsafe { SendDlgItemMessageW(hwnd, ID_STYLES, LB_GETCURSEL, 0, 0) }.max(0);
                    let height = -unsafe { MulDiv(points, 96, 72) };
                    fd.logfont[0..4].copy_from_slice(&height.to_le_bytes());
                    fd.logfont[4..8].fill(0);
                    fd.logfont[16..20].copy_from_slice(&(if style >= 2 { 700i32 } else { 400 }).to_le_bytes());
                    fd.logfont[20] = (style & 1) as u8;
                    fd.points = points;
                    unsafe { EndDialog(hwnd, 1) };
                }
                (2, _) => unsafe {
                    EndDialog(hwnd, 0);
                },
                (ID_SIZES, LBN_SELCHANGE) => {
                    let sel = unsafe { SendDlgItemMessageW(hwnd, ID_SIZES, LB_GETCURSEL, 0, 0) };
                    set_text(hwnd, ID_SIZE, &list_text(hwnd, ID_SIZES, sel));
                }
                _ => return 0,
            }
            1
        }
        WM_CLOSE => {
            unsafe { EndDialog(hwnd, 0) };
            1
        }
        0x0002 => {
            unsafe { RemovePropW(hwnd, prop.as_ptr()) };
            0
        }
        _ => 0,
    }
}

/// CHOOSEFONT: lStructSize, hwndOwner, hDC, lpLogFont, iPointSize, Flags...
/// The LOGFONT is LOGFONTW (92 bytes) or LOGFONTA (60: a byte face name).
unsafe fn choose_font(cf: *mut u8, unicode: bool) -> Bool {
    LAST_ERROR.store(0, Ordering::Relaxed);
    if cf.is_null() {
        return fail(CDERR_INITIALIZATION);
    }
    let owner = unsafe { core::ptr::read_unaligned(cf.add(8) as *const u64) };
    let lf = unsafe { core::ptr::read_unaligned(cf.add(24) as *const u64) } as *mut u8;
    let flags = unsafe { core::ptr::read_unaligned(cf.add(36) as *const u32) };
    if lf.is_null() {
        return fail(CDERR_INITIALIZATION);
    }
    let mut fd = FontDialog { logfont: [0; 92], points: 11 };
    if flags & CF_INITTOLOGFONTSTRUCT != 0 {
        unsafe {
            core::ptr::copy_nonoverlapping(lf, fd.logfont.as_mut_ptr(), 28);
            let face = if unicode { wstr(lf.add(28) as *const u16) } else { astr(lf.add(28)) };
            for (i, c) in face.iter().take(31).enumerate() {
                fd.logfont[28 + i * 2..30 + i * 2].copy_from_slice(&c.to_le_bytes());
            }
        }
        let height = i32::from_le_bytes([fd.logfont[0], fd.logfont[1], fd.logfont[2], fd.logfont[3]]);
        if height != 0 {
            fd.points = unsafe { MulDiv(height.abs(), 72, 96) };
        }
    } else {
        let face: Vec<u16> = "Segoe UI".encode_utf16().collect();
        for (i, c) in face.iter().enumerate() {
            fd.logfont[28 + i * 2..30 + i * 2].copy_from_slice(&c.to_le_bytes());
        }
        fd.logfont[16..20].copy_from_slice(&400i32.to_le_bytes());
    }
    let mut t = Template::new(&"Font".encode_utf16().collect::<Vec<u16>>(), 250, 140, 9);
    label(&mut t, [7, 7, 98, 9], "&Font:");
    t.item(0x83, ID_FACES as u16, 0x1 | WS_BORDER | WS_VSCROLL | WS_TABSTOP, [7, 18, 98, 96], "");
    label(&mut t, [112, 7, 62, 9], "Font st&yle:");
    t.item(0x83, ID_STYLES as u16, 0x1 | WS_BORDER | WS_TABSTOP, [112, 18, 62, 50], "");
    label(&mut t, [181, 7, 62, 9], "&Size:");
    t.item(0x81, ID_SIZE as u16, 0x80 | WS_BORDER | WS_TABSTOP, [181, 18, 62, 12], "");
    t.item(0x83, ID_SIZES as u16, 0x1 | WS_BORDER | WS_VSCROLL | WS_TABSTOP, [181, 32, 62, 82], "");
    button(&mut t, IDOK, true, [136, 120, 50, 14], "OK");
    button(&mut t, IDCANCEL, false, [193, 120, 50, 14], "Cancel");
    let r = unsafe { DialogBoxIndirectParamW(0, t.0.as_ptr(), owner, font_proc, &mut fd as *mut FontDialog as isize) };
    if r != 1 {
        return 0;
    }
    unsafe {
        core::ptr::copy_nonoverlapping(fd.logfont.as_ptr(), lf, 28);
        let face = wstr(fd.logfont.as_ptr().add(28) as *const u16);
        if unicode {
            core::ptr::copy_nonoverlapping(fd.logfont.as_ptr().add(28), lf.add(28), 64);
        } else {
            core::ptr::write_bytes(lf.add(28), 0, 32);
            write_string(lf.add(28) as u64, &face[..face.len().min(31)], false);
        }
        core::ptr::write_unaligned(cf.add(32) as *mut i32, fd.points * 10);
    }
    1
}

#[no_mangle]
pub unsafe extern "system" fn ChooseFontW(cf: *mut u8) -> Bool {
    unsafe { choose_font(cf, true) }
}

#[no_mangle]
pub unsafe extern "system" fn ChooseFontA(cf: *mut u8) -> Bool {
    unsafe { choose_font(cf, false) }
}

/// No color picker yet: as if the user cancelled.
#[no_mangle]
pub extern "system" fn ChooseColorW(_cc: *mut u8) -> Bool {
    LAST_ERROR.store(0, Ordering::Relaxed);
    0
}

#[no_mangle]
pub extern "system" fn ChooseColorA(_cc: *mut u8) -> Bool {
    LAST_ERROR.store(0, Ordering::Relaxed);
    0
}

// -----| printing: no printers |-----

#[no_mangle]
pub extern "system" fn PrintDlgW(_pd: *mut u8) -> Bool {
    fail(PDERR_NODEFAULTPRN)
}

#[no_mangle]
pub extern "system" fn PrintDlgA(_pd: *mut u8) -> Bool {
    fail(PDERR_NODEFAULTPRN)
}

#[no_mangle]
pub extern "system" fn PageSetupDlgW(_psd: *mut u8) -> Bool {
    fail(PDERR_NODEFAULTPRN)
}

#[no_mangle]
pub extern "system" fn PageSetupDlgA(_psd: *mut u8) -> Bool {
    fail(PDERR_NODEFAULTPRN)
}

/// PRINTDLGEX: S_OK with dwResultAction (at 132) PD_RESULT_CANCEL, and
/// the extended error says there's no printer.
#[no_mangle]
pub unsafe extern "system" fn PrintDlgExW(pd: *mut u8) -> i32 {
    LAST_ERROR.store(PDERR_NODEFAULTPRN, Ordering::Relaxed);
    if pd.is_null() {
        return 0x8007_0057u32 as i32;
    }
    unsafe { core::ptr::write_unaligned(pd.add(132) as *mut u32, 0) };
    0x8000_4005u32 as i32
}

#[no_mangle]
pub unsafe extern "system" fn PrintDlgExA(pd: *mut u8) -> i32 {
    unsafe { PrintDlgExW(pd) }
}
