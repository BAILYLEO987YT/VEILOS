//! Selection and feedback (WIN32_DESIGN.md G5c): glRenderMode, the name
//! stack and its hit records, glFeedbackBuffer and its tokens. Primitives
//! are caught after clipping and culling, where they would be rasterized.

use alloc::vec::Vec;

use crate::*;

pub(crate) const GL_RENDER: u32 = 0x1C00;
const GL_FEEDBACK: u32 = 0x1C01;
const GL_SELECT: u32 = 0x1C02;

const POINT_TOKEN: f32 = 0x0701 as f32;
const LINE_TOKEN: f32 = 0x0702 as f32;
const POLYGON_TOKEN: f32 = 0x0703 as f32;
const BITMAP_TOKEN: f32 = 0x0704 as f32;
const DRAW_PIXEL_TOKEN: f32 = 0x0705 as f32;
const COPY_PIXEL_TOKEN: f32 = 0x0706 as f32;
const LINE_RESET_TOKEN: f32 = 0x0707 as f32;
const PASS_THROUGH_TOKEN: f32 = 0x0700 as f32;

#[derive(Clone)]
pub(crate) struct Select {
    pub mode: u32,
    pub select_ptr: usize,
    select_size: usize,
    select_used: usize,
    hits: i32,
    names: Vec<u32>,
    hit: bool,
    min_z: f64,
    max_z: f64,
    overflow: bool,
    pub feedback_ptr: usize,
    feedback_size: usize,
    feedback_type: u32,
    feedback_used: usize,
    /// The next line of a strip continues the last (GL_LINE_TOKEN), unless
    /// a primitive started (GL_LINE_RESET_TOKEN).
    pub line_reset: bool,
}

impl Select {
    pub fn new() -> Select {
        Select {
            mode: GL_RENDER,
            select_ptr: 0,
            select_size: 0,
            select_used: 0,
            hits: 0,
            names: Vec::new(),
            hit: false,
            min_z: 1.0,
            max_z: 0.0,
            overflow: false,
            feedback_ptr: 0,
            feedback_size: 0,
            feedback_type: 0,
            feedback_used: 0,
            line_reset: true,
        }
    }

    /// Write the pending hit record, if there is one.
    fn flush_hit(&mut self) {
        if !self.hit {
            return;
        }
        self.hit = false;
        let mut rec = Vec::with_capacity(3 + self.names.len());
        rec.push(self.names.len() as u32);
        rec.push((self.min_z * 4294967295.0) as u32);
        rec.push((self.max_z * 4294967295.0) as u32);
        rec.extend_from_slice(&self.names);
        for v in rec {
            if self.select_used >= self.select_size {
                self.overflow = true;
                return;
            }
            unsafe { *(self.select_ptr as *mut u32).add(self.select_used) = v };
            self.select_used += 1;
        }
        self.hits += 1;
        self.min_z = 1.0;
        self.max_z = 0.0;
    }

    fn feedback(&mut self, v: f32) {
        if self.feedback_used >= self.feedback_size {
            self.overflow = true;
            return;
        }
        unsafe { *(self.feedback_ptr as *mut f32).add(self.feedback_used) = v };
        self.feedback_used += 1;
    }
}

impl Ctx {
    /// In selection or feedback mode, take a clipped primitive (window
    /// coordinates `win`, y upwards) instead of drawing it: true if taken.
    pub(crate) fn capture(&mut self, kind: u32, verts: &[Vtx], win: &[[f64; 4]], back: bool) -> bool {
        match self.select.mode {
            GL_SELECT => {
                let s = &mut self.select;
                s.hit = true;
                for w in win {
                    s.min_z = s.min_z.min(w[2].clamp(0.0, 1.0));
                    s.max_z = s.max_z.max(w[2].clamp(0.0, 1.0));
                }
                true
            }
            GL_FEEDBACK => {
                match kind {
                    KIND_POINTS => self.select.feedback(POINT_TOKEN),
                    KIND_LINES => {
                        let reset = core::mem::replace(&mut self.select.line_reset, false);
                        self.select.feedback(if reset { LINE_RESET_TOKEN } else { LINE_TOKEN });
                    }
                    _ => {
                        self.select.feedback(POLYGON_TOKEN);
                        self.select.feedback(verts.len() as f32);
                    }
                }
                for (v, w) in verts.iter().zip(win) {
                    let col = if back { v.back } else { v.col };
                    self.feedback_vertex(*w, 1.0 / w[3] as f32, col, v.tc[0]);
                }
                true
            }
            _ => false,
        }
    }

    fn feedback_vertex(&mut self, w: [f64; 4], clip_w: f32, col: [f32; 4], tc: [f32; 4]) {
        let s = &mut self.select;
        let t = s.feedback_type;
        s.feedback(w[0] as f32);
        s.feedback(w[1] as f32);
        // GL_3D and up: depth; GL_4D_COLOR_TEXTURE: clip w.
        if t != 0x0600 {
            s.feedback(w[2] as f32);
        }
        if t == 0x0604 {
            s.feedback(clip_w);
        }
        if t >= 0x0602 {
            for c in col {
                s.feedback(clamp01(c));
            }
        }
        if t >= 0x0603 {
            for x in tc {
                s.feedback(x);
            }
        }
    }

    /// glBitmap (0), glDrawPixels (1), glCopyPixels (2) in selection or
    /// feedback mode: true if taken.
    pub(crate) fn capture_pixels(&mut self, which: u32) -> bool {
        match self.select.mode {
            GL_SELECT => {
                if self.raster.valid {
                    let z = self.raster.win[2].clamp(0.0, 1.0);
                    let s = &mut self.select;
                    s.hit = true;
                    s.min_z = s.min_z.min(z);
                    s.max_z = s.max_z.max(z);
                }
                true
            }
            GL_FEEDBACK => {
                if self.raster.valid {
                    self.select.feedback([BITMAP_TOKEN, DRAW_PIXEL_TOKEN, COPY_PIXEL_TOKEN][which as usize]);
                    let r = self.raster;
                    self.feedback_vertex([r.win[0], r.win[1], r.win[2], 1.0], 1.0, r.color, r.tc);
                }
                true
            }
            _ => false,
        }
    }
}

#[no_mangle]
pub extern "system" fn glRenderMode(mode: u32) -> i32 {
    outside(|c| {
        if !matches!(mode, GL_RENDER | GL_FEEDBACK | GL_SELECT) {
            c.err(GL_INVALID_ENUM);
            return 0;
        }
        if mode == GL_SELECT && c.select.select_ptr == 0 || mode == GL_FEEDBACK && c.select.feedback_ptr == 0 {
            c.err(GL_INVALID_OPERATION);
            return 0;
        }
        c.flush();
        let s = &mut c.select;
        // What leaving the old mode returns.
        let result = match s.mode {
            GL_SELECT => {
                s.flush_hit();
                if s.overflow { -1 } else { s.hits }
            }
            GL_FEEDBACK => if s.overflow { -1 } else { s.feedback_used as i32 },
            _ => 0,
        };
        s.mode = mode;
        s.overflow = false;
        s.hits = 0;
        s.select_used = 0;
        s.feedback_used = 0;
        s.hit = false;
        s.min_z = 1.0;
        s.max_z = 0.0;
        s.line_reset = true;
        if mode == GL_SELECT {
            s.names.clear();
        }
        result
    })
}

#[no_mangle]
pub extern "system" fn glSelectBuffer(size: i32, buffer: *mut u32) {
    outside(|c| {
        if size < 0 {
            return c.err(GL_INVALID_VALUE);
        }
        if c.select.mode == GL_SELECT {
            return c.err(GL_INVALID_OPERATION);
        }
        c.select.select_ptr = buffer as usize;
        c.select.select_size = size as usize;
    })
}

#[no_mangle]
pub extern "system" fn glFeedbackBuffer(size: i32, kind: u32, buffer: *mut f32) {
    outside(|c| {
        if size < 0 {
            return c.err(GL_INVALID_VALUE);
        }
        if !(0x0600..=0x0604).contains(&kind) {
            return c.err(GL_INVALID_ENUM);
        }
        if c.select.mode == GL_FEEDBACK {
            return c.err(GL_INVALID_OPERATION);
        }
        c.select.feedback_ptr = buffer as usize;
        c.select.feedback_size = size as usize;
        c.select.feedback_type = kind;
    })
}

#[no_mangle]
pub extern "system" fn glPassThrough(token: f32) {
    rec!(glPassThrough(token));
    outside(|c| {
        if c.select.mode == GL_FEEDBACK {
            c.select.feedback(PASS_THROUGH_TOKEN);
            c.select.feedback(token);
        }
    })
}

/// The name stack calls only act in selection mode; each change writes the
/// pending hit record first.
fn names(f: impl FnOnce(&mut Ctx)) {
    outside(|c| {
        if c.select.mode != GL_SELECT {
            return;
        }
        c.flush();
        c.select.flush_hit();
        f(c);
    })
}

#[no_mangle]
pub extern "system" fn glInitNames() {
    rec!(glInitNames());
    names(|c| c.select.names.clear())
}

#[no_mangle]
pub extern "system" fn glPushName(name: u32) {
    rec!(glPushName(name));
    names(|c| {
        if c.select.names.len() >= 64 {
            c.err(GL_STACK_OVERFLOW);
        } else {
            c.select.names.push(name);
        }
    })
}

#[no_mangle]
pub extern "system" fn glPopName() {
    rec!(glPopName());
    names(|c| {
        if c.select.names.pop().is_none() {
            c.err(GL_STACK_UNDERFLOW);
        }
    })
}

#[no_mangle]
pub extern "system" fn glLoadName(name: u32) {
    rec!(glLoadName(name));
    names(|c| match c.select.names.last_mut() {
        Some(n) => *n = name,
        None => c.err(GL_INVALID_OPERATION),
    })
}
