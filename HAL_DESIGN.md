# VEILOS HAL: design proposal (step 7)

**Status:** 7A and 7B done; 7C started (first part done); 7D–7E to do.

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
| 7C (part 2) | Widget and text painting as ops: it still runs on one CPU through the global painter (`ui::PAINTER_PTR`, `ui::CLIP_*`), about 12 Mcycles of each frame. Also route `gpu_driver` command batches and Win32 surfaces through the logical GPU. | Paint stage scales with CPUs too |
| 7D | Display abstraction: real mode and stride, no `SCREEN_W/H` constants | Other resolutions work |
| 7E | Mesh backends for the logical GPU; NIC aggregation | Remote devices join the pool |

## Decisions (agreed 2026-10-28)

- **First class:** GPU / compute.
- **Default split:** tile split.
- **APs:** become a shared worker pool, running user processes and HAL jobs.
- **Remote devices:** local first. The backend trait is shaped so mesh nodes fit in later (7E).
