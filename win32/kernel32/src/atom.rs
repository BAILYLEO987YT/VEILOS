//! Atoms (WIN32_DESIGN.md, G4): names to 16-bit numbers. String atoms are
//! 0xC000 and up, matched ignoring case and reference-counted; "#123" and
//! integer atoms (below 0xC000) are the number itself. The global and local
//! tables are one table, private to the process for now.

use core::sync::atomic::{AtomicBool, Ordering};

use crate::util::wstr;
use crate::Bool;

const MAX_NAME: usize = 255;
const SLOTS: usize = 512;
const FIRST: u16 = 0xC000;
const ERROR_INVALID_PARAMETER: u32 = 87;
const ERROR_FILE_NOT_FOUND: u32 = 2;
const ERROR_INSUFFICIENT_BUFFER: u32 = 122;

#[derive(Clone, Copy)]
struct Atom {
    refs: u32,
    len: u16,
    name: [u16; MAX_NAME],
}

const EMPTY: Atom = Atom { refs: 0, len: 0, name: [0; MAX_NAME] };
static mut TABLE: [Atom; SLOTS] = [EMPTY; SLOTS];
static LOCK: AtomicBool = AtomicBool::new(false);

fn with<R>(f: impl FnOnce(&mut [Atom; SLOTS]) -> R) -> R {
    while LOCK.compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() {
        core::hint::spin_loop();
    }
    let r = f(unsafe { &mut *core::ptr::addr_of_mut!(TABLE) });
    LOCK.store(false, Ordering::Release);
    r
}

fn fold(c: u16) -> u16 {
    if (b'a' as u16..=b'z' as u16).contains(&c) { c - 32 } else { c }
}

/// What a name argument means: an integer atom (MAKEINTATOM, or "#123"),
/// or a string.
enum Name<'a> {
    Int(u16),
    Str(&'a [u16]),
}

/// `p` is MAKEINTATOM(n) when its high bits are clear.
unsafe fn name_of<'a>(p: *const u16) -> Option<Name<'a>> {
    if (p as usize) >> 16 == 0 {
        let n = p as usize as u16;
        return (n != 0 && n < FIRST).then_some(Name::Int(n));
    }
    let s = unsafe { wstr(p) };
    if s.len() > 1 && s[0] == b'#' as u16 && s[1..].iter().all(|c| (b'0' as u16..=b'9' as u16).contains(c)) {
        let n = s[1..].iter().fold(0u32, |n, &c| n.saturating_mul(10).saturating_add((c - b'0' as u16) as u32));
        return (n != 0 && n < FIRST as u32).then_some(Name::Int(n as u16));
    }
    (!s.is_empty() && s.len() <= MAX_NAME).then_some(Name::Str(s))
}

fn find(t: &[Atom; SLOTS], s: &[u16]) -> Option<usize> {
    t.iter().position(|a| a.refs > 0 && a.len as usize == s.len() && a.name[..s.len()].iter().zip(s).all(|(x, y)| fold(*x) == fold(*y)))
}

/// A wide copy of an ANSI name (or the integer atom itself).
fn widen_name(p: *const u8, buf: &mut [u16; MAX_NAME + 1]) -> *const u16 {
    if (p as usize) >> 16 == 0 {
        return p as *const u16;
    }
    let mut n = 0;
    while n < MAX_NAME {
        let c = unsafe { *p.add(n) };
        if c == 0 {
            break;
        }
        buf[n] = c as u16;
        n += 1;
    }
    buf[n] = 0;
    buf.as_ptr()
}

#[no_mangle]
pub unsafe extern "system" fn GlobalAddAtomW(p: *const u16) -> u16 {
    match unsafe { name_of(p) } {
        Some(Name::Int(n)) => n,
        Some(Name::Str(s)) => with(|t| {
            if let Some(i) = find(t, s) {
                t[i].refs += 1;
                return FIRST + i as u16;
            }
            let Some(i) = t.iter().position(|a| a.refs == 0) else {
                crate::SetLastError(8);
                return 0;
            };
            t[i].refs = 1;
            t[i].len = s.len() as u16;
            t[i].name[..s.len()].copy_from_slice(s);
            FIRST + i as u16
        }),
        None => {
            crate::SetLastError(ERROR_INVALID_PARAMETER);
            0
        }
    }
}

#[no_mangle]
pub unsafe extern "system" fn GlobalFindAtomW(p: *const u16) -> u16 {
    match unsafe { name_of(p) } {
        Some(Name::Int(n)) => n,
        Some(Name::Str(s)) => with(|t| find(t, s)).map_or_else(
            || {
                crate::SetLastError(ERROR_FILE_NOT_FOUND);
                0
            },
            |i| FIRST + i as u16,
        ),
        None => {
            crate::SetLastError(ERROR_INVALID_PARAMETER);
            0
        }
    }
}

/// 0 when it's gone (or was an integer atom), else the atom.
#[no_mangle]
pub extern "system" fn GlobalDeleteAtom(atom: u16) -> u16 {
    if atom < FIRST {
        return 0;
    }
    let i = (atom - FIRST) as usize;
    with(|t| {
        if i < SLOTS && t[i].refs > 0 {
            t[i].refs -= 1;
            0
        } else {
            crate::SetLastError(6);
            atom
        }
    })
}

/// The name (an integer atom's is "#n"); its length, 0 on failure.
#[no_mangle]
pub unsafe extern "system" fn GlobalGetAtomNameW(atom: u16, buf: *mut u16, cap: i32) -> u32 {
    let mut name = [0u16; MAX_NAME + 1];
    let len = if atom == 0 {
        0
    } else if atom < FIRST {
        let mut digits = [0u16; 5];
        let (mut n, mut k) = (atom, 0);
        while n > 0 {
            digits[k] = b'0' as u16 + n % 10;
            n /= 10;
            k += 1;
        }
        name[0] = b'#' as u16;
        for j in 0..k {
            name[1 + j] = digits[k - 1 - j];
        }
        k + 1
    } else {
        let i = (atom - FIRST) as usize;
        with(|t| {
            if i < SLOTS && t[i].refs > 0 {
                name[..t[i].len as usize].copy_from_slice(&t[i].name[..t[i].len as usize]);
                t[i].len as usize
            } else {
                0
            }
        })
    };
    if len == 0 {
        crate::SetLastError(ERROR_INVALID_PARAMETER);
        return 0;
    }
    if buf.is_null() || cap <= 0 {
        crate::SetLastError(ERROR_INSUFFICIENT_BUFFER);
        return 0;
    }
    let n = len.min(cap as usize - 1);
    unsafe {
        core::ptr::copy_nonoverlapping(name.as_ptr(), buf, n);
        *buf.add(n) = 0;
    }
    n as u32
}

#[no_mangle]
pub unsafe extern "system" fn GlobalAddAtomA(p: *const u8) -> u16 {
    let mut w = [0u16; MAX_NAME + 1];
    unsafe { GlobalAddAtomW(widen_name(p, &mut w)) }
}

#[no_mangle]
pub unsafe extern "system" fn GlobalFindAtomA(p: *const u8) -> u16 {
    let mut w = [0u16; MAX_NAME + 1];
    unsafe { GlobalFindAtomW(widen_name(p, &mut w)) }
}

#[no_mangle]
pub unsafe extern "system" fn GlobalGetAtomNameA(atom: u16, buf: *mut u8, cap: i32) -> u32 {
    let mut w = [0u16; MAX_NAME + 1];
    let n = unsafe { GlobalGetAtomNameW(atom, w.as_mut_ptr(), (MAX_NAME + 1) as i32) } as usize;
    if n == 0 || buf.is_null() || cap <= 0 {
        return 0;
    }
    let k = n.min(cap as usize - 1);
    for (i, &c) in w[..k].iter().enumerate() {
        unsafe { *buf.add(i) = if c < 0x80 { c as u8 } else { b'?' } };
    }
    unsafe { *buf.add(k) = 0 };
    k as u32
}

// The local atom table is the same table.

#[no_mangle]
pub unsafe extern "system" fn AddAtomW(p: *const u16) -> u16 {
    unsafe { GlobalAddAtomW(p) }
}

#[no_mangle]
pub unsafe extern "system" fn AddAtomA(p: *const u8) -> u16 {
    unsafe { GlobalAddAtomA(p) }
}

#[no_mangle]
pub unsafe extern "system" fn FindAtomW(p: *const u16) -> u16 {
    unsafe { GlobalFindAtomW(p) }
}

#[no_mangle]
pub unsafe extern "system" fn FindAtomA(p: *const u8) -> u16 {
    unsafe { GlobalFindAtomA(p) }
}

#[no_mangle]
pub extern "system" fn DeleteAtom(atom: u16) -> u16 {
    GlobalDeleteAtom(atom)
}

#[no_mangle]
pub unsafe extern "system" fn GetAtomNameW(atom: u16, buf: *mut u16, cap: i32) -> u32 {
    unsafe { GlobalGetAtomNameW(atom, buf, cap) }
}

#[no_mangle]
pub unsafe extern "system" fn GetAtomNameA(atom: u16, buf: *mut u8, cap: i32) -> u32 {
    unsafe { GlobalGetAtomNameA(atom, buf, cap) }
}

#[no_mangle]
pub extern "system" fn InitAtomTable(_size: u32) -> Bool {
    1
}
