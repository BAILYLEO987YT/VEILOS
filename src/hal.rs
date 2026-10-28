// =============================================================================
// VEILOS: hardware abstraction layer, device model
// =============================================================================
//
// Every PCI device gets one `Device` record. Drivers are listed in `DRIVERS`;
// `bind(class)` hands each device of that class to the first driver whose
// `matches` accepts it, calls the driver's `probe`, and records the outcome.
// Drivers no longer search the PCI list themselves, so a second AHCI
// controller or NIC needs no driver changes, and `devices()` shows what is
// driven and what isn't (the terminal's DEVICES command).
//
// Binding runs one class at a time, at the point in boot where that class
// used to be initialised (storage and network early, display once the
// executor is about to start), so the boot order is unchanged.
//
// Logical devices (the one GPU / display / NIC software sees, backed by
// however many physical devices are bound here) build on this: see
// HAL_DESIGN.md.

use alloc::{format, string::String, vec::Vec};
use spin::Mutex;

use crate::pci::PciDevice;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DeviceClass {
    Storage,
    Network,
    Display,
    Multimedia,
    Bridge,
    SerialBus,
    Other,
}

impl DeviceClass {
    fn of(dev: &PciDevice) -> Self {
        match dev.class {
            0x01 => Self::Storage,
            0x02 => Self::Network,
            0x03 => Self::Display,
            0x04 => Self::Multimedia,
            0x06 => Self::Bridge,
            0x0C => Self::SerialBus,
            _ => Self::Other,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeviceState {
    /// No driver matches (or its class hasn't been bound yet).
    Unbound,
    /// A driver is running it.
    Bound,
    /// A driver matched but declined or failed; the reason is kept.
    Failed(String),
}

#[derive(Clone)]
pub struct Device {
    pub id: usize,
    pub class: DeviceClass,
    pub pci: PciDevice,
    pub driver: Option<&'static str>,
    pub state: DeviceState,
}

impl Device {
    pub fn location(&self) -> String {
        format!("{:02x}:{:02x}.{}", self.pci.bus, self.pci.device, self.pci.function)
    }
}

pub struct Driver {
    pub name: &'static str,
    pub class: DeviceClass,
    pub matches: fn(&PciDevice) -> bool,
    /// Take ownership of the device. `Err` explains why it wasn't driven.
    pub probe: fn(&PciDevice) -> Result<(), String>,
}

/// Every driver the kernel has, in match priority order.
static DRIVERS: &[Driver] = &[
    crate::ahci::DRIVER,
    crate::network::VIRTIO_NET_DRIVER,
    crate::graphics::LFB_DISPLAY_DRIVER,
];

static DEVICES: Mutex<Vec<Device>> = Mutex::new(Vec::new());

/// Build the device list from the PCI registry. Call once, after `pci::init`.
pub fn init() {
    let devices: Vec<Device> = crate::pci::devices()
        .into_iter()
        .enumerate()
        .map(|(id, pci)| Device { id, class: DeviceClass::of(&pci), pci, driver: None, state: DeviceState::Unbound })
        .collect();
    crate::serial_println!("[HAL] {} device(s), {} driver(s)", devices.len(), DRIVERS.len());
    *DEVICES.lock() = devices;
}

/// Offer every device of `class` to the drivers.
pub fn bind(class: DeviceClass) {
    let candidates: Vec<(usize, PciDevice)> = DEVICES
        .lock()
        .iter()
        .filter(|d| d.class == class && d.state == DeviceState::Unbound)
        .map(|d| (d.id, d.pci.clone()))
        .collect();

    for (id, pci) in candidates {
        let Some(driver) = DRIVERS.iter().find(|drv| drv.class == class && (drv.matches)(&pci)) else {
            continue;
        };
        // The device list is not locked during probe: drivers may take a
        // while (disk spin-up, DHCP start) and may call back into the HAL.
        let result = (driver.probe)(&pci);
        let mut devices = DEVICES.lock();
        let dev = &mut devices[id];
        dev.driver = Some(driver.name);
        dev.state = match result {
            Ok(()) => DeviceState::Bound,
            Err(why) => DeviceState::Failed(why),
        };
        crate::serial_println!("[HAL] {} {:?} -> {} ({:?})", dev.location(), class, driver.name, dev.state);
    }
}

/// A snapshot of every device.
pub fn devices() -> Vec<Device> {
    DEVICES.lock().clone()
}

/// Devices of `class` that a driver is running.
pub fn bound(class: DeviceClass) -> Vec<Device> {
    DEVICES.lock().iter().filter(|d| d.class == class && d.state == DeviceState::Bound).cloned().collect()
}

/// One line per device, for the terminal's DEVICES command.
pub fn describe() -> Vec<String> {
    devices()
        .iter()
        .map(|d| {
            let state = match &d.state {
                DeviceState::Unbound => String::from("no driver"),
                DeviceState::Bound => format!("{}", d.driver.unwrap_or("?")),
                DeviceState::Failed(why) => format!("{}: {}", d.driver.unwrap_or("?"), why),
            };
            format!(
                "{} {:04x}:{:04x} {:?} - {}",
                d.location(), d.pci.vendor_id, d.pci.device_id, d.class, state
            )
        })
        .collect()
}
