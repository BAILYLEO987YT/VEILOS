# VEILOS: accounts, sign-in and permissions

**Status:** in progress on one branch, merged when every stage is done.
**A0 done (2026-11-28):** idle lock counts all input; account commands,
Performance Mode and the admin tasks go through `users::authorize`; mesh
commands can't run them; saved keys are never silently replaced
(`KEYS RELINK`); random salts; username rules. **A1 done (2026-11-28):**
accounts v2 in `/etc/users2.vdb` (the v1 file is read once and left for
older builds), sign-in steps and methods, length rules, per-account
back-off instead of lockout, built-in guest that can be turned off, `root`
can't sign in, `user.manage`, `USERMOD`/`GUEST`, auto-login, and passwords
asked for with hidden input instead of on the command line. **A2 done
(2026-11-28):** saved keys in key slots (sign-in, recovery key, admin slot
through the system key); a password change rewrites only its slot; an admin
reset re-wraps through the admin slot or keeps the keys for the recovery
key; accounts with no password use the system key; `KEYS STATUS`,
`KEYS RECOVERY ON|OFF`, `KEYS ADMINSLOT ON|OFF`, `KEYS RECOVER`. Sign-out
also closes the user's windows. **A3 done (2026-11-28):** file owners
(`security/access.rs`, `/etc/owners.vdb`), one `may_access` for every way
in and in both modes, processes run as a uid, `OWNER`/`CHOWN`/`GRANT`, the
security log (`/etc/log/security.log`), a per-user clipboard, the guest's
home emptied at sign-out. Next: A4 (VSH interface).

**Changed from the plan in A3:** "system" files outside `/system` and `/etc`
can be changed by admins, not only by root. With root-only, admins could
no longer write anywhere outside the homes and shared folders on a new
install (everything there is the system's), which broke Windows programs
saving at `/`. `/system`, `/etc` and the sensitive files stay root-only.

**A4 done (2026-11-28):** the VSH interface (`vsh/accounts_api.rs`): the
account policy (idle lock, sign-in delays, PIN/password minimums, which
roles need a password, guest, auto-login) kept in `/etc/policy.vdb` and
read and changed from scripts (`policy()`, `policy_set`, `set NAME = value`),
a per-user idle lock (`my_lock_timeout`, never longer than the system's),
account and file-owner functions, `login`/`logout`/`session_locked`/
`session_unlocked` events, the user's own `/home/<name>/user.vsh` loaded at
sign-in and unloaded at sign-out (the shell is rebuilt then), and the
ready-made settings panel (`/SETTINGS.VSH`, opened from the Settings menu
item).

**A5 done (2026-11-28):** `gui/text.rs` draws the kernel's own screens
(login and lock screens, window titles, notifications, the panel, the file
manager, launcher and other apps) in the theme's TrueType font, and the
terminal and text editor in a monospaced face (Cascadia Mono) sized to their
9-pixel character cell. Widths are measured (carets, centring, labels fitted
with an ellipsis); `THEME[font_family]` and `font_size` now apply. Fonts load
when the desktop starts, so the login screen has them; the bitmap font is
the fallback (no fonts, early boot, the panic screen). Next: A6 (login and
lock screens, looks, first-boot wizard).

**Changed from the plan in A4:**
- Scripts act with the signed-in account's rights (every call goes through
  `authorize`), instead of the interpreter tracking a "system" and a "user"
  level per script. Functions share one namespace, so a per-user script
  could replace a system script's helper and run with its level; account
  rights can't be borrowed that way. Where scripts live is protected by A3:
  `/config.vsh` and `/SHELL.VSH` are the system's, a user's script is theirs.
- The settings live in `/etc/policy.vdb`, written by the kernel, not in a
  system script: they are changed from the panel or `policy_set` and kept.
- Until the new screens (A6), the password dialogs are the terminal's hidden
  prompts, opened by `request_password_change()` and `user_create_dialog()`.
- Standard users have `system.power` (restart and shut down); the guest
  doesn't, and VSH `shutdown()`/`restart()` check it.

## Goal

- A first-boot wizard creates the first account. No built-in `root` or
  `admin` logins; only a built-in, heavily restricted guest.
- Login and lock screens that work with the mouse and keyboard, where a
  typing mistake can always be fixed, and whose look (never behaviour) each
  user can script.
- Proper permissions: one place decides who may manage accounts and who may
  touch which files, the same for the terminal, VSH, the desktop and
  Windows programs, in Security and Performance Mode alike.
- Non-admins see only their own things and what is assigned to them;
  admins see everything, and that access is logged.
- Saved encryption keys that survive password changes, and are never
  destroyed silently.
- Settings live in VSH, not in a settings app. VEILOS ships a premade VSH
  settings window as a convenience; it uses only the public VSH interface.

## Where VEILOS is today (honestly)

### Login and lock screens (`src/gui/desktop.rs:1888–2274`)

- Keyboard only. The login/lock branch of the GUI loop (`src/main.rs:787`)
  skips `poll_input` and draws no cursor.
- No caret movement. Arrow keys arrive as raw keys and `dispatch_raw_key`
  (`desktop.rs:2756`) drops them outside the desktop. No Home, End or
  Delete. A typo can only be fixed by backspacing from the end.
- Tab does switch fields (`desktop.rs:1946`; pc-keyboard decodes Tab as
  `'\t'`), but nothing on screen says so, and focus is a 1px underline.
- **Traps that need a restart:**
  - From the 3rd failed attempt the back-off swallows *every* key,
    Backspace and Tab included (`desktop.rs:1935`), with no countdown and
    the old error still shown: the screen looks frozen. An empty Enter and
    "Unknown username" both count. The counter is per screen, not per
    account.
  - 5 failures lock the account permanently (`src/security/users.rs:254`),
    with no unlock from the login screen. The lock screen does the same and
    has no sign-out. The lock reaches disk at the next successful sign-in by
    anyone (`desktop.rs:2017`), after which it survives reboots too.
- Typing `guest` bypasses the user database (`desktop.rs:1988`); guest
  cannot be turned off.
- Text is the 8×8 bitmap placeholder font.

### Idle lock

`Session::tick` locks after 5 minutes without activity (`TICKS` is in ms,
so the time is right), but only keyboard paths call `Session::touch()`
(`src/main.rs:696, 706, 724, 729`, `desktop.rs:2772`). The mouse driver
(`src/task/mouse.rs:367`, `process_packet`, run from the mouse interrupt)
never does. Minutes of mouse-only use lock the screen. Modifier-only key
presses do not count either. Programs cannot ask to stay awake
(`SetThreadExecutionState` is not implemented).

### Accounts (`src/security/users.rs`)

- First boot seeds `root` with no password and `admin`/`veilos`
  (`users.rs:360`).
- `/etc/users.vdb` is `username:hash:home:role:locked:failed`, split on `:`;
  usernames are not validated, so a `:` corrupts the file.
- Salts come from the tick counter (`users.rs:216`) although
  `security::entropy::fill` exists.
- Roles Root / Admin / Standard / Guest map to capability masks
  (`UserRole::default_caps`); `capability_set_for` (`users.rs:413`) is the
  one place a user's capabilities are computed.

### Permission holes

- `USERADD`, `USERDEL`, `PASSWD`, `UNLOCK` (`src/task/terminal.rs:537–605`)
  check nothing. Any signed-in user, guest included, can create an admin,
  reset any password (which also deletes that user's saved keys) or delete
  accounts. `USERADD` takes the password on the command line, so it stays
  in the terminal history.
- `MODE PERFORMANCE` (`terminal.rs:1059`) checks nothing, and Performance
  Mode turns off the protection of `/system` and `/etc`
  (`src/security/mode.rs:215, 239`). A Standard user can switch mode and
  edit `/etc/users.vdb`.
- The VSH `key_event` fires for every key in every phase
  (`src/main.rs:681`), login and lock screens included. Scripts get only
  ESC/ENTER/SPACE/OTHER, but can count and time password keystrokes.
- VSH `shutdown()`/`restart()` do not check `system.power`.

### Files

- The VFS has no owners or permissions (`src/fs/vfs.rs`); checks are
  scattered in callers (`nt/file.rs:343`, `nt/syscalls.rs:259`, desktop
  drag and drop). `/` is FAT32, which cannot store owners.
- Processes carry a capability set but not the user who started them
  (`src/sched/process.rs`).
- `fs.read` is all or nothing: every account, guest included, can read every
  home folder and the hashes in `/etc/users.vdb`.

### Saved keys (`src/security/encryption.rs`)

The design is sound: each user has a random master key, and the password
only wraps it (`write_master`); `cen_change_password` re-wraps it, so a
password change loses nothing. But:

- `User::change_password` (`users.rs:275–279`) stores the new hash even if
  re-wrapping fails, only logging the error. At the next sign-in
  `cen_unlock_user` (`encryption.rs:487`) finds the password does not open
  the master key, makes a new one and overwrites the file: every saved key
  is lost.
- Any mismatch between `users.vdb` and `master.vmk` does the same, silently.
- An admin reset deletes the keys by design (`cen_forget_user`).

### Audit log (`src/security/mode.rs:152–205`)

`/var/log/audit.log`: off in Performance Mode, not protected (anyone who can
write files can edit it), entries carry uptime ms and a pid, not a date or
a user.

### VSH

Kernel settings are `set NAME = value`, validated in `apply_system_setting`
(`src/vsh/vscript.rs:225`). `THEME[font_family]` and `font_size` are
validated and then discarded. One global engine runs every script with no
notion of which user or level it belongs to; `/config.vsh` is outside
`/etc`, so Standard users can edit it, and it is loaded once, at the first
desktop (`src/main.rs:816`).

### Text

`draw_string_atomic` (`src/gui/ui.rs:616`, 8×8 bitmap) is used about 110
times, almost all in `desktop.rs` and `userland/apps.rs`. TrueType works
(`gfx::render2d_kernel::with_text`, `FontLibrary`, `/FONTS`) but only the
VSH shell and Windows programs use it. Layout code assumes 8 px per
character (carets at `len() * 8`, fixed centring offsets).

## Design

### 1. Accounts

**Store.** `/etc/users.vdb` v2: a version header, and per account:
username, display name, uid, role, enabled, hidden, auto-login,
must-change-password, sign-in steps (section 3), back-off state, created and
last-sign-in times, extra and removed capabilities, key-slot choices.
Saved on every change. Old 6-field lines are recognised and trigger the
upgrade step (section 6).

**Usernames.** `[a-z0-9._-]`, 1–32 characters, starting with a letter,
unique ignoring case. Reserved: `root`, `guest`, `system`.

**Built-in accounts.**

- `root`: a system account. It cannot sign in at the login screen; admins
  act with root's rights through the elevation prompt.
- `guest`: kept, heavily restricted. No writes, no network, home in
  `/tmp/guest` wiped at sign-out, reads only system files and its own home
  (not other homes, not `/etc/users.vdb`). Admins can turn it off.

**Roles and capabilities.** Roles stay Root / Admin / Standard / Guest. A
user's capabilities are `(role default | extra) & !removed`, computed only in
`capability_set_for`. Only Root can grant beyond the Admin set. New
capability `user.manage` for Admins.

**One permission check.** `users::authorize(actor, action, target)` is used
by the terminal, VSH and the sign-in code alike:

- Standard: their own password, sign-in methods, display name and scripts.
- Admin: create, change, reset, unlock, enable/disable and delete Standard
  and Admin accounts. Not the last admin, not themselves while signed in,
  not `root`.
- Root: everything.
- Sensitive actions (role changes, resets, deletion, ownership changes,
  turning off a protection) show the elevation prompt: kernel-drawn, asks
  for the acting admin's sign-in again; scripts cannot draw, fill in or
  dismiss it.
- `MODE` requires `policy.switch`; VSH `shutdown()`/`restart()` require
  `system.power`.

**Terminal commands** stay, go through `authorize`, and ask for passwords
with hidden input instead of taking them as arguments.

### 2. Who sees what

Non-admins see only their own things and what is assigned to them. Admins
see everything, and every access by an admin to another user's things is
recorded in the security log.

**Process users.** Every process carries the uid of the user who started
it; children inherit it. Kernel apps run as the signed-in user.

**Ownership.** Files and folders belong to whoever created them, except
system files (`/system`, `/etc`, and anything given to "system") and
**exempt paths**, a list in the system-level VSH script (admins only) of
places that are shared. FAT32 cannot store owners, so ownership lives in
`/etc/owners.vdb`: owner per path prefix plus grants (path, user, read or
read-write). Grants are how things are assigned to a user. `/home/<user>`
belongs to that user.

**One access check.** `may_access(uid, path, read | write | delete)` is
called by the Windows file layer (replacing `require_change`), the desktop,
the kernel's apps and VSH. It applies in both modes; Performance Mode no
longer opens `/system` or `/etc`. Lookups are cached by prefix.

**Lists follow the same rule.** For a non-admin, `users()` in VSH and the
`USERS` command show only their own account; other users' settings and
login looks are not readable.

**Files that predate ownership** go to the first admin, who can hand any
file or folder to another user, to "shared" or to "system" with
`file_set_owner(path, user | "shared" | "system")` (admin only, works on
whole folders, logged). Giving something to "system" makes it root-only
from then on, and the call says so first.

### 3. Sign-in

**Steps and methods.** Each account has a list of sign-in steps. Each step
is met by one of its methods:

- Alternatives within a step (biometric *or* PIN): any one gets you in.
- Several steps (password *and* password *and* password): all are needed.
  Every step is entered, then all are checked together; a failure says
  only "incorrect", never which step. The back-off counts the whole sign-in
  as one attempt.

**Built-in methods.**

- Password: up to 256 characters.
- PIN: 4–64 digits.
- Limits are counted in characters and checked once, in `users.rs`, for the
  screens, the terminal and VSH alike. The system script may raise the
  minimums (`PIN_MIN_LENGTH`, `PASSWORD_MIN_LENGTH`), never below 4 or
  above the maximums.

**No password.** Allowed, and strongly discouraged by the wizard. Such an
account may auto-login, and Lock becomes a screensaver: the lock screen
appears and Enter unlocks. Its saved keys are only as safe as `/etc` (see
section 4).

**Auto-login** is set by the account holder or an admin, for one account.
Guest stays reachable through Sign out or Switch user.

**Role rules.** The system script can require a minimum number of steps per
role (for example admins ≥ 2). Accounts that fall short are taken through
adding the missing steps at their next sign-in, not locked out. Where the
rule and an account's choice conflict (no password, auto-login), the rule
wins, and the wizard and VSH refuse the combination with the reason. The
first-boot wizard is never blocked: no system script exists yet.

**Back-off, not lockout.** Wrong attempts bring a time-based delay per
account, shown as a live countdown; the fields stay editable and only
Sign in waits. Accounts are never locked permanently. Configured by
`LOGIN_BACKOFF_AFTER` and `LOGIN_BACKOFF_MAX`.

**Plug-in methods** (biometrics and anything else, later). A kernel
interface: name, the kind of prompt (text field, or waiting for a device),
check, and optionally a stable secret for the key slots. Methods are kernel
modules installed by admins, never VSH scripts: a script that decides
whether a sign-in succeeded could simply say yes. Users turn installed
methods on for their own account; their password or PIN is kept as the
backup. A method that cannot provide a stable secret unlocks sign-in only,
and the user's keys are opened by their password or PIN.

### 4. Saved keys

The master key stays random. It is stored wrapped in **key slots**, each an
independent wrapped copy:

- **Sign-in slot(s).** One per alternative method that provides a secret.
  Steps that are all required wrap the master key with their secrets
  combined, so no single one opens it. A password change rewrites only its
  own slot.
- **Recovery key** (strongly recommended, skippable: pointless on a one-off
  boot, and some hardware is short of space). Shown once at setup, saved
  wherever the user chooses. Under 100 bytes on disk and a code of about
  50 characters.
- **Admin slot** (the account holder's choice). Wrapped with a system key
  under `/etc`. An admin password reset uses it instead of deleting the
  keys. It also means admins can open that user's keys.

Without an admin slot or a recovery key, a forgotten password or an admin
reset loses the keys, and setup says so at that step. An account with no
password has only the admin slot.

**Never destroy silently.** A password change writes the new slot before
storing the new hash, and stores nothing if that fails. If no slot opens
at sign-in, the file is kept, the user is told, and the recovery key is
offered; a new master key is only made by an explicit choice.

The choices are made in the first-boot wizard for the first account, and
by the account holder at their first sign-in for accounts created later.

### 5. Security log

Separate from the audit log, always on in both modes:
`/etc/log/security.log` (or under `/var/log` once `/var` is protected),
written only by the kernel, with wall-clock time (`vsh::shell::wall_clock`)
and username. Admins can read it but not change or delete it; only root can
clear it, and clearing is logged. It records sign-ins and failures, account
changes, elevations, ownership changes, and every admin access to another
user's things.

### 6. First boot, upgrade and first sign-in

**First-boot wizard** (no `users.vdb`): username, display name, password or
PIN (strongly recommended, skippable), auto-login if none, key choices
(admin slot, recovery key), guest on or off. The account becomes an Admin.
No other accounts are created besides the built-in guest.

**Upgrade step** (`users.vdb` in the old format): the same wizard, shorter.
Pick the first admin (an existing account or a new one); the old `root`
sign-in and the default `admin`/`veilos` are removed or given new
passwords; key choices for that account; hand over unowned files now or
leave them with the first admin.

**First sign-in** of any account created later: the account holder sets
their sign-in steps (meeting any role rule) and key choices.

### 7. Login and lock screens

**Behaviour is kernel code, always.** Fields, focus, submission, the sign-in
check and the power buttons are drawn and handled by the kernel. No script
can read what is typed, and VSH key events are not delivered while the
login or lock screen is up.

**Input.** A real text field (extending `gui_toolkit::TextInput`):
Left/Right, Home/End, Delete, click to place the caret, Tab/Shift+Tab, a
clear focus ring, show/hide password, a Caps Lock warning. Mouse cursor and
clicks work on both screens; raw keys are routed to them.

**Login.**

- One active account: no username field, just its sign-in (or straight in,
  with auto-login).
- Two or more (the default: the first account and guest): account tiles,
  chosen by mouse or arrows and Enter; Esc goes back. Hidden accounts are
  reached through "Other user".
- Restart and Shut down are always there.

**Lock.** The same sign-in as the account's login, plus **Sign out** and
**Switch user**. The back-off applies; a permanent lock never does.

**Looks.** Each user's look is a data-only file, `/home/<user>/LOGIN.VSH`,
loaded with an allowlist: colours, wallpaper, logo, font, a few layout
presets, clock format, a welcome message. Rejected: `on_event`, elements
with handlers, OS actions, anything reading input. Which look is used:

1. The selected user's own look, when their tile is chosen (and on their
   lock screen).
2. The last signed-in user's look, at boot and for guest.
3. The system look, `/etc/GREETER.VSH` (admins only).
4. A look built into the kernel, if all else fails.

A strip the kernel draws itself, naming the selected account, is always
visible and cannot be covered. The welcome message is length-limited and
drawn in a fixed frame, so a look cannot pass for a system prompt
("enter the admin password to continue…").

### 8. Idle lock

A lock-free `LAST_INPUT_MS` (an atomic, because the mouse path runs in an
interrupt and must not take the `SESSION` lock) is updated on any key event
(including modifiers) and any mouse movement or button change;
`Session::tick` reads it. Settings: `LOCK_IDLE_TIMEOUT` (seconds, 0 =
never). A way for programs to keep the session awake
(`SetThreadExecutionState`) can follow.

### 9. VSH interface

**Two levels of script.**

- System script, under `/etc` (admins only, whatever the mode): sets the
  rules.
- Per-user scripts, in the user's home: can only tighten them (a shorter
  idle timeout than the system maximum, never longer). Loaded at each
  sign-in and on user switch, not once per boot.

The engine tracks which level and user each loaded script belongs to.

**Settings** (`set NAME = value`, validated in `apply_system_setting`):
`LOCK_IDLE_TIMEOUT`, `LOGIN_BACKOFF_AFTER`, `LOGIN_BACKOFF_MAX`,
`LOGIN_GUEST`, `LOGIN_AUTO_USER`, `PIN_MIN_LENGTH`, `PASSWORD_MIN_LENGTH`,
minimum steps per role, exempt paths. System-level only, except where a
user may tighten.

**Read:** `session_user()`, `session_role()`, `idle_ms()`, `users()`
(names, display names, roles; filtered for non-admins; never hashes).

**Act** (through `authorize`, elevation where needed): `user_create`,
`user_set_role`, `user_enable`, `user_unlock`, `user_delete`,
`file_set_owner`, `file_grant`.

**Secrets never pass through a script.** `request_password_change()` and the
sign-in steps of `user_create` open kernel-drawn dialogs; a custom settings
window never sees a password.

**Events:** `login`, `logout`, `session_locked`, `session_unlocked`.

**Premade settings window:** a VSH file shipped with VEILOS, built only on
the above, replaceable by the user. Not a settings app.

### 10. Text

One `draw_text` / `measure_text` pair that draws with the selected TrueType
font (`THEME[font_family]`, `font_size`, which are then actually applied),
used everywhere `draw_string_atomic` is today: login, lock, panel,
notifications, kernel apps. Every caret position and centring measures the
real text instead of assuming 8 px. The bitmap font remains only for very
early boot, the panic screen and when no font loads. The login screen's
font follows the same order as its look (section 7).

## Stages

Ordered so the parts that touch drawing and the GUI loop
(`gui_refresh_task` in `src/main.rs`, painting in `desktop.rs`) come last,
after the GPU work (WDDM_DESIGN.md) has settled.

| Stage | What | Touches |
|---|---|---|
| **A0** | Stand-alone fixes: idle timer counts the mouse and all keys; no VSH key events at login/lock; `MODE` needs `policy.switch`; account commands go through a first `authorize`; password change writes the key slot first; no silent master-key overwrite; salts from `entropy::fill`; username validation | `users.rs`, `encryption.rs`, `terminal.rs`, `mouse.rs`, one line in `main.rs` |
| **A1** | `users.vdb` v2, built-in root/guest, `user.manage`, full `authorize`, elevation, length rules, steps and role rules, back-off instead of lockout | `users.rs`, `capability.rs`, `terminal.rs` |
| **A2** | Key slots, recovery key, admin slot | `encryption.rs` |
| **A3** | Process uids, `/etc/owners.vdb`, `may_access`, protection in both modes, security log, `file_set_owner` | `process.rs`, `security/`, `nt/file.rs`, `nt/syscalls.rs`, desktop drag and drop |
| **A4** | VSH: two levels, settings, read/act calls, events, kernel-drawn password dialogs, per-user loading | `vsh/` |
| **A5** | `draw_text` / `measure_text`; move all bitmap text over | `gui/ui.rs`, `desktop.rs`, `userland/apps.rs` |
| **A6** | Text field, login and lock screens, looks, first-boot wizard, upgrade step, first-sign-in setup, premade settings window | `desktop.rs`, `gui_toolkit.rs`, `main.rs`, `storage/` |
| **A7** | Plug-in sign-in method interface (password and PIN become its first two methods) | `security/` |

README's "Log in as `admin` with password `veilos`" changes in A6. The CI
boot test (`tools/boot_test.sh`) only waits for the GUI to start, so it is
unaffected; a test build will need a way past the wizard (a preset answer
file on the test disk).

## Rules

- The kernel owns sign-in behaviour. Scripts change looks, never decisions.
- Secrets never pass through scripts, and are never taken on a command line.
- Protections do not depend on Security or Performance Mode.
- One `authorize` for account actions, one `may_access` for files, one
  `capability_set_for` for capabilities. No second copy anywhere.
- Saved keys are never destroyed without the user's explicit choice.
- Admin access to other users' things is always logged, and the log cannot
  be edited by admins.
- Nobody is ever stuck at a screen that needs a restart: every screen keeps
  editing, Sign out / Switch user and the power buttons.

## Risks

- **Ownership outside the file system.** `/etc/owners.vdb` can drift from
  the disk if it is changed elsewhere (another OS mounting the FAT32
  volume). Unknown paths fall back to the first admin and are logged.
- **Admin slot and no-password accounts** weaken key protection to how well
  `/etc` is protected. Opt-in, and explained when chosen.
- **Proportional text** will move things that assumed 8 px characters;
  every screen needs checking after A5.
- **`may_access` on every file operation** costs time; prefix caching and
  measuring in Performance Mode.
- **Biometric methods** rarely give a stable secret; they unlock sign-in,
  not keys, unless the method stores a secret it releases on a match.

## Decisions record (2026-11-26)

| Question | Decision |
|---|---|
| Settings app | None. VSH interface plus a premade VSH settings window |
| Default accounts | Removed. First-boot wizard creates the first (Admin) account; built-in restricted guest kept |
| Password for first account | Strongly recommended, optional; no password allows auto-login |
| Username field | Omitted when only one account is active (default is two: first account and guest) |
| Login screen customisation | Looks only, never behaviour |
| Lockout | Time-based back-off, configurable from VSH; no permanent lock |
| Idle lock | Must count all activity (was keyboard only) |
| Lock with no password | Screensaver: Enter unlocks |
| Login looks | Selected user's, then last user's, then system's, then built-in |
| Auto-login set by | Account holder and admins |
| Visibility | Non-admins: own and assigned; admins: everything, logged |
| Admin key slot | Per account, chosen in the wizard / first sign-in by the holder |
| Admin access | Logged |
| Ownership outside `/home` | Creator, except system and exempt paths |
| Recovery key | Strongly recommended, skippable |
| Files predating ownership | First admin, who can hand them to a user, shared or system |
| Password / PIN length | Password ≤ 256 characters; PIN 4–64 digits |
| Several required steps | All checked at the end; failure never says which |
| Role step minimums | Yes, set in the system script |
| Plug-in methods | Kernel modules, admin-installed; never VSH |
| Fonts | User-selected TrueType everywhere; bitmap only as fallback |
