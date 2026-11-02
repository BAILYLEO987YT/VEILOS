//! Windows: creating and destroying them, showing, moving, sizing and
//! activating them (with the messages Windows sends for each, in its
//! order), their data, text and family.

use alloc::vec::Vec;
use nt_abi::{WindowParams, WindowPlace};

use crate::message::*;
use crate::{set_error, state, Bool, Hwnd, Name, State, Wnd};

pub const WS_POPUP: u32 = 0x8000_0000;
pub const WS_CHILD: u32 = 0x4000_0000;
pub const WS_MINIMIZE: u32 = 0x2000_0000;
pub const WS_VISIBLE: u32 = 0x1000_0000;
pub const WS_DISABLED: u32 = 0x0800_0000;
pub const WS_CLIPSIBLINGS: u32 = 0x0400_0000;
pub const WS_MAXIMIZE: u32 = 0x0100_0000;
pub const WS_CAPTION: u32 = 0x00C0_0000;
pub const WS_BORDER: u32 = 0x0080_0000;
pub const WS_DLGFRAME: u32 = 0x0040_0000;
pub const WS_THICKFRAME: u32 = 0x0004_0000;
pub const WS_EX_DLGMODALFRAME: u32 = 0x0000_0001;
pub const WS_EX_NOPARENTNOTIFY: u32 = 0x0000_0004;
pub const WS_EX_CLIENTEDGE: u32 = 0x0000_0200;
pub const WS_EX_STATICEDGE: u32 = 0x0002_0000;

pub const SWP_NOSIZE: u32 = 0x0001;
pub const SWP_NOMOVE: u32 = 0x0002;
pub const SWP_NOZORDER: u32 = 0x0004;
pub const SWP_NOREDRAW: u32 = 0x0008;
pub const SWP_NOACTIVATE: u32 = 0x0010;
pub const SWP_FRAMECHANGED: u32 = 0x0020;
pub const SWP_SHOWWINDOW: u32 = 0x0040;
pub const SWP_HIDEWINDOW: u32 = 0x0080;
pub const SWP_NOSENDCHANGING: u32 = 0x0400;
const SWP_NOCLIENTSIZE: u32 = 0x0800;
const SWP_NOCLIENTMOVE: u32 = 0x1000;

const CW_USEDEFAULT: i32 = 0x8000_0000u32 as i32;
const HWND_MESSAGE: Hwnd = -3i64 as u64;
/// GetDesktopWindow's answer: no window of ours has it.
const DESKTOP: Hwnd = 0x0001_0000;

/// The desktop's frame around a kernel window's client area: border and
/// title bar (gui_toolkit's titlebar_height).
const DESK_BORDER: i32 = 1;
const DESK_TITLE: i32 = 32;

/// Windows 10's non-client margins (left, top, right, bottom) for a style.
pub fn frame_margins(style: u32, ex: u32, top_level: bool) -> [i32; 4] {
    let mut b = if style & WS_THICKFRAME != 0 {
        if top_level { 8 } else { 4 }
    } else if style & WS_DLGFRAME != 0 || ex & WS_EX_DLGMODALFRAME != 0 {
        3
    } else if style & WS_BORDER != 0 {
        1
    } else {
        0
    };
    if ex & WS_EX_CLIENTEDGE != 0 {
        b += 2;
    }
    if ex & WS_EX_STATICEDGE != 0 {
        b += 1;
    }
    let caption = if style & WS_CAPTION == WS_CAPTION { 23 } else { 0 };
    [b, b + caption, b, b]
}

pub const WS_VSCROLL: u32 = 0x0020_0000;
pub const WS_HSCROLL: u32 = 0x0010_0000;
/// SM_CXVSCROLL / SM_CYHSCROLL.
pub const SCROLL_SIZE: i32 = 17;

/// The default non-client margins (WM_NCCALCSIZE's): the frame, the menu
/// bar and the standard scroll bars.
pub fn nc_margins(style: u32, ex: u32, top_level: bool, menu: bool) -> [i32; 4] {
    let mut m = frame_margins(style, ex, top_level);
    if menu && top_level {
        m[1] += crate::menu::BAR_H;
    }
    if style & WS_VSCROLL != 0 {
        m[2] += SCROLL_SIZE;
    }
    if style & WS_HSCROLL != 0 {
        m[3] += SCROLL_SIZE;
    }
    m
}

fn adjust(r: *mut [i32; 4], style: u32, menu: Bool, ex: u32) -> Bool {
    if r.is_null() {
        return 0;
    }
    let m = frame_margins(style, ex, style & WS_CHILD == 0);
    unsafe {
        (*r)[0] -= m[0];
        (*r)[1] -= m[1] + if menu != 0 { 20 } else { 0 };
        (*r)[2] += m[2];
        (*r)[3] += m[3];
    }
    1
}

#[no_mangle]
pub extern "system" fn AdjustWindowRect(r: *mut [i32; 4], style: u32, menu: Bool) -> Bool {
    adjust(r, style, menu, 0)
}

#[no_mangle]
pub extern "system" fn AdjustWindowRectEx(r: *mut [i32; 4], style: u32, menu: Bool, ex: u32) -> Bool {
    adjust(r, style, menu, ex)
}

#[no_mangle]
pub extern "system" fn AdjustWindowRectExForDpi(r: *mut [i32; 4], style: u32, menu: Bool, ex: u32, _dpi: u32) -> Bool {
    adjust(r, style, menu, ex)
}

// -----| creating |-----

static CASCADE: crate::dllrt::Lock<i32> = crate::dllrt::Lock::new(0);

/// A kernel window's pixels' rectangle on screen, as the kernel has it.
fn kernel_place(hwnd: Hwnd) -> Option<WindowPlace> {
    let mut p = WindowPlace::default();
    (unsafe { crate::VeilUserGetPos(hwnd, &mut p) } == 0).then_some(p)
}

/// Move/size kernel window `hwnd` to match its window rectangle (`flags`:
/// 1 move, 2 size).
fn kernel_sync(hwnd: Hwnd, flags: u32) {
    let Some((x, y, popup, f, (sw, sh))) = state().get(hwnd).filter(|w| w.kernel).map(|w| (w.x, w.y, w.popup, w.frame(), w.surface_size())) else {
        return;
    };
    // VEIL_USER_SET_POS takes the desktop frame's corner.
    let (dx, dy) = if popup { (0, 0) } else { (DESK_BORDER, DESK_TITLE) };
    let p = WindowPlace { x: x + f[0] - dx, y: y + f[1] - dy, w: sw as u32, h: sh as u32 };
    unsafe { crate::VeilUserSetPos(hwnd, &p, flags) };
}

#[allow(clippy::too_many_arguments)]
unsafe fn create(ex: u32, class: Name, name: u64, style: u32, x: i32, y: i32, w: i32, h: i32,
                 parent: Hwnd, menu: u64, instance: u64, param: u64, class_ptr: u64, unicode: bool) -> Hwnd {
    let s = state();
    let Some(ci) = crate::find_class(&s, &class) else {
        drop(s);
        set_error(crate::ERROR_CANNOT_FIND_WND_CLASS);
        return 0;
    };
    let want_visible = style & WS_VISIBLE != 0;
    let mut style = style & !WS_VISIBLE;
    let child = style & WS_CHILD != 0;
    if child && s.get(parent).is_none() {
        drop(s);
        set_error(1406); // ERROR_TLW_WITH_WSCHILD
        return 0;
    }
    if !child && style & WS_POPUP == 0 {
        // Overlapped windows always have a caption.
        style |= WS_CAPTION;
    }
    if !child {
        style |= WS_CLIPSIBLINGS;
    }
    let message_only = parent == HWND_MESSAGE;
    let (parent, owner) = if child { (parent, 0) } else { (0, if message_only { 0 } else { s.root(parent) }) };
    // A top-level window's menu: given, or its class's (loading it takes
    // only the menu table's lock).
    let menu_bar = if child {
        0
    } else if menu != 0 {
        menu
    } else {
        let c = &s.classes[ci];
        if c.menu_name != 0 { crate::menu::LoadMenuW(c.instance, c.menu_name as *const u16) } else { 0 }
    };
    let margins = nc_margins(style, ex, !child, menu_bar != 0);
    let (sw, sh) = crate::screen_size();
    let (mut x, mut y, mut w, mut h) = (x, y, w, h);
    if child {
        if x == CW_USEDEFAULT {
            x = 0;
            y = 0;
        }
        if w == CW_USEDEFAULT {
            w = 0;
            h = 0;
        }
    } else {
        if x == CW_USEDEFAULT {
            let mut n = CASCADE.lock();
            x = 40 + 26 * *n;
            y = 60 + 26 * *n;
            *n = (*n + 1) % 8;
        }
        if w == CW_USEDEFAULT {
            w = sw * 3 / 4;
            h = sh * 3 / 4;
        }
    }
    let class_rec = &s.classes[ci];
    let (proc, wnd_unicode, wnd_extra) = (class_rec.proc, class_rec.unicode, class_rec.wnd_extra);
    let tid = unsafe { crate::GetCurrentThreadId() };
    drop(s);

    // The kernel window (hidden until shown) gives a top-level window its
    // handle; a child's is ours.
    let kernel = !child && !message_only;
    // A popup without a caption has no desktop frame.
    let popup = kernel && style & WS_POPUP != 0 && style & WS_CAPTION != WS_CAPTION;
    let hwnd = if kernel {
        let title = Vec::from([0u16]);
        let f = if popup { [0; 4] } else { frame_margins(style, ex, true) };
        let (dx, dy) = if popup { (0, 0) } else { (DESK_BORDER, DESK_TITLE) };
        let params = WindowParams {
            title: title.as_ptr() as u64,
            x: x + f[0] - dx,
            y: y + f[1] - dy,
            client_w: (w - f[0] - f[2]).max(1) as u32,
            client_h: (h - f[1] - f[3]).max(1) as u32,
            flags: if popup { nt_abi::WINDOW_POPUP } else { 0 } | if owner != 0 { nt_abi::WINDOW_OWNED } else { 0 },
        };
        let mut hwnd = 0u64;
        if unsafe { crate::VeilUserCreateWindow(&params, &mut hwnd) } != 0 {
            set_error(8);
            return 0;
        }
        // Where the desktop put it.
        if let Some(p) = kernel_place(hwnd) {
            x = p.x - f[0];
            y = p.y - f[1];
            w = p.w as i32 + f[0] + f[2];
            h = p.h as i32 + f[1] + f[3];
        }
        hwnd
    } else {
        let mut s = state();
        s.next_child += 4;
        s.next_child
    };
    {
        let mut s = state();
        let id = if child { menu } else { 0 };
        s.windows.push(Wnd {
            hwnd,
            tid,
            parent,
            owner,
            class: ci,
            proc,
            unicode: wnd_unicode,
            style,
            ex_style: ex,
            id,
            instance,
            user_data: 0,
            extra: alloc::vec![0; wnd_extra as usize],
            text: Vec::new(),
            x,
            y,
            w,
            h,
            margins,
            visible: false,
            enabled: style & WS_DISABLED == 0,
            minimized: false,
            maximized: false,
            send_size_move: !child,
            update: None,
            erase: false,
            destroying: false,
            surface: Vec::new(),
            stride: 0,
            dirty: None,
            kernel,
            popup,
            scroll: [crate::ScrollBar { max: 100, ..Default::default() }; 2],
            menu: menu_bar,
        });
        if kernel {
            let w = s.get_mut(hwnd).unwrap();
            crate::paint::ensure_surface(w);
        }
    }

    // Sizable and overlapped windows learn their size limits first.
    if !child && (style & WS_THICKFRAME != 0 || style & WS_POPUP == 0) {
        let (minw, minh, maxw, maxh) = min_max(hwnd, style, ex);
        let (cw, ch) = (w.clamp(minw, maxw), h.clamp(minh, maxh));
        if (cw, ch) != (w, h) {
            w = cw;
            h = ch;
            if let Some(wnd) = state().get_mut(hwnd) {
                wnd.w = w;
                wnd.h = h;
                crate::paint::ensure_surface(wnd);
            }
            kernel_sync(hwnd, 2);
        }
    }

    // CREATESTRUCT: the caller's own name and class pointers.
    let mut cs = [0u8; 80];
    let mut put = |off: usize, v: &[u8]| cs[off..off + v.len()].copy_from_slice(v);
    put(0, &param.to_le_bytes());
    put(8, &instance.to_le_bytes());
    put(16, &menu.to_le_bytes());
    put(24, &(if child { parent } else { owner }).to_le_bytes());
    put(32, &h.to_le_bytes());
    put(36, &w.to_le_bytes());
    put(40, &y.to_le_bytes());
    put(44, &x.to_le_bytes());
    put(48, &(style | if want_visible { WS_VISIBLE } else { 0 }).to_le_bytes());
    put(56, &name.to_le_bytes());
    put(64, &class_ptr.to_le_bytes());
    put(72, &ex.to_le_bytes());
    let csp = cs.as_mut_ptr() as i64;

    if call(hwnd, WM_NCCREATE, 0, csp, unicode) == 0 {
        destroy_now(hwnd);
        return 0;
    }
    // The client area inside the window rectangle.
    let mut r = [x, y, x + w, y + h];
    call(hwnd, WM_NCCALCSIZE, 0, r.as_mut_ptr() as i64, unicode);
    set_margins(hwnd, [r[0] - x, r[1] - y, x + w - r[2], y + h - r[3]]);
    if call(hwnd, WM_CREATE, 0, csp, unicode) == -1 {
        DestroyWindow(hwnd);
        return 0;
    }
    if child {
        let (cw, ch, cx, cy, id) = {
            let s = state();
            let Some(wnd) = s.get(hwnd) else { return 0 };
            (wnd.client_w(), wnd.client_h(), wnd.x, wnd.y, wnd.id)
        };
        send(hwnd, WM_SIZE, 0, ((ch as u16 as i64) << 16) | cw as u16 as i64);
        send(hwnd, WM_MOVE, 0, ((cy as u16 as i64) << 16) | cx as u16 as i64);
        if ex & WS_EX_NOPARENTNOTIFY == 0 {
            send(parent, WM_PARENTNOTIFY, (id << 16) | WM_CREATE as u64, hwnd as i64);
        }
    }
    if state().get(hwnd).is_none() {
        return 0;
    }
    if want_visible {
        ShowWindow(hwnd, 5);
    }
    hwnd
}

/// A window's client area moved inside its rectangle (WM_NCCALCSIZE).
fn set_margins(hwnd: Hwnd, m: [i32; 4]) {
    let mut s = state();
    let Some(w) = s.get_mut(hwnd) else { return };
    if w.margins == m {
        return;
    }
    w.margins = m;
    if w.kernel {
        crate::paint::ensure_surface(w);
        drop(s);
        kernel_sync(hwnd, 3);
    }
}

/// WM_GETMINMAXINFO's defaults, as the program changes them: minimum and
/// maximum tracking size.
fn min_max(hwnd: Hwnd, style: u32, ex: u32) -> (i32, i32, i32, i32) {
    let (sw, sh) = crate::screen_size();
    let m = frame_margins(style, ex, true);
    // MINMAXINFO: reserved, max size, max position, min track, max track.
    let mut mmi: [i32; 10] = [0, 0, sw + m[0] + m[2], sh + m[1] + m[3], -m[0], -m[1], 136, 39, sw + m[0] + m[2] + 12, sh + m[1] + m[3] + 12];
    if hwnd != 0 {
        send(hwnd, WM_GETMINMAXINFO, 0, mmi.as_mut_ptr() as i64);
    }
    (mmi[6].max(0), mmi[7].max(0), mmi[8].max(mmi[6]), mmi[9].max(mmi[7]))
}

#[no_mangle]
pub unsafe extern "system" fn CreateWindowExW(ex: u32, class: *const u16, name: *const u16, style: u32, x: i32, y: i32, w: i32, h: i32,
                                              parent: Hwnd, menu: u64, instance: u64, param: u64) -> Hwnd {
    unsafe { create(ex, crate::name_w(class), name as u64, style, x, y, w, h, parent, menu, instance, param, class as u64, true) }
}

#[no_mangle]
pub unsafe extern "system" fn CreateWindowExA(ex: u32, class: *const u8, name: *const u8, style: u32, x: i32, y: i32, w: i32, h: i32,
                                              parent: Hwnd, menu: u64, instance: u64, param: u64) -> Hwnd {
    unsafe { create(ex, crate::name_a(class), name as u64, style, x, y, w, h, parent, menu, instance, param, class as u64, false) }
}

// -----| destroying |-----

/// `hwnd` and its descendants, parents first.
fn family(s: &State, hwnd: Hwnd) -> Vec<Hwnd> {
    let mut out = alloc::vec![hwnd];
    let mut i = 0;
    while i < out.len() {
        let h = out[i];
        out.extend(s.windows.iter().filter(|w| w.parent == h).map(|w| w.hwnd));
        i += 1;
    }
    out
}

/// Take windows out of the table (and the kernel's), with their timers.
fn destroy_now(hwnd: Hwnd) {
    let mut s = state();
    let all = family(&s, hwnd);
    let mut timers: Vec<(Hwnd, u64)> = Vec::new();
    for h in &all {
        timers.extend(s.timer_procs.iter().filter(|t| t.0 == *h).map(|t| (t.0, t.1)));
    }
    s.timer_procs.retain(|t| !all.contains(&t.0));
    let kernel: Vec<Hwnd> = all.iter().copied().filter(|h| s.get(*h).is_some_and(|w| w.kernel)).collect();
    let menus: Vec<(Hwnd, u64)> = all.iter().filter_map(|h| s.get(*h).map(|w| (w.hwnd, if w.parent == 0 { w.menu } else { 0 }))).collect();
    s.windows.retain(|w| !all.contains(&w.hwnd));
    let st = &mut *s;
    for f in [&mut st.focus, &mut st.active, &mut st.capture] {
        if all.contains(f) {
            *f = 0;
        }
    }
    drop(s);
    for (h, id) in timers {
        kill_timers(h, &[id]);
    }
    for h in kernel {
        unsafe { crate::VeilUserDestroyWindow(h) };
    }
    // The menu bar and system menu go with the window.
    for (h, menu) in menus {
        crate::menu::window_destroyed(h);
        crate::clipboard::window_destroyed(h);
        if menu != 0 {
            crate::menu::DestroyMenu(menu);
        }
    }
}

#[no_mangle]
pub extern "system" fn DestroyWindow(hwnd: Hwnd) -> Bool {
    let (visible, child, parent, id, ex, already) = match state().get(hwnd) {
        Some(w) => (w.visible, !w.top_level(), w.parent, w.id, w.ex_style, w.destroying),
        None => {
            set_error(crate::ERROR_INVALID_WINDOW_HANDLE);
            return 0;
        }
    };
    if already {
        return 1;
    }
    if visible {
        set_window_pos(hwnd, 0, 0, 0, 0, 0, SWP_HIDEWINDOW | SWP_NOACTIVATE | SWP_NOZORDER | SWP_NOMOVE | SWP_NOSIZE);
    }
    if child && ex & WS_EX_NOPARENTNOTIFY == 0 && state().get(parent).is_some_and(|p| !p.destroying) {
        send(parent, WM_PARENTNOTIFY, (id << 16) | WM_DESTROY as u64, hwnd as i64);
    }
    // Owned windows go first.
    let owned: Vec<Hwnd> = state().windows.iter().filter(|w| w.owner == hwnd).map(|w| w.hwnd).collect();
    for o in owned {
        DestroyWindow(o);
    }
    let all = {
        let mut s = state();
        let all = family(&s, hwnd);
        for h in &all {
            if let Some(w) = s.get_mut(*h) {
                w.destroying = true;
            }
        }
        all
    };
    for h in &all {
        send(*h, WM_DESTROY, 0, 0);
    }
    for h in all.iter().rev() {
        send(*h, WM_NCDESTROY, 0, 0);
    }
    destroy_now(hwnd);
    1
}

// -----| showing, moving, sizing, activating |-----

/// Windows' SetWindowPos with its messages: WM_WINDOWPOSCHANGING, size
/// limits and WM_NCCALCSIZE for a new size, activation, erasing what's
/// now visible, WM_WINDOWPOSCHANGED (whose DefWindowProc sends WM_SIZE
/// and WM_MOVE).
pub fn set_window_pos(hwnd: Hwnd, after: Hwnd, x: i32, y: i32, cx: i32, cy: i32, flags: u32) -> Bool {
    let Some((style, ex, top, kernel, was_visible)) = state().get(hwnd).map(|w| (w.style, w.ex_style, w.top_level(), w.kernel, w.visible)) else {
        set_error(crate::ERROR_INVALID_WINDOW_HANDLE);
        return 0;
    };
    // WINDOWPOS: hwnd, insert after, x, y, cx, cy, flags.
    #[repr(C)]
    struct WindowPos {
        hwnd: Hwnd,
        after: Hwnd,
        x: i32,
        y: i32,
        cx: i32,
        cy: i32,
        flags: u32,
    }
    let mut wp = WindowPos { hwnd, after, x, y, cx, cy, flags };
    if flags & SWP_NOSENDCHANGING == 0 {
        send(hwnd, WM_WINDOWPOSCHANGING, 0, &mut wp as *mut WindowPos as i64);
    }
    let flags = wp.flags;
    let (old_x, old_y, old_w, old_h, old_margins) = {
        let s = state();
        let Some(w) = s.get(hwnd) else { return 0 };
        (w.x, w.y, w.w, w.h, w.margins)
    };
    let (mut nx, mut ny, mut nw, mut nh) = (old_x, old_y, old_w, old_h);
    if flags & SWP_NOMOVE == 0 {
        nx = wp.x;
        ny = wp.y;
    }
    let mut margins = old_margins;
    if flags & SWP_NOSIZE == 0 || flags & SWP_FRAMECHANGED != 0 {
        if flags & SWP_NOSIZE == 0 {
            nw = wp.cx.max(0);
            nh = wp.cy.max(0);
            if top && (style & WS_THICKFRAME != 0 || style & WS_POPUP == 0) {
                let (minw, minh, maxw, maxh) = min_max(hwnd, style, ex);
                nw = nw.clamp(minw, maxw);
                nh = nh.clamp(minh, maxh);
            }
        }
        // NCCALCSIZE_PARAMS: the new window rectangle becomes the client's.
        let mut params = [0i32; 14];
        params[..4].copy_from_slice(&[nx, ny, nx + nw, ny + nh]);
        params[4..8].copy_from_slice(&[old_x, old_y, old_x + old_w, old_y + old_h]);
        params[8..12].copy_from_slice(&[old_x + old_margins[0], old_y + old_margins[1], old_x + old_w - old_margins[2], old_y + old_h - old_margins[3]]);
        send(hwnd, WM_NCCALCSIZE, 1, params.as_mut_ptr() as i64);
        margins = [params[0] - nx, params[1] - ny, nx + nw - params[2], ny + nh - params[3]];
    }
    let show = flags & SWP_SHOWWINDOW != 0 && !was_visible;
    let hide = flags & SWP_HIDEWINDOW != 0 && was_visible;
    // Did the client area move or change size? (The first time a window
    // is shown, WM_SIZE and WM_MOVE are owed either way.)
    let mut out_flags = flags;
    let (old_cw, old_ch) = (old_w - old_margins[0] - old_margins[2], old_h - old_margins[1] - old_margins[3]);
    let (new_cw, new_ch) = (nw - margins[0] - margins[2], nh - margins[1] - margins[3]);
    let owed = state().get(hwnd).is_some_and(|w| w.send_size_move) && (show || was_visible);
    if (old_cw, old_ch) == (new_cw, new_ch) && !owed {
        out_flags |= SWP_NOCLIENTSIZE;
    }
    if (old_x + old_margins[0], old_y + old_margins[1]) == (nx + margins[0], ny + margins[1]) && !owed {
        out_flags |= SWP_NOCLIENTMOVE;
    }
    let (parent, old_rect) = {
        let mut s = state();
        let Some(w) = s.get_mut(hwnd) else { return 0 };
        let old_rect = [w.x, w.y, w.x + w.w, w.y + w.h];
        w.x = nx;
        w.y = ny;
        w.w = nw;
        w.h = nh;
        w.margins = margins;
        if show {
            w.visible = true;
            w.style |= WS_VISIBLE;
        }
        if hide {
            w.visible = false;
            w.style &= !WS_VISIBLE;
        }
        if owed {
            w.send_size_move = false;
        }
        if w.kernel {
            crate::paint::ensure_surface(w);
        }
        (w.parent, old_rect)
    };
    if kernel {
        let moved = out_flags & SWP_NOCLIENTMOVE == 0;
        let sized = out_flags & SWP_NOCLIENTSIZE == 0;
        if moved || sized || margins != old_margins {
            kernel_sync(hwnd, (moved as u32) | ((sized || margins != old_margins) as u32) << 1);
        }
        if show {
            unsafe { crate::VeilUserShowWindow(hwnd, if flags & SWP_NOACTIVATE != 0 { 4 } else { 1 }) };
        }
        if hide {
            unsafe { crate::VeilUserShowWindow(hwnd, 0) };
        }
    }
    if top && flags & SWP_NOACTIVATE == 0 && (show || (was_visible && !hide)) {
        activate(hwnd);
    }
    // A hidden window can't keep the keyboard focus.
    if hide {
        let focus_inside = {
            let s = state();
            let f = s.focus;
            f != 0 && (f == hwnd || family(&s, hwnd).contains(&f))
        };
        if focus_inside {
            SetFocus(0);
        }
        if state().active == hwnd {
            state().active = 0;
        }
    }
    // Repaint what changed.
    let changed = show || hide || out_flags & (SWP_NOCLIENTSIZE | SWP_NOCLIENTMOVE) != (SWP_NOCLIENTSIZE | SWP_NOCLIENTMOVE);
    let visible_now = (show || was_visible) && !hide;
    if changed && flags & SWP_NOREDRAW == 0 {
        if visible_now {
            crate::paint::invalidate(hwnd, None, true);
        }
        if top {
            if visible_now {
                crate::paint::erase_now(hwnd);
            }
        } else {
            // The parent shows through where the child was and is.
            crate::paint::invalidate(parent, Some(old_rect), true);
            crate::paint::invalidate(parent, Some([nx, ny, nx + nw, ny + nh]), true);
            crate::paint::erase_now(parent);
        }
    }
    let mut done = WindowPos { hwnd, after: wp.after, x: nx, y: ny, cx: nw, cy: nh, flags: out_flags };
    send(hwnd, WM_WINDOWPOSCHANGED, 0, &mut done as *mut WindowPos as i64);
    1
}

// -----| deferred positions, scrolling |-----

/// BeginDeferWindowPos's handle: the moves to make.
type Deferred = Vec<(Hwnd, Hwnd, i32, i32, i32, i32, u32)>;

#[no_mangle]
pub extern "system" fn BeginDeferWindowPos(_n: i32) -> u64 {
    alloc::boxed::Box::into_raw(alloc::boxed::Box::new(Deferred::new())) as u64
}

#[no_mangle]
pub extern "system" fn DeferWindowPos(h: u64, hwnd: Hwnd, after: Hwnd, x: i32, y: i32, cx: i32, cy: i32, flags: u32) -> u64 {
    if h == 0 {
        return 0;
    }
    unsafe { (*(h as *mut Deferred)).push((hwnd, after, x, y, cx, cy, flags)) };
    h
}

#[no_mangle]
pub extern "system" fn EndDeferWindowPos(h: u64) -> Bool {
    if h == 0 {
        return 0;
    }
    let list = unsafe { alloc::boxed::Box::from_raw(h as *mut Deferred) };
    for (hwnd, after, x, y, cx, cy, flags) in list.iter().copied() {
        SetWindowPos(hwnd, after, x, y, cx, cy, flags);
    }
    1
}

#[no_mangle]
pub extern "system" fn GetLastActivePopup(hwnd: Hwnd) -> Hwnd {
    hwnd
}

/// Left-to-right.
#[no_mangle]
pub unsafe extern "system" fn GetProcessDefaultLayout(out: *mut u32) -> Bool {
    if out.is_null() {
        return 0;
    }
    unsafe { *out = 0 };
    1
}

#[no_mangle]
pub extern "system" fn SetProcessDefaultLayout(_layout: u32) -> Bool {
    1
}

#[no_mangle]
pub extern "system" fn DrawAnimatedRects(_hwnd: Hwnd, _anim: i32, _from: *const [i32; 4], _to: *const [i32; 4]) -> Bool {
    1
}

/// Children move with the scroll if asked; the pixels aren't moved, the
/// whole client area paints again instead (programs paint from their
/// positions either way).
#[no_mangle]
pub unsafe extern "system" fn ScrollWindowEx(hwnd: Hwnd, dx: i32, dy: i32, _scroll: *const [i32; 4], _clip: *const [i32; 4], _rgn: u64,
                                             update: *mut [i32; 4], flags: u32) -> i32 {
    const SW_SCROLLCHILDREN: u32 = 1;
    if flags & SW_SCROLLCHILDREN != 0 {
        scroll_children(hwnd, dx, dy);
    }
    let mut c = [0i32; 4];
    GetClientRect(hwnd, &mut c);
    if !update.is_null() {
        unsafe { *update = c };
    }
    // SW_INVALIDATE, SW_ERASE
    if flags & 6 != 0 {
        crate::paint::invalidate(hwnd, None, flags & 4 != 0);
    }
    2 // SIMPLEREGION
}

#[no_mangle]
pub unsafe extern "system" fn ScrollWindow(hwnd: Hwnd, dx: i32, dy: i32, scroll: *const [i32; 4], _clip: *const [i32; 4]) -> Bool {
    if scroll.is_null() {
        scroll_children(hwnd, dx, dy);
    }
    crate::paint::invalidate(hwnd, None, true);
    1
}

fn scroll_children(hwnd: Hwnd, dx: i32, dy: i32) {
    let mut child = GetWindow(hwnd, 5); // GW_CHILD
    while child != 0 {
        let mut r = [0i32; 4];
        GetWindowRect(child, &mut r);
        let mut pts = [[r[0], r[1]]];
        MapWindowPoints(0, hwnd, pts.as_mut_ptr(), 1);
        // SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE
        SetWindowPos(child, 0, pts[0][0] + dx, pts[0][1] + dy, 0, 0, 0x1 | 0x4 | 0x10);
        child = GetWindow(child, 2); // GW_HWNDNEXT
    }
}

#[no_mangle]
pub extern "system" fn SetWindowPos(hwnd: Hwnd, after: Hwnd, x: i32, y: i32, cx: i32, cy: i32, flags: u32) -> Bool {
    set_window_pos(hwnd, after, x, y, cx, cy, flags)
}

#[no_mangle]
pub extern "system" fn MoveWindow(hwnd: Hwnd, x: i32, y: i32, w: i32, h: i32, repaint: Bool) -> Bool {
    set_window_pos(hwnd, 0, x, y, w, h, SWP_NOZORDER | SWP_NOACTIVATE | if repaint == 0 { SWP_NOREDRAW } else { 0 })
}

/// Make `hwnd` the active window: deactivate the old one, then
/// WM_WINDOWPOSCHANGING (to the top), WM_ACTIVATEAPP, WM_NCACTIVATE and
/// WM_ACTIVATE, whose DefWindowProc gives it the focus.
fn activate(hwnd: Hwnd) {
    let old = {
        let s = state();
        if s.active == hwnd {
            return;
        }
        s.active
    };
    let mut wp = [0u8; 40];
    wp[0..8].copy_from_slice(&hwnd.to_le_bytes());
    wp[32..36].copy_from_slice(&(SWP_NOSIZE | SWP_NOMOVE).to_le_bytes());
    send(hwnd, WM_WINDOWPOSCHANGING, 0, wp.as_mut_ptr() as i64);
    if old != 0 {
        send(old, WM_NCACTIVATE, 0, 0);
        send(old, WM_ACTIVATE, 0, hwnd as i64);
    } else {
        send(hwnd, WM_ACTIVATEAPP, 1, 0);
    }
    state().active = hwnd;
    send(hwnd, WM_NCACTIVATE, 1, 0);
    send(hwnd, WM_ACTIVATE, 1, old as i64);
}

#[no_mangle]
pub extern "system" fn ShowWindow(hwnd: Hwnd, cmd: i32) -> Bool {
    let Some((visible, top, minimized, maximized)) = state().get(hwnd).map(|w| (w.visible, w.top_level(), w.minimized, w.maximized)) else {
        return 0;
    };
    let no_activate = !top || matches!(cmd, 4 | 7 | 8);
    let extra = if no_activate { SWP_NOACTIVATE | SWP_NOZORDER } else { 0 };
    match cmd {
        0 => {
            if visible {
                send(hwnd, WM_SHOWWINDOW, 0, 0);
                set_window_pos(hwnd, 0, 0, 0, 0, 0, SWP_HIDEWINDOW | SWP_NOSIZE | SWP_NOMOVE | SWP_NOZORDER | if top { 0 } else { SWP_NOACTIVATE });
            }
        }
        2 | 6 | 7 | 11 => {
            if !visible {
                send(hwnd, WM_SHOWWINDOW, 1, 0);
                set_window_pos(hwnd, 0, 0, 0, 0, 0, SWP_SHOWWINDOW | SWP_NOSIZE | SWP_NOMOVE | SWP_NOACTIVATE | SWP_NOZORDER);
            }
            if !minimized {
                if let Some(w) = state().get_mut(hwnd) {
                    w.minimized = true;
                    w.style |= WS_MINIMIZE;
                }
                if top {
                    unsafe { crate::VeilUserShowWindow(hwnd, 2) };
                }
                send(hwnd, WM_SIZE, 1, 0);
            }
        }
        3 => {
            if !visible {
                send(hwnd, WM_SHOWWINDOW, 1, 0);
                set_window_pos(hwnd, 0, 0, 0, 0, 0, SWP_SHOWWINDOW | SWP_NOSIZE | SWP_NOMOVE | extra);
            }
            if !maximized {
                if let Some(w) = state().get_mut(hwnd) {
                    w.maximized = true;
                    w.minimized = false;
                    w.style = (w.style | WS_MAXIMIZE) & !WS_MINIMIZE;
                }
                // The desktop resizes it; the new size arrives as a geometry notice.
                if top {
                    unsafe { crate::VeilUserShowWindow(hwnd, 3) };
                }
            }
        }
        _ => {
            if !visible {
                send(hwnd, WM_SHOWWINDOW, 1, 0);
                set_window_pos(hwnd, 0, 0, 0, 0, 0, SWP_SHOWWINDOW | SWP_NOSIZE | SWP_NOMOVE | extra);
            } else if !no_activate {
                activate(hwnd);
            }
            if (minimized || (maximized && matches!(cmd, 1 | 9))) && top {
                if let Some(w) = state().get_mut(hwnd) {
                    w.minimized = false;
                    w.maximized = false;
                    w.style &= !(WS_MINIMIZE | WS_MAXIMIZE);
                }
                unsafe { crate::VeilUserShowWindow(hwnd, 4) };
                geometry_changed(hwnd);
            }
        }
    }
    visible as Bool
}

#[no_mangle]
pub extern "system" fn ShowWindowAsync(hwnd: Hwnd, cmd: i32) -> Bool {
    ShowWindow(hwnd, cmd)
}

/// The kernel says the desktop moved or resized top-level window `hwnd`.
pub fn geometry_changed(hwnd: Hwnd) {
    let Some(p) = kernel_place(hwnd) else { return };
    let Some(f) = state().get(hwnd).map(|w| w.frame()) else { return };
    let (x, y) = (p.x - f[0], p.y - f[1]);
    let (w, h) = (p.w as i32 + f[0] + f[2], p.h as i32 + f[1] + f[3]);
    let Some(old) = state().get(hwnd).map(|w| (w.x, w.y, w.w, w.h)) else { return };
    if old == (x, y, w, h) {
        return;
    }
    let mut flags = SWP_NOZORDER | SWP_NOACTIVATE;
    if (old.0, old.1) == (x, y) {
        flags |= SWP_NOMOVE;
    }
    if (old.2, old.3) == (w, h) {
        flags |= SWP_NOSIZE;
    }
    set_window_pos(hwnd, 0, x, y, w, h, flags);
}

#[no_mangle]
pub extern "system" fn UpdateWindow(hwnd: Hwnd) -> Bool {
    if state().get(hwnd).is_none() {
        return 0;
    }
    crate::paint::update_now(hwnd);
    1
}

#[no_mangle]
pub extern "system" fn SetFocus(hwnd: Hwnd) -> Hwnd {
    let old = {
        let s = state();
        if hwnd != 0 && s.get(hwnd).is_none() {
            return 0;
        }
        s.focus
    };
    if old == hwnd {
        return old;
    }
    if old != 0 {
        send(old, WM_KILLFOCUS, hwnd, 0);
    }
    state().focus = hwnd;
    if hwnd != 0 {
        send(hwnd, WM_SETFOCUS, old, 0);
    }
    old
}

#[no_mangle]
pub extern "system" fn GetFocus() -> Hwnd {
    state().focus
}

#[no_mangle]
pub extern "system" fn GetActiveWindow() -> Hwnd {
    state().active
}

#[no_mangle]
pub extern "system" fn GetForegroundWindow() -> Hwnd {
    state().active
}

#[no_mangle]
pub extern "system" fn SetActiveWindow(hwnd: Hwnd) -> Hwnd {
    let old = state().active;
    if state().get(hwnd).is_some_and(|w| w.top_level()) {
        activate(hwnd);
    }
    old
}

#[no_mangle]
pub extern "system" fn SetForegroundWindow(hwnd: Hwnd) -> Bool {
    let top = state().get(hwnd).is_some_and(|w| w.top_level() && w.kernel);
    if top {
        unsafe { crate::VeilUserShowWindow(hwnd, 1) };
        activate(hwnd);
    }
    top as Bool
}

#[no_mangle]
pub extern "system" fn BringWindowToTop(hwnd: Hwnd) -> Bool {
    SetForegroundWindow(hwnd);
    1
}

#[no_mangle]
pub extern "system" fn SetCapture(hwnd: Hwnd) -> Hwnd {
    core::mem::replace(&mut state().capture, hwnd)
}

#[no_mangle]
pub extern "system" fn GetCapture() -> Hwnd {
    state().capture
}

#[no_mangle]
pub extern "system" fn ReleaseCapture() -> Bool {
    state().capture = 0;
    1
}

#[no_mangle]
pub extern "system" fn EnableWindow(hwnd: Hwnd, enable: Bool) -> Bool {
    let was_disabled = match state().get_mut(hwnd) {
        Some(w) => {
            let was = !w.enabled;
            w.enabled = enable != 0;
            if enable != 0 { w.style &= !WS_DISABLED } else { w.style |= WS_DISABLED }
            was
        }
        None => return 0,
    };
    if was_disabled == (enable != 0) {
        send(hwnd, WM_ENABLE, (enable != 0) as u64, 0);
    }
    was_disabled as Bool
}

#[no_mangle]
pub extern "system" fn IsWindowEnabled(hwnd: Hwnd) -> Bool {
    state().get(hwnd).is_some_and(|w| w.enabled) as Bool
}

// -----| questions about windows |-----

#[no_mangle]
pub extern "system" fn IsWindow(hwnd: Hwnd) -> Bool {
    state().get(hwnd).is_some() as Bool
}

/// The window's procedure takes Unicode messages.
#[no_mangle]
pub extern "system" fn IsWindowUnicode(hwnd: Hwnd) -> Bool {
    state().get(hwnd).is_some_and(|w| w.unicode) as Bool
}

#[no_mangle]
pub extern "system" fn IsWindowVisible(hwnd: Hwnd) -> Bool {
    let s = state();
    let mut h = hwnd;
    loop {
        match s.get(h) {
            Some(w) if w.visible => {
                if w.parent == 0 {
                    return 1;
                }
                h = w.parent;
            }
            _ => return 0,
        }
    }
}

#[no_mangle]
pub extern "system" fn IsIconic(hwnd: Hwnd) -> Bool {
    state().get(hwnd).is_some_and(|w| w.minimized) as Bool
}

#[no_mangle]
pub extern "system" fn IsZoomed(hwnd: Hwnd) -> Bool {
    state().get(hwnd).is_some_and(|w| w.maximized) as Bool
}

#[no_mangle]
pub extern "system" fn GetParent(hwnd: Hwnd) -> Hwnd {
    match state().get(hwnd) {
        Some(w) if w.style & WS_CHILD != 0 => w.parent,
        Some(w) if w.style & WS_POPUP != 0 => w.owner,
        _ => 0,
    }
}

#[no_mangle]
pub extern "system" fn GetAncestor(hwnd: Hwnd, kind: u32) -> Hwnd {
    let s = state();
    let Some(w) = s.get(hwnd) else { return 0 };
    match kind {
        1 => if w.parent == 0 { DESKTOP } else { w.parent },
        2 => s.root(hwnd),
        3 => {
            let mut r = s.root(hwnd);
            while let Some(o) = s.get(r).map(|w| w.owner).filter(|&o| o != 0) {
                r = o;
            }
            r
        }
        _ => 0,
    }
}

#[no_mangle]
pub extern "system" fn IsChild(parent: Hwnd, hwnd: Hwnd) -> Bool {
    let s = state();
    let mut h = hwnd;
    while let Some(w) = s.get(h) {
        if w.parent == 0 {
            return 0;
        }
        if w.parent == parent {
            return 1;
        }
        h = w.parent;
    }
    0
}

#[no_mangle]
pub extern "system" fn GetDesktopWindow() -> Hwnd {
    DESKTOP
}

#[no_mangle]
pub extern "system" fn GetShellWindow() -> Hwnd {
    0
}

/// GetWindow: siblings in creation order (the first created is on top).
#[no_mangle]
pub extern "system" fn GetWindow(hwnd: Hwnd, cmd: u32) -> Hwnd {
    let s = state();
    let parent_of = |h: Hwnd| if h == DESKTOP { None } else { s.get(h).map(|w| w.parent) };
    let siblings = |p: Hwnd| -> Vec<Hwnd> { s.windows.iter().filter(|w| w.parent == p && !w.destroying).map(|w| w.hwnd).collect() };
    match cmd {
        0 | 1 | 2 | 3 => {
            let Some(p) = parent_of(hwnd) else { return 0 };
            let sib = siblings(p);
            let i = sib.iter().position(|&h| h == hwnd).unwrap_or(0);
            match cmd {
                0 => sib.first().copied().unwrap_or(0),
                1 => sib.last().copied().unwrap_or(0),
                2 => sib.get(i + 1).copied().unwrap_or(0),
                _ => if i == 0 { 0 } else { sib[i - 1] },
            }
        }
        4 => s.get(hwnd).map_or(0, |w| w.owner),
        5 => siblings(if hwnd == DESKTOP { 0 } else { hwnd }).first().copied().unwrap_or(0),
        _ => 0,
    }
}

#[no_mangle]
pub extern "system" fn GetTopWindow(hwnd: Hwnd) -> Hwnd {
    GetWindow(if hwnd == 0 { DESKTOP } else { hwnd }, 5)
}

#[no_mangle]
pub extern "system" fn GetNextWindow(hwnd: Hwnd, cmd: u32) -> Hwnd {
    GetWindow(hwnd, cmd)
}

type EnumProc = unsafe extern "system" fn(Hwnd, i64) -> Bool;

fn enumerate(list: Vec<Hwnd>, proc: EnumProc, lp: i64) -> Bool {
    for h in list {
        if unsafe { proc(h, lp) } == 0 {
            break;
        }
    }
    1
}

#[no_mangle]
pub extern "system" fn EnumWindows(proc: EnumProc, lp: i64) -> Bool {
    let list = state().windows.iter().filter(|w| w.parent == 0 && w.kernel).map(|w| w.hwnd).collect();
    enumerate(list, proc, lp)
}

#[no_mangle]
pub extern "system" fn EnumChildWindows(parent: Hwnd, proc: EnumProc, lp: i64) -> Bool {
    let list = {
        let s = state();
        let mut all = family(&s, parent);
        all.remove(0);
        all
    };
    enumerate(list, proc, lp)
}

#[no_mangle]
pub extern "system" fn EnumThreadWindows(tid: u32, proc: EnumProc, lp: i64) -> Bool {
    let list = state().windows.iter().filter(|w| w.parent == 0 && w.tid == tid).map(|w| w.hwnd).collect();
    enumerate(list, proc, lp)
}

fn find(parent: Hwnd, after: Hwnd, class: Option<Name>, title: Option<Vec<u16>>) -> Hwnd {
    let s = state();
    let ci = match &class {
        Some(n) => match crate::find_class(&s, n) {
            Some(i) => Some(i),
            None => return 0,
        },
        None => None,
    };
    let p = if parent == DESKTOP { 0 } else { parent };
    let mut past = after == 0;
    for w in s.windows.iter().filter(|w| w.parent == p && !w.destroying) {
        if !past {
            past = w.hwnd == after;
            continue;
        }
        if ci.is_some_and(|c| c != w.class) {
            continue;
        }
        if title.as_ref().is_some_and(|t| *t != w.text) {
            continue;
        }
        return w.hwnd;
    }
    0
}

#[no_mangle]
pub unsafe extern "system" fn FindWindowW(class: *const u16, title: *const u16) -> Hwnd {
    let class = (!class.is_null()).then(|| unsafe { crate::name_w(class) });
    let title = (!title.is_null()).then(|| unsafe { crate::wstr(title) });
    find(0, 0, class, title)
}

#[no_mangle]
pub unsafe extern "system" fn FindWindowA(class: *const u8, title: *const u8) -> Hwnd {
    let class = (!class.is_null()).then(|| unsafe { crate::name_a(class) });
    let title = (!title.is_null()).then(|| unsafe { crate::from_ansi(title, -1) });
    find(0, 0, class, title)
}

#[no_mangle]
pub unsafe extern "system" fn FindWindowExW(parent: Hwnd, after: Hwnd, class: *const u16, title: *const u16) -> Hwnd {
    let class = (!class.is_null()).then(|| unsafe { crate::name_w(class) });
    let title = (!title.is_null()).then(|| unsafe { crate::wstr(title) });
    find(parent, after, class, title)
}

#[no_mangle]
pub unsafe extern "system" fn FindWindowExA(parent: Hwnd, after: Hwnd, class: *const u8, title: *const u8) -> Hwnd {
    let class = (!class.is_null()).then(|| unsafe { crate::name_a(class) });
    let title = (!title.is_null()).then(|| unsafe { crate::from_ansi(title, -1) });
    find(parent, after, class, title)
}

#[no_mangle]
pub unsafe extern "system" fn GetWindowThreadProcessId(hwnd: Hwnd, pid: *mut u32) -> u32 {
    let Some(tid) = state().get(hwnd).map(|w| w.tid) else { return 0 };
    if !pid.is_null() {
        unsafe { *pid = crate::GetCurrentProcessId() };
    }
    tid
}

#[no_mangle]
pub unsafe extern "system" fn GetClassNameW(hwnd: Hwnd, out: *mut u16, cap: i32) -> i32 {
    let s = state();
    let Some(w) = s.get(hwnd) else { return 0 };
    unsafe { crate::copy_out(&s.classes[w.class].name, out, cap.max(0) as usize) as i32 }
}

#[no_mangle]
pub unsafe extern "system" fn GetClassNameA(hwnd: Hwnd, out: *mut u8, cap: i32) -> i32 {
    let s = state();
    let Some(w) = s.get(hwnd) else { return 0 };
    unsafe { crate::copy_out(&crate::to_ansi(&s.classes[w.class].name), out, cap.max(0) as usize) as i32 }
}

#[no_mangle]
pub extern "system" fn GetDlgCtrlID(hwnd: Hwnd) -> i32 {
    state().get(hwnd).map_or(0, |w| w.id as i32)
}

// -----| window data |-----

const GWLP_WNDPROC: i32 = -4;
const GWLP_HINSTANCE: i32 = -6;
const GWLP_HWNDPARENT: i32 = -8;
const GWLP_ID: i32 = -12;
const GWL_STYLE: i32 = -16;
const GWL_EXSTYLE: i32 = -20;
const GWLP_USERDATA: i32 = -21;

fn get_long(hwnd: Hwnd, index: i32, size: usize) -> i64 {
    let s = state();
    let Some(w) = s.get(hwnd) else {
        set_error(crate::ERROR_INVALID_WINDOW_HANDLE);
        return 0;
    };
    match index {
        GWLP_WNDPROC => w.proc as i64,
        GWLP_HINSTANCE => w.instance as i64,
        GWLP_HWNDPARENT => (if w.parent != 0 { w.parent } else { w.owner }) as i64,
        GWLP_ID => w.id as i64,
        GWL_STYLE => w.style as i32 as i64,
        GWL_EXSTYLE => w.ex_style as i32 as i64,
        GWLP_USERDATA => w.user_data as i64,
        i if i >= 0 && i as usize + size <= w.extra.len() => {
            let mut b = [0u8; 8];
            b[..size].copy_from_slice(&w.extra[i as usize..i as usize + size]);
            if size == 4 { i32::from_le_bytes(b[..4].try_into().unwrap()) as i64 } else { i64::from_le_bytes(b) }
        }
        _ => {
            set_error(1413); // ERROR_INVALID_INDEX
            0
        }
    }
}

fn set_long(hwnd: Hwnd, index: i32, value: i64, size: usize, unicode: bool) -> i64 {
    let old = get_long(hwnd, index, size);
    if index == GWL_STYLE || index == GWL_EXSTYLE {
        // STYLESTRUCT: old and new.
        let mut ss = [old as u32, value as u32];
        send(hwnd, 0x007C, index as i64 as u64, ss.as_mut_ptr() as i64); // WM_STYLECHANGING
        let new = ss[1];
        if let Some(w) = state().get_mut(hwnd) {
            if index == GWL_STYLE {
                w.style = new;
            } else {
                w.ex_style = new;
            }
        }
        let mut ss = [old as u32, new];
        send(hwnd, 0x007D, index as i64 as u64, ss.as_mut_ptr() as i64); // WM_STYLECHANGED
        return old;
    }
    let mut s = state();
    let Some(w) = s.get_mut(hwnd) else { return 0 };
    match index {
        GWLP_WNDPROC => {
            w.proc = value as usize;
            w.unicode = unicode;
        }
        GWLP_HINSTANCE => w.instance = value as u64,
        GWLP_HWNDPARENT => {
            if w.parent == 0 {
                w.owner = value as u64;
            }
        }
        GWLP_ID => w.id = value as u64,
        GWLP_USERDATA => w.user_data = value as u64,
        i if i >= 0 && i as usize + size <= w.extra.len() => {
            let b = value.to_le_bytes();
            w.extra[i as usize..i as usize + size].copy_from_slice(&b[..size]);
        }
        _ => {
            drop(s);
            set_error(1413);
            return 0;
        }
    }
    old
}

#[no_mangle]
pub extern "system" fn GetWindowLongPtrW(hwnd: Hwnd, index: i32) -> i64 {
    get_long(hwnd, index, 8)
}

#[no_mangle]
pub extern "system" fn GetWindowLongPtrA(hwnd: Hwnd, index: i32) -> i64 {
    get_long(hwnd, index, 8)
}

#[no_mangle]
pub extern "system" fn GetWindowLongW(hwnd: Hwnd, index: i32) -> i32 {
    get_long(hwnd, index, 4) as i32
}

#[no_mangle]
pub extern "system" fn GetWindowLongA(hwnd: Hwnd, index: i32) -> i32 {
    get_long(hwnd, index, 4) as i32
}

#[no_mangle]
pub extern "system" fn SetWindowLongPtrW(hwnd: Hwnd, index: i32, value: i64) -> i64 {
    set_long(hwnd, index, value, 8, true)
}

#[no_mangle]
pub extern "system" fn SetWindowLongPtrA(hwnd: Hwnd, index: i32, value: i64) -> i64 {
    set_long(hwnd, index, value, 8, false)
}

#[no_mangle]
pub extern "system" fn SetWindowLongW(hwnd: Hwnd, index: i32, value: i32) -> i32 {
    set_long(hwnd, index, value as i64, 4, true) as i32
}

#[no_mangle]
pub extern "system" fn SetWindowLongA(hwnd: Hwnd, index: i32, value: i32) -> i32 {
    set_long(hwnd, index, value as i64, 4, false) as i32
}

const GCLP_MENUNAME: i32 = -8;
const GCLP_HBRBACKGROUND: i32 = -10;
const GCLP_HCURSOR: i32 = -12;
const GCLP_HICON: i32 = -14;
const GCLP_HMODULE: i32 = -16;
const GCL_CBWNDEXTRA: i32 = -18;
const GCL_CBCLSEXTRA: i32 = -20;
const GCLP_WNDPROC: i32 = -24;
const GCL_STYLE: i32 = -26;
const GCW_ATOM: i32 = -32;
const GCLP_HICONSM: i32 = -34;

fn class_long(hwnd: Hwnd, index: i32, set: Option<i64>, size: usize) -> i64 {
    let mut s = state();
    let Some(ci) = s.get(hwnd).map(|w| w.class) else {
        set_error(crate::ERROR_INVALID_WINDOW_HANDLE);
        return 0;
    };
    let c = &mut s.classes[ci];
    let field: &mut u64 = match index {
        GCLP_MENUNAME => &mut c.menu_name,
        GCLP_HBRBACKGROUND => &mut c.brush,
        GCLP_HCURSOR => &mut c.cursor,
        GCLP_HICON => &mut c.icon,
        GCLP_HMODULE => &mut c.instance,
        GCLP_HICONSM => &mut c.icon_sm,
        GCL_CBWNDEXTRA => return c.wnd_extra as i64,
        GCL_CBCLSEXTRA => return c.cls_extra.len() as i64,
        GCW_ATOM => return c.atom as i64,
        GCLP_WNDPROC => {
            let old = c.proc as i64;
            if let Some(v) = set {
                c.proc = v as usize;
            }
            return old;
        }
        GCL_STYLE => {
            let old = c.style as i64;
            if let Some(v) = set {
                c.style = v as u32;
            }
            return old;
        }
        i if i >= 0 && i as usize + size <= c.cls_extra.len() => {
            let at = i as usize;
            let mut b = [0u8; 8];
            b[..size].copy_from_slice(&c.cls_extra[at..at + size]);
            if let Some(v) = set {
                c.cls_extra[at..at + size].copy_from_slice(&v.to_le_bytes()[..size]);
            }
            return i64::from_le_bytes(b);
        }
        _ => return 0,
    };
    let old = *field as i64;
    if let Some(v) = set {
        *field = v as u64;
    }
    old
}

#[no_mangle]
pub extern "system" fn GetClassLongPtrW(hwnd: Hwnd, index: i32) -> u64 {
    class_long(hwnd, index, None, 8) as u64
}

#[no_mangle]
pub extern "system" fn GetClassLongPtrA(hwnd: Hwnd, index: i32) -> u64 {
    class_long(hwnd, index, None, 8) as u64
}

#[no_mangle]
pub extern "system" fn GetClassLongW(hwnd: Hwnd, index: i32) -> u32 {
    class_long(hwnd, index, None, 4) as u32
}

#[no_mangle]
pub extern "system" fn GetClassLongA(hwnd: Hwnd, index: i32) -> u32 {
    class_long(hwnd, index, None, 4) as u32
}

#[no_mangle]
pub extern "system" fn SetClassLongPtrW(hwnd: Hwnd, index: i32, v: i64) -> u64 {
    class_long(hwnd, index, Some(v), 8) as u64
}

#[no_mangle]
pub extern "system" fn SetClassLongPtrA(hwnd: Hwnd, index: i32, v: i64) -> u64 {
    class_long(hwnd, index, Some(v), 8) as u64
}

#[no_mangle]
pub extern "system" fn SetClassLongW(hwnd: Hwnd, index: i32, v: i32) -> u32 {
    class_long(hwnd, index, Some(v as i64), 4) as u32
}

#[no_mangle]
pub extern "system" fn SetClassLongA(hwnd: Hwnd, index: i32, v: i32) -> u32 {
    class_long(hwnd, index, Some(v as i64), 4) as u32
}

#[no_mangle]
pub extern "system" fn GetClassWord(hwnd: Hwnd, index: i32) -> u16 {
    class_long(hwnd, index, None, 2) as u16
}

// -----| text |-----

/// The window's text, as WM_SETTEXT's DefWindowProc keeps it.
pub fn set_text(hwnd: Hwnd, text: Vec<u16>) {
    let kernel = {
        let mut s = state();
        let Some(w) = s.get_mut(hwnd) else { return };
        w.text = text.clone();
        w.kernel
    };
    if kernel {
        let mut z = text;
        z.push(0);
        unsafe { crate::VeilUserSetText(hwnd, z.as_ptr()) };
    }
}

#[no_mangle]
pub unsafe extern "system" fn SetWindowTextW(hwnd: Hwnd, text: *const u16) -> Bool {
    call(hwnd, WM_SETTEXT, 0, text as i64, true) as Bool
}

#[no_mangle]
pub unsafe extern "system" fn SetWindowTextA(hwnd: Hwnd, text: *const u8) -> Bool {
    call(hwnd, WM_SETTEXT, 0, text as i64, false) as Bool
}

#[no_mangle]
pub unsafe extern "system" fn GetWindowTextW(hwnd: Hwnd, out: *mut u16, cap: i32) -> i32 {
    if cap <= 0 || out.is_null() {
        return 0;
    }
    unsafe { *out = 0 };
    call(hwnd, WM_GETTEXT, cap as u64, out as i64, true) as i32
}

#[no_mangle]
pub unsafe extern "system" fn GetWindowTextA(hwnd: Hwnd, out: *mut u8, cap: i32) -> i32 {
    if cap <= 0 || out.is_null() {
        return 0;
    }
    unsafe { *out = 0 };
    call(hwnd, WM_GETTEXT, cap as u64, out as i64, false) as i32
}

#[no_mangle]
pub extern "system" fn GetWindowTextLengthW(hwnd: Hwnd) -> i32 {
    call(hwnd, WM_GETTEXTLENGTH, 0, 0, true) as i32
}

#[no_mangle]
pub extern "system" fn GetWindowTextLengthA(hwnd: Hwnd) -> i32 {
    call(hwnd, WM_GETTEXTLENGTH, 0, 0, false) as i32
}

// -----| geometry |-----

fn write_rect(out: *mut [i32; 4], r: [i32; 4]) -> Bool {
    if out.is_null() {
        return 0;
    }
    unsafe { *out = r };
    1
}

#[no_mangle]
pub extern "system" fn GetClientRect(hwnd: Hwnd, out: *mut [i32; 4]) -> Bool {
    if hwnd == DESKTOP {
        let (w, h) = crate::screen_size();
        return write_rect(out, [0, 0, w, h]);
    }
    match state().get(hwnd) {
        Some(w) => write_rect(out, [0, 0, w.client_w(), w.client_h()]),
        None => {
            set_error(crate::ERROR_INVALID_WINDOW_HANDLE);
            0
        }
    }
}

#[no_mangle]
pub extern "system" fn GetWindowRect(hwnd: Hwnd, out: *mut [i32; 4]) -> Bool {
    if hwnd == DESKTOP {
        let (w, h) = crate::screen_size();
        return write_rect(out, [0, 0, w, h]);
    }
    let s = state();
    let Some(w) = s.get(hwnd) else {
        set_error(crate::ERROR_INVALID_WINDOW_HANDLE);
        return 0;
    };
    let (px, py) = if w.parent == 0 { (0, 0) } else { s.client_to_screen(w.parent) };
    write_rect(out, [px + w.x, py + w.y, px + w.x + w.w, py + w.y + w.h])
}

/// A window's client (0, 0) on screen (the desktop's is (0, 0)).
fn origin(hwnd: Hwnd) -> Option<(i32, i32)> {
    if hwnd == 0 || hwnd == DESKTOP {
        return Some((0, 0));
    }
    let s = state();
    s.get(hwnd)?;
    Some(s.client_to_screen(hwnd))
}

#[no_mangle]
pub extern "system" fn ClientToScreen(hwnd: Hwnd, p: *mut [i32; 2]) -> Bool {
    let Some((x, y)) = origin(hwnd) else { return 0 };
    unsafe {
        (*p)[0] += x;
        (*p)[1] += y;
    }
    1
}

#[no_mangle]
pub extern "system" fn ScreenToClient(hwnd: Hwnd, p: *mut [i32; 2]) -> Bool {
    let Some((x, y)) = origin(hwnd) else { return 0 };
    unsafe {
        (*p)[0] -= x;
        (*p)[1] -= y;
    }
    1
}

#[no_mangle]
pub extern "system" fn MapWindowPoints(from: Hwnd, to: Hwnd, pts: *mut [i32; 2], n: u32) -> i32 {
    let (Some(a), Some(b)) = (origin(from), origin(to)) else {
        set_error(crate::ERROR_INVALID_WINDOW_HANDLE);
        return 0;
    };
    let (dx, dy) = (a.0 - b.0, a.1 - b.1);
    for i in 0..n as usize {
        unsafe {
            (*pts.add(i))[0] += dx;
            (*pts.add(i))[1] += dy;
        }
    }
    ((dy as u16 as i32) << 16) | dx as u16 as i32
}

/// WM_NCHITTEST's answer for screen (x, y): HTCLIENT, a scroll bar,
/// HTBORDER or HTNOWHERE.
pub fn nc_hit(hwnd: Hwnd, x: i32, y: i32) -> u64 {
    let mut r = [0i32; 4];
    if GetWindowRect(hwnd, &mut r) == 0 {
        return 0;
    }
    let (wx, wy) = (x - r[0], y - r[1]);
    if wx < 0 || wy < 0 || x >= r[2] || y >= r[3] {
        return 0;
    }
    if crate::menu::on_bar(hwnd, wx, wy) {
        return 5; // HTMENU
    }
    let bars = crate::scroll::bar_rects(hwnd);
    let inside = |b: Option<[i32; 4]>| b.is_some_and(|b| wx >= b[0] && wx < b[2] && wy >= b[1] && wy < b[3]);
    if inside(bars[1]) {
        return crate::scroll::HTVSCROLL;
    }
    if inside(bars[0]) {
        return crate::scroll::HTHSCROLL;
    }
    let Some(m) = state().get(hwnd).map(|w| w.margins) else { return 0 };
    if wx >= m[0] && wy >= m[1] && wx < r[2] - r[0] - m[2] && wy < r[3] - r[1] - m[3] {
        1 // HTCLIENT
    } else {
        18 // HTBORDER
    }
}

/// The deepest visible, enabled child of `top` under its client point
/// (x, y), or `top`.
pub fn child_at(s: &State, top: Hwnd, x: i32, y: i32) -> Hwnd {
    let mut h = top;
    let (mut x, mut y) = (x, y);
    'down: loop {
        // Later siblings lie under earlier ones.
        for w in s.windows.iter().filter(|w| w.parent == h && w.visible && !w.destroying) {
            if x >= w.x && x < w.x + w.w && y >= w.y && y < w.y + w.h {
                x -= w.x + w.margins[0];
                y -= w.y + w.margins[1];
                h = w.hwnd;
                continue 'down;
            }
        }
        return h;
    }
}

#[no_mangle]
pub extern "system" fn WindowFromPoint(x: i32, y: i32) -> Hwnd {
    let s = state();
    for w in s.windows.iter().filter(|w| w.parent == 0 && w.kernel && w.visible) {
        let (cx, cy) = (w.x + w.margins[0], w.y + w.margins[1]);
        if x >= w.x && x < w.x + w.w && y >= w.y && y < w.y + w.h {
            return child_at(&s, w.hwnd, x - cx, y - cy);
        }
    }
    0
}

#[no_mangle]
pub extern "system" fn ChildWindowFromPoint(hwnd: Hwnd, x: i32, y: i32) -> Hwnd {
    let s = state();
    let Some(p) = s.get(hwnd) else { return 0 };
    if x < 0 || y < 0 || x >= p.client_w() || y >= p.client_h() {
        return 0;
    }
    s.windows
        .iter()
        .find(|w| w.parent == hwnd && x >= w.x && x < w.x + w.w && y >= w.y && y < w.y + w.h)
        .map_or(hwnd, |w| w.hwnd)
}

#[no_mangle]
pub extern "system" fn ChildWindowFromPointEx(hwnd: Hwnd, x: i32, y: i32, _flags: u32) -> Hwnd {
    ChildWindowFromPoint(hwnd, x, y)
}

/// WINDOWPLACEMENT: length, flags, showCmd, min position, max position,
/// normal rectangle.
#[no_mangle]
pub unsafe extern "system" fn GetWindowPlacement(hwnd: Hwnd, out: *mut i32) -> Bool {
    let s = state();
    let Some(w) = s.get(hwnd) else { return 0 };
    let show = if w.minimized { 2 } else if w.maximized { 3 } else if w.visible { 1 } else { 0 };
    let v = [44, 0, show, -1, -1, -1, -1, w.x, w.y, w.x + w.w, w.y + w.h];
    for (i, x) in v.iter().enumerate() {
        unsafe { *out.add(i) = *x };
    }
    1
}

#[no_mangle]
pub unsafe extern "system" fn SetWindowPlacement(hwnd: Hwnd, p: *const i32) -> Bool {
    let r = unsafe { [*p.add(7), *p.add(8), *p.add(9), *p.add(10)] };
    let show = unsafe { *p.add(2) };
    set_window_pos(hwnd, 0, r[0], r[1], r[2] - r[0], r[3] - r[1], SWP_NOZORDER | SWP_NOACTIVATE);
    ShowWindow(hwnd, show);
    1
}

#[no_mangle]
pub extern "system" fn SetParent(hwnd: Hwnd, parent: Hwnd) -> Hwnd {
    let mut s = state();
    if parent != 0 && s.get(parent).is_none() {
        return 0;
    }
    match s.get_mut(hwnd) {
        Some(w) if !w.kernel => core::mem::replace(&mut w.parent, parent),
        _ => 0,
    }
}
