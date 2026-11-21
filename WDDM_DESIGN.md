# VEILOS: GPU drivers through WDDM

**Status:** in progress (2026-11-21). W0 done. W1: import census run, and
the first slice of the kernel driver host works (below).

## Goal

VEILOS should use real GPUs (NVIDIA, AMD, Intel, any generation their vendor
still supports) by loading **the vendors' own Windows display drivers**, the
ones a user downloads from nvidia.com, amd.com or intel.com. VEILOS provides
the interfaces those drivers expect, and the vendor supplies the code that
knows its hardware and keeps it current. No per-card code in VEILOS.

Windows drivers are the only form all three vendors ship for every
consumer GPU, with full 3D, video and compute, updated monthly. The other
route (Linux's open drivers through a Linux-API shim, as FreeBSD does) would
cover Intel and AMD but not NVIDIA's current driver, and its updates would
come from rebuilding Linux code, not from the vendor. VEILOS already runs
Windows programs, so the user-mode half of a Windows driver fits what it has.

## Where VEILOS is today (honestly)

- **No GPU is driven.** The display is the framebuffer the firmware set up
  (UEFI GOP or VBE) at the firmware's mode, or the Bochs/QEMU adapter. The GPU
  only scans that memory out to the monitor.
- **All drawing is on the CPU.** The desktop, `gpu_driver.rs`'s command queue
  and `lgpu.rs` (the "logical GPU": a renderer that splits pixel work over
  every core) run on the CPU. So do Win32 Direct3D 9/11 and OpenGL
  (`win32/d3d9`, `win32/d3d11`, `win32/opengl32` → `raster3d.rs`).
- **`gfx_api.rs` is a stub.** It once claimed DXVK/vkd3d; neither exists.
- Boot messages used to say "GPU-accelerated", "VRAM-backed swapchains",
  "D3D → VkIR → GPU". Stage W0 corrected them.
- What helps: Windows user-mode programs run (PE loader, ntdll, kernel32,
  user32, gdi32, d3d9/11, opengl32 of VEILOS's own), an NT object/handle
  model, a registry (`nt_registry.rs`), PCI with MSI/MSI-X (`pci.rs`), and a
  PE image mapper (`modules.rs`).

## What a Windows display driver needs

A WDDM driver package has two halves:

1. **Kernel-mode driver** (`nvlddmkm.sys`, `amdkmdag.sys`, `igdkmdn64.sys`):
   a PE32+ image linked against `ntoskrnl.exe`, `hal.dll`, and the display
   port driver `dxgkrnl.sys` (via `displib.lib`'s `DxgkInitialize`, which
   opens `\Device\DxgKrnl` and fetches dxgkrnl's registration function with
   an internal IOCTL). In `DriverEntry` it hands dxgkrnl a table of
   `DxgkDdi*` callbacks; dxgkrnl then calls those for everything and gives the
   driver a `DXGKRNL_INTERFACE` of `DxgkCb*` callbacks back.
2. **User-mode drivers**: DLLs loaded into each program. The Direct3D runtime
   loads the D3D UMD (`nvldumdx.dll`, …), named in the adapter's registry key,
   and talks to it through the documented D3D10/11/12 DDI; the UMD talks to
   the kernel through `D3DKMT*` calls (gdi32 → `NtGdiDdDDI*` system calls →
   dxgkrnl). OpenGL and Vulkan come as vendor ICDs that call `D3DKMT*` too.

So VEILOS must provide, in the kernel:

| Piece | What it is | Size |
|---|---|---|
| **Kernel driver host** | Load `.sys` images into ring 0, resolve imports against VEILOS's own `ntoskrnl`/`hal` exports, run `DriverEntry`. IRQL model, spinlocks, DPCs, timers, events, work items, system threads, pool and contiguous memory, MDLs, `MmMapIoSpace`, PnP and power IRPs with PCI resources, interrupts (MSI), registry and file access, ETW/WMI stubs, bug checks to the crash screen | Large: hundreds of exports, most simple, some (memory, IRQL) subtle |
| **dxgkrnl** | The display port driver: adapter start, `QueryAdapterInfo` (caps, memory segments), child devices and monitors, the VidPN model (topologies and modes, which the OS implements and the driver queries), mode commit, scan-out address, interrupts and DPCs, cursor | Large |
| **VidMm and VidSch** | Video memory manager (allocations, segments or GPU virtual addressing, paging and residency, fences) and the GPU scheduler (per-context queues, preemption, TDR when a GPU hangs) | Very large |
| **D3DKMT system calls** | The `NtGdiDdDDI*` calls UMDs and runtimes make: adapters, devices, contexts, allocations, submit, present, fences | Medium |

and in user mode:

| Piece | What it is |
|---|---|
| **Runtimes** | VEILOS's `d3d11`/`d3d12`/`dxgi` call the vendor UMD through the DDI instead of `raster3d`; `opengl32` loads the vendor ICD; a Vulkan loader for the vendor's Vulkan ICD |
| **Installation** | Read the vendor's INF: copy files, write the service and adapter class keys (`UserModeDriverName`, …). Later: run vendor installers (SetupAPI, driver store) |

Microsoft documents all of this in the Windows Driver Kit (headers and
docs). VEILOS ships none of Microsoft's binaries: its ntoskrnl, hal and
dxgkrnl are its own Rust, and test drivers come from Microsoft's MIT-licensed
Windows-driver-samples, built from source.

## Stages

Each stage ends in something that works and can be tested on its own.

| Stage | Contents | Done when | Tested on |
|---|---|---|---|
| **W0** | Honest status: boot messages, `gpu_driver.rs` / `gfx_api.rs` headers, this document | Nothing on screen claims GPU use that doesn't happen | Done |
| **W1** | **Import census, then the kernel driver host.** A host tool lists every import of the vendors' `.sys` and UMD files (by DLL and function) and how many VEILOS provides, which sizes and orders the ntoskrnl work. Then: map a `.sys`, apply relocations, resolve imports (unresolved ones refuse the load and are listed), the security cookie, `DriverEntry`, `DRIVER_OBJECT`/`DEVICE_OBJECT`, IRPs (`IoCallDriver`, `IoCompleteRequest`), pool, IRQL and spinlocks, DPCs, timers, events, system threads | A sample WDM driver (built from Windows-driver-samples) loads, creates its device, answers IOCTLs from a Win32 program and unloads | QEMU |
| **W2** | **PnP, resources and interrupts.** `AddDevice`, `IRP_MN_START_DEVICE` with a `CM_RESOURCE_LIST` from PCI (BARs, MSI), `IoConnectInterruptEx`, `MmMapIoSpaceEx`, contiguous DMA memory, power IRPs. Driver binding by PCI ID through the HAL (a "Windows driver" `hal::Driver`) | A sample PCI driver (or a small one of our own built with the WDK headers) drives a QEMU device through its interrupt | QEMU |
| **W3** | **dxgkrnl, display only.** `DxgkInitialize` registration, adapter start, `QueryAdapterInfo`, child relations and status, EDID, the VidPN objects, `CommitVidPn`, `PresentDisplayOnly`. The desktop presents through it | Microsoft's KMDOD sample (a display-only driver over the firmware framebuffer, built from source) runs the VEILOS desktop | QEMU and both PCs |
| **W4** | **A vendor driver, mode-setting.** Load a real vendor `.sys`, get through `DriverEntry`, `StartDevice` and `QueryAdapterInfo`, enumerate monitors, commit a mode, scan out the desktop. Everything it calls that VEILOS lacks shows up here | The desktop at a mode chosen at run time, through the vendor driver | Real hardware: the test PC (GTX 745, NVIDIA 580.x, the last Maxwell branch) or the main PC (Intel Arc) |
| **W5** | **VidMm and VidSch.** Allocations, segments / GPU virtual memory, paging buffers, residency, monitored fences, contexts and submission, `DxgkDdiSubmitCommand`, interrupts → DPC → fence completion, TDR | A `D3DKMT`-level test program allocates, submits a vendor-built command buffer through the UMD and waits on its fence | Real hardware |
| **W6** | **User mode.** `NtGdiDdDDI*` system calls; VEILOS's `d3d11`/`dxgi` drive the vendor UMD through the D3D11 DDI; present through dxgkrnl | A D3D11 triangle and the D3D test programs (`storage/tests/d3d*`, `dx*`) render on the GPU, matching their `.ref` images | Real hardware |
| **W7** | D3D12 DDI; OpenGL ICD in `opengl32`; Vulkan loader and ICD; video decode; installing from the vendor's package; several GPUs | Games run on the GPU | Real hardware |

### W1 import census (2026-11-21)

`tools/import_census.py` on two current driver packages (read only, kept
out of the repository): NVIDIA 581.80 (R580, the last branch for
Maxwell/Pascal; lists the GTX 745, `DEV_1382`) and Intel Arc 32.0.101.9175
(the A770 in the main PC, from its driver store).

| Driver part | NVIDIA 581.80 | Intel Arc 32.0.101.9175 | VEILOS has today |
|---|---|---|---|
| Kernel driver | `nvlddmkm.sys`: 336 imports (327 ntoskrnl, 4 hal, 4 netio, 1 ksecdd) | `igdkmdnd64.sys`: 259 (240 ntoskrnl, 15 ksecdd BCrypt, 4 hal) | none |
| D3D11/D3D12 UMD | `nvwgf2umx.dll` 342, `nvd3dumx.dll` 288 | `igd10umt64xe.dll` 215, `igd12um64xe2.dll` 222 | 89–96 % |
| OpenGL ICD | `nvoglv64.dll` 385 | `igxe2hpgicd64.dll` 213 | 89–95 % |
| Vulkan | (in `nvoglv64.dll`) | `igvk64.dll` 196 | 94 % |
| Loader / shader compiler | `nvldumdx.dll` 135, `nvgpucomp64.dll` 187 | `igc-default64.dll` 184 | 97–99 % |

- **Kernel:** the two drivers together use 399 distinct ntoskrnl functions,
  168 of them shared. By area: Rtl 68 (strings, bitmaps, generic tables,
  security descriptors), Ke 68 (IRQL, spinlocks, DPCs, timers, events,
  waits), Io 57 (devices, IRPs, work items, interrupts, PnP and interface
  registration), Ex 46 (pool, resources, fast mutexes, SLists, callbacks),
  Zw 41 (registry, files, sections), C runtime 38, Mm 32 (contiguous and I/O
  memory, MDLs), Ps 21, a few Etw/Ob/Se/Po/Cm/Kd. hal: 4 (PCI config,
  performance counter, stall). That is W1's list; most entries are small.
- **Neither kernel driver imports dxgkrnl.** Both reach it at run time
  (`IoGetDeviceObjectPointer` on `\Device\DxgKrnl`, from `displib`), so the
  DxgkDdi/DxgkCb interface, VidPN, VidMm and VidSch don't show in an import
  list: they are W3–W5's work and the largest part.
- **User mode is mostly there already:** VEILOS's own DLLs provide 89–99 % of
  what the vendors' UMDs and ICDs import; each lacks a few dozen functions,
  mostly in kernel32, user32 and advapi32. The UMDs' real dependency is the
  D3DKMT calls and the runtimes' DDI, which an import list doesn't show
  either (they go through gdi32's `D3DKMT*`, a handful of exports).

### W1 kernel driver host, first slice (2026-11-21)

`src/ntos/`: VEILOS's own implementation of the kernel interface, 89
exports so far (pool, IRQL as CR8, spinlocks including queued ones, events,
semaphores, mutexes and waits, time, DriverObject/DeviceObject/IRPs with
IoCallDriver and IoCompleteRequest and its completion routines, device
names and links, strings, version, the CRT, DbgPrint with printf). Pool and
driver images live in the upper half (PML4 slot 448), as drivers expect.
The loader maps a `.sys`, relocates it, binds every import by name
(refusing the load with a list when one is missing), sets the /GS cookie
and calls `DriverEntry`.

K1 (`win32/tests/k1`): a WDM driver built with clang-cl against the WDK.
`layout.c` asserts every structure offset `src/ntos` uses against the WDK
headers at compile time (all matched). In QEMU, `DRIVER LOAD
/tests/veiltest.sys` runs its DriverEntry (pool zeroed, IRQL 2 under its
spinlock, event waits time out and succeed as they should, device and link
created); `DRIVER OPEN` sends create and close; `DRIVER IOCTL` gets its
echo reversed, its counters, and `STATUS_INVALID_DEVICE_REQUEST` for an
unknown code; `DRIVER UNLOAD` runs its unload routine.

### W1 second slice (2026-11-21)

124 exports now. Added:
- **Windows programs reach drivers.** `CreateFile("\\.\Name")` opens a
  device: `NtCreateFile` sees a device name, sends IRP_MJ_CREATE with a
  real `FILE_OBJECT`, and the handle sends IRP_MJ_CLEANUP and IRP_MJ_CLOSE
  when it closes. `DeviceIoControl` is a new system call,
  `NtDeviceIoControlFile`: METHOD_BUFFERED goes through a pool buffer;
  METHOD_NEITHER hands the driver the program's own pointers, with SMAP
  lifted.
- **System threads** (`PsCreateSystemThread`, waitable, and
  `PsTerminateSystemThread`).
- **DPCs**: run at once, at DISPATCH_LEVEL, when queued below it; at the
  next 1 ms tick otherwise.
- **Timers**: one-shot and periodic, checked every tick, signalling
  waiters and queueing their DPC.
- **Work items**: `IoQueueWorkItem(Ex)` and `ExQueueWorkItem`, each on a
  thread of its own.
- **Registry**: `Zw*Key` and `RtlWriteRegistryValue` on VEILOS's registry
  through kernel handles. A driver's Services key exists before its
  `DriverEntry` runs.

K1 grew to match. Its driver self-tests all of the above in `DriverEntry`,
and `k1test.exe`, an ordinary Windows program, opens `\\.\VeilTest` and
checks the following through `DeviceIoControl`: buffered and
METHOD_NEITHER IOCTLs, the create on its handle, the self-test results,
and an unknown code failing with `ERROR_INVALID_FUNCTION`. In QEMU: all
passed (self-test `1f of 1f`, `k1test: 0 failed`).

### W1 third slice: SEH in drivers (2026-11-21)

`src/ntos/seh.rs` brings `__try`/`__except`/`__finally` to drivers. It is
adapted from VEILOS's user-mode SEH (ntdll's unwinder and dispatcher, and
ucrtbase's `__C_specific_handler`).

- **CPU faults** in ring 0 whose RIP lies in the driver image region:
  nt_exception's entry stubs send them to `kernel_entry`. That builds a
  `CONTEXT` and `EXCEPTION_RECORD`, moves off the IST stack onto the
  driver's own, and dispatches through the driver's `.pdata` with the
  interrupt flag as it was.
- **Software exceptions**: `RtlRaiseException`, `ExRaiseStatus`,
  `ProbeForRead/Write` and `RtlUnwind(Ex)` are assembly stubs that capture
  their caller's registers first. VEILOS's own frames have no Windows
  unwind tables, so every walk starts in driver code.
- An unwind made during a dispatch starts from the context that exception
  was raised in (kept per thread).
- **Unhandled**: an exception no driver frame takes is bug check 0x1E, as
  on Windows.
- Data exports `MmUserProbeAddress`, `MmHighestUserAddress` and
  `MmSystemRangeStart`.

K1's driver is now built with MSVC, as vendors build theirs. clang-cl's
`__try` only catches exceptions raised by calls; MSVC also catches a fault
in the `__try` body itself. In QEMU every part passed, and `k1test` checks
each:
- a page fault caught by `__except` (`STATUS_ACCESS_VIOLATION`);
- `ExRaiseStatus` caught two calls up, with both `__finally` blocks run;
- `ProbeForRead` refusing a kernel address and passing a user one;
- `EXCEPTION_CONTINUE_EXECUTION` resuming after `RtlRaiseException`.

### W1 memory for drivers (2026-11-21)

`src/ntos/mm.rs`:
- **MDLs**: `IoAllocateMdl`, `MmBuildMdlForNonPagedPool`,
  `MmProbeAndLockPages` (raising into the driver's `__try` for a bad
  page; a program's not-yet-made pages are touched first),
  `MmMapLockedPagesSpecifyCache`, `IoBuildPartialMdl`, and
  `MmAllocatePagesForMdl(Ex)` / `MmFreePagesFromMdl`.
- **Contiguous memory** below a limit and within a boundary
  (`MmAllocateContiguousMemory(SpecifyCache)`, `MmAllocateNonCachedMemory`).
- **Device memory**: `MmMapIoSpace(Ex)` and `MmUnmapIoSpace`, cached,
  write-combining or uncached (VEILOS's PAT).
- `MmGetPhysicalAddress` and `MmIsAddressValid`.
- Mappings go in a system-PTE range of the ntos region. Programs' requests
  with `METHOD_IN_DIRECT`/`METHOD_OUT_DIRECT` get their output buffer as an
  MDL (`Irp->MdlAddress`).

K1 tests each of these: contiguous memory below 4 GiB with consecutive
physical pages, the same pages through `MmMapIoSpace`, an MDL over pool
whose PFNs match `MmGetPhysicalAddress`, pages for an MDL mapped and
written, and a `METHOD_OUT_DIRECT` request writing 9000 bytes across three
of the program's pages (and nothing either side). All passed in QEMU
(`3fff of 3fff`, `k1test: 0 failed`). 163 exports now.

### W1 census, most of the way (2026-11-21)

386 functions and 12 data exports now. The NVIDIA and Arc kernel drivers
together lack 97 imports, down from 399:

- `crt.rs`: the sprintf family, narrow and wide, with one printf engine for
  all of ntos (MSVC sizes: `l` is 32-bit); bounded and `_s` string
  functions; `sqrt`.
- `rtlx.rs`: counted-string conversions and numbers, bitmaps, generic
  tables, SIDs, ACLs and security descriptors, `RtlTimeToTimeFields`,
  resource-descriptor encoding, `RtlCheckRegistryKey`,
  `RtlCreateRegistryKey`, and `RtlQueryRegistryValues` (direct values,
  query routines, subkeys, defaults).
- `ex.rs`: ERESOURCE (recursive exclusive, shared), fast and guarded
  mutexes, SLists in Windows' header layout, lookaside lists, interlocked
  lists, callback objects, Ex timers, UUIDs, and the object-type and
  debugger data exports.
- `misc.rs`: kernel queues, processor queries, and accepted-but-unused
  registrations (process and image notifications, power settings, ETW,
  registry callbacks, bug-check callbacks). Also tokens, pageable-section
  calls, reserved mappings and `MmCopyMemory`.

K1 checks each group in its self-test (now 22 parts, `3fffff`), and
layout.c checks the new structures against the WDK (with ntifs.h for
SECURITY_DESCRIPTOR). In QEMU: `3fffff of 3fffff`, `k1test: 0 failed`.

The 97 left, by area:
- **Device stacks and PnP** (attach and detach, remove locks, device
  interfaces and properties, device registry keys, PnP notifications, error
  log, `IoConnectInterruptEx`, PCI config through `HalGet/SetBusDataByOffset`,
  `PoRequestPowerIrp`): that's W2.
- **`Zw*` files, sections, events, processes, tokens and system
  information**: on VEILOS's VFS and objects.
- **BCrypt from `ksecdd.sys`** (15) and the network module registrar from
  `netio.sys` (4).
- **`SeExports`.**

### W1: Zw, BCrypt, HAL, SeExports (2026-11-21)

445 exported functions. Every import of the NVIDIA and Arc kernel drivers
now resolves except 41, all W2's: the device stack and PnP (attach,
detach, remove locks, device interfaces and properties, device registry
keys, PnP notifications), `IoConnectInterruptEx`, the error log,
`IoCancelIrp`, `PoRequestPowerIrp`, `ObOpenObjectByPointer` and
`FsRtlRegisterFileSystemFilterCallbacks`.

- `zw.rs`:
  - files on the VFS (`\??\C:\`, `\DosDevices\`, `\SystemRoot\`), and
    devices by name with kernel-side IOCTLs;
  - named events, symbolic links, handle duplication, LUIDs;
  - registry enumeration (`ZwEnumerateValueKey`, `ZwEnumerateKey`,
    `ZwQueryKey`);
  - `ZwQuerySystemInformation` (basic, and the module list with the
    kernel and loaded drivers);
  - `ZwLoadDriver`/`ZwUnloadDriver` through a Services key's `ImagePath`.

  Process virtual memory, sections, process and token queries and power
  information return `STATUS_NOT_SUPPORTED` and log it the first time.
- `bcrypt.rs` (ksecdd.sys) uses the kernel's crypto crates:
  - random numbers;
  - SHA-1/256/384/512, plain or HMAC;
  - AES-128/192/256 decryption in ECB, CBC (PKCS#7) and GCM.

  Asymmetric keys and key derivation return `STATUS_NOT_SUPPORTED` (no RSA
  or ECC crate in the kernel yet).
- `misc.rs`: `SeExports` (privilege LUIDs and the well-known SIDs), the
  netio.sys network module registrar (accepted; nothing attaches), and
  `HalGet/SetBusDataByOffset` on PCI configuration space.

The first run caught a data-export rule. A `dllimport` declaration
(`extern NTKERNELAPI PSE_EXPORTS SeExports`) reads the exported variable's
value, so `SeExports` must hold a pointer to the structure. Plainly
declared ones (`extern POBJECT_TYPE *ExEventObjectType`) bind to the
exported address itself. The driver had read a privilege LUID as the
pointer.

K1 now has 29 self-test parts, `1fffffff`. The new ones:
- `ZwCreateFile`/`Write`/`Read`/`QueryInformationFile` round trip;
- a named event opened twice;
- the device's symbolic link;
- registry enumeration;
- the driver finding itself in the module list;
- SHA-256 and HMAC-SHA256 test vectors and the FIPS-197 AES vector;
- PCI 00:00.0's vendor ID;
- `SeExports`' LocalSystem SID.

layout.c checks SE_EXPORTS and the information structures. In QEMU:
`1fffffff of 1fffffff`, `k1test: 0 failed`.

W1 is finished. The last items:
- ~~per-section protection~~: done. After relocation each page of a driver
  image gets its sections' rights: no-execute unless the section is
  executable, writable only if the section is writable. K1 calls a `ret`
  stored in `.data` inside `__try` and gets an access violation (passed).
  CR0.WP is on, so ring 0 can't write read-only sections either.
- ~~freeing an unloaded image~~: done. DRIVER UNLOAD calls DriverUnload,
  then frees the image's pages, the DRIVER_OBJECT and its extension. K1
  unloads, loads again (self-test passes again) and unloads. The reload
  found that named events outlived their handles; as in Windows, the name
  now goes with the last handle.
- ~~mapping an MDL into a program (`UserMode`)~~: done.
  MmMapLockedPagesSpecifyCache with UserMode reserves a view in the calling
  program and maps the MDL's pages there; MmUnmapLockedPages takes it back.
  K1's program reads the driver's pattern through it and writes a byte the
  driver then sees (passed).
- ~~`KeGetCurrentThread` and the other GS inlines~~: done. VEILOS's
  per-CPU block now carries the KPCR/KPRCB fields drivers read through GS:
  Self 0x18, CurrentPrcb 0x20, Number 0x184, CurrentThread 0x188 (each
  kernel thread's own record, set on every switch) and ProcessorIndex
  0x1A4. Kernel threads run with GS there, and DPCs run from an interrupt
  that came from user mode switch it there too. K1 checks it in
  DriverEntry, in a system thread and inside a program's request (all
  passed). The thread records are in the kernel heap, which by Windows'
  rules is a user-range address; no driver seen so far checks that.

### Waits inside a program's request (2026-11-21)

A driver's IOCTL runs inside the program's system call, with interrupts
off. The concern was that its waits would never see the 1 ms tick that
fires timers and DPCs. A negative control with the old loop (yield, then
spin) passed every wait too: yielding hands the CPU to another thread (on
the boot CPU, the desktop's), which runs with interrupts on, so ticks
still came. Waits below DISPATCH_LEVEL now pause the way VEILOS's own
system calls do (`ke::pause`): another ready thread on this CPU, else HLT
with interrupts on until the next interrupt, and off again after. That's
the same result without spinning when nothing else is ready.

The bug the test did find was in the clock: `KeQuerySystemTime(Precise)`
came from the 1 ms tick count, which lags whenever ticks arrive late
(WHPX delivers them in bursts), so a 30 ms delay could measure under
20 ms. It now comes from the TSC (plus the NTP offset): 29.5–29.8 ms in
four runs.

K1's `IOCTL_VEIL_WAITTEST`, sent by `k1test`, waits inside the request on
a system thread, a timer object, `KeDelayExecutionThread`, a work item and
a timer's DPC. All five passed in QEMU on 1 CPU (twice) and on 2 CPUs.

Opening a device after its driver unloads fails with `ERROR_INVALID_NAME`
where Windows says `ERROR_FILE_NOT_FOUND`. The system time drivers see is
VEILOS's wall clock, which is 1970 until NTP has answered.

W1 to W3 need no GPU and run in QEMU. From W4 on, testing is on real
hardware only: QEMU on a Windows host can't pass a GPU through. Logs go to
the screen and the crash log on disk (`crash.rs`).

## Rules

- **Vendor drivers run in ring 0, inside VEILOS.** A bug in them can take
  down VEILOS, as on Windows. Security Mode should load only drivers whose
  Authenticode signature checks out (vendor-signed packages), and the user
  installs a driver explicitly.
- **Licensing:** vendor drivers are the user's download under the vendor's
  licence; VEILOS doesn't redistribute them. No Microsoft binaries in the
  repository (WDK headers are used to write VEILOS's own code; samples are
  built from MIT source).
- **Version:** VEILOS's dxgkrnl reports the WDDM version it implements
  (`DXGKDDI_INTERFACE_VERSION`) and the Windows version drivers check
  (registry, `RtlGetVersion`). Current drivers need WDDM 2.x or later.
- **Fallback:** without a driver for the GPU (or when one fails to start),
  VEILOS keeps the firmware framebuffer and CPU rendering, as now. A driver
  failure must never leave the PC without a display.
- Before W4: a boot-time improvement for every GPU, choosing the monitor's
  native resolution from the firmware's mode list and EDID in the UEFI
  bootloader stage, so the CPU-rendered desktop is at least at the right
  resolution everywhere.

## Risks

- **Scale.** ReactOS has worked toward WDDM for years and has only early,
  experimental support. Expect W1–W3 to take weeks each and W4–W6 months.
- **Undocumented behaviour.** Vendor drivers rely on details of Windows
  beyond the documentation (call order, timing, registry values, IRQL at
  each callback). Each one is found on real hardware, one failure at a time.
- **Checks in the drivers.** OS version checks, and some drivers refuse to
  load outside the Windows versions they support.
- **Old hardware:** NVIDIA's 580 branch is the last for Maxwell/Pascal, so
  the test PC's GTX 745 gets no newer drivers. Newer cards are the target;
  the GTX 745 is still a useful first test.
