# VEILOS Win32: design proposal

**Status:** W1 done (2026-10-30). Next: W2.

## Goal

Run unmodified Windows x64 programs. Console programs come first: test programs built with MSVC or clang, then Windows' own `cmd.exe`. GUI programs come after, then games.

**Agreed 2026-10-29:**
- The Windows API is implemented as **user-mode DLLs** (the Wine/ReactOS approach). VEILOS ships its own `ntdll.dll`, `kernel32.dll`, CRT and later `user32.dll`, written in Rust and built as real PE DLLs. They run in ring 3 on top of a small set of NT-style syscalls.
- The first target is **console programs**.

## What exists today

| Area | State |
|---|---|
| Loader (`loader.rs`) | Maps the EXE into its own address space and sets up a TEB, PEB and KUSER_SHARED_DATA. Imports are resolved one of three ways: (1) a DLL found in `/SYSTEM` is mapped and relocated, but its own imports are never resolved and `DllMain` never runs; (2) a name in the built-in registry (`dll.rs`, ~630 functions) gets a `mov r10,rcx; mov eax,ID; syscall; ret` thunk into `subsystem.rs`; (3) anything else points at a shared "return 0" stub. |
| Import gaps | No API-set mapping: `api-ms-win-core-*` names aren't forwarded to kernel32, so most modern imports fail. Data imports (`_fmode`, `_acmdln`, `__argc`) point at the read-only stub page, so the program crashes on its first write. Ordinal imports and delay-load imports are skipped. Each DLL's thunks live in a single 4 KiB page, which overflows above 256 imports. |
| Win32 functions | `subsystem.rs` dispatches ~440 syscall IDs; 256 of them return a constant (`ReadFile` → FALSE, `WaitForSingleObject` → signalled, `GetLastError` → 0, ...). CRT functions such as `strlen`, `malloc` and the math functions are each a syscall. |
| Processes | One Windows process at a time (`USER_PROCESS_SLOT`), because syscalls go to the single "active" process. No `CreateProcess`. |
| Console | Launching an EXE opens a terminal window with a canned banner; the program's output goes to the serial log, not the window. |

**Measured (2026-10-29, QEMU):**
- `gui_test.exe` (3 KiB, 9 imports) opens its window. Its content stays empty, and closing it leaves the process running.
- `cmd.exe` (Windows 10, 271 imports from msvcrt, ntdll and 35 API-set DLLs) gets 14 imports resolved. It page-faults right after start, writing a msvcrt data variable that points into the stub page.

## Architecture

```
  program.exe
     │ imports (by name, ordinal, or API-set name)
  ─────────────────────────────────────────────────────────── ring 3
  ucrtbase.dll / msvcrt.dll / vcruntime140.dll   CRT: stdio, printf, malloc, strings, math, startup
  kernel32.dll (+ kernelbase.dll)                Win32: files, console, processes, sync, modules, errors
  ntdll.dll                                      Rtl (heap, strings, paths), Ldr (loader), Nt* syscall stubs
  ─────────────────────────────────────────────────────────── syscall
  kernel: NT services                            memory, files, console, processes/threads, objects, waits
```

- **System DLLs** are Rust `no_std` crates in a `win32/` workspace, built for `x86_64-pc-windows-msvc` with `lld-link` and no CRT. A build step copies them to `storage/system`. Each one exports exactly what real Windows exports under the same names, so programs link to them unchanged.
- **One syscall ABI** is shared by kernel and DLLs: an `nt_abi` crate with the syscall numbers and structs. Syscalls take arguments in registers plus the user stack, as NT's do (`syscall_stack_arg` already exists). The ~440 `subsystem.rs` handlers are reworked into a much smaller set of NT-style services; logic such as heaps, string conversion and CRT code moves to user mode.
- **The loader** (in the kernel first, moving into `ntdll`'s `Ldr` later):
  - maps the EXE and every DLL it needs, once per process;
  - resolves imports recursively, including ordinals, data exports, forwarded exports and the API-set map (`api-ms-win-*`, `ext-ms-win-*` → the DLL that implements them);
  - applies relocations and section protections (read-only, read-execute, read-write, NX);
  - runs TLS callbacks and `DllMain` in dependency order;
  - lists modules in `PEB.Ldr` (`GetModuleHandle`, `GetProcAddress` and the CRT rely on it).
- **The existing kernel thunks stay** as a fallback for any name no DLL provides, so `gui_test.exe` keeps working while GUI support moves to `user32.dll` later.
- **Console:** each console program gets a VEILOS terminal window as its console. `WriteFile`/`WriteConsole` on stdout/stderr appear in it, and keyboard input in that window feeds stdin. A terminal `RUN` command starts a program with arguments and shows its output inline.

## Proposed order

Each step is verified in QEMU before the next one starts, and comes with test programs (C, built with `clang-cl`, kept in `win32/tests/` and copied to `storage/tests/`).

| Step | Scope | Done when |
|---|---|---|
| W1 ✅ | **DLL loader** (`modules.rs`): DLLs from `/SYSTEM`, then the EXE's folder, loaded once per process and their imports resolved in turn. Imports bind by name or ordinal to code or data, following forwarders; API-set names map to their host DLL (`api_set_host`: `api-ms-win-crt-*` → `ucrtbase.dll`, the rest of `api-ms-win-*` → `kernel32.dll`). DLLs are relocated (one that can't move — relocations stripped — loads at its preferred base or not at all). Static TLS per module with callbacks; a generated startup routine runs TLS callbacks and `DllMain` dependencies-first, then the EXE entry; `PEB.Ldr` lists the modules; each section gets its own protection (read-only/writable, NX unless code). Names no DLL exports fall back to the kernel's built-ins; anything else gets its own stub that logs `dll!name` on first call and is writable, so data imports nothing provides don't fault. Thunks have their own growing region (the 256-per-page overflow is gone), and the loader's regions are recorded in the process's VAD so `VirtualAlloc` can't hand them out. `OutputDebugString` now logs to serial; the terminal's `RUN <path>` starts a program. **Follow-up fixes (2026-10-30):** a returning entry point ends the process with its value (the startup routine calls `ExitProcess`, as `BaseThreadInitThunk` does; it used to return to address 0); `GetModuleHandle(NULL)` returns the EXE's base again (it was derived from the entry point, now the startup routine). Closing a Win32 window sends `WM_CLOSE` only, as on Windows (it also forced `WM_QUIT`, which would end apps that ask to save or hide to the tray); if the app then has no windows left but is still running 3 s later, VEILOS ends it, like an automatic "End task" (`win32_user::check_close_watch`; a timer interrupt carries out the kill the next time it finds the process in user mode, `loader::request_kill`). `gui_test.exe` needs this: after its message loop it spins in `jmp $` forever. Not in W1: delay-load imports and `LoadLibrary`/`GetProcAddress` at run time (they need the loader at run time, W4). | `w1test.exe` (`win32/tests/w1`, built with `clang-cl`, no CRT): 12/12 checks — import by name, a DLL that needs relocating, a data export set by `DllMain`, `DllMain` order, a forwarder, an ordinal, an API-set import, TLS data, a TLS write, a TLS callback, an unresolved function, an unresolved data import — and again in a second run in the same session. `w1ret.exe` (entry point returns 7) exits with 7, twice. `gui_test.exe` opens its window, closes with ✕, is ended 3 s later and opens again. `cmd.exe`: 95 of 271 imports now bind (14 before, mostly through the API-set map); it runs further into startup and faults later, at the first of the 176 functions still missing |
| W2 | **Build and ABI:** the `win32/` workspace (`no_std`, `lld-link`), the `nt_abi` crate, a build script that installs DLLs into `storage/system`. Minimal `ntdll.dll` and `kernel32.dll` with `NtTerminateProcess`, `NtWriteFile` to a console handle, `GetStdHandle` and `WriteFile`. | A C program calling only `WriteFile` + `ExitProcess` prints to its terminal window and exits with its code |
| W3 | **Console and processes:** the console window per process (output, keyboard input, closing), the terminal `RUN` command, exit codes. Per-process syscall dispatch (current process from per-CPU state), dropping the one-process limit. | Two console programs run at once, each in its own window; exit codes are reported |
| W4 | **ntdll + kernel32 core:** virtual memory, the heap in user mode (`RtlAllocateHeap`), files over the VFS (`CreateFile`, `ReadFile`, `WriteFile`, `FindFirstFile`, attributes, paths), command line and environment, `GetLastError` in the TEB, critical sections and events, time, `LoadLibrary`/`GetProcAddress`. | Test programs for files, the heap, arguments and the environment pass |
| W5 | **CRT:** `ucrtbase.dll`, `vcruntime140.dll` and `msvcrt.dll`: startup (`_initterm`, argv), stdio and the `printf`/`scanf` families, strings, memory, `malloc`, math on SSE2, `exit`/`atexit`. Exceptions: `RtlLookupFunctionEntry`, `RtlVirtualUnwind`, `__C_specific_handler` (SEH), then C++ exceptions. | A `printf` test suite matches its output on Windows; a C++ program using exceptions runs |
| W6 | **Child processes:** `CreateProcess` (console inheritance, handles, wait), `WaitForSingleObject` on processes and threads, real threads (`CreateThread`, TLS slots). | A program starts a child, waits for it and reads its exit code; two threads with a critical section |
| W7 | **`cmd.exe` milestone:** the remaining imports it needs (registry reads, locale, console modes, file system queries). | `cmd.exe` starts, and `echo`, `dir`, `cd`, `type`, `set` and launching another program work |
| Later | **GUI:** `user32.dll`/`gdi32.dll` over window-manager syscalls, painting into the desktop, messages, closing a window → `WM_CLOSE` → exit. Then **games:** Direct3D/OpenGL onto the logical GPU. | — |

## Open questions (decided as each step starts)

- **Real Microsoft DLLs.** With our `ntdll`/`kernel32` in place, a user could copy their own `ucrtbase.dll` or `vcruntime140.dll` from Windows into `/SYSTEM` and the loader would prefer it. VEILOS can't ship them (license), but allowing them gives better CRT compatibility early. Allowed but not relied on?
- **Exceptions.** Some programs need SEH early: MSVC `/GS` checks and `__C_specific_handler` appear even in C programs. If a W2–W4 test program needs it, part of W5's exception work moves earlier.
- **Interrupts during syscalls.** Interrupts are still off for a whole syscall (audit tier 1, item 6). Blocking console reads in W3 need them on (or the read to sleep), so W3 turns them on.
