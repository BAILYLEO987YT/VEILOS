// Device interrupts for Windows drivers: IoConnectInterruptEx and the
// interrupt object calls (KeSynchronizeExecution, interrupt spin locks).
//
// Each PCI device a Windows driver runs gets a vector of its own from
// `interrupts::DRIVER_VECTOR_FIRST` on (`alloc_vector`); pnp.rs points the
// device's MSI (or MSI-X entry 0) at it and reports it in the device's
// translated resources. VEILOS doesn't route legacy INTx lines to drivers:
// a device without MSI gets no interrupt resource.
//
// When the vector fires, `dispatch` runs every service routine connected to
// it at the vector's IRQL (its top nibble, as the local APIC sees it), each
// under its interrupt object's spin lock, then the handler EOIs. A DPC the
// routine queues (IoRequestDpc) runs as soon as IRQL drops (dpc.rs).
//
// A KINTERRUPT is opaque to drivers; here it is a `Kint`.

use alloc::{boxed::Box, vec::Vec};
use spin::Mutex;

use super::{ke, pool, NtStatus, STATUS_INSUFFICIENT_RESOURCES, STATUS_INVALID_PARAMETER, STATUS_SUCCESS};
use crate::arch::interrupts::{DRIVER_VECTORS, DRIVER_VECTOR_FIRST};

const STATUS_NOT_SUPPORTED: NtStatus = 0xC000_00BBu32 as i32;
const STATUS_INVALID_PARAMETER_1: NtStatus = 0xC000_00EFu32 as i32;

// IO_CONNECT_INTERRUPT_PARAMETERS.Version
pub const CONNECT_FULLY_SPECIFIED: u32 = 1;
pub const CONNECT_LINE_BASED: u32 = 2;
pub const CONNECT_MESSAGE_BASED: u32 = 3;
pub const CONNECT_FULLY_SPECIFIED_GROUP: u32 = 4;

/// An interrupt object.
pub struct Kint {
    vector: u8,
    sync_irql: u8,
    routine: u64,
    context: u64,
    /// Some(message id): a message-signaled routine (with the id argument).
    message: Option<u32>,
    /// The driver's spin lock, or `own_lock`.
    lock: *mut u64,
    own_lock: u64,
}

unsafe impl Send for Kint {}

/// Connected interrupt objects, by vector.
static TABLE: Mutex<[Vec<usize>; DRIVER_VECTORS]> = Mutex::new([const { Vec::new() }; DRIVER_VECTORS]);
/// Vectors given to devices.
static GIVEN: Mutex<u64> = Mutex::new(0);
/// Interrupts taken, by vector (the terminal's DRIVER IRQS).
static COUNTS: [core::sync::atomic::AtomicU64; DRIVER_VECTORS] = [const { core::sync::atomic::AtomicU64::new(0) }; DRIVER_VECTORS];

fn locked<R>(f: impl FnOnce() -> R) -> R {
    x86_64::instructions::interrupts::without_interrupts(f)
}

fn slot(vector: u8) -> Option<usize> {
    let i = vector.checked_sub(DRIVER_VECTOR_FIRST)? as usize;
    (i < DRIVER_VECTORS).then_some(i)
}

/// A free vector for a device, or None if all are given out.
pub fn alloc_vector() -> Option<u8> {
    locked(|| {
        let mut given = GIVEN.lock();
        let i = (0..DRIVER_VECTORS).find(|&i| *given & 1 << i == 0)?;
        *given |= 1 << i;
        Some(DRIVER_VECTOR_FIRST + i as u8)
    })
}

pub fn free_vector(vector: u8) {
    if let Some(i) = slot(vector) {
        locked(|| *GIVEN.lock() &= !(1 << i));
    }
}

/// The IRQL a vector's interrupts run at.
pub fn irql_of(vector: u8) -> u8 {
    vector >> 4
}

/// Interrupts taken on `vector` so far.
pub fn count(vector: u8) -> u64 {
    slot(vector).map_or(0, |i| COUNTS[i].load(core::sync::atomic::Ordering::Relaxed))
}

/// From the vector's IDT handler (interrupts off): every connected routine,
/// at the vector's IRQL, under its lock.
pub fn dispatch(vector: u8) {
    let Some(i) = slot(vector) else { return };
    COUNTS[i].fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    let mut list = [0usize; 4];
    let n = {
        let t = TABLE.lock();
        let n = t[i].len().min(list.len());
        list[..n].copy_from_slice(&t[i][..n]);
        n
    };
    if n == 0 {
        return;
    }
    super::with_kernel_gs(|| {
        let old = ke::irql();
        ke::set_irql(irql_of(vector));
        for &k in &list[..n] {
            let k = unsafe { &*(k as *const Kint) };
            spin_acquire(k.lock);
            unsafe {
                match k.message {
                    Some(id) => {
                        let f: extern "win64" fn(*const Kint, u64, u32) -> u8 = core::mem::transmute(k.routine);
                        f(k, k.context, id);
                    }
                    None => {
                        let f: extern "win64" fn(*const Kint, u64) -> u8 = core::mem::transmute(k.routine);
                        f(k, k.context);
                    }
                }
            }
            spin_release(k.lock);
        }
        ke::set_irql(old);
    });
}

fn spin_acquire(lock: *mut u64) {
    let a = unsafe { &*(lock as *const core::sync::atomic::AtomicU64) };
    while a.compare_exchange_weak(0, 1, core::sync::atomic::Ordering::Acquire, core::sync::atomic::Ordering::Relaxed).is_err() {
        core::hint::spin_loop();
    }
}

fn spin_release(lock: *mut u64) {
    unsafe { &*(lock as *const core::sync::atomic::AtomicU64) }.store(0, core::sync::atomic::Ordering::Release);
}

fn connect(vector: u8, routine: u64, context: u64, lock: *mut u64, sync_irql: u8, message: Option<u32>) -> Result<*mut Kint, NtStatus> {
    let i = slot(vector).ok_or(STATUS_INVALID_PARAMETER)?;
    if routine == 0 {
        return Err(STATUS_INVALID_PARAMETER);
    }
    let irql = irql_of(vector);
    let k = Box::into_raw(Box::new(Kint {
        vector,
        sync_irql: sync_irql.max(irql),
        routine,
        context,
        message,
        lock: core::ptr::null_mut(),
        own_lock: 0,
    }));
    unsafe {
        (*k).lock = if lock.is_null() { &raw mut (*k).own_lock } else { lock };
    }
    locked(|| TABLE.lock()[i].push(k as usize));
    crate::serial_println!("[NTOS] interrupt connected: vector {:#x}, IRQL {}{}", vector, irql, if message.is_some() { ", message-signaled" } else { "" });
    Ok(k)
}

fn disconnect(k: *mut Kint) {
    if k.is_null() {
        return;
    }
    let v = unsafe { (*k).vector };
    if let Some(i) = slot(v) {
        locked(|| TABLE.lock()[i].retain(|&x| x != k as usize));
    }
    drop(unsafe { Box::from_raw(k) });
}

// -----| IoConnectInterruptEx |-----

// IO_CONNECT_INTERRUPT_PARAMETERS (checked in k1's layout.c). The three
// forms share their first fields: PhysicalDeviceObject 0x08, the object or
// table pointer 0x10, ServiceRoutine 0x18, ServiceContext 0x20, SpinLock
// 0x28, SynchronizeIrql 0x30. FullySpecified also has Vector 0x34 and Irql
// 0x38; MessageBased has FallBackServiceRoutine 0x38.
const P_PDO: usize = 0x08;
const P_OBJECT: usize = 0x10;
const P_ROUTINE: usize = 0x18;
const P_CONTEXT: usize = 0x20;
const P_LOCK: usize = 0x28;
const P_SYNC_IRQL: usize = 0x30;
const P_VECTOR: usize = 0x34;
const P_FALLBACK: usize = 0x38;

// IO_INTERRUPT_MESSAGE_INFO: UnifiedIrql 0, MessageCount 4, entries from 8,
// each 0x28: MessageAddress 0, TargetProcessorSet 8, InterruptObject 0x10,
// MessageData 0x18, Vector 0x1C, Irql 0x20, Mode 0x24 (and Polarity 0x28 on
// newer headers, which the entry's 0x30 size... see layout.c).
pub const MSG_INFO_HEADER: usize = 8;
pub const MSG_ENTRY_SIZE: usize = 0x30;

unsafe fn rd<T: Copy>(p: *const u8, off: usize) -> T {
    unsafe { (p.add(off) as *const T).read_unaligned() }
}
unsafe fn wr<T>(p: *mut u8, off: usize, v: T) {
    unsafe { (p.add(off) as *mut T).write_unaligned(v) }
}

pub extern "win64" fn IoConnectInterruptEx(params: *mut u8) -> NtStatus {
    if params.is_null() {
        return STATUS_INVALID_PARAMETER;
    }
    let version: u32 = unsafe { rd(params, 0) };
    let (pdo, routine, context, lock, sync) = unsafe {
        (rd::<u64>(params, P_PDO), rd::<u64>(params, P_ROUTINE), rd::<u64>(params, P_CONTEXT), rd::<*mut u64>(params, P_LOCK), rd::<u8>(params, P_SYNC_IRQL))
    };
    match version {
        CONNECT_FULLY_SPECIFIED | CONNECT_FULLY_SPECIFIED_GROUP => {
            let vector: u32 = unsafe { rd(params, P_VECTOR) };
            match connect(vector as u8, routine, context, lock, sync, None) {
                Ok(k) => {
                    unsafe { rd::<*mut *mut Kint>(params, P_OBJECT).write(k) };
                    STATUS_SUCCESS
                }
                Err(s) => s,
            }
        }
        CONNECT_LINE_BASED => {
            // The device's interrupt, whichever kind it is, to a routine
            // without a message id.
            let Some(vector) = super::pnp::vector_of(pdo) else { return STATUS_NOT_SUPPORTED };
            match connect(vector, routine, context, lock, sync, None) {
                Ok(k) => {
                    unsafe { rd::<*mut *mut Kint>(params, P_OBJECT).write(k) };
                    STATUS_SUCCESS
                }
                Err(s) => s,
            }
        }
        CONNECT_MESSAGE_BASED => {
            let Some(vector) = super::pnp::vector_of(pdo) else { return STATUS_NOT_SUPPORTED };
            if !super::pnp::is_message(pdo) {
                // No messages: the fallback line-based routine, as Windows
                // does, reporting LINE_BASED back.
                let fallback: u64 = unsafe { rd(params, P_FALLBACK) };
                return match connect(vector, fallback, context, lock, sync, None) {
                    Ok(k) => unsafe {
                        rd::<*mut *mut Kint>(params, P_OBJECT).write(k);
                        wr::<u32>(params, 0, CONNECT_LINE_BASED);
                        STATUS_SUCCESS
                    },
                    Err(s) => s,
                };
            }
            let k = match connect(vector, routine, context, lock, sync, Some(0)) {
                Ok(k) => k,
                Err(s) => return s,
            };
            let table = pool::alloc_tracked(MSG_INFO_HEADER + MSG_ENTRY_SIZE);
            if table.is_null() {
                disconnect(k);
                return STATUS_INSUFFICIENT_RESOURCES;
            }
            let (addr, data) = super::pnp::message_of(pdo).unwrap_or((0, vector as u32));
            unsafe {
                wr::<u8>(table, 0, irql_of(vector).max(sync));
                wr::<u32>(table, 4, 1);
                let e = table.add(MSG_INFO_HEADER);
                wr::<u64>(e, 0, addr);
                wr::<u64>(e, 8, 1);
                wr(e, 0x10, k);
                wr::<u32>(e, 0x18, data);
                wr::<u32>(e, 0x1C, vector as u32);
                wr::<u32>(e, 0x20, irql_of(vector) as u32);
                wr::<u32>(e, 0x24, 1); // Latched
                rd::<*mut *mut u8>(params, P_OBJECT).write(table);
            }
            STATUS_SUCCESS
        }
        _ => STATUS_INVALID_PARAMETER_1,
    }
}

/// IoDisconnectInterruptEx(IO_DISCONNECT_INTERRUPT_PARAMETERS: Version 0,
/// ConnectionContext 8 as IoConnectInterruptEx left it).
pub extern "win64" fn IoDisconnectInterruptEx(params: *mut u8) {
    if params.is_null() {
        return;
    }
    let (version, ctx): (u32, *mut u8) = unsafe { (rd(params, 0), rd(params, 8)) };
    if ctx.is_null() {
        return;
    }
    if version == CONNECT_MESSAGE_BASED {
        let n: u32 = unsafe { rd(ctx, 4) };
        for m in 0..n as usize {
            disconnect(unsafe { rd::<*mut Kint>(ctx.add(MSG_INFO_HEADER + m * MSG_ENTRY_SIZE), 0x10) });
        }
        pool::free_tracked(ctx);
    } else {
        disconnect(ctx as *mut Kint);
    }
}

/// KeSynchronizeExecution(Interrupt, SynchronizeRoutine, Context): the
/// routine at the interrupt's synchronize IRQL, under its lock.
pub extern "win64" fn KeSynchronizeExecution(k: *mut Kint, routine: u64, context: u64) -> u8 {
    let k = unsafe { &*k };
    let old = ke::KfRaiseIrql(k.sync_irql);
    spin_acquire(k.lock);
    let f: extern "win64" fn(u64) -> u8 = unsafe { core::mem::transmute(routine) };
    let r = f(context);
    spin_release(k.lock);
    ke::KeLowerIrql(old);
    r
}

pub extern "win64" fn KeAcquireInterruptSpinLock(k: *mut Kint) -> u8 {
    let k = unsafe { &*k };
    let old = ke::KfRaiseIrql(k.sync_irql);
    spin_acquire(k.lock);
    old
}

pub extern "win64" fn KeReleaseInterruptSpinLock(k: *mut Kint, old: u8) {
    spin_release(unsafe { (*k).lock });
    ke::KeLowerIrql(old);
}
