//! Messages: calling window procedures (with ANSI/Unicode conversion of the
//! text messages), the thread's queue as GetMessage/PeekMessage see it, timers,
//! and DefWindowProc.
//!
//! The queue is the kernel's (posted messages, input, timers) plus WM_PAINT
//! for windows with an update rectangle. Order: posted and input messages,
//! then WM_PAINT, then WM_TIMER, as on Windows.

use alloc::vec::Vec;
use nt_abi::Msg;

use crate::{state, Bool, Hwnd, WndProc};

pub const WM_CREATE: u32 = 0x0001;
pub const WM_DESTROY: u32 = 0x0002;
pub const WM_MOVE: u32 = 0x0003;
pub const WM_SIZE: u32 = 0x0005;
pub const WM_ACTIVATE: u32 = 0x0006;
pub const WM_SETFOCUS: u32 = 0x0007;
pub const WM_KILLFOCUS: u32 = 0x0008;
pub const WM_ENABLE: u32 = 0x000A;
pub const WM_SETTEXT: u32 = 0x000C;
pub const WM_GETTEXT: u32 = 0x000D;
pub const WM_GETTEXTLENGTH: u32 = 0x000E;
pub const WM_PAINT: u32 = 0x000F;
pub const WM_CLOSE: u32 = 0x0010;
pub const WM_QUIT: u32 = 0x0012;
pub const WM_ERASEBKGND: u32 = 0x0014;
pub const WM_SHOWWINDOW: u32 = 0x0018;
pub const WM_ACTIVATEAPP: u32 = 0x001C;
pub const WM_MOUSEACTIVATE: u32 = 0x0021;
pub const WM_GETMINMAXINFO: u32 = 0x0024;
pub const WM_WINDOWPOSCHANGING: u32 = 0x0046;
pub const WM_WINDOWPOSCHANGED: u32 = 0x0047;
pub const WM_NCCREATE: u32 = 0x0081;
pub const WM_NCDESTROY: u32 = 0x0082;
pub const WM_NCCALCSIZE: u32 = 0x0083;
pub const WM_NCHITTEST: u32 = 0x0084;
pub const WM_NCPAINT: u32 = 0x0085;
pub const WM_NCACTIVATE: u32 = 0x0086;
pub const WM_KEYDOWN: u32 = 0x0100;
pub const WM_KEYUP: u32 = 0x0101;
pub const WM_SYSKEYDOWN: u32 = 0x0104;
pub const WM_SYSCOMMAND: u32 = 0x0112;
pub const WM_TIMER: u32 = 0x0113;
pub const WM_MOUSEFIRST: u32 = 0x0200;
pub const WM_MOUSELAST: u32 = 0x020E;
pub const WM_PARENTNOTIFY: u32 = 0x0210;

/// Private messages from the kernel and between threads (outside the
/// 16-bit range programs use).
const VEIL_WAKE: u32 = 0x7FFF_0001;
const VEIL_GEOMETRY: u32 = 0x7FFF_0002;
/// The pointer left this top-level window.
const VEIL_MOUSE_LEFT: u32 = 0x7FFF_0003;

const PM_REMOVE: u32 = 1;

// -----| calling window procedures |-----

/// Call `hwnd`'s procedure with a message from a caller using Unicode
/// (`unicode`) or ANSI strings, converting text messages for a procedure of
/// the other kind.
pub fn call(hwnd: Hwnd, msg: u32, wp: u64, lp: i64, unicode: bool) -> i64 {
    let (proc, wnd_unicode) = match state().get(hwnd) {
        Some(w) => (w.proc, w.unicode),
        None => return 0,
    };
    call_proc(proc, wnd_unicode, hwnd, msg, wp, lp, unicode)
}

/// Send from user32 itself (Unicode strings).
pub fn send(hwnd: Hwnd, msg: u32, wp: u64, lp: i64) -> i64 {
    call(hwnd, msg, wp, lp, true)
}

pub fn call_proc(proc: usize, wnd_unicode: bool, hwnd: Hwnd, msg: u32, wp: u64, lp: i64, unicode: bool) -> i64 {
    if proc == 0 {
        return 0;
    }
    let f: WndProc = unsafe { core::mem::transmute(proc) };
    if wnd_unicode == unicode {
        return unsafe { f(hwnd, msg, wp, lp) };
    }
    match msg {
        WM_SETTEXT if lp != 0 => {
            let mut conv = convert_str(lp as u64, unicode);
            unsafe { f(hwnd, msg, wp, conv.as_mut_ptr() as i64) }
        }
        WM_GETTEXT if lp != 0 && wp != 0 => {
            // Into a buffer of the procedure's kind, then back.
            let mut buf = alloc::vec![0u8; wp as usize * 4 + 4];
            let r = unsafe { f(hwnd, msg, wp * 2, buf.as_mut_ptr() as i64) };
            let text: Vec<u16> = if wnd_unicode {
                let p = buf.as_ptr() as *const u16;
                (0..r.max(0) as usize).map(|i| unsafe { *p.add(i) }).collect()
            } else {
                unsafe { crate::from_ansi(buf.as_ptr(), r.max(0) as i32) }
            };
            if unicode {
                unsafe { crate::copy_out(&text, lp as *mut u16, wp as usize) as i64 }
            } else {
                unsafe { crate::copy_out(&crate::to_ansi(&text), lp as *mut u8, wp as usize) as i64 }
            }
        }
        WM_NCCREATE | WM_CREATE if lp != 0 => {
            // CREATESTRUCT with its window and class names converted.
            let mut cs = [0u8; 80];
            unsafe { core::ptr::copy_nonoverlapping(lp as *const u8, cs.as_mut_ptr(), 80) };
            let name = u64::from_le_bytes(cs[56..64].try_into().unwrap());
            let class = u64::from_le_bytes(cs[64..72].try_into().unwrap());
            let mut name_c = if name >> 16 != 0 { convert_str(name, unicode) } else { Vec::new() };
            let mut class_c = if class >> 16 != 0 { convert_str(class, unicode) } else { Vec::new() };
            if !name_c.is_empty() {
                cs[56..64].copy_from_slice(&(name_c.as_mut_ptr() as u64).to_le_bytes());
            }
            if !class_c.is_empty() {
                cs[64..72].copy_from_slice(&(class_c.as_mut_ptr() as u64).to_le_bytes());
            }
            unsafe { f(hwnd, msg, wp, cs.as_mut_ptr() as i64) }
        }
        _ if lp != 0 && string_in(hwnd, msg) => {
            let mut conv = convert_str(lp as u64, unicode);
            unsafe { f(hwnd, msg, wp, conv.as_mut_ptr() as i64) }
        }
        _ if lp != 0 && string_out(hwnd, msg) => {
            // Into a big buffer of the procedure's kind (EM_GETLINE's first
            // word is its size), then back without the NUL's change.
            let cap = if msg == 0x00C4 { (unsafe { *(lp as *const u16) }) as usize } else { 4096 };
            let mut buf = alloc::vec![0u8; cap * 4 + 8];
            if msg == 0x00C4 {
                buf[..2].copy_from_slice(&(cap as u16).to_le_bytes());
            }
            let r = unsafe { f(hwnd, msg, wp, buf.as_mut_ptr() as i64) };
            if r < 0 {
                return r;
            }
            let text: Vec<u16> = if wnd_unicode {
                let p = buf.as_ptr() as *const u16;
                (0..r as usize).map(|i| unsafe { *p.add(i) }).collect()
            } else {
                unsafe { crate::from_ansi(buf.as_ptr(), r as i32) }
            };
            let nul = msg != 0x00C4;
            if unicode {
                unsafe { core::ptr::copy_nonoverlapping(text.as_ptr(), lp as *mut u16, text.len()) };
                if nul {
                    unsafe { *(lp as *mut u16).add(text.len()) = 0 };
                }
                text.len() as i64
            } else {
                let a = crate::to_ansi(&text);
                unsafe { core::ptr::copy_nonoverlapping(a.as_ptr(), lp as *mut u8, a.len()) };
                if nul {
                    unsafe { *(lp as *mut u8).add(a.len()) = 0 };
                }
                a.len() as i64
            }
        }
        _ => unsafe { f(hwnd, msg, wp, lp) },
    }
}

/// Messages to list boxes, combo boxes and edits that pass a string in
/// lParam (converted between ANSI and Unicode callers like WM_SETTEXT).
fn string_in(hwnd: Hwnd, msg: u32) -> bool {
    match msg {
        0x0180 | 0x0181 | 0x018C | 0x018F | 0x01A2 | 0x0143 | 0x014A | 0x014C | 0x014D | 0x0158 => crate::controls::takes_strings(hwnd),
        0x00C2 => true, // EM_REPLACESEL
        _ => false,
    }
}

/// ... and those that return one into lParam's buffer.
fn string_out(hwnd: Hwnd, msg: u32) -> bool {
    match msg {
        0x0189 | 0x0148 => crate::controls::takes_strings(hwnd), // LB_GETTEXT, CB_GETLBTEXT
        0x00C4 => true,                                           // EM_GETLINE
        _ => false,
    }
}

/// A NUL-terminated string from a caller of the other kind, as bytes of the
/// procedure's kind (NUL-terminated).
fn convert_str(p: u64, from_unicode: bool) -> Vec<u8> {
    if from_unicode {
        let mut v = crate::to_ansi(&unsafe { crate::wstr(p as *const u16) });
        v.push(0);
        v
    } else {
        let w = unsafe { crate::from_ansi(p as *const u8, -1) };
        let mut v: Vec<u8> = w.iter().flat_map(|c| c.to_le_bytes()).collect();
        v.extend_from_slice(&[0, 0]);
        v
    }
}

#[no_mangle]
pub extern "system" fn SendMessageW(hwnd: Hwnd, msg: u32, wp: u64, lp: i64) -> i64 {
    call(hwnd, msg, wp, lp, true)
}

#[no_mangle]
pub extern "system" fn SendMessageA(hwnd: Hwnd, msg: u32, wp: u64, lp: i64) -> i64 {
    call(hwnd, msg, wp, lp, false)
}

#[no_mangle]
pub extern "system" fn SendNotifyMessageW(hwnd: Hwnd, msg: u32, wp: u64, lp: i64) -> Bool {
    call(hwnd, msg, wp, lp, true);
    1
}

#[no_mangle]
pub extern "system" fn SendNotifyMessageA(hwnd: Hwnd, msg: u32, wp: u64, lp: i64) -> Bool {
    call(hwnd, msg, wp, lp, false);
    1
}

#[no_mangle]
pub unsafe extern "system" fn SendMessageTimeoutW(hwnd: Hwnd, msg: u32, wp: u64, lp: i64, _flags: u32, _ms: u32, result: *mut i64) -> i64 {
    let r = call(hwnd, msg, wp, lp, true);
    if !result.is_null() {
        unsafe { *result = r };
    }
    1
}

#[no_mangle]
pub unsafe extern "system" fn SendMessageTimeoutA(hwnd: Hwnd, msg: u32, wp: u64, lp: i64, _flags: u32, _ms: u32, result: *mut i64) -> i64 {
    let r = call(hwnd, msg, wp, lp, false);
    if !result.is_null() {
        unsafe { *result = r };
    }
    1
}

#[no_mangle]
pub extern "system" fn CallWindowProcW(proc: usize, hwnd: Hwnd, msg: u32, wp: u64, lp: i64) -> i64 {
    call_proc(proc, true, hwnd, msg, wp, lp, true)
}

#[no_mangle]
pub extern "system" fn CallWindowProcA(proc: usize, hwnd: Hwnd, msg: u32, wp: u64, lp: i64) -> i64 {
    call_proc(proc, false, hwnd, msg, wp, lp, false)
}

/// For comctl32's subclassing: call `proc` (taking Unicode messages if
/// `proc_unicode`) with a message from a caller using Unicode strings or
/// not, converting between them like CallWindowProc's thunks.
#[no_mangle]
pub extern "system" fn VeilCallProc(proc: usize, proc_unicode: Bool, hwnd: Hwnd, msg: u32, wp: u64, lp: i64, from_unicode: Bool) -> i64 {
    call_proc(proc, proc_unicode != 0, hwnd, msg, wp, lp, from_unicode != 0)
}

#[no_mangle]
pub extern "system" fn InSendMessage() -> Bool {
    0
}

#[no_mangle]
pub extern "system" fn ReplyMessage(_result: i64) -> Bool {
    0
}

// -----| posting |-----

const HWND_BROADCAST: Hwnd = 0xFFFF;

fn post(hwnd: Hwnd, msg: u32, wp: u64, lp: i64) -> Bool {
    if hwnd == HWND_BROADCAST {
        return 1;
    }
    let tid = if hwnd == 0 {
        0
    } else {
        match state().get(hwnd) {
            Some(w) => w.tid,
            None => {
                crate::set_error(crate::ERROR_INVALID_WINDOW_HANDLE);
                return 0;
            }
        }
    };
    (unsafe { crate::VeilUserPost(hwnd, msg, wp, lp as u64, tid) } == 0) as Bool
}

#[no_mangle]
pub extern "system" fn PostMessageW(hwnd: Hwnd, msg: u32, wp: u64, lp: i64) -> Bool {
    post(hwnd, msg, wp, lp)
}

#[no_mangle]
pub extern "system" fn PostMessageA(hwnd: Hwnd, msg: u32, wp: u64, lp: i64) -> Bool {
    post(hwnd, msg, wp, lp)
}

#[no_mangle]
pub extern "system" fn PostThreadMessageW(tid: u32, msg: u32, wp: u64, lp: i64) -> Bool {
    (unsafe { crate::VeilUserPost(0, msg, wp, lp as u64, tid) } == 0) as Bool
}

#[no_mangle]
pub extern "system" fn PostThreadMessageA(tid: u32, msg: u32, wp: u64, lp: i64) -> Bool {
    PostThreadMessageW(tid, msg, wp, lp)
}

#[no_mangle]
pub extern "system" fn PostQuitMessage(code: i32) {
    unsafe { crate::VeilUserPost(0, WM_QUIT, code as i64 as u64, 0, 0) };
}

/// Wake thread `tid` if it's waiting for messages (another thread changed
/// what it should paint).
pub fn wake(tid: u32) {
    if tid != unsafe { crate::GetCurrentThreadId() } {
        unsafe { crate::VeilUserPost(0, VEIL_WAKE, 0, 0, tid) };
    }
}

// -----| the queue |-----

/// Alt went down with no other key since (for the menu bar).
static ALT_ALONE: crate::dllrt::Lock<bool> = crate::dllrt::Lock::new(false);

static LAST: crate::dllrt::Lock<(u32, i32, i32)> = crate::dllrt::Lock::new((0, 0, 0));

fn kernel_get(m: &mut Msg, timeout: u32, flags: u32, hwnd: Hwnd, min: u32, max: u32) -> bool {
    // Filtering by window works for kernel (top-level) windows only.
    let hwnd = match state().get(hwnd) {
        Some(w) if w.top_level() => hwnd,
        _ => 0,
    };
    unsafe { crate::VeilUserGetMessage(m, timeout, flags, hwnd, min, max) == 0 }
}

/// Handle the kernel's private messages; true if `m` was one.
fn private(m: &Msg) -> bool {
    match m.message {
        VEIL_WAKE => true,
        VEIL_MOUSE_LEFT => {
            mouse_gone(0);
            true
        }
        VEIL_GEOMETRY => {
            crate::window::geometry_changed(m.hwnd);
            true
        }
        _ => false,
    }
}

const WM_MOUSEWHEEL: u32 = 0x020A;
const WM_MOUSEMOVE: u32 = 0x0200;
const WM_MOUSELEAVE: u32 = 0x02A3;
const WM_MOUSEHOVER: u32 = 0x02A1;

/// The last click: (window, message, time, x, y).
static LAST_CLICK: crate::dllrt::Lock<(Hwnd, u32, u32, i32, i32)> = crate::dllrt::Lock::new((0, 0, 0, 0, 0));

/// A second click soon, in the same place, on a window of a CS_DBLCLKS class
/// is a double click. (GetDoubleClickTime: 500 ms; SM_CXDOUBLECLK: 4.)
fn double_click(m: &mut Msg, x: i32, y: i32) {
    if !matches!(m.message, 0x0201 | 0x0204 | 0x0207) {
        return;
    }
    let dblclks = {
        let s = state();
        s.get(m.hwnd).is_some_and(|w| s.classes[w.class].style & 0x8 != 0)
    };
    let mut last = LAST_CLICK.lock();
    let again = dblclks && last.0 == m.hwnd && last.1 == m.message && m.time.wrapping_sub(last.2) <= 500 && (x - last.3).abs() <= 2 && (y - last.4).abs() <= 2;
    if again {
        m.message += 2;
        *last = (0, 0, 0, 0, 0);
    } else {
        *last = (m.hwnd, m.message, m.time, x, y);
    }
}

/// TrackMouseEvent's request: (window, flags, hover time, when the pointer last moved).
static TRACKED: crate::dllrt::Lock<(Hwnd, u32, u32, u32)> = crate::dllrt::Lock::new((0, 0, 0, 0));
const TME_HOVER: u32 = 1;
const TME_LEAVE: u32 = 2;
const TME_QUERY: u32 = 0x4000_0000;
const TME_CANCEL: u32 = 0x8000_0000;

/// The pointer is now over `now` (0: none of this process's windows): the
/// tracked window hears if it left, and a hover starts over.
fn mouse_gone(now: Hwnd) {
    let tick = unsafe { GetTickCount() };
    let leave = {
        let mut t = TRACKED.lock();
        if t.0 != 0 && t.0 == now {
            t.3 = tick;
        }
        if t.0 != 0 && t.0 != now && t.1 & TME_LEAVE != 0 {
            let h = t.0;
            t.1 &= !TME_LEAVE;
            if t.1 == 0 {
                t.0 = 0;
            }
            h
        } else {
            0
        }
    };
    if leave != 0 {
        PostMessageW(leave, WM_MOUSELEAVE, 0, 0);
    }
}

/// Input for a top-level window goes to the child under the mouse, or to
/// the window with the keyboard focus.
/// Input for a top-level window (mouse positions in its pixels) goes to
/// the child under the mouse, as a non-client message over a window's
/// non-client part (its scroll bars), or to the window with the keyboard
/// focus.
fn route(m: &mut Msg) {
    if (WM_MOUSEFIRST..=WM_MOUSELAST).contains(&m.message) {
        let (sx, sy) = ((m.lparam & 0xFFFF) as i16 as i32, ((m.lparam >> 16) & 0xFFFF) as i16 as i32);
        let (target, cx, cy, inside) = {
            let s = state();
            let Some(root) = s.get(m.hwnd) else { return };
            let i = root.inset();
            let (rx, ry) = (sx - i[0], sy - i[1]);
            let in_root = rx >= 0 && ry >= 0 && rx < root.client_w() && ry < root.client_h();
            let target = if s.capture != 0 && s.root(s.capture) == m.hwnd {
                s.capture
            } else if in_root {
                crate::window::child_at(&s, m.hwnd, rx, ry)
            } else {
                m.hwnd
            };
            let (ox, oy) = s.client_origin(target);
            let (cx, cy) = (sx - ox, sy - oy);
            let w = s.get(target).unwrap();
            let inside = s.capture == target || (cx >= 0 && cy >= 0 && cx < w.client_w() && cy < w.client_h());
            (target, cx, cy, inside)
        };
        m.hwnd = target;
        // The wheel says where on the screen, and goes to the window under it.
        if m.message == WM_MOUSEWHEEL {
            m.lparam = ((m.pt_y as u16 as u64) << 16) | m.pt_x as u16 as u64;
            return;
        }
        if m.message == WM_MOUSEMOVE {
            mouse_gone(target);
        }
        if inside {
            m.lparam = ((cy as u16 as u64) << 16) | cx as u16 as u64;
            double_click(m, cx, cy);
        } else {
            // WM_NC*: the hit-test code, and the screen position.
            m.message -= 0x160;
            m.wparam = crate::window::nc_hit(target, m.pt_x, m.pt_y);
            m.lparam = ((m.pt_y as u16 as u64) << 16) | m.pt_x as u16 as u64;
        }
    } else if (WM_KEYDOWN..=0x0109).contains(&m.message) {
        let s = state();
        let tid = unsafe { crate::GetCurrentThreadId() };
        if s.get(s.focus).is_some_and(|w| w.tid == tid) {
            m.hwnd = s.focus;
        }
    }
}

/// The next window of this thread that needs painting (parents first).
fn paint_due(hwnd: Hwnd) -> Option<Hwnd> {
    let tid = unsafe { crate::GetCurrentThreadId() };
    let s = state();
    s.windows
        .iter()
        .find(|w| w.tid == tid && w.update.is_some() && !w.destroying && (hwnd == 0 || w.hwnd == hwnd || s.root(w.hwnd) == hwnd))
        .map(|w| w.hwnd)
}

/// Any of this thread's windows still to paint.
pub fn paint_pending() -> bool {
    paint_due(0).is_some()
}

fn in_range(msg: u32, min: u32, max: u32) -> bool {
    (min == 0 && max == 0) || (msg >= min && msg <= max)
}

/// The next message, waiting for one if `wait`. False if there was none.
fn next(out: *mut Msg, hwnd: Hwnd, min: u32, max: u32, remove: bool, wait: bool) -> bool {
    let mut m = Msg::default();
    loop {
        run_win_events();
        // The kernel's private messages first, whatever the filter.
        while kernel_get(&mut m, 0, 1 | 2, 0, VEIL_WAKE, VEIL_MOUSE_LEFT) {
            private(&m);
        }
        let flags = if remove { 1 } else { 0 };
        // Posted messages and input.
        if kernel_get(&mut m, 0, flags | 2, hwnd, min, max) {
            if private(&m) {
                continue;
            }
            if !typed(&mut m, remove) {
                continue;
            }
            // A steady stream of messages still shows progress.
            crate::paint::present_if_due();
            return deliver(out, m);
        }
        // Painting.
        if in_range(WM_PAINT, min, max) {
            if let Some(h) = paint_due(hwnd) {
                m = Msg { hwnd: h, message: WM_PAINT, ..Msg::default() };
                return deliver(out, m);
            }
        }
        // Timers.
        if in_range(WM_TIMER, min, max) && kernel_get(&mut m, 0, flags, hwnd, WM_TIMER, WM_TIMER) {
            crate::paint::present_if_due();
            return deliver(out, m);
        }
        // Nothing left to do: what's drawn is finished, show it. All of it at
        // once, so a window never appears half painted.
        if wait || !paint_pending() {
            crate::paint::present_all();
        }
        if !wait {
            return false;
        }
        // Wait for anything (or a hover coming due), then look again.
        kernel_get(&mut m, check_hover(), 0, 0, 0, 0);
    }
}

fn deliver(out: *mut Msg, mut m: Msg) -> bool {
    route(&mut m);
    crate::keyboard::track(&m);
    if m.message == WM_TIMER {
        let s = state();
        if let Some(t) = s.timer_procs.iter().find(|t| t.0 == m.hwnd && t.1 == m.wparam) {
            m.lparam = t.2;
        }
    }
    *LAST.lock() = (m.time, m.pt_x, m.pt_y);
    if !out.is_null() {
        unsafe { *out = m };
    }
    true
}

fn get(out: *mut Msg, hwnd: Hwnd, min: u32, max: u32) -> Bool {
    next(out, hwnd, min, max, true, true);
    if unsafe { (*out).message } == WM_QUIT { 0 } else { 1 }
}

#[no_mangle]
pub extern "system" fn GetMessageW(out: *mut Msg, hwnd: Hwnd, min: u32, max: u32) -> Bool {
    get(out, hwnd, min, max)
}

#[no_mangle]
pub extern "system" fn GetMessageA(out: *mut Msg, hwnd: Hwnd, min: u32, max: u32) -> Bool {
    get(out, hwnd, min, max)
}

#[no_mangle]
pub extern "system" fn PeekMessageW(out: *mut Msg, hwnd: Hwnd, min: u32, max: u32, flags: u32) -> Bool {
    next(out, hwnd, min, max, flags & PM_REMOVE != 0, false) as Bool
}

#[no_mangle]
pub extern "system" fn PeekMessageA(out: *mut Msg, hwnd: Hwnd, min: u32, max: u32, flags: u32) -> Bool {
    PeekMessageW(out, hwnd, min, max, flags)
}

#[no_mangle]
pub extern "system" fn WaitMessage() -> Bool {
    if !next(core::ptr::null_mut(), 0, 0, 0, false, false) {
        let mut m = Msg::default();
        kernel_get(&mut m, u32::MAX, 0, 0, 0, 0);
    }
    1
}

#[no_mangle]
pub extern "system" fn GetMessageTime() -> i32 {
    LAST.lock().0 as i32
}

#[no_mangle]
pub extern "system" fn GetMessagePos() -> u32 {
    let l = LAST.lock();
    (l.2 as u16 as u32) << 16 | l.1 as u16 as u32
}

/// Threads whose last key-down wasn't passed to TranslateMessage (a dialog
/// or an accelerator took it).
static UNTRANSLATED: crate::dllrt::Lock<Vec<u32>> = crate::dllrt::Lock::new(Vec::new());

/// The desktop sends a key's character right after its key-down; as on
/// Windows, the character only arrives if the key-down was translated
/// (posted characters always do). False for a character to drop.
fn typed(m: &mut Msg, remove: bool) -> bool {
    let tid = unsafe { crate::GetCurrentThreadId() };
    match m.message {
        WM_KEYDOWN | WM_SYSKEYDOWN if remove => {
            let mut u = UNTRANSLATED.lock();
            if !u.contains(&tid) {
                u.push(tid);
            }
            true
        }
        0x0102 | 0x0106 if m.lparam & nt_abi::CHAR_TYPED != 0 => {
            m.lparam &= !nt_abi::CHAR_TYPED;
            !(remove && UNTRANSLATED.lock().contains(&tid))
        }
        _ => true,
    }
}

/// The desktop already sends WM_CHAR after each character's key-down.
#[no_mangle]
pub extern "system" fn TranslateMessage(m: *const Msg) -> Bool {
    if m.is_null() {
        return 0;
    }
    let message = unsafe { (*m).message };
    if matches!(message, WM_KEYDOWN | WM_SYSKEYDOWN) {
        let tid = unsafe { crate::GetCurrentThreadId() };
        UNTRANSLATED.lock().retain(|&t| t != tid);
    }
    matches!(message, WM_KEYDOWN | WM_KEYUP | WM_SYSKEYDOWN | 0x0105) as Bool
}

fn dispatch(m: *const Msg, unicode: bool) -> i64 {
    if m.is_null() {
        return 0;
    }
    let m = unsafe { *m };
    if m.message == WM_TIMER && m.lparam != 0 {
        let f: unsafe extern "system" fn(Hwnd, u32, u64, u32) = unsafe { core::mem::transmute(m.lparam as usize) };
        unsafe { f(m.hwnd, WM_TIMER, m.wparam, m.time) };
        return 0;
    }
    call(m.hwnd, m.message, m.wparam, m.lparam as i64, unicode)
}

#[no_mangle]
pub extern "system" fn DispatchMessageW(m: *const Msg) -> i64 {
    dispatch(m, true)
}

#[no_mangle]
pub extern "system" fn DispatchMessageA(m: *const Msg) -> i64 {
    dispatch(m, false)
}

/// Input (or an object) to wake for: wait in small steps, looking at both.
#[no_mangle]
pub unsafe extern "system" fn MsgWaitForMultipleObjectsEx(n: u32, handles: *const u64, ms: u32, _mask: u32, flags: u32) -> u32 {
    const WAIT_TIMEOUT: u32 = 258;
    let wait_all = (flags & 1 != 0) as Bool; // MWMO_WAITALL
    let start = unsafe { GetTickCount() };
    loop {
        let mut m = Msg::default();
        if next(&mut m, 0, 0, 0, false, false) {
            return n;
        }
        if n > 0 {
            let r = unsafe { WaitForMultipleObjects(n, handles, wait_all, 0) };
            if r != WAIT_TIMEOUT {
                return r;
            }
        }
        if ms != u32::MAX && unsafe { GetTickCount() }.wrapping_sub(start) >= ms {
            return WAIT_TIMEOUT;
        }
        unsafe { Sleep(5) };
    }
}

#[no_mangle]
pub unsafe extern "system" fn MsgWaitForMultipleObjects(n: u32, handles: *const u64, all: Bool, ms: u32, mask: u32) -> u32 {
    unsafe { MsgWaitForMultipleObjectsEx(n, handles, ms, mask, all as u32 & 1) }
}

#[link(name = "kernel32", kind = "raw-dylib")]
unsafe extern "system" {
    fn GetTickCount() -> u32;
    fn Sleep(ms: u32);
    fn WaitForMultipleObjects(n: u32, handles: *const u64, all: Bool, ms: u32) -> u32;
}

// -----| WinEvent hooks |-----

/// A WinEvent hook in this process: its handle, event range, procedure,
/// the thread to run it on (the one that set it), and the thread filter.
struct WinEventHook {
    handle: u64,
    min: u32,
    max: u32,
    proc: usize,
    owner_tid: u32,
    tid: u32,
    flags: u32,
}

/// An event waiting for its hook's thread: (hook, event, hwnd, object,
/// child, the thread that raised it).
type Pending = (u64, u32, Hwnd, i32, i32, u32);

static WIN_EVENT_HOOKS: crate::dllrt::Lock<Vec<WinEventHook>> = crate::dllrt::Lock::new(Vec::new());
static WIN_EVENTS: crate::dllrt::Lock<Vec<Pending>> = crate::dllrt::Lock::new(Vec::new());
static NEXT_HOOK: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0x0007_0001);

const WINEVENT_SKIPOWNTHREAD: u32 = 0x1;
const WINEVENT_SKIPOWNPROCESS: u32 = 0x2;

/// Hooks see this process's events only (another process's hook would
/// need its own copy of user32 to run in), all out of context: each runs
/// on the thread that set it, when that thread next looks for a message.
#[no_mangle]
pub extern "system" fn SetWinEventHook(min: u32, max: u32, _module: u64, proc: usize, pid: u32, tid: u32, flags: u32) -> u64 {
    let me = unsafe { crate::GetCurrentProcessId() };
    if proc == 0 || min > max || (pid != 0 && pid != me) || flags & WINEVENT_SKIPOWNPROCESS != 0 {
        // Nothing it could see: a handle all the same.
        return if proc == 0 { 0 } else { NEXT_HOOK.fetch_add(1, core::sync::atomic::Ordering::Relaxed) };
    }
    let handle = NEXT_HOOK.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    let owner_tid = unsafe { crate::GetCurrentThreadId() };
    WIN_EVENT_HOOKS.lock().push(WinEventHook { handle, min, max, proc, owner_tid, tid, flags });
    handle
}

#[no_mangle]
pub extern "system" fn UnhookWinEvent(hook: u64) -> Bool {
    let mut hooks = WIN_EVENT_HOOKS.lock();
    let before = hooks.len();
    hooks.retain(|h| h.handle != hook);
    WIN_EVENTS.lock().retain(|e| e.0 != hook);
    (hooks.len() != before || hook != 0) as Bool
}

/// Queues `event` for the hooks that want it (a repeat of an event still
/// waiting is dropped).
pub(crate) fn win_event(event: u32, hwnd: Hwnd, object: i32, child: i32) {
    let hooks = WIN_EVENT_HOOKS.lock();
    if hooks.is_empty() {
        return;
    }
    let tid = unsafe { crate::GetCurrentThreadId() };
    let mut pending = WIN_EVENTS.lock();
    for h in hooks.iter().filter(|h| (h.min..=h.max).contains(&event)) {
        if (h.tid != 0 && h.tid != tid) || (h.flags & WINEVENT_SKIPOWNTHREAD != 0 && h.owner_tid == tid) {
            continue;
        }
        let e = (h.handle, event, hwnd, object, child, tid);
        if !pending.contains(&e) {
            pending.push(e);
        }
    }
}

/// Runs this thread's waiting events.
fn run_win_events() {
    let tid = unsafe { crate::GetCurrentThreadId() };
    loop {
        let next = {
            let hooks = WIN_EVENT_HOOKS.lock();
            let mut pending = WIN_EVENTS.lock();
            let at = pending.iter().position(|e| hooks.iter().any(|h| h.handle == e.0 && h.owner_tid == tid));
            at.map(|i| {
                let e = pending.remove(i);
                (hooks.iter().find(|h| h.handle == e.0).map_or(0, |h| h.proc), e)
            })
        };
        let Some((proc, (hook, event, hwnd, object, child, from))) = next else { return };
        if proc == 0 {
            continue;
        }
        let f: unsafe extern "system" fn(u64, u32, Hwnd, i32, i32, u32, u32) = unsafe { core::mem::transmute(proc) };
        let time = LAST.lock().0;
        unsafe { f(hook, event, hwnd, object, child, from, time) };
    }
}

#[no_mangle]
pub extern "system" fn NotifyWinEvent(event: u32, hwnd: Hwnd, object: i32, child: i32) {
    win_event(event, hwnd, object, child);
}

/// TRACKMOUSEEVENT: size, flags, window, hover time.
#[no_mangle]
pub unsafe extern "system" fn TrackMouseEvent(tme: *mut u8) -> Bool {
    if tme.is_null() {
        return 0;
    }
    let (flags, hwnd, hover) = unsafe {
        (*(tme.add(4) as *const u32), core::ptr::read_unaligned(tme.add(8) as *const u64), *(tme.add(16) as *const u32))
    };
    if flags & TME_QUERY != 0 {
        let t = TRACKED.lock();
        unsafe {
            *(tme.add(4) as *mut u32) = t.1;
            core::ptr::write_unaligned(tme.add(8) as *mut u64, t.0);
            *(tme.add(16) as *mut u32) = t.2;
        }
        return 1;
    }
    let mut t = TRACKED.lock();
    if flags & TME_CANCEL != 0 {
        if t.0 == hwnd {
            t.1 &= !(flags & 3);
            if t.1 == 0 {
                t.0 = 0;
            }
        }
        return 1;
    }
    // HOVER_DEFAULT: SPI_GETMOUSEHOVERTIME's 400 ms.
    let time = if hover == 0xFFFF_FFFF || hover == 0 { 400 } else { hover };
    *t = (hwnd, flags & 3, time, unsafe { GetTickCount() });
    1
}

/// A due WM_MOUSEHOVER, and how long until one is due otherwise.
fn check_hover() -> u32 {
    let (hwnd, due) = {
        let mut t = TRACKED.lock();
        if t.0 == 0 || t.1 & TME_HOVER == 0 {
            return u32::MAX;
        }
        let waited = unsafe { GetTickCount() }.wrapping_sub(t.3);
        if waited < t.2 {
            return t.2 - waited;
        }
        t.1 &= !TME_HOVER;
        let h = t.0;
        if t.1 == 0 {
            t.0 = 0;
        }
        (h, 0)
    };
    PostMessageW(hwnd, WM_MOUSEHOVER, 0, 0);
    due
}

#[no_mangle]
pub extern "system" fn SetWindowsHookExW(_kind: i32, _proc: usize, _module: u64, _tid: u32) -> u64 {
    0x0007_0002
}

#[no_mangle]
pub extern "system" fn SetWindowsHookExA(kind: i32, proc: usize, module: u64, tid: u32) -> u64 {
    SetWindowsHookExW(kind, proc, module, tid)
}

#[no_mangle]
pub extern "system" fn UnhookWindowsHookEx(_hook: u64) -> Bool {
    1
}

#[no_mangle]
pub extern "system" fn CallNextHookEx(_hook: u64, _code: i32, _wp: u64, _lp: i64) -> i64 {
    0
}

// -----| timers |-----

#[no_mangle]
pub extern "system" fn SetTimer(hwnd: Hwnd, id: u64, ms: u32, proc: u64) -> u64 {
    let mut s = state();
    if hwnd != 0 && s.get(hwnd).is_none() {
        crate::set_error(crate::ERROR_INVALID_WINDOW_HANDLE);
        return 0;
    }
    let id = if hwnd == 0 {
        s.next_timer += 1;
        s.next_timer
    } else {
        id
    };
    s.timer_procs.retain(|t| !(t.0 == hwnd && t.1 == id));
    if proc != 0 {
        s.timer_procs.push((hwnd, id, proc));
    }
    drop(s);
    unsafe { crate::VeilUserTimer(hwnd, id, ms.max(1)) };
    if id == 0 { 1 } else { id }
}

#[no_mangle]
pub extern "system" fn KillTimer(hwnd: Hwnd, id: u64) -> Bool {
    state().timer_procs.retain(|t| !(t.0 == hwnd && t.1 == id));
    (unsafe { crate::VeilUserTimer(hwnd, id, 0) } == 0) as Bool
}

/// A destroyed window's timers.
pub fn kill_timers(hwnd: Hwnd, ids: &[u64]) {
    for &id in ids {
        unsafe { crate::VeilUserTimer(hwnd, id, 0) };
    }
}

// -----| DefWindowProc |-----

const SWP_NOCLIENTSIZE: u32 = 0x0800;
const SWP_NOCLIENTMOVE: u32 = 0x1000;

pub fn def_window_proc(hwnd: Hwnd, msg: u32, wp: u64, lp: i64, unicode: bool) -> i64 {
    match msg {
        WM_NCCREATE => {
            if lp != 0 {
                let name = unsafe { *((lp as usize + 56) as *const u64) };
                if name >> 16 != 0 {
                    let text = if unicode { unsafe { crate::wstr(name as *const u16) } } else { unsafe { crate::from_ansi(name as *const u8, -1) } };
                    crate::window::set_text(hwnd, text);
                }
            }
            1
        }
        WM_NCCALCSIZE => {
            // The first RECT (alone, or NCCALCSIZE_PARAMS' first) becomes
            // the client rectangle.
            if lp != 0 {
                let r = lp as *mut [i32; 4];
                if let Some(w) = state().get(hwnd) {
                    let m = crate::window::nc_margins(w.style, w.ex_style, w.parent == 0, w.parent == 0 && w.menu != 0);
                    unsafe {
                        (*r)[0] += m[0];
                        (*r)[1] += m[1];
                        (*r)[2] -= m[2];
                        (*r)[3] -= m[3];
                    }
                }
            }
            0
        }
        WM_SETTEXT => {
            let text = if lp == 0 { Vec::new() } else if unicode { unsafe { crate::wstr(lp as *const u16) } } else { unsafe { crate::from_ansi(lp as *const u8, -1) } };
            crate::window::set_text(hwnd, text);
            1
        }
        WM_GETTEXT => {
            let text = state().get(hwnd).map(|w| w.text.clone()).unwrap_or_default();
            if unicode {
                unsafe { crate::copy_out(&text, lp as *mut u16, wp as usize) as i64 }
            } else {
                unsafe { crate::copy_out(&crate::to_ansi(&text), lp as *mut u8, wp as usize) as i64 }
            }
        }
        WM_GETTEXTLENGTH => {
            let text = state().get(hwnd).map(|w| w.text.clone()).unwrap_or_default();
            if unicode { text.len() as i64 } else { crate::to_ansi(&text).len() as i64 }
        }
        WM_CLOSE => {
            crate::window::DestroyWindow(hwnd);
            0
        }
        WM_ERASEBKGND => crate::paint::erase_background(hwnd, wp) as i64,
        // A wheel nobody handled goes up to the parent.
        WM_MOUSEWHEEL | 0x020E => {
            let parent = state().get(hwnd).map_or(0, |w| w.parent);
            if parent != 0 { send(parent, msg, wp, lp) } else { 0 }
        }
        // WM_NOTIFYFORMAT, NF_QUERY: NFR_UNICODE or NFR_ANSI, as the window is.
        0x0055 if lp == 3 => if crate::window::IsWindowUnicode(hwnd) != 0 { 2 } else { 1 },
        WM_PAINT => {
            let mut ps = [0u8; 72];
            crate::paint::BeginPaint(hwnd, ps.as_mut_ptr());
            crate::paint::EndPaint(hwnd, ps.as_ptr());
            0
        }
        WM_NCPAINT => {
            crate::paint::paint_frame(hwnd);
            0
        }
        WM_WINDOWPOSCHANGED => {
            let flags = unsafe { *((lp as usize + 32) as *const u32) };
            let info = state().get(hwnd).map(|w| {
                let kind = if w.minimized { 1 } else if w.maximized { 2 } else { 0 };
                (kind, w.client_w(), w.client_h(), w.x + w.margins[0], w.y + w.margins[1])
            });
            if let Some((kind, cw, ch, cx, cy)) = info {
                if flags & SWP_NOCLIENTSIZE == 0 {
                    send(hwnd, WM_SIZE, kind, ((ch as u16 as i64) << 16) | cw as u16 as i64);
                }
                if flags & SWP_NOCLIENTMOVE == 0 {
                    send(hwnd, WM_MOVE, 0, ((cy as u16 as i64) << 16) | cx as u16 as i64);
                }
            }
            0
        }
        WM_SYSCOMMAND => {
            match wp as u32 & 0xFFF0 {
                0xF060 => {
                    send(hwnd, WM_CLOSE, 0, 0);
                }
                0xF020 => {
                    crate::window::ShowWindow(hwnd, 6);
                }
                0xF030 => {
                    crate::window::ShowWindow(hwnd, 3);
                }
                0xF120 => {
                    crate::window::ShowWindow(hwnd, 9);
                }
                // SC_KEYMENU: lParam the key.
                0xF100 => crate::menu::key_menu(hwnd, lp as u16),
                _ => {}
            }
            0
        }
        WM_ACTIVATE => {
            // The focus comes to the window unless it's inside it already.
            if wp & 0xFFFF != 0 && wp >> 16 == 0 {
                let inside = {
                    let s = state();
                    let mut h = s.focus;
                    loop {
                        if h == hwnd {
                            break true;
                        }
                        match s.get(h) {
                            Some(w) if w.parent != 0 => h = w.parent,
                            _ => break false,
                        }
                    }
                };
                if !inside {
                    crate::window::SetFocus(hwnd);
                }
            }
            0
        }
        0x0132..=0x0138 => crate::controls::default_ctl_color(wp, msg) as i64,
        WM_NCHITTEST => {
            let (x, y) = ((lp & 0xFFFF) as i16 as i32, ((lp >> 16) & 0xFFFF) as i16 as i32);
            crate::window::nc_hit(hwnd, x, y) as i64
        }
        // A click on the menu bar.
        0x00A1 if wp == 5 => {
            let (x, y) = ((lp & 0xFFFF) as i16 as i32, ((lp >> 16) & 0xFFFF) as i16 as i32);
            crate::menu::bar_click(hwnd, x, y);
            0
        }
        // Alt+key: the menu with that mnemonic (Alt+Space: the system
        // menu), from the top-level window.
        0x0106 => {
            let root = state().root(hwnd);
            if wp == 0x0D {
                return 0;
            }
            if root != 0 {
                send(root, WM_SYSCOMMAND, 0xF100, wp as u16 as i64);
            }
            0
        }
        // Alt alone, F10: the menu bar from the keyboard; Alt+F4: close.
        WM_SYSKEYDOWN => {
            let mut alone = ALT_ALONE.lock();
            if wp == 0x12 {
                *alone = lp & (1 << 30) == 0 || *alone;
            } else {
                *alone = false;
            }
            drop(alone);
            if wp == 0x73 && lp & (1 << 29) != 0 {
                let root = state().root(hwnd);
                send(root, WM_SYSCOMMAND, 0xF060, 0);
            }
            0
        }
        0x0105 => {
            let alone = core::mem::replace(&mut *ALT_ALONE.lock(), false);
            if (wp == 0x12 && alone) || wp == 0x79 {
                let root = state().root(hwnd);
                send(root, WM_SYSCOMMAND, 0xF100, 0);
            }
            0
        }
        WM_KEYDOWN => {
            *ALT_ALONE.lock() = false;
            0
        }
        // A click on a standard scroll bar.
        0x00A1 if wp == crate::scroll::HTVSCROLL || wp == crate::scroll::HTHSCROLL => {
            let (x, y) = ((lp & 0xFFFF) as i16 as i32, ((lp >> 16) & 0xFFFF) as i16 as i32);
            crate::scroll::nc_click(hwnd, wp == crate::scroll::HTVSCROLL, x, y);
            0
        }
        WM_MOUSEACTIVATE => 1,
        WM_NCACTIVATE => 1,
        0x0011 => 1, // WM_QUERYENDSESSION
        0x0013 => 1, // WM_QUERYOPEN
        _ => 0,
    }
}

#[no_mangle]
pub extern "system" fn DefWindowProcW(hwnd: Hwnd, msg: u32, wp: u64, lp: i64) -> i64 {
    def_window_proc(hwnd, msg, wp, lp, true)
}

#[no_mangle]
pub extern "system" fn DefWindowProcA(hwnd: Hwnd, msg: u32, wp: u64, lp: i64) -> i64 {
    def_window_proc(hwnd, msg, wp, lp, false)
}
