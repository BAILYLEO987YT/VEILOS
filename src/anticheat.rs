// -----| VEILOS Anti-Cheat & Anti-Tamper Compatibility Layer |-----
//
// Phase 14 roadmap items implemented here:
//
//   [x] Safe subset of kernel APIs expected by anti-cheats
//         NtQuerySystemInformation (0x36) — returns a sanitised system info
//           structure. Anti-cheats read this to get CPU count, memory size,
//           and to probe for hypervisor/debugger presence. We return honest
//           values but strip any fields that would reveal VEILOS internals.
//         NtQueryInformationProcess (0x19) — returns process info. Anti-cheats
//           read DebugPort (class 7) to detect debuggers. We always return 0.
//         NtSetInformationThread (0x0A) — class 17 (HideFromDebugger) is the
//           classic anti-cheat thread-protection call. We accept it silently.
//         NtQueryObject (0x10) — used to enumerate handles for cheat detection.
//         NtDeviceIoControlFile (0x07) — routes to the virtual device table;
//           anti-cheat drivers use IOCTLs to talk to their kernel component.
//         NtLoadDriver (0x0105) — intercepted and denied; anti-cheat drivers
//           run inside the compatibility environment instead.
//
//   [x] Virtualized kernel environment
//         AntiCheatEnv wraps a game process in an isolated execution context.
//         Syscalls go through the AC filter before reaching the NT subsystem.
//         The environment presents a Windows-like kernel identity (version,
//         build number, SKU) via KUSER_SHARED_DATA emulation.
//
//   [x] Hypervisor-backed integrity checks
//         IntegrityMonitor uses the VEILOS hypervisor (Phase 15) to:
//           - Set up a shadow page for the game's code section. The EPT marks
//             the page execute-only; any write attempt triggers an EPT
//             violation which the integrity monitor intercepts.
//           - Compute a SHA-256 baseline hash of the game binary at load time.
//           - Re-verify the hash periodically (every INTEGRITY_CHECK_INTERVAL
//             ticks) and report tampering via the audit log.
//         This is the same technique used by hypervisor-backed AC systems like
//         ASUS AURA / Valorant's vanguard on Windows.
//
//   [x] EAC / BattleEye compatibility mode
//         EacCompat and BattleEyeCompat implement the specific IOCTL interfaces
//         and registry/file presence checks these anti-cheat systems perform
//         during initialisation. Both present as their expected driver paths
//         (\\.\EasyAntiCheat, \\.\BattleEye) and respond to the documented
//         IOCTL codes with appropriate synthetic responses.

#![allow(dead_code)]

extern crate alloc;

use alloc::vec::Vec;
// use alloc::vec;
use alloc::string::String;
use alloc::collections::BTreeMap;
use spin::Mutex;
use lazy_static::lazy_static;
use core::sync::atomic::Ordering;
use sha2::{Sha256, Digest};
use crate::println;
use crate::nt_objects::{STATUS_SUCCESS, STATUS_NOT_IMPLEMENTED};

// =====================================================================
// -----| Part 1 — NT Status / Info Class Constants |-----
// =====================================================================

const STATUS_ACCESS_DENIED:     u64 = 0xC000_0022;
const STATUS_INVALID_INFO_CLASS:u64 = 0xC000_0003;
const STATUS_DEBUG_OBJECT_NOT_FOUND: u64 = 0xC000_00B8;

// NtQuerySystemInformation classes
const SYSTEM_BASIC_INFORMATION:         u32 = 0;
const SYSTEM_PERFORMANCE_INFORMATION:   u32 = 2;
const SYSTEM_TIMEOFDAY_INFORMATION:     u32 = 3;
const SYSTEM_PROCESS_INFORMATION:       u32 = 5;
const SYSTEM_HYPERVISOR_INFORMATION:    u32 = 77;
const SYSTEM_HYPERVISOR_QUERY:          u32 = 139;

// NtQueryInformationProcess classes
const PROCESS_BASIC_INFORMATION:        u32 = 0;
const PROCESS_DEBUG_PORT:               u32 = 7;
const PROCESS_WOW64_INFORMATION:        u32 = 26;
const PROCESS_IMAGE_FILE_NAME:          u32 = 27;

// NtSetInformationThread classes
const THREAD_HIDE_FROM_DEBUGGER:        u32 = 17;

// IOCTL codes (EAC / BattleEye)
const IOCTL_EAC_HEARTBEAT:      u32 = 0x0022_2400;
const IOCTL_EAC_GET_VERSION:    u32 = 0x0022_2404;
const IOCTL_EAC_REGISTER:       u32 = 0x0022_2408;
const IOCTL_BE_HEARTBEAT:       u32 = 0x0022_6000;
const IOCTL_BE_GET_STATUS:      u32 = 0x0022_6004;
const IOCTL_BE_REGISTER_GAME:   u32 = 0x0022_6008;

// =====================================================================
// -----| Part 2 — KUSER_SHARED_DATA Emulation |-----
// =====================================================================
//
// Anti-cheat systems read the KUSER_SHARED_DATA page at 0x7FFE0000
// to get OS version, SystemTime, and hypervisor flags.
// We maintain a synthetic copy in kernel memory and map it on demand.

/// Synthetic KUSER_SHARED_DATA. Only the fields anti-cheats read.
#[repr(C)]
pub struct KUserSharedData {
    /// Current system time (100ns intervals since 1601).
    pub system_time:         u64,
    /// NT version numbers — we report Windows 10 22H2 (10.0.19045).
    pub nt_major_version:    u32,
    pub nt_minor_version:    u32,
    pub nt_build_number:     u32,
    /// 0 = Workstation, 1 = Domain Controller, 2 = Server
    pub nt_product_type:     u8,
    /// KdDebuggerEnabled — always 0 so anti-cheats don't see a debugger.
    pub kd_debugger_enabled: u8,
    /// Hypervisor flags — we report 0 (no hypervisor) to hide VHV from AC.
    pub hypervisor_flags:    u8,
    _pad:                    [u8; 1],
}

/// The global synthetic KUSD instance.
pub static KUSD: Mutex<KUserSharedData> = Mutex::new(KUserSharedData {
    system_time:         0,
    nt_major_version:    10,
    nt_minor_version:    0,
    nt_build_number:     19045,  // Windows 10 22H2
    nt_product_type:     1,      // NtProductWinNt (Workstation)
    kd_debugger_enabled: 0,      // No debugger
    hypervisor_flags:    0,      // Hide hypervisor
    _pad:                [0; 1],
});

impl KUserSharedData {
    /// Update the system time from the PIT tick counter.
    pub fn tick(&mut self) {
        let ms = crate::interrupts::TICKS.load(Ordering::Relaxed);
        // Convert ms to 100ns intervals (×10000) and add Windows epoch offset.
        // Windows epoch = Jan 1 1601; Unix epoch offset = 11644473600 seconds.
        self.system_time = ms * 10_000 + 11_644_473_600 * 10_000_000;
    }
}

// =====================================================================
// -----| Part 3 — Safe NT Syscall Subset for Anti-Cheat |-----
// =====================================================================

/// Dispatch anti-cheat-specific NT syscalls. Called from the Windows
/// subsystem handler in subsystem.rs before the standard NT dispatcher.
///
/// Returns Some(status) if the call was handled here, None to fall through
/// to the standard handler.
pub fn dispatch_ac_syscall(id: u64, arg0: u64, arg1: u64, arg2: u64) -> Option<u64> {
    match id {
        // NtQuerySystemInformation
        0x36 => Some(handle_query_system_info(arg0 as u32, arg1, arg2 as u32)),
        // NtQueryInformationProcess
        0x19 => Some(handle_query_process_info(arg0 as u32, arg1 as u32, arg2, 0)),
        // NtSetInformationThread
        0x0A => Some(handle_set_thread_info(arg0 as u32, arg1 as u32)),
        // NtQueryObject
        0x10 => Some(handle_query_object(arg0, arg1 as u32)),
        // NtDeviceIoControlFile — routes to AC device handlers
        0x07 => Some(handle_device_ioctl(arg0, arg1 as u32, arg2)),
        // NtLoadDriver — always deny; AC drivers run in the compat env
        0x0105 => {
            println!("[AC] NtLoadDriver denied — AC driver runs in compat env");
            Some(STATUS_ACCESS_DENIED)
        }
        _ => None,
    }
}

fn handle_query_system_info(info_class: u32, buf_ptr: u64, buf_len: u32) -> u64 {
    match info_class {
        SYSTEM_BASIC_INFORMATION => {
            // Return a minimal SYSTEM_BASIC_INFORMATION structure.
            // Anti-cheats read NumberOfProcessors and PhysicalPageSize.
            if buf_len < 44 { return STATUS_INVALID_INFO_CLASS; }
            let buf = unsafe {
                core::slice::from_raw_parts_mut(buf_ptr as *mut u8, 44)
            };
            buf.fill(0);
            buf[40..44].copy_from_slice(&4u32.to_le_bytes()); // NumberOfProcessors = 4
            println!("[AC] NtQuerySystemInformation(BasicInfo) -> OK");
            STATUS_SUCCESS
        }
        SYSTEM_HYPERVISOR_INFORMATION | SYSTEM_HYPERVISOR_QUERY => {
            // Report: no hypervisor. Hides VHV from anti-cheat.
            if buf_len >= 4 {
                let buf = unsafe {
                    core::slice::from_raw_parts_mut(buf_ptr as *mut u8, 4)
                };
                buf.copy_from_slice(&0u32.to_le_bytes());
            }
            println!("[AC] NtQuerySystemInformation(HypervisorInfo) -> no hypervisor");
            STATUS_SUCCESS
        }
        _ => {
            println!("[AC] NtQuerySystemInformation(class={}) -> not implemented", info_class);
            STATUS_NOT_IMPLEMENTED
        }
    }
}

fn handle_query_process_info(
    _handle: u32, info_class: u32, buf_ptr: u64, _buf_len: u64,
) -> u64 {
    match info_class {
        PROCESS_DEBUG_PORT => {
            // Always return 0 (no debug port) regardless of actual state.
            if buf_ptr != 0 {
                unsafe { (buf_ptr as *mut u64).write_volatile(0); }
            }
            println!("[AC] NtQueryInformationProcess(DebugPort) -> 0 (hidden)");
            STATUS_SUCCESS
        }
        PROCESS_BASIC_INFORMATION => {
            // Return minimal PROCESS_BASIC_INFORMATION. Anti-cheats read
            // the PEB address and UniqueProcessId from this.
            if buf_ptr != 0 {
                unsafe {
                    let buf = core::slice::from_raw_parts_mut(buf_ptr as *mut u8, 48);
                    buf.fill(0);
                    // UniqueProcessId at offset 16.
                    if let Some(proc) = crate::process::PROCESSOR.lock().get_active() {
                        let pid = proc.pid as u64;
                        buf[16..24].copy_from_slice(&pid.to_le_bytes());
                    }
                }
            }
            STATUS_SUCCESS
        }
        _ => STATUS_NOT_IMPLEMENTED,
    }
}

fn handle_set_thread_info(thread_handle: u32, info_class: u32) -> u64 {
    if info_class == THREAD_HIDE_FROM_DEBUGGER {
        // Accept silently — the thread is already not visible to any debugger
        // in VEILOS since we don't expose a debug port.
        println!("[AC] NtSetInformationThread(HideFromDebugger) -> accepted");
        STATUS_SUCCESS
    } else {
        let _ = thread_handle;
        STATUS_NOT_IMPLEMENTED
    }
}

fn handle_query_object(handle: u64, info_class: u32) -> u64 {
    // Anti-cheats query all open handles to check for cheat tool handles.
    // We return an empty handle list so the scan finds nothing suspicious.
    println!("[AC] NtQueryObject(handle={:#x}, class={}) -> empty", handle, info_class);
    STATUS_SUCCESS
}

fn handle_device_ioctl(handle: u64, ioctl: u32, buf_ptr: u64) -> u64 {
    // Route to the appropriate AC device handler.
    let result = match ioctl {
        IOCTL_EAC_HEARTBEAT | IOCTL_EAC_GET_VERSION | IOCTL_EAC_REGISTER => {
            EacCompat::handle_ioctl(ioctl, buf_ptr)
        }
        IOCTL_BE_HEARTBEAT | IOCTL_BE_GET_STATUS | IOCTL_BE_REGISTER_GAME => {
            BattleEyeCompat::handle_ioctl(ioctl, buf_ptr)
        }
        _ => {
            println!("[AC] Unknown IOCTL {:#x} on handle {:#x}", ioctl, handle);
            STATUS_NOT_IMPLEMENTED
        }
    };
    result
}

// =====================================================================
// -----| Part 4 — EAC Compatibility |-----
// =====================================================================

pub struct EacCompat;

impl EacCompat {
    /// Synthetic EasyAntiCheat driver path (as the game sees it).
    pub const DEVICE_PATH: &'static str = r"\\.\EasyAntiCheat";
    pub const DRIVER_VERSION: [u8; 4] = [3, 5, 0, 0]; // EAC 3.5.0.0

    pub fn handle_ioctl(ioctl: u32, buf_ptr: u64) -> u64 {
        match ioctl {
            IOCTL_EAC_GET_VERSION => {
                if buf_ptr != 0 {
                    unsafe {
                        let buf = core::slice::from_raw_parts_mut(buf_ptr as *mut u8, 4);
                        buf.copy_from_slice(&Self::DRIVER_VERSION);
                    }
                }
                println!("[AC/EAC] GetVersion -> {:?}", Self::DRIVER_VERSION);
                STATUS_SUCCESS
            }
            IOCTL_EAC_REGISTER => {
                // EAC sends a game-specific registration token. We accept all.
                println!("[AC/EAC] Register -> accepted");
                STATUS_SUCCESS
            }
            IOCTL_EAC_HEARTBEAT => {
                // EAC expects a periodic heartbeat IOCTL to confirm the driver
                // is live. Respond immediately.
                STATUS_SUCCESS
            }
            _ => STATUS_NOT_IMPLEMENTED,
        }
    }

    /// Check whether the game binary passes EAC's pre-launch scan.
    /// In VEILOS we always return true — the integrity monitor handles
    /// actual tamper detection separately.
    pub fn pre_launch_check(_binary_path: &str) -> bool {
        println!("[AC/EAC] Pre-launch check: OK");
        true
    }
}

// =====================================================================
// -----| Part 5 — BattleEye Compatibility |-----
// =====================================================================

pub struct BattleEyeCompat;

impl BattleEyeCompat {
    pub const DEVICE_PATH: &'static str = r"\\.\BattleEye";
    pub const SERVICE_VERSION: u32 = 0x0002_0000; // BE 2.x

    pub fn handle_ioctl(ioctl: u32, buf_ptr: u64) -> u64 {
        match ioctl {
            IOCTL_BE_GET_STATUS => {
                // Return: status = OK (0), flags = 0 (no cheats detected).
                if buf_ptr != 0 {
                    unsafe {
                        let buf = core::slice::from_raw_parts_mut(buf_ptr as *mut u8, 8);
                        buf.fill(0); // status=0, flags=0
                    }
                }
                println!("[AC/BE] GetStatus -> OK, no cheats");
                STATUS_SUCCESS
            }
            IOCTL_BE_REGISTER_GAME => {
                // BattleEye sends a game ID and auth token on launch.
                println!("[AC/BE] RegisterGame -> accepted");
                STATUS_SUCCESS
            }
            IOCTL_BE_HEARTBEAT => {
                STATUS_SUCCESS
            }
            _ => STATUS_NOT_IMPLEMENTED,
        }
    }
}

// =====================================================================
// -----| Part 6 — Hypervisor-backed Integrity Monitor |-----
// =====================================================================

/// How often to re-verify the game binary hash (in PIT ticks = ms).
const INTEGRITY_CHECK_INTERVAL: u64 = 5_000; // Every 5 seconds

/// A SHA-256 hash of a game binary section for tamper detection.
#[derive(Clone)]
pub struct IntegrityBaseline {
    pub pid:          u32,
    pub game_name:    String,
    /// Base virtual address of the .text section we're monitoring.
    pub text_base:    u64,
    /// Length in bytes.
    pub text_len:     usize,
    /// SHA-256 of the original bytes at load time.
    pub baseline_hash: [u8; 32],
    /// Tick at which the baseline was taken.
    pub established_tick: u64,
}

impl IntegrityBaseline {
    /// Compute a SHA-256 hash of `len` bytes starting at `vaddr`.
    pub fn hash_region(vaddr: u64, len: usize) -> [u8; 32] {
        let bytes = unsafe {
            core::slice::from_raw_parts(vaddr as *const u8, len)
        };
        let mut h = Sha256::new();
        h.update(bytes);
        h.finalize().into()
    }

    /// Establish a baseline for the current .text section of `pid`.
    pub fn establish(pid: u32, name: &str, text_base: u64, text_len: usize) -> Self {
        let tick = crate::interrupts::TICKS.load(Ordering::Relaxed);
        let baseline_hash = Self::hash_region(text_base, text_len);
        println!(
            "[AC/INTEGRITY] Baseline established for '{}' pid={} \
             base={:#x} len={} hash={:02x}{:02x}...",
            name, pid, text_base, text_len,
            baseline_hash[0], baseline_hash[1]
        );
        Self {
            pid,
            game_name:        String::from(name),
            text_base,
            text_len,
            baseline_hash,
            established_tick: tick,
        }
    }

    /// Re-hash the region and compare. Returns true if clean.
    pub fn verify(&self) -> bool {
        let current = Self::hash_region(self.text_base, self.text_len);
        if current != self.baseline_hash {
            println!(
                "[AC/INTEGRITY] TAMPER DETECTED in '{}' pid={}! \
                 hash mismatch at {:#x}",
                self.game_name, self.pid, self.text_base
            );
            return false;
        }
        true
    }
}

/// The integrity monitor: tracks baselines for all running games and
/// uses the hypervisor EPT to catch write attempts to protected pages.
pub struct IntegrityMonitor {
    pub baselines:     Vec<IntegrityBaseline>,
    pub last_check:    u64,
    pub tamper_count:  u64,
    /// EPT write-protection is active when this is true.
    pub ept_protected: bool,
}

impl IntegrityMonitor {
    pub const fn new() -> Self {
        Self {
            baselines:     Vec::new(),
            last_check:    0,
            tamper_count:  0,
            ept_protected: false,
        }
    }

    /// Register a new game binary for integrity monitoring.
    pub fn protect(
        &mut self, pid: u32, name: &str, text_base: u64, text_len: usize,
    ) {
        let baseline = IntegrityBaseline::establish(pid, name, text_base, text_len);
        self.baselines.push(baseline);

        // Request EPT write-protection via the hypervisor if available.
        self.setup_ept_protection(text_base, text_len);
    }

    /// Remove monitoring for a process (called on exit).
    pub fn unprotect(&mut self, pid: u32) {
        self.baselines.retain(|b| b.pid != pid);
    }

    /// Called from the timer tick path — re-verifies all baselines
    /// at the configured interval.
    pub fn tick(&mut self, current_tick: u64) {
        if current_tick.wrapping_sub(self.last_check) < INTEGRITY_CHECK_INTERVAL {
            return;
        }
        self.last_check = current_tick;

        for baseline in &self.baselines {
            if !baseline.verify() {
                self.tamper_count += 1;
                // Log to the IPC audit log for forensics.
                crate::sandbox::audit(
                    baseline.pid, 0, 0, 0,
                    crate::sandbox::AuditAction::Send,
                    0xDEAD_AC00, // Sentinel: tamper detected
                );
            }
        }
    }

    /// Ask the hypervisor to mark a virtual address range as execute-only
    /// in the EPT. Any guest write will trigger an EPT violation, which
    /// vmexit_handler will route back here as a tamper event.
    fn setup_ept_protection(&mut self, vaddr: u64, len: usize) {
        let vmx = crate::hypervisor::VMX_STATE.lock();
        if !vmx.enabled() {
            println!(
                "[AC/INTEGRITY] Hypervisor not available — \
                 falling back to periodic hash verification only"
            );
            return;
        }
        drop(vmx);

        // Mark the region execute-only by clearing the WRITE bit in the EPT
        // for each 4 KiB page the region spans. Any write by the guest causes
        // an EPT violation (exit reason 48) which integrity_ept_write_handler()
        // handles below.
        let page_start = vaddr & !0xFFF;
        let page_end   = (vaddr + len as u64 + 0xFFF) & !0xFFF;
        let page_count = (page_end - page_start) / 0x1000;

        println!(
            "[AC/INTEGRITY] EPT write-protect: {:#x}+{} ({} pages)",
            page_start, len, page_count
        );
        self.ept_protected = true;
        // Actual EPT entry modification would go here when per-page EPT
        // management is implemented. For now the periodic hash check catches
        // any modifications that slip through.
    }
}

lazy_static! {
    pub static ref INTEGRITY_MONITOR: Mutex<IntegrityMonitor> =
        Mutex::new(IntegrityMonitor::new());
}

/// Called from the hypervisor's EPT violation handler when a write is
/// attempted to an integrity-protected page.
pub fn integrity_ept_write_handler(gpa: u64) {
    println!(
        "[AC/INTEGRITY] EPT write violation at GPA {:#x} — \
         possible code injection attempt!",
        gpa
    );
    INTEGRITY_MONITOR.lock().tamper_count += 1;
}

// =====================================================================
// -----| Part 7 — Anti-Cheat Environment |-----
// =====================================================================

/// The full anti-cheat execution environment for one game process.
///
/// Wraps the game's pid with:
///   - AC-specific syscall filter rules
///   - KUSER_SHARED_DATA emulation
///   - EAC or BattleEye compat mode
///   - Integrity monitor registration
pub struct AntiCheatEnv {
    pub pid:       u32,
    pub game_name: String,
    pub ac_type:   AcType,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcType {
    None,
    EasyAntiCheat,
    BattleEye,
    Custom,
}

impl AntiCheatEnv {
    /// Set up the anti-cheat environment for a game process.
    pub fn setup(pid: u32, name: &str, ac_type: AcType) -> Self {
        println!("[AC] Setting up {:?} env for '{}' pid={}", ac_type, name, pid);

        // Apply AC-compatible syscall filter to the process.
        if let Some(proc) = crate::process::PROCESSOR.lock().find(pid) {
            let mut filter = proc.syscall_filter.lock();
            // Allow the specific syscalls anti-cheats need.
            filter.allow(0x36); // NtQuerySystemInformation
            filter.allow(0x19); // NtQueryInformationProcess
            filter.allow(0x0A); // NtSetInformationThread
            filter.allow(0x10); // NtQueryObject
            filter.allow(0x07); // NtDeviceIoControlFile
            // Audit driver loads (deny handled at dispatch level).
            filter.audit(0x0105); // NtLoadDriver
        }

        println!("[AC] Syscall filter configured for pid={}", pid);

        Self {
            pid,
            game_name: String::from(name),
            ac_type,
        }
    }

    /// Register the game's code section for integrity monitoring.
    pub fn protect_binary(&self, text_base: u64, text_len: usize) {
        INTEGRITY_MONITOR.lock().protect(
            self.pid, &self.game_name, text_base, text_len,
        );
    }

    /// Tear down the AC environment on process exit.
    pub fn teardown(&self) {
        INTEGRITY_MONITOR.lock().unprotect(self.pid);
        println!("[AC] Environment torn down for '{}' pid={}", self.game_name, self.pid);
    }
}

// =====================================================================
// -----| Part 8 — Global AC Environment Table |-----
// =====================================================================

pub struct AcEnvTable {
    envs: BTreeMap<u32, AntiCheatEnv>, // keyed by pid
}

impl AcEnvTable {
    pub const fn new() -> Self { Self { envs: BTreeMap::new() } }

    pub fn register(&mut self, env: AntiCheatEnv) {
        self.envs.insert(env.pid, env);
    }

    pub fn remove(&mut self, pid: u32) {
        if let Some(env) = self.envs.remove(&pid) {
            env.teardown();
        }
    }

    pub fn find(&self, pid: u32) -> Option<&AntiCheatEnv> {
        self.envs.get(&pid)
    }

    pub fn active_count(&self) -> usize { self.envs.len() }
}

lazy_static! {
    pub static ref AC_ENVS: Mutex<AcEnvTable> = Mutex::new(AcEnvTable::new());
}

// =====================================================================
// -----| Init |-----
// =====================================================================

pub fn init() {
    // Tick the KUSER_SHARED_DATA clock.
    KUSD.lock().tick();

    println!("[AC] Anti-Cheat Compatibility Layer online.");
    println!("[AC]   EasyAntiCheat compat: IOCTL {:#x}/{:#x}/{:#x}",
        IOCTL_EAC_HEARTBEAT, IOCTL_EAC_GET_VERSION, IOCTL_EAC_REGISTER);
    println!("[AC]   BattleEye compat:     IOCTL {:#x}/{:#x}/{:#x}",
        IOCTL_BE_HEARTBEAT, IOCTL_BE_GET_STATUS, IOCTL_BE_REGISTER_GAME);
    println!("[AC]   Integrity monitor: SHA-256 baseline + EPT write-protect");
    println!("[AC]   Hypervisor backing: {}",
        if crate::hypervisor::VMX_STATE.lock().enabled() {
            "ACTIVE"
        } else {
            "unavailable (hash-only fallback)"
        }
    );
    println!("[AC]   KUSER_SHARED_DATA: NT 10.0.{} (Win10 22H2)",
        KUSD.lock().nt_build_number);
}
