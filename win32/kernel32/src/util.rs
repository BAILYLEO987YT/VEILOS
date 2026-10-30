//! String conversions and the process heap, for the rest of kernel32.
//!
//! "ANSI" strings are treated as Latin-1 (each byte one character); real
//! Windows uses the system code page, which for ASCII text is the same.

use nt_abi::layout;

/// A UTF-16 string in a fixed buffer (MAX_PATH-sized and then some).
pub struct WBuf {
    pub buf: [u16; 1024],
    pub len: usize,
}

impl WBuf {
    pub const fn new() -> Self {
        WBuf { buf: [0; 1024], len: 0 }
    }

    pub fn as_slice(&self) -> &[u16] {
        &self.buf[..self.len]
    }

    pub fn push(&mut self, u: u16) {
        if self.len < self.buf.len() - 1 {
            self.buf[self.len] = u;
            self.len += 1;
        }
        self.buf[self.len] = 0;
    }

    pub fn extend(&mut self, units: &[u16]) {
        for &u in units {
            self.push(u);
        }
    }

    pub fn push_str(&mut self, s: &str) {
        for u in s.encode_utf16() {
            self.push(u);
        }
    }

    /// NUL-terminated pointer.
    pub fn as_ptr(&self) -> *const u16 {
        self.buf.as_ptr()
    }
}

/// Length of a NUL-terminated UTF-16 string.
pub unsafe fn wlen(p: *const u16) -> usize {
    let mut n = 0;
    while unsafe { *p.add(n) } != 0 {
        n += 1;
    }
    n
}

pub unsafe fn wstr<'a>(p: *const u16) -> &'a [u16] {
    if p.is_null() {
        return &[];
    }
    unsafe { core::slice::from_raw_parts(p, wlen(p)) }
}

/// Length of a NUL-terminated byte string.
pub unsafe fn alen(p: *const u8) -> usize {
    let mut n = 0;
    while unsafe { *p.add(n) } != 0 {
        n += 1;
    }
    n
}

/// An ANSI argument as UTF-16.
pub unsafe fn widen(p: *const u8) -> WBuf {
    let mut w = WBuf::new();
    if !p.is_null() {
        for i in 0..unsafe { alen(p) } {
            w.push(unsafe { *p.add(i) } as u16);
        }
    }
    w
}

/// Write `units` to an ANSI buffer of `size` bytes, NUL-terminated, the
/// Win32 way: the length without the NUL if it fit, else the size needed
/// with the NUL.
pub unsafe fn narrow_out(units: &[u16], out: *mut u8, size: u32) -> u32 {
    if (size as usize) <= units.len() || out.is_null() {
        return units.len() as u32 + 1;
    }
    for (i, &u) in units.iter().enumerate() {
        unsafe { *out.add(i) = if u < 0x100 { u as u8 } else { b'?' } };
    }
    unsafe { *out.add(units.len()) = 0 };
    units.len() as u32
}

/// The same for a UTF-16 buffer of `size` units.
pub unsafe fn wide_out(units: &[u16], out: *mut u16, size: u32) -> u32 {
    if (size as usize) <= units.len() || out.is_null() {
        return units.len() as u32 + 1;
    }
    unsafe {
        core::ptr::copy_nonoverlapping(units.as_ptr(), out, units.len());
        *out.add(units.len()) = 0;
    }
    units.len() as u32
}

pub fn eq_ignore_case(a: &[u16], b: &[u16]) -> bool {
    let lower = |u: u16| if (b'A' as u16..=b'Z' as u16).contains(&u) { u + 32 } else { u };
    a.len() == b.len() && a.iter().zip(b).all(|(&x, &y)| lower(x) == lower(y))
}

pub fn process_heap() -> usize {
    unsafe { *(crate::peb().add(layout::PEB_PROCESS_HEAP) as *const usize) }
}

pub fn alloc(size: usize) -> *mut u8 {
    unsafe { crate::RtlAllocateHeap(process_heap(), 0, size) }
}

pub fn free(p: *mut u8) {
    unsafe { crate::RtlFreeHeap(process_heap(), 0, p) };
}
