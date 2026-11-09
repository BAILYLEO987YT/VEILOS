use crate::println;
use crate::pci;
use spin::Mutex;
use x86_64::{VirtAddr, structures::paging::{Mapper, Page, PageTableFlags, Size4KiB}};

// -----| framebuffer abstractions |-----

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

#[derive(Clone, Copy)]
pub struct Framebuffer {
    pub base_addr: u64,
    pub width: usize,
    pub height: usize,
    pub stride: usize,
}

impl Framebuffer {
    /// Robustly plots a single pixel.
    pub fn plot_pixel(&self, x: usize, y: usize, color: Color) {
        if x >= self.width || y >= self.height {
            return;
        }

        let pixel_offset = y * self.stride + x;
        let p_ptr = (self.base_addr + (pixel_offset as u64 * 4)) as *mut u32;

        // Default to ARGB format for manual probing (Typical for QEMU stdvga)
        let pixel_val = (color.b as u32) | ((color.g as u32) << 8) | ((color.r as u32) << 16);

        unsafe { *p_ptr = pixel_val; }
    }

    pub fn clear(&self, color: Color) {
        // Pack the pixel value once, then fill entire rows at a time.
        let pixel_val = (color.b as u32) | ((color.g as u32) << 8) | ((color.r as u32) << 16);
        for y in 0..self.height {
            let row_ptr = (self.base_addr + (y * self.stride) as u64 * 4) as *mut u32;
            unsafe {
                core::slice::from_raw_parts_mut(row_ptr, self.width).fill(pixel_val);
            }
        }
    }
}

/// The first display head's framebuffer: boot console text and the GPU
/// command model's scanout. The desktop draws through `HEADS`.
pub static FRAMEBUFFER: Mutex<Option<Framebuffer>> = Mutex::new(None);

// -----| logical display (HAL step 7D) |-----
//
// Every display adapter bound is one head. The desktop sees one logical
// display: the heads side by side, left to right in the order they bound,
// all in the same mode (so their rows line up). `ui::commit_dirty_rect` and
// the hardware cursor split their work over the heads; everything else draws
// into the one back buffer and never knows how many screens there are.

/// One display head: an adapter's framebuffer and where it sits on the desktop.
pub struct Head {
    pub fb: Framebuffer,
    /// Left edge on the logical display.
    pub x: usize,
    /// The adapter's whole VRAM aperture (identity-mapped): the framebuffer is
    /// its start, `gpu_driver` keeps surfaces after it.
    pub vram_base: u64,
    pub vram_size: usize,
    pub location: alloc::string::String,
}

pub static HEADS: Mutex<alloc::vec::Vec<Head>> = Mutex::new(alloc::vec::Vec::new());

/// Size of the VRAM aperture starting at `base`, if a head maps it.
pub fn vram_mapping(base: u64) -> Option<usize> {
    HEADS.lock().iter().find(|h| h.vram_base == base).map(|h| h.vram_size)
}

/// Bytes at the start of the aperture at `base` that are scanout.
pub fn scanout_bytes(base: u64) -> usize {
    HEADS.lock().iter().find(|h| h.vram_base == base).map_or(0, |h| h.fb.stride * h.fb.height * 4)
}

/// One line per head, for the terminal's DISPLAYS command.
pub fn describe_heads() -> alloc::vec::Vec<alloc::string::String> {
    let heads = HEADS.lock();
    let mut out: alloc::vec::Vec<_> = heads.iter().enumerate().map(|(i, h)| alloc::format!(
        "  head {}: {} {}x{} at x={} (VRAM {} MiB)",
        i, h.location, h.fb.width, h.fb.height, h.x, h.vram_size >> 20
    )).collect();
    out.push(alloc::format!("  logical display: {}x{}", crate::ui::screen_w(), crate::ui::screen_h()));
    out
}

// -----| Bochs DISPI registers |-----

use x86_64::instructions::port::Port;

/// VBE_DISPI register indices.
const DISPI_XRES: u16 = 1;
const DISPI_YRES: u16 = 2;
const DISPI_BPP: u16 = 3;
const DISPI_ENABLE: u16 = 4;
const DISPI_VIRT_WIDTH: u16 = 6;
const DISPI_ENABLED: u16 = 0x01;
const DISPI_LFB_ENABLED: u16 = 0x40;

/// Where an adapter's DISPI registers are. The I/O ports (0x1CE/0x1CF) only
/// reach the legacy VGA device, of which there is at most one; the MMIO block
/// in BAR2 (DISPI at +0x500, one u16 per index) reaches each adapter, and is
/// the only way into `bochs-display`, which has no I/O ports at all.
#[derive(Clone, Copy)]
enum Dispi {
    Ports,
    Mmio(u64),
}

impl Dispi {
    const MMIO_OFFSET: u64 = 0x500;

    fn for_device(dev: &pci::PciDevice) -> Result<Self, &'static str> {
        let bar2 = dev.bar(2);
        let Some(base) = bar2.mem_base().filter(|_| bar2.size >= 0x600) else {
            return Ok(Dispi::Ports);
        };
        // Registers: uncached, unlike the framebuffer.
        let mut mapper = unsafe { crate::memory::get_mapper() };
        let page = Page::<Size4KiB>::containing_address(VirtAddr::new(base));
        let frame = x86_64::structures::paging::PhysFrame::containing_address(x86_64::PhysAddr::new(base));
        let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::NO_CACHE;
        match unsafe { mapper.map_to(page, frame, flags, &mut crate::memory::GlobalFrameAllocator) } {
            Ok(f) => f.flush(),
            Err(x86_64::structures::paging::mapper::MapToError::PageAlreadyMapped(_)) => {}
            Err(_) => return Err("could not map the register BAR"),
        }
        Ok(Dispi::Mmio(base))
    }

    fn write(self, index: u16, data: u16) {
        match self {
            Dispi::Ports => unsafe {
                Port::<u16>::new(0x1CE).write(index);
                Port::<u16>::new(0x1CF).write(data);
            },
            Dispi::Mmio(base) => unsafe {
                core::ptr::write_volatile((base + Self::MMIO_OFFSET + index as u64 * 2) as *mut u16, data);
            },
        }
    }

    fn read(self, index: u16) -> u16 {
        match self {
            Dispi::Ports => unsafe {
                Port::<u16>::new(0x1CE).write(index);
                Port::<u16>::new(0x1CF).read()
            },
            Dispi::Mmio(base) => unsafe { core::ptr::read_volatile((base + Self::MMIO_OFFSET + index as u64 * 2) as *const u16) },
        }
    }
}

/// Mode used when no valid one is requested.
const DEFAULT_MODE: (usize, usize) = (1024, 768);

/// Where the boot resolution can be chosen: one line, `WIDTHxHEIGHT`.
const DISPLAY_CONFIG_PATH: &str = "/etc/display.cfg";

/// The mode requested in `/etc/display.cfg`, if it parses, is in a sane range
/// and fits in `vram_bytes` at 32 bpp. Read when the display binds, which is
/// after the storage drivers have mounted the root volume.
fn requested_mode(vram_bytes: usize) -> Option<(usize, usize)> {
    let raw = crate::vfs::VFS.read_file(DISPLAY_CONFIG_PATH).ok()?;
    let text = core::str::from_utf8(&raw).ok()?.trim();
    let (w, h) = text.split_once(|c| c == 'x' || c == 'X')?;
    let (w, h): (usize, usize) = (w.trim().parse().ok()?, h.trim().parse().ok()?);
    let fits = (320..=4096).contains(&w) && (200..=2160).contains(&h) && w % 8 == 0 && w * h * 4 <= vram_bytes;
    if !fits {
        crate::serial_println!("[GRAPHICS] {} asks for {}x{}: out of range or too big for {} MiB VRAM; using default",
            DISPLAY_CONFIG_PATH, w, h, vram_bytes >> 20);
        return None;
    }
    Some((w, h))
}

/// Bochs/QEMU display adapters (stdvga, bochs-display): a linear framebuffer
/// in BAR0, mode set through the DISPI registers. Bound by `hal::bind`; every
/// adapter becomes a head of the logical display.
///
/// Matching on the Bochs vendor ID matters: on Intel/AMD/NVIDIA cards BAR0 is
/// the register block, and the old "first display device's BAR0" logic would
/// have written pixels into device registers.
pub const LFB_DISPLAY_DRIVER: crate::hal::Driver = crate::hal::Driver {
    name: "bochs-lfb",
    class: crate::hal::DeviceClass::Display,
    matches: is_bochs_display,
    probe: probe_display,
    // The logical display's size is fixed once the back buffer exists.
    remove: None,
};

fn is_bochs_display(dev: &pci::PciDevice) -> bool {
    dev.vendor_id == 0x1234 && dev.bar(0).mem_base().is_some_and(|b| b != 0)
}

fn probe_display(dev: &pci::PciDevice) -> Result<(), alloc::string::String> {
    // The back buffer is sized from the logical display once, at boot.
    if crate::ui::BACK_BUFFER.lock().is_some() {
        return Err(alloc::string::String::from("display added after boot: not driven"));
    }
    let location = alloc::format!("{:02x}:{:02x}.{}", dev.bus, dev.device, dev.function);
    println!("[GRAPHICS] Probing Bochs/QEMU display adapter at {}...", location);

    // The display controller's linear framebuffer (BAR0 on BGA).
    let bar0 = dev.bar(0);
    let phys_addr = bar0.mem_base().unwrap_or(0);
    dev.enable_decode();
    let regs = Dispi::for_device(dev).map_err(alloc::string::String::from)?;
    if matches!(regs, Dispi::Ports) && !HEADS.lock().is_empty() {
        // The ports belong to the legacy VGA device, already driven.
        return Err(alloc::string::String::from("no MMIO registers and the I/O ports are taken"));
    }

    // Mode: /etc/display.cfg ("WIDTHxHEIGHT") if present and it fits the
    // adapter's memory, else 1024x768, 32 bpp. Later heads take the first
    // head's mode so the logical display's rows line up.
    let first = HEADS.lock().first().map(|h| (h.fb.width, h.fb.height));
    let (want_w, want_h) = first.unwrap_or_else(|| requested_mode(bar0.size as usize).unwrap_or(DEFAULT_MODE));
    regs.write(DISPI_ENABLE, 0);
    regs.write(DISPI_XRES, want_w as u16);
    regs.write(DISPI_YRES, want_h as u16);
    regs.write(DISPI_BPP, 32);
    regs.write(DISPI_ENABLE, DISPI_ENABLED | DISPI_LFB_ENABLED);

    // Use what the adapter actually set, and its line pitch (virtual width),
    // rather than assuming the request was honoured and stride == width.
    let width = regs.read(DISPI_XRES) as usize;
    let height = regs.read(DISPI_YRES) as usize;
    let stride = (regs.read(DISPI_VIRT_WIDTH) as usize).max(width);
    if width == 0 || height == 0 || first.is_some_and(|f| f != (width, height)) {
        return Err(alloc::format!("adapter rejected mode {}x{} (set {}x{})", want_w, want_h, width, height));
    }
    println!("[GRAPHICS] Hardware Resolution Set: {}x{}x32 (stride {})", width, height, stride);
    crate::serial_println!("[GRAPHICS] {} mode {}x{} stride {} (requested {}x{}, registers {})", location, width, height, stride,
        want_w, want_h, if matches!(regs, Dispi::Mmio(_)) { "MMIO" } else { "I/O ports" });

    // Identity-map the whole aperture (write-combining, see map_direct_hw):
    // the framebuffer at its start, GPU surfaces (gpu_driver) after it.
    let mut mapper = unsafe { crate::memory::get_mapper() };
    let mut frame_allocator = crate::memory::GlobalFrameAllocator;

    let fb_size = stride * height * 4;
    let vram_size = (bar0.size as usize).max(fb_size);
    if let Err(e) = map_direct_hw(phys_addr as u64, vram_size as u64, phys_addr as u64, &mut mapper, &mut frame_allocator) {
        println!("[GRAPHICS] ERROR: Failed to map framebuffer: {}", e);
        return Err(alloc::format!("framebuffer mapping failed: {}", e));
    }

    let fb = Framebuffer { base_addr: phys_addr as u64, width, height, stride };
    fb.clear(Color { r: 20, g: 20, b: 35 });
    let mut heads = HEADS.lock();
    let x = heads.iter().map(|h| h.fb.width).sum();
    if heads.is_empty() {
        *FRAMEBUFFER.lock() = Some(fb);
    }
    heads.push(Head { fb, x, vram_base: phys_addr as u64, vram_size, location });

    // Everything sized from the screen (back buffer, clip, layout, mouse)
    // follows the logical display from here on.
    crate::ui::set_screen_size(x + width, height);
    println!("[GRAPHICS] Manually mapped framebuffer at {:#X}", phys_addr);
    Ok(())
}

// -----| hardware security (DirectHW) |-----

pub fn map_direct_hw(
    phys_addr: u64,
    size: u64,
    virt_addr: u64,
    mapper: &mut impl Mapper<Size4KiB>,
    frame_allocator: &mut impl x86_64::structures::paging::FrameAllocator<Size4KiB>,
) -> Result<(), &'static str> {
    if phys_addr < 0x100000 {
        return Err("Security Violation: Attempted to bridge protected device memory!");
    }

    let num_pages = (size + 4095) / 4096;
    for i in 0..num_pages {
        let page = Page::containing_address(VirtAddr::new(virt_addr + i * 4096));
        let frame = x86_64::structures::paging::PhysFrame::containing_address(
            x86_64::PhysAddr::new(phys_addr + i * 4096),
        );

        // PWT alone selects PAT entry 1 = write-combining (smp::init_pat).
        // This was UC (PCD|PWT): every pixel store was its own bus write.
        let flags = PageTableFlags::PRESENT
            | PageTableFlags::WRITABLE
            | PageTableFlags::WRITE_THROUGH;

        unsafe {
            // `.ignore()` skips the per-page `invlpg` — we do one full TLB
            // flush below after all pages are mapped, dropping latency from
            // 768 serialising stalls to a single `mov cr3` round-trip.
            mapper
                .map_to(page, frame, flags, frame_allocator)
                .map_err(|_| "Failed to map HW page")?
                .ignore();
        }
    }

    // One full TLB flush covers all the pages we just mapped.
    x86_64::instructions::tlb::flush_all();

    println!("[DirectHW] Successfully mapped hardware at {:#X} to user space.", phys_addr);
    Ok(())
}
