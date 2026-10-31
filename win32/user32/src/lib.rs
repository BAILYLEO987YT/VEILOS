//! VEILOS `user32.dll`: windows, messages and painting (WIN32_DESIGN.md, G1).
//!
//! A top-level window is the kernel's (nt_user.rs): a desktop window whose
//! client area shows pixels this DLL presents. Child windows exist only
//! here: each is a rectangle of its top-level window's pixels. Messages are
//! the kernel's per-thread queue (posted messages, input, timers) plus
//! WM_PAINT, which is made here from the windows' update rectangles.
//!
//! Window geometry follows Windows 10's metrics (a sizable caption window's
//! frame is 8 pixels with a 23-pixel caption), so programs size and lay out
//! as they do on Windows; the desktop draws its own frame around the client
//! area.
//!
//!   lib.rs      classes, the window table, text and character sets
//!   window.rs   creating, destroying, showing, moving; window data
//!   message.rs  the queue, sending and dispatching, timers, DefWindowProc
//!   paint.rs    pixels, DCs, invalidation and painting, DrawText, colors,
//!               metrics
//!   image.rs    bitmaps, icons and cursors (resources, files, standard ones)
//!   scroll.rs   scroll bars: standard ones and the ScrollBar control
//!   keyboard.rs key state, virtual keys, the caret
//!   controls.rs the standard controls' shared parts; Button, Static
//!   edit.rs     the Edit control
//!   listbox.rs  ListBox, ComboBox (and its drop-down list)
//!   dialog.rs   dialog templates, DefDlgProc, DialogBox, IsDialogMessage,
//!               MessageBox
//!   menu.rs     menus, the menu bar, popup menus and the menu loop, the
//!               system menu, accelerators

#![no_std]

extern crate alloc;

mod controls;
mod dialog;
#[path = "../../dllrt.rs"]
mod dllrt;
mod edit;
mod image;
mod keyboard;
mod listbox;
mod menu;
mod message;
mod paint;
mod scroll;
mod window;

use alloc::vec::Vec;
use dllrt::Lock;

pub(crate) type Hwnd = u64;
pub(crate) type Bool = i32;
pub(crate) type WndProc = unsafe extern "system" fn(Hwnd, u32, u64, i64) -> i64;

#[link(name = "kernel32", kind = "raw-dylib")]
unsafe extern "system" {
    pub(crate) fn GetCurrentThreadId() -> u32;
    pub(crate) fn GetCurrentProcessId() -> u32;
    pub(crate) fn SetLastError(code: u32);
    pub(crate) fn MultiByteToWideChar(cp: u32, flags: u32, s: *const u8, n: i32, out: *mut u16, cap: i32) -> i32;
    pub(crate) fn WideCharToMultiByte(cp: u32, flags: u32, s: *const u16, n: i32, out: *mut u8, cap: i32, def: *const u8, used: *mut i32) -> i32;
}

#[link(name = "ntdll", kind = "raw-dylib")]
unsafe extern "system" {
    pub(crate) fn VeilUserCreateWindow(params: *const nt_abi::WindowParams, hwnd: *mut u64) -> i32;
    pub(crate) fn VeilUserDestroyWindow(hwnd: u64) -> i32;
    pub(crate) fn VeilUserShowWindow(hwnd: u64, show: u32) -> i32;
    pub(crate) fn VeilUserSetText(hwnd: u64, text: *const u16) -> i32;
    pub(crate) fn VeilUserSetPos(hwnd: u64, place: *const nt_abi::WindowPlace, flags: u32) -> i32;
    pub(crate) fn VeilUserGetPos(hwnd: u64, place: *mut nt_abi::WindowPlace) -> i32;
    pub(crate) fn VeilUserPresent(hwnd: u64, pixels: *const u32, stride: u64, rect: *const [i32; 4]) -> i32;
    pub(crate) fn VeilUserPost(hwnd: u64, message: u32, wparam: u64, lparam: u64, tid: u32) -> i32;
    pub(crate) fn VeilUserGetMessage(msg: *mut nt_abi::Msg, timeout: u32, flags: u32, hwnd: u64, min: u32, max: u32) -> i32;
    pub(crate) fn VeilUserTimer(hwnd: u64, id: u64, interval: u32) -> i32;
    pub(crate) fn VeilUserQuery(what: u32, out: *mut [i32; 2]) -> i32;
}

#[link(name = "gdi32", kind = "raw-dylib")]
unsafe extern "system" {
    pub(crate) fn VeilGdiCreateDC(pixels: *mut u32, w: i32, h: i32, stride: usize, ox: i32, oy: i32, clip: *const [i32; 4]) -> u64;
    pub(crate) fn DeleteDC(hdc: u64) -> Bool;
    pub(crate) fn SelectObject(hdc: u64, obj: u64) -> u64;
    pub(crate) fn PatBlt(hdc: u64, x: i32, y: i32, w: i32, h: i32, rop: u32) -> Bool;
    pub(crate) fn CreateSolidBrush(color: u32) -> u64;
    pub(crate) fn GetClipBox(hdc: u64, out: *mut [i32; 4]) -> i32;
    pub(crate) fn TextOutW(hdc: u64, x: i32, y: i32, s: *const u16, n: i32) -> Bool;
    pub(crate) fn GetTextExtentPoint32W(hdc: u64, s: *const u16, n: i32, out: *mut [i32; 2]) -> Bool;
    pub(crate) fn SetTextAlign(hdc: u64, align: u32) -> u32;
    pub(crate) fn GetTextMetricsW(hdc: u64, out: *mut u8) -> Bool;
    pub(crate) fn GetTextColor(hdc: u64) -> u32;
    pub(crate) fn DeleteObject(h: u64) -> Bool;
    pub(crate) fn SaveDC(hdc: u64) -> i32;
    pub(crate) fn RestoreDC(hdc: u64, n: i32) -> Bool;
    pub(crate) fn IntersectClipRect(hdc: u64, l: i32, t: i32, r: i32, b: i32) -> i32;
    pub(crate) fn SetTextColor(hdc: u64, color: u32) -> u32;
    pub(crate) fn SetBkColor(hdc: u64, color: u32) -> u32;
    pub(crate) fn SetBkMode(hdc: u64, mode: i32) -> i32;
    pub(crate) fn CreateCompatibleDC(hdc: u64) -> u64;
    pub(crate) fn BitBlt(dst: u64, x: i32, y: i32, w: i32, h: i32, src: u64, sx: i32, sy: i32, rop: u32) -> Bool;
    pub(crate) fn GetObjectW(h: u64, size: i32, out: *mut u8) -> i32;
    pub(crate) fn CreateFontW(h: i32, w: i32, esc: i32, orient: i32, weight: i32, italic: u32, underline: u32, strike: u32,
                              charset: u32, out_prec: u32, clip_prec: u32, quality: u32, pitch: u32, face: *const u16) -> u64;
}

pub(crate) const ERROR_INVALID_WINDOW_HANDLE: u32 = 1400;
pub(crate) const ERROR_CLASS_ALREADY_EXISTS: u32 = 1410;
pub(crate) const ERROR_CANNOT_FIND_WND_CLASS: u32 = 1407;
pub(crate) const ERROR_CLASS_DOES_NOT_EXIST: u32 = 1411;
pub(crate) const ERROR_INVALID_PARAMETER: u32 = 87;

pub(crate) fn set_error(code: u32) {
    unsafe { SetLastError(code) };
}


// -----| text |-----

/// A NUL-terminated UTF-16 string.
pub(crate) unsafe fn wstr(p: *const u16) -> Vec<u16> {
    let mut v = Vec::new();
    if p.is_null() {
        return v;
    }
    let mut i = 0;
    loop {
        let c = unsafe { *p.add(i) };
        if c == 0 {
            return v;
        }
        v.push(c);
        i += 1;
    }
}

/// An ANSI (UTF-8) string as UTF-16: `n` bytes, or to the NUL if negative.
pub(crate) unsafe fn from_ansi(p: *const u8, n: i32) -> Vec<u16> {
    if p.is_null() || n == 0 {
        return Vec::new();
    }
    let len = if n < 0 {
        let mut k = 0;
        while unsafe { *p.add(k) } != 0 {
            k += 1;
        }
        k as i32
    } else {
        n
    };
    if len == 0 {
        return Vec::new();
    }
    let want = unsafe { MultiByteToWideChar(0, 0, p, len, core::ptr::null_mut(), 0) };
    let mut v = alloc::vec![0u16; want.max(0) as usize];
    unsafe { MultiByteToWideChar(0, 0, p, len, v.as_mut_ptr(), want) };
    v
}

/// UTF-16 to ANSI (UTF-8) bytes, without a NUL.
pub(crate) fn to_ansi(s: &[u16]) -> Vec<u8> {
    if s.is_empty() {
        return Vec::new();
    }
    let n = s.len() as i32;
    let want = unsafe { WideCharToMultiByte(0, 0, s.as_ptr(), n, core::ptr::null_mut(), 0, core::ptr::null(), core::ptr::null_mut()) };
    let mut v = alloc::vec![0u8; want.max(0) as usize];
    unsafe { WideCharToMultiByte(0, 0, s.as_ptr(), n, v.as_mut_ptr(), want, core::ptr::null(), core::ptr::null_mut()) };
    v
}

/// A class or window name argument: a string, or an atom (MAKEINTATOM).
pub(crate) enum Name {
    Text(Vec<u16>),
    Atom(u16),
}

pub(crate) unsafe fn name_w(p: *const u16) -> Name {
    if (p as usize) >> 16 == 0 { Name::Atom(p as usize as u16) } else { Name::Text(unsafe { wstr(p) }) }
}

pub(crate) unsafe fn name_a(p: *const u8) -> Name {
    if (p as usize) >> 16 == 0 { Name::Atom(p as usize as u16) } else { Name::Text(unsafe { from_ansi(p, -1) }) }
}

/// Case-insensitive (ASCII) comparison, as class names compare.
pub(crate) fn same_name(a: &[u16], b: &[u16]) -> bool {
    let lower = |c: u16| if (b'A' as u16..=b'Z' as u16).contains(&c) { c + 32 } else { c };
    a.len() == b.len() && a.iter().zip(b).all(|(&x, &y)| lower(x) == lower(y))
}

/// Copy `s` into a caller's buffer of `cap` units, NUL-terminated and
/// truncated to fit; the units copied (without the NUL).
pub(crate) unsafe fn copy_out<T: Copy + Default>(s: &[T], out: *mut T, cap: usize) -> usize {
    if out.is_null() || cap == 0 {
        return 0;
    }
    let n = s.len().min(cap - 1);
    unsafe {
        core::ptr::copy_nonoverlapping(s.as_ptr(), out, n);
        *out.add(n) = T::default();
    }
    n
}

// -----| classes |-----

pub(crate) struct Class {
    pub name: Vec<u16>,
    pub atom: u16,
    pub style: u32,
    pub proc: usize,
    pub unicode: bool,
    pub cls_extra: Vec<u8>,
    pub wnd_extra: i32,
    pub instance: u64,
    pub icon: u64,
    pub icon_sm: u64,
    pub cursor: u64,
    pub brush: u64,
    pub menu_name: u64,
}

// -----| windows |-----

pub(crate) struct Wnd {
    pub hwnd: Hwnd,
    pub tid: u32,
    pub parent: Hwnd,
    pub owner: Hwnd,
    pub class: usize,
    pub proc: usize,
    pub unicode: bool,
    pub style: u32,
    pub ex_style: u32,
    pub id: u64,
    pub instance: u64,
    pub user_data: u64,
    pub extra: Vec<u8>,
    pub text: Vec<u16>,
    /// Window rectangle: in the parent's client coordinates (a top-level
    /// window's in screen coordinates).
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    /// The client area inside it: left, top, right and bottom margins.
    pub margins: [i32; 4],
    pub visible: bool,
    pub enabled: bool,
    pub minimized: bool,
    pub maximized: bool,
    /// WM_SIZE and WM_MOVE are still owed (a top-level window gets them
    /// when first shown).
    pub send_size_move: bool,
    /// Update rectangle (client coordinates) and whether it needs erasing.
    pub update: Option<[i32; 4]>,
    pub erase: bool,
    pub destroying: bool,
    /// The kernel's window: a top-level window on the desktop (not a child,
    /// nor a message-only window).
    pub kernel: bool,
    /// A kernel window without the desktop's frame (a popup).
    pub popup: bool,
    /// The standard scroll bars (WS_HSCROLL, WS_VSCROLL).
    pub scroll: [ScrollBar; 2],
    /// A top-level window's menu bar.
    pub menu: u64,
    /// A top-level window's client pixels (stride `stride`), and the part
    /// drawn since it was last presented.
    pub surface: Vec<u32>,
    pub stride: usize,
    pub dirty: Option<[i32; 4]>,
}

/// A scroll bar's SCROLLINFO, and whether its arrows are disabled.
#[derive(Clone, Copy, Default)]
pub(crate) struct ScrollBar {
    pub min: i32,
    pub max: i32,
    pub page: u32,
    pub pos: i32,
    pub disabled: u32,
}

impl Wnd {
    pub fn top_level(&self) -> bool {
        self.parent == 0
    }
    /// The part of a kernel window's margins the desktop's frame stands
    /// in for (Windows' border and caption); the rest of the non-client
    /// area (scroll bars, the menu bar) is drawn in the window's pixels.
    pub fn frame(&self) -> [i32; 4] {
        if !self.kernel || self.popup {
            return [0; 4];
        }
        let f = crate::window::frame_margins(self.style, self.ex_style, true);
        [f[0].min(self.margins[0]), f[1].min(self.margins[1]), f[2].min(self.margins[2]), f[3].min(self.margins[3])]
    }
    /// Where the client area is inside the window's pixels: the non-client
    /// parts drawn there (left, top, right, bottom).
    pub fn inset(&self) -> [i32; 4] {
        let f = self.frame();
        [self.margins[0] - f[0], self.margins[1] - f[1], self.margins[2] - f[2], self.margins[3] - f[3]]
    }
    /// A kernel window's pixels: their size.
    pub fn surface_size(&self) -> (i32, i32) {
        let f = self.frame();
        ((self.w - f[0] - f[2]).max(1), (self.h - f[1] - f[3]).max(1))
    }
    pub fn client_w(&self) -> i32 {
        (self.w - self.margins[0] - self.margins[2]).max(0)
    }
    pub fn client_h(&self) -> i32 {
        (self.h - self.margins[1] - self.margins[3]).max(0)
    }
}

pub(crate) struct State {
    pub classes: Vec<Class>,
    pub windows: Vec<Wnd>,
    pub next_atom: u16,
    pub next_child: u64,
    /// (hwnd, id, TIMERPROC) for timers with a procedure.
    pub timer_procs: Vec<(Hwnd, u64, u64)>,
    pub next_timer: u64,
    pub focus: Hwnd,
    pub active: Hwnd,
    pub capture: Hwnd,
}

pub(crate) static STATE: Lock<State> = Lock::new(State {
    classes: Vec::new(),
    windows: Vec::new(),
    next_atom: 0xC000,
    next_child: 0x0080_0010,
    timer_procs: Vec::new(),
    next_timer: 0x7FFF_0001,
    focus: 0,
    active: 0,
    capture: 0,
});

impl State {
    pub fn get(&self, hwnd: Hwnd) -> Option<&Wnd> {
        if hwnd == 0 {
            return None;
        }
        self.windows.iter().find(|w| w.hwnd == hwnd)
    }
    pub fn get_mut(&mut self, hwnd: Hwnd) -> Option<&mut Wnd> {
        if hwnd == 0 {
            return None;
        }
        self.windows.iter_mut().find(|w| w.hwnd == hwnd)
    }
    /// The top-level window `hwnd` is in.
    pub fn root(&self, mut hwnd: Hwnd) -> Hwnd {
        while let Some(w) = self.get(hwnd) {
            if w.parent == 0 {
                return hwnd;
            }
            hwnd = w.parent;
        }
        0
    }
    /// Where `hwnd`'s client (0, 0) is in its top-level window's client area.
    /// (In its top-level window's pixels, whose own client area may start
    /// after an inset.)
    pub fn client_origin(&self, hwnd: Hwnd) -> (i32, i32) {
        let (mut x, mut y) = (0, 0);
        let mut h = hwnd;
        while let Some(w) = self.get(h) {
            if w.parent == 0 {
                let i = w.inset();
                return (x + i[0], y + i[1]);
            }
            x += w.x + w.margins[0];
            y += w.y + w.margins[1];
            h = w.parent;
        }
        (x, y)
    }
    /// `hwnd`'s client area in screen coordinates: its top-left corner.
    pub fn client_to_screen(&self, hwnd: Hwnd) -> (i32, i32) {
        let root = self.root(hwnd);
        let (ox, oy) = self.client_origin(hwnd);
        match self.get(root) {
            Some(r) => (r.x + r.frame()[0] + ox, r.y + r.frame()[1] + oy),
            None => (ox, oy),
        }
    }
    /// The part of `hwnd`'s client area its ancestors don't cut off, in its
    /// top-level window's pixels.
    pub fn visible_clip(&self, hwnd: Hwnd) -> [i32; 4] {
        let (ox, oy) = self.client_origin(hwnd);
        let Some(w) = self.get(hwnd) else { return [0, 0, 0, 0] };
        let mut r = [ox, oy, ox + w.client_w(), oy + w.client_h()];
        let mut h = w.parent;
        while let Some(p) = self.get(h) {
            let (px, py) = self.client_origin(h);
            r = intersect(r, [px, py, px + p.client_w(), py + p.client_h()]);
            h = p.parent;
        }
        r
    }
    pub fn children(&self, hwnd: Hwnd) -> Vec<Hwnd> {
        self.windows.iter().filter(|w| w.parent == hwnd && !w.destroying).map(|w| w.hwnd).collect()
    }
}

pub(crate) fn intersect(a: [i32; 4], b: [i32; 4]) -> [i32; 4] {
    [a[0].max(b[0]), a[1].max(b[1]), a[2].min(b[2]), a[3].min(b[3])]
}

pub(crate) fn union(a: Option<[i32; 4]>, b: [i32; 4]) -> [i32; 4] {
    match a {
        Some(a) => [a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])],
        None => b,
    }
}

pub(crate) fn empty(r: [i32; 4]) -> bool {
    r[0] >= r[2] || r[1] >= r[3]
}

pub(crate) fn state() -> dllrt::Guard<'static, State> {
    STATE.lock()
}

pub(crate) fn screen_size() -> (i32, i32) {
    let mut v = [1024, 768];
    unsafe { VeilUserQuery(0, &mut v) };
    (v[0], v[1])
}

#[no_mangle]
pub extern "system" fn DllMain(_module: usize, reason: u32, _reserved: usize) -> i32 {
    if reason == 1 {
        controls::register_classes();
    }
    1
}

// -----| window classes |-----

/// WNDCLASSEX and WNDCLASS share their layout from lpfnWndProc (offset 8)
/// on; `style` is at 4 in one and 0 in the other.
unsafe fn register(wc: *const u8, ex: bool, unicode: bool) -> u16 {
    let base = wc;
    let rd = |off: usize| unsafe { core::ptr::read_unaligned(base.add(off) as *const u64) };
    let rd32 = |off: usize| unsafe { core::ptr::read_unaligned(base.add(off) as *const i32) };
    let style = rd32(if ex { 4 } else { 0 }) as u32;
    let proc = rd(8) as usize;
    let name_ptr = rd(64);
    let name = if unicode { unsafe { name_w(name_ptr as *const u16) } } else { unsafe { name_a(name_ptr as *const u8) } };
    let Name::Text(name) = name else {
        set_error(ERROR_INVALID_PARAMETER);
        return 0;
    };
    let instance = rd(24);
    let mut s = state();
    if s.classes.iter().any(|c| same_name(&c.name, &name)) {
        set_error(ERROR_CLASS_ALREADY_EXISTS);
        return 0;
    }
    let atom = s.next_atom;
    s.next_atom += 1;
    s.classes.push(Class {
        name,
        atom,
        style,
        proc,
        unicode,
        cls_extra: alloc::vec![0; rd32(16).clamp(0, 4096) as usize],
        wnd_extra: rd32(20).clamp(0, 4096),
        instance,
        icon: rd(32),
        cursor: rd(40),
        brush: rd(48),
        menu_name: rd(56),
        icon_sm: if ex { rd(72) } else { 0 },
    });
    atom
}

#[no_mangle]
pub unsafe extern "system" fn RegisterClassExW(wc: *const u8) -> u16 {
    unsafe { register(wc, true, true) }
}

#[no_mangle]
pub unsafe extern "system" fn RegisterClassExA(wc: *const u8) -> u16 {
    unsafe { register(wc, true, false) }
}

#[no_mangle]
pub unsafe extern "system" fn RegisterClassW(wc: *const u8) -> u16 {
    unsafe { register(wc, false, true) }
}

#[no_mangle]
pub unsafe extern "system" fn RegisterClassA(wc: *const u8) -> u16 {
    unsafe { register(wc, false, false) }
}

pub(crate) fn find_class(s: &State, name: &Name) -> Option<usize> {
    s.classes.iter().position(|c| match name {
        Name::Atom(a) => c.atom == *a,
        Name::Text(t) => same_name(&c.name, t),
    })
}

fn unregister(name: Name) -> Bool {
    let mut s = state();
    match find_class(&s, &name) {
        Some(i) if !s.windows.iter().any(|w| w.class == i) => {
            // Keep the indices of the other classes: leave an unusable entry.
            s.classes[i].name.clear();
            s.classes[i].atom = 0;
            1
        }
        Some(_) => 0,
        None => {
            set_error(ERROR_CLASS_DOES_NOT_EXIST);
            0
        }
    }
}

#[no_mangle]
pub unsafe extern "system" fn UnregisterClassW(name: *const u16, _instance: u64) -> Bool {
    unregister(unsafe { name_w(name) })
}

#[no_mangle]
pub unsafe extern "system" fn UnregisterClassA(name: *const u8, _instance: u64) -> Bool {
    unregister(unsafe { name_a(name) })
}

/// GetClassInfo(Ex): fill a WNDCLASS(EX) from a registered class.
unsafe fn class_info(name: Name, out: *mut u8, ex: bool) -> u16 {
    let s = state();
    let Some(i) = find_class(&s, &name) else {
        set_error(ERROR_CLASS_DOES_NOT_EXIST);
        return 0;
    };
    let c = &s.classes[i];
    let base = out;
    let wr = |off: usize, v: u64| unsafe { core::ptr::write_unaligned(base.add(off) as *mut u64, v) };
    let wr32 = |off: usize, v: u32| unsafe { core::ptr::write_unaligned(base.add(off) as *mut u32, v) };
    wr32(if ex { 4 } else { 0 }, c.style);
    wr(8, c.proc as u64);
    wr32(16, c.cls_extra.len() as u32);
    wr32(20, c.wnd_extra as u32);
    wr(24, c.instance);
    wr(32, c.icon);
    wr(40, c.cursor);
    wr(48, c.brush);
    wr(56, c.menu_name);
    // The name pointer stays the caller's.
    if ex {
        wr(72, c.icon_sm);
    }
    c.atom
}

#[no_mangle]
pub unsafe extern "system" fn GetClassInfoExW(_instance: u64, name: *const u16, out: *mut u8) -> Bool {
    (unsafe { class_info(name_w(name), out, true) } != 0) as Bool
}

#[no_mangle]
pub unsafe extern "system" fn GetClassInfoExA(_instance: u64, name: *const u8, out: *mut u8) -> Bool {
    (unsafe { class_info(name_a(name), out, true) } != 0) as Bool
}

#[no_mangle]
pub unsafe extern "system" fn GetClassInfoW(_instance: u64, name: *const u16, out: *mut u8) -> Bool {
    (unsafe { class_info(name_w(name), out, false) } != 0) as Bool
}

#[no_mangle]
pub unsafe extern "system" fn GetClassInfoA(_instance: u64, name: *const u8, out: *mut u8) -> Bool {
    (unsafe { class_info(name_a(name), out, false) } != 0) as Bool
}
