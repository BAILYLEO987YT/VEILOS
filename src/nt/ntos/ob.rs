// Kernel handles (what Zw* calls and PsCreateSystemThread hand drivers) and
// the object manager calls drivers make on them.
//
// Kernel handles have the top bit set, as Windows' do. Objects here are
// either a registry key (by path) or a dispatcher object in pool (a thread):
// ObReferenceObjectByHandle gives the latter's address, which drivers wait
// on. Reference counts aren't kept: objects live until the handle closes or
// the thread they stand for ends.

use alloc::{collections::BTreeMap, string::String, sync::Arc};
use core::sync::atomic::{AtomicU64, Ordering};
use spin::Mutex;

use super::{NtStatus, STATUS_SUCCESS};

const STATUS_INVALID_HANDLE: NtStatus = 0xC000_0008u32 as i32;

#[derive(Clone)]
pub enum Object {
    /// A registry key: its hive-rooted path ("HKLM\SYSTEM\...").
    Key(String),
    /// A dispatcher object in pool (a system thread's, an event's).
    Dispatcher(usize),
    /// A file on the VFS (zw.rs).
    File(Arc<super::zw::KFile>),
    /// A directory on the VFS, by path.
    Directory(String),
    /// A driver's device, opened by name.
    Device(Arc<super::io::DeviceHandle>),
    /// A symbolic link: its target.
    Link(String),
}

static HANDLES: Mutex<BTreeMap<u64, Object>> = Mutex::new(BTreeMap::new());
static NEXT: AtomicU64 = AtomicU64::new(4);

pub fn insert(o: Object) -> u64 {
    let h = 0xFFFF_FFFF_8000_0000 | NEXT.fetch_add(4, Ordering::Relaxed);
    x86_64::instructions::interrupts::without_interrupts(|| HANDLES.lock().insert(h, o));
    h
}

pub fn get(h: u64) -> Option<Object> {
    x86_64::instructions::interrupts::without_interrupts(|| HANDLES.lock().get(&h).cloned())
}

pub extern "win64" fn ZwClose(h: u64) -> NtStatus {
    match x86_64::instructions::interrupts::without_interrupts(|| HANDLES.lock().remove(&h)) {
        Some(Object::Dispatcher(p)) => {
            super::zw::dispatcher_closed(p);
            STATUS_SUCCESS
        }
        Some(_) => STATUS_SUCCESS,
        None => STATUS_INVALID_HANDLE,
    }
}

/// ObReferenceObjectByHandle(Handle, DesiredAccess, ObjectType, AccessMode,
/// Object*, HandleInformation*).
pub extern "win64" fn ObReferenceObjectByHandle(h: u64, _access: u32, _ty: u64, _mode: u8, out: *mut u64, _info: *mut u8) -> NtStatus {
    match get(h) {
        Some(Object::Dispatcher(p)) => {
            unsafe { out.write(p as u64) };
            STATUS_SUCCESS
        }
        _ => STATUS_INVALID_HANDLE,
    }
}

pub extern "win64" fn ObfReferenceObject(_o: u64) -> i64 {
    1
}

pub extern "win64" fn ObfDereferenceObject(_o: u64) -> i64 {
    0
}

pub extern "win64" fn ObDereferenceObjectDeferDelete(_o: u64) {}
