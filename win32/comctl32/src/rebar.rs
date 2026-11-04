//! The rebar ("ReBarWindow32"): bands in rows, each with a gripper, optional
//! text, and a child window (usually a toolbar) it keeps in place. Dragging a
//! gripper moves the boundary with the band before it.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use crate::dllrt::Lock;
use crate::Hwnd;

const WM_USER: u32 = 0x0400;
const RB_INSERTBANDA: u32 = WM_USER + 1;
const RB_DELETEBAND: u32 = WM_USER + 2;
const RB_GETBARINFO: u32 = WM_USER + 3;
const RB_SETBARINFO: u32 = WM_USER + 4;
const RB_SETBANDINFOA: u32 = WM_USER + 6;
const RB_SETPARENT: u32 = WM_USER + 7;
const RB_HITTEST: u32 = WM_USER + 8;
const RB_GETRECT: u32 = WM_USER + 9;
const RB_INSERTBANDW: u32 = WM_USER + 10;
const RB_SETBANDINFOW: u32 = WM_USER + 11;
const RB_GETBANDCOUNT: u32 = WM_USER + 12;
const RB_GETROWCOUNT: u32 = WM_USER + 13;
const RB_GETROWHEIGHT: u32 = WM_USER + 14;
const RB_IDTOINDEX: u32 = WM_USER + 16;
const RB_SETBKCOLOR: u32 = WM_USER + 19;
const RB_GETBKCOLOR: u32 = WM_USER + 20;
const RB_SIZETORECT: u32 = WM_USER + 23;
const RB_GETBARHEIGHT: u32 = WM_USER + 27;
const RB_GETBANDINFOW: u32 = WM_USER + 28;
const RB_GETBANDINFOA: u32 = WM_USER + 29;
const RB_MINIMIZEBAND: u32 = WM_USER + 30;
const RB_MAXIMIZEBAND: u32 = WM_USER + 31;
const RB_GETBANDBORDERS: u32 = WM_USER + 34;
const RB_SHOWBAND: u32 = WM_USER + 35;

const RBBIM_STYLE: u32 = 0x1;
const RBBIM_COLORS: u32 = 0x2;
const RBBIM_TEXT: u32 = 0x4;
const RBBIM_IMAGE: u32 = 0x8;
const RBBIM_CHILD: u32 = 0x10;
const RBBIM_CHILDSIZE: u32 = 0x20;
const RBBIM_SIZE: u32 = 0x40;
const RBBIM_ID: u32 = 0x100;
const RBBIM_IDEALSIZE: u32 = 0x200;
const RBBIM_LPARAM: u32 = 0x400;

const RBBS_BREAK: u32 = 0x1;
const RBBS_FIXEDSIZE: u32 = 0x2;
const RBBS_HIDDEN: u32 = 0x8;
const RBBS_NOGRIPPER: u32 = 0x100;

const CCS_NORESIZE: u32 = 0x4;
const CCS_NOPARENTALIGN: u32 = 0x8;
const CCS_BOTTOM: u32 = 0x3;

const RBN_HEIGHTCHANGE: i32 = -831;
const RBN_BEGINDRAG: i32 = -844;
const RBN_ENDDRAG: i32 = -845;

const GRIPPER: i32 = 10;
const CLR_DEFAULT: u32 = 0xFF00_0000;
const CLR_NONE: u32 = 0xFFFF_FFFF;

#[derive(Clone)]
struct Band {
    style: u32,
    fore: u32,
    back: u32,
    text: Vec<u16>,
    image: i32,
    child: Hwnd,
    min_w: i32,
    min_h: i32,
    cx: i32,
    id: u32,
    ideal: i32,
    param: i64,
}

#[derive(Default)]
struct Rebar {
    bands: Vec<Band>,
    font: u64,
    bk: u32,
    owner: Hwnd,
    images: u64,
    height: i32,
    /// A gripper being dragged: (band, where it started, the widths then).
    drag: Option<(usize, i32, i32, i32)>,
}

static BARS: Lock<BTreeMap<Hwnd, Rebar>> = Lock::new(BTreeMap::new());

fn with<R>(hwnd: Hwnd, f: impl FnOnce(&mut Rebar) -> R) -> Option<R> {
    BARS.lock().get_mut(&hwnd).map(f)
}

pub fn register_class() {
    crate::register("ReBarWindow32", rebar_proc as *const () as usize, true);
}

fn style(hwnd: Hwnd) -> u32 {
    unsafe { crate::GetWindowLongPtrW(hwnd, -16) as u32 }
}

/// The text's width in the band's header.
fn header_w(hwnd: Hwnd, b: &Band) -> i32 {
    let grip = if b.style & (RBBS_NOGRIPPER | RBBS_FIXEDSIZE) == 0 { GRIPPER } else { 2 };
    if b.text.is_empty() {
        return grip;
    }
    let font = with(hwnd, |r| r.font).unwrap_or(0);
    let dc = unsafe { crate::GetDC(hwnd) };
    let old = unsafe { crate::SelectObject(dc, crate::font_or_default(font)) };
    let w = crate::text_size(dc, &b.text).0;
    unsafe {
        crate::SelectObject(dc, old);
        crate::ReleaseDC(hwnd, dc);
    }
    grip + w + 6
}

/// Each band's rectangle: rows break at RBBS_BREAK; the last in a row takes
/// what's left. Hidden bands are empty.
fn rects(hwnd: Hwnd) -> Vec<[i32; 4]> {
    let mut c = [0i32; 4];
    unsafe { crate::GetClientRect(hwnd, &mut c) };
    let bands = with(hwnd, |r| r.bands.clone()).unwrap_or_default();
    let line = crate::font_height(hwnd, with(hwnd, |r| r.font).unwrap_or(0)) + 4;
    let mut out = alloc::vec![[0i32; 4]; bands.len()];
    let mut i = 0;
    let mut y = 0;
    while i < bands.len() {
        let mut end = i;
        let mut h = 0;
        loop {
            if bands[end].style & RBBS_HIDDEN == 0 {
                h = h.max(bands[end].min_h.max(line) + 2);
            }
            end += 1;
            if end >= bands.len() || bands[end].style & RBBS_BREAK != 0 {
                break;
            }
        }
        let mut x = 0;
        let last_shown = (i..end).rev().find(|&k| bands[k].style & RBBS_HIDDEN == 0);
        for k in i..end {
            if bands[k].style & RBBS_HIDDEN != 0 {
                out[k] = [x, y, x, y];
                continue;
            }
            let least = header_w(hwnd, &bands[k]) + bands[k].min_w;
            let mut w = bands[k].cx.max(least);
            if Some(k) == last_shown {
                w = (c[2] - x).max(least);
            }
            out[k] = [x, y, x + w, y + h];
            x += w;
        }
        y += h + 1;
        i = end;
    }
    out
}

/// Children to their bands, and the bar to its height.
fn layout(hwnd: Hwnd) {
    let st = style(hwnd);
    let rs = rects(hwnd);
    let bands = with(hwnd, |r| r.bands.clone()).unwrap_or_default();
    let height = rs.iter().map(|r| r[3]).max().unwrap_or(0) + 1;
    for (i, b) in bands.iter().enumerate() {
        if b.child == 0 {
            continue;
        }
        if b.style & RBBS_HIDDEN != 0 {
            unsafe { crate::ShowWindow(b.child, 0) };
            continue;
        }
        let r = rs[i];
        let x = r[0] + header_w(hwnd, b);
        let h = (r[3] - r[1] - 2).min(if b.min_h > 0 { b.min_h } else { r[3] - r[1] - 2 });
        let y = r[1] + (r[3] - r[1] - h) / 2;
        unsafe {
            crate::SetWindowPos(b.child, 0, x, y, (r[2] - x - 2).max(1), h, 0x14);
            crate::ShowWindow(b.child, 8);
        }
    }
    let old = with(hwnd, |r| core::mem::replace(&mut r.height, height)).unwrap_or(0);
    if st & (CCS_NORESIZE | CCS_NOPARENTALIGN) == 0 {
        let parent = unsafe { crate::GetParent(hwnd) };
        let mut pr = [0i32; 4];
        unsafe { crate::GetClientRect(parent, &mut pr) };
        let y = if st & 0x3 == CCS_BOTTOM { pr[3] - height } else { 0 };
        unsafe { crate::SetWindowPos(hwnd, 0, 0, y, pr[2], height, 0x14) };
    }
    if old != height {
        crate::notify(hwnd, RBN_HEIGHTCHANGE, &[]);
    }
    unsafe { crate::InvalidateRect(hwnd, core::ptr::null(), 1) };
}

fn paint(hwnd: Hwnd, hdc: u64) {
    let mut c = [0i32; 4];
    unsafe { crate::GetClientRect(hwnd, &mut c) };
    let (bk, font, images) = with(hwnd, |r| (r.bk, r.font, r.images)).unwrap_or((CLR_NONE, 0, 0));
    let fill = |r: &[i32; 4], color: u32| unsafe {
        if color == CLR_DEFAULT || color == CLR_NONE {
            crate::FillRect(hdc, r, crate::GetSysColorBrush(15));
        } else {
            let b = crate::CreateSolidBrush(color);
            crate::FillRect(hdc, r, b);
            crate::DeleteObject(b);
        }
    };
    fill(&c, bk);
    let bands = with(hwnd, |r| r.bands.clone()).unwrap_or_default();
    let old = unsafe { crate::SelectObject(hdc, crate::font_or_default(font)) };
    unsafe { crate::SetBkMode(hdc, 1) };
    let (light, dark) = unsafe { (crate::GetSysColorBrush(20), crate::GetSysColorBrush(16)) };
    for (b, r) in bands.iter().zip(rects(hwnd)) {
        if b.style & RBBS_HIDDEN != 0 {
            continue;
        }
        if b.back != CLR_DEFAULT {
            fill(&r, b.back);
        }
        unsafe {
            crate::FillRect(hdc, &[r[0], r[3], r[2], r[3] + 1], dark);
            if r[0] > 0 {
                crate::FillRect(hdc, &[r[0], r[1], r[0] + 1, r[3]], light);
            }
        }
        let mut x = r[0] + 2;
        if b.style & (RBBS_NOGRIPPER | RBBS_FIXEDSIZE) == 0 {
            unsafe {
                crate::FillRect(hdc, &[x + 2, r[1] + 3, x + 3, r[3] - 3], light);
                crate::FillRect(hdc, &[x + 3, r[1] + 3, x + 4, r[3] - 3], dark);
            }
            x = r[0] + GRIPPER;
        }
        if images != 0 && b.image >= 0 {
            crate::imagelist::ImageList_Draw(images, b.image, hdc, x, r[1] + 2, 0);
        }
        if !b.text.is_empty() {
            let mut t = b.text.clone();
            let color = if b.fore == CLR_DEFAULT { unsafe { crate::GetSysColor(18) } } else { b.fore };
            unsafe { crate::SetTextColor(hdc, color) };
            let mut tr = [x, r[1], r[2], r[3]];
            // DT_SINGLELINE | DT_VCENTER
            unsafe { crate::DrawTextW(hdc, t.as_mut_ptr(), t.len() as i32, &mut tr, 0x20 | 0x4) };
        }
    }
    unsafe { crate::SelectObject(hdc, old) };
}

fn read_band(lp: i64, unicode: bool, b: &mut Band) {
    let p = lp as *const u8;
    unsafe {
        let d = |o: usize| core::ptr::read_unaligned(p.add(o) as *const u32);
        let q = |o: usize| core::ptr::read_unaligned(p.add(o) as *const u64);
        let mask = d(4);
        if mask & RBBIM_STYLE != 0 {
            b.style = d(8);
        }
        if mask & RBBIM_COLORS != 0 {
            b.fore = d(12);
            b.back = d(16);
        }
        if mask & RBBIM_TEXT != 0 {
            b.text = crate::text_arg(q(24), unicode);
        }
        if mask & RBBIM_IMAGE != 0 {
            b.image = d(36) as i32;
        }
        if mask & RBBIM_CHILD != 0 {
            b.child = q(40);
        }
        if mask & RBBIM_CHILDSIZE != 0 {
            b.min_w = d(48) as i32;
            b.min_h = d(52) as i32;
        }
        if mask & RBBIM_SIZE != 0 {
            b.cx = d(56) as i32;
        }
        if mask & RBBIM_ID != 0 {
            b.id = d(72);
        }
        if mask & RBBIM_IDEALSIZE != 0 {
            b.ideal = d(88) as i32;
        }
        if mask & RBBIM_LPARAM != 0 {
            b.param = q(96) as i64;
        }
    }
}

fn write_band(lp: i64, unicode: bool, b: &Band) {
    let p = lp as *mut u8;
    unsafe {
        let mask = core::ptr::read_unaligned(p.add(4) as *const u32);
        let d = |o: usize, v: u32| core::ptr::write_unaligned(p.add(o) as *mut u32, v);
        if mask & RBBIM_STYLE != 0 {
            d(8, b.style);
        }
        if mask & RBBIM_COLORS != 0 {
            d(12, b.fore);
            d(16, b.back);
        }
        if mask & RBBIM_TEXT != 0 {
            let dst = core::ptr::read_unaligned(p.add(24) as *const u64);
            let cap = core::ptr::read_unaligned(p.add(32) as *const i32);
            crate::put_text(dst, cap, &b.text, unicode);
        }
        if mask & RBBIM_IMAGE != 0 {
            d(36, b.image as u32);
        }
        if mask & RBBIM_CHILD != 0 {
            core::ptr::write_unaligned(p.add(40) as *mut u64, b.child);
        }
        if mask & RBBIM_CHILDSIZE != 0 {
            d(48, b.min_w as u32);
            d(52, b.min_h as u32);
        }
        if mask & RBBIM_SIZE != 0 {
            d(56, b.cx as u32);
        }
        if mask & RBBIM_ID != 0 {
            d(72, b.id);
        }
        if mask & RBBIM_IDEALSIZE != 0 {
            d(88, b.ideal as u32);
        }
        if mask & RBBIM_LPARAM != 0 {
            core::ptr::write_unaligned(p.add(96) as *mut i64, b.param);
        }
    }
}

unsafe extern "system" fn rebar_proc(hwnd: Hwnd, msg: u32, wp: u64, lp: i64) -> i64 {
    match msg {
        0x0081 => {
            let owner = unsafe { crate::GetParent(hwnd) };
            BARS.lock().insert(hwnd, Rebar { bk: CLR_NONE, owner, ..Rebar::default() });
            unsafe { crate::DefWindowProcW(hwnd, msg, wp, lp) }
        }
        0x0082 => {
            BARS.lock().remove(&hwnd);
            unsafe { crate::DefWindowProcW(hwnd, msg, wp, lp) }
        }
        0x000F => {
            let mut ps = [0u8; 72];
            let hdc = unsafe { crate::BeginPaint(hwnd, ps.as_mut_ptr()) };
            paint(hwnd, hdc);
            unsafe { crate::EndPaint(hwnd, ps.as_ptr()) };
            0
        }
        0x0014 => 1,
        0x0005 => {
            layout(hwnd);
            0
        }
        0x0030 => {
            with(hwnd, |r| r.font = wp);
            layout(hwnd);
            0
        }
        0x0031 => with(hwnd, |r| r.font as i64).unwrap_or(0),
        // What the children send goes on to the owner.
        crate::WM_NOTIFY | 0x0111 => {
            let owner = with(hwnd, |r| r.owner).unwrap_or(0);
            unsafe { crate::SendMessageW(owner, msg, wp, lp) }
        }
        0x0201 => {
            let (x, y) = ((lp & 0xFFFF) as i16 as i32, ((lp >> 16) & 0xFFFF) as i16 as i32);
            let rs = rects(hwnd);
            let bands = with(hwnd, |r| r.bands.clone()).unwrap_or_default();
            for i in 1..bands.len() {
                let r = rs[i];
                if x >= r[0] && x < r[0] + GRIPPER && y >= r[1] && y < r[3] && bands[i].style & (RBBS_NOGRIPPER | RBBS_FIXEDSIZE) == 0 && bands[i].style & RBBS_BREAK == 0 {
                    let prev = rs[i - 1][2] - rs[i - 1][0];
                    with(hwnd, |b| b.drag = Some((i, x, prev, r[2] - r[0])));
                    unsafe { crate::SetCapture(hwnd) };
                    crate::notify(hwnd, RBN_BEGINDRAG, &[]);
                    break;
                }
            }
            0
        }
        0x0200 => {
            let x = (lp & 0xFFFF) as i16 as i32;
            if let Some(Some((i, start, prev, own))) = with(hwnd, |b| b.drag) {
                let d = x - start;
                with(hwnd, |b| {
                    b.bands[i - 1].cx = (prev + d).max(16);
                    b.bands[i].cx = (own - d).max(16);
                });
                layout(hwnd);
            }
            0
        }
        0x0202 => {
            if with(hwnd, |b| b.drag.take()).flatten().is_some() {
                unsafe { crate::ReleaseCapture() };
                crate::notify(hwnd, RBN_ENDDRAG, &[]);
            }
            0
        }
        RB_INSERTBANDW | RB_INSERTBANDA => {
            if lp == 0 {
                return 0;
            }
            let mut b = Band { style: 0, fore: CLR_DEFAULT, back: CLR_DEFAULT, text: Vec::new(), image: -1, child: 0, min_w: 0, min_h: 0, cx: 0, id: 0, ideal: 0, param: 0 };
            read_band(lp, msg == RB_INSERTBANDW, &mut b);
            with(hwnd, |r| {
                let at = if wp as i32 == -1 { r.bands.len() } else { (wp as usize).min(r.bands.len()) };
                r.bands.insert(at, b);
            });
            layout(hwnd);
            1
        }
        RB_DELETEBAND => {
            let Some(Some(b)) = with(hwnd, |r| ((wp as usize) < r.bands.len()).then(|| r.bands.remove(wp as usize))) else { return 0 };
            if b.child != 0 {
                unsafe { crate::ShowWindow(b.child, 0) };
            }
            layout(hwnd);
            1
        }
        RB_SETBANDINFOW | RB_SETBANDINFOA => {
            if lp == 0 {
                return 0;
            }
            let ok = with(hwnd, |r| match r.bands.get_mut(wp as usize) {
                Some(b) => {
                    read_band(lp, msg == RB_SETBANDINFOW, b);
                    true
                }
                None => false,
            })
            .unwrap_or(false);
            layout(hwnd);
            ok as i64
        }
        RB_GETBANDINFOW | RB_GETBANDINFOA => {
            if lp == 0 {
                return 0;
            }
            let Some(Some(b)) = with(hwnd, |r| r.bands.get(wp as usize).cloned()) else { return 0 };
            write_band(lp, msg == RB_GETBANDINFOW, &b);
            1
        }
        RB_GETBANDCOUNT => with(hwnd, |r| r.bands.len() as i64).unwrap_or(0),
        RB_GETROWCOUNT => {
            let mut tops: Vec<i32> = rects(hwnd).iter().filter(|r| r[2] > r[0]).map(|r| r[1]).collect();
            tops.dedup();
            tops.len() as i64
        }
        RB_GETROWHEIGHT => rects(hwnd).get(wp as usize).map_or(0, |r| (r[3] - r[1]) as i64),
        RB_GETBARHEIGHT => with(hwnd, |r| r.height as i64).unwrap_or(0),
        RB_IDTOINDEX => with(hwnd, |r| r.bands.iter().position(|b| b.id == wp as u32).map_or(-1, |i| i as i64)).unwrap_or(-1),
        RB_GETRECT => {
            if lp == 0 {
                return 0;
            }
            match rects(hwnd).get(wp as usize) {
                Some(r) => {
                    unsafe { *(lp as *mut [i32; 4]) = *r };
                    1
                }
                None => 0,
            }
        }
        RB_HITTEST => {
            if lp == 0 {
                return -1;
            }
            let p = lp as *mut i32;
            let (x, y) = unsafe { (*p, *p.add(1)) };
            let i = rects(hwnd).iter().position(|r| x >= r[0] && x < r[2] && y >= r[1] && y < r[3]);
            unsafe {
                // RBHT_NOWHERE 1, RBHT_CLIENT 3.
                *p.add(2) = if i.is_some() { 3 } else { 1 };
                *p.add(3) = i.map_or(-1, |k| k as i32);
            }
            i.map_or(-1, |k| k as i64)
        }
        RB_SHOWBAND => {
            let ok = with(hwnd, |r| match r.bands.get_mut(wp as usize) {
                Some(b) => {
                    if lp != 0 {
                        b.style &= !RBBS_HIDDEN;
                    } else {
                        b.style |= RBBS_HIDDEN;
                    }
                    true
                }
                None => false,
            })
            .unwrap_or(false);
            layout(hwnd);
            ok as i64
        }
        RB_MINIMIZEBAND | RB_MAXIMIZEBAND => {
            with(hwnd, |r| {
                if let Some(b) = r.bands.get_mut(wp as usize) {
                    b.cx = if msg == RB_MINIMIZEBAND { 0 } else { 32767 };
                }
            });
            layout(hwnd);
            0
        }
        RB_SIZETORECT => {
            if lp != 0 {
                let r = unsafe { *(lp as *const [i32; 4]) };
                unsafe { crate::SetWindowPos(hwnd, 0, r[0], r[1], r[2] - r[0], r[3] - r[1], 0x14) };
            }
            layout(hwnd);
            1
        }
        RB_GETBANDBORDERS => {
            if lp != 0 {
                let b = with(hwnd, |r| r.bands.get(wp as usize).cloned()).flatten();
                let w = b.map_or(0, |b| header_w(hwnd, &b));
                unsafe { *(lp as *mut [i32; 4]) = [w, 0, 2, 0] };
            }
            0
        }
        RB_SETBKCOLOR => {
            let old = with(hwnd, |r| core::mem::replace(&mut r.bk, lp as u32)).unwrap_or(CLR_NONE);
            unsafe { crate::InvalidateRect(hwnd, core::ptr::null(), 1) };
            old as i64
        }
        RB_GETBKCOLOR => with(hwnd, |r| r.bk as i64).unwrap_or(0),
        RB_SETPARENT => with(hwnd, |r| core::mem::replace(&mut r.owner, wp) as i64).unwrap_or(0),
        // REBARINFO { cbSize, fMask, himl }: RBIM_IMAGELIST 1.
        RB_SETBARINFO => {
            if lp == 0 {
                return 0;
            }
            let (mask, il) = unsafe { (core::ptr::read_unaligned((lp as *const u8).add(4) as *const u32), core::ptr::read_unaligned((lp as *const u8).add(8) as *const u64)) };
            if mask & 1 != 0 {
                with(hwnd, |r| r.images = il);
            }
            1
        }
        RB_GETBARINFO => {
            if lp == 0 {
                return 0;
            }
            let il = with(hwnd, |r| r.images).unwrap_or(0);
            unsafe { core::ptr::write_unaligned((lp as *mut u8).add(8) as *mut u64, il) };
            1
        }
        _ => unsafe { crate::DefWindowProcW(hwnd, msg, wp, lp) },
    }
}
