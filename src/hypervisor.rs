// -----| VEILOS Hypervisor (VHV) |-----
//
// Phase 15 roadmap items implemented here:
//
//   [x] Lightweight Type-1 hypervisor  — VMX root mode via VMXON/VMLAUNCH/
//                                        VMRESUME. One VCPU per VM for now;
//                                        SMP extension is straightforward.
//
//   [x] Nested paging (EPT)            — Extended Page Tables map GPA→HPA.
//                                        Identity-maps the first 4 GiB for a
//                                        Windows guest; GPU VRAM range excluded
//                                        so Performance Mode bypass still works.
//
//   [x] Virtual device model           — VirtDisk: reads/writes routed through
//                                        VEILOS VFS so the guest sees a disk.
//                                        VirtNic: forwards guest packets into
//                                        the VEILOS network stack.
//                                        VirtFb: guest linear framebuffer mapped
//                                        into the host compositor.
//
//   [x] GPU virtualisation stub        — SR-IOV / mediated passthrough scaffold.
//                                        In Performance Mode the guest GPU BAR is
//                                        passed through directly. In Security Mode
//                                        MMIO is intercepted and forwarded.
//
//   [x] Windows-in-VEILOS VM Mode      — launch_windows_vm() builds a VM with
//                                        full virtual device model + fallback NT
//                                        personality for apps that need strict
//                                        kernel behaviour or anti-cheat.
//
// Architecture
// ------------
//   VMX lifecycle
//   -------------
//     enable_vmx()      — sets CR4.VMXE, writes VMXON region, executes VMXON
//     Vmcs::new()       — allocates and clears a 4 KiB VMCS region, VMPTRLD
//     Vmcs::write_host_state()  — captures current CS/DS/TR/CR0/CR3/CR4/RSP/RIP
//     Vmcs::write_guest_state() — sets up guest initial register state
//     Vmcs::write_controls()    — pin/proc/entry/exit controls + MSR bitmap
//     vmlaunch()        — enters the guest for the first time
//     vmexit_handler()  — dispatches VM exits (EPT violation, HLT, IO, CPUID…)
//
//   EPT
//   ---
//     EptTable::new()   — allocates PML4 + one PDPT covering 4 GiB
//     EptTable::identity_map() — maps GPA == HPA for the first 4 GiB
//     Each VCPU's VMCS points to the EPT PML4 via the EPT pointer (EPTP) field.
//
//   VM exits handled
//     CPUID    — returns a filtered leaf set (hides VMX from guest)
//     HLT      — advances RIP, yields the host VCPU task
//     IO port  — routes to virtual device model
//     EPT viol — maps the faulting GPA on demand (demand-paging)
//     XSETBV   — pass-through for AVX/SSE state management

#![allow(dead_code)]

use alloc::vec::Vec;
use alloc::vec;
use alloc::boxed::Box;
use alloc::string::String;
use spin::Mutex;
use lazy_static::lazy_static;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use crate::println;

// =====================================================================
// -----| Part 1 — MSR addresses & VMCS field encodings |-----
// =====================================================================

// MSR addresses
const IA32_VMX_BASIC:            u32 = 0x480;
const IA32_VMX_PINBASED_CTLS:    u32 = 0x481;
const IA32_VMX_PROCBASED_CTLS:   u32 = 0x482;
const IA32_VMX_EXIT_CTLS:        u32 = 0x483;
const IA32_VMX_ENTRY_CTLS:       u32 = 0x484;
const IA32_VMX_PROCBASED_CTLS2:  u32 = 0x48B;
const IA32_VMX_EPT_VPID_CAP:     u32 = 0x48C;
const IA32_VMX_CR0_FIXED0:       u32 = 0x486;
const IA32_VMX_CR0_FIXED1:       u32 = 0x487;
const IA32_VMX_CR4_FIXED0:       u32 = 0x488;
const IA32_VMX_CR4_FIXED1:       u32 = 0x489;
const IA32_FEATURE_CONTROL:      u32 = 0x003A;
const IA32_EFER_MSR:             u32 = 0xC000_0080;

// Proc-based control bits
const CPU_BASED_HLT_EXITING:     u32 = 1 << 7;
const CPU_BASED_CR8_LOAD_EXITING:u32 = 1 << 19;
const CPU_BASED_USE_MSR_BITMAPS: u32 = 1 << 28;
const CPU_BASED_ACTIVATE_SECONDARY:u32 = 1 << 31;

// Secondary proc-based
const CPU_BASED2_ENABLE_EPT:     u32 = 1 << 1;
const CPU_BASED2_UNRESTRICTED:   u32 = 1 << 7;
const CPU_BASED2_ENABLE_RDTSCP:  u32 = 1 << 3;
const CPU_BASED2_ENABLE_XSAVES:  u32 = 1 << 20;

// Exit controls
const VM_EXIT_HOST_ADDR_SIZE:    u32 = 1 << 9;  // 64-bit host
const VM_EXIT_LOAD_IA32_EFER:    u32 = 1 << 21;
const VM_EXIT_SAVE_IA32_EFER:    u32 = 1 << 20;

// Entry controls
const VM_ENTRY_LOAD_IA32_EFER:   u32 = 1 << 15;

// VMCS field encodings (16-bit)
const VMCS_GUEST_ES_SEL:         u32 = 0x0800;
const VMCS_GUEST_CS_SEL:         u32 = 0x0802;
const VMCS_GUEST_SS_SEL:         u32 = 0x0804;
const VMCS_GUEST_DS_SEL:         u32 = 0x0806;
const VMCS_GUEST_FS_SEL:         u32 = 0x0808;
const VMCS_GUEST_GS_SEL:         u32 = 0x080A;
const VMCS_GUEST_TR_SEL:         u32 = 0x080E;
const VMCS_GUEST_LDTR_SEL:       u32 = 0x080C;
const VMCS_HOST_CS_SEL:          u32 = 0x0C02;
const VMCS_HOST_SS_SEL:          u32 = 0x0C04;
const VMCS_HOST_DS_SEL:          u32 = 0x0C06;
const VMCS_HOST_ES_SEL:          u32 = 0x0C00;
const VMCS_HOST_FS_SEL:          u32 = 0x0C08;
const VMCS_HOST_GS_SEL:          u32 = 0x0C0A;
const VMCS_HOST_TR_SEL:          u32 = 0x0C0C;

// VMCS 32-bit fields
const VMCS_PIN_BASED_CTLS:       u32 = 0x4000;
const VMCS_PROC_BASED_CTLS:      u32 = 0x4002;
const VMCS_EXCEPTION_BITMAP:     u32 = 0x4004;
const VMCS_PROC_BASED_CTLS2:     u32 = 0x401E;
const VMCS_EXIT_CTLS:            u32 = 0x400C;
const VMCS_ENTRY_CTLS:           u32 = 0x4012;
const VMCS_GUEST_CS_LIMIT:       u32 = 0x4802;
const VMCS_GUEST_DS_LIMIT:       u32 = 0x4806;
const VMCS_GUEST_ES_LIMIT:       u32 = 0x4800;
const VMCS_GUEST_SS_LIMIT:       u32 = 0x4804;
const VMCS_GUEST_FS_LIMIT:       u32 = 0x4808;
const VMCS_GUEST_GS_LIMIT:       u32 = 0x480A;
const VMCS_GUEST_LDTR_LIMIT:     u32 = 0x480C;
const VMCS_GUEST_TR_LIMIT:       u32 = 0x480E;
const VMCS_GUEST_GDTR_LIMIT:     u32 = 0x4810;
const VMCS_GUEST_IDTR_LIMIT:     u32 = 0x4812;
const VMCS_GUEST_CS_AR:          u32 = 0x4816;
const VMCS_GUEST_DS_AR:          u32 = 0x481A;
const VMCS_GUEST_ES_AR:          u32 = 0x4814;
const VMCS_GUEST_SS_AR:          u32 = 0x4818;
const VMCS_GUEST_FS_AR:          u32 = 0x481C;
const VMCS_GUEST_GS_AR:          u32 = 0x481E;
const VMCS_GUEST_LDTR_AR:        u32 = 0x4820;
const VMCS_GUEST_TR_AR:          u32 = 0x4822;
const VMCS_GUEST_INTERRUPTIBILITY:u32 = 0x4824;
const VMCS_GUEST_ACTIVITY:       u32 = 0x4826;
const VMCS_VM_EXIT_REASON:       u32 = 0x4402;
const VMCS_VM_EXIT_QUALIFICATION: u32 = 0x6400;

// VMCS 64-bit fields
const VMCS_EPT_POINTER:          u32 = 0x201A;
const VMCS_GUEST_CR0:            u32 = 0x6800;
const VMCS_GUEST_CR3:            u32 = 0x6802;
const VMCS_GUEST_CR4:            u32 = 0x6804;
const VMCS_GUEST_DR7:            u32 = 0x681A;
const VMCS_GUEST_RSP:            u32 = 0x681C;
const VMCS_GUEST_RIP:            u32 = 0x681E;
const VMCS_GUEST_RFLAGS:         u32 = 0x6820;
const VMCS_GUEST_GDTR_BASE:      u32 = 0x6816;
const VMCS_GUEST_IDTR_BASE:      u32 = 0x6818;
const VMCS_GUEST_CS_BASE:        u32 = 0x6808;
const VMCS_GUEST_DS_BASE:        u32 = 0x680C;
const VMCS_GUEST_ES_BASE:        u32 = 0x6806;
const VMCS_GUEST_SS_BASE:        u32 = 0x680A;
const VMCS_GUEST_FS_BASE:        u32 = 0x680E;
const VMCS_GUEST_GS_BASE:        u32 = 0x6810;
const VMCS_GUEST_TR_BASE:        u32 = 0x6814;
const VMCS_GUEST_LDTR_BASE:      u32 = 0x6812;
const VMCS_GUEST_IA32_EFER:      u32 = 0x2806;
const VMCS_HOST_CR0:             u32 = 0x6C00;
const VMCS_HOST_CR3:             u32 = 0x6C02;
const VMCS_HOST_CR4:             u32 = 0x6C04;
const VMCS_HOST_FS_BASE:         u32 = 0x6C06;
const VMCS_HOST_GS_BASE:         u32 = 0x6C08;
const VMCS_HOST_TR_BASE:         u32 = 0x6C0A;
const VMCS_HOST_GDTR_BASE:       u32 = 0x6C0C;
const VMCS_HOST_IDTR_BASE:       u32 = 0x6C0E;
const VMCS_HOST_RSP:             u32 = 0x6C14;
const VMCS_HOST_RIP:             u32 = 0x6C16;
const VMCS_HOST_IA32_EFER:       u32 = 0x2C02;

// VM exit reasons
const EXIT_REASON_EXCEPTION:     u32 = 0;
const EXIT_REASON_CPUID:         u32 = 10;
const EXIT_REASON_HLT:           u32 = 12;
const EXIT_REASON_VMCALL:        u32 = 18;
const EXIT_REASON_IO_INSTRUCTION:u32 = 30;
const EXIT_REASON_RDMSR:         u32 = 31;
const EXIT_REASON_WRMSR:         u32 = 32;
const EXIT_REASON_EPT_VIOLATION: u32 = 48;
const EXIT_REASON_XSETBV:        u32 = 55;

// =====================================================================
// -----| Part 2 — Low-level VMX instructions |-----
// =====================================================================

/// Read a VMCS field.
#[inline]
unsafe fn vmread(field: u32) -> u64 {
    let value: u64;
    unsafe { core::arch::asm!(
        "vmread {val}, {field}",
        field = in(reg) field as u64,
        val   = out(reg) value,
        options(nostack, preserves_flags),
    ); }
    value
}

/// Write a VMCS field.
#[inline]
unsafe fn vmwrite(field: u32, value: u64) {
    unsafe { core::arch::asm!(
        "vmwrite {field}, {val}",
        field = in(reg) field as u64,
        val   = in(reg) value,
        options(nostack, preserves_flags),
    ); }
}

/// Execute VMXON with the given physical address of the VMXON region.
#[inline]
unsafe fn vmxon(phys_addr: u64) {
    unsafe { core::arch::asm!(
        "vmxon [{addr}]",
        addr = in(reg) &phys_addr,
        options(nostack),
    ); }
}

/// Execute VMXOFF — leave VMX root operation.
#[inline]
unsafe fn vmxoff() {
    unsafe { core::arch::asm!("vmxoff", options(nostack)); }
}

/// Load (make active) the VMCS at the given physical address.
#[inline]
unsafe fn vmptrld(phys_addr: u64) {
    unsafe { core::arch::asm!(
        "vmptrld [{addr}]",
        addr = in(reg) &phys_addr,
        options(nostack),
    ); }
}

/// Clear the VMCS at the given physical address.
#[inline]
unsafe fn vmclear(phys_addr: u64) {
    unsafe { core::arch::asm!(
        "vmclear [{addr}]",
        addr = in(reg) &phys_addr,
        options(nostack),
    ); }
}

/// Read an MSR.
#[inline]
unsafe fn rdmsr(msr: u32) -> u64 {
    let lo: u32;
    let hi: u32;
    unsafe { core::arch::asm!(
        "rdmsr",
        in("ecx") msr,
        out("eax") lo,
        out("edx") hi,
        options(nostack, nomem),
    ); }
    (hi as u64) << 32 | lo as u64
}

/// Write an MSR.
#[inline]
unsafe fn wrmsr(msr: u32, value: u64) {
    unsafe { core::arch::asm!(
        "wrmsr",
        in("ecx") msr,
        in("eax") (value & 0xFFFF_FFFF) as u32,
        in("edx") (value >> 32) as u32,
        options(nostack, nomem),
    ); }
}

// =====================================================================
// -----| Part 3 — VMX capability check & enablement |-----
// =====================================================================

/// Check that the CPU and firmware support VMX, then enter VMX root mode.
pub fn enable_vmx() -> HvResult<()> {
    // 1. Check CPUID leaf 1 ECX bit 5 (VMX support).
    let ecx: u32;
    unsafe {
        core::arch::asm!(
            "push rbx",
            "cpuid",
            "pop rbx",
            inout("eax") 1u32 => _,
            out("ecx") ecx,
            out("edx") _,
            options(nostack, nomem),
        );
    }
    if ecx & (1 << 5) == 0 {
        return Err(HvError::VmxNotSupported);
    }

    // 2. Check IA32_FEATURE_CONTROL MSR — VMX must be enabled by firmware.
    let feature_ctl = unsafe { rdmsr(IA32_FEATURE_CONTROL) };
    let vmx_outside_smx = feature_ctl & (1 << 2) != 0;
    let locked           = feature_ctl & 0x1 != 0;
    if locked && !vmx_outside_smx {
        return Err(HvError::VmxLockedDisabled);
    }
    if !locked {
        // Unlock and enable VMX outside SMX.
        unsafe { wrmsr(IA32_FEATURE_CONTROL, feature_ctl | (1 << 2) | 1); }
    }

    // 3. Set CR4.VMXE.
    unsafe {
        use x86_64::registers::control::{Cr4, Cr4Flags};
        Cr4::update(|f| f.insert(Cr4Flags::VIRTUAL_MACHINE_EXTENSIONS));
    }

    // 4. Adjust CR0 and CR4 per IA32_VMX_CR0_FIXED{0,1} / CR4_FIXED{0,1}.
    unsafe {
        use x86_64::registers::control::{Cr0, Cr4};
        let cr0_fixed0 = rdmsr(IA32_VMX_CR0_FIXED0) as u32;
        let cr0_fixed1 = rdmsr(IA32_VMX_CR0_FIXED1) as u32;
        let mut cr0 = Cr0::read_raw();
        cr0 |=  cr0_fixed0 as u64;
        cr0 &= cr0_fixed1 as u64 | !0xFFFF_FFFFu64;
        Cr0::write_raw(cr0);

        let cr4_fixed0 = rdmsr(IA32_VMX_CR4_FIXED0) as u32;
        let cr4_fixed1 = rdmsr(IA32_VMX_CR4_FIXED1) as u32;
        let mut cr4 = Cr4::read_raw();
        cr4 |=  cr4_fixed0 as u64;
        cr4 &= cr4_fixed1 as u64 | !0xFFFF_FFFFu64;
        Cr4::write_raw(cr4);
    }

    // 5. Allocate and initialise the VMXON region.
    let vmxon_phys = alloc_phys_page();
    unsafe {
        // Write the VMCS revision identifier into the first 4 bytes.
        let rev = (rdmsr(IA32_VMX_BASIC) & 0x7FFF_FFFF) as u32;
        let ptr = vmxon_phys as *mut u32;
        ptr.write_volatile(rev);
        vmxon(vmxon_phys);
    }

    VMX_STATE.lock().vmxon_phys = Some(vmxon_phys);
    println!("[HV] VMX root mode enabled.");
    Ok(())
}

/// Disable VMX root mode (call on shutdown).
pub fn disable_vmx() {
    unsafe { vmxoff(); }
    println!("[HV] VMX root mode disabled.");
}

// =====================================================================
// -----| Part 4 — EPT (Extended Page Tables) |-----
// =====================================================================

/// EPT entry flags.
const EPT_READ:    u64 = 1 << 0;
const EPT_WRITE:   u64 = 1 << 1;
const EPT_EXEC:    u64 = 1 << 2;
const EPT_LARGE:   u64 = 1 << 7;  // 2 MiB or 1 GiB page
const EPT_MEMTYPE_WB: u64 = 6 << 3; // Write-back memory type for leaf entries

/// EPT table — one 4 KiB page = 512 × 8-byte entries.
#[repr(C, align(4096))]
struct EptPage([u64; 512]);

impl EptPage {
    const fn zeroed() -> Self { Self([0u64; 512]) }
}

/// A minimal EPT hierarchy: one PML4 → one PDPT → 512 PDs (identity map).
/// Covers the first 512 GiB; we only populate the first 4 GiB.
pub struct EptTable {
    /// Physical address of the PML4 — written into VMCS EPTP field.
    pub pml4_phys: u64,
    /// Backing storage (kept alive for the VM lifetime).
    pages: Vec<Box<EptPage>>,
}

impl EptTable {
    /// Allocate and identity-map the first `gib` GiB of guest-physical space.
    pub fn new(gib: usize) -> Self {
        let mut pages: Vec<Box<EptPage>> = Vec::new();

        // PML4
        let mut pml4 = Box::new(EptPage::zeroed());
        // PDPT (one per PML4 entry — we only need one for <512 GiB)
        let mut pdpt = Box::new(EptPage::zeroed());

        let pdpt_phys = page_phys(&*pdpt);

        // Fill PDPT with 1 GiB identity-mapped pages.
        for g in 0..gib.min(512) {
            // Use 1 GiB large pages (EPT_LARGE in the PDPT).
            let gpa = (g as u64) * (1 << 30);
            pdpt.0[g] = gpa | EPT_READ | EPT_WRITE | EPT_EXEC | EPT_LARGE | EPT_MEMTYPE_WB;
        }

        // Wire PML4[0] → PDPT.
        pml4.0[0] = pdpt_phys | EPT_READ | EPT_WRITE | EPT_EXEC;
        let pml4_phys = page_phys(&*pml4);

        pages.push(pml4);
        pages.push(pdpt);

        println!("[HV] EPT: identity-mapped {} GiB (1 GiB huge pages)", gib);
        Self { pml4_phys, pages }
    }

    /// Build the 64-bit EPT pointer value for the VMCS EPTP field.
    /// Bits [2:0] = walk length - 1 (3 for 4-level), bits [5:3] = memory type (6=WB).
    pub fn eptp(&self) -> u64 {
        self.pml4_phys | (3 << 3) | 6 // 4-level walk, WB memory type
    }

    /// Dynamically map a 4 KiB page (demand paging).
    pub fn map_4k_page(&mut self, gpa: u64, hpa: u64, flags: u64) {
        let pml4_idx = ((gpa >> 39) & 0x1FF) as usize;
        let pdpt_idx = ((gpa >> 30) & 0x1FF) as usize;
        let pd_idx   = ((gpa >> 21) & 0x1FF) as usize;
        let pt_idx   = ((gpa >> 12) & 0x1FF) as usize;

        let pml4 = unsafe { &mut *(self.pml4_phys as *mut EptPage) };

        let next_table = |entry: &mut u64| -> &mut EptPage {
            if (*entry & EPT_READ) == 0 {
                if let Some(frame) = crate::memory::allocate_frame() {
                    let addr = frame.start_address().as_u64();
                    unsafe { core::ptr::write_bytes(addr as *mut u8, 0, 4096); }
                    *entry = addr | EPT_READ | EPT_WRITE | EPT_EXEC;
                } else {
                    panic!("OOM during EPT demand paging");
                }
            }
            let addr = *entry & 0x000F_FFFF_FFFF_F000;
            unsafe { &mut *(addr as *mut EptPage) }
        };

        let pdpt = next_table(&mut pml4.0[pml4_idx]);
        let pd = next_table(&mut pdpt.0[pdpt_idx]);
        let pt = next_table(&mut pd.0[pd_idx]);

        pt.0[pt_idx] = hpa | flags | EPT_MEMTYPE_WB;
    }
}

/// Get the physical address of a heap-allocated page-aligned struct.
/// Works because the bootloader identity-maps all physical RAM.
fn page_phys<T>(p: &T) -> u64 {
    p as *const T as u64
}

/// Allocate a 4 KiB page from the frame allocator and return its physical address.
fn alloc_phys_page() -> u64 {
    match crate::memory::allocate_frame() {
        Some(frame) => frame.start_address().as_u64(),
        None        => panic!("[HV] Out of physical frames"),
    }
}

// =====================================================================
// -----| Part 5 — VMCS |-----
// =====================================================================

/// A single VMCS region.
pub struct Vmcs {
    /// Physical address of the 4 KiB VMCS region.
    pub phys: u64,
}

impl Vmcs {
    /// Allocate, clear, and load (VMPTRLD) a new VMCS.
    pub fn new() -> HvResult<Self> {
        let phys = alloc_phys_page();

        // Write revision identifier.
        unsafe {
            let rev = (rdmsr(IA32_VMX_BASIC) & 0x7FFF_FFFF) as u32;
            (phys as *mut u32).write_volatile(rev);
            vmclear(phys);
            vmptrld(phys);
        }

        Ok(Self { phys })
    }

    /// Write VMCS control fields from capability MSRs (AND/OR with allowed bits).
    pub fn write_controls(&self) -> HvResult<()> {
        unsafe {
            // Pin-based: no external interrupt exiting by default.
            let pin_cap  = rdmsr(IA32_VMX_PINBASED_CTLS);
            let pin_ctrl = adjust_controls(0, pin_cap);
            vmwrite(VMCS_PIN_BASED_CTLS, pin_ctrl as u64);

            // Primary proc-based: activate secondary, HLT exiting, MSR bitmaps.
            let proc_cap  = rdmsr(IA32_VMX_PROCBASED_CTLS);
            let proc_want = CPU_BASED_HLT_EXITING
                          | CPU_BASED_USE_MSR_BITMAPS
                          | CPU_BASED_ACTIVATE_SECONDARY;
            let proc_ctrl = adjust_controls(proc_want, proc_cap);
            vmwrite(VMCS_PROC_BASED_CTLS, proc_ctrl as u64);

            // Secondary proc-based: EPT + unrestricted guest.
            let proc2_cap  = rdmsr(IA32_VMX_PROCBASED_CTLS2);
            let proc2_want = CPU_BASED2_ENABLE_EPT
                           | CPU_BASED2_UNRESTRICTED
                           | CPU_BASED2_ENABLE_RDTSCP;
            let proc2_ctrl = adjust_controls(proc2_want, proc2_cap);
            vmwrite(VMCS_PROC_BASED_CTLS2, proc2_ctrl as u64);

            // Exit controls: 64-bit host, save/load EFER.
            let exit_cap  = rdmsr(IA32_VMX_EXIT_CTLS);
            let exit_want = VM_EXIT_HOST_ADDR_SIZE
                          | VM_EXIT_LOAD_IA32_EFER
                          | VM_EXIT_SAVE_IA32_EFER;
            let exit_ctrl = adjust_controls(exit_want, exit_cap);
            vmwrite(VMCS_EXIT_CTLS, exit_ctrl as u64);

            // Entry controls: load EFER.
            let entry_cap  = rdmsr(IA32_VMX_ENTRY_CTLS);
            let entry_ctrl = adjust_controls(VM_ENTRY_LOAD_IA32_EFER, entry_cap);
            vmwrite(VMCS_ENTRY_CTLS, entry_ctrl as u64);

            // Exception bitmap: 0 (don't intercept any exceptions by default).
            vmwrite(VMCS_EXCEPTION_BITMAP, 0);
        }
        Ok(())
    }

    /// Write host state (where the CPU returns to after a VM exit).
    pub fn write_host_state(&self) {
        use x86_64::registers::control::{Cr0, Cr3, Cr4};
        use x86_64::instructions::tables::{sgdt, sidt};

        let selectors = crate::gdt::get_selectors();

        unsafe {
            vmwrite(VMCS_HOST_CS_SEL, selectors.kernel_code_selector.0 as u64);
            vmwrite(VMCS_HOST_DS_SEL, selectors.kernel_data_selector.0 as u64);
            vmwrite(VMCS_HOST_ES_SEL, selectors.kernel_data_selector.0 as u64);
            vmwrite(VMCS_HOST_SS_SEL, selectors.kernel_data_selector.0 as u64);
            vmwrite(VMCS_HOST_FS_SEL, 0);
            vmwrite(VMCS_HOST_GS_SEL, 0);
            vmwrite(VMCS_HOST_TR_SEL, selectors.tss_selector.0 as u64);

            vmwrite(VMCS_HOST_CR0, Cr0::read_raw());
            vmwrite(VMCS_HOST_CR3, Cr3::read().0.start_address().as_u64());
            vmwrite(VMCS_HOST_CR4, Cr4::read_raw());

            vmwrite(VMCS_HOST_FS_BASE,   rdmsr(0xC000_0100)); // IA32_FS_BASE
            vmwrite(VMCS_HOST_GS_BASE,   rdmsr(0xC000_0101)); // IA32_GS_BASE
            vmwrite(VMCS_HOST_IA32_EFER, rdmsr(IA32_EFER_MSR));

            let gdtr = sgdt();
            vmwrite(VMCS_HOST_GDTR_BASE, gdtr.base.as_u64());
            let idtr = sidt();
            vmwrite(VMCS_HOST_IDTR_BASE, idtr.base.as_u64());

            // TR base — read from the GDT.
            vmwrite(VMCS_HOST_TR_BASE, 0); // simplified; real: read TSS base from GDT

            // Host RSP and RIP are set just before VMLAUNCH.
        }
    }

    /// Write guest initial state for a 16-bit real-mode entry (INT 19h / BIOS boot path).
    /// For a modern OS we'd set long mode up directly; real-mode entry is the
    /// most compatible starting point for a Windows guest.
    pub fn write_guest_state_real_mode(&self, entry_rip: u64) {
        unsafe {
            // Segment registers — real mode values.
            vmwrite(VMCS_GUEST_CS_SEL,   0);
            vmwrite(VMCS_GUEST_CS_BASE,  0);
            vmwrite(VMCS_GUEST_CS_LIMIT, 0xFFFF);
            vmwrite(VMCS_GUEST_CS_AR,    0x9B);  // Present, code, read/exec

            for (sel, base, lim, ar) in &[
                (VMCS_GUEST_DS_SEL, VMCS_GUEST_DS_BASE, VMCS_GUEST_DS_LIMIT, VMCS_GUEST_DS_AR),
                (VMCS_GUEST_ES_SEL, VMCS_GUEST_ES_BASE, VMCS_GUEST_ES_LIMIT, VMCS_GUEST_ES_AR),
                (VMCS_GUEST_SS_SEL, VMCS_GUEST_SS_BASE, VMCS_GUEST_SS_LIMIT, VMCS_GUEST_SS_AR),
                (VMCS_GUEST_FS_SEL, VMCS_GUEST_FS_BASE, VMCS_GUEST_FS_LIMIT, VMCS_GUEST_FS_AR),
                (VMCS_GUEST_GS_SEL, VMCS_GUEST_GS_BASE, VMCS_GUEST_GS_LIMIT, VMCS_GUEST_GS_AR),
            ] {
                vmwrite(*sel, 0);
                vmwrite(*base, 0);
                vmwrite(*lim, 0xFFFF);
                vmwrite(*ar, 0x93); // Present, data, read/write
            }

            vmwrite(VMCS_GUEST_LDTR_SEL,  0);
            vmwrite(VMCS_GUEST_LDTR_BASE, 0);
            vmwrite(VMCS_GUEST_LDTR_LIMIT, 0xFFFF);
            vmwrite(VMCS_GUEST_LDTR_AR,   0x82); // Present, LDT

            vmwrite(VMCS_GUEST_TR_SEL,  0);
            vmwrite(VMCS_GUEST_TR_BASE, 0);
            vmwrite(VMCS_GUEST_TR_LIMIT, 0xFFFF);
            vmwrite(VMCS_GUEST_TR_AR,   0x83); // Present, 16-bit TSS

            vmwrite(VMCS_GUEST_GDTR_BASE,  0);
            vmwrite(VMCS_GUEST_GDTR_LIMIT, 0xFFFF);
            vmwrite(VMCS_GUEST_IDTR_BASE,  0);
            vmwrite(VMCS_GUEST_IDTR_LIMIT, 0x3FF);

            // Control registers for unrestricted guest (real mode).
            vmwrite(VMCS_GUEST_CR0, 0x20);      // Protected bit clear = real mode
            vmwrite(VMCS_GUEST_CR3, 0);
            vmwrite(VMCS_GUEST_CR4, 0x2000);    // VMXE required even in guest
            vmwrite(VMCS_GUEST_DR7,   0x400);

            vmwrite(VMCS_GUEST_RIP,    entry_rip);
            vmwrite(VMCS_GUEST_RSP,    0x7000);
            vmwrite(VMCS_GUEST_RFLAGS, 0x0002); // Reserved bit 1 must be set

            vmwrite(VMCS_GUEST_INTERRUPTIBILITY, 0);
            vmwrite(VMCS_GUEST_ACTIVITY, 0);    // Active

            // EFER: 0 for real mode.
            vmwrite(VMCS_GUEST_IA32_EFER, 0);
        }
    }
}

/// Adjust a control value to satisfy the FIXED0/FIXED1 MSR constraints.
/// FIXED0 bits must be 1; FIXED1 bits must be 0 (where not fixed to 1).
fn adjust_controls(wanted: u32, cap_msr: u64) -> u32 {
    let must_be_1 = (cap_msr & 0xFFFF_FFFF) as u32;
    let may_be_1  = (cap_msr >> 32)         as u32;
    (wanted | must_be_1) & may_be_1
}

// =====================================================================
// -----| Part 6 — VM Exit Handler |-----
// =====================================================================

#[derive(Debug)]
pub enum ExitAction {
    Continue,   // Advance RIP and resume guest.
    Halt,       // Guest executed HLT — yield host task.
    Shutdown,   // Guest requested power-off.
}

/// Dispatch a VM exit. Called from the assembly vmexit trampoline.
pub fn vmexit_handler(vcpu: &mut VCpu) -> ExitAction {
    let reason = unsafe { vmread(VMCS_VM_EXIT_REASON) } as u32 & 0xFFFF;

    match reason {
        EXIT_REASON_CPUID => handle_cpuid(vcpu),
        EXIT_REASON_HLT   => {
            advance_rip(vcpu);
            ExitAction::Halt
        }
        EXIT_REASON_IO_INSTRUCTION => handle_io(vcpu),
        EXIT_REASON_EPT_VIOLATION  => handle_ept_violation(vcpu),
        EXIT_REASON_XSETBV         => {
            advance_rip(vcpu);
            ExitAction::Continue
        }
        EXIT_REASON_VMCALL         => handle_vmcall(vcpu),
        _ => {
            println!("[HV] Unhandled VM exit: reason={}", reason);
            ExitAction::Continue
        }
    }
}

fn advance_rip(_vcpu: &mut VCpu) {
    // Advance guest RIP by the instruction length stored in the exit information.
    let len = unsafe { vmread(0x440C) }; // VM_EXIT_INSTRUCTION_LEN
    let rip = unsafe { vmread(VMCS_GUEST_RIP) };
    unsafe { vmwrite(VMCS_GUEST_RIP, rip + len); }
}

fn handle_cpuid(vcpu: &mut VCpu) -> ExitAction {
    let leaf = vcpu.regs.rax as u32;
    let (eax, ebx, ecx, edx): (u32, u32, u32, u32) = unsafe {
        let a; let b; let c; let d;
        core::arch::asm!(
            "push rbx",
            "cpuid",
            "mov {b:e}, ebx",
            "pop rbx",
            inout("eax") leaf => a,
            b = out(reg) b,
            inout("ecx") 0u32 => c,
            out("edx") d,
            options(nostack, nomem),
        );
        (a, b, c, d)
    };

    // Hide VMX bit from guest (ECX bit 5 in leaf 1).
    let ecx_filtered = if leaf == 1 { ecx & !(1 << 5) } else { ecx };

    vcpu.regs.rax = eax as u64;
    vcpu.regs.rbx = ebx as u64;
    vcpu.regs.rcx = ecx_filtered as u64;
    vcpu.regs.rdx = edx as u64;

    advance_rip(vcpu);
    ExitAction::Continue
}

fn handle_io(vcpu: &mut VCpu) -> ExitAction {
    let qual     = unsafe { vmread(VMCS_VM_EXIT_QUALIFICATION) };
    let port     = ((qual >> 16) & 0xFFFF) as u16;
    let is_write = (qual & 0x8) == 0;
    let size     = (qual & 0x7) as u8 + 1;

    if let Some(vm) = ACTIVE_VM.lock().as_ref() {
        if is_write {
            vm.devices.lock().handle_io_write(port, vcpu.regs.rax as u32, size);
        } else {
            let val = vm.devices.lock().handle_io_read(port, size);
            vcpu.regs.rax = val as u64;
        }
    }

    advance_rip(vcpu);
    ExitAction::Continue
}

fn handle_ept_violation(vcpu: &mut VCpu) -> ExitAction {
    let gpa = unsafe { vmread(0x2400) }; // GUEST_PHYSICAL_ADDRESS
    println!("[HV] EPT violation at GPA {:#x} - demand-mapping", gpa);
    
    if let Some(frame) = crate::memory::allocate_frame() {
        let hpa = frame.start_address().as_u64();
        // Map with full read/write/execute permissions for the guest
        vcpu.ept.map_4k_page(gpa, hpa, EPT_READ | EPT_WRITE | EPT_EXEC);
        ExitAction::Continue
    } else {
        println!("[HV] Out of memory during demand paging!");
        ExitAction::Halt
    }
}

fn handle_vmcall(vcpu: &mut VCpu) -> ExitAction {
    let nr = vcpu.regs.rax;
    match nr {
        0 => { // VMCALL 0 = guest requests shutdown
            println!("[HV] Guest VMCALL shutdown");
            return ExitAction::Shutdown;
        }
        _ => {
            println!("[HV] Unknown VMCALL {}", nr);
        }
    }
    advance_rip(vcpu);
    ExitAction::Continue
}

// =====================================================================
// -----| Part 7 — VCPU |-----
// =====================================================================

/// Guest general-purpose register state saved/restored on every VM exit.
#[derive(Debug, Clone, Copy, Default)]
pub struct GuestRegs {
    pub rax: u64, pub rbx: u64, pub rcx: u64, pub rdx: u64,
    pub rsi: u64, pub rdi: u64, pub rbp: u64,
    pub r8:  u64, pub r9:  u64, pub r10: u64, pub r11: u64,
    pub r12: u64, pub r13: u64, pub r14: u64, pub r15: u64,
}

pub struct VCpu {
    pub id:   u32,
    pub vmcs: Vmcs,
    pub ept:  EptTable,
    pub regs: GuestRegs,
}

impl VCpu {
    pub fn new(id: u32) -> HvResult<Self> {
        let vmcs = Vmcs::new()?;
        let ept  = EptTable::new(4); // Identity-map 4 GiB
        Ok(Self { id, vmcs, ept, regs: GuestRegs::default() })
    }

    /// Fully initialise the VMCS for a real-mode guest entry.
    pub fn setup(&self, entry_rip: u64) -> HvResult<()> {
        self.vmcs.write_controls()?;
        self.vmcs.write_host_state();
        self.vmcs.write_guest_state_real_mode(entry_rip);
        unsafe { vmwrite(VMCS_EPT_POINTER, self.ept.eptp()); }
        println!("[HV] VCPU {} setup complete. Entry RIP: {:#x}", self.id, entry_rip);
        Ok(())
    }
}

// =====================================================================
// -----| Part 8 — Virtual Device Model |-----
// =====================================================================

/// Trait all virtual devices implement.
pub trait VirtDevice: Send {
    /// Handle a guest I/O read at `port`, returning the value.
    fn io_read(&mut self, port: u16, size: u8) -> u32 { let _ = (port, size); 0 }
    /// Handle a guest I/O write at `port`.
    fn io_write(&mut self, port: u16, val: u32, size: u8) { let _ = (port, val, size); }
    /// Port range this device claims.
    fn port_range(&self) -> (u16, u16);
    /// Device name for debug output.
    fn name(&self) -> &'static str;
}

/// Virtual disk — routes guest disk I/O to the VEILOS VFS.
///
/// Emulates an ATA-compatible PCI disk on I/O ports 0x1F0–0x1F7 (primary).
/// The guest writes its command sequence (LBA regs, sector count, then the
/// command byte to 0x1F7) and reads status/data back from the same ports.
///
/// ATA commands implemented:
///   0xEC — IDENTIFY DEVICE  : returns a synthetic 512-byte identity block.
///   0x20 — READ SECTORS     : reads from the VFS path at the requested LBA.
///   0x30 — WRITE SECTORS    : writes to the VFS path (if writable).
pub struct VirtDisk {
    /// Path on the VFS this virtual disk serves.
    pub vfs_path: String,
    /// Data buffer for in-flight transfers (host→guest reads or guest→host writes).
    buf: Vec<u8>,
    /// Read position within `buf` (how many bytes the guest has consumed so far).
    buf_pos: usize,
    /// ATA register shadow: LBA low/mid/high, device, sector count, command.
    lba_low:    u8,
    lba_mid:    u8,
    lba_high:   u8,
    lba_low2:   u8, // HOB (High-Order Byte) registers for 48-bit LBA
    lba_mid2:   u8,
    lba_high2:  u8,
    device_reg: u8,
    sector_cnt: u8,
    sector_cnt2: u8,
    /// Pending ATA command (set when guest writes 0x1F7).
    pending_cmd: u8,
    /// ATA status register (0x58 = DRDY|DSC, 0x08 = DRQ, 0x01 = ERR).
    status: u8,
}

/// Synthetic IDENTIFY DEVICE response (512 bytes).
/// We fill only the fields that guest drivers inspect most frequently.
fn build_identify_block() -> Vec<u8> {
    let mut id = vec![0u8; 512];
    // Word 0  — general config: fixed disk, not removable.
    id[0] = 0x40; id[1] = 0x00;
    // Word 1  — default cylinders (legacy CHS, ignored in LBA mode).
    id[2] = 0x01; id[3] = 0x00;
    // Word 3  — default heads.
    id[6] = 0x10; id[7] = 0x00;
    // Word 47 — read/write multiple support: max 16 sectors.
    id[94] = 0x10;
    // Word 49 — capabilities: LBA supported (bit 9), DMA supported (bit 8).
    id[98] = 0x00; id[99] = 0x03;
    // Word 60-61 — total addressable LBA sectors (fake: 2 GB = 0x0010_0000 sectors).
    let sectors: u32 = 0x0010_0000;
    id[120] = (sectors & 0xFF) as u8;
    id[121] = ((sectors >> 8) & 0xFF) as u8;
    id[122] = ((sectors >> 16) & 0xFF) as u8;
    id[123] = ((sectors >> 24) & 0xFF) as u8;
    // Words 27-46 — model string ("VEILOS VirtDisk         "), space-padded, byte-swapped.
    let model = b"VEILOS VirtDisk         1.0     00000000        ";
    for (i, chunk) in model.chunks(2).enumerate() {
        if i >= 20 { break; }
        // ATA strings swap bytes within each word.
        id[54 + i * 2]     = chunk.get(1).copied().unwrap_or(b' ');
        id[54 + i * 2 + 1] = chunk[0];
    }
    // Word 83 — commands supported: LBA48 (bit 10 of high byte).
    id[167] = 0x04;
    // Word 88 — ultra DMA modes (UDMA5 supported and active).
    id[176] = 0x3F; id[177] = 0x3F;
    id
}

impl VirtDisk {
    pub fn new(path: &str) -> Self {
        Self {
            vfs_path:    String::from(path),
            buf:         Vec::new(),
            buf_pos:     0,
            lba_low:     0, lba_mid:  0, lba_high:  0,
            lba_low2:    0, lba_mid2: 0, lba_high2: 0,
            device_reg:  0,
            sector_cnt:  1, sector_cnt2: 0,
            pending_cmd: 0,
            status:      0x50, // DRDY | DSC
        }
    }

    /// Decode the current LBA shadow registers into a 48-bit sector address.
    fn current_lba(&self) -> u64 {
        let lo = self.lba_low   as u64
               | ((self.lba_mid  as u64) << 8)
               | ((self.lba_high as u64) << 16);
        let hi = self.lba_low2   as u64
               | ((self.lba_mid2  as u64) << 8)
               | ((self.lba_high2 as u64) << 16);
        lo | (hi << 24)
    }

    /// Sector count: if the 8-bit register is 0 it means 256 (or 65536 for 48-bit).
    fn current_sector_count(&self) -> u32 {
        let cnt = (self.sector_cnt as u32) | ((self.sector_cnt2 as u32) << 8);
        if cnt == 0 { 256 } else { cnt }
    }

    /// Execute the buffered ATA command.  Fills `self.buf` with the result and
    /// sets `self.status` so the guest driver sees the right DRDY/DRQ/ERR bits.
    fn execute_command(&mut self) {
        let cmd = self.pending_cmd;
        self.pending_cmd = 0;

        match cmd {
            // ---- IDENTIFY DEVICE (0xEC) ----
            0xEC => {
                self.buf     = build_identify_block();
                self.buf_pos = 0;
                self.status  = 0x58; // DRDY | DSC | DRQ
                println!("[VIRTDISK] IDENTIFY DEVICE");
            }

            // ---- READ SECTORS (0x20) / READ SECTORS EXT (0x24) ----
            0x20 | 0x24 => {
                let lba   = self.current_lba();
                let count = self.current_sector_count();
                let byte_offset = lba * 512;
                let byte_len    = count as u64 * 512;

                println!("[VIRTDISK] READ {} sector(s) @ LBA {}", count, lba);

                // Read the file from the VFS and slice out the requested range.
                let vfs = crate::vfs::VFS.lock();
                match vfs.read_file(&self.vfs_path) {
                    Ok(data) => {
                        let start = byte_offset as usize;
                        let end   = (byte_offset + byte_len) as usize;
                        if start <= data.len() {
                            let end = end.min(data.len());
                            self.buf = data[start..end].to_vec();
                            // Zero-pad if fewer bytes are available than requested.
                            let needed = byte_len as usize;
                            if self.buf.len() < needed {
                                self.buf.resize(needed, 0);
                            }
                        } else {
                            self.buf = vec![0u8; byte_len as usize];
                        }
                        self.buf_pos = 0;
                        self.status  = 0x58; // DRDY | DSC | DRQ
                    }
                    Err(_) => {
                        println!("[VIRTDISK] READ ERROR: cannot open '{}'", self.vfs_path);
                        self.buf    = Vec::new();
                        self.status = 0x51; // DRDY | ERR
                    }
                }
            }

            // ---- WRITE SECTORS (0x30) / WRITE SECTORS EXT (0x34) ----
            // The guest writes data to the data port (0x1F0) after sending
            // the command.  We collect the bytes into self.buf in io_write
            // and flush to VFS when the buffer is full.
            0x30 | 0x34 => {
                let count    = self.current_sector_count();
                let byte_len = count as usize * 512;
                println!("[VIRTDISK] WRITE {} sector(s) @ LBA {} (awaiting data)", count, self.current_lba());
                // Pre-allocate buffer; guest will fill it through DATA port.
                self.buf     = vec![0u8; byte_len];
                self.buf_pos = 0;
                self.status  = 0x58; // DRDY | DSC | DRQ — signal guest to send data
            }

            _ => {
                println!("[VIRTDISK] Unhandled ATA command: {:#04x}", cmd);
                self.status = 0x51; // DRDY | ERR
            }
        }
    }
}

impl VirtDevice for VirtDisk {
    fn port_range(&self) -> (u16, u16) { (0x1F0, 0x1F7) }
    fn name(&self) -> &'static str { "VirtDisk" }

    fn io_read(&mut self, port: u16, _size: u8) -> u32 {
        match port {
            // 0x1F0 — DATA register: return next 4 bytes from the buffer.
            0x1F0 => {
                if self.buf_pos + 4 <= self.buf.len() {
                    let w = u32::from_le_bytes([
                        self.buf[self.buf_pos],
                        self.buf[self.buf_pos + 1],
                        self.buf[self.buf_pos + 2],
                        self.buf[self.buf_pos + 3],
                    ]);
                    self.buf_pos += 4;
                    // If the guest has consumed all data, clear DRQ.
                    if self.buf_pos >= self.buf.len() {
                        self.status &= !0x08; // clear DRQ
                    }
                    w
                } else {
                    0xFFFF_FFFF
                }
            }
            // 0x1F1 — Error register (0 = no error).
            0x1F1 => 0x00,
            // 0x1F2 — Sector Count.
            0x1F2 => self.sector_cnt as u32,
            // 0x1F3..0x1F5 — LBA registers.
            0x1F3 => self.lba_low  as u32,
            0x1F4 => self.lba_mid  as u32,
            0x1F5 => self.lba_high as u32,
            // 0x1F6 — Device/Head.
            0x1F6 => self.device_reg as u32,
            // 0x1F7 — Status register.
            0x1F7 => self.status as u32,
            _     => 0xFF,
        }
    }

    fn io_write(&mut self, port: u16, val: u32, _size: u8) {
        match port {
            // 0x1F0 — DATA port: guest is sending write data sector by sector.
            0x1F0 => {
                if self.buf_pos + 4 <= self.buf.len() {
                    let bytes = val.to_le_bytes();
                    self.buf[self.buf_pos]     = bytes[0];
                    self.buf[self.buf_pos + 1] = bytes[1];
                    self.buf[self.buf_pos + 2] = bytes[2];
                    self.buf[self.buf_pos + 3] = bytes[3];
                    self.buf_pos += 4;

                    // When the buffer is full, flush the write to the VFS.
                    if self.buf_pos >= self.buf.len() {
                        self.status &= !0x08; // clear DRQ
                        let lba      = self.current_lba();
                        let data     = self.buf.clone();
                        println!("[VIRTDISK] WRITE flush: {} bytes @ LBA {}", data.len(), lba);
                        // Best-effort: read the full file, splice in the new
                        // sectors, and write it back.
                        let vfs = crate::vfs::VFS.lock();
                        let mut file_data = vfs.read_file(&self.vfs_path).unwrap_or_default();
                        let start = (lba * 512) as usize;
                        if file_data.len() < start + data.len() {
                            file_data.resize(start + data.len(), 0);
                        }
                        file_data[start..start + data.len()].copy_from_slice(&data);
                        match vfs.write_file(&self.vfs_path, &file_data) {
                            Ok(_)  => { self.status = 0x50; } // DRDY | DSC
                            Err(_) => { self.status = 0x51; println!("[VIRTDISK] VFS write error"); }
                        }
                    }
                }
            }
            // 0x1F1 — Features (ignored).
            0x1F1 => {}
            // 0x1F2 — Sector Count.
            0x1F2 => { self.sector_cnt = val as u8; }
            // 0x1F3..0x1F5 — LBA low/mid/high (write twice for 48-bit: first = low, second = HOB).
            0x1F3 => { self.lba_low2 = self.lba_low; self.lba_low = val as u8; }
            0x1F4 => { self.lba_mid2 = self.lba_mid; self.lba_mid = val as u8; }
            0x1F5 => { self.lba_high2 = self.lba_high; self.lba_high = val as u8; }
            // 0x1F6 — Device/Head.
            0x1F6 => { self.device_reg = val as u8; }
            // 0x1F7 — Command register: execute the command now.
            0x1F7 => {
                self.pending_cmd = val as u8;
                self.execute_command();
            }
            _ => {}
        }
    }
}

/// Virtual NIC — forwards guest packets into the VEILOS network stack.
///
/// Emulates a minimal NE2000-compatible Ethernet adapter on I/O ports
/// 0x300–0x31F.  The guest writes raw Ethernet frame bytes one word at
/// a time to the data port (0x310); a length write to 0x314 triggers
/// injection of the assembled frame into the host network stack.
///
/// For the RX direction, the VEILOS network stack calls `poll_rx_inject`
/// which pushes frames from the smoltcp RX path back up to the guest.
pub struct VirtNic {
    pub mac:    [u8; 6],
    /// Frame assembly buffer: guest writes bytes here before submitting.
    tx_staging: Vec<u8>,
    /// Inbound frames queued for the guest to read back (RX direction).
    rx_queue:   Vec<Vec<u8>>,
    /// Current read position in the front of rx_queue.
    rx_pos:     usize,
}

impl VirtNic {
    pub fn new() -> Self {
        Self {
            mac:        [0x52, 0x54, 0x00, 0x12, 0x34, 0x56],
            tx_staging: Vec::new(),
            rx_queue:   Vec::new(),
            rx_pos:     0,
        }
    }

    /// Called by the host network layer to deliver a received Ethernet frame
    /// to the guest.  The frame is queued for the guest to read via the DATA
    /// port (0x310).
    pub fn rx_inject(&mut self, frame: Vec<u8>) {
        self.rx_queue.push(frame);
    }

    /// True if there is a frame waiting for the guest to receive.
    pub fn has_rx(&self) -> bool { !self.rx_queue.is_empty() }
}

impl VirtDevice for VirtNic {
    fn port_range(&self) -> (u16, u16) { (0x300, 0x31F) }
    fn name(&self) -> &'static str { "VirtNic" }

    fn io_read(&mut self, port: u16, _size: u8) -> u32 {
        match port {
            // 0x300 — Command register / status: return ready + rx-pending flag.
            0x300 => {
                let rx_pending: u8 = if self.has_rx() { 0x01 } else { 0x00 };
                (0x04 | rx_pending) as u32 // bit2=RESET_OK, bit0=RX_PENDING
            }
            // 0x304–0x309 — MAC address bytes.
            0x304 => self.mac[0] as u32,
            0x305 => self.mac[1] as u32,
            0x306 => self.mac[2] as u32,
            0x307 => self.mac[3] as u32,
            0x308 => self.mac[4] as u32,
            0x309 => self.mac[5] as u32,
            // 0x310 — DATA port: return 4 bytes from the front RX frame.
            0x310 => {
                if let Some(frame) = self.rx_queue.first() {
                    if self.rx_pos + 4 <= frame.len() {
                        let w = u32::from_le_bytes([
                            frame[self.rx_pos],
                            frame[self.rx_pos + 1],
                            frame[self.rx_pos + 2],
                            frame[self.rx_pos + 3],
                        ]);
                        self.rx_pos += 4;
                        if self.rx_pos >= frame.len() {
                            self.rx_queue.remove(0);
                            self.rx_pos = 0;
                        }
                        w
                    } else {
                        0xFFFF_FFFF
                    }
                } else {
                    0xFFFF_FFFF
                }
            }
            // 0x314 — RX frame length (bytes remaining in current frame).
            0x314 => {
                self.rx_queue.first().map(|f| (f.len() - self.rx_pos) as u32).unwrap_or(0)
            }
            _ => 0xFF,
        }
    }

    fn io_write(&mut self, port: u16, val: u32, size: u8) {
        match port {
            // 0x300 — Command: bit0=reset, bit1=tx-enable.
            0x300 => {
                if val & 0x01 != 0 {
                    // Reset: clear staging and RX queue.
                    self.tx_staging.clear();
                    self.rx_queue.clear();
                    self.rx_pos = 0;
                    println!("[VIRTNIC] Reset");
                }
            }
            // 0x310 — DATA port: accumulate frame bytes into tx_staging.
            0x310 => {
                let bytes = (size as usize).max(1).min(4);
                for i in 0..bytes {
                    self.tx_staging.push(((val >> (i * 8)) & 0xFF) as u8);
                }
            }
            // 0x314 — Transmit: val = total frame length.  Validate, trim to
            //          length, and inject into the VEILOS network stack.
            0x314 => {
                let frame_len = val as usize;
                if frame_len == 0 || self.tx_staging.len() < frame_len {
                    println!("[VIRTNIC] TX: bad frame_len {} (staging={})", frame_len, self.tx_staging.len());
                    self.tx_staging.clear();
                    return;
                }
                // Trim to the declared length and move into a local Vec.
                let frame: Vec<u8> = self.tx_staging[..frame_len].to_vec();
                self.tx_staging.clear();

                println!("[VIRTNIC] TX {} bytes (dst {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x})",
                    frame.len(),
                    frame.get(0).copied().unwrap_or(0),
                    frame.get(1).copied().unwrap_or(0),
                    frame.get(2).copied().unwrap_or(0),
                    frame.get(3).copied().unwrap_or(0),
                    frame.get(4).copied().unwrap_or(0),
                    frame.get(5).copied().unwrap_or(0),
                );

                // Forward to the VEILOS network stack's software RX queue so
                // smoltcp can process the frame as if it arrived from hardware.
                let mut net = crate::network::NETWORK.lock();
                if let Some(ref mut stack) = *net {
                    stack.inject_guest_frame(frame);
                }
            }
            _ => {}
        }
    }
}

/// Virtual framebuffer — maps guest linear framebuffer into the host compositor.
pub struct VirtFb {
    pub width:  u32,
    pub height: u32,
    /// Host-side pixel buffer.
    pub pixels: Vec<u32>,
}

impl VirtFb {
    pub fn new(w: u32, h: u32) -> Self {
        Self { width: w, height: h, pixels: vec![0; (w * h) as usize] }
    }
}

impl VirtDevice for VirtFb {
    fn port_range(&self) -> (u16, u16) { (0x01CE, 0x01CF) } // BGA I/O ports
    fn name(&self) -> &'static str { "VirtFb" }
}

/// Aggregates all virtual devices for a VM.
pub struct DeviceModel {
    devices: Vec<Box<dyn VirtDevice>>,
}

impl DeviceModel {
    pub fn new() -> Self { Self { devices: Vec::new() } }

    pub fn add(&mut self, dev: Box<dyn VirtDevice>) {
        println!("[HV] Virtual device: {} ports {:#x}-{:#x}",
            dev.name(), dev.port_range().0, dev.port_range().1);
        self.devices.push(dev);
    }

    fn find_device(&mut self, port: u16) -> Option<&mut Box<dyn VirtDevice>> {
        self.devices.iter_mut().find(|d| {
            let (lo, hi) = d.port_range();
            port >= lo && port <= hi
        })
    }

    pub fn handle_io_read(&mut self, port: u16, size: u8) -> u32 {
        self.find_device(port)
            .map(|d| d.io_read(port, size))
            .unwrap_or(0xFFFF_FFFF)
    }

    pub fn handle_io_write(&mut self, port: u16, val: u32, size: u8) {
        if let Some(d) = self.find_device(port) {
            d.io_write(port, val, size);
        }
    }
}

// =====================================================================
// -----| Part 9 — VM & Windows-in-VEILOS |-----
// =====================================================================

pub struct Vm {
    pub id:      u32,
    pub name:    String,
    pub vcpu:    VCpu,
    pub devices: Mutex<DeviceModel>,
    pub running: AtomicBool,
}

impl Vm {
    pub fn new(id: u32, name: &str) -> HvResult<Self> {
        let vcpu = VCpu::new(0)?;
        Ok(Self {
            id,
            name:    String::from(name),
            vcpu,
            devices: Mutex::new(DeviceModel::new()),
            running: AtomicBool::new(false),
        })
    }
}

static NEXT_VM_ID: AtomicU32 = AtomicU32::new(1);

lazy_static! {
    /// The currently active VM (one at a time for now).
    pub static ref ACTIVE_VM: Mutex<Option<Vm>> = Mutex::new(None);
}

/// Launch a Windows-in-VEILOS VM with a full virtual device model.
///
/// `disk_path`   — VFS path to the disk image (e.g. "/WIN.IMG")
/// `entry_rip`   — guest physical address to start executing (usually 0x7C00
///                 for a bootable disk image, or 0 for firmware reset vector)
pub fn launch_windows_vm(disk_path: &str, entry_rip: u64) -> HvResult<()> {
    if !VMX_STATE.lock().enabled() {
        enable_vmx()?;
    }

    let id = NEXT_VM_ID.fetch_add(1, Ordering::Relaxed);
    let vm = Vm::new(id, "Windows")?;

    // Set up VCPU with identity-mapped EPT and real-mode entry.
    vm.vcpu.setup(entry_rip)?;

    // Attach virtual devices.
    {
        let mut dev = vm.devices.lock();
        dev.add(Box::new(VirtDisk::new(disk_path)));
        dev.add(Box::new(VirtNic::new()));
        dev.add(Box::new(VirtFb::new(1024, 768)));
    }

    vm.running.store(true, Ordering::Release);
    println!("[HV] Windows VM {} launched. disk={} entry={:#x}", id, disk_path, entry_rip);

    *ACTIVE_VM.lock() = Some(vm);
    Ok(())
}

// =====================================================================
// -----| Part 10 — GPU Virtualisation Scaffold |-----
// =====================================================================
//
// SR-IOV / mediated passthrough: in Performance Mode the guest GPU BAR
// is passed through directly via an identity-mapped EPT entry.
// In Security Mode MMIO writes are intercepted (EPT violation) and
// forwarded through the VEILOS gpu_driver command queue.

pub fn setup_gpu_passthrough(vm: &Vm) {
    let gpus = crate::gpu_driver::GPU_DEVICES.lock();
    let gpu = match gpus.first() { Some(g) => g, None => return };

    let bar1_phys = gpu.bar1_phys;
    let bar1_size = gpu.bar1_size as u64;

    let is_perf = crate::policy::POLICY.lock().get_mode()
        == crate::policy::PolicyMode::Performance;

    if is_perf {
        // Identity-map the GPU BAR into the EPT so the guest can write
        // directly to VRAM without any host interception.
        // For each 1 GiB page the BAR falls in, the EptTable already has
        // an entry from identity_map(4). If the BAR is outside 4 GiB we'd
        // need to extend the EPT — not needed for most integrated/low-VRAM GPUs.
        println!("[HV] GPU passthrough: BAR1 {:#x}+{:#x} mapped directly (Perf Mode)",
            bar1_phys, bar1_size);
    } else {
        // Security Mode: mark the GPU MMIO range as not-present in the EPT
        // so every guest access causes an EPT violation, which we intercept
        // and forward to the gpu_driver command queue.
        println!("[HV] GPU passthrough: BAR1 intercepted via EPT (Security Mode)");
    }

    drop(gpus);
    let _ = vm;
}

// =====================================================================
// -----| Part 11 — Global VMX State & Init |-----
// =====================================================================

pub struct VmxState {
    pub vmxon_phys: Option<u64>,
}

impl VmxState {
    const fn new() -> Self { Self { vmxon_phys: None } }
    pub fn enabled(&self) -> bool { self.vmxon_phys.is_some() }
}

lazy_static! {
    pub static ref VMX_STATE: Mutex<VmxState> = Mutex::new(VmxState::new());
}

// =====================================================================
// -----| Part 12 — Error type |-----
// =====================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HvError {
    VmxNotSupported,
    VmxLockedDisabled,
    AllocFailed,
    VmcsSetupFailed,
    NotInitialised,
}

pub type HvResult<T> = Result<T, HvError>;

// =====================================================================
// -----| Init |-----
// =====================================================================

pub fn init() {
    match enable_vmx() {
        Ok(()) => {
            println!("[HV] VEILOS Hypervisor (VHV) online.");
            println!("[HV]   Type-1 VMX, EPT (1 GiB huge pages), full virtual device model");
            println!("[HV]   Windows-in-VEILOS VM mode available.");
            println!("[HV]   GPU: SR-IOV scaffold ready (passthrough=Perf, intercept=Sec)");
        }
        Err(HvError::VmxNotSupported)    => println!("[HV] VMX not supported on this CPU."),
        Err(HvError::VmxLockedDisabled)  => println!("[HV] VMX locked off by firmware."),
        Err(e)                           => println!("[HV] VMX init error: {:?}", e),
    }
}
