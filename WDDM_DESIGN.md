# VEILOS: GPU drivers through WDDM

**Status:** planned (2026-11-21). Nothing below exists yet except stage W0.

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
