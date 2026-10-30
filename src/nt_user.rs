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
            post_to(win.tid, hwnd, message, buttons, lp);
            // The desktop reports clicks, not releases: the button comes up at once.
            if message == WM_LBUTTONDOWN {
                post_to(win.tid, hwnd, WM_LBUTTONUP, 0, lp);
            } else if message == WM_RBUTTONDOWN {
                post_to(win.tid, hwnd, WM_RBUTTONUP, 0, lp);
            }
        }
        WM_CHAR => {
            // A typed character: key down, the character, key up.
            let ch = wparam as u32;
            let vk = vk_for_char(ch);
            if vk != 0 {
                post_to(win.tid, hwnd, WM_KEYDOWN, vk as u64, 1);
            }
            post_to(win.tid, hwnd, WM_CHAR, ch as u64, 1);
            if vk != 0 {
                post_to(win.tid, hwnd, WM_KEYUP, vk as u64, 0xC000_0001);
            }
        }
        _ => post_to(win.tid, hwnd, message, wparam, lparam),
    }
    true
}

/// A key the desktop didn't turn into a character (arrows, F-keys, ...)
/// for window `vid`: key down and up.
pub fn deliver_key(vid: u32, vk: u32) -> bool {
    let Some(win) = by_vid(vid) else { return false };
    post_to(win.tid, win.hwnd as u64, WM_KEYDOWN, vk as u64, 1);
    post_to(win.tid, win.hwnd as u64, WM_KEYUP, vk as u64, 0xC000_0001);
    true
}

/// The virtual-key code for a typed character (letters and digits are
/// their upper-case ASCII; a few others by name).
fn vk_for_char(ch: u32) -> u32 {
    match ch {
        0x61..=0x7A => ch - 0x20,
        0x41..=0x5A | 0x30..=0x39 => ch,
        0x20 => 0x20,       // VK_SPACE
        0x0D | 0x0A => 0x0D, // VK_RETURN
        0x08 => 0x08,       // VK_BACK
        0x09 => 0x09,       // VK_TAB
        0x1B => 0x1B,       // VK_ESCAPE
        _ => 0,
    }
}

/// The virtual-key code for a key that types no character (0: none).
pub fn vk_for_key(key: pc_keyboard::KeyCode) -> u32 {
    use pc_keyboard::KeyCode as K;
    match key {
        K::ArrowLeft => 0x25,
        K::ArrowUp => 0x26,
        K::ArrowRight => 0x27,
        K::ArrowDown => 0x28,
        K::PageUp => 0x21,
        K::PageDown => 0x22,
        K::End => 0x23,
        K::Home => 0x24,
        K::Insert => 0x2D,
        K::Delete => 0x2E,
        K::F1 => 0x70,
        K::F2 => 0x71,
        K::F3 => 0x72,
        K::F4 => 0x73,
        K::F5 => 0x74,
        K::F6 => 0x75,
        K::F7 => 0x76,
        K::F8 => 0x77,
        K::F9 => 0x78,
        K::F10 => 0x79,
        K::F11 => 0x7A,
        K::F12 => 0x7B,
        _ => 0,
    }
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
        if let Some(s) = w.surface.try_lock() {
            crate::ui::draw_pixels_atomic(&s.pixels, s.w, s.h, x, y, cw.min(s.w), ch.min(s.h));
        }
    }
    fn handle_event(&mut self, _ev: crate::gui_toolkit::WidgetEvent) -> bool {
        false
    }
}

// -----| services for user32 |-----

/// Height of the desktop's title bar above a window's client area, and its
/// border on each side.
fn frame() -> (u32, u32) {
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
    let (title_h, border) = frame();
    let vid = crate::desktop::DESKTOP_WM.lock().open(&title, p.x, p.y, cw + 2 * border, ch + title_h, pid);
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
            if p.visible == 0 {
                dw.state = crate::desktop::WindowState::Minimized;
            }
        }
    }
    WINDOWS.lock().push(win);
    unsafe { *(out as *mut u64) = hwnd as u64 };
    crate::serial_println!("[USER] pid {} window {:#x} '{}' {}x{}", pid, hwnd, title, cw, ch);
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
    if show as u32 == 1 || show as u32 == 4 {
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
    let (title_h, border) = frame();
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
            *s = Surface::new(place.w, place.h);
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
            *w.surface.lock() = Surface::new(now.w, now.h);
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
        _ => return status::INVALID_PARAMETER,
    };
    unsafe { *(out as *mut [i32; 2]) = v };
    status::SUCCESS
}
