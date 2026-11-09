// =============================================================================
// VEILOS: KUSER_SHARED_DATA (WIN32_DESIGN.md, W4)
// =============================================================================
//
// The page at 0x7FFE0000 that every Windows process can read: OS version,
// tick count, interrupt time, system time, the performance-counter
// frequency. One physical frame, mapped read-only into each process; the
// kernel writes it through the physical-memory map, and the BSP's timer
// keeps the clocks current (`tick`).
//
// System time comes from the CMOS clock read once, at the first program
// start, plus the time since: FILETIME units (100 ns since 1601-01-01), UTC.

use core::sync::atomic::{AtomicU64, Ordering};
use x86_64::structures::paging::{FrameAllocator, Mapper, Page, PageTableFlags, PhysFrame, Size4KiB};
use x86_64::{PhysAddr, VirtAddr};

use nt_abi::kusd as off;

pub const ADDRESS: u64 = 0x7FFE_0000;

/// Physical address of the shared frame (0 until the first program starts).
static FRAME: AtomicU64 = AtomicU64::new(0);
/// System time (FILETIME) at kernel time 0.
static BOOT_FILETIME: AtomicU64 = AtomicU64::new(0);

/// Kernel address of the page, if it exists.
fn page() -> Option<*mut u8> {
    let phys = FRAME.load(Ordering::Acquire);
    (phys != 0).then(|| (crate::memory::get_phys_mem_offset().as_u64() + phys) as *mut u8)
}

/// Days from 1601-01-01 to `y-m-d` (proleptic Gregorian).
pub fn days_since_1601(y: i64, m: i64, d: i64) -> i64 {
    // days_from_civil (Howard Hinnant), epoch moved from 1970 to 1601.
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468 + 134_774
}

/// The time now, as a FILETIME (what the page's SystemTime says).
pub fn system_time() -> u64 {
    let Some(p) = page() else { return 0 };
    loop {
        unsafe {
            let high1 = core::ptr::read_volatile(p.add(0x18) as *const u32);
            let low = core::ptr::read_volatile(p.add(0x14) as *const u32);
            let high2 = core::ptr::read_volatile(p.add(0x1C) as *const u32);
            if high1 == high2 {
                return (high1 as u64) << 32 | low as u64;
            }
        }
    }
}

fn write_ksystem_time(p: *mut u8, offset: usize, value: u64) {
    // Readers loop until High1Time == High2Time, so High2 goes first.
    unsafe {
        core::ptr::write_volatile(p.add(offset + 8) as *mut u32, (value >> 32) as u32);
        core::ptr::write_volatile(p.add(offset) as *mut u32, value as u32);
        core::ptr::write_volatile(p.add(offset + 4) as *mut u32, (value >> 32) as u32);
    }
}

/// Create and fill the page (once).
fn ensure() -> Result<(), &'static str> {
    if FRAME.load(Ordering::Acquire) != 0 {
        return Ok(());
    }
    let frame = crate::memory::allocate_frame().ok_or("no frame for KUSER_SHARED_DATA")?;
    let phys = frame.start_address().as_u64();
    let p = (crate::memory::get_phys_mem_offset().as_u64() + phys) as *mut u8;
    let now = crate::shell::wall_clock();
    let days = days_since_1601(now.year as i64, now.month as i64, now.day as i64);
    let secs = days * 86_400 + now.hour as i64 * 3600 + now.minute as i64 * 60 + now.second as i64;
    let ms = crate::interrupts::TICKS.load(Ordering::Relaxed);
    BOOT_FILETIME.store((secs as u64 * 10_000_000).saturating_sub(ms * 10_000), Ordering::Release);
    unsafe {
        core::ptr::write_bytes(p, 0, 4096);
        let w32 = |o: usize, v: u32| core::ptr::write_unaligned(p.add(o) as *mut u32, v);
        w32(off::TICK_COUNT_MULTIPLIER, 1 << 24); // tick count is in ms
        w32(off::NT_BUILD_NUMBER, 19041);
        w32(off::NT_PRODUCT_TYPE, 1); // NtProductWinNt
        *p.add(off::PRODUCT_TYPE_IS_VALID) = 1;
        core::ptr::write_unaligned(p.add(off::NATIVE_PROCESSOR_ARCHITECTURE) as *mut u16, 9); // AMD64
        w32(off::NT_MAJOR_VERSION, 10);
        w32(off::NT_MINOR_VERSION, 0);
        core::ptr::write_unaligned(p.add(off::QPC_FREQUENCY) as *mut u64, crate::time::tsc_hz());
        w32(off::COOKIE, (crate::time::rdtsc() as u32) | 1);
    }
    FRAME.store(phys, Ordering::Release);
    tick(ms);
    crate::serial_println!("[KUSD] shared page at {:#x}, {:04}-{:02}-{:02} {:02}:{:02}:{:02} UTC", phys,
        now.year, now.month, now.day, now.hour, now.minute, now.second);
    Ok(())
}

/// Map the page read-only at 0x7FFE0000 in the address space `mapper` edits.
pub fn map_into(mapper: &mut impl Mapper<Size4KiB>, fa: &mut impl FrameAllocator<Size4KiB>) -> Result<(), &'static str> {
    ensure()?;
    let frame = PhysFrame::containing_address(PhysAddr::new(FRAME.load(Ordering::Acquire)));
    let flags = PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE | PageTableFlags::NO_EXECUTE;
    crate::memory::map_page_checked(Page::containing_address(VirtAddr::new(ADDRESS)), frame, flags, mapper, fa)
}

/// The BSP's timer: update the clocks to kernel time `now_ms`.
pub fn tick(now_ms: u64) {
    let Some(p) = page() else { return };
    let interrupt_time = now_ms * 10_000;
    write_ksystem_time(p, off::INTERRUPT_TIME, interrupt_time);
    write_ksystem_time(p, off::SYSTEM_TIME, BOOT_FILETIME.load(Ordering::Relaxed) + interrupt_time);
    write_ksystem_time(p, off::TICK_COUNT, now_ms);
    unsafe { core::ptr::write_volatile(p.add(off::TICK_COUNT_LOW_DEPRECATED) as *mut u32, now_ms as u32) };
}
