# VEILOS — Design Philosophy & Long-Term Vision

## What is VEILOS?

VEILOS is a bare-metal operating system written in Rust, built around one central idea:
**the user should never have to choose between safety and speed.** Instead, VEILOS makes
that choice a live, runtime toggle — not a compile-time tradeoff, not a different OS install,
just a switch.

Beyond that, VEILOS is built to run Windows software natively. Not in a virtual machine, not
through a compatibility shim bolted onto another OS — VEILOS treats Windows application
compatibility as a first-class concern, at the same level as its own native format.

---

## Core Design Pillars

### 1. The Security / Performance Duality

Every design decision in VEILOS ultimately traces back to its dual-mode architecture.

**Security Mode** is the default. In Security Mode, every process — whether a VEILOS-native
app, a Windows executable, or a driver — runs in Ring 3 (user space), fully isolated from
the kernel and from each other. All calls to hardware, the GPU, the filesystem, and the
network pass through the kernel's capability and audit layer. Malicious software cannot
escalate privileges, cannot read another process's memory, and cannot silently exfiltrate
data. Security Mode is the right choice for everyday use, browsing, productivity, and any
time the machine is exposed to untrusted input.

**Performance Mode** is the fast lane. When the user switches to Performance Mode — via the
toggle in the top-right corner of the desktop — the kernel dynamically patches page tables
to elevate the active process (typically a game or a compute workload) into kernel space.
Ring transitions vanish. GPU memory is mapped directly. The context-switch overhead between
user space and kernel space is eliminated entirely. The goal is bare-metal speeds: the kind
of throughput you would get if the OS simply wasn't there.

Critically, switching modes is a **live operation**. The user doesn't reboot. The kernel
applies the policy change to the active process immediately, and the rest of the system
remains unaffected.

### 2. Windows Compatibility as a First-Class Feature

VEILOS is not a Linux replacement, and it is not a "run Wine and hope for the best" setup.
The Windows NT personality is built into the kernel architecture from the ground up:

- A PE/ELF loader that loads `.exe` files the same way Windows would.
- A syscall translation layer that intercepts NT system calls and maps them to VEILOS native calls.
- Implementations of core Windows DLLs (`kernel32.dll`, `user32.dll`, etc.) built directly into the OS.
- DirectX translation via DXVK (DX9/10/11 → Vulkan) and vkd3d (DX12 → Vulkan).

The ambition is that a user should be able to take a Windows game or application, place it on
the VEILOS filesystem, and run it — with Performance Mode giving it access to GPU resources with
minimal overhead.

### 3. Deep Root Customisation

VEILOS customisation does not stop at themes and wallpapers. The philosophy is that the user
owns the operating system at every level:

- **Surface level:** Colours, fonts, icon styles, panel layout, window decorations.
- **Behavioural level:** How windows group, tile, and focus. How notifications are routed.
  How the taskbar organises running applications.
- **System level:** How the scheduler prioritises processes. How the policy engine treats
  specific applications. Per-app capability profiles that define exactly what an application
  is and isn't allowed to do.
- **Kernel level (advanced):** The architecture is designed so that users who want to can
  load custom policy rules, modify the capability token system, and define their own
  application profiles — without recompiling the kernel.

The design principle is: *if a user has a reason to want it to work differently, VEILOS
should provide a path to make it work differently.*

### 4. The Adaptive Resource Engine

VEILOS tracks how applications actually use the system over time. When a user plays a game
repeatedly, the kernel learns its typical CPU, memory, and GPU envelope — and the next time
the application launches, the scheduler pre-allocates resources accordingly, with a small
buffer, before the application even asks. The result is faster startup, fewer resource
contention spikes, and a system that gets measurably better at running *your* workloads the
longer you use it.

This is not general-purpose AI. It is a practical, observable heuristic: usage history
drives priority and pre-allocation decisions. The data stays local and is used exclusively
to improve the user's experience on their machine.

### 5. Cluster Mode — Distributed Resource Sharing

VEILOS supports a native clustering feature that allows multiple machines running VEILOS to
communicate directly and share computational load across a trusted group of nodes. Connections
can be established over wired links (direct Ethernet or a local switch) or over a peer-to-peer
encrypted tunnel. The goal is not to replace local performance, but to make significantly
greater parallelism available to workloads that can use it.

**Cluster Mode is a Security Mode feature, not a Performance Mode one.** This is a deliberate
design boundary: Performance Mode is about eliminating OS overhead on a single machine for
latency-sensitive workloads like games. Cluster Mode is about expanding the available compute
envelope for parallelisable workloads — rendering, compilation, simulation, inference — where
tasks are large and independent enough to tolerate network round-trips. Attempting to cluster
a game's main loop would be counterproductive; attempting to distribute a render farm across
three machines in the same room is genuinely useful.

#### Node Discovery and Trust

- **Wired discovery** uses a lightweight broadcast protocol over the local network.
- **P2P connections** are established over WireGuard tunnels, consistent with VEILOS's
  Rust-native philosophy and its existing capability security model.
- Nodes authenticate each other using capability tokens. A node that has not been explicitly
  trusted by the user cannot join the cluster. Trust is always user-initiated.

#### Task Capsule Model

Rather than attempting to distribute threads across machines — which would require shared
memory semantics across a network, a fundamentally hard problem — Cluster Mode models
distributable work as **capability-isolated task capsules**: self-contained units with a
defined input/output contract that can be serialised and dispatched to a remote node. This
maps cleanly onto VEILOS's existing process isolation model. Task capsules are VEILOS-native
only; Windows executables are not dispatched to remote nodes.

Applications that wish to use the cluster do so explicitly through the cluster API. Cluster
participation is never transparent or implicit — explicit opt-in avoids the failure modes
that arise when a node drops mid-execution and the application has no awareness of the
distributed context.

#### Resource Broker

A lightweight broker daemon — operating as an extension of the Adaptive Resource Engine —
manages the cluster at runtime:

1. Maintains an up-to-date view of each node's available CPU, memory, and network envelope.
2. Accepts task capsule submissions from the local scheduler.
3. Routes capsules to the most suitable available node based on current load, capability
   match, and estimated network cost.
4. Collects and returns results to the originating process.

#### Filesystem Coherence

Remote nodes need access to the data a task operates on. Rather than requiring full
filesystem replication, VEILOS uses a content-addressed block cache: blocks are identified
by their hash, fetched on demand, and cached locally on each node for the duration of the
task. This keeps data transfer minimal and avoids the complexity of maintaining a fully
synchronised distributed filesystem.

---

## Long-Term Direction

VEILOS is primarily a **gaming-first, capability-secure operating system**. Its identity is
not "a Linux with a different kernel" or "a Windows clone." It is its own thing, built from
scratch in Rust, with a personality that is simultaneously maximally fast for games and
maximally safe for everything else — without those two goals fighting each other.

The long-term trajectory, in rough order of priority:

1. **Single-machine excellence first.** A polished, stable, usable VEILOS on a single
   gaming PC — where the user can do real work in Security Mode and run real games in
   Performance Mode — is the foundation everything else builds on.

2. **Developer platform second.** Once the OS is stable, VEILOS should be a platform that
   Rust developers can target natively. The capability-secure runtime, the predictable memory
   model, and the direct GPU access APIs make it genuinely interesting as a systems
   programming target.

3. **Cluster Mode third.** Local multi-machine clustering — grounded in the single-machine
   model and built on top of the Adaptive Resource Engine — sits between the developer
   platform phase and broader cloud ambitions. The task capsule API is a natural compile
   target for Rust workloads that already use VEILOS's native parallelism primitives.

4. **Distributed and cloud features last.** Remote GPU workloads and cloud gaming host mode
   are interesting future directions, but they are built on top of a solid single-machine OS
   and a working local cluster model — not instead of them.

---

## What VEILOS Is Not

- **Not a general-purpose Linux replacement.** VEILOS has no interest in being another
  distro. It is a distinct OS with its own kernel, its own scheduler, its own filesystem
  layer, and its own application model.
- **Not a Windows fork.** Windows compatibility is implemented through a translation layer,
  not by incorporating Windows code. VEILOS owns its own kernel.
- **Not a virtual machine.** Windows applications run on the VEILOS kernel directly, not
  inside a guest OS. The hypervisor layer (when implemented) is for specific edge cases like
  anti-cheat compatibility, not the primary execution model.
- **Not locked down.** The user has root access to their own machine, always. VEILOS's
  security model protects the user from software, not from themselves.

---

## Design Principles (Summary)

| Principle | Expression in VEILOS |
|---|---|
| Speed when you want it | Performance Mode eliminates OS overhead for the active workload |
| Safety when you need it | Security Mode enforces full capability isolation by default |
| Own your machine | Deep root customisation at every layer, including scheduler and policy |
| Windows compatibility | NT personality is a first-class kernel feature, not an afterthought |
| Gets better over time | Adaptive scheduler learns your workload patterns |
| Rust all the way down | Memory safety at the kernel level; no C runtime |
| Scale when you need it | Cluster Mode distributes parallelisable workloads across trusted nodes |

---

*VEILOS is built by Baily Hyslop.*
