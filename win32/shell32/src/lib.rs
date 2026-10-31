//! VEILOS `shell32.dll`: the shell basics programs reach for (WIN32_DESIGN.md,
//! G4): the About box, drag and drop (nothing is dropped yet), known and
//! special folders (all under C:\home), ShellExecute for programs,
//! CommandLineToArgvW, icon extraction, and the shell's COM entry points
//! (no shell items: they answer E_NOTIMPL, and programs fall back).

#![no_std]

extern crate alloc;

#[path = "../../dllrt.rs"]
#[allow(dead_code)]
mod dllrt;

use alloc::vec::Vec;

type Hwnd = u64;
type Bool = i32;
type HResult = i32;

const E_NOTIMPL: HResult = 0x8000_4001u32 as i32;
const E_INVALIDARG: HResult = 0x8007_0057u32 as i32;
const S_OK: HResult = 0;

#[link(name = "kernel32", kind = "raw-dylib")]
unsafe extern "system" {
    fn CreateProcessW(app: *const u16, cmd: *mut u16, psa: usize, tsa: usize, inherit: Bool, flags: u32, env: usize,
                      dir: *const u16, si: *const u8, pi: *mut u8) -> Bool;
    fn CloseHandle(h: u64) -> Bool;
    fn CoTaskMemAlloc(size: usize) -> *mut u8;
    fn LocalAlloc(flags: u32, size: usize) -> *mut u8;
    fn GetModuleHandleW(name: *const u16) -> u64;
    fn LoadLibraryExW(name: *const u16, file: u64, flags: u32) -> u64;
    fn FreeLibrary(module: u64) -> Bool;
    fn CreateDirectoryW(name: *const u16, sa: usize) -> Bool;
}

#[link(name = "user32", kind = "raw-dylib")]
unsafe extern "system" {
    fn MessageBoxW(hwnd: Hwnd, text: *const u16, caption: *const u16, kind: u32) -> i32;
    fn LoadImageW(inst: u64, name: *const u16, kind: u32, cx: i32, cy: i32, flags: u32) -> u64;
    fn LoadIconW(inst: u64, name: *const u16) -> u64;
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(core::iter::once(0)).collect()
}

unsafe fn wstr(p: *const u16) -> Vec<u16> {
    if p.is_null() {
        return Vec::new();
    }
    (0..).map(|i| unsafe { *p.add(i) }).take_while(|&c| c != 0).collect()
}

unsafe fn ansi(p: *const u8) -> Vec<u16> {
    if p.is_null() {
        return Vec::new();
    }
    let bytes: Vec<u8> = (0..).map(|i| unsafe { *p.add(i) }).take_while(|&c| c != 0).collect();
    alloc::string::String::from_utf8_lossy(&bytes).encode_utf16().collect()
}

#[no_mangle]
pub extern "system" fn DllMain(_module: usize, _reason: u32, _reserved: usize) -> i32 {
    1
}

// -----| the About box |-----

/// "Windows" (or the caption before a '#') and VEILOS's version, then
/// the program's own lines.
fn about(hwnd: Hwnd, app: &[u16], other: &[u16]) -> Bool {
    let app_s = alloc::string::String::from_utf16_lossy(app);
    let (caption, title) = match app_s.find('#') {
        Some(i) => (alloc::string::String::from(&app_s[..i]), alloc::string::String::from(&app_s[i + 1..])),
        None => (alloc::format!("About {}", app_s), app_s.clone()),
    };
    let mut text = alloc::format!("{}\n\nVEILOS, running Windows programs\nVersion 10.0 (Build 19041)", title);
    if !other.is_empty() {
        text.push_str("\n\n");
        text.push_str(&alloc::string::String::from_utf16_lossy(other));
    }
    let (t, c) = (wide(&text), wide(&caption));
    unsafe { MessageBoxW(hwnd, t.as_ptr(), c.as_ptr(), 0x40) };
    1
}

#[no_mangle]
pub unsafe extern "system" fn ShellAboutW(hwnd: Hwnd, app: *const u16, other: *const u16, _icon: u64) -> Bool {
    about(hwnd, &unsafe { wstr(app) }, &unsafe { wstr(other) })
}

#[no_mangle]
pub unsafe extern "system" fn ShellAboutA(hwnd: Hwnd, app: *const u8, other: *const u8, _icon: u64) -> Bool {
    about(hwnd, &unsafe { ansi(app) }, &unsafe { ansi(other) })
}

// -----| drag and drop: nothing is dropped yet |-----

#[no_mangle]
pub extern "system" fn DragAcceptFiles(_hwnd: Hwnd, _accept: Bool) {}

#[no_mangle]
pub extern "system" fn DragQueryFileW(_drop: u64, _i: u32, _out: *mut u16, _cap: u32) -> u32 {
    0
}

#[no_mangle]
pub extern "system" fn DragQueryFileA(_drop: u64, _i: u32, _out: *mut u8, _cap: u32) -> u32 {
    0
}

#[no_mangle]
pub extern "system" fn DragQueryPoint(_drop: u64, _pt: *mut [i32; 2]) -> Bool {
    0
}

#[no_mangle]
pub extern "system" fn DragFinish(_drop: u64) {}

// -----| folders |-----

/// Every user folder is the home folder; system folders are C:\SYSTEM.
fn folder_for_csidl(csidl: u32) -> &'static str {
    match csidl & 0xFF {
        0x24 | 0x25 | 0x29 | 0x2A => "C:\\system", // Windows, System, SystemX86, Program Files ...
        0x26 | 0x2B => "C:\\",
        0x10 | 0x19 => "C:\\desktop",               // Desktop
        _ => "C:\\home",
    }
}

fn copy_path(path: &str, out: *mut u16) {
    let w = wide(path);
    unsafe { core::ptr::copy_nonoverlapping(w.as_ptr(), out, w.len()) };
}

#[no_mangle]
pub unsafe extern "system" fn SHGetFolderPathW(_hwnd: Hwnd, csidl: i32, _token: u64, _flags: u32, out: *mut u16) -> HResult {
    if out.is_null() {
        return E_INVALIDARG;
    }
    let p = folder_for_csidl(csidl as u32);
    if csidl & 0x8000 != 0 {
        // CSIDL_FLAG_CREATE
        let w = wide(p);
        unsafe { CreateDirectoryW(w.as_ptr(), 0) };
    }
    copy_path(p, out);
    S_OK
}

#[no_mangle]
pub unsafe extern "system" fn SHGetFolderPathA(hwnd: Hwnd, csidl: i32, token: u64, flags: u32, out: *mut u8) -> HResult {
    let mut w = [0u16; 260];
    let r = unsafe { SHGetFolderPathW(hwnd, csidl, token, flags, w.as_mut_ptr()) };
    if r == S_OK {
        let n = w.iter().position(|&c| c == 0).unwrap_or(0);
        for i in 0..=n {
            unsafe { *out.add(i) = w[i] as u8 };
        }
    }
    r
}

#[no_mangle]
pub unsafe extern "system" fn SHGetSpecialFolderPathW(_hwnd: Hwnd, out: *mut u16, csidl: i32, _create: Bool) -> Bool {
    if out.is_null() {
        return 0;
    }
    copy_path(folder_for_csidl(csidl as u32), out);
    1
}

/// SHGetKnownFolderPath: a CoTaskMemAlloc'd path.
#[no_mangle]
pub unsafe extern "system" fn SHGetKnownFolderPath(id: *const [u8; 16], _flags: u32, _token: u64, out: *mut *mut u16) -> HResult {
    if out.is_null() {
        return E_INVALIDARG;
    }
    // FOLDERID_Windows, FOLDERID_System, FOLDERID_ProgramFiles start
    // with these bytes; the rest are the user's.
    let first = if id.is_null() { 0 } else { unsafe { u32::from_le_bytes([(*id)[0], (*id)[1], (*id)[2], (*id)[3]]) } };
    let p = match first {
        0xF38BF404 | 0x1AC14E77 | 0x905E63B6 | 0xD65231B0 => "C:\\system",
        0xB4BFCC3A => "C:\\desktop",
        _ => "C:\\home",
    };
    let w = wide(p);
    let mem = unsafe { CoTaskMemAlloc(w.len() * 2) } as *mut u16;
    if mem.is_null() {
        return 0x8007_000Eu32 as i32;
    }
    unsafe {
        core::ptr::copy_nonoverlapping(w.as_ptr(), mem, w.len());
        *out = mem;
    }
    S_OK
}

/// No shell namespace: item IDs and shell items aren't there yet.
#[no_mangle]
pub unsafe extern "system" fn SHCreateItemFromParsingName(_path: *const u16, _bc: usize, _iid: *const u8, out: *mut usize) -> HResult {
    if !out.is_null() {
        unsafe { *out = 0 };
    }
    E_NOTIMPL
}

#[no_mangle]
pub unsafe extern "system" fn SHGetSpecialFolderLocation(_hwnd: Hwnd, _csidl: i32, out: *mut usize) -> HResult {
    if !out.is_null() {
        unsafe { *out = 0 };
    }
    E_NOTIMPL
}

#[no_mangle]
pub extern "system" fn SHGetPathFromIDListW(_pidl: usize, _out: *mut u16) -> Bool {
    0
}

#[no_mangle]
pub extern "system" fn SHBrowseForFolderW(_bi: *const u8) -> usize {
    0
}

#[no_mangle]
pub extern "system" fn SHAddToRecentDocs(_flags: u32, _pv: usize) {}

#[no_mangle]
pub extern "system" fn SHChangeNotify(_event: i32, _flags: u32, _a: usize, _b: usize) {}

#[no_mangle]
pub extern "system" fn ILFree(_pidl: usize) {}

// -----| running things |-----

/// Programs (.exe) start; documents need file associations, which don't
/// exist yet (SE_ERR_NOASSOC).
fn execute(file: &[u16], params: &[u16], dir: &[u16]) -> u64 {
    let lower: Vec<u16> = file.iter().map(|&c| if (0x41..=0x5A).contains(&c) { c + 32 } else { c }).collect();
    let is_exe = lower.ends_with(&".exe".encode_utf16().collect::<Vec<u16>>()) || !lower.contains(&(b'.' as u16));
    if !is_exe {
        return 31;
    }
    let mut cmd: Vec<u16> = Vec::new();
    cmd.push(b'"' as u16);
    cmd.extend_from_slice(file);
    cmd.push(b'"' as u16);
    if !params.is_empty() {
        cmd.push(b' ' as u16);
        cmd.extend_from_slice(params);
    }
    cmd.push(0);
    let mut dirz = dir.to_vec();
    dirz.push(0);
    // STARTUPINFOW: cb first.
    let mut si = [0u8; 104];
    si[0..4].copy_from_slice(&104u32.to_le_bytes());
    let mut pi = [0u8; 24];
    let ok = unsafe {
        CreateProcessW(core::ptr::null(), cmd.as_mut_ptr(), 0, 0, 0, 0, 0, if dir.is_empty() { core::ptr::null() } else { dirz.as_ptr() }, si.as_ptr(), pi.as_mut_ptr())
    };
    if ok == 0 {
        return 2; // SE_ERR_FNF
    }
    unsafe {
        CloseHandle(u64::from_le_bytes(pi[0..8].try_into().unwrap()));
        CloseHandle(u64::from_le_bytes(pi[8..16].try_into().unwrap()));
    }
    33
}

#[no_mangle]
pub unsafe extern "system" fn ShellExecuteW(_hwnd: Hwnd, _op: *const u16, file: *const u16, params: *const u16, dir: *const u16, _show: i32) -> u64 {
    execute(&unsafe { wstr(file) }, &unsafe { wstr(params) }, &unsafe { wstr(dir) })
}

#[no_mangle]
pub unsafe extern "system" fn ShellExecuteA(_hwnd: Hwnd, _op: *const u8, file: *const u8, params: *const u8, dir: *const u8, _show: i32) -> u64 {
    execute(&unsafe { ansi(file) }, &unsafe { ansi(params) }, &unsafe { ansi(dir) })
}

/// SHELLEXECUTEINFOW: cbSize, fMask, hwnd, verb, file, parameters,
/// directory, nShow, hInstApp, ...
#[no_mangle]
pub unsafe extern "system" fn ShellExecuteExW(info: *mut u8) -> Bool {
    if info.is_null() {
        return 0;
    }
    let rd = |o: usize| unsafe { *(info.add(o) as *const u64) };
    let r = execute(&unsafe { wstr(rd(24) as *const u16) }, &unsafe { wstr(rd(32) as *const u16) }, &unsafe { wstr(rd(40) as *const u16) });
    unsafe { *(info.add(56) as *mut u64) = r };
    (r > 32) as Bool
}

/// Split a command line as the C runtime does; one LocalAlloc'd block of
/// pointers then strings.
#[no_mangle]
pub unsafe extern "system" fn CommandLineToArgvW(line: *const u16, count: *mut i32) -> *mut *mut u16 {
    let s = unsafe { wstr(line) };
    let mut args: Vec<Vec<u16>> = Vec::new();
    let mut i = 0;
    let q = b'"' as u16;
    let bs = b'\\' as u16;
    // The program name: up to the closing quote or a space, no escapes.
    if !s.is_empty() {
        let mut a = Vec::new();
        if s[0] == q {
            i = 1;
            while i < s.len() && s[i] != q {
                a.push(s[i]);
                i += 1;
            }
            i += 1;
        } else {
            while i < s.len() && s[i] > 32 {
                a.push(s[i]);
                i += 1;
            }
        }
        args.push(a);
    }
    loop {
        while i < s.len() && (s[i] == 32 || s[i] == 9) {
            i += 1;
        }
        if i >= s.len() {
            break;
        }
        let mut a = Vec::new();
        let mut quoted = false;
        while i < s.len() && (quoted || (s[i] != 32 && s[i] != 9)) {
            if s[i] == bs {
                let mut n = 0;
                while i < s.len() && s[i] == bs {
                    n += 1;
                    i += 1;
                }
                if i < s.len() && s[i] == q {
                    a.extend(core::iter::repeat_n(bs, n / 2));
                    if n % 2 == 1 {
                        a.push(q);
                        i += 1;
                    }
                } else {
                    a.extend(core::iter::repeat_n(bs, n));
                }
            } else if s[i] == q {
                if quoted && i + 1 < s.len() && s[i + 1] == q {
                    a.push(q);
                    i += 2;
                    continue;
                }
                quoted = !quoted;
                i += 1;
            } else {
                a.push(s[i]);
                i += 1;
            }
        }
        args.push(a);
    }
    let ptrs = (args.len() + 1) * 8;
    let chars: usize = args.iter().map(|a| a.len() + 1).sum();
    let block = unsafe { LocalAlloc(0x40, ptrs + chars * 2) };
    if block.is_null() {
        return core::ptr::null_mut();
    }
    let table = block as *mut *mut u16;
    let mut text = unsafe { block.add(ptrs) } as *mut u16;
    for (k, a) in args.iter().enumerate() {
        unsafe {
            *table.add(k) = text;
            core::ptr::copy_nonoverlapping(a.as_ptr(), text, a.len());
            *text.add(a.len()) = 0;
            text = text.add(a.len() + 1);
        }
    }
    if !count.is_null() {
        unsafe { *count = args.len() as i32 };
    }
    table
}

// -----| icons from files |-----

/// The `index`th icon of a module (only the first is found by index 0;
/// negative indexes are resource ids).
fn extract(file: &[u16], index: i32, size: i32) -> u64 {
    let mut name = file.to_vec();
    name.push(0);
    let module = unsafe { LoadLibraryExW(name.as_ptr(), 0, 0x2) }; // LOAD_LIBRARY_AS_DATAFILE
    let module = if module == 0 { unsafe { GetModuleHandleW(core::ptr::null()) } } else { module };
    let id = if index < 0 { (-index) as usize } else { 1 + index as usize };
    let h = unsafe { LoadImageW(module, id as *const u16, 1, size, size, 0) };
    if h == 0 && index == 0 {
        return unsafe { LoadIconW(0, 32512usize as *const u16) };
    }
    unsafe { FreeLibrary(module) };
    h
}

#[no_mangle]
pub unsafe extern "system" fn ExtractIconW(_inst: u64, file: *const u16, index: u32) -> u64 {
    extract(&unsafe { wstr(file) }, index as i32, 32)
}

#[no_mangle]
pub unsafe extern "system" fn ExtractIconExW(file: *const u16, index: i32, large: *mut u64, small: *mut u64, n: u32) -> u32 {
    let f = unsafe { wstr(file) };
    if index == -1 {
        return 1;
    }
    unsafe {
        if !large.is_null() && n > 0 {
            *large = extract(&f, index, 32);
        }
        if !small.is_null() && n > 0 {
            *small = extract(&f, index, 16);
        }
    }
    1
}

/// SHFILEINFOW: hIcon, iIcon, dwAttributes, szDisplayName[260],
/// szTypeName[80]: the display name is the file name.
#[no_mangle]
pub unsafe extern "system" fn SHGetFileInfoW(path: *const u16, _attrs: u32, info: *mut u8, size: u32, flags: u32) -> usize {
    if info.is_null() || (size as usize) < 696 {
        return 0;
    }
    unsafe { core::ptr::write_bytes(info, 0, 696) };
    let p = unsafe { wstr(path) };
    let name_at = p.iter().rposition(|&c| c == b'\\' as u16 || c == b'/' as u16).map_or(0, |i| i + 1);
    let name = &p[name_at..];
    unsafe { core::ptr::copy_nonoverlapping(name.as_ptr(), info.add(16) as *mut u16, name.len().min(259)) };
    if flags & 0x100 != 0 {
        // SHGFI_ICON: the application icon.
        unsafe { *(info as *mut u64) = LoadIconW(0, 32512usize as *const u16) };
    }
    1
}

/// No notification area yet.
#[no_mangle]
pub extern "system" fn Shell_NotifyIconW(_msg: u32, _data: *const u8) -> Bool {
    0
}

#[no_mangle]
pub extern "system" fn SHFileOperationW(_op: *mut u8) -> i32 {
    0x78 // DE_ACCESSDENIEDSRC: not done
}

#[no_mangle]
pub extern "system" fn SHGetMalloc(out: *mut usize) -> HResult {
    if !out.is_null() {
        unsafe { *out = 0 };
    }
    E_NOTIMPL
}
