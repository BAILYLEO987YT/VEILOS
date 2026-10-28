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

pub fn probe_gpu() {
    println!("[GRAPHICS] Probing for Bochs/QEMU GPU (BGA)...");
    
    // Set resolution explicitly: 1024x768x32
    bga_write(4, 0x00); // VBE_DISPI_INDEX_ENABLE -> Disable
    bga_write(1, 1024); // VBE_DISPI_INDEX_XRES
    bga_write(2, 768);  // VBE_DISPI_INDEX_YRES
    bga_write(3, 32);   // VBE_DISPI_INDEX_BPP
    bga_write(4, 0x01 | 0x40); // VBE_DISPI_INDEX_ENABLE -> (Enabled | LFB)
    
    println!("[GRAPHICS] Hardware Resolution Set: 1024x768x32");

    // Scan PCI for the Buffer address
    let devices = pci::scan_buses();
    for dev in devices {
        if dev.class == 0x03 { // Display Controller
            let bar0 = pci::read_bar(dev.bus, dev.device, dev.function, 0);
            let phys_addr = bar0 & !0xF; // Mask out flags
            
            if phys_addr != 0 {
                println!("[GRAPHICS] Found GPU at PCI {:02X}:{:02X}. BAR0: {:#X}", dev.bus, dev.device, phys_addr);
                
                // CRITICAL: We MUST map the physical address to a virtual address!
                // We'll use identity mapping for the hardware for simplicity.
                let mut mapper = unsafe { crate::memory::get_mapper() };
                let mut frame_allocator = crate::memory::GlobalFrameAllocator;
                
                let fb_size = crate::ui::SCREEN_W * crate::ui::SCREEN_H * 4;
                if let Err(e) = map_direct_hw(phys_addr as u64, fb_size as u64, phys_addr as u64, &mut mapper, &mut frame_allocator) {
                    println!("[GRAPHICS] ERROR: Failed to map framebuffer: {}", e);
                    return;
                }
                
                let fb = Framebuffer {
                    base_addr: phys_addr as u64,
                    width:  crate::ui::SCREEN_W,
                    height: crate::ui::SCREEN_H,
                    stride: crate::ui::SCREEN_W,
                };
                
                fb.clear(Color { r: 20, g: 20, b: 35 });
                *FRAMEBUFFER.lock() = Some(fb);
                println!("[GRAPHICS] Manually mapped framebuffer at {:#X}", phys_addr);
                return;
            }
        }
    }
    
    println!("[GRAPHICS] No PCI GPU found. Falling back to VGA Text Mode.");
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

        let flags = PageTableFlags::PRESENT
            | PageTableFlags::WRITABLE
            | PageTableFlags::NO_CACHE
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
