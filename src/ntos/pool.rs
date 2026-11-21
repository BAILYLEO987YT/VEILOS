// Kernel pool and driver image memory, in PML4 slot 448 (0xFFFF_E000_...).
//
//   POOL_BASE   .. +1 GiB   ExAllocatePool*: a heap that grows by mapping
//                           more pages as it runs out
//   IMAGE_BASE  .. +64 GiB  driver images, one after another (each mapped
//                           when loaded)
//   SYSTEM_PTE_BASE .. +128 GiB  what drivers map (mm.rs)
//
// Both are mapped present + writable + executable, like the kernel heap:
// driver sections get their own protection in a later stage.

use core::alloc::Layout;
use core::sync::atomic::{AtomicU64, Ordering};
use linked_list_allocator::Heap;
use spin::Mutex;
use x86_64::structures::paging::{Page, PageTableFlags, PhysFrame, Size4KiB};
use x86_64::VirtAddr;

pub const REGION: u64 = 0xFFFF_E000_0000_0000;
pub const POOL_BASE: u64 = REGION;
const POOL_MAX: u64 = 1 << 30;
const POOL_START_SIZE: u64 = 4 << 20;
/// Grow by at least this much at a time.
const POOL_GROW: u64 = 4 << 20;
pub const IMAGE_BASE: u64 = REGION + (1 << 39) / 2;
/// Mappings drivers ask for (MDLs, MmMapIoSpace, contiguous memory): VA
/// only here; mm.rs maps the frames.
pub const SYSTEM_PTE_BASE: u64 = REGION + (1 << 39) / 4 * 3;

static POOL: Mutex<Option<(Heap, u64)>> = Mutex::new(None);
static NEXT_IMAGE: AtomicU64 = AtomicU64::new(IMAGE_BASE);
static NEXT_SYSTEM: AtomicU64 = AtomicU64::new(SYSTEM_PTE_BASE);

/// `bytes` (rounded to pages) of system-PTE address space, unmapped, with a
/// guard page after it. Never reused (VEILOS has 128 GiB of it).
pub fn system_va(bytes: u64) -> Option<u64> {
    let bytes = bytes.next_multiple_of(4096);
    let va = NEXT_SYSTEM.fetch_add(bytes + 4096, Ordering::Relaxed);
    (va + bytes <= SYSTEM_PTE_BASE + (128 << 30)).then_some(va)
}

/// Map `bytes` of fresh, zeroed pages at `va`. False if memory ran out.
pub fn map_fresh(va: u64, bytes: u64) -> bool {
    let mut mapper = unsafe { crate::memory::get_mapper() };
    let mut frames = crate::memory::GlobalFrameAllocator;
    let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::GLOBAL;
    let off = crate::memory::get_phys_mem_offset().as_u64();
    for p in (0..bytes).step_by(4096) {
        let Some(frame) = crate::memory::allocate_frame() else { return false };
        unsafe { core::ptr::write_bytes((off + frame.start_address().as_u64()) as *mut u8, 0, 4096) };
        let page = Page::<Size4KiB>::containing_address(VirtAddr::new(va + p));
        if crate::memory::map_page_checked(page, PhysFrame::from_start_address(frame.start_address()).unwrap(), flags, &mut mapper, &mut frames).is_err() {
            return false;
        }
    }
    true
}

pub fn init() {
    if !map_fresh(POOL_BASE, POOL_START_SIZE) {
        crate::serial_println!("[NTOS] couldn't map the kernel pool");
        return;
    }
    // The first image page too, so the region's tables exist before any
    // process copies the kernel's upper half.
    let first = NEXT_IMAGE.fetch_add(4096, Ordering::Relaxed);
    let _ = map_fresh(first, 4096);
    let mut heap = Heap::empty();
    unsafe { heap.init(POOL_BASE as *mut u8, POOL_START_SIZE as usize) };
    *POOL.lock() = Some((heap, POOL_START_SIZE));
    crate::serial_println!("[NTOS] kernel pool at {:#x}, driver images at {:#x}", POOL_BASE, IMAGE_BASE);
}

/// `size` bytes of pool, aligned to `align`, zeroed. Null if out of memory.
pub fn alloc(size: usize, align: usize) -> *mut u8 {
    let layout = match Layout::from_size_align(size.max(1), align.max(16)) {
        Ok(l) => l,
        Err(_) => return core::ptr::null_mut(),
    };
    x86_64::instructions::interrupts::without_interrupts(|| {
        let mut g = POOL.lock();
        let Some((heap, mapped)) = g.as_mut() else { return core::ptr::null_mut() };
        loop {
            if let Ok(p) = heap.allocate_first_fit(layout) {
                unsafe { core::ptr::write_bytes(p.as_ptr(), 0, size) };
                return p.as_ptr();
            }
            let grow = ((size + align) as u64).next_multiple_of(4096).max(POOL_GROW);
            if *mapped + grow > POOL_MAX || !map_fresh(POOL_BASE + *mapped, grow) {
                return core::ptr::null_mut();
            }
            unsafe { heap.extend(grow as usize) };
            *mapped += grow;
        }
    })
}

pub fn free(p: *mut u8, size: usize, align: usize) {
    if p.is_null() {
        return;
    }
    let layout = Layout::from_size_align(size.max(1), align.max(16)).unwrap();
    x86_64::instructions::interrupts::without_interrupts(|| {
        if let Some((heap, _)) = POOL.lock().as_mut() {
            unsafe { heap.deallocate(core::ptr::NonNull::new_unchecked(p), layout) };
        }
    });
}

/// A pool block with its size kept in front of it, for ExFreePool (which
/// gets only the pointer). Page-sized or larger blocks are page-aligned, as
/// Windows' are.
pub fn alloc_tracked(size: usize) -> *mut u8 {
    let align = if size >= 4096 { 4096 } else { 16 };
    let head = align.max(16);
    let base = alloc(size + head, align);
    if base.is_null() {
        return base;
    }
    unsafe {
        let p = base.add(head);
        (p as *mut u64).sub(1).write((size + head) as u64 | (align as u64) << 48);
        (p as *mut u64).sub(2).write(0x4C4F_4F50_5445_4E56); // "VNETPOOL", a check
        p
    }
}

pub fn free_tracked(p: *mut u8) {
    if p.is_null() {
        return;
    }
    unsafe {
        if (p as *mut u64).sub(2).read() != 0x4C4F_4F50_5445_4E56 {
            crate::serial_println!("[NTOS] ExFreePool({:p}): not a pool block", p);
            return;
        }
        let word = (p as *mut u64).sub(1).read();
        let align = (word >> 48) as usize;
        let total = (word & 0xFFFF_FFFF_FFFF) as usize;
        let head = align.max(16);
        (p as *mut u64).sub(2).write(0);
        free(p.sub(head), total, align);
    }
}

/// Set each page of an image to its section's protection: `flags[i]` for
/// page i (PRESENT and GLOBAL are added).
pub fn protect_pages(base: u64, flags: &[PageTableFlags]) {
    use x86_64::structures::paging::Mapper;
    let mut mapper = unsafe { crate::memory::get_mapper() };
    for (i, f) in flags.iter().enumerate() {
        let page = Page::<Size4KiB>::containing_address(VirtAddr::new(base + i as u64 * 4096));
        if let Ok(flush) = unsafe { mapper.update_flags(page, *f | PageTableFlags::PRESENT | PageTableFlags::GLOBAL) } {
            flush.flush();
        }
    }
}

/// Unmap an image's pages and give their frames back (its address range
/// isn't reused). The number of pages freed.
pub fn free_image(base: u64, bytes: u64) -> u64 {
    let mut mapper = unsafe { crate::memory::get_mapper() };
    let mut n = 0;
    for p in (0..bytes.next_multiple_of(4096)).step_by(4096) {
        let page = Page::<Size4KiB>::containing_address(VirtAddr::new(base + p));
        if crate::memory::unmap_and_free_page(page, &mut mapper).is_ok() {
            n += 1;
        }
    }
    n
}

/// Room for a driver image: `bytes` (page multiple) of mapped, zeroed
/// memory in the image region. None if memory ran out.
pub fn image_space(bytes: u64) -> Option<u64> {
    let bytes = bytes.next_multiple_of(4096);
    // A guard page between images.
    let va = NEXT_IMAGE.fetch_add(bytes + 4096, Ordering::Relaxed);
    map_fresh(va, bytes).then_some(va)
}
