use crate::ahci::HbaPort;
use crate::println;
use crate::vfs::{BlockDevice, FileSystem, DirEntry, FileType, VfsError};
use x86_64::VirtAddr;
use x86_64::structures::paging::FrameAllocator;
use x86_64::structures::paging::Size4KiB;
use alloc::vec::Vec;
use alloc::string::String;

// -----| fat32 structures |-----

/// The BIOS Parameter Block (BPB) for FAT32.
#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct Fat32BPB {
    pub jmp_instruction: [u8; 3],
    pub oem_name: [u8; 8],
    pub bytes_per_sector: u16,
    pub sectors_per_cluster: u8,
    pub reserved_sectors: u16,
    pub fat_count: u8,
    pub root_entry_count: u16,
    pub total_sectors_16: u16,
    pub media_type: u8,
    pub sectors_per_fat_16: u16,
    pub sectors_per_track: u16,
    pub head_count: u16,
    pub hidden_sectors: u32,
    pub total_sectors_32: u32,
    
    // FAT32 Extended Fields
    pub sectors_per_fat_32: u32,
    pub extended_flags: u16,
    pub fat_version: u16,
    pub root_cluster: u32,
    pub fs_info_sector: u16,
    pub backup_boot_sector: u16,
    pub reserved: [u8; 12],
    pub drive_number: u8,
    pub reserved1: u8,
    pub extended_boot_signature: u8,
    pub volume_id: u32,
    pub volume_label: [u8; 11],
    pub system_id: [u8; 8],
    pub boot_code: [u8; 420],
    pub boot_signature: u16,
}

#[repr(C, packed)]
pub struct FatDirectoryEntry {
    pub name: [u8; 11],
    pub attributes: u8,
    pub reserved_winnt: u8,
    pub creation_time_tenth: u8,
    pub creation_time: u16,
    pub creation_date: u16,
    pub last_access_date: u16,
    pub cluster_hi: u16,
    pub write_time: u16,
    pub write_date: u16,
    pub cluster_lo: u16,
    pub file_size: u32,
}

impl FatDirectoryEntry {
    pub fn is_end(&self) -> bool { self.name[0] == 0x00 }
    pub fn is_free(&self) -> bool { self.name[0] == 0xE5 }

    pub fn get_name(&self) -> String {
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

    pub fn get_cluster(&self) -> u32 {
        (self.cluster_lo as u32) | ((self.cluster_hi as u32) << 16)
    }
}

// -----| fat32 implementation |-----

#[derive(Clone, Copy)]
pub struct Fat32FileSystem {
    pub bpb: Fat32BPB,
}

impl Fat32FileSystem {
    pub fn cluster_to_lba(&self, cluster: u32) -> u64 {
        let fat_size = self.bpb.sectors_per_fat_32;
        let reserved = self.bpb.reserved_sectors as u32;
        let fat_count = self.bpb.fat_count as u32;
        
        let first_data_sector = reserved + (fat_count * fat_size);
        let sectors_per_cluster = self.bpb.sectors_per_cluster as u32;
        
        (first_data_sector + (cluster - 2) * sectors_per_cluster) as u64
    }

    fn get_next_cluster(&self, device: &dyn BlockDevice, cluster: u32) -> u32 {
        let fat_offset = cluster * 4;
        let fat_sector = (self.bpb.reserved_sectors as u32) + (fat_offset / 512);
        let ent_offset = fat_offset % 512;

        let mut buffer = [0u8; 512];
        if device.read_sectors(fat_sector as u64, 1, buffer.as_mut_ptr() as u64) {
            let next_cluster = unsafe {
                *(buffer.as_ptr().add(ent_offset as usize) as *const u32)
            } & 0x0FFFFFFF;

            if next_cluster >= 0x0FFFFFF8 {
                0 // End of chain
            } else {
                next_cluster
            }
        } else {
            crate::serial_println!("[FAT32] get_next_cluster({}) read_sectors FAILED", cluster);
            0
        }
    }

    fn resolve_path(&self, device: &dyn BlockDevice, path: &str) -> Result<(u32, u32, bool), VfsError> {
        let mut current_cluster = self.bpb.root_cluster;
        let components: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();

        if components.is_empty() {
            return Ok((current_cluster, 0, true)); // Root
        }

        for (i, component) in components.iter().enumerate() {
            let entries = self.get_entries(device, current_cluster)?;
            let mut found = false;
            for entry in &entries {
                if entry.name.eq_ignore_ascii_case(component) {
                    current_cluster = entry.cluster;
                    if i == components.len() - 1 {
                        return Ok((entry.cluster, entry.size as u32, entry.file_type == FileType::Directory));
                    }
                    if entry.file_type != FileType::Directory {
                        return Err(VfsError::NotADirectory);
                    }
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

    fn get_entries(&self, device: &dyn BlockDevice, cluster: u32) -> Result<Vec<DirEntry>, VfsError> {
        let mut entries = Vec::new();
        let mut current_cluster = cluster;
        let sectors_per_cluster = self.bpb.sectors_per_cluster as u32;
        let mut buffer = alloc::vec![0u8; (sectors_per_cluster * 512) as usize];

        while current_cluster != 0 {
            let lba = self.cluster_to_lba(current_cluster);

            if device.read_sectors(lba, sectors_per_cluster, buffer.as_mut_ptr() as u64) {
                let raw_entries = unsafe {
                    core::slice::from_raw_parts(
                        buffer.as_ptr() as *const FatDirectoryEntry,
                        (sectors_per_cluster * 512) as usize / 32
                    )
                };

                for entry in raw_entries {
                    if entry.is_end() { return Ok(entries); }
                    if entry.is_free() { continue; }
                    if (entry.attributes & 0x08) != 0 { continue; } // Volume Label

                    entries.push(DirEntry {
                        name: entry.get_name(),
                        size: entry.file_size as u64,
                        cluster: entry.get_cluster(),
                        file_type: if (entry.attributes & 0x10) != 0 { FileType::Directory } else { FileType::File },
                    });
                }
            } else {
                return Err(VfsError::ReadError);
            }

            current_cluster = self.get_next_cluster(device, current_cluster);
        }

        Ok(entries)
    }
}

// =====================================================================
// -----| fat32 write helpers |-----
// =====================================================================

impl Fat32FileSystem {

    // ---- FAT table I/O -----------------------------------------------

    fn fat_lba(&self, cluster: u32) -> u64 {
        let fat_offset = (cluster as u64) * 4;
        self.bpb.reserved_sectors as u64 + fat_offset / 512
    }

    fn fat_entry_byte_offset(cluster: u32) -> usize {
        ((cluster * 4) % 512) as usize
    }

    /// Read the FAT value for `cluster`. Returns 0 on read failure.
    fn get_fat_entry(&self, device: &dyn BlockDevice, cluster: u32) -> u32 {
        let mut sector = [0u8; 512];
        if !device.read_sectors(self.fat_lba(cluster), 1, sector.as_mut_ptr() as u64) {
            return 0;
        }
        let off = Self::fat_entry_byte_offset(cluster);
        u32::from_le_bytes([sector[off], sector[off+1], sector[off+2], sector[off+3]])
            & 0x0FFF_FFFF
    }

    /// Write `value` into the FAT entry for `cluster` (both FAT copies).
    fn set_fat_entry(&self, device: &dyn BlockDevice, cluster: u32, value: u32) {
        let lba = self.fat_lba(cluster);
        let mut sector = [0u8; 512];
        if !device.read_sectors(lba, 1, sector.as_mut_ptr() as u64) { return; }

        let off = Self::fat_entry_byte_offset(cluster);
        // Preserve the top 4 bits as required by the FAT32 spec.
        let existing_hi = u32::from_le_bytes([sector[off], sector[off+1],
                                               sector[off+2], sector[off+3]])
                          & 0xF000_0000;
        let new_val = (value & 0x0FFF_FFFF) | existing_hi;
        let bytes = new_val.to_le_bytes();
        sector[off..off+4].copy_from_slice(&bytes);

        device.write_sectors(lba, 1, sector.as_ptr() as u64);

        // Mirror write to FAT2 if present.
        if self.bpb.fat_count > 1 {
            let fat2_lba = lba + self.bpb.sectors_per_fat_32 as u64;
            device.write_sectors(fat2_lba, 1, sector.as_ptr() as u64);
        }
    }

    // ---- Cluster allocation -----------------------------------------

    /// Scan the FAT for a free cluster (value == 0), starting from
    /// `search_from`. Returns None if the volume is full.
    fn find_free_cluster(&self, device: &dyn BlockDevice, search_from: u32) -> Option<u32> {
        let total = self.bpb.total_sectors_32 / (self.bpb.sectors_per_cluster as u32);
        // Clusters 0 and 1 are reserved; data starts at 2.
        let start = search_from.max(2);
        for cluster in start..total {
            if self.get_fat_entry(device, cluster) == 0 {
                return Some(cluster);
            }
        }
        None
    }

    /// Free the entire cluster chain starting at `first_cluster`.
    fn free_cluster_chain(&self, device: &dyn BlockDevice, first_cluster: u32) {
        let mut cluster = first_cluster;
        while cluster >= 2 && cluster < 0x0FFF_FFF8 {
            let next = self.get_fat_entry(device, cluster);
            self.set_fat_entry(device, cluster, 0); // Mark free.
            cluster = next;
        }
    }

    /// Allocate a chain of clusters large enough for `data`, write the
    /// data into them, and return the first cluster of the chain.
    /// Returns None if there is not enough free space.
    fn allocate_and_write_chain(
        &self, device: &dyn BlockDevice, data: &[u8],
    ) -> Option<u32> {
        if data.is_empty() { return Some(0); }

        let spc  = self.bpb.sectors_per_cluster as usize;
        let bpc  = spc * 512;
        let needed = (data.len() + bpc - 1) / bpc;

        // Find all needed free clusters up front.
        let mut clusters: Vec<u32> = Vec::with_capacity(needed);
        let mut search = 2u32;
        for _ in 0..needed {
            let c = self.find_free_cluster(device, search)?;
            clusters.push(c);
            search = c + 1;
        }

        // Write data and link the chain.
        let mut buf = alloc::vec![0u8; bpc];
        for (i, &cluster) in clusters.iter().enumerate() {
            let data_start = i * bpc;
            let data_end   = (data_start + bpc).min(data.len());
            let chunk      = &data[data_start..data_end];

            // Build a zero-padded cluster-sized buffer.
            buf.fill(0);
            buf[..chunk.len()].copy_from_slice(chunk);

            let lba = self.cluster_to_lba(cluster);
            device.write_sectors(lba, spc as u32, buf.as_ptr() as u64);

            // Link: point this entry to the next cluster (or EOC).
            let fat_val = if i + 1 < clusters.len() { clusters[i+1] } else { 0x0FFF_FFFF };
            self.set_fat_entry(device, cluster, fat_val);
        }

        Some(clusters[0])
    }

    // ---- Directory entry I/O ----------------------------------------

    /// Convert a filename like "SCHEDPRO.BIN" into a FAT 8.3 name array.
    /// Returns Err(WriteError) if the name exceeds 8.3 limits.
    fn to_fat83_name(filename: &str) -> Result<[u8; 11], crate::vfs::VfsError> {
        use crate::vfs::VfsError;
        let mut result = [b' '; 11];
        match filename.rfind('.') {
            Some(dot) => {
                let base = &filename[..dot];
                let ext  = &filename[dot+1..];
                if base.len() > 8 || ext.len() > 3 {
                    return Err(VfsError::WriteError);
                }
                for (i, b) in base.bytes().enumerate() { result[i]   = b.to_ascii_uppercase(); }
                for (i, b) in ext.bytes().enumerate()  { result[8+i] = b.to_ascii_uppercase(); }
            }
            None => {
                if filename.len() > 8 { return Err(VfsError::WriteError); }
                for (i, b) in filename.bytes().enumerate() { result[i] = b.to_ascii_uppercase(); }
            }
        }
        Ok(result)
    }

    /// Scan `dir_cluster` for a directory entry matching `name83` OR the
    /// first free slot. Returns (sector_lba, byte_offset_in_sector, found_existing).
    fn find_dir_entry_slot(
        &self,
        device: &dyn BlockDevice,
        dir_cluster: u32,
        name83: &[u8; 11],
    ) -> Result<(u64, usize, bool), crate::vfs::VfsError> {
        use crate::vfs::VfsError;
        let spc = self.bpb.sectors_per_cluster as u64;
        let mut cluster    = dir_cluster;
        let mut free_slot: Option<(u64, usize)> = None;

        while cluster >= 2 && cluster < 0x0FFF_FFF8 {
            let base_lba = self.cluster_to_lba(cluster);
            for s in 0..spc {
                let lba = base_lba + s;
                let mut sector = [0u8; 512];
                if !device.read_sectors(lba, 1, sector.as_mut_ptr() as u64) {
                    return Err(VfsError::ReadError);
                }
                // Each directory entry is 32 bytes; 16 entries per 512-byte sector.
                for e in 0..16usize {
                    let off   = e * 32;
                    let first = sector[off];

                    if first == 0x00 {
                        // End-of-directory marker: all remaining entries are free.
                        if free_slot.is_none() { free_slot = Some((lba, off)); }
                        return free_slot
                            .map(|(l, o)| (l, o, false))
                            .ok_or(VfsError::GenericError);
                    }
                    if first == 0xE5 {
                        // Deleted entry — usable as a free slot.
                        if free_slot.is_none() { free_slot = Some((lba, off)); }
                        continue;
                    }
                    if sector[off + 11] & 0x08 != 0 { continue; } // Volume label — skip.
                    if sector[off + 11] & 0x10 != 0 { continue; } // Sub-directory — skip.

                    // Name match?
                    if &sector[off..off+11] == name83.as_ref() {
                        return Ok((lba, off, true)); // Found existing file.
                    }
                }
            }
            cluster = self.get_next_cluster(device, cluster);
        }

        // Exhausted all directory clusters — use the free slot we found, if any.
        free_slot
            .map(|(l, o)| (l, o, false))
            .ok_or(VfsError::GenericError)
    }

    /// Write (create or overwrite) a directory entry in `dir_cluster` for
    /// the file named `name83` pointing to `first_cluster` with `file_size`.
    fn write_dir_entry(
        &self,
        device: &dyn BlockDevice,
        dir_cluster: u32,
        name83: &[u8; 11],
        first_cluster: u32,
        file_size: u32,
    ) -> Result<(), crate::vfs::VfsError> {
        use crate::vfs::VfsError;
        let (lba, off, _existing) =
            self.find_dir_entry_slot(device, dir_cluster, name83)?;

        let mut sector = [0u8; 512];
        if !device.read_sectors(lba, 1, sector.as_mut_ptr() as u64) {
            return Err(VfsError::WriteError);
        }

        // Zero the 32-byte slot, then fill in the fields.
        for b in &mut sector[off..off + 32] { *b = 0; }

        // Name (8+3 bytes, space-padded).
        sector[off..off + 11].copy_from_slice(name83);
        // Attribute: ARCHIVE (0x20).
        sector[off + 11] = 0x20;
        // Cluster high word (bytes 20-21).
        let cluster_hi = ((first_cluster >> 16) & 0xFFFF) as u16;
        sector[off + 20] = (cluster_hi & 0xFF) as u8;
        sector[off + 21] = ((cluster_hi >> 8) & 0xFF) as u8;
        // Cluster low word (bytes 26-27).
        let cluster_lo = (first_cluster & 0xFFFF) as u16;
        sector[off + 26] = (cluster_lo & 0xFF) as u8;
        sector[off + 27] = ((cluster_lo >> 8) & 0xFF) as u8;
        // File size (bytes 28-31, little-endian).
        let size_bytes = file_size.to_le_bytes();
        sector[off + 28..off + 32].copy_from_slice(&size_bytes);

        if !device.write_sectors(lba, 1, sector.as_ptr() as u64) {
            return Err(VfsError::WriteError);
        }
        Ok(())
    }
}

// =====================================================================
// -----| FileSystem trait implementation |-----
// =====================================================================

impl FileSystem for Fat32FileSystem {
    fn list_dir(&self, device: &dyn BlockDevice, path: &str) -> Result<Vec<DirEntry>, VfsError> {
        let (cluster, _, is_dir) = self.resolve_path(device, path)?;
        if !is_dir {
            return Err(VfsError::NotADirectory);
        }
        self.get_entries(device, cluster)
    }

    fn read_file(&self, device: &dyn BlockDevice, path: &str) -> Result<Vec<u8>, VfsError> {
        let (cluster, size, is_dir) = self.resolve_path(device, path)?;
        if is_dir {
            return Err(VfsError::IsDirectory);
        }

        if size == 0 {
            return Ok(Vec::new());
        }

        let mut data = Vec::with_capacity(size as usize);
        let mut current_cluster = cluster;
        let sectors_per_cluster = self.bpb.sectors_per_cluster as u32;
        let bytes_per_cluster = sectors_per_cluster * 512;
        let mut buffer = alloc::vec![0u8; bytes_per_cluster as usize];
        let mut loop_count = 0u32;

        while current_cluster != 0 && current_cluster < 0x0FFF_FFF8 {
            loop_count += 1;
            if loop_count > 65536 {
                crate::serial_println!("[FAT32] read_file: cluster chain too long — aborting (corruption?)");
                break;
            }

            let lba = self.cluster_to_lba(current_cluster);

            if device.read_sectors(lba, sectors_per_cluster, buffer.as_mut_ptr() as u64) {
                let remaining = size as usize - data.len();
                let to_copy = core::cmp::min(remaining, bytes_per_cluster as usize);
                data.extend_from_slice(&buffer[..to_copy]);

                if data.len() >= size as usize {
                    break;
                }
            } else {
                return Err(VfsError::ReadError);
            }

            current_cluster = self.get_next_cluster(device, current_cluster);
        }

        Ok(data)
    }

    /// Create or overwrite a root-level file.
    ///
    /// Only supports 8.3 filenames in the root directory. Subdirectory
    /// creation is not yet implemented — the scheduler profile path
    /// /SCHEDPRO.BIN satisfies this constraint.
    fn write_file(
        &self, device: &dyn BlockDevice, path: &str, data: &[u8],
    ) -> Result<(), crate::vfs::VfsError> {
        use crate::vfs::VfsError;

        // Only root-level files for now (single path component after /).
        let components: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
        if components.len() != 1 {
            return Err(VfsError::WriteError);
        }

        let name83      = Self::to_fat83_name(components[0])?;
        let root_cluster = self.bpb.root_cluster;

        // If the file already exists, free its old cluster chain.
        if let Ok((lba, off, true)) =
            self.find_dir_entry_slot(device, root_cluster, &name83)
        {
            let mut sector = [0u8; 512];
            if device.read_sectors(lba, 1, sector.as_mut_ptr() as u64) {
                let cluster_lo = u16::from_le_bytes([sector[off + 26], sector[off + 27]]) as u32;
                let cluster_hi = u16::from_le_bytes([sector[off + 20], sector[off + 21]]) as u32;
                let old_first  = cluster_lo | (cluster_hi << 16);
                if old_first >= 2 {
                    self.free_cluster_chain(device, old_first);
                }
            }
        }

        // Allocate and write the new cluster chain.
        let first_cluster = self
            .allocate_and_write_chain(device, data)
            .ok_or(VfsError::WriteError)?;

        // Write (or create) the directory entry.
        self.write_dir_entry(device, root_cluster, &name83, first_cluster, data.len() as u32)
    }

    /// Delete a file.
    /// Marks the directory entry as deleted (0xE5) and frees its cluster chain.
    fn delete_file(
        &self, device: &dyn BlockDevice, path: &str,
    ) -> Result<(), crate::vfs::VfsError> {
        use crate::vfs::VfsError;

        // Only root-level files for now.
        let components: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
        if components.len() != 1 {
            return Err(VfsError::FileNotFound);
        }

        let name83 = Self::to_fat83_name(components[0]).map_err(|_| VfsError::FileNotFound)?;
        let root_cluster = self.bpb.root_cluster;

        // Find the existing directory entry.
        if let Ok((lba, off, true)) = self.find_dir_entry_slot(device, root_cluster, &name83) {
            let mut sector = [0u8; 512];
            if device.read_sectors(lba, 1, sector.as_mut_ptr() as u64) {
                // Get the old cluster chain start
                let cluster_lo = u16::from_le_bytes([sector[off + 26], sector[off + 27]]) as u32;
                let cluster_hi = u16::from_le_bytes([sector[off + 20], sector[off + 21]]) as u32;
                let old_first  = cluster_lo | (cluster_hi << 16);
                
                // Mark entry as deleted (0xE5)
                sector[off] = 0xE5;
                if !device.write_sectors(lba, 1, sector.as_ptr() as u64) {
                    return Err(VfsError::WriteError);
                }

                // Free the cluster chain
                if old_first >= 2 {
                    self.free_cluster_chain(device, old_first);
                }
                
                return Ok(());
            } else {
                return Err(VfsError::ReadError);
            }
        }

        Err(VfsError::FileNotFound)
    }
} // impl FileSystem for Fat32FileSystem

// -----| bootstrapping |-----

pub fn probe_fat32(
    port: &mut HbaPort, 
    frame_allocator: &mut impl FrameAllocator<Size4KiB>,
    physical_memory_offset: VirtAddr,
    partition_lba: u64,
) {
    println!("\nProbing for FAT File System on Port 0...");

    let frame = frame_allocator.allocate_frame().expect("No frame for FAT probe");
    let buffer_phys = frame.start_address().as_u64();
    let buffer_virt = physical_memory_offset.as_u64() + buffer_phys;

    if port.read_sectors(partition_lba, 1, buffer_phys, physical_memory_offset) {
        let bpb = unsafe { &*(buffer_virt as *const Fat32BPB) };

        if bpb.boot_signature == 0xAA55 {
            let raw_bytes = unsafe { core::slice::from_raw_parts(buffer_virt as *const u8, 512) };

            // FAT32 structurally requires the 16-bit FAT size to be 0
            // AND the root entry count to be 0.
            let root_count = unsafe { core::ptr::read_unaligned(raw_bytes.as_ptr().add(17) as *const u16) };
            let spf16 = unsafe { core::ptr::read_unaligned(raw_bytes.as_ptr().add(22) as *const u16) };
            
            if root_count != 0 || spf16 != 0 || bpb.sectors_per_fat_32 == 0 {
                return; // Not a FAT32 volume
            }

            println!("  -> FAT32 File System Detected!");
            
            let fs = Fat32FileSystem { bpb: *bpb };
            let device = crate::ahci::AhciBlockDevice {
                port: port as *mut HbaPort,
                physical_memory_offset,
                bounce_buffer_phys: buffer_phys,
                partition_lba,
            };

            // Mount to VFS!
            use alloc::boxed::Box;
            let mut vfs = crate::vfs::VFS.lock();
            
            vfs.mount(
                alloc::string::String::from("/"), 
                Box::new(fs), 
                Box::new(device)
            );
            
            println!("  -> Mounted FAT32 to root (/)");

            // Quick Verify
            if let Ok(content) = vfs.read_file("/HELLO.TXT") {
                let content_str = core::str::from_utf8(&content).unwrap_or("[Binary]");
                println!("     - /HELLO.TXT: \"{}\"", content_str.trim_matches(char::from(0)));
            } else {
                println!("     - /HELLO.TXT: not found (list_dir may still work)");
            }

            // Quick verify directory listing
            match vfs.list_dir("/") {
                Ok(entries) => {
                    println!("     - Root dir: {} entries", entries.len());
                    for e in &entries {
                        println!("       {} {}", 
                            if e.file_type == crate::vfs::FileType::Directory { "D" } else { "F" },
                            e.name
                        );
                    }
                }
                Err(e) => println!("     - Root dir listing FAILED: {:?}", e),
            }
        } else {
            println!("  -> No valid boot signature (0xAA55) found.");
        }
    } else {
        println!("  -> Failed to read sector 0.");
    }
}
