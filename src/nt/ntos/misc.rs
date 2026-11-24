// The rest of the census's smaller pieces: kernel queues (KQUEUE) and other
// Ke calls, process/thread queries and notifications (Ps), power (Po),
// tracing (Etw), registry callbacks (Cm), and Mm odds and ends.
//
// Registrations VEILOS has no events for yet (process and image-load
// notifications, power settings, registry callbacks, ETW providers) are
// accepted and never called, which is what a driver needs to start; each is
// marked where it is.

use alloc::{collections::BTreeMap, vec::Vec};
use core::sync::atomic::{AtomicU64, Ordering};
use spin::Mutex;

use super::{ke, mm, pool, NtStatus, STATUS_SUCCESS, STATUS_TIMEOUT};

const STATUS_NOT_SUPPORTED: NtStatus = 0xC000_00BBu32 as i32;
const STATUS_INVALID_PARAMETER: NtStatus = 0xC000_000Du32 as i32;

static NEXT_HANDLE: AtomicU64 = AtomicU64::new(0x1000);

fn new_cookie() -> u64 {
    NEXT_HANDLE.fetch_add(0x10, Ordering::Relaxed)
}

// -----| Ke: queues |-----

/// KQUEUE entries, kept here (the queue's own list head isn't used).
static QUEUES: Mutex<BTreeMap<usize, Vec<u64>>> = Mutex::new(BTreeMap::new());

fn q<R>(f: impl FnOnce(&mut BTreeMap<usize, Vec<u64>>) -> R) -> R {
    x86_64::instructions::interrupts::without_interrupts(|| f(&mut QUEUES.lock()))
}

pub extern "win64" fn KeInitializeQueue(queue: *mut u8, _count: u32) {
    unsafe {
        core::ptr::write_bytes(queue, 0, 0x40);
        queue.write(4); // QueueObject
    }
    q(|m| m.insert(queue as usize, Vec::new()));
}

fn insert(queue: *mut u8, entry: u64, head: bool) -> i32 {
    q(|m| {
        let v = m.entry(queue as usize).or_default();
        let before = v.len() as i32;
        if head { v.insert(0, entry) } else { v.push(entry) }
        before
    })
}

pub extern "win64" fn KeInsertQueue(queue: *mut u8, entry: u64) -> i32 {
    insert(queue, entry, false)
}

pub extern "win64" fn KeInsertHeadQueue(queue: *mut u8, entry: u64) -> i32 {
    insert(queue, entry, true)
}

/// KeRemoveQueue(Queue, WaitMode, Timeout*): the next entry, or
/// STATUS_TIMEOUT as the pointer.
pub extern "win64" fn KeRemoveQueue(queue: *mut u8, _mode: u8, timeout: *const i64) -> u64 {
    let mut got = 0;
    let st = ke::wait_for(
        || {
            got = q(|m| m.get_mut(&(queue as usize)).and_then(|v| if v.is_empty() { None } else { Some(v.remove(0)) })).unwrap_or(0);
            got != 0
        },
        timeout,
    );
    if st == STATUS_SUCCESS { got } else { STATUS_TIMEOUT as u64 }
}

/// KeRundownQueue(Queue): what's left, linked into a ring through their
/// LIST_ENTRYs (null if none), and the queue emptied.
pub extern "win64" fn KeRundownQueue(queue: *mut u8) -> u64 {
    let v = q(|m| m.get_mut(&(queue as usize)).map(core::mem::take).unwrap_or_default());
    if v.is_empty() {
        return 0;
    }
    for (i, &e) in v.iter().enumerate() {
        let next = v[(i + 1) % v.len()];
        let prev = v[(i + v.len() - 1) % v.len()];
        unsafe {
            (e as *mut u64).write(next);
            (e as *mut u64).add(1).write(prev);
        }
    }
    v[0]
}

// -----| Ke: everything else |-----

pub extern "win64" fn KePulseEvent(ev: *mut u8, inc: i32, wait: u8) -> i32 {
    let prev = ke::KeSetEvent(ev, inc, wait);
    ke::KeResetEvent(ev);
    prev
}

pub extern "win64" fn KeReadStateSemaphore(sem: *mut u8) -> i32 {
    unsafe { (sem.add(4) as *const i32).read_volatile() }
}

pub extern "win64" fn KeQueryActiveProcessors() -> u64 {
    crate::arch::smp::CPU_ONLINE.load(Ordering::Relaxed)
}

pub extern "win64" fn KeQueryMaximumProcessorCount() -> u32 {
    crate::arch::smp::CPU_ONLINE.load(Ordering::Relaxed).count_ones()
}

pub extern "win64" fn KeQueryMaximumProcessorCountEx(_group: u16) -> u32 {
    KeQueryMaximumProcessorCount()
}

/// Processor topology isn't described yet: drivers fall back.
pub extern "win64" fn KeQueryLogicalProcessorRelationship(_proc: *const u8, _rel: u32, _info: *mut u8, _len: *mut u32) -> NtStatus {
    STATUS_NOT_SUPPORTED
}

/// VEILOS's kernel threads have no priorities: everyone is "normal" (8).
pub extern "win64" fn KeQueryPriorityThread(_thread: u64) -> i32 {
    8
}

pub extern "win64" fn KeSetPriorityThread(_thread: u64, _priority: i32) -> i32 {
    8
}

pub extern "win64" fn KeQueryUnbiasedInterruptTime() -> u64 {
    ke::KeQueryInterruptTime()
}

pub extern "win64" fn KeInvalidateAllCaches() -> u8 {
    unsafe { core::arch::asm!("wbinvd", options(nostack)) };
    1
}

/// Kernel code may use the FPU/SSE/AVX between these on Windows. VEILOS
/// saves that state on every thread switch and its interrupt handlers
/// don't touch the upper YMM halves, so there is nothing to save.
pub extern "win64" fn KeSaveExtendedProcessorState(_mask: u64, _save: *mut u8) -> NtStatus {
    STATUS_SUCCESS
}

pub extern "win64" fn KeRestoreExtendedProcessorState(_save: *mut u8) {}

/// Bug check callbacks: accepted, not called (VEILOS's crash screen and log
/// don't take contributions yet).
pub extern "win64" fn KeRegisterBugCheckReasonCallback(_rec: *mut u8, _routine: u64, _reason: u32, _component: *const u8) -> u8 {
    1
}

pub extern "win64" fn KeDeregisterBugCheckReasonCallback(_rec: *mut u8) -> u8 {
    1
}

/// APCs aren't delivered in VEILOS's kernel: an APC is never queued.
pub extern "win64" fn KeInitializeApc(apc: *mut u8, _thread: u64, _env: u32, _kernel: u64, _rundown: u64, _normal: u64, _mode: u8, _ctx: u64) {
    unsafe { core::ptr::write_bytes(apc, 0, 0x58) };
}

pub extern "win64" fn KeInsertQueueApc(_apc: *mut u8, _a1: u64, _a2: u64, _inc: i32) -> u8 {
    0
}

/// Attaching to a process's address space: drivers in VEILOS run in the
/// kernel's, whose upper half every process shares.
pub extern "win64" fn KeStackAttachProcess(_process: u64, _state: *mut u8) {}
pub extern "win64" fn KeUnstackDetachProcess(_state: *mut u8) {}

pub extern "win64" fn KdRefreshDebuggerNotPresent() -> u8 {
    1
}

// -----| Ps |-----

pub extern "win64" fn PsGetVersion(major: *mut u32, minor: *mut u32, build: *mut u32, _csd: *mut u8) -> u8 {
    unsafe {
        if !major.is_null() {
            major.write(10);
        }
        if !minor.is_null() {
            minor.write(0);
        }
        if !build.is_null() {
            build.write(26100);
        }
    }
    0 // not a checked build
}

pub extern "win64" fn PsGetProcessId(_process: u64) -> u64 {
    super::io::PsGetCurrentProcessId()
}

pub extern "win64" fn PsLookupProcessByProcessId(pid: u64, out: *mut u64) -> NtStatus {
    if pid == 0 {
        return STATUS_INVALID_PARAMETER;
    }
    unsafe { out.write(super::io::IoGetCurrentProcess() as u64) };
    STATUS_SUCCESS
}

pub extern "win64" fn PsGetProcessCreateTimeQuadPart(_process: u64) -> i64 {
    0
}

// Notifications: accepted; VEILOS doesn't report process or image events
// to drivers yet.
pub extern "win64" fn PsSetCreateProcessNotifyRoutineEx(_routine: u64, _remove: u8) -> NtStatus {
    STATUS_SUCCESS
}
pub extern "win64" fn PsSetCreateThreadNotifyRoutine(_routine: u64) -> NtStatus {
    STATUS_SUCCESS
}
pub extern "win64" fn PsRemoveCreateThreadNotifyRoutine(_routine: u64) -> NtStatus {
    STATUS_SUCCESS
}
pub extern "win64" fn PsSetLoadImageNotifyRoutine(_routine: u64) -> NtStatus {
    STATUS_SUCCESS
}
pub extern "win64" fn PsRemoveLoadImageNotifyRoutine(_routine: u64) -> NtStatus {
    STATUS_SUCCESS
}

/// Tokens: one stand-in, an administrator's (VEILOS's driver layer runs as
/// the system).
static TOKEN: [u8; 0x100] = [0; 0x100];

pub extern "win64" fn PsReferencePrimaryToken(_process: u64) -> u64 {
    TOKEN.as_ptr() as u64
}
pub extern "win64" fn PsDereferencePrimaryToken(_token: u64) {}
pub extern "win64" fn PsReferenceImpersonationToken(_thread: u64, copy: *mut u8, effective: *mut u8, level: *mut u32) -> u64 {
    unsafe {
        if !copy.is_null() {
            copy.write(0);
        }
        if !effective.is_null() {
            effective.write(0);
        }
        if !level.is_null() {
            level.write(0);
        }
    }
    0
}
pub extern "win64" fn PsDereferenceImpersonationToken(_token: u64) {}
pub extern "win64" fn SeTokenIsAdmin(_token: u64) -> u8 {
    1
}
pub extern "win64" fn SeQueryInformationToken(_token: u64, _class: u32, _info: *mut u64) -> NtStatus {
    STATUS_NOT_SUPPORTED
}
pub extern "win64" fn PsChargePoolQuota(_process: u64, _pool: u32, _amount: usize) {}
pub extern "win64" fn PsReturnPoolQuota(_process: u64, _pool: u32, _amount: usize) {}

// -----| Po, Cm, Etw |-----

/// Power setting callbacks: accepted, not called (no power events yet).
pub extern "win64" fn PoRegisterPowerSettingCallback(_dev: u64, _guid: *const u8, _cb: u64, _ctx: u64, handle: *mut u64) -> NtStatus {
    if !handle.is_null() {
        unsafe { handle.write(new_cookie()) };
    }
    STATUS_SUCCESS
}
pub extern "win64" fn PoUnregisterPowerSettingCallback(_handle: u64) -> NtStatus {
    STATUS_SUCCESS
}

/// PoSetPowerState(DeviceObject, Type, State): the previous state; VEILOS
/// doesn't keep one, so "working" (D0, S0: 1).
pub extern "win64" fn PoSetPowerState(_dev: u64, _kind: u32, _state: u32) -> u32 {
    1
}

/// Registry callbacks: accepted, not called.
pub extern "win64" fn CmRegisterCallbackEx(_routine: u64, _altitude: *const u8, _driver: u64, _ctx: u64, cookie: *mut u64, _reserved: u64) -> NtStatus {
    if !cookie.is_null() {
        unsafe { cookie.write(new_cookie()) };
    }
    STATUS_SUCCESS
}
pub extern "win64" fn CmUnRegisterCallback(_cookie: u64) -> NtStatus {
    STATUS_SUCCESS
}

/// ETW: a provider registers and writes; nothing is listening.
pub extern "win64" fn EtwRegister(_guid: *const u8, _cb: u64, _ctx: u64, handle: *mut u64) -> NtStatus {
    if !handle.is_null() {
        unsafe { handle.write(new_cookie()) };
    }
    STATUS_SUCCESS
}
pub extern "win64" fn EtwUnregister(_handle: u64) -> NtStatus {
    STATUS_SUCCESS
}
pub extern "win64" fn EtwProviderEnabled(_handle: u64, _level: u8, _keyword: u64) -> u8 {
    0
}
pub extern "win64" fn EtwSetInformation(_handle: u64, _class: u32, _info: *const u8, _len: u32) -> NtStatus {
    STATUS_SUCCESS
}
pub extern "win64" fn EtwWrite(_handle: u64, _desc: *const u8, _activity: *const u8, _count: u32, _data: *const u8) -> NtStatus {
    STATUS_SUCCESS
}
pub extern "win64" fn EtwWriteTransfer(_handle: u64, _desc: *const u8, _activity: *const u8, _related: *const u8, _count: u32, _data: *const u8) -> NtStatus {
    STATUS_SUCCESS
}

// -----| Mm odds and ends |-----

pub extern "win64" fn MmIsDriverVerifying(_driver: u64) -> u32 {
    0
}

/// Pageable sections: VEILOS pages nothing out, so they're always in.
pub extern "win64" fn MmLockPagableDataSection(addr: u64) -> u64 {
    addr
}
pub extern "win64" fn MmLockPagableSectionByHandle(_handle: u64) {}
pub extern "win64" fn MmUnlockPagableImageSection(_handle: u64) {}
pub extern "win64" fn MmUnsecureVirtualMemory(_handle: u64) {}

/// PHYSICAL_MEMORY_RANGE { BaseAddress, NumberOfBytes } entries, ended by
/// a zero one, in pool (the caller frees it). VEILOS gives one range: all
/// of RAM it manages, from 0.
pub extern "win64" fn MmGetPhysicalMemoryRanges() -> *mut u64 {
    let r = pool::alloc_tracked(32) as *mut u64;
    if !r.is_null() {
        unsafe {
            r.write(0);
            r.add(1).write(crate::mm::memory::total_frame_count() as u64 * 4096);
        }
    }
    r
}

/// MmCopyMemory(Target, Source (MM_COPY_ADDRESS), Bytes, Flags,
/// Transferred*): Flags 1 physical, 2 virtual.
pub extern "win64" fn MmCopyMemory(target: *mut u8, source: u64, bytes: usize, flags: u32, done: *mut usize) -> NtStatus {
    let from = if flags & 1 != 0 { crate::mm::memory::get_phys_mem_offset().as_u64() + source } else { source };
    if flags & 2 != 0 && mm::phys_of(source).is_none() {
        unsafe { done.write(0) };
        return 0xC000_0005u32 as i32;
    }
    unsafe {
        core::ptr::copy_nonoverlapping(from as *const u8, target, bytes);
        done.write(bytes);
    }
    STATUS_SUCCESS
}

/// Reserved mappings: address space set aside now, pages mapped into it
/// later (MmMapLockedPagesWithReservedMapping).
pub extern "win64" fn MmAllocateMappingAddress(bytes: usize, _tag: u32) -> u64 {
    pool::system_va(bytes as u64).unwrap_or(0)
}
pub extern "win64" fn MmFreeMappingAddress(_va: u64, _tag: u32) {}

pub extern "win64" fn MmMapLockedPagesWithReservedMapping(va: u64, _tag: u32, mdl: *mut u8, cache: u32) -> u64 {
    mm::map_mdl_at(mdl, va, cache)
}

pub extern "win64" fn MmUnmapReservedMapping(va: u64, _tag: u32, mdl: *mut u8) {
    mm::MmUnmapLockedPages(va, mdl)
}

// -----| SeExports |-----
//
// SE_EXPORTS: 23 privilege LUIDs (SeCreateTokenPrivilege = 2 ...
// SeRemoteShutdownPrivilege = 24), then PSIDs from 0xB8: SeNullSid,
// SeWorldSid, SeLocalSid, SeCreatorOwnerSid, SeCreatorGroupSid,
// SeNtAuthoritySid, SeDialupSid, SeNetworkSid, SeBatchSid,
// SeInteractiveSid, SeLocalSystemSid (0x108), SeAliasAdminsSid,
// SeAliasUsersSid, SeAliasGuestsSid, SeAliasPowerUsersSid,
// SeAliasAccountOpsSid, SeAliasSystemOpsSid, SeAliasPrintOpsSid,
// SeAliasBackupOpsSid, SeAuthenticatedUsersSid (0x150), ... (layout.c
// checks these offsets). Later fields stay zero.

static mut SE_EXPORTS: [u64; 0x80] = [0; 0x80];
/// The exported variable: drivers declare it `extern NTKERNELAPI
/// PSE_EXPORTS SeExports` (dllimport), so its value is the structure's
/// address. (Plainly declared data exports, `extern POBJECT_TYPE
/// *ExEventObjectType`, bind to the exported address itself; ex.rs.)
static mut SE_EXPORTS_PTR: u64 = 0;

/// Well-known SIDs: revision 1, count, authority (6 bytes), sub-authorities.
static SIDS: [[u8; 16]; 20] = {
    const fn sid(auth: u8, subs: &[u32]) -> [u8; 16] {
        let mut s = [0u8; 16];
        s[0] = 1;
        s[1] = subs.len() as u8;
        s[7] = auth;
        let mut i = 0;
        while i < subs.len() {
            let b = subs[i].to_le_bytes();
            s[8 + 4 * i] = b[0];
            s[9 + 4 * i] = b[1];
            s[10 + 4 * i] = b[2];
            s[11 + 4 * i] = b[3];
            i += 1;
        }
        s
    }
    [
        sid(0, &[0]),         // Null S-1-0-0
        sid(1, &[0]),         // World S-1-1-0
        sid(2, &[0]),         // Local S-1-2-0
        sid(3, &[0]),         // Creator owner S-1-3-0
        sid(3, &[1]),         // Creator group S-1-3-1
        sid(5, &[]),          // NT authority S-1-5
        sid(5, &[1]),         // Dialup
        sid(5, &[2]),         // Network
        sid(5, &[3]),         // Batch
        sid(5, &[4]),         // Interactive
        sid(5, &[18]),        // Local system
        sid(5, &[32, 544]),   // Administrators
        sid(5, &[32, 545]),   // Users
        sid(5, &[32, 546]),   // Guests
        sid(5, &[32, 547]),   // Power users
        sid(5, &[32, 548]),   // Account operators
        sid(5, &[32, 549]),   // System operators
        sid(5, &[32, 550]),   // Print operators
        sid(5, &[32, 551]),   // Backup operators
        sid(5, &[11]),        // Authenticated users
    ]
};

pub fn init() {
    unsafe {
        let e = &mut *(&raw mut SE_EXPORTS);
        for (i, slot) in e.iter_mut().take(23).enumerate() {
            *slot = i as u64 + 2;
        }
        for (k, s) in SIDS.iter().enumerate() {
            e[23 + k] = s.as_ptr() as u64;
        }
        SE_EXPORTS_PTR = e.as_ptr() as u64;
    }
}

pub fn data_export(name: &str) -> Option<u64> {
    (name == "SeExports").then(|| &raw const SE_EXPORTS_PTR as u64)
}

// -----| netio.sys: the network module registrar |-----
//
// Drivers that offer a network module (NVIDIA's for its network features)
// register a provider; no client attaches in VEILOS, so it never gets
// called.

pub extern "win64" fn NmrRegisterProvider(_chars: *const u8, _ctx: u64, handle: *mut u64) -> NtStatus {
    if !handle.is_null() {
        unsafe { handle.write(new_cookie()) };
    }
    STATUS_SUCCESS
}
pub extern "win64" fn NmrDeregisterProvider(_handle: u64) -> NtStatus {
    0x103 // STATUS_PENDING: done when NmrWaitForProviderDeregisterComplete returns
}
pub extern "win64" fn NmrWaitForProviderDeregisterComplete(_handle: u64) -> NtStatus {
    STATUS_SUCCESS
}
pub extern "win64" fn NmrProviderDetachClientComplete(_binding: u64) -> NtStatus {
    STATUS_SUCCESS
}

// -----| hal.dll: PCI configuration space |-----

/// HalGetBusDataByOffset(BusDataType, BusNumber, SlotNumber, Buffer,
/// Offset, Length): PCIConfiguration (4) only. SlotNumber is a
/// PCI_SLOT_NUMBER: device in bits 0-4, function in 5-7. Bytes read.
pub extern "win64" fn HalGetBusDataByOffset(kind: u32, bus: u32, slot: u32, buf: *mut u8, offset: u32, len: u32) -> u32 {
    if kind != 4 || bus > 255 {
        return 0;
    }
    let a = crate::hal::pci::PciAddress::new(bus as u8, (slot & 0x1F) as u8, ((slot >> 5) & 7) as u8);
    if offset == 0 && len >= 2 && a.read_u16(0) == 0xFFFF {
        // No device there: Windows writes PCI_INVALID_VENDORID and reports 2.
        unsafe { (buf as *mut u16).write_unaligned(0xFFFF) };
        return 2;
    }
    let n = len.min(256u32.saturating_sub(offset));
    for i in 0..n {
        unsafe { buf.add(i as usize).write(a.read_u8((offset + i) as u8)) };
    }
    n
}

pub extern "win64" fn HalSetBusDataByOffset(kind: u32, bus: u32, slot: u32, buf: *const u8, offset: u32, len: u32) -> u32 {
    if kind != 4 || bus > 255 {
        return 0;
    }
    let a = crate::hal::pci::PciAddress::new(bus as u8, (slot & 0x1F) as u8, ((slot >> 5) & 7) as u8);
    let n = len.min(256u32.saturating_sub(offset));
    for i in 0..n {
        a.write_u8((offset + i) as u8, unsafe { buf.add(i as usize).read() });
    }
    n
}
