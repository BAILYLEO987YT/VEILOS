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
    // Mounted volumes can't be detached yet, so the controller can't be released.
    remove: None,
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
    take_ownership(hba_mem);
    reinit_ports(hba_mem, crate::memory::get_phys_mem_offset());
    Ok(())
}

// Capability bits (CAP, CAP2, GHC, BOHC) and port registers used below.
const CAP_S64A: u32 = 1 << 31; // 64-bit DMA addresses
const CAP_SSS: u32 = 1 << 27;  // staggered spin-up: ports start powered down
const CAP2_BOH: u32 = 1 << 0;  // BIOS/OS handoff
const GHC_AE: u32 = 1 << 31;   // AHCI enable
const BOHC_BOS: u32 = 1 << 0;  // BIOS owns the controller
const BOHC_OOS: u32 = 1 << 1;  // OS wants it
const BOHC_BB: u32 = 1 << 4;   // BIOS busy
const PXCMD_SUD: u32 = 1 << 1; // spin up device
const PXCMD_ICC_ACTIVE: u32 = 1 << 28;
const SIG_ATA: u32 = 0x0000_0101;

/// Before touching the ports, as on a real PC: AHCI mode on (the controller
/// may also speak legacy IDE), and the controller taken from the firmware
/// (BIOS/OS handoff: firmware with SMM SATA code keeps using it until
/// asked). QEMU needs neither; real chipsets can. All DMA memory comes from
/// below 4 GiB (memory::DMA_LIMIT), so controllers without 64-bit
/// addressing are fine.
fn take_ownership(hba: &mut HbaMemory) {
    let cap = hba.capability.read();
    let ghc = hba.global_host_control.read();
    if ghc & GHC_AE == 0 {
        hba.global_host_control.write(ghc | GHC_AE);
    }
    if hba.capability2.read() & CAP2_BOH != 0 {
        let bohc = hba.bohc.read();
        if bohc & BOHC_BOS != 0 {
            hba.bohc.write(bohc | BOHC_OOS);
            // The firmware gets 25 ms to let go, 2 s more if it says it's busy.
            let mut released = crate::time::spin_until(25_000, || hba.bohc.read() & BOHC_BOS == 0);
            if !released && hba.bohc.read() & BOHC_BB != 0 {
                released = crate::time::spin_until(2_000_000, || hba.bohc.read() & BOHC_BOS == 0);
            }
            crate::serial_println!("[AHCI] BIOS/OS handoff: {}", if released { "the firmware let go" } else { "WARN: the firmware kept it" });
        }
    }
    crate::serial_println!(
        "[AHCI] CAP {:#010x}: {} command slots, {} ports, 64-bit DMA {}, staggered spin-up {}",
        cap, ((cap >> 8) & 0x1F) + 1, (cap & 0x1F) + 1,
        cap & CAP_S64A != 0, cap & CAP_SSS != 0
    );
}

/// Bring a port's link up if it isn't: spin the device up (staggered
/// spin-up), then a COMRESET if a device is there but the link isn't
/// established. Returns DET (3: device present and link up).
fn bring_up_link(port: &mut HbaPort, sss: bool) -> u32 {
    let det = |p: &HbaPort| p.sata_status.read() & 0x0F;
    if det(port) == 3 {
        return 3;
    }
    if sss && port.command_and_status.read() & PXCMD_SUD == 0 {
        let cmd = port.command_and_status.read();
        port.command_and_status.write(cmd | PXCMD_SUD);
        crate::time::spin_until(10_000, || det(port) == 3);
    }
    if det(port) == 1 || (sss && det(port) != 3) {
        // COMRESET: DET=1 in SControl for at least 1 ms, then back to 0.
        let sctl = port.sata_control.read() & !0x0F;
        port.sata_control.write(sctl | 1);
        crate::time::delay_ms(2);
        port.sata_control.write(sctl);
        // A device shows up within a few ms; then the link takes up to 1 s.
        if crate::time::spin_until(50_000, || det(port) != 0) {
            crate::time::spin_until(1_000_000, || det(port) == 3);
        }
        port.sata_error.write(0xFFFF_FFFF);
    }
    det(port)
}

fn reinit_ports(
    hba_mem: &mut HbaMemory, 
    physical_memory_offset: VirtAddr,
) {
    let ports_implemented = hba_mem.ports_implemented.read();
    let sss = hba_mem.capability.read() & CAP_SSS != 0;
    for i in 0..32 {
        if (ports_implemented & (1 << i)) != 0 {
            let port = &mut hba_mem.ports[i];
            if bring_up_link(port, sss) != 3 {
                continue;
            }
            // Asleep (partial or slumber) after the firmware: wake the link.
            if (port.sata_status.read() >> 8) & 0x0F != 1 {
                let cmd = port.command_and_status.read() & !(0xF << 28);
                port.command_and_status.write(cmd | PXCMD_ICC_ACTIVE);
                crate::time::spin_until(10_000, || (port.sata_status.read() >> 8) & 0x0F == 1);
            }
            // Only disks: an optical drive (ATAPI), port multiplier or
            // enclosure answers IDENTIFY DEVICE with an error.
            let sig = port.signature.read();
            if sig != SIG_ATA && sig != 0xFFFF_FFFF {
                crate::serial_println!("[AHCI] Port {}: signature {:#010x} is not a disk; skipped", i, sig);
                continue;
            }
            setup_port(port, i, physical_memory_offset);
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
    // The device may still be busy from power-on or the COMRESET (BSY, DRQ
    // in the task file): a command issued now would be lost.
    if !crate::time::spin_until(2_000_000, || port.task_file_data.read() & 0x88 == 0) {
        crate::serial_println!("[AHCI] Port {}: device still busy (TFD {:#x}); trying anyway", port_num, port.task_file_data.read());
    }
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
        match crate::block::AhciTransport::new(port_static) {
            Some(t) => crate::block::mount_disk(crate::block::Disk::new(
                alloc::boxed::Box::new(t),
                alloc::format!("d{}", port_num),
                sectors,
            )),
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
    fn wait_for_command(&mut self, slot: usize, what: &str) -> bool {
        let mut fatal = false;
        let done = crate::time::spin_until(AHCI_COMMAND_TIMEOUT_US, || {
            if self.command_issue.read() & (1 << slot) == 0 { return true; }
            fatal = self.interrupt_status.read() & (1 << 30) != 0;
            fatal
        });
        if fatal {
            let tfd = self.task_file_data.read();
            crate::serial_println!(
                "[AHCI] {}: device reported an error (status {:#04x}, error {:#04x})",
                what, tfd & 0xFF, (tfd >> 8) & 0xFF
            );
            self.recover();
            false
        } else if !done {
            crate::serial_println!("[AHCI] {}: command timed out", what);
            self.recover();
            false
        } else {
            true
        }
    }

    /// Put the port back in a usable state after a failed command (AHCI 1.3
    /// §6.2.2.1): stop the command engine, clear the error and interrupt
    /// status, restart. Without this the stale error status made every later
    /// command fail too, so one refused write left the whole disk unreadable.
    fn recover(&mut self) {
        let _ = self.stop();
        self.sata_error.write(0xFFFF_FFFF);
        self.interrupt_status.write(0xFFFF_FFFF);
        if self.task_file_data.read() & (0x80 | 0x08) != 0 {
            // BSY/DRQ still set: only a COMRESET clears that, not done yet.
            crate::serial_println!("[AHCI] port still busy after error; needs a COMRESET (not implemented)");
        }
        let _ = self.start();
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

        // PxIS bits are sticky (write 1 to clear): clear any left from an
        // earlier command so this one's completion check reads its own status.
        self.interrupt_status.write(0xFFFF_FFFF);
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

        // PxIS bits are sticky (write 1 to clear): clear any left from an
        // earlier command so this one's completion check reads its own status.
        self.interrupt_status.write(0xFFFF_FFFF);
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

        // PxIS bits are sticky (write 1 to clear): clear any left from an
        // earlier command so this one's completion check reads its own status.
        self.interrupt_status.write(0xFFFF_FFFF);
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
