// =============================================================================
// VEILOS: USB keyboards and mice (HID boot protocol)
// =============================================================================
//
// A boot keyboard reports [modifiers, reserved, up to six key usages]; a boot
// mouse [buttons, dx, dy] and, on most mice, a wheel byte. Keyboard reports
// become the PS/2 set-1 scancodes the rest of VEILOS already decodes
// (task::keyboard), make codes for keys newly down and break codes for keys
// released; mouse reports go into the same state the PS/2 mouse fills.

/// HID usage (0x04..=0x65) to set-1 make code; 0xE0xx marks an extended key,
/// 0 a key with no set-1 code here.
const USAGE_TO_SET1: [u16; 0x66] = [
    0, 0, 0, 0, // 0x00-0x03: no key, errors
    0x1E, 0x30, 0x2E, 0x20, 0x12, 0x21, 0x22, 0x23, 0x17, 0x24, 0x25, 0x26, 0x32, // a-m
    0x31, 0x18, 0x19, 0x10, 0x13, 0x1F, 0x14, 0x16, 0x2F, 0x11, 0x2D, 0x15, 0x2C, // n-z
    0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B, // 1-0
    0x1C, 0x01, 0x0E, 0x0F, 0x39, // enter, esc, backspace, tab, space
    0x0C, 0x0D, 0x1A, 0x1B, 0x2B, 0x2B, 0x27, 0x28, 0x29, 0x33, 0x34, 0x35, // - = [ ] \ # ; ' ` , . /
    0x3A, // caps lock
    0x3B, 0x3C, 0x3D, 0x3E, 0x3F, 0x40, 0x41, 0x42, 0x43, 0x44, 0x57, 0x58, // F1-F12
    0xE037, 0x46, 0, // print screen, scroll lock, pause
    0xE052, 0xE047, 0xE049, 0xE053, 0xE04F, 0xE051, // insert home pgup delete end pgdn
    0xE04D, 0xE04B, 0xE050, 0xE048, // right left down up
    0x45, 0xE035, 0x37, 0x4A, 0x4E, 0xE01C, // num lock, keypad / * - + enter
    0x4F, 0x50, 0x51, 0x4B, 0x4C, 0x4D, 0x47, 0x48, 0x49, 0x52, 0x53, // keypad 1-9 0 .
    0x56, 0xE05D, // non-US \, application
];

/// Modifier bits 0..7 (left ctrl, shift, alt, GUI; right ctrl, shift, alt, GUI).
const MODIFIER_SET1: [u16; 8] = [0x1D, 0x2A, 0x38, 0xE05B, 0xE01D, 0x36, 0xE038, 0xE05C];

fn send(code: u16, release: bool) {
    if code == 0 {
        return;
    }
    if code & 0xFF00 == 0xE000 {
        crate::task::keyboard::add_scancode(0xE0);
    }
    let make = code as u8;
    crate::task::keyboard::add_scancode(if release { make | 0x80 } else { make });
}

fn usage_code(usage: u8) -> u16 {
    USAGE_TO_SET1.get(usage as usize).copied().unwrap_or(0)
}

/// A boot keyboard report; `last` is the previous one.
pub fn keyboard_report(report: &[u8], last: &mut [u8; 8]) {
    if report.len() < 3 {
        return;
    }
    let mut now = [0u8; 8];
    let n = report.len().min(8);
    now[..n].copy_from_slice(&report[..n]);
    // All keys reporting 0x01 (rollover error): nothing changed that can be known.
    if now[2..].iter().all(|&k| k == 1) {
        return;
    }
    let (old_mod, new_mod) = (last[0], now[0]);
    for bit in 0..8 {
        let (was, is) = (old_mod & (1 << bit) != 0, new_mod & (1 << bit) != 0);
        if was != is {
            send(MODIFIER_SET1[bit], was);
        }
    }
    for &k in &last[2..] {
        if k > 3 && !now[2..].contains(&k) {
            send(usage_code(k), true);
        }
    }
    for &k in &now[2..] {
        if k > 3 && !last[2..].contains(&k) {
            send(usage_code(k), false);
        }
    }
    *last = now;
}

/// A boot mouse report: buttons, dx, dy (down is positive), wheel if sent
/// (up is positive).
pub fn mouse_report(report: &[u8]) {
    if report.len() < 3 {
        return;
    }
    let buttons = report[0];
    let dx = report[1] as i8 as i32;
    let dy = report[2] as i8 as i32;
    let wheel = report.get(3).map_or(0, |&w| w as i8 as i32);
    // MouseState's wheel counts notches towards the user (scrolling down).
    crate::task::mouse::add_motion(dx, dy, buttons & 1 != 0, buttons & 2 != 0, -wheel);
}
