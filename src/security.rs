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
    audit(proc.pid, &alloc::format!("denied {} (needs {})", what, cap.name()));
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

// -----| Security Mode's audit log (S5) and system folders (S6) |-----
//
// In Security Mode, what a program does that matters later is written to
// /var/log/audit.log: programs started, connections beyond this machine,
// denials, and changes to the system folders. Lines collect in memory and are
// appended every few seconds from the main loop (`flush_audit`), so no system
// call waits on the disk for them. And /system and /etc (DLLs, users, the
// registry) can be changed only by the root account's programs, as Windows
// keeps C:\Windows from programs that aren't elevated. Performance Mode does
// neither.

const AUDIT_PATH: &str = "/var/log/audit.log";
static AUDIT: Mutex<alloc::vec::Vec<u8>> = Mutex::new(alloc::vec::Vec::new());

/// Record an event (Security Mode only).
pub fn audit(pid: u32, what: &str) {
    if crate::policy::performance() {
        return;
    }
    let line = alloc::format!("{} pid {}: {}\n", crate::time::now_ms(), pid, what);
    let mut a = AUDIT.lock();
    if a.len() < 1 << 20 {
        a.extend_from_slice(line.as_bytes());
    }
}

/// From the main loop (and at power-off with `now_ms` 0): append what has
/// collected to the log, at most every 5 s.
pub fn flush_audit(now_ms: u64) {
    static LAST: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
    use core::sync::atomic::Ordering;
    if now_ms != 0 && now_ms.wrapping_sub(LAST.load(Ordering::Relaxed)) < 5_000 {
        return;
    }
    LAST.store(now_ms, Ordering::Relaxed);
    let data = match AUDIT.try_lock() {
        Some(mut a) if !a.is_empty() => core::mem::take(&mut *a),
        _ => return,
    };
    let vfs = &crate::vfs::VFS;
    let _ = vfs.create_dir("/var");
    let _ = vfs.create_dir("/var/log");
    let ok = match vfs.open(AUDIT_PATH) {
        Ok(f) => {
            let at = f.len();
            f.write_at(at, &data).and_then(|_| f.sync()).is_ok()
        }
        Err(_) => vfs.write_file(AUDIT_PATH, &data).is_ok(),
    };
    if !ok {
        crate::serial_println!("[SECURITY] audit log not written ({} bytes)", data.len());
    }
}

/// /system or /etc, or something in them.
pub fn system_path(path: &str) -> bool {
    let p = path.to_ascii_lowercase();
    ["/system", "/etc"].iter().any(|d| p == *d || p.starts_with(&alloc::format!("{}/", d)))
}

/// Changing `path`: in Security Mode a system folder needs the root
/// account's programs (kernel.module), and the change is audited.
pub fn require_change(proc: &Process, path: &str) -> Result<(), NtStatus> {
    if crate::policy::performance() || !system_path(path) {
        return Ok(());
    }
    require(proc, Capability::KernelModule, "changing system files")?;
    audit(proc.pid, &alloc::format!("changes {}", path));
    Ok(())
}

// -----| W^X (Security Mode) |-----
//
// No memory may be writable and executable at once: allocating, protecting
// or mapping with PAGE_EXECUTE_READWRITE or PAGE_EXECUTE_WRITECOPY answers
// STATUS_DYNAMIC_CODE_BLOCKED, as Windows' Arbitrary Code Guard does. Writing
// code and then making it executable (RW, then RX) stays allowed. Programs
// that generate code at run time (JavaScript, Java and .NET runtimes,
// browsers) are exempt: a built-in list, and /etc/wx-exempt.txt (one program
// name per line; /etc changes only by root's programs). So is every program
// built on the MSYS2 or Cygwin runtime (msys-2.0.dll, cygwin1.dll): it needs
// writable code, and Git Bash, and with it claude.exe's shell tool, run on it.

const DYNAMIC_CODE_BLOCKED: NtStatus = 0xC000_0604u32 as i32;
const WX_EXEMPT: &[&str] = &[
    "claude.exe", "node.exe", "bun.exe", "deno.exe", "java.exe", "javaw.exe", "dotnet.exe",
    "firefox.exe", "chrome.exe", "msedge.exe", "electron.exe", "code.exe",
];
static WX_FILE: Mutex<Option<alloc::vec::Vec<alloc::string::String>>> = Mutex::new(None);

fn wx_exempt(name: &str) -> bool {
    if WX_EXEMPT.contains(&name) {
        return true;
    }
    let mut f = WX_FILE.lock();
    let list = f.get_or_insert_with(|| {
        let text = crate::vfs::VFS.read_file("/etc/wx-exempt.txt").unwrap_or_default();
        core::str::from_utf8(&text).unwrap_or("").lines()
            .map(|l| l.trim().to_ascii_lowercase())
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .collect()
    });
    list.iter().any(|n| n == name)
}

/// Ok unless `protect` is writable and executable, in Security Mode, for a
/// program that isn't exempt.
pub fn wx_check(proc: &Process, protect: u32) -> Result<(), NtStatus> {
    const PAGE_EXECUTE_READWRITE: u32 = 0x40;
    const PAGE_EXECUTE_WRITECOPY: u32 = 0x80;
    if !matches!(protect & 0xFF, PAGE_EXECUTE_READWRITE | PAGE_EXECUTE_WRITECOPY) || crate::policy::performance() {
        return Ok(());
    }
    let (name, cygwin) = match proc.linker.lock().as_ref() {
        Some(l) => (l.exe_name().unwrap_or_default(), l.has_module("msys-2.0.dll") || l.has_module("cygwin1.dll")),
        None => (alloc::string::String::new(), false),
    };
    if cygwin || wx_exempt(&name) {
        return Ok(());
    }
    let first = {
        let mut d = DENIED.lock();
        d.insert((proc.pid, u64::MAX))
    };
    if first {
        crate::serial_println!("[SECURITY] pid {} ({}): writable and executable memory refused (W^X)", proc.pid, name);
        audit(proc.pid, &alloc::format!("{}: writable and executable memory refused", name));
    }
    Err(DYNAMIC_CODE_BLOCKED)
}
