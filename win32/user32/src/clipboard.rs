//! Names made numbers (registered window messages and clipboard formats,
//! one table from 0xC000 as on Windows), window properties, and the
//! clipboard.
//!
//! The clipboard's text is the kernel's (VEIL_CLIP_*), shared with every
//! program and the desktop; other formats stay in the program that set
//! them. Data is a GlobalAlloc'd block, as on Windows: SetClipboardData
//! takes it over, GetClipboardData's stays the clipboard's.

use alloc::vec::Vec;

use crate::dllrt::Lock;
use crate::{state, Bool, Hwnd};

#[link(name = "ntdll", kind = "raw-dylib")]
unsafe extern "system" {
    fn VeilClipSet(text: *const u16, chars: usize) -> i32;
    fn VeilClipGet(buf: *mut u16, cap: usize, chars: *mut u32, seq: *mut u32) -> i32;
}

#[link(name = "kernel32", kind = "raw-dylib")]
unsafe extern "system" {
    fn GlobalAlloc(flags: u32, size: usize) -> *mut u8;
    fn GlobalFree(p: *mut u8) -> *mut u8;
    fn GlobalSize(p: *mut u8) -> usize;
    fn GlobalLock(h: *mut u8) -> *mut u8;
    fn GlobalGetAtomNameW(atom: u16, buf: *mut u16, cap: i32) -> u32;
    fn GlobalUnlock(h: *mut u8) -> Bool;
}

const GMEM_MOVEABLE: u32 = 0x2;
const GMEM_ZEROINIT: u32 = 0x40;

// -----| registered names |-----

static NAMES: Lock<Vec<Vec<u16>>> = Lock::new(Vec::new());

fn lower(s: &[u16]) -> Vec<u16> {
    s.iter().map(|&c| if (0x41..=0x5A).contains(&c) { c + 32 } else { c }).collect()
}

/// The number for `name` (0xC000 up), the same for the same name in any case.
pub fn register(name: &[u16]) -> u32 {
    if name.is_empty() {
        return 0;
    }
    let key = lower(name);
    let mut n = NAMES.lock();
    let i = match n.iter().position(|e| lower(e) == key) {
        Some(i) => i,
        None => {
            n.push(name.to_vec());
            n.len() - 1
        }
    };
    0xC000 + i as u32
}

fn name_of(id: u32) -> Option<Vec<u16>> {
    NAMES.lock().get(id.checked_sub(0xC000)? as usize).cloned()
}

#[no_mangle]
pub unsafe extern "system" fn RegisterWindowMessageW(name: *const u16) -> u32 {
    register(&unsafe { crate::wstr(name) })
}

#[no_mangle]
pub unsafe extern "system" fn RegisterWindowMessageA(name: *const u8) -> u32 {
    register(&unsafe { crate::from_ansi(name, -1) })
}

#[no_mangle]
pub unsafe extern "system" fn RegisterClipboardFormatW(name: *const u16) -> u32 {
    register(&unsafe { crate::wstr(name) })
}

#[no_mangle]
pub unsafe extern "system" fn RegisterClipboardFormatA(name: *const u8) -> u32 {
    register(&unsafe { crate::from_ansi(name, -1) })
}

#[no_mangle]
pub unsafe extern "system" fn GetClipboardFormatNameW(id: u32, out: *mut u16, cap: i32) -> i32 {
    match name_of(id) {
        Some(n) => unsafe { crate::copy_out(&n, out, cap.max(0) as usize) as i32 },
        None => 0,
    }
}

#[no_mangle]
pub unsafe extern "system" fn GetClipboardFormatNameA(id: u32, out: *mut u8, cap: i32) -> i32 {
    match name_of(id) {
        Some(n) => unsafe { crate::copy_out(&crate::to_ansi(&n), out, cap.max(0) as usize) as i32 },
        None => 0,
    }
}

// -----| window properties |-----

/// (window, name, value); an atom name is "#atom".
static PROPS: Lock<Vec<(Hwnd, Vec<u16>, u64)>> = Lock::new(Vec::new());

fn prop_key(p: u64, wide: bool) -> Vec<u16> {
    if p >> 16 == 0 {
        // An atom (a global one, as SetProp's are): its name, so the name
        // finds it too.
        let mut buf = [0u16; 256];
        let n = unsafe { GlobalGetAtomNameW(p as u16, buf.as_mut_ptr(), 256) } as usize;
        if n > 0 {
            return lower(&buf[..n]);
        }
        return alloc::format!("#{}", p).encode_utf16().collect();
    }
    let s = if wide { unsafe { crate::wstr(p as *const u16) } } else { unsafe { crate::from_ansi(p as *const u8, -1) } };
    lower(&s)
}

fn set_prop(hwnd: Hwnd, key: Vec<u16>, v: u64) -> Bool {
    if state().get(hwnd).is_none() {
        return 0;
    }
    let mut p = PROPS.lock();
    match p.iter_mut().find(|e| e.0 == hwnd && e.1 == key) {
        Some(e) => e.2 = v,
        None => p.push((hwnd, key, v)),
    }
    1
}

fn get_prop(hwnd: Hwnd, key: &[u16]) -> u64 {
    PROPS.lock().iter().find(|e| e.0 == hwnd && e.1 == key).map_or(0, |e| e.2)
}

fn remove_prop(hwnd: Hwnd, key: &[u16]) -> u64 {
    let mut p = PROPS.lock();
    match p.iter().position(|e| e.0 == hwnd && e.1 == key) {
        Some(i) => p.remove(i).2,
        None => 0,
    }
}

#[no_mangle]
pub extern "system" fn SetPropW(hwnd: Hwnd, name: u64, v: u64) -> Bool {
    set_prop(hwnd, prop_key(name, true), v)
}

#[no_mangle]
pub extern "system" fn SetPropA(hwnd: Hwnd, name: u64, v: u64) -> Bool {
    set_prop(hwnd, prop_key(name, false), v)
}

#[no_mangle]
pub extern "system" fn GetPropW(hwnd: Hwnd, name: u64) -> u64 {
    get_prop(hwnd, &prop_key(name, true))
}

#[no_mangle]
pub extern "system" fn GetPropA(hwnd: Hwnd, name: u64) -> u64 {
    get_prop(hwnd, &prop_key(name, false))
}

#[no_mangle]
pub extern "system" fn RemovePropW(hwnd: Hwnd, name: u64) -> u64 {
    remove_prop(hwnd, &prop_key(name, true))
}

#[no_mangle]
pub extern "system" fn RemovePropA(hwnd: Hwnd, name: u64) -> u64 {
    remove_prop(hwnd, &prop_key(name, false))
}

/// A window is gone: its properties too.
pub fn window_destroyed(hwnd: Hwnd) {
    PROPS.lock().retain(|e| e.0 != hwnd);
}

// -----| the clipboard |-----

const CF_TEXT: u32 = 1;
const CF_OEMTEXT: u32 = 7;
const CF_UNICODETEXT: u32 = 13;
const CF_LOCALE: u32 = 16;

struct Clip {
    /// The window that has it open (0: open without one), and whether it
    /// is open at all.
    open: Option<Hwnd>,
    owner: Hwnd,
    /// This program's data: (format, block).
    data: Vec<(u32, u64)>,
    /// Text set while open, for the kernel at CloseClipboard.
    text_dirty: bool,
    /// The kernel's sequence number this program last saw its own text at.
    seq: u32,
}

static CLIP: Lock<Clip> = Lock::new(Clip { open: None, owner: 0, data: Vec::new(), text_dirty: false, seq: 0 });

fn kernel_text() -> (Vec<u16>, u32) {
    let (mut n, mut seq) = (0u32, 0u32);
    unsafe { VeilClipGet(core::ptr::null_mut(), 0, &mut n, &mut seq) };
    let mut buf = alloc::vec![0u16; n as usize];
    unsafe { VeilClipGet(buf.as_mut_ptr(), buf.len(), &mut n, &mut seq) };
    buf.truncate(n as usize);
    (buf, seq)
}

fn is_text(f: u32) -> bool {
    matches!(f, CF_TEXT | CF_OEMTEXT | CF_UNICODETEXT)
}

#[no_mangle]
pub extern "system" fn OpenClipboard(hwnd: Hwnd) -> Bool {
    let mut c = CLIP.lock();
    if c.open.is_some() {
        crate::set_error(5);
        return 0;
    }
    c.open = Some(hwnd);
    1
}

#[no_mangle]
pub extern "system" fn CloseClipboard() -> Bool {
    let text = {
        let mut c = CLIP.lock();
        if c.open.take().is_none() {
            return 0;
        }
        let dirty = core::mem::replace(&mut c.text_dirty, false);
        let block = c.data.iter().find(|e| e.0 == CF_UNICODETEXT).map(|e| e.1);
        let ansi = c.data.iter().find(|e| e.0 == CF_TEXT || e.0 == CF_OEMTEXT).map(|e| e.1);
        dirty.then(|| (block, ansi))
    };
    // New text goes to the kernel's clipboard, for everyone.
    if let Some((w, a)) = text {
        // The blocks may be moveable: read them locked.
        let units: Option<Vec<u16>> = match (w, a) {
            (Some(h), _) if h != 0 => {
                let n = unsafe { GlobalSize(h as *mut u8) } / 2;
                let p = unsafe { GlobalLock(h as *mut u8) } as *const u16;
                (!p.is_null()).then(|| {
                    let s = unsafe { core::slice::from_raw_parts(p, n) };
                    let v = s[..s.iter().position(|&c| c == 0).unwrap_or(n)].to_vec();
                    unsafe { GlobalUnlock(h as *mut u8) };
                    v
                })
            }
            (_, Some(h)) if h != 0 => {
                let p = unsafe { GlobalLock(h as *mut u8) };
                (!p.is_null()).then(|| {
                    let v = unsafe { crate::from_ansi(p, -1) };
                    unsafe { GlobalUnlock(h as *mut u8) };
                    v
                })
            }
            _ => None,
        };
        if let Some(u) = units {
            unsafe { VeilClipSet(u.as_ptr(), u.len()) };
            let (_, seq) = kernel_text();
            CLIP.lock().seq = seq;
        }
    }
    1
}

#[no_mangle]
pub extern "system" fn EmptyClipboard() -> Bool {
    let old = {
        let mut c = CLIP.lock();
        let Some(w) = c.open else {
            crate::set_error(1418); // ERROR_CLIPBOARD_NOT_OPEN
            return 0;
        };
        c.owner = w;
        core::mem::take(&mut c.data)
    };
    for (_, p) in old {
        if p != 0 {
            unsafe { GlobalFree(p as *mut u8) };
        }
    }
    1
}

#[no_mangle]
pub extern "system" fn SetClipboardData(format: u32, data: u64) -> u64 {
    let mut c = CLIP.lock();
    if c.open.is_none() {
        crate::set_error(1418);
        return 0;
    }
    let old = match c.data.iter_mut().find(|e| e.0 == format) {
        Some(e) => core::mem::replace(&mut e.1, data),
        None => {
            c.data.push((format, data));
            0
        }
    };
    if is_text(format) {
        c.text_dirty = true;
    }
    drop(c);
    if old != 0 && old != data {
        unsafe { GlobalFree(old as *mut u8) };
    }
    data
}

/// The clipboard's text in format `f` as a fresh block (kept with the
/// clipboard's data).
fn text_block(f: u32, text: &[u16]) -> u64 {
    let bytes: Vec<u8> = if f == CF_UNICODETEXT { text.iter().flat_map(|c| c.to_le_bytes()).collect() } else { crate::to_ansi(text) };
    let unit = if f == CF_UNICODETEXT { 2 } else { 1 };
    // Moveable, as on Windows (programs GlobalLock what they get).
    let h = unsafe { GlobalAlloc(GMEM_MOVEABLE | GMEM_ZEROINIT, bytes.len() + unit) };
    let p = if h.is_null() { h } else { unsafe { GlobalLock(h) } };
    if !p.is_null() {
        unsafe {
            core::ptr::copy_nonoverlapping(bytes.as_ptr(), p, bytes.len());
            GlobalUnlock(h);
        }
    }
    h as u64
}

#[no_mangle]
pub extern "system" fn GetClipboardData(format: u32) -> u64 {
    if CLIP.lock().open.is_none() {
        crate::set_error(1418);
        return 0;
    }
    if is_text(format) {
        let (text, seq) = kernel_text();
        let mut c = CLIP.lock();
        // Our own text if nobody copied since; else the kernel's.
        if seq == c.seq {
            if let Some(e) = c.data.iter().find(|e| e.0 == format) {
                return e.1;
            }
        }
        if text.is_empty() {
            return 0;
        }
        if seq != c.seq {
            // Someone else's: replace whatever text we held.
            let stale: Vec<u64> = c.data.iter().filter(|e| is_text(e.0)).map(|e| e.1).collect();
            c.data.retain(|e| !is_text(e.0));
            for p in stale {
                unsafe { GlobalFree(p as *mut u8) };
            }
            c.seq = seq;
        }
        let block = text_block(format, &text);
        c.data.push((format, block));
        return block;
    }
    if format == CF_LOCALE {
        return 0;
    }
    CLIP.lock().data.iter().find(|e| e.0 == format).map_or(0, |e| e.1)
}

#[no_mangle]
pub extern "system" fn IsClipboardFormatAvailable(format: u32) -> Bool {
    if is_text(format) {
        let (text, _) = kernel_text();
        if !text.is_empty() {
            return 1;
        }
    }
    CLIP.lock().data.iter().any(|e| e.0 == format) as Bool
}

/// This program's formats, then the text ones if the kernel has text.
fn formats() -> Vec<u32> {
    let mut f: Vec<u32> = CLIP.lock().data.iter().map(|e| e.0).collect();
    if !kernel_text().0.is_empty() {
        for t in [CF_UNICODETEXT, CF_TEXT, CF_OEMTEXT] {
            if !f.contains(&t) {
                f.push(t);
            }
        }
    }
    f
}

#[no_mangle]
pub extern "system" fn EnumClipboardFormats(after: u32) -> u32 {
    let f = formats();
    if after == 0 {
        return f.first().copied().unwrap_or(0);
    }
    f.iter().position(|&x| x == after).and_then(|i| f.get(i + 1)).copied().unwrap_or(0)
}

#[no_mangle]
pub extern "system" fn CountClipboardFormats() -> i32 {
    formats().len() as i32
}

#[no_mangle]
pub unsafe extern "system" fn GetPriorityClipboardFormat(list: *const u32, n: i32) -> i32 {
    let f = formats();
    if f.is_empty() {
        return 0;
    }
    for i in 0..n.max(0) as usize {
        let x = unsafe { *list.add(i) };
        if f.contains(&x) {
            return x as i32;
        }
    }
    -1
}

#[no_mangle]
pub extern "system" fn GetClipboardOwner() -> Hwnd {
    CLIP.lock().owner
}

#[no_mangle]
pub extern "system" fn GetOpenClipboardWindow() -> Hwnd {
    CLIP.lock().open.unwrap_or(0)
}

#[no_mangle]
pub extern "system" fn GetClipboardSequenceNumber() -> u32 {
    kernel_text().1
}

/// Clipboard change notifications don't come yet.
#[no_mangle]
pub extern "system" fn AddClipboardFormatListener(_hwnd: Hwnd) -> Bool {
    1
}

#[no_mangle]
pub extern "system" fn RemoveClipboardFormatListener(_hwnd: Hwnd) -> Bool {
    1
}

#[no_mangle]
pub extern "system" fn SetClipboardViewer(_hwnd: Hwnd) -> Hwnd {
    0
}

#[no_mangle]
pub extern "system" fn ChangeClipboardChain(_remove: Hwnd, _next: Hwnd) -> Bool {
    1
}

/// The clipboard's text, for the edit control's paste.
pub fn paste_text() -> Vec<u16> {
    kernel_text().0
}

/// Put text on the clipboard, for the edit control's copy.
pub fn copy_text(owner: Hwnd, text: &[u16]) {
    if OpenClipboard(owner) == 0 {
        return;
    }
    EmptyClipboard();
    let block = text_block(CF_UNICODETEXT, text);
    SetClipboardData(CF_UNICODETEXT, block);
    CloseClipboard();
}
