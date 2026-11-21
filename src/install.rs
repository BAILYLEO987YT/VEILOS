// =============================================================================
// VEILOS: installing to an internal disk
// =============================================================================
//
// `INSTALL <disk> ERASE` (terminal, administrators) puts the running system
// on another disk, erasing it, so the PC starts without the USB stick:
//
//   1. The boot partitions of the disk `/` is on (the EFI System Partition
//      the bootloader and kernel live on: every GPT partition but the VEILOS
//      volume) are copied sector for sector to the same places.
//   2. A new FAT32 VEILOS volume is made from where the old one starts to
//      the end of the disk (at most 2 TiB, FAT32's limit), and every file of
//      `/` is copied onto it: users, settings, Wi-Fi networks, programs.
//   3. Last, a new GPT (fresh disk and partition GUIDs, the volume's entry
//      grown to fill the disk, the backup table at the end) and protective
//      MBR. Until then the disk still has its old partition table, so an
//      install that stops halfway leaves nothing that looks bootable.
//
// Only a GPT boot disk (the UEFI image) can be copied. The block layer lets
// a disk be written only inside ranges opened for it; the installer opens
// the whole target disk for the length of the install and closes it after,
// and unmounts what was mounted from it first (its read-only mounts would
// otherwise read the new data as the old filesystem).
//
// It runs on a kernel thread of its own, yielding as it goes, so the desktop
// keeps working; `INSTALL STATUS` shows how far it is.

use alloc::{format, string::String, sync::Arc, vec, vec::Vec};
use core::sync::atomic::{AtomicBool, Ordering};
use spin::Mutex;

use crate::block::{Disk, Partition};
use crate::vfs::{BlockDevice, FileSystem, FileType};

const SECTOR: usize = 512;
/// Copy and zero in pieces this big.
const CHUNK: usize = 1 << 20;
/// The smallest VEILOS volume worth installing.
const MIN_VOLUME: u64 = 2 << 30;

static RUNNING: AtomicBool = AtomicBool::new(false);
static STATE: Mutex<String> = Mutex::new(String::new());

fn status(s: String) {
    crate::serial_println!("[INSTALL] {}", s);
    *STATE.lock() = s;
}

/// The last thing the installer said ("" before any install).
pub fn state() -> String {
    STATE.lock().clone()
}

/// Disks the system could be installed on, one line each: every disk but
/// the one `/` is on (and the RAM disk), with what is on it now.
pub fn targets() -> Vec<String> {
    let root = crate::block::root_location().map(|(d, _, _)| d.name.clone());
    let found = crate::block::found_volumes();
    let mut out = Vec::new();
    for d in crate::block::disks() {
        if Some(&d.name) == root.as_ref() || d.name == "ram" {
            continue;
        }
        let gib = d.sectors * SECTOR as u64 / (1 << 30);
        out.push(format!("  {}: {} GiB", d.name.to_ascii_uppercase(), gib));
        let tag = format!("disk {} ", d.name);
        let mut any = false;
        for l in found.iter().filter(|l| l.contains(&tag)) {
            out.push(format!("      {}", l.split(" at ").next().unwrap_or(l)));
            any = true;
        }
        if !any {
            for p in crate::block::read_partitions(&d) {
                out.push(format!("      {} ({} MiB)", p.kind, p.sectors / 2048));
            }
        }
    }
    out
}

/// Start installing onto disk `target` (a name from `targets`), erasing it.
/// Checks what it can first; the copy itself runs on its own thread.
pub fn start(target: &str) -> Result<(), String> {
    let (src, root_start, root_sectors) =
        crate::block::root_location().ok_or_else(|| String::from("no VEILOS volume at /"))?;
    if src.name == "ram" {
        return Err(String::from("/ is on the RAM disk: start from a disk image (veilos-uefi-disk.img) instead"));
    }
    let dst = crate::block::disks()
        .into_iter()
        .find(|d| d.name.eq_ignore_ascii_case(target))
        .ok_or_else(|| format!("no disk {}", target))?;
    if Arc::ptr_eq(&src, &dst) {
        return Err(String::from("that is the disk VEILOS is running from"));
    }
    let plan = Plan::new(&src, root_start, root_sectors, &dst)?;
    if RUNNING.swap(true, Ordering::AcqRel) {
        return Err(String::from("an install is already running"));
    }
    status(format!("starting: {} onto {}", src.name, dst.name));
    crate::kthread::spawn("install", move || {
        let result = plan.run(&src, &dst);
        dst.set_install_window(false);
        match result {
            Ok(msg) => status(msg),
            Err(e) => status(format!("FAILED: {}", e)),
        }
        RUNNING.store(false, Ordering::Release);
    });
    Ok(())
}

/// What goes where, worked out (and checked) before anything is written.
struct Plan {
    /// The source GPT header (sector 1) and its entry array.
    header: [u8; SECTOR],
    entries: Vec<u8>,
    entry_size: usize,
    count: usize,
    /// Entry index of the VEILOS volume.
    volume_entry: usize,
    /// (first, last) LBAs of each boot partition, copied as they are.
    boot: Vec<(u64, u64)>,
    /// The new volume: where it starts and how many sectors.
    vol_start: u64,
    vol_sectors: u64,
    /// The target's last usable LBA, and its entry array's sectors.
    last_usable: u64,
    entry_sectors: u64,
}

fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}
fn u64_at(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().unwrap())
}

impl Plan {
    fn new(src: &Disk, root_start: u64, _root_sectors: u64, dst: &Disk) -> Result<Self, String> {
        let mut header = [0u8; SECTOR];
        if !src.read(1, &mut header) || &header[0..8] != b"EFI PART" {
            return Err(String::from("the disk VEILOS runs from has no GPT (only the UEFI image can be installed)"));
        }
        let entries_lba = u64_at(&header, 72);
        let count = u32_at(&header, 80) as usize;
        let entry_size = u32_at(&header, 84) as usize;
        if !(128..=SECTOR).contains(&entry_size) || SECTOR % entry_size != 0 || count == 0 || count > 1024 {
            return Err(String::from("its GPT has an entry layout this can't copy"));
        }
        let entry_sectors = ((count * entry_size + SECTOR - 1) / SECTOR) as u64;
        let mut entries = vec![0u8; entry_sectors as usize * SECTOR];
        if !src.read(entries_lba, &mut entries) {
            return Err(String::from("couldn't read its GPT entries"));
        }

        let mut volume_entry = None;
        let mut boot = Vec::new();
        for i in 0..count {
            let e = &entries[i * entry_size..(i + 1) * entry_size];
            if e[0..16].iter().all(|&b| b == 0) {
                continue;
            }
            let (first, last) = (u64_at(e, 32), u64_at(e, 40));
            if first == root_start {
                volume_entry = Some(i);
            } else if last >= first {
                boot.push((first, last));
            }
        }
        let volume_entry = volume_entry.ok_or_else(|| String::from("the VEILOS volume isn't in its GPT"))?;
        if boot.is_empty() {
            return Err(String::from("it has no boot partition to copy"));
        }
        if boot.iter().any(|&(_, last)| last >= root_start) {
            return Err(String::from("a boot partition lies after the VEILOS volume"));
        }

        // The backup entry array and header take the last entry_sectors + 1
        // sectors of the target.
        let total = dst.sectors;
        let last_usable = total
            .checked_sub(2 + entry_sectors)
            .ok_or_else(|| String::from("the target disk is too small"))?;
        let max_fat32 = (1u64 << 32) - 1;
        let vol_sectors = (last_usable + 1).saturating_sub(root_start).min(max_fat32) & !7;
        if vol_sectors * (SECTOR as u64) < MIN_VOLUME {
            return Err(format!("{} is too small (a VEILOS volume needs 2 GiB)", dst.name));
        }
        Ok(Self {
            header,
            entries,
            entry_size,
            count,
            volume_entry,
            boot,
            vol_start: root_start,
            vol_sectors,
            last_usable,
            entry_sectors,
        })
    }

    fn run(&self, src: &Arc<Disk>, dst: &Arc<Disk>) -> Result<String, String> {
        let gone = crate::vfs::VFS.unmount_disk(&dst.name);
        if gone > 0 {
            status(format!("unmounted {} filesystem(s) of {}", gone, dst.name));
        }
        dst.set_install_window(true);

        // 1. Boot partitions, as they are.
        let mut buf = vec![0u8; CHUNK];
        for &(first, last) in &self.boot {
            let mut lba = first;
            while lba <= last {
                let n = ((last + 1 - lba) as usize).min(CHUNK / SECTOR);
                let b = &mut buf[..n * SECTOR];
                if !src.read(lba, b) {
                    return Err(format!("reading {} sector {}", src.name, lba));
                }
                if !dst.write_through(lba, b) {
                    return Err(format!("writing {} sector {}", dst.name, lba));
                }
                lba += n as u64;
                crate::kthread::yield_if_slice_used(10);
            }
            status(format!("copied boot partition ({} MiB)", (last + 1 - first) / 2048));
        }

        // 2. A new FAT32 volume, then every file of `/` onto it.
        format_fat32(dst, self.vol_start, self.vol_sectors)?;
        status(format!("made a {} GiB FAT32 volume", self.vol_sectors / 2048 / 1024));
        let volume = Partition { disk: dst.clone(), start: self.vol_start, sectors: self.vol_sectors };
        let (fs, _) = crate::fat::detect(&volume).ok_or_else(|| String::from("the new volume doesn't read back as FAT32"))?;
        let root = crate::vfs::VFS.root_mount().ok_or_else(|| String::from("nothing mounted at /"))?;
        let mut copied = (0u64, 0u64);
        copy_tree(&*root.fs, &*root.device, &*fs, &volume, "/", &mut copied)?;
        crate::block::flush_all();
        status(format!("copied {} files ({} MiB)", copied.0, copied.1 >> 20));

        // 3. The partition table, last.
        self.write_gpt(dst)?;
        crate::block::flush_all();
        Ok(format!(
            "DONE: VEILOS is on {} ({} GiB volume, {} files). Remove the USB stick and restart; \
             if the PC doesn't start VEILOS, pick the SSD (UEFI) in its boot menu.",
            dst.name.to_ascii_uppercase(),
            self.vol_sectors / 2048 / 1024,
            copied.0
        ))
    }

    fn write_gpt(&self, dst: &Disk) -> Result<(), String> {
        let total = dst.sectors;
        let mut entries = self.entries.clone();
        for i in 0..self.count {
            let e = &mut entries[i * self.entry_size..(i + 1) * self.entry_size];
            if e[0..16].iter().all(|&b| b == 0) {
                continue;
            }
            e[16..32].copy_from_slice(&new_guid());
            if i == self.volume_entry {
                e[40..48].copy_from_slice(&(self.vol_start + self.vol_sectors - 1).to_le_bytes());
            }
        }
        let used = self.count * self.entry_size;
        let entries_crc = crc32(&entries[..used]);
        let backup_entries = total - 1 - self.entry_sectors;

        let header_size = (u32_at(&self.header, 12) as usize).clamp(92, SECTOR);
        let make = |my: u64, other: u64, entries_lba: u64, disk_guid: &[u8; 16]| {
            let mut h = self.header;
            h[24..32].copy_from_slice(&my.to_le_bytes());
            h[32..40].copy_from_slice(&other.to_le_bytes());
            h[48..56].copy_from_slice(&self.last_usable.to_le_bytes());
            h[56..72].copy_from_slice(disk_guid);
            h[72..80].copy_from_slice(&entries_lba.to_le_bytes());
            h[88..92].copy_from_slice(&entries_crc.to_le_bytes());
            h[16..20].fill(0);
            let crc = crc32(&h[..header_size]);
            h[16..20].copy_from_slice(&crc.to_le_bytes());
            h
        };
        let guid = new_guid();
        let primary = make(1, total - 1, 2, &guid);
        let backup = make(total - 1, 1, backup_entries, &guid);

        // Protective MBR: one 0xEE entry over the whole disk.
        let mut mbr = [0u8; SECTOR];
        if !dst.read(0, &mut mbr) {
            return Err(String::from("reading the target's sector 0"));
        }
        mbr[446..510].fill(0);
        let e = &mut mbr[446..462];
        e[1..4].copy_from_slice(&[0x00, 0x02, 0x00]);
        e[4] = 0xEE;
        e[5..8].copy_from_slice(&[0xFF, 0xFF, 0xFF]);
        e[8..12].copy_from_slice(&1u32.to_le_bytes());
        e[12..16].copy_from_slice(&((total - 1).min(0xFFFF_FFFF) as u32).to_le_bytes());
        mbr[510] = 0x55;
        mbr[511] = 0xAA;

        let ok = dst.write_through(backup_entries, &entries)
            && dst.write_through(total - 1, &backup)
            && dst.write_through(2, &entries)
            && dst.write_through(1, &primary)
            && dst.write_through(0, &mbr);
        if !ok {
            return Err(String::from("writing the partition table"));
        }
        status(String::from("wrote the partition table"));
        Ok(())
    }
}

/// Copy directory `path` of `from` (and everything in it) onto `to`.
fn copy_tree(
    from: &dyn FileSystem,
    from_dev: &dyn BlockDevice,
    to: &dyn FileSystem,
    to_dev: &dyn BlockDevice,
    path: &str,
    copied: &mut (u64, u64),
) -> Result<(), String> {
    let list = from.list_dir(from_dev, path).map_err(|e| format!("listing {}: {:?}", path, e))?;
    for entry in list {
        if entry.name == "." || entry.name == ".." {
            continue;
        }
        let child = if path == "/" { format!("/{}", entry.name) } else { format!("{}/{}", path, entry.name) };
        match entry.file_type {
            FileType::Directory => {
                to.create_dir(to_dev, &child).map_err(|e| format!("making {}: {:?}", child, e))?;
                copy_tree(from, from_dev, to, to_dev, &child, copied)?;
            }
            FileType::File => {
                let data = from.read_file(from_dev, &child).map_err(|e| format!("reading {}: {:?}", child, e))?;
                to.write_file(to_dev, &child, &data).map_err(|e| format!("writing {}: {:?}", child, e))?;
                copied.0 += 1;
                copied.1 += data.len() as u64;
                if copied.0 % 50 == 0 {
                    status(format!("copying files: {} ({} MiB)", copied.0, copied.1 >> 20));
                }
            }
        }
        crate::kthread::yield_if_slice_used(10);
    }
    Ok(())
}

/// An empty FAT32 filesystem on `sectors` sectors at `start` of `disk`, laid
/// out as tools/mkdisk.py does (and Windows, for the cluster sizes).
fn format_fat32(disk: &Disk, start: u64, sectors: u64) -> Result<(), String> {
    const RESERVED: u64 = 32;
    const FATS: u64 = 2;
    let bytes = sectors * SECTOR as u64;
    let spc: u64 = if bytes <= 8 << 30 {
        8
    } else if bytes <= 16 << 30 {
        16
    } else if bytes <= 32 << 30 {
        32
    } else {
        64
    };
    let clusters_guess = sectors / spc;
    let spf = (clusters_guess + 2) * 4 / SECTOR as u64 + 1;
    let data_start = RESERVED + FATS * spf;
    let clusters = (sectors - data_start) / spc;
    if clusters < 65525 {
        return Err(String::from("volume too small for FAT32"));
    }

    // Zero the reserved sectors, both FATs and the root directory cluster.
    let zeros = vec![0u8; CHUNK];
    let mut lba = 0;
    let end = data_start + spc;
    while lba < end {
        let n = (end - lba).min((CHUNK / SECTOR) as u64);
        if !disk.write_through(start + lba, &zeros[..n as usize * SECTOR]) {
            return Err(format!("zeroing the new volume at sector {}", start + lba));
        }
        lba += n;
        crate::kthread::yield_if_slice_used(10);
    }

    // The first FAT sector of each FAT: media, end-of-chain, and the root
    // directory (cluster 2) as a one-cluster chain.
    let mut fat0 = [0u8; SECTOR];
    fat0[0..4].copy_from_slice(&0x0FFF_FFF8u32.to_le_bytes());
    fat0[4..8].copy_from_slice(&0x0FFF_FFFFu32.to_le_bytes());
    fat0[8..12].copy_from_slice(&0x0FFF_FFFFu32.to_le_bytes());

    let mut boot = [0u8; SECTOR];
    boot[0..3].copy_from_slice(&[0xEB, 0x58, 0x90]);
    boot[3..11].copy_from_slice(b"VEILOS  ");
    boot[11..13].copy_from_slice(&(SECTOR as u16).to_le_bytes());
    boot[13] = spc as u8;
    boot[14..16].copy_from_slice(&(RESERVED as u16).to_le_bytes());
    boot[16] = FATS as u8;
    boot[21] = 0xF8;
    boot[24..26].copy_from_slice(&63u16.to_le_bytes());
    boot[26..28].copy_from_slice(&255u16.to_le_bytes());
    boot[28..32].copy_from_slice(&(start.min(0xFFFF_FFFF) as u32).to_le_bytes());
    boot[32..36].copy_from_slice(&(sectors as u32).to_le_bytes());
    boot[36..40].copy_from_slice(&(spf as u32).to_le_bytes());
    boot[44..48].copy_from_slice(&2u32.to_le_bytes()); // root cluster
    boot[48..50].copy_from_slice(&1u16.to_le_bytes()); // FSInfo
    boot[50..52].copy_from_slice(&6u16.to_le_bytes()); // backup boot sector
    boot[64] = 0x80;
    boot[66] = 0x29;
    let mut serial = [0u8; 4];
    crate::entropy::fill(&mut serial);
    boot[67..71].copy_from_slice(&serial);
    boot[71..82].copy_from_slice(b"VEILOS     ");
    boot[82..90].copy_from_slice(b"FAT32   ");
    boot[510] = 0x55;
    boot[511] = 0xAA;

    let mut info = [0u8; SECTOR];
    info[0..4].copy_from_slice(&0x4161_5252u32.to_le_bytes());
    info[484..488].copy_from_slice(&0x6141_7272u32.to_le_bytes());
    info[488..492].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes()); // free count unknown
    info[492..496].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
    info[508..512].copy_from_slice(&0xAA55_0000u32.to_le_bytes());

    let ok = (0..FATS).all(|k| disk.write_through(start + RESERVED + k * spf, &fat0))
        && [0u64, 6].iter().all(|&at| disk.write_through(start + at, &boot) && disk.write_through(start + at + 1, &info));
    if !ok {
        return Err(String::from("writing the new volume's boot sector"));
    }
    Ok(())
}

/// A random (version 4) GUID, in the byte order GPT stores.
fn new_guid() -> [u8; 16] {
    let mut g = [0u8; 16];
    crate::entropy::fill(&mut g);
    g[7] = (g[7] & 0x0F) | 0x40;
    g[8] = (g[8] & 0x3F) | 0x80;
    g
}

/// CRC-32 (IEEE, reflected), as GPT uses.
fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}
