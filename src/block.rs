// =============================================================================
// VEILOS: block layer
// =============================================================================
//
// One `Disk` per AHCI port, shared by everything that uses that disk, with a
// lock around the port so two CPUs can never drive it at once. Each disk has
// a 64 KiB DMA bounce buffer (128 sectors per command; it was 8) and a
// write-through sector cache, which is also the FAT cache: FAT and directory
// sectors are re-read constantly and now come from memory.
//
// On top of a disk sit `Partition`s (MBR primary entries, GPT entries, or
// the whole disk for a superfloppy). Each is a `vfs::BlockDevice` with
// partition-relative LBAs and bounds checking. `mount_disk` probes every
// partition for a filesystem and mounts what it finds: the first one at `/`,
// the rest at `/mnt/d<port>p<n>`.
//
// Buffers passed to `BlockDevice` are kernel virtual addresses (heap,
// stack); the bounce buffer handles the DMA side.

use alloc::{boxed::Box, collections::BTreeMap, format, string::String, sync::Arc, vec, vec::Vec};
use spin::Mutex;
use x86_64::VirtAddr;

use crate::ahci::HbaPort;
use crate::vfs::BlockDevice;

pub const SECTOR: usize = 512;

/// Bounce buffer size: sectors moved per AHCI command.
const BOUNCE_BYTES: usize = 64 * 1024;
const BOUNCE_SECTORS: u32 = (BOUNCE_BYTES / SECTOR) as u32;

/// Sectors kept in each disk's cache (1 MiB).
const CACHE_SECTORS: usize = 2048;
/// Reads larger than this bypass the cache (bulk file data would only evict
/// the metadata the cache is for).
const CACHEABLE_READ_SECTORS: u32 = 64;

struct SectorCache {
    map: BTreeMap<u64, (u64, Box<[u8; SECTOR]>)>, // lba -> (last use, data)
    clock: u64,
    hits: u64,
    misses: u64,
}

impl SectorCache {
    const fn new() -> Self {
        Self { map: BTreeMap::new(), clock: 0, hits: 0, misses: 0 }
    }

    fn get(&mut self, lba: u64) -> Option<&[u8; SECTOR]> {
        self.clock += 1;
        let clock = self.clock;
        self.map.get_mut(&lba).map(|(used, data)| {
            *used = clock;
            &**data
        })
    }

    fn put(&mut self, lba: u64, data: &[u8]) {
        self.clock += 1;
        let clock = self.clock;
        match self.map.get_mut(&lba) {
            Some((used, buf)) => {
                *used = clock;
                buf.copy_from_slice(data);
            }
            None => {
                let mut buf = Box::new([0u8; SECTOR]);
                buf.copy_from_slice(data);
                self.map.insert(lba, (clock, buf));
                if self.map.len() > CACHE_SECTORS {
                    self.evict();
                }
            }
        }
    }

    /// Drop the least recently used quarter in one pass.
    fn evict(&mut self) {
        let mut stamps: Vec<u64> = self.map.values().map(|(used, _)| *used).collect();
        stamps.sort_unstable();
        let cutoff = stamps[stamps.len() / 4];
        self.map.retain(|_, (used, _)| *used > cutoff);
    }
}

struct DiskInner {
    port: *mut HbaPort,
    bounce_phys: u64,
    bounce_virt: *mut u8,
    cache: SectorCache,
}

/// An AHCI disk. Clone the `Arc` to share it.
pub struct Disk {
    pub port_num: usize,
    pub sectors: u64,
    inner: Mutex<DiskInner>,
}

// The raw port pointer is only used under `inner`'s lock.
unsafe impl Send for Disk {}
unsafe impl Sync for Disk {}

impl Disk {
    pub fn new(port: &'static mut HbaPort, port_num: usize, sectors: u64) -> Option<Arc<Self>> {
        let (bounce_phys, bounce_virt) = crate::memory::DmaRegion::new(BOUNCE_BYTES)?.leak();
        Some(Arc::new(Self {
            port_num,
            sectors,
            inner: Mutex::new(DiskInner {
                port,
                bounce_phys,
                bounce_virt,
                cache: SectorCache::new(),
            }),
        }))
    }

    fn phys_offset() -> VirtAddr {
        crate::memory::get_phys_mem_offset()
    }

    /// Read `buf.len() / 512` sectors from absolute `lba`.
    pub fn read(&self, lba: u64, buf: &mut [u8]) -> bool {
        let count = (buf.len() / SECTOR) as u32;
        let mut d = self.inner.lock();

        if count <= CACHEABLE_READ_SECTORS {
            let all_cached = (0..count as u64).all(|i| d.cache.map.contains_key(&(lba + i)));
            if all_cached {
                for i in 0..count as usize {
                    let s = d.cache.get(lba + i as u64).unwrap();
                    buf[i * SECTOR..(i + 1) * SECTOR].copy_from_slice(s);
                }
                d.cache.hits += 1;
                return true;
            }
            d.cache.misses += 1;
        }

        let mut done = 0u32;
        while done < count {
            let chunk = (count - done).min(BOUNCE_SECTORS);
            let bytes = chunk as usize * SECTOR;
            let (port, phys, virt) = (d.port, d.bounce_phys, d.bounce_virt);
            if !unsafe { &mut *port }.read_sectors(lba + done as u64, chunk, phys, Self::phys_offset()) {
                return false;
            }
            let dst = &mut buf[done as usize * SECTOR..done as usize * SECTOR + bytes];
            unsafe { core::ptr::copy_nonoverlapping(virt, dst.as_mut_ptr(), bytes) };
            if count <= CACHEABLE_READ_SECTORS {
                for i in 0..chunk as usize {
                    d.cache.put(lba + done as u64 + i as u64, &dst[i * SECTOR..(i + 1) * SECTOR]);
                }
            }
            done += chunk;
        }
        true
    }

    /// Write `buf.len() / 512` sectors to absolute `lba` (write-through:
    /// cached copies are updated, never left stale).
    pub fn write(&self, lba: u64, buf: &[u8]) -> bool {
        let count = (buf.len() / SECTOR) as u32;
        let mut d = self.inner.lock();
        let mut done = 0u32;
        while done < count {
            let chunk = (count - done).min(BOUNCE_SECTORS);
            let bytes = chunk as usize * SECTOR;
            let src = &buf[done as usize * SECTOR..done as usize * SECTOR + bytes];
            let (port, phys, virt) = (d.port, d.bounce_phys, d.bounce_virt);
            unsafe { core::ptr::copy_nonoverlapping(src.as_ptr(), virt, bytes) };
            if !unsafe { &mut *port }.write_sectors(lba + done as u64, chunk, phys, Self::phys_offset()) {
                // Unknown what reached the disk: forget the range.
                for i in 0..count as u64 {
                    d.cache.map.remove(&(lba + i));
                }
                return false;
            }
            for i in 0..chunk as usize {
                d.cache.put(lba + done as u64 + i as u64, &src[i * SECTOR..(i + 1) * SECTOR]);
            }
            done += chunk;
        }
        true
    }

    /// (hits, misses) of the sector cache, for diagnostics.
    pub fn cache_stats(&self) -> (u64, u64) {
        let d = self.inner.lock();
        (d.cache.hits, d.cache.misses)
    }
}

/// A partition (or a whole disk) as a block device. LBAs are relative to the
/// partition start and bounds-checked.
pub struct Partition {
    pub disk: Arc<Disk>,
    pub start: u64,
    pub sectors: u64,
}

impl Partition {
    fn in_bounds(&self, lba: u64, count: u32) -> bool {
        lba.checked_add(count as u64).is_some_and(|end| end <= self.sectors)
    }
}

impl BlockDevice for Partition {
    fn read_sectors(&self, lba: u64, count: u32, buffer_virt: u64) -> bool {
        if !self.in_bounds(lba, count) {
            crate::serial_println!("[BLOCK] read past partition end: lba {} + {} > {}", lba, count, self.sectors);
            return false;
        }
        let buf = unsafe { core::slice::from_raw_parts_mut(buffer_virt as *mut u8, count as usize * SECTOR) };
        self.disk.read(self.start + lba, buf)
    }

    fn write_sectors(&self, lba: u64, count: u32, buffer_virt: u64) -> bool {
        if !self.in_bounds(lba, count) {
            crate::serial_println!("[BLOCK] write past partition end: lba {} + {} > {}", lba, count, self.sectors);
            return false;
        }
        let buf = unsafe { core::slice::from_raw_parts(buffer_virt as *const u8, count as usize * SECTOR) };
        self.disk.write(self.start + lba, buf)
    }
}

// -----| partition tables |-----

#[derive(Clone, Debug)]
pub struct PartitionInfo {
    pub start: u64,
    pub sectors: u64,
    pub kind: String,
}

/// Read the partition table: GPT (behind a protective MBR), MBR primary
/// entries, or — if sector 0 holds no valid table — the whole disk.
pub fn read_partitions(disk: &Disk) -> Vec<PartitionInfo> {
    let whole = || vec![PartitionInfo { start: 0, sectors: disk.sectors, kind: String::from("whole disk") }];
    let mut mbr = [0u8; SECTOR];
    if !disk.read(0, &mut mbr) {
        return Vec::new();
    }
    if u16::from_le_bytes([mbr[510], mbr[511]]) != 0xAA55 {
        return whole();
    }

    let mut parts = Vec::new();
    for i in 0..4 {
        let e = &mbr[446 + i * 16..446 + (i + 1) * 16];
        let boot = e[0];
        let kind = e[4];
        let start = u32::from_le_bytes([e[8], e[9], e[10], e[11]]) as u64;
        let sectors = u32::from_le_bytes([e[12], e[13], e[14], e[15]]) as u64;
        if kind == 0 {
            continue;
        }
        // A FAT boot sector has code here, not a table: reject anything that
        // doesn't look like a real entry and treat the disk as unpartitioned.
        if (boot != 0x00 && boot != 0x80) || start == 0 || sectors == 0 || start + sectors > disk.sectors {
            return whole();
        }
        match kind {
            0xEE => return read_gpt(disk).unwrap_or_else(whole),
            0x05 | 0x0F | 0x85 => {
                crate::serial_println!("[BLOCK] disk {}: extended partition {} not read yet", disk.port_num, i + 1);
            }
            _ => parts.push(PartitionInfo { start, sectors, kind: format!("MBR #{} type {:#04x}", i + 1, kind) }),
        }
    }
    if parts.is_empty() { whole() } else { parts }
}

fn read_gpt(disk: &Disk) -> Option<Vec<PartitionInfo>> {
    let mut hdr = [0u8; SECTOR];
    if !disk.read(1, &mut hdr) || &hdr[0..8] != b"EFI PART" {
        crate::serial_println!("[BLOCK] disk {}: protective MBR but no GPT header", disk.port_num);
        return None;
    }
    let u32_at = |b: &[u8], o: usize| u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
    let u64_at = |b: &[u8], o: usize| u64::from_le_bytes(b[o..o + 8].try_into().unwrap());
    let entries_lba = u64_at(&hdr, 72);
    let count = u32_at(&hdr, 80).min(128) as usize;
    let size = u32_at(&hdr, 84) as usize;
    if size < 128 || size > SECTOR || SECTOR % size != 0 {
        return None;
    }

    let total = count * size;
    let mut table = vec![0u8; (total + SECTOR - 1) / SECTOR * SECTOR];
    if !disk.read(entries_lba, &mut table) {
        return None;
    }
    let mut parts = Vec::new();
    for i in 0..count {
        let e = &table[i * size..(i + 1) * size];
        if e[0..16].iter().all(|&b| b == 0) {
            continue; // unused entry
        }
        let first = u64_at(e, 32);
        let last = u64_at(e, 40);
        if last < first || last >= disk.sectors {
            continue;
        }
        let name: String = e[56..128]
            .chunks(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .take_while(|&c| c != 0)
            .map(|c| char::from_u32(c as u32).unwrap_or('?'))
            .collect();
        parts.push(PartitionInfo { start: first, sectors: last - first + 1, kind: format!("GPT #{} '{}'", i + 1, name) });
    }
    Some(parts)
}

// -----| mounting |-----

/// Probe every partition of `disk` and mount each filesystem found.
pub fn mount_disk(disk: Arc<Disk>) {
    let parts = read_partitions(&disk);
    crate::println!("     - {} partition(s)", parts.len());
    for (n, p) in parts.iter().enumerate() {
        crate::serial_println!(
            "[BLOCK] disk {} part {}: {} start {} ({} MiB)",
            disk.port_num, n, p.kind, p.start, p.sectors / 2048
        );
        let device = Partition { disk: disk.clone(), start: p.start, sectors: p.sectors };
        let Some((fs, name)) = detect_filesystem(&device) else {
            crate::serial_println!("[BLOCK]   no filesystem recognised");
            continue;
        };
        let path = if crate::vfs::VFS.mount_count() == 0 {
            String::from("/")
        } else {
            format!("/mnt/d{}p{}", disk.port_num, n)
        };
        crate::println!("  -> Mounted {} (disk {} partition {}) at {}", name, disk.port_num, n, path);
        crate::vfs::VFS.mount(path, fs, Box::new(device));
    }
}

/// Try each filesystem driver on `device`.
fn detect_filesystem(device: &Partition) -> Option<(Box<dyn crate::vfs::FileSystem>, &'static str)> {
    if let Some(fs) = crate::ntfs::detect(device) { return Some((fs, "NTFS")); }
    if let Some(fs) = crate::exfat::detect(device) { return Some((fs, "exFAT")); }
    if let Some(fs) = crate::fat32::detect(device) { return Some((fs, "FAT32")); }
    if let Some((fs, name)) = crate::fat16::detect(device) { return Some((fs, name)); }
    None
}

/// Read sector 0 of `device` (a filesystem's boot sector).
pub fn read_boot_sector(device: &dyn BlockDevice) -> Option<[u8; SECTOR]> {
    let mut buf = [0u8; SECTOR];
    device.read_sectors(0, 1, buf.as_mut_ptr() as u64).then_some(buf)
}
