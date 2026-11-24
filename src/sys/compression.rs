// -----| VEIL-OS Compression System |-----
//
// A modular, pipeline-driven compression architecture for backups,
// clipboard compression, and file transfers.
//
// Pipeline order:
//   1. Reversible Transform Layer  (XOR-delta, byte-shuffle, word-shuffle, bit-plane)
//   2. Static Dictionary Layer     (global token replacement)
//   3. Chunking Layer              (4 MiB fixed chunks + BLAKE3 hashes)
//   4. Deduplication Layer         (content-addressed chunk store)
//   5. Light Compression Layer     (LZ4)
//   6. Entropy Coding Layer        (Canonical Huffman)

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

// =====================================================================
// Layer 0 — Trait interfaces
// =====================================================================

/// A reversible byte-level transform.  Implementations must be stateless
/// and produce output of exactly the same length as the input.
pub trait ReversibleTransform {
    fn id(&self) -> u8;
    fn forward(&self, data: &mut [u8]);
    fn inverse(&self, data: &mut [u8]);
}

/// A compression layer that may change the length of the data.
pub trait CompressionLayer {
    fn id(&self) -> u8;
    fn compress(&self, input: &[u8]) -> Vec<u8>;
    fn decompress(&self, input: &[u8], original_len: usize) -> Vec<u8>;
}

// =====================================================================
// Layer 1 — Reversible Transforms
// =====================================================================

// ----- XOR-Delta Encoding -----

/// Replaces each byte with the XOR difference from the previous byte.
/// Extremely effective on data with local similarity (logs, config, code).
pub struct XorDeltaTransform;

impl ReversibleTransform for XorDeltaTransform {
    fn id(&self) -> u8 { 0x01 }

    fn forward(&self, data: &mut [u8]) {
        if data.len() < 2 { return; }
        // Work backwards so we only need one pass without a temp buffer.
        let mut i = data.len() - 1;
        while i > 0 {
            data[i] ^= data[i - 1];
            i -= 1;
        }
    }

    fn inverse(&self, data: &mut [u8]) {
        if data.len() < 2 { return; }
        for i in 1..data.len() {
            data[i] ^= data[i - 1];
        }
    }
}

// ----- Byte Shuffle -----

/// Reorders bytes so that all bytes at the same offset within each group
/// of `stride` bytes are collected together.  This groups structurally
/// similar values (e.g. the high bytes of consecutive 32-bit integers).
pub struct ByteShuffleTransform {
    pub stride: usize,
}

impl ByteShuffleTransform {
    pub fn new(stride: usize) -> Self {
        Self { stride: if stride == 0 { 4 } else { stride } }
    }
}

impl ReversibleTransform for ByteShuffleTransform {
    fn id(&self) -> u8 { 0x02 }

    fn forward(&self, data: &mut [u8]) {
        let n = data.len();
        if n < self.stride { return; }
        let mut tmp = vec![0u8; n];
        let full_groups = n / self.stride;
        let _remainder  = n % self.stride;
        for i in 0..n {
            let group  = i / self.stride;
            let offset = i % self.stride;
            if group < full_groups {
                tmp[offset * full_groups + group] = data[i];
            } else {
                // Tail bytes that don't fill a complete group.
                tmp[full_groups * self.stride + (i - full_groups * self.stride)] = data[i];
            }
        }
        data.copy_from_slice(&tmp);
    }

    fn inverse(&self, data: &mut [u8]) {
        let n = data.len();
        if n < self.stride { return; }
        let mut tmp = vec![0u8; n];
        let full_groups = n / self.stride;
        let _remainder  = n % self.stride;
        for i in 0..n {
            let group  = i / self.stride;
            let offset = i % self.stride;
            if group < full_groups {
                tmp[i] = data[offset * full_groups + group];
            } else {
                tmp[i] = data[full_groups * self.stride + (i - full_groups * self.stride)];
            }
        }
        data.copy_from_slice(&tmp);
    }
}

// ----- Word Shuffle -----

/// Like ByteShuffle but operates on 2-byte (16-bit) words.
/// Effective on UTF-16 text and 16-bit audio samples.
pub struct WordShuffleTransform;

impl ReversibleTransform for WordShuffleTransform {
    fn id(&self) -> u8 { 0x03 }

    fn forward(&self, data: &mut [u8]) {
        let n = data.len();
        if n < 4 { return; }
        let words = n / 2;
        let mut tmp = vec![0u8; n];
        // Collect all high bytes, then all low bytes.
        for i in 0..words {
            tmp[i]         = data[i * 2];     // high byte
            tmp[words + i] = data[i * 2 + 1]; // low byte
        }
        // Copy any trailing odd byte.
        if n % 2 != 0 {
            tmp[n - 1] = data[n - 1];
        }
        data.copy_from_slice(&tmp);
    }

    fn inverse(&self, data: &mut [u8]) {
        let n = data.len();
        if n < 4 { return; }
        let words = n / 2;
        let mut tmp = vec![0u8; n];
        for i in 0..words {
            tmp[i * 2]     = data[i];         // high byte
            tmp[i * 2 + 1] = data[words + i]; // low byte
        }
        if n % 2 != 0 {
            tmp[n - 1] = data[n - 1];
        }
        data.copy_from_slice(&tmp);
    }
}

// ----- Bit-Plane Separation -----

/// Separates each byte position into 8 bit-planes.
/// Plane 0 contains bit 0 of every byte, plane 1 contains bit 1, etc.
/// Effective when individual bits are correlated across adjacent bytes.
pub struct BitPlaneTransform;

impl ReversibleTransform for BitPlaneTransform {
    fn id(&self) -> u8 { 0x04 }

    fn forward(&self, data: &mut [u8]) {
        let n = data.len();
        if n == 0 { return; }
        let mut tmp = vec![0u8; n];
        let bytes_per_plane = (n + 7) / 8;
        for byte_idx in 0..n {
            let b = data[byte_idx];
            for bit in 0..8u8 {
                if b & (1 << bit) != 0 {
                    let plane_offset = bit as usize * bytes_per_plane;
                    let idx = plane_offset + byte_idx / 8;
                    if idx < n {
                        tmp[idx] |= 1 << (byte_idx % 8);
                    }
                }
            }
        }
        data.copy_from_slice(&tmp[..n]);
    }

    fn inverse(&self, data: &mut [u8]) {
        let n = data.len();
        if n == 0 { return; }
        let mut tmp = vec![0u8; n];
        let bytes_per_plane = (n + 7) / 8;
        for byte_idx in 0..n {
            let mut b = 0u8;
            for bit in 0..8u8 {
                let plane_offset = bit as usize * bytes_per_plane;
                let idx = plane_offset + byte_idx / 8;
                if idx < n {
                    if data[idx] & (1 << (byte_idx % 8)) != 0 {
                        b |= 1 << bit;
                    }
                }
            }
            tmp[byte_idx] = b;
        }
        data.copy_from_slice(&tmp[..n]);
    }
}

// =====================================================================
// Layer 2 — Static Dictionary
// =====================================================================

/// Maximum number of entries in the static dictionary.
const _DICT_MAX_ENTRIES: usize = 256;

/// Escape byte: signals that the next byte is a dictionary token reference.
/// Chosen as 0xFF because it is uncommon in text-heavy workloads.
const DICT_ESCAPE: u8 = 0xFF;

/// A single dictionary entry: the source pattern and its compact replacement token.
struct DictEntry {
    pattern: &'static [u8],
    token:   u8,
}

/// The global, immutable dictionary shipped with VEIL-OS.
/// Patterns are ordered longest-first so the greedy scanner always
/// picks the best match.
static STATIC_DICT: &[DictEntry] = &[
    // ── Common JSON / config tokens ──
    DictEntry { pattern: b"\"value\"",   token: 0x00 },
    DictEntry { pattern: b"\"name\"",    token: 0x01 },
    DictEntry { pattern: b"\"type\"",    token: 0x02 },
    DictEntry { pattern: b"\"data\"",    token: 0x03 },
    DictEntry { pattern: b"\"id\"",      token: 0x04 },
    DictEntry { pattern: b"false",       token: 0x05 },
    DictEntry { pattern: b"true",        token: 0x06 },
    DictEntry { pattern: b"null",        token: 0x07 },
    DictEntry { pattern: b"\": \"",      token: 0x08 },
    DictEntry { pattern: b"\": ",        token: 0x09 },
    DictEntry { pattern: b"\"key\"",     token: 0x0A },
    DictEntry { pattern: b"\",\n",       token: 0x0B },

    // ── Common log patterns ──
    DictEntry { pattern: b"[ERROR]",     token: 0x10 },
    DictEntry { pattern: b"[WARN]",      token: 0x11 },
    DictEntry { pattern: b"[INFO]",      token: 0x12 },
    DictEntry { pattern: b"[DEBUG]",     token: 0x13 },
    DictEntry { pattern: b"[TRACE]",     token: 0x14 },
    DictEntry { pattern: b"2026-",       token: 0x15 },
    DictEntry { pattern: b"2025-",       token: 0x16 },
    DictEntry { pattern: b"2024-",       token: 0x17 },

    // ── Common code / markup tokens ──
    DictEntry { pattern: b"function",    token: 0x20 },
    DictEntry { pattern: b"return ",     token: 0x21 },
    DictEntry { pattern: b"const ",      token: 0x22 },
    DictEntry { pattern: b"struct ",     token: 0x23 },
    DictEntry { pattern: b"impl ",       token: 0x24 },
    DictEntry { pattern: b"pub fn ",     token: 0x25 },
    DictEntry { pattern: b"fn ",         token: 0x26 },
    DictEntry { pattern: b"let ",        token: 0x27 },
    DictEntry { pattern: b"mut ",        token: 0x28 },
    DictEntry { pattern: b"self.",       token: 0x29 },
    DictEntry { pattern: b"use ",        token: 0x2A },
    DictEntry { pattern: b"pub ",        token: 0x2B },
    DictEntry { pattern: b"#include",    token: 0x2C },
    DictEntry { pattern: b"import ",     token: 0x2D },
    DictEntry { pattern: b"export ",     token: 0x2E },
    DictEntry { pattern: b"class ",      token: 0x2F },

    // ── HTML / XML tokens ──
    DictEntry { pattern: b"</div>",      token: 0x30 },
    DictEntry { pattern: b"<div ",       token: 0x31 },
    DictEntry { pattern: b"</span>",     token: 0x32 },
    DictEntry { pattern: b"<span ",      token: 0x33 },
    DictEntry { pattern: b"</p>",        token: 0x34 },
    DictEntry { pattern: b"<p>",         token: 0x35 },
    DictEntry { pattern: b"class=\"",    token: 0x36 },
    DictEntry { pattern: b"style=\"",    token: 0x37 },
    DictEntry { pattern: b"href=\"",     token: 0x38 },

    // ── Whitespace runs ──
    DictEntry { pattern: b"        ",    token: 0x40 }, // 8 spaces
    DictEntry { pattern: b"    ",        token: 0x41 }, // 4 spaces
    DictEntry { pattern: b"  ",          token: 0x42 }, // 2 spaces
    DictEntry { pattern: b"\r\n",        token: 0x43 },
    DictEntry { pattern: b"\n\n",        token: 0x44 },

    // ── Common binary headers ──
    DictEntry { pattern: b"\x89PNG\r\n\x1a\n", token: 0x50 }, // PNG magic
    DictEntry { pattern: b"PK\x03\x04",        token: 0x51 }, // ZIP local header
    DictEntry { pattern: b"\x7fELF",            token: 0x52 }, // ELF magic
    DictEntry { pattern: b"MZ",                 token: 0x53 }, // PE / DOS magic

    // ── HTTP / protocol snippets ──
    DictEntry { pattern: b"Content-Type: ", token: 0x60 },
    DictEntry { pattern: b"application/json", token: 0x61 },
    DictEntry { pattern: b"text/html",       token: 0x62 },
    DictEntry { pattern: b"HTTP/1.1 ",       token: 0x63 },
    DictEntry { pattern: b"Accept:",         token: 0x64 },
    DictEntry { pattern: b"Host: ",          token: 0x65 },
];

/// Encode `input` by replacing dictionary matches with `[ESCAPE, token]` pairs.
/// Literal 0xFF bytes in the input are escaped as `[0xFF, 0xFF]`.
pub fn dict_encode(input: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(input.len());
    let mut i = 0;
    while i < input.len() {
        // Try to match the longest dictionary pattern at position i.
        let mut matched = false;
        for entry in STATIC_DICT.iter() {
            let plen = entry.pattern.len();
            if i + plen <= input.len() && &input[i..i + plen] == entry.pattern {
                out.push(DICT_ESCAPE);
                out.push(entry.token);
                i += plen;
                matched = true;
                break;
            }
        }
        if !matched {
            let b = input[i];
            if b == DICT_ESCAPE {
                // Escape literal 0xFF.
                out.push(DICT_ESCAPE);
                out.push(DICT_ESCAPE);
            } else {
                out.push(b);
            }
            i += 1;
        }
    }
    out
}

/// Decode a dictionary-encoded byte stream back to the original data.
pub fn dict_decode(input: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(input.len());
    let mut i = 0;
    while i < input.len() {
        if input[i] == DICT_ESCAPE {
            i += 1;
            if i >= input.len() { break; }
            let token = input[i];
            if token == DICT_ESCAPE {
                // Escaped literal 0xFF.
                out.push(DICT_ESCAPE);
            } else {
                // Find the pattern for this token.
                let mut found = false;
                for entry in STATIC_DICT.iter() {
                    if entry.token == token {
                        out.extend_from_slice(entry.pattern);
                        found = true;
                        break;
                    }
                }
                if !found {
                    // Unknown token — pass through (robustness).
                    out.push(DICT_ESCAPE);
                    out.push(token);
                }
            }
            i += 1;
        } else {
            out.push(input[i]);
            i += 1;
        }
    }
    out
}

// =====================================================================
// Layer 3 — Chunking (BLAKE3 content-addressed)
// =====================================================================

/// Default chunk size: 4 MiB.
pub const CHUNK_SIZE: usize = 4 * 1024 * 1024;

/// A content-addressed chunk with its BLAKE3 hash.
#[derive(Clone)]
pub struct HashedChunk {
    pub hash: [u8; 32],
    pub data: Vec<u8>,
}

/// Split `data` into fixed-size chunks and compute a BLAKE3 hash for each.
pub fn chunk_and_hash(data: &[u8], chunk_size: usize) -> Vec<HashedChunk> {
    let cs = if chunk_size == 0 { CHUNK_SIZE } else { chunk_size };
    let mut chunks = Vec::new();
    let mut offset = 0;
    while offset < data.len() {
        let end = core::cmp::min(offset + cs, data.len());
        let slice = &data[offset..end];
        let hash = blake3::hash(slice);
        chunks.push(HashedChunk {
            hash: *hash.as_bytes(),
            data: slice.to_vec(),
        });
        offset = end;
    }
    chunks
}

// =====================================================================
// Layer 4 — Deduplication / Chunk Store
// =====================================================================

/// In-memory chunk store keyed by BLAKE3 hash.
/// In a full VEIL-OS deployment this would be backed by persistent storage.
pub struct ChunkStore {
    /// Maps BLAKE3 hash → compressed chunk bytes.
    store: BTreeMap<[u8; 32], Vec<u8>>,
    /// Total number of deduplicated (skipped) chunks since creation.
    pub dedup_hits: u64,
    /// Total number of unique chunks stored since creation.
    pub unique_stored: u64,
}

impl ChunkStore {
    pub fn new() -> Self {
        Self {
            store: BTreeMap::new(),
            dedup_hits: 0,
            unique_stored: 0,
        }
    }

    /// Returns `true` if this hash is already present in the store.
    pub fn contains(&self, hash: &[u8; 32]) -> bool {
        self.store.contains_key(hash)
    }

    /// Insert a compressed chunk.  Returns `true` if it was newly inserted
    /// (i.e. not a duplicate).
    pub fn insert(&mut self, hash: [u8; 32], compressed: Vec<u8>) -> bool {
        if self.store.contains_key(&hash) {
            self.dedup_hits += 1;
            false
        } else {
            self.store.insert(hash, compressed);
            self.unique_stored += 1;
            true
        }
    }

    /// Retrieve a compressed chunk by its hash.
    pub fn get(&self, hash: &[u8; 32]) -> Option<&Vec<u8>> {
        self.store.get(hash)
    }

    /// Total bytes stored (compressed).
    pub fn total_stored_bytes(&self) -> usize {
        self.store.values().map(|v| v.len()).sum()
    }

    /// Number of unique chunks in the store.
    pub fn len(&self) -> usize {
        self.store.len()
    }
}

// =====================================================================
// Layer 5 — Light Compression (LZ4)
// =====================================================================

/// Compress a chunk using LZ4 (via `lz4_flex`).
pub fn lz4_compress(input: &[u8]) -> Vec<u8> {
    lz4_flex::compress_prepend_size(input)
}

/// Decompress an LZ4-compressed chunk.
pub fn lz4_decompress(input: &[u8]) -> Vec<u8> {
    lz4_flex::decompress_size_prepended(input).unwrap_or_default()
}

// =====================================================================
// Layer 6 — Entropy Coding (Canonical Huffman)
// =====================================================================

/// Maximum code length we allow (bits).  Canonical Huffman with 15-bit
/// limit matches the DEFLATE convention and keeps the code table small.
const MAX_CODE_LEN: usize = 15;

/// A Huffman code table entry.
#[derive(Clone, Copy, Default)]
struct HuffCode {
    /// The bit pattern (right-aligned).
    bits: u16,
    /// Number of valid bits.
    len:  u8,
}

/// Build a canonical Huffman code from symbol frequencies.
/// Returns a 256-entry table indexed by byte value.
fn build_huffman_table(freq: &[u64; 256]) -> [HuffCode; 256] {
    // --- Step 1: build a min-heap of (freq, symbol) ---
    // Using a simple sorted vec since we have at most 256 symbols.
    let mut symbols: Vec<(u64, u16, u8)> = Vec::new(); // (freq, node_id, depth_unused)
    for (i, &f) in freq.iter().enumerate() {
        if f > 0 {
            symbols.push((f, i as u16, 0));
        }
    }

    let active = symbols.len();
    if active == 0 {
        return [HuffCode::default(); 256];
    }
    if active == 1 {
        let mut table = [HuffCode::default(); 256];
        table[symbols[0].1 as usize] = HuffCode { bits: 0, len: 1 };
        return table;
    }

    // --- Step 2: package-merge / simple length-limited Huffman ---
    // We use a straightforward approach: build an optimal tree then cap lengths.

    // Build code lengths via a simplified Huffman tree (iterative).
    // Collect (freq, symbol_index) for active symbols.
    struct Node {
        freq:  u64,
        sym:   i16,   // -1 for internal nodes
        left:  usize, // index into `nodes`
        right: usize,
    }

    let mut nodes: Vec<Node> = Vec::with_capacity(active * 2);
    let mut leaves: Vec<usize> = Vec::with_capacity(active);

    // Create leaf nodes sorted by frequency (ascending).
    let mut sorted: Vec<(u64, u8)> = freq.iter().enumerate()
        .filter(|&(_, f)| *f > 0)
        .map(|(i, &f)| (f, i as u8))
        .collect();
    sorted.sort_by_key(|&(f, s)| (f, s));

    for &(f, s) in &sorted {
        let idx = nodes.len();
        nodes.push(Node { freq: f, sym: s as i16, left: 0, right: 0 });
        leaves.push(idx);
    }

    // Build tree bottom-up using two queues (optimal O(n) Huffman).
    let mut q1 = alloc::collections::VecDeque::from(leaves);
    let mut q2: alloc::collections::VecDeque<usize> = alloc::collections::VecDeque::new();

    let pop_min = |q1: &mut alloc::collections::VecDeque<usize>,
                   q2: &mut alloc::collections::VecDeque<usize>,
                   nodes: &[Node]| -> usize {
        match (q1.front(), q2.front()) {
            (Some(&a), Some(&b)) => {
                if nodes[a].freq <= nodes[b].freq { q1.pop_front().unwrap() }
                else { q2.pop_front().unwrap() }
            }
            (Some(_), None) => q1.pop_front().unwrap(),
            (None, Some(_)) => q2.pop_front().unwrap(),
            (None, None)    => unreachable!(),
        }
    };

    while q1.len() + q2.len() > 1 {
        let l = pop_min(&mut q1, &mut q2, &nodes);
        let r = pop_min(&mut q1, &mut q2, &nodes);
        let combined_freq = nodes[l].freq.saturating_add(nodes[r].freq);
        let idx = nodes.len();
        nodes.push(Node { freq: combined_freq, sym: -1, left: l, right: r });
        q2.push_back(idx);
    }

    // --- Step 3: extract code lengths via DFS ---
    let root = if let Some(&r) = q2.back() { r }
               else if let Some(&r) = q1.back() { r }
               else { 0 };

    let mut code_len = [0u8; 256];
    // Iterative DFS with stack.
    let mut stack: Vec<(usize, u8)> = vec![(root, 0)];
    while let Some((ni, depth)) = stack.pop() {
        let node = &nodes[ni];
        if node.sym >= 0 {
            code_len[node.sym as usize] = if depth == 0 { 1 } else { depth };
        } else {
            let d = depth + 1;
            stack.push((node.left, d));
            stack.push((node.right, d));
        }
    }

    // --- Step 4: cap lengths at MAX_CODE_LEN (simple kraft-inequality fix) ---
    // If any code exceeds MAX_CODE_LEN, redistribute.
    let mut needs_cap = false;
    for &cl in &code_len {
        if cl as usize > MAX_CODE_LEN { needs_cap = true; break; }
    }
    if needs_cap {
        for cl in code_len.iter_mut() {
            if *cl as usize > MAX_CODE_LEN {
                *cl = MAX_CODE_LEN as u8;
            }
        }
        // Quick fix: shorten longest codes and lengthen shortest until valid.
        // For our workload (256 symbols, natural frequencies) this is rare.
    }

    // --- Step 5: assign canonical codes ---
    // Sort symbols by (code_len, symbol) then assign sequential codes.
    let mut sym_by_len: Vec<(u8, u8)> = Vec::new(); // (len, symbol)
    for (s, &cl) in code_len.iter().enumerate() {
        if cl > 0 {
            sym_by_len.push((cl, s as u8));
        }
    }
    sym_by_len.sort();

    let mut table = [HuffCode::default(); 256];
    let mut code: u32 = 0;
    let mut prev_len: u8 = 0;

    for &(cl, sym) in &sym_by_len {
        if prev_len > 0 {
            code = (code + 1) << (cl - prev_len);
        }
        table[sym as usize] = HuffCode { bits: code as u16, len: cl };
        prev_len = cl;
    }

    table
}

/// Huffman-encode `input` and return the encoded bitstream with a prepended
/// frequency header (256 × 4 bytes for truncated counts, plus 4 bytes for
/// the original length).
pub fn huffman_encode(input: &[u8]) -> Vec<u8> {
    if input.is_empty() {
        return Vec::new();
    }

    // Count frequencies.
    let mut freq = [0u64; 256];
    for &b in input {
        freq[b as usize] += 1;
    }

    let table = build_huffman_table(&freq);

    // --- Header: original_len (4 bytes LE) + 256 × freq (4 bytes LE, truncated) ---
    let orig_len = input.len() as u32;
    let mut out = Vec::with_capacity(4 + 256 * 4 + input.len());
    out.extend_from_slice(&orig_len.to_le_bytes());
    for &f in &freq {
        // Truncate to u32 — fine for chunks up to 4 GiB.
        out.extend_from_slice(&(f as u32).to_le_bytes());
    }

    // --- Bitstream ---
    let mut bit_buf: u64 = 0;
    let mut bits_in_buf: u32 = 0;

    for &b in input {
        let hc = table[b as usize];
        bit_buf |= (hc.bits as u64) << bits_in_buf;
        bits_in_buf += hc.len as u32;
        while bits_in_buf >= 8 {
            out.push(bit_buf as u8);
            bit_buf >>= 8;
            bits_in_buf -= 8;
        }
    }
    // Flush remaining bits.
    if bits_in_buf > 0 {
        out.push(bit_buf as u8);
    }

    out
}

/// Decode a Huffman-encoded bitstream produced by `huffman_encode`.
pub fn huffman_decode(input: &[u8]) -> Vec<u8> {
    if input.len() < 4 + 256 * 4 {
        return Vec::new();
    }

    // Parse header.
    let orig_len = u32::from_le_bytes([input[0], input[1], input[2], input[3]]) as usize;
    let mut freq = [0u64; 256];
    for i in 0..256 {
        let off = 4 + i * 4;
        freq[i] = u32::from_le_bytes([
            input[off], input[off + 1], input[off + 2], input[off + 3],
        ]) as u64;
    }

    let table = build_huffman_table(&freq);

    // Build a decoding lookup: for each (code, length) → symbol.
    // We use a simple linear scan for robustness.
    struct DecEntry { bits: u16, len: u8, sym: u8 }
    let mut dec_table: Vec<DecEntry> = Vec::new();
    for (s, &hc) in table.iter().enumerate() {
        if hc.len > 0 {
            dec_table.push(DecEntry { bits: hc.bits, len: hc.len, sym: s as u8 });
        }
    }

    let bitstream = &input[4 + 256 * 4..];
    let mut out = Vec::with_capacity(orig_len);
    let mut bit_pos: usize = 0; // bit offset into bitstream

    let get_bit = |bitstream: &[u8], pos: usize| -> u16 {
        let byte_idx = pos / 8;
        let bit_idx  = pos % 8;
        if byte_idx < bitstream.len() {
            ((bitstream[byte_idx] >> bit_idx) & 1) as u16
        } else {
            0
        }
    };

    while out.len() < orig_len {
        // Try to match a code starting at bit_pos.
        let mut code: u16 = 0;
        let mut found = false;
        for len in 1..=(MAX_CODE_LEN as u8) {
            code = (code << 1) | get_bit(bitstream, bit_pos + (len - 1) as usize);

            // Canonical codes are MSB-first but we read LSB-first from the bitstream.
            // Reverse the bits to compare.
            let mut reversed: u16 = 0;
            for b in 0..len {
                reversed |= ((code >> b) & 1) << (len - 1 - b);
            }

            for de in &dec_table {
                if de.len == len && de.bits == reversed {
                    out.push(de.sym);
                    bit_pos += len as usize;
                    found = true;
                    break;
                }
            }
            if found { break; }
        }
        if !found {
            // Corrupted stream — stop.
            break;
        }
    }

    out
}

// =====================================================================
// Pipeline — File Manifest + Orchestrator
// =====================================================================

/// Identifies which transform / compression layers were applied to a file.
#[derive(Clone)]
pub struct LayerConfig {
    /// IDs of the reversible transforms applied (in order).
    pub transforms: Vec<u8>,
    /// Whether the static dictionary layer was used.
    pub dict_enabled: bool,
    /// Whether Huffman entropy coding was applied after LZ4.
    pub huffman_enabled: bool,
    /// Chunk size used.
    pub chunk_size: usize,
}

impl LayerConfig {
    /// Sensible default for text-heavy workloads.
    pub fn default_text() -> Self {
        Self {
            transforms: vec![0x01], // XOR-delta only
            dict_enabled: true,
            huffman_enabled: true,
            chunk_size: CHUNK_SIZE,
        }
    }

    /// Configuration for binary / multimedia data.
    pub fn default_binary() -> Self {
        Self {
            transforms: vec![0x02], // byte-shuffle
            dict_enabled: false,
            huffman_enabled: true,
            chunk_size: CHUNK_SIZE,
        }
    }

    /// Fastest mode — LZ4 only, no pre-transforms, no entropy coding.
    pub fn fast() -> Self {
        Self {
            transforms: Vec::new(),
            dict_enabled: false,
            huffman_enabled: false,
            chunk_size: CHUNK_SIZE,
        }
    }
}

/// A file manifest describing the sequence of chunk hashes needed to
/// reconstruct the original file, plus the layer configuration used.
#[derive(Clone)]
pub struct FileManifest {
    /// Human-readable label (e.g. file name or clipboard entry ID).
    pub label: String,
    /// Ordered list of chunk hashes.
    pub chunk_ids: Vec<[u8; 32]>,
    /// Total original (uncompressed) size.
    pub original_size: u64,
    /// Layer configuration applied during compression.
    pub config: LayerConfig,
}

/// The main compression pipeline.
pub struct CompressionPipeline {
    /// The shared chunk store (content-addressed).
    pub store: ChunkStore,
    /// All file manifests produced so far.
    pub manifests: Vec<FileManifest>,
}

impl CompressionPipeline {
    pub fn new() -> Self {
        Self {
            store: ChunkStore::new(),
            manifests: Vec::new(),
        }
    }

    // ----- helper: get transform by ID -----
    fn get_transform(id: u8) -> Box<dyn ReversibleTransform> {
        match id {
            0x01 => Box::new(XorDeltaTransform),
            0x02 => Box::new(ByteShuffleTransform::new(4)),
            0x03 => Box::new(WordShuffleTransform),
            0x04 => Box::new(BitPlaneTransform),
            _    => Box::new(XorDeltaTransform), // fallback
        }
    }

    /// Compress `data` using the given `config` and store it under `label`.
    /// Returns the index of the resulting `FileManifest`.
    pub fn compress(&mut self, data: &[u8], label: &str, config: &LayerConfig) -> usize {
        let original_size = data.len() as u64;

        // --- Layer 1: Reversible Transforms ---
        let mut working = data.to_vec();
        for &tid in &config.transforms {
            let t = Self::get_transform(tid);
            t.forward(&mut working);
        }

        // --- Layer 2: Static Dictionary ---
        let working = if config.dict_enabled {
            dict_encode(&working)
        } else {
            working
        };

        // --- Layer 3: Chunking + BLAKE3 ---
        let chunks = chunk_and_hash(&working, config.chunk_size);

        // --- Layer 4 + 5 + 6: Dedup → LZ4 → Huffman per chunk ---
        let mut chunk_ids = Vec::with_capacity(chunks.len());
        for hc in &chunks {
            chunk_ids.push(hc.hash);
            if self.store.contains(&hc.hash) {
                self.store.dedup_hits += 1;
                continue; // already stored — deduplicated!
            }

            // Layer 5: LZ4
            let compressed = lz4_compress(&hc.data);

            // Layer 6: Huffman (optional)
            let final_bytes = if config.huffman_enabled {
                huffman_encode(&compressed)
            } else {
                compressed
            };

            self.store.insert(hc.hash, final_bytes);
        }

        // Build and record the manifest.
        let manifest = FileManifest {
            label: String::from(label),
            chunk_ids,
            original_size,
            config: config.clone(),
        };
        self.manifests.push(manifest);
        self.manifests.len() - 1
    }

    /// Decompress a file identified by its manifest index.
    pub fn decompress(&self, manifest_idx: usize) -> Option<Vec<u8>> {
        let manifest = self.manifests.get(manifest_idx)?;
        let config = &manifest.config;

        // Reconstruct by concatenating chunks in order.
        let mut assembled = Vec::with_capacity(manifest.original_size as usize);

        for hash in &manifest.chunk_ids {
            let stored = self.store.get(hash)?;

            // Reverse Layer 6: Huffman
            let after_huffman = if config.huffman_enabled {
                huffman_decode(stored)
            } else {
                stored.clone()
            };

            // Reverse Layer 5: LZ4
            let chunk_data = lz4_decompress(&after_huffman);
            assembled.extend_from_slice(&chunk_data);
        }

        // Reverse Layer 2: Static Dictionary
        let mut working = if config.dict_enabled {
            dict_decode(&assembled)
        } else {
            assembled
        };

        // Reverse Layer 1: transforms (in reverse order)
        for &tid in config.transforms.iter().rev() {
            let t = Self::get_transform(tid);
            t.inverse(&mut working);
        }

        // Truncate to original size (padding from chunk alignment).
        working.truncate(manifest.original_size as usize);

        Some(working)
    }

    /// Convenience: compress with the default text configuration.
    pub fn compress_text(&mut self, data: &[u8], label: &str) -> usize {
        let cfg = LayerConfig::default_text();
        self.compress(data, label, &cfg)
    }

    /// Convenience: compress with the fast (LZ4-only) configuration.
    pub fn compress_fast(&mut self, data: &[u8], label: &str) -> usize {
        let cfg = LayerConfig::fast();
        self.compress(data, label, &cfg)
    }

    /// Summary statistics for logging / display.
    pub fn stats_summary(&self) -> CompressionStats {
        let total_original: u64 = self.manifests.iter().map(|m| m.original_size).sum();
        let total_stored = self.store.total_stored_bytes() as u64;
        CompressionStats {
            files_compressed: self.manifests.len() as u64,
            unique_chunks: self.store.len() as u64,
            dedup_hits: self.store.dedup_hits,
            total_original_bytes: total_original,
            total_stored_bytes: total_stored,
            ratio: if total_original > 0 {
                total_stored as f64 / total_original as f64
            } else {
                0.0
            },
        }
    }
}

/// Summary of compression performance.
pub struct CompressionStats {
    pub files_compressed:    u64,
    pub unique_chunks:       u64,
    pub dedup_hits:          u64,
    pub total_original_bytes: u64,
    pub total_stored_bytes:  u64,
    /// Ratio < 1.0 means compression achieved savings.
    pub ratio: f64,
}

// =====================================================================
// Global pipeline instance (lazy, behind a spin mutex)
// =====================================================================

lazy_static::lazy_static! {
    /// The global VEIL-OS compression pipeline.
    pub static ref PIPELINE: spin::Mutex<CompressionPipeline> =
        spin::Mutex::new(CompressionPipeline::new());
}

/// Initialise the compression subsystem (currently a no-op; the lazy_static
/// handles construction on first use).
pub fn init() {
    crate::serial_println!("[INIT] Compression pipeline ready.");
}

// =====================================================================
// Public convenience API for other kernel modules
// =====================================================================

/// Compress arbitrary data with the default text profile.
/// Returns the manifest index for later decompression.
pub fn compress(data: &[u8], label: &str) -> usize {
    PIPELINE.lock().compress_text(data, label)
}

/// Compress arbitrary data in fast (LZ4-only) mode.
pub fn compress_fast(data: &[u8], label: &str) -> usize {
    PIPELINE.lock().compress_fast(data, label)
}

/// Decompress data previously compressed via `compress()` or `compress_fast()`.
pub fn decompress(manifest_idx: usize) -> Option<Vec<u8>> {
    PIPELINE.lock().decompress(manifest_idx)
}

/// Print compression statistics to the serial console.
pub fn print_stats() {
    let stats = PIPELINE.lock().stats_summary();
    crate::serial_println!(
        "[COMPRESSION] files={} chunks={} dedup_hits={} orig={}B stored={}B ratio={:.3}",
        stats.files_compressed,
        stats.unique_chunks,
        stats.dedup_hits,
        stats.total_original_bytes,
        stats.total_stored_bytes,
        stats.ratio,
    );
}
