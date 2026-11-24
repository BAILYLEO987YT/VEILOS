// The rest of what PnP drivers call (WDDM_DESIGN.md, stage W2): remove
// locks, the error log, named events, I/O timers, stack limits, device and
// driver registry keys, device interfaces and PnP notifications, power
// requests, and a few object manager and file system calls.

use alloc::{boxed::Box, format, string::String, vec::Vec};
use spin::Mutex;

use super::{io, ke, ob, pool, rtl, NtStatus, STATUS_INVALID_PARAMETER, STATUS_NO_MEMORY, STATUS_OBJECT_NAME_NOT_FOUND, STATUS_PENDING, STATUS_SUCCESS};

const STATUS_DELETE_PENDING: NtStatus = 0xC000_0056u32 as i32;
const STATUS_OBJECT_NAME_EXISTS: NtStatus = 0x4000_0000;
const STATUS_INVALID_DEVICE_REQUEST: NtStatus = 0xC000_0010u32 as i32;

unsafe fn rd<T: Copy>(p: *const u8, off: usize) -> T {
    unsafe { (p.add(off) as *const T).read_unaligned() }
}
unsafe fn wr<T>(p: *mut u8, off: usize, v: T) {
    unsafe { (p.add(off) as *mut T).write_unaligned(v) }
}

fn locked<R>(f: impl FnOnce() -> R) -> R {
    x86_64::instructions::interrupts::without_interrupts(f)
}

/// A UNICODE_STRING whose buffer is in pool (freed with RtlFreeUnicodeString).
fn put_unicode(us: *mut u8, s: &str) -> NtStatus {
    let units: Vec<u16> = s.encode_utf16().chain(core::iter::once(0)).collect();
    let buf = pool::alloc_tracked(units.len() * 2);
    if buf.is_null() {
        return STATUS_NO_MEMORY;
    }
    unsafe {
        core::ptr::copy_nonoverlapping(units.as_ptr(), buf as *mut u16, units.len());
        wr::<u16>(us, 0, ((units.len() - 1) * 2) as u16);
        wr::<u16>(us, 2, (units.len() * 2) as u16);
        wr(us, 8, buf);
    }
    STATUS_SUCCESS
}

fn guid_string(g: &[u8; 16]) -> String {
    let d1 = u32::from_le_bytes([g[0], g[1], g[2], g[3]]);
    let d2 = u16::from_le_bytes([g[4], g[5]]);
    let d3 = u16::from_le_bytes([g[6], g[7]]);
    format!(
        "{{{:08x}-{:04x}-{:04x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}}}",
        d1, d2, d3, g[8], g[9], g[10], g[11], g[12], g[13], g[14], g[15]
    )
}

fn guid_at(p: *const u8) -> [u8; 16] {
    unsafe { core::ptr::read_unaligned(p as *const [u8; 16]) }
}

/// A handle to the registry key at `path` (made if missing).
fn key_handle(path: &str, out: *mut u64) -> NtStatus {
    if crate::nt::registry::kernel_key(path, true).is_none() {
        return STATUS_OBJECT_NAME_NOT_FOUND;
    }
    unsafe { out.write(ob::insert(ob::Object::Key(String::from(path)))) };
    STATUS_SUCCESS
}

// -----| remove locks |-----

// IO_REMOVE_LOCK (free build: IO_REMOVE_LOCK_COMMON_BLOCK only): Removed 0,
// IoCount 4, RemoveEvent (a KEVENT) 8. Checked in k1's layout.c.
const RL_REMOVED: usize = 0;
const RL_COUNT: usize = 4;
const RL_EVENT: usize = 8;

fn rl_count(lock: *mut u8) -> &'static core::sync::atomic::AtomicI32 {
    unsafe { &*(lock.add(RL_COUNT) as *const core::sync::atomic::AtomicI32) }
}

pub extern "win64" fn IoInitializeRemoveLockEx(lock: *mut u8, _tag: u32, _max_minutes: u32, _high_water: u32, _size: u32) {
    unsafe {
        wr::<u8>(lock, RL_REMOVED, 0);
        wr::<i32>(lock, RL_COUNT, 1);
        ke::KeInitializeEvent(lock.add(RL_EVENT), 1, 0);
    }
}

pub extern "win64" fn IoAcquireRemoveLockEx(lock: *mut u8, _tag: u64, _file: *const u8, _line: u32, _size: u32) -> NtStatus {
    use core::sync::atomic::Ordering;
    rl_count(lock).fetch_add(1, Ordering::AcqRel);
    if unsafe { rd::<u8>(lock, RL_REMOVED) } != 0 {
        if rl_count(lock).fetch_sub(1, Ordering::AcqRel) == 1 {
            ke::KeSetEvent(unsafe { lock.add(RL_EVENT) }, 0, 0);
        }
        return STATUS_DELETE_PENDING;
    }
    STATUS_SUCCESS
}

pub extern "win64" fn IoReleaseRemoveLockEx(lock: *mut u8, _tag: u64, _size: u32) {
    if rl_count(lock).fetch_sub(1, core::sync::atomic::Ordering::AcqRel) == 1 {
        ke::KeSetEvent(unsafe { lock.add(RL_EVENT) }, 0, 0);
    }
}

/// Called with the lock held once (for IRP_MN_REMOVE_DEVICE): marks it
/// removed, drops that hold and the initial one, and waits for the rest.
pub extern "win64" fn IoReleaseRemoveLockAndWaitEx(lock: *mut u8, _tag: u64, _size: u32) {
    use core::sync::atomic::Ordering;
    unsafe { wr::<u8>(lock, RL_REMOVED, 1) };
    rl_count(lock).fetch_sub(1, Ordering::AcqRel);
    if rl_count(lock).fetch_sub(1, Ordering::AcqRel) > 1 {
        ke::KeWaitForSingleObject(unsafe { lock.add(RL_EVENT) }, 0, 0, 0, core::ptr::null());
    }
}

// -----| error log |-----

/// IoAllocateErrorLogEntry(IoObject, EntrySize): zeroed pool.
pub extern "win64" fn IoAllocateErrorLogEntry(_io_object: *mut u8, size: u8) -> *mut u8 {
    if size < 0x30 {
        return core::ptr::null_mut();
    }
    pool::alloc_tracked(size as usize)
}

pub extern "win64" fn IoFreeErrorLogEntry(entry: *mut u8) {
    pool::free_tracked(entry);
}

/// IoWriteErrorLogEntry: to the serial log (IO_ERROR_LOG_PACKET: DumpDataSize
/// 2, NumberOfStrings 4, StringOffset 6, ErrorCode 0xC, UniqueErrorValue
/// 0x10, FinalStatus 0x14, DumpData 0x28), then freed.
pub extern "win64" fn IoWriteErrorLogEntry(entry: *mut u8) {
    if entry.is_null() {
        return;
    }
    let (code, unique, fin, nstr, soff): (u32, u32, u32, u16, u16) = unsafe { (rd(entry, 0xC), rd(entry, 0x10), rd(entry, 0x14), rd(entry, 4), rd(entry, 6)) };
    let mut strings = String::new();
    if nstr > 0 && soff > 0 {
        let mut p = unsafe { entry.add(soff as usize) as *const u16 };
        for _ in 0..nstr.min(4) {
            let mut s = String::new();
            unsafe {
                while p.read_unaligned() != 0 && s.len() < 128 {
                    s.push(char::from_u32(p.read_unaligned() as u32).unwrap_or('?'));
                    p = p.add(1);
                }
                p = p.add(1);
            }
            strings.push_str(" \"");
            strings.push_str(&s);
            strings.push('"');
        }
    }
    crate::serial_println!("[DRV] error log: code {:#010x}, value {:#x}, status {:#010x}{}", code, unique, fin, strings);
    pool::free_tracked(entry);
}

// -----| named events |-----

fn named_event(name: *const u8, kind: u32, handle: *mut u64) -> *mut u8 {
    // OBJECT_ATTRIBUTES: Length 0, RootDirectory 8, ObjectName 0x10.
    let mut oa = [0u8; 0x30];
    unsafe {
        wr::<u32>(oa.as_mut_ptr(), 0, 0x30);
        wr(oa.as_mut_ptr(), 0x10, name);
    }
    let mut h = 0u64;
    // Made signalled, as Windows makes them.
    if super::zw::ZwCreateEvent(&mut h, 0x1F0003, oa.as_ptr(), kind, 1) < 0 {
        return core::ptr::null_mut();
    }
    if !handle.is_null() {
        unsafe { handle.write(h) };
    }
    match ob::get(h) {
        Some(ob::Object::Dispatcher(p)) => p as *mut u8,
        _ => core::ptr::null_mut(),
    }
}

pub extern "win64" fn IoCreateNotificationEvent(name: *const u8, handle: *mut u64) -> *mut u8 {
    named_event(name, 0, handle)
}

pub extern "win64" fn IoCreateSynchronizationEvent(name: *const u8, handle: *mut u64) -> *mut u8 {
    named_event(name, 1, handle)
}

// -----| I/O timers: once a second, at DISPATCH_LEVEL |-----

struct IoTimer {
    dev: usize,
    routine: u64,
    context: u64,
    /// A KTIMER and a KDPC in pool.
    timer: usize,
    dpc: usize,
}

static IO_TIMERS: Mutex<Vec<IoTimer>> = Mutex::new(Vec::new());

extern "win64" fn io_timer_dpc(_dpc: *mut u8, dev: u64, _a: u64, _b: u64) {
    let found = locked(|| IO_TIMERS.lock().iter().find(|t| t.dev == dev as usize).map(|t| (t.routine, t.context)));
    if let Some((routine, ctx)) = found {
        let f: extern "win64" fn(u64, u64) = unsafe { core::mem::transmute(routine) };
        f(dev, ctx);
    }
}

/// IoInitializeTimer(DeviceObject, TimerRoutine, Context).
pub extern "win64" fn IoInitializeTimer(dev: *mut u8, routine: u64, context: u64) -> NtStatus {
    let timer = pool::alloc_tracked(0x40);
    let dpc = pool::alloc_tracked(0x40);
    if timer.is_null() || dpc.is_null() {
        return STATUS_NO_MEMORY;
    }
    super::dpc::KeInitializeTimer(timer);
    super::dpc::KeInitializeDpc(dpc, io_timer_dpc as *const () as u64, dev as u64);
    locked(|| {
        let mut list = IO_TIMERS.lock();
        list.retain(|t| t.dev != dev as usize);
        list.push(IoTimer { dev: dev as usize, routine, context, timer: timer as usize, dpc: dpc as usize });
    });
    STATUS_SUCCESS
}

fn timer_of(dev: *mut u8) -> Option<(usize, usize)> {
    locked(|| IO_TIMERS.lock().iter().find(|t| t.dev == dev as usize).map(|t| (t.timer, t.dpc)))
}

pub extern "win64" fn IoStartTimer(dev: *mut u8) {
    if let Some((t, d)) = timer_of(dev) {
        super::dpc::KeSetTimerEx(t as *mut u8, -10_000_000, 1000, d as *mut u8);
    }
}

pub extern "win64" fn IoStopTimer(dev: *mut u8) {
    if let Some((t, _)) = timer_of(dev) {
        super::dpc::KeCancelTimer(t as *mut u8);
    }
}

// -----| stack limits |-----

/// IoGetStackLimits(LowLimit, HighLimit): the running thread's stack.
pub extern "win64" fn IoGetStackLimits(low: *mut u64, high: *mut u64) {
    let (lo, hi) = crate::sched::kthread::current_stack().unwrap_or_else(|| {
        // A stack VEILOS didn't allocate (the boot or an idle stack): the
        // 64 KiB around where this runs.
        let sp: u64;
        unsafe { core::arch::asm!("mov {}, rsp", out(reg) sp, options(nomem, nostack)) };
        ((sp & !0xFFFF), (sp & !0xFFFF) + 0x10000)
    });
    unsafe {
        low.write(lo);
        high.write(hi);
    }
}

// -----| registry keys of devices and drivers |-----

/// IoOpenDeviceRegistryKey(DeviceObject, DevInstKeyType, DesiredAccess,
/// DeviceRegKey*): PLUGPLAY_REGKEY_DEVICE (1) or _DRIVER (2).
pub extern "win64" fn IoOpenDeviceRegistryKey(dev: *mut u8, which: u32, _access: u32, out: *mut u64) -> NtStatus {
    match super::pnp::registry_key_of(dev, which) {
        Some(path) => key_handle(&path, out),
        None => STATUS_INVALID_PARAMETER,
    }
}

/// IoOpenDriverRegistryKey(DriverObject, RegKeyType, DesiredAccess, Flags,
/// DriverRegKey*): DriverRegKeyParameters (0) or PersistentState (1).
pub extern "win64" fn IoOpenDriverRegistryKey(drv: *mut u8, which: u32, _access: u32, _flags: u32, out: *mut u64) -> NtStatus {
    let Some(name) = super::image::name_of(drv) else { return STATUS_INVALID_PARAMETER };
    let sub = match which {
        0 => "Parameters",
        1 => "State",
        _ => return STATUS_INVALID_PARAMETER,
    };
    key_handle(&format!(r"HKLM\SYSTEM\CurrentControlSet\Services\{}\{}", name, sub), out)
}

/// IoQueryFullDriverPath(DriverObject, FullPath): the .sys it was loaded
/// from, as an NT path (\??\C:\...).
pub extern "win64" fn IoQueryFullDriverPath(drv: *mut u8, out: *mut u8) -> NtStatus {
    let Some(path) = super::image::path_of(drv) else { return STATUS_INVALID_PARAMETER };
    put_unicode(out, &format!("\\??\\C:{}", path.replace('/', "\\")))
}

// -----| device interfaces |-----

struct Interface {
    guid: [u8; 16],
    pdo: usize,
    link: String,
    enabled: bool,
}

static INTERFACES: Mutex<Vec<Interface>> = Mutex::new(Vec::new());

/// GUID_DEVICE_INTERFACE_ARRIVAL and _REMOVAL.
const ARRIVAL: [u8; 16] = [0x04, 0x40, 0x3A, 0xCB, 0xF0, 0x46, 0xD0, 0x11, 0xB0, 0x8F, 0x00, 0x60, 0x97, 0x13, 0x05, 0x3F];
const REMOVAL: [u8; 16] = [0x05, 0x40, 0x3A, 0xCB, 0xF0, 0x46, 0xD0, 0x11, 0xB0, 0x8F, 0x00, 0x60, 0x97, 0x13, 0x05, 0x3F];

/// IoRegisterDeviceInterface(PhysicalDeviceObject, InterfaceClassGuid,
/// ReferenceString, SymbolicLinkName).
pub extern "win64" fn IoRegisterDeviceInterface(pdo: *mut u8, guid: *const u8, reference: *const u8, out: *mut u8) -> NtStatus {
    let Some(inst) = super::pnp::instance_path(pdo) else { return STATUS_INVALID_DEVICE_REQUEST };
    let g = guid_at(guid);
    let mut link = format!("\\??\\{}#{}", inst.replace('\\', "#"), guid_string(&g));
    if !reference.is_null() {
        let r = rtl::unicode_to_string(reference);
        if !r.is_empty() {
            link = format!("{}\\{}", link, r);
        }
    }
    let base = io::stack_base(pdo) as usize;
    let existed = locked(|| {
        let mut list = INTERFACES.lock();
        if list.iter().any(|i| i.link.eq_ignore_ascii_case(&link)) {
            return true;
        }
        list.push(Interface { guid: g, pdo: base, link: link.clone(), enabled: false });
        false
    });
    let st = put_unicode(out, &link);
    if st < 0 {
        return st;
    }
    if existed { STATUS_OBJECT_NAME_EXISTS } else { STATUS_SUCCESS }
}

/// IoSetDeviceInterfaceState(SymbolicLinkName, Enable): the link to the
/// device appears or goes, and notification callbacks hear of it.
pub extern "win64" fn IoSetDeviceInterfaceState(name: *const u8, enable: u8) -> NtStatus {
    let link = rtl::unicode_to_string(name);
    let found = locked(|| {
        let mut list = INTERFACES.lock();
        let i = list.iter_mut().find(|i| i.link.eq_ignore_ascii_case(&link))?;
        let changed = i.enabled != (enable != 0);
        i.enabled = enable != 0;
        Some((i.guid, i.pdo, changed))
    });
    let Some((guid, pdo, changed)) = found else { return STATUS_OBJECT_NAME_NOT_FOUND };
    if !changed {
        return STATUS_SUCCESS;
    }
    let target = io::name_of(pdo as *mut u8).unwrap_or_default();
    let us = super::image::pool_unicode(&link);
    if enable != 0 {
        let t = super::image::pool_unicode(&target);
        io::IoCreateSymbolicLink(us, t);
        free_unicode(t);
    } else {
        io::IoDeleteSymbolicLink(us);
    }
    notify_interface(if enable != 0 { &ARRIVAL } else { &REMOVAL }, &guid, us);
    free_unicode(us);
    STATUS_SUCCESS
}

fn free_unicode(us: *mut u8) {
    super::rtlx::RtlFreeUnicodeString(us);
    pool::free_tracked(us);
}

/// IoGetDeviceInterfaces(InterfaceClassGuid, PhysicalDeviceObject,
/// Flags, SymbolicLinkList*): a MULTI_SZ of links, enabled ones only unless
/// DEVICE_INTERFACE_INCLUDE_NONACTIVE (1).
pub extern "win64" fn IoGetDeviceInterfaces(guid: *const u8, pdo: *mut u8, flags: u32, out: *mut *mut u16) -> NtStatus {
    let g = guid_at(guid);
    let base = if pdo.is_null() { 0 } else { io::stack_base(pdo) as usize };
    let links: Vec<String> = locked(|| {
        INTERFACES
            .lock()
            .iter()
            .filter(|i| i.guid == g && (base == 0 || i.pdo == base) && (i.enabled || flags & 1 != 0))
            .map(|i| i.link.clone())
            .collect()
    });
    let mut units: Vec<u16> = Vec::new();
    for l in &links {
        units.extend(l.encode_utf16());
        units.push(0);
    }
    units.push(0);
    let buf = pool::alloc_tracked(units.len() * 2);
    if buf.is_null() {
        return STATUS_NO_MEMORY;
    }
    unsafe {
        core::ptr::copy_nonoverlapping(units.as_ptr(), buf as *mut u16, units.len());
        out.write(buf as *mut u16);
    }
    STATUS_SUCCESS
}

/// IoOpenDeviceInterfaceRegistryKey(SymbolicLinkName, DesiredAccess,
/// DeviceInterfaceKey*): the interface's Device Parameters.
pub extern "win64" fn IoOpenDeviceInterfaceRegistryKey(name: *const u8, _access: u32, out: *mut u64) -> NtStatus {
    let link = rtl::unicode_to_string(name);
    let Some(g) = locked(|| INTERFACES.lock().iter().find(|i| i.link.eq_ignore_ascii_case(&link)).map(|i| i.guid)) else {
        return STATUS_OBJECT_NAME_NOT_FOUND;
    };
    let leaf = link.trim_start_matches("\\??\\").replace('\\', "#");
    key_handle(&format!(r"HKLM\SYSTEM\CurrentControlSet\Control\DeviceClasses\{}\##?#{}\#\Device Parameters", guid_string(&g), leaf), out)
}

/// The device's interfaces go with it (pnp.rs, on removal).
pub fn forget_interfaces(pdo: *mut u8) {
    let gone: Vec<Interface> = locked(|| {
        let mut list = INTERFACES.lock();
        let (gone, keep): (Vec<Interface>, Vec<Interface>) = core::mem::take(&mut *list).into_iter().partition(|i| i.pdo == pdo as usize);
        *list = keep;
        gone
    });
    for i in gone.iter().filter(|i| i.enabled) {
        let us = super::image::pool_unicode(&i.link);
        io::IoDeleteSymbolicLink(us);
        notify_interface(&REMOVAL, &i.guid, us);
        free_unicode(us);
    }
}

// -----| PnP notifications |-----

struct Notification {
    category: u32,
    guid: [u8; 16],
    routine: u64,
    context: u64,
}

static NOTIFICATIONS: Mutex<Vec<Box<Notification>>> = Mutex::new(Vec::new());
const EVENT_CATEGORY_DEVICE_INTERFACE_CHANGE: u32 = 2;
const INCLUDE_EXISTING_INTERFACES: u32 = 1;

/// DEVICE_INTERFACE_CHANGE_NOTIFICATION: Version 0, Size 2, Event 4,
/// InterfaceClassGuid 0x14, SymbolicLinkName 0x28 (0x30 bytes).
fn call_interface_callback(routine: u64, context: u64, event: &[u8; 16], guid: &[u8; 16], link: *mut u8) {
    let mut n = [0u8; 0x30];
    unsafe {
        let p = n.as_mut_ptr();
        wr::<u16>(p, 0, 1);
        wr::<u16>(p, 2, 0x30);
        core::ptr::copy_nonoverlapping(event.as_ptr(), p.add(4), 16);
        core::ptr::copy_nonoverlapping(guid.as_ptr(), p.add(0x14), 16);
        wr(p, 0x28, link);
    }
    let f: extern "win64" fn(*mut u8, u64) -> NtStatus = unsafe { core::mem::transmute(routine) };
    let _ = f(n.as_mut_ptr(), context);
    ke::set_irql(0);
}

fn notify_interface(event: &[u8; 16], guid: &[u8; 16], link: *mut u8) {
    let targets: Vec<(u64, u64)> = locked(|| {
        NOTIFICATIONS
            .lock()
            .iter()
            .filter(|n| n.category == EVENT_CATEGORY_DEVICE_INTERFACE_CHANGE && n.guid == *guid)
            .map(|n| (n.routine, n.context))
            .collect()
    });
    for (r, c) in targets {
        call_interface_callback(r, c, event, guid, link);
    }
}

/// IoRegisterPlugPlayNotification(EventCategory, EventCategoryFlags,
/// EventCategoryData, DriverObject, CallbackRoutine, Context,
/// NotificationEntry*). Interface changes are reported (with the ones
/// already there, if asked); other categories are accepted and never fire.
pub extern "win64" fn IoRegisterPlugPlayNotification(category: u32, flags: u32, data: *const u8, _drv: *mut u8, routine: u64, context: u64, entry: *mut u64) -> NtStatus {
    let guid = if category == EVENT_CATEGORY_DEVICE_INTERFACE_CHANGE && !data.is_null() { guid_at(data) } else { [0; 16] };
    let n = Box::new(Notification { category, guid, routine, context });
    let handle = &*n as *const Notification as u64;
    locked(|| NOTIFICATIONS.lock().push(n));
    unsafe { entry.write(handle) };
    if category == EVENT_CATEGORY_DEVICE_INTERFACE_CHANGE && flags & INCLUDE_EXISTING_INTERFACES != 0 {
        let existing: Vec<String> = locked(|| INTERFACES.lock().iter().filter(|i| i.enabled && i.guid == guid).map(|i| i.link.clone()).collect());
        for l in existing {
            let us = super::image::pool_unicode(&l);
            call_interface_callback(routine, context, &ARRIVAL, &guid, us);
            free_unicode(us);
        }
    }
    STATUS_SUCCESS
}

pub extern "win64" fn IoUnregisterPlugPlayNotification(entry: u64) -> NtStatus {
    locked(|| NOTIFICATIONS.lock().retain(|n| &**n as *const Notification as u64 != entry));
    STATUS_SUCCESS
}

pub extern "win64" fn IoUnregisterPlugPlayNotificationEx(entry: u64) -> NtStatus {
    IoUnregisterPlugPlayNotification(entry)
}

// -----| power |-----

const IRP_MN_WAIT_WAKE: u8 = 0;
const IRP_MN_SET_POWER: u8 = 2;
const IRP_MN_QUERY_POWER: u8 = 3;

/// PoRequestPowerIrp(DeviceObject, MinorFunction, PowerState,
/// CompletionFunction, Context, Irp*): a device power IRP down the stack
/// `dev` is in; when it's done, CompletionFunction(DeviceObject,
/// MinorFunction, PowerState, Context, IoStatus). Wait-wake isn't
/// supported (nothing wakes devices yet).
pub extern "win64" fn PoRequestPowerIrp(dev: *mut u8, minor: u8, state: u32, completion: u64, context: u64, irp_out: *mut *mut u8) -> NtStatus {
    if minor == IRP_MN_WAIT_WAKE {
        return super::pnp::STATUS_NOT_SUPPORTED;
    }
    if minor != IRP_MN_SET_POWER && minor != IRP_MN_QUERY_POWER {
        return STATUS_INVALID_PARAMETER;
    }
    let top = io::IoGetAttachedDevice(io::stack_base(dev));
    let mut iosb = [0u64; 2];
    let (st, info) = io::send(
        top,
        core::ptr::null_mut(),
        super::pnp::IRP_MJ_POWER,
        |irp, sl| unsafe {
            wr(irp, io::IRP_STATUS, super::pnp::STATUS_NOT_SUPPORTED);
            wr::<u8>(sl, 1, minor);
            // Parameters.Power: SystemContext 0, Type 8 (DevicePowerState),
            // State 0x10.
            wr::<u32>(sl, io::SL_PARAMETERS + 8, 1);
            wr::<u32>(sl, io::SL_PARAMETERS + 0x10, state);
            if !irp_out.is_null() {
                irp_out.write(irp);
            }
        },
        |_| {},
    );
    ke::set_irql(0);
    iosb[0] = st as u32 as u64;
    iosb[1] = info;
    if completion != 0 {
        let f: extern "win64" fn(*mut u8, u8, u32, u64, *mut u64) = unsafe { core::mem::transmute(completion) };
        f(dev, minor, state, context, iosb.as_mut_ptr());
        ke::set_irql(0);
    }
    if !irp_out.is_null() {
        // The IRP is gone by now.
        unsafe { irp_out.write(core::ptr::null_mut()) };
    }
    STATUS_PENDING
}

// -----| odds and ends |-----

/// ObOpenObjectByPointer(Object, HandleAttributes, PassedAccessState,
/// DesiredAccess, ObjectType, AccessMode, Handle*): a kernel handle to it.
pub extern "win64" fn ObOpenObjectByPointer(object: u64, _attrs: u32, _state: *const u8, _access: u32, _ty: u64, _mode: u8, out: *mut u64) -> NtStatus {
    if object == 0 {
        return STATUS_INVALID_PARAMETER;
    }
    // Dispatcher objects start with their type (events 0/1, mutants 2,
    // semaphores 5, threads 6, timers 8/9); anything else is handed back
    // as it is (ObReferenceObjectByHandle returns the same pointer).
    let kind = unsafe { (object as *const u8).read() };
    let o = if matches!(kind, 0 | 1 | 2 | 5 | 6 | 8 | 9) { ob::Object::Dispatcher(object as usize) } else { ob::Object::Pointer(object as usize) };
    unsafe { out.write(ob::insert(o)) };
    STATUS_SUCCESS
}

/// No file system filters run on VEILOS: accepted, never called.
pub extern "win64" fn FsRtlRegisterFileSystemFilterCallbacks(_drv: *mut u8, _callbacks: *const u8) -> NtStatus {
    STATUS_SUCCESS
}

pub extern "win64" fn IoInvalidateDeviceRelations(dev: *mut u8, kind: u32) {
    crate::serial_println!("[PNP] IoInvalidateDeviceRelations({:p}, {}): bus relations don't change on VEILOS", dev, kind);
}

pub extern "win64" fn IoRequestDeviceEject(dev: *mut u8) {
    crate::serial_println!("[PNP] IoRequestDeviceEject({:p}): not supported", dev);
}

pub extern "win64" fn IoWMIRegistrationControl(_dev: *mut u8, _action: u32) -> NtStatus {
    STATUS_SUCCESS
}
