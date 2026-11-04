//! The registry (WIN32_DESIGN.md): one tree of keys and values in the kernel,
//! shared by every Windows program and kept in `/etc/registry.dat`. Programs
//! reach it through kernel32's Reg* calls and the one syscall `VEIL_REG`.
//!
//! Handles hold a key's path rather than a pointer into the tree, so a key
//! deleted under an open handle just stops being found. There are no ACLs:
//! like the rest of VEILOS's Windows layer, this runs as one standard user.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use nt_abi::reg::{self, RegCall};
use nt_abi::NtStatus;
use spin::Mutex;

use crate::nt_file::ptr_ok;

const FILE: &str = "/etc/registry.dat";
const MAGIC: &[u8; 5] = b"VREG1";
const BAD_FILE: &str = "/etc/registry.bad";
/// Largest value or name accepted.
const MAX_DATA: usize = 1 << 20;
const MAX_NAME: usize = 16383;

const ERROR_SUCCESS: i32 = 0;
const ERROR_FILE_NOT_FOUND: i32 = 2;
const ERROR_ACCESS_DENIED: i32 = 5;
const ERROR_INVALID_HANDLE: i32 = 6;
const ERROR_INVALID_PARAMETER: i32 = 87;
const ERROR_MORE_DATA: i32 = 234;
const ERROR_NO_MORE_ITEMS: i32 = 259;
const ERROR_NOACCESS: i32 = 998;

const REG_SZ: u32 = 1;
const REG_DWORD: u32 = 4;

struct Value {
    name: Vec<u16>,
    kind: u32,
    data: Vec<u8>,
}

#[derive(Default)]
struct Key {
    name: Vec<u16>,
    values: Vec<Value>,
    subkeys: Vec<Key>,
}

fn fold(c: u16) -> u16 {
    if (b'a' as u16..=b'z' as u16).contains(&c) { c - 32 } else { c }
}

fn same(a: &[u16], b: &[u16]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| fold(*x) == fold(*y))
}

impl Key {
    fn child(&self, name: &[u16]) -> Option<&Key> {
        self.subkeys.iter().find(|k| same(&k.name, name))
    }
    fn child_mut(&mut self, name: &[u16]) -> Option<&mut Key> {
        self.subkeys.iter_mut().find(|k| same(&k.name, name))
    }
    /// A new child, keeping names sorted as Windows lists them.
    fn add_child(&mut self, name: &[u16]) -> &mut Key {
        let key = |n: &[u16]| n.iter().map(|&c| fold(c)).collect::<Vec<u16>>();
        let k = key(name);
        let at = self.subkeys.iter().position(|o| key(&o.name) > k).unwrap_or(self.subkeys.len());
        self.subkeys.insert(at, Key { name: name.to_vec(), ..Key::default() });
        &mut self.subkeys[at]
    }
    fn walk(&self, path: &[Vec<u16>]) -> Option<&Key> {
        let mut k = self;
        for p in path {
            k = k.child(p)?;
        }
        Some(k)
    }
    fn walk_mut(&mut self, path: &[Vec<u16>]) -> Option<&mut Key> {
        let mut k = self;
        for p in path {
            k = k.child_mut(p)?;
        }
        Some(k)
    }
    fn set(&mut self, name: &[u16], kind: u32, data: Vec<u8>) {
        match self.values.iter_mut().find(|v| same(&v.name, name)) {
            Some(v) => {
                v.kind = kind;
                v.data = data;
            }
            None => self.values.push(Value { name: name.to_vec(), kind, data }),
        }
    }
}

struct Handle {
    pid: u32,
    path: Vec<Vec<u16>>,
}

struct Registry {
    root: Key,
    handles: BTreeMap<u32, Handle>,
}

static REGISTRY: Mutex<Option<Registry>> = Mutex::new(None);
static NEXT_HANDLE: AtomicU32 = AtomicU32::new(0x1000);
static DIRTY: AtomicBool = AtomicBool::new(false);
static CHANGED_AT: AtomicU64 = AtomicU64::new(0);

fn w(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

/// `a\b\c` as path pieces.
fn split(p: &[u16]) -> Vec<Vec<u16>> {
    p.split(|&c| c == b'\\' as u16).filter(|s| !s.is_empty()).map(|s| s.to_vec()).collect()
}

// -----| what's there on a new system |-----

fn seed() -> Key {
    let mut root = Key::default();
    let mut put = |path: &str, vals: &[(&str, Val)]| {
        let mut k = &mut root;
        for piece in path.split('\\') {
            let n = w(piece);
            if k.child(&n).is_none() {
                k.add_child(&n);
            }
            k = k.child_mut(&n).unwrap();
        }
        for (name, v) in vals {
            match v {
                Val::S(s) => {
                    let mut bytes: Vec<u8> = s.encode_utf16().flat_map(|c| c.to_le_bytes()).collect();
                    bytes.extend_from_slice(&[0, 0]);
                    k.set(&w(name), REG_SZ, bytes);
                }
                Val::D(d) => k.set(&w(name), REG_DWORD, d.to_le_bytes().to_vec()),
            }
        }
    };
    use Val::{D, S};
    put(
        r"HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion",
        &[
            ("ProductName", S("VEILOS")),
            ("EditionID", S("Core")),
            ("InstallationType", S("Client")),
            ("CurrentVersion", S("6.3")),
            ("CurrentBuild", S("19041")),
            ("CurrentBuildNumber", S("19041")),
            ("CurrentMajorVersionNumber", D(10)),
            ("CurrentMinorVersionNumber", D(0)),
            ("UBR", D(0)),
            ("RegisteredOwner", S("user")),
            ("SystemRoot", S("C:\\system")),
        ],
    );
    put(
        r"HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion",
        &[("ProgramFilesDir", S("C:\\Program Files")), ("CommonFilesDir", S("C:\\Program Files\\Common Files"))],
    );
    put(r"HKLM\SOFTWARE\Classes\.txt", &[("", S("txtfile")), ("Content Type", S("text/plain"))]);
    put(r"HKLM\SOFTWARE\Classes\.exe", &[("", S("exefile")), ("Content Type", S("application/x-msdownload"))]);
    put(r"HKLM\SOFTWARE\Classes\txtfile", &[("", S("Text Document"))]);
    put(r"HKLM\SOFTWARE\Classes\exefile", &[("", S("Application"))]);
    put(r"HKLM\SYSTEM\CurrentControlSet\Control\ComputerName\ActiveComputerName", &[("ComputerName", S("VEILOS"))]);
    put(r"HKLM\SYSTEM\CurrentControlSet\Control\Session Manager\Environment", &[("Path", S("C:\\system")), ("TEMP", S("C:\\tmp")), ("TMP", S("C:\\tmp"))]);
    put(r"HKLM\HARDWARE\DESCRIPTION\System\CentralProcessor\0", &[("ProcessorNameString", S("VEILOS virtual processor"))]);
    put(r"HKU\.DEFAULT\Environment", &[("TEMP", S("C:\\tmp")), ("TMP", S("C:\\tmp"))]);
    put(r"HKU\.DEFAULT\Software", &[]);
    put(r"HKU\.DEFAULT\Control Panel\Desktop", &[("WheelScrollLines", S("3"))]);
    put(r"HKU\.DEFAULT\Control Panel\International", &[("LocaleName", S("en-US")), ("sList", S(","))]);
    put(r"HKCC\System", &[]);
    put(r"HKCC\Software", &[]);
    root
}

enum Val {
    S(&'static str),
    D(u32),
}

// -----| the file |-----

fn save_key(k: &Key, out: &mut Vec<u8>) {
    let put_str = |s: &[u16], out: &mut Vec<u8>| {
        out.extend_from_slice(&(s.len() as u32).to_le_bytes());
        for c in s {
            out.extend_from_slice(&c.to_le_bytes());
        }
    };
    put_str(&k.name, out);
    out.extend_from_slice(&(k.values.len() as u32).to_le_bytes());
    for v in &k.values {
        put_str(&v.name, out);
        out.extend_from_slice(&v.kind.to_le_bytes());
        out.extend_from_slice(&(v.data.len() as u32).to_le_bytes());
        out.extend_from_slice(&v.data);
    }
    out.extend_from_slice(&(k.subkeys.len() as u32).to_le_bytes());
    for s in &k.subkeys {
        save_key(s, out);
    }
}

/// A careful reader: a damaged file gives None, never a panic.
struct Reader<'a> {
    d: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn u32(&mut self) -> Option<u32> {
        let b = self.d.get(self.at..self.at + 4)?;
        self.at += 4;
        Some(u32::from_le_bytes(b.try_into().ok()?))
    }
    fn str(&mut self) -> Option<Vec<u16>> {
        let n = self.u32()? as usize;
        if n > MAX_NAME {
            return None;
        }
        let b = self.d.get(self.at..self.at + n * 2)?;
        self.at += n * 2;
        Some(b.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect())
    }
    fn key(&mut self, depth: u32) -> Option<Key> {
        if depth > 512 {
            return None;
        }
        let mut k = Key { name: self.str()?, ..Key::default() };
        for _ in 0..self.u32()? {
            let name = self.str()?;
            let kind = self.u32()?;
            let n = self.u32()? as usize;
            let data = self.d.get(self.at..self.at.checked_add(n)?)?.to_vec();
            self.at += n;
            k.values.push(Value { name, kind, data });
        }
        for _ in 0..self.u32()? {
            let s = self.key(depth + 1)?;
            k.subkeys.push(s);
        }
        Some(k)
    }
}

fn load() -> Key {
    if let Ok(d) = crate::vfs::VFS.read_file(FILE) {
        if d.len() > MAGIC.len() && &d[..MAGIC.len()] == MAGIC {
            let mut r = Reader { d: &d, at: MAGIC.len() };
            if let Some(k) = r.key(0) {
                return k;
            }
        }
        // Not thrown away: the defaults are about to take its place.
        let _ = crate::vfs::VFS.write_file(BAD_FILE, &d);
        crate::serial_println!("[REGISTRY] {} is damaged (kept as {}): starting from the defaults", FILE, BAD_FILE);
    }
    DIRTY.store(true, Ordering::Relaxed);
    seed()
}

/// From the main loop: write changes out once they've settled for a moment.
pub fn tick(ms: u64) {
    if !DIRTY.load(Ordering::Relaxed) {
        CHANGED_AT.store(ms, Ordering::Relaxed);
        return;
    }
    let since = CHANGED_AT.load(Ordering::Relaxed);
    if ms.wrapping_sub(since) < 2000 {
        return;
    }
    flush();
}

fn flush() {
    let bytes = {
        let Some(g) = REGISTRY.try_lock() else { return };
        let Some(r) = g.as_ref() else { return };
        let mut out = MAGIC.to_vec();
        save_key(&r.root, &mut out);
        out
    };
    DIRTY.store(false, Ordering::Relaxed);
    if crate::vfs::VFS.write_file(FILE, &bytes).is_err() {
        crate::serial_println!("[REGISTRY] couldn't write {}", FILE);
    }
}

/// A process's handles go with it.
pub fn process_exited(pid: u32) {
    if let Some(r) = REGISTRY.lock().as_mut() {
        r.handles.retain(|_, h| h.pid != pid);
    }
}

// -----| the syscall |-----

fn root_path(key: u64) -> Option<Vec<Vec<u16>>> {
    let p = match key as u32 {
        0x8000_0000 => r"HKLM\SOFTWARE\Classes",
        0x8000_0001 => r"HKU\.DEFAULT",
        0x8000_0002 => "HKLM",
        0x8000_0003 => "HKU",
        0x8000_0005 => "HKCC",
        _ => return None,
    };
    if key >> 32 != 0 && key >> 32 != 0xFFFF_FFFF {
        return None;
    }
    Some(split(&w(p)))
}

fn key_path(r: &Registry, pid: u32, key: u64) -> Option<Vec<Vec<u16>>> {
    if let Some(p) = root_path(key) {
        return Some(p);
    }
    r.handles.get(&(key as u32)).filter(|h| h.pid == pid).map(|h| h.path.clone())
}

fn read_str(p: u64, n: u32) -> Option<Vec<u16>> {
    if p == 0 || n == 0 {
        return Some(Vec::new());
    }
    if n as usize > MAX_NAME || !ptr_ok(p, n as usize * 2) {
        return None;
    }
    Some(unsafe { core::slice::from_raw_parts(p as *const u16, n as usize) }.to_vec())
}

fn write_name(c: &mut RegCall, name: &[u16]) -> i32 {
    // The length without a NUL; the buffer needs room for one too.
    c.out_name_len = name.len() as u32;
    if c.out_name == 0 {
        return ERROR_SUCCESS;
    }
    if (c.out_name_cap as usize) <= name.len() {
        return ERROR_MORE_DATA;
    }
    if !ptr_ok(c.out_name, (name.len() + 1) * 2) {
        return ERROR_NOACCESS;
    }
    unsafe {
        core::ptr::copy_nonoverlapping(name.as_ptr(), c.out_name as *mut u16, name.len());
        *(c.out_name as *mut u16).add(name.len()) = 0;
    }
    ERROR_SUCCESS
}

fn write_data(c: &mut RegCall, kind: u32, data: &[u8]) -> i32 {
    c.kind = kind;
    let room = c.data_len as usize;
    c.data_len = data.len() as u32;
    if c.data == 0 {
        return ERROR_SUCCESS;
    }
    if room < data.len() {
        return ERROR_MORE_DATA;
    }
    if !ptr_ok(c.data, data.len()) {
        return ERROR_NOACCESS;
    }
    unsafe { core::ptr::copy_nonoverlapping(data.as_ptr(), c.data as *mut u8, data.len()) };
    ERROR_SUCCESS
}

/// VEIL_REG(call*): a Win32 error code.
pub fn call(pid: u32, p: u64) -> NtStatus {
    if !ptr_ok(p, core::mem::size_of::<RegCall>()) {
        return ERROR_NOACCESS;
    }
    let mut c = unsafe { core::ptr::read_unaligned(p as *const RegCall) };
    let r = run(pid, &mut c);
    unsafe { core::ptr::write_unaligned(p as *mut RegCall, c) };
    r
}

fn changed() {
    DIRTY.store(true, Ordering::Relaxed);
    CHANGED_AT.store(crate::time::now_ms(), Ordering::Relaxed);
}

fn run(pid: u32, c: &mut RegCall) -> i32 {
    let mut guard = REGISTRY.lock();
    if guard.is_none() {
        *guard = Some(Registry { root: load(), handles: BTreeMap::new() });
    }
    let r = guard.as_mut().unwrap();
    if c.op == reg::CLOSE {
        if root_path(c.key).is_some() {
            return ERROR_SUCCESS;
        }
        return match r.handles.get(&(c.key as u32)) {
            Some(h) if h.pid == pid => {
                r.handles.remove(&(c.key as u32));
                ERROR_SUCCESS
            }
            _ => ERROR_INVALID_HANDLE,
        };
    }
    let Some(base) = key_path(r, pid, c.key) else { return ERROR_INVALID_HANDLE };
    let Some(name) = read_str(c.name, c.name_len) else { return ERROR_NOACCESS };
    match c.op {
        reg::OPEN | reg::CREATE => {
            let mut path = base;
            path.extend(split(&name));
            if c.op == reg::CREATE {
                // Every missing piece along the way.
                let mut k = &mut r.root;
                let mut created = false;
                for piece in &path {
                    if k.child(piece).is_none() {
                        k.add_child(piece);
                        created = true;
                    }
                    k = k.child_mut(piece).unwrap();
                }
                c.flags = if created { 1 } else { 2 };
                if created {
                    changed();
                }
            } else if r.root.walk(&path).is_none() {
                return ERROR_FILE_NOT_FOUND;
            }
            let h = NEXT_HANDLE.fetch_add(4, Ordering::Relaxed);
            r.handles.insert(h, Handle { pid, path });
            c.out_key = h as u64;
            ERROR_SUCCESS
        }
        reg::QUERY_VALUE => {
            let Some(k) = r.root.walk(&base) else { return ERROR_INVALID_HANDLE };
            match k.values.iter().find(|v| same(&v.name, &name)) {
                Some(v) => {
                    let (kind, data) = (v.kind, v.data.clone());
                    write_data(c, kind, &data)
                }
                None => ERROR_FILE_NOT_FOUND,
            }
        }
        reg::SET_VALUE => {
            if c.data_len as usize > MAX_DATA || (c.data_len != 0 && !ptr_ok(c.data, c.data_len as usize)) {
                return ERROR_INVALID_PARAMETER;
            }
            let data = if c.data_len == 0 { Vec::new() } else { unsafe { core::slice::from_raw_parts(c.data as *const u8, c.data_len as usize) }.to_vec() };
            let Some(k) = r.root.walk_mut(&base) else { return ERROR_INVALID_HANDLE };
            k.set(&name, c.kind, data);
            changed();
            ERROR_SUCCESS
        }
        reg::DELETE_VALUE => {
            let Some(k) = r.root.walk_mut(&base) else { return ERROR_INVALID_HANDLE };
            let before = k.values.len();
            k.values.retain(|v| !same(&v.name, &name));
            if k.values.len() == before {
                return ERROR_FILE_NOT_FOUND;
            }
            changed();
            ERROR_SUCCESS
        }
        // `flags` 1: with whatever's under it.
        reg::DELETE_KEY => {
            let mut path = base;
            path.extend(split(&name));
            let Some(last) = path.pop() else { return ERROR_ACCESS_DENIED };
            if path.is_empty() {
                return ERROR_ACCESS_DENIED; // A hive.
            }
            let Some(parent) = r.root.walk_mut(&path) else { return ERROR_FILE_NOT_FOUND };
            let Some(i) = parent.subkeys.iter().position(|k| same(&k.name, &last)) else { return ERROR_FILE_NOT_FOUND };
            if !parent.subkeys[i].subkeys.is_empty() && c.flags & 1 == 0 {
                return ERROR_ACCESS_DENIED;
            }
            parent.subkeys.remove(i);
            changed();
            ERROR_SUCCESS
        }
        reg::ENUM_KEY => {
            let Some(k) = r.root.walk(&base) else { return ERROR_INVALID_HANDLE };
            match k.subkeys.get(c.index as usize) {
                Some(s) => {
                    let n = s.name.clone();
                    write_name(c, &n)
                }
                None => ERROR_NO_MORE_ITEMS,
            }
        }
        reg::ENUM_VALUE => {
            let Some(k) = r.root.walk(&base) else { return ERROR_INVALID_HANDLE };
            match k.values.get(c.index as usize) {
                Some(v) => {
                    let (n, kind, data) = (v.name.clone(), v.kind, v.data.clone());
                    let e = write_name(c, &n);
                    if e != ERROR_SUCCESS {
                        return e;
                    }
                    write_data(c, kind, &data)
                }
                None => ERROR_NO_MORE_ITEMS,
            }
        }
        reg::INFO => {
            let Some(k) = r.root.walk(&base) else { return ERROR_INVALID_HANDLE };
            c.info = [
                k.subkeys.len() as u32,
                k.subkeys.iter().map(|s| s.name.len()).max().unwrap_or(0) as u32,
                k.values.len() as u32,
                k.values.iter().map(|v| v.name.len()).max().unwrap_or(0) as u32,
                k.values.iter().map(|v| v.data.len()).max().unwrap_or(0) as u32,
                0,
            ];
            ERROR_SUCCESS
        }
        reg::RENAME => {
            let Some(new) = read_str(c.data, c.data_len) else { return ERROR_NOACCESS };
            let mut path = base;
            path.extend(split(&name));
            let Some(last) = path.pop() else { return ERROR_ACCESS_DENIED };
            if path.is_empty() || new.is_empty() {
                return ERROR_ACCESS_DENIED;
            }
            let Some(parent) = r.root.walk_mut(&path) else { return ERROR_FILE_NOT_FOUND };
            if parent.child(&new).is_some() {
                return ERROR_ACCESS_DENIED;
            }
            let Some(i) = parent.subkeys.iter().position(|k| same(&k.name, &last)) else { return ERROR_FILE_NOT_FOUND };
            let mut old = parent.subkeys.remove(i);
            old.name = new.clone();
            let slot = parent.add_child(&new);
            *slot = old;
            changed();
            ERROR_SUCCESS
        }
        reg::FLUSH => {
            drop(guard);
            if DIRTY.load(Ordering::Relaxed) {
                flush();
            }
            ERROR_SUCCESS
        }
        _ => ERROR_INVALID_PARAMETER,
    }
}
