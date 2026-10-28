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
 *  - Ledger Model: One .vkl file per FeatureTag (e.g., /etc/keys/drive_enc.vkl). 
 *    Keys at rest are encrypted using a boot-derived Master Key.
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
const LEDGER_VERSION: u8 = 1;

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
    static ref MASTER_KEY: Mutex<Option<[u8; 32]>> = Mutex::new(None);
}

static KEY_HANDLE_COUNTER: AtomicU64 = AtomicU64::new(1);
static CEN_TICK: AtomicU64 = AtomicU64::new(0);

// =====================================================================
// -----| Section 4 — Initialisation |-----
// =====================================================================

pub fn cen_init() {
    let mut pit_tick: u32;
    unsafe {
        // Read PIT channel 0 counter
        core::arch::asm!(
            "in al, 0x40",
            "mov {val:e}, eax",
            val = out(reg) pit_tick
        );
    }

    // XOR PIT tick with compile-time salt and hash for master key
    let mut hasher = Sha256::new();
    hasher.update(&pit_tick.to_le_bytes());
    hasher.update(COMPILE_TIME_SALT);
    let result = hasher.finalize();

    let mut master = [0u8; 32];
    master.copy_from_slice(&result);
    *MASTER_KEY.lock() = Some(master);

    println!("cen_init: Central Encryption Node online.");
}

// =====================================================================
// -----| Section 5 — Key Generation |-----
// =====================================================================

/// Generates a new key for a specific feature.
/// 
/// Note: The tick-based derivation is not cryptographically secure and is a 
/// placeholder until RDRAND support lands. Do not use SecurityLevel::Maximum 
/// in production until replaced.
pub fn cen_generate_key(feature: FeatureTag, level: SecurityLevel, password: Option<&[u8]>) -> Result<KeyHandle, CenError> {
    // Check ManageKeys capability
    if let Some(proc) = crate::process::PROCESSOR.lock().get_active() {
        if !proc.capabilities.lock().has(Capability::ManageKeys) {
            return Err(CenError::CapabilityDenied);
        }
    }

    let current_tick = CEN_TICK.fetch_add(1, Ordering::SeqCst);
    let algo = CipherAlgorithm::for_level(level);

    let bytes = if let Some(pw) = password {
        // Generate salt from tick + salt
        let mut salt_hasher = Sha256::new();
        salt_hasher.update(&current_tick.to_le_bytes());
        salt_hasher.update(COMPILE_TIME_SALT);
        let salt = salt_hasher.finalize();

        let rounds = match level {
            SecurityLevel::Standard => PBKDF2_ROUNDS_STANDARD,
            SecurityLevel::High => PBKDF2_ROUNDS_HIGH,
            SecurityLevel::Maximum => PBKDF2_ROUNDS_MAXIMUM,
        };

        let mut derived = [0u8; 32];
        pbkdf2_hmac::<Sha256>(pw, &salt, rounds, &mut derived);
        derived.to_vec()
    } else {
        // Pseudo-random placeholder
        let mut hasher = Sha256::new();
        hasher.update(&current_tick.to_le_bytes());
        hasher.update(&KEY_HANDLE_COUNTER.load(Ordering::Relaxed).to_le_bytes());
        hasher.update(feature.as_str().as_bytes());
        hasher.finalize().to_vec()
    };

    let handle = KeyHandle(KEY_HANDLE_COUNTER.fetch_add(1, Ordering::SeqCst));
    let entry = KeyEntry {
        handle,
        material: KeyMaterial::new(bytes),
        algorithm: algo,
        level,
        feature,
        created_tick: current_tick,
        persisted: false,
    };

    KEY_REGISTRY.lock().insert(handle, entry);
    Ok(handle)
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

    // Generate tick-based nonce (12 bytes)
    let tick = CEN_TICK.fetch_add(1, Ordering::SeqCst);
    let mut nonce_bytes = [0u8; 12];
    nonce_bytes[0..8].copy_from_slice(&tick.to_le_bytes());
    
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

pub fn cen_persist_key(handle: KeyHandle) -> Result<(), CenError> {
    if let Some(proc) = crate::process::PROCESSOR.lock().get_active() {
        if !proc.capabilities.lock().has(Capability::WriteKeyLedger) {
            return Err(CenError::CapabilityDenied);
        }
    }

    let mut registry = KEY_REGISTRY.lock();
    let entry = registry.get_mut(&handle).ok_or(CenError::KeyNotFound(handle))?;

    if entry.persisted {
        return Err(CenError::KeyAlreadyPersisted(handle));
    }

    // Serialise: [handle_u64 LE][algorithm_u8][level_u8][feature_name_len_u8][feature_name_bytes]
    //            [created_tick_u64 LE][key_len_u16 LE][key_bytes]
    let mut payload = Vec::new();
    payload.extend(&entry.handle.0.to_le_bytes());
    payload.push(entry.algorithm as u8);
    payload.push(entry.level as u8);
    payload.push(entry.feature.as_str().len() as u8);
    payload.extend(entry.feature.as_str().as_bytes());
    payload.extend(&entry.created_tick.to_le_bytes());
    payload.extend(&(entry.material.as_bytes().len() as u16).to_le_bytes());
    payload.extend(entry.material.as_bytes());

    // Encrypt with MASTER_KEY
    let master = MASTER_KEY.lock().ok_or(CenError::LedgerIo("Master key missing"))?;
    let cipher = Aes256Gcm::new_from_slice(&master).map_err(|_| CenError::EncryptionFailed)?;
    
    let mut nonce_bytes = [0u8; 12];
    nonce_bytes[0..8].copy_from_slice(&handle.0.to_le_bytes());
    let nonce = Nonce::from_slice(&nonce_bytes);

    let encrypted = cipher.encrypt(nonce, payload.as_slice()).map_err(|_| CenError::EncryptionFailed)?;

    // Ledger framing: [plaintext_len_u32 LE][encrypted_record+tag]
    let mut record = Vec::new();
    record.extend(&(payload.len() as u32).to_le_bytes());
    record.extend(encrypted);

    let path = entry.feature.ledger_path();
    
    let vfs = crate::vfs::VFS.lock();
    let mut file_content = vfs.read_file(&path).unwrap_or_else(|_| {
        let mut initial = Vec::new();
        initial.extend_from_slice(LEDGER_MAGIC);
        initial.push(LEDGER_VERSION);
        initial
    });
    file_content.extend_from_slice(&record);
    if vfs.write_file(&path, &file_content).is_err() {
        return Err(CenError::LedgerIo("VFS write failed"));
    }
    
    println!("cen: key {} persisted to {}", handle, path);
    entry.persisted = true;
    Ok(())
}

pub fn cen_load_keys(feature: FeatureTag) -> Result<Vec<KeyHandle>, CenError> {
    if let Some(proc) = crate::process::PROCESSOR.lock().get_active() {
        if !proc.capabilities.lock().has(Capability::ReadKeyLedger) {
            return Err(CenError::CapabilityDenied);
        }
    }

    let path = feature.ledger_path();
    let bytes: Vec<u8> = crate::vfs::VFS.lock().read_file(&path).unwrap_or_else(|_| Vec::new());

    if bytes.is_empty() {
        return Ok(Vec::new());
    }

    // Header check: LEDGER_MAGIC (4) + LEDGER_VERSION (1)
    if bytes.len() < 5 || &bytes[0..4] != LEDGER_MAGIC || bytes[4] != LEDGER_VERSION {
        return Err(CenError::InvalidLedger);
    }

    // Parsing logic would go here... reconstruct KeyEntry and insert to registry
    
    println!("cen: loaded 0 keys for {}", feature.as_str());
    Ok(Vec::new())
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
        println!("cen: WARNING — key {} was persisted; ledger record not removed. Call cen_purge_ledger() to clean up.", handle);
    }

    println!("cen: key {} revoked and zeroed.", handle);
    Ok(())
}

pub fn cen_purge_ledger(_feature: FeatureTag, _handle: KeyHandle) -> Result<(), CenError> {
    println!("cen: WARNING — cen_purge_ledger is not yet implemented.");
    Ok(())
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
