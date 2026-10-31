//! The keyboard: each thread's view of the keys (GetKeyState, from the
//! messages it has read), the live state (GetAsyncKeyState, the kernel's),
//! virtual-key translations, and the caret.

use nt_abi::Msg;

use crate::dllrt::Lock;
use crate::{state, Bool, Hwnd};

/// The keys as of the last message read: bit 7 down, bit 0 toggled.
static KEYS: Lock<[u8; 256]> = Lock::new([0; 256]);

const VK_LBUTTON: usize = 0x01;
const VK_RBUTTON: usize = 0x02;
const VK_SHIFT: usize = 0x10;
const VK_CONTROL: usize = 0x11;
const VK_MENU: usize = 0x12;

/// A message was read: keep the key state it implies.
pub fn track(m: &Msg) {
    let down = match m.message {
        0x0100 | 0x0104 => true,  // WM_KEYDOWN, WM_SYSKEYDOWN
        0x0101 | 0x0105 => false, // WM_KEYUP, WM_SYSKEYUP
        0x0201 | 0x0202 => return set(VK_LBUTTON, m.message == 0x0201),
        0x0204 | 0x0205 => return set(VK_RBUTTON, m.message == 0x0204),
        _ => return,
    };
    let vk = m.wparam as usize & 0xFF;
    set(vk, down);
    // The left or right Shift, Ctrl or Alt.
    let (scan, ext) = ((m.lparam >> 16) & 0xFF, (m.lparam >> 24) & 1 != 0);
    let side = match vk {
        VK_SHIFT => Some(if scan == 0x36 { 0xA1 } else { 0xA0 }),
        VK_CONTROL => Some(if ext { 0xA3 } else { 0xA2 }),
        VK_MENU => Some(if ext { 0xA5 } else { 0xA4 }),
        _ => None,
    };
    if let Some(s) = side {
        set(s, down);
    }
}

fn set(vk: usize, down: bool) {
    let mut k = KEYS.lock();
    let old = k[vk];
    let toggled = if down && old & 0x80 == 0 { (old ^ 1) & 1 } else { old & 1 };
    k[vk] = toggled | if down { 0x80 } else { 0 };
}

#[no_mangle]
pub extern "system" fn GetKeyState(vk: i32) -> i16 {
    let k = KEYS.lock()[vk as usize & 0xFF];
    (if k & 0x80 != 0 { 0x8000u16 as i16 } else { 0 }) | (k & 1) as i16
}

#[no_mangle]
pub unsafe extern "system" fn GetKeyboardState(out: *mut u8) -> Bool {
    if out.is_null() {
        return 0;
    }
    let k = KEYS.lock();
    unsafe { core::ptr::copy_nonoverlapping(k.as_ptr(), out, 256) };
    1
}

#[no_mangle]
pub unsafe extern "system" fn SetKeyboardState(keys: *const u8) -> Bool {
    if keys.is_null() {
        return 0;
    }
    let mut k = KEYS.lock();
    unsafe { core::ptr::copy_nonoverlapping(keys, k.as_mut_ptr(), 256) };
    1
}

/// The key's state right now (the kernel's).
#[no_mangle]
pub extern "system" fn GetAsyncKeyState(vk: i32) -> i16 {
    let mut v = [0i32; 2];
    unsafe { crate::VeilUserQuery(2 | ((vk as u32 & 0xFF) << 8), &mut v) };
    if v[0] != 0 { 0x8000u16 as i16 } else { 0 }
}

/// Virtual key and scan code of each letter key (set 1), for MapVirtualKey.
const SCANS: [(u8, u8); 47] = [
    (b'A', 0x1E), (b'B', 0x30), (b'C', 0x2E), (b'D', 0x20), (b'E', 0x12), (b'F', 0x21), (b'G', 0x22), (b'H', 0x23),
    (b'I', 0x17), (b'J', 0x24), (b'K', 0x25), (b'L', 0x26), (b'M', 0x32), (b'N', 0x31), (b'O', 0x18), (b'P', 0x19),
    (b'Q', 0x10), (b'R', 0x13), (b'S', 0x1F), (b'T', 0x14), (b'U', 0x16), (b'V', 0x2F), (b'W', 0x11), (b'X', 0x2D),
    (b'Y', 0x15), (b'Z', 0x2C), (b'1', 0x02), (b'2', 0x03), (b'3', 0x04), (b'4', 0x05), (b'5', 0x06), (b'6', 0x07),
    (b'7', 0x08), (b'8', 0x09), (b'9', 0x0A), (b'0', 0x0B), (0x0D, 0x1C), (0x1B, 0x01), (0x08, 0x0E), (0x09, 0x0F),
    (0x20, 0x39), (0x10, 0x2A), (0x11, 0x1D), (0x12, 0x38), (0x25, 0x4B), (0x26, 0x48), (0x27, 0x4D),
];

/// MAPVK_VK_TO_VSC (0), MAPVK_VSC_TO_VK (1), MAPVK_VK_TO_CHAR (2),
/// MAPVK_VSC_TO_VK_EX (3).
#[no_mangle]
pub extern "system" fn MapVirtualKeyW(code: u32, kind: u32) -> u32 {
    match kind {
        0 => SCANS.iter().find(|e| e.0 as u32 == code).map_or(0, |e| e.1 as u32),
        1 | 3 => SCANS.iter().find(|e| e.1 as u32 == code).map_or(0, |e| e.0 as u32),
        2 => match code {
            0x30..=0x39 | 0x41..=0x5A | 0x20 | 0x08 | 0x09 | 0x0D | 0x1B => code,
            0xBA => ';' as u32,
            0xBB => '=' as u32,
            0xBC => ',' as u32,
            0xBD => '-' as u32,
            0xBE => '.' as u32,
            0xBF => '/' as u32,
            0xC0 => '`' as u32,
            0xDB => '[' as u32,
            0xDC => '\\' as u32,
            0xDD => ']' as u32,
            0xDE => '\'' as u32,
            _ => 0,
        },
        _ => 0,
    }
}

#[no_mangle]
pub extern "system" fn MapVirtualKeyA(code: u32, kind: u32) -> u32 {
    MapVirtualKeyW(code, kind)
}

/// The key and shift state that type `ch` (US layout): low byte the
/// virtual key, high byte 1 for Shift. -1 if none does.
#[no_mangle]
pub extern "system" fn VkKeyScanW(ch: u16) -> i16 {
    let c = ch as u8 as char;
    if ch > 0x7F {
        return -1;
    }
    let shifted = "~!@#$%^&*()_+{}|:\"<>?";
    let plain = "`1234567890-=[]\\;',./";
    let vk = |c: char| -> i16 {
        match c {
            'a'..='z' => (c as u8 - 32) as i16,
            '0'..='9' => c as i16,
            ' ' => 0x20,
            '\r' | '\n' => 0x0D,
            '\t' => 0x09,
            '\u{8}' => 0x08,
            '\u{1b}' => 0x1B,
            '`' => 0xC0,
            '-' => 0xBD,
            '=' => 0xBB,
            '[' => 0xDB,
            ']' => 0xDD,
            '\\' => 0xDC,
            ';' => 0xBA,
            '\'' => 0xDE,
            ',' => 0xBC,
            '.' => 0xBE,
            '/' => 0xBF,
            _ => -1,
        }
    };
    if c.is_ascii_uppercase() {
        return 0x100 | (c as u8) as i16;
    }
    if let Some(i) = shifted.find(c) {
        return 0x100 | vk(plain.as_bytes()[i] as char);
    }
    vk(c)
}

#[no_mangle]
pub extern "system" fn VkKeyScanA(ch: u8) -> i16 {
    VkKeyScanW(ch as u16)
}

// -----| the caret |-----

struct Caret {
    hwnd: Hwnd,
    w: i32,
    h: i32,
    x: i32,
    y: i32,
    /// ShowCaret count minus HideCaret count (shown when > 0).
    shown: i32,
    /// Drawn (inverted) on the window now.
    drawn: bool,
}

static CARET: Lock<Option<Caret>> = Lock::new(None);

/// Invert the caret's rectangle in its window.
fn invert(c: &Caret) {
    let hdc = crate::paint::GetDC(c.hwnd);
    unsafe { crate::PatBlt(hdc, c.x, c.y, c.w, c.h, 0x0055_0009) };
    crate::paint::ReleaseDC(c.hwnd, hdc);
}

fn redraw(want: bool) {
    let mut g = CARET.lock();
    let Some(c) = g.as_mut() else { return };
    if c.drawn != want {
        c.drawn = want;
        invert(c);
    }
}

fn update() {
    let want = CARET.lock().as_ref().is_some_and(|c| c.shown > 0);
    redraw(want);
}

#[no_mangle]
pub extern "system" fn CreateCaret(hwnd: Hwnd, _bitmap: u64, w: i32, h: i32) -> Bool {
    DestroyCaret();
    if state().get(hwnd).is_none() {
        return 0;
    }
    *CARET.lock() = Some(Caret { hwnd, w: if w == 0 { 1 } else { w }, h: if h == 0 { 16 } else { h }, x: 0, y: 0, shown: 0, drawn: false });
    1
}

#[no_mangle]
pub extern "system" fn DestroyCaret() -> Bool {
    redraw(false);
    CARET.lock().take().is_some() as Bool
}

#[no_mangle]
pub extern "system" fn ShowCaret(hwnd: Hwnd) -> Bool {
    if let Some(c) = CARET.lock().as_mut().filter(|c| hwnd == 0 || c.hwnd == hwnd) {
        c.shown = (c.shown + 1).min(1);
    }
    update();
    1
}

#[no_mangle]
pub extern "system" fn HideCaret(hwnd: Hwnd) -> Bool {
    if let Some(c) = CARET.lock().as_mut().filter(|c| hwnd == 0 || c.hwnd == hwnd) {
        c.shown -= 1;
    }
    update();
    1
}

#[no_mangle]
pub extern "system" fn SetCaretPos(x: i32, y: i32) -> Bool {
    redraw(false);
    if let Some(c) = CARET.lock().as_mut() {
        c.x = x;
        c.y = y;
    }
    update();
    1
}

#[no_mangle]
pub extern "system" fn GetCaretPos(out: *mut [i32; 2]) -> Bool {
    let g = CARET.lock();
    match (g.as_ref(), out.is_null()) {
        (Some(c), false) => {
            unsafe { *out = [c.x, c.y] };
            1
        }
        _ => 0,
    }
}

#[no_mangle]
pub extern "system" fn GetCaretBlinkTime() -> u32 {
    530
}

#[no_mangle]
pub extern "system" fn SetCaretBlinkTime(_ms: u32) -> Bool {
    1
}

/// BeginPaint hides the caret of the window it paints (painting would
/// leave half an inversion), and EndPaint brings it back.
pub fn hide_caret_for_paint(hwnd: Hwnd) {
    if CARET.lock().as_ref().is_some_and(|c| c.hwnd == hwnd && c.drawn) {
        // Painting draws over it: it's gone without inverting back.
        if let Some(c) = CARET.lock().as_mut() {
            c.drawn = false;
        }
    }
}

pub fn show_caret_after_paint(hwnd: Hwnd) {
    if CARET.lock().as_ref().is_some_and(|c| c.hwnd == hwnd) {
        update();
    }
}
