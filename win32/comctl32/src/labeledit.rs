//! In-place label editing for the list and tree views: an edit box over the
//! label. Return keeps the text, Escape or losing the focus ends it too (the
//! focus keeps it, as on Windows), and the owner gets `DONE` with the result.

use crate::Hwnd;

/// Sent to the owner when editing ends: wParam 1 to keep the text.
pub const DONE: u32 = 0x7F10;

const VK_RETURN: u64 = 0x0D;
const VK_ESCAPE: u64 = 0x1B;

unsafe extern "system" fn edit_subclass(hwnd: Hwnd, msg: u32, wp: u64, lp: i64, _id: u64, owner: u64) -> i64 {
    match msg {
        0x0087 => return 0x4 | crate::DefSubclassProc(hwnd, msg, wp, lp), // WM_GETDLGCODE: DLGC_WANTALLKEYS
        0x0100 if wp == VK_RETURN || wp == VK_ESCAPE => {
            unsafe { crate::PostMessageW(owner, DONE, (wp == VK_RETURN) as u64, 0) };
            return 0;
        }
        0x0102 if wp == VK_RETURN || wp == VK_ESCAPE => return 0,
        0x0008 => {
            // WM_KILLFOCUS
            unsafe { crate::PostMessageW(owner, DONE, 1, 0) };
        }
        _ => {}
    }
    crate::DefSubclassProc(hwnd, msg, wp, lp)
}

/// An edit box with `text` selected, focused, in `owner` at `r`.
pub fn start(owner: Hwnd, r: [i32; 4], text: &[u16], font: u64) -> Hwnd {
    let mut t = text.to_vec();
    t.push(0);
    let class = crate::wide("EDIT");
    // WS_CHILD | WS_VISIBLE | WS_BORDER | ES_AUTOHSCROLL
    let e = unsafe {
        crate::CreateWindowExW(0, class.as_ptr(), t.as_ptr(), 0x5080_0080, r[0], r[1], r[2] - r[0], r[3] - r[1], owner, 0, 0, 0)
    };
    if e == 0 {
        return 0;
    }
    unsafe {
        crate::SendMessageW(e, 0x0030, crate::font_or_default(font), 0);
        crate::SendMessageW(e, 0x00B1, 0, -1);
    }
    crate::SetWindowSubclass(e, edit_subclass as *const () as usize, 1, owner);
    unsafe { crate::SetFocus(e) };
    e
}

/// The edit box's text.
pub fn text(e: Hwnd) -> alloc::vec::Vec<u16> {
    let n = unsafe { crate::SendMessageW(e, 0x000E, 0, 0) }.max(0) as usize;
    let mut v = alloc::vec![0u16; n + 1];
    unsafe { crate::SendMessageW(e, 0x000D, n as u64 + 1, v.as_mut_ptr() as i64) };
    v.truncate(n);
    v
}
