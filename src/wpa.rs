// =============================================================================
// VEILOS: WPA2-Personal key handling (IEEE 802.11i)
// =============================================================================
//
// What a station needs to join a WPA2-PSK network, in software; the frames'
// encryption (CCMP) is done by the Wi-Fi card with the keys installed here:
//   * PMK = PBKDF2-HMAC-SHA1(passphrase, SSID, 4096 rounds, 32 bytes);
//   * the EAPOL-Key 4-way handshake: PTK = PRF-384(PMK, "Pairwise key
//     expansion", addresses, nonces) split into KCK (MIC key), KEK (key
//     encryption key) and TK (the CCMP key); MICs are HMAC-SHA1-128 over the
//     EAPOL frame (key descriptor version 2);
//   * the group key (GTK) arrives AES-key-wrapped (RFC 3394) with the KEK, in
//     message 3 and in later group key handshakes.

use alloc::vec::Vec;
use hmac::{Hmac, Mac};
use sha1::Sha1;

type HmacSha1 = Hmac<Sha1>;

pub fn pmk(passphrase: &[u8], ssid: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    pbkdf2::pbkdf2_hmac::<Sha1>(passphrase, ssid, 4096, &mut out);
    out
}

pub fn hmac_sha1(key: &[u8], parts: &[&[u8]]) -> [u8; 20] {
    let mut m = HmacSha1::new_from_slice(key).expect("HMAC takes any key length");
    for p in parts {
        m.update(p);
    }
    m.finalize().into_bytes().into()
}

/// IEEE 802.11 PRF-n (SHA-1): HMAC(K, A || 0 || B || i) for i = 0, 1, ...
pub fn prf(key: &[u8], label: &[u8], data: &[u8], len: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(len + 20);
    let mut i = 0u8;
    while out.len() < len {
        out.extend_from_slice(&hmac_sha1(key, &[label, &[0], data, &[i]]));
        i += 1;
    }
    out.truncate(len);
    out
}

/// The pairwise keys: KCK (16), KEK (16), TK (16).
pub struct Ptk {
    pub kck: [u8; 16],
    pub kek: [u8; 16],
    pub tk: [u8; 16],
}

pub fn ptk(pmk: &[u8; 32], aa: &[u8; 6], spa: &[u8; 6], anonce: &[u8; 32], snonce: &[u8; 32]) -> Ptk {
    let mut data = Vec::with_capacity(76);
    let (a1, a2) = if aa < spa { (aa, spa) } else { (spa, aa) };
    data.extend_from_slice(a1);
    data.extend_from_slice(a2);
    let (n1, n2) = if anonce < snonce { (anonce, snonce) } else { (snonce, anonce) };
    data.extend_from_slice(n1);
    data.extend_from_slice(n2);
    let k = prf(pmk, b"Pairwise key expansion", &data, 48);
    let mut p = Ptk { kck: [0; 16], kek: [0; 16], tk: [0; 16] };
    p.kck.copy_from_slice(&k[0..16]);
    p.kek.copy_from_slice(&k[16..32]);
    p.tk.copy_from_slice(&k[32..48]);
    p
}

/// The EAPOL-Key MIC (descriptor version 2): HMAC-SHA1, first 16 bytes.
pub fn mic(kck: &[u8; 16], eapol: &[u8]) -> [u8; 16] {
    let h = hmac_sha1(kck, &[eapol]);
    let mut m = [0u8; 16];
    m.copy_from_slice(&h[..16]);
    m
}

/// RFC 3394 AES key unwrap (AES-128). None if the integrity check fails.
pub fn aes_unwrap(kek: &[u8; 16], c: &[u8]) -> Option<Vec<u8>> {
    use aes::cipher::{BlockDecrypt, KeyInit, generic_array::GenericArray};
    if c.len() < 24 || c.len() % 8 != 0 {
        return None;
    }
    let cipher = aes::Aes128::new(GenericArray::from_slice(kek));
    let n = c.len() / 8 - 1;
    let mut a = [0u8; 8];
    a.copy_from_slice(&c[..8]);
    let mut r: Vec<[u8; 8]> = (0..n).map(|i| {
        let mut b = [0u8; 8];
        b.copy_from_slice(&c[8 + 8 * i..16 + 8 * i]);
        b
    }).collect();
    for j in (0..6).rev() {
        for i in (1..=n).rev() {
            let t = (n * j + i) as u64;
            let mut b = [0u8; 16];
            let at = u64::from_be_bytes(a) ^ t;
            b[..8].copy_from_slice(&at.to_be_bytes());
            b[8..].copy_from_slice(&r[i - 1]);
            let mut block = GenericArray::clone_from_slice(&b);
            cipher.decrypt_block(&mut block);
            a.copy_from_slice(&block[..8]);
            r[i - 1].copy_from_slice(&block[8..]);
        }
    }
    if a != [0xA6; 8] {
        return None;
    }
    Some(r.concat())
}

/// A GTK KDE (dd len 00-0F-AC 01 keyid reserved GTK) in decrypted key data:
/// (key id, key).
pub fn find_gtk(key_data: &[u8]) -> Option<(u8, Vec<u8>)> {
    let mut o = 0;
    while o + 2 <= key_data.len() {
        let (id, l) = (key_data[o], key_data[o + 1] as usize);
        if id == 0xDD && l == 0 {
            break; // padding
        }
        let v = key_data.get(o + 2..o + 2 + l)?;
        if id == 0xDD && l >= 6 && v[..4] == [0x00, 0x0F, 0xAC, 0x01] {
            return Some((v[4] & 3, v[6..].to_vec()));
        }
        o += 2 + l;
    }
    None
}

/// Self-test with the IEEE 802.11 test vectors (run once at boot in debug
/// builds' log, cheap): PBKDF2 and PRF.
pub fn self_test() -> bool {
    // IEEE 802.11-2016 J.4.2: "password", "IEEE", PMK
    // f42c6fc52df0ebef9ebb4b90b38a5f902e83fe1b135a70e23aed762e9710a12e
    let p = pmk(b"password", b"IEEE");
    let ok_pmk = p[..4] == [0xF4, 0x2C, 0x6F, 0xC5] && p[28..] == [0x97, 0x10, 0xA1, 0x2E];
    // RFC 3394 4.1: KEK 000102..0F, key 00112233..FF.
    let kek: [u8; 16] = core::array::from_fn(|i| i as u8);
    let c = [0x1F, 0xA6, 0x8B, 0x0A, 0x81, 0x12, 0xB4, 0x47, 0xAE, 0xF3, 0x4B, 0xD8, 0xFB, 0x5A, 0x7B, 0x82,
        0x9D, 0x3E, 0x86, 0x23, 0x71, 0xD2, 0xCF, 0xE5];
    let ok_wrap = aes_unwrap(&kek, &c).is_some_and(|k| k == [0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77,
        0x88, 0x99, 0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF]);
    // IEEE 802.11 PRF test: key 0x0b x 20, "prefix", "Hi There", PRF-512.
    let r = prf(&[0x0B; 20], b"prefix", b"Hi There", 64);
    let ok_prf = r[..4] == [0xBC, 0xD4, 0xC6, 0x50] && r[60..] == [0x83, 0x56, 0xCF, 0x5A];
    crate::serial_println!("[WIFI] self-test: PMK {} key unwrap {} PRF {}", ok_pmk, ok_wrap, ok_prf);
    ok_pmk && ok_wrap && ok_prf
}
