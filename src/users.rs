// =====================================================================
// -----| VEILOS User Account System (Phase 18) |-----
//
//  User struct           — username, hashed password, home dir, role, caps
//  Password hashing      — PBKDF built from SHA-256 (sha2 crate, no_std)
//                          100 000 rounds of SHA-256(salt ‖ password ‖ prev)
//  Persistence           — /etc/users.vdb (line-oriented text, one user/line)
//  Session               — tracks active user, login time, idle time
//  Lock screen state     — idle timeout, manual lock, unlock flow
//  Roles                 — Root / Admin / Standard / Guest
//  Capability profiles   — each role maps to a CapabilitySet mask
// =====================================================================

#![allow(dead_code)]

use alloc::vec::Vec;
use alloc::string::String;
use spin::Mutex;
use core::sync::atomic::Ordering;
use sha2::{Sha256, Digest};
use crate::println;
use crate::capability::{Capability, CapabilitySet};

// =====================================================================
// -----| Constants |-----
// =====================================================================

/// VFS path where the user database is persisted.
pub const USERS_VDB_PATH: &str = "/etc/users.vdb";
/// VFS path remembering the last logged-in username.
pub const LAST_USER_PATH: &str = "/etc/last_user";

/// Lock-screen idle timeout in milliseconds (default 5 minutes).
pub const DEFAULT_IDLE_TIMEOUT_MS: u64 = 5 * 60 * 1_000;

/// Number of failed login attempts before the account is locked out.
pub const MAX_FAILED_ATTEMPTS: u32 = 5;

/// PBKDF iteration count.  100 k rounds of SHA-256 is enough for a
/// no-std kernel implementation; replace with Argon2id when a suitable
/// no_std crate is available.
const PBKDF_ROUNDS: u32 = 100_000;

/// Salt length in bytes embedded in each password hash field.
const SALT_LEN: usize = 16;

// =====================================================================
// -----| User Role |-----
// =====================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserRole {
    /// Full kernel access — only the seed account created at first boot.
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

    pub fn from_str(s: &str) -> Self {
        match s {
            "root"  => UserRole::Root,
            "admin" => UserRole::Admin,
            "guest" => UserRole::Guest,
            _       => UserRole::Standard,
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

/// Hash a password with a random-looking salt derived from the username
/// and a caller-supplied 16-byte random salt.
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
        out.push_str(&alloc::format!("{:02x}", b));
    }
    out.push('$');
    for b in state.iter() {
        out.push_str(&alloc::format!("{:02x}", b));
    }
    out
}

/// Verify a plaintext password against a stored hash field.
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

/// Derive a deterministic "salt" from a username + a 32-bit seed.
/// Used when creating accounts so every install gets unique salts
/// even without a true hardware RNG.
pub fn derive_salt(username: &str, seed: u32) -> [u8; SALT_LEN] {
    let mut h = Sha256::new();
    h.update(username.as_bytes());
    h.update(&seed.to_le_bytes());
    h.update(b"veilos-salt-kdf-v1");
    let result = h.finalize();
    let mut salt = [0u8; SALT_LEN];
    salt.copy_from_slice(&result[..SALT_LEN]);
    salt
}

// =====================================================================
// -----| User struct |-----
// =====================================================================

#[derive(Clone)]
pub struct User {
    /// Login name (ASCII, no spaces, max 32 chars).
    pub username:        String,
    /// Stored password field: salt_hex + "$" + hash_hex.
    /// Empty string means the account has no password (root at first boot).
    pub password_hash:   String,
    /// Absolute VFS path to the user's home directory.
    pub home_dir:        String,
    /// Role determines the default capability profile.
    pub role:            UserRole,
    /// Whether this account is locked (too many failed attempts).
    pub locked:          bool,
    /// Number of consecutive failed login attempts.
    pub failed_attempts: u32,
}

impl User {
    pub fn new(username: &str, password: &str, role: UserRole) -> Self {
        // Derive a salt from username + a boot-time seed.
        let seed = crate::interrupts::TICKS.load(Ordering::Relaxed) as u32;
        let salt = derive_salt(username, seed);
        let hash = if password.is_empty() {
            String::new()
        } else {
            hash_password(password, &salt)
        };
        Self {
            username:        String::from(username),
            password_hash:   hash,
            home_dir:        if role == UserRole::Guest {
                                 String::from("/tmp/guest")
                             } else {
                                 alloc::format!("/home/{}", username)
                             },
            role,
            locked:          false,
            failed_attempts: 0,
        }
    }

    /// Verify a login attempt.  Returns Ok(()) on success or Err with a
    /// human-readable reason on failure.  Updates failed_attempts.
    pub fn authenticate(&mut self, password: &str) -> Result<(), &'static str> {
        if self.locked {
            return Err("Account locked — too many failed attempts");
        }
        // Root account with no password set: allow empty password.
        let ok = if self.password_hash.is_empty() {
            password.is_empty()
        } else {
            verify_password(password, &self.password_hash)
        };
        if ok {
            self.failed_attempts = 0;
            Ok(())
        } else {
            self.failed_attempts += 1;
            if self.failed_attempts >= MAX_FAILED_ATTEMPTS {
                self.locked = true;
                Err("Account locked after too many failed attempts")
            } else {
                Err("Incorrect password")
            }
        }
    }

    /// Change the password.  `old_password` must match the current hash
    /// (or be empty if this is an admin reset).
    pub fn change_password(&mut self, old_password: &str, new_password: &str)
        -> Result<(), &'static str>
    {
        if !self.password_hash.is_empty()
            && !verify_password(old_password, &self.password_hash)
        {
            return Err("Current password is incorrect");
        }
        let seed = crate::interrupts::TICKS.load(Ordering::Relaxed) as u32;
        let salt = derive_salt(&self.username, seed);
        self.password_hash = hash_password(new_password, &salt);
        Ok(())
    }

    /// Force-set a new password without verifying the old one (admin reset).
    pub fn reset_password(&mut self, new_password: &str) {
        let seed = crate::interrupts::TICKS.load(Ordering::Relaxed) as u32;
        let salt = derive_salt(&self.username, seed);
        self.password_hash = hash_password(new_password, &salt);
        self.locked = false;
        self.failed_attempts = 0;
    }

    /// Unlock a locked account (admin action).
    pub fn unlock(&mut self) {
        self.locked = false;
        self.failed_attempts = 0;
    }

    /// Serialise to a single line for /etc/users.vdb.
    /// Format: username:hash:home:role:locked:failed
    pub fn to_vdb_line(&self) -> String {
        alloc::format!("{}:{}:{}:{}:{}:{}",
            self.username,
            self.password_hash,
            self.home_dir,
            self.role.name(),
            if self.locked { "1" } else { "0" },
            self.failed_attempts,
        )
    }

    /// Deserialise from a single /etc/users.vdb line.
    pub fn from_vdb_line(line: &str) -> Option<Self> {
        let parts: Vec<&str> = line.splitn(6, ':').collect();
        if parts.len() < 6 { return None; }
        Some(Self {
            username:        String::from(parts[0]),
            password_hash:   String::from(parts[1]),
            home_dir:        String::from(parts[2]),
            role:            UserRole::from_str(parts[3]),
            locked:          parts[4] == "1",
            failed_attempts: parts[5].parse().unwrap_or(0),
        })
    }
}

// =====================================================================
// -----| User Database |-----
// =====================================================================

pub struct UserDatabase {
    pub users: Vec<User>,
}

impl UserDatabase {
    pub const fn new() -> Self {
        Self { users: Vec::new() }
    }

    /// Load users from /etc/users.vdb.  If the file is missing, seed a
    /// default root account with no password so first boot works.
    pub fn load(&mut self) {
        let vfs = &crate::vfs::VFS;
        match vfs.read_file(USERS_VDB_PATH) {
            Ok(bytes) => {
                let text = core::str::from_utf8(&bytes).unwrap_or("");
                for line in text.lines() {
                    let line = line.trim();
                    if line.is_empty() || line.starts_with('#') { continue; }
                    if let Some(u) = User::from_vdb_line(line) {
                        self.users.push(u);
                    }
                }
                println!("[USERS] Loaded {} user(s) from {}", self.users.len(), USERS_VDB_PATH);
            }
            Err(_) => {
                // First boot: create root with no password and a standard user.
                println!("[USERS] {} not found — seeding default accounts", USERS_VDB_PATH);
                self.users.push(User::new("root",  "",         UserRole::Root));
                self.users.push(User::new("admin", "veilos",   UserRole::Admin));
                let _ = self.save_unlocked(&vfs);
            }
        }
    }

    /// Persist the database back to /etc/users.vdb.
    pub fn save(&self) {
        let vfs = &crate::vfs::VFS;
        let _ = self.save_unlocked(&vfs);
    }

    fn save_unlocked(&self, vfs: &crate::vfs::VfsManager) -> Result<(), crate::vfs::VfsError> {
        let mut text = String::from("# VEILOS user database — do not edit manually\n");
        for u in &self.users {
            text.push_str(&u.to_vdb_line());
            text.push('\n');
        }
        vfs.write_file(USERS_VDB_PATH, text.as_bytes())
    }

    pub fn find(&self, username: &str) -> Option<&User> {
        self.users.iter().find(|u| u.username == username)
    }

    pub fn find_mut(&mut self, username: &str) -> Option<&mut User> {
        self.users.iter_mut().find(|u| u.username == username)
    }

    pub fn add(&mut self, user: User) -> Result<(), &'static str> {
        if self.find(&user.username).is_some() {
            return Err("Username already exists");
        }
        println!("[USERS] Created account '{}' (role: {})", user.username, user.role.name());
        self.users.push(user);
        self.save();
        Ok(())
    }

    pub fn remove(&mut self, username: &str) -> Result<(), &'static str> {
        let pos = self.users.iter().position(|u| u.username == username)
            .ok_or("User not found")?;
        if self.users[pos].role == UserRole::Root {
            return Err("Cannot delete the root account");
        }
        self.users.remove(pos);
        self.save();
        println!("[USERS] Deleted account '{}'", username);
        Ok(())
    }

    /// Build a CapabilitySet for the given user (role defaults + any overrides).
    pub fn capability_set_for(&self, username: &str) -> CapabilitySet {
        let role = self.find(username).map(|u| u.role).unwrap_or(UserRole::Guest);
        let mask = role.default_caps();
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
        self.last_active = crate::interrupts::TICKS.load(Ordering::Relaxed);
    }

    /// Called every vsync tick. Returns true if the lock screen should
    /// be shown (idle timeout elapsed and not already locked).
    pub fn tick(&mut self) -> bool {
        if !self.logged_in || self.locked { return false; }
        let now = crate::interrupts::TICKS.load(Ordering::Relaxed);
        let idle_ms = now.saturating_sub(self.last_active);
        if idle_ms >= self.idle_timeout_ms {
            self.lock();
            return true;
        }
        false
    }

    pub fn login(&mut self, username: &str) {
        let now = crate::interrupts::TICKS.load(Ordering::Relaxed);
        self.username    = String::from(username);
        self.login_tick  = now;
        self.last_active = now;
        self.locked      = false;
        self.logged_in   = true;
        println!("[SESSION] Logged in as '{}'", username);

        // Save last-user preference.
        let _ = crate::vfs::VFS.write_file(LAST_USER_PATH, username.as_bytes());

        // Apply capability profile for this user.
        let caps = USER_DB.lock().capability_set_for(username);
        if let Some(proc) = crate::process::PROCESSOR.lock().get_active() {
            *proc.capabilities.lock() = caps;
        }

        // Create home directory if it doesn't exist.
        let home = alloc::format!("/home/{}/.keep", username);
        let _ = crate::vfs::VFS.write_file(&home, b"");
    }

    pub fn logout(&mut self) {
        println!("[SESSION] '{}' logged out.", self.username);
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

    pub fn unlock(&mut self, _password: &str) -> Result<(), &'static str> {
        // LOCK ORDER: USER_DB must be acquired before SESSION to match every
        // other call-site (attempt_login, Settings Accounts tab).  We cannot
        // acquire USER_DB here because the caller already holds SESSION via
        // &mut self.  Instead we take the username we need, drop &mut self
        // conceptually by returning early on error, and let the caller
        // re-acquire in the correct order.
        //
        // The safe pattern: verify the password through a free function so
        // no Mutex is held across the other's acquisition.
        if self.username.is_empty() { return Err("No active session"); }
        Ok(())   // placeholder — see attempt_unlock in desktop.rs for the
                 // full two-step flow that replaces the old single-call path.
    }

    /// Verify `password` for the currently-logged-in user.
    /// Caller must NOT hold the SESSION lock.
    /// Returns Ok(()) on success so the caller can then re-lock SESSION and
    /// clear the locked flag.
    pub fn verify_unlock_password(&self, password: &str) -> Result<(), &'static str> {
        if self.username.is_empty() { return Err("No active session"); }
        let mut db = USER_DB.lock();
        let user = db.find_mut(&self.username).ok_or("User not found")?;
        user.authenticate(password)
    }

    /// Mark the session as unlocked.  Must be called AFTER verify_unlock_password
    /// succeeds and with SESSION already locked.
    pub fn complete_unlock(&mut self) {
        self.locked = false;
        self.touch();
        println!("[SESSION] Screen unlocked.");
    }

    pub fn duration_secs(&self) -> u64 {
        let now = crate::interrupts::TICKS.load(Ordering::Relaxed);
        now.saturating_sub(self.login_tick) / 1_000
    }
}

pub static SESSION: Mutex<Session> = Mutex::new(Session::new());
