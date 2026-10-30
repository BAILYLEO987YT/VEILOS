// =============================================================================
// VEILOS: NT system services for VEILOS's own ntdll.dll (WIN32_DESIGN.md)
// =============================================================================
//
// The numbers and structure layouts are shared with ntdll through the
// `nt_abi` crate. Arguments 1-4 arrive in registers (r10 copied to the first
// argument, rdx, r8, r9); the rest are read from the caller's stack with
// `interrupts::syscall_stack_arg`. Every service returns an NTSTATUS.

use alloc::sync::Arc;
use alloc::vec::Vec;
use nt_abi::{status, sys, IoStatusBlock, NtStatus, CURRENT_PROCESS};

use crate::nt_objects::KernelObjectRef;
use crate::process::Process;

/// True for the syscall numbers this module serves.
pub fn handles(id: u64) -> bool {
    (sys::BASE as u64..sys::END as u64).contains(&id)
}

pub fn dispatch(id: u64, a0: u64, a1: u64, a2: u64, a3: u64) -> u64 {
    let _ = (a2, a3);
    let Some(proc) = crate::process::PROCESSOR.lock().get_active() else {
        return ret(status::INVALID_HANDLE);
    };
    let s = match id as u32 {
        sys::NT_TERMINATE_PROCESS => terminate_process(&proc, a0, a1 as NtStatus),
        sys::NT_WRITE_FILE => write_file(&proc, a0),
        sys::NT_CLOSE => close(&proc, a0),
        _ => status::NOT_IMPLEMENTED,
    };
    ret(s)
}

/// NTSTATUS in rax: the 32-bit value, zero-extended.
fn ret(s: NtStatus) -> u64 {
    s as u32 as u64
}

/// NtTerminateProcess(ProcessHandle, ExitStatus). Only the current process
/// for now (other processes need process handles: W6).
fn terminate_process(proc: &Arc<Process>, handle: u64, exit_status: NtStatus) -> NtStatus {
    if handle != CURRENT_PROCESS {
        return status::INVALID_HANDLE;
    }
    crate::serial_println!("[NT] pid {} exits with code {}", proc.pid, exit_status);
    proc.terminate(exit_status);
    crate::interrupts::USER_PROCESS_ACTIVE.store(false, core::sync::atomic::Ordering::Relaxed);
    unsafe { crate::loader::return_from_usermode() }
}

/// The object a handle refers to.
fn object(proc: &Process, handle: u64) -> Option<KernelObjectRef> {
    let h = u32::try_from(handle).ok()?;
    proc.handles.lock().get_ref(h).cloned()
}

/// NtWriteFile(FileHandle, Event, ApcRoutine, ApcContext, IoStatusBlock,
/// Buffer, Length, ByteOffset, Key). Synchronous; consoles only so far (files
/// are W4).
fn write_file(proc: &Process, handle: u64) -> NtStatus {
    let (Some(iosb), Some(buffer), Some(length)) = (
        crate::interrupts::syscall_stack_arg(4),
        crate::interrupts::syscall_stack_arg(5),
        crate::interrupts::syscall_stack_arg(6),
    ) else {
        return status::ACCESS_VIOLATION;
    };
    let length = length as u32 as usize;
    if !crate::memory::user_ptr_ok(iosb, core::mem::size_of::<IoStatusBlock>()) {
        return status::ACCESS_VIOLATION;
    }
    if length > 0 && !crate::memory::user_ptr_ok(buffer, length) {
        return status::ACCESS_VIOLATION;
    }
    let Some(obj) = object(proc, handle) else { return status::INVALID_HANDLE };
    let KernelObjectRef::Console(console) = obj else { return status::OBJECT_TYPE_MISMATCH };
    let mut data = Vec::with_capacity(length);
    data.extend_from_slice(unsafe { core::slice::from_raw_parts(buffer as *const u8, length) });
    console.write(&data);
    unsafe { *(iosb as *mut IoStatusBlock) = IoStatusBlock { status: 0, information: length as u64 } };
    status::SUCCESS
}

/// NtClose(Handle).
fn close(proc: &Process, handle: u64) -> NtStatus {
    match u32::try_from(handle) {
        Ok(h) if proc.handles.lock().close(h) => status::SUCCESS,
        _ => status::INVALID_HANDLE,
    }
}
