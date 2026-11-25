// Plug and Play for Windows drivers: PCI devices handed to a driver's
// AddDevice, started with their resources, and removed again.
//
// When a driver with an AddDevice routine loads (image.rs), every PCI
// device no VEILOS driver runs whose IDs match the driver's (its .inf,
// beside the .sys, or IDs given to DRIVER LOAD) becomes a device of that
// driver, as Windows' PnP manager would make it:
//
//   1. A physical device object (PDO), \Device\NTPNP_PCInnnn, owned by
//      VEILOS's own "PCI bus driver" (`pdo_dispatch` below). It answers the
//      PnP requests function drivers pass down: IDs, capabilities, the bus
//      interface (PCI configuration space), device text, config reads.
//   2. The device's interrupt: a vector of its own (intr.rs), with the
//      device's MSI (or MSI-X entry 0) pointed at it.
//   3. AddDevice(driver, PDO): the driver makes its device object and
//      attaches it above the PDO.
//   4. IRP_MN_QUERY_CAPABILITIES, then IRP_MN_START_DEVICE down the stack
//      with the raw and translated CM_RESOURCE_LISTs: every BAR (memory and
//      I/O ports) and the message-signaled interrupt.
//
// Each device is separate: two cards of one make share the driver, each
// with its own PDO, stack, resources, vector and registry keys; cards of
// different makes load different drivers (WDDM_DESIGN.md, Several GPUs).
//
// DRIVER UNLOAD sends IRP_MN_REMOVE_DEVICE to each of the driver's stacks
// before DriverUnload, and gives the devices back to the HAL.

use alloc::{boxed::Box, format, string::String, vec, vec::Vec};
use spin::Mutex;

use super::{io, ke, pool, NtStatus, STATUS_BUFFER_TOO_SMALL, STATUS_INVALID_PARAMETER, STATUS_OBJECT_NAME_NOT_FOUND, STATUS_SUCCESS};
use crate::hal::pci::PciDevice;

pub const IRP_MJ_POWER: u8 = 0x16;
pub const IRP_MJ_SYSTEM_CONTROL: u8 = 0x17;
pub const IRP_MJ_PNP: u8 = 0x1B;

pub const IRP_MN_START_DEVICE: u8 = 0x00;
pub const IRP_MN_QUERY_REMOVE_DEVICE: u8 = 0x01;
pub const IRP_MN_REMOVE_DEVICE: u8 = 0x02;
pub const IRP_MN_STOP_DEVICE: u8 = 0x04;
pub const IRP_MN_QUERY_DEVICE_RELATIONS: u8 = 0x07;
pub const IRP_MN_QUERY_INTERFACE: u8 = 0x08;
pub const IRP_MN_QUERY_CAPABILITIES: u8 = 0x09;
pub const IRP_MN_QUERY_RESOURCES: u8 = 0x0A;
pub const IRP_MN_QUERY_RESOURCE_REQUIREMENTS: u8 = 0x0B;
pub const IRP_MN_QUERY_DEVICE_TEXT: u8 = 0x0C;
pub const IRP_MN_READ_CONFIG: u8 = 0x0F;
pub const IRP_MN_WRITE_CONFIG: u8 = 0x10;
pub const IRP_MN_QUERY_ID: u8 = 0x13;
pub const IRP_MN_QUERY_PNP_DEVICE_STATE: u8 = 0x14;
pub const IRP_MN_QUERY_BUS_INFORMATION: u8 = 0x15;
pub const IRP_MN_DEVICE_USAGE_NOTIFICATION: u8 = 0x16;
pub const IRP_MN_SURPRISE_REMOVAL: u8 = 0x17;

pub const STATUS_NOT_SUPPORTED: NtStatus = 0xC000_00BBu32 as i32;

const SL_MINOR: usize = 0x01;

/// GUID_BUS_INTERFACE_STANDARD {496B8280-6F25-11D0-BEAF-08002BE2092F}.
const GUID_BUS_INTERFACE_STANDARD: [u8; 16] = [0x80, 0x82, 0x6B, 0x49, 0x25, 0x6F, 0xD0, 0x11, 0xBE, 0xAF, 0x08, 0x00, 0x2B, 0xE2, 0x09, 0x2F];
/// GUID_BUS_TYPE_PCI {C8EBDFB0-B510-11D0-80E5-00A0C92542E3}.
pub const GUID_BUS_TYPE_PCI: [u8; 16] = [0xB0, 0xDF, 0xEB, 0xC8, 0x10, 0xB5, 0xD0, 0x11, 0x80, 0xE5, 0x00, 0xA0, 0xC9, 0x25, 0x42, 0xE3];

/// A PCI device a Windows driver runs.
pub struct PnpDevice {
    pub hal_id: usize,
    pub pci: PciDevice,
    pub pdo: usize,
    /// The device's interrupt vector, and whether it is message-signaled.
    pub vector: Option<u8>,
    pub message: Option<(u64, u32)>,
    pub driver: String,
    pub driver_object: usize,
    /// "PCI\VEN_xxxx&DEV_xxxx&SUBSYS_xxxxxxxx&REV_xx".
    pub device_id: String,
    pub instance: String,
    pub hardware_ids: Vec<String>,
    pub compatible_ids: Vec<String>,
    /// Pool CM_RESOURCE_LISTs (raw, translated), for the life of the device.
    pub raw: usize,
    pub translated: usize,
    pub started: bool,
    /// The driver key's instance number (Control\Class\{guid}\nnnn).
    pub class_index: u32,
    /// IoSetDevicePropertyData: (property key, type, data).
    pub props: Vec<([u8; 20], u32, Vec<u8>)>,
}

unsafe impl Send for PnpDevice {}

static DEVICES: Mutex<Vec<PnpDevice>> = Mutex::new(Vec::new());
static BUS_DRIVER: Mutex<usize> = Mutex::new(0);
static NEXT_PDO: Mutex<u32> = Mutex::new(0);

unsafe fn rd<T: Copy>(p: *const u8, off: usize) -> T {
    unsafe { (p.add(off) as *const T).read_unaligned() }
}
unsafe fn wr<T>(p: *mut u8, off: usize, v: T) {
    unsafe { (p.add(off) as *mut T).write_unaligned(v) }
}

/// Run `f` on the record of the device whose stack `dev` is in.
pub fn with_device<R>(dev: *mut u8, f: impl FnOnce(&mut PnpDevice) -> R) -> Option<R> {
    let base = io::stack_base(dev) as usize;
    let mut list = DEVICES.lock();
    list.iter_mut().find(|d| d.pdo == base).map(f)
}

pub fn vector_of(pdo: u64) -> Option<u8> {
    with_device(pdo as *mut u8, |d| d.vector).flatten()
}

pub fn is_message(pdo: u64) -> bool {
    with_device(pdo as *mut u8, |d| d.message.is_some()).unwrap_or(false)
}

/// The MSI address and data the device sends.
pub fn message_of(pdo: u64) -> Option<(u64, u32)> {
    with_device(pdo as *mut u8, |d| d.message).flatten()
}

// -----| IDs |-----

fn ids_of(pci: &PciDevice) -> (String, Vec<String>, Vec<String>) {
    let a = pci.address();
    let (ssvid, ssid) = (a.read_u16(0x2C), a.read_u16(0x2E));
    let ven = format!("PCI\\VEN_{:04X}&DEV_{:04X}", pci.vendor_id, pci.device_id);
    let subsys = format!("SUBSYS_{:04X}{:04X}", ssid, ssvid);
    let cc = format!("{:02X}{:02X}", pci.class, pci.subclass);
    let device_id = format!("{}&{}&REV_{:02X}", ven, subsys, pci.revision);
    let hardware = vec![
        device_id.clone(),
        format!("{}&{}", ven, subsys),
        format!("{}&CC_{}{:02X}", ven, cc, pci.prog_if),
        format!("{}&CC_{}", ven, cc),
    ];
    let compatible = vec![
        format!("{}&REV_{:02X}", ven, pci.revision),
        ven.clone(),
        format!("PCI\\VEN_{:04X}&CC_{}{:02X}", pci.vendor_id, cc, pci.prog_if),
        format!("PCI\\VEN_{:04X}&CC_{}", pci.vendor_id, cc),
        format!("PCI\\VEN_{:04X}", pci.vendor_id),
        format!("PCI\\CC_{}{:02X}", cc, pci.prog_if),
        format!("PCI\\CC_{}", cc),
    ];
    (device_id, hardware, compatible)
}

/// The PCI hardware IDs an .inf names (its `PCI\VEN_...` tokens).
pub fn inf_ids(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in text.lines() {
        let line = line.split(';').next().unwrap_or("");
        let up = line.to_ascii_uppercase();
        let mut rest = up.as_str();
        // PCI\VEN_... and class IDs (PCI\CC_0300: any VGA-compatible
        // adapter, as display-only drivers name it).
        while let Some(at) = [rest.find("PCI\\VEN_"), rest.find("PCI\\CC_")].into_iter().flatten().min() {
            let tail = &rest[at..];
            let end = tail.find(|c: char| !(c.is_ascii_alphanumeric() || c == '\\' || c == '_' || c == '&')).unwrap_or(tail.len());
            let id = &tail[..end];
            if !out.iter().any(|x| x == id) {
                out.push(String::from(id));
            }
            rest = &tail[end..];
        }
    }
    out
}

fn class_guid(pci: &PciDevice) -> &'static str {
    match pci.class {
        0x03 => "{4d36e968-e325-11ce-bfc1-08002be10318}", // Display
        0x02 => "{4d36e972-e325-11ce-bfc1-08002be10318}", // Net
        0x04 => "{4d36e96c-e325-11ce-bfc1-08002be10318}", // Media
        _ => "{4d36e97d-e325-11ce-bfc1-08002be10318}",    // System
    }
}

fn class_name(pci: &PciDevice) -> &'static str {
    match pci.class {
        0x03 => "Display",
        0x02 => "Net",
        0x04 => "MEDIA",
        _ => "System",
    }
}

/// The device's registry keys (hive paths).
pub fn device_key(d: &PnpDevice) -> String {
    format!(r"HKLM\SYSTEM\CurrentControlSet\Enum\{}\{}", d.device_id, d.instance)
}

pub fn driver_key(d: &PnpDevice) -> String {
    format!(r"HKLM\SYSTEM\CurrentControlSet\Control\Class\{}\{:04}", class_guid(&d.pci), d.class_index)
}

// -----| resources |-----

// CM_RESOURCE_LIST (pack 4, checked in k1's layout.c): Count 0, then one
// CM_FULL_RESOURCE_DESCRIPTOR: InterfaceType 4, BusNumber 8, Version 0xC,
// Revision 0xE, Count 0x10, descriptors from 0x14, 0x14 bytes each: Type 0,
// ShareDisposition 1, Flags 2, u 4.
const CM_LIST_DESCRIPTORS: usize = 0x14;
const CM_DESCRIPTOR_SIZE: usize = 0x14;
const PCI_BUS: u32 = 5;
const CM_RESOURCE_TYPE_PORT: u8 = 1;
const CM_RESOURCE_TYPE_INTERRUPT: u8 = 2;
const CM_RESOURCE_TYPE_MEMORY: u8 = 3;
const SHARE_DEVICE_EXCLUSIVE: u8 = 1;
const CM_RESOURCE_PORT_IO: u16 = 0x1;
const CM_RESOURCE_MEMORY_PREFETCHABLE: u16 = 0x4;
const CM_RESOURCE_MEMORY_LARGE_40: u16 = 0x200;
const CM_RESOURCE_MEMORY_LARGE_48: u16 = 0x400;
const CM_RESOURCE_MEMORY_LARGE_64: u16 = 0x800;
const CM_RESOURCE_INTERRUPT_LATCHED: u16 = 0x1;
const CM_RESOURCE_INTERRUPT_MESSAGE: u16 = 0x2;

/// One descriptor: (type, flags, the 16 bytes of `u`).
type Desc = (u8, u16, [u8; 16]);

fn mem_desc(base: u64, size: u64, prefetch: bool) -> Desc {
    let mut u = [0u8; 16];
    u[0..8].copy_from_slice(&base.to_le_bytes());
    let mut flags = if prefetch { CM_RESOURCE_MEMORY_PREFETCHABLE } else { 0 };
    // Lengths over 4 GiB: the large forms, in 256-byte, 64 KiB or 4 GiB units.
    let len = if size <= 0xFFFF_FFFF {
        size as u32
    } else if size >> 8 <= 0xFFFF_FFFF && size & 0xFF == 0 {
        flags |= CM_RESOURCE_MEMORY_LARGE_40;
        (size >> 8) as u32
    } else if size >> 16 <= 0xFFFF_FFFF && size & 0xFFFF == 0 {
        flags |= CM_RESOURCE_MEMORY_LARGE_48;
        (size >> 16) as u32
    } else {
        flags |= CM_RESOURCE_MEMORY_LARGE_64;
        (size >> 32) as u32
    };
    u[8..12].copy_from_slice(&len.to_le_bytes());
    (CM_RESOURCE_TYPE_MEMORY, flags, u)
}

/// The raw and translated resource lists for a device, in pool.
fn resource_lists(pci: &PciDevice, vector: Option<u8>, message: bool) -> (*mut u8, *mut u8) {
    let mut descs: Vec<Desc> = Vec::new();
    for bar in pci.bars.iter() {
        if bar.is_mem() {
            descs.push(mem_desc(bar.base, bar.size, bar.prefetchable));
        } else if bar.is_io() {
            let mut u = [0u8; 16];
            u[0..8].copy_from_slice(&bar.base.to_le_bytes());
            u[8..12].copy_from_slice(&(bar.size as u32).to_le_bytes());
            descs.push((CM_RESOURCE_TYPE_PORT, CM_RESOURCE_PORT_IO, u));
        }
    }
    let make = |translated: bool| -> *mut u8 {
        let mut all = descs.clone();
        if let Some(v) = vector {
            let mut u = [0u8; 16];
            if translated {
                // Level (the IRQL), Group, Vector, Affinity.
                u[0..2].copy_from_slice(&(super::intr::irql_of(v) as u16).to_le_bytes());
            } else if message {
                // Reserved, MessageCount, Vector, Affinity.
                u[2..4].copy_from_slice(&1u16.to_le_bytes());
            }
            u[4..8].copy_from_slice(&(v as u32).to_le_bytes());
            u[8..16].copy_from_slice(&1u64.to_le_bytes());
            let flags = CM_RESOURCE_INTERRUPT_LATCHED | if message { CM_RESOURCE_INTERRUPT_MESSAGE } else { 0 };
            all.push((CM_RESOURCE_TYPE_INTERRUPT, flags, u));
        }
        let list = pool::alloc_tracked(CM_LIST_DESCRIPTORS + all.len() * CM_DESCRIPTOR_SIZE);
        if list.is_null() {
            return list;
        }
        unsafe {
            wr::<u32>(list, 0, 1);
            wr::<u32>(list, 4, PCI_BUS);
            wr::<u32>(list, 8, pci.bus as u32);
            wr::<u16>(list, 0xC, 1);
            wr::<u16>(list, 0xE, 1);
            wr::<u32>(list, 0x10, all.len() as u32);
            for (i, (ty, flags, u)) in all.iter().enumerate() {
                let d = list.add(CM_LIST_DESCRIPTORS + i * CM_DESCRIPTOR_SIZE);
                wr::<u8>(d, 0, *ty);
                wr::<u8>(d, 1, SHARE_DEVICE_EXCLUSIVE);
                wr::<u16>(d, 2, *flags);
                core::ptr::copy_nonoverlapping(u.as_ptr(), d.add(4), 16);
            }
        }
        list
    };
    (make(false), make(true))
}

fn list_size(list: *const u8) -> usize {
    if list.is_null() {
        return 0;
    }
    CM_LIST_DESCRIPTORS + unsafe { rd::<u32>(list, 0x10) } as usize * CM_DESCRIPTOR_SIZE
}

/// Point the device's MSI, or MSI-X entry 0, at `vector` on this CPU.
/// The message's (address, data), or None if it has neither.
fn route_msi(pci: &PciDevice, vector: u8) -> Option<(u64, u32)> {
    if crate::arch::smp::LAPIC_BASE.load(core::sync::atomic::Ordering::Relaxed) == 0 {
        return None;
    }
    let apic = u8::try_from(crate::arch::smp::current_apic_id()).ok()?;
    let addr = 0xFEE0_0000u64 | (apic as u64) << 12;
    if pci.enable_msi(vector, apic) {
        return Some((addr, vector as u32));
    }
    let m = pci.msix()?;
    let table = pci.bar(m.table_bar as usize).mem_base()? + m.table_offset as u64;
    let page = super::mm::MmMapIoSpace(table & !0xFFF, 0x1000, 0);
    if page == 0 {
        return None;
    }
    pci.enable_msix(page + (table & 0xFFF), vector, apic).then_some((addr, vector as u32))
}

fn unroute_msi(pci: &PciDevice) {
    let a = pci.address();
    if let Some(m) = pci.msi() {
        let c = a.read_u16(m.offset + 2);
        a.write_u16(m.offset + 2, c & !1);
    }
    if let Some(m) = pci.msix() {
        let c = a.read_u16(m.offset + 2);
        a.write_u16(m.offset + 2, c & !(1 << 15));
    }
}

// -----| the bus driver's PDOs |-----

fn bus_driver() -> *mut u8 {
    let mut b = BUS_DRIVER.lock();
    if *b == 0 {
        let drv = pool::alloc_tracked(io::DRV_SIZE);
        let ext = pool::alloc_tracked(0x80);
        unsafe {
            wr::<u16>(drv, 0, 4);
            wr::<u16>(drv, 2, io::DRV_SIZE as u16);
            wr(drv, io::DRV_EXTENSION, ext);
            wr(ext, 0, drv);
            for k in 0..=io::IRP_MJ_MAXIMUM_FUNCTION {
                wr::<u64>(drv, io::DRV_MAJOR_FUNCTION + 8 * k, pdo_dispatch as *const () as u64);
            }
        }
        *b = drv as usize;
    }
    *b as *mut u8
}

fn complete(irp: *mut u8, status: NtStatus, info: u64) -> NtStatus {
    unsafe {
        wr(irp, io::IRP_STATUS, status);
        wr(irp, io::IRP_INFORMATION, info);
    }
    io::IofCompleteRequest(irp, 0);
    status
}

/// A wide string (or MULTI_SZ, `multi`) in pool, as PnP answers carry.
fn pool_wide(parts: &[String], multi: bool) -> *mut u8 {
    let mut units: Vec<u16> = Vec::new();
    for p in parts {
        units.extend(p.encode_utf16());
        units.push(0);
        if !multi {
            break;
        }
    }
    if multi {
        units.push(0);
    }
    let buf = pool::alloc_tracked(units.len() * 2);
    if !buf.is_null() {
        unsafe { core::ptr::copy_nonoverlapping(units.as_ptr(), buf as *mut u16, units.len()) };
    }
    buf
}

/// What VEILOS's PCI bus driver does with a request reaching a PDO.
extern "win64" fn pdo_dispatch(dev: *mut u8, irp: *mut u8) -> NtStatus {
    let sl: *mut u8 = unsafe { rd(irp, io::IRP_CURRENT_STACK) };
    let (major, minor): (u8, u8) = unsafe { (rd(sl, 0), rd(sl, SL_MINOR)) };
    let current: NtStatus = unsafe { rd(irp, io::IRP_STATUS) };
    match major {
        IRP_MJ_PNP => {}
        IRP_MJ_POWER => return complete(irp, STATUS_SUCCESS, 0),
        IRP_MJ_SYSTEM_CONTROL => return complete(irp, current, unsafe { rd(irp, io::IRP_INFORMATION) }),
        io::IRP_MJ_CREATE | io::IRP_MJ_CLOSE | io::IRP_MJ_CLEANUP => return complete(irp, STATUS_SUCCESS, 0),
        _ => return complete(irp, super::STATUS_INVALID_DEVICE_REQUEST, 0),
    }
    let p = |off: usize| unsafe { sl.add(io::SL_PARAMETERS + off) };
    let found = with_device(dev, |d| (d.pci.clone(), d.device_id.clone(), d.instance.clone(), d.hardware_ids.clone(), d.compatible_ids.clone()));
    let Some((pci, device_id, instance, hw, compat)) = found else { return complete(irp, current, 0) };
    match minor {
        IRP_MN_START_DEVICE => {
            pci.set_command_bits(crate::hal::pci::CMD_MEMORY_SPACE | crate::hal::pci::CMD_IO_SPACE | crate::hal::pci::CMD_BUS_MASTER);
            complete(irp, STATUS_SUCCESS, 0)
        }
        IRP_MN_QUERY_REMOVE_DEVICE | IRP_MN_REMOVE_DEVICE | IRP_MN_STOP_DEVICE | 0x03 | 0x05 | 0x06 | IRP_MN_SURPRISE_REMOVAL
        | IRP_MN_DEVICE_USAGE_NOTIFICATION | IRP_MN_QUERY_PNP_DEVICE_STATE => complete(irp, STATUS_SUCCESS, if minor == IRP_MN_QUERY_PNP_DEVICE_STATE { 0 } else { unsafe { rd(irp, io::IRP_INFORMATION) } }),
        IRP_MN_QUERY_CAPABILITIES => {
            let caps: *mut u8 = unsafe { rd(p(0), 0) };
            if !caps.is_null() {
                unsafe {
                    wr::<u32>(caps, 8, (pci.device as u32) << 16 | pci.function as u32); // Address
                    wr::<u32>(caps, 0xC, u32::MAX); // UINumber
                    // DeviceState: S0 -> D0, S1..S5 -> D3.
                    wr::<u32>(caps, 0x10, 0);
                    wr::<u32>(caps, 0x14, 1);
                    for s in 2..7 {
                        wr::<u32>(caps, 0x10 + 4 * s, 4);
                    }
                }
            }
            complete(irp, STATUS_SUCCESS, 0)
        }
        IRP_MN_QUERY_ID => {
            let kind: u32 = unsafe { rd(p(0), 0) };
            let buf = match kind {
                0 => pool_wide(&[device_id], false),
                1 => pool_wide(&hw, true),
                2 => pool_wide(&compat, true),
                3 => pool_wide(&[instance], false),
                _ => return complete(irp, current, 0),
            };
            complete(irp, STATUS_SUCCESS, buf as u64)
        }
        IRP_MN_QUERY_DEVICE_TEXT => {
            let kind: u32 = unsafe { rd(p(0), 0) };
            let text = match kind {
                0 => format!("PCI device {:04X}:{:04X}", pci.vendor_id, pci.device_id),
                1 => format!("PCI bus {}, device {}, function {}", pci.bus, pci.device, pci.function),
                _ => return complete(irp, current, 0),
            };
            complete(irp, STATUS_SUCCESS, pool_wide(&[text], false) as u64)
        }
        IRP_MN_QUERY_BUS_INFORMATION => {
            let info = pool::alloc_tracked(24);
            unsafe {
                core::ptr::copy_nonoverlapping(GUID_BUS_TYPE_PCI.as_ptr(), info, 16);
                wr::<u32>(info, 16, PCI_BUS);
                wr::<u32>(info, 20, pci.bus as u32);
            }
            complete(irp, STATUS_SUCCESS, info as u64)
        }
        IRP_MN_QUERY_DEVICE_RELATIONS => {
            // TargetDeviceRelation: the PDO itself.
            let kind: u32 = unsafe { rd(p(0), 0) };
            if kind != 4 {
                return complete(irp, current, unsafe { rd(irp, io::IRP_INFORMATION) });
            }
            let rel = pool::alloc_tracked(16);
            unsafe {
                wr::<u32>(rel, 0, 1);
                wr(rel, 8, dev);
            }
            complete(irp, STATUS_SUCCESS, rel as u64)
        }
        IRP_MN_QUERY_INTERFACE => {
            // Parameters.QueryInterface (from the stack location's 0x08):
            // InterfaceType 0, Size 8, Version 0xA, Interface 0x10.
            let guid: *const u8 = unsafe { rd(p(0), 0) };
            let size: u16 = unsafe { rd(p(0), 8) };
            let iface: *mut u8 = unsafe { rd(p(0), 0x10) };
            let is_bus = !guid.is_null() && unsafe { core::slice::from_raw_parts(guid, 16) } == GUID_BUS_INTERFACE_STANDARD;
            if !is_bus || iface.is_null() {
                return complete(irp, current, 0);
            }
            if (size as usize) < BUS_INTERFACE_SIZE {
                return complete(irp, STATUS_INVALID_PARAMETER, 0);
            }
            unsafe {
                wr::<u16>(iface, 0, BUS_INTERFACE_SIZE as u16);
                wr::<u16>(iface, 2, 1);
                wr(iface, 8, dev);
                wr::<u64>(iface, 0x10, iface_reference as *const () as u64);
                wr::<u64>(iface, 0x18, iface_reference as *const () as u64);
                wr::<u64>(iface, 0x20, bus_translate_address as *const () as u64);
                wr::<u64>(iface, 0x28, bus_get_dma_adapter as *const () as u64);
                wr::<u64>(iface, 0x30, bus_set_data as *const () as u64);
                wr::<u64>(iface, 0x38, bus_get_data as *const () as u64);
            }
            complete(irp, STATUS_SUCCESS, 0)
        }
        IRP_MN_READ_CONFIG | IRP_MN_WRITE_CONFIG => {
            // Parameters.ReadWriteConfig: WhichSpace 0, Buffer 8, Offset 0x10,
            // Length 0x18 (pointer-aligned).
            let (space, buf, off, len): (u32, *mut u8, u32, u32) = unsafe { (rd(p(0), 0), rd(p(0), 8), rd(p(0), 0x10), rd(p(0), 0x18)) };
            if space != 0 {
                return complete(irp, STATUS_NOT_SUPPORTED, 0);
            }
            let n = config(&pci, minor == IRP_MN_WRITE_CONFIG, buf, off, len);
            complete(irp, STATUS_SUCCESS, n as u64)
        }
        IRP_MN_QUERY_RESOURCES | IRP_MN_QUERY_RESOURCE_REQUIREMENTS => complete(irp, current, unsafe { rd(irp, io::IRP_INFORMATION) }),
        _ => complete(irp, current, unsafe { rd(irp, io::IRP_INFORMATION) }),
    }
}

/// Read or write PCI configuration space: bytes done.
fn config(pci: &PciDevice, write: bool, buf: *mut u8, off: u32, len: u32) -> u32 {
    let a = pci.address();
    let n = len.min(256u32.saturating_sub(off));
    for i in 0..n {
        let at = (off + i) as u8;
        unsafe {
            if write {
                a.write_u8(at, buf.add(i as usize).read());
            } else {
                buf.add(i as usize).write(a.read_u8(at));
            }
        }
    }
    n
}

// BUS_INTERFACE_STANDARD: INTERFACE (Size 0, Version 2, Context 8,
// InterfaceReference 0x10, InterfaceDereference 0x18), then
// TranslateBusAddress 0x20, GetDmaAdapter 0x28, SetBusData 0x30,
// GetBusData 0x38.
const BUS_INTERFACE_SIZE: usize = 0x40;

extern "win64" fn iface_reference(_ctx: u64) {}

extern "win64" fn bus_translate_address(_ctx: u64, bus: i64, _len: u32, space: *mut u32, out: *mut i64) -> u8 {
    unsafe {
        out.write(bus);
        if !space.is_null() && space.read() > 1 {
            space.write(0);
        }
    }
    1
}

/// DMA adapters come with VidMm (W5): none yet.
extern "win64" fn bus_get_dma_adapter(_ctx: u64, _desc: *const u8, regs: *mut u32) -> u64 {
    if !regs.is_null() {
        unsafe { regs.write(0) };
    }
    0
}

extern "win64" fn bus_set_data(ctx: u64, kind: u32, buf: *mut u8, off: u32, len: u32) -> u32 {
    // PCI_WHICHSPACE_CONFIG (0), as drivers pass it; PCIConfiguration (4)
    // too, the HAL's name for it.
    if kind != 0 && kind != 4 {
        return 0;
    }
    with_device(ctx as *mut u8, |d| d.pci.clone()).map_or(0, |pci| config(&pci, true, buf, off, len))
}

extern "win64" fn bus_get_data(ctx: u64, kind: u32, buf: *mut u8, off: u32, len: u32) -> u32 {
    // PCI_WHICHSPACE_CONFIG (0), as drivers pass it; PCIConfiguration (4)
    // too, the HAL's name for it.
    if kind != 0 && kind != 4 {
        return 0;
    }
    with_device(ctx as *mut u8, |d| d.pci.clone()).map_or(0, |pci| config(&pci, false, buf, off, len))
}

// -----| adding, starting and removing devices |-----

/// Send a PnP request down the stack `top` heads. `setup` fills the
/// parameters. Status and Information.
fn pnp_request(top: *mut u8, minor: u8, setup: impl FnOnce(*mut u8)) -> (NtStatus, u64) {
    io::send(
        top,
        core::ptr::null_mut(),
        IRP_MJ_PNP,
        |irp, sl| unsafe {
            wr(irp, io::IRP_STATUS, STATUS_NOT_SUPPORTED);
            wr::<u8>(sl, SL_MINOR, minor);
            setup(sl);
        },
        |_| {},
    )
}

/// After DriverEntry: give the driver every unclaimed PCI device whose IDs
/// match `ids`. The number of devices started.
pub fn start_driver(name: &str, drv: *mut u8, ids: &[String]) -> usize {
    let add: u64 = unsafe { rd(rd::<*mut u8>(drv, io::DRV_EXTENSION), 8) };
    if add == 0 || ids.is_empty() {
        return 0;
    }
    let wanted: Vec<String> = ids.iter().map(|s| s.to_ascii_uppercase()).collect();
    let mut started = 0;
    for dev in crate::hal::devices() {
        if dev.pci.header_type != 0 {
            continue;
        }
        let (device_id, hw, compat) = ids_of(&dev.pci);
        if !hw.iter().chain(compat.iter()).any(|id| wanted.iter().any(|w| w == id)) {
            continue;
        }
        let leaked: &'static str = Box::leak(String::from(name).into_boxed_str());
        let display = crate::nt::dxgk::is_display_driver(drv) && dev.class == crate::hal::DeviceClass::Display;
        if !crate::hal::claim(dev.id, leaked) && !(display && crate::hal::claim_display(dev.id, leaked)) {
            continue;
        }
        match add_device(name, drv, add, dev.id, &dev.pci, device_id, hw, compat) {
            Ok(()) => started += 1,
            Err(why) => {
                crate::serial_println!("[PNP] {} on {}: {}", name, dev.location(), why);
                crate::hal::release(dev.id, Some(why));
            }
        }
    }
    started
}

#[allow(clippy::too_many_arguments)]
fn add_device(name: &str, drv: *mut u8, add: u64, hal_id: usize, pci: &PciDevice, device_id: String, hw: Vec<String>, compat: Vec<String>) -> Result<(), String> {
    let n = {
        let mut next = NEXT_PDO.lock();
        *next += 1;
        *next
    };
    let pdo_name = format!("\\Device\\NTPNP_PCI{:04}", n);
    let mut pdo: *mut u8 = core::ptr::null_mut();
    let us = super::image::pool_unicode(&pdo_name);
    let st = io::IoCreateDevice(bus_driver(), 0, us, 0x1B /* FILE_DEVICE_BUS_EXTENDER */, 0, 0, &mut pdo);
    if st < 0 {
        return Err(format!("no device object ({:#x})", st as u32));
    }
    unsafe {
        let flags: u32 = rd(pdo, io::DEV_FLAGS);
        wr::<u32>(pdo, io::DEV_FLAGS, (flags & !io::DO_DEVICE_INITIALIZING) | 0x1000 /* DO_BUS_ENUMERATED_DEVICE */);
    }
    let vector = super::intr::alloc_vector();
    let message = vector.and_then(|v| route_msi(pci, v));
    let vector = match (vector, message) {
        (Some(v), None) => {
            super::intr::free_vector(v);
            None
        }
        (v, _) => v,
    };
    let (raw, translated) = resource_lists(pci, vector, message.is_some());
    let class_index = DEVICES.lock().iter().filter(|d| class_guid(&d.pci) == class_guid(pci)).count() as u32;
    let instance = format!("3&{:08x}&0&{:02X}", (pci.bus as u32) << 8 | 0x11, (pci.device << 3) | pci.function);
    DEVICES.lock().push(PnpDevice {
        hal_id,
        pci: pci.clone(),
        pdo: pdo as usize,
        vector,
        message,
        driver: String::from(name),
        driver_object: drv as usize,
        device_id,
        instance,
        hardware_ids: hw,
        compatible_ids: compat,
        raw: raw as usize,
        translated: translated as usize,
        started: false,
        class_index,
        props: Vec::new(),
    });
    // Its registry keys, as installing it would leave them.
    with_device(pdo, |d| {
        let dk = device_key(d);
        let _ = crate::nt::registry::kernel_key(&format!(r"{}\Device Parameters", dk), true);
        let _ = crate::nt::registry::kernel_key(&driver_key(d), true);
        crate::nt::registry::kernel_set(&dk, "Service", 1, wide_z(name));
        crate::nt::registry::kernel_set(&dk, "Driver", 1, wide_z(&format!("{}\\{:04}", class_guid(&d.pci), d.class_index)));
        crate::nt::registry::kernel_set(&dk, "ClassGUID", 1, wide_z(class_guid(&d.pci)));
    });
    crate::serial_println!(
        "[PNP] {:02x}:{:02x}.{} {:04x}:{:04x} -> {} ({}, interrupt {})",
        pci.bus, pci.device, pci.function, pci.vendor_id, pci.device_id, name, pdo_name,
        match (vector, message) {
            (Some(v), Some(_)) => format!("MSI vector {:#x}", v),
            _ => String::from("none"),
        }
    );

    let f: extern "win64" fn(*mut u8, *mut u8) -> NtStatus = unsafe { core::mem::transmute(add) };
    let st = f(drv, pdo);
    ke::set_irql(0);
    let top = io::IoGetAttachedDevice(pdo);
    if st < 0 || top == pdo {
        remove_record(pdo);
        return Err(format!("AddDevice returned {:#010x}{}", st as u32, if top == pdo && st >= 0 { " without attaching" } else { "" }));
    }

    // Capabilities first, as Windows asks before starting.
    let caps = pool::alloc_tracked(0x40);
    unsafe {
        wr::<u16>(caps, 0, 0x40);
        wr::<u16>(caps, 2, 1);
        wr::<u32>(caps, 8, u32::MAX);
        wr::<u32>(caps, 0xC, u32::MAX);
    }
    let _ = pnp_request(top, IRP_MN_QUERY_CAPABILITIES, |sl| unsafe { wr(sl, io::SL_PARAMETERS, caps) });
    pool::free_tracked(caps);

    let (st, _) = pnp_request(top, IRP_MN_START_DEVICE, |sl| unsafe {
        wr(sl, io::SL_PARAMETERS, raw);
        wr(sl, io::SL_PARAMETERS + 8, translated);
    });
    ke::set_irql(0);
    crate::serial_println!("[PNP] {} START_DEVICE on {} -> {:#010x}", name, pdo_name, st as u32);
    if st < 0 {
        let _ = pnp_request(top, IRP_MN_REMOVE_DEVICE, |_| {});
        remove_record(pdo);
        return Err(format!("START_DEVICE returned {:#010x}", st as u32));
    }
    with_device(pdo, |d| d.started = true);
    let _ = pnp_request(top, IRP_MN_QUERY_PNP_DEVICE_STATE, |_| {});
    Ok(())
}

fn wide_z(s: &str) -> Vec<u8> {
    s.encode_utf16().chain(core::iter::once(0)).flat_map(|u| u.to_le_bytes()).collect()
}

/// Forget a device: its PDO, interfaces, vector and resource lists.
fn remove_record(pdo: *mut u8) {
    let rec = {
        let mut list = DEVICES.lock();
        let at = list.iter().position(|d| d.pdo == pdo as usize);
        at.map(|i| list.remove(i))
    };
    let Some(d) = rec else { return };
    super::iox::forget_interfaces(pdo);
    unroute_msi(&d.pci);
    if let Some(v) = d.vector {
        super::intr::free_vector(v);
    }
    pool::free_tracked(d.raw as *mut u8);
    pool::free_tracked(d.translated as *mut u8);
    io::IoDeleteDevice(pdo);
    crate::hal::release(d.hal_id, None);
}

/// Before a driver unloads: IRP_MN_REMOVE_DEVICE to each of its stacks
/// (the driver detaches and deletes its device objects), then the PDOs go.
pub fn remove_driver(drv: *mut u8) -> usize {
    let pdos: Vec<usize> = DEVICES.lock().iter().filter(|d| d.driver_object == drv as usize).map(|d| d.pdo).collect();
    for &pdo in &pdos {
        let pdo = pdo as *mut u8;
        let top = io::IoGetAttachedDevice(pdo);
        let (st, _) = pnp_request(top, IRP_MN_REMOVE_DEVICE, |_| {});
        ke::set_irql(0);
        crate::serial_println!("[PNP] REMOVE_DEVICE -> {:#010x}", st as u32);
        remove_record(pdo);
    }
    pdos.len()
}

/// One line per device a Windows driver runs (the terminal's DRIVER LIST).
pub fn describe() -> Vec<String> {
    DEVICES
        .lock()
        .iter()
        .map(|d| {
            format!(
                "  {:02x}:{:02x}.{} {:04x}:{:04x} {} {}{}",
                d.pci.bus, d.pci.device, d.pci.function, d.pci.vendor_id, d.pci.device_id, d.driver,
                if d.started { "started" } else { "not started" },
                match d.vector {
                    Some(v) => format!(", vector {:#x}, {} interrupts", v, super::intr::count(v)),
                    None => String::new(),
                }
            )
        })
        .collect()
}

// -----| device properties |-----

pub const DEVICE_PROPERTY_HARDWARE_ID: u32 = 1;

fn wide_bytes(parts: &[String], multi: bool) -> Vec<u8> {
    let mut units: Vec<u16> = Vec::new();
    for p in parts {
        units.extend(p.encode_utf16());
        units.push(0);
        if !multi {
            break;
        }
    }
    if multi {
        units.push(0);
    }
    units.iter().flat_map(|u| u.to_le_bytes()).collect()
}

/// IoGetDeviceProperty(DeviceObject, DeviceProperty, BufferLength,
/// PropertyBuffer, ResultLength).
pub extern "win64" fn IoGetDeviceProperty(dev: *mut u8, prop: u32, len: u32, buf: *mut u8, result: *mut u32) -> NtStatus {
    let data = with_device(dev, |d| -> Result<Vec<u8>, NtStatus> {
        let pci = &d.pci;
        Ok(match prop {
            0 => wide_bytes(&[format!("PCI device {:04X}:{:04X}", pci.vendor_id, pci.device_id)], false),
            1 => wide_bytes(&d.hardware_ids, true),
            2 => wide_bytes(&d.compatible_ids, true),
            3 => unsafe { core::slice::from_raw_parts(d.raw as *const u8, list_size(d.raw as *const u8)) }.to_vec(),
            4 => unsafe { core::slice::from_raw_parts(d.translated as *const u8, list_size(d.translated as *const u8)) }.to_vec(),
            5 => wide_bytes(&[String::from(class_name(pci))], false),
            6 => wide_bytes(&[String::from(class_guid(pci))], false),
            7 => wide_bytes(&[format!("{}\\{:04}", class_guid(pci), d.class_index)], false),
            8 => wide_bytes(&[String::from("(Standard)")], false),
            9 => wide_bytes(&[format!("PCI device {:04X}:{:04X}", pci.vendor_id, pci.device_id)], false),
            10 => wide_bytes(&[format!("PCI bus {}, device {}, function {}", pci.bus, pci.device, pci.function)], false),
            11 => wide_bytes(&[io::name_of(d.pdo as *mut u8).unwrap_or_default()], false),
            12 => GUID_BUS_TYPE_PCI.to_vec(),
            13 => PCI_BUS.to_le_bytes().to_vec(),
            14 => (pci.bus as u32).to_le_bytes().to_vec(),
            15 => wide_bytes(&[String::from("PCI")], false),
            16 => ((pci.device as u32) << 16 | pci.function as u32).to_le_bytes().to_vec(),
            18 => 0u32.to_le_bytes().to_vec(), // InstallStateInstalled
            19 => 1u32.to_le_bytes().to_vec(), // RemovalPolicyExpectNoRemoval
            21 => unsafe { core::slice::from_raw_parts(d.translated as *const u8, list_size(d.translated as *const u8)) }.to_vec(),
            _ => return Err(0xC000_00F0u32 as i32), // STATUS_INVALID_PARAMETER_2
        })
    });
    let data = match data {
        Some(Ok(v)) => v,
        Some(Err(s)) => return s,
        None => return 0xC000_0010u32 as i32, // STATUS_INVALID_DEVICE_REQUEST: not a PnP device
    };
    if !result.is_null() {
        unsafe { result.write(data.len() as u32) };
    }
    if (len as usize) < data.len() {
        return STATUS_BUFFER_TOO_SMALL;
    }
    unsafe { core::ptr::copy_nonoverlapping(data.as_ptr(), buf, data.len()) };
    STATUS_SUCCESS
}

/// IoGetDevicePropertyData(Pdo, PropertyKey, Lcid, Flags, Size, Data,
/// RequiredSize, Type): what IoSetDevicePropertyData stored.
pub extern "win64" fn IoGetDevicePropertyData(pdo: *mut u8, key: *const u8, _lcid: u32, _flags: u32, size: u32, data: *mut u8, required: *mut u32, kind: *mut u32) -> NtStatus {
    let k: [u8; 20] = unsafe { core::ptr::read_unaligned(key as *const [u8; 20]) };
    let found = with_device(pdo, |d| d.props.iter().find(|(x, _, _)| *x == k).map(|(_, t, v)| (*t, v.clone()))).flatten();
    let Some((t, v)) = found else { return STATUS_OBJECT_NAME_NOT_FOUND };
    unsafe {
        if !required.is_null() {
            required.write(v.len() as u32);
        }
        if !kind.is_null() {
            kind.write(t);
        }
    }
    if (size as usize) < v.len() {
        return STATUS_BUFFER_TOO_SMALL;
    }
    unsafe { core::ptr::copy_nonoverlapping(v.as_ptr(), data, v.len()) };
    STATUS_SUCCESS
}

/// IoSetDevicePropertyData(Pdo, PropertyKey, Lcid, Flags, Type, Size,
/// Data). DEVPROP_TYPE_EMPTY (0) or no data deletes it.
pub extern "win64" fn IoSetDevicePropertyData(pdo: *mut u8, key: *const u8, _lcid: u32, _flags: u32, kind: u32, size: u32, data: *const u8) -> NtStatus {
    let k: [u8; 20] = unsafe { core::ptr::read_unaligned(key as *const [u8; 20]) };
    let v = if data.is_null() || size == 0 { Vec::new() } else { unsafe { core::slice::from_raw_parts(data, size as usize) }.to_vec() };
    with_device(pdo, |d| {
        d.props.retain(|(x, _, _)| *x != k);
        if kind != 0 && !v.is_empty() {
            d.props.push((k, kind, v));
        }
    })
    .map_or(0xC000_0010u32 as i32, |_| STATUS_SUCCESS)
}

/// The hive path of a device's key (IoOpenDeviceRegistryKey): DEVICE (1)
/// is its Enum key's Device Parameters, DRIVER (2) its Class key.
pub fn registry_key_of(dev: *mut u8, which: u32) -> Option<String> {
    with_device(dev, |d| match which & 3 {
        1 => Some(format!(r"{}\Device Parameters", device_key(d))),
        2 => Some(driver_key(d)),
        _ => None,
    })
    .flatten()
}

/// The device instance path ("PCI\VEN_...\3&...") of the stack `dev` is in.
pub fn instance_path(dev: *mut u8) -> Option<String> {
    with_device(dev, |d| format!("{}\\{}", d.device_id, d.instance))
}

/// Every PDO that has a record (DRIVER LIST, interface lookups).
pub fn pdos() -> Vec<usize> {
    DEVICES.lock().iter().map(|d| d.pdo).collect()
}
