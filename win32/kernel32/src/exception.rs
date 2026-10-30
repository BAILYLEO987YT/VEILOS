//! Exceptions (W5b): raising them and the process-wide filters. The
//! machinery itself is ntdll's; `RtlCaptureContext` and the other `Rtl*`
//! names kernel32 exports are forwarders to it (build.rs).

#[link(name = "ntdll", kind = "raw-dylib")]
unsafe extern "system" {
    fn RtlRaiseException(rec: *mut ExceptionRecord);
    fn RtlSetUnhandledExceptionFilter(f: usize) -> usize;
    fn RtlAddVectoredExceptionHandler(first: u32, handler: usize) -> usize;
    fn RtlRemoveVectoredExceptionHandler(handle: usize) -> u32;
}

#[repr(C)]
pub struct ExceptionRecord {
    code: u32,
    flags: u32,
    record: usize,
    address: usize,
    count: u32,
    info: [u64; 15],
}

const EXCEPTION_MAXIMUM_PARAMETERS: u32 = 15;

#[no_mangle]
pub unsafe extern "system" fn RaiseException(code: u32, flags: u32, count: u32, args: *const u64) {
    let mut rec = ExceptionRecord {
        code,
        flags: flags & 1, // only EXCEPTION_NONCONTINUABLE is the caller's
        record: 0,
        address: 0, // RtlRaiseException fills it in
        count: 0,
        info: [0; 15],
    };
    if !args.is_null() {
        let n = count.min(EXCEPTION_MAXIMUM_PARAMETERS);
        rec.count = n;
        for i in 0..n as usize {
            rec.info[i] = unsafe { *args.add(i) };
        }
    }
    unsafe { RtlRaiseException(&mut rec) };
}

#[no_mangle]
pub unsafe extern "system" fn SetUnhandledExceptionFilter(filter: usize) -> usize {
    unsafe { RtlSetUnhandledExceptionFilter(filter) }
}

/// The default top-level filter: let the process end (EXCEPTION_EXECUTE_HANDLER).
#[no_mangle]
pub extern "system" fn UnhandledExceptionFilter(_ptrs: usize) -> i32 {
    1
}

#[no_mangle]
pub unsafe extern "system" fn AddVectoredExceptionHandler(first: u32, handler: usize) -> usize {
    unsafe { RtlAddVectoredExceptionHandler(first, handler) }
}

#[no_mangle]
pub unsafe extern "system" fn RemoveVectoredExceptionHandler(handle: usize) -> u32 {
    unsafe { RtlRemoveVectoredExceptionHandler(handle) }
}

/// Continue handlers run after vectored handlers allow a continue: none are
/// called yet, but registering one succeeds.
#[no_mangle]
pub extern "system" fn AddVectoredContinueHandler(_first: u32, handler: usize) -> usize {
    handler
}

#[no_mangle]
pub extern "system" fn RemoveVectoredContinueHandler(_handle: usize) -> u32 {
    1
}

#[no_mangle]
pub extern "system" fn IsDebuggerPresent() -> i32 {
    0
}

#[no_mangle]
pub extern "system" fn SetErrorMode(mode: u32) -> u32 {
    static mut MODE: u32 = 0;
    unsafe { core::mem::replace(&mut *(&raw mut MODE), mode) }
}

#[no_mangle]
pub extern "system" fn GetErrorMode() -> u32 {
    0
}
