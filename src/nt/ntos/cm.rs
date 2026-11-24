// The registry for drivers: Zw*Key on VEILOS's one registry tree
// (nt/registry.rs). NT names map to hives: \Registry\Machine\X is HKLM\X,
// \Registry\User\X is HKU\X. A driver's RegistryPath (its Services key)
// exists before DriverEntry runs (image.rs makes it).

use alloc::{format, string::String, vec::Vec};

use super::{ob, rtl, NtStatus, STATUS_INVALID_PARAMETER, STATUS_OBJECT_NAME_NOT_FOUND, STATUS_SUCCESS};

const STATUS_INVALID_HANDLE: NtStatus = 0xC000_0008u32 as i32;
const STATUS_BUFFER_OVERFLOW: NtStatus = 0x8000_0005u32 as i32;
const STATUS_BUFFER_TOO_SMALL: NtStatus = 0xC000_0023u32 as i32;
const REG_CREATED_NEW_KEY: u32 = 1;
const REG_OPENED_EXISTING_KEY: u32 = 2;

/// An NT registry name as a hive-rooted path, or None if it isn't one.
pub fn hive_path(nt: &str) -> Option<String> {
    let lower = nt.to_ascii_lowercase();
    for (prefix, hive) in [("\\registry\\machine", "HKLM"), ("\\registry\\user", "HKU")] {
        if let Some(rest) = lower.strip_prefix(prefix) {
            let rest = &nt[nt.len() - rest.len()..];
            return Some(format!("{}{}", hive, rest));
        }
    }
    None
}

/// The path an OBJECT_ATTRIBUTES names: ObjectName, relative to
/// RootDirectory (a key handle) when there is one.
fn path_of(oa: *const u8) -> Result<String, NtStatus> {
    if oa.is_null() {
        return Err(STATUS_INVALID_PARAMETER);
    }
    let (root, name) = unsafe { ((oa.add(8) as *const u64).read_unaligned(), (oa.add(0x10) as *const *const u8).read_unaligned()) };
    let name = rtl::unicode_to_string(name);
    if root != 0 {
        match ob::get(root) {
            Some(ob::Object::Key(base)) => Ok(if name.is_empty() { base } else { format!("{}\\{}", base, name.trim_start_matches('\\')) }),
            _ => Err(STATUS_INVALID_HANDLE),
        }
    } else {
        hive_path(&name).ok_or(STATUS_OBJECT_NAME_NOT_FOUND)
    }
}

fn key_of(h: u64) -> Result<String, NtStatus> {
    match ob::get(h) {
        Some(ob::Object::Key(p)) => Ok(p),
        _ => Err(STATUS_INVALID_HANDLE),
    }
}

/// ZwCreateKey(KeyHandle*, DesiredAccess, ObjectAttributes*, TitleIndex,
/// Class*, CreateOptions, Disposition*).
pub extern "win64" fn ZwCreateKey(out: *mut u64, _access: u32, oa: *const u8, _title: u32, _class: *const u8, _options: u32, disposition: *mut u32) -> NtStatus {
    let path = match path_of(oa) {
        Ok(p) => p,
        Err(s) => return s,
    };
    let Some(made) = crate::nt::registry::kernel_key(&path, true) else { return STATUS_OBJECT_NAME_NOT_FOUND };
    if !disposition.is_null() {
        unsafe { disposition.write(if made { REG_CREATED_NEW_KEY } else { REG_OPENED_EXISTING_KEY }) };
    }
    unsafe { out.write(ob::insert(ob::Object::Key(path))) };
    STATUS_SUCCESS
}

pub extern "win64" fn ZwOpenKey(out: *mut u64, _access: u32, oa: *const u8) -> NtStatus {
    let path = match path_of(oa) {
        Ok(p) => p,
        Err(s) => return s,
    };
    if crate::nt::registry::kernel_key(&path, false).is_none() {
        return STATUS_OBJECT_NAME_NOT_FOUND;
    }
    unsafe { out.write(ob::insert(ob::Object::Key(path))) };
    STATUS_SUCCESS
}

/// ZwQueryValueKey(KeyHandle, ValueName*, KeyValueInformationClass,
/// KeyValueInformation, Length, ResultLength*). Classes: 0 basic
/// {TitleIndex, Type, NameLength, Name}, 1 full {TitleIndex, Type,
/// DataOffset, DataLength, NameLength, Name, data}, 2 partial {TitleIndex,
/// Type, DataLength, Data}.
pub extern "win64" fn ZwQueryValueKey(h: u64, name: *const u8, class: u32, info: *mut u8, len: u32, result: *mut u32) -> NtStatus {
    let path = match key_of(h) {
        Ok(p) => p,
        Err(s) => return s,
    };
    let vname = rtl::unicode_to_string(name);
    let Some((kind, data)) = crate::nt::registry::kernel_get(&path, &vname) else { return STATUS_OBJECT_NAME_NOT_FOUND };
    value_info(&vname, kind, &data, class, info, len, result)
}

/// A KEY_VALUE_*_INFORMATION for one value into the caller's buffer:
/// classes 0 basic {TitleIndex, Type, NameLength, Name}, 1 full
/// {TitleIndex, Type, DataOffset, DataLength, NameLength, Name, data}, 2
/// partial {TitleIndex, Type, DataLength, Data}. The fixed part must fit;
/// then as much as fits, and STATUS_BUFFER_OVERFLOW.
pub fn value_info(vname: &str, kind: u32, data: &[u8], class: u32, info: *mut u8, len: u32, result: *mut u32) -> NtStatus {
    let name_bytes: Vec<u8> = vname.encode_utf16().flat_map(|c| c.to_le_bytes()).collect();
    let mut out: Vec<u8> = Vec::new();
    let push = |v: &mut Vec<u8>, x: u32| v.extend_from_slice(&x.to_le_bytes());
    match class {
        0 => {
            push(&mut out, 0);
            push(&mut out, kind);
            push(&mut out, name_bytes.len() as u32);
            out.extend_from_slice(&name_bytes);
        }
        1 => {
            let data_off = (20 + name_bytes.len()).next_multiple_of(8) as u32;
            push(&mut out, 0);
            push(&mut out, kind);
            push(&mut out, data_off);
            push(&mut out, data.len() as u32);
            push(&mut out, name_bytes.len() as u32);
            out.extend_from_slice(&name_bytes);
            out.resize(data_off as usize, 0);
            out.extend_from_slice(data);
        }
        2 => {
            push(&mut out, 0);
            push(&mut out, kind);
            push(&mut out, data.len() as u32);
            out.extend_from_slice(data);
        }
        _ => return STATUS_INVALID_PARAMETER,
    }
    put_info(&out, if class == 1 { 20 } else { 12 }, info, len, result)
}

/// Copy a built information block out: the fixed part must fit, then as
/// much as fits (BUFFER_OVERFLOW if not all of it).
pub fn put_info(out: &[u8], fixed: usize, info: *mut u8, len: u32, result: *mut u32) -> NtStatus {
    if !result.is_null() {
        unsafe { result.write(out.len() as u32) };
    }
    if (len as usize) < fixed {
        return STATUS_BUFFER_TOO_SMALL;
    }
    let n = out.len().min(len as usize);
    unsafe { core::ptr::copy_nonoverlapping(out.as_ptr(), info, n) };
    if n < out.len() { STATUS_BUFFER_OVERFLOW } else { STATUS_SUCCESS }
}

pub fn key_path(h: u64) -> Result<String, NtStatus> {
    key_of(h)
}

/// ZwSetValueKey(KeyHandle, ValueName*, TitleIndex, Type, Data, DataSize).
pub extern "win64" fn ZwSetValueKey(h: u64, name: *const u8, _title: u32, kind: u32, data: *const u8, size: u32) -> NtStatus {
    let path = match key_of(h) {
        Ok(p) => p,
        Err(s) => return s,
    };
    let bytes = if size == 0 { Vec::new() } else { unsafe { core::slice::from_raw_parts(data, size as usize) }.to_vec() };
    if crate::nt::registry::kernel_set(&path, &rtl::unicode_to_string(name), kind, bytes) { STATUS_SUCCESS } else { STATUS_OBJECT_NAME_NOT_FOUND }
}

pub extern "win64" fn ZwDeleteValueKey(h: u64, name: *const u8) -> NtStatus {
    let path = match key_of(h) {
        Ok(p) => p,
        Err(s) => return s,
    };
    if crate::nt::registry::kernel_delete(&path, Some(&rtl::unicode_to_string(name))) { STATUS_SUCCESS } else { STATUS_OBJECT_NAME_NOT_FOUND }
}

pub extern "win64" fn ZwDeleteKey(h: u64) -> NtStatus {
    let path = match key_of(h) {
        Ok(p) => p,
        Err(s) => return s,
    };
    if crate::nt::registry::kernel_delete(&path, None) { STATUS_SUCCESS } else { STATUS_OBJECT_NAME_NOT_FOUND }
}

/// RtlWriteRegistryValue(RelativeTo, Path, ValueName, ValueType,
/// ValueData, ValueLength). RelativeTo: 0 absolute, 1 services, 2 control,
/// 3 Windows NT, 4 device map; RTL_REGISTRY_OPTIONAL (0x80000000) ignored.
pub extern "win64" fn RtlWriteRegistryValue(rel: u32, path: *const u16, name: *const u16, kind: u32, data: *const u8, len: u32) -> NtStatus {
    let wide = |p: *const u16| -> String {
        if p.is_null() {
            return String::new();
        }
        let n = rtl::wcslen(p);
        char::decode_utf16(unsafe { core::slice::from_raw_parts(p, n) }.iter().copied()).map(|c| c.unwrap_or('?')).collect()
    };
    let (p, n) = (wide(path), wide(name));
    let base = match rel & 0xFFFF {
        0 => hive_path(&p),
        1 => Some(format!("HKLM\\SYSTEM\\CurrentControlSet\\Services\\{}", p)),
        2 => Some(format!("HKLM\\SYSTEM\\CurrentControlSet\\Control\\{}", p)),
        3 => Some(format!("HKLM\\SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion\\{}", p)),
        4 => Some(format!("HKLM\\HARDWARE\\DEVICEMAP\\{}", p)),
        _ => None,
    };
    let Some(base) = base else { return STATUS_INVALID_PARAMETER };
    let base = String::from(base.trim_end_matches('\\'));
    if crate::nt::registry::kernel_key(&base, true).is_none() {
        return STATUS_OBJECT_NAME_NOT_FOUND;
    }
    let bytes = if len == 0 { Vec::new() } else { unsafe { core::slice::from_raw_parts(data, len as usize) }.to_vec() };
    crate::nt::registry::kernel_set(&base, &n, kind, bytes);
    STATUS_SUCCESS
}
