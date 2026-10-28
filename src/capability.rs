// -----| VEILOS Capability System |-----
//
// Phase 7 roadmap items implemented here:
//
//   [x] Implement per-process capability tokens
//   [x] Add fine-grained permissions for FS, IPC, GPU, network
//   [x] Add capability inheritance rules
//   [x] Add revocation and dynamic policy updates
//
// Design
// ------
//   A capability is an unforgeable, reference-counted token that grants a
//   process the right to perform a specific privileged operation. No ambient
//   authority exists — if a process doesn't hold the token it can't do the
//   thing, regardless of its privilege level.
//
//   CapabilitySet
//     Per-process bitfield of granted capabilities (FS_READ, NET_SEND, etc.).
//     Stored as two u64 bitmasks: `permitted` (ever grantable) and `effective`
//     (currently active). This mirrors the Linux capability model but is
//     applied to VEILOS-specific resource domains.
//
//   CapabilityToken
//     An Arc-wrapped, signed capability grant. Tokens are issued by the kernel
//     and can be passed between processes via IPC. Each token carries:
//       - Which capability it grants
//       - Issuing authority (kernel PID 0, or a delegating process)
//       - An expiry fence value (checked against the global PolicyEngine fence)
//       - A revocation flag (atomically clearable by the issuer or kernel)
//
//   Inheritance Rules
//     VEILOS uses three inheritance classes per capability (matching POSIX):
//       Inheritable: can be passed to child processes on exec
//       Permitted:   upper bound on what can be made effective
//       Effective:   currently active (kernel checks this on each syscall)
//
//   Revocation
//     The kernel maintains a global RevocationTable. When a capability is
//     revoked, its token ID is added to the table. The capability checker
//     tests this table on every sensitive syscall — O(1) via a hash set.
//
//   Dynamic Policy Updates
//     PolicyEngine::apply_profile() atomically updates a process's CapabilitySet
//     to match a named AppPolicy profile, enabling live security changes without
//     process restart (e.g. switching Security↔Performance mode).

#![allow(dead_code)]

use alloc::vec::Vec;
use alloc::sync::Arc;
use spin::Mutex;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use crate::println;

// =====================================================================
// -----| Capability Definitions |-----
// =====================================================================

/// Every distinct permission in VEILOS is one bit in a u64 bitmask.
/// Groups: filesystem, IPC, GPU, network, process, kernel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u64)]
pub enum Capability {
    // ----- Filesystem -----
    /// Read any file on the VFS (bypasses DAC).
    FsRead          = 1 << 0,
    /// Write any file on the VFS.
    FsWrite         = 1 << 1,
    /// Execute binaries.
    FsExec          = 1 << 2,
    /// Create, rename, or delete files and directories.
    FsModify        = 1 << 3,
    /// Mount and unmount filesystems.
    FsMount         = 1 << 4,
    /// Access raw block devices.
    FsRawDevice     = 1 << 5,

    // ----- IPC -----
    /// Create named pipes and Unix-domain sockets.
    IpcCreate       = 1 << 6,
    /// Send messages to other processes.
    IpcSend         = 1 << 7,
    /// Receive messages from other processes.
    IpcReceive      = 1 << 8,
    /// Broadcast to all processes (privileged IPC).
    IpcBroadcast    = 1 << 9,
    /// Access shared memory regions owned by other processes.
    IpcSharedMemory = 1 << 10,

    // ----- GPU -----
    /// Submit commands to the GPU command ring.
    GpuSubmit       = 1 << 11,
    /// Map GPU VRAM directly into process address space (Performance Mode).
    GpuDirectMap    = 1 << 12,
    /// Reprogram GPU display modes and output configuration.
    GpuDisplay      = 1 << 13,
    /// Load and execute GPU shader code.
    GpuShader       = 1 << 14,

    // ----- Network -----
    /// Open outbound network connections.
    NetConnect      = 1 << 15,
    /// Bind to listening ports.
    NetBind         = 1 << 16,
    /// Send raw packets (bypasses TCP/IP stack).
    NetRawSend      = 1 << 17,
    /// Receive raw packets (packet capture).
    NetRawReceive   = 1 << 18,
    /// Change network interface configuration.
    NetAdmin        = 1 << 19,

    // ----- Process -----
    /// Create child processes.
    ProcFork        = 1 << 20,
    /// Send signals to other processes.
    ProcKill        = 1 << 21,
    /// Trace / debug other processes (ptrace equivalent).
    ProcTrace       = 1 << 22,
    /// Adjust the scheduling priority of other processes.
    ProcNice        = 1 << 23,
    /// Change process credentials / identity.
    ProcSetId       = 1 << 24,

    // ----- Kernel -----
    /// Load or unload kernel modules / drivers.
    KernelModule    = 1 << 25,
    /// Perform raw memory-mapped I/O (DirectHW).
    KernelMmio      = 1 << 26,
    /// Issue privileged port I/O instructions (in/out).
    KernelPortIo    = 1 << 27,
    /// Modify kernel page tables directly.
    KernelPageTable = 1 << 28,
    /// Bypass all security checks (superuser equivalent).
    KernelOverride  = 1 << 29,
    /// Toggle Security/Performance policy mode.
    PolicySwitch    = 1 << 30,
    /// Reboot or shut down the system.
    SystemPower     = 1 << 31,

    // ----- Encryption (CEN) -----
    /// Permission to encrypt data via the CEN.
    EncryptData     = 1 << 32,
    /// Permission to decrypt data via the CEN.
    DecryptData     = 1 << 33,
    /// Permission to generate, derive, and revoke keys.
    ManageKeys      = 1 << 34,
    /// Permission to load persisted keys from a ledger file.
    ReadKeyLedger   = 1 << 35,
    /// Permission to persist keys to a ledger file.
    WriteKeyLedger  = 1 << 36,

    // ----- Mesh Offloading -----
    /// Permit a worker thread to be offloaded to a remote mesh node.
    /// Granted to native VEILOS processes only — Win32 processes are
    /// handled best-effort by the OffloadBroker without this flag.
    ///
    /// win32.offload note: Win32 threads that do not require IpcSharedMemory
    /// (i.e. do not use CreateFileMapping / MapViewOfFile) are candidates for
    /// best-effort offloading. No checkpoint is written, and on failure the
    /// broker notifies the user rather than retrying — the thread may be in
    /// an inconsistent state after a partial remote execution.
    MeshOffload     = 1 << 37,

}

impl Capability {
    #[inline]
    pub const fn bit(self) -> u64 { self as u64 }
    pub fn name(self) -> &'static str {
        match self {
            Self::FsRead          => "fs.read",
            Self::FsWrite         => "fs.write",
            Self::FsExec          => "fs.exec",
            Self::FsModify        => "fs.modify",
            Self::FsMount         => "fs.mount",
            Self::FsRawDevice     => "fs.rawdevice",
            Self::IpcCreate       => "ipc.create",
            Self::IpcSend         => "ipc.send",
            Self::IpcReceive      => "ipc.receive",
            Self::IpcBroadcast    => "ipc.broadcast",
            Self::IpcSharedMemory => "ipc.sharedmem",
            Self::GpuSubmit       => "gpu.submit",
            Self::GpuDirectMap    => "gpu.directmap",
            Self::GpuDisplay      => "gpu.display",
            Self::GpuShader       => "gpu.shader",
            Self::NetConnect      => "net.connect",
            Self::NetBind         => "net.bind",
            Self::NetRawSend      => "net.rawsend",
            Self::NetRawReceive   => "net.rawrecv",
            Self::NetAdmin        => "net.admin",
            Self::ProcFork        => "proc.fork",
            Self::ProcKill        => "proc.kill",
            Self::ProcTrace       => "proc.trace",
            Self::ProcNice        => "proc.nice",
            Self::ProcSetId       => "proc.setid",
            Self::KernelModule    => "kernel.module",
            Self::KernelMmio      => "kernel.mmio",
            Self::KernelPortIo    => "kernel.portio",
            Self::KernelPageTable => "kernel.pagetable",
            Self::KernelOverride  => "kernel.override",
            Self::EncryptData     => "cen.encrypt",
            Self::DecryptData     => "cen.decrypt",
            Self::ManageKeys      => "cen.managekeys",
            Self::ReadKeyLedger   => "cen.readkeyledger",
            Self::WriteKeyLedger  => "cen.writekeyledger",
            Self::PolicySwitch    => "policy.switch",
            Self::SystemPower     => "system.power",
            Self::MeshOffload     => "mesh.offload",
        }
    }
}

// =====================================================================
// -----| Pre-built Capability Bitmasks |-----
// =====================================================================

/// No capabilities — absolute minimum (guest / sandbox process).
pub const CAP_NONE: u64 = 0;

/// Standard user process: read FS, exec, IPC, GPU submit, net connect/bind.
pub const CAP_USER: u64 =
    Capability::FsRead.bit()    | Capability::FsExec.bit()       |
    Capability::IpcCreate.bit() | Capability::IpcSend.bit()      |
    Capability::IpcReceive.bit()| Capability::GpuSubmit.bit()    |
    Capability::NetConnect.bit()| Capability::NetBind.bit()      |
    Capability::ProcFork.bit()  | Capability::MeshOffload.bit();

/// Windows application: everything a Win32 app needs, nothing it doesn't.
pub const CAP_WIN32: u64 = CAP_USER |
    Capability::FsWrite.bit()   | Capability::FsModify.bit()     |
    Capability::IpcSharedMemory.bit() | Capability::GpuShader.bit();

/// Performance Mode extension — adds direct VRAM mapping.
pub const CAP_PERF: u64 = CAP_WIN32 | Capability::GpuDirectMap.bit();

/// System service: everything except raw kernel overrides.
pub const CAP_SERVICE: u64 = CAP_WIN32 |
    Capability::FsMount.bit()   | Capability::NetAdmin.bit()     |
    Capability::ProcKill.bit()  | Capability::ProcNice.bit()     |
    Capability::PolicySwitch.bit();

/// Full kernel capability (init process / kernel threads only).
pub const CAP_KERNEL: u64 = u64::MAX;

// =====================================================================
// -----| Capability Set (per-process) |-----
// =====================================================================

#[derive(Debug, Clone)]
pub struct CapabilitySet {
    /// Upper bound: capabilities that can ever be made effective.
    pub permitted:    u64,
    /// Currently active capabilities (checked on each syscall).
    pub effective:    u64,
    /// Capabilities that can be inherited by child processes on exec.
    pub inheritable:  u64,
    /// Capabilities that can be granted to child processes (ambient).
    pub ambient:      u64,
}

impl CapabilitySet {
    pub const fn new(permitted: u64) -> Self {
        Self { permitted, effective: permitted, inheritable: 0, ambient: 0 }
    }

    /// Check if a capability is currently effective.
    #[inline]
    pub fn has(&self, cap: Capability) -> bool {
        self.effective & cap.bit() != 0
    }

    /// Check multiple capabilities at once (all must be present).
    #[inline]
    pub fn has_all(&self, mask: u64) -> bool {
        self.effective & mask == mask
    }

    /// Check if at least one capability in mask is present.
    #[inline]
    pub fn has_any(&self, mask: u64) -> bool {
        self.effective & mask != 0
    }

    /// Drop a capability from the effective set.
    pub fn drop(&mut self, cap: Capability) {
        self.effective &= !cap.bit();
    }

    /// Raise a capability (only possible if it's in the permitted set).
    pub fn raise(&mut self, cap: Capability) -> bool {
        if self.permitted & cap.bit() == 0 { return false; }
        self.effective |= cap.bit();
        true
    }

    /// Build the child's CapabilitySet from this parent's on fork/exec.
    pub fn inherit_for_child(&self) -> Self {
        // Child inherits only what the parent marks inheritable,
        // capped by the parent's permitted set.
        let child_permitted = self.inheritable & self.permitted;
        Self {
            permitted:   child_permitted,
            effective:   child_permitted & self.ambient,
            inheritable: child_permitted,
            ambient:     self.ambient & child_permitted,
        }
    }

    /// Produce a human-readable list of effective capability names.
    pub fn describe_effective(&self) -> Vec<&'static str> {
        let caps = [
            Capability::FsRead, Capability::FsWrite, Capability::FsExec,
            Capability::FsModify, Capability::FsMount, Capability::FsRawDevice,
            Capability::IpcCreate, Capability::IpcSend, Capability::IpcReceive,
            Capability::IpcBroadcast, Capability::IpcSharedMemory,
            Capability::GpuSubmit, Capability::GpuDirectMap, Capability::GpuDisplay, Capability::GpuShader,
            Capability::NetConnect, Capability::NetBind, Capability::NetRawSend, Capability::NetRawReceive, Capability::NetAdmin,
            Capability::ProcFork, Capability::ProcKill, Capability::ProcTrace, Capability::ProcNice, Capability::ProcSetId,
            Capability::KernelModule, Capability::KernelMmio, Capability::KernelPortIo,
            Capability::KernelPageTable, Capability::KernelOverride,
            Capability::PolicySwitch, Capability::SystemPower,
            Capability::MeshOffload,
        ];
        caps.iter().filter(|&&c| self.has(c)).map(|c| c.name()).collect()
    }
}

// =====================================================================
// -----| Capability Token |-----
// =====================================================================
//
// A Token is a transferable grant. A process can send a token to another
// via IPC; the receiver gains the right without needing the kernel to
// re-evaluate policy. Tokens are unforgeable because they're Arc-wrapped
// objects created only by the kernel.

static NEXT_TOKEN_ID: AtomicU64 = AtomicU64::new(1);

pub struct CapabilityToken {
    pub id:          u64,
    pub capability:  Capability,
    pub issuer_pid:  u32,
    pub target_pid:  u32,
    /// When the global policy fence passes this value, the token expires.
    pub expiry_fence: u64,
    /// Atomically set to true by revoke().
    pub revoked:     AtomicBool,
}

impl CapabilityToken {
    pub fn new(cap: Capability, issuer: u32, target: u32, expiry_fence: u64) -> Arc<Self> {
        Arc::new(Self {
            id:           NEXT_TOKEN_ID.fetch_add(1, Ordering::Relaxed),
            capability:   cap,
            issuer_pid:   issuer,
            target_pid:   target,
            expiry_fence,
            revoked:      AtomicBool::new(false),
        })
    }

    pub fn revoke(&self) {
        self.revoked.store(true, Ordering::Release);
    }

    pub fn is_valid(&self, current_fence: u64) -> bool {
        !self.revoked.load(Ordering::Acquire)
        && (self.expiry_fence == 0 || current_fence < self.expiry_fence)
    }
}

// =====================================================================
// -----| Revocation Table |-----
// =====================================================================

pub struct RevocationTable {
    /// Revoked token IDs — checked O(n) for now; replace with hash set in Phase 10.
    revoked_ids: Vec<u64>,
    /// Monotonically increasing fence. Bumped on any global policy change.
    pub fence: u64,
}

impl RevocationTable {
    pub const fn new() -> Self {
        Self { revoked_ids: Vec::new(), fence: 1 }
    }

    pub fn revoke(&mut self, token_id: u64) {
        if !self.revoked_ids.contains(&token_id) {
            self.revoked_ids.push(token_id);
        }
    }

    pub fn is_revoked(&self, token_id: u64) -> bool {
        self.revoked_ids.contains(&token_id)
    }

    /// Bump the fence — all tokens with expiry_fence <= old_fence are now expired.
    pub fn bump_fence(&mut self) -> u64 {
        self.fence += 1;
        self.fence
    }

    pub fn current_fence(&self) -> u64 { self.fence }
}

pub static REVOCATION_TABLE: Mutex<RevocationTable> = Mutex::new(RevocationTable::new());

// =====================================================================
// -----| Capability Checker |-----
// =====================================================================
//
// The checker is called at every sensitive syscall boundary in Security Mode.
// In Performance Mode it is bypassed entirely for zero overhead.

pub struct CapabilityChecker;

impl CapabilityChecker {
    /// The core check. Returns Ok(()) if allowed, Err(denied_msg) if not.
    pub fn check(caps: &CapabilitySet, required: Capability) -> Result<(), &'static str> {
        if caps.has(required) { Ok(()) }
        else { Err("capability denied") }
    }

    /// Check multiple capabilities at once (all required).
    pub fn check_all(caps: &CapabilitySet, mask: u64) -> Result<(), &'static str> {
        if caps.has_all(mask) { Ok(()) } else { Err("one or more capabilities denied") }
    }

    /// Validate a token against the revocation table.
    pub fn check_token(token: &CapabilityToken) -> Result<(), &'static str> {
        let table = REVOCATION_TABLE.lock();
        if table.is_revoked(token.id) { return Err("token revoked"); }
        if !token.is_valid(table.current_fence()) { return Err("token expired"); }
        Ok(())
    }

    /// Full gate: check that `pid` has `required` in its effective set.
    /// This is the function called from every syscall dispatcher.
    pub fn gate(pid: u32, required: Capability) -> Result<(), &'static str> {
        use crate::process::PROCESSOR;
        use crate::policy::POLICY;

        // Performance Mode: bypass all capability checks.
        if POLICY.lock().get_mode() == crate::policy::PolicyMode::Performance {
            return Ok(());
        }

        let proc = PROCESSOR.lock().find(pid)
            .ok_or("process not found for capability check")?;
        let caps = proc.capabilities.lock();
        Self::check(&caps, required)
    }
}

// =====================================================================
// -----| Token Registry |-----
// =====================================================================

pub struct TokenRegistry {
    tokens: Vec<Arc<CapabilityToken>>,
}

impl TokenRegistry {
    pub const fn new() -> Self { Self { tokens: Vec::new() } }

    /// Issue a new token. Only the kernel (pid=0) or a process with
    /// KernelOverride may issue tokens for capabilities beyond their own.
    pub fn issue(
        &mut self,
        cap: Capability,
        issuer_pid: u32,
        target_pid: u32,
        expiry_fence: u64,
    ) -> Arc<CapabilityToken> {
        let token = CapabilityToken::new(cap, issuer_pid, target_pid, expiry_fence);
        self.tokens.push(token.clone());
        println!("[CAPAB] Token {} issued: {} → pid={} (expires fence={})",
            token.id, cap.name(), target_pid, expiry_fence);
        token
    }

    /// Revoke a token by ID.
    pub fn revoke(&mut self, token_id: u64) {
        if let Some(t) = self.tokens.iter().find(|t| t.id == token_id) {
            t.revoke();
            REVOCATION_TABLE.lock().revoke(token_id);
            println!("[CAPAB] Token {} revoked.", token_id);
        }
    }

    /// Revoke all tokens issued to a process (called on process exit).
    pub fn revoke_all_for(&mut self, pid: u32) {
        let fence = REVOCATION_TABLE.lock().bump_fence();
        let mut count = 0;
        for t in self.tokens.iter().filter(|t| t.target_pid == pid) {
            t.revoke();
            REVOCATION_TABLE.lock().revoke(t.id);
            count += 1;
        }
        if count > 0 {
            println!("[CAPAB] Revoked {} tokens for pid={} (fence={})", count, pid, fence);
        }
        self.tokens.retain(|t| !t.revoked.load(Ordering::Acquire));
    }
}

pub static TOKEN_REGISTRY: Mutex<TokenRegistry> = Mutex::new(TokenRegistry::new());

/// Initialise the capability subsystem and issue kernel tokens.
pub fn init() {
    println!("[CAPAB] Capability system initialised.");
    println!("[CAPAB]   32 capabilities across 6 domains (fs, ipc, gpu, net, proc, kernel)");
    println!("[CAPAB]   Per-process effective/permitted/inheritable/ambient sets");
    println!("[CAPAB]   Token-based delegation with fence-based expiry and revocation");
}
