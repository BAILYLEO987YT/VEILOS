//! What the MSYS2/Cygwin runtime (Git Bash's msys-2.0.dll) takes from ntdll
//! and can be answered in user mode: SIDs, ACLs and security descriptors
//! (built and read here; nothing enforces them), counted Unicode strings,
//! critical sections (the same as kernel32's), and the token, job, time and
//! file calls VEILOS answers simply.

use core::ptr::{null_mut, read_unaligned, write_unaligned};
use core::sync::atomic::{AtomicU64, Ordering};

use nt_abi::{layout, status, NtStatus};

use crate::rtl::UnicodeString;

const STATUS_BUFFER_TOO_SMALL: NtStatus = 0xC000_0023u32 as i32;
const STATUS_UNKNOWN_REVISION: NtStatus = 0xC000_0058u32 as i32;
const STATUS_ALLOTTED_SPACE_EXCEEDED: NtStatus = 0xC000_0099u32 as i32;
const STATUS_BAD_DESCRIPTOR_FORMAT: NtStatus = 0xC000_00E7u32 as i32;
const STATUS_INVALID_ACL: NtStatus = 0xC000_0077u32 as i32;
const STATUS_NO_MEMORY: NtStatus = 0xC000_0017u32 as i32;
const STATUS_NOT_SUPPORTED: NtStatus = 0xC000_00BBu32 as i32;
const STATUS_EAS_NOT_SUPPORTED: NtStatus = 0xC000_004Fu32 as i32;
const STATUS_NO_MORE_ENTRIES: NtStatus = 0x8000_001Au32 as i32;

fn heap() -> *mut crate::heap::Heap {
    let peb: u64;
    unsafe {
        core::arch::asm!("mov {}, gs:[0x60]", out(reg) peb, options(nostack, readonly));
        read_unaligned((peb + 0x30) as *const u64) as *mut crate::heap::Heap
    }
}

fn alloc(n: usize) -> *mut u8 {
    unsafe { crate::RtlAllocateHeap(heap(), 0, n) }
}

// -----| SIDs |-----
//
// Revision, SubAuthorityCount, IdentifierAuthority[6], SubAuthority[count].

unsafe fn sid_len(sid: *const u8) -> usize {
    8 + 4 * unsafe { *sid.add(1) } as usize
}

#[no_mangle]
pub unsafe extern "system" fn RtlLengthSid(sid: *const u8) -> u32 {
    unsafe { sid_len(sid) as u32 }
}

#[no_mangle]
pub unsafe extern "system" fn RtlCopySid(len: u32, dst: *mut u8, src: *const u8) -> NtStatus {
    let n = unsafe { sid_len(src) };
    if (len as usize) < n {
        return STATUS_BUFFER_TOO_SMALL;
    }
    unsafe { core::ptr::copy(src, dst, n) };
    status::SUCCESS
}

#[no_mangle]
pub unsafe extern "system" fn RtlEqualSid(a: *const u8, b: *const u8) -> u8 {
    let (n, m) = unsafe { (sid_len(a), sid_len(b)) };
    (n == m && unsafe { core::slice::from_raw_parts(a, n) == core::slice::from_raw_parts(b, m) }) as u8
}

/// Equal but for the last sub-authority.
#[no_mangle]
pub unsafe extern "system" fn RtlEqualPrefixSid(a: *const u8, b: *const u8) -> u8 {
    unsafe {
        if *a != *b || *a.add(1) != *b.add(1) {
            return 0;
        }
        let n = sid_len(a) - if *a.add(1) > 0 { 4 } else { 0 };
        (core::slice::from_raw_parts(a, n) == core::slice::from_raw_parts(b, n)) as u8
    }
}

#[no_mangle]
pub unsafe extern "system" fn RtlInitializeSid(sid: *mut u8, authority: *const u8, count: u8) -> NtStatus {
    unsafe {
        *sid = 1;
        *sid.add(1) = count;
        core::ptr::copy(authority, sid.add(2), 6);
    }
    status::SUCCESS
}

#[no_mangle]
pub unsafe extern "system" fn RtlSubAuthoritySid(sid: *mut u8, i: u32) -> *mut u32 {
    unsafe { sid.add(8 + 4 * i as usize) as *mut u32 }
}

#[no_mangle]
pub unsafe extern "system" fn RtlSubAuthorityCountSid(sid: *mut u8) -> *mut u8 {
    unsafe { sid.add(1) }
}

#[no_mangle]
pub unsafe extern "system" fn RtlIdentifierAuthoritySid(sid: *mut u8) -> *mut u8 {
    unsafe { sid.add(2) }
}

/// "S-1-5-21-..." (the authority in hex when it doesn't fit 32 bits).
#[no_mangle]
pub unsafe extern "system" fn RtlConvertSidToUnicodeString(us: *mut UnicodeString, sid: *const u8, allocate: u8) -> NtStatus {
    let mut text = [0u16; 192];
    let mut n = 0usize;
    let mut put = |s: &[u8]| {
        for &b in s {
            if n < text.len() {
                text[n] = b as u16;
                n += 1;
            }
        }
    };
    let mut num = [0u8; 24];
    fn dec(mut v: u64, out: &mut [u8; 24]) -> &[u8] {
        let mut i = out.len();
        loop {
            i -= 1;
            out[i] = b'0' + (v % 10) as u8;
            v /= 10;
            if v == 0 {
                break;
            }
        }
        &out[i..]
    }
    unsafe {
        put(b"S-");
        put(dec(*sid as u64, &mut num));
        put(b"-");
        let a = core::slice::from_raw_parts(sid.add(2), 6);
        let auth = a.iter().fold(0u64, |v, &b| v << 8 | b as u64);
        if auth >> 32 == 0 {
            put(dec(auth, &mut num));
        } else {
            put(b"0x");
            for &b in a {
                let hex = b"0123456789ABCDEF";
                put(&[hex[(b >> 4) as usize], hex[(b & 15) as usize]]);
            }
        }
        for i in 0..*sid.add(1) as usize {
            put(b"-");
            put(dec(read_unaligned(sid.add(8 + 4 * i) as *const u32) as u64, &mut num));
        }
    }
    unsafe { set_string(us, &text[..n], allocate != 0) }
}

/// Put `units` in `us`: a fresh heap buffer, or the one it has.
unsafe fn set_string(us: *mut UnicodeString, units: &[u16], allocate: bool) -> NtStatus {
    let bytes = units.len() * 2;
    unsafe {
        if allocate {
            let buf = alloc(bytes + 2) as *mut u16;
            if buf.is_null() {
                return STATUS_NO_MEMORY;
            }
            (*us).buffer = buf;
            (*us).maximum_length = (bytes + 2) as u16;
        } else if ((*us).maximum_length as usize) < bytes {
            return STATUS_BUFFER_TOO_SMALL;
        }
        core::ptr::copy_nonoverlapping(units.as_ptr(), (*us).buffer, units.len());
        if (*us).maximum_length as usize >= bytes + 2 {
            *(*us).buffer.add(units.len()) = 0;
        }
        (*us).length = bytes as u16;
    }
    status::SUCCESS
}

// -----| ACLs |-----
//
// ACL: AclRevision, Sbz1, AclSize u16, AceCount u16, Sbz2 u16; then ACEs:
// AceType, AceFlags, AceSize u16, Mask u32, then a SID.

unsafe fn acl_size(acl: *const u8) -> usize {
    unsafe { read_unaligned(acl.add(2) as *const u16) as usize }
}

unsafe fn ace_count(acl: *const u8) -> usize {
    unsafe { read_unaligned(acl.add(4) as *const u16) as usize }
}

/// Where ACE `index` starts (index == count: where a new one goes).
unsafe fn ace_at(acl: *const u8, index: usize) -> Option<usize> {
    let mut off = 8usize;
    for _ in 0..index {
        let size = unsafe { read_unaligned(acl.add(off + 2) as *const u16) } as usize;
        if size < 4 {
            return None;
        }
        off += size;
    }
    (off <= unsafe { acl_size(acl) }).then_some(off)
}

#[no_mangle]
pub unsafe extern "system" fn RtlCreateAcl(acl: *mut u8, len: u32, revision: u32) -> NtStatus {
    if len < 8 || !(2..=4).contains(&revision) {
        return if len < 8 { STATUS_BUFFER_TOO_SMALL } else { STATUS_INVALID_ACL };
    }
    unsafe {
        core::ptr::write_bytes(acl, 0, 8);
        *acl = revision as u8;
        write_unaligned(acl.add(2) as *mut u16, (len & !3) as u16);
    }
    status::SUCCESS
}

unsafe fn add_ace(acl: *mut u8, kind: u8, flags: u32, mask: u32, sid: *const u8) -> NtStatus {
    let Some(off) = (unsafe { ace_at(acl, ace_count(acl)) }) else { return STATUS_INVALID_ACL };
    let size = 8 + unsafe { sid_len(sid) };
    if off + size > unsafe { acl_size(acl) } {
        return STATUS_ALLOTTED_SPACE_EXCEEDED;
    }
    unsafe {
        let ace = acl.add(off);
        *ace = kind;
        *ace.add(1) = flags as u8;
        write_unaligned(ace.add(2) as *mut u16, size as u16);
        write_unaligned(ace.add(4) as *mut u32, mask);
        core::ptr::copy(sid, ace.add(8), size - 8);
        write_unaligned(acl.add(4) as *mut u16, (ace_count(acl) + 1) as u16);
    }
    status::SUCCESS
}

#[no_mangle]
pub unsafe extern "system" fn RtlAddAccessAllowedAce(acl: *mut u8, _revision: u32, mask: u32, sid: *const u8) -> NtStatus {
    unsafe { add_ace(acl, 0, 0, mask, sid) }
}

#[no_mangle]
pub unsafe extern "system" fn RtlAddAccessAllowedAceEx(acl: *mut u8, _revision: u32, flags: u32, mask: u32, sid: *const u8) -> NtStatus {
    unsafe { add_ace(acl, 0, flags, mask, sid) }
}

#[no_mangle]
pub unsafe extern "system" fn RtlAddAccessDeniedAceEx(acl: *mut u8, _revision: u32, flags: u32, mask: u32, sid: *const u8) -> NtStatus {
    unsafe { add_ace(acl, 1, flags, mask, sid) }
}

#[no_mangle]
pub unsafe extern "system" fn RtlGetAce(acl: *mut u8, index: u32, ace: *mut *mut u8) -> NtStatus {
    if index as usize >= unsafe { ace_count(acl) } {
        return status::INVALID_PARAMETER;
    }
    match unsafe { ace_at(acl, index as usize) } {
        Some(off) => {
            unsafe { *ace = acl.add(off) };
            status::SUCCESS
        }
        None => STATUS_INVALID_ACL,
    }
}

/// TRUE, with where the next ACE would go (NULL if the ACL is full).
#[no_mangle]
pub unsafe extern "system" fn RtlFirstFreeAce(acl: *mut u8, ace: *mut *mut u8) -> u8 {
    match unsafe { ace_at(acl, ace_count(acl)) } {
        Some(off) => {
            unsafe { *ace = if off < acl_size(acl) { acl.add(off) } else { null_mut() } };
            1
        }
        None => 0,
    }
}

// -----| security descriptors |-----
//
// Absolute: Revision, Sbz1, Control u16, then pointers to the owner, group,
// SACL and DACL. Self-relative (Control has SE_SELF_RELATIVE): 32-bit
// offsets from its start instead.

const SE_OWNER_DEFAULTED: u16 = 0x1;
const SE_GROUP_DEFAULTED: u16 = 0x2;
const SE_DACL_PRESENT: u16 = 0x4;
const SE_DACL_DEFAULTED: u16 = 0x8;
const SE_SACL_PRESENT: u16 = 0x10;
const SE_SELF_RELATIVE: u16 = 0x8000;

unsafe fn control(sd: *const u8) -> u16 {
    unsafe { read_unaligned(sd.add(2) as *const u16) }
}

/// Field 0 owner, 1 group, 2 SACL, 3 DACL.
unsafe fn sd_field(sd: *const u8, i: usize) -> *mut u8 {
    unsafe {
        if control(sd) & SE_SELF_RELATIVE != 0 {
            let off = read_unaligned(sd.add(4 + 4 * i) as *const u32);
            if off == 0 { null_mut() } else { sd.add(off as usize) as *mut u8 }
        } else {
            read_unaligned(sd.add(8 + 8 * i) as *const *mut u8)
        }
    }
}

unsafe fn set_sd_field(sd: *mut u8, i: usize, p: *const u8, flag: u16, on: bool) -> NtStatus {
    unsafe {
        if control(sd) & SE_SELF_RELATIVE != 0 {
            return STATUS_BAD_DESCRIPTOR_FORMAT;
        }
        write_unaligned(sd.add(8 + 8 * i) as *mut *const u8, p);
        let c = control(sd);
        write_unaligned(sd.add(2) as *mut u16, if on { c | flag } else { c & !flag });
    }
    status::SUCCESS
}

#[no_mangle]
pub unsafe extern "system" fn RtlCreateSecurityDescriptor(sd: *mut u8, revision: u32) -> NtStatus {
    if revision != 1 {
        return STATUS_UNKNOWN_REVISION;
    }
    unsafe {
        core::ptr::write_bytes(sd, 0, 40);
        *sd = 1;
    }
    status::SUCCESS
}

#[no_mangle]
pub unsafe extern "system" fn RtlSetOwnerSecurityDescriptor(sd: *mut u8, owner: *const u8, defaulted: u8) -> NtStatus {
    unsafe { set_sd_field(sd, 0, owner, SE_OWNER_DEFAULTED, defaulted != 0) }
}

#[no_mangle]
pub unsafe extern "system" fn RtlSetGroupSecurityDescriptor(sd: *mut u8, group: *const u8, defaulted: u8) -> NtStatus {
    unsafe { set_sd_field(sd, 1, group, SE_GROUP_DEFAULTED, defaulted != 0) }
}

#[no_mangle]
pub unsafe extern "system" fn RtlSetDaclSecurityDescriptor(sd: *mut u8, present: u8, dacl: *const u8, defaulted: u8) -> NtStatus {
    unsafe {
        let s = set_sd_field(sd, 3, if present != 0 { dacl } else { core::ptr::null() }, SE_DACL_PRESENT, present != 0);
        if s != status::SUCCESS {
            return s;
        }
        let c = control(sd);
        write_unaligned(sd.add(2) as *mut u16, if defaulted != 0 && present != 0 { c | SE_DACL_DEFAULTED } else { c & !SE_DACL_DEFAULTED });
    }
    status::SUCCESS
}

#[no_mangle]
pub unsafe extern "system" fn RtlGetOwnerSecurityDescriptor(sd: *const u8, owner: *mut *mut u8, defaulted: *mut u8) -> NtStatus {
    unsafe {
        *owner = sd_field(sd, 0);
        *defaulted = (control(sd) & SE_OWNER_DEFAULTED != 0) as u8;
    }
    status::SUCCESS
}

#[no_mangle]
pub unsafe extern "system" fn RtlGetGroupSecurityDescriptor(sd: *const u8, group: *mut *mut u8, defaulted: *mut u8) -> NtStatus {
    unsafe {
        *group = sd_field(sd, 1);
        *defaulted = (control(sd) & SE_GROUP_DEFAULTED != 0) as u8;
    }
    status::SUCCESS
}

#[no_mangle]
pub unsafe extern "system" fn RtlGetDaclSecurityDescriptor(sd: *const u8, present: *mut u8, dacl: *mut *mut u8, defaulted: *mut u8) -> NtStatus {
    unsafe {
        let c = control(sd);
        *present = (c & SE_DACL_PRESENT != 0) as u8;
        if c & SE_DACL_PRESENT != 0 {
            *dacl = sd_field(sd, 3);
            *defaulted = (c & SE_DACL_DEFAULTED != 0) as u8;
        }
    }
    status::SUCCESS
}

#[no_mangle]
pub unsafe extern "system" fn RtlGetControlSecurityDescriptor(sd: *const u8, ctl: *mut u16, revision: *mut u32) -> NtStatus {
    unsafe {
        *ctl = control(sd);
        *revision = *sd as u32;
    }
    status::SUCCESS
}

#[no_mangle]
pub unsafe extern "system" fn RtlSetControlSecurityDescriptor(sd: *mut u8, mask: u16, bits: u16) -> NtStatus {
    unsafe {
        let c = control(sd);
        write_unaligned(sd.add(2) as *mut u16, (c & !mask) | (bits & mask));
    }
    status::SUCCESS
}

/// The same descriptor in one block: header, then owner, group, SACL, DACL.
#[no_mangle]
pub unsafe extern "system" fn RtlAbsoluteToSelfRelativeSD(abs: *const u8, rel: *mut u8, len: *mut u32) -> NtStatus {
    unsafe {
        let c = control(abs);
        if c & SE_SELF_RELATIVE != 0 {
            return STATUS_BAD_DESCRIPTOR_FORMAT;
        }
        let parts: [*const u8; 4] = [sd_field(abs, 0), sd_field(abs, 1), sd_field(abs, 2), sd_field(abs, 3)];
        let size = |i: usize, p: *const u8| -> usize {
            if p.is_null() {
                0
            } else if i < 2 {
                sid_len(p)
            } else {
                acl_size(p)
            }
        };
        let need = 20 + (0..4).map(|i| size(i, parts[i])).sum::<usize>();
        if (*len as usize) < need {
            *len = need as u32;
            return STATUS_BUFFER_TOO_SMALL;
        }
        core::ptr::write_bytes(rel, 0, 20);
        *rel = *abs;
        write_unaligned(rel.add(2) as *mut u16, c | SE_SELF_RELATIVE);
        let mut off = 20usize;
        for (i, &p) in parts.iter().enumerate() {
            let n = size(i, p);
            if n > 0 {
                core::ptr::copy(p, rel.add(off), n);
                write_unaligned(rel.add(4 + 4 * i) as *mut u32, off as u32);
                off += n;
            }
        }
        let _ = SE_SACL_PRESENT;
    }
    status::SUCCESS
}

// -----| counted Unicode strings |-----

unsafe fn units<'a>(s: *const UnicodeString) -> &'a [u16] {
    unsafe {
        if s.is_null() || (*s).buffer.is_null() {
            &[]
        } else {
            core::slice::from_raw_parts((*s).buffer, (*s).length as usize / 2)
        }
    }
}

fn upcase(c: u16) -> u16 {
    match char::from_u32(c as u32) {
        Some(ch) => {
            let mut up = ch.to_uppercase();
            match (up.next(), up.next()) {
                (Some(u), None) if (u as u32) < 0x1_0000 => u as u16,
                _ => c,
            }
        }
        None => c,
    }
}

fn downcase(c: u16) -> u16 {
    match char::from_u32(c as u32) {
        Some(ch) => {
            let mut low = ch.to_lowercase();
            match (low.next(), low.next()) {
                (Some(l), None) if (l as u32) < 0x1_0000 => l as u16,
                _ => c,
            }
        }
        None => c,
    }
}

#[no_mangle]
pub extern "system" fn RtlUpcaseUnicodeChar(c: u16) -> u16 {
    upcase(c)
}

#[no_mangle]
pub unsafe extern "system" fn RtlCompareUnicodeString(a: *const UnicodeString, b: *const UnicodeString, ci: u8) -> i32 {
    let (x, y) = unsafe { (units(a), units(b)) };
    for (&p, &q) in x.iter().zip(y) {
        let (p, q) = if ci != 0 { (upcase(p), upcase(q)) } else { (p, q) };
        if p != q {
            return p as i32 - q as i32;
        }
    }
    x.len() as i32 - y.len() as i32
}

#[no_mangle]
pub unsafe extern "system" fn RtlEqualUnicodeString(a: *const UnicodeString, b: *const UnicodeString, ci: u8) -> u8 {
    let (x, y) = unsafe { (units(a), units(b)) };
    (x.len() == y.len() && unsafe { RtlCompareUnicodeString(a, b, ci) } == 0) as u8
}

#[no_mangle]
pub unsafe extern "system" fn RtlPrefixUnicodeString(prefix: *const UnicodeString, s: *const UnicodeString, ci: u8) -> u8 {
    let (p, x) = unsafe { (units(prefix), units(s)) };
    if p.len() > x.len() {
        return 0;
    }
    p.iter().zip(x).all(|(&a, &b)| if ci != 0 { upcase(a) == upcase(b) } else { a == b }) as u8
}

#[no_mangle]
pub unsafe extern "system" fn RtlCopyUnicodeString(dst: *mut UnicodeString, src: *const UnicodeString) {
    unsafe {
        let s = units(src);
        let n = s.len().min((*dst).maximum_length as usize / 2);
        core::ptr::copy(s.as_ptr(), (*dst).buffer, n);
        (*dst).length = (n * 2) as u16;
        if ((*dst).maximum_length as usize) >= n * 2 + 2 {
            *(*dst).buffer.add(n) = 0;
        }
    }
}

unsafe fn append(dst: *mut UnicodeString, add: &[u16]) -> NtStatus {
    unsafe {
        let have = (*dst).length as usize / 2;
        if (have + add.len()) * 2 > (*dst).maximum_length as usize {
            return STATUS_BUFFER_TOO_SMALL;
        }
        core::ptr::copy(add.as_ptr(), (*dst).buffer.add(have), add.len());
        (*dst).length = ((have + add.len()) * 2) as u16;
        if ((*dst).maximum_length as usize) >= (have + add.len()) * 2 + 2 {
            *(*dst).buffer.add(have + add.len()) = 0;
        }
    }
    status::SUCCESS
}

#[no_mangle]
pub unsafe extern "system" fn RtlAppendUnicodeStringToString(dst: *mut UnicodeString, src: *const UnicodeString) -> NtStatus {
    unsafe { append(dst, units(src)) }
}

#[no_mangle]
pub unsafe extern "system" fn RtlAppendUnicodeToString(dst: *mut UnicodeString, src: *const u16) -> NtStatus {
    if src.is_null() {
        return status::SUCCESS;
    }
    let mut n = 0;
    while unsafe { *src.add(n) } != 0 {
        n += 1;
    }
    unsafe { append(dst, core::slice::from_raw_parts(src, n)) }
}

unsafe fn map_case(dst: *mut UnicodeString, src: *const UnicodeString, allocate: u8, f: fn(u16) -> u16) -> NtStatus {
    let s = unsafe { units(src) };
    let mut buf = [0u16; 512];
    if s.len() > buf.len() {
        return STATUS_BUFFER_TOO_SMALL;
    }
    for (o, &c) in buf.iter_mut().zip(s) {
        *o = f(c);
    }
    unsafe { set_string(dst, &buf[..s.len()], allocate != 0) }
}

#[no_mangle]
pub unsafe extern "system" fn RtlUpcaseUnicodeString(dst: *mut UnicodeString, src: *const UnicodeString, allocate: u8) -> NtStatus {
    unsafe { map_case(dst, src, allocate, upcase) }
}

#[no_mangle]
pub unsafe extern "system" fn RtlDowncaseUnicodeString(dst: *mut UnicodeString, src: *const UnicodeString, allocate: u8) -> NtStatus {
    unsafe { map_case(dst, src, allocate, downcase) }
}

/// ANSI here is ASCII; anything else becomes '?'.
#[no_mangle]
pub unsafe extern "system" fn RtlUnicodeStringToAnsiString(dst: *mut UnicodeString, src: *const UnicodeString, allocate: u8) -> NtStatus {
    let s = unsafe { units(src) };
    unsafe {
        if allocate != 0 {
            let buf = alloc(s.len() + 1);
            if buf.is_null() {
                return STATUS_NO_MEMORY;
            }
            (*dst).buffer = buf as *mut u16;
            (*dst).maximum_length = (s.len() + 1) as u16;
        } else if ((*dst).maximum_length as usize) < s.len() {
            return STATUS_BUFFER_TOO_SMALL;
        }
        let out = (*dst).buffer as *mut u8;
        for (i, &c) in s.iter().enumerate() {
            *out.add(i) = if c < 0x80 { c as u8 } else { b'?' };
        }
        if (*dst).maximum_length as usize > s.len() {
            *out.add(s.len()) = 0;
        }
        (*dst).length = s.len() as u16;
    }
    status::SUCCESS
}

// -----| critical sections: kernel32's own scheme, on the same layout |-----
//
// DebugInfo (8), LockCount (4), RecursionCount (4), OwningThread (8), ...

fn thread_id() -> u64 {
    let t: u64;
    unsafe { core::arch::asm!("mov {}, gs:[0x48]", out(reg) t, options(nostack, readonly)) };
    t
}

#[no_mangle]
pub unsafe extern "system" fn RtlEnterCriticalSection(cs: *mut u8) -> NtStatus {
    let me = thread_id();
    let owner = unsafe { AtomicU64::from_ptr(cs.add(16) as *mut u64) };
    let recursion = unsafe { cs.add(12) as *mut i32 };
    let mut spins = 0u32;
    loop {
        if owner.load(Ordering::Acquire) == me {
            unsafe { *recursion += 1 };
            return status::SUCCESS;
        }
        if owner.compare_exchange(0, me, Ordering::Acquire, Ordering::Relaxed).is_ok() {
            unsafe {
                *recursion = 1;
                *(cs.add(8) as *mut i32) = 0;
            }
            return status::SUCCESS;
        }
        spins += 1;
        if spins > 64 {
            let y: unsafe extern "system" fn() -> NtStatus = crate::NtYieldExecution;
            unsafe { y() };
        } else {
            core::hint::spin_loop();
        }
    }
}

#[no_mangle]
pub unsafe extern "system" fn RtlLeaveCriticalSection(cs: *mut u8) -> NtStatus {
    unsafe {
        let recursion = cs.add(12) as *mut i32;
        *recursion -= 1;
        if *recursion <= 0 {
            *recursion = 0;
            *(cs.add(8) as *mut i32) = -1;
            AtomicU64::from_ptr(cs.add(16) as *mut u64).store(0, Ordering::Release);
        }
    }
    status::SUCCESS
}

// -----| the current directory |-----

/// RtlSetCurrentDirectory_U: a DOS path (or \??\ one), kept in the process
/// parameters where kernel32 keeps it.
#[no_mangle]
pub unsafe extern "system" fn RtlSetCurrentDirectory_U(path: *const UnicodeString) -> NtStatus {
    let mut src = unsafe { units(path) };
    let nt_prefix = [b'\\' as u16, b'?' as u16, b'?' as u16, b'\\' as u16];
    if src.starts_with(&nt_prefix) {
        src = &src[4..];
    }
    let mut full = [0u16; 1024];
    let Some((mut len, _)) = crate::rtl::full_dos_path(src, &mut full) else { return status::INVALID_PARAMETER };
    if len == 0 || len + 2 > full.len() {
        return status::INVALID_PARAMETER;
    }
    if full[len - 1] != b'\\' as u16 {
        full[len] = b'\\' as u16;
        len += 1;
    }
    unsafe {
        let params: u64;
        core::arch::asm!("mov {}, gs:[0x60]", out(reg) params, options(nostack, readonly));
        let params = read_unaligned((params + layout::PEB_PROCESS_PARAMETERS as u64) as *const u64);
        let us = &mut *((params as usize + layout::PARAMS_CURRENT_DIRECTORY) as *mut UnicodeString);
        if len + 1 > us.maximum_length as usize / 2 {
            return 0xC000_0106u32 as i32; // STATUS_NAME_TOO_LONG
        }
        core::ptr::copy_nonoverlapping(full.as_ptr(), us.buffer, len);
        *us.buffer.add(len) = 0;
        us.length = (len * 2) as u16;
    }
    status::SUCCESS
}

// -----| tokens: one user, every privilege, nothing checked |-----

/// The previous state, if asked for, lists no privileges.
#[no_mangle]
pub unsafe extern "system" fn NtAdjustPrivilegesToken(_token: u64, _disable_all: u8, _new: *const u8, len: u32, prev: *mut u8, ret: *mut u32) -> NtStatus {
    unsafe {
        if !prev.is_null() && len >= 4 {
            write_unaligned(prev as *mut u32, 0);
        }
        if !ret.is_null() {
            *ret = 4;
        }
    }
    status::SUCCESS
}

#[no_mangle]
pub extern "system" fn NtSetInformationToken(_token: u64, _class: u32, _info: *const u8, _len: u32) -> NtStatus {
    status::SUCCESS
}

#[no_mangle]
pub extern "system" fn NtDuplicateToken(_token: u64, _access: u32, _attrs: *const u8, _effective: u8, _kind: u32, _out: *mut u64) -> NtStatus {
    STATUS_NOT_SUPPORTED
}

/// Every privilege is held.
#[no_mangle]
pub unsafe extern "system" fn NtPrivilegeCheck(_token: u64, _privileges: *mut u8, result: *mut u8) -> NtStatus {
    if !result.is_null() {
        unsafe { *result = 1 };
    }
    status::SUCCESS
}

/// Whatever was asked for is granted.
#[no_mangle]
pub unsafe extern "system" fn NtAccessCheck(_sd: *const u8, _token: u64, desired: u32, _mapping: *const u8, _privs: *mut u8, privs_len: *mut u32, granted: *mut u32, result: *mut NtStatus) -> NtStatus {
    unsafe {
        if !privs_len.is_null() && *privs_len >= 8 && !_privs.is_null() {
            write_unaligned(_privs as *mut u32, 0);
        }
        *granted = desired;
        *result = status::SUCCESS;
    }
    status::SUCCESS
}

static NEXT_LUID: AtomicU64 = AtomicU64::new(0x1000);

#[no_mangle]
pub unsafe extern "system" fn NtAllocateLocallyUniqueId(luid: *mut u64) -> NtStatus {
    unsafe { write_unaligned(luid, NEXT_LUID.fetch_add(1, Ordering::Relaxed)) };
    status::SUCCESS
}

// -----| threads, jobs, transactions: accepted, or not offered |-----

#[no_mangle]
pub extern "system" fn NtSetInformationThread(_thread: u64, _class: u32, _info: *const u8, _len: u32) -> NtStatus {
    status::SUCCESS
}

#[no_mangle]
pub unsafe extern "system" fn NtCreateJobObject(out: *mut u64, _access: u32, _attrs: *const u8) -> NtStatus {
    // A job is only a handle here (kernel32 does the same).
    let create: unsafe extern "system" fn(*mut u64, u32, *const u8, u32, u8) -> NtStatus =
        unsafe { core::mem::transmute(crate::NtCreateEvent as *const ()) };
    unsafe { create(out, 0x1F_0003, core::ptr::null(), 0, 0) }
}

#[no_mangle]
pub extern "system" fn NtOpenJobObject(_out: *mut u64, _access: u32, _attrs: *const u8) -> NtStatus {
    0xC000_0034u32 as i32 // STATUS_OBJECT_NAME_NOT_FOUND
}

#[no_mangle]
pub extern "system" fn NtAssignProcessToJobObject(_job: u64, _process: u64) -> NtStatus {
    status::SUCCESS
}

#[no_mangle]
pub extern "system" fn NtSetInformationJobObject(_job: u64, _class: u32, _info: *const u8, _len: u32) -> NtStatus {
    status::SUCCESS
}

#[no_mangle]
pub extern "system" fn NtQueryInformationJobObject(_job: u64, _class: u32, _info: *mut u8, _len: u32, _ret: *mut u32) -> NtStatus {
    STATUS_NOT_SUPPORTED
}

#[no_mangle]
pub extern "system" fn NtCreateTransaction(_out: *mut u64, _access: u32, _attrs: *const u8, _uow: *const u8, _tm: u64, _options: u32, _isolation: u32, _flags: u32, _timeout: *const i64, _desc: *const u8) -> NtStatus {
    STATUS_NOT_SUPPORTED
}

#[no_mangle]
pub extern "system" fn NtCommitTransaction(_tx: u64, _wait: u8) -> NtStatus {
    STATUS_NOT_SUPPORTED
}

#[no_mangle]
pub extern "system" fn NtRollbackTransaction(_tx: u64, _wait: u8) -> NtStatus {
    STATUS_NOT_SUPPORTED
}

#[no_mangle]
pub extern "system" fn RtlGetCurrentTransaction() -> u64 {
    0
}

#[no_mangle]
pub extern "system" fn RtlSetCurrentTransaction(_tx: u64) -> u8 {
    1
}

// -----| time |-----

/// KUSER_SHARED_DATA.SystemTime (a KSYSTEM_TIME at 0x14).
#[no_mangle]
pub unsafe extern "system" fn NtQuerySystemTime(out: *mut i64) -> NtStatus {
    let kusd = 0x7FFE_0000usize;
    loop {
        unsafe {
            let high1 = read_unaligned((kusd + 0x18) as *const i32);
            let low = read_unaligned((kusd + 0x14) as *const u32);
            let high2 = read_unaligned((kusd + 0x1C) as *const i32);
            if high1 == high2 {
                write_unaligned(out, (high1 as i64) << 32 | low as i64);
                return status::SUCCESS;
            }
        }
    }
}

/// 100 ns units: the tick is 15.625 ms, 0.5 ms at best.
#[no_mangle]
pub unsafe extern "system" fn NtQueryTimerResolution(max: *mut u32, min: *mut u32, current: *mut u32) -> NtStatus {
    unsafe {
        *max = 156_250;
        *min = 5_000;
        *current = 156_250;
    }
    status::SUCCESS
}

// -----| files and memory: accepted, or not offered |-----

unsafe fn iosb_ok(iosb: *mut u8) {
    if !iosb.is_null() {
        unsafe { core::ptr::write_bytes(iosb, 0, 16) };
    }
}

/// Byte-range locks aren't kept: every lock is granted.
#[no_mangle]
pub unsafe extern "system" fn NtLockFile(_file: u64, _event: u64, _apc: u64, _ctx: u64, iosb: *mut u8, _offset: *const i64, _len: *const i64, _key: u32, _fail: u8, _exclusive: u8) -> NtStatus {
    unsafe { iosb_ok(iosb) };
    status::SUCCESS
}

#[no_mangle]
pub unsafe extern "system" fn NtUnlockFile(_file: u64, iosb: *mut u8, _offset: *const i64, _len: *const i64, _key: u32) -> NtStatus {
    unsafe { iosb_ok(iosb) };
    status::SUCCESS
}

#[no_mangle]
pub extern "system" fn NtQueryEaFile(_file: u64, _iosb: *mut u8, _buf: *mut u8, _len: u32, _single: u8, _list: *const u8, _list_len: u32, _index: *const u32, _restart: u8) -> NtStatus {
    STATUS_EAS_NOT_SUPPORTED
}

#[no_mangle]
pub extern "system" fn NtSetEaFile(_file: u64, _iosb: *mut u8, _buf: *const u8, _len: u32) -> NtStatus {
    STATUS_EAS_NOT_SUPPORTED
}

#[no_mangle]
pub extern "system" fn NtQueryQuotaInformationFile(_file: u64, _iosb: *mut u8, _buf: *mut u8, _len: u32, _single: u8, _list: *const u8, _list_len: u32, _start: *const u8, _restart: u8) -> NtStatus {
    STATUS_NOT_SUPPORTED
}

#[no_mangle]
pub extern "system" fn NtSetQuotaInformationFile(_file: u64, _iosb: *mut u8, _buf: *const u8, _len: u32) -> NtStatus {
    STATUS_NOT_SUPPORTED
}

#[no_mangle]
pub extern "system" fn NtSetVolumeInformationFile(_file: u64, _iosb: *mut u8, _buf: *const u8, _len: u32, _class: u32) -> NtStatus {
    STATUS_NOT_SUPPORTED
}

/// Pages are never swapped out, so locking them changes nothing.
#[no_mangle]
pub extern "system" fn NtLockVirtualMemory(_process: u64, _base: *mut u64, _size: *mut u64, _kind: u32) -> NtStatus {
    status::SUCCESS
}

#[no_mangle]
pub extern "system" fn NtUnlockVirtualMemory(_process: u64, _base: *mut u64, _size: *mut u64, _kind: u32) -> NtStatus {
    status::SUCCESS
}

// -----| the registry through ntdll, and debug buffers: not yet |-----

#[no_mangle]
pub extern "system" fn RtlCheckRegistryKey(_relative_to: u32, _path: *const u16) -> NtStatus {
    0xC000_0034u32 as i32
}

#[no_mangle]
pub extern "system" fn RtlQueryRegistryValues(_relative_to: u32, _path: *const u16, _table: *mut u8, _context: u64, _env: u64) -> NtStatus {
    0xC000_0034u32 as i32
}

#[no_mangle]
pub extern "system" fn RtlCreateQueryDebugBuffer(_size: u32, _event_pair: u8) -> *mut u8 {
    null_mut()
}

#[no_mangle]
pub extern "system" fn RtlQueryProcessDebugInformation(_pid: u64, _flags: u32, _buffer: *mut u8) -> NtStatus {
    STATUS_NOT_SUPPORTED
}

#[no_mangle]
pub extern "system" fn RtlDestroyQueryDebugBuffer(_buffer: *mut u8) -> NtStatus {
    status::SUCCESS
}

/// Directory objects list nothing yet.
#[no_mangle]
pub extern "system" fn NtQueryDirectoryObject(_dir: u64, _buf: *mut u8, _len: u32, _single: u8, _restart: u8, _context: *mut u32, _ret: *mut u32) -> NtStatus {
    STATUS_NO_MORE_ENTRIES
}

// -----| registry keys through ntdll: not yet |-----
// (kernel32's registry calls work; these answer as for keys that aren't there.)

#[no_mangle]
pub extern "system" fn NtOpenKey(_out: *mut u64, _access: u32, _attrs: *const u8) -> NtStatus {
    0xC000_0034u32 as i32 // STATUS_OBJECT_NAME_NOT_FOUND
}

#[no_mangle]
pub extern "system" fn NtCreateKey(_out: *mut u64, _access: u32, _attrs: *const u8, _index: u32, _class: *const u8, _options: u32, _disposition: *mut u32) -> NtStatus {
    0xC000_0034u32 as i32
}

#[no_mangle]
pub extern "system" fn NtQueryValueKey(_key: u64, _name: *const u8, _class: u32, _info: *mut u8, _len: u32, _ret: *mut u32) -> NtStatus {
    status::INVALID_HANDLE
}

#[no_mangle]
pub extern "system" fn NtSetValueKey(_key: u64, _name: *const u8, _index: u32, _kind: u32, _data: *const u8, _len: u32) -> NtStatus {
    status::INVALID_HANDLE
}

// -----| what Cygwin loads from ntdll by name |-----

// -----| the current directory's FAST_CWD pointer |-----
//
// Windows keeps the current directory in a FAST_CWD structure that a pointer
// in ntdll's data points at, locked by FastPebLock (also ntdll's). The MSYS2
// runtime finds that pointer by following the first call in
// RtlGetCurrentDirectory_U and reading the instructions there, then keeps
// its own FAST_CWD in it (writing the process parameters as well, which is
// where VEILOS keeps the directory). Without it, it warns at every start.
// So the pointer and the lock are here, and `fast_cwd` reads the pointer
// with the instructions it looks for: lea rcx,[lock]; call
// RtlEnterCriticalSection; mov rbx,[pointer]; test rbx,rbx.

/// FastPebLock: a CRITICAL_SECTION (40 bytes; LockCount -1 when free).
#[repr(C, align(16))]
struct Lock([u8; 40]);
static mut FAST_PEB_LOCK: Lock = Lock([0, 0, 0, 0, 0, 0, 0, 0, 0xFF, 0xFF, 0xFF, 0xFF, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
/// The FAST_CWD pointer (only programs that keep their own write it).
static mut FAST_CWD: u64 = 0;

pub fn fast_peb_lock() -> *mut u8 {
    unsafe { core::ptr::addr_of_mut!(FAST_PEB_LOCK) as *mut u8 }
}

/// The FAST_CWD pointer, read under FastPebLock.
#[unsafe(naked)]
unsafe extern "system" fn fast_cwd() -> u64 {
    core::arch::naked_asm!(
        "push rbx",
        "sub rsp, 0x20",
        "lea rcx, [rip + {lock}]",
        "call {enter}",
        "mov rbx, [rip + {cwd}]",
        "test rbx, rbx",
        "lea rcx, [rip + {lock}]",
        "call {leave}",
        "mov rax, rbx",
        "add rsp, 0x20",
        "pop rbx",
        "ret",
        lock = sym FAST_PEB_LOCK,
        cwd = sym FAST_CWD,
        enter = sym RtlEnterCriticalSection,
        leave = sym RtlLeaveCriticalSection,
    )
}

/// RtlGetCurrentDirectory_U(size_in_bytes, buffer): bytes written, or needed.
/// Its first call is to `fast_cwd` (see above); the directory itself comes
/// from the process parameters.
#[unsafe(naked)]
#[no_mangle]
pub unsafe extern "system" fn RtlGetCurrentDirectory_U(_size: u32, _out: *mut u16) -> u32 {
    core::arch::naked_asm!(
        "push rcx",
        "push rdx",
        "sub rsp, 0x28",
        "call {fast_cwd}",
        "add rsp, 0x28",
        "pop rdx",
        "pop rcx",
        "jmp {current}",
        fast_cwd = sym fast_cwd,
        current = sym current_directory,
    )
}

unsafe extern "system" fn current_directory(size: u32, out: *mut u16) -> u32 {
    unsafe {
        let peb: u64;
        core::arch::asm!("mov {}, gs:[0x60]", out(reg) peb, options(nostack, readonly));
        let params = read_unaligned((peb + layout::PEB_PROCESS_PARAMETERS as u64) as *const u64);
        let us = &*((params as usize + layout::PARAMS_CURRENT_DIRECTORY) as *const UnicodeString);
        let mut n = us.length as usize / 2;
        // Without its trailing backslash, unless it's a drive's root.
        if n > 3 && *us.buffer.add(n - 1) == 0x5C {
            n -= 1;
        }
        if (size as usize) < (n + 1) * 2 {
            return ((n + 1) * 2) as u32;
        }
        core::ptr::copy_nonoverlapping(us.buffer, out, n);
        *out.add(n) = 0;
        (n * 2) as u32
    }
}

/// Placeholder files (cloud files) are shown as what they are: the previous
/// mode was PHCM_EXPOSE_PLACEHOLDERS.
#[no_mangle]
pub extern "system" fn RtlSetProcessPlaceholderCompatibilityMode(_mode: i8) -> i8 {
    2
}

/// NtMapViewOfSectionEx(section, process, base*, offset*, size*, type,
/// protect, params, count): extended parameters aren't honoured.
#[no_mangle]
pub unsafe extern "system" fn NtMapViewOfSectionEx(section: u64, process: u64, base: *mut u64, offset: *const i64, size: *mut u64, kind: u32, protect: u32, _params: *const u8, _count: u32) -> NtStatus {
    let map: unsafe extern "system" fn(u64, u64, *mut u64, usize, usize, *const i64, *mut u64, u32, u32, u32) -> NtStatus =
        unsafe { core::mem::transmute(crate::NtMapViewOfSection as *const ()) };
    unsafe { map(section, process, base, 0, 0, offset, size, 2, kind, protect) }
}
