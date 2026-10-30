//! Pixels: a `Canvas` is a 32bpp (0x00RRGGBB) target with a clip rectangle,
//! and these are the shapes GDI draws on it, with GDI's rules for which
//! pixels a shape covers (right and bottom edges excluded, the last point
//! of a line not drawn).

use alloc::vec::Vec;

#[derive(Clone, Copy)]
pub struct Canvas {
    pub ptr: *mut u32,
    pub stride: usize,
    /// Drawable area, device pixels, exclusive right/bottom.
    pub x0: i32,
    pub y0: i32,
    pub x1: i32,
    pub y1: i32,
}

/// COLORREF (0x00BBGGRR, palette flags in the top byte) to a pixel.
pub fn pixel(c: u32) -> u32 {
    ((c & 0xFF) << 16) | (c & 0xFF00) | ((c >> 16) & 0xFF)
}

/// A pixel back to a COLORREF.
pub fn colorref(p: u32) -> u32 {
    pixel(p & 0x00FF_FFFF)
}

impl Canvas {
    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x0 && x < self.x1 && y >= self.y0 && y < self.y1
    }

    pub fn get(&self, x: i32, y: i32) -> Option<u32> {
        self.contains(x, y).then(|| unsafe { *self.ptr.add(y as usize * self.stride + x as usize) } & 0x00FF_FFFF)
    }

    pub fn put(&self, x: i32, y: i32, p: u32, rop2: u32) {
        if self.contains(x, y) {
            let at = unsafe { self.ptr.add(y as usize * self.stride + x as usize) };
            unsafe { *at = mix2(rop2, p, *at) };
        }
    }

    /// Fill [x0, x1) x [y0, y1).
    pub fn fill(&self, x0: i32, y0: i32, x1: i32, y1: i32, p: u32, rop2: u32) {
        let (x0, x1) = (x0.max(self.x0), x1.min(self.x1));
        let (y0, y1) = (y0.max(self.y0), y1.min(self.y1));
        if x0 >= x1 || y0 >= y1 {
            return;
        }
        for y in y0..y1 {
            let row = unsafe { core::slice::from_raw_parts_mut(self.ptr.add(y as usize * self.stride + x0 as usize), (x1 - x0) as usize) };
            if rop2 == R2_COPYPEN {
                row.fill(p);
            } else {
                for d in row {
                    *d = mix2(rop2, p, *d);
                }
            }
        }
    }

    pub fn hline(&self, x0: i32, x1: i32, y: i32, p: u32, rop2: u32) {
        self.fill(x0, y, x1, y + 1, p, rop2);
    }
}

pub const R2_COPYPEN: u32 = 13;

/// Binary raster operation `rop2` (R2_*) of pen `p` and destination `d`.
pub fn mix2(rop2: u32, p: u32, d: u32) -> u32 {
    // R2 codes 1..16 are the 16 functions of (P, D): bit (P<<1 | D) of
    // code - 1, as in the ternary codes.
    let code = (rop2.clamp(1, 16) - 1) as u8;
    let mut r = 0u32;
    for i in 0..4u32 {
        if code & (1 << i) != 0 {
            let pp = if i & 2 != 0 { p } else { !p };
            let dd = if i & 1 != 0 { d } else { !d };
            r |= pp & dd;
        }
    }
    r & 0x00FF_FFFF
}

/// Ternary raster operation (the high word's low byte of a ROP3 code) of
/// pattern, source and destination.
pub fn mix3(code: u8, p: u32, s: u32, d: u32) -> u32 {
    match code {
        0xCC => s,
        0xF0 => p,
        0x00 => 0,
        0xFF => 0x00FF_FFFF,
        _ => {
            let mut r = 0u32;
            for i in 0..8u32 {
                if code & (1 << i) != 0 {
                    let pp = if i & 4 != 0 { p } else { !p };
                    let ss = if i & 2 != 0 { s } else { !s };
                    let dd = if i & 1 != 0 { d } else { !d };
                    r |= pp & ss & dd;
                }
            }
            r & 0x00FF_FFFF
        }
    }
}

/// A line from (x0, y0) towards (x1, y1), the last point not drawn, `w`
/// pixels wide.
pub fn line(c: &Canvas, x0: i32, y0: i32, x1: i32, y1: i32, w: i32, p: u32, rop2: u32) {
    let (dx, dy) = ((x1 - x0).abs(), -(y1 - y0).abs());
    let (sx, sy) = (if x0 < x1 { 1 } else { -1 }, if y0 < y1 { 1 } else { -1 });
    let (mut x, mut y, mut err) = (x0, y0, dx + dy);
    let half = (w - 1) / 2;
    while (x, y) != (x1, y1) {
        if w <= 1 {
            c.put(x, y, p, rop2);
        } else {
            c.fill(x - half, y - half, x - half + w, y - half + w, p, rop2);
        }
        let e2 = 2 * err;
        if e2 >= dy {
            err += dy;
            x += sx;
        }
        if e2 <= dx {
            err += dx;
            y += sy;
        }
    }
}

fn isqrt(v: u64) -> u64 {
    if v < 2 {
        return v;
    }
    let mut x = v;
    let mut y = (x + 1) / 2;
    while y < x {
        x = y;
        y = (x + v / x) / 2;
    }
    x
}

/// The pixels of row `y` inside the ellipse that fits [l, r) x [t, b), as
/// [from, to). None if the row misses it.
fn ellipse_span(l: i32, t: i32, r: i32, b: i32, y: i32) -> Option<(i32, i32)> {
    if r <= l || b <= t || y < t || y >= b {
        return None;
    }
    // Twice the centre and the axes, measured through pixel centres.
    let (w, h) = ((r - l) as i64, (b - t) as i64);
    let dy2 = (2 * y as i64 + 1) - (t as i64 + b as i64);
    let inside = h * h - dy2 * dy2;
    if inside < 0 {
        return None;
    }
    // Half-width, doubled: w * sqrt(1 - (dy2/h)^2).
    let hw2 = isqrt((w * w * inside) as u64) as i64 / h;
    let cx2 = l as i64 + r as i64;
    // Pixel x is inside when |2x + 1 - cx2| <= hw2.
    let from = (cx2 - hw2).div_euclid(2) as i32;
    let to = ((cx2 + hw2 - 1).div_euclid(2) + 1) as i32;
    (from < to).then_some((from, to))
}

/// An ellipse in [l, r) x [t, b): a `w`-pixel outline in `pen` (if any) and
/// the inside in `brush` (if any).
pub fn ellipse(c: &Canvas, l: i32, t: i32, r: i32, b: i32, pen: Option<(u32, i32)>, brush: Option<u32>, rop2: u32) {
    let (l, r) = (l.min(r), l.max(r));
    let (t, b) = (t.min(b), t.max(b));
    let w = pen.map_or(0, |(_, w)| w.max(1));
    // Without a pen, GDI's shape is a pixel smaller.
    let (r, b) = if pen.is_none() { (r - 1, b - 1) } else { (r, b) };
    for y in t..b {
        let Some((ol, or)) = ellipse_span(l, t, r, b, y) else { continue };
        match ellipse_span(l + w, t + w, r - w, b - w, y) {
            Some((il, ir)) => {
                if let Some((pc, _)) = pen {
                    c.hline(ol, il, y, pc, rop2);
                    c.hline(ir, or, y, pc, rop2);
                }
                if let Some(bc) = brush {
                    c.hline(il, ir, y, bc, rop2);
                }
            }
            None => match pen {
                Some((pc, _)) => c.hline(ol, or, y, pc, rop2),
                None => {
                    if let Some(bc) = brush {
                        c.hline(ol, or, y, bc, rop2);
                    }
                }
            },
        }
    }
}

/// A rectangle [l, r) x [t, b): outline and inside as for `ellipse`.
pub fn rectangle(c: &Canvas, l: i32, t: i32, r: i32, b: i32, pen: Option<(u32, i32)>, brush: Option<u32>, rop2: u32) {
    let (l, r) = (l.min(r), l.max(r));
    let (t, b) = (t.min(b), t.max(b));
    match pen {
        Some((pc, w)) => {
            let w = w.max(1);
            c.fill(l, t, r, (t + w).min(b), pc, rop2);
            c.fill(l, (b - w).max(t + w), r, b, pc, rop2);
            c.fill(l, t + w, (l + w).min(r), b - w, pc, rop2);
            c.fill((r - w).max(l + w), t + w, r, b - w, pc, rop2);
            if let Some(bc) = brush {
                c.fill(l + w, t + w, r - w, b - w, bc, rop2);
            }
        }
        None => {
            if let Some(bc) = brush {
                c.fill(l, t, r - 1, b - 1, bc, rop2);
            }
        }
    }
}

/// Fill a polygon (device points), even-odd or (`winding`) nonzero, at
/// pixel centres.
pub fn fill_polygon(c: &Canvas, pts: &[(i32, i32)], winding: bool, p: u32, rop2: u32) {
    if pts.len() < 3 {
        return;
    }
    let ymin = pts.iter().map(|q| q.1).min().unwrap().max(c.y0);
    let ymax = pts.iter().map(|q| q.1).max().unwrap().min(c.y1);
    let mut xs: Vec<(i32, i32)> = Vec::new();
    for y in ymin..ymax {
        xs.clear();
        // Scanline through the pixel centres, in 1/2 pixel units.
        let sy = 2 * y as i64 + 1;
        for i in 0..pts.len() {
            let (a, b) = (pts[i], pts[(i + 1) % pts.len()]);
            let (ay, by) = (2 * a.1 as i64, 2 * b.1 as i64);
            if ay == by || sy < ay.min(by) || sy >= ay.max(by) {
                continue;
            }
            // x at the scanline, doubled.
            let x2 = 2 * a.0 as i64 + (sy - ay) * 2 * (b.0 - a.0) as i64 / (by - ay);
            xs.push((x2 as i32, if by > ay { 1 } else { -1 }));
        }
        xs.sort_unstable_by_key(|e| e.0);
        let mut count = 0;
        for i in 0..xs.len() {
            let before = count;
            count += if winding { xs[i].1 } else { 1 };
            let inside = if winding { before != 0 } else { before % 2 == 1 };
            if inside && i > 0 {
                // Pixels whose centre lies between the two crossings.
                let from = xs[i - 1].0.div_euclid(2);
                let to = xs[i].0.div_euclid(2);
                c.hline(from, to, y, p, rop2);
            }
        }
    }
}
