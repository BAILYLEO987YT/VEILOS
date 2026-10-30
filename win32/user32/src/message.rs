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
        _ => unsafe { f(hwnd, msg, wp, lp) },
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
        VEIL_GEOMETRY => {
            crate::window::geometry_changed(m.hwnd);
            true
        }
        _ => false,
    }
}

/// Input for a top-level window goes to the child under the mouse, or to
/// the window with the keyboard focus.
fn route(m: &mut Msg) {
    if (WM_MOUSEFIRST..=WM_MOUSELAST).contains(&m.message) {
        let s = state();
        let target = if s.capture != 0 && s.root(s.capture) == m.hwnd { s.capture } else {
            let (x, y) = ((m.lparam & 0xFFFF) as i16 as i32, ((m.lparam >> 16) & 0xFFFF) as i16 as i32);
            crate::window::child_at(&s, m.hwnd, x, y)
        };
        if target != m.hwnd {
            let (ox, oy) = s.client_origin(target);
            let (x, y) = ((m.lparam & 0xFFFF) as i16 as i32 - ox, ((m.lparam >> 16) & 0xFFFF) as i16 as i32 - oy);
            m.lparam = ((y as u16 as u64) << 16) | x as u16 as u64;
            m.hwnd = target;
        }
    } else if (WM_KEYDOWN..=0x0109).contains(&m.message) {
        let s = state();
        if s.focus != 0 && s.root(s.focus) == m.hwnd {
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

fn in_range(msg: u32, min: u32, max: u32) -> bool {
    (min == 0 && max == 0) || (msg >= min && msg <= max)
}

/// The next message, waiting for one if `wait`. False if there was none.
fn next(out: *mut Msg, hwnd: Hwnd, min: u32, max: u32, remove: bool, wait: bool) -> bool {
    let mut m = Msg::default();
    loop {
        crate::paint::present_all();
        // The kernel's private messages first, whatever the filter.
        while kernel_get(&mut m, 0, 1 | 2, 0, VEIL_WAKE, VEIL_GEOMETRY) {
            private(&m);
        }
        let flags = if remove { 1 } else { 0 };
        // Posted messages and input.
        if kernel_get(&mut m, 0, flags | 2, hwnd, min, max) {
            if private(&m) {
                continue;
            }
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
            return deliver(out, m);
        }
        if !wait {
            return false;
        }
        // Wait for anything, then look again.
        kernel_get(&mut m, u32::MAX, 0, 0, 0, 0);
    }
}

fn deliver(out: *mut Msg, mut m: Msg) -> bool {
    route(&mut m);
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

/// The desktop already sends WM_CHAR after each character's key-down.
#[no_mangle]
pub extern "system" fn TranslateMessage(m: *const Msg) -> Bool {
    if m.is_null() {
        return 0;
    }
    matches!(unsafe { (*m).message }, WM_KEYDOWN | WM_KEYUP | WM_SYSKEYDOWN | 0x0105) as Bool
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

/// Dialogs and accelerators come with G3.
#[no_mangle]
pub extern "system" fn IsDialogMessageW(_dlg: Hwnd, _m: *const Msg) -> Bool {
    0
}

#[no_mangle]
pub extern "system" fn IsDialogMessageA(_dlg: Hwnd, _m: *const Msg) -> Bool {
    0
}

#[no_mangle]
pub extern "system" fn TranslateAcceleratorW(_hwnd: Hwnd, _accel: u64, _m: *const Msg) -> i32 {
    0
}

#[no_mangle]
pub extern "system" fn TranslateAcceleratorA(_hwnd: Hwnd, _accel: u64, _m: *const Msg) -> i32 {
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
                    let m = crate::window::frame_margins(w.style, w.ex_style, w.parent == 0);
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
                _ => {}
            }
            0
        }
        WM_ACTIVATE => {
            if wp & 0xFFFF != 0 && wp >> 16 == 0 {
                crate::window::SetFocus(hwnd);
            }
            0
        }
        WM_NCHITTEST => 1,
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
