//! Pixels and painting: each top-level window's client pixels (presented
//! to the kernel after drawing), DCs on them for a window and its children,
//! update rectangles, WM_ERASEBKGND and WM_PAINT; system colors and
//! metrics; the rectangle helpers.
//!
//! A child window draws into its top-level window's pixels, so when a
//! window is invalidated its children inside the area are too, and are
//! painted after it.

use alloc::vec::Vec;

use crate::message::{send, WM_ERASEBKGND, WM_NCPAINT, WM_PAINT};
use crate::{empty, intersect, state, union, Bool, Hwnd, Wnd};

const DESKTOP: Hwnd = 0x0001_0000;

/// Give a top-level window room for its client area. The pixels never
/// move while big enough (a DC may still point at them), and are kept
/// (leaked) when they must grow.
pub fn ensure_surface(w: &mut Wnd) {
    let (cw, ch) = (w.client_w().max(1) as usize, w.client_h().max(1) as usize);
    let rows = if w.stride == 0 { 0 } else { w.surface.len() / w.stride };
    if cw <= w.stride && ch <= rows {
        return;
    }
    let (sw, sh) = crate::screen_size();
    let stride = cw.max(sw as usize).max(w.stride);
    let rows = ch.max(sh as usize).max(rows);
    let old = core::mem::replace(&mut w.surface, alloc::vec![0x00FF_FFFF; stride * rows]);
    core::mem::forget(old);
    w.stride = stride;
}

/// Send the drawn part of a top-level window's pixels to the kernel.
pub fn present(root: Hwnd) {
    let mut s = state();
    let Some(w) = s.get_mut(root) else { return };
    let Some(d) = w.dirty.take() else { return };
    let d = intersect(d, [0, 0, w.client_w(), w.client_h()]);
    if !w.kernel || empty(d) {
        return;
    }
    let rect = [d[0], d[1], d[2] - d[0], d[3] - d[1]];
    unsafe { crate::VeilUserPresent(root, w.surface.as_ptr(), w.stride as u64, &rect) };
}

/// Present everything this thread has drawn.
pub fn present_all() {
    let tid = unsafe { crate::GetCurrentThreadId() };
    let roots: Vec<Hwnd> = state().windows.iter().filter(|w| w.tid == tid && w.dirty.is_some()).map(|w| w.hwnd).collect();
    for r in roots {
        present(r);
    }
}

// -----| DCs |-----

/// DCs handed out by GetDC/BeginPaint: (hdc, top-level window).
static DCS: crate::dllrt::Lock<Vec<(u64, Hwnd)>> = crate::dllrt::Lock::new(Vec::new());

/// A DC on `hwnd`'s client area, drawing only inside `clip` (client
/// coordinates, None: all of it); `frame`: on the whole window rectangle
/// instead (its non-client area).
fn make_dc(hwnd: Hwnd, clip: Option<[i32; 4]>, frame: bool) -> u64 {
    if hwnd == 0 || hwnd == DESKTOP {
        let (w, h) = crate::screen_size();
        return unsafe { crate::VeilGdiCreateDC(core::ptr::null_mut(), w, h, w as usize, 0, 0, core::ptr::null()) };
    }
    let mut s = state();
    let root = s.root(hwnd);
    let (mut ox, mut oy) = s.client_origin(hwnd);
    let mut area = s.visible_clip(hwnd);
    if frame {
        if let Some(w) = s.get(hwnd).filter(|w| !w.top_level()) {
            ox -= w.margins[0];
            oy -= w.margins[1];
            area = [ox, oy, ox + w.w, oy + w.h];
            if let Some(p) = s.get(w.parent) {
                let _ = p;
                area = intersect(area, s.visible_clip(w.parent));
            }
        }
    }
    if let Some(c) = clip {
        area = intersect(area, [c[0] + ox, c[1] + oy, c[2] + ox, c[3] + oy]);
    }
    let Some(r) = s.get_mut(root) else { return 0 };
    let (ptr, w, h, stride) = (r.surface.as_mut_ptr(), r.client_w(), r.client_h(), r.stride);
    if !empty(area) {
        r.dirty = Some(union(r.dirty, area));
    }
    drop(s);
    let hdc = unsafe { crate::VeilGdiCreateDC(ptr, w, h, stride, ox, oy, &area) };
    DCS.lock().push((hdc, root));
    hdc
}

fn release(hdc: u64) -> bool {
    let root = {
        let mut dcs = DCS.lock();
        let Some(i) = dcs.iter().position(|d| d.0 == hdc) else { return false };
        dcs.swap_remove(i).1
    };
    unsafe { crate::DeleteDC(hdc) };
    present(root);
    true
}

#[no_mangle]
pub extern "system" fn GetDC(hwnd: Hwnd) -> u64 {
    make_dc(hwnd, None, false)
}

#[no_mangle]
pub extern "system" fn GetDCEx(hwnd: Hwnd, _rgn: u64, _flags: u32) -> u64 {
    make_dc(hwnd, None, false)
}

#[no_mangle]
pub extern "system" fn GetWindowDC(hwnd: Hwnd) -> u64 {
    make_dc(hwnd, None, true)
}

#[no_mangle]
pub extern "system" fn ReleaseDC(_hwnd: Hwnd, hdc: u64) -> i32 {
    if !release(hdc) {
        // The screen DC isn't in the list.
        unsafe { crate::DeleteDC(hdc) };
    }
    1
}

// -----| update rectangles |-----

/// Add `rect` (client coordinates; None: all of it) to `hwnd`'s update
/// rectangle, and to its children's where they overlap. `hwnd` 0: every
/// window of this thread.
pub fn invalidate(hwnd: Hwnd, rect: Option<[i32; 4]>, erase: bool) {
    if hwnd == 0 || hwnd == DESKTOP {
        let tid = unsafe { crate::GetCurrentThreadId() };
        let roots: Vec<Hwnd> = state().windows.iter().filter(|w| w.parent == 0 && w.tid == tid).map(|w| w.hwnd).collect();
        for r in roots {
            invalidate(r, None, erase);
        }
        return;
    }
    let (children, tid) = {
        let mut s = state();
        let Some(w) = s.get_mut(hwnd) else { return };
        if !w.visible {
            return;
        }
        let all = [0, 0, w.client_w(), w.client_h()];
        let r = intersect(rect.unwrap_or(all), all);
        if empty(r) {
            return;
        }
        w.update = Some(union(w.update, r));
        w.erase |= erase;
        let tid = w.tid;
        let kids: Vec<(Hwnd, [i32; 4])> = s
            .windows
            .iter()
            .filter(|c| c.parent == hwnd && c.visible && !c.destroying)
            .map(|c| (c.hwnd, [r[0] - c.x - c.margins[0], r[1] - c.y - c.margins[1], r[2] - c.x - c.margins[0], r[3] - c.y - c.margins[1]]))
            .collect();
        (kids, tid)
    };
    for (c, r) in children {
        invalidate(c, Some(r), erase);
    }
    crate::message::wake(tid);
}

/// Send WM_ERASEBKGND now if `hwnd`'s update rectangle needs erasing.
pub fn erase_now(hwnd: Hwnd) {
    let update = {
        let mut s = state();
        let Some(w) = s.get_mut(hwnd) else { return };
        if !w.erase {
            return;
        }
        w.erase = false;
        w.update
    };
    let hdc = make_dc(hwnd, update, false);
    send(hwnd, WM_ERASEBKGND, hdc, 0);
    release(hdc);
}

/// UpdateWindow: WM_PAINT now for `hwnd` and its children that need it.
pub fn update_now(hwnd: Hwnd) {
    if state().get(hwnd).is_some_and(|w| w.update.is_some()) {
        send(hwnd, WM_PAINT, 0, 0);
    }
    let kids = state().children(hwnd);
    for k in kids {
        update_now(k);
    }
}

fn rect_arg(r: *const [i32; 4]) -> Option<[i32; 4]> {
    (!r.is_null()).then(|| unsafe { *r })
}

#[no_mangle]
pub extern "system" fn InvalidateRect(hwnd: Hwnd, r: *const [i32; 4], erase: Bool) -> Bool {
    if hwnd != 0 && state().get(hwnd).is_none() {
        return 0;
    }
    invalidate(hwnd, rect_arg(r), erase != 0);
    1
}

/// Regions come with G2: the whole window.
#[no_mangle]
pub extern "system" fn InvalidateRgn(hwnd: Hwnd, _rgn: u64, erase: Bool) -> Bool {
    InvalidateRect(hwnd, core::ptr::null(), erase)
}

fn validate(hwnd: Hwnd, r: Option<[i32; 4]>) -> Bool {
    let mut s = state();
    let Some(w) = s.get_mut(hwnd) else { return 0 };
    match (r, w.update) {
        (None, _) => w.update = None,
        (Some(r), Some(u)) if r[0] <= u[0] && r[1] <= u[1] && r[2] >= u[2] && r[3] >= u[3] => w.update = None,
        _ => {}
    }
    if w.update.is_none() {
        w.erase = false;
    }
    1
}

#[no_mangle]
pub extern "system" fn ValidateRect(hwnd: Hwnd, r: *const [i32; 4]) -> Bool {
    validate(hwnd, rect_arg(r))
}

#[no_mangle]
pub extern "system" fn ValidateRgn(hwnd: Hwnd, _rgn: u64) -> Bool {
    validate(hwnd, None)
}

#[no_mangle]
pub extern "system" fn GetUpdateRect(hwnd: Hwnd, out: *mut [i32; 4], erase: Bool) -> Bool {
    let u = state().get(hwnd).and_then(|w| w.update);
    if !out.is_null() {
        unsafe { *out = u.unwrap_or([0; 4]) };
    }
    if u.is_some() && erase != 0 {
        erase_now(hwnd);
    }
    u.is_some() as Bool
}

const RDW_INVALIDATE: u32 = 0x0001;
const RDW_ERASE: u32 = 0x0004;
const RDW_VALIDATE: u32 = 0x0008;
const RDW_ERASENOW: u32 = 0x0200;
const RDW_UPDATENOW: u32 = 0x0100;

#[no_mangle]
pub extern "system" fn RedrawWindow(hwnd: Hwnd, r: *const [i32; 4], _rgn: u64, flags: u32) -> Bool {
    let hwnd = if hwnd == 0 { DESKTOP } else { hwnd };
    if flags & RDW_INVALIDATE != 0 {
        invalidate(hwnd, rect_arg(r), flags & RDW_ERASE != 0);
    }
    if flags & RDW_VALIDATE != 0 {
        validate(hwnd, rect_arg(r));
    }
    if flags & (RDW_ERASENOW | RDW_UPDATENOW) != 0 && hwnd != DESKTOP {
        erase_now(hwnd);
    }
    if flags & RDW_UPDATENOW != 0 && hwnd != DESKTOP {
        update_now(hwnd);
    }
    1
}

// -----| BeginPaint / EndPaint |-----

/// PAINTSTRUCT: hdc, fErase, rcPaint, fRestore, fIncUpdate, reserved.
#[no_mangle]
pub extern "system" fn BeginPaint(hwnd: Hwnd, ps: *mut u8) -> u64 {
    let (update, erase, framed) = {
        let mut s = state();
        let Some(w) = s.get_mut(hwnd) else { return 0 };
        let u = w.update.take().unwrap_or([0; 4]);
        let e = core::mem::replace(&mut w.erase, false);
        (u, e, !w.top_level() && w.margins != [0; 4])
    };
    if framed {
        send(hwnd, WM_NCPAINT, 1, 0);
    }
    let hdc = make_dc(hwnd, Some(update), false);
    let not_erased = erase && send(hwnd, WM_ERASEBKGND, hdc, 0) == 0;
    if !ps.is_null() {
        unsafe {
            core::ptr::write_bytes(ps, 0, 72);
            *(ps as *mut u64) = hdc;
            *(ps.add(8) as *mut i32) = not_erased as i32;
            *(ps.add(12) as *mut [i32; 4]) = update;
        }
    }
    hdc
}

#[no_mangle]
pub extern "system" fn EndPaint(_hwnd: Hwnd, ps: *const u8) -> Bool {
    if !ps.is_null() {
        release(unsafe { *(ps as *const u64) });
    }
    1
}

/// WM_ERASEBKGND's DefWindowProc: the class background brush over what
/// the DC may draw. True if there was one.
pub fn erase_background(hwnd: Hwnd, hdc: u64) -> bool {
    let brush = {
        let s = state();
        let Some(w) = s.get(hwnd) else { return false };
        s.classes[w.class].brush
    };
    if brush == 0 {
        return false;
    }
    let mut r = [0i32; 4];
    unsafe { crate::GetClipBox(hdc, &mut r) };
    FillRect(hdc, &r, brush);
    true
}

/// WM_NCPAINT's DefWindowProc for a child window: its border.
pub fn paint_frame(hwnd: Hwnd) {
    let Some((w, h, style, ex)) = state().get(hwnd).filter(|w| !w.top_level()).map(|w| (w.w, w.h, w.style, w.ex_style)) else {
        return;
    };
    let hdc = make_dc(hwnd, None, true);
    let mut inset = 0;
    if ex & crate::window::WS_EX_CLIENTEDGE != 0 {
        frame(hdc, [0, 0, w, h], GetSysColorBrush(16));
        frame(hdc, [1, 1, w - 1, h - 1], GetSysColorBrush(21));
        inset = 2;
    }
    if style & crate::window::WS_BORDER != 0 {
        frame(hdc, [inset, inset, w - inset, h - inset], GetSysColorBrush(6));
    }
    release(hdc);
}

// -----| filling |-----

/// A brush argument: a brush, or a system color index + 1.
fn brush_of(b: u64) -> u64 {
    if b != 0 && b <= 31 { GetSysColorBrush(b as i32 - 1) } else { b }
}

#[no_mangle]
pub extern "system" fn FillRect(hdc: u64, r: *const [i32; 4], brush: u64) -> i32 {
    let Some(r) = rect_arg(r) else { return 0 };
    let old = unsafe { crate::SelectObject(hdc, brush_of(brush)) };
    unsafe { crate::PatBlt(hdc, r[0], r[1], r[2] - r[0], r[3] - r[1], 0x00F0_0021) };
    unsafe { crate::SelectObject(hdc, old) };
    1
}

fn frame(hdc: u64, r: [i32; 4], brush: u64) {
    let old = unsafe { crate::SelectObject(hdc, brush) };
    let (w, h) = (r[2] - r[0], r[3] - r[1]);
    if w > 0 && h > 0 {
        unsafe {
            crate::PatBlt(hdc, r[0], r[1], w, 1, 0x00F0_0021);
            crate::PatBlt(hdc, r[0], r[3] - 1, w, 1, 0x00F0_0021);
            crate::PatBlt(hdc, r[0], r[1], 1, h, 0x00F0_0021);
            crate::PatBlt(hdc, r[2] - 1, r[1], 1, h, 0x00F0_0021);
        }
    }
    unsafe { crate::SelectObject(hdc, old) };
}

#[no_mangle]
pub extern "system" fn FrameRect(hdc: u64, r: *const [i32; 4], brush: u64) -> i32 {
    let Some(r) = rect_arg(r) else { return 0 };
    frame(hdc, r, brush_of(brush));
    1
}

#[no_mangle]
pub extern "system" fn InvertRect(hdc: u64, r: *const [i32; 4]) -> Bool {
    let Some(r) = rect_arg(r) else { return 0 };
    unsafe { crate::PatBlt(hdc, r[0], r[1], r[2] - r[0], r[3] - r[1], 0x0055_0009) }
}

// -----| DrawText |-----

const DT_CENTER: u32 = 0x1;
const DT_RIGHT: u32 = 0x2;
const DT_VCENTER: u32 = 0x4;
const DT_BOTTOM: u32 = 0x8;
const DT_SINGLELINE: u32 = 0x20;
const DT_CALCRECT: u32 = 0x400;
const DT_NOPREFIX: u32 = 0x800;

/// DrawText's lines, alignment, prefixes and DT_CALCRECT; word wrapping
/// and ellipses come with G2.
fn draw_text(hdc: u64, text: &[u16], r: *mut [i32; 4], flags: u32) -> i32 {
    let Some(rect) = rect_arg(r) else { return 0 };
    let cleaned: Vec<u16> = if flags & DT_NOPREFIX != 0 {
        text.to_vec()
    } else {
        // "&x" shows x ("&&" shows "&").
        let mut v = Vec::new();
        let mut i = 0;
        while i < text.len() {
            if text[i] == b'&' as u16 && i + 1 < text.len() {
                i += 1;
            }
            v.push(text[i]);
            i += 1;
        }
        v
    };
    let lines: Vec<&[u16]> = if flags & DT_SINGLELINE != 0 {
        alloc::vec![&cleaned[..]]
    } else {
        cleaned.split(|&c| c == b'\n' as u16).map(|l| l.strip_suffix(&[b'\r' as u16]).unwrap_or(l)).collect()
    };
    let mut size = [0i32; 2];
    unsafe { crate::GetTextExtentPoint32W(hdc, [b'X' as u16].as_ptr(), 1, &mut size) };
    let line_h = size[1];
    let total_h = line_h * lines.len() as i32;
    let widths: Vec<i32> = lines
        .iter()
        .map(|l| {
            let mut sz = [0i32; 2];
            unsafe { crate::GetTextExtentPoint32W(hdc, l.as_ptr(), l.len() as i32, &mut sz) };
            sz[0]
        })
        .collect();
    if flags & DT_CALCRECT != 0 {
        let w = widths.iter().copied().max().unwrap_or(0);
        unsafe {
            (*r)[2] = rect[0] + w;
            (*r)[3] = rect[1] + total_h;
        }
        return total_h;
    }
    let mut y = rect[1];
    if flags & DT_SINGLELINE != 0 {
        if flags & DT_VCENTER != 0 {
            y = rect[1] + (rect[3] - rect[1] - line_h) / 2;
        } else if flags & DT_BOTTOM != 0 {
            y = rect[3] - line_h;
        }
    }
    let old_align = unsafe { crate::SetTextAlign(hdc, 0) };
    for (line, w) in lines.iter().zip(&widths) {
        let x = if flags & DT_CENTER != 0 {
            rect[0] + (rect[2] - rect[0] - w) / 2
        } else if flags & DT_RIGHT != 0 {
            rect[2] - w
        } else {
            rect[0]
        };
        unsafe { crate::TextOutW(hdc, x, y, line.as_ptr(), line.len() as i32) };
        y += line_h;
    }
    unsafe { crate::SetTextAlign(hdc, old_align) };
    total_h
}

#[no_mangle]
pub unsafe extern "system" fn DrawTextW(hdc: u64, s: *const u16, n: i32, r: *mut [i32; 4], flags: u32) -> i32 {
    let text = if n < 0 { unsafe { crate::wstr(s) } } else { unsafe { core::slice::from_raw_parts(s, n as usize) }.to_vec() };
    draw_text(hdc, &text, r, flags)
}

#[no_mangle]
pub unsafe extern "system" fn DrawTextA(hdc: u64, s: *const u8, n: i32, r: *mut [i32; 4], flags: u32) -> i32 {
    draw_text(hdc, &unsafe { crate::from_ansi(s, n) }, r, flags)
}

#[no_mangle]
pub unsafe extern "system" fn DrawTextExW(hdc: u64, s: *const u16, n: i32, r: *mut [i32; 4], flags: u32, _p: usize) -> i32 {
    unsafe { DrawTextW(hdc, s, n, r, flags) }
}

#[no_mangle]
pub unsafe extern "system" fn DrawTextExA(hdc: u64, s: *const u8, n: i32, r: *mut [i32; 4], flags: u32, _p: usize) -> i32 {
    unsafe { DrawTextA(hdc, s, n, r, flags) }
}

// -----| system colors and metrics |-----

/// Windows 10's default colors (COLORREF), COLOR_SCROLLBAR to COLOR_MENUBAR.
const SYS_COLORS: [u32; 31] = [
    0xC8C8C8, 0x000000, 0xD1B499, 0xDBCDBF, 0xF0F0F0, 0xFFFFFF, 0x646464, 0x000000, 0x000000, 0x000000,
    0xB4B4B4, 0xFCF7F4, 0xABABAB, 0xD77800, 0xFFFFFF, 0xF0F0F0, 0xA0A0A0, 0x6D6D6D, 0x000000, 0x000000,
    0xFFFFFF, 0x696969, 0xE3E3E3, 0x000000, 0xE1FFFF, 0x000000, 0xCC6600, 0xEAD1B9, 0xF2E4D7, 0xFF9933,
    0xF0F0F0,
];

static SYS_BRUSHES: crate::dllrt::Lock<[u64; 31]> = crate::dllrt::Lock::new([0; 31]);

#[no_mangle]
pub extern "system" fn GetSysColor(index: i32) -> u32 {
    SYS_COLORS.get(index as usize).copied().unwrap_or(0)
}

#[no_mangle]
pub extern "system" fn GetSysColorBrush(index: i32) -> u64 {
    let Some(&color) = SYS_COLORS.get(index as usize) else { return 0 };
    let mut b = SYS_BRUSHES.lock();
    if b[index as usize] == 0 {
        b[index as usize] = unsafe { crate::CreateSolidBrush(color) };
    }
    b[index as usize]
}

#[no_mangle]
pub extern "system" fn GetSystemMetrics(index: i32) -> i32 {
    let (sw, sh) = crate::screen_size();
    match index {
        0 | 16 | 61 | 78 => sw, // SM_CXSCREEN, CXFULLSCREEN, CXMAXIMIZED, CXVIRTUALSCREEN
        1 | 62 | 79 => sh,
        17 => sh - 23,          // SM_CYFULLSCREEN
        2 | 3 | 9 | 10 | 20 | 21 => 17, // scroll bars and thumbs
        4 => 23,                // SM_CYCAPTION
        5 | 6 => 1,             // SM_CXBORDER, CYBORDER
        7 | 8 => 3,             // SM_CXFIXEDFRAME
        11 | 12 | 13 | 14 => 32, // icons, cursors
        15 => 20,               // SM_CYMENU
        19 => 1,                // SM_MOUSEPRESENT
        28 | 34 => 136,         // SM_CXMIN, CXMINTRACK
        29 | 35 => 39,          // SM_CYMIN, CYMINTRACK
        30 => 36,               // SM_CXSIZE
        31 => 22,               // SM_CYSIZE
        32 | 33 => 4,           // SM_CXFRAME
        36 | 37 => 4,           // double-click area
        38 | 39 => 75,          // icon spacing
        43 => 3,                // SM_CMOUSEBUTTONS
        45 | 46 => 2,           // SM_CXEDGE
        49 | 50 => 16,          // small icons
        51 => 23,               // SM_CYSMCAPTION
        59 => sw + 12,          // SM_CXMAXTRACK
        60 => sh + 12,
        80 => 1,                // SM_CMONITORS
        81 => 1,                // SM_SAMEDISPLAYFORMAT
        92 => 4,                // SM_CXPADDEDBORDER
        _ => 0,
    }
}

#[no_mangle]
pub extern "system" fn GetSystemMetricsForDpi(index: i32, _dpi: u32) -> i32 {
    GetSystemMetrics(index)
}

#[no_mangle]
pub extern "system" fn SystemParametersInfoW(action: u32, _p: u32, out: *mut u8, _ini: u32) -> Bool {
    match action {
        0x30 => {
            // SPI_GETWORKAREA: below the desktop's panel.
            let (w, h) = crate::screen_size();
            unsafe { *(out as *mut [i32; 4]) = [0, 0, w, h] };
            1
        }
        _ => 0,
    }
}

#[no_mangle]
pub extern "system" fn SystemParametersInfoA(action: u32, p: u32, out: *mut u8, ini: u32) -> Bool {
    SystemParametersInfoW(action, p, out, ini)
}

#[no_mangle]
pub extern "system" fn GetDpiForWindow(_hwnd: Hwnd) -> u32 {
    96
}

#[no_mangle]
pub extern "system" fn GetDpiForSystem() -> u32 {
    96
}

#[no_mangle]
pub extern "system" fn SetProcessDPIAware() -> Bool {
    1
}

#[no_mangle]
pub extern "system" fn IsProcessDPIAware() -> Bool {
    1
}

#[no_mangle]
pub extern "system" fn SetProcessDpiAwarenessContext(_ctx: i64) -> Bool {
    1
}

#[no_mangle]
pub extern "system" fn SetThreadDpiAwarenessContext(_ctx: i64) -> i64 {
    -1
}

// -----| cursors, icons, message boxes (resources and dialogs: G2, G3) |-----

/// Stand-in handles: the standard cursor or icon asked for.
#[no_mangle]
pub extern "system" fn LoadCursorW(_instance: u64, name: u64) -> u64 {
    0x0001_0000 | (name & 0xFFFF)
}

#[no_mangle]
pub extern "system" fn LoadCursorA(instance: u64, name: u64) -> u64 {
    LoadCursorW(instance, name)
}

#[no_mangle]
pub extern "system" fn LoadIconW(_instance: u64, name: u64) -> u64 {
    0x0002_0000 | (name & 0xFFFF)
}

#[no_mangle]
pub extern "system" fn LoadIconA(instance: u64, name: u64) -> u64 {
    LoadIconW(instance, name)
}

#[no_mangle]
pub extern "system" fn LoadImageW(_instance: u64, name: u64, kind: u32, _w: i32, _h: i32, _flags: u32) -> u64 {
    0x0003_0000 | (kind as u64) << 12 | (name & 0xFFF)
}

#[no_mangle]
pub extern "system" fn LoadImageA(instance: u64, name: u64, kind: u32, w: i32, h: i32, flags: u32) -> u64 {
    LoadImageW(instance, name, kind, w, h, flags)
}

#[no_mangle]
pub extern "system" fn DestroyIcon(_h: u64) -> Bool {
    1
}

#[no_mangle]
pub extern "system" fn DestroyCursor(_h: u64) -> Bool {
    1
}

static CURSOR: crate::dllrt::Lock<(u64, i32)> = crate::dllrt::Lock::new((0x0001_7F00, 0));

#[no_mangle]
pub extern "system" fn SetCursor(c: u64) -> u64 {
    core::mem::replace(&mut CURSOR.lock().0, c)
}

#[no_mangle]
pub extern "system" fn GetCursor() -> u64 {
    CURSOR.lock().0
}

#[no_mangle]
pub extern "system" fn ShowCursor(show: Bool) -> i32 {
    let mut c = CURSOR.lock();
    c.1 += if show != 0 { 1 } else { -1 };
    c.1
}

#[no_mangle]
pub extern "system" fn GetCursorPos(out: *mut [i32; 2]) -> Bool {
    if out.is_null() {
        return 0;
    }
    unsafe { (crate::VeilUserQuery(1, out) == 0) as Bool }
}

#[no_mangle]
pub extern "system" fn SetCursorPos(_x: i32, _y: i32) -> Bool {
    1
}

#[no_mangle]
pub extern "system" fn ClipCursor(_r: *const [i32; 4]) -> Bool {
    1
}

/// Until dialogs (G3): the text goes to the debug log and the answer is
/// the first button's (IDOK, or IDYES / IDABORT / IDRETRY for those sets).
fn message_box(text: &[u16], caption: &[u16], kind: u32) -> i32 {
    let mut line = alloc::string::String::from("MessageBox: ");
    line.extend(char::decode_utf16(caption.iter().copied()).map(|c| c.unwrap_or('?')));
    line.push_str(": ");
    line.extend(char::decode_utf16(text.iter().copied()).map(|c| c.unwrap_or('?')));
    crate::debug(&line);
    match kind & 0xF {
        2 => 3, // MB_ABORTRETRYIGNORE: IDABORT
        3 | 4 => 6, // MB_YESNO(CANCEL): IDYES
        5 => 4, // MB_RETRYCANCEL: IDRETRY
        _ => 1,
    }
}

#[no_mangle]
pub unsafe extern "system" fn MessageBoxW(_hwnd: Hwnd, text: *const u16, caption: *const u16, kind: u32) -> i32 {
    message_box(&unsafe { crate::wstr(text) }, &unsafe { crate::wstr(caption) }, kind)
}

#[no_mangle]
pub unsafe extern "system" fn MessageBoxA(_hwnd: Hwnd, text: *const u8, caption: *const u8, kind: u32) -> i32 {
    message_box(&unsafe { crate::from_ansi(text, -1) }, &unsafe { crate::from_ansi(caption, -1) }, kind)
}

#[no_mangle]
pub extern "system" fn MessageBeep(_kind: u32) -> Bool {
    1
}

// -----| rectangles |-----

type Rect = [i32; 4];

#[no_mangle]
pub extern "system" fn SetRect(r: *mut Rect, l: i32, t: i32, rr: i32, b: i32) -> Bool {
    if r.is_null() {
        return 0;
    }
    unsafe { *r = [l, t, rr, b] };
    1
}

#[no_mangle]
pub extern "system" fn SetRectEmpty(r: *mut Rect) -> Bool {
    SetRect(r, 0, 0, 0, 0)
}

#[no_mangle]
pub extern "system" fn CopyRect(d: *mut Rect, s: *const Rect) -> Bool {
    if d.is_null() || s.is_null() {
        return 0;
    }
    unsafe { *d = *s };
    1
}

#[no_mangle]
pub extern "system" fn OffsetRect(r: *mut Rect, dx: i32, dy: i32) -> Bool {
    if r.is_null() {
        return 0;
    }
    unsafe {
        (*r)[0] += dx;
        (*r)[1] += dy;
        (*r)[2] += dx;
        (*r)[3] += dy;
    }
    1
}

#[no_mangle]
pub extern "system" fn InflateRect(r: *mut Rect, dx: i32, dy: i32) -> Bool {
    if r.is_null() {
        return 0;
    }
    unsafe {
        (*r)[0] -= dx;
        (*r)[1] -= dy;
        (*r)[2] += dx;
        (*r)[3] += dy;
    }
    1
}

#[no_mangle]
pub extern "system" fn IsRectEmpty(r: *const Rect) -> Bool {
    rect_arg(r).is_none_or(empty) as Bool
}

#[no_mangle]
pub extern "system" fn EqualRect(a: *const Rect, b: *const Rect) -> Bool {
    (!a.is_null() && !b.is_null() && unsafe { *a == *b }) as Bool
}

#[no_mangle]
pub extern "system" fn PtInRect(r: *const Rect, x: i32, y: i32) -> Bool {
    rect_arg(r).is_some_and(|r| x >= r[0] && x < r[2] && y >= r[1] && y < r[3]) as Bool
}

#[no_mangle]
pub extern "system" fn IntersectRect(d: *mut Rect, a: *const Rect, b: *const Rect) -> Bool {
    let (Some(a), Some(b)) = (rect_arg(a), rect_arg(b)) else { return 0 };
    let r = intersect(a, b);
    let r = if empty(r) { [0; 4] } else { r };
    SetRect(d, r[0], r[1], r[2], r[3]);
    (!empty(r)) as Bool
}

#[no_mangle]
pub extern "system" fn UnionRect(d: *mut Rect, a: *const Rect, b: *const Rect) -> Bool {
    let (Some(a), Some(b)) = (rect_arg(a), rect_arg(b)) else { return 0 };
    let r = match (empty(a), empty(b)) {
        (true, true) => [0; 4],
        (true, false) => b,
        (false, true) => a,
        _ => union(Some(a), b),
    };
    SetRect(d, r[0], r[1], r[2], r[3]);
    (!empty(r)) as Bool
}
