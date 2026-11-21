// The I/O manager's part drivers see: DRIVER_OBJECT, DEVICE_OBJECT, IRPs.
//
// Offsets are Windows x64's (WDK wdm.h); win32/tests/k1/layout.c asserts
// each one against the real headers.

use alloc::{collections::BTreeMap, string::String, vec::Vec};
use spin::Mutex;

use super::{pool, rtl, NtStatus, STATUS_INVALID_DEVICE_REQUEST, STATUS_NO_MEMORY, STATUS_OBJECT_NAME_COLLISION, STATUS_PENDING, STATUS_SUCCESS};

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
pub const DEV_FLAGS: usize = 0x30;
pub const DEV_CHARACTERISTICS: usize = 0x34;
pub const DEV_EXTENSION: usize = 0x40;
pub const DEV_TYPE: usize = 0x48;
pub const DEV_STACK_SIZE: usize = 0x4C;
pub const DEV_OBJECT_EXTENSION: usize = 0x138;
const DO_DEVICE_INITIALIZING: u32 = 0x80;

// IRP (0xD0 bytes, then its stack locations)
pub const IRP_SIZE: usize = 0xD0;
pub const IRP_MDL_ADDRESS: usize = 0x08;
pub const IRP_SYSTEM_BUFFER: usize = 0x18;
pub const IRP_STATUS: usize = 0x30;
pub const IRP_INFORMATION: usize = 0x38;
pub const IRP_PENDING_RETURNED: usize = 0x41;
pub const IRP_STACK_COUNT: usize = 0x42;
pub const IRP_CURRENT_LOCATION: usize = 0x43;
pub const IRP_USER_IOSB: usize = 0x48;
pub const IRP_USER_EVENT: usize = 0x50;
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
        let ev: *mut u8 = rd(irp, IRP_USER_EVENT);
        if !ev.is_null() {
            super::ke::KeSetEvent(ev, 0, 0);
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
    let dev = device_by_name(nt_name)?;
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
    use crate::nt_file::ptr_ok;
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
            crate::smp::user_access(|| unsafe { core::ptr::copy_nonoverlapping(input as *const u8, buf, in_len) });
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
                    crate::smp::user_access(|| core::ptr::copy_nonoverlapping(buf, output as *mut u8, n));
                },
            );
            pool::free_tracked(buf);
            r
        }
        METHOD_NEITHER => crate::smp::user_access(|| {
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
            crate::smp::user_access(|| unsafe { core::ptr::copy_nonoverlapping(input as *const u8, buf, in_len) });
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
        crate::smp::user_access(|| unsafe {
            (iosb as *mut u64).write(status as u32 as u64);
            (iosb as *mut u64).add(1).write(info);
        });
    }
    status
}

// -----| processes and threads, as far as W1's drivers ask |-----

static FAKE_PROCESS: [u8; 0x800] = [0; 0x800];

pub extern "win64" fn IoGetCurrentProcess() -> *const u8 {
    FAKE_PROCESS.as_ptr()
}

pub extern "win64" fn PsGetCurrentProcessId() -> u64 {
    crate::kthread::current_pid() as u64 * 4
}

pub extern "win64" fn PsGetCurrentThreadId() -> u64 {
    (crate::smp::cpu_index() as u64 + 1) * 4
}

pub extern "win64" fn IoIsWdmVersionAvailable(major: u8, _minor: u8) -> u8 {
    (major <= 6) as u8
}
