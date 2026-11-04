//! The registry calls (advapi32's and the core API sets'), over the kernel's
//! store through `VEIL_REG`. W forms do the work; A forms convert names, and
//! string data, to and from the ANSI code page.

use nt_abi::reg::{self, RegCall};

use crate::util::{alloc, free, widen, wstr};
use crate::Handle;

const ERROR_SUCCESS: u32 = 0;
const ERROR_INVALID_PARAMETER: u32 = 87;
const ERROR_MORE_DATA: u32 = 234;
const ERROR_UNSUPPORTED_TYPE: u32 = 1630;
const ERROR_NOT_ENOUGH_MEMORY: u32 = 8;
const ERROR_FILE_NOT_FOUND: u32 = 2;
const ERROR_ACCESS_DENIED: u32 = 5;

const REG_SZ: u32 = 1;
const REG_EXPAND_SZ: u32 = 2;
const REG_DWORD: u32 = 4;
const REG_MULTI_SZ: u32 = 7;
const REG_QWORD: u32 = 11;

fn run(c: &mut RegCall) -> u32 {
    unsafe { crate::VeilReg(c) as u32 }
}

fn call(op: u32, key: Handle) -> RegCall {
    RegCall { op, key, ..RegCall::default() }
}

fn with_name(c: &mut RegCall, name: *const u16) {
    let s = unsafe { wstr(name) };
    c.name = s.as_ptr() as u64;
    c.name_len = s.len() as u32;
}

fn is_text(kind: u32) -> bool {
    matches!(kind, REG_SZ | REG_EXPAND_SZ | REG_MULTI_SZ)
}

// -----| keys |-----

#[no_mangle]
pub unsafe extern "system" fn RegOpenKeyExW(key: Handle, sub: *const u16, _opts: u32, _sam: u32, out: *mut Handle) -> u32 {
    if out.is_null() {
        return ERROR_INVALID_PARAMETER;
    }
    let mut c = call(reg::OPEN, key);
    with_name(&mut c, sub);
    let e = run(&mut c);
    unsafe { *out = if e == ERROR_SUCCESS { c.out_key } else { 0 } };
    e
}

#[no_mangle]
pub unsafe extern "system" fn RegOpenKeyExA(key: Handle, sub: *const u8, opts: u32, sam: u32, out: *mut Handle) -> u32 {
    let w = unsafe { widen(sub) };
    unsafe { RegOpenKeyExW(key, w.as_ptr(), opts, sam, out) }
}

#[no_mangle]
pub unsafe extern "system" fn RegOpenKeyW(key: Handle, sub: *const u16, out: *mut Handle) -> u32 {
    unsafe { RegOpenKeyExW(key, sub, 0, 0, out) }
}

#[no_mangle]
pub unsafe extern "system" fn RegOpenKeyA(key: Handle, sub: *const u8, out: *mut Handle) -> u32 {
    unsafe { RegOpenKeyExA(key, sub, 0, 0, out) }
}

#[no_mangle]
pub unsafe extern "system" fn RegOpenCurrentUser(_sam: u32, out: *mut Handle) -> u32 {
    if out.is_null() {
        return ERROR_INVALID_PARAMETER;
    }
    unsafe { *out = 0x8000_0001 };
    ERROR_SUCCESS
}

#[no_mangle]
pub unsafe extern "system" fn RegCreateKeyExW(key: Handle, sub: *const u16, _r: u32, _class: usize, _opts: u32, _sam: u32, _sa: usize, out: *mut Handle, disp: *mut u32) -> u32 {
    if out.is_null() {
        return ERROR_INVALID_PARAMETER;
    }
    let mut c = call(reg::CREATE, key);
    with_name(&mut c, sub);
    let e = run(&mut c);
    unsafe {
        *out = if e == ERROR_SUCCESS { c.out_key } else { 0 };
        if e == ERROR_SUCCESS && !disp.is_null() {
            *disp = c.flags;
        }
    }
    e
}

#[no_mangle]
pub unsafe extern "system" fn RegCreateKeyExA(key: Handle, sub: *const u8, r: u32, class: usize, opts: u32, sam: u32, sa: usize, out: *mut Handle, disp: *mut u32) -> u32 {
    let w = unsafe { widen(sub) };
    unsafe { RegCreateKeyExW(key, w.as_ptr(), r, class, opts, sam, sa, out, disp) }
}

#[no_mangle]
pub unsafe extern "system" fn RegCreateKeyW(key: Handle, sub: *const u16, out: *mut Handle) -> u32 {
    unsafe { RegCreateKeyExW(key, sub, 0, 0, 0, 0, 0, out, core::ptr::null_mut()) }
}

#[no_mangle]
pub unsafe extern "system" fn RegCreateKeyA(key: Handle, sub: *const u8, out: *mut Handle) -> u32 {
    unsafe { RegCreateKeyExA(key, sub, 0, 0, 0, 0, 0, out, core::ptr::null_mut()) }
}

#[no_mangle]
pub extern "system" fn RegCloseKey(key: Handle) -> u32 {
    let mut c = call(reg::CLOSE, key);
    run(&mut c)
}

#[no_mangle]
pub extern "system" fn RegFlushKey(key: Handle) -> u32 {
    let mut c = call(reg::FLUSH, key);
    run(&mut c)
}

/// Only a key with no subkeys.
#[no_mangle]
pub unsafe extern "system" fn RegDeleteKeyW(key: Handle, sub: *const u16) -> u32 {
    let mut c = call(reg::DELETE_KEY, key);
    with_name(&mut c, sub);
    run(&mut c)
}

#[no_mangle]
pub unsafe extern "system" fn RegDeleteKeyA(key: Handle, sub: *const u8) -> u32 {
    let w = unsafe { widen(sub) };
    unsafe { RegDeleteKeyW(key, w.as_ptr()) }
}

#[no_mangle]
pub unsafe extern "system" fn RegDeleteKeyExW(key: Handle, sub: *const u16, _sam: u32, _r: u32) -> u32 {
    unsafe { RegDeleteKeyW(key, sub) }
}

#[no_mangle]
pub unsafe extern "system" fn RegDeleteKeyExA(key: Handle, sub: *const u8, _sam: u32, _r: u32) -> u32 {
    unsafe { RegDeleteKeyA(key, sub) }
}

/// The key `sub` with everything under it (or, with no `sub`, all that's
/// under `key` but not `key` itself).
#[no_mangle]
pub unsafe extern "system" fn RegDeleteTreeW(key: Handle, sub: *const u16) -> u32 {
    if !sub.is_null() && unsafe { *sub } != 0 {
        let mut c = call(reg::DELETE_KEY, key);
        with_name(&mut c, sub);
        c.flags = 1;
        return run(&mut c);
    }
    // Empty it: keep taking the first subkey and the first value.
    let mut name = [0u16; 512];
    loop {
        let mut c = call(reg::ENUM_KEY, key);
        c.out_name = name.as_mut_ptr() as u64;
        c.out_name_cap = 512;
        if run(&mut c) != ERROR_SUCCESS {
            break;
        }
        let mut d = call(reg::DELETE_KEY, key);
        d.name = name.as_ptr() as u64;
        d.name_len = c.out_name_len;
        d.flags = 1;
        if run(&mut d) != ERROR_SUCCESS {
            break;
        }
    }
    loop {
        let mut c = call(reg::ENUM_VALUE, key);
        c.out_name = name.as_mut_ptr() as u64;
        c.out_name_cap = 512;
        if run(&mut c) != ERROR_SUCCESS {
            break;
        }
        let mut d = call(reg::DELETE_VALUE, key);
        d.name = name.as_ptr() as u64;
        d.name_len = c.out_name_len;
        if run(&mut d) != ERROR_SUCCESS {
            break;
        }
    }
    ERROR_SUCCESS
}

#[no_mangle]
pub unsafe extern "system" fn RegDeleteTreeA(key: Handle, sub: *const u8) -> u32 {
    if sub.is_null() {
        return unsafe { RegDeleteTreeW(key, core::ptr::null()) };
    }
    let w = unsafe { widen(sub) };
    unsafe { RegDeleteTreeW(key, w.as_ptr()) }
}

#[no_mangle]
pub unsafe extern "system" fn RegRenameKey(key: Handle, sub: *const u16, new: *const u16) -> u32 {
    let mut c = call(reg::RENAME, key);
    with_name(&mut c, sub);
    let n = unsafe { wstr(new) };
    c.data = n.as_ptr() as u64;
    c.data_len = n.len() as u32;
    run(&mut c)
}

// -----| values |-----

#[no_mangle]
pub unsafe extern "system" fn RegQueryValueExW(key: Handle, name: *const u16, _r: usize, kind: *mut u32, data: *mut u8, size: *mut u32) -> u32 {
    if !data.is_null() && size.is_null() {
        return ERROR_INVALID_PARAMETER;
    }
    let mut c = call(reg::QUERY_VALUE, key);
    with_name(&mut c, name);
    c.data = data as u64;
    c.data_len = if size.is_null() { 0 } else { unsafe { *size } };
    let e = run(&mut c);
    if e == ERROR_SUCCESS || e == ERROR_MORE_DATA {
        unsafe {
            if !kind.is_null() {
                *kind = c.kind;
            }
            if !size.is_null() {
                *size = c.data_len;
            }
        }
    }
    e
}

/// Wide text in `w` (`bytes` long) as ANSI bytes into `out`: the bytes it
/// takes (with `out` null or too small, just the count).
unsafe fn narrow_into(w: *const u16, bytes: u32, out: *mut u8, cap: u32) -> u32 {
    let units = (bytes / 2) as i32;
    if units == 0 {
        return 0;
    }
    let need = unsafe { crate::locale::WideCharToMultiByte(0, 0, w, units, core::ptr::null_mut(), 0, core::ptr::null(), core::ptr::null_mut()) }.max(0) as u32;
    if !out.is_null() && need <= cap {
        unsafe { crate::locale::WideCharToMultiByte(0, 0, w, units, out, cap as i32, core::ptr::null(), core::ptr::null_mut()) };
    }
    need
}

#[no_mangle]
pub unsafe extern "system" fn RegQueryValueExA(key: Handle, name: *const u8, _r: usize, kind: *mut u32, data: *mut u8, size: *mut u32) -> u32 {
    if !data.is_null() && size.is_null() {
        return ERROR_INVALID_PARAMETER;
    }
    let wname = unsafe { widen(name) };
    // How big, and what kind, in the wide form.
    let mut c = call(reg::QUERY_VALUE, key);
    with_name(&mut c, if name.is_null() { core::ptr::null() } else { wname.as_ptr() });
    let e = run(&mut c);
    if e != ERROR_SUCCESS {
        return e;
    }
    let (k, bytes) = (c.kind, c.data_len);
    if !kind.is_null() {
        unsafe { *kind = k };
    }
    if !is_text(k) {
        return unsafe { RegQueryValueExW(key, c.name as *const u16, 0, core::ptr::null_mut(), data, size) };
    }
    let tmp = alloc(bytes.max(2) as usize) as *mut u16;
    if tmp.is_null() {
        return ERROR_NOT_ENOUGH_MEMORY;
    }
    let mut q = call(reg::QUERY_VALUE, key);
    q.name = c.name;
    q.name_len = c.name_len;
    q.data = tmp as u64;
    q.data_len = bytes;
    let e = run(&mut q);
    let mut result = e;
    if e == ERROR_SUCCESS {
        let cap = if size.is_null() { 0 } else { unsafe { *size } };
        let need = unsafe { narrow_into(tmp, bytes, data, cap) };
        if !size.is_null() {
            unsafe { *size = need };
        }
        if !data.is_null() && need > cap {
            result = ERROR_MORE_DATA;
        }
    }
    free(tmp as *mut u8);
    result
}

/// The RRF_RT_* bit a value's kind satisfies.
fn rrf_bit(kind: u32) -> u32 {
    match kind {
        0 => 0x1,
        REG_SZ => 0x2,
        REG_EXPAND_SZ => 0x4,
        3 => 0x8,
        REG_DWORD => 0x10,
        REG_MULTI_SZ => 0x20,
        REG_QWORD => 0x40,
        _ => 0,
    }
}

#[no_mangle]
pub unsafe extern "system" fn RegGetValueW(key: Handle, sub: *const u16, name: *const u16, flags: u32, kind: *mut u32, data: *mut u8, size: *mut u32) -> u32 {
    const RRF_NOEXPAND: u32 = 0x1000_0000;
    const RRF_ZEROONFAILURE: u32 = 0x2000_0000;
    let mut opened: Handle = 0;
    let at = if !sub.is_null() && unsafe { *sub } != 0 {
        let e = unsafe { RegOpenKeyExW(key, sub, 0, 0, &mut opened) };
        if e != ERROR_SUCCESS {
            return e;
        }
        opened
    } else {
        key
    };
    let cap = if size.is_null() { 0 } else { unsafe { *size } };
    let mut k = 0u32;
    let mut len = cap;
    let mut e = unsafe { RegQueryValueExW(at, name, 0, &mut k, data, &mut len) };
    if (e == ERROR_SUCCESS || e == ERROR_MORE_DATA) && flags & 0xFFFF & rrf_bit(k) == 0 {
        // An expandable string asked for as a plain one is expanded to one.
        if !(k == REG_EXPAND_SZ && flags & 0x2 != 0 && flags & RRF_NOEXPAND == 0) {
            e = ERROR_UNSUPPORTED_TYPE;
        }
    }
    if e == ERROR_SUCCESS || e == ERROR_MORE_DATA {
        if k == REG_EXPAND_SZ && flags & RRF_NOEXPAND == 0 && e == ERROR_SUCCESS && !data.is_null() {
            // Expand in place, by way of a copy.
            let tmp = alloc(len.max(2) as usize) as *mut u16;
            if !tmp.is_null() {
                unsafe {
                    core::ptr::copy_nonoverlapping(data, tmp as *mut u8, len as usize);
                    let n = crate::env::ExpandEnvironmentStringsW(tmp, data as *mut u16, cap / 2);
                    if n * 2 > cap {
                        e = ERROR_MORE_DATA;
                    }
                    // The larger of the two forms, as Windows reports it.
                    len = len.max(n * 2);
                    k = REG_SZ;
                }
                free(tmp as *mut u8);
            }
        }
        // Strings come back terminated, whatever was stored.
        if is_text(k) && e == ERROR_SUCCESS && !data.is_null() && len >= 2 {
            let last = unsafe { *(data as *const u16).add(len as usize / 2 - 1) };
            if last != 0 {
                if len + 2 > cap {
                    e = ERROR_MORE_DATA;
                } else {
                    unsafe { *(data as *mut u16).add(len as usize / 2) = 0 };
                }
                len += 2;
            }
        }
        unsafe {
            if !kind.is_null() {
                *kind = k;
            }
            if !size.is_null() {
                *size = len;
            }
        }
    } else if flags & RRF_ZEROONFAILURE != 0 && !data.is_null() {
        unsafe { core::ptr::write_bytes(data, 0, cap as usize) };
    }
    if opened != 0 {
        RegCloseKey(opened);
    }
    e
}

#[no_mangle]
pub unsafe extern "system" fn RegGetValueA(key: Handle, sub: *const u8, name: *const u8, flags: u32, kind: *mut u32, data: *mut u8, size: *mut u32) -> u32 {
    let mut opened: Handle = 0;
    let at = if !sub.is_null() && unsafe { *sub } != 0 {
        let e = unsafe { RegOpenKeyExA(key, sub, 0, 0, &mut opened) };
        if e != ERROR_SUCCESS {
            return e;
        }
        opened
    } else {
        key
    };
    let mut k = 0u32;
    let e = unsafe { RegQueryValueExA(at, name, 0, &mut k, data, size) };
    let e = if (e == ERROR_SUCCESS || e == ERROR_MORE_DATA) && flags & 0xFFFF & rrf_bit(k) == 0 { ERROR_UNSUPPORTED_TYPE } else { e };
    if !kind.is_null() && e != ERROR_UNSUPPORTED_TYPE {
        unsafe { *kind = k };
    }
    if opened != 0 {
        RegCloseKey(opened);
    }
    e
}

#[no_mangle]
pub unsafe extern "system" fn RegSetValueExW(key: Handle, name: *const u16, _r: u32, kind: u32, data: *const u8, size: u32) -> u32 {
    let mut c = call(reg::SET_VALUE, key);
    with_name(&mut c, name);
    c.kind = kind;
    c.data = data as u64;
    c.data_len = if data.is_null() { 0 } else { size };
    run(&mut c)
}

#[no_mangle]
pub unsafe extern "system" fn RegSetValueExA(key: Handle, name: *const u8, r: u32, kind: u32, data: *const u8, size: u32) -> u32 {
    let wname = unsafe { widen(name) };
    let np = if name.is_null() { core::ptr::null() } else { wname.as_ptr() };
    if !is_text(kind) || data.is_null() {
        return unsafe { RegSetValueExW(key, np, r, kind, data, size) };
    }
    let units = unsafe { crate::locale::MultiByteToWideChar(0, 0, data, size as i32, core::ptr::null_mut(), 0) }.max(0) as usize;
    let tmp = alloc((units + 1) * 2) as *mut u16;
    if tmp.is_null() {
        return ERROR_NOT_ENOUGH_MEMORY;
    }
    unsafe { crate::locale::MultiByteToWideChar(0, 0, data, size as i32, tmp, units as i32) };
    let e = unsafe { RegSetValueExW(key, np, r, kind, tmp as *const u8, units as u32 * 2) };
    free(tmp as *mut u8);
    e
}

#[no_mangle]
pub unsafe extern "system" fn RegSetKeyValueW(key: Handle, sub: *const u16, name: *const u16, kind: u32, data: *const u8, size: u32) -> u32 {
    let mut h: Handle = 0;
    let at = if !sub.is_null() && unsafe { *sub } != 0 {
        let e = unsafe { RegCreateKeyW(key, sub, &mut h) };
        if e != ERROR_SUCCESS {
            return e;
        }
        h
    } else {
        key
    };
    let e = unsafe { RegSetValueExW(at, name, 0, kind, data, size) };
    if h != 0 {
        RegCloseKey(h);
    }
    e
}

#[no_mangle]
pub unsafe extern "system" fn RegSetKeyValueA(key: Handle, sub: *const u8, name: *const u8, kind: u32, data: *const u8, size: u32) -> u32 {
    let mut h: Handle = 0;
    let at = if !sub.is_null() && unsafe { *sub } != 0 {
        let e = unsafe { RegCreateKeyA(key, sub, &mut h) };
        if e != ERROR_SUCCESS {
            return e;
        }
        h
    } else {
        key
    };
    let e = unsafe { RegSetValueExA(at, name, 0, kind, data, size) };
    if h != 0 {
        RegCloseKey(h);
    }
    e
}

/// The old form: `sub`'s default value, a string.
#[no_mangle]
pub unsafe extern "system" fn RegSetValueW(key: Handle, sub: *const u16, kind: u32, data: *const u16, _size: u32) -> u32 {
    if kind != REG_SZ || data.is_null() {
        return ERROR_INVALID_PARAMETER;
    }
    let bytes = (unsafe { wstr(data) }.len() as u32 + 1) * 2;
    unsafe { RegSetKeyValueW(key, sub, core::ptr::null(), REG_SZ, data as *const u8, bytes) }
}

#[no_mangle]
pub unsafe extern "system" fn RegQueryValueW(key: Handle, sub: *const u16, out: *mut u16, size: *mut i32) -> u32 {
    let mut len = if size.is_null() { 0 } else { unsafe { *size as u32 } };
    let e = unsafe { RegGetValueW(key, sub, core::ptr::null(), 0xFFFF, core::ptr::null_mut(), out as *mut u8, &mut len) };
    if !size.is_null() {
        unsafe { *size = len as i32 };
    }
    // A key without a default value reads as empty.
    if e == ERROR_FILE_NOT_FOUND && unsafe { key_exists(key, sub) } {
        unsafe {
            if !out.is_null() {
                *out = 0;
            }
            if !size.is_null() {
                *size = 2;
            }
        }
        return ERROR_SUCCESS;
    }
    e
}

unsafe fn key_exists(key: Handle, sub: *const u16) -> bool {
    let mut h: Handle = 0;
    let ok = unsafe { RegOpenKeyExW(key, sub, 0, 0, &mut h) } == ERROR_SUCCESS;
    if ok {
        RegCloseKey(h);
    }
    ok
}

#[no_mangle]
pub unsafe extern "system" fn RegDeleteValueW(key: Handle, name: *const u16) -> u32 {
    let mut c = call(reg::DELETE_VALUE, key);
    with_name(&mut c, name);
    run(&mut c)
}

#[no_mangle]
pub unsafe extern "system" fn RegDeleteValueA(key: Handle, name: *const u8) -> u32 {
    let w = unsafe { widen(name) };
    unsafe { RegDeleteValueW(key, if name.is_null() { core::ptr::null() } else { w.as_ptr() }) }
}

// -----| listing |-----

#[no_mangle]
pub unsafe extern "system" fn RegEnumKeyExW(key: Handle, i: u32, name: *mut u16, len: *mut u32, _r: usize, class: *mut u16, clen: *mut u32, time: *mut u64) -> u32 {
    if name.is_null() || len.is_null() {
        return ERROR_INVALID_PARAMETER;
    }
    let mut c = call(reg::ENUM_KEY, key);
    c.index = i;
    c.out_name = name as u64;
    c.out_name_cap = unsafe { *len };
    let e = run(&mut c);
    if e == ERROR_SUCCESS {
        unsafe { *len = c.out_name_len };
    }
    unsafe {
        if !class.is_null() && !clen.is_null() && *clen > 0 {
            *class = 0;
        }
        if !clen.is_null() {
            *clen = 0;
        }
        if !time.is_null() {
            *time = 0;
        }
    }
    e
}

#[no_mangle]
pub unsafe extern "system" fn RegEnumKeyW(key: Handle, i: u32, name: *mut u16, cap: u32) -> u32 {
    let mut len = cap;
    unsafe { RegEnumKeyExW(key, i, name, &mut len, 0, core::ptr::null_mut(), core::ptr::null_mut(), core::ptr::null_mut()) }
}

#[no_mangle]
pub unsafe extern "system" fn RegEnumKeyExA(key: Handle, i: u32, name: *mut u8, len: *mut u32, _r: usize, class: *mut u8, clen: *mut u32, time: *mut u64) -> u32 {
    if name.is_null() || len.is_null() {
        return ERROR_INVALID_PARAMETER;
    }
    let mut w = [0u16; 512];
    let mut wl = 512u32;
    let e = unsafe { RegEnumKeyExW(key, i, w.as_mut_ptr(), &mut wl, 0, core::ptr::null_mut(), core::ptr::null_mut(), time) };
    if e != ERROR_SUCCESS {
        return e;
    }
    let cap = unsafe { *len };
    let need = unsafe { narrow_into(w.as_ptr(), wl * 2, name, cap) };
    if need >= cap {
        return ERROR_MORE_DATA;
    }
    unsafe {
        *name.add(need as usize) = 0;
        *len = need;
        if !class.is_null() && !clen.is_null() && *clen > 0 {
            *class = 0;
        }
        if !clen.is_null() {
            *clen = 0;
        }
    }
    ERROR_SUCCESS
}

#[no_mangle]
pub unsafe extern "system" fn RegEnumValueW(key: Handle, i: u32, name: *mut u16, len: *mut u32, _r: usize, kind: *mut u32, data: *mut u8, dlen: *mut u32) -> u32 {
    if name.is_null() || len.is_null() {
        return ERROR_INVALID_PARAMETER;
    }
    let mut c = call(reg::ENUM_VALUE, key);
    c.index = i;
    c.out_name = name as u64;
    c.out_name_cap = unsafe { *len };
    c.data = data as u64;
    c.data_len = if dlen.is_null() { 0 } else { unsafe { *dlen } };
    let e = run(&mut c);
    if e == ERROR_SUCCESS || e == ERROR_MORE_DATA {
        // Text comes back terminated, whatever was stored.
        if e == ERROR_SUCCESS && is_text(c.kind) && !data.is_null() && c.data_len >= 2 {
            let room = if dlen.is_null() { 0 } else { unsafe { *dlen } };
            let at = c.data_len as usize / 2;
            if unsafe { *(data as *const u16).add(at - 1) } != 0 {
                if c.data_len + 2 <= room {
                    unsafe { *(data as *mut u16).add(at) = 0 };
                }
                c.data_len += 2;
            }
        }
        unsafe {
            *len = c.out_name_len;
            if !kind.is_null() {
                *kind = c.kind;
            }
            if !dlen.is_null() {
                *dlen = c.data_len;
            }
        }
    }
    e
}

#[no_mangle]
pub unsafe extern "system" fn RegEnumValueA(key: Handle, i: u32, name: *mut u8, len: *mut u32, _r: usize, kind: *mut u32, data: *mut u8, dlen: *mut u32) -> u32 {
    if name.is_null() || len.is_null() {
        return ERROR_INVALID_PARAMETER;
    }
    let mut w = [0u16; 512];
    let mut wl = 512u32;
    let mut k = 0u32;
    let mut size = 0u32;
    let e = unsafe { RegEnumValueW(key, i, w.as_mut_ptr(), &mut wl, 0, &mut k, core::ptr::null_mut(), &mut size) };
    if e != ERROR_SUCCESS {
        return e;
    }
    let cap = unsafe { *len };
    let need = unsafe { narrow_into(w.as_ptr(), wl * 2, name, cap) };
    if need >= cap {
        return ERROR_MORE_DATA;
    }
    unsafe {
        *name.add(need as usize) = 0;
        *len = need;
        if !kind.is_null() {
            *kind = k;
        }
    }
    unsafe { RegQueryValueExA(key, name, 0, core::ptr::null_mut(), data, dlen) }
}

#[no_mangle]
pub unsafe extern "system" fn RegQueryInfoKeyW(key: Handle, class: *mut u16, clen: *mut u32, _r: usize, subkeys: *mut u32, maxsub: *mut u32, maxclass: *mut u32,
                                               values: *mut u32, maxname: *mut u32, maxdata: *mut u32, sd: *mut u32, time: *mut u64) -> u32 {
    let mut c = call(reg::INFO, key);
    let e = run(&mut c);
    if e != ERROR_SUCCESS {
        return e;
    }
    let put = |p: *mut u32, v: u32| {
        if !p.is_null() {
            unsafe { *p = v };
        }
    };
    put(subkeys, c.info[0]);
    put(maxsub, c.info[1]);
    put(maxclass, 0);
    put(values, c.info[2]);
    put(maxname, c.info[3]);
    put(maxdata, c.info[4]);
    put(sd, 0);
    unsafe {
        if !class.is_null() && !clen.is_null() && *clen > 0 {
            *class = 0;
        }
        if !time.is_null() {
            *time = 0;
        }
    }
    put(clen, 0);
    ERROR_SUCCESS
}

#[no_mangle]
pub unsafe extern "system" fn RegQueryInfoKeyA(key: Handle, class: *mut u8, clen: *mut u32, r: usize, subkeys: *mut u32, maxsub: *mut u32, maxclass: *mut u32,
                                               values: *mut u32, maxname: *mut u32, maxdata: *mut u32, sd: *mut u32, time: *mut u64) -> u32 {
    if !class.is_null() && !clen.is_null() && unsafe { *clen } > 0 {
        unsafe { *class = 0 };
    }
    unsafe { RegQueryInfoKeyW(key, core::ptr::null_mut(), clen, r, subkeys, maxsub, maxclass, values, maxname, maxdata, sd, time) }
}

// -----| not for this store |-----

#[no_mangle]
pub extern "system" fn RegConnectRegistryW(_machine: *const u16, _key: Handle, _out: *mut Handle) -> u32 {
    ERROR_ACCESS_DENIED
}

#[no_mangle]
pub extern "system" fn RegLoadKeyW(_key: Handle, _sub: *const u16, _file: *const u16) -> u32 {
    ERROR_ACCESS_DENIED
}

#[no_mangle]
pub extern "system" fn RegUnLoadKeyW(_key: Handle, _sub: *const u16) -> u32 {
    ERROR_ACCESS_DENIED
}

#[no_mangle]
pub extern "system" fn RegSaveKeyW(_key: Handle, _file: *const u16, _sa: usize) -> u32 {
    ERROR_ACCESS_DENIED
}

#[no_mangle]
pub extern "system" fn RegRestoreKeyW(_key: Handle, _file: *const u16, _flags: u32) -> u32 {
    ERROR_ACCESS_DENIED
}

#[no_mangle]
pub extern "system" fn RegSetKeySecurity(_key: Handle, _info: u32, _sd: *const u8) -> u32 {
    ERROR_SUCCESS
}

/// Nobody's told of changes yet; the call succeeds, the event never fires.
#[no_mangle]
pub extern "system" fn RegNotifyChangeKeyValue(_key: Handle, _sub: crate::Bool, _filter: u32, _event: Handle, _async: crate::Bool) -> u32 {
    ERROR_SUCCESS
}
