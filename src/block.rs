// =============================================================================
// VEILOS: block layer
// =============================================================================
//
// One `Disk` per AHCI port, shared by everything that uses that disk, with a
// lock around the port so two CPUs can never drive it at once. Each disk has
// a 64 KiB DMA bounce buffer (128 sectors per command; it was 8) and a
// sector cache, which is also the FAT cache: FAT and directory sectors are
// re-read constantly and come from memory. In Security Mode the cache is
// write-through and holds 1 MiB; in Performance Mode it is write-back (dirty
// sectors go to the disk within a second, sorted and merged into large
// commands, and on FlushFileBuffers, power-off and leaving the mode) and holds
// 64 MiB, bulk reads included.
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

/// Sectors kept in each disk's cache: 1 MiB in Security Mode, 64 MiB in
/// Performance Mode (P2).
const CACHE_SECTORS: usize = 2048;
const CACHE_SECTORS_PERF: usize = 131_072;
/// In Security Mode, reads larger than this bypass the cache (bulk file data
/// would only evict the metadata the cache is for). Performance Mode keeps
/// everything.
const CACHEABLE_READ_SECTORS: u32 = 64;
/// Performance Mode writes back what's dirty at least this often (ms).
const WRITE_BACK_MS: u64 = 1000;

fn performance() -> bool {
    crate::policy::POLICY.lock().get_mode() == crate::policy::PolicyMode::Performance
}

struct Entry {
    used: u64,
    /// Newer than the disk (Performance Mode's write-back).
    dirty: bool,
    data: Box<[u8; SECTOR]>,
}

struct SectorCache {
    map: BTreeMap<u64, Entry>,
    clock: u64,
    hits: u64,
    misses: u64,
    dirty: usize,
}

impl SectorCache {
    const fn new() -> Self {
        Self { map: BTreeMap::new(), clock: 0, hits: 0, misses: 0, dirty: 0 }
    }

    fn get(&mut self, lba: u64) -> Option<&[u8; SECTOR]> {
        self.clock += 1;
        let clock = self.clock;
        self.map.get_mut(&lba).map(|e| {
            e.used = clock;
            &*e.data
        })
    }

    /// Store a sector. `dirty`: it isn't on the disk yet. A clean copy never
    /// replaces a dirty one (`keep_dirty`), so a read can't undo a write.
    fn put(&mut self, lba: u64, data: &[u8], dirty: bool, keep_dirty: bool) {
        self.clock += 1;
        let clock = self.clock;
        match self.map.get_mut(&lba) {
            Some(e) => {
                e.used = clock;
                if e.dirty && keep_dirty && !dirty {
                    return;
                }
                e.data.copy_from_slice(data);
                if e.dirty != dirty {
                    if dirty { self.dirty += 1 } else { self.dirty -= 1 }
                    e.dirty = dirty;
                }
            }
            None => {
                let mut buf = Box::new([0u8; SECTOR]);
                buf.copy_from_slice(data);
                self.map.insert(lba, Entry { used: clock, dirty, data: buf });
                if dirty {
                    self.dirty += 1;
                }
            }
        }
    }

    fn forget(&mut self, lba: u64) {
        if let Some(e) = self.map.remove(&lba) {
            if e.dirty {
                self.dirty -= 1;
            }
        }
    }
}

struct DiskInner {
    port: *mut HbaPort,
    bounce_phys: u64,
    bounce_virt: *mut u8,
    cache: SectorCache,
}

impl DiskInner {
    fn write_disk(&mut self, lba: u64, src: &[u8]) -> bool {
        let count = (src.len() / SECTOR) as u32;
        let mut done = 0u32;
        while done < count {
            let chunk = (count - done).min(BOUNCE_SECTORS);
            let bytes = chunk as usize * SECTOR;
            let part = &src[done as usize * SECTOR..done as usize * SECTOR + bytes];
            unsafe { core::ptr::copy_nonoverlapping(part.as_ptr(), self.bounce_virt, bytes) };
            if !unsafe { &mut *self.port }.write_sectors(lba + done as u64, chunk, self.bounce_phys, Disk::phys_offset()) {
                return false;
            }
            done += chunk;
        }
        true
    }

    /// Write the dirty sectors among `lbas` (sorted), contiguous ones in one
    /// command each, and mark them clean.
    fn write_back(&mut self, lbas: &[u64]) -> bool {
        let mut ok = true;
        let mut i = 0;
        while i < lbas.len() {
            let start = lbas[i];
            let mut run: Vec<u8> = Vec::new();
            let mut n = 0u64;
            while i < lbas.len() && lbas[i] == start + n && n < BOUNCE_SECTORS as u64 {
                run.extend_from_slice(&*self.cache.map[&lbas[i]].data);
                n += 1;
                i += 1;
            }
            if self.write_disk(start, &run) {
                for k in 0..n {
                    if let Some(e) = self.cache.map.get_mut(&(start + k)) {
                        if e.dirty {
                            e.dirty = false;
                            self.cache.dirty -= 1;
                        }
                    }
                }
            } else {
                crate::serial_println!("[BLOCK] write-back of {} sector(s) at {} failed", n, start);
                ok = false;
            }
        }
        ok
    }

    /// Everything dirty to the disk.
    fn flush(&mut self) -> bool {
        if self.cache.dirty == 0 {
            return true;
        }
        let lbas: Vec<u64> = self.cache.map.iter().filter(|(_, e)| e.dirty).map(|(&l, _)| l).collect();
        self.write_back(&lbas)
    }

    /// Over the limit: drop the least recently used quarter, writing back
    /// what's dirty among it first.
    fn trim(&mut self, limit: usize) {
        if self.cache.map.len() <= limit {
            return;
        }
        let mut stamps: Vec<u64> = self.cache.map.values().map(|e| e.used).collect();
        stamps.sort_unstable();
        let cutoff = stamps[stamps.len() / 4];
        let victims: Vec<u64> = self.cache.map.iter().filter(|(_, e)| e.used <= cutoff).map(|(&l, _)| l).collect();
        let dirty: Vec<u64> = victims.iter().copied().filter(|l| self.cache.map[l].dirty).collect();
        if !dirty.is_empty() && !self.write_back(&dirty) {
            // Keep what couldn't be written rather than lose it.
            for l in victims {
                if !self.cache.map[&l].dirty {
                    self.cache.forget(l);
                }
            }
            return;
        }
        for l in victims {
            self.cache.forget(l);
        }
    }
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

/// Every disk, for write-back.
static DISKS: Mutex<Vec<Arc<Disk>>> = Mutex::new(Vec::new());
static LAST_FLUSH_MS: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

/// Write every disk's dirty sectors now (FlushFileBuffers, power-off,
/// leaving Performance Mode).
pub fn flush_all() {
    let disks: Vec<Arc<Disk>> = DISKS.lock().clone();
    for d in disks {
        d.inner.lock().flush();
    }
}

/// From the main loop: Performance Mode's lazy writer.
pub fn tick(now_ms: u64) {
    use core::sync::atomic::Ordering;
    let last = LAST_FLUSH_MS.load(Ordering::Relaxed);
    if now_ms.wrapping_sub(last) < WRITE_BACK_MS {
        return;
    }
    LAST_FLUSH_MS.store(now_ms, Ordering::Relaxed);
    let disks: Vec<Arc<Disk>> = match DISKS.try_lock() {
        Some(d) => d.clone(),
        None => return,
    };
    for d in disks {
        if let Some(mut inner) = d.inner.try_lock() {
            inner.flush();
        }
    }
}

impl Disk {
    pub fn new(port: &'static mut HbaPort, port_num: usize, sectors: u64) -> Option<Arc<Self>> {
        let (bounce_phys, bounce_virt) = crate::memory::DmaRegion::new(BOUNCE_BYTES)?.leak();
        let disk = Arc::new(Self {
            port_num,
            sectors,
            inner: Mutex::new(DiskInner {
                port,
                bounce_phys,
                bounce_virt,
                cache: SectorCache::new(),
            }),
        });
        DISKS.lock().push(disk.clone());
        Some(disk)
    }

    fn phys_offset() -> VirtAddr {
        crate::memory::get_phys_mem_offset()
    }

    /// Read `buf.len() / 512` sectors from absolute `lba`. Cached sectors
    /// (dirty ones in particular) win over what's on the disk.
    pub fn read(&self, lba: u64, buf: &mut [u8]) -> bool {
        let count = (buf.len() / SECTOR) as u32;
        let perf = performance();
        let cacheable = perf || count <= CACHEABLE_READ_SECTORS;
        let mut d = self.inner.lock();

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
            done += chunk;
        }
        // What the cache holds is at least as new as the disk.
        let end = lba + count as u64;
        let cached: Vec<u64> = d.cache.map.range(lba..end).map(|(&l, _)| l).collect();
        for l in &cached {
            let i = (l - lba) as usize;
            let s = d.cache.get(*l).unwrap();
            buf[i * SECTOR..(i + 1) * SECTOR].copy_from_slice(s);
        }
        if cacheable {
            for i in 0..count as usize {
                d.cache.put(lba + i as u64, &buf[i * SECTOR..(i + 1) * SECTOR], false, true);
            }
            d.trim(if perf { CACHE_SECTORS_PERF } else { CACHE_SECTORS });
        }
        true
    }

    /// Write `buf.len() / 512` sectors to absolute `lba`. Security Mode
    /// writes through; Performance Mode (P1) keeps them dirty in the cache
    /// for the lazy writer (`tick`), at most `WRITE_BACK_MS` behind.
    pub fn write(&self, lba: u64, buf: &[u8]) -> bool {
        let count = (buf.len() / SECTOR) as u32;
        let perf = performance();
        let mut d = self.inner.lock();
        if perf {
            for i in 0..count as usize {
                d.cache.put(lba + i as u64, &buf[i * SECTOR..(i + 1) * SECTOR], true, false);
            }
            d.trim(CACHE_SECTORS_PERF);
            return true;
        }
        if !d.write_disk(lba, buf) {
            // Unknown what reached the disk: forget the range.
            for i in 0..count as u64 {
                d.cache.forget(lba + i);
            }
            return false;
        }
        for i in 0..count as usize {
            d.cache.put(lba + i as u64, &buf[i * SECTOR..(i + 1) * SECTOR], false, false);
        }
        d.trim(CACHE_SECTORS);
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
    if let Some((fs, name)) = crate::fat::detect(device) { return Some((fs, name)); }
    None
}

/// Read sector 0 of `device` (a filesystem's boot sector).
pub fn read_boot_sector(device: &dyn BlockDevice) -> Option<[u8; SECTOR]> {
    let mut buf = [0u8; SECTOR];
    device.read_sectors(0, 1, buf.as_mut_ptr() as u64).then_some(buf)
}
