// =============================================================================
// VEILOS: logical GPU (HAL step 7C)
// =============================================================================
//
// Software hands pixel work to one logical GPU as a list of `Op`s against a
// target `Surface`. `execute` cuts the target into horizontal bands (tile
// split, HAL_DESIGN.md) and renders the bands in parallel on every CPU
// through the worker pool. Each band clips every op to itself, so no two
// CPUs ever write the same pixel and the result is identical to a serial
// render, in any order.
//
// Backends: today the only one is `CpuRaster`, which any CPU in the pool
// runs. The `Backend` trait is the seam for hardware GPUs and for mesh nodes
// (`distributed::remote_gpu_submit`, step 7E): a band can be given to
// whichever backend owns it.
//
// Desktop frames go through a display list (part 2): while a frame is being
// painted (`ui::BackBufferGuard`), fills, text, the wallpaper blit and large
// `render2d` shapes (`ShapeOffload`) are recorded instead of drawn, and the
// list is rendered banded on every CPU at once, so even small ops are shared
// out. Code that still needs the raw back buffer (image decoders, the
// `render2d` canvas for anything it doesn't offload) gets it from
// `ui::painter_ptr_flushed` or `ShapeOffload::sync`, which render everything
// recorded so far first, so drawing order is never changed. The back-buffer
// -> framebuffer flush and `gpu_driver` command batches are direct `execute`s.

use alloc::boxed::Box;
use alloc::vec::Vec;
use spin::Mutex;

use crate::render2d::{self, Borders, Cache, Canvas, IRect, Paint, Shape, Stop};
use crate::workers;

/// A 32-bit-per-pixel image somewhere in kernel memory.
#[derive(Clone, Copy)]
pub struct Surface {
    pub ptr: *mut u32,
    pub width: usize,
    pub height: usize,
    /// Pixels per row (>= width).
    pub stride: usize,
}

// Bands write disjoint rows of a surface; reads of a source surface are shared.
unsafe impl Send for Surface {}
unsafe impl Sync for Surface {}

/// A pixel rectangle, `x0..x1` × `y0..y1`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x0: usize,
    pub y0: usize,
    pub x1: usize,
    pub y1: usize,
}

impl Rect {
    pub const fn new(x0: usize, y0: usize, x1: usize, y1: usize) -> Self {
        Self { x0, y0, x1, y1 }
    }

    fn intersect(&self, o: &Rect) -> Option<Rect> {
        let r = Rect::new(self.x0.max(o.x0), self.y0.max(o.y0), self.x1.min(o.x1), self.y1.min(o.y1));
        (r.x0 < r.x1 && r.y0 < r.y1).then_some(r)
    }
}

#[derive(Clone, Copy)]
pub enum Op {
    /// Fill `rect` with `color` (0x00RRGGBB).
    Fill { rect: Rect, color: u32 },
    /// Copy pixels into `dst`: target pixel (x, y) takes source pixel
    /// (x - dst.x0 + src_x, y - dst.y0 + src_y). Source pixels equal to
    /// `transparent` are skipped.
    Copy { src: Surface, src_x: usize, src_y: usize, dst: Rect, transparent: Option<u32> },
    /// An 8×8 1-bit glyph (`bits`: 8 rows, MSB = leftmost pixel) at (x, y)
    /// in `color`, drawn only inside `clip`.
    Glyph8 { bits: &'static [u8], x: usize, y: usize, color: u32, clip: Rect },
    /// A `render2d` shape fill or stroke (anti-aliased, rounded, gradient),
    /// touching only `bounds`. The record lives in the display list until
    /// the list is rendered.
    Shape { rec: ShapeRef, bounds: Rect },
}

/// A `render2d` shape draw with everything it needs owned, so it can wait in
/// the display list after the caller's borrowed paint is gone.
pub struct ShapeRec {
    shape: Shape,
    borders: Option<Borders>,
    paint: OwnedPaint,
    opacity: u8,
}

enum OwnedPaint {
    Solid(render2d::Color32),
    Linear { angle: render2d::Angle, stops: Vec<Stop> },
    Radial { cx: u16, cy: u16, stops: Vec<Stop> },
}

impl OwnedPaint {
    /// Image paints borrow their pixels, so they are never offloaded.
    fn from(p: &Paint) -> Option<OwnedPaint> {
        Some(match *p {
            Paint::Solid(c) => OwnedPaint::Solid(c),
            Paint::Linear { angle, stops } => OwnedPaint::Linear { angle, stops: stops.to_vec() },
            Paint::Radial { cx, cy, stops } => OwnedPaint::Radial { cx, cy, stops: stops.to_vec() },
            Paint::Image(_) => return None,
        })
    }

    fn paint(&self) -> Paint<'_> {
        match self {
            OwnedPaint::Solid(c) => Paint::Solid(*c),
            OwnedPaint::Linear { angle, stops } => Paint::Linear { angle: *angle, stops },
            OwnedPaint::Radial { cx, cy, stops } => Paint::Radial { cx: *cx, cy: *cy, stops },
        }
    }
}

/// Points at a `ShapeRec` boxed in the display list; read-only while bands render.
#[derive(Clone, Copy)]
pub struct ShapeRef(*const ShapeRec);

unsafe impl Send for ShapeRef {}
unsafe impl Sync for ShapeRef {}

/// Shape masks (rounded corners) built by each CPU's bands, kept across
/// frames like `render2d_kernel::GFX_CACHE`. One per CPU, so never contended.
static SHAPE_CACHES: [Mutex<Cache>; crate::smp::MAX_CPUS] = [const { Mutex::new(Cache::new()) }; crate::smp::MAX_CPUS];

/// Something that can render ops into one band of a target.
pub trait Backend: Sync {
    fn name(&self) -> &'static str;
    /// Render `ops`, in order, into the part of `target` inside `band`.
    fn render(&self, ops: &[Op], target: &Surface, band: Rect);
}

/// Renders on the CPU it is called on.
pub struct CpuRaster;

impl Backend for CpuRaster {
    fn name(&self) -> &'static str {
        "cpu-raster"
    }

    fn render(&self, ops: &[Op], target: &Surface, band: Rect) {
        let bounds = Rect::new(0, 0, target.width, target.height);
        let Some(band) = band.intersect(&bounds) else { return };
        for op in ops {
            match *op {
                Op::Fill { rect, color } => {
                    let Some(r) = rect.intersect(&band) else { continue };
                    for y in r.y0..r.y1 {
                        let row = unsafe {
                            core::slice::from_raw_parts_mut(target.ptr.add(y * target.stride + r.x0), r.x1 - r.x0)
                        };
                        row.fill(color);
                    }
                }
                Op::Glyph8 { bits, x, y, color, clip } => {
                    let Some(r) = Rect::new(x, y, x + 8, y + 8).intersect(&clip).and_then(|r| r.intersect(&band)) else { continue };
                    for py in r.y0..r.y1 {
                        let row_bits = bits[py - y];
                        for px in r.x0..r.x1 {
                            if row_bits & (0x80 >> (px - x)) != 0 {
                                unsafe { *target.ptr.add(py * target.stride + px) = color; }
                            }
                        }
                    }
                }
                Op::Shape { rec, bounds } => {
                    let Some(r) = bounds.intersect(&band) else { continue };
                    // SAFETY: the record stays boxed in the display list until
                    // `execute` returns (dl_flush).
                    let rec = unsafe { &*rec.0 };
                    // A canvas over just this band's rows, so no two CPUs ever
                    // hold the same pixels; the shape moves up by the band's
                    // first row (coverage and paint only depend on positions
                    // relative to the shape, so this is exact).
                    let rows = r.y1 - r.y0;
                    let px = unsafe { core::slice::from_raw_parts_mut(target.ptr.add(r.y0 * target.stride), rows * target.stride) };
                    let Some(mut c) = Canvas::new_opaque(px, target.stride as i32, rows as i32) else { continue };
                    c.set_clip(IRect::new(r.x0 as i32, 0, (r.x1 - r.x0) as i32, rows as i32));
                    let mut shape = rec.shape;
                    shape.rect.y -= r.y0 as i32;
                    let paint = rec.paint.paint();
                    let mut cache = SHAPE_CACHES[crate::smp::cpu_index()].lock();
                    match &rec.borders {
                        None => c.fill_shape(&shape, &paint, rec.opacity, &mut cache),
                        Some(b) => c.stroke_shape(&shape, b, &paint, rec.opacity, &mut cache),
                    }
                }
                Op::Copy { src, src_x, src_y, dst, transparent } => {
                    let Some(r) = dst.intersect(&band) else { continue };
                    let sx = src_x + (r.x0 - dst.x0);
                    let n = (r.x1 - r.x0).min(src.width.saturating_sub(sx));
                    if n == 0 {
                        continue;
                    }
                    for y in r.y0..r.y1 {
                        let sy = src_y + (y - dst.y0);
                        if sy >= src.height {
                            break;
                        }
                        unsafe {
                            let s = src.ptr.add(sy * src.stride + sx) as *const u32;
                            let d = target.ptr.add(y * target.stride + r.x0);
                            match transparent {
                                None => core::ptr::copy_nonoverlapping(s, d, n),
                                Some(key) => {
                                    for i in 0..n {
                                        let p = *s.add(i);
                                        if p != key {
                                            *d.add(i) = p;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

static CPU_RASTER: CpuRaster = CpuRaster;

/// Bands per CPU: a few more bands than CPUs evens out uneven bands
/// (a window covering only part of the screen) and a CPU that starts late.
const BANDS_PER_CPU: usize = 2;
/// Bands thinner than this cost more to hand out than they save.
const MIN_BAND_ROWS: usize = 32;
/// Jobs touching fewer pixels than this (a cursor-sized flush) run on the
/// calling CPU: waking the others would cost more than the work.
const MIN_PARALLEL_PIXELS: usize = 64 * 1024;

fn op_rect(op: &Op) -> Rect {
    match *op {
        Op::Fill { rect, .. } => rect,
        Op::Copy { dst, .. } => dst,
        Op::Glyph8 { x, y, clip, .. } => Rect::new(x, y, x + 8, y + 8).intersect(&clip).unwrap_or(Rect::new(0, 0, 0, 0)),
        Op::Shape { bounds, .. } => bounds,
    }
}

/// Render `ops` into `target`, split over every CPU. Returns when done.
pub fn execute(ops: &[Op], target: &Surface) {
    let bounds = Rect::new(0, 0, target.width, target.height);
    // Only the area the ops touch is split into bands.
    let Some(area) = ops
        .iter()
        .filter_map(|op| op_rect(op).intersect(&bounds))
        .reduce(|a, b| Rect::new(a.x0.min(b.x0), a.y0.min(b.y0), a.x1.max(b.x1), a.y1.max(b.y1)))
    else {
        return;
    };
    let rows = area.y1 - area.y0;
    let pixels = rows * (area.x1 - area.x0);
    let cpus = (crate::smp::CPU_COUNT.load(core::sync::atomic::Ordering::Relaxed) as usize).max(1);
    let bands = if pixels < MIN_PARALLEL_PIXELS { 1 } else { (cpus * BANDS_PER_CPU).min(rows / MIN_BAND_ROWS).max(1) };
    let backend: &dyn Backend = &CPU_RASTER;
    if bands == 1 {
        backend.render(ops, target, area);
        return;
    }
    let per_band = rows.div_ceil(bands);
    workers::for_each(bands, &|i| {
        let y0 = area.y0 + i * per_band;
        let y1 = (y0 + per_band).min(area.y1);
        if y0 < y1 {
            backend.render(ops, target, Rect::new(area.x0, y0, area.x1, y1));
        }
    });
}

// -----| frame display list |-----

struct DisplayList {
    target: Option<Surface>,
    ops: Vec<Op>,
    /// What the list's `Op::Shape`s point at.
    shapes: Vec<Box<ShapeRec>>,
}

/// Only the painting thread (the executor's GUI task) records and flushes;
/// the lock just makes that explicit.
static DISPLAY_LIST: Mutex<DisplayList> = Mutex::new(DisplayList { target: None, ops: Vec::new(), shapes: Vec::new() });

/// Start recording a frame for `target` (`ui::BackBufferGuard::acquire`).
pub fn dl_begin(target: Surface) {
    let mut dl = DISPLAY_LIST.lock();
    dl.target = Some(target);
    dl.ops.clear();
    dl.shapes.clear();
}

/// Record `op` into the current frame. Returns false if no frame is being
/// recorded; the caller then draws directly.
pub fn dl_record(op: Op) -> bool {
    let mut dl = DISPLAY_LIST.lock();
    if dl.target.is_none() {
        return false;
    }
    record_locked(&mut dl, op);
    true
}

fn record_locked(dl: &mut DisplayList, op: Op) {
    // Overdraw elimination: a large opaque op hides every earlier op it fully
    // covers (e.g. the frame clear under a full-screen wallpaper), so those
    // never need rendering. Only large ops check, to keep recording cheap.
    if let Some(cover) = opaque_rect(&op) {
        if (cover.x1 - cover.x0) * (cover.y1 - cover.y0) >= MIN_PARALLEL_PIXELS {
            dl.ops.retain(|old| {
                let r = op_rect(old);
                !(r.x0 >= cover.x0 && r.y0 >= cover.y0 && r.x1 <= cover.x1 && r.y1 <= cover.y1)
            });
        }
    }
    dl.ops.push(op);
}

/// Shapes smaller than this are drawn by the canvas on the spot: recording
/// one costs an allocation and a later flush, which only pays off when the
/// shape is big enough to be split over CPUs.
const MIN_OFFLOAD_PIXELS: usize = MIN_PARALLEL_PIXELS;

/// `render2d` canvases over the frame's back buffer hand large shapes to the
/// display list through this (`render2d_kernel::with_backbuffer`).
pub struct ShapeOffload;

impl render2d::Offload for ShapeOffload {
    fn shape(&self, shape: &Shape, borders: Option<&Borders>, paint: &Paint, opacity: u8, clip: IRect) -> bool {
        let b = render2d::shape_bounds(shape.rect, shape.rotation).intersect(&clip);
        if b.is_empty() || (b.w as usize) * (b.h as usize) < MIN_OFFLOAD_PIXELS {
            return false;
        }
        let Some(paint) = OwnedPaint::from(paint) else { return false };
        let mut dl = DISPLAY_LIST.lock();
        let Some(target) = dl.target else { return false };
        // The canvas clip is inside its bounds, which are the target's.
        let bounds = Rect::new(b.x as usize, b.y as usize, (b.x + b.w) as usize, (b.y + b.h) as usize);
        if bounds.x1 > target.width || bounds.y1 > target.height {
            return false;
        }
        let rec = Box::new(ShapeRec { shape: *shape, borders: borders.copied(), paint, opacity });
        let ptr: *const ShapeRec = &*rec;
        dl.shapes.push(rec);
        record_locked(&mut dl, Op::Shape { rec: ShapeRef(ptr), bounds });
        true
    }

    fn sync(&self) {
        dl_flush();
    }
}

/// The area `op` overwrites completely, if it is opaque.
fn opaque_rect(op: &Op) -> Option<Rect> {
    match *op {
        Op::Fill { rect, .. } => Some(rect),
        Op::Copy { dst, transparent: None, src, src_x, src_y } => {
            // Only the part the source actually covers is overwritten.
            let w = src.width.saturating_sub(src_x).min(dst.x1 - dst.x0);
            let h = src.height.saturating_sub(src_y).min(dst.y1 - dst.y0);
            Some(Rect::new(dst.x0, dst.y0, dst.x0 + w, dst.y0 + h))
        }
        Op::Shape { rec, bounds } => {
            // A plain opaque rectangle (e.g. a full-screen root panel).
            let rec = unsafe { &*rec.0 };
            let plain = rec.borders.is_none()
                && rec.opacity == 255
                && rec.shape.radii.is_zero()
                && rec.shape.rotation.is_zero()
                && matches!(rec.paint, OwnedPaint::Solid(c) if c.a == 255);
            plain.then_some(bounds)
        }
        _ => None,
    }
}

/// (flushes that rendered something, ops rendered, TSC cycles spent
/// rendering them) since the last call: diagnostics for the `[GUI] perf` line.
pub fn take_stats() -> (u64, u64, u64) {
    use core::sync::atomic::Ordering::Relaxed;
    (FLUSHES.swap(0, Relaxed), OPS.swap(0, Relaxed), FLUSH_CYCLES.swap(0, Relaxed))
}

static FLUSH_CYCLES: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

static FLUSHES: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
static OPS: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

/// Render everything recorded so far. Call before touching the target's
/// pixels directly (see `ui::painter_ptr_flushed`).
pub fn dl_flush() {
    let (target, ops, shapes) = {
        let mut dl = DISPLAY_LIST.lock();
        let Some(target) = dl.target else { return };
        if dl.ops.is_empty() {
            return;
        }
        (target, core::mem::take(&mut dl.ops), core::mem::take(&mut dl.shapes))
    };
    FLUSHES.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    OPS.fetch_add(ops.len() as u64, core::sync::atomic::Ordering::Relaxed);
    let t0 = crate::time::rdtsc();
    execute(&ops, &target);
    FLUSH_CYCLES.fetch_add(crate::time::rdtsc() - t0, core::sync::atomic::Ordering::Relaxed);
    // The ops that pointed at these are rendered.
    drop(shapes);
    // Hand the allocation back so the next frame doesn't regrow it.
    let mut dl = DISPLAY_LIST.lock();
    if dl.ops.is_empty() {
        let mut ops = ops;
        ops.clear();
        dl.ops = ops;
    }
}

/// Render what's left and stop recording (`ui::BackBufferGuard` drop).
pub fn dl_end() {
    dl_flush();
    DISPLAY_LIST.lock().target = None;
}
