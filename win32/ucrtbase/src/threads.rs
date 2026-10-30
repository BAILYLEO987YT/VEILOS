//! Threads in the C runtime (W6): each thread's own `errno`, `strtok`
//! position, `rand` seed and C++ exception state, kept in a TLS slot;
//! `_beginthread(ex)`; and running other programs (`system`, `_popen`,
//! `_pipe`) on CreateProcess and pipes.

use crate::heap;
use crate::stdio;
use core::sync::atomic::{AtomicU32, Ordering};

#[link(name = "kernel32", kind = "raw-dylib")]
unsafe extern "system" {
    fn TlsAlloc() -> u32;
    fn TlsGetValue(index: u32) -> *mut u8;
    fn TlsSetValue(index: u32, value: *mut u8) -> i32;
    fn GetCurrentThreadId() -> u32;
    fn CreateThread(sa: usize, stack: usize, start: usize, param: usize, flags: u32, tid: *mut u32) -> u64;
    fn ExitThread(code: u32) -> !;
    fn CloseHandle(h: u64) -> i32;
    fn CreateProcessW(app: *const u16, cmd: *mut u16, psa: usize, tsa: usize, inherit: i32, flags: u32, env: usize, dir: *const u16, si: *const u8, pi: *mut u8) -> i32;
    fn WaitForSingleObject(h: u64, ms: u32) -> u32;
    fn GetExitCodeProcess(h: u64, code: *mut u32) -> i32;
    fn CreatePipe(read: *mut u64, write: *mut u64, sa: usize, size: u32) -> i32;
    fn GetEnvironmentVariableW(name: *const u16, buf: *mut u16, size: u32) -> u32;
}

/// A thread's own C runtime state.
pub struct PerThread {
    pub errno: i32,
    pub doserrno: u32,
    pub strtok: *mut u8,
    pub wcstok: *mut u16,
    pub rand: u32,
    pub eh: crate::eh::EhState,
}

static mut SLOT: u32 = u32::MAX;

pub fn init() {
    unsafe { SLOT = TlsAlloc() };
}

/// The calling thread's state, made on first use.
pub fn this() -> &'static mut PerThread {
    unsafe {
        let p = TlsGetValue(SLOT) as *mut PerThread;
        if !p.is_null() {
            return &mut *p;
        }
        let p = heap::malloc(size_of::<PerThread>()) as *mut PerThread;
        if p.is_null() {
            crate::startup::fatal("out of memory for thread data", 0xC000_0017);
        }
        p.write(PerThread {
            errno: 0,
            doserrno: 0,
            strtok: core::ptr::null_mut(),
            wcstok: core::ptr::null_mut(),
            rand: 1,
            eh: crate::eh::EhState::new(),
        });
        TlsSetValue(SLOT, p as *mut u8);
        &mut *p
    }
}

/// DLL_THREAD_DETACH: the thread's state goes.
pub fn detach() {
    unsafe {
        let p = TlsGetValue(SLOT);
        if !p.is_null() {
            TlsSetValue(SLOT, core::ptr::null_mut());
            heap::free(p);
        }
    }
}

// -----| the stdio lock |-----
//
// One lock, recursive, for every stream: a printf's output isn't
// interleaved with another thread's.

static OWNER: AtomicU32 = AtomicU32::new(0);
static mut DEPTH: u32 = 0;

pub fn lock() {
    let me = unsafe { GetCurrentThreadId() };
    if OWNER.load(Ordering::Acquire) == me {
        unsafe { DEPTH += 1 };
        return;
    }
    let mut spins = 0u32;
    while OWNER.compare_exchange_weak(0, me, Ordering::Acquire, Ordering::Relaxed).is_err() {
        spins += 1;
        if spins > 64 {
            unsafe { crate::Sleep(0) };
        } else {
            core::hint::spin_loop();
        }
    }
    unsafe { DEPTH = 1 };
}

pub fn unlock() {
    unsafe {
        DEPTH -= 1;
        if DEPTH == 0 {
            OWNER.store(0, Ordering::Release);
        }
    }
}

/// Holds the stdio lock while it lives.
pub struct Locked;

impl Locked {
    pub fn new() -> Self {
        lock();
        Locked
    }
}

impl Drop for Locked {
    fn drop(&mut self) {
        unlock();
    }
}

// -----| _beginthread |-----

#[no_mangle]
pub unsafe extern "C" fn _beginthreadex(sa: usize, stack: u32, start: usize, arg: usize, flags: u32, tid: *mut u32) -> u64 {
    let h = unsafe { CreateThread(sa, stack as usize, start, arg, flags, tid) };
    if h == 0 {
        crate::errno_from_win32();
    }
    h
}

#[no_mangle]
pub extern "C" fn _endthreadex(code: u32) -> ! {
    unsafe { ExitThread(code) }
}

/// `_beginthread`'s routine returns nothing: run it, then end the thread.
struct Plain {
    start: unsafe extern "C" fn(usize),
    arg: usize,
}

unsafe extern "system" fn plain_start(p: *mut Plain) -> u32 {
    let Plain { start, arg } = unsafe { p.read() };
    heap::free(p as *mut u8);
    unsafe { start(arg) };
    0
}

#[no_mangle]
pub unsafe extern "C" fn _beginthread(start: unsafe extern "C" fn(usize), stack: u32, arg: usize) -> u64 {
    let p = heap::malloc(size_of::<Plain>()) as *mut Plain;
    if p.is_null() {
        crate::set_errno(crate::errno::ENOMEM);
        return u64::MAX;
    }
    unsafe { p.write(Plain { start, arg }) };
    let h = unsafe { CreateThread(0, stack as usize, plain_start as *const () as usize, p as usize, 0, core::ptr::null_mut()) };
    if h == 0 {
        heap::free(p as *mut u8);
        crate::errno_from_win32();
        return u64::MAX;
    }
    h
}

#[no_mangle]
pub extern "C" fn _endthread() -> ! {
    unsafe { ExitThread(0) }
}

// -----| other programs |-----

/// The command processor: %ComSpec%, else C:\cmd.exe.
fn comspec(out: &mut [u16; 260]) -> usize {
    let name: [u16; 8] = [b'C' as u16, b'o' as u16, b'm' as u16, b'S' as u16, b'p' as u16, b'e' as u16, b'c' as u16, 0];
    let n = unsafe { GetEnvironmentVariableW(name.as_ptr(), out.as_mut_ptr(), out.len() as u32) } as usize;
    if n > 0 && n < out.len() {
        return n;
    }
    let fallback = "C:\\cmd.exe";
    for (i, u) in fallback.encode_utf16().enumerate() {
        out[i] = u;
    }
    out[fallback.len()] = 0;
    fallback.len()
}

/// Start `comspec /c command` with the given standard handles (0: ours).
/// Returns the process handle.
fn run_command(command: &[u16], std: [u64; 3]) -> Option<u64> {
    let mut shell = [0u16; 260];
    let n = comspec(&mut shell);
    let mut line: alloc::vec::Vec<u16> = alloc::vec::Vec::new();
    line.push(b'"' as u16);
    line.extend_from_slice(&shell[..n]);
    line.extend("\" /c ".encode_utf16());
    line.extend_from_slice(command);
    line.push(0);
    let mut si = [0u8; 104];
    si[0] = 104;
    if std != [0; 3] {
        si[60] = 0x00;
        si[61] = 0x01; // STARTF_USESTDHANDLES
        for (i, h) in std.iter().enumerate() {
            si[80 + 8 * i..88 + 8 * i].copy_from_slice(&h.to_le_bytes());
        }
    }
    let mut pi = [0u8; 24];
    let ok = unsafe { CreateProcessW(shell.as_ptr(), line.as_mut_ptr(), 0, 0, 1, 0, 0, core::ptr::null(), si.as_ptr(), pi.as_mut_ptr()) };
    if ok == 0 {
        crate::errno_from_win32();
        return None;
    }
    unsafe { CloseHandle(u64::from_le_bytes(pi[8..16].try_into().unwrap())) };
    Some(u64::from_le_bytes(pi[0..8].try_into().unwrap()))
}

fn wait_for(process: u64) -> i32 {
    let mut code = 0u32;
    unsafe {
        WaitForSingleObject(process, u32::MAX);
        GetExitCodeProcess(process, &mut code);
        CloseHandle(process);
    }
    code as i32
}

#[no_mangle]
pub unsafe extern "C" fn _wsystem(command: *const u16) -> i32 {
    if command.is_null() {
        let mut shell = [0u16; 260];
        comspec(&mut shell);
        return 1;
    }
    stdio::flush_all();
    match run_command(unsafe { crate::string::slice(command) }, [0; 3]) {
        Some(p) => wait_for(p),
        None => -1,
    }
}

#[no_mangle]
pub unsafe extern "C" fn system(command: *const u8) -> i32 {
    if command.is_null() {
        return unsafe { _wsystem(core::ptr::null()) };
    }
    let w = crate::convert::to_wide(unsafe { crate::string::slice(command) });
    let r = unsafe { _wsystem(w) };
    heap::free(w as *mut u8);
    r
}

#[no_mangle]
pub unsafe extern "C" fn _pipe(fds: *mut i32, size: u32, mode: i32) -> i32 {
    let (mut r, mut w) = (0u64, 0u64);
    if unsafe { CreatePipe(&mut r, &mut w, 0, size) } == 0 {
        crate::errno_from_win32();
        return -1;
    }
    let mode = if mode & (stdio::O_TEXT | stdio::O_BINARY) == 0 { 0 } else { mode };
    let fr = stdio::_open_osfhandle(r as isize, mode);
    let fw = stdio::_open_osfhandle(w as isize, mode | stdio::O_WRONLY);
    unsafe {
        *fds = fr;
        *fds.add(1) = fw;
    }
    0
}

/// `_popen` streams and the programs behind them, for `_pclose`.
static mut POPEN: [(usize, u64); 16] = [(0, 0); 16];

unsafe fn popen(command: &[u16], mode: &[u8]) -> *mut stdio::File {
    let reading = mode.first() == Some(&b'r');
    let (mut r, mut w) = (0u64, 0u64);
    if unsafe { CreatePipe(&mut r, &mut w, 0, 0) } == 0 {
        crate::errno_from_win32();
        return core::ptr::null_mut();
    }
    stdio::flush_all();
    let out = unsafe { [crate::GetStdHandle(0xFFFF_FFF6), crate::GetStdHandle(0xFFFF_FFF5), crate::GetStdHandle(0xFFFF_FFF4)] };
    let std = if reading { [out[0], w, out[2]] } else { [r, out[1], out[2]] };
    let Some(process) = run_command(command, std) else {
        unsafe {
            CloseHandle(r);
            CloseHandle(w);
        }
        return core::ptr::null_mut();
    };
    // Our end becomes the stream; the child's end closes here.
    let (ours, theirs) = if reading { (r, w) } else { (w, r) };
    unsafe { CloseHandle(theirs) };
    let binary = mode.contains(&b'b');
    let fd = stdio::_open_osfhandle(ours as isize, if binary { stdio::O_BINARY } else { stdio::O_TEXT } | if reading { 0 } else { stdio::O_WRONLY });
    let m: &[u8] = if reading { b"r\0" } else { b"w\0" };
    let f = unsafe { stdio::_fdopen(fd, m.as_ptr()) };
    unsafe {
        for slot in (*(&raw mut POPEN)).iter_mut() {
            if slot.0 == 0 {
                *slot = (f as usize, process);
                break;
            }
        }
    }
    f
}

#[no_mangle]
pub unsafe extern "C" fn _wpopen(command: *const u16, mode: *const u16) -> *mut stdio::File {
    let m: alloc::vec::Vec<u8> = unsafe { crate::string::slice(mode) }.iter().map(|&c| c as u8).collect();
    unsafe { popen(crate::string::slice(command), &m) }
}

#[no_mangle]
pub unsafe extern "C" fn _popen(command: *const u8, mode: *const u8) -> *mut stdio::File {
    let w = crate::convert::to_wide(unsafe { crate::string::slice(command) });
    let f = unsafe { popen(crate::string::slice(w), crate::string::slice(mode)) };
    heap::free(w as *mut u8);
    f
}

#[no_mangle]
pub unsafe extern "C" fn _pclose(f: *mut stdio::File) -> i32 {
    let process = unsafe {
        (*(&raw mut POPEN)).iter_mut().find(|s| s.0 == f as usize).map(|s| {
            let p = s.1;
            *s = (0, 0);
            p
        })
    };
    unsafe { stdio::fclose(f) };
    match process {
        Some(p) => wait_for(p),
        None => -1,
    }
}
