// =============================================================================
// VEILOS: named objects, address waits and named pipes (WIN32_DESIGN.md, S1)
// =============================================================================
//
// Named objects: one table of (name, kind) -> object, shared by every process.
// An entry holds a reference of its own, and goes once it's the only one left.
//
// Address waits (WaitOnAddress): a table of sequence numbers hashed by address.
// A waiter reads the number, compares the memory, and sleeps until the number
// moves; a wake moves it. Colliding addresses just wake early, which the API
// allows callers to see anyway.
//
// Named pipes: an instance is two of nt_process's byte pipes, one each way. Its
// server end exists from creation, and a client takes the oldest instance
// nobody has connected to. Messages aren't kept apart: pipes are byte streams.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use nt_abi::{status, NtStatus};
use spin::Mutex;

use crate::nt_file::ptr_ok;
use crate::nt_objects::KernelObjectRef;
use crate::nt_process::{new_pipe, pipe_available, Pipe, PipeEnd};
use crate::process::Process;

const STATUS_OBJECT_NAME_NOT_FOUND: NtStatus = status::OBJECT_NAME_NOT_FOUND;
const STATUS_OBJECT_TYPE_MISMATCH: NtStatus = 0xC000_0024u32 as NtStatus;
const STATUS_PIPE_NOT_AVAILABLE: NtStatus = 0xC000_00ACu32 as NtStatus;
const STATUS_PIPE_LISTENING: NtStatus = 0xC000_00B3u32 as NtStatus;
const STATUS_PIPE_CONNECTED: NtStatus = 0xC000_00B2u32 as NtStatus;
const STATUS_IO_TIMEOUT: NtStatus = 0xC000_00B5u32 as NtStatus;
const STATUS_TIMEOUT: NtStatus = 0x0000_0102;
const NAME_MAX: usize = 520;

/// A NUL-terminated UTF-16 string from the caller.
fn read_name(p: u64) -> Option<String> {
    let mut units = Vec::new();
    while units.len() < NAME_MAX {
        let a = p + units.len() as u64 * 2;
        if !ptr_ok(a, 2) {
            return None;
        }
        let u = unsafe { core::ptr::read_unaligned(a as *const u16) };
        if u == 0 {
            return Some(char::decode_utf16(units).map(|c| c.unwrap_or('?')).collect());
        }
        units.push(u);
    }
    None
}

// -----| small questions |-----

/// VEIL_SYSINFO(op, out*, size).
pub fn sys_info(op: u64, out: u64, size: u64) -> NtStatus {
    if !ptr_ok(out, size as usize) {
        return status::ACCESS_VIOLATION;
    }
    match op {
        0 => {
            fill_random(unsafe { core::slice::from_raw_parts_mut(out as *mut u8, size as usize) });
            status::SUCCESS
        }
        1 if size >= 16 => {
            let free = crate::memory::free_frame_count() as u64 * 4096;
            let total = crate::memory::total_frame_count() as u64 * 4096;
            unsafe {
                *(out as *mut u64) = total.max(free);
                *(out as *mut u64).add(1) = free;
            }
            status::SUCCESS
        }
        _ => status::INVALID_PARAMETER,
    }
}

/// Random bytes: RDRAND where the CPU has it. Elsewhere (QEMU's default CPU)
/// this is the time stamp counter stirred by a generator, good enough for hash
/// tables and temp names but NOT for keys.
fn fill_random(out: &mut [u8]) {
    static STATE: AtomicU64 = AtomicU64::new(0x9E37_79B9_7F4A_7C15);
    let has_rdrand = unsafe { core::arch::x86_64::__cpuid(1) }.ecx & (1 << 30) != 0;
    for chunk in out.chunks_mut(8) {
        let mut v = 0u64;
        let got = has_rdrand && unsafe { core::arch::x86_64::_rdrand64_step(&mut v) } == 1;
        if !got {
            // SplitMix64, with the clock mixed in on every step.
            let mut z = STATE.fetch_add(0x9E37_79B9_7F4A_7C15 ^ crate::time::rdtsc(), Ordering::Relaxed);
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            v = z ^ (z >> 31);
        }
        chunk.copy_from_slice(&v.to_le_bytes()[..chunk.len()]);
    }
}

// -----| named objects |-----

static NAMED: Mutex<BTreeMap<String, (u32, KernelObjectRef)>> = Mutex::new(BTreeMap::new());

fn still_held(o: &KernelObjectRef) -> bool {
    match o {
        KernelObjectRef::Event(a) => Arc::strong_count(a) > 1,
        KernelObjectRef::Semaphore(a) => Arc::strong_count(a) > 1,
        KernelObjectRef::Mutex(a) => Arc::strong_count(a) > 1,
        _ => false,
    }
}

/// VEIL_NAME_OBJECT(handle, name*, kind): give the object at `handle` a name.
pub fn name_object(proc: &Process, h: u64, name: u64, kind: u64) -> NtStatus {
    let Some(name) = read_name(name) else { return status::ACCESS_VIOLATION };
    let Some(obj) = crate::nt_process::object(proc, h) else { return status::INVALID_HANDLE };
    let mut table = NAMED.lock();
    table.retain(|_, (_, o)| still_held(o));
    table.insert(name, (kind as u32, obj));
    status::SUCCESS
}

/// VEIL_OPEN_NAMED(name*, kind, out*). A name held by another kind of object
/// is STATUS_OBJECT_TYPE_MISMATCH.
pub fn open_named(proc: &Process, name: u64, kind: u64, out: u64) -> NtStatus {
    if !ptr_ok(out, 8) {
        return status::ACCESS_VIOLATION;
    }
    let Some(name) = read_name(name) else { return status::ACCESS_VIOLATION };
    let found = {
        let mut table = NAMED.lock();
        table.retain(|_, (_, o)| still_held(o));
        table.get(&name).cloned()
    };
    match found {
        None => STATUS_OBJECT_NAME_NOT_FOUND,
        Some((k, _)) if k != kind as u32 => STATUS_OBJECT_TYPE_MISMATCH,
        Some((_, obj)) => {
            let h = proc.handles.lock().allocate_ref(obj, 0x1F_0003, 0);
            unsafe { *(out as *mut u64) = h as u64 };
            status::SUCCESS
        }
    }
}

/// VEIL_MUTEX(op, a, b). Op 0, create: initially owned, out*. Op 1, release: handle.
pub fn mutex_call(proc: &Process, op: u64, a: u64, b: u64) -> NtStatus {
    use crate::nt_objects::MutexObject;
    match op {
        0 => {
            if !ptr_ok(b, 8) {
                return status::ACCESS_VIOLATION;
            }
            let m = Arc::new(MutexObject::new(None, false));
            if a != 0 {
                m.try_acquire(MutexObject::current_owner());
            }
            let h = proc.handles.lock().allocate_mutex(m, 0x1F_0001);
            unsafe { *(b as *mut u64) = h as u64 };
            status::SUCCESS
        }
        1 => match crate::nt_process::object(proc, a) {
            Some(KernelObjectRef::Mutex(m)) => match m.release(MutexObject::current_owner()) {
                Ok(_) => status::SUCCESS,
                Err(_) => 0xC000_0046u32 as NtStatus, // STATUS_MUTANT_NOT_OWNED
            },
            _ => status::INVALID_HANDLE,
        },
        _ => status::INVALID_PARAMETER,
    }
}

// -----| address waits |-----

const WAKE_SLOTS: usize = 256;
static WAKES: [AtomicU64; WAKE_SLOTS] = [const { AtomicU64::new(0) }; WAKE_SLOTS];

fn slot(addr: u64) -> &'static AtomicU64 {
    &WAKES[((addr >> 3) ^ (addr >> 11)) as usize % WAKE_SLOTS]
}

/// VEIL_WAIT_ADDRESS(addr, compare*, size, ms; u64::MAX: forever).
pub fn wait_address(addr: u64, compare: u64, size: u64, ms: u64) -> NtStatus {
    let size = size as usize;
    if !matches!(size, 1 | 2 | 4 | 8) {
        return status::INVALID_PARAMETER;
    }
    if !ptr_ok(addr, size) || !ptr_ok(compare, size) {
        return status::ACCESS_VIOLATION;
    }
    let seq = slot(addr);
    let seen = seq.load(Ordering::SeqCst);
    let same = unsafe {
        core::slice::from_raw_parts(addr as *const u8, size) == core::slice::from_raw_parts(compare as *const u8, size)
    };
    if !same {
        return status::SUCCESS;
    }
    let until = (ms != u64::MAX).then(|| crate::time::now_ms().saturating_add(ms));
    let mut note = crate::nt_syscalls::WaitNote::new("WaitOnAddress");
    while seq.load(Ordering::SeqCst) == seen {
        if until.is_some_and(|t| crate::time::now_ms() >= t) {
            return STATUS_TIMEOUT;
        }
        crate::loader::kill_if_requested();
        note.tick();
        crate::nt_syscalls::wait_a_little();
    }
    status::SUCCESS
}

/// VEIL_WAKE_ADDRESS(addr).
pub fn wake_address(addr: u64) -> NtStatus {
    slot(addr).fetch_add(1, Ordering::SeqCst);
    status::SUCCESS
}

// -----| named pipes |-----

/// Both directions of one connection.
struct Stream {
    client_to_server: Arc<Pipe>,
    server_to_client: Arc<Pipe>,
}

impl Stream {
    fn new(capacity: u64) -> Self {
        Self { client_to_server: new_pipe(capacity), server_to_client: new_pipe(capacity) }
    }
}

const LISTENING: u8 = 0;
const CONNECTED: u8 = 1;
const DISCONNECTED: u8 = 2;

pub struct Instance {
    name: String,
    capacity: u64,
    stream: Mutex<Stream>,
    state: AtomicU8,
}

/// A handle to one side of an instance.
pub struct DuplexEnd {
    instance: Arc<Instance>,
    is_server: bool,
    /// (read, write).
    ends: Mutex<(Arc<PipeEnd>, Arc<PipeEnd>)>,
}

impl DuplexEnd {
    fn server(instance: Arc<Instance>) -> Self {
        let ends = {
            let s = instance.stream.lock();
            (PipeEnd::reader(s.client_to_server.clone()), PipeEnd::writer(s.server_to_client.clone()))
        };
        Self { instance, is_server: true, ends: Mutex::new(ends) }
    }

    fn client(instance: Arc<Instance>) -> Self {
        let ends = {
            let s = instance.stream.lock();
            (PipeEnd::reader(s.server_to_client.clone()), PipeEnd::writer(s.client_to_server.clone()))
        };
        Self { instance, is_server: false, ends: Mutex::new(ends) }
    }

    /// Read or write this side: (status, bytes).
    pub fn io(&self, buffer: u64, length: usize, writing: bool) -> (NtStatus, usize) {
        if self.is_server && self.instance.state.load(Ordering::Acquire) != CONNECTED {
            return (STATUS_PIPE_LISTENING, 0);
        }
        let (read, write) = self.ends.lock().clone();
        crate::nt_process::pipe_io(if writing { &write } else { &read }, buffer, length, writing)
    }

    fn available(&self) -> usize {
        pipe_available(&self.ends.lock().0.pipe)
    }
}

static PIPES: Mutex<Vec<alloc::sync::Weak<Instance>>> = Mutex::new(Vec::new());

fn find_listening(name: &str) -> (bool, Option<Arc<Instance>>) {
    let mut pipes = PIPES.lock();
    pipes.retain(|w| w.strong_count() != 0);
    let mut exists = false;
    for w in pipes.iter() {
        let Some(i) = w.upgrade() else { continue };
        if !i.name.eq_ignore_ascii_case(name) {
            continue;
        }
        exists = true;
        if i.state.load(Ordering::Acquire) == LISTENING {
            return (true, Some(i));
        }
    }
    (exists, None)
}

fn put_end(proc: &Process, end: DuplexEnd, out: u64) {
    let h = proc.handles.lock().allocate_ref(KernelObjectRef::Duplex(Arc::new(end)), 0x0012_019F, 0);
    unsafe { *(out as *mut u64) = h as u64 };
}

fn duplex(proc: &Process, h: u64) -> Option<Arc<DuplexEnd>> {
    match crate::nt_process::object(proc, h) {
        Some(KernelObjectRef::Duplex(d)) => Some(d),
        _ => None,
    }
}

/// VEIL_PIPE(op, a, b, c).
pub fn pipe_call(proc: &Process, op: u64, a: u64, b: u64, c: u64) -> NtStatus {
    match op {
        // Create a server end: name* (0: nameless), out*, buffer size.
        0 => {
            if !ptr_ok(b, 8) {
                return status::ACCESS_VIOLATION;
            }
            let name = if a == 0 {
                String::new()
            } else {
                match read_name(a) {
                    Some(n) => n,
                    None => return status::ACCESS_VIOLATION,
                }
            };
            let capacity = c.max(4096);
            let instance = Arc::new(Instance { name, capacity, stream: Mutex::new(Stream::new(capacity)), state: AtomicU8::new(LISTENING) });
            if a != 0 {
                PIPES.lock().push(Arc::downgrade(&instance));
            }
            put_end(proc, DuplexEnd::server(instance), b);
            status::SUCCESS
        }
        // Connect as a client by name: name*, out*.
        1 => {
            if !ptr_ok(b, 8) {
                return status::ACCESS_VIOLATION;
            }
            let Some(name) = read_name(a) else { return status::ACCESS_VIOLATION };
            match find_listening(&name) {
                (false, _) => STATUS_OBJECT_NAME_NOT_FOUND,
                (true, None) => STATUS_PIPE_NOT_AVAILABLE,
                (true, Some(i)) => {
                    i.state.store(CONNECTED, Ordering::Release);
                    put_end(proc, DuplexEnd::client(i), b);
                    status::SUCCESS
                }
            }
        }
        // ConnectNamedPipe(server): waits for a client.
        2 => {
            let Some(d) = duplex(proc, a) else { return status::INVALID_HANDLE };
            let _ = d.instance.state.compare_exchange(DISCONNECTED, LISTENING, Ordering::AcqRel, Ordering::Acquire);
            if d.instance.state.load(Ordering::Acquire) == CONNECTED {
                return STATUS_PIPE_CONNECTED;
            }
            while d.instance.state.load(Ordering::Acquire) != CONNECTED {
                crate::loader::kill_if_requested();
                crate::nt_syscalls::wait_a_little();
            }
            status::SUCCESS
        }
        // DisconnectNamedPipe(server): new streams; no client until ConnectNamedPipe.
        3 => {
            let Some(d) = duplex(proc, a) else { return status::INVALID_HANDLE };
            if !d.is_server {
                return status::INVALID_PARAMETER;
            }
            let ends = {
                let mut s = d.instance.stream.lock();
                *s = Stream::new(d.instance.capacity);
                (PipeEnd::reader(s.client_to_server.clone()), PipeEnd::writer(s.server_to_client.clone()))
            };
            *d.ends.lock() = ends;
            d.instance.state.store(DISCONNECTED, Ordering::Release);
            status::SUCCESS
        }
        // PeekNamedPipe(handle, available*).
        4 => {
            if !ptr_ok(b, 4) {
                return status::ACCESS_VIOLATION;
            }
            let n = match crate::nt_process::object(proc, a) {
                Some(KernelObjectRef::Duplex(d)) => d.available(),
                Some(KernelObjectRef::Pipe(e)) => pipe_available(&e.pipe),
                _ => return status::INVALID_HANDLE,
            };
            unsafe { *(b as *mut u32) = n as u32 };
            status::SUCCESS
        }
        // WaitNamedPipe(name*, ms): for an instance to be free.
        5 => {
            let Some(name) = read_name(a) else { return status::ACCESS_VIOLATION };
            let until = crate::time::now_ms().saturating_add(b);
            loop {
                match find_listening(&name) {
                    (false, _) => return STATUS_OBJECT_NAME_NOT_FOUND,
                    (true, Some(_)) => return status::SUCCESS,
                    (true, None) => {}
                }
                if crate::time::now_ms() >= until {
                    return STATUS_IO_TIMEOUT;
                }
                crate::loader::kill_if_requested();
                crate::nt_syscalls::wait_a_little();
            }
        }
        // A new handle to this process's console, redirected or not: out*.
        8 => {
            if !ptr_ok(a, 8) {
                return status::ACCESS_VIOLATION;
            }
            let Some(console) = proc.console.clone() else { return status::INVALID_HANDLE };
            let h = proc.handles.lock().allocate_ref(KernelObjectRef::Console(console), 0x0012_019F, 0);
            unsafe { *(a as *mut u64) = h as u64 };
            status::SUCCESS
        }
        // A handle to the NUL device: out*.
        7 => {
            if !ptr_ok(a, 8) {
                return status::ACCESS_VIOLATION;
            }
            let h = proc.handles.lock().allocate_ref(KernelObjectRef::Null, 0x0012_019F, 0);
            unsafe { *(a as *mut u64) = h as u64 };
            status::SUCCESS
        }
        // Connect as a client to the instance of a server handle: server, out*.
        6 => {
            if !ptr_ok(b, 8) {
                return status::ACCESS_VIOLATION;
            }
            let Some(d) = duplex(proc, a) else { return status::INVALID_HANDLE };
            if d.instance.state.swap(CONNECTED, Ordering::AcqRel) != LISTENING {
                return STATUS_PIPE_NOT_AVAILABLE;
            }
            put_end(proc, DuplexEnd::client(d.instance.clone()), b);
            status::SUCCESS
        }
        _ => status::INVALID_PARAMETER,
    }
}
