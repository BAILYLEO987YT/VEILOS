# VEILOS 1.0 checklist

**What 1.0 means here:** someone could install VEILOS on an ordinary PC and use it every day (browse, write, manage files, listen to music, install software, shut down) without going back to Windows for routine things. It doesn't need polish, just the basics.

Status is as of 2026-11-10, worked out from the code and the design docs. Nothing was run to check it.
✅ done · ⚠️ partly done / only in a VM · ❌ missing.
**1.0** marks items that block a 1.0. The others are nice to have.

---

## 1. Boot and install on a real PC

| | Requirement | Status | Notes |
|---|---|---|---|
| **1.0** | UEFI boot | ❌ | `bootloader 0.9` is BIOS-only. Most PCs since about 2020 have CSM (legacy boot) off or removed. |
| **1.0** | Boots and runs on at least one real machine | ⚠️ | Only QEMU (TCG/WHPX) is tested so far. |
| **1.0** | Installer: partition a disk, copy the system, set up boot | ❌ | Today it runs from a disk image built by `tools/mkdisk.py`. |
| **1.0** | Graphics from the firmware framebuffer (GOP/VBE) on any GPU | ❌ | Only the Bochs/QEMU display adapter is driven, and the bootloader's framebuffer isn't used. |
| | Recovery / safe boot when something breaks | ❌ | |

## 2. Hardware drivers

| | Requirement | Status | Notes |
|---|---|---|---|
| **1.0** | USB (xHCI) with HID keyboard and mouse | ❌ | Input is PS/2 only. Laptops and most desktops need USB HID, or I2C HID for touchpads. |
| **1.0** | USB mass storage (flash drives) | ❌ | Needs xHCI first. |
| **1.0** | NVMe | ❌ | Most current PCs boot from NVMe. Only AHCI/SATA is supported. |
| ✅ | SATA (AHCI), several disks, GPT/MBR | ✅ | |
| **1.0** | A common real Ethernet NIC (Intel e1000e/I219 and/or Realtek r8169) | ❌ | Only virtio-net is supported (and bonded). |
| | Wi-Fi | ❌ | The networks listed in Quick Settings are hard-coded in `apps.rs`. |
| **1.0** | Audio output (Intel HD Audio) | ⚠️ | Done 2026-11-11 in QEMU (`intel-hda` + `hda-output`): `audio.rs` drives the controller and its codec and mixes any number of streams at 48 kHz. The volume slider and the `SOUND` command set the master volume. Real codecs (Realtek etc.) are untested, and there's no input or headphone-jack switching. |
| | Laptop: battery, brightness, lid, touchpad | ❌ | Since 2026-11-11 the brightness slider dims the picture on its way to the screen (`ui.rs`, never below 10%, on every CPU). There's still no backlight control, and nothing for battery, lid or touchpad. |
| ✅ | Several displays, runtime resolution | ✅ | In a VM. Set in `/etc/display.cfg`, with no GUI for it. |
| | Interrupt-driven drivers (MSI) rather than polling | ⚠️ | Wastes power and CPU. Fine for 1.0. Looked at 2026-11-12 and left: `pci.rs` reads MSI/MSI-X, but no driver could use an interrupt yet. Kernel code switches threads only cooperatively, and the storage path holds spinlocks across a command, so AHCI can't block or yield while it waits (another thread on that CPU would spin on the same lock forever). It could only halt until the interrupt, which must then be sent to the waiting CPU, or each command waits up to a timer tick (1 ms). The network is polled from the main loop every millisecond, and sound from the timer. Doing it right needs sleeping locks in the storage stack first. |

## 3. Kernel and system core

| | Requirement | Status | Notes |
|---|---|---|---|
| ✅ | Preemptive multitasking, SMP, threads | ✅ | |
| ✅ | Per-process address spaces, memory protection | ✅ | |
| ✅ | A crashing program doesn't take the OS down | ✅ | Win32 exceptions end the process. |
| **1.0** | ACPI: real power-off, reboot, power button | ❌ | Power-off only works through QEMU's and VirtualBox's ports (`shell.rs`, `power_off`). Since 2026-11-11 the Quick Settings power button calls it too (checked in QEMU: the VM powers off), so it now does the same as the menu's Shut down. Real hardware still needs ACPI. |
| | Sleep / resume (S3) | ❌ | Needs an AML interpreter. |
| | Swap / pagefile, demand-zero memory | ⚠️ | Images page in from disk. Nothing pages out. |
| **1.0** | Runs for days without leaking or hanging (soak test) | ⚠️ | Never measured. |
| **1.0** | Fast enough to use on one CPU / a normal PC | ⚠️ | The S6 notes say building in the VM is too slow, and a small `hello.rs` takes about 36 s. Measure this on real hardware. |
| | Kernel panic screen + saved crash log | ⚠️ | Logs go to serial only. |

## 4. Files and data safety

| | Requirement | Status | Notes |
|---|---|---|---|
| ✅ | Read/write FAT32 with long names, rename, handles, seek | ✅ | |
| **1.0** | Survives a power cut without corrupting the disk | ❌ | FAT has no journal. Needs a journaled or copy-on-write native FS, or at least fsck-on-boot. |
| | Write NTFS / exFAT (read Windows disks *and* USB sticks) | ⚠️ | Both are read-only. |
| **1.0** | File ownership and permissions between users | ❌ | FAT stores none ("everything is the user's"). |
| | Backups / versioning (your 3-phase ledger) | ❌ | See wishlist. |

## 5. Users and security

| | Requirement | Status | Notes |
|---|---|---|---|
| ✅ | Login with hashed passwords, lock screen, sign out | ✅ | PBKDF-SHA256 with 100k rounds. |
| **1.0** | Security Mode actually enforced on Win32 programs | ✅ | Done 2026-11-12 (`src/security.rs`; WIN32_DESIGN.md A3). Each Windows program gets the signed-in user's role capabilities, and its children get its own. The NT services check them: writing, creating and deleting files, starting programs, ending another program or touching its memory outside its own family, the network beyond loopback, windows and 3D. Denials answer ERROR_ACCESS_DENIED / WSAEACCES and show a notification. Checked in QEMU with `win32/tests/w26/sec1.c`: as admin the output is identical to Windows (only an unrelated program's memory is refused), and as guest writes, deletes, new processes and the network are refused while reading and loopback work. git (add, commit, rm, log, status through cmd) runs as admin with no denials. Git Bash's fork wasn't run (no Git Bash on the test disk). Files have no owners yet, so this is per role, not per folder. |
| **1.0** | Credential storage (`CredWriteW` etc.) | ❌ | claude.exe's login sits in a plain file. The key ledger (§5 below, done 2026-11-12) is what it can be built on: per-user, unlocked by the sign-in password. |
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
| **1.0** | Settings app with working controls (display, network, sound, users, date/time) | ⚠️ | Mostly `config.vsh` edits. The Wi-Fi list is fake and the power button does nothing. |
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
| **1.0** | Install software: run common Windows installers (NSIS/Inno setup.exe, MSI) or a package manager that works | ⚠️ | Portable copy-and-run works. Installers are untested. The October audit found `pkgmgr.rs` faked its build steps. |
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
| Security Mode / Performance Mode live switch | ⚠️ | Since 2026-11-12 the two modes differ in speed and protection, not just in capability bits (WIN32_DESIGN.md A4; `MODE SECURITY|PERFORMANCE` in the terminal). Performance Mode: write-back disk cache flushed within a second (64 MiB), quiet logging, a longer time slice for the focused program, and Windows' own rules between a user's processes. Security Mode: write-through disk, the full audit trail in the serial log and `/var/log/audit.log`, every capability check, and `/system` and `/etc` changed only by root's programs. Both: SMEP, freed memory wiped. Measured in QEMU (`win32/tests/w28/bench1`): Performance Mode is 2.0x faster creating and writing files and 6x faster starting programs, and the same on everything else. Security Mode costs nothing measurable against how VEILOS ran before. Not done yet: ASLR, W^X and SMAP for Security Mode (each needs care: Git Bash relies on fixed DLL addresses, claude.exe's JavaScript JIT on writable code, and SMAP on wrapping every kernel access to user memory). The ring-0 `policy::run_task` stays unused: running programs in ring 0 was ruled out. |
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
- [~] Package manager GUI *(`pkgmgr.rs`/`app_repo.rs` exist; the audit found them faking work)*
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
- [ ] Secure drag-and-drop channels *(you marked it done, but I couldn't find channel code for drag-and-drop: `GLOBAL_DRAG_DROP` in `apps.rs` only carries a path. It may have been lost; worth checking)*

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
