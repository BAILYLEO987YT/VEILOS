# VEILOS Codebase Audit & Design Flaws Report

Based on a thorough review of the `VEILOS_DESIGN.md` vision, the existing `VEILOS_STUB_AUDIT.md`, and the current state of the `src/` directory, here is a detailed breakdown of issues, design flaws, and inperformant code currently present in the VEILOS codebase.

## 1. Design Flaws (Violations of VEILOS Design Goals)

### Performance Mode is Fundamentally Broken
**Goal:** According to `VEILOS_DESIGN.md`, Performance Mode "dynamically patches page tables to elevate the active process... into kernel space. Ring transitions vanish. GPU memory is mapped directly."
**Reality:** 
- In `src/policy.rs`, the switch to `PolicyMode::Performance` within `run_task()` performs a naked, raw `jmp` from Ring 0 directly to the application's entry point (`jmp {entry_point}`).
- **No Page Tables are Patched:** There is no logic that modifies the page table flags (e.g., adding `USER_ACCESSIBLE` or identity mapping the kernel) to actually prepare the address space securely.
- **Undefined Execution Environment:** A direct `jmp` from Ring 0 into user-compiled code without setting up a proper environment (GDT, IDT, stack) will cause undefined behavior or crashes the moment the application attempts to execute a syscall or return. 
- In `src/sandbox.rs` and `src/capability.rs`, Performance Mode just skips capability checks (e.g., `if is_perf { return true; }`) rather than mapping VRAM securely into the user's process space as designed.

## 2. Inperformant Code

### CPU-Dependent Busy Waiting (Spin Loops)
In `src/virtio_net.rs` (around line 187), the driver uses a hardcoded spin loop to wait for the device to reset:
```rust
for _ in 0..1000 { core::hint::spin_loop(); }
```
This is highly inefficient and inaccurate, as the actual delay varies wildly depending on the CPU's clock speed. A proper PIT (Programmable Interval Timer) delay or `delay_ms()` function should be used instead. Similar arbitrary iteration loops exist in `tls.rs`, `stability.rs`, and `users.rs`.

### Memory Leak in Windows Subsystem
In `src/subsystem.rs` (line 1167), the `free()` syscall (`0x9002`) for the Win32 compatibility layer is implemented as a no-op:
```rust
0, // free (no-op leak for now)
```
While `malloc`/`calloc` map real pages, `free` never unmaps them, meaning any complex Windows application will inevitably exhaust the system's memory and crash.

## 3. Major Stubbed / Deceptive Implementations
*(Note: These violate the implicit goal that the system actually performs the work it reports as successful.)*

### Fake Math & Floating Point Intrinsics
In `src/subsystem.rs`, core math intrinsics (`sin`, `cos`, `tan`, `sqrt`, `pow`, etc.) are hardcoded to return identity constants (`0` or `1.0`). Any application or game relying on floating-point geometry or physics will silently compute wildly incorrect results with no error thrown.

### Cryptographic Keys are Not Persisted
In `src/encryption.rs`, `cen_persist_key()` encrypts a key but skips writing it to the VFS. Similarly, `cen_load_keys()` returns an empty array. The code prints `"key persisted"`, but all keys evaporate upon reboot, breaking the encrypted clipboard and drive encryption features.

### Distributed Mode / RDP is Faked
- **Screen Capture:** In `src/distributed.rs`, `capture_framebuffer()` ignores the real GPU framebuffer and instead generates a synthetic RGB gradient test pattern. An RDP client will connect and see a perfectly smooth gradient, hiding the fact that desktop capture is unimplemented.
- **Input Injection:** Mouse inputs received over the network are parsed and then explicitly discarded.
- **Cluster GPU offload:** `remote_gpu_submit()` drops all 3D commands, transmitting only 2D primitives (`FillRect`, `Present`).

### Deceptive Package Manager
In `src/pkgmgr.rs`, the compiler and linker steps (`BuildStep::Compile` and `Link`) do not invoke any toolchain. They instead write 36-byte placeholder files containing hashes of the source code and print a success message to the build log, pretending that in-kernel compilation works.

### Hypervisor Demand Paging is Missing
In `src/hypervisor.rs`, `handle_ept_violation()` intercepts memory faults and prints `"...demand-mapping"`, but resumes the VM without actually mapping any host frames. This causes the VM to infinitely fault on the exact same instruction.
