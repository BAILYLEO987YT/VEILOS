// -----| Saved credentials and DPAPI for Win32 programs |-----
//
// VEIL_CRED(op, a, b, c): what advapi32's Cred* functions and crypt32's
// CryptProtectData / CryptUnprotectData stand on (kernel32 cred.rs).
//
// Both use the signed-in user's master key (encryption.rs Section 8), which
// is unwrapped by their sign-in password: nothing here can be read while
// they're signed out, by another user, or from the disk on its own.
//
//   DPAPI    op 0 seals bytes, op 1 opens them: AES-256-GCM under a key made
//            from the master key (or, for CRYPTPROTECT_LOCAL_MACHINE, the
//            machine's key, /etc/keys/machine.key, which any user's program
//            may use, as on Windows), the caller's entropy as associated
//            data. kernel32 puts the description inside.
//   Cred*    op 2 writes, 3 reads, 4 deletes, 5 lists. Credentials are kept
//            here as kernel32 lays them out (a record), keyed by type and
//            target name (case doesn't matter). Those that last
//            (CRED_PERSIST_LOCAL_MACHINE, _ENTERPRISE) are written to
//            /etc/keys/<user>/credentials.vcr sealed by the master key;
//            CRED_PERSIST_SESSION ones are only in memory and go at sign-out.
//
//   credentials.vcr: "VCR1", then AES-GCM(master) of records
//       [type u32] [persist u32] [target_len u32] [target UTF-16] [len u32] [record]
//
// No user signed in: STATUS_NO_SUCH_LOGON_SESSION (ERROR_NO_SUCH_LOGON_SESSION).

use alloc::string::String;
use alloc::vec::Vec;
use spin::Mutex;

use nt_abi::{status, NtStatus};

use crate::nt::file::ptr_ok;
use crate::sched::process::Process;

const NO_SUCH_LOGON_SESSION: NtStatus = 0xC000_005Fu32 as i32;
const NOT_FOUND: NtStatus = 0xC000_0225u32 as i32;
const UNSUCCESSFUL: NtStatus = 0xC000_0001u32 as i32;
/// STATUS_INVALID_PARAMETER_2 stands for "this blob doesn't open"
/// (kernel32 answers ERROR_INVALID_DATA, as Windows does).
const BAD_DATA: NtStatus = 0xC000_00F0u32 as i32;

const FILE_NAME: &str = "credentials.vcr";
const MAGIC: &[u8; 4] = b"VCR1";
const PERSIST_SESSION: u32 = 1;
/// The most a user may keep (Windows' limit is per logon session too).
const MAX_CREDS: usize = 1024;
const MAX_RECORD: usize = 64 * 1024;

/// VEIL_CRED ops 0 and 1.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ProtectArgs {
    pub data: u64,
    pub data_len: u64,
    pub entropy: u64,
    pub entropy_len: u64,
    pub out: u64,
    pub out_cap: u64,
    /// Out: the bytes written, or needed (BUFFER_TOO_SMALL).
    pub out_len: u64,
    /// 1: the machine's key (CRYPTPROTECT_LOCAL_MACHINE).
    pub machine: u32,
    pub _pad: u32,
}

/// Names a credential (ops 2-4).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct CredKey {
    pub kind: u32,
    pub persist: u32,
    pub target: u64,
    /// UTF-16 units.
    pub target_len: u32,
    pub _pad: u32,
}

/// Where a read or list puts its bytes (ops 3 and 5).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct CredOut {
    pub ptr: u64,
    pub cap: u64,
    pub len: u64,
}

struct Cred {
    kind: u32,
    persist: u32,
    target: Vec<u16>,
    record: Vec<u8>,
}

struct Store {
    user: String,
    creds: Vec<Cred>,
}

/// The signed-in user's credentials, loaded at first use.
static STORE: Mutex<Option<Store>> = Mutex::new(None);

/// At sign-out (cen_lock_user): forget them, session ones for good.
pub fn forget() {
    if let Some(mut s) = STORE.lock().take() {
        for c in s.creds.iter_mut() {
            c.record.iter_mut().for_each(|b| *b = 0);
        }
    }
}

fn lower(u: u16) -> u16 {
    if (b'A' as u16..=b'Z' as u16).contains(&u) { u + 32 } else { u }
}

fn same_name(a: &[u16], b: &[u16]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| lower(*x) == lower(*y))
}

fn path(user: &str) -> String {
    alloc::format!("{}{}", crate::security::encryption::keys_dir(user), FILE_NAME)
}

fn load(master: &[u8; 32], user: &str) -> Vec<Cred> {
    let Ok(file) = crate::fs::vfs::VFS.read_file(&path(user)) else { return Vec::new() };
    if file.len() < 4 || &file[..4] != MAGIC {
        crate::serial_println!("[CRED] {}: {} isn't a credentials file", user, FILE_NAME);
        return Vec::new();
    }
    let Some(body) = crate::security::encryption::open_aad(master, &file[4..], MAGIC) else {
        crate::serial_println!("[CRED] {}: saved credentials don't open", user);
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut at = 0;
    let u32_at = |at: usize| body.get(at..at + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
    while at < body.len() {
        let (Some(kind), Some(persist), Some(tl)) = (u32_at(at), u32_at(at + 4), u32_at(at + 8)) else { break };
        let t_end = at + 12 + tl as usize * 2;
        let Some(t) = body.get(at + 12..t_end) else { break };
        let target = t.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        let Some(rl) = u32_at(t_end) else { break };
        let Some(record) = body.get(t_end + 4..t_end + 4 + rl as usize) else { break };
        out.push(Cred { kind, persist, target, record: record.to_vec() });
        at = t_end + 4 + rl as usize;
    }
    out
}

fn save(master: &[u8; 32], s: &Store) -> NtStatus {
    let mut body = Vec::new();
    for c in s.creds.iter().filter(|c| c.persist != PERSIST_SESSION) {
        body.extend(&c.kind.to_le_bytes());
        body.extend(&c.persist.to_le_bytes());
        body.extend(&(c.target.len() as u32).to_le_bytes());
        for u in &c.target {
            body.extend(&u.to_le_bytes());
        }
        body.extend(&(c.record.len() as u32).to_le_bytes());
        body.extend(&c.record);
    }
    let Some(sealed) = crate::security::encryption::seal_aad(master, &body, MAGIC) else {
        return UNSUCCESSFUL;
    };
    body.iter_mut().for_each(|b| *b = 0);
    let mut file = MAGIC.to_vec();
    file.extend(sealed);
    let vfs = &crate::fs::vfs::VFS;
    let dir = crate::security::encryption::keys_dir(&s.user);
    let _ = vfs.create_dir("/etc/keys");
    let _ = vfs.create_dir(dir.trim_end_matches('/'));
    match vfs.write_file(&path(&s.user), &file) {
        Ok(_) => status::SUCCESS,
        Err(_) => {
            crate::serial_println!("[CRED] {}: credentials not saved", s.user);
            UNSUCCESSFUL
        }
    }
}

/// Run `f` on the signed-in user's store (loaded if it isn't), with their
/// master key.
fn with_store(f: impl FnOnce(&[u8; 32], &mut Store) -> NtStatus) -> NtStatus {
    let Some((master, user)) = crate::security::encryption::signed_in() else { return NO_SUCH_LOGON_SESSION };
    let mut g = STORE.lock();
    if g.as_ref().is_none_or(|s| s.user != user) {
        let creds = load(&master, &user);
        *g = Some(Store { user, creds });
    }
    f(&master, g.as_mut().unwrap())
}

/// The machine's DPAPI key, made the first time it's asked for.
fn machine_key() -> Option<[u8; 32]> {
    const PATH: &str = "/etc/keys/machine.key";
    static KEY: Mutex<Option<[u8; 32]>> = Mutex::new(None);
    let mut k = KEY.lock();
    if k.is_none() {
        match crate::fs::vfs::VFS.read_file(PATH) {
            Ok(b) if b.len() == 32 => {
                let mut m = [0u8; 32];
                m.copy_from_slice(&b);
                *k = Some(m);
            }
            _ => {
                let mut m = [0u8; 32];
                crate::security::entropy::fill(&mut m);
                let _ = crate::fs::vfs::VFS.create_dir("/etc/keys");
                crate::fs::vfs::VFS.write_file(PATH, &m).ok()?;
                *k = Some(m);
            }
        }
    }
    *k
}

/// The DPAPI key: SHA-256 of a label and the user's (or machine's) key, so
/// it isn't the key the user's other secrets are sealed with.
fn dpapi_key(base: &[u8; 32]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(b"VEILOS DPAPI 1");
    h.update(base);
    h.finalize().into()
}

unsafe fn bytes<'a>(p: u64, n: u64) -> Option<&'a [u8]> {
    if n == 0 {
        return Some(&[]);
    }
    if n > 16 << 20 || !ptr_ok(p, n as usize) {
        return None;
    }
    Some(unsafe { core::slice::from_raw_parts(p as *const u8, n as usize) })
}

/// Copy `data` out to (ptr, cap), setting len; BUFFER_TOO_SMALL with the
/// size needed when it doesn't fit.
fn give(ptr: u64, cap: u64, len: &mut u64, data: &[u8]) -> NtStatus {
    *len = data.len() as u64;
    if (data.len() as u64) > cap {
        return status::BUFFER_TOO_SMALL;
    }
    if !ptr_ok(ptr, data.len()) {
        return status::ACCESS_VIOLATION;
    }
    unsafe { core::ptr::copy_nonoverlapping(data.as_ptr(), ptr as *mut u8, data.len()) };
    status::SUCCESS
}

fn protect(a: u64, seal: bool) -> NtStatus {
    if !ptr_ok(a, core::mem::size_of::<ProtectArgs>()) {
        return status::ACCESS_VIOLATION;
    }
    let args = unsafe { &mut *(a as *mut ProtectArgs) };
    let (Some(data), Some(entropy)) = (unsafe { bytes(args.data, args.data_len) }, unsafe { bytes(args.entropy, args.entropy_len) }) else {
        return status::ACCESS_VIOLATION;
    };
    let base = if args.machine != 0 {
        match machine_key() {
            Some(k) => k,
            None => return UNSUCCESSFUL,
        }
    } else {
        match crate::security::encryption::signed_in() {
            Some((m, _)) => m,
            None => return NO_SUCH_LOGON_SESSION,
        }
    };
    let key = dpapi_key(&base);
    // A blob says which key: 0 the user's, 1 the machine's.
    let result = if seal {
        crate::security::encryption::seal_aad(&key, data, entropy).map(|s| {
            let mut v = alloc::vec![args.machine as u8 & 1];
            v.extend(s);
            v
        })
    } else {
        match data.split_first() {
            Some((&which, rest)) if which as u32 == args.machine & 1 => crate::security::encryption::open_aad(&key, rest, entropy),
            Some((1, rest)) => machine_key().and_then(|k| crate::security::encryption::open_aad(&dpapi_key(&k), rest, entropy)),
            _ => None,
        }
    };
    let Some(mut out) = result else { return if seal { UNSUCCESSFUL } else { BAD_DATA } };
    let s = give(args.out, args.out_cap, &mut args.out_len, &out);
    out.iter_mut().for_each(|b| *b = 0);
    s
}

fn key_of(a: u64) -> Result<(CredKey, Vec<u16>), NtStatus> {
    if !ptr_ok(a, core::mem::size_of::<CredKey>()) {
        return Err(status::ACCESS_VIOLATION);
    }
    let k = unsafe { *(a as *const CredKey) };
    let n = k.target_len as usize;
    if n == 0 || n > 32767 || !ptr_ok(k.target, n * 2) {
        return Err(status::INVALID_PARAMETER);
    }
    let target = unsafe { core::slice::from_raw_parts(k.target as *const u16, n) }.to_vec();
    Ok((k, target))
}

/// VEIL_CRED(op, a, b, c): see the top of this file and nt_abi.
pub fn call(_proc: &Process, op: u64, a: u64, b: u64, c: u64) -> NtStatus {
    match op {
        0 => protect(a, true),
        1 => protect(a, false),
        // Write: a = CredKey*, b = record, c = its length.
        2 => {
            let (k, target) = match key_of(a) {
                Ok(v) => v,
                Err(s) => return s,
            };
            if c as usize > MAX_RECORD {
                return status::INVALID_PARAMETER;
            }
            let Some(record) = (unsafe { bytes(b, c) }).map(|r| r.to_vec()) else { return status::ACCESS_VIOLATION };
            with_store(|master, s| {
                let lasting = match s.creds.iter().position(|x| x.kind == k.kind && same_name(&x.target, &target)) {
                    Some(i) => {
                        let was = s.creds[i].persist != PERSIST_SESSION;
                        s.creds[i] = Cred { kind: k.kind, persist: k.persist, target, record };
                        was || k.persist != PERSIST_SESSION
                    }
                    None => {
                        if s.creds.len() >= MAX_CREDS {
                            return status::NO_MEMORY;
                        }
                        s.creds.push(Cred { kind: k.kind, persist: k.persist, target, record });
                        k.persist != PERSIST_SESSION
                    }
                };
                if lasting { save(master, s) } else { status::SUCCESS }
            })
        }
        // Read: a = CredKey*, b = CredOut*.
        3 => {
            let (k, target) = match key_of(a) {
                Ok(v) => v,
                Err(s) => return s,
            };
            if !ptr_ok(b, core::mem::size_of::<CredOut>()) {
                return status::ACCESS_VIOLATION;
            }
            let out = unsafe { &mut *(b as *mut CredOut) };
            with_store(|_, s| match s.creds.iter().find(|x| x.kind == k.kind && same_name(&x.target, &target)) {
                Some(x) => give(out.ptr, out.cap, &mut out.len, &x.record),
                None => NOT_FOUND,
            })
        }
        // Delete: a = CredKey*.
        4 => {
            let (k, target) = match key_of(a) {
                Ok(v) => v,
                Err(s) => return s,
            };
            with_store(|master, s| match s.creds.iter().position(|x| x.kind == k.kind && same_name(&x.target, &target)) {
                Some(i) => {
                    let gone = s.creds.remove(i);
                    if gone.persist != PERSIST_SESSION { save(master, s) } else { status::SUCCESS }
                }
                None => NOT_FOUND,
            })
        }
        // List: a = CredOut*, every record as [len u32][record].
        5 => {
            if !ptr_ok(a, core::mem::size_of::<CredOut>()) {
                return status::ACCESS_VIOLATION;
            }
            let out = unsafe { &mut *(a as *mut CredOut) };
            with_store(|_, s| {
                let mut all = Vec::new();
                for x in &s.creds {
                    all.extend(&(x.record.len() as u32).to_le_bytes());
                    all.extend(&x.record);
                }
                let r = give(out.ptr, out.cap, &mut out.len, &all);
                all.iter_mut().for_each(|b| *b = 0);
                r
            })
        }
        _ => status::INVALID_PARAMETER,
    }
}
