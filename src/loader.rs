use crate::println;
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
pub fn load_and_spawn_pe(
    path: &str,
    mapper: &mut impl Mapper<Size4KiB>,
    frame_allocator: &mut impl FrameAllocator<Size4KiB>,
) -> Result<Arc<Process>, &'static str> {
    let mut scratch: Vec<u64> = Vec::new();
    let result = load_and_spawn_pe_inner(path, mapper, frame_allocator, &mut scratch);

    if result.is_err() && !scratch.is_empty() {
        crate::serial_println!(
            "[LOADER] Load failed — rolling back {} mapped pages", scratch.len()
        );
        let mut m = unsafe { memory::get_mapper() };
        for &addr in scratch.iter() {
            let page = Page::containing_address(VirtAddr::new(addr));
            let _ = memory::unmap_and_free_page(page, &mut m);
        }
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
    let data = match VFS.lock().read_file(path) {
        Ok(d) => d,
        Err(_) => return Err("Failed to read PE file from VFS"),
    };

    // 2. Parse PE Headers
    let pe = match PeFile::parse(&data) {
        Ok(p) => p,
        Err(e) => return Err(e),
    };
    pe.print_info();

    // 3. Map Sections into Memory
    
    // Safety check: if the PE has a low image_base (like 4MB), it will collide and blindly overwrite
    // the kernel's own physical or virtual space in a single-address-space monolithic table.
    // So we artificially shove it high up in 64-bit space to keep it containerized.
    let mut mapped_image_base = pe.image_base;
    if mapped_image_base < 0x8000_0000 {
        mapped_image_base |= 0x1_0000_0000;
        crate::serial_println!("[LOADER] Relocated Low-Memory Image Base -> {:#X}", mapped_image_base);
    }
    
    crate::serial_println!("[LOADER] Mapping {} sections to Virtual RAM...", pe.sections.len());

    // ── Map the PE header page ────────────────────────────────────────────────
    // The MSVC CRT startup reads the PE header at image_base (checksum, timestamp,
    // module name) for __security_init_cookie entropy and PEB.ImageBaseAddress
    // verification.  VEILOS only mapped sections, leaving the header page absent.
    // Map one read-only user-accessible page at image_base and copy the raw header.
    {
        use x86_64::structures::paging::PageTableFlags;
        let header_va = VirtAddr::new(mapped_image_base);
        let header_page = Page::containing_address(header_va);
        if let Some(frame) = memory::allocate_frame() {
            memory::map_page(
                header_page, frame,
                PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE | PageTableFlags::WRITABLE,
                mapper, frame_allocator,
            );
            // Copy the first 4 KB of the PE file (DOS header + PE header + section table)
            let copy_size = core::cmp::min(4096, data.len());
            unsafe {
                core::ptr::copy_nonoverlapping(
                    data.as_ptr(),
                    header_va.as_mut_ptr::<u8>(),
                    copy_size,
                );
                // Zero any remainder of the page
                if copy_size < 4096 {
                    core::ptr::write_bytes(
                        header_va.as_mut_ptr::<u8>().add(copy_size),
                        0,
                        4096 - copy_size,
                    );
                }
            }
            mapped_pages.push(mapped_image_base);
            crate::serial_println!("[LOADER] PE header mapped at {:#X} ({} bytes)", mapped_image_base, copy_size);
        } else {
            crate::serial_println!("[LOADER] WARNING: could not allocate frame for PE header page");
        }
    }

    for section in &pe.sections {
        let v_addr = section.virtual_address as u64;
        let p_data = section.pointer_to_raw_data as usize;
        let raw_size = section.size_of_raw_data as usize;
        let virt_size = section.virtual_size as usize;
        
        let section_base = mapped_image_base + v_addr;
        
        let mut flags = PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE;
        if (section.characteristics & 0x80000000) != 0 { flags |= PageTableFlags::WRITABLE; }
        
        // Map one page at a time
        let num_pages = (virt_size + 4095) / 4096;
        for i in 0..num_pages {
            let page_addr_u64 = section_base + (i as u64 * 4096);
            let page_addr = VirtAddr::new(page_addr_u64);
            let page = Page::containing_address(page_addr);
            
            let aligned_page_start = page.start_address().as_u64();
            if !mapped_pages.contains(&aligned_page_start) {
                let frame = memory::allocate_frame().ok_or("No frames left for PE loading")?;
                memory::map_page(page, frame, flags | PageTableFlags::WRITABLE, mapper, frame_allocator);
                mapped_pages.push(aligned_page_start);
            }
            
            // If we have raw data for this page, copy it!
            let offset_in_section = (i * 4096) as usize;
            if offset_in_section < raw_size {
                let to_copy = core::cmp::min(4096, raw_size - offset_in_section);
                let src = &data[p_data + offset_in_section .. p_data + offset_in_section + to_copy];
                
                unsafe {
                    core::ptr::copy_nonoverlapping(src.as_ptr(), page_addr.as_mut_ptr::<u8>(), to_copy);
                    // Zero out the rest of the page if needed
                    if to_copy < 4096 {
                        core::ptr::write_bytes(page_addr.as_mut_ptr::<u8>().add(to_copy), 0, 4096 - to_copy);
                    }
                }
            }
        }
    }

    // NOTE: IAT resolution and the read-only protection sweep used to happen
    // HERE, before the NT environment existed.  That meant `_initterm` /
    // `_initterm_e` were linked against addresses that had not been assigned
    // yet (0 on the first launch, a dangling pointer into the previous
    // process's TEB pool afterwards).  Both now run after the NT environment
    // is built — see "3c" further down.

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
    process_inner.entry_point = mapped_image_base + pe.entry_point;
    process_inner.stack_top   = launch_rsp;  // launch RSP, not the raw top
    
    // Set up initial register state for User Mode execution
    {
        let mut reg = process_inner.registers.lock();
        reg.rip = process_inner.entry_point;
        reg.rsp = launch_rsp;  // ABI-adjusted: points at the null return address
        reg.rflags = 0x202; // IF bit set
    }
    
    process_inner.image_base = mapped_image_base;
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

    // ── 3b. Apply base relocations ───────────────────────────────────────────
    // Must happen before the IAT is linked (IAT slots are written afterwards
    // and must not be relocated) and while sections are still writable.
    apply_base_relocations(&pe, &data, mapped_image_base);

    // ── 3c. Resolve Imports (IAT) ────────────────────────────────────────────
    // Runs HERE, not before the NT environment is built, so `_initterm` and
    // `_initterm_e` are linked against this process's own trampoline page.
    resolve_imports(&pe, &data, mapped_image_base, &env, mapper, frame_allocator, mapped_pages)?;

    // ── 3d. Finalise memory protection ───────────────────────────────────────
    // Read-only bounds must be applied after ALL copies (including the IAT
    // patches above) or overlapping pages fault mid-write.
    for section in &pe.sections {
        let mut flags = PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE;
        if (section.characteristics & 0x80000000) != 0 { flags |= PageTableFlags::WRITABLE; }
        if !flags.contains(PageTableFlags::WRITABLE) {
            let section_base = mapped_image_base + section.virtual_address as u64;
            let num_pages = (section.virtual_size as usize + 4095) / 4096;
            for i in 0..num_pages {
                let page_addr = VirtAddr::new(section_base + (i as u64 * 4096));
                let page = Page::containing_address(page_addr);
                unsafe {
                    let _ = mapper.update_flags(page, flags).map(|f| f.flush());
                }
            }
        }
    }

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

    crate::serial_println!("[LOADER] Successfully prepared Windows executable at {:#X}", mapped_image_base + pe.entry_point);
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
            entry:     process.entry_point,
            stack_top: process.stack_top,
            teb_base,
            pid:       process.pid,
        });
        return;
    }

    unsafe { crate::loader::launch_into_usermode(process.entry_point, process.stack_top, teb_base); }

    // ── Post-exit ────────────────────────────────────────────────────────
    //
    // return_from_usermode() longjmps here from either:
    //   • ExitProcess syscall handler  (SYSCALL clears IF via SFMASK)
    //   • Page fault / GPF handler     (exception entry clears IF)
    //
    // In BOTH cases the CPU's interrupt flag is still 0 because we bypassed
    // the normal iretq that would restore RFLAGS.  Without this STI,
    // timer/keyboard/mouse interrupts are permanently dead and the entire OS
    // appears frozen.
    unsafe { core::arch::asm!("sti"); }

    on_process_exit(&process);

    crate::serial_println!(
        "[SCHED] Process {} completed execution on BSP.  Interrupts re-enabled.",
        process.pid
    );
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

    // 4. Close its NT handles and VADs.
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
    (crate::smp::current_apic_id() as usize) % MAX_EXEC_CPUS
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

/// Apply the PE base relocation table to an image that was loaded somewhere
/// other than its preferred `image_base`.
///
/// Without this, every absolute address baked into the image — vtables,
/// jump tables, string pointers, the `__security_cookie` slot — still refers
/// to the link-time base.  The loader has *always* rebased low-image-base
/// EXEs (`image_base |= 0x1_0000_0000`) and loads every DLL at a synthetic
/// address, so this was silently corrupting both.
///
/// `actual_base` is where the image really lives; the image must already be
/// mapped WRITABLE when this is called (the protection sweep runs later).
fn apply_base_relocations(pe: &PeFile, data: &[u8], actual_base: u64) -> usize {
    const IMAGE_REL_BASED_ABSOLUTE: u16 = 0;
    const IMAGE_REL_BASED_HIGHLOW:  u16 = 3;
    const IMAGE_REL_BASED_DIR64:    u16 = 10;

    let delta = actual_base.wrapping_sub(pe.image_base);
    if delta == 0 { return 0; }

    let (reloc_rva, reloc_size) = match pe.reloc_dir {
        Some(r) => r,
        None => {
            crate::serial_println!(
                "[LOADER] WARNING: image rebased by {:#X} but has NO .reloc table — \
                 absolute addresses will be wrong", delta
            );
            return 0;
        }
    };

    let mut file_off = match PeFile::rva_to_offset(&pe.sections, reloc_rva) {
        Some(o) => o,
        None => return 0,
    };
    let end = file_off + reloc_size as usize;
    let mut fixups = 0usize;

    while file_off + 8 <= end && file_off + 8 <= data.len() {
        let page_rva   = unsafe { core::ptr::read_unaligned(data.as_ptr().add(file_off) as *const u32) };
        let block_size = unsafe { core::ptr::read_unaligned(data.as_ptr().add(file_off + 4) as *const u32) } as usize;
        if block_size < 8 || file_off + block_size > data.len() { break; }

        let count = (block_size - 8) / 2;
        for i in 0..count {
            let ent_off = file_off + 8 + i * 2;
            if ent_off + 2 > data.len() { break; }
            let entry = unsafe { core::ptr::read_unaligned(data.as_ptr().add(ent_off) as *const u16) };
            let kind   = entry >> 12;
            let offset = (entry & 0x0FFF) as u32;
            let target = actual_base + page_rva as u64 + offset as u64;

            match kind {
                IMAGE_REL_BASED_ABSOLUTE => {}  // padding entry, skip
                IMAGE_REL_BASED_DIR64 => unsafe {
                    let p = target as *mut u64;
                    let v = core::ptr::read_unaligned(p);
                    core::ptr::write_unaligned(p, v.wrapping_add(delta));
                    fixups += 1;
                },
                IMAGE_REL_BASED_HIGHLOW => unsafe {
                    let p = target as *mut u32;
                    let v = core::ptr::read_unaligned(p);
                    core::ptr::write_unaligned(p, v.wrapping_add(delta as u32));
                    fixups += 1;
                },
                other => {
                    crate::serial_println!("[LOADER] Unsupported relocation type {} at {:#X}", other, target);
                }
            }
        }
        file_off += block_size;
    }

    crate::serial_println!(
        "[LOADER] Applied {} base relocations (delta {:#X})", fixups, delta
    );
    fixups
}

fn resolve_imports(
    exe_pe: &PeFile,
    exe_data: &[u8],
    exe_mapped_base: u64,
    env: &NtEnv,
    mapper: &mut impl Mapper<Size4KiB>,
    frame_allocator: &mut impl FrameAllocator<Size4KiB>,
    mapped_pages: &mut alloc::vec::Vec<u64>,
) -> Result<(), &'static str> {

    // Allocate one shared "null stub" page.  Any import that has no real DLL
    // AND no fake-DLL handler is redirected here instead of being left with
    // its original on-disk RVA (which would be jumped to as a raw address).
    //
    // Map WRITABLE from the start so the kernel write below succeeds.
    // On x86_64 pages are executable by default (NX bit not set here), so
    // PRESENT | WRITABLE | USER_ACCESSIBLE = read/write/execute.
    // The stub bytes:  xor rax, rax  (48 31 C0)  +  ret  (C3)
    static NULL_STUB_BASE: core::sync::atomic::AtomicU64 =
        core::sync::atomic::AtomicU64::new(0x5_0000_0000);
    let stub_base = NULL_STUB_BASE.fetch_add(0x1000, core::sync::atomic::Ordering::Relaxed);

    let stub_page  = Page::containing_address(VirtAddr::new(stub_base));
    let stub_frame = memory::allocate_frame().ok_or("No frame for null stub")?;
    memory::map_page(
        stub_page, stub_frame,
        PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::USER_ACCESSIBLE,
        mapper, frame_allocator,
    );
    mapped_pages.push(stub_page.start_address().as_u64());
    // Write stub directly — using write_volatile to prevent dead-store elimination.
    unsafe {
        let p = stub_base as *mut u8;
        core::ptr::write_volatile(p.add(0), 0x48);
        core::ptr::write_volatile(p.add(1), 0x31);
        core::ptr::write_volatile(p.add(2), 0xC0);  // xor rax, rax
        core::ptr::write_volatile(p.add(3), 0xC3);  // ret
        let check = core::ptr::read(p as *const [u8; 4]);
        crate::serial_println!("[LOADER] Null stub explicitly written. Verification read: {:02X} {:02X} {:02X} {:02X} at PhysFrame {:#X}", 
            check[0], check[1], check[2], check[3], stub_frame.start_address().as_u64());
    }
    // IMMEDIATELY revoke write permissions to the stub page.
    // If the OS or user-executable is secretly zeroing out `0x500000000` later on,
    // this will trap the culprit red-handed with a Write Page Fault at 0x500000000!
    unsafe {
        mapper.update_flags(stub_page, PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE)
            .expect("Failed to revoke write access to stub")
            .flush();
    }
    crate::serial_println!("[LOADER] Null-return stub secured at {:#X}", stub_base);

    for dll_info in &exe_pe.imports {
        println!("[LOADER] Resolving Imports for: {}", dll_info.name);
        
        // 1. Load the DLL binary
        // We look in /SYSTEM/ for the DLL.
        let dll_path = alloc::format!("/SYSTEM/{}", dll_info.name);
        let dll_data_opt = VFS.lock().read_file(&dll_path);
        
        if let Ok(dll_data) = dll_data_opt {
            // --- REAL DLL LOADING PATH ---
            // A malformed DLL must NOT abort the whole EXE load — fall back to
            // the internal handler / null-stub path instead.
            let dll_pe = match PeFile::parse(&dll_data) {
                Ok(p) => p,
                Err(e) => {
                    crate::serial_println!(
                        "[LOADER] {} failed to parse ({}) — falling back to kernel thunks",
                        dll_info.name, e
                    );
                    link_fallback(exe_pe, exe_data, exe_mapped_base, dll_info, stub_base, env,
                                  mapper, frame_allocator, mapped_pages)?;
                    continue;
                }
            };
            
            static DLL_LOAD_ADDR: core::sync::atomic::AtomicU64 =
                core::sync::atomic::AtomicU64::new(0x2_0000_0000);
            let dll_base = DLL_LOAD_ADDR.fetch_add(0x1_0000_0000, core::sync::atomic::Ordering::Relaxed);
            
            crate::serial_println!("[LOADER] Mapping REAL DLL {} to {:#X}", dll_info.name, dll_base);
            
            for section in &dll_pe.sections {
                let num_pages = (section.virtual_size as usize + 4095) / 4096;
                for i in 0..num_pages {
                    let page_addr = VirtAddr::new(dll_base + section.virtual_address as u64 + (i as u64 * 4096));
                    let page = Page::containing_address(page_addr);
                    let frame = memory::allocate_frame().ok_or("No frames for DLL")?;
                    memory::map_page(page, frame, PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE | PageTableFlags::WRITABLE, mapper, frame_allocator);
                    mapped_pages.push(page.start_address().as_u64());
                    
                    let offset_in_section = i * 4096;
                    if offset_in_section < section.size_of_raw_data as usize {
                        let to_copy = core::cmp::min(4096, section.size_of_raw_data as usize - offset_in_section);
                        let src = &dll_data[section.pointer_to_raw_data as usize + offset_in_section .. section.pointer_to_raw_data as usize + offset_in_section + to_copy];
                        unsafe {
                            core::ptr::copy_nonoverlapping(src.as_ptr(), page_addr.as_mut_ptr::<u8>(), to_copy);
                        }
                    }
                }
            }
            
            // Rebase the DLL now that every section is resident and writable.
            apply_base_relocations(&dll_pe, &dll_data, dll_base);

            link_iat_from_eat(exe_pe, exe_data, exe_mapped_base, &dll_info.name, &dll_pe, dll_base, stub_base)?;
        } else {
            link_fallback(exe_pe, exe_data, exe_mapped_base, dll_info, stub_base, env,
                          mapper, frame_allocator, mapped_pages)?;
        }
    }
    Ok(())
}

/// Link a DLL's imports when no real on-disk DLL could be loaded: try the
/// internal fake-DLL registry first, otherwise point every slot at the
/// null-return stub.
fn link_fallback(
    exe_pe: &PeFile,
    exe_data: &[u8],
    exe_mapped_base: u64,
    dll_info: &crate::executable::ImportedDll,
    stub_base: u64,
    env: &NtEnv,
    mapper: &mut impl Mapper<Size4KiB>,
    frame_allocator: &mut impl FrameAllocator<Size4KiB>,
    mapped_pages: &mut alloc::vec::Vec<u64>,
) -> Result<(), &'static str> {
    crate::serial_println!("[LOADER] No real DLL for {}. Attempting kernel thunking...", dll_info.name);

    if let Some(fake_dll) = crate::dll::DLL_REGISTRY.lock().get_dll(&dll_info.name) {
        crate::serial_println!("[LOADER] Found INTERNAL handler for {}. Linking thunks...", dll_info.name);
        link_iat_from_fake(exe_pe, exe_data, exe_mapped_base, dll_info, fake_dll, stub_base, env,
                           mapper, frame_allocator, mapped_pages)?;
    } else {
        crate::serial_println!("[LOADER] No handler for {} — patching IAT with null stub (returns 0)", dll_info.name);
        // Patch every import from this DLL to the null-return stub so the
        // exe calls a valid address instead of jumping to a raw on-disk RVA.
        let oft_rva = if dll_info.original_first_thunk != 0 { dll_info.original_first_thunk } else { dll_info.first_thunk };
        let ft_rva  = dll_info.first_thunk;
        if let Some(oft_off) = PeFile::rva_to_offset(&exe_pe.sections, oft_rva) {
            let mut i = 0usize;
            loop {
                let thunk_offset = i * 8;
                if oft_off + thunk_offset + 8 > exe_data.len() { break; }
                let val = unsafe { core::ptr::read_unaligned(exe_data.as_ptr().add(oft_off + thunk_offset) as *const u64) };
                if val == 0 { break; }
                let ft_mem_addr = exe_mapped_base + ft_rva as u64 + thunk_offset as u64;
                unsafe { *(ft_mem_addr as *mut u64) = stub_base; }
                i += 1;
            }
        }
    }
    Ok(())
}

fn link_iat_from_eat(
    exe_pe: &PeFile,
    exe_data: &[u8],
    exe_mapped_base: u64,
    dll_name: &str,
    dll_pe: &PeFile,
    dll_base: u64,
    stub_base: u64,  // null-return stub for symbols not in the EAT
) -> Result<(), &'static str> {
    let mut resolved_count = 0;
    let mut stubbed_count  = 0;
    for dll_info in &exe_pe.imports {
        if !dll_info.name.eq_ignore_ascii_case(dll_name) { continue; }

        let oft_rva = if dll_info.original_first_thunk != 0 { dll_info.original_first_thunk } else { dll_info.first_thunk };
        let ft_rva  = dll_info.first_thunk;

        if let Some(oft_off) = PeFile::rva_to_offset(&exe_pe.sections, oft_rva) {
            let mut i = 0;
            loop {
                let thunk_offset = i * 8;
                if oft_off + thunk_offset + 8 > exe_data.len() { break; }
                let val = unsafe { core::ptr::read_unaligned(exe_data.as_ptr().add(oft_off + thunk_offset) as *const u64) };
                if val == 0 { break; }

                let ft_mem_addr = exe_mapped_base + ft_rva as u64 + thunk_offset as u64;

                if val & (1 << 63) == 0 {
                    // Import by name.
                    if let Some(name_entry_off) = PeFile::rva_to_offset(&exe_pe.sections, (val & 0x7FFFFFFF) as u32) {
                        let mut name = alloc::string::String::new();
                        let mut k = name_entry_off + 2;
                        while k < exe_data.len() && exe_data[k] != 0 {
                            name.push(exe_data[k] as char);
                            k += 1;
                        }
                        if let Some(exp) = dll_pe.exports.iter().find(|e| e.name.as_deref() == Some(&name)) {
                            // A forwarded export stores an ASCII string
                            // ("NTDLL.RtlAllocHeap") inside the export
                            // directory instead of code.  Jumping to it
                            // executes the string as instructions.  We can't
                            // chase forwarders yet, so stub them out.
                            if dll_pe.export_dir_range.map_or(false, |(lo, hi)| exp.rva >= lo && exp.rva < hi) {
                                crate::serial_println!(
                                    "[LOADER] Forwarded export {}!{} → null stub (forwarders unsupported)",
                                    dll_name, name
                                );
                                unsafe { *(ft_mem_addr as *mut u64) = stub_base; }
                                stubbed_count += 1;
                            } else {
                                let func_addr = dll_base + exp.rva as u64;
                                unsafe { *(ft_mem_addr as *mut u64) = func_addr; }
                                resolved_count += 1;
                            }
                        } else {
                            // Symbol exists in the EXE's import table but not in the
                            // DLL's export table — patch to null stub so it doesn't crash.
                            crate::serial_println!("[LOADER] EAT miss: {} in {} → null stub", name, dll_name);
                            unsafe { *(ft_mem_addr as *mut u64) = stub_base; }
                            stubbed_count += 1;
                        }
                    }
                } else {
                    // Import by ordinal.  The low 16 bits hold the ordinal;
                    // match it against the DLL's export table before falling
                    // back to the stub.
                    let ordinal = (val & 0xFFFF) as u16;
                    match dll_pe.exports.iter().find(|e| e.ordinal == ordinal) {
                        Some(exp) if !dll_pe.export_dir_range
                            .map_or(false, |(lo, hi)| exp.rva >= lo && exp.rva < hi) =>
                        {
                            unsafe { *(ft_mem_addr as *mut u64) = dll_base + exp.rva as u64; }
                            resolved_count += 1;
                        }
                        _ => {
                            crate::serial_println!("[LOADER] Ordinal import #{} from {} → null stub", ordinal, dll_name);
                            unsafe { *(ft_mem_addr as *mut u64) = stub_base; }
                            stubbed_count += 1;
                        }
                    }
                }
                i += 1;
            }
        }
    }
    crate::serial_println!("[LOADER] Linked {} from REAL DLL {} ({} stubbed)", resolved_count, dll_name, stubbed_count);
    Ok(())
}

fn link_iat_from_fake(
    exe_pe: &PeFile,
    exe_data: &[u8],
    exe_mapped_base: u64,
    dll_info: &crate::executable::ImportedDll,
    fake_dll: &crate::dll::FakeDll,
    stub_base: u64,  // null-return stub for symbols not in fake_dll.symbols
    env: &NtEnv,     // this process's own trampoline/data addresses
    mapper: &mut impl Mapper<Size4KiB>,
    frame_allocator: &mut impl FrameAllocator<Size4KiB>,
    mapped_pages: &mut alloc::vec::Vec<u64>,
) -> Result<(), &'static str> {
    // 1. Create a Trampoline Page for this DLL's thunks
    // Every thunk will be:  MOV RAX, ID; SYSCALL; RET (approx 16 bytes)
    static THUNK_POOL_BASE: core::sync::atomic::AtomicU64 =
        core::sync::atomic::AtomicU64::new(0x3_0000_0000);
    let pool_base = THUNK_POOL_BASE.fetch_add(0x1000, core::sync::atomic::Ordering::Relaxed);
    
    let page = Page::containing_address(VirtAddr::new(pool_base));
    let frame = memory::allocate_frame().ok_or("No frames for thunks")?;
    memory::map_page(page, frame, PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE | PageTableFlags::WRITABLE, mapper, frame_allocator);
    mapped_pages.push(page.start_address().as_u64());

    let oft_rva = if dll_info.original_first_thunk != 0 { dll_info.original_first_thunk } else { dll_info.first_thunk };
    let ft_rva = dll_info.first_thunk;
    
    let mut resolved_count: u64 = 0;
    if let Some(oft_off) = PeFile::rva_to_offset(&exe_pe.sections, oft_rva) {
        let mut i = 0;
        loop {
            let thunk_offset = i * 8;
            if oft_off + thunk_offset + 8 > exe_data.len() { break; }
            let val = unsafe { core::ptr::read_unaligned(exe_data.as_ptr().add(oft_off + thunk_offset) as *const u64) };
            if val == 0 { break; }
            
            if val & (1 << 63) == 0 {
                if let Some(name_entry_off) = PeFile::rva_to_offset(&exe_pe.sections, (val & 0x7FFFFFFF) as u32) {
                    let mut name = alloc::string::String::new();
                    let mut k = name_entry_off + 2;
                    while k < exe_data.len() && exe_data[k] != 0 {
                        name.push(exe_data[k] as char);
                        k += 1;
                    }
                    
                    let ft_mem_addr = exe_mapped_base + ft_rva as u64 + thunk_offset as u64;

                    // ── Special user-mode overrides ──
                    // These functions MUST NOT go through SYSCALL — they need to
                    // execute in user-mode with access to user-mode pointers.
                    let user_mode_addr = match name.as_str() {
                        "_initterm"   if env.initterm   != 0 => Some(env.initterm),
                        "_initterm_e" if env.initterm_e != 0 => Some(env.initterm_e),
                        _ => None,
                    };

                    if let Some(addr) = user_mode_addr {
                        crate::serial_println!("[LOADER] IAT override: {}!{} → user-mode {:#X}", dll_info.name, name, addr);
                        unsafe { *(ft_mem_addr as *mut u64) = addr; }
                        resolved_count += 1;
                    } else if let Some(sym) = fake_dll.symbols.iter().find(|s| s.name == name) {
                        // Build a syscall trampoline in the thunk pool:
                        //   mov r10, rcx   (preserve Windows arg0 before SYSCALL clobbers RCX)
                        //   mov rax, ID    (syscall number)
                        //   syscall
                        //   ret
                        let tramp_addr = pool_base + (resolved_count * 16);
                        unsafe {
                            let p = tramp_addr as *mut u8;
                            core::ptr::write_volatile(p.add(0),  0x49); // mov r10, rcx
                            core::ptr::write_volatile(p.add(1),  0x89);
                            core::ptr::write_volatile(p.add(2),  0xCA);
                            core::ptr::write_volatile(p.add(3),  0x48); // mov rax, imm32
                            core::ptr::write_volatile(p.add(4),  0xC7);
                            core::ptr::write_volatile(p.add(5),  0xC0);
                            let id = sym.syscall_id as u32;
                            core::ptr::write_volatile(p.add(6), (id & 0xFF) as u8);
                            core::ptr::write_volatile(p.add(7), ((id >> 8) & 0xFF) as u8);
                            core::ptr::write_volatile(p.add(8), ((id >> 16) & 0xFF) as u8);
                            core::ptr::write_volatile(p.add(9), ((id >> 24) & 0xFF) as u8);
                            core::ptr::write_volatile(p.add(10), 0x0F); // syscall
                            core::ptr::write_volatile(p.add(11), 0x05);
                            core::ptr::write_volatile(p.add(12), 0xC3); // ret
                        }
                        unsafe { *(ft_mem_addr as *mut u64) = tramp_addr; }
                        resolved_count += 1;
                    } else {
                        // Symbol not in our fake DLL registry — redirect to null stub
                        // so the EXE calls a valid address that returns 0 instead of
                        // jumping to the raw on-disk hint/name RVA.
                        crate::serial_println!("[LOADER] Unresolved: {}!{} → null stub", dll_info.name, name);
                        unsafe { *(ft_mem_addr as *mut u64) = stub_base; }
                    }
                }
            }
            i += 1;
        }
    }
    
    // Set to read-execute only after writing
    unsafe {
        let _ = mapper.update_flags(page, PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE).map(|f| f.flush());
    }

    crate::serial_println!("[LOADER] Created {} syscall-thunks for internal {}", resolved_count, dll_info.name);
    Ok(())
}
