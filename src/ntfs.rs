// =====================================================================
// -----| NTFS File System Driver |-----
//
// Read-only NTFS implementation for VEIL-OS.
// Supports:
//   - Boot sector detection and parsing
//   - MFT record reading with Update Sequence Array (USA) fixup
//   - Resident and non-resident attribute parsing
//   - Data run (extent) decoding for non-resident data
//   - Directory listing via $INDEX_ROOT (resident indexes)
//   - File reads (resident $DATA and non-resident $DATA)
//   - Full path resolution through nested directories
//   - UTF-16LE filename decoding
// =====================================================================

use alloc::vec::Vec;
use alloc::string::String;
use crate::vfs::{BlockDevice, FileSystem, DirEntry, FileType, VfsError};
use crate::ahci::HbaPort;
use crate::println;
use x86_64::VirtAddr;

// -----| Constants |-----

const NTFS_OEM_ID: &[u8; 8] = b"NTFS    ";

/// MFT record magic signature ("FILE")
const MFT_SIGNATURE: &[u8; 4] = b"FILE";

// Attribute type codes (see NTFS spec §4)
#[allow(dead_code)] const ATTR_FILE_NAME:       u32 = 0x30;
const ATTR_DATA:            u32 = 0x80;
const ATTR_INDEX_ROOT:      u32 = 0x90;
const ATTR_INDEX_ALLOC:     u32 = 0xA0;  // $INDEX_ALLOCATION — non-resident B-tree blocks
const ATTR_END:             u32 = 0xFFFF_FFFF;

// Magic signature for an INDX (index allocation) block.
const INDX_SIGNATURE: &[u8; 4] = b"INDX";

// MFT record flags
const MFT_FLAG_IN_USE:    u16 = 0x01;
const MFT_FLAG_DIRECTORY: u16 = 0x02;

// Index entry flags
const IE_FLAG_LAST: u16 = 0x02;

// Well-known MFT record numbers
#[allow(dead_code)] const MFT_NUM_MFT:  u64 = 0;
const MFT_NUM_ROOT: u64 = 5;   // Root directory "."

// FILE_NAME namespace values
#[allow(dead_code)] const NS_POSIX:    u8 = 0;
#[allow(dead_code)] const NS_WIN32:    u8 = 1;
const NS_DOS:      u8 = 2;
#[allow(dead_code)] const NS_WIN32DOS: u8 = 3;

// =====================================================================
// -----| Boot Sector Structure |-----
// =====================================================================
//
// Offsets (all little-endian unless noted):
//  0x00  jmp[3]                jump instruction
//  0x03  oem_id[8]             "NTFS    "
//  0x0B  bytes_per_sector      u16
//  0x0D  sectors_per_cluster   u8
//  0x0E  reserved_sectors      u16  (always 0)
//  0x10  fat_count             u8   (always 0)
//  0x11  root_entries          u16  (always 0)
//  0x13  total_sectors_16      u16  (always 0)
//  0x15  media_type            u8
//  0x16  sectors_per_fat_16    u16  (always 0)
//  0x18  sectors_per_track     u16
//  0x1A  heads                 u16
//  0x1C  hidden_sectors        u32
//  0x20  total_sectors_32      u32  (always 0)
//  0x24  ntfs_reserved         u32  (0x00800080)
//  0x28  total_sectors         u64
//  0x30  mft_lcn               u64  Logical Cluster of $MFT
//  0x38  mft_mirror_lcn        u64  Logical Cluster of $MFTMirr
//  0x40  clusters_per_mft_rec  i8   if <0: record_size = 2^(-x)
//  0x41  _pad0[3]
//  0x44  clusters_per_index    i8
//  0x45  _pad1[3]
//  0x48  volume_serial         u64
//  0x50  checksum              u32
//  0x54  bootstrap[426]
//  0x1FE boot_signature        u16  0xAA55

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct NtfsBootSector {
    pub jmp:                  [u8; 3],
    pub oem_id:               [u8; 8],
    pub bytes_per_sector:     u16,
    pub sectors_per_cluster:  u8,
    pub reserved_sectors:     u16,
    pub fat_count:            u8,
    pub root_entries:         u16,
    pub total_sectors_16:     u16,
    pub media_type:           u8,
    pub sectors_per_fat_16:   u16,
    pub sectors_per_track:    u16,
    pub heads:                u16,
    pub hidden_sectors:       u32,
    pub total_sectors_32:     u32,
    pub ntfs_reserved:        u32,  // 0x24 — always 0x00800080
    pub total_sectors:        u64,  // 0x28
    pub mft_lcn:              u64,  // 0x30
    pub mft_mirror_lcn:       u64,  // 0x38
    pub clusters_per_mft_rec: i8,   // 0x40
    pub _pad0:                [u8; 3],
    pub clusters_per_index:   i8,   // 0x44
    pub _pad1:                [u8; 3],
    pub volume_serial:        u64,  // 0x48
    pub checksum:             u32,  // 0x50
    pub bootstrap:            [u8; 426],
    pub boot_signature:       u16,  // 0x1FE — 0xAA55
}

// =====================================================================
// -----| NtfsFileSystem |-----
// =====================================================================

/// Runtime parameters extracted from the NTFS boot sector.
/// This is the only state the filesystem driver keeps — everything else
/// is read directly from disk through the BlockDevice trait.
#[derive(Clone, Copy)]
pub struct NtfsFileSystem {
    pub bytes_per_sector:    u32,
    pub sectors_per_cluster: u32,
    pub bytes_per_cluster:   u32,
    pub mft_lcn:             u64,
    pub mft_record_size:     u32,   // bytes per MFT record (typically 1024)
}

// =====================================================================
// -----| Core disk helpers |-----
// =====================================================================

impl NtfsFileSystem {

    /// Convert a Logical Cluster Number (LCN) to a disk LBA.
    fn lcn_to_lba(&self, lcn: u64) -> u64 {
        lcn * self.sectors_per_cluster as u64
    }

    /// Return the LBA of the first sector of MFT record `n`.
    fn mft_record_lba(&self, n: u64) -> u64 {
        let byte_offset      = n * self.mft_record_size as u64;
        let cluster_index    = byte_offset / self.bytes_per_cluster as u64;
        let cluster_byte_off = byte_offset % self.bytes_per_cluster as u64;
        let sector_off       = cluster_byte_off / self.bytes_per_sector as u64;
        self.lcn_to_lba(self.mft_lcn + cluster_index) + sector_off
    }

    /// Read MFT record `n` from disk, apply the Update Sequence Array
    /// fixup, validate the "FILE" magic, and return the raw bytes.
    fn read_mft_record(&self, device: &dyn BlockDevice, n: u64) -> Result<Vec<u8>, VfsError> {
        let sectors = (self.mft_record_size / self.bytes_per_sector) as u32;
        let lba     = self.mft_record_lba(n);

        let mut buf = alloc::vec![0u8; self.mft_record_size as usize];
        if !device.read_sectors(lba, sectors, buf.as_mut_ptr() as u64) {
            return Err(VfsError::ReadError);
        }

        // Validate "FILE" magic.
        if buf.len() < 4 || &buf[0..4] != MFT_SIGNATURE {
            return Err(VfsError::ReadError);
        }

        // Check the in-use flag before paying the fixup cost.
        if buf.len() >= 24 {
            let flags = u16::from_le_bytes([buf[22], buf[23]]);
            if flags & MFT_FLAG_IN_USE == 0 {
                return Err(VfsError::FileNotFound);
            }
        }

        self.apply_usa_fixup(&mut buf)?;
        Ok(buf)
    }

    /// Apply the Update Sequence Array (USA) fixup to an MFT record buffer.
    ///
    /// Each 512-byte sector in the record ends with a sequence-number
    /// word that must be restored to its original value (stored in the
    /// USA) for the data to be correct.
    fn apply_usa_fixup(&self, record: &mut [u8]) -> Result<(), VfsError> {
        if record.len() < 8 {
            return Err(VfsError::ReadError);
        }

        let usa_offset = u16::from_le_bytes([record[4], record[5]]) as usize;
        let usa_count  = u16::from_le_bytes([record[6], record[7]]) as usize;

        // usa_count includes the sequence number itself, so usable
        // replacements are entries 1..(usa_count).
        if usa_count < 2 || usa_offset + usa_count * 2 > record.len() {
            return Ok(()); // Broken USA — continue anyway (best-effort).
        }

        let _seq_lo = record[usa_offset];
        let _seq_hi = record[usa_offset + 1];

        for i in 1..usa_count {
            let sector_end = i * (self.bytes_per_sector as usize) - 2;
            if sector_end + 1 >= record.len() {
                break;
            }
            // (Optionally validate match here — skipped for robustness.)
            let orig_off = usa_offset + i * 2;
            if orig_off + 1 < record.len() {
                record[sector_end]     = record[orig_off];
                record[sector_end + 1] = record[orig_off + 1];
            }
        }

        Ok(())
    }
}

// =====================================================================
// -----| Attribute helpers |-----
// =====================================================================

impl NtfsFileSystem {

    /// Return the raw bytes of the first attribute of type `attr_type`
    /// found in the MFT record `record`, or `None` if absent.
    fn find_attribute<'a>(&self, record: &'a [u8], attr_type: u32) -> Option<&'a [u8]> {
        if record.len() < 22 {
            return None;
        }

        let first = u16::from_le_bytes([record[20], record[21]]) as usize;
        let mut pos = first;

        while pos + 8 <= record.len() {
            let atype = u32::from_le_bytes([
                record[pos], record[pos+1], record[pos+2], record[pos+3],
            ]);
            if atype == ATTR_END {
                break;
            }

            let alen = u32::from_le_bytes([
                record[pos+4], record[pos+5], record[pos+6], record[pos+7],
            ]) as usize;

            if alen < 16 || pos + alen > record.len() {
                break;
            }

            if atype == attr_type {
                return Some(&record[pos..pos + alen]);
            }

            pos += alen;
        }

        None
    }

    /// For a **resident** attribute, return a slice of just the value bytes.
    /// Returns `None` if the attribute is non-resident or the header is malformed.
    fn resident_value<'a>(&self, attr: &'a [u8]) -> Option<&'a [u8]> {
        if attr.len() < 24 {
            return None;
        }
        if attr[8] != 0 {
            return None; // non-resident
        }
        let val_len = u32::from_le_bytes([attr[16], attr[17], attr[18], attr[19]]) as usize;
        let val_off = u16::from_le_bytes([attr[20], attr[21]]) as usize;

        if val_off + val_len <= attr.len() {
            Some(&attr[val_off..val_off + val_len])
        } else {
            None
        }
    }

    /// Return (data_run_slice, real_size, initialized_size) for a
    /// **non-resident** attribute.  Returns `None` for resident attributes
    /// or malformed headers.
    fn nonresident_info<'a>(&self, attr: &'a [u8]) -> Option<(&'a [u8], u64, u64)> {
        if attr.len() < 64 {
            return None;
        }
        if attr[8] != 1 {
            return None; // resident
        }
        let run_off  = u16::from_le_bytes([attr[32], attr[33]]) as usize;
        let real_sz  = u64::from_le_bytes([
            attr[48], attr[49], attr[50], attr[51],
            attr[52], attr[53], attr[54], attr[55],
        ]);
        let init_sz  = u64::from_le_bytes([
            attr[56], attr[57], attr[58], attr[59],
            attr[60], attr[61], attr[62], attr[63],
        ]);

        if run_off >= attr.len() {
            return None;
        }

        Some((&attr[run_off..], real_sz, init_sz))
    }
}

// =====================================================================
// -----| Data run decoding |-----
// =====================================================================

impl NtfsFileSystem {

    /// Decode the data runs starting at `runs` and read up to `real_size`
    /// bytes of file content from disk.
    ///
    /// Each data run is encoded as:
    ///   [header_byte] [length_bytes...] [offset_bytes...]
    ///
    /// where header_byte low nibble = number of bytes for length,
    ///       header_byte high nibble = number of bytes for (signed) LCN delta.
    /// A header byte of 0x00 terminates the list.
    fn decode_and_read_runs(
        &self,
        device:    &dyn BlockDevice,
        runs:      &[u8],
        real_size: u64,
    ) -> Result<Vec<u8>, VfsError> {
        let mut result      = Vec::new();
        let mut current_lcn = 0i64;
        let mut pos         = 0usize;

        while pos < runs.len() {
            let header = runs[pos];
            if header == 0 {
                break;
            }
            pos += 1;

            let len_bytes = (header & 0x0F) as usize;
            let off_bytes = ((header >> 4) & 0x0F) as usize;

            if len_bytes == 0 || pos + len_bytes + off_bytes > runs.len() {
                break;
            }

            // Run length (unsigned, in clusters)
            let mut run_len: u64 = 0;
            for i in 0..len_bytes {
                run_len |= (runs[pos + i] as u64) << (i * 8);
            }
            pos += len_bytes;

            // LCN delta (signed)
            let run_lcn: i64 = if off_bytes == 0 {
                // Sparse run — all zeroes; skip reading.
                let sparse_bytes = (run_len * self.bytes_per_cluster as u64)
                    .min(real_size - result.len() as u64) as usize;
                result.extend(core::iter::repeat(0u8).take(sparse_bytes));
                if result.len() as u64 >= real_size {
                    break;
                }
                continue;
            } else {
                let mut raw: i64 = 0;
                for i in 0..off_bytes {
                    raw |= (runs[pos + i] as i64) << (i * 8);
                }
                pos += off_bytes;

                // Sign-extend to i64
                let sign_bit = 1i64 << (off_bytes * 8 - 1);
                if raw & sign_bit != 0 {
                    raw |= -(sign_bit << 1);
                }
                raw
            };

            current_lcn += run_lcn;
            if current_lcn < 0 {
                break; // Corrupt run list
            }

            let lba      = self.lcn_to_lba(current_lcn as u64);
            let sectors  = (run_len * self.sectors_per_cluster as u64) as u32;
            let buf_size = (run_len * self.bytes_per_cluster as u64) as usize;

            let mut chunk = alloc::vec![0u8; buf_size];
            if !device.read_sectors(lba, sectors, chunk.as_mut_ptr() as u64) {
                return Err(VfsError::ReadError);
            }

            let remaining  = (real_size - result.len() as u64) as usize;
            let copy_bytes = remaining.min(buf_size);
            result.extend_from_slice(&chunk[..copy_bytes]);

            if result.len() as u64 >= real_size {
                break;
            }
        }

        Ok(result)
    }
}

// =====================================================================
// -----| $FILE_NAME helpers |-----
// =====================================================================

impl NtfsFileSystem {

    /// Decode UTF-16LE bytes into a Rust String.
    /// Non-BMP characters and surrogates are replaced with '?'.
    fn utf16le_to_string(&self, data: &[u8]) -> String {
        let mut s = String::new();
        let mut i = 0;
        while i + 1 < data.len() {
            let code = u16::from_le_bytes([data[i], data[i + 1]]);
            i += 2;
            if code == 0 {
                break;
            }
            if let Some(c) = char::from_u32(code as u32) {
                s.push(c);
            } else {
                s.push('?');
            }
        }
        s
    }

    /// Extract the best filename from an MFT record.
    /// Preference order: Win32&DOS (3) > Win32 (1) > POSIX (0).
    /// DOS-only (2) is skipped.
    #[allow(dead_code)]
    fn extract_filename(&self, record: &[u8]) -> Option<String> {
        if record.len() < 22 {
            return None;
        }
        let first = u16::from_le_bytes([record[20], record[21]]) as usize;
        let mut pos = first;
        let mut best: Option<(u8, String)> = None; // (namespace priority, name)

        while pos + 8 <= record.len() {
            let atype = u32::from_le_bytes([
                record[pos], record[pos+1], record[pos+2], record[pos+3],
            ]);
            if atype == ATTR_END {
                break;
            }
            let alen = u32::from_le_bytes([
                record[pos+4], record[pos+5], record[pos+6], record[pos+7],
            ]) as usize;
            if alen < 16 || pos + alen > record.len() {
                break;
            }

            if atype == ATTR_FILE_NAME {
                let attr = &record[pos..pos + alen];
                if let Some(val) = self.resident_value(attr) {
                    if val.len() >= 66 {
                        let name_len = val[64] as usize;
                        let ns       = val[65];

                        if ns != NS_DOS && 66 + name_len * 2 <= val.len() {
                            let name = self.utf16le_to_string(&val[66..66 + name_len * 2]);
                            let prio = match ns {
                                NS_WIN32DOS => 3,
                                NS_WIN32    => 2,
                                NS_POSIX    => 1,
                                _           => 0,
                            };
                            let better = best.as_ref().map_or(true, |(p, _)| prio > *p);
                            if better {
                                best = Some((prio, name));
                            }
                        }
                    }
                }
            }

            pos += alen;
        }

        best.map(|(_, name)| name)
    }

    /// Return true when an MFT record represents a directory.
    fn is_directory(&self, record: &[u8]) -> bool {
        if record.len() < 24 {
            return false;
        }
        let flags = u16::from_le_bytes([record[22], record[23]]);
        flags & MFT_FLAG_DIRECTORY != 0
    }

    /// Return the logical file size (bytes) for an MFT record, consulting
    /// the $DATA attribute (resident length or non-resident real_size).
    fn file_size(&self, record: &[u8]) -> u64 {
        match self.find_attribute(record, ATTR_DATA) {
            None => 0,
            Some(attr) => {
                if attr[8] == 0 {
                    // Resident
                    u32::from_le_bytes([attr[16], attr[17], attr[18], attr[19]]) as u64
                } else if attr.len() >= 56 {
                    // Non-resident
                    u64::from_le_bytes([
                        attr[48], attr[49], attr[50], attr[51],
                        attr[52], attr[53], attr[54], attr[55],
                    ])
                } else {
                    0
                }
            }
        }
    }
}

// =====================================================================
// -----| Directory index helpers |-----
// =====================================================================

/// A lightweight entry produced while scanning an index node.
struct RawIndexEntry {
    file_ref_num: u64,
    name:         String,
    is_dir:       bool,
}

impl NtfsFileSystem {

    /// Parse the index entries stored in a single index node buffer
    /// (either the inline part of $INDEX_ROOT or an INDX block).
    ///
    /// `node` must start at the **Index Node Header** (offset 0 = start
    /// offset field), not at the attribute header or INDEX_ROOT header.
    fn parse_index_node(&self, node: &[u8], out: &mut Vec<RawIndexEntry>) {
        if node.len() < 16 {
            return;
        }

        let entry_start = u32::from_le_bytes([node[0], node[1], node[2], node[3]]) as usize;
        let used_size   = u32::from_le_bytes([node[4], node[5], node[6], node[7]]) as usize;

        if entry_start >= used_size || used_size > node.len() {
            return;
        }

        let mut pos = entry_start;

        while pos + 16 <= used_size && pos + 16 <= node.len() {
            // Index Entry header (16 bytes minimum):
            //   0: file reference (u64)
            //   8: entry length (u16)
            //  10: key attribute length (u16) — 0 for the "last" sentinel
            //  12: flags (u16)
            //  14: padding (u16)
            let file_ref  = u64::from_le_bytes([
                node[pos],   node[pos+1], node[pos+2], node[pos+3],
                node[pos+4], node[pos+5], node[pos+6], node[pos+7],
            ]);
            let entry_len = u16::from_le_bytes([node[pos+8],  node[pos+9]])  as usize;
            let key_len   = u16::from_le_bytes([node[pos+10], node[pos+11]]) as usize;
            let flags     = u16::from_le_bytes([node[pos+12], node[pos+13]]);

            if entry_len == 0 {
                break;
            }

            let is_last = flags & IE_FLAG_LAST != 0;

            if !is_last && key_len >= 66 && pos + 16 + key_len <= node.len() {
                // The key is a raw $FILE_NAME attribute value.
                let key = &node[pos + 16..pos + 16 + key_len];
                let name_len = key[64] as usize;
                let ns       = key[65];

                // Skip DOS-only entries — they duplicate Win32 names.
                if ns != NS_DOS && 66 + name_len * 2 <= key.len() {
                    let name = self.utf16le_to_string(&key[66..66 + name_len * 2]);

                    // Filter out NTFS metadata files (start with '$').
                    if !name.starts_with('$') && !name.is_empty() {
                        // Bit 28 of FILE_ATTRIBUTES = directory in $FILE_NAME
                        let fa = u32::from_le_bytes([key[56], key[57], key[58], key[59]]);
                        let is_dir = (fa & 0x1000_0000) != 0 || (fa & 0x10) != 0;

                        // file_ref lower 48 bits = MFT record number
                        let record_num = file_ref & 0x0000_FFFF_FFFF_FFFF;

                        // Avoid duplicates from POSIX vs Win32 namespaces
                        let already = out.iter().any(|e| e.file_ref_num == record_num);
                        if !already {
                            out.push(RawIndexEntry { file_ref_num: record_num, name, is_dir });
                        }
                    }
                }
            }

            pos += entry_len;
            if is_last {
                break;
            }
        }
    }

    /// Collect all index entries for the directory at MFT record `dir_num`.
    ///
    /// List all entries in directory MFT record `dir_num`.
    ///
    /// Reads the resident $INDEX_ROOT first (always present), then walks every
    /// INDX block in the non-resident $INDEX_ALLOCATION stream so that large
    /// directories with more entries than fit in the root are fully enumerated.
    fn list_directory(
        &self,
        device:  &dyn BlockDevice,
        dir_num: u64,
    ) -> Result<Vec<RawIndexEntry>, VfsError> {
        let record = self.read_mft_record(device, dir_num)?;

        if !self.is_directory(&record) {
            return Err(VfsError::NotADirectory);
        }

        let mut entries = Vec::new();

        // ----------------------------------------------------------------
        // 1. $INDEX_ROOT — resident B-tree root node (always present).
        //    Layout of the attribute value:
        //      0..4   : attribute type indexed (0x30 = $FILE_NAME)
        //      4..8   : collation rule
        //      8..12  : index block size in bytes
        //     12..16  : clusters per INDX block (1 byte + 3 pad)
        //     16..    : Index Node Header + index entries
        // ----------------------------------------------------------------
        let index_block_size: u32;

        if let Some(attr) = self.find_attribute(&record, ATTR_INDEX_ROOT) {
            if let Some(val) = self.resident_value(attr) {
                if val.len() >= 16 {
                    index_block_size = u32::from_le_bytes([val[8], val[9], val[10], val[11]]);
                    self.parse_index_node(&val[16..], &mut entries);
                } else {
                    index_block_size = self.bytes_per_cluster;
                }
            } else {
                index_block_size = self.bytes_per_cluster;
            }
        } else {
            return Ok(entries);
        }

        // ----------------------------------------------------------------
        // 2. $INDEX_ALLOCATION — non-resident stream of INDX blocks.
        //    Each block is a self-contained B-tree node with its own Index
        //    Node Header.  We read every block and parse its entries.
        //
        //    INDX block layout:
        //      0..4   : signature "INDX"
        //      4..6   : USA offset
        //      6..8   : USA entry count
        //      8..16  : $LogFile sequence number (ignored)
        //     16..24  : this block's VCN
        //     24..    : Index Node Header + index entries
        // ----------------------------------------------------------------
        if let Some(ia_attr) = self.find_attribute(&record, ATTR_INDEX_ALLOC) {
            if let Some((runs, real_size, _)) = self.nonresident_info(ia_attr) {
                let runs_owned: Vec<u8> = runs.to_vec();
                if let Ok(ia_data) = self.decode_and_read_runs(device, &runs_owned, real_size) {
                    let block_sz = if index_block_size > 0 {
                        index_block_size as usize
                    } else {
                        4096
                    };

                    let mut offset = 0usize;
                    while offset + block_sz <= ia_data.len() {
                        let block = &ia_data[offset..offset + block_sz];
                        offset += block_sz;

                        // Validate INDX signature; skip sparse/unused blocks.
                        if block.len() < 4 || &block[0..4] != INDX_SIGNATURE {
                            continue;
                        }

                        // Apply USA fixup so sector-end words are correct.
                        let mut block_buf = block.to_vec();
                        let _ = self.apply_usa_fixup(&mut block_buf);

                        // Index Node Header begins at byte 24 in an INDX block.
                        if block_buf.len() > 24 {
                            self.parse_index_node(&block_buf[24..], &mut entries);
                        }
                    }
                }
            }
        }

        Ok(entries)
    }

    /// Find the MFT record number for `component` inside directory
    /// `dir_num`, comparing case-insensitively (ASCII range only).
    fn find_in_dir(
        &self,
        device:  &dyn BlockDevice,
        dir_num: u64,
        component: &str,
    ) -> Result<u64, VfsError> {
        let entries = self.list_directory(device, dir_num)?;
        for e in entries {
            if e.name.eq_ignore_ascii_case(component) {
                return Ok(e.file_ref_num);
            }
        }
        Err(VfsError::FileNotFound)
    }

    /// Walk `path` (e.g. "/boot/kernel.bin") from the root directory
    /// and return the MFT record number of the target.
    fn resolve_path(&self, device: &dyn BlockDevice, path: &str) -> Result<u64, VfsError> {
        let components: Vec<&str> = path
            .split('/')
            .filter(|s| !s.is_empty())
            .collect();

        let mut cur = MFT_NUM_ROOT;
        for component in &components {
            cur = self.find_in_dir(device, cur, component)?;
        }
        Ok(cur)
    }
}

// =====================================================================
// -----| FileSystem trait implementation |-----
// =====================================================================

impl FileSystem for NtfsFileSystem {

    fn list_dir(&self, device: &dyn BlockDevice, path: &str) -> Result<Vec<DirEntry>, VfsError> {
        let dir_num = if path.is_empty() || path == "/" {
            MFT_NUM_ROOT
        } else {
            self.resolve_path(device, path)?
        };

        let raw = self.list_directory(device, dir_num)?;
        let mut result = Vec::with_capacity(raw.len());

        for entry in raw {
            // Best-effort: try to read the child record for its file size.
            let size = self.read_mft_record(device, entry.file_ref_num)
                .map(|r| self.file_size(&r))
                .unwrap_or(0);

            result.push(DirEntry {
                name:      entry.name,
                size,
                cluster:   entry.file_ref_num as u32, // lower 32 bits
                file_type: if entry.is_dir { FileType::Directory } else { FileType::File },
            });
        }

        Ok(result)
    }

    fn read_file(&self, device: &dyn BlockDevice, path: &str) -> Result<Vec<u8>, VfsError> {
        let record_num = self.resolve_path(device, path)?;
        let record     = self.read_mft_record(device, record_num)?;

        if self.is_directory(&record) {
            return Err(VfsError::IsDirectory);
        }

        match self.find_attribute(&record, ATTR_DATA) {
            None => Err(VfsError::FileNotFound),

            Some(attr) => {
                if attr[8] == 0 {
                    // --- Resident $DATA ---
                    self.resident_value(attr)
                        .map(|v| v.to_vec())
                        .ok_or(VfsError::ReadError)
                } else {
                    // --- Non-resident $DATA ---
                    let (runs, real_sz, _) =
                        self.nonresident_info(attr).ok_or(VfsError::ReadError)?;
                    self.decode_and_read_runs(device, runs, real_sz)
                }
            }
        }
    }

    /// NTFS write support is not yet implemented.
    fn write_file(&self, _device: &dyn BlockDevice, _path: &str, _data: &[u8]) -> Result<(), VfsError> {
        Err(VfsError::WriteError)
    }
}

// =====================================================================
// -----| Detection and mounting |-----
// =====================================================================

/// Probe for an NTFS volume on AHCI port `port`.
///
/// Reads the first sector, checks the NTFS OEM ID, then parses the boot
/// sector to derive the filesystem parameters and mounts the volume to
/// the VFS root ("/").  Silently returns if the volume is not NTFS.
pub fn probe_ntfs(
    port:                    &mut HbaPort,
    physical_memory_offset:  VirtAddr,
    partition_lba:           u64,
) {
    // Sector buffer the HBA DMAs into: below 4 GiB, kept for the mount's lifetime.
    let (buf_phys, _) = crate::memory::DmaRegion::new(4096).expect("No DMA memory for NTFS probe").leak();
    let buf_virt   = physical_memory_offset.as_u64() + buf_phys;

    if !port.read_sectors(partition_lba, 1, buf_phys, physical_memory_offset) {
        return;
    }

    let raw = unsafe { core::slice::from_raw_parts(buf_virt as *const u8, 512) };

    // Must have the NTFS OEM ID at offset 3
    if &raw[3..11] != NTFS_OEM_ID {
        return;
    }

    let bpb = unsafe { &*(buf_virt as *const NtfsBootSector) };

    let bytes_per_sector    = bpb.bytes_per_sector    as u32;
    let sectors_per_cluster = bpb.sectors_per_cluster as u32;
    let bytes_per_cluster   = bytes_per_sector * sectors_per_cluster;

    // If clusters_per_mft_rec < 0, record size = 2^(-clusters_per_mft_rec).
    // Otherwise, record size = clusters_per_mft_rec * bytes_per_cluster.
    let mft_record_size = if bpb.clusters_per_mft_rec < 0 {
        1u32 << (-(bpb.clusters_per_mft_rec as i32) as u32)
    } else {
        bpb.clusters_per_mft_rec as u32 * bytes_per_cluster
    };

    println!("\n  -> NTFS File System Detected!");
    println!("     - Bytes/Sector: {}", bytes_per_sector);
    println!("     - Sectors/Cluster: {}", sectors_per_cluster);
    let mft_lcn = bpb.mft_lcn;
    println!("     - MFT LCN: {}", mft_lcn);
    println!("     - MFT Record Size: {} bytes", mft_record_size);

    let fs = NtfsFileSystem {
        bytes_per_sector,
        sectors_per_cluster,
        bytes_per_cluster,
        mft_lcn: bpb.mft_lcn,
        mft_record_size,
    };

    let device = crate::ahci::AhciBlockDevice {
        port: port as *mut HbaPort,
        physical_memory_offset,
        bounce_buffer_phys: buf_phys,
        partition_lba,
    };

    use alloc::boxed::Box;
    let mut vfs = crate::vfs::VFS.lock();
    vfs.mount(
        alloc::string::String::from("/"),
        Box::new(fs),
        Box::new(device),
    );

    println!("     - Mounted NTFS to root (/)");

    // Quick sanity check: try to list the root directory.
    if let Ok(entries) = vfs.list_dir("/") {
        println!("     - Root entries: {}", entries.len());
        for e in entries.iter().take(5) {
            println!("       {:?}  {}", e.file_type, e.name);
        }
    }
}
