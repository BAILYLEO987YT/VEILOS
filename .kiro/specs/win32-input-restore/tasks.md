# Implementation Plan

- [x] 1. Write bug condition exploration tests (BEFORE implementing any fix)
  - **Property 1: Bug Condition** - GetMessage/wndproc/double-paint Tripled Bugs
  - **CRITICAL**: These tests MUST FAIL on unfixed code — failure confirms each bug exists
  - **DO NOT attempt to fix the test or the code when it fails**
  - **NOTE**: These tests encode the expected behavior — they validate the fixes when they pass after implementation
  - **GOAL**: Surface counterexamples that demonstrate all three bugs on the unfixed codebase
  - **Scoped PBT Approach**: Each sub-test is scoped to the concrete failing input that reproducibly triggers its bug condition

  - **Sub-test A — Bug 1: GetMessage returns 0 when window is invisible (queue empty, no WM_QUIT, window visible=false)**
    - Create a `Win32UserState`, register a thread ID (e.g. `tid = 1`)
    - Call `state.create_window("MainWnd", "Test", 0,0,800,600, 0, 0, 0xDEAD_0000, tid, 0)` — window defaults to `visible: false`
    - Call `state.get_message(tid, true)` on the now-empty queue
    - Assert result is `Some(msg)` where `msg.message == WM_PAINT` (synthesized)
    - On UNFIXED code: the synthesis predicate includes `&& w.visible` → no visible window → synthesis skipped → `get_message` returns `None` → test FAILS
    - Bug condition from design: `isBugCondition_1` — queue empty AND no WM_QUIT AND at least one thread-owned window exists with `visible == false`
    - Document counterexample: `get_message` returns `None` instead of `Some(WM_PAINT)`

  - **Sub-test B — Bug 3: CreateWindowExW gets wnd_proc=0 when class name is empty (4-arg ABI delivers empty string)**
    - Create a `Win32UserState`, register one `ClassEntry` with `wnd_proc = 0xDEAD_0000`
    - Call `state.create_window("", "Test", 0,0,800,600, 0, 0, state.lookup_class_wndproc(""), tid, 0)`
      - `lookup_class_wndproc("")` returns `0` on both unfixed and fixed code — the bug is that no fallback exists
    - Find the created window via `state.find_window(hwnd)`
    - Assert `entry.wnd_proc == 0xDEAD_0000`
    - On UNFIXED code: no fallback → `wnd_proc = 0` → test FAILS
    - Bug condition from design: `isBugCondition_3` — `read_wide(arg1)` returns empty string → `lookup_class_wndproc` returns `0`
    - Document counterexample: `WindowEntry.wnd_proc == 0` when class name is `""`

  - **Sub-test C — Bug 2: LAST_USER_MODE_TICK is non-zero after process_execution_task cleanup**
    - Set `crate::interrupts::LAST_USER_MODE_TICK.store(50000, Relaxed)` to simulate a tick value from an active process
    - Run (or simulate) the post-exit cleanup block in `on_process_exit` / `process_execution_task`
    - Read `crate::interrupts::LAST_USER_MODE_TICK.load(Relaxed)`
    - Assert value `== 0`
    - On UNFIXED code: cleanup never stores `0` → `LAST_USER_MODE_TICK` stays at `50000` → `ticks_since_user = 50016 < 2000` → ISR keeps painting → test FAILS
    - Bug condition from design: `isBugCondition_2` — `USER_PROCESS_ACTIVE == false AND LAST_USER_MODE_TICK != 0 AND (TICKS - LAST_USER_MODE_TICK) < 2000`
    - Document counterexample: `LAST_USER_MODE_TICK = 50000` after cleanup; timer ISR would continue painting for ~2 s

  - Run all three sub-tests on UNFIXED code
  - **EXPECTED OUTCOME**: All three sub-tests FAIL (this is correct — it proves all three bugs exist)
  - Mark task complete when tests are written, run, and failures documented
  - _Requirements: 1.1, 1.2, 1.3, 1.4, 1.5, 1.6_

- [x] 2. Write preservation property tests (BEFORE implementing any fix)
  - **Property 2: Preservation** - Non-Buggy Paths Unchanged
  - **IMPORTANT**: Follow observation-first methodology — observe UNFIXED code behavior for inputs where no bug condition holds
  - Observe behavior on UNFIXED code for the following non-buggy inputs:

  - **Preservation A — GetMessage fast path: queue non-empty (Bug 1 non-triggering)**
    - Observe: push a `WM_TIMER` message to `tid`'s queue, then call `get_message(tid, true)` → returns `Some(WM_TIMER msg)` immediately, no synthesis
    - Write property: for all non-empty queue states, `get_message` returns the head message without modification
    - Verify test PASSES on UNFIXED code (this path was always correct)

  - **Preservation B — WM_QUIT path unchanged (Bug 1 non-triggering)**
    - Observe: call `state.post_quit(tid, 0)`, then call syscall `0x4009` → returns `0` with a `WM_QUIT` message
    - Write property: after `post_quit`, `get_message` returns `0` (not `1`) and the message is `WM_QUIT`
    - Verify test PASSES on UNFIXED code

  - **Preservation C — Class name resolves normally (Bug 3 non-triggering)**
    - Observe: register class "MainWnd" with `wnd_proc = 0xDEAD_0000`, call `create_window("MainWnd", ...)` → `lookup_class_wndproc("MainWnd")` returns `0xDEAD_0000` → `wnd_proc` in created window equals `0xDEAD_0000`, fallback never triggered
    - Write property: for all `create_window` calls where `lookup_class_wndproc` returns non-zero, the created window's `wnd_proc` equals that value (fallback not invoked)
    - Verify test PASSES on UNFIXED code

  - **Preservation D — ISR display path unchanged while process is alive (Bug 2 non-triggering)**
    - Observe: with `USER_PROCESS_ACTIVE = true` and a recent ring-3 tick, `recently_in_user_mode` is `true` → ISR drives display at ~60 fps
    - Write property: `LAST_USER_MODE_TICK` reset is only triggered after `process_execution_task` cleanup; it is never written while `USER_PROCESS_ACTIVE == true`
    - Verify test PASSES on UNFIXED code (the reset doesn't exist yet, so this is trivially true — record for regression use after fix)

  - Run all four preservation tests on UNFIXED code
  - **EXPECTED OUTCOME**: All four preservation tests PASS (confirms baseline behavior to preserve)
  - Mark task complete when tests are written, run, and passing on unfixed code
  - _Requirements: 3.1, 3.2, 3.3, 3.4, 3.5_

- [x] 3. Fix all three bugs

  - [x] 3.1 Bug 1 fix — remove `&& w.visible` predicate in `0x4009` empty-queue synthesis (`src/win32_user.rs`)
    - In `dispatch_win32_user_syscall`, match arm `0x4009`, locate the synthesis block:
      ```rust
      if let Some(hwnd) = state.windows.iter()
          .find(|w| w.owning_tid == tid && w.visible)
          .map(|w| w.hwnd)
      ```
    - Change to (remove `&& w.visible`):
      ```rust
      if let Some(hwnd) = state.windows.iter()
          .find(|w| w.owning_tid == tid)
          .map(|w| w.hwnd)
      ```
    - The `&& w.visible` predicate is the sole cause: it prevents WM_PAINT synthesis before `ShowWindow` has been called, causing `GetMessage` to return `0` which the CRT message loop treats as WM_QUIT
    - _Bug_Condition: isBugCondition_1 — queue empty AND no WM_QUIT AND thread owns a window with visible=false_
    - _Expected_Behavior: synthesis fires for any thread-owned window regardless of visible state; GetMessage returns 1 with WM_PAINT_
    - _Preservation: fast path (non-empty queue) and WM_QUIT path are not touched by this change_
    - _Requirements: 2.1, 2.2_

  - [x] 3.2 Bug 3 fix — add `wnd_proc` fallback in `0x4001` CreateWindowExW (`src/win32_user.rs`)
    - In `dispatch_win32_user_syscall`, match arm `0x4001`, locate the line:
      ```rust
      let wnd_proc = state.lookup_class_wndproc(&class_name);
      ```
    - Immediately after that line, insert:
      ```rust
      // Fallback: if the 4-arg ABI delivered an empty/unresolvable class name,
      // use the most recently registered class's wndproc so the window is usable.
      let wnd_proc = if wnd_proc == 0 {
          state.class_entries.last().map(|c| c.wnd_proc).unwrap_or(0)
      } else {
          wnd_proc
      };
      ```
    - No new locks: `class_entries` is already accessible through the `WIN32_USER` lock held for this entire match arm
    - _Bug_Condition: isBugCondition_3 — class_name parsed as empty string (4-arg ABI cannot reach stack arg); lookup_class_wndproc returns 0_
    - _Expected_Behavior: window gets wnd_proc from class_entries.last(); wnd_proc is non-zero when at least one class is registered_
    - _Preservation: if lookup_class_wndproc returns non-zero (name resolves), the else branch preserves the original value exactly — fallback is never entered_
    - _Requirements: 2.5, 2.6_

  - [x] 3.3 Bug 2 fix — reset `LAST_USER_MODE_TICK` to `0` after cleanup in `on_process_exit` (`src/loader.rs`)
    - In `on_process_exit`, locate the terminal re-focus block ending with `wm.focus(term_vid)` and the `release_process_memory` call, then find the `serial_println!` line:
      ```rust
      crate::serial_println!(
          "[EXIT] Pid={} torn down. {} frames on free list.",
          ...
      );
      ```
    - Insert the following immediately before that `serial_println!`:
      ```rust
      // Reset LAST_USER_MODE_TICK so the timer ISR's recently_in_user_mode
      // grace period ends immediately, preventing the ISR from painting
      // alongside gui_refresh_task after clean process exit.
      crate::interrupts::LAST_USER_MODE_TICK.store(0, core::sync::atomic::Ordering::Relaxed);
      ```
    - `LAST_USER_MODE_TICK` is an `AtomicU64`; no mutex needed. `Relaxed` ordering matches existing load/store sites in the timer ISR
    - _Bug_Condition: isBugCondition_2 — USER_PROCESS_ACTIVE==false AND LAST_USER_MODE_TICK != 0 AND (TICKS - LAST_USER_MODE_TICK) < 2000_
    - _Expected_Behavior: ticks_since_user immediately exceeds 2000; recently_in_user_mode becomes false on the next timer tick; only gui_refresh_task drives the display_
    - _Preservation: the reset is placed after all cleanup; USER_PROCESS_ACTIVE, sti, and Win32 teardown sequence are unmodified; ISR display path while a process is alive is completely unaffected_
    - _Requirements: 2.3, 2.4_

  - [x] 3.4 Run `cargo check` to verify zero compiler errors
    - Run `cargo check` from workspace root `c:\Users\Baily\OneDrive\code\Rust\VEILOS`
    - Target: `x86_64-unknown-none` (or the configured target in `rust-toolchain.toml`)
    - Confirm zero errors before proceeding
    - If errors arise, fix them in place before moving to step 3.5
    - _Requirements: 2.1, 2.2, 2.3, 2.4, 2.5, 2.6_

  - [x] 3.5 Verify bug condition exploration tests now pass
    - **Property 1: Expected Behavior** - GetMessage/wndproc/double-paint Tripled Bugs
    - **IMPORTANT**: Re-run the SAME three sub-tests from task 1 — do NOT write new tests
    - Re-run Sub-test A: `get_message` with invisible window → assert result is `Some(WM_PAINT)` (synthesis now fires without `w.visible` guard)
    - Re-run Sub-test B: `create_window("")` with one registered class → assert `wnd_proc == 0xDEAD_0000` (fallback now in place)
    - Re-run Sub-test C: `LAST_USER_MODE_TICK` after simulated cleanup → assert value `== 0` (reset now inserted)
    - **EXPECTED OUTCOME**: All three sub-tests PASS (confirms all three bugs are fixed)
    - _Requirements: 2.1, 2.2, 2.3, 2.4, 2.5, 2.6_

  - [x] 3.6 Verify preservation tests still pass
    - **Property 2: Preservation** - Non-Buggy Paths Unchanged
    - **IMPORTANT**: Re-run the SAME four preservation tests from task 2 — do NOT write new tests
    - Re-run Preservation A: non-empty queue → head message returned, no synthesis (fast path unchanged)
    - Re-run Preservation B: WM_QUIT path → `get_message` returns `0` with `WM_QUIT` (quit path unchanged)
    - Re-run Preservation C: non-empty class name resolves → `wnd_proc == lookup_class_wndproc(name)`, fallback not invoked (lookup path unchanged)
    - Re-run Preservation D: `USER_PROCESS_ACTIVE == true` → `LAST_USER_MODE_TICK` never reset while process is alive (ISR path unchanged)
    - **EXPECTED OUTCOME**: All four preservation tests PASS (confirms no regressions)
    - _Requirements: 3.1, 3.2, 3.3, 3.4, 3.5_

- [x] 4. Checkpoint — Ensure all tests pass
  - Verify bug condition exploration tests (Property 1, all three sub-tests A/B/C) pass
  - Verify preservation property tests (Property 2, all four sub-tests A/B/C/D) pass
  - Verify `cargo check` reports zero errors
  - Confirm the clock runs at correct speed (no double-paint) via serial log tick stamps around `paint_desktop` calls after EXE exit, if integration testing is possible
  - Ask the user if any questions arise before marking the spec done
