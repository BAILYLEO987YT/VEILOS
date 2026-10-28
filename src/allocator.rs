use linked_list_allocator::LockedHeap;
use x86_64::{
    structures::paging::{
        mapper::MapToError, FrameAllocator, Mapper, Page, PageTableFlags, Size4KiB,
    },
    VirtAddr,
};

#[global_allocator]
static ALLOCATOR: LockedHeap = LockedHeap::empty();

// We pick an arbitrary unused virtual address for our heap to start at.
pub const HEAP_START: usize = 0x4444_4444_0000;
pub const HEAP_SIZE: usize = 64 * 1024 * 1024; // 64 MB for Professional Compositing

pub fn init_heap(
    mapper: &mut impl Mapper<Size4KiB>,
    frame_allocator: &mut impl FrameAllocator<Size4KiB>,
) -> Result<(), MapToError<Size4KiB>> {
    // Determine the range of memory pages our heap will use
    let page_range = {
        let heap_start = VirtAddr::new(HEAP_START as u64);
        let heap_end = heap_start + HEAP_SIZE as u64 - 1u64;
        let heap_start_page = Page::containing_address(heap_start);
        let heap_end_page = Page::containing_address(heap_end);
        Page::range_inclusive(heap_start_page, heap_end_page)
    };

    // Map each virtual page in that range to a physical frame
    for page in page_range {
        let frame = frame_allocator
            .allocate_frame()
            .ok_or(MapToError::FrameAllocationFailed)?;
            
        let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE;
        
        // This is unsafe because the caller must guarantee that the frame isn't already mapped
        unsafe {
            mapper.map_to(page, frame, flags, frame_allocator)?.flush()
        };
    }

    // Now that the virtual memory is securely mapped to physical RAM, initialize the Allocator!
    unsafe {
        ALLOCATOR.lock().init(HEAP_START as *mut u8, HEAP_SIZE);
    }

    Ok(())
}

/// (bytes in use, heap size). Interrupts are off while the allocator lock is
/// held, so an interrupt handler that allocates can never spin on it.
pub fn heap_usage() -> (usize, usize) {
    x86_64::instructions::interrupts::without_interrupts(|| {
        let heap = ALLOCATOR.lock();
        (heap.used(), heap.size())
    })
}
