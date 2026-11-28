// =====================================================================
// -----| VEILOS accounts (ACCOUNTS_DESIGN.md) |-----
//
//  User                  — one account: name, display name, uid, role,
//                          sign-in steps, flags, per-user capabilities
//  Sign-in               — steps, each met by one of its methods (password,
//                          PIN, plug-in); every step checked, then a
//                          per-account back-off on failure (never a
//                          permanent lock)
//  Password hashing      — PBKDF built from SHA-256 (sha2 crate, no_std)
//                          100 000 rounds of SHA-256(salt ‖ password ‖ prev)
//  Persistence           — /etc/users2.vdb (v2, tab-separated key=value);
//                          /etc/users.vdb (v1) is read once to migrate and
//                          left as it was, for older builds on the same disk
//  authorize             — the one place account permissions are decided
//  Session               — active user, login time, idle time, lock state
//  Roles                 — Root / Admin / Standard / Guest
//  Capability profiles   — role defaults, plus and minus per user
// =====================================================================

#![allow(dead_code)]

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use spin::Mutex;
use core::sync::atomic::{AtomicU64, Ordering};
use sha2::{Sha256, Digest};
use crate::println;
use crate::security::capability::{Capability, CapabilitySet};

// =====================================================================
// -----| Constants |-----
// =====================================================================

/// The v1 user database: read once to migrate, never written.
pub const USERS_VDB_PATH: &str = "/etc/users.vdb";
/// The v2 user database.
pub const USERS2_VDB_PATH: &str = "/etc/users2.vdb";
/// VFS path remembering the last logged-in username.
pub const LAST_USER_PATH: &str = "/etc/last_user";

/// Lock-screen idle timeout in milliseconds (default 5 minutes).
pub const DEFAULT_IDLE_TIMEOUT_MS: u64 = 5 * 60 * 1_000;

/// The longest password, in characters.
pub const PASSWORD_MAX: usize = 256;
/// The longest PIN, in digits.
pub const PIN_MAX: usize = 64;
/// No minimum (password or PIN) may be set below this.
pub const SECRET_MIN_FLOOR: usize = 4;

/// The guest account's home: wiped at sign-out.
pub const GUEST_HOME: &str = "/tmp/guest";

/// PBKDF iteration count.  100 k rounds of SHA-256 is enough for a
/// no-std kernel implementation; replace with Argon2id when a suitable
/// no_std crate is available.
const PBKDF_ROUNDS: u32 = 100_000;

/// Salt length in bytes embedded in each password hash field.
const SALT_LEN: usize = 16;

/// The first uid given to a person's account (root is 0, guest 999).
const FIRST_UID: u32 = 1000;
const GUEST_UID: u32 = 999;

// =====================================================================
// -----| User Role |-----
// =====================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserRole {
    /// The system account. Owns system files; can't sign in at the login
    /// screen (admins act with its rights through elevation).
    Root,
    /// Can manage users, install packages, change policy.
    Admin,
    /// Standard desktop user.
    Standard,
    /// Ephemeral session, no persistent home, restricted caps.
    Guest,
}

impl UserRole {
    pub fn name(self) -> &'static str {
        match self {
            UserRole::Root     => "root",
            UserRole::Admin    => "admin",
            UserRole::Standard => "standard",
            UserRole::Guest    => "guest",
        }
    }

    /// Lenient: anything unknown is Standard (the v1 file's reading).
    pub fn from_str(s: &str) -> Self {
        Self::parse(s).unwrap_or(UserRole::Standard)
    }

    /// Strict, ignoring case: None for an unknown role.
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "root"     => Some(UserRole::Root),
            "admin"    => Some(UserRole::Admin),
            "standard" => Some(UserRole::Standard),
            "guest"    => Some(UserRole::Guest),
            _          => None,
        }
    }

    fn index(self) -> usize {
        match self {
            UserRole::Root => 0,
            UserRole::Admin => 1,
            UserRole::Standard => 2,
            UserRole::Guest => 3,
        }
    }

    /// Return the capability bitmask granted to this role by default.
    pub fn default_caps(self) -> u64 {
        use Capability::*;
        match self {
            UserRole::Root => {
                // All capabilities.
                u64::MAX
            }
            UserRole::Admin => {
                FsRead.bit() | FsWrite.bit() | FsExec.bit() | FsModify.bit()
                | IpcCreate.bit() | IpcSend.bit() | IpcReceive.bit()
                | GpuSubmit.bit() | GpuDisplay.bit() | GpuShader.bit()
                | NetConnect.bit() | NetBind.bit()
                | ProcFork.bit() | ProcKill.bit() | ProcNice.bit()
                | PolicySwitch.bit() | SystemPower.bit()
                | UserManage.bit()
            }
            UserRole::Standard => {
                FsRead.bit() | FsWrite.bit() | FsExec.bit() | FsModify.bit()
                | IpcCreate.bit() | IpcSend.bit() | IpcReceive.bit()
                | GpuSubmit.bit() | GpuShader.bit()
                | NetConnect.bit()
                | ProcFork.bit() | ProcKill.bit()
            }
            UserRole::Guest => {
                // Read-only filesystem, no network, no IPC broadcast.
                FsRead.bit()
                | IpcReceive.bit()
                | GpuSubmit.bit()
            }
        }
    }
}

// =====================================================================
// -----| Password hashing |-----
// =====================================================================

/// Hash a secret (password or PIN) with a caller-supplied 16-byte salt.
///
/// Hash = PBKDF-SHA256(password, salt, PBKDF_ROUNDS)
///
/// The stored field is:  hex(salt) ++ "$" ++ hex(hash[0..32])
pub fn hash_password(password: &str, salt: &[u8; SALT_LEN]) -> String {
    let mut state = [0u8; 32];

    // Round 0: SHA-256(salt ‖ password)
    let mut h = Sha256::new();
    h.update(salt);
    h.update(password.as_bytes());
    let first = h.finalize();
    state.copy_from_slice(&first);

    // Rounds 1..PBKDF_ROUNDS: SHA-256(state ‖ salt)
    for _ in 1..PBKDF_ROUNDS {
        let mut h2 = Sha256::new();
        h2.update(&state);
        h2.update(salt);
        let next = h2.finalize();
        state.copy_from_slice(&next);
    }

    // Encode as salt_hex + "$" + hash_hex
    let mut out = String::new();
    for b in salt.iter() {
        out.push_str(&format!("{:02x}", b));
    }
    out.push('$');
    for b in state.iter() {
        out.push_str(&format!("{:02x}", b));
    }
    out
}

/// Verify a plaintext secret against a stored hash field.
pub fn verify_password(password: &str, stored: &str) -> bool {
    // Parse: salt_hex (32 hex chars) + "$" + hash_hex (64 hex chars)
    let parts: Vec<&str> = stored.splitn(2, '$').collect();
    if parts.len() != 2 || parts[0].len() != SALT_LEN * 2 {
        return false;
    }
    let mut salt = [0u8; SALT_LEN];
    for (i, chunk) in parts[0].as_bytes().chunks(2).enumerate() {
        if i >= SALT_LEN { break; }
        let hi = (chunk[0] as char).to_digit(16).unwrap_or(0) as u8;
        let lo = (chunk[1] as char).to_digit(16).unwrap_or(0) as u8;
        salt[i] = (hi << 4) | lo;
    }
    let computed = hash_password(password, &salt);
    // Constant-time comparison via XOR accumulation.
    if computed.len() != stored.len() { return false; }
    let diff: u8 = computed.bytes().zip(stored.bytes())
        .fold(0u8, |acc, (a, b)| acc | (a ^ b));
    diff == 0
}

/// A fresh random salt for a password hash (security::entropy). It used to
/// be derived from the username and the tick count, which two accounts
/// created at the same moment, or the same account on two machines, could
/// share.
fn new_salt() -> [u8; SALT_LEN] {
    let mut salt = [0u8; SALT_LEN];
    crate::security::entropy::fill(&mut salt);
    salt
}

/// The same work as checking a secret, for a name with no account: the
/// time a sign-in takes doesn't say whether the account exists.
pub fn spend_verify_time(secret: &str) {
    let _ = hash_password(secret, &[0u8; SALT_LEN]);
}

// =====================================================================
// -----| Usernames and display names |-----
// =====================================================================

/// Names no new account may take: the built-in accounts, and `system`,
/// which owns system files (ACCOUNTS_DESIGN.md, section 2).
const RESERVED_NAMES: &[&str] = &["root", "guest", "system"];

/// A new username: 1-32 of `a-z 0-9 . _ -`, starting with a letter, and not
/// reserved. `:` and tabs in particular would break the user files.
pub fn validate_username(name: &str) -> Result<(), &'static str> {
    if name.is_empty() || name.len() > 32 {
        return Err("a username is 1 to 32 characters");
    }
    if !name.starts_with(|c: char| c.is_ascii_lowercase()) {
        return Err("a username starts with a letter (a-z)");
    }
    if !name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-')) {
        return Err("a username uses only a-z, 0-9, '.', '_' and '-'");
    }
    if RESERVED_NAMES.contains(&name) {
        return Err("that username is reserved");
    }
    Ok(())
}

/// A display name as stored: no control characters (tabs and newlines would
/// break the file), trimmed, at most 64 characters.
pub fn clean_display_name(name: &str) -> String {
    name.chars().filter(|c| !c.is_control()).take(64).collect::<String>().trim().into()
}

// =====================================================================
// -----| Account policy |-----
// =====================================================================

/// System-wide rules for accounts. Defaults here; the system VSH script
/// sets them (stage A4).
#[derive(Clone, Copy, Debug)]
pub struct AccountPolicy {
    /// Failed sign-ins (in a row, per account) before the delays start.
    pub backoff_after: u32,
    /// The longest delay, in seconds.
    pub backoff_max_secs: u64,
    /// Shortest password, in characters (never below SECRET_MIN_FLOOR).
    pub password_min: usize,
    /// Shortest PIN, in digits (never below SECRET_MIN_FLOOR).
    pub pin_min: usize,
    /// The fewest sign-in steps each role needs: Root, Admin, Standard,
    /// Guest. 0 allows an account with no password.
    pub min_steps: [u8; 4],
}

impl AccountPolicy {
    pub const fn new() -> Self {
        Self { backoff_after: 3, backoff_max_secs: 60, password_min: 4, pin_min: 4, min_steps: [0; 4] }
    }

    pub fn min_steps_for(&self, role: UserRole) -> usize {
        // Guest never has a password.
        if role == UserRole::Guest { 0 } else { self.min_steps[role.index()] as usize }
    }

    /// The delay after `failures` failed sign-ins in a row, in seconds:
    /// none until `backoff_after`, then 1, 2, 4 ... up to the maximum.
    pub fn delay_secs(&self, failures: u32) -> u64 {
        if failures < self.backoff_after {
            return 0;
        }
        let exp = (failures - self.backoff_after).min(20);
        (1u64 << exp).min(self.backoff_max_secs)
    }
}

pub static POLICY: Mutex<AccountPolicy> = Mutex::new(AccountPolicy::new());

/// What kind of secret.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SecretKind { Password, Pin }

/// Check a new secret against the length rules: passwords up to 256
/// characters, PINs 4-64 digits, minimums from POLICY. Counted in
/// characters, not bytes.
pub fn validate_secret(kind: SecretKind, secret: &str) -> Result<(), String> {
    let p = *POLICY.lock();
    let n = secret.chars().count();
    match kind {
        SecretKind::Password => {
            let min = p.password_min.max(SECRET_MIN_FLOOR);
            if n < min {
                return Err(format!("a password is at least {} characters", min));
            }
            if n > PASSWORD_MAX {
                return Err(format!("a password is at most {} characters", PASSWORD_MAX));
            }
        }
        SecretKind::Pin => {
            if !secret.chars().all(|c| c.is_ascii_digit()) {
                return Err(String::from("a PIN is digits only"));
            }
            let min = p.pin_min.max(SECRET_MIN_FLOOR);
            if n < min || n > PIN_MAX {
                return Err(format!("a PIN is {} to {} digits", min, PIN_MAX));
            }
        }
    }
    Ok(())
}

// =====================================================================
// -----| Input activity (idle lock) |-----
// =====================================================================

/// When the last key or mouse event arrived (TICKS, ms). An atomic, not a
/// field of SESSION, because the mouse drivers record it from their
/// interrupts, where taking the SESSION lock could deadlock.
pub static LAST_INPUT_MS: AtomicU64 = AtomicU64::new(0);

/// Any keyboard or mouse input: resets the idle-lock timer. Safe from an
/// interrupt (one atomic load and store, no locks).
pub fn note_input() {
    let now = crate::arch::interrupts::TICKS.load(Ordering::Relaxed);
    LAST_INPUT_MS.store(now, Ordering::Relaxed);
}

fn now_ticks() -> u64 {
    crate::arch::interrupts::TICKS.load(Ordering::Relaxed)
}

/// The wall-clock time as stored in the user file: 2026-11-28T11:44:00.
fn now_stamp() -> String {
    let t = crate::vsh::shell::wall_clock();
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}", t.year, t.month, t.day, t.hour, t.minute, t.second)
}

// =====================================================================
// -----| Sign-in steps and methods |-----
// =====================================================================

/// One way to meet a sign-in step.
#[derive(Clone, Debug)]
pub enum Method {
    /// A password: its hash (hash_password).
    Password(String),
    /// A PIN: its hash.
    Pin(String),
    /// A plug-in method (stage A7): its name and its own stored data.
    /// Until plug-ins exist it never succeeds.
    Plugin { name: String, data: String },
}

impl Method {
    pub fn password(secret: &str) -> Self {
        Method::Password(hash_password(secret, &new_salt()))
    }

    pub fn pin(secret: &str) -> Self {
        Method::Pin(hash_password(secret, &new_salt()))
    }

    pub fn kind_name(&self) -> &str {
        match self {
            Method::Password(_) => "password",
            Method::Pin(_) => "pin",
            Method::Plugin { name, .. } => name,
        }
    }

    fn verify(&self, secret: &str) -> bool {
        match self {
            Method::Password(h) | Method::Pin(h) => verify_password(secret, h),
            Method::Plugin { .. } => false,
        }
    }

    /// `kind:data` for the user file. Data never holds `|`, `;` or tabs.
    fn encode(&self) -> String {
        match self {
            Method::Password(h) => format!("password:{}", h),
            Method::Pin(h) => format!("pin:{}", h),
            Method::Plugin { name, data } => format!("plugin.{}:{}", name, data),
        }
    }

    fn decode(s: &str) -> Option<Self> {
        let (kind, data) = s.split_once(':')?;
        let data = String::from(data);
        match kind {
            "password" => Some(Method::Password(data)),
            "pin" => Some(Method::Pin(data)),
            k => k.strip_prefix("plugin.").map(|n| Method::Plugin { name: String::from(n), data }),
        }
    }
}

/// A sign-in step: met by any one of its methods.
#[derive(Clone, Debug, Default)]
pub struct Step {
    pub methods: Vec<Method>,
}

/// Why a sign-in failed. Deliberately says nothing about which step was
/// wrong, or whether the account exists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthError {
    /// Wrong secret(s), or no such account.
    Incorrect,
    /// Too many failures: try again in this many seconds.
    Wait(u64),
    /// The account is turned off.
    Disabled,
    /// root and other system accounts don't sign in at the login screen.
    SystemAccount,
}

impl AuthError {
    pub fn message(self) -> String {
        match self {
            AuthError::Incorrect => String::from("Username or password is incorrect"),
            AuthError::Wait(s) => format!("Too many attempts: try again in {} s", s),
            AuthError::Disabled => String::from("This account is turned off"),
            AuthError::SystemAccount => String::from("This account can't sign in here"),
        }
    }
}

// =====================================================================
// -----| User |-----
// =====================================================================

#[derive(Clone)]
pub struct User {
    /// Login name (validate_username for new accounts).
    pub username:        String,
    /// Shown on the login screen; anything printable, up to 64 characters.
    pub display_name:    String,
    /// Numeric id: root 0, guest 999, people from 1000.
    pub uid:             u32,
    /// Role determines the default capability profile.
    pub role:            UserRole,
    /// Absolute VFS path to the user's home directory.
    pub home_dir:        String,
    /// Off: can't sign in.
    pub enabled:         bool,
    /// Not shown as a tile on the login screen ("Other user" reaches it).
    pub hidden:          bool,
    /// Signs in at boot without asking (only with no sign-in steps).
    pub auto_login:      bool,
    /// Set up sign-in at the next sign-in (role rules, first sign-in).
    pub must_change:     bool,
    /// Sign-in steps, all needed. Empty: no password.
    pub steps:           Vec<Step>,
    /// Failed sign-ins in a row.
    pub failed_attempts: u32,
    /// No sign-in before this (TICKS). Not saved: after a restart the
    /// next failure brings the delay back (failed_attempts is saved).
    pub backoff_until:   u64,
    /// Wall-clock times: created, last signed in ("" if never).
    pub created:         String,
    pub last_login:      String,
    /// Capabilities added to and removed from the role's defaults.
    pub caps_extra:      u64,
    pub caps_removed:    u64,
    /// Saved-key choices (stage A2): an admin slot, a recovery key.
    pub admin_slot:      bool,
    pub recovery_key:    bool,
}

impl User {
    /// A new account. An empty password means no sign-in steps. The uid is
    /// given by UserDatabase::add.
    pub fn new(username: &str, password: &str, role: UserRole) -> Self {
        let steps = if password.is_empty() {
            Vec::new()
        } else {
            alloc::vec![Step { methods: alloc::vec![Method::password(password)] }]
        };
        Self {
            username:        String::from(username),
            display_name:    String::from(username),
            uid:             0,
            role,
            home_dir:        if role == UserRole::Guest {
                                 String::from(GUEST_HOME)
                             } else {
                                 format!("/home/{}", username)
                             },
            enabled:         true,
            hidden:          false,
            auto_login:      false,
            must_change:     false,
            steps,
            failed_attempts: 0,
            backoff_until:   0,
            created:         now_stamp(),
            last_login:      String::new(),
            caps_extra:      0,
            caps_removed:    0,
            admin_slot:      false,
            recovery_key:    false,
        }
    }

    pub fn has_secret(&self) -> bool {
        !self.steps.is_empty()
    }

    /// Does `secret` meet the first step (or is it empty, for an account
    /// with no steps)? Used where one current password is asked for
    /// (KEYS RELINK, the password change). Doesn't count as a sign-in.
    pub fn first_step_accepts(&self, secret: &str) -> bool {
        match self.steps.first() {
            None => secret.is_empty(),
            Some(step) => step.methods.iter().any(|m| m.verify(secret)),
        }
    }

    /// A sign-in: one secret per step, in order. Every step is checked
    /// (no stopping at the first wrong one) and a failure never says which.
    /// Failures in a row bring growing delays (POLICY); there is no
    /// permanent lock.
    pub fn authenticate(&mut self, secrets: &[&str]) -> Result<(), AuthError> {
        if self.role == UserRole::Root {
            return Err(AuthError::SystemAccount);
        }
        let now = now_ticks();
        if self.backoff_until > now {
            return Err(AuthError::Wait((self.backoff_until - now).div_ceil(1_000)));
        }
        let ok = if self.steps.is_empty() {
            // No password: whatever was typed, it opens.
            true
        } else {
            let mut all = secrets.len() == self.steps.len();
            for (i, step) in self.steps.iter().enumerate() {
                let secret = secrets.get(i).copied().unwrap_or("");
                let mut any = false;
                for m in &step.methods {
                    any |= m.verify(secret);
                }
                all &= any;
            }
            all
        };
        if ok {
            self.failed_attempts = 0;
            self.backoff_until = 0;
            // Turned off: said only once the secrets were right, so a wrong
            // guess doesn't learn that the account exists.
            if !self.enabled {
                return Err(AuthError::Disabled);
            }
            Ok(())
        } else {
            self.failed_attempts = self.failed_attempts.saturating_add(1);
            let delay = POLICY.lock().delay_secs(self.failed_attempts);
            if delay > 0 {
                self.backoff_until = now + delay * 1_000;
            }
            Err(AuthError::Incorrect)
        }
    }

    /// Put `method` in the first step in place of any of the same kind
    /// (password replaces password, PIN replaces PIN), making the step if
    /// there is none.
    fn set_first_step_method(&mut self, method: Method) {
        if self.steps.is_empty() {
            self.steps.push(Step::default());
        }
        let same = |m: &Method| core::mem::discriminant(m) == core::mem::discriminant(&method);
        let step = &mut self.steps[0];
        step.methods.retain(|m| !same(m));
        step.methods.insert(0, method);
    }

    /// Change your own password. `old_password` must meet the first step
    /// (empty for an account with no password).
    pub fn change_password(&mut self, old_password: &str, new_password: &str) -> Result<(), String> {
        validate_secret(SecretKind::Password, new_password)?;
        if !self.first_step_accepts(old_password) {
            return Err(String::from("Current password is incorrect"));
        }
        // The saved keys' master key is re-wrapped with the new password
        // FIRST. If that fails the password stays as it was: storing the
        // new hash anyway left the master key wrapped with the old password,
        // and the next sign-in then could not open the saved keys.
        if let Err(e) = crate::security::encryption::cen_change_password(&self.username, old_password, new_password) {
            crate::serial_println!("[CEN] {}: master key not re-wrapped: {}", self.username, e);
            return Err(String::from("Saved keys could not be re-locked with the new password; password not changed \
                                     (if sign-in said they weren't opened, run KEYS RELINK first)"));
        }
        self.set_first_step_method(Method::password(new_password));
        Ok(())
    }

    /// Set a new password without the old one (admin reset). Clears the
    /// back-off. The saved keys can't be carried over (until key slots,
    /// stage A2), so they are removed.
    pub fn reset_password(&mut self, new_password: &str) -> Result<(), String> {
        validate_secret(SecretKind::Password, new_password)?;
        self.set_first_step_method(Method::password(new_password));
        self.unlock();
        crate::security::encryption::cen_forget_user(&self.username);
        Ok(())
    }

    /// Clear the failed sign-ins and any delay (admin action).
    pub fn unlock(&mut self) {
        self.failed_attempts = 0;
        self.backoff_until = 0;
    }

    /// The flags field: e enabled, h hidden, a auto-login, m must change.
    fn flags(&self) -> String {
        let mut f = String::new();
        if self.enabled { f.push('e'); }
        if self.hidden { f.push('h'); }
        if self.auto_login { f.push('a'); }
        if self.must_change { f.push('m'); }
        f
    }

    /// One line of /etc/users2.vdb: tab-separated key=value. Steps are
    /// separated by `;`, a step's methods by `|`.
    pub fn to_line(&self) -> String {
        let steps: Vec<String> = self.steps.iter()
            .map(|s| s.methods.iter().map(Method::encode).collect::<Vec<_>>().join("|"))
            .collect();
        let mut keys = String::new();
        if self.admin_slot { keys.push('a'); }
        if self.recovery_key { keys.push('r'); }
        format!(
            "user={}\tuid={}\trole={}\tname={}\thome={}\tflags={}\tsteps={}\tfails={}\tcreated={}\tlast={}\tcaps+={:x}\tcaps-={:x}\tkeys={}",
            self.username, self.uid, self.role.name(), self.display_name, self.home_dir,
            self.flags(), steps.join(";"), self.failed_attempts, self.created, self.last_login,
            self.caps_extra, self.caps_removed, keys,
        )
    }

    /// Read a /etc/users2.vdb line. Unknown keys are ignored, so a newer
    /// file still loads.
    pub fn from_line(line: &str) -> Option<Self> {
        let mut u = User::new("", "", UserRole::Standard);
        u.created.clear();
        let mut have_name = false;
        for field in line.split('\t') {
            let Some((k, v)) = field.split_once('=') else { continue };
            match k {
                "user" => { u.username = String::from(v); have_name = !v.is_empty(); }
                "uid" => u.uid = v.parse().ok()?,
                "role" => u.role = UserRole::parse(v)?,
                "name" => u.display_name = clean_display_name(v),
                "home" => u.home_dir = String::from(v),
                "flags" => {
                    u.enabled = v.contains('e');
                    u.hidden = v.contains('h');
                    u.auto_login = v.contains('a');
                    u.must_change = v.contains('m');
                }
                "steps" => {
                    u.steps = v.split(';').filter(|s| !s.is_empty())
                        .map(|s| Step { methods: s.split('|').filter_map(Method::decode).collect() })
                        .filter(|s| !s.methods.is_empty())
                        .collect();
                }
                "fails" => u.failed_attempts = v.parse().unwrap_or(0),
                "created" => u.created = String::from(v),
                "last" => u.last_login = String::from(v),
                "caps+" => u.caps_extra = u64::from_str_radix(v, 16).unwrap_or(0),
                "caps-" => u.caps_removed = u64::from_str_radix(v, 16).unwrap_or(0),
                "keys" => {
                    u.admin_slot = v.contains('a');
                    u.recovery_key = v.contains('r');
                }
                _ => {}
            }
        }
        if !have_name {
            return None;
        }
        if u.display_name.is_empty() {
            u.display_name = u.username.clone();
        }
        Some(u)
    }

    /// Read a v1 /etc/users.vdb line: username:hash:home:role:locked:failed.
    /// The permanent lock is dropped (back-off replaces it).
    pub fn from_v1_line(line: &str) -> Option<Self> {
        let parts: Vec<&str> = line.splitn(6, ':').collect();
        if parts.len() < 6 || parts[0].is_empty() { return None; }
        let mut u = User::new(parts[0], "", UserRole::from_str(parts[3]));
        if !parts[1].is_empty() {
            u.steps.push(Step { methods: alloc::vec![Method::Password(String::from(parts[1]))] });
        }
        u.home_dir = String::from(parts[2]);
        u.failed_attempts = parts[5].parse().unwrap_or(0);
        u.created.clear();
        Some(u)
    }
}

// =====================================================================
// -----| User Database |-----
// =====================================================================

pub struct UserDatabase {
    pub users: Vec<User>,
    /// The first-boot wizard (no accounts yet) or the upgrade step (moved
    /// over from v1) hasn't run: stage A6 shows it. Saved in the file.
    pub setup_pending: bool,
}

impl UserDatabase {
    pub const fn new() -> Self {
        Self { users: Vec::new(), setup_pending: false }
    }

    /// Load /etc/users2.vdb; else move /etc/users.vdb (v1) over, leaving
    /// it as it was; else seed first-boot accounts. Either way root and
    /// guest are there afterwards.
    pub fn load(&mut self) {
        let vfs = &crate::fs::vfs::VFS;
        self.users.clear();
        if let Ok(bytes) = vfs.read_file(USERS2_VDB_PATH) {
            let text = core::str::from_utf8(&bytes).unwrap_or("");
            for line in text.lines() {
                let line = line.trim_end_matches('\r');
                if let Some(v) = line.strip_prefix("@setup=") {
                    self.setup_pending = v == "pending";
                    continue;
                }
                if line.is_empty() || line.starts_with('#') { continue; }
                match User::from_line(line) {
                    Some(u) => self.users.push(u),
                    None => println!("[USERS] {}: a line could not be read; skipped", USERS2_VDB_PATH),
                }
            }
            println!("[USERS] Loaded {} account(s) from {}", self.users.len(), USERS2_VDB_PATH);
        } else if let Ok(bytes) = vfs.read_file(USERS_VDB_PATH) {
            let text = core::str::from_utf8(&bytes).unwrap_or("");
            for line in text.lines() {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') { continue; }
                if let Some(u) = User::from_v1_line(line) {
                    self.users.push(u);
                }
            }
            // The upgrade step (choose the first admin, replace the default
            // passwords) runs once the wizard exists (A6).
            self.setup_pending = true;
            println!("[USERS] Moved {} account(s) from {} to {} (the old file is kept)",
                self.users.len(), USERS_VDB_PATH, USERS2_VDB_PATH);
        } else {
            // First boot. Until the first-boot wizard (A6) creates the first
            // account, the default admin keeps VEILOS usable.
            println!("[USERS] No user database: seeding the default admin until first-boot setup exists");
            self.users.push(User::new("admin", "veilos", UserRole::Admin));
            self.setup_pending = true;
        }
        self.ensure_builtin();
        self.save();
    }

    /// root (uid 0) and guest (uid 999) exist; everyone else has a uid.
    fn ensure_builtin(&mut self) {
        if !self.users.iter().any(|u| u.role == UserRole::Root) {
            let mut root = User::new("root", "", UserRole::Root);
            root.hidden = true;
            self.users.insert(0, root);
        }
        if !self.users.iter().any(|u| u.username.eq_ignore_ascii_case("guest")) {
            self.users.push(User::new("guest", "", UserRole::Guest));
        }
        for u in self.users.iter_mut() {
            if u.role == UserRole::Root {
                u.uid = 0;
                u.hidden = true;
            } else if u.username.eq_ignore_ascii_case("guest") {
                u.uid = GUEST_UID;
                u.role = UserRole::Guest;
                u.steps.clear();
                u.home_dir = String::from(GUEST_HOME);
            }
        }
        let mut next = self.next_uid();
        for u in self.users.iter_mut() {
            if u.uid == 0 && u.role != UserRole::Root {
                u.uid = next;
                next += 1;
            }
        }
    }

    fn next_uid(&self) -> u32 {
        self.users.iter().map(|u| u.uid).filter(|&id| id >= FIRST_UID).max().map_or(FIRST_UID, |m| m + 1)
    }

    /// Persist to /etc/users2.vdb.
    pub fn save(&self) {
        let vfs = &crate::fs::vfs::VFS;
        let mut text = String::from("# VEILOS users v2 - do not edit by hand\n");
        text.push_str(if self.setup_pending { "@setup=pending\n" } else { "@setup=done\n" });
        for u in &self.users {
            text.push_str(&u.to_line());
            text.push('\n');
        }
        if vfs.write_file(USERS2_VDB_PATH, text.as_bytes()).is_err() {
            println!("[USERS] {} could not be written", USERS2_VDB_PATH);
        }
    }

    /// Usernames are matched ignoring case: they are unique ignoring case
    /// (`add`), and accounts made before names were checked may be in
    /// capitals (the terminal used to upper-case USERADD's arguments).
    pub fn find(&self, username: &str) -> Option<&User> {
        self.users.iter().find(|u| u.username.eq_ignore_ascii_case(username))
    }

    pub fn find_mut(&mut self, username: &str) -> Option<&mut User> {
        self.users.iter_mut().find(|u| u.username.eq_ignore_ascii_case(username))
    }

    /// A new account. The name must pass `validate_username` and be unique
    /// ignoring case; the account must meet its role's step rule.
    pub fn add(&mut self, mut user: User) -> Result<(), String> {
        validate_username(&user.username)?;
        if self.find(&user.username).is_some() {
            return Err(String::from("Username already exists"));
        }
        if user.steps.len() < POLICY.lock().min_steps_for(user.role) {
            return Err(format!("{} accounts need a password", user.role.name()));
        }
        user.uid = self.next_uid();
        println!("[USERS] Created account '{}' (uid {}, role {})", user.username, user.uid, user.role.name());
        self.users.push(user);
        self.save();
        Ok(())
    }

    pub fn remove(&mut self, username: &str) -> Result<(), &'static str> {
        let pos = self.users.iter().position(|u| u.username.eq_ignore_ascii_case(username))
            .ok_or("User not found")?;
        match self.users[pos].role {
            UserRole::Root => return Err("Cannot delete the root account"),
            UserRole::Guest if self.users[pos].uid == GUEST_UID => return Err("The guest account can't be deleted; turn it off instead"),
            _ => {}
        }
        let name = self.users[pos].username.clone();
        self.users.remove(pos);
        self.save();
        println!("[USERS] Deleted account '{}'", name);
        Ok(())
    }

    /// Admin accounts other than `except`, optionally only enabled ones.
    pub fn admins_other_than(&self, except: &str, enabled_only: bool) -> usize {
        self.users.iter()
            .filter(|u| u.role == UserRole::Admin && !u.username.eq_ignore_ascii_case(except))
            .filter(|u| !enabled_only || u.enabled)
            .count()
    }

    /// The account that signs in at boot without asking, if any: enabled,
    /// auto-login on, no sign-in steps, and its role allows that.
    pub fn auto_login_user(&self) -> Option<String> {
        let policy = *POLICY.lock();
        self.users.iter()
            .find(|u| u.auto_login && u.enabled && !u.has_secret() && u.role != UserRole::Root
                && policy.min_steps_for(u.role) == 0)
            .map(|u| u.username.clone())
    }

    /// Turn auto-login on for `username` (off for everyone else), or off.
    /// Only for an account with no password, whose role allows none.
    pub fn set_auto_login(&mut self, username: &str, on: bool) -> Result<(), String> {
        let policy = *POLICY.lock();
        let u = self.find(username).ok_or_else(|| String::from("user not found"))?;
        if on {
            if u.has_secret() {
                return Err(String::from("auto-login is only for an account with no password"));
            }
            if policy.min_steps_for(u.role) > 0 {
                return Err(format!("{} accounts need a password, so they can't sign in automatically", u.role.name()));
            }
            if u.role == UserRole::Root || u.role == UserRole::Guest {
                return Err(String::from("this account can't sign in automatically"));
            }
        }
        let target = u.username.clone();
        for x in self.users.iter_mut() {
            x.auto_login = on && x.username == target;
        }
        self.save();
        Ok(())
    }

    /// Does `user` meet its role's step rule? (Not: set up at next sign-in.)
    pub fn meets_role_rule(&self, user: &User) -> bool {
        user.steps.len() >= POLICY.lock().min_steps_for(user.role)
    }

    /// The user's capabilities: the role's defaults, plus extra, minus
    /// removed. Nobody found: Guest.
    pub fn capability_set_for(&self, username: &str) -> CapabilitySet {
        let mask = match self.find(username) {
            Some(u) => (u.role.default_caps() | u.caps_extra) & !u.caps_removed,
            None => UserRole::Guest.default_caps(),
        };
        CapabilitySet::new(mask)
    }
}

pub static USER_DB: Mutex<UserDatabase> = Mutex::new(UserDatabase::new());

/// Called from kernel_main after the heap and VFS are ready.
pub fn init() {
    USER_DB.lock().load();
    println!("[USERS] User account system ready.");
}

// =====================================================================
// -----| Authorization |-----
// =====================================================================

/// Something only some accounts may do. Checked by `authorize`, the one
/// place these rules live (ACCOUNTS_DESIGN.md, section 1).
#[derive(Clone, Copy, Debug)]
pub enum Action<'a> {
    /// Create an account with this role.
    CreateUser(UserRole),
    /// Delete this account.
    DeleteUser(&'a str),
    /// Set this account's password without knowing the old one.
    ResetPassword(&'a str),
    /// Clear this account's failed sign-ins and delay.
    UnlockUser(&'a str),
    /// Give this account this role.
    SetRole(&'a str, UserRole),
    /// Turn this account on or off.
    SetEnabled(&'a str, bool),
    /// Show or hide this account on the login screen.
    SetHidden(&'a str),
    /// Change this account's display name (the holder or an admin).
    SetDisplayName(&'a str),
    /// Turn auto-login on or off for this account (the holder or an admin).
    SetAutoLogin(&'a str),
    /// See every account (USERS); without it, only your own.
    ListAllUsers,
    /// A system task with admin rights: drivers, installing, updating, the
    /// crash test (terminal DRIVER, INSTALL, UPDATE, CRASH).
    AdminTask,
    /// Switch to Performance Mode, which relaxes protections. (Switching
    /// back to Security Mode only tightens them, so anyone may.)
    EnterPerformanceMode,
}

/// May the signed-in user do `action`? Ok, or a reason to show them.
///
///   user.manage (the Admin role; root has everything): create accounts
///     (root accounts only by root); delete, reset, unlock, change and
///     hide any account but root's; not delete, disable or demote
///     themselves; never remove, disable or demote the last admin.
///   Everyone: their own display name and auto-login.
///   Performance Mode: policy.switch.
///
/// Nobody signed in: denied. Commands from other machines on the mesh
/// never reach here (terminal::execute_remote_command refuses them).
///
/// LOCK ORDER: takes SESSION, releases it, then takes USER_DB. Callers must
/// hold neither.
pub fn authorize(action: Action) -> Result<(), &'static str> {
    let (me, logged_in) = {
        let s = SESSION.lock();
        (s.username.clone(), s.logged_in)
    };
    if !logged_in || me.is_empty() {
        return Err("nobody is signed in");
    }
    let db = USER_DB.lock();
    let caps = db.capability_set_for(&me);
    let my_role = db.find(&me).map(|u| u.role).unwrap_or(UserRole::Guest);
    let is_root = my_role == UserRole::Root;
    let manager = caps.has(Capability::UserManage);

    // Actions the holder of an account may do to it themselves.
    let own = |target: &str| target.eq_ignore_ascii_case(&me);

    match action {
        Action::EnterPerformanceMode => {
            return if caps.has(Capability::PolicySwitch) {
                Ok(())
            } else {
                Err("switching to Performance Mode needs an admin account (policy.switch)")
            };
        }
        Action::SetDisplayName(t) | Action::SetAutoLogin(t) if own(t) => return Ok(()),
        _ => {}
    }

    if !manager {
        return Err("permission denied: only an admin can do that");
    }

    match action {
        Action::AdminTask | Action::ListAllUsers => Ok(()),
        Action::CreateUser(role) => {
            if role == UserRole::Root && !is_root {
                return Err("only root can create a root account");
            }
            Ok(())
        }
        Action::DeleteUser(target) | Action::ResetPassword(target) | Action::UnlockUser(target)
        | Action::SetRole(target, _) | Action::SetEnabled(target, _) | Action::SetHidden(target)
        | Action::SetDisplayName(target) | Action::SetAutoLogin(target) => {
            let t = db.find(target).ok_or("user not found")?;
            if t.role == UserRole::Root && !is_root {
                return Err("only root can change the root account");
            }
            let is_self = own(&t.username);
            let last_admin = t.role == UserRole::Admin && db.admins_other_than(&t.username, true) == 0;
            match action {
                Action::DeleteUser(_) => {
                    if is_self {
                        return Err("you can't delete the account you are signed in as");
                    }
                    if t.role == UserRole::Admin && db.admins_other_than(&t.username, false) == 0 {
                        return Err("you can't delete the last admin account");
                    }
                }
                Action::ResetPassword(_) if is_self => {
                    return Err("to change your own password: PASSWD");
                }
                Action::SetRole(_, role) => {
                    if is_self {
                        return Err("you can't change your own role");
                    }
                    if role == UserRole::Root && !is_root {
                        return Err("only root can give the root role");
                    }
                    if role != UserRole::Admin && last_admin {
                        return Err("that is the last admin account");
                    }
                    if t.uid == GUEST_UID {
                        return Err("the guest account's role can't change");
                    }
                }
                Action::SetEnabled(_, false) => {
                    if is_self {
                        return Err("you can't turn off the account you are signed in as");
                    }
                    if last_admin {
                        return Err("that is the last admin account");
                    }
                }
                _ => {}
            }
            Ok(())
        }
        Action::EnterPerformanceMode => unreachable!(),
    }
}

// =====================================================================
// -----| Active Session |-----
// =====================================================================

pub struct Session {
    /// Currently logged-in username, or empty if at the login screen.
    pub username:    String,
    /// Tick count at login time (for session duration).
    pub login_tick:  u64,
    /// Tick count of the last keyboard or mouse input.
    pub last_active: u64,
    /// Idle timeout in ms before the lock screen fires.
    pub idle_timeout_ms: u64,
    /// True when the session is locked.
    pub locked:      bool,
    /// True when a user is logged in (false = at login screen).
    pub logged_in:   bool,
}

impl Session {
    pub const fn new() -> Self {
        Self {
            username:        String::new(),
            login_tick:      0,
            last_active:     0,
            idle_timeout_ms: DEFAULT_IDLE_TIMEOUT_MS,
            locked:          false,
            logged_in:       false,
        }
    }

    /// Record input activity so the idle timer resets.
    pub fn touch(&mut self) {
        self.last_active = now_ticks();
    }

    /// Called every vsync tick. Returns true if the lock screen should
    /// be shown (idle timeout elapsed and not already locked).
    pub fn tick(&mut self) -> bool {
        if !self.logged_in || self.locked { return false; }
        let now = now_ticks();
        // Activity is the latest of `touch()` (sign-in, characters typed)
        // and LAST_INPUT_MS (every key event and mouse movement or click,
        // from the drivers). It used to be keyboard characters only, so
        // using just the mouse locked the screen after the timeout.
        let last = self.last_active.max(LAST_INPUT_MS.load(Ordering::Relaxed));
        let idle_ms = now.saturating_sub(last);
        if idle_ms >= self.idle_timeout_ms {
            self.lock();
            return true;
        }
        false
    }

    /// Start a session for `username` (already signed in: the caller
    /// checked the secrets). Pass the account's stored spelling.
    ///
    /// LOCK ORDER: the caller must not hold USER_DB.
    pub fn login(&mut self, username: &str) {
        let now = now_ticks();
        self.username    = String::from(username);
        self.login_tick  = now;
        self.last_active = now;
        self.locked      = false;
        self.logged_in   = true;
        println!("[SESSION] Logged in as '{}'", username);

        let (caps, home, guest) = {
            let mut db = USER_DB.lock();
            let caps = db.capability_set_for(username);
            let (home, guest) = match db.find_mut(username) {
                Some(u) => {
                    u.last_login = now_stamp();
                    (u.home_dir.clone(), u.role == UserRole::Guest)
                }
                None => (format!("/home/{}", username), false),
            };
            db.save();
            (caps, home, guest)
        };

        // Remember who signed in last (the login screen's default); not the
        // guest, whose session leaves nothing behind.
        if !guest {
            let _ = crate::fs::vfs::VFS.write_file(LAST_USER_PATH, username.as_bytes());
        }

        // Apply capability profile for this user.
        if let Some(proc) = crate::sched::process::PROCESSOR.lock().get_active() {
            *proc.capabilities.lock() = caps;
        }

        // Create the home directory if it doesn't exist.
        if guest {
            let _ = crate::fs::vfs::VFS.create_dir("/tmp");
        }
        let _ = crate::fs::vfs::VFS.create_dir(&home);
    }

    pub fn logout(&mut self) {
        println!("[SESSION] '{}' logged out.", self.username);
        crate::security::encryption::cen_lock_user();
        self.username   = String::new();
        self.logged_in  = false;
        self.locked     = false;
    }

    pub fn lock(&mut self) {
        if self.logged_in && !self.locked {
            self.locked = true;
            println!("[SESSION] Screen locked.");
        }
    }

    /// Verify the signed-in user's secret(s) at the lock screen: the same
    /// steps and back-off as signing in. Caller must NOT hold USER_DB.
    pub fn verify_unlock(&self, secrets: &[&str]) -> Result<(), AuthError> {
        if self.username.is_empty() { return Err(AuthError::Incorrect); }
        let mut db = USER_DB.lock();
        let result = match db.find_mut(&self.username) {
            Some(user) => user.authenticate(secrets),
            None => Err(AuthError::Incorrect),
        };
        db.save();
        result
    }

    /// Mark the session as unlocked.  Must be called AFTER verify_unlock
    /// succeeds and with SESSION already locked.
    pub fn complete_unlock(&mut self) {
        self.locked = false;
        self.touch();
        println!("[SESSION] Screen unlocked.");
    }

    pub fn duration_secs(&self) -> u64 {
        now_ticks().saturating_sub(self.login_tick) / 1_000
    }
}

pub static SESSION: Mutex<Session> = Mutex::new(Session::new());
