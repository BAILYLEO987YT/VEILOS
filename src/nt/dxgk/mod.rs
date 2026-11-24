// =============================================================================
// VEILOS: dxgkrnl, the display port driver (WDDM_DESIGN.md, stage W3)
// =============================================================================
//
// What Windows display drivers register with and are driven by. A driver
// links displib.lib from the WDK, whose DxgkInitialize /
// DxgkInitializeDisplayOnlyDriver load the "DXGKrnl" service (built in
// here), open \Device\DxgKrnl and ask it, with internal IOCTL 0x230047 (full
// WDDM; 0x23003F before Windows 8) or 0x230043 (display-only), for the
// address of dxgkrnl's own initialize routine, which they call with the
// driver's table of DxgkDdi* functions (`dpi_initialize*`).
//
// From then on dxgkrnl owns the driver's PnP: its AddDevice makes the
// adapter's device object and calls DxgkDdiAddDevice; START_DEVICE calls
// DxgkDdiStartDevice with the DXGKRNL_INTERFACE of DxgkCb* callbacks
// (cb.rs), then brings the display up (`bring_up`): child devices and their
// status, the monitor's EDID and modes, a VidPN built and committed through
// the VidPN objects (vidpn.rs), and the source made visible. The desktop
// then presents through DxgkDdiPresentDisplayOnly (`present`).
//
// Everything is per adapter (`Adapter`): several display drivers, of one
// make or several, can each run their own adapters (WDDM_DESIGN.md, Several
// GPUs). Only the POST adapter, the one whose framebuffer VEILOS draws on at
// boot, is handed over (DxgkCbAcquirePostDisplayOwnership).
//
// W3 covers display-only drivers (KMDDOD_INITIALIZATION_DATA). A full WDDM
// driver's table (DRIVER_INITIALIZATION_DATA) is accepted and its adapter
// started, but rendering (VidMm, VidSch) is W5.

pub mod cb;
pub mod vidpn;

use alloc::{format, string::String, vec, vec::Vec};
use spin::Mutex;

use crate::nt::ntos::{self, io, ke, pool, NtStatus, STATUS_INVALID_PARAMETER, STATUS_NO_MEMORY, STATUS_SUCCESS};

pub const STATUS_NOT_SUPPORTED: NtStatus = 0xC000_00BBu32 as i32;
pub const STATUS_UNSUCCESSFUL: NtStatus = 0xC000_0001u32 as i32;
pub const STATUS_MONITOR_NO_DESCRIPTOR: NtStatus = 0xC01D_0001u32 as i32;

// Internal IOCTLs displib sends to \Device\DxgKrnl.
const IOCTL_VIDEO_DDI_FUNC_REGISTER: u32 = 0x23_003F;
const IOCTL_VIDEO_DDI_FUNC_REGISTER_WIN8: u32 = 0x23_0047;
const IOCTL_VIDEO_KMDOD_DDI_REGISTER: u32 = 0x23_0043;
const IOCTL_VIDEO_DDI_UNINITIALIZE: u32 = 0x23_004F;

// KMDDOD_INITIALIZATION_DATA (checked in k1's layout.c). The full WDDM
// table (DRIVER_INITIALIZATION_DATA) shares its first entries.
pub const DDI_VERSION: usize = 0x00;
pub const DDI_ADD_DEVICE: usize = 0x08;
pub const DDI_START_DEVICE: usize = 0x10;
pub const DDI_STOP_DEVICE: usize = 0x18;
pub const DDI_REMOVE_DEVICE: usize = 0x20;
pub const DDI_INTERRUPT_ROUTINE: usize = 0x30;
pub const DDI_DPC_ROUTINE: usize = 0x38;
pub const DDI_QUERY_CHILD_RELATIONS: usize = 0x40;
pub const DDI_QUERY_CHILD_STATUS: usize = 0x48;
pub const DDI_QUERY_DEVICE_DESCRIPTOR: usize = 0x50;
pub const DDI_SET_POWER_STATE: usize = 0x58;
pub const DDI_UNLOAD: usize = 0x70;
pub const DDI_QUERY_ADAPTER_INFO: usize = 0x88;
pub const DDI_IS_SUPPORTED_VIDPN: usize = 0xB8;
pub const DDI_ENUM_VIDPN_COFUNC_MODALITY: usize = 0xC8;
pub const DDI_SET_VIDPN_SOURCE_VISIBILITY: usize = 0xD0;
pub const DDI_COMMIT_VIDPN: usize = 0xD8;
pub const DDI_RECOMMEND_MONITOR_MODES: usize = 0xE8;
pub const DDI_PRESENT_DISPLAY_ONLY: usize = 0x100;
pub const DDI_STOP_AND_RELEASE_POST: usize = 0x108;
pub const KMDDOD_SIZE: usize = 0x150;
/// How much of a full WDDM driver's table is kept (it is longer than the
/// display-only one; the rest is W5's).
const FULL_INIT_KEEP: usize = 0x400;

/// A display adapter a Windows display driver runs.
pub struct Adapter {
    pub id: usize,
    pub display_only: bool,
    /// The driver's DxgkDdi* table (a copy).
    pub ddi: Vec<u8>,
    pub driver_object: usize,
    pub pdo: usize,
    /// dxgkrnl's device object for the adapter, attached above the PDO.
    pub fdo: usize,
    pub lower: usize,
    /// The driver's MiniportDeviceContext.
    pub ctx: u64,
    /// The DXGKRNL_INTERFACE given to DxgkDdiStartDevice (pool).
    pub interface: usize,
    pub sources: u32,
    pub children: Vec<Child>,
    /// The POST display, if the driver acquired it: what VEILOS showed.
    pub post: Option<Display>,
    /// The committed mode of source 0, and the head showing it.
    pub mode: Option<Display>,
    pub head: Option<usize>,
    /// Memory ranges of the device (from its resources): which head is its.
    pub memory: Vec<(u64, u64)>,
    pub presents: u64,
    pub started: bool,
    pub log: Vec<String>,
}

unsafe impl Send for Adapter {}

#[derive(Clone, Copy, Debug)]
pub struct Child {
    pub uid: u32,
    pub kind: u32,
    pub connected: bool,
}

/// A display mode as DXGK_DISPLAY_INFORMATION describes one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Display {
    pub width: u32,
    pub height: u32,
    /// Bytes per row.
    pub pitch: u32,
    /// D3DDDIFORMAT (22: X8R8G8B8).
    pub format: u32,
    pub phys: u64,
    pub target: u32,
}

pub const D3DDDIFMT_A8R8G8B8: u32 = 21;
pub const D3DDDIFMT_X8R8G8B8: u32 = 22;

pub static ADAPTERS: Mutex<Vec<Adapter>> = Mutex::new(Vec::new());
/// Drivers registered with dxgkrnl: (DRIVER_OBJECT, display-only, table).
static DRIVERS: Mutex<Vec<(usize, bool, Vec<u8>)>> = Mutex::new(Vec::new());
static DXGK_DEVICE: Mutex<usize> = Mutex::new(0);

pub(crate) unsafe fn rd<T: Copy>(p: *const u8, off: usize) -> T {
    unsafe { (p.add(off) as *const T).read_unaligned() }
}
pub(crate) unsafe fn wr<T>(p: *mut u8, off: usize, v: T) {
    unsafe { (p.add(off) as *mut T).write_unaligned(v) }
}

/// Run `f` on adapter `id`.
pub fn with_adapter<R>(id: usize, f: impl FnOnce(&mut Adapter) -> R) -> Option<R> {
    ADAPTERS.lock().iter_mut().find(|a| a.id == id).map(f)
}

/// A DxgkDdi entry of adapter `id`'s driver (0 if it has none).
pub fn ddi(id: usize, off: usize) -> (u64, u64) {
    with_adapter(id, |a| {
        let f = if off + 8 <= a.ddi.len() { u64::from_le_bytes(a.ddi[off..off + 8].try_into().unwrap()) } else { 0 };
        (f, a.ctx)
    })
    .unwrap_or((0, 0))
}

pub(crate) fn note(id: usize, s: String) {
    crate::serial_println!("[DXGK] adapter {}: {}", id, s);
    with_adapter(id, |a| a.log.push(s));
}

// -----| \Device\DxgKrnl |-----

/// Make \Device\DxgKrnl (at boot, after ntos::init).
pub fn init() {
    let drv = pool::alloc_tracked(io::DRV_SIZE);
    let ext = pool::alloc_tracked(0x80);
    if drv.is_null() || ext.is_null() {
        return;
    }
    unsafe {
        wr::<u16>(drv, 0, 4);
        wr::<u16>(drv, 2, io::DRV_SIZE as u16);
        wr(drv, io::DRV_EXTENSION, ext);
        wr(ext, 0, drv);
        for k in 0..=io::IRP_MJ_MAXIMUM_FUNCTION {
            wr::<u64>(drv, io::DRV_MAJOR_FUNCTION + 8 * k, dxgk_dispatch as *const () as u64);
        }
    }
    let name = ntos::image::pool_unicode("\\Device\\DxgKrnl");
    let mut dev: *mut u8 = core::ptr::null_mut();
    if io::IoCreateDevice(drv, 0, name, 0x23 /* FILE_DEVICE_VIDEO */, 0, 0, &mut dev) >= 0 {
        unsafe { wr::<u32>(dev, io::DEV_FLAGS, rd::<u32>(dev, io::DEV_FLAGS) & !io::DO_DEVICE_INITIALIZING) };
        *DXGK_DEVICE.lock() = dev as usize;
    }
}

fn complete(irp: *mut u8, status: NtStatus, info: u64) -> NtStatus {
    unsafe {
        wr(irp, io::IRP_STATUS, status);
        wr(irp, io::IRP_INFORMATION, info);
    }
    io::IofCompleteRequest(irp, 0);
    status
}

/// \Device\DxgKrnl's requests: opens, and the registration IOCTLs.
extern "win64" fn dxgk_dispatch(_dev: *mut u8, irp: *mut u8) -> NtStatus {
    let sl: *mut u8 = unsafe { rd(irp, io::IRP_CURRENT_STACK) };
    let major: u8 = unsafe { rd(sl, 0) };
    match major {
        io::IRP_MJ_CREATE | io::IRP_MJ_CLOSE | io::IRP_MJ_CLEANUP => complete(irp, STATUS_SUCCESS, 0),
        io::IRP_MJ_INTERNAL_DEVICE_CONTROL => {
            let code: u32 = unsafe { rd(sl, io::SL_IOCTL_CODE) };
            let out_len: u32 = unsafe { rd(sl, io::SL_OUTPUT_LENGTH) };
            let out: *mut u64 = unsafe { rd(irp, io::IRP_USER_BUFFER) };
            let f: u64 = match code {
                IOCTL_VIDEO_DDI_FUNC_REGISTER | IOCTL_VIDEO_DDI_FUNC_REGISTER_WIN8 => dpi_initialize as *const () as u64,
                IOCTL_VIDEO_KMDOD_DDI_REGISTER => dpi_initialize_display_only as *const () as u64,
                IOCTL_VIDEO_DDI_UNINITIALIZE => return complete(irp, STATUS_SUCCESS, 0),
                _ => return complete(irp, ntos::STATUS_INVALID_DEVICE_REQUEST, 0),
            };
            if out.is_null() || out_len < 8 {
                return complete(irp, STATUS_INVALID_PARAMETER, 0);
            }
            unsafe { out.write_unaligned(f) };
            complete(irp, STATUS_SUCCESS, 8)
        }
        _ => complete(irp, ntos::STATUS_INVALID_DEVICE_REQUEST, 0),
    }
}

/// Whether `drv` registered with dxgkrnl (pnp.rs: such a driver may take a
/// display adapter from VEILOS).
pub fn is_display_driver(drv: *mut u8) -> bool {
    DRIVERS.lock().iter().any(|d| d.0 == drv as usize)
}

/// dxgkrnl's initialize routine for full WDDM drivers:
/// (DriverObject, RegistryPath, DRIVER_INITIALIZATION_DATA*).
extern "win64" fn dpi_initialize(drv: *mut u8, _reg: *const u8, init: *const u8) -> NtStatus {
    register(drv, init, false)
}

/// ... and for display-only drivers: (DriverObject, RegistryPath,
/// KMDDOD_INITIALIZATION_DATA*).
extern "win64" fn dpi_initialize_display_only(drv: *mut u8, _reg: *const u8, init: *const u8) -> NtStatus {
    register(drv, init, true)
}

fn register(drv: *mut u8, init: *const u8, display_only: bool) -> NtStatus {
    if drv.is_null() || init.is_null() {
        return STATUS_INVALID_PARAMETER;
    }
    let len = if display_only { KMDDOD_SIZE } else { FULL_INIT_KEEP };
    let table = unsafe { core::slice::from_raw_parts(init, len) }.to_vec();
    let version = u32::from_le_bytes(table[0..4].try_into().unwrap());
    crate::serial_println!(
        "[DXGK] {} registered: {}, DDI version {:#x}",
        ntos::image::name_of(drv).unwrap_or_default(),
        if display_only { "display-only" } else { "full WDDM" },
        version
    );
    DRIVERS.lock().push((drv as usize, display_only, table));
    // dxgkrnl takes the driver's PnP: its AddDevice, its dispatch routines
    // and its unload.
    unsafe {
        let ext: *mut u8 = rd(drv, io::DRV_EXTENSION);
        wr::<u64>(ext, 8, dpi_add_device as *const () as u64);
        for k in 0..=io::IRP_MJ_MAXIMUM_FUNCTION {
            wr::<u64>(drv, io::DRV_MAJOR_FUNCTION + 8 * k, dpi_dispatch as *const () as u64);
        }
        wr::<u64>(drv, io::DRV_UNLOAD, dpi_unload as *const () as u64);
    }
    STATUS_SUCCESS
}

/// AddDevice for a display driver: dxgkrnl's device object for the adapter,
/// attached above the PDO, then DxgkDdiAddDevice(PDO, &MiniportDeviceContext).
extern "win64" fn dpi_add_device(drv: *mut u8, pdo: *mut u8) -> NtStatus {
    let Some((display_only, table)) = DRIVERS.lock().iter().find(|d| d.0 == drv as usize).map(|d| (d.1, d.2.clone())) else {
        return STATUS_INVALID_PARAMETER;
    };
    let id = {
        static NEXT: Mutex<usize> = Mutex::new(0);
        let mut n = NEXT.lock();
        *n += 1;
        *n
    };
    let name = ntos::image::pool_unicode(&format!("\\Device\\Video{}", id - 1));
    let mut fdo: *mut u8 = core::ptr::null_mut();
    let st = io::IoCreateDevice(drv, 0, name, 0x23, 0, 0, &mut fdo);
    if st < 0 {
        return st;
    }
    let lower = io::IoAttachDeviceToDeviceStack(fdo, pdo);
    ADAPTERS.lock().push(Adapter {
        id,
        display_only,
        ddi: table,
        driver_object: drv as usize,
        pdo: pdo as usize,
        fdo: fdo as usize,
        lower: lower as usize,
        ctx: 0,
        interface: 0,
        sources: 0,
        children: Vec::new(),
        post: None,
        mode: None,
        head: None,
        memory: Vec::new(),
        presents: 0,
        started: false,
        log: Vec::new(),
    });
    let (add, _) = ddi(id, DDI_ADD_DEVICE);
    let mut ctx: u64 = 0;
    let st = if add == 0 {
        STATUS_INVALID_PARAMETER
    } else {
        let f: extern "win64" fn(*mut u8, *mut u64) -> NtStatus = unsafe { core::mem::transmute(add) };
        f(pdo, &mut ctx)
    };
    ke::set_irql(0);
    if st < 0 {
        ADAPTERS.lock().retain(|a| a.id != id);
        io::IoDetachDevice(lower);
        io::IoDeleteDevice(fdo);
        return st;
    }
    with_adapter(id, |a| a.ctx = ctx);
    unsafe { wr::<u32>(fdo, io::DEV_FLAGS, rd::<u32>(fdo, io::DEV_FLAGS) & !io::DO_DEVICE_INITIALIZING) };
    note(id, format!("DxgkDdiAddDevice -> context {:#x}", ctx));
    STATUS_SUCCESS
}

fn adapter_of_fdo(fdo: *mut u8) -> Option<usize> {
    ADAPTERS.lock().iter().find(|a| a.fdo == fdo as usize).map(|a| a.id)
}

/// Pass the request to the PDO below unchanged.
fn pass_down(lower: usize, irp: *mut u8) -> NtStatus {
    unsafe {
        // IoSkipCurrentIrpStackLocation.
        wr::<i8>(irp, io::IRP_CURRENT_LOCATION, rd::<i8>(irp, io::IRP_CURRENT_LOCATION) + 1);
        wr(irp, io::IRP_CURRENT_STACK, rd::<*mut u8>(irp, io::IRP_CURRENT_STACK).add(io::STACK_SIZE));
    }
    io::IofCallDriver(lower as *mut u8, irp)
}

/// The display driver's requests, as dxgkrnl handles them.
extern "win64" fn dpi_dispatch(dev: *mut u8, irp: *mut u8) -> NtStatus {
    let Some(id) = adapter_of_fdo(dev) else {
        // A device the driver made itself (none expected): fail it.
        return complete(irp, ntos::STATUS_INVALID_DEVICE_REQUEST, 0);
    };
    let lower = with_adapter(id, |a| a.lower).unwrap_or(0);
    let sl: *mut u8 = unsafe { rd(irp, io::IRP_CURRENT_STACK) };
    let (major, minor): (u8, u8) = unsafe { (rd(sl, 0), rd(sl, 1)) };
    match major {
        io::IRP_MJ_CREATE | io::IRP_MJ_CLOSE | io::IRP_MJ_CLEANUP => complete(irp, STATUS_SUCCESS, 0),
        ntos::pnp::IRP_MJ_PNP => match minor {
            ntos::pnp::IRP_MN_START_DEVICE => {
                io::IoForwardIrpSynchronously(lower as *mut u8, irp);
                let mut st: NtStatus = unsafe { rd(irp, io::IRP_STATUS) };
                if st >= 0 {
                    let translated: *mut u8 = unsafe { rd(sl, io::SL_PARAMETERS + 8) };
                    st = start(id, translated);
                }
                complete(irp, st, 0)
            }
            ntos::pnp::IRP_MN_REMOVE_DEVICE => {
                stop(id);
                unsafe { wr(irp, io::IRP_STATUS, STATUS_SUCCESS) };
                let st = pass_down(lower, irp);
                remove(id);
                st
            }
            ntos::pnp::IRP_MN_SURPRISE_REMOVAL | ntos::pnp::IRP_MN_STOP_DEVICE | ntos::pnp::IRP_MN_QUERY_REMOVE_DEVICE | 0x03 | 0x05 | 0x06 => {
                unsafe { wr(irp, io::IRP_STATUS, STATUS_SUCCESS) };
                pass_down(lower, irp)
            }
            _ => pass_down(lower, irp),
        },
        ntos::pnp::IRP_MJ_POWER | ntos::pnp::IRP_MJ_SYSTEM_CONTROL => pass_down(lower, irp),
        _ => complete(irp, ntos::STATUS_INVALID_DEVICE_REQUEST, 0),
    }
}

/// The driver unloads: DxgkDdiUnload, then forget its registration.
extern "win64" fn dpi_unload(drv: *mut u8) {
    let table = DRIVERS.lock().iter().find(|d| d.0 == drv as usize).map(|d| d.2.clone());
    if let Some(t) = table {
        let f = u64::from_le_bytes(t[DDI_UNLOAD..DDI_UNLOAD + 8].try_into().unwrap());
        if f != 0 {
            let f: extern "win64" fn() = unsafe { core::mem::transmute(f) };
            f();
            ke::set_irql(0);
        }
    }
    DRIVERS.lock().retain(|d| d.0 != drv as usize);
    crate::serial_println!("[DXGK] {} unloaded", ntos::image::name_of(drv).unwrap_or_default());
}

// -----| starting an adapter |-----

/// DXGK_START_INFO: RequiredDmaQueueEntry 0, AdapterGuid 4, AdapterLuid 0x14.
const START_INFO_SIZE: usize = 0x1C;

fn start(id: usize, translated: *mut u8) -> NtStatus {
    // The device's memory ranges, to find the head it shows.
    let memory = memory_ranges(translated);
    with_adapter(id, |a| a.memory = memory);
    let interface = cb::make_interface(id);
    if interface.is_null() {
        return STATUS_NO_MEMORY;
    }
    with_adapter(id, |a| a.interface = interface as usize);
    let mut info = [0u8; START_INFO_SIZE];
    unsafe {
        // An adapter GUID and LUID of its own.
        let mut g = [0u8; 16];
        crate::security::entropy::fill(&mut g);
        core::ptr::copy_nonoverlapping(g.as_ptr(), info.as_mut_ptr().add(4), 16);
        wr::<u32>(info.as_mut_ptr(), 0x14, 0x1000 + id as u32);
    }
    let (f, ctx) = ddi(id, DDI_START_DEVICE);
    let (mut sources, mut children) = (0u32, 0u32);
    let f: extern "win64" fn(u64, *mut u8, *mut u8, *mut u32, *mut u32) -> NtStatus = unsafe { core::mem::transmute(f) };
    let st = f(ctx, info.as_mut_ptr(), interface, &mut sources, &mut children);
    ke::set_irql(0);
    note(id, format!("DxgkDdiStartDevice -> {:#010x}: {} source(s), {} child(ren)", st as u32, sources, children));
    if st < 0 {
        return st;
    }
    with_adapter(id, |a| {
        a.sources = sources;
        a.started = true;
    });
    bring_up(id, children);
    STATUS_SUCCESS
}

/// Memory resources (CmResourceTypeMemory) of a translated resource list.
fn memory_ranges(list: *const u8) -> Vec<(u64, u64)> {
    let mut out = Vec::new();
    if list.is_null() {
        return out;
    }
    unsafe {
        let n: u32 = rd(list, 0x10);
        for i in 0..n as usize {
            let d = list.add(0x14 + i * 0x14);
            if rd::<u8>(d, 0) == 3 {
                out.push((rd::<u64>(d, 4), rd::<u32>(d, 0xC) as u64));
            }
        }
    }
    out
}

// DXGK_CHILD_DESCRIPTOR (0x1C): ChildDeviceType 0, ChildCapabilities 4
// (HpdAwareness 0x10), AcpiUid 0x14, ChildUid 0x18.
const CHILD_SIZE: usize = 0x1C;
// DXGK_CHILD_STATUS (0x10): Type 0, ChildUid 4, HotPlug.Connected 8.
const STATUS_CONNECTION: u32 = 1;

/// After DxgkDdiStartDevice: children, monitors, a committed VidPN, and the
/// desktop on it.
fn bring_up(id: usize, children: u32) {
    // Children: the outputs and what is plugged into them.
    let n = children.max(1) as usize + 1;
    let mut buf = vec![0u8; n * CHILD_SIZE];
    let (f, ctx) = ddi(id, DDI_QUERY_CHILD_RELATIONS);
    let mut found = Vec::new();
    if f != 0 {
        let f: extern "win64" fn(u64, *mut u8, u32) -> NtStatus = unsafe { core::mem::transmute(f) };
        let st = f(ctx, buf.as_mut_ptr(), buf.len() as u32);
        ke::set_irql(0);
        if st >= 0 {
            for c in buf.chunks_exact(CHILD_SIZE) {
                let kind = u32::from_le_bytes(c[0..4].try_into().unwrap());
                if kind == 0 {
                    break;
                }
                found.push(Child { uid: u32::from_le_bytes(c[0x18..0x1C].try_into().unwrap()), kind, connected: false });
            }
        }
        note(id, format!("DxgkDdiQueryChildRelations -> {:#010x}: {} child(ren)", st as u32, found.len()));
    }
    let (f, ctx) = ddi(id, DDI_QUERY_CHILD_STATUS);
    for c in found.iter_mut() {
        if f == 0 {
            break;
        }
        let mut s = [0u8; 0x10];
        unsafe {
            wr::<u32>(s.as_mut_ptr(), 0, STATUS_CONNECTION);
            wr::<u32>(s.as_mut_ptr(), 4, c.uid);
        }
        let f: extern "win64" fn(u64, *mut u8, u8) -> NtStatus = unsafe { core::mem::transmute(f) };
        let st = f(ctx, s.as_mut_ptr(), 0);
        ke::set_irql(0);
        c.connected = st >= 0 && s[8] != 0;
    }
    with_adapter(id, |a| a.children = found.clone());
    let Some(target) = found.iter().find(|c| c.kind == 1 && c.connected).map(|c| c.uid) else {
        note(id, String::from("no connected video output: nothing to show"));
        return;
    };

    // The monitor: its EDID's preferred mode, if the driver has one.
    let edid = vidpn::query_edid(id, target);
    // The mode to show: the POST display's, if the driver took it over (the
    // desktop's size can't change after boot), else the EDID's.
    let post = with_adapter(id, |a| a.post).flatten();
    let want = post.map(|p| (p.width, p.height)).or(edid.map(|e| (e.0, e.1)));
    note(id, format!("monitor on target {}: EDID {}, showing {:?}", target,
        match edid { Some((w, h)) => format!("prefers {}x{}", w, h), None => String::from("none") }, want));
    let Some((w, h)) = want else {
        note(id, String::from("no mode to show"));
        return;
    };
    match vidpn::commit(id, target, w, h) {
        Ok(()) => {}
        Err(why) => {
            note(id, format!("VidPN not committed: {}", why));
            return;
        }
    }
    // The source visible.
    let (f, ctx) = ddi(id, DDI_SET_VIDPN_SOURCE_VISIBILITY);
    if f != 0 {
        let mut v = [0u8; 8];
        v[4] = 1;
        let f: extern "win64" fn(u64, *const u8) -> NtStatus = unsafe { core::mem::transmute(f) };
        let st = f(ctx, v.as_ptr());
        ke::set_irql(0);
        note(id, format!("DxgkDdiSetVidPnSourceVisibility(0, visible) -> {:#010x}", st as u32));
    }
    // The desktop now presents through the driver.
    let display_only = with_adapter(id, |a| a.display_only).unwrap_or(false);
    if display_only {
        take_head(id);
        present_all(id);
    }
}

/// Make the head this adapter shows present through the driver.
fn take_head(id: usize) {
    if with_adapter(id, |a| a.head.is_some()).unwrap_or(true) {
        return;
    }
    let ranges = with_adapter(id, |a| a.memory.clone()).unwrap_or_default();
    let post = with_adapter(id, |a| a.post).flatten();
    match crate::gfx::graphics::hand_over(id, &ranges, post.map(|p| p.phys)) {
        Some((head, _)) => {
            with_adapter(id, |a| a.head = Some(head));
            note(id, format!("the desktop's head {} now presents through the driver", head));
        }
        None => note(id, String::from("no VEILOS head shows this adapter's memory")),
    }
}

/// Present the whole of the adapter's head (after taking it over).
fn present_all(id: usize) {
    if let Some(m) = with_adapter(id, |a| a.mode).flatten() {
        present(id, 0, 0, m.width as usize, m.height as usize);
    }
}

// DXGKARG_PRESENT_DISPLAYONLY (0x40): VidPnSourceId 0, pSource 8,
// BytesPerPixel 0x10, Pitch 0x14, Flags 0x18, NumMoves 0x1C, pMoves 0x20,
// NumDirtyRects 0x28, pDirtyRect 0x30.

/// The desktop changed [x, x+w) x [y, y+h) of the head adapter `id` shows
/// (head coordinates): DxgkDdiPresentDisplayOnly from its system-memory
/// copy. Called with the heads locked (graphics.rs), so this mustn't lock
/// them.
pub fn present(id: usize, x: usize, y: usize, w: usize, h: usize) {
    let Some((src, stride)) = crate::gfx::graphics::shadow_of(id) else { return };
    let (f, ctx) = ddi(id, DDI_PRESENT_DISPLAY_ONLY);
    if f == 0 || w == 0 || h == 0 {
        return;
    }
    let rect: [i32; 4] = [x as i32, y as i32, (x + w) as i32, (y + h) as i32];
    let mut args = [0u8; 0x40];
    unsafe {
        let p = args.as_mut_ptr();
        wr::<u32>(p, 0, 0);
        wr::<u64>(p, 8, src);
        wr::<u32>(p, 0x10, 4);
        wr::<u32>(p, 0x14, (stride * 4) as u32);
        wr::<u32>(p, 0x28, 1);
        wr::<u64>(p, 0x30, rect.as_ptr() as u64);
    }
    let f: extern "win64" fn(u64, *const u8) -> NtStatus = unsafe { core::mem::transmute(f) };
    let irql = ke::irql();
    let _ = ntos::with_kernel_gs(|| f(ctx, args.as_ptr()));
    ke::set_irql(irql);
    if let Some(mut list) = ADAPTERS.try_lock() {
        if let Some(a) = list.iter_mut().find(|a| a.id == id) {
            a.presents += 1;
        }
    }
}

// -----| stopping |-----

fn stop(id: usize) {
    let (display_only, started) = with_adapter(id, |a| (a.display_only, a.started)).unwrap_or((false, false));
    if !started {
        return;
    }
    if display_only {
        // The driver hands the display back: what it shows now.
        let (f, ctx) = ddi(id, DDI_STOP_AND_RELEASE_POST);
        let mut back: Option<Display> = None;
        if f != 0 {
            let mut info = [0u8; 0x20];
            let f: extern "win64" fn(u64, u32, *mut u8) -> NtStatus = unsafe { core::mem::transmute(f) };
            let st = f(ctx, 0, info.as_mut_ptr());
            ke::set_irql(0);
            if st >= 0 {
                back = Some(cb::display_from(&info));
            }
            note(id, format!("DxgkDdiStopDeviceAndReleasePostDisplayOwnership -> {:#010x}, {:?}", st as u32, back));
        }
        crate::gfx::graphics::give_back(id, back.filter(|d| d.width != 0));
    } else {
        let (f, ctx) = ddi(id, DDI_STOP_DEVICE);
        if f != 0 {
            let f: extern "win64" fn(u64) -> NtStatus = unsafe { core::mem::transmute(f) };
            let _ = f(ctx);
            ke::set_irql(0);
        }
    }
    with_adapter(id, |a| {
        a.started = false;
        a.head = None;
    });
}

fn remove(id: usize) {
    let (f, ctx) = ddi(id, DDI_REMOVE_DEVICE);
    if f != 0 {
        let f: extern "win64" fn(u64) -> NtStatus = unsafe { core::mem::transmute(f) };
        let _ = f(ctx);
        ke::set_irql(0);
    }
    let a = {
        let mut list = ADAPTERS.lock();
        let at = list.iter().position(|a| a.id == id);
        at.map(|i| list.remove(i))
    };
    if let Some(a) = a {
        pool::free_tracked(a.interface as *mut u8);
        io::IoDetachDevice(a.lower as *mut u8);
        io::IoDeleteDevice(a.fdo as *mut u8);
        crate::serial_println!("[DXGK] adapter {} removed after {} present(s)", id, a.presents);
    }
}

/// One line per adapter (the terminal's DRIVER command).
pub fn describe() -> Vec<String> {
    ADAPTERS
        .lock()
        .iter()
        .map(|a| {
            format!(
                "  adapter {} ({}): {}, mode {}, {} present(s)",
                a.id,
                ntos::image::name_of(a.driver_object as *mut u8).unwrap_or_default(),
                if a.display_only { "display-only" } else { "full WDDM" },
                match a.mode {
                    Some(m) => format!("{}x{}", m.width, m.height),
                    None => String::from("none"),
                },
                a.presents
            )
        })
        .collect()
}
