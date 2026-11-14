/*
 * VEILOS Central Encryption Node (CEN)
 * ====================================
 *
 * The CEN is a kernel-level, centralised, and standardised encryption service 
 * designed to prevent future OS features from re-implementing cryptography 
 * independently. It provides a unified API for key lifecycle management, 
 * authenticated encryption, and secure key persistence.
 *
 * Use Cases:
 *  - Encrypted Clipboard: transient keys for secure copy-paste.
 *  - Drive Encryption: persistent high-sensitivity keys for block devices.
 *  - File Encryption: per-file or per-directory cryptographic isolation.
 *
 * Architecture:
 *  - Algorithms: AES-256-GCM (Standard/High) and ChaCha20-Poly1305 (Maximum).
 *  - KDF: PBKDF2-HMAC-SHA256 with security-level-dependent iteration counts.
 *  - Lifecycle: Keys are generated, stored in an in-memory registry, and 
 *    optionally persisted to an encrypted ledger file on disk.
 *  - Ledger Model: One .vkl file per user and FeatureTag
 *    (/etc/keys/<user>/<feature>.vkl). Keys at rest are encrypted with the
 *    user's Master Key, itself kept encrypted by their sign-in password
 *    (Section 8), so saved keys are readable only while they're signed in.
 *  - Randomness: keys, salts and nonces come from the kernel's pool
 *    (entropy.rs).
 *  - Security Levels: Standard, High, and Maximum tiers for balanced performance.
 */

#![allow(dead_code)]

extern crate alloc;

use alloc::vec::Vec;
use alloc::vec;
use alloc::string::String;
use alloc::collections::BTreeMap;
use alloc::format;
use core::sync::atomic::{AtomicU64, Ordering};
use core::fmt;

use spin::Mutex;
use lazy_static::lazy_static;
use zeroize::ZeroizeOnDrop;

use aes_gcm::{Aes256Gcm, KeyInit, Nonce, aead::Aead};
use orion::hazardous::aead::chacha20poly1305 as orion_chacha20;
use pbkdf2::pbkdf2_hmac;

use sha2::{Sha256, Sha512, Digest};

use crate::capability::Capability;
use crate::println;

// =====================================================================
// -----| Section 1 — Constants |-----
// =====================================================================

const AES_KEY_LEN: usize = 32;
const AES_NONCE_LEN: usize = 12;
const CHACHA_KEY_LEN: usize = 32;
const CHACHA_NONCE_LEN: usize = 12;

const PBKDF2_ROUNDS_STANDARD: u32 = 100_000;
const PBKDF2_ROUNDS_HIGH: u32 = 250_000;
const PBKDF2_ROUNDS_MAXIMUM: u32 = 600_000;

const KDF_SALT_LEN: usize = 32;

/// Root directory for all key ledger files.
const LEDGER_DIR: &str = "/etc/keys/";
/// Magic header written at the start of every .vkl ledger file.
const LEDGER_MAGIC: &[u8; 4] = b"VKLY";
const LEDGER_VERSION: u8 = 2;

/// Compile-time salt used for master key derivation.
const COMPILE_TIME_SALT: &[u8; 32] = b"VEILOS_CEN_PROTECTION_SALT_V1_00";

// =====================================================================
// -----| Section 2 — Enums and Types |-----
// =====================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecurityLevel {
    /// AES-256-GCM, PBKDF2_ROUNDS_STANDARD. Everyday use.
    Standard,
    /// AES-256-GCM, PBKDF2_ROUNDS_HIGH. Sensitive data.
    High,
    /// ChaCha20-Poly1305, PBKDF2_ROUNDS_MAXIMUM. Full drive encryption.
    Maximum,
}

/// An opaque string tag identifying which OS feature owns a key.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct FeatureTag(String);

impl FeatureTag {
    pub fn new(name: &str) -> Self { FeatureTag(alloc::string::ToString::to_string(name)) }
    pub fn as_str(&self) -> &str { &self.0 }
    
    /// Derives the VFS path for this feature's key ledger file.
    pub fn ledger_path(&self) -> String {
        format!("{}{}.vkl", LEDGER_DIR, self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CipherAlgorithm {
    Aes256Gcm,
    ChaCha20Poly1305,
}

impl CipherAlgorithm {
    pub fn for_level(level: SecurityLevel) -> Self {
        match level {
            SecurityLevel::Standard | SecurityLevel::High => CipherAlgorithm::Aes256Gcm,
            SecurityLevel::Maximum => CipherAlgorithm::ChaCha20Poly1305,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HashAlgorithm {
    Sha256,
    Sha512,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct KeyHandle(pub u64);

impl fmt::Display for KeyHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "KH-{}", self.0)
    }
}

#[derive(ZeroizeOnDrop)]
pub struct KeyMaterial {
    bytes: Vec<u8>
}

impl KeyMaterial {
    pub fn new(bytes: Vec<u8>) -> Self { Self { bytes } }
    pub fn as_bytes(&self) -> &[u8] { &self.bytes }
}

pub struct KeyEntry {
    pub handle: KeyHandle,
    /// Random, kept in the ledger: which saved key this is across boots.
    pub id: u64,
    pub material: KeyMaterial,
    pub algorithm: CipherAlgorithm,
    pub level: SecurityLevel,
    pub feature: FeatureTag,
    pub created_tick: u64,
    pub persisted: bool,
}

#[derive(Debug)]
pub enum CenError {
    CapabilityDenied,
    KeyNotFound(KeyHandle),
    EncryptionFailed,
    DecryptionFailed,
    KdfFailed,
    LedgerIo(&'static str),
    InvalidLedger,
    KeyAlreadyPersisted(KeyHandle),
}

impl fmt::Display for CenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CapabilityDenied => write!(f, "cen: capability denied"),
            Self::KeyNotFound(h) => write!(f, "cen: key not found ({})", h),
            Self::EncryptionFailed => write!(f, "cen: encryption failed"),
            Self::DecryptionFailed => write!(f, "cen: decryption failed"),
            Self::KdfFailed => write!(f, "cen: key derivation failed"),
            Self::LedgerIo(s) => write!(f, "cen: ledger I/O error: {}", s),
            Self::InvalidLedger => write!(f, "cen: invalid ledger format"),
            Self::KeyAlreadyPersisted(h) => write!(f, "cen: key {} already persisted", h),
        }
    }
}

// =====================================================================
// -----| Section 3 — Global State |-----
// =====================================================================

lazy_static! {
    static ref KEY_REGISTRY: Mutex<BTreeMap<KeyHandle, KeyEntry>> = Mutex::new(BTreeMap::new());
}

/// The signed-in user's master key, which encrypts their ledgers, and whose
/// it is. None until someone signs in (`cen_unlock_user`).
static MASTER: Mutex<Option<([u8; 32], String)>> = Mutex::new(None);

static KEY_HANDLE_COUNTER: AtomicU64 = AtomicU64::new(1);
static CEN_TICK: AtomicU64 = AtomicU64::new(0);

fn random<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    crate::entropy::fill(&mut b);
    b
}

// =====================================================================
// -----| Section 4 — Initialisation |-----
// =====================================================================

/// Nothing to set up at boot: the master key arrives at sign-in.
pub fn cen_init() {
    println!("cen_init: Central Encryption Node online.");
}

// =====================================================================
// -----| Section 5 — Key Generation |-----
// =====================================================================

/// Generates a new key for a specific feature: random bytes from the kernel's
/// pool (entropy.rs), or derived from `password` with a random salt.
pub fn cen_generate_key(feature: FeatureTag, level: SecurityLevel, password: Option<&[u8]>) -> Result<KeyHandle, CenError> {
    // Check ManageKeys capability
    if let Some(proc) = crate::process::PROCESSOR.lock().get_active() {
        if !proc.capabilities.lock().has(Capability::ManageKeys) {
            return Err(CenError::CapabilityDenied);
        }
    }
    Ok(generate(feature, level, password))
}

fn generate(feature: FeatureTag, level: SecurityLevel, password: Option<&[u8]>) -> KeyHandle {
    let current_tick = CEN_TICK.fetch_add(1, Ordering::SeqCst);
    let algo = CipherAlgorithm::for_level(level);

    let bytes = if let Some(pw) = password {
        let salt: [u8; KDF_SALT_LEN] = random();
        let rounds = match level {
            SecurityLevel::Standard => PBKDF2_ROUNDS_STANDARD,
            SecurityLevel::High => PBKDF2_ROUNDS_HIGH,
            SecurityLevel::Maximum => PBKDF2_ROUNDS_MAXIMUM,
        };
        let mut derived = [0u8; 32];
        pbkdf2_hmac::<Sha256>(pw, &salt, rounds, &mut derived);
        derived.to_vec()
    } else {
        random::<32>().to_vec()
    };

    let handle = KeyHandle(KEY_HANDLE_COUNTER.fetch_add(1, Ordering::SeqCst));
    let entry = KeyEntry {
        handle,
        id: u64::from_le_bytes(random()),
        material: KeyMaterial::new(bytes),
        algorithm: algo,
        level,
        feature,
        created_tick: current_tick,
        persisted: false,
    };

    KEY_REGISTRY.lock().insert(handle, entry);
    handle
}

// =====================================================================
// -----| Section 6 — Encrypt / Decrypt |-----
// =====================================================================

pub fn cen_encrypt(handle: KeyHandle, plaintext: &[u8], aad: Option<&[u8]>) -> Result<Vec<u8>, CenError> {
    if let Some(proc) = crate::process::PROCESSOR.lock().get_active() {
        if !proc.capabilities.lock().has(Capability::EncryptData) {
            return Err(CenError::CapabilityDenied);
        }
    }

    let registry = KEY_REGISTRY.lock();
    let entry = registry.get(&handle).ok_or(CenError::KeyNotFound(handle))?;

    // A random nonce: a counter would repeat after a reboot with saved keys.
    let nonce_bytes: [u8; 12] = random();
    
    let aad_data = aad.unwrap_or(&[]);
    let mut output = nonce_bytes.to_vec();

    match entry.algorithm {
        CipherAlgorithm::Aes256Gcm => {
            let cipher = Aes256Gcm::new_from_slice(entry.material.as_bytes()).map_err(|_| CenError::EncryptionFailed)?;
            let nonce = Nonce::from_slice(&nonce_bytes);
            let ciphertext = cipher.encrypt(nonce, aes_gcm::aead::Payload { msg: plaintext, aad: aad_data })
                .map_err(|_| CenError::EncryptionFailed)?;
            output.extend(ciphertext);
        },
        CipherAlgorithm::ChaCha20Poly1305 => {
            let key = orion_chacha20::SecretKey::from_slice(entry.material.as_bytes())
                .map_err(|_| CenError::EncryptionFailed)?;
            let nonce = orion_chacha20::Nonce::from_slice(&nonce_bytes)
                .map_err(|_| CenError::EncryptionFailed)?;
            let mut dst_out = vec![0u8; plaintext.len() + 16];
            orion_chacha20::seal(&key, &nonce, plaintext, aad, &mut dst_out)
                .map_err(|_| CenError::EncryptionFailed)?;
            output.extend(dst_out);
        }
    }

    Ok(output)
}

pub fn cen_decrypt(handle: KeyHandle, ciphertext: &[u8], aad: Option<&[u8]>) -> Result<Vec<u8>, CenError> {
    if let Some(proc) = crate::process::PROCESSOR.lock().get_active() {
        if !proc.capabilities.lock().has(Capability::DecryptData) {
            return Err(CenError::CapabilityDenied);
        }
    }

    if ciphertext.len() < 12 { return Err(CenError::DecryptionFailed); }

    let registry = KEY_REGISTRY.lock();
    let entry = registry.get(&handle).ok_or(CenError::KeyNotFound(handle))?;

    let nonce_bytes = &ciphertext[0..12];
    let actual_ciphertext = &ciphertext[12..];
    let aad_data = aad.unwrap_or(&[]);

    match entry.algorithm {
        CipherAlgorithm::Aes256Gcm => {
            let cipher = Aes256Gcm::new_from_slice(entry.material.as_bytes()).map_err(|_| CenError::DecryptionFailed)?;
            let nonce = Nonce::from_slice(nonce_bytes);
            cipher.decrypt(nonce, aes_gcm::aead::Payload { msg: actual_ciphertext, aad: aad_data })
                .map_err(|_| CenError::DecryptionFailed)
        },
        CipherAlgorithm::ChaCha20Poly1305 => {
            if actual_ciphertext.len() < 16 {
                return Err(CenError::DecryptionFailed);
            }
            let key = orion_chacha20::SecretKey::from_slice(entry.material.as_bytes())
                .map_err(|_| CenError::DecryptionFailed)?;
            let nonce = orion_chacha20::Nonce::from_slice(nonce_bytes)
                .map_err(|_| CenError::DecryptionFailed)?;
            let mut dst_out = vec![0u8; actual_ciphertext.len() - 16];
            orion_chacha20::open(&key, &nonce, actual_ciphertext, aad, &mut dst_out)
                .map_err(|_| CenError::DecryptionFailed)?;
            Ok(dst_out)
        }
    }
}

// =====================================================================
// -----| Section 7 — Hashing |-----
// =====================================================================

pub fn cen_hash(data: &[u8], algorithm: HashAlgorithm) -> Vec<u8> {
    match algorithm {
        HashAlgorithm::Sha256 => {
            let mut h = Sha256::new();
            h.update(data);
            h.finalize().to_vec()
        },
        HashAlgorithm::Sha512 => {
            let mut h = Sha512::new();
            h.update(data);
            h.finalize().to_vec()
        }
    }
}

// =====================================================================
// -----| Section 8 — Key Persistence |-----
// =====================================================================
//
// Each user has a random master key, kept in /etc/keys/<user>/master.vmk
// encrypted with a key derived from their sign-in password (PBKDF2-SHA256,
// a random salt). It is unwrapped at sign-in and forgotten at sign-out, so
// a user's saved keys can only be read while they're signed in.
//
//   master.vmk:  "VMK1" [salt 32] [rounds u32] [nonce 12] [AES-GCM(master) 48]
//   <feature>.vkl: "VKLY" [version 2], then records:
//       [payload_len u32] [nonce 12] [AES-GCM(master, payload)]
//   payload: [id u64] [algorithm u8] [level u8] [feature_len u8] [feature]
//            [created_tick u64] [key_len u16] [key]
//
// Nonces are random: handles start again at every boot, so they can't be.

const MASTER_MAGIC: &[u8; 4] = b"VMK1";
const MASTER_FILE: &str = "master.vmk";

fn user_dir(user: &str) -> String {
    format!("{}{}/", LEDGER_DIR, user)
}

fn ledger_path(user: &str, feature: &FeatureTag) -> String {
    format!("{}{}.vkl", user_dir(user), feature.as_str())
}

fn seal(key: &[u8; 32], plain: &[u8]) -> Option<Vec<u8>> {
    let nonce: [u8; 12] = random();
    let cipher = Aes256Gcm::new_from_slice(key).ok()?;
    let mut out = nonce.to_vec();
    out.extend(cipher.encrypt(Nonce::from_slice(&nonce), plain).ok()?);
    Some(out)
}

fn open(key: &[u8; 32], sealed: &[u8]) -> Option<Vec<u8>> {
    if sealed.len() < 12 + 16 {
        return None;
    }
    let cipher = Aes256Gcm::new_from_slice(key).ok()?;
    cipher.decrypt(Nonce::from_slice(&sealed[..12]), &sealed[12..]).ok()
}

fn wrap_key(password: &str, salt: &[u8], rounds: u32) -> [u8; 32] {
    let mut k = [0u8; 32];
    pbkdf2_hmac::<Sha256>(password.as_bytes(), salt, rounds, &mut k);
    k
}

/// Write `master` to the user's master.vmk, wrapped by `password`.
fn write_master(user: &str, master: &[u8; 32], password: &str) -> Result<(), CenError> {
    let vfs = &crate::vfs::VFS;
    let _ = vfs.create_dir(LEDGER_DIR.trim_end_matches('/'));
    let _ = vfs.create_dir(user_dir(user).trim_end_matches('/'));
    let salt: [u8; KDF_SALT_LEN] = random();
    let rounds = PBKDF2_ROUNDS_STANDARD;
    let mut wk = wrap_key(password, &salt, rounds);
    let sealed = seal(&wk, master).ok_or(CenError::EncryptionFailed);
    wk.iter_mut().for_each(|b| *b = 0);
    let mut file = MASTER_MAGIC.to_vec();
    file.extend_from_slice(&salt);
    file.extend_from_slice(&rounds.to_le_bytes());
    file.extend(sealed?);
    vfs.write_file(&format!("{}{}", user_dir(user), MASTER_FILE), &file)
        .map_err(|_| CenError::LedgerIo("master key write failed"))
}

/// The user's master key from master.vmk, if `password` opens it.
fn read_master(user: &str, password: &str) -> Option<Result<[u8; 32], CenError>> {
    let file = crate::vfs::VFS.read_file(&format!("{}{}", user_dir(user), MASTER_FILE)).ok()?;
    if file.len() < 4 + KDF_SALT_LEN + 4 || &file[..4] != MASTER_MAGIC {
        return Some(Err(CenError::InvalidLedger));
    }
    let salt = &file[4..4 + KDF_SALT_LEN];
    let at = 4 + KDF_SALT_LEN;
    let rounds = u32::from_le_bytes([file[at], file[at + 1], file[at + 2], file[at + 3]]);
    let mut wk = wrap_key(password, salt, rounds);
    let plain = open(&wk, &file[at + 4..]);
    wk.iter_mut().for_each(|b| *b = 0);
    Some(match plain {
        Some(p) if p.len() == 32 => {
            let mut m = [0u8; 32];
            m.copy_from_slice(&p);
            Ok(m)
        }
        _ => Err(CenError::DecryptionFailed),
    })
}

/// At sign-in: unwrap (or, the first time, create) the user's master key
/// and load every key they saved. Returns how many keys were loaded.
pub fn cen_unlock_user(user: &str, password: &str) -> Result<usize, CenError> {
    let master = match read_master(user, password) {
        Some(Ok(m)) => m,
        Some(Err(e)) => {
            // Not this password's: the password was reset by an admin, which
            // can't carry the old key over. Start again; the old ledgers
            // stay on disk, unreadable, until cen_forget_user removes them.
            crate::serial_println!("[CEN] {}: saved master key unreadable ({}); a new one is made", user, e);
            let m: [u8; 32] = random();
            write_master(user, &m, password)?;
            m
        }
        None => {
            let m: [u8; 32] = random();
            write_master(user, &m, password)?;
            crate::serial_println!("[CEN] {}: new master key", user);
            m
        }
    };
    *MASTER.lock() = Some((master, String::from(user)));

    let mut loaded = 0;
    if let Ok(entries) = crate::vfs::VFS.list_dir(user_dir(user).trim_end_matches('/')) {
        for e in entries {
            let lower = e.name.to_ascii_lowercase();
            if let Some(name) = lower.strip_suffix(".vkl") {
                if let Ok(keys) = load_ledger(user, &FeatureTag::new(name)) {
                    loaded += keys.len();
                }
            }
        }
    }
    crate::serial_println!("[CEN] {}: unlocked, {} saved key(s) loaded", user, loaded);
    Ok(loaded)
}

/// At sign-out: forget the master key and every saved key in memory.
pub fn cen_lock_user() {
    let had = MASTER.lock().take();
    if let Some((mut m, user)) = had {
        m.iter_mut().for_each(|b| *b = 0);
        KEY_REGISTRY.lock().retain(|_, e| !e.persisted);
        crate::serial_println!("[CEN] {}: locked", user);
    }
}

/// After a password change: wrap the user's master key with the new
/// password. The old one opens it if the user isn't signed in.
pub fn cen_change_password(user: &str, old_password: &str, new_password: &str) -> Result<(), CenError> {
    let current = MASTER.lock().as_ref().filter(|(_, u)| u == user).map(|(m, _)| *m);
    let master = match current {
        Some(m) => m,
        None => match read_master(user, old_password) {
            Some(Ok(m)) => m,
            Some(Err(e)) => return Err(e),
            None => return Ok(()), // nothing saved yet
        },
    };
    write_master(user, &master, new_password)
}

/// An admin reset the user's password: their saved keys can't be opened
/// any more, so remove them.
pub fn cen_forget_user(user: &str) {
    let dir = user_dir(user);
    if let Ok(entries) = crate::vfs::VFS.list_dir(dir.trim_end_matches('/')) {
        for e in entries {
            let _ = crate::vfs::VFS.delete_file(&format!("{}{}", dir, e.name));
        }
    }
    crate::serial_println!("[CEN] {}: saved keys removed (password reset)", user);
}

fn payload_of(entry: &KeyEntry) -> Vec<u8> {
    let mut p = Vec::new();
    p.extend(&entry.id.to_le_bytes());
    p.push(entry.algorithm as u8);
    p.push(entry.level as u8);
    p.push(entry.feature.as_str().len() as u8);
    p.extend(entry.feature.as_str().as_bytes());
    p.extend(&entry.created_tick.to_le_bytes());
    p.extend(&(entry.material.as_bytes().len() as u16).to_le_bytes());
    p.extend(entry.material.as_bytes());
    p
}

/// Save a key to the signed-in user's ledger for its feature.
pub fn cen_persist_key(handle: KeyHandle) -> Result<(), CenError> {
    if let Some(proc) = crate::process::PROCESSOR.lock().get_active() {
        if !proc.capabilities.lock().has(Capability::WriteKeyLedger) {
            return Err(CenError::CapabilityDenied);
        }
    }
    let (master, user) = MASTER.lock().clone().ok_or(CenError::LedgerIo("nobody is signed in"))?;
    let mut registry = KEY_REGISTRY.lock();
    let entry = registry.get_mut(&handle).ok_or(CenError::KeyNotFound(handle))?;
    if entry.persisted {
        return Err(CenError::KeyAlreadyPersisted(handle));
    }
    let payload = payload_of(entry);
    let sealed = seal(&master, &payload).ok_or(CenError::EncryptionFailed)?;
    let mut record = Vec::new();
    record.extend(&(payload.len() as u32).to_le_bytes());
    record.extend(sealed);

    let path = ledger_path(&user, &entry.feature);
    let vfs = &crate::vfs::VFS;
    let _ = vfs.create_dir(LEDGER_DIR.trim_end_matches('/'));
    let _ = vfs.create_dir(user_dir(&user).trim_end_matches('/'));
    let mut file = match vfs.read_file(&path) {
        Ok(f) if f.len() >= 5 && &f[..4] == LEDGER_MAGIC && f[4] == LEDGER_VERSION => f,
        _ => {
            let mut f = LEDGER_MAGIC.to_vec();
            f.push(LEDGER_VERSION);
            f
        }
    };
    file.extend(record);
    vfs.write_file(&path, &file).map_err(|_| CenError::LedgerIo("VFS write failed"))?;
    entry.persisted = true;
    crate::serial_println!("[CEN] key {} saved to {}", handle, path);
    Ok(())
}

/// The records of a ledger: (record bytes, decrypted payload), in order.
fn ledger_records(master: &[u8; 32], file: &[u8]) -> Result<Vec<(core::ops::Range<usize>, Vec<u8>)>, CenError> {
    if file.len() < 5 || &file[..4] != LEDGER_MAGIC || file[4] != LEDGER_VERSION {
        return Err(CenError::InvalidLedger);
    }
    let mut out = Vec::new();
    let mut at = 5;
    while at + 4 <= file.len() {
        let len = u32::from_le_bytes([file[at], file[at + 1], file[at + 2], file[at + 3]]) as usize;
        let end = at + 4 + 12 + len + 16;
        if end > file.len() {
            return Err(CenError::InvalidLedger);
        }
        let payload = open(master, &file[at + 4..end]).ok_or(CenError::DecryptionFailed)?;
        out.push((at..end, payload));
        at = end;
    }
    Ok(out)
}

fn entry_from_payload(p: &[u8]) -> Option<KeyEntry> {
    let id = u64::from_le_bytes(p.get(0..8)?.try_into().ok()?);
    let algorithm = match *p.get(8)? { 0 => CipherAlgorithm::Aes256Gcm, 1 => CipherAlgorithm::ChaCha20Poly1305, _ => return None };
    let level = match *p.get(9)? { 0 => SecurityLevel::Standard, 1 => SecurityLevel::High, 2 => SecurityLevel::Maximum, _ => return None };
    let flen = *p.get(10)? as usize;
    let feature = core::str::from_utf8(p.get(11..11 + flen)?).ok()?;
    let at = 11 + flen;
    let created_tick = u64::from_le_bytes(p.get(at..at + 8)?.try_into().ok()?);
    let klen = u16::from_le_bytes(p.get(at + 8..at + 10)?.try_into().ok()?) as usize;
    let key = p.get(at + 10..at + 10 + klen)?.to_vec();
    Some(KeyEntry {
        handle: KeyHandle(KEY_HANDLE_COUNTER.fetch_add(1, Ordering::SeqCst)),
        id,
        material: KeyMaterial::new(key),
        algorithm,
        level,
        feature: FeatureTag::new(feature),
        created_tick,
        persisted: true,
    })
}

fn load_ledger(user: &str, feature: &FeatureTag) -> Result<Vec<KeyHandle>, CenError> {
    let master = MASTER.lock().as_ref().map(|(m, _)| *m).ok_or(CenError::LedgerIo("nobody is signed in"))?;
    let file = match crate::vfs::VFS.read_file(&ledger_path(user, feature)) {
        Ok(f) => f,
        Err(_) => return Ok(Vec::new()),
    };
    let mut registry = KEY_REGISTRY.lock();
    let mut handles = Vec::new();
    for (_, payload) in ledger_records(&master, &file)? {
        let Some(entry) = entry_from_payload(&payload) else { return Err(CenError::InvalidLedger) };
        // Already loaded (sign-in twice without a sign-out): keep that one.
        if let Some(e) = registry.values().find(|e| e.persisted && e.id == entry.id) {
            handles.push(e.handle);
            continue;
        }
        handles.push(entry.handle);
        registry.insert(entry.handle, entry);
    }
    Ok(handles)
}

/// The signed-in user's saved keys for `feature`, loaded (or already loaded
/// at sign-in), oldest first.
pub fn cen_load_keys(feature: FeatureTag) -> Result<Vec<KeyHandle>, CenError> {
    if let Some(proc) = crate::process::PROCESSOR.lock().get_active() {
        if !proc.capabilities.lock().has(Capability::ReadKeyLedger) {
            return Err(CenError::CapabilityDenied);
        }
    }
    let user = MASTER.lock().as_ref().map(|(_, u)| u.clone()).ok_or(CenError::LedgerIo("nobody is signed in"))?;
    load_ledger(&user, &feature)
}

// =====================================================================
// -----| Section 9 — Revocation |-----
// =====================================================================

pub fn cen_revoke_key(handle: KeyHandle) -> Result<(), CenError> {
    if let Some(proc) = crate::process::PROCESSOR.lock().get_active() {
        if !proc.capabilities.lock().has(Capability::ManageKeys) {
            return Err(CenError::CapabilityDenied);
        }
    }

    let entry = KEY_REGISTRY.lock().remove(&handle).ok_or(CenError::KeyNotFound(handle))?;
    if entry.persisted {
        purge(&entry)?;
    }
    println!("cen: key {} revoked and zeroed.", handle);
    Ok(())
}

/// Take a saved key's record out of its ledger.
fn purge(entry: &KeyEntry) -> Result<(), CenError> {
    let (master, user) = MASTER.lock().clone().ok_or(CenError::LedgerIo("nobody is signed in"))?;
    let path = ledger_path(&user, &entry.feature);
    let file = crate::vfs::VFS.read_file(&path).map_err(|_| CenError::LedgerIo("ledger read failed"))?;
    let mut kept = file[..5].to_vec();
    for (range, payload) in ledger_records(&master, &file)? {
        if payload.get(0..8) != Some(&entry.id.to_le_bytes()[..]) {
            kept.extend_from_slice(&file[range]);
        }
    }
    crate::vfs::VFS.write_file(&path, &kept).map_err(|_| CenError::LedgerIo("VFS write failed"))
}

/// Remove a saved key from its ledger (and from memory).
pub fn cen_purge_ledger(_feature: FeatureTag, handle: KeyHandle) -> Result<(), CenError> {
    cen_revoke_key(handle)
}

// =====================================================================
// -----| Section 10 — Inspection |-----
// =====================================================================

pub fn cen_list_keys(feature: Option<&FeatureTag>) -> Vec<KeyHandle> {
    let registry = KEY_REGISTRY.lock();
    registry.values()
        .filter(|e| feature.map_or(true, |f| &e.feature == f))
        .map(|e| e.handle)
        .collect()
}

pub fn cen_key_info(handle: KeyHandle) -> Option<(FeatureTag, SecurityLevel, CipherAlgorithm, bool)> {
    let registry = KEY_REGISTRY.lock();
    registry.get(&handle).map(|e| (e.feature.clone(), e.level, e.algorithm, e.persisted))
}
