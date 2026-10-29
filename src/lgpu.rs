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
// What goes through it now: clearing the desktop frame, the wallpaper blit
// and the flush of the back buffer to the framebuffer, the pixel-heavy part
// of each frame. Widget and text drawing still runs on one CPU through the
// global painter (`ui::PAINTER_PTR`, `ui::CLIP_*`), which isn't safe to run on
// several CPUs at once; moving it to ops is the next part of this step.

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
}

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
