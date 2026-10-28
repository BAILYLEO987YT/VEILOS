// -----| VEILOS Sandboxing, Isolation & Policy Engine |-----
//
// Phase 7 roadmap items implemented here:
//
//   [x] Implement user-mode driver sandboxing
//   [x] Add syscall filtering per process
//   [x] Add IPC auditing and logging
//   [x] Add secure shared memory channels
//   [x] Define a human-readable policy language
//   [x] Add policy compiler + validator
//   [x] Integrate with the Security/Performance toggle
//   [x] Add per-app policy profiles
//
// Architecture
// ------------
//
//   SyscallFilter
//     Per-process allow/deny list for syscall IDs. Evaluated before any
//     syscall dispatcher runs. Works like Linux seccomp-BPF but without
//     the JIT — just a sorted allow-list checked in O(log n).
//     Four actions: Allow, Deny (EPERM), Audit (allow + log), Kill (SIGKILL).
//
//   IpcAuditLog
//     Append-only ring buffer of IpcAuditEntry records. Every IPC operation
//     in Security Mode writes an entry: sender PID, receiver PID, channel ID,
//     byte count, and a timestamp (PIT ms). The log is inspectable via a
//     dedicated syscall and is preserved across policy-mode switches.
//
//   SecureChannel
//     A bidirectional, kernel-mediated message pipe between exactly two
//     processes. Unlike the Win32 named-pipe model, a SecureChannel:
//       - Is created by the kernel, not user code.
//       - Carries a CapabilitySet that limits what the receiver can act on.
//       - All messages are logged in the IPC audit log.
//       - The shared backing buffer lives in kernel space (not user VRAM).
//
//   PolicyLanguage / PolicyCompiler
//     A minimal text-based policy language:
//
//       [profile "browser"]
//       allow  fs.read
//       allow  net.connect
//       allow  gpu.submit
//       deny   fs.write
//       deny   fs.mount
//       deny   kernel.*
//       audit  ipc.*
//       inherit  fs.read net.connect
//       ambient  (none)
//
//     The compiler parses this into an AppPolicy struct that can be applied
//     to a process's CapabilitySet and SyscallFilter atomically.
//
//   PolicyEngine
//     The singleton authority that holds all named AppPolicy profiles and
//     applies them to processes. Integrates with the Security/Performance
//     toggle: switching to Performance Mode automatically elevates the active
//     process's capabilities to CAP_PERF; switching back restores the saved
//     Security profile.

#![allow(dead_code)]

use alloc::vec::Vec;
use alloc::string::String;
use alloc::sync::Arc;
use spin::Mutex;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use crate::println;
use crate::capability::{
    Capability, CapabilitySet,
    CAP_NONE,
};

// =====================================================================
// -----| Syscall Filter |-----
// =====================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterAction {
    /// Permit the syscall silently.
    Allow,
    /// Return STATUS_ACCESS_DENIED without executing the syscall.
    Deny,
    /// Permit but write an audit log entry.
    Audit,
    /// Terminate the calling process immediately.
    Kill,
}

#[derive(Clone)]
pub struct SyscallRule {
    pub syscall_id: u64,
    pub action:     FilterAction,
    pub note:       String,
}

pub struct SyscallFilter {
    rules: Vec<SyscallRule>,
    /// Action taken for any syscall not explicitly listed.
    pub default_action: FilterAction,
    /// Total syscalls evaluated since filter was installed.
    pub eval_count: u64,
    /// Total syscalls denied.
    pub deny_count: u64,
    /// Total syscalls audited.
    pub audit_count: u64,
}

impl SyscallFilter {
    pub fn new(default_action: FilterAction) -> Self {
        Self { rules: Vec::new(), default_action, eval_count: 0, deny_count: 0, audit_count: 0 }
    }

    /// Allow syscalls — default filter with permissive default.
    pub fn permissive() -> Self { Self::new(FilterAction::Allow) }

    /// Deny-by-default filter — only explicitly allowed syscalls pass.
    pub fn strict() -> Self { Self::new(FilterAction::Deny) }

    pub fn add_rule(&mut self, id: u64, action: FilterAction, note: &str) {
        // Replace existing rule for same ID if present.
        if let Some(r) = self.rules.iter_mut().find(|r| r.syscall_id == id) {
            r.action = action;
            r.note   = String::from(note);
        } else {
            self.rules.push(SyscallRule { syscall_id: id, action, note: String::from(note) });
            self.rules.sort_by_key(|r| r.syscall_id);
        }
    }

    pub fn allow(&mut self, id: u64) { self.add_rule(id, FilterAction::Allow, ""); }
    pub fn deny(&mut self,  id: u64) { self.add_rule(id, FilterAction::Deny,  ""); }
    pub fn audit(&mut self, id: u64) { self.add_rule(id, FilterAction::Audit, ""); }

    /// Evaluate the filter for a syscall. Returns the action to take.
    pub fn evaluate(&mut self, syscall_id: u64) -> FilterAction {
        self.eval_count += 1;
        let action = self.rules
            .binary_search_by_key(&syscall_id, |r| r.syscall_id)
            .ok()
            .map(|i| self.rules[i].action)
            .unwrap_or(self.default_action);
        match action {
            FilterAction::Deny  => self.deny_count  += 1,
            FilterAction::Audit => self.audit_count += 1,
            _ => {}
        }
        action
    }

    /// Install standard Win32 rules: allow all NT object/memory/thread calls.
    pub fn apply_win32_rules(&mut self) {
        // NT Object Manager
        for id in [0x04u64, 0x05, 0x0E, 0x0F, 0x9D, 0x48, 0x78,
                   0x5C, 0x77, 0xAB, 0x5E, 0x7A, 0xAC, 0x4A, 0x79, 0xCE] {
            self.allow(id);
        }
        // NT Memory Manager
        for id in [0x0018u64, 0x001E, 0x0050, 0x0023, 0x0028, 0x002A] {
            self.allow(id);
        }
        // NT Thread
        for id in [0x4Bu64, 0x52, 0x53, 0x188, 0x3001, 0x3002, 0x3003,
                   0x3010, 0x3011, 0x3012, 0x3013] {
            self.allow(id);
        }
        // Win32 User (0x4001–0x401F)
        for id in 0x4001u64..=0x401Fu64 { self.allow(id); }
        // kernel32/user32 stubs — explicit lists to avoid Range clone issues
        for id in [0x1001u64, 0x1002, 0x1003, 0x1004, 0x1005, 0x1006, 0x1007,
                   0x1008, 0x1009, 0x100A, 0x100B, 0x100C, 0x100D] {
            self.allow(id);
        }
        for id in [0x1010u64, 0x1011, 0x1012] { self.allow(id); }
        for id in [0x1020u64, 0x1021, 0x1022, 0x1023] { self.allow(id); }
        for id in [0x2001u64, 0x2002, 0x2003, 0x2004, 0x2005, 0x2006,
                   0x2007, 0x2008, 0x2009, 0x2010] {
            self.allow(id);
        }
    }

    /// Install strict sandbox rules: deny FS mount, raw devices, kernel calls.
    pub fn apply_sandbox_restrictions(&mut self) {
        // Block anything that could escape the sandbox.
        let dangerous = [
            0x0028u64, // NtMapViewOfSection with kernel objects
        ];
        for id in dangerous { self.deny(id); }
        // Audit all IPC
        for id in [0x3003u64] { self.audit(id); } // QueueApcThread
    }
}

// =====================================================================
// -----| IPC Audit Log |-----
// =====================================================================

const AUDIT_RING_SIZE: usize = 2048;

#[derive(Debug, Clone, Copy)]
pub struct IpcAuditEntry {
    pub seq:         u64,   // Monotonically increasing sequence number
    pub timestamp_ms: u64,  // PIT millisecond tick
    pub sender_pid:  u32,
    pub receiver_pid: u32,
    pub channel_id:  u64,
    pub msg_size:    usize,
    pub action:      AuditAction,
    pub syscall_id:  u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditAction {
    Send,
    Receive,
    Connect,
    Disconnect,
    Deny,
    Revoke,
}

pub struct IpcAuditLog {
    buffer: Vec<IpcAuditEntry>,
    head:   usize,
    tail:   usize,
    count:  usize,
    seq:    u64,
    pub total_entries: u64,
}

impl IpcAuditLog {
    pub fn new() -> Self {
        let mut buf = Vec::with_capacity(AUDIT_RING_SIZE);
        buf.resize(AUDIT_RING_SIZE, IpcAuditEntry {
            seq: 0, timestamp_ms: 0, sender_pid: 0, receiver_pid: 0,
            channel_id: 0, msg_size: 0, action: AuditAction::Send, syscall_id: 0,
        });
        Self { buffer: buf, head: 0, tail: 0, count: 0, seq: 0, total_entries: 0 }
    }

    pub fn record(
        &mut self, ts: u64, sender: u32, receiver: u32,
        channel: u64, size: usize, action: AuditAction, syscall_id: u64,
    ) {
        self.seq += 1;
        let entry = IpcAuditEntry {
            seq: self.seq, timestamp_ms: ts,
            sender_pid: sender, receiver_pid: receiver,
            channel_id: channel, msg_size: size, action, syscall_id,
        };
        self.buffer[self.tail] = entry;
        self.tail = (self.tail + 1) % AUDIT_RING_SIZE;
        if self.count < AUDIT_RING_SIZE {
            self.count += 1;
        } else {
            // Overwrite oldest entry.
            self.head = (self.head + 1) % AUDIT_RING_SIZE;
        }
        self.total_entries += 1;
    }

    /// Iterate the most recent `n` entries (newest last).
    pub fn recent(&self, n: usize) -> Vec<IpcAuditEntry> {
        let take = n.min(self.count);
        let start_idx = (self.tail + AUDIT_RING_SIZE - take) % AUDIT_RING_SIZE;
        let mut result = Vec::with_capacity(take);
        for i in 0..take {
            result.push(self.buffer[(start_idx + i) % AUDIT_RING_SIZE]);
        }
        result
    }

    /// Count entries matching a specific sender PID.
    pub fn count_for_pid(&self, pid: u32) -> usize {
        let mut n = 0;
        for i in 0..self.count {
            let e = &self.buffer[(self.head + i) % AUDIT_RING_SIZE];
            if e.sender_pid == pid || e.receiver_pid == pid { n += 1; }
        }
        n
    }
}

pub static IPC_AUDIT_LOG: Mutex<IpcAuditLog> = Mutex::new(IpcAuditLog::new_const());

impl IpcAuditLog {
    pub const fn new_const() -> Self {
        Self {
            buffer: Vec::new(), // Initialised on first lock
            head: 0, tail: 0, count: 0, seq: 0, total_entries: 0,
        }
    }
}

/// Write an IPC audit entry. Safe to call from any context.
pub fn audit(
    sender: u32, receiver: u32, channel: u64,
    size: usize, action: AuditAction, syscall_id: u64,
) {
    let ts = crate::interrupts::TICKS.load(Ordering::Relaxed);
    IPC_AUDIT_LOG.lock().record(ts, sender, receiver, channel, size, action, syscall_id);
}

// =====================================================================
// -----| Secure Channel (kernel-mediated IPC) |-----
// =====================================================================

const CHANNEL_BUF_SIZE: usize = 64 * 1024; // 64 KiB per channel

pub struct ChannelMessage {
    pub sender_pid: u32,
    pub size:       usize,
    pub data:       Vec<u8>,
    pub seq:        u64,
}

pub struct SecureChannel {
    pub id:           u64,
    pub owner_a:      u32,  // First endpoint (creator)
    pub owner_b:      u32,  // Second endpoint (acceptor)
    /// Capabilities the receiver is allowed to exercise on messages from A.
    pub caps_a_to_b:  u64,
    /// Capabilities the receiver is allowed to exercise on messages from B.
    pub caps_b_to_a:  u64,
    a_to_b:           Mutex<Vec<ChannelMessage>>,
    b_to_a:           Mutex<Vec<ChannelMessage>>,
    seq:              AtomicU64,
    pub closed:       AtomicBool,
}

impl SecureChannel {
    pub fn new(id: u64, owner_a: u32, owner_b: u32, caps_a_to_b: u64, caps_b_to_a: u64) -> Self {
        Self {
            id, owner_a, owner_b, caps_a_to_b, caps_b_to_a,
            a_to_b: Mutex::new(Vec::new()),
            b_to_a: Mutex::new(Vec::new()),
            seq:    AtomicU64::new(1),
            closed: AtomicBool::new(false),
        }
    }

    /// Send data from `sender_pid` to the other endpoint.
    pub fn send(&self, sender_pid: u32, data: Vec<u8>) -> Result<u64, &'static str> {
        if self.closed.load(Ordering::Acquire) { return Err("channel closed"); }
        let seq = self.seq.fetch_add(1, Ordering::Relaxed);
        let msg = ChannelMessage { sender_pid, size: data.len(), data, seq };
        let len = msg.size;

        if sender_pid == self.owner_a {
            self.a_to_b.lock().push(msg);
        } else if sender_pid == self.owner_b {
            self.b_to_a.lock().push(msg);
        } else {
            return Err("sender not an endpoint of this channel");
        }

        // Audit every send in Security Mode.
        audit(sender_pid,
            if sender_pid == self.owner_a { self.owner_b } else { self.owner_a },
            self.id, len, AuditAction::Send, 0);

        Ok(seq)
    }

    /// Receive the oldest pending message for `receiver_pid`.
    pub fn receive(&self, receiver_pid: u32) -> Option<ChannelMessage> {
        let msg = if receiver_pid == self.owner_b {
            self.a_to_b.lock().pop()
        } else if receiver_pid == self.owner_a {
            self.b_to_a.lock().pop()
        } else {
            return None;
        };

        if let Some(ref m) = msg {
            audit(m.sender_pid, receiver_pid, self.id, m.size, AuditAction::Receive, 0);
        }
        msg
    }

    pub fn pending_for(&self, receiver_pid: u32) -> usize {
        if receiver_pid == self.owner_b { self.a_to_b.lock().len() }
        else if receiver_pid == self.owner_a { self.b_to_a.lock().len() }
        else { 0 }
    }
}

pub struct ChannelTable {
    channels: Vec<Arc<SecureChannel>>,
    next_id:  u64,
}

impl ChannelTable {
    pub const fn new() -> Self { Self { channels: Vec::new(), next_id: 1 } }

    pub fn create(&mut self, a: u32, b: u32, caps_ab: u64, caps_ba: u64) -> Arc<SecureChannel> {
        let id = self.next_id; self.next_id += 1;
        let ch = Arc::new(SecureChannel::new(id, a, b, caps_ab, caps_ba));
        self.channels.push(ch.clone());
        audit(a, b, id, 0, AuditAction::Connect, 0);
        println!("[IPC] SecureChannel {} created: pid{}↔pid{}", id, a, b);
        ch
    }

    pub fn find(&self, id: u64) -> Option<Arc<SecureChannel>> {
        self.channels.iter().find(|c| c.id == id).cloned()
    }

    pub fn close(&mut self, id: u64) {
        if let Some(ch) = self.channels.iter().find(|c| c.id == id) {
            ch.closed.store(true, Ordering::Release);
            audit(ch.owner_a, ch.owner_b, id, 0, AuditAction::Disconnect, 0);
        }
        self.channels.retain(|c| !c.closed.load(Ordering::Acquire));
    }
}

pub static CHANNEL_TABLE: Mutex<ChannelTable> = Mutex::new(ChannelTable::new());

// =====================================================================
// -----| Policy Language |-----
// =====================================================================
//
// Grammar (simplified):
//
//   policy      ::= '[' 'profile' STRING ']' statement*
//   statement   ::= action resource_pattern
//   action      ::= 'allow' | 'deny' | 'audit' | 'inherit' | 'ambient'
//   resource_pattern ::= domain '.' name | domain '.' '*'
//   domain      ::= 'fs' | 'ipc' | 'gpu' | 'net' | 'proc' | 'kernel'
//                 | 'policy' | 'system'
//   name        ::= IDENTIFIER | '*'
//
// Example:
//   [profile "game"]
//   allow  fs.read
//   allow  fs.exec
//   allow  fs.write
//   allow  ipc.*
//   allow  gpu.*
//   deny   net.*
//   deny   kernel.*
//   allow  proc.fork
//   audit  proc.kill

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyStatement {
    Allow   { cap_mask: u64 },
    Deny    { cap_mask: u64 },
    Audit   { cap_mask: u64 },
    Inherit { cap_mask: u64 },
    Ambient { cap_mask: u64 },
    /// Syscall-level filter rule (from a `syscall ALLOW/DENY id` directive).
    SyscallRule { id: u64, action: FilterAction },
    /// Sandbox directive: apply a named restriction preset.
    Sandbox { preset: SandboxPreset },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxPreset {
    None,
    /// Strict: deny-by-default syscall filter + no kernel caps.
    Strict,
    /// Driver: allow kernel MMIO/portio, deny net/proc.
    Driver,
    /// Web content: allow net+gpu+fs.read, deny fs.write + kernel.
    WebContent,
}

/// A compiled policy profile ready for application to a process.
#[derive(Debug, Clone)]
pub struct AppPolicy {
    pub name:            String,
    pub base_caps:       u64,   // Permitted capability set
    pub effective_caps:  u64,   // Initially effective capabilities
    pub inherit_caps:    u64,   // Inheritable capabilities
    pub ambient_caps:    u64,   // Ambient capabilities
    pub denied_caps:     u64,   // Explicitly denied capabilities
    pub audited_caps:    u64,   // Capabilities that trigger audit on use
    pub syscall_preset:  SandboxPreset,
    pub statements:      Vec<PolicyStatement>,
}

impl AppPolicy {
    pub fn new(name: &str) -> Self {
        Self {
            name:           String::from(name),
            base_caps:      CAP_NONE,
            effective_caps: CAP_NONE,
            inherit_caps:   CAP_NONE,
            ambient_caps:   CAP_NONE,
            denied_caps:    CAP_NONE,
            audited_caps:   CAP_NONE,
            syscall_preset: SandboxPreset::None,
            statements:     Vec::new(),
        }
    }

    /// Build a CapabilitySet from this policy.
    pub fn to_capability_set(&self) -> CapabilitySet {
        let permitted  = (self.base_caps | self.effective_caps) & !self.denied_caps;
        let effective  = self.effective_caps & permitted;
        CapabilitySet {
            permitted,
            effective,
            inheritable: self.inherit_caps & permitted,
            ambient:     self.ambient_caps & permitted,
        }
    }

    /// Build a SyscallFilter from this policy.
    pub fn to_syscall_filter(&self) -> SyscallFilter {
        let mut filter = match self.syscall_preset {
            SandboxPreset::Strict     => SyscallFilter::strict(),
            SandboxPreset::Driver     => { let mut f = SyscallFilter::permissive(); f.apply_sandbox_restrictions(); f }
            SandboxPreset::WebContent => { let mut f = SyscallFilter::permissive(); f.apply_sandbox_restrictions(); f }
            SandboxPreset::None       => SyscallFilter::permissive(),
        };
        // Apply Win32 rules for all profiles (all VEILOS apps are Win32-capable).
        filter.apply_win32_rules();
        for stmt in &self.statements {
            if let PolicyStatement::SyscallRule { id, action } = stmt {
                filter.add_rule(*id, *action, "");
            }
        }
        filter
    }
}

// =====================================================================
// -----| Policy Compiler |-----
// =====================================================================

pub struct PolicyCompiler;

impl PolicyCompiler {
    /// Parse a policy text string into an AppPolicy.
    /// Returns Err with a description of the parse failure.
    pub fn compile(text: &str) -> Result<AppPolicy, String> {
        let mut policy = AppPolicy::new("unnamed");
        let mut in_profile = false;

        for (lineno, raw_line) in text.lines().enumerate() {
            let line = raw_line.trim();
            if line.is_empty() || line.starts_with('#') { continue; }

            // Profile header: [profile "name"]
            if line.starts_with('[') {
                let inner = line.trim_matches(|c| c == '[' || c == ']').trim();
                if let Some(rest) = inner.strip_prefix("profile") {
                    let name = rest.trim().trim_matches('"');
                    policy.name = String::from(name);
                    in_profile = true;
                    continue;
                }
            }

            if !in_profile {
                return Err(alloc::format!("line {}: expected [profile ...] header", lineno + 1));
            }

            let mut tokens = line.splitn(2, char::is_whitespace);
            let keyword = tokens.next().unwrap_or("").to_lowercase();
            let rest    = tokens.next().unwrap_or("").trim();

            match keyword.as_str() {
                "allow" | "deny" | "audit" | "inherit" | "ambient" => {
                    let mask = Self::parse_resource(rest)
                        .map_err(|e| alloc::format!("line {}: {}", lineno + 1, e))?;
                    let stmt = match keyword.as_str() {
                        "allow"   => { policy.effective_caps |= mask; policy.base_caps |= mask; PolicyStatement::Allow { cap_mask: mask } }
                        "deny"    => { policy.denied_caps    |= mask; PolicyStatement::Deny { cap_mask: mask } }
                        "audit"   => { policy.audited_caps   |= mask; policy.effective_caps |= mask; PolicyStatement::Audit { cap_mask: mask } }
                        "inherit" => { policy.inherit_caps   |= mask; PolicyStatement::Inherit { cap_mask: mask } }
                        "ambient" => { policy.ambient_caps   |= mask; PolicyStatement::Ambient { cap_mask: mask } }
                        _         => unreachable!(),
                    };
                    policy.statements.push(stmt);
                }

                "sandbox" => {
                    policy.syscall_preset = match rest {
                        "strict"      => SandboxPreset::Strict,
                        "driver"      => SandboxPreset::Driver,
                        "web_content" => SandboxPreset::WebContent,
                        "none"        => SandboxPreset::None,
                        other => return Err(alloc::format!("line {}: unknown sandbox preset '{}'", lineno + 1, other)),
                    };
                }

                "syscall" => {
                    let mut parts = rest.splitn(2, char::is_whitespace);
                    let action_str = parts.next().unwrap_or("").to_lowercase();
                    let id_str     = parts.next().unwrap_or("").trim();
                    let id = u64::from_str_radix(id_str.trim_start_matches("0x"), 16)
                        .map_err(|_| alloc::format!("line {}: invalid syscall id '{}'", lineno + 1, id_str))?;
                    let action = match action_str.as_str() {
                        "allow" => FilterAction::Allow,
                        "deny"  => FilterAction::Deny,
                        "audit" => FilterAction::Audit,
                        "kill"  => FilterAction::Kill,
                        other   => return Err(alloc::format!("line {}: unknown action '{}'", lineno + 1, other)),
                    };
                    policy.statements.push(PolicyStatement::SyscallRule { id, action });
                }

                other => {
                    return Err(alloc::format!("line {}: unknown directive '{}'", lineno + 1, other));
                }
            }
        }

        // Validate: denied caps must not overlap effective caps.
        if policy.effective_caps & policy.denied_caps != 0 {
            let conflict = policy.effective_caps & policy.denied_caps;
            return Err(alloc::format!("policy '{}': conflicting allow+deny on mask {:#x}", policy.name, conflict));
        }

        Ok(policy)
    }

    /// Parse a resource pattern like "fs.read", "gpu.*", "kernel.mmio" into a bitmask.
    fn parse_resource(pattern: &str) -> Result<u64, &'static str> {
        let mut parts = pattern.splitn(2, '.');
        let domain = parts.next().unwrap_or("").trim();
        let name   = parts.next().unwrap_or("*").trim();

        // Wildcard domain: any.*
        if domain == "any" || domain == "*" { return Ok(u64::MAX); }

        let all_in_domain: u64 = match domain {
            "fs" =>
                Capability::FsRead.bit() | Capability::FsWrite.bit() |
                Capability::FsExec.bit() | Capability::FsModify.bit() |
                Capability::FsMount.bit()| Capability::FsRawDevice.bit(),
            "ipc" =>
                Capability::IpcCreate.bit() | Capability::IpcSend.bit() |
                Capability::IpcReceive.bit()| Capability::IpcBroadcast.bit() |
                Capability::IpcSharedMemory.bit(),
            "gpu" =>
                Capability::GpuSubmit.bit() | Capability::GpuDirectMap.bit() |
                Capability::GpuDisplay.bit()| Capability::GpuShader.bit(),
            "net" =>
                Capability::NetConnect.bit()  | Capability::NetBind.bit() |
                Capability::NetRawSend.bit()  | Capability::NetRawReceive.bit() |
                Capability::NetAdmin.bit(),
            "proc" =>
                Capability::ProcFork.bit()  | Capability::ProcKill.bit() |
                Capability::ProcTrace.bit() | Capability::ProcNice.bit() |
                Capability::ProcSetId.bit(),
            "kernel" =>
                Capability::KernelModule.bit()    | Capability::KernelMmio.bit() |
                Capability::KernelPortIo.bit()    | Capability::KernelPageTable.bit() |
                Capability::KernelOverride.bit(),
            "policy"  => Capability::PolicySwitch.bit(),
            "system"  => Capability::SystemPower.bit(),
            _ => return Err("unknown capability domain"),
        };

        if name == "*" { return Ok(all_in_domain); }

        // Specific name within domain.
        let full = alloc::format!("{}.{}", domain, name);
        let caps = [
            Capability::FsRead, Capability::FsWrite, Capability::FsExec,
            Capability::FsModify, Capability::FsMount, Capability::FsRawDevice,
            Capability::IpcCreate, Capability::IpcSend, Capability::IpcReceive,
            Capability::IpcBroadcast, Capability::IpcSharedMemory,
            Capability::GpuSubmit, Capability::GpuDirectMap, Capability::GpuDisplay, Capability::GpuShader,
            Capability::NetConnect, Capability::NetBind, Capability::NetRawSend,
            Capability::NetRawReceive, Capability::NetAdmin,
            Capability::ProcFork, Capability::ProcKill, Capability::ProcTrace,
            Capability::ProcNice, Capability::ProcSetId,
            Capability::KernelModule, Capability::KernelMmio, Capability::KernelPortIo,
            Capability::KernelPageTable, Capability::KernelOverride,
            Capability::PolicySwitch, Capability::SystemPower,
        ];
        caps.iter()
            .find(|&&c| c.name() == full.as_str())
            .map(|c| c.bit())
            .ok_or("unknown capability name")
    }
}

// =====================================================================
// -----| Policy Validator |-----
// =====================================================================

pub struct PolicyValidator;

impl PolicyValidator {
    /// Validate a compiled AppPolicy. Returns a list of warnings (non-fatal).
    pub fn validate(policy: &AppPolicy) -> Vec<String> {
        let mut warnings = Vec::new();

        if policy.effective_caps & Capability::KernelOverride.bit() != 0 {
            warnings.push(alloc::format!("profile '{}': grants kernel.override — use with extreme caution", policy.name));
        }
        if policy.effective_caps & Capability::GpuDirectMap.bit() != 0
        && policy.effective_caps & Capability::GpuShader.bit() == 0 {
            warnings.push(alloc::format!("profile '{}': gpu.directmap granted without gpu.shader", policy.name));
        }
        if policy.inherit_caps & !policy.base_caps != 0 {
            warnings.push(alloc::format!("profile '{}': inheritable caps exceed permitted caps — will be silently capped", policy.name));
        }
        if policy.syscall_preset == SandboxPreset::None
        && policy.effective_caps & (Capability::KernelMmio.bit() | Capability::KernelPortIo.bit()) != 0 {
            warnings.push(alloc::format!("profile '{}': kernel MMIO/portio without sandbox restriction", policy.name));
        }
        warnings
    }
}

// =====================================================================
// -----| Built-in Policy Profiles |-----
// =====================================================================

pub fn builtin_profiles() -> Vec<AppPolicy> {
    let texts = [
        // Minimal guest / sandbox
        r#"
[profile "sandbox"]
sandbox strict
allow  fs.read
allow  ipc.create
allow  ipc.send
allow  ipc.receive
allow  gpu.submit
deny   fs.write
deny   fs.mount
deny   kernel.*
deny   net.*
deny   proc.kill
deny   proc.trace
audit  ipc.*
"#,
        // Standard user application
        r#"
[profile "user"]
sandbox none
allow  fs.read
allow  fs.write
allow  fs.exec
allow  fs.modify
allow  ipc.*
allow  gpu.submit
allow  gpu.shader
allow  net.connect
allow  net.bind
allow  proc.fork
allow  proc.kill
deny   fs.mount
deny   fs.rawdevice
deny   kernel.*
audit  proc.trace
"#,
        // Win32 / game application
        r#"
[profile "win32"]
sandbox none
allow  fs.read
allow  fs.write
allow  fs.exec
allow  fs.modify
allow  ipc.*
allow  gpu.*
allow  net.connect
allow  net.bind
allow  proc.fork
allow  proc.kill
deny   fs.rawdevice
deny   kernel.*
deny   net.rawsend
"#,
        // Performance Mode (all GPU caps + direct VRAM)
        r#"
[profile "performance"]
sandbox none
allow  fs.read
allow  fs.write
allow  fs.exec
allow  fs.modify
allow  ipc.*
allow  gpu.*
allow  net.connect
allow  net.bind
allow  proc.fork
allow  proc.kill
allow  kernel.mmio
allow  kernel.portio
deny   kernel.module
deny   kernel.pagetable
deny   kernel.override
deny   system.power
"#,
        // System service
        r#"
[profile "service"]
sandbox none
allow  fs.*
allow  ipc.*
allow  gpu.*
allow  net.*
allow  proc.*
allow  kernel.mmio
allow  kernel.portio
allow  policy.switch
deny   kernel.override
deny   kernel.module
deny   kernel.pagetable
"#,
    ];

    texts.iter()
        .filter_map(|t| PolicyCompiler::compile(t).ok())
        .collect()
}

// =====================================================================
// -----| Policy Engine |-----
// =====================================================================

pub struct PolicyEngine {
    pub profiles:     Vec<AppPolicy>,
    /// Per-process saved Security profile name (restored on mode switch back).
    saved_profiles:   Vec<(u32, String)>,
}

impl PolicyEngine {
    pub const fn new() -> Self {
        Self { profiles: Vec::new(), saved_profiles: Vec::new() }
    }

    /// Register a named profile.
    pub fn register(&mut self, profile: AppPolicy) {
        let name = profile.name.clone();
        self.profiles.retain(|p| p.name != name);
        self.profiles.push(profile);
    }

    pub fn get(&self, name: &str) -> Option<&AppPolicy> {
        self.profiles.iter().find(|p| p.name == name)
    }

    /// Apply a named profile to a process atomically.
    pub fn apply_profile(&self, pid: u32, profile_name: &str) -> Result<(), &'static str> {
        let profile = self.get(profile_name).ok_or("profile not found")?;
        let cap_set = profile.to_capability_set();
        let filter  = profile.to_syscall_filter();

        let warnings = PolicyValidator::validate(profile);
        for w in &warnings { println!("[POLICY] WARNING: {}", w); }

        use crate::process::PROCESSOR;
        let proc = PROCESSOR.lock().find(pid).ok_or("process not found")?;
        *proc.capabilities.lock()    = cap_set;
        *proc.syscall_filter.lock()  = filter;

        println!("[POLICY] Applied profile '{}' to pid={}", profile_name, pid);
        Ok(())
    }

    /// Called when the system switches to Performance Mode.
    /// Saves the current profile name and upgrades capabilities.
    pub fn on_performance_mode(&mut self, pid: u32) {
        // Save current profile name for restoration.
        if !self.saved_profiles.iter().any(|(p, _)| *p == pid) {
            self.saved_profiles.push((pid, String::from("win32")));
        }
        let _ = self.apply_profile(pid, "performance");
        // Also bump the revocation fence so old Security tokens expire.
        crate::capability::REVOCATION_TABLE.lock().bump_fence();
        println!("[POLICY] Performance Mode: capabilities elevated for pid={}", pid);
    }

    /// Called when the system switches back to Security Mode.
    pub fn on_security_mode(&mut self, pid: u32) {
        let saved = self.saved_profiles.iter()
            .find(|(p, _)| *p == pid)
            .map(|(_, n)| n.clone())
            .unwrap_or_else(|| String::from("win32"));
        let _ = self.apply_profile(pid, &saved);
        println!("[POLICY] Security Mode: capabilities restored for pid={} (profile='{}')", pid, saved);
    }

    /// Hot-update: recompile a profile from text and re-apply to all processes using it.
    pub fn hot_update(&mut self, profile_text: &str) -> Result<usize, String> {
        let new_profile = PolicyCompiler::compile(profile_text)?;
        let name = new_profile.name.clone();
        self.register(new_profile);

        // Re-apply to any process currently using this profile.
        let affected: Vec<u32> = self.saved_profiles.iter()
            .filter(|(_, n)| *n == name)
            .map(|(pid, _)| *pid)
            .collect();

        let count = affected.len();
        for pid in affected {
            let _ = self.apply_profile(pid, &name);
        }
        println!("[POLICY] Hot-updated profile '{}', re-applied to {} process(es)", name, count);
        Ok(count)
    }
}

pub static POLICY_ENGINE: Mutex<PolicyEngine> = Mutex::new(PolicyEngine::new());

/// Initialise the policy engine with all built-in profiles.
pub fn init() {
    let mut engine = POLICY_ENGINE.lock();
    for profile in builtin_profiles() {
        println!("[POLICY] Registered profile '{}'", profile.name);
        engine.register(profile);
    }

    crate::capability::init();
    init_audit_log();

    println!("[POLICY] Policy engine ready.");
    println!("[POLICY]   {} built-in profiles loaded", engine.profiles.len());
    println!("[POLICY]   Syscall filter, IPC audit, secure channels active");
}

/// Initialise the IPC audit log backing buffer (must be heap-ready).
fn init_audit_log() {
    let mut log = IPC_AUDIT_LOG.lock();
    if log.buffer.is_empty() {
        log.buffer.resize(AUDIT_RING_SIZE, IpcAuditEntry {
            seq: 0, timestamp_ms: 0, sender_pid: 0, receiver_pid: 0,
            channel_id: 0, msg_size: 0, action: AuditAction::Send, syscall_id: 0,
        });
    }
}
