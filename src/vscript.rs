// -----| VEILOS Scripting Engine (kernel glue) |-----
//
// The VSH language itself lives in vsh_lang.rs (pure, unit tested). This file
// connects it to the kernel:
//   * GLOBAL_ENGINE           - the one shared interpreter
//   * KernelHost              - implements `set NAME = value`, `print`, `import`
//   * VScriptEngine           - thin wrapper that keeps the old public API
//                               (execute, trigger_event, variables) and adds
//                               named loading (hot reload) and payload events.
//
// Behaviour changes versus the old engine (all deliberate, all reported as
// script errors instead of being silently ignored):
//   * `set` with an unknown name, wrong type or invalid value is an error.
//   * undefined variables and assignment to undeclared variables are errors.
//   * `set` values are stored, so scripts can read them back (THEME[accent]).
//
// Element functions (add, destroy, ref[...], time, launch, focus_window,
// lock_session, shutdown) are implemented in shell.rs.

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::ops::{Deref, DerefMut};
use spin::Mutex;

use crate::desktop::{notify, NotifCategory};
use crate::graphics::Color;
use crate::gui_toolkit::{ColorPalette, ACTIVE_THEME};
use crate::vsh_lang::{CallArg, Handler, Host, Interp};

pub use crate::vsh_lang::VValue;

lazy_static::lazy_static! {
    pub static ref GLOBAL_ENGINE: Mutex<VScriptEngine> = Mutex::new(VScriptEngine::new());
}

// =====================================================================
// -----| engine wrapper |-----
// =====================================================================

pub struct VScriptEngine {
    interp: Interp,
}

// `eng.variables`, `eng.get_global(..)`, `eng.unload(..)` keep working through
// the wrapper.
impl Deref for VScriptEngine {
    type Target = Interp;
    fn deref(&self) -> &Interp { &self.interp }
}

impl DerefMut for VScriptEngine {
    fn deref_mut(&mut self) -> &mut Interp { &mut self.interp }
}

impl VScriptEngine {
    pub fn new() -> Self {
        Self { interp: Interp::new() }
    }

    /// Run a script under an anonymous id (terminal `VSH` command).
    /// Handlers it registers accumulate, exactly like the old engine.
    /// Returns error lines (empty = success).
    pub fn execute(&mut self, script: &str) -> Vec<String> {
        let mut host = KernelHost;
        self.interp.run_anonymous(script, &mut host)
    }

    /// Load or hot-reload a named script (e.g. "/config.vsh"). Loading the
    /// same name again replaces its functions, handlers, globals and imports
    /// instead of duplicating them; a script that fails to parse leaves the
    /// previous version untouched.
    pub fn execute_named(&mut self, source: &str, script: &str) -> Vec<String> {
        let mut host = KernelHost;
        self.interp.load(source, script, &mut host)
    }

    /// Fire an event with string filters only (key_event, start, ...).
    pub fn trigger_event(
        &mut self,
        event_name: &str,
        filters_in: Option<&BTreeMap<String, String>>,
    ) -> Vec<String> {
        self.trigger_event_with(event_name, filters_in, &BTreeMap::new())
    }

    /// Fire an event that also carries values for `on_event name(a, b)`
    /// bindings (window_opened(id, title), ...).
    pub fn trigger_event_with(
        &mut self,
        event_name: &str,
        filters_in: Option<&BTreeMap<String, String>>,
        payload: &BTreeMap<String, VValue>,
    ) -> Vec<String> {
        let mut host = KernelHost;
        self.interp.trigger(event_name, filters_in, payload, &mut host)
    }

    /// Fire an event only for the handlers one script registered (shell reload).
    pub fn trigger_source(&mut self, source: &str, event_name: &str) -> Vec<String> {
        let mut host = KernelHost;
        self.interp.trigger_source(source, event_name, &BTreeMap::new(), &mut host)
    }

    /// Run a handler packaged with an element (click, hover, ...).
    pub fn run_handler(&mut self, handler: &Handler, args: Vec<VValue>) -> Result<(), String> {
        let mut host = KernelHost;
        self.interp.run_handler(handler, args, &mut host)
    }
}

// =====================================================================
// -----| host: what scripts may ask the kernel to do |-----
// =====================================================================

struct KernelHost;

impl Host for KernelHost {
    fn set_system(&mut self, name: &str, value: &VValue) -> Result<(), String> {
        apply_system_setting(name, value)
    }

    fn call(&mut self, name: &str, args: &[CallArg]) -> Option<Result<VValue, String>> {
        match name {
            "print" => {
                let mut out = String::new();
                for (i, a) in args.iter().enumerate() {
                    if i > 0 { out.push(' '); }
                    out.push_str(&a.value.to_display());
                }
                notify(NotifCategory::Info, "VSCRIPT", &out);
                Some(Ok(VValue::None))
            }
            // time, launch, focus_window, lock_session, shutdown, destroy
            _ => crate::shell::host_call(name, args),
        }
    }

    fn add_element(
        &mut self,
        kind: &str,
        def: &crate::vsh_lang::ElementDef,
        named: &[CallArg],
        snapshot: &[(String, Result<VValue, String>)],
    ) -> Result<VValue, String> {
        crate::shell::host_add(kind, def, named, snapshot)
    }

    fn element_prop(&mut self, id: u32, key: &str) -> Result<VValue, String> {
        crate::shell::host_prop(id, key)
    }

    fn is_element_kind(&self, kind: &str) -> bool {
        crate::shell::host_is_kind(kind)
    }

    fn snapshot_key(&self, key: &str) -> bool {
        crate::ui_runtime::is_snapshot_key(key)
    }

    /// `import "/path.vsh"`.
    ///
    /// The VFS mount-table lock is only held while resolving a path (never
    /// across I/O or while the caller holds other locks), so this can't
    /// deadlock against the engine lock the way the old long-held VFS lock
    /// could.
    fn read_file(&mut self, path: &str) -> Result<String, String> {
        let bytes = match crate::vfs::VFS.read_file(path) {
            Ok(b) => b,
            Err(_) => return Err(format!("cannot read '{}'", path)),
        };
        match core::str::from_utf8(&bytes) {
            Ok(s) => Ok(s.to_string()),
            Err(_) => Err(String::from("file is not valid UTF-8")),
        }
    }
}

// =====================================================================
// -----| `set NAME = value` system settings |-----
// =====================================================================

fn want_str<'a>(name: &str, v: &'a VValue) -> Result<&'a str, String> {
    match v {
        VValue::String(s) => Ok(s.as_str()),
        other => Err(format!("{} expects a string, got {}", name, other.type_name())),
    }
}

fn want_bool(name: &str, v: &VValue) -> Result<bool, String> {
    match v {
        VValue::Boolean(b) => Ok(*b),
        other => Err(format!("{} expects true or false, got {}", name, other.type_name())),
    }
}

fn want_int(name: &str, v: &VValue, min: i64, max: i64) -> Result<i64, String> {
    match v {
        VValue::Integer(i) | VValue::IndexVar(i) => {
            if *i < min || *i > max {
                Err(format!("{} must be between {} and {}, got {}", name, min, max, i))
            } else {
                Ok(*i)
            }
        }
        other => Err(format!("{} expects a whole number, got {}", name, other.type_name())),
    }
}

fn want_int_list(name: &str, v: &VValue, min: i64, max: i64) -> Result<Vec<u32>, String> {
    match v {
        VValue::Array(items) if !items.is_empty() => {
            let mut out = Vec::new();
            for it in items {
                out.push(want_int(name, it, min, max)? as u32);
            }
            Ok(out)
        }
        VValue::Array(_) => Err(format!("{} must not be an empty list", name)),
        other => Err(format!("{} expects a list of numbers, got {}", name, other.type_name())),
    }
}

fn apply_system_setting(name: &str, v: &VValue) -> Result<(), String> {
    match name {
        "THEME" => match v {
            VValue::Object(obj) => apply_theme_object(obj),
            other => Err(format!("THEME expects an object, got {}", other.type_name())),
        },
        "SHELL" => {
            // `set SHELL = "/SHELL.VSH";` starts the VSH shell once this script finishes.
            let s = want_str(name, v)?;
            if !s.starts_with('/') {
                return Err(format!("SHELL expects an absolute path such as \"/SHELL.VSH\", got '{}'", s));
            }
            crate::shell::request_start(s);
            Ok(())
        }
        "DESKTOP_DIR" => {
            let s = want_str(name, v)?;
            crate::desktop::DESKTOP_BG.lock().set_desktop_dir(s);
            Ok(())
        }
        "WALLPAPER_PATH" => {
            let s = want_str(name, v)?;
            crate::desktop::DESKTOP_BG.lock().set_wallpaper(s);
            Ok(())
        }
        "WALLPAPER_POSITION" => {
            let s = want_str(name, v)?;
            match s.to_uppercase().as_str() {
                "STRETCH" | "CENTER" | "TILE" | "CROP" | "BASIC" | "WINDOW_FIT" | "WINDOW_CENTER" => {}
                _ => return Err(format!(
                    "unknown wallpaper position '{}' (STRETCH, CENTER, TILE, CROP, BASIC, WINDOW_FIT, WINDOW_CENTER)", s
                )),
            }
            crate::desktop::DESKTOP_BG.lock().set_wallpaper_position(s);
            Ok(())
        }
        "WINDOW_TILES" => {
            let l = want_int_list(name, v, 1, 64)?;
            if l.len() != 2 {
                return Err(format!("WINDOW_TILES expects (columns, rows), got {} value(s)", l.len()));
            }
            crate::desktop::DESKTOP_WM.lock().window_tiles = (l[0], l[1]);
            Ok(())
        }
        "WINDOW_TILE_SIZE_RATIO" => {
            match v {
                VValue::Array(parts) if parts.len() == 2 => {
                    let rx = want_int_list("WINDOW_TILE_SIZE_RATIO", &parts[0], 1, 1000)?;
                    let ry = want_int_list("WINDOW_TILE_SIZE_RATIO", &parts[1], 1, 1000)?;
                    let mut wm = crate::desktop::DESKTOP_WM.lock();
                    wm.window_tile_size_ratio_x = rx;
                    wm.window_tile_size_ratio_y = ry;
                    Ok(())
                }
                _ => Err(String::from("WINDOW_TILE_SIZE_RATIO expects ([x ratios], [y ratios])")),
            }
        }
        "WINDOW_SNAP" => {
            let s = want_str(name, v)?;
            let snap = match s.to_uppercase().as_str() {
                "OFF" => crate::desktop::WindowSnapMode::Off,
                "MAGNETIC" => crate::desktop::WindowSnapMode::Magnetic,
                "STICKY" => crate::desktop::WindowSnapMode::Sticky,
                "SLIPPERY" => crate::desktop::WindowSnapMode::Slippery,
                _ => return Err(format!("unknown snap mode '{}' (OFF, MAGNETIC, STICKY, SLIPPERY)", s)),
            };
            crate::desktop::DESKTOP_WM.lock().window_snap = snap;
            Ok(())
        }
        "WINDOW_STACK" => {
            let b = want_bool(name, v)?;
            crate::desktop::DESKTOP_WM.lock().window_stack = b;
            Ok(())
        }
        "CLIPBOARD_CHANNELS_DEFAULT" => {
            let s = want_str(name, v)?;
            match crate::clipboard::ClipboardChannel::from_str(s) {
                Some(ch) => {
                    crate::clipboard::CLIPBOARD.lock().active_channel = ch;
                    Ok(())
                }
                None => Err(format!("unknown clipboard channel '{}'", s)),
            }
        }
        "CLIPBOARD_STORAGE_DEFAULT" => {
            let s = want_str(name, v)?;
            match crate::clipboard::ClipboardStorage::from_str(s) {
                Some(st) => {
                    crate::clipboard::CLIPBOARD.lock().default_storage = st;
                    Ok(())
                }
                None => Err(format!("unknown clipboard storage '{}'", s)),
            }
        }
        "CLIPBOARD_LOG_SIZE" => {
            let n = want_int(name, v, 0, 1_000_000)?;
            crate::clipboard::CLIPBOARD.lock().max_history = n as usize;
            Ok(())
        }
        "CLIPBOARD_LOCATION" => {
            let s = want_str(name, v)?;
            let mut c = crate::clipboard::CLIPBOARD.lock();
            c.storage_path = s.to_string();
            // Reload history since the path changed.
            c.load_persisted();
            Ok(())
        }
        "CLIPBOARD_FILE_SIZE_LIMIT" => {
            let n = want_int(name, v, 0, i64::MAX)?;
            crate::clipboard::CLIPBOARD.lock().file_size_limit = n as usize;
            Ok(())
        }
        "CLIPBOARD_MAX_SIZE" => {
            let n = want_int(name, v, 0, i64::MAX)?;
            crate::clipboard::CLIPBOARD.lock().max_entry_size = n as usize;
            Ok(())
        }
        other => Err(format!("unknown system setting '{}'", other)),
    }
}

// =====================================================================
// -----| THEME |-----
// =====================================================================

fn theme_color(key: &str, v: &VValue) -> Result<Color, String> {
    match v {
        // The palette has no alpha channel yet; alpha is dropped here.
        VValue::Color(c) => Ok(Color { r: c.r, g: c.g, b: c.b }),
        VValue::String(s) if s.len() == 7 && s.starts_with('#') => {
            let ch = |a: usize, b: usize| u8::from_str_radix(&s[a..b], 16);
            match (ch(1, 3), ch(3, 5), ch(5, 7)) {
                (Ok(r), Ok(g), Ok(b)) => Ok(Color { r, g, b }),
                _ => Err(format!("THEME[{}]: bad color '{}'", key, s)),
            }
        }
        other => Err(format!("THEME[{}] expects a color, got {}", key, other.type_name())),
    }
}

fn palette_slot<'a>(p: &'a mut ColorPalette, key: &str) -> Option<&'a mut Color> {
    match key {
        "background" => Some(&mut p.background),
        "foreground" | "text_primary" => Some(&mut p.text_primary),
        "foreground_dim" | "text_secondary" => Some(&mut p.text_secondary),
        "text_disabled" => Some(&mut p.text_disabled),
        "surface_raised" | "surface" => Some(&mut p.surface),
        "border" => Some(&mut p.border),
        "accent" => Some(&mut p.accent),
        "accent_hover" => Some(&mut p.accent_hover),
        "accent_press" => Some(&mut p.accent_press),
        "danger" => Some(&mut p.danger),
        "warning" => Some(&mut p.warning),
        "success" => Some(&mut p.success),
        "titlebar" => Some(&mut p.titlebar),
        "titlebar_text" => Some(&mut p.titlebar_text),
        "selection" => Some(&mut p.selection),
        "cursor" => Some(&mut p.cursor),
        _ => None,
    }
}

/// Validate the whole THEME object first, then apply it. A theme replaces the
/// palette (unspecified colors come from the Veil palette, as before), so a
/// bad object changes nothing.
fn apply_theme_object(obj: &BTreeMap<String, VValue>) -> Result<(), String> {
    let mut p = ColorPalette::veil();
    let mut radius: u32 = 4;
    let mut unknown: Vec<String> = Vec::new();

    for (k, v) in obj {
        if let Some(slot) = palette_slot(&mut p, k) {
            *slot = theme_color(k, v)?;
            continue;
        }
        match k.as_str() {
            "border_radius" => radius = want_int("THEME[border_radius]", v, 0, 64)? as u32,
            // Accepted for the theme contract; consumed by the element runtime
            // / font engine when those land, not by the palette.
            "surface_sunken" => { theme_color(k, v)?; }
            "font_family" => { want_str("THEME[font_family]", v)?; }
            "font_size" => { want_int("THEME[font_size]", v, 4, 128)?; }
            _ => unknown.push(k.clone()),
        }
    }
    if !unknown.is_empty() {
        return Err(format!("unknown THEME key(s): {}", unknown.join(", ")));
    }

    {
        let mut t = ACTIVE_THEME.lock();
        t.palette = p;
        t.corner_radius = radius;
    }
    crate::desktop::DESKTOP_WM.lock().z_order_dirty = true;
    Ok(())
}
