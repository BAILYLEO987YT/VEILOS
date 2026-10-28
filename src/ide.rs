use x86_64::instructions::port::Port;
use crate::println;
use alloc::vec::Vec;
use crate::pci::PciDevice;

// -----| Legacy IDE Ports |-----
// Primary Channel
const PRIMARY_DATA: u16 = 0x1F0;
const PRIMARY_LBA_LO: u16 = 0x1F3;
const PRIMARY_LBA_MID: u16 = 0x1F4;
const PRIMARY_LBA_HI: u16 = 0x1F5;
const PRIMARY_DRIVE_SEL: u16 = 0x1F6;
const PRIMARY_COMMAND: u16 = 0x1F7;   // Write
const PRIMARY_STATUS: u16 = 0x1F7;    // Read

// Status Register Bits
const STATUS_BSY: u8 = 0x80; // Busy
const STATUS_DRDY: u8 = 0x40; // Drive Ready
const STATUS_ERR: u8 = 0x01; // Error

pub fn init(devices: &Vec<PciDevice>) {
    println!("\nSearching for Legacy IDE/ATA Controllers...");

    let mut found = false;
    for dev in devices.iter() {
        // Class 0x01 (Storage), Subclass 0x01 (IDE)
        if dev.class == 0x01 && dev.subclass == 0x01 {
            found = true;
            println!(
                "  -> Legacy IDE Controller found at {}:{}:{}",
                dev.bus, dev.device, dev.function
            );
            
            // Try to identify the drive on the Primary Master
            identify_primary_master();
        }
    }

    if !found {
        // Many systems don't have an IDE PCI entry but STILL have the hardware ports 
        // at 0x1F0. We check for a "ghost" device by reading the status port.
        let mut status_port = Port::<u8>::new(PRIMARY_STATUS);
        let status = unsafe { status_port.read() };
        
        if status != 0xFF {
            println!("  -> No PCI IDE entry, but legacy I/O ports detected at 0x1F0.");
            identify_primary_master();
        } else {
            println!("  -> No Legacy IDE Hardware detected.");
        }
    }
}

fn identify_primary_master() {
    let mut drive_sel = Port::<u8>::new(PRIMARY_DRIVE_SEL);
    let mut command_port = Port::<u8>::new(PRIMARY_COMMAND);
    let mut status_port = Port::<u8>::new(PRIMARY_STATUS);
    let mut data_port = Port::<u16>::new(PRIMARY_DATA);
    let mut lba_lo = Port::<u8>::new(PRIMARY_LBA_LO);
    let mut lba_mid = Port::<u8>::new(PRIMARY_LBA_MID);
    let mut lba_hi = Port::<u8>::new(PRIMARY_LBA_HI);

    unsafe {
        // Select Primary Master (0xA0)
        drive_sel.write(0xA0);
        
        // Zero out LBA and Sector Count
        lba_lo.write(0);
        lba_mid.write(0);
        lba_hi.write(0);
        
        // Send IDENTIFY (0xEC)
        command_port.write(0xEC);

        // Check if drive exists
        let status = status_port.read();
        if status == 0 {
            println!("     - Primary Master slot is empty.");
            return;
        }

        // Wait for BSY to clear
        while (status_port.read() & STATUS_BSY) != 0 {}

        // Check for ATAPI (Not a hard drive)
        let mid = lba_mid.read();
        let hi = lba_hi.read();
        if mid != 0 || hi != 0 {
            println!("     - ATAPI device detected (Not LBA compatible).");
            return;
        }

        // Wait until DRDY or ERR
        loop {
            let s = status_port.read();
            if (s & STATUS_ERR) != 0 {
                println!("     - IDE Error during identification.");
                return;
            }
            if (s & STATUS_DRDY) != 0 {
                break;
            }
        }

        // Read 256 words (512 bytes) of data
        println!("     - Drive identified! Reading capacity...");
        let mut identify_data = [0u16; 256];
        for i in 0..256 {
            identify_data[i] = data_port.read();
        }

        // LBA28 total sectors at offset 60-61
        let sectors = (identify_data[60] as u32) | ((identify_data[61] as u32) << 16);
        let size_mb = (sectors as u64 * 512) / (1024 * 1024);
        
        println!("     - LBA28 Capacity: {} MB ({} sectors)", size_mb, sectors);
    }
}
