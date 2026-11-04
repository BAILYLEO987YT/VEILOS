//! INI files (`GetPrivateProfile*`, `WritePrivateProfile*`), and the version
//! resources of files (version.dll forwards here). Both read the file each
//! time, as there's nothing to keep in step with.

use crate::util::{alloc, free, widen, wstr, WBuf};
use crate::{Bool, Handle};

const GENERIC_READ: u32 = 0x8000_0000;
const GENERIC_WRITE: u32 = 0x4000_0000;
const FILE_SHARE_ALL: u32 = 7;
const OPEN_EXISTING: u32 = 3;
const CREATE_ALWAYS: u32 = 2;
const INVALID: Handle = u64::MAX;
const ERROR_FILE_NOT_FOUND: u32 = 2;
const ERROR_INVALID_PARAMETER: u32 = 87;
const ERROR_RESOURCE_DATA_NOT_FOUND: u32 = 1812;
const ERROR_INSUFFICIENT_BUFFER: u32 = 122;

/// A file's bytes in a block from `alloc`.
struct Bytes {
    p: *mut u8,
    n: usize,
}

impl Bytes {
    fn as_slice(&self) -> &[u8] {
        if self.p.is_null() { &[] } else { unsafe { core::slice::from_raw_parts(self.p, self.n) } }
    }
}

impl Drop for Bytes {
    fn drop(&mut self) {
        if !self.p.is_null() {
            free(self.p);
        }
    }
}

fn read_file(path: *const u16) -> Option<Bytes> {
    let h = unsafe { crate::file::CreateFileW(path, GENERIC_READ, FILE_SHARE_ALL, 0, OPEN_EXISTING, 0x80, 0) };
    if h == INVALID {
        return None;
    }
    let mut size = 0i64;
    unsafe { crate::file::GetFileSizeEx(h, &mut size) };
    let n = size.max(0) as usize;
    let p = alloc(n.max(1));
    if p.is_null() {
        crate::CloseHandle(h);
        return None;
    }
    let mut at = 0usize;
    while at < n {
        let mut got = 0u32;
        let ok = unsafe { crate::ReadFile(h, p.add(at), (n - at) as u32, &mut got, 0) };
        if ok == 0 || got == 0 {
            break;
        }
        at += got as usize;
    }
    crate::CloseHandle(h);
    Some(Bytes { p, n: at })
}

// -----| INI files |-----

/// A file name without a path means the Windows directory, as on Windows.
fn ini_path(name: *const u16, out: &mut [u16; 520]) {
    let s = unsafe { wstr(name) };
    let has_path = s.iter().any(|&c| c == b'\\' as u16 || c == b'/' as u16 || c == b':' as u16);
    let mut at = 0;
    if !has_path {
        for c in "C:\\system\\".encode_utf16() {
            out[at] = c;
            at += 1;
        }
    }
    for &c in s.iter().take(510 - at) {
        out[at] = c;
        at += 1;
    }
    out[at] = 0;
}

fn lower(c: u8) -> u8 {
    c.to_ascii_lowercase()
}

fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| lower(*x) == lower(*y))
}

fn trim(mut s: &[u8]) -> &[u8] {
    while let [b' ' | b'\t' | b'\r', rest @ ..] = s {
        s = rest;
    }
    while let [rest @ .., b' ' | b'\t' | b'\r'] = s {
        s = rest;
    }
    s
}

/// One line at a time, with where each starts.
struct Lines<'a> {
    d: &'a [u8],
    at: usize,
}

impl<'a> Iterator for Lines<'a> {
    type Item = (usize, &'a [u8]);
    fn next(&mut self) -> Option<Self::Item> {
        if self.at >= self.d.len() {
            return None;
        }
        let start = self.at;
        let rest = &self.d[start..];
        let n = rest.iter().position(|&c| c == b'\n').unwrap_or(rest.len());
        self.at = start + n + 1;
        Some((start, &rest[..n]))
    }
}

fn section_of(line: &[u8]) -> Option<&[u8]> {
    let l = trim(line);
    if l.first() != Some(&b'[') {
        return None;
    }
    let end = l.iter().position(|&c| c == b']').unwrap_or(l.len());
    Some(trim(&l[1..end]))
}

/// (key, value) of a line, unless it's a comment or has no `=`.
fn entry_of(line: &[u8]) -> Option<(&[u8], &[u8])> {
    let l = trim(line);
    if l.first().is_none_or(|&c| c == b';') {
        return None;
    }
    let eq = l.iter().position(|&c| c == b'=')?;
    Some((trim(&l[..eq]), trim(&l[eq + 1..])))
}

/// A value without the quotes around it.
fn unquote(v: &[u8]) -> &[u8] {
    if v.len() >= 2 && (v[0] == b'"' || v[0] == b'\'') && v[v.len() - 1] == v[0] {
        &v[1..v.len() - 1]
    } else {
        v
    }
}

/// Copy with a NUL, cut to fit: the characters kept.
fn put(out: *mut u8, cap: u32, s: &[u8]) -> u32 {
    if out.is_null() || cap == 0 {
        return 0;
    }
    let n = s.len().min(cap as usize - 1);
    unsafe {
        core::ptr::copy_nonoverlapping(s.as_ptr(), out, n);
        *out.add(n) = 0;
    }
    n as u32
}

/// Names, each with a NUL, ending in a second NUL; cut names at the end still
/// end in two (returning `cap - 2`).
fn put_list<'a>(out: *mut u8, cap: u32, names: impl Iterator<Item = &'a [u8]>) -> u32 {
    if out.is_null() || cap == 0 {
        return 0;
    }
    if cap == 1 {
        unsafe { *out = 0 };
        return 0;
    }
    let cap = cap as usize;
    let mut at = 0usize;
    for n in names {
        if at + n.len() + 2 > cap {
            let room = (cap - 2).saturating_sub(at);
            unsafe {
                core::ptr::copy_nonoverlapping(n.as_ptr(), out.add(at), room);
                *out.add(at + room) = 0;
                *out.add(at + room + 1) = 0;
            }
            return (cap - 2) as u32;
        }
        unsafe {
            core::ptr::copy_nonoverlapping(n.as_ptr(), out.add(at), n.len());
            *out.add(at + n.len()) = 0;
        }
        at += n.len() + 1;
    }
    unsafe { *out.add(at) = 0 };
    at as u32
}

/// A's narrow string, NUL-free; null stays None.
unsafe fn cstr<'a>(p: *const u8) -> Option<&'a [u8]> {
    if p.is_null() {
        return None;
    }
    let mut n = 0;
    while unsafe { *p.add(n) } != 0 {
        n += 1;
    }
    Some(unsafe { core::slice::from_raw_parts(p, n) })
}

/// `GetPrivateProfileString` on narrow strings, with the path already wide.
unsafe fn get_string(sec: Option<&[u8]>, key: Option<&[u8]>, def: Option<&[u8]>, out: *mut u8, cap: u32, path: *const u16) -> u32 {
    let file = read_file(path);
    if file.is_none() {
        crate::SetLastError(ERROR_FILE_NOT_FOUND);
    }
    let d = file.as_ref().map_or(&[][..], |b| b.as_slice());
    let lines = || Lines { d, at: 0 };
    let Some(sec) = sec else {
        // All the sections.
        return put_list(out, cap, lines().filter_map(|(_, l)| section_of(l)));
    };
    let in_section = || {
        let mut inside = false;
        lines().filter_map(move |(_, l)| {
            if let Some(s) = section_of(l) {
                inside = same(s, sec);
                return None;
            }
            if inside { Some(l) } else { None }
        })
    };
    let Some(key) = key else {
        // All its keys.
        return put_list(out, cap, in_section().filter_map(|l| entry_of(l).map(|e| e.0)));
    };
    for l in in_section() {
        if let Some((k, v)) = entry_of(l) {
            if same(k, key) {
                return put(out, cap, unquote(v));
            }
        }
    }
    // Not there: the default, without trailing spaces.
    let mut dv = def.unwrap_or(&[]);
    while let [rest @ .., b' '] = dv {
        dv = rest;
    }
    put(out, cap, dv)
}

#[no_mangle]
pub unsafe extern "system" fn GetPrivateProfileStringA(sec: *const u8, key: *const u8, def: *const u8, out: *mut u8, cap: u32, file: *const u8) -> u32 {
    let w = unsafe { widen(file) };
    let mut path = [0u16; 520];
    ini_path(w.as_ptr(), &mut path);
    unsafe { get_string(cstr(sec), cstr(key), cstr(def), out, cap, path.as_ptr()) }
}

/// Wide to narrow for the file's sake: a block in `alloc` memory.
struct Narrow(*mut u8);

impl Narrow {
    fn new(p: *const u16) -> Option<Narrow> {
        if p.is_null() {
            return None;
        }
        let s = unsafe { wstr(p) };
        let b = alloc(s.len() * 3 + 1);
        let n = unsafe { crate::locale::WideCharToMultiByte(0, 0, s.as_ptr(), s.len() as i32, b, s.len() as i32 * 3, core::ptr::null(), core::ptr::null_mut()) }.max(0);
        unsafe { *b.add(n as usize) = 0 };
        Some(Narrow(b))
    }
    fn bytes(&self) -> &[u8] {
        unsafe { cstr(self.0) }.unwrap_or(&[])
    }
}

impl Drop for Narrow {
    fn drop(&mut self) {
        free(self.0);
    }
}

fn narrow_opt(n: &Option<Narrow>) -> Option<&[u8]> {
    n.as_ref().map(|x| x.bytes())
}

/// Bytes (with embedded NULs, for lists) to wide: characters written with the
/// final NUL excluded.
unsafe fn widen_out(tmp: *const u8, n: u32, list: bool, out: *mut u16, cap: u32) -> u32 {
    let units = n as usize + if list { 2 } else { 1 };
    let k = unsafe { crate::locale::MultiByteToWideChar(0, 0, tmp, units as i32, out, cap as i32) }.max(0) as u32;
    k.saturating_sub(if list { 2 } else { 1 })
}

#[no_mangle]
pub unsafe extern "system" fn GetPrivateProfileStringW(sec: *const u16, key: *const u16, def: *const u16, out: *mut u16, cap: u32, file: *const u16) -> u32 {
    if out.is_null() || cap == 0 {
        return 0;
    }
    let mut path = [0u16; 520];
    ini_path(file, &mut path);
    let (s, k, d) = (Narrow::new(sec), Narrow::new(key), Narrow::new(def));
    let tmp = alloc(cap as usize * 3 + 2);
    let n = unsafe { get_string(narrow_opt(&s), narrow_opt(&k), narrow_opt(&d), tmp, cap, path.as_ptr()) };
    let list = sec.is_null() || key.is_null();
    let r = unsafe { widen_out(tmp, n, list, out, cap) };
    free(tmp);
    r
}

#[no_mangle]
pub unsafe extern "system" fn GetPrivateProfileIntA(sec: *const u8, key: *const u8, def: i32, file: *const u8) -> u32 {
    let mut buf = [0u8; 64];
    let n = unsafe { GetPrivateProfileStringA(sec, key, core::ptr::null(), buf.as_mut_ptr(), 64, file) };
    parse_int(&buf[..n as usize], def)
}

#[no_mangle]
pub unsafe extern "system" fn GetPrivateProfileIntW(sec: *const u16, key: *const u16, def: i32, file: *const u16) -> u32 {
    let mut buf = [0u16; 64];
    let n = unsafe { GetPrivateProfileStringW(sec, key, core::ptr::null(), buf.as_mut_ptr(), 64, file) } as usize;
    let mut b = [0u8; 64];
    for i in 0..n {
        b[i] = buf[i] as u8;
    }
    parse_int(&b[..n], def)
}

/// The number the text starts with (0x for hex, a sign); the default only
/// when there's no text at all.
fn parse_int(s: &[u8], def: i32) -> u32 {
    if s.is_empty() {
        return def as u32;
    }
    let (neg, rest) = if s[0] == b'-' { (true, &s[1..]) } else { (false, s) };
    let (base, digits) = if rest.len() > 2 && rest[0] == b'0' && (rest[1] | 32) == b'x' { (16u32, &rest[2..]) } else { (10, rest) };
    let mut v = 0u32;
    for &c in digits {
        let Some(d) = (c as char).to_digit(base) else { break };
        v = v.wrapping_mul(base).wrapping_add(d);
    }
    if neg { v.wrapping_neg() } else { v }
}

/// The lines of a section, as key=value, each with a NUL.
unsafe fn get_section(sec: &[u8], out: *mut u8, cap: u32, path: *const u16) -> u32 {
    let file = read_file(path);
    let d = file.as_ref().map_or(&[][..], |b| b.as_slice());
    let mut inside = false;
    let body = Lines { d, at: 0 }.filter_map(|(_, l)| {
        if let Some(s) = section_of(l) {
            inside = same(s, sec);
            return None;
        }
        let t = trim(l);
        if inside && !t.is_empty() && t[0] != b';' { Some(t) } else { None }
    });
    put_list(out, cap, body)
}

#[no_mangle]
pub unsafe extern "system" fn GetPrivateProfileSectionA(sec: *const u8, out: *mut u8, cap: u32, file: *const u8) -> u32 {
    let w = unsafe { widen(file) };
    let mut path = [0u16; 520];
    ini_path(w.as_ptr(), &mut path);
    unsafe { get_section(cstr(sec).unwrap_or(&[]), out, cap, path.as_ptr()) }
}

#[no_mangle]
pub unsafe extern "system" fn GetPrivateProfileSectionW(sec: *const u16, out: *mut u16, cap: u32, file: *const u16) -> u32 {
    if out.is_null() || cap == 0 {
        return 0;
    }
    let mut path = [0u16; 520];
    ini_path(file, &mut path);
    let s = Narrow::new(sec);
    let tmp = alloc(cap as usize * 3 + 2);
    let n = unsafe { get_section(narrow_opt(&s).unwrap_or(&[]), tmp, cap, path.as_ptr()) };
    let r = unsafe { widen_out(tmp, n, true, out, cap) };
    free(tmp);
    r
}

#[no_mangle]
pub unsafe extern "system" fn GetPrivateProfileSectionNamesA(out: *mut u8, cap: u32, file: *const u8) -> u32 {
    unsafe { GetPrivateProfileStringA(core::ptr::null(), core::ptr::null(), core::ptr::null(), out, cap, file) }
}

#[no_mangle]
pub unsafe extern "system" fn GetPrivateProfileSectionNamesW(out: *mut u16, cap: u32, file: *const u16) -> u32 {
    unsafe { GetPrivateProfileStringW(core::ptr::null(), core::ptr::null(), core::ptr::null(), out, cap, file) }
}

// -----| writing |-----

/// Replace the file's contents with these pieces.
fn write_pieces(path: *const u16, pieces: &[&[u8]]) -> Bool {
    let h = unsafe { crate::file::CreateFileW(path, GENERIC_WRITE, 0, 0, CREATE_ALWAYS, 0x80, 0) };
    if h == INVALID {
        return 0;
    }
    let mut ok = 1;
    for p in pieces {
        let mut done = 0u32;
        if !p.is_empty() && unsafe { crate::WriteFile(h, p.as_ptr(), p.len() as u32, &mut done, 0) } == 0 {
            ok = 0;
        }
    }
    crate::CloseHandle(h);
    ok
}

/// `key` in `sec` to `val` (None deletes it; no key deletes the section).
unsafe fn write_entry(sec: &[u8], key: Option<&[u8]>, val: Option<&[u8]>, path: *const u16) -> Bool {
    let file = read_file(path);
    let d = file.as_ref().map_or(&[][..], |b| b.as_slice());
    // Where the section starts (past its header) and ends.
    let mut sec_head = None;
    let mut body_start = 0;
    let mut sec_end = d.len();
    let mut found_line: Option<(usize, usize)> = None;
    for (at, line) in (Lines { d, at: 0 }) {
        let next = at + line.len() + 1;
        if let Some(s) = section_of(line) {
            if sec_head.is_some() {
                sec_end = at;
                break;
            }
            if same(s, sec) {
                sec_head = Some(at);
                body_start = next.min(d.len());
            }
            continue;
        }
        if sec_head.is_some() {
            if let (Some(key), Some((k, _))) = (key, entry_of(line)) {
                if same(k, key) && found_line.is_none() {
                    found_line = Some((at, next.min(d.len())));
                }
            }
        }
    }
    let Some(head) = sec_head else {
        // A new section, if there's something to put in it.
        let (Some(key), Some(val)) = (key, val) else { return 1 };
        let lead: &[u8] = if d.is_empty() || d.ends_with(b"\n") { b"" } else { b"\r\n" };
        return write_pieces(path, &[d, lead, b"[", sec, b"]\r\n", key, b"=", val, b"\r\n"]);
    };
    let Some(key) = key else {
        return write_pieces(path, &[&d[..head], &d[sec_end..]]);
    };
    match (found_line, val) {
        (Some((a, b)), Some(v)) => write_pieces(path, &[&d[..a], key, b"=", v, b"\r\n", &d[b..]]),
        (Some((a, b)), None) => write_pieces(path, &[&d[..a], &d[b..]]),
        (None, Some(v)) => {
            // Right after the section's last entry (its header if none).
            let mut at = body_start;
            for (start, line) in (Lines { d: &d[..sec_end], at: body_start }) {
                if entry_of(line).is_some() {
                    at = (start + line.len() + 1).min(sec_end);
                }
            }
            let lead: &[u8] = if at == 0 || d[at - 1] == b'\n' { b"" } else { b"\r\n" };
            write_pieces(path, &[&d[..at], lead, key, b"=", v, b"\r\n", &d[at..]])
        }
        (None, None) => 1,
    }
}

#[no_mangle]
pub unsafe extern "system" fn WritePrivateProfileStringA(sec: *const u8, key: *const u8, val: *const u8, file: *const u8) -> Bool {
    let Some(s) = (unsafe { cstr(sec) }) else {
        crate::SetLastError(ERROR_INVALID_PARAMETER);
        return 0;
    };
    let w = unsafe { widen(file) };
    let mut path = [0u16; 520];
    ini_path(w.as_ptr(), &mut path);
    unsafe { write_entry(s, cstr(key), cstr(val), path.as_ptr()) }
}

#[no_mangle]
pub unsafe extern "system" fn WritePrivateProfileStringW(sec: *const u16, key: *const u16, val: *const u16, file: *const u16) -> Bool {
    let Some(s) = Narrow::new(sec) else {
        crate::SetLastError(ERROR_INVALID_PARAMETER);
        return 0;
    };
    let (k, v) = (Narrow::new(key), Narrow::new(val));
    let mut path = [0u16; 520];
    ini_path(file, &mut path);
    unsafe { write_entry(s.bytes(), narrow_opt(&k), narrow_opt(&v), path.as_ptr()) }
}

/// A whole section from a double-NUL list of key=value lines.
#[no_mangle]
pub unsafe extern "system" fn WritePrivateProfileSectionA(sec: *const u8, body: *const u8, file: *const u8) -> Bool {
    let Some(s) = (unsafe { cstr(sec) }) else { return 0 };
    let w = unsafe { widen(file) };
    let mut path = [0u16; 520];
    ini_path(w.as_ptr(), &mut path);
    unsafe { write_entry(s, None, None, path.as_ptr()) };
    if body.is_null() {
        return 1;
    }
    let mut p = body;
    unsafe {
        while *p != 0 {
            let line = cstr(p).unwrap_or(&[]);
            if let Some((k, v)) = entry_of(line) {
                write_entry(s, Some(k), Some(v), path.as_ptr());
            }
            p = p.add(line.len() + 1);
        }
    }
    1
}

// win.ini
fn win_ini() -> WBuf {
    unsafe { widen(b"win.ini\0".as_ptr()) }
}

#[no_mangle]
pub unsafe extern "system" fn GetProfileStringW(sec: *const u16, key: *const u16, def: *const u16, out: *mut u16, cap: u32) -> u32 {
    let w = win_ini();
    unsafe { GetPrivateProfileStringW(sec, key, def, out, cap, w.as_ptr()) }
}

#[no_mangle]
pub unsafe extern "system" fn GetProfileIntW(sec: *const u16, key: *const u16, def: i32) -> u32 {
    let w = win_ini();
    unsafe { GetPrivateProfileIntW(sec, key, def, w.as_ptr()) }
}

#[no_mangle]
pub unsafe extern "system" fn WriteProfileStringW(sec: *const u16, key: *const u16, val: *const u16) -> Bool {
    let w = win_ini();
    unsafe { WritePrivateProfileStringW(sec, key, val, w.as_ptr()) }
}

// -----| version resources |-----

/// A PE file's RT_VERSION resource, found through its sections.
fn version_block(d: &[u8]) -> Option<&[u8]> {
    let u16_at = |o: usize| d.get(o..o + 2).map(|b| u16::from_le_bytes([b[0], b[1]]));
    let u32_at = |o: usize| d.get(o..o + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
    if d.get(..2)? != b"MZ" {
        return None;
    }
    let pe = u32_at(0x3C)? as usize;
    if d.get(pe..pe + 4)? != b"PE\0\0" {
        return None;
    }
    let sections = u16_at(pe + 6)? as usize;
    let opt = pe + 24;
    let opt_size = u16_at(pe + 20)? as usize;
    let magic = u16_at(opt)?;
    let dirs = opt + if magic == 0x20B { 112 } else { 96 };
    let rsrc_rva = u32_at(dirs + 2 * 8)? as usize;
    if rsrc_rva == 0 {
        return None;
    }
    let table = opt + opt_size;
    let to_off = |rva: usize| -> Option<usize> {
        for i in 0..sections {
            let s = table + i * 40;
            let (vs, va, raw_n, raw) = (u32_at(s + 8)? as usize, u32_at(s + 12)? as usize, u32_at(s + 16)? as usize, u32_at(s + 20)? as usize);
            if rva >= va && rva < va + vs.max(raw_n) {
                return Some(raw + rva - va);
            }
        }
        None
    };
    let root = to_off(rsrc_rva)?;
    // A directory's entry for `id`, or its first.
    let entry = |dir: usize, id: Option<u32>| -> Option<u32> {
        let named = u16_at(dir + 12)? as usize;
        let ids = u16_at(dir + 14)? as usize;
        for i in 0..named + ids {
            let e = dir + 16 + i * 8;
            let n = u32_at(e)?;
            if id.is_none() || (n & 0x8000_0000 == 0 && Some(n) == id) {
                return u32_at(e + 4);
            }
        }
        None
    };
    let types = entry(root, Some(16))?;
    let names = entry(root + (types & 0x7FFF_FFFF) as usize, None)?;
    let langs = entry(root + (names & 0x7FFF_FFFF) as usize, None)?;
    if langs & 0x8000_0000 != 0 {
        return None;
    }
    let data = root + langs as usize;
    let (rva, size) = (u32_at(data)? as usize, u32_at(data + 4)? as usize);
    let off = to_off(rva)?;
    d.get(off..off + size)
}

#[no_mangle]
pub unsafe extern "system" fn GetFileVersionInfoSizeW(name: *const u16, handle: *mut u32) -> u32 {
    if !handle.is_null() {
        unsafe { *handle = 0 };
    }
    let Some(f) = read_file(name) else {
        crate::SetLastError(ERROR_FILE_NOT_FOUND);
        return 0;
    };
    match version_block(f.as_slice()) {
        // Room as Windows gives it, for the conversions it makes in place.
        Some(b) => (b.len() * 2 + 4) as u32,
        None => {
            crate::SetLastError(ERROR_RESOURCE_DATA_NOT_FOUND);
            0
        }
    }
}

#[no_mangle]
pub unsafe extern "system" fn GetFileVersionInfoSizeA(name: *const u8, handle: *mut u32) -> u32 {
    let w = unsafe { widen(name) };
    unsafe { GetFileVersionInfoSizeW(w.as_ptr(), handle) }
}

#[no_mangle]
pub unsafe extern "system" fn GetFileVersionInfoW(name: *const u16, _h: u32, cap: u32, out: *mut u8) -> Bool {
    if out.is_null() {
        crate::SetLastError(ERROR_INVALID_PARAMETER);
        return 0;
    }
    let Some(f) = read_file(name) else {
        crate::SetLastError(ERROR_FILE_NOT_FOUND);
        return 0;
    };
    let Some(b) = version_block(f.as_slice()) else {
        crate::SetLastError(ERROR_RESOURCE_DATA_NOT_FOUND);
        return 0;
    };
    let n = b.len().min(cap as usize);
    if n < b.len() {
        crate::SetLastError(ERROR_INSUFFICIENT_BUFFER);
    }
    unsafe { core::ptr::copy_nonoverlapping(b.as_ptr(), out, n) };
    1
}

#[no_mangle]
pub unsafe extern "system" fn GetFileVersionInfoA(name: *const u8, h: u32, cap: u32, out: *mut u8) -> Bool {
    let w = unsafe { widen(name) };
    unsafe { GetFileVersionInfoW(w.as_ptr(), h, cap, out) }
}

/// One node of the version tree: wLength, wValueLength, wType, key, padding,
/// value, padding, children. (key start, value start, value length, its end.)
fn node(block: *const u8, at: usize) -> (usize, usize, usize, usize) {
    unsafe {
        let len = core::ptr::read_unaligned(block.add(at) as *const u16) as usize;
        let val = core::ptr::read_unaligned(block.add(at + 2) as *const u16) as usize;
        let ty = core::ptr::read_unaligned(block.add(at + 4) as *const u16);
        let key = at + 6;
        let mut k = key;
        while core::ptr::read_unaligned(block.add(k) as *const u16) != 0 {
            k += 2;
        }
        let value = (k + 2 + 3) & !3;
        // wValueLength is characters for text, bytes for binary.
        let bytes = if ty == 1 { val * 2 } else { val };
        (key, value, bytes, at + len)
    }
}

unsafe fn query(block: *const u8, sub: &[u16], out: *mut usize, len: *mut u32) -> Bool {
    let mut pieces = sub.split(|&c| c == b'\\' as u16).filter(|p| !p.is_empty());
    let mut at = 0usize;
    let mut cur = node(block, 0);
    'walk: for piece in &mut pieces {
        // Its children start after its value.
        let mut child = (cur.1 + cur.2 + 3) & !3;
        while child < cur.3 {
            let n = node(block, child);
            if n.3 <= child {
                break;
            }
            let name = unsafe { wstr(block.add(n.0) as *const u16) };
            let eq = name.len() == piece.len() && name.iter().zip(piece).all(|(a, b)| lower(*a as u8) == lower(*b as u8));
            if eq {
                at = child;
                cur = n;
                continue 'walk;
            }
            child = (n.3 + 3) & !3;
        }
        return 0;
    }
    let _ = at;
    if cur.2 == 0 {
        return 0;
    }
    let ty = unsafe { core::ptr::read_unaligned(block.add(at + 4) as *const u16) };
    unsafe {
        if !out.is_null() {
            *out = block.add(cur.1) as usize;
        }
        if !len.is_null() {
            *len = if ty == 1 { (cur.2 / 2) as u32 } else { cur.2 as u32 };
        }
    }
    1
}

#[no_mangle]
pub unsafe extern "system" fn VerQueryValueW(block: *const u8, sub: *const u16, out: *mut usize, len: *mut u32) -> Bool {
    if block.is_null() {
        return 0;
    }
    unsafe { query(block, wstr(sub), out, len) }
}

/// The value as a narrow string, converted into the spare half of the block
/// (GetFileVersionInfoSize left room).
#[no_mangle]
pub unsafe extern "system" fn VerQueryValueA(block: *const u8, sub: *const u8, out: *mut usize, len: *mut u32) -> Bool {
    if block.is_null() {
        return 0;
    }
    let w = unsafe { widen(sub) };
    let (mut p, mut n) = (0usize, 0u32);
    if unsafe { query(block, wstr(w.as_ptr()), &mut p, &mut n) } == 0 {
        return 0;
    }
    let is_string = unsafe { wstr(w.as_ptr()) }.iter().take(16).map(|&c| lower(c as u8)).eq(b"\\stringfileinfo\\".iter().copied());
    if is_string {
        let total = unsafe { core::ptr::read_unaligned(block as *const u16) } as usize;
        // Where the wide string sits in the block picks its narrow slot.
        let spare = unsafe { block.add(total + (p - block as usize)) as *mut u8 };
        let k = unsafe { crate::locale::WideCharToMultiByte(0, 0, p as *const u16, n as i32, spare, n as i32 * 2, core::ptr::null(), core::ptr::null_mut()) }.max(0);
        p = spare as usize;
        n = k as u32;
    }
    unsafe {
        if !out.is_null() {
            *out = p;
        }
        if !len.is_null() {
            *len = n;
        }
    }
    1
}

// -----| authz: everyone may do everything (no ACLs yet) |-----

#[no_mangle]
pub unsafe extern "system" fn AuthzInitializeResourceManager(_flags: u32, _a: usize, _b: usize, _c: usize, _name: *const u16, out: *mut u64) -> Bool {
    if !out.is_null() {
        unsafe { *out = 1 };
    }
    1
}

#[no_mangle]
pub extern "system" fn AuthzFreeResourceManager(_h: u64) -> Bool {
    1
}

#[no_mangle]
pub unsafe extern "system" fn AuthzInitializeContextFromSid(_flags: u32, _sid: *const u8, _rm: u64, _exp: *const i64, _id: u64, _args: usize, out: *mut u64) -> Bool {
    if !out.is_null() {
        unsafe { *out = 2 };
    }
    1
}

#[no_mangle]
pub extern "system" fn AuthzFreeContext(_h: u64) -> Bool {
    1
}

/// AUTHZ_ACCESS_REPLY: result count, granted masks*, audit*, errors*.
#[no_mangle]
pub unsafe extern "system" fn AuthzAccessCheck(_flags: u32, _ctx: u64, _req: *const u8, _audit: u64, _sd: *const u8, _opt: usize, _n: u32, reply: *mut u8, _res: usize) -> Bool {
    if reply.is_null() {
        return 1;
    }
    unsafe {
        let count = core::ptr::read_unaligned(reply as *const u32) as usize;
        let granted = core::ptr::read_unaligned(reply.add(8) as *const *mut u32);
        let errors = core::ptr::read_unaligned(reply.add(24) as *const *mut u32);
        for i in 0..count {
            if !granted.is_null() {
                *granted.add(i) = 0x1F_FFFF;
            }
            if !errors.is_null() {
                *errors.add(i) = 0;
            }
        }
    }
    1
}

// -----| ShellMessageBox, in the API set kernel32 hosts |-----

/// `ShellMessageBoxW(inst, hwnd, text, title, type, args...)`: text and title
/// may be string ids; %1 and so on fill from the arguments. user32 is looked
/// up when it's needed (kernel32 doesn't link to it).
#[no_mangle]
pub unsafe extern "system" fn ShellMessageBoxW(inst: u64, hwnd: u64, text: *const u16, title: *const u16, kind: u32, a1: usize, a2: usize, a3: usize, a4: usize) -> i32 {
    const FROM_STRING: u32 = 0x400;
    const ARGUMENT_ARRAY: u32 = 0x2000;
    let load = |p: *const u16, buf: &mut [u16; 1024]| -> *const u16 {
        if (p as usize) >> 16 != 0 || p.is_null() {
            return p;
        }
        let n = unsafe { crate::resource::LoadStringW(inst, p as usize as u32, buf.as_mut_ptr(), 1024) };
        if n > 0 { buf.as_ptr() } else { core::ptr::null() }
    };
    let (mut tb, mut cb, mut out) = ([0u16; 1024], [0u16; 1024], [0u16; 2048]);
    let t = load(text, &mut tb);
    let c = load(title, &mut cb);
    let args = [a1, a2, a3, a4];
    let n = unsafe { crate::resource::FormatMessageW(FROM_STRING | ARGUMENT_ARRAY, t as u64, 0, 0, out.as_mut_ptr(), 2048, args.as_ptr() as u64) };
    let shown = if n > 0 { out.as_ptr() } else { t };
    let user32 = unsafe { crate::module::LoadLibraryW(widen(b"user32.dll\0".as_ptr()).as_ptr()) };
    let f = unsafe { crate::module::GetProcAddress(user32, b"MessageBoxW\0".as_ptr()) };
    if f == 0 {
        return 0;
    }
    let mb: unsafe extern "system" fn(u64, *const u16, *const u16, u32) -> i32 = unsafe { core::mem::transmute(f as usize) };
    unsafe { mb(hwnd, shown, c, kind) }
}

/// No files are ever dropped yet.
#[no_mangle]
pub extern "system" fn DragQueryFileW(_drop: u64, _i: u32, _out: *mut u16, _cap: u32) -> u32 {
    0
}
