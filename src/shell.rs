// -----| VEILOS shell: kernel glue for the VSH element runtime |-----
//
// Connects ui_runtime::Shell (pure, host-tested) to the desktop:
//
//   start(path) / stop() / reload()   SHELL ON | OFF | RELOAD (terminal) or
//                                     `set SHELL = "/SHELL.VSH";` in /config.vsh
//   pump(ms) -> bool                  once per GUI loop: runs queued handlers,
//                                     window events, kernel variables, live
//                                     refresh + layout, then OS actions.
//   pointer_hook(..) -> bool          from poll_input: hover / press. Runs in
//                                     the timer ISR too, so it never allocates,
//                                     never blocks and never runs script.
//   paint_desktop_layer()             before the wallpaper (root bodies)
//   paint_overlay_layer()             after the windows (everything else)
//   host_*                            what vscript.rs's KernelHost calls
//
// LOCK ORDER (never reversed): GLOBAL_ENGINE -> FONTS -> SHELL. The pointer hook only
// try_locks SHELL. OS actions (launch, focus, lock, shutdown) are queued by
// scripts and executed by pump() with no lock held, because opening a window
// re-enters the window manager.

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use spin::Mutex;

use crate::ui_runtime::{time_call, DateTime, Eval, Shell, KINDS};
use crate::vsh_lang::{CallArg, ElementDef, VValue};

pub static SHELL: Mutex<Shell> = Mutex::new(Shell::new());

static ACTIVE: AtomicBool = AtomicBool::new(false);
/// Set by anything that changes what the shell looks like outside pump()
/// (pointer hook, add/destroy from the terminal). pump() reports and clears it.
static DIRTY: AtomicBool = AtomicBool::new(false);
static SOURCE: Mutex<String> = Mutex::new(String::new());
/// A start requested from inside a script (`set SHELL = ...`), done by pump().
static START_REQUEST: Mutex<Option<String>> = Mutex::new(None);
static PENDING: Mutex<Vec<OsAction>> = Mutex::new(Vec::new());
/// Windows the shell has been told about: (id, title).
static KNOWN_WINDOWS: Mutex<Vec<(u32, String)>> = Mutex::new(Vec::new());
static NOW: Mutex<DateTime> = Mutex::new(DateTime { year: 2000, month: 1, day: 1, hour: 0, minute: 0, second: 0 });
/// Where time() comes from: the CMOS RTC, unless replaced (host tests, or an
/// NTP-corrected clock later).
static CLOCK: Mutex<fn() -> DateTime> = Mutex::new(read_rtc);

pub fn set_clock_source(f: fn() -> DateTime) {
    *CLOCK.lock() = f;
    LAST_RTC_MS.store(u64::MAX, Ordering::Relaxed);
}
static LAST_RTC_MS: AtomicU64 = AtomicU64::new(u64::MAX);

// CPU usage: idle cycles accumulated by the executor's hlt (see executor.rs hook).
static IDLE_CYCLES: AtomicU64 = AtomicU64::new(0);
static CPU_SAMPLE: Mutex<(u64, u64, u64)> = Mutex::new((0, 0, u64::MAX)); // (tsc, idle, second)
static LAST_VOLUME: AtomicU64 = AtomicU64::new(u64::MAX);

pub const DEFAULT_PATH: &str = "/SHELL.VSH";

#[derive(Clone, Debug)]
enum OsAction {
    Launch(String),
    Focus(u32),
    Lock,
    Shutdown,
}

/// App names `launch()` understands.
pub const APPS: &[&str] = &["files", "terminal", "clipboard", "mesh", "launcher", "apps", "editor"];

pub fn is_active() -> bool {
    ACTIVE.load(Ordering::Acquire)
}

// =====================================================================
// -----| starting and stopping |-----
// =====================================================================

fn read_text(path: &str) -> Result<String, String> {
    let bytes = {
        let vfs = crate::vfs::VFS.lock();
        vfs.read_file(path)
    };
    let bytes = bytes.map_err(|_| format!("cannot read {} (8.3 names only: e.g. /SHELL.VSH)", path))?;
    core::str::from_utf8(&bytes).map(|s| s.to_string()).map_err(|_| format!("{} is not valid UTF-8", path))
}

fn log_errors(errs: &[String]) {
    for e in errs {
        crate::serial_println!("[SHELL] {}", e);
    }
    if !errs.is_empty() {
        crate::desktop::notify(
            crate::desktop::NotifCategory::Warning,
            "SHELL",
            &format!("{} problem(s) in the shell script, see the serial log", errs.len()),
        );
    }
}

/// Load (or reload) a shell script and run its `start`. Returns a one-line
/// summary for the terminal. Must not be called with GLOBAL_ENGINE held.
pub fn start(path: &str) -> Result<String, String> {
    crate::render2d_kernel::ensure_fonts();
    let text = read_text(path)?;
    // A different file replaces the old shell completely.
    let old = SOURCE.lock().clone();
    {
        let mut eng = crate::vscript::GLOBAL_ENGINE.lock();
        if !old.is_empty() && old != path {
            eng.unload(&old);
        }
        let load_errs = eng.execute_named(path, &text);
        if load_errs.iter().any(|e| e.starts_with("PARSE ERROR") || e.starts_with("LEX ERROR")) {
            // The engine kept the previous version; keep the current shell too.
            log_errors(&load_errs);
            return Err(format!("{} did not parse: {}", path, load_errs[0]));
        }
        SHELL.lock().clear();
        KNOWN_WINDOWS.lock().clear();
        let start_errs = eng.trigger_source(path, "start");
        let mut all = load_errs;
        all.extend(start_errs);
        all.extend(SHELL.lock().take_errors());
        log_errors(&all);
    }
    *SOURCE.lock() = String::from(path);
    LAST_VOLUME.store(u64::MAX, Ordering::Relaxed);
    ACTIVE.store(true, Ordering::Release);
    DIRTY.store(true, Ordering::Release);
    let n = SHELL.lock().len();
    crate::serial_println!("[SHELL] {} running: {} elements", path, n);
    Ok(format!("{} running: {} elements (problems, if any, are on the serial log)", path, n))
}

pub fn reload() -> Result<String, String> {
    let path = SOURCE.lock().clone();
    if path.is_empty() {
        return Err(String::from("no shell is loaded (SHELL ON [path])"));
    }
    start(&path)
}

/// Remove the shell: elements, handlers and its variables. The built-in panel
/// keeps working the whole time.
pub fn stop() {
    ACTIVE.store(false, Ordering::Release);
    let path = core::mem::take(&mut *SOURCE.lock());
    if !path.is_empty() {
        crate::vscript::GLOBAL_ENGINE.lock().unload(&path);
    }
    SHELL.lock().clear();
    KNOWN_WINDOWS.lock().clear();
    DIRTY.store(true, Ordering::Release);
}

/// Called by `set SHELL = "/PATH";` (engine is locked then, so it is deferred).
pub fn request_start(path: &str) {
    *START_REQUEST.lock() = Some(String::from(path));
}

pub fn status() -> String {
    let s = SHELL.lock();
    format!(
        "shell {} ({}), {} elements, {} on top of windows",
        if is_active() { "ON" } else { "OFF" },
        {
            let p = SOURCE.lock().clone();
            if p.is_empty() { String::from("no script") } else { p }
        },
        s.len(),
        s.overlay_order().len()
    )
}

// =====================================================================
// -----| host functions (called by vscript.rs while the engine is locked) |-----
// =====================================================================

pub fn host_add(kind: &str, def: &ElementDef, named: &[CallArg], snapshot: &[(String, Result<VValue, String>)]) -> Result<VValue, String> {
    let r = SHELL.lock().add(kind, def, named, snapshot).map(VValue::Ref);
    DIRTY.store(true, Ordering::Release);
    r
}

pub fn host_prop(id: u32, key: &str) -> Result<VValue, String> {
    SHELL.lock().prop(id, key)
}

pub fn host_is_kind(kind: &str) -> bool {
    KINDS.contains(&kind)
}

pub fn host_call(name: &str, args: &[CallArg]) -> Option<Result<VValue, String>> {
    let one = |what: &str| -> Result<&VValue, String> {
        if args.len() != 1 {
            Err(format!("{}() takes exactly 1 argument", what))
        } else {
            Ok(&args[0].value)
        }
    };
    Some(match name {
        "time" => time_call(args, &NOW.lock()),
        "destroy" => match one("destroy") {
            Ok(VValue::Ref(id)) => {
                let r = SHELL.lock().destroy(*id).map(|_| VValue::None);
                DIRTY.store(true, Ordering::Release);
                r
            }
            Ok(other) => Err(format!("destroy(ref): expected an element_ref, got {}", other.type_name())),
            Err(e) => Err(e),
        },
        "launch" => match one("launch") {
            Ok(VValue::String(app)) => {
                let a = app.to_ascii_lowercase();
                if APPS.contains(&a.as_str()) {
                    PENDING.lock().push(OsAction::Launch(a));
                    Ok(VValue::Boolean(true))
                } else {
                    // Not a script bug: the app just is not installed. Tell the user, return false.
                    crate::desktop::notify(crate::desktop::NotifCategory::Info, "SHELL", &format!("No app called '{}' is installed", app));
                    Ok(VValue::Boolean(false))
                }
            }
            Ok(other) => Err(format!("launch(name): expected a string, got {}", other.type_name())),
            Err(e) => Err(e),
        },
        "focus_window" => match one("focus_window") {
            Ok(VValue::Integer(id)) if *id >= 0 && *id <= u32::MAX as i64 => {
                PENDING.lock().push(OsAction::Focus(*id as u32));
                Ok(VValue::None)
            }
            Ok(other) => Err(format!("focus_window(id): expected a window id, got {}", other.to_display())),
            Err(e) => Err(e),
        },
        "lock_session" => {
            PENDING.lock().push(OsAction::Lock);
            Ok(VValue::None)
        }
        "shutdown" => {
            PENDING.lock().push(OsAction::Shutdown);
            Ok(VValue::None)
        }
        _ => return None,
    })
}

// =====================================================================
// -----| real-time clock (CMOS) |-----
// =====================================================================

fn cmos(reg: u8) -> u8 {
    use x86_64::instructions::port::Port;
    let mut addr: Port<u8> = Port::new(0x70);
    let mut data: Port<u8> = Port::new(0x71);
    // SAFETY: standard CMOS index/data ports; reads only. Bit 7 (NMI disable) is left clear.
    unsafe {
        addr.write(reg & 0x7F);
        data.read()
    }
}

fn read_rtc() -> DateTime {
    x86_64::instructions::interrupts::without_interrupts(|| {
        let raw = || {
            let mut guard = 0;
            while cmos(0x0A) & 0x80 != 0 && guard < 100_000 {
                guard += 1; // update in progress (lasts < 2 ms)
            }
            [cmos(0x00), cmos(0x02), cmos(0x04), cmos(0x07), cmos(0x08), cmos(0x09)]
        };
        // Read until two consecutive reads agree (avoids a torn read mid-update).
        let mut a = raw();
        for _ in 0..5 {
            let b = raw();
            if a == b {
                break;
            }
            a = b;
        }
        let reg_b = cmos(0x0B);
        let bcd = reg_b & 0x04 == 0;
        let h24 = reg_b & 0x02 != 0;
        let conv = |v: u8| if bcd { (v & 0x0F) + (v >> 4) * 10 } else { v };
        let pm = a[2] & 0x80 != 0;
        let mut hour = conv(a[2] & 0x7F);
        if !h24 {
            hour %= 12;
            if pm {
                hour += 12;
            }
        }
        DateTime {
            year: 2000 + conv(a[5]) as u16,
            month: conv(a[4]).clamp(1, 12),
            day: conv(a[3]).clamp(1, 31),
            hour: hour.min(23),
            minute: conv(a[1]).min(59),
            second: conv(a[0]).min(59),
        }
    })
}

// =====================================================================
// -----| CPU usage (executor hook) |-----
// =====================================================================

/// Add to executor.rs `sleep_if_idle`, around `enable_and_hlt()`:
/// `let t0 = rdtsc(); interrupts::enable_and_hlt(); crate::shell::note_idle(rdtsc() - t0);`
pub fn note_idle(cycles: u64) {
    IDLE_CYCLES.fetch_add(cycles, Ordering::Relaxed);
}

fn rdtsc() -> u64 {
    // SAFETY: rdtsc has no preconditions.
    unsafe { core::arch::x86_64::_rdtsc() }
}

/// CPU busy % of the core the executor runs on, over the last whole second.
/// None until a second has passed or if the idle hook is not installed.
fn cpu_usage(sec: u64) -> Option<f64> {
    let mut s = CPU_SAMPLE.lock();
    if s.2 == sec {
        return None;
    }
    let (tsc, idle) = (rdtsc(), IDLE_CYCLES.load(Ordering::Relaxed));
    let prev = *s;
    *s = (tsc, idle, sec);
    if prev.2 == u64::MAX || idle == 0 {
        return None;
    }
    let total = tsc.wrapping_sub(prev.0).max(1);
    let idle_d = idle.wrapping_sub(prev.1).min(total);
    let busy_permille = 1000 - idle_d * 1000 / total;
    Some(busy_permille as f64 / 10.0)
}

// =====================================================================
// -----| per-frame pump |-----
// =====================================================================

/// Run once per GUI loop iteration from gui_refresh_task (task context, never
/// the ISR). Returns true when the screen needs repainting.
pub fn pump(ms: u64) -> bool {
    if let Some(path) = START_REQUEST.lock().take() {
        match start(&path) {
            Ok(m) => crate::serial_println!("[SHELL] {}", m),
            Err(e) => crate::serial_println!("[SHELL] could not start: {}", e),
        }
    }
    if !is_active() {
        return DIRTY.swap(false, Ordering::AcqRel);
    }

    // Clock: the RTC is read at most 4 times a second; time() is stable within a frame.
    if LAST_RTC_MS.load(Ordering::Relaxed) == u64::MAX || ms.wrapping_sub(LAST_RTC_MS.load(Ordering::Relaxed)) >= 250 {
        let clock = *CLOCK.lock();
        *NOW.lock() = clock();
        LAST_RTC_MS.store(ms, Ordering::Relaxed);
    }
    let now = *NOW.lock();
    let now_sec = now.hour as u64 * 3600 + now.minute as u64 * 60 + now.second as u64;

    // Window list diff (the window manager is not modified).
    let mut opened: Vec<(u32, String)> = Vec::new();
    let mut closed: Vec<u32> = Vec::new();
    let mut retitled: Vec<(u32, String)> = Vec::new();
    let mut focused: i64 = 0;
    if let Some(wm) = crate::desktop::DESKTOP_WM.try_lock() {
        focused = wm.focused_id.map(|f| f as i64).unwrap_or(0);
        let mut known = KNOWN_WINDOWS.lock();
        for w in &wm.windows {
            match known.iter_mut().find(|(id, _)| *id == w.id) {
                None => opened.push((w.id, w.title.clone())),
                Some((_, t)) if *t != w.title => {
                    *t = w.title.clone();
                    retitled.push((w.id, w.title.clone()));
                }
                _ => {}
            }
        }
        for (id, _) in known.iter() {
            if !wm.windows.iter().any(|w| w.id == *id) {
                closed.push(*id);
            }
        }
        known.retain(|(id, _)| !closed.contains(id));
        for o in &opened {
            known.push(o.clone());
        }
    }

    let handlers = SHELL.lock().take_handlers();
    let mut errs: Vec<String> = Vec::new();
    let mut changed;
    {
        let mut eng = crate::vscript::GLOBAL_ENGINE.lock();
        // 1. Element handlers queued by the pointer hook.
        for (h, me) in handlers {
            if let Err(e) = eng.run_handler(&h, alloc::vec![me]) {
                errs.push(format!("handler: {}", e));
            }
        }
        // 2. Window events.
        let src = SOURCE.lock().clone();
        for (id, title) in &opened {
            let mut p = BTreeMap::new();
            p.insert(String::from("id"), VValue::Integer(*id as i64));
            p.insert(String::from("title"), VValue::String(title.clone()));
            errs.extend(eng.trigger_event_with("window_opened", None, &p));
        }
        for id in &closed {
            let mut p = BTreeMap::new();
            p.insert(String::from("id"), VValue::Integer(*id as i64));
            errs.extend(eng.trigger_event_with("window_closed", None, &p));
        }
        for (id, title) in &retitled {
            let mut p = BTreeMap::new();
            p.insert(String::from("id"), VValue::Integer(*id as i64));
            p.insert(String::from("title"), VValue::String(title.clone()));
            errs.extend(eng.trigger_event_with("window_title_changed", None, &p));
        }
        let _ = src;
        // 3. Kernel-owned variables (a write only happens when the value changes).
        if let Some(cpu) = cpu_usage(now_sec) {
            eng.set_global("CPU_USAGE", VValue::Float(cpu));
        }
        // heap_usage() locks the allocator: interrupts are off inside it so an ISR
        // that allocates cannot spin on a lock this task holds.
        let (used, size) = crate::allocator::heap_usage();
        if size > 0 {
            let pct = (used as u64 * 1000 / size as u64) as f64 / 10.0;
            eng.set_global("MEM_USAGE", VValue::Float(pct));
        }
        eng.set_global("NETWORK_UP", VValue::Boolean(crate::network::is_ready()));
        eng.set_global("FOCUSED_WINDOW", VValue::Integer(focused));
        // Script -> kernel: volume.
        if let Some(VValue::Integer(v)) = eng.get_global("VOLUME_LEVEL") {
            let v = (*v).clamp(0, 100) as u64;
            if LAST_VOLUME.swap(v, Ordering::Relaxed) != v {
                crate::audio::set_volume(v as u8);
            }
        }
        // 4. Live refresh + layout.
        let fonts_guard = crate::render2d_kernel::FONTS.lock();
        let fonts = if fonts_guard.is_empty() { None } else { Some(&*fonts_guard) };
        let mut sh = SHELL.lock();
        let scopes = sh.scopes();
        let mut ev = Eval { interp: &mut **eng, scopes, now };
        changed = sh.refresh(&mut ev, now_sec);
        changed |= sh.layout((crate::ui::SCREEN_W as i32, crate::ui::SCREEN_H as i32), fonts);
        changed |= core::mem::replace(&mut sh.state_changed, false);
        errs.extend(sh.take_errors());
    }
    if !errs.is_empty() {
        log_errors(&errs);
    }

    // 5. OS actions, with no lock held.
    let actions: Vec<OsAction> = core::mem::take(&mut *PENDING.lock());
    for a in actions {
        run_action(a);
        changed = true;
    }
    changed | DIRTY.swap(false, Ordering::AcqRel)
}

fn run_action(a: OsAction) {
    match a {
        OsAction::Launch(app) => {
            let p = crate::desktop::work_area().0;
            match app.as_str() {
                "files" => {
                    crate::apps::open_file_manager(100, 60, 520, 400, 0);
                }
                "terminal" => {
                    crate::apps::open_terminal(80, 80, 550, 380, 0);
                }
                "clipboard" => {
                    crate::apps::open_clipboard_viewer(200, 150, 400, 500, 0);
                }
                "mesh" => {
                    crate::apps::open_mesh_manager(150, 100, 600, 450, 0);
                }
                "launcher" | "apps" => {
                    crate::apps::open_app_launcher(0, p, 600, 480, 0);
                }
                "editor" => {
                    crate::apps::open_text_editor(100, 80, 560, 400, 0, "");
                }
                _ => {}
            }
        }
        OsAction::Focus(id) => {
            let mut wm = crate::desktop::DESKTOP_WM.lock();
            if let Some(w) = wm.windows.iter_mut().find(|w| w.id == id) {
                if w.state == crate::desktop::WindowState::Minimized {
                    w.state = crate::desktop::WindowState::Normal;
                }
                wm.focus(id);
            }
        }
        OsAction::Lock => crate::desktop::lock_desktop(),
        OsAction::Shutdown => power_off(),
    }
}

/// Emulator power-off. Real hardware needs ACPI (not implemented yet): if the
/// machine is still running afterwards, the user is told.
fn power_off() {
    use x86_64::instructions::port::Port;
    crate::serial_println!("[SHELL] shutdown requested");
    crate::users::USER_DB.lock().save();
    // SAFETY: these ports are the ACPI PM1a control registers of QEMU (newer, older)
    // and VirtualBox; writing the SLP_EN|SLP_TYP value powers the VM off.
    unsafe {
        Port::<u16>::new(0x604).write(0x2000);
        Port::<u16>::new(0xB004).write(0x2000);
        Port::<u16>::new(0x4004).write(0x3400);
    }
    crate::desktop::notify(crate::desktop::NotifCategory::Warning, "SHUTDOWN", "Power-off is not supported on this machine yet");
}

// =====================================================================
// -----| input and painting hooks |-----
// =====================================================================

/// From poll_input, before windows see the click. Returns true when the shell
/// took a left press. ISR-safe: try_lock only, no allocation, no script.
pub fn pointer_hook(mx: i32, my: i32, mb: bool, last_mb: bool) -> bool {
    if !is_active() {
        return false;
    }
    let mut s = match SHELL.try_lock() {
        Some(s) => s,
        None => return false,
    };
    let took = s.pointer(mx, my, mb && !last_mb, !mb && last_mb);
    if s.state_changed {
        DIRTY.store(true, Ordering::Release);
    }
    took
}

fn paint(desktop: bool) {
    if !is_active() {
        return;
    }
    // Same order as pump(): FONTS before SHELL.
    let fonts_guard = crate::render2d_kernel::FONTS.lock();
    let fonts = if fonts_guard.is_empty() { None } else { Some(&*fonts_guard) };
    let mut sh = SHELL.lock();
    let ver = crate::render2d_kernel::FONTS_GEN.load(Ordering::Relaxed);
    let mut shared = crate::render2d_kernel::GLYPHS.try_lock();
    let mut temp = crate::font::GlyphCache::new();
    let glyphs: &mut crate::font::GlyphCache = match shared.as_mut() {
        Some(g) => &mut **g,
        None => &mut temp,
    };
    let _ = crate::render2d_kernel::with_backbuffer(|c, cache| {
        if desktop {
            sh.paint_desktop(c, cache, fonts, glyphs, ver);
        } else {
            sh.paint_overlay(c, cache, fonts, glyphs, ver);
        }
    });
}

/// desktop.rs paint_desktop, right before `bg.paint()` (the wallpaper).
pub fn paint_desktop_layer() {
    paint(true);
}

/// desktop.rs paint_desktop, right after the window loop, before notifications.
pub fn paint_overlay_layer() {
    paint(false);
}
