use crate::pci::PciDevice;
use crate::println;
use volatile::Volatile;
use x86_64::{
    structures::paging::{
        Mapper, Page, PageTableFlags, PhysFrame, Size4KiB,
    },
    PhysAddr, VirtAddr,
};

/// AHCI SATA controllers (class 01:06, prog-if 01). Bound by `hal::bind`.
pub const DRIVER: crate::hal::Driver = crate::hal::Driver {
    name: "ahci",
    class: crate::hal::DeviceClass::Storage,
    matches: is_ahci,
    probe,
};

fn is_ahci(dev: &PciDevice) -> bool {
    dev.class == 0x01 && dev.subclass == 0x06 && dev.prog_if == 0x01
}

/// Map the controller's registers, start every connected port, and hand
/// each disk to the block layer.
fn probe(dev: &PciDevice) -> Result<(), alloc::string::String> {
    println!("
Initializing AHCI controller at {}:{}:{}...", dev.bus, dev.device, dev.function);
    // ABAR is always BAR5 and always memory-mapped.
    let base_address = dev.bar(5).mem_base().ok_or_else(|| alloc::string::String::from("no ABAR (BAR5)"))?;

    // The HBA DMAs command lists, FISes and data: it needs memory
    // decode and bus mastering, which firmware may not have set.
    dev.enable_for_driver();

    let phys_addr = PhysAddr::new(base_address);
    let virt_addr = VirtAddr::new(base_address as u64);

    // Map enough pages to cover the entire HbaMemory struct.
    // One page (4 KiB) is not enough on controllers with many ports
    // implemented — HbaMemory can be up to ~8 KiB with 32 ports.
    const HBA_SIZE: u64 = core::mem::size_of::<HbaMemory>() as u64;
    const PAGE_SIZE: u64 = 4096;
    let pages_needed = (HBA_SIZE + PAGE_SIZE - 1) / PAGE_SIZE;

    let flags = PageTableFlags::PRESENT
        | PageTableFlags::WRITABLE
        | PageTableFlags::NO_CACHE;

    let mut mapper = unsafe { crate::memory::get_mapper() };
    let mut frame_allocator = crate::memory::GlobalFrameAllocator;
    for i in 0..pages_needed {
        let phys_page = PhysAddr::new(phys_addr.as_u64() + i * PAGE_SIZE);
        let virt_page = VirtAddr::new(virt_addr.as_u64() + i * PAGE_SIZE);
        let frame = PhysFrame::<Size4KiB>::containing_address(phys_page);
        let page  = Page::<Size4KiB>::containing_address(virt_page);
        unsafe {
            match mapper.map_to(page, frame, flags, &mut frame_allocator) {
                Ok(tlb) => tlb.flush(),
                Err(_)  => {} // already mapped (page 0 may overlap another mapping)
            }
        }
    }
    println!("  -> ABAR mapped at {:#010X}", virt_addr.as_u64());

    let hba_mem = unsafe { &mut *(virt_addr.as_mut_ptr::<HbaMemory>()) };
    reinit_ports(hba_mem, crate::memory::get_phys_mem_offset());
    Ok(())
}

fn reinit_ports(
    hba_mem: &mut HbaMemory, 
    physical_memory_offset: VirtAddr,
) {
    let ports_implemented = hba_mem.ports_implemented.read();
    for i in 0..32 {
        if (ports_implemented & (1 << i)) != 0 {
            let port = &mut hba_mem.ports[i];
            let status = port.sata_status.read();
            let det = status & 0x0F;
            let ipm = (status >> 8) & 0x0F;

            if det == 0x03 && ipm == 0x01 {
                setup_port(port, i, physical_memory_offset);
            }
        }
    }
}

fn setup_port(
    port: &mut HbaPort, 
    port_num: usize, 
    physical_memory_offset: VirtAddr,
) {
    if !port.stop() {
        crate::serial_println!("[AHCI] Port {}: stop() failed — skipping port setup.", port_num);
        return;
    }

    // One contiguous DMA block per port, below 4 GiB:
    //   page 0       command list (32 headers x 32 B = 1 KiB, 1 KiB aligned)
    //                + received-FIS area at +0x400 (256 B, 256 B aligned)
    //   pages 1-2    32 command tables x 256 B = 8 KiB
    // The tables used to get a single 4 KiB frame, so slots 16-31 pointed
    // into whatever frame happened to follow it.
    let Some(dma) = crate::memory::DmaRegion::new(3 * 4096) else {
        crate::serial_println!("[AHCI] Port {}: no contiguous DMA memory — skipping port setup.", port_num);
        return;
    };
    let (dma_phys, _) = dma.leak(); // the port uses it for the kernel's lifetime
    let cl_base  = dma_phys;
    let fis_base = dma_phys + 0x400;
    let ct_base  = dma_phys + 4096;

    port.command_list_base.write(cl_base as u32);
    port.command_list_base_upper.write((cl_base >> 32) as u32);
    port.fis_base.write(fis_base as u32);
    port.fis_base_upper.write((fis_base >> 32) as u32);

    let cl_virt = physical_memory_offset.as_u64() + cl_base;
    let headers = unsafe { core::slice::from_raw_parts_mut(cl_virt as *mut CommandHeader, 32) };
    for (i, header) in headers.iter_mut().enumerate() {
        let table_addr = ct_base + (i as u64 * 256);
        header.command_table_base = table_addr as u32;
        header.command_table_base_upper = (table_addr >> 32) as u32;
        header.prdt_length = 8;
    }

    port.interrupt_status.write(0xFFFF_FFFF);
    port.sata_error.write(0xFFFF_FFFF);
    if !port.start() {
        crate::serial_println!("[AHCI] Port {}: start() failed — skipping port setup.", port_num);
        return;
    }

    println!("  -> Port {}: Initialized and Running.", port_num);
    
    let Some(identify_buffer) = crate::memory::DmaRegion::new(512) else {
        crate::serial_println!("[AHCI] Port {}: no DMA memory for IDENTIFY.", port_num);
        return;
    };
    let buffer_phys = identify_buffer.phys();
    let buffer_virt = identify_buffer.as_mut_ptr::<u8>() as u64;
    
    if port.identify(buffer_phys, physical_memory_offset) {
        let raw_data = unsafe { core::slice::from_raw_parts(buffer_virt as *const u16, 256) };
        let sectors = (raw_data[100] as u64) | ((raw_data[101] as u64) << 16) | ((raw_data[102] as u64) << 32) | ((raw_data[103] as u64) << 48);
        println!("     - Total Sectors: {}", sectors);
        
        let mut model = [0u8; 40];
        for i in 0..20 {
            model[i*2] = (raw_data[27+i] >> 8) as u8;
            model[i*2+1] = (raw_data[27+i] & 0xFF) as u8;
        }
        let model_str = core::str::from_utf8(&model).unwrap_or("Unknown Model");
        println!("     - Model: {}", model_str.trim());

        // Hand the port to the block layer: one shared, locked device per
        // disk. It reads the partition table and mounts every filesystem it
        // recognises (block::mount_disk). This used to read only MBR entry 1
        // and stop probing once anything was mounted, so a second partition
        // or disk was never seen.
        let port_static: &'static mut HbaPort = unsafe { &mut *(port as *mut HbaPort) };
        match crate::block::Disk::new(port_static, port_num, sectors) {
            Some(disk) => crate::block::mount_disk(disk),
            None => crate::serial_println!("[AHCI] Port {}: no DMA memory for the disk buffer.", port_num),
        }
    }
}

// -----| AHCI Register Structures |-----

#[repr(C, packed)]
pub struct CommandHeader {
    pub command_fis_length: u8,
    pub flags: u8,
    pub prdt_length: u16,
    pub prdb_count: u32,
    pub command_table_base: u32,
    pub command_table_base_upper: u32,
    pub reserved: [u32; 4],
}

#[repr(C, packed)]
pub struct PrdtEntry {
    pub data_base_address: u32,
    pub data_base_address_upper: u32,
    pub reserved0: u32,
    pub byte_count: u32,
}

#[repr(C, packed)]
pub struct CommandTable {
    pub command_fis: [u8; 64],
    pub atapi_command: [u8; 16],
    pub reserved: [u8; 48],
    pub prdt_entries: [PrdtEntry; 8],
}

#[repr(C)]
pub struct HbaPort {
    pub command_list_base: Volatile<u32>,
    pub command_list_base_upper: Volatile<u32>,
    pub fis_base: Volatile<u32>,
    pub fis_base_upper: Volatile<u32>,
    pub interrupt_status: Volatile<u32>,
    pub interrupt_enable: Volatile<u32>,
    pub command_and_status: Volatile<u32>,
    pub reserved0: Volatile<u32>,
    pub task_file_data: Volatile<u32>,
    pub signature: Volatile<u32>,
    pub sata_status: Volatile<u32>,
    pub sata_control: Volatile<u32>,
    pub sata_error: Volatile<u32>,
    pub sata_active: Volatile<u32>,
    pub command_issue: Volatile<u32>,
    pub sata_notification: Volatile<u32>,
    pub fis_switch_control: Volatile<u32>,
    pub reserved1: [Volatile<u32>; 11],
    pub vendor: [Volatile<u32>; 4],
}

/// Engine start/stop: the AHCI spec allows CR/FR up to 500 ms to clear.
const AHCI_ENGINE_TIMEOUT_US: u64 = 500_000;
/// BSY/DRQ must clear before a command is issued.
const AHCI_BUSY_TIMEOUT_US: u64 = 1_000_000;
/// One command, including a drive spin-up on real hardware.
const AHCI_COMMAND_TIMEOUT_US: u64 = 10_000_000;

impl HbaPort {
    /// Wait for the device to clear BSY and DRQ.
    fn wait_not_busy(&self) -> bool {
        let ok = crate::time::spin_until(AHCI_BUSY_TIMEOUT_US, || {
            self.task_file_data.read() & (0x80 | 0x08) == 0
        });
        if !ok {
            crate::serial_println!("[AHCI] device stayed busy (tfd={:#X})", self.task_file_data.read());
        }
        ok
    }

    /// Wait for the command in `slot` to complete. False on a task-file error
    /// (PxIS.TFES) or timeout. An unbounded spin here would hang the whole
    /// kernel on a stalled DMA.
    fn wait_for_command(&self, slot: usize, what: &str) -> bool {
        let mut fatal = false;
        let done = crate::time::spin_until(AHCI_COMMAND_TIMEOUT_US, || {
            if self.command_issue.read() & (1 << slot) == 0 { return true; }
            fatal = self.interrupt_status.read() & (1 << 30) != 0;
            fatal
        });
        if fatal {
            crate::serial_println!("[AHCI] {}: fatal error bit set", what);
            false
        } else if !done {
            crate::serial_println!("[AHCI] {}: command timed out", what);
            false
        } else {
            true
        }
    }
    pub fn stop(&mut self) -> bool {
        let mut command = self.command_and_status.read();
        command &= !(1 << 0); // Clear ST  (Start)
        command &= !(1 << 4); // Clear FRE (FIS Receive Enable)
        self.command_and_status.write(command);
        // Wait for CR (bit 15) and FR (bit 14) to clear — AHCI spec allows up to 500 ms.
        let stopped = crate::time::spin_until(AHCI_ENGINE_TIMEOUT_US, || {
            self.command_and_status.read() & ((1 << 14) | (1 << 15)) == 0
        });
        if !stopped {
            crate::serial_println!("[AHCI] WARN: stop() timed out — CR/FR bits did not clear (status={:#010X})",
                self.command_and_status.read());
        }
        stopped
    }

    pub fn start(&mut self) -> bool {
        // Wait for CR (bit 15) to clear before enabling FRE/ST.
        if !crate::time::spin_until(AHCI_ENGINE_TIMEOUT_US, || self.command_and_status.read() & (1 << 15) == 0) {
            crate::serial_println!("[AHCI] WARN: start() timed out waiting for CR to clear");
            return false;
        }
        let mut command = self.command_and_status.read();
        command |= 1 << 4; // FRE
        command |= 1 << 0; // ST
        self.command_and_status.write(command);
        true
    }

    pub fn find_free_slot(&self) -> Option<usize> {
        let slots = self.sata_active.read() | self.command_issue.read();
        for i in 0..32 {
            if (slots & (1 << i)) == 0 { return Some(i); }
        }
        None
    }

    pub fn identify(&mut self, buffer_phys: u64, physical_memory_offset: VirtAddr) -> bool {
        self.interrupt_status.write(0xFFFF_FFFF);
        let slot = match self.find_free_slot() {
            Some(s) => s,
            None => return false,
        };

        let clb = ((self.command_list_base_upper.read() as u64) << 32) | (self.command_list_base.read() as u64);
        let cl_virt = physical_memory_offset.as_u64() + clb;
        let cmd_header = unsafe { &mut *(cl_virt as *mut CommandHeader).add(slot) };

        cmd_header.command_fis_length = 5; 
        cmd_header.flags = 0;
        cmd_header.prdt_length = 1;
        cmd_header.prdb_count = 0;

        let ctb = ((cmd_header.command_table_base_upper as u64) << 32) | (cmd_header.command_table_base as u64);
        let ct_virt = physical_memory_offset.as_u64() + ctb;
        let cmd_table = unsafe { &mut *(ct_virt as *mut CommandTable) };
        
        for i in 0..64 { cmd_table.command_fis[i] = 0; }
        cmd_table.prdt_entries[0].data_base_address = (buffer_phys & 0xFFFF_FFFF) as u32;
        cmd_table.prdt_entries[0].data_base_address_upper = (buffer_phys >> 32) as u32;
        cmd_table.prdt_entries[0].byte_count = 511 | (1 << 31);

        cmd_table.command_fis[0] = 0x27;
        cmd_table.command_fis[1] = 0x80;
        cmd_table.command_fis[2] = 0xEC;
        
        if !self.wait_not_busy() { return false; }

        self.command_issue.write(1 << slot);
        self.wait_for_command(slot, "identify()")
    }

    pub fn read_sectors(&mut self, lba: u64, count: u32, buffer_phys: u64, physical_memory_offset: VirtAddr) -> bool {
        self.interrupt_status.write(0xFFFF_FFFF);
        let slot = match self.find_free_slot() {
            Some(s) => s,
            None => return false,
        };

        let clb = ((self.command_list_base_upper.read() as u64) << 32) | (self.command_list_base.read() as u64);
        let cl_virt = physical_memory_offset.as_u64() + clb;
        let cmd_header = unsafe { &mut *(cl_virt as *mut CommandHeader).add(slot) };

        cmd_header.command_fis_length = 5;
        cmd_header.flags = 0; 
        cmd_header.prdt_length = 1;
        cmd_header.prdb_count = 0;
        
        let ctb = ((cmd_header.command_table_base_upper as u64) << 32) | (cmd_header.command_table_base as u64);
        let ct_virt = physical_memory_offset.as_u64() + ctb;
        let cmd_table = unsafe { &mut *(ct_virt as *mut CommandTable) };

        cmd_table.prdt_entries[0].data_base_address = (buffer_phys & 0xFFFF_FFFF) as u32;
        cmd_table.prdt_entries[0].data_base_address_upper = (buffer_phys >> 32) as u32;
        cmd_table.prdt_entries[0].byte_count = (count * 512 - 1) | (1 << 31);

        for i in 0..64 { cmd_table.command_fis[i] = 0; }
        cmd_table.command_fis[0] = 0x27;
        cmd_table.command_fis[1] = 0x80;
        cmd_table.command_fis[2] = 0x25;

        cmd_table.command_fis[4] = (lba & 0xFF) as u8;
        cmd_table.command_fis[5] = ((lba >> 8) & 0xFF) as u8;
        cmd_table.command_fis[6] = ((lba >> 16) & 0xFF) as u8;
        cmd_table.command_fis[7] = 0x40;
        cmd_table.command_fis[8] = ((lba >> 24) & 0xFF) as u8;
        cmd_table.command_fis[9] = ((lba >> 32) & 0xFF) as u8;
        cmd_table.command_fis[10] = ((lba >> 40) & 0xFF) as u8;
        cmd_table.command_fis[12] = (count & 0xFF) as u8;
        cmd_table.command_fis[13] = ((count >> 8) & 0xFF) as u8;

        if !self.wait_not_busy() { return false; }

        self.command_issue.write(1 << slot);
        let ok = self.wait_for_command(slot, "read_sectors()");
        if !ok {
            crate::serial_println!("[AHCI]   (lba={} count={})", lba, count);
        }
        ok
    }

    /// Write `count` sectors from the physical buffer at `buffer_phys` to
    /// the disk starting at `lba`.  Uses ATA WRITE DMA EXT (0x35).
    pub fn write_sectors(&mut self, lba: u64, count: u32, buffer_phys: u64, physical_memory_offset: VirtAddr) -> bool {
        self.interrupt_status.write(0xFFFF_FFFF);
        let slot = match self.find_free_slot() {
            Some(s) => s,
            None => return false,
        };

        let clb     = ((self.command_list_base_upper.read() as u64) << 32) | (self.command_list_base.read() as u64);
        let cl_virt = physical_memory_offset.as_u64() + clb;
        let cmd_header = unsafe { &mut *(cl_virt as *mut CommandHeader).add(slot) };

        // Bit 6 of flags = Write direction (host→device).
        cmd_header.command_fis_length = 5;
        cmd_header.flags       = 1 << 6; // Write bit
        cmd_header.prdt_length = 1;
        cmd_header.prdb_count  = 0;

        let ctb     = ((cmd_header.command_table_base_upper as u64) << 32) | (cmd_header.command_table_base as u64);
        let ct_virt = physical_memory_offset.as_u64() + ctb;
        let cmd_table = unsafe { &mut *(ct_virt as *mut CommandTable) };

        // Point PRDT at the caller's physical buffer.
        cmd_table.prdt_entries[0].data_base_address       = (buffer_phys & 0xFFFF_FFFF) as u32;
        cmd_table.prdt_entries[0].data_base_address_upper = (buffer_phys >> 32) as u32;
        cmd_table.prdt_entries[0].byte_count              = (count * 512 - 1) | (1 << 31);

        // Build H2D Register FIS for ATA WRITE DMA EXT (0x35).
        for i in 0..64 { cmd_table.command_fis[i] = 0; }
        cmd_table.command_fis[0]  = 0x27; // FIS type: H2D Register
        cmd_table.command_fis[1]  = 0x80; // Command bit
        cmd_table.command_fis[2]  = 0x35; // ATA WRITE DMA EXT

        // 48-bit LBA address.
        cmd_table.command_fis[4]  = (lba        & 0xFF) as u8;
        cmd_table.command_fis[5]  = ((lba >> 8)  & 0xFF) as u8;
        cmd_table.command_fis[6]  = ((lba >> 16) & 0xFF) as u8;
        cmd_table.command_fis[7]  = 0x40; // LBA mode (bit 6)
        cmd_table.command_fis[8]  = ((lba >> 24) & 0xFF) as u8;
        cmd_table.command_fis[9]  = ((lba >> 32) & 0xFF) as u8;
        cmd_table.command_fis[10] = ((lba >> 40) & 0xFF) as u8;

        // Sector count (16-bit).
        cmd_table.command_fis[12] = (count & 0xFF) as u8;
        cmd_table.command_fis[13] = ((count >> 8) & 0xFF) as u8;

        // Wait for BSY/DRQ to clear before issuing.
        if !self.wait_not_busy() { return false; }

        self.command_issue.write(1 << slot);
        let ok = self.wait_for_command(slot, "write_sectors()");
        if !ok {
            crate::serial_println!("[AHCI]   (lba={} count={})", lba, count);
        }
        ok
    }
}

#[repr(C)]
struct HbaMemory {
    capability: Volatile<u32>,
    global_host_control: Volatile<u32>,
    interrupt_status: Volatile<u32>,
    ports_implemented: Volatile<u32>,
    version: Volatile<u32>,
    ccc_control: Volatile<u32>,
    ccc_ports: Volatile<u32>,
    em_location: Volatile<u32>,
    em_control: Volatile<u32>,
    capability2: Volatile<u32>,
    bohc: Volatile<u32>,
    reserved: [u8; 116],
    vendor: [u8; 96],
    ports: [HbaPort; 32],
}
