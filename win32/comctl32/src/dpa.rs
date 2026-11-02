//! Dynamic pointer arrays (DPA) and structure arrays (DSA), which programs
//! import from comctl32 by ordinal, and a few small helpers: `Str_SetPtrW`,
//! `MenuHelp`, `GetEffectiveClientRect`, and `TaskDialog` as a message box.

use alloc::boxed::Box;
use alloc::vec::Vec;

use crate::{Bool, Hwnd};

// -----| DPA |-----

type Dpa = Vec<u64>;

fn dpa<'a>(h: u64) -> Option<&'a mut Dpa> {
    if h == 0 { None } else { Some(unsafe { &mut *(h as *mut Dpa) }) }
}

#[no_mangle]
pub extern "system" fn DPA_Create(_grow: i32) -> u64 {
    Box::into_raw(Box::new(Dpa::new())) as u64
}

#[no_mangle]
pub extern "system" fn DPA_CreateEx(grow: i32, _heap: u64) -> u64 {
    DPA_Create(grow)
}

#[no_mangle]
pub extern "system" fn DPA_Destroy(h: u64) -> Bool {
    if h == 0 {
        return 0;
    }
    drop(unsafe { Box::from_raw(h as *mut Dpa) });
    1
}

#[no_mangle]
pub extern "system" fn DPA_Grow(h: u64, n: i32) -> Bool {
    let Some(d) = dpa(h) else { return 0 };
    d.reserve((n as usize).saturating_sub(d.len()));
    1
}

#[no_mangle]
pub extern "system" fn DPA_Clone(h: u64, into: u64) -> u64 {
    let Some(d) = dpa(h) else { return 0 };
    let copy = d.clone();
    match dpa(into) {
        Some(t) => {
            *t = copy;
            into
        }
        None => Box::into_raw(Box::new(copy)) as u64,
    }
}

#[no_mangle]
pub extern "system" fn DPA_GetPtr(h: u64, i: i64) -> u64 {
    dpa(h).and_then(|d| d.get(i as usize).copied()).unwrap_or(0)
}

#[no_mangle]
pub extern "system" fn DPA_GetPtrIndex(h: u64, p: u64) -> i32 {
    dpa(h).and_then(|d| d.iter().position(|&x| x == p)).map_or(-1, |i| i as i32)
}

/// At `i`, or the end if `i` is past it.
#[no_mangle]
pub extern "system" fn DPA_InsertPtr(h: u64, i: i32, p: u64) -> i32 {
    let Some(d) = dpa(h) else { return -1 };
    if i < 0 {
        return -1;
    }
    let at = (i as usize).min(d.len());
    d.insert(at, p);
    at as i32
}

#[no_mangle]
pub extern "system" fn DPA_SetPtr(h: u64, i: i32, p: u64) -> Bool {
    let Some(d) = dpa(h) else { return 0 };
    if i < 0 {
        return 0;
    }
    if i as usize >= d.len() {
        d.resize(i as usize + 1, 0);
    }
    d[i as usize] = p;
    1
}

#[no_mangle]
pub extern "system" fn DPA_DeletePtr(h: u64, i: i32) -> u64 {
    let Some(d) = dpa(h) else { return 0 };
    if i < 0 || i as usize >= d.len() {
        return 0;
    }
    d.remove(i as usize)
}

#[no_mangle]
pub extern "system" fn DPA_DeleteAllPtrs(h: u64) -> Bool {
    let Some(d) = dpa(h) else { return 0 };
    d.clear();
    1
}

/// PFNDACOMPARE(p1, p2, lParam).
#[no_mangle]
pub extern "system" fn DPA_Sort(h: u64, cmp: usize, lp: i64) -> Bool {
    let Some(d) = dpa(h) else { return 0 };
    if cmp == 0 {
        return 0;
    }
    let f: unsafe extern "system" fn(u64, u64, i64) -> i32 = unsafe { core::mem::transmute(cmp) };
    crate::merge_sort(d, &mut |a, b| unsafe { f(*a, *b, lp) });
    1
}

const DPAS_SORTED: u32 = 1;
const DPAS_INSERTBEFORE: u32 = 2;
const DPAS_INSERTAFTER: u32 = 4;

#[no_mangle]
pub extern "system" fn DPA_Search(h: u64, find: u64, start: i32, cmp: usize, lp: i64, opts: u32) -> i32 {
    let Some(d) = dpa(h) else { return -1 };
    if cmp == 0 {
        return -1;
    }
    let f: unsafe extern "system" fn(u64, u64, i64) -> i32 = unsafe { core::mem::transmute(cmp) };
    if opts & DPAS_SORTED == 0 {
        return (start.max(0) as usize..d.len()).find(|&i| unsafe { f(find, d[i], lp) } == 0).map_or(-1, |i| i as i32);
    }
    let (mut lo, mut hi) = (start.max(0) as usize, d.len());
    while lo < hi {
        let mid = (lo + hi) / 2;
        let c = unsafe { f(find, d[mid], lp) };
        if c == 0 {
            return mid as i32;
        }
        if c < 0 {
            hi = mid;
        } else {
            lo = mid + 1;
        }
    }
    // Not there: where it would go, if asked.
    if opts & DPAS_INSERTBEFORE != 0 {
        lo as i32
    } else if opts & DPAS_INSERTAFTER != 0 {
        lo as i32 - 1
    } else {
        -1
    }
}

/// PFNDAENUMCALLBACK(p, data): 0 stops.
#[no_mangle]
pub extern "system" fn DPA_EnumCallback(h: u64, cb: usize, data: u64) {
    let Some(d) = dpa(h) else { return };
    if cb == 0 {
        return;
    }
    let f: unsafe extern "system" fn(u64, u64) -> i32 = unsafe { core::mem::transmute(cb) };
    for p in d.clone() {
        if unsafe { f(p, data) } == 0 {
            break;
        }
    }
}

#[no_mangle]
pub extern "system" fn DPA_DestroyCallback(h: u64, cb: usize, data: u64) {
    DPA_EnumCallback(h, cb, data);
    DPA_Destroy(h);
}

// -----| DSA |-----

struct Dsa {
    size: usize,
    bytes: Vec<u8>,
}

impl Dsa {
    fn count(&self) -> usize {
        self.bytes.len() / self.size
    }
}

fn dsa<'a>(h: u64) -> Option<&'a mut Dsa> {
    if h == 0 { None } else { Some(unsafe { &mut *(h as *mut Dsa) }) }
}

#[no_mangle]
pub extern "system" fn DSA_Create(size: i32, _grow: i32) -> u64 {
    if size <= 0 {
        return 0;
    }
    Box::into_raw(Box::new(Dsa { size: size as usize, bytes: Vec::new() })) as u64
}

#[no_mangle]
pub extern "system" fn DSA_Destroy(h: u64) -> Bool {
    if h == 0 {
        return 0;
    }
    drop(unsafe { Box::from_raw(h as *mut Dsa) });
    1
}

#[no_mangle]
pub unsafe extern "system" fn DSA_GetItem(h: u64, i: i32, out: *mut u8) -> Bool {
    let Some(d) = dsa(h) else { return 0 };
    if i < 0 || i as usize >= d.count() || out.is_null() {
        return 0;
    }
    unsafe { core::ptr::copy_nonoverlapping(d.bytes.as_ptr().add(i as usize * d.size), out, d.size) };
    1
}

#[no_mangle]
pub extern "system" fn DSA_GetItemPtr(h: u64, i: i32) -> *mut u8 {
    let Some(d) = dsa(h) else { return core::ptr::null_mut() };
    if i < 0 || i as usize >= d.count() {
        return core::ptr::null_mut();
    }
    unsafe { d.bytes.as_mut_ptr().add(i as usize * d.size) }
}

#[no_mangle]
pub unsafe extern "system" fn DSA_InsertItem(h: u64, i: i32, item: *const u8) -> i32 {
    let Some(d) = dsa(h) else { return -1 };
    if i < 0 || item.is_null() {
        return -1;
    }
    let at = (i as usize).min(d.count());
    let src = unsafe { core::slice::from_raw_parts(item, d.size) }.to_vec();
    let off = at * d.size;
    d.bytes.splice(off..off, src);
    at as i32
}

#[no_mangle]
pub unsafe extern "system" fn DSA_SetItem(h: u64, i: i32, item: *const u8) -> Bool {
    let Some(d) = dsa(h) else { return 0 };
    if i < 0 || item.is_null() {
        return 0;
    }
    let need = (i as usize + 1) * d.size;
    if d.bytes.len() < need {
        d.bytes.resize(need, 0);
    }
    unsafe { core::ptr::copy_nonoverlapping(item, d.bytes.as_mut_ptr().add(i as usize * d.size), d.size) };
    1
}

#[no_mangle]
pub extern "system" fn DSA_DeleteItem(h: u64, i: i32) -> Bool {
    let Some(d) = dsa(h) else { return 0 };
    if i < 0 || i as usize >= d.count() {
        return 0;
    }
    let off = i as usize * d.size;
    d.bytes.drain(off..off + d.size);
    1
}

#[no_mangle]
pub extern "system" fn DSA_DeleteAllItems(h: u64) -> Bool {
    let Some(d) = dsa(h) else { return 0 };
    d.bytes.clear();
    1
}

#[no_mangle]
pub extern "system" fn DSA_EnumCallback(h: u64, cb: usize, data: u64) {
    let Some(d) = dsa(h) else { return };
    if cb == 0 {
        return;
    }
    let f: unsafe extern "system" fn(*mut u8, u64) -> i32 = unsafe { core::mem::transmute(cb) };
    for i in 0..d.count() {
        if unsafe { f(d.bytes.as_mut_ptr().add(i * d.size), data) } == 0 {
            break;
        }
    }
}

#[no_mangle]
pub extern "system" fn DSA_DestroyCallback(h: u64, cb: usize, data: u64) {
    DSA_EnumCallback(h, cb, data);
    DSA_Destroy(h);
}

// -----| small helpers |-----

/// Replace `*slot` with a LocalAlloc'd copy of `s` (or nothing for NULL).
#[no_mangle]
pub unsafe extern "system" fn Str_SetPtrW(slot: *mut *mut u16, s: *const u16) -> Bool {
    if slot.is_null() {
        return 0;
    }
    let old = unsafe { *slot };
    let new = if s.is_null() {
        core::ptr::null_mut()
    } else {
        let v = crate::wstr(s);
        let p = unsafe { LocalAlloc(0, (v.len() + 1) * 2) } as *mut u16;
        if p.is_null() {
            return 0;
        }
        unsafe {
            core::ptr::copy_nonoverlapping(v.as_ptr(), p, v.len());
            *p.add(v.len()) = 0;
        }
        p
    };
    unsafe { *slot = new };
    if !old.is_null() {
        unsafe { LocalFree(old as *mut u8) };
    }
    1
}

#[link(name = "kernel32", kind = "raw-dylib")]
unsafe extern "system" {
    fn LocalAlloc(flags: u32, size: usize) -> *mut u8;
    fn LocalFree(p: *mut u8) -> *mut u8;
}

/// Show a menu item's help string (from `ids`, pairs of command base and
/// string id) in a status bar.
#[no_mangle]
pub unsafe extern "system" fn MenuHelp(msg: u32, wp: u64, lp: i64, _menu: u64, inst: u64, status: Hwnd, ids: *const u32) {
    const SB_SIMPLE: u32 = 0x0409;
    const SB_SETTEXTW: u32 = 0x040B;
    if status == 0 {
        return;
    }
    if msg == 0x011F {
        // WM_MENUSELECT: flags 0xFFFF with no menu when the menu closes.
        let (item, flags) = ((wp & 0xFFFF) as u32, ((wp >> 16) & 0xFFFF) as u32);
        if flags == 0xFFFF && lp == 0 {
            unsafe { crate::SendMessageW(status, SB_SIMPLE, 0, 0) };
            return;
        }
        let mut text = [0u16; 256];
        let mut n = 0;
        if !ids.is_null() && flags & 0x10 == 0 && flags & 0x800 == 0 {
            let mut p = ids;
            unsafe {
                while *p != 0 || *p.add(1) != 0 {
                    let (base, string) = (*p, *p.add(1));
                    if item >= base && item < base.saturating_add(0x100) {
                        n = crate::LoadStringW(inst, string + (item - base), text.as_mut_ptr(), 256);
                        break;
                    }
                    p = p.add(2);
                }
            }
        }
        text[(n.max(0) as usize).min(255)] = 0;
        unsafe {
            crate::SendMessageW(status, SB_SIMPLE, 1, 0);
            crate::SendMessageW(status, SB_SETTEXTW, 255, text.as_ptr() as i64);
        }
    }
}

/// The client rectangle less the visible controls listed in `info` (pairs of
/// menu id and control id; after two leading words, ending at 0).
#[no_mangle]
pub unsafe extern "system" fn GetEffectiveClientRect(hwnd: Hwnd, r: *mut [i32; 4], info: *const i32) {
    unsafe { crate::GetClientRect(hwnd, r) };
    if info.is_null() {
        return;
    }
    let mut p = unsafe { info.add(2) };
    unsafe {
        while *p != 0 {
            let ctl = crate::GetDlgItem(hwnd, *p.add(1));
            p = p.add(2);
            if ctl == 0 || crate::IsWindowVisible(ctl) == 0 {
                continue;
            }
            let mut w = [0i32; 4];
            crate::GetWindowRect(ctl, &mut w);
            let mut pts = [[w[0], w[1]], [w[2], w[3]]];
            crate::MapWindowPoints(0, hwnd, pts.as_mut_ptr(), 2);
            let (cl, ct, cr, cb) = (pts[0][0], pts[0][1], pts[1][0], pts[1][1]);
            let rr = &mut *r;
            // Whichever edge it spans.
            if cl <= rr[0] && cr >= rr[2] {
                if ct <= rr[1] {
                    rr[1] = cb;
                } else if cb >= rr[3] {
                    rr[3] = ct;
                }
            } else if ct <= rr[1] && cb >= rr[3] {
                if cl <= rr[0] {
                    rr[0] = cr;
                } else if cr >= rr[2] {
                    rr[2] = cl;
                }
            }
        }
    }
}

// -----| TaskDialog: a message box |-----

/// `TaskDialog(hwnd, inst, title, main, content, buttons, icon, *button)`.
/// Common buttons: OK 1, Yes 2, No 4, Cancel 8, Retry 0x10, Close 0x20.
#[no_mangle]
pub unsafe extern "system" fn TaskDialog(hwnd: Hwnd, _inst: u64, title: *const u16, main: *const u16, content: *const u16, buttons: u32,
                                         icon: *const u16, out: *mut i32) -> i32 {
    let safe = |p: *const u16| if (p as usize) >> 16 == 0 { Vec::new() } else { crate::wstr(p) };
    let mut text = safe(main);
    let body = safe(content);
    if !text.is_empty() && !body.is_empty() {
        text.extend_from_slice(&[13, 10, 13, 10]);
    }
    text.extend_from_slice(&body);
    text.push(0);
    let mut caption = safe(title);
    caption.push(0);
    // TD_WARNING_ICON, TD_ERROR_ICON, TD_INFORMATION_ICON, TD_SHIELD_ICON.
    let mark = match icon as u64 as u16 as i16 {
        -1 => 0x30,
        -2 => 0x10,
        -3 => 0x40,
        _ => 0,
    };
    let kind = match buttons & 0x3F {
        b if b & 6 == 6 && b & 8 != 0 => 3, // Yes, No, Cancel
        b if b & 6 == 6 => 4,               // Yes, No
        b if b & 0x18 == 0x18 => 5,         // Retry, Cancel
        b if b & 9 == 9 => 1,               // OK, Cancel
        _ => 0,
    };
    let r = unsafe { crate::MessageBoxW(hwnd, text.as_ptr(), caption.as_ptr(), kind | mark) };
    if !out.is_null() {
        unsafe { *out = r };
    }
    0
}

/// TASKDIALOGCONFIG: size, hwndParent, hInstance, flags, buttons, title,
/// main icon, main instruction, content, ... (the common parts).
#[no_mangle]
pub unsafe extern "system" fn TaskDialogIndirect(config: *const u8, out: *mut i32, radio: *mut i32, verify: *mut Bool) -> i32 {
    if config.is_null() {
        return 0x8007_0057u32 as i32;
    }
    let (hwnd, buttons, title, icon, main, content) = unsafe {
        let q = |off: usize| core::ptr::read_unaligned(config.add(off) as *const u64);
        // cbSize@0, hwndParent@4, hInstance@12, dwFlags@20, dwCommonButtons@24,
        // pszWindowTitle@28, pszMainIcon@36, pszMainInstruction@44, pszContent@52
        // (the structure is packed).
        (q(4), core::ptr::read_unaligned(config.add(24) as *const u32), q(28), q(36), q(44), q(52))
    };
    unsafe {
        if !radio.is_null() {
            *radio = 0;
        }
        if !verify.is_null() {
            *verify = 0;
        }
        TaskDialog(hwnd, 0, title as *const u16, main as *const u16, content as *const u16, buttons, icon as *const u16, out)
    }
}
