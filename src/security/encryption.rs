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

use crate::security::capability::Capability;
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
    crate::security::entropy::fill(&mut b);
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
    if let Some(proc) = crate::sched::process::PROCESSOR.lock().get_active() {
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
    if let Some(proc) = crate::sched::process::PROCESSOR.lock().get_active() {
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
    if let Some(proc) = crate::sched::process::PROCESSOR.lock().get_active() {
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
// in KEY SLOTS: independent copies of it, each wrapped by a different
// secret (ACCOUNTS_DESIGN.md, section 4):
//
//   sign-in    the sign-in password (PBKDF2-SHA256, random salt). A password
//              change rewrites only this slot.
//   recovery   a one-time recovery key shown to the user (optional).
//   system     the system key, /etc/keys/system.key (the "admin slot",
//              optional; the only slot of an account with no password).
//              An admin password reset re-wraps from it instead of removing
//              the keys.
//
// It is unwrapped at sign-in and forgotten at sign-out, so a user's saved
// keys can only be read while they're signed in. It is never replaced
// silently: a slot that doesn't open leaves the file as it was.
//
//   master.vmk v2: "VMK2" [count u8], then per slot:
//       [kind u8] [id_len u8] [id] [salt 32] [rounds u32] [len u32]
//       [nonce 12] [AES-GCM(master) 48]
//   master.vmk v1: "VMK1" [salt 32] [rounds u32] [nonce 12] [AES-GCM(master)]
//       (read as one sign-in slot; rewritten as v2 at the next change)
//   <feature>.vkl: "VKLY" [version 2], then records:
//       [payload_len u32] [nonce 12] [AES-GCM(master, payload)]
//   payload: [id u64] [algorithm u8] [level u8] [feature_len u8] [feature]
//            [created_tick u64] [key_len u16] [key]
//
// Nonces are random: handles start again at every boot, so they can't be.

const MASTER_MAGIC_V1: &[u8; 4] = b"VMK1";
const MASTER_MAGIC: &[u8; 4] = b"VMK2";
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

/// `seal`/`open` with associated data (credentials.rs: DPAPI's entropy).
pub(crate) fn seal_aad(key: &[u8; 32], plain: &[u8], aad: &[u8]) -> Option<Vec<u8>> {
    use aes_gcm::aead::Payload;
    let nonce: [u8; 12] = random();
    let cipher = Aes256Gcm::new_from_slice(key).ok()?;
    let mut out = nonce.to_vec();
    out.extend(cipher.encrypt(Nonce::from_slice(&nonce), Payload { msg: plain, aad }).ok()?);
    Some(out)
}

pub(crate) fn open_aad(key: &[u8; 32], sealed: &[u8], aad: &[u8]) -> Option<Vec<u8>> {
    use aes_gcm::aead::Payload;
    if sealed.len() < 12 + 16 {
        return None;
    }
    let cipher = Aes256Gcm::new_from_slice(key).ok()?;
    cipher.decrypt(Nonce::from_slice(&sealed[..12]), Payload { msg: &sealed[12..], aad }).ok()
}

/// The signed-in user's master key and name (credentials.rs), or None.
pub(crate) fn signed_in() -> Option<([u8; 32], String)> {
    MASTER.lock().clone()
}

/// The folder of `user`'s keys, with a trailing slash.
pub(crate) fn keys_dir(user: &str) -> String {
    user_dir(user)
}

fn wrap_key(password: &str, salt: &[u8], rounds: u32) -> [u8; 32] {
    let mut k = [0u8; 32];
    pbkdf2_hmac::<Sha256>(password.as_bytes(), salt, rounds, &mut k);
    k
}

/// The system key (the admin slot's secret): 32 random bytes, made the
/// first time an account needs it.
const SYSTEM_KEY_PATH: &str = "/etc/keys/system.key";
/// The sign-in slot's id: the password of the first sign-in step.
const PASSWORD_SLOT: &str = "password";

/// What opens a key slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlotKind {
    /// A sign-in secret.
    SignIn,
    /// The user's recovery key.
    Recovery,
    /// The system key (admin slot).
    System,
}

impl SlotKind {
    fn byte(self) -> u8 {
        match self { SlotKind::SignIn => 1, SlotKind::Recovery => 2, SlotKind::System => 3 }
    }
    fn from_byte(b: u8) -> Option<Self> {
        match b { 1 => Some(SlotKind::SignIn), 2 => Some(SlotKind::Recovery), 3 => Some(SlotKind::System), _ => None }
    }
}

/// One wrapped copy of the master key.
#[derive(Clone)]
struct Slot {
    kind: SlotKind,
    id: String,
    salt: [u8; KDF_SALT_LEN],
    rounds: u32,
    sealed: Vec<u8>,
}

/// What unlocks a user's saved keys at sign-in.
#[derive(Clone, Copy, Debug)]
pub enum Unlock<'a> {
    /// The sign-in secret (the password).
    Secret(&'a str),
    /// The system key: an account with no password.
    System,
}

fn master_path(user: &str) -> String {
    format!("{}{}", user_dir(user), MASTER_FILE)
}

/// The system key, made (and saved) if `create` and there is none.
fn system_key(create: bool) -> Option<[u8; 32]> {
    let vfs = &crate::fs::vfs::VFS;
    if let Ok(k) = vfs.read_file(SYSTEM_KEY_PATH) {
        if k.len() == 32 {
            let mut key = [0u8; 32];
            key.copy_from_slice(&k);
            return Some(key);
        }
        crate::serial_println!("[CEN] {} is damaged; not replaced", SYSTEM_KEY_PATH);
        return None;
    }
    if !create {
        return None;
    }
    let key: [u8; 32] = random();
    let _ = vfs.create_dir(LEDGER_DIR.trim_end_matches('/'));
    vfs.write_file(SYSTEM_KEY_PATH, &key).ok()?;
    crate::serial_println!("[CEN] system key made");
    Some(key)
}

/// A new slot holding `master`, wrapped by `secret`.
fn make_slot(kind: SlotKind, id: &str, master: &[u8; 32], secret: &[u8]) -> Result<Slot, CenError> {
    let salt: [u8; KDF_SALT_LEN] = random();
    // The system key is already 32 random bytes: one round is enough.
    let rounds = if kind == SlotKind::System { 1 } else { PBKDF2_ROUNDS_STANDARD };
    let mut wk = [0u8; 32];
    pbkdf2_hmac::<Sha256>(secret, &salt, rounds, &mut wk);
    let sealed = seal(&wk, master).ok_or(CenError::EncryptionFailed);
    wk.iter_mut().for_each(|b| *b = 0);
    Ok(Slot { kind, id: String::from(id), salt, rounds, sealed: sealed? })
}

/// The master key, if `secret` opens `slot`.
fn open_slot(slot: &Slot, secret: &[u8]) -> Option<[u8; 32]> {
    let mut wk = [0u8; 32];
    pbkdf2_hmac::<Sha256>(secret, &slot.salt, slot.rounds, &mut wk);
    let plain = open(&wk, &slot.sealed);
    wk.iter_mut().for_each(|b| *b = 0);
    match plain {
        Some(p) if p.len() == 32 => {
            let mut m = [0u8; 32];
            m.copy_from_slice(&p);
            Some(m)
        }
        _ => None,
    }
}

/// Try every slot of `kind` with `secret`.
fn open_kind(slots: &[Slot], kind: SlotKind, secret: &[u8]) -> Option<[u8; 32]> {
    slots.iter().filter(|s| s.kind == kind).find_map(|s| open_slot(s, secret))
}

/// The master key through the system slot, if there is one and the system
/// key opens it.
fn open_system(slots: &[Slot]) -> Option<[u8; 32]> {
    if !slots.iter().any(|s| s.kind == SlotKind::System) {
        return None;
    }
    let key = system_key(false)?;
    open_kind(slots, SlotKind::System, &key)
}

/// The user's slots: None if they have no master key file; Err if it is
/// damaged (it is then left alone). The bool: the file is v1.
fn read_slots(user: &str) -> Option<Result<(Vec<Slot>, bool), CenError>> {
    let file = crate::fs::vfs::VFS.read_file(&master_path(user)).ok()?;
    let u32_at = |at: usize| -> Option<u32> {
        Some(u32::from_le_bytes([*file.get(at)?, *file.get(at + 1)?, *file.get(at + 2)?, *file.get(at + 3)?]))
    };
    if file.len() >= 4 && &file[..4] == MASTER_MAGIC_V1 {
        if file.len() < 4 + KDF_SALT_LEN + 4 {
            return Some(Err(CenError::InvalidLedger));
        }
        let mut salt = [0u8; KDF_SALT_LEN];
        salt.copy_from_slice(&file[4..4 + KDF_SALT_LEN]);
        let at = 4 + KDF_SALT_LEN;
        let rounds = u32_at(at).unwrap_or(0);
        let slot = Slot { kind: SlotKind::SignIn, id: String::from(PASSWORD_SLOT), salt, rounds, sealed: file[at + 4..].to_vec() };
        return Some(Ok((vec![slot], true)));
    }
    if file.len() < 5 || &file[..4] != MASTER_MAGIC {
        return Some(Err(CenError::InvalidLedger));
    }
    let mut slots = Vec::new();
    let mut at = 5;
    for _ in 0..file[4] {
        let parse = || -> Option<(Slot, usize)> {
            let kind = SlotKind::from_byte(*file.get(at)?)?;
            let id_len = *file.get(at + 1)? as usize;
            let id = core::str::from_utf8(file.get(at + 2..at + 2 + id_len)?).ok()?;
            let mut p = at + 2 + id_len;
            let mut salt = [0u8; KDF_SALT_LEN];
            salt.copy_from_slice(file.get(p..p + KDF_SALT_LEN)?);
            p += KDF_SALT_LEN;
            let rounds = u32_at(p)?;
            let len = u32_at(p + 4)? as usize;
            p += 8;
            let sealed = file.get(p..p + len)?.to_vec();
            Some((Slot { kind, id: String::from(id), salt, rounds, sealed }, p + len))
        };
        match parse() {
            Some((slot, next)) => {
                slots.push(slot);
                at = next;
            }
            None => return Some(Err(CenError::InvalidLedger)),
        }
    }
    Some(Ok((slots, false)))
}

/// Write the user's slots (v2), replacing the file.
fn write_slots(user: &str, slots: &[Slot]) -> Result<(), CenError> {
    let vfs = &crate::fs::vfs::VFS;
    let _ = vfs.create_dir(LEDGER_DIR.trim_end_matches('/'));
    let _ = vfs.create_dir(user_dir(user).trim_end_matches('/'));
    let mut file = MASTER_MAGIC.to_vec();
    file.push(slots.len() as u8);
    for s in slots {
        file.push(s.kind.byte());
        file.push(s.id.len() as u8);
        file.extend_from_slice(s.id.as_bytes());
        file.extend_from_slice(&s.salt);
        file.extend_from_slice(&s.rounds.to_le_bytes());
        file.extend_from_slice(&(s.sealed.len() as u32).to_le_bytes());
        file.extend_from_slice(&s.sealed);
    }
    vfs.write_file(&master_path(user), &file)
        .map_err(|_| CenError::LedgerIo("master key write failed"))
}

/// `slots` with the sign-in password slot replaced by one for `password`.
fn with_password_slot(mut slots: Vec<Slot>, master: &[u8; 32], password: &str) -> Result<Vec<Slot>, CenError> {
    slots.retain(|s| !(s.kind == SlotKind::SignIn && s.id == PASSWORD_SLOT));
    slots.push(make_slot(SlotKind::SignIn, PASSWORD_SLOT, master, password.as_bytes())?);
    Ok(slots)
}

/// `slots` with the system slot added (`on`) or removed.
fn with_system_slot(mut slots: Vec<Slot>, master: &[u8; 32], on: bool) -> Result<Vec<Slot>, CenError> {
    slots.retain(|s| s.kind != SlotKind::System);
    if on {
        let key = system_key(true).ok_or(CenError::LedgerIo("the system key can't be read or made"))?;
        slots.push(make_slot(SlotKind::System, "system", master, &key)?);
    }
    Ok(slots)
}

/// The signed-in `user`'s master key, from memory.
fn master_of_signed_in(user: &str) -> Result<[u8; 32], CenError> {
    MASTER.lock().as_ref().filter(|(_, u)| u == user).map(|(m, _)| *m)
        .ok_or(CenError::LedgerIo("the saved keys aren't open (sign in again)"))
}

/// Load every key `user` saved (with MASTER set). Returns how many.
fn load_all_ledgers(user: &str) -> usize {
    let mut loaded = 0;
    if let Ok(entries) = crate::fs::vfs::VFS.list_dir(user_dir(user).trim_end_matches('/')) {
        for e in entries {
            let lower = e.name.to_ascii_lowercase();
            if let Some(name) = lower.strip_suffix(".vkl") {
                if let Ok(keys) = load_ledger(user, &FeatureTag::new(name)) {
                    loaded += keys.len();
                }
            }
        }
    }
    loaded
}

/// At sign-in: open (or, the first time, create) the user's master key and
/// load every key they saved. Returns how many keys were loaded.
///
/// If nothing opens it, the file is KEPT and Err(DecryptionFailed) says so:
/// it used to be replaced, silently losing every saved key. The user can
/// re-wrap it (cen_relink with the old password, cen_recover with the
/// recovery key). Until then nothing is loaded and keys can't be saved.
pub fn cen_unlock_user(user: &str, with: Unlock) -> Result<usize, CenError> {
    let master = match read_slots(user) {
        None => {
            // First sign-in: a new master key in one slot.
            let m: [u8; 32] = random();
            let slots = match with {
                Unlock::Secret(s) => vec![make_slot(SlotKind::SignIn, PASSWORD_SLOT, &m, s.as_bytes())?],
                Unlock::System => with_system_slot(Vec::new(), &m, true)?,
            };
            write_slots(user, &slots)?;
            crate::serial_println!("[CEN] {}: new master key", user);
            m
        }
        Some(Err(e)) => {
            crate::serial_println!("[CEN] {}: master key file not read ({}); kept, nothing loaded", user, e);
            return Err(e);
        }
        Some(Ok((slots, v1))) => {
            let opened = match with {
                Unlock::Secret(s) => open_kind(&slots, SlotKind::SignIn, s.as_bytes()),
                // No password: the system slot, or (made before slots) a
                // sign-in slot wrapped by the empty password, which then
                // becomes a system slot.
                Unlock::System => open_system(&slots).or_else(|| {
                    let m = open_kind(&slots, SlotKind::SignIn, b"")?;
                    let moved = with_system_slot(slots.iter().filter(|s| s.kind != SlotKind::SignIn).cloned().collect(), &m, true).ok()?;
                    write_slots(user, &moved).ok()?;
                    Some(m)
                }),
            };
            let Some(m) = opened else {
                crate::serial_println!("[CEN] {}: no key slot opened; the file is kept, nothing loaded", user);
                return Err(CenError::DecryptionFailed);
            };
            if v1 {
                write_slots(user, &slots)?;
                crate::serial_println!("[CEN] {}: master key file moved to key slots (v2)", user);
            }
            m
        }
    };
    *MASTER.lock() = Some((master, String::from(user)));
    let loaded = load_all_ledgers(user);
    crate::serial_println!("[CEN] {}: unlocked, {} saved key(s) loaded", user, loaded);
    Ok(loaded)
}

/// The saved keys are wrapped with an older password (cen_unlock_user
/// refused them at sign-in): open them with `old_password`, put the current
/// password in the sign-in slot, then unlock. The caller checks that
/// `current_password` is the user's password.
pub fn cen_relink(user: &str, old_password: &str, current_password: &str) -> Result<usize, CenError> {
    if MASTER.lock().is_some() {
        return Err(CenError::LedgerIo("the saved keys are already open"));
    }
    let (slots, _) = read_slots(user).ok_or(CenError::LedgerIo("there are no saved keys"))??;
    let master = open_kind(&slots, SlotKind::SignIn, old_password.as_bytes()).ok_or(CenError::DecryptionFailed)?;
    write_slots(user, &with_password_slot(slots, &master, current_password)?)?;
    crate::serial_println!("[CEN] {}: sign-in slot re-wrapped with the current password", user);
    cen_unlock_user(user, Unlock::Secret(current_password))
}

/// The recovery key opens the saved keys after a forgotten password (or an
/// admin reset without an admin slot): open them with `code`, put the
/// current sign-in in its slot, then unlock. The caller checks `current`.
pub fn cen_recover(user: &str, code: &str, current: Unlock) -> Result<usize, CenError> {
    if MASTER.lock().is_some() {
        return Err(CenError::LedgerIo("the saved keys are already open"));
    }
    let (slots, _) = read_slots(user).ok_or(CenError::LedgerIo("there are no saved keys"))??;
    let norm = normalize_recovery_key(code);
    let master = open_kind(&slots, SlotKind::Recovery, norm.as_bytes()).ok_or(CenError::DecryptionFailed)?;
    let slots = match current {
        Unlock::Secret(p) => with_password_slot(slots, &master, p)?,
        Unlock::System => with_system_slot(slots, &master, true)?,
    };
    write_slots(user, &slots)?;
    crate::serial_println!("[CEN] {}: opened with the recovery key and re-wrapped", user);
    cen_unlock_user(user, current)
}

/// At sign-out: forget the master key and every saved key in memory.
pub fn cen_lock_user() {
    let had = MASTER.lock().take();
    if let Some((mut m, user)) = had {
        m.iter_mut().for_each(|b| *b = 0);
        KEY_REGISTRY.lock().retain(|_, e| !e.persisted);
        crate::security::credentials::forget();
        crate::serial_println!("[CEN] {}: locked", user);
    }
}

/// After a password change: put the new password in the sign-in slot
/// (other slots stay). The old one opens the key if the user isn't signed
/// in. `keep_system`: whether the account keeps its system (admin) slot;
/// an account that had no password had one only because of that.
pub fn cen_change_password(user: &str, old_password: &str, new_password: &str, keep_system: bool) -> Result<(), CenError> {
    let Some(read) = read_slots(user) else { return Ok(()) }; // nothing saved yet
    let (slots, _) = read?;
    let master = match master_of_signed_in(user) {
        Ok(m) => m,
        Err(_) => open_kind(&slots, SlotKind::SignIn, old_password.as_bytes())
            .or_else(|| if old_password.is_empty() { open_system(&slots) } else { None })
            .ok_or(CenError::DecryptionFailed)?,
    };
    let mut slots = with_password_slot(slots, &master, new_password)?;
    if !keep_system {
        slots = with_system_slot(slots, &master, false)?;
    }
    write_slots(user, &slots)
}

/// What an admin password reset did to the user's saved keys.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResetOutcome {
    /// Nothing was saved.
    NothingSaved,
    /// Re-wrapped for the new password through the admin slot.
    Rewrapped,
    /// Kept: the user opens them with their recovery key (KEYS RECOVER).
    KeptForRecovery,
    /// No admin slot or recovery key: they couldn't be opened again, so
    /// they were removed.
    Removed,
}

/// An admin set the user's password without the old one.
pub fn cen_reset_password(user: &str, new_password: &str) -> Result<ResetOutcome, CenError> {
    let Some(read) = read_slots(user) else { return Ok(ResetOutcome::NothingSaved) };
    let (slots, _) = read?;
    if let Some(master) = open_system(&slots) {
        write_slots(user, &with_password_slot(slots, &master, new_password)?)?;
        crate::serial_println!("[CEN] {}: password reset; keys re-wrapped through the admin slot", user);
        return Ok(ResetOutcome::Rewrapped);
    }
    if slots.iter().any(|s| s.kind == SlotKind::Recovery) {
        crate::serial_println!("[CEN] {}: password reset; keys kept for the recovery key", user);
        return Ok(ResetOutcome::KeptForRecovery);
    }
    cen_forget_user(user);
    Ok(ResetOutcome::Removed)
}

/// The signed-in user turns their admin slot on or off.
pub fn cen_set_system_slot(user: &str, on: bool) -> Result<(), CenError> {
    let master = master_of_signed_in(user)?;
    let slots = match read_slots(user) {
        Some(r) => r?.0,
        None => Vec::new(),
    };
    if !on && !slots.iter().any(|s| s.kind != SlotKind::System) {
        return Err(CenError::LedgerIo("the admin slot is the only way in (no password); set a password first"));
    }
    write_slots(user, &with_system_slot(slots, &master, on)?)
}

/// Crockford base32: no I, L, O or U to misread.
const RECOVERY_ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// A recovery key as typed: upper case, without dashes or spaces, with the
/// look-alike letters read as digits.
fn normalize_recovery_key(code: &str) -> String {
    code.chars().filter(|c| c.is_ascii_alphanumeric()).map(|c| match c.to_ascii_uppercase() {
        'O' => '0',
        'I' | 'L' => '1',
        c => c,
    }).collect()
}

/// The signed-in user gets a new recovery key (any old one stops working).
/// Returns it, to be shown once: 40 characters (200 bits) in groups of 5.
pub fn cen_new_recovery_key(user: &str) -> Result<String, CenError> {
    let master = master_of_signed_in(user)?;
    let bytes: [u8; 40] = random();
    let code: String = bytes.iter().map(|b| RECOVERY_ALPHABET[(*b & 31) as usize] as char).collect();
    let mut slots = match read_slots(user) {
        Some(r) => r?.0,
        None => Vec::new(),
    };
    slots.retain(|s| s.kind != SlotKind::Recovery);
    slots.push(make_slot(SlotKind::Recovery, "recovery", &master, code.as_bytes())?);
    write_slots(user, &slots)?;
    let groups: Vec<&str> = (0..code.len()).step_by(5).map(|i| &code[i..i + 5]).collect();
    Ok(groups.join("-"))
}

/// The signed-in user removes their recovery key.
pub fn cen_remove_recovery_key(user: &str) -> Result<(), CenError> {
    let _ = master_of_signed_in(user)?;
    let Some(read) = read_slots(user) else { return Ok(()) };
    let (mut slots, _) = read?;
    slots.retain(|s| s.kind != SlotKind::Recovery);
    write_slots(user, &slots)
}

/// Which slots a user's saved keys have.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SlotInfo {
    /// A master key file exists.
    pub exists: bool,
    pub sign_in: usize,
    pub recovery: bool,
    pub system: bool,
}

pub fn cen_slot_info(user: &str) -> SlotInfo {
    match read_slots(user) {
        Some(Ok((slots, _))) => SlotInfo {
            exists: true,
            sign_in: slots.iter().filter(|s| s.kind == SlotKind::SignIn).count(),
            recovery: slots.iter().any(|s| s.kind == SlotKind::Recovery),
            system: slots.iter().any(|s| s.kind == SlotKind::System),
        },
        Some(Err(_)) => SlotInfo { exists: true, ..SlotInfo::default() },
        None => SlotInfo::default(),
    }
}

/// Remove the user's saved keys and master key (a reset with no admin slot
/// or recovery key, which can't carry them over).
pub fn cen_forget_user(user: &str) {
    let dir = user_dir(user);
    if let Ok(entries) = crate::fs::vfs::VFS.list_dir(dir.trim_end_matches('/')) {
        for e in entries {
            let _ = crate::fs::vfs::VFS.delete_file(&format!("{}{}", dir, e.name));
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
    if let Some(proc) = crate::sched::process::PROCESSOR.lock().get_active() {
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
    let vfs = &crate::fs::vfs::VFS;
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
    let file = match crate::fs::vfs::VFS.read_file(&ledger_path(user, feature)) {
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
    if let Some(proc) = crate::sched::process::PROCESSOR.lock().get_active() {
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
    if let Some(proc) = crate::sched::process::PROCESSOR.lock().get_active() {
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
    let file = crate::fs::vfs::VFS.read_file(&path).map_err(|_| CenError::LedgerIo("ledger read failed"))?;
    let mut kept = file[..5].to_vec();
    for (range, payload) in ledger_records(&master, &file)? {
        if payload.get(0..8) != Some(&entry.id.to_le_bytes()[..]) {
            kept.extend_from_slice(&file[range]);
        }
    }
    crate::fs::vfs::VFS.write_file(&path, &kept).map_err(|_| CenError::LedgerIo("VFS write failed"))
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
