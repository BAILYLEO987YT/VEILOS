//! The status bar ("msctls_statusbar32"): a strip along the bottom of its
//! parent (it moves there on WM_SIZE), split into parts with text (a tab
//! centres what follows it, two tabs right-align it), an optional size
//! grip, and a simple mode with one part.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use crate::dllrt::Lock;
use crate::{Bool, Hwnd};

const SBARS_SIZEGRIP: u32 = 0x0100;
const CCS_TOP: u32 = 0x0001;
const SBT_NOBORDERS: u32 = 0x0100;
const SBT_POPOUT: u32 = 0x0200;

struct Bar {
    /// Right edges (-1: to the end) and each part's text and type.
    edges: Vec<i32>,
    texts: Vec<(Vec<u16>, u32)>,
    simple: bool,
    simple_text: (Vec<u16>, u32),
    font: u64,
    min_h: i32,
    icons: Vec<u64>,
}

static BARS: Lock<BTreeMap<Hwnd, Bar>> = Lock::new(BTreeMap::new());

fn with<R>(hwnd: Hwnd, f: impl FnOnce(&mut Bar) -> R) -> Option<R> {
    BARS.lock().get_mut(&hwnd).map(f)
}

pub fn register_class() {
    crate::register("msctls_statusbar32", status_proc as *const () as usize, true);
}

/// The bar's height for its font.
fn height(hwnd: Hwnd) -> i32 {
    let font = crate::font_or_default(with(hwnd, |b| b.font).unwrap_or(0));
    let hdc = unsafe { crate::GetDC(hwnd) };
    let old = unsafe { crate::SelectObject(hdc, font) };
    let mut tm = [0u8; 60];
    unsafe { crate::GetTextMetricsW(hdc, tm.as_mut_ptr()) };
    unsafe {
        crate::SelectObject(hdc, old);
        crate::ReleaseDC(hwnd, hdc);
    }
    let th = i32::from_le_bytes(tm[0..4].try_into().unwrap());
    (th + 8).max(with(hwnd, |b| b.min_h).unwrap_or(0))
}

/// Along the bottom (or top) of the parent, its full width.
fn place(hwnd: Hwnd) {
    let parent = unsafe { crate::GetParent(hwnd) };
    let mut r = [0i32; 4];
    unsafe { crate::GetClientRect(parent, &mut r) };
    let h = height(hwnd);
    let style = unsafe { crate::GetWindowLongPtrW(hwnd, -16) } as u32;
    let y = if style & CCS_TOP != 0 { 0 } else { r[3] - h };
    unsafe { crate::SetWindowPos(hwnd, 0, 0, y, r[2], h, 0x4 | 0x10) };
}

/// Each part's rectangle (client coordinates).
fn part_rects(hwnd: Hwnd) -> Vec<[i32; 4]> {
    let mut r = [0i32; 4];
    unsafe { crate::GetClientRect(hwnd, &mut r) };
    let (edges, simple) = with(hwnd, |b| (b.edges.clone(), b.simple)).unwrap_or_default();
    if simple || edges.is_empty() {
        return alloc::vec![[0, 2, r[2], r[3]]];
    }
    let mut left = 0;
    edges
        .iter()
        .map(|&e| {
            let right = if e < 0 || e > r[2] { r[2] } else { e };
            let pr = [left, 2, right, r[3]];
            left = right + 2;
            pr
        })
        .collect()
}

fn paint(hwnd: Hwnd, hdc: u64) {
    let mut r = [0i32; 4];
    unsafe { crate::GetClientRect(hwnd, &mut r) };
    unsafe { crate::FillRect(hdc, &r, crate::GetSysColorBrush(15)) };
    let Some((texts, simple, simple_text, font, icons)) = with(hwnd, |b| (b.texts.clone(), b.simple, b.simple_text.clone(), b.font, b.icons.clone())) else { return };
    let old = unsafe { crate::SelectObject(hdc, crate::font_or_default(font)) };
    unsafe { crate::SetBkMode(hdc, 1) };
    unsafe { crate::SetTextColor(hdc, crate::GetSysColor(18)) };
    let style = unsafe { crate::GetWindowLongPtrW(hwnd, -16) } as u32;
    let grip = style & SBARS_SIZEGRIP != 0 && unsafe { crate::IsZoomed(crate::GetParent(hwnd)) } == 0;
    for (i, pr) in part_rects(hwnd).iter().enumerate() {
        let (text, kind) = if simple { simple_text.clone() } else { texts.get(i).cloned().unwrap_or_default() };
        let mut pr = *pr;
        if grip && pr[2] >= r[2] {
            pr[2] -= 14;
        }
        if kind & SBT_NOBORDERS == 0 {
            // Sunken (popped out with SBT_POPOUT).
            let (tl, br) = if kind & SBT_POPOUT != 0 { (20, 16) } else { (16, 20) };
            unsafe {
                crate::FillRect(hdc, &[pr[0], pr[1], pr[2], pr[1] + 1], crate::GetSysColorBrush(tl));
                crate::FillRect(hdc, &[pr[0], pr[1], pr[0] + 1, pr[3]], crate::GetSysColorBrush(tl));
                crate::FillRect(hdc, &[pr[0], pr[3] - 1, pr[2], pr[3]], crate::GetSysColorBrush(br));
                crate::FillRect(hdc, &[pr[2] - 1, pr[1], pr[2], pr[3]], crate::GetSysColorBrush(br));
            }
        }
        let mut x = pr[0] + 4;
        if let Some(&icon) = icons.get(i).filter(|&&ic| ic != 0) {
            #[link(name = "user32", kind = "raw-dylib")]
            unsafe extern "system" {
                fn DrawIconEx(hdc: u64, x: i32, y: i32, icon: u64, cx: i32, cy: i32, step: u32, brush: u64, flags: u32) -> Bool;
            }
            unsafe { DrawIconEx(hdc, x, (pr[1] + pr[3] - 16) / 2, icon, 16, 16, 0, 0, 3) };
            x += 20;
        }
        // "left\tcentre\tright"
        let mut pieces = text.split(|&c| c == 9);
        for flag in [0x0u32, 0x1, 0x2] {
            let Some(p) = pieces.next() else { break };
            let mut t = p.to_vec();
            let mut tr = [x, pr[1], pr[2] - 4, pr[3]];
            unsafe { crate::DrawTextW(hdc, t.as_mut_ptr(), t.len() as i32, &mut tr, 0x24 | 0x800 | flag) };
        }
    }
    if grip {
        // Three diagonal ridges.
        for k in 0..3 {
            for d in 0..(3 + 4 * k) {
                let (x, y) = (r[2] - 2 - d, r[3] - 3 - (3 + 4 * k - 1 - d));
                if d % 4 != 3 {
                    unsafe { crate::FillRect(hdc, &[x, y, x + 1, y + 1], crate::GetSysColorBrush(if d % 4 == 0 { 20 } else { 16 })) };
                }
            }
        }
    }
    unsafe { crate::SelectObject(hdc, old) };
}

fn set_text(hwnd: Hwnd, part: u32, text: Vec<u16>, kind: u32) -> Bool {
    let simple_part = part == 255;
    let r = with(hwnd, |b| {
        if simple_part {
            b.simple_text = (text, kind);
        } else {
            let i = part as usize;
            if i >= 256 {
                return false;
            }
            if b.texts.len() <= i {
                b.texts.resize(i + 1, (Vec::new(), 0));
            }
            b.texts[i] = (text, kind);
        }
        true
    })
    .unwrap_or(false);
    unsafe { crate::InvalidateRect(hwnd, core::ptr::null(), 1) };
    r as Bool
}

fn get_text(hwnd: Hwnd, part: usize) -> (Vec<u16>, u32) {
    with(hwnd, |b| if b.simple { b.simple_text.clone() } else { b.texts.get(part).cloned().unwrap_or_default() }).unwrap_or_default()
}

unsafe extern "system" fn status_proc(hwnd: Hwnd, msg: u32, wp: u64, lp: i64) -> i64 {
    match msg {
        0x0001 => {
            BARS.lock().insert(hwnd, Bar { edges: alloc::vec![-1], texts: Vec::new(), simple: false, simple_text: (Vec::new(), 0), font: 0, min_h: 0, icons: Vec::new() });
            // The window's text is part 0's.
            if lp != 0 {
                let name = unsafe { *((lp as usize + 56) as *const u64) };
                if name >> 16 != 0 {
                    let t: Vec<u16> = (0..).map(|i| unsafe { *(name as *const u16).add(i) }).take_while(|&c| c != 0).collect();
                    set_text(hwnd, 0, t, 0);
                }
            }
            place(hwnd);
            0
        }
        0x0082 => {
            BARS.lock().remove(&hwnd);
            unsafe { crate::DefWindowProcW(hwnd, msg, wp, lp) }
        }
        // WM_SIZE (forwarded by the parent): back along its edge.
        0x0005 => {
            place(hwnd);
            unsafe { crate::InvalidateRect(hwnd, core::ptr::null(), 1) };
            0
        }
        0x000F => {
            let mut ps = [0u8; 72];
            let hdc = unsafe { crate::BeginPaint(hwnd, ps.as_mut_ptr()) };
            paint(hwnd, hdc);
            unsafe { crate::EndPaint(hwnd, ps.as_ptr()) };
            0
        }
        0x0014 => 1,
        0x0030 => {
            with(hwnd, |b| b.font = wp);
            place(hwnd);
            0
        }
        0x0031 => with(hwnd, |b| b.font as i64).unwrap_or(0),
        // WM_SETTEXT / WM_GETTEXT / WM_GETTEXTLENGTH: part 0.
        0x000C => {
            let t = if lp == 0 { Vec::new() } else { (0..).map(|i| unsafe { *(lp as *const u16).add(i) }).take_while(|&c| c != 0).collect() };
            set_text(hwnd, 0, t, 0) as i64
        }
        0x000D => {
            let (t, _) = get_text(hwnd, 0);
            let n = t.len().min((wp as usize).saturating_sub(1));
            if lp != 0 && wp > 0 {
                unsafe {
                    core::ptr::copy_nonoverlapping(t.as_ptr(), lp as *mut u16, n);
                    *(lp as *mut u16).add(n) = 0;
                }
            }
            n as i64
        }
        0x000E => get_text(hwnd, 0).0.len() as i64,
        // SB_SETTEXTA / SB_SETTEXTW: part in the low byte, type above.
        0x0401 | 0x040B => {
            let t: Vec<u16> = if lp == 0 {
                Vec::new()
            } else if msg == 0x040B {
                (0..).map(|i| unsafe { *(lp as *const u16).add(i) }).take_while(|&c| c != 0).collect()
            } else {
                (0..).map(|i| unsafe { *(lp as *const u8).add(i) } as u16).take_while(|&c| c != 0).collect()
            };
            set_text(hwnd, wp as u32 & 0xFF, t, wp as u32 & 0xFF00) as i64
        }
        // SB_GETTEXTA / SB_GETTEXTW: length and type.
        0x0402 | 0x040D => {
            let (t, kind) = get_text(hwnd, wp as usize & 0xFF);
            if lp != 0 {
                unsafe {
                    if msg == 0x040D {
                        core::ptr::copy_nonoverlapping(t.as_ptr(), lp as *mut u16, t.len());
                        *(lp as *mut u16).add(t.len()) = 0;
                    } else {
                        for (i, c) in t.iter().enumerate() {
                            *(lp as *mut u8).add(i) = *c as u8;
                        }
                        *(lp as *mut u8).add(t.len()) = 0;
                    }
                }
            }
            ((kind as i64) << 16) | t.len() as i64
        }
        // SB_GETTEXTLENGTHA / W
        0x0403 | 0x040C => {
            let (t, kind) = get_text(hwnd, wp as usize & 0xFF);
            ((kind as i64) << 16) | t.len() as i64
        }
        // SB_SETPARTS
        0x0404 => {
            let n = (wp as usize).min(256);
            if lp == 0 || n == 0 {
                return 0;
            }
            let edges: Vec<i32> = (0..n).map(|i| unsafe { *(lp as *const i32).add(i) }).collect();
            with(hwnd, |b| {
                b.edges = edges;
                b.texts.truncate(n);
            });
            unsafe { crate::InvalidateRect(hwnd, core::ptr::null(), 1) };
            1
        }
        // SB_GETPARTS
        0x0406 => {
            let edges = with(hwnd, |b| b.edges.clone()).unwrap_or_default();
            if lp != 0 {
                for (i, e) in edges.iter().take(wp as usize).enumerate() {
                    unsafe { *(lp as *mut i32).add(i) = *e };
                }
            }
            edges.len() as i64
        }
        // SB_GETBORDERS: horizontal, vertical, between parts.
        0x0407 => {
            if lp != 0 {
                unsafe { *(lp as *mut [i32; 3]) = [0, 2, 2] };
            }
            1
        }
        // SB_SETMINHEIGHT
        0x0408 => {
            with(hwnd, |b| b.min_h = wp as i32);
            0
        }
        // SB_SIMPLE
        0x0409 => {
            with(hwnd, |b| b.simple = wp != 0);
            unsafe { crate::InvalidateRect(hwnd, core::ptr::null(), 1) };
            0
        }
        // SB_GETRECT
        0x040A => {
            let rects = part_rects(hwnd);
            match rects.get(wp as usize) {
                Some(r) if lp != 0 => {
                    unsafe { *(lp as *mut [i32; 4]) = *r };
                    1
                }
                _ => 0,
            }
        }
        // SB_ISSIMPLE
        0x040E => with(hwnd, |b| b.simple as i64).unwrap_or(0),
        // SB_SETICON
        0x040F => {
            with(hwnd, |b| {
                let i = wp as usize;
                if b.icons.len() <= i {
                    b.icons.resize(i + 1, 0);
                }
                b.icons[i] = lp as u64;
            });
            unsafe { crate::InvalidateRect(hwnd, core::ptr::null(), 1) };
            1
        }
        // SB_SETBKCOLOR
        0x2001 => 0,
        _ => unsafe { crate::DefWindowProcW(hwnd, msg, wp, lp) },
    }
}

/// A status bar for `parent`, with `text` in its first part.
#[no_mangle]
pub unsafe extern "system" fn CreateStatusWindowW(style: i32, text: *const u16, parent: Hwnd, id: u32) -> Hwnd {
    let class: Vec<u16> = "msctls_statusbar32\0".encode_utf16().collect();
    unsafe { crate::CreateWindowExW(0, class.as_ptr(), text, style as u32 | 0x4000_0000, 0, 0, 0, 0, parent, id as u64, 0, 0) }
}

#[no_mangle]
pub unsafe extern "system" fn CreateStatusWindowA(style: i32, text: *const u8, parent: Hwnd, id: u32) -> Hwnd {
    let t: Vec<u16> = if text.is_null() {
        alloc::vec![0]
    } else {
        (0..).map(|i| unsafe { *text.add(i) } as u16).take_while(|&c| c != 0).chain(core::iter::once(0)).collect()
    };
    unsafe { CreateStatusWindowW(style, t.as_ptr(), parent, id) }
}

#[no_mangle]
pub unsafe extern "system" fn DrawStatusTextW(hdc: u64, r: *const [i32; 4], text: *const u16, _flags: u32) {
    if r.is_null() {
        return;
    }
    let mut rr = unsafe { *r };
    unsafe { crate::FillRect(hdc, &rr, crate::GetSysColorBrush(15)) };
    if !text.is_null() {
        let mut t: Vec<u16> = (0..).map(|i| unsafe { *text.add(i) }).take_while(|&c| c != 0).collect();
        rr[0] += 4;
        unsafe {
            crate::SetBkMode(hdc, 1);
            crate::DrawTextW(hdc, t.as_mut_ptr(), t.len() as i32, &mut rr, 0x24 | 0x800);
        }
    }
}
