//! Resources, strings and messages (W7): FindResource and friends,
//! LoadString, FormatMessage.
//!
//! A module's resources are read from its mapped image. Windows keeps most
//! programs' text in a satellite "MUI" file (`<folder>\en-US\<name>.mui`, a
//! resource-only PE); when the image itself lacks a resource, it's looked
//! up there, the file being read into memory once per module.
//! FORMAT_MESSAGE_FROM_SYSTEM uses a table of the common error messages.

use crate::util::{wstr, WBuf};
use crate::{Bool, Handle};
use core::ptr::read_unaligned;

const RT_STRING: u16 = 6;
const RT_MESSAGETABLE: u16 = 11;
const ERROR_RESOURCE_TYPE_NOT_FOUND: u32 = 1813;
const ERROR_MR_MID_NOT_FOUND: u32 = 317;

/// A PE image's bytes: mapped (RVA = offset from base) or a raw file.
#[derive(Clone, Copy)]
struct Image {
    base: u64,
    mapped: bool,
    size: usize,
}

impl Image {
    fn u16(&self, off: u64) -> u16 {
        unsafe { read_unaligned((self.base + off) as *const u16) }
    }
    fn u32(&self, off: u64) -> u32 {
        unsafe { read_unaligned((self.base + off) as *const u32) }
    }
    /// The file offset (or mapped offset) of an RVA.
    fn rva(&self, rva: u32) -> Option<u64> {
        if self.mapped {
            return Some(rva as u64);
        }
        let nt = self.u32(0x3C) as u64;
        let sections = self.u16(nt + 6) as u64;
        let opt_size = self.u16(nt + 20) as u64;
        let first = nt + 24 + opt_size;
        for i in 0..sections {
            let s = first + 40 * i;
            let (va, vsize, raw) = (self.u32(s + 12), self.u32(s + 8).max(self.u32(s + 16)), self.u32(s + 20));
            if rva >= va && rva < va + vsize {
                let off = (rva - va + raw) as u64;
                return (off < self.size as u64).then_some(off);
            }
        }
        None
    }
    /// The resource directory's offset.
    fn resources(&self) -> Option<u64> {
        if self.size < 0x40 || self.u16(0) != 0x5A4D {
            return None;
        }
        let nt = self.u32(0x3C) as u64;
        let opt = nt + 24;
        // PE32+ or PE32 (MUI files are usually PE32 whatever the program).
        let (count, dirs) = match self.u16(opt) {
            0x20B => (opt + 0x6C, opt + 0x70),
            0x10B => (opt + 0x5C, opt + 0x60),
            _ => return None,
        };
        if self.u32(count) <= 2 {
            return None;
        }
        let rva = self.u32(dirs + 2 * 8);
        if rva == 0 {
            return None;
        }
        self.rva(rva)
    }
    /// The entry for `id` (or the first, for None) in the directory at `dir`:
    /// (offset of its data or subdirectory, is a subdirectory).
    fn entry(&self, root: u64, dir: u64, id: Option<Name>) -> Option<(u64, bool)> {
        let named = self.u16(dir + 12) as u64;
        let ids = self.u16(dir + 14) as u64;
        for k in 0..named + ids {
            let e = dir + 16 + 8 * k;
            let name = self.u32(e);
            let data = self.u32(e + 4);
            let hit = match id {
                None => true,
                Some(Name::Id(n)) => name & 0x8000_0000 == 0 && name as u16 == n,
                Some(Name::Str(s)) => {
                    name & 0x8000_0000 != 0 && {
                        let at = root + (name & 0x7FFF_FFFF) as u64;
                        let len = self.u16(at) as usize;
                        len == s.len() && (0..len).all(|i| crate::util::eq_ignore_case(&[self.u16(at + 2 + 2 * i as u64)], &[s[i]]))
                    }
                }
            };
            if hit {
                return Some((root + (data & 0x7FFF_FFFF) as u64, data & 0x8000_0000 != 0));
            }
        }
        None
    }
    /// The data (offset, size) of resource type/name, any language.
    fn find(&self, kind: Name, name: Name) -> Option<(u64, u32)> {
        let root = self.resources()?;
        let (types, sub) = self.entry(root, root, Some(kind))?;
        if !sub {
            return None;
        }
        let (names, sub) = self.entry(root, types, Some(name))?;
        if !sub {
            return None;
        }
        // en-US, neutral, else the first language.
        let (leaf, sub) = self
            .entry(root, names, Some(Name::Id(0x409)))
            .or_else(|| self.entry(root, names, Some(Name::Id(0))))
            .or_else(|| self.entry(root, names, None))?;
        if sub {
            return None;
        }
        let off = self.rva(self.u32(leaf))?;
        Some((off, self.u32(leaf + 4)))
    }
}

#[derive(Clone, Copy)]
enum Name<'a> {
    Id(u16),
    Str(&'a [u16]),
}

/// MAKEINTRESOURCE or a string.
unsafe fn name_of<'a>(p: *const u16) -> Name<'a> {
    if (p as usize) >> 16 == 0 { Name::Id(p as usize as u16) } else { Name::Str(unsafe { wstr(p) }) }
}

fn module_image(module: Handle) -> Image {
    let base = if module == 0 { unsafe { crate::module::GetModuleHandleW(core::ptr::null()) } } else { module & !3 };
    let size = unsafe { read_unaligned((base + read_unaligned((base + 0x3C) as *const u32) as u64 + 24 + 0x38) as *const u32) } as usize;
    Image { base, mapped: true, size }
}

// -----| MUI files |-----

/// MUI files read so far: (module base, file in memory, its size).
static mut MUI: [(u64, u64, usize); 16] = [(0, 0, 0); 16];

/// The module's MUI file, read on first use.
fn mui_of(module: u64) -> Option<Image> {
    unsafe {
        for &(m, data, size) in (*(&raw const MUI)).iter() {
            if m == module {
                return (data != 0).then_some(Image { base: data, mapped: false, size });
            }
        }
    }
    // "<folder>\en-US\<file name>.mui"
    let mut path = [0u16; 520];
    let n = unsafe { crate::module::GetModuleFileNameW(module, path.as_mut_ptr(), 500) } as usize;
    let slash = path[..n].iter().rposition(|&c| c == b'\\' as u16)?;
    let mut full = WBuf::new();
    full.extend(&path[..=slash]);
    full.push_str("en-US\\");
    full.extend(&path[slash + 1..n]);
    full.push_str(".mui");
    let (data, size) = read_file(full.as_ptr()).unwrap_or((0, 0));
    unsafe {
        if let Some(slot) = (*(&raw mut MUI)).iter_mut().find(|s| s.0 == 0) {
            *slot = (module, data, size);
        }
    }
    (data != 0).then_some(Image { base: data, mapped: false, size })
}

/// A whole file in memory (process heap): (address, size).
fn read_file(path: *const u16) -> Option<(u64, usize)> {
    let h = unsafe { crate::file::CreateFileW(path, 0x8000_0000, 1, 0, 3, 0, 0) };
    if h == crate::INVALID_HANDLE_VALUE {
        return None;
    }
    let mut size = 0i64;
    unsafe { crate::file::GetFileSizeEx(h, &mut size) };
    let buf = crate::util::alloc(size.max(1) as usize);
    let mut done = 0u32;
    let ok = unsafe { crate::ReadFile(h, buf, size as u32, &mut done, 0) };
    crate::CloseHandle(h);
    if ok == 0 || done as i64 != size {
        crate::util::free(buf);
        return None;
    }
    Some((buf as u64, size as usize))
}

/// Find a resource in `module`, else in its MUI file: (address, size).
fn locate(module: Handle, kind: Name, name: Name) -> Option<(u64, u32)> {
    let img = module_image(module);
    if let Some((off, size)) = img.find(kind, name) {
        return Some((img.base + off, size));
    }
    let mui = mui_of(img.base)?;
    let (off, size) = mui.find(kind, name)?;
    Some((mui.base + off, size))
}

// -----| FindResource and friends |-----
//
// HRSRC is the address of a (data address, size) pair kept here.

static mut FOUND: [(u64, u32); 64] = [(0, 0); 64];
static mut FOUND_NEXT: usize = 0;

fn hrsrc(found: (u64, u32)) -> Handle {
    unsafe {
        let i = FOUND_NEXT % 64;
        FOUND_NEXT += 1;
        let slot = &mut (*(&raw mut FOUND))[i];
        *slot = found;
        slot as *mut (u64, u32) as Handle
    }
}

#[no_mangle]
pub unsafe extern "system" fn FindResourceW(module: Handle, name: *const u16, kind: *const u16) -> Handle {
    match locate(module, unsafe { name_of(kind) }, unsafe { name_of(name) }) {
        Some(f) => hrsrc(f),
        None => {
            crate::SetLastError(ERROR_RESOURCE_TYPE_NOT_FOUND);
            0
        }
    }
}

#[no_mangle]
pub unsafe extern "system" fn FindResourceExW(module: Handle, kind: *const u16, name: *const u16, _lang: u16) -> Handle {
    unsafe { FindResourceW(module, name, kind) }
}

#[no_mangle]
pub unsafe extern "system" fn FindResourceA(module: Handle, name: *const u8, kind: *const u8) -> Handle {
    let widen = |p: *const u8| if (p as usize) >> 16 == 0 { None } else { Some(unsafe { crate::util::widen(p) }) };
    let (n, k) = (widen(name), widen(kind));
    let np = n.as_ref().map_or(name as *const u16, |w| w.as_ptr());
    let kp = k.as_ref().map_or(kind as *const u16, |w| w.as_ptr());
    unsafe { FindResourceW(module, np, kp) }
}

#[no_mangle]
pub unsafe extern "system" fn LoadResource(_module: Handle, res: Handle) -> Handle {
    if res == 0 { 0 } else { unsafe { (*(res as *const (u64, u32))).0 } }
}

#[no_mangle]
pub extern "system" fn LockResource(data: Handle) -> *mut u8 {
    data as *mut u8
}

#[no_mangle]
pub unsafe extern "system" fn SizeofResource(_module: Handle, res: Handle) -> u32 {
    if res == 0 { 0 } else { unsafe { (*(res as *const (u64, u32))).1 } }
}

#[no_mangle]
pub extern "system" fn FreeResource(_data: Handle) -> Bool {
    0
}

/// String table entry `id`: (pointer to its UTF-16 text, length).
fn string_resource(module: Handle, id: u32) -> Option<(*const u16, usize)> {
    let (block, size) = locate(module, Name::Id(RT_STRING), Name::Id((id / 16 + 1) as u16))?;
    let mut p = block;
    let end = block + size as u64;
    for k in 0..16 {
        if p + 2 > end {
            return None;
        }
        let len = unsafe { read_unaligned(p as *const u16) } as usize;
        if k == id % 16 {
            return Some(((p + 2) as *const u16, len));
        }
        p += 2 + 2 * len as u64;
    }
    None
}

#[no_mangle]
pub unsafe extern "system" fn LoadStringW(module: Handle, id: u32, out: *mut u16, cap: i32) -> i32 {
    let Some((text, len)) = string_resource(module, id) else {
        crate::SetLastError(ERROR_RESOURCE_TYPE_NOT_FOUND);
        if cap > 0 && !out.is_null() {
            unsafe { *out = 0 };
        }
        return 0;
    };
    if cap == 0 {
        // A pointer to the read-only text itself.
        unsafe { *(out as *mut *const u16) = text };
        return len as i32;
    }
    let n = len.min(cap as usize - 1);
    unsafe {
        core::ptr::copy_nonoverlapping(text, out, n);
        *out.add(n) = 0;
    }
    n as i32
}

#[no_mangle]
pub unsafe extern "system" fn LoadStringA(module: Handle, id: u32, out: *mut u8, cap: i32) -> i32 {
    let Some((text, len)) = string_resource(module, id) else { return 0 };
    if cap <= 0 || out.is_null() {
        return 0;
    }
    let n = len.min(cap as usize - 1);
    for i in 0..n {
        let u = unsafe { *text.add(i) };
        unsafe { *out.add(i) = if u < 0x100 { u as u8 } else { b'?' } };
    }
    unsafe { *out.add(n) = 0 };
    n as i32
}

// -----| FormatMessage |-----

const FORMAT_MESSAGE_ALLOCATE_BUFFER: u32 = 0x100;
const FORMAT_MESSAGE_IGNORE_INSERTS: u32 = 0x200;
const FORMAT_MESSAGE_FROM_STRING: u32 = 0x400;
const FORMAT_MESSAGE_FROM_HMODULE: u32 = 0x800;
const FORMAT_MESSAGE_FROM_SYSTEM: u32 = 0x1000;
const FORMAT_MESSAGE_ARGUMENT_ARRAY: u32 = 0x2000;
const FORMAT_MESSAGE_MAX_WIDTH_MASK: u32 = 0xFF;

/// Message `id` from a module's RT_MESSAGETABLE: (text, units, is wide).
fn message_from_module(module: Handle, id: u32) -> Option<(u64, usize, bool)> {
    let (table, _) = locate(module, Name::Id(RT_MESSAGETABLE), Name::Id(1))?;
    let blocks = unsafe { read_unaligned(table as *const u32) } as u64;
    for b in 0..blocks {
        let e = table + 4 + 12 * b;
        let (low, high, offset) = unsafe { (read_unaligned(e as *const u32), read_unaligned((e + 4) as *const u32), read_unaligned((e + 8) as *const u32)) };
        if id < low || id > high {
            continue;
        }
        let mut p = table + offset as u64;
        for _ in low..id {
            p += unsafe { read_unaligned(p as *const u16) } as u64;
        }
        let length = unsafe { read_unaligned(p as *const u16) } as usize;
        let wide = unsafe { read_unaligned((p + 2) as *const u16) } & 1 != 0;
        let body = length.saturating_sub(4);
        let units = if wide { body / 2 } else { body };
        // Trim the padding NULs.
        let mut n = units;
        while n > 0 && unsafe { if wide { read_unaligned((p + 4 + 2 * (n as u64 - 1)) as *const u16) == 0 } else { *((p + 4 + n as u64 - 1) as *const u8) == 0 } } {
            n -= 1;
        }
        return Some((p + 4, n, wide));
    }
    None
}

/// The common system error messages (FORMAT_MESSAGE_FROM_SYSTEM).
fn system_message(id: u32) -> Option<&'static str> {
    Some(match id {
        0 => "The operation completed successfully.\r\n",
        1 => "Incorrect function.\r\n",
        2 => "The system cannot find the file specified.\r\n",
        3 => "The system cannot find the path specified.\r\n",
        4 => "The system cannot open the file.\r\n",
        5 => "Access is denied.\r\n",
        6 => "The handle is invalid.\r\n",
        8 => "Not enough memory resources are available to process this command.\r\n",
        13 => "The data is invalid.\r\n",
        15 => "The system cannot find the drive specified.\r\n",
        16 => "The directory cannot be removed.\r\n",
        17 => "The system cannot move the file to a different disk drive.\r\n",
        18 => "There are no more files.\r\n",
        19 => "The media is write protected.\r\n",
        21 => "The device is not ready.\r\n",
        32 => "The process cannot access the file because it is being used by another process.\r\n",
        38 => "Reached the end of the file.\r\n",
        50 => "The request is not supported.\r\n",
        80 => "The file exists.\r\n",
        87 => "The parameter is incorrect.\r\n",
        109 => "The pipe has been ended.\r\n",
        112 => "There is not enough space on the disk.\r\n",
        120 => "This function is not supported on this system.\r\n",
        122 => "The data area passed to a system call is too small.\r\n",
        123 => "The filename, directory name, or volume label syntax is incorrect.\r\n",
        126 => "The specified module could not be found.\r\n",
        127 => "The specified procedure could not be found.\r\n",
        145 => "The directory is not empty.\r\n",
        183 => "Cannot create a file when that file already exists.\r\n",
        193 => "%1 is not a valid Win32 application.\r\n",
        206 => "The filename or extension is too long.\r\n",
        258 => "The wait operation timed out.\r\n",
        267 => "The directory name is invalid.\r\n",
        998 => "Invalid access to memory location.\r\n",
        1155 => "No application is associated with the specified file for this operation.\r\n",
        _ => return None,
    })
}

/// FormatMessage's output, built in UTF-16.
struct Out {
    buf: alloc_buf::Buf,
}

mod alloc_buf {
    /// A growable UTF-16 buffer on the process heap.
    pub struct Buf {
        pub p: *mut u16,
        pub len: usize,
        pub cap: usize,
    }
    impl Buf {
        pub fn new() -> Self {
            Buf { p: core::ptr::null_mut(), len: 0, cap: 0 }
        }
        pub fn push(&mut self, u: u16) {
            if self.len + 1 >= self.cap {
                let cap = (self.cap * 2).max(256);
                let p = crate::util::alloc(cap * 2) as *mut u16;
                if !self.p.is_null() {
                    unsafe { core::ptr::copy_nonoverlapping(self.p, p, self.len) };
                    crate::util::free(self.p as *mut u8);
                }
                self.p = p;
                self.cap = cap;
            }
            unsafe { *self.p.add(self.len) = u };
            self.len += 1;
        }
        pub fn slice(&self) -> &[u16] {
            if self.p.is_null() { &[] } else { unsafe { core::slice::from_raw_parts(self.p, self.len) } }
        }
    }
    impl Drop for Buf {
        fn drop(&mut self) {
            if !self.p.is_null() {
                crate::util::free(self.p as *mut u8);
            }
        }
    }
}

impl Out {
    fn push(&mut self, u: u16) {
        self.buf.push(u);
    }
    fn ascii(&mut self, s: &[u8]) {
        for &b in s {
            self.push(b as u16);
        }
    }
}

/// One `%n!fmt!` insert: printf-style formatting of argument `v`.
fn insert(out: &mut Out, fmt: &[u16], v: u64, wide_default: bool) {
    // [flags][width][.precision][size]type
    let mut i = 0;
    let (mut left, mut zero, mut plus, mut space, mut alt) = (false, false, false, false, false);
    while i < fmt.len() {
        match fmt[i] as u8 {
            b'-' => left = true,
            b'0' => zero = true,
            b'+' => plus = true,
            b' ' => space = true,
            b'#' => alt = true,
            _ => break,
        }
        i += 1;
    }
    let mut width = 0usize;
    while i < fmt.len() && (fmt[i] as u8).is_ascii_digit() {
        width = width * 10 + (fmt[i] - b'0' as u16) as usize;
        i += 1;
    }
    let mut precision: Option<usize> = None;
    if i < fmt.len() && fmt[i] == b'.' as u16 {
        i += 1;
        let mut p = 0;
        while i < fmt.len() && (fmt[i] as u8).is_ascii_digit() {
            p = p * 10 + (fmt[i] - b'0' as u16) as usize;
            i += 1;
        }
        precision = Some(p);
    }
    let mut wide = wide_default;
    let mut bits64 = false;
    while i < fmt.len() {
        match fmt[i] as u8 {
            b'l' | b'w' => wide = true,
            b'h' => wide = false,
            b'I' => {
                bits64 = true;
                if i + 2 < fmt.len() && fmt[i + 1] == b'6' as u16 {
                    i += 2;
                } else if i + 2 < fmt.len() && fmt[i + 1] == b'3' as u16 {
                    bits64 = false;
                    i += 2;
                }
            }
            _ => break,
        }
        i += 1;
    }
    let conv = if i < fmt.len() { fmt[i] as u8 } else { b's' };
    let mut text = [0u16; 512];
    let mut n = 0;
    let mut sign: &[u8] = b"";
    let mut put = |u: u16, n: &mut usize| {
        if *n < text.len() {
            text[*n] = u;
            *n += 1;
        }
    };
    match conv {
        b's' | b'S' => {
            let w = if conv == b'S' { !wide } else { wide };
            let max = precision.unwrap_or(usize::MAX);
            if v == 0 {
                for &b in b"(null)" {
                    put(b as u16, &mut n);
                }
            } else if w {
                let s = unsafe { wstr(v as *const u16) };
                for &u in s.iter().take(max) {
                    put(u, &mut n);
                }
            } else {
                let mut k = 0;
                while k < max {
                    let b = unsafe { *((v + k as u64) as *const u8) };
                    if b == 0 {
                        break;
                    }
                    put(b as u16, &mut n);
                    k += 1;
                }
            }
        }
        b'c' | b'C' => put(v as u16, &mut n),
        b'd' | b'i' | b'u' | b'x' | b'X' | b'o' => {
            let signed = matches!(conv, b'd' | b'i');
            let raw = if bits64 { v } else { v as u32 as u64 };
            let (mag, neg) = if signed {
                let s = if bits64 { v as i64 } else { v as u32 as i32 as i64 };
                (s.unsigned_abs(), s < 0)
            } else {
                (raw, false)
            };
            sign = if neg { b"-" } else if plus && signed { b"+" } else if space && signed { b" " } else { b"" };
            let base = match conv {
                b'x' | b'X' => 16,
                b'o' => 8,
                _ => 10,
            };
            let mut digits = [0u8; 24];
            let mut k = 0;
            let mut m = mag;
            loop {
                let d = (m % base) as u8;
                digits[k] = if d < 10 { b'0' + d } else if conv == b'X' { b'A' + d - 10 } else { b'a' + d - 10 };
                k += 1;
                m /= base;
                if m == 0 {
                    break;
                }
            }
            if alt && base == 16 && mag != 0 {
                put(b'0' as u16, &mut n);
                put(if conv == b'X' { b'X' } else { b'x' } as u16, &mut n);
            }
            for _ in k..precision.unwrap_or(1) {
                put(b'0' as u16, &mut n);
            }
            for &d in digits[..k].iter().rev() {
                put(d as u16, &mut n);
            }
        }
        _ => {}
    }
    let len = sign.len() + n;
    let pad = width.saturating_sub(len);
    if !left && !(zero && conv != b's') {
        for _ in 0..pad {
            out.push(b' ' as u16);
        }
    }
    out.ascii(sign);
    if !left && zero && conv != b's' {
        for _ in 0..pad {
            out.push(b'0' as u16);
        }
    }
    for &u in &text[..n] {
        out.push(u);
    }
    if left {
        for _ in 0..pad {
            out.push(b' ' as u16);
        }
    }
}

/// Expand `source` (UTF-16) with its inserts into `out`.
fn expand(out: &mut Out, source: &[u16], flags: u32, args: u64, wide: bool) {
    let ignore = flags & FORMAT_MESSAGE_IGNORE_INSERTS != 0;
    let width = flags & FORMAT_MESSAGE_MAX_WIDTH_MASK;
    let arg = |n: usize| -> u64 { if args == 0 { 0 } else { unsafe { read_unaligned((args as *const u64).add(n - 1)) } } };
    let mut i = 0;
    while i < source.len() {
        let c = source[i];
        // Line breaks in the text: kept, unless told to ignore them.
        if c == b'\r' as u16 || c == b'\n' as u16 {
            if width == FORMAT_MESSAGE_MAX_WIDTH_MASK {
                if c == b'\n' as u16 && i + 1 < source.len() {
                    out.push(b' ' as u16);
                }
            } else if c == b'\n' as u16 {
                out.push(b'\r' as u16);
                out.push(b'\n' as u16);
            }
            i += 1;
            continue;
        }
        if c != b'%' as u16 || i + 1 >= source.len() {
            out.push(c);
            i += 1;
            continue;
        }
        let d = source[i + 1];
        if ignore {
            out.push(c);
            i += 1;
            continue;
        }
        match d as u8 {
            b'0' => return,
            b'n' => {
                out.push(b'\r' as u16);
                out.push(b'\n' as u16);
                i += 2;
            }
            b'r' => {
                out.push(b'\r' as u16);
                i += 2;
            }
            b't' => {
                out.push(b'\t' as u16);
                i += 2;
            }
            b'b' => {
                out.push(b' ' as u16);
                i += 2;
            }
            b'1'..=b'9' => {
                let mut k = i + 1;
                let mut n = 0usize;
                while k < source.len() && (source[k] as u8).is_ascii_digit() && n < 100 {
                    n = n * 10 + (source[k] - b'0' as u16) as usize;
                    k += 1;
                }
                // Optional !printf format!.
                let mut fmt: &[u16] = &[b's' as u16];
                if k < source.len() && source[k] == b'!' as u16 {
                    if let Some(end) = source[k + 1..].iter().position(|&u| u == b'!' as u16) {
                        fmt = &source[k + 1..k + 1 + end];
                        k = k + 2 + end;
                    }
                }
                insert(out, fmt, arg(n), wide);
                i = k;
            }
            _ => {
                // "%%", "%.", "%!", "% ", ...: the character itself.
                out.push(d);
                i += 2;
            }
        }
    }
}

/// FormatMessage into an Out; None if the message wasn't found.
unsafe fn format(flags: u32, source: u64, id: u32, args: u64, wide_source: bool) -> Option<Out> {
    let mut out = Out { buf: alloc_buf::Buf::new() };
    // The text, as UTF-16.
    let mut text = alloc_buf::Buf::new();
    if flags & FORMAT_MESSAGE_FROM_STRING != 0 {
        if wide_source {
            for &u in unsafe { wstr(source as *const u16) } {
                text.push(u);
            }
        } else {
            for i in 0..unsafe { crate::util::alen(source as *const u8) } {
                text.push(unsafe { *((source + i as u64) as *const u8) } as u16);
            }
        }
    } else {
        let mut found = false;
        if flags & FORMAT_MESSAGE_FROM_HMODULE != 0 {
            if let Some((p, n, wide)) = message_from_module(source, id) {
                for k in 0..n {
                    text.push(if wide { unsafe { read_unaligned((p + 2 * k as u64) as *const u16) } } else { (unsafe { *((p + k as u64) as *const u8) }) as u16 });
                }
                found = true;
            }
        }
        if !found && flags & FORMAT_MESSAGE_FROM_SYSTEM != 0 {
            if let Some(s) = system_message(id) {
                for u in s.encode_utf16() {
                    text.push(u);
                }
                found = true;
            }
        }
        if !found {
            return None;
        }
    }
    // A va_list is a pointer to the 8-byte slots; ARGUMENT_ARRAY is the
    // array itself.
    let args = if args == 0 || flags & FORMAT_MESSAGE_ARGUMENT_ARRAY != 0 { args } else { unsafe { read_unaligned(args as *const u64) } };
    expand(&mut out, text.slice(), flags, args, wide_source);
    Some(out)
}

#[no_mangle]
pub unsafe extern "system" fn FormatMessageW(flags: u32, source: u64, id: u32, _lang: u32, buffer: *mut u16, size: u32, args: u64) -> u32 {
    let Some(out) = (unsafe { format(flags, source, id, args, true) }) else {
        crate::SetLastError(ERROR_MR_MID_NOT_FOUND);
        return 0;
    };
    let text = out.buf.slice();
    if flags & FORMAT_MESSAGE_ALLOCATE_BUFFER != 0 {
        let p = crate::mem::LocalAlloc(0, (text.len().max(size as usize) + 1) * 2) as *mut u16;
        unsafe {
            core::ptr::copy_nonoverlapping(text.as_ptr(), p, text.len());
            *p.add(text.len()) = 0;
            *(buffer as *mut *mut u16) = p;
        }
        return text.len() as u32;
    }
    if text.len() + 1 > size as usize {
        crate::SetLastError(122); // ERROR_INSUFFICIENT_BUFFER
        return 0;
    }
    unsafe {
        core::ptr::copy_nonoverlapping(text.as_ptr(), buffer, text.len());
        *buffer.add(text.len()) = 0;
    }
    text.len() as u32
}

#[no_mangle]
pub unsafe extern "system" fn FormatMessageA(flags: u32, source: u64, id: u32, _lang: u32, buffer: *mut u8, size: u32, args: u64) -> u32 {
    let Some(out) = (unsafe { format(flags, source, id, args, false) }) else {
        crate::SetLastError(ERROR_MR_MID_NOT_FOUND);
        return 0;
    };
    let text = out.buf.slice();
    let mut bytes = alloc_buf::Buf::new();
    let mut tmp = [0u8; 4];
    for &u in text {
        let n = crate::locale::encode_one(u, &mut tmp);
        for &b in &tmp[..n] {
            bytes.push(b as u16);
        }
    }
    let b = bytes.slice();
    let dst = if flags & FORMAT_MESSAGE_ALLOCATE_BUFFER != 0 {
        let p = crate::mem::LocalAlloc(0, b.len().max(size as usize) + 1);
        unsafe { *(buffer as *mut *mut u8) = p };
        p
    } else {
        if b.len() + 1 > size as usize {
            crate::SetLastError(122);
            return 0;
        }
        buffer
    };
    for (i, &u) in b.iter().enumerate() {
        unsafe { *dst.add(i) = u as u8 };
    }
    unsafe { *dst.add(b.len()) = 0 };
    b.len() as u32
}
