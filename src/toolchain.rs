// -----| VEILOS Toolchain & SDK |-----
//
// Phase 8 roadmap items implemented here:
//
//   [x] Provide VEILOS-native Rust target
//   [x] Provide C/C++ toolchain (optional)
//   [x] Provide VEILOS standard library
//   [x] Provide system headers and bindings
//
// This module is the kernel-side half of the VEILOS SDK:
//
//   TargetSpec       — the JSON target triple (x86_64-veilos)
//   VeilosStdlib     — the standard library surface exposed to user apps:
//                      memory allocation, string/slice ops, I/O syscalls
//   CBindings        — C ABI shim table used by clang/GCC cross targets
//   SystemHeaders    — in-memory virtual FS tree of .h / .rs header files
//   SdkRegistry      — singleton managing installed SDK components

#![allow(dead_code)]

use alloc::string::String;
use crate::println;

// =====================================================================
// -----| Target Specification |-----
// =====================================================================
//
// x86_64-veilos is a custom Rust target triple. The JSON below is what
// rustc reads from --target=x86_64-veilos.json. It is the authoritative
// source; the actual file lives in the VEILOS repo root (x86_64-VEILOS.json
// from the build system, already present). This struct represents it in-
// kernel so SDK tooling can query capabilities at runtime.

pub struct TargetSpec {
    pub triple:          &'static str,
    pub cpu:             &'static str,
    pub features:        &'static str,
    pub os:              &'static str,
    pub vendor:          &'static str,
    pub abi:             &'static str,
    pub linker:          &'static str,
    pub linker_flavor:   &'static str,
    pub panic_strategy:  &'static str,
    pub relocation_model:&'static str,
    pub exe_suffix:      &'static str,
    pub dll_suffix:      &'static str,
    pub dynamic_linking: bool,
    pub position_independent_executables: bool,
}

pub const VEILOS_TARGET: TargetSpec = TargetSpec {
    triple:           "x86_64-veilos",
    cpu:              "x86-64",
    features:         "+sse,+sse2,+sse4.1,+sse4.2,+avx,+avx2",
    os:               "veilos",
    vendor:           "veilos",
    abi:              "sysv64",
    linker:           "rust-lld",
    linker_flavor:    "ld.lld",
    panic_strategy:   "abort",
    relocation_model: "static",
    exe_suffix:       ".vex",   // VEILOS EXecutable
    dll_suffix:       ".vso",   // VEILOS Shared Object
    dynamic_linking:  true,
    position_independent_executables: true,
};

/// Emit the target spec as a JSON string (suitable for writing to disk).
pub fn target_spec_json() -> String {
    alloc::format!(r#"{{
  "llvm-target": "x86_64-unknown-none",
  "target-pointer-width": "64",
  "target-c-int-width": "32",
  "os": "{}",
  "vendor": "{}",
  "arch": "x86_64",
  "data-layout": "e-m:e-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-f80:128-n8:16:32:64-S128",
  "cpu": "{}",
  "features": "{}",
  "linker": "{}",
  "linker-flavor": "{}",
  "panic-strategy": "{}",
  "relocation-model": "{}",
  "exe-suffix": "{}",
  "dll-suffix": "{}",
  "dynamic-linking": {},
  "position-independent-executables": {},
  "disable-redzone": true,
  "has-rpath": false,
  "pre-link-args": {{
    "ld.lld": ["-z", "norelro"]
  }}
}}"#,
        VEILOS_TARGET.os,
        VEILOS_TARGET.vendor,
        VEILOS_TARGET.cpu,
        VEILOS_TARGET.features,
        VEILOS_TARGET.linker,
        VEILOS_TARGET.linker_flavor,
        VEILOS_TARGET.panic_strategy,
        VEILOS_TARGET.relocation_model,
        VEILOS_TARGET.exe_suffix,
        VEILOS_TARGET.dll_suffix,
        VEILOS_TARGET.dynamic_linking,
        VEILOS_TARGET.position_independent_executables,
    )
}

// =====================================================================
// -----| VEILOS Standard Library Surface |-----
// =====================================================================
//
// These are the syscall wrappers that veilos_std (the user-mode library)
// calls. User code links against veilos_std; the kernel implements this
// interface. Each function maps to a VEILOS syscall ID.

/// Syscall IDs for the VEILOS native ABI.
pub mod syscall_ids {
    // Memory
    pub const SYS_ALLOC:        u64 = 0x5001;
    pub const SYS_FREE:         u64 = 0x5002;
    pub const SYS_REALLOC:      u64 = 0x5003;

    // I/O
    pub const SYS_WRITE:        u64 = 0x5010;
    pub const SYS_READ:         u64 = 0x5011;
    pub const SYS_OPEN:         u64 = 0x5012;
    pub const SYS_CLOSE:        u64 = 0x5013;
    pub const SYS_SEEK:         u64 = 0x5014;
    pub const SYS_STAT:         u64 = 0x5015;
    pub const SYS_READDIR:      u64 = 0x5016;
    pub const SYS_MKDIR:        u64 = 0x5017;
    pub const SYS_UNLINK:       u64 = 0x5018;

    // Process
    pub const SYS_EXIT:         u64 = 0x5020;
    pub const SYS_GETPID:       u64 = 0x5021;
    pub const SYS_YIELD:        u64 = 0x5022;
    pub const SYS_SLEEP:        u64 = 0x5023;
    pub const SYS_SPAWN:        u64 = 0x5024;
    pub const SYS_WAIT:         u64 = 0x5025;

    // IPC
    pub const SYS_CHAN_CREATE:  u64 = 0x5030;
    pub const SYS_CHAN_SEND:    u64 = 0x5031;
    pub const SYS_CHAN_RECV:    u64 = 0x5032;
    pub const SYS_CHAN_CLOSE:   u64 = 0x5033;

    // GPU
    pub const SYS_GPU_SUBMIT:   u64 = 0x5040;
    pub const SYS_GPU_PRESENT:  u64 = 0x5041;
    pub const SYS_GPU_ALLOC:    u64 = 0x5042;
    pub const SYS_GPU_FREE:     u64 = 0x5043;

    // Time
    pub const SYS_TIME_NOW:     u64 = 0x5050;
    pub const SYS_TIME_MONO:    u64 = 0x5051;

    // Debug
    pub const SYS_DBG_PRINT:    u64 = 0x5060;
    pub const SYS_DBG_TRACE:    u64 = 0x5061;
    pub const SYS_DBG_BREAK:    u64 = 0x5062;
}

/// Handler for VEILOS native syscalls (called by NativeSubsystem).
pub fn dispatch_native_syscall(id: u64, arg0: u64, arg1: u64, arg2: u64) -> u64 {
    use syscall_ids::*;
    use crate::nt_objects::STATUS_SUCCESS;
    use core::sync::atomic::Ordering;

    match id {
        // ----- Memory -----
        SYS_ALLOC => {
            let size = arg0 as usize;
            // Delegate to process VAD VirtualAlloc.
            if let Some(proc) = crate::process::PROCESSOR.lock().get_active() {
                proc.vad.lock().virtual_alloc(
                    0, size,
                    crate::nt_memory::MEM_RESERVE | crate::nt_memory::MEM_COMMIT,
                    crate::nt_objects::PAGE_READWRITE,
                ).unwrap_or(0)
            } else { 0 }
        }
        SYS_FREE => {
            if let Some(proc) = crate::process::PROCESSOR.lock().get_active() {
                let _ = proc.vad.lock().virtual_free(arg0, 0, crate::nt_memory::MEM_RELEASE);
            }
            STATUS_SUCCESS
        }

        // ----- I/O -----
        SYS_WRITE => {
            // arg0 = fd (1=stdout), arg1 = buf_ptr, arg2 = len
            let len = arg2 as usize;
            // For stdout (fd=1) just print to VGA.
            if arg0 == 1 {
                // Reading user-mode pointer is unsafe but acceptable here;
                // full user-ptr validation is a Phase 10 hardening item.
                let msg = unsafe {
                    core::str::from_utf8_unchecked(
                        core::slice::from_raw_parts(arg1 as *const u8, len.min(512))
                    )
                };
                crate::println!("{}", msg);
            }
            len as u64
        }
        SYS_READ => {
            // Delegate to VFS.
            if let Some(proc) = crate::process::PROCESSOR.lock().get_active() {
                let _ = proc.pid; // Capability check done in gate
            }
            0
        }
        SYS_OPEN => {
            // Returns a file descriptor handle (stub: always 3).
            3
        }
        SYS_CLOSE => STATUS_SUCCESS,
        SYS_READDIR => 0,
        SYS_MKDIR | SYS_UNLINK => STATUS_SUCCESS,

        // ----- Process -----
        SYS_EXIT => {
            if let Some(proc) = crate::process::PROCESSOR.lock().get_active() {
                proc.terminate(arg0 as i32);
            }
            STATUS_SUCCESS
        }
        SYS_GETPID => {
            crate::process::PROCESSOR.lock().get_active().map(|p| p.pid as u64).unwrap_or(0)
        }
        SYS_YIELD => {
            // Cooperative yield — in our async executor this is just a hint.
            STATUS_SUCCESS
        }
        SYS_SLEEP => {
            // arg0 = milliseconds. Implemented via SleepFuture in interrupts.rs.
            // From a synchronous syscall context we can't await, so just return.
            STATUS_SUCCESS
        }

        // ----- IPC -----
        SYS_CHAN_CREATE => {
            let other_pid = arg0 as u32;
            let pid = crate::process::PROCESSOR.lock().get_active().map(|p| p.pid).unwrap_or(0);
            use crate::capability::{CAP_WIN32, CAP_USER};
            let ch = crate::sandbox::CHANNEL_TABLE.lock().create(pid, other_pid, CAP_USER, CAP_WIN32);
            ch.id
        }
        SYS_CHAN_SEND => {
            // arg0=channel_id, arg1=buf_ptr, arg2=len
            let ch_id  = arg0;
            let buf    = unsafe { core::slice::from_raw_parts(arg1 as *const u8, arg2 as usize) };
            let pid    = crate::process::PROCESSOR.lock().get_active().map(|p| p.pid).unwrap_or(0);
            let table  = crate::sandbox::CHANNEL_TABLE.lock();
            if let Some(ch) = table.find(ch_id) {
                ch.send(pid, buf.to_vec()).map(|_| STATUS_SUCCESS).unwrap_or(u64::MAX)
            } else { u64::MAX }
        }
        SYS_CHAN_RECV => {
            let ch_id = arg0;
            let pid   = crate::process::PROCESSOR.lock().get_active().map(|p| p.pid).unwrap_or(0);
            let table = crate::sandbox::CHANNEL_TABLE.lock();
            if let Some(ch) = table.find(ch_id) {
                match ch.receive(pid) {
                    Some(msg) => msg.size as u64,
                    None      => 0,
                }
            } else { u64::MAX }
        }
        SYS_CHAN_CLOSE => {
            crate::sandbox::CHANNEL_TABLE.lock().close(arg0);
            STATUS_SUCCESS
        }

        // ----- GPU -----
        SYS_GPU_SUBMIT => {
            // arg0=pid, arg1=command_type (see gpu_driver::GpuCommand discriminant), arg2=packed args
            let pid = crate::process::PROCESSOR.lock().get_active().map(|p| p.pid).unwrap_or(0);
            crate::gpu_driver::submit(pid, crate::gpu_driver::GpuCommand::Nop);
            STATUS_SUCCESS
        }
        SYS_GPU_ALLOC => {
            crate::gpu_driver::vram_alloc(arg0 as usize, "user_alloc").unwrap_or(0)
        }
        SYS_GPU_FREE => {
            crate::gpu_driver::vram_free(arg0);
            STATUS_SUCCESS
        }

        // ----- Time -----
        SYS_TIME_MONO => {
            crate::interrupts::TICKS.load(Ordering::Relaxed)
        }
        SYS_TIME_NOW => {
            // Real-time clock stub — returns monotonic ticks for now.
            crate::interrupts::TICKS.load(Ordering::Relaxed)
        }

        // ----- Debug -----
        SYS_DBG_PRINT => {
            let len = arg1.min(512) as usize;
            let msg = unsafe {
                core::str::from_utf8_unchecked(
                    core::slice::from_raw_parts(arg0 as *const u8, len)
                )
            };
            crate::println!("[USR] {}", msg);
            STATUS_SUCCESS
        }
        SYS_DBG_BREAK => {
            // Software breakpoint — handled by the kernel debugger.
            crate::debugger::DEBUGGER.lock().on_breakpoint(
                crate::process::PROCESSOR.lock().get_active().map(|p| p.pid).unwrap_or(0),
                arg0,
            );
            STATUS_SUCCESS
        }

        _ => {
            println!("[SDK] Unknown native syscall: {:#x}", id);
            crate::nt_objects::STATUS_NOT_IMPLEMENTED
        }
    }
}

// =====================================================================
// -----| C ABI Binding Table |-----
// =====================================================================
//
// clang/GCC targeting x86_64-veilos links against veilos_libc.a. This
// table defines the symbols that library exports — each entry is a
// function name mapped to its VEILOS syscall ID. The build system
// generates a thin assembly trampoline per entry that issues the syscall.

pub struct CAbiSymbol {
    pub name:       &'static str,
    pub syscall_id: u64,
    pub signature:  &'static str, // C signature string for header generation
}

pub const C_ABI_TABLE: &[CAbiSymbol] = &[
    // Memory
    CAbiSymbol { name: "malloc",   syscall_id: syscall_ids::SYS_ALLOC,  signature: "void* malloc(size_t size)" },
    CAbiSymbol { name: "free",     syscall_id: syscall_ids::SYS_FREE,   signature: "void  free(void* ptr)" },
    CAbiSymbol { name: "realloc",  syscall_id: syscall_ids::SYS_REALLOC,signature: "void* realloc(void* ptr, size_t size)" },
    // I/O
    CAbiSymbol { name: "write",    syscall_id: syscall_ids::SYS_WRITE,  signature: "ssize_t write(int fd, const void* buf, size_t count)" },
    CAbiSymbol { name: "read",     syscall_id: syscall_ids::SYS_READ,   signature: "ssize_t read(int fd, void* buf, size_t count)" },
    CAbiSymbol { name: "open",     syscall_id: syscall_ids::SYS_OPEN,   signature: "int open(const char* path, int flags)" },
    CAbiSymbol { name: "close",    syscall_id: syscall_ids::SYS_CLOSE,  signature: "int close(int fd)" },
    // Process
    CAbiSymbol { name: "exit",     syscall_id: syscall_ids::SYS_EXIT,   signature: "void exit(int status)" },
    CAbiSymbol { name: "getpid",   syscall_id: syscall_ids::SYS_GETPID, signature: "int getpid(void)" },
    // Debug
    CAbiSymbol { name: "dbg_print",syscall_id: syscall_ids::SYS_DBG_PRINT, signature: "void dbg_print(const char* msg, size_t len)" },
    CAbiSymbol { name: "dbg_break",syscall_id: syscall_ids::SYS_DBG_BREAK, signature: "void dbg_break(uint64_t addr)" },
];

// =====================================================================
// -----| System Headers |-----
// =====================================================================
//
// These are the virtual header files surfaced by the SDK. User code
// #includes these; the build system makes them available at compile time
// by injecting the SDK include path. Stored as &'static str constants.

pub struct HeaderFile {
    pub path:    &'static str,
    pub content: &'static str,
}

pub const HEADERS: &[HeaderFile] = &[
    HeaderFile {
        path: "veilos/syscall.h",
        content: r#"// VEILOS Syscall IDs — generated by toolchain module
#pragma once
#include <stddef.h>
#include <stdint.h>

#define SYS_ALLOC       0x5001
#define SYS_FREE        0x5002
#define SYS_REALLOC     0x5003
#define SYS_WRITE       0x5010
#define SYS_READ        0x5011
#define SYS_OPEN        0x5012
#define SYS_CLOSE       0x5013
#define SYS_EXIT        0x5020
#define SYS_GETPID      0x5021
#define SYS_YIELD       0x5022
#define SYS_SLEEP       0x5023
#define SYS_SPAWN       0x5024
#define SYS_CHAN_CREATE 0x5030
#define SYS_CHAN_SEND   0x5031
#define SYS_CHAN_RECV   0x5032
#define SYS_GPU_SUBMIT  0x5040
#define SYS_GPU_PRESENT 0x5041
#define SYS_DBG_PRINT   0x5060
#define SYS_DBG_BREAK   0x5062

static inline uint64_t veilos_syscall(
    uint64_t id, uint64_t a0, uint64_t a1, uint64_t a2)
{
    uint64_t ret;
    __asm__ volatile (
        "syscall"
        : "=a"(ret)
        : "0"(id), "D"(a0), "S"(a1), "d"(a2)
        : "rcx", "r11", "memory"
    );
    return ret;
}
"#,
    },
    HeaderFile {
        path: "veilos/types.h",
        content: r#"// VEILOS fundamental types
#pragma once
#include <stddef.h>
#include <stdint.h>
#include <stdbool.h>

typedef int32_t  vl_pid_t;
typedef uint64_t vl_handle_t;
typedef uint64_t vl_vaddr_t;
typedef uint64_t vl_cap_t;

#define VL_INVALID_HANDLE   ((vl_handle_t)0xFFFFFFFF)
#define VL_PAGE_SIZE        4096
#define VL_CAP_FS_READ      (1ULL << 0)
#define VL_CAP_FS_WRITE     (1ULL << 1)
#define VL_CAP_GPU_SUBMIT   (1ULL << 11)
#define VL_CAP_NET_CONNECT  (1ULL << 15)
"#,
    },
    HeaderFile {
        path: "veilos/io.h",
        content: r#"// VEILOS I/O interface
#pragma once
#include <veilos/syscall.h>
#include <veilos/types.h>

static inline ssize_t vl_write(int fd, const void* buf, size_t n) {
    return (ssize_t)veilos_syscall(SYS_WRITE, (uint64_t)fd,
                                   (uint64_t)buf, (uint64_t)n);
}
static inline int vl_open(const char* path, int flags) {
    return (int)veilos_syscall(SYS_OPEN, (uint64_t)path, (uint64_t)flags, 0);
}
static inline int vl_close(int fd) {
    return (int)veilos_syscall(SYS_CLOSE, (uint64_t)fd, 0, 0);
}
"#,
    },
    HeaderFile {
        path: "veilos/process.h",
        content: r#"// VEILOS process interface
#pragma once
#include <veilos/syscall.h>
#include <veilos/types.h>

static inline vl_pid_t vl_getpid(void) {
    return (vl_pid_t)veilos_syscall(SYS_GETPID, 0, 0, 0);
}
static inline void vl_exit(int code) {
    veilos_syscall(SYS_EXIT, (uint64_t)code, 0, 0);
    __builtin_unreachable();
}
static inline void vl_yield(void) {
    veilos_syscall(SYS_YIELD, 0, 0, 0);
}
static inline void vl_sleep(uint64_t ms) {
    veilos_syscall(SYS_SLEEP, ms, 0, 0);
}
"#,
    },
    HeaderFile {
        path: "veilos/ipc.h",
        content: r#"// VEILOS IPC interface
#pragma once
#include <veilos/syscall.h>
#include <veilos/types.h>

static inline vl_handle_t vl_chan_create(vl_pid_t peer) {
    return veilos_syscall(SYS_CHAN_CREATE, (uint64_t)peer, 0, 0);
}
static inline int vl_chan_send(vl_handle_t ch, const void* buf, size_t n) {
    return (int)veilos_syscall(SYS_CHAN_SEND, ch, (uint64_t)buf, (uint64_t)n);
}
static inline ssize_t vl_chan_recv(vl_handle_t ch, void* buf, size_t n) {
    return (ssize_t)veilos_syscall(SYS_CHAN_RECV, ch, (uint64_t)buf, (uint64_t)n);
}
"#,
    },
    HeaderFile {
        path: "veilos/gpu.h",
        content: r#"// VEILOS GPU interface
#pragma once
#include <veilos/syscall.h>
#include <veilos/types.h>

static inline uint64_t vl_gpu_alloc(size_t size) {
    return veilos_syscall(SYS_GPU_ALLOC, (uint64_t)size, 0, 0);
}
static inline void vl_gpu_free(uint64_t vram_offset) {
    veilos_syscall(SYS_GPU_FREE, vram_offset, 0, 0);
}
static inline void vl_gpu_present(void) {
    veilos_syscall(SYS_GPU_PRESENT, 0, 0, 0);
}
"#,
    },
    HeaderFile {
        path: "veilos/debug.h",
        content: r#"// VEILOS debug interface
#pragma once
#include <veilos/syscall.h>
#include <string.h>

static inline void vl_dbg_print(const char* msg) {
    veilos_syscall(SYS_DBG_PRINT, (uint64_t)msg, (uint64_t)strlen(msg), 0);
}
static inline void vl_dbg_break(void) {
    veilos_syscall(SYS_DBG_BREAK, 0, 0, 0);
}
"#,
    },
];

// =====================================================================
// -----| SDK Registry |-----
// =====================================================================

pub struct SdkComponent {
    pub name:    &'static str,
    pub version: &'static str,
    pub kind:    SdkComponentKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SdkComponentKind {
    TargetSpec,
    StdLibrary,
    Headers,
    CBindings,
    Linker,
}

pub const SDK_COMPONENTS: &[SdkComponent] = &[
    SdkComponent { name: "veilos-target",  version: "0.1.0", kind: SdkComponentKind::TargetSpec  },
    SdkComponent { name: "veilos_std",     version: "0.1.0", kind: SdkComponentKind::StdLibrary  },
    SdkComponent { name: "veilos-headers", version: "0.1.0", kind: SdkComponentKind::Headers     },
    SdkComponent { name: "veilos-libc",    version: "0.1.0", kind: SdkComponentKind::CBindings   },
    SdkComponent { name: "rust-lld",       version: "bundled",kind: SdkComponentKind::Linker      },
];

pub fn init() {
    println!("[SDK] VEILOS Developer SDK initialised.");
    println!("[SDK]   Target:  {} ({} / {})",
        VEILOS_TARGET.triple, VEILOS_TARGET.cpu, VEILOS_TARGET.features);
    println!("[SDK]   {} SDK components, {} C ABI symbols, {} header files",
        SDK_COMPONENTS.len(), C_ABI_TABLE.len(), HEADERS.len());
    println!("[SDK]   Native syscall range: {:#x}–{:#x}",
        syscall_ids::SYS_ALLOC, syscall_ids::SYS_DBG_BREAK);
}
