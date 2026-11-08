//! Overlapped I/O and completion ports.
//!
//! Files finish before the call returns. A pipe read with nothing to read yet
//! is the one thing that has to wait: a worker thread does the blocking read
//! and completes the OVERLAPPED, so the caller gets ERROR_IO_PENDING as on
//! Windows. A completion port is a queue and a semaphore, in this process.

use core::sync::atomic::{AtomicBool, Ordering};
use nt_abi::{status, IoStatusBlock, NtStatus};

use crate::{check, Bool, Handle, INVALID_HANDLE_VALUE};

const STATUS_PENDING: NtStatus = 0x103;
const STATUS_CANCELLED: NtStatus = 0xC000_0120u32 as NtStatus;
const ERROR_IO_PENDING: u32 = 997;
const ERROR_IO_INCOMPLETE: u32 = 996;
const ERROR_NOT_FOUND: u32 = 1168;
const ERROR_NOT_ENOUGH_MEMORY: u32 = 8;
const WAIT_TIMEOUT: u32 = 258;
const FILE_TYPE_PIPE: u32 = 3;
const SKIP_PORT_ON_SUCCESS: u8 = 1;

const MAX_PORTS: usize = 16;
const MAX_QUEUED: usize = 256;
const MAX_BOUND: usize = 128;
const MAX_PENDING: usize = 64;

#[derive(Clone, Copy)]
struct Packet {
    key: usize,
    overlapped: usize,
    bytes: u32,
    status: NtStatus,
}

const NO_PACKET: Packet = Packet { key: 0, overlapped: 0, bytes: 0, status: 0 };

#[derive(Clone, Copy)]
struct Port {
    handle: Handle,
    queue: [Packet; MAX_QUEUED],
    head: usize,
    len: usize,
}

#[derive(Clone, Copy)]
struct Bound {
    file: Handle,
    port: usize,
    key: usize,
    modes: u8,
}

/// A worker hasn't reached its blocking call yet.
const STARTING: u8 = 0;
/// It's in the blocking call: the only place it's safe to stop it.
const BLOCKED: u8 = 1;
/// CancelIo took the request: the worker must do nothing more with it.
const CANCELLED: u8 = 2;

#[derive(Clone, Copy)]
struct Pending {
    file: Handle,
    overlapped: usize,
    thread: Handle,
    state: u8,
}

const NO_PENDING: Pending = Pending { file: 0, overlapped: 0, thread: 0, state: STARTING };

struct Tables {
    ports: [Port; MAX_PORTS],
    bound: [Bound; MAX_BOUND],
    pending: [Pending; MAX_PENDING],
}

static mut TABLES: Tables = Tables {
    ports: [Port { handle: 0, queue: [NO_PACKET; MAX_QUEUED], head: 0, len: 0 }; MAX_PORTS],
    bound: [Bound { file: 0, port: 0, key: 0, modes: 0 }; MAX_BOUND],
    pending: [NO_PENDING; MAX_PENDING],
};
static LOCK: AtomicBool = AtomicBool::new(false);

fn with<R>(f: impl FnOnce(&mut Tables) -> R) -> R {
    while LOCK.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() {
        core::hint::spin_loop();
    }
    #[allow(static_mut_refs)]
    let r = f(unsafe { &mut TABLES });
    LOCK.store(false, Ordering::Release);
    r
}

/// A handle is closing: a port gives up its slot and its files, and a file its
/// binding, or the next object to get that handle would inherit them.
pub fn closed(h: Handle) {
    with(|t| {
        if let Some(i) = t.ports.iter().position(|p| p.handle == h) {
            t.ports[i].handle = 0;
            t.ports[i].head = 0;
            t.ports[i].len = 0;
            for b in t.bound.iter_mut().filter(|b| b.file != 0 && b.port == i) {
                b.file = 0;
            }
        }
        for b in t.bound.iter_mut().filter(|b| b.file == h) {
            b.file = 0;
        }
    });
}

// -----| OVERLAPPED |-----

/// The offset an OVERLAPPED asks for.
pub fn offset_of(overlapped: usize) -> Option<i64> {
    if overlapped == 0 {
        return None;
    }
    let p = overlapped as *const u32;
    Some(unsafe { (*p.add(5) as i64) << 32 | *p.add(4) as i64 })
}

/// Record the result, set the event, and queue the packet if the file has a
/// port. `sync`: it finished before the call returned.
pub fn complete(file: Handle, overlapped: usize, s: NtStatus, bytes: u64, sync: bool) {
    if overlapped == 0 {
        return;
    }
    let p = overlapped as *mut u64;
    let raw_event = unsafe {
        *p = s as u32 as u64;
        *p.add(1) = bytes;
        *p.add(3)
    };
    let event = raw_event & !1;
    if event != 0 {
        crate::sync::SetEvent(event);
    }
    // The low bit of hEvent asks for no packet.
    if raw_event & 1 != 0 {
        return;
    }
    let target = with(|t| t.bound.iter().find(|b| b.file == file).copied());
    let Some(b) = target else { return };
    // A failure that returned straight away isn't queued either.
    if sync && (b.modes & SKIP_PORT_ON_SUCCESS != 0 || !status::is_success(s)) {
        return;
    }
    post(b.port, Packet { key: b.key, overlapped, bytes: bytes as u32, status: s });
}

fn post(port: usize, packet: Packet) -> bool {
    let handle = with(|t| {
        let q = &mut t.ports[port];
        if q.handle == 0 || q.len == MAX_QUEUED {
            return 0;
        }
        let at = (q.head + q.len) % MAX_QUEUED;
        q.queue[at] = packet;
        q.len += 1;
        q.handle
    });
    if handle == 0 {
        return false;
    }
    unsafe { crate::support::ReleaseSemaphore(handle, 1, core::ptr::null_mut()) };
    true
}

#[repr(C)]
struct ReadJob {
    file: Handle,
    buffer: *mut u8,
    length: u32,
    overlapped: usize,
    /// A ConnectNamedPipe, not a read.
    connect: bool,
    /// Its entry in `pending`.
    slot: usize,
}

unsafe extern "system" fn read_worker(job: *mut ReadJob) -> u32 {
    let j = unsafe { core::ptr::read(job) };
    unsafe { crate::mem::HeapFree(crate::mem::GetProcessHeap(), 0, job as *mut u8) };
    // Cancelled while starting: nothing was begun, and the entry is ours to free.
    let early = with(|t| {
        let p = &mut t.pending[j.slot];
        if p.state == CANCELLED {
            let th = p.thread;
            *p = NO_PENDING;
            Some(th)
        } else {
            p.state = BLOCKED;
            None
        }
    });
    if let Some(th) = early {
        if th != 0 {
            crate::CloseHandle(th);
        }
        return 0;
    }
    let mut iosb = IoStatusBlock::default();
    let s = if j.connect {
        // STATUS_PIPE_CONNECTED counts as connecting: a client came.
        match unsafe { crate::VeilPipe(2, j.file, 0, 0) } {
            s if s == 0xC000_00B2u32 as NtStatus => status::SUCCESS,
            s => s,
        }
    } else {
        unsafe { crate::NtReadFile(j.file, 0, 0, 0, &mut iosb, j.buffer, j.length, core::ptr::null(), core::ptr::null()) }
    };
    // Gone from the table first: once complete, the caller may reuse it all.
    let thread = with(|t| {
        let p = &mut t.pending[j.slot];
        if p.state == CANCELLED {
            None
        } else {
            let th = p.thread;
            *p = NO_PENDING;
            Some(th)
        }
    });
    // Cancelled just as it finished: it's being stopped, so it holds nothing
    // and touches nothing until then.
    let Some(th) = thread else {
        loop {
            crate::sync::SwitchToThread();
        }
    };
    complete(j.file, j.overlapped, s, iosb.information, false);
    if th != 0 {
        crate::CloseHandle(th);
    }
    0
}

/// A read on a pipe with nothing waiting: a worker does it. None if this
/// isn't such a read; otherwise it's pending, or couldn't start.
pub fn read_pending(file: Handle, buffer: *mut u8, length: u32, overlapped: usize) -> Option<Bool> {
    if overlapped == 0 || crate::file::GetFileType(file) != FILE_TYPE_PIPE {
        return None;
    }
    let mut avail = 0u32;
    if unsafe { crate::VeilPipe(4, file, &mut avail as *mut u32 as u64, 0) } != 0 || avail != 0 {
        return None;
    }
    Some(start_worker(ReadJob { file, buffer, length, overlapped, connect: false, slot: 0 }))
}

/// ConnectNamedPipe with an OVERLAPPED and no client yet: a worker waits for one.
pub fn connect_pending(file: Handle, overlapped: usize) -> Bool {
    start_worker(ReadJob { file, buffer: core::ptr::null_mut(), length: 0, overlapped, connect: true, slot: 0 })
}

/// Hand `work` to a worker thread; FALSE with ERROR_IO_PENDING once it's going.
fn start_worker(mut work: ReadJob) -> Bool {
    let (file, overlapped) = (work.file, work.overlapped);
    let job = unsafe { crate::mem::HeapAlloc(crate::mem::GetProcessHeap(), 0, core::mem::size_of::<ReadJob>()) } as *mut ReadJob;
    if job.is_null() {
        crate::SetLastError(ERROR_NOT_ENOUGH_MEMORY);
        return 0;
    }
    // Reserved before the thread starts, so it always finds its entry.
    let slot = with(|t| {
        t.pending.iter().position(|p| p.overlapped == 0).inspect(|&i| {
            t.pending[i] = Pending { file, overlapped, thread: 0, state: STARTING };
        })
    });
    let Some(slot) = slot else {
        unsafe { crate::mem::HeapFree(crate::mem::GetProcessHeap(), 0, job as *mut u8) };
        crate::SetLastError(ERROR_NOT_ENOUGH_MEMORY);
        return 0;
    };
    work.slot = slot;
    unsafe {
        core::ptr::write(job, work);
        *(overlapped as *mut u64) = STATUS_PENDING as u32 as u64;
        *(overlapped as *mut u64).add(1) = 0;
    }
    // Suspended until the table has its handle, so a fast read can't miss it.
    let th = unsafe { crate::process::CreateThread(0, 64 << 10, read_worker as *const () as u64, job as u64, 4, core::ptr::null_mut()) };
    if th == 0 {
        with(|t| t.pending[slot] = NO_PENDING);
        unsafe { crate::mem::HeapFree(crate::mem::GetProcessHeap(), 0, job as *mut u8) };
        return 0;
    }
    with(|t| t.pending[slot].thread = th);
    unsafe { crate::process::ResumeThread(th) };
    crate::SetLastError(ERROR_IO_PENDING);
    0
}

#[no_mangle]
pub unsafe extern "system" fn GetOverlappedResult(_file: Handle, overlapped: *const u64, done: *mut u32, wait: Bool) -> Bool {
    let current = || unsafe { core::ptr::read_volatile(overlapped) } as u32 as NtStatus;
    if current() == STATUS_PENDING {
        if wait == 0 {
            crate::SetLastError(ERROR_IO_INCOMPLETE);
            return 0;
        }
        let event = unsafe { *overlapped.add(3) } & !1;
        while current() == STATUS_PENDING {
            if event != 0 {
                crate::sync::WaitForSingleObject(event, 50);
            } else {
                crate::sync::SwitchToThread();
            }
        }
    }
    if !done.is_null() {
        unsafe { *done = *overlapped.add(1) as u32 };
    }
    check(current())
}

#[no_mangle]
pub unsafe extern "system" fn GetOverlappedResultEx(file: Handle, overlapped: *const u64, done: *mut u32, _ms: u32, _alertable: Bool) -> Bool {
    unsafe { GetOverlappedResult(file, overlapped, done, 1) }
}

/// Stop what's waiting on `file` (just `only`, if given).
fn cancel(file: Handle, only: usize) -> bool {
    let mut any = false;
    for i in 0..MAX_PENDING {
        let taken = with(|t| {
            let p = &mut t.pending[i];
            if p.overlapped != 0 && p.file == file && p.state != CANCELLED && (only == 0 || p.overlapped == only) {
                let was = *p;
                p.state = CANCELLED;
                Some(was)
            } else {
                None
            }
        });
        let Some(p) = taken else { continue };
        any = true;
        // Stopped only in its blocking call. A thread stopped anywhere else
        // could be holding the heap's lock or the loader's, for good. One
        // still starting sees CANCELLED, and ends by itself.
        if p.state == BLOCKED {
            if p.thread != 0 {
                unsafe {
                    crate::process::TerminateThread(p.thread, 0);
                    crate::sync::WaitForSingleObject(p.thread, 1000);
                    crate::CloseHandle(p.thread);
                }
            }
            with(|t| t.pending[i] = NO_PENDING);
        }
        complete(file, p.overlapped, STATUS_CANCELLED, 0, false);
    }
    any
}

#[no_mangle]
pub extern "system" fn CancelIo(file: Handle) -> Bool {
    cancel(file, 0);
    1
}

#[no_mangle]
pub extern "system" fn CancelIoEx(file: Handle, overlapped: usize) -> Bool {
    if cancel(file, overlapped) {
        1
    } else {
        crate::SetLastError(ERROR_NOT_FOUND);
        0
    }
}

#[no_mangle]
pub extern "system" fn CancelSynchronousIo(_thread: Handle) -> Bool {
    crate::SetLastError(ERROR_NOT_FOUND);
    0
}

// -----| completion ports |-----

#[no_mangle]
pub unsafe extern "system" fn CreateIoCompletionPort(file: Handle, existing: Handle, key: usize, _threads: u32) -> Handle {
    let port = if existing != 0 {
        match with(|t| t.ports.iter().position(|p| p.handle == existing)) {
            Some(i) => i,
            None => {
                crate::SetLastError(6);
                return 0;
            }
        }
    } else {
        let h = unsafe { crate::support::CreateSemaphoreExW(0, 0, 0x7FFF_FFFF, core::ptr::null(), 0, 0) };
        if h == 0 {
            return 0;
        }
        match with(|t| {
            t.ports.iter().position(|p| p.handle == 0).inspect(|&i| {
                t.ports[i].handle = h;
                t.ports[i].head = 0;
                t.ports[i].len = 0;
            })
        }) {
            Some(i) => i,
            None => {
                crate::CloseHandle(h);
                crate::SetLastError(ERROR_NOT_ENOUGH_MEMORY);
                return 0;
            }
        }
    };
    if file != INVALID_HANDLE_VALUE {
        let ok = with(|t| match t.bound.iter().position(|b| b.file == 0 || b.file == file) {
            Some(i) => {
                t.bound[i] = Bound { file, port, key, modes: 0 };
                true
            }
            None => false,
        });
        if !ok {
            crate::SetLastError(ERROR_NOT_ENOUGH_MEMORY);
            return 0;
        }
    }
    with(|t| t.ports[port].handle)
}

#[no_mangle]
pub extern "system" fn SetFileCompletionNotificationModes(file: Handle, modes: u8) -> Bool {
    with(|t| {
        if let Some(b) = t.bound.iter_mut().find(|b| b.file == file) {
            b.modes = modes;
        }
    });
    1
}

fn take(port: Handle) -> Option<Packet> {
    with(|t| {
        let q = t.ports.iter_mut().find(|p| p.handle == port)?;
        if q.len == 0 {
            return None;
        }
        let packet = q.queue[q.head];
        q.head = (q.head + 1) % MAX_QUEUED;
        q.len -= 1;
        Some(packet)
    })
}

#[no_mangle]
pub unsafe extern "system" fn GetQueuedCompletionStatus(port: Handle, bytes: *mut u32, key: *mut usize, overlapped: *mut usize, ms: u32) -> Bool {
    unsafe { *overlapped = 0 };
    if crate::sync::WaitForSingleObject(port, ms) == WAIT_TIMEOUT {
        crate::SetLastError(WAIT_TIMEOUT);
        return 0;
    }
    let Some(p) = take(port) else {
        crate::SetLastError(WAIT_TIMEOUT);
        return 0;
    };
    unsafe {
        *bytes = p.bytes;
        *key = p.key;
        *overlapped = p.overlapped;
    }
    check(p.status)
}

/// OVERLAPPED_ENTRY: key, overlapped, internal, bytes.
#[no_mangle]
pub unsafe extern "system" fn GetQueuedCompletionStatusEx(port: Handle, entries: *mut usize, count: u32, removed: *mut u32, ms: u32, _alertable: Bool) -> Bool {
    let mut n = 0u32;
    while n < count {
        if crate::sync::WaitForSingleObject(port, if n == 0 { ms } else { 0 }) == WAIT_TIMEOUT {
            break;
        }
        let Some(p) = take(port) else { break };
        unsafe {
            let e = entries.add(n as usize * 4);
            *e = p.key;
            *e.add(1) = p.overlapped;
            *e.add(2) = p.status as u32 as usize;
            *e.add(3) = p.bytes as usize;
        }
        n += 1;
    }
    unsafe { *removed = n };
    if n == 0 {
        crate::SetLastError(WAIT_TIMEOUT);
        return 0;
    }
    1
}

#[no_mangle]
pub extern "system" fn PostQueuedCompletionStatus(port: Handle, bytes: u32, key: usize, overlapped: usize) -> Bool {
    match with(|t| t.ports.iter().position(|p| p.handle == port)) {
        Some(i) if post(i, Packet { key, overlapped, bytes, status: status::SUCCESS }) => 1,
        Some(_) => {
            crate::SetLastError(ERROR_NOT_ENOUGH_MEMORY);
            0
        }
        None => {
            crate::SetLastError(6);
            0
        }
    }
}
