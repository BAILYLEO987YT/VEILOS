// =====================================================================
// -----| FAT12 / FAT16 File System Driver |-----
//
// Read-only driver for FAT12 and FAT16 volumes.  Covers the format
// QEMU's VVFAT emulation typically generates for small host folders.
//
// Key differences from FAT32:
//   - Root directory is a fixed-size area (not a cluster chain).
//   - FAT entries are 12 or 16 bits wide (not 32).
//   - Extended BPB fields start at byte offset 36 (not 64).
//   - The total-cluster count determines the variant:
//       < 4085   → FAT12
//       < 65525  → FAT16
// =====================================================================

use crate::ahci::HbaPort;
use crate::println;
use crate::vfs::{BlockDevice, FileSystem, DirEntry, FileType, VfsError};
use x86_64::VirtAddr;
use alloc::vec::Vec;
use alloc::string::String;

// -----| fat variant enum |-----

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FatVariant {
    Fat12,
    Fat16,
}

// -----| BPB structure |-----

/// BIOS Parameter Block for FAT12 / FAT16 volumes.
///
/// Identical to the FAT32 BPB for the first 36 bytes, after which the
/// extended fields differ.
#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct Fat16BPB {
    pub jmp_instruction:          [u8; 3],
    pub oem_name:                 [u8; 8],
    pub bytes_per_sector:         u16,
    pub sectors_per_cluster:      u8,
    pub reserved_sectors:         u16,
    pub fat_count:                u8,
    pub root_entry_count:         u16,
    pub total_sectors_16:         u16,
    pub media_type:               u8,
    pub sectors_per_fat_16:       u16,
    pub sectors_per_track:        u16,
    pub head_count:               u16,
    pub hidden_sectors:           u32,
    pub total_sectors_32:         u32,
    // -----| extended BPB (FAT12/16) |-----
    pub drive_number:             u8,
    pub reserved1:                u8,
    pub extended_boot_signature:  u8,   // 0x28 or 0x29
    pub volume_id:                u32,
    pub volume_label:             [u8; 11],
    pub fs_type:                  [u8; 8],
    pub boot_code:                [u8; 448],
    pub boot_signature:           u16,  // 0xAA55
}

// -----| directory entry (same 32-byte format as FAT32) |-----

#[repr(C, packed)]
pub struct FatDirEntry {
    pub name:                [u8; 11],
    pub attributes:          u8,
    pub reserved_winnt:      u8,
    pub creation_time_tenth: u8,
    pub creation_time:       u16,
    pub creation_date:       u16,
    pub last_access_date:    u16,
    pub cluster_hi:          u16,
    pub write_time:          u16,
    pub write_date:          u16,
    pub cluster_lo:          u16,
    pub file_size:           u32,
}

impl FatDirEntry {
    fn is_end(&self) -> bool  { self.name[0] == 0x00 }
    fn is_free(&self) -> bool { self.name[0] == 0xE5 }

    fn get_name(&self) -> String {
        let mut name = String::new();
        for i in 0..8 {
            if self.name[i] == b' ' { break; }
            name.push(self.name[i] as char);
        }
        let mut ext = String::new();
        for i in 8..11 {
            if self.name[i] == b' ' { break; }
            ext.push(self.name[i] as char);
        }
        if !ext.is_empty() {
            name.push('.');
            name.push_str(&ext);
        }
        name
    }

    fn get_cluster(&self) -> u32 {
        // FAT12/16 volumes ignore cluster_hi (always 0), but read it
        // for correctness on FAT16 volumes that happen to use it.
        let lo = { self.cluster_lo } as u32;
        let hi = { self.cluster_hi } as u32;
        lo | (hi << 16)
    }
}

// =====================================================================
// -----| Fat16FileSystem |-----
// =====================================================================

#[derive(Clone, Copy)]
pub struct Fat16FileSystem {
    pub bpb:     Fat16BPB,
    pub variant: FatVariant,
}

impl Fat16FileSystem {

    // -----| geometry helpers |-----

    /// Total sectors on the volume.
    #[allow(dead_code)]
    fn total_sectors(&self) -> u32 {
        let s16 = { self.bpb.total_sectors_16 } as u32;
        if s16 != 0 { s16 } else { self.bpb.total_sectors_32 }
    }

    /// Number of sectors occupied by the root directory.
    fn root_dir_sectors(&self) -> u32 {
        let bps = { self.bpb.bytes_per_sector } as u32;
        let rec = { self.bpb.root_entry_count } as u32;
        ((rec * 32) + bps - 1) / bps
    }

    /// LBA of the first sector of the root directory.
    fn root_dir_lba(&self) -> u64 {
        let reserved   = { self.bpb.reserved_sectors } as u64;
        let fat_count  = { self.bpb.fat_count } as u64;
        let spf        = { self.bpb.sectors_per_fat_16 } as u64;
        reserved + fat_count * spf
    }

    /// LBA of the first data sector (cluster 2).
    fn first_data_sector(&self) -> u32 {
        let reserved   = { self.bpb.reserved_sectors } as u32;
        let fat_count  = { self.bpb.fat_count } as u32;
        let spf        = { self.bpb.sectors_per_fat_16 } as u32;
        reserved + fat_count * spf + self.root_dir_sectors()
    }

    /// Convert a cluster number to an LBA.
    fn cluster_to_lba(&self, cluster: u32) -> u64 {
        let spc = { self.bpb.sectors_per_cluster } as u32;
        (self.first_data_sector() + (cluster - 2) * spc) as u64
    }

    // -----| FAT chain traversal |-----

    /// Read the next cluster value from the FAT.
    fn get_next_cluster(&self, device: &dyn BlockDevice, cluster: u32) -> u32 {
        match self.variant {
            FatVariant::Fat12 => self._next_cluster_12(device, cluster),
            FatVariant::Fat16 => self._next_cluster_16(device, cluster),
        }
    }

    fn _next_cluster_12(&self, device: &dyn BlockDevice, cluster: u32) -> u32 {
        let fat_start = { self.bpb.reserved_sectors } as u64;
        let fat_offset = cluster + (cluster / 2); // 1.5 bytes per entry
        let sector = fat_start + (fat_offset as u64 / 512);
        let offset = (fat_offset % 512) as usize;

        // We may straddle two sectors, so read 2 sectors to be safe.
        let mut buffer = [0u8; 1024];
        if !device.read_sectors(sector, 2, buffer.as_mut_ptr() as u64) {
            return 0;
        }

        let raw = u16::from_le_bytes([buffer[offset], buffer[offset + 1]]);
        let value = if cluster & 1 == 0 {
            raw & 0x0FFF
        } else {
            raw >> 4
        };

        if value >= 0x0FF8 { 0 } else { value as u32 }
    }

    fn _next_cluster_16(&self, device: &dyn BlockDevice, cluster: u32) -> u32 {
        let fat_start = { self.bpb.reserved_sectors } as u64;
        let fat_offset = (cluster * 2) as u64;
        let sector = fat_start + fat_offset / 512;
        let offset = (fat_offset % 512) as usize;

        let mut buffer = [0u8; 512];
        if !device.read_sectors(sector, 1, buffer.as_mut_ptr() as u64) {
            return 0;
        }

        let value = u16::from_le_bytes([buffer[offset], buffer[offset + 1]]);
        if value >= 0xFFF8 { 0 } else { value as u32 }
    }

    /// Check if a cluster value is end-of-chain.
    fn is_eoc(&self, cluster: u32) -> bool {
        cluster == 0 || match self.variant {
            FatVariant::Fat12 => cluster >= 0x0FF8,
            FatVariant::Fat16 => cluster >= 0xFFF8,
        }
    }

    // -----| directory enumeration |-----

    /// Read entries from the fixed root directory.
    fn get_root_entries(&self, device: &dyn BlockDevice) -> Result<Vec<DirEntry>, VfsError> {
        let lba = self.root_dir_lba();
        let sectors = self.root_dir_sectors();

        let buf_size = (sectors * 512) as usize;
        let mut buffer = alloc::vec![0u8; buf_size];
        if !device.read_sectors(lba, sectors, buffer.as_mut_ptr() as u64) {
            return Err(VfsError::ReadError);
        }

        self.parse_dir_buffer(&buffer)
    }

    /// Read entries from a subdirectory stored in a cluster chain.
    fn get_subdir_entries(
        &self, device: &dyn BlockDevice, start_cluster: u32,
    ) -> Result<Vec<DirEntry>, VfsError> {
        let spc = { self.bpb.sectors_per_cluster } as u32;
        let bpc = (spc * 512) as usize;
        let mut all_data = Vec::new();
        let mut cluster = start_cluster;
        let mut buf = alloc::vec![0u8; bpc];

        while !self.is_eoc(cluster) {
            let lba = self.cluster_to_lba(cluster);
            if !device.read_sectors(lba, spc, buf.as_mut_ptr() as u64) {
                return Err(VfsError::ReadError);
            }
            all_data.extend_from_slice(&buf);
            cluster = self.get_next_cluster(device, cluster);
        }

        self.parse_dir_buffer(&all_data)
    }

    /// Parse a raw buffer of 32-byte directory entries.
    fn parse_dir_buffer(&self, buffer: &[u8]) -> Result<Vec<DirEntry>, VfsError> {
        let mut entries = Vec::new();
        let raw_entries = unsafe {
            core::slice::from_raw_parts(
                buffer.as_ptr() as *const FatDirEntry,
                buffer.len() / 32,
            )
        };

        for entry in raw_entries {
            if entry.is_end() { break; }
            if entry.is_free() { continue; }
            if (entry.attributes & 0x08) != 0 { continue; } // Volume label
            if (entry.attributes & 0x0F) == 0x0F { continue; } // LFN entry

            entries.push(DirEntry {
                name:      entry.get_name(),
                size:      { entry.file_size } as u64,
                cluster:   entry.get_cluster(),
                file_type: if (entry.attributes & 0x10) != 0 {
                    FileType::Directory
                } else {
                    FileType::File
                },
            });
        }

        Ok(entries)
    }

    // -----| path resolution |-----

    /// Resolve a path to (cluster, file_size, is_directory).
    ///
    /// Cluster 0 is used as a sentinel for "root directory" (which is
    /// not stored in a cluster chain on FAT12/16).
    fn resolve_path(
        &self, device: &dyn BlockDevice, path: &str,
    ) -> Result<(u32, u32, bool), VfsError> {
        let components: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();

        if components.is_empty() {
            return Ok((0, 0, true)); // Root directory
        }

        // Start from root.
        let mut entries = self.get_root_entries(device)?;

        for (i, component) in components.iter().enumerate() {
            let mut found = false;
            for entry in &entries {
                if entry.name.eq_ignore_ascii_case(component) {
                    if i == components.len() - 1 {
                        return Ok((
                            entry.cluster,
                            entry.size as u32,
                            entry.file_type == FileType::Directory,
                        ));
                    }
                    if entry.file_type != FileType::Directory {
                        return Err(VfsError::NotADirectory);
                    }
                    entries = self.get_subdir_entries(device, entry.cluster)?;
                    found = true;
                    break;
                }
            }
            if !found {
                return Err(VfsError::FileNotFound);
            }
        }

        Err(VfsError::FileNotFound)
    }
}

// =====================================================================
// -----| FileSystem trait implementation |-----
// =====================================================================

impl FileSystem for Fat16FileSystem {

    fn list_dir(
        &self, device: &dyn BlockDevice, path: &str,
    ) -> Result<Vec<DirEntry>, VfsError> {
        let (cluster, _, is_dir) = self.resolve_path(device, path)?;
        if !is_dir {
            return Err(VfsError::NotADirectory);
        }
        if cluster == 0 {
            self.get_root_entries(device)
        } else {
            self.get_subdir_entries(device, cluster)
        }
    }

    fn read_file(
        &self, device: &dyn BlockDevice, path: &str,
    ) -> Result<Vec<u8>, VfsError> {
        let (cluster, size, is_dir) = self.resolve_path(device, path)?;
        if is_dir {
            return Err(VfsError::IsDirectory);
        }

        let spc = { self.bpb.sectors_per_cluster } as u32;
        let bpc = (spc * 512) as usize;
        let mut data = Vec::with_capacity(size as usize);
        let mut current_cluster = cluster;
        let mut buf = alloc::vec![0u8; bpc];

        while !self.is_eoc(current_cluster) {
            let lba = self.cluster_to_lba(current_cluster);
            
            if !device.read_sectors(lba, spc, buf.as_mut_ptr() as u64) {
                return Err(VfsError::ReadError);
            }

            let bytes_left = (size as usize).saturating_sub(data.len());
            let copy_len = bytes_left.min(bpc);
            data.extend_from_slice(&buf[..copy_len]);

            if data.len() >= size as usize { break; }
            current_cluster = self.get_next_cluster(device, current_cluster);
        }

        Ok(data)
    }
}

// =====================================================================
// -----| detection and mounting |-----
// =====================================================================

/// Determine FAT variant from BPB fields using the Microsoft-defined
/// cluster-count algorithm.
fn classify_fat(bpb: &Fat16BPB) -> FatVariant {
    let total_sectors = {
        let s16 = { bpb.total_sectors_16 } as u32;
        if s16 != 0 { s16 } else { bpb.total_sectors_32 }
    };
    let bps = { bpb.bytes_per_sector } as u32;
    let rec = { bpb.root_entry_count } as u32;
    let root_dir_sectors = ((rec * 32) + bps - 1) / bps;
    let reserved = { bpb.reserved_sectors } as u32;
    let fat_count = { bpb.fat_count } as u32;
    let spf = { bpb.sectors_per_fat_16 } as u32;

    let data_sectors = total_sectors
        .saturating_sub(reserved)
        .saturating_sub(fat_count * spf)
        .saturating_sub(root_dir_sectors);
    let spc = { bpb.sectors_per_cluster } as u32;
    let total_clusters = if spc > 0 { data_sectors / spc } else { 0 };

    if total_clusters < 4085 {
        FatVariant::Fat12
    } else {
        FatVariant::Fat16
    }
}

/// Probe for a FAT12 or FAT16 volume on the given AHCI port.
///
/// Checks the extended boot signature at byte 38 (the FAT12/16 location)
/// and mounts to the VFS root if the volume passes validation.
pub fn probe_fat16(
    port: &mut HbaPort,
    physical_memory_offset: VirtAddr,
    partition_lba: u64,
) {
    // Sector buffer the HBA DMAs into: below 4 GiB, kept for the mount's lifetime.
    let (buffer_phys, _) = crate::memory::DmaRegion::new(4096).expect("No DMA memory for FAT16 probe").leak();
    let buffer_virt = physical_memory_offset.as_u64() + buffer_phys;

    if !port.read_sectors(partition_lba, 1, buffer_phys, physical_memory_offset) {
        return;
    }

    let raw = unsafe { core::slice::from_raw_parts(buffer_virt as *const u8, 512) };

    // Boot signature at 0x1FE must be 0xAA55.
    let boot_sig = u16::from_le_bytes([raw[0x1FE], raw[0x1FF]]);
    if boot_sig != 0xAA55 { return; }

    // We can't strictly rely on the extended boot signature (0x28 or 0x29) 
    // at byte 38, because older DOS versions and QEMU's `vvfat` do not set it.
    // Instead we do a structural check on the BPB itself.
    let bps = u16::from_le_bytes([raw[11], raw[12]]);
    let spc = raw[13];
    let reserved = u16::from_le_bytes([raw[14], raw[15]]);
    let fat_count = raw[16];
    let root_count = u16::from_le_bytes([raw[17], raw[18]]);

    // Validate sane FAT values
    if bps == 0 || spc == 0 || reserved == 0 || fat_count == 0 { return; }
    if root_count == 0 { return; } // FAT32 has root_count = 0, so this safely skips FAT32.

    // Make sure this is NOT a FAT32 volume (FAT32 also has 0xAA55 but
    // its ext sig is at byte 66, and sectors_per_fat_16 == 0).
    let spf16 = u16::from_le_bytes([raw[22], raw[23]]);
    if spf16 == 0 { return; } // Likely FAT32 — leave for fat32.rs.

    let bpb = unsafe { &*(buffer_virt as *const Fat16BPB) };
    let variant = classify_fat(bpb);

    println!("  -> {:?} File System Detected!", variant);

    let fs = Fat16FileSystem { bpb: *bpb, variant };
    let device = crate::ahci::AhciBlockDevice {
        port: port as *mut HbaPort,
        physical_memory_offset,
        bounce_buffer_phys: buffer_phys,
        partition_lba,
    };

    use alloc::boxed::Box;
    let mut vfs = crate::vfs::VFS.lock();

    vfs.mount(
        String::from("/"),
        Box::new(fs),
        Box::new(device),
    );

    let variant_str = match variant {
        FatVariant::Fat12 => "FAT12",
        FatVariant::Fat16 => "FAT16",
    };
    println!("  -> Mounted {} to root (/)", variant_str);

    // Quick verify — file read
    if let Ok(content) = vfs.read_file("/HELLO.TXT") {
        let s = core::str::from_utf8(&content).unwrap_or("[Binary]");
        println!("     - /HELLO.TXT: \"{}\"", s.trim_matches(char::from(0)));
    } else {
        println!("     - /HELLO.TXT: not found");
    }

    // Quick verify — root listing
    match vfs.list_dir("/") {
        Ok(entries) => {
            println!("     - Root dir: {} entries", entries.len());
            for e in &entries {
                println!("       {} {}",
                    if e.file_type == FileType::Directory { "D" } else { "F" },
                    e.name,
                );
            }
        }
        Err(e) => println!("     - Root dir listing FAILED: {:?}", e),
    }
}
