# win32-input-restore Bugfix Design

## Overview

Three confirmed bugs prevent a Win32 EXE from staying alive long enough to display its window,
corrupt the kernel UI after the EXE exits, and install a null window procedure on every created
window. Serial log analysis pins all three to specific functions and code paths:

1. **GetMessage returns 0 on empty queue** (`win32_user.rs`, syscall `0x4009`) — The empty-queue
   WM_PAINT synthesis path requires `w.visible == true`, but `ShowWindow` has not been called yet
   when the EXE first enters its message loop, so no window is visible. The synthesis is skipped,
   the syscall returns `0`, and the CRT message loop interprets that as WM_QUIT and exits WinMain.

2. **Double-paint race after process exit** (`loader.rs`, `process_execution_task`) — After cleanup
   `LAST_USER_MODE_TICK` is left at its last non-zero value. The timer ISR's `recently_in_user_mode`
   guard remains true for up to 2000 subsequent ticks (~2 s), so both the ISR and `gui_refresh_task`
   repaint every 16 ms. The clock appears to run at half speed and input events are sluggish.

3. **wnd_proc = 0 from empty class name** (`win32_user.rs`, syscall `0x4001`) — The VEILOS 4-arg
   syscall ABI cannot reach the class-name stack argument. `arg1` holds whatever happened to be in
   RDX at the call site, not the class-name pointer the EXE intended. `read_wide` returns an empty
   string, `lookup_class_wndproc` returns `0`, and every created window has a null procedure.

Each fix is a minimal, targeted change with no new lock acquisitions. Together they allow a Win32
EXE to open and keep its window alive, restore the single-repaint cadence after exit, and give
every window a valid window procedure.

---

## Glossary

- **Bug_Condition (C)**: The input condition that causes defective behavior — see each bug's
  formal specification below.
- **Property (P)**: The correct observable outcome when the bug condition holds.
- **Preservation**: All currently correct behaviors that must not change after the fix.
- **`dispatch_win32_user_syscall`**: Function in `src/win32_user.rs` that routes Win32 syscalls
  (`0x4001`–`0x401C`) arriving from user-mode thunks.
- **`process_execution_task`**: Async function in `src/loader.rs` that launches a user-mode PE,
  waits for it to return, and performs post-exit cleanup.
- **`LAST_USER_MODE_TICK`**: `AtomicU64` in `src/interrupts.rs`; the timer ISR stores the current
  tick here whenever it observes ring-3 execution. Used to keep the display alive for 2000 ticks
  after a process runs. Reset to `0` by the fix to end the grace period immediately on exit.
- **`recently_in_user_mode`**: Local boolean in the timer ISR, true when either the interrupted
  frame is ring-3 or `ticks_since_user < 2000`. Controls whether the ISR paints the display.
- **`wnd_proc`**: User-mode function pointer stored in `WindowEntry`; called by the Win32 message
  dispatch loop to handle window messages.
- **`class_entries`**: `Vec<ClassEntry>` in `Win32UserState`; populated by `RegisterClassExW`
  (syscall `0x4000`). Each entry stores the class name and its associated `wnd_proc`.

---

## Bug Details

### Bug 1 — GetMessage Returns 0 on Empty Queue

The bug manifests when a Win32 thread's message queue is empty and it calls `GetMessage` (syscall
`0x4009`) before `ShowWindow` has been called. The synthesis path searches for a window with
`w.visible == true` — none exist yet — so it skips synthesis and returns `0`.

**Formal Specification:**
```
FUNCTION isBugCondition_1(state, tid)
  INPUT:  Win32UserState, calling thread-id
  OUTPUT: boolean

  queue   := state.queues.find(tid)
  windows := state.windows.filter(owning_tid == tid)

  RETURN queue.is_empty()
         AND NOT queue.is_quit()
         AND windows.any()                     -- at least one window exists
         AND NOT windows.any(w.visible == true) -- but none are visible yet
END FUNCTION
```

**Examples:**
- EXE calls `CreateWindow`, window is created with `visible: false`. First `GetMessage` call hits
  empty queue → synthesis skipped → returns `0` → CRT exits WinMain. **Bug.**
- EXE calls `ShowWindow` first, then `GetMessage` on empty queue → synthesis fires → returns
  WM_PAINT. **Not a bug (visible window exists).**
- EXE calls `PostMessage` before `GetMessage` → queue non-empty → fast path returns message.
  **Not a bug (queue not empty).**
- EXE calls `PostQuitMessage` → `queue.is_quit() == true` → `GetMessage` returns `0` intentionally.
  **Not a bug (quit was explicitly posted).**

---

### Bug 2 — Double-Paint Race After Process Exit

The bug manifests when a Win32 process finishes and `process_execution_task` completes cleanup
without resetting `LAST_USER_MODE_TICK`.

**Formal Specification:**
```
FUNCTION isBugCondition_2(state)
  INPUT:  kernel timer state after process_execution_task returns
  OUTPUT: boolean

  RETURN USER_PROCESS_ACTIVE == false
         AND LAST_USER_MODE_TICK != 0
         AND (TICKS - LAST_USER_MODE_TICK) < 2000
END FUNCTION
```

**Examples:**
- EXE exits at tick 50000. `LAST_USER_MODE_TICK` = 50000. At tick 50016, `ticks_since_user = 16 <
  2000` → ISR paints. `gui_refresh_task` also paints. Clock runs at ~half speed. **Bug.**
- After fix: `LAST_USER_MODE_TICK` reset to `0` on exit. `ticks_since_user = 50016 >> 2000` →
  `recently_in_user_mode = false` → ISR skips paint. Only `gui_refresh_task` paints. **Fixed.**

---

### Bug 3 — wnd_proc = 0 from Empty Class Name

The bug manifests when `CreateWindowExW` (syscall `0x4001`) is called and the VEILOS 4-arg syscall
ABI delivers an incorrect value in `arg1` (class-name pointer).

**Formal Specification:**
```
FUNCTION isBugCondition_3(state, arg1)
  INPUT:  Win32UserState, arg1 register value on syscall entry
  OUTPUT: boolean

  class_name := read_wide(arg1)    -- reads UTF-16 from user-mode pointer

  RETURN class_name.is_empty()
         OR lookup_class_wndproc(class_name) == 0
END FUNCTION
```

**Examples:**
- EXE registers class "MainWnd" with wndproc at `0xDEAD_0000`. Calls `CreateWindowExA`. ABI
  delivers wrong pointer in `arg1` → `read_wide` returns `""` → `lookup_class_wndproc("")` = `0` →
  window created with `wnd_proc = 0`. **Bug.**
- After fix: fallback reads `class_entries.last()` → returns `0xDEAD_0000` → window gets valid
  procedure. **Fixed.**
- EXE passes correctly-reachable class name (future ABI fix) → `lookup_class_wndproc` returns
  non-zero → fallback not used. **Preservation.**

---

## Expected Behavior

### Preservation Requirements

**Unchanged Behaviors:**
- When the message queue contains a real (non-synthesized) message, `GetMessage` returns it
  immediately without any synthesis.
- When `PostQuitMessage` has been called, `GetMessage` returns `0` with WM_QUIT, signaling the
  message loop to exit.
- While a user process is alive (`USER_PROCESS_ACTIVE == true`, CPU observed in ring-3), the timer
  ISR continues to drive the display at ~60 fps, completely unaffected by the `LAST_USER_MODE_TICK`
  reset logic.
- When `CreateWindowExW` receives a non-empty, correctly resolved class name, the existing
  `lookup_class_wndproc` path is used and the fallback is not invoked.
- After process exit, `USER_PROCESS_ACTIVE` is set to `false` and `sti` re-enables interrupts; this
  sequence is unchanged.
- `dispatch_key` continues to route keyboard input to `TERMINAL_POOL[0]` and clear stale focus
  state as established by the previous dispatch_key fix.

**Scope:**
All inputs that do NOT satisfy the respective bug conditions are completely unaffected by these
fixes. Each change is confined to its one code path:
- Bug 1 fix touches only the `.find` predicate inside the empty-queue synthesis branch of `0x4009`.
- Bug 2 fix adds a single atomic store after the existing cleanup block.
- Bug 3 fix adds a two-branch `if wnd_proc == 0` guard that is never entered when a class name
  resolves normally.

---

## Hypothesized Root Cause

### Bug 1 Root Cause
The `.visible` filter was written to avoid synthesizing WM_PAINT for windows the user hasn't shown
yet, but Win32 semantics require `GetMessage` to block (never return `0`) until a message or quit
arrives. The filter is simply the wrong predicate — it should be `owning_tid == tid` only, so any
window owned by the calling thread qualifies for synthesis.

### Bug 2 Root Cause
`LAST_USER_MODE_TICK` was introduced to keep the display live for a brief grace period after a
process exits in a crash scenario (where the CPU abruptly leaves ring-3 and `USER_PROCESS_ACTIVE`
isn't cleared until the fault handler runs). The grace period is necessary for crash visibility but
has no mechanism to self-terminate on clean exit. The cleanup in `process_execution_task` clears
`USER_PROCESS_ACTIVE` but never touches `LAST_USER_MODE_TICK`, leaving the ISR painting stale
frames.

### Bug 3 Root Cause
The VEILOS syscall ABI uses the Windows x64 convention re-mapped to SysV registers
(`R10→RSI=arg0, RDX=arg1, R8→RCX=arg2, R9→R8=arg3`). `CreateWindowExA/W` on real Windows has
more than 4 arguments; the class name is typically `arg1` (RDX) in the Windows x64 convention,
but between the thunk (`mov r10, rcx`) and the kernel's register remapping, the value that
arrives in the kernel's `arg1` may not be the class-name pointer. Until the ABI is extended to
pass stack arguments, the class name will often be empty or garbage. The fallback to
`class_entries.last()` is an interim mitigation.

---

## Correctness Properties

Property 1: Bug Condition 1 — GetMessage Synthesizes WM_PAINT on Empty Queue

_For any_ Win32 thread state where the message queue is empty, no WM_QUIT has been posted, and at
least one window exists (regardless of its `visible` state), the fixed `0x4009` handler SHALL
synthesize a WM_PAINT message, enqueue it, dequeue it, write it to the caller's MSG pointer, and
return `1`, ensuring `GetMessage` never returns `0` on a non-quit empty queue.

**Validates: Requirements 2.1, 2.2**

Property 2: Bug Condition 2 — LAST_USER_MODE_TICK Reset on Clean Exit

_For any_ completion of `process_execution_task` (normal exit or crash-recovered exit), the fixed
code SHALL store `0` into `LAST_USER_MODE_TICK` after all cleanup and before the serial log line,
ensuring that `ticks_since_user` immediately exceeds 2000 and the timer ISR's
`recently_in_user_mode` condition becomes false on the very next tick.

**Validates: Requirements 2.3, 2.4**

Property 3: Bug Condition 3 — wnd_proc Fallback on Empty Class Name

_For any_ `CreateWindowExW` invocation where `lookup_class_wndproc` returns `0` (empty or
unresolvable class name), the fixed `0x4001` handler SHALL use `class_entries.last().wnd_proc` as
the window procedure, producing a non-zero `wnd_proc` whenever at least one class has been
registered.

**Validates: Requirements 2.5, 2.6**

Property 4: Preservation — GetMessage Fast Path Unchanged

_For any_ Win32 thread state where the message queue is non-empty, the fixed `0x4009` handler SHALL
return the first queued message immediately, producing exactly the same result as the original code
(the synthesis branch is not entered).

**Validates: Requirements 3.1, 3.2**

Property 5: Preservation — Timer ISR Display Path Unchanged While Process Runs

_For any_ timer tick where `USER_PROCESS_ACTIVE == true` and the interrupted frame is ring-3, the
fixed timer ISR SHALL continue to paint the display at the same ~60 fps cadence (every 16 ticks),
completely unaffected by the `LAST_USER_MODE_TICK` reset because it is only written after
`process_execution_task` completes.

**Validates: Requirements 3.3, 3.5**

Property 6: Preservation — Class Name Lookup Unchanged When Name Resolves

_For any_ `CreateWindowExW` invocation where `lookup_class_wndproc` returns a non-zero value, the
fixed `0x4001` handler SHALL use that value and SHALL NOT invoke the fallback, preserving the
existing class-registry lookup path identically.

**Validates: Requirements 3.4**

---

## Fix Implementation

### Changes Required

**File:** `src/win32_user.rs`

**Change 1 — Bug 1: Remove `.visible` filter in 0x4009 empty-queue synthesis**

**Function:** `dispatch_win32_user_syscall`, match arm `0x4009`

**Specific Change:**
```rust
// BEFORE
if let Some(hwnd) = state.windows.iter()
    .find(|w| w.owning_tid == tid && w.visible)
    .map(|w| w.hwnd)

// AFTER
if let Some(hwnd) = state.windows.iter()
    .find(|w| w.owning_tid == tid)
    .map(|w| w.hwnd)
```

The predicate `&& w.visible` is the sole cause of Bug 1. Removing it makes the synthesis fire for
any thread-owned window, matching Win32 semantics where `GetMessage` blocks until a message
arrives, never returning 0 on a non-quit queue.

---

**Change 2 — Bug 3: Add wnd_proc fallback in 0x4001 CreateWindowExW**

**Function:** `dispatch_win32_user_syscall`, match arm `0x4001`

**Specific Change** (insert immediately after the `lookup_class_wndproc` call):
```rust
let wnd_proc = state.lookup_class_wndproc(&class_name);
// Fallback: if class name was empty (4-arg ABI couldn't reach it) or not found,
// use the most recently registered class's wndproc.
let wnd_proc = if wnd_proc == 0 {
    state.class_entries.last().map(|c| c.wnd_proc).unwrap_or(0)
} else {
    wnd_proc
};
```

No new locks: `class_entries` is accessed through the `WIN32_USER` lock already held by the
caller throughout this match arm.

---

**File:** `src/loader.rs`

**Change 3 — Bug 2: Reset LAST_USER_MODE_TICK after cleanup**

**Function:** `process_execution_task` (async fn, single-CPU path inside the `else` block)

**Specific Change** (insert after the terminal re-focus block, before the `serial_println!`):
```rust
// Reset LAST_USER_MODE_TICK so the timer ISR's recently_in_user_mode grace period
// ends immediately, preventing the ISR from painting alongside gui_refresh_task.
crate::interrupts::LAST_USER_MODE_TICK.store(0, core::sync::atomic::Ordering::Relaxed);
```

`LAST_USER_MODE_TICK` is an `AtomicU64`; no lock is needed or acquired. The store uses
`Relaxed` ordering, matching the existing load/store sites in the timer ISR.

---

### Lock-Order Analysis

| Change | Locks acquired | Notes |
|--------|---------------|-------|
| Bug 1  | none          | Modifies only the search predicate; `WIN32_USER` already held |
| Bug 2  | none          | `AtomicU64::store` — no mutex involved |
| Bug 3  | none          | `class_entries` read through already-held `WIN32_USER` lock |

No new lock acquisitions means no risk of deadlock or lock-order inversion.

---

## Testing Strategy

### Validation Approach

The testing strategy follows a two-phase approach: first surface counterexamples on unfixed code
to confirm root causes, then verify the fixes and guard against regressions.

---

### Exploratory Bug Condition Checking

**Goal:** Confirm each root cause by running tests on the *unfixed* code and observing failures.

**Test Plan:** Construct minimal in-kernel unit tests that exercise each syscall path directly,
without loading a full PE. Run on unfixed code → expect specific failure patterns. Re-run on
fixed code → expect all assertions to pass.

**Test Cases:**

1. **Bug 1 — GetMessage with invisible window (will fail on unfixed code)**
   Create a `Win32UserState`, register a thread, add one window with `visible: false`, call
   `get_message(tid, true)` on an empty queue. Assert result is `Some(WM_PAINT)`. On unfixed
   code, result is `None` (synthesis skipped).

2. **Bug 1 — GetMessage with no windows (must stay None on both unfixed and fixed code)**
   Create a `Win32UserState`, register a thread with no windows, call `get_message` on empty queue.
   Assert result is `None`. This must remain `None` after the fix (no window → nothing to
   synthesize WM_PAINT for).

3. **Bug 2 — LAST_USER_MODE_TICK reset (will fail on unfixed code)**
   Simulate `process_execution_task` cleanup by reading `LAST_USER_MODE_TICK` after the cleanup
   block. On unfixed code, value is non-zero. After fix, value is `0`. Assert `== 0`.

4. **Bug 3 — CreateWindowExW with empty class name (will fail on unfixed code)**
   Create a `Win32UserState`, push one `ClassEntry` with `wnd_proc = 0xDEAD_0000`. Call
   `create_window("", ...)`. On unfixed code, `wnd_proc` in the resulting `WindowEntry` is `0`.
   After fix, it is `0xDEAD_0000`.

5. **Edge case — GetMessage returns 0 for WM_QUIT (must pass on both)**
   Post a quit message via `post_quit(tid, 0)`, then call `get_message(tid, true)`. Assert
   message is `WM_QUIT` and the synthesized WM_PAINT path is never entered.

**Expected Counterexamples (unfixed code):**
- Bug 1: `get_message` returns `None` (0 at syscall boundary) when window is invisible.
- Bug 2: `LAST_USER_MODE_TICK` is non-zero after cleanup, causing ISR to paint for ~2 s.
- Bug 3: `WindowEntry.wnd_proc == 0` when class name is empty.

---

### Fix Checking

**Goal:** Verify that for all inputs where each bug condition holds, the fixed function produces
the expected correct behavior.

**Pseudocode (Bug 1):**
```
FOR ALL (state, tid) WHERE isBugCondition_1(state, tid) DO
  result := get_message_fixed(state, tid, remove=true)
  ASSERT result.is_some()
  ASSERT result.message == WM_PAINT
  ASSERT return_value != 0
END FOR
```

**Pseudocode (Bug 2):**
```
FOR ALL process_exit_events DO
  run process_execution_task_cleanup_fixed()
  ASSERT LAST_USER_MODE_TICK == 0
  ASSERT (TICKS - LAST_USER_MODE_TICK) > 2000
END FOR
```

**Pseudocode (Bug 3):**
```
FOR ALL (state, arg1) WHERE isBugCondition_3(state, arg1)
                         AND state.class_entries.non_empty() DO
  hwnd := create_window_fixed(state, "", ...)
  entry := state.find_window(hwnd)
  ASSERT entry.wnd_proc != 0
  ASSERT entry.wnd_proc == state.class_entries.last().wnd_proc
END FOR
```

---

### Preservation Checking

**Goal:** Verify that for all inputs where the bug condition does NOT hold, the fixed functions
produce the same result as the original functions.

**Pseudocode (Bug 1 — queue non-empty):**
```
FOR ALL (state, tid) WHERE NOT isBugCondition_1(state, tid) DO
  ASSERT get_message_original(state, tid) == get_message_fixed(state, tid)
END FOR
```

**Testing Approach:** Property-based testing is recommended for the preservation cases because:
- `Win32UserState` has many combinations of queue contents, window visibility, and thread IDs.
- PBT generates non-empty queues, quit-posted queues, and zero-window scenarios automatically.
- It provides a strong guarantee that the fast path (non-empty queue) and the WM_QUIT path are
  bit-for-bit unchanged.

**Preservation Test Cases:**

1. **Queue non-empty** — generate random `Msg` values, push them to the queue, call `get_message`.
   Assert the returned message is the head of the queue (not a synthesized WM_PAINT).

2. **WM_QUIT path** — call `post_quit`, then `get_message`. Assert result is `WM_QUIT`, return
   value is `0`, and the WM_PAINT synthesis branch is not entered.

3. **No windows** — empty `windows` vec, empty queue. Assert `get_message` returns `None` / `0`
   (no window to synthesize for; behavior unchanged from original).

4. **Class name resolves** — push a class entry, call `create_window` with the matching name.
   Assert `wnd_proc == lookup_class_wndproc(name)` (fallback not invoked).

5. **Live process ISR** — set `USER_PROCESS_ACTIVE = true`, simulate timer tick with ring-3 CS.
   Assert `recently_in_user_mode == true` (display path still active; reset not yet written).

---

### Unit Tests

- Test `get_message` with invisible window → `Some(WM_PAINT)` after fix, `None` before.
- Test `get_message` with non-empty queue → head message returned unchanged.
- Test `get_message` after `post_quit` → `WM_QUIT`, return 0.
- Test `create_window("")` with one registered class → `wnd_proc == class_entries[0].wnd_proc`.
- Test `create_window("MatchingName")` → `wnd_proc == lookup_class_wndproc("MatchingName")`.
- Test `LAST_USER_MODE_TICK` value after simulated `process_execution_task` cleanup → `0`.

### Property-Based Tests

- Generate random queue states: assert that if queue is non-empty, `get_message` returns the
  head item and never synthesizes WM_PAINT.
- Generate random window lists (various `visible` values, various `owning_tid`): assert that if
  any window matches `owning_tid == tid` and queue is empty (non-quit), `get_message` returns
  `Some(WM_PAINT)`.
- Generate random `class_entries` vecs and random (possibly empty) class names: assert that if
  `lookup_class_wndproc` returns `0` and `class_entries` is non-empty, the created window's
  `wnd_proc` equals `class_entries.last().wnd_proc`.
- Generate random tick values with `USER_PROCESS_ACTIVE = false` and `LAST_USER_MODE_TICK = 0`:
  assert `ticks_since_user > 2000` for all reasonable current-tick values ≥ 2001.

### Integration Tests

- Load `gui_test.exe` (present in `storage/desktop/`), trace the full syscall sequence from
  `RegisterClassExW` through `CreateWindowExW` → `ShowWindow` → `GetMessage` loop. Confirm
  the EXE does not exit before `ShowWindow` is reached.
- After EXE exits cleanly, verify the taskbar clock advances at exactly 1 second per 1000 ticks
  (no double-paint race). Observable via serial log tick stamps around `paint_desktop` calls.
- After fix, confirm that a second `LoadPE` call in the same session correctly re-uses the last
  registered class when the ABI delivers an empty class name.
