//! Saved credentials (advapi32's CredWriteW, CredReadW, CredEnumerateW,
//! CredDeleteW, CredFree) and DPAPI (crypt32's CryptProtectData,
//! CryptUnprotectData); both DLLs forward here.
//!
//! The kernel keeps both under the signed-in user's master key (VEIL_CRED,
//! src/credentials.rs), so they open only while that user is signed in.
//! A credential goes to the kernel as one record, laid out here:
//!
//!   [flags u32] [type u32] [persist u32] [last written u64]
//!   [target] [comment] [alias] [user name]     strings: [units u32, or
//!                                              u32::MAX for NULL] [UTF-16]
//!   [blob length u32] [blob] [attribute count u32]
//!   then per attribute: [keyword] [flags u32] [value length u32] [value]
//!
//! A DPAPI blob holds [description units u32] [description] [data].
//! Errors are Windows': ERROR_NOT_FOUND, ERROR_INVALID_PARAMETER,
//! ERROR_INVALID_DATA for a blob that doesn't open (other key, other
//! entropy, damaged), ERROR_NO_SUCH_LOGON_SESSION with nobody signed in.

use crate::util::{alloc, free};
use crate::Bool;

#[link(name = "ntdll", kind = "raw-dylib")]
unsafe extern "system" {
    fn VeilCred(op: u64, a: u64, b: u64, c: u64) -> i32;
}

const ERROR_INVALID_PARAMETER: u32 = 87;
const ERROR_INVALID_FLAGS: u32 = 1004;
const ERROR_NOT_FOUND: u32 = 1168;
const RPC_X_BAD_STUB_DATA: u32 = 1783;
const BUFFER_TOO_SMALL: i32 = 0xC000_0023u32 as i32;
const CRED_PRESERVE_CREDENTIAL_BLOB: u32 = 1;
const CRED_ENUMERATE_ALL_CREDENTIALS: u32 = 1;
const CRED_TYPE_DOMAIN_PASSWORD: u32 = 2;
const CRED_TYPE_MAXIMUM: u32 = 7;
const CRED_PERSIST_ENTERPRISE: u32 = 3;
const CRED_MAX_CREDENTIAL_BLOB_SIZE: u32 = 5 * 512;
const CRED_MAX_ATTRIBUTES: u32 = 64;
const CRYPTPROTECT_LOCAL_MACHINE: u32 = 4;

#[repr(C)]
struct ProtectArgs {
    data: u64,
    data_len: u64,
    entropy: u64,
    entropy_len: u64,
    out: u64,
    out_cap: u64,
    out_len: u64,
    machine: u32,
    _pad: u32,
}

#[repr(C)]
struct CredKey {
    kind: u32,
    persist: u32,
    target: u64,
    target_len: u32,
    _pad: u32,
}

#[repr(C)]
struct CredOut {
    ptr: u64,
    cap: u64,
    len: u64,
}

/// CREDENTIALW.
#[repr(C)]
struct Credential {
    flags: u32,
    kind: u32,
    target: *mut u16,
    comment: *mut u16,
    last_written: u64,
    blob_size: u32,
    blob: *mut u8,
    persist: u32,
    attribute_count: u32,
    attributes: *mut Attribute,
    alias: *mut u16,
    user: *mut u16,
}

/// CREDENTIAL_ATTRIBUTEW.
#[repr(C)]
struct Attribute {
    keyword: *mut u16,
    flags: u32,
    value_size: u32,
    value: *mut u8,
}

/// DATA_BLOB.
#[repr(C)]
pub struct DataBlob {
    len: u32,
    data: *mut u8,
}

/// A growing byte buffer on the process heap.
struct Buf {
    p: *mut u8,
    len: usize,
    cap: usize,
}

impl Buf {
    fn new() -> Self {
        Buf { p: core::ptr::null_mut(), len: 0, cap: 0 }
    }
    fn push(&mut self, b: &[u8]) -> bool {
        if self.len + b.len() > self.cap {
            let cap = (self.len + b.len()).max(self.cap * 2).max(256);
            let p = alloc(cap);
            if p.is_null() {
                return false;
            }
            if !self.p.is_null() {
                unsafe { core::ptr::copy_nonoverlapping(self.p, p, self.len) };
                self.wipe();
                free(self.p);
            }
            self.p = p;
            self.cap = cap;
        }
        unsafe { core::ptr::copy_nonoverlapping(b.as_ptr(), self.p.add(self.len), b.len()) };
        self.len += b.len();
        true
    }
    fn u32(&mut self, v: u32) -> bool {
        self.push(&v.to_le_bytes())
    }
    fn wstr(&mut self, s: *const u16) -> bool {
        if s.is_null() {
            return self.u32(u32::MAX);
        }
        let w = unsafe { crate::util::wstr(s) };
        self.u32(w.len() as u32) && self.push(unsafe { core::slice::from_raw_parts(w.as_ptr() as *const u8, w.len() * 2) })
    }
    fn bytes(&self) -> &[u8] {
        if self.p.is_null() { &[] } else { unsafe { core::slice::from_raw_parts(self.p, self.len) } }
    }
    fn wipe(&mut self) {
        if !self.p.is_null() {
            unsafe { core::ptr::write_bytes(self.p, 0, self.cap) };
        }
    }
}

impl Drop for Buf {
    fn drop(&mut self) {
        if !self.p.is_null() {
            self.wipe();
            free(self.p);
        }
    }
}

/// Reads a record back.
struct Reader<'a> {
    b: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn u32(&mut self) -> Option<u32> {
        let v = self.b.get(self.at..self.at + 4)?;
        self.at += 4;
        Some(u32::from_le_bytes([v[0], v[1], v[2], v[3]]))
    }
    fn u64(&mut self) -> Option<u64> {
        Some(self.u32()? as u64 | (self.u32()? as u64) << 32)
    }
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let v = self.b.get(self.at..self.at + n)?;
        self.at += n;
        Some(v)
    }
    /// A string: its bytes (without NUL), or None inside for NULL.
    fn wstr(&mut self) -> Option<Option<&'a [u8]>> {
        let n = self.u32()?;
        if n == u32::MAX { Some(None) } else { Some(Some(self.take(n as usize * 2)?)) }
    }
}

/// Bump allocation inside one block, 8-aligned.
struct Place {
    p: *mut u8,
    at: usize,
}

impl Place {
    fn take(&mut self, n: usize) -> *mut u8 {
        let q = unsafe { self.p.add(self.at) };
        self.at += (n + 7) & !7;
        q
    }
    fn wstr(&mut self, s: Option<&[u8]>) -> *mut u16 {
        let Some(s) = s else { return core::ptr::null_mut() };
        let q = self.take(s.len() + 2);
        unsafe {
            core::ptr::copy_nonoverlapping(s.as_ptr(), q, s.len());
            *(q.add(s.len()) as *mut u16) = 0;
        }
        q as *mut u16
    }
    fn bytes(&mut self, s: &[u8]) -> *mut u8 {
        if s.is_empty() {
            return core::ptr::null_mut();
        }
        let q = self.take(s.len());
        unsafe { core::ptr::copy_nonoverlapping(s.as_ptr(), q, s.len()) };
        q
    }
}

/// What a record needs as a CREDENTIALW, and the CREDENTIALW built at `p`
/// (enough room from `record_size`).
fn record_size(rec: &[u8]) -> usize {
    // Every string grows by a NUL and each piece by up to 7 for alignment.
    let attrs = Reader { b: rec, at: 0 }.attribute_count().unwrap_or(0) as usize;
    (core::mem::size_of::<Credential>() + attrs * core::mem::size_of::<Attribute>() + rec.len() + (6 + 2 * attrs) * 10 + 7) & !7
}

impl<'a> Reader<'a> {
    fn attribute_count(&mut self) -> Option<u32> {
        self.at = 20;
        for _ in 0..4 {
            self.wstr()?;
        }
        let n = self.u32()?;
        self.take(n as usize)?;
        self.u32()
    }
}

fn build(rec: &[u8], p: *mut u8) -> Option<*mut Credential> {
    let mut r = Reader { b: rec, at: 0 };
    let flags = r.u32()?;
    let kind = r.u32()?;
    let persist = r.u32()?;
    let last_written = r.u64()?;
    let (target, comment, alias, user) = (r.wstr()?, r.wstr()?, r.wstr()?, r.wstr()?);
    let blob_len = r.u32()? as usize;
    let blob = r.take(blob_len)?;
    let count = r.u32()?;
    let mut place = Place { p, at: 0 };
    let c = place.take(core::mem::size_of::<Credential>()) as *mut Credential;
    let attrs = place.take(count as usize * core::mem::size_of::<Attribute>()) as *mut Attribute;
    for i in 0..count as usize {
        let keyword = r.wstr()?;
        let aflags = r.u32()?;
        let n = r.u32()? as usize;
        let value = r.take(n)?;
        unsafe {
            attrs.add(i).write(Attribute { keyword: place.wstr(keyword), flags: aflags, value_size: n as u32, value: place.bytes(value) });
        }
    }
    unsafe {
        c.write(Credential {
            flags,
            kind,
            target: place.wstr(target),
            comment: place.wstr(comment),
            last_written,
            blob_size: blob_len as u32,
            blob: place.bytes(blob),
            persist,
            attribute_count: count,
            attributes: if count == 0 { core::ptr::null_mut() } else { attrs },
            alias: place.wstr(alias),
            user: place.wstr(user),
        });
    }
    Some(c)
}

fn fail_with(code: u32) -> Bool {
    crate::SetLastError(code);
    0
}

fn key(target: *const u16, kind: u32, persist: u32) -> CredKey {
    let n = unsafe { crate::util::wstr(target) }.len();
    CredKey { kind, persist, target: target as u64, target_len: n as u32, _pad: 0 }
}

/// A credential's record from the kernel, in a block from `alloc` (with
/// its length), or the failing status.
fn fetch(op: u64, a: u64) -> Result<(*mut u8, usize), i32> {
    let mut cap = 1024usize;
    loop {
        let p = alloc(cap);
        if p.is_null() {
            return Err(nt_abi::status::NO_MEMORY);
        }
        let mut out = CredOut { ptr: p as u64, cap: cap as u64, len: 0 };
        let o = &mut out as *mut CredOut as u64;
        let s = unsafe { if op == 5 { VeilCred(5, o, 0, 0) } else { VeilCred(op, a, o, 0) } };
        if s == 0 {
            return Ok((p, out.len as usize));
        }
        unsafe { core::ptr::write_bytes(p, 0, cap) };
        free(p);
        if s != BUFFER_TOO_SMALL {
            return Err(s);
        }
        cap = out.len as usize;
    }
}

fn wipe_free(p: *mut u8, n: usize) {
    unsafe { core::ptr::write_bytes(p, 0, n) };
    free(p);
}

#[no_mangle]
pub unsafe extern "system" fn CredWriteW(c: *const Credential, flags: u32) -> Bool {
    if c.is_null() {
        return fail_with(ERROR_INVALID_PARAMETER);
    }
    let c = unsafe { &*c };
    if flags & !CRED_PRESERVE_CREDENTIAL_BLOB != 0 {
        return fail_with(ERROR_INVALID_FLAGS);
    }
    // Windows' RPC layer turns down a blob over the limit itself.
    if c.blob_size > CRED_MAX_CREDENTIAL_BLOB_SIZE {
        return fail_with(RPC_X_BAD_STUB_DATA);
    }
    if c.target.is_null() || c.kind == 0 || c.kind >= CRED_TYPE_MAXIMUM || c.persist == 0 || c.persist > CRED_PERSIST_ENTERPRISE
        || (c.blob_size != 0 && c.blob.is_null())
        || c.attribute_count > CRED_MAX_ATTRIBUTES || (c.attribute_count != 0 && c.attributes.is_null())
    {
        return fail_with(ERROR_INVALID_PARAMETER);
    }
    let k = key(c.target, c.kind, c.persist);
    if k.target_len == 0 || k.target_len > 32767 {
        return fail_with(ERROR_INVALID_PARAMETER);
    }
    // CRED_PRESERVE_CREDENTIAL_BLOB: the saved blob stays.
    let mut kept: Option<(*mut u8, usize)> = None;
    if flags & CRED_PRESERVE_CREDENTIAL_BLOB != 0 {
        match fetch(3, &k as *const CredKey as u64) {
            Ok(v) => kept = Some(v),
            Err(s) => return crate::fail(s),
        }
    }
    let mut now = 0u64;
    unsafe { crate::time::GetSystemTimeAsFileTime(&mut now) };
    let mut b = Buf::new();
    let mut ok = b.u32(c.flags) && b.u32(c.kind) && b.u32(c.persist) && b.u32(now as u32) && b.u32((now >> 32) as u32)
        && b.wstr(c.target) && b.wstr(c.comment) && b.wstr(c.alias) && b.wstr(c.user);
    match kept {
        Some((p, n)) => {
            let rec = unsafe { core::slice::from_raw_parts(p, n) };
            let mut r = Reader { b: rec, at: 20 };
            let blob = (|| {
                for _ in 0..4 {
                    r.wstr()?;
                }
                let n = r.u32()? as usize;
                r.take(n)
            })()
            .unwrap_or(&[]);
            ok = ok && b.u32(blob.len() as u32) && b.push(blob);
            wipe_free(p, n);
        }
        None => {
            let blob = if c.blob_size == 0 { &[][..] } else { unsafe { core::slice::from_raw_parts(c.blob, c.blob_size as usize) } };
            ok = ok && b.u32(c.blob_size) && b.push(blob);
        }
    }
    ok = ok && b.u32(c.attribute_count);
    for i in 0..c.attribute_count as usize {
        let a = unsafe { &*c.attributes.add(i) };
        let value = if a.value_size == 0 || a.value.is_null() { &[][..] } else { unsafe { core::slice::from_raw_parts(a.value, a.value_size as usize) } };
        ok = ok && b.wstr(a.keyword) && b.u32(a.flags) && b.u32(value.len() as u32) && b.push(value);
    }
    if !ok {
        return fail_with(8); // ERROR_NOT_ENOUGH_MEMORY
    }
    let bytes = b.bytes();
    done(unsafe { VeilCred(2, &k as *const CredKey as u64, bytes.as_ptr() as u64, bytes.len() as u64) })
}

/// The Cred* functions clear the last error when they work, as Windows'.
fn done(s: i32) -> Bool {
    if s == 0 {
        crate::SetLastError(0);
        1
    } else {
        crate::fail(s)
    }
}

#[no_mangle]
pub unsafe extern "system" fn CredReadW(target: *const u16, kind: u32, _flags: u32, out: *mut *mut Credential) -> Bool {
    // Windows keeps domain passwords from programs; it ignores the flags.
    if target.is_null() || out.is_null() || kind == 0 || kind >= CRED_TYPE_MAXIMUM || kind == CRED_TYPE_DOMAIN_PASSWORD {
        return fail_with(ERROR_INVALID_PARAMETER);
    }
    let k = key(target, kind, 0);
    let (p, n) = match fetch(3, &k as *const CredKey as u64) {
        Ok(v) => v,
        Err(s) => return crate::fail(s),
    };
    let rec = unsafe { core::slice::from_raw_parts(p, n) };
    let block = alloc(record_size(rec));
    let c = if block.is_null() { None } else { build(rec, block) };
    wipe_free(p, n);
    match c {
        Some(c) => {
            unsafe { *out = c };
            done(0)
        }
        None => {
            if !block.is_null() {
                free(block);
            }
            fail_with(13)
        }
    }
}

/// `name` against a CredEnumerate filter: the same, or for "prefix*" one
/// that starts with prefix. Case doesn't matter.
fn matches(filter: &[u16], name: &[u8]) -> bool {
    let lower = |u: u16| if (b'A' as u16..=b'Z' as u16).contains(&u) { u + 32 } else { u };
    // The record's UTF-16 needn't be 2-aligned: read it pair by pair.
    let units = name.len() / 2;
    let unit = |i: usize| u16::from_le_bytes([name[2 * i], name[2 * i + 1]]);
    let (pattern, prefix_only) = match filter.split_last() {
        Some((&star, prefix)) if star == b'*' as u16 => (prefix, true),
        _ => (filter, false),
    };
    (if prefix_only { units >= pattern.len() } else { units == pattern.len() })
        && pattern.iter().enumerate().all(|(i, &a)| lower(a) == lower(unit(i)))
}

#[no_mangle]
pub unsafe extern "system" fn CredEnumerateW(filter: *const u16, flags: u32, count: *mut u32, out: *mut *mut *mut Credential) -> Bool {
    if count.is_null() || out.is_null() || flags & !CRED_ENUMERATE_ALL_CREDENTIALS != 0 {
        return fail_with(if flags & !CRED_ENUMERATE_ALL_CREDENTIALS != 0 { ERROR_INVALID_FLAGS } else { ERROR_INVALID_PARAMETER });
    }
    if flags & CRED_ENUMERATE_ALL_CREDENTIALS != 0 && !filter.is_null() {
        return fail_with(ERROR_INVALID_PARAMETER);
    }
    let filter: Option<&[u16]> = if filter.is_null() { None } else { Some(unsafe { crate::util::wstr(filter) }) };
    let (p, n) = match fetch(5, 0) {
        Ok(v) => v,
        Err(s) => return crate::fail(s),
    };
    let all = unsafe { core::slice::from_raw_parts(p, n) };
    // Two passes over the records: count and size, then build.
    let each = |f: &mut dyn FnMut(&[u8])| {
        let mut r = Reader { b: all, at: 0 };
        while let Some(len) = r.u32() {
            let Some(rec) = r.take(len as usize) else { break };
            let mut t = Reader { b: rec, at: 20 };
            let Some(Some(target)) = t.wstr() else { continue };
            if filter.is_none_or(|f| matches(f, target)) {
                f(rec);
            }
        }
    };
    let (mut found, mut size) = (0usize, 0usize);
    each(&mut |rec| {
        found += 1;
        size += record_size(rec);
    });
    if found == 0 {
        wipe_free(p, n);
        return fail_with(ERROR_NOT_FOUND);
    }
    let table = found * 8;
    let block = alloc(table + size);
    if block.is_null() {
        wipe_free(p, n);
        return fail_with(8);
    }
    let (mut i, mut at) = (0usize, table);
    each(&mut |rec| {
        if let Some(c) = build(rec, unsafe { block.add(at) }) {
            unsafe { *(block as *mut *mut Credential).add(i) = c };
            i += 1;
        }
        at += record_size(rec);
    });
    wipe_free(p, n);
    unsafe {
        *count = i as u32;
        *out = block as *mut *mut Credential;
    }
    1
}

#[no_mangle]
pub unsafe extern "system" fn CredDeleteW(target: *const u16, kind: u32, _flags: u32) -> Bool {
    // Windows ignores the flags here too.
    if target.is_null() || kind == 0 || kind >= CRED_TYPE_MAXIMUM {
        return fail_with(ERROR_INVALID_PARAMETER);
    }
    let k = key(target, kind, 0);
    done(unsafe { VeilCred(4, &k as *const CredKey as u64, 0, 0) })
}

#[no_mangle]
pub extern "system" fn CredFree(p: *mut u8) {
    if !p.is_null() {
        free(p);
    }
}

/// Every type can be kept as long as CRED_PERSIST_ENTERPRISE.
#[no_mangle]
pub unsafe extern "system" fn CredGetSessionTypes(max: u32, persists: *mut u32) -> Bool {
    if persists.is_null() {
        return fail_with(ERROR_INVALID_PARAMETER);
    }
    for i in 0..max as usize {
        unsafe { *persists.add(i) = if i == 0 { 0 } else { CRED_PERSIST_ENTERPRISE } };
    }
    1
}

// -----| DPAPI |-----

fn blob_slice<'a>(b: *const DataBlob) -> Option<&'a [u8]> {
    if b.is_null() {
        return Some(&[]);
    }
    let b = unsafe { &*b };
    if b.len == 0 {
        Some(&[])
    } else if b.data.is_null() {
        None
    } else {
        Some(unsafe { core::slice::from_raw_parts(b.data, b.len as usize) })
    }
}

/// Seal or open `data` with the kernel; the result in a LocalAlloc block.
fn veil_protect(op: u64, data: &[u8], entropy: &[u8], machine: bool) -> Result<(*mut u8, usize), i32> {
    let mut cap = data.len() + 64;
    loop {
        let p = crate::mem::LocalAlloc(0, cap.max(1));
        if p.is_null() {
            return Err(nt_abi::status::NO_MEMORY);
        }
        let mut a = ProtectArgs {
            data: data.as_ptr() as u64,
            data_len: data.len() as u64,
            entropy: entropy.as_ptr() as u64,
            entropy_len: entropy.len() as u64,
            out: p as u64,
            out_cap: cap as u64,
            out_len: 0,
            machine: machine as u32,
            _pad: 0,
        };
        let s = unsafe { VeilCred(op, &mut a as *mut ProtectArgs as u64, 0, 0) };
        if s == 0 {
            return Ok((p, a.out_len as usize));
        }
        crate::mem::LocalFree(p);
        if s != BUFFER_TOO_SMALL {
            return Err(s);
        }
        cap = a.out_len as usize;
    }
}

#[no_mangle]
pub unsafe extern "system" fn CryptProtectData(input: *const DataBlob, description: *const u16, entropy: *const DataBlob,
                                               _reserved: usize, _prompt: usize, flags: u32, out: *mut DataBlob) -> Bool {
    let (Some(data), Some(ent)) = (if input.is_null() { None } else { blob_slice(input) }, blob_slice(entropy)) else {
        return fail_with(ERROR_INVALID_PARAMETER);
    };
    if out.is_null() {
        return fail_with(ERROR_INVALID_PARAMETER);
    }
    let mut b = Buf::new();
    let ok = if description.is_null() { b.u32(0) } else { b.wstr(description) } && b.push(data);
    if !ok {
        return fail_with(8);
    }
    match veil_protect(0, b.bytes(), ent, flags & CRYPTPROTECT_LOCAL_MACHINE != 0) {
        Ok((p, n)) => {
            unsafe { *out = DataBlob { len: n as u32, data: p } };
            1
        }
        Err(s) => crate::fail(s),
    }
}

#[no_mangle]
pub unsafe extern "system" fn CryptUnprotectData(input: *const DataBlob, description: *mut *mut u16, entropy: *const DataBlob,
                                                 _reserved: usize, _prompt: usize, _flags: u32, out: *mut DataBlob) -> Bool {
    let (Some(data), Some(ent)) = (if input.is_null() { None } else { blob_slice(input) }, blob_slice(entropy)) else {
        return fail_with(ERROR_INVALID_PARAMETER);
    };
    if out.is_null() {
        return fail_with(ERROR_INVALID_PARAMETER);
    }
    let (p, n) = match veil_protect(1, data, ent, false) {
        Ok(v) => v,
        Err(s) => return crate::fail(s),
    };
    let plain = unsafe { core::slice::from_raw_parts(p, n) };
    let mut r = Reader { b: plain, at: 0 };
    let Some(units) = r.u32() else {
        crate::mem::LocalFree(p);
        return fail_with(13);
    };
    let Some(desc) = r.take(units as usize * 2) else {
        crate::mem::LocalFree(p);
        return fail_with(13);
    };
    let rest = &plain[r.at..];
    let d = crate::mem::LocalAlloc(0, rest.len().max(1));
    let w = if description.is_null() { core::ptr::null_mut() } else { crate::mem::LocalAlloc(0, desc.len() + 2) };
    if d.is_null() || (!description.is_null() && w.is_null()) {
        wipe_free_local(p, n);
        return fail_with(8);
    }
    unsafe {
        core::ptr::copy_nonoverlapping(rest.as_ptr(), d, rest.len());
        if !w.is_null() {
            core::ptr::copy_nonoverlapping(desc.as_ptr(), w, desc.len());
            *(w.add(desc.len()) as *mut u16) = 0;
            *description = w as *mut u16;
        }
        *out = DataBlob { len: rest.len() as u32, data: d };
    }
    wipe_free_local(p, n);
    1
}

fn wipe_free_local(p: *mut u8, n: usize) {
    unsafe { core::ptr::write_bytes(p, 0, n) };
    crate::mem::LocalFree(p);
}
