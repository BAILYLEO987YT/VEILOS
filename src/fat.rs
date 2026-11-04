// =============================================================================
// VEILOS: FAT12 / FAT16 / FAT32
// =============================================================================
//
// One driver for all three FAT variants, reading and writing: files and
// directories anywhere in the tree, VFAT long file names (read and written),
// create/overwrite, delete, mkdir.
//
// It replaced fat16.rs (read-only) and fat32.rs (writes to the root
// directory only, 8.3 names only). QEMU's default `fat:rw:` disk is FAT16, so
// until then nothing the OS saved (users, settings, editor files) reached
// the disk.
//
// Sector I/O goes through the block layer, whose cache holds the FAT and
// directory sectors. A per-volume lock serialises operations, so a reader
// never sees the FAT or a directory half-updated.
//
// Not done yet: keeping the FAT32 FSInfo free count (it is marked "unknown"
// on the first write, which makes other systems recount), last-access
// dates, and file handles/seek (the VFS API is whole-file).

use alloc::{boxed::Box, string::String, vec, vec::Vec};
use spin::Mutex;

use crate::vfs::{BlockDevice, DirEntry, FileState, FileSystem, FileType, Stat, VfsError};

const SECTOR: usize = 512;
const ENTRY: usize = 32;
const ENTRIES_PER_SECTOR: usize = SECTOR / ENTRY;

const ATTR_VOLUME: u8 = 0x08;
const ATTR_DIR: u8 = 0x10;
const ATTR_ARCHIVE: u8 = 0x20;
const ATTR_LFN: u8 = 0x0F;

/// Byte 12 of a short entry: base / extension stored lowercase (no LFN needed).
const NT_LOWER_BASE: u8 = 0x08;
const NT_LOWER_EXT: u8 = 0x10;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FatType {
    Fat12,
    Fat16,
    Fat32,
}

/// Where a directory's entries live.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Dir {
    /// FAT12/16 root: a fixed run of sectors before the data area.
    FixedRoot,
    /// A cluster chain starting here.
    Chain(u32),
}

/// One directory entry, with its long name if it has one.
#[derive(Clone, Debug)]
struct Item {
    name: String,
    short: [u8; 11],
    attr: u8,
    cluster: u32,
    size: u32,
    mtime: i64,
    /// First slot belonging to this entry (its first LFN slot, if any).
    first_slot: usize,
    /// Slot of the short entry.
    slot: usize,
}

impl Item {
    fn is_dir(&self) -> bool {
        self.attr & ATTR_DIR != 0
    }

    fn is_dot(&self) -> bool {
        self.short[0] == b'.'
    }
}

struct State {
    /// Where the next free-cluster search starts.
    next_free: u32,
    fsinfo_invalidated: bool,
}

pub struct FatFileSystem {
    pub fat_type: FatType,
    sectors_per_cluster: u32,
    reserved: u32,
    fat_count: u32,
    sectors_per_fat: u32,
    root_dir_sectors: u32,
    root_cluster: u32,
    first_data_sector: u32,
    /// Valid clusters are 2 ..= cluster_count + 1.
    cluster_count: u32,
    fs_info_sector: u32,
    /// QEMU's fat:rw: folder disk. It stops QEMU with an assertion when it sees
    /// two entries for one file, as a rename does for a moment, so here a file
    /// moves by copying and a folder can't move.
    vvfat: bool,
    state: Mutex<State>,
}

fn le16(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}

fn le32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

/// Recognise a FAT12/16/32 volume on `device`. The variant is decided by
/// cluster count, as the FAT specification requires.
pub fn detect(device: &dyn BlockDevice) -> Option<(Box<dyn FileSystem>, &'static str)> {
    let b = crate::block::read_boot_sector(device)?;
    if le16(&b, 510) != 0xAA55 {
        return None;
    }
    let bytes_per_sector = le16(&b, 11) as u32;
    let spc = b[13] as u32;
    let reserved = le16(&b, 14) as u32;
    let fat_count = b[16] as u32;
    let root_entries = le16(&b, 17) as u32;
    let total16 = le16(&b, 19) as u32;
    let spf16 = le16(&b, 22) as u32;
    let total32 = le32(&b, 32);
    let spf32 = le32(&b, 36);

    if spc == 0 || !spc.is_power_of_two() || reserved == 0 || fat_count == 0 {
        return None;
    }
    if bytes_per_sector != SECTOR as u32 {
        crate::serial_println!("[FAT] {}-byte sectors not supported", bytes_per_sector);
        return None;
    }
    let sectors_per_fat = if spf16 != 0 { spf16 } else { spf32 };
    let total = if total16 != 0 { total16 } else { total32 };
    let root_dir_sectors = (root_entries * ENTRY as u32).div_ceil(SECTOR as u32);
    let first_data_sector = reserved + fat_count * sectors_per_fat + root_dir_sectors;
    if sectors_per_fat == 0 || total <= first_data_sector {
        return None;
    }
    let cluster_count = (total - first_data_sector) / spc;

    let fat_type = if cluster_count < 4085 {
        FatType::Fat12
    } else if cluster_count < 65525 {
        FatType::Fat16
    } else {
        FatType::Fat32
    };
    if (fat_type == FatType::Fat32) != (root_entries == 0) {
        return None; // inconsistent BPB
    }
    let (root_cluster, fs_info_sector) = if fat_type == FatType::Fat32 {
        (le32(&b, 44), le16(&b, 48) as u32)
    } else {
        (0, 0)
    };

    let name = match fat_type {
        FatType::Fat12 => "FAT12",
        FatType::Fat16 => "FAT16",
        FatType::Fat32 => "FAT32",
    };
    crate::serial_println!(
        "[FAT] {}: {} clusters of {} bytes, {} FAT(s) of {} sectors",
        name, cluster_count, spc * SECTOR as u32, fat_count, sectors_per_fat
    );
    let fs = FatFileSystem {
        fat_type,
        sectors_per_cluster: spc,
        reserved,
        fat_count,
        sectors_per_fat,
        root_dir_sectors,
        root_cluster,
        first_data_sector,
        cluster_count,
        fs_info_sector,
        vvfat: b[43..54] == *b"QEMU VVFAT " || b[71..82] == *b"QEMU VVFAT ",
        state: Mutex::new(State { next_free: 2, fsinfo_invalidated: false }),
    };
    Some((Box::new(fs), name))
}

impl FatFileSystem {
    // -----| raw access |-----

    fn read_sector(&self, dev: &dyn BlockDevice, lba: u64) -> Result<[u8; SECTOR], VfsError> {
        let mut buf = [0u8; SECTOR];
        if dev.read_sectors(lba, 1, buf.as_mut_ptr() as u64) { Ok(buf) } else { Err(VfsError::ReadError) }
    }

    fn write_sector(&self, dev: &dyn BlockDevice, lba: u64, buf: &[u8; SECTOR]) -> Result<(), VfsError> {
        if dev.write_sectors(lba, 1, buf.as_ptr() as u64) { Ok(()) } else { Err(VfsError::WriteError) }
    }

    fn cluster_bytes(&self) -> usize {
        self.sectors_per_cluster as usize * SECTOR
    }

    fn cluster_lba(&self, cluster: u32) -> u64 {
        (self.first_data_sector + (cluster - 2) * self.sectors_per_cluster) as u64
    }

    fn valid_cluster(&self, c: u32) -> bool {
        c >= 2 && c < self.cluster_count + 2
    }

    fn root_dir(&self) -> Dir {
        if self.fat_type == FatType::Fat32 { Dir::Chain(self.root_cluster) } else { Dir::FixedRoot }
    }

    /// The directory a `..` (or any directory entry) cluster number names.
    fn dir_at(&self, cluster: u32) -> Dir {
        if cluster == 0 { self.root_dir() } else { Dir::Chain(cluster) }
    }

    // -----| the FAT |-----

    fn eoc(&self) -> u32 {
        match self.fat_type {
            FatType::Fat12 => 0xFFF,
            FatType::Fat16 => 0xFFFF,
            FatType::Fat32 => 0x0FFF_FFFF,
        }
    }

    /// True for end-of-chain and bad-cluster markers.
    fn is_end(&self, v: u32) -> bool {
        match self.fat_type {
            FatType::Fat12 => v >= 0xFF7,
            FatType::Fat16 => v >= 0xFFF7,
            FatType::Fat32 => v >= 0x0FFF_FFF7,
        }
    }

    /// Byte offset of `cluster`'s entry within a FAT.
    fn fat_offset(&self, cluster: u32) -> u32 {
        match self.fat_type {
            FatType::Fat12 => cluster + cluster / 2,
            FatType::Fat16 => cluster * 2,
            FatType::Fat32 => cluster * 4,
        }
    }

    fn fat_get(&self, dev: &dyn BlockDevice, cluster: u32) -> Result<u32, VfsError> {
        let off = self.fat_offset(cluster);
        let lba = (self.reserved + off / SECTOR as u32) as u64;
        let i = (off % SECTOR as u32) as usize;
        let s = self.read_sector(dev, lba)?;
        Ok(match self.fat_type {
            FatType::Fat12 => {
                // A 12-bit entry can straddle two sectors.
                let hi = if i == SECTOR - 1 { self.read_sector(dev, lba + 1)?[0] } else { s[i + 1] };
                let v = u16::from_le_bytes([s[i], hi]) as u32;
                if cluster & 1 == 1 { v >> 4 } else { v & 0xFFF }
            }
            FatType::Fat16 => le16(&s, i) as u32,
            FatType::Fat32 => le32(&s, i) & 0x0FFF_FFFF,
        })
    }

    /// Set `cluster`'s entry in every FAT copy.
    fn fat_set(&self, dev: &dyn BlockDevice, cluster: u32, value: u32) -> Result<(), VfsError> {
        let off = self.fat_offset(cluster);
        for copy in 0..self.fat_count {
            let lba = (self.reserved + copy * self.sectors_per_fat + off / SECTOR as u32) as u64;
            let i = (off % SECTOR as u32) as usize;
            let mut s = self.read_sector(dev, lba)?;
            match self.fat_type {
                FatType::Fat12 => {
                    let straddles = i == SECTOR - 1;
                    let mut next = if straddles { self.read_sector(dev, lba + 1)? } else { [0u8; SECTOR] };
                    let hi_byte = if straddles { next[0] } else { s[i + 1] };
                    let old = u16::from_le_bytes([s[i], hi_byte]);
                    let v = (value & 0xFFF) as u16;
                    let new = if cluster & 1 == 1 { (old & 0x000F) | (v << 4) } else { (old & 0xF000) | v };
                    let [lo, hi] = new.to_le_bytes();
                    s[i] = lo;
                    if straddles {
                        next[0] = hi;
                        self.write_sector(dev, lba + 1, &next)?;
                    } else {
                        s[i + 1] = hi;
                    }
                }
                FatType::Fat16 => s[i..i + 2].copy_from_slice(&(value as u16).to_le_bytes()),
                FatType::Fat32 => {
                    // The top 4 bits are reserved and must be preserved.
                    let v = (le32(&s, i) & 0xF000_0000) | (value & 0x0FFF_FFFF);
                    s[i..i + 4].copy_from_slice(&v.to_le_bytes());
                }
            }
            self.write_sector(dev, lba, &s)?;
        }
        Ok(())
    }

    /// The clusters of the chain starting at `start` (empty for 0).
    fn chain(&self, dev: &dyn BlockDevice, start: u32) -> Result<Vec<u32>, VfsError> {
        let mut out = Vec::new();
        let mut c = start;
        while self.valid_cluster(c) {
            if out.len() > self.cluster_count as usize {
                crate::serial_println!("[FAT] cluster chain loop at {}", start);
                return Err(VfsError::ReadError);
            }
            out.push(c);
            let next = self.fat_get(dev, c)?;
            if self.is_end(next) {
                break;
            }
            c = next;
        }
        Ok(out)
    }

    /// Allocate one free cluster and mark it end-of-chain.
    fn alloc_cluster(&self, dev: &dyn BlockDevice, st: &mut State) -> Result<u32, VfsError> {
        self.invalidate_fsinfo(dev, st)?;
        let n = self.cluster_count;
        let start = st.next_free.clamp(2, n + 1);
        for k in 0..n {
            let c = 2 + (start - 2 + k) % n;
            if self.fat_get(dev, c)? == 0 {
                self.fat_set(dev, c, self.eoc())?;
                st.next_free = c + 1;
                return Ok(c);
            }
        }
        crate::serial_println!("[FAT] volume full");
        Err(VfsError::WriteError)
    }

    /// Allocate a linked chain of `n` clusters. On failure nothing stays allocated.
    fn alloc_chain(&self, dev: &dyn BlockDevice, st: &mut State, n: usize) -> Result<Vec<u32>, VfsError> {
        let mut chain: Vec<u32> = Vec::with_capacity(n);
        for _ in 0..n {
            match self.alloc_cluster(dev, st) {
                Ok(c) => {
                    if let Some(&prev) = chain.last() {
                        self.fat_set(dev, prev, c)?;
                    }
                    chain.push(c);
                }
                Err(e) => {
                    if let Some(&first) = chain.first() {
                        let _ = self.free_chain(dev, first);
                    }
                    return Err(e);
                }
            }
        }
        Ok(chain)
    }

    fn free_chain(&self, dev: &dyn BlockDevice, start: u32) -> Result<(), VfsError> {
        for c in self.chain(dev, start)? {
            self.fat_set(dev, c, 0)?;
        }
        Ok(())
    }

    /// FAT32 keeps a free-cluster count in its FSInfo sector. Rather than
    /// maintain it, mark it unknown before the first change; other systems
    /// then recount instead of trusting a stale number.
    fn invalidate_fsinfo(&self, dev: &dyn BlockDevice, st: &mut State) -> Result<(), VfsError> {
        if st.fsinfo_invalidated || self.fat_type != FatType::Fat32 || self.fs_info_sector == 0 {
            return Ok(());
        }
        st.fsinfo_invalidated = true;
        let lba = self.fs_info_sector as u64;
        let mut s = self.read_sector(dev, lba)?;
        if le32(&s, 0) == 0x4161_5252 && le32(&s, 484) == 0x6141_7272 {
            s[488..496].fill(0xFF);
            self.write_sector(dev, lba, &s)?;
        }
        Ok(())
    }

    fn zero_cluster(&self, dev: &dyn BlockDevice, cluster: u32) -> Result<(), VfsError> {
        let zeros = vec![0u8; self.cluster_bytes()];
        if dev.write_sectors(self.cluster_lba(cluster), self.sectors_per_cluster, zeros.as_ptr() as u64) {
            Ok(())
        } else {
            Err(VfsError::WriteError)
        }
    }

    // -----| directories |-----

    /// LBAs of every sector holding `dir`'s entries, in order.
    fn dir_sectors(&self, dev: &dyn BlockDevice, dir: Dir) -> Result<Vec<u64>, VfsError> {
        Ok(match dir {
            Dir::FixedRoot => {
                let start = (self.reserved + self.fat_count * self.sectors_per_fat) as u64;
                (start..start + self.root_dir_sectors as u64).collect()
            }
            Dir::Chain(c) => self
                .chain(dev, c)?
                .into_iter()
                .flat_map(|c| {
                    let lba = self.cluster_lba(c);
                    lba..lba + self.sectors_per_cluster as u64
                })
                .collect(),
        })
    }

    /// All raw 32-byte slots of `dir`, plus the sector each block of 16 came from.
    fn read_slots(&self, dev: &dyn BlockDevice, dir: Dir) -> Result<(Vec<u64>, Vec<u8>), VfsError> {
        let lbas = self.dir_sectors(dev, dir)?;
        let mut raw = vec![0u8; lbas.len() * SECTOR];
        for (k, &lba) in lbas.iter().enumerate() {
            raw[k * SECTOR..(k + 1) * SECTOR].copy_from_slice(&self.read_sector(dev, lba)?);
        }
        Ok((lbas, raw))
    }

    fn read_dir(&self, dev: &dyn BlockDevice, dir: Dir) -> Result<Vec<Item>, VfsError> {
        let (_, raw) = self.read_slots(dev, dir)?;
        Ok(parse_dir(&raw))
    }

    fn lookup(&self, dev: &dyn BlockDevice, dir: Dir, name: &str) -> Result<Option<Item>, VfsError> {
        Ok(self.read_dir(dev, dir)?.into_iter().find(|it| {
            it.name.eq_ignore_ascii_case(name) || short_to_string(&it.short, 0).eq_ignore_ascii_case(name)
        }))
    }

    /// The directory at `path` (relative to this volume).
    fn resolve_dir(&self, dev: &dyn BlockDevice, path: &str) -> Result<Dir, VfsError> {
        let mut dir = self.root_dir();
        for part in path.split('/').filter(|s| !s.is_empty()) {
            let item = self.lookup(dev, dir, part)?.ok_or(VfsError::FileNotFound)?;
            if !item.is_dir() {
                return Err(VfsError::NotADirectory);
            }
            dir = self.dir_at(item.cluster);
        }
        Ok(dir)
    }

    /// (parent directory, final component) of `path`.
    fn resolve_parent<'a>(&self, dev: &dyn BlockDevice, path: &'a str) -> Result<(Dir, &'a str), VfsError> {
        let trimmed = path.trim_end_matches('/');
        let (parent, name) = match trimmed.rfind('/') {
            Some(i) => (&trimmed[..i], &trimmed[i + 1..]),
            None => ("", trimmed),
        };
        if name.is_empty() || name == "." || name == ".." {
            return Err(VfsError::GenericError);
        }
        Ok((self.resolve_dir(dev, parent)?, name))
    }

    /// Write slots `[first, first + entries.len()/32)` of `dir`.
    fn write_slots(
        &self,
        dev: &dyn BlockDevice,
        lbas: &[u64],
        raw: &mut [u8],
        first: usize,
        entries: &[u8],
    ) -> Result<(), VfsError> {
        raw[first * ENTRY..first * ENTRY + entries.len()].copy_from_slice(entries);
        let last = first + entries.len() / ENTRY - 1;
        for k in first / ENTRIES_PER_SECTOR..=last / ENTRIES_PER_SECTOR {
            let s: &[u8; SECTOR] = raw[k * SECTOR..(k + 1) * SECTOR].try_into().unwrap();
            self.write_sector(dev, lbas[k], s)?;
        }
        Ok(())
    }

    /// Append one zeroed cluster to a cluster-chain directory.
    fn grow_dir(&self, dev: &dyn BlockDevice, st: &mut State, dir: Dir) -> Result<(), VfsError> {
        let Dir::Chain(start) = dir else {
            crate::serial_println!("[FAT] root directory is full");
            return Err(VfsError::WriteError);
        };
        let last = *self.chain(dev, start)?.last().ok_or(VfsError::ReadError)?;
        let c = self.alloc_cluster(dev, st)?;
        self.zero_cluster(dev, c)?;
        self.fat_set(dev, last, c)
    }

    /// Add an entry named `name` to `dir`, with LFN slots if the name needs them.
    fn create_entry(
        &self,
        dev: &dyn BlockDevice,
        st: &mut State,
        dir: Dir,
        name: &str,
        attr: u8,
        cluster: u32,
        size: u32,
    ) -> Result<(), VfsError> {
        let items = self.read_dir(dev, dir)?;
        let (short, case_flags, needs_lfn) = make_short_name(name, &items)?;
        let utf16: Vec<u16> = name.encode_utf16().collect();
        let lfn_slots = if needs_lfn { utf16.len().div_ceil(13) } else { 0 };
        if lfn_slots > 20 {
            return Err(VfsError::GenericError); // over 255 characters
        }
        let needed = lfn_slots + 1;

        let mut entries = Vec::with_capacity(needed * ENTRY);
        let sum = lfn_checksum(&short);
        for seq in (1..=lfn_slots).rev() {
            entries.extend_from_slice(&lfn_entry(&utf16, seq, seq == lfn_slots, sum));
        }
        entries.extend_from_slice(&short_entry(&short, case_flags, attr, cluster, size));

        // Find `needed` consecutive free slots, growing the directory if needed
        // (one cluster can hold fewer slots than a 255-character name needs).
        for _ in 0..3 {
            let (lbas, mut raw) = self.read_slots(dev, dir)?;
            let slots = raw.len() / ENTRY;
            let mut run = 0;
            for i in 0..slots {
                let b0 = raw[i * ENTRY];
                if b0 == 0x00 || b0 == 0xE5 {
                    run += 1;
                    if run == needed {
                        return self.write_slots(dev, &lbas, &mut raw, i + 1 - needed, &entries);
                    }
                } else {
                    run = 0;
                }
            }
            self.grow_dir(dev, st, dir)?;
        }
        Err(VfsError::WriteError)
    }

    /// Rewrite the short entry of `item` in `dir` via `f`.
    fn update_entry(&self, dev: &dyn BlockDevice, dir: Dir, item: &Item, f: impl FnOnce(&mut [u8])) -> Result<(), VfsError> {
        let (lbas, mut raw) = self.read_slots(dev, dir)?;
        let mut e = [0u8; ENTRY];
        e.copy_from_slice(&raw[item.slot * ENTRY..(item.slot + 1) * ENTRY]);
        f(&mut e);
        self.write_slots(dev, &lbas, &mut raw, item.slot, &e)
    }

    fn remove_entry(&self, dev: &dyn BlockDevice, dir: Dir, item: &Item) -> Result<(), VfsError> {
        let (lbas, mut raw) = self.read_slots(dev, dir)?;
        let mut slots = vec![0u8; (item.slot - item.first_slot + 1) * ENTRY];
        slots.copy_from_slice(&raw[item.first_slot * ENTRY..(item.slot + 1) * ENTRY]);
        for e in slots.chunks_mut(ENTRY) {
            e[0] = 0xE5;
        }
        self.write_slots(dev, &lbas, &mut raw, item.first_slot, &slots)
    }

    fn read_chain_data(&self, dev: &dyn BlockDevice, start: u32, size: usize) -> Result<Vec<u8>, VfsError> {
        let chain = self.chain(dev, start)?;
        let cb = self.cluster_bytes();
        let mut data = vec![0u8; chain.len() * cb];
        // Read runs of consecutive clusters with one request each.
        let mut i = 0;
        while i < chain.len() {
            let mut j = i + 1;
            while j < chain.len() && chain[j] == chain[j - 1] + 1 {
                j += 1;
            }
            let sectors = ((j - i) as u32) * self.sectors_per_cluster;
            if !dev.read_sectors(self.cluster_lba(chain[i]), sectors, data[i * cb..].as_mut_ptr() as u64) {
                return Err(VfsError::ReadError);
            }
            i = j;
        }
        data.truncate(size.min(data.len()));
        Ok(data)
    }
}

// -----| open files |-----

impl FatFileSystem {
    fn load_chain(&self, dev: &dyn BlockDevice, f: &mut FileState) -> Result<(), VfsError> {
        if !f.chain_loaded {
            f.chain = if f.first != 0 { self.chain(dev, f.first)? } else { Vec::new() };
            f.chain_loaded = true;
        }
        Ok(())
    }

    /// Clusters from `off` on: (sector, bytes available in consecutive
    /// clusters, limited to `want`).
    fn extent(&self, f: &FileState, off: u64, want: usize) -> Option<(u64, usize)> {
        let cb = self.cluster_bytes() as u64;
        let mut k = (off / cb) as usize;
        let first = *f.chain.get(k)?;
        let inside = off % cb;
        let mut run = cb - inside;
        while (run as usize) < want && f.chain.get(k + 1) == Some(&(f.chain[k] + 1)) {
            k += 1;
            run += cb;
        }
        Some((self.cluster_lba(first) + inside / SECTOR as u64, (run as usize).min(want)))
    }

    /// Clusters and directory entry for a file `size` bytes long. The entry
    /// goes before the data: see `write_file`.
    fn resize(&self, dev: &dyn BlockDevice, f: &mut FileState, size: u64) -> Result<(), VfsError> {
        if size > u32::MAX as u64 {
            return Err(VfsError::WriteError);
        }
        self.load_chain(dev, f)?;
        let need = (size as usize).div_ceil(self.cluster_bytes());
        if need > f.chain.len() {
            let extra = self.alloc_chain(dev, &mut self.state.lock(), need - f.chain.len())?;
            match f.chain.last() {
                Some(&last) => self.fat_set(dev, last, extra[0])?,
                None => f.first = extra[0],
            }
            f.chain.extend(extra);
        }
        let old_chain = need < f.chain.len();
        let freed = f.chain.get(need).copied();
        if old_chain {
            f.chain.truncate(need);
            if need == 0 {
                f.first = 0;
            }
        }
        f.size = size;
        self.write_entry(dev, f, None)?;
        if let Some(c) = freed {
            if let Some(&last) = f.chain.last() {
                self.fat_set(dev, last, self.eoc())?;
            }
            self.free_chain(dev, c)?;
        }
        Ok(())
    }

    /// The directory entry as `f` has it; its time from `mtime`, or now.
    fn write_entry(&self, dev: &dyn BlockDevice, f: &mut FileState, mtime: Option<i64>) -> Result<(), VfsError> {
        let (date, time) = mtime.map_or_else(fat_now, filetime_to_fat);
        f.mtime = fat_to_filetime(date, time);
        let mut s = self.read_sector(dev, f.entry_lba)?;
        let e = &mut s[f.entry_off..f.entry_off + ENTRY];
        e[11] |= ATTR_ARCHIVE;
        e[20..22].copy_from_slice(&((f.first >> 16) as u16).to_le_bytes());
        e[26..28].copy_from_slice(&(f.first as u16).to_le_bytes());
        e[28..32].copy_from_slice(&(f.size as u32).to_le_bytes());
        e[22..24].copy_from_slice(&time.to_le_bytes());
        e[24..26].copy_from_slice(&date.to_le_bytes());
        e[18..20].copy_from_slice(&date.to_le_bytes());
        self.write_sector(dev, f.entry_lba, &s)
    }

    /// Write inside the clusters the file already has.
    fn write_inside(&self, dev: &dyn BlockDevice, f: &FileState, mut off: u64, mut data: &[u8]) -> Result<(), VfsError> {
        while !data.is_empty() {
            let (lba, run) = self.extent(f, off, data.len()).ok_or(VfsError::WriteError)?;
            let inside = off as usize % SECTOR;
            if inside != 0 || run < SECTOR {
                let n = run.min(SECTOR - inside);
                let mut s = self.read_sector(dev, lba)?;
                s[inside..inside + n].copy_from_slice(&data[..n]);
                self.write_sector(dev, lba, &s)?;
                off += n as u64;
                data = &data[n..];
            } else {
                let sectors = run / SECTOR;
                if !dev.write_sectors(lba, sectors as u32, data.as_ptr() as u64) {
                    return Err(VfsError::WriteError);
                }
                off += (sectors * SECTOR) as u64;
                data = &data[sectors * SECTOR..];
            }
        }
        Ok(())
    }

    /// Zeros from `from` to `to`, in clusters the file already has.
    fn zero_inside(&self, dev: &dyn BlockDevice, f: &FileState, mut from: u64, to: u64) -> Result<(), VfsError> {
        let zeros = vec![0u8; self.cluster_bytes()];
        while from < to {
            let n = (zeros.len() as u64).min(to - from) as usize;
            self.write_inside(dev, f, from, &zeros[..n])?;
            from += n as u64;
        }
        Ok(())
    }
}

/// A FAT date and time as a FILETIME (100 ns since 1601).
fn fat_to_filetime(date: u16, time: u16) -> i64 {
    let (y, m, d) = ((date >> 9) as i64 + 1980, ((date >> 5) & 15) as i64, (date & 31) as i64);
    if m == 0 || d == 0 {
        return 0;
    }
    // Days since 1970-01-01 (Howard Hinnant's days_from_civil).
    let y2 = if m <= 2 { y - 1 } else { y };
    let era = y2.div_euclid(400);
    let yoe = y2 - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    let secs = days * 86400 + ((time >> 11) as i64) * 3600 + (((time >> 5) & 63) as i64) * 60 + ((time & 31) as i64) * 2;
    (secs + 11_644_473_600) * 10_000_000
}

fn filetime_to_fat(ft: i64) -> (u16, u16) {
    let secs = ft / 10_000_000 - 11_644_473_600;
    let days = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400);
    // civil_from_days.
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    if y < 1980 {
        return ((1 << 5) | 1, 0);
    }
    (
        (((y - 1980).min(127) as u16) << 9) | ((m as u16) << 5) | d as u16,
        (((rem / 3600) as u16) << 11) | ((((rem % 3600) / 60) as u16) << 5) | ((rem % 60 / 2) as u16),
    )
}

impl FileSystem for FatFileSystem {
    fn stat(&self, dev: &dyn BlockDevice, path: &str) -> Option<Stat> {
        let _guard = self.state.lock();
        if path.trim_end_matches('/').is_empty() {
            return Some(Stat { is_dir: true, size: 0, mtime: 0 });
        }
        let (dir, name) = self.resolve_parent(dev, path).ok()?;
        let it = self.lookup(dev, dir, name).ok()??;
        Some(Stat { is_dir: it.is_dir(), size: it.size as u64, mtime: it.mtime })
    }

    fn open_file(&self, dev: &dyn BlockDevice, path: &str) -> Result<FileState, VfsError> {
        let _guard = self.state.lock();
        let (dir, name) = self.resolve_parent(dev, path).map_err(|_| VfsError::FileNotFound)?;
        let item = self.lookup(dev, dir, name)?.ok_or(VfsError::FileNotFound)?;
        if item.is_dir() {
            return Err(VfsError::IsDirectory);
        }
        let lbas = self.dir_sectors(dev, dir)?;
        Ok(FileState {
            path: String::from(path),
            size: item.size as u64,
            mtime: item.mtime,
            first: item.cluster,
            entry_lba: lbas[item.slot / ENTRIES_PER_SECTOR],
            entry_off: item.slot % ENTRIES_PER_SECTOR * ENTRY,
            ..FileState::default()
        })
    }

    fn read_at(&self, dev: &dyn BlockDevice, f: &mut FileState, off: u64, buf: &mut [u8]) -> Result<usize, VfsError> {
        if off >= f.size || buf.is_empty() {
            return Ok(0);
        }
        self.load_chain(dev, f)?;
        let total = buf.len().min((f.size - off) as usize);
        let mut done = 0;
        while done < total {
            let at = off + done as u64;
            let (lba, run) = self.extent(f, at, total - done).ok_or(VfsError::ReadError)?;
            let inside = at as usize % SECTOR;
            if inside != 0 || run < SECTOR {
                let n = run.min(SECTOR - inside);
                let s = self.read_sector(dev, lba)?;
                buf[done..done + n].copy_from_slice(&s[inside..inside + n]);
                done += n;
            } else {
                let sectors = run / SECTOR;
                if !dev.read_sectors(lba, sectors as u32, buf[done..].as_mut_ptr() as u64) {
                    return Err(VfsError::ReadError);
                }
                done += sectors * SECTOR;
            }
        }
        Ok(total)
    }

    fn write_at(&self, dev: &dyn BlockDevice, f: &mut FileState, off: u64, data: &[u8]) -> Result<(), VfsError> {
        if data.is_empty() {
            return Ok(());
        }
        let end = off + data.len() as u64;
        let old = f.size;
        if end > old {
            self.resize(dev, f, end)?;
            if off > old {
                self.zero_inside(dev, f, old, off)?;
            }
        } else {
            self.load_chain(dev, f)?;
        }
        f.dirty = true;
        self.write_inside(dev, f, off, data)
    }

    fn set_len(&self, dev: &dyn BlockDevice, f: &mut FileState, len: u64) -> Result<(), VfsError> {
        let old = f.size;
        if len != old {
            self.resize(dev, f, len)?;
            if len > old {
                self.zero_inside(dev, f, old, len)?;
            }
        }
        Ok(())
    }

    fn sync(&self, dev: &dyn BlockDevice, f: &mut FileState) -> Result<(), VfsError> {
        if f.dirty {
            f.dirty = false;
            self.write_entry(dev, f, None)?;
        }
        Ok(())
    }

    fn set_mtime(&self, dev: &dyn BlockDevice, f: &mut FileState, mtime: i64) -> Result<(), VfsError> {
        f.dirty = false;
        self.write_entry(dev, f, Some(mtime))
    }

    fn rename(&self, dev: &dyn BlockDevice, from: &str, to: &str) -> Result<(), VfsError> {
        if self.vvfat {
            if self.stat(dev, from).is_none_or(|s| s.is_dir) {
                return Err(VfsError::WriteError);
            }
            // Only the case changes: the host folder ignores case too, and QEMU
            // stops if it can't delete what it then sees as gone. The name stays.
            if from.eq_ignore_ascii_case(to) {
                return Ok(());
            }
            let data = self.read_file(dev, from)?;
            self.write_file(dev, to, &data)?;
            return self.delete_file(dev, from);
        }
        let mut st = self.state.lock();
        let (from_dir, from_name) = self.resolve_parent(dev, from)?;
        let item = self.lookup(dev, from_dir, from_name)?.ok_or(VfsError::FileNotFound)?;
        let (to_dir, to_name) = self.resolve_parent(dev, to)?;
        // Only a change of case may find itself there.
        let same_entry = to_dir == from_dir && to_name.eq_ignore_ascii_case(&item.name);
        if !same_entry && self.lookup(dev, to_dir, to_name)?.is_some() {
            return Err(VfsError::WriteError);
        }
        // A directory can't go inside itself.
        if item.is_dir() && to.len() > from.len() && to[..from.len()].eq_ignore_ascii_case(from) && to.as_bytes()[from.len()] == b'/' {
            return Err(VfsError::WriteError);
        }
        // Moved in two steps, so a failure in the middle leaves the data named.
        if same_entry {
            self.remove_entry(dev, from_dir, &item)?;
            self.create_entry(dev, &mut st, to_dir, to_name, item.attr, item.cluster, item.size)?;
        } else {
            self.create_entry(dev, &mut st, to_dir, to_name, item.attr, item.cluster, item.size)?;
            self.remove_entry(dev, from_dir, &item)?;
        }
        // The time goes with it.
        if let Some(moved) = self.lookup(dev, to_dir, to_name)? {
            let (date, time) = filetime_to_fat(item.mtime);
            self.update_entry(dev, to_dir, &moved, |e| {
                e[22..24].copy_from_slice(&time.to_le_bytes());
                e[24..26].copy_from_slice(&date.to_le_bytes());
            })?;
        }
        // A directory's ".." names its parent.
        if item.is_dir() && to_dir != from_dir {
            let parent = match to_dir {
                Dir::Chain(p) if p != self.root_cluster || self.fat_type != FatType::Fat32 => p,
                _ => 0,
            };
            let lba = self.cluster_lba(item.cluster);
            let mut s = self.read_sector(dev, lba)?;
            s[ENTRY + 20..ENTRY + 22].copy_from_slice(&((parent >> 16) as u16).to_le_bytes());
            s[ENTRY + 26..ENTRY + 28].copy_from_slice(&(parent as u16).to_le_bytes());
            self.write_sector(dev, lba, &s)?;
        }
        Ok(())
    }

    fn read_file(&self, dev: &dyn BlockDevice, path: &str) -> Result<Vec<u8>, VfsError> {
        let _guard = self.state.lock();
        let (dir, name) = self.resolve_parent(dev, path).map_err(|_| VfsError::FileNotFound)?;
        let item = self.lookup(dev, dir, name)?.ok_or(VfsError::FileNotFound)?;
        if item.is_dir() {
            return Err(VfsError::IsDirectory);
        }
        self.read_chain_data(dev, item.cluster, item.size as usize)
    }

    fn list_dir(&self, dev: &dyn BlockDevice, path: &str) -> Result<Vec<DirEntry>, VfsError> {
        let _guard = self.state.lock();
        let dir = self.resolve_dir(dev, path)?;
        Ok(self
            .read_dir(dev, dir)?
            .into_iter()
            .filter(|it| !it.is_dot())
            .map(|it| DirEntry {
                file_type: if it.is_dir() { FileType::Directory } else { FileType::File },
                name: it.name,
                size: it.size as u64,
                cluster: it.cluster,
                mtime: it.mtime,
            })
            .collect())
    }

    fn write_file(&self, dev: &dyn BlockDevice, path: &str, data: &[u8]) -> Result<(), VfsError> {
        let mut st = self.state.lock();
        let (dir, name) = self.resolve_parent(dev, path)?;
        let existing = self.lookup(dev, dir, name)?;
        if existing.as_ref().is_some_and(|it| it.is_dir()) {
            return Err(VfsError::IsDirectory);
        }
        let size: u32 = data.len().try_into().map_err(|_| VfsError::WriteError)?; // FAT max 4 GiB - 1

        // Overwrite in place: the file keeps its first cluster and its chain
        // grows or shrinks. Writing to fresh clusters and repointing the entry
        // would survive a failed write better, but QEMU's vvfat (the default
        // `fat:rw:` disk) tracks host files by first cluster and reads a
        // changed first cluster as a delete: the host file disappeared.
        let cb = self.cluster_bytes();
        let need = data.len().div_ceil(cb);
        let old = match &existing {
            Some(it) if it.cluster != 0 => self.chain(dev, it.cluster)?,
            _ => Vec::new(),
        };
        let fresh = old.is_empty();
        let chain: Vec<u32> = if need == 0 {
            Vec::new()
        } else if fresh {
            self.alloc_chain(dev, &mut st, need)?
        } else if need > old.len() {
            let extra = self.alloc_chain(dev, &mut st, need - old.len())?;
            self.fat_set(dev, *old.last().unwrap(), extra[0])?;
            old.iter().copied().chain(extra).collect()
        } else {
            old[..need].to_vec()
        };

        let first = chain.first().copied().unwrap_or(0);
        let (date, time) = fat_now();

        // An existing file's entry goes before its data: vvfat commits a file
        // when its clusters are written, with the size the entry has by then.
        // Written after, the host file keeps the size of the write before.
        if let Some(item) = &existing {
            self.update_entry(dev, dir, item, |e| {
                e[11] |= ATTR_ARCHIVE;
                e[20..22].copy_from_slice(&((first >> 16) as u16).to_le_bytes());
                e[26..28].copy_from_slice(&(first as u16).to_le_bytes());
                e[28..32].copy_from_slice(&size.to_le_bytes());
                e[22..24].copy_from_slice(&time.to_le_bytes());
                e[24..26].copy_from_slice(&date.to_le_bytes());
                e[18..20].copy_from_slice(&date.to_le_bytes());
            })?;
        }

        let mut buf = vec![0u8; cb];
        for (k, &c) in chain.iter().enumerate() {
            let part = &data[k * cb..data.len().min((k + 1) * cb)];
            buf[..part.len()].copy_from_slice(part);
            buf[part.len()..].fill(0);
            if !dev.write_sectors(self.cluster_lba(c), self.sectors_per_cluster, buf.as_ptr() as u64) {
                if fresh && existing.is_none() {
                    let _ = self.free_chain(dev, chain[0]);
                }
                return Err(VfsError::WriteError);
            }
        }
        match existing {
            Some(_) => {
                // Release the clusters the shorter file no longer uses.
                if need == 0 && !old.is_empty() {
                    self.free_chain(dev, old[0])?;
                } else if need > 0 && need < old.len() {
                    self.fat_set(dev, old[need - 1], self.eoc())?;
                    self.free_chain(dev, old[need])?;
                }
                Ok(())
            }
            None => {
                let r = self.create_entry(dev, &mut st, dir, name, ATTR_ARCHIVE, first, size);
                if r.is_err() && first != 0 {
                    let _ = self.free_chain(dev, first);
                }
                r
            }
        }
    }

    fn delete_file(&self, dev: &dyn BlockDevice, path: &str) -> Result<(), VfsError> {
        let _guard = self.state.lock();
        let (dir, name) = self.resolve_parent(dev, path)?;
        let item = self.lookup(dev, dir, name)?.ok_or(VfsError::FileNotFound)?;
        if item.is_dir() {
            // Only empty directories may go.
            let children = self.read_dir(dev, Dir::Chain(item.cluster))?;
            if children.iter().any(|c| !c.is_dot()) {
                return Err(VfsError::IsDirectory);
            }
        }
        self.remove_entry(dev, dir, &item)?;
        if item.cluster != 0 {
            self.free_chain(dev, item.cluster)?;
        }
        Ok(())
    }

    fn create_dir(&self, dev: &dyn BlockDevice, path: &str) -> Result<(), VfsError> {
        let mut st = self.state.lock();
        let (parent, name) = self.resolve_parent(dev, path)?;
        if let Some(item) = self.lookup(dev, parent, name)? {
            return if item.is_dir() { Ok(()) } else { Err(VfsError::NotADirectory) };
        }
        let c = self.alloc_cluster(dev, &mut st)?;
        let made = (|| {
            self.zero_cluster(dev, c)?;
            // "." and "..": `..` holds 0 when the parent is the root.
            let parent_cluster = match parent {
                Dir::Chain(p) if p != self.root_cluster || self.fat_type != FatType::Fat32 => p,
                _ => 0,
            };
            let mut first = [0u8; SECTOR];
            first[..ENTRY].copy_from_slice(&short_entry(b".          ", 0, ATTR_DIR, c, 0));
            first[ENTRY..2 * ENTRY].copy_from_slice(&short_entry(b"..         ", 0, ATTR_DIR, parent_cluster, 0));
            self.write_sector(dev, self.cluster_lba(c), &first)?;
            self.create_entry(dev, &mut st, parent, name, ATTR_DIR, c, 0)
        })();
        if made.is_err() {
            let _ = self.free_chain(dev, c);
        }
        made
    }
}

// -----| directory entry encoding |-----

/// Parse raw directory slots, attaching long names to their short entries.
fn parse_dir(raw: &[u8]) -> Vec<Item> {
    let mut items = Vec::new();
    let mut lfn: Vec<(u8, [u16; 13])> = Vec::new();
    let mut lfn_start = 0;
    let mut lfn_sum = 0u8;
    for (i, e) in raw.chunks_exact(ENTRY).enumerate() {
        match e[0] {
            0x00 => break,
            0xE5 => {
                lfn.clear();
                continue;
            }
            _ => {}
        }
        let attr = e[11];
        if attr == ATTR_LFN {
            if e[0] & 0x40 != 0 {
                lfn.clear();
                lfn_start = i;
                lfn_sum = e[13];
            }
            let mut chars = [0u16; 13];
            for (k, &o) in [1, 3, 5, 7, 9, 14, 16, 18, 20, 22, 24, 28, 30].iter().enumerate() {
                chars[k] = le16(e, o);
            }
            lfn.push((e[0] & 0x1F, chars));
            continue;
        }
        if attr & ATTR_VOLUME != 0 {
            lfn.clear();
            continue;
        }
        let mut short = [0u8; 11];
        short.copy_from_slice(&e[0..11]);
        if short[0] == 0x05 {
            short[0] = 0xE5; // 0x05 escapes a real 0xE5 first character
        }
        let on_disk: [u8; 11] = e[0..11].try_into().unwrap();
        let long = (!lfn.is_empty() && lfn_sum == lfn_checksum(&on_disk)).then(|| {
            lfn.sort_by_key(|(seq, _)| *seq);
            let units: Vec<u16> = lfn.iter().flat_map(|(_, c)| c.iter().copied()).take_while(|&u| u != 0).collect();
            String::from_utf16_lossy(&units)
        });
        let first_slot = if long.is_some() { lfn_start } else { i };
        items.push(Item {
            name: long.unwrap_or_else(|| short_to_string(&short, e[12])),
            short,
            attr,
            cluster: (le16(e, 26) as u32) | ((le16(e, 20) as u32) << 16),
            size: le32(e, 28),
            mtime: fat_to_filetime(le16(e, 24), le16(e, 22)),
            first_slot,
            slot: i,
        });
        lfn.clear();
    }
    items
}

fn short_to_string(short: &[u8; 11], nt_flags: u8) -> String {
    let part = |b: &[u8], lower: bool| -> String {
        b.iter()
            .take_while(|&&c| c != b' ')
            .map(|&c| if lower { (c as char).to_ascii_lowercase() } else { c as char })
            .collect()
    };
    let mut s = part(&short[0..8], nt_flags & NT_LOWER_BASE != 0);
    let ext = part(&short[8..11], nt_flags & NT_LOWER_EXT != 0);
    if !ext.is_empty() {
        s.push('.');
        s.push_str(&ext);
    }
    s
}

fn lfn_checksum(short: &[u8; 11]) -> u8 {
    short.iter().fold(0u8, |sum, &c| (sum >> 1).wrapping_add(sum << 7).wrapping_add(c))
}

fn valid_short_char(c: u8) -> bool {
    c.is_ascii_uppercase() || c.is_ascii_digit() || b"$%'-_@~`!(){}^#&".contains(&c)
}

/// The 8.3 name for `name`: (short name, NT case flags, whether LFN slots
/// are needed). Names that fit 8.3 exactly (all one case per part) need no
/// LFN; others get a unique `BASIS~N.EXT` alias.
fn make_short_name(name: &str, existing: &[Item]) -> Result<([u8; 11], u8, bool), VfsError> {
    if name.bytes().any(|c| c < 0x20 || b"\\/:*?\"<>|".contains(&c)) || name.ends_with('.') || name.ends_with(' ') {
        return Err(VfsError::GenericError);
    }

    let (base, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i + 1..]),
        _ => (name, ""),
    };
    let single_case = |s: &str| !(s.bytes().any(|c| c.is_ascii_lowercase()) && s.bytes().any(|c| c.is_ascii_uppercase()));
    let fits = |s: &str, max: usize| {
        !s.is_empty() && s.len() <= max && s.bytes().all(|c| valid_short_char(c.to_ascii_uppercase()))
    };
    if fits(base, 8) && (ext.is_empty() || fits(ext, 3)) && single_case(base) && single_case(ext) {
        let mut short = [b' '; 11];
        for (k, c) in base.bytes().enumerate() {
            short[k] = c.to_ascii_uppercase();
        }
        for (k, c) in ext.bytes().enumerate() {
            short[8 + k] = c.to_ascii_uppercase();
        }
        let mut flags = 0;
        if base.bytes().any(|c| c.is_ascii_lowercase()) { flags |= NT_LOWER_BASE; }
        if ext.bytes().any(|c| c.is_ascii_lowercase()) { flags |= NT_LOWER_EXT; }
        return Ok((short, flags, false));
    }

    // Long name: a numbered alias from the valid characters of base and ext.
    let clean = |s: &str, max: usize| -> Vec<u8> {
        s.bytes()
            .map(|c| c.to_ascii_uppercase())
            .filter(|&c| c != b' ' && c != b'.')
            .map(|c| if valid_short_char(c) { c } else { b'_' })
            .take(max)
            .collect()
    };
    let base_clean = clean(base, 8);
    let ext_clean = clean(ext, 3);
    for n in 1..1_000_000u32 {
        let tail = alloc::format!("~{}", n);
        let keep = 8usize.saturating_sub(tail.len()).min(base_clean.len());
        let mut short = [b' '; 11];
        short[..keep].copy_from_slice(&base_clean[..keep]);
        if keep == 0 {
            short[0] = b'_';
        }
        let start = keep.max(1);
        short[start..start + tail.len()].copy_from_slice(tail.as_bytes());
        short[8..8 + ext_clean.len()].copy_from_slice(&ext_clean);
        if !existing.iter().any(|it| it.short == short) {
            return Ok((short, 0, true));
        }
    }
    Err(VfsError::WriteError)
}

fn short_entry(short: &[u8; 11], nt_flags: u8, attr: u8, cluster: u32, size: u32) -> [u8; ENTRY] {
    let (date, time) = fat_now();
    let mut e = [0u8; ENTRY];
    e[0..11].copy_from_slice(short);
    e[11] = attr;
    e[12] = nt_flags;
    e[14..16].copy_from_slice(&time.to_le_bytes()); // created
    e[16..18].copy_from_slice(&date.to_le_bytes());
    e[18..20].copy_from_slice(&date.to_le_bytes()); // accessed
    e[20..22].copy_from_slice(&((cluster >> 16) as u16).to_le_bytes());
    e[22..24].copy_from_slice(&time.to_le_bytes()); // modified
    e[24..26].copy_from_slice(&date.to_le_bytes());
    e[26..28].copy_from_slice(&(cluster as u16).to_le_bytes());
    e[28..32].copy_from_slice(&size.to_le_bytes());
    e
}

/// LFN slot `seq` (1-based) holding characters `(seq-1)*13 ..` of `name`.
fn lfn_entry(name: &[u16], seq: usize, last: bool, checksum: u8) -> [u8; ENTRY] {
    let mut e = [0u8; ENTRY];
    e[0] = seq as u8 | if last { 0x40 } else { 0 };
    e[11] = ATTR_LFN;
    e[13] = checksum;
    for (k, &o) in [1, 3, 5, 7, 9, 14, 16, 18, 20, 22, 24, 28, 30].iter().enumerate() {
        let idx = (seq - 1) * 13 + k;
        let unit = match idx.cmp(&name.len()) {
            core::cmp::Ordering::Less => name[idx],
            core::cmp::Ordering::Equal => 0x0000, // terminator
            core::cmp::Ordering::Greater => 0xFFFF, // padding
        };
        e[o..o + 2].copy_from_slice(&unit.to_le_bytes());
    }
    e
}

/// Current (date, time) in FAT format, from the RTC.
fn fat_now() -> (u16, u16) {
    let t = crate::shell::wall_clock();
    let year = t.year.clamp(1980, 2107);
    let date = ((year - 1980) << 9) | ((t.month as u16) << 5) | t.day as u16;
    let time = ((t.hour as u16) << 11) | ((t.minute as u16) << 5) | (t.second as u16 / 2);
    (date, time)
}
