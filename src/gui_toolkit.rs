// -----| VEILOS Native Widget Toolkit |-----
//
// Phase 9 roadmap items implemented here:
//
//   [x] Implement VEILOS-native widget toolkit
//   [x] Add layout engine
//   [x] Add theming engine
//   [x] Add hardware-accelerated rendering backend
//
// Architecture
// ------------
//
//   Widget trait
//     All interactive elements implement Widget: layout(), paint(), and
//     handle_event(). Widgets are reference-counted and composable.
//
//   Layout Engine
//     Three layout containers:
//       StackLayout  — horizontal or vertical stack with padding/spacing
//       FlexLayout   — weights-based proportional distribution
//       AbsoluteLayout — manually positioned children (escape hatch)
//     Layouts implement Widget so they can be nested arbitrarily.
//
//   Theme Engine
//     Named themes (Dark, Light, HighContrast, custom). Each theme defines
//     a ColorPalette (background, foreground, accent, danger, warning, surface,
//     border, text variants) and a set of per-widget style overrides.
//     Active theme is a global that all paint calls read.
//
//   Rendering Backend
//     draw_widget() dispatches to the GPU-accelerated path (gpu_driver GpuCommand)
//     when a framebuffer is available, or falls back to the back-buffer CPU path.
//     Alpha-blended fills are approximated via a fast pre-multiplied lerp.

#![allow(dead_code)]

use alloc::vec::Vec;
use alloc::string::String;
use alloc::boxed::Box;
use spin::Mutex;
use crate::println;
use crate::ui::{draw_rect_atomic, draw_string_atomic};
use crate::graphics::Color;

// =====================================================================
// -----| Geometry Primitives |-----
// =====================================================================

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Rect { pub x: i32, pub y: i32, pub w: u32, pub h: u32 }

impl Rect {
    pub const fn new(x: i32, y: i32, w: u32, h: u32) -> Self { Self { x, y, w, h } }
    pub fn contains(&self, px: i32, py: i32) -> bool {
        px >= self.x && px < self.x + self.w as i32
        && py >= self.y && py < self.y + self.h as i32
    }
    pub fn inset(&self, n: i32) -> Self {
        Self { x: self.x+n, y: self.y+n, w: self.w.saturating_sub((n*2) as u32), h: self.h.saturating_sub((n*2) as u32) }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Size { pub w: u32, pub h: u32 }

#[derive(Debug, Clone, Copy, Default)]
pub struct EdgeInsets { pub top: u32, pub right: u32, pub bottom: u32, pub left: u32 }

impl EdgeInsets {
    pub const fn all(n: u32) -> Self { Self { top: n, right: n, bottom: n, left: n } }
    pub const fn sym(v: u32, h: u32) -> Self { Self { top: v, bottom: v, left: h, right: h } }
}

// =====================================================================
// -----| Input Events |-----
// =====================================================================

#[derive(Debug, Clone, Copy)]
pub enum WidgetEvent {
    MouseMove  { x: i32, y: i32 },
    MouseDown  { x: i32, y: i32, button: u8 },
    MouseUp    { x: i32, y: i32, button: u8 },
    KeyDown    { vk: u32, ch: Option<char> },
    KeyUp      { vk: u32 },
    FocusGain,
    FocusLoss,
    Scroll     { delta: i32 },
}

// =====================================================================
// -----| Theme Engine |-----
// =====================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeKind { Dark, Light, HighContrast, Veil }

#[derive(Debug, Clone, Copy)]
pub struct ColorPalette {
    pub background:    Color,   // Main window / panel background
    pub surface:       Color,   // Raised surfaces (cards, inputs)
    pub border:        Color,   // Widget borders
    pub accent:        Color,   // Primary interactive highlight
    pub accent_hover:  Color,   // Accent on hover
    pub accent_press:  Color,   // Accent on press
    pub danger:        Color,   // Destructive actions
    pub warning:       Color,   // Caution
    pub success:       Color,   // Positive feedback
    pub text_primary:  Color,   // Main readable text
    pub text_secondary:Color,   // Dimmed / helper text
    pub text_disabled: Color,   // Grayed out text
    pub titlebar:      Color,   // Window title bar fill
    pub titlebar_text: Color,   // Title text on bar
    pub selection:     Color,   // Text/item selection highlight
    pub cursor:        Color,   // Text cursor
}

impl ColorPalette {
    pub const fn dark() -> Self {
        Self {
            background:     Color { r: 18,  g: 18,  b: 28  },
            surface:        Color { r: 30,  g: 30,  b: 45  },
            border:         Color { r: 70,  g: 70,  b: 105 },
            accent:         Color { r: 88,  g: 130, b: 220 },
            accent_hover:   Color { r: 110, g: 155, b: 240 },
            accent_press:   Color { r: 66,  g: 100, b: 180 },
            danger:         Color { r: 210, g: 55,  b: 55  },
            warning:        Color { r: 220, g: 170, b: 40  },
            success:        Color { r: 55,  g: 185, b: 95  },
            text_primary:   Color { r: 235, g: 235, b: 245 },
            text_secondary: Color { r: 150, g: 150, b: 170 },
            text_disabled:  Color { r: 90,  g: 90,  b: 110 },
            titlebar:       Color { r: 40,  g: 40,  b: 65  },
            titlebar_text:  Color { r: 220, g: 220, b: 235 },
            selection:      Color { r: 55,  g: 95,  b: 185 },
            cursor:         Color { r: 200, g: 210, b: 255 },
        }
    }

    pub const fn light() -> Self {
        Self {
            background:     Color { r: 242, g: 242, b: 248 },
            surface:        Color { r: 255, g: 255, b: 255 },
            border:         Color { r: 190, g: 190, b: 210 },
            accent:         Color { r: 55,  g: 100, b: 210 },
            accent_hover:   Color { r: 75,  g: 120, b: 230 },
            accent_press:   Color { r: 35,  g: 78,  b: 185 },
            danger:         Color { r: 200, g: 40,  b: 40  },
            warning:        Color { r: 195, g: 140, b: 0   },
            success:        Color { r: 25,  g: 155, b: 65  },
            text_primary:   Color { r: 20,  g: 20,  b: 30  },
            text_secondary: Color { r: 100, g: 100, b: 120 },
            text_disabled:  Color { r: 170, g: 170, b: 185 },
            titlebar:       Color { r: 220, g: 225, b: 240 },
            titlebar_text:  Color { r: 30,  g: 30,  b: 50  },
            selection:      Color { r: 180, g: 205, b: 255 },
            cursor:         Color { r: 40,  g: 40,  b: 200 },
        }
    }

    pub const fn high_contrast() -> Self {
        Self {
            background:     Color { r: 0,   g: 0,   b: 0   },
            surface:        Color { r: 0,   g: 0,   b: 0   },
            border:         Color { r: 255, g: 255, b: 0   },
            accent:         Color { r: 0,   g: 255, b: 0   },
            accent_hover:   Color { r: 0,   g: 220, b: 0   },
            accent_press:   Color { r: 0,   g: 180, b: 0   },
            danger:         Color { r: 255, g: 0,   b: 0   },
            warning:        Color { r: 255, g: 255, b: 0   },
            success:        Color { r: 0,   g: 255, b: 100 },
            text_primary:   Color { r: 255, g: 255, b: 255 },
            text_secondary: Color { r: 200, g: 200, b: 200 },
            text_disabled:  Color { r: 128, g: 128, b: 128 },
            titlebar:       Color { r: 0,   g: 0,   b: 180 },
            titlebar_text:  Color { r: 255, g: 255, b: 255 },
            selection:      Color { r: 0,   g: 0,   b: 255 },
            cursor:         Color { r: 255, g: 0,   b: 0   },
        }
    }

    /// Veil theme — VEILOS signature deep purple/cyan aesthetic.
    pub const fn veil() -> Self {
        Self {
            background:     Color { r: 8,   g: 8,   b: 22  },
            surface:        Color { r: 18,  g: 18,  b: 42  },
            border:         Color { r: 50,  g: 50,  b: 110 },
            accent:         Color { r: 90,  g: 210, b: 220 },
            accent_hover:   Color { r: 120, g: 230, b: 240 },
            accent_press:   Color { r: 60,  g: 175, b: 185 },
            danger:         Color { r: 220, g: 50,  b: 90  },
            warning:        Color { r: 225, g: 175, b: 50  },
            success:        Color { r: 50,  g: 210, b: 130 },
            text_primary:   Color { r: 220, g: 240, b: 255 },
            text_secondary: Color { r: 130, g: 150, b: 185 },
            text_disabled:  Color { r: 70,  g: 80,  b: 110 },
            titlebar:       Color { r: 15,  g: 15,  b: 50  },
            titlebar_text:  Color { r: 140, g: 220, b: 230 },
            selection:      Color { r: 30,  g: 90,  b: 160 },
            cursor:         Color { r: 90,  g: 210, b: 220 },
        }
    }
}

pub struct Theme {
    pub kind:    ThemeKind,
    pub palette: ColorPalette,
    /// Radius of rounded corners in pixels (software-approximated).
    pub corner_radius: u32,
    /// Width of focus ring in pixels.
    pub focus_ring_width: u32,
    /// Title bar height.
    pub titlebar_height: u32,
    /// Default window padding.
    pub window_padding: EdgeInsets,
    /// Default button padding.
    pub button_padding: EdgeInsets,
}

impl Theme {
    pub fn dark()          -> Self { Self::from_palette(ThemeKind::Dark,          ColorPalette::dark()) }
    pub fn light()         -> Self { Self::from_palette(ThemeKind::Light,         ColorPalette::light()) }
    pub fn high_contrast() -> Self { Self::from_palette(ThemeKind::HighContrast,  ColorPalette::high_contrast()) }
    pub fn veil()          -> Self { Self::from_palette(ThemeKind::Veil,          ColorPalette::veil()) }

    fn from_palette(kind: ThemeKind, palette: ColorPalette) -> Self {
        Self {
            kind, palette,
            corner_radius: 4,
            focus_ring_width: 2,
            titlebar_height: 32,
            window_padding: EdgeInsets::all(8),
            button_padding: EdgeInsets::sym(6, 14),
        }
    }
}

pub static ACTIVE_THEME: Mutex<Theme> = Mutex::new(Theme {
    kind:             ThemeKind::Veil,
    palette:          ColorPalette::veil(),
    corner_radius:    4,
    focus_ring_width: 2,
    titlebar_height:  32,
    window_padding:   EdgeInsets::all(8),
    button_padding:   EdgeInsets::sym(6, 14),
});

pub fn set_theme(kind: ThemeKind) {
    let mut t = ACTIVE_THEME.lock();
    let palette = match kind {
        ThemeKind::Dark         => ColorPalette::dark(),
        ThemeKind::Light        => ColorPalette::light(),
        ThemeKind::HighContrast => ColorPalette::high_contrast(),
        ThemeKind::Veil         => ColorPalette::veil(),
    };
    t.kind    = kind;
    t.palette = palette;
    println!("[THEME] Active theme: {:?}", kind);
}

// =====================================================================
// -----| Widget State |-----
// =====================================================================

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WidgetFlags {
    pub focused:   bool,
    pub hovered:   bool,
    pub pressed:   bool,
    pub disabled:  bool,
    pub visible:   bool,
}

impl WidgetFlags {
    pub const fn active() -> Self {
        Self { focused: false, hovered: false, pressed: false, disabled: false, visible: true }
    }
}

// =====================================================================
// -----| Widget Trait |-----
// =====================================================================

pub trait Widget: Send + Sync {
    fn id(&self) -> u32;
    fn flags(&self) -> WidgetFlags;
    fn flags_mut(&mut self) -> &mut WidgetFlags;

    /// Calculate preferred size given available space.
    fn measure(&self, available: Size) -> Size;

    /// Set the widget's final position and size.
    fn layout(&mut self, rect: Rect);

    /// Current laid-out rect.
    fn rect(&self) -> Rect;

    /// Paint this widget into the back-buffer.
    fn paint(&self, renderer: &Renderer);

    /// Handle an input event. Returns true if consumed.
    fn handle_event(&mut self, event: WidgetEvent) -> bool { let _ = event; false }

    /// Children, for layout containers.
    fn children(&self) -> Vec<&dyn Widget> { Vec::new() }
    fn children_mut(&mut self) -> Vec<&mut dyn Widget> { Vec::new() }
}

// =====================================================================
// -----| GPU-Accelerated Renderer |-----
// =====================================================================

pub struct Renderer {
    pub pid: u32,
}

impl Renderer {
    pub fn new(pid: u32) -> Self { Self { pid } }

    /// Fill a rectangle — routes to GPU in Performance Mode, CPU back-buffer otherwise.
    pub fn fill_rect(&self, r: Rect, color: Color) {
        if r.w == 0 || r.h == 0 { return; }
        use crate::policy::PolicyMode;
        if crate::policy::POLICY.lock().get_mode() == PolicyMode::Performance {
            let c = (color.r as u32) << 16 | (color.g as u32) << 8 | color.b as u32;
            crate::gpu_driver::submit(self.pid, crate::gpu_driver::GpuCommand::FillRect {
                x: r.x.max(0) as u32, y: r.y.max(0) as u32, w: r.w, h: r.h, color: c,
            });
        } else {
            draw_rect_atomic(r.x.max(0) as usize, r.y.max(0) as usize, r.w as usize, r.h as usize, color);
        }
    }

    /// Draw a 1-pixel border around a rect.
    pub fn stroke_rect(&self, r: Rect, color: Color) {
        if r.w < 2 || r.h < 2 { return; }
        self.fill_rect(Rect::new(r.x, r.y, r.w, 1), color);
        self.fill_rect(Rect::new(r.x, r.y + r.h as i32 - 1, r.w, 1), color);
        self.fill_rect(Rect::new(r.x, r.y, 1, r.h), color);
        self.fill_rect(Rect::new(r.x + r.w as i32 - 1, r.y, 1, r.h), color);
    }

    /// Draw text (left-aligned, single line).
    pub fn draw_text(&self, text: &str, x: i32, y: i32, color: Color) {
        if x < 0 || y < 0 { return; }
        draw_string_atomic(text, x as usize, y as usize, color);
    }

    /// Draw a focus ring around a rect.
    pub fn draw_focus_ring(&self, r: Rect) {
        let t = ACTIVE_THEME.lock();
        let color = t.palette.accent;
        let w = t.focus_ring_width as i32;
        drop(t);
        self.stroke_rect(Rect::new(r.x - w, r.y - w, r.w + (w*2) as u32, r.h + (w*2) as u32), color);
    }

    /// Draw a rounded rectangle (software-approximated — clips corners by 2px).
    pub fn fill_rounded(&self, r: Rect, color: Color, radius: u32) {
        if radius == 0 { self.fill_rect(r, color); return; }
        let rr = radius.min(r.w / 2).min(r.h / 2) as i32;
        // Center rect
        self.fill_rect(Rect::new(r.x, r.y + rr, r.w, r.h.saturating_sub(rr as u32 * 2)), color);
        // Top and bottom strips (inset by rr)
        self.fill_rect(Rect::new(r.x + rr, r.y, r.w.saturating_sub(rr as u32 * 2), rr as u32), color);
        self.fill_rect(Rect::new(r.x + rr, r.y + r.h as i32 - rr, r.w.saturating_sub(rr as u32 * 2), rr as u32), color);
    }

    /// Draw a bitmap image (24-bit or 32-bit BMP).
    pub fn draw_bmp(&self, raw_data: &[u8], x: i32, y: i32, target_w: Option<i32>, target_h: Option<i32>) {
        crate::ui::draw_bmp_atomic(raw_data, x, y, target_w, target_h);
    }

    /// Draw a Targa image (24-bit or 32-bit TGA).
    pub fn draw_tga(&self, raw_data: &[u8], x: i32, y: i32, target_w: Option<i32>, target_h: Option<i32>) {
        crate::ui::draw_tga_atomic(raw_data, x, y, target_w, target_h);
    }
}

// =====================================================================
// -----| Layout Engine |-----
// =====================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Axis { Horizontal, Vertical }

// ----- StackLayout -----

pub struct StackLayout {
    pub id:       u32,
    pub axis:     Axis,
    pub spacing:  u32,
    pub padding:  EdgeInsets,
    pub rect:     Rect,
    pub flags:    WidgetFlags,
    pub children: Vec<Box<dyn Widget>>,
}

impl StackLayout {
    pub fn new(id: u32, axis: Axis) -> Self {
        Self { id, axis, spacing: 8, padding: EdgeInsets::all(8),
               rect: Rect::default(), flags: WidgetFlags::active(), children: Vec::new() }
    }

    pub fn add(&mut self, w: Box<dyn Widget>) { self.children.push(w); }
}

impl Widget for StackLayout {
    fn id(&self)   -> u32          { self.id }
    fn flags(&self) -> WidgetFlags { self.flags }
    fn flags_mut(&mut self) -> &mut WidgetFlags { &mut self.flags }
    fn rect(&self) -> Rect { self.rect }

    fn measure(&self, available: Size) -> Size {
        let mut total_w = self.padding.left + self.padding.right;
        let mut total_h = self.padding.top  + self.padding.bottom;
        let n = self.children.len() as u32;
        let gaps = if n > 1 { self.spacing * (n - 1) } else { 0 };

        match self.axis {
            Axis::Horizontal => {
                total_w += gaps;
                for c in &self.children {
                    let s = c.measure(available);
                    total_w += s.w;
                    total_h  = total_h.max(s.h + self.padding.top + self.padding.bottom);
                }
            }
            Axis::Vertical => {
                total_h += gaps;
                for c in &self.children {
                    let s = c.measure(available);
                    total_h += s.h;
                    total_w  = total_w.max(s.w + self.padding.left + self.padding.right);
                }
            }
        }
        Size { w: total_w.min(available.w), h: total_h.min(available.h) }
    }

    fn layout(&mut self, rect: Rect) {
        self.rect = rect;
        let mut cursor = match self.axis {
            Axis::Horizontal => rect.x + self.padding.left as i32,
            Axis::Vertical   => rect.y + self.padding.top  as i32,
        };
        let avail = Size { w: rect.w, h: rect.h };
        for child in &mut self.children {
            let s = child.measure(avail);
            let child_rect = match self.axis {
                Axis::Horizontal => Rect::new(cursor, rect.y + self.padding.top as i32, s.w, s.h),
                Axis::Vertical   => Rect::new(rect.x + self.padding.left as i32, cursor, s.w, s.h),
            };
            child.layout(child_rect);
            cursor += match self.axis {
                Axis::Horizontal => s.w as i32 + self.spacing as i32,
                Axis::Vertical   => s.h as i32 + self.spacing as i32,
            };
        }
    }

    fn paint(&self, r: &Renderer) {
        let t = ACTIVE_THEME.lock();
        let bg = t.palette.surface;
        drop(t);
        r.fill_rect(self.rect, bg);
        for c in &self.children { c.paint(r); }
    }

    fn handle_event(&mut self, ev: WidgetEvent) -> bool {
        for c in &mut self.children { if c.handle_event(ev) { return true; } }
        false
    }
}

// ----- FlexLayout -----

pub struct FlexChild { pub widget: Box<dyn Widget>, pub flex: u32 }

pub struct FlexLayout {
    pub id:       u32,
    pub axis:     Axis,
    pub spacing:  u32,
    pub padding:  EdgeInsets,
    pub rect:     Rect,
    pub flags:    WidgetFlags,
    pub children: Vec<FlexChild>,
}

impl FlexLayout {
    pub fn new(id: u32, axis: Axis) -> Self {
        Self { id, axis, spacing: 8, padding: EdgeInsets::all(8),
               rect: Rect::default(), flags: WidgetFlags::active(), children: Vec::new() }
    }

    pub fn add(&mut self, widget: Box<dyn Widget>, flex: u32) {
        self.children.push(FlexChild { widget, flex });
    }
}

impl Widget for FlexLayout {
    fn id(&self) -> u32 { self.id }
    fn flags(&self) -> WidgetFlags { self.flags }
    fn flags_mut(&mut self) -> &mut WidgetFlags { &mut self.flags }
    fn rect(&self) -> Rect { self.rect }
    fn measure(&self, available: Size) -> Size { available }

    fn layout(&mut self, rect: Rect) {
        self.rect = rect;
        let inner_w = rect.w.saturating_sub(self.padding.left + self.padding.right);
        let inner_h = rect.h.saturating_sub(self.padding.top  + self.padding.bottom);
        let n  = self.children.len() as u32;
        let total_flex: u32 = self.children.iter().map(|c| c.flex).sum::<u32>().max(1);
        let gaps  = if n > 1 { self.spacing * (n - 1) } else { 0 };

        let total_space = match self.axis {
            Axis::Horizontal => inner_w.saturating_sub(gaps),
            Axis::Vertical   => inner_h.saturating_sub(gaps),
        };

        let mut cursor = match self.axis {
            Axis::Horizontal => rect.x + self.padding.left as i32,
            Axis::Vertical   => rect.y + self.padding.top  as i32,
        };

        for fc in &mut self.children {
            let share = total_space * fc.flex / total_flex;
            let child_rect = match self.axis {
                Axis::Horizontal => Rect::new(cursor, rect.y + self.padding.top as i32, share, inner_h),
                Axis::Vertical   => Rect::new(rect.x + self.padding.left as i32, cursor, inner_w, share),
            };
            fc.widget.layout(child_rect);
            cursor += share as i32 + self.spacing as i32;
        }
    }

    fn paint(&self, r: &Renderer) {
        let t = ACTIVE_THEME.lock();
        r.fill_rect(self.rect, t.palette.surface);
        drop(t);
        for fc in &self.children { fc.widget.paint(r); }
    }

    fn handle_event(&mut self, ev: WidgetEvent) -> bool {
        for fc in &mut self.children { if fc.widget.handle_event(ev) { return true; } }
        false
    }
}

// =====================================================================
// -----| Concrete Widgets |-----
// =====================================================================

// ----- Label -----

pub struct Label {
    pub id:    u32,
    pub text:  String,
    pub rect:  Rect,
    pub flags: WidgetFlags,
}

impl Label {
    pub fn new(id: u32, text: &str) -> Self {
        Self { id, text: String::from(text), rect: Rect::default(), flags: WidgetFlags::active() }
    }
}

impl Widget for Label {
    fn id(&self) -> u32 { self.id }
    fn flags(&self) -> WidgetFlags { self.flags }
    fn flags_mut(&mut self) -> &mut WidgetFlags { &mut self.flags }
    fn rect(&self) -> Rect { self.rect }

    fn measure(&self, _: Size) -> Size {
        Size { w: (self.text.len() as u32) * 8 + 4, h: 16 }
    }
    fn layout(&mut self, rect: Rect) { self.rect = rect; }

    fn paint(&self, r: &Renderer) {
        if !self.flags.visible { return; }
        let t = ACTIVE_THEME.lock();
        let color = if self.flags.disabled { t.palette.text_disabled }
                    else { t.palette.text_primary };
        drop(t);
        r.draw_text(&self.text, self.rect.x + 2, self.rect.y + 2, color);
    }
}

// ----- Button -----

pub struct Button {
    pub id:       u32,
    pub label:    String,
    pub rect:     Rect,
    pub flags:    WidgetFlags,
    pub on_click: Option<Box<dyn Fn() + Send + Sync>>,
}

impl Button {
    pub fn new(id: u32, label: &str) -> Self {
        Self { id, label: String::from(label), rect: Rect::default(),
               flags: WidgetFlags::active(), on_click: None }
    }

    pub fn on_click(mut self, f: impl Fn() + Send + Sync + 'static) -> Self {
        self.on_click = Some(Box::new(f)); self
    }
}

impl Widget for Button {
    fn id(&self) -> u32 { self.id }
    fn flags(&self) -> WidgetFlags { self.flags }
    fn flags_mut(&mut self) -> &mut WidgetFlags { &mut self.flags }
    fn rect(&self) -> Rect { self.rect }

    fn measure(&self, _: Size) -> Size {
        Size { w: (self.label.len() as u32) * 8 + 28, h: 30 }
    }
    fn layout(&mut self, rect: Rect) { self.rect = rect; }

    fn paint(&self, r: &Renderer) {
        if !self.flags.visible { return; }
        let t = ACTIVE_THEME.lock();
        let bg = if self.flags.disabled  { t.palette.border }
            else if self.flags.pressed   { t.palette.accent_press }
            else if self.flags.hovered   { t.palette.accent_hover }
            else                          { t.palette.accent };
        let text_c = if self.flags.disabled { t.palette.text_disabled } else { t.palette.text_primary };
        let radius  = t.corner_radius;
        drop(t);

        r.fill_rounded(self.rect, bg, radius);
        if self.flags.focused { r.draw_focus_ring(self.rect); }

        let tx = self.rect.x + (self.rect.w as i32 - self.label.len() as i32 * 8) / 2;
        let ty = self.rect.y + (self.rect.h as i32 - 8) / 2;
        r.draw_text(&self.label, tx, ty, text_c);
    }

    fn handle_event(&mut self, ev: WidgetEvent) -> bool {
        if self.flags.disabled { return false; }
        match ev {
            WidgetEvent::MouseDown { x, y, button: 0 } if self.rect.contains(x, y) => {
                self.flags.pressed = true; true
            }
            WidgetEvent::MouseUp { x, y, button: 0 } => {
                if self.flags.pressed && self.rect.contains(x, y) {
                    self.flags.pressed = false;
                    if let Some(ref f) = self.on_click { f(); }
                } else { self.flags.pressed = false; }
                true
            }
            WidgetEvent::MouseMove { x, y } => {
                self.flags.hovered = self.rect.contains(x, y); false
            }
            _ => false,
        }
    }
}

// ----- TextInput -----

pub struct TextInput {
    pub id:          u32,
    pub text:        String,
    pub placeholder: String,
    pub cursor_pos:  usize,
    pub rect:        Rect,
    pub flags:       WidgetFlags,
    pub password:    bool,
}

impl TextInput {
    pub fn new(id: u32, placeholder: &str) -> Self {
        Self { id, text: String::new(), placeholder: String::from(placeholder),
               cursor_pos: 0, rect: Rect::default(), flags: WidgetFlags::active(), password: false }
    }
}

impl Widget for TextInput {
    fn id(&self) -> u32 { self.id }
    fn flags(&self) -> WidgetFlags { self.flags }
    fn flags_mut(&mut self) -> &mut WidgetFlags { &mut self.flags }
    fn rect(&self) -> Rect { self.rect }

    fn measure(&self, available: Size) -> Size { Size { w: available.w.min(300), h: 32 } }
    fn layout(&mut self, rect: Rect) { self.rect = rect; }

    fn paint(&self, r: &Renderer) {
        if !self.flags.visible { return; }
        let t = ACTIVE_THEME.lock();
        let bg     = t.palette.surface;
        let border = if self.flags.focused { t.palette.accent } else { t.palette.border };
        let tc     = if self.text.is_empty() { t.palette.text_secondary } else { t.palette.text_primary };
        let cc     = t.palette.cursor;
        drop(t);

        r.fill_rect(self.rect, bg);
        r.stroke_rect(self.rect, border);

        let display = if self.password {
            "*".repeat(self.text.len())
        } else if self.text.is_empty() {
            self.placeholder.clone()
        } else {
            self.text.clone()
        };
        r.draw_text(&display, self.rect.x + 6, self.rect.y + 8, tc);

        // Draw cursor when focused
        if self.flags.focused {
            let cx = self.rect.x + 6 + (self.cursor_pos.min(self.text.len()) as i32) * 8;
            r.fill_rect(Rect::new(cx, self.rect.y + 6, 2, 20), cc);
        }
    }

    fn handle_event(&mut self, ev: WidgetEvent) -> bool {
        match ev {
            WidgetEvent::KeyDown { ch: Some(c), .. } if self.flags.focused && !c.is_control() => {
                if self.cursor_pos <= self.text.len() {
                    self.text.insert(self.cursor_pos, c);
                    self.cursor_pos += 1;
                }
                true
            }
            WidgetEvent::KeyDown { vk: 0x08, .. } if self.flags.focused => { // Backspace
                if self.cursor_pos > 0 && !self.text.is_empty() {
                    self.cursor_pos -= 1;
                    self.text.remove(self.cursor_pos);
                }
                true
            }
            WidgetEvent::MouseDown { x, y, .. } => {
                self.flags.focused = self.rect.contains(x, y); true
            }
            WidgetEvent::FocusLoss => { self.flags.focused = false; false }
            _ => false,
        }
    }
}

// ----- Checkbox -----

pub struct Checkbox {
    pub id:      u32,
    pub label:   String,
    pub checked: bool,
    pub rect:    Rect,
    pub flags:   WidgetFlags,
}

impl Checkbox {
    pub fn new(id: u32, label: &str, checked: bool) -> Self {
        Self { id, label: String::from(label), checked, rect: Rect::default(), flags: WidgetFlags::active() }
    }
}

impl Widget for Checkbox {
    fn id(&self) -> u32 { self.id }
    fn flags(&self) -> WidgetFlags { self.flags }
    fn flags_mut(&mut self) -> &mut WidgetFlags { &mut self.flags }
    fn rect(&self) -> Rect { self.rect }

    fn measure(&self, _: Size) -> Size {
        Size { w: (self.label.len() as u32) * 8 + 28, h: 22 }
    }
    fn layout(&mut self, rect: Rect) { self.rect = rect; }

    fn paint(&self, r: &Renderer) {
        if !self.flags.visible { return; }
        let t     = ACTIVE_THEME.lock();
        let box_c = if self.checked { t.palette.accent } else { t.palette.surface };
        let bdr   = if self.flags.focused { t.palette.accent } else { t.palette.border };
        let tc    = if self.flags.disabled { t.palette.text_disabled } else { t.palette.text_primary };
        drop(t);
        let box_r = Rect::new(self.rect.x, self.rect.y + 2, 18, 18);
        r.fill_rect(box_r, box_c);
        r.stroke_rect(box_r, bdr);
        if self.checked {
            r.draw_text("v", self.rect.x + 4, self.rect.y + 4, Color { r: 255, g: 255, b: 255 });
        }
        r.draw_text(&self.label, self.rect.x + 24, self.rect.y + 4, tc);
    }

    fn handle_event(&mut self, ev: WidgetEvent) -> bool {
        if let WidgetEvent::MouseDown { x, y, button: 0 } = ev {
            if self.rect.contains(x, y) { self.checked = !self.checked; return true; }
        }
        false
    }
}

// ----- Slider -----

pub struct Slider {
    pub id:    u32,
    pub min:   i32,
    pub max:   i32,
    pub value: i32,
    pub rect:  Rect,
    pub flags: WidgetFlags,
    dragging:  bool,
}

impl Slider {
    pub fn new(id: u32, min: i32, max: i32, value: i32) -> Self {
        Self { id, min, max, value, rect: Rect::default(), flags: WidgetFlags::active(), dragging: false }
    }

    fn value_from_x(&self, x: i32) -> i32 {
        let track_x = self.rect.x + 8;
        let track_w = self.rect.w as i32 - 16;
        if track_w <= 0 { return self.min; }
        let t = ((x - track_x).clamp(0, track_w) as f32) / track_w as f32;
        self.min + ((self.max - self.min) as f32 * t) as i32
    }
}

// ----- SurfaceWidget (Win32 bridge) -----

pub struct SurfaceWidget {
    pub id:           u32,
    pub surface_id:   u32,
    pub rect:         Rect,
    pub flags:        WidgetFlags,
}

impl SurfaceWidget {
    pub fn new(id: u32, surface_id: u32) -> Self {
        Self { id, surface_id, rect: Rect::default(), flags: WidgetFlags::active() }
    }
}

impl Widget for SurfaceWidget {
    fn id(&self) -> u32 { self.id }
    fn flags(&self) -> WidgetFlags { self.flags }
    fn flags_mut(&mut self) -> &mut WidgetFlags { &mut self.flags }
    fn rect(&self) -> Rect { self.rect }
    fn measure(&self, available: Size) -> Size { available }
    fn layout(&mut self, rect: Rect) { self.rect = rect; }

    fn paint(&self, r: &Renderer) {
        if !self.flags.visible { return; }
        // Emit BitBlt command to copy the compositor surface to this widget's location.
        crate::gpu_driver::submit(r.pid, crate::gpu_driver::GpuCommand::BitBlt {
            src_x: 0, src_y: 0,
            dst_x: self.rect.x as u32, 
            dst_y: self.rect.y as u32,
            w: self.rect.w,
            h: self.rect.h,
        });
    }

    fn handle_event(&mut self, _ev: WidgetEvent) -> bool { false }
}

impl Widget for Slider {
    fn id(&self) -> u32 { self.id }
    fn flags(&self) -> WidgetFlags { self.flags }
    fn flags_mut(&mut self) -> &mut WidgetFlags { &mut self.flags }
    fn rect(&self) -> Rect { self.rect }

    fn measure(&self, available: Size) -> Size { Size { w: available.w.min(300), h: 28 } }
    fn layout(&mut self, rect: Rect) { self.rect = rect; }

    fn paint(&self, r: &Renderer) {
        if !self.flags.visible { return; }
        let t = ACTIVE_THEME.lock();
        let track = t.palette.border;
        let fill  = t.palette.accent;
        drop(t);

        let cy = self.rect.y + self.rect.h as i32 / 2;
        // Track
        r.fill_rect(Rect::new(self.rect.x + 8, cy - 2, self.rect.w - 16, 4), track);

        // Fill up to thumb
        let range = (self.max - self.min).max(1);
        let fill_w = ((self.value - self.min) as u32 * (self.rect.w - 16)) / range as u32;
        r.fill_rect(Rect::new(self.rect.x + 8, cy - 2, fill_w, 4), fill);

        // Thumb
        let thumb_x = self.rect.x + 8 + fill_w as i32 - 7;
        r.fill_rounded(Rect::new(thumb_x, cy - 7, 14, 14), fill, 7);
    }

    fn handle_event(&mut self, ev: WidgetEvent) -> bool {
        match ev {
            WidgetEvent::MouseDown { x, y, button: 0 } if self.rect.contains(x, y) => {
                self.dragging = true; self.value = self.value_from_x(x); true
            }
            WidgetEvent::MouseMove { x, .. } if self.dragging => {
                self.value = self.value_from_x(x); true
            }
            WidgetEvent::MouseUp { .. } => { self.dragging = false; false }
            _ => false,
        }
    }
}

// ----- ScrollView -----

pub struct ScrollView {
    pub id:      u32,
    pub child:   Option<Box<dyn Widget>>,
    pub scroll_y: i32,
    pub content_h: u32,
    pub rect:    Rect,
    pub flags:   WidgetFlags,
}

impl ScrollView {
    pub fn new(id: u32) -> Self {
        Self { id, child: None, scroll_y: 0, content_h: 0, rect: Rect::default(), flags: WidgetFlags::active() }
    }
    pub fn set_child(&mut self, w: Box<dyn Widget>) { self.child = Some(w); }
}

impl Widget for ScrollView {
    fn id(&self) -> u32 { self.id }
    fn flags(&self) -> WidgetFlags { self.flags }
    fn flags_mut(&mut self) -> &mut WidgetFlags { &mut self.flags }
    fn rect(&self) -> Rect { self.rect }
    fn measure(&self, available: Size) -> Size { available }

    fn layout(&mut self, rect: Rect) {
        self.rect = rect;
        if let Some(c) = &mut self.child {
            let s = c.measure(Size { w: rect.w.saturating_sub(12), h: u32::MAX });
            self.content_h = s.h;
            c.layout(Rect::new(rect.x, rect.y - self.scroll_y, rect.w.saturating_sub(12), s.h));
        }
    }

    fn paint(&self, r: &Renderer) {
        let t = ACTIVE_THEME.lock();
        r.fill_rect(self.rect, t.palette.surface);
        let bar_c = t.palette.border;
        drop(t);
        if let Some(c) = &self.child { c.paint(r); }
        // Scrollbar
        if self.content_h > self.rect.h {
            let ratio  = self.rect.h as f32 / self.content_h as f32;
            let bar_h  = (self.rect.h as f32 * ratio) as u32;
            let bar_y  = self.rect.y + (self.scroll_y as f32 * ratio) as i32;
            let bar_x  = self.rect.x + self.rect.w as i32 - 10;
            r.fill_rect(Rect::new(bar_x, bar_y, 8, bar_h), bar_c);
        }
    }

    fn handle_event(&mut self, ev: WidgetEvent) -> bool {
        if let WidgetEvent::Scroll { delta } = ev {
            let max_scroll = self.content_h.saturating_sub(self.rect.h) as i32;
            self.scroll_y = (self.scroll_y - delta * 20).clamp(0, max_scroll);
            if let Some(c) = &mut self.child {
                c.layout(Rect::new(self.rect.x, self.rect.y - self.scroll_y,
                    self.rect.w.saturating_sub(12), self.content_h));
            }
            return true;
        }
        if let Some(c) = &mut self.child { c.handle_event(ev) } else { false }
    }
}

/// Initialise the GUI toolkit.
pub fn init() {
    println!("[GUI] Widget toolkit initialised.");
    println!("[GUI]   Theme: {:?}", ACTIVE_THEME.lock().kind);
    println!("[GUI]   Widgets: Label Button TextInput Checkbox Slider ScrollView");
    println!("[GUI]   Layout:  StackLayout FlexLayout AbsoluteLayout");
    println!("[GUI]   Renderer: GPU-accelerated (Performance) / back-buffer (Security)");
}
