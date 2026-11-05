//! The command line and environment variables. Both live in the process
//! parameters; the environment block ("NAME=value\0" ... "\0", UTF-16) is
//! rebuilt on the process heap when a variable changes.

use core::cell::UnsafeCell;
use nt_abi::{layout, UnicodeString};

use crate::util::{alloc, free, narrow_out, wide_out, widen, wlen, wstr, WBuf};
use crate::Bool;

const ERROR_ENVVAR_NOT_FOUND: u32 = 203;

/// A buffer kernel32 owns for the life of the process.
struct Static<T>(UnsafeCell<T>);
unsafe impl<T> Sync for Static<T> {}

fn params() -> *mut u8 {
    crate::process_parameters()
}

#[no_mangle]
pub extern "system" fn GetCommandLineW() -> *const u16 {
    unsafe { (*(params().add(layout::PARAMS_COMMAND_LINE) as *const UnicodeString)).buffer }
}

static COMMAND_LINE_A: Static<[u8; 98304]> = Static(UnsafeCell::new([0; 98304]));

#[no_mangle]
pub extern "system" fn GetCommandLineA() -> *const u8 {
    let buf = COMMAND_LINE_A.0.get() as *mut u8;
    unsafe {
        if *buf == 0 {
            let w = wstr(GetCommandLineW());
            narrow_out(w, buf, 98304);
        }
    }
    buf
}

fn block() -> *mut u16 {
    unsafe { *(params().add(layout::PARAMS_ENVIRONMENT) as *const *mut u16) }
}

/// Every "NAME=value" entry, in order.
fn entries(mut f: impl FnMut(&[u16]) -> bool) {
    let mut p = block();
    if p.is_null() {
        return;
    }
    unsafe {
        loop {
            let n = wlen(p);
            if n == 0 {
                return;
            }
            if !f(core::slice::from_raw_parts(p, n)) {
                return;
            }
            p = p.add(n + 1);
        }
    }
}

/// The value part of `entry` if its name is `name` (ignoring case).
fn value_of<'a>(entry: &'a [u16], name: &[u16]) -> Option<&'a [u16]> {
    // Skip a leading '=' (the hidden "=C:=..." entries) when finding the separator.
    let eq = entry.iter().skip(1).position(|&u| u == b'=' as u16)? + 1;
    crate::util::eq_ignore_case(&entry[..eq], name).then(|| &entry[eq + 1..])
}

fn lookup(name: &[u16], f: impl FnOnce(Option<&[u16]>) -> u32) -> u32 {
    let mut found: Option<(*const u16, usize)> = None;
    entries(|e| {
        if let Some(v) = value_of(e, name) {
            found = Some((v.as_ptr(), v.len()));
            return false;
        }
        true
    });
    f(found.map(|(p, n)| unsafe { core::slice::from_raw_parts(p, n) }))
}

#[no_mangle]
pub unsafe extern "system" fn GetEnvironmentVariableW(name: *const u16, buffer: *mut u16, size: u32) -> u32 {
    lookup(unsafe { wstr(name) }, |v| match v {
        Some(v) => unsafe { wide_out(v, buffer, size) },
        None => {
            crate::SetLastError(ERROR_ENVVAR_NOT_FOUND);
            0
        }
    })
}

#[no_mangle]
pub unsafe extern "system" fn GetEnvironmentVariableA(name: *const u8, buffer: *mut u8, size: u32) -> u32 {
    let n = unsafe { widen(name) };
    lookup(n.as_slice(), |v| match v {
        Some(v) => unsafe { narrow_out(v, buffer, size) },
        None => {
            crate::SetLastError(ERROR_ENVVAR_NOT_FOUND);
            0
        }
    })
}

/// Set `name` to `value` (None removes it): a new block on the heap.
fn set_var(name: &[u16], value: Option<&[u16]>) -> Bool {
    let mut size = 1usize;
    entries(|e| {
        if value_of(e, name).is_none() {
            size += e.len() + 1;
        }
        true
    });
    if let Some(v) = value {
        size += name.len() + 1 + v.len() + 1;
    }
    let new = alloc(size * 2) as *mut u16;
    if new.is_null() {
        crate::SetLastError(8);
        return 0;
    }
    let mut at = 0usize;
    let mut put = |units: &[u16]| {
        unsafe { core::ptr::copy_nonoverlapping(units.as_ptr(), new.add(at), units.len()) };
        at += units.len();
    };
    entries(|e| {
        if value_of(e, name).is_none() {
            put(e);
            put(&[0]);
        }
        true
    });
    if let Some(v) = value {
        put(name);
        put(&[b'=' as u16]);
        put(v);
        put(&[0]);
    }
    put(&[0]);
    unsafe {
        *(params().add(layout::PARAMS_ENVIRONMENT) as *mut *mut u16) = new;
        *(params().add(layout::PARAMS_ENVIRONMENT_SIZE) as *mut u64) = (size * 2) as u64;
    }
    1
}

#[no_mangle]
pub unsafe extern "system" fn SetEnvironmentVariableW(name: *const u16, value: *const u16) -> Bool {
    let v = (!value.is_null()).then(|| unsafe { wstr(value) });
    set_var(unsafe { wstr(name) }, v)
}

#[no_mangle]
pub unsafe extern "system" fn SetEnvironmentVariableA(name: *const u8, value: *const u8) -> Bool {
    let n = unsafe { widen(name) };
    if value.is_null() {
        return set_var(n.as_slice(), None);
    }
    let v = unsafe { widen(value) };
    set_var(n.as_slice(), Some(v.as_slice()))
}

/// The block's size in UTF-16 units, through its final NUL.
fn block_units() -> usize {
    let mut n = 1;
    entries(|e| {
        n += e.len() + 1;
        true
    });
    n
}

#[no_mangle]
pub extern "system" fn GetEnvironmentStringsW() -> *mut u16 {
    let units = block_units();
    let copy = alloc(units * 2) as *mut u16;
    if !copy.is_null() {
        unsafe { core::ptr::copy_nonoverlapping(block(), copy, units) };
    }
    copy
}

#[no_mangle]
pub extern "system" fn GetEnvironmentStringsA() -> *mut u8 {
    let units = block_units();
    let copy = alloc(units);
    if !copy.is_null() {
        let src = unsafe { core::slice::from_raw_parts(block(), units) };
        for (i, &u) in src.iter().enumerate() {
            unsafe { *copy.add(i) = if u < 0x100 { u as u8 } else { b'?' } };
        }
    }
    copy
}

#[no_mangle]
pub extern "system" fn GetEnvironmentStrings() -> *mut u8 {
    GetEnvironmentStringsA()
}

#[no_mangle]
pub extern "system" fn FreeEnvironmentStringsW(p: *mut u16) -> Bool {
    free(p as *mut u8);
    1
}

#[no_mangle]
pub extern "system" fn FreeEnvironmentStringsA(p: *mut u8) -> Bool {
    free(p);
    1
}

/// %NAME% replaced by its value; unknown names are left as they are.
fn expand(src: &[u16]) -> WBuf {
    let mut out = WBuf::new();
    let mut i = 0;
    while i < src.len() {
        if src[i] == b'%' as u16 {
            if let Some(len) = src[i + 1..].iter().position(|&u| u == b'%' as u16) {
                let name = &src[i + 1..i + 1 + len];
                let mut replaced = false;
                if !name.is_empty() {
                    lookup(name, |v| {
                        if let Some(v) = v {
                            out.extend(v);
                            replaced = true;
                        }
                        0
                    });
                }
                if replaced {
                    i += len + 2;
                    continue;
                }
            }
        }
        out.push(src[i]);
        i += 1;
    }
    out
}

/// Returns the size needed, including the terminator.
#[no_mangle]
pub unsafe extern "system" fn ExpandEnvironmentStringsW(src: *const u16, dst: *mut u16, size: u32) -> u32 {
    let e = expand(unsafe { wstr(src) });
    if (size as usize) > e.len && !dst.is_null() {
        unsafe { wide_out(e.as_slice(), dst, size) };
    }
    e.len as u32 + 1
}

#[no_mangle]
pub unsafe extern "system" fn ExpandEnvironmentStringsA(src: *const u8, dst: *mut u8, size: u32) -> u32 {
    let w = unsafe { widen(src) };
    let e = expand(w.as_slice());
    if (size as usize) > e.len && !dst.is_null() {
        unsafe { narrow_out(e.as_slice(), dst, size) };
    }
    e.len as u32 + 1
}
