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
// `UPDATE <disk>` (administrators) upgrades a VEILOS already installed on
// another disk from the running one, keeping its users and data:
//
//   1. Its boot partitions are rewritten sector for sector from the running
//      disk's (the new bootloader and kernel). Both disks must have the same
//      boot partitions, at the same places with the same types (they do when
//      both came from the same kind of image or one was INSTALLed from the
//      other); otherwise it refuses and INSTALL is the way.
//   2. Every file of `/` is copied onto its VEILOS volume, overwriting what
//      is there, except what belongs to the people using it: `/etc` (users,
//      the last user, settings, the registry, Wi-Fi networks and keys),
//      `/home`, `/var`, `/tmp`, `/net` and `/config.vsh` only get what is
//      missing there. `/system` and `/tests` are replaced: files the new
//      build doesn't have are removed.
//
// Its partition table and volume are left as they are. From a RAM-disk boot
// it refuses: that image's boot partition carries the RAM disk, and copying
// it would make the updated disk start from memory too.
//
// Both run on a kernel thread of their own, yielding as they go, so the
// desktop keeps working; `INSTALL STATUS` (or `UPDATE STATUS`) shows how far
// one is.

use alloc::{format, string::String, sync::Arc, vec, vec::Vec};
use core::sync::atomic::{AtomicBool, Ordering};
use spin::Mutex;

use crate::fs::block::{Disk, Partition};
use crate::fs::vfs::{BlockDevice, FileSystem, FileType};

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
    let root = crate::fs::block::root_location().map(|(d, _, _)| d.name.clone());
    let found = crate::fs::block::found_volumes();
    let mut out = Vec::new();
    for d in crate::fs::block::disks() {
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
            for p in crate::fs::block::read_partitions(&d) {
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
        crate::fs::block::root_location().ok_or_else(|| String::from("no VEILOS volume at /"))?;
    if src.name == "ram" {
        return Err(String::from("/ is on the RAM disk: start from a disk image (veilos-uefi-disk.img) instead"));
    }
    let dst = crate::fs::block::disks()
        .into_iter()
        .find(|d| d.name.eq_ignore_ascii_case(target))
        .ok_or_else(|| format!("no disk {}", target))?;
    if Arc::ptr_eq(&src, &dst) {
        return Err(String::from("that is the disk VEILOS is running from"));
    }
    let plan = Plan::new(&src, root_start, root_sectors, &dst)?;
    if RUNNING.swap(true, Ordering::AcqRel) {
        return Err(String::from("an install or update is already running"));
    }
    status(format!("starting: {} onto {}", src.name, dst.name));
    crate::sched::kthread::spawn("install", move || {
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

/// A disk's GPT: its header (sector 1) and entry array.
struct Gpt {
    header: [u8; SECTOR],
    entries: Vec<u8>,
    entry_size: usize,
    count: usize,
    entry_sectors: u64,
}

impl Gpt {
    fn read(disk: &Disk) -> Option<Result<Self, String>> {
        let mut header = [0u8; SECTOR];
        if !disk.read(1, &mut header) || &header[0..8] != b"EFI PART" {
            return None;
        }
        let entries_lba = u64_at(&header, 72);
        let count = u32_at(&header, 80) as usize;
        let entry_size = u32_at(&header, 84) as usize;
        if !(128..=SECTOR).contains(&entry_size) || SECTOR % entry_size != 0 || count == 0 || count > 1024 {
            return Some(Err(format!("the GPT of {} has an entry layout this can't copy", disk.name)));
        }
        let entry_sectors = ((count * entry_size + SECTOR - 1) / SECTOR) as u64;
        let mut entries = vec![0u8; entry_sectors as usize * SECTOR];
        if !disk.read(entries_lba, &mut entries) {
            return Some(Err(format!("couldn't read the GPT entries of {}", disk.name)));
        }
        Some(Ok(Self { header, entries, entry_size, count, entry_sectors }))
    }

    /// The used entries: (index, type GUID, first LBA, last LBA).
    fn used(&self) -> Vec<(usize, [u8; 16], u64, u64)> {
        (0..self.count)
            .filter_map(|i| {
                let e = &self.entries[i * self.entry_size..(i + 1) * self.entry_size];
                if e[0..16].iter().all(|&b| b == 0) {
                    return None;
                }
                Some((i, e[0..16].try_into().unwrap(), u64_at(e, 32), u64_at(e, 40)))
            })
            .collect()
    }

    /// Split into the VEILOS volume's entry (the one starting at
    /// `volume_start`) and the boot partitions (every other), checked.
    fn layout(&self, disk: &Disk, volume_start: u64) -> Result<(usize, Vec<([u8; 16], u64, u64)>), String> {
        let mut volume_entry = None;
        let mut boot = Vec::new();
        for (i, kind, first, last) in self.used() {
            if first == volume_start {
                volume_entry = Some(i);
            } else if last >= first {
                boot.push((kind, first, last));
            }
        }
        let volume_entry = volume_entry.ok_or_else(|| format!("the VEILOS volume of {} isn't in its GPT", disk.name))?;
        if boot.is_empty() {
            return Err(format!("{} has no boot partition", disk.name));
        }
        if boot.iter().any(|&(_, _, last)| last >= volume_start) {
            return Err(format!("a boot partition of {} lies after its VEILOS volume", disk.name));
        }
        Ok((volume_entry, boot))
    }
}

/// Copy sectors `first..=last` of `src` to the same place on `dst`.
fn copy_sectors(src: &Disk, dst: &Disk, first: u64, last: u64, buf: &mut [u8]) -> Result<(), String> {
    let mut lba = first;
    while lba <= last {
        let n = ((last + 1 - lba) as usize).min(buf.len() / SECTOR);
        let b = &mut buf[..n * SECTOR];
        if !src.read(lba, b) {
            return Err(format!("reading {} sector {}", src.name, lba));
        }
        if !dst.write_through(lba, b) {
            return Err(format!("writing {} sector {}", dst.name, lba));
        }
        lba += n as u64;
        crate::sched::kthread::yield_if_slice_used(10);
    }
    Ok(())
}

impl Plan {
    fn new(src: &Disk, root_start: u64, _root_sectors: u64, dst: &Disk) -> Result<Self, String> {
        let gpt = match Gpt::read(src) {
            Some(g) => g?,
            None => return Err(String::from("the disk VEILOS runs from has no GPT (only the UEFI image can be installed)")),
        };
        let (volume_entry, boot) = gpt.layout(src, root_start)?;
        let boot = boot.into_iter().map(|(_, f, l)| (f, l)).collect();
        let Gpt { header, entries, entry_size, count, entry_sectors } = gpt;

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
        let gone = crate::fs::vfs::VFS.unmount_disk(&dst.name);
        if gone > 0 {
            status(format!("unmounted {} filesystem(s) of {}", gone, dst.name));
        }
        dst.set_install_window(true);

        // 1. Boot partitions, as they are.
        let mut buf = vec![0u8; CHUNK];
        for &(first, last) in &self.boot {
            copy_sectors(src, dst, first, last, &mut buf)?;
            status(format!("copied boot partition ({} MiB)", (last + 1 - first) / 2048));
        }

        // 2. A new FAT32 volume, then every file of `/` onto it.
        format_fat32(dst, self.vol_start, self.vol_sectors)?;
        status(format!("made a {} GiB FAT32 volume", self.vol_sectors / 2048 / 1024));
        let volume = Partition { disk: dst.clone(), start: self.vol_start, sectors: self.vol_sectors };
        let (fs, _) = crate::fs::fat::detect(&volume).ok_or_else(|| String::from("the new volume doesn't read back as FAT32"))?;
        let root = crate::fs::vfs::VFS.root_mount().ok_or_else(|| String::from("nothing mounted at /"))?;
        let mut copied = (0u64, 0u64);
        copy_tree(&*root.fs, &*root.device, &*fs, &volume, "/", &mut copied)?;
        crate::fs::block::flush_all();
        status(format!("copied {} files ({} MiB)", copied.0, copied.1 >> 20));

        // 3. The partition table, last.
        self.write_gpt(dst)?;
        crate::fs::block::flush_all();
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
        crate::sched::kthread::yield_if_slice_used(10);
    }
    Ok(())
}

// -----| UPDATE |-----

/// Disks UPDATE could upgrade, one line each: every other disk with a
/// VEILOS volume.
pub fn update_targets() -> Vec<String> {
    let root = crate::fs::block::root_location().map(|(d, _, _)| d.name.clone());
    let mut out = Vec::new();
    for l in crate::fs::block::found_volumes() {
        if l.contains("another VEILOS volume") && !root.as_ref().is_some_and(|r| l.contains(&format!("disk {} ", r))) {
            let disk = l.split("disk ").nth(1).and_then(|r| r.split(' ').next()).unwrap_or("?");
            out.push(format!("  {}: {}", disk.to_ascii_uppercase(), l.split(" at ").next().unwrap_or(&l)));
        }
    }
    out
}

/// Start updating the VEILOS installed on disk `target` from the running
/// one, keeping its users and data. Checks what it can first; the copy runs
/// on its own thread.
pub fn start_update(target: &str) -> Result<(), String> {
    let (src, root_start, _) =
        crate::fs::block::root_location().ok_or_else(|| String::from("no VEILOS volume at /"))?;
    if src.name == "ram" || crate::fs::block::boot_disks().iter().any(|(_, ramdisk)| *ramdisk) {
        return Err(String::from(
            "this VEILOS runs from a RAM disk, and the boot partition it started from carries that RAM disk: \
             copied, it would make the updated disk start from memory too. Start from the disk image \
             (veilos-uefi-disk.img) to update.",
        ));
    }
    let dst = crate::fs::block::disks()
        .into_iter()
        .find(|d| d.name.eq_ignore_ascii_case(target))
        .ok_or_else(|| format!("no disk {}", target))?;
    if Arc::ptr_eq(&src, &dst) {
        return Err(String::from("that is the disk VEILOS is running from"));
    }
    let plan = Update::new(&src, root_start, &dst)?;
    if RUNNING.swap(true, Ordering::AcqRel) {
        return Err(String::from("an install or update is already running"));
    }
    status(format!("starting: updating {} from {}", dst.name, src.name));
    crate::sched::kthread::spawn("install", move || {
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

/// What UPDATE does, worked out (and checked) before anything is written.
struct Update {
    /// (first, last) LBAs of each boot partition, the same on both disks.
    boot: Vec<(u64, u64)>,
    /// The target's VEILOS volume: start and sectors.
    vol_start: u64,
    vol_sectors: u64,
}

impl Update {
    fn new(src: &Disk, root_start: u64, dst: &Arc<Disk>) -> Result<Self, String> {
        let from = match Gpt::read(src) {
            Some(g) => g?,
            None => return Err(String::from("the disk VEILOS runs from has no GPT (only a UEFI image can update another disk)")),
        };
        let (_, boot) = from.layout(src, root_start)?;
        let to = match Gpt::read(dst) {
            Some(g) => g?,
            None => return Err(format!("{} has no GPT, so no VEILOS installed by INSTALL: use INSTALL {} ERASE", dst.name, dst.name)),
        };
        // Its VEILOS volume: the GPT entry whose filesystem has the marker.
        let mut volume = None;
        for (_, _, first, last) in to.used() {
            if last < first {
                continue;
            }
            let part = Partition { disk: dst.clone(), start: first, sectors: last - first + 1 };
            if let Some((_, fs_name)) = crate::fs::block::open_veilos_volume(&part) {
                if !fs_name.starts_with("FAT") {
                    return Err(format!("the VEILOS volume of {} is {}, which this can't write", dst.name, fs_name));
                }
                volume = Some((first, last - first + 1));
                break;
            }
        }
        let (vol_start, vol_sectors) =
            volume.ok_or_else(|| format!("{} has no VEILOS volume to update (INSTALL {} ERASE puts VEILOS on it)", dst.name, dst.name))?;
        let (_, dst_boot) = to.layout(dst, vol_start)?;
        let describe = |b: &[([u8; 16], u64, u64)]| {
            b.iter().map(|&(_, f, l)| format!("{}..{}", f, l)).collect::<Vec<_>>().join(", ")
        };
        if dst_boot != boot {
            return Err(format!(
                "the boot partitions of {} (sectors {}) aren't laid out like this disk's (sectors {}): \
                 UPDATE can only rewrite matching ones. INSTALL {} ERASE replaces everything instead.",
                dst.name,
                describe(&dst_boot),
                describe(&boot),
                dst.name
            ));
        }
        Ok(Self { boot: boot.into_iter().map(|(_, f, l)| (f, l)).collect(), vol_start, vol_sectors })
    }

    fn run(&self, src: &Arc<Disk>, dst: &Arc<Disk>) -> Result<String, String> {
        let gone = crate::fs::vfs::VFS.unmount_disk(&dst.name);
        if gone > 0 {
            status(format!("unmounted {} filesystem(s) of {}", gone, dst.name));
        }
        dst.set_install_window(true);

        // 1. Boot partitions: the new bootloader and kernel.
        let mut buf = vec![0u8; CHUNK];
        let mut boot_mib = 0;
        for &(first, last) in &self.boot {
            copy_sectors(src, dst, first, last, &mut buf)?;
            boot_mib += (last + 1 - first) / 2048;
        }
        drop(buf);
        status(format!("rewrote the boot partitions ({} MiB)", boot_mib));

        // 2. The system's files onto its volume, keeping its people's.
        let volume = Partition { disk: dst.clone(), start: self.vol_start, sectors: self.vol_sectors };
        let (fs, _) = crate::fs::block::open_veilos_volume(&volume)
            .ok_or_else(|| String::from("its VEILOS volume no longer reads as one"))?;
        let root = crate::fs::vfs::VFS.root_mount().ok_or_else(|| String::from("nothing mounted at /"))?;
        let mut n = Counts::default();
        update_tree(&*root.fs, &*root.device, &*fs, &volume, "/", Mode::Overwrite, &mut n)?;
        crate::fs::block::flush_all();
        Ok(format!(
            "DONE: updated VEILOS on {}: boot partitions ({} MiB) rewritten, {} files written ({} MiB), \
             {} kept, {} old ones removed. Start from {} to run this build.",
            dst.name.to_ascii_uppercase(),
            boot_mib,
            n.written,
            n.bytes >> 20,
            n.kept,
            n.removed,
            dst.name.to_ascii_uppercase()
        ))
    }
}

#[derive(Default)]
struct Counts {
    written: u64,
    bytes: u64,
    kept: u64,
    removed: u64,
}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    /// Write every file, replacing what's there.
    Overwrite,
    /// As Overwrite, and remove files the source doesn't have.
    Replace,
    /// Write only files the target doesn't have: its users' data.
    MissingOnly,
}

/// What UPDATE does with a top-level name of the volume.
fn mode_for(name: &str) -> Mode {
    const KEEP: [&str; 6] = ["etc", "home", "var", "tmp", "net", "config.vsh"];
    const REPLACE: [&str; 2] = ["system", "tests"];
    if KEEP.iter().any(|k| k.eq_ignore_ascii_case(name)) {
        Mode::MissingOnly
    } else if REPLACE.iter().any(|k| k.eq_ignore_ascii_case(name)) {
        Mode::Replace
    } else {
        Mode::Overwrite
    }
}

/// Copy directory `path` of `from` onto `to` the UPDATE way.
fn update_tree(
    from: &dyn FileSystem,
    from_dev: &dyn BlockDevice,
    to: &dyn FileSystem,
    to_dev: &dyn BlockDevice,
    path: &str,
    mode: Mode,
    n: &mut Counts,
) -> Result<(), String> {
    let list = from.list_dir(from_dev, path).map_err(|e| format!("listing {}: {:?}", path, e))?;
    let mut names = Vec::new();
    for entry in &list {
        if entry.name == "." || entry.name == ".." {
            continue;
        }
        names.push(entry.name.to_ascii_lowercase());
        let child = if path == "/" { format!("/{}", entry.name) } else { format!("{}/{}", path, entry.name) };
        let mode = if path == "/" { mode_for(&entry.name) } else { mode };
        match entry.file_type {
            FileType::Directory => {
                to.create_dir(to_dev, &child).map_err(|e| format!("making {}: {:?}", child, e))?;
                update_tree(from, from_dev, to, to_dev, &child, mode, n)?;
            }
            FileType::File => {
                if mode == Mode::MissingOnly && to.stat(to_dev, &child).is_some() {
                    n.kept += 1;
                    continue;
                }
                let data = from.read_file(from_dev, &child).map_err(|e| format!("reading {}: {:?}", child, e))?;
                to.write_file(to_dev, &child, &data).map_err(|e| format!("writing {}: {:?}", child, e))?;
                n.written += 1;
                n.bytes += data.len() as u64;
                if n.written % 50 == 0 {
                    status(format!("updating files: {} ({} MiB)", n.written, n.bytes >> 20));
                }
            }
        }
        crate::sched::kthread::yield_if_slice_used(10);
    }
    if mode == Mode::Replace && path != "/" {
        // Files of the old build the new one doesn't ship.
        let old = to.list_dir(to_dev, path).map_err(|e| format!("listing {} on the target: {:?}", path, e))?;
        for entry in old {
            if entry.file_type != FileType::File || names.contains(&entry.name.to_ascii_lowercase()) {
                continue;
            }
            let child = format!("{}/{}", path, entry.name);
            to.delete_file(to_dev, &child).map_err(|e| format!("removing {}: {:?}", child, e))?;
            n.removed += 1;
        }
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
        crate::sched::kthread::yield_if_slice_used(10);
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
    crate::security::entropy::fill(&mut serial);
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
    crate::security::entropy::fill(&mut g);
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
