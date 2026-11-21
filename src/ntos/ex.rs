// Executive support: reader/writer resources (ERESOURCE), fast and guarded
// mutexes, interlocked singly linked lists (SLIST), lookaside lists,
// interlocked doubly linked lists, callback objects, Ex timers, UUIDs, and
// object-type data exports.
//
// Drivers inline some of these (the WDK's ExAllocateFromNPagedLookasideList,
// ExInitializeFastMutex, ExQueryDepthSList's header read), so those
// structures have Windows' x64 layout exactly (checked in k1's layout.c):
//
//   FAST_MUTEX / KGUARDED_MUTEX: Count (i32, bit 0 = free) 0, Owner 8,
//     Contention 0x10, Event/Gate 0x18, OldIrql 0x30.
//   SLIST_HEADER: Depth (bits 0-15) and Sequence (16-63) in the first
//     quadword; HeaderType (bit 0) and NextEntry >> 4 (bits 4-63) in the
//     second. Entries are 16-byte aligned; every change goes through here,
//     under one lock.
//   GENERAL_LOOKASIDE: ListHead 0, Depth 0x10, MaximumDepth 0x12, the
//     counters, Type 0x24, Tag 0x28, Size 0x2C, Allocate 0x30, Free 0x38,
//     ListEntry 0x40.
//
// ERESOURCE (0x68 bytes) is opaque to drivers; VEILOS keeps its own fields
// in it: a lock word 0, exclusive owner 8, exclusive count 0x10, shared
// count 0x14.

use alloc::{collections::BTreeMap, string::String, vec::Vec};
use core::sync::atomic::{AtomicI32, AtomicU64, Ordering};
use spin::Mutex;

use super::{dpc, ke, pool, rtl, NtStatus, STATUS_SUCCESS};

const STATUS_OBJECT_NAME_NOT_FOUND: NtStatus = 0xC000_0034u32 as i32;
const STATUS_NO_MEMORY: NtStatus = 0xC000_0017u32 as i32;

unsafe fn rd<T: Copy>(p: *const u8, off: usize) -> T {
    unsafe { (p.add(off) as *const T).read_unaligned() }
}
unsafe fn wr<T>(p: *mut u8, off: usize, v: T) {
    unsafe { (p.add(off) as *mut T).write_unaligned(v) }
}

/// This thread, as owner fields record it (never 0).
fn me() -> u64 {
    crate::kthread::current_id() | 1 << 63
}

// -----| ERESOURCE |-----

fn res_lock(r: *mut u8) -> &'static AtomicU64 {
    unsafe { &*(r as *const AtomicU64) }
}

/// Run `f` on the resource's fields under its lock.
fn with_res<R>(r: *mut u8, f: impl FnOnce(&mut u64, &mut u32, &mut u32) -> R) -> R {
    let l = res_lock(r);
    while l.compare_exchange_weak(0, 1, Ordering::Acquire, Ordering::Relaxed).is_err() {
        core::hint::spin_loop();
    }
    let (owner, excl, shared) = unsafe { (&mut *(r.add(8) as *mut u64), &mut *(r.add(0x10) as *mut u32), &mut *(r.add(0x14) as *mut u32)) };
    let v = f(owner, excl, shared);
    l.store(0, Ordering::Release);
    v
}

pub extern "win64" fn ExInitializeResourceLite(r: *mut u8) -> NtStatus {
    unsafe { core::ptr::write_bytes(r, 0, 0x68) };
    STATUS_SUCCESS
}
pub extern "win64" fn ExReinitializeResourceLite(r: *mut u8) -> NtStatus {
    ExInitializeResourceLite(r)
}
pub extern "win64" fn ExDeleteResourceLite(_r: *mut u8) -> NtStatus {
    STATUS_SUCCESS
}

fn acquire(r: *mut u8, wait: u8, exclusive: bool) -> u8 {
    let me = me();
    loop {
        let got = with_res(r, |owner, excl, shared| {
            if *excl > 0 && *owner == me {
                *excl += 1; // recursion; shared inside exclusive counts as exclusive
                return true;
            }
            if exclusive {
                if *excl == 0 && *shared == 0 {
                    *owner = me;
                    *excl = 1;
                    return true;
                }
            } else if *excl == 0 {
                *shared += 1;
                return true;
            }
            false
        });
        if got {
            return 1;
        }
        if wait == 0 {
            return 0;
        }
        ke::pause_at_current_irql();
    }
}

pub extern "win64" fn ExAcquireResourceExclusiveLite(r: *mut u8, wait: u8) -> u8 {
    acquire(r, wait, true)
}
pub extern "win64" fn ExAcquireResourceSharedLite(r: *mut u8, wait: u8) -> u8 {
    acquire(r, wait, false)
}
pub extern "win64" fn ExAcquireSharedStarveExclusive(r: *mut u8, wait: u8) -> u8 {
    acquire(r, wait, false)
}
pub extern "win64" fn ExAcquireSharedWaitForExclusive(r: *mut u8, wait: u8) -> u8 {
    acquire(r, wait, false)
}

pub extern "win64" fn ExReleaseResourceLite(r: *mut u8) {
    let me = me();
    with_res(r, |owner, excl, shared| {
        if *excl > 0 && *owner == me {
            *excl -= 1;
            if *excl == 0 {
                *owner = 0;
            }
        } else if *shared > 0 {
            *shared -= 1;
        }
    });
}
pub extern "win64" fn ExReleaseResourceForThreadLite(r: *mut u8, _thread: u64) {
    ExReleaseResourceLite(r)
}

pub extern "win64" fn ExIsResourceAcquiredExclusiveLite(r: *mut u8) -> u8 {
    let me = me();
    with_res(r, |owner, excl, _| (*excl > 0 && *owner == me) as u8)
}
pub extern "win64" fn ExIsResourceAcquiredSharedLite(r: *mut u8) -> u32 {
    let me = me();
    with_res(r, |owner, excl, shared| if *excl > 0 && *owner == me { *excl } else { (*shared > 0) as u32 })
}
pub extern "win64" fn ExConvertExclusiveToSharedLite(r: *mut u8) {
    with_res(r, |owner, excl, shared| {
        *shared += *excl;
        *excl = 0;
        *owner = 0;
    });
}
pub extern "win64" fn ExGetExclusiveWaiterCount(_r: *mut u8) -> u32 {
    0
}
pub extern "win64" fn ExGetSharedWaiterCount(_r: *mut u8) -> u32 {
    0
}
pub extern "win64" fn ExSetResourceOwnerPointer(_r: *mut u8, _owner: u64) {}

pub extern "win64" fn ExEnterCriticalRegionAndAcquireResourceExclusive(r: *mut u8) -> *mut u8 {
    acquire(r, 1, true);
    r
}
pub extern "win64" fn ExEnterCriticalRegionAndAcquireResourceShared(r: *mut u8) -> *mut u8 {
    acquire(r, 1, false);
    r
}
pub extern "win64" fn ExReleaseResourceAndLeaveCriticalRegion(r: *mut u8) {
    ExReleaseResourceLite(r)
}

// -----| fast mutexes and guarded mutexes |-----

fn fm_count(m: *mut u8) -> &'static AtomicI32 {
    unsafe { &*(m as *const AtomicI32) }
}

fn fm_take(m: *mut u8) {
    loop {
        if fm_count(m).fetch_and(!1, Ordering::Acquire) & 1 != 0 {
            unsafe { wr::<u64>(m, 8, me()) };
            return;
        }
        ke::pause_at_current_irql();
    }
}

fn fm_give(m: *mut u8) {
    unsafe { wr::<u64>(m, 8, 0) };
    fm_count(m).fetch_or(1, Ordering::Release);
}

/// What the WDK's inline ExInitializeFastMutex does.
pub extern "win64" fn KeInitializeGuardedMutex(m: *mut u8) {
    unsafe {
        core::ptr::write_bytes(m, 0, 0x38);
        wr::<i32>(m, 0, 1);
        ke::KeInitializeEvent(m.add(0x18), 1, 0);
    }
}

pub extern "win64" fn ExAcquireFastMutex(m: *mut u8) {
    let old = ke::KfRaiseIrql(1); // APC_LEVEL
    fm_take(m);
    unsafe { wr::<u32>(m, 0x30, old as u32) };
}
pub extern "win64" fn ExReleaseFastMutex(m: *mut u8) {
    let old = unsafe { rd::<u32>(m, 0x30) } as u8;
    fm_give(m);
    ke::set_irql(old);
}
pub extern "win64" fn ExTryToAcquireFastMutex(m: *mut u8) -> u8 {
    let old = ke::KfRaiseIrql(1);
    if fm_count(m).fetch_and(!1, Ordering::Acquire) & 1 != 0 {
        unsafe {
            wr::<u64>(m, 8, me());
            wr::<u32>(m, 0x30, old as u32);
        }
        1
    } else {
        ke::set_irql(old);
        0
    }
}
pub extern "win64" fn ExAcquireFastMutexUnsafe(m: *mut u8) {
    fm_take(m)
}
pub extern "win64" fn ExReleaseFastMutexUnsafe(m: *mut u8) {
    fm_give(m)
}
pub extern "win64" fn KeAcquireGuardedMutex(m: *mut u8) {
    fm_take(m)
}
pub extern "win64" fn KeReleaseGuardedMutex(m: *mut u8) {
    fm_give(m)
}
pub extern "win64" fn KeAcquireGuardedMutexUnsafe(m: *mut u8) {
    fm_take(m)
}
pub extern "win64" fn KeReleaseGuardedMutexUnsafe(m: *mut u8) {
    fm_give(m)
}

// -----| SLISTs |-----

static SLIST_LOCK: AtomicU64 = AtomicU64::new(0);

fn slist<R>(f: impl FnOnce() -> R) -> R {
    x86_64::instructions::interrupts::without_interrupts(|| {
        while SLIST_LOCK.compare_exchange_weak(0, 1, Ordering::Acquire, Ordering::Relaxed).is_err() {
            core::hint::spin_loop();
        }
        let r = f();
        SLIST_LOCK.store(0, Ordering::Release);
        r
    })
}

unsafe fn sl_first(h: *mut u8) -> u64 {
    unsafe { rd::<u64>(h, 8) & !0xF }
}
unsafe fn sl_set(h: *mut u8, first: u64, depth: u16) {
    unsafe {
        let seq = rd::<u64>(h, 0) >> 16;
        wr::<u64>(h, 0, depth as u64 | (seq.wrapping_add(1) & 0xFFFF_FFFF_FFFF) << 16);
        wr::<u64>(h, 8, first | 1); // HeaderType: the x64 header
    }
}

pub extern "win64" fn InitializeSListHead(h: *mut u8) {
    unsafe {
        wr::<u64>(h, 0, 0);
        wr::<u64>(h, 8, 1);
    }
}

pub extern "win64" fn ExpInterlockedPushEntrySList(h: *mut u8, e: *mut u64) -> u64 {
    slist(|| unsafe {
        let first = sl_first(h);
        e.write(first);
        sl_set(h, e as u64, rd::<u16>(h, 0).wrapping_add(1));
        first
    })
}

pub extern "win64" fn ExpInterlockedPopEntrySList(h: *mut u8) -> u64 {
    slist(|| unsafe {
        let first = sl_first(h);
        if first != 0 {
            sl_set(h, (first as *const u64).read() & !0xF, rd::<u16>(h, 0).saturating_sub(1));
        }
        first
    })
}

pub extern "win64" fn ExpInterlockedFlushSList(h: *mut u8) -> u64 {
    slist(|| unsafe {
        let first = sl_first(h);
        sl_set(h, 0, 0);
        first
    })
}

pub extern "win64" fn ExQueryDepthSList(h: *mut u8) -> u16 {
    unsafe { rd(h, 0) }
}

pub extern "win64" fn FirstEntrySList(h: *mut u8) -> u64 {
    unsafe { sl_first(h) }
}

// -----| lookaside lists |-----

unsafe fn lookaside_init(l: *mut u8, allocate: u64, free: u64, size: usize, tag: u32, pool_type: u32) {
    unsafe {
        core::ptr::write_bytes(l, 0, 0x60);
        InitializeSListHead(l);
        wr::<u16>(l, 0x10, 4); // Depth
        wr::<u16>(l, 0x12, 256); // MaximumDepth
        wr::<u32>(l, 0x24, pool_type);
        wr::<u32>(l, 0x28, tag);
        wr::<u32>(l, 0x2C, size.max(16) as u32);
        let alloc = if allocate != 0 { allocate } else { super::exports::ExAllocatePoolWithTag as *const () as u64 };
        let fr = if free != 0 { free } else { super::exports::ExFreePool as *const () as u64 };
        wr::<u64>(l, 0x30, alloc);
        wr::<u64>(l, 0x38, fr);
        let list = l.add(0x40) as *mut u64;
        list.write(list as u64);
        list.add(1).write(list as u64);
    }
}

/// ExInitializeNPagedLookasideList(Lookaside, Allocate, Free, Flags, Size,
/// Tag, Depth).
pub extern "win64" fn ExInitializeNPagedLookasideList(l: *mut u8, allocate: u64, free: u64, _flags: u32, size: usize, tag: u32, _depth: u16) {
    unsafe { lookaside_init(l, allocate, free, size, tag, 0) }
}

pub extern "win64" fn ExInitializePagedLookasideList(l: *mut u8, allocate: u64, free: u64, _flags: u32, size: usize, tag: u32, _depth: u16) {
    unsafe { lookaside_init(l, allocate, free, size, tag, 1) }
}

/// Free everything still on the list.
pub extern "win64" fn ExDeleteNPagedLookasideList(l: *mut u8) {
    let free: extern "win64" fn(u64) = unsafe { core::mem::transmute(rd::<u64>(l, 0x38)) };
    loop {
        let e = ExpInterlockedPopEntrySList(l);
        if e == 0 {
            break;
        }
        free(e);
    }
}

/// What the WDK's inline lookaside allocate does (newer WDKs import it):
/// a block from the list, else a new one from its Allocate routine.
pub extern "win64" fn ExAllocateFromNPagedLookasideList(l: *mut u8) -> u64 {
    unsafe {
        wr::<u32>(l, 0x14, rd::<u32>(l, 0x14).wrapping_add(1)); // TotalAllocates
        let e = ExpInterlockedPopEntrySList(l);
        if e != 0 {
            return e;
        }
        wr::<u32>(l, 0x18, rd::<u32>(l, 0x18).wrapping_add(1)); // AllocateMisses
        let alloc: extern "win64" fn(u32, usize, u32) -> u64 = core::mem::transmute(rd::<u64>(l, 0x30));
        alloc(rd(l, 0x24), rd::<u32>(l, 0x2C) as usize, rd(l, 0x28))
    }
}

/// Back on the list, or to its Free routine once the list holds Depth.
pub extern "win64" fn ExFreeToNPagedLookasideList(l: *mut u8, e: *mut u64) {
    unsafe {
        wr::<u32>(l, 0x1C, rd::<u32>(l, 0x1C).wrapping_add(1)); // TotalFrees
        if ExQueryDepthSList(l) >= rd::<u16>(l, 0x10) {
            wr::<u32>(l, 0x20, rd::<u32>(l, 0x20).wrapping_add(1)); // FreeMisses
            let free: extern "win64" fn(*mut u64) = core::mem::transmute(rd::<u64>(l, 0x38));
            free(e);
        } else {
            ExpInterlockedPushEntrySList(l, e);
        }
    }
}

pub extern "win64" fn ExAllocateFromPagedLookasideList(l: *mut u8) -> u64 {
    ExAllocateFromNPagedLookasideList(l)
}

pub extern "win64" fn ExFreeToPagedLookasideList(l: *mut u8, e: *mut u64) {
    ExFreeToNPagedLookasideList(l, e)
}

pub extern "win64" fn ExDeletePagedLookasideList(l: *mut u8) {
    ExDeleteNPagedLookasideList(l)
}

// -----| interlocked doubly linked lists |-----
// LIST_ENTRY: Flink 0, Blink 8.

/// ExInterlockedInsertTailList(ListHead, ListEntry, Lock): the entry that
/// was last before (null if the list was empty).
pub extern "win64" fn ExInterlockedInsertTailList(head: *mut u64, e: *mut u64, lock: *mut u64) -> *mut u64 {
    let old = ke::KeAcquireSpinLockRaiseToDpc(lock);
    let r = unsafe {
        let tail = head.add(1).read() as *mut u64;
        e.write(head as u64);
        e.add(1).write(tail as u64);
        tail.write(e as u64);
        head.add(1).write(e as u64);
        if tail == head { core::ptr::null_mut() } else { tail }
    };
    ke::KeReleaseSpinLock(lock, old);
    r
}

pub extern "win64" fn ExInterlockedInsertHeadList(head: *mut u64, e: *mut u64, lock: *mut u64) -> *mut u64 {
    let old = ke::KeAcquireSpinLockRaiseToDpc(lock);
    let r = unsafe {
        let first = head.read() as *mut u64;
        e.write(first as u64);
        e.add(1).write(head as u64);
        first.add(1).write(e as u64);
        head.write(e as u64);
        if first == head { core::ptr::null_mut() } else { first }
    };
    ke::KeReleaseSpinLock(lock, old);
    r
}

pub extern "win64" fn ExInterlockedRemoveHeadList(head: *mut u64, lock: *mut u64) -> *mut u64 {
    let old = ke::KeAcquireSpinLockRaiseToDpc(lock);
    let r = unsafe {
        let first = head.read() as *mut u64;
        if first == head {
            core::ptr::null_mut()
        } else {
            let next = first.read() as *mut u64;
            head.write(next as u64);
            next.add(1).write(head as u64);
            first
        }
    };
    ke::KeReleaseSpinLock(lock, old);
    r
}

// -----| callback objects |-----

struct Callback {
    regs: Vec<(u64, u64, u64)>, // (registration id, routine, context)
}

static CALLBACKS: Mutex<BTreeMap<String, usize>> = Mutex::new(BTreeMap::new());
static CALLBACK_OBJS: Mutex<BTreeMap<usize, Callback>> = Mutex::new(BTreeMap::new());
static NEXT_REG: AtomicU64 = AtomicU64::new(0x100);

fn cb_locked<R>(f: impl FnOnce() -> R) -> R {
    x86_64::instructions::interrupts::without_interrupts(f)
}

/// ExCreateCallback(CallbackObject*, ObjectAttributes*, Create,
/// AllowMultipleCallbacks): the named callback object (\Callback\...).
pub extern "win64" fn ExCreateCallback(out: *mut usize, oa: *const u8, create: u8, _multiple: u8) -> NtStatus {
    let name = unsafe { rtl::unicode_to_string(rd::<*const u8>(oa, 0x10)) }.to_ascii_lowercase();
    let existing = cb_locked(|| CALLBACKS.lock().get(&name).copied());
    let obj = match existing {
        Some(o) => o,
        None if create == 0 => return STATUS_OBJECT_NAME_NOT_FOUND,
        None => {
            let o = pool::alloc_tracked(16) as usize;
            if o == 0 {
                return STATUS_NO_MEMORY;
            }
            cb_locked(|| {
                CALLBACKS.lock().insert(name, o);
                CALLBACK_OBJS.lock().insert(o, Callback { regs: Vec::new() });
            });
            o
        }
    };
    unsafe { out.write(obj) };
    STATUS_SUCCESS
}

pub extern "win64" fn ExRegisterCallback(obj: usize, routine: u64, context: u64) -> u64 {
    let id = NEXT_REG.fetch_add(0x10, Ordering::Relaxed);
    cb_locked(|| CALLBACK_OBJS.lock().get_mut(&obj).map(|c| c.regs.push((id, routine, context))));
    id
}

pub extern "win64" fn ExUnregisterCallback(reg: u64) {
    cb_locked(|| {
        for c in CALLBACK_OBJS.lock().values_mut() {
            c.regs.retain(|r| r.0 != reg);
        }
    });
}

/// ExNotifyCallback(CallbackObject, Argument1, Argument2): every
/// registered routine(Context, Argument1, Argument2).
pub extern "win64" fn ExNotifyCallback(obj: usize, a1: u64, a2: u64) {
    let regs = cb_locked(|| CALLBACK_OBJS.lock().get(&obj).map(|c| c.regs.clone()).unwrap_or_default());
    for (_, routine, ctx) in regs {
        let f: extern "win64" fn(u64, u64, u64) = unsafe { core::mem::transmute(routine) };
        f(ctx, a1, a2);
    }
}

// -----| Ex timers |-----
//
// An EX_TIMER is opaque: VEILOS's is a KTIMER (0), a KDPC (0x40), the
// callback (0x80) and its context (0x88).

extern "win64" fn ex_timer_dpc(_dpc: *mut u8, t: u64, _a1: u64, _a2: u64) {
    let t = t as *mut u8;
    unsafe {
        let cb: extern "win64" fn(*mut u8, u64) = core::mem::transmute(rd::<u64>(t, 0x80));
        cb(t, rd(t, 0x88));
    }
}

/// ExAllocateTimer(Callback, CallbackContext, Attributes).
pub extern "win64" fn ExAllocateTimer(callback: u64, context: u64, _attributes: u32) -> *mut u8 {
    let t = pool::alloc_tracked(0x90);
    if t.is_null() {
        return t;
    }
    unsafe {
        dpc::KeInitializeTimerEx(t, 0);
        dpc::KeInitializeDpc(t.add(0x40), ex_timer_dpc as *const () as u64, t as u64);
        wr::<u64>(t, 0x80, callback);
        wr::<u64>(t, 0x88, context);
    }
    t
}

/// ExSetTimer(Timer, DueTime (100 ns), Period (100 ns, 0: once),
/// Parameters): TRUE if it was already set.
pub extern "win64" fn ExSetTimer(t: *mut u8, due: i64, period: i64, _params: *const u8) -> u8 {
    let ms = if period > 0 { (period as u64).div_ceil(10_000) as i32 } else { 0 };
    dpc::KeSetTimerEx(t, due, ms, unsafe { t.add(0x40) })
}

pub extern "win64" fn ExCancelTimer(t: *mut u8, _params: *const u8) -> u8 {
    dpc::KeCancelTimer(t)
}

pub extern "win64" fn ExDeleteTimer(t: *mut u8, cancel: u8, _wait: u8, _params: *const u8) -> u8 {
    let was = if cancel != 0 { dpc::KeCancelTimer(t) } else { 0 };
    dpc::KeRemoveQueueDpc(unsafe { t.add(0x40) });
    pool::free_tracked(t);
    was
}

// -----| odds and ends |-----

/// ExUuidCreate(Uuid*): a random (version 4) UUID.
pub extern "win64" fn ExUuidCreate(g: *mut u8) -> NtStatus {
    let mut b = [0u8; 16];
    crate::entropy::fill(&mut b);
    b[7] = (b[7] & 0x0F) | 0x40;
    b[8] = (b[8] & 0x3F) | 0x80;
    unsafe { core::ptr::copy_nonoverlapping(b.as_ptr(), g, 16) };
    STATUS_SUCCESS
}

/// UserMode while running a program's request, KernelMode otherwise.
pub extern "win64" fn ExGetPreviousMode() -> u8 {
    (crate::kthread::current_pid() != 0) as u8
}

/// ExQueryTimerResolution(Maximum*, Minimum*, Current*): VEILOS's tick is
/// 1 ms (10 000 in 100 ns units) and doesn't change.
pub extern "win64" fn ExQueryTimerResolution(max: *mut u32, min: *mut u32, cur: *mut u32) {
    unsafe {
        max.write(10_000);
        min.write(10_000);
        cur.write(10_000);
    }
}

pub extern "win64" fn ExSetTimerResolution(_res: u32, _set: u8) -> u32 {
    10_000
}

pub extern "win64" fn ExAllocatePoolWithQuotaTag(_pool_type: u32, size: usize, _tag: u32) -> *mut u8 {
    pool::alloc_tracked(size)
}

// Object types (POBJECT_TYPE variables): drivers pass them to
// ObReferenceObjectByHandle, which VEILOS doesn't check against. Each points
// at a block of zeros of its own.

pub static mut EX_EVENT_OBJECT_TYPE: u64 = 0;
pub static mut EX_SEMAPHORE_OBJECT_TYPE: u64 = 0;
pub static mut IO_DEVICE_OBJECT_TYPE: u64 = 0;
pub static mut IO_FILE_OBJECT_TYPE: u64 = 0;
pub static mut PS_THREAD_TYPE: u64 = 0;
pub static mut PS_PROCESS_TYPE: u64 = 0;
pub static mut KD_DEBUGGER_ENABLED: u8 = 0;
pub static mut KD_DEBUGGER_NOT_PRESENT: u8 = 1;
pub static mut KI_BUGCHECK_DATA: [u64; 5] = [0; 5];

static OBJECT_TYPES: [[u8; 0x100]; 6] = [[0; 0x100]; 6];

pub fn init() {
    unsafe {
        EX_EVENT_OBJECT_TYPE = OBJECT_TYPES[0].as_ptr() as u64;
        EX_SEMAPHORE_OBJECT_TYPE = OBJECT_TYPES[1].as_ptr() as u64;
        IO_DEVICE_OBJECT_TYPE = OBJECT_TYPES[2].as_ptr() as u64;
        IO_FILE_OBJECT_TYPE = OBJECT_TYPES[3].as_ptr() as u64;
        PS_THREAD_TYPE = OBJECT_TYPES[4].as_ptr() as u64;
        PS_PROCESS_TYPE = OBJECT_TYPES[5].as_ptr() as u64;
    }
}

/// Data exports: the address of the variable.
pub fn data_export(name: &str) -> Option<u64> {
    Some(match name {
        "ExEventObjectType" => &raw const EX_EVENT_OBJECT_TYPE as u64,
        "ExSemaphoreObjectType" => &raw const EX_SEMAPHORE_OBJECT_TYPE as u64,
        "IoDeviceObjectType" => &raw const IO_DEVICE_OBJECT_TYPE as u64,
        "IoFileObjectType" => &raw const IO_FILE_OBJECT_TYPE as u64,
        "PsThreadType" => &raw const PS_THREAD_TYPE as u64,
        "PsProcessType" => &raw const PS_PROCESS_TYPE as u64,
        "KdDebuggerEnabled" => &raw const KD_DEBUGGER_ENABLED as u64,
        "KdDebuggerNotPresent" => &raw const KD_DEBUGGER_NOT_PRESENT as u64,
        "KiBugCheckData" => &raw const KI_BUGCHECK_DATA as u64,
        _ => return None,
    })
}
