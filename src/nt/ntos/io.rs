// The I/O manager's part drivers see: DRIVER_OBJECT, DEVICE_OBJECT, IRPs.
//
// Offsets are Windows x64's (WDK wdm.h); win32/tests/k1/layout.c asserts
// each one against the real headers.

use alloc::{collections::BTreeMap, string::String, vec::Vec};
use spin::Mutex;

use super::{pool, rtl, NtStatus, STATUS_BUFFER_TOO_SMALL, STATUS_INVALID_DEVICE_REQUEST, STATUS_NO_MEMORY, STATUS_OBJECT_NAME_COLLISION, STATUS_PENDING, STATUS_SUCCESS};

// DRIVER_OBJECT (0x150 bytes)
pub const DRV_SIZE: usize = 0x150;
pub const DRV_DEVICE_OBJECT: usize = 0x08;
pub const DRV_START: usize = 0x18;
pub const DRV_IMAGE_SIZE: usize = 0x20;
pub const DRV_EXTENSION: usize = 0x30;
pub const DRV_NAME: usize = 0x38;
pub const DRV_INIT: usize = 0x58;
pub const DRV_UNLOAD: usize = 0x68;
pub const DRV_MAJOR_FUNCTION: usize = 0x70;
pub const IRP_MJ_MAXIMUM_FUNCTION: usize = 0x1B;

// DEVICE_OBJECT (0x150 bytes)
pub const DEV_SIZE: usize = 0x150;
pub const DEV_DRIVER_OBJECT: usize = 0x08;
pub const DEV_NEXT_DEVICE: usize = 0x10;
pub const DEV_ATTACHED_DEVICE: usize = 0x18;
pub const DEV_FLAGS: usize = 0x30;
pub const DEV_CHARACTERISTICS: usize = 0x34;
pub const DEV_EXTENSION: usize = 0x40;
pub const DEV_TYPE: usize = 0x48;
pub const DEV_STACK_SIZE: usize = 0x4C;
pub const DEV_ALIGNMENT: usize = 0x98;
pub const DEV_OBJECT_EXTENSION: usize = 0x138;
pub const DO_DEVICE_INITIALIZING: u32 = 0x80;
const DO_BUFFERED_IO: u32 = 0x04;
const DO_DIRECT_IO: u32 = 0x10;

// IRP (0xD0 bytes, then its stack locations)
pub const IRP_SIZE: usize = 0xD0;
pub const IRP_MDL_ADDRESS: usize = 0x08;
pub const IRP_SYSTEM_BUFFER: usize = 0x18;
pub const IRP_STATUS: usize = 0x30;
pub const IRP_INFORMATION: usize = 0x38;
pub const IRP_PENDING_RETURNED: usize = 0x41;
pub const IRP_STACK_COUNT: usize = 0x42;
pub const IRP_CURRENT_LOCATION: usize = 0x43;
pub const IRP_CANCEL: usize = 0x44;
pub const IRP_CANCEL_IRQL: usize = 0x45;
pub const IRP_USER_IOSB: usize = 0x48;
pub const IRP_USER_EVENT: usize = 0x50;
pub const IRP_CANCEL_ROUTINE: usize = 0x68;
pub const IRP_USER_BUFFER: usize = 0x70;
pub const IRP_CURRENT_STACK: usize = 0xB8;
pub const IRP_ORIGINAL_FILE_OBJECT: usize = 0xC0;

// IO_STACK_LOCATION (0x48 bytes)
pub const STACK_SIZE: usize = 0x48;
pub const SL_MAJOR: usize = 0x00;
pub const SL_CONTROL: usize = 0x03;
pub const SL_PARAMETERS: usize = 0x08;
pub const SL_DEVICE_OBJECT: usize = 0x28;
pub const SL_FILE_OBJECT: usize = 0x30;
pub const SL_COMPLETION_ROUTINE: usize = 0x38;
pub const SL_CONTEXT: usize = 0x40;
// Parameters.DeviceIoControl
pub const SL_OUTPUT_LENGTH: usize = 0x08;
pub const SL_INPUT_LENGTH: usize = 0x10;
pub const SL_IOCTL_CODE: usize = 0x18;
pub const SL_TYPE3_INPUT: usize = 0x20;

// FILE_OBJECT (0xD8 bytes)
pub const FO_SIZE: usize = 0xD8;
pub const FO_DEVICE_OBJECT: usize = 0x08;
pub const FO_FILE_NAME: usize = 0x58;

const METHOD_BUFFERED: u32 = 0;
const METHOD_IN_DIRECT: u32 = 1;
const METHOD_OUT_DIRECT: u32 = 2;
const METHOD_NEITHER: u32 = 3;
const STATUS_ACCESS_VIOLATION: NtStatus = 0xC000_0005u32 as i32;

const SL_PENDING_RETURNED: u8 = 0x01;
const SL_INVOKE_ON_CANCEL: u8 = 0x20;
const SL_INVOKE_ON_SUCCESS: u8 = 0x40;
const SL_INVOKE_ON_ERROR: u8 = 0x80;
const STATUS_MORE_PROCESSING_REQUIRED: NtStatus = 0xC000_0016u32 as i32;
const STATUS_CANCELLED: NtStatus = 0xC000_0120u32 as i32;

pub const IRP_MJ_CREATE: u8 = 0x00;
pub const IRP_MJ_CLOSE: u8 = 0x02;
pub const IRP_MJ_CLEANUP: u8 = 0x12;
pub const IRP_MJ_DEVICE_CONTROL: u8 = 0x0E;
pub const IRP_MJ_INTERNAL_DEVICE_CONTROL: u8 = 0x0F;
pub const IRP_MJ_READ: u8 = 0x03;
pub const IRP_MJ_WRITE: u8 = 0x04;

unsafe fn rd<T: Copy>(base: *mut u8, off: usize) -> T {
    unsafe { (base.add(off) as *const T).read_unaligned() }
}
unsafe fn wr<T>(base: *mut u8, off: usize, v: T) {
    unsafe { (base.add(off) as *mut T).write_unaligned(v) }
}

/// Device names (\Device\X) and symbolic links (\DosDevices\X, \??\X).
static NAMES: Mutex<BTreeMap<String, usize>> = Mutex::new(BTreeMap::new());
static LINKS: Mutex<BTreeMap<String, String>> = Mutex::new(BTreeMap::new());

fn key(name: &str) -> String {
    name.to_ascii_lowercase()
}

/// Where a symbolic link (\??\X, \DosDevices\X, \GLOBAL??\X) points.
pub fn link_target(name: &str) -> Option<String> {
    let mut n = key(name);
    for p in [r"\??\", r"\global??\"] {
        if let Some(rest) = n.strip_prefix(p) {
            n = alloc::format!(r"\dosdevices\{}", rest);
        }
    }
    LINKS.lock().get(&n).cloned()
}

/// The device a name (or a link to it) refers to.
pub fn device_by_name(name: &str) -> Option<*mut u8> {
    // A bare name: the device of that name, or the link.
    if !name.contains('\\') {
        return device_by_name(&alloc::format!("\\Device\\{}", name))
            .or_else(|| device_by_name(&alloc::format!("\\DosDevices\\{}", name)));
    }
    let mut n = key(name);
    if let Some(rest) = n.strip_prefix("\\??\\") {
        n = alloc::format!("\\dosdevices\\{}", rest);
    }
    if let Some(t) = LINKS.lock().get(&n) {
        n = key(t);
    }
    NAMES.lock().get(&n).map(|&p| p as *mut u8)
}

// -----| devices |-----

pub extern "win64" fn IoCreateDevice(
    driver: *mut u8,
    ext_size: u32,
    name: *const u8,
    dev_type: u32,
    characteristics: u32,
    _exclusive: u8,
    out: *mut *mut u8,
) -> NtStatus {
    let name = if name.is_null() { None } else { Some(rtl::unicode_to_string(name)) };
    if let Some(n) = &name {
        if NAMES.lock().contains_key(&key(n)) {
            return STATUS_OBJECT_NAME_COLLISION;
        }
    }
    let ext_size = (ext_size as usize).next_multiple_of(16);
    let dev = pool::alloc_tracked(DEV_SIZE + ext_size);
    let dox = pool::alloc_tracked(0x60);
    if dev.is_null() || dox.is_null() {
        return STATUS_NO_MEMORY;
    }
    unsafe {
        wr::<u16>(dev, 0, 3); // IO_TYPE_DEVICE
        wr::<u16>(dev, 2, (DEV_SIZE + ext_size) as u16);
        wr(dev, DEV_DRIVER_OBJECT, driver);
        wr(dev, DEV_NEXT_DEVICE, rd::<*mut u8>(driver, DRV_DEVICE_OBJECT));
        wr(driver, DRV_DEVICE_OBJECT, dev);
        wr::<u32>(dev, DEV_FLAGS, DO_DEVICE_INITIALIZING);
        wr::<u32>(dev, DEV_CHARACTERISTICS, characteristics);
        if ext_size > 0 {
            wr(dev, DEV_EXTENSION, dev.add(DEV_SIZE));
        }
        wr::<u32>(dev, DEV_TYPE, dev_type);
        wr::<i8>(dev, DEV_STACK_SIZE, 1);
        wr::<u16>(dox, 0, 13); // IO_TYPE_DEVICE_OBJECT_EXTENSION
        wr::<u16>(dox, 2, 0x60);
        wr(dox, 8, dev);
        wr(dev, DEV_OBJECT_EXTENSION, dox);
        out.write(dev);
    }
    if let Some(n) = name {
        crate::serial_println!("[NTOS] device {} created", n);
        NAMES.lock().insert(key(&n), dev as usize);
    }
    STATUS_SUCCESS
}

pub extern "win64" fn IoDeleteDevice(dev: *mut u8) {
    unsafe {
        let driver: *mut u8 = rd(dev, DEV_DRIVER_OBJECT);
        // Unlink it from the driver's list.
        let mut link = driver.add(DRV_DEVICE_OBJECT) as *mut *mut u8;
        while !(*link).is_null() {
            if *link == dev {
                *link = rd(dev, DEV_NEXT_DEVICE);
                break;
            }
            link = (*link).add(DEV_NEXT_DEVICE) as *mut *mut u8;
        }
        NAMES.lock().retain(|_, &mut p| p != dev as usize);
        x86_64::instructions::interrupts::without_interrupts(|| ATTACHED_TO.lock().remove(&(dev as usize)));
        pool::free_tracked(rd(dev, DEV_OBJECT_EXTENSION));
    }
    pool::free_tracked(dev);
}

pub extern "win64" fn IoCreateSymbolicLink(link: *const u8, target: *const u8) -> NtStatus {
    let (l, t) = (rtl::unicode_to_string(link), rtl::unicode_to_string(target));
    let mut l = key(&l);
    if let Some(rest) = l.strip_prefix("\\??\\") {
        l = alloc::format!("\\dosdevices\\{}", rest);
    }
    LINKS.lock().insert(l, t);
    STATUS_SUCCESS
}

pub extern "win64" fn IoDeleteSymbolicLink(link: *const u8) -> NtStatus {
    let mut l = key(&rtl::unicode_to_string(link));
    if let Some(rest) = l.strip_prefix("\\??\\") {
        l = alloc::format!("\\dosdevices\\{}", rest);
    }
    LINKS.lock().remove(&l);
    STATUS_SUCCESS
}

// -----| IRPs |-----

pub extern "win64" fn IoAllocateIrp(stack_size: i8, _charge_quota: u8) -> *mut u8 {
    let n = stack_size.max(1) as usize;
    let size = IRP_SIZE + n * STACK_SIZE;
    let irp = pool::alloc_tracked(size);
    if irp.is_null() {
        return irp;
    }
    unsafe {
        wr::<u16>(irp, 0, 6); // IO_TYPE_IRP
        wr::<u16>(irp, 2, size as u16);
        // ThreadListEntry points at itself.
        let tle = irp.add(0x20) as *mut u64;
        tle.write(tle as u64);
        tle.add(1).write(tle as u64);
        wr::<i8>(irp, IRP_STACK_COUNT, n as i8);
        wr::<i8>(irp, IRP_CURRENT_LOCATION, n as i8 + 1);
        wr(irp, IRP_CURRENT_STACK, irp.add(IRP_SIZE + n * STACK_SIZE));
    }
    irp
}

pub extern "win64" fn IoFreeIrp(irp: *mut u8) {
    pool::free_tracked(irp);
}

/// IoCallDriver: step to the next stack location and call the driver's
/// dispatch routine for its major function.
pub extern "win64" fn IofCallDriver(dev: *mut u8, irp: *mut u8) -> NtStatus {
    unsafe {
        let loc: i8 = rd(irp, IRP_CURRENT_LOCATION);
        if loc <= 1 {
            super::ke::KeBugCheckEx(0x35, irp as u64, 0, 0, 0); // NO_MORE_IRP_STACK_LOCATIONS
        }
        wr::<i8>(irp, IRP_CURRENT_LOCATION, loc - 1);
        let sl: *mut u8 = rd::<*mut u8>(irp, IRP_CURRENT_STACK).sub(STACK_SIZE);
        wr(irp, IRP_CURRENT_STACK, sl);
        wr(sl, SL_DEVICE_OBJECT, dev);
        let driver: *mut u8 = rd(dev, DEV_DRIVER_OBJECT);
        let major = rd::<u8>(sl, SL_MAJOR) as usize;
        if major > IRP_MJ_MAXIMUM_FUNCTION {
            return STATUS_INVALID_DEVICE_REQUEST;
        }
        let f: extern "win64" fn(*mut u8, *mut u8) -> NtStatus = rd(driver, DRV_MAJOR_FUNCTION + 8 * major);
        f(dev, irp)
    }
}

/// IoCompleteRequest: walk back up the stack locations, calling each
/// completion routine that asked to be called for this outcome; a routine
/// returning STATUS_MORE_PROCESSING_REQUIRED stops the walk (it owns the IRP
/// again). At the top, report the result to whoever sent it (UserIosb,
/// UserEvent).
pub extern "win64" fn IofCompleteRequest(irp: *mut u8, _boost: i8) {
    unsafe {
        let count: i8 = rd(irp, IRP_STACK_COUNT);
        loop {
            let loc: i8 = rd(irp, IRP_CURRENT_LOCATION);
            if loc > count {
                break;
            }
            let sl: *mut u8 = rd(irp, IRP_CURRENT_STACK);
            let control: u8 = rd(sl, SL_CONTROL);
            wr::<u8>(irp, IRP_PENDING_RETURNED, control & SL_PENDING_RETURNED);
            // Up one location.
            wr::<i8>(irp, IRP_CURRENT_LOCATION, loc + 1);
            let above = sl.add(STACK_SIZE);
            wr(irp, IRP_CURRENT_STACK, above);
            let status: NtStatus = rd(irp, IRP_STATUS);
            let routine: Option<extern "win64" fn(*mut u8, *mut u8, *mut u8) -> NtStatus> = rd(sl, SL_COMPLETION_ROUTINE);
            let wanted = (status >= 0 && control & SL_INVOKE_ON_SUCCESS != 0)
                || (status < 0 && status != STATUS_CANCELLED && control & SL_INVOKE_ON_ERROR != 0)
                || (status == STATUS_CANCELLED && control & SL_INVOKE_ON_CANCEL != 0);
            if let (Some(r), true) = (routine, wanted) {
                let dev: *mut u8 = if loc + 1 <= count { rd(above, SL_DEVICE_OBJECT) } else { core::ptr::null_mut() };
                if r(dev, irp, rd(sl, SL_CONTEXT)) == STATUS_MORE_PROCESSING_REQUIRED {
                    return;
                }
            } else if control & SL_PENDING_RETURNED != 0 && loc + 1 <= count {
                // IoMarkIrpPending travels up to the next driver.
                wr::<u8>(above, SL_CONTROL, rd::<u8>(above, SL_CONTROL) | SL_PENDING_RETURNED);
            }
        }
        let iosb: *mut u8 = rd(irp, IRP_USER_IOSB);
        if !iosb.is_null() {
            wr::<NtStatus>(iosb, 0, rd(irp, IRP_STATUS));
            wr::<u64>(iosb, 8, rd(irp, IRP_INFORMATION));
        }
        let built = x86_64::instructions::interrupts::without_interrupts(|| BUILT.lock().remove(&(irp as usize)));
        if let Some(b) = &built {
            b.finish(irp);
        }
        let ev: *mut u8 = rd(irp, IRP_USER_EVENT);
        if !ev.is_null() {
            super::ke::KeSetEvent(ev, 0, 0);
        }
        if built.is_some() {
            IoFreeIrp(irp);
        }
    }
}

/// Send an IRP to `dev` (for `file`, a FILE_OBJECT or null) and wait for it
/// to complete. `setup` fills the driver's stack location and the IRP's
/// buffer fields; `finish` reads what the driver left before the IRP is
/// freed. Returns the final status and IoStatus.Information.
pub fn send(
    dev: *mut u8,
    file: *mut u8,
    major: u8,
    setup: impl FnOnce(*mut u8, *mut u8),
    finish: impl FnOnce(*mut u8),
) -> (NtStatus, u64) {
    unsafe {
        let irp = IoAllocateIrp(rd(dev, DEV_STACK_SIZE), 0);
        let ev = pool::alloc_tracked(0x18);
        let iosb = pool::alloc_tracked(16);
        if irp.is_null() || ev.is_null() || iosb.is_null() {
            pool::free_tracked(irp);
            pool::free_tracked(ev);
            pool::free_tracked(iosb);
            return (STATUS_NO_MEMORY, 0);
        }
        super::ke::KeInitializeEvent(ev, 0, 0);
        wr(irp, IRP_USER_EVENT, ev);
        wr(irp, IRP_USER_IOSB, iosb);
        wr(irp, IRP_ORIGINAL_FILE_OBJECT, file);
        let sl = rd::<*mut u8>(irp, IRP_CURRENT_STACK).sub(STACK_SIZE);
        wr::<u8>(sl, SL_MAJOR, major);
        wr(sl, SL_FILE_OBJECT, file);
        setup(irp, sl);
        let mut status = IofCallDriver(dev, irp);
        if status == STATUS_PENDING {
            super::ke::KeWaitForSingleObject(ev, 0, 0, 0, core::ptr::null());
            status = rd(iosb, 0);
        }
        let info = rd::<u64>(irp, IRP_INFORMATION);
        finish(irp);
        IoFreeIrp(irp);
        pool::free_tracked(ev);
        pool::free_tracked(iosb);
        (status, info)
    }
}

/// A METHOD_BUFFERED device control from the kernel itself (the terminal's
/// DRIVER IOCTL): the reply, at most `out_len` bytes.
pub fn device_control(dev: *mut u8, code: u32, input: &[u8], out_len: usize) -> (NtStatus, Vec<u8>) {
    let buf = pool::alloc_tracked(input.len().max(out_len).max(1));
    if buf.is_null() {
        return (STATUS_NO_MEMORY, Vec::new());
    }
    unsafe { core::ptr::copy_nonoverlapping(input.as_ptr(), buf, input.len()) };
    let mut out = Vec::new();
    let (status, _) = send(
        dev,
        core::ptr::null_mut(),
        IRP_MJ_DEVICE_CONTROL,
        |irp, sl| unsafe {
            wr(irp, IRP_SYSTEM_BUFFER, buf);
            wr::<u32>(sl, SL_OUTPUT_LENGTH, out_len as u32);
            wr::<u32>(sl, SL_INPUT_LENGTH, input.len() as u32);
            wr::<u32>(sl, SL_IOCTL_CODE, code);
        },
        |irp| unsafe {
            let info = (rd::<u64>(irp, IRP_INFORMATION) as usize).min(out_len);
            out = core::slice::from_raw_parts(buf, info).to_vec();
        },
    );
    pool::free_tracked(buf);
    (status, out)
}

/// IRP_MJ_CREATE or IRP_MJ_CLOSE to `dev` with no file object (the
/// terminal's DRIVER OPEN).
pub fn create_close(dev: *mut u8, major: u8) -> NtStatus {
    send(dev, core::ptr::null_mut(), major, |_, _| {}, |_| {}).0
}

// -----| device handles for Windows programs |-----

/// A program's handle to a driver's device: CreateFile("\\.\Name") made
/// IRP_MJ_CREATE with this FILE_OBJECT; closing the last handle sends
/// IRP_MJ_CLEANUP and IRP_MJ_CLOSE and frees it.
pub struct DeviceHandle {
    dev: usize,
    file: usize,
}

// The FILE_OBJECT and device are pool memory the driver owns jointly with
// the I/O manager; the handle only passes the pointers along.
unsafe impl Send for DeviceHandle {}
unsafe impl Sync for DeviceHandle {}

impl Drop for DeviceHandle {
    fn drop(&mut self) {
        let (dev, file) = (self.dev as *mut u8, self.file as *mut u8);
        let _ = send(dev, file, IRP_MJ_CLEANUP, |_, _| {}, |_| {});
        let _ = send(dev, file, IRP_MJ_CLOSE, |_, _| {}, |_| {});
        unsafe { pool::free_tracked(rd(file, FO_FILE_NAME + 8)) };
        pool::free_tracked(file);
    }
}

/// NtCreateFile on a device name ("\??\Name", "\Device\Name"): None when no
/// driver made a device of that name (the name is then a file path).
pub fn open_device(nt_name: &str) -> Option<Result<DeviceHandle, NtStatus>> {
    // Requests go to the top of the device's stack, as on Windows: opening a
    // PDO's name reaches the function driver attached above it.
    let dev = IoGetAttachedDevice(device_by_name(nt_name)?);
    let file = pool::alloc_tracked(FO_SIZE);
    if file.is_null() {
        return Some(Err(STATUS_NO_MEMORY));
    }
    unsafe {
        wr::<u16>(file, 0, 5); // IO_TYPE_FILE
        wr::<u16>(file, 2, FO_SIZE as u16);
        wr(file, FO_DEVICE_OBJECT, dev);
        // FileName: what follows the device name (nothing here).
        let name_buf = pool::alloc_tracked(2);
        wr::<u16>(file, FO_FILE_NAME + 2, 2);
        wr(file, FO_FILE_NAME + 8, name_buf);
    }
    let (status, _) = send(dev, file, IRP_MJ_CREATE, |_, _| {}, |_| {});
    if status < 0 {
        unsafe { pool::free_tracked(rd(file, FO_FILE_NAME + 8)) };
        pool::free_tracked(file);
        return Some(Err(status));
    }
    Some(Ok(DeviceHandle { dev: dev as usize, file: file as usize }))
}

/// ZwDeviceIoControlFile from a driver: its buffers are kernel memory.
/// Buffered through pool; direct with an MDL over the output buffer;
/// METHOD_NEITHER with the pointers as they are.
pub fn kernel_device_control(h: &DeviceHandle, code: u32, input: u64, in_len: usize, output: u64, out_len: usize) -> (NtStatus, u64) {
    let (dev, file) = (h.dev as *mut u8, h.file as *mut u8);
    match code & 3 {
        METHOD_NEITHER => send(
            dev,
            file,
            IRP_MJ_DEVICE_CONTROL,
            |irp, sl| unsafe {
                wr::<u64>(irp, IRP_USER_BUFFER, output);
                wr::<u32>(sl, SL_OUTPUT_LENGTH, out_len as u32);
                wr::<u32>(sl, SL_INPUT_LENGTH, in_len as u32);
                wr::<u32>(sl, SL_IOCTL_CODE, code);
                wr::<u64>(sl, SL_TYPE3_INPUT, input);
            },
            |_| {},
        ),
        method => {
            let direct = method != METHOD_BUFFERED;
            let size = if direct { in_len } else { in_len.max(out_len) };
            let buf = pool::alloc_tracked(size.max(1));
            if buf.is_null() {
                return (STATUS_NO_MEMORY, 0);
            }
            unsafe { core::ptr::copy_nonoverlapping(input as *const u8, buf, in_len) };
            let mdl = if direct && out_len > 0 {
                let m = super::mm::IoAllocateMdl(output, out_len as u32, 0, 0, core::ptr::null_mut());
                if !m.is_null() {
                    super::mm::MmBuildMdlForNonPagedPool(m);
                }
                m
            } else {
                core::ptr::null_mut()
            };
            let r = send(
                dev,
                file,
                IRP_MJ_DEVICE_CONTROL,
                |irp, sl| unsafe {
                    wr(irp, IRP_SYSTEM_BUFFER, buf);
                    wr(irp, IRP_MDL_ADDRESS, mdl);
                    wr::<u32>(sl, SL_OUTPUT_LENGTH, out_len as u32);
                    wr::<u32>(sl, SL_INPUT_LENGTH, in_len as u32);
                    wr::<u32>(sl, SL_IOCTL_CODE, code);
                },
                |irp| unsafe {
                    if !direct {
                        let n = (rd::<u64>(irp, IRP_INFORMATION) as usize).min(out_len);
                        core::ptr::copy_nonoverlapping(buf, output as *mut u8, n);
                    }
                },
            );
            if !mdl.is_null() {
                super::mm::IoFreeMdl(mdl);
            }
            pool::free_tracked(buf);
            r
        }
    }
}

/// NtDeviceIoControlFile for a program. METHOD_BUFFERED copies through a
/// pool buffer; the direct methods give the output buffer as an MDL of the
/// program's pages; METHOD_NEITHER hands the driver the program's own
/// pointers, which it reads with SMAP lifted, as Windows drivers expect.
pub fn program_device_control(h: &DeviceHandle, iosb: u64, code: u32, input: u64, in_len: usize, output: u64, out_len: usize) -> NtStatus {
    use crate::nt::file::ptr_ok;
    let (dev, file) = (h.dev as *mut u8, h.file as *mut u8);
    if (in_len > 0 && !ptr_ok(input, in_len)) || (out_len > 0 && !ptr_ok(output, out_len)) {
        return STATUS_ACCESS_VIOLATION;
    }
    let (status, info) = match code & 3 {
        METHOD_BUFFERED => {
            let buf = pool::alloc_tracked(in_len.max(out_len).max(1));
            if buf.is_null() {
                return STATUS_NO_MEMORY;
            }
            crate::arch::smp::user_access(|| unsafe { core::ptr::copy_nonoverlapping(input as *const u8, buf, in_len) });
            let r = send(
                dev,
                file,
                IRP_MJ_DEVICE_CONTROL,
                |irp, sl| unsafe {
                    wr(irp, IRP_SYSTEM_BUFFER, buf);
                    wr::<u32>(sl, SL_OUTPUT_LENGTH, out_len as u32);
                    wr::<u32>(sl, SL_INPUT_LENGTH, in_len as u32);
                    wr::<u32>(sl, SL_IOCTL_CODE, code);
                },
                |irp| unsafe {
                    let n = (rd::<u64>(irp, IRP_INFORMATION) as usize).min(out_len);
                    crate::arch::smp::user_access(|| core::ptr::copy_nonoverlapping(buf, output as *mut u8, n));
                },
            );
            pool::free_tracked(buf);
            r
        }
        METHOD_NEITHER => crate::arch::smp::user_access(|| {
            send(
                dev,
                file,
                IRP_MJ_DEVICE_CONTROL,
                |irp, sl| unsafe {
                    wr::<u64>(irp, IRP_USER_BUFFER, output);
                    wr::<u32>(sl, SL_OUTPUT_LENGTH, out_len as u32);
                    wr::<u32>(sl, SL_INPUT_LENGTH, in_len as u32);
                    wr::<u32>(sl, SL_IOCTL_CODE, code);
                    wr::<u64>(sl, SL_TYPE3_INPUT, input);
                },
                |_| {},
            )
        }),
        METHOD_IN_DIRECT | METHOD_OUT_DIRECT => {
            // Input through a pool buffer, the output buffer through an MDL
            // of the program's own pages (mapped by the driver with
            // MmGetSystemAddressForMdlSafe).
            let buf = pool::alloc_tracked(in_len.max(1));
            if buf.is_null() {
                return STATUS_NO_MEMORY;
            }
            crate::arch::smp::user_access(|| unsafe { core::ptr::copy_nonoverlapping(input as *const u8, buf, in_len) });
            let mdl = if out_len > 0 { super::mm::lock_user_buffer(output, out_len) } else { Ok(core::ptr::null_mut()) };
            let r = match mdl {
                Err(s) => (s, 0),
                Ok(mdl) => {
                    let r = send(
                        dev,
                        file,
                        IRP_MJ_DEVICE_CONTROL,
                        |irp, sl| unsafe {
                            wr(irp, IRP_SYSTEM_BUFFER, buf);
                            wr(irp, IRP_MDL_ADDRESS, mdl);
                            wr::<u32>(sl, SL_OUTPUT_LENGTH, out_len as u32);
                            wr::<u32>(sl, SL_INPUT_LENGTH, in_len as u32);
                            wr::<u32>(sl, SL_IOCTL_CODE, code);
                        },
                        |_| {},
                    );
                    if !mdl.is_null() {
                        super::mm::MmUnlockPages(mdl);
                        super::mm::IoFreeMdl(mdl);
                    }
                    r
                }
            };
            pool::free_tracked(buf);
            r
        }
        _ => (super::STATUS_NOT_IMPLEMENTED, 0),
    };
    if ptr_ok(iosb, 16) {
        crate::arch::smp::user_access(|| unsafe {
            (iosb as *mut u64).write(status as u32 as u64);
            (iosb as *mut u64).add(1).write(info);
        });
    }
    status
}

// -----| device stacks |-----

/// Each attached device object and the one it is attached to (Windows keeps
/// this in DEVOBJ_EXTENSION.AttachedTo, which drivers don't read).
static ATTACHED_TO: Mutex<BTreeMap<usize, usize>> = Mutex::new(BTreeMap::new());

/// The device object at the top of the stack `dev` is in.
pub extern "win64" fn IoGetAttachedDevice(dev: *mut u8) -> *mut u8 {
    let mut d = dev;
    unsafe {
        loop {
            let up: *mut u8 = rd(d, DEV_ATTACHED_DEVICE);
            if up.is_null() {
                return d;
            }
            d = up;
        }
    }
}

/// Reference counts aren't kept (ob.rs), so this is IoGetAttachedDevice.
pub extern "win64" fn IoGetAttachedDeviceReference(dev: *mut u8) -> *mut u8 {
    IoGetAttachedDevice(dev)
}

/// The bottom of the stack `dev` is in (for a PnP device, its PDO).
pub fn stack_base(dev: *mut u8) -> *mut u8 {
    let map = ATTACHED_TO.lock();
    let mut d = dev as usize;
    while let Some(&lower) = map.get(&d) {
        d = lower;
    }
    d as *mut u8
}

pub extern "win64" fn IoGetDeviceAttachmentBaseRef(dev: *mut u8) -> *mut u8 {
    stack_base(dev)
}

/// Attach `src` above the top of `target`'s stack: the device it went on.
pub extern "win64" fn IoAttachDeviceToDeviceStack(src: *mut u8, target: *mut u8) -> *mut u8 {
    let top = IoGetAttachedDevice(target);
    unsafe {
        wr(top, DEV_ATTACHED_DEVICE, src);
        wr::<i8>(src, DEV_STACK_SIZE, rd::<i8>(top, DEV_STACK_SIZE) + 1);
        let align = rd::<u32>(top, DEV_ALIGNMENT).max(rd::<u32>(src, DEV_ALIGNMENT));
        wr::<u32>(src, DEV_ALIGNMENT, align);
    }
    x86_64::instructions::interrupts::without_interrupts(|| ATTACHED_TO.lock().insert(src as usize, top as usize));
    top
}

pub extern "win64" fn IoAttachDeviceToDeviceStackSafe(src: *mut u8, target: *mut u8, out: *mut *mut u8) -> NtStatus {
    let top = IoAttachDeviceToDeviceStack(src, target);
    if !out.is_null() {
        unsafe { out.write(top) };
    }
    STATUS_SUCCESS
}

/// Detach whatever is attached above `target`.
pub extern "win64" fn IoDetachDevice(target: *mut u8) {
    unsafe {
        let up: *mut u8 = rd(target, DEV_ATTACHED_DEVICE);
        wr::<*mut u8>(target, DEV_ATTACHED_DEVICE, core::ptr::null_mut());
        if !up.is_null() {
            x86_64::instructions::interrupts::without_interrupts(|| ATTACHED_TO.lock().remove(&(up as usize)));
        }
    }
}

/// A device's name, if it has one.
pub fn name_of(dev: *mut u8) -> Option<String> {
    NAMES.lock().iter().find(|(_, p)| **p == dev as usize).map(|(n, _)| n.clone())
}

/// IoEnumerateDeviceObjectList(DriverObject, Array, ArraySize (bytes),
/// ActualNumber).
pub extern "win64" fn IoEnumerateDeviceObjectList(driver: *mut u8, array: *mut *mut u8, size: u32, actual: *mut u32) -> NtStatus {
    let mut list = Vec::new();
    unsafe {
        let mut d: *mut u8 = rd(driver, DRV_DEVICE_OBJECT);
        while !d.is_null() {
            list.push(d);
            d = rd(d, DEV_NEXT_DEVICE);
        }
        if !actual.is_null() {
            actual.write(list.len() as u32);
        }
        if (size as usize) < list.len() * 8 {
            return STATUS_BUFFER_TOO_SMALL;
        }
        for (i, d) in list.iter().enumerate() {
            array.add(i).write(*d);
        }
    }
    STATUS_SUCCESS
}

/// IoGetDeviceObjectPointer(ObjectName, DesiredAccess, FileObject*,
/// DeviceObject*): opens the device (IRP_MJ_CREATE) and gives the top of
/// its stack. The open stays for the driver's life (no reference counts).
pub extern "win64" fn IoGetDeviceObjectPointer(name: *const u8, _access: u32, file_out: *mut *mut u8, dev_out: *mut *mut u8) -> NtStatus {
    let n = rtl::unicode_to_string(name);
    match open_device(&n) {
        None => super::STATUS_OBJECT_NAME_NOT_FOUND,
        Some(Err(s)) => s,
        Some(Ok(h)) => {
            let h = core::mem::ManuallyDrop::new(h);
            unsafe {
                if !file_out.is_null() {
                    file_out.write(h.file as *mut u8);
                }
                if !dev_out.is_null() {
                    dev_out.write(h.dev as *mut u8);
                }
            }
            STATUS_SUCCESS
        }
    }
}

// -----| IRPs the I/O manager builds for drivers |-----

/// An IRP from IoBuildDeviceIoControlRequest or IoBuildSynchronousFsdRequest:
/// what IoCompleteRequest does before freeing it.
struct Built {
    /// Pool buffer to copy back to `out` (buffered output) and free.
    system: usize,
    out: usize,
    out_len: usize,
    copy_back: bool,
    mdl: usize,
}

impl Built {
    fn finish(&self, irp: *mut u8) {
        unsafe {
            if self.copy_back && rd::<NtStatus>(irp, IRP_STATUS) >= 0 && self.out != 0 {
                let n = (rd::<u64>(irp, IRP_INFORMATION) as usize).min(self.out_len);
                core::ptr::copy_nonoverlapping(self.system as *const u8, self.out as *mut u8, n);
            }
        }
        if self.system != 0 {
            pool::free_tracked(self.system as *mut u8);
        }
        if self.mdl != 0 {
            super::mm::IoFreeMdl(self.mdl as *mut u8);
        }
    }
}

static BUILT: Mutex<BTreeMap<usize, Built>> = Mutex::new(BTreeMap::new());

/// The stack location the next IoCallDriver will use.
unsafe fn next_location(irp: *mut u8) -> *mut u8 {
    unsafe { rd::<*mut u8>(irp, IRP_CURRENT_STACK).sub(STACK_SIZE) }
}

/// IoBuildDeviceIoControlRequest(IoControlCode, DeviceObject, InputBuffer,
/// InputBufferLength, OutputBuffer, OutputBufferLength,
/// InternalDeviceIoControl, Event, IoStatusBlock): kernel buffers.
#[allow(clippy::too_many_arguments)]
pub extern "win64" fn IoBuildDeviceIoControlRequest(
    code: u32,
    dev: *mut u8,
    input: *const u8,
    in_len: u32,
    output: *mut u8,
    out_len: u32,
    internal: u8,
    event: *mut u8,
    iosb: *mut u8,
) -> *mut u8 {
    let irp = IoAllocateIrp(unsafe { rd(dev, DEV_STACK_SIZE) }, 0);
    if irp.is_null() {
        return irp;
    }
    let (in_len, out_len) = (in_len as usize, out_len as usize);
    let mut b = Built { system: 0, out: output as usize, out_len, copy_back: false, mdl: 0 };
    unsafe {
        wr(irp, IRP_USER_EVENT, event);
        wr(irp, IRP_USER_IOSB, iosb);
        wr(irp, IRP_USER_BUFFER, output);
        let sl = next_location(irp);
        wr::<u8>(sl, SL_MAJOR, if internal != 0 { IRP_MJ_INTERNAL_DEVICE_CONTROL } else { IRP_MJ_DEVICE_CONTROL });
        wr::<u32>(sl, SL_OUTPUT_LENGTH, out_len as u32);
        wr::<u32>(sl, SL_INPUT_LENGTH, in_len as u32);
        wr::<u32>(sl, SL_IOCTL_CODE, code);
        match code & 3 {
            METHOD_BUFFERED => {
                let size = in_len.max(out_len);
                if size > 0 {
                    let buf = pool::alloc_tracked(size);
                    if !input.is_null() {
                        core::ptr::copy_nonoverlapping(input, buf, in_len);
                    }
                    wr(irp, IRP_SYSTEM_BUFFER, buf);
                    b.system = buf as usize;
                    b.copy_back = out_len > 0;
                }
            }
            METHOD_NEITHER => wr(sl, SL_TYPE3_INPUT, input),
            _ => {
                if in_len > 0 {
                    let buf = pool::alloc_tracked(in_len);
                    core::ptr::copy_nonoverlapping(input, buf, in_len);
                    wr(irp, IRP_SYSTEM_BUFFER, buf);
                    b.system = buf as usize;
                }
                if out_len > 0 && !output.is_null() {
                    let m = super::mm::IoAllocateMdl(output as u64, out_len as u32, 0, 0, core::ptr::null_mut());
                    if !m.is_null() {
                        super::mm::MmBuildMdlForNonPagedPool(m);
                    }
                    wr(irp, IRP_MDL_ADDRESS, m);
                    b.mdl = m as usize;
                }
            }
        }
    }
    x86_64::instructions::interrupts::without_interrupts(|| BUILT.lock().insert(irp as usize, b));
    irp
}

/// IoBuildSynchronousFsdRequest(MajorFunction, DeviceObject, Buffer,
/// Length, StartingOffset, Event, IoStatusBlock): read, write, flush or
/// shutdown, buffered or direct as the device asks.
pub extern "win64" fn IoBuildSynchronousFsdRequest(
    major: u32,
    dev: *mut u8,
    buffer: *mut u8,
    len: u32,
    offset: *const i64,
    event: *mut u8,
    iosb: *mut u8,
) -> *mut u8 {
    let irp = IoAllocateIrp(unsafe { rd(dev, DEV_STACK_SIZE) }, 0);
    if irp.is_null() {
        return irp;
    }
    let mut b = Built { system: 0, out: buffer as usize, out_len: len as usize, copy_back: false, mdl: 0 };
    unsafe {
        wr(irp, IRP_USER_EVENT, event);
        wr(irp, IRP_USER_IOSB, iosb);
        wr(irp, IRP_USER_BUFFER, buffer);
        let sl = next_location(irp);
        wr::<u8>(sl, SL_MAJOR, major as u8);
        let rw = major as u8 == IRP_MJ_READ || major as u8 == IRP_MJ_WRITE;
        if rw {
            // Parameters.Read/Write: Length 0, Key 4, ByteOffset 8.
            wr::<u32>(sl, SL_PARAMETERS, len);
            wr::<i64>(sl, SL_PARAMETERS + 0x10, if offset.is_null() { 0 } else { offset.read() });
            let flags: u32 = rd(dev, DEV_FLAGS);
            if flags & DO_BUFFERED_IO != 0 && len > 0 {
                let buf = pool::alloc_tracked(len as usize);
                if major as u8 == IRP_MJ_WRITE {
                    core::ptr::copy_nonoverlapping(buffer, buf, len as usize);
                }
                wr(irp, IRP_SYSTEM_BUFFER, buf);
                b.system = buf as usize;
                b.copy_back = major as u8 == IRP_MJ_READ;
            } else if flags & DO_DIRECT_IO != 0 && len > 0 {
                let m = super::mm::IoAllocateMdl(buffer as u64, len, 0, 0, core::ptr::null_mut());
                if !m.is_null() {
                    super::mm::MmBuildMdlForNonPagedPool(m);
                }
                wr(irp, IRP_MDL_ADDRESS, m);
                b.mdl = m as usize;
            }
        }
    }
    x86_64::instructions::interrupts::without_interrupts(|| BUILT.lock().insert(irp as usize, b));
    irp
}

extern "win64" fn forward_done(_dev: *mut u8, _irp: *mut u8, event: *mut u8) -> NtStatus {
    super::ke::KeSetEvent(event, 0, 0);
    STATUS_MORE_PROCESSING_REQUIRED
}

/// IoForwardIrpSynchronously(DeviceObject, Irp): the current stack location
/// copied to the next, sent to `dev`, and waited for. TRUE once it's back.
pub extern "win64" fn IoForwardIrpSynchronously(dev: *mut u8, irp: *mut u8) -> u8 {
    let ev = pool::alloc_tracked(0x18);
    if ev.is_null() {
        return 0;
    }
    super::ke::KeInitializeEvent(ev, 0, 0);
    unsafe {
        let cur: *mut u8 = rd(irp, IRP_CURRENT_STACK);
        let next = cur.sub(STACK_SIZE);
        core::ptr::copy_nonoverlapping(cur, next, SL_COMPLETION_ROUTINE);
        wr::<u8>(next, SL_CONTROL, SL_INVOKE_ON_SUCCESS | SL_INVOKE_ON_ERROR | SL_INVOKE_ON_CANCEL);
        wr::<u64>(next, SL_COMPLETION_ROUTINE, forward_done as *const () as u64);
        wr(next, SL_CONTEXT, ev);
    }
    if IofCallDriver(dev, irp) == STATUS_PENDING {
        super::ke::KeWaitForSingleObject(ev, 0, 0, 0, core::ptr::null());
    }
    pool::free_tracked(ev);
    1
}

/// PoCallDriver: power IRPs need nothing special here.
pub extern "win64" fn PoCallDriver(dev: *mut u8, irp: *mut u8) -> NtStatus {
    IofCallDriver(dev, irp)
}

pub extern "win64" fn PoStartNextPowerIrp(_irp: *mut u8) {}

// -----| cancelling |-----

/// The cancel spin lock (a KSPIN_LOCK).
static CANCEL_LOCK: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

fn cancel_lock() -> *mut u64 {
    CANCEL_LOCK.as_ptr()
}

pub extern "win64" fn IoAcquireCancelSpinLock(irql: *mut u8) {
    let old = super::ke::KeAcquireSpinLockRaiseToDpc(cancel_lock());
    if !irql.is_null() {
        unsafe { irql.write(old) };
    }
}

pub extern "win64" fn IoReleaseCancelSpinLock(irql: u8) {
    super::ke::KeReleaseSpinLock(cancel_lock(), irql);
}

/// IoCancelIrp: mark it cancelled and call its cancel routine (with the
/// cancel spin lock held, which the routine releases). TRUE if it had one.
pub extern "win64" fn IoCancelIrp(irp: *mut u8) -> u8 {
    unsafe {
        wr::<u8>(irp, IRP_CANCEL, 1);
        let mut irql = 0u8;
        IoAcquireCancelSpinLock(&mut irql);
        let slot = &*(irp.add(IRP_CANCEL_ROUTINE) as *const core::sync::atomic::AtomicU64);
        let routine = slot.swap(0, core::sync::atomic::Ordering::AcqRel);
        if routine == 0 {
            IoReleaseCancelSpinLock(irql);
            return 0;
        }
        wr::<u8>(irp, IRP_CANCEL_IRQL, irql);
        let sl: *mut u8 = rd(irp, IRP_CURRENT_STACK);
        let dev: *mut u8 = if rd::<i8>(irp, IRP_CURRENT_LOCATION) <= rd::<i8>(irp, IRP_STACK_COUNT) { rd(sl, SL_DEVICE_OBJECT) } else { core::ptr::null_mut() };
        let f: extern "win64" fn(*mut u8, *mut u8) = core::mem::transmute(routine);
        f(dev, irp);
        1
    }
}

// -----| processes and threads, as far as W1's drivers ask |-----

static FAKE_PROCESS: [u8; 0x800] = [0; 0x800];

pub extern "win64" fn IoGetCurrentProcess() -> *const u8 {
    FAKE_PROCESS.as_ptr()
}

pub extern "win64" fn PsGetCurrentProcessId() -> u64 {
    crate::sched::kthread::current_pid() as u64 * 4
}

pub extern "win64" fn PsGetCurrentThreadId() -> u64 {
    (crate::arch::smp::cpu_index() as u64 + 1) * 4
}

pub extern "win64" fn IoIsWdmVersionAvailable(major: u8, _minor: u8) -> u8 {
    (major <= 6) as u8
}
