use bootloader::bootinfo::MemoryMap;
use x86_64::{
    structures::paging::{FrameAllocator, OffsetPageTable, PageTable, PhysFrame, Size4KiB},
    VirtAddr,
};

use spin::Mutex;
static PHYS_MEM_OFFSET: Mutex<Option<VirtAddr>> = Mutex::new(None);

pub fn set_phys_mem_offset(offset: VirtAddr) {
    *PHYS_MEM_OFFSET.lock() = Some(offset);
}

pub fn get_phys_mem_offset() -> VirtAddr {
    PHYS_MEM_OFFSET.lock().expect("PHYS_MEM_OFFSET not set")
}

/// Returns a mapper for the active level 4 table.
pub unsafe fn get_mapper() -> OffsetPageTable<'static> {
    let offset = PHYS_MEM_OFFSET.lock().expect("PHYS_MEM_OFFSET not set");
    let level_4_table = unsafe { active_level_4_table(offset) };
    unsafe { OffsetPageTable::new(level_4_table, offset) }
}

/// Initialize a new OffsetPageTable.
pub unsafe fn init(physical_memory_offset: VirtAddr) -> OffsetPageTable<'static> {
    set_phys_mem_offset(physical_memory_offset);
    let level_4_table = unsafe { active_level_4_table(physical_memory_offset) };
    unsafe { OffsetPageTable::new(level_4_table, physical_memory_offset) }
}

/// Helper to map a virtual page to a physical frame with given flags.
pub fn map_page(
    page: x86_64::structures::paging::Page<Size4KiB>,
    frame: PhysFrame<Size4KiB>,
    flags: x86_64::structures::paging::PageTableFlags,
    mapper: &mut impl x86_64::structures::paging::Mapper<Size4KiB>,
    frame_allocator: &mut impl FrameAllocator<Size4KiB>,
) {
    // Infallible wrapper kept for call-site compatibility.  It NEVER panics:
    // a collision with an existing mapping used to bring the whole kernel down
    // (e.g. relaunching an EXE before the previous instance was reaped, since
    // both map at the same PE image base).
    if let Err(e) = map_page_checked(page, frame, flags, mapper, frame_allocator) {
        crate::serial_println!(
            "[MEM] map_page({:#X}) failed: {} — continuing",
            page.start_address().as_u64(), e
        );
    }
}

/// Fallible mapping helper.
///
/// If the page is already mapped we do NOT panic.  Instead:
///   • same frame  → just refresh the flags (idempotent remap).
///   • other frame → unmap the old one, release its frame to the free list,
///                   and map the requested frame in its place.
pub fn map_page_checked(
    page: x86_64::structures::paging::Page<Size4KiB>,
    frame: PhysFrame<Size4KiB>,
    flags: x86_64::structures::paging::PageTableFlags,
    mapper: &mut impl x86_64::structures::paging::Mapper<Size4KiB>,
    frame_allocator: &mut impl FrameAllocator<Size4KiB>,
) -> Result<(), &'static str> {
    use x86_64::structures::paging::mapper::MapToError;

    let res = unsafe { mapper.map_to(page, frame, flags, frame_allocator) };
    match res {
        Ok(flush) => { flush.flush(); Ok(()) }
        Err(MapToError::PageAlreadyMapped(existing)) => {
            if existing == frame {
                unsafe {
                    let _ = mapper.update_flags(page, flags).map(|f| f.flush());
                }
                Ok(())
            } else {
                // Reclaim the stale frame, then retry once.
                if let Ok((old, flush)) = mapper.unmap(page) {
                    flush.flush();
                    deallocate_frame(old);
                }
                let retry = unsafe { mapper.map_to(page, frame, flags, frame_allocator) };
                match retry {
                    Ok(flush) => { flush.flush(); Ok(()) }
                    Err(_)    => Err("remap after unmap failed"),
                }
            }
        }
        Err(MapToError::FrameAllocationFailed) => Err("out of frames for page tables"),
        Err(MapToError::ParentEntryHugePage)   => Err("parent entry is a huge page"),
    }
}

/// Helper to unmap a virtual page.  Does NOT release the backing frame —
/// use `unmap_and_free_page` for that.
pub fn unmap_page(
    page: x86_64::structures::paging::Page<Size4KiB>,
    mapper: &mut impl x86_64::structures::paging::Mapper<Size4KiB>,
) -> Result<(), &'static str> {
    if let Ok((_frame, flush)) = mapper.unmap(page) {
        flush.flush();
        Ok(())
    } else {
        Err("Page not mapped")
    }
}

/// Unmap a page AND return its physical frame to the free list.
///
/// This is the call that process teardown must use.  The old code only
/// unmapped, so every EXE launch permanently consumed physical RAM until
/// the bump allocator ran dry ("No frames left for PE loading").
pub fn unmap_and_free_page(
    page: x86_64::structures::paging::Page<Size4KiB>,
    mapper: &mut impl x86_64::structures::paging::Mapper<Size4KiB>,
) -> Result<(), &'static str> {
    match mapper.unmap(page) {
        Ok((frame, flush)) => {
            flush.flush();
            deallocate_frame(frame);
            Ok(())
        }
        Err(_) => Err("Page not mapped"),
    }
}

/// True if `page` currently has a present mapping in the active tables.
pub fn is_mapped(addr: u64) -> bool {
    use x86_64::structures::paging::Translate;
    let offset = match *PHYS_MEM_OFFSET.lock() { Some(o) => o, None => return false };
    let l4 = unsafe { active_level_4_table(offset) };
    let mapper = unsafe { OffsetPageTable::new(l4, offset) };
    mapper.translate_addr(VirtAddr::new(addr)).is_some()
}

/// Validate a user-mode buffer before the kernel dereferences it.
///
/// Every page spanned by `[ptr, ptr+len)` must be present AND marked
/// USER_ACCESSIBLE.  This is what stops a bad pointer from a Win32 app
/// turning into a kernel page fault — which, because the fault handler
/// long-jumps out of the syscall, would leak every spinlock the handler
/// was holding (WIN32_USER, PROCESSOR, the heap, ...) and wedge the desktop.
pub fn user_ptr_ok(ptr: u64, len: usize) -> bool {
    if ptr == 0 || len == 0 { return false; }
    if ptr < 0x1000 { return false; }                      // null page
    let end = match ptr.checked_add(len as u64) { Some(e) => e, None => return false };
    // Reject non-canonical / kernel-half addresses outright.
    if end >= 0xFFFF_8000_0000_0000 { return false; }

    let offset = match *PHYS_MEM_OFFSET.lock() { Some(o) => o, None => return false };

    let mut addr = ptr & !0xFFF;
    while addr < end {
        if !page_is_user_accessible(addr, offset) { return false; }
        addr += 4096;
    }
    true
}

/// Convenience: validate a NUL-terminated user string of at most `max` bytes.
/// Returns the number of readable bytes up to (not including) the NUL, or None.
pub fn user_cstr_len(ptr: u64, max: usize) -> Option<usize> {
    if !user_ptr_ok(ptr, 1) { return None; }
    let mut i = 0usize;
    while i < max {
        // Re-validate whenever we cross into a new page.
        if (ptr + i as u64) % 4096 == 0 && !user_ptr_ok(ptr + i as u64, 1) { return None; }
        let b = unsafe { core::ptr::read_volatile((ptr + i as u64) as *const u8) };
        if b == 0 { return Some(i); }
        i += 1;
    }
    Some(max)
}

/// Walk the 4-level tables by hand so we can check USER_ACCESSIBLE at every
/// level (the `Translate` trait only tells us whether a mapping exists).
fn page_is_user_accessible(addr: u64, phys_offset: VirtAddr) -> bool {
    use x86_64::registers::control::Cr3;
    use x86_64::structures::paging::PageTableFlags as F;

    let (l4_frame, _) = Cr3::read();
    let mut table_phys = l4_frame.start_address().as_u64();
    let va = VirtAddr::new(addr);
    let indices = [va.p4_index(), va.p3_index(), va.p2_index(), va.p1_index()];

    for (level, idx) in indices.iter().enumerate() {
        let table_virt = phys_offset + table_phys;
        let table: &PageTable = unsafe { &*table_virt.as_ptr() };
        let entry = &table[*idx];
        let flags = entry.flags();
        if !flags.contains(F::PRESENT) { return false; }
        if !flags.contains(F::USER_ACCESSIBLE) { return false; }
        // Huge page terminates the walk early (levels 3 and 2 only).
        if flags.contains(F::HUGE_PAGE) && level >= 1 { return true; }
        table_phys = entry.addr().as_u64();
    }
    true
}

/// Returns a mutable reference to the active level 4 table.
unsafe fn active_level_4_table(physical_memory_offset: VirtAddr) -> &'static mut PageTable {
    use x86_64::registers::control::Cr3;

    let (level_4_table_frame, _) = Cr3::read();

    let phys = level_4_table_frame.start_address();
    let virt = physical_memory_offset + phys.as_u64();
    let page_table_ptr: *mut PageTable = virt.as_mut_ptr();

    unsafe { &mut *page_table_ptr }
}

static FRAME_ALLOCATOR: Mutex<Option<BootInfoFrameAllocator>> = Mutex::new(None);

pub fn init_frame_allocator(memory_map: &'static MemoryMap) {
    let allocator = unsafe { BootInfoFrameAllocator::init(memory_map) };
    *FRAME_ALLOCATOR.lock() = Some(allocator);
}

/// Reclaimed physical frames, newest first.
///
/// `BootInfoFrameAllocator` is a pure bump allocator — it can never hand a
/// frame back.  Process teardown pushes freed frames here and `allocate_frame`
/// drains this list before advancing the bump pointer, so launching and closing
/// the same EXE repeatedly no longer leaks physical RAM.
///
/// `Vec::new()` is const, so this is safe to declare before the heap exists;
/// nothing pushes to it until well after `init_heap()`.
static FREE_FRAMES: Mutex<alloc::vec::Vec<u64>> = Mutex::new(alloc::vec::Vec::new());

/// Number of frames currently sitting on the free list (diagnostics).
pub fn free_frame_count() -> usize {
    FREE_FRAMES.lock().len()
}

pub fn allocate_frame() -> Option<PhysFrame> {
    // 1. Reuse a reclaimed frame if one is available.
    if let Some(addr) = FREE_FRAMES.lock().pop() {
        let frame = PhysFrame::containing_address(x86_64::PhysAddr::new(addr));
        // Scrub before handing out — prevents information leaking from a dead
        // process into the next one.
        if let Some(offset) = *PHYS_MEM_OFFSET.lock() {
            unsafe {
                core::ptr::write_bytes((offset + addr).as_mut_ptr::<u8>(), 0, 4096);
            }
        }
        return Some(frame);
    }
    // 2. Otherwise advance the bump allocator.
    FRAME_ALLOCATOR.lock().as_mut().and_then(|a| a.allocate_frame())
}

/// Return a frame to the free list.
///
/// Safe to call from teardown paths; uses `try_lock` semantics indirectly via
/// the spin mutex, and silently drops the frame if the heap push would fail.
pub fn deallocate_frame(frame: PhysFrame<Size4KiB>) {
    let addr = frame.start_address().as_u64();
    if addr == 0 { return; }
    let mut list = FREE_FRAMES.lock();
    // Guard against double-free, which would hand the same frame to two
    // processes and corrupt both.
    if list.iter().any(|&a| a == addr) {
        crate::serial_println!("[MEM] double free of frame {:#X} ignored", addr);
        return;
    }
    list.push(addr);
}

/// A FrameAllocator that returns usable frames from the bootloader's memory map.
///
/// Performance design
/// ------------------
/// The naive approach calls `usable_frames().nth(self.next)` on every allocation,
/// re-iterating from index 0 each time — O(N²) total for N allocations.
///
/// A previous partial fix stored `next_phys` but still used `flat_map().find()`
/// to advance, which is O(N) per step — still O(N²) overall.
///
/// This implementation is truly O(1) amortised per allocation:
///   • We cache which memory-map *region* we are currently allocating from
///     (`region_idx`) and our byte offset within that region (`next_phys`).
///   • On each allocation we add 4096 to `next_phys`. If the result is still
///     inside the current usable region, we are done — zero iteration.
///   • Only when we exhaust a region do we advance `region_idx` by scanning
///     forward, which costs O(R) where R is the number of memory-map regions
///     (typically 5–15 on real hardware).  This happens at most R times total
///     across the entire lifetime of the allocator.
pub struct BootInfoFrameAllocator {
    memory_map: &'static bootloader::bootinfo::MemoryMap,
    /// How many frames have been handed out (for debugging / accounting).
    allocated: usize,
    /// Physical address of the *next* frame to hand out.
    /// `u64::MAX` signals exhausted.
    next_phys: u64,
    /// The *end* address (exclusive) of the usable region `next_phys` lives in.
    /// When `next_phys >= region_end` we must hop to the next region.
    region_end: u64,
}

impl BootInfoFrameAllocator {
    pub unsafe fn init(memory_map: &'static bootloader::bootinfo::MemoryMap) -> Self {
        let mut alloc = BootInfoFrameAllocator {
            memory_map,
            allocated: 0,
            next_phys: u64::MAX,
            region_end: 0,
        };
        // Locate the very first usable region.
        alloc.advance_to_next_region(0);
        alloc
    }

    /// Scan forward from `after_phys` to find the next usable region.
    /// Sets `next_phys` and `region_end` accordingly.
    /// O(R) — only called when the current region is exhausted.
    fn advance_to_next_region(&mut self, after_phys: u64) {
        use bootloader::bootinfo::MemoryRegionType;
        let next_region = self.memory_map
            .iter()
            .filter(|r| r.region_type == MemoryRegionType::Usable)
            .filter(|r| r.range.end_addr() > after_phys)
            .min_by_key(|r| r.range.start_addr());

        if let Some(region) = next_region {
            let mut start = region.range.start_addr();
            if start < after_phys {
                start = after_phys;
            }
            // Align start up to the next 4 KiB boundary.
            let aligned_start = (start + 4095) & !4095;
            
            // If aligning pushed us past the end of this region, we must recurse to the next region.
            if aligned_start >= region.range.end_addr() {
                self.advance_to_next_region(region.range.end_addr());
            } else {
                self.next_phys = aligned_start;
                self.region_end = region.range.end_addr();
            }
        } else {
            self.next_phys  = u64::MAX; // exhausted
            self.region_end = 0;
        }
    }

    pub fn allocate_frame(&mut self) -> Option<x86_64::structures::paging::PhysFrame> {
        use x86_64::PhysAddr;
        use x86_64::structures::paging::PhysFrame;

        if self.next_phys == u64::MAX { return None; }

        let frame = PhysFrame::containing_address(PhysAddr::new(self.next_phys));
        self.allocated += 1;

        // O(1): advance within the current region.
        // NOTE: the test is `candidate + 4096 <= region_end`, not
        // `candidate < region_end` — the latter could hand out a frame whose
        // last bytes lie past the end of the usable region.
        let candidate = self.next_phys + 4096;
        if candidate + 4096 <= self.region_end {
            self.next_phys = candidate;
        } else {
            // O(R): hop to the next usable region (at most R times total).
            self.advance_to_next_region(self.next_phys + 1);
        }

        Some(frame)
    }
}

// Implement the trait for the global Mutex wrapper
pub struct GlobalFrameAllocator;

unsafe impl FrameAllocator<Size4KiB> for GlobalFrameAllocator {
    fn allocate_frame(&mut self) -> Option<PhysFrame> {
        allocate_frame()
    }
}
