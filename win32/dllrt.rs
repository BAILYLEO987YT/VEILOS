//! What a VEILOS system DLL above kernel32 needs from a runtime it doesn't
//! link: Rust allocations from the process heap, a panic that ends the
//! process, the stack probe, `_fltused`, and a spin lock. Included by path
//! (`#[path = "../../dllrt.rs"] mod dllrt;`) into user32 and gdi32.

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, Ordering};

#[link(name = "kernel32", kind = "raw-dylib")]
unsafe extern "system" {
    fn GetProcessHeap() -> usize;
    fn HeapAlloc(heap: usize, flags: u32, size: usize) -> *mut u8;
    fn HeapFree(heap: usize, flags: u32, p: *mut u8) -> i32;
    fn ExitProcess(code: u32) -> !;
}

/// Allocations aligned past 16 bytes keep the heap block's address just
/// below the aligned one.
struct ProcessHeap;

unsafe impl core::alloc::GlobalAlloc for ProcessHeap {
    unsafe fn alloc(&self, layout: core::alloc::Layout) -> *mut u8 {
        let heap = unsafe { GetProcessHeap() };
        if layout.align() <= 16 {
            return unsafe { HeapAlloc(heap, 0, layout.size()) };
        }
        let raw = unsafe { HeapAlloc(heap, 0, layout.size() + layout.align() + 8) };
        if raw.is_null() {
            return raw;
        }
        let aligned = (raw as usize + 8 + layout.align() - 1) & !(layout.align() - 1);
        unsafe { *((aligned - 8) as *mut usize) = raw as usize };
        aligned as *mut u8
    }
    unsafe fn dealloc(&self, p: *mut u8, layout: core::alloc::Layout) {
        let raw = if layout.align() <= 16 { p } else { unsafe { *((p as usize - 8) as *const usize) as *mut u8 } };
        unsafe { HeapFree(GetProcessHeap(), 0, raw) };
    }
}

#[global_allocator]
static ALLOCATOR: ProcessHeap = ProcessHeap;

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    unsafe { ExitProcess(0xDEAD) }
}

/// Stack probe (normally from the MSVC C runtime): touch each page of a big
/// frame from the caller's RSP down, so a guard page is never skipped.
#[unsafe(naked)]
#[no_mangle]
pub unsafe extern "C" fn __chkstk() {
    core::arch::naked_asm!(
        "mov r10, rax",
        "lea r11, [rsp + 8]",
        "2:",
        "cmp r10, 0x1000",
        "jb 3f",
        "sub r11, 0x1000",
        "test [r11], r11",
        "sub r10, 0x1000",
        "jmp 2b",
        "3:",
        "sub r11, r10",
        "test [r11], r11",
        "ret",
    )
}

#[no_mangle]
static _fltused: i32 = 0;

/// A spin lock. Never held while calling back into the program.
pub struct Lock<T> {
    held: AtomicBool,
    value: UnsafeCell<T>,
}

unsafe impl<T> Sync for Lock<T> {}

pub struct Guard<'a, T>(&'a Lock<T>);

impl<T> Lock<T> {
    pub const fn new(value: T) -> Self {
        Lock { held: AtomicBool::new(false), value: UnsafeCell::new(value) }
    }
    pub fn lock(&self) -> Guard<'_, T> {
        while self.held.compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() {
            core::hint::spin_loop();
        }
        Guard(self)
    }
}

impl<T> core::ops::Deref for Guard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        unsafe { &*self.0.value.get() }
    }
}

impl<T> core::ops::DerefMut for Guard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut *self.0.value.get() }
    }
}

impl<T> Drop for Guard<'_, T> {
    fn drop(&mut self) {
        self.0.held.store(false, Ordering::Release);
    }
}
