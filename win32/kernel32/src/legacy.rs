//! What older and system programs (regedit, iexpress) ask for beyond the
//! calls that do the work: ANSI file and string functions, one-time
//! initialization, path helpers, and security descriptors (kept as blank
//! blocks: files have no ACLs).

use core::sync::atomic::{AtomicU64, Ordering};

use crate::util::{widen, wstr};
use crate::{Bool, Handle};

const ERROR_INSUFFICIENT_BUFFER: u32 = 122;
const ERROR_NOT_SUPPORTED: u32 = 50;
const ERROR_INVALID_PARAMETER: u32 = 87;
const ERROR_NONE_MAPPED: u32 = 1332;

// -----| ANSI strings |-----

#[no_mangle]
pub unsafe extern "system" fn lstrcmpA(a: *const u8, b: *const u8) -> i32 {
    let (wa, wb) = unsafe { (widen(a), widen(b)) };
    unsafe { crate::locale::lstrcmpW(wa.as_ptr(), wb.as_ptr()) }
}

#[no_mangle]
pub unsafe extern "system" fn lstrcmpiA(a: *const u8, b: *const u8) -> i32 {
    let (wa, wb) = unsafe { (widen(a), widen(b)) };
    unsafe { crate::locale::lstrcmpiW(wa.as_ptr(), wb.as_ptr()) }
}

/// Bytes as UTF-16 code units (Latin-1), for a comparison.
fn latin(p: *const u8, n: i32, out: &mut [u16]) -> i32 {
    let mut k = 0;
    while (n < 0 || (k as i32) < n) && k < out.len() {
        let c = unsafe { *p.add(k) };
        if n < 0 && c == 0 {
            break;
        }
        out[k] = c as u16;
        k += 1;
    }
    k as i32
}

#[no_mangle]
pub unsafe extern "system" fn CompareStringA(lcid: u32, flags: u32, a: *const u8, alen: i32, b: *const u8, blen: i32) -> i32 {
    let (mut wa, mut wb) = ([0u16; 2048], [0u16; 2048]);
    let (na, nb) = (latin(a, alen, &mut wa), latin(b, blen, &mut wb));
    unsafe { crate::locale::CompareStringW(lcid, flags, wa.as_ptr(), na, wb.as_ptr(), nb) }
}

#[no_mangle]
pub extern "system" fn IsDBCSLeadByte(_c: u8) -> Bool {
    0
}

#[no_mangle]
pub extern "system" fn IsDBCSLeadByteEx(_cp: u32, _c: u8) -> Bool {
    0
}

// -----| old file calls |-----

#[no_mangle]
pub unsafe extern "system" fn _lread(h: Handle, buf: *mut u8, n: u32) -> u32 {
    let mut done = 0u32;
    if unsafe { crate::ReadFile(h, buf, n, &mut done, 0) } != 0 { done } else { u32::MAX }
}

#[no_mangle]
pub unsafe extern "system" fn _lwrite(h: Handle, buf: *const u8, n: u32) -> u32 {
    let mut done = 0u32;
    if unsafe { crate::WriteFile(h, buf, n, &mut done, 0) } != 0 { done } else { u32::MAX }
}

#[no_mangle]
pub unsafe extern "system" fn _llseek(h: Handle, off: i32, origin: i32) -> i32 {
    unsafe { crate::file::SetFilePointer(h, off, core::ptr::null_mut(), origin as u32) as i32 }
}

#[no_mangle]
pub extern "system" fn _lclose(h: Handle) -> Handle {
    if crate::CloseHandle(h) != 0 { 0 } else { u64::MAX }
}

/// There are no 8.3 names to shorten to: the path as it is.
#[no_mangle]
pub unsafe extern "system" fn GetShortPathNameW(long: *const u16, out: *mut u16, cap: u32) -> u32 {
    unsafe { copy_path(wstr(long), out, cap) }
}

#[no_mangle]
pub unsafe extern "system" fn GetLongPathNameW(short: *const u16, out: *mut u16, cap: u32) -> u32 {
    unsafe { copy_path(wstr(short), out, cap) }
}

/// The length without the NUL if it fit, else the size needed with it.
unsafe fn copy_path(s: &[u16], out: *mut u16, cap: u32) -> u32 {
    if s.len() as u32 >= cap || out.is_null() {
        return s.len() as u32 + 1;
    }
    unsafe {
        core::ptr::copy(s.as_ptr(), out, s.len());
        *out.add(s.len()) = 0;
    }
    s.len() as u32
}

#[no_mangle]
pub unsafe extern "system" fn GetShortPathNameA(long: *const u8, out: *mut u8, cap: u32) -> u32 {
    if long.is_null() {
        crate::SetLastError(ERROR_INVALID_PARAMETER);
        return 0;
    }
    let mut n = 0;
    while unsafe { *long.add(n) } != 0 {
        n += 1;
    }
    if n as u32 >= cap || out.is_null() {
        return n as u32 + 1;
    }
    unsafe {
        core::ptr::copy(long, out, n);
        *out.add(n) = 0;
    }
    n as u32
}

static TEMP_NAME: AtomicU64 = AtomicU64::new(0x1000);

/// `dir\preXXXX.TMP`, created empty unless `unique` is given.
#[no_mangle]
pub unsafe extern "system" fn GetTempFileNameA(dir: *const u8, prefix: *const u8, unique: u32, out: *mut u8) -> u32 {
    if dir.is_null() || out.is_null() {
        crate::SetLastError(ERROR_INVALID_PARAMETER);
        return 0;
    }
    let n = if unique != 0 { unique & 0xFFFF } else { (TEMP_NAME.fetch_add(1, Ordering::Relaxed) & 0xFFFF) as u32 };
    let mut at = 0;
    unsafe {
        let mut p = dir;
        while *p != 0 {
            *out.add(at) = *p;
            at += 1;
            p = p.add(1);
        }
        if at > 0 && *out.add(at - 1) != b'\\' {
            *out.add(at) = b'\\';
            at += 1;
        }
        let mut k = 0;
        while !prefix.is_null() && k < 3 && *prefix.add(k) != 0 {
            *out.add(at) = *prefix.add(k);
            at += 1;
            k += 1;
        }
        for shift in [12, 8, 4, 0] {
            *out.add(at) = b"0123456789ABCDEF"[((n >> shift) & 15) as usize];
            at += 1;
        }
        for &c in b".TMP\0" {
            *out.add(at) = c;
            at += 1;
        }
        if unique == 0 {
            let w = widen(out);
            let h = crate::file::CreateFileW(w.as_ptr(), 0x4000_0000, 0, 0, 2, 0x80, 0);
            if h != u64::MAX {
                crate::CloseHandle(h);
            }
        }
    }
    n
}

// -----| resources |-----

#[no_mangle]
pub unsafe extern "system" fn FindResourceExA(module: Handle, kind: *const u8, name: *const u8, _lang: u16) -> Handle {
    unsafe { crate::resource::FindResourceA(module, name, kind) }
}

/// Listing a module's resources isn't there yet: an empty list.
#[no_mangle]
pub extern "system" fn EnumResourceTypesA(_module: Handle, _f: usize, _lp: isize) -> Bool {
    1
}

#[no_mangle]
pub extern "system" fn EnumResourceNamesA(_module: Handle, _kind: *const u8, _f: usize, _lp: isize) -> Bool {
    1
}

#[no_mangle]
pub extern "system" fn EnumResourceLanguagesA(_module: Handle, _kind: *const u8, _name: *const u8, _f: usize, _lp: isize) -> Bool {
    1
}

// -----| one-time initialization, begin and complete |-----

const INIT_ONCE_CHECK_ONLY: u32 = 1;
const INIT_ONCE_ASYNC: u32 = 2;
const INIT_ONCE_INIT_FAILED: u32 = 4;
// The block: 0 untouched, 1 being done, else done (the context with bit 1).
const BUSY: u64 = 1;
const DONE: u64 = 2;

#[no_mangle]
pub unsafe extern "system" fn InitOnceBeginInitialize(once: *mut u64, flags: u32, pending: *mut Bool, context: *mut usize) -> Bool {
    let a = unsafe { &*(once as *const AtomicU64) };
    loop {
        let v = a.load(Ordering::Acquire);
        if v & DONE != 0 {
            unsafe {
                *pending = 0;
                if !context.is_null() {
                    *context = (v & !3) as usize;
                }
            }
            return 1;
        }
        if flags & INIT_ONCE_CHECK_ONLY != 0 {
            crate::SetLastError(ERROR_INVALID_PARAMETER);
            return 0;
        }
        if v == 0 && a.compare_exchange(0, BUSY, Ordering::AcqRel, Ordering::Acquire).is_ok() {
            unsafe { *pending = 1 };
            return 1;
        }
        if flags & INIT_ONCE_ASYNC != 0 && v == BUSY {
            unsafe { *pending = 1 };
            return 1;
        }
        core::hint::spin_loop();
    }
}

#[no_mangle]
pub unsafe extern "system" fn InitOnceComplete(once: *mut u64, flags: u32, context: usize) -> Bool {
    let a = unsafe { &*(once as *const AtomicU64) };
    if flags & INIT_ONCE_INIT_FAILED != 0 {
        a.store(0, Ordering::Release);
    } else {
        a.store((context as u64 & !3) | DONE, Ordering::Release);
    }
    1
}

// -----| names |-----

#[no_mangle]
pub unsafe extern "system" fn GetComputerNameW(out: *mut u16, size: *mut u32) -> Bool {
    let name: [u16; 6] = [b'V' as u16, b'E' as u16, b'I' as u16, b'L' as u16, b'O' as u16, b'S' as u16];
    if size.is_null() {
        crate::SetLastError(ERROR_INVALID_PARAMETER);
        return 0;
    }
    unsafe {
        if out.is_null() || (*size as usize) <= name.len() {
            *size = name.len() as u32 + 1;
            crate::SetLastError(111); // ERROR_BUFFER_OVERFLOW
            return 0;
        }
        core::ptr::copy_nonoverlapping(name.as_ptr(), out, name.len());
        *out.add(name.len()) = 0;
        *size = name.len() as u32;
    }
    1
}

// -----| paths and strings (shlwapi, pathcch) |-----

const S_OK: i32 = 0;
const E_INVALIDARG: i32 = 0x8007_0057u32 as i32;
const E_NOINTERFACE: i32 = 0x8000_4002u32 as i32;
const BUFFER_TOO_SMALL: i32 = 0x8007_007Au32 as i32;
const S_FALSE: i32 = 1;

#[no_mangle]
pub unsafe extern "system" fn PathCchAddBackslash(path: *mut u16, cap: usize) -> i32 {
    let s = unsafe { wstr(path) };
    if s.last() == Some(&(b'\\' as u16)) {
        return S_FALSE;
    }
    if s.len() + 2 > cap {
        return BUFFER_TOO_SMALL;
    }
    let n = s.len();
    unsafe {
        *path.add(n) = b'\\' as u16;
        *path.add(n + 1) = 0;
    }
    S_OK
}

#[no_mangle]
pub unsafe extern "system" fn PathCchCombine(out: *mut u16, cap: usize, a: *const u16, b: *const u16) -> i32 {
    if out.is_null() || cap == 0 {
        return E_INVALIDARG;
    }
    let (sa, sb) = unsafe { (wstr(a), wstr(b)) };
    let absolute = sb.first() == Some(&(b'\\' as u16)) || (sb.len() >= 2 && sb[1] == b':' as u16);
    let parts: [&[u16]; 2] = if absolute { [&[], sb] } else { [sa, sb] };
    let sep = !absolute && !sa.is_empty() && !sb.is_empty() && sa.last() != Some(&(b'\\' as u16));
    let total = parts[0].len() + sep as usize + parts[1].len();
    if total + 1 > cap {
        return BUFFER_TOO_SMALL;
    }
    unsafe {
        core::ptr::copy(parts[0].as_ptr(), out, parts[0].len());
        let mut at = parts[0].len();
        if sep {
            *out.add(at) = b'\\' as u16;
            at += 1;
        }
        core::ptr::copy(parts[1].as_ptr(), out.add(at), parts[1].len());
        *out.add(total) = 0;
    }
    S_OK
}

fn fold(c: u16) -> u16 {
    if (b'a' as u16..=b'z' as u16).contains(&c) { c - 32 } else { c }
}

#[no_mangle]
pub unsafe extern "system" fn StrChrW(s: *const u16, c: u16) -> *const u16 {
    let t = unsafe { wstr(s) };
    t.iter().position(|&x| x == c).map_or(core::ptr::null(), |i| unsafe { s.add(i) })
}

#[no_mangle]
pub unsafe extern "system" fn StrChrIW(s: *const u16, c: u16) -> *const u16 {
    let t = unsafe { wstr(s) };
    t.iter().position(|&x| fold(x) == fold(c)).map_or(core::ptr::null(), |i| unsafe { s.add(i) })
}

/// The last `c` before `end` (the terminator when NULL).
#[no_mangle]
pub unsafe extern "system" fn StrRChrW(s: *const u16, end: *const u16, c: u16) -> *const u16 {
    let t = unsafe { wstr(s) };
    let n = if end.is_null() { t.len() } else { unsafe { end.offset_from(s) }.max(0) as usize };
    t[..n.min(t.len())].iter().rposition(|&x| x == c).map_or(core::ptr::null(), |i| unsafe { s.add(i) })
}

#[no_mangle]
pub unsafe extern "system" fn StrToIntW(s: *const u16) -> i32 {
    let t = unsafe { wstr(s) };
    let (neg, rest) = if t.first() == Some(&(b'-' as u16)) { (true, &t[1..]) } else { (false, t) };
    let n = rest.iter().take_while(|c| (b'0' as u16..=b'9' as u16).contains(c)).fold(0i32, |n, &c| n.wrapping_mul(10).wrapping_add((c - b'0' as u16) as i32));
    if neg { n.wrapping_neg() } else { n }
}

/// IUnknown::QueryInterface from a table of { IID*, offset } entries.
#[no_mangle]
pub unsafe extern "system" fn QISearch(this: *mut u8, table: *const [usize; 2], iid: *const [u8; 16], out: *mut *mut u8) -> i32 {
    if out.is_null() {
        return E_INVALIDARG;
    }
    unsafe { *out = core::ptr::null_mut() };
    if table.is_null() || iid.is_null() {
        return E_NOINTERFACE;
    }
    let want = unsafe { *iid };
    let mut e = table;
    unsafe {
        while (*e)[0] != 0 {
            let entry_iid = *((*e)[0] as *const [u8; 16]);
            if entry_iid == want {
                let obj = this.add((*e)[1]);
                // AddRef, the vtable's second entry.
                let vtable = *(obj as *const *const usize);
                let add_ref: unsafe extern "system" fn(*mut u8) -> u32 = core::mem::transmute(*vtable.add(1));
                add_ref(obj);
                *out = obj;
                return S_OK;
            }
            e = e.add(1);
        }
    }
    // Every object is its first interface's IUnknown.
    const IUNKNOWN: [u8; 16] = [0, 0, 0, 0, 0, 0, 0, 0, 0xC0, 0, 0, 0, 0, 0, 0, 0x46];
    unsafe {
        if want == IUNKNOWN && (*table)[0] != 0 {
            let obj = this.add((*table)[1]);
            let vtable = *(obj as *const *const usize);
            let add_ref: unsafe extern "system" fn(*mut u8) -> u32 = core::mem::transmute(*vtable.add(1));
            add_ref(obj);
            *out = obj;
            return S_OK;
        }
    }
    E_NOINTERFACE
}

/// ERROR_NO_MORE_ITEMS (no subkeys).
// -----| security: blank descriptors, no ACLs |-----

#[no_mangle]
pub unsafe extern "system" fn InitializeSecurityDescriptor(sd: *mut u8, rev: u32) -> Bool {
    if sd.is_null() || rev != 1 {
        crate::SetLastError(ERROR_INVALID_PARAMETER);
        return 0;
    }
    unsafe {
        core::ptr::write_bytes(sd, 0, 40);
        *sd = 1;
    }
    1
}

#[no_mangle]
pub extern "system" fn IsValidSecurityDescriptor(sd: *const u8) -> Bool {
    (!sd.is_null()) as Bool
}

#[no_mangle]
pub unsafe extern "system" fn GetSecurityDescriptorControl(_sd: *const u8, control: *mut u16, rev: *mut u32) -> Bool {
    unsafe {
        if !control.is_null() {
            *control = 0;
        }
        if !rev.is_null() {
            *rev = 1;
        }
    }
    1
}

/// (present, dacl, defaulted): no list.
#[no_mangle]
pub unsafe extern "system" fn GetSecurityDescriptorDacl(_sd: *const u8, present: *mut Bool, acl: *mut usize, defaulted: *mut Bool) -> Bool {
    unsafe {
        if !present.is_null() {
            *present = 0;
        }
        if !acl.is_null() {
            *acl = 0;
        }
        if !defaulted.is_null() {
            *defaulted = 0;
        }
    }
    1
}

#[no_mangle]
pub unsafe extern "system" fn GetSecurityDescriptorSacl(sd: *const u8, present: *mut Bool, acl: *mut usize, defaulted: *mut Bool) -> Bool {
    unsafe { GetSecurityDescriptorDacl(sd, present, acl, defaulted) }
}

#[no_mangle]
pub unsafe extern "system" fn GetSecurityDescriptorGroup(_sd: *const u8, sid: *mut usize, defaulted: *mut Bool) -> Bool {
    unsafe {
        if !sid.is_null() {
            *sid = 0;
        }
        if !defaulted.is_null() {
            *defaulted = 0;
        }
    }
    1
}

#[no_mangle]
pub extern "system" fn SetSecurityDescriptorDacl(_sd: *mut u8, _present: Bool, _acl: *const u8, _defaulted: Bool) -> Bool {
    1
}

#[no_mangle]
pub extern "system" fn SetSecurityDescriptorSacl(_sd: *mut u8, _present: Bool, _acl: *const u8, _defaulted: Bool) -> Bool {
    1
}

#[no_mangle]
pub extern "system" fn SetSecurityDescriptorOwner(_sd: *mut u8, _sid: *const u8, _defaulted: Bool) -> Bool {
    1
}

#[no_mangle]
pub extern "system" fn SetSecurityDescriptorGroup(_sd: *mut u8, _sid: *const u8, _defaulted: Bool) -> Bool {
    1
}

#[no_mangle]
pub unsafe extern "system" fn InitializeAcl(acl: *mut u8, len: u32, rev: u32) -> Bool {
    if acl.is_null() || len < 8 {
        crate::SetLastError(ERROR_INSUFFICIENT_BUFFER);
        return 0;
    }
    unsafe {
        core::ptr::write_bytes(acl, 0, 8);
        *acl = rev as u8;
        core::ptr::write_unaligned(acl.add(2) as *mut u16, len as u16);
    }
    1
}

#[no_mangle]
pub unsafe extern "system" fn MapGenericMask(mask: *mut u32, map: *const [u32; 4]) {
    if mask.is_null() || map.is_null() {
        return;
    }
    unsafe {
        let m = *map;
        let mut v = *mask;
        for (bit, to) in [(0x8000_0000u32, m[0]), (0x4000_0000, m[1]), (0x2000_0000, m[2]), (0x1000_0000, m[3])] {
            if v & bit != 0 {
                v = (v & !bit) | to;
            }
        }
        *mask = v;
    }
}

/// A SID: revision, count, authority (6), subauthorities.
#[no_mangle]
pub unsafe extern "system" fn GetSidSubAuthorityCount(sid: *mut u8) -> *mut u8 {
    unsafe { sid.add(1) }
}

#[no_mangle]
pub unsafe extern "system" fn GetSidSubAuthority(sid: *mut u8, i: u32) -> *mut u32 {
    unsafe { sid.add(8 + i as usize * 4) as *mut u32 }
}

#[no_mangle]
pub extern "system" fn AdjustTokenPrivileges(_token: Handle, _off: Bool, _new: *const u8, _len: u32, _prev: *mut u8, _ret: *mut u32) -> Bool {
    crate::SetLastError(1300); // ERROR_NOT_ALL_ASSIGNED
    1
}

#[no_mangle]
pub extern "system" fn LookupPrivilegeValueW(_system: *const u16, _name: *const u16, luid: *mut u64) -> Bool {
    if !luid.is_null() {
        unsafe { *luid = 0 };
    }
    crate::SetLastError(1313); // ERROR_NO_SUCH_PRIVILEGE
    0
}

#[no_mangle]
pub extern "system" fn LookupAccountSidW(_system: *const u16, _sid: *const u8, _name: *mut u16, _n: *mut u32, _dom: *mut u16, _d: *mut u32, _use: *mut u32) -> Bool {
    crate::SetLastError(ERROR_NONE_MAPPED);
    0
}

#[no_mangle]
pub extern "system" fn SetNamedSecurityInfoW(_name: *const u16, _kind: u32, _info: u32, _o: *const u8, _g: *const u8, _d: *const u8, _s: *const u8) -> u32 {
    ERROR_NOT_SUPPORTED
}

#[no_mangle]
pub extern "system" fn GetSecurityInfo(_h: Handle, _kind: u32, _info: u32, _o: *mut usize, _g: *mut usize, _d: *mut usize, _s: *mut usize, _sd: *mut usize) -> u32 {
    ERROR_NOT_SUPPORTED
}

#[no_mangle]
pub extern "system" fn SetSecurityInfo(_h: Handle, _kind: u32, _info: u32, _o: *const u8, _g: *const u8, _d: *const u8, _s: *const u8) -> u32 {
    ERROR_NOT_SUPPORTED
}

