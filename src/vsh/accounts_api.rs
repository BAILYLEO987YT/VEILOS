// -----| VSH: accounts, sign-in policy and file owners (ACCOUNTS_DESIGN.md, section 9) |-----
//
// What scripts may ask about accounts. Every call acts with the signed-in
// user's rights, checked by users::authorize and security::access exactly as
// the terminal's commands are: a script can do what its user could. Where a
// script comes from is protected by file ownership (A3): /config.vsh and
// /SHELL.VSH are the system's, a user's own script is in their home.
//
// Read:   session_user() session_role() session_is_admin() idle_ms()
//         users() policy() file_owner(path)
// Change: policy_set(name, value) my_lock_timeout(seconds)
//         user_role(name, role) user_enable(name, bool) user_hide(name, bool)
//         user_display(name, text) user_autologin(name, bool)
//         user_unlock(name) user_delete(name)
//         file_set_owner(path, owner) file_grant(path, user, "r"|"rw"|"none")
// Secrets: request_password_change() user_create_dialog()
//         open a terminal whose hidden prompts ask for them: a script never
//         sees a password.
//
// Changes return true, or false with the reason shown as a notification
// (a refusal is not a script error). Bad arguments are script errors.

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::security::users::{self, Action, PolicyValue, UserRole};
use crate::vsh::vsh_lang::{CallArg, VValue};

fn arg_str<'a>(args: &'a [CallArg], i: usize, f: &str) -> Result<&'a str, String> {
    match args.get(i).map(|a| &a.value) {
        Some(VValue::String(s)) => Ok(s),
        Some(other) => Err(format!("{}: argument {} must be a string, got {}", f, i + 1, other.type_name())),
        None => Err(format!("{}: missing argument {}", f, i + 1)),
    }
}

fn arg_bool(args: &[CallArg], i: usize, f: &str) -> Result<bool, String> {
    match args.get(i).map(|a| &a.value) {
        Some(VValue::Boolean(b)) => Ok(*b),
        Some(other) => Err(format!("{}: argument {} must be true or false, got {}", f, i + 1, other.type_name())),
        None => Err(format!("{}: missing argument {}", f, i + 1)),
    }
}

fn arg_int(args: &[CallArg], i: usize, f: &str) -> Result<i64, String> {
    match args.get(i).map(|a| &a.value) {
        Some(VValue::Integer(n)) | Some(VValue::IndexVar(n)) => Ok(*n),
        Some(other) => Err(format!("{}: argument {} must be a whole number, got {}", f, i + 1, other.type_name())),
        None => Err(format!("{}: missing argument {}", f, i + 1)),
    }
}

/// A change's outcome: true, or false and the reason as a notification.
fn outcome(r: Result<String, String>) -> Result<VValue, String> {
    match r {
        Ok(msg) => {
            crate::serial_println!("[VSH] {}", msg);
            Ok(VValue::Boolean(true))
        }
        Err(e) => {
            crate::gui::desktop::notify(crate::gui::desktop::NotifCategory::Warning, "ACCOUNTS", &e);
            Ok(VValue::Boolean(false))
        }
    }
}

pub fn policy_value(v: &VValue) -> Option<PolicyValue> {
    match v {
        VValue::Integer(i) | VValue::IndexVar(i) => Some(PolicyValue::Int(*i)),
        VValue::Boolean(b) => Some(PolicyValue::Bool(*b)),
        VValue::String(s) => Some(PolicyValue::Text(s.clone())),
        _ => None,
    }
}

fn to_vvalue(v: PolicyValue) -> VValue {
    match v {
        PolicyValue::Int(i) => VValue::Integer(i),
        PolicyValue::Bool(b) => VValue::Boolean(b),
        PolicyValue::Text(t) => VValue::String(t),
    }
}

/// `set NAME = value` for a policy name (vscript.rs apply_system_setting):
/// None if `name` isn't one. Refusals are script errors here.
pub fn set_policy_setting(name: &str, v: &VValue) -> Option<Result<(), String>> {
    if !users::POLICY_NAMES.contains(&name) {
        return None;
    }
    Some(match policy_value(v) {
        Some(pv) => users::policy_set(name, pv).map(|msg| crate::serial_println!("[VSH] {}", msg)),
        None => Err(format!("{} expects a number, true/false or a string", name)),
    })
}

/// The host functions this module answers. None: not one of them.
pub fn host_call(name: &str, args: &[CallArg]) -> Option<Result<VValue, String>> {
    Some(match name {
        "session_user" => Ok(VValue::String(users::SESSION.lock().username.clone())),
        "session_role" => {
            let me = users::SESSION.lock().username.clone();
            Ok(VValue::String(users::USER_DB.lock().find(&me).map_or("", |u| u.role.name()).to_string()))
        }
        "session_is_admin" => Ok(VValue::Boolean(users::authorize(Action::ListAllUsers).is_ok())),
        "idle_ms" => {
            let now = crate::arch::interrupts::TICKS.load(core::sync::atomic::Ordering::Relaxed);
            let last = users::SESSION.lock().last_active.max(users::LAST_INPUT_MS.load(core::sync::atomic::Ordering::Relaxed));
            Ok(VValue::Integer(now.saturating_sub(last) as i64))
        }
        "users" => {
            // Admins see every account; anyone else only their own.
            let me = users::SESSION.lock().username.clone();
            let all = users::authorize(Action::ListAllUsers).is_ok();
            let db = users::USER_DB.lock();
            let list = db.users.iter()
                .filter(|u| u.role != UserRole::Root)
                .filter(|u| all || u.username.eq_ignore_ascii_case(&me))
                .map(|u| {
                    let mut o = BTreeMap::new();
                    o.insert(String::from("name"), VValue::String(u.username.clone()));
                    o.insert(String::from("display"), VValue::String(u.display_name.clone()));
                    o.insert(String::from("role"), VValue::String(String::from(u.role.name())));
                    o.insert(String::from("uid"), VValue::Integer(u.uid as i64));
                    o.insert(String::from("enabled"), VValue::Boolean(u.enabled));
                    o.insert(String::from("hidden"), VValue::Boolean(u.hidden));
                    o.insert(String::from("auto_login"), VValue::Boolean(u.auto_login));
                    o.insert(String::from("has_password"), VValue::Boolean(u.has_secret()));
                    o.insert(String::from("lock_timeout"), VValue::Integer(u.idle_secs as i64));
                    VValue::Object(o)
                })
                .collect();
            Ok(VValue::Array(list))
        }
        "policy" => {
            let mut o: BTreeMap<String, VValue> = users::policy_values().into_iter()
                .map(|(k, v)| (String::from(k), to_vvalue(v)))
                .collect();
            let me = users::SESSION.lock().username.clone();
            let mine = users::USER_DB.lock().find(&me).map_or(0, |u| u.idle_secs);
            o.insert(String::from("MY_LOCK_TIMEOUT"), VValue::Integer(mine as i64));
            Ok(VValue::Object(o))
        }
        "policy_set" => {
            let r = (|| {
                let n = arg_str(args, 0, name)?;
                let v = args.get(1).and_then(|a| policy_value(&a.value))
                    .ok_or_else(|| format!("{}: argument 2 must be a number, true/false or a string", name))?;
                Ok::<_, String>((String::from(n), v))
            })();
            match r {
                Ok((n, v)) => {
                    let result = users::policy_set(&n, v);
                    if let Ok(msg) = &result {
                        crate::security::access::seclog(&format!("policy: {}", msg));
                    }
                    outcome(result)
                }
                Err(e) => Err(e),
            }
        }
        "my_lock_timeout" => match arg_int(args, 0, name) {
            Ok(n) if n >= 0 => outcome(users::my_lock_timeout(n as u64)),
            Ok(_) => Err(format!("{}: seconds can't be negative", name)),
            Err(e) => Err(e),
        },
        "user_role" => match (arg_str(args, 0, name), arg_str(args, 1, name)) {
            (Ok(u), Ok(r)) => outcome(crate::task::terminal::account_modify(&[u, "ROLE", r])),
            (Err(e), _) | (_, Err(e)) => Err(e),
        },
        "user_enable" => match (arg_str(args, 0, name), arg_bool(args, 1, name)) {
            (Ok(u), Ok(on)) => outcome(crate::task::terminal::account_modify(&[u, if on { "ENABLE" } else { "DISABLE" }])),
            (Err(e), _) | (_, Err(e)) => Err(e),
        },
        "user_hide" => match (arg_str(args, 0, name), arg_bool(args, 1, name)) {
            (Ok(u), Ok(hide)) => outcome(crate::task::terminal::account_modify(&[u, if hide { "HIDE" } else { "SHOW" }])),
            (Err(e), _) | (_, Err(e)) => Err(e),
        },
        "user_display" => match (arg_str(args, 0, name), arg_str(args, 1, name)) {
            (Ok(u), Ok(d)) => {
                let mut a: Vec<&str> = alloc::vec![u, "NAME"];
                a.extend(d.split_whitespace());
                outcome(crate::task::terminal::account_modify(&a))
            }
            (Err(e), _) | (_, Err(e)) => Err(e),
        },
        "user_autologin" => match (arg_str(args, 0, name), arg_bool(args, 1, name)) {
            (Ok(u), Ok(on)) => outcome(crate::task::terminal::account_modify(&[u, "AUTOLOGIN", if on { "ON" } else { "OFF" }])),
            (Err(e), _) | (_, Err(e)) => Err(e),
        },
        "user_unlock" => match arg_str(args, 0, name) {
            Ok(u) => outcome(users::authorize(Action::UnlockUser(u)).map_err(String::from).and_then(|_| {
                let mut db = users::USER_DB.lock();
                let user = db.find_mut(u).ok_or_else(|| String::from("user not found"))?;
                user.unlock();
                let n = user.username.clone();
                db.save();
                drop(db);
                crate::security::access::seclog(&format!("cleared the sign-in delay of '{}'", n));
                Ok(format!("'{}' can sign in again without waiting", n))
            })),
            Err(e) => Err(e),
        },
        "user_delete" => match arg_str(args, 0, name) {
            Ok(u) => outcome(users::authorize(Action::DeleteUser(u)).map_err(String::from).and_then(|_| {
                users::USER_DB.lock().remove(u).map_err(String::from)?;
                crate::security::access::seclog(&format!("deleted account '{}'", u));
                Ok(format!("deleted '{}'", u))
            })),
            Err(e) => Err(e),
        },
        "file_owner" => match arg_str(args, 0, name) {
            Ok(p) => {
                if crate::security::access::user_may_quiet(p, crate::security::access::Op::Read) {
                    Ok(VValue::String(crate::security::access::describe(p).0))
                } else {
                    Ok(VValue::String(String::new()))
                }
            }
            Err(e) => Err(e),
        },
        "file_set_owner" => match (arg_str(args, 0, name), arg_str(args, 1, name)) {
            (Ok(p), Ok(o)) => {
                use crate::security::access::{self, Actor, Owner};
                let owner = match o.to_ascii_lowercase().as_str() {
                    "shared" => Some(Owner::Shared),
                    "system" => Some(Owner::System),
                    n => users::USER_DB.lock().find(n).map(|u| Owner::User(u.uid)),
                };
                match owner {
                    Some(owner) => outcome(access::set_owner(Actor::session(), p, owner)
                        .map(|_| format!("{} now belongs to {}", p, owner.describe()))
                        .map_err(String::from)),
                    None => outcome(Err(format!("no user '{}' (or \"shared\", \"system\")", o))),
                }
            }
            (Err(e), _) | (_, Err(e)) => Err(e),
        },
        "file_grant" => match (arg_str(args, 0, name), arg_str(args, 1, name), arg_str(args, 2, name)) {
            (Ok(p), Ok(u), Ok(how)) => {
                use crate::security::access::{self, Actor};
                let grant = match how.to_ascii_lowercase().as_str() {
                    "r" | "read" => Some(Some(false)),
                    "rw" | "write" => Some(Some(true)),
                    "none" => Some(None),
                    _ => None,
                };
                let Some(grant) = grant else { return Some(Err(format!("{}: \"r\", \"rw\" or \"none\"", name))) };
                match users::USER_DB.lock().find(u).map(|x| x.uid) {
                    Some(uid) => outcome(access::set_grant(Actor::session(), p, uid, grant)
                        .map(|_| format!("{} shared with {}", p, u))
                        .map_err(String::from)),
                    None => outcome(Err(format!("no user '{}'", u))),
                }
            }
            (Err(e), _, _) | (_, Err(e), _) | (_, _, Err(e)) => Err(e),
        },
        // Secrets go to the kernel's hidden prompts, never through a script.
        "request_password_change" => {
            crate::vsh::shell::request_terminal("PASSWD", true);
            Ok(VValue::None)
        }
        "user_create_dialog" => {
            if users::authorize(Action::CreateUser(UserRole::Standard)).is_err() {
                return Some(outcome(Err(String::from("only an admin can create accounts"))));
            }
            crate::vsh::shell::request_terminal("USERADD ", false);
            Ok(VValue::None)
        }
        _ => return None,
    })
}
