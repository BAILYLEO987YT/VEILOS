// The DxgkCb* callbacks dxgkrnl gives a display driver in DXGKRNL_INTERFACE
// (DxgkDdiStartDevice). DeviceHandle is the adapter's id.
//
// W3 fills what display-only drivers use: device information, the POST
// display, PCI config space, mapping memory, DPCs and synchronizing with
// the interrupt, child status, the VidPN and monitor interfaces. The rest
// (allocations, GPU virtual memory, power components: W5 and W7) answer
// STATUS_NOT_SUPPORTED and are logged the first time.

use alloc::vec::Vec;
use spin::Mutex;

use super::{rd, with_adapter, wr, Display, STATUS_NOT_SUPPORTED, STATUS_UNSUCCESSFUL};
use crate::nt::ntos::{self, ke, mm, pool, NtStatus, STATUS_INVALID_PARAMETER, STATUS_NO_MEMORY, STATUS_SUCCESS};

// DXGKRNL_INTERFACE offsets (checked in k1's layout.c). 0x240 bytes.
pub const INTERFACE_SIZE: usize = 0x240;
const IF_DEVICE_HANDLE: usize = 0x08;

fn id_of(h: u64) -> usize {
    h as usize
}

/// DXGKRNL_INTERFACE for adapter `id`, in pool.
pub fn make_interface(id: usize) -> *mut u8 {
    let p = pool::alloc_tracked(INTERFACE_SIZE);
    if p.is_null() {
        return p;
    }
    let version = with_adapter(id, |a| u32::from_le_bytes(a.ddi[0..4].try_into().unwrap())).unwrap_or(0);
    let slots: &[(usize, *const ())] = &[
        (0x10, eval_acpi_method as *const ()),
        (0x18, get_device_information as *const ()),
        (0x20, indicate_child_status as *const ()),
        (0x28, map_memory as *const ()),
        (0x30, queue_dpc as *const ()),
        (0x38, query_services as *const ()),
        (0x40, read_device_space as *const ()),
        (0x48, synchronize_execution as *const ()),
        (0x50, unmap_memory as *const ()),
        (0x58, write_device_space as *const ()),
        (0x60, is_device_present as *const ()),
        (0x88, notify_dpc as *const ()),
        (0x90, super::vidpn::query_vidpn_interface as *const ()),
        (0x98, super::vidpn::query_monitor_interface as *const ()),
        (0xA8, log_etw_event as *const ()),
        (0xB0, exclude_adapter_access as *const ()),
        (0xC8, power_component as *const ()),
        (0xD0, power_component as *const ()),
        (0xD8, acquire_post_display_ownership as *const ()),
        (0xE8, power_component as *const ()),
        (0xF0, power_component as *const ()),
    ];
    unsafe {
        wr::<u32>(p, 0, INTERFACE_SIZE as u32);
        wr::<u32>(p, 4, version);
        wr::<u64>(p, IF_DEVICE_HANDLE, id as u64);
        // Everything not done yet answers STATUS_NOT_SUPPORTED.
        let mut off = 0x10;
        while off < INTERFACE_SIZE {
            wr::<u64>(p, off, not_supported as *const () as u64);
            off += 8;
        }
        for &(off, f) in slots {
            wr::<u64>(p, off, f as u64);
        }
    }
    p
}

/// Any callback VEILOS doesn't do yet.
extern "win64" fn not_supported(h: u64) -> NtStatus {
    static SEEN: Mutex<bool> = Mutex::new(false);
    let mut s = SEEN.lock();
    if !*s {
        *s = true;
        crate::serial_println!("[DXGK] adapter {}: a DxgkCb callback VEILOS doesn't have yet was called", h);
    }
    STATUS_NOT_SUPPORTED
}

extern "win64" fn eval_acpi_method(_h: u64, _uid: u32, _input: *const u8, _in_len: u32, _out: *mut u8, _out_len: u32) -> NtStatus {
    STATUS_NOT_SUPPORTED
}

// DXGK_DEVICE_INFO (0x50): MiniportDeviceContext 0, PhysicalDeviceObject 8,
// DeviceRegistryPath 0x10, TranslatedResourceList 0x20, SystemMemorySize
// 0x28, HighestPhysicalAddress 0x30, AgpApertureBase 0x38, AgpApertureSize
// 0x40, DockingState 0x48.
extern "win64" fn get_device_information(h: u64, info: *mut u8) -> NtStatus {
    let Some((ctx, pdo)) = with_adapter(id_of(h), |a| (a.ctx, a.pdo)) else { return STATUS_INVALID_PARAMETER };
    let translated = ntos::pnp::with_device(pdo as *mut u8, |d| d.translated).unwrap_or(0);
    let key = ntos::pnp::registry_key_of(pdo as *mut u8, 2).unwrap_or_default();
    let nt_key = alloc::format!("\\Registry\\Machine{}", key.trim_start_matches("HKLM"));
    let us = ntos::image::pool_unicode(&nt_key);
    let total = crate::mm::memory::total_frame_count() as u64 * 4096;
    unsafe {
        core::ptr::write_bytes(info, 0, 0x50);
        wr::<u64>(info, 0, ctx);
        wr::<u64>(info, 8, pdo as u64);
        core::ptr::copy_nonoverlapping(us, info.add(0x10), 16);
        wr::<u64>(info, 0x20, translated as u64);
        wr::<u64>(info, 0x28, total);
        wr::<u64>(info, 0x30, total.saturating_sub(1));
    }
    STATUS_SUCCESS
}

// DXGK_CHILD_STATUS: Type 0, ChildUid 4, HotPlug.Connected 8.
extern "win64" fn indicate_child_status(h: u64, status: *const u8) -> NtStatus {
    let (kind, uid, connected): (u32, u32, u8) = unsafe { (rd(status, 0), rd(status, 4), rd(status, 8)) };
    if kind == 1 {
        with_adapter(id_of(h), |a| {
            if let Some(c) = a.children.iter_mut().find(|c| c.uid == uid) {
                c.connected = connected != 0;
            }
        });
        crate::serial_println!("[DXGK] adapter {}: child {} {}", h, uid, if connected != 0 { "connected" } else { "disconnected" });
    }
    STATUS_SUCCESS
}

/// Mappings DxgkCbMapMemory made: (virtual address, length).
static MAPS: Mutex<Vec<(u64, u64)>> = Mutex::new(Vec::new());

/// DxgkCbMapMemory(DeviceHandle, TranslatedAddress, Length, InIoSpace,
/// MapToUserMode, CacheType, VirtualAddress*): device memory into system
/// space (I/O ports and user-mode mappings aren't supported).
extern "win64" fn map_memory(_h: u64, phys: u64, len: u32, io_space: u8, user: u8, cache: u32, out: *mut u64) -> NtStatus {
    if io_space != 0 || user != 0 || out.is_null() {
        return STATUS_INVALID_PARAMETER;
    }
    let va = mm::MmMapIoSpace(phys, len as usize, cache);
    if va == 0 {
        return STATUS_NO_MEMORY;
    }
    MAPS.lock().push((va, len as u64));
    unsafe { out.write(va) };
    STATUS_SUCCESS
}

extern "win64" fn unmap_memory(_h: u64, va: u64) -> NtStatus {
    let len = {
        let mut m = MAPS.lock();
        let at = m.iter().position(|x| x.0 == va);
        at.map(|i| m.remove(i).1)
    };
    match len {
        Some(l) => {
            mm::MmUnmapIoSpace(va, l as usize);
            STATUS_SUCCESS
        }
        None => STATUS_INVALID_PARAMETER,
    }
}

/// DxgkCbQueueDpc: the driver's DxgkDdiDpcRoutine, as a DPC.
extern "win64" fn queue_dpc(h: u64) -> u8 {
    let dpc = dpc_of(id_of(h));
    if dpc.is_null() {
        return 0;
    }
    ntos::dpc::KeInsertQueueDpc(dpc, 0, 0)
}

static DPCS: Mutex<Vec<(usize, usize)>> = Mutex::new(Vec::new());

fn dpc_of(id: usize) -> *mut u8 {
    let mut list = DPCS.lock();
    if let Some(&(_, d)) = list.iter().find(|x| x.0 == id) {
        return d as *mut u8;
    }
    let d = pool::alloc_tracked(0x40);
    if !d.is_null() {
        ntos::dpc::KeInitializeDpc(d, adapter_dpc as *const () as u64, id as u64);
        list.push((id, d as usize));
    }
    d
}

extern "win64" fn adapter_dpc(_dpc: *mut u8, id: u64, _a: u64, _b: u64) {
    let (f, ctx) = super::ddi(id as usize, super::DDI_DPC_ROUTINE);
    if f != 0 {
        let f: extern "win64" fn(u64) = unsafe { core::mem::transmute(f) };
        f(ctx);
    }
}

extern "win64" fn query_services(_h: u64, _kind: u32, _iface: *mut u8) -> NtStatus {
    STATUS_NOT_SUPPORTED
}

/// DxgkCbReadDeviceSpace(DeviceHandle, DataType, Buffer, Offset, Length,
/// BytesRead*): PCI configuration space (DXGK_WHICHSPACE_CONFIG, 0).
extern "win64" fn read_device_space(h: u64, kind: u32, buf: *mut u8, off: u32, len: u32, done: *mut u32) -> NtStatus {
    device_space(h, kind, buf, off, len, done, false)
}

extern "win64" fn write_device_space(h: u64, kind: u32, buf: *mut u8, off: u32, len: u32, done: *mut u32) -> NtStatus {
    device_space(h, kind, buf, off, len, done, true)
}

fn device_space(h: u64, kind: u32, buf: *mut u8, off: u32, len: u32, done: *mut u32, write: bool) -> NtStatus {
    if kind != 0 {
        return STATUS_NOT_SUPPORTED;
    }
    let Some(pdo) = with_adapter(id_of(h), |a| a.pdo) else { return STATUS_INVALID_PARAMETER };
    let Some(pci) = ntos::pnp::with_device(pdo as *mut u8, |d| d.pci.clone()) else { return STATUS_INVALID_PARAMETER };
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
    if !done.is_null() {
        unsafe { done.write(n) };
    }
    STATUS_SUCCESS
}

/// DxgkCbSynchronizeExecution(DeviceHandle, SynchronizeRoutine, Context,
/// MessageNumber, ReturnValue*): the routine with the device's interrupt
/// held off. Display-only drivers have no interrupt connected through
/// dxgkrnl, so this runs it at the highest driver IRQL.
extern "win64" fn synchronize_execution(_h: u64, routine: u64, context: u64, _message: u32, ret: *mut u8) -> NtStatus {
    if routine == 0 {
        return STATUS_INVALID_PARAMETER;
    }
    let old = ke::KfRaiseIrql(0x8);
    let f: extern "win64" fn(u64) -> u8 = unsafe { core::mem::transmute(routine) };
    let r = x86_64::instructions::interrupts::without_interrupts(|| f(context));
    ke::KeLowerIrql(old);
    if !ret.is_null() {
        unsafe { ret.write(r) };
    }
    STATUS_SUCCESS
}

extern "win64" fn is_device_present(_h: u64, _params: *const u8, present: *mut u8) -> NtStatus {
    if !present.is_null() {
        unsafe { present.write(0) };
    }
    STATUS_SUCCESS
}

extern "win64" fn notify_dpc(_h: u64) {}

extern "win64" fn log_etw_event(_guid: *const u8, _kind: u8, _size: u16, _data: *const u8) {}

/// DxgkCbExcludeAdapterAccess(DeviceHandle, Attributes, Callback, Context):
/// nothing else touches the adapter here, so the callback just runs.
extern "win64" fn exclude_adapter_access(_h: u64, _attrs: u32, callback: u64, context: u64) -> NtStatus {
    if callback == 0 {
        return STATUS_INVALID_PARAMETER;
    }
    let f: extern "win64" fn(u64) = unsafe { core::mem::transmute(callback) };
    f(context);
    STATUS_SUCCESS
}

/// Power components (runtime power, W7): accepted.
extern "win64" fn power_component(_h: u64, _component: u32) -> NtStatus {
    STATUS_SUCCESS
}

// DXGK_DISPLAY_INFORMATION (0x20): Width 0, Height 4, Pitch 8, ColorFormat
// 0xC, PhysicAddress 0x10, TargetId 0x18, AcpiId 0x1C.

pub fn display_from(b: &[u8]) -> Display {
    let u = |o: usize| u32::from_le_bytes(b[o..o + 4].try_into().unwrap());
    Display { width: u(0), height: u(4), pitch: u(8), format: u(0xC), phys: u64::from_le_bytes(b[0x10..0x18].try_into().unwrap()), target: u(0x18) }
}

/// DxgkCbAcquirePostDisplayOwnership(DeviceHandle, DisplayInfo*): the mode
/// and framebuffer VEILOS shows on this adapter, if it is the POST adapter
/// (the firmware's, or one VEILOS's own display driver set up). Width 0
/// when it isn't, as Windows answers.
extern "win64" fn acquire_post_display_ownership(h: u64, info: *mut u8) -> NtStatus {
    let id = id_of(h);
    if info.is_null() {
        return STATUS_INVALID_PARAMETER;
    }
    unsafe { core::ptr::write_bytes(info, 0, 0x20) };
    let Some(ranges) = with_adapter(id, |a| a.memory.clone()) else { return STATUS_UNSUCCESSFUL };
    let Some(fb) = crate::gfx::graphics::head_framebuffer(&ranges) else {
        crate::serial_println!("[DXGK] adapter {}: not the POST adapter (no VEILOS head on its memory)", id);
        return STATUS_SUCCESS;
    };
    let d = Display {
        width: fb.width as u32,
        height: fb.height as u32,
        pitch: (fb.stride * 4) as u32,
        format: super::D3DDDIFMT_X8R8G8B8,
        phys: fb.base_addr,
        target: 0,
    };
    unsafe {
        wr::<u32>(info, 0, d.width);
        wr::<u32>(info, 4, d.height);
        wr::<u32>(info, 8, d.pitch);
        wr::<u32>(info, 0xC, d.format);
        wr::<u64>(info, 0x10, d.phys);
    }
    with_adapter(id, |a| a.post = Some(d));
    crate::serial_println!("[DXGK] adapter {}: POST display {}x{} pitch {} at {:#x} handed to the driver", id, d.width, d.height, d.pitch, d.phys);
    STATUS_SUCCESS
}
