// =============================================================================
// VEILOS: section objects, and writing another process's memory (S6)
// =============================================================================
//
// NtCreateSection makes a SectionObject (nt_objects.rs): pages that belong to
// the section, from zeros or a file. NtMapViewOfSection adds a region to an
// address space whose faults map the section's own pages (or, copy-on-write,
// a private copy), so views in different processes share memory, as the
// MSYS2/Cygwin runtime's shared regions need. A section over a file reads the
// file once per page; writes to it aren't carried back.
//
// NtWriteVirtualMemory, for a process starting a child (Cygwin's fork copies
// its memory into the child this way), writes through the physical map, so a
// read-only page takes the write as Windows' does.

use alloc::sync::Arc;
use nt_abi::{status, NtStatus};
use x86_64::structures::paging::{PageTableFlags, Translate};
use x86_64::VirtAddr;

use crate::interrupts::syscall_stack_arg as stack;
use crate::nt_file::ptr_ok;
use crate::nt_objects::{KernelObjectRef, SectionObject};
use crate::process::Process;

const SEC_IMAGE: u32 = 0x0100_0000;
const STATUS_INVALID_VIEW_SIZE: NtStatus = 0xC000_001Fu32 as NtStatus;
const STATUS_NOT_MAPPED_VIEW: NtStatus = 0xC000_0019u32 as NtStatus;
const STATUS_MAPPED_ALIGNMENT: NtStatus = 0xC000_0220u32 as NtStatus;
const STATUS_NOT_SUPPORTED: NtStatus = 0xC000_00BBu32 as NtStatus;
const STATUS_PARTIAL_COPY: NtStatus = 0x8000_000Du32 as NtStatus;

fn read_u64(p: u64) -> Option<u64> {
    ptr_ok(p, 8).then(|| unsafe { core::ptr::read_unaligned(p as *const u64) })
}

fn process_of(proc: &Arc<Process>, h: u64) -> Option<Arc<Process>> {
    match crate::nt_process::object_or_self(proc, h)? {
        KernelObjectRef::Process(p) => Some(p),
        _ => None,
    }
}

/// NtCreateSection(out*, access, attrs*, max_size*, protect, alloc_attrs, file).
pub fn create_section(proc: &Arc<Process>, out: u64, access: u64, attrs: u64, max_size: u64) -> NtStatus {
    let protect = stack(4).unwrap_or(0) as u32;
    let alloc_attrs = stack(5).unwrap_or(0) as u32;
    let file = stack(6).unwrap_or(0);
    if alloc_attrs & SEC_IMAGE != 0 {
        return STATUS_NOT_SUPPORTED; // images are mapped by the loader
    }
    let shared = if file != 0 {
        match crate::nt_process::object(proc, file) {
            Some(KernelObjectRef::File(f)) => match f.shared() {
                Some(s) => Some(s),
                None => return status::INVALID_PARAMETER,
            },
            Some(_) => return status::OBJECT_TYPE_MISMATCH,
            None => return status::INVALID_HANDLE,
        }
    } else {
        None
    };
    let asked = if max_size != 0 { read_u64(max_size).unwrap_or(0) } else { 0 };
    let size = if asked != 0 { asked } else { shared.as_ref().map_or(0, |f| f.len()) };
    if size == 0 {
        return if shared.is_some() { 0xC000_011Eu32 as NtStatus } else { status::INVALID_PARAMETER }; // STATUS_MAPPED_FILE_SIZE_ZERO
    }
    crate::nt_ipc::create_named(proc, out, access, attrs, crate::nt_ipc::KIND_SECTION, move || {
        KernelObjectRef::Section(Arc::new(SectionObject::new(size, protect, shared)))
    })
}

/// Page flags for a view's protection, and whether it's copy-on-write.
fn view_flags(protect: u32) -> (PageTableFlags, bool) {
    let user = PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE;
    let w = PageTableFlags::WRITABLE;
    let nx = PageTableFlags::NO_EXECUTE;
    match protect & 0xFF {
        0x04 => (user | w | nx, false),       // PAGE_READWRITE
        0x08 => (user | w | nx, true),        // PAGE_WRITECOPY
        0x10 | 0x20 => (user, false),         // PAGE_EXECUTE(_READ)
        0x40 => (user | w, false),            // PAGE_EXECUTE_READWRITE
        0x80 => (user | w, true),             // PAGE_EXECUTE_WRITECOPY
        _ => (user | nx, false),              // PAGE_READONLY, PAGE_NOACCESS
    }
}

/// NtMapViewOfSection(section, process, base*, zero_bits, commit_size,
/// offset*, view_size*, inherit, alloc_type, protect).
pub fn map_view(proc: &Arc<Process>, section: u64, process: u64, base_ptr: u64) -> NtStatus {
    let offset_ptr = stack(5).unwrap_or(0);
    let size_ptr = stack(6).unwrap_or(0);
    let protect = stack(9).unwrap_or(0) as u32;
    let sec = match crate::nt_process::object(proc, section) {
        Some(KernelObjectRef::Section(s)) => s,
        Some(_) => return status::OBJECT_TYPE_MISMATCH,
        None => return status::INVALID_HANDLE,
    };
    let Some(target) = process_of(proc, process) else { return status::INVALID_HANDLE };
    let (Some(want), Some(view)) = (read_u64(base_ptr), read_u64(size_ptr)) else { return status::ACCESS_VIOLATION };
    let offset = if offset_ptr != 0 { read_u64(offset_ptr).ok_or(status::ACCESS_VIOLATION).unwrap_or(u64::MAX) } else { 0 };
    if offset == u64::MAX {
        return status::ACCESS_VIOLATION;
    }
    if offset & 0xFFF != 0 || want & 0xFFF != 0 {
        return STATUS_MAPPED_ALIGNMENT;
    }
    if offset >= sec.size {
        return STATUS_INVALID_VIEW_SIZE;
    }
    let size = if view == 0 { sec.size - offset } else { view };
    if offset + size > sec.size.next_multiple_of(4096) {
        return STATUS_INVALID_VIEW_SIZE;
    }
    let size = size.next_multiple_of(4096);
    let (flags, private) = view_flags(protect);
    let mapped = crate::address_space::with(target.cr3, || {
        let base = target.vad.lock().record_view(want, size as usize, protect, sec.clone(), offset)?;
        crate::nt_demand::add_section_view(base, size, sec, offset, flags, private);
        Ok::<u64, u64>(base)
    });
    match mapped {
        Ok(base) => {
            unsafe {
                *(base_ptr as *mut u64) = base;
                *(size_ptr as *mut u64) = size;
            }
            status::SUCCESS
        }
        Err(e) => e as u32 as NtStatus,
    }
}

/// NtUnmapViewOfSection(process, base).
pub fn unmap_view(proc: &Arc<Process>, process: u64, base: u64) -> NtStatus {
    let Some(target) = process_of(proc, process) else { return status::INVALID_HANDLE };
    let base = base & !0xFFF;
    crate::address_space::with(target.cr3, || match crate::nt_demand::remove_section_view(base) {
        Some(_) => {
            let _ = target.vad.lock().unmap_view_of_section(base);
            status::SUCCESS
        }
        None => STATUS_NOT_MAPPED_VIEW,
    })
}

/// NtWriteVirtualMemory(process, address, buffer*, size, written*): a page
/// at a time through a kernel buffer, written through the physical map.
pub fn write_memory(proc: &Arc<Process>, process: u64, addr: u64, buf: u64, size: u64) -> NtStatus {
    let written_out = stack(4).unwrap_or(0);
    let Some(target) = process_of(proc, process) else { return status::INVALID_HANDLE };
    if !ptr_ok(buf, size as usize) {
        return status::ACCESS_VIOLATION;
    }
    let mut done = 0u64;
    let mut page = alloc::vec![0u8; 0x1000];
    let mut result = status::SUCCESS;
    while done < size {
        let at = addr.wrapping_add(done);
        let n = (0x1000 - (at & 0xFFF)).min(size - done) as usize;
        unsafe { core::ptr::copy_nonoverlapping((buf + done) as *const u8, page.as_mut_ptr(), n) };
        let ok = crate::address_space::with(target.cr3, || {
            // Brings the page in if it isn't yet.
            if !ptr_ok(at, n) {
                return false;
            }
            let mapper = unsafe { crate::memory::get_mapper() };
            let Some(phys) = mapper.translate_addr(VirtAddr::new(at)) else { return false };
            let dst = (crate::memory::get_phys_mem_offset().as_u64() + phys.as_u64()) as *mut u8;
            unsafe { core::ptr::copy_nonoverlapping(page.as_ptr(), dst, n) };
            true
        });
        if !ok {
            result = STATUS_PARTIAL_COPY;
            break;
        }
        done += n as u64;
    }
    if ptr_ok(written_out, 8) {
        unsafe { *(written_out as *mut u64) = done };
    }
    result
}
