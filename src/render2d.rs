// -----| VEILOS 2D renderer core |-----
//
// Software rasteriser for the UI toolkit: anti-aliased rounded rectangles,
// per-side borders, linear/radial gradients, image fills, blurred shadows,
// opacity, rotation and off-screen layers. Pure `alloc` code with NO kernel
// dependencies, so it can be unit tested and rendered to PNG on a host. The
// kernel glue (back buffer access, clip rect) is in render2d_kernel.rs.
//
// Design rules
//   * Integer / fixed-point only. No floats, no libm: works on a soft-float
//     kernel target and never takes the per-pixel cost of software floats.
//   * Premultiplied ARGB inside the pipeline. Straight-alpha inputs (Color32,
//     RGBA images) are premultiplied once at the edge. Gradients interpolate
//     premultiplied, so "white -> transparent" never goes grey.
//   * Targets: the screen back buffer is 0x00RRGGBB (no alpha, top byte kept
//     zero); layers are premultiplied 0xAARRGGBB.
//   * Anti-aliasing is exact-ish coverage: circular corners come from a cached
//     per-radius mask, rotated shapes from a Q8 signed-distance function.
//   * Everything is clipped to the canvas clip rect; coordinates are clamped to
//     +-COORD_LIMIT so arithmetic can never overflow on hostile script values.
//   * Angles are in 1/64 degree. CSS convention for gradients: 0 deg points up,
//     90 deg points right. Shape rotation is clockwise on screen.

use alloc::vec;
use alloc::vec::Vec;

/// Public coordinates are clamped to this magnitude.
pub const COORD_LIMIT: i32 = 1 << 20;
/// Largest corner radius that is honoured (keeps corner masks small).
pub const MAX_RADIUS: u16 = 512;
/// Largest shadow mask / layer we will allocate (pixels).
pub const MAX_MASK_PIXELS: usize = 4 * 1024 * 1024;
/// Largest blurred-shadow mask. Blur cost is ~12 passes over the mask, so this
/// bounds one-off shadow cost; bigger requests get a tighter blur.
pub const MAX_SHADOW_PIXELS: usize = 1024 * 1024;

// =====================================================================
// -----| integer helpers |-----
// =====================================================================

const RB: u32 = 0x00FF_00FF;
const AG: u32 = 0xFF00_FF00;

/// Exact rounded a*b/255 for 0..=255 inputs.
/// Exact rounded a*b/255 for 0..=255 inputs.
#[inline(always)]
pub fn mul255(a: u32, b: u32) -> u32 {
    let t = a.wrapping_mul(b).wrapping_add(128);
    t.wrapping_add(t >> 8) >> 8
}

/// Multiply all four 8-bit lanes of `px` by `s/255` (s in 0..=255), rounded.
/// Multiply all four 8-bit lanes of `px` by `s/255` (s in 0..=255), rounded.
/// Wrapping ops: lane overflow is impossible for s <= 255 (exhaustively tested).
#[inline(always)]
pub fn scale_argb(px: u32, s: u32) -> u32 {
    let rb = (px & RB).wrapping_mul(s).wrapping_add(0x0080_0080);
    let rb = (rb.wrapping_add((rb >> 8) & RB) >> 8) & RB;
    let ag = ((px >> 8) & RB).wrapping_mul(s).wrapping_add(0x0080_0080);
    let ag = ag.wrapping_add((ag >> 8) & RB) & AG;
    rb | ag
}

/// Per-lane linear interpolation, `f` in 0..=255 (0 = a, ~255 = b).
/// Per-lane linear interpolation, `f` in 0..=255 (0 = a, ~255 = b).
#[inline(always)]
fn lerp_px(a: u32, b: u32, f: u32) -> u32 {
    let g = 256 - f;
    let rb = ((((a & RB).wrapping_mul(g)).wrapping_add((b & RB).wrapping_mul(f))) >> 8) & RB;
    let ag = (((((a >> 8) & RB).wrapping_mul(g)).wrapping_add(((b >> 8) & RB).wrapping_mul(f))) >> 8) & RB;
    rb | (ag << 8)
}

/// Integer square root (floor).
/// Integer square root (floor). Newton from a power-of-two over-estimate:
/// ~5 divisions, versus the 32-step bitwise loop it replaces.
pub fn isqrt_u64(n: u64) -> u64 {
    if n < 2 {
        return n;
    }
    let bits = 64 - n.leading_zeros();
    // 2^ceil(bits/2) >= sqrt(n), so Newton decreases monotonically to floor(sqrt(n)).
    let mut x = 1u64 << ((bits + 1) / 2);
    loop {
        let y = (x + n / x) >> 1;
        if y >= x {
            return x;
        }
        x = y;
    }
}

/// sin(0..=90 degrees) in Q15, one entry per quarter degree (361 entries).
const SIN_Q15: [i32; 361] = [
    0, 143, 286, 429, 572, 715, 858, 1001, 1144, 1286, 1429, 1572,
    1715, 1858, 2000, 2143, 2286, 2428, 2571, 2713, 2856, 2998, 3141, 3283,
    3425, 3567, 3709, 3851, 3993, 4135, 4277, 4419, 4560, 4702, 4843, 4985,
    5126, 5267, 5408, 5549, 5690, 5831, 5971, 6112, 6252, 6393, 6533, 6673,
    6813, 6953, 7092, 7232, 7371, 7510, 7650, 7788, 7927, 8066, 8204, 8343,
    8481, 8619, 8757, 8895, 9032, 9169, 9307, 9444, 9580, 9717, 9854, 9990,
    10126, 10262, 10397, 10533, 10668, 10803, 10938, 11073, 11207, 11342, 11476, 11609,
    11743, 11876, 12010, 12142, 12275, 12408, 12540, 12672, 12803, 12935, 13066, 13197,
    13328, 13458, 13589, 13719, 13848, 13978, 14107, 14236, 14365, 14493, 14621, 14749,
    14876, 15004, 15131, 15257, 15384, 15510, 15636, 15761, 15886, 16011, 16136, 16260,
    16384, 16508, 16631, 16754, 16877, 16999, 17121, 17243, 17364, 17485, 17606, 17727,
    17847, 17966, 18086, 18205, 18324, 18442, 18560, 18678, 18795, 18912, 19028, 19145,
    19261, 19376, 19491, 19606, 19720, 19834, 19948, 20061, 20174, 20286, 20399, 20510,
    20622, 20732, 20843, 20953, 21063, 21172, 21281, 21390, 21498, 21605, 21713, 21820,
    21926, 22032, 22138, 22243, 22348, 22452, 22556, 22659, 22763, 22865, 22967, 23069,
    23170, 23271, 23372, 23472, 23571, 23670, 23769, 23867, 23965, 24062, 24159, 24255,
    24351, 24447, 24542, 24636, 24730, 24824, 24917, 25010, 25102, 25193, 25285, 25375,
    25466, 25555, 25645, 25733, 25822, 25909, 25997, 26083, 26170, 26255, 26341, 26426,
    26510, 26594, 26677, 26760, 26842, 26924, 27005, 27086, 27166, 27246, 27325, 27403,
    27482, 27559, 27636, 27713, 27789, 27864, 27939, 28014, 28088, 28161, 28234, 28306,
    28378, 28449, 28520, 28590, 28660, 28729, 28797, 28865, 28932, 28999, 29066, 29131,
    29197, 29261, 29325, 29389, 29452, 29514, 29576, 29637, 29698, 29758, 29818, 29877,
    29935, 29993, 30050, 30107, 30163, 30219, 30274, 30328, 30382, 30435, 30488, 30540,
    30592, 30643, 30693, 30743, 30792, 30840, 30888, 30936, 30983, 31029, 31075, 31120,
    31164, 31208, 31251, 31294, 31336, 31378, 31419, 31459, 31499, 31538, 31576, 31614,
    31651, 31688, 31724, 31760, 31795, 31829, 31863, 31896, 31928, 31960, 31991, 32022,
    32052, 32081, 32110, 32138, 32166, 32193, 32219, 32245, 32270, 32295, 32319, 32342,
    32365, 32387, 32408, 32429, 32449, 32469, 32488, 32506, 32524, 32541, 32557, 32573,
    32588, 32603, 32617, 32631, 32643, 32655, 32667, 32678, 32688, 32698, 32707, 32715,
    32723, 32730, 32737, 32743, 32748, 32753, 32757, 32760, 32763, 32765, 32767, 32768,
    32768,
];

#[inline]
fn table_sin(x_q6: i32) -> i32 {
    // x in 1/64 degree, 0..=90*64. Table step is 0.25 degree = 16 units.
    let x = x_q6.clamp(0, 90 * 64);
    let idx = (x >> 4) as usize;
    let frac = x & 15;
    let a = SIN_Q15[idx];
    let b = SIN_Q15[(idx + 1).min(360)];
    a + (((b - a) * frac + 8) >> 4)
}

/// An angle in 1/64 degree.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Angle(pub i32);

impl Angle {
    pub const ZERO: Angle = Angle(0);
    pub const fn degrees(d: i32) -> Angle {
        Angle(d.wrapping_mul(64))
    }
    fn normalized(self) -> i32 {
        self.0.rem_euclid(360 * 64)
    }
    pub fn is_zero(self) -> bool {
        self.normalized() == 0
    }
}

/// (sin, cos) of an angle in Q15 (32768 = 1.0).
pub fn sin_cos_q15(a: Angle) -> (i32, i32) {
    let n = a.normalized();
    let q = n / (90 * 64);
    let t = n % (90 * 64);
    let rev = 90 * 64 - t;
    match q {
        0 => (table_sin(t), table_sin(rev)),
        1 => (table_sin(rev), -table_sin(t)),
        2 => (-table_sin(t), -table_sin(rev)),
        _ => (-table_sin(rev), table_sin(t)),
    }
}

// Plain comparisons instead of Ord::min/max/clamp/abs: in an unoptimised build
// those are real, several-calls-deep function calls, and they sit in per-pixel loops.
#[inline(always)]
fn clamp_i64(v: i64, lo: i64, hi: i64) -> i64 {
    if v < lo { lo } else if v > hi { hi } else { v }
}

// =====================================================================
// -----| basic types |-----
// =====================================================================

/// Straight (non-premultiplied) RGBA colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Color32 {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color32 {
    pub const TRANSPARENT: Color32 = Color32 { r: 0, g: 0, b: 0, a: 0 };
    pub const BLACK: Color32 = Color32 { r: 0, g: 0, b: 0, a: 255 };
    pub const WHITE: Color32 = Color32 { r: 255, g: 255, b: 255, a: 255 };
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Color32 { r, g, b, a }
    }
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Color32 { r, g, b, a: 255 }
    }
    /// Opaque-alpha packed pixel carrying this colour's RGB (alpha lane = 255).
    #[inline]
    fn rgb_px(self) -> u32 {
        0xFF00_0000 | ((self.r as u32) << 16) | ((self.g as u32) << 8) | self.b as u32
    }
    /// Premultiplied packed pixel.
    #[inline]
    pub fn premul_px(self) -> u32 {
        scale_argb(self.rgb_px(), self.a as u32)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct IRect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl IRect {
    pub const EMPTY: IRect = IRect { x: 0, y: 0, w: 0, h: 0 };
    pub const fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        IRect { x, y, w, h }
    }
    #[inline]
    pub fn right(&self) -> i32 {
        self.x.saturating_add(self.w)
    }
    #[inline]
    pub fn bottom(&self) -> i32 {
        self.y.saturating_add(self.h)
    }
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.w <= 0 || self.h <= 0
    }
    /// Clamp into the supported coordinate range.
    pub fn sane(self) -> IRect {
        IRect {
            x: self.x.clamp(-COORD_LIMIT, COORD_LIMIT),
            y: self.y.clamp(-COORD_LIMIT, COORD_LIMIT),
            w: self.w.clamp(0, 2 * COORD_LIMIT),
            h: self.h.clamp(0, 2 * COORD_LIMIT),
        }
    }
    pub fn intersect(&self, o: &IRect) -> IRect {
        let x0 = self.x.max(o.x);
        let y0 = self.y.max(o.y);
        let x1 = self.right().min(o.right());
        let y1 = self.bottom().min(o.bottom());
        if x1 <= x0 || y1 <= y0 {
            IRect::EMPTY
        } else {
            IRect { x: x0, y: y0, w: x1 - x0, h: y1 - y0 }
        }
    }
    pub fn union(&self, o: &IRect) -> IRect {
        if self.is_empty() {
            return *o;
        }
        if o.is_empty() {
            return *self;
        }
        let x0 = self.x.min(o.x);
        let y0 = self.y.min(o.y);
        let x1 = self.right().max(o.right());
        let y1 = self.bottom().max(o.bottom());
        IRect { x: x0, y: y0, w: x1 - x0, h: y1 - y0 }
    }
}

/// Per-corner radii (circular), in pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Radii {
    pub tl: u16,
    pub tr: u16,
    pub br: u16,
    pub bl: u16,
}

impl Radii {
    pub const ZERO: Radii = Radii { tl: 0, tr: 0, br: 0, bl: 0 };
    pub const fn uniform(r: u16) -> Radii {
        Radii { tl: r, tr: r, br: r, bl: r }
    }
    pub fn is_zero(&self) -> bool {
        self.tl == 0 && self.tr == 0 && self.br == 0 && self.bl == 0
    }
    /// CSS-style: if adjacent radii don't fit a side, scale all of them down.
    pub fn clamped(self, w: i32, h: i32) -> Radii {
        let c = |r: u16| r.min(MAX_RADIUS) as u64;
        let (tl, tr, br, bl) = (c(self.tl), c(self.tr), c(self.br), c(self.bl));
        let (w, h) = (w.max(0) as u64, h.max(0) as u64);
        let mut num = 1u64;
        let mut den = 1u64;
        for (sum, limit) in [(tl + tr, w), (bl + br, w), (tl + bl, h), (tr + br, h)] {
            if sum > limit && sum > 0 && limit * den < num * sum {
                num = limit;
                den = sum;
            }
        }
        let s = |r: u64| (r * num / den) as u16;
        Radii { tl: s(tl), tr: s(tr), br: s(br), bl: s(bl) }
    }
}

/// Per-side border thickness in pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Borders {
    pub top: u16,
    pub right: u16,
    pub bottom: u16,
    pub left: u16,
}

impl Borders {
    pub const fn uniform(t: u16) -> Borders {
        Borders { top: t, right: t, bottom: t, left: t }
    }
    pub fn is_zero(&self) -> bool {
        self.top == 0 && self.right == 0 && self.bottom == 0 && self.left == 0
    }
}

/// A rectangle with optional rounded corners, rotated clockwise about its centre.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shape {
    pub rect: IRect,
    pub radii: Radii,
    pub rotation: Angle,
}

impl Shape {
    pub fn rect(r: IRect) -> Shape {
        Shape { rect: r, radii: Radii::ZERO, rotation: Angle::ZERO }
    }
    pub fn rounded(r: IRect, radius: u16) -> Shape {
        Shape { rect: r, radii: Radii::uniform(radius), rotation: Angle::ZERO }
    }
    pub fn with_rotation(mut self, a: Angle) -> Shape {
        self.rotation = a;
        self
    }
}

/// Axis-aligned bounds (with 1px AA margin) of a rectangle rotated about its centre.
pub fn shape_bounds(rect: IRect, rot: Angle) -> IRect {
    let rect = rect.sane();
    if rot.is_zero() {
        return rect;
    }
    let (s, c) = sin_cos_q15(rot);
    let (w, h) = (rect.w as i64, rect.h as i64);
    let (s, c) = ((s as i64).abs(), (c as i64).abs());
    let bw = ((w * c + h * s) >> 15) as i32;
    let bh = ((w * s + h * c) >> 15) as i32;
    let cx2 = 2 * rect.x as i64 + w;
    let cy2 = 2 * rect.y as i64 + h;
    let x0 = ((cx2 - bw as i64).div_euclid(2) - 1) as i32;
    let y0 = ((cy2 - bh as i64).div_euclid(2) - 1) as i32;
    IRect { x: x0, y: y0, w: bw + 3, h: bh + 3 }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stop {
    /// Position along the gradient in permille (0..=1000).
    pub pos: u16,
    pub color: Color32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fit {
    /// Stretch to the shape (ignores aspect ratio).
    Stretch,
    /// Scale to fit inside, keep aspect, letterbox with transparency.
    Contain,
    /// Scale to cover the whole shape, keep aspect, crop the overflow.
    Cover,
    /// 1:1, centred.
    Center,
    /// 1:1, repeated from the top-left.
    Tile,
}

/// RGB (3 channel) or RGBA (4 channel) straight-alpha, row-major image, the
/// same layout the PNG/JPEG decoders return.
#[derive(Clone, Copy)]
pub struct ImageView<'a> {
    pub w: i32,
    pub h: i32,
    pub channels: u8,
    pub data: &'a [u8],
}

#[derive(Clone, Copy)]
pub struct ImagePaint<'a> {
    pub image: ImageView<'a>,
    pub fit: Fit,
    /// Bilinear when true, nearest-neighbour when false.
    pub smooth: bool,
}

#[derive(Clone, Copy)]
pub enum Paint<'a> {
    Solid(Color32),
    /// CSS convention: 0 deg = towards the top, 90 deg = towards the right.
    Linear { angle: Angle, stops: &'a [Stop] },
    /// Centre in permille of the shape; radius reaches the farthest corner.
    Radial { cx: u16, cy: u16, stops: &'a [Stop] },
    Image(ImagePaint<'a>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shadow {
    pub dx: i32,
    pub dy: i32,
    /// CSS blur radius (Gaussian sigma = blur / 2).
    pub blur: u16,
    pub spread: i32,
    pub color: Color32,
    /// Do not draw the shadow underneath the element itself (CSS box-shadow
    /// behaviour); needed so translucent elements don't show their own shadow.
    pub knockout: bool,
}

/// Off-screen premultiplied ARGB surface (for group opacity / rotation).
pub struct Layer {
    pub w: i32,
    pub h: i32,
    pub px: Vec<u32>,
}

impl Layer {
    /// Transparent layer, or None if it would be too large / memory is short.
    pub fn new(w: i32, h: i32) -> Option<Layer> {
        if w <= 0 || h <= 0 {
            return None;
        }
        let total = (w as usize).checked_mul(h as usize)?;
        if total > MAX_MASK_PIXELS {
            return None;
        }
        let mut px: Vec<u32> = Vec::new();
        px.try_reserve_exact(total).ok()?;
        px.resize(total, 0);
        Some(Layer { w, h, px })
    }
    pub fn clear(&mut self) {
        for p in self.px.iter_mut() {
            *p = 0;
        }
    }
    pub fn texture(&self) -> Tex<'_> {
        Tex::Premul { w: self.w, h: self.h, px: &self.px }
    }
}

/// A cached rendering of one UI element (or group): drawn once, composited many
/// times, re-drawn only when invalidated or when its size / key changes.
///
/// This is the "draw once, redraw on dirty" building block. A full desktop
/// frame then costs one `draw_layer` per cached element instead of re-rasterising
/// every gradient, shadow and border. `const fn new` so it can live in a static.
pub struct CachedLayer {
    layer: Option<Layer>,
    dirty: bool,
    key: u64,
    /// How many times the content has been re-rendered (for tests / diagnostics).
    pub renders: u64,
}

impl CachedLayer {
    pub const fn new() -> CachedLayer {
        CachedLayer { layer: None, dirty: true, key: 0, renders: 0 }
    }

    /// Mark the content stale: the next `get` re-renders.
    pub fn invalidate(&mut self) {
        self.dirty = true;
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Release the pixel memory (e.g. while the element is hidden).
    pub fn drop_pixels(&mut self) {
        self.layer = None;
        self.dirty = true;
    }

    /// The cached layer, re-rendered through `render` first if it is dirty, has a
    /// different size, or `key` changed (use a hash of anything that affects the look).
    /// `render` receives a cleared canvas whose origin is the layer's top-left.
    /// Returns None only if the layer cannot be allocated.
    pub fn get(&mut self, w: i32, h: i32, key: u64, render: impl FnOnce(&mut Canvas)) -> Option<&Layer> {
        let size_ok = matches!(&self.layer, Some(l) if l.w == w && l.h == h);
        if !size_ok {
            self.layer = Layer::new(w, h);
            self.dirty = true;
        }
        if key != self.key {
            self.key = key;
            self.dirty = true;
        }
        if self.dirty {
            if let Some(l) = self.layer.as_mut() {
                l.clear();
                let mut c = Canvas::from_layer(l);
                render(&mut c);
                self.renders += 1;
                self.dirty = false;
            }
        }
        self.layer.as_ref()
    }
}

// =====================================================================
// -----| caches |-----
// =====================================================================

const MAX_CORNER_ENTRIES: usize = 32;
const MAX_SHADOW_BYTES: usize = 6 * 1024 * 1024;

struct CornerMask {
    r: u16,
    last_used: u64,
    data: Vec<u8>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct ShadowKey {
    w: i32,
    h: i32,
    radii: [u16; 4],
    rot: i32,
    blur: u16,
}

struct ShadowEntry {
    key: ShadowKey,
    last_used: u64,
    mw: i32,
    mh: i32,
    data: Vec<u8>,
}

/// Reusable work products: circular corner masks and blurred shadow masks.
/// Own one per renderer (the kernel keeps a global); `const fn new` so it can
/// live in a `static`.
pub struct Cache {
    corners: Vec<CornerMask>,
    shadows: Vec<ShadowEntry>,
    shadow_bytes: usize,
    tick: u64,
    pub shadow_hits: u64,
    pub shadow_misses: u64,
}

fn make_corner_mask(r: u16) -> Vec<u8> {
    let r = r as i32;
    let mut v = Vec::with_capacity((r * r) as usize);
    for py in 0..r {
        for px in 0..r {
            // Pixel centre vs circle centre at (r, r), in half pixels.
            let dx = 2 * r - 2 * px - 1;
            let dy = 2 * r - 2 * py - 1;
            let sq = (dx * dx + dy * dy) as u64;
            let d_hp_q8 = isqrt_u64(sq << 16) as i64; // distance * 2 * 256
            let x = ((2 * r as i64 + 1) * 256 - d_hp_q8).clamp(0, 512);
            v.push(((x * 255) >> 9) as u8);
        }
    }
    v
}

impl Cache {
    pub const fn new() -> Cache {
        Cache {
            corners: Vec::new(),
            shadows: Vec::new(),
            shadow_bytes: 0,
            tick: 0,
            shadow_hits: 0,
            shadow_misses: 0,
        }
    }

    pub fn clear(&mut self) {
        self.corners.clear();
        self.shadows.clear();
        self.shadow_bytes = 0;
    }

    pub fn shadow_bytes(&self) -> usize {
        self.shadow_bytes
    }

    fn ensure_corner(&mut self, r: u16) {
        if r == 0 {
            return;
        }
        self.tick += 1;
        let tick = self.tick;
        if let Some(c) = self.corners.iter_mut().find(|c| c.r == r) {
            c.last_used = tick;
            return;
        }
        if self.corners.len() >= MAX_CORNER_ENTRIES {
            if let Some((i, _)) = self.corners.iter().enumerate().min_by_key(|(_, c)| c.last_used) {
                self.corners.swap_remove(i);
            }
        }
        self.corners.push(CornerMask { r, last_used: tick, data: make_corner_mask(r) });
    }

    fn ensure_corners(&mut self, radii: &Radii) {
        self.ensure_corner(radii.tl);
        self.ensure_corner(radii.tr);
        self.ensure_corner(radii.br);
        self.ensure_corner(radii.bl);
    }

    fn corner(&self, r: u16) -> &[u8] {
        match self.corners.iter().find(|c| c.r == r) {
            Some(c) => &c.data,
            None => &[],
        }
    }
}

// =====================================================================
// -----| rotation walker |-----
// =====================================================================

/// Rotation about a pivot, evaluated incrementally along a scanline.
/// Local coordinates are Q23 pixels relative to the pivot (Q8 pixel * Q15).
#[derive(Clone, Copy)]
struct Xform {
    sin: i64,
    cos: i64,
    pivot_x: i64, // Q8
    pivot_y: i64, // Q8
}

impl Xform {
    fn new(rot: Angle, pivot_x_q8: i64, pivot_y_q8: i64) -> Xform {
        let (s, c) = sin_cos_q15(rot);
        Xform { sin: s as i64, cos: c as i64, pivot_x: pivot_x_q8, pivot_y: pivot_y_q8 }
    }
    /// Inverse-rotated local position of pixel centre (x, y).
    #[inline]
    fn start(&self, x: i32, y: i32) -> (i64, i64) {
        let dx = x as i64 * 256 + 128 - self.pivot_x;
        let dy = y as i64 * 256 + 128 - self.pivot_y;
        (dx * self.cos + dy * self.sin, -dx * self.sin + dy * self.cos)
    }
    /// Change of the local position for one pixel step in +x.
    #[inline]
    fn step(&self) -> (i64, i64) {
        (256 * self.cos, -256 * self.sin)
    }
}

fn centre_q8(r: &IRect) -> (i64, i64) {
    (r.x as i64 * 256 + r.w as i64 * 128, r.y as i64 * 256 + r.h as i64 * 128)
}

// =====================================================================
// -----| shape coverage generators |-----
// =====================================================================

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum RowKind {
    /// Nothing covered on this row.
    Empty,
    /// Every requested pixel is fully covered.
    Full,
    Partial,
}

/// Axis-aligned rounded rectangle using cached corner masks (the fast path).
struct PlainCov<'c> {
    rect: IRect,
    rad: Radii,
    m_tl: &'c [u8],
    m_tr: &'c [u8],
    m_br: &'c [u8],
    m_bl: &'c [u8],
}

impl<'c> PlainCov<'c> {
    fn row(&self, y: i32, x0: i32, x1: i32, out: &mut [u8]) -> RowKind {
        let n = (x1 - x0).max(0) as usize;
        let out = &mut out[..n];
        out.fill(0); // u8 fill lowers to memset
        let r = &self.rect;
        if y < r.y || y >= r.bottom() {
            return RowKind::Empty;
        }
        let rx0 = r.x.max(x0);
        let rx1 = r.right().min(x1);
        if rx0 >= rx1 {
            return RowKind::Empty;
        }
        out[(rx0 - x0) as usize..(rx1 - x0) as usize].fill(255);
        let mut partial = rx0 != x0 || rx1 != x1;
        let dy_top = y - r.y;
        let dy_bot = r.bottom() - 1 - y;

        let mut corner = |radius: u16, mask: &[u8], row: i32, from_left: bool| {
            let rr = radius as i32;
            for xx in 0..rr {
                let gx = if from_left { r.x + xx } else { r.right() - 1 - xx };
                if gx >= x0 && gx < x1 {
                    let idx = (row * rr + xx) as usize;
                    if idx < mask.len() {
                        out[(gx - x0) as usize] = mask[idx];
                    }
                }
            }
        };
        if dy_top < self.rad.tl as i32 {
            partial = true;
            corner(self.rad.tl, self.m_tl, dy_top, true);
        }
        if dy_top < self.rad.tr as i32 {
            partial = true;
            corner(self.rad.tr, self.m_tr, dy_top, false);
        }
        if dy_bot < self.rad.bl as i32 {
            partial = true;
            corner(self.rad.bl, self.m_bl, dy_bot, true);
        }
        if dy_bot < self.rad.br as i32 {
            partial = true;
            corner(self.rad.br, self.m_br, dy_bot, false);
        }
        if partial { RowKind::Partial } else { RowKind::Full }
    }
}

/// Rounded rectangle via a Q8 signed-distance function (rotated shapes).
struct SdfCov {
    xf: Xform,
    off_x: i64, // Q8: pivot - rect centre
    off_y: i64,
    hw: i64, // Q8 half extents
    hh: i64,
    r: [i64; 4], // Q8 radii: tl, tr, br, bl
}

#[inline(always)]
fn rrect_sd_q8(lx: i64, ly: i64, hw: i64, hh: i64, r: &[i64; 4]) -> i64 {
    let rad = match (lx < 0, ly < 0) {
        (true, true) => r[0],
        (false, true) => r[1],
        (false, false) => r[2],
        (true, false) => r[3],
    };
    let ax = if lx < 0 { -lx } else { lx };
    let ay = if ly < 0 { -ly } else { ly };
    let qx = ax - (hw - rad);
    let qy = ay - (hh - rad);
    let ox = if qx > 0 { qx } else { 0 };
    let oy = if qy > 0 { qy } else { 0 };
    let outside = if ox > 0 && oy > 0 { isqrt_u64((ox * ox + oy * oy) as u64) as i64 } else { ox + oy };
    let m = if qx > qy { qx } else { qy };
    let inside = if m < 0 { m } else { 0 };
    outside + inside - rad
}

impl SdfCov {
    fn row(&self, y: i32, x0: i32, x1: i32, out: &mut [u8]) -> RowKind {
        let n = (x1 - x0).max(0) as usize;
        let (mut lx, mut ly) = self.xf.start(x0, y);
        let (dx, dy) = self.xf.step();
        let mut any = false;
        let mut all = true;
        let mut i = 0;
        while i < n {
            let ax = (lx >> 15) + self.off_x;
            let ay = (ly >> 15) + self.off_y;
            let sd = rrect_sd_q8(ax, ay, self.hw, self.hh, &self.r);
            let v = 128 - sd;
            let c = if v <= 0 { 0 } else if v >= 256 { 255 } else { ((v * 255) >> 8) as u8 };
            out[i] = c;
            any |= c != 0;
            all &= c == 255;
            lx += dx;
            ly += dy;
            i += 1;
        }
        if !any {
            RowKind::Empty
        } else if all {
            RowKind::Full
        } else {
            RowKind::Partial
        }
    }
}

enum Prepared<'c> {
    Plain(PlainCov<'c>),
    Sdf(SdfCov),
}

impl<'c> Prepared<'c> {
    /// `pivot` is the rotation centre in Q8 (defaults to the rect centre).
    /// Corner masks must already be in `cache` (call `ensure_corners`).
    fn new(cache: &'c Cache, rect: IRect, rad: Radii, rot: Angle, pivot: Option<(i64, i64)>) -> Prepared<'c> {
        if rot.is_zero() {
            Prepared::Plain(PlainCov {
                rect,
                rad,
                m_tl: cache.corner(rad.tl),
                m_tr: cache.corner(rad.tr),
                m_br: cache.corner(rad.br),
                m_bl: cache.corner(rad.bl),
            })
        } else {
            let (cx, cy) = centre_q8(&rect);
            let (px, py) = pivot.unwrap_or((cx, cy));
            Prepared::Sdf(SdfCov {
                xf: Xform::new(rot, px, py),
                off_x: px - cx,
                off_y: py - cy,
                hw: rect.w as i64 * 128,
                hh: rect.h as i64 * 128,
                r: [rad.tl as i64 * 256, rad.tr as i64 * 256, rad.br as i64 * 256, rad.bl as i64 * 256],
            })
        }
    }

    fn row(&self, y: i32, x0: i32, x1: i32, out: &mut [u8]) -> RowKind {
        match self {
            Prepared::Plain(p) => p.row(y, x0, x1, out),
            Prepared::Sdf(s) => s.row(y, x0, x1, out),
        }
    }
}

// =====================================================================
// -----| textures and sampling |-----
// =====================================================================

/// A read-only source of premultiplied pixels.
#[derive(Clone, Copy)]
pub enum Tex<'a> {
    Straight { w: i32, h: i32, ch: u8, data: &'a [u8] },
    Premul { w: i32, h: i32, px: &'a [u32] },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Wrap {
    Clamp,
    Transparent,
    Repeat,
}

impl<'a> Tex<'a> {
    fn dims(&self) -> (i32, i32) {
        match *self {
            Tex::Straight { w, h, .. } => (w, h),
            Tex::Premul { w, h, .. } => (w, h),
        }
    }

    /// Premultiplied pixel at in-range coordinates (0 if the data is short).
    #[inline(always)]
    fn fetch(&self, x: i32, y: i32) -> u32 {
        match *self {
            Tex::Straight { w, ch, data, .. } => {
                let ch = ch as usize;
                let i = (y as usize * w as usize + x as usize) * ch;
                if ch < 3 || i + ch > data.len() {
                    return 0;
                }
                let rgb = ((data[i] as u32) << 16) | ((data[i + 1] as u32) << 8) | data[i + 2] as u32;
                let a = if ch >= 4 { data[i + 3] as u32 } else { 255 };
                if a == 255 {
                    0xFF00_0000 | rgb
                } else if a == 0 {
                    0
                } else {
                    scale_argb(0xFF00_0000 | rgb, a)
                }
            }
            Tex::Premul { w, px, .. } => px.get(y as usize * w as usize + x as usize).copied().unwrap_or(0),
        }
    }
}

#[inline(always)]
fn wrap_coord(c: i32, n: i32, wrap: Wrap) -> Option<i32> {
    match wrap {
        Wrap::Clamp => Some(c.clamp(0, n - 1)),
        Wrap::Repeat => Some(c.rem_euclid(n)),
        Wrap::Transparent => {
            if c < 0 || c >= n { None } else { Some(c) }
        }
    }
}

#[inline(always)]
fn fetch_wrapped(tex: &Tex, x: i32, y: i32, wrap: Wrap) -> u32 {
    let (w, h) = tex.dims();
    match (wrap_coord(x, w, wrap), wrap_coord(y, h, wrap)) {
        (Some(xx), Some(yy)) => tex.fetch(xx, yy),
        _ => 0,
    }
}

/// Sample at (u, v) in Q16 texel units (texel centres at +0.5).
fn sample(tex: &Tex, u: i64, v: i64, wrap: Wrap, smooth: bool) -> u32 {
    const LIM: i64 = 1 << 20;
    if !smooth {
        let x = clamp_i64(u >> 16, -LIM, LIM) as i32;
        let y = clamp_i64(v >> 16, -LIM, LIM) as i32;
        return fetch_wrapped(tex, x, y, wrap);
    }
    let u2 = u - 32768;
    let v2 = v - 32768;
    let x0 = clamp_i64(u2 >> 16, -LIM, LIM) as i32;
    let y0 = clamp_i64(v2 >> 16, -LIM, LIM) as i32;
    let fx = ((u2 >> 8) & 255) as u32;
    let fy = ((v2 >> 8) & 255) as u32;
    let (w, h) = tex.dims();
    let (p00, p10, p01, p11);
    if x0 >= 0 && y0 >= 0 && x0 + 1 < w && y0 + 1 < h {
        // Interior: all four texels exist, no wrapping logic needed.
        p00 = tex.fetch(x0, y0);
        p10 = tex.fetch(x0 + 1, y0);
        p01 = tex.fetch(x0, y0 + 1);
        p11 = tex.fetch(x0 + 1, y0 + 1);
    } else {
        p00 = fetch_wrapped(tex, x0, y0, wrap);
        p10 = fetch_wrapped(tex, x0 + 1, y0, wrap);
        p01 = fetch_wrapped(tex, x0, y0 + 1, wrap);
        p11 = fetch_wrapped(tex, x0 + 1, y0 + 1, wrap);
    }
    lerp_px(lerp_px(p00, p10, fx), lerp_px(p01, p11, fx), fy)
}

// =====================================================================
// -----| paint (colour sources) |-----
// =====================================================================

/// Where a shape's local space sits, so paints can be evaluated per pixel.
#[derive(Clone, Copy)]
struct ShadeGeom {
    rect: IRect,
    xf: Xform,
    off_x: i64, // Q8: pivot - rect centre
    off_y: i64,
}

impl ShadeGeom {
    fn new(rect: IRect, rot: Angle) -> ShadeGeom {
        let (cx, cy) = centre_q8(&rect);
        ShadeGeom { rect, xf: Xform::new(rot, cx, cy), off_x: 0, off_y: 0 }
    }
    /// Local position relative to the rect centre, Q23, at pixel (x, y).
    #[inline]
    fn walker(&self, x: i32, y: i32) -> (i64, i64, i64, i64) {
        let (lx, ly) = self.xf.start(x, y);
        let (dx, dy) = self.xf.step();
        (lx + (self.off_x << 15), ly + (self.off_y << 15), dx, dy)
    }
}

/// 4x4 Bayer thresholds scaled to 8..=248 (mean 128), added in Q8 before truncation.
const DITHER4: [u32; 16] = [8, 136, 40, 168, 200, 72, 232, 104, 56, 184, 24, 152, 248, 120, 216, 88];
/// Gradient lookup resolution: one entry per 1/1024 of the gradient length.
const LUT_N: usize = 1024;

enum GradKind {
    Linear { dirx: i64, diry: i64, k: i64 },
    Radial { cx_q8: i64, cy_q8: i64, kr: i64 },
}

struct GradPre {
    /// Premultiplied [a, r, g, b], Q8, LUT_N entries across t = 0..=1.
    lut: Vec<[u16; 4]>,
    kind: GradKind,
    /// 0 = unknown, 1 = rows differ, 2 = every row is identical up to dither phase.
    invariant: core::cell::Cell<u8>,
    /// Row cache (by y & 3) used when `invariant == 2`: (x0, pixels).
    rows: core::cell::RefCell<[(i32, u32, Vec<u32>); 4]>,
}

#[inline(always)]
fn pack_premul(a: u32, r: u32, g: u32, b: u32) -> u32 {
    let r = if r > a { a } else { r };
    let g = if g > a { a } else { g };
    let b = if b > a { a } else { b };
    (a << 24) | (r << 16) | (g << 8) | b
}

impl GradPre {
    fn new(lut: Vec<[u16; 4]>, kind: GradKind) -> GradPre {
        GradPre {
            lut,
            kind,
            invariant: core::cell::Cell::new(0),
            rows: core::cell::RefCell::new([(0, 0, Vec::new()), (0, 0, Vec::new()), (0, 0, Vec::new()), (0, 0, Vec::new())]),
        }
    }

    /// One dithered pixel from LUT entry `idx` at dither slot `d`.
    #[inline(always)]
    fn px(&self, idx: usize, d: u32, mask: u32) -> u32 {
        let e = &self.lut[idx];
        let a = (e[0] as u32 + d) >> 8;
        let a = if a > 255 { 255 } else { a };
        pack_premul(a, (e[1] as u32 + d) >> 8, (e[2] as u32 + d) >> 8, (e[3] as u32 + d) >> 8) & mask
    }

    /// True when every LUT entry is fully opaque (then rows can be written straight to the target).
    fn is_opaque(&self) -> bool {
        self.lut.iter().all(|e| e[0] >= 65280)
    }

    /// Does a linear gradient look the same on every row (angle 90/270, unrotated)?
    fn rows_identical(&self, g: &ShadeGeom) -> bool {
        match self.kind {
            GradKind::Linear { dirx, diry, .. } => {
                let (lx0, ly0, _, _) = g.walker(0, 0);
                let (lx1, ly1, _, _) = g.walker(0, 1);
                (lx1 - lx0) * dirx + (ly1 - ly0) * diry == 0
            }
            GradKind::Radial { .. } => false,
        }
    }

    fn shade(&self, g: &ShadeGeom, y: i32, x0: i32, out: &mut [u32], mask: u32) {
        let n = out.len();
        let ybase = ((y as u32) & 3) * 4;
        let xb = (x0 as u32) & 3;
        match self.kind {
            GradKind::Linear { dirx, diry, k } => {
                if self.invariant.get() == 0 {
                    self.invariant.set(if self.rows_identical(g) { 2 } else { 1 });
                }
                let cached = self.invariant.get() == 2;
                if cached {
                    let rows = self.rows.borrow();
                    let slot = &rows[(y & 3) as usize];
                    if slot.0 == x0 && slot.1 == mask && slot.2.len() == n && n != 0 {
                        out.copy_from_slice(&slot.2);
                        return;
                    }
                }
                let (lx, ly, dx, dy) = g.walker(x0, y);
                let dot0 = (lx * dirx + ly * diry) >> 15; // Q23 px
                let ddot = (dx * dirx + dy * diry) >> 15;
                let mut t = (32768i64 << 8) + ((dot0 * k) >> 24); // Q24, 1.0 = 1<<24
                let dt = (ddot * k) >> 24;
                let last = LUT_N as i64 - 1;
                if dt == 0 {
                    // Constant along the row (angle 0/180): only the dither varies.
                    let idx = clamp_i64(t >> 14, 0, last) as usize;
                    let pat = [self.px(idx, DITHER4[ybase as usize + ((xb) & 3) as usize], mask),
                               self.px(idx, DITHER4[ybase as usize + ((xb + 1) & 3) as usize], mask),
                               self.px(idx, DITHER4[ybase as usize + ((xb + 2) & 3) as usize], mask),
                               self.px(idx, DITHER4[ybase as usize + ((xb + 3) & 3) as usize], mask)];
                    let mut i = 0;
                    while i < n {
                        out[i] = pat[i & 3];
                        i += 1;
                    }
                } else {
                    let mut i = 0;
                    while i < n {
                        let mut idx = t >> 14;
                        if idx < 0 {
                            idx = 0;
                        } else if idx > last {
                            idx = last;
                        }
                        let d = DITHER4[ybase as usize + ((xb + i as u32) & 3) as usize];
                        out[i] = self.px(idx as usize, d, mask);
                        t += dt;
                        i += 1;
                    }
                }
                if cached {
                    let mut rows = self.rows.borrow_mut();
                    let slot = &mut rows[(y & 3) as usize];
                    slot.0 = x0;
                    slot.1 = mask;
                    slot.2.clear();
                    slot.2.extend_from_slice(out);
                }
            }
            GradKind::Radial { cx_q8, cy_q8, kr } => {
                let (mut lx, mut ly, dx, dy) = g.walker(x0, y);
                let last = LUT_N as i64 - 1;
                let mut i = 0;
                while i < n {
                    let ax = (lx >> 15) - cx_q8;
                    let ay = (ly >> 15) - cy_q8;
                    let d = isqrt_u64((ax * ax + ay * ay) as u64) as i64;
                    let idx = clamp_i64(((d * kr) >> 16) >> 6, 0, last) as usize;
                    let dither = DITHER4[ybase as usize + ((xb + i as u32) & 3) as usize];
                    out[i] = self.px(idx, dither, mask);
                    lx += dx;
                    ly += dy;
                    i += 1;
                }
            }
        }
    }
}

struct ImgPre<'a> {
    tex: Tex<'a>,
    wrap: Wrap,
    smooth: bool,
    inv_x: i64, // Q16 source px per destination px
    inv_y: i64,
    cx_q8: i64, // content origin relative to the rect's top-left, Q8
    cy_q8: i64,
}

enum PaintPre<'a> {
    Solid(Color32),
    Grad(GradPre),
    Img(ImgPre<'a>),
}

fn premul_q8(c: Color32) -> [u32; 4] {
    let a = c.a as u32;
    let f = |v: u8| (v as u32 * a * 256) / 255;
    [a * 256, f(c.r), f(c.g), f(c.b)]
}

fn build_lut(stops: &[Stop]) -> Vec<[u16; 4]> {
    let mut s: Vec<Stop> = stops.iter().take(16).copied().collect();
    for st in s.iter_mut() {
        st.pos = st.pos.min(1000);
    }
    // insertion sort by position (stable, tiny n)
    for i in 1..s.len() {
        let mut j = i;
        while j > 0 && s[j - 1].pos > s[j].pos {
            s.swap(j - 1, j);
            j -= 1;
        }
    }
    let mut lut = Vec::with_capacity(LUT_N);
    for i in 0..LUT_N as u32 {
        let t = (i * 1000 / (LUT_N as u32 - 1)) as u16; // permille
        let px: [u32; 4] = if s.is_empty() {
            [0; 4]
        } else if t <= s[0].pos {
            premul_q8(s[0].color)
        } else if t >= s[s.len() - 1].pos {
            premul_q8(s[s.len() - 1].color)
        } else {
            let mut k = 0;
            while k + 1 < s.len() && s[k + 1].pos < t {
                k += 1;
            }
            let (a, b) = (s[k], s[k + 1]);
            let span = (b.pos - a.pos).max(1) as u32;
            let f = (t - a.pos) as u32 * 256 / span; // 0..=256
            let (pa, pb) = (premul_q8(a.color), premul_q8(b.color));
            let mut out = [0u32; 4];
            for c in 0..4 {
                out[c] = (pa[c] * (256 - f) + pb[c] * f) >> 8;
            }
            out
        };
        lut.push([px[0] as u16, px[1] as u16, px[2] as u16, px[3] as u16]);
    }
    lut
}

impl<'a> PaintPre<'a> {
    fn new(paint: &Paint<'a>, rect: IRect) -> PaintPre<'a> {
        match *paint {
            Paint::Solid(c) => PaintPre::Solid(c),
            Paint::Linear { angle, stops } => {
                if stops.len() == 1 {
                    return PaintPre::Solid(stops[0].color);
                }
                let (s, c) = sin_cos_q15(angle);
                // Direction: 0 deg = up. y grows downward.
                let (dirx, diry) = (s as i64, -(c as i64));
                let (w, h) = (rect.w as i64 * 256, rect.h as i64 * 256);
                let len = ((w * dirx.abs() + h * diry.abs()) >> 15).max(1);
                PaintPre::Grad(GradPre::new(build_lut(stops), GradKind::Linear { dirx, diry, k: (2i64 << 32) / len }))
            }
            Paint::Radial { cx, cy, stops } => {
                if stops.len() == 1 {
                    return PaintPre::Solid(stops[0].color);
                }
                let (w, h) = (rect.w as i64 * 256, rect.h as i64 * 256);
                // Centre relative to the rect centre, Q8.
                let cx_q8 = (cx.min(1000) as i64 - 500) * w / 1000;
                let cy_q8 = (cy.min(1000) as i64 - 500) * h / 1000;
                // Farthest corner distance.
                let mut far = 1i64;
                for (sx, sy) in [(-1i64, -1i64), (1, -1), (1, 1), (-1, 1)] {
                    let ddx = sx * (w / 2) - cx_q8;
                    let ddy = sy * (h / 2) - cy_q8;
                    far = far.max(isqrt_u64((ddx * ddx + ddy * ddy) as u64) as i64);
                }
                PaintPre::Grad(GradPre::new(build_lut(stops), GradKind::Radial { cx_q8, cy_q8, kr: (1i64 << 32) / far }))
            }
            Paint::Image(ip) => {
                let im = ip.image;
                if im.w <= 0 || im.h <= 0 || !(im.channels == 3 || im.channels == 4) {
                    return PaintPre::Solid(Color32::TRANSPARENT);
                }
                let tex = Tex::Straight { w: im.w, h: im.h, ch: im.channels, data: im.data };
                let (rw, rh) = (rect.w.max(1) as i64, rect.h.max(1) as i64);
                let (iw, ih) = (im.w as i64, im.h as i64);
                // Content size/origin in whole destination pixels.
                let (cw, ch, ox, oy, wrap) = match ip.fit {
                    Fit::Stretch => (rw, rh, 0, 0, Wrap::Clamp),
                    Fit::Contain => {
                        let (cw, ch) = if rw * ih <= rh * iw { (rw, (ih * rw / iw).max(1)) } else { ((iw * rh / ih).max(1), rh) };
                        (cw, ch, (rw - cw) / 2, (rh - ch) / 2, Wrap::Transparent)
                    }
                    Fit::Cover => {
                        let (cw, ch) = if rw * ih >= rh * iw { (rw, (ih * rw / iw).max(1)) } else { ((iw * rh / ih).max(1), rh) };
                        (cw, ch, (rw - cw) / 2, (rh - ch) / 2, Wrap::Clamp)
                    }
                    Fit::Center => (iw, ih, (rw - iw) / 2, (rh - ih) / 2, Wrap::Transparent),
                    Fit::Tile => (iw, ih, 0, 0, Wrap::Repeat),
                };
                PaintPre::Img(ImgPre {
                    tex,
                    wrap,
                    smooth: ip.smooth,
                    inv_x: (iw << 16) / cw.max(1),
                    inv_y: (ih << 16) / ch.max(1),
                    cx_q8: ox * 256,
                    cy_q8: oy * 256,
                })
            }
        }
    }

    /// Fill `out` with premultiplied pixels for x0.. on row y.
    /// Fill `out` with premultiplied pixels for x0.. on row y, ANDed with `mask`
    /// (all ones when compositing; 0x00FFFFFF when writing straight to an opaque target).
    fn shade_row(&self, g: &ShadeGeom, y: i32, x0: i32, out: &mut [u32], mask: u32) {
        match self {
            PaintPre::Solid(c) => {
                fill_u32(out, c.premul_px() & mask);
            }
            PaintPre::Grad(gp) => gp.shade(g, y, x0, out, mask),
            PaintPre::Img(ip) => {
                let (lx, ly, dx, dy) = g.walker(x0, y);
                // Relative to the rect's top-left, then to the content origin (Q23).
                let tlx = (g.rect.w as i64 * 128) << 15;
                let tly = (g.rect.h as i64 * 128) << 15;
                let lx = lx + tlx - (ip.cx_q8 << 15);
                let ly = ly + tly - (ip.cy_q8 << 15);
                // dx/dy are multiples of 128, so (l >> 7) is exactly linear in x:
                // accumulate in Q32 instead of multiplying per pixel (bit-identical).
                let mut ua = (lx >> 7) * ip.inv_x;
                let mut va = (ly >> 7) * ip.inv_y;
                let dua = (dx >> 7) * ip.inv_x;
                let dva = (dy >> 7) * ip.inv_y;
                let n = out.len();
                let mut i = 0;
                while i < n {
                    out[i] = sample(&ip.tex, ua >> 16, va >> 16, ip.wrap, ip.smooth) & mask;
                    ua += dua;
                    va += dva;
                    i += 1;
                }
            }
        }
    }

    /// Fully opaque everywhere the shape covers (so rows can skip blending).
    fn is_opaque(&self) -> bool {
        match self {
            PaintPre::Solid(c) => c.a == 255,
            PaintPre::Grad(g) => g.is_opaque(),
            PaintPre::Img(ip) => {
                ip.wrap != Wrap::Transparent
                    && matches!(ip.tex, Tex::Straight { ch, .. } if ch == 3)
            }
        }
    }
}

// =====================================================================
// -----| blur |-----
// =====================================================================

/// Three box sizes approximating a Gaussian with sigma = blur / 2.
fn box_sizes(blur: u16) -> [i64; 3] {
    let b = blur as i64;
    let mut wl = b;
    if wl % 2 == 0 {
        wl -= 1;
    }
    if wl < 1 {
        wl = 1;
    }
    let wu = wl + 2;
    let nn = -(3 * b * b - 3 * wl * wl - 12 * wl - 9);
    let dd = 4 * wl + 4;
    let m = ((2 * nn + dd) / (2 * dd)).clamp(0, 3);
    [if m > 0 { wl } else { wu }, if m > 1 { wl } else { wu }, if m > 2 { wl } else { wu }]
}

/// One sliding-window box blur over a (possibly strided) line, zero padded.
/// Values are Q8 (value * 256) so repeated passes neither lose mass nor band.
fn box_line(data: &mut [u16], step: usize, len: usize, r: usize, tmp: &mut [u16]) {
    if len == 0 {
        return;
    }
    for i in 0..len {
        tmp[i] = data[i * step];
    }
    let d = (2 * r + 1) as u64;
    let recip = (65536 + d - 1) / d;
    let mut sum: u32 = 0;
    for j in 0..=r.min(len - 1) {
        sum += tmp[j] as u32;
    }
    for i in 0..len {
        data[i * step] = (((sum as u64 * recip + 32768) >> 16).min(65280)) as u16;
        let add = i + r + 1;
        if add < len {
            sum += tmp[add] as u32;
        }
        if i >= r {
            sum -= tmp[i - r] as u32;
        }
    }
}

/// Gaussian-ish blur of an 8-bit coverage mask. Returns false (mask left
/// sharp) only if the temporary 16-bit buffer cannot be allocated.
fn blur_mask(mask: &mut [u8], w: usize, h: usize, blur: u16) -> bool {
    if blur == 0 || w == 0 || h == 0 {
        return true;
    }
    let mut m16: Vec<u16> = Vec::new();
    if m16.try_reserve_exact(mask.len()).is_err() {
        return false;
    }
    m16.extend(mask.iter().map(|&v| (v as u16) << 8));
    let mut tmp = vec![0u16; w.max(h)];
    for bw in box_sizes(blur) {
        let r = ((bw - 1) / 2) as usize;
        if r == 0 {
            continue;
        }
        for y in 0..h {
            box_line(&mut m16[y * w..(y + 1) * w], 1, w, r, &mut tmp);
        }
        for x in 0..w {
            box_line(&mut m16[x..], w, h, r, &mut tmp);
        }
    }
    for (o, &v) in mask.iter_mut().zip(m16.iter()) {
        *o = ((v as u32 + 128) >> 8).min(255) as u8;
    }
    true
}

// =====================================================================
// -----| canvas |-----
// =====================================================================

/// `d` blended with premultiplied `s` (after applying `opacity`), masked for the target.
/// `d` blended with premultiplied `s` (after applying `opacity`), masked for the target.
#[inline(always)]
fn over_px(d: u32, s: u32, opacity: u32, amask: u32) -> u32 {
    if s >> 24 == 0 {
        return d;
    }
    let s = if opacity == 255 { s } else { scale_argb(s, opacity) };
    let sa = s >> 24;
    if sa == 255 {
        s & amask
    } else {
        s.wrapping_add(scale_argb(d, 255 - sa)) & amask
    }
}

/// Fill a u32 slice. Doubling `copy_within` lowers to memmove, which stays
/// fast even in unoptimised builds (a plain loop does not).
#[inline]
fn fill_u32(dst: &mut [u32], v: u32) {
    let n = dst.len();
    let head = n.min(8);
    let mut i = 0;
    while i < head {
        dst[i] = v;
        i += 1;
    }
    let mut filled = head;
    while filled < n {
        let chunk = filled.min(n - filled);
        dst.copy_within(0..chunk, filled);
        filled += chunk;
    }
}

/// A drawing surface: the back buffer (opaque 0x00RRGGBB) or a `Layer`
/// (premultiplied ARGB), with a clip rectangle and dirty-rect tracking.
pub struct Canvas<'a> {
    px: &'a mut [u32],
    w: i32,
    h: i32,
    stride: usize,
    clip: IRect,
    amask: u32,
    dirty: IRect,
}

impl<'a> Canvas<'a> {
    /// Opaque target such as the screen back buffer (top byte kept zero).
    pub fn new_opaque(px: &'a mut [u32], w: i32, h: i32) -> Option<Canvas<'a>> {
        if w <= 0 || h <= 0 {
            return None;
        }
        let need = (w as usize).checked_mul(h as usize)?;
        if px.len() < need {
            return None;
        }
        Some(Canvas { px, w, h, stride: w as usize, clip: IRect::new(0, 0, w, h), amask: 0x00FF_FFFF, dirty: IRect::EMPTY })
    }

    /// Draw into a layer (premultiplied ARGB, keeps alpha).
    pub fn from_layer(layer: &'a mut Layer) -> Canvas<'a> {
        let (w, h) = (layer.w, layer.h);
        Canvas { px: &mut layer.px[..], w, h, stride: w as usize, clip: IRect::new(0, 0, w, h), amask: 0xFFFF_FFFF, dirty: IRect::EMPTY }
    }

    pub fn bounds(&self) -> IRect {
        IRect::new(0, 0, self.w, self.h)
    }
    pub fn clip(&self) -> IRect {
        self.clip
    }
    /// Replace the clip (limited to the canvas bounds).
    pub fn set_clip(&mut self, r: IRect) {
        self.clip = r.sane().intersect(&self.bounds());
    }
    /// Narrow the clip; returns the previous clip for `pop_clip`.
    pub fn push_clip(&mut self, r: IRect) -> IRect {
        let prev = self.clip;
        self.clip = prev.intersect(&r.sane());
        prev
    }
    pub fn pop_clip(&mut self, prev: IRect) {
        self.clip = prev;
    }
    /// Bounding box of everything drawn since the last call.
    pub fn take_dirty(&mut self) -> IRect {
        core::mem::replace(&mut self.dirty, IRect::EMPTY)
    }
    fn mark_dirty(&mut self, r: IRect) {
        self.dirty = self.dirty.union(&r);
    }
    /// Raw pixel (0 outside the canvas). Mostly for tests.
    pub fn pixel(&self, x: i32, y: i32) -> u32 {
        if x < 0 || y < 0 || x >= self.w || y >= self.h {
            0
        } else {
            self.px[y as usize * self.stride + x as usize]
        }
    }

    #[inline]
    fn row_mut(&mut self, y: i32, x0: i32, n: usize) -> &mut [u32] {
        let i = y as usize * self.stride + x0 as usize;
        &mut self.px[i..i + n]
    }

    // ----- low-level spans -----

    fn blend_span_const(&mut self, y: i32, x0: i32, x1: i32, col: Color32, opacity: u32) {
        let a = mul255(col.a as u32, opacity);
        if a == 0 || x1 <= x0 {
            return;
        }
        let amask = self.amask;
        let n = (x1 - x0) as usize;
        let row = self.row_mut(y, x0, n);
        if a == 255 {
            fill_u32(row, col.rgb_px() & amask);
            return;
        }
        let s = scale_argb(col.rgb_px(), a);
        let inv = 255 - a;
        let mut i = 0;
        while i < n {
            row[i] = s.wrapping_add(scale_argb(row[i], inv)) & amask;
            i += 1;
        }
    }

    fn composite_cov_solid(&mut self, y: i32, x0: i32, cov: &[u8], col: Color32, opacity: u32) {
        let a0 = mul255(col.a as u32, opacity);
        if a0 == 0 {
            return;
        }
        let amask = self.amask;
        let base = col.rgb_px();
        // Constants for fully covered pixels (the common case inside a shape).
        let s_full = scale_argb(base, a0);
        let inv_full = 255 - a0;
        let n = cov.len();
        let row = self.row_mut(y, x0, n);
        let mut i = 0;
        while i < n {
            let c = cov[i] as u32;
            if c == 255 {
                row[i] = if a0 == 255 { base & amask } else { s_full.wrapping_add(scale_argb(row[i], inv_full)) & amask };
            } else if c != 0 {
                let a = mul255(a0, c);
                if a != 0 {
                    row[i] = scale_argb(base, a).wrapping_add(scale_argb(row[i], 255 - a)) & amask;
                }
            }
            i += 1;
        }
    }

    fn composite_cov_row(&mut self, y: i32, x0: i32, cov: &[u8], src: &[u32], opacity: u32) {
        let amask = self.amask;
        let n = cov.len().min(src.len());
        let row = self.row_mut(y, x0, n);
        let mut i = 0;
        while i < n {
            let c = cov[i] as u32;
            let s = src[i];
            if c != 0 && s >> 24 != 0 {
                let k = if opacity == 255 { c } else { mul255(c, opacity) };
                let s = if k == 255 { s } else { scale_argb(s, k) };
                row[i] = over_px(row[i], s, 255, amask);
            }
            i += 1;
        }
    }

    fn emit_row(&mut self, pre: &PaintPre, geom: &ShadeGeom, y: i32, x0: i32, cov: &[u8], kind: RowKind, rowbuf: &mut [u32], opacity: u32) {
        if kind == RowKind::Empty {
            return;
        }
        let n = cov.len();
        match pre {
            PaintPre::Solid(c) => {
                if kind == RowKind::Full {
                    self.blend_span_const(y, x0, x0 + n as i32, *c, opacity);
                } else {
                    self.composite_cov_solid(y, x0, cov, *c, opacity);
                }
            }
            _ => {
                // Fully covered row, opaque paint, full opacity: shade straight into
                // the target. No scratch row, no blend pass.
                if kind == RowKind::Full && opacity == 255 && pre.is_opaque() {
                    let amask = self.amask;
                    let row = self.row_mut(y, x0, n);
                    pre.shade_row(geom, y, x0, row, amask);
                    return;
                }
                pre.shade_row(geom, y, x0, &mut rowbuf[..n], 0xFFFF_FFFF);
                self.composite_cov_row(y, x0, cov, &rowbuf[..n], opacity);
            }
        }
    }

    // ----- simple fills -----

    /// Blend a solid colour over a plain rectangle (no anti-aliasing needed).
    pub fn fill_rect(&mut self, r: IRect, color: Color32, opacity: u8) {
        let r = r.sane().intersect(&self.clip);
        if r.is_empty() || opacity == 0 {
            return;
        }
        for y in r.y..r.bottom() {
            self.blend_span_const(y, r.x, r.right(), color, opacity as u32);
        }
        self.mark_dirty(r);
    }

    /// Overwrite (no blending) a rectangle with a colour, e.g. to clear a layer.
    pub fn clear_rect(&mut self, r: IRect, color: Color32) {
        let r = r.sane().intersect(&self.clip);
        if r.is_empty() {
            return;
        }
        let v = color.premul_px() & self.amask;
        for y in r.y..r.bottom() {
            for p in self.row_mut(y, r.x, r.w as usize).iter_mut() {
                *p = v;
            }
        }
        self.mark_dirty(r);
    }

    // ----- shapes -----

    /// Fill a (rounded, optionally rotated) shape with a paint.
    pub fn fill_shape(&mut self, shape: &Shape, paint: &Paint, opacity: u8, cache: &mut Cache) {
        let rect = shape.rect.sane();
        if opacity == 0 || rect.is_empty() {
            return;
        }
        let bounds = shape_bounds(rect, shape.rotation).intersect(&self.clip);
        if bounds.is_empty() {
            return;
        }
        let radii = shape.radii.clamped(rect.w, rect.h);
        cache.ensure_corners(&radii);
        let cache: &Cache = &*cache;
        let prep = Prepared::new(cache, rect, radii, shape.rotation, None);
        let geom = ShadeGeom::new(rect, shape.rotation);
        let pre = PaintPre::new(paint, rect);
        let n = bounds.w as usize;
        let mut cov = vec![0u8; n];
        let mut rowbuf: Vec<u32> = if matches!(pre, PaintPre::Solid(_)) { Vec::new() } else { vec![0u32; n] };
        for y in bounds.y..bounds.bottom() {
            let kind = prep.row(y, bounds.x, bounds.right(), &mut cov);
            self.emit_row(&pre, &geom, y, bounds.x, &cov, kind, &mut rowbuf, opacity as u32);
        }
        self.mark_dirty(bounds);
    }

    /// Draw the border ring of a shape with per-side thickness (CSS-like: the
    /// inner edge's corner radius shrinks by the adjoining border widths).
    pub fn stroke_shape(&mut self, shape: &Shape, b: &Borders, paint: &Paint, opacity: u8, cache: &mut Cache) {
        let rect = shape.rect.sane();
        if opacity == 0 || rect.is_empty() || b.is_zero() {
            return;
        }
        let bounds = shape_bounds(rect, shape.rotation).intersect(&self.clip);
        if bounds.is_empty() {
            return;
        }
        let radii = shape.radii.clamped(rect.w, rect.h);
        let inner_rect = IRect {
            x: rect.x + b.left as i32,
            y: rect.y + b.top as i32,
            w: rect.w - b.left as i32 - b.right as i32,
            h: rect.h - b.top as i32 - b.bottom as i32,
        };
        let has_inner = !inner_rect.is_empty();
        let inner_radii = Radii {
            tl: radii.tl.saturating_sub(b.left.max(b.top)),
            tr: radii.tr.saturating_sub(b.right.max(b.top)),
            br: radii.br.saturating_sub(b.right.max(b.bottom)),
            bl: radii.bl.saturating_sub(b.left.max(b.bottom)),
        }
        .clamped(inner_rect.w, inner_rect.h);
        cache.ensure_corners(&radii);
        cache.ensure_corners(&inner_radii);
        let cache: &Cache = &*cache;
        let outer = Prepared::new(cache, rect, radii, shape.rotation, None);
        let pivot = centre_q8(&rect);
        let inner = if has_inner { Some(Prepared::new(cache, inner_rect, inner_radii, shape.rotation, Some(pivot))) } else { None };
        let geom = ShadeGeom::new(rect, shape.rotation);
        let pre = PaintPre::new(paint, rect);
        let n = bounds.w as usize;
        let mut cov = vec![0u8; n];
        let mut cov2 = vec![0u8; n];
        let mut rowbuf: Vec<u32> = if matches!(pre, PaintPre::Solid(_)) { Vec::new() } else { vec![0u32; n] };
        for y in bounds.y..bounds.bottom() {
            let mut kind = outer.row(y, bounds.x, bounds.right(), &mut cov);
            if kind == RowKind::Empty {
                continue;
            }
            if let Some(inn) = &inner {
                match inn.row(y, bounds.x, bounds.right(), &mut cov2) {
                    RowKind::Empty => {}
                    RowKind::Full => continue, // the interior covers the whole row: no ring here
                    RowKind::Partial => {
                        let mut i = 0;
                        while i < n {
                            let c2 = cov2[i];
                            if c2 == 255 {
                                cov[i] = 0;
                            } else if c2 != 0 {
                                cov[i] = mul255(cov[i] as u32, 255 - c2 as u32) as u8;
                            }
                            i += 1;
                        }
                        kind = RowKind::Partial;
                    }
                }
            }
            self.emit_row(&pre, &geom, y, bounds.x, &cov, kind, &mut rowbuf, opacity as u32);
        }
        self.mark_dirty(bounds);
    }

    // ----- masks and shadows -----

    /// Blend `color` through an 8-bit coverage mask (glyphs, shadows, ...).
    pub fn blit_mask(&mut self, mask: &[u8], mw: i32, mh: i32, x: i32, y: i32, color: Color32, opacity: u8) {
        if mw <= 0 || mh <= 0 || opacity == 0 || mask.len() < mw as usize * mh as usize {
            return;
        }
        let dst = IRect::new(x, y, mw, mh).intersect(&self.clip);
        if dst.is_empty() {
            return;
        }
        let n = dst.w as usize;
        for yy in dst.y..dst.bottom() {
            let start = ((yy - y) as usize) * mw as usize + (dst.x - x) as usize;
            self.composite_cov_solid(yy, dst.x, &mask[start..start + n], color, opacity as u32);
        }
        self.mark_dirty(dst);
    }

    /// Draw a blurred drop shadow for `shape`. Masks are cached, so repeated
    /// draws of the same shadow cost one blit.
    pub fn draw_shadow(&mut self, shape: &Shape, sh: &Shadow, opacity: u8, cache: &mut Cache) {
        let rect = shape.rect.sane();
        if rect.is_empty() || opacity == 0 || sh.color.a == 0 {
            return;
        }
        let spread = sh.spread.clamp(-512, 512);
        let sr = IRect { x: rect.x - spread, y: rect.y - spread, w: rect.w + 2 * spread, h: rect.h + 2 * spread };
        if sr.is_empty() {
            return;
        }
        // CSS: a square corner stays square however much the shadow spreads.
        let grow = |r: u16| if r == 0 { 0 } else { (r as i32 + spread).clamp(0, MAX_RADIUS as i32) as u16 };
        let radii = Radii { tl: grow(shape.radii.tl), tr: grow(shape.radii.tr), br: grow(shape.radii.br), bl: grow(shape.radii.bl) }
            .clamped(sr.w, sr.h);
        let own_radii = shape.radii.clamped(rect.w, rect.h);
        let own_rot = shape.rotation;
        cache.ensure_corners(&radii);
        cache.ensure_corners(&own_radii);

        // Blur cost grows with mask area, so shrink the blur until the mask fits.
        let bb = shape_bounds(sr, shape.rotation);
        let mut blur = sh.blur.min(256);
        let mut margin;
        loop {
            margin = blur as i32 + blur as i32 / 2 + 1;
            let (mw, mh) = (bb.w as i64 + 2 * margin as i64, bb.h as i64 + 2 * margin as i64);
            if blur == 0 || mw * mh <= MAX_SHADOW_PIXELS as i64 {
                break;
            }
            blur -= (blur / 4).max(1);
        }

        // A hard-edged shadow needs no mask at all: draw it row by row.
        if blur == 0 {
            let dx = sh.dx.clamp(-COORD_LIMIT, COORD_LIMIT);
            let dy = sh.dy.clamp(-COORD_LIMIT, COORD_LIMIT);
            let moved = IRect { x: sr.x + dx, y: sr.y + dy, w: sr.w, h: sr.h };
            let bounds = shape_bounds(moved, own_rot).intersect(&self.clip);
            if bounds.is_empty() {
                return;
            }
            let cache: &Cache = &*cache;
            let prep = Prepared::new(cache, moved, radii, own_rot, None);
            let own = if sh.knockout { Some(Prepared::new(cache, rect, own_radii, own_rot, None)) } else { None };
            let n = bounds.w as usize;
            let mut cov = vec![0u8; n];
            let mut kbuf = vec![0u8; n];
            for y in bounds.y..bounds.bottom() {
                let kind = prep.row(y, bounds.x, bounds.right(), &mut cov);
                if kind == RowKind::Empty {
                    continue;
                }
                let mut full = kind == RowKind::Full;
                if let Some(p) = &own {
                    if p.row(y, bounds.x, bounds.right(), &mut kbuf) != RowKind::Empty {
                        full = false;
                        let mut i = 0;
                        while i < n {
                            let k = kbuf[i];
                            if k == 255 {
                                cov[i] = 0;
                            } else if k != 0 {
                                cov[i] = mul255(cov[i] as u32, 255 - k as u32) as u8;
                            }
                            i += 1;
                        }
                    }
                }
                if full {
                    self.blend_span_const(y, bounds.x, bounds.right(), sh.color, opacity as u32);
                } else {
                    self.composite_cov_solid(y, bounds.x, &cov, sh.color, opacity as u32);
                }
            }
            self.mark_dirty(bounds);
            return;
        }

        let (mw, mh) = (bb.w + 2 * margin, bb.h + 2 * margin);
        if mw <= 0 || mh <= 0 {
            return;
        }
        let key = ShadowKey { w: sr.w, h: sr.h, radii: [radii.tl, radii.tr, radii.br, radii.bl], rot: shape.rotation.normalized(), blur };
        let (lx0, ly0) = ((mw - sr.w) / 2, (mh - sr.h) / 2);

        cache.tick += 1;
        let tick = cache.tick;
        if let Some(i) = cache.shadows.iter().position(|e| e.key == key) {
            cache.shadow_hits += 1;
            cache.shadows[i].last_used = tick;
        } else {
            cache.shadow_misses += 1;
            let total = mw as usize * mh as usize;
            let mut mask: Vec<u8> = Vec::new();
            if mask.try_reserve_exact(total).is_err() {
                return;
            }
            mask.resize(total, 0);
            {
                let c: &Cache = &*cache;
                let local = IRect::new(lx0, ly0, sr.w, sr.h);
                let prep = Prepared::new(c, local, radii, shape.rotation, None);
                for y in 0..mh {
                    let o = y as usize * mw as usize;
                    prep.row(y, 0, mw, &mut mask[o..o + mw as usize]);
                }
            }
            blur_mask(&mut mask, mw as usize, mh as usize, blur);
            cache.shadow_bytes += mask.len();
            cache.shadows.push(ShadowEntry { key, last_used: tick, mw, mh, data: mask });
            while cache.shadow_bytes > MAX_SHADOW_BYTES && cache.shadows.len() > 1 {
                let last = cache.shadows.len() - 1;
                let victim = cache.shadows[..last].iter().enumerate().min_by_key(|(_, e)| e.last_used).map(|(i, _)| i);
                match victim {
                    Some(v) => {
                        let gone = cache.shadows.swap_remove(v);
                        cache.shadow_bytes -= gone.data.len();
                    }
                    None => break,
                }
            }
        }

        let cache: &Cache = &*cache;
        let entry = match cache.shadows.iter().find(|e| e.key == key) {
            Some(e) => e,
            None => return,
        };
        let dx = sh.dx.clamp(-COORD_LIMIT, COORD_LIMIT);
        let dy = sh.dy.clamp(-COORD_LIMIT, COORD_LIMIT);
        let (ox, oy) = (sr.x - lx0 + dx, sr.y - ly0 + dy);
        let dst = IRect::new(ox, oy, entry.mw, entry.mh).intersect(&self.clip);
        if dst.is_empty() {
            return;
        }
        let n = dst.w as usize;
        let own = if sh.knockout { Some(Prepared::new(cache, rect, own_radii, shape.rotation, None)) } else { None };
        let mut cov = vec![0u8; n];
        let mut kbuf = vec![0u8; n];
        for y in dst.y..dst.bottom() {
            let start = ((y - oy) as usize) * entry.mw as usize + (dst.x - ox) as usize;
            cov.copy_from_slice(&entry.data[start..start + n]);
            if let Some(p) = &own {
                if p.row(y, dst.x, dst.right(), &mut kbuf) != RowKind::Empty {
                    let mut i = 0;
                    while i < n {
                        let k = kbuf[i];
                        if k == 255 {
                            cov[i] = 0;
                        } else if k != 0 {
                            cov[i] = mul255(cov[i] as u32, 255 - k as u32) as u8;
                        }
                        i += 1;
                    }
                }
            }
            self.composite_cov_solid(y, dst.x, &cov, sh.color, opacity as u32);
        }
        self.mark_dirty(dst);
    }

    // ----- layers -----

    /// Composite a layer 1:1 with its top-left at (x, y).
    pub fn draw_layer(&mut self, layer: &Layer, x: i32, y: i32, opacity: u8) {
        if opacity == 0 {
            return;
        }
        let dst = IRect::new(x, y, layer.w, layer.h).intersect(&self.clip);
        if dst.is_empty() {
            return;
        }
        let amask = self.amask;
        let n = dst.w as usize;
        for yy in dst.y..dst.bottom() {
            let start = ((yy - y) as usize) * layer.w as usize + (dst.x - x) as usize;
            let src = &layer.px[start..start + n];
            let row = self.row_mut(yy, dst.x, n);
            let mut i = 0;
            while i < n {
                row[i] = over_px(row[i], src[i], opacity as u32, amask);
                i += 1;
            }
        }
        self.mark_dirty(dst);
    }

    /// Composite a layer scaled (permille, 1000 = 1:1) and rotated clockwise
    /// about its centre, which is placed at (cx, cy).
    pub fn draw_layer_xf(&mut self, layer: &Layer, cx: i32, cy: i32, rot: Angle, scale_pm: u32, opacity: u8, smooth: bool) {
        if opacity == 0 {
            return;
        }
        let sc = scale_pm.clamp(1, 16000) as i64;
        let (dw, dh) = ((layer.w as i64 * sc / 1000).clamp(1, 1 << 16) as i32, (layer.h as i64 * sc / 1000).clamp(1, 1 << 16) as i32);
        let cx = cx.clamp(-COORD_LIMIT, COORD_LIMIT);
        let cy = cy.clamp(-COORD_LIMIT, COORD_LIMIT);
        let rect = IRect { x: cx - dw / 2, y: cy - dh / 2, w: dw, h: dh };
        let bounds = shape_bounds(rect, rot).intersect(&self.clip);
        if bounds.is_empty() {
            return;
        }
        let xf = Xform::new(rot, cx as i64 * 256, cy as i64 * 256);
        let inv_s = (1000i64 << 16) / sc;
        let (half_w, half_h) = ((layer.w as i64) << 15, (layer.h as i64) << 15);
        let tex = layer.texture();
        let amask = self.amask;
        let n = bounds.w as usize;
        let (dx, dy) = xf.step();
        for y in bounds.y..bounds.bottom() {
            let (mut lx, mut ly) = xf.start(bounds.x, y);
            let row = self.row_mut(y, bounds.x, n);
            for d in row.iter_mut() {
                let u = (((lx >> 7) * inv_s) >> 16) + half_w;
                let v = (((ly >> 7) * inv_s) >> 16) + half_h;
                let s = sample(&tex, u, v, Wrap::Transparent, smooth);
                *d = over_px(*d, s, opacity as u32, amask);
                lx += dx;
                ly += dy;
            }
        }
        self.mark_dirty(bounds);
    }
}

// =====================================================================
// -----| demo: test card |-----
// =====================================================================

/// A visual test card that exercises every renderer feature. The kernel draws
/// it on screen (GFXTEST), the host test harness renders it to a PNG. Laid out
/// for 1024x768; smaller canvases simply clip it.
pub mod demo {
    use super::*;

    const ACCENT: Color32 = Color32::rgb(88, 130, 220);
    const PINK: Color32 = Color32::rgb(235, 90, 140);
    const GREEN: Color32 = Color32::rgb(80, 200, 150);
    const YELLOW: Color32 = Color32::rgb(240, 190, 70);
    const PANEL: Color32 = Color32::rgb(36, 38, 56);

    /// 64x64 RGBA test image: colour ramps inside a soft-edged disc, transparent outside.
    pub fn demo_image() -> Vec<u8> {
        let mut v = Vec::with_capacity(64 * 64 * 4);
        for y in 0..64i32 {
            for x in 0..64i32 {
                let (dx, dy) = (x * 2 - 63, y * 2 - 63);
                let d2 = dx * dx + dy * dy; // 0 at the centre, 63^2*2 at corners
                let edge = (60 * 60 - d2).clamp(0, 600) * 255 / 600;
                v.push((x * 4) as u8);
                v.push((y * 4) as u8);
                v.push((255 - x * 2 - y * 2).clamp(0, 255) as u8);
                v.push(edge as u8);
            }
        }
        v
    }

    fn slot(i: i32, n: i32, margin: i32, gap: i32, total: i32) -> (i32, i32) {
        let w = (total - 2 * margin - gap * (n - 1)) / n;
        (margin + i * (w + gap), w)
    }

    fn checker(c: &mut Canvas, r: IRect) {
        let prev = c.push_clip(r);
        c.fill_rect(r, Color32::rgb(70, 70, 78), 255);
        let mut y = r.y;
        while y < r.bottom() {
            let mut x = r.x;
            while x < r.right() {
                if ((x - r.x) / 8 + (y - r.y) / 8) % 2 == 0 {
                    c.fill_rect(IRect::new(x, y, 8, 8), Color32::rgb(110, 110, 120), 255);
                }
                x += 8;
            }
            y += 8;
        }
        c.pop_clip(prev);
    }

    /// Size of a layer that holds the floating taskbar plus its shadow.
    pub const TASKBAR_LAYER_W: i32 = 660;
    pub const TASKBAR_LAYER_H: i32 = 130;
    /// Where the bar's top-left sits inside that layer.
    pub const TASKBAR_IN_LAYER: (i32, i32) = (50, 28);

    /// The floating glass taskbar (560x52) with its top-left at (x, y): the shape
    /// this renderer exists for. Used on the card and, via `CachedLayer`, as the
    /// "draw once" example.
    pub fn draw_taskbar(c: &mut Canvas, cache: &mut Cache, x: i32, y: i32) {
        let bar = Shape::rounded(IRect::new(x, y, 560, 52), 26);
        c.draw_shadow(&bar, &Shadow { dx: 0, dy: 10, blur: 30, spread: -4, color: Color32::rgba(0, 0, 0, 170), knockout: true }, 255, cache);
        c.fill_shape(&bar, &Paint::Solid(Color32::rgba(24, 26, 40, 210)), 255, cache);
        c.fill_shape(&bar, &Paint::Linear { angle: Angle::degrees(180), stops: &[Stop { pos: 0, color: Color32::rgba(255, 255, 255, 34) }, Stop { pos: 600, color: Color32::rgba(255, 255, 255, 0) }] }, 255, cache);
        c.stroke_shape(&bar, &Borders::uniform(1), &Paint::Solid(Color32::rgba(255, 255, 255, 46)), 255, cache);
        c.fill_shape(&Shape::rounded(IRect::new(x + 14, y + 8, 36, 36), 18), &Paint::Linear { angle: Angle::degrees(135), stops: &[Stop { pos: 0, color: ACCENT }, Stop { pos: 1000, color: PINK }] }, 255, cache);
        let app_cols = [GREEN, YELLOW, PINK, Color32::rgb(150, 110, 240)];
        for (i, col) in app_cols.iter().enumerate() {
            c.fill_shape(&Shape::rounded(IRect::new(x + 72 + i as i32 * 48, y + 10, 32, 32), 9), &Paint::Solid(*col), 255, cache);
        }
        c.fill_shape(&Shape::rounded(IRect::new(x + 130, y + 45, 12, 3), 2), &Paint::Solid(Color32::WHITE), 255, cache);
        c.fill_shape(&Shape::rounded(IRect::new(x + 458, y + 14, 88, 24), 12), &Paint::Solid(Color32::rgba(255, 255, 255, 36)), 255, cache);
    }

    pub fn draw_test_card(c: &mut Canvas, cache: &mut Cache) {
        draw_test_card_timed(c, cache, &mut |_| {});
    }

    /// Same card, calling `mark(label)` after each band so a caller can time it
    /// (the kernel prints rdtsc deltas, the host harness prints microseconds).
    pub fn draw_test_card_timed(c: &mut Canvas, cache: &mut Cache, mark: &mut dyn FnMut(&'static str)) {
        let (cw, _) = (c.bounds().w, c.bounds().h);
        let full = IRect::new(0, 0, c.bounds().w, c.bounds().h);
        // Background: diagonal gradient with a mid stop.
        let bg = [
            Stop { pos: 0, color: Color32::rgb(14, 16, 30) },
            Stop { pos: 550, color: Color32::rgb(30, 24, 58) },
            Stop { pos: 1000, color: Color32::rgb(52, 30, 70) },
        ];
        c.fill_shape(&Shape::rect(full), &Paint::Linear { angle: Angle::degrees(160), stops: &bg }, 255, cache);
        mark("background gradient (full screen)");

        // Band 1: corner radii.
        let radii = [0u16, 6, 16, 28, 35];
        for (i, r) in radii.iter().enumerate() {
            let (x, w) = slot(i as i32, 6, 24, 20, cw);
            c.fill_shape(&Shape::rounded(IRect::new(x, 20, w, 70), *r), &Paint::Solid(ACCENT), 255, cache);
        }
        let (x, w) = slot(5, 6, 24, 20, cw);
        let mixed = Shape { rect: IRect::new(x, 20, w, 70), radii: Radii { tl: 30, tr: 0, br: 30, bl: 0 }, rotation: Angle::ZERO };
        c.fill_shape(&mixed, &Paint::Solid(PINK), 255, cache);
        mark("band1 corner radii");

        // Band 2: borders.
        let bord_fill = Paint::Solid(PANEL);
        let (x, w) = slot(0, 5, 24, 24, cw);
        let s = Shape::rect(IRect::new(x, 120, w, 90));
        c.fill_shape(&s, &bord_fill, 255, cache);
        c.stroke_shape(&s, &Borders::uniform(2), &Paint::Solid(ACCENT), 255, cache);
        let (x, w) = slot(1, 5, 24, 24, cw);
        let s = Shape::rect(IRect::new(x, 120, w, 90));
        c.fill_shape(&s, &bord_fill, 255, cache);
        c.stroke_shape(&s, &Borders { top: 5, right: 0, bottom: 0, left: 0 }, &Paint::Solid(YELLOW), 255, cache);
        let (x, w) = slot(2, 5, 24, 24, cw);
        let s = Shape::rounded(IRect::new(x, 120, w, 90), 22);
        let rainbow = [Stop { pos: 0, color: PINK }, Stop { pos: 500, color: YELLOW }, Stop { pos: 1000, color: GREEN }];
        c.fill_shape(&s, &bord_fill, 255, cache);
        c.stroke_shape(&s, &Borders::uniform(4), &Paint::Linear { angle: Angle::degrees(120), stops: &rainbow }, 255, cache);
        let (x, w) = slot(3, 5, 24, 24, cw);
        let s = Shape::rounded(IRect::new(x, 120, w, 90), 26);
        c.fill_shape(&s, &Paint::Linear { angle: Angle::degrees(90), stops: &[Stop { pos: 0, color: PINK }, Stop { pos: 1000, color: ACCENT }] }, 255, cache);
        c.stroke_shape(&s, &Borders::uniform(8), &Paint::Solid(Color32::rgba(255, 255, 255, 90)), 255, cache);
        let (x, w) = slot(4, 5, 24, 24, cw);
        let s = Shape::rounded(IRect::new(x, 120, w, 90), 18);
        c.fill_shape(&s, &bord_fill, 255, cache);
        c.stroke_shape(&s, &Borders { top: 6, right: 12, bottom: 2, left: 2 }, &Paint::Solid(GREEN), 255, cache);
        mark("band2 borders");

        // Band 3: gradients.
        let (x, w) = slot(0, 6, 24, 18, cw);
        let sunset = [Stop { pos: 0, color: Color32::rgb(255, 94, 98) }, Stop { pos: 500, color: Color32::rgb(255, 195, 113) }, Stop { pos: 1000, color: Color32::rgb(255, 255, 160) }];
        c.fill_shape(&Shape::rect(IRect::new(x, 240, w, 100)), &Paint::Linear { angle: Angle::degrees(90), stops: &sunset }, 255, cache);
        let (x, w) = slot(1, 6, 24, 18, cw);
        let ocean = [Stop { pos: 0, color: Color32::rgb(20, 30, 90) }, Stop { pos: 1000, color: Color32::rgb(60, 220, 230) }];
        c.fill_shape(&Shape::rect(IRect::new(x, 240, w, 100)), &Paint::Linear { angle: Angle::degrees(45), stops: &ocean }, 255, cache);
        let (x, w) = slot(2, 6, 24, 18, cw);
        c.fill_shape(&Shape::rounded(IRect::new(x, 240, w, 100), 24), &Paint::Linear { angle: Angle::degrees(180), stops: &[Stop { pos: 0, color: Color32::rgb(180, 120, 255) }, Stop { pos: 1000, color: Color32::rgb(60, 20, 120) }] }, 255, cache);
        let (x, w) = slot(3, 6, 24, 18, cw);
        c.fill_shape(&Shape::rounded(IRect::new(x, 240, w, 100), 12), &Paint::Radial { cx: 350, cy: 350, stops: &[Stop { pos: 0, color: Color32::rgb(255, 255, 255) }, Stop { pos: 350, color: GREEN }, Stop { pos: 1000, color: Color32::rgb(10, 40, 40) }] }, 255, cache);
        let (x, w) = slot(4, 6, 24, 18, cw);
        let r = IRect::new(x, 240, w, 100);
        checker(c, r);
        c.fill_shape(&Shape::rect(r), &Paint::Linear { angle: Angle::degrees(90), stops: &[Stop { pos: 0, color: Color32::rgba(255, 60, 120, 255) }, Stop { pos: 1000, color: Color32::rgba(255, 60, 120, 0) }] }, 255, cache);
        let (x, w) = slot(5, 6, 24, 18, cw);
        let rot = Shape::rounded(IRect::new(x + 6, 252, w - 12, 76), 20).with_rotation(Angle::degrees(20));
        c.fill_shape(&rot, &Paint::Linear { angle: Angle::degrees(90), stops: &rainbow }, 255, cache);
        mark("band3 gradients");

        // Band 4: shadows.
        let (x, w) = slot(0, 5, 24, 30, cw);
        let card = Shape::rounded(IRect::new(x, 380, w, 90), 16);
        c.draw_shadow(&card, &Shadow { dx: 0, dy: 10, blur: 28, spread: -2, color: Color32::rgba(0, 0, 0, 170), knockout: true }, 255, cache);
        c.fill_shape(&card, &Paint::Solid(PANEL), 255, cache);
        let (x, w) = slot(1, 5, 24, 30, cw);
        let card = Shape::rounded(IRect::new(x, 380, w, 90), 6);
        c.draw_shadow(&card, &Shadow { dx: 0, dy: 3, blur: 6, spread: 0, color: Color32::rgba(0, 0, 0, 200), knockout: true }, 255, cache);
        c.fill_shape(&card, &Paint::Solid(PANEL), 255, cache);
        let (x, w) = slot(2, 5, 24, 30, cw);
        let card = Shape::rounded(IRect::new(x, 380, w, 90), 20);
        c.draw_shadow(&card, &Shadow { dx: 0, dy: 0, blur: 26, spread: 4, color: Color32::rgba(88, 130, 220, 200), knockout: true }, 255, cache);
        c.fill_shape(&card, &Paint::Solid(PANEL), 255, cache);
        let (x, w) = slot(3, 5, 24, 30, cw);
        let stripe = [Stop { pos: 0, color: PINK }, Stop { pos: 500, color: YELLOW }, Stop { pos: 1000, color: ACCENT }];
        c.fill_shape(&Shape::rect(IRect::new(x - 10, 395, w + 20, 60)), &Paint::Linear { angle: Angle::degrees(90), stops: &stripe }, 255, cache);
        let glass = Shape::rounded(IRect::new(x + 8, 378, w - 16, 94), 18);
        c.draw_shadow(&glass, &Shadow { dx: 0, dy: 8, blur: 20, spread: 0, color: Color32::rgba(0, 0, 0, 160), knockout: true }, 255, cache);
        c.fill_shape(&glass, &Paint::Solid(Color32::rgba(255, 255, 255, 60)), 255, cache);
        c.stroke_shape(&glass, &Borders::uniform(1), &Paint::Solid(Color32::rgba(255, 255, 255, 120)), 255, cache);
        let (x, w) = slot(4, 5, 24, 30, cw);
        let card = Shape::rounded(IRect::new(x, 380, w, 80), 0);
        c.draw_shadow(&card, &Shadow { dx: 9, dy: 9, blur: 0, spread: 0, color: Color32::rgba(0, 0, 0, 255), knockout: true }, 255, cache);
        c.fill_shape(&card, &Paint::Solid(YELLOW), 255, cache);
        mark("band4 shadows");

        // Band 5: images.
        let img = demo_image();
        let view = ImageView { w: 64, h: 64, channels: 4, data: &img };
        for (i, fit) in [Fit::Stretch, Fit::Contain, Fit::Cover, Fit::Tile, Fit::Center].iter().enumerate() {
            let (x, w) = slot(i as i32, 6, 24, 18, cw);
            let r = IRect::new(x, 500, w, 90);
            let s = Shape::rounded(r, 14);
            c.fill_shape(&s, &Paint::Solid(PANEL), 255, cache);
            c.fill_shape(&s, &Paint::Image(ImagePaint { image: view, fit: *fit, smooth: true }), 255, cache);
            c.stroke_shape(&s, &Borders::uniform(1), &Paint::Solid(Color32::rgba(255, 255, 255, 60)), 255, cache);
        }
        let (x, w) = slot(5, 6, 24, 18, cw);
        let r = IRect::new(x + 10, 508, w - 20, 74);
        c.fill_shape(&Shape::rounded(r, 14).with_rotation(Angle::degrees(-14)), &Paint::Image(ImagePaint { image: view, fit: Fit::Cover, smooth: true }), 255, cache);
        mark("band5 images");

        // Band 6: rotations, group opacity, rotated layer.
        for (i, deg) in [0, 15, 30, 45, 60, 90].iter().enumerate() {
            let x = 40 + i as i32 * 62;
            c.fill_shape(&Shape::rounded(IRect::new(x, 625, 46, 22), 8).with_rotation(Angle::degrees(*deg)), &Paint::Solid(if i % 2 == 0 { ACCENT } else { PINK }), 255, cache);
        }
        // Group vs per-element opacity.
        if let Some(mut l) = Layer::new(90, 50) {
            {
                let mut lc = Canvas::from_layer(&mut l);
                lc.fill_shape(&Shape::rounded(IRect::new(0, 0, 60, 50), 12), &Paint::Solid(PINK), 255, cache);
                lc.fill_shape(&Shape::rounded(IRect::new(30, 0, 60, 50), 12), &Paint::Solid(ACCENT), 255, cache);
            }
            checker(c, IRect::new(430, 610, 220, 70));
            c.draw_layer(&l, 440, 620, 128);
            c.fill_shape(&Shape::rounded(IRect::new(550, 620, 60, 50), 12), &Paint::Solid(PINK), 128, cache);
            c.fill_shape(&Shape::rounded(IRect::new(580, 620, 60, 50), 12), &Paint::Solid(ACCENT), 128, cache);
        }
        // Rotated layer containing a card.
        if let Some(mut l) = Layer::new(150, 80) {
            {
                let mut lc = Canvas::from_layer(&mut l);
                let card = Shape::rounded(IRect::new(6, 6, 138, 68), 16);
                lc.draw_shadow(&card, &Shadow { dx: 0, dy: 4, blur: 8, spread: 0, color: Color32::rgba(0, 0, 0, 160), knockout: true }, 255, cache);
                lc.fill_shape(&card, &Paint::Linear { angle: Angle::degrees(135), stops: &[Stop { pos: 0, color: PINK }, Stop { pos: 1000, color: ACCENT }] }, 255, cache);
                lc.stroke_shape(&card, &Borders::uniform(2), &Paint::Solid(Color32::rgba(255, 255, 255, 140)), 255, cache);
            }
            c.draw_layer_xf(&l, 800, 645, Angle::degrees(-8), 1000, 255, true);
            c.draw_layer_xf(&l, 940, 645, Angle::degrees(20), 600, 200, true);
        }
        mark("band6 rotations + layers");

        draw_taskbar(c, cache, 232, 704);
        mark("glass taskbar");
    }
}
