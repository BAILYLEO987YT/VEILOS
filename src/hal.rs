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
// executor is about to start), so the boot order is unchanged. Devices
// hot-plugged later (`hotplug.rs`) are added with `add` and bound on the
// spot; `remove` asks the driver to let go before the slot is powered off.
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
    /// Hot-removed. The record stays so device ids don't change.
    Removed,
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
    /// Let go of the device (stop DMA, drop it from the logical device) so it
    /// can be removed. None: the driver can't, and hot-removal is refused.
    pub remove: Option<fn(&PciDevice) -> Result<(), String>>,
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
        bind_one(id, &pci);
    }
}

fn bind_one(id: usize, pci: &PciDevice) {
    let class = DeviceClass::of(pci);
    let Some(driver) = DRIVERS.iter().find(|drv| drv.class == class && (drv.matches)(pci)) else {
        return;
    };
    // Firmware or a previous OS may have left it asleep.
    if pci.power_state().is_some_and(|s| s != crate::pci::PowerState::D0) {
        pci.set_power_state(crate::pci::PowerState::D0);
        crate::serial_println!("[HAL] {:02x}:{:02x}.{} woken to D0 for its driver", pci.bus, pci.device, pci.function);
    }
    // The device list is not locked during probe: drivers may take a
    // while (disk spin-up, DHCP start) and may call back into the HAL.
    let result = (driver.probe)(pci);
    let mut devices = DEVICES.lock();
    let dev = &mut devices[id];
    dev.driver = Some(driver.name);
    dev.state = match result {
        Ok(()) => DeviceState::Bound,
        Err(why) => DeviceState::Failed(why),
    };
    crate::serial_println!("[HAL] {} {:?} -> {} ({:?})", dev.location(), class, driver.name, dev.state);
}

/// A device that appeared after boot: record it and offer it to the drivers.
pub fn add(pci: PciDevice) {
    let id = {
        let mut devices = DEVICES.lock();
        let id = devices.len();
        devices.push(Device { id, class: DeviceClass::of(&pci), pci: pci.clone(), driver: None, state: DeviceState::Unbound });
        id
    };
    crate::serial_println!("[HAL] {:02x}:{:02x}.{} added", pci.bus, pci.device, pci.function);
    bind_one(id, &pci);
}

/// Prepare every device on `bus` for removal. Fails, changing nothing, if
/// any of them has a driver that can't let go.
pub fn remove_bus(bus: u8) -> Result<(), String> {
    let on_bus: Vec<Device> = DEVICES.lock().iter().filter(|d| d.pci.bus == bus && d.state != DeviceState::Removed).cloned().collect();
    let driver_of = |d: &Device| DRIVERS.iter().find(|drv| Some(drv.name) == d.driver);
    if let Some(d) = on_bus.iter().find(|d| d.state == DeviceState::Bound && driver_of(d).is_some_and(|drv| drv.remove.is_none())) {
        return Err(format!("{} ({}) can't be released", d.location(), d.driver.unwrap_or("?")));
    }
    for d in &on_bus {
        if d.state == DeviceState::Bound {
            if let Some(remove) = driver_of(d).and_then(|drv| drv.remove) {
                if let Err(why) = remove(&d.pci) {
                    crate::serial_println!("[HAL] {}: driver release reported: {}", d.location(), why);
                }
            }
        }
        mark_removed(d.id);
    }
    Ok(())
}

/// Record that the device is gone (after `remove_bus`, or a surprise removal,
/// where the driver finds out from the hardware no longer answering).
fn mark_removed(id: usize) {
    let mut devices = DEVICES.lock();
    let d = &mut devices[id];
    d.state = DeviceState::Removed;
    crate::pci::unregister(d.pci.bus, d.pci.device, d.pci.function);
    crate::serial_println!("[HAL] {} removed", d.location());
}

/// Surprise removal of everything on `bus`: drivers are told (best effort),
/// the records are marked removed.
pub fn gone_bus(bus: u8) {
    let on_bus: Vec<Device> = DEVICES.lock().iter().filter(|d| d.pci.bus == bus && d.state != DeviceState::Removed).cloned().collect();
    for d in &on_bus {
        if d.state == DeviceState::Bound {
            if let Some(remove) = DRIVERS.iter().find(|drv| Some(drv.name) == d.driver).and_then(|drv| drv.remove) {
                let _ = remove(&d.pci);
            }
        }
        mark_removed(d.id);
    }
}

/// Put devices nobody drives into D3hot (call once every class is bound).
/// Bridges keep forwarding for the devices behind them, display adapters may
/// still show firmware's console, and USB controllers may be running the
/// firmware's legacy keyboard emulation, so those stay on.
pub fn power_down_idle() {
    let idle: Vec<Device> = DEVICES.lock().iter()
        .filter(|d| d.state != DeviceState::Bound && d.state != DeviceState::Removed)
        .filter(|d| !matches!(d.class, DeviceClass::Bridge | DeviceClass::Display | DeviceClass::SerialBus))
        .filter(|d| d.pci.header_type == 0)
        .cloned()
        .collect();
    for d in idle {
        if d.pci.set_power_state(crate::pci::PowerState::D3hot) {
            crate::serial_println!("[HAL] {} {:04x}:{:04x} not driven: now in D3hot", d.location(), d.pci.vendor_id, d.pci.device_id);
        }
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
                DeviceState::Removed => String::from("removed"),
            };
            let power = match d.pci.power_state() {
                Some(p) if d.state != DeviceState::Removed => format!(" [{:?}]", p),
                _ => String::new(),
            };
            format!(
                "{} {:04x}:{:04x} {:?} - {}{}",
                d.location(), d.pci.vendor_id, d.pci.device_id, d.class, state, power
            )
        })
        .collect()
}
