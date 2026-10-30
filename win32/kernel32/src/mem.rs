//! Virtual memory, heaps (over ntdll's Rtl heap), Local/Global memory and
//! system information.

use nt_abi::{status, CURRENT_PROCESS};

use crate::util::process_heap;
use crate::{check, fail, Bool, Handle};

const HEAP_ZERO_MEMORY: u32 = 0x08;
const LMEM_ZEROINIT: u32 = 0x40;
const MEM_RELEASE: u32 = 0x8000;

#[no_mangle]
pub unsafe extern "system" fn VirtualAlloc(address: usize, size: usize, kind: u32, protect: u32) -> usize {
    let (mut base, mut len) = (address, size);
    let s = unsafe { crate::NtAllocateVirtualMemory(CURRENT_PROCESS, &mut base, 0, &mut len, kind, protect) };
    if status::is_success(s) {
        base
    } else {
        fail(s);
        0
    }
}

#[no_mangle]
pub unsafe extern "system" fn VirtualFree(address: usize, size: usize, kind: u32) -> Bool {
    if kind & MEM_RELEASE != 0 && size != 0 {
        return fail(status::INVALID_PARAMETER);
    }
    let (mut base, mut len) = (address, size);
    check(unsafe { crate::NtFreeVirtualMemory(CURRENT_PROCESS, &mut base, &mut len, kind) })
}

#[no_mangle]
pub unsafe extern "system" fn VirtualProtect(address: usize, size: usize, protect: u32, old: *mut u32) -> Bool {
    let (mut base, mut len) = (address, size);
    let mut previous = 0u32;
    let s = unsafe { crate::NtProtectVirtualMemory(CURRENT_PROCESS, &mut base, &mut len, protect, &mut previous) };
    if !old.is_null() {
        unsafe { *old = previous };
    }
    check(s)
}

/// MEMORY_BASIC_INFORMATION (48 bytes); returns the bytes written, 0 on failure.
#[no_mangle]
pub unsafe extern "system" fn VirtualQuery(address: usize, info: *mut u8, length: usize) -> usize {
    let mut written = 0usize;
    let s = unsafe { crate::NtQueryVirtualMemory(CURRENT_PROCESS, address, 0, info, length, &mut written) };
    if status::is_success(s) {
        written
    } else {
        fail(s);
        0
    }
}

#[no_mangle]
pub extern "system" fn GetProcessHeap() -> Handle {
    process_heap() as Handle
}

#[no_mangle]
pub extern "system" fn HeapCreate(_options: u32, _initial: usize, _maximum: usize) -> Handle {
    let h = unsafe { crate::RtlCreateHeap(0, 0, 0, 0, 0, 0) };
    if h == 0 {
        fail(status::NO_MEMORY);
    }
    h as Handle
}

#[no_mangle]
pub extern "system" fn HeapDestroy(heap: Handle) -> Bool {
    unsafe { crate::RtlDestroyHeap(heap as usize) };
    1
}

#[no_mangle]
pub extern "system" fn HeapAlloc(heap: Handle, flags: u32, size: usize) -> *mut u8 {
    let p = unsafe { crate::RtlAllocateHeap(heap as usize, flags, size) };
    if p.is_null() {
        crate::SetLastError(8); // ERROR_NOT_ENOUGH_MEMORY
    }
    p
}

#[no_mangle]
pub extern "system" fn HeapFree(heap: Handle, flags: u32, p: *mut u8) -> Bool {
    if unsafe { crate::RtlFreeHeap(heap as usize, flags, p) } != 0 { 1 } else { fail(status::INVALID_PARAMETER) }
}

#[no_mangle]
pub extern "system" fn HeapReAlloc(heap: Handle, flags: u32, p: *mut u8, size: usize) -> *mut u8 {
    let q = unsafe { crate::RtlReAllocateHeap(heap as usize, flags, p, size) };
    if q.is_null() {
        crate::SetLastError(8);
    }
    q
}

#[no_mangle]
pub extern "system" fn HeapSize(heap: Handle, flags: u32, p: *mut u8) -> usize {
    unsafe { crate::RtlSizeHeap(heap as usize, flags, p) }
}

#[no_mangle]
pub extern "system" fn HeapValidate(_heap: Handle, _flags: u32, _p: *mut u8) -> Bool {
    1
}

#[no_mangle]
pub extern "system" fn HeapSetInformation(_heap: Handle, _class: u32, _info: usize, _length: usize) -> Bool {
    1
}

// Local and Global memory: fixed blocks from the process heap (handles are
// pointers, so GMEM_MOVEABLE blocks behave as fixed ones).

#[no_mangle]
pub extern "system" fn LocalAlloc(flags: u32, size: usize) -> *mut u8 {
    HeapAlloc(GetProcessHeap(), if flags & LMEM_ZEROINIT != 0 { HEAP_ZERO_MEMORY } else { 0 }, size)
}

#[no_mangle]
pub extern "system" fn LocalFree(p: *mut u8) -> *mut u8 {
    if p.is_null() || HeapFree(GetProcessHeap(), 0, p) != 0 { core::ptr::null_mut() } else { p }
}

#[no_mangle]
pub extern "system" fn LocalReAlloc(p: *mut u8, size: usize, flags: u32) -> *mut u8 {
    HeapReAlloc(GetProcessHeap(), if flags & LMEM_ZEROINIT != 0 { HEAP_ZERO_MEMORY } else { 0 }, p, size)
}

#[no_mangle]
pub extern "system" fn LocalSize(p: *mut u8) -> usize {
    HeapSize(GetProcessHeap(), 0, p)
}

#[no_mangle]
pub extern "system" fn LocalLock(p: *mut u8) -> *mut u8 {
    p
}

#[no_mangle]
pub extern "system" fn LocalUnlock(_p: *mut u8) -> Bool {
    1
}

#[no_mangle]
pub extern "system" fn GlobalAlloc(flags: u32, size: usize) -> *mut u8 {
    LocalAlloc(flags, size)
}

#[no_mangle]
pub extern "system" fn GlobalFree(p: *mut u8) -> *mut u8 {
    LocalFree(p)
}

#[no_mangle]
pub extern "system" fn GlobalReAlloc(p: *mut u8, size: usize, flags: u32) -> *mut u8 {
    LocalReAlloc(p, size, flags)
}

#[no_mangle]
pub extern "system" fn GlobalSize(p: *mut u8) -> usize {
    LocalSize(p)
}

#[no_mangle]
pub extern "system" fn GlobalLock(p: *mut u8) -> *mut u8 {
    p
}

#[no_mangle]
pub extern "system" fn GlobalUnlock(_p: *mut u8) -> Bool {
    1
}

/// SYSTEM_INFO (48 bytes).
#[no_mangle]
pub unsafe extern "system" fn GetSystemInfo(info: *mut u8) {
    let cpus = unsafe { *(crate::peb().add(0xB8) as *const u32) }.max(1);
    unsafe {
        core::ptr::write_bytes(info, 0, 48);
        *(info as *mut u16) = 9; // PROCESSOR_ARCHITECTURE_AMD64
        *(info.add(4) as *mut u32) = 4096; // dwPageSize
        *(info.add(8) as *mut u64) = 0x1_0000; // lpMinimumApplicationAddress
        *(info.add(16) as *mut u64) = 0x7FFF_FFFE_FFFF; // lpMaximumApplicationAddress
        *(info.add(24) as *mut u64) = (1u64 << cpus) - 1; // dwActiveProcessorMask
        *(info.add(32) as *mut u32) = cpus; // dwNumberOfProcessors
        *(info.add(36) as *mut u32) = 8664; // dwProcessorType: PROCESSOR_AMD_X8664
        *(info.add(40) as *mut u32) = 0x1_0000; // dwAllocationGranularity
        *(info.add(44) as *mut u16) = 6; // wProcessorLevel
    }
}

#[no_mangle]
pub unsafe extern "system" fn GetNativeSystemInfo(info: *mut u8) {
    unsafe { GetSystemInfo(info) };
}
