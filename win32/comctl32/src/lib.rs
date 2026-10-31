//! VEILOS `comctl32.dll`: the common controls (WIN32_DESIGN.md, G4).
//!
//! Built on our user32 and gdi32 like any program: the controls are window
//! classes registered when the DLL loads, drawn in the classic look.
//!
//!   lib.rs        initialization, window subclassing, icons by metric
//!   imagelist.rs  image lists
//!   status.rs     the status bar
//!   controls.rs   progress bar, trackbar, up-down control, tooltips

#![no_std]
// build.rs gives some exports their Windows ordinals; the linker then sees
// those names twice (with and without the ordinal) and says so.
#![allow(linker_messages)]

extern crate alloc;

mod controls;
#[path = "../../dllrt.rs"]
mod dllrt;
mod imagelist;
mod status;

use alloc::vec::Vec;
use dllrt::Lock;

pub(crate) type Hwnd = u64;
pub(crate) type Bool = i32;

#[link(name = "user32", kind = "raw-dylib")]
unsafe extern "system" {
    pub(crate) fn RegisterClassExW(wc: *const u8) -> u16;
    pub(crate) fn DefWindowProcW(hwnd: Hwnd, msg: u32, wp: u64, lp: i64) -> i64;
    pub(crate) fn SendMessageW(hwnd: Hwnd, msg: u32, wp: u64, lp: i64) -> i64;
    pub(crate) fn GetWindowLongPtrW(hwnd: Hwnd, index: i32) -> i64;
    pub(crate) fn SetWindowLongPtrW(hwnd: Hwnd, index: i32, v: i64) -> i64;
    pub(crate) fn GetParent(hwnd: Hwnd) -> Hwnd;
    pub(crate) fn GetClientRect(hwnd: Hwnd, r: *mut [i32; 4]) -> Bool;
    pub(crate) fn GetWindowRect(hwnd: Hwnd, r: *mut [i32; 4]) -> Bool;
    pub(crate) fn SetWindowPos(hwnd: Hwnd, after: Hwnd, x: i32, y: i32, cx: i32, cy: i32, flags: u32) -> Bool;
    pub(crate) fn InvalidateRect(hwnd: Hwnd, r: *const [i32; 4], erase: Bool) -> Bool;
    pub(crate) fn BeginPaint(hwnd: Hwnd, ps: *mut u8) -> u64;
    pub(crate) fn EndPaint(hwnd: Hwnd, ps: *const u8) -> Bool;
    pub(crate) fn GetDC(hwnd: Hwnd) -> u64;
    pub(crate) fn ReleaseDC(hwnd: Hwnd, hdc: u64) -> i32;
    pub(crate) fn FillRect(hdc: u64, r: *const [i32; 4], brush: u64) -> i32;
    pub(crate) fn GetSysColorBrush(i: i32) -> u64;
    pub(crate) fn GetSysColor(i: i32) -> u32;
    pub(crate) fn DrawTextW(hdc: u64, s: *mut u16, n: i32, r: *mut [i32; 4], flags: u32) -> i32;
    pub(crate) fn GetDlgCtrlID(hwnd: Hwnd) -> i32;
    pub(crate) fn GetWindow(hwnd: Hwnd, cmd: u32) -> Hwnd;
    pub(crate) fn SetWindowTextW(hwnd: Hwnd, s: *const u16) -> Bool;
    pub(crate) fn MapWindowPoints(from: Hwnd, to: Hwnd, pts: *mut [i32; 2], n: u32) -> i32;
    pub(crate) fn SetFocus(hwnd: Hwnd) -> Hwnd;
    pub(crate) fn IsWindowUnicode(hwnd: Hwnd) -> Bool;
    pub(crate) fn IsZoomed(hwnd: Hwnd) -> Bool;
    pub(crate) fn LoadImageW(inst: u64, name: *const u16, kind: u32, cx: i32, cy: i32, flags: u32) -> u64;
    pub(crate) fn GetIconInfo(icon: u64, info: *mut u8) -> Bool;
    pub(crate) fn CreateIconIndirect(info: *const u8) -> u64;
    pub(crate) fn CreateWindowExW(ex: u32, class: *const u16, name: *const u16, style: u32, x: i32, y: i32, w: i32, h: i32,
                                  parent: Hwnd, menu: u64, inst: u64, param: u64) -> Hwnd;
    pub(crate) fn TrackMouseEvent(tme: *mut u8) -> Bool;
    pub(crate) fn VeilCallProc(proc: usize, proc_unicode: Bool, hwnd: Hwnd, msg: u32, wp: u64, lp: i64, from_unicode: Bool) -> i64;
}

#[link(name = "gdi32", kind = "raw-dylib")]
unsafe extern "system" {
    pub(crate) fn SelectObject(hdc: u64, obj: u64) -> u64;
    pub(crate) fn SetBkMode(hdc: u64, mode: i32) -> i32;
    pub(crate) fn SetTextColor(hdc: u64, color: u32) -> u32;
    pub(crate) fn GetTextMetricsW(hdc: u64, tm: *mut u8) -> Bool;
    pub(crate) fn GetStockObject(i: i32) -> u64;
    pub(crate) fn CreateCompatibleDC(hdc: u64) -> u64;
    pub(crate) fn DeleteDC(hdc: u64) -> Bool;
    pub(crate) fn DeleteObject(h: u64) -> Bool;
    pub(crate) fn GetObjectW(h: u64, size: i32, out: *mut u8) -> i32;
    pub(crate) fn GetDIBits(hdc: u64, hbm: u64, start: u32, lines: u32, bits: *mut u8, bmi: *mut u8, usage: u32) -> i32;
    pub(crate) fn CreateDIBSection(hdc: u64, bmi: *const u8, usage: u32, bits: *mut *mut u8, section: u64, offset: u32) -> u64;
    pub(crate) fn CreateBitmap(w: i32, h: i32, planes: u32, bpp: u32, bits: *const u8) -> u64;
    pub(crate) fn GdiAlphaBlend(dst: u64, x: i32, y: i32, w: i32, h: i32, src: u64, sx: i32, sy: i32, sw: i32, sh: i32, blend: u32) -> Bool;
    pub(crate) fn CreateSolidBrush(color: u32) -> u64;
}

pub(crate) const WM_NOTIFY: u32 = 0x004E;

/// Register a window class whose procedure takes Unicode messages, with
/// `extra` bytes per window.
pub(crate) fn register(name: &str, proc: usize, cursor_arrow: bool) {
    let class: Vec<u16> = name.encode_utf16().chain(core::iter::once(0)).collect();
    // WNDCLASSEXW: cbSize, style (CS_GLOBALCLASS | CS_DBLCLKS |
    // CS_HREDRAW | CS_VREDRAW), proc, extra, instance, icon, cursor,
    // brush, menu, name, small icon.
    let mut wc = [0u8; 80];
    wc[0..4].copy_from_slice(&80u32.to_le_bytes());
    wc[4..8].copy_from_slice(&0x400Bu32.to_le_bytes());
    wc[8..16].copy_from_slice(&(proc as u64).to_le_bytes());
    let _ = cursor_arrow;
    wc[64..72].copy_from_slice(&(class.as_ptr() as u64).to_le_bytes());
    unsafe { RegisterClassExW(wc.as_ptr()) };
}

/// The font a control was given, or DEFAULT_GUI_FONT.
pub(crate) fn font_or_default(font: u64) -> u64 {
    if font != 0 { font } else { unsafe { GetStockObject(17) } }
}

/// WM_NOTIFY to the parent: NMHDR (from, id, code) followed by `extra`.
pub(crate) fn notify(hwnd: Hwnd, code: i32, extra: &[u8]) -> i64 {
    let parent = unsafe { GetParent(hwnd) };
    let id = unsafe { GetDlgCtrlID(hwnd) } as u64;
    let mut buf: Vec<u8> = Vec::with_capacity(24 + extra.len());
    buf.extend_from_slice(&hwnd.to_le_bytes());
    buf.extend_from_slice(&id.to_le_bytes());
    buf.extend_from_slice(&(code as u32).to_le_bytes());
    buf.extend_from_slice(&[0, 0, 0, 0]);
    buf.extend_from_slice(extra);
    unsafe { SendMessageW(parent, WM_NOTIFY, id, buf.as_mut_ptr() as i64) }
}

#[no_mangle]
pub extern "system" fn DllMain(_module: usize, reason: u32, _reserved: usize) -> i32 {
    if reason == 1 {
        status::register_class();
        controls::register_classes();
    }
    1
}

#[no_mangle]
pub extern "system" fn InitCommonControls() {}

#[no_mangle]
pub extern "system" fn InitCommonControlsEx(_icc: *const u8) -> Bool {
    1
}

/// DLLVERSIONINFO: size, major, minor, build, platform. Version 6.10.
#[no_mangle]
pub unsafe extern "system" fn DllGetVersion(info: *mut u32) -> i32 {
    if info.is_null() {
        return 0x8007_0057u32 as i32;
    }
    unsafe {
        *info.add(1) = 6;
        *info.add(2) = 10;
        *info.add(3) = 19041;
        *info.add(4) = 2;
    }
    0
}

#[no_mangle]
pub extern "system" fn _TrackMouseEvent(tme: *mut u8) -> Bool {
    unsafe { TrackMouseEvent(tme) }
}

/// LIM_SMALL (0): SM_CXSMICON; LIM_LARGE (1): SM_CXICON.
#[no_mangle]
pub unsafe extern "system" fn LoadIconMetric(inst: u64, name: *const u16, metric: i32, out: *mut u64) -> i32 {
    let size = if metric == 0 { 16 } else { 32 };
    unsafe { LoadIconWithScaleDown(inst, name, size, size, out) }
}

#[no_mangle]
pub unsafe extern "system" fn LoadIconWithScaleDown(inst: u64, name: *const u16, cx: i32, cy: i32, out: *mut u64) -> i32 {
    let h = unsafe { LoadImageW(inst, name, 1, cx, cy, 0) };
    if !out.is_null() {
        unsafe { *out = h };
    }
    if h != 0 { 0 } else { 0x8007_0002u32 as i32 }
}

// -----| window subclassing |-----
//
// SetWindowSubclass puts a procedure of ours in the window, which calls
// the newest subclass procedure; DefSubclassProc calls the next older one,
// and at the end the window's own procedure.

struct Chain {
    hwnd: Hwnd,
    /// The window's own procedure, and whether it takes Unicode.
    original: usize,
    unicode: bool,
    /// (SUBCLASSPROC, id, reference data), oldest first.
    procs: Vec<(usize, u64, u64)>,
    /// The position in `procs` each nested call is at.
    depth: Vec<usize>,
}

static CHAINS: Lock<Vec<Chain>> = Lock::new(Vec::new());

/// Call chain entry `i` of `hwnd` (or the original procedure when `i` is
/// past the oldest).
fn call_entry(hwnd: Hwnd, i: Option<usize>, msg: u32, wp: u64, lp: i64) -> i64 {
    let entry = {
        let mut cs = CHAINS.lock();
        let Some(c) = cs.iter_mut().find(|c| c.hwnd == hwnd) else { return 0 };
        match i {
            Some(i) => {
                c.depth.push(i);
                Ok(c.procs[i])
            }
            None => Err((c.original, c.unicode)),
        }
    };
    match entry {
        Ok((proc, id, data)) => {
            // SUBCLASSPROC(hwnd, msg, wParam, lParam, id, data)
            let f: unsafe extern "system" fn(Hwnd, u32, u64, i64, u64, u64) -> i64 = unsafe { core::mem::transmute(proc) };
            let r = unsafe { f(hwnd, msg, wp, lp, id, data) };
            if let Some(c) = CHAINS.lock().iter_mut().find(|c| c.hwnd == hwnd) {
                c.depth.pop();
            }
            if msg == 0x0082 {
                // WM_NCDESTROY: the chain goes with the window.
                CHAINS.lock().retain(|c| c.hwnd != hwnd);
            }
            r
        }
        Err((original, unicode)) => unsafe { VeilCallProc(original, unicode as Bool, hwnd, msg, wp, lp, 1) },
    }
}

unsafe extern "system" fn subclass_proc(hwnd: Hwnd, msg: u32, wp: u64, lp: i64) -> i64 {
    let top = CHAINS.lock().iter().find(|c| c.hwnd == hwnd).map(|c| c.procs.len());
    match top {
        Some(n) if n > 0 => call_entry(hwnd, Some(n - 1), msg, wp, lp),
        _ => call_entry(hwnd, None, msg, wp, lp),
    }
}

#[no_mangle]
pub extern "system" fn SetWindowSubclass(hwnd: Hwnd, proc: usize, id: u64, data: u64) -> Bool {
    if proc == 0 {
        return 0;
    }
    let mut cs = CHAINS.lock();
    match cs.iter_mut().find(|c| c.hwnd == hwnd) {
        Some(c) => match c.procs.iter_mut().find(|p| p.0 == proc && p.1 == id) {
            Some(p) => p.2 = data,
            None => c.procs.push((proc, id, data)),
        },
        None => {
            drop(cs);
            let unicode = unsafe { IsWindowUnicode(hwnd) } != 0;
            let original = unsafe { GetWindowLongPtrW(hwnd, -4) } as usize;
            if original == 0 {
                return 0;
            }
            CHAINS.lock().push(Chain { hwnd, original, unicode, procs: alloc::vec![(proc, id, data)], depth: Vec::new() });
            unsafe { SetWindowLongPtrW(hwnd, -4, subclass_proc as *const () as i64) };
        }
    }
    1
}

#[no_mangle]
pub unsafe extern "system" fn GetWindowSubclass(hwnd: Hwnd, proc: usize, id: u64, data: *mut u64) -> Bool {
    let found = CHAINS.lock().iter().find(|c| c.hwnd == hwnd).and_then(|c| c.procs.iter().find(|p| p.0 == proc && p.1 == id).map(|p| p.2));
    if let (Some(d), false) = (found, data.is_null()) {
        unsafe { *data = d };
    }
    found.is_some() as Bool
}

#[no_mangle]
pub extern "system" fn RemoveWindowSubclass(hwnd: Hwnd, proc: usize, id: u64) -> Bool {
    let mut cs = CHAINS.lock();
    let Some(c) = cs.iter_mut().find(|c| c.hwnd == hwnd) else { return 0 };
    let before = c.procs.len();
    c.procs.retain(|p| !(p.0 == proc && p.1 == id));
    let removed = before != c.procs.len();
    // The last one out puts the window's own procedure back (unless a call
    // through the chain is still running).
    if c.procs.is_empty() && c.depth.is_empty() {
        let original = c.original;
        cs.retain(|c| c.hwnd != hwnd);
        drop(cs);
        unsafe { SetWindowLongPtrW(hwnd, -4, original as i64) };
    }
    removed as Bool
}

#[no_mangle]
pub extern "system" fn DefSubclassProc(hwnd: Hwnd, msg: u32, wp: u64, lp: i64) -> i64 {
    let at = CHAINS.lock().iter().find(|c| c.hwnd == hwnd).and_then(|c| c.depth.last().copied());
    match at {
        Some(i) if i > 0 => call_entry(hwnd, Some(i - 1), msg, wp, lp),
        _ => call_entry(hwnd, None, msg, wp, lp),
    }
}
