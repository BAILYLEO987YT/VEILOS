use crate::fs::vfs::VFS;
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::Ordering;

// -----| path helpers |-----

/// Ensure a filesystem path starts with '/' so VFS mount-prefix
/// matching works correctly. Without this, bare filenames like
/// "HELLO.TXT" would fail to match the "/" mount.
fn normalize_path(path: &str) -> String {
    if path.starts_with('/') {
        String::from(path)
    } else {
        alloc::format!("/{}", path)
    }
}

/// The legacy low-level keyboard task is replaced by the Phase 18
/// desktop keyboard dispatcher (desktop::dispatch_key), which routes
/// input to the correct phase (login / lock / desktop) and then calls
/// apps::TERMINAL_EMU.on_key() for terminal input.
///
/// This task is kept as a no-op future so the executor slot is filled
/// and no spawn call needs to be removed from main.rs.
pub async fn terminal_task() {
    loop {
        core::future::pending::<()>().await;
    }
}

/// Public entry point used by the native terminal emulator app.
pub fn execute_command_pub(cmd: &str, history: &mut Vec<String>) {
    execute_command(cmd, history);
}

/// Commands that act with the signed-in user's account rights. Another
/// machine on the mesh must not run them: it would act as whoever is signed
/// in here, an admin included.
const LOCAL_ONLY: &[&str] = &["USERADD", "USERDEL", "PASSWD", "UNLOCK", "LOGOUT", "MODE", "KEYS"];

/// A command sent by another machine on the mesh (net/distributed.rs exec
/// requests and task capsules, sched/scheduler.rs capsules). Account,
/// sign-in, mode and key commands are refused; everything else runs as
/// before.
pub fn execute_remote_command(cmd: &str, history: &mut Vec<String>) {
    let name = cmd.split_whitespace().next().unwrap_or("").to_ascii_uppercase();
    if LOCAL_ONLY.contains(&name.as_str()) {
        crate::serial_println!("[TERM] {} refused: sent from another machine", name);
        history.push(alloc::format!("{}: not allowed from another machine", name));
        return;
    }
    execute_command(cmd, history);
}

fn execute_command(cmd: &str, history: &mut Vec<String>) {
    let parts: Vec<&str> = cmd.split_whitespace().collect();
    if parts.is_empty() { return; }

    match parts[0] {
        // -----| help |-----
        "HELP" => {
            history.push(String::from("VEILOS SHELL - AVAILABLE COMMANDS:"));
            history.push(String::from(" FS:      LS [PATH]  CAT [FILE]  HEAD [FILE]  MKDIR [DIR]  RM [FILE]  PWD"));
            history.push(String::from(" PROC:    PS  KILL [PID]  CAPS [PID]"));
            history.push(String::from(" HW:      DEVICES  USB  INSTALL  UPDATE  DISPLAYS  SLOTS  WORKERS  GPUTEST  SOUND [VOLUME]  RUN"));
            history.push(String::from(" PKG:     PKG LIST  PKG INSTALL [NAME]  PKG REMOVE [NAME]"));
            history.push(String::from(" DEBUG:   TRACE [N]  PROFILE [START|STOP|REPORT]  DBG [ATTACH|BP] [PID/ADDR]"));
            history.push(String::from(" POLICY:  POLICY  POLICY SET [PROFILE] [PID]  MODE [SECURITY|PERFORMANCE]"));
            history.push(String::from(" SDK:     SDK INFO  SDK HEADERS"));
            history.push(String::from(" STAB:    FUZZ [KERNEL|SYSCALL]  AUDIT  STRESS DRIVERS"));
            history.push(String::from(" DIST:    UPDATE [CHECK|APPLY|STATUS]  DOCS [LIST|READ path]"));
            history.push(String::from(" USERS:   WHOAMI  USERS  USERADD  USERDEL  PASSWD  UNLOCK  LOGOUT"));
            history.push(String::from(" UTIL:    ECHO  DATE  UPTIME  FREE  NET STATUS  PEERS  DMESG [TEXT]"));
            history.push(String::from(" WI-FI:   WIFI [STATUS|UP|SCAN]"));
            history.push(String::from(" POWER:   REBOOT  REBOOT [ACPI|CF9|KBD|TRIPLE]  (one reset method, to check a machine)"));
            history.push(String::from(" MESH:    PING [IP]  EXEC [ID/ALIAS] [CMD] [ARGS]  DFSGET [ID/ALIAS] [PATH] [LOCAL]  DFSPUT [LOCAL] [ID/ALIAS] [PATH]  (see PEERS for aliases like peer1)"));
            history.push(String::from(" OFFLOAD: THREADS  BROKER STATUS  CHECKPOINT LIST"));
            history.push(String::from("          OFFLOAD <peer> <cmd>  CAPSULE TEST"));
            history.push(String::from("          MESH_OFFLOAD_APPROVE <job_hex>  MESH_OFFLOAD_DENY <job_hex>"));
            history.push(String::from(" UI:      OPEN [TERMINAL|HUB|EXPLORER]  CLEAR"));
        }

        // -----| filesystem |-----
        "LS" => {
            let raw_path = if parts.len() > 1 { parts[1] } else { "/" };
            let path = normalize_path(raw_path);
            let vfs = &VFS;
            match vfs.list_dir(&path) {
                Ok(entries) => {
                    history.push(alloc::format!("  {}", path));
                    for e in entries {
                        let s = if e.file_type == crate::fs::vfs::FileType::Directory {
                            alloc::format!("  [DIR]  {}", e.name)
                        } else {
                            alloc::format!("  [FILE] {}  ({} B)", e.name, e.size)
                        };
                        history.push(s);
                    }
                }
                Err(_) => history.push(alloc::format!("ERROR: CANNOT READ '{}'", path)),
            }
        }
        "CAT" => {
            if parts.len() < 2 { history.push(String::from("USAGE: CAT [FILE]")); return; }
            let path = normalize_path(parts[1]);
            match VFS.read_file(&path) {
                Ok(bytes) => {
                    match core::str::from_utf8(&bytes) {
                        Ok(s) => {
                            for line in s.lines() {
                                history.push(String::from(line));
                            }
                        }
                        Err(_) => {
                            history.push(alloc::format!("[BINARY FILE: {} BYTES]", bytes.len()));
                            // Show hex dump of first 128 bytes.
                            for (i, chunk) in bytes.chunks(16).enumerate() {
                                if i >= 8 {
                                    history.push(String::from("... (USE HEAD FOR QUICK PREVIEW)"));
                                    break;
                                }
                                let mut hex = String::new();
                                for b in chunk {
                                    hex.push_str(&alloc::format!("{:02X} ", b));
                                }
                                history.push(alloc::format!("  {:04X}: {}", i * 16, hex));
                            }
                        }
                    }
                }
                Err(_) => history.push(alloc::format!("ERROR: CANNOT READ '{}'", path)),
            }
        }
        "HEAD" => {
            if parts.len() < 2 { history.push(String::from("USAGE: HEAD [FILE] [LINES]")); return; }
            let path = normalize_path(parts[1]);
            let max_lines = parts.get(2).and_then(|s| s.parse::<usize>().ok()).unwrap_or(10);
            match VFS.read_file(&path) {
                Ok(bytes) => {
                    let s = core::str::from_utf8(&bytes).unwrap_or("<binary>");
                    for line in s.lines().take(max_lines) {
                        history.push(String::from(line));
                    }
                }
                Err(_) => history.push(alloc::format!("ERROR: CANNOT READ '{}'", path)),
            }
        }
        "PWD" => history.push(String::from("/")),
        "MKDIR" => {
            if parts.len() < 2 { history.push(String::from("USAGE: MKDIR [DIR]")); return; }
            let path = normalize_path(parts[1]);
            match VFS.create_dir(&path) {
                Ok(_)  => history.push(alloc::format!("mkdir: created directory '{}'", path)),
                Err(crate::fs::vfs::VfsError::FileNotFound) =>
                    history.push(alloc::format!("mkdir: cannot create '{}': parent directory does not exist", path)),
                Err(_) =>
                    history.push(alloc::format!("mkdir: cannot create '{}': filesystem is read-only or unsupported", path)),
            }
        }
        "RM" => {
            if parts.len() < 2 { history.push(String::from("USAGE: RM [FILE]")); return; }
            let path = normalize_path(parts[1]);
            match VFS.delete_file(&path) {
                Ok(_)  => history.push(alloc::format!("rm: removed '{}'", path)),
                Err(crate::fs::vfs::VfsError::FileNotFound) =>
                    history.push(alloc::format!("rm: cannot remove '{}': no such file", path)),
                Err(crate::fs::vfs::VfsError::IsDirectory) =>
                    history.push(alloc::format!("rm: cannot remove '{}': directory not empty", path)),
                Err(_) =>
                    history.push(alloc::format!("rm: cannot remove '{}': filesystem is read-only or unsupported", path)),
            }
        }

        // -----| process management |-----
        "PS" => {
            history.push(String::from("  PID  NAME            ALIVE  CAPS"));
            let pm = crate::sched::process::PROCESSOR.lock();
            for p in &pm.processes {
                let caps = p.capabilities.lock().effective;
                history.push(alloc::format!("  {:3}  {:15} {}  {:#018x}",
                    p.pid, p.name, if p.is_alive() { "yes" } else { "no" }, caps));
            }
            if pm.processes.is_empty() { history.push(String::from("  (no processes)")); }
        }
        "KILL" => {
            if parts.len() < 2 { history.push(String::from("USAGE: KILL [PID]")); return; }
            if let Ok(pid) = parts[1].parse::<u32>() {
                if let Some(p) = crate::sched::process::PROCESSOR.lock().find(pid) {
                    p.terminate(-9);
                    history.push(alloc::format!("KILLED PID {}", pid));
                } else {
                    history.push(alloc::format!("ERROR: PID {} NOT FOUND", pid));
                }
            } else {
                history.push(String::from("ERROR: INVALID PID"));
            }
        }
        "CAPS" => {
            let pid = if parts.len() > 1 { parts[1].parse::<u32>().unwrap_or(0) } else { 0 };
            if let Some(p) = crate::sched::process::PROCESSOR.lock().find(pid) {
                let caps = p.capabilities.lock();
                history.push(alloc::format!("  CAPS FOR PID {}:", pid));
                for name in caps.describe_effective() {
                    history.push(alloc::format!("    + {}", name));
                }
            } else {
                history.push(alloc::format!("ERROR: PID {} NOT FOUND", pid));
            }
        }

        // -----| policy |-----
        "POLICY" => {
            if parts.len() >= 3 && parts[1] == "SET" {
                let profile = parts[2];
                let pid = parts.get(3).and_then(|s| s.parse::<u32>().ok()).unwrap_or(1);
                match crate::security::sandbox::POLICY_ENGINE.lock().apply_profile(pid, profile) {
                    Ok(_)  => history.push(alloc::format!("APPLIED '{}' TO PID {}", profile, pid)),
                    Err(e) => history.push(alloc::format!("ERROR: {}", e)),
                }
            } else {
                let mode = crate::security::policy::POLICY.lock().get_mode();
                history.push(alloc::format!("CURRENT MODE: {:?}", mode).to_uppercase());
                let engine = crate::security::sandbox::POLICY_ENGINE.lock();
                history.push(alloc::format!("  PROFILES: {}", engine.profiles.len()));
                for p in &engine.profiles {
                    history.push(alloc::format!("    - {}", p.name));
                }
            }
        }

        // -----| package manager |-----
        "PKG" => {
            let sub = parts.get(1).copied().unwrap_or("LIST");
            match sub {
                "LIST" => {
                    history.push(String::from("  INSTALLED PACKAGES:"));
                    let reg = crate::userland::pkgmgr::PACKAGE_REGISTRY.lock();
                    for pkg in reg.list_installed() {
                        history.push(alloc::format!("    {} v{}.{}.{}  {}",
                            pkg.manifest.name,
                            pkg.manifest.version.major,
                            pkg.manifest.version.minor,
                            pkg.manifest.version.patch,
                            pkg.manifest.description));
                    }
                }
                "INSTALL" => {
                    let name = parts.get(2).copied().unwrap_or("");
                    if name.is_empty() { history.push(String::from("USAGE: PKG INSTALL [NAME]")); return; }
                    match crate::userland::pkgmgr::PACKAGE_REGISTRY.lock().install(name) {
                        Ok(_)  => history.push(alloc::format!("INSTALLED: {}", name)),
                        Err(e) => history.push(alloc::format!("ERROR: {}", e)),
                    }
                }
                "REMOVE" => {
                    let name = parts.get(2).copied().unwrap_or("");
                    if name.is_empty() { history.push(String::from("USAGE: PKG REMOVE [NAME]")); return; }
                    crate::userland::pkgmgr::PACKAGE_REGISTRY.lock().remove(name);
                    history.push(alloc::format!("REMOVED: {}", name));
                }
                _ => history.push(String::from("USAGE: PKG [LIST|INSTALL|REMOVE] [NAME]")),
            }
        }

        // -----| trace log |-----
        "TRACE" => {
            let n = parts.get(1).and_then(|s| s.parse::<usize>().ok()).unwrap_or(10);
            let log = crate::sys::debugger::TRACE_LOG.lock();
            let events = log.recent(n);
            if events.is_empty() { history.push(String::from("  (no trace events)")); }
            for ev in events {
                history.push(alloc::format!("  [T+{}] pid={} {:?}/{:?} {}",
                    ev.tick, ev.pid, ev.level, ev.category, ev.message));
            }
        }

        // -----| performance profiler |-----
        "PROFILE" => {
            let sub = parts.get(1).copied().unwrap_or("REPORT");
            match sub {
                "START" => {
                    crate::sys::debugger::PROFILER.lock().enabled = true;
                    history.push(String::from("PROFILER STARTED"));
                }
                "STOP" => {
                    crate::sys::debugger::PROFILER.lock().enabled = false;
                    history.push(String::from("PROFILER STOPPED"));
                }
                "RESET" => {
                    crate::sys::debugger::PROFILER.lock().reset();
                    history.push(String::from("PROFILER RESET"));
                }
                _ => {
                    // Print top 10 hot spots into history.
                    let prof = crate::sys::debugger::PROFILER.lock();
                    let hist = prof.histogram();
                    history.push(alloc::format!("  TOP HOTSPOTS ({} SAMPLES):", prof.total_samples));
                    for (i, &(pid, rip, hits)) in hist.iter().take(8).enumerate() {
                        let pct = hits * 100 / prof.total_samples.max(1);
                        history.push(alloc::format!("  #{}: PID={} RIP={:#x} {}%",
                            i+1, pid, rip, pct));
                    }
                    if hist.is_empty() { history.push(String::from("  (no data - use PROFILE START)")); }
                }
            }
        }

        // -----| debugger |-----
        "DBG" => {
            let sub = parts.get(1).copied().unwrap_or("STATUS");
            match sub {
                "ATTACH" => {
                    crate::sys::debugger::DEBUGGER.lock().attach();
                    history.push(String::from("KERNEL DEBUGGER ATTACHED"));
                }
                "DETACH" => {
                    crate::sys::debugger::DEBUGGER.lock().detach();
                    history.push(String::from("KERNEL DEBUGGER DETACHED"));
                }
                "BP" => {
                    let addr = parts.get(2)
                        .and_then(|s| u64::from_str_radix(s.trim_start_matches("0X"), 16).ok())
                        .unwrap_or(0);
                    if addr == 0 { history.push(String::from("USAGE: DBG BP [HEX_ADDR]")); return; }
                    let set = crate::sys::debugger::DEBUGGER.lock().set_breakpoint(addr, 0, false);
                    history.push(if set {
                        alloc::format!("BREAKPOINT SET AT {:#x}", addr)
                    } else {
                        String::from("ERROR: BREAKPOINT TABLE FULL")
                    });
                }
                "BPS" => {
                    let bps = crate::sys::debugger::DEBUGGER.lock().breakpoints.list();
                    history.push(alloc::format!("  {} BREAKPOINT(S):", bps.len()));
                    for bp in bps {
                        history.push(alloc::format!("    {:#x} pid={} hits={}", bp.addr, bp.pid, bp.hit_count));
                    }
                }
                "STATUS" => {
                    let dbg = crate::sys::debugger::DEBUGGER.lock();
                    history.push(alloc::format!("  ACTIVE: {}  STATE: {:?}", dbg.active, dbg.state));
                }
                _ => history.push(String::from("USAGE: DBG [ATTACH|DETACH|BP|BPS|STATUS]")),
            }
        }

        // -----| SDK info |-----
        "SDK" => {
            let sub = parts.get(1).copied().unwrap_or("INFO");
            match sub {
                "INFO" => {
                    let t = &crate::userland::toolchain::VEILOS_TARGET;
                    history.push(alloc::format!("  TARGET:  {}", t.triple));
                    history.push(alloc::format!("  CPU:     {}", t.cpu));
                    history.push(alloc::format!("  ABI:     {}", t.abi));
                    history.push(alloc::format!("  LINKER:  {}", t.linker));
                    for c in crate::userland::toolchain::SDK_COMPONENTS {
                        history.push(alloc::format!("  {:?}: {} v{}", c.kind, c.name, c.version));
                    }
                }
                "HEADERS" => {
                    history.push(String::from("  VEILOS SDK HEADERS:"));
                    for h in crate::userland::toolchain::HEADERS {
                        history.push(alloc::format!("    {}", h.path));
                    }
                }
                _ => history.push(String::from("USAGE: SDK [INFO|HEADERS]")),
            }
        }

        // -----| stability |-----
        "FUZZ" => {
            let sub = parts.get(1).copied().unwrap_or("STATUS");
            match sub {
                "KERNEL" => {
                    crate::sys::stability::KERNEL_FUZZER.enabled.store(true, Ordering::Relaxed);
                    crate::sys::stability::KERNEL_FUZZER.run_batch(200);
                    crate::sys::stability::KERNEL_FUZZER.report();
                    history.push(String::from("KERNEL FUZZ PASS COMPLETE"));
                }
                "SYSCALL" => {
                    crate::sys::stability::SYSCALL_FUZZER.enabled.store(true, Ordering::Relaxed);
                    crate::sys::stability::SYSCALL_FUZZER.run_pass();
                    crate::sys::stability::SYSCALL_FUZZER.report();
                    history.push(String::from("SYSCALL FUZZ PASS COMPLETE"));
                }
                "STATUS" => {
                    history.push(alloc::format!("  KERNEL:  {} iters, {} failures",
                        crate::sys::stability::KERNEL_FUZZER.iterations.load(Ordering::Relaxed),
                        crate::sys::stability::KERNEL_FUZZER.failures.load(Ordering::Relaxed)));
                    history.push(alloc::format!("  SYSCALL: {} calls, {} anomalies",
                        crate::sys::stability::SYSCALL_FUZZER.iterations.load(Ordering::Relaxed),
                        crate::sys::stability::SYSCALL_FUZZER.panics.load(Ordering::Relaxed)));
                }
                _ => history.push(String::from("USAGE: FUZZ [KERNEL|SYSCALL|STATUS]")),
            }
        }
        "AUDIT" => {
            crate::sys::stability::MEM_AUDITOR.run();
            crate::sys::stability::MEM_AUDITOR.report();
            let f = crate::sys::stability::MEM_AUDITOR.findings.lock();
            history.push(alloc::format!("AUDIT COMPLETE: {} findings", f.len()));
        }
        "STRESS" => {
            if parts.get(1).copied() == Some("DRIVERS") {
                crate::sys::stability::DRIVER_STRESS.run();
                history.push(String::from("DRIVER STRESS COMPLETE"));
            } else {
                history.push(String::from("USAGE: STRESS DRIVERS"));
            }
        }

        // -----| distribution / update |-----
        "UPDATE" => {
            // UPDATE <DISK>: upgrade the VEILOS installed on another disk
            // from this one, keeping its users and data (install.rs).
            // UPDATE STATUS: how far that is (the installer's status), and
            // the update channel's. UPDATE alone: the disks it could update.
            // CHECK, APPLY and ENROLL-LTS: the update channel (distribution.rs).
            let admin = {
                let user = crate::security::users::SESSION.lock().username.clone();
                crate::security::users::USER_DB.lock().find(&user).is_some_and(|u| matches!(u.role, crate::security::users::UserRole::Admin | crate::security::users::UserRole::Root))
            };
            match parts.get(1).copied() {
                Some("CHECK")      => { crate::userland::distribution::UPDATE_SYSTEM.lock().check(); history.push(String::from("UPDATE CHECK COMPLETE")); }
                Some("APPLY")      => { crate::userland::distribution::UPDATE_SYSTEM.lock().apply(); history.push(String::from("UPDATE APPLIED")); }
                Some("ENROLL-LTS") => { crate::userland::distribution::LTS_CHANNEL.lock().enroll(); history.push(String::from("ENROLLED IN LTS")); }
                Some("STATUS") => {
                    let s = crate::sys::install::state();
                    if s.is_empty() { history.push(String::from("  No install or update has run.")) } else { history.extend(wrap_lines(&s)) }
                    crate::userland::distribution::LTS_CHANNEL.lock().status();
                }
                Some(_) if !admin => history.push(String::from("UPDATE: administrators only")),
                Some(disk) => match crate::sys::install::start_update(disk) {
                    Ok(()) => {
                        history.extend(wrap_lines(&alloc::format!(
                            "Updating VEILOS on {}: its boot partitions and system files are being replaced.                              Users, home folders and settings stay. UPDATE STATUS shows progress.",
                            disk
                        )));
                    }
                    Err(e) => history.extend(wrap_lines(&alloc::format!("UPDATE: {}", e))),
                },
                None => {
                    history.push(String::from("  Upgrade the VEILOS installed on another disk to this build."));
                    history.push(String::from("  Keeps its users, home folders, Wi-Fi networks and settings. Disks:"));
                    let t = crate::sys::install::update_targets();
                    if t.is_empty() {
                        history.push(String::from("    (no other disk has a VEILOS volume)"));
                    }
                    history.extend(t);
                    history.push(String::from("  USAGE: UPDATE <DISK>   (e.g. UPDATE D0)"));
                    history.push(String::from("         UPDATE STATUS    UPDATE [CHECK|APPLY|ENROLL-LTS]"));
                }
            }
        }

        // -----| documentation |-----
        "DOCS" => {
            let sub = parts.get(1).copied().unwrap_or("LIST");
            match sub {
                "LIST" => {
                    history.push(String::from("  DOCUMENTATION PAGES:"));
                    for (path, title) in crate::userland::distribution::list_docs() {
                        history.push(alloc::format!("    {} — {}", path, title));
                    }
                }
                "READ" => {
                    let path = parts.get(2).copied().unwrap_or("");
                    if path.is_empty() { history.push(String::from("USAGE: DOCS READ [path]")); return; }
                    match crate::userland::distribution::get_doc(path) {
                        Some(doc) => {
                            history.push(alloc::format!("=== {} ===", doc.title));
                            for line in doc.content.lines().take(15) {
                                history.push(String::from(line));
                            }
                            if doc.content.lines().count() > 15 {
                                history.push(String::from("... (truncated, see full docs)"));
                            }
                        }
                        None => history.push(alloc::format!("DOC NOT FOUND: '{}'", path)),
                    }
                }
                _ => history.push(String::from("USAGE: DOCS [LIST|READ path]")),
            }
        }

        // -----| documentation |-----
        "VSH" => {
            if parts.len() < 2 {
                history.push(String::from("USAGE: VSH [FILE_PATH | -c \"SCRIPT_LINE\"]"));
            } else if parts[1] == "-C" || parts[1] == "-c" {
                let script_line = parts[2..].join(" ");
                let mut eng = crate::vsh::vscript::GLOBAL_ENGINE.lock();
                for out in eng.execute(&script_line) {
                    history.push(out);
                }
            } else {
                let path = normalize_path(parts[1]);
                match VFS.read_file(&path) {
                    Ok(bytes) => {
                        let script = core::str::from_utf8(&bytes).unwrap_or("");
                        let mut eng = crate::vsh::vscript::GLOBAL_ENGINE.lock();
                        for out in eng.execute(script) {
                            history.push(out);
                        }
                    }
                    Err(_) => history.push(alloc::format!("ERROR: CANNOT READ '{}'", path)),
                }
            }
        }

        // -----| original commands |-----
        "CLEAR" => { history.clear(); }

        // -----| renderer test card |-----
        "GFXTEST" => {
            let secs = parts.get(1).and_then(|s| s.parse::<u64>().ok()).unwrap_or(8);
            if secs == 0 {
                crate::gfx::render2d_kernel::hide_test_card();
                history.push(String::from("GFXTEST: hidden"));
            } else {
                crate::gfx::render2d_kernel::show_test_card(secs);
                history.push(String::from("GFXTEST: drawing the renderer test card (cycle counts go to the serial log)"));
            }
        }
        // -----| fonts |-----
        "FONTS" => {
            let (ok, bad) = crate::gfx::render2d_kernel::load_fonts();
            history.push(alloc::format!("FONTS: {} loaded, {} failed (details on the serial log)", ok, bad));
        }

        // -----| Phase 18: user management |-----
        "WHOAMI" => {
            let s = crate::security::users::SESSION.lock();
            if s.logged_in {
                history.push(alloc::format!("  {} ({})",
                    s.username,
                    crate::security::users::USER_DB.lock()
                        .find(&s.username)
                        .map(|u| u.role.name())
                        .unwrap_or("unknown"),
                ));
            } else {
                history.push(String::from("  not logged in"));
            }
        }
        "LOGOUT" => {
            crate::security::users::SESSION.lock().logout();
            crate::gui::desktop::DESKTOP_PHASE.store(
                crate::gui::desktop::DesktopPhase::Login as u8,
                Ordering::Release,
            );
            history.push(String::from("Logged out."));
        }
        // Account commands are checked by users::authorize (the signed-in
        // user's role), before USER_DB is locked: authorize locks it itself.
        "USERADD" => {
            // USERADD <username> <password> [role]
            if parts.len() < 3 {
                history.push(String::from("USAGE: USERADD <username> <password> [admin|standard|guest]"));
                return;
            }
            let role = match parts.get(3).map(|r| r.to_ascii_lowercase()).as_deref().unwrap_or("standard") {
                "admin"  => crate::security::users::UserRole::Admin,
                "guest"  => crate::security::users::UserRole::Guest,
                _        => crate::security::users::UserRole::Standard,
            };
            if let Err(e) = crate::security::users::authorize(crate::security::users::Action::CreateUser(role)) {
                history.push(alloc::format!("useradd: {}", e));
                return;
            }
            let name = parts[1].to_ascii_lowercase();
            if let Err(e) = crate::security::users::validate_username(&name) {
                history.push(alloc::format!("useradd: {}", e));
                return;
            }
            let user = crate::security::users::User::new(&name, parts[2], role);
            match crate::security::users::USER_DB.lock().add(user) {
                Ok(_)  => history.push(alloc::format!("useradd: created '{}' ({})", name, role.name())),
                Err(e) => history.push(alloc::format!("useradd: {}", e)),
            }
        }
        "USERDEL" => {
            if parts.len() < 2 {
                history.push(String::from("USAGE: USERDEL <username>"));
                return;
            }
            if let Err(e) = crate::security::users::authorize(crate::security::users::Action::DeleteUser(parts[1])) {
                history.push(alloc::format!("userdel: {}", e));
                return;
            }
            match crate::security::users::USER_DB.lock().remove(parts[1]) {
                Ok(_)  => history.push(alloc::format!("userdel: removed '{}'", parts[1])),
                Err(e) => history.push(alloc::format!("userdel: {}", e)),
            }
        }
        "PASSWD" => {
            // PASSWD <username> <newpassword>  (admin reset of another account) or
            // PASSWD <oldpassword> <newpassword>  (your own)
            if parts.len() < 3 {
                history.push(String::from("USAGE: PASSWD <username|oldpwd> <newpwd>"));
                return;
            }
            let me = crate::security::users::SESSION.lock().username.clone();
            if me.is_empty() {
                history.push(String::from("passwd: nobody is signed in"));
                return;
            }
            // parts[1] names another existing account: an admin reset.
            let other = {
                let db = crate::security::users::USER_DB.lock();
                db.find(parts[1])
                    .filter(|u| !u.username.eq_ignore_ascii_case(&me))
                    .map(|u| u.username.clone())
            };
            if let Some(target) = other {
                if let Err(e) = crate::security::users::authorize(crate::security::users::Action::ResetPassword(&target)) {
                    history.push(alloc::format!("passwd: {}", e));
                    // A non-admin may have meant their own password.
                    if e.starts_with("permission denied") {
                        history.push(String::from("passwd: to change your own password: PASSWD <old> <new>"));
                    }
                    return;
                }
                let mut db = crate::security::users::USER_DB.lock();
                if let Some(u) = db.find_mut(&target) {
                    u.reset_password(parts[2]);
                    db.save();
                    history.push(alloc::format!("passwd: password reset for '{}'", target));
                    history.push(String::from("passwd: their saved keys were removed (a reset can't carry them over)"));
                }
                return;
            }
            let mut db = crate::security::users::USER_DB.lock();
            if let Some(u) = db.find_mut(&me) {
                // Self-change: parts[1] = old, parts[2] = new
                match u.change_password(parts[1], parts[2]) {
                    Ok(_)  => {
                        history.push(String::from("passwd: password updated"));
                        db.save();
                    }
                    Err(e) => history.push(alloc::format!("passwd: {}", e)),
                }
            } else {
                history.push(String::from("passwd: user not found"));
            }
        }
        "UNLOCK" => {
            if parts.len() < 2 {
                history.push(String::from("USAGE: UNLOCK <username>"));
                return;
            }
            if let Err(e) = crate::security::users::authorize(crate::security::users::Action::UnlockUser(parts[1])) {
                history.push(alloc::format!("unlock: {}", e));
                return;
            }
            let mut db = crate::security::users::USER_DB.lock();
            if let Some(u) = db.find_mut(parts[1]) {
                u.unlock();
                db.save();
                history.push(alloc::format!("unlock: '{}' account unlocked", parts[1]));
            } else {
                history.push(alloc::format!("unlock: user '{}' not found", parts[1]));
            }
        }
        "USERS" => {
            history.push(String::from("  USERNAME        ROLE       LOCKED  FAILS"));
            let db = crate::security::users::USER_DB.lock();
            for u in &db.users {
                history.push(alloc::format!("  {:<15} {:<10} {}       {}",
                    u.username, u.role.name(),
                    if u.locked { "yes" } else { "no " },
                    u.failed_attempts,
                ));
            }
        }

        // -----| utility commands |-----
        "ECHO" => {
            history.push(parts[1..].join(" "));
        }
        "DATE" => {
            let ms  = crate::arch::interrupts::TICKS.load(Ordering::Relaxed);
            let sec = ms / 1_000;
            let hh  = (sec / 3_600) % 24;
            let mm  = (sec / 60) % 60;
            let ss  = sec % 60;
            history.push(alloc::format!("  {:02}:{:02}:{:02} (ticks: {})", hh, mm, ss, ms));
        }
        "UPTIME" => {
            let ms   = crate::arch::interrupts::TICKS.load(Ordering::Relaxed);
            let secs = ms / 1_000;
            history.push(alloc::format!("  Up {} h {} m {} s", secs/3600, (secs/60)%60, secs%60));
            let s = crate::security::users::SESSION.lock();
            if s.logged_in {
                history.push(alloc::format!("  Session: {} m {} s",
                    s.duration_secs()/60, s.duration_secs()%60));
            }
        }
        "WORKERS" => {
            // Worker-pool benchmark: 64 chunks of busy work, run on this CPU
            // alone and then spread over every CPU (workers::for_each).
            let (seq, par, per_cpu) = crate::sched::workers::benchmark(64, 2_000_000);
            history.push(alloc::format!("workers: 64 chunks  1 CPU {} ms  all CPUs {} ms  speedup {}.{}x",
                seq / 1000, par / 1000, seq / par.max(1), (seq * 10 / par.max(1)) % 10));
            history.push(alloc::format!("chunks run per CPU: {:?}", per_cpu));
        }
        "KEYS" => {
            // KEYS: the keys in memory. KEYS TEST: save a new key and encrypt a
            // file with it. KEYS CHECK: decrypt that file with the saved keys
            // (after a reboot and sign-in). See encryption.rs, Section 8.
            use crate::security::encryption as cen;
            let me = crate::security::users::SESSION.lock().username.clone();
            let file = alloc::format!("/home/{}/keytest.bin", me);
            let mut out: Vec<String> = Vec::new();
            match parts.get(1).copied() {
                Some("TEST") => {
                    let r = cen::cen_generate_key(cen::FeatureTag::new("selftest"), cen::SecurityLevel::Standard, None)
                        .and_then(|h| cen::cen_persist_key(h).map(|_| h))
                        .and_then(|h| cen::cen_encrypt(h, b"VEILOS key ledger test", None));
                    match r {
                        Ok(enc) => {
                            let ok = crate::fs::vfs::VFS.write_file(&file, &enc).is_ok();
                            out.push(alloc::format!("keys: saved a key; {} bytes encrypted to {} ({})", enc.len(), file, if ok { "written" } else { "write failed" }));
                        }
                        Err(e) => out.push(alloc::format!("keys: {}", e)),
                    }
                }
                Some("RELINK") => {
                    // KEYS RELINK <old password> <current password>: the saved
                    // keys were left wrapped with an older password, so sign-in
                    // couldn't open them (it keeps them instead of replacing
                    // them). Wrap them with the current password.
                    match (parts.get(2), parts.get(3)) {
                        (Some(old), Some(current)) => {
                            let current_ok = crate::security::users::USER_DB.lock().find(&me).is_some_and(|u| {
                                if u.password_hash.is_empty() {
                                    current.is_empty()
                                } else {
                                    crate::security::users::verify_password(current, &u.password_hash)
                                }
                            });
                            if me.is_empty() || !current_ok {
                                out.push(String::from("keys: that is not your current password"));
                            } else {
                                match cen::cen_relink(&me, old, current) {
                                    Ok(n) => out.push(alloc::format!("keys: saved keys re-locked with your current password; {} loaded", n)),
                                    Err(e) => out.push(alloc::format!("keys: {}", e)),
                                }
                            }
                        }
                        _ => out.push(String::from("USAGE: KEYS RELINK <old password> <current password>")),
                    }
                }
                Some("CHECK") => {
                    let enc = crate::fs::vfs::VFS.read_file(&file).unwrap_or_default();
                    let keys = cen::cen_load_keys(cen::FeatureTag::new("selftest")).unwrap_or_default();
                    let hit = keys.iter().find_map(|&h| cen::cen_decrypt(h, &enc, None).ok());
                    out.push(match hit {
                        Some(p) => alloc::format!("keys: {} saved key(s); decrypted: {}", keys.len(), core::str::from_utf8(&p).unwrap_or("?")),
                        None => alloc::format!("keys: {} saved key(s); {} not decrypted", keys.len(), file),
                    });
                }
                _ => {
                    for h in cen::cen_list_keys(None) {
                        if let Some((f, level, algo, saved)) = cen::cen_key_info(h) {
                            out.push(alloc::format!("  {} {} {:?} {:?}{}", h, f.as_str(), level, algo, if saved { " saved" } else { "" }));
                        }
                    }
                    out.push(alloc::format!("keys: {} in memory", cen::cen_list_keys(None).len()));
                }
            }
            for l in out {
                crate::serial_println!("[KEYS] {}", l);
                history.push(l);
            }
        }
        "SOUND" => {
            // SOUND [0-100]: the sound device, and the master volume (audio.rs).
            if let Some(v) = parts.get(1).and_then(|v| v.parse::<u8>().ok()) {
                crate::drivers::audio::set_volume(v);
            }
            history.push(alloc::format!("  {}", crate::drivers::audio::describe()));
        }
        "RUN" => {
            // RUN <path>: start a Windows program (as double-clicking it does).
            let Some(arg) = parts.get(1) else {
                history.push(String::from("USAGE: RUN [PATH]  e.g. RUN /TESTS/W1TEST.EXE"));
                return;
            };
            let path = arg.to_ascii_lowercase();
            let name = String::from(path.rsplit('/').next().unwrap_or(&path));
            history.push(alloc::format!("  starting {}", path));
            crate::userland::apps::open_file_absolute(&path, &name);
        }
        "SLOTS" => {
            // PCIe hot-plug slots (hotplug.rs).
            history.extend(crate::hal::hotplug::describe());
        }
        "DISPLAYS" => {
            // The heads behind the one logical display (HAL 7D).
            history.extend(crate::gfx::graphics::describe_heads());
        }
        "GPUTEST" => {
            // A gpu_driver batch through the logical GPU, checked and timed.
            for line in crate::gfx::gpu_driver::self_test() {
                crate::serial_println!("[GPU] {}", line);
                history.push(line);
            }
        }
        "INSTALL" => {
            // INSTALL: disks VEILOS could go on. INSTALL <DISK> ERASE: put
            // this system on it (install.rs). INSTALL STATUS: how far it is.
            let admin = {
                let user = crate::security::users::SESSION.lock().username.clone();
                crate::security::users::USER_DB.lock().find(&user).is_some_and(|u| matches!(u.role, crate::security::users::UserRole::Admin | crate::security::users::UserRole::Root))
            };
            match (parts.get(1).copied(), parts.get(2).copied()) {
                (Some("STATUS"), _) => {
                    let s = crate::sys::install::state();
                    if s.is_empty() { history.push(String::from("  No install has run.")) } else { history.extend(wrap_lines(&s)) }
                }
                (Some(_), Some("ERASE")) if !admin => history.push(String::from("INSTALL: administrators only")),
                (Some(disk), Some("ERASE")) => match crate::sys::install::start(disk) {
                    Ok(()) => {
                        history.push(alloc::format!("  Installing onto {}: EVERYTHING ON IT IS BEING ERASED.", disk));
                        history.push(String::from("  This takes a few minutes. INSTALL STATUS shows progress."));
                    }
                    Err(e) => history.extend(wrap_lines(&alloc::format!("INSTALL: {}", e))),
                },
                _ => {
                    history.push(String::from("  Put VEILOS on another disk, to start without the stick."));
                    history.push(String::from("  ERASES EVERYTHING on that disk. Disks:"));
                    let t = crate::sys::install::targets();
                    if t.is_empty() {
                        history.push(String::from("    (none besides the one VEILOS runs from)"));
                    }
                    history.extend(t);
                    history.push(String::from("  USAGE: INSTALL <DISK> ERASE   (e.g. INSTALL D2 ERASE)"));
                    history.push(String::from("         INSTALL STATUS"));
                }
            }
        }
        "DRIVER" => {
            // Windows kernel drivers (ntos, WDDM_DESIGN.md W1).
            let admin = {
                let user = crate::security::users::SESSION.lock().username.clone();
                crate::security::users::USER_DB.lock().find(&user).is_some_and(|u| matches!(u.role, crate::security::users::UserRole::Admin | crate::security::users::UserRole::Root))
            };
            match parts.get(1).copied() {
                Some("LOAD") | Some("UNLOAD") | Some("IOCTL") | Some("OPEN") if !admin => {
                    history.push(String::from("DRIVER: administrators only"));
                }
                Some("LOAD") => match parts.get(2) {
                    // Any further words are PCI hardware IDs (PCI\VEN_xxxx&DEV_xxxx)
                    // for a PnP driver, besides those in its .inf.
                    Some(p) => {
                        let ids: Vec<String> = parts.iter().skip(3).map(|s| s.to_ascii_uppercase()).collect();
                        match crate::nt::ntos::image::load_with(&p.to_ascii_lowercase(), &ids) {
                            Ok((name, st, devices)) => {
                                history.push(alloc::format!("  {}: DriverEntry returned {:#010X}", name, st as u32));
                                if devices > 0 {
                                    history.push(alloc::format!("  {}: {} device(s) started", name, devices));
                                }
                            }
                            Err(e) => history.push(alloc::format!("  DRIVER LOAD: {}", e)),
                        }
                    }
                    None => history.push(String::from("USAGE: DRIVER LOAD <path.sys> [PCI IDs]")),
                },
                Some("UNLOAD") => match parts.get(2) {
                    Some(n) => match crate::nt::ntos::image::unload(&n.to_ascii_lowercase()) {
                        Ok(()) => history.push(alloc::format!("  {} unloaded", n)),
                        Err(e) => history.push(alloc::format!("  DRIVER UNLOAD: {}", e)),
                    },
                    None => history.push(String::from("USAGE: DRIVER UNLOAD <name>")),
                },
                Some("OPEN") => {
                    // DRIVER OPEN <\Device\X or \??\X>: IRP_MJ_CREATE, then CLOSE.
                    match parts.get(2).and_then(|n| crate::nt::ntos::io::device_by_name(n)) {
                        Some(dev) => {
                            let c = crate::nt::ntos::io::create_close(dev, crate::nt::ntos::io::IRP_MJ_CREATE);
                            let d = crate::nt::ntos::io::create_close(dev, crate::nt::ntos::io::IRP_MJ_CLOSE);
                            history.push(alloc::format!("  create {:#010X}, close {:#010X}", c as u32, d as u32));
                        }
                        None => history.push(String::from("  DRIVER OPEN: no such device")),
                    }
                }
                Some("IOCTL") => {
                    // DRIVER IOCTL <device> <code hex> [text]: METHOD_BUFFERED, shows the reply.
                    let dev = parts.get(2).and_then(|n| crate::nt::ntos::io::device_by_name(n));
                    let code = parts.get(3).and_then(|c| u32::from_str_radix(c.trim_start_matches("0X").trim_start_matches("0x"), 16).ok());
                    match (dev, code) {
                        (Some(dev), Some(code)) => {
                            let text = parts.get(4..).map(|p| p.join(" ")).unwrap_or_default();
                            let (st, out) = crate::nt::ntos::io::device_control(dev, code, text.as_bytes(), 256);
                            let shown: String = out.iter().map(|&b| if (32..127).contains(&b) { b as char } else { '.' }).collect();
                            let hex: String = out.iter().take(16).map(|b| alloc::format!("{:02X} ", b)).collect();
                            history.push(alloc::format!("  status {:#010X}, {} byte(s): \"{}\"  {}", st as u32, out.len(), shown, hex));
                        }
                        _ => history.push(String::from("USAGE: DRIVER IOCTL <DEVICE NAME> <CODE HEX> [TEXT]")),
                    }
                }
                _ => {
                    let list = crate::nt::ntos::image::DRIVERS.lock();
                    if list.is_empty() {
                        history.push(String::from("  No Windows drivers loaded."));
                    }
                    for d in list.iter() {
                        history.push(alloc::format!("  {}  {} at {:#X} ({} KiB)", d.name, d.path, d.base, d.size / 1024));
                    }
                    drop(list);
                    history.extend(crate::nt::ntos::pnp::describe());
                    history.extend(crate::nt::dxgk::describe());
                    history.push(String::from("  USAGE: DRIVER LOAD <path.sys> [PCI IDs] | UNLOAD <name> | OPEN <dev> | IOCTL <dev> <code> [text]"));
                }
            }
        }
        "USB" => {
            // xHCI controllers, how their events arrive, and their devices.
            history.extend(crate::drivers::usb::xhci::status());
        }
        "DEVICES" => {
            // Every PCI device and the driver bound to it (hal::describe).
            for line in crate::hal::describe() {
                history.push(line);
            }
        }
        "FREE" => {
            // Report heap usage via the allocator stats if available.
            // Fallback: report compile-time heap size constant.
            history.push(String::from("  MEMORY USAGE (approximate):"));
            let heap_start = crate::mm::allocator::HEAP_START;
            let heap_size  = crate::mm::allocator::heap_usage().1;
            history.push(alloc::format!("  Heap base: {:#x}  size: {} KiB",
                heap_start, heap_size / 1024));
            history.push(String::from("  (per-allocation tracking not yet implemented)"));
        }
        "NET" => {
            let sub = parts.get(1).copied().unwrap_or("STATUS");
            match sub {
                "STATUS" => {
                    if crate::net::network::is_ready() {
                        history.push(String::from(if crate::net::network::on_fallback_address() {
                            "  Network: NO DHCP ANSWER (fallback address; still asking)"
                        } else {
                            "  Network: ONLINE"
                        }));
                        let cfg = crate::net::network::NETWORK_CONFIG.lock();
                        history.push(alloc::format!("  IP:      {}/{}", cfg.ip.address(), cfg.ip.prefix_len()));
                        if let Some(gw) = cfg.gateway { history.push(alloc::format!("  Gateway: {}", gw)); }
                        if let Some(dns) = cfg.dns     { history.push(alloc::format!("  DNS:     {}", dns)); }
                        drop(cfg);
                        // The NICs behind the one logical interface (HAL 7E).
                        history.extend(crate::net::network::nic_status());
                    } else {
                        history.push(String::from("  Network: OFFLINE (no NIC or DHCP pending)"));
                        // Still show the NICs: "no link" and "no DHCP answer" differ.
                        history.extend(crate::net::network::nic_status());
                    }
                }
                _ => history.push(String::from("USAGE: NET STATUS")),
            }
        }
        "PEERS" => {
            let mesh = crate::net::distributed::NODE_MESH.lock();
            if mesh.peers.is_empty() {
                history.push(String::from("  No distributed peers discovered."));
            } else {
                history.push(alloc::format!("  {} peer(s):", mesh.peers.len()));
                let now = crate::arch::interrupts::TICKS.load(Ordering::Relaxed);
                for (i, (id, p)) in mesh.peers.iter().enumerate() {
                    let age_ms = now.saturating_sub(p.last_seen);
                    history.push(alloc::format!("    peer{}  {}  id:{}  ip:{}  caps:{:#x}  seen:{}ms ago",
                        i + 1, p.hostname, id.to_hex(), p.ip, p.capabilities, age_ms));
                }
                history.push(String::from("  (use \"peerN\" as a shortcut for the full id in EXEC/DFSGET/DFSPUT)"));
            }
        }
        "DIAG" => {
            history.push(String::from("  === VEILOS DIAGNOSTICS ==="));
            // Network status
            if crate::net::network::is_ready() {
                let cfg = crate::net::network::NETWORK_CONFIG.lock();
                history.push(alloc::format!("  NET: ONLINE  IP: {}/{}", cfg.ip.address(), cfg.ip.prefix_len()));
                if let Some(gw) = cfg.gateway { history.push(alloc::format!("  GW:  {}", gw)); }
            } else {
                history.push(String::from("  NET: OFFLINE (no NIC or DHCP pending)"));
            }
            // Mesh status
            let mesh = crate::net::distributed::NODE_MESH.lock();
            history.push(alloc::format!("  MESH: {} peer(s)", mesh.peer_count()));
            history.push(alloc::format!("  LOCAL ID: {}", mesh.local_id.to_hex()));
            // Policy mode
            let mode = crate::security::policy::POLICY.lock().get_mode();
            history.push(alloc::format!("  POLICY: {:?}", mode));
            // Process list
            let pm = crate::sched::process::PROCESSOR.lock();
            history.push(alloc::format!("  PROCESSES: {}", pm.processes.len()));
            for p in &pm.processes {
                let caps = p.capabilities.lock().effective;
                history.push(alloc::format!("    PID {} {} caps={:#x}", p.pid, p.name, caps));
            }
        }
        "PING" => {
            if parts.len() < 2 {
                history.push(String::from("USAGE: PING [IP]"));
                history.push(String::from("  Sends a test announcement to the given IP."));
                return;
            }
            let target_str = parts[1];
            let octets: Vec<u8> = target_str.split('.').filter_map(|s| s.parse().ok()).collect();
            if octets.len() != 4 {
                history.push(String::from("ERROR: Invalid IP address"));
                return;
            }
            let target_ip = smoltcp::wire::Ipv4Address::new(octets[0], octets[1], octets[2], octets[3]);
            if !crate::net::network::is_ready() {
                history.push(String::from("ERROR: Network not ready"));
                return;
            }
            // Send a test announcement to the target
            if let Some(ref mut stack) = *crate::net::network::NETWORK.lock() {
                if let Ok(handle) = stack.open_udp(0) {
                    let _ = stack.udp_bind(handle, 0);
                    let (local_id, mesh_size) = {
                        let mesh = crate::net::distributed::NODE_MESH.lock();
                        (mesh.local_id, mesh.local_mesh_size())
                    };
                    let payload = crate::net::distributed::encode_announcement(
                        &local_id,
                        crate::net::distributed::NodeType::Controller,
                        0,
                        "PING",
                        mesh_size,
                    );
                    let msg = crate::net::distributed::encode_message(
                        crate::net::distributed::MessageType::NodeAnnouncement,
                        &payload,
                    );
                    let dst = smoltcp::wire::IpEndpoint::new(
                        smoltcp::wire::IpAddress::Ipv4(target_ip),
                        crate::net::distributed::DISCOVERY_PORT,
                    );
                    match stack.udp_send(handle, &msg, dst) {
                        Ok(()) => history.push(alloc::format!("  PING sent to {}:{}", target_ip, crate::net::distributed::DISCOVERY_PORT)),
                        Err(e) => history.push(alloc::format!("  PING failed: {:?}", e)),
                    }
                    stack.close_socket(handle);
                } else {
                    history.push(String::from("ERROR: Could not open UDP socket"));
                }
            } else {
                history.push(String::from("ERROR: Network stack not available"));
            }
        }
        "EXEC" => {
            // EXEC <node_id_or_alias> <command> [args...]
            if parts.len() < 3 {
                history.push(String::from("USAGE: EXEC [NODE_ID_OR_ALIAS] [COMMAND] [ARGS...]"));
                history.push(String::from("  Runs COMMAND on the remote node's own shell and prints its output."));
                history.push(String::from("  NODE_ID_OR_ALIAS can be the full hex ID or a short alias like peer1 — see PEERS."));
                return;
            }
            let node_id = match crate::net::distributed::NODE_MESH.lock().resolve_target(parts[1]) {
                Some(id) => id,
                None => { history.push(String::from("ERROR: Unknown node (expected a hex ID or peerN alias, see PEERS)")); return; }
            };
            let command = String::from(parts[2]);
            let args = if parts.len() > 3 { parts[3..].join(" ") } else { String::new() };
            let task = crate::net::distributed::RemoteTask::new(&command, &args);
            history.push(alloc::format!("  Sending to {}: {} {}", node_id.to_hex(), command, args));
            match crate::net::distributed::remote_exec(&node_id, task) {
                Ok(stdout) => {
                    match core::str::from_utf8(&stdout) {
                        Ok(s) => { for line in s.lines() { history.push(alloc::format!("  {}", line)); } }
                        Err(_) => history.push(alloc::format!("  [{} bytes, not valid UTF-8]", stdout.len())),
                    }
                }
                Err(e) => history.push(alloc::format!("ERROR: {:?}", e)),
            }
        }
        "DFSGET" => {
            // DFSGET <node_id_or_alias> <remote_path> [local_path]
            if parts.len() < 3 {
                history.push(String::from("USAGE: DFSGET [NODE_ID_OR_ALIAS] [REMOTE_PATH] [LOCAL_PATH]"));
                history.push(String::from("  Reads REMOTE_PATH from the given node. Prints it if LOCAL_PATH is omitted."));
                history.push(String::from("  NODE_ID_OR_ALIAS can be the full hex ID or a short alias like peer1 — see PEERS."));
                return;
            }
            let node_id = match crate::net::distributed::NODE_MESH.lock().resolve_target(parts[1]) {
                Some(id) => id,
                None => { history.push(String::from("ERROR: Unknown node (expected a hex ID or peerN alias, see PEERS)")); return; }
            };
            let net_path = alloc::format!("/net/{}/{}", node_id.to_hex(), parts[2].trim_start_matches('/'));
            match crate::net::distributed::DistributedFs::read_file(&net_path) {
                Ok(data) => {
                    if parts.len() > 3 {
                        let local_path = normalize_path(parts[3]);
                        match VFS.write_file(&local_path, &data) {
                            Ok(()) => history.push(alloc::format!("  Wrote {} bytes to {}", data.len(), local_path)),
                            Err(e) => history.push(alloc::format!("ERROR writing local file: {:?}", e)),
                        }
                    } else {
                        match core::str::from_utf8(&data) {
                            Ok(s) => { for line in s.lines() { history.push(alloc::format!("  {}", line)); } }
                            Err(_) => history.push(alloc::format!("  [{} bytes, not valid UTF-8 — give a LOCAL_PATH to save it instead]", data.len())),
                        }
                    }
                }
                Err(e) => history.push(alloc::format!("ERROR: {:?}", e)),
            }
        }
        "DFSPUT" => {
            // DFSPUT <local_path> <node_id_hex> <remote_path>
            if parts.len() < 4 {
                history.push(String::from("USAGE: DFSPUT [LOCAL_PATH] [NODE_ID_OR_ALIAS] [REMOTE_PATH]"));
                history.push(String::from("  Uploads LOCAL_PATH to REMOTE_PATH on the given node."));
                history.push(String::from("  NODE_ID_OR_ALIAS can be the full hex ID or a short alias like peer1 — see PEERS."));
                return;
            }
            let local_path = normalize_path(parts[1]);
            let node_id = match crate::net::distributed::NODE_MESH.lock().resolve_target(parts[2]) {
                Some(id) => id,
                None => { history.push(String::from("ERROR: Unknown node (expected a hex ID or peerN alias, see PEERS)")); return; }
            };
            let data = match VFS.read_file(&local_path) {
                Ok(d) => d,
                Err(e) => { history.push(alloc::format!("ERROR reading local file: {:?}", e)); return; }
            };
            let net_path = alloc::format!("/net/{}/{}", node_id.to_hex(), parts[3].trim_start_matches('/'));
            match crate::net::distributed::DistributedFs::write_file(&net_path, &data) {
                Ok(()) => history.push(alloc::format!("  Sent {} bytes to {}", data.len(), net_path)),
                Err(e) => history.push(alloc::format!("ERROR: {:?}", e)),
            }
        }
        "MODE" => {
            let sub = parts.get(1).copied().unwrap_or("STATUS");
            match sub {
                "SECURITY" => {
                    crate::security::policy::POLICY.lock().set_mode(crate::security::policy::PolicyMode::Security);
                    history.push(String::from("Switched to SECURITY mode."));
                }
                "PERFORMANCE" => {
                    // Relaxes protections: needs policy.switch. (SECURITY
                    // only tightens them, so anyone may switch back.)
                    match crate::security::users::authorize(crate::security::users::Action::EnterPerformanceMode) {
                        Ok(()) => {
                            crate::security::policy::POLICY.lock().set_mode(crate::security::policy::PolicyMode::Performance);
                            history.push(String::from("Switched to PERFORMANCE mode."));
                        }
                        Err(e) => history.push(alloc::format!("mode: {}", e)),
                    }
                }
                _ => {
                    let mode = crate::security::policy::POLICY.lock().get_mode();
                    history.push(alloc::format!("  Current mode: {:?}", mode));
                    history.push(String::from("  Usage: MODE [SECURITY|PERFORMANCE]"));
                }
            }
        }
        "WIFI" => {
            // The Intel 7265 Wi-Fi card (iwlwifi.rs).
            history.extend(crate::drivers::net::iwlwifi::command(&parts[1..]));
        }
        "DMESG" => {
            // The kernel log, for machines with no serial port (klog.rs):
            // the last 30 lines, or those containing the text given.
            let filter = parts[1..].join(" ");
            let lines = crate::sys::klog::lines(&filter, 30);
            if lines.is_empty() {
                history.push(alloc::format!("DMESG: nothing containing '{}'", filter));
            }
            for l in lines {
                history.push(l.chars().take(110).collect());
            }
        }
        "REBOOT" | "RESTART" => {
            // Saves like power-off, then resets (power.rs). A method name
            // tries only that one, then the triple fault.
            let method = match parts.get(1).copied() {
                None => Some(crate::sys::power::Method::Any),
                Some("ACPI") => Some(crate::sys::power::Method::Acpi),
                Some("CF9") => Some(crate::sys::power::Method::Cf9),
                Some("KBD") => Some(crate::sys::power::Method::Kbd),
                Some("TRIPLE") => Some(crate::sys::power::Method::Triple),
                Some(_) => None,
            };
            match method {
                Some(m) => crate::sys::power::reboot(m),
                None => history.push(String::from("Usage: REBOOT [ACPI|CF9|KBD|TRIPLE]")),
            }
        }
        "CRASH" => {
            // CRASH TEST: stop the system on purpose, to see the crash screen
            // and the saved log (crash.rs). Administrators only.
            let admin = {
                let user = crate::security::users::SESSION.lock().username.clone();
                crate::security::users::USER_DB.lock().find(&user).is_some_and(|u| matches!(u.role, crate::security::users::UserRole::Admin | crate::security::users::UserRole::Root))
            };
            match parts.get(1).copied() {
                Some("TEST") if admin => panic!("CRASH TEST typed in the terminal"),
                Some("TEST") => history.push(String::from("CRASH TEST: administrators only")),
                _ => history.push(String::from("Usage: CRASH TEST   (stops the system: the crash screen and log)")),
            }
        }
        "SYSPROF" => {
            // Time per NT service (sysprof.rs), dumped to the serial log.
            match parts.get(1).copied() {
                Some("START") => { crate::nt::sysprof::start(); history.push(String::from("SYSPROF: on")); }
                Some("STOP") => { crate::nt::sysprof::stop(); history.push(String::from("SYSPROF: off")); }
                Some("DUMP") => { crate::nt::sysprof::dump(); history.push(String::from("SYSPROF: written to the serial log")); }
                _ => history.push(String::from("Usage: SYSPROF START | STOP | DUMP")),
            }
        }
        "SAMPLE" => {
            match parts.get(1).map(|s| s.to_uppercase()).as_deref() {
                Some("START") => {
                    crate::sys::sampler::start();
                    history.push(String::from("SAMPLE: recording (1 sample/ms, ring holds 16 s). Do the slow thing, then SAMPLE STOP."));
                }
                Some("STOP") => {
                    crate::sys::sampler::stop();
                    history.push(alloc::format!("SAMPLE: stopped, {} samples. Run SAMPLE DUMP.", crate::sys::sampler::total()));
                }
                Some("DUMP") => {
                    crate::sys::sampler::dump();
                    history.push(String::from("SAMPLE: histogram written to the serial log."));
                }
                _ => history.push(String::from("Usage: SAMPLE START | STOP | DUMP")),
            }
        }

        "OPEN" => {
            if parts.len() < 2 { history.push(String::from("USAGE: OPEN [TERMINAL|HUB|EXPLORER|LAUNCHER|SETTINGS]")); return; }
            match parts[1] {
                "TERMINAL" => {
                    crate::userland::apps::open_terminal(80, 80, 550, 380, 0);
                    history.push(String::from("OPENED: TERMINAL"));
                }
                "EXPLORER" => {
                    crate::userland::apps::open_file_manager(50, 60, 480, 360, 0);
                    history.push(String::from("OPENED: FILE MANAGER"));
                }
                "LAUNCHER" => {
                    crate::userland::apps::open_app_launcher(180, 50, 440, 380, 0);
                    history.push(String::from("OPENED: APP LAUNCHER"));
                }
                _ => {
                    history.push(alloc::string::String::from("ERROR: UNKNOWN APP: ") + parts[1]);
                    history.push(String::from("  KNOWN APPS: TERMINAL  EXPLORER  SETTINGS  LAUNCHER"));
                }
            }
        }
        // =====================================================================
        // -----| Mesh Offload Commands |-----
        // =====================================================================

        // THREADS — list all active worker threads with offload eligibility
        "THREADS" => {
            let mgr = crate::sched::process::THREAD_MANAGER.lock();
            let count = mgr.thread_count();
            history.push(alloc::format!("  {} worker thread(s) registered:", count));
            if count == 0 {
                history.push(String::from("  (no worker threads)"));
            } else {
                history.push(String::from(
                    "  TID    PID    NAME                             SUB      ELIGIBLE           STATE"
                ));
                history.push(String::from(
                    "  -----  -----  ------------------------------   -------  -----------------  ---------"
                ));
                // Iterate all known PIDs so we catch threads for every process,
                // including pid 0 (kernel) and any user processes.
                let pids: alloc::vec::Vec<u32> = {
                    crate::sched::process::PROCESSOR.lock()
                        .processes.iter()
                        .map(|p| p.pid)
                        .collect()
                };
                let mut printed = false;
                for pid in &pids {
                    for t in mgr.threads_for_pid(*pid) {
                        printed = true;
                        let sub_label = match t.subsystem {
                            crate::sched::process::ThreadSubsystem::Native => "Native ",
                            crate::sched::process::ThreadSubsystem::Win32  => "Win32  ",
                        };
                        let elig_label = if !t.offload_eligible {
                            "no (flagged off)"
                        } else if t.subsystem == crate::sched::process::ThreadSubsystem::Win32 {
                            if t.is_offload_candidate() {
                                "yes (best-effort)"
                            } else {
                                "no  (shared-mem)"
                            }
                        } else {
                            "yes"
                        };
                        history.push(alloc::format!(
                            "  {:<6} {:<6} {:<32} {}  {:<18} {:?}",
                            t.thread_id, t.owner_pid,
                            t.name_str(), sub_label, elig_label, t.state
                        ));
                    }
                }
                if !printed {
                    history.push(String::from("  (no worker threads for any live process)"));
                }
            }
        }

        // BROKER STATUS — show all in-flight offload jobs
        "BROKER" => {
            let sub = parts.get(1).copied().unwrap_or("STATUS");
            if sub == "STATUS" {
                history.push(String::from("  [OFFLOAD BROKER STATUS]"));
                let broker = crate::sched::scheduler::OFFLOAD_BROKER.lock();
                for line in broker.status_lines() {
                    history.push(line);
                }
                history.push(alloc::format!(
                    "  Policy: {:?}",
                    crate::sched::scheduler::OffloadBroker::read_policy()
                ));
            } else {
                history.push(String::from("USAGE: BROKER STATUS"));
            }
        }

        // CHECKPOINT LIST — list checkpoint files on this node
        "CHECKPOINT" => {
            let sub = parts.get(1).copied().unwrap_or("LIST");
            if sub == "LIST" {
                history.push(String::from("  [CHECKPOINTS on local node]"));
                for line in crate::net::checkpoint::CheckpointManager::list_all() {
                    history.push(line);
                }
            } else {
                history.push(String::from("USAGE: CHECKPOINT LIST"));
            }
        }

        // MESH_OFFLOAD_APPROVE <job_id_hex> — approve a pending ask-mode offload
        "MESH_OFFLOAD_APPROVE" => {
            if parts.len() < 2 {
                history.push(String::from("USAGE: MESH_OFFLOAD_APPROVE <job_id_hex>"));
                return;
            }
            let id_str = parts[1].trim_start_matches("0x").trim_start_matches("0X");
            match u64::from_str_radix(id_str, 16) {
                Ok(job_id) => {
                    crate::sched::scheduler::OFFLOAD_BROKER.lock().approve(job_id);
                    history.push(alloc::format!("  Approved offload job {:#x}", job_id));
                }
                Err(_) => history.push(String::from("ERROR: invalid job_id (expected hex)")),
            }
        }

        // MESH_OFFLOAD_DENY <job_id_hex> — deny a pending ask-mode offload
        "MESH_OFFLOAD_DENY" => {
            if parts.len() < 2 {
                history.push(String::from("USAGE: MESH_OFFLOAD_DENY <job_id_hex>"));
                return;
            }
            let id_str = parts[1].trim_start_matches("0x").trim_start_matches("0X");
            match u64::from_str_radix(id_str, 16) {
                Ok(job_id) => {
                    crate::sched::scheduler::OFFLOAD_BROKER.lock().deny(job_id);
                    history.push(alloc::format!("  Denied offload job {:#x}", job_id));
                }
                Err(_) => history.push(String::from("ERROR: invalid job_id (expected hex)")),
            }
        }

        // OFFLOAD <peer_alias_or_hex> <command…>
        // Manually offload a command to a specific peer — useful for testing
        // the full pipeline from the terminal without writing an app.
        "OFFLOAD" => {
            if parts.len() < 3 {
                history.push(String::from("USAGE: OFFLOAD <peer> <command> [args…]"));
                history.push(String::from("  peer: hex NodeId or peer1/peer2 alias (see PEERS)"));
                return;
            }
            let target_str = parts[1];
            let cmd_str: alloc::string::String = parts[2..].join(" ");

            let node_id = crate::net::distributed::NODE_MESH.lock().resolve_target(target_str);
            match node_id {
                None => {
                    history.push(alloc::format!("ERROR: unknown peer '{}'", target_str));
                }
                Some(nid) => {
                    let job_id = crate::net::task_capsule::alloc_job_id();
                    let capsule = crate::net::task_capsule::TaskCapsule::new(
                        job_id,
                        0, 0, 0, true,
                        cmd_str.into_bytes(),
                        alloc::vec::Vec::new(),
                        30_000,
                        crate::net::distributed::CAP_EXEC as u64,
                    );
                    history.push(alloc::format!(
                        "  Offloading job={:#x} to {}…", job_id, nid.to_hex()
                    ));
                    match crate::net::distributed::remote_exec_capsule(&nid, &capsule) {
                        Ok(result) => {
                            history.push(alloc::format!(
                                "  Result: success={}", result.success
                            ));
                            if let Ok(s) = core::str::from_utf8(&result.output_data) {
                                for line in s.lines() {
                                    history.push(alloc::format!("  | {}", line));
                                }
                            }
                            if let Some(ref err) = result.error_msg {
                                history.push(alloc::format!("  ERROR: {}", err));
                            }
                        }
                        Err(e) => {
                            history.push(alloc::format!("  FAILED: {:?}", e));
                        }
                    }
                }
            }
        }

        // CAPSULE TEST — round-trip a test capsule through serialise/deserialise
        "CAPSULE" => {
            let sub = parts.get(1).copied().unwrap_or("TEST");
            if sub == "TEST" {
                let job_id = crate::net::task_capsule::alloc_job_id();
                let capsule = crate::net::task_capsule::TaskCapsule::new(
                    job_id,
                    42,   // owner_pid
                    7,    // thread_id
                    3,    // step_index
                    true, // is_native
                    b"ECHO HELLO FROM CAPSULE TEST".to_vec(),
                    alloc::vec![
                        alloc::string::String::from("/net/aabbccdd/data/asset.bin"),
                    ],
                    15_000,
                    crate::net::distributed::CAP_EXEC as u64
                        | crate::net::distributed::CAP_GPU as u64,
                );
                let bytes = capsule.serialise();
                history.push(alloc::format!("  Serialised: {} bytes", bytes.len()));
                match crate::net::task_capsule::TaskCapsule::deserialise(&bytes) {
                    Some(c) => {
                        history.push(String::from("  Deserialise: OK"));
                        history.push(alloc::format!("  {}", c.describe()));
                        if c.job_id    == job_id
                            && c.owner_pid  == 42
                            && c.thread_id  == 7
                            && c.step_index == 3
                            && c.is_native
                            && c.asset_paths.len() == 1
                        {
                            history.push(String::from("  All fields verified OK"));
                        } else {
                            history.push(String::from("  WARNING: field mismatch"));
                        }
                    }
                    None => history.push(String::from("  Deserialise: FAILED")),
                }
            } else {
                history.push(String::from("USAGE: CAPSULE TEST"));
            }
        }
        // -----| VSH shell |-----
        "SHELL" => {
            match parts.get(1).map(|s| s.to_uppercase()).as_deref() {
                Some("ON") => {
                    let path = parts.get(2).copied().unwrap_or(crate::vsh::shell::DEFAULT_PATH);
                    match crate::vsh::shell::start(path) {
                        Ok(m) => history.push(m),
                        Err(e) => history.push(alloc::format!("SHELL: {}", e)),
                    }
                }
                Some("RELOAD") => match crate::vsh::shell::reload() {
                    Ok(m) => history.push(m),
                    Err(e) => history.push(alloc::format!("SHELL: {}", e)),
                },
                Some("OFF") => {
                    crate::vsh::shell::stop();
                    history.push(String::from("SHELL: off (the built-in panel keeps working)"));
                }
                _ => {
                    history.push(crate::vsh::shell::status());
                    history.push(String::from("Usage: SHELL ON [path] | RELOAD | OFF"));
                }
            }
        }

        _ => { history.push(String::from("UNKNOWN COMMAND: ") + parts[0] + "  (TYPE HELP)"); }
    }
    
}

/// A long message (the installer's) as indented lines that fit the
/// terminal window, broken between words.
fn wrap_lines(text: &str) -> Vec<String> {
    const WIDTH: usize = 56;
    let mut out = Vec::new();
    let mut line = String::from(" ");
    for word in text.split_whitespace() {
        if line.len() > 1 && line.len() + 1 + word.len() > WIDTH {
            out.push(core::mem::replace(&mut line, String::from(" ")));
        }
        line.push(' ');
        line.push_str(word);
    }
    if line.len() > 1 {
        out.push(line);
    }
    out
}
