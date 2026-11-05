// =============================================================================
// VEILOS: Windows threads, child processes and pipes (WIN32_DESIGN.md, W6)
// =============================================================================
//
// A Windows process runs any number of threads. Each is a `ThreadObj`: a
// TEB and a user stack in the process, and a kernel thread (kthread) of its
// own, placed on the least-loaded CPU. The first starts at the loader's
// startup routine; the others at ntdll's RtlUserThreadStart(start, param),
// which sets up their TLS and calls the DLLs before the start routine.
//
// A process ends when its last thread stops: that thread tears it down
// (loader::on_process_exit). ExitProcess or TerminateProcess marks it
// `dying`; every thread checks that whenever it could be stopped (a timer
// tick in user mode, any wait in a syscall), so all of them stop and the
// last one out cleans up.
//
// Processes, threads and anonymous pipes are handle-table objects. Process
// and thread handles are signalled once the process has been torn down or
// the thread has stopped; waits (single, any, all) give the CPU away until
// then. CreateProcess loads the program with the loader's options (command
// line, environment, current directory, console, standard handles).

use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use nt_abi::{status, NtStatus, ProcessParams, ProcessResult, ThreadResult, CURRENT_PROCESS, CURRENT_THREAD};
use spin::Mutex;
use x86_64::structures::paging::{Page, PageTableFlags};
use x86_64::VirtAddr;

use crate::interrupts::syscall_stack_arg as stack;
use crate::nt_file::ptr_ok;
use crate::nt_objects::KernelObjectRef;
use crate::process::Process;

/// Where and how a thread enters user mode.
#[derive(Clone, Copy)]
pub struct Start {
    pub rip: u64,
    pub rsp: u64,
    pub teb: u64,
    pub rcx: u64,
    pub rdx: u64,
}

pub struct ThreadObj {
    pub tid: u32,
    pub pid: u32,
    pub teb: u64,
    pub start: Start,
    /// Created suspended and not started yet.
    pub suspended: AtomicBool,
    /// The kthread running it (0 until started).
    pub kthread: AtomicU64,
    pub exit_code: Mutex<Option<u32>>,
    /// Stopped: its handles are signalled.
    pub done: AtomicBool,
    /// Its stack and TEB pages (the first thread's belong to the process).
    pub pages: Mutex<Vec<u64>>,
}

static NEXT_TID: AtomicU32 = AtomicU32::new(1);

/// Thread ids, like Windows', are multiples of 4.
pub fn alloc_tid() -> u32 {
    NEXT_TID.fetch_add(1, Ordering::Relaxed) * 4
}

impl ThreadObj {
    pub fn new(pid: u32, tid: u32, start: Start, suspended: bool, pages: Vec<u64>) -> Self {
        ThreadObj {
            tid,
            pid,
            teb: start.teb,
            start,
            suspended: AtomicBool::new(suspended),
            kthread: AtomicU64::new(0),
            exit_code: Mutex::new(None),
            done: AtomicBool::new(false),
            pages: Mutex::new(pages),
        }
    }
}

// -----| running threads |-----

/// Run `thread` (counted as live already) on the least-loaded CPU.
pub fn start_thread(process: Arc<Process>, thread: Arc<ThreadObj>) {
    if crate::smp::CPU_COUNT.load(Ordering::Relaxed) > 1 {
        crate::smp::AP_WORK_QUEUE.lock().push(crate::smp::ApWorkItem { pid: process.pid, tid: thread.tid });
        crate::smp::wake_aps();
    } else {
        run_thread_here(process, thread);
    }
}

/// Start `thread` on a new kthread on this CPU.
pub fn run_thread_here(process: Arc<Process>, thread: Arc<ThreadObj>) {
    let cpu = crate::smp::cpu_index();
    let (pid, cr3) = (process.pid, process.cr3);
    crate::smp::USER_THREADS[cpu].fetch_add(1, Ordering::AcqRel);
    let t = thread.clone();
    let id = crate::kthread::spawn_user("user-thread", cr3, pid, move || {
        unsafe {
            let user = crate::kthread::current_user();
            (*user).dying = &process.dying as *const AtomicBool as usize;
            (*user).tid = t.tid;
        }
        if !process.dying.load(Ordering::Acquire) {
            let s = t.start;
            unsafe { crate::loader::launch_into_usermode_with(s.rip, s.rsp, s.teb, s.rcx, s.rdx) };
            // Back here by the long jump out of ring 3 (thread or process
            // exit, a fault, a kill), with interrupts still off.
            x86_64::instructions::interrupts::enable();
        }
        thread_stopped(&process, &t);
        let cpu = crate::smp::cpu_index();
        crate::smp::USER_THREADS[cpu].fetch_sub(1, Ordering::AcqRel);
    });
    thread.kthread.store(id, Ordering::Release);
}

/// A thread has left user mode for good. The last one out ends the process.
fn thread_stopped(process: &Arc<Process>, thread: &Arc<ThreadObj>) {
    crate::loader::release_leaked_locks();
    let code = {
        let mut c = thread.exit_code.lock();
        *c.get_or_insert_with(|| process.exit_code.lock().map_or(0, |c| c as u32))
    };
    crate::nt_user::thread_exited(thread.tid);
    let last = process.live_threads.fetch_sub(1, Ordering::AcqRel) == 1;
    if last {
        if process.is_alive() {
            crate::serial_println!("[NT] pid {}: last thread {} exited with {}", process.pid, thread.tid, code as i32);
            process.terminate(code as i32);
        }
        process.dying.store(true, Ordering::Release);
        thread.done.store(true, Ordering::Release);
        // Every thread's own pages, then the process.
        for t in process.threads.lock().iter() {
            free_pages(&mut t.pages.lock());
            t.done.store(true, Ordering::Release);
        }
        crate::loader::on_process_exit(process);
        process.ended.store(true, Ordering::Release);
    } else {
        free_pages(&mut thread.pages.lock());
        thread.done.store(true, Ordering::Release);
        crate::loader::invalidate_exec_context();
        crate::serial_println!("[NT] pid {}: thread {} exited with {}", process.pid, thread.tid, code as i32);
    }
}

/// Unmap and free pages of the current address space.
fn free_pages(pages: &mut Vec<u64>) {
    if pages.is_empty() {
        return;
    }
    let mut mapper = unsafe { crate::memory::get_mapper() };
    for addr in pages.drain(..) {
        let _ = crate::memory::unmap_and_free_page(Page::containing_address(VirtAddr::new(addr)), &mut mapper);
    }
}

/// End process `p` (another, or this one) with `code`: all its threads
/// stop at their next chance.
pub fn kill_process(p: &Process, code: i32) {
    if p.is_alive() {
        p.terminate(code);
    }
    p.dying.store(true, Ordering::Release);
    crate::kthread::kill_pid(p.pid);
}

/// The running thread's ThreadObj.
pub fn current_thread(proc: &Process) -> Option<Arc<ThreadObj>> {
    let tid = unsafe { (*crate::kthread::current_user()).tid };
    proc.threads.lock().iter().find(|t| t.tid == tid).cloned()
}

// -----| thread services |-----

/// User stacks of threads after the first: 1 MiB each, in their own range.
const THREAD_STACK: u64 = 0x10_0000;
static NEXT_STACK: AtomicU64 = AtomicU64::new(0x6000_0000_0000);

/// Map `pages` fresh zeroed user pages at `base` in the current address space.
fn map_user(base: u64, pages: u64, exec: bool, out: &mut Vec<u64>) -> Result<(), NtStatus> {
    let mut mapper = unsafe { crate::memory::get_mapper() };
    let mut fa = crate::memory::GlobalFrameAllocator;
    let mut flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::USER_ACCESSIBLE;
    if !exec {
        flags |= PageTableFlags::NO_EXECUTE;
    }
    for i in 0..pages {
        let page = Page::containing_address(VirtAddr::new(base + i * 0x1000));
        let frame = crate::memory::allocate_frame().ok_or(status::NO_MEMORY)?;
        crate::memory::map_page(page, frame, flags, &mut mapper, &mut fa);
        out.push(page.start_address().as_u64());
        unsafe { core::ptr::write_bytes((base + i * 0x1000) as *mut u8, 0, 4096) };
    }
    Ok(())
}

/// VEIL_CREATE_THREAD(start, parameter, flags, result*).
pub fn create_thread(proc: &Arc<Process>, start: u64, param: u64, flags: u64, result: u64) -> NtStatus {
    if !ptr_ok(result, core::mem::size_of::<ThreadResult>()) {
        return status::ACCESS_VIOLATION;
    }
    let entry = proc.thread_entry.load(Ordering::Acquire);
    if entry == 0 {
        return status::PROCEDURE_NOT_FOUND;
    }
    let tid = alloc_tid();
    let mut pages = Vec::new();

    // Stack: its size in pages in the upper half of `flags`, at least the
    // default, zero pages as they're touched. A gap below it catches overflow.
    let stack_size = ((flags >> 32) * 0x1000).max(THREAD_STACK).next_multiple_of(0x1_0000);
    let stack_base = NEXT_STACK.fetch_add(stack_size + THREAD_STACK, Ordering::Relaxed);
    crate::nt_demand::add_zero(stack_base, stack_size);
    let stack_top = stack_base + stack_size;
    // As if called: a null return address and home space.
    let rsp = stack_top - 40;

    // TEB: like the first thread's (loader.rs), sharing its PEB.
    let teb = crate::loader::alloc_teb_range(2);
    if let Err(s) = map_user(teb, 2, false, &mut pages) {
        free_pages(&mut pages);
        return s;
    }
    let peb = proc.nt_env.lock().peb_base;
    unsafe {
        use nt_abi::layout::*;
        let w = |o: usize, v: u64| core::ptr::write_unaligned((teb as usize + o) as *mut u64, v);
        w(0x00, u64::MAX);
        w(0x08, stack_top);
        w(0x10, stack_base);
        w(0x30, teb);
        w(TEB_PROCESS_ID, proc.pid as u64);
        w(TEB_THREAD_ID, tid as u64);
        w(TEB_PEB, peb);
    }
    {
        let mut vad = proc.vad.lock();
        vad.record_image(stack_base, stack_size as usize);
        vad.record_image(teb, 0x2000);
    }

    let suspended = flags as u32 & 4 != 0; // CREATE_SUSPENDED
    let t = Arc::new(ThreadObj::new(proc.pid, tid, Start { rip: entry, rsp, teb, rcx: start, rdx: param }, suspended, pages));
    proc.threads.lock().push(t.clone());
    let h = proc.handles.lock().allocate_ref(KernelObjectRef::Thread(t.clone()), 0x001F_FFFF, 0);
    unsafe { *(result as *mut ThreadResult) = ThreadResult { handle: h as u64, tid: tid as u64 } };
    if !suspended {
        proc.live_threads.fetch_add(1, Ordering::AcqRel);
        start_thread(proc.clone(), t);
    }
    status::SUCCESS
}

/// NtTerminateThread(handle, code).
pub fn terminate_thread(proc: &Arc<Process>, handle: u64, code: u64) -> NtStatus {
    let target = if handle == CURRENT_THREAD || handle == 0 {
        current_thread(proc)
    } else {
        match object(proc, handle) {
            Some(KernelObjectRef::Thread(t)) => Some(t),
            Some(_) => return status::OBJECT_TYPE_MISMATCH,
            None => return status::INVALID_HANDLE,
        }
    };
    let Some(t) = target else { return status::INVALID_HANDLE };
    {
        let mut c = t.exit_code.lock();
        if c.is_none() {
            *c = Some(code as u32);
        }
    }
    let mine = unsafe { (*crate::kthread::current_user()).tid } == t.tid;
    if mine {
        unsafe { crate::loader::return_from_usermode() }
    }
    if t.suspended.swap(false, Ordering::AcqRel) {
        // Never started: nothing runs, it's simply over.
        t.done.store(true, Ordering::Release);
        return status::SUCCESS;
    }
    crate::kthread::kill_thread(t.kthread.load(Ordering::Acquire));
    status::SUCCESS
}

/// NtResumeThread(handle, previous_count*): starts a thread created suspended.
pub fn resume_thread(proc: &Arc<Process>, handle: u64, prev: u64) -> NtStatus {
    let Some(KernelObjectRef::Thread(t)) = object(proc, handle) else { return status::INVALID_HANDLE };
    let was = t.suspended.swap(false, Ordering::AcqRel);
    if ptr_ok(prev, 4) {
        unsafe { *(prev as *mut u32) = was as u32 };
    }
    if was {
        // It may belong to a child process created suspended.
        let owner = if t.pid == proc.pid { Some(proc.clone()) } else { crate::process::PROCESSOR.lock().find(t.pid) };
        if let Some(owner) = owner {
            owner.live_threads.fetch_add(1, Ordering::AcqRel);
            start_thread(owner, t);
        }
    }
    status::SUCCESS
}

// -----| objects, waits and queries |-----

pub fn object(proc: &Process, handle: u64) -> Option<KernelObjectRef> {
    let h = u32::try_from(handle).ok()?;
    proc.handles.lock().get_ref(h).cloned()
}

/// A handle, or one of the pseudo handles for this process and thread.
fn object_or_self(proc: &Arc<Process>, handle: u64) -> Option<KernelObjectRef> {
    match handle {
        CURRENT_PROCESS => Some(KernelObjectRef::Process(proc.clone())),
        CURRENT_THREAD => current_thread(proc).map(KernelObjectRef::Thread),
        h => object(proc, h),
    }
}

/// Is `obj` signalled? Auto-reset events are consumed if `take`.
fn signalled(obj: &KernelObjectRef, take: bool) -> bool {
    match obj {
        KernelObjectRef::Event(ev) => {
            if take {
                ev.test_and_consume()
            } else {
                ev.signaled.load(Ordering::Acquire)
            }
        }
        KernelObjectRef::Semaphore(s) => {
            if take {
                s.try_acquire()
            } else {
                s.count.load(Ordering::Acquire) > 0
            }
        }
        KernelObjectRef::Mutex(m) => {
            let me = crate::nt_objects::MutexObject::current_owner();
            if take {
                m.try_acquire(me)
            } else {
                !m.is_locked() || m.owner() == me
            }
        }
        KernelObjectRef::Process(p) => p.ended.load(Ordering::Acquire),
        KernelObjectRef::Thread(t) => t.done.load(Ordering::Acquire),
        _ => true,
    }
}

/// Wait for one (`all` false) or all of `objs`. Returns the index of the
/// signalled object (any), 0 (all), or STATUS_TIMEOUT.
pub fn wait(objs: &[KernelObjectRef], all: bool, until: Option<u64>) -> NtStatus {
    let mut note = crate::nt_syscalls::WaitNote::new(match objs.first() {
        Some(KernelObjectRef::Event(_)) => "a wait on an event",
        Some(KernelObjectRef::Semaphore(_)) => "a wait on a semaphore",
        Some(KernelObjectRef::Mutex(_)) => "a wait on a mutex",
        Some(KernelObjectRef::Thread(_)) => "a wait on a thread",
        Some(KernelObjectRef::Process(_)) => "a wait on a process",
        _ => "a wait",
    });
    loop {
        if all {
            if objs.iter().all(|o| signalled(o, false)) {
                for o in objs {
                    signalled(o, true);
                }
                return status::SUCCESS;
            }
        } else if let Some(i) = objs.iter().position(|o| signalled(o, true)) {
            return i as NtStatus;
        }
        if until.is_some_and(|u| crate::interrupts::TICKS.load(Ordering::Relaxed) >= u) {
            return status::TIMEOUT;
        }
        crate::loader::kill_if_requested();
        note.tick();
        crate::nt_syscalls::wait_a_little();
    }
}

/// NtWaitForSingleObject(handle, alertable, timeout*).
pub fn wait_single(proc: &Arc<Process>, h: u64, timeout: u64) -> NtStatus {
    let Some(obj) = object_or_self(proc, h) else { return status::INVALID_HANDLE };
    wait(&[obj], false, crate::nt_syscalls::deadline(timeout))
}

/// NtWaitForMultipleObjects(count, handles*, wait_type, alertable, timeout*).
pub fn wait_multiple(proc: &Arc<Process>, count: u64, handles: u64, kind: u64) -> NtStatus {
    let timeout = stack(4).unwrap_or(0);
    let (n, kind) = (count as u32 as usize, kind as u32);
    if n == 0 || n > 64 || !ptr_ok(handles, n * 8) {
        return status::INVALID_PARAMETER;
    }
    let mut objs = Vec::with_capacity(n);
    for i in 0..n {
        let h = unsafe { core::ptr::read_unaligned((handles as *const u64).add(i)) };
        match object_or_self(proc, h) {
            Some(o) => objs.push(o),
            None => return status::INVALID_HANDLE,
        }
    }
    wait(&objs, kind == 0, crate::nt_syscalls::deadline(timeout))
}

/// NtQueryInformationProcess(handle, ProcessBasicInformation, info*, length, return_length*).
pub fn query_process(proc: &Arc<Process>, h: u64, class: u64, out: u64, length: u64) -> NtStatus {
    let ret_len = stack(4).unwrap_or(0);
    let (class, length) = (class as u32, length as u32);
    let Some(KernelObjectRef::Process(p)) = object_or_self(proc, h) else { return status::INVALID_HANDLE };
    if class != 0 {
        return status::INVALID_INFO_CLASS;
    }
    if length < 48 {
        return status::BUFFER_TOO_SMALL;
    }
    if !ptr_ok(out, 48) {
        return status::ACCESS_VIOLATION;
    }
    let exit = if p.ended.load(Ordering::Acquire) { p.exit_code.lock().unwrap_or(0) } else { status::PENDING };
    unsafe {
        let o = out as *mut u64;
        core::ptr::write_bytes(o as *mut u8, 0, 48);
        *o = exit as u32 as u64;
        *o.add(1) = p.nt_env.lock().peb_base;
        *o.add(2) = 1;
        *o.add(3) = 8;
        *o.add(4) = p.pid as u64;
        *o.add(5) = p.parent_pid.load(Ordering::Relaxed) as u64;
    }
    if ptr_ok(ret_len, 4) {
        unsafe { *(ret_len as *mut u32) = 48 };
    }
    status::SUCCESS
}

/// NtQueryInformationThread(handle, ThreadBasicInformation, info*, length, return_length*).
pub fn query_thread(proc: &Arc<Process>, h: u64, class: u64, out: u64, length: u64) -> NtStatus {
    let ret_len = stack(4).unwrap_or(0);
    let (class, length) = (class as u32, length as u32);
    let Some(KernelObjectRef::Thread(t)) = object_or_self(proc, h) else { return status::INVALID_HANDLE };
    if class != 0 {
        return status::INVALID_INFO_CLASS;
    }
    if length < 48 {
        return status::BUFFER_TOO_SMALL;
    }
    if !ptr_ok(out, 48) {
        return status::ACCESS_VIOLATION;
    }
    let exit = if t.done.load(Ordering::Acquire) { t.exit_code.lock().unwrap_or(0) as i32 } else { status::PENDING };
    unsafe {
        let o = out as *mut u64;
        core::ptr::write_bytes(o as *mut u8, 0, 48);
        *o = exit as u32 as u64;
        *o.add(1) = t.teb;
        *o.add(2) = t.pid as u64;
        *o.add(3) = t.tid as u64;
        *o.add(4) = 1;
        *(o.add(5) as *mut u32) = 8;
        *((o.add(5) as *mut u32).add(1)) = 8;
    }
    if ptr_ok(ret_len, 4) {
        unsafe { *(ret_len as *mut u32) = 48 };
    }
    status::SUCCESS
}

/// NtDuplicateObject(source_process, source_handle, target_process,
/// target_handle*, access, attributes, options): from this process, to
/// this process or one it has a handle to.
pub fn duplicate(proc: &Arc<Process>, src_proc: u64, src: u64, dst_proc: u64, out: u64) -> NtStatus {
    let attributes = stack(5).unwrap_or(0) as u32;
    let options = stack(6).unwrap_or(0) as u32;
    if src_proc != CURRENT_PROCESS {
        match object(proc, src_proc) {
            Some(KernelObjectRef::Process(p)) if p.pid == proc.pid => {}
            _ => return status::ACCESS_DENIED,
        }
    }
    let Some(obj) = object_or_self(proc, src) else { return status::INVALID_HANDLE };
    let target = if dst_proc == CURRENT_PROCESS {
        proc.clone()
    } else {
        match object(proc, dst_proc) {
            Some(KernelObjectRef::Process(p)) => p,
            _ => return status::INVALID_HANDLE,
        }
    };
    // DUPLICATE_SAME_ATTRIBUTES keeps the source's; otherwise OBJ_INHERIT says.
    let inherit = if options & 4 != 0 {
        proc.handles.lock().inheritable(src as u32).unwrap_or(false)
    } else {
        attributes & 2 != 0
    };
    let kept = if inherit { 0 } else { crate::nt_objects::HANDLE_NO_INHERIT };
    let h = target.handles.lock().allocate_ref(obj, 0x001F_FFFF, kept);
    if options & 1 != 0 && src != CURRENT_PROCESS && src != CURRENT_THREAD {
        // DUPLICATE_CLOSE_SOURCE
        proc.handles.lock().close(src as u32);
    }
    if ptr_ok(out, 8) {
        unsafe { *(out as *mut u64) = h as u64 };
    }
    status::SUCCESS
}

// -----| pipes |-----

pub struct Pipe {
    data: Mutex<VecDeque<u8>>,
    readers: AtomicUsize,
    writers: AtomicUsize,
    capacity: usize,
}

/// One end of a pipe; the pipe breaks when all handles to an end are gone.
pub struct PipeEnd {
    pub pipe: Arc<Pipe>,
    pub write: bool,
}

/// A pipe with no ends yet.
pub fn new_pipe(capacity: u64) -> Arc<Pipe> {
    Arc::new(Pipe {
        data: Mutex::new(VecDeque::new()),
        readers: AtomicUsize::new(0),
        writers: AtomicUsize::new(0),
        capacity: (capacity as u32 as usize).clamp(4096, 1 << 20),
    })
}

/// Bytes waiting to be read.
pub fn pipe_available(pipe: &Pipe) -> usize {
    pipe.data.lock().len()
}

impl PipeEnd {
    pub fn reader(pipe: Arc<Pipe>) -> Arc<Self> {
        Arc::new(Self::new(pipe, false))
    }

    pub fn writer(pipe: Arc<Pipe>) -> Arc<Self> {
        Arc::new(Self::new(pipe, true))
    }

    fn new(pipe: Arc<Pipe>, write: bool) -> Self {
        let n = if write { &pipe.writers } else { &pipe.readers };
        n.fetch_add(1, Ordering::AcqRel);
        PipeEnd { pipe, write }
    }
}

impl Drop for PipeEnd {
    fn drop(&mut self) {
        let n = if self.write { &self.pipe.writers } else { &self.pipe.readers };
        n.fetch_sub(1, Ordering::AcqRel);
    }
}

/// VEIL_CREATE_PIPE(read_out*, write_out*, size).
pub fn create_pipe(proc: &Process, read_out: u64, write_out: u64, size: u64) -> NtStatus {
    if !ptr_ok(read_out, 8) || !ptr_ok(write_out, 8) {
        return status::ACCESS_VIOLATION;
    }
    let pipe = new_pipe(size);
    let mut handles = proc.handles.lock();
    let r = handles.allocate_ref(KernelObjectRef::Pipe(Arc::new(PipeEnd::new(pipe.clone(), false))), 0x0012_019F, 0);
    let w = handles.allocate_ref(KernelObjectRef::Pipe(Arc::new(PipeEnd::new(pipe, true))), 0x0012_019F, 0);
    unsafe {
        *(read_out as *mut u64) = r as u64;
        *(write_out as *mut u64) = w as u64;
    }
    status::SUCCESS
}

/// Read or write a pipe end; returns (status, bytes).
pub fn pipe_io(end: &PipeEnd, buffer: u64, length: usize, writing: bool) -> (NtStatus, usize) {
    let pipe = &end.pipe;
    let mut note = crate::nt_syscalls::WaitNote::new(if writing { "a pipe write" } else { "a pipe read" });
    if writing {
        if !end.write {
            return (status::ACCESS_DENIED, 0);
        }
        let mut done = 0;
        while done < length {
            if pipe.readers.load(Ordering::Acquire) == 0 {
                return (status::PIPE_BROKEN, done);
            }
            {
                let mut data = pipe.data.lock();
                let room = pipe.capacity.saturating_sub(data.len()).min(length - done);
                for i in 0..room {
                    data.push_back(unsafe { *((buffer as usize + done + i) as *const u8) });
                }
                done += room;
            }
            if done < length {
                crate::loader::kill_if_requested();
                note.tick();
                crate::nt_syscalls::wait_a_little();
            }
        }
        (status::SUCCESS, done)
    } else {
        if end.write {
            return (status::ACCESS_DENIED, 0);
        }
        loop {
            {
                let mut data = pipe.data.lock();
                if !data.is_empty() {
                    let n = data.len().min(length);
                    for i in 0..n {
                        unsafe { *((buffer as usize + i) as *mut u8) = data.pop_front().unwrap() };
                    }
                    return (status::SUCCESS, n);
                }
            }
            if pipe.writers.load(Ordering::Acquire) == 0 {
                return (status::PIPE_BROKEN, 0);
            }
            if length == 0 {
                return (status::SUCCESS, 0);
            }
            crate::loader::kill_if_requested();
            note.tick();
            crate::nt_syscalls::wait_a_little();
        }
    }
}

// -----| processes |-----

/// A NUL-terminated UTF-16 string from the caller (at most `max` units).
fn read_wide(p: u64, max: usize) -> Option<String> {
    if p == 0 {
        return None;
    }
    let mut units = Vec::new();
    while units.len() < max {
        let a = p + units.len() as u64 * 2;
        if !ptr_ok(a, 2) {
            return None;
        }
        let u = unsafe { core::ptr::read_unaligned(a as *const u16) };
        if u == 0 {
            break;
        }
        units.push(u);
    }
    Some(char::decode_utf16(units).map(|c| c.unwrap_or('?')).collect())
}

/// A double-NUL-terminated UTF-16 environment block from the caller.
fn read_environment(p: u64) -> Option<Vec<u16>> {
    if p == 0 {
        return None;
    }
    let mut units = Vec::new();
    loop {
        let a = p + units.len() as u64 * 2;
        if units.len() > 32_000 || !ptr_ok(a, 2) {
            return None;
        }
        let u = unsafe { core::ptr::read_unaligned(a as *const u16) };
        units.push(u);
        let n = units.len();
        if u == 0 && (n == 1 || units[n - 2] == 0) {
            if n == 1 {
                units.push(0);
            }
            return Some(units);
        }
    }
}

const CREATE_SUSPENDED: u64 = 0x4;
const DETACHED_PROCESS: u64 = 0x8;
const CREATE_NEW_CONSOLE: u64 = 0x10;

/// VEIL_CREATE_PROCESS(params*, result*).
pub fn create_process(proc: &Arc<Process>, params: u64, result: u64) -> NtStatus {
    if !ptr_ok(params, core::mem::size_of::<ProcessParams>()) || !ptr_ok(result, core::mem::size_of::<ProcessResult>()) {
        return status::ACCESS_VIOLATION;
    }
    let p = unsafe { core::ptr::read_unaligned(params as *const ProcessParams) };
    let Some(image) = read_wide(p.image, 520) else { return status::INVALID_PARAMETER };
    let Some(path) = crate::nt_file::vfs_path(&image) else { return status::OBJECT_PATH_NOT_FOUND };
    if crate::vfs::VFS.stat(&path).is_none_or(|s| s.is_dir) {
        return status::OBJECT_NAME_NOT_FOUND;
    }

    let console = if p.flags & DETACHED_PROCESS != 0 {
        crate::loader::ConsoleChoice::None
    } else if p.flags & CREATE_NEW_CONSOLE != 0 {
        crate::loader::ConsoleChoice::New
    } else {
        match &proc.console {
            Some(c) => crate::loader::ConsoleChoice::Inherit(c.clone()),
            None => crate::loader::ConsoleChoice::New,
        }
    };
    let std_objects = (p.use_std_handles != 0).then(|| {
        p.std_handles.map(|h| if h == 0 { None } else { object(proc, h) })
    });
    let runtime_data = if p.runtime_data != 0 && p.runtime_size > 0 && p.runtime_size <= 1024 && ptr_ok(p.runtime_data, p.runtime_size as usize) {
        unsafe { core::slice::from_raw_parts(p.runtime_data as *const u8, p.runtime_size as usize) }.to_vec()
    } else {
        Vec::new()
    };
    let inherited = (p.inherit_handles != 0).then(|| proc.handles.lock().duplicate_all());
    let options = crate::loader::LaunchOptions {
        command_line: read_wide(p.command_line, 32_000),
        current_dir: read_wide(p.current_directory, 520),
        environment: read_environment(p.environment),
        console,
        std_objects,
        suspended: p.flags & CREATE_SUSPENDED != 0,
        parent_pid: proc.pid,
        inherited,
        startup_flags: p.startup_flags as u32,
        runtime_data,
    };
    let mut fa = crate::memory::GlobalFrameAllocator;
    let child = match crate::loader::load_and_spawn_pe_with(&path, options, &mut fa) {
        Ok(c) => c,
        Err(e) => {
            crate::serial_println!("[NT] CreateProcess {}: {}", path, e);
            return status::INVALID_IMAGE_FORMAT;
        }
    };
    let Some(main) = child.threads.lock().first().cloned() else { return status::INVALID_IMAGE_FORMAT };
    let mut handles = proc.handles.lock();
    let ph = handles.allocate_ref(KernelObjectRef::Process(child.clone()), 0x001F_FFFF, 0);
    let th = handles.allocate_ref(KernelObjectRef::Thread(main.clone()), 0x001F_FFFF, 0);
    unsafe {
        *(result as *mut ProcessResult) = ProcessResult { process: ph as u64, thread: th as u64, pid: child.pid as u64, tid: main.tid as u64 };
    }
    crate::serial_println!("[NT] pid {} started pid {} ({})", proc.pid, child.pid, path);
    status::SUCCESS
}

/// NtQueryVolumeInformationFile(handle, iosb*, info*, length, class):
/// FileFsDeviceInformation (the device type GetFileType reports).
pub fn query_volume(proc: &Arc<Process>, h: u64, iosb: u64, out: u64, length: u64) -> NtStatus {
    let class = stack(4).unwrap_or(0) as u32;
    let length = length as u32;
    if class != 4 {
        return status::INVALID_INFO_CLASS;
    }
    if length < 8 || !ptr_ok(out, 8) {
        return status::BUFFER_TOO_SMALL;
    }
    let device: u32 = match object(proc, h) {
        Some(KernelObjectRef::Duplex(_)) => 0x11,  // FILE_DEVICE_NAMED_PIPE
        Some(KernelObjectRef::Null) => 0x15,       // FILE_DEVICE_NULL
        Some(KernelObjectRef::File(_)) => 0x07,    // FILE_DEVICE_DISK
        Some(KernelObjectRef::Console(_)) => 0x50, // FILE_DEVICE_CONSOLE
        Some(KernelObjectRef::Pipe(_)) => 0x11,    // FILE_DEVICE_NAMED_PIPE
        Some(_) => 0x22,                           // FILE_DEVICE_UNKNOWN
        None => return status::INVALID_HANDLE,
    };
    unsafe {
        *(out as *mut u32) = device;
        *((out + 4) as *mut u32) = 0;
    }
    if ptr_ok(iosb, 16) {
        unsafe { *(iosb as *mut nt_abi::IoStatusBlock) = nt_abi::IoStatusBlock { status: 0, information: 8 } };
    }
    status::SUCCESS
}
