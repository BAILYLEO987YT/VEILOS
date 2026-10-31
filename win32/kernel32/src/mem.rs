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

// Local and Global memory, from the process heap. A fixed block's handle
// is its address. A moveable block's handle is an entry in this table
// holding its address (so `*(void**)h` is the data, as on Windows) and a
// lock count; the handle stays the same when the block is reallocated.

const LMEM_MOVEABLE: u32 = 0x2;
const LMEM_MODIFY: u32 = 0x80;
const LMEM_DISCARDED: u32 = 0x4000;
const ERROR_NOT_LOCKED: u32 = 158;
const ERROR_INVALID_HANDLE: u32 = 6;

#[repr(C, align(16))]
#[derive(Clone, Copy)]
struct Moveable {
    data: *mut u8,
    locks: u32,
    used: bool,
}

const FREE: Moveable = Moveable { data: core::ptr::null_mut(), locks: 0, used: false };
const MOVEABLE_SLOTS: usize = 4096;
static mut MOVEABLE: [Moveable; MOVEABLE_SLOTS] = [FREE; MOVEABLE_SLOTS];
static MOVEABLE_LOCK: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

fn moveable<R>(f: impl FnOnce(&mut [Moveable; MOVEABLE_SLOTS]) -> R) -> R {
    use core::sync::atomic::Ordering;
    while MOVEABLE_LOCK.compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() {
        core::hint::spin_loop();
    }
    let r = f(unsafe { &mut *core::ptr::addr_of_mut!(MOVEABLE) });
    MOVEABLE_LOCK.store(false, Ordering::Release);
    r
}

/// The table entry `h` names, if it's a moveable block's handle.
fn slot_of(h: *mut u8) -> Option<usize> {
    let base = core::ptr::addr_of!(MOVEABLE) as usize;
    let at = (h as usize).wrapping_sub(base);
    let size = core::mem::size_of::<Moveable>();
    (at < MOVEABLE_SLOTS * size && at % size == 0).then_some(at / size)
}

fn heap_flags(flags: u32) -> u32 {
    if flags & LMEM_ZEROINIT != 0 { HEAP_ZERO_MEMORY } else { 0 }
}

#[no_mangle]
pub extern "system" fn LocalAlloc(flags: u32, size: usize) -> *mut u8 {
    if flags & LMEM_MOVEABLE == 0 {
        return HeapAlloc(GetProcessHeap(), heap_flags(flags), size);
    }
    // Size 0: a discarded block (no data until reallocated).
    let data = if size == 0 { core::ptr::null_mut() } else { HeapAlloc(GetProcessHeap(), heap_flags(flags), size) };
    if size != 0 && data.is_null() {
        return core::ptr::null_mut();
    }
    let h = moveable(|t| {
        let i = t.iter().position(|m| !m.used)?;
        t[i] = Moveable { data, locks: 0, used: true };
        Some(&mut t[i] as *mut Moveable as *mut u8)
    });
    match h {
        Some(h) => h,
        None => {
            if !data.is_null() {
                HeapFree(GetProcessHeap(), 0, data);
            }
            crate::SetLastError(8);
            core::ptr::null_mut()
        }
    }
}

#[no_mangle]
pub extern "system" fn LocalFree(h: *mut u8) -> *mut u8 {
    if h.is_null() {
        return h;
    }
    if let Some(i) = slot_of(h) {
        let data = moveable(|t| {
            let m = t[i];
            t[i] = FREE;
            m.used.then_some(m.data)
        });
        return match data {
            Some(d) => {
                if !d.is_null() {
                    HeapFree(GetProcessHeap(), 0, d);
                }
                core::ptr::null_mut()
            }
            None => {
                crate::SetLastError(ERROR_INVALID_HANDLE);
                h
            }
        };
    }
    if HeapFree(GetProcessHeap(), 0, h) != 0 { core::ptr::null_mut() } else { h }
}

#[no_mangle]
pub extern "system" fn LocalReAlloc(h: *mut u8, size: usize, flags: u32) -> *mut u8 {
    let Some(i) = slot_of(h) else {
        return HeapReAlloc(GetProcessHeap(), heap_flags(flags), h, size);
    };
    if flags & LMEM_MODIFY != 0 {
        return h;
    }
    let old = moveable(|t| t[i].used.then_some(t[i].data));
    let Some(old) = old else {
        crate::SetLastError(ERROR_INVALID_HANDLE);
        return core::ptr::null_mut();
    };
    let data = match (old.is_null(), size) {
        (_, 0) => {
            if !old.is_null() {
                HeapFree(GetProcessHeap(), 0, old);
            }
            core::ptr::null_mut()
        }
        (true, _) => HeapAlloc(GetProcessHeap(), heap_flags(flags), size),
        (false, _) => HeapReAlloc(GetProcessHeap(), heap_flags(flags), old, size),
    };
    if size != 0 && data.is_null() {
        return core::ptr::null_mut();
    }
    moveable(|t| t[i].data = data);
    h
}

#[no_mangle]
pub extern "system" fn LocalSize(h: *mut u8) -> usize {
    match slot_of(h) {
        Some(i) => {
            let d = moveable(|t| t[i].data);
            if d.is_null() { 0 } else { HeapSize(GetProcessHeap(), 0, d) }
        }
        None => HeapSize(GetProcessHeap(), 0, h),
    }
}

#[no_mangle]
pub extern "system" fn LocalLock(h: *mut u8) -> *mut u8 {
    match slot_of(h) {
        Some(i) => moveable(|t| {
            if t[i].used && !t[i].data.is_null() {
                t[i].locks += 1;
                t[i].data
            } else {
                core::ptr::null_mut()
            }
        }),
        None => h,
    }
}

/// Nonzero while still locked; 0 when the last lock goes (or for a fixed
/// block, which was never locked).
#[no_mangle]
pub extern "system" fn LocalUnlock(h: *mut u8) -> Bool {
    let Some(i) = slot_of(h) else {
        crate::SetLastError(ERROR_NOT_LOCKED);
        return 0;
    };
    let left = moveable(|t| {
        if t[i].locks == 0 {
            return None;
        }
        t[i].locks -= 1;
        Some(t[i].locks)
    });
    match left {
        Some(0) => {
            crate::SetLastError(0);
            0
        }
        Some(_) => 1,
        None => {
            crate::SetLastError(ERROR_NOT_LOCKED);
            0
        }
    }
}

/// The handle of the block whose data is at `p` (a fixed block's is `p`).
#[no_mangle]
pub extern "system" fn LocalHandle(p: *mut u8) -> *mut u8 {
    if p.is_null() {
        return p;
    }
    moveable(|t| t.iter_mut().find(|m| m.used && m.data == p).map(|m| m as *mut Moveable as *mut u8)).unwrap_or(p)
}

/// The lock count, and LMEM_DISCARDED for a moveable block without data.
#[no_mangle]
pub extern "system" fn LocalFlags(h: *mut u8) -> u32 {
    match slot_of(h) {
        Some(i) => moveable(|t| t[i].locks.min(0xFF) | if t[i].data.is_null() { LMEM_DISCARDED } else { 0 }),
        None => 0,
    }
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
    LocalLock(p)
}

#[no_mangle]
pub extern "system" fn GlobalUnlock(p: *mut u8) -> Bool {
    LocalUnlock(p)
}

#[no_mangle]
pub extern "system" fn GlobalHandle(p: *mut u8) -> *mut u8 {
    LocalHandle(p)
}

#[no_mangle]
pub extern "system" fn GlobalFlags(p: *mut u8) -> u32 {
    LocalFlags(p)
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
