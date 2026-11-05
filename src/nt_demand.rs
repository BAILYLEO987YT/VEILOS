// =============================================================================
// VEILOS: pages from files, on demand (WIN32_DESIGN.md, S1)
// =============================================================================
//
// A program image, a read-only file view or a thread's stack is a `Region` of an
// address space: nothing is read or mapped until a page is first touched. A fault (in ring 3,
// or the kernel touching user memory) reads that page's bytes from the file,
// applies its base relocations if the image moved, and maps it. So a 338 MB
// program costs what it uses, and no file passes through the kernel heap.
//
// Kernel code holding the disk's lock must not fault here, so anything that
// hands a user buffer to the disk has been through `user_ptr_ok`, which brings
// in the pages it checks.
//
// Pages are private to each process, even read-only ones: nothing shares them
// yet, so N processes running one program read it N times.

use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};
use spin::Mutex;
use x86_64::structures::paging::{Page, PageTableFlags, Size4KiB, Translate};
use x86_64::VirtAddr;

use crate::memory::{self, GlobalFrameAllocator};
use crate::nt_file::{ImageHold, SharedFile};

const PAGE: u64 = 4096;
const SCN_CNT_CODE: u32 = 0x20;
const SCN_MEM_EXECUTE: u32 = 0x2000_0000;
const SCN_MEM_WRITE: u32 = 0x8000_0000;
const USER: PageTableFlags = PageTableFlags::PRESENT.union(PageTableFlags::USER_ACCESSIBLE);

/// A section of an image.
#[derive(Clone, Copy)]
pub struct Sec {
    pub va: u32,
    pub vsize: u32,
    pub raw_ptr: u32,
    pub raw_size: u32,
    pub chars: u32,
}

enum Backing {
    Image {
        hold: ImageHold,
        headers: u32,
        sections: Vec<Sec>,
        /// How far the image moved from its preferred base.
        delta: u64,
        /// Per page RVA: where its relocation entries are in the file, and
        /// how many. Empty if the image didn't move.
        relocs: BTreeMap<u32, (u64, u32)>,
        /// Linked: pages take their sections' protection, not read-write.
        linked: AtomicBool,
    },
    View {
        file: Arc<SharedFile>,
        offset: u64,
        flags: PageTableFlags,
    },
    /// Pages of zeros.
    Zero,
}

struct Region {
    base: u64,
    size: u64,
    backing: Backing,
}

#[derive(Default)]
struct Space {
    regions: Mutex<Vec<Arc<Region>>>,
    /// Pages brought in, to free with the address space.
    touched: Mutex<Vec<u64>>,
}

/// By page-table root.
static SPACES: Mutex<BTreeMap<u64, Arc<Space>>> = Mutex::new(BTreeMap::new());

fn space(cr3: u64) -> Arc<Space> {
    SPACES.lock().entry(cr3).or_default().clone()
}

fn current() -> Option<Arc<Space>> {
    SPACES.lock().get(&crate::address_space::active()).cloned()
}

/// Add an image at `base` to the active address space.
pub fn add_image(base: u64, size: u64, file: Arc<SharedFile>, headers: u32, sections: Vec<Sec>, delta: u64, reloc_dir: Option<(u32, u32)>) {
    let relocs = if delta != 0 { index_relocs(&file, &sections, reloc_dir) } else { BTreeMap::new() };
    let hold = ImageHold::new(file);
    let backing = Backing::Image { hold, headers, sections, delta, relocs, linked: AtomicBool::new(false) };
    let r = Region { base, size, backing };
    space(crate::address_space::active()).regions.lock().push(Arc::new(r));
}

/// All images linked: from now on their pages take their real protection.
pub fn images_linked() {
    let Some(s) = current() else { return };
    for r in s.regions.lock().iter() {
        if let Backing::Image { linked, .. } = &r.backing {
            linked.store(true, Ordering::Release);
        }
    }
}

/// A view of `file` from `offset` at `base`; what's past its end reads as zero.
pub fn add_view(base: u64, size: u64, file: Arc<SharedFile>, offset: u64, writable: bool, executable: bool) {
    let mut flags = USER;
    if writable {
        flags |= PageTableFlags::WRITABLE;
    }
    if !executable {
        flags |= PageTableFlags::NO_EXECUTE;
    }
    let r = Region { base, size, backing: Backing::View { file, offset, flags } };
    space(crate::address_space::active()).regions.lock().push(Arc::new(r));
}

/// Zero pages at `base`, as they're touched.
pub fn add_zero(base: u64, size: u64) {
    let r = Region { base, size, backing: Backing::Zero };
    space(crate::address_space::active()).regions.lock().push(Arc::new(r));
}

/// Take away the view holding `addr`, and free its pages. Its (base, size).
pub fn remove_view(addr: u64) -> Option<(u64, u64)> {
    let s = current()?;
    let r = {
        let mut regions = s.regions.lock();
        let at = regions.iter().position(|r| matches!(r.backing, Backing::View { .. }) && addr >= r.base && addr < r.base + r.size)?;
        regions.remove(at)
    };
    let mut mapper = unsafe { memory::get_mapper() };
    s.touched.lock().retain(|&p| {
        let inside = p >= r.base && p < r.base + r.size;
        if inside {
            let _ = memory::unmap_and_free_page(Page::containing_address(VirtAddr::new(p)), &mut mapper);
        }
        !inside
    });
    Some((r.base, r.size))
}

/// The active address space is going: free every page brought in.
pub fn release_active() {
    let Some(s) = SPACES.lock().remove(&crate::address_space::active()) else { return };
    let mut mapper = unsafe { memory::get_mapper() };
    for &p in s.touched.lock().iter() {
        let _ = memory::unmap_and_free_page(Page::containing_address(VirtAddr::new(p)), &mut mapper);
    }
}

/// Bring in every page of a range that has a region behind it.
pub fn fault_range(base: u64, size: u64) {
    let mut p = base & !(PAGE - 1);
    let end = base.saturating_add(size);
    while p < end {
        fault_in(p);
        p += PAGE;
    }
}

/// Bring in the page holding `addr`: true if it's now mapped by this call or
/// another thread's, false if no region has it (so the fault is a real one).
pub fn fault_in(addr: u64) -> bool {
    let Some(s) = current() else { return false };
    let page = addr & !(PAGE - 1);
    let Some(r) = s.regions.lock().iter().find(|r| page >= r.base && page < r.base + r.size).cloned() else { return false };
    let mut mapper = unsafe { memory::get_mapper() };
    if mapper.translate_addr(VirtAddr::new(page)).is_some() {
        return true; // Another thread got here first; callers only ask about pages that weren't there.
    }
    let Some(frame) = memory::allocate_frame() else {
        crate::serial_println!("[DEMAND] out of memory bringing in {:#x}", page);
        return false;
    };
    let phys = memory::get_phys_mem_offset().as_u64() + frame.start_address().as_u64();
    let bytes = unsafe { core::slice::from_raw_parts_mut(phys as *mut u8, PAGE as usize) };
    bytes.fill(0);
    let flags = fill(&r, page, bytes);
    // Another thread may have got here while the disk was read.
    let mut touched = s.touched.lock();
    if mapper.translate_addr(VirtAddr::new(page)).is_some() {
        memory::deallocate_frame(frame);
        return true;
    }
    if memory::map_page_checked(Page::<Size4KiB>::containing_address(VirtAddr::new(page)), frame, flags, &mut mapper, &mut GlobalFrameAllocator).is_err() {
        memory::deallocate_frame(frame);
        return false;
    }
    touched.push(page);
    true
}

/// The contents of `page` into `bytes`; the protection it should have.
fn fill(r: &Region, page: u64, bytes: &mut [u8]) -> PageTableFlags {
    match &r.backing {
        Backing::Zero => USER | PageTableFlags::WRITABLE | PageTableFlags::NO_EXECUTE,
        Backing::View { file, offset, flags } => {
            let _ = file.read_at(offset + (page - r.base), bytes);
            *flags
        }
        Backing::Image { hold, headers, sections, delta, relocs, linked } => {
            let file = hold.file();
            let rva = (page - r.base) as u32;
            if rva < *headers {
                let n = (*headers - rva).min(PAGE as u32) as usize;
                let _ = file.read_at(rva as u64, &mut bytes[..n]);
            }
            let (mut w, mut x) = (false, false);
            for s in sections {
                let span = s.vsize.max(s.raw_size);
                let (lo, hi) = (s.va, s.va.saturating_add(span));
                if rva + PAGE as u32 <= lo || rva >= (hi as u64).next_multiple_of(PAGE) as u32 {
                    continue;
                }
                w |= s.chars & SCN_MEM_WRITE != 0;
                x |= s.chars & (SCN_MEM_EXECUTE | SCN_CNT_CODE) != 0;
                // Only what's in the file; the rest stays zero.
                let raw = if s.vsize != 0 { s.raw_size.min(s.vsize) } else { s.raw_size };
                let start = lo.max(rva);
                let end = (lo + raw).min(rva + PAGE as u32);
                if start < end {
                    let at = (start - rva) as usize;
                    let from = s.raw_ptr as u64 + (start - lo) as u64;
                    let _ = file.read_at(from, &mut bytes[at..at + (end - start) as usize]);
                }
            }
            if *delta != 0 {
                apply_relocs(file, sections, relocs, *delta, rva, bytes);
            }
            if !linked.load(Ordering::Acquire) {
                return USER | PageTableFlags::WRITABLE;
            }
            let mut flags = USER;
            if w {
                flags |= PageTableFlags::WRITABLE;
            }
            if !x {
                flags |= PageTableFlags::NO_EXECUTE;
            }
            flags
        }
    }
}

fn rva_to_file(sections: &[Sec], rva: u32) -> Option<u64> {
    sections.iter().find(|s| rva >= s.va && rva < s.va + s.raw_size).map(|s| s.raw_ptr as u64 + (rva - s.va) as u64)
}

/// Walk the relocation table once for where each page's entries are.
fn index_relocs(file: &SharedFile, sections: &[Sec], dir: Option<(u32, u32)>) -> BTreeMap<u32, (u64, u32)> {
    let mut index = BTreeMap::new();
    let Some((rva, size)) = dir else { return index };
    let Some(start) = rva_to_file(sections, rva) else { return index };
    let mut off = 0u64;
    while off + 8 <= size as u64 {
        let mut h = [0u8; 8];
        if !matches!(file.read_at(start + off, &mut h), Ok(8)) {
            break;
        }
        let page = u32::from_le_bytes(h[0..4].try_into().unwrap());
        let block = u32::from_le_bytes(h[4..8].try_into().unwrap());
        if block < 8 {
            break;
        }
        index.insert(page, (start + off + 8, (block - 8) / 2));
        off += block as u64;
    }
    index
}

fn apply_relocs(file: &SharedFile, sections: &[Sec], relocs: &BTreeMap<u32, (u64, u32)>, delta: u64, rva: u32, bytes: &mut [u8]) {
    const HIGHLOW: u16 = 3;
    const DIR64: u16 = 10;
    if let Some(&(at, n)) = relocs.get(&rva) {
        for e in entries(file, at, n) {
            let off = (e & 0xFFF) as usize;
            match e >> 12 {
                DIR64 if off + 8 <= bytes.len() => {
                    let v = u64::from_le_bytes(bytes[off..off + 8].try_into().unwrap()).wrapping_add(delta);
                    bytes[off..off + 8].copy_from_slice(&v.to_le_bytes());
                }
                // Starts here and ends on the next page: this page's part.
                DIR64 => {
                    if let Some(v) = moved_value(file, sections, rva + off as u32, delta) {
                        let n = bytes.len() - off;
                        bytes[off..].copy_from_slice(&v.to_le_bytes()[..n]);
                    }
                }
                HIGHLOW if off + 4 <= bytes.len() => {
                    let v = u32::from_le_bytes(bytes[off..off + 4].try_into().unwrap()).wrapping_add(delta as u32);
                    bytes[off..off + 4].copy_from_slice(&v.to_le_bytes());
                }
                _ => {}
            }
        }
    }
    // Started on the page before and ends on this one: this page's part.
    if rva >= PAGE as u32 {
        if let Some(&(at, n)) = relocs.get(&(rva - PAGE as u32)) {
            for e in entries(file, at, n) {
                let off = (e & 0xFFF) as usize;
                if e >> 12 == DIR64 && off + 8 > PAGE as usize {
                    if let Some(v) = moved_value(file, sections, rva - PAGE as u32 + off as u32, delta) {
                        let n = off + 8 - PAGE as usize;
                        bytes[..n].copy_from_slice(&v.to_le_bytes()[8 - n..]);
                    }
                }
            }
        }
    }
}

/// A value that crosses a page, read whole from the file and moved.
fn moved_value(file: &SharedFile, sections: &[Sec], rva: u32, delta: u64) -> Option<u64> {
    let mut b = [0u8; 8];
    let from = rva_to_file(sections, rva)?;
    matches!(file.read_at(from, &mut b), Ok(8)).then(|| u64::from_le_bytes(b).wrapping_add(delta))
}

fn entries(file: &SharedFile, at: u64, n: u32) -> Vec<u16> {
    let mut raw = alloc::vec![0u8; n as usize * 2];
    let _ = file.read_at(at, &mut raw);
    raw.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect()
}
