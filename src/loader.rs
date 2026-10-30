use crate::vfs::VFS;
use crate::executable::PeFile;
use crate::memory;
use crate::process::{Process, PROCESSOR};
use crate::subsystem::WindowsSubsystem;
use x86_64::{VirtAddr, structures::paging::{Mapper, FrameAllocator, Size4KiB, Page, PageTableFlags}};
use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec::Vec;

// -----| per-process user-mode trampoline addresses |-----
//
// These used to be `static mut` globals written AFTER resolve_imports() had
// already linked the IAT.  That had two consequences:
//   • on the first launch they were still 0, so `_initterm` fell through to
//     the null stub and the CRT's C++ initialiser table never ran;
//   • on later launches they pointed into the *previous* process's TEB pool,
//     which reap_zombies may already have unmapped → fault during CRT init.
//
// They are now built BEFORE the IAT is linked and passed down explicitly.
#[derive(Clone, Copy, Default)]
pub struct NtEnv {
    pub teb_base:       u64,
    pub peb_base:       u64,
    pub ldr_base:       u64,
    pub params_base:    u64,
    pub initterm:       u64,
    pub initterm_e:     u64,
    pub argc_ptr:       u64,
    pub argv_ptr:       u64,
}

// -----| pe loader implementation |-----

/// Public entry point.  Wraps the real loader so that a failure part-way
/// through leaves NOTHING mapped behind.
///
/// Previously every `?` after the first `map_page` leaked the pages mapped so
/// far and left the image half-resident, which guaranteed a collision (and,
/// before the map_page fix, a kernel panic) on the next attempt.
/// Held from the start of `load_and_spawn_pe` until `on_process_exit` has
/// released the process's memory.
///
/// One Windows process at a time. Each process now has its own page tables
/// (address_space), but the syscall layer still dispatches to a single
/// "active" process (`ProcessManager::get_active`), and the user long-jump
/// context is per CPU, so a second concurrent process would receive the first
/// one's syscalls. Lifting this belongs with the Win32/EXE work.
static USER_PROCESS_SLOT: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// Load a PE into a fresh address space and queue it to run.
///
/// The whole load runs with the new process's page tables active
/// (`address_space::with`), so the loader's mappings and its writes to user
/// addresses land in that process alone, and `Process::new` records its CR3.
pub fn load_and_spawn_pe(
    path: &str,
    frame_allocator: &mut impl FrameAllocator<Size4KiB>,
) -> Result<Arc<Process>, &'static str> {
    use core::sync::atomic::Ordering;
    if USER_PROCESS_SLOT.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire).is_err() {
        crate::serial_println!("[LOADER] Refusing to load {}: another Windows process is still running", path);
        return Err("Another Windows program is still running (one at a time for now)");
    }
    let Some(cr3) = crate::address_space::create() else {
        USER_PROCESS_SLOT.store(false, Ordering::Release);
        return Err("Out of memory for page tables");
    };

    let result = crate::address_space::with(cr3, || {
        let mut mapper = unsafe { memory::get_mapper() }; // the new process's tables
        let mut scratch: Vec<u64> = Vec::new();
        let result = load_and_spawn_pe_inner(path, &mut mapper, frame_allocator, &mut scratch);
        if result.is_err() && !scratch.is_empty() {
            crate::serial_println!(
                "[LOADER] Load failed — rolling back {} mapped pages", scratch.len()
            );
            for &addr in scratch.iter() {
                let page = Page::containing_address(VirtAddr::new(addr));
                let _ = memory::unmap_and_free_page(page, &mut mapper);
            }
        }
        result
    });

    if result.is_err() {
        crate::address_space::destroy(cr3);
        USER_PROCESS_SLOT.store(false, Ordering::Release);
    }
    result
}

fn load_and_spawn_pe_inner(
    path: &str,
    mapper: &mut impl Mapper<Size4KiB>,
    frame_allocator: &mut impl FrameAllocator<Size4KiB>,
    mapped_pages: &mut Vec<u64>,
) -> Result<Arc<Process>, &'static str> {
    crate::serial_println!("\n[LOADER] >>> LOADING WINDOWS EXECUTABLE: {}", path);

    // 1. Read binary data from VFS
    let data = match VFS.read_file(path) {
        Ok(d) => d,
        Err(_) => return Err("Failed to read PE file from VFS"),
    };

    // 2. Parse PE Headers
    let pe = match PeFile::parse(&data) {
        Ok(p) => p,
        Err(e) => return Err(e),
    };
    pe.print_info();

    // IMAGE_SUBSYSTEM_WINDOWS_CUI: a console program gets a console, and a
    // terminal window to show it (apps::open_file_absolute).
    let is_console = {
        let opt = u32::from_le_bytes(data[0x3C..0x40].try_into().map_err(|_| "truncated PE")?) as usize + 24;
        data.get(opt + 68..opt + 70).map(|b| u16::from_le_bytes([b[0], b[1]])) == Some(3)
    };

    // 3. Map the EXE (headers + sections, relocated). Its DLLs are loaded and
    //    every import bound once the NT environment exists (3c below).
    let exe_entry_rva = pe.entry_point;
    let mut linker = crate::modules::Linker::new(path);
    let mapped_image_base = {
        let mut mem = crate::modules::Mem { mapper: &mut *mapper, fa: &mut *frame_allocator, pages: &mut *mapped_pages };
        linker.load_exe(&mut mem, path, &data, pe)?
    };

    // 4. Set up Stack (Small 64KB stack for now)
    // To prevent overlapping processes from hitting PageAlreadyMapped over same execution runs
    static PROC_COUNT: core::sync::atomic::AtomicU64 =
        core::sync::atomic::AtomicU64::new(0);
    let proc_idx = PROC_COUNT.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    
    let stack_base = 0x7000_0000_0000 + (proc_idx * 0x100000); // Shift each stack by 1MB
    let stack_size = 16 * 4096;
    
    for i in 0..16 {
        let page = Page::containing_address(VirtAddr::new(stack_base + (i as u64 * 4096)));
        let stack_flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::USER_ACCESSIBLE;
        let frame = memory::allocate_frame().ok_or("No frames for stack")?;
        memory::map_page(page, frame, stack_flags, mapper, frame_allocator);
        mapped_pages.push(page.start_address().as_u64());
    }
    let stack_top = stack_base + stack_size;

    // ── Windows x64 ABI: simulate a `call` to the entry point ───────────────
    //
    // On real Windows, the loader jumps to the entry point via a `call`
    // instruction, which:
    //   1. Pushes an 8-byte return address, decrementing RSP by 8.
    //   2. Leaves RSP at (16n + 8) alignment — required at every function entry
    //      by the x64 ABI (so that after the callee does `push rbp`, RSP is
    //      16-byte aligned again).
    //   3. Means the callee's four shadow-space slots live at
    //      [RSP+8] .. [RSP+32], all below the original stack_top.
    //
    // Our `iretq` sets RSP directly to stack_top (the unmapped page boundary),
    // so the very first prologue instruction — `mov [rsp+8], rcx` — writes 8
    // bytes above RSP into unmapped memory, causing an immediate page fault.
    //
    // Fix: write a NULL return address 8 bytes below stack_top to simulate the
    // `call`, then launch with RSP pointing at it.  We also subtract a further
    // 32 bytes for the four required shadow-space slots, so the full 40-byte
    // region the CRT startup touches ([RSP] .. [RSP+39]) is within the mapped
    // stack pages.
    //
    //   stack_top  (0x...10000) — unmapped, the page above
    //   stack_top - 8           ← launch RSP here, holds NULL return address
    //   stack_top - 16          \
    //   stack_top - 24           } shadow space (home params for entry fn)
    //   stack_top - 32           |
    //   stack_top - 40          /  ← lowest address touched by CRT prolog
    //   ...                     (rest of 64 KB stack, all mapped)
    //
    let launch_rsp = stack_top - 40;
    unsafe {
        // NULL return address at [launch_rsp] — the entry function will never
        // actually ret, but the slot must be accessible or the prolog faults.
        *(launch_rsp as *mut u64) = 0;
        // Zero the shadow-space slots too so stale data can't confuse the CRT.
        *((launch_rsp + 8)  as *mut u64) = 0;
        *((launch_rsp + 16) as *mut u64) = 0;
        *((launch_rsp + 24) as *mut u64) = 0;
        *((launch_rsp + 32) as *mut u64) = 0;
    }
    crate::serial_println!(
        "[LOADER] Stack: base={:#X} top={:#X} launch_rsp={:#X} (ABI-adjusted, -40 bytes)",
        stack_base, stack_top, launch_rsp
    );

    // 5. Create Process
    let pid = PROCESSOR.lock().alloc_pid();
    let mut process_inner = Process::new(pid, "WindowsApp", Box::new(WindowsSubsystem));
    // The startup routine runs TLS callbacks and DllMains, then jumps to the
    // EXE's own entry point (modules::Linker::link).
    process_inner.entry_point = crate::modules::INIT_ENTRY;
    process_inner.stack_top   = launch_rsp;  // launch RSP, not the raw top
    
    // Set up initial register state for User Mode execution
    {
        let mut reg = process_inner.registers.lock();
        reg.rip = process_inner.entry_point;
        reg.rsp = launch_rsp;  // ABI-adjusted: points at the null return address
        reg.rflags = 0x202; // IF bit set
    }
    
    process_inner.image_base = mapped_image_base;
    if is_console {
        process_inner.console = Some(Arc::new(crate::console::Console::new(pid)));
    }
    process_inner.stack_base = stack_base;
    // mapped_pages is transferred to the process at the very end of this
    // function, once nothing else can fail — until then the caller's rollback
    // list owns it.
    let process = Arc::new(process_inner);
    PROCESSOR.lock().set_active(process.clone());

    // Register the main Win32 thread with the ThreadManager so the
    // OffloadBroker can enumerate it and evaluate best-effort eligibility.
    {
        use crate::process::{THREAD_MANAGER, ThreadSubsystem};
        let node_caps = crate::distributed::CAP_EXEC;
        let short_name = path.split('/').last().unwrap_or(path);
        THREAD_MANAGER.lock().spawn_worker(
            pid,
            short_name,
            ThreadSubsystem::Win32,
            process.entry_point,
            0x8000,
            node_caps,
        );
    }

    // Notify the scheduler (Adaptive Scheduling)
    crate::scheduler::SCHEDULER.lock().on_process_launch(pid, path, crate::interrupts::TICKS.load(core::sync::atomic::Ordering::Relaxed));

    // 7. Allocate a minimal TEB (Thread Environment Block) + fake PEB so that
    //    MSVC CRT startup code using gs:[offset] lands in valid memory instead
    //    of reading/crashing at raw kernel addresses.
    //
    //    Layout (two contiguous pages):
    //      teb_base + 0x0000 : TEB  (Thread Environment Block)
    //      teb_base + 0x1000 : PEB  (Process Environment Block, minimal stub)
    //
    //    Key TEB fields (64-bit offsets, all others zeroed):
    //      0x00  ExceptionList  = 0xFFFFFFFFFFFFFFFF  (end-of-chain sentinel)
    //      0x08  StackBase      = stack_top           (top / high address)
    //      0x10  StackLimit     = stack_base          (bottom / low address)
    //      0x30  Self           = teb_base            (NtCurrentTeb())
    //      0x40  PID
    //      0x48  TID = 1
    //      0x60  PEB pointer    = teb_base + 0x1000

    // -----| NT Environment Setup |-----
    //
    // Allocate 5 pages for the per-process NT environment:
    //   Page 0  (teb_base + 0x0000) : TEB  (Thread Environment Block)
    //   Page 1  (teb_base + 0x1000) : PEB  (Process Environment Block)
    //   Page 2  (teb_base + 0x2000) : PEB_LDR_DATA + RTL_USER_PROCESS_PARAMETERS
    //   Page 3  (teb_base + 0x3000) : _initterm user-mode trampoline code
    //   Page 4  (teb_base + 0x4000) : __argc/__argv stubs (static data)
    //
    // Additionally, a shared KUSER_SHARED_DATA page is mapped at the canonical
    // Windows address (0x7FFE_0000) if not already present.

    // The pool only ever moves forward.  That is deliberate: reusing a VA
    // while a previous process still has it mapped is exactly the class of bug
    // that made relaunching an app unpredictable.  Frames are reclaimed on
    // teardown (see memory::deallocate_frame); only the 48-bit address space
    // is consumed, at 20 KiB per launch.
    static TEB_POOL_BASE: core::sync::atomic::AtomicU64 =
        core::sync::atomic::AtomicU64::new(0x7FF_0000_0000);
    let teb_base = TEB_POOL_BASE.fetch_add(0x5000, core::sync::atomic::Ordering::Relaxed);

    for page_offset in [0x0000u64, 0x1000u64, 0x2000u64, 0x3000u64, 0x4000u64] {
        let page  = Page::containing_address(VirtAddr::new(teb_base + page_offset));
        let frame = memory::allocate_frame().ok_or("No frame for TEB/PEB")?;
        memory::map_page(
            page, frame,
            PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::USER_ACCESSIBLE,
            mapper, frame_allocator,
        );
        mapped_pages.push(page.start_address().as_u64());
        unsafe { core::ptr::write_bytes((teb_base + page_offset) as *mut u8, 0, 4096); }
    }

    let peb_base = teb_base + 0x1000;
    let ldr_base = teb_base + 0x2000;  // PEB_LDR_DATA
    let params_base = ldr_base + 0x100; // RTL_USER_PROCESS_PARAMETERS (same page, offset 0x100)
    let initterm_code_base = teb_base + 0x3000; // user-mode _initterm code
    let argv_data_base = teb_base + 0x4000; // __argc/__argv data page

    // -----| TEB fields |-----
    unsafe {
        let b = teb_base as *mut u64;
        *b.add(0)  = 0xFFFF_FFFF_FFFF_FFFFu64;  // 0x00: ExceptionList end sentinel
        *b.add(1)  = stack_top;                   // 0x08: StackBase  (high address)
        *b.add(2)  = stack_base;                  // 0x10: StackLimit (low address)
        *b.add(6)  = teb_base;                    // 0x30: Self (NtCurrentTeb returns this)
        *b.add(8)  = pid as u64;                  // 0x40: ClientId.UniqueProcess
        *b.add(9)  = 1u64;                        // 0x48: ClientId.UniqueThread
        *b.add(12) = peb_base;                    // 0x60: ProcessEnvironmentBlock pointer
    }

    // -----| PEB fields |-----
    //   0x02 (u8) : BeingDebugged = 0
    //   0x08      : Mutant = -1 (standard)
    //   0x10      : ImageBaseAddress
    //   0x18      : Ldr pointer → PEB_LDR_DATA
    //   0x20      : ProcessParameters pointer → RTL_USER_PROCESS_PARAMETERS
    //   0x28      : SubSystemData = 0
    //   0x30      : ProcessHeap (fake heap handle)
    //   0x78 (u32): NtGlobalFlag = 0
    //   0x118 (u32): OSMajorVersion = 10
    //   0x11C (u32): OSMinorVersion = 0
    //   0x120 (u16): OSBuildNumber = 19041
    //   0x122 (u16): OSPlatformId = 2 (VER_PLATFORM_WIN32_NT)
    unsafe {
        let p = peb_base as *mut u8;
        // 0x08: Mutant = -1
        *(p.add(0x08) as *mut u64) = 0xFFFF_FFFF_FFFF_FFFFu64;
        // 0x10: ImageBaseAddress
        *(p.add(0x10) as *mut u64) = mapped_image_base;
        // 0x18: Ldr → PEB_LDR_DATA
        *(p.add(0x18) as *mut u64) = ldr_base;
        // 0x20: ProcessParameters
        *(p.add(0x20) as *mut u64) = params_base;
        // 0x30: ProcessHeap (fake heap handle — non-null so HeapAlloc checks pass)
        *(p.add(0x30) as *mut u64) = 0x1000_0000u64;
        // Version info
        *(p.add(0x118) as *mut u32) = 10;      // OSMajorVersion
        *(p.add(0x11C) as *mut u32) = 0;       // OSMinorVersion
        *(p.add(0x120) as *mut u16) = 19041;   // OSBuildNumber
        *(p.add(0x122) as *mut u16) = 2;       // OSPlatformId = VER_PLATFORM_WIN32_NT
    }

    // -----| PEB_LDR_DATA |-----
    //   0x00 (u32): Length = 0x30 (standard size)
    //   0x04 (u32): Initialized = 1
    //   0x10       : InLoadOrderModuleList (LIST_ENTRY — circular, self-linked = empty)
    //   0x20       : InMemoryOrderModuleList (LIST_ENTRY)
    //   0x30       : InInitializationOrderModuleList (LIST_ENTRY)
    unsafe {
        let l = ldr_base as *mut u8;
        *(l.add(0x00) as *mut u32) = 0x30;
        *(l.add(0x04) as *mut u32) = 1;
        // Self-linked circular LIST_ENTRY means "empty list"
        let in_load   = ldr_base + 0x10;
        let in_mem    = ldr_base + 0x20;
        let in_init   = ldr_base + 0x30;
        *(l.add(0x10) as *mut u64) = in_load;   // Flink = self
        *(l.add(0x18) as *mut u64) = in_load;   // Blink = self
        *(l.add(0x20) as *mut u64) = in_mem;
        *(l.add(0x28) as *mut u64) = in_mem;
        *(l.add(0x30) as *mut u64) = in_init;
        *(l.add(0x38) as *mut u64) = in_init;
    }

    // -----| RTL_USER_PROCESS_PARAMETERS |-----
    //   0x00 (u32): MaximumLength = 0x100
    //   0x04 (u32): Length = 0x100
    //   0x60       : ImagePathName (UNICODE_STRING — Length, MaxLength, Buffer)
    //   0x70       : CommandLine   (UNICODE_STRING)
    // We place the actual string data at params_base + 0xC0.
    unsafe {
        let pp = params_base as *mut u8;
        *(pp.add(0x00) as *mut u32) = 0x100;
        *(pp.add(0x04) as *mut u32) = 0x100;

        // ImagePathName: L"C:\\VEILAPP.EXE\0"
        let img_str: [u16; 15] = [
            b'C' as u16, b':' as u16, b'\\' as u16,
            b'V' as u16, b'E' as u16, b'I' as u16, b'L' as u16,
            b'A' as u16, b'P' as u16, b'P' as u16,
            b'.' as u16, b'E' as u16, b'X' as u16, b'E' as u16,
            0,
        ];
        let str_buf = params_base + 0xC0;
        core::ptr::copy_nonoverlapping(img_str.as_ptr(), str_buf as *mut u16, img_str.len());
        // UNICODE_STRING at 0x60: Length (bytes), MaxLength, Buffer
        *(pp.add(0x60) as *mut u16) = (14 * 2) as u16; // Length (bytes, excl NUL)
        *(pp.add(0x62) as *mut u16) = (15 * 2) as u16; // MaximumLength
        *(pp.add(0x68) as *mut u64) = str_buf;          // Buffer pointer

        // CommandLine: same string for simplicity
        *(pp.add(0x70) as *mut u16) = (14 * 2) as u16;
        *(pp.add(0x72) as *mut u16) = (15 * 2) as u16;
        *(pp.add(0x78) as *mut u64) = str_buf;
    }

    // Standard handles (StandardInput/Output/Error): a console program's all
    // refer to its console; a GUI program has none (0), as on Windows.
    if let Some(console) = &process.console {
        use nt_abi::layout::{PARAMS_STD_ERROR, PARAMS_STD_INPUT, PARAMS_STD_OUTPUT};
        let mut handles = process.handles.lock();
        for offset in [PARAMS_STD_INPUT, PARAMS_STD_OUTPUT, PARAMS_STD_ERROR] {
            let h = handles.allocate_ref(crate::nt_objects::KernelObjectRef::Console(console.clone()), 0x001F_01FF, 0);
            unsafe { *((params_base as usize + offset) as *mut u64) = h as u64 };
        }
    }

    // -----| KUSER_SHARED_DATA (0x7FFE_0000) |-----
    // This is a system-wide page. Map it once if not already done.
    static KUSD_MAPPED: core::sync::atomic::AtomicBool =
        core::sync::atomic::AtomicBool::new(false);
    if !KUSD_MAPPED.load(core::sync::atomic::Ordering::Acquire) {
        let kusd_addr: u64 = 0x7FFE_0000;
        let kusd_page = Page::containing_address(VirtAddr::new(kusd_addr));
        let kusd_frame = memory::allocate_frame().ok_or("No frame for KUSER_SHARED_DATA")?;
        memory::map_page(
            kusd_page, kusd_frame,
            PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::USER_ACCESSIBLE,
            mapper, frame_allocator,
        );
        // DELIBERATELY NOT added to mapped_pages / the rollback list.
        //
        // KUSER_SHARED_DATA is system-wide, not per-process.  It used to be
        // recorded as process 1's page, so reap_zombies unmapped it when that
        // process died — while KUSD_MAPPED stayed true, so it was never
        // remapped.  Every subsequent launch then faulted the moment the CRT
        // touched 0x7FFE0000.
        unsafe {
            let k = kusd_addr as *mut u8;
            core::ptr::write_bytes(k, 0, 4096);
            
            // 0x000: TickCountLowDeprecated
            core::ptr::write_unaligned(k.add(0x000) as *mut u32, 0x0001_0000);
            // 0x004: TickCountMultiplier = 0x0FA0_0000 (standard 15.625 ms)
            core::ptr::write_unaligned(k.add(0x004) as *mut u32, 0x0FA0_0000);
            
            // 0x008: InterruptTime (KSYSTEM_TIME) - Note: offset 0x008, 0x00C, 0x010
            core::ptr::write_unaligned(k.add(0x008) as *mut u32, 0x0001_0000);
            core::ptr::write_unaligned(k.add(0x00C) as *mut u32, 0);
            core::ptr::write_unaligned(k.add(0x010) as *mut u32, 0);

            // 0x014: SystemTime (KSYSTEM_TIME) - Note: offset 0x014, 0x018, 0x01C
            // This is what was causing the alignment panic (0x14 is not 8-aligned for u64).
            core::ptr::write_unaligned(k.add(0x014) as *mut u32, 0x12345678); // low
            core::ptr::write_unaligned(k.add(0x018) as *mut u32, 0x00000000); // high1
            core::ptr::write_unaligned(k.add(0x01C) as *mut u32, 0x00000000); // high2

            // 0x260: NtMajorVersion / NtMinorVersion
            core::ptr::write_unaligned(k.add(0x260) as *mut u32, 10);
            core::ptr::write_unaligned(k.add(0x264) as *mut u32, 0);

            // 0x26C: NtProductType = 1 (WinNt)
            core::ptr::write_unaligned(k.add(0x26C) as *mut u32, 1);

            // 0x2D0: Cookie (security cookie seed)
            core::ptr::write_unaligned(k.add(0x2D0) as *mut u64, 0xBAAD_F00D_CAFE_BEEFu64);

            // 0x320: TickCount (KSYSTEM_TIME)
            core::ptr::write_unaligned(k.add(0x320) as *mut u32, 0x0001_0000);
            core::ptr::write_unaligned(k.add(0x324) as *mut u32, 0);
            core::ptr::write_unaligned(k.add(0x328) as *mut u32, 0);

        }
        KUSD_MAPPED.store(true, core::sync::atomic::Ordering::Release);
        crate::serial_println!("[LOADER] KUSER_SHARED_DATA mapped at {:#X}", kusd_addr);
    }

    // -----| _initterm user-mode trampoline |-----
    // Writes a small piece of x86_64 machine code at initterm_code_base that
    // implements _initterm(PVFV *begin, PVFV *end) in user mode:
    //
    //   _initterm:                   ; rcx=begin, rdx=end
    //       push rbx
    //       push rsi
    //       mov  rsi, rcx            ; rsi = begin
    //       mov  rbx, rdx            ; rbx = end
    //   .loop:
    //       cmp  rsi, rbx
    //       jae  .done
    //       mov  rax, [rsi]
    //       test rax, rax
    //       jz   .skip
    //       call rax
    //   .skip:
    //       add  rsi, 8
    //       jmp  .loop
    //   .done:
    //       pop  rsi
    //       pop  rbx
    //       ret
    //
    //   _initterm_e:                 ; rcx=begin, rdx=end (returns 0)
    //       push rbx
    //       push rsi
    //       mov  rsi, rcx
    //       mov  rbx, rdx
    //   .loop_e:
    //       cmp  rsi, rbx
    //       jae  .done_e
    //       mov  rax, [rsi]
    //       test rax, rax
    //       jz   .skip_e
    //       call rax
    //   .skip_e:
    //       add  rsi, 8
    //       jmp  .loop_e
    //   .done_e:
    //       xor  eax, eax
    //       pop  rsi
    //       pop  rbx
    //       ret
    unsafe {
        let c = initterm_code_base as *mut u8;

        // Correct bytecode for _initterm(PVFV *begin, PVFV *end)
        let initterm_fn: [u8; 31] = [
            0x53,                               // 0: push rbx
            0x56,                               // 1: push rsi
            0x48, 0x89, 0xCE,                   // 2: mov rsi, rcx
            0x48, 0x89, 0xD3,                   // 5: mov rbx, rdx
            // .loop @ offset 8:
            0x48, 0x39, 0xDE,                   // 8:  cmp rsi, rbx
            0x73, 0x10,                         // 11: jae .done (+16, target=29)
            0x48, 0x8B, 0x06,                   // 13: mov rax, [rsi]
            0x48, 0x85, 0xC0,                   // 16: test rax, rax
            0x74, 0x02,                         // 19: jz .skip (+2, target=23)
            0xFF, 0xD0,                         // 21: call rax
            // .skip @ offset 23:
            0x48, 0x83, 0xC6, 0x08,             // 23: add rsi, 8
            0xEB, 0xEB,                         // 27: jmp .loop (-21, target=8)
            // .done @ offset 29:
            0x5E,                               // 29: pop rsi
            0x5B,                               // 30: pop rbx
        ];
        core::ptr::copy_nonoverlapping(initterm_fn.as_ptr(), c, initterm_fn.len());
        core::ptr::write_volatile(c.add(31), 0xC3); // ret

        // _initterm_e at offset 0x40 (same logic, but xor eax before ret)
        core::ptr::copy_nonoverlapping(initterm_fn.as_ptr(), c.add(0x40), initterm_fn.len());
        core::ptr::write_volatile(c.add(0x40 + 29), 0x5E); // pop rsi
        core::ptr::write_volatile(c.add(0x40 + 30), 0x5B); // pop rbx
        core::ptr::write_volatile(c.add(0x40 + 31), 0x31); // xor eax, eax
        core::ptr::write_volatile(c.add(0x40 + 32), 0xC0);
        core::ptr::write_volatile(c.add(0x40 + 33), 0xC3); // ret
    }

    // -----| __p___argc / __p___argv data stubs |-----
    // __p___argc returns a pointer to an int containing argc (= 1)
    // __p___argv returns a pointer to a char** containing a single empty arg
    unsafe {
        let d = argv_data_base as *mut u8;
        // offset 0x00: argc = 1
        *(d.add(0x00) as *mut i32) = 1;
        // offset 0x08: argv[0] → pointer to the string at offset 0x20
        *(d.add(0x08) as *mut u64) = argv_data_base + 0x20;
        // offset 0x10: argv[1] = NULL (terminator)
        *(d.add(0x10) as *mut u64) = 0;
        // offset 0x20: "VEILAPP\0"
        let name = b"VEILAPP\0";
        core::ptr::copy_nonoverlapping(name.as_ptr(), d.add(0x20), name.len());
    }

    // Collect everything the IAT linker needs into a per-process value.
    let env = NtEnv {
        teb_base,
        peb_base,
        ldr_base,
        params_base,
        initterm:   initterm_code_base,
        initterm_e: initterm_code_base + 0x40,
        argc_ptr:   argv_data_base,          // &argc
        argv_ptr:   argv_data_base + 0x08,   // &argv
    };
    *process.nt_env.lock() = env;

    crate::serial_println!("[LOADER] TEB={:#X}  PEB={:#X}  Ldr={:#X}  KUSD=0x7FFE0000", teb_base, peb_base, ldr_base);
    crate::serial_println!("[LOADER] _initterm={:#X}  _initterm_e={:#X}", initterm_code_base, initterm_code_base + 0x40);

    // ── 3c. Load DLLs and bind every import ─────────────────────────────────
    // Runs HERE, after the NT environment is built: `_initterm`/`_initterm_e`
    // link against this process's own trampoline page, and TLS and PEB.Ldr
    // need the TEB and PEB. Also protects each section (read-only, NX).
    linker.set_user_mode_overrides(crate::modules::UserModeOverrides {
        initterm: env.initterm,
        initterm_e: env.initterm_e,
    });
    {
        let mut mem = crate::modules::Mem { mapper: &mut *mapper, fa: &mut *frame_allocator, pages: &mut *mapped_pages };
        linker.link(&mut mem, teb_base, peb_base, ldr_base)?;
    }
    // VirtualAlloc must never hand out what the loader mapped.
    {
        let mut vad = process.vad.lock();
        for (base, size) in linker.regions() {
            vad.record_image(base, size);
        }
    }
    *process.unresolved_imports.lock() = linker.unresolved.iter().map(|n| (n.clone(), false)).collect();

    // Everything that can fail has now succeeded — hand ownership of the page
    // list to the process so teardown (not rollback) reclaims it.
    {
        let mut owned = process.mapped_pages.lock();
        owned.clear();
        owned.extend_from_slice(mapped_pages);
    }
    mapped_pages.clear();

    // 8. Spawn the Process Execution Task onto the Async Executor.
    crate::serial_println!("[LOADER] Spawning Execution Fiber for Pid={}...", pid);
    crate::vnr::spawn_fiber(process_execution_task(process.clone(), teb_base));

    crate::serial_println!("[LOADER] Successfully prepared Windows executable at {:#X}", mapped_image_base + exe_entry_rva);
    Ok(process)
}

async fn process_execution_task(process: Arc<Process>, teb_base: u64) {
    crate::serial_println!(
        "[SCHED] Launching Pid={} ({}) → entry={:#X} stack={:#X} teb={:#X}",
        process.pid, process.name, process.entry_point, process.stack_top, teb_base
    );
    crate::task::yield_now().await;
    // Signal to the timer ISR that a user process is now running so it
    // takes over display duties from gui_refresh_task.
    crate::interrupts::USER_PROCESS_ACTIVE.store(true, core::sync::atomic::Ordering::Relaxed);

    if crate::smp::CPU_COUNT.load(core::sync::atomic::Ordering::Relaxed) > 1 {
        // The AP runs it and calls on_process_exit() itself when it returns.
        // The pid travels with the work item so the AP can find the right
        // process — previously the SMP path did NO cleanup at all.
        crate::smp::AP_WORK_QUEUE.lock().push(crate::smp::ApWorkItem {
            teb_base,
            pid: process.pid,
        });
        crate::smp::wake_aps();
        return;
    }

    // Single CPU: run the process on its own kernel thread. This task returns
    // at once, so the executor (GUI, input, network) keeps running; the timer
    // switches between the two (see kthread). It used to call
    // launch_into_usermode right here, which froze the executor until the
    // process exited and left the timer ISR painting the desktop.
    spawn_user_thread(process, teb_base);
}

/// Run `process` on a new kernel thread on this CPU, in its own address
/// space; the thread tears the process down when it stops. Used by the BSP
/// (single CPU) and by each AP's worker loop (`smp::park_and_wait_for_work`).
pub fn spawn_user_thread(process: Arc<Process>, teb_base: u64) {
    let cpu = crate::smp::cpu_index();
    let pid = process.pid;
    let cr3 = process.cr3;
    crate::smp::USER_THREAD_ON[cpu].store(true, core::sync::atomic::Ordering::Release);
    crate::kthread::spawn_in("user-process", cr3, move || {
        unsafe { crate::loader::launch_into_usermode(process.entry_point, process.stack_top, teb_base); }

        // ── Post-exit ────────────────────────────────────────────────────
        //
        // return_from_usermode() longjmps here from either:
        //   • ExitProcess syscall handler  (SYSCALL clears IF via SFMASK)
        //   • Page fault / GPF handler     (exception entry clears IF)
        //
        // In BOTH cases the CPU's interrupt flag is still 0 because we
        // bypassed the normal iretq that would restore RFLAGS.
        unsafe { core::arch::asm!("sti"); }

        on_process_exit(&process);

        let cpu = crate::smp::cpu_index();
        crate::smp::USER_THREAD_ON[cpu].store(false, core::sync::atomic::Ordering::Release);
        crate::serial_println!("[SCHED] Process {} completed execution on CPU {}.", process.pid, cpu);
    });
    crate::serial_println!("[SCHED] Pid={} runs on a kernel thread (CPU {})", pid, cpu);
}

/// Everything that must happen once a user process stops running, regardless
/// of *how* it stopped (ExitProcess, page fault, GPF, #UD, ...) and regardless
/// of which CPU it was running on.
///
/// This used to live inline in the single-CPU branch of
/// `process_execution_task`, so on an SMP boot none of it ran: windows were
/// never destroyed, the process was never marked dead, USER_PROCESS_ACTIVE
/// stayed true and no memory was ever reclaimed.
pub fn on_process_exit(process: &Arc<Process>) {
    // 0. Release any spinlock the long-jump skipped over.  See
    //    lock_guard::release_all_for_cpu() for why this exists.
    crate::loader::release_leaked_locks();

    crate::interrupts::USER_PROCESS_ACTIVE.store(false, core::sync::atomic::Ordering::Relaxed);

    // Invalidate this CPU's long-jump slot: with the process gone, a stray
    // ring-3 fault must NOT jump back onto a stack frame that no longer exists.
    invalidate_exec_context();

    // 1. Mark dead (idempotent — the fault handler / ExitProcess may have
    //    already done it).
    if process.is_alive() {
        process.terminate(0);
    }

    // 2. Tear down Win32 windows owned by this process so the desktop
    //    doesn't keep rendering stale frames.
    //
    //    try_lock, not lock: if a syscall handler died holding WIN32_USER we
    //    must not spin here forever.  release_leaked_locks() above should have
    //    freed it, but belt and braces.
    if let Some(mut w32) = crate::win32_user::WIN32_USER.try_lock() {
        let owned: alloc::vec::Vec<u32> = w32.windows.iter()
            .filter(|w| w.owning_pid == process.pid as u32)
            .map(|w| w.hwnd)
            .collect();
        for hwnd in owned {
            w32.destroy_window(hwnd);
        }
    } else {
        crate::serial_println!("[EXIT] WIN32_USER busy — window teardown deferred");
    }

    // 3. Drop the process's worker threads.
    if let Some(mut tm) = crate::process::THREAD_MANAGER.try_lock() {
        tm.remove_threads_for_pid(process.pid);
    }

    // 4. Tell its console window, then close its NT handles and VADs.
    if let Some(console) = &process.console {
        console.finish(*process.exit_code.lock());
    }
    if let Some(mut h) = process.handles.try_lock() { h.close_all(); }

    // 5. Re-focus the boot terminal so keyboard/mouse input is routed to a
    //    live kernel window rather than landing on the Settings window.
    let term_vid = crate::apps::TERMINAL_POOL[0].try_lock()
        .map(|t| t.window_id)
        .unwrap_or(0);
    if term_vid != 0 {
        if let Some(mut wm) = crate::desktop::DESKTOP_WM.try_lock() {
            if wm.windows.iter().any(|w| w.id == term_vid) {
                wm.focus(term_vid);
            }
        }
    }

    // 6. Release the address space.  reap_zombies() will also run, but doing
    //    it here means the frames are back on the free list immediately —
    //    relaunching the same EXE no longer has to wait up to a second for
    //    the reaper, and no longer collides with its own stale mapping.
    release_process_memory(process);

    // 7. Drop the page tables. release_process_memory needed them active; now
    //    move this CPU (and this thread's record) back to the kernel's tables.
    crate::kthread::set_current_cr3(0);
    crate::address_space::activate(0);
    crate::address_space::destroy(process.cr3);
    USER_PROCESS_SLOT.store(false, core::sync::atomic::Ordering::Release);

    // Reset LAST_USER_MODE_TICK so the timer ISR's recently_in_user_mode
    // grace period ends immediately, preventing the ISR from painting
    // alongside gui_refresh_task after clean process exit.
    crate::interrupts::LAST_USER_MODE_TICK.store(0, core::sync::atomic::Ordering::Relaxed);

    crate::serial_println!(
        "[EXIT] Pid={} torn down. {} frames on free list.",
        process.pid, crate::memory::free_frame_count()
    );
}

/// Unmap every page the process owns and return the frames to the allocator.
pub fn release_process_memory(process: &Arc<Process>) {
    let pages: alloc::vec::Vec<u64> = {
        let mut owned = match process.mapped_pages.try_lock() {
            Some(g) => g,
            None => return,
        };
        core::mem::take(&mut *owned)
    };
    if pages.is_empty() { return; }

    let mut mapper = unsafe { memory::get_mapper() };
    let mut freed = 0usize;
    for addr in pages {
        let page = Page::containing_address(VirtAddr::new(addr));
        if memory::unmap_and_free_page(page, &mut mapper).is_ok() { freed += 1; }
    }
    crate::serial_println!("[EXIT] Released {} pages for pid {}", freed, process.pid);
}

// =====================================================================
// -----| Ring-3 launch / long-jump context |-----
// =====================================================================
//
// One saved kernel context per CPU.  launch_into_usermode() fills it in;
// return_from_usermode() jumps back through it from a syscall or a fault.
//
// The slot is now guarded by a validity flag.  Without it, a stray ring-3
// fault at a moment when no process was live (a stale slot from a launch
// that had already returned, or a fault on a CPU that never launched
// anything) would `jmp` onto a dead stack frame and corrupt the kernel in
// an unattributable way.

pub const MAX_EXEC_CPUS: usize = 8;

#[repr(C, align(64))]
pub struct ExecContext {
    pub regs: [u64; 8],
}

struct ExecContexts(core::cell::UnsafeCell<[ExecContext; MAX_EXEC_CPUS]>);
unsafe impl Sync for ExecContexts {}

static EXEC_CONTEXTS: ExecContexts = ExecContexts(core::cell::UnsafeCell::new(
    [const { ExecContext { regs: [0; 8] } }; MAX_EXEC_CPUS]
));

static EXEC_CTX_VALID: [core::sync::atomic::AtomicBool; MAX_EXEC_CPUS] =
    [const { core::sync::atomic::AtomicBool::new(false) }; MAX_EXEC_CPUS];

#[inline]
pub fn current_exec_cpu() -> usize {
    // Dense CPU index, not the raw APIC ID: APIC IDs can be sparse or
    // exceed MAX_EXEC_CPUS on real hardware, and `id % 8` made CPUs collide.
    crate::smp::cpu_index().min(MAX_EXEC_CPUS - 1)
}

#[inline]
fn exec_ctx_ptr(cpu: usize) -> *mut u64 {
    unsafe { (*EXEC_CONTEXTS.0.get())[cpu].regs.as_mut_ptr() }
}

/// Clear this CPU's long-jump slot once the process it belonged to is gone.
pub fn invalidate_exec_context() {
    EXEC_CTX_VALID[current_exec_cpu()]
        .store(false, core::sync::atomic::Ordering::Release);
}

/// True if this CPU currently has a live launch context to return to.
pub fn has_exec_context() -> bool {
    EXEC_CTX_VALID[current_exec_cpu()]
        .load(core::sync::atomic::Ordering::Acquire)
}

/// Long-jump out of ring-3 back into the kernel context that launched it.
///
/// Callers are fault handlers and ExitProcess.  Because this is a raw `jmp`
/// that unwinds nothing, every spinlock held at this instant would stay
/// locked forever — so we force-release the tracked ones first.  See
/// `lock_guard`.
/// Set when the running Windows process must be ended from outside
/// (`request_kill`). One Windows process runs at a time, so any ring-3 code
/// a timer interrupts belongs to it.
static KILL_REQUESTED: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// End the running Windows process the next time a timer interrupt finds it
/// in user mode (within a tick). Its exit code must already be set.
pub fn request_kill() {
    KILL_REQUESTED.store(true, core::sync::atomic::Ordering::Release);
}

/// Timer ISRs call this when they interrupted ring 3: carries out a pending
/// `request_kill` the way ExitProcess would. Returns if nothing is pending.
pub fn kill_if_requested() {
    if KILL_REQUESTED.swap(false, core::sync::atomic::Ordering::AcqRel) {
        crate::interrupts::USER_PROCESS_ACTIVE.store(false, core::sync::atomic::Ordering::Relaxed);
        unsafe { return_from_usermode() }
    }
}

pub unsafe fn return_from_usermode() -> ! {
    let cpu_id = current_exec_cpu();

    // Release anything the abandoned kernel frame was holding.  This is the
    // fix for "app crashes → desktop freezes": a bad user pointer inside a
    // syscall used to leave WIN32_USER (or PROCESSOR, or the heap) locked.
    lock_guard::release_all_for_cpu(cpu_id);

    if !EXEC_CTX_VALID[cpu_id].load(core::sync::atomic::Ordering::Acquire) {
        // Nothing to return to.  Jumping would be worse than stopping.
        crate::serial_println!(
            "[LOADER] return_from_usermode() on CPU {} with no valid context — halting CPU",
            cpu_id
        );
        crate::interrupts::USER_PROCESS_ACTIVE
            .store(false, core::sync::atomic::Ordering::Relaxed);
        unsafe { core::arch::asm!("sti") };
        loop { x86_64::instructions::hlt(); }
    }

    // Consume the context: a second fault must not reuse it.
    EXEC_CTX_VALID[cpu_id].store(false, core::sync::atomic::Ordering::Release);

    unsafe {
        let ctx_ptr = exec_ctx_ptr(cpu_id);
        core::arch::asm!(
            "mov rbx, [rdi + 0]",
            "mov rbp, [rdi + 8]",
            "mov r12, [rdi + 16]",
            "mov r13, [rdi + 24]",
            "mov r14, [rdi + 32]",
            "mov r15, [rdi + 40]",
            "mov rsp, [rdi + 48]",
            "mov rcx, [rdi + 56]",
            "jmp rcx",
            in("rdi") ctx_ptr,
            options(noreturn)
        );
    }
}

/// Force-release locks abandoned by a long-jump on the current CPU.
pub fn release_leaked_locks() {
    lock_guard::release_all_for_cpu(current_exec_cpu());
}

// =====================================================================
// -----| lock_guard: spinlocks that survive a long-jump |-----
// =====================================================================
//
// `return_from_usermode()` is a raw `jmp` — no unwinding, no Drop.  Any
// spin::Mutex held by the syscall handler at that moment stays locked for
// the rest of the boot.  In practice that meant one bad pointer from a
// Win32 app permanently wedged WIN32_USER and the desktop stopped drawing
// windows (or the timer ISR spun on it with interrupts off, which looks
// like a total freeze).
//
// Syscall handlers take their locks through `lock_tracked()`.  The guard
// registers a force-unlock thunk in a small per-CPU table and deregisters
// it on normal Drop.  If the frame is abandoned instead, the long-jump
// path walks the table and releases everything in reverse order.
pub mod lock_guard {
    use core::sync::atomic::{AtomicUsize, Ordering};

    const MAX_TRACKED: usize = 8;

    #[derive(Clone, Copy)]
    struct Entry {
        ptr:      usize,
        unlocker: Option<unsafe fn(usize)>,
    }

    struct Table(core::cell::UnsafeCell<[[Entry; MAX_TRACKED]; super::MAX_EXEC_CPUS]>);
    unsafe impl Sync for Table {}

    static TABLE: Table = Table(core::cell::UnsafeCell::new(
        [[Entry { ptr: 0, unlocker: None }; MAX_TRACKED]; super::MAX_EXEC_CPUS]
    ));

    static DEPTH: [AtomicUsize; super::MAX_EXEC_CPUS] =
        [const { AtomicUsize::new(0) }; super::MAX_EXEC_CPUS];

    unsafe fn unlock_thunk<T>(ptr: usize) {
        let m = unsafe { &*(ptr as *const spin::Mutex<T>) };
        unsafe { m.force_unlock() };
    }

    /// RAII guard that both dereferences like a MutexGuard and keeps the
    /// lock in the per-CPU release table.
    pub struct TrackedGuard<'a, T: 'a> {
        guard: Option<spin::MutexGuard<'a, T>>,
        cpu:   usize,
        slot:  usize,
    }

    impl<'a, T> core::ops::Deref for TrackedGuard<'a, T> {
        type Target = T;
        fn deref(&self) -> &T { self.guard.as_ref().unwrap() }
    }
    impl<'a, T> core::ops::DerefMut for TrackedGuard<'a, T> {
        fn deref_mut(&mut self) -> &mut T { self.guard.as_mut().unwrap() }
    }

    impl<'a, T> Drop for TrackedGuard<'a, T> {
        fn drop(&mut self) {
            // Deregister first, then release: if we are unwinding normally
            // the entry must not outlive the guard.
            let depth = DEPTH[self.cpu].load(Ordering::Acquire);
            if depth == self.slot + 1 {
                DEPTH[self.cpu].store(self.slot, Ordering::Release);
            }
            unsafe {
                (*TABLE.0.get())[self.cpu][self.slot] = Entry { ptr: 0, unlocker: None };
            }
            drop(self.guard.take());
        }
    }

    /// Acquire `m`, recording it so a long-jump can release it.
    pub fn lock_tracked<T>(m: &'static spin::Mutex<T>) -> TrackedGuard<'static, T> {
        let cpu  = super::current_exec_cpu();
        let slot = DEPTH[cpu].load(Ordering::Acquire);
        let guard = m.lock();

        if slot < MAX_TRACKED {
            unsafe {
                (*TABLE.0.get())[cpu][slot] = Entry {
                    ptr: m as *const _ as usize,
                    unlocker: Some(unlock_thunk::<T>),
                };
            }
            DEPTH[cpu].store(slot + 1, Ordering::Release);
            TrackedGuard { guard: Some(guard), cpu, slot }
        } else {
            // Table full — degrade to an untracked lock rather than failing.
            crate::serial_println!("[LOCK] tracking table full on CPU {}", cpu);
            TrackedGuard { guard: Some(guard), cpu, slot: MAX_TRACKED - 1 }
        }
    }

    /// Release every lock still registered for `cpu`, newest first.
    ///
    /// SAFETY-ish: this is only ever reached on a path where the owning
    /// kernel frame has been abandoned, so nobody can still be using the
    /// protected data.
    pub fn release_all_for_cpu(cpu: usize) {
        if cpu >= super::MAX_EXEC_CPUS { return; }
        let depth = DEPTH[cpu].swap(0, Ordering::AcqRel);
        if depth == 0 { return; }
        crate::serial_println!("[LOCK] force-releasing {} abandoned lock(s) on CPU {}", depth, cpu);
        for i in (0..depth.min(MAX_TRACKED)).rev() {
            let entry = unsafe { (*TABLE.0.get())[cpu][i] };
            if let (Some(f), true) = (entry.unlocker, entry.ptr != 0) {
                unsafe { f(entry.ptr) };
            }
            unsafe {
                (*TABLE.0.get())[cpu][i] = Entry { ptr: 0, unlocker: None };
            }
        }
    }
}

/// Transition from ring 0 to ring 3.
pub unsafe fn launch_into_usermode(entry: u64, stack_top: u64, teb_base: u64) {
    unsafe {
    let sel = crate::gdt::get_selectors();
    let ucs = sel.user_code_selector.0 as u64;
    let uds = sel.user_data_selector.0 as u64;
    let cpu_id = current_exec_cpu();
    let ctx_ptr = exec_ctx_ptr(cpu_id);

    // Arm the long-jump slot *before* entering ring 3 — the very first
    // instruction of the entry point can fault.
    EXEC_CTX_VALID[cpu_id].store(true, core::sync::atomic::Ordering::Release);

    // syscall_entry swapgs's into the per-CPU block. If the previous process
    // left via a long-jump out of a syscall, the exit swapgs never ran and
    // KERNEL_GS_BASE still holds that process's TEB — fix it before ring 3.
    crate::smp::reset_kernel_gs_base();

    crate::serial_println!(
        "[LOADER] iretq   entry={:#X}  rsp={:#X}  CS={:#X}  DS={:#X}  gs.base={:#X}",
        entry, stack_top, ucs, uds, teb_base
    );
    core::arch::asm!(
        "mov [rdi + 0], rbx",
        "mov [rdi + 8], rbp",
        "mov [rdi + 16], r12",
        "mov [rdi + 24], r13",
        "mov [rdi + 32], r14",
        "mov [rdi + 40], r15",
        "mov [rdi + 48], rsp",
        "lea rax, [rip + 2f]",
        "mov [rdi + 56], rax",
        
        "mov ds, r8w",
        "mov es, r8w",
        "mov gs, r8w",
        
        "mov ecx, 0xC0000101",
        "mov eax, r14d",
        "mov rdx, r14",
        "shr rdx, 32",
        "wrmsr",
        
        "push r8",
        "push r10",
        "push 0x202",
        "push r9",
        "push r11",
        "iretq",
        "2:",
        in("rdi") ctx_ptr,
        in("r8") uds,
        in("r9") ucs,
        in("r10") stack_top,
        in("r11") entry,
        in("r14") teb_base,
        out("rax") _,
        out("rcx") _,
        out("rdx") _,
        clobber_abi("C")
    );

    // Control reaches here only via return_from_usermode(), which already
    // cleared the flag.  Clearing again is harmless and documents intent.
    EXEC_CTX_VALID[cpu_id].store(false, core::sync::atomic::Ordering::Release);
    }
}
