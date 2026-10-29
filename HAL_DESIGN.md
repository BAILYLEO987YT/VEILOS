# VEILOS HAL: design proposal (step 7)

**Status:** done for this phase. 7A, 7B, 7D (with several displays), 7F (PCIe hot-plug) and 7G (power states) done; 7C done apart from Win32 surfaces (with the Win32 work) and a real-hardware re-measure; 7E logical NIC done, mesh GPU backend deferred (see the table).

## Goal

1. **Device-count independence.** The OS works the same whatever number of discrete devices the
   machine has: one disk or five, one GPU or three, zero or several NICs.
2. **One-device software uses many devices.** Software written for a single device (most Windows
   software, which assumes one GPU) transparently gets the work of several.

## What exists today (after steps 1–6)

| Area | State |
|---|---|
| Discovery | `pci.rs` is a registry scanned once, with BARs, capabilities, MSI info. No driver binding: each subsystem calls `pci::find_class` itself. |
| Storage | Already device-count independent: every partition on every disk is mounted (`block.rs`, `/mnt/d<port>p<n>`). |
| Network | One virtio-net NIC; the first one found is used. |
| Display | One framebuffer (BAR0 of the first display device). `1024×768` is hard-coded at 103 sites in 10 files (`ui::SCREEN_W/H`). |
| GPU | `gpu_driver.rs` is a software model: every `GpuCommand` is executed on the CPU into the framebuffer, and 3D commands are no-ops. All submission goes to `primary()`. No hardware GPU driver exists. |
| Remote | `distributed.rs` can already ship a `GpuCommand` batch to a mesh node and get pixels back. |
| CPUs | Kernel threads exist, but only on the BSP. Each AP runs one Windows process to completion and is otherwise idle. |

So in QEMU (and on real hardware until a GPU driver exists), the devices a "logical GPU" can spread
work over are **CPU cores acting as software rasterisers** and **remote mesh nodes**.

## Proposed architecture

```
  software (Win32 / gfx_api / desktop)         sees exactly ONE device per class
  ─────────────────────────────────────────────────────────────────────────────
  logical devices      LogicalGpu    LogicalDisplay    VFS (exists)    LogicalNet
                           │  split policy                                │ policy
  ─────────────────────────┼───────────────────────────────────────────────┼─────
  backends             cpu-worker × N   fb device   mesh node × M     nic × K
  ─────────────────────────────────────────────────────────────────────────────
  device model         hal::Device registry  ← drivers bind to pci::PciDevice once
```

1. **Device model (`hal`).** A `Device` record per physical device (id, class, bus location, bound
   driver, state), plus driver binding: each driver declares what it matches, and the registry
   binds each PCI device once at boot. This replaces the ad-hoc `find_class` calls, and is where
   hot-plug and power state would hang later.
2. **Class interfaces.** One trait per class that backends implement. `BlockDevice` already
   exists; this adds `GpuBackend` (execute a command batch for a region of a target),
   `DisplayDevice` (mode, stride, scanout) and `NetDevice`.
3. **Logical devices.** What software sees: one per class, made of N backends plus a policy. This
   gives you goal 1 (software never counts devices). The policy that splits one stream of work
   across backends gives you goal 2.
4. **Logical GPU first**, since that's where the Windows single-device bottleneck lives:
   - Software keeps submitting one `GpuCommand` stream to "the GPU".
   - The logical GPU splits each frame across backends and composes the result:
     - **Tile split** (split-frame): the target is cut into tiles and each backend renders its
       tiles from the same command list. Good for fill and blit-heavy 2D, which is what VEILOS
       draws today.
     - **Alternate frame:** each backend renders whole frames in turn. Higher throughput, one
       frame of extra latency.
     - **Per surface:** independent windows or surfaces go to different backends.
   - First backends: one **CPU rasteriser worker per AP** (kernel threads on APs) and the local
     framebuffer. Mesh nodes come next, by wrapping `distributed::remote_gpu_submit`.
5. **Logical display.** Replace the compile-time `1024×768` with the display device's real mode and
   stride. Later the desktop can span several displays.

## Proposed order

| Part | Content | Visible result |
|---|---|---|
| 7A ✅ | `hal` device registry and driver binding; AHCI, virtio-net and Bochs display drivers bind through it | No behaviour change; the terminal's `DEVICES` lists every device with its driver, or why it isn't driven |
| 7B ✅ | APs run kernel threads. `workers::for_each(n, f)` spreads work over every CPU, with the caller taking part. A user process on an AP is preempted every 10 ms so pool work isn't stuck behind it. | `WORKERS` benchmark: 3.7× on 4 CPUs (183 → 50 ms), unchanged with gui_test.exe busy on an AP; 1 CPU runs everything on the caller |
| 7C (part 1 ✅) | `lgpu.rs`: `Op` lists (fill, copy with transparent key) against a `Surface`, cut into horizontal bands and rendered on every CPU through the worker pool by the `CpuRaster` backend. The frame clear, the wallpaper blit and the back-buffer flush go through it. | Full desktop frame 19–20 → 15–16 Mcycles on 4 CPUs (clear 4.1 → 2.0, flush 2.8 → 1.4); 1 CPU unchanged |
| 7C (part 2) | **Frame display list** (`lgpu::dl_*`): while a frame is painted, fills, text (8×8 glyphs) and the wallpaper blit are recorded, then rendered banded on every CPU in one go. Code that needs the raw buffer (image decoders, `render2d`) gets `ui::painter_ptr_flushed()`, which renders everything recorded first; recording is suspended inside `render2d_kernel::with_backbuffer`. Large opaque ops drop earlier ops they fully cover (overdraw elimination, e.g. the clear under the wallpaper). Typical frame: 2 flushes, ~276 ops. **Measured (QEMU TCG):** rendering the list takes ~7 Mcycles on 4 CPUs vs ~10.5 on 1, but whole frames are about the same as before the list on 4 CPUs (14–16) and ~1–2 Mcycles slower on 1 CPU (18–20 vs 17–18). Pixel work under TCG is bound by emulated memory speed, and ~6 Mcycles of each frame is CPU logic (layout, `render2d` shapes) on the main CPU. The list is kept as the structure other backends need: ops are data that a GPU or mesh node can take. **Still to do:** Win32 surfaces through the logical GPU (with the Win32 work); re-measure on real hardware. | Correct rendering verified (text-heavy terminal, file manager, taskbar) |
| 7C (part 3 ✅) | **`render2d` shapes and `gpu_driver` batches on the logical GPU.** A `render2d::Canvas` can hand shape fills and strokes to an `Offload`; over the frame's back buffer, shapes of 64K pixels or more (solid, linear or radial paint) become `Op::Shape` records in the display list and render banded, each band through a canvas over its own rows with a per-CPU mask cache. The canvas renders what's recorded before any pixel it draws or reads itself, so order holds (this replaces suspending the list around `render2d` code). `gpu_driver::flush` now runs the ring as one batch: FillRect and Present become ops rendered together; an overlapping BitBlt (memmove order) or an upload ends the run. Commands now have one target and real VRAM: the display driver maps the whole aperture, the scanout at its start is reserved, UploadSurface writes the VRAM surface at its offset (it used to copy onto the top-left of the screen), Present copies a VRAM surface to the screen (it was a no-op), and FillRect draws on the scanout like the others (it went to the back buffer). Security Mode queues now run once per GUI loop (`gpu_driver::tick`; nothing called the scheduler before, so they never ran), and uploads run at submit time, in the submitter's address space. `GPUTEST` checks a batch's pixels and times it. | Test card (`GFXTEST`) pixel-identical to the previous build; full-screen gradient 73–76 → 23–32 Mcycles, uncached card frame 153 → 101 on 4 CPUs. Desktop: the shell's full-screen root panel (3.7 Mcycles a frame, one CPU) is now banded, frames 13–14 → 12–13 on 4 CPUs, 1 CPU unchanged (18–20). `GPUTEST`: all pixel checks pass; 6.4–6.6 Mcycles on 1 CPU, 4.6–5.0 on 4 |
| 7D ✅ | Display mode at run time. `ui::screen_w()/screen_h()` replace the `SCREEN_W/H` constants (~100 uses in 10 files). The Bochs driver sets the mode from `/etc/display.cfg` (`WIDTHxHEIGHT`, checked against VRAM), reads back the real size and stride, and publishes them before the back buffer is allocated. Mouse clamps follow the mode. | 1024×768 (default), 1280×720 and 800×600 render correctly; an invalid request falls back to the default with a log line. **Several displays (part 2 ✅):** every Bochs adapter binds and becomes a head of one logical display, side by side left to right in bind order, all in the first head's mode; the back buffer, layout, wallpaper and mouse cover the whole width, and `ui::commit_dirty_rect` and the hardware cursor split their work per head (`graphics::HEADS`). DISPI registers go through the MMIO block in BAR2 (+0x500) when the adapter has one (the only way into `bochs-display`; the I/O ports only reach the legacy VGA). `DISPLAYS` lists the heads. Displays can't be added after boot (the back buffer is sized once). | Default VGA + `bochs-display`: both bound through MMIO, one 2048×768 desktop with wallpaper and taskbar continuing across the two screens (per-device screendumps). Frames 17–20 Mcycles on 4 CPUs for twice the pixels |
| 7E (logical NIC ✅) | `network::LogicalNic`: every bound virtio-net NIC joins one interface, bonded active-backup (Linux mode 1). Frames go out on the active member and come in from all; when the active member's link drops (`VIRTIO_NET_F_STATUS`) the next member that's up takes over, keeping the first member's MAC. `virtio_net` is per-instance now (own queues and DMA buffers). `NET STATUS` lists each member. | Two NICs: DHCP on the logical interface; after `set_link net0 off`, NIC 0 shows DOWN and NIC 1 carries traffic (rx and tx), with a failover log line |
| 7F ✅ | **PCIe native hot-plug** (`hotplug.rs`). Ports with a hot-plug capable slot are found at boot and their Slot Status polled from the GUI loop (no interrupts). Card inserted: slot powered on, link waited for, BARs given addresses from the port's bridge windows, functions registered (`pci::register`) and bound (`hal::add`). Removal requested (attention button, QEMU `device_del`): `hal::remove_bus` asks each driver to release its device (`Driver::remove`); if all can, the slot is powered off, which completes the removal; otherwise it is refused. Card pulled out: `hal::gone_bus`. Empty slots are powered off. virtio-net can be released (it leaves the logical NIC; the NIC now owns its DMA memory and resets the device before freeing it); AHCI and displays can't yet. `SLOTS` lists the slots, `DEVICES` shows removed devices. On real machines whose firmware keeps hot-plug control through ACPI (granted to an OS via `_OSC`, which needs an ACPI interpreter), the slot registers stay quiet. | q35 with native hot-plug (`-global ICH9-LPC.acpi-pci-hotplug-with-bridge-support=off`; QEMU 6.1+ otherwise uses ACPI hot-plug and leaves the slot registers alone): a NIC added to an empty slot gets I/O and memory BARs from the slot's windows and joins the logical NIC; removing the NIC that carried traffic moves traffic to the added one and QEMU completes the removal; removing a hot-added AHCI controller is refused and QEMU keeps it |
| 7G ✅ | **Device power states** (PCI power management capability). `PciDevice::power_state` / `set_power_state` (D0, D3hot, with the 10 ms transition delay). A device is woken to D0 before its driver probes; once every class is bound, endpoints nobody drives go to D3hot, except display adapters (firmware console), USB/serial-bus controllers (firmware legacy keyboard emulation) and bridges. `DEVICES` shows the state. Runtime power management of driven devices (idle timeouts, D3cold, ASPM) isn't done. | Unbound `e1000e`: D3hot at boot |
| 7E (mesh GPU backend) | Not built. Copy ops point at local memory, so a remote band would need its source pixels shipped (3 MiB for a wallpaper), and a TCP round trip per band breaks a 16 ms desktop frame. A mesh backend fits offscreen or batch rendering (latency-tolerant) better than the live desktop; the `Backend` trait is the seam. | — |

## Decisions (agreed 2026-10-28)

- **First class:** GPU / compute.
- **Default split:** tile split.
- **APs:** become a shared worker pool, running user processes and HAL jobs.
- **Remote devices:** local first. The backend trait is shaped so mesh nodes fit in later (7E).
