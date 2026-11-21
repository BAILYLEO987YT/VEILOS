// =============================================================================
// VEILOS: Windows kernel drivers (WDDM_DESIGN.md, stage W1)
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
//   * `ob`: kernel handles; `cm`: the registry (Zw*Key on nt_registry).
//   * `exports`: the name -> address tables of ntoskrnl.exe and hal.dll.
//
// The structures drivers read directly (DRIVER_OBJECT, DEVICE_OBJECT, IRP,
// IO_STACK_LOCATION, KEVENT, UNICODE_STRING) have Windows' x64 layout, byte
// for byte: win32/tests/k1 checks every offset used here against the WDK's
// own headers at compile time.
//
// Calling convention: everything a driver calls is `extern "win64"`.
//
// What W1 covers is what a simple WDM driver needs (k1's test driver). The
// import census (tools/import_census.py) lists what the GPU drivers need on
// top; W2 adds PnP, PCI resources and interrupts.

// Exported functions keep the names drivers link against.
#![allow(non_snake_case)]

pub mod cm;
pub mod dpc;
pub mod exports;
pub mod image;
pub mod io;
pub mod ke;
pub mod ob;
pub mod pool;
pub mod ps;
pub mod rtl;

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
}

/// From the BSP's 1 ms timer interrupt: due timers and queued DPCs.
pub fn tick(now_ms: u64) {
    dpc::tick(now_ms);
}
