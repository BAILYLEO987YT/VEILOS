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

pub static FRAMEBUFFER: Mutex<Option<Framebuffer>> = Mutex::new(None);

/// The display adapter's whole VRAM aperture (physical base = kernel address,
/// identity-mapped, and size). The visible framebuffer is its start; the rest
/// is where `gpu_driver` keeps surfaces.
static VRAM: Mutex<Option<(u64, usize)>> = Mutex::new(None);

pub fn vram_mapping() -> Option<(u64, usize)> {
    *VRAM.lock()
}

// -----| manual gpu probe |-----

use x86_64::instructions::port::Port;

fn bga_write(index: u16, data: u16) {
    let mut index_port = Port::<u16>::new(0x1CE);
    let mut data_port = Port::<u16>::new(0x1CF);
    unsafe {
        index_port.write(index);
        data_port.write(data);
    }
}

fn bga_read(index: u16) -> u16 {
    let mut index_port = Port::<u16>::new(0x1CE);
    let mut data_port = Port::<u16>::new(0x1CF);
    unsafe {
        index_port.write(index);
        data_port.read()
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
/// in BAR0, mode set through the BGA ports. Bound by `hal::bind`.
///
/// Matching on the Bochs vendor ID matters: on Intel/AMD/NVIDIA cards BAR0 is
/// the register block, and the old "first display device's BAR0" logic would
/// have written pixels into device registers.
pub const LFB_DISPLAY_DRIVER: crate::hal::Driver = crate::hal::Driver {
    name: "bochs-lfb",
    class: crate::hal::DeviceClass::Display,
    matches: is_bochs_display,
    probe: probe_display,
};

fn is_bochs_display(dev: &pci::PciDevice) -> bool {
    dev.vendor_id == 0x1234 && dev.bar(0).mem_base().is_some_and(|b| b != 0)
}

fn probe_display(dev: &pci::PciDevice) -> Result<(), alloc::string::String> {
    // The desktop drives one scanout for now; more displays are HAL step 7D.
    if FRAMEBUFFER.lock().is_some() {
        return Err(alloc::string::String::from("second display not driven yet"));
    }
    println!("[GRAPHICS] Probing for Bochs/QEMU GPU (BGA)...");

    // The display controller's linear framebuffer (BAR0 on BGA).
    let bar0 = dev.bar(0);
    let phys_addr = bar0.mem_base().unwrap_or(0);
    dev.enable_decode();
    println!("[GRAPHICS] Found GPU at PCI {:02X}:{:02X}. BAR0: {:#X}", dev.bus, dev.device, phys_addr);

    // Mode: /etc/display.cfg ("WIDTHxHEIGHT") if present and it fits the
    // adapter's memory, else 1024x768. 32 bpp.
    let (want_w, want_h) = requested_mode(bar0.size as usize).unwrap_or(DEFAULT_MODE);
    bga_write(4, 0x00); // VBE_DISPI_INDEX_ENABLE -> Disable
    bga_write(1, want_w as u16); // VBE_DISPI_INDEX_XRES
    bga_write(2, want_h as u16); // VBE_DISPI_INDEX_YRES
    bga_write(3, 32);   // VBE_DISPI_INDEX_BPP
    bga_write(4, 0x01 | 0x40); // VBE_DISPI_INDEX_ENABLE -> (Enabled | LFB)

    // Use what the adapter actually set, and its line pitch (virtual width),
    // rather than assuming the request was honoured and stride == width.
    let width = bga_read(1) as usize;
    let height = bga_read(2) as usize;
    let stride = (bga_read(6) as usize).max(width);
    if width == 0 || height == 0 {
        return Err(alloc::format!("adapter rejected mode {}x{}", want_w, want_h));
    }
    println!("[GRAPHICS] Hardware Resolution Set: {}x{}x32 (stride {})", width, height, stride);
    crate::serial_println!("[GRAPHICS] mode {}x{} stride {} (requested {}x{})", width, height, stride, want_w, want_h);

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
    *VRAM.lock() = Some((phys_addr as u64, vram_size));

    // Everything sized from the screen (back buffer, clip, layout, mouse)
    // follows this mode from here on.
    crate::ui::set_screen_size(width, height);

    let fb = Framebuffer {
        base_addr: phys_addr as u64,
        width,
        height,
        stride,
    };

    fb.clear(Color { r: 20, g: 20, b: 35 });
    *FRAMEBUFFER.lock() = Some(fb);
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
