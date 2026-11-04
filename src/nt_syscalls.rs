// =============================================================================
// VEILOS: NT system services for VEILOS's own ntdll.dll (WIN32_DESIGN.md)
// =============================================================================
//
// The numbers and structure layouts are shared with ntdll through the
// `nt_abi` crate. Arguments 1-4 arrive in registers (r10 copied to the first
// argument, rdx, r8, r9); the rest are read from the caller's stack with
// `interrupts::syscall_stack_arg`. Every service returns an NTSTATUS.
//
// Waiting services (console reads, delays, object waits) give the CPU to
// other threads while they wait and end the process if it's killed meanwhile.

use alloc::sync::Arc;
use alloc::vec::Vec;
use nt_abi::{status, sys, IoStatusBlock, NtStatus, CURRENT_PROCESS};

use crate::interrupts::syscall_stack_arg as stack;
use crate::nt_file::ptr_ok;
use crate::nt_objects::KernelObjectRef;
use crate::process::Process;

/// True for the syscall numbers this module serves.
pub fn handles(id: u64) -> bool {
    (sys::BASE as u64..sys::END as u64).contains(&id)
}

pub fn dispatch(id: u64, a0: u64, a1: u64, a2: u64, a3: u64) -> u64 {
    let Some(proc) = crate::process::PROCESSOR.lock().get_active() else {
        return ret(status::INVALID_HANDLE);
    };
    let s = match id as u32 {
        sys::NT_TERMINATE_PROCESS => terminate_process(&proc, a0, a1 as NtStatus),
        sys::NT_WRITE_FILE => io(&proc, a0, true),
        sys::NT_READ_FILE => io(&proc, a0, false),
        sys::NT_CLOSE => close(&proc, a0),
        sys::NT_DELAY_EXECUTION => delay_execution(a1),
        sys::NT_ALLOCATE_VIRTUAL_MEMORY => allocate_vm(&proc, a0, a1, a3),
        sys::NT_FREE_VIRTUAL_MEMORY => free_vm(&proc, a0, a1, a2, a3),
        sys::NT_PROTECT_VIRTUAL_MEMORY => protect_vm(&proc, a0, a1, a2, a3),
        sys::NT_QUERY_VIRTUAL_MEMORY => query_vm(&proc, a0, a1, a2, a3),
        sys::NT_CREATE_FILE => crate::nt_file::create_file(&proc, a0, a1, a2, a3),
        sys::NT_QUERY_INFORMATION_FILE => with_file(&proc, a0, |f| {
            crate::nt_file::query_information(f, a1, a2, a3 as u32 as usize, stack(4).unwrap_or(0) as u32)
        }),
        sys::NT_SET_INFORMATION_FILE => with_file(&proc, a0, |f| {
            crate::nt_file::set_information(f, a1, a2, a3 as u32 as usize, stack(4).unwrap_or(0) as u32)
        }),
        sys::NT_QUERY_DIRECTORY_FILE => with_file(&proc, a0, crate::nt_file::query_directory),
        sys::NT_QUERY_ATTRIBUTES_FILE => crate::nt_file::query_attributes(a0, a1),
        sys::NT_DELETE_FILE => crate::nt_file::delete(a0),
        sys::NT_FLUSH_BUFFERS_FILE => with_file(&proc, a0, |f| crate::nt_file::flush(f, a1)),
        sys::NT_CREATE_EVENT => create_event(&proc, a0, a1, a3, stack(4).unwrap_or(0)),
        sys::NT_SET_EVENT => set_event(&proc, a0, a1, true),
        sys::NT_RESET_EVENT => set_event(&proc, a0, a1, false),
        sys::NT_WAIT_FOR_SINGLE_OBJECT => crate::nt_process::wait_single(&proc, a0, a2),
        sys::NT_WAIT_FOR_MULTIPLE_OBJECTS => crate::nt_process::wait_multiple(&proc, a0, a1, a2),
        sys::NT_TERMINATE_THREAD => crate::nt_process::terminate_thread(&proc, a0, a1),
        sys::NT_QUERY_INFORMATION_PROCESS => crate::nt_process::query_process(&proc, a0, a1, a2, a3),
        sys::NT_QUERY_INFORMATION_THREAD => crate::nt_process::query_thread(&proc, a0, a1, a2, a3),
        sys::NT_RESUME_THREAD => crate::nt_process::resume_thread(&proc, a0, a1),
        sys::NT_DUPLICATE_OBJECT => crate::nt_process::duplicate(&proc, a0, a1, a2, a3),
        sys::VEIL_CREATE_THREAD => crate::nt_process::create_thread(&proc, a0, a1, a2, a3),
        sys::VEIL_CREATE_PROCESS => crate::nt_process::create_process(&proc, a0, a1),
        sys::VEIL_CREATE_PIPE => crate::nt_process::create_pipe(&proc, a0, a1, a2),
        sys::NT_QUERY_VOLUME_INFORMATION_FILE => crate::nt_process::query_volume(&proc, a0, a1, a2, a3),
        sys::NT_SET_INFORMATION_OBJECT => {
            // ObjectHandleFlagInformation { Inherit: u8, ProtectFromClose: u8 }
            if a1 as u32 != 4 || (a3 as u32) < 2 || !ptr_ok(a2, 2) {
                status::INVALID_PARAMETER
            } else {
                let inherit = unsafe { *(a2 as *const u8) } != 0;
                match u32::try_from(a0) {
                    Ok(h) if proc.handles.lock().set_inheritable(h, inherit) => status::SUCCESS,
                    _ => status::INVALID_HANDLE,
                }
            }
        }
        sys::NT_CREATE_SEMAPHORE => {
            let max = stack(4).unwrap_or(1) as u32 as i32;
            let initial = a3 as u32 as i32;
            if !ptr_ok(a0, 8) {
                status::ACCESS_VIOLATION
            } else if max <= 0 || initial < 0 || initial > max {
                status::INVALID_PARAMETER
            } else {
                let sem = Arc::new(crate::nt_objects::SemaphoreObject::new(None, initial, max));
                let h = proc.handles.lock().allocate_semaphore(sem, a1 as u32);
                unsafe { *(a0 as *mut u64) = h as u64 };
                status::SUCCESS
            }
        }
        sys::NT_RELEASE_SEMAPHORE => match object(&proc, a0) {
            Some(KernelObjectRef::Semaphore(sem)) => match sem.release(a1 as u32 as i32) {
                Ok(prev) => {
                    if ptr_ok(a2, 4) {
                        unsafe { *(a2 as *mut i32) = prev };
                    }
                    status::SUCCESS
                }
                // STATUS_SEMAPHORE_LIMIT_EXCEEDED
                Err(_) => 0xC000_0047u32 as i32,
            },
            _ => status::INVALID_HANDLE,
        },
        sys::NT_QUERY_OBJECT => {
            if a1 as u32 != 4 || (a3 as u32) < 2 || !ptr_ok(a2, 2) {
                status::INVALID_PARAMETER
            } else {
                match u32::try_from(a0).ok().and_then(|h| proc.handles.lock().inheritable(h)) {
                    Some(inherit) => {
                        unsafe {
                            *(a2 as *mut u8) = inherit as u8;
                            *((a2 + 1) as *mut u8) = 0;
                        }
                        status::SUCCESS
                    }
                    None => status::INVALID_HANDLE,
                }
            }
        }
        sys::VEIL_DEBUG_PRINT => {
            let n = (a1 as usize).min(1024);
            if ptr_ok(a0, n) {
                let text = unsafe { core::slice::from_raw_parts(a0 as *const u8, n) };
                crate::serial_println!("[DEBUG {}] {}", proc.pid, alloc::string::String::from_utf8_lossy(text).trim_end());
            }
            status::SUCCESS
        }
        sys::NT_YIELD_EXECUTION => {
            if crate::kthread::others_ready() {
                crate::kthread::yield_now();
            }
            status::SUCCESS
        }
        sys::VEIL_USER_CREATE_WINDOW => crate::nt_user::create_window(proc.pid as u32, a0, a1),
        sys::VEIL_USER_DESTROY_WINDOW => crate::nt_user::destroy_window(a0),
        sys::VEIL_USER_SHOW_WINDOW => crate::nt_user::show_window(a0, a1),
        sys::VEIL_USER_SET_TEXT => crate::nt_user::set_text(a0, a1),
        sys::VEIL_USER_SET_POS => crate::nt_user::set_pos(a0, a1, a2),
        sys::VEIL_USER_GET_POS => crate::nt_user::get_pos(a0, a1),
        sys::VEIL_USER_PRESENT => crate::nt_user::present(a0, a1, a2, a3),
        sys::VEIL_USER_POST => crate::nt_user::post(a0, a1, a2, a3),
        sys::VEIL_USER_GET_MESSAGE => crate::nt_user::get_message(a0, a1, a2, a3),
        sys::VEIL_USER_TIMER => crate::nt_user::timer(a0, a1, a2),
        sys::VEIL_USER_QUERY => crate::nt_user::query(a0, a1),
        sys::VEIL_CLIP_SET => crate::nt_user::clip_set(proc.pid as u32, a0, a1),
        sys::VEIL_CLIP_GET => crate::nt_user::clip_get(proc.pid as u32, a0, a1, a2),
        sys::VEIL_REG => crate::nt_registry::call(proc.pid as u32, a0),
        sys::VEIL_LOAD_DLL => load_dll(&proc, a0, a1, a2, a3),
        sys::VEIL_GET_PROC_ADDRESS => get_proc_address(&proc, a0, a1, a2),
        _ => status::NOT_IMPLEMENTED,
    };
    ret(s)
}

/// NTSTATUS in rax: the 32-bit value, zero-extended.
fn ret(s: NtStatus) -> u64 {
    s as u32 as u64
}

/// The kernel's u64 status codes (nt_memory) as NTSTATUS.
fn st(e: u64) -> NtStatus {
    e as u32 as NtStatus
}

/// NtTerminateProcess(ProcessHandle, ExitStatus): this process (all its
/// threads stop) or another one it has a handle to.
fn terminate_process(proc: &Arc<Process>, handle: u64, exit_status: NtStatus) -> NtStatus {
    if handle != CURRENT_PROCESS {
        return match object(proc, handle) {
            Some(KernelObjectRef::Process(p)) if p.pid != proc.pid => {
                crate::serial_println!("[NT] pid {} ends pid {} with code {}", proc.pid, p.pid, exit_status);
                crate::nt_process::kill_process(&p, exit_status);
                status::SUCCESS
            }
            Some(KernelObjectRef::Process(_)) => terminate_process(proc, CURRENT_PROCESS, exit_status),
            Some(_) => status::OBJECT_TYPE_MISMATCH,
            None => status::INVALID_HANDLE,
        };
    }
    crate::serial_println!("[NT] pid {} exits with code {}", proc.pid, exit_status);
    crate::nt_process::kill_process(proc, exit_status);
    unsafe { crate::loader::return_from_usermode() }
}

/// The object a handle refers to.
fn object(proc: &Process, handle: u64) -> Option<KernelObjectRef> {
    let h = u32::try_from(handle).ok()?;
    proc.handles.lock().get_ref(h).cloned()
}

fn with_file(proc: &Process, handle: u64, f: impl FnOnce(&crate::nt_file::FileObject) -> NtStatus) -> NtStatus {
    match object(proc, handle) {
        Some(KernelObjectRef::File(file)) => f(&file),
        Some(_) => status::OBJECT_TYPE_MISMATCH,
        None => status::INVALID_HANDLE,
    }
}

/// NtReadFile / NtWriteFile(FileHandle, Event, ApcRoutine, ApcContext,
/// IoStatusBlock, Buffer, Length, ByteOffset, Key). Synchronous.
fn io(proc: &Process, handle: u64, writing: bool) -> NtStatus {
    // Stack arguments first: the per-CPU user RSP they're read through is
    // another thread's once this one waits.
    let (Some(iosb), Some(buffer), Some(length)) = (stack(4), stack(5), stack(6)) else {
        return status::ACCESS_VIOLATION;
    };
    let offset = stack(7).unwrap_or(0);
    let length = length as u32 as usize;
    if !ptr_ok(buffer, length) || !ptr_ok(iosb, core::mem::size_of::<IoStatusBlock>()) {
        return status::ACCESS_VIOLATION;
    }
    match object(proc, handle) {
        Some(KernelObjectRef::File(f)) if writing => crate::nt_file::write(&f, iosb, buffer, length, offset),
        Some(KernelObjectRef::File(f)) => crate::nt_file::read(&f, iosb, buffer, length, offset),
        Some(KernelObjectRef::Pipe(end)) => {
            let (s, n) = crate::nt_process::pipe_io(&end, buffer, length, writing);
            unsafe { *(iosb as *mut IoStatusBlock) = IoStatusBlock { status: s as u32 as u64, information: n as u64 } };
            s
        }
        Some(KernelObjectRef::Console(console)) if writing => {
            let mut data = Vec::with_capacity(length);
            data.extend_from_slice(unsafe { core::slice::from_raw_parts(buffer as *const u8, length) });
            console.write(&data);
            unsafe { *(iosb as *mut IoStatusBlock) = IoStatusBlock { status: 0, information: length as u64 } };
            status::SUCCESS
        }
        Some(KernelObjectRef::Console(console)) => {
            if length == 0 {
                return status::SUCCESS;
            }
            // Wait for a whole typed line.
            let data = loop {
                if let Some(line) = console.read_line(length) {
                    break line;
                }
                if !console.is_live() {
                    return status::END_OF_FILE;
                }
                crate::loader::kill_if_requested();
                wait_a_little();
            };
            unsafe {
                core::ptr::copy_nonoverlapping(data.as_ptr(), buffer as *mut u8, data.len());
                *(iosb as *mut IoStatusBlock) = IoStatusBlock { status: 0, information: data.len() as u64 };
            }
            status::SUCCESS
        }
        Some(_) => status::OBJECT_TYPE_MISMATCH,
        None => status::INVALID_HANDLE,
    }
}

/// A timeout argument (LARGE_INTEGER*, 100 ns units: negative = relative,
/// positive = absolute system time) as a TICKS deadline; None = forever.
pub fn deadline(timeout: u64) -> Option<u64> {
    if timeout == 0 || !ptr_ok(timeout, 8) {
        return None;
    }
    let t = unsafe { core::ptr::read_unaligned(timeout as *const i64) };
    let now = crate::interrupts::TICKS.load(core::sync::atomic::Ordering::Relaxed);
    Some(if t < 0 { now + (t.unsigned_abs() / 10_000) } else { now }) // absolute times: not used yet
}

/// NtDelayExecution(Alertable, DelayInterval*).
fn delay_execution(interval: u64) -> NtStatus {
    let Some(until) = deadline(interval) else { return status::ACCESS_VIOLATION };
    if until <= crate::interrupts::TICKS.load(core::sync::atomic::Ordering::Relaxed) {
        // Sleep(0): give up the rest of the slice.
        if crate::kthread::others_ready() {
            crate::kthread::yield_now();
        }
        return status::SUCCESS;
    }
    while crate::interrupts::TICKS.load(core::sync::atomic::Ordering::Relaxed) < until {
        crate::loader::kill_if_requested();
        wait_a_little();
    }
    status::SUCCESS
}

/// Inside a syscall (interrupts off, no locks held): let other threads run,
/// or sleep until the next interrupt if there are none.
pub fn wait_a_little() {
    if crate::kthread::others_ready() {
        crate::kthread::yield_now();
    } else {
        // A timer interrupt in ring 0 never switches threads, so this only
        // pauses until it (or a keystroke) arrives.
        x86_64::instructions::interrupts::enable_and_hlt();
        x86_64::instructions::interrupts::disable();
    }
}

/// NtClose(Handle). Closing a file's last handle writes it back.
fn close(proc: &Process, handle: u64) -> NtStatus {
    match u32::try_from(handle) {
        Ok(h) if proc.handles.lock().close(h) => status::SUCCESS,
        _ => status::INVALID_HANDLE,
    }
}

// -----| virtual memory |-----

fn read_u64(p: u64) -> Option<u64> {
    ptr_ok(p, 8).then(|| unsafe { core::ptr::read_unaligned(p as *const u64) })
}

fn write_u64(p: u64, v: u64) {
    if ptr_ok(p, 8) {
        unsafe { core::ptr::write_unaligned(p as *mut u64, v) };
    }
}

/// NtAllocateVirtualMemory(ProcessHandle, BaseAddress*, ZeroBits,
/// RegionSize*, AllocationType, Protect).
fn allocate_vm(proc: &Process, h: u64, base_ptr: u64, size_ptr: u64) -> NtStatus {
    if h != CURRENT_PROCESS {
        return status::INVALID_HANDLE;
    }
    let (Some(alloc_type), Some(protect)) = (stack(4), stack(5)) else { return status::ACCESS_VIOLATION };
    let (Some(base), Some(size)) = (read_u64(base_ptr), read_u64(size_ptr)) else { return status::ACCESS_VIOLATION };
    match proc.vad.lock().virtual_alloc(base, size as usize, alloc_type as u32, protect as u32) {
        Ok(addr) => {
            let end = (base.max(addr) + size + 0xFFF) & !0xFFF;
            write_u64(base_ptr, addr);
            write_u64(size_ptr, end - addr);
            status::SUCCESS
        }
        Err(e) => st(e),
    }
}

/// NtFreeVirtualMemory(ProcessHandle, BaseAddress*, RegionSize*, FreeType).
fn free_vm(proc: &Process, h: u64, base_ptr: u64, size_ptr: u64, free_type: u64) -> NtStatus {
    if h != CURRENT_PROCESS {
        return status::INVALID_HANDLE;
    }
    let (Some(base), Some(size)) = (read_u64(base_ptr), read_u64(size_ptr)) else { return status::ACCESS_VIOLATION };
    match proc.vad.lock().virtual_free(base, size as usize, free_type as u32) {
        Ok(()) => status::SUCCESS,
        Err(e) => st(e),
    }
}

/// NtProtectVirtualMemory(ProcessHandle, BaseAddress*, RegionSize*,
/// NewProtect, OldProtect*).
fn protect_vm(proc: &Process, h: u64, base_ptr: u64, size_ptr: u64, new: u64) -> NtStatus {
    if h != CURRENT_PROCESS {
        return status::INVALID_HANDLE;
    }
    let old_ptr = stack(4).unwrap_or(0);
    let (Some(base), Some(size)) = (read_u64(base_ptr), read_u64(size_ptr)) else { return status::ACCESS_VIOLATION };
    match proc.vad.lock().virtual_protect(base, size as usize, new as u32) {
        Ok(old) => {
            if ptr_ok(old_ptr, 4) {
                unsafe { *(old_ptr as *mut u32) = old };
            }
            status::SUCCESS
        }
        Err(e) => st(e),
    }
}

/// NtQueryVirtualMemory(ProcessHandle, BaseAddress, MemoryInformationClass,
/// MemoryInformation*, Length, ReturnLength*): MemoryBasicInformation only.
fn query_vm(proc: &Process, h: u64, addr: u64, class: u64, out: u64) -> NtStatus {
    if h != CURRENT_PROCESS {
        return status::INVALID_HANDLE;
    }
    let (length, ret_len) = (stack(4).unwrap_or(0) as usize, stack(5).unwrap_or(0));
    if class != 0 {
        return status::INVALID_INFO_CLASS;
    }
    if length < 48 {
        return status::BUFFER_TOO_SMALL;
    }
    if !ptr_ok(out, 48) {
        return status::ACCESS_VIOLATION;
    }
    let m = proc.vad.lock().virtual_query(addr);
    unsafe {
        let p = out as *mut u8;
        core::ptr::write_bytes(p, 0, 48);
        core::ptr::write_unaligned(p as *mut u64, m.base_address);
        core::ptr::write_unaligned(p.add(8) as *mut u64, m.allocation_base);
        core::ptr::write_unaligned(p.add(16) as *mut u32, m.allocation_protect);
        core::ptr::write_unaligned(p.add(24) as *mut u64, m.region_size as u64);
        core::ptr::write_unaligned(p.add(32) as *mut u32, m.state);
        core::ptr::write_unaligned(p.add(36) as *mut u32, m.protect);
        core::ptr::write_unaligned(p.add(40) as *mut u32, m.region_type);
    }
    write_u64(ret_len, 48);
    status::SUCCESS
}

// -----| events and waits |-----

/// NtCreateEvent(EventHandle*, DesiredAccess, ObjectAttributes*, EventType
/// (0 = manual reset, 1 = auto reset), InitialState). Names are ignored.
fn create_event(proc: &Process, out: u64, access: u64, kind: u64, initial: u64) -> NtStatus {
    if !ptr_ok(out, 8) {
        return status::ACCESS_VIOLATION;
    }
    let ev = Arc::new(crate::nt_objects::EventObject::new(None, kind as u32 == 1, initial as u8 != 0));
    let h = proc.handles.lock().allocate_event(ev, access as u32);
    unsafe { *(out as *mut u64) = h as u64 };
    status::SUCCESS
}

/// NtSetEvent / NtResetEvent(EventHandle, PreviousState*).
fn set_event(proc: &Process, h: u64, prev: u64, set: bool) -> NtStatus {
    let Some(KernelObjectRef::Event(ev)) = object(proc, h) else { return status::INVALID_HANDLE };
    let was = ev.signaled.load(core::sync::atomic::Ordering::Acquire);
    if set { ev.set() } else { ev.reset() }
    if ptr_ok(prev, 4) {
        unsafe { *(prev as *mut i32) = was as i32 };
    }
    status::SUCCESS
}

// -----| run-time loading |-----

/// VEIL_LOAD_DLL(name*, name_chars, base_out*, inits*, inits_capacity, count_out*).
fn load_dll(proc: &Process, name: u64, chars: u64, base_out: u64, inits: u64) -> NtStatus {
    let (Some(capacity), Some(count_out)) = (stack(4), stack(5)) else { return status::ACCESS_VIOLATION };
    let chars = chars as usize;
    if chars == 0 || chars > 520 || !ptr_ok(name, chars * 2) || !ptr_ok(base_out, 8) || !ptr_ok(count_out, 4) {
        return status::ACCESS_VIOLATION;
    }
    let units = unsafe { core::slice::from_raw_parts(name as *const u16, chars) };
    let name: alloc::string::String = char::decode_utf16(units.iter().copied()).map(|c| c.unwrap_or('?')).collect();
    let mut linker = proc.linker.lock();
    let Some(linker) = linker.as_mut() else { return status::DLL_NOT_FOUND };
    let mut mapper = unsafe { crate::memory::get_mapper() };
    let mut fa = crate::memory::GlobalFrameAllocator;
    let mut pages = proc.mapped_pages.lock();
    let mut mem = crate::modules::Mem { mapper: &mut mapper, fa: &mut fa, pages: &mut pages };
    let (base, calls) = match linker.load_at_runtime(&mut mem, &name) {
        Ok(r) => r,
        Err(s) => return s,
    };
    let n = calls.len().min(capacity as usize);
    if n > 0 && !ptr_ok(inits, n * core::mem::size_of::<nt_abi::InitCall>()) {
        return status::ACCESS_VIOLATION;
    }
    for (i, (routine, module)) in calls.iter().take(n).enumerate() {
        unsafe { *((inits as *mut nt_abi::InitCall).add(i)) = nt_abi::InitCall { routine: *routine, module: *module } };
    }
    unsafe {
        *(base_out as *mut u64) = base;
        *(count_out as *mut u32) = n as u32;
    }
    // VirtualAlloc must not hand out what was just mapped.
    let mut vad = proc.vad.lock();
    for (b, s) in linker.regions() {
        if vad.is_free(b, s) {
            vad.record_image(b, s);
        }
    }
    status::SUCCESS
}

/// VEIL_GET_PROC_ADDRESS(module_base, name* or ordinal, address_out*).
fn get_proc_address(proc: &Process, base: u64, name: u64, out: u64) -> NtStatus {
    if !ptr_ok(out, 8) {
        return status::ACCESS_VIOLATION;
    }
    let symbol = if name < 0x10000 {
        None
    } else {
        match crate::memory::user_cstr_len(name, 256) {
            Some(n) => Some(alloc::string::String::from_utf8_lossy(unsafe { core::slice::from_raw_parts(name as *const u8, n) }).into_owned()),
            None => return status::ACCESS_VIOLATION,
        }
    };
    let mut linker = proc.linker.lock();
    let Some(linker) = linker.as_mut() else { return status::PROCEDURE_NOT_FOUND };
    let mut mapper = unsafe { crate::memory::get_mapper() };
    let mut fa = crate::memory::GlobalFrameAllocator;
    let mut pages = proc.mapped_pages.lock();
    let mut mem = crate::modules::Mem { mapper: &mut mapper, fa: &mut fa, pages: &mut pages };
    match linker.proc_address(&mut mem, base, symbol.as_deref(), name as u16) {
        Some(addr) => {
            unsafe { *(out as *mut u64) = addr };
            status::SUCCESS
        }
        None => status::PROCEDURE_NOT_FOUND,
    }
}
