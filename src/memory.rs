use bootloader::bootinfo::MemoryMap;
use x86_64::{
    structures::paging::{FrameAllocator, OffsetPageTable, PageTable, PhysFrame, Size4KiB},
    VirtAddr,
};

use core::sync::atomic::{AtomicU64, Ordering};
use spin::Mutex;

/// Virtual base of the bootloader's physical-memory mapping. Set once in
/// `init`; an atomic rather than a lock so ISRs can read it freely.
/// `u64::MAX` = not set yet.
static PHYS_MEM_OFFSET: AtomicU64 = AtomicU64::new(u64::MAX);

pub fn set_phys_mem_offset(offset: VirtAddr) {
    PHYS_MEM_OFFSET.store(offset.as_u64(), Ordering::Release);
}

fn phys_mem_offset() -> Option<VirtAddr> {
    match PHYS_MEM_OFFSET.load(Ordering::Acquire) {
        u64::MAX => None,
        o => Some(VirtAddr::new(o)),
    }
}

pub fn get_phys_mem_offset() -> VirtAddr {
    phys_mem_offset().expect("PHYS_MEM_OFFSET not set")
}

/// Returns a mapper for the active level 4 table.
pub unsafe fn get_mapper() -> OffsetPageTable<'static> {
    let offset = get_phys_mem_offset();
    let level_4_table = unsafe { active_level_4_table(offset) };
    unsafe { OffsetPageTable::new(level_4_table, offset) }
}

/// Initialize a new OffsetPageTable.
pub unsafe fn init(physical_memory_offset: VirtAddr) -> OffsetPageTable<'static> {
    set_phys_mem_offset(physical_memory_offset);
    crate::address_space::init();
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
///   • other frame → unmap the old one and map the requested frame in its
///                   place. The old frame is NOT freed: this function can't
///                   know who owns it (MMIO, a kernel page, another process),
///                   and freeing it would hand that memory out again as RAM.
///                   Its owner is responsible for it.
pub fn map_page_checked(
    page: x86_64::structures::paging::Page<Size4KiB>,
    frame: PhysFrame<Size4KiB>,
    flags: x86_64::structures::paging::PageTableFlags,
    mapper: &mut impl x86_64::structures::paging::Mapper<Size4KiB>,
    frame_allocator: &mut impl FrameAllocator<Size4KiB>,
) -> Result<(), &'static str> {
    use x86_64::structures::paging::mapper::MapToError;

    // A user page in a slot shared with the kernel would appear in the kernel
    // and in every process (see address_space).
    if flags.contains(x86_64::structures::paging::PageTableFlags::USER_ACCESSIBLE)
        && crate::address_space::is_kernel_slot(page.start_address().as_u64())
    {
        crate::serial_println!(
            "[MEM] refusing user mapping at {:#X}: shared kernel slot",
            page.start_address().as_u64()
        );
        return Err("user mapping in a kernel-shared slot");
    }

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
                if let Ok((old, flush)) = mapper.unmap(page) {
                    flush.flush();
                    crate::serial_println!(
                        "[MEM] WARN: {:#X} was mapped to frame {:#X}; replacing with {:#X} (old frame not freed)",
                        page.start_address().as_u64(), old.start_address().as_u64(),
                        frame.start_address().as_u64()
                    );
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
    let offset = match phys_mem_offset() { Some(o) => o, None => return false };
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

    let offset = match phys_mem_offset() { Some(o) => o, None => return false };

    let mut addr = ptr & !0xFFF;
    while addr < end {
        // Not there yet may mean not read from its file yet.
        if !page_is_user_accessible(addr, offset) && !crate::nt_demand::fault_in(addr) { return false; }
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

// -----| physical frame allocator |-----
//
// One bit per 4 KiB frame (1 = free), covering every frame up to the end of
// the highest usable region. The bitmap lives in usable RAM, reserved at init
// and reached through the physical-memory mapping, so the allocator never
// touches the heap. It replaced a bump allocator plus a heap-backed free list,
// which could not hand out contiguous runs, needed the heap to free a frame,
// and accepted any address as "freed RAM".
//
// The lock is only ever taken with interrupts off, so a frame freed from an
// interrupt handler can't deadlock against the code it interrupted.

/// Frames below 1 MiB are never handed out: the AP trampoline (0x8000) and
/// firmware structures live there.
const LOW_MEMORY_END: u64 = 0x10_0000;

/// Usable RAM ranges from the memory map. `deallocate_frame` refuses anything
/// outside them, so MMIO or a kernel image page can never become "free RAM".
const MAX_USABLE_RANGES: usize = 64;

struct FrameBitmap {
    words:  &'static mut [u64],
    frames: usize,
    free:   usize,
    /// Word index where single-frame searches start.
    hint:   usize,
    usable: [(u64, u64); MAX_USABLE_RANGES], // [start, end) physical
    usable_count: usize,
}

static FRAMES: Mutex<Option<FrameBitmap>> = Mutex::new(None);

impl FrameBitmap {
    #[inline]
    fn is_free(&self, frame: usize) -> bool {
        self.words[frame / 64] & (1 << (frame % 64)) != 0
    }

    #[inline]
    fn set_free(&mut self, frame: usize, free: bool) {
        let bit = 1u64 << (frame % 64);
        if free { self.words[frame / 64] |= bit; } else { self.words[frame / 64] &= !bit; }
    }

    fn is_usable_ram(&self, phys: u64) -> bool {
        phys >= LOW_MEMORY_END
            && self.usable[..self.usable_count].iter().any(|&(s, e)| phys >= s && phys + 4096 <= e)
    }

    fn alloc_one(&mut self) -> Option<usize> {
        let n = self.words.len();
        for k in 0..n {
            let w = (self.hint + k) % n;
            if self.words[w] != 0 {
                let frame = w * 64 + self.words[w].trailing_zeros() as usize;
                self.set_free(frame, false);
                self.free -= 1;
                self.hint = w;
                return Some(frame);
            }
        }
        None
    }

    /// First run of `count` free frames that starts on an `align`-frame
    /// boundary and ends below `limit` (exclusive physical address).
    fn alloc_run(&mut self, count: usize, align: usize, limit: u64) -> Option<usize> {
        let max_frame = ((limit / 4096) as usize).min(self.frames);
        let mut start = 0;
        while start + count <= max_frame {
            if start % align != 0 {
                start = (start / align + 1) * align;
                continue;
            }
            match (start..start + count).find(|&f| !self.is_free(f)) {
                Some(used) => start = used + 1,
                None => {
                    for f in start..start + count { self.set_free(f, false); }
                    self.free -= count;
                    return Some(start);
                }
            }
        }
        None
    }
}

/// Build the frame bitmap from the bootloader memory map. Call once, after
/// `init` (the bitmap is reached through the physical-memory mapping) and
/// before anything allocates a frame.
pub fn init_frame_allocator(memory_map: &'static MemoryMap) {
    use bootloader::bootinfo::MemoryRegionType;
    let offset = get_phys_mem_offset();

    // Page-aligned usable ranges.
    let mut usable = [(0u64, 0u64); MAX_USABLE_RANGES];
    let mut usable_count = 0;
    for r in memory_map.iter().filter(|r| r.region_type == MemoryRegionType::Usable) {
        let start = (r.range.start_addr() + 4095) & !4095;
        let end = r.range.end_addr() & !4095;
        if end <= start { continue; }
        if usable_count == MAX_USABLE_RANGES {
            crate::serial_println!("[MEM] WARN: more than {} usable regions; ignoring {:#X}..{:#X}",
                MAX_USABLE_RANGES, start, end);
            continue;
        }
        usable[usable_count] = (start, end);
        usable_count += 1;
    }
    let max_phys = usable[..usable_count].iter().map(|&(_, e)| e).max().unwrap_or(0);
    let frames = (max_phys / 4096) as usize;
    let words_len = (frames + 63) / 64;
    let bitmap_bytes = ((words_len * 8 + 4095) & !4095) as u64;

    // Put the bitmap in the first usable range above 1 MiB that fits it.
    let bitmap_phys = usable[..usable_count].iter()
        .map(|&(s, e)| (s.max(LOW_MEMORY_END), e))
        .find(|&(s, e)| e > s && e - s >= bitmap_bytes)
        .map(|(s, _)| s)
        .expect("[MEM] no usable region can hold the frame bitmap");

    let words = unsafe {
        core::slice::from_raw_parts_mut((offset + bitmap_phys).as_mut_ptr::<u64>(), words_len)
    };
    words.fill(0);

    let mut bitmap = FrameBitmap { words, frames, free: 0, hint: 0, usable, usable_count };
    for &(s, e) in &usable[..usable_count] {
        for phys in (s.max(LOW_MEMORY_END)..e).step_by(4096) {
            if phys >= bitmap_phys && phys < bitmap_phys + bitmap_bytes { continue; }
            bitmap.set_free((phys / 4096) as usize, true);
            bitmap.free += 1;
        }
    }
    bitmap.hint = (LOW_MEMORY_END / 4096 / 64) as usize;

    crate::serial_println!(
        "[MEM] Frame bitmap: {} frames tracked, {} free ({} MiB), bitmap {} KiB at {:#X}",
        frames, bitmap.free, bitmap.free / 256, bitmap_bytes / 1024, bitmap_phys
    );
    x86_64::instructions::interrupts::without_interrupts(|| *FRAMES.lock() = Some(bitmap));
    TOTAL_FRAMES.store(free_frame_count(), core::sync::atomic::Ordering::Relaxed);
}

fn with_frames<R>(f: impl FnOnce(&mut FrameBitmap) -> R) -> Option<R> {
    x86_64::instructions::interrupts::without_interrupts(|| FRAMES.lock().as_mut().map(f))
}

/// Number of free frames (diagnostics).
/// Frames there were to start with (0 before the allocator is up).
pub fn total_frame_count() -> usize {
    TOTAL_FRAMES.load(core::sync::atomic::Ordering::Relaxed)
}

static TOTAL_FRAMES: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

pub fn free_frame_count() -> usize {
    with_frames(|b| b.free).unwrap_or(0)
}

/// Allocate one 4 KiB frame. Frames freed through `deallocate_frame` come
/// back zeroed; never-used frames hold whatever was there at boot.
pub fn allocate_frame() -> Option<PhysFrame> {
    with_frames(|b| b.alloc_one()).flatten()
        .map(|f| PhysFrame::containing_address(x86_64::PhysAddr::new(f as u64 * 4096)))
}

/// Return a frame to the allocator. It is zeroed first, so one process's data
/// never reaches the next owner. Frames that aren't usable RAM (MMIO, the
/// kernel image, low memory) and double frees are refused and logged.
pub fn deallocate_frame(frame: PhysFrame<Size4KiB>) {
    let addr = frame.start_address().as_u64();
    let checked = with_frames(|b| {
        if !b.is_usable_ram(addr) { return Err("not usable RAM"); }
        if b.is_free((addr / 4096) as usize) { return Err("double free"); }
        Ok(())
    });
    match checked {
        Some(Ok(())) => {}
        Some(Err(why)) => {
            crate::serial_println!("[MEM] deallocate_frame({:#X}) refused: {}", addr, why);
            return;
        }
        None => return,
    }
    // Zero outside the lock; the frame is still marked used, so it can't be
    // handed out meanwhile.
    if let Some(offset) = phys_mem_offset() {
        unsafe { core::ptr::write_bytes((offset + addr).as_mut_ptr::<u8>(), 0, 4096); }
    }
    with_frames(|b| {
        b.set_free((addr / 4096) as usize, true);
        b.free += 1;
    });
}

/// Allocate `count` physically contiguous frames, the first aligned to
/// `align_frames` frames, all below `limit`. For device DMA; prefer `DmaRegion`.
pub fn allocate_contiguous(count: usize, align_frames: usize, limit: u64) -> Option<PhysFrame> {
    if count == 0 { return None; }
    with_frames(|b| b.alloc_run(count, align_frames.max(1), limit)).flatten()
        .map(|f| PhysFrame::containing_address(x86_64::PhysAddr::new(f as u64 * 4096)))
}

/// Free `count` frames starting at `start`.
pub fn deallocate_contiguous(start: PhysFrame<Size4KiB>, count: usize) {
    for i in 0..count as u64 {
        deallocate_frame(start + i);
    }
}

/// Highest address a `DmaRegion` may use: 32-bit-only devices (and AHCI HBAs
/// without S64A) can't reach above 4 GiB.
pub const DMA_LIMIT: u64 = 0x1_0000_0000;

/// Zeroed, physically contiguous memory for a device to DMA into, below
/// `DMA_LIMIT`. The CPU reaches it through the physical-memory mapping.
/// Freed on drop.
pub struct DmaRegion {
    phys:   u64,
    frames: usize,
}

impl DmaRegion {
    pub fn new(bytes: usize) -> Option<Self> {
        let frames = (bytes.max(1) + 4095) / 4096;
        let start = allocate_contiguous(frames, 1, DMA_LIMIT)?;
        let region = Self { phys: start.start_address().as_u64(), frames };
        unsafe { core::ptr::write_bytes(region.as_mut_ptr::<u8>(), 0, frames * 4096); }
        Some(region)
    }

    pub fn phys(&self) -> u64 { self.phys }

    pub fn len(&self) -> usize { self.frames * 4096 }

    pub fn as_mut_ptr<T>(&self) -> *mut T {
        (get_phys_mem_offset() + self.phys).as_mut_ptr::<T>()
    }

    /// Keep the memory for the rest of the kernel's life (drivers that never
    /// unload). Returns (physical address, CPU pointer).
    pub fn leak(self) -> (u64, *mut u8) {
        let out = (self.phys, self.as_mut_ptr::<u8>());
        core::mem::forget(self);
        out
    }
}

impl Drop for DmaRegion {
    fn drop(&mut self) {
        deallocate_contiguous(PhysFrame::containing_address(x86_64::PhysAddr::new(self.phys)), self.frames);
    }
}

// Implement the trait for the global allocator
pub struct GlobalFrameAllocator;

unsafe impl FrameAllocator<Size4KiB> for GlobalFrameAllocator {
    fn allocate_frame(&mut self) -> Option<PhysFrame> {
        allocate_frame()
    }
}
