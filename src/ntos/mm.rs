// Memory for drivers: MDLs, physically contiguous memory, device memory
// (MmMapIoSpace), physical addresses, and mappings of pages into kernel
// address space ("system PTEs").
//
// VEILOS never pages memory out, so "locking" pages is only finding them:
// a program's pages may not exist yet (nt_demand.rs makes them on first
// touch), so probing touches each one first. Mappings drivers ask for go in
// the system-PTE part of the ntos region (pool.rs) with the cache type they
// ask for: cached, write-combining (PAT entry 1, see smp.rs) or uncached.
//
// MDL (x64): Next 0, Size 8 (i16), MdlFlags 0xA (i16), Process 0x10,
// MappedSystemVa 0x18, StartVa 0x20, ByteCount 0x28, ByteOffset 0x2C, then
// one PFN (u64) per page. Checked in k1's layout.c.

use alloc::collections::BTreeMap;
use spin::Mutex;
use x86_64::structures::paging::{Mapper, Page, PageTableFlags, PhysFrame, Size4KiB, Translate};
use x86_64::{PhysAddr, VirtAddr};

use super::{pool, seh};

pub const MDL_HEADER: usize = 0x30;
const MDL_SIZE: usize = 0x08;
const MDL_FLAGS: usize = 0x0A;
const MDL_PROCESS: usize = 0x10;
const MDL_MAPPED_VA: usize = 0x18;
const MDL_START_VA: usize = 0x20;
const MDL_BYTE_COUNT: usize = 0x28;
const MDL_BYTE_OFFSET: usize = 0x2C;

pub const MDL_MAPPED_TO_SYSTEM_VA: u16 = 0x0001;
pub const MDL_PAGES_LOCKED: u16 = 0x0002;
pub const MDL_SOURCE_IS_NONPAGED_POOL: u16 = 0x0004;
const MDL_PARTIAL: u16 = 0x0010;

// MEMORY_CACHING_TYPE
const MM_NON_CACHED: u32 = 0;
const MM_WRITE_COMBINED: u32 = 2;

unsafe fn rd<T: Copy>(p: *mut u8, off: usize) -> T {
    unsafe { (p.add(off) as *const T).read_unaligned() }
}
unsafe fn wr<T>(p: *mut u8, off: usize, v: T) {
    unsafe { (p.add(off) as *mut T).write_unaligned(v) }
}

fn pages_spanned(va: u64, len: u64) -> u64 {
    if len == 0 { 0 } else { ((va & 0xFFF) + len).div_ceil(4096) }
}

/// The physical address behind `va` in the current address space.
pub fn phys_of(va: u64) -> Option<u64> {
    let mapper = unsafe { crate::memory::get_mapper() };
    mapper.translate_addr(VirtAddr::new_truncate(va)).map(|p| p.as_u64())
}

fn cache_flags(cache: u32) -> PageTableFlags {
    match cache {
        MM_NON_CACHED => PageTableFlags::NO_CACHE | PageTableFlags::WRITE_THROUGH,
        MM_WRITE_COMBINED => PageTableFlags::WRITE_THROUGH, // PAT entry 1: write-combining
        _ => PageTableFlags::empty(),
    }
}

/// Map physical pages (`pfns`) contiguously into system space with
/// `cache`; returns the base VA.
fn map_pfns(pfns: &[u64], cache: u32) -> Option<u64> {
    let va = pool::system_va(pfns.len() as u64 * 4096)?;
    let mut mapper = unsafe { crate::memory::get_mapper() };
    let mut frames = crate::memory::GlobalFrameAllocator;
    let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::GLOBAL | cache_flags(cache);
    for (i, &pfn) in pfns.iter().enumerate() {
        let page = Page::<Size4KiB>::containing_address(VirtAddr::new(va + i as u64 * 4096));
        let frame = PhysFrame::<Size4KiB>::containing_address(PhysAddr::new(pfn * 4096));
        if crate::memory::map_page_checked(page, frame, flags, &mut mapper, &mut frames).is_err() {
            return None;
        }
    }
    Some(va)
}

/// Unmap `pages` pages at `va` (the frames stay their owner's).
fn unmap(va: u64, pages: u64) {
    let mut mapper = unsafe { crate::memory::get_mapper() };
    for i in 0..pages {
        let page = Page::<Size4KiB>::containing_address(VirtAddr::new(va + i * 4096));
        if let Ok((_, flush)) = mapper.unmap(page) {
            flush.flush();
        }
    }
}

// -----| physical addresses, validity |-----

pub extern "win64" fn MmGetPhysicalAddress(va: u64) -> u64 {
    phys_of(va).unwrap_or(0)
}

pub extern "win64" fn MmIsAddressValid(va: u64) -> u8 {
    phys_of(va).is_some() as u8
}

// -----| MDLs |-----

/// IoAllocateMdl(VirtualAddress, Length, SecondaryBuffer, ChargeQuota, Irp).
pub extern "win64" fn IoAllocateMdl(va: u64, len: u32, secondary: u8, _quota: u8, irp: *mut u8) -> *mut u8 {
    let pages = pages_spanned(va, len as u64) as usize;
    let size = MDL_HEADER + pages * 8;
    let mdl = pool::alloc_tracked(size);
    if mdl.is_null() {
        return mdl;
    }
    unsafe {
        wr::<i16>(mdl, MDL_SIZE, size as i16);
        wr::<u64>(mdl, MDL_START_VA, va & !0xFFF);
        wr::<u32>(mdl, MDL_BYTE_COUNT, len);
        wr::<u32>(mdl, MDL_BYTE_OFFSET, (va & 0xFFF) as u32);
        if !irp.is_null() {
            let head = irp.add(super::io::IRP_MDL_ADDRESS) as *mut *mut u8;
            if secondary == 0 || (*head).is_null() {
                head.write(mdl);
            } else {
                // Chained after the IRP's existing MDLs.
                let mut m = *head;
                while !(*(m as *mut *mut u8)).is_null() {
                    m = *(m as *mut *mut u8);
                }
                (m as *mut *mut u8).write(mdl);
            }
        }
    }
    mdl
}

pub extern "win64" fn IoFreeMdl(mdl: *mut u8) {
    unsafe {
        let flags: u16 = rd(mdl, MDL_FLAGS);
        if flags & MDL_MAPPED_TO_SYSTEM_VA != 0 && flags & MDL_SOURCE_IS_NONPAGED_POOL == 0 {
            let va: u64 = rd(mdl, MDL_MAPPED_VA);
            unmap(va & !0xFFF, mdl_pages(mdl));
        }
    }
    pool::free_tracked(mdl);
}

fn mdl_pages(mdl: *mut u8) -> u64 {
    unsafe { pages_spanned(rd::<u64>(mdl, MDL_START_VA) + rd::<u32>(mdl, MDL_BYTE_OFFSET) as u64, rd::<u32>(mdl, MDL_BYTE_COUNT) as u64) }
}

fn pfn_array(mdl: *mut u8) -> *mut u64 {
    unsafe { mdl.add(MDL_HEADER) as *mut u64 }
}

/// Fill in the PFNs of a nonpaged (pool) buffer the MDL describes.
pub extern "win64" fn MmBuildMdlForNonPagedPool(mdl: *mut u8) {
    unsafe {
        let start: u64 = rd(mdl, MDL_START_VA);
        for i in 0..mdl_pages(mdl) {
            pfn_array(mdl).add(i as usize).write(phys_of(start + i * 4096).unwrap_or(0) >> 12);
        }
        wr::<u64>(mdl, MDL_MAPPED_VA, start + rd::<u32>(mdl, MDL_BYTE_OFFSET) as u64);
        wr::<u16>(mdl, MDL_FLAGS, rd::<u16>(mdl, MDL_FLAGS) | MDL_SOURCE_IS_NONPAGED_POOL);
    }
}

/// Lock (find) the pages of the buffer an MDL describes: a program's or the
/// kernel's. A page that isn't there raises STATUS_ACCESS_VIOLATION into
/// the caller's __try.
fn probe_and_lock(args: *const u64, c: *mut seh::Context) {
    let mdl = unsafe { *args } as *mut u8;
    unsafe {
        let start: u64 = rd(mdl, MDL_START_VA);
        for i in 0..mdl_pages(mdl) {
            let va = start + i * 4096;
            let mut pa = phys_of(va);
            if pa.is_none() && va < seh::MM_USER_PROBE_ADDRESS {
                // A program's page not made yet: touching it makes it.
                if crate::memory::user_ptr_ok(va, 1) {
                    crate::smp::user_access(|| core::ptr::read_volatile(va as *const u8));
                    pa = phys_of(va);
                }
            }
            match pa {
                Some(p) => pfn_array(mdl).add(i as usize).write(p >> 12),
                None => seh::raise_access_violation(va, c),
            }
        }
        if start < seh::MM_USER_PROBE_ADDRESS {
            wr::<u64>(mdl, MDL_PROCESS, super::io::IoGetCurrentProcess() as u64);
        }
        wr::<u16>(mdl, MDL_FLAGS, rd::<u16>(mdl, MDL_FLAGS) | MDL_PAGES_LOCKED);
    }
}
seh::capture!(MmProbeAndLockPages, probe_and_lock);

/// The I/O manager's own probe-and-lock of a program's buffer (direct
/// I/O): an MDL with its pages found, or STATUS_ACCESS_VIOLATION.
pub fn lock_user_buffer(va: u64, len: usize) -> Result<*mut u8, super::NtStatus> {
    let mdl = IoAllocateMdl(va, len as u32, 0, 0, core::ptr::null_mut());
    if mdl.is_null() {
        return Err(super::STATUS_NO_MEMORY);
    }
    unsafe {
        for i in 0..mdl_pages(mdl) {
            let page = (va & !0xFFF) + i * 4096;
            if phys_of(page).is_none() && crate::memory::user_ptr_ok(page.max(va), 1) {
                crate::smp::user_access(|| core::ptr::read_volatile(page.max(va) as *const u8));
            }
            match phys_of(page) {
                Some(p) => pfn_array(mdl).add(i as usize).write(p >> 12),
                None => {
                    pool::free_tracked(mdl);
                    return Err(seh::STATUS_ACCESS_VIOLATION as i32);
                }
            }
        }
        wr::<u64>(mdl, MDL_PROCESS, super::io::IoGetCurrentProcess() as u64);
        wr::<u16>(mdl, MDL_FLAGS, MDL_PAGES_LOCKED);
    }
    Ok(mdl)
}

pub extern "win64" fn MmUnlockPages(mdl: *mut u8) {
    unsafe {
        let flags: u16 = rd(mdl, MDL_FLAGS);
        if flags & MDL_MAPPED_TO_SYSTEM_VA != 0 && flags & MDL_SOURCE_IS_NONPAGED_POOL == 0 {
            unmap(rd::<u64>(mdl, MDL_MAPPED_VA) & !0xFFF, mdl_pages(mdl));
        }
        wr::<u16>(mdl, MDL_FLAGS, flags & !(MDL_PAGES_LOCKED | MDL_MAPPED_TO_SYSTEM_VA));
    }
}

/// Pages a driver mapped into a program: user VA -> (address space, pages).
static USER_MAPS: Mutex<BTreeMap<u64, (u64, u64)>> = Mutex::new(BTreeMap::new());

/// UserMode: the MDL's pages in the current program's address space (the
/// program whose request the driver is serving), writable, not executable.
/// The pages stay the driver's: the program's memory teardown frees neither
/// leaf pages of its tables nor regions recorded as images, so a program
/// that exits without the driver unmapping them doesn't free them.
fn map_into_program(mdl: *mut u8, cache: u32) -> u64 {
    let Some(proc) = crate::process::PROCESSOR.lock().get_active() else { return 0 };
    let n = mdl_pages(mdl);
    let Some(base) = proc.vad.lock().reserve_view((n * 4096) as usize) else { return 0 };
    let flags = PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE | PageTableFlags::WRITABLE | PageTableFlags::NO_EXECUTE | cache_flags(cache);
    let ok = crate::address_space::with(proc.cr3, || {
        let mut mapper = unsafe { crate::memory::get_mapper() };
        let mut frames = crate::memory::GlobalFrameAllocator;
        (0..n).all(|i| {
            let pfn = unsafe { pfn_array(mdl).add(i as usize).read() };
            let page = Page::<Size4KiB>::containing_address(VirtAddr::new(base + i * 4096));
            let frame = PhysFrame::<Size4KiB>::containing_address(PhysAddr::new(pfn * 4096));
            crate::memory::map_page_checked(page, frame, flags, &mut mapper, &mut frames).is_ok()
        })
    });
    if !ok {
        proc.vad.lock().forget(base);
        return 0;
    }
    locked(|| USER_MAPS.lock().insert(base, (proc.cr3, n)));
    base + unsafe { rd::<u32>(mdl, MDL_BYTE_OFFSET) } as u64
}

/// Undo `map_into_program`; false if `va` isn't one of its mappings.
fn unmap_from_program(va: u64) -> bool {
    let base = va & !0xFFF;
    let Some((cr3, n)) = locked(|| USER_MAPS.lock().remove(&base)) else { return false };
    crate::address_space::with(cr3, || unmap(base, n));
    if let Some(proc) = crate::process::PROCESSOR.lock().get_active().filter(|p| p.cr3 == cr3) {
        proc.vad.lock().forget(base);
    }
    true
}

/// MmMapLockedPagesSpecifyCache(Mdl, AccessMode, CacheType, RequestedAddress,
/// BugCheckOnFailure, Priority): KernelMode in system space, UserMode in
/// the program whose request this is.
pub extern "win64" fn MmMapLockedPagesSpecifyCache(mdl: *mut u8, mode: u8, cache: u32, _req: u64, _bugcheck: u32, _priority: u32) -> u64 {
    if mode != 0 {
        return map_into_program(mdl, cache);
    }
    unsafe {
        let flags: u16 = rd(mdl, MDL_FLAGS);
        if flags & (MDL_MAPPED_TO_SYSTEM_VA | MDL_SOURCE_IS_NONPAGED_POOL) != 0 {
            return rd(mdl, MDL_MAPPED_VA);
        }
        let n = mdl_pages(mdl) as usize;
        let pfns = core::slice::from_raw_parts(pfn_array(mdl), n);
        let Some(va) = map_pfns(pfns, cache) else { return 0 };
        let mapped = va + rd::<u32>(mdl, MDL_BYTE_OFFSET) as u64;
        wr::<u64>(mdl, MDL_MAPPED_VA, mapped);
        wr::<u16>(mdl, MDL_FLAGS, flags | MDL_MAPPED_TO_SYSTEM_VA);
        mapped
    }
}

/// Map an MDL's pages at `va` (reserved address space): the mapped address.
pub fn map_mdl_at(mdl: *mut u8, va: u64, cache: u32) -> u64 {
    let mut mapper = unsafe { crate::memory::get_mapper() };
    let mut frames = crate::memory::GlobalFrameAllocator;
    let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::GLOBAL | cache_flags(cache);
    for i in 0..mdl_pages(mdl) {
        let pfn = unsafe { pfn_array(mdl).add(i as usize).read() };
        let page = Page::<Size4KiB>::containing_address(VirtAddr::new(va + i * 4096));
        let frame = PhysFrame::<Size4KiB>::containing_address(PhysAddr::new(pfn * 4096));
        if crate::memory::map_page_checked(page, frame, flags, &mut mapper, &mut frames).is_err() {
            return 0;
        }
    }
    unsafe {
        let mapped = va + rd::<u32>(mdl, MDL_BYTE_OFFSET) as u64;
        wr::<u64>(mdl, MDL_MAPPED_VA, mapped);
        wr::<u16>(mdl, MDL_FLAGS, rd::<u16>(mdl, MDL_FLAGS) | MDL_MAPPED_TO_SYSTEM_VA);
        mapped
    }
}

pub extern "win64" fn MmMapLockedPages(mdl: *mut u8, mode: u8) -> u64 {
    MmMapLockedPagesSpecifyCache(mdl, mode, 1, 0, 0, 0)
}

pub extern "win64" fn MmUnmapLockedPages(va: u64, mdl: *mut u8) {
    if va < seh::MM_USER_PROBE_ADDRESS && unmap_from_program(va) {
        return;
    }
    unsafe {
        unmap(va & !0xFFF, mdl_pages(mdl));
        wr::<u16>(mdl, MDL_FLAGS, rd::<u16>(mdl, MDL_FLAGS) & !MDL_MAPPED_TO_SYSTEM_VA);
    }
}

/// IoBuildPartialMdl(Source, Target, VirtualAddress, Length): Target
/// describes part of Source's buffer, sharing its pages.
pub extern "win64" fn IoBuildPartialMdl(src: *mut u8, dst: *mut u8, va: u64, len: u32) {
    unsafe {
        let src_start = rd::<u64>(src, MDL_START_VA);
        let len = if len == 0 { rd::<u32>(src, MDL_BYTE_COUNT) - (va - src_start - rd::<u32>(src, MDL_BYTE_OFFSET) as u64) as u32 } else { len };
        wr::<u64>(dst, MDL_START_VA, va & !0xFFF);
        wr::<u32>(dst, MDL_BYTE_OFFSET, (va & 0xFFF) as u32);
        wr::<u32>(dst, MDL_BYTE_COUNT, len);
        let first = ((va & !0xFFF) - src_start) / 4096;
        for i in 0..pages_spanned(va, len as u64) {
            pfn_array(dst).add(i as usize).write(pfn_array(src).add((first + i) as usize).read());
        }
        wr::<u16>(dst, MDL_FLAGS, MDL_PARTIAL | (rd::<u16>(src, MDL_FLAGS) & MDL_PAGES_LOCKED));
    }
}

/// MmAllocatePagesForMdlEx(LowAddress, HighAddress, SkipBytes, TotalBytes,
/// CacheType, Flags): fresh, zeroed pages below HighAddress, described by a
/// new MDL (not mapped anywhere).
pub extern "win64" fn MmAllocatePagesForMdlEx(_low: u64, high: u64, _skip: u64, total: usize, _cache: u32, _flags: u32) -> *mut u8 {
    let pages = (total as u64).div_ceil(4096);
    let mdl = IoAllocateMdl(0, (pages * 4096) as u32, 0, 0, core::ptr::null_mut());
    if mdl.is_null() {
        return mdl;
    }
    let off = crate::memory::get_phys_mem_offset().as_u64();
    for i in 0..pages {
        let Some(f) = crate::memory::allocate_contiguous(1, 1, high.saturating_add(1).max(4096)) else {
            // Give back what was taken.
            for j in 0..i {
                let pfn = unsafe { pfn_array(mdl).add(j as usize).read() };
                crate::memory::deallocate_frame(PhysFrame::containing_address(PhysAddr::new(pfn * 4096)));
            }
            pool::free_tracked(mdl);
            return core::ptr::null_mut();
        };
        let pa = f.start_address().as_u64();
        unsafe {
            core::ptr::write_bytes((off + pa) as *mut u8, 0, 4096);
            pfn_array(mdl).add(i as usize).write(pa >> 12);
        }
    }
    unsafe { wr::<u16>(mdl, MDL_FLAGS, MDL_PAGES_LOCKED) };
    mdl
}

pub extern "win64" fn MmAllocatePagesForMdl(low: u64, high: u64, skip: u64, total: usize) -> *mut u8 {
    MmAllocatePagesForMdlEx(low, high, skip, total, 1, 0)
}

/// Free the pages MmAllocatePagesForMdl(Ex) gave (the MDL itself is freed
/// with ExFreePool).
pub extern "win64" fn MmFreePagesFromMdl(mdl: *mut u8) {
    for i in 0..mdl_pages(mdl) {
        let pfn = unsafe { pfn_array(mdl).add(i as usize).read() };
        if pfn != 0 {
            crate::memory::deallocate_frame(PhysFrame::containing_address(PhysAddr::new(pfn * 4096)));
            unsafe { pfn_array(mdl).add(i as usize).write(0) };
        }
    }
}

// -----| contiguous memory, device memory |-----

/// What a contiguous allocation or a device mapping took: VA -> (first
/// frame, pages, frames are ours to free).
static RANGES: Mutex<BTreeMap<u64, (u64, u64, bool)>> = Mutex::new(BTreeMap::new());

fn locked<R>(f: impl FnOnce() -> R) -> R {
    x86_64::instructions::interrupts::without_interrupts(f)
}

/// MmAllocateContiguousMemorySpecifyCache(NumberOfBytes, Lowest, Highest,
/// BoundaryAddressMultiple, CacheType).
pub extern "win64" fn MmAllocateContiguousMemorySpecifyCache(bytes: usize, _low: u64, high: u64, boundary: u64, cache: u32) -> u64 {
    let pages = (bytes as u64).div_ceil(4096).max(1);
    // A boundary the run mustn't cross: aligning it to its own size (a
    // power of two at least as big) keeps it inside one.
    let align = if boundary != 0 { (pages * 4096).next_power_of_two().min(boundary) / 4096 } else { 1 };
    let Some(f) = crate::memory::allocate_contiguous(pages as usize, align.max(1) as usize, high.saturating_add(1).max(4096)) else {
        return 0;
    };
    let pa = f.start_address().as_u64();
    let off = crate::memory::get_phys_mem_offset().as_u64();
    unsafe { core::ptr::write_bytes((off + pa) as *mut u8, 0, (pages * 4096) as usize) };
    let pfns: alloc::vec::Vec<u64> = (0..pages).map(|i| (pa >> 12) + i).collect();
    let Some(va) = map_pfns(&pfns, cache) else {
        crate::memory::deallocate_contiguous(f, pages as usize);
        return 0;
    };
    locked(|| RANGES.lock().insert(va, (pa >> 12, pages, true)));
    va
}

pub extern "win64" fn MmAllocateContiguousMemory(bytes: usize, high: u64) -> u64 {
    MmAllocateContiguousMemorySpecifyCache(bytes, 0, high, 0, 1)
}

pub extern "win64" fn MmAllocateNonCachedMemory(bytes: usize) -> u64 {
    MmAllocateContiguousMemorySpecifyCache(bytes, 0, u64::MAX, 0, MM_NON_CACHED)
}

fn release(va: u64) {
    if let Some((pfn, pages, owned)) = locked(|| RANGES.lock().remove(&va)) {
        unmap(va, pages);
        if owned {
            crate::memory::deallocate_contiguous(PhysFrame::containing_address(PhysAddr::new(pfn * 4096)), pages as usize);
        }
    }
}

pub extern "win64" fn MmFreeContiguousMemory(va: u64) {
    release(va)
}

pub extern "win64" fn MmFreeContiguousMemorySpecifyCache(va: u64, _bytes: usize, _cache: u32) {
    release(va)
}

pub extern "win64" fn MmFreeNonCachedMemory(va: u64, _bytes: usize) {
    release(va)
}

/// MmMapIoSpace(PhysicalAddress, NumberOfBytes, CacheType): device memory
/// (or any physical range) in system space.
pub extern "win64" fn MmMapIoSpace(pa: u64, bytes: usize, cache: u32) -> u64 {
    let first = pa >> 12;
    let pages = pages_spanned(pa, bytes as u64);
    let pfns: alloc::vec::Vec<u64> = (0..pages).map(|i| first + i).collect();
    let Some(va) = map_pfns(&pfns, cache) else { return 0 };
    locked(|| RANGES.lock().insert(va, (first, pages, false)));
    va + (pa & 0xFFF)
}

/// MmMapIoSpaceEx(PhysicalAddress, NumberOfBytes, Protect): PAGE_NOCACHE
/// (0x200) uncached, PAGE_WRITECOMBINE (0x400) write-combining.
pub extern "win64" fn MmMapIoSpaceEx(pa: u64, bytes: usize, protect: u32) -> u64 {
    let cache = if protect & 0x400 != 0 { MM_WRITE_COMBINED } else if protect & 0x200 != 0 { MM_NON_CACHED } else { 1 };
    MmMapIoSpace(pa, bytes, cache)
}

pub extern "win64" fn MmUnmapIoSpace(va: u64, _bytes: usize) {
    release(va & !0xFFF)
}
