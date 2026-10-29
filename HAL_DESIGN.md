# VEILOS HAL: design proposal (step 7)

**Status:** 7A, 7B and 7D done; 7C mostly done (see the table); 7E to do.

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
| 7C (part 2) | **Frame display list** (`lgpu::dl_*`): while a frame is painted, fills, text (8×8 glyphs) and the wallpaper blit are recorded, then rendered banded on every CPU in one go. Code that needs the raw buffer (image decoders, `render2d`) gets `ui::painter_ptr_flushed()`, which renders everything recorded first; recording is suspended inside `render2d_kernel::with_backbuffer`. Large opaque ops drop earlier ops they fully cover (overdraw elimination, e.g. the clear under the wallpaper). Typical frame: 2 flushes, ~276 ops. **Measured (QEMU TCG):** rendering the list takes ~7 Mcycles on 4 CPUs vs ~10.5 on 1, but whole frames are about the same as before the list on 4 CPUs (14–16) and ~1–2 Mcycles slower on 1 CPU (18–20 vs 17–18). Pixel work under TCG is bound by emulated memory speed, and ~6 Mcycles of each frame is CPU logic (layout, `render2d` shapes) on the main CPU. The list is kept as the structure other backends need: ops are data that a GPU or mesh node can take. **Still to do:** `render2d` shapes as ops; `gpu_driver` batches and Win32 surfaces through the logical GPU; re-measure on real hardware. | Correct rendering verified (text-heavy terminal, file manager, taskbar) |
| 7D ✅ | Display mode at run time. `ui::screen_w()/screen_h()` replace the `SCREEN_W/H` constants (~100 uses in 10 files). The Bochs driver sets the mode from `/etc/display.cfg` (`WIDTHxHEIGHT`, checked against VRAM), reads back the real size and stride, and publishes them before the back buffer is allocated. Mouse clamps follow the mode. | 1024×768 (default), 1280×720 and 800×600 render correctly; an invalid request falls back to the default with a log line. Still one display; multi-display desktop and the MMIO-only `bochs-display` register path are open. |
| 7E | Mesh backends for the logical GPU; NIC aggregation | Remote devices join the pool |

## Decisions (agreed 2026-10-28)

- **First class:** GPU / compute.
- **Default split:** tile split.
- **APs:** become a shared worker pool, running user processes and HAL jobs.
- **Remote devices:** local first. The backend trait is shaped so mesh nodes fit in later (7E).
