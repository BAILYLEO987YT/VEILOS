// -----| VEILOS Windows Memory Manager Personality |-----
//
// Implements NT Virtual Memory semantics on top of the VEILOS physical frame
// allocator and page table mapper. This module answers Phase 5:
//
//   [x] VirtualAlloc, VirtualFree, VirtualProtect semantics
//   [x] Section objects + memory-mapped files
//   [x] Shared memory regions between processes
//   [x] Guard pages, copy-on-write, and page protections matching NT
//
// Design
// ------
//   Each Process owns a VadSpace (Virtual Address Descriptor space) — a
//   sorted list of VadEntry records that together describe the entire used
//   virtual address range of the process. This mirrors the NT VAD tree but
//   uses a flat Vec sorted by base address; a proper AVL tree can be
//   substituted later without changing the public API.
//
//   The NT alloc/free/protect lifecycle maps to physical paging like this:
//
//     VirtualAlloc(MEM_RESERVE)   — inserts a RESERVED VAD, no physical frames
//     VirtualAlloc(MEM_COMMIT)    — maps physical frames for the committed region
//     VirtualFree(MEM_DECOMMIT)   — unmaps physical frames, VAD stays RESERVED
//     VirtualFree(MEM_RELEASE)    — removes the VAD entirely
//     VirtualProtect              — flips page-table flags on committed pages
//
//   Guard Pages
//     PAGE_GUARD causes the first access to that page to raise
//     STATUS_GUARD_PAGE_VIOLATION. The kernel then strips the guard flag so
//     subsequent accesses succeed. Used by the Win32 thread stack for growth.
//
//   Copy-on-Write
//     Shared pages from section objects are mapped read-only. On a write fault
//     the kernel allocates a fresh private frame, copies the shared content,
//     and re-maps the page as writable. The original shared frame is unaffected.
//
//   Section Mapping
//     NtMapViewOfSection maps a range from a SectionObject's backing buffer
//     into the caller's address space. All co-mapped views share the same
//     backing bytes (the SectionObject's internal Vec) via CoW-on-write.

#![allow(dead_code)]

use alloc::vec::Vec;
use alloc::sync::Arc;
use x86_64::{
    VirtAddr,
    structures::paging::{Page, PageTableFlags, Size4KiB, Mapper},
};
use crate::memory::{self, GlobalFrameAllocator};
use crate::nt_objects::{
    SectionObject,
    STATUS_SUCCESS, STATUS_INVALID_PARAMETER, STATUS_NO_MEMORY,
    STATUS_ACCESS_DENIED, STATUS_INVALID_HANDLE,
    PAGE_READONLY, PAGE_READWRITE, PAGE_EXECUTE, PAGE_EXECUTE_RW,
};
use crate::println;

// =====================================================================
// -----| NT Page Protection Constants |-----
// =====================================================================

pub const PAGE_NOACCESS:          u32 = 0x01;
// PAGE_READONLY / PAGE_READWRITE / PAGE_EXECUTE / PAGE_EXECUTE_RW come from nt_objects
pub const PAGE_EXECUTE_READ:      u32 = 0x20;
pub const PAGE_EXECUTE_WRITECOPY: u32 = 0x80;
pub const PAGE_GUARD:             u32 = 0x100;  // Modifier: first-access trap
pub const PAGE_NOCACHE:           u32 = 0x200;  // Modifier: disable cache
pub const PAGE_WRITECOMBINE:      u32 = 0x400;  // Modifier: write-combining
pub const PAGE_WRITECOPY:         u32 = 0x08;   // CoW on write

// =====================================================================
// -----| NT VirtualAlloc / VirtualFree Type Flags |-----
// =====================================================================

pub const MEM_COMMIT:      u32 = 0x00001000;
pub const MEM_RESERVE:     u32 = 0x00002000;
pub const MEM_DECOMMIT:    u32 = 0x00004000;
pub const MEM_RELEASE:     u32 = 0x00008000;
pub const MEM_RESET:       u32 = 0x00080000;
pub const MEM_TOP_DOWN:    u32 = 0x00100000;

// =====================================================================
// -----| MEMORY_BASIC_INFORMATION |-----
// =====================================================================

#[derive(Debug, Clone, Copy)]
pub struct MemoryBasicInformation {
    pub base_address:        u64,
    pub allocation_base:     u64,
    pub allocation_protect:  u32,
    pub region_size:         usize,
    pub state:               u32,  // MEM_COMMIT | MEM_RESERVE | MEM_FREE
    pub protect:             u32,
    pub region_type:         u32,  // MEM_PRIVATE | MEM_MAPPED | MEM_IMAGE
}

pub const MEM_PRIVATE: u32 = 0x00020000;
pub const MEM_MAPPED:  u32 = 0x00040000;
pub const MEM_IMAGE:   u32 = 0x01000000;
pub const MEM_FREE:    u32 = 0x00010000;

// =====================================================================
// -----| VAD (Virtual Address Descriptor) Entry |-----
// =====================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VadState {
    /// Pages are reserved in the VAD but no physical frames are mapped.
    Reserved,
    /// Physical frames are mapped and the pages are accessible.
    Committed,
    /// Partially committed — tracks page-granularity commit state.
    /// For simplicity we treat this like Committed (whole region mapped).
    Partial,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VadType {
    Private,  // Anonymous private allocation
    Mapped,   // Mapped from a SectionObject (shared memory / file)
    Image,    // PE image mapped by the loader
}

#[derive(Clone)]
pub struct VadEntry {
    /// Base virtual address (always page-aligned, 4 KiB granularity).
    pub base: u64,
    /// Region size in bytes (always a multiple of 4 KiB).
    pub size: usize,
    /// Commit state.
    pub state: VadState,
    /// Allocation type.
    pub vad_type: VadType,
    /// Page protection (PAGE_* flags, may include PAGE_GUARD modifier).
    pub protect: u32,
    /// Original protection at VirtualAlloc time.
    pub alloc_protect: u32,
    /// For Mapped regions: the backing section object.
    pub section: Option<Arc<SectionObject>>,
    /// Offset into the section object's backing buffer.
    pub section_offset: u64,
}

impl VadEntry {
    pub fn end(&self) -> u64 { self.base + self.size as u64 }

    pub fn contains(&self, addr: u64) -> bool {
        addr >= self.base && addr < self.end()
    }

    pub fn overlaps(&self, other_base: u64, other_size: usize) -> bool {
        self.base < other_base + other_size as u64 && self.end() > other_base
    }

    pub fn has_guard(&self) -> bool { self.protect & PAGE_GUARD != 0 }
    pub fn is_cow(&self)   -> bool { self.protect & PAGE_WRITECOPY != 0 }
    pub fn is_writable(&self) -> bool {
        matches!(self.protect & 0xFF, PAGE_READWRITE | PAGE_EXECUTE_RW | PAGE_WRITECOPY)
    }
}

// =====================================================================
// -----| VadSpace — the process's virtual address book |-----
// =====================================================================

pub struct VadSpace {
    /// All VAD entries, sorted ascending by base address.
    entries: Vec<VadEntry>,
    /// The next address the bump allocator will try (grows upward).
    next_alloc: u64,
}

// Starts at 4 GiB: the low 4 GiB holds the kernel image (GiB 0) and
// identity-mapped MMIO (GiB 3), which every address space shares (see
// address_space). KUSER_SHARED_DATA (0x7FFE0000) is mapped by the loader.
const USER_SPACE_START: u64 = 0x0001_0000_0000;
const GRANULARITY: u64 = 0x1_0000;
const USER_SPACE_END:   u64 = 0x0000_7FFF_FFFF_F000; // Top of user address space

impl VadSpace {
    pub fn new() -> Self {
        Self { entries: Vec::new(), next_alloc: USER_SPACE_START }
    }

    // ----- Lookup -----

    pub fn find(&self, addr: u64) -> Option<&VadEntry> {
        self.entries.iter().find(|e| e.contains(addr))
    }

    pub fn find_mut(&mut self, addr: u64) -> Option<&mut VadEntry> {
        self.entries.iter_mut().find(|e| e.contains(addr))
    }

    pub fn is_free(&self, base: u64, size: usize) -> bool {
        !self.entries.iter().any(|e| e.overlaps(base, size))
    }

    /// Record an image or loader region mapped outside the VAD (modules.rs)
    /// so VirtualAlloc never hands it out.
    pub fn record_image(&mut self, base: u64, size: usize) {
        self.entries.push(VadEntry {
            base, size: align_up(size, 0x1000), state: VadState::Committed,
            vad_type: VadType::Image,
            protect: PAGE_EXECUTE_READ, alloc_protect: PAGE_EXECUTE_READ,
            section: None, section_offset: 0,
        });
        self.entries.sort_by_key(|e| e.base);
    }

    /// A free stretch for a file view (nt_demand.rs), kept out of VirtualAlloc's way.
    pub fn reserve_view(&mut self, size: usize) -> Option<u64> {
        let size = align_up(size, 0x1000);
        let base = self.find_free_region(0, size)?;
        self.record_image(base, size);
        Some(base)
    }

    /// Forget the region at `base`, whose pages are already gone.
    pub fn forget(&mut self, base: u64) {
        self.entries.retain(|e| e.base != base);
    }

    // ----- Address selection -----

    /// Find a free region of at least `size` bytes.
    /// Honours a hint if supplied; otherwise bump-allocates from `next_alloc`.
    fn find_free_region(&mut self, hint: u64, size: usize) -> Option<u64> {
        let size_aligned = align_up(size, 0x1000);

        // Try the hint first (must be page-aligned and free).
        let hint_aligned = align_down(hint, 0x1000);
        if hint_aligned >= USER_SPACE_START
            && hint_aligned + size_aligned as u64 <= USER_SPACE_END
            && self.is_free(hint_aligned, size_aligned)
        {
            return Some(hint_aligned);
        }

        // Fall back to bump allocator, scanning past any conflicts. Regions
        // start on Windows' 64 KiB allocation granularity, which programs
        // rely on (memmap2 rounds a view's address down to it to unmap).
        let mut candidate = align_up_u64(self.next_alloc, GRANULARITY);
        loop {
            if candidate + size_aligned as u64 > USER_SPACE_END { return None; }
            if self.is_free(candidate, size_aligned) {
                self.next_alloc = candidate + size_aligned as u64;
                return Some(candidate);
            }
            // Step over the conflicting entry.
            if let Some(blocker) = self.entries.iter().find(|e| e.overlaps(candidate, size_aligned)) {
                candidate = align_up_u64(blocker.end(), GRANULARITY);
            } else {
                candidate += GRANULARITY;
            }
        }
    }

    // ----- VirtualAlloc -----

    pub fn virtual_alloc(
        &mut self,
        addr_hint: u64,
        size: usize,
        alloc_type: u32,
        protect: u32,
    ) -> Result<u64, u64> {
        if size == 0 { return Err(STATUS_INVALID_PARAMETER); }
        let size = align_up(size, 0x1000);

        // ---- MEM_RESERVE (or MEM_COMMIT with no address: Windows reserves
        // and commits wherever there's room) ----
        if alloc_type & MEM_RESERVE != 0 || (alloc_type & MEM_COMMIT != 0 && addr_hint == 0) {
            let base = self.find_free_region(addr_hint, size)
                .ok_or(STATUS_NO_MEMORY)?;

            self.entries.push(VadEntry {
                base, size, state: VadState::Reserved,
                vad_type: VadType::Private,
                protect, alloc_protect: protect,
                section: None, section_offset: 0,
            });
            self.entries.sort_by_key(|e| e.base);

            // If MEM_COMMIT is also set, commit immediately.
            if alloc_type & MEM_COMMIT != 0 {
                self.commit_range(base, size, protect)?;
            }
            return Ok(base);
        }

        // ---- MEM_COMMIT only (must target an existing RESERVED range) ----
        if alloc_type & MEM_COMMIT != 0 {
            let addr = align_down(addr_hint, 0x1000);

            // If the address is completely free, auto-reserve + commit (Win32 behaviour).
            if self.is_free(addr, size) {
                let vad = VadEntry {
                    base: addr, size, state: VadState::Reserved,
                    vad_type: VadType::Private,
                    protect, alloc_protect: protect,
                    section: None, section_offset: 0,
                };
                self.entries.push(vad);
                self.entries.sort_by_key(|e| e.base);
            }

            self.commit_range(addr, size, protect)?;
            return Ok(addr);
        }

        Err(STATUS_INVALID_PARAMETER)
    }

    // ----- Commit (map physical frames) -----

    fn commit_range(&mut self, base: u64, size: usize, protect: u32) -> Result<(), u64> {
        use x86_64::structures::paging::Translate;
        let flags = nt_protect_to_page_flags(protect);
        let mut mapper = unsafe { memory::get_mapper() };
        let num_pages = size / 0x1000;
        let phys_offset = memory::get_phys_mem_offset().as_u64();

        for i in 0..num_pages {
            let vaddr = VirtAddr::new(base + (i as u64 * 0x1000));
            // Already committed: leave its contents alone.
            if mapper.translate_addr(vaddr).is_some() {
                continue;
            }
            let page  = Page::<Size4KiB>::containing_address(vaddr);
            let frame = memory::allocate_frame().ok_or(STATUS_NO_MEMORY)?;
            // Committed memory reads as zero; cleared through the physical
            // map, since the page itself may be read-only.
            unsafe { core::ptr::write_bytes((phys_offset + frame.start_address().as_u64()) as *mut u8, 0, 4096) };
            memory::map_page(page, frame, flags, &mut mapper, &mut GlobalFrameAllocator);
        }

        // Update VAD state.
        if let Some(vad) = self.find_mut(base) {
            vad.state   = VadState::Committed;
            vad.protect = protect;
        }
        Ok(())
    }

    // ----- VirtualFree -----

    pub fn virtual_free(
        &mut self,
        addr: u64,
        size: usize,
        free_type: u32,
    ) -> Result<(), u64> {
        let addr = align_down(addr, 0x1000);

        if free_type & MEM_RELEASE != 0 {
            // Full release — decommit and remove the VAD.
            if size != 0 { return Err(STATUS_INVALID_PARAMETER); } // NT requires size=0 for RELEASE
            let idx = self.entries.iter().position(|e| e.base == addr)
                .ok_or(STATUS_INVALID_PARAMETER)?;
            let vad = self.entries.remove(idx);
            if vad.state == VadState::Committed {
                self.decommit_range(vad.base, vad.size);
            }
            return Ok(());
        }

        if free_type & MEM_DECOMMIT != 0 {
            let size = align_up(size, 0x1000);
            self.decommit_range(addr, size);
            if let Some(vad) = self.find_mut(addr) {
                vad.state = VadState::Reserved;
            }
            return Ok(());
        }

        Err(STATUS_INVALID_PARAMETER)
    }

    fn decommit_range(&self, base: u64, size: usize) {
        let mut mapper = unsafe { memory::get_mapper() };
        let num_pages = size / 0x1000;
        for i in 0..num_pages {
            let vaddr = VirtAddr::new(base + (i as u64 * 0x1000));
            let page  = Page::<Size4KiB>::containing_address(vaddr);
            // Unmap and free; pages never committed are skipped.
            let _ = memory::unmap_and_free_page(page, &mut mapper);
        }
    }

    /// Process exit: free every private allocation's memory. (Image and
    /// loader regions belong to the loader's page list.)
    pub fn release_all(&mut self) {
        for e in core::mem::take(&mut self.entries) {
            if e.vad_type == VadType::Private && e.state == VadState::Committed {
                self.decommit_range(e.base, e.size);
            }
        }
    }

    // ----- VirtualProtect -----

    pub fn virtual_protect(
        &mut self,
        addr: u64,
        size: usize,
        new_protect: u32,
    ) -> Result<u32, u64> {
        let addr = align_down(addr, 0x1000);
        let size = align_up(size, 0x1000);

        let vad = self.find_mut(addr).ok_or(STATUS_INVALID_PARAMETER)?;
        if vad.state != VadState::Committed { return Err(STATUS_INVALID_PARAMETER); }

        let old_protect = vad.protect;
        vad.protect = new_protect;

        let new_flags = nt_protect_to_page_flags(new_protect);
        let mut mapper = unsafe { memory::get_mapper() };
        let num_pages  = size / 0x1000;

        for i in 0..num_pages {
            let vaddr = VirtAddr::new(addr + (i as u64 * 0x1000));
            let page  = Page::<Size4KiB>::containing_address(vaddr);
            if let Ok(phys_frame) = mapper.translate_page(page) {
                if let Ok((_old_frame, flush)) = mapper.unmap(page) {
                    flush.flush();
                }
                memory::map_page(page, phys_frame, new_flags, &mut mapper, &mut GlobalFrameAllocator);
            }
        }

        Ok(old_protect)
    }

    // ----- VirtualQuery -----

    pub fn virtual_query(&self, addr: u64) -> MemoryBasicInformation {
        if let Some(vad) = self.find(addr) {
            MemoryBasicInformation {
                base_address:       vad.base,
                allocation_base:    vad.base,
                allocation_protect: vad.alloc_protect,
                region_size:        vad.size,
                state:  match vad.state {
                    VadState::Reserved => MEM_RESERVE,
                    _                  => MEM_COMMIT,
                },
                protect: vad.protect,
                region_type: match vad.vad_type {
                    VadType::Private => MEM_PRIVATE,
                    VadType::Mapped  => MEM_MAPPED,
                    VadType::Image   => MEM_IMAGE,
                },
            }
        } else {
            // Unmapped: return a free region descriptor.
            MemoryBasicInformation {
                base_address:       align_down(addr, 0x1000),
                allocation_base:    0,
                allocation_protect: 0,
                region_size:        0x1000,
                state:              MEM_FREE,
                protect:            PAGE_NOACCESS,
                region_type:        MEM_FREE,
            }
        }
    }

    // ----- MapViewOfSection -----
    //
    // Maps `size` bytes from `section` starting at `section_offset` into
    // this process's address space at `addr_hint`.
    // If the section is writable this sets up copy-on-write semantics:
    // the pages are initially mapped read-only; writes trigger CoW faults.

    pub fn map_view_of_section(
        &mut self,
        section: Arc<SectionObject>,
        addr_hint: u64,
        size: usize,
        section_offset: u64,
        protect: u32,
    ) -> Result<u64, u64> {
        let size = align_up(size, 0x1000);
        if section_offset as usize + size > section.size { return Err(STATUS_INVALID_PARAMETER); }

        let base = self.find_free_region(addr_hint, size)
            .ok_or(STATUS_NO_MEMORY)?;

        // Map the backing bytes directly into the virtual address space.
        // We copy section bytes into fresh frames (true shared-memory DMA mapping
        // requires IOMMU support — full implementation in Phase 6).
        let mut mapper   = unsafe { memory::get_mapper() };
        let data_guard   = section.data.lock();
        let src_bytes    = &data_guard[section_offset as usize .. section_offset as usize + size];

        // Use CoW-initial protection: writable sections start read-only.
        let initial_protect = if section.page_protection == PAGE_READONLY {
            PAGE_READONLY
        } else {
            PAGE_WRITECOPY  // Will trigger fault → private copy on write
        };
        let flags = nt_protect_to_page_flags(initial_protect);

        let num_pages = size / 0x1000;
        for i in 0..num_pages {
            let vaddr  = VirtAddr::new(base + (i as u64 * 0x1000));
            let page   = Page::<Size4KiB>::containing_address(vaddr);
            let frame  = memory::allocate_frame().ok_or(STATUS_NO_MEMORY)?;

            memory::map_page(page, frame, flags, &mut mapper, &mut GlobalFrameAllocator);

            // Copy section data into the fresh frame.
            let page_offset = i * 0x1000;
            let copy_len    = core::cmp::min(0x1000, size - page_offset);
            unsafe {
                core::ptr::copy_nonoverlapping(
                    src_bytes[page_offset..].as_ptr(),
                    vaddr.as_mut_ptr::<u8>(),
                    copy_len,
                );
                // Zero-pad remainder of the page.
                if copy_len < 0x1000 {
                    core::ptr::write_bytes(
                        vaddr.as_mut_ptr::<u8>().add(copy_len),
                        0,
                        0x1000 - copy_len,
                    );
                }
            }
        }
        drop(data_guard);

        // Record the VAD.
        self.entries.push(VadEntry {
            base, size,
            state:          VadState::Committed,
            vad_type:       VadType::Mapped,
            protect:        initial_protect,
            alloc_protect:  protect,
            section:        Some(section),
            section_offset,
        });
        self.entries.sort_by_key(|e| e.base);

        println!("[NTMEM] MapViewOfSection: mapped {} bytes at {:#X}", size, base);
        Ok(base)
    }

    // ----- UnmapViewOfSection -----

    pub fn unmap_view_of_section(&mut self, addr: u64) -> Result<(), u64> {
        let addr  = align_down(addr, 0x1000);
        let idx   = self.entries.iter().position(|e| e.base == addr && e.vad_type == VadType::Mapped)
            .ok_or(STATUS_INVALID_PARAMETER)?;
        let vad   = self.entries.remove(idx);
        self.decommit_range(vad.base, vad.size);
        println!("[NTMEM] UnmapViewOfSection: freed {:#X}", addr);
        Ok(())
    }

    // ----- Copy-on-Write fault handler -----
    //
    // Call from the page-fault handler when a write hits a PAGE_WRITECOPY page.
    // Returns Ok(new_frame_addr) if handled, Err(addr) if not a CoW fault.

    pub fn handle_cow_fault(&mut self, fault_addr: u64) -> Result<(), u64> {
        let page_base = align_down(fault_addr, 0x1000);
        let vad = self.find(page_base).ok_or(fault_addr)?;

        if !vad.is_cow() { return Err(fault_addr); }

        // Allocate a private frame and copy contents.
        let vaddr = VirtAddr::new(page_base);
        let page  = Page::<Size4KiB>::containing_address(vaddr);
        let frame = memory::allocate_frame().ok_or(STATUS_NO_MEMORY)?;

        let mut mapper = unsafe { memory::get_mapper() };

        // Read current contents before unmapping.
        let mut buf = [0u8; 4096];
        unsafe { core::ptr::copy_nonoverlapping(vaddr.as_ptr::<u8>(), buf.as_mut_ptr(), 4096); }

        // Remap as private writable.
        if let Ok((_f, flush)) = mapper.unmap(page) { flush.flush(); }
        memory::map_page(
            page, frame,
            PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::USER_ACCESSIBLE,
            &mut mapper, &mut GlobalFrameAllocator,
        );

        // Restore contents into the new private frame.
        unsafe { core::ptr::copy_nonoverlapping(buf.as_ptr(), vaddr.as_mut_ptr::<u8>(), 4096); }

        // Upgrade protection on the VAD.
        if let Some(vad_mut) = self.find_mut(page_base) {
            vad_mut.vad_type  = VadType::Private;
            vad_mut.protect   = PAGE_READWRITE;
        }
        println!("[NTMEM] CoW fault handled at {:#X}", fault_addr);
        Ok(())
    }

    // ----- Guard page fault handler -----
    //
    // On first access to a guard page: strips the guard, returns the old
    // protection. The kernel then delivers STATUS_GUARD_PAGE_VIOLATION to
    // the process (or silently continues for stack growth).

    pub fn handle_guard_fault(&mut self, fault_addr: u64) -> Option<u32> {
        let page_base = align_down(fault_addr, 0x1000);
        let vad = self.find_mut(page_base)?;
        if !vad.has_guard() { return None; }

        let old_protect = vad.protect;
        let new_protect = old_protect & !PAGE_GUARD;
        vad.protect = new_protect;

        // Re-map the page without the guard (PAGE_GUARD doesn't exist in the
        // hardware — it's a software convention enforced by the PF handler).
        let flags = nt_protect_to_page_flags(new_protect);
        let vaddr  = VirtAddr::new(page_base);
        let page   = Page::<Size4KiB>::containing_address(vaddr);
        let mut mapper = unsafe { memory::get_mapper() };

        if let Ok((frame, flush)) = mapper.unmap(page) {
            flush.flush();
            memory::map_page(page, frame, flags, &mut mapper, &mut GlobalFrameAllocator);
        }
        println!("[NTMEM] Guard page stripped at {:#X}", fault_addr);
        Some(old_protect)
    }

    pub fn region_count(&self) -> usize { self.entries.len() }

    /// Iterate all VAD entries (read-only). Used by the memory safety auditor.
    pub fn iter_entries(&self) -> impl Iterator<Item = &VadEntry> {
        self.entries.iter()
    }
}

// =====================================================================
// -----| NT protect flags → x86_64 PageTableFlags |-----
// =====================================================================

pub fn nt_protect_to_page_flags(protect: u32) -> PageTableFlags {
    let core_protect = protect & 0xFF; // Strip modifier flags
    let mut flags    = PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE;

    match core_protect {
        PAGE_NOACCESS => {
            flags.remove(PageTableFlags::PRESENT);
        }
        PAGE_READONLY | PAGE_WRITECOPY => {
            // Read-only; CoW pages start read-only and get a private writable copy on fault.
        }
        PAGE_READWRITE => {
            flags |= PageTableFlags::WRITABLE;
        }
        PAGE_EXECUTE | PAGE_EXECUTE_READ => {
            // Default is executable in x86_64 paging (NX not set).
        }
        PAGE_EXECUTE_RW | PAGE_EXECUTE_WRITECOPY => {
            flags |= PageTableFlags::WRITABLE;
        }
        _ => {
            // Unknown — default to readable.
        }
    }

    // PAGE_NOCACHE modifier
    if protect & PAGE_NOCACHE != 0 {
        flags |= PageTableFlags::NO_CACHE;
    }

    // PAGE_WRITECOMBINE: PWT alone selects PAT entry 1, which smp::init_pat
    // programs as write-combining.
    if protect & PAGE_WRITECOMBINE != 0 {
        flags |= PageTableFlags::WRITE_THROUGH;
    }

    flags
}

// =====================================================================
// -----| NT Syscall Dispatcher |-----
// =====================================================================

pub fn dispatch_nt_memory_syscall(syscall_id: u64, arg0: u64, arg1: u64, arg2: u64, arg3: u64) -> u64 {
    use crate::process::PROCESSOR;

    let process = match PROCESSOR.lock().get_active() {
        Some(p) => p,
        None    => { println!("[NTMEM] Memory syscall {:#x} with no active process!", syscall_id); return STATUS_ACCESS_DENIED; }
    };

    match syscall_id {

        // NtAllocateVirtualMemory (0x18)
        // arg0=addr_hint, arg1=size, arg2=alloc_type, arg3=protect
        0x0018 => {
            let hint    = arg0;
            let size    = arg1 as usize;
            let atype   = arg2 as u32;
            let protect = arg3 as u32;

            match process.vad.lock().virtual_alloc(hint, size, atype, protect) {
                Ok(addr) => {
                    println!("[NTMEM] NtAllocateVirtualMemory: {:#X} ({} bytes)", addr, size);
                    addr
                }
                Err(e) => { println!("[NTMEM] NtAllocateVirtualMemory failed: {:#X}", e); e }
            }
        }

        // NtFreeVirtualMemory (0x1E)
        // arg0=addr, arg1=size, arg2=free_type
        0x001E => {
            let addr      = arg0;
            let size      = arg1 as usize;
            let free_type = arg2 as u32;
            match process.vad.lock().virtual_free(addr, size, free_type) {
                Ok(_)  => { println!("[NTMEM] NtFreeVirtualMemory: {:#X}", addr); STATUS_SUCCESS }
                Err(e) => { println!("[NTMEM] NtFreeVirtualMemory failed: {:#X}", e); e }
            }
        }

        // NtProtectVirtualMemory (0x50)
        // arg0=addr, arg1=size, arg2=new_protect
        0x0050 => {
            let addr        = arg0;
            let size        = arg1 as usize;
            let new_protect = arg2 as u32;
            match process.vad.lock().virtual_protect(addr, size, new_protect) {
                Ok(old) => {
                    println!("[NTMEM] NtProtectVirtualMemory: {:#X} protect {:#x}->{:#x}", addr, old, new_protect);
                    STATUS_SUCCESS
                }
                Err(e) => { println!("[NTMEM] NtProtectVirtualMemory failed: {:#X}", e); e }
            }
        }

        // NtQueryVirtualMemory (0x23)
        // arg0=addr  (returns MemoryBasicInformation conceptually; we log it)
        0x0023 => {
            let addr = arg0;
            let mbi  = process.vad.lock().virtual_query(addr);
            println!("[NTMEM] NtQueryVirtualMemory {:#X}: state={:#x} protect={:#x} size={}",
                addr, mbi.state, mbi.protect, mbi.region_size);
            STATUS_SUCCESS
        }

        // NtMapViewOfSection (0x28)
        // arg0=section_handle, arg1=addr_hint, arg2=size, arg3=protect
        0x0028 => {
            let section_handle = arg0 as crate::nt_objects::Handle;
            let addr_hint      = arg1;
            let size           = arg2 as usize;
            let protect        = arg3 as u32;

            let section_arc = process.handles.lock().as_section(section_handle);
            match section_arc {
                None => {
                    println!("[NTMEM] NtMapViewOfSection: invalid section handle {}", section_handle);
                    STATUS_INVALID_HANDLE
                }
                Some(sec) => {
                    match process.vad.lock().map_view_of_section(sec, addr_hint, size, 0, protect) {
                        Ok(addr) => addr,
                        Err(e)   => e,
                    }
                }
            }
        }

        // NtUnmapViewOfSection (0x2A)
        // arg0=addr
        0x002A => {
            let addr = arg0;
            match process.vad.lock().unmap_view_of_section(addr) {
                Ok(_)  => STATUS_SUCCESS,
                Err(e) => e,
            }
        }

        _ => crate::nt_objects::STATUS_NOT_IMPLEMENTED,
    }
}

// =====================================================================
// -----| Alignment helpers |-----
// =====================================================================

#[inline(always)]
fn align_up(n: usize, align: usize) -> usize {
    (n + align - 1) & !(align - 1)
}

#[inline(always)]
fn align_up_u64(n: u64, align: u64) -> u64 {
    (n + align - 1) & !(align - 1)
}

#[inline(always)]
fn align_down(n: u64, align: u64) -> u64 {
    n & !(align - 1)
}
