//! Threads after the first (W6): where they start and how they end.
//!
//! The kernel starts a thread at `RtlUserThreadStart(start, param)`. Before
//! the start routine runs, the thread gets its own copy of every module's
//! static TLS (`__declspec(thread)` data), and each module's TLS callbacks
//! and each DLL's DllMain hear DLL_THREAD_ATTACH, in initialisation order.
//! `RtlExitUserThread` does the reverse (DLL_THREAD_DETACH, freeing the
//! TLS) and asks the kernel to end the thread; the last thread's exit ends
//! the process.

use core::ptr::read_unaligned;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

const DLL_THREAD_ATTACH: u32 = 2;
const DLL_THREAD_DETACH: u32 = 3;
const LDRP_IMAGE_DLL: u32 = 0x4;
/// Slots in a thread's ThreadLocalStoragePointer array. One more follows
/// them: the next array in `THREADS`.
const TLS_ARRAY_SLOTS: usize = nt_abi::layout::TLS_ARRAY_SLOTS;

/// Held while a DLL is loaded and while a thread gets its TLS, so that a new
/// thread either sees the DLL or is in `THREADS` for it to be given a block.
static TLS_LOCK: AtomicBool = AtomicBool::new(false);
/// The arrays of the threads started after the first (under `TLS_LOCK`).
static THREADS: AtomicU64 = AtomicU64::new(0);

fn lock_tls() {
    while TLS_LOCK.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() {
        core::hint::spin_loop();
    }
}

fn unlock_tls() {
    TLS_LOCK.store(false, Ordering::Release);
}

type DllEntry = unsafe extern "system" fn(u64, u32, usize) -> i32;
type TlsCallback = unsafe extern "system" fn(u64, u32, usize);

unsafe fn rd64(a: u64) -> u64 {
    unsafe { read_unaligned(a as *const u64) }
}

unsafe fn rd32(a: u64) -> u32 {
    unsafe { read_unaligned(a as *const u32) }
}

fn teb() -> u64 {
    let t: u64;
    unsafe { core::arch::asm!("mov {}, gs:[0x30]", out(reg) t, options(nostack, readonly)) };
    t
}

fn peb() -> u64 {
    unsafe { rd64(teb() + 0x60) }
}

fn heap() -> *mut crate::heap::Heap {
    unsafe { rd64(peb() + 0x30) as *mut crate::heap::Heap }
}

/// LDR entries from one of PEB.Ldr's lists: load order (`0x10`, every
/// module, the EXE first) or initialisation order (`0x30`, DLLs only).
fn modules(list: u64, mut f: impl FnMut(u64)) {
    let ldr = unsafe { rd64(peb() + 0x18) };
    if ldr == 0 {
        return;
    }
    let head = ldr + list;
    // The links sit at +0x00 (load order) and +0x20 (init order) in an entry.
    let link_offset = if list == LOAD_ORDER { 0 } else { 0x20 };
    let mut link = unsafe { rd64(head) };
    let mut guard = 0;
    while link != head && link != 0 && guard < 512 {
        f(link - link_offset);
        link = unsafe { rd64(link) };
        guard += 1;
    }
}

const LOAD_ORDER: u64 = 0x10;
const INIT_ORDER: u64 = 0x30;

/// The module's IMAGE_TLS_DIRECTORY64, if it has one.
fn tls_directory(base: u64) -> Option<u64> {
    unsafe {
        let nt = base + rd32(base + 0x3C) as u64;
        let opt = nt + 0x18;
        if read_unaligned(opt as *const u16) != 0x20B || rd32(opt + 0x6C) <= 9 {
            return None;
        }
        let rva = rd32(opt + 0x70 + 9 * 8);
        (rva != 0).then(|| base + rva as u64)
    }
}

/// Run the module's TLS callbacks (absolute VAs, null-terminated).
fn tls_callbacks(base: u64, dir: u64, reason: u32) {
    let mut cb = unsafe { rd64(dir + 24) };
    if cb == 0 {
        return;
    }
    loop {
        let f = unsafe { rd64(cb) };
        if f == 0 {
            break;
        }
        unsafe { core::mem::transmute::<u64, TlsCallback>(f)(base, reason, 0) };
        cb += 8;
    }
}

/// Fill the empty slots of a thread's array: a copy of the TLS data of every
/// module that has an index and isn't there yet.
fn give_blocks(tls_array: *mut u64) {
    modules(LOAD_ORDER, |e| {
        let base = unsafe { rd64(e + 0x30) };
        if let Some(dir) = tls_directory(base) {
            unsafe {
                let (start, end, index_va, zero_fill) = (rd64(dir), rd64(dir + 8), rd64(dir + 16), rd32(dir + 32) as u64);
                let index = if index_va != 0 { rd32(index_va) as usize } else { usize::MAX };
                if index < TLS_ARRAY_SLOTS && *tls_array.add(index) == 0 {
                    let size = (end - start + zero_fill) as usize;
                    let block = crate::RtlAllocateHeap(heap(), 8, size.max(1));
                    if !block.is_null() {
                        core::ptr::copy_nonoverlapping(start as *const u8, block, (end - start) as usize);
                    }
                    *tls_array.add(index) = block as u64;
                }
            }
        }
    });
}

/// Give this thread its copy of every module's TLS data, then tell the
/// modules about it.
fn attach() {
    lock_tls();
    let tls_array = unsafe {
        crate::RtlAllocateHeap(heap(), 8, (TLS_ARRAY_SLOTS + 1) * 8) as *mut u64 // HEAP_ZERO_MEMORY
    };
    unsafe { *((teb() + 0x58) as *mut u64) = tls_array as u64 };
    if !tls_array.is_null() {
        give_blocks(tls_array);
        unsafe { *tls_array.add(TLS_ARRAY_SLOTS) = THREADS.load(Ordering::Relaxed) };
        THREADS.store(tls_array as u64, Ordering::Relaxed);
    }
    unlock_tls();
    notify(DLL_THREAD_ATTACH, false);
}

/// LoadLibrary is about to link a DLL.
#[no_mangle]
pub extern "system" fn LdrVeilTlsBegin() {
    lock_tls();
}

/// LoadLibrary has linked it: every thread running now gets a block for each
/// new module with TLS, before the module's own code runs.
#[no_mangle]
pub extern "system" fn LdrVeilTlsEnd() {
    if !heap().is_null() {
        let main = unsafe { rd64(peb() + nt_abi::layout::PEB_MAIN_TLS_ARRAY as u64) } as *mut u64;
        if !main.is_null() {
            give_blocks(main);
        }
        let mut array = THREADS.load(Ordering::Relaxed) as *mut u64;
        while !array.is_null() {
            give_blocks(array);
            array = unsafe { *array.add(TLS_ARRAY_SLOTS) } as *mut u64;
        }
    }
    unlock_tls();
}

/// TLS callbacks and DllMains, for `reason`: the DLLs in initialisation
/// order, then the EXE's TLS callbacks (all reversed if `backwards`).
fn notify(reason: u32, backwards: bool) {
    let mut entries = [0u64; 128];
    let mut n = 0;
    modules(INIT_ORDER, |e| {
        if n < entries.len() {
            entries[n] = e;
            n += 1;
        }
    });
    modules(LOAD_ORDER, |e| {
        if n < entries.len() && unsafe { rd32(e + 0x68) } & LDRP_IMAGE_DLL == 0 {
            entries[n] = e;
            n += 1;
        }
    });
    let order: &mut dyn Iterator<Item = &u64> = if backwards { &mut entries[..n].iter().rev() } else { &mut entries[..n].iter() };
    for &e in order {
        let base = unsafe { rd64(e + 0x30) };
        if let Some(dir) = tls_directory(base) {
            tls_callbacks(base, dir, reason);
        }
        let flags = unsafe { rd32(e + 0x68) };
        let entry = unsafe { rd64(e + 0x38) };
        if flags & LDRP_IMAGE_DLL != 0 && entry != 0 {
            unsafe { core::mem::transmute::<u64, DllEntry>(entry)(base, reason, 0) };
        }
    }
}

/// Where the kernel starts every thread after the first.
#[no_mangle]
pub unsafe extern "system" fn RtlUserThreadStart(start: u64, param: u64) -> ! {
    attach();
    let f: unsafe extern "system" fn(u64) -> u32 = unsafe { core::mem::transmute(start) };
    let code = unsafe { f(param) };
    unsafe { RtlExitUserThread(code) }
}

/// End the calling thread with `code` (ExitThread).
#[no_mangle]
pub unsafe extern "system" fn RtlExitUserThread(code: u32) -> ! {
    notify(DLL_THREAD_DETACH, true);
    let array = unsafe { rd64(teb() + 0x58) } as *mut u64;
    if !array.is_null() {
        lock_tls();
        let main_field = (peb() + nt_abi::layout::PEB_MAIN_TLS_ARRAY as u64) as *mut u64;
        let is_main = unsafe { *main_field } == array as u64;
        if is_main {
            // Its array and first blocks are the loader's, not the heap's.
            unsafe { *main_field = 0 };
        } else {
            let mut at = THREADS.load(Ordering::Relaxed) as *mut u64;
            if at == array {
                THREADS.store(unsafe { *array.add(TLS_ARRAY_SLOTS) }, Ordering::Relaxed);
            } else {
                while !at.is_null() {
                    let next = unsafe { *at.add(TLS_ARRAY_SLOTS) } as *mut u64;
                    if next == array {
                        unsafe { *at.add(TLS_ARRAY_SLOTS) = *array.add(TLS_ARRAY_SLOTS) };
                        break;
                    }
                    at = next;
                }
            }
        }
        unlock_tls();
        unsafe { *((teb() + 0x58) as *mut u64) = 0 };
        if !is_main {
            for i in 0..TLS_ARRAY_SLOTS {
                let block = unsafe { *array.add(i) };
                if block != 0 {
                    unsafe { crate::RtlFreeHeap(heap(), 0, block as *mut u8) };
                }
            }
            unsafe { crate::RtlFreeHeap(heap(), 0, array as *mut u8) };
        }
    }
    loop {
        unsafe { crate::NtTerminateThread(nt_abi::CURRENT_THREAD, code as i32) };
    }
}

#[no_mangle]
pub extern "system" fn LdrShutdownThread() {
    notify(DLL_THREAD_DETACH, true);
}
