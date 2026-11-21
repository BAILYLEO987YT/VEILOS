// System threads and work items: drivers' code that runs at PASSIVE_LEVEL
// on a thread of its own. Each is a VEILOS kernel thread on the CPU that
// asked for it (kernel threads switch where they wait, see kthread.rs).

use alloc::collections::BTreeMap;
use spin::Mutex;

use super::{ke, ob, pool, NtStatus, STATUS_NO_MEMORY, STATUS_SUCCESS};

/// Kernel thread id -> its thread object (a dispatcher header, signalled
/// when the thread ends), for PsTerminateSystemThread.
static RUNNING: Mutex<BTreeMap<u64, usize>> = Mutex::new(BTreeMap::new());

const THREAD_OBJECT: u8 = 6;

fn finish(obj: usize) -> ! {
    x86_64::instructions::interrupts::without_interrupts(|| RUNNING.lock().remove(&crate::kthread::current_id()));
    ke::set_irql(0);
    unsafe { ((obj as *mut u8).add(4) as *mut i32).write_volatile(1) };
    crate::kthread::exit_current()
}

/// PsCreateSystemThread(ThreadHandle*, DesiredAccess, ObjectAttributes*,
/// ProcessHandle, ClientId*, StartRoutine, StartContext).
pub extern "win64" fn PsCreateSystemThread(
    out: *mut u64,
    _access: u32,
    _oa: *const u8,
    _process: u64,
    client_id: *mut u64,
    start: extern "win64" fn(u64),
    context: u64,
) -> NtStatus {
    let obj = pool::alloc_tracked(0x40);
    if obj.is_null() {
        return STATUS_NO_MEMORY;
    }
    unsafe {
        obj.write(THREAD_OBJECT);
        obj.add(2).write(0x10);
        let list = obj.add(8) as *mut u64;
        list.write(list as u64);
        list.add(1).write(list as u64);
    }
    let obj = obj as usize;
    let id = crate::kthread::spawn("driver thread", move || {
        x86_64::instructions::interrupts::without_interrupts(|| RUNNING.lock().insert(crate::kthread::current_id(), obj));
        ke::set_irql(0);
        start(context);
        finish(obj)
    });
    if !client_id.is_null() {
        unsafe {
            client_id.write(4);
            client_id.add(1).write((id + 1) * 4);
        }
    }
    unsafe { out.write(ob::insert(ob::Object::Dispatcher(obj))) };
    STATUS_SUCCESS
}

pub extern "win64" fn PsTerminateSystemThread(_status: NtStatus) -> NtStatus {
    let obj = x86_64::instructions::interrupts::without_interrupts(|| RUNNING.lock().get(&crate::kthread::current_id()).copied());
    match obj {
        Some(o) => finish(o),
        None => super::STATUS_INVALID_PARAMETER,
    }
}

pub extern "win64" fn PsIsSystemThread(_thread: u64) -> u8 {
    1
}

// -----| work items |-----

/// IO_WORKITEM, VEILOS's layout (drivers treat it as opaque): DeviceObject
/// at 0, then routine and context while queued.
pub extern "win64" fn IoSizeofWorkItem() -> u32 {
    0x58
}

pub extern "win64" fn IoAllocateWorkItem(dev: u64) -> *mut u8 {
    let w = pool::alloc_tracked(0x58);
    if !w.is_null() {
        IoInitializeWorkItem(dev, w);
    }
    w
}

pub extern "win64" fn IoInitializeWorkItem(dev: u64, w: *mut u8) {
    unsafe { (w as *mut u64).write(dev) };
}

pub extern "win64" fn IoUninitializeWorkItem(_w: *mut u8) {}

pub extern "win64" fn IoFreeWorkItem(w: *mut u8) {
    pool::free_tracked(w);
}

/// IoQueueWorkItem(IoWorkItem, WorkerRoutine(DeviceObject, Context),
/// QueueType, Context).
pub extern "win64" fn IoQueueWorkItem(w: *mut u8, routine: extern "win64" fn(u64, u64), _queue: u32, context: u64) {
    let dev = unsafe { (w as *const u64).read() };
    crate::kthread::spawn("driver work item", move || {
        ke::set_irql(0);
        routine(dev, context);
        crate::kthread::exit_current()
    });
}

/// IoQueueWorkItemEx: the routine also gets the work item.
pub extern "win64" fn IoQueueWorkItemEx(w: *mut u8, routine: extern "win64" fn(u64, u64, u64), _queue: u32, context: u64) {
    let dev = unsafe { (w as *const u64).read() };
    let item = w as u64;
    crate::kthread::spawn("driver work item", move || {
        ke::set_irql(0);
        routine(dev, context, item);
        crate::kthread::exit_current()
    });
}

/// ExQueueWorkItem(WORK_QUEUE_ITEM*, QueueType): WorkerRoutine at 0x10,
/// Parameter at 0x18.
pub extern "win64" fn ExQueueWorkItem(item: *mut u8, _queue: u32) {
    let (routine, param) = unsafe { ((item.add(0x10) as *const u64).read(), (item.add(0x18) as *const u64).read()) };
    let routine: extern "win64" fn(u64) = unsafe { core::mem::transmute(routine) };
    crate::kthread::spawn("driver work item", move || {
        ke::set_irql(0);
        routine(param);
        crate::kthread::exit_current()
    });
}
