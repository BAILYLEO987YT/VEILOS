use alloc::vec::Vec;
use spin::Mutex;
use x86_64::instructions::port::Port;
use crate::println;

const CONFIG_ADDRESS: u16 = 0xCF8;
const CONFIG_DATA: u16 = 0xCFC;

pub struct PciDevice {
    pub bus: u8,
    pub device: u8,
    pub function: u8,
    pub vendor_id: u16,
    pub device_id: u16,
    pub class: u8,
    pub subclass: u8,
    pub prog_if: u8,
    pub header_type: u8,
}

// We use a global lock to strictly prevent Race Conditions when reading or writing 
// to the multiplexed PCI configuration ports. If two threads tried to read PCI devices 
// simultaneously, their bytes would interleave and crash the hardware!
static CONFIG_LOCK: Mutex<()> = Mutex::new(());

/// Reads a 32-bit register from a specific device on the PCI bus.
pub fn read_config_register(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    let address = (1 << 31)
        | ((bus as u32) << 16)
        | ((device as u32) << 11)
        | ((function as u32) << 8)
        | ((offset as u32) & 0xFC);

    let _lock = CONFIG_LOCK.lock();
    let mut address_port = Port::<u32>::new(CONFIG_ADDRESS);
    let mut data_port = Port::<u32>::new(CONFIG_DATA);

    unsafe {
        address_port.write(address);
        data_port.read()
    }
}

pub fn get_vendor_id(bus: u8, device: u8, function: u8) -> u16 {
    let reg = read_config_register(bus, device, function, 0);
    (reg & 0xFFFF) as u16
}

/// Reads one of the 6 Base Address Registers (BARs) from a PCI device.
/// `bar_index` should be between 0 and 5.
pub fn read_bar(bus: u8, device: u8, function: u8, bar_index: u8) -> u32 {
    // BAR0 starts at offset 0x10, and each BAR is 4 bytes.
    let offset = 0x10 + (bar_index * 4);
    read_config_register(bus, device, function, offset)
}

/// Recursively scans every bus, device, and function branch on the motherboard 
/// to construct a complete hardware tree of everything plugged in!
pub fn scan_buses() -> Vec<PciDevice> {
    let mut devices = Vec::new();

    // Check if multiple PCI host controllers exist on the motherboard
    let header_type = (read_config_register(0, 0, 0, 0x0C) >> 16) as u8;

    if (header_type & 0x80) == 0 {
        // Single PCI host controller exists (Standard PC architecture)
        scan_bus(0, &mut devices);
    } else {
        // Multiple PCI host controllers
        for function in 0..8 {
            let vendor_id = get_vendor_id(0, 0, function);
            if vendor_id != 0xFFFF {
                scan_bus(function, &mut devices);
            }
        }
    }

    devices
}

fn scan_bus(bus: u8, devices: &mut Vec<PciDevice>) {
    // A single PCI bus can hold up to 32 hard-wired devices
    for device in 0..32 {
        let vendor_id = get_vendor_id(bus, device, 0);
        // Vendor ID 0xFFFF mathematically means the physical slot is completely empty
        if vendor_id != 0xFFFF {
            scan_device(bus, device, devices);
        }
    }
}

fn scan_device(bus: u8, device: u8, devices: &mut Vec<PciDevice>) {
    let function = 0;

    let reg3 = read_config_register(bus, device, function, 0x0C);
    let header_type = (reg3 >> 16) as u8;

    scan_function(bus, device, function, devices);

    // If it's a multi-function device (like a GPU that also has a Sound Chip in it), 
    // we must scan the rest of its internal functions
    if (header_type & 0x80) != 0 {
        for f in 1..8 {
            let vendor_id = get_vendor_id(bus, device, f);
            if vendor_id != 0xFFFF {
                scan_function(bus, device, f, devices);
            }
        }
    }
}

fn scan_function(bus: u8, device: u8, function: u8, devices: &mut Vec<PciDevice>) {
    let reg0 = read_config_register(bus, device, function, 0);
    let reg2 = read_config_register(bus, device, function, 0x08);
    let reg3 = read_config_register(bus, device, function, 0x0C);

    let vendor_id = (reg0 & 0xFFFF) as u16;
    let device_id = ((reg0 >> 16) & 0xFFFF) as u16;

    let prog_if = ((reg2 >> 8) & 0xFF) as u8;
    let subclass = ((reg2 >> 16) & 0xFF) as u8;
    let class_code = ((reg2 >> 24) & 0xFF) as u8;

    let header_type = ((reg3 >> 16) & 0xFF) as u8;

    let pci_dev = PciDevice {
        bus,
        device,
        function,
        vendor_id,
        device_id,
        class: class_code,
        subclass,
        prog_if,
        header_type,
    };

    println!(
        "PCI | {:04X}:{:04X} | {}",
        vendor_id, device_id, get_class_name(class_code, subclass)
    );

    devices.push(pci_dev);

    // If this device is actually a PCI-to-PCI Bridge, it means it connects to a 
    // whole sub-layer of the motherboard (like an extended PCIe lane). We MUST 
    // recursively scan the secondary bus hidden behind it!
    if class_code == 0x06 && subclass == 0x04 {
        let reg6 = read_config_register(bus, device, function, 0x18);
        let secondary_bus = ((reg6 >> 8) & 0xFF) as u8;
        scan_bus(secondary_bus, devices);
    }
}

/// Translates raw numeric hardware classifications into human-readable strings.
pub fn get_class_name(class: u8, subclass: u8) -> &'static str {
    match class {
        0x01 => match subclass {
            0x01 => "IDE Storage Controller",
            0x06 => "SATA Storage Controller",
            0x08 => "NVMe Storage Controller",
            _ => "Mass Storage Controller",
        },
        0x02 => match subclass {
            0x00 => "Ethernet Controller",
            0x80 => "Network Controller",
            _ => "Network Device",
        },
        0x03 => "Display Controller (VGA/GPU)",
        0x04 => "Multimedia Audio Controller",
        0x06 => match subclass {
            0x00 => "Host Bridge",
            0x01 => "ISA Bridge",
            0x04 => "PCI-to-PCI Bridge",
            _ => "Bridge Device",
        },
        0x0C => match subclass {
            0x03 => "USB Controller",
            _ => "Serial Bus Controller",
        },
        _ => "Unknown Hardware Device",
    }
}
