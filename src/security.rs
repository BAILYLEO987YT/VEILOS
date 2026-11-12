// -----| Security Mode for Windows programs |-----
//
// Every Windows process carries a capability set (capability.rs). It's set
// when the process starts: a program started from the desktop gets what the
// signed-in user's role allows (`caps_for_new`), a child what its parent has.
// The NT services check it (`require`) where a program reaches past itself:
//
//   fs.write    open a disk file for writing, create, replace or truncate one
//   fs.modify   delete or rename (DELETE access, delete-on-close, NtDeleteFile)
//   fs.read     open a disk file at all
//   proc.fork   start a program
//   proc.kill   end a process outside its own family
//   proc.trace  read, write, map or allocate another process's memory, or
//               start a thread in it, outside its own family
//   net.connect / net.bind   reach or listen beyond this machine (nt_socket.rs)
//   gpu.submit  windows and 3D drawing
//
// "Its own family" is the process, its ancestors and its descendants: Git
// Bash's fork copies memory between parent and child, and programs end
// their own children. (Windows lets any process of the same user do these.)
// Anything denied answers STATUS_ACCESS_DENIED, which programs see as
// ERROR_ACCESS_DENIED, as on Windows. Each (process, capability) denial is
// logged once and shown as a notification.
//
// Performance Mode (P5) keeps the role's limits (a guest still can't write
// or reach the network) and relaxes only what Windows doesn't check either:
// between processes of the signed-in user, ending one or using its memory
// needs nothing.

use alloc::collections::BTreeSet;
use spin::Mutex;

use crate::capability::{Capability, CapabilitySet};
use crate::process::Process;
use nt_abi::{status, NtStatus};

/// What every Windows program needs to run at all: read files, run
/// images, its own IPC and shared memory, draw.
const WIN32_BASE: u64 = Capability::FsRead.bit()
    | Capability::FsExec.bit()
    | Capability::IpcCreate.bit()
    | Capability::IpcSend.bit()
    | Capability::IpcReceive.bit()
    | Capability::IpcSharedMemory.bit()
    | Capability::GpuSubmit.bit()
    | Capability::GpuShader.bit();

/// What a role may add to that.
const WIN32_BY_ROLE: u64 = Capability::FsWrite.bit()
    | Capability::FsModify.bit()
    | Capability::NetConnect.bit()
    | Capability::NetBind.bit()
    | Capability::ProcFork.bit()
    | Capability::ProcKill.bit()
    | Capability::ProcTrace.bit()
    | Capability::MeshOffload.bit();

/// The capabilities of a new Windows process: its parent's (a live Win32
/// parent), else what the signed-in user's role allows. Nobody signed in
/// (boot-time tests) counts as the root account.
pub fn caps_for_new(parent_pid: u32) -> CapabilitySet {
    if parent_pid != 0 {
        // The parent's own set, as it is now (inherit_for_child keeps only
        // the inheritable bits, which nothing sets: a child would get none).
        if let Some(p) = crate::process::PROCESSOR.lock().find(parent_pid) {
            let caps = p.capabilities.lock();
            let mut child = CapabilitySet::new(caps.permitted);
            child.effective = caps.effective;
            return child;
        }
    }
    let (user, logged_in) = {
        let s = crate::users::SESSION.lock();
        (s.username.clone(), s.logged_in)
    };
    let role_caps = if logged_in {
        crate::users::USER_DB.lock().capability_set_for(&user).permitted
    } else {
        u64::MAX
    };
    CapabilitySet::new(WIN32_BASE | (role_caps & WIN32_BY_ROLE))
}

static DENIED: Mutex<BTreeSet<(u32, u64)>> = Mutex::new(BTreeSet::new());

/// Ok if `proc` has `cap` (or Performance Mode is on), else ACCESS_DENIED,
/// logged and shown once per process and capability.
pub fn require(proc: &Process, cap: Capability, what: &str) -> Result<(), NtStatus> {
    if proc.capabilities.lock().has(cap) {
        return Ok(());
    }
    let first = {
        let mut d = DENIED.lock();
        if d.len() > 4096 {
            d.clear();
        }
        d.insert((proc.pid, cap.bit()))
    };
    if first {
        crate::serial_println!("[SECURITY] pid {}: {} denied (needs {})", proc.pid, what, cap.name());
        crate::desktop::notify(
            crate::desktop::NotifCategory::Warning,
            "SECURITY",
            &alloc::format!("A program was denied {} ({})", what, cap.name()),
        );
    }
    Err(status::ACCESS_DENIED)
}

/// `require` as an NtStatus: SUCCESS or ACCESS_DENIED.
pub fn check(proc: &Process, cap: Capability, what: &str) -> NtStatus {
    match require(proc, cap, what) {
        Ok(()) => status::SUCCESS,
        Err(s) => s,
    }
}

fn parent_of(pid: u32) -> Option<u32> {
    let p = crate::process::PROCESSOR.lock().find(pid)?;
    let parent = p.parent_pid.load(core::sync::atomic::Ordering::Relaxed);
    (parent != 0).then_some(parent)
}

/// Is `b` an ancestor of `a`? (Parent pids, followed up to 64 steps.)
fn ancestor(a: u32, b: u32) -> bool {
    let mut cur = a;
    for _ in 0..64 {
        match parent_of(cur) {
            Some(p) if p == b => return true,
            Some(p) => cur = p,
            None => return false,
        }
    }
    false
}

/// `proc` acting on process `target`: itself, an ancestor or a descendant
/// needs nothing more; anything else needs `cap`.
pub fn require_on(proc: &Process, target_pid: u32, cap: Capability, what: &str) -> Result<(), NtStatus> {
    if target_pid == proc.pid || ancestor(proc.pid, target_pid) || ancestor(target_pid, proc.pid) {
        return Ok(());
    }
    // Performance Mode: every program belongs to the signed-in user, which
    // is all Windows asks.
    if crate::policy::performance() {
        return Ok(());
    }
    require(proc, cap, what)
}
