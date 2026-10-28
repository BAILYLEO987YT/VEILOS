// =====================================================================
// -----| exFAT File System Driver |-----
//
// Read-only driver for exFAT volumes.
//
// exFAT (Extended File Allocation Table) is Microsoft's successor to
// FAT32 for flash media.  Key differences from FAT32:
//   - Boot sector layout is completely different.
//   - Directory entries are 32 bytes but use a type-based format with
//     multi-entry file records (File + Stream Extension + File Name).
//   - Filenames are native UTF-16LE (no 8.3 limitation).
//   - Cluster bitmap replaces FAT scanning for free-space tracking.
//   - Sector/cluster sizes are encoded as power-of-2 shifts.
// =====================================================================

use crate::ahci::HbaPort;
use crate::println;
use crate::vfs::{BlockDevice, FileSystem, DirEntry, FileType, VfsError};
use x86_64::VirtAddr;
use alloc::vec::Vec;
use alloc::string::String;

// -----| constants |-----

const EXFAT_OEM_ID: &[u8; 8] = b"EXFAT   ";

// Directory entry type codes
const ENTRY_END:              u8 = 0x00;
#[allow(dead_code)] const ENTRY_ALLOC_BITMAP:     u8 = 0x81;
#[allow(dead_code)] const ENTRY_UPCASE_TABLE:     u8 = 0x82;
#[allow(dead_code)] const ENTRY_VOLUME_LABEL:     u8 = 0x83;
const ENTRY_FILE:             u8 = 0x85;
const ENTRY_STREAM_EXTENSION: u8 = 0xC0;
const ENTRY_FILE_NAME:        u8 = 0xC1;

// File attributes
const ATTR_DIRECTORY: u16 = 0x0010;

// -----| boot sector |-----

/// exFAT Boot Sector (first 512 bytes of the volume).
///
/// Field access is done via helper methods to avoid packed-struct
/// alignment issues.
#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct ExFatBootSector {
    pub jump_boot:                 [u8; 3],
    pub filesystem_name:           [u8; 8],   // "EXFAT   "
    pub must_be_zero:              [u8; 53],  // bytes 0x0B–0x3F
    pub partition_offset:          u64,       // 0x40
    pub volume_length:             u64,       // 0x48 (sectors)
    pub fat_offset:                u32,       // 0x50 (sectors from start)
    pub fat_length:                u32,       // 0x54 (sectors)
    pub cluster_heap_offset:      u32,       // 0x58 (sectors from start)
    pub cluster_count:             u32,       // 0x5C
    pub first_cluster_of_root_dir: u32,       // 0x60
    pub volume_serial_number:      u32,       // 0x64
    pub filesystem_revision:       u16,       // 0x68
    pub volume_flags:              u16,       // 0x6A
    pub bytes_per_sector_shift:    u8,        // 0x6C (log2)
    pub sectors_per_cluster_shift: u8,        // 0x6D (log2)
    pub number_of_fats:            u8,        // 0x6E
    pub drive_select:              u8,        // 0x6F
    pub percent_in_use:            u8,        // 0x70
    pub reserved:                  [u8; 7],   // 0x71
    pub boot_code:                 [u8; 390], // 0x78
    pub boot_signature:            u16,       // 0x1FE — 0xAA55
}

// =====================================================================
// -----| ExFatFileSystem |-----
// =====================================================================

#[derive(Clone, Copy)]
pub struct ExFatFileSystem {
    pub bytes_per_sector:    u32,
    pub sectors_per_cluster: u32,
    pub bytes_per_cluster:   u32,
    pub fat_offset:          u32,  // sectors from volume start
    pub cluster_heap_offset: u32,  // sectors from volume start
    pub cluster_count:       u32,
    pub root_cluster:        u32,
}

impl ExFatFileSystem {

    // -----| geometry helpers |-----

    /// Convert a cluster number to an LBA.
    /// exFAT clusters start at 2 (like FAT32).
    fn cluster_to_lba(&self, cluster: u32) -> u64 {
        (self.cluster_heap_offset as u64) +
            ((cluster - 2) as u64) * (self.sectors_per_cluster as u64)
    }

    // -----| FAT chain traversal |-----

    /// Read the next cluster value from the FAT (32-bit entries).
    fn get_next_cluster(&self, device: &dyn BlockDevice, cluster: u32) -> u32 {
        let fat_offset_bytes = (cluster as u64) * 4;
        let sector = (self.fat_offset as u64) + fat_offset_bytes / 512;
        let offset = (fat_offset_bytes % 512) as usize;

        let mut buffer = [0u8; 512];
        if !device.read_sectors(sector, 1, buffer.as_mut_ptr() as u64) {
            return 0;
        }

        let value = u32::from_le_bytes([
            buffer[offset], buffer[offset + 1],
            buffer[offset + 2], buffer[offset + 3],
        ]);

        // End-of-chain: 0xFFFFFFFF
        if value >= 0xFFFFFFF8 { 0 } else { value }
    }

    /// Check if a cluster value is end-of-chain.
    fn is_eoc(&self, cluster: u32) -> bool {
        cluster == 0 || cluster >= 0xFFFFFFF8
    }

    // -----| cluster chain reading |-----

    /// Read all data from a cluster chain, up to `max_bytes`.
    fn read_chain(
        &self, device: &dyn BlockDevice, start_cluster: u32, max_bytes: u64,
    ) -> Result<Vec<u8>, VfsError> {
        let bpc = self.bytes_per_cluster as usize;
        let spc = self.sectors_per_cluster;
        let mut data = Vec::new();
        let mut cluster = start_cluster;

        while !self.is_eoc(cluster) && (data.len() as u64) < max_bytes {
            let lba = self.cluster_to_lba(cluster);
            let mut buf = alloc::vec![0u8; bpc];

            if !device.read_sectors(lba, spc, buf.as_mut_ptr() as u64) {
                return Err(VfsError::ReadError);
            }

            let bytes_left = (max_bytes as usize).saturating_sub(data.len());
            let copy_len = bytes_left.min(bpc);
            data.extend_from_slice(&buf[..copy_len]);

            cluster = self.get_next_cluster(device, cluster);
        }

        Ok(data)
    }

    // -----| directory parsing |-----

    /// Read all raw directory entry data from a directory cluster chain.
    fn read_dir_data(
        &self, device: &dyn BlockDevice, start_cluster: u32,
    ) -> Result<Vec<u8>, VfsError> {
        // Directories don't have a "file size" — read their full chain.
        // Cap at a reasonable maximum to avoid runaway reads.
        self.read_chain(device, start_cluster, 16 * 1024 * 1024)
    }

    /// Parse exFAT directory entries from raw data.
    ///
    /// exFAT file records consist of:
    ///   1. File Directory Entry (0x85) — attributes, secondary count
    ///   2. Stream Extension (0xC0) — file size, first cluster, name len
    ///   3. File Name Entry(s) (0xC1) — up to 15 UTF-16 chars each
    fn parse_directory(&self, data: &[u8]) -> Vec<DirEntry> {
        let mut entries = Vec::new();
        let entry_count = data.len() / 32;
        let mut i = 0;

        while i < entry_count {
            let base = i * 32;
            if base + 32 > data.len() { break; }

            let entry_type = data[base];

            // End of directory
            if entry_type == ENTRY_END { break; }

            // We only care about File entries (0x85)
            if entry_type != ENTRY_FILE {
                i += 1;
                continue;
            }

            // File Directory Entry format:
            //   Byte 0: Type (0x85)
            //   Byte 1: Secondary count
            //   Bytes 4-5: File attributes (u16)
            let secondary_count = data[base + 1] as usize;
            let attributes = u16::from_le_bytes([data[base + 4], data[base + 5]]);
            let is_dir = (attributes & ATTR_DIRECTORY) != 0;

            // Now read the secondary entries
            let mut file_size:     u64 = 0;
            let mut first_cluster: u32 = 0;
            let mut name_chars:    Vec<u16> = Vec::new();
            let mut name_len:      u8 = 0;

            for s in 1..=secondary_count {
                let si = i + s;
                if si >= entry_count { break; }
                let sb = si * 32;
                if sb + 32 > data.len() { break; }

                let stype = data[sb];

                match stype {
                    ENTRY_STREAM_EXTENSION => {
                        // Stream Extension Entry:
                        //   Byte 3: Name length (Unicode chars)
                        //   Bytes 8-15: Valid data length (u64)
                        //   Bytes 20-23: First cluster (u32)
                        //   Bytes 24-31: Data length (u64)
                        name_len = data[sb + 3];
                        file_size = u64::from_le_bytes([
                            data[sb + 24], data[sb + 25],
                            data[sb + 26], data[sb + 27],
                            data[sb + 28], data[sb + 29],
                            data[sb + 30], data[sb + 31],
                        ]);
                        first_cluster = u32::from_le_bytes([
                            data[sb + 20], data[sb + 21],
                            data[sb + 22], data[sb + 23],
                        ]);
                    }
                    ENTRY_FILE_NAME => {
                        // File Name Entry:
                        //   Bytes 2-31: 15 UTF-16LE characters
                        for c in 0..15 {
                            let ci = sb + 2 + c * 2;
                            if ci + 1 >= data.len() { break; }
                            let ch = u16::from_le_bytes([data[ci], data[ci + 1]]);
                            if ch == 0 { break; }
                            name_chars.push(ch);
                        }
                    }
                    _ => {}
                }
            }

            // Truncate name to the specified length
            name_chars.truncate(name_len as usize);

            // Convert UTF-16 to String
            let name = utf16_to_string(&name_chars);

            if !name.is_empty() {
                entries.push(DirEntry {
                    name,
                    size:      file_size,
                    cluster:   first_cluster,
                    file_type: if is_dir { FileType::Directory } else { FileType::File },
                });
            }

            // Skip past all secondary entries
            i += 1 + secondary_count;
        }

        entries
    }

    // -----| path resolution |-----

    fn resolve_path(
        &self, device: &dyn BlockDevice, path: &str,
    ) -> Result<(u32, u64, bool), VfsError> {
        let components: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();

        if components.is_empty() {
            return Ok((self.root_cluster, 0, true));
        }

        let mut current_cluster = self.root_cluster;

        for (i, component) in components.iter().enumerate() {
            let dir_data = self.read_dir_data(device, current_cluster)?;
            let entries = self.parse_directory(&dir_data);

            let mut found = false;
            for entry in &entries {
                if entry.name.eq_ignore_ascii_case(component) {
                    if i == components.len() - 1 {
                        return Ok((
                            entry.cluster,
                            entry.size,
                            entry.file_type == FileType::Directory,
                        ));
                    }
                    if entry.file_type != FileType::Directory {
                        return Err(VfsError::NotADirectory);
                    }
                    current_cluster = entry.cluster;
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

// -----| UTF-16 helper |-----

fn utf16_to_string(chars: &[u16]) -> String {
    let mut s = String::new();
    for &code in chars {
        if code == 0 { break; }
        if let Some(c) = char::from_u32(code as u32) {
            s.push(c);
        } else {
            s.push('?');
        }
    }
    s
}

// =====================================================================
// -----| FileSystem trait implementation |-----
// =====================================================================

impl FileSystem for ExFatFileSystem {

    fn list_dir(
        &self, device: &dyn BlockDevice, path: &str,
    ) -> Result<Vec<DirEntry>, VfsError> {
        let (cluster, _, is_dir) = self.resolve_path(device, path)?;
        if !is_dir {
            return Err(VfsError::NotADirectory);
        }
        let data = self.read_dir_data(device, cluster)?;
        Ok(self.parse_directory(&data))
    }

    fn read_file(
        &self, device: &dyn BlockDevice, path: &str,
    ) -> Result<Vec<u8>, VfsError> {
        let (cluster, size, is_dir) = self.resolve_path(device, path)?;
        if is_dir {
            return Err(VfsError::IsDirectory);
        }
        self.read_chain(device, cluster, size)
    }
}

// =====================================================================
// -----| detection and mounting |-----
// =====================================================================

/// Probe for an exFAT volume on the given AHCI port.
///
/// Checks the "EXFAT   " OEM ID at bytes 3-10 of the boot sector.
pub fn probe_exfat(
    port: &mut HbaPort,
    physical_memory_offset: VirtAddr,
    partition_lba: u64,
) {
    // Sector buffer the HBA DMAs into: below 4 GiB, kept for the mount's lifetime.
    let (buffer_phys, _) = crate::memory::DmaRegion::new(4096).expect("No DMA memory for exFAT probe").leak();
    let buffer_virt = physical_memory_offset.as_u64() + buffer_phys;

    if !port.read_sectors(partition_lba, 1, buffer_phys, physical_memory_offset) {
        return;
    }

    let raw = unsafe { core::slice::from_raw_parts(buffer_virt as *const u8, 512) };

    // Check for "EXFAT   " at offset 3
    if &raw[3..11] != EXFAT_OEM_ID {
        return;
    }

    // Boot signature at 0x1FE must be 0xAA55.
    let boot_sig = u16::from_le_bytes([raw[0x1FE], raw[0x1FF]]);
    if boot_sig != 0xAA55 { return; }

    let bpb = unsafe { &*(buffer_virt as *const ExFatBootSector) };

    // Extract fields safely (packed struct).
    let bps_shift  = { bpb.bytes_per_sector_shift };
    let spc_shift  = { bpb.sectors_per_cluster_shift };
    let bytes_per_sector    = 1u32 << bps_shift;
    let sectors_per_cluster = 1u32 << spc_shift;
    let bytes_per_cluster   = bytes_per_sector * sectors_per_cluster;
    let fat_offset          = { bpb.fat_offset };
    let cluster_heap_offset = { bpb.cluster_heap_offset };
    let cluster_count       = { bpb.cluster_count };
    let root_cluster        = { bpb.first_cluster_of_root_dir };

    println!("\n  -> exFAT File System Detected!");
    println!("     - Bytes/Sector: {}", bytes_per_sector);
    println!("     - Sectors/Cluster: {}", sectors_per_cluster);
    println!("     - Root Cluster: {}", root_cluster);

    let fs = ExFatFileSystem {
        bytes_per_sector,
        sectors_per_cluster,
        bytes_per_cluster,
        fat_offset,
        cluster_heap_offset,
        cluster_count,
        root_cluster,
    };

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

    println!("  -> Mounted exFAT to root (/)");

    // Quick verify
    if let Ok(content) = vfs.read_file("/HELLO.TXT") {
        let s = core::str::from_utf8(&content).unwrap_or("[Binary]");
        println!("     - /HELLO.TXT: \"{}\"", s.trim_matches(char::from(0)));
    } else {
        println!("     - /HELLO.TXT: not found");
    }

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
