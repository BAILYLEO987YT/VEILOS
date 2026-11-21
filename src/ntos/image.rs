// Loading a driver image (.sys) and running it.

use alloc::{format, string::String, vec::Vec};
use spin::Mutex;

use super::{exports, io, pool, NtStatus};

const IMAGE_FILE_MACHINE_AMD64: u16 = 0x8664;
const DIR_IMPORT: usize = 1;
const DIR_BASERELOC: usize = 5;
const DIR_LOAD_CONFIG: usize = 10;
const DIR_DELAY_IMPORT: usize = 13;
/// The /GS cookie a linked image holds until its loader sets a real one.
const DEFAULT_COOKIE: u64 = 0x0000_2B99_2DDF_A232;

/// A loaded driver.
pub struct Driver {
    pub name: String,
    pub path: String,
    pub base: u64,
    pub size: u64,
    pub object: usize,
}

pub static DRIVERS: Mutex<Vec<Driver>> = Mutex::new(Vec::new());

fn u16_at(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}
fn u64_at(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().unwrap())
}

/// A mapped, relocated and linked image: where it is and its entry point.
pub struct Mapped {
    pub base: u64,
    pub size: u64,
    pub entry: u64,
}

/// Map `file` (a PE32+ native image) and bind it. Every import must be one
/// VEILOS exports: the error lists the ones that aren't.
pub fn map(file: &[u8]) -> Result<Mapped, String> {
    if file.len() < 0x40 || &file[0..2] != b"MZ" {
        return Err(String::from("not an MZ image"));
    }
    let pe = u32_at(file, 0x3C) as usize;
    if file.len() < pe + 0x108 || &file[pe..pe + 4] != b"PE\0\0" {
        return Err(String::from("no PE header"));
    }
    if u16_at(file, pe + 4) != IMAGE_FILE_MACHINE_AMD64 {
        return Err(String::from("not an x64 image"));
    }
    let nsec = u16_at(file, pe + 6) as usize;
    let opt = pe + 24;
    let opt_size = u16_at(file, pe + 20) as usize;
    if u16_at(file, opt) != 0x20B {
        return Err(String::from("not PE32+"));
    }
    let entry_rva = u32_at(file, opt + 16) as u64;
    let pref_base = u64_at(file, opt + 24);
    let image_size = u32_at(file, opt + 56) as u64;
    let headers = u32_at(file, opt + 60) as usize;
    let ndirs = u32_at(file, opt + 108) as usize;
    let dir = |i: usize| -> (u32, u32) {
        if i < ndirs { (u32_at(file, opt + 112 + 8 * i), u32_at(file, opt + 116 + 8 * i)) } else { (0, 0) }
    };

    let base = pool::image_space(image_size).ok_or_else(|| String::from("no memory for the image"))?;
    let img = unsafe { core::slice::from_raw_parts_mut(base as *mut u8, image_size as usize) };
    img[..headers.min(file.len())].copy_from_slice(&file[..headers.min(file.len())]);
    let sec = opt + opt_size;
    for i in 0..nsec {
        let s = sec + 40 * i;
        let va = u32_at(file, s + 12) as usize;
        let raw_size = u32_at(file, s + 16) as usize;
        let raw = u32_at(file, s + 20) as usize;
        let n = raw_size.min(file.len().saturating_sub(raw)).min(img.len().saturating_sub(va));
        img[va..va + n].copy_from_slice(&file[raw..raw + n]);
    }

    // Relocations: base-relative 64-bit addresses (type 10).
    let delta = base.wrapping_sub(pref_base);
    let (rel, rel_size) = dir(DIR_BASERELOC);
    if delta != 0 && rel != 0 {
        let mut at = rel as usize;
        let end = (rel + rel_size) as usize;
        while at + 8 <= end {
            let page = u32_at(img, at) as usize;
            let block = u32_at(img, at + 4) as usize;
            if block < 8 {
                break;
            }
            for k in 0..(block - 8) / 2 {
                let e = u16_at(img, at + 8 + 2 * k);
                match e >> 12 {
                    0 => {}
                    10 => {
                        let o = page + (e & 0xFFF) as usize;
                        let v = u64_at(img, o).wrapping_add(delta);
                        img[o..o + 8].copy_from_slice(&v.to_le_bytes());
                    }
                    t => return Err(format!("relocation type {} not handled", t)),
                }
            }
            at += block;
        }
    }

    // Imports (and delay-load imports, bound now).
    let mut missing: Vec<String> = Vec::new();
    let cstr = |img: &[u8], rva: usize| -> String {
        let end = img[rva..].iter().position(|&b| b == 0).map_or(img.len(), |p| rva + p);
        String::from_utf8_lossy(&img[rva..end]).into_owned()
    };
    let bind = |img: &mut [u8], dll: &str, names_rva: usize, iat_rva: usize, missing: &mut Vec<String>| {
        let mut k = 0;
        loop {
            let thunk = u64_at(img, names_rva + 8 * k);
            if thunk == 0 {
                break;
            }
            let addr = if thunk & (1 << 63) != 0 {
                missing.push(format!("{}!#{}", dll, thunk & 0xFFFF));
                0
            } else {
                let name = cstr(img, (thunk & 0x7FFF_FFFF) as usize + 2);
                exports::lookup(dll, &name).unwrap_or_else(|| {
                    missing.push(format!("{}!{}", dll, name));
                    0
                })
            };
            img[iat_rva + 8 * k..iat_rva + 8 * k + 8].copy_from_slice(&addr.to_le_bytes());
            k += 1;
        }
    };
    let (imp, _) = dir(DIR_IMPORT);
    if imp != 0 {
        let mut d = imp as usize;
        loop {
            let names = u32_at(img, d) as usize;
            let name = u32_at(img, d + 12) as usize;
            let iat = u32_at(img, d + 16) as usize;
            if name == 0 {
                break;
            }
            let dll = cstr(img, name).to_ascii_lowercase();
            bind(img, &dll, if names != 0 { names } else { iat }, iat, &mut missing);
            d += 20;
        }
    }
    let (dimp, _) = dir(DIR_DELAY_IMPORT);
    if dimp != 0 {
        let mut d = dimp as usize;
        loop {
            let name = u32_at(img, d + 4) as usize;
            let iat = u32_at(img, d + 12) as usize;
            let names = u32_at(img, d + 16) as usize;
            if name == 0 {
                break;
            }
            let dll = cstr(img, name).to_ascii_lowercase();
            bind(img, &dll, names, iat, &mut missing);
            d += 32;
        }
    }
    if !missing.is_empty() {
        return Err(format!("{} import(s) VEILOS doesn't have: {}", missing.len(), missing.join(", ")));
    }

    // The /GS cookie: Windows' loader sets it, and a driver whose cookie is
    // still the linker's default stops itself (bug check 0xF7).
    let (lc, lc_size) = dir(DIR_LOAD_CONFIG);
    if lc != 0 && lc_size >= 0x60 {
        let cookie_va = u64_at(img, lc as usize + 0x58);
        if cookie_va > base && cookie_va + 8 <= base + image_size {
            let o = (cookie_va - base) as usize;
            if u64_at(img, o) == DEFAULT_COOKIE {
                let mut r = [0u8; 8];
                crate::entropy::fill(&mut r);
                // The top 16 bits stay clear, as Windows' do.
                let v = (u64::from_le_bytes(r) & 0x0000_FFFF_FFFF_FFFF).max(1);
                img[o..o + 8].copy_from_slice(&v.to_le_bytes());
            }
        }
    }
    // Each section's protection, now that relocations, imports and the
    // cookie are written: code executable and read-only, data writable and
    // not executable, read-only data neither, headers read-only. A page two
    // sections share gets both's rights. (Ring 0 only honours read-only with
    // CR0.WP, which VEILOS doesn't set yet; no-execute always holds.)
    {
        use x86_64::structures::paging::PageTableFlags as F;
        let pages = (image_size as usize).div_ceil(4096);
        let mut flags: Vec<F> = alloc::vec![F::NO_EXECUTE; pages];
        let mut set = |va: usize, len: usize, f: F| {
            let (first, last) = (va / 4096, (va + len.max(1)).div_ceil(4096).min(pages));
            for p in first..last {
                if f.contains(F::WRITABLE) {
                    flags[p] |= F::WRITABLE;
                }
                if !f.contains(F::NO_EXECUTE) {
                    flags[p].remove(F::NO_EXECUTE);
                }
            }
        };
        for i in 0..nsec {
            let s = sec + 40 * i;
            let va = u32_at(file, s + 12) as usize;
            let vsize = (u32_at(file, s + 8) as usize).max(u32_at(file, s + 16) as usize);
            let ch = u32_at(file, s + 36);
            let mut f = F::NO_EXECUTE;
            if ch & 0x2000_0000 != 0 {
                f.remove(F::NO_EXECUTE); // IMAGE_SCN_MEM_EXECUTE
            }
            if ch & 0x8000_0000 != 0 {
                f |= F::WRITABLE; // IMAGE_SCN_MEM_WRITE
            }
            set(va, vsize, f);
        }
        pool::protect_pages(base, &flags);
    }
    Ok(Mapped { base, size: image_size, entry: base + entry_rva })
}

/// A UNICODE_STRING (and its buffer) in pool, for the life of the driver.
fn pool_unicode(s: &str) -> *mut u8 {
    let units: Vec<u16> = s.encode_utf16().chain(core::iter::once(0)).collect();
    let buf = pool::alloc_tracked(units.len() * 2);
    let us = pool::alloc_tracked(16);
    unsafe {
        core::ptr::copy_nonoverlapping(units.as_ptr(), buf as *mut u16, units.len());
        (us as *mut u16).write(((units.len() - 1) * 2) as u16);
        (us.add(2) as *mut u16).write((units.len() * 2) as u16);
        (us.add(8) as *mut *mut u8).write(buf);
    }
    us
}

/// Load the driver at `path` and run its DriverEntry. Returns its name and
/// DriverEntry's status.
pub fn load(path: &str) -> Result<(String, NtStatus), String> {
    let name = path.rsplit('/').next().unwrap_or(path).trim_end_matches(".sys").trim_end_matches(".SYS").to_ascii_lowercase();
    if DRIVERS.lock().iter().any(|d| d.name == name) {
        return Err(format!("{} is already loaded", name));
    }
    let file = crate::vfs::VFS.read_file(path).map_err(|e| format!("reading {}: {:?}", path, e))?;
    let m = map(&file)?;
    crate::serial_println!("[NTOS] {} mapped at {:#x} ({} KiB), entry {:#x}", name, m.base, m.size / 1024, m.entry);

    let drv = pool::alloc_tracked(io::DRV_SIZE);
    let ext = pool::alloc_tracked(0x80);
    if drv.is_null() || ext.is_null() {
        return Err(String::from("no memory for the driver object"));
    }
    let registry = pool_unicode(&format!("\\Registry\\Machine\\System\\CurrentControlSet\\Services\\{}", name));
    // Its Services key, which drivers read their settings from.
    let _ = crate::nt_registry::kernel_key(&format!(r"HKLM\SYSTEM\CurrentControlSet\Services\{}", name), true);
    let drv_name = pool_unicode(&format!("\\Driver\\{}", name));
    unsafe {
        (drv as *mut u16).write(4); // IO_TYPE_DRIVER
        (drv.add(2) as *mut u16).write(io::DRV_SIZE as u16);
        (drv.add(io::DRV_START) as *mut u64).write(m.base);
        (drv.add(io::DRV_IMAGE_SIZE) as *mut u32).write(m.size as u32);
        (drv.add(io::DRV_EXTENSION) as *mut *mut u8).write(ext);
        core::ptr::copy_nonoverlapping(drv_name, drv.add(io::DRV_NAME), 16);
        (drv.add(io::DRV_INIT) as *mut u64).write(m.entry);
        // Unhandled major functions fail the request, as Windows' default does.
        for k in 0..=io::IRP_MJ_MAXIMUM_FUNCTION {
            (drv.add(io::DRV_MAJOR_FUNCTION + 8 * k) as *mut u64).write(invalid_request as usize as u64);
        }
        (ext as *mut *mut u8).write(drv); // DRIVER_EXTENSION.DriverObject
    }
    DRIVERS.lock().push(Driver { name: name.clone(), path: String::from(path), base: m.base, size: m.size, object: drv as usize });

    let entry: extern "win64" fn(*mut u8, *mut u8) -> NtStatus = unsafe { core::mem::transmute(m.entry) };
    let status = entry(drv, registry);
    super::ke::set_irql(0);
    crate::serial_println!("[NTOS] {} DriverEntry returned {:#010x}", name, status as u32);
    if status < 0 {
        DRIVERS.lock().retain(|d| d.name != name);
    }
    Ok((name, status))
}

extern "win64" fn invalid_request(_dev: *mut u8, irp: *mut u8) -> NtStatus {
    unsafe {
        (irp.add(io::IRP_STATUS) as *mut NtStatus).write(super::STATUS_INVALID_DEVICE_REQUEST);
        (irp.add(io::IRP_INFORMATION) as *mut u64).write(0);
    }
    io::IofCompleteRequest(irp, 0);
    super::STATUS_INVALID_DEVICE_REQUEST
}

/// Call the driver's DriverUnload and forget it. Its image stays mapped
/// (a leak until image memory is freed, W2).
pub fn unload(name: &str) -> Result<(), String> {
    let obj = DRIVERS.lock().iter().find(|d| d.name == name).map(|d| d.object).ok_or_else(|| format!("{} isn't loaded", name))?;
    let drv = obj as *mut u8;
    let unload: u64 = unsafe { (drv.add(io::DRV_UNLOAD) as *const u64).read() };
    if unload == 0 {
        return Err(format!("{} has no unload routine", name));
    }
    let f: extern "win64" fn(*mut u8) = unsafe { core::mem::transmute(unload) };
    f(drv);
    super::ke::set_irql(0);
    let (base, size) = {
        let mut list = DRIVERS.lock();
        let at = list.iter().position(|d| d.name == name).unwrap();
        let d = list.remove(at);
        (d.base, d.size)
    };
    // Its image goes; whatever the driver left behind (a queued DPC, a
    // timer) would now point at nothing, as on Windows.
    let pages = pool::free_image(base, size);
    pool::free_tracked(unsafe { (drv.add(io::DRV_EXTENSION) as *const *mut u8).read() });
    pool::free_tracked(drv);
    crate::serial_println!("[NTOS] {} unloaded, {} image pages freed", name, pages);
    Ok(())
}
