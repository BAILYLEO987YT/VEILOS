use crate::executable::PeFile;
use crate::memory;
use crate::process::{Process, PROCESSOR};
use crate::subsystem::WindowsSubsystem;
use x86_64::{VirtAddr, structures::paging::{Mapper, FrameAllocator, Size4KiB, Page, PageTableFlags}};
use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::string::String;
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

/// Load a PE into a fresh address space and queue it to run.
///
/// The whole load runs with the new process's page tables active
/// (`address_space::with`), so the loader's mappings and its writes to user
/// addresses land in that process alone, and `Process::new` records its CR3.
pub fn load_and_spawn_pe(
    path: &str,
    frame_allocator: &mut impl FrameAllocator<Size4KiB>,
) -> Result<Arc<Process>, &'static str> {
    load_and_spawn_pe_with_args(path, "", frame_allocator)
}

/// Like `load_and_spawn_pe`, with `args` after the program's name on its
/// command line.
pub fn load_and_spawn_pe_with_args(
    path: &str,
    args: &str,
    frame_allocator: &mut impl FrameAllocator<Size4KiB>,
) -> Result<Arc<Process>, &'static str> {
    let dos_path = alloc::format!("C:{}", path.replace('/', "\\"));
    let command_line = if args.is_empty() { alloc::format!("\"{}\"", dos_path) } else { alloc::format!("\"{}\" {}", dos_path, args) };
    let options = LaunchOptions { command_line: Some(command_line), ..LaunchOptions::default() };
    load_and_spawn_pe_with(path, options, frame_allocator)
}

/// Which console a program gets.
#[derive(Default)]
pub enum ConsoleChoice {
    /// A console program gets a new console (the caller shows it).
    #[default]
    New,
    /// It shares this one (CreateProcess from a console program).
    Inherit(Arc<crate::console::Console>),
    /// None (DETACHED_PROCESS).
    None,
}

/// How a program is started (CreateProcess); the default is RUN's.
#[derive(Default)]
pub struct LaunchOptions {
    /// The whole command line (default: the quoted DOS path).
    pub command_line: Option<String>,
    /// Its current directory (default: the program's folder).
    pub current_dir: Option<String>,
    /// Its environment block (default: the system's).
    pub environment: Option<Vec<u16>>,
    pub console: ConsoleChoice,
    /// The objects its standard handles refer to (default: its console's).
    pub std_objects: Option<[Option<crate::nt_objects::KernelObjectRef>; 3]>,
    /// Its first thread waits for NtResumeThread.
    pub suspended: bool,
    pub parent_pid: u32,
    /// The caller's handles, copied to the same values (bInheritHandles).
    pub inherited: Option<crate::nt_objects::HandleTable>,
    /// STARTUPINFO's dwFlags and lpReserved2 bytes, for GetStartupInfo.
    pub startup_flags: u32,
    pub runtime_data: Vec<u8>,
}

/// Load `path` into a fresh address space with `options` and queue its
/// first thread to run (unless suspended).
pub fn load_and_spawn_pe_with(
    path: &str,
    options: LaunchOptions,
    frame_allocator: &mut impl FrameAllocator<Size4KiB>,
) -> Result<Arc<Process>, &'static str> {
    let args = &options;
    let Some(cr3) = crate::address_space::create() else {
        return Err("Out of memory for page tables");
    };

    let result = crate::address_space::with(cr3, || {
        let mut mapper = unsafe { memory::get_mapper() }; // the new process's tables
        let mut scratch: Vec<u64> = Vec::new();
        let result = load_and_spawn_pe_inner(path, args, &mut mapper, frame_allocator, &mut scratch);
        if result.is_err() && !scratch.is_empty() {
            crate::serial_println!(
                "[LOADER] Load failed — rolling back {} mapped pages", scratch.len()
            );
            for &addr in scratch.iter() {
                let page = Page::containing_address(VirtAddr::new(addr));
                let _ = memory::unmap_and_free_page(page, &mut mapper);
            }
        }
        if result.is_err() {
            crate::nt_demand::release_active();
        }
        result
    });

    if result.is_err() {
        crate::address_space::destroy(cr3);
    }
    result
}

/// Zeroed pages for the program, `bytes` of them, owned like the rest.
fn extra_pages(
    bytes: usize,
    mapper: &mut impl Mapper<Size4KiB>,
    frame_allocator: &mut impl FrameAllocator<Size4KiB>,
    mapped_pages: &mut Vec<u64>,
) -> Result<u64, &'static str> {
    let pages = (bytes as u64).div_ceil(0x1000);
    let base = alloc_teb_range(pages);
    for i in 0..pages {
        let addr = base + i * 0x1000;
        let frame = memory::allocate_frame().ok_or("No frame for process parameters")?;
        memory::map_page(
            Page::containing_address(VirtAddr::new(addr)),
            frame,
            PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::USER_ACCESSIBLE | PageTableFlags::NO_EXECUTE,
            mapper,
            frame_allocator,
        );
        mapped_pages.push(addr);
        unsafe { core::ptr::write_bytes(addr as *mut u8, 0, 4096) };
    }
    Ok(base)
}

fn load_and_spawn_pe_inner(
    path: &str,
    options: &LaunchOptions,
    mapper: &mut impl Mapper<Size4KiB>,
    frame_allocator: &mut impl FrameAllocator<Size4KiB>,
    mapped_pages: &mut Vec<u64>,
) -> Result<Arc<Process>, &'static str> {
    crate::serial_println!("\n[LOADER] >>> LOADING WINDOWS EXECUTABLE: {}", path);

    // 1. Read binary data from VFS
    let file = crate::nt_file::open_mapped(path).map_err(|_| "Failed to read PE file from VFS")?;
    let data = crate::modules::read_headers(&file)?;

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
        linker.load_exe(path, &file, &data, pe)?
    };

    // 4. Set up Stack (Small 64KB stack for now)
    // To prevent overlapping processes from hitting PageAlreadyMapped over same execution runs
    static PROC_COUNT: core::sync::atomic::AtomicU64 =
        core::sync::atomic::AtomicU64::new(0);
    let proc_idx = PROC_COUNT.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    
    // 1 MiB, Windows' default reserve (committed up front).
    const STACK_PAGES: u64 = 256;
    let stack_base = 0x7000_0000_0000 + (proc_idx * 0x200000);
    let stack_size = STACK_PAGES * 4096;

    for i in 0..STACK_PAGES {
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
        process_inner.console = match &options.console {
            ConsoleChoice::New => Some(crate::console::Console::new(pid)),
            ConsoleChoice::Inherit(c) => Some(c.clone()),
            ConsoleChoice::None => None,
        };
    }
    process_inner.parent_pid.store(options.parent_pid, core::sync::atomic::Ordering::Relaxed);
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
    // Layout (ENV_PAGES pages; pool slots never reused):
    //   +0x0000 TEB (two pages: TLS slots sit at 0x1480)
    //   +0x2000 PEB
    //   +0x3000 PEB_LDR_DATA
    //   +0x4000 _initterm / _initterm_e user-mode code
    //   +0x5000 __argc / __argv data
    //   +0x6000 RTL_USER_PROCESS_PARAMETERS, its strings after PARAMS_SIZE
    //   +0x7000 environment block (two pages)
    const ENV_PAGES: u64 = 9;
    let teb_base = alloc_teb_range(ENV_PAGES);
    for i in 0..ENV_PAGES {
        let page  = Page::containing_address(VirtAddr::new(teb_base + i * 0x1000));
        let frame = memory::allocate_frame().ok_or("No frame for TEB/PEB")?;
        memory::map_page(
            page, frame,
            PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::USER_ACCESSIBLE | PageTableFlags::NO_EXECUTE,
            mapper, frame_allocator,
        );
        mapped_pages.push(page.start_address().as_u64());
        unsafe { core::ptr::write_bytes((teb_base + i * 0x1000) as *mut u8, 0, 4096); }
    }
    // The _initterm page holds code.
    unsafe {
        let _ = mapper.update_flags(Page::containing_address(VirtAddr::new(teb_base + 0x4000)),
            PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::USER_ACCESSIBLE).map(|f| f.flush());
    }

    let peb_base = teb_base + 0x2000;
    let ldr_base = teb_base + 0x3000;
    let initterm_code_base = teb_base + 0x4000;
    let argv_data_base = teb_base + 0x5000;
    let params_base = teb_base + 0x6000;
    let env_base = teb_base + 0x7000;
    {
        let mut vad = process.vad.lock();
        vad.record_image(teb_base, (ENV_PAGES * 0x1000) as usize);
        vad.record_image(stack_base, stack_size as usize);
    }

    // -----| TEB |-----
    let main_tid = crate::nt_process::alloc_tid();
    unsafe {
        use nt_abi::layout::*;
        let b = teb_base as *mut u8;
        let w = |o: usize, v: u64| core::ptr::write_unaligned(b.add(o) as *mut u64, v);
        w(0x00, u64::MAX);                  // ExceptionList: end of chain
        w(0x08, stack_top);                 // StackBase (high)
        w(0x10, stack_base);                // StackLimit (low)
        w(0x30, teb_base);                  // Self
        w(TEB_PROCESS_ID, pid as u64);      // ClientId.UniqueProcess
        w(TEB_THREAD_ID, main_tid as u64); // ClientId.UniqueThread
        w(TEB_PEB, peb_base);
    }

    // -----| PEB |-----
    // The process heap (PEB.ProcessHeap) is created by ntdll when it loads.
    unsafe {
        use nt_abi::layout::*;
        let p = peb_base as *mut u8;
        *(p.add(0x08) as *mut u64) = u64::MAX;          // Mutant
        *(p.add(PEB_IMAGE_BASE) as *mut u64) = mapped_image_base;
        *(p.add(PEB_LDR) as *mut u64) = ldr_base;
        *(p.add(PEB_PROCESS_PARAMETERS) as *mut u64) = params_base;
        *(p.add(0x118) as *mut u32) = 10;              // OSMajorVersion
        *(p.add(0x11C) as *mut u32) = 0;               // OSMinorVersion
        *(p.add(0x120) as *mut u16) = 19041;           // OSBuildNumber
        *(p.add(0x122) as *mut u16) = 2;               // OSPlatformId = VER_PLATFORM_WIN32_NT
        *(p.add(0xB8) as *mut u32) = crate::smp::CPU_COUNT.load(core::sync::atomic::Ordering::Relaxed); // NumberOfProcessors
        // ImageSubsystem and its version, from the optional header: cmd.exe
        // reads a child's to decide whether to wait for it (not for GUI ones).
        let img = mapped_image_base;
        if crate::memory::user_ptr_ok(img, 0x40) {
            let nt = img + *((img + 0x3C) as *const u32) as u64;
            let opt = nt + 0x18;
            if crate::memory::user_ptr_ok(opt, 0x48) {
                *(p.add(0x128) as *mut u32) = *((opt + 0x44) as *const u16) as u32; // ImageSubsystem
                *(p.add(0x12C) as *mut u32) = *((opt + 0x30) as *const u16) as u32; // ImageSubsystemMajorVersion
                *(p.add(0x130) as *mut u32) = *((opt + 0x32) as *const u16) as u32; // ImageSubsystemMinorVersion
            }
        }
    }

    // -----| PEB_LDR_DATA |----- (modules.rs fills the lists)
    unsafe {
        let l = ldr_base as *mut u8;
        *(l.add(0x00) as *mut u32) = 0x58;
        *(l.add(0x04) as *mut u32) = 1;
        for head in [0x10usize, 0x20, 0x30] {
            *(l.add(head) as *mut u64) = ldr_base + head as u64;
            *(l.add(head + 8) as *mut u64) = ldr_base + head as u64;
        }
    }

    // -----| RTL_USER_PROCESS_PARAMETERS |-----
    // DOS paths: VEILOS "/dir/x.exe" is "C:\dir\x.exe". The current
    // directory starts as the program's folder.
    let dos_path = alloc::format!("C:{}", path.replace('/', "\\"));
    let exe_dir = match &options.current_dir {
        Some(d) if d.ends_with('\\') => d.clone(),
        Some(d) => alloc::format!("{}\\", d),
        None => match dos_path.rfind('\\') {
            Some(i) => String::from(&dos_path[..=i]),
            None => String::from("C:\\"),
        },
    };
    let command_line = options.command_line.clone().unwrap_or_else(|| alloc::format!("\"{}\"", dos_path));
    // The command line and a large environment get pages of their own: Windows
    // allows 32767 characters of each, and rustc's linker lines use them.
    let line_units: Vec<u16> = command_line.encode_utf16().take(32_766).collect();
    let line_base = extra_pages((line_units.len() + 1) * 2, mapper, frame_allocator, mapped_pages)?;
    let env_base = match &options.environment {
        Some(block) if block.len() > 0x1000 - 2 => extra_pages((block.len() + 2) * 2, mapper, frame_allocator, mapped_pages)?,
        _ => env_base,
    };
    unsafe {
        use nt_abi::layout::*;
        let pp = params_base as *mut u8;
        *(pp.add(0x00) as *mut u32) = PARAMS_SIZE as u32; // MaximumLength
        *(pp.add(0x04) as *mut u32) = PARAMS_SIZE as u32; // Length
        *(pp.add(0x08) as *mut u32) = 1;                  // Flags: PROCESS_PARAMETERS_NORMALIZED
        let mut next = params_base + PARAMS_SIZE as u64;
        // A UNICODE_STRING at `field` holding `text`, with room for `capacity` units.
        let mut put = |field: usize, text: &str, capacity: usize| {
            let units: Vec<u16> = text.encode_utf16().collect();
            let cap = capacity.max(units.len() + 1);
            core::ptr::copy_nonoverlapping(units.as_ptr(), next as *mut u16, units.len());
            *(pp.add(field) as *mut u16) = (units.len() * 2) as u16;
            *(pp.add(field + 2) as *mut u16) = (cap * 2) as u16;
            *(pp.add(field + 8) as *mut u64) = next;
            next += (cap as u64 * 2 + 15) & !15;
        };
        put(PARAMS_CURRENT_DIRECTORY, &exe_dir, 261);
        put(PARAMS_IMAGE_PATH_NAME, &dos_path, 0);
        core::ptr::copy_nonoverlapping(line_units.as_ptr(), line_base as *mut u16, line_units.len());
        *(pp.add(PARAMS_COMMAND_LINE) as *mut u16) = (line_units.len() * 2) as u16;
        *(pp.add(PARAMS_COMMAND_LINE + 2) as *mut u16) = ((line_units.len() + 1) * 2) as u16;
        *(pp.add(PARAMS_COMMAND_LINE + 8) as *mut u64) = line_base;

        // Environment: "NAME=value\0" ... "\0".
        let cpus = crate::smp::CPU_COUNT.load(core::sync::atomic::Ordering::Relaxed);
        let vars = [
            String::from("ALLUSERSPROFILE=C:\\ProgramData"),
            String::from("COMPUTERNAME=VEILOS"),
            String::from("ComSpec=C:\\cmd.exe"),
            String::from("HOMEDRIVE=C:"),
            String::from("HOMEPATH=\\home"),
            alloc::format!("NUMBER_OF_PROCESSORS={}", cpus),
            String::from("OS=Windows_NT"),
            String::from("Path=C:\\SYSTEM;C:\\"),
            String::from("PATHEXT=.COM;.EXE;.BAT;.CMD"),
            String::from("PROCESSOR_ARCHITECTURE=AMD64"),
            String::from("PROMPT=$P$G"),
            String::from("SystemDrive=C:"),
            String::from("SystemRoot=C:\\"),
            String::from("TEMP=C:\\tmp"),
            String::from("TMP=C:\\tmp"),
            String::from("USERNAME=user"),
            String::from("USERPROFILE=C:\\home"),
            String::from("windir=C:\\"),
        ];
        let mut at = env_base as *mut u16;
        if let Some(block) = &options.environment {
            let n = block.len();
            core::ptr::copy_nonoverlapping(block.as_ptr(), at, n);
            at = at.add(n);
            *at = 0;
            *at.add(1) = 0;
        } else {
            for v in vars.iter() {
                for u in v.encode_utf16() {
                    *at = u;
                    at = at.add(1);
                }
                *at = 0;
                at = at.add(1);
            }
            *at = 0;
            at = at.add(1);
        }
        *(pp.add(PARAMS_ENVIRONMENT) as *mut u64) = env_base;
        *(pp.add(PARAMS_ENVIRONMENT_SIZE) as *mut u64) = at as u64 - env_base;
    }

    // Inherited handles first, so they keep their values.
    if let Some(table) = &options.inherited {
        *process.handles.lock() = table.duplicate_all();
    }
    unsafe {
        use nt_abi::layout::{PARAMS_RUNTIME_DATA, PARAMS_WINDOW_FLAGS};
        let pp = params_base as *mut u8;
        *(pp.add(PARAMS_WINDOW_FLAGS) as *mut u32) = options.startup_flags;
        // lpReserved2 goes in the params page's spare room, past the strings
        // (the page's last 1 KiB).
        let n = options.runtime_data.len().min(1024);
        if n > 0 {
            let at = params_base + 0x1000 - 1024;
            core::ptr::copy_nonoverlapping(options.runtime_data.as_ptr(), at as *mut u8, n);
            *(pp.add(PARAMS_RUNTIME_DATA) as *mut u16) = n as u16;
            *(pp.add(PARAMS_RUNTIME_DATA + 2) as *mut u16) = n as u16;
            *(pp.add(PARAMS_RUNTIME_DATA + 8) as *mut u64) = at;
        }
    }

    // Standard handles (StandardInput/Output/Error): the objects the caller
    // gave (CreateProcess), else a console program's all refer to its
    // console; a GUI program has none (0), as on Windows.
    {
        use nt_abi::layout::{PARAMS_STD_ERROR, PARAMS_STD_INPUT, PARAMS_STD_OUTPUT};
        let mut handles = process.handles.lock();
        for (i, offset) in [PARAMS_STD_INPUT, PARAMS_STD_OUTPUT, PARAMS_STD_ERROR].into_iter().enumerate() {
            let object = match (&options.std_objects, &process.console) {
                (Some(objects), _) => objects[i].clone(),
                // Input: the console; output and errors: its active screen buffer.
                (None, Some(console)) if i == 0 => Some(crate::nt_objects::KernelObjectRef::Console(console.clone())),
                (None, Some(console)) => Some(crate::nt_objects::KernelObjectRef::ConsoleBuffer(console.active())),
                (None, None) => None,
            };
            let h = object.map_or(0, |o| handles.allocate_ref(o, 0x001F_01FF, 0));
            unsafe { *((params_base as usize + offset) as *mut u64) = h as u64 };
        }
    }

    // -----| KUSER_SHARED_DATA (0x7FFE_0000) |-----
    // One shared frame, mapped read-only into every process (kusd.rs). Not
    // in mapped_pages: it outlives the process.
    crate::kusd::map_into(mapper, frame_allocator)?;

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
        // Where the kernel sends this process's exceptions (nt_exception.rs),
        // and where its threads after the first start (nt_process.rs).
        let dispatcher = linker.module_export(&mut mem, "ntdll.dll", "KiUserExceptionDispatcher").unwrap_or(0);
        process.exception_dispatcher.store(dispatcher, core::sync::atomic::Ordering::Release);
        let thread_start = linker.module_export(&mut mem, "ntdll.dll", "RtlUserThreadStart").unwrap_or(0);
        process.thread_entry.store(thread_start, core::sync::atomic::Ordering::Release);
    }
    // Its first thread (its stack and TEB belong to the process).
    process.threads.lock().push(Arc::new(crate::nt_process::ThreadObj::new(
        pid,
        main_tid,
        crate::nt_process::Start { rip: crate::modules::INIT_ENTRY, rsp: launch_rsp, teb: teb_base, rcx: 0, rdx: 0 },
        options.suspended,
        Vec::new(),
    )));
    // VirtualAlloc must never hand out what the loader mapped.
    {
        let mut vad = process.vad.lock();
        for (base, size) in linker.regions() {
            vad.record_image(base, size);
        }
    }
    *process.unresolved_imports.lock() = linker.unresolved.iter().map(|n| (n.clone(), false)).collect();
    // Kept for LoadLibrary / GetProcAddress.
    *process.linker.lock() = Some(linker);

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
    if !options.suspended {
        process.live_threads.store(1, core::sync::atomic::Ordering::Release);
        crate::vnr::spawn_fiber(process_execution_task(process.clone(), teb_base));
    }

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

    // Its first thread, on the least-loaded CPU (nt_process.rs), so the
    // executor (GUI, input, network) keeps running.
    let _ = teb_base;
    let main = process.threads.lock().first().cloned();
    if let Some(main) = main {
        crate::nt_process::start_thread(process, main);
    }
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

    crate::nt_user::process_exited(process.pid);
    crate::nt_registry::process_exited(process.pid as u32);

    // 3. Drop the process's worker threads.
    if let Some(mut tm) = crate::process::THREAD_MANAGER.try_lock() {
        tm.remove_threads_for_pid(process.pid);
    }

    // 4. Tell its console window (if it owns it: a child shares its
    //    parent's), then close its NT handles and VADs.
    if let Some(console) = process.console.as_ref().filter(|c| c.pid() == process.pid) {
        console.finish(*process.exit_code.lock());
    }
    if let Some(mut h) = process.handles.try_lock() { h.close_all(); }
    if let Some(mut vad) = process.vad.try_lock() { vad.release_all(); }
    process.linker.lock().take();

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
    crate::nt_demand::release_active();
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
// One saved kernel context per user thread (kthread::UserCtx).
// launch_into_usermode() fills it in; return_from_usermode() jumps back
// through it from a syscall or a fault.
//
// The slot is guarded by a validity flag. Without it, a stray ring-3 fault
// at a moment when no process was live would `jmp` onto a dead stack frame
// and corrupt the kernel in an unattributable way.

pub const MAX_EXEC_CPUS: usize = 8;

/// The CPU index lock tracking (lock_guard) is keyed by.
#[inline]
pub fn current_exec_cpu() -> usize {
    crate::smp::cpu_index().min(MAX_EXEC_CPUS - 1)
}

/// Clear the running thread's long-jump slot once its process is gone.
pub fn invalidate_exec_context() {
    if crate::kthread::current_pid() != 0 {
        unsafe { (*crate::kthread::current_user()).jump_valid = false };
    }
}

/// True if the running thread has a live launch context to return to.
pub fn has_exec_context() -> bool {
    crate::kthread::current_pid() != 0 && unsafe { (*crate::kthread::current_user()).jump_valid }
}

/// Long-jump out of ring-3 back into the kernel context that launched it.
///
/// Callers are fault handlers and ExitProcess.  Because this is a raw `jmp`
/// that unwinds nothing, every spinlock held at this instant would stay
/// locked forever — so we force-release the tracked ones first.  See
/// `lock_guard`.
/// End process `pid` from outside: its thread stops the next time a timer
/// interrupt finds it in user mode (within a tick), or when a blocking
/// syscall it's waiting in checks (`kill_if_requested`). Its exit code must
/// already be set.
pub fn request_kill(pid: u32) {
    crate::kthread::kill_pid(pid);
}

/// Called with the running thread in ring 3 (timer ISRs) or waiting in a
/// syscall: carries out a pending `request_kill` the way ExitProcess would.
/// Returns if nothing is pending.
pub fn kill_if_requested() {
    if crate::kthread::current_pid() != 0 && crate::kthread::take_kill() {
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

    let user = crate::kthread::current_user();
    if crate::kthread::current_pid() == 0 || !unsafe { (*user).jump_valid } {
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
    unsafe { (*user).jump_valid = false };

    unsafe {
        let ctx_ptr = (*user).jump.as_mut_ptr();
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

/// Address space for a TEB (and, for a process, its environment pages):
/// `pages` pages plus a guard page. The pool only moves forward.
pub fn alloc_teb_range(pages: u64) -> u64 {
    static TEB_POOL_BASE: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0x7FF_0000_0000);
    TEB_POOL_BASE.fetch_add((pages + 1) * 0x1000, core::sync::atomic::Ordering::Relaxed)
}

/// Transition from ring 0 to ring 3.
pub unsafe fn launch_into_usermode(entry: u64, stack_top: u64, teb_base: u64) {
    unsafe { launch_into_usermode_with(entry, stack_top, teb_base, 0, 0) }
}

/// Enter ring 3 at `entry` with RSP `stack_top`, GS_BASE `teb_base`, and
/// RCX/RDX holding the first two arguments.
pub unsafe fn launch_into_usermode_with(entry: u64, stack_top: u64, teb_base: u64, arg0: u64, arg1: u64) {
    unsafe {
    let sel = crate::gdt::get_selectors();
    let ucs = sel.user_code_selector.0 as u64;
    let uds = sel.user_data_selector.0 as u64;
    let user = crate::kthread::current_user();
    let ctx_ptr = (*user).jump.as_mut_ptr();

    // Syscalls and interrupts from ring 3 enter on this thread's own stack,
    // below this frame (which the long jump returns to).
    let rsp: u64;
    core::arch::asm!("mov {}, rsp", out(reg) rsp, options(nomem, nostack, preserves_flags));
    crate::kthread::set_kernel_entry_stack((rsp - 512) & !0xF);

    // Arm the long-jump slot *before* entering ring 3 — the very first
    // instruction of the entry point can fault.
    (*user).jump_valid = true;

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
        // The entry point's first two arguments.
        "mov rcx, r12",
        "mov rdx, r13",

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
        in("r12") arg0,
        in("r13") arg1,
        out("rax") _,
        out("rcx") _,
        out("rdx") _,
        clobber_abi("C")
    );

    // Control reaches here only via return_from_usermode(), which already
    // cleared the flag.  Clearing again is harmless and documents intent.
    (*crate::kthread::current_user()).jump_valid = false;
    }
}
