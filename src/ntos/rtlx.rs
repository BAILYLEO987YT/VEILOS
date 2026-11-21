// More of the run-time library: counted-string conversions, bitmaps,
// generic tables, SIDs and security descriptors, time fields, resource
// descriptor encoding, and the registry helpers (RtlQueryRegistryValues).
//
// Layouts (x64, checked in k1's layout.c): RTL_BITMAP { SizeOfBitMap 0,
// Buffer 8 }; RTL_GENERIC_TABLE's NumberGenericTableElements at 0x24 (the
// elements themselves are kept here, in compare order); SECURITY_DESCRIPTOR
// (absolute) { Revision 0, Control 2, Owner 8, Group 0x10, Sacl 0x18,
// Dacl 0x20 }; TIME_FIELDS: eight i16; RTL_QUERY_REGISTRY_TABLE (0x38).

use alloc::{collections::BTreeMap, format, string::String, vec::Vec};
use spin::Mutex;

use super::{pool, rtl, NtStatus, STATUS_INVALID_PARAMETER, STATUS_NO_MEMORY, STATUS_OBJECT_NAME_NOT_FOUND, STATUS_SUCCESS};

const STATUS_BUFFER_OVERFLOW: NtStatus = 0x8000_0005u32 as i32;
const STATUS_BUFFER_TOO_SMALL: NtStatus = 0xC000_0023u32 as i32;
const STATUS_NOT_IMPLEMENTED: NtStatus = 0xC000_0002u32 as i32;
const STATUS_INVALID_SID: NtStatus = 0xC000_0078u32 as i32;

unsafe fn rd<T: Copy>(p: *const u8, off: usize) -> T {
    unsafe { (p.add(off) as *const T).read_unaligned() }
}
unsafe fn wr<T>(p: *mut u8, off: usize, v: T) {
    unsafe { (p.add(off) as *mut T).write_unaligned(v) }
}

// -----| counted strings |-----
// UNICODE_STRING / ANSI_STRING: Length 0 (bytes), MaximumLength 2, Buffer 8.

fn us_units(us: *const u8) -> Vec<u16> {
    unsafe {
        let len = rd::<u16>(us, 0) as usize / 2;
        let b: *const u16 = rd(us, 8);
        if b.is_null() { Vec::new() } else { core::slice::from_raw_parts(b, len).to_vec() }
    }
}

fn as_bytes(s: *const u8) -> Vec<u8> {
    unsafe {
        let len = rd::<u16>(s, 0) as usize;
        let b: *const u8 = rd(s, 8);
        if b.is_null() { Vec::new() } else { core::slice::from_raw_parts(b, len).to_vec() }
    }
}

/// Put `units` into a UNICODE_STRING: a new pool buffer when `allocate`,
/// else its own buffer (BUFFER_OVERFLOW if it doesn't fit).
unsafe fn set_unicode(dst: *mut u8, units: &[u16], allocate: bool) -> NtStatus {
    let bytes = units.len() * 2;
    if bytes > 0xFFFC {
        return STATUS_INVALID_PARAMETER;
    }
    unsafe {
        if allocate {
            let b = pool::alloc_tracked(bytes + 2);
            if b.is_null() {
                return STATUS_NO_MEMORY;
            }
            core::ptr::copy_nonoverlapping(units.as_ptr(), b as *mut u16, units.len());
            wr::<u16>(dst, 2, (bytes + 2) as u16);
            wr::<*mut u8>(dst, 8, b);
        } else {
            let max = rd::<u16>(dst, 2) as usize;
            if bytes > max {
                return STATUS_BUFFER_OVERFLOW;
            }
            let b: *mut u16 = rd(dst, 8);
            core::ptr::copy_nonoverlapping(units.as_ptr(), b, units.len());
            if bytes + 2 <= max {
                b.add(units.len()).write(0);
            }
        }
        wr::<u16>(dst, 0, bytes as u16);
    }
    STATUS_SUCCESS
}

unsafe fn set_ansi(dst: *mut u8, bytes: &[u8], allocate: bool) -> NtStatus {
    if bytes.len() > 0xFFFE {
        return STATUS_INVALID_PARAMETER;
    }
    unsafe {
        if allocate {
            let b = pool::alloc_tracked(bytes.len() + 1);
            if b.is_null() {
                return STATUS_NO_MEMORY;
            }
            core::ptr::copy_nonoverlapping(bytes.as_ptr(), b, bytes.len());
            wr::<u16>(dst, 2, (bytes.len() + 1) as u16);
            wr::<*mut u8>(dst, 8, b);
        } else {
            let max = rd::<u16>(dst, 2) as usize;
            if bytes.len() > max {
                return STATUS_BUFFER_OVERFLOW;
            }
            let b: *mut u8 = rd(dst, 8);
            core::ptr::copy_nonoverlapping(bytes.as_ptr(), b, bytes.len());
            if bytes.len() < max {
                b.add(bytes.len()).write(0);
            }
        }
        wr::<u16>(dst, 0, bytes.len() as u16);
    }
    STATUS_SUCCESS
}

/// Code page conversion as VEILOS's kernel does it: Latin-1 (the low 256
/// code points), '?' for the rest.
fn to_narrow(units: &[u16]) -> Vec<u8> {
    units.iter().map(|&u| if u < 256 { u as u8 } else { b'?' }).collect()
}

pub extern "win64" fn RtlAnsiStringToUnicodeString(dst: *mut u8, src: *const u8, allocate: u8) -> NtStatus {
    let units: Vec<u16> = as_bytes(src).iter().map(|&b| b as u16).collect();
    unsafe { set_unicode(dst, &units, allocate != 0) }
}

pub extern "win64" fn RtlUnicodeStringToAnsiString(dst: *mut u8, src: *const u8, allocate: u8) -> NtStatus {
    unsafe { set_ansi(dst, &to_narrow(&us_units(src)), allocate != 0) }
}

pub extern "win64" fn RtlFreeUnicodeString(us: *mut u8) {
    unsafe {
        pool::free_tracked(rd(us, 8));
        core::ptr::write_bytes(us, 0, 16);
    }
}

pub extern "win64" fn RtlFreeAnsiString(s: *mut u8) {
    RtlFreeUnicodeString(s)
}

pub extern "win64" fn RtlCreateUnicodeString(us: *mut u8, src: *const u16) -> u8 {
    let n = rtl::wcslen(src);
    let units = unsafe { core::slice::from_raw_parts(src, n) };
    (unsafe { set_unicode(us, units, true) } == STATUS_SUCCESS) as u8
}

unsafe fn append(dst: *mut u8, more: &[u16]) -> NtStatus {
    unsafe {
        let len = rd::<u16>(dst, 0) as usize;
        let max = rd::<u16>(dst, 2) as usize;
        if len + more.len() * 2 > max {
            return STATUS_BUFFER_TOO_SMALL;
        }
        let b: *mut u16 = rd(dst, 8);
        core::ptr::copy_nonoverlapping(more.as_ptr(), b.add(len / 2), more.len());
        let new = len + more.len() * 2;
        if new + 2 <= max {
            b.add(new / 2).write(0);
        }
        wr::<u16>(dst, 0, new as u16);
    }
    STATUS_SUCCESS
}

pub extern "win64" fn RtlAppendUnicodeStringToString(dst: *mut u8, src: *const u8) -> NtStatus {
    unsafe { append(dst, &us_units(src)) }
}

pub extern "win64" fn RtlAppendUnicodeToString(dst: *mut u8, src: *const u16) -> NtStatus {
    if src.is_null() {
        return STATUS_SUCCESS;
    }
    let n = rtl::wcslen(src);
    unsafe { append(dst, core::slice::from_raw_parts(src, n)) }
}

/// Text to a ULONG, as RtlCharToInteger: leading blanks, a sign, then
/// base 0 meaning 0x/0o/0b prefixes or decimal.
fn parse_int(chars: &[u16], base: u32) -> Option<u32> {
    let mut i = 0;
    while i < chars.len() && (chars[i] == b' ' as u16 || chars[i] == b'\t' as u16) {
        i += 1;
    }
    let mut neg = false;
    if i < chars.len() && (chars[i] == b'-' as u16 || chars[i] == b'+' as u16) {
        neg = chars[i] == b'-' as u16;
        i += 1;
    }
    let mut base = base;
    if base == 0 {
        base = 10;
        if i + 1 < chars.len() && chars[i] == b'0' as u16 {
            match chars[i + 1] as u8 {
                b'x' | b'X' => (base, i) = (16, i + 2),
                b'o' | b'O' => (base, i) = (8, i + 2),
                b'b' | b'B' => (base, i) = (2, i + 2),
                _ => {}
            }
        }
    }
    if !matches!(base, 2 | 8 | 10 | 16) {
        return None;
    }
    let mut v: u32 = 0;
    while i < chars.len() {
        let Some(d) = char::from_u32(chars[i] as u32).and_then(|c| c.to_digit(base)) else { break };
        v = v.wrapping_mul(base).wrapping_add(d);
        i += 1;
    }
    Some(if neg { v.wrapping_neg() } else { v })
}

pub extern "win64" fn RtlUnicodeStringToInteger(us: *const u8, base: u32, out: *mut u32) -> NtStatus {
    match parse_int(&us_units(us), base) {
        Some(v) => {
            unsafe { out.write(v) };
            STATUS_SUCCESS
        }
        None => STATUS_INVALID_PARAMETER,
    }
}

pub extern "win64" fn RtlCharToInteger(s: *const u8, base: u32, out: *mut u32) -> NtStatus {
    let n = rtl::strlen(s);
    let units: Vec<u16> = unsafe { core::slice::from_raw_parts(s, n) }.iter().map(|&b| b as u16).collect();
    match parse_int(&units, base) {
        Some(v) => {
            unsafe { out.write(v) };
            STATUS_SUCCESS
        }
        None => STATUS_INVALID_PARAMETER,
    }
}

pub extern "win64" fn RtlUpcaseUnicodeChar(c: u16) -> u16 {
    if (b'a' as u16..=b'z' as u16).contains(&c) || (0xE0..=0xFE).contains(&c) && c != 0xF7 { c - 32 } else { c }
}

/// RtlAnsiCharToUnicodeChar(PUCHAR* SourceCharacter): one character, and
/// the pointer moved past it.
pub extern "win64" fn RtlAnsiCharToUnicodeChar(p: *mut *const u8) -> u16 {
    unsafe {
        let c = (*p).read();
        *p = (*p).add(1);
        c as u16
    }
}

pub extern "win64" fn RtlMultiByteToUnicodeN(dst: *mut u16, max_bytes: u32, out_bytes: *mut u32, src: *const u8, src_bytes: u32) -> NtStatus {
    let n = (max_bytes / 2).min(src_bytes) as usize;
    for i in 0..n {
        unsafe { dst.add(i).write(src.add(i).read() as u16) };
    }
    if !out_bytes.is_null() {
        unsafe { out_bytes.write(n as u32 * 2) };
    }
    STATUS_SUCCESS
}

pub extern "win64" fn RtlUnicodeToMultiByteN(dst: *mut u8, max_bytes: u32, out_bytes: *mut u32, src: *const u16, src_bytes: u32) -> NtStatus {
    let n = max_bytes.min(src_bytes / 2) as usize;
    for i in 0..n {
        let u = unsafe { src.add(i).read() };
        unsafe { dst.add(i).write(if u < 256 { u as u8 } else { b'?' }) };
    }
    if !out_bytes.is_null() {
        unsafe { out_bytes.write(n as u32) };
    }
    STATUS_SUCCESS
}

pub extern "win64" fn RtlUnicodeToMultiByteSize(out_bytes: *mut u32, _src: *const u16, src_bytes: u32) -> NtStatus {
    unsafe { out_bytes.write(src_bytes / 2) };
    STATUS_SUCCESS
}

pub extern "win64" fn RtlZeroMemory(d: *mut u8, n: usize) {
    unsafe { core::ptr::write_bytes(d, 0, n) };
}

// -----| bitmaps |-----

struct Bits {
    size: u32,
    buf: *mut u32,
}

fn bits(h: *const u8) -> Bits {
    unsafe { Bits { size: rd(h, 0), buf: rd(h, 8) } }
}

impl Bits {
    fn get(&self, i: u32) -> bool {
        unsafe { self.buf.add((i / 32) as usize).read() >> (i % 32) & 1 != 0 }
    }
    fn put(&self, i: u32, v: bool) {
        unsafe {
            let w = self.buf.add((i / 32) as usize);
            if v { w.write(w.read() | 1 << (i % 32)) } else { w.write(w.read() & !(1 << (i % 32))) }
        }
    }
    fn range(&self, start: u32, n: u32, v: bool) {
        for i in start..start.saturating_add(n).min(self.size) {
            self.put(i, v);
        }
    }
    fn all(&self, start: u32, n: u32, v: bool) -> bool {
        start.checked_add(n).is_some_and(|e| e <= self.size) && (start..start + n).all(|i| self.get(i) == v)
    }
    /// The first run of `n` bits equal to `v`, searching from `hint` and
    /// wrapping round.
    fn find(&self, n: u32, hint: u32, v: bool) -> u32 {
        if n == 0 {
            return 0;
        }
        if n > self.size {
            return u32::MAX;
        }
        let hint = if hint >= self.size { 0 } else { hint };
        let last = self.size - n;
        for s in (hint..=last).chain(0..hint.min(last + 1)) {
            if self.all(s, n, v) {
                return s;
            }
        }
        u32::MAX
    }
}

pub extern "win64" fn RtlInitializeBitMap(h: *mut u8, buf: *mut u32, size: u32) {
    unsafe {
        wr::<u32>(h, 0, size);
        wr::<*mut u32>(h, 8, buf);
    }
}
pub extern "win64" fn RtlClearAllBits(h: *const u8) {
    let b = bits(h);
    unsafe { core::ptr::write_bytes(b.buf, 0, b.size.div_ceil(32) as usize) };
}
pub extern "win64" fn RtlSetAllBits(h: *const u8) {
    let b = bits(h);
    unsafe { core::ptr::write_bytes(b.buf, 0xFF, b.size.div_ceil(32) as usize) };
}
pub extern "win64" fn RtlSetBit(h: *const u8, i: u32) {
    bits(h).put(i, true)
}
pub extern "win64" fn RtlClearBit(h: *const u8, i: u32) {
    bits(h).put(i, false)
}
pub extern "win64" fn RtlTestBit(h: *const u8, i: u32) -> u8 {
    bits(h).get(i) as u8
}
pub extern "win64" fn RtlSetBits(h: *const u8, start: u32, n: u32) {
    bits(h).range(start, n, true)
}
pub extern "win64" fn RtlClearBits(h: *const u8, start: u32, n: u32) {
    bits(h).range(start, n, false)
}
pub extern "win64" fn RtlAreBitsSet(h: *const u8, start: u32, n: u32) -> u8 {
    bits(h).all(start, n, true) as u8
}
pub extern "win64" fn RtlAreBitsClear(h: *const u8, start: u32, n: u32) -> u8 {
    bits(h).all(start, n, false) as u8
}
pub extern "win64" fn RtlFindClearBits(h: *const u8, n: u32, hint: u32) -> u32 {
    bits(h).find(n, hint, false)
}
pub extern "win64" fn RtlFindSetBits(h: *const u8, n: u32, hint: u32) -> u32 {
    bits(h).find(n, hint, true)
}
pub extern "win64" fn RtlFindClearBitsAndSet(h: *const u8, n: u32, hint: u32) -> u32 {
    let b = bits(h);
    let at = b.find(n, hint, false);
    if at != u32::MAX {
        b.range(at, n, true);
    }
    at
}
/// RtlFindNextForwardRunClear(BitMap, FromIndex, StartingRunIndex*): the
/// next run of clear bits at or after FromIndex; its length (0: none).
pub extern "win64" fn RtlFindNextForwardRunClear(h: *const u8, from: u32, start: *mut u32) -> u32 {
    let b = bits(h);
    let Some(s) = (from..b.size).find(|&i| !b.get(i)) else { return 0 };
    let e = (s..b.size).find(|&i| b.get(i)).unwrap_or(b.size);
    unsafe { start.write(s) };
    e - s
}
pub extern "win64" fn RtlNumberOfSetBits(h: *const u8) -> u32 {
    let b = bits(h);
    (0..b.size).filter(|&i| b.get(i)).count() as u32
}

// -----| generic tables |-----
//
// RTL_GENERIC_TABLE: CompareRoutine 0x28, AllocateRoutine 0x30, FreeRoutine
// 0x38, TableContext 0x40, NumberGenericTableElements 0x24. Each element is
// allocated through the table's AllocateRoutine with Windows' header in
// front (splay links and a list entry: 0x28 bytes); its data follows.

const ELEMENT_HEADER: usize = 0x28;
type Compare = extern "win64" fn(*mut u8, *const u8, *const u8) -> u32; // 0 less, 1 greater, 2 equal
type Allocate = extern "win64" fn(*mut u8, u32) -> *mut u8;
type Free = extern "win64" fn(*mut u8, *mut u8);

static TABLES: Mutex<BTreeMap<usize, Vec<usize>>> = Mutex::new(BTreeMap::new());

fn locked<R>(f: impl FnOnce(&mut BTreeMap<usize, Vec<usize>>) -> R) -> R {
    x86_64::instructions::interrupts::without_interrupts(|| f(&mut TABLES.lock()))
}

pub extern "win64" fn RtlInitializeGenericTable(t: *mut u8, compare: u64, allocate: u64, free: u64, context: u64) {
    unsafe {
        core::ptr::write_bytes(t, 0, 0x48);
        let list = t.add(8) as *mut u64;
        list.write(list as u64);
        list.add(1).write(list as u64);
        wr::<u64>(t, 0x28, compare);
        wr::<u64>(t, 0x30, allocate);
        wr::<u64>(t, 0x38, free);
        wr::<u64>(t, 0x40, context);
    }
    locked(|m| m.insert(t as usize, Vec::new()));
}

/// Where `data` is or would go in the table's order: Ok(index) if equal.
fn position(t: *mut u8, elems: &[usize], data: *const u8) -> Result<usize, usize> {
    let cmp: Compare = unsafe { core::mem::transmute(rd::<u64>(t, 0x28)) };
    let (mut lo, mut hi) = (0, elems.len());
    while lo < hi {
        let mid = (lo + hi) / 2;
        let e = (elems[mid] + ELEMENT_HEADER) as *const u8;
        match cmp(t, data, e) {
            2 => return Ok(mid),
            0 => hi = mid,
            _ => lo = mid + 1,
        }
    }
    Err(lo)
}

fn elements(t: *mut u8) -> Vec<usize> {
    locked(|m| m.get(&(t as usize)).cloned().unwrap_or_default())
}

pub extern "win64" fn RtlInsertElementGenericTable(t: *mut u8, data: *const u8, size: u32, new: *mut u8) -> *mut u8 {
    let elems = elements(t);
    match position(t, &elems, data) {
        Ok(i) => {
            if !new.is_null() {
                unsafe { new.write(0) };
            }
            (elems[i] + ELEMENT_HEADER) as *mut u8
        }
        Err(i) => {
            let alloc: Allocate = unsafe { core::mem::transmute(rd::<u64>(t, 0x30)) };
            let node = alloc(t, ELEMENT_HEADER as u32 + size);
            if node.is_null() {
                return node;
            }
            unsafe {
                core::ptr::write_bytes(node, 0, ELEMENT_HEADER);
                core::ptr::copy_nonoverlapping(data, node.add(ELEMENT_HEADER), size as usize);
                wr::<u32>(t, 0x24, rd::<u32>(t, 0x24) + 1);
                if !new.is_null() {
                    new.write(1);
                }
            }
            locked(|m| m.entry(t as usize).or_default().insert(i, node as usize));
            unsafe { node.add(ELEMENT_HEADER) }
        }
    }
}

pub extern "win64" fn RtlLookupElementGenericTable(t: *mut u8, data: *const u8) -> *mut u8 {
    let elems = elements(t);
    match position(t, &elems, data) {
        Ok(i) => (elems[i] + ELEMENT_HEADER) as *mut u8,
        Err(_) => core::ptr::null_mut(),
    }
}

pub extern "win64" fn RtlDeleteElementGenericTable(t: *mut u8, data: *const u8) -> u8 {
    let elems = elements(t);
    let Ok(i) = position(t, &elems, data) else { return 0 };
    let node = elems[i];
    locked(|m| {
        if let Some(v) = m.get_mut(&(t as usize)) {
            v.retain(|&n| n != node);
        }
    });
    unsafe { wr::<u32>(t, 0x24, rd::<u32>(t, 0x24).saturating_sub(1)) };
    let free: Free = unsafe { core::mem::transmute(rd::<u64>(t, 0x38)) };
    free(t, node as *mut u8);
    1
}

/// RtlEnumerateGenericTableWithoutSplaying(Table, RestartKey*): the next
/// element in order after *RestartKey (null: the first).
pub extern "win64" fn RtlEnumerateGenericTableWithoutSplaying(t: *mut u8, restart: *mut u64) -> *mut u8 {
    let elems = elements(t);
    let last = unsafe { restart.read() };
    let next = if last == 0 { 0 } else { elems.iter().position(|&n| n as u64 == last).map_or(elems.len(), |i| i + 1) };
    match elems.get(next) {
        Some(&n) => {
            unsafe { restart.write(n as u64) };
            (n + ELEMENT_HEADER) as *mut u8
        }
        None => core::ptr::null_mut(),
    }
}

/// RtlEnumerateGenericTable(Table, Restart): its own position in the table.
pub extern "win64" fn RtlEnumerateGenericTable(t: *mut u8, restart: u8) -> *mut u8 {
    unsafe {
        let key = t.add(0x18) as *mut u64; // OrderedPointer
        if restart != 0 {
            key.write(0);
        }
        RtlEnumerateGenericTableWithoutSplaying(t, key)
    }
}

pub extern "win64" fn RtlGetElementGenericTable(t: *mut u8, i: u32) -> *mut u8 {
    elements(t).get(i as usize).map_or(core::ptr::null_mut(), |&n| (n + ELEMENT_HEADER) as *mut u8)
}

pub extern "win64" fn RtlNumberGenericTableElements(t: *mut u8) -> u32 {
    unsafe { rd(t, 0x24) }
}

// -----| SIDs, ACLs, security descriptors |-----
//
// VEILOS doesn't check access in the kernel's driver layer; these build and
// read the structures drivers make (for device objects, registry keys).

pub extern "win64" fn RtlLengthSid(sid: *const u8) -> u32 {
    8 + 4 * unsafe { sid.add(1).read() } as u32
}

pub extern "win64" fn RtlValidSid(sid: *const u8) -> u8 {
    (!sid.is_null() && unsafe { sid.read() == 1 && sid.add(1).read() <= 15 }) as u8
}

pub extern "win64" fn RtlEqualSid(a: *const u8, b: *const u8) -> u8 {
    let (la, lb) = (RtlLengthSid(a), RtlLengthSid(b));
    (la == lb && unsafe { core::slice::from_raw_parts(a, la as usize) == core::slice::from_raw_parts(b, lb as usize) }) as u8
}

/// "S-1-5-32-544".
pub extern "win64" fn RtlConvertSidToUnicodeString(us: *mut u8, sid: *const u8, allocate: u8) -> NtStatus {
    if RtlValidSid(sid) == 0 {
        return STATUS_INVALID_SID;
    }
    let auth = unsafe { (0..6).fold(0u64, |a, i| a << 8 | sid.add(2 + i).read() as u64) };
    let mut s = format!("S-{}-{}", unsafe { sid.read() }, auth);
    for k in 0..unsafe { sid.add(1).read() } as usize {
        s += &format!("-{}", unsafe { rd::<u32>(sid, 8 + 4 * k) });
    }
    let units: Vec<u16> = s.encode_utf16().collect();
    unsafe { set_unicode(us, &units, allocate != 0) }
}

/// ACL header: AclRevision 0, AclSize 2 (u16), AceCount 4 (u16).
pub extern "win64" fn RtlCreateAcl(acl: *mut u8, len: u32, rev: u32) -> NtStatus {
    if len < 8 {
        return STATUS_BUFFER_TOO_SMALL;
    }
    unsafe {
        core::ptr::write_bytes(acl, 0, 8);
        acl.write(rev as u8);
        wr::<u16>(acl, 2, len as u16);
    }
    STATUS_SUCCESS
}

/// Append an ACCESS_ALLOWED_ACE { type 0, flags, size, mask, SID }.
pub extern "win64" fn RtlAddAccessAllowedAce(acl: *mut u8, _rev: u32, mask: u32, sid: *const u8) -> NtStatus {
    unsafe {
        let size: u16 = rd(acl, 2);
        let count: u16 = rd(acl, 4);
        // Find the end of the ACEs there are.
        let mut at = 8usize;
        for _ in 0..count {
            at += rd::<u16>(acl, at + 2) as usize;
        }
        let ace = 8 + RtlLengthSid(sid) as usize;
        if at + ace > size as usize {
            return STATUS_BUFFER_TOO_SMALL;
        }
        let p = acl.add(at);
        p.write(0);
        p.add(1).write(0);
        wr::<u16>(p, 2, ace as u16);
        wr::<u32>(p, 4, mask);
        core::ptr::copy_nonoverlapping(sid, p.add(8), ace - 8);
        wr::<u16>(acl, 4, count + 1);
    }
    STATUS_SUCCESS
}

const SE_SELF_RELATIVE: u16 = 0x8000;

pub extern "win64" fn RtlCreateSecurityDescriptor(sd: *mut u8, rev: u32) -> NtStatus {
    unsafe {
        core::ptr::write_bytes(sd, 0, 0x28);
        sd.write(rev as u8);
    }
    STATUS_SUCCESS
}

/// The pointer field at `abs_off` (absolute form) of either form.
unsafe fn sd_part(sd: *const u8, abs_off: usize) -> *mut u8 {
    unsafe {
        if rd::<u16>(sd, 2) & SE_SELF_RELATIVE != 0 {
            // Self-relative: Owner 4, Group 8, Sacl 12, Dacl 16 as offsets.
            let off: u32 = rd(sd, 4 + (abs_off - 8) / 2);
            if off == 0 { core::ptr::null_mut() } else { sd.add(off as usize) as *mut u8 }
        } else {
            rd(sd, abs_off)
        }
    }
}

pub extern "win64" fn RtlSetDaclSecurityDescriptor(sd: *mut u8, present: u8, dacl: *mut u8, defaulted: u8) -> NtStatus {
    unsafe {
        let mut c: u16 = rd(sd, 2);
        c &= !0x000C;
        if present != 0 {
            c |= 0x0004;
            if defaulted != 0 {
                c |= 0x0008;
            }
        }
        wr::<u16>(sd, 2, c);
        wr::<*mut u8>(sd, 0x20, if present != 0 { dacl } else { core::ptr::null_mut() });
    }
    STATUS_SUCCESS
}

pub extern "win64" fn RtlGetDaclSecurityDescriptor(sd: *const u8, present: *mut u8, dacl: *mut *mut u8, defaulted: *mut u8) -> NtStatus {
    unsafe {
        let c: u16 = rd(sd, 2);
        present.write((c & 0x4 != 0) as u8);
        if c & 0x4 != 0 {
            dacl.write(sd_part(sd, 0x20));
            defaulted.write((c & 0x8 != 0) as u8);
        }
    }
    STATUS_SUCCESS
}

pub extern "win64" fn RtlGetSaclSecurityDescriptor(sd: *const u8, present: *mut u8, sacl: *mut *mut u8, defaulted: *mut u8) -> NtStatus {
    unsafe {
        let c: u16 = rd(sd, 2);
        present.write((c & 0x10 != 0) as u8);
        if c & 0x10 != 0 {
            sacl.write(sd_part(sd, 0x18));
            defaulted.write((c & 0x20 != 0) as u8);
        }
    }
    STATUS_SUCCESS
}

pub extern "win64" fn RtlGetOwnerSecurityDescriptor(sd: *const u8, owner: *mut *mut u8, defaulted: *mut u8) -> NtStatus {
    unsafe {
        owner.write(sd_part(sd, 0x08));
        defaulted.write((rd::<u16>(sd, 2) & 0x1 != 0) as u8);
    }
    STATUS_SUCCESS
}

pub extern "win64" fn RtlGetGroupSecurityDescriptor(sd: *const u8, group: *mut *mut u8, defaulted: *mut u8) -> NtStatus {
    unsafe {
        group.write(sd_part(sd, 0x10));
        defaulted.write((rd::<u16>(sd, 2) & 0x2 != 0) as u8);
    }
    STATUS_SUCCESS
}

fn acl_len(acl: *const u8) -> usize {
    if acl.is_null() { 0 } else { unsafe { rd::<u16>(acl, 2) as usize } }
}
fn sid_len(sid: *const u8) -> usize {
    if sid.is_null() { 0 } else { RtlLengthSid(sid) as usize }
}

pub extern "win64" fn RtlLengthSecurityDescriptor(sd: *const u8) -> u32 {
    unsafe {
        let head = if rd::<u16>(sd, 2) & SE_SELF_RELATIVE != 0 { 20 } else { 0x28 };
        (head + sid_len(sd_part(sd, 0x08)) + sid_len(sd_part(sd, 0x10)) + acl_len(sd_part(sd, 0x18)) + acl_len(sd_part(sd, 0x20))) as u32
    }
}

/// RtlAbsoluteToSelfRelativeSD(Absolute, SelfRelative, BufferLength*).
pub extern "win64" fn RtlAbsoluteToSelfRelativeSD(abs: *const u8, rel: *mut u8, len: *mut u32) -> NtStatus {
    unsafe {
        let parts = [
            (sd_part(abs, 0x08), sid_len(sd_part(abs, 0x08))),
            (sd_part(abs, 0x10), sid_len(sd_part(abs, 0x10))),
            (sd_part(abs, 0x18), acl_len(sd_part(abs, 0x18))),
            (sd_part(abs, 0x20), acl_len(sd_part(abs, 0x20))),
        ];
        let need = 20 + parts.iter().map(|p| p.1).sum::<usize>();
        if (*len as usize) < need {
            *len = need as u32;
            return STATUS_BUFFER_TOO_SMALL;
        }
        core::ptr::write_bytes(rel, 0, 20);
        rel.write(abs.read());
        wr::<u16>(rel, 2, rd::<u16>(abs, 2) | SE_SELF_RELATIVE);
        let mut at = 20usize;
        for (k, (p, n)) in parts.iter().enumerate() {
            if !p.is_null() && *n > 0 {
                core::ptr::copy_nonoverlapping(*p, rel.add(at), *n);
                wr::<u32>(rel, 4 + 4 * k, at as u32);
                at += n;
            }
        }
        *len = need as u32;
    }
    STATUS_SUCCESS
}

/// SeCaptureSecurityDescriptor(Original, RequestorMode, PoolType,
/// ForceCapture, Captured*): a self-relative copy in pool.
pub extern "win64" fn SeCaptureSecurityDescriptor(sd: *const u8, _mode: u8, _pool: u32, _force: u8, out: *mut *mut u8) -> NtStatus {
    if sd.is_null() {
        unsafe { out.write(core::ptr::null_mut()) };
        return STATUS_SUCCESS;
    }
    let mut len = RtlLengthSecurityDescriptor(sd) + 32;
    let p = pool::alloc_tracked(len as usize);
    if p.is_null() {
        return STATUS_NO_MEMORY;
    }
    let st = RtlAbsoluteToSelfRelativeSD(sd, p, &mut len);
    unsafe { out.write(p) };
    st
}

// -----| time, randomness |-----

/// RtlTimeToTimeFields(Time*, TimeFields*): 100 ns units since 1601.
pub extern "win64" fn RtlTimeToTimeFields(time: *const i64, tf: *mut i16) {
    let t = unsafe { time.read() };
    let ms_total = t / 10_000;
    let days = ms_total / 86_400_000;
    let ms_day = ms_total % 86_400_000;
    // Civil date (proleptic Gregorian) from days since 1970-01-01, which is
    // day 134 774 after 1601-01-01; z counts from 0000-03-01.
    let z = days - 134_774 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + (month <= 2) as i64;
    let vals = [
        year,
        month,
        day,
        ms_day / 3_600_000,
        ms_day / 60_000 % 60,
        ms_day / 1000 % 60,
        ms_day % 1000,
        (days + 1).rem_euclid(7), // 1601-01-01 was a Monday
    ];
    for (i, v) in vals.iter().enumerate() {
        unsafe { tf.add(i).write(*v as i16) };
    }
}

/// RtlRandomEx(Seed*): a pseudo-random ULONG, the seed advanced.
pub extern "win64" fn RtlRandomEx(seed: *mut u32) -> u32 {
    unsafe {
        let s = seed.read().wrapping_mul(0x7FFF_FFED).wrapping_add(0x7FFF_FFC3) % 0x7FFF_FFFF;
        seed.write(s);
        s
    }
}

pub extern "win64" fn RtlCaptureStackBackTrace(_skip: u32, _count: u32, _out: *mut u64, hash: *mut u32) -> u16 {
    if !hash.is_null() {
        unsafe { hash.write(0) };
    }
    0
}

pub extern "win64" fn RtlVolumeDeviceToDosName(_dev: *mut u8, _name: *mut u8) -> NtStatus {
    STATUS_NOT_IMPLEMENTED
}

// -----| resource descriptors (CM_PARTIAL_RESOURCE_DESCRIPTOR) |-----
//
// Type 0, ShareDisposition 1, Flags 2 (u16), Start 4 (i64), Length 0xC.
// Memory above 4 GiB long uses CmResourceTypeMemoryLarge with the length
// shifted by 8, 16 or 32 (CM_RESOURCE_MEMORY_LARGE_40/48/64).

const CM_MEMORY: u8 = 3;
const CM_MEMORY_LARGE: u8 = 7;
const LARGE_40: u16 = 0x200;
const LARGE_48: u16 = 0x400;
const LARGE_64: u16 = 0x800;

pub extern "win64" fn RtlCmDecodeMemIoResource(d: *const u8, start: *mut u64) -> u64 {
    unsafe {
        if !start.is_null() {
            start.write(rd(d, 4));
        }
        let len = rd::<u32>(d, 0xC) as u64;
        if d.read() != CM_MEMORY_LARGE {
            return len;
        }
        let f: u16 = rd(d, 2);
        if f & LARGE_40 != 0 {
            len << 8
        } else if f & LARGE_48 != 0 {
            len << 16
        } else if f & LARGE_64 != 0 {
            len << 32
        } else {
            len
        }
    }
}

pub extern "win64" fn RtlCmEncodeMemIoResource(d: *mut u8, kind: u8, length: u64, start: u64) -> NtStatus {
    unsafe {
        d.write(kind);
        wr::<u64>(d, 4, start);
        let mut f: u16 = rd(d, 2);
        f &= !(LARGE_40 | LARGE_48 | LARGE_64);
        let (len, flag) = if kind != CM_MEMORY_LARGE || length <= u32::MAX as u64 {
            (length, 0)
        } else if length & 0xFF == 0 && length >> 8 <= u32::MAX as u64 {
            (length >> 8, LARGE_40)
        } else if length & 0xFFFF == 0 && length >> 16 <= u32::MAX as u64 {
            (length >> 16, LARGE_48)
        } else if length & 0xFFFF_FFFF == 0 {
            (length >> 32, LARGE_64)
        } else {
            return STATUS_INVALID_PARAMETER;
        };
        if kind == CM_MEMORY && length > u32::MAX as u64 {
            return STATUS_INVALID_PARAMETER;
        }
        wr::<u16>(d, 2, f | flag);
        wr::<u32>(d, 0xC, len as u32);
    }
    STATUS_SUCCESS
}

/// IO_RESOURCE_DESCRIPTOR: Type 1, Flags 4, Length 8, Alignment 0xC,
/// MinimumAddress 0x10, MaximumAddress 0x18.
pub extern "win64" fn RtlIoDecodeMemIoResource(d: *const u8, align: *mut u64, min: *mut u64, max: *mut u64) -> u64 {
    unsafe {
        if !align.is_null() {
            align.write(rd::<u32>(d, 0xC) as u64);
        }
        if !min.is_null() {
            min.write(rd(d, 0x10));
        }
        if !max.is_null() {
            max.write(rd(d, 0x18));
        }
        rd::<u32>(d, 8) as u64
    }
}

pub extern "win64" fn RtlIoEncodeMemIoResource(d: *mut u8, kind: u8, length: u64, align: u64, min: u64, max: u64) -> NtStatus {
    if length > u32::MAX as u64 || align > u32::MAX as u64 {
        return STATUS_INVALID_PARAMETER;
    }
    unsafe {
        d.add(1).write(kind);
        wr::<u32>(d, 8, length as u32);
        wr::<u32>(d, 0xC, align as u32);
        wr::<u64>(d, 0x10, min);
        wr::<u64>(d, 0x18, max);
    }
    STATUS_SUCCESS
}

// -----| registry helpers |-----

/// The key a RelativeTo + Path pair names (RTL_REGISTRY_*), hive-rooted.
pub fn relative_key(rel: u32, path: *const u16) -> Option<String> {
    let p = if path.is_null() {
        String::new()
    } else {
        let n = rtl::wcslen(path);
        char::decode_utf16(unsafe { core::slice::from_raw_parts(path, n) }.iter().copied()).map(|c| c.unwrap_or('?')).collect()
    };
    let key = match rel & 0xFFFF {
        0 => super::cm::hive_path(&p)?,
        1 => format!("HKLM\\SYSTEM\\CurrentControlSet\\Services\\{}", p),
        2 => format!("HKLM\\SYSTEM\\CurrentControlSet\\Control\\{}", p),
        3 => format!("HKLM\\SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion\\{}", p),
        4 => format!("HKLM\\HARDWARE\\DEVICEMAP\\{}", p),
        5 => format!("HKU\\.DEFAULT\\{}", p),
        _ => return None,
    };
    Some(String::from(key.trim_end_matches('\\')))
}

pub extern "win64" fn RtlCheckRegistryKey(rel: u32, path: *const u16) -> NtStatus {
    match relative_key(rel, path) {
        Some(k) if crate::nt_registry::kernel_key(&k, false).is_some() => STATUS_SUCCESS,
        _ => STATUS_OBJECT_NAME_NOT_FOUND,
    }
}

pub extern "win64" fn RtlCreateRegistryKey(rel: u32, path: *const u16) -> NtStatus {
    match relative_key(rel, path).and_then(|k| crate::nt_registry::kernel_key(&k, true)) {
        Some(_) => STATUS_SUCCESS,
        None => STATUS_OBJECT_NAME_NOT_FOUND,
    }
}

const RTL_QUERY_REGISTRY_SUBKEY: u32 = 0x1;
const RTL_QUERY_REGISTRY_TOPKEY: u32 = 0x2;
const RTL_QUERY_REGISTRY_REQUIRED: u32 = 0x4;
const RTL_QUERY_REGISTRY_NOVALUE: u32 = 0x8;
const RTL_QUERY_REGISTRY_DIRECT: u32 = 0x20;
const REG_NONE: u32 = 0;
const REG_SZ: u32 = 1;
const REG_EXPAND_SZ: u32 = 2;
const REG_DWORD: u32 = 4;
const REG_MULTI_SZ: u32 = 7;
const REG_QWORD: u32 = 11;

type QueryRoutine = extern "win64" fn(*const u16, u32, *const u8, u32, u64, u64) -> NtStatus;

/// Hand one value to a table entry: its QueryRoutine, or (DIRECT) straight
/// into EntryContext. A DIRECT string goes into the UNICODE_STRING there
/// (a buffer of its own if Buffer is null); a DWORD or QWORD into the
/// variable; anything else into a buffer whose first LONG is its size (a
/// negative size: the room, with the length and type written first).
unsafe fn deliver(e: *const u8, name: &[u16], kind: u32, data: &[u8], context: u64) -> NtStatus {
    unsafe {
        let flags: u32 = rd(e, 8);
        let entry_ctx: *mut u8 = rd(e, 0x18);
        if flags & RTL_QUERY_REGISTRY_DIRECT == 0 {
            let routine: Option<QueryRoutine> = rd(e, 0);
            let Some(r) = routine else { return STATUS_SUCCESS };
            let mut z: Vec<u16> = name.to_vec();
            z.push(0);
            return r(z.as_ptr(), kind, data.as_ptr(), data.len() as u32, context, entry_ctx as u64);
        }
        match kind {
            REG_SZ | REG_EXPAND_SZ | REG_MULTI_SZ => {
                let units: Vec<u16> = data.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).take_while(|&u| u != 0 || kind == REG_MULTI_SZ).collect();
                let alloc = rd::<*mut u8>(entry_ctx, 8).is_null();
                set_unicode(entry_ctx, &units, alloc)
            }
            REG_DWORD if data.len() >= 4 => {
                core::ptr::copy_nonoverlapping(data.as_ptr(), entry_ctx, 4);
                STATUS_SUCCESS
            }
            REG_QWORD if data.len() >= 8 => {
                core::ptr::copy_nonoverlapping(data.as_ptr(), entry_ctx, 8);
                STATUS_SUCCESS
            }
            _ => {
                let size: i32 = rd(entry_ctx, 0);
                if size < 0 {
                    let room = (-size) as usize;
                    if data.len() + 8 > room {
                        return STATUS_BUFFER_TOO_SMALL;
                    }
                    wr::<u32>(entry_ctx, 0, data.len() as u32);
                    wr::<u32>(entry_ctx, 4, kind);
                    core::ptr::copy_nonoverlapping(data.as_ptr(), entry_ctx.add(8), data.len());
                } else {
                    if data.len() > size as usize {
                        return STATUS_BUFFER_TOO_SMALL;
                    }
                    core::ptr::copy_nonoverlapping(data.as_ptr(), entry_ctx, data.len());
                }
                STATUS_SUCCESS
            }
        }
    }
}

/// RtlQueryRegistryValues(RelativeTo, Path, QueryTable, Context,
/// Environment): each entry of the table (ended by one with no routine and
/// no name) names a value of the key (or, SUBKEY, moves to a subkey); a
/// missing value takes the entry's default, or fails a REQUIRED entry.
/// Querying every value of a key (an entry with a routine and no name)
/// isn't supported: VEILOS's kernel registry API lists no values yet.
pub extern "win64" fn RtlQueryRegistryValues(rel: u32, path: *const u16, table: *const u8, context: u64, _env: u64) -> NtStatus {
    let Some(base) = relative_key(rel, path) else { return STATUS_OBJECT_NAME_NOT_FOUND };
    if crate::nt_registry::kernel_key(&base, false).is_none() {
        return STATUS_OBJECT_NAME_NOT_FOUND;
    }
    let mut key = base.clone();
    let mut e = table;
    unsafe {
        loop {
            let routine: u64 = rd(e, 0);
            let flags: u32 = rd(e, 8);
            let name_p: *const u16 = rd(e, 0x10);
            if routine == 0 && name_p.is_null() && flags & (RTL_QUERY_REGISTRY_DIRECT | RTL_QUERY_REGISTRY_SUBKEY) == 0 {
                break;
            }
            let name: Vec<u16> = if name_p.is_null() { Vec::new() } else { core::slice::from_raw_parts(name_p, rtl::wcslen(name_p)).to_vec() };
            let name_s: String = char::decode_utf16(name.iter().copied()).map(|c| c.unwrap_or('?')).collect();
            if flags & RTL_QUERY_REGISTRY_TOPKEY != 0 {
                key = base.clone();
            }
            if flags & RTL_QUERY_REGISTRY_SUBKEY != 0 {
                key = format!("{}\\{}", base, name_s);
                if crate::nt_registry::kernel_key(&key, false).is_none() {
                    return STATUS_OBJECT_NAME_NOT_FOUND;
                }
            } else if flags & RTL_QUERY_REGISTRY_NOVALUE != 0 {
                let r: Option<QueryRoutine> = core::mem::transmute(routine);
                if let Some(r) = r {
                    let st = r(core::ptr::null(), REG_NONE, core::ptr::null(), 0, context, rd(e, 0x18));
                    if st < 0 {
                        return st;
                    }
                }
            } else if name.is_empty() {
                return STATUS_NOT_IMPLEMENTED;
            } else {
                let st = match crate::nt_registry::kernel_get(&key, &name_s) {
                    Some((kind, data)) => deliver(e, &name, kind, &data, context),
                    None if flags & RTL_QUERY_REGISTRY_REQUIRED != 0 => return STATUS_OBJECT_NAME_NOT_FOUND,
                    None => {
                        let dtype: u32 = rd(e, 0x20);
                        if dtype == REG_NONE {
                            STATUS_SUCCESS
                        } else {
                            let ddata: *const u8 = rd(e, 0x28);
                            let mut dlen: u32 = rd(e, 0x30);
                            if dlen == 0 && matches!(dtype, REG_SZ | REG_EXPAND_SZ) && !ddata.is_null() {
                                dlen = (rtl::wcslen(ddata as *const u16) as u32 + 1) * 2;
                            }
                            let bytes = if ddata.is_null() { Vec::new() } else { core::slice::from_raw_parts(ddata, dlen as usize).to_vec() };
                            deliver(e, &name, dtype, &bytes, context)
                        }
                    }
                };
                if st < 0 {
                    return st;
                }
            }
            e = e.add(0x38);
        }
    }
    STATUS_SUCCESS
}
