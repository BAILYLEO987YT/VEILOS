//! The Rtl heap: what HeapAlloc, malloc (through the CRT) and LocalAlloc end
//! up in. Entirely user mode; memory comes from NtAllocateVirtualMemory.
//!
//! Each block has a 16-byte header: its capacity (bytes after the header, a
//! multiple of 16), the size the caller asked for, and a tag. Freed blocks
//! go on free lists: one per 16-byte size class up to SMALL_LIMIT (exact
//! fit), one first-fit list above that (blocks split when much too big).
//! New blocks are carved from the newest segment; when it's full a new one
//! is mapped. Requests of BIG_LIMIT or more get their own mapping, released
//! on free. A spinlock serialises each heap.

use core::sync::atomic::{AtomicBool, Ordering};
use nt_abi::{status, CURRENT_PROCESS};

use crate::{NtAllocateVirtualMemory, NtFreeVirtualMemory};

const ALIGN: usize = 16;
const HEADER: usize = 16;
const SMALL_LIMIT: usize = 1024;
const SMALL_CLASSES: usize = SMALL_LIMIT / ALIGN + 1;
const BIG_LIMIT: usize = 512 * 1024;
const SEGMENT_SIZE: usize = 1024 * 1024;

const MEM_COMMIT: u32 = 0x1000;
const MEM_RESERVE: u32 = 0x2000;
const MEM_RELEASE: u32 = 0x8000;
const PAGE_READWRITE: u32 = 0x04;

pub const HEAP_ZERO_MEMORY: u32 = 0x08;
pub const HEAP_REALLOC_IN_PLACE_ONLY: u32 = 0x10;

const TAG_USED: u32 = 0x5548_4541; // "AEHU"
const TAG_BIG: u32 = 0x4248_4541; // "AEHB"
const TAG_FREE: u32 = 0x4648_4541; // "AEHF"

#[repr(C)]
struct Block {
    capacity: usize,
    requested: u32,
    tag: u32,
}

/// A free block's link, stored in its (now unused) data area.
#[repr(C)]
struct FreeLink {
    next: *mut Block,
}

#[repr(C)]
pub struct Heap {
    lock: AtomicBool,
    small: [*mut Block; SMALL_CLASSES],
    large: *mut Block,
    /// Carving position and end of the newest segment.
    top: usize,
    end: usize,
    /// Segments (first word of each points at the previous), for destroy.
    segments: usize,
}

fn round_up(n: usize) -> usize {
    (n + ALIGN - 1) & !(ALIGN - 1)
}

unsafe fn map(size: usize) -> Option<usize> {
    let mut base: usize = 0;
    let mut len: usize = size;
    let s = unsafe {
        NtAllocateVirtualMemory(CURRENT_PROCESS, &mut base, 0, &mut len, MEM_RESERVE | MEM_COMMIT, PAGE_READWRITE)
    };
    (status::is_success(s) && base != 0).then_some(base)
}

unsafe fn unmap(base: usize) {
    let mut b = base;
    let mut len: usize = 0;
    unsafe { NtFreeVirtualMemory(CURRENT_PROCESS, &mut b, &mut len, MEM_RELEASE) };
}

/// Holds a heap's lock; a raw pointer so the heap can still be mutated
/// while it's held.
struct Guard(*const AtomicBool);
impl Drop for Guard {
    fn drop(&mut self) {
        unsafe { (*self.0).store(false, Ordering::Release) };
    }
}

impl Heap {
    /// A new heap: its header sits at the start of its first segment.
    pub unsafe fn create() -> *mut Heap {
        let Some(base) = (unsafe { map(SEGMENT_SIZE) }) else { return core::ptr::null_mut() };
        let heap = base as *mut Heap;
        let first = round_up(base + core::mem::size_of::<Heap>());
        unsafe {
            heap.write(Heap {
                lock: AtomicBool::new(false),
                small: [core::ptr::null_mut(); SMALL_CLASSES],
                large: core::ptr::null_mut(),
                top: first,
                end: base + SEGMENT_SIZE,
                // The first segment holds this header; destroy frees it last.
                segments: 0,
            });
        }
        heap
    }

    /// Remember an extra segment for `destroy`: its first word links to the
    /// previous one.
    unsafe fn push_segment(&mut self, base: usize) {
        unsafe { *(base as *mut usize) = self.segments };
        self.segments = base;
    }

    fn lock(&self) -> Guard {
        let mut spins = 0u32;
        while self.lock.compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() {
            // Another thread holds it: let it run (it may share this CPU).
            spins += 1;
            if spins > 64 {
                unsafe { crate::NtYieldExecution() };
            } else {
                core::hint::spin_loop();
            }
        }
        Guard(&self.lock)
    }

    pub unsafe fn alloc(&mut self, size: usize, flags: u32) -> *mut u8 {
        let cap = round_up(size.max(1));
        let p = if cap >= BIG_LIMIT {
            let Some(base) = (unsafe { map(cap + HEADER) }) else { return core::ptr::null_mut() };
            let b = base as *mut Block;
            unsafe { b.write(Block { capacity: cap, requested: size as u32, tag: TAG_BIG }) };
            (base + HEADER) as *mut u8
        } else {
            let _g = self.lock();
            let b = match unsafe { self.take_free(cap) } {
                Some(b) => b,
                None => unsafe { self.carve(cap) },
            };
            if b.is_null() {
                return core::ptr::null_mut();
            }
            unsafe {
                (*b).requested = size as u32;
                (*b).tag = TAG_USED;
            }
            (b as usize + HEADER) as *mut u8
        };
        if flags & HEAP_ZERO_MEMORY != 0 {
            unsafe { core::ptr::write_bytes(p, 0, size) };
        }
        p
    }

    unsafe fn take_free(&mut self, cap: usize) -> Option<*mut Block> {
        if cap <= SMALL_LIMIT {
            let class = cap / ALIGN;
            let b = self.small[class];
            if !b.is_null() {
                self.small[class] = unsafe { (*(b as *mut u8).add(HEADER).cast::<FreeLink>()).next };
                return Some(b);
            }
            return None;
        }
        // First fit in the large list; split off what's left if it's worth it.
        let mut prev: *mut *mut Block = &mut self.large;
        let mut b = self.large;
        while !b.is_null() {
            let next = unsafe { (*(b as *mut u8).add(HEADER).cast::<FreeLink>()).next };
            if unsafe { (*b).capacity } >= cap {
                unsafe { *prev = next };
                let spare = unsafe { (*b).capacity } - cap;
                if spare >= HEADER + SMALL_LIMIT {
                    unsafe {
                        (*b).capacity = cap;
                        let rest = (b as usize + HEADER + cap) as *mut Block;
                        rest.write(Block { capacity: spare - HEADER, requested: 0, tag: TAG_FREE });
                        self.put_free(rest);
                    }
                }
                return Some(b);
            }
            prev = unsafe { &mut (*(b as *mut u8).add(HEADER).cast::<FreeLink>()).next };
            b = next;
        }
        None
    }

    unsafe fn carve(&mut self, cap: usize) -> *mut Block {
        if self.top + HEADER + cap > self.end {
            let Some(base) = (unsafe { map(SEGMENT_SIZE) }) else { return core::ptr::null_mut() };
            unsafe { self.push_segment(base) };
            self.top = round_up(base + core::mem::size_of::<usize>());
            self.end = base + SEGMENT_SIZE;
        }
        let b = self.top as *mut Block;
        self.top += HEADER + cap;
        unsafe { b.write(Block { capacity: cap, requested: 0, tag: TAG_USED }) };
        b
    }

    unsafe fn put_free(&mut self, b: *mut Block) {
        unsafe { (*b).tag = TAG_FREE };
        let cap = unsafe { (*b).capacity };
        let link = unsafe { (b as *mut u8).add(HEADER).cast::<FreeLink>() };
        if cap <= SMALL_LIMIT {
            let class = cap / ALIGN;
            unsafe { (*link).next = self.small[class] };
            self.small[class] = b;
        } else {
            unsafe { (*link).next = self.large };
            self.large = b;
        }
    }

    /// The block behind `p`, if `p` is a live allocation of this allocator.
    unsafe fn block(p: *mut u8) -> Option<*mut Block> {
        if p.is_null() || (p as usize) % ALIGN != 0 {
            return None;
        }
        let b = (p as usize - HEADER) as *mut Block;
        let tag = unsafe { (*b).tag };
        (tag == TAG_USED || tag == TAG_BIG).then_some(b)
    }

    pub unsafe fn free(&mut self, p: *mut u8) -> bool {
        let Some(b) = (unsafe { Self::block(p) }) else { return false };
        if unsafe { (*b).tag } == TAG_BIG {
            unsafe { unmap(b as usize) };
            return true;
        }
        let _g = self.lock();
        unsafe { self.put_free(b) };
        true
    }

    pub unsafe fn size(p: *mut u8) -> Option<usize> {
        unsafe { Self::block(p).map(|b| (*b).requested as usize) }
    }

    pub unsafe fn realloc(&mut self, p: *mut u8, size: usize, flags: u32) -> *mut u8 {
        let Some(b) = (unsafe { Self::block(p) }) else { return core::ptr::null_mut() };
        let old = unsafe { (*b).requested as usize };
        if size <= unsafe { (*b).capacity } {
            if flags & HEAP_ZERO_MEMORY != 0 && size > old {
                unsafe { core::ptr::write_bytes(p.add(old), 0, size - old) };
            }
            unsafe { (*b).requested = size as u32 };
            return p;
        }
        if flags & HEAP_REALLOC_IN_PLACE_ONLY != 0 {
            return core::ptr::null_mut();
        }
        let q = unsafe { self.alloc(size, flags) };
        if !q.is_null() {
            unsafe {
                core::ptr::copy_nonoverlapping(p, q, old.min(size));
                self.free(p);
            }
        }
        q
    }

    /// Release every segment. Big blocks still allocated keep their own
    /// mappings until the process exits.
    pub unsafe fn destroy(heap: *mut Heap) {
        let mut seg = unsafe { (*heap).segments };
        while seg != 0 {
            let prev = unsafe { *(seg as *const usize) };
            unsafe { unmap(seg) };
            seg = prev;
        }
        unsafe { unmap(heap as usize) };
    }
}
