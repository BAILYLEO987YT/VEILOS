# VEILOS

**A from-scratch operating system, written in Rust, that runs Windows programs natively and lets you switch between security and speed while it's running.**

[![Build and release](https://github.com/BAILYLEO987YT/VEILOS/actions/workflows/release.yml/badge.svg)](https://github.com/BAILYLEO987YT/VEILOS/actions/workflows/release.yml)
[![License: OCVSAL 1.0](https://img.shields.io/badge/license-OCVSAL%201.0-lightgrey)](LICENSE)

> [!WARNING]
> VEILOS is an early, experimental project. It currently runs **only in the QEMU virtual machine** and has not been tested on real hardware. It boots with the legacy BIOS (not UEFI), so it won't start on most modern PCs yet. See [Status](#status) for what works and what doesn't.

## Why VEILOS?

- **Security and speed, switchable at runtime.** *Security Mode* (the default) enforces every capability check, keeps a full audit trail, writes through to disk and enforces W^X. *Performance Mode* uses a write-back disk cache, quieter logging and a longer time slice for the focused program, and measures about 2x faster at file creation and 6x faster at starting programs in QEMU. ASLR, SMEP and SMAP are on in both modes. Switch live, without rebooting, with `MODE SECURITY` or `MODE PERFORMANCE` in the terminal.
- **Windows programs run natively.** There's no virtual machine and no Linux underneath. VEILOS implements the Windows NT interface itself, as user-mode DLLs written in Rust, in the same spirit as Wine and ReactOS.
- **Customisable all the way down.** Behaviour, not just colours. The shell, window snapping, themes and policy are driven by scripts you can edit.
- **Rust all the way down.** The kernel, drivers, network stack, and Windows DLLs are all Rust.

For the full vision, see [VEILOS_DESIGN.md](VEILOS_DESIGN.md).

## Try it

You don't need Rust. Every push to `main` is built and boot-tested in CI, then published as a zip.

1. Download **[VEILOS.zip](https://github.com/BAILYLEO987YT/VEILOS/releases/latest/download/VEILOS.zip)** and unzip it.
2. Install [QEMU](https://www.qemu.org/download/) (`brew install qemu` on macOS, `sudo apt install qemu-system-x86` on Debian/Ubuntu).
3. Run it:
   - **Windows:** double-click `run-windows.bat`
   - **macOS / Linux:** `sh run.sh`
4. Log in as `admin` with password `veilos` (or `root` with no password).

The first boot can take a minute or two, because VEILOS runs emulated. VirtualBox and VMware aren't supported yet: the only display driver is for QEMU's. Files you create are kept in `veilos-storage.img`, and the boot messages go to `serial.log`.

## Status

VEILOS can currently do the following, in QEMU:

| Area | What works |
|---|---|
| **Desktop** | Window manager with snapping and tiling, taskbar, launcher, notifications, terminal, file manager (browse and open), text editor, image viewer, multiple displays and runtime resolution |
| **Windows compatibility** | Console and classic GUI programs: `cmd.exe`, Notepad, regedit, Git Bash, git, cargo, curl, and more. A real registry, GDI with TrueType text, common controls, dialogs and menus. Direct3D 9 / 11 and OpenGL are in progress |
| **Kernel** | Preemptive multitasking, SMP, per-process address spaces, threads, a user/role system with hashed passwords and a lock screen |
| **Hardware** | SATA (AHCI) with GPT/MBR, FAT32 read/write, NTFS and exFAT read-only, virtio-net (with NIC bonding), Intel HD Audio, PCIe hot-plug |
| **Networking** | DHCP, TCP/UDP, DNS and TLS |

It hasn't run on real hardware yet. It now boots on BIOS and UEFI, from one disk or a USB stick, with USB keyboards and mice; the main gaps are NVMe, a real network card driver, ACPI power-off, an installer, a crash-safe filesystem and a web browser. [V1_CHECKLIST.md](V1_CHECKLIST.md) tracks every item with its status.

Some modules are scaffolds, not finished features. These include the native app format, parts of Cluster Mode and the hypervisor and anti-cheat support. The audit notes ([AUDIT_2026-10.md](AUDIT_2026-10.md)) say honestly which pieces are real and which are not.

## Building from source

You'll need:

- **Rust nightly** with `rust-src` and `llvm-tools-preview`. [`rust-toolchain.toml`](rust-toolchain.toml) installs it automatically through `rustup`. CI pins `nightly-2026-05-11`; see [`release.yml`](.github/workflows/release.yml).
- **A second, newer nightly for the boot image builder** (`tools/mkimage`, bootloader 0.11): `rustup toolchain install nightly-2026-11-01 --component rust-src,llvm-tools-preview`. The bootloader's own stages need nightly-2026-07-10 or later; `MKIMAGE_TOOLCHAIN` picks another.
- **QEMU** (`qemu-system-x86_64`)
- **Python 3**, used by the disk-image tools

```bash
# Build the kernel and boot it in QEMU (reads the disk from ./storage)
sh tools/run.sh            # BIOS (SeaBIOS)
sh tools/run.sh --uefi     # UEFI (QEMU's OVMF)

# Build the boot images only: target/x86_64-unknown-none/debug/veilos-{bios,uefi}.img
sh tools/build.sh

# One bootable disk (bootloader, kernel and a 1 GB VEILOS volume from storage/):
# veilos-{bios,uefi}-disk.img. Or the volume loaded into memory with the kernel,
# for a USB stick until VEILOS drives USB storage: veilos-{bios,uefi}-ram.img.
sh tools/build.sh --disk 1G
sh tools/build.sh --ramdisk 64M

# Build the release zip (dist/VEILOS.zip), as CI does
bash tools/package_release.sh
```

The kernel targets `x86_64-unknown-none` with `build-std`, linked in the top 2 GiB; see [`.cargo/config.toml`](.cargo/config.toml). It boots through [`bootloader` 0.11](https://github.com/rust-osdev/bootloader), on BIOS and UEFI alike, and draws on the framebuffer the firmware sets up until a display driver takes over. The QEMU options are in [`tools/run.sh`](tools/run.sh). VEILOS mounts as `/` only a volume with `VEILOS.VOL` at its root (other volumes are read-only); `tools/mkdisk.py` adds it, and `tools/addmarker.py IMAGE` adds it to an older disk image.

To test the mesh/cluster networking, [`run_dual.ps1`](run_dual.ps1) starts two VEILOS VMs connected by a socket network (Windows PowerShell).

### Windows system DLLs

VEILOS's own `ntdll.dll`, `kernel32.dll`, `user32.dll` and the rest are built from the [`win32/`](win32) workspace. Prebuilt copies are committed to [`storage/system`](storage/system), so you only need to rebuild them if you change that code. [`win32/build.sh`](win32/build.sh) does it. It's written for Git Bash on Windows and needs LLVM (`clang-cl`, `lld-link`) installed. The Win32 test programs are under [`win32/tests`](win32/tests), with prebuilt binaries in [`storage/tests`](storage/tests).

## Repository layout

| Path | Contents |
|---|---|
| [`src/`](src) | The kernel: memory, scheduler, drivers, filesystems, network, window system, and the NT syscall layer (`nt_*.rs`) |
| [`win32/`](win32) | The Windows system DLLs (Rust, built as real PE DLLs) and the Win32 test programs |
| [`storage/`](storage) | The disk contents VEILOS boots with: system DLLs, config and themes, fonts, wallpapers, test programs |
| [`tools/`](tools) | Disk-image builder, release packaging and boot-test scripts, launcher scripts that ship in the zip |
| [`.github/workflows/`](.github/workflows) | CI: builds, boots the result in QEMU, and publishes the latest release |

## Documentation

| Document | What it covers |
|---|---|
| [VEILOS_DESIGN.md](VEILOS_DESIGN.md) | Philosophy and long-term vision: the Security/Performance duality, Windows compatibility, customisation, the Adaptive Resource Engine, Cluster Mode |
| [WIN32_DESIGN.md](WIN32_DESIGN.md) | How Windows programs run: the loader, the NT syscall ABI, the DLLs and the graphics stack |
| [HAL_DESIGN.md](HAL_DESIGN.md) | The hardware abstraction layer: device registry, multi-CPU and multi-device rendering, hot-plug, power states |
| [V1_CHECKLIST.md](V1_CHECKLIST.md) | What's left before VEILOS could be someone's everyday system |
| [AUDIT_2026-10.md](AUDIT_2026-10.md) | A code audit from October 2026 |
| [goal.md](goal.md), [ideas.txt](ideas.txt) | The original goals and the idea backlog |

## Roadmap

In rough priority order (details in the design document and checklist):

1. **Single-machine excellence.** UEFI boot, USB, NVMe, a real network card, ACPI, an installer, a crash-safe filesystem, and a web browser, so VEILOS can be used on a normal PC.
2. **Developer platform.** A native, capability-secure target for Rust programs.
3. **Cluster Mode.** Share work between trusted VEILOS machines.
4. **Distributed and cloud features.**

## License

VEILOS is **source-available, not open source**, under the [Open Core Ventures Source Available License 1.0](LICENSE). You may modify, test and share modifications for **non-production use**. Using it in production needs a commercial agreement with the copyright holder. Any feedback or contributions you submit are licensed to the project's owner without restriction.

---

Built by Baily Hyslop.
