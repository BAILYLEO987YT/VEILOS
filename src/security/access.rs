// =====================================================================
// -----| Who may touch which files (ACCOUNTS_DESIGN.md, sections 2, 5) |-----
// =====================================================================
//
// Every file has an owner: a user, "shared" (everyone) or "system".
// `may_access` is the one place that decides, for every way in: the Windows
// file layer (nt/file.rs, nt/syscalls.rs, through mode::require_change), the
// desktop, the kernel's own apps and the terminal. It holds in Security and
// Performance Mode alike.
//
//   sensitive   /etc/keys, the user files, /etc/owners.vdb, /etc/log:
//               admins read (logged), only root changes
//   protected   /system, /etc: everyone reads, only root changes
//   system      everyone reads, admins change (OS files, and anything
//               given to "system")
//   shared      everyone (/tmp, /desktop, /mnt: other disks)
//   a user      that user, users they granted it to, and admins (logged)
//
// Where the owner comes from, most specific first: the user file rules
// (/home/<name> is that user's, /tmp/guest the guest's), the ownership
// table (/etc/owners.vdb: owners and grants by path prefix), then its
// default. Files are owned by whoever creates them: `note_created` records
// a new file's creator where it would otherwise get the table's default.
//
// The default: on a new install, everything already on the disk is the
// system's (it is VEILOS itself). On an install from before ownership,
// it is the first admin's (who can hand it over with CHOWN), except the
// files VEILOS ships with, which are the system's.
//
// Paths are compared ignoring case (FAT32; the terminal upper-cases them).
//
// The security log (/etc/log/security.log) records sign-ins, account and
// key changes, ownership changes and every admin access to another user's
// files, with the wall-clock time and the user. Unlike the audit log it is
// always on, and only the kernel writes it.

use alloc::collections::BTreeSet;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use spin::Mutex;

use crate::security::capability::Capability;
use crate::security::users::{self, UserRole};

pub const OWNERS_PATH: &str = "/etc/owners.vdb";
pub const SECURITY_LOG_DIR: &str = "/etc/log";
pub const SECURITY_LOG_PATH: &str = "/etc/log/security.log";

/// What is being done to a file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    /// Open, read, list, run.
    Read,
    /// Write, create, replace.
    Write,
    /// Delete or rename away.
    Delete,
}

/// Who owns a file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Owner {
    System,
    Shared,
    User(u32),
}

impl Owner {
    fn encode(self) -> String {
        match self {
            Owner::System => String::from("system"),
            Owner::Shared => String::from("shared"),
            Owner::User(uid) => format!("{}", uid),
        }
    }

    fn decode(s: &str) -> Option<Self> {
        match s {
            "system" => Some(Owner::System),
            "shared" => Some(Owner::Shared),
            n => n.parse().ok().map(Owner::User),
        }
    }

    /// For people: "system", "shared", or the username.
    pub fn describe(self) -> String {
        match self {
            Owner::User(uid) => users::USER_DB.lock().users.iter().find(|u| u.uid == uid)
                .map_or_else(|| format!("uid {}", uid), |u| u.username.clone()),
            other => other.encode(),
        }
    }
}

/// An ownership table entry: everything under `path` (unless a longer
/// entry or rule says otherwise).
#[derive(Clone, Debug)]
struct Entry {
    /// Lower case, no trailing slash ("/" for the root).
    path: String,
    owner: Owner,
    /// (uid, may change): users the owner shared it with.
    grants: Vec<(u32, bool)>,
}

struct Owners {
    loaded: bool,
    default: Owner,
    entries: Vec<Entry>,
}

static OWNERS: Mutex<Owners> = Mutex::new(Owners { loaded: false, default: Owner::System, entries: Vec::new() });

/// What VEILOS ships at the top of its volume (storage/): the system's on
/// an install from before ownership, whatever the default.
const SHIPPED: &[&str] = &[
    "/system", "/lib", "/fonts", "/themes", "/en-us", "/tests", "/shell.vsh", "/config.vsh",
    "/cmd.exe", "/notepad.exe", "/wallpapr.jpg", "/spin.gif", "/data.bin", "/large.txt",
    "/readme.md", "/veilos.vol",
];

/// Shared by default (entries in the table, so admins can change them).
const SHARED_BY_DEFAULT: &[&str] = &["/tmp", "/desktop", "/mnt"];

fn norm(path: &str) -> String {
    let mut p = path.trim().to_ascii_lowercase().replace('\\', "/");
    while p.len() > 1 && p.ends_with('/') {
        p.pop();
    }
    if !p.starts_with('/') {
        p.insert(0, '/');
    }
    p
}

/// Is `path` `prefix` or inside it?
fn under(path: &str, prefix: &str) -> bool {
    prefix == "/" || path == prefix || (path.starts_with(prefix) && path.as_bytes().get(prefix.len()) == Some(&b'/'))
}

/// Only admins read these, only root changes them.
fn sensitive(p: &str) -> bool {
    under(p, "/etc/keys") || under(p, "/etc/log") || p == "/etc/users.vdb" || p == "/etc/users2.vdb" || p == OWNERS_PATH
}

/// Everyone reads these, only root changes them.
fn protected(p: &str) -> bool {
    under(p, "/system") || under(p, "/etc")
}

// -----| the table on disk |-----

impl Owners {
    fn load(&mut self) {
        if self.loaded {
            return;
        }
        self.loaded = true;
        match crate::fs::vfs::VFS.read_file(OWNERS_PATH) {
            Ok(bytes) => {
                let text = core::str::from_utf8(&bytes).unwrap_or("");
                for line in text.lines() {
                    if let Some(v) = line.strip_prefix("@default=") {
                        self.default = Owner::decode(v.trim()).unwrap_or(Owner::System);
                        continue;
                    }
                    if line.is_empty() || line.starts_with('#') {
                        continue;
                    }
                    let mut f = line.split('\t');
                    let (Some(path), Some(owner)) = (f.next(), f.next().and_then(Owner::decode)) else { continue };
                    let grants = f.next().unwrap_or("").split(',').filter_map(|g| {
                        let (uid, how) = g.split_once(':')?;
                        Some((uid.parse().ok()?, how == "rw"))
                    }).collect();
                    self.entries.push(Entry { path: norm(path), owner, grants });
                }
                crate::serial_println!("[ACCESS] {} owner entries, default {}", self.entries.len(), self.default.encode());
            }
            Err(_) => self.create(),
        }
    }

    /// No table yet: VEILOS was just installed, or is upgrading from before
    /// ownership (the users file existed already).
    fn create(&mut self) {
        let (fresh, first_admin) = {
            let db = users::USER_DB.lock();
            let first = db.users.iter().filter(|u| u.role == UserRole::Admin).map(|u| u.uid).min();
            (db.fresh_install, first)
        };
        self.default = match (fresh, first_admin) {
            (false, Some(uid)) => Owner::User(uid),
            _ => Owner::System,
        };
        for p in SHARED_BY_DEFAULT {
            self.entries.push(Entry { path: String::from(*p), owner: Owner::Shared, grants: Vec::new() });
        }
        if self.default != Owner::System {
            for p in SHIPPED {
                self.entries.push(Entry { path: String::from(*p), owner: Owner::System, grants: Vec::new() });
            }
        }
        crate::serial_println!("[ACCESS] ownership table made: files already here belong to {} ({})",
            self.default.encode(), if fresh { "new install" } else { "from before ownership" });
        self.save();
    }

    fn save(&self) {
        let mut text = String::from("# VEILOS file owners - path, owner (uid, shared, system), grants (uid:r|rw)\n");
        text.push_str(&format!("@default={}\n", self.default.encode()));
        for e in &self.entries {
            let grants: Vec<String> = e.grants.iter().map(|(u, rw)| format!("{}:{}", u, if *rw { "rw" } else { "r" })).collect();
            text.push_str(&format!("{}\t{}\t{}\n", e.path, e.owner.encode(), grants.join(",")));
        }
        let vfs = &crate::fs::vfs::VFS;
        let _ = vfs.create_dir("/etc");
        if vfs.write_file(OWNERS_PATH, text.as_bytes()).is_err() {
            crate::serial_println!("[ACCESS] {} could not be written", OWNERS_PATH);
        }
    }

    /// The owner of `p` and its grants, and whether that came from the
    /// table's default (nothing more specific covers it).
    fn effective(&self, p: &str) -> (Owner, Vec<(u32, bool)>, bool) {
        // The longest table entry.
        let entry = self.entries.iter().filter(|e| under(p, &e.path)).max_by_key(|e| e.path.len());
        // The user file rules: /home/<name>, /tmp/guest.
        let rule: Option<(usize, Owner)> = if let Some(rest) = p.strip_prefix("/home/") {
            let name = rest.split('/').next().unwrap_or("");
            users::USER_DB.lock().find(name).map(|u| ("/home/".len() + name.len(), Owner::User(u.uid)))
        } else if p == "/home" {
            Some((5, Owner::System))
        } else if under(p, users::GUEST_HOME) {
            users::USER_DB.lock().find("guest").map(|u| (users::GUEST_HOME.len(), Owner::User(u.uid)))
        } else {
            None
        };
        match (entry, rule) {
            (Some(e), Some((len, _))) if e.path.len() >= len => (e.owner, e.grants.clone(), false),
            (_, Some((_, owner))) => (owner, Vec::new(), false),
            (Some(e), None) => (e.owner, e.grants.clone(), false),
            (None, None) => (self.default, Vec::new(), true),
        }
    }
}

/// Load (or make) the ownership table. After users::init.
pub fn init() {
    OWNERS.lock().load();
}

// -----| deciding |-----

/// Someone acting: a uid, and what their account may do.
#[derive(Clone, Copy, Debug)]
pub struct Actor {
    pub uid: u32,
    /// root (or the kernel itself, uid 0 with nobody signed in).
    pub root: bool,
    /// user.manage: sees everything (logged).
    pub admin: bool,
}

impl Actor {
    pub const KERNEL: Actor = Actor { uid: 0, root: true, admin: true };
    /// No account: shared and system files only (mesh commands).
    pub const NOBODY: Actor = Actor { uid: u32::MAX, root: false, admin: false };

    /// The account with this uid.
    pub fn of_uid(uid: u32) -> Actor {
        if uid == 0 {
            return Actor::KERNEL;
        }
        let db = users::USER_DB.lock();
        match db.users.iter().find(|u| u.uid == uid) {
            Some(u) => Actor {
                uid,
                root: u.role == UserRole::Root,
                admin: db.capability_set_for(&u.username).has(Capability::UserManage),
            },
            None => Actor { uid, root: false, admin: false },
        }
    }

    /// The signed-in user (the kernel's own apps and the terminal act for
    /// them). Nobody signed in: the kernel.
    pub fn session() -> Actor {
        if REMOTE.load(core::sync::atomic::Ordering::Acquire) {
            return Actor::NOBODY;
        }
        let (name, logged_in) = {
            let s = users::SESSION.lock();
            (s.username.clone(), s.logged_in)
        };
        if !logged_in {
            return Actor::KERNEL;
        }
        let uid = users::USER_DB.lock().find(&name).map(|u| u.uid);
        match uid {
            Some(uid) => Actor::of_uid(uid),
            None => Actor { uid: u32::MAX, root: false, admin: false },
        }
    }
}

/// The signed-in user's uid (0: nobody, the kernel).
pub fn session_uid() -> u32 {
    Actor::session().uid
}

/// A command from another machine on the mesh is running
/// (terminal::execute_remote_command): the terminal's file checks act as
/// nobody, not as whoever is signed in here.
pub static REMOTE: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// (admin uid, owner uid, top folder) already logged this session.
static LOGGED: Mutex<BTreeSet<(u32, u32, String)>> = Mutex::new(BTreeSet::new());

/// May `who` do `op` to `path`? Admin access to another user's files is
/// allowed and logged (once per user, owner and top folder a session).
pub fn may_access(who: Actor, path: &str, op: Op) -> bool {
    if who.root {
        return true;
    }
    if who.uid == u32::MAX && op != Op::Read {
        return false; // nobody (a mesh command) only reads
    }
    let p = norm(path);
    if sensitive(&p) {
        let ok = op == Op::Read && who.admin;
        if ok {
            log_admin_access(who, u32::MAX, &p);
        }
        return ok;
    }
    if protected(&p) {
        return op == Op::Read;
    }
    let (owner, grants) = {
        let mut o = OWNERS.lock();
        o.load();
        let (owner, grants, _) = o.effective(&p);
        (owner, grants)
    };
    match owner {
        Owner::Shared => true,
        Owner::System => op == Op::Read || who.admin,
        Owner::User(uid) if uid == who.uid => true,
        Owner::User(uid) => {
            if let Some(&(_, rw)) = grants.iter().find(|(g, _)| *g == who.uid) {
                if op == Op::Read || rw {
                    return true;
                }
            }
            if who.admin {
                log_admin_access(who, uid, &p);
                return true;
            }
            false
        }
    }
}

fn log_admin_access(who: Actor, owner: u32, p: &str) {
    // The first two components: /home/bob, /etc/keys.
    let top: String = {
        let mut parts = p.splitn(4, '/');
        let _ = parts.next();
        let a = parts.next().unwrap_or("");
        let b = parts.next().unwrap_or("");
        if b.is_empty() { format!("/{}", a) } else { format!("/{}/{}", a, b) }
    };
    if LOGGED.lock().insert((who.uid, owner, top.clone())) {
        let whose = if owner == u32::MAX { String::from("system's protected files") } else { format!("{}'s files", Owner::User(owner).describe()) };
        seclog_as(who.uid, &format!("admin access to {} ({})", whose, top));
    }
}

/// The capability an operation needs from the account as well.
fn capability_for(op: Op) -> Capability {
    match op {
        Op::Read => Capability::FsRead,
        Op::Write => Capability::FsWrite,
        Op::Delete => Capability::FsModify,
    }
}

/// `user_may` without the notification (the file manager asks every refresh).
pub fn user_may_quiet(path: &str, op: Op) -> bool {
    let who = Actor::session();
    let cap_ok = who.root || who.uid == u32::MAX || {
        let name = users::SESSION.lock().username.clone();
        users::USER_DB.lock().capability_set_for(&name).has(capability_for(op))
    };
    cap_ok && may_access(who, path, op)
}

/// The signed-in user doing `op` to `path` through the kernel's own apps,
/// the terminal or the desktop: their account's capability and
/// `may_access`. False is shown as a notification.
pub fn user_may(path: &str, op: Op) -> bool {
    let who = Actor::session();
    let cap_ok = who.root || who.uid == u32::MAX || {
        let name = users::SESSION.lock().username.clone();
        users::USER_DB.lock().capability_set_for(&name).has(capability_for(op))
    };
    let ok = cap_ok && may_access(who, path, op);
    if !ok {
        let what = match op { Op::Read => "open", Op::Write => "change", Op::Delete => "delete" };
        crate::gui::desktop::notify(
            crate::gui::desktop::NotifCategory::Warning,
            "ACCESS",
            &format!("You can't {} {}", what, path),
        );
    }
    ok
}

/// The uid a Windows process acts as (sched::process::Process::uid).
pub fn process_actor(proc: &crate::sched::process::Process) -> Actor {
    Actor::of_uid(proc.uid.load(core::sync::atomic::Ordering::Relaxed))
}

/// The uid a new process runs as: its parent's, else the signed-in user's
/// (nobody signed in: 0, the kernel).
pub fn uid_for_new(parent_pid: u32) -> u32 {
    if parent_pid != 0 {
        if let Some(p) = crate::sched::process::PROCESSOR.lock().find(parent_pid) {
            return p.uid.load(core::sync::atomic::Ordering::Relaxed);
        }
    }
    session_uid()
}

/// `uid` made `path`: where it would otherwise only get the table's default,
/// it is theirs (files belong to whoever creates them; in someone's home,
/// a shared folder or a system folder they belong to that).
pub fn note_created(uid: u32, path: &str) {
    if uid == 0 || uid == u32::MAX {
        return;
    }
    let p = norm(path);
    if sensitive(&p) || protected(&p) {
        return;
    }
    let mut o = OWNERS.lock();
    if !o.loaded {
        return; // before access::init (boot): nothing is owned yet
    }
    let (owner, _, from_default) = o.effective(&p);
    if from_default && owner != Owner::User(uid) {
        o.entries.push(Entry { path: p, owner: Owner::User(uid), grants: Vec::new() });
        o.save();
    }
}

/// A file or folder was deleted: drop its entries (and those under it).
pub fn note_deleted(path: &str) {
    let p = norm(path);
    let mut o = OWNERS.lock();
    if !o.loaded {
        return; // before access::init (boot): nothing is owned yet
    }
    let before = o.entries.len();
    o.entries.retain(|e| !under(&e.path, &p) || SHARED_BY_DEFAULT.contains(&e.path.as_str()));
    if o.entries.len() != before {
        o.save();
    }
}

/// A file or folder was renamed: its entries (and those under it) follow.
pub fn note_renamed(from: &str, to: &str) {
    let (f, t) = (norm(from), norm(to));
    let mut o = OWNERS.lock();
    if !o.loaded {
        return; // before access::init (boot): nothing is owned yet
    }
    let mut changed = false;
    for e in o.entries.iter_mut() {
        if under(&e.path, &f) && !SHARED_BY_DEFAULT.contains(&e.path.as_str()) {
            e.path = format!("{}{}", t, &e.path[f.len()..]);
            changed = true;
        }
    }
    if changed {
        o.save();
    }
}

// -----| changing owners (terminal OWNER, CHOWN, GRANT) |-----

/// The owner of `path`, and its grants (uid, may change), for people.
pub fn describe(path: &str) -> (String, Vec<(String, bool)>) {
    let p = norm(path);
    if sensitive(&p) {
        return (String::from("system (protected; admins read)"), Vec::new());
    }
    if protected(&p) {
        return (String::from("system (protected)"), Vec::new());
    }
    let (owner, grants) = {
        let mut o = OWNERS.lock();
        o.load();
        let (owner, grants, _) = o.effective(&p);
        (owner, grants)
    };
    (owner.describe(), grants.into_iter().map(|(u, rw)| (Owner::User(u).describe(), rw)).collect())
}

/// An admin gives `path` (and what is under it) to `owner`.
pub fn set_owner(who: Actor, path: &str, owner: Owner) -> Result<(), &'static str> {
    if !who.admin {
        return Err("only an admin can change who owns something");
    }
    let p = norm(path);
    if sensitive(&p) || protected(&p) {
        return Err("system folders always belong to the system");
    }
    let mut o = OWNERS.lock();
    o.load();
    // The grants on that exact path are kept. What is inside it goes with
    // it: deeper entries are dropped (but not the shared folders' own).
    let grants = o.entries.iter().find(|e| e.path == p).map(|e| e.grants.clone()).unwrap_or_default();
    o.entries.retain(|e| !under(&e.path, &p) || (e.path != p && SHARED_BY_DEFAULT.contains(&e.path.as_str())));
    o.entries.push(Entry { path: p.clone(), owner, grants });
    o.save();
    drop(o);
    seclog_as(who.uid, &format!("owner of {} set to {}", p, owner.describe()));
    Ok(())
}

/// The owner of `path` (or an admin) shares it with `uid`: read, read and
/// change, or (None) no longer.
pub fn set_grant(who: Actor, path: &str, uid: u32, grant: Option<bool>) -> Result<(), &'static str> {
    let p = norm(path);
    if sensitive(&p) || protected(&p) {
        return Err("system folders can't be shared out");
    }
    let mut o = OWNERS.lock();
    o.load();
    let (owner, grants, _) = o.effective(&p);
    let mine = owner == Owner::User(who.uid);
    if !(mine || who.admin) {
        return Err("only its owner or an admin can share it");
    }
    if !matches!(owner, Owner::User(_)) {
        return Err("only a user's own files are shared out (shared and system ones are everyone's to read)");
    }
    let mut grants = grants;
    grants.retain(|(u, _)| *u != uid);
    if let Some(rw) = grant {
        grants.push((uid, rw));
    }
    o.entries.retain(|e| e.path != p);
    o.entries.push(Entry { path: p.clone(), owner, grants });
    o.save();
    drop(o);
    let how = match grant { Some(true) => "read and change", Some(false) => "read", None => "nothing (removed)" };
    seclog_as(who.uid, &format!("{} granted {} on {}", Owner::User(uid).describe(), how, p));
    Ok(())
}

// -----| the security log |-----

/// Append a line to the security log: time, the signed-in user, `what`.
pub fn seclog(what: &str) {
    let name = users::SESSION.lock().username.clone();
    seclog_line(if name.is_empty() { "-" } else { &name }, what);
}

/// Append a line for the user with `uid`.
pub fn seclog_as(uid: u32, what: &str) {
    let name = if uid == 0 { String::from("kernel") } else { Owner::User(uid).describe() };
    seclog_line(&name, what);
}

/// Append a line naming `user`.
pub fn seclog_line(user: &str, what: &str) {
    let t = crate::vsh::shell::wall_clock();
    let line = format!("{:04}-{:02}-{:02} {:02}:{:02}:{:02} {}: {}\n",
        t.year, t.month, t.day, t.hour, t.minute, t.second, user, what);
    crate::serial_println!("[SECLOG] {}", line.trim_end());
    let vfs = &crate::fs::vfs::VFS;
    let _ = vfs.create_dir("/etc");
    let _ = vfs.create_dir(SECURITY_LOG_DIR);
    let ok = match vfs.open(SECURITY_LOG_PATH) {
        Ok(f) => {
            let at = f.len();
            f.write_at(at, line.as_bytes()).and_then(|_| f.sync()).is_ok()
        }
        Err(_) => vfs.write_file(SECURITY_LOG_PATH, line.as_bytes()).is_ok(),
    };
    if !ok {
        crate::serial_println!("[SECLOG] not written to {}", SECURITY_LOG_PATH);
    }
}

/// At sign-out: admin accesses are logged again in the next session.
pub fn end_session() {
    LOGGED.lock().clear();
}
