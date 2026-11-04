// =============================================================================
// VEILOS: windows for Windows programs (WIN32_DESIGN.md, G1)
// =============================================================================
//
// The kernel half of user32. A Windows program's top-level window is a
// desktop window whose content is a pixel surface: user32 and gdi32 draw
// in user mode and present finished pixels here, and the desktop copies
// them to the screen when it paints. Child windows (controls) exist only in
// user32, drawn into their top-level window's pixels.
//
// HWNDs are global, as on Windows. Each thread that owns windows has a
// message queue here, fed by the desktop (clicks, keys, closing, moving and
// resizing) and by PostMessage from any thread; timers add WM_TIMER when
// the queue is otherwise empty. user32 reads the queue with
// VEIL_USER_GET_MESSAGE, which waits (giving the CPU away) when it's empty.
//
// The desktop reports each window's content rectangle as it paints it
// (`set_content_rect`); a change is reported to user32 (VEIL_GEOMETRY), and a new
// size gives the window a new surface for user32 to fill.

use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, Ordering};
use nt_abi::{status, Msg, NtStatus, WindowParams, WindowPlace};
use spin::Mutex;

use crate::interrupts::syscall_stack_arg as stack;
use crate::nt_file::ptr_ok;

pub const WM_CLOSE: u32 = 0x0010;
pub const WM_TIMER: u32 = 0x0113;
pub const WM_KEYDOWN: u32 = 0x0100;
pub const WM_KEYUP: u32 = 0x0101;
pub const WM_CHAR: u32 = 0x0102;
pub const WM_SYSKEYDOWN: u32 = 0x0104;
pub const WM_SYSKEYUP: u32 = 0x0105;
pub const WM_SYSCHAR: u32 = 0x0106;
pub const WM_MOUSEMOVE: u32 = 0x0200;
pub const WM_LBUTTONDOWN: u32 = 0x0201;
pub const WM_LBUTTONUP: u32 = 0x0202;
pub const WM_RBUTTONDOWN: u32 = 0x0204;
pub const WM_RBUTTONUP: u32 = 0x0205;

/// A window's client-area pixels (0x00RRGGBB, `w` per row).
pub struct Surface {
    pub w: u32,
    pub h: u32,
    pub pixels: Vec<u32>,
}

impl Surface {
    fn new(w: u32, h: u32) -> Self {
        Surface { w, h, pixels: alloc::vec![0x00FF_FFFF; w as usize * h as usize] }
    }

    /// Change size keeping the picture, so a resize doesn't blank the window
    /// until the program has repainted.
    fn resize(&mut self, w: u32, h: u32) {
        let mut new = Surface::new(w, h);
        let (cw, ch) = (self.w.min(w) as usize, self.h.min(h) as usize);
        for y in 0..ch {
            let from = y * self.w as usize;
            new.pixels[y * w as usize..][..cw].copy_from_slice(&self.pixels[from..from + cw]);
        }
        *self = new;
    }
}

pub struct UserWindow {
    pub hwnd: u32,
    pub pid: u32,
    /// The thread whose queue gets its messages.
    pub tid: u32,
    /// Its desktop window.
    pub vid: u32,
    pub surface: Mutex<Surface>,
    /// The content rectangle as the desktop last painted it.
    content: [AtomicI32; 4],
    /// ... and as last reported to user32.
    reported: Mutex<WindowPlace>,
    pub alive: AtomicBool,
    /// No desktop frame (WINDOW_POPUP).
    pub popup: bool,
}

static WINDOWS: Mutex<Vec<Arc<UserWindow>>> = Mutex::new(Vec::new());
static NEXT_HWND: AtomicU32 = AtomicU32::new(0x0001_0010);

fn by_hwnd(hwnd: u64) -> Option<Arc<UserWindow>> {
    WINDOWS.lock().iter().find(|w| w.hwnd as u64 == hwnd).cloned()
}

fn by_vid(vid: u32) -> Option<Arc<UserWindow>> {
    WINDOWS.lock().iter().find(|w| w.vid == vid).cloned()
}

/// Is desktop window `vid` a Windows program's?
pub fn owns(vid: u32) -> bool {
    WINDOWS.lock().iter().any(|w| w.vid == vid)
}

// -----| message queues |-----

struct Timer {
    hwnd: u64,
    id: u64,
    interval: u64,
    due: u64,
}

struct Queue {
    tid: u32,
    messages: VecDeque<Msg>,
    timers: Vec<Timer>,
}

static QUEUES: Mutex<Vec<Queue>> = Mutex::new(Vec::new());

fn now() -> u64 {
    crate::interrupts::TICKS.load(Ordering::Relaxed)
}

fn with_queue<R>(tid: u32, f: impl FnOnce(&mut Queue) -> R) -> R {
    let mut queues = QUEUES.lock();
    if let Some(q) = queues.iter_mut().find(|q| q.tid == tid) {
        return f(q);
    }
    queues.push(Queue { tid, messages: VecDeque::new(), timers: Vec::new() });
    let q = queues.last_mut().unwrap();
    f(q)
}

fn cursor() -> (i32, i32) {
    let m = crate::task::mouse::MOUSE_STATE.lock();
    (m.x as i32, m.y as i32)
}

fn post_to(tid: u32, hwnd: u64, message: u32, wparam: u64, lparam: u64) {
    let (x, y) = cursor();
    let msg = Msg { hwnd, message, wparam, lparam, time: now() as u32, pt_x: x, pt_y: y, private: 0 };
    with_queue(tid, |q| {
        // Keep a runaway poster from eating the heap.
        if q.messages.len() < 10_000 {
            q.messages.push_back(msg);
        }
    });
}

/// The thread is gone: so is its queue.
pub fn thread_exited(tid: u32) {
    QUEUES.lock().retain(|q| q.tid != tid);
}

/// The process is gone: close its windows.
pub fn process_exited(pid: u32) {
    let gone: Vec<Arc<UserWindow>> = {
        let mut windows = WINDOWS.lock();
        let (gone, keep) = core::mem::take(&mut *windows).into_iter().partition(|w| w.pid == pid);
        *windows = keep;
        gone
    };
    for w in gone {
        w.alive.store(false, Ordering::Release);
        close_desktop_window(w.vid);
    }
}

fn close_desktop_window(vid: u32) {
    if let Some(mut wm) = crate::desktop::DESKTOP_WM.try_lock() {
        wm.close_silent(vid);
    } else {
        crate::desktop::schedule_close_silent(vid);
    }
    crate::apps::TERMINAL_OUTPUT.store(true, Ordering::Release);
}

// -----| from the desktop |-----

/// The desktop is painting window `vid`'s content at this rectangle.
pub fn set_content_rect(vid: u32, x: i32, y: i32, w: u32, h: u32) {
    if let Some(win) = WINDOWS.try_lock().and_then(|ws| ws.iter().find(|w| w.vid == vid).cloned()) {
        for (slot, v) in win.content.iter().zip([x, y, w as i32, h as i32]) {
            slot.store(v, Ordering::Relaxed);
        }
    }
}

/// Input for desktop window `vid` (mouse positions in screen coordinates,
/// packed y << 16 | x as the desktop does). True if it was a Windows
/// program's window.
pub fn deliver(vid: u32, message: u32, wparam: u64, lparam: u64) -> bool {
    let Some(win) = by_vid(vid) else { return false };
    let hwnd = win.hwnd as u64;
    match message {
        WM_LBUTTONDOWN | WM_LBUTTONUP | WM_RBUTTONDOWN | WM_RBUTTONUP | WM_MOUSEMOVE => {
            // Screen to client coordinates.
            let (sx, sy) = ((lparam & 0xFFFF) as i32, ((lparam >> 16) & 0xFFFF) as i32);
            let cx = sx - win.content[0].load(Ordering::Relaxed);
            let cy = sy - win.content[1].load(Ordering::Relaxed);
            let lp = ((cy as u16 as u64) << 16) | cx as u16 as u64;
            let buttons = match message {
                WM_LBUTTONDOWN => 0x1,
                WM_RBUTTONDOWN => 0x2,
                _ => 0,
            };
            post_to(win.tid, hwnd, message, buttons | modifier_keys(), lp);
            // The window has the mouse until the button comes up.
            let (up, held) = {
                let m = crate::task::mouse::MOUSE_STATE.lock();
                if message == WM_LBUTTONDOWN { (WM_LBUTTONUP, m.left_button) } else { (WM_RBUTTONUP, m.right_button) }
            };
            if message == WM_LBUTTONDOWN || message == WM_RBUTTONDOWN {
                if held {
                    TRACK.lock().capture = Some((vid, up));
                } else {
                    // Released before the click got here.
                    post_to(win.tid, hwnd, up, modifier_keys(), lp);
                }
            }
        }
        WM_CHAR => {
            // A typed character (its key went as WM_KEYDOWN/UP): with Ctrl a
            // control character, with Alt WM_SYSCHAR. Enter types CR.
            let mut ch = if wparam == 0x0A { 0x0D } else { wparam as u32 };
            let (ctrl, alt) = (key_down(VK_CONTROL), key_down(VK_MENU));
            if ctrl && (0x40..0x80).contains(&ch) {
                ch &= 0x1F;
            }
            let msg = if alt && !ctrl { WM_SYSCHAR } else { WM_CHAR };
            post_to(win.tid, hwnd, msg, ch as u64, 1 | nt_abi::CHAR_TYPED);
        }
        _ => post_to(win.tid, hwnd, message, wparam, lparam),
    }
    true
}

// -----| the mouse beyond clicks |-----

/// What the pointer is doing to Windows programs' windows.
struct MouseTrack {
    /// A window a button went down in, and the message when it comes up.
    capture: Option<(u32, u32)>,
    /// The window under the pointer.
    hover: u32,
    last: (i32, i32),
}

static TRACK: Mutex<MouseTrack> = Mutex::new(MouseTrack { capture: None, hover: 0, last: (-1, -1) });

const WM_MOUSEWHEEL: u32 = 0x020A;
/// The pointer left this window.
const VEIL_MOUSE_LEFT: u32 = 0x7FFF_0003;

fn modifier_keys() -> u64 {
    (key_down(VK_SHIFT) as u64) << 2 | (key_down(VK_CONTROL) as u64) << 3
}

/// Screen coordinates as the window's pixels, for lParam.
fn client_lparam(win: &UserWindow, x: i32, y: i32) -> u64 {
    let (cx, cy) = (x - win.content[0].load(Ordering::Relaxed), y - win.content[1].load(Ordering::Relaxed));
    ((cy as u16 as u64) << 16) | cx as u16 as u64
}

/// A mouse move replaces one the program hasn't taken yet: a slow program
/// sees where the pointer is now, not everywhere it has been.
fn post_move(win: &UserWindow, wparam: u64, lparam: u64) {
    let (x, y) = cursor();
    let msg = Msg { hwnd: win.hwnd as u64, message: WM_MOUSEMOVE, wparam, lparam, time: now() as u32, pt_x: x, pt_y: y, private: 0 };
    with_queue(win.tid, |q| match q.messages.back_mut() {
        Some(last) if last.message == WM_MOUSEMOVE && last.hwnd == msg.hwnd => *last = msg,
        _ => q.messages.push_back(msg),
    });
}

/// Every desktop poll: movement, button releases and the wheel.
pub fn mouse_tick(x: i32, y: i32, left: bool, right: bool, wheel: i32) {
    let Some(mut t) = TRACK.try_lock() else { return };
    let moved = t.last != (x, y);
    t.last = (x, y);
    let buttons = left as u64 | (right as u64) << 1 | modifier_keys();
    if let Some((vid, up)) = t.capture {
        let held = if up == WM_LBUTTONUP { left } else { right };
        match by_vid(vid) {
            Some(win) if !held => {
                post_to(win.tid, win.hwnd as u64, up, buttons, client_lparam(&win, x, y));
                t.capture = None;
            }
            Some(win) if moved => post_move(&win, buttons, client_lparam(&win, x, y)),
            Some(_) => {}
            None => t.capture = None,
        }
    }
    // Who's under the pointer (with the mouse held, the capture's window).
    let under = match t.capture {
        Some((vid, _)) => vid,
        None => match crate::desktop::DESKTOP_WM.try_lock() {
            Some(wm) => wm.content_at(x, y).filter(|&v| owns(v)).unwrap_or(0),
            None => t.hover,
        },
    };
    if under != t.hover {
        if let Some(old) = by_vid(t.hover) {
            post_to(old.tid, old.hwnd as u64, VEIL_MOUSE_LEFT, 0, 0);
        }
        t.hover = under;
    }
    let Some(win) = by_vid(under) else { return };
    if moved && t.capture.is_none() {
        post_move(&win, buttons, client_lparam(&win, x, y));
    }
    if wheel != 0 {
        // One notch is WHEEL_DELTA (120); forward, away from the user, is
        // positive. lParam carries the window's pixels here; user32 puts the
        // screen position there, having found the child.
        let delta = (-wheel * 120).clamp(-32768, 32767);
        post_to(win.tid, win.hwnd as u64, WM_MOUSEWHEEL, ((delta as i16 as u16 as u64) << 16) | buttons, client_lparam(&win, x, y));
    }
}

// -----| keys |-----

const VK_SHIFT: u32 = 0x10;
const VK_CONTROL: u32 = 0x11;
const VK_MENU: u32 = 0x12;
const VK_F10: u32 = 0x79;

/// Each virtual key's state: bit 0 down, bit 1 toggled (Caps Lock, ...).
static KEYS: [core::sync::atomic::AtomicU8; 256] = [const { core::sync::atomic::AtomicU8::new(0) }; 256];

fn key_down(vk: u32) -> bool {
    KEYS[vk as usize & 0xFF].load(Ordering::Relaxed) & 1 != 0
}

/// A key's virtual-key code, the left/right one for Shift, Ctrl and Alt,
/// its set-1 scan code and whether it's an extended key.
fn vk_of(key: pc_keyboard::KeyCode) -> Option<(u32, Option<u32>, u32, bool)> {
    use pc_keyboard::KeyCode as K;
    Some(match key {
        K::Escape => (0x1B, None, 0x01, false),
        K::F1 => (0x70, None, 0x3B, false),
        K::F2 => (0x71, None, 0x3C, false),
        K::F3 => (0x72, None, 0x3D, false),
        K::F4 => (0x73, None, 0x3E, false),
        K::F5 => (0x74, None, 0x3F, false),
        K::F6 => (0x75, None, 0x40, false),
        K::F7 => (0x76, None, 0x41, false),
        K::F8 => (0x77, None, 0x42, false),
        K::F9 => (0x78, None, 0x43, false),
        K::F10 => (0x79, None, 0x44, false),
        K::F11 => (0x7A, None, 0x57, false),
        K::F12 => (0x7B, None, 0x58, false),
        K::PrintScreen | K::SysRq => (0x2C, None, 0x37, true),
        K::ScrollLock => (0x91, None, 0x46, false),
        K::PauseBreak => (0x13, None, 0x45, false),
        K::Oem8 => (0xC0, None, 0x29, false),
        K::Key1 => (0x31, None, 0x02, false),
        K::Key2 => (0x32, None, 0x03, false),
        K::Key3 => (0x33, None, 0x04, false),
        K::Key4 => (0x34, None, 0x05, false),
        K::Key5 => (0x35, None, 0x06, false),
        K::Key6 => (0x36, None, 0x07, false),
        K::Key7 => (0x37, None, 0x08, false),
        K::Key8 => (0x38, None, 0x09, false),
        K::Key9 => (0x39, None, 0x0A, false),
        K::Key0 => (0x30, None, 0x0B, false),
        K::OemMinus => (0xBD, None, 0x0C, false),
        K::OemPlus => (0xBB, None, 0x0D, false),
        K::Backspace => (0x08, None, 0x0E, false),
        K::Tab => (0x09, None, 0x0F, false),
        K::Oem4 => (0xDB, None, 0x1A, false),
        K::Oem6 => (0xDD, None, 0x1B, false),
        K::Oem5 | K::Oem7 => (0xDC, None, 0x2B, false),
        K::Return => (0x0D, None, 0x1C, false),
        K::CapsLock => (0x14, None, 0x3A, false),
        K::Oem1 => (0xBA, None, 0x27, false),
        K::Oem3 => (0xDE, None, 0x28, false),
        K::OemComma => (0xBC, None, 0x33, false),
        K::OemPeriod => (0xBE, None, 0x34, false),
        K::Oem2 => (0xBF, None, 0x35, false),
        K::Spacebar => (0x20, None, 0x39, false),
        K::LShift => (VK_SHIFT, Some(0xA0), 0x2A, false),
        K::RShift => (VK_SHIFT, Some(0xA1), 0x36, false),
        K::LControl => (VK_CONTROL, Some(0xA2), 0x1D, false),
        K::RControl | K::RControl2 => (VK_CONTROL, Some(0xA3), 0x1D, true),
        K::LAlt => (VK_MENU, Some(0xA4), 0x38, false),
        K::RAltGr | K::RAlt2 => (VK_MENU, Some(0xA5), 0x38, true),
        K::LWin => (0x5B, None, 0x5B, true),
        K::RWin => (0x5C, None, 0x5C, true),
        K::Apps => (0x5D, None, 0x5D, true),
        K::Insert => (0x2D, None, 0x52, true),
        K::Home => (0x24, None, 0x47, true),
        K::PageUp => (0x21, None, 0x49, true),
        K::Delete => (0x2E, None, 0x53, true),
        K::End => (0x23, None, 0x4F, true),
        K::PageDown => (0x22, None, 0x51, true),
        K::ArrowUp => (0x26, None, 0x48, true),
        K::ArrowLeft => (0x25, None, 0x4B, true),
        K::ArrowDown => (0x28, None, 0x50, true),
        K::ArrowRight => (0x27, None, 0x4D, true),
        K::NumpadLock => (0x90, None, 0x45, true),
        K::NumpadDivide => (0x6F, None, 0x35, true),
        K::NumpadMultiply => (0x6A, None, 0x37, false),
        K::NumpadSubtract => (0x6D, None, 0x4A, false),
        K::NumpadAdd => (0x6B, None, 0x4E, false),
        K::NumpadEnter => (0x0D, None, 0x1C, true),
        K::NumpadPeriod => (0x6E, None, 0x53, false),
        K::Numpad0 => (0x60, None, 0x52, false),
        K::Numpad1 => (0x61, None, 0x4F, false),
        K::Numpad2 => (0x62, None, 0x50, false),
        K::Numpad3 => (0x63, None, 0x51, false),
        K::Numpad4 => (0x64, None, 0x4B, false),
        K::Numpad5 => (0x65, None, 0x4C, false),
        K::Numpad6 => (0x66, None, 0x4D, false),
        K::Numpad7 => (0x67, None, 0x47, false),
        K::Numpad8 => (0x68, None, 0x48, false),
        K::Numpad9 => (0x69, None, 0x49, false),
        K::A => (0x41, None, 0x1E, false),
        K::B => (0x42, None, 0x30, false),
        K::C => (0x43, None, 0x2E, false),
        K::D => (0x44, None, 0x20, false),
        K::E => (0x45, None, 0x12, false),
        K::F => (0x46, None, 0x21, false),
        K::G => (0x47, None, 0x22, false),
        K::H => (0x48, None, 0x23, false),
        K::I => (0x49, None, 0x17, false),
        K::J => (0x4A, None, 0x24, false),
        K::K => (0x4B, None, 0x25, false),
        K::L => (0x4C, None, 0x26, false),
        K::M => (0x4D, None, 0x32, false),
        K::N => (0x4E, None, 0x31, false),
        K::O => (0x4F, None, 0x18, false),
        K::P => (0x50, None, 0x19, false),
        K::Q => (0x51, None, 0x10, false),
        K::R => (0x52, None, 0x13, false),
        K::S => (0x53, None, 0x1F, false),
        K::T => (0x54, None, 0x14, false),
        K::U => (0x55, None, 0x16, false),
        K::V => (0x56, None, 0x2F, false),
        K::W => (0x57, None, 0x11, false),
        K::X => (0x58, None, 0x2D, false),
        K::Y => (0x59, None, 0x15, false),
        K::Z => (0x5A, None, 0x2C, false),
        _ => return None,
    })
}

/// A key went down or up. The key state is kept for every key (for
/// GetAsyncKeyState); if a Windows program's window has the keyboard, it
/// gets WM_KEYDOWN/WM_KEYUP (WM_SYSKEYDOWN/UP with Alt, or for F10).
/// True if a program's window had the keyboard.
pub fn key_event(key: pc_keyboard::KeyCode, state: pc_keyboard::KeyState) -> bool {
    use pc_keyboard::KeyState;
    let Some((vk, side, scan, ext)) = vk_of(key) else { return false };
    let presses: &[bool] = match state {
        KeyState::Down => &[true],
        KeyState::Up => &[false],
        KeyState::SingleShot => &[true, false],
    };
    let target = crate::desktop::DESKTOP_WM.try_lock().and_then(|wm| wm.focused_id).and_then(by_vid);
    for &down in presses {
        let was = key_down(vk);
        for v in core::iter::once(vk).chain(side) {
            let k = &KEYS[v as usize];
            let old = k.load(Ordering::Relaxed);
            let toggled = if down && old & 1 == 0 { old ^ 2 } else { old };
            k.store((toggled & 2) | down as u8, Ordering::Relaxed);
        }
        let Some(win) = target.as_ref() else { continue };
        let alt = key_down(VK_MENU);
        let sys = (alt && !key_down(VK_CONTROL)) || vk == VK_F10 || vk == VK_MENU;
        let message = match (down, sys) {
            (true, false) => WM_KEYDOWN,
            (false, false) => WM_KEYUP,
            (true, true) => WM_SYSKEYDOWN,
            (false, true) => WM_SYSKEYUP,
        };
        // lParam: repeat count, scan code, extended, context (Alt), previous
        // state, transition.
        let lp = 1 | (scan as u64) << 16 | (ext as u64) << 24 | ((alt && vk != VK_MENU) as u64) << 29
            | ((was || !down) as u64) << 30 | (!down as u64) << 31;
        post_to(win.tid, win.hwnd as u64, message, vk as u64, lp);
    }
    target.is_some()
}

// -----| the desktop's view: drawing a window's pixels |-----

/// A desktop window's content: a Windows program's surface.
pub struct PixelWidget {
    id: u32,
    rect: crate::gui_toolkit::Rect,
    flags: crate::gui_toolkit::WidgetFlags,
    window: Arc<UserWindow>,
}

impl crate::gui_toolkit::Widget for PixelWidget {
    fn id(&self) -> u32 {
        self.id
    }
    fn flags(&self) -> crate::gui_toolkit::WidgetFlags {
        self.flags
    }
    fn flags_mut(&mut self) -> &mut crate::gui_toolkit::WidgetFlags {
        &mut self.flags
    }
    fn rect(&self) -> crate::gui_toolkit::Rect {
        self.rect
    }
    fn measure(&self, available: crate::gui_toolkit::Size) -> crate::gui_toolkit::Size {
        available
    }
    fn layout(&mut self, rect: crate::gui_toolkit::Rect) {
        self.rect = rect;
    }
    fn paint(&self, _r: &crate::gui_toolkit::Renderer) {
        let w = &self.window;
        let (x, y) = (w.content[0].load(Ordering::Relaxed), w.content[1].load(Ordering::Relaxed));
        let (cw, ch) = (w.content[2].load(Ordering::Relaxed).max(0) as u32, w.content[3].load(Ordering::Relaxed).max(0) as u32);
        // Wait out a present that's copying: skipping would show what's
        // behind the window for a frame, and the window would flash.
        let s = w.surface.lock();
        crate::ui::draw_pixels_atomic(&s.pixels, s.w, s.h, x, y, cw.min(s.w), ch.min(s.h));
    }
    fn handle_event(&mut self, _ev: crate::gui_toolkit::WidgetEvent) -> bool {
        false
    }
}

// -----| services for user32 |-----

/// Height of the desktop's title bar above a window's client area, and its
/// border on each side.
fn frame(popup: bool) -> (u32, u32) {
    if popup {
        return (0, 0);
    }
    (crate::gui_toolkit::ACTIVE_THEME.lock().titlebar_height, 1)
}

fn read_title(p: u64) -> String {
    let mut s = String::new();
    if p == 0 {
        return s;
    }
    for i in 0..256u64 {
        let a = p + i * 2;
        if !ptr_ok(a, 2) {
            break;
        }
        let u = unsafe { core::ptr::read_unaligned(a as *const u16) };
        if u == 0 {
            break;
        }
        s.push(char::from_u32(u as u32).unwrap_or('?'));
    }
    s
}

fn current_tid() -> u32 {
    unsafe { (*crate::kthread::current_user()).tid }
}

/// VEIL_USER_CREATE_WINDOW(params*, hwnd_out*).
pub fn create_window(pid: u32, params: u64, out: u64) -> NtStatus {
    if !ptr_ok(params, core::mem::size_of::<WindowParams>()) || !ptr_ok(out, 8) {
        return status::ACCESS_VIOLATION;
    }
    let p = unsafe { core::ptr::read_unaligned(params as *const WindowParams) };
    let (cw, ch) = (p.client_w.clamp(1, 4096), p.client_h.clamp(1, 4096));
    let title = read_title(p.title);
    let popup = p.flags & nt_abi::WINDOW_POPUP != 0;
    let (title_h, border) = frame(popup);
    let vid = {
        let mut wm = crate::desktop::DESKTOP_WM.lock();
        let focused = wm.focused_id;
        let vid = wm.open(&title, p.x, p.y, cw + 2 * border, ch + title_h, pid);
        if popup {
            // Exactly where asked (no tiling), and the focus stays put.
            if let Some(dw) = wm.windows.iter_mut().find(|w| w.id == vid) {
                dw.frameless = true;
                dw.resizable = false;
                dw.rect = crate::gui_toolkit::Rect::new(p.x, p.y, cw, ch);
            }
            wm.focused_id = focused;
        }
        if p.flags & nt_abi::WINDOW_OWNED != 0 {
            if let Some(dw) = wm.windows.iter_mut().find(|w| w.id == vid) {
                dw.taskbar = false;
            }
        }
        vid
    };
    let hwnd = NEXT_HWND.fetch_add(4, Ordering::Relaxed);
    let win = Arc::new(UserWindow {
        hwnd,
        pid,
        tid: current_tid(),
        vid,
        surface: Mutex::new(Surface::new(cw, ch)),
        content: [AtomicI32::new(p.x + border as i32), AtomicI32::new(p.y + title_h as i32), AtomicI32::new(cw as i32), AtomicI32::new(ch as i32)],
        reported: Mutex::new(WindowPlace { x: p.x + border as i32, y: p.y + title_h as i32, w: cw, h: ch }),
        alive: AtomicBool::new(true),
        popup,
    });
    {
        let mut wm = crate::desktop::DESKTOP_WM.lock();
        if let Some(dw) = wm.windows.iter_mut().find(|w| w.id == vid) {
            dw.set_content(alloc::boxed::Box::new(PixelWidget {
                id: vid,
                rect: crate::gui_toolkit::Rect::default(),
                flags: crate::gui_toolkit::WidgetFlags::active(),
                window: win.clone(),
            }));
            if p.flags & nt_abi::WINDOW_VISIBLE == 0 {
                dw.state = crate::desktop::WindowState::Minimized;
            }
        }
    }
    WINDOWS.lock().push(win);
    unsafe { *(out as *mut u64) = hwnd as u64 };
    crate::serial_println!("[USER] pid {} window {:#x} '{}' {}x{} flags {:#x}", pid, hwnd, title, cw, ch, p.flags);
    status::SUCCESS
}

pub fn destroy_window(hwnd: u64) -> NtStatus {
    let Some(win) = by_hwnd(hwnd) else { return status::INVALID_HANDLE };
    WINDOWS.lock().retain(|w| w.hwnd != win.hwnd);
    win.alive.store(false, Ordering::Release);
    close_desktop_window(win.vid);
    // Its timers go too.
    let mut queues = QUEUES.lock();
    for q in queues.iter_mut() {
        q.timers.retain(|t| t.hwnd != hwnd);
    }
    status::SUCCESS
}

pub fn show_window(hwnd: u64, show: u64) -> NtStatus {
    let Some(win) = by_hwnd(hwnd) else { return status::INVALID_HANDLE };
    use crate::desktop::WindowState;
    let mut wm = crate::desktop::DESKTOP_WM.lock();
    if let Some(dw) = wm.windows.iter_mut().find(|w| w.id == win.vid) {
        match show as u32 {
            0 | 2 => dw.state = WindowState::Minimized,
            3 => dw.state = WindowState::Maximized,
            _ => {
                if dw.state == WindowState::Minimized {
                    dw.state = WindowState::Normal;
                }
            }
        }
        dw.dirty = true;
    }
    if show as u32 == 1 {
        wm.focus(win.vid);
    }
    crate::apps::TERMINAL_OUTPUT.store(true, Ordering::Release);
    status::SUCCESS
}

pub fn set_text(hwnd: u64, text: u64) -> NtStatus {
    let Some(win) = by_hwnd(hwnd) else { return status::INVALID_HANDLE };
    let title = read_title(text);
    if let Some(dw) = crate::desktop::DESKTOP_WM.lock().windows.iter_mut().find(|w| w.id == win.vid) {
        dw.title = title;
        dw.dirty = true;
    }
    crate::apps::TERMINAL_OUTPUT.store(true, Ordering::Release);
    status::SUCCESS
}

/// VEIL_USER_SET_POS(hwnd, place*, flags): place is the frame's screen
/// position and the client size.
pub fn set_pos(hwnd: u64, place: u64, flags: u64) -> NtStatus {
    let Some(win) = by_hwnd(hwnd) else { return status::INVALID_HANDLE };
    if !ptr_ok(place, core::mem::size_of::<WindowPlace>()) {
        return status::ACCESS_VIOLATION;
    }
    let p = unsafe { core::ptr::read_unaligned(place as *const WindowPlace) };
    let (title_h, border) = frame(win.popup);
    if let Some(dw) = crate::desktop::DESKTOP_WM.lock().windows.iter_mut().find(|w| w.id == win.vid) {
        if flags & 1 != 0 {
            dw.rect.x = p.x;
            dw.rect.y = p.y;
        }
        if flags & 2 != 0 {
            dw.rect.w = p.w.clamp(1, 4096) + 2 * border;
            dw.rect.h = p.h.clamp(1, 4096) + title_h;
        }
        dw.dirty = true;
        // What the program now sees (no notice for its own change).
        let place = WindowPlace { x: dw.rect.x + border as i32, y: dw.rect.y + title_h as i32, w: dw.rect.w - 2 * border, h: dw.rect.h - title_h };
        set_content_rect(win.vid, place.x, place.y, place.w, place.h);
        *win.reported.lock() = place;
        let mut s = win.surface.lock();
        if s.w != place.w || s.h != place.h {
            s.resize(place.w, place.h);
        }
    }
    crate::apps::TERMINAL_OUTPUT.store(true, Ordering::Release);
    status::SUCCESS
}

pub fn get_pos(hwnd: u64, out: u64) -> NtStatus {
    let Some(win) = by_hwnd(hwnd) else { return status::INVALID_HANDLE };
    if !ptr_ok(out, core::mem::size_of::<WindowPlace>()) {
        return status::ACCESS_VIOLATION;
    }
    unsafe { *(out as *mut WindowPlace) = *win.reported.lock() };
    status::SUCCESS
}

/// VEIL_USER_PRESENT(hwnd, pixels*, stride, rect*): copy a rectangle of
/// the program's pixels into the window's surface.
pub fn present(hwnd: u64, pixels: u64, stride: u64, rect: u64) -> NtStatus {
    let Some(win) = by_hwnd(hwnd) else { return status::INVALID_HANDLE };
    if !ptr_ok(rect, 16) {
        return status::ACCESS_VIOLATION;
    }
    let r = unsafe { core::ptr::read_unaligned(rect as *const [i32; 4]) };
    let stride = stride as u32 as usize;
    let mut s = win.surface.lock();
    let x0 = r[0].clamp(0, s.w as i32) as usize;
    let y0 = r[1].clamp(0, s.h as i32) as usize;
    let x1 = (r[0] + r[2]).clamp(0, s.w as i32) as usize;
    let y1 = (r[1] + r[3]).clamp(0, s.h as i32) as usize;
    if x0 >= x1 || y0 >= y1 {
        return status::SUCCESS;
    }
    // The source is the program's whole client-sized buffer.
    let last = (y1 - 1) * stride + x1;
    if stride < x1 || !ptr_ok(pixels, last * 4) {
        return status::ACCESS_VIOLATION;
    }
    let sw = s.w as usize;
    for y in y0..y1 {
        let src = unsafe { core::slice::from_raw_parts((pixels as *const u32).add(y * stride + x0), x1 - x0) };
        s.pixels[y * sw + x0..y * sw + x1].copy_from_slice(src);
    }
    drop(s);
    if let Some(mut wm) = crate::desktop::DESKTOP_WM.try_lock() {
        if let Some(dw) = wm.windows.iter_mut().find(|w| w.id == win.vid) {
            dw.dirty = true;
        }
    }
    crate::apps::TERMINAL_OUTPUT.store(true, Ordering::Release);
    status::SUCCESS
}

/// VEIL_USER_POST(hwnd, message, wparam, lparam, tid): to a window's thread,
/// or, for hwnd 0 or a window only user32 knows (a child window), to thread
/// `tid` (0: this thread). The message keeps its hwnd.
pub fn post(hwnd: u64, message: u64, wparam: u64, lparam: u64) -> NtStatus {
    let tid = match by_hwnd(hwnd).filter(|_| hwnd != 0) {
        Some(w) => w.tid,
        None => match stack(4).unwrap_or(0) as u32 {
            0 => current_tid(),
            t => t,
        },
    };
    post_to(tid, hwnd, message as u32, wparam, lparam);
    status::SUCCESS
}

/// user32's notice that the desktop moved or resized a window (it asks
/// VEIL_USER_GET_POS where to, and sends the program WM_WINDOWPOSCHANGED,
/// WM_SIZE and WM_MOVE itself).
const VEIL_GEOMETRY: u32 = 0x7FFF_0002;

/// Compare the thread's windows with what the desktop shows and tell user32
/// about changes the user made (dragging, resizing).
fn report_changes(tid: u32) {
    let mine: Vec<Arc<UserWindow>> = WINDOWS.lock().iter().filter(|w| w.tid == tid).cloned().collect();
    for w in mine {
        let now = WindowPlace {
            x: w.content[0].load(Ordering::Relaxed),
            y: w.content[1].load(Ordering::Relaxed),
            w: w.content[2].load(Ordering::Relaxed).max(1) as u32,
            h: w.content[3].load(Ordering::Relaxed).max(1) as u32,
        };
        let mut rep = w.reported.lock();
        if (now.x, now.y, now.w, now.h) == (rep.x, rep.y, rep.w, rep.h) {
            continue;
        }
        let sized = (now.w, now.h) != (rep.w, rep.h);
        *rep = now;
        drop(rep);
        if sized {
            w.surface.lock().resize(now.w, now.h);
        }
        post_to(tid, w.hwnd as u64, VEIL_GEOMETRY, 0, 0);
    }
}

/// VEIL_USER_GET_MESSAGE(msg_out*, timeout_ms, flags, hwnd filter, min, max).
pub fn get_message(out: u64, timeout: u64, flags: u64, filter: u64) -> NtStatus {
    // Stack arguments first: the user RSP they're read through changes once
    // this thread waits.
    let (min, max) = (stack(4).unwrap_or(0) as u32, stack(5).unwrap_or(0) as u32);
    if !ptr_ok(out, core::mem::size_of::<Msg>()) {
        return status::ACCESS_VIOLATION;
    }
    let remove = flags & 1 != 0;
    let timers = flags & 2 == 0;
    let tid = current_tid();
    let timeout = timeout as u32;
    let until = if timeout == u32::MAX { None } else { Some(now() + timeout as u64) };
    let wanted = |m: &Msg| (filter == 0 || m.hwnd == filter) && (min == 0 && max == 0 || (m.message >= min && m.message <= max));
    loop {
        report_changes(tid);
        let found = with_queue(tid, |q| {
            if let Some(i) = q.messages.iter().position(|m| wanted(m)) {
                return Some(if remove { q.messages.remove(i).unwrap() } else { q.messages[i] });
            }
            if !timers {
                return None;
            }
            let t = now();
            let (x, y) = cursor();
            for timer in q.timers.iter_mut() {
                if t >= timer.due {
                    let m = Msg { hwnd: timer.hwnd, message: WM_TIMER, wparam: timer.id, lparam: 0, time: t as u32, pt_x: x, pt_y: y, private: 0 };
                    if wanted(&m) {
                        if remove {
                            timer.due = t + timer.interval;
                        }
                        return Some(m);
                    }
                }
            }
            None
        });
        if let Some(m) = found {
            unsafe { *(out as *mut Msg) = m };
            return status::SUCCESS;
        }
        if until.is_some_and(|u| now() >= u) {
            return status::TIMEOUT;
        }
        crate::loader::kill_if_requested();
        crate::nt_syscalls::wait_a_little();
    }
}

/// VEIL_USER_TIMER(hwnd, id, interval_ms): set or reset a timer, or (interval
/// 0) kill it. A timer for hwnd 0 or a window only user32 knows is this
/// thread's.
pub fn timer(hwnd: u64, id: u64, interval: u64) -> NtStatus {
    let tid = by_hwnd(hwnd).filter(|_| hwnd != 0).map_or_else(current_tid, |w| w.tid);
    let interval = interval as u32 as u64;
    let found = with_queue(tid, |q| {
        let before = q.timers.len();
        q.timers.retain(|t| !(t.hwnd == hwnd && t.id == id));
        if interval != 0 {
            // Windows rounds tiny intervals up to USER_TIMER_MINIMUM (10 ms).
            let interval = interval.max(10);
            q.timers.push(Timer { hwnd, id, interval, due: now() + interval });
        }
        before != q.timers.len()
    });
    if interval == 0 && !found { status::INVALID_PARAMETER } else { status::SUCCESS }
}

/// VEIL_CLIP_SET(text*, chars): text a Windows program copied.
pub fn clip_set(pid: u32, text: u64, chars: u64) -> NtStatus {
    let n = (chars as usize).min(1 << 20);
    if !ptr_ok(text, n * 2) {
        return status::ACCESS_VIOLATION;
    }
    let units = unsafe { core::slice::from_raw_parts(text as *const u16, n) };
    let s: String = char::decode_utf16(units.iter().copied()).map(|c| c.unwrap_or('\u{FFFD}')).collect();
    crate::clipboard::CLIPBOARD.lock().copy_text(s, pid, false);
    status::SUCCESS
}

/// VEIL_CLIP_GET(buf*, cap, chars_out*, seq_out*): the latest text.
pub fn clip_get(pid: u32, buf: u64, cap: u64, chars_out: u64) -> NtStatus {
    let seq_out = stack(4).unwrap_or(0);
    let text = crate::clipboard::CLIPBOARD.lock().paste_text(pid).unwrap_or_default();
    let units: Vec<u16> = text.encode_utf16().collect();
    if ptr_ok(chars_out, 4) {
        unsafe { *(chars_out as *mut u32) = units.len() as u32 };
    }
    if ptr_ok(seq_out, 4) {
        unsafe { *(seq_out as *mut u32) = crate::clipboard::SEQUENCE.load(Ordering::Relaxed) };
    }
    if buf != 0 {
        let n = units.len().min(cap as usize);
        if !ptr_ok(buf, n * 2) {
            return status::ACCESS_VIOLATION;
        }
        unsafe { core::ptr::copy_nonoverlapping(units.as_ptr(), buf as *mut u16, n) };
    }
    status::SUCCESS
}

/// VEIL_USER_QUERY(what, out*).
pub fn query(what: u64, out: u64) -> NtStatus {
    if !ptr_ok(out, 8) {
        return status::ACCESS_VIOLATION;
    }
    let v: [i32; 2] = match what as u32 {
        0 => [crate::ui::screen_w() as i32, crate::ui::screen_h() as i32],
        1 => {
            let (x, y) = cursor();
            [x, y]
        }
        w if w & 0xFF == 2 => {
            let vk = (w >> 8) & 0xFF;
            let k = KEYS[vk as usize].load(Ordering::Relaxed);
            // The mouse buttons are keys too.
            let m = crate::task::mouse::MOUSE_STATE.lock();
            let down = match vk {
                1 => m.left_button,
                2 => m.right_button,
                _ => k & 1 != 0,
            };
            [down as i32, (k >> 1 & 1) as i32]
        }
        _ => return status::INVALID_PARAMETER,
    };
    unsafe { *(out as *mut [i32; 2]) = v };
    status::SUCCESS
}
