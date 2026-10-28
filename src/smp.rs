// =====================================================================
// smp.rs — Symmetric Multi-Processing bootstrap
// =====================================================================
//
// Responsibilities:
//   1.  Discover how many logical CPUs the firmware reported (via CPUID).
//   2.  Identify the Local APIC base address and map it.
//   3.  For each Application Processor (AP, i.e. every CPU except core 0),
//       send the standard INIT → SIPI → SIPI startup sequence.
//   4.  Each AP executes `ap_entry()` which:
//         a) loads its own per-CPU GDT + TSS (unique stacks)
//         b) loads the shared IDT
//         c) enables its Local APIC and sets the timer vector
//         d) enables SYSCALL/SYSRET
//         e) parks in a lightweight run-loop ready for work
//
// ── Limitations & future work ─────────────────────────────────────────
// • AP scheduling: APs currently idle-halt.  The scheduler's SchedDomain
//   will be extended to N domains (one per CPU) in a follow-up.
// • CPU discovery: ACPI MADT is parsed for an authoritative CPU count and
//   the real APIC ID of each processor (see `discover_cpus_via_madt`).
//   CPUID leaf 0x1 EBX[23:16] is kept only as a fallback for the rare case
//   the MADT can't be found/parsed — that field is unreliable under some
//   hypervisors (notably it can reflect the host's topology rather than the
//   guest's requested `-smp N` under WHPX), which is why relying on it alone
//   previously reported "1 CPU" even when more were requested.
// • 16-bit trampoline: real hardware — and real QEMU, on every accelerator —
//   always starts an AP in 16-bit real mode after SIPI, regardless of the
//   guest CPU model string. There is no "start directly in long mode"
//   special case for `qemu64`. A previous version of this file assumed one
//   existed and pointed SIPI directly at `ap_entry` (a 64-bit function) with
//   no real-mode code ever placed at the SIPI vector — every AP jumped into
//   effectively-zeroed low memory and never came up, which is the actual
//   reason only the BSP ever appeared online. `AP_TRAMPOLINE` below is a
//   real, self-relocating 16→32→64-bit bootstrap copied to a fixed low page
//   before any SIPI is sent.

use core::sync::atomic::{AtomicU32, Ordering};
use x86_64::VirtAddr;
use x86_64::structures::paging::{
    Page, PhysFrame, Size4KiB, FrameAllocator, PageTableFlags, Mapper,
};
use x86_64::PhysAddr;

// ── LAPIC register offsets (relative to LAPIC base) ──────────────────
const LAPIC_ID:            usize = 0x020; // Local APIC ID
const LAPIC_EOI:           usize = 0x0B0; // End-of-interrupt
const LAPIC_SVR:           usize = 0x0F0; // Spurious vector register
const LAPIC_ICR_LOW:       usize = 0x300; // Interrupt Command Register (low)
const LAPIC_ICR_HIGH:      usize = 0x310; // Interrupt Command Register (high)
const LAPIC_TIMER_LVT:     usize = 0x320; // Timer LVT entry
const LAPIC_TIMER_INITIAL: usize = 0x380; // Timer initial count
const LAPIC_TIMER_CURRENT: usize = 0x390; // Timer current count
const LAPIC_TIMER_DIVIDE:  usize = 0x3E0; // Timer divide config

/// Number of logical CPUs detected at runtime.
pub static CPU_COUNT: AtomicU32 = AtomicU32::new(1);

/// LAPIC virtual base address (set once during init, read-only thereafter).
pub static LAPIC_BASE: AtomicU32 = AtomicU32::new(0);

/// Bitmask: bit N is set when CPU N has completed its AP startup sequence.
pub static CPU_ONLINE: AtomicU32 = AtomicU32::new(1); // BSP (core 0) always online

// ── Per-CPU syscall stacks ─────────────────────────────────────────────
//
// Each logical CPU gets an independent 64 KiB kernel stack for syscall
// handling.  The BSP uses STACK[0]; AP n uses STACK[n].
// This eliminates the shared-stack crash risk identified in the audit.
//
// The stacks are a static array so they are baked into the kernel image
// and have known, fixed addresses — critical for the `syscall_entry` asm
// which must compute `RSP = STACK[cpu_id].top` without dynamic allocation
// (at syscall time, the allocator lock may be held).

pub const SYSCALL_STACK_SIZE: usize = 4096 * 16;   // 64 KiB per CPU
pub const MAX_CPUS:           usize = 8;            // supports up to 8-way SMP

#[repr(C, align(16))]
pub struct PerCpuStack(pub [u8; SYSCALL_STACK_SIZE]);

/// The per-CPU syscall stacks.  Exported so `interrupts.rs` can reference
/// the array in the naked ASM (replacing the old single `SYSCALL_KERNEL_STACK`).
#[unsafe(no_mangle)]
pub static PERCPU_SYSCALL_STACKS: [PerCpuStack; MAX_CPUS] =
    [const { PerCpuStack([0u8; SYSCALL_STACK_SIZE]) }; MAX_CPUS];

/// Write a 64-bit value to `KERNEL_GS_BASE` MSR (0xC000_0102).
///
/// We use this MSR as the per-CPU syscall stack pointer storage because:
///   • it is readable from a naked asm stub with a single `rdmsr`
///   • it is architecturally per-CPU (swapped by `swapgs`)
///   • it costs zero memory-mapping complexity
///
/// Called once during init for each CPU.
pub fn set_cpu_syscall_stack(cpu_index: usize) {
    let stack_top = syscall_stack_top(cpu_index.min(MAX_CPUS - 1)) as u64;
    unsafe {
        core::arch::asm!(
            "wrmsr",
            in("ecx") 0xC0000102u32,  // MSR_KERNEL_GS_BASE
            in("eax") (stack_top & 0xFFFF_FFFF) as u32,
            in("edx") (stack_top >> 32) as u32,
        );
    }
    crate::serial_println!(
        "[SMP] CPU {} syscall stack top set to {:#018x}",
        cpu_index, stack_top
    );
}

/// Return a pointer to the **top** (high address) of the syscall stack for
/// the given CPU index.  "Top" because x86 stacks grow downward.
#[inline(always)]
pub fn syscall_stack_top(cpu_index: usize) -> *const u8 {
    let base = PERCPU_SYSCALL_STACKS[cpu_index].0.as_ptr();
    unsafe { base.add(SYSCALL_STACK_SIZE) }
}

// ── LAPIC access ──────────────────────────────────────────────────────

/// Read a 32-bit LAPIC register at `offset` bytes from the LAPIC base.
#[inline(always)]
unsafe fn lapic_read(offset: usize) -> u32 {
    let base = LAPIC_BASE.load(Ordering::Relaxed) as usize;
    unsafe { core::ptr::read_volatile((base + offset) as *const u32) }
}

/// Write a 32-bit value to a LAPIC register.
#[inline(always)]
unsafe fn lapic_write(offset: usize, val: u32) {
    let base = LAPIC_BASE.load(Ordering::Relaxed) as usize;
    unsafe { core::ptr::write_volatile((base + offset) as *mut u32, val); }
}

/// Read the APIC ID from CPUID leaf 1, EBX[31:24].
/// rbx is reserved by LLVM so we spill it manually around cpuid.
fn cpuid_apic_id() -> u32 {
    let result: u32;
    unsafe {
        core::arch::asm!(
            "push rbx",
            "mov eax, 1",
            "cpuid",
            "mov {out:e}, ebx",
            "pop rbx",
            out = out(reg) result,
            out("eax") _,
            out("ecx") _,
            out("edx") _,
            options(nostack),
        );
    }
    (result >> 24) & 0xFF
}

/// Read the current CPU's APIC ID from LAPIC register 0x20.
#[inline(always)]
pub fn current_apic_id() -> u32 {
    if LAPIC_BASE.load(Ordering::Relaxed) == 0 {
        cpuid_apic_id()
    } else {
        unsafe { lapic_read(LAPIC_ID) >> 24 }
    }
}

/// Send a write to LAPIC ICR.  Waits for the Delivery Status bit to clear
/// (i.e. the IPI has been accepted by the bus) before returning.
unsafe fn lapic_send_ipi(apic_id: u8, delivery: u32) {
    unsafe {
        lapic_write(LAPIC_ICR_HIGH, (apic_id as u32) << 24);
        lapic_write(LAPIC_ICR_LOW,  delivery);
        // Spin until Delivery Status (bit 12) clears.
        let mut timeout = 1_000_000u32;
        while (lapic_read(LAPIC_ICR_LOW) & (1 << 12)) != 0 {
            core::hint::spin_loop();
            timeout -= 1;
            if timeout == 0 {
                crate::serial_println!("[SMP] IPI delivery timeout for APIC ID {}", apic_id);
                break;
            }
        }
    }
}

// ── LAPIC discovery & mapping ─────────────────────────────────────────

/// Read the IA32_APIC_BASE MSR to get the physical base of the LAPIC.
fn read_lapic_phys_base_proper() -> u64 {
    let lo: u32;
    let hi: u32;
    unsafe {
        core::arch::asm!(
            "rdmsr",
            in("ecx") 0x1Bu32,
            out("eax") lo,
            out("edx") hi,
        );
    }
    let full = ((hi as u64) << 32) | (lo as u64);
    full & 0xFFFF_FFFF_F000u64
}

// =====================================================================
// -----| Real-mode -> protected-mode -> long-mode AP trampoline |-----
// =====================================================================
//
// Pre-assembled x86 machine code blob for the AP startup trampoline.
//
// The blob is hand-encoded rather than produced by global_asm! because
// Rust's global_asm! on Windows uses the MSVC toolchain assembler (MASM),
// which does not understand GAS/AT&T directives (.code16, .quad, .org, etc.).
// The encoded bytes are identical to what a GAS invocation would produce.
//
// Layout (all offsets relative to the copy at AP_TRAMPOLINE_PHYS = 0x8000):
//   0x000  16-bit real-mode entry (SIPI lands here)
//   0x080  GDT: null(8) + code32(8) + data32(8) + code64(8)
//   0x0A0  GDTR (6 bytes: word limit + dword base, base patched at runtime)
//   0x0B0  PM far-jump ptr (dword offset + word selector, offset patched)
//   0x0C0  parameter block (written by write_ap_params before every SIPI):
//            +0x00 cr3 (u64), +0x08 entry (u64), +0x10 stack_top (u64),
//            +0x18 cpu_index (u32), +0x1C padding (u32)
//   0x0E0  LM far-jump ptr (dword offset + word selector, offset patched)
//   0x100  32-bit protected-mode code
//   0x180  64-bit long-mode code
//
// NOTE: the real-mode far jump at 0x04A MUST carry the 0x66 operand-size prefix
// (66 67 FF AE B0 00 00 00 = ljmpl *0xB0(%esi)). Without it the CPU reads a
// m16:16 pointer, takes selector 0x0000 from the high half of the offset,
// and triple-faults (QEMU then resets the whole VM -> boot loop).
// NOTE: the 32-bit stage writes EFER with LME *and* NXE (or eax,0x900). The kernel's
// page tables (built by bootloader 0.9) mark .data/.bss/.rodata NO_EXECUTE and the
// bootloader only enables EFER.NXE on the BSP. An AP that enables paging with NXE=0
// takes a reserved-bit #PF on the first NX-marked entry it touches (its own stack),
// and with no IDT yet that triple-faults. The 64-bit stub also does `sub rsp, 8`
// before `jmp rax` so ap_entry sees the SysV-expected RSP % 16 == 8.
// NOTE: GDT descriptor byte order is limit[15:0], base[23:0], ACCESS (byte 5), FLAGS|limit[19:16]
// (byte 6), base[31:24]. i.e. flat 32-bit code = FF FF 00 00 00 9A CF 00 (NOT ... CF 9A ...).
// To regenerate: tools/assemble_trampoline.py (requires Python).
#[allow(clippy::all)]
static AP_TRAMPOLINE_BYTES: [u8; 0x200] = [
    /* 0x000 */ 0xFA, 0x31, 0xC0, 0x8E, 0xD8, 0x8E, 0xC0, 0x8E, 0xD0, 0xBC, 0xF0, 0x0F, 0x8C, 0xC8, 0x66, 0x0F,
    /* 0x010 */ 0xB7, 0xF0, 0x66, 0xC1, 0xE6, 0x04, 0x67, 0x66, 0x8D, 0x86, 0x80, 0x00, 0x00, 0x00, 0x67, 0x66,
    /* 0x020 */ 0x89, 0x86, 0xA2, 0x00, 0x00, 0x00, 0x67, 0x66, 0x8D, 0x86, 0x00, 0x01, 0x00, 0x00, 0x67, 0x66,
    /* 0x030 */ 0x89, 0x86, 0xB0, 0x00, 0x00, 0x00, 0x67, 0x0F, 0x01, 0x96, 0xA0, 0x00, 0x00, 0x00, 0x66, 0x0F,
    /* 0x040 */ 0x20, 0xC0, 0x66, 0x83, 0xC8, 0x01, 0x66, 0x0F, 0x22, 0xC0, 0x66, 0x67, 0xFF, 0xAE, 0xB0, 0x00,
    /* 0x050 */ 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    /* 0x060 */ 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    /* 0x070 */ 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    /* 0x080 */ 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xFF, 0xFF, 0x00, 0x00, 0x00, 0x9A, 0xCF, 0x00,
    /* 0x090 */ 0xFF, 0xFF, 0x00, 0x00, 0x00, 0x92, 0xCF, 0x00, 0xFF, 0xFF, 0x00, 0x00, 0x00, 0x9A, 0xAF, 0x00,
    /* 0x0a0 */ 0x1F, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    /* 0x0b0 */ 0x00, 0x00, 0x00, 0x00, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    /* 0x0c0 */ 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    /* 0x0d0 */ 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    /* 0x0e0 */ 0x00, 0x00, 0x00, 0x00, 0x18, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    /* 0x0f0 */ 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    /* 0x100 */ 0x66, 0xB8, 0x10, 0x00, 0x8E, 0xD8, 0x8E, 0xC0, 0x8E, 0xE0, 0x8E, 0xE8, 0x8E, 0xD0, 0xBC, 0xF0,
    /* 0x110 */ 0x0F, 0x00, 0x00, 0x0F, 0x20, 0xE0, 0x83, 0xC8, 0x20, 0x0F, 0x22, 0xE0, 0x8B, 0x86, 0xC0, 0x00,
    /* 0x120 */ 0x00, 0x00, 0x0F, 0x22, 0xD8, 0xB9, 0x80, 0x00, 0x00, 0xC0, 0x0F, 0x32, 0x0D, 0x00, 0x09, 0x00,
    /* 0x130 */ 0x00, 0x0F, 0x30, 0x0F, 0x20, 0xC0, 0x0D, 0x00, 0x00, 0x00, 0x80, 0x0F, 0x22, 0xC0, 0x8D, 0x86,
    /* 0x140 */ 0x80, 0x01, 0x00, 0x00, 0x89, 0x86, 0xE0, 0x00, 0x00, 0x00, 0xFF, 0xAE, 0xE0, 0x00, 0x00, 0x00,
    /* 0x150 */ 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    /* 0x160 */ 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    /* 0x170 */ 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    /* 0x180 */ 0x48, 0x89, 0xF3, 0x48, 0x8B, 0x83, 0xC8, 0x00, 0x00, 0x00, 0x48, 0x8B, 0xA3, 0xD0, 0x00, 0x00,
    /* 0x190 */ 0x00, 0x8B, 0xBB, 0xD8, 0x00, 0x00, 0x00, 0x66, 0xB9, 0x10, 0x00, 0x8E, 0xD9, 0x8E, 0xC1, 0x8E,
    /* 0x1a0 */ 0xE1, 0x8E, 0xE9, 0x8E, 0xD1, 0x48, 0x83, 0xEC, 0x08, 0xFF, 0xE0, 0x00, 0x00, 0x00, 0x00, 0x00,
    /* 0x1b0 */ 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    /* 0x1c0 */ 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    /* 0x1d0 */ 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    /* 0x1e0 */ 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    /* 0x1f0 */ 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];

/// Size of the trampoline page we install (full 4 KiB, zero-padded after the blob).
const AP_TRAMPOLINE_SIZE: usize = 4096;

/// Physical address the trampoline is copied to (must match SIPI vector 0x08 -> 0x8000).
const AP_TRAMPOLINE_PHYS: u64 = 0x8000;

const AP_TR_PARAMS_OFF: u64 = 0x0C0;

/// Copy the trampoline blob to its fixed physical address and identity-map the page.
fn install_ap_trampoline(mapper: &mut impl Mapper<Size4KiB>, frame_allocator: &mut impl FrameAllocator<Size4KiB>) {
    unsafe {
        core::ptr::copy_nonoverlapping(
            AP_TRAMPOLINE_BYTES.as_ptr(),
            AP_TRAMPOLINE_PHYS as *mut u8,
            AP_TRAMPOLINE_BYTES.len(),
        );
        core::ptr::write_bytes(
            (AP_TRAMPOLINE_PHYS as *mut u8).add(AP_TRAMPOLINE_BYTES.len()),
            0u8,
            AP_TRAMPOLINE_SIZE - AP_TRAMPOLINE_BYTES.len(),
        );
    }

    let page  = Page::<Size4KiB>::containing_address(VirtAddr::new(AP_TRAMPOLINE_PHYS));
    let frame = PhysFrame::<Size4KiB>::containing_address(PhysAddr::new(AP_TRAMPOLINE_PHYS));
    let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE;
    unsafe {
        match mapper.map_to(page, frame, flags, frame_allocator) {
            Ok(tlb) => tlb.flush(),
            Err(_)  => {}
        }
    }
    crate::serial_println!("[SMP] AP trampoline installed at {:#X}", AP_TRAMPOLINE_PHYS);
    crate::serial_println!("[SMP] trampoline check: rm-ljmp={:02X?} efer-or={:02X?}",
        unsafe { core::slice::from_raw_parts((AP_TRAMPOLINE_PHYS + 0x4A) as *const u8, 8) },
        unsafe { core::slice::from_raw_parts((AP_TRAMPOLINE_PHYS + 0x12C) as *const u8, 5) });
}


/// Write this AP's launch parameters into the live trampoline copy.
/// Called once per AP, immediately before sending its SIPI — APs are
/// brought up strictly one at a time (the caller waits for CPU_ONLINE
/// before moving to the next), so reusing the same parameter block is safe.
fn write_ap_params(cpu_index: u32, stack_top: u64) {
    use x86_64::registers::control::Cr3;
    let (cr3_frame, _) = Cr3::read();
    let cr3 = cr3_frame.start_address().as_u64();
    debug_assert!(cr3 < (1u64 << 32), "kernel CR3 must be below 4GiB for the 32-bit trampoline stage");

    let entry = ap_entry as *const () as u64;
    let base = (AP_TRAMPOLINE_PHYS + AP_TR_PARAMS_OFF) as *mut u8;
    unsafe {
        core::ptr::write_unaligned(base.add(0x00) as *mut u64, cr3);
        core::ptr::write_unaligned(base.add(0x08) as *mut u64, entry);
        core::ptr::write_unaligned(base.add(0x10) as *mut u64, stack_top);
        core::ptr::write_unaligned(base.add(0x18) as *mut u32, cpu_index);
    }
}

/// Detect if we are running under Hyper-V (which includes WHPX on Windows).
///
/// Hyper-V advertises itself via CPUID leaf 0x40000000:
///   EBX:ECX:EDX = b"Microsoft Hv" (little-endian, 4 bytes each)
/// When this is detected, the 16-bit AP real-mode trampoline cannot be used
/// because WHPX does not support executing 16-bit guest code.
fn running_under_hyper_v() -> bool {
    let ebx: u32;
    let ecx: u32;
    let edx: u32;
    unsafe {
        core::arch::asm!(
            "push rbx",
            "mov eax, 0x40000000",
            "cpuid",
            "mov {ebx:e}, ebx",
            "pop rbx",
            ebx = out(reg) ebx,
            out("eax") _,
            out("ecx") ecx,
            out("edx") edx,
            options(nostack),
        );
    }
    // "Microsoft Hv" as three little-endian u32s:
    //   EBX = 0x7263694D  ('M','i','c','r')
    //   ECX = 0x666F736F  ('o','s','o','f')
    //   EDX = 0x76482074  ('t',' ','H','v')
    ebx == 0x7263694D && ecx == 0x666F736F && edx == 0x76482074
}

fn cpuid_logical_cpu_count() -> u32 {
    let ebx: u32;
    unsafe {
        core::arch::asm!(
            "push rbx",
            "mov eax, 1",
            "cpuid",
            "mov {out:e}, ebx",
            "pop rbx",
            out = out(reg) ebx,
            out("eax") _,
            out("ecx") _,
            out("edx") _,
            options(nostack),
        );
    }
    let count = (ebx >> 16) & 0xFF;
    if count == 0 { 1 } else { count.min(MAX_CPUS as u32) }
}

// =====================================================================
// -----| ACPI MADT discovery |-----
// =====================================================================
//
// The MADT (Multiple APIC Description Table) is the authoritative,
// firmware/hypervisor-provided list of every logical processor and its real
// APIC ID. Unlike CPUID leaf 1's topology field, QEMU always populates the
// MADT correctly according to `-smp N` regardless of which accelerator
// (tcg, kvm, whpx, hvf) is backing the guest, because it's a static table
// built at VM construction time rather than something that has to be
// virtualized per-instruction.

/// One entry per enabled logical processor found in the MADT.
struct MadtResult {
    apic_ids: alloc::vec::Vec<u8>,
}

/// Read `len` bytes from physical memory via the bootloader's physical
/// memory mapping (`map_physical_memory`), which covers all of low memory
/// and any ACPI tables the firmware reports.
unsafe fn read_phys<T: Copy>(phys_addr: u64) -> T {
    let virt = crate::memory::get_phys_mem_offset().as_u64() + phys_addr;
    unsafe { core::ptr::read_unaligned(virt as *const T) }
}

unsafe fn read_phys_bytes(phys_addr: u64, len: usize) -> &'static [u8] {
    let virt = crate::memory::get_phys_mem_offset().as_u64() + phys_addr;
    unsafe { core::slice::from_raw_parts(virt as *const u8, len) }
}

/// Locate the RSDP by scanning the two regions the ACPI spec guarantees it
/// lives in on a BIOS boot: the first 1 KiB of the Extended BIOS Data Area,
/// and the BIOS read-only memory space 0xE0000–0xFFFFF. Both are scanned on
/// 16-byte boundaries for the 8-byte signature "RSD PTR ".
fn find_rsdp() -> Option<u64> {
    const SIG: &[u8; 8] = b"RSD PTR ";

    let checksum_ok = |addr: u64, len: usize| -> bool {
        let bytes = unsafe { read_phys_bytes(addr, len) };
        bytes.iter().fold(0u8, |acc, &b| acc.wrapping_add(b)) == 0
    };

    let scan = |start: u64, end: u64| -> Option<u64> {
        let mut addr = start;
        while addr + 8 <= end {
            let sig = unsafe { read_phys_bytes(addr, 8) };
            if sig == SIG && checksum_ok(addr, 20) {
                return Some(addr);
            }
            addr += 16;
        }
        None
    };

    // EBDA base is a real-mode segment stored as a u16 at physical 0x40E.
    let ebda_seg: u16 = unsafe { read_phys(0x40E) };
    let ebda_base = (ebda_seg as u64) << 4;
    if ebda_base != 0 {
        if let Some(a) = scan(ebda_base, ebda_base + 1024) { return Some(a); }
    }
    scan(0xE0000, 0x100000)
}

/// Parse the MADT (signature "APIC") reachable from the RSDP, returning the
/// APIC ID of every processor entry with the Enabled flag set.
fn discover_cpus_via_madt() -> Option<MadtResult> {
    let rsdp_addr = find_rsdp()?;

    let revision: u8 = unsafe { read_phys(rsdp_addr + 15) };

    // ACPI 2.0+: prefer the XSDT (64-bit pointers) when present and valid.
    let (sdt_addr, entry_size, is_xsdt): (u64, usize, bool) = if revision >= 2 {
        let xsdt_addr: u64 = unsafe { read_phys(rsdp_addr + 24) };
        if xsdt_addr != 0 { (xsdt_addr, 8, true) }
        else {
            let rsdt_addr: u32 = unsafe { read_phys(rsdp_addr + 16) };
            (rsdt_addr as u64, 4, false)
        }
    } else {
        let rsdt_addr: u32 = unsafe { read_phys(rsdp_addr + 16) };
        (rsdt_addr as u64, 4, false)
    };
    if sdt_addr == 0 { return None; }

    // Standard ACPI SDT header: sig[4] len[4] rev[1] checksum[1] oem_id[6]
    // oem_table_id[8] oem_rev[4] creator_id[4] creator_rev[4] = 36 bytes.
    let sdt_len: u32 = unsafe { read_phys(sdt_addr + 4) };
    if (sdt_len as usize) < 36 { return None; }
    let entry_count = (sdt_len as usize - 36) / entry_size;

    let mut madt_addr: Option<u64> = None;
    for i in 0..entry_count {
        let ptr_addr = sdt_addr + 36 + (i * entry_size) as u64;
        let table_addr: u64 = if is_xsdt {
            unsafe { read_phys::<u64>(ptr_addr) }
        } else {
            unsafe { read_phys::<u32>(ptr_addr) as u64 }
        };
        if table_addr == 0 { continue; }
        let sig = unsafe { read_phys_bytes(table_addr, 4) };
        if sig == b"APIC" {
            madt_addr = Some(table_addr);
            break;
        }
    }
    let madt_addr = madt_addr?;

    let madt_len: u32 = unsafe { read_phys(madt_addr + 4) };
    if (madt_len as usize) < 44 { return None; }

    let mut apic_ids = alloc::vec::Vec::new();
    let mut offset: u64 = madt_addr + 44; // past header + local_apic_addr + flags
    let end: u64 = madt_addr + madt_len as u64;

    while offset + 2 <= end {
        let entry_type: u8 = unsafe { read_phys(offset) };
        let entry_len: u8  = unsafe { read_phys(offset + 1) };
        if entry_len < 2 { break; } // malformed — avoid an infinite loop

        if entry_type == 0 && entry_len as u64 >= 8 {
            // Processor Local APIC: acpi_id[1] apic_id[1] flags[4]
            let apic_id: u8 = unsafe { read_phys(offset + 3) };
            let flags: u32  = unsafe { read_phys(offset + 4) };
            const ENABLED: u32 = 1 << 0;
            if flags & ENABLED != 0 {
                apic_ids.push(apic_id);
            }
        }
        // Type 9 (Local x2APIC) exists for >255 CPUs; out of scope for the
        // MAX_CPUS=8 cap this kernel currently supports.

        offset += entry_len as u64;
    }

    if apic_ids.is_empty() { None } else { Some(MadtResult { apic_ids }) }
}

// ── AP entry point ─────────────────────────────────────────────────────

/// A unit of ring-3 work handed to an idle AP.
///
/// This used to be a bare `(u64, u64, u64)` tuple with no pid, so the AP path
/// had no way to find the `Arc<Process>` again once the app exited — it just
/// cleared `USER_PROCESS_ACTIVE` and parked.  Windows were never destroyed,
/// the process was never marked dead, and its memory was never reclaimed on
/// an SMP boot.  The pid lets `park_and_wait_for_work` call the same
/// `on_process_exit()` the BSP path uses.
pub struct ApWorkItem {
    pub entry:     u64,
    pub stack_top: u64,
    pub teb_base:  u64,
    pub pid:       u32,
}

pub static AP_WORK_QUEUE: spin::Mutex<alloc::vec::Vec<ApWorkItem>> = spin::Mutex::new(alloc::vec::Vec::new());

pub fn park_and_wait_for_work() -> ! {
    loop {
        let work = AP_WORK_QUEUE.lock().pop();
        if let Some(ApWorkItem { entry, stack_top, teb_base, pid }) = work {
            unsafe { crate::loader::launch_into_usermode(entry, stack_top, teb_base); }
            // return_from_usermode() longjmps here with IF=0 — re-enable.
            unsafe { core::arch::asm!("sti"); }

            // Process exited (ExitProcess syscall or a fault handler called
            // return_from_usermode()).  Run the SAME teardown the BSP path
            // uses instead of just clearing the flag.
            if let Some(process) = crate::process::PROCESSOR.lock().find(pid) {
                crate::loader::on_process_exit(&process);
            } else {
                // Process already reaped from under us — still clear the flag.
                crate::interrupts::USER_PROCESS_ACTIVE.store(false, core::sync::atomic::Ordering::Relaxed);
            }
            crate::serial_println!("[SMP] AP process (pid {}) completed, parking.", pid);
        }
        unsafe { core::arch::asm!("hlt"); }
    }
}

/// Each AP jumps here after the SIPI.
///
/// At this point the AP is in 64-bit mode (QEMU qemu64 CPU) with:
///   • no GDT of its own   → we load one immediately
///   • no IDT loaded       → we load the shared IDT
///   • LAPIC enabled but timer not configured
///   • interrupts disabled (by INIT)
///
/// We keep this function `#[no_mangle]` so the SIPI target address can be
/// computed at compile time from the symbol, and `extern "C"` to get a
/// predictable calling convention when invoked from inline asm.
#[unsafe(no_mangle)]
pub extern "C" fn ap_entry(cpu_index: u32) -> ! {
    // 1. Load this AP's own GDT + TSS (unique stacks per CPU).
    //    init_ap() also calls set_cpu_syscall_stack(cpu_index) internally.
    crate::gdt::init_ap(cpu_index as usize);

    // 2. Load the shared IDT (already built on the BSP).
    crate::interrupts::init_idt_ap();

    // 3. Enable the LAPIC and configure the timer on this core.
    unsafe {
        // Spurious vector 0xFF — enables the LAPIC.
        lapic_write(LAPIC_SVR, 0x100 | 0xFF);
        // Timer: periodic, on the dedicated AP-timer vector. NOT vector 32: that is the
        // BSP's PIT handler, which bumps the global TICKS clock, runs the scheduler tick and
        // acks the legacy PIC -- and never sends a LAPIC EOI, so an AP would get exactly one
        // tick and then never wake from `hlt` again (see interrupts::AP_TIMER_VECTOR).
        lapic_write(LAPIC_TIMER_LVT,     0x20000 | crate::interrupts::AP_TIMER_VECTOR as u32); // periodic
        lapic_write(LAPIC_TIMER_DIVIDE,  0x3);      // divide by 16
        lapic_write(LAPIC_TIMER_INITIAL, 0x40000);  // initial count (~1 ms at 1 GHz)
    }

    // 4. Enable SYSCALL/SYSRET on this AP using the same MSR values as the BSP.
    crate::interrupts::init_syscall_ap();

    // 5. Mark this CPU as online.
    CPU_ONLINE.fetch_or(1 << cpu_index, Ordering::Release);
    crate::serial_println!("[SMP] CPU {} online (APIC ID {})", cpu_index, current_apic_id());

    // 6. Enable interrupts and park — the scheduler will give this core work.
    unsafe { core::arch::asm!("sti"); }
    park_and_wait_for_work()
}

// ── BSP startup sequence ───────────────────────────────────────────────

/// Called from `kernel_main` after the heap, GDT, and IDT are live.
///
/// 1. Discover LAPIC base and CPU count.
/// 2. Map the LAPIC into the virtual address space.
/// 3. Enable the BSP's LAPIC.
/// 4. Send INIT + SIPI × 2 to each AP.
/// 5. Wait for all APs to check in (with a 200 ms timeout per AP).
pub fn init(
    mapper:           &mut impl Mapper<Size4KiB>,
    frame_allocator:  &mut impl FrameAllocator<Size4KiB>,
) {
    // ── 1. Discover ──────────────────────────────────────────────────────
    let lapic_phys = read_lapic_phys_base_proper();

    // Prefer the ACPI MADT: it's the firmware/hypervisor's own authoritative
    // processor list and is populated correctly by QEMU according to
    // `-smp N` regardless of accelerator. CPUID leaf 1's topology field is
    // kept only as a fallback — under some hypervisors (WHPX in particular)
    // it can reflect host topology rather than the guest's requested count,
    // which is why this previously reported "1 CPU" even when more were
    // requested on the command line.
    let madt = discover_cpus_via_madt();
    let (cpu_count, apic_ids): (u32, alloc::vec::Vec<u8>) = match &madt {
        Some(m) => {
            let n = (m.apic_ids.len() as u32).min(MAX_CPUS as u32);
            crate::serial_println!("[SMP] MADT reports {} enabled processor(s): {:?}", m.apic_ids.len(), m.apic_ids);
            (n, m.apic_ids.clone())
        }
        None => {
            let n = cpuid_logical_cpu_count();
            crate::serial_println!("[SMP] MADT not found/parseable — falling back to CPUID leaf 1 ({})", n);
            // Fallback assumes the common QEMU layout: sequential APIC IDs 0..N-1.
            (n, (0..n as u8).collect())
        }
    };

    crate::serial_println!("[SMP] LAPIC phys base: {:#012x}", lapic_phys);
    crate::serial_println!("[SMP] Logical CPU count: {}", cpu_count);
    crate::println!("[SMP] {} logical CPUs detected", cpu_count);

    CPU_COUNT.store(cpu_count, Ordering::Release);

    // ── 2. Map the LAPIC page ─────────────────────────────────────────────
    // The LAPIC is MMIO — it must be mapped as uncacheable (NO_CACHE).
    let lapic_virt = lapic_phys; // identity map for simplicity
    let flags = PageTableFlags::PRESENT
              | PageTableFlags::WRITABLE
              | PageTableFlags::NO_CACHE;
    {
        let phys_frame = PhysFrame::<Size4KiB>::containing_address(PhysAddr::new(lapic_phys));
        let virt_page  = Page::<Size4KiB>::containing_address(VirtAddr::new(lapic_virt));
        unsafe {
            match mapper.map_to(virt_page, phys_frame, flags, frame_allocator) {
                Ok(tlb) => tlb.flush(),
                Err(_)  => {} // already mapped (e.g. if BIOS identity-mapped it)
            }
        }
    }
    LAPIC_BASE.store(lapic_virt as u32, Ordering::Release);

    // ── 3. Enable BSP LAPIC ───────────────────────────────────────────────
    unsafe {
        // Spurious Interrupt Vector Register: bit 8 = APIC Software Enable.
        lapic_write(LAPIC_SVR, 0x100 | 0xFF);
    }

    if cpu_count <= 1 {
        crate::serial_println!("[SMP] Single CPU — skipping AP startup.");
        return;
    }

    // ── WHPX / Hyper-V guard ────────────────────────────────────────────────────
    //
    // WHPX (Windows Hypervisor Platform) is built on Hyper-V and does not
    // support running guest code in 16-bit real mode. After SIPI the AP would
    // immediately take an Unrecoverable VP exit (exit code 4) and crash the VM.
    // Hyper-V identifies itself via CPUID leaf 0x40000000 returning
    // "Microsoft Hv" in EBX:ECX:EDX. When detected, skip AP startup entirely.
    if running_under_hyper_v() {
        crate::serial_println!(
            "[SMP] WARNING: Hyper-V/WHPX detected. 16-bit real-mode AP trampoline is "
        );
        crate::serial_println!(
            "[SMP]   not supported under this hypervisor. Running single-core."
        );
        crate::serial_println!(
            "[SMP]   To enable SMP: use -accel tcg,thread=multi -smp N instead of whpx."
        );
        CPU_COUNT.store(1, Ordering::Release);
        return;
    }

    // ── 4. Install the real AP trampoline ───────────────────────────────
    //
    // Every AP, on every accelerator, starts in 16-bit real mode after SIPI
    // — there is no "starts directly in long mode" special case for any
    // guest CPU model. A previous version of this code assumed one existed
    // and pointed SIPI straight at `ap_entry` (a 64-bit function) with no
    // real code ever placed at the SIPI vector; every AP jumped into
    // effectively empty low memory and silently never came up, which is why
    // only the BSP was ever online regardless of `-smp N`.
    install_ap_trampoline(mapper, frame_allocator);

    // ── 5. Start APs ──────────────────────────────────────────────────────
    //
    // The boot protocol (Intel SDM Vol.3 §8.4.4):
    //   a) INIT IPI  — resets the AP; it enters wait-for-SIPI state.
    //   b) 10 ms delay
    //   c) SIPI #1   — sends the startup vector (AP begins executing there).
    //   d) 200 µs delay
    //   e) SIPI #2   — in case #1 was missed.
    //
    // The SIPI vector byte encodes a 4 KiB-page-aligned address in the first
    // 1 MiB: real_mode_addr = vector << 12. Vector 0x08 → 0x8000, matching
    // AP_TRAMPOLINE_PHYS above.

    let bsp_apic_id = current_apic_id();

    let mut cpu_index: u32 = 0;
    for &ap_apic_id in apic_ids.iter() {
        if ap_apic_id == bsp_apic_id as u8 { continue; }
        cpu_index += 1;
        if cpu_index >= cpu_count { break; }

        crate::serial_println!("[SMP] Starting AP index {} (APIC ID {})", cpu_index, ap_apic_id);

        // Give this AP its own kernel stack and tell the trampoline where to
        // jump once it reaches long mode. Written fresh for each AP — they
        // come up strictly one at a time (we wait for CPU_ONLINE below
        // before touching the parameter block again).
        let stack_top = syscall_stack_top(cpu_index as usize) as u64;
        write_ap_params(cpu_index, stack_top);

        unsafe {
            // a) INIT IPI
            // Delivery mode 5 (INIT), Level=Assert, all others 0.
            lapic_send_ipi(ap_apic_id, 0x0000_4500);

            // b) 10 ms delay — busy loop; no timer yet on BSP at this point.
            busy_delay_ms(10);

            // c) SIPI #1 — vector byte = 0x08 (address 0x8000, where the real
            //    trampoline installed above now lives).
            lapic_send_ipi(ap_apic_id, 0x0000_4608); // delivery=STARTUP, vector=0x08

            // d) ~200 µs delay
            busy_delay_ms(1);

            // e) SIPI #2
            lapic_send_ipi(ap_apic_id, 0x0000_4608);
        }

        // ── 6. Wait for AP to come online (200 ms timeout) ────────────────
        let mut waited_ms = 0u32;
        loop {
            if CPU_ONLINE.load(Ordering::Acquire) & (1 << cpu_index) != 0 { break; }
            busy_delay_ms(1);
            waited_ms += 1;
            if waited_ms >= 200 {
                crate::serial_println!(
                    "[SMP] AP index {} (APIC ID {}) did not come online within 200 ms — skipping.",
                    cpu_index, ap_apic_id
                );
                // Reduce the advertised CPU count so the scheduler doesn't
                // try to assign work to a core that never woke up.
                CPU_COUNT.fetch_sub(1, Ordering::AcqRel);
                break;
            }
        }
    }

    let online = CPU_ONLINE.load(Ordering::Acquire).count_ones();
    crate::println!("[SMP] {}/{} CPUs online.", online, cpu_count);
    crate::serial_println!("[SMP] init complete. Online mask: {:#010b}", CPU_ONLINE.load(Ordering::Acquire));
}

/// Send an EOI to the LAPIC on the current CPU.
/// Called at the end of every hardware interrupt handler.
#[inline(always)]
pub fn lapic_eoi() {
    if LAPIC_BASE.load(Ordering::Relaxed) != 0 {
        unsafe { lapic_write(LAPIC_EOI, 0); }
    }
}

/// Busy-wait for `ms` milliseconds, timed by PIT channel 2 (1.193182 MHz).
///
/// This replaces a fixed-iteration `spin_loop()` (PAUSE) delay that was tuned for WHPX.
/// Under QEMU's TCG every PAUSE costs ~160 ns (it forces a trip out of the translated-code
/// loop), so the old "1 ms" took ~80 ms of wall time and SMP bring-up spun for seconds.
/// The PIT is an emulated hardware timer, so this delay is the same length on every
/// accelerator.
///
/// Used only during early SMP boot, before the kernel has its own timebase. Channel 2 is
/// used (not channel 0) so the system-tick PIT programmed by `init_pit` is left untouched.
#[inline(never)]
fn busy_delay_ms(ms: u32) {
    use x86_64::instructions::port::Port;
    let mut gate: Port<u8> = Port::new(0x61); // PIT ch2 gate + speaker control
    let mut cmd:  Port<u8> = Port::new(0x43); // PIT command
    let mut ch2:  Port<u8> = Port::new(0x42); // PIT channel 2 data
    for _ in 0..ms {
        unsafe {
            let saved = gate.read();
            gate.write(saved & !0x03);          // gate low, speaker off
            cmd.write(0b1011_0000);             // ch2, lobyte/hibyte, mode 0, binary
            ch2.write((1193u16 & 0xFF) as u8);  // 1193 ticks = ~0.99989 ms
            ch2.write((1193u16 >> 8) as u8);
            gate.write((saved & !0x02) | 0x01); // gate high: start counting
            // OUT2 (bit 5 of port 0x61) rises when the count reaches zero. Bounded so a
            // machine without a working channel 2 cannot hang boot.
            let mut polls = 0u32;
            while gate.read() & 0x20 == 0 && polls < 2_000_000 {
                polls += 1;
            }
            gate.write(saved);                  // restore original gate/speaker bits
        }
    }
}
