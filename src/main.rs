// -----| VEILOS Kernel Entry |-----

#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]
// #![feature(naked_functions)] // Stable since 1.88

extern crate alloc;

#[macro_use]
pub mod encryption;
pub mod vga_buffer;
pub mod power;
pub mod usb;
pub mod e1000;
pub mod nvme;
pub mod r8169;
pub mod iwlwifi;
pub mod wpa;
pub mod klog;
pub mod interrupts;
pub mod gdt;
pub mod allocator;
pub mod memory;
pub mod task;
pub mod pci;
pub mod ahci;
pub mod ide;
pub mod vfs;
pub mod fat;
pub mod exfat;
pub mod ntfs;
pub mod executable;
pub mod policy;
pub mod subsystem;
pub mod sampler;
pub mod process;
pub mod loader;
pub mod dll;
pub mod graphics;
pub mod ui;
pub mod nt_objects;
pub mod nt_memory;
pub mod nt_thread;
pub mod win32_user;
pub mod gpu_driver;
pub mod gfx_api;
pub mod compositor;
pub mod capability;
pub mod sandbox;
pub mod toolchain;
pub mod debugger;
pub mod pkgmgr;
pub mod gui_toolkit;
pub mod desktop;
pub mod apps;
pub mod stability;
pub mod distribution;
pub mod scheduler;
pub mod vnr;
pub mod app_repo;
pub mod smp;
pub mod time;
pub mod ioapic;
pub mod kthread;
pub mod address_space;
pub mod block;
pub mod install;
pub mod ntos;
pub mod hal;
pub mod hotplug;
pub mod modules;
pub mod console;
pub mod nt_syscalls;
pub mod kusd;
pub mod nt_file;
pub mod nt_exception;
pub mod nt_process;
pub mod nt_user;
pub mod nt_registry;
pub mod nt_demand;
pub mod nt_ipc;
pub mod nt_section;
pub mod nt_socket;
pub mod conbuf;
pub mod nt_console;
pub mod pty;
pub mod workers;
pub mod lgpu;
pub mod raster3d;
pub mod nt_gpu3d;
pub mod nt_sound;
pub mod credentials;
pub mod crash;
pub mod entropy;
pub mod security;
pub mod sysprof;
pub mod aslr;
pub mod virtio_net;
pub mod game_sdk;
pub mod network;
pub mod hypervisor;
pub mod tls;
pub mod anticheat;
pub mod distributed;
pub mod task_capsule;
pub mod checkpoint;
pub mod users;
pub mod vscript;
pub mod vsh_lang;
pub mod audio;
pub mod gif_decoder;
pub mod clipboard;
pub mod compression;
pub mod font;
pub mod render2d;
pub mod render2d_kernel;
pub mod ui_runtime;
pub mod shell;
use core::panic::PanicInfo;
use core::fmt::{self, Write};
use spin::Mutex;
use uart_16550::SerialPort;
use pc_keyboard::{layouts, DecodedKey, HandleControl, Keyboard, ScancodeSet1};
use bootloader_api::{BootInfo, BootloaderConfig, entry_point};

/// Persistent keyboard decoder state — shared across all frames of the
/// gui_refresh_task so that modifier keys (Shift, Caps-Lock) are remembered.
static KB: spin::Mutex<Option<Keyboard<layouts::Us104Key, ScancodeSet1>>> =
    spin::Mutex::new(None);

// -----| serial port |-----

/// The standard serial port address for COM1.
const COM1_ADDR: u16 = 0x3F8;

/// Global interface for serial port output.
pub static SERIAL_WRITER: Mutex<SerialPort> = Mutex::new(unsafe {
    SerialPort::new(COM1_ADDR)
});

/// Prints to the host across the serial port.
#[macro_export]
macro_rules! serial_print {
    ($($arg:tt)*) => ($crate::_serial_print(format_args!($($arg)*)));
}

/// Prints to the host across the serial port, appending a newline.
#[macro_export]
macro_rules! serial_println {
    () => ($crate::serial_print!("\n"));
    ($($arg:tt)*) => ($crate::serial_print!("{}\n", format_args!($($arg)*)));
}

/// Detailed serial logging (loader, modules, thread and exit details): on in
/// Security Mode, where it is part of the audit trail, off in Performance
/// Mode, where each byte is an I/O-port write (a VM exit in a VM).
pub static VERBOSE_LOG: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(true);

/// `serial_println!` while `VERBOSE_LOG` is on.
#[macro_export]
macro_rules! trace_println {
    ($($arg:tt)*) => {
        if $crate::VERBOSE_LOG.load(core::sync::atomic::Ordering::Relaxed) {
            $crate::serial_println!($($arg)*);
        }
    };
}

// -----| vga macros |-----

#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => ($crate::vga_buffer::_print(format_args!($($arg)*)));
}

#[macro_export]
macro_rules! println {
    () => ($crate::print!("\n"));
    ($($arg:tt)*) => ($crate::print!("{}\n", format_args!($($arg)*)));
}

#[doc(hidden)]
pub fn _serial_print(args: fmt::Arguments) {
    // We disable interrupts while locking the serial port to prevent deadlocks
    x86_64::instructions::interrupts::without_interrupts(|| {
        SERIAL_WRITER.lock().write_fmt(args).expect("Printing to serial failed");
    });
    // Kept in memory too, for DMESG on machines with no serial port.
    crate::klog::record(args);
}

// -----| kernel entry point |-----

/// What the bootloader (0.11) sets up before kernel_main. The physical-memory
/// map and the bootloader's other mappings (stack, boot info, framebuffer)
/// go in the upper half: the low 512 GiB holds the kernel image and every
/// process's user space (address_space.rs).
pub static BOOTLOADER_CONFIG: BootloaderConfig = {
    use bootloader_api::config::Mapping;
    let mut config = BootloaderConfig::new_default();
    config.mappings.physical_memory = Some(Mapping::Dynamic);
    config.mappings.dynamic_range_start = Some(0xFFFF_8000_0000_0000);
    config.kernel_stack_size = 1024 * 1024;
    config
};

entry_point!(kernel_main, config = &BOOTLOADER_CONFIG);

/// Halt with a fatal message on critical boot failures.
/// Unlike panic!, this is intentionally minimal — it runs before the heap
/// or IDT are guaranteed to be live, so it only touches serial + VGA.
macro_rules! boot_assert {
    ($result:expr, $msg:literal) => {
        match $result {
            Ok(v) => v,
            Err(e) => {
                $crate::serial_println!("[BOOT FATAL] {}: {:?}", $msg, e);
                $crate::vga_buffer::VGA_WRITER.lock().set_colour(
                    $crate::vga_buffer::VgaColour::White,
                    $crate::vga_buffer::VgaColour::Red,
                );
                $crate::println!("\n[BOOT FATAL] {}", $msg);
                loop { unsafe { core::arch::asm!("hlt"); } }
            }
        }
    };
}

/// Keep what the bootloader found that the rest of boot needs: the
/// framebuffer (the boot console draws on it from here on) and the ACPI RSDP
/// (UEFI machines have none where smp.rs used to look).
fn take_boot_info(boot_info: &mut BootInfo) {
    use bootloader_api::info::PixelFormat;
    if let Some(rsdp) = boot_info.rsdp_addr.into_option() {
        crate::smp::set_rsdp(rsdp);
    }
    if let Some(addr) = boot_info.ramdisk_addr.into_option() {
        crate::block::set_ram_disk(addr, boot_info.ramdisk_len as usize);
    }
    // Where the kernel's ELF file is: block::choose_root finds the disk it
    // came from by looking for it on each boot partition.
    crate::block::set_kernel_image(boot_info.kernel_addr, boot_info.kernel_len);
    if let Some(fb) = boot_info.framebuffer.as_mut() {
        let info = fb.info();
        let virt = fb.buffer_mut().as_mut_ptr() as u64;
        let pixels = match info.pixel_format {
            PixelFormat::Rgb => Some(vga_buffer::Pixels::Rgb),
            PixelFormat::Bgr => Some(vga_buffer::Pixels::Bgr),
            PixelFormat::U8 => Some(vga_buffer::Pixels::Grey),
            _ => None,
        };
        serial_println!(
            "[BOOT] framebuffer {}x{}, {} bytes/pixel, {:?}, stride {}",
            info.width, info.height, info.bytes_per_pixel, info.pixel_format, info.stride
        );
        if let Some(pixels) = pixels {
            // The bootloader maps the framebuffer at `virt`; its physical
            // address is found again once paging is known (graphics.rs).
            vga_buffer::init_framebuffer(vga_buffer::FirmwareFb {
                virt,
                byte_len: info.byte_len,
                width: info.width,
                height: info.height,
                stride: info.stride,
                bytes_per_pixel: info.bytes_per_pixel,
                pixels,
            });
        }
    } else {
        serial_println!("[BOOT] no framebuffer from the firmware");
    }
}

/// No disk holds a VEILOS volume (block.rs: the `VEILOS.VOL` marker). Stop
/// here, before anything is written: every other volume stays untouched.
fn no_system_volume() -> ! {
    vga_buffer::VGA_WRITER.lock().set_colour(vga_buffer::VgaColour::White, vga_buffer::VgaColour::Red);
    println!();
    println!(" VEILOS could not find its system volume.                                       ");
    vga_buffer::VGA_WRITER.lock().set_colour(vga_buffer::VgaColour::LightGray, vga_buffer::VgaColour::Black);
    println!();
    println!(" No disk has a filesystem with {} at its root, so VEILOS has", crate::block::VOLUME_MARKER);
    println!(" stopped here. It has written nothing to any disk. The volumes it found");
    println!(" are listed above; they were only read.");
    println!();
    println!(" Make a VEILOS disk with tools/mkdisk.py, or copy storage/VEILOS.VOL to the");
    println!(" root of the FAT32 volume VEILOS should use. Press any key to restart.");
    serial_println!("[BOOT] no VEILOS system volume ({} not found on any disk): stopped, nothing written", crate::block::VOLUME_MARKER);
    // The same on a framebuffer, for machines with no VGA text mode (UEFI,
    // a display with no VGA core). Binding the display writes to no disk.
    crate::hal::bind(crate::hal::DeviceClass::Display);
    let volumes = crate::block::found_volumes();
    let mut body = alloc::string::String::from("Volumes found (each was only read):\n");
    if volumes.is_empty() {
        body.push_str("  none\n");
    }
    for v in &volumes {
        body.push_str("  ");
        body.push_str(v);
        body.push('\n');
    }
    crate::crash::paint_stop(
        b"VEILOS COULD NOT FIND ITS SYSTEM VOLUME.",
        &[
            b"NO DISK HAS A FILESYSTEM WITH VEILOS.VOL AT ITS ROOT, SO VEILOS STOPPED HERE.",
            b"IT HAS WRITTEN NOTHING TO ANY DISK.",
            b"MAKE A VEILOS DISK WITH TOOLS/MKDISK.PY, OR COPY STORAGE/VEILOS.VOL TO THE ROOT",
            b"OF THE FAT32 VOLUME VEILOS SHOULD USE. PRESS ANY KEY TO RESTART.",
        ],
        body.as_bytes(),
    );
    crate::power::wait_key_then_reset()
}

fn kernel_main(boot_info: &'static mut BootInfo) -> ! {
    // -----| initialization |-----
    SERIAL_WRITER.lock().init();
    take_boot_info(boot_info);
    let boot_info: &'static BootInfo = boot_info;
    
    // Welcome message to Serial Terminal
    serial_println!("\n\n\n");
    serial_print!(
        r"
         ____   _______________.___.____    ________    _________
         \   \ /   /\_   _____/|   |    |   \_____  \  /   _____/
          \   Y   /  |    __)_ |   |    |    /   |   \ \_____  \ 
           \     /   |        \|   |    |___/    |    \/        \
            \___/   /_______  /|___|_______ \_______  /_______  /
                            \/             \/       \/        \/ 
        "
    );
    serial_println!("\n\n\n");
    
    // Welcome message to Screen
    vga_buffer::VGA_WRITER.lock().set_colour(
        vga_buffer::VgaColour::LightCyan, 
        vga_buffer::VgaColour::Black
    );
    println!("VEILOS v0.1.0");
    
    vga_buffer::VGA_WRITER.lock().set_colour(
        vga_buffer::VgaColour::LightGreen, 
        vga_buffer::VgaColour::Black
    );
    println!("System starting...");

    // Set up GDT and TSS for Double Fault handling
    gdt::init();

    // Enable SSE on the BSP (APs do the same in smp::ap_entry).
    crate::smp::enable_sse();
    // Write-combining memory type for framebuffers (same PAT on every CPU).
    crate::smp::init_pat();
    crate::serial_println!("[INIT] SSE enabled (CR4.OSFXSR+OSXMMEXCPT, CR0.EM/TS cleared)");

    // Calibrate the TSC against the PIT: the clock for delay_us and driver timeouts.
    crate::time::init();
    crate::entropy::init();

    // Set up IDT and Hardware Interrupts (PIC)
    interrupts::init_idt();
    unsafe { interrupts::PICS.lock().initialize() };
    // Which lines start enabled: the timer, the keyboard and the cascade (the
    // mouse enables IRQ 12 itself). `initialize` keeps the firmware's masks,
    // and the I/O APIC set-up copies them: a BIOS leaves these open, but UEFI
    // firmware masks every line, and then no timer tick or key ever arrived.
    unsafe { interrupts::PICS.lock().write_masks(0xF8, 0xFF) };
    x86_64::instructions::interrupts::enable();
    
    // Set up Advanced Memory Management (Paging and Frame Allocation)
    use x86_64::VirtAddr;
    
    let phys_mem_offset = VirtAddr::new(
        boot_info.physical_memory_offset.into_option().expect("the bootloader didn't map physical memory"),
    );
    let mut mapper = unsafe { memory::init(phys_mem_offset) };
    memory::init_frame_allocator(&boot_info.memory_regions);
    let mut frame_allocator = memory::GlobalFrameAllocator;
    
    println!("Memory Map loaded.");
    
    // Set up Global Allocator for Heap memory (Vec, Box, String)
    boot_assert!(
        allocator::init_heap(&mut mapper, &mut frame_allocator),
        "Heap initialization failed — cannot continue"
    );

    // Heap is now live — safe to initialise the adaptive scheduler.
    crate::scheduler::init();
    // Windows drivers' pool and image region: before any process exists, so
    // every address space shares its tables (ntos/pool.rs).
    crate::ntos::init();
    println!("Heap Initialized with Physical RAM Paging.");
    // Without PIT channel 2 or a CPUID rate: the TSC against the HPET.
    crate::time::calibrate_late();

    // Register kernel process (PID 0) with full capabilities for system services.
    crate::process::PROCESSOR.lock().register_kernel();

    // -----| SMP: start Application Processors |-----
    // Must happen after the heap (so the LAPIC page can be mapped), after
    // GDT+IDT are live (APs load them immediately on wake), and before the
    // scheduler tries to dispatch work across CPUs.
    crate::smp::init(&mut mapper, &mut frame_allocator);

    // Move device IRQs from the 8259 to the I/O APIC (needs the LAPIC from smp::init).
    crate::ioapic::init(&mut mapper, &mut frame_allocator);
    // ACPI power management: FADT, \_S5, ACPI mode, the power button.
    crate::power::init();

    // -----| Hardware Enumeration |-----
    vga_buffer::VGA_WRITER.lock().set_colour(
        vga_buffer::VgaColour::LightCyan, 
        vga_buffer::VgaColour::Black
    );
    println!("\nScanning Peripheral Component Interconnect (PCI) Bus...");
    vga_buffer::VGA_WRITER.lock().set_colour(
        vga_buffer::VgaColour::White, 
        vga_buffer::VgaColour::Black
    );
    
    // Enumerate the bus once and populate the device registry. Every driver
    // queries the registry from here on; nothing may rescan the bus.
    pci::init();
    crate::hal::init();
    // Hot-plug slots: empty ones are powered off before drivers bind.
    crate::hotplug::init();

    // Storage first: the filesystems are needed by most of the rest of boot.
    crate::block::mount_ram_disk();
    crate::hal::bind(crate::hal::DeviceClass::Storage);
    // USB too before the root volume is settled: it may be on a USB stick.
    crate::hal::bind(crate::hal::DeviceClass::SerialBus);
    // Every disk is mounted read-only so far: make the VEILOS volume on the
    // disk the kernel booted from `/` (not just the first one found).
    crate::block::choose_root();
    if !crate::block::root_mounted() {
        no_system_volume();
    }
    crate::vfs::ensure_standard_dirs();
    crate::scheduler::load_profiles();
    crate::crash::prepare();
    if crate::block::root_on_ram_disk() {
        crate::desktop::notify(
            crate::desktop::NotifCategory::Warning,
            "VEILOS",
            "Running from memory (RAM disk): nothing you change is kept after a restart",
        );
    }

    // Phase 13: the network stack (VirtIO-net + smoltcp) starts when its NIC binds.
    crate::hal::bind(crate::hal::DeviceClass::Network);

    // Phase 15: initialise the hypervisor (VT-x VMX + EPT).
    // Only detects support; VMX root mode is entered when a VM starts.
    crate::hypervisor::init();

    // Phase 13 (TLS): load certificate store + initialise TLS layer.
    crate::tls::init();

    // Phase 14: anti-cheat compatibility layer.
    crate::anticheat::init();

    // Phase 16: distributed VEILOS (node mesh, DFS, remote exec, cloud gaming).
    crate::distributed::init();

    // Also check for legacy IDE hardware
    // ide::init(&hardware_tree);

    // Let's reset the colour back to standard for general text
    vga_buffer::VGA_WRITER.lock().set_colour(
        vga_buffer::VgaColour::Yellow,
        vga_buffer::VgaColour::Black
    );
    println!("\nInterrupts enabled, Memory mapped, PCI Scanned. Ready. Type away!\n");
    vga_buffer::VGA_WRITER.lock().set_colour(
        vga_buffer::VgaColour::LightGray,
        vga_buffer::VgaColour::Black
    );

    // -----| multitasking setup |-----

    crate::hal::bind(crate::hal::DeviceClass::Display);
    crate::hal::bind(crate::hal::DeviceClass::Multimedia);
    // Every class is bound: devices nobody drives can sleep.
    crate::hal::power_down_idle();
    if crate::graphics::FRAMEBUFFER.lock().is_none() {
        println!("[GRAPHICS] No supported display adapter. Falling back to VGA Text Mode.");
    }

    serial_println!("[BOOT] >> dll::init ...");
    crate::dll::init();
    serial_println!("[BOOT] >> nt_objects::init ...");
    crate::nt_objects::init();
    serial_println!("[BOOT] >> nt_thread::init ...");
    crate::nt_thread::init();
    serial_println!("[BOOT] >> win32_user::init ...");
    crate::win32_user::init();
    serial_println!("[BOOT] >> sandbox::init ...");
    crate::sandbox::init();
    serial_println!("[BOOT] >> gpu_driver::init ...");
    crate::gpu_driver::init();
    serial_println!("[BOOT] >> gfx_api::init ...");
    crate::gfx_api::init();
    serial_println!("[BOOT] >> compositor::init ...");
    crate::compositor::init();
    serial_println!("[BOOT] >> toolchain::init ...");
    crate::toolchain::init();
    serial_println!("[BOOT] >> debugger::init ...");
    crate::debugger::init();
    serial_println!("[BOOT] >> pkgmgr::init ...");
    crate::pkgmgr::init();
    serial_println!("[BOOT] >> users::init ...");
    crate::users::init();
    serial_println!("[BOOT] >> gui_toolkit::init ...");
    crate::gui_toolkit::init();
    serial_println!("[BOOT] >> desktop::init ...");
    crate::desktop::init();
    serial_println!("[BOOT] >> apps::init ...");
    crate::apps::init();
    serial_println!("[BOOT] >> stability::init ...");
    crate::stability::init();
    serial_println!("[BOOT] >> distribution::init ...");
    crate::distribution::init();
    serial_println!("[BOOT] >> vnr::init ...");
    crate::vnr::init();
    serial_println!("[BOOT] >> app_repo::init ...");
    crate::app_repo::init();
    serial_println!("[BOOT] >> game_sdk::init ...");
    crate::game_sdk::init();
    serial_println!("[BOOT] >> compression::init ...");
    crate::compression::init();
    serial_println!("[BOOT] << All subsystems initialised.");

    // Register Windows in WM
    crate::interrupts::init_pit();
    crate::interrupts::check_tick_source();
    crate::ui::init_back_buffer();

    // Initialize Input Subsystems
    task::keyboard::initialize();
    // NOTE: mouse hardware init (PS/2 reset, handshake, IRQ unmask) now runs
    // fully inside the async mouse_tick() task via hw_init().await, so a slow
    // or absent PS/2 controller can never block kernel_main from reaching
    // executor.run(). No synchronous mouse::initialize() call needed here.
    
    // Phase 18: the desktop starts at the login screen.
    // paint_desktop() will render the correct phase on the first vsync tick.
    
    crate::address_space::log_kernel_layout();

    let mut executor = task::executor::Executor::new();
    
    // Spawn core kernel tasks
    executor.spawn(task::Task::new(task::mouse::mouse_tick()));
    executor.spawn(task::Task::new(gui_refresh_task()));
    executor.spawn(task::Task::new(crate::scheduler::reaper_task()));
    executor.spawn(task::Task::new(crate::usb::hotplug_task()));
    
    // (All non-blocking system apps are initialized in apps::init())
    
    println!("Starting Coöperative App Scheduler...");
    executor.run();
}

// -----| async apps |-----

/// Atomic flag: set to 1 when the 0xE0 extended prefix has been seen,
/// so the next scancode 0x5B is recognised as Left Super.
static EXTENDED_PREFIX: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);
/// Where we are in the Pause key's E1 sequence: 0 outside it, else how many
/// of its six bytes have arrived.
static PAUSE_SEQ: core::sync::atomic::AtomicU8 = core::sync::atomic::AtomicU8::new(0);

// -----| Keyboard modifier state |-----
// Tracked at the raw scancode level so they are available globally
// for clipboard shortcuts (Ctrl+C/V/X), text selection (Shift+Click), etc.

pub static SHIFT_HELD: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);
pub static CTRL_HELD: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);
pub static ALT_HELD: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

/// True when pointer motion ALONE must trigger a full-scene repaint.
///
/// Plain hover motion (no buttons, no menu/dialog, no drag in progress) only needs the
/// hardware cursor moved -- see the cheap cursor path in `gui_refresh_task`. Everything that
/// genuinely redraws on motion is listed here; when in doubt (a lock is contended) this
/// answers `true`, i.e. falls back to the old always-repaint behaviour.
fn motion_needs_repaint() -> bool {
    use crate::desktop::{CONTEXT_MENU, DESKTOP_WM, PENDING_ROLE_DIALOG};
    // Any button held: window drag/resize, file drag-and-drop and in-app drags
    // (text selection, scrollbars) all redraw on every move.
    {
        let s = crate::task::mouse::MOUSE_STATE.lock();
        if s.left_button || s.right_button { return true; }
    }
    // The context menu highlights the hovered row.
    if CONTEXT_MENU.try_lock().map(|m| m.visible).unwrap_or(true) { return true; }
    // The role-confirmation dialog repaints (countdown / hit areas).
    if PENDING_ROLE_DIALOG.try_lock().map(|d| d.visible).unwrap_or(true) { return true; }
    // Defensive: a window drag/resize still active (e.g. a missed button-release).
    DESKTOP_WM
        .try_lock()
        .map(|wm| wm.dragging_id.is_some() || wm.resizing_id.is_some())
        .unwrap_or(true)
}

async fn gui_refresh_task() {
    // ---- Boot diagnostics — visible immediately on the serial console ----
    // These fire before the first frame is rendered, making it trivial to
    // distinguish "task never started" from "task started but can't render".
    serial_println!("[GUI] gui_refresh_task started.");
    serial_println!("[GUI] TICKS      = {}",
        crate::interrupts::TICKS.load(core::sync::atomic::Ordering::Relaxed));
    serial_println!("[GUI] FRAMEBUFFER present = {}",
        crate::graphics::FRAMEBUFFER.lock().is_some());
    serial_println!("[GUI] BACK_BUFFER present = {}",
        crate::ui::BACK_BUFFER.lock().is_some());
    // ---- end diagnostics ----

    let mut last_mb = false;
    let mut last_mx = 512;
    let mut last_my = 384;
    let mut last_blink = false;
    let mut last_sec = 0;
    let mut last_mesh_debug_sec: u64 = 0;

    // Frame-time diagnostics, printed to serial every 5 s of kernel time.
    let mut diag_sec: u64 = 0;
    let mut diag_tsc0: u64 = unsafe { core::arch::x86_64::_rdtsc() };
    let mut diag_ticks0: u64 = 0;
    let mut diag_iters: u64 = 0;
    let mut diag_full: u64 = 0;
    let mut diag_cursor: u64 = 0;
    let mut diag_full_cycles: u64 = 0;
    // Per-stage split of full frames: (clear, paint, flush) cycles.
    let mut diag_stage: [u64; 3] = [0; 3];

    loop {
        let ms = crate::interrupts::TICKS.load(core::sync::atomic::Ordering::Relaxed);
        // The power button: shut down, saving what power-off saves.
        if crate::power::take_button_press() {
            crate::serial_println!("[POWER] power button pressed");
            crate::shell::power_off();
        }
        diag_iters += 1;
        let mut ui_dirty = false;
        // Separate flag: only the panel clock needs repainting, not the whole scene.
        let mut clock_dirty = false;

        let blink = (ms / 500) % 2 == 0;
        if blink != last_blink {
            last_blink = blink;
            ui_dirty = true;
        }
        let sec = ms / 1_000;
        if sec != last_sec {
            last_sec = sec;
            // A new second only dirties the clock strip, not every window.
            clock_dirty = true;
        }

        // Drain profiler ISR ticks from safe task context (no locks in ISR).
        crate::debugger::pump_profiler_ticks();


        // Drain all pending scancodes and route them through the desktop
        // keyboard dispatcher.  Handles login screen, lock screen, desktop
        // app forwarding, and the Super+L global lock shortcut.
        {
            // Initialise the decoder on first frame.
            {
                let mut kb_guard = KB.lock();
                if kb_guard.is_none() {
                    *kb_guard = Some(Keyboard::new(
                        ScancodeSet1::new(),
                        layouts::Us104Key,
                        HandleControl::Ignore,
                    ));
                }
            }
            loop {
                let sc = match crate::task::keyboard::pop_scancode() {
                    Some(s) => s,
                    None    => break,
                };
                // Detect extended prefix (0xE0) for Super/arrow keys. The
                // decoder needs it too, or End, the arrows and the other
                // grey keys decode as their numeric-keypad twins.
                if sc == 0xE0 {
                    EXTENDED_PREFIX.store(true, core::sync::atomic::Ordering::Relaxed);
                    if let Some(kb) = KB.lock().as_mut() {
                        let _ = kb.add_byte(sc);
                    }
                    continue;
                }
                // Pause is the one key with an E1 sequence, E1 1D 45 E1 9D C5,
                // sent all at once on press (nothing on release). pc-keyboard
                // reads it as Right Ctrl and Num Lock, so it never reached a
                // program: take the six bytes here as Pause down (at 45) and up
                // (at C5), and keep them from the decoder and the Ctrl state.
                let mut pause_event = None;
                {
                    use core::sync::atomic::Ordering::Relaxed;
                    let at = PAUSE_SEQ.load(Relaxed);
                    if at == 0 && sc == 0xE1 {
                        PAUSE_SEQ.store(1, Relaxed);
                        continue;
                    }
                    if at != 0 {
                        PAUSE_SEQ.store(if at >= 5 { 0 } else { at + 1 }, Relaxed);
                        let state = match (at, sc) {
                            (2, 0x45) => pc_keyboard::KeyState::Down,
                            (5, 0xC5) => pc_keyboard::KeyState::Up,
                            _ => continue,
                        };
                        pause_event = Some(pc_keyboard::KeyEvent::new(pc_keyboard::KeyCode::PauseBreak, state));
                    }
                }
                let extended = EXTENDED_PREFIX.swap(false, core::sync::atomic::Ordering::Relaxed);

                // -----| Track modifier key state from raw scancodes |-----
                if pause_event.is_none() { match sc {
                    0x2A | 0x36 => { SHIFT_HELD.store(true, core::sync::atomic::Ordering::Relaxed); } // LShift/RShift down
                    0xAA | 0xB6 => { SHIFT_HELD.store(false, core::sync::atomic::Ordering::Relaxed); } // LShift/RShift up
                    0x1D        => { CTRL_HELD.store(true, core::sync::atomic::Ordering::Relaxed); }  // LCtrl down (also RCtrl via 0xE0 prefix)
                    0x9D        => { CTRL_HELD.store(false, core::sync::atomic::Ordering::Relaxed); } // LCtrl/RCtrl up
                    0x38        => { ALT_HELD.store(true, core::sync::atomic::Ordering::Relaxed); }   // LAlt down
                    0xB8        => { ALT_HELD.store(false, core::sync::atomic::Ordering::Relaxed); }  // LAlt up
                    _ => {}
                } }

                // Super+L (0xE0 0x5B) — lock the desktop.
                if extended && sc == 0x5B {
                    crate::desktop::lock_desktop();
                    continue;
                }
                // Decode and dispatch.
                let mut maybe_event = pause_event.take();
                if maybe_event.is_none() {
                    let mut kb_guard = KB.lock();
                    if let Some(kb) = kb_guard.as_mut() {
                        maybe_event = kb.add_byte(sc).ok().flatten();
                    }
                }

                if let Some(ev) = maybe_event {
                    // A Windows program's window gets the key itself too.
                    let program_key = crate::nt_user::key_event(ev.code, ev.state);
                    let key_name = match ev.code {
                        pc_keyboard::KeyCode::Escape => "ESC",
                        pc_keyboard::KeyCode::Return => "ENTER",
                        pc_keyboard::KeyCode::Spacebar => "SPACE",
                        _ => "OTHER",
                    };
                    let action_name = match ev.state {
                        pc_keyboard::KeyState::Down => "HOLD",
                        pc_keyboard::KeyState::Up => "RELEASE",
                        pc_keyboard::KeyState::SingleShot => "PRESS",
                    };

                    let mut filters = alloc::collections::BTreeMap::new();
                    filters.insert(alloc::string::String::from("key"), alloc::string::String::from(key_name));
                    filters.insert(alloc::string::String::from("action"), alloc::string::String::from(action_name));
                    
                    {
                        let mut eng = crate::vscript::GLOBAL_ENGINE.lock();
                        let _ = eng.trigger_event("key_event", Some(&filters));
                    }

                    let maybe_key = {
                        let mut kb_guard = KB.lock();
                        kb_guard.as_mut().unwrap().process_keyevent(ev.clone())
                    };

                    // A terminal running a console program takes keys whole:
                    // key events with VK codes and modifiers, down and up.
                    if !program_key && crate::desktop::DesktopPhase::from_u8(crate::desktop::DESKTOP_PHASE.load(core::sync::atomic::Ordering::Acquire)) == crate::desktop::DesktopPhase::Desktop {
                        let decoded = match maybe_key { Some(DecodedKey::Unicode(c)) => Some(c), _ => None };
                        if crate::apps::console_key(ev.code, ev.state, decoded) {
                            crate::users::SESSION.lock().touch();
                            ui_dirty = true;
                            continue;
                        }
                    }

                    if let Some(key) = maybe_key {
                        ui_dirty = true;
                        match key {
                            DecodedKey::Unicode(ch) => {
                                crate::users::SESSION.lock().touch();
                                // Intercept Ctrl+letter for clipboard / selection shortcuts.
                                if CTRL_HELD.load(core::sync::atomic::Ordering::Relaxed) && !program_key {
                                    let lower = ch.to_ascii_lowercase();
                                    match lower {
                                        'c' => { crate::clipboard::clipboard_copy(); continue; }
                                        'v' => { crate::clipboard::clipboard_paste(); continue; }
                                        'x' => { crate::clipboard::clipboard_cut(); continue; }
                                        'a' => { crate::clipboard::clipboard_select_all(); continue; }
                                        _ => {} // Fall through to normal dispatch
                                    }
                                }
                                crate::desktop::dispatch_key(ch);
                            }
                            // pc-keyboard 0.7 decodes the US backslash key
                            // (scancode 0x2B) as Oem7, which its Us104 layout
                            // doesn't map: it is '\' (or '|' with Shift).
                            DecodedKey::RawKey(pc_keyboard::KeyCode::Oem7) => {
                                crate::users::SESSION.lock().touch();
                                let shift = SHIFT_HELD.load(core::sync::atomic::Ordering::Relaxed);
                                crate::desktop::dispatch_key(if shift { '|' } else { '\\' });
                            }
                            DecodedKey::RawKey(key) => {
                                crate::users::SESSION.lock().touch();
                                crate::desktop::dispatch_raw_key(key);
                            }
                        }
                    }
                }
            }
        }

        // Tick always-on subsystems.
        // Drive the smoltcp network stack — processes incoming Ethernet
        // frames, runs DHCP/DNS, and delivers UDP/TCP data to sockets.
        crate::network::poll();
        // Performance Mode's lazy writer (block.rs).
        crate::block::tick(crate::time::now_ms());
        crate::scheduler::flush_if_due(crate::time::now_ms());
        crate::security::flush_audit(crate::time::now_ms());
        
        // Drive the distributed mesh layer — sends LAN announcements,
        // polls the discovery UDP socket, and processes incoming packets.
        let tick_ms = ms / 1000;
        crate::distributed::tick();
        // Periodic debug: log peer count every few seconds.
        // (Previously compared against a copy of `last_sec` taken *after*
        // `last_sec` had already been synced to the current second above,
        // so the inequality below was always false and this never fired.
        // Uses its own tracker now.)
        if tick_ms % 5 == 0 && tick_ms != last_mesh_debug_sec {
            last_mesh_debug_sec = tick_ms;
            let peer_count = crate::distributed::NODE_MESH.lock().peer_count();
            crate::serial_println!("[MESH] Active peers: {}", peer_count);
        }

        // Tick all live terminal pool slots (cursor blink, etc.).
        for slot in &crate::apps::TERMINAL_POOL {
            if let Some(mut term) = slot.try_lock() {
                if term.window_id != 0 { term.tick(ms); }
            }
        }
        // crate::compositor::COMPOSITOR.lock().tick(ms); // Disabled: competing with new Desktop WM and causing lag.

        let phase = crate::desktop::DesktopPhase::from_u8(
            crate::desktop::DESKTOP_PHASE.load(core::sync::atomic::Ordering::Acquire)
        );

        // Read mouse state once — used in both Desktop and login/lock branches.
        let (mx, my, mb) = {
            let state = crate::task::mouse::MOUSE_STATE.lock();
            (state.x as usize, state.y as usize, state.left_button)
        };

        if mb != last_mb { ui_dirty = true; }

        // ----------------------------------------------------------------
        // LOGIN / LOCK phases — full-screen takeover.
        // Skip the WM entirely, paint directly to hardware, no cursor.
        // Still handle pending role dialog clicks though.
        // ----------------------------------------------------------------
        if phase != crate::desktop::DesktopPhase::Desktop {
            // Handle dialog clicks during login/lock phases
            use crate::desktop::PENDING_ROLE_DIALOG;
            if mb && PENDING_ROLE_DIALOG.try_lock().map(|d| d.visible).unwrap_or(false) {
                let mut d = PENDING_ROLE_DIALOG.lock();
                if d.handle_click(mx as i32, my as i32) {
                    ui_dirty = true; // Repaint to remove dialog
                }
            }
            if ui_dirty {
                crate::desktop::paint_desktop(ms);
            }
            last_mb = mb;
            last_mx = mx;
            last_my = my;
            crate::interrupts::SleepFuture::new(16).await;
            continue;
        }

        // ----------------------------------------------------------------
        // DESKTOP phase — WM input handling + single composite per frame.
        // ----------------------------------------------------------------

        // One-shot: load /config.vsh the first time we reach the Desktop phase.
        // This is deferred from apps::init() because the VFS has no mounts
        // until after AHCI init completes — which happens well after init().
        {
            static CONFIG_LOADED: core::sync::atomic::AtomicBool =
                core::sync::atomic::AtomicBool::new(false);
            if !CONFIG_LOADED.swap(true, core::sync::atomic::Ordering::AcqRel) {
                crate::apps::load_config_vsh();
            }
        }

        // Poll centrally — this handles window buttons, drags, and content clicks.
        // It works both here and in the timer ISR while a process is running.
        crate::desktop::poll_input();
        crate::nt_registry::tick(ms);

        // Drain any desktop icon / app launcher clicks that the timer ISR
        // deferred (they involve heavy heap allocations — PNG decode, PE loading —
        // and must not run inside the ISR where the allocator may already be locked).
        crate::desktop::drain_deferred_clicks();

        // VSH shell: handlers, window events, live values, layout, OS actions.
        if crate::shell::pump(ms) {
            ui_dirty = true;
        }

        // Repaint if anything changed. Plain pointer motion only moves the hardware cursor:
        // it must NOT force a full clear + paint_desktop + flush, otherwise the cursor can only
        // update as fast as a whole frame renders. (This used to set `ui_dirty` on any motion,
        // which also made the lightweight cursor branch below unreachable.)
        let cursor_moved = mx != last_mx || my != last_my;
        if mb != last_mb || (cursor_moved && motion_needs_repaint()) {
            ui_dirty = true;
        }

        // Advance animations for focused windows or wallpaper
        if !ui_dirty && (crate::apps::needs_animation_refresh(ms) || crate::desktop::needs_wallpaper_refresh(ms)) {
            ui_dirty = true;
        }

        let frame_t0 = unsafe { core::arch::x86_64::_rdtsc() };

        if ui_dirty {
            // Acquire the back-buffer lock ONCE for the entire frame.
            // While _painter is live, every call to draw_rect_atomic,
            // draw_string_atomic, and commit_back_buffer — including those
            // made deep inside paint_desktop — bypasses the per-call Mutex
            // and writes directly through the guard's raw pointer.
            // This collapses ~30 lock/unlock round-trips per frame down to 1.
            let _painter = crate::ui::BackBufferGuard::acquire();

            let rdtsc = || unsafe { core::arch::x86_64::_rdtsc() };
            let t_a = rdtsc();
            // (a) Clear the back buffer (through the logical GPU when the painter
            //     fast path is up, which the guard above guarantees).
            match crate::ui::painter_surface() {
                Some(target) => {
                    let op = crate::lgpu::Op::Fill {
                        rect: crate::lgpu::Rect::new(0, 0, crate::ui::screen_w(), crate::ui::screen_h()),
                        color: 0x14_14_23, // r 20, g 20, b 35
                    };
                    if !crate::lgpu::dl_record(op) {
                        crate::lgpu::execute(&[op], &target);
                    }
                }
                None => crate::ui::draw_rect_atomic(0, 0, crate::ui::screen_w(), crate::ui::screen_h(), crate::graphics::Color { r: 20, g: 20, b: 35 }),
            }
            let t_b = rdtsc();

            // (b) Phase 18 DE (panel, app windows, notifs) composites on top.
            crate::desktop::paint_desktop(ms);
            let t_c = rdtsc();

            // (c) One full flush to hardware (also uses the fast path).
            crate::ui::commit_back_buffer();
            let t_d = rdtsc();
            diag_stage[0] += t_b - t_a;
            diag_stage[1] += t_c - t_b;
            diag_stage[2] += t_d - t_c;

            // (d) Release the guard — restores normal per-call locking.
            drop(_painter);

            // (e) Cursor drawn directly on hardware — never in back buffer.
            {
                let mut wm = crate::ui::WINDOW_MANAGER.lock();
                wm.cursor_x = mx;
                wm.cursor_y = my;
                wm.draw_cursor_hw();
            }
        } else {
            // Cheap per-second clock update: only the panel strip is repainted and flushed.
            if clock_dirty {
                crate::desktop::repaint_panel_only(ms);
            }
            // Lightweight cursor movement: restore the pixels under the old cursor from the
            // back buffer, then stamp the new cursor on the hardware framebuffer. Runs AFTER
            // the panel repaint (which re-stamps the cursor at its previously stored position)
            // and independently of it, so motion during a clock-update frame is never dropped
            // (that would leave a ghost cursor behind).
            if cursor_moved {
                crate::ui::commit_dirty_rect(last_mx, last_my, 16, 16);
                let mut wm = crate::ui::WINDOW_MANAGER.lock();
                wm.cursor_x = mx;
                wm.cursor_y = my;
                wm.draw_cursor_hw();
            }
        }

        // Process GPU command batches queued since the last iteration.
        crate::gpu_driver::tick();

        // Cards inserted into or removed from PCIe hot-plug slots.
        crate::hotplug::poll();

        // An app the user closed that kept running without windows.
        crate::win32_user::check_close_watch();

        // ---- frame-time diagnostics ----
        if ui_dirty {
            diag_full += 1;
            diag_full_cycles += unsafe { core::arch::x86_64::_rdtsc() }.wrapping_sub(frame_t0);
        } else if cursor_moved {
            diag_cursor += 1;
        }
        if sec >= diag_sec + 5 {
            let now_tsc = unsafe { core::arch::x86_64::_rdtsc() };
            let avg_mc = if diag_full > 0 { diag_full_cycles / diag_full / 1_000_000 } else { 0 };
            let per = |c: u64| if diag_full > 0 { c / diag_full / 100_000 } else { 0 }; // tenths of Mcycles
            let (dl_flushes, dl_ops, dl_cycles) = crate::lgpu::take_stats();
            let shell_cycles = crate::shell::take_paint_cycles();
            serial_println!(
                "[GUI] perf: {} loop iters, {} full frames (avg {} Mcycles each: clear {}.{} paint {}.{} flush {}.{}; display list {} flushes, {} ops, {}.{} rendering; shell {}.{}), {} cursor-only; {} Mcycles elapsed over {} ticks",
                diag_iters, diag_full, avg_mc,
                per(diag_stage[0]) / 10, per(diag_stage[0]) % 10,
                per(diag_stage[1]) / 10, per(diag_stage[1]) % 10,
                per(diag_stage[2]) / 10, per(diag_stage[2]) % 10,
                dl_flushes / diag_full.max(1), dl_ops / diag_full.max(1),
                per(dl_cycles) / 10, per(dl_cycles) % 10,
                per(shell_cycles) / 10, per(shell_cycles) % 10,
                diag_cursor,
                now_tsc.wrapping_sub(diag_tsc0) / 1_000_000,
                ms.wrapping_sub(diag_ticks0));
            diag_stage = [0; 3];
            diag_sec = sec;
            diag_tsc0 = now_tsc;
            diag_ticks0 = ms;
            diag_iters = 0;
            diag_full = 0;
            diag_cursor = 0;
            diag_full_cycles = 0;
        }

        last_mb = mb;
        last_mx = mx;
        last_my = my;

        crate::interrupts::SleepFuture::new(16).await;
    }
}

// example_task removed

// -----| panic handler |-----

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    // Log reliably to the serial port
    serial_println!("\n!!! KERNEL PANIC !!!\n{}", info);
    // Stop the other CPUs, show the crash screen, save the report (crash.rs).
    crate::crash::on_panic(info);

    // Tell the user on the screen! (best-effort: if VGA_WRITER is itself the
    // thing that was locked when we panicked, this could still hang here —
    // but the serial_println! above already got the diagnostic out.)
    if let Some(mut w) = vga_buffer::VGA_WRITER.try_lock() {
        w.set_colour(
            vga_buffer::VgaColour::White, 
            vga_buffer::VgaColour::Red
        );
    }
    println!("\n[KERNEL PANIC]\n{}", info);
    println!("Press any key to restart.");

    // The crash screen says any key restarts: poll the keyboard with
    // interrupts off (power.rs), then reset without saving anything.
    crate::power::wait_key_then_reset()
}
