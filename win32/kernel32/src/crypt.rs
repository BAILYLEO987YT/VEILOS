//! The legacy CryptoAPI as far as hashing and random bytes: SHA-1 and SHA-256,
//! which is what libgit2 asks of it. No keys.

use crate::{Bool, Handle};

const CALG_SHA1: u32 = 0x8004;
const CALG_SHA_256: u32 = 0x800C;
const HP_HASHVAL: u32 = 2;
const HP_HASHSIZE: u32 = 4;
const NTE_BAD_ALGID: u32 = 0x8009_0008;
const NTE_BAD_TYPE: u32 = 0x8009_000A;
const ERROR_MORE_DATA: u32 = 234;
/// Any non-zero value: a provider holds nothing.
const PROVIDER: Handle = 0x5645_494C;

struct Hash {
    sha256: bool,
    state: [u32; 8],
    block: [u8; 64],
    fill: usize,
    total: u64,
}

const K256: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

impl Hash {
    fn new(sha256: bool) -> Self {
        let state = if sha256 {
            [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19]
        } else {
            [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0, 0, 0, 0]
        };
        Self { sha256, state, block: [0; 64], fill: 0, total: 0 }
    }

    fn size(&self) -> usize {
        if self.sha256 { 32 } else { 20 }
    }

    fn compress(&mut self) {
        let mut w = [0u32; 80];
        for (i, c) in self.block.chunks_exact(4).enumerate() {
            w[i] = u32::from_be_bytes(c.try_into().unwrap());
        }
        if self.sha256 {
            for i in 16..64 {
                let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
                let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
                w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
            }
            let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = self.state;
            for i in 0..64 {
                let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
                let ch = (e & f) ^ (!e & g);
                let t1 = h.wrapping_add(s1).wrapping_add(ch).wrapping_add(K256[i]).wrapping_add(w[i]);
                let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
                let maj = (a & b) ^ (a & c) ^ (b & c);
                let t2 = s0.wrapping_add(maj);
                h = g;
                g = f;
                f = e;
                e = d.wrapping_add(t1);
                d = c;
                c = b;
                b = a;
                a = t1.wrapping_add(t2);
            }
            for (s, v) in self.state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
                *s = s.wrapping_add(v);
            }
        } else {
            for i in 16..80 {
                w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
            }
            let [mut a, mut b, mut c, mut d, mut e, ..] = self.state;
            for (i, &word) in w.iter().enumerate() {
                let (f, k) = match i / 20 {
                    0 => ((b & c) | (!b & d), 0x5A827999),
                    1 => (b ^ c ^ d, 0x6ED9EBA1),
                    2 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
                    _ => (b ^ c ^ d, 0xCA62C1D6u32),
                };
                let t = a.rotate_left(5).wrapping_add(f).wrapping_add(e).wrapping_add(k).wrapping_add(word);
                e = d;
                d = c;
                c = b.rotate_left(30);
                b = a;
                a = t;
            }
            for (s, v) in self.state.iter_mut().zip([a, b, c, d, e]) {
                *s = s.wrapping_add(v);
            }
        }
    }

    fn update(&mut self, mut data: &[u8]) {
        self.total = self.total.wrapping_add(data.len() as u64);
        while !data.is_empty() {
            let n = data.len().min(64 - self.fill);
            self.block[self.fill..self.fill + n].copy_from_slice(&data[..n]);
            self.fill += n;
            data = &data[n..];
            if self.fill == 64 {
                self.compress();
                self.fill = 0;
            }
        }
    }

    /// The digest. Works on a copy: more can still be asked of the original.
    fn digest(&self, out: &mut [u8]) {
        let mut h = Hash { sha256: self.sha256, state: self.state, block: self.block, fill: self.fill, total: self.total };
        let bits = h.total.wrapping_mul(8);
        h.update(&[0x80]);
        while h.fill != 56 {
            h.update(&[0]);
        }
        h.update(&bits.to_be_bytes());
        for (i, v) in h.state.iter().take(self.size() / 4).enumerate() {
            out[i * 4..i * 4 + 4].copy_from_slice(&v.to_be_bytes());
        }
    }
}

#[no_mangle]
pub unsafe extern "system" fn CryptAcquireContextW(out: *mut Handle, _container: *const u16, _provider: *const u16, _kind: u32, _flags: u32) -> Bool {
    unsafe { *out = PROVIDER };
    1
}

#[no_mangle]
pub unsafe extern "system" fn CryptAcquireContextA(out: *mut Handle, _container: *const u8, _provider: *const u8, _kind: u32, _flags: u32) -> Bool {
    unsafe { *out = PROVIDER };
    1
}

#[no_mangle]
pub extern "system" fn CryptReleaseContext(_provider: Handle, _flags: u32) -> Bool {
    1
}

#[no_mangle]
pub unsafe extern "system" fn CryptGenRandom(_provider: Handle, len: u32, out: *mut u8) -> Bool {
    (unsafe { crate::VeilSysInfo(0, out as u64, len as u64) } == 0) as Bool
}

#[no_mangle]
pub unsafe extern "system" fn CryptCreateHash(_provider: Handle, algorithm: u32, _key: usize, _flags: u32, out: *mut usize) -> Bool {
    let sha256 = match algorithm {
        CALG_SHA1 => false,
        CALG_SHA_256 => true,
        _ => {
            crate::SetLastError(NTE_BAD_ALGID);
            return 0;
        }
    };
    let h = crate::mem::HeapAlloc(crate::mem::GetProcessHeap(), 0, core::mem::size_of::<Hash>()) as *mut Hash;
    if h.is_null() {
        crate::SetLastError(8);
        return 0;
    }
    unsafe {
        h.write(Hash::new(sha256));
        *out = h as usize;
    }
    1
}

#[no_mangle]
pub unsafe extern "system" fn CryptHashData(hash: usize, data: *const u8, len: u32, _flags: u32) -> Bool {
    let h = unsafe { &mut *(hash as *mut Hash) };
    if len != 0 {
        h.update(unsafe { core::slice::from_raw_parts(data, len as usize) });
    }
    1
}

#[no_mangle]
pub unsafe extern "system" fn CryptGetHashParam(hash: usize, param: u32, out: *mut u8, len: *mut u32, _flags: u32) -> Bool {
    let h = unsafe { &*(hash as *const Hash) };
    let size = h.size();
    match param {
        HP_HASHSIZE => {
            if !out.is_null() {
                unsafe { core::ptr::write_unaligned(out as *mut u32, size as u32) };
            }
            unsafe { *len = 4 };
            1
        }
        HP_HASHVAL => {
            let room = unsafe { *len } as usize;
            unsafe { *len = size as u32 };
            if out.is_null() {
                return 1;
            }
            if room < size {
                crate::SetLastError(ERROR_MORE_DATA);
                return 0;
            }
            h.digest(unsafe { core::slice::from_raw_parts_mut(out, size) });
            1
        }
        _ => {
            crate::SetLastError(NTE_BAD_TYPE);
            0
        }
    }
}

#[no_mangle]
pub extern "system" fn CryptDestroyHash(hash: usize) -> Bool {
    crate::mem::HeapFree(crate::mem::GetProcessHeap(), 0, hash as *mut u8)
}
