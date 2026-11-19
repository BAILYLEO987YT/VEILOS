# VEILOS 1.0 checklist

**What 1.0 means here:** someone could install VEILOS on an ordinary PC and use it every day (browse, write, manage files, listen to music, install software, shut down) without going back to Windows for routine things. It doesn't need polish, just the basics.

Status is as of 2026-11-10, worked out from the code and the design docs. Nothing was run to check it.
✅ done · ⚠️ partly done / only in a VM · ❌ missing.
**1.0** marks items that block a 1.0. The others are nice to have.

---

## 1. Boot and install on a real PC

| | Requirement | Status | Notes |
|---|---|---|---|
| **1.0** | UEFI boot | ⚠️ | Since 2026-11-18 VEILOS boots through `bootloader` 0.11 (`tools/build.sh` makes `veilos-bios.img` and `veilos-uefi.img` from one build). Both reach the desktop in QEMU (UEFI with OVMF under WHPX, BIOS with SeaBIOS). Not tried on a real PC yet. Since 2026-11-18 `tools/build.sh --disk SIZE` also makes one bootable disk (bootloader partition, then the VEILOS volume as a GPT partition of VEILOS's own type, or MBR slot 3), and `--ramdisk SIZE` an image whose volume the firmware loads into memory with the kernel, so a USB stick works before VEILOS drives USB storage (changes are lost at restart). Both boot to the desktop in QEMU, BIOS and UEFI. |
| **1.0** | Boots and runs on at least one real machine | ⚠️ | Only QEMU (TCG/WHPX) is tested so far. |
| **1.0** | Installer: partition a disk, copy the system, set up boot | ❌ | Today it runs from a disk image built by `tools/mkdisk.py`. |
| **1.0** | Graphics from the firmware framebuffer (GOP/VBE) on any GPU | ⚠️ | Since 2026-11-18: boot messages draw on the firmware's framebuffer (no VGA text mode needed), and with no driver for the adapter the desktop runs on it at the firmware's mode (`graphics::bind_firmware_framebuffer`). Only 32-bit BGRX modes carry the desktop; a 24-bit VBE mode keeps the text console. The mode can't be changed. |
| | Recovery / safe boot when something breaks | ❌ | |

## 2. Hardware drivers

| | Requirement | Status | Notes |
|---|---|---|---|
| **1.0** | USB (xHCI) with HID keyboard and mouse | ⚠️ | Since 2026-11-18 (`src/usb/`): an xHCI driver (firmware handoff, reset, rings, every root port), and boot-protocol keyboards and mice, whose reports become the same scancodes and mouse state as PS/2. Polled from the BSP's timer tick. Checked in QEMU (`qemu-xhci`, `usb-kbd`, `usb-mouse`) with the PS/2 controller removed (`i8042=off`): signing in, the mouse and the terminal all work through USB. Not yet: devices behind hubs, hot-plug, EHCI-only machines, I2C touchpads, a real PC. |
| **1.0** | USB mass storage (flash drives) | ⚠️ | Since 2026-11-18: bulk-only transport with SCSI commands (`usb/msc.rs`), a block transport like AHCI, so a stick's VEILOS volume can be `/` and other sticks mount read-only. Checked in QEMU (`usb-storage` on xHCI) as the root volume, running Win32 programs from it. Not yet: 4 KiB-sector drives, UAS, a real stick. |
| **1.0** | NVMe | ❌ | Most current PCs boot from NVMe. Only AHCI/SATA is supported. |
| ✅ | SATA (AHCI), several disks, GPT/MBR | ✅ | Since 2026-11-18 also what real controllers need: AHCI mode enabled, BIOS/OS handoff, staggered spin-up and COMRESET for a link that isn't up, waking a sleeping link, waiting for a busy device, and skipping optical drives and port multipliers. Checked in QEMU only, which needs none of it. |
| **1.0** | A common real Ethernet NIC (Intel e1000e/I219 and/or Realtek r8169) | ⚠️ | Since 2026-11-19 (`e1000.rs`): Intel e1000 (8254x), e1000e (82571-82574, 82583) and the chipset LAN of Intel PCs since 2010 (82577-82579, I217, I218, I219 through Meteor Lake), polled, joining the logical NIC beside virtio-net. Checked in QEMU with `e1000` and `e1000e` alone and beside virtio-net: DHCP, and git, sockets and TLS (to example.com too) match Windows. The chipset-LAN steps (PHY semaphore, LANPHYPC power-cycle) have no QEMU model and are untried. No Realtek driver yet: QEMU can't emulate an RTL8111. |
| | Wi-Fi | ❌ | No Wi-Fi driver. Since 2026-11-17 Quick Settings' network panel shows what's really there (the wired connection, its address and adapters) and says there's no Wi-Fi adapter, instead of four made-up networks. |
| **1.0** | Audio output (Intel HD Audio) | ⚠️ | Done 2026-11-11 in QEMU (`intel-hda` + `hda-output`): `audio.rs` drives the controller and its codec and mixes any number of streams at 48 kHz. The volume slider and the `SOUND` command set the master volume. Real codecs (Realtek etc.) are untested, and there's no input or headphone-jack switching. |
| | Laptop: battery, brightness, lid, touchpad | ❌ | Since 2026-11-11 the brightness slider dims the picture on its way to the screen (`ui.rs`, never below 10%, on every CPU). There's still no backlight control, and nothing for battery, lid or touchpad. |
| ✅ | Several displays, runtime resolution | ✅ | In a VM. Set in `/etc/display.cfg`, with no GUI for it. |
| | Interrupt-driven drivers (MSI) rather than polling | ⚠️ | Wastes power and CPU. Fine for 1.0. Looked at 2026-11-12 and left: `pci.rs` reads MSI/MSI-X, but no driver could use an interrupt yet. Kernel code switches threads only cooperatively, and the storage path holds spinlocks across a command, so AHCI can't block or yield while it waits (another thread on that CPU would spin on the same lock forever). It could only halt until the interrupt, which must then be sent to the waiting CPU, or each command waits up to a timer tick (1 ms). The network is polled from the main loop every millisecond, and sound from the timer. Doing it right needs sleeping locks in the storage stack first. |

## 3. Kernel and system core

| | Requirement | Status | Notes |
|---|---|---|---|
| ✅ | Preemptive multitasking, SMP, threads | ✅ | |
| **1.0** | Starts on a modern PC's CPU and timers: x2APIC, no 8254, AMD, many cores | ✅ | Done 2026-11-18 (`smp.rs`, `time.rs`). x2APIC mode when the firmware hands over in it or an APIC ID needs it (MADT type-9 entries, 32-bit IDs); up to 64 CPUs (was 8). With no 8254 the TSC rate comes from CPUID 0x15/0x16 or the HPET and the BSP ticks from its LAPIC timer. Checked in QEMU under TCG with 4 CPUs: `pit=off`, an AMD EPYC model, and x2APIC (forced with `VEILOS_X2APIC=1`, as no QEMU firmware hands over in it). |
| ✅ | Per-process address spaces, memory protection | ✅ | |
| ✅ | A crashing program doesn't take the OS down | ✅ | Win32 exceptions end the process. |
| **1.0** | ACPI: real power-off, reboot, power button | ⚠️ | Restart done 2026-11-18 (`power.rs`): the FADT's reset register, then port 0xCF9, the 8042's reset line, and a triple fault. `REBOOT` in the terminal (`REBOOT ACPI|CF9|KBD|TRIPLE` tries one), Restart in the Start menu, and any key on the crash screen or the no-system-volume screen. Each method checked in QEMU under TCG, signed in as admin (WHPX itself stops QEMU on any guest reset: "Unexpected VP exit code 4"). Power-off only works through QEMU's and VirtualBox's ports (`shell.rs`, `power_off`). Since 2026-11-11 the Quick Settings power button calls it too (checked in QEMU: the VM powers off), so it now does the same as the menu's Shut down. Real hardware still needs ACPI. |
| | Sleep / resume (S3) | ❌ | Needs an AML interpreter. |
| | Swap / pagefile, demand-zero memory | ⚠️ | Images page in from disk. Nothing pages out. |
| **1.0** | Runs for days without leaking or hanging (soak test) | ⚠️ | Never measured. |
| **1.0** | Fast enough to use on one CPU / a normal PC | ⚠️ | The S6 notes say building in the VM is too slow, and a small `hello.rs` takes about 36 s. Measure this on real hardware. |
| | Kernel panic screen + saved crash log | ✅ | Since 2026-11-17 (`crash.rs`): a panic stops the other CPUs (NMI), paints a crash screen with the report straight onto the display, and writes the report to `/var/log/crash.log` through sectors set aside at start-up (past the filesystem and the cache, as Windows writes its dump). The next start keeps it as `/var/log/crash-N.log` and says so in a notification. `CRASH TEST` in the terminal (administrators) checks it; checked in QEMU, report kept across the restart. No memory dump. Since 2026-11-18 any key restarts (PS/2, or USB with the firmware's PS/2 emulation). |

## 4. Files and data safety

| | Requirement | Status | Notes |
|---|---|---|---|
| ✅ | Read/write FAT32 with long names, rename, handles, seek | ✅ | |
| **1.0** | Never writes to a volume that isn't VEILOS's (an ESP, Windows, a USB stick) | ✅ | Done 2026-11-18 (`block.rs`). Only a volume with `VEILOS.VOL` at its root becomes `/` and is writable; every other filesystem mounts read-only at `/mnt/d<port>p<n>`, and each disk refuses writes outside the root volume's partition. With no VEILOS volume, boot stops on a screen listing what it found and writes nothing. `tools/mkdisk.py` and `storage/` carry the marker. Checked in QEMU with `tools/mkwindisk.py` (a GPT disk with an ESP, MSR and NTFS, attached before the VEILOS disk): signed in as admin, `MKDIR`/`RM` on both Windows volumes fail in Security and Performance Mode, reads work, and the image's SHA-256 is unchanged. Older disk images need `VEILOS.VOL` copied to their root. |
| **1.0** | Survives a power cut without corrupting the disk | ❌ | FAT has no journal. Needs a journaled or copy-on-write native FS, or at least fsck-on-boot. |
| | Write NTFS / exFAT (read Windows disks *and* USB sticks) | ⚠️ | Both are read-only. |
| **1.0** | File ownership and permissions between users | ❌ | FAT stores none ("everything is the user's"). |
| | Backups / versioning (your 3-phase ledger) | ❌ | See wishlist. |

## 5. Users and security

| | Requirement | Status | Notes |
|---|---|---|---|
| ✅ | Login with hashed passwords, lock screen, sign out | ✅ | PBKDF-SHA256 with 100k rounds. |
| **1.0** | Security Mode actually enforced on Win32 programs | ✅ | Done 2026-11-12 (`src/security.rs`; WIN32_DESIGN.md A3). Each Windows program gets the signed-in user's role capabilities, and its children get its own. The NT services check them: writing, creating and deleting files, starting programs, ending another program or touching its memory outside its own family, the network beyond loopback, windows and 3D. Denials answer ERROR_ACCESS_DENIED / WSAEACCES and show a notification. Checked in QEMU with `win32/tests/w26/sec1.c`: as admin the output is identical to Windows (only an unrelated program's memory is refused), and as guest writes, deletes, new processes and the network are refused while reading and loopback work. git (add, commit, rm, log, status through cmd) runs as admin with no denials. Git Bash's fork wasn't run (no Git Bash on the test disk). Files have no owners yet, so this is per role, not per folder. |
| **1.0** | Credential storage (`CredWriteW` etc.) | ✅ | Done 2026-11-17: the Credential Manager functions and DPAPI, sealed by the signed-in user's master key from the key ledger (WIN32_DESIGN.md A5). Checked against Windows 11 and across a reboot. claude.exe still keeps its login in a plain file, as it does on Windows; Git Credential Manager and programs using DPAPI now have somewhere safe. |
| | Encrypted keys persist across reboots | ✅ | Done 2026-11-12. Rechecked first: `cen_persist_key` did write, but `cen_init` was never called (so there was no master key and every save failed). The master key was also re-derived from a timer at each boot, `cen_load_keys` parsed nothing, keys and nonces came from counters (predictable keys, and nonces that repeat after a reboot) and nothing ever saved a key. Now each user has a random master key in `/etc/keys/<user>/master.vmk`, encrypted with a key derived from their sign-in password. It's unlocked at sign-in, when their saved keys load, and forgotten at sign-out. A password change re-wraps it; an admin reset removes that user's saved keys, which can't be opened any more. Keys, salts and nonces come from a new kernel random pool (`entropy.rs`). Checked in QEMU with the `KEYS TEST` / `KEYS CHECK` terminal commands: a key saved in one boot decrypts its file after a reboot, and again after a password change. No feature saves keys yet: the clipboard's keys are per entry and stay in memory. |
| | Firewall / per-app network permission | ❌ | |
| | TLS certificate revocation | ❌ | |

## 6. Desktop and everyday UX

| | Requirement | Status | Notes |
|---|---|---|---|
| ✅ | Window manager, taskbar, launcher, snapping/tiling | ✅ | |
| ✅ | Notifications (toasts) | ✅ | |
| ✅ | Terminal, cmd.exe, Git Bash | ✅ | |
| ✅ | Text editor (own + Windows Notepad), image viewer | ✅ | |
| **1.0** | File manager that can copy, move, rename, delete, make folders, and drag-and-drop | ⚠️ | Browses and opens files only. Everything else is done in the terminal. |
| **1.0** | Settings app with working controls (display, network, sound, users, date/time) | ⚠️ | Mostly `config.vsh` edits. Since 2026-11-17 the network panel shows the real wired connection (no made-up Wi-Fi list); the power button powers off since 2026-11-11. |
| **1.0** | Keyboard layouts other than US | ❌ | Hard-coded `Us104Key`. (Matters if you're in the UK.) |
| **1.0** | Time zone / local time | ⚠️ | The CMOS clock is treated as UTC. |
| **1.0** | Unicode text: box drawing, symbols, accents, emoji fallback | ⚠️ | Bitmap font gaps (`√`, `╌`). No CJK or emoji. |
| | Screenshots | ❌ | |
| | DPI scaling | ❌ | |
| | Basic accessibility (zoom, high contrast, keyboard-only navigation) | ❌ | |

## 7. Apps and getting things done

| | Requirement | Status | Notes |
|---|---|---|---|
| **1.0** | **A web browser** | ❌ | The biggest single gap. The realistic route is running a Windows browser (Firefox or a Chromium build) with software rendering. That needs more Win32 (DirectWrite/D2D or GDI fallbacks, COM, more of `ws2_32` overlapped I/O) and is a good target to drive the work. |
| **1.0** | Play music/video | ⚠️ | Sound and `winmm` (waveOut, PlaySound, Beep, MessageBeep) work since 2026-11-11. A Windows player (e.g. VLC or mpv) would also want WASAPI or DirectSound, which aren't there yet. |
| | PDF viewer, zip/unzip in the GUI | ❌ | SumatraPDF or 7-Zip through Win32 would cover them. |
| ✅ | Classic Win32 GUI programs, console programs | ✅ | Notepad, regedit, cmd, git, cargo, curl, claude.exe. |
| **1.0** | Install software: run common Windows installers (NSIS/Inno setup.exe, MSI) or a package manager that works | ⚠️ | Portable copy-and-run works. Installers are untested. `pkgmgr.rs` (checked 2026-11-17): nothing calls its build-script runner; compile and link already failed honestly, and sign (it signed the path of a missing file) and run (it said it started the program) now fail too. Installing a package only marks it installed in a list: no payload is unpacked, the five "built-in" SDK packages it lists don't exist on disk, and signing uses a key written in the source. A real one needs the native package format decided first. |
| **1.0** | Update VEILOS itself | ❌ | GitHub builds a release zip. Nothing updates from inside the OS. |
| | Games (D3D/OpenGL) | ❌ | G5, next on the Win32 plan. |

## 8. Networking

| | Requirement | Status | Notes |
|---|---|---|---|
| ✅ | DHCP, TCP/UDP, DNS, TLS (rustls + Schannel) | ✅ | |
| | IPv6 | ❌ | |
| | Overlapped sockets, `AcceptEx`/`ConnectEx` | ❌ | Browsers and Node/libuv want them. |
| | Network settings UI that's real | ❌ | |

---

## Shortest path to 1.0, roughly in order

1. **UEFI boot + GOP framebuffer.** Without these it won't start on most PCs.
2. **xHCI + USB HID + mass storage.** Without these, real machines have no keyboard or mouse.
3. **NVMe** and **one real NIC** (pick the one in your own PC).
4. **ACPI power-off/reboot** (a minimal AML interpreter, or the `acpi`/`aml` crates).
5. **Installer.**
6. **Crash-safe filesystem** (journal on your own FS, or fsck for FAT as a stopgap).
7. **HD Audio** + Win32 audio APIs. *(HD Audio and waveOut done in QEMU 2026-11-11; WASAPI/DirectSound left.)*
8. **A browser running.** This will drag in a lot of the remaining Win32 work.
9. **Settings and file manager made real**, keyboard layouts, time zone.
10. **Soak test on real hardware** and an update mechanism.

Steps 1-4 can be tested in QEMU (OVMF for UEFI, `-device qemu-xhci`, `-device nvme`, `-device e1000e`, `-device intel-hda`) before touching real hardware.

---

# Your wishlist (from `ideas.txt`, `goal.md`, `VEILOS_DESIGN.md`)

## Core design goals

| Goal | Status | Notes |
|---|---|---|
| Windows program compatibility | ✅ ⚠️ | Very far along for console and classic GUI programs (W1-S6). Games, 3D and modern UI frameworks aren't there yet. |
| Own native format alongside Windows' | ⚠️ | `vnr.rs` / `game_sdk.rs` exist. The October audit found them only initialised and never used. |
| Security Mode / Performance Mode live switch | ⚠️ | Since 2026-11-12 the two modes differ in speed and protection, not just in capability bits (WIN32_DESIGN.md A4; `MODE SECURITY|PERFORMANCE` in the terminal). Performance Mode: write-back disk cache flushed within a second (64 MiB), quiet logging, a longer time slice for the focused program, and Windows' own rules between a user's processes. Security Mode: write-through disk, the full audit trail in the serial log and `/var/log/audit.log`, every capability check, and `/system` and `/etc` changed only by root's programs. Both: SMEP, freed memory wiped. Measured in QEMU (`win32/tests/w28/bench1`): Performance Mode is 2.0x faster creating and writing files and 6x faster starting programs, and the same on everything else. Security Mode costs nothing measurable against how VEILOS ran before. Since 2026-11-14 also ASLR (per boot, both modes), W^X in Security Mode (code generators and MSYS2/Cygwin programs exempt) and SMAP (both modes); Git Bash, claude.exe and git still work in Security Mode. The ring-0 `policy::run_task` stays unused: running programs in ring 0 was ruled out. |
| Deep-root customisation (behaviour, not just colours) | ⚠️ | VScript (`config.vsh`, `SHELL.VSH`, themes) drives the shell, snapping and clipboard. Scheduler and policy customisation isn't exposed. |
| Adaptive Resource Engine (learns app usage) | ⚠️ | Since 2026-11-11 the profiles learn: every process exit records its session (`loader::on_process_exit`) and saves `/SCHEDPRO.BIN`. They are read back once the disk is mounted (before, the load ran before storage existed and always found nothing). Checked in QEMU across a reboot. Still open: the scheduler tracks one session at a time (a child process replaces its parent's), and whether a learned priority changes how threads are actually scheduled hasn't been checked. |
| Cluster Mode / VEIL-MESH (delegate, compute, offload, storage, controller, hub nodes) | ⚠️ | `distributed.rs`, `task_capsule.rs` and `checkpoint.rs` exist, with an offload policy in `config.vsh`. The audit found screen capture and input injection faked and the mesh GPU backend deferred. Node types other than offload/compute aren't there. |
| Anti-cheat compatibility / hypervisor | ⚠️ | Scaffolds (`anticheat.rs`, `hypervisor.rs`). |

## ideas.txt master list

**Core UX foundations**
- [x] Unified Settings Hub *(you marked it done; it works through `config.vsh` plus the settings panel. Some controls are still fake, see §6)*
- [x] Consistent window manager: tile snapping, snap lines, magnetic edges *(`WindowSnapMode` Magnetic/Sticky/Slippery/Off, `WINDOW_TILES`)*
- [ ] Global search palette
- [~] Notification centre *(toasts with categories exist; no grouped history or per-app controls)*
- [~] Package manager GUI *(`pkgmgr.rs`/`app_repo.rs` keep a list of packages but install nothing; see §8, "Install software")*
- [~] User profiles and permissions *(users, roles and login are real; per-app permissions aren't enforced on Win32)*

**Premium UX**
- [ ] Universal command palette
- [~] Adaptive theming engine *(VScript themes, `THEMES/DARK.VSH`; no dynamic or per-app themes)*
- [~] Unified file dialogs *(comdlg32 Open/Save for Win32 apps; no native dialog for VEILOS apps)*
- [ ] Quick actions bar (screenshots, clipboard history, screen recording) *(the clipboard history exists, the bar doesn't)*
- [ ] Multi-desktop workspaces
- [~] Smart window layouts *(ratio tiles via `WINDOW_TILE_SIZE_RATIO`; no golden-ratio/70-30 presets)*

**Experimental / unique**
- [~] Per-app sandboxing controls *(`sandbox.rs`/`capability.rs`; no UI, not applied to Win32)*
- [~] Live system introspection *(`DEVICES`, `WORKERS`, a PC sampler and `debugger.rs`; no visualiser)*
- [ ] Modular shell layers
- [ ] Native overlay system
- [ ] Task-oriented workflows
- [ ] AI-assisted system tuning
- [ ] Deep customisation profiles (Gaming/Privacy/Creator/Low-power)
- [x] Unified clipboard timeline *(history log, `CLIPBOARD_LOG_SIZE`; previews are text only)*
- [~] VEIL-MESH *(see above)*
- [ ] 3-phase ledger (directory ledger, directory metadata ledger, system ledger) *(no ledger code in the FS; the only "ledger" is the encryption key ledger)*
- [~] Custom VEIL file types with native encryption *(`encryption.rs` exists; no file type, and the audit found keys not persisted)*

**Developer-focused**
- [~] Syscall inspector *(serial logging, `debugger.rs`; nothing live)*
- [x] Rust tooling integration *(cargo and rustc run on VEILOS, S2; no native debugger)*
- [~] Unified logging framework *(one serial log; no viewer)*
- [~] Virtualised test environments *(`hypervisor.rs` scaffold)*
- [ ] Custom file format integration (previews, metadata, encryption)

**Personality and behaviour**
- [ ] Personality profiles
- [ ] Predictive workspace flow
- [ ] Zero-latency windowing (<5 ms input-to-pixel) *(not measured)*
- [ ] Deep freeze sessions (save/restore OS state)
- [ ] Semantic file system (tags, projects)
- [ ] Per-window security zones
- [ ] Live kernel controls (scheduler/memory/I/O)
- [ ] Composable UI blocks *(SHELL.VSH elements are a start)*
- [ ] Context-aware command palette
- [ ] Instant app virtualisation
- [ ] Self-healing system *(only the registry: a damaged file is kept as `registry.bad`)*
- [ ] Native game overlay framework

**Clipboard and data safety**
- [x] Secure clipboard channels: Public, Private, Sensitive, App-restricted
- [ ] Secure drag-and-drop channels *(checked 2026-11-17: there never was drag-and-drop channel code, in the tree or its history. The channels (PUBLIC, PRIVATE, SENSITIVE, APP_RESTRICTED) are the clipboard's (`clipboard.rs`, encrypted). Drag-and-drop is the desktop copying a file between File Manager windows or to the desktop; since 2026-11-17 it checks the signed-in user may write there (a guest can't, and in Security Mode only root changes /system and /etc), as their programs must. Windows programs' OLE drag-and-drop (`RegisterDragDrop`) is a stub.)*

**Smart security UX**
- [~] Smart error explanations *(missing-import errors name the function; nothing more general)*
- [ ] Live file metadata inspector
- [ ] Process personality profiles

**System hygiene**
- [ ] True app uninstallation
- [ ] System hygiene dashboard
- [ ] Automatic workspace cleanup
- [ ] Cold-boot resume

**Windowing**
- [ ] Universal back/forward navigation
- [ ] Drag-anywhere window movement
- [x] Magnetic window edges
- [ ] Window intent modes

**File explorer**
- [ ] Predictive file actions *(a "frequent files" list exists)*
- [ ] Intent-based file actions
- [ ] Smart explorer tabs
- [ ] Cross-app panels
- [ ] Cross-app undo history

**Predictive and adaptive**
- [ ] Predictive performance boosts
- [~] Adaptive performance scheduler *(see Adaptive Resource Engine)*
- [ ] Adaptive UI density
- [ ] Intent-based notifications

**Tally (ideas.txt list):** 6 of 62 done, 15 partly done, 41 not started.
