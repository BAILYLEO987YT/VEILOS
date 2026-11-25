// =============================================================================
// VEILOS: Windows kernel drivers (WDDM_DESIGN.md, stages W1 and W2)
// =============================================================================
//
// The kernel interface Windows drivers are written against, implemented by
// VEILOS, so a `.sys` file can be loaded into ring 0 and run:
//
//   * `image`: map a driver image, apply its relocations, bind its imports
//     against the exports here (by name; an import nothing provides refuses
//     the load and is listed), set its /GS cookie, call DriverEntry.
//   * `pool`: kernel pool (ExAllocatePool*), in the upper half of the
//     address space like Windows' (drivers treat low addresses as user
//     memory), with the images beside it.
//   * `ke`: IRQL (CR8, as Windows: KeGetCurrentIrql and friends are inline
//     CR8 accesses in the WDK headers, so drivers write it themselves),
//     spinlocks, events and waits, time.
//   * `io`: DRIVER_OBJECT, DEVICE_OBJECT, IRPs and their stack locations,
//     IoCallDriver / IoCompleteRequest, device names and symbolic links.
//   * `rtl`: strings, version, the C runtime functions ntoskrnl exports,
//     DbgPrint (to the serial log).
//   * `dpc`: DPCs and timers; `ps`: system threads and work items.
//   * `seh`: __try/__except/__finally in drivers: their CPU faults and
//     raised exceptions dispatched through their unwind tables.
//   * `mm`: MDLs, contiguous memory, device memory, physical addresses.
//   * `ob`: kernel handles; `cm`: the registry (Zw*Key on nt_registry).
//   * `crt`, `rtlx`: the C runtime and run-time library beyond the basics.
//   * `ex`: resources, fast mutexes, SLists, lookaside lists, callbacks,
//     Ex timers; `misc`: queues, Ps, Po, Cm, Etw and Mm odds and ends.
//   * `zw`: files on the VFS, devices, events, links, registry enumeration,
//     system information, ZwLoadDriver.
//   * `pnp`: Plug and Play: PCI devices handed to a driver's AddDevice
//     (a PDO each, from VEILOS's own PCI bus driver) and started with
//     their resources; `intr`: their interrupts (a vector per device, MSI)
//     and IoConnectInterruptEx; `iox`: the rest PnP drivers call (remove
//     locks, interfaces, notifications, power requests, registry keys).
//   * `exports`: the name -> address tables of ntoskrnl.exe, hal.dll and
//     the other modules drivers import from (netio.sys).
//
// The structures drivers read directly (DRIVER_OBJECT, DEVICE_OBJECT, IRP,
// IO_STACK_LOCATION, KEVENT, UNICODE_STRING) have Windows' x64 layout, byte
// for byte: win32/tests/k1 checks every offset used here against the WDK's
// own headers at compile time.
//
// Calling convention: everything a driver calls is `extern "win64"`.
//
// W1 covers what a simple WDM driver needs (win32/tests/k1), W2 what a PnP
// driver for a PCI device needs (win32/tests/k2). Every ntoskrnl, hal and
// ksecdd import of the NVIDIA and Intel Arc kernel drivers resolves
// (tools/import_census.py); dxgkrnl is next (W3).

// Exported functions keep the names drivers link against.
#![allow(non_snake_case)]

pub mod bcrypt;
pub mod cm;
pub mod crt;
pub mod dpc;
pub mod ex;
pub mod exports;
pub mod image;
pub mod intr;
pub mod io;
pub mod iox;
pub mod ke;
pub mod misc;
pub mod mm;
pub mod ob;
pub mod pnp;
pub mod pool;
pub mod ps;
pub mod rtl;
pub mod rtlx;
pub mod seh;
pub mod zw;

pub type NtStatus = i32;
pub const STATUS_SUCCESS: NtStatus = 0;
pub const STATUS_TIMEOUT: NtStatus = 0x102;
pub const STATUS_PENDING: NtStatus = 0x103;
pub const STATUS_UNSUCCESSFUL: NtStatus = 0xC000_0001u32 as i32;
pub const STATUS_NOT_IMPLEMENTED: NtStatus = 0xC000_0002u32 as i32;
pub const STATUS_INVALID_PARAMETER: NtStatus = 0xC000_000Du32 as i32;
pub const STATUS_NO_MEMORY: NtStatus = 0xC000_0017u32 as i32;
pub const STATUS_OBJECT_NAME_COLLISION: NtStatus = 0xC000_0035u32 as i32;
pub const STATUS_OBJECT_NAME_NOT_FOUND: NtStatus = 0xC000_0034u32 as i32;
pub const STATUS_INVALID_DEVICE_REQUEST: NtStatus = 0xC000_0010u32 as i32;
pub const STATUS_BUFFER_TOO_SMALL: NtStatus = 0xC000_0023u32 as i32;
pub const STATUS_INSUFFICIENT_RESOURCES: NtStatus = 0xC000_009Au32 as i32;

/// Set up the pool and image regions. At boot, before any process exists:
/// processes share the kernel's upper-half PML4 entries as they are when
/// the process is made, so the region's tables must already be there.
pub fn init() {
    pool::init();
    if let Err(e) = crate::nt::kusd::map_kernel_view() {
        crate::serial_println!("[NTOS] KUSER_SHARED_DATA for drivers: {}", e);
    }
    ex::init();
    misc::init();
}

/// Run `f` (driver code) with GS on this CPU's per-CPU block, where
/// drivers find their KPCR fields: an interrupt that came from user mode
/// still has the program's TEB there.
pub fn with_kernel_gs<R>(f: impl FnOnce() -> R) -> R {
    use x86_64::registers::model_specific::Msr;
    let mut gs = Msr::new(0xC000_0101);
    let want = crate::arch::smp::percpu_base(crate::arch::smp::cpu_index());
    let was = unsafe { gs.read() };
    if was != want {
        unsafe { gs.write(want) };
    }
    let r = f();
    if was != want {
        unsafe { gs.write(was) };
    }
    r
}

/// From the BSP's 1 ms timer interrupt: due timers and queued DPCs.
pub fn tick(now_ms: u64) {
    dpc::tick(now_ms);
}
