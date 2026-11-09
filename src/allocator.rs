use core::alloc::{GlobalAlloc, Layout};
use linked_list_allocator::LockedHeap;
use x86_64::instructions::interrupts::without_interrupts;
use x86_64::{
    structures::paging::{
        mapper::MapToError, FrameAllocator, Mapper, Page, PageTableFlags, Size4KiB,
    },
    VirtAddr,
};

/// The kernel heap. Every alloc/free runs with interrupts off, so an
/// interrupt handler that allocates can never spin on the heap lock held by
/// the code it interrupted (a guaranteed deadlock on that CPU). Other CPUs
/// still contend normally.
struct IrqSafeHeap(LockedHeap);

unsafe impl GlobalAlloc for IrqSafeHeap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        without_interrupts(|| unsafe { self.0.alloc(layout) })
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        without_interrupts(|| unsafe { self.0.dealloc(ptr, layout) })
    }
}

#[global_allocator]
static ALLOCATOR: IrqSafeHeap = IrqSafeHeap(LockedHeap::empty());

// We pick an arbitrary unused virtual address for our heap to start at.
pub const HEAP_START: usize = 0x4444_4444_0000;
/// The heap is an eighth of RAM within these bounds. Every user thread's kernel
/// stack comes from it, so what fits here limits how many threads can run.
const MIN_HEAP_SIZE: usize = 64 * 1024 * 1024;
const MAX_HEAP_SIZE: usize = 512 * 1024 * 1024;

fn chosen_heap_size() -> usize {
    (crate::memory::total_frame_count() * 4096 / 8).clamp(MIN_HEAP_SIZE, MAX_HEAP_SIZE)
}

pub fn init_heap(
    mapper: &mut impl Mapper<Size4KiB>,
    frame_allocator: &mut impl FrameAllocator<Size4KiB>,
) -> Result<(), MapToError<Size4KiB>> {
    let heap_size = chosen_heap_size();
    // Determine the range of memory pages our heap will use
    let page_range = {
        let heap_start = VirtAddr::new(HEAP_START as u64);
        let heap_end = heap_start + heap_size as u64 - 1u64;
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
        without_interrupts(|| ALLOCATOR.0.lock().init(HEAP_START as *mut u8, heap_size));
    }

    Ok(())
}

/// (bytes in use, heap size). Like every heap access, runs with interrupts
/// off (see `IrqSafeHeap`).
pub fn heap_usage() -> (usize, usize) {
    without_interrupts(|| {
        let heap = ALLOCATOR.0.lock();
        (heap.used(), heap.size())
    })
}
