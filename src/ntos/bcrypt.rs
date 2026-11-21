// Kernel-mode CNG (what drivers import from ksecdd.sys): BCrypt over the
// crypto crates VEILOS's kernel already has.
//
//   * Random numbers (the kernel's entropy pool).
//   * Hashes: SHA1, SHA256, SHA384, SHA512, plain or HMAC
//     (BCRYPT_ALG_HANDLE_HMAC_FLAG).
//   * AES keys of 128, 192 or 256 bits, decrypting in ECB, CBC (with
//     BCRYPT_BLOCK_PADDING's PKCS#7) or GCM (authenticated).
//
// Not yet (STATUS_NOT_SUPPORTED, logged once each): asymmetric keys
// (BCryptImportKeyPair, BCryptVerifySignature: no RSA or ECC crate in the
// kernel yet) and BCryptKeyDerivation.
//
// Handles are pointers to VEILOS's own objects (boxed); the object buffers
// callers pass (pbHashObject, pbKeyObject) aren't needed and go unused.

use alloc::{boxed::Box, string::String, vec::Vec};
use hmac::{Hmac, Mac};
use sha1::Sha1;
use sha2::{Digest, Sha256, Sha384, Sha512};

use super::{rtl, NtStatus, STATUS_INVALID_PARAMETER, STATUS_SUCCESS};

const STATUS_NOT_SUPPORTED: NtStatus = 0xC000_00BBu32 as i32;
const STATUS_INVALID_HANDLE: NtStatus = 0xC000_0008u32 as i32;
const STATUS_BUFFER_TOO_SMALL: NtStatus = 0xC000_0023u32 as i32;
const STATUS_NOT_FOUND: NtStatus = 0xC000_0225u32 as i32;
const STATUS_AUTH_TAG_MISMATCH: NtStatus = 0xC000_A002u32 as i32;
const STATUS_INVALID_BUFFER_SIZE: NtStatus = 0xC000_0206u32 as i32;

const HMAC_FLAG: u32 = 0x8;
const BLOCK_PADDING: u32 = 0x1;

#[derive(Clone, Copy, PartialEq)]
enum Hash {
    Sha1,
    Sha256,
    Sha384,
    Sha512,
}

impl Hash {
    fn len(self) -> usize {
        match self {
            Hash::Sha1 => 20,
            Hash::Sha256 => 32,
            Hash::Sha384 => 48,
            Hash::Sha512 => 64,
        }
    }
    fn block(self) -> usize {
        match self {
            Hash::Sha1 | Hash::Sha256 => 64,
            _ => 128,
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Ecb,
    Cbc,
    Gcm,
}

enum Alg {
    Hash { h: Hash, hmac: bool },
    Aes { mode: Mode },
    Rng,
}

enum HashState {
    Sha1(Sha1),
    Sha256(Sha256),
    Sha384(Sha384),
    Sha512(Sha512),
    HSha1(Hmac<Sha1>),
    HSha256(Hmac<Sha256>),
    HSha384(Hmac<Sha384>),
    HSha512(Hmac<Sha512>),
}

struct HashObj {
    state: HashState,
}

struct KeyObj {
    key: Vec<u8>,
    mode: Mode,
}

fn wide(p: *const u16) -> String {
    if p.is_null() {
        return String::new();
    }
    let n = rtl::wcslen(p);
    char::decode_utf16(unsafe { core::slice::from_raw_parts(p, n) }.iter().copied()).map(|c| c.unwrap_or('?')).collect()
}

fn unsupported(name: &str) -> NtStatus {
    crate::serial_println!("[NTOS] {}: not supported yet (STATUS_NOT_SUPPORTED)", name);
    STATUS_NOT_SUPPORTED
}

/// BCryptOpenAlgorithmProvider(Algorithm*, AlgId, Implementation, Flags).
pub extern "win64" fn BCryptOpenAlgorithmProvider(out: *mut u64, id: *const u16, _imp: *const u16, flags: u32) -> NtStatus {
    let hmac = flags & HMAC_FLAG != 0;
    let alg = match wide(id).to_ascii_uppercase().as_str() {
        "SHA1" => Alg::Hash { h: Hash::Sha1, hmac },
        "SHA256" => Alg::Hash { h: Hash::Sha256, hmac },
        "SHA384" => Alg::Hash { h: Hash::Sha384, hmac },
        "SHA512" => Alg::Hash { h: Hash::Sha512, hmac },
        "AES" => Alg::Aes { mode: Mode::Cbc },
        "RNG" => Alg::Rng,
        _ => return STATUS_NOT_FOUND,
    };
    unsafe { out.write(Box::into_raw(Box::new(alg)) as u64) };
    STATUS_SUCCESS
}

pub extern "win64" fn BCryptCloseAlgorithmProvider(h: u64, _flags: u32) -> NtStatus {
    if h == 0 {
        return STATUS_INVALID_HANDLE;
    }
    drop(unsafe { Box::from_raw(h as *mut Alg) });
    STATUS_SUCCESS
}

fn put_dword(out: *mut u8, cb: u32, result: *mut u32, v: u32) -> NtStatus {
    if !result.is_null() {
        unsafe { result.write(4) };
    }
    if out.is_null() {
        return STATUS_SUCCESS;
    }
    if cb < 4 {
        return STATUS_BUFFER_TOO_SMALL;
    }
    unsafe { (out as *mut u32).write_unaligned(v) };
    STATUS_SUCCESS
}

/// BCryptGetProperty(Object, Property, Output, cbOutput, cbResult*, Flags):
/// ObjectLength, HashDigestLength, BlockLength, KeyLength.
pub extern "win64" fn BCryptGetProperty(h: u64, prop: *const u16, out: *mut u8, cb: u32, result: *mut u32, _flags: u32) -> NtStatus {
    if h == 0 {
        return STATUS_INVALID_HANDLE;
    }
    let alg = unsafe { &*(h as *const Alg) };
    let v = match (wide(prop).as_str(), alg) {
        ("ObjectLength", _) => 16,
        ("HashDigestLength", Alg::Hash { h, .. }) => h.len() as u32,
        ("BlockLength", Alg::Hash { h, .. }) => h.block() as u32,
        ("BlockLength", Alg::Aes { .. }) => 16,
        _ => return STATUS_NOT_SUPPORTED,
    };
    put_dword(out, cb, result, v)
}

/// BCryptSetProperty: the AES chaining mode (ChainingModeECB / CBC / GCM).
pub extern "win64" fn BCryptSetProperty(h: u64, prop: *const u16, input: *const u8, cb: u32, _flags: u32) -> NtStatus {
    if h == 0 {
        return STATUS_INVALID_HANDLE;
    }
    let alg = unsafe { &mut *(h as *mut Alg) };
    if wide(prop) != "ChainingMode" {
        return STATUS_NOT_SUPPORTED;
    }
    let Alg::Aes { mode } = alg else { return STATUS_INVALID_PARAMETER };
    let units = unsafe { core::slice::from_raw_parts(input as *const u16, cb as usize / 2) };
    let v: String = char::decode_utf16(units.iter().copied().take_while(|&u| u != 0)).map(|c| c.unwrap_or('?')).collect();
    *mode = match v.as_str() {
        "ChainingModeECB" => Mode::Ecb,
        "ChainingModeCBC" => Mode::Cbc,
        "ChainingModeGCM" => Mode::Gcm,
        _ => return STATUS_NOT_SUPPORTED,
    };
    STATUS_SUCCESS
}

// -----| hashes |-----

/// BCryptCreateHash(Algorithm, Hash*, HashObject, cbHashObject, Secret,
/// cbSecret, Flags).
pub extern "win64" fn BCryptCreateHash(alg: u64, out: *mut u64, _obj: *mut u8, _cb: u32, secret: *const u8, cb_secret: u32, _flags: u32) -> NtStatus {
    if alg == 0 {
        return STATUS_INVALID_HANDLE;
    }
    let Alg::Hash { h, hmac } = (unsafe { &*(alg as *const Alg) }) else { return STATUS_INVALID_PARAMETER };
    let key = if secret.is_null() { &[][..] } else { unsafe { core::slice::from_raw_parts(secret, cb_secret as usize) } };
    let state = match (h, hmac) {
        (Hash::Sha1, false) => HashState::Sha1(Sha1::new()),
        (Hash::Sha256, false) => HashState::Sha256(Sha256::new()),
        (Hash::Sha384, false) => HashState::Sha384(Sha384::new()),
        (Hash::Sha512, false) => HashState::Sha512(Sha512::new()),
        (Hash::Sha1, true) => HashState::HSha1(<Hmac<Sha1> as Mac>::new_from_slice(key).unwrap()),
        (Hash::Sha256, true) => HashState::HSha256(<Hmac<Sha256> as Mac>::new_from_slice(key).unwrap()),
        (Hash::Sha384, true) => HashState::HSha384(<Hmac<Sha384> as Mac>::new_from_slice(key).unwrap()),
        (Hash::Sha512, true) => HashState::HSha512(<Hmac<Sha512> as Mac>::new_from_slice(key).unwrap()),
    };
    unsafe { out.write(Box::into_raw(Box::new(HashObj { state })) as u64) };
    STATUS_SUCCESS
}

pub extern "win64" fn BCryptHashData(h: u64, input: *const u8, len: u32, _flags: u32) -> NtStatus {
    if h == 0 {
        return STATUS_INVALID_HANDLE;
    }
    let o = unsafe { &mut *(h as *mut HashObj) };
    let data = if len == 0 { &[][..] } else { unsafe { core::slice::from_raw_parts(input, len as usize) } };
    match &mut o.state {
        HashState::Sha1(s) => Digest::update(s, data),
        HashState::Sha256(s) => Digest::update(s, data),
        HashState::Sha384(s) => Digest::update(s, data),
        HashState::Sha512(s) => Digest::update(s, data),
        HashState::HSha1(s) => Mac::update(s, data),
        HashState::HSha256(s) => Mac::update(s, data),
        HashState::HSha384(s) => Mac::update(s, data),
        HashState::HSha512(s) => Mac::update(s, data),
    }
    STATUS_SUCCESS
}

/// BCryptFinishHash(Hash, Output, cbOutput, Flags): cbOutput must be the
/// digest's length. The hash can't take more data after.
pub extern "win64" fn BCryptFinishHash(h: u64, out: *mut u8, len: u32, _flags: u32) -> NtStatus {
    if h == 0 {
        return STATUS_INVALID_HANDLE;
    }
    let o = unsafe { &mut *(h as *mut HashObj) };
    let state = core::mem::replace(&mut o.state, HashState::Sha1(Sha1::new()));
    let digest: Vec<u8> = match state {
        HashState::Sha1(s) => s.finalize().to_vec(),
        HashState::Sha256(s) => s.finalize().to_vec(),
        HashState::Sha384(s) => s.finalize().to_vec(),
        HashState::Sha512(s) => s.finalize().to_vec(),
        HashState::HSha1(s) => s.finalize().into_bytes().to_vec(),
        HashState::HSha256(s) => s.finalize().into_bytes().to_vec(),
        HashState::HSha384(s) => s.finalize().into_bytes().to_vec(),
        HashState::HSha512(s) => s.finalize().into_bytes().to_vec(),
    };
    if len as usize != digest.len() {
        return STATUS_INVALID_PARAMETER;
    }
    unsafe { core::ptr::copy_nonoverlapping(digest.as_ptr(), out, digest.len()) };
    STATUS_SUCCESS
}

pub extern "win64" fn BCryptDestroyHash(h: u64) -> NtStatus {
    if h == 0 {
        return STATUS_INVALID_HANDLE;
    }
    drop(unsafe { Box::from_raw(h as *mut HashObj) });
    STATUS_SUCCESS
}

// -----| AES |-----

/// BCryptGenerateSymmetricKey(Algorithm, Key*, KeyObject, cbKeyObject,
/// Secret, cbSecret, Flags).
pub extern "win64" fn BCryptGenerateSymmetricKey(alg: u64, out: *mut u64, _obj: *mut u8, _cb: u32, secret: *const u8, cb_secret: u32, _flags: u32) -> NtStatus {
    if alg == 0 {
        return STATUS_INVALID_HANDLE;
    }
    let Alg::Aes { mode } = (unsafe { &*(alg as *const Alg) }) else { return STATUS_INVALID_PARAMETER };
    if !matches!(cb_secret, 16 | 24 | 32) {
        return STATUS_INVALID_PARAMETER;
    }
    let key = unsafe { core::slice::from_raw_parts(secret, cb_secret as usize) }.to_vec();
    unsafe { out.write(Box::into_raw(Box::new(KeyObj { key, mode: *mode })) as u64) };
    STATUS_SUCCESS
}

pub extern "win64" fn BCryptDestroyKey(h: u64) -> NtStatus {
    if h == 0 {
        return STATUS_INVALID_HANDLE;
    }
    drop(unsafe { Box::from_raw(h as *mut KeyObj) });
    STATUS_SUCCESS
}

fn aes_decrypt_blocks(key: &[u8], data: &mut [u8]) {
    use aes::cipher::{generic_array::GenericArray, BlockDecrypt, KeyInit};
    macro_rules! run {
        ($c:ty) => {{
            let c = <$c>::new(GenericArray::from_slice(key));
            for b in data.chunks_exact_mut(16) {
                c.decrypt_block(GenericArray::from_mut_slice(b));
            }
        }};
    }
    match key.len() {
        16 => run!(aes::Aes128),
        24 => run!(aes::Aes192),
        _ => run!(aes::Aes256),
    }
}

/// BCryptDecrypt(Key, Input, cbInput, PaddingInfo, IV, cbIV, Output,
/// cbOutput, cbResult*, Flags). ECB and CBC (the IV is updated to the
/// last ciphertext block, as CNG does); BCRYPT_BLOCK_PADDING strips
/// PKCS#7. GCM: PaddingInfo is a BCRYPT_AUTHENTICATED_CIPHER_MODE_INFO
/// (nonce at 0x08/0x10, auth data 0x18/0x20, tag 0x28/0x30).
pub extern "win64" fn BCryptDecrypt(
    h: u64,
    input: *const u8,
    in_len: u32,
    padding: *const u8,
    iv: *mut u8,
    iv_len: u32,
    output: *mut u8,
    out_len: u32,
    result: *mut u32,
    flags: u32,
) -> NtStatus {
    if h == 0 {
        return STATUS_INVALID_HANDLE;
    }
    let k = unsafe { &*(h as *const KeyObj) };
    let data_in = unsafe { core::slice::from_raw_parts(input, in_len as usize) };
    let mut buf = data_in.to_vec();
    if k.mode == Mode::Gcm {
        use aes_gcm::aead::{AeadInPlace, KeyInit};
        if padding.is_null() {
            return STATUS_INVALID_PARAMETER;
        }
        let field = |o: usize| unsafe { ((padding.add(o) as *const u64).read_unaligned(), (padding.add(o + 8) as *const u32).read_unaligned() as usize) };
        let (nonce_p, nonce_n) = field(0x08);
        let (aad_p, aad_n) = field(0x18);
        let (tag_p, tag_n) = field(0x28);
        if nonce_n != 12 || tag_n != 16 || k.key.len() != 32 && k.key.len() != 16 {
            return STATUS_NOT_SUPPORTED;
        }
        let nonce = unsafe { core::slice::from_raw_parts(nonce_p as *const u8, 12) };
        let aad = if aad_n == 0 { &[][..] } else { unsafe { core::slice::from_raw_parts(aad_p as *const u8, aad_n) } };
        let tag = unsafe { core::slice::from_raw_parts(tag_p as *const u8, 16) };
        let ok = if k.key.len() == 32 {
            aes_gcm::Aes256Gcm::new_from_slice(&k.key).unwrap().decrypt_in_place_detached(nonce.into(), aad, &mut buf, tag.into()).is_ok()
        } else {
            aes_gcm::Aes128Gcm::new_from_slice(&k.key).unwrap().decrypt_in_place_detached(nonce.into(), aad, &mut buf, tag.into()).is_ok()
        };
        if !ok {
            return STATUS_AUTH_TAG_MISMATCH;
        }
    } else {
        if buf.len() % 16 != 0 {
            return STATUS_INVALID_BUFFER_SIZE;
        }
        aes_decrypt_blocks(&k.key, &mut buf);
        if k.mode == Mode::Cbc {
            if iv.is_null() || iv_len != 16 {
                return STATUS_INVALID_PARAMETER;
            }
            let mut prev = [0u8; 16];
            unsafe { core::ptr::copy_nonoverlapping(iv, prev.as_mut_ptr(), 16) };
            for (i, b) in buf.chunks_exact_mut(16).enumerate() {
                for j in 0..16 {
                    b[j] ^= prev[j];
                }
                prev.copy_from_slice(&data_in[i * 16..i * 16 + 16]);
            }
            unsafe { core::ptr::copy_nonoverlapping(prev.as_ptr(), iv, 16) };
        }
        if flags & BLOCK_PADDING != 0 {
            let pad = *buf.last().unwrap_or(&0) as usize;
            if pad == 0 || pad > 16 || pad > buf.len() || buf[buf.len() - pad..].iter().any(|&b| b as usize != pad) {
                return STATUS_INVALID_PARAMETER;
            }
            buf.truncate(buf.len() - pad);
        }
    }
    if !result.is_null() {
        unsafe { result.write(buf.len() as u32) };
    }
    if output.is_null() {
        return STATUS_SUCCESS;
    }
    if (out_len as usize) < buf.len() {
        return STATUS_BUFFER_TOO_SMALL;
    }
    unsafe { core::ptr::copy_nonoverlapping(buf.as_ptr(), output, buf.len()) };
    STATUS_SUCCESS
}

// -----| random numbers |-----

/// BCryptGenRandom(Algorithm, Buffer, cbBuffer, Flags).
pub extern "win64" fn BCryptGenRandom(_alg: u64, buf: *mut u8, len: u32, _flags: u32) -> NtStatus {
    crate::entropy::fill(unsafe { core::slice::from_raw_parts_mut(buf, len as usize) });
    STATUS_SUCCESS
}

// -----| not yet |-----

pub extern "win64" fn BCryptImportKeyPair(_alg: u64, _import: u64, _blob_type: *const u16, _out: *mut u64, _input: *const u8, _len: u32, _flags: u32) -> NtStatus {
    unsupported("BCryptImportKeyPair")
}

pub extern "win64" fn BCryptVerifySignature(_key: u64, _pad: *const u8, _hash: *const u8, _hlen: u32, _sig: *const u8, _slen: u32, _flags: u32) -> NtStatus {
    unsupported("BCryptVerifySignature")
}

pub extern "win64" fn BCryptKeyDerivation(_key: u64, _params: *const u8, _out: *mut u8, _len: u32, _result: *mut u32, _flags: u32) -> NtStatus {
    unsupported("BCryptKeyDerivation")
}
