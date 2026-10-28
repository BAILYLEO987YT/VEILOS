# Bugfix Requirements Document

## Introduction

Three related bugs cause a Win32 EXE to exit immediately after `CreateWindow`, leave the kernel
display pipeline in a broken double-paint state after the EXE closes, and result in WM_PAINT
messages being delivered to a null window procedure. Together they prevent any Win32 application
from staying alive long enough to display its window, and corrupt the kernel UI (slow clock,
input loss) in the aftermath.

The bugs are confirmed by serial log evidence: `GetMessage` (syscall `0x4009`) returns `0` on an
empty queue, which the EXE's CRT message loop treats as WM_QUIT, causing it to exit WinMain
before `ShowWindow` is ever called. After exit, `LAST_USER_MODE_TICK` keeps the timer-ISR display
path active for 2000 ticks alongside `gui_refresh_task`, halving the effective repaint rate and
making the taskbar clock appear to run at half speed. Additionally, `CreateWindowExW` receives an
empty class name due to the 4-arg syscall ABI not reaching the class-name stack argument, so
`wnd_proc` is always resolved as `0x0`.

## Bug Analysis

### Current Behavior (Defect)

1.1 WHEN a Win32 thread calls GetMessage and its message queue is empty and no WM_QUIT has been
posted THEN the system returns `0`, which Win32 applications interpret as WM_QUIT, causing the
message loop to exit prematurely.

1.2 WHEN GetMessage is called and the only windows owned by the calling thread have `visible:
false` (i.e. ShowWindow has not yet been called) THEN the system skips WM_PAINT synthesis and
returns `0`, causing the EXE to exit before ShowWindow is ever reached.

1.3 WHEN a Win32 process exits or is terminated and `process_execution_task` completes its cleanup
THEN the system leaves `LAST_USER_MODE_TICK` at its last non-zero value, causing the timer ISR's
`recently_in_user_mode` condition to remain true for up to 2000 subsequent ticks (~2 seconds) and
the ISR continues driving the display pipeline in parallel with `gui_refresh_task`.

1.4 WHEN `USER_PROCESS_ACTIVE` is false but `LAST_USER_MODE_TICK` is within 2000 ticks of the
current tick THEN the system exhibits a double-paint race: both the timer ISR and `gui_refresh_task`
acquire `BACK_BUFFER` and repaint the screen on every 16 ms interval, causing the taskbar clock to
appear to advance at half the correct rate and input events to be sluggish.

1.5 WHEN `CreateWindowExW` (syscall `0x4001`) is called and the class-name argument (`arg1`) is a
pointer that is not reachable via the 4-argument syscall ABI THEN the system parses an empty class
name string, causing `lookup_class_wndproc` to return `0`, and the created window's `wnd_proc`
field is set to `0x0`.

1.6 WHEN a window is created with `wnd_proc = 0x0` and WM_PAINT or any other message is dispatched
to it THEN the system dispatches the message with a null procedure pointer, producing incorrect
behavior because no registered class handler is invoked.

### Expected Behavior (Correct)

2.1 WHEN a Win32 thread calls GetMessage and its message queue is empty and no WM_QUIT has been
posted THEN the system SHALL synthesize a WM_PAINT message for any window owned by that thread
(regardless of its `visible` state) and return that message, ensuring GetMessage never returns `0`
unless WM_QUIT was explicitly posted via `PostQuitMessage`.

2.2 WHEN GetMessage is called and the calling thread owns at least one window (visible or not)
THEN the system SHALL find that window, synthesize a WM_PAINT for it, enqueue it, and return the
message with a non-zero result, allowing the EXE's message loop to continue running and reach
`ShowWindow`.

2.3 WHEN `process_execution_task` completes all post-exit cleanup (Win32 window teardown, focus
restore) THEN the system SHALL store `0` into `LAST_USER_MODE_TICK` so that `ticks_since_user`
immediately exceeds 2000, ending the ISR's `recently_in_user_mode` grace period and stopping the
ISR from painting alongside `gui_refresh_task`.

2.4 WHEN `LAST_USER_MODE_TICK` is reset to `0` after process exit THEN the system SHALL have
only `gui_refresh_task` driving the display pipeline, restoring the correct single-repaint cadence
and making the taskbar clock advance at exactly 1 second per 1000 timer ticks.

2.5 WHEN `CreateWindowExW` (syscall `0x4001`) is called and the parsed class name is empty THEN
the system SHALL fall back to the most recently registered class entry's `wnd_proc`, ensuring the
created window has a valid (non-zero) window procedure when only one class has been registered.

2.6 WHEN a window is created with a valid fallback `wnd_proc` obtained from the class registry
THEN the system SHALL dispatch WM_PAINT and other messages using that procedure pointer, allowing
the registered class handler to execute correctly.

### Unchanged Behavior (Regression Prevention)

3.1 WHEN a Win32 thread calls GetMessage and its message queue contains a real message (not
synthesized) THEN the system SHALL CONTINUE TO return that message immediately without
synthesizing any additional WM_PAINT.

3.2 WHEN `PostQuitMessage` has been called for a thread and GetMessage is subsequently called by
that thread THEN the system SHALL CONTINUE TO return `0` with a WM_QUIT message, correctly
signaling the message loop to exit.

3.3 WHEN a Win32 EXE is still running (USER_PROCESS_ACTIVE is true and the CPU is in ring-3)
THEN the system SHALL CONTINUE TO drive the display from the timer ISR at the existing ~60 fps
cadence, unaffected by the LAST_USER_MODE_TICK reset logic.

3.4 WHEN `CreateWindowExW` is called with a non-empty, correctly parsed class name THEN the
system SHALL CONTINUE TO look up the `wnd_proc` via `lookup_class_wndproc` using that name,
preserving the existing class-registry lookup path.

3.5 WHEN a Win32 process exits and `process_execution_task` restores focus to the boot terminal
THEN the system SHALL CONTINUE TO restore `USER_PROCESS_ACTIVE` to false and re-enable interrupts
via `sti`, preserving the existing shutdown sequence for all other kernel state.

3.6 WHEN `dispatch_key` is called with a Settings window or a stale Win32 window focused THEN
the system SHALL CONTINUE TO route the character to `TERMINAL_POOL[0]` and clear the stale
`focused_id` respectively, as established by the previous dispatch_key bugfix.
