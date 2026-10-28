// -----| VEILOS UI runtime: the element tree behind VSH `add element.*` |-----
//
// Pure `alloc` code (no kernel dependencies, no floats), so it is unit tested on
// a host against the real shell script. The kernel glue lives in shell.rs.
//
// The model (dark.vsh header):
//   * `add` turns a definition into an instance. SNAPSHOT properties are baked
//     at that moment (the interpreter evaluates them, see vsh_lang eval_add);
//     LIVE properties keep their expressions and are re-evaluated only when a
//     variable is written (Interp::generation) or, for expressions that call
//     time(), once a second. Nothing is polled.
//   * Instances are never mutated from script: the only changes are variable
//     writes (live re-evaluation) and destroy + add.
//   * Hover / pressed looks are packaged with the element as `hover: {...}` and
//     `pressed: {...}` blocks of live style overrides.
//   * Every element renders into its own CachedLayer, so it is re-rasterised
//     only when its resolved look, size or state changes.
//
// Layers: a ROOT element's own body draws on the desktop (under the wallpaper,
// so it is the fallback colour when no wallpaper covers it); everything added
// under a root draws above windows.

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::font::{fit_with_ellipsis, measure, draw_text, FontLibrary, GlyphCache, TextStyle};
use crate::render2d::*;
use crate::vsh_lang::{CallArg, ElementDef, Expr, Handler, Host, Interp, InterpPart, ObjEntry, VValue};

// =====================================================================
// -----| property tables (edit here to move a property between lists) |-----
// =====================================================================

pub const KINDS: &[&str] = &["panel", "row", "column", "menu", "button", "label", "divider", "icon"];

/// Baked at `add`.
pub const SNAPSHOT_KEYS: &[&str] = &[
    "size", "min_width", "max_width", "min_height", "max_height", "position", "anchor", "padding",
    "border_radius", "border_thickness", "shadow", "flow", "rotation", "text_align", "align",
    "cursor", "font_features", "z",
];

/// Re-evaluated when a variable they read is written.
pub const LIVE_KEYS: &[&str] = &[
    "text", "label", "background", "color", "border", "visible", "opacity", "font_size",
    "font_weight", "font_family",
];

/// Packaged live style overrides for interaction states.
pub const STATE_KEYS: &[&str] = &["hover", "pressed"];

pub const HANDLER_KEYS: &[&str] = &["on_click", "on_hover_enter", "on_hover_leave"];

pub fn is_snapshot_key(k: &str) -> bool {
    SNAPSHOT_KEYS.contains(&k)
}

fn is_known_key(k: &str) -> bool {
    SNAPSHOT_KEYS.contains(&k) || LIVE_KEYS.contains(&k) || STATE_KEYS.contains(&k) || HANDLER_KEYS.contains(&k) || k == "id"
}

/// Colour used where a property's value is missing or invalid (spec: "a visible
/// error colour rather than crashing").
pub const ERROR_COLOR: Color32 = Color32::rgb(255, 0, 255);

const MAX_ERRORS: usize = 64;
const MAX_EVENTS: usize = 32;
/// Elements bigger than this are painted directly each frame instead of being
/// cached in a layer (a full-screen layer would cost 3 MB and a full blend).
const MAX_CACHED_PIXELS: i64 = 256 * 1024;

// =====================================================================
// -----| value parsing |-----
// =====================================================================

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Panel,
    Row,
    Column,
    Menu,
    Button,
    Label,
    Divider,
    Icon,
}

impl Kind {
    fn parse(s: &str) -> Option<Kind> {
        Some(match s {
            "panel" => Kind::Panel,
            "row" => Kind::Row,
            "column" => Kind::Column,
            "menu" => Kind::Menu,
            "button" => Kind::Button,
            "label" => Kind::Label,
            "divider" => Kind::Divider,
            "icon" => Kind::Icon,
            _ => return None,
        })
    }
    pub fn name(self) -> &'static str {
        match self {
            Kind::Panel => "panel",
            Kind::Row => "row",
            Kind::Column => "column",
            Kind::Menu => "menu",
            Kind::Button => "button",
            Kind::Label => "label",
            Kind::Divider => "divider",
            Kind::Icon => "icon",
        }
    }
    fn has_text(self) -> bool {
        matches!(self, Kind::Button | Kind::Label)
    }
}

/// A length: pixels, a percentage of the parent (plus an offset), or auto.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dim {
    Auto,
    /// `pct_bp` is in basis points (100% = 10000), `px` is added after.
    Rel { pct_bp: i32, px: i32 },
}

impl Dim {
    pub const fn px(n: i32) -> Dim {
        Dim::Rel { pct_bp: 0, px: n }
    }
    fn resolve(self, base: i32, auto: i32) -> i32 {
        match self {
            Dim::Auto => auto,
            Dim::Rel { pct_bp, px } => ((base as i64 * pct_bp as i64) / 10000) as i32 + px,
        }
    }
    fn is_auto(self) -> bool {
        self == Dim::Auto
    }
}

/// Parse "100% - 32", "50%", "auto", "12", "33.5%+4". Integer-only arithmetic.
/// Terms must be joined by '+' or '-' ("10 20" is an error, not 1020).
pub fn parse_dim_str(s: &str) -> Result<Dim, String> {
    let t = s.trim();
    if t.eq_ignore_ascii_case("auto") {
        return Ok(Dim::Auto);
    }
    if t.is_empty() {
        return Err(String::from("empty length"));
    }
    let bad = || format!("bad length '{}' (use pixels, N%, N% - M or auto)", s);
    let (mut pct_bp, mut px) = (0i64, 0i64);
    let b = t.as_bytes();
    let mut i = 0usize;
    let skip_ws = |i: &mut usize| {
        while *i < b.len() && b[*i].is_ascii_whitespace() {
            *i += 1;
        }
    };
    let mut first = true;
    loop {
        skip_ws(&mut i);
        let mut sign = 1i64;
        if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
            sign = if b[i] == b'-' { -1 } else { 1 };
            i += 1;
            skip_ws(&mut i);
        } else if !first {
            return Err(format!("expected '+' or '-' between the parts of '{}'", s));
        }
        first = false;
        let start = i;
        let mut whole: i64 = 0;
        let mut frac: i64 = 0; // hundredths
        let mut frac_digits = 0;
        let mut seen_dot = false;
        while i < b.len() && (b[i].is_ascii_digit() || b[i] == b'.') {
            if b[i] == b'.' {
                if seen_dot {
                    return Err(bad());
                }
                seen_dot = true;
            } else if seen_dot {
                if frac_digits < 2 {
                    frac = frac * 10 + (b[i] - b'0') as i64;
                    frac_digits += 1;
                }
            } else {
                whole = whole.saturating_mul(10).saturating_add((b[i] - b'0') as i64);
            }
            i += 1;
        }
        if i == start {
            return Err(bad());
        }
        while frac_digits < 2 {
            frac *= 10;
            frac_digits += 1;
        }
        if i < b.len() && b[i] == b'%' {
            pct_bp += sign * (whole * 100 + frac);
            i += 1;
        } else {
            if seen_dot {
                return Err(format!("pixel values must be whole numbers in '{}'", s));
            }
            px += sign * whole;
        }
        if pct_bp.abs() > 1_000_000 || px.abs() > 1_000_000 {
            return Err(format!("length '{}' is out of range", s));
        }
        skip_ws(&mut i);
        if i >= b.len() {
            break;
        }
        if b[i] != b'+' && b[i] != b'-' {
            return Err(bad());
        }
    }
    Ok(Dim::Rel { pct_bp: pct_bp as i32, px: px as i32 })
}

fn val_dim(v: &VValue) -> Result<Dim, String> {
    match v {
        VValue::Integer(n) | VValue::IndexVar(n) => {
            if n.abs() > 1_000_000 {
                Err(format!("{} is out of range", n))
            } else {
                Ok(Dim::px(*n as i32))
            }
        }
        VValue::Float(_) => Err(String::from("pixel values must be whole numbers")),
        VValue::String(s) => parse_dim_str(s),
        other => Err(format!("expected a length, got {}", other.type_name())),
    }
}

fn val_int(v: &VValue) -> Result<i64, String> {
    match v {
        VValue::Integer(n) | VValue::IndexVar(n) => Ok(*n),
        VValue::Float(_) => Err(String::from("expected a whole number")),
        other => Err(format!("expected a number, got {}", other.type_name())),
    }
}

fn val_px(v: &VValue, lo: i64, hi: i64) -> Result<i32, String> {
    let n = val_int(v)?;
    if n < lo || n > hi {
        return Err(format!("{} is outside {}..{}", n, lo, hi));
    }
    Ok(n as i32)
}

fn val_color(v: &VValue) -> Result<Color32, String> {
    match v {
        VValue::Color(c) => Ok(Color32::rgba(c.r, c.g, c.b, c.a)),
        VValue::None => Err(String::from("value is missing (a THEME key that does not exist?)")),
        VValue::String(s) if s.starts_with('#') && (s.len() == 7 || s.len() == 9) => {
            let h = |i: usize| u8::from_str_radix(&s[i..i + 2], 16).map_err(|_| format!("bad colour '{}'", s));
            let a = if s.len() == 9 { h(7)? } else { 255 };
            Ok(Color32::rgba(h(1)?, h(3)?, h(5)?, a))
        }
        other => Err(format!("expected a colour, got {}", other.type_name())),
    }
}

fn val_str(v: &VValue) -> Result<String, String> {
    match v {
        VValue::String(s) => Ok(s.clone()),
        VValue::None => Err(String::from("value is missing")),
        other => Ok(other.to_display()),
    }
}

fn val_bool(v: &VValue) -> Result<bool, String> {
    match v {
        VValue::Boolean(b) => Ok(*b),
        VValue::Integer(n) => Ok(*n != 0),
        VValue::None => Err(String::from("value is missing")),
        other => Err(format!("expected true or false, got {}", other.type_name())),
    }
}

fn list_of(v: &VValue) -> Vec<VValue> {
    match v {
        VValue::Array(a) => a.clone(),
        other => alloc::vec![other.clone()],
    }
}

/// (top, right, bottom, left) from n | (v, h) | (t, h, b) | (t, r, b, l).
fn val_sides(v: &VValue, hi: i64) -> Result<[i32; 4], String> {
    let l = list_of(v);
    let n: Vec<i32> = l.iter().map(|x| val_px(x, 0, hi)).collect::<Result<_, _>>()?;
    Ok(match n.len() {
        1 => [n[0], n[0], n[0], n[0]],
        2 => [n[0], n[1], n[0], n[1]],
        3 => [n[0], n[1], n[2], n[1]],
        4 => [n[0], n[1], n[2], n[3]],
        k => return Err(format!("expected 1, 2, 3 or 4 values, got {}", k)),
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HAnchor {
    Left,
    Center,
    Right,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VAnchor {
    Top,
    Center,
    Bottom,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Justify {
    Start,
    Center,
    End,
    SpaceBetween,
    SpaceAround,
    SpaceEvenly,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    Start,
    Center,
    End,
    Stretch,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextAlign {
    Left,
    Center,
    Right,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Flow {
    pub horizontal: bool,
    pub justify: Justify,
    pub align: Align,
    pub gap: i32,
    pub padding: [i32; 4],
}

fn parse_flow(v: &VValue) -> Result<Flow, String> {
    let m = match v {
        VValue::Object(m) => m,
        other => return Err(format!("flow must be an object, got {}", other.type_name())),
    };
    let mut f = Flow { horizontal: true, justify: Justify::Start, align: Align::Start, gap: 0, padding: [0; 4] };
    for (k, x) in m {
        match k.as_str() {
            "axis" => {
                f.horizontal = match val_str(x)?.as_str() {
                    "horizontal" | "row" | "x" => true,
                    "vertical" | "column" | "y" => false,
                    o => return Err(format!("flow.axis '{}' (horizontal | vertical)", o)),
                }
            }
            "justify" => {
                f.justify = match val_str(x)?.as_str() {
                    "start" => Justify::Start,
                    "center" => Justify::Center,
                    "end" => Justify::End,
                    "space_between" => Justify::SpaceBetween,
                    "space_around" => Justify::SpaceAround,
                    "space_evenly" => Justify::SpaceEvenly,
                    o => return Err(format!("flow.justify '{}' (start | center | end | space_between | space_around | space_evenly)", o)),
                }
            }
            "align" => {
                f.align = match val_str(x)?.as_str() {
                    "start" => Align::Start,
                    "center" => Align::Center,
                    "end" => Align::End,
                    "stretch" => Align::Stretch,
                    o => return Err(format!("flow.align '{}' (start | center | end | stretch)", o)),
                }
            }
            "gap" => f.gap = val_px(x, 0, 4096)?,
            "padding" => f.padding = val_sides(x, 4096)?,
            o => return Err(format!("unknown flow key '{}'", o)),
        }
    }
    Ok(f)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fill {
    None,
    Solid(Color32),
    Linear { angle: i32, stops: Vec<Stop> },
    Radial { cx: u16, cy: u16, stops: Vec<Stop> },
}

fn parse_stops(v: &VValue) -> Result<Vec<Stop>, String> {
    let l = match v {
        VValue::Array(a) => a.clone(),
        other => return Err(format!("stops must be a list, got {}", other.type_name())),
    };
    if l.is_empty() || l.len() > 16 {
        return Err(String::from("a gradient needs 1 to 16 stops"));
    }
    let n = l.len();
    let mut out = Vec::new();
    for (i, s) in l.iter().enumerate() {
        match s {
            VValue::Array(pair) if pair.len() == 2 => {
                let pct = val_px(&pair[0], 0, 100)?;
                out.push(Stop { pos: (pct * 10) as u16, color: val_color(&pair[1])? });
            }
            other => {
                let pos = if n == 1 { 0 } else { (i * 1000 / (n - 1)) as u16 };
                out.push(Stop { pos, color: val_color(other)? });
            }
        }
    }
    Ok(out)
}

fn val_fill(v: &VValue) -> Result<Fill, String> {
    match v {
        VValue::Color(c) if c.a == 0 => Ok(Fill::None),
        VValue::Object(m) => {
            let stops = parse_stops(m.get("stops").ok_or_else(|| String::from("a gradient needs 'stops'"))?)?;
            if m.get("image").is_some() {
                return Err(String::from("image backgrounds are not supported yet"));
            }
            let radial = match m.get("radial") {
                Some(r) => val_bool(r)?,
                None => false,
            };
            if radial {
                let (cx, cy) = match m.get("center") {
                    Some(c) => {
                        let l = list_of(c);
                        if l.len() != 2 {
                            return Err(String::from("center must be (x%, y%)"));
                        }
                        (val_px(&l[0], 0, 100)? as u16 * 10, val_px(&l[1], 0, 100)? as u16 * 10)
                    }
                    None => (500, 500),
                };
                Ok(Fill::Radial { cx, cy, stops })
            } else {
                let angle = match m.get("angle") {
                    Some(a) => val_px(a, -3600, 3600)?,
                    None => 180,
                };
                Ok(Fill::Linear { angle, stops })
            }
        }
        other => Ok(Fill::Solid(val_color(other)?)),
    }
}

// =====================================================================
// -----| resolved properties |-----
// =====================================================================

/// Snapshot properties, parsed once at `add`.
#[derive(Clone, Debug, PartialEq)]
pub struct Snap {
    pub size: (Dim, Dim),
    pub min: (i32, i32),
    pub max: (i32, i32),
    pub position: Option<(Dim, Dim)>,
    pub z: i32,
    pub anchor: (HAnchor, VAnchor),
    pub padding: [i32; 4],
    pub radius: u16,
    pub border_thickness: [i32; 4],
    pub shadow: Option<Shadow>,
    pub flow: Option<Flow>,
    pub rotation: i32,
    pub text_align: Option<TextAlign>,
    pub tabular: bool,
}

impl Snap {
    fn defaults(kind: Kind) -> Snap {
        let flow = match kind {
            Kind::Row => Some(Flow { horizontal: true, justify: Justify::Start, align: Align::Center, gap: 0, padding: [0; 4] }),
            Kind::Column | Kind::Menu => Some(Flow { horizontal: false, justify: Justify::Start, align: Align::Stretch, gap: 0, padding: [0; 4] }),
            _ => None,
        };
        Snap {
            size: (Dim::Auto, Dim::Auto),
            min: (0, 0),
            max: (i32::MAX, i32::MAX),
            position: None,
            z: 0,
            anchor: (HAnchor::Left, VAnchor::Top),
            padding: [0; 4],
            radius: 0,
            border_thickness: [0; 4],
            shadow: None,
            flow,
            rotation: 0,
            text_align: None,
            tabular: false,
        }
    }

    fn apply(&mut self, key: &str, v: &VValue) -> Result<(), String> {
        match key {
            "size" => {
                let l = list_of(v);
                if l.len() != 2 {
                    return Err(String::from("size must be (width, height)"));
                }
                self.size = (val_dim(&l[0])?, val_dim(&l[1])?);
            }
            "min_width" => self.min.0 = val_px(v, 0, 1 << 20)?,
            "min_height" => self.min.1 = val_px(v, 0, 1 << 20)?,
            "max_width" => self.max.0 = val_px(v, 0, 1 << 20)?,
            "max_height" => self.max.1 = val_px(v, 0, 1 << 20)?,
            "position" => {
                let l = list_of(v);
                if l.len() != 2 && l.len() != 3 {
                    return Err(String::from("position must be (x, y) or (x, y, z)"));
                }
                self.position = Some((val_dim(&l[0])?, val_dim(&l[1])?));
                if l.len() == 3 {
                    self.z = val_px(&l[2], -1000, 1000)?;
                }
            }
            "z" => self.z = val_px(v, -1000, 1000)?,
            "anchor" => {
                // (horizontal, vertical); a single "center" centres both ways.
                let l = list_of(v);
                if l.is_empty() || l.len() > 2 {
                    return Err(String::from("anchor must be (horizontal, vertical)"));
                }
                let mut h = HAnchor::Left;
                let mut vv = VAnchor::Top;
                for (i, a) in l.iter().enumerate() {
                    match val_str(a)?.as_str() {
                        "left" => h = HAnchor::Left,
                        "right" => h = HAnchor::Right,
                        "top" => vv = VAnchor::Top,
                        "bottom" => vv = VAnchor::Bottom,
                        "center" if l.len() == 1 => {
                            h = HAnchor::Center;
                            vv = VAnchor::Center;
                        }
                        "center" if i == 0 => h = HAnchor::Center,
                        "center" => vv = VAnchor::Center,
                        o => return Err(format!("anchor '{}' (left | center | right, top | center | bottom)", o)),
                    }
                }
                self.anchor = (h, vv);
            }
            "padding" => self.padding = val_sides(v, 4096)?,
            "border_radius" => self.radius = val_px(v, 0, 512)? as u16,
            "border_thickness" => self.border_thickness = val_sides(v, 256)?,
            "shadow" => {
                self.shadow = match v {
                    VValue::None => None,
                    VValue::Array(l) if l.len() == 4 || l.len() == 5 => {
                        let spread = if l.len() == 5 { val_px(&l[3], -256, 256)? } else { 0 };
                        Some(Shadow {
                            dx: val_px(&l[0], -512, 512)?,
                            dy: val_px(&l[1], -512, 512)?,
                            blur: val_px(&l[2], 0, 256)? as u16,
                            spread,
                            color: val_color(&l[l.len() - 1])?,
                            knockout: true,
                        })
                    }
                    _ => return Err(String::from("shadow must be (x, y, blur, colour) or (x, y, blur, spread, colour)")),
                }
            }
            "flow" => self.flow = Some(parse_flow(v)?),
            "rotation" => self.rotation = val_px(v, -36000, 36000)?,
            "text_align" | "align" => {
                self.text_align = Some(match val_str(v)?.as_str() {
                    "left" | "start" => TextAlign::Left,
                    "center" => TextAlign::Center,
                    "right" | "end" => TextAlign::Right,
                    o => return Err(format!("text_align '{}' (left | center | right)", o)),
                })
            }
            "font_features" => {
                let s = val_str(v)?;
                self.tabular = s.split(|c: char| c == ',' || c.is_whitespace()).any(|f| f == "tnum");
            }
            "cursor" => {
                val_str(v)?; // accepted; the OS cursor is not switchable yet
            }
            _ => {}
        }
        Ok(())
    }
}

/// Live properties after evaluation.
#[derive(Clone, Debug, PartialEq)]
pub struct Live {
    pub background: Fill,
    pub color: Color32,
    pub border: Color32,
    pub visible: bool,
    pub opacity: u8,
    pub text: String,
    pub font_size: u32,
    pub font_weight: u16,
    pub font_family: String,
    /// White wash drawn over the body (default hover / press feedback).
    pub wash: u8,
}

impl Live {
    fn defaults() -> Live {
        Live {
            background: Fill::None,
            color: Color32::rgb(235, 238, 250),
            border: Color32::TRANSPARENT,
            visible: true,
            opacity: 255,
            text: String::new(),
            font_size: 14,
            font_weight: 400,
            font_family: String::from("default"),
            wash: 0,
        }
    }

    fn apply(&mut self, key: &str, v: &VValue) -> Result<(), String> {
        match key {
            "background" => self.background = val_fill(v)?,
            "color" => self.color = val_color(v)?,
            "border" => self.border = val_color(v)?,
            "visible" => self.visible = val_bool(v)?,
            "opacity" => {
                self.opacity = match v {
                    VValue::Float(f) => {
                        if !(0.0..=1.0).contains(f) {
                            return Err(String::from("opacity must be between 0.0 and 1.0"));
                        }
                        (*f * 255.0 + 0.5) as u8
                    }
                    VValue::Integer(0) => 0,
                    VValue::Integer(1) => 255,
                    _ => return Err(String::from("opacity must be between 0.0 and 1.0")),
                }
            }
            "text" | "label" => self.text = val_str(v)?,
            "font_size" => {
                self.font_size = match v {
                    VValue::Float(f) if *f >= 1.0 && *f <= 256.0 => (*f + 0.5) as u32,
                    _ => val_px(v, 1, 256)? as u32,
                }
            }
            "font_weight" => self.font_weight = val_px(v, 1, 1000)? as u16,
            "font_family" => self.font_family = val_str(v)?,
            "wash" => self.wash = val_px(v, 0, 255)? as u8,
            _ => {}
        }
        Ok(())
    }

    /// Where a bad value falls back to (the spec's visible error colour for colours).
    fn on_error(&mut self, key: &str) {
        match key {
            "background" => self.background = Fill::Solid(ERROR_COLOR),
            "color" => self.color = ERROR_COLOR,
            "border" => self.border = ERROR_COLOR,
            "text" | "label" => self.text = String::from("?"),
            _ => {}
        }
    }
}

// =====================================================================
// -----| elements |-----
// =====================================================================

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventKind {
    Click,
    HoverEnter,
    HoverLeave,
}

pub struct Elem {
    pub id: u32,
    pub kind: Kind,
    pub parent: Option<u32>,
    pub children: Vec<u32>,
    seq: u64,
    /// The element's named `add` arguments (plus `self`), visible to its expressions.
    pub scope: BTreeMap<String, VValue>,
    live_exprs: Vec<(String, Expr)>,
    hover_exprs: Vec<(String, Expr)>,
    pressed_exprs: Vec<(String, Expr)>,
    uses_time: bool,
    pub snap: Snap,
    pub live: Live,
    live_hover: Live,
    live_pressed: Live,
    on_click: Option<Handler>,
    on_hover_enter: Option<Handler>,
    on_hover_leave: Option<Handler>,
    pub rect: IRect,
    pub hovered: bool,
    pub pressed: bool,
    layer: CachedLayer,
    evaluated: bool,
}

impl Elem {
    /// The look for its current interaction state.
    pub fn current(&self) -> &Live {
        if self.pressed {
            &self.live_pressed
        } else if self.hovered {
            &self.live_hover
        } else {
            &self.live
        }
    }
}

fn expr_calls_time(e: &Expr) -> bool {
    match e {
        Expr::Call(name, args) => name == "time" || args.iter().any(|a| expr_calls_time(&a.value)),
        Expr::Lit(_) | Expr::Var(_) | Expr::Block(_) => false,
        Expr::Object(entries) => entries.iter().any(|en| match en {
            ObjEntry::Prop(_, x) | ObjEntry::Spread(x) => expr_calls_time(x),
        }),
        Expr::Array(items) | Expr::Concat(items) => items.iter().any(expr_calls_time),
        Expr::Interp(parts) => parts.iter().any(|p| matches!(p, InterpPart::Expr(x) if expr_calls_time(x))),
        Expr::Unary(_, x) | Expr::Format(x) | Expr::Field(x, _) => expr_calls_time(x),
        Expr::Binary(_, a, b) | Expr::Index(a, b) => expr_calls_time(a) || expr_calls_time(b),
        Expr::IfExpr(c, a, b) => expr_calls_time(c) || expr_calls_time(a) || expr_calls_time(b),
        Expr::Method(t, _, args) => expr_calls_time(t) || args.iter().any(|a| expr_calls_time(&a.value)),
        Expr::AddElement { args, .. } => args.iter().any(|a| expr_calls_time(&a.value)),
    }
}

// =====================================================================
// -----| the shell |-----
// =====================================================================

/// Something a frame needs the interpreter to evaluate (passed in by the kernel).
pub trait Evaluator {
    fn eval(&mut self, expr: &Expr, scope: &BTreeMap<String, VValue>) -> Result<VValue, String>;
    fn generation(&self) -> u64;
}

pub struct Shell {
    elems: BTreeMap<u32, Elem>,
    roots: Vec<u32>,
    next_id: u32,
    seq: u64,
    gen_seen: u64,
    time_seen: u64,
    /// Painted above windows, back to front. Only visible elements.
    overlay_order: Vec<u32>,
    /// Root bodies, painted on the desktop under the wallpaper.
    desktop_order: Vec<u32>,
    events: [(u32, EventKind); MAX_EVENTS],
    n_events: usize,
    errors: Vec<String>,
    /// Every error already reported, so a bad live value re-evaluated each
    /// second (CPU_USAGE ticks) is logged once, not once a second.
    reported: alloc::collections::BTreeSet<String>,
    hover_hit: Option<u32>,
    press_target: Option<u32>,
    /// Set when hover/press changed since the last frame (needs a repaint).
    pub state_changed: bool,
    structure_changed: bool,
    screen: (i32, i32),
}

impl Shell {
    pub const fn new() -> Shell {
        Shell {
            elems: BTreeMap::new(),
            roots: Vec::new(),
            next_id: 1,
            seq: 0,
            gen_seen: u64::MAX,
            time_seen: u64::MAX,
            overlay_order: Vec::new(),
            desktop_order: Vec::new(),
            events: [(0, EventKind::Click); MAX_EVENTS],
            n_events: 0,
            errors: Vec::new(),
            reported: alloc::collections::BTreeSet::new(),
            hover_hit: None,
            press_target: None,
            state_changed: false,
            structure_changed: false,
            screen: (1024, 768),
        }
    }

    pub fn len(&self) -> usize {
        self.elems.len()
    }

    pub fn is_empty(&self) -> bool {
        self.elems.is_empty()
    }

    pub fn elem(&self, id: u32) -> Option<&Elem> {
        self.elems.get(&id)
    }

    pub fn find_by_id(&self, id_name: &str) -> Option<&Elem> {
        self.elems.values().find(|e| matches!(e.scope.get("id"), Some(VValue::String(s)) if s == id_name))
    }

    pub fn overlay_order(&self) -> &[u32] {
        &self.overlay_order
    }

    pub fn desktop_order(&self) -> &[u32] {
        &self.desktop_order
    }

    pub fn take_errors(&mut self) -> Vec<String> {
        core::mem::take(&mut self.errors)
    }

    fn error(&mut self, msg: String) {
        if self.reported.contains(&msg) {
            return;
        }
        if self.reported.len() < 256 {
            self.reported.insert(msg.clone());
        }
        if self.errors.len() < MAX_ERRORS {
            self.errors.push(msg);
        }
    }

    /// Remove every element (shell reload / shell off).
    pub fn clear(&mut self) {
        self.elems.clear();
        self.roots.clear();
        self.overlay_order.clear();
        self.desktop_order.clear();
        self.n_events = 0;
        self.hover_hit = None;
        self.press_target = None;
        self.gen_seen = u64::MAX;
        self.structure_changed = true;
        self.reported.clear();
    }

    // ---------------- add / destroy / read ----------------

    /// `add element.<kind>(Def, name: value, ...)`. `snapshot` comes from the
    /// interpreter (evaluated at this moment); named arguments win over the
    /// definition. Problems with individual properties are reported through
    /// `take_errors` and do not stop the element from appearing.
    pub fn add(
        &mut self,
        kind: &str,
        def: &ElementDef,
        named: &[CallArg],
        snapshot: &[(String, Result<VValue, String>)],
    ) -> Result<u32, String> {
        let kind = Kind::parse(kind).ok_or_else(|| format!("unknown element kind '{}'", kind))?;
        let mut scope: BTreeMap<String, VValue> = BTreeMap::new();
        let mut parent: Option<u32> = None;
        for a in named {
            let n = match &a.name {
                Some(n) => n,
                None => continue,
            };
            if n == "parent" {
                match &a.value {
                    VValue::Ref(p) => {
                        if !self.elems.contains_key(p) {
                            return Err(format!("parent element #{} does not exist (destroyed?)", p));
                        }
                        parent = Some(*p);
                    }
                    VValue::None => return Err(String::from("parent is not set (the element_ref variable is empty)")),
                    other => return Err(format!("parent must be an element_ref, got {}", other.type_name())),
                }
            } else {
                scope.insert(n.clone(), a.value.clone());
            }
        }
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1).max(1);
        let label = |k: &str| format!("element.{} #{} '{}'", kind.name(), id, k);
        let mut errors: Vec<String> = Vec::new();

        // Snapshot: definition values (already evaluated), then named arguments.
        let mut snap = Snap::defaults(kind);
        let mut handlers: [Option<Handler>; 3] = [None, None, None];
        for (k, r) in snapshot {
            match r {
                Ok(v) => {
                    if let Some(i) = HANDLER_KEYS.iter().position(|h| h == k) {
                        match v {
                            VValue::Handler(h) => handlers[i] = Some(h.clone()),
                            VValue::None => {}
                            other => errors.push(format!("{}: expected a function or {{ ... }} block, got {}", label(k), other.type_name())),
                        }
                    } else if k.starts_with("on_") {
                        errors.push(format!("{}: unknown event (use on_click, on_hover_enter, on_hover_leave)", label(k)));
                    } else if let Err(e) = snap.apply(k, v) {
                        errors.push(format!("{}: {}", label(k), e));
                    }
                }
                Err(e) => errors.push(format!("{}: {}", label(k), e)),
            }
        }
        for (k, v) in &scope {
            if is_snapshot_key(k) {
                if let Err(e) = snap.apply(k, v) {
                    errors.push(format!("{}: {}", label(k), e));
                }
            }
        }

        // Live expressions (named arguments become constant expressions).
        let mut live_exprs: Vec<(String, Expr)> = Vec::new();
        let mut hover_exprs: Vec<(String, Expr)> = Vec::new();
        let mut pressed_exprs: Vec<(String, Expr)> = Vec::new();
        for (k, e) in def.props.iter() {
            if !is_known_key(k) && !k.starts_with("on_") {
                errors.push(format!("{}: unknown property", label(k)));
                continue;
            }
            if scope.contains_key(k) {
                continue;
            }
            if LIVE_KEYS.contains(&k.as_str()) {
                live_exprs.push((k.clone(), e.clone()));
            } else if STATE_KEYS.contains(&k.as_str()) {
                let target = if k == "hover" { &mut hover_exprs } else { &mut pressed_exprs };
                match e {
                    Expr::Object(entries) => {
                        for en in entries {
                            match en {
                                ObjEntry::Prop(pk, pe) if LIVE_KEYS.contains(&pk.as_str()) => target.push((pk.clone(), pe.clone())),
                                ObjEntry::Prop(pk, _) => errors.push(format!("{}: '{}' cannot change on {} (only live properties can)", label(k), pk, k)),
                                ObjEntry::Spread(_) => errors.push(format!("{}: spreading inside a {} block is not supported", label(k), k)),
                            }
                        }
                    }
                    _ => errors.push(format!("{}: must be a {{ property: value }} block", label(k))),
                }
            }
        }
        for (k, v) in &scope {
            if LIVE_KEYS.contains(&k.as_str()) {
                live_exprs.push((k.clone(), Expr::Lit(v.clone())));
            }
        }
        let uses_time = live_exprs.iter().chain(hover_exprs.iter()).chain(pressed_exprs.iter()).any(|(_, e)| expr_calls_time(e));
        scope.insert(String::from("self"), VValue::Ref(id));

        self.seq += 1;
        let e = Elem {
            id,
            kind,
            parent,
            children: Vec::new(),
            seq: self.seq,
            scope,
            live_exprs,
            hover_exprs,
            pressed_exprs,
            uses_time,
            snap,
            live: Live::defaults(),
            live_hover: Live::defaults(),
            live_pressed: Live::defaults(),
            on_click: handlers[0].take(),
            on_hover_enter: handlers[1].take(),
            on_hover_leave: handlers[2].take(),
            rect: IRect::EMPTY,
            hovered: false,
            pressed: false,
            layer: CachedLayer::new(),
            evaluated: false,
        };
        self.elems.insert(id, e);
        match parent {
            Some(p) => {
                if let Some(pe) = self.elems.get_mut(&p) {
                    pe.children.push(id);
                }
            }
            None => self.roots.push(id),
        }
        for m in errors {
            self.error(m);
        }
        self.structure_changed = true;
        Ok(id)
    }

    /// Remove an element and everything under it.
    pub fn destroy(&mut self, id: u32) -> Result<(), String> {
        if !self.elems.contains_key(&id) {
            return Err(format!("element #{} does not exist (already destroyed?)", id));
        }
        let mut stack = alloc::vec![id];
        let mut doomed = Vec::new();
        while let Some(x) = stack.pop() {
            if let Some(e) = self.elems.get(&x) {
                stack.extend(e.children.iter().copied());
                doomed.push(x);
            }
        }
        let parent = self.elems.get(&id).and_then(|e| e.parent);
        match parent {
            Some(p) => {
                if let Some(pe) = self.elems.get_mut(&p) {
                    pe.children.retain(|c| *c != id);
                }
            }
            None => self.roots.retain(|r| *r != id),
        }
        for d in &doomed {
            self.elems.remove(d);
            if self.hover_hit == Some(*d) {
                self.hover_hit = None;
            }
            if self.press_target == Some(*d) {
                self.press_target = None;
            }
        }
        // Drop queued events for destroyed elements.
        let mut w = 0;
        for i in 0..self.n_events {
            if self.elems.contains_key(&self.events[i].0) {
                self.events[w] = self.events[i];
                w += 1;
            }
        }
        self.n_events = w;
        self.structure_changed = true;
        Ok(())
    }

    /// `ref[key]`: a named `add` argument of a live element.
    pub fn prop(&self, id: u32, key: &str) -> Result<VValue, String> {
        let e = self.elems.get(&id).ok_or_else(|| format!("element #{} does not exist (destroyed?)", id))?;
        match key {
            "kind" => Ok(VValue::String(e.kind.name().to_string())),
            "x" => Ok(VValue::Integer(e.rect.x as i64)),
            "y" => Ok(VValue::Integer(e.rect.y as i64)),
            "width" => Ok(VValue::Integer(e.rect.w as i64)),
            "height" => Ok(VValue::Integer(e.rect.h as i64)),
            "hovered" => Ok(VValue::Boolean(e.hovered)),
            _ => e.scope.get(key).cloned().ok_or_else(|| format!("element #{} has no property '{}'", id, key)),
        }
    }

    /// A copy of every element's scope, so live expressions can read `ref[...]`
    /// of other elements while the tree is borrowed.
    pub fn scopes(&self) -> BTreeMap<u32, BTreeMap<String, VValue>> {
        self.elems.iter().map(|(k, e)| (*k, e.scope.clone())).collect()
    }

    // ---------------- live evaluation ----------------

    /// Re-evaluate live properties if a variable was written since last time
    /// (or, for elements that call time(), when `now_sec` changed). Returns
    /// true if anything visible changed.
    pub fn refresh(&mut self, ev: &mut dyn Evaluator, now_sec: u64) -> bool {
        let generation = ev.generation();
        let all = generation != self.gen_seen;
        let tick = now_sec != self.time_seen;
        if !all && !tick && !self.structure_changed {
            return false;
        }
        self.gen_seen = generation;
        self.time_seen = now_sec;
        let mut changed = self.structure_changed;
        let ids: Vec<u32> = self.elems.keys().copied().collect();
        let mut errs: Vec<String> = Vec::new();
        for id in ids {
            let e = match self.elems.get_mut(&id) {
                Some(e) => e,
                None => continue,
            };
            if !(all || !e.evaluated || (tick && e.uses_time)) {
                continue;
            }
            let mut base = Live::defaults();
            let mut fail = |k: &str, msg: String, l: &mut Live, kind: Kind| {
                errs.push(format!("element.{} #{} '{}': {}", kind.name(), id, k, msg));
                l.on_error(k);
            };
            for (k, x) in &e.live_exprs {
                match ev.eval(x, &e.scope) {
                    Ok(v) => {
                        if let Err(m) = base.apply(k, &v) {
                            fail(k, m, &mut base, e.kind);
                        }
                    }
                    Err(m) => fail(k, m, &mut base, e.kind),
                }
            }
            let mut hover = base.clone();
            if e.hover_exprs.is_empty() {
                if e.kind == Kind::Button {
                    hover.wash = 20;
                }
            } else {
                for (k, x) in &e.hover_exprs {
                    match ev.eval(x, &e.scope) {
                        Ok(v) => {
                            if let Err(m) = hover.apply(k, &v) {
                                fail(k, m, &mut hover, e.kind);
                            }
                        }
                        Err(m) => fail(k, m, &mut hover, e.kind),
                    }
                }
            }
            let mut pressed = hover.clone();
            if e.pressed_exprs.is_empty() {
                if e.kind == Kind::Button && e.hover_exprs.is_empty() {
                    pressed.wash = 36;
                }
            } else {
                for (k, x) in &e.pressed_exprs {
                    match ev.eval(x, &e.scope) {
                        Ok(v) => {
                            if let Err(m) = pressed.apply(k, &v) {
                                fail(k, m, &mut pressed, e.kind);
                            }
                        }
                        Err(m) => fail(k, m, &mut pressed, e.kind),
                    }
                }
            }
            if base != e.live || hover != e.live_hover || pressed != e.live_pressed || !e.evaluated {
                changed = true;
            }
            e.live = base;
            e.live_hover = hover;
            e.live_pressed = pressed;
            e.evaluated = true;
        }
        for m in errs {
            self.error(m);
        }
        self.structure_changed = false;
        changed
    }

    // ---------------- layout ----------------

    fn text_size(e: &Elem, live: &Live, fonts: Option<&FontLibrary>) -> (i32, i32) {
        if live.text.is_empty() {
            return (0, 0);
        }
        match fonts.and_then(|l| l.pick(&live.font_family, live.font_weight, false)) {
            Some(f) => {
                let mut st = TextStyle::px(live.font_size);
                if e.snap.tabular {
                    st = st.tabular();
                }
                let w = (measure(f, &live.text, &st) + 63) / 64;
                let m = f.metrics(st.size_q6);
                (w, (m.ascent_q6 + m.descent_q6 + 63) / 64)
            }
            // Without fonts: the 8x8 legacy cell, so layout still makes sense.
            None => (live.text.chars().count() as i32 * 9, 10),
        }
    }

    fn content_pad(e: &Elem) -> [i32; 4] {
        let p = e.snap.padding;
        match &e.snap.flow {
            Some(f) => [p[0] + f.padding[0], p[1] + f.padding[1], p[2] + f.padding[2], p[3] + f.padding[3]],
            None => p,
        }
    }

    fn in_flow(e: &Elem) -> bool {
        e.snap.position.is_none()
    }

    /// Preferred size of an element given the space its parent offers.
    fn measure(&self, id: u32, avail: (i32, i32), fonts: Option<&FontLibrary>, depth: u32) -> (i32, i32) {
        let e = match self.elems.get(&id) {
            Some(e) => e,
            None => return (0, 0),
        };
        if !e.live.visible || depth > 64 {
            return (0, 0);
        }
        let (dw, dh) = e.snap.size;
        let fixed_w = if dw.is_auto() { None } else { Some(dw.resolve(avail.0, 0)) };
        let fixed_h = if dh.is_auto() { None } else { Some(dh.resolve(avail.1, 0)) };
        let (mut w, mut h) = (fixed_w.unwrap_or(0), fixed_h.unwrap_or(0));
        if fixed_w.is_none() || fixed_h.is_none() {
            let (iw, ih) = self.intrinsic(e, (fixed_w.unwrap_or(avail.0), fixed_h.unwrap_or(avail.1)), fonts, depth);
            if fixed_w.is_none() {
                w = iw;
            }
            if fixed_h.is_none() {
                h = ih;
            }
        }
        let w = w.max(e.snap.min.0).min(e.snap.max.0).max(0);
        let h = h.max(e.snap.min.1).min(e.snap.max.1).max(0);
        (w, h)
    }

    fn intrinsic(&self, e: &Elem, avail: (i32, i32), fonts: Option<&FontLibrary>, depth: u32) -> (i32, i32) {
        let pad = Self::content_pad(e);
        let (px, py) = (pad[1] + pad[3], pad[0] + pad[2]);
        if e.kind.has_text() && e.children.is_empty() {
            let (tw, th) = Self::text_size(e, &e.live, fonts);
            return (tw + px, th + py);
        }
        if e.kind == Kind::Divider && e.children.is_empty() {
            return (1, 1);
        }
        let inner = ((avail.0 - px).max(0), (avail.1 - py).max(0));
        let kids: Vec<(i32, i32)> = e
            .children
            .iter()
            .filter_map(|c| self.elems.get(c))
            .filter(|c| c.live.visible && Self::in_flow(c))
            .map(|c| self.measure(c.id, inner, fonts, depth + 1))
            .collect();
        if kids.is_empty() {
            return (px, py);
        }
        match &e.snap.flow {
            Some(f) => {
                let gaps = f.gap * (kids.len() as i32 - 1);
                if f.horizontal {
                    (kids.iter().map(|k| k.0).sum::<i32>() + gaps + px, kids.iter().map(|k| k.1).max().unwrap_or(0) + py)
                } else {
                    (kids.iter().map(|k| k.0).max().unwrap_or(0) + px, kids.iter().map(|k| k.1).sum::<i32>() + gaps + py)
                }
            }
            None => (kids.iter().map(|k| k.0).max().unwrap_or(0) + px, kids.iter().map(|k| k.1).max().unwrap_or(0) + py),
        }
    }

    fn place_abs(e: &Elem, parent: IRect, size: (i32, i32)) -> IRect {
        let (dx, dy) = e.snap.position.unwrap_or((Dim::px(0), Dim::px(0)));
        let ox = dx.resolve(parent.w, 0);
        let oy = dy.resolve(parent.h, 0);
        let x = match e.snap.anchor.0 {
            HAnchor::Left => parent.x + ox,
            HAnchor::Right => parent.right() - ox - size.0,
            HAnchor::Center => parent.x + (parent.w - size.0) / 2 + ox,
        };
        let y = match e.snap.anchor.1 {
            VAnchor::Top => parent.y + oy,
            VAnchor::Bottom => parent.bottom() - oy - size.1,
            VAnchor::Center => parent.y + (parent.h - size.1) / 2 + oy,
        };
        IRect::new(x, y, size.0, size.1)
    }

    fn arrange(&self, id: u32, rect: IRect, fonts: Option<&FontLibrary>, out: &mut Vec<(u32, IRect)>, depth: u32) {
        let e = match self.elems.get(&id) {
            Some(e) => e,
            None => return,
        };
        out.push((id, rect));
        if depth > 64 {
            return;
        }
        let pad = Self::content_pad(e);
        let content = IRect::new(rect.x + pad[3], rect.y + pad[0], (rect.w - pad[1] - pad[3]).max(0), (rect.h - pad[0] - pad[2]).max(0));
        // Positioned children: relative to this element's box.
        let mut flow_kids: Vec<(u32, (i32, i32))> = Vec::new();
        for c in &e.children {
            let ce = match self.elems.get(c) {
                Some(x) => x,
                None => continue,
            };
            if !ce.live.visible {
                continue;
            }
            if !Self::in_flow(ce) {
                let size = self.measure(*c, (rect.w, rect.h), fonts, depth + 1);
                self.arrange(*c, Self::place_abs(ce, rect, size), fonts, out, depth + 1);
            } else {
                flow_kids.push((*c, self.measure(*c, (content.w, content.h), fonts, depth + 1)));
            }
        }
        if flow_kids.is_empty() {
            return;
        }
        let flow = match &e.snap.flow {
            Some(f) => f.clone(),
            None => {
                // No flow: every child sits at the content origin.
                for (c, s) in flow_kids {
                    self.arrange(c, IRect::new(content.x, content.y, s.0, s.1), fonts, out, depth + 1);
                }
                return;
            }
        };
        let n = flow_kids.len() as i32;
        let (main_len, cross_len) = if flow.horizontal { (content.w, content.h) } else { (content.h, content.w) };
        let total: i32 = flow_kids.iter().map(|(_, s)| if flow.horizontal { s.0 } else { s.1 }).sum::<i32>() + flow.gap * (n - 1);
        let free = (main_len - total).max(0);
        let (mut cursor, extra) = match flow.justify {
            Justify::Start => (0, 0),
            Justify::Center => (free / 2, 0),
            Justify::End => (free, 0),
            Justify::SpaceBetween => (0, if n > 1 { free / (n - 1) } else { 0 }),
            Justify::SpaceAround => (free / (2 * n), free / n),
            Justify::SpaceEvenly => (free / (n + 1), free / (n + 1)),
        };
        if flow.justify == Justify::SpaceBetween && n == 1 {
            cursor = 0;
        }
        for (i, (c, s)) in flow_kids.iter().enumerate() {
            let ce = &self.elems[c];
            let (mut cw, mut ch) = *s;
            let stretch_cross = flow.align == Align::Stretch
                && if flow.horizontal { ce.snap.size.1.is_auto() } else { ce.snap.size.0.is_auto() };
            if stretch_cross {
                if flow.horizontal {
                    ch = cross_len;
                } else {
                    cw = cross_len;
                }
            }
            let (m_size, c_size) = if flow.horizontal { (cw, ch) } else { (ch, cw) };
            let cross_off = match flow.align {
                Align::Start | Align::Stretch => 0,
                Align::Center => (cross_len - c_size) / 2,
                Align::End => cross_len - c_size,
            };
            let r = if flow.horizontal {
                IRect::new(content.x + cursor, content.y + cross_off, cw, ch)
            } else {
                IRect::new(content.x + cross_off, content.y + cursor, cw, ch)
            };
            self.arrange(*c, r, fonts, out, depth + 1);
            cursor += m_size + flow.gap;
            if i + 1 < flow_kids.len() {
                cursor += extra;
            }
        }
    }

    /// Lay out the whole tree for a screen, rebuild the paint order. Returns
    /// true if any element moved or resized.
    pub fn layout(&mut self, screen: (i32, i32), fonts: Option<&FontLibrary>) -> bool {
        self.screen = screen;
        let mut out: Vec<(u32, IRect)> = Vec::new();
        let screen_rect = IRect::new(0, 0, screen.0, screen.1);
        for r in self.roots.clone() {
            let e = match self.elems.get(&r) {
                Some(e) => e,
                None => continue,
            };
            if !e.live.visible {
                continue;
            }
            let size = self.measure(r, screen, fonts, 0);
            let rect = Self::place_abs(e, screen_rect, size);
            self.arrange(r, rect, fonts, &mut out, 0);
        }
        let mut moved = false;
        let mut placed: BTreeMap<u32, IRect> = BTreeMap::new();
        for (id, r) in out {
            placed.insert(id, r);
        }
        for (id, e) in self.elems.iter_mut() {
            // Elements not placed (hidden, or under a hidden parent) get an empty box.
            let r = placed.get(id).copied().unwrap_or(IRect::EMPTY);
            if r != e.rect {
                moved = true;
                e.rect = r;
            }
        }
        // Paint order: depth first; within siblings by (z, insertion order);
        // negative z draws before (behind) the parent.
        let mut overlay = Vec::new();
        let mut desktop = Vec::new();
        for r in self.roots.clone() {
            if let Some(e) = self.elems.get(&r) {
                if !e.live.visible {
                    continue;
                }
                desktop.push(r);
                let kids = self.sorted_children(r);
                for c in kids {
                    self.collect_order(c, &mut overlay, 0);
                }
            }
        }
        if overlay != self.overlay_order || desktop != self.desktop_order {
            moved = true;
        }
        self.overlay_order = overlay;
        self.desktop_order = desktop;
        moved
    }

    fn sorted_children(&self, id: u32) -> Vec<u32> {
        let mut kids: Vec<(i32, u64, u32)> = match self.elems.get(&id) {
            Some(e) => e.children.iter().filter_map(|c| self.elems.get(c)).map(|c| (c.snap.z, c.seq, c.id)).collect(),
            None => Vec::new(),
        };
        kids.sort();
        kids.into_iter().map(|k| k.2).collect()
    }

    fn collect_order(&self, id: u32, out: &mut Vec<u32>, depth: u32) {
        let e = match self.elems.get(&id) {
            Some(e) => e,
            None => return,
        };
        if !e.live.visible || depth > 64 {
            return;
        }
        let kids = self.sorted_children(id);
        for c in kids.iter().filter(|c| self.elems[c].snap.z < 0) {
            self.collect_order(*c, out, depth + 1);
        }
        out.push(id);
        for c in kids.iter().filter(|c| self.elems[c].snap.z >= 0) {
            self.collect_order(*c, out, depth + 1);
        }
    }

    // ---------------- input (no allocation: safe from the timer ISR) ----------------

    fn hit(&self, x: i32, y: i32) -> Option<u32> {
        for id in self.overlay_order.iter().rev() {
            if let Some(e) = self.elems.get(id) {
                let r = e.rect;
                if x >= r.x && y >= r.y && x < r.right() && y < r.bottom() {
                    return Some(*id);
                }
            }
        }
        None
    }

    fn is_ancestor_or_self(&self, anc: u32, mut node: u32) -> bool {
        for _ in 0..65 {
            if node == anc {
                return true;
            }
            match self.elems.get(&node).and_then(|e| e.parent) {
                Some(p) => node = p,
                None => return false,
            }
        }
        false
    }

    fn push_event(&mut self, id: u32, k: EventKind) {
        if self.n_events < MAX_EVENTS {
            self.events[self.n_events] = (id, k);
            self.n_events += 1;
        }
    }

    /// Feed one pointer sample. `down_edge` / `up_edge` are left-button
    /// transitions. Returns true if the shell took the press (so it must not
    /// reach the windows underneath). Allocation-free.
    pub fn pointer(&mut self, x: i32, y: i32, down_edge: bool, up_edge: bool) -> bool {
        let hit = self.hit(x, y);
        if hit != self.hover_hit {
            // Leave: old chain minus new chain. Enter: new chain minus old chain.
            let old = self.hover_hit;
            let mut node = old;
            let mut guard = 0;
            while let Some(n) = node {
                guard += 1;
                if guard > 65 {
                    break;
                }
                let still = hit.map_or(false, |h| self.is_ancestor_or_self(n, h));
                let parent = self.elems.get(&n).and_then(|e| e.parent);
                if !still {
                    let has = self.elems.get(&n).map_or(false, |e| e.on_hover_leave.is_some());
                    if let Some(e) = self.elems.get_mut(&n) {
                        e.hovered = false;
                    }
                    if has {
                        self.push_event(n, EventKind::HoverLeave);
                    }
                }
                node = parent;
            }
            let mut node = hit;
            guard = 0;
            while let Some(n) = node {
                guard += 1;
                if guard > 65 {
                    break;
                }
                let was = old.map_or(false, |o| self.is_ancestor_or_self(n, o));
                let parent = self.elems.get(&n).and_then(|e| e.parent);
                if !was {
                    let has = self.elems.get(&n).map_or(false, |e| e.on_hover_enter.is_some());
                    if let Some(e) = self.elems.get_mut(&n) {
                        e.hovered = true;
                    }
                    if has {
                        self.push_event(n, EventKind::HoverEnter);
                    }
                }
                node = parent;
            }
            self.hover_hit = hit;
            self.state_changed = true;
        }
        if up_edge {
            if let Some(p) = self.press_target.take() {
                if let Some(e) = self.elems.get_mut(&p) {
                    e.pressed = false;
                }
                self.state_changed = true;
            }
        }
        if down_edge {
            if let Some(h) = hit {
                // The click goes to the nearest element (self or ancestor) with on_click.
                let mut node = Some(h);
                let mut target = h;
                let mut guard = 0;
                while let Some(n) = node {
                    guard += 1;
                    if guard > 65 {
                        break;
                    }
                    if self.elems.get(&n).map_or(false, |e| e.on_click.is_some()) {
                        target = n;
                        self.push_event(n, EventKind::Click);
                        break;
                    }
                    node = self.elems.get(&n).and_then(|e| e.parent);
                }
                if let Some(e) = self.elems.get_mut(&target) {
                    e.pressed = true;
                }
                self.press_target = Some(target);
                self.state_changed = true;
                return true;
            }
        }
        false
    }

    /// Handlers to run for queued pointer events, with the element ref as `self`.
    pub fn take_handlers(&mut self) -> Vec<(Handler, VValue)> {
        let mut out = Vec::new();
        for i in 0..self.n_events {
            let (id, k) = self.events[i];
            if let Some(e) = self.elems.get(&id) {
                let h = match k {
                    EventKind::Click => &e.on_click,
                    EventKind::HoverEnter => &e.on_hover_enter,
                    EventKind::HoverLeave => &e.on_hover_leave,
                };
                if let Some(h) = h {
                    out.push((h.clone(), VValue::Ref(id)));
                }
            }
        }
        self.n_events = 0;
        out
    }

    // ---------------- painting ----------------

    fn shadow_margin(e: &Elem) -> i32 {
        match &e.snap.shadow {
            Some(s) => s.blur as i32 + s.blur as i32 / 2 + s.spread.abs() + s.dx.abs().max(s.dy.abs()) + 2,
            None => 0,
        }
    }

    fn look_key(e: &Elem, l: &Live, w: i32, h: i32, fonts_ver: u64) -> u64 {
        let mut hsh = Fnv::new();
        hsh.u32(w as u32);
        hsh.u32(h as u32);
        hsh.u32(e.snap.radius as u32);
        for b in e.snap.border_thickness {
            hsh.u32(b as u32);
        }
        for p in e.snap.padding {
            hsh.u32(p as u32);
        }
        hsh.u32(match e.snap.text_align {
            Some(TextAlign::Left) => 1,
            Some(TextAlign::Center) => 2,
            Some(TextAlign::Right) => 3,
            None => 0,
        });
        hsh.u32(e.snap.tabular as u32);
        if let Some(s) = &e.snap.shadow {
            hsh.u32(s.dx as u32);
            hsh.u32(s.dy as u32);
            hsh.u32(s.blur as u32);
            hsh.u32(s.spread as u32);
            hsh.color(s.color);
        }
        match &l.background {
            Fill::None => hsh.u32(0),
            Fill::Solid(c) => {
                hsh.u32(1);
                hsh.color(*c);
            }
            Fill::Linear { angle, stops } => {
                hsh.u32(2);
                hsh.u32(*angle as u32);
                for s in stops {
                    hsh.u32(s.pos as u32);
                    hsh.color(s.color);
                }
            }
            Fill::Radial { cx, cy, stops } => {
                hsh.u32(3);
                hsh.u32(*cx as u32);
                hsh.u32(*cy as u32);
                for s in stops {
                    hsh.u32(s.pos as u32);
                    hsh.color(s.color);
                }
            }
        }
        hsh.color(l.color);
        hsh.color(l.border);
        hsh.u32(l.font_size);
        hsh.u32(l.font_weight as u32);
        hsh.bytes(l.font_family.as_bytes());
        hsh.bytes(l.text.as_bytes());
        hsh.u32(l.wash as u32);
        hsh.u32(fonts_ver as u32);
        hsh.0
    }

    /// Draw one element's body + text with its box at `at` (canvas coordinates).
    #[allow(clippy::too_many_arguments)]
    fn render(e: &Elem, l: &Live, at: IRect, c: &mut Canvas, cache: &mut Cache, fonts: Option<&FontLibrary>, glyphs: &mut GlyphCache) {
        let shape = Shape { rect: at, radii: Radii::uniform(e.snap.radius), rotation: Angle::ZERO };
        if let Some(s) = &e.snap.shadow {
            c.draw_shadow(&shape, s, 255, cache);
        }
        match &l.background {
            Fill::None => {}
            Fill::Solid(col) => c.fill_shape(&shape, &Paint::Solid(*col), 255, cache),
            Fill::Linear { angle, stops } => c.fill_shape(&shape, &Paint::Linear { angle: Angle::degrees(*angle), stops }, 255, cache),
            Fill::Radial { cx, cy, stops } => c.fill_shape(&shape, &Paint::Radial { cx: *cx, cy: *cy, stops }, 255, cache),
        }
        if l.wash > 0 {
            c.fill_shape(&shape, &Paint::Solid(Color32::rgba(255, 255, 255, l.wash)), 255, cache);
        }
        let bt = e.snap.border_thickness;
        if l.border.a > 0 && bt.iter().any(|b| *b > 0) {
            let b = Borders { top: bt[0] as u16, right: bt[1] as u16, bottom: bt[2] as u16, left: bt[3] as u16 };
            c.stroke_shape(&shape, &b, &Paint::Solid(l.border), 255, cache);
        }
        if e.kind.has_text() && !l.text.is_empty() {
            if let Some(f) = fonts.and_then(|lib| lib.pick(&l.font_family, l.font_weight, false)) {
                let p = e.snap.padding;
                let content = IRect::new(at.x + p[3], at.y + p[0], (at.w - p[1] - p[3]).max(0), (at.h - p[0] - p[2]).max(0));
                let mut st = TextStyle::px(l.font_size);
                if e.snap.tabular {
                    st = st.tabular();
                }
                let shown = fit_with_ellipsis(f, &l.text, &st, content.w * 64);
                let tw = (measure(f, &shown, &st) + 63) / 64;
                let align = e.snap.text_align.unwrap_or(if e.kind == Kind::Button { TextAlign::Center } else { TextAlign::Left });
                let x = match align {
                    TextAlign::Left => content.x,
                    TextAlign::Center => content.x + (content.w - tw) / 2,
                    TextAlign::Right => content.right() - tw,
                };
                let baseline = f.baseline_for_centre(content.y + content.h / 2, st.size_q6);
                let prev = c.push_clip(content);
                draw_text(c, glyphs, f, &shown, x, baseline, l.color, 255, &st);
                c.pop_clip(prev);
            }
        }
    }

    fn paint_one(&mut self, id: u32, c: &mut Canvas, cache: &mut Cache, fonts: Option<&FontLibrary>, glyphs: &mut GlyphCache, fonts_ver: u64) {
        let e = match self.elems.get_mut(&id) {
            Some(e) => e,
            None => return,
        };
        let r = e.rect;
        if r.is_empty() {
            return;
        }
        let l = e.current().clone();
        let opacity = l.opacity;
        if opacity == 0 {
            return;
        }
        let m = Self::shadow_margin(e);
        let (lw, lh) = (r.w + 2 * m, r.h + 2 * m);
        let direct = (lw as i64) * (lh as i64) > MAX_CACHED_PIXELS && e.snap.rotation == 0 && opacity == 255;
        if direct {
            Self::render(e, &l, r, c, cache, fonts, glyphs);
            return;
        }
        let key = Self::look_key(e, &l, r.w, r.h, fonts_ver);
        let rot = e.snap.rotation;
        // Take the layer out so the element can be read while the layer is written.
        let mut layer = core::mem::replace(&mut e.layer, CachedLayer::new());
        {
            let er: &Elem = &*e;
            let got = layer.get(lw, lh, key, |lc| {
                Self::render(er, &l, IRect::new(m, m, r.w, r.h), lc, cache, fonts, glyphs);
            });
            if let Some(lay) = got {
                if rot == 0 {
                    c.draw_layer(lay, r.x - m, r.y - m, opacity);
                } else {
                    c.draw_layer_xf(lay, r.x + r.w / 2, r.y + r.h / 2, Angle::degrees(rot), 1000, opacity, true);
                }
            }
        }
        e.layer = layer;
    }

    /// Root bodies: call before the wallpaper is drawn.
    pub fn paint_desktop(&mut self, c: &mut Canvas, cache: &mut Cache, fonts: Option<&FontLibrary>, glyphs: &mut GlyphCache, fonts_ver: u64) {
        let order = self.desktop_order.clone();
        for id in order {
            self.paint_one(id, c, cache, fonts, glyphs, fonts_ver);
        }
    }

    /// Everything under the roots: call after windows are drawn.
    pub fn paint_overlay(&mut self, c: &mut Canvas, cache: &mut Cache, fonts: Option<&FontLibrary>, glyphs: &mut GlyphCache, fonts_ver: u64) {
        let order = self.overlay_order.clone();
        for id in order {
            self.paint_one(id, c, cache, fonts, glyphs, fonts_ver);
        }
    }

    /// Union of all overlay element boxes (for dirty-rect commits).
    pub fn overlay_bounds(&self) -> IRect {
        let mut u = IRect::EMPTY;
        for id in &self.overlay_order {
            if let Some(e) = self.elems.get(id) {
                let m = Self::shadow_margin(e);
                u = u.union(&IRect::new(e.rect.x - m, e.rect.y - m, e.rect.w + 2 * m, e.rect.h + 2 * m));
            }
        }
        u
    }
}

struct Fnv(u64);
impl Fnv {
    fn new() -> Fnv {
        Fnv(0xcbf29ce484222325)
    }
    fn bytes(&mut self, b: &[u8]) {
        for x in b {
            self.0 ^= *x as u64;
            self.0 = self.0.wrapping_mul(0x100000001b3);
        }
        self.u32(b.len() as u32);
    }
    fn u32(&mut self, v: u32) {
        for x in v.to_le_bytes() {
            self.0 ^= x as u64;
            self.0 = self.0.wrapping_mul(0x100000001b3);
        }
    }
    fn color(&mut self, c: Color32) {
        self.u32(u32::from_le_bytes([c.r, c.g, c.b, c.a]));
    }
}

// =====================================================================
// -----| read-only evaluation for live properties |-----
// =====================================================================

/// Functions that act on the OS. They are allowed in handlers and events, never
/// inside a live property (a property must not have side effects).
pub const ACTION_FUNCTIONS: &[&str] = &["launch", "focus_window", "lock_session", "shutdown", "destroy", "print"];

/// Host used while evaluating live properties: reads only.
pub struct ReadOnlyHost<'a> {
    pub scopes: &'a BTreeMap<u32, BTreeMap<String, VValue>>,
    pub now: DateTime,
}

impl<'a> Host for ReadOnlyHost<'a> {
    fn set_system(&mut self, name: &str, _value: &VValue) -> Result<(), String> {
        Err(format!("'set {}' is not allowed inside a live property", name))
    }

    fn call(&mut self, name: &str, args: &[CallArg]) -> Option<Result<VValue, String>> {
        if name == "time" {
            return Some(time_call(args, &self.now));
        }
        if ACTION_FUNCTIONS.contains(&name) {
            return Some(Err(format!("'{}' cannot be called from a live property (call it from a handler)", name)));
        }
        None
    }

    fn element_prop(&mut self, id: u32, key: &str) -> Result<VValue, String> {
        let sc = self.scopes.get(&id).ok_or_else(|| format!("element #{} does not exist (destroyed?)", id))?;
        sc.get(key).cloned().ok_or_else(|| format!("element #{} has no property '{}'", id, key))
    }

    fn is_element_kind(&self, kind: &str) -> bool {
        KINDS.contains(&kind)
    }
}

/// `time()` / `time("%H:%M")`.
pub fn time_call(args: &[CallArg], now: &DateTime) -> Result<VValue, String> {
    match args.len() {
        0 => Ok(VValue::String(format_time("%H:%M", now))),
        1 => match &args[0].value {
            VValue::String(f) => Ok(VValue::String(format_time(f, now))),
            other => Err(format!("time(format): format must be a string, got {}", other.type_name())),
        },
        _ => Err(String::from("time() takes at most 1 argument")),
    }
}

/// The interpreter as an `Evaluator` for `Shell::refresh`.
pub struct Eval<'a> {
    pub interp: &'a mut Interp,
    pub scopes: BTreeMap<u32, BTreeMap<String, VValue>>,
    pub now: DateTime,
}

impl<'a> Evaluator for Eval<'a> {
    fn eval(&mut self, expr: &Expr, scope: &BTreeMap<String, VValue>) -> Result<VValue, String> {
        let mut host = ReadOnlyHost { scopes: &self.scopes, now: self.now };
        self.interp.eval_in(expr, scope, &mut host)
    }
    fn generation(&self) -> u64 {
        self.interp.generation
    }
}

// =====================================================================
// -----| time() formatting |-----
// =====================================================================

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct DateTime {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
}

/// strftime subset: %H %M %S %I %p %d %m %y %Y %%. Unknown codes are kept literally.
pub fn format_time(fmt: &str, t: &DateTime) -> String {
    let mut out = String::new();
    let mut it = fmt.chars();
    while let Some(ch) = it.next() {
        if ch != '%' {
            out.push(ch);
            continue;
        }
        match it.next() {
            Some('H') => out.push_str(&format!("{:02}", t.hour)),
            Some('M') => out.push_str(&format!("{:02}", t.minute)),
            Some('S') => out.push_str(&format!("{:02}", t.second)),
            Some('I') => out.push_str(&format!("{:02}", if t.hour % 12 == 0 { 12 } else { t.hour % 12 })),
            Some('p') => out.push_str(if t.hour < 12 { "AM" } else { "PM" }),
            Some('d') => out.push_str(&format!("{:02}", t.day)),
            Some('m') => out.push_str(&format!("{:02}", t.month)),
            Some('y') => out.push_str(&format!("{:02}", t.year % 100)),
            Some('Y') => out.push_str(&format!("{}", t.year)),
            Some('%') => out.push('%'),
            Some(o) => {
                out.push('%');
                out.push(o);
            }
            None => out.push('%'),
        }
    }
    out
}

#[allow(dead_code)]
fn _unused(_: &dyn ToString) {}
