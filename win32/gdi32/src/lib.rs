//! VEILOS `gdi32.dll`: device contexts and drawing (WIN32_DESIGN.md, G1).
//!
//! Everything is drawn in user mode into 32bpp pixels: a window's client
//! area (user32 owns those pixels, presents them to the kernel, and makes
//! its DCs with the private `VeilGdiCreateDC`), or a memory bitmap selected
//! into a memory DC. GDI objects (pens, brushes, fonts, bitmaps, DCs) live
//! in one table; a handle is the object's type (the OBJ_* value) and slot.
//!
//!   lib.rs      objects, DCs and their state, the exports
//!   draw.rs     pixels: lines, rectangles, ellipses, polygons, raster ops
//!   dib.rs      DIBs and DIB sections, palettes, alpha and transparent blits
//!   text.rs     text: TrueType fonts (ttf.rs) and the built-in 8x16 font

#![no_std]

extern crate alloc;

mod dib;
#[path = "../../dllrt.rs"]
mod dllrt;
mod draw;
#[path = "../../../src/font.rs"]
#[allow(dead_code)]
mod font;
mod font_data;
mod render2d;
mod text;
mod ttf;

use alloc::boxed::Box;
use alloc::vec::Vec;
use dllrt::Lock;
use draw::{colorref, pixel, Canvas};

pub(crate) type Handle = u64;
type Bool = i32;

const OBJ_PEN: u32 = 1;
const OBJ_BRUSH: u32 = 2;
const OBJ_DC: u32 = 3;
const OBJ_PAL: u32 = 5;
const OBJ_FONT: u32 = 6;
const OBJ_BITMAP: u32 = 7;
const OBJ_MEMDC: u32 = 10;

const CLR_INVALID: u32 = 0xFFFF_FFFF;
const HGDI_ERROR: Handle = 0xFFFF_FFFF;

#[link(name = "ntdll", kind = "raw-dylib")]
unsafe extern "system" {
    fn VeilUserQuery(what: u32, out: *mut [i32; 2]) -> i32;
}

// -----| objects |-----

#[derive(Clone, Copy)]
pub(crate) struct Pen {
    style: u32,
    width: i32,
    color: u32,
}

#[derive(Clone, Copy)]
pub(crate) struct Brush {
    style: u32,
    color: u32,
    hatch: u32,
}

#[derive(Clone)]
pub(crate) struct Font {
    /// LOGFONTW, as given.
    pub log: [u8; 92],
    /// The TrueType face and size it draws with (None inside: the bitmap
    /// font), once first used.
    pub tt: Option<Option<ttf::Realized>>,
}

pub(crate) struct Bitmap {
    pub w: i32,
    pub h: i32,
    pub bpp: u16,
    /// Top-down 0x00RRGGBB (unused for a 32bpp DIB section, drawn in place).
    pub bits: Vec<u32>,
    /// The memory DC it's selected into.
    pub dc: Handle,
    pub dib: Option<Box<dib::Section>>,
}

impl Bitmap {
    /// The pixels GDI draws on (read from a DIB section's bits if they're
    /// in another format): first pixel of the top row, and the stride.
    pub fn pixels(&mut self) -> (*mut u32, isize) {
        match self.dib.as_ref() {
            Some(sec) if sec.fmt.direct() => {
                let f = &sec.fmt;
                let row = f.row(sec.ptr(), 0) as *mut u32;
                let stride = (f.stride / 4) as isize;
                (row, if f.bottom_up { -stride } else { stride })
            }
            Some(sec) => {
                sec.read_into(&mut self.bits);
                (self.bits.as_mut_ptr(), self.w as isize)
            }
            None => (self.bits.as_mut_ptr(), self.w as isize),
        }
    }

    /// GDI drew on the pixels: write them back to a DIB section's bits.
    pub fn written(&mut self) {
        if let Some(sec) = self.dib.as_mut() {
            if !sec.fmt.direct() {
                sec.write_from(&self.bits);
            }
        }
    }
}

#[derive(Clone)]
pub(crate) struct DcState {
    pen: Handle,
    brush: Handle,
    font: Handle,
    text_color: u32,
    bk_color: u32,
    bk_mode: u32,
    text_align: u32,
    rop2: u32,
    fill_mode: u32,
    pos: (i32, i32),
    viewport: (i32, i32),
    window: (i32, i32),
    dc_brush: u32,
    dc_pen: u32,
    /// IntersectClipRect, in device coordinates.
    clip: Option<(i32, i32, i32, i32)>,
}

pub(crate) enum Target {
    /// A window's pixels (user32's), or nothing (the screen DC).
    Surface { ptr: usize, w: i32, h: i32, stride: usize },
    /// The memory DC's selected bitmap.
    Bitmap(Handle),
}

pub(crate) struct Dc {
    target: Target,
    memory: bool,
    /// Where the DC's (0, 0) is in the target, and the part it may draw on.
    origin: (i32, i32),
    bounds: (i32, i32, i32, i32),
    s: DcState,
    saved: Vec<DcState>,
    palette: Handle,
}

pub(crate) enum Obj {
    Pen(Pen),
    Brush(Brush),
    Font(Font),
    Bitmap(Bitmap),
    /// Colors as pixels.
    Palette(Vec<u32>),
    Dc(Box<Dc>),
}

impl Obj {
    fn kind(&self) -> u32 {
        match self {
            Obj::Pen(_) => OBJ_PEN,
            Obj::Brush(_) => OBJ_BRUSH,
            Obj::Font(_) => OBJ_FONT,
            Obj::Bitmap(_) => OBJ_BITMAP,
            Obj::Palette(_) => OBJ_PAL,
            Obj::Dc(_) => OBJ_DC,
        }
    }
}

pub(crate) struct Table {
    slots: Vec<Option<Obj>>,
}

static TABLE: Lock<Table> = Lock::new(Table { slots: Vec::new() });

// Stock objects take the slots of their GetStockObject numbers.
const WHITE_BRUSH: u32 = 0;
const BLACK_PEN: u32 = 7;
const SYSTEM_FONT: u32 = 13;
const DEFAULT_PALETTE: u32 = 15;
const DC_BRUSH: u32 = 18;
const DC_PEN: u32 = 19;
/// The 1x1 monochrome bitmap a new memory DC starts with.
const STOCK_BITMAP: u32 = 21;
const STOCK_COUNT: u32 = 22;

fn handle(kind: u32, slot: u32) -> Handle {
    (0x0100_0000 | kind << 20 | slot) as Handle
}

fn slot_of(h: Handle) -> Option<(u32, usize)> {
    let h = h as u32;
    (h & 0xFF00_0000 == 0x0100_0000).then(|| ((h >> 20) & 0xF, (h & 0xF_FFFF) as usize))
}

fn stock(n: u32) -> Option<Obj> {
    let pen = |color| Some(Obj::Pen(Pen { style: 0, width: 1, color }));
    let brush = |style, color| Some(Obj::Brush(Brush { style, color, hatch: 0 }));
    let font = |pitch: u8, height: i32, face: &str| {
        let mut log = [0u8; 92];
        log[0..4].copy_from_slice(&height.to_le_bytes());
        log[16..20].copy_from_slice(&400i32.to_le_bytes());
        log[27] = pitch;
        for (i, c) in face.encode_utf16().enumerate() {
            log[28 + 2 * i..30 + 2 * i].copy_from_slice(&c.to_le_bytes());
        }
        Some(Obj::Font(Font { log, tt: None }))
    };
    match n {
        0 => brush(0, 0xFF_FFFF),
        1 => brush(0, 0xC0_C0C0),
        2 => brush(0, 0x80_8080),
        3 => brush(0, 0x40_4040),
        4 => brush(0, 0),
        5 => brush(1, 0),
        6 => pen(0xFF_FFFF),
        7 => pen(0),
        8 => Some(Obj::Pen(Pen { style: 5, width: 1, color: 0 })),
        10 | 11 | 16 => font(0x31, 16, "System"),
        13 | 14 => font(0x22, 16, "System"),
        // ANSI_VAR_FONT and DEFAULT_GUI_FONT: Windows' "MS Shell Dlg", 8 points.
        12 | 17 => font(0x22, -11, "MS Shell Dlg"),
        15 => Some(Obj::Palette(dib::stock_palette())),
        18 => brush(0, 0xFF_FFFF),
        19 => pen(0),
        21 => Some(Obj::Bitmap(Bitmap { w: 1, h: 1, bpp: 1, bits: alloc::vec![0], dc: 0, dib: None })),
        _ => None,
    }
}

fn stock_kind(n: u32) -> u32 {
    match n {
        0..=5 | 18 => OBJ_BRUSH,
        6..=8 | 19 => OBJ_PEN,
        10..=14 | 16 | 17 => OBJ_FONT,
        15 => OBJ_PAL,
        21 => OBJ_BITMAP,
        _ => 0,
    }
}

impl Table {
    fn init(&mut self) {
        if self.slots.is_empty() {
            for n in 0..STOCK_COUNT {
                self.slots.push(stock(n));
            }
        }
    }

    pub(crate) fn add(&mut self, obj: Obj) -> Handle {
        self.init();
        let kind = obj.kind();
        let slot = match self.slots.iter().skip(STOCK_COUNT as usize).position(|s| s.is_none()) {
            Some(i) => i + STOCK_COUNT as usize,
            None => {
                self.slots.push(None);
                self.slots.len() - 1
            }
        };
        self.slots[slot] = Some(obj);
        handle(kind, slot as u32)
    }

    /// The object behind `h`, if `h` is a live handle of its type (memory
    /// DCs are handed out as OBJ_DC handles too).
    pub(crate) fn get(&mut self, h: Handle) -> Option<&mut Obj> {
        self.init();
        let (kind, slot) = slot_of(h)?;
        let obj = self.slots.get_mut(slot)?.as_mut()?;
        (obj.kind() == kind).then_some(obj)
    }

    pub(crate) fn dc(&mut self, h: Handle) -> Option<&mut Dc> {
        match self.get(h)? {
            Obj::Dc(dc) => Some(dc),
            _ => None,
        }
    }

    fn pen(&mut self, h: Handle) -> Pen {
        match self.get(h) {
            Some(Obj::Pen(p)) => *p,
            _ => Pen { style: 0, width: 1, color: 0 },
        }
    }

    fn brush(&mut self, h: Handle) -> Brush {
        match self.get(h) {
            Some(Obj::Brush(b)) => *b,
            _ => Brush { style: 0, color: 0xFF_FFFF, hatch: 0 },
        }
    }

    /// The DC's pen as (pixel, width), None for a null pen.
    fn dc_pen(&mut self, hdc: Handle) -> Option<(u32, i32)> {
        let s = self.dc(hdc)?.s.clone();
        let p = if s.pen == handle(OBJ_PEN, DC_PEN) { Pen { style: 0, width: 1, color: s.dc_pen } } else { self.pen(s.pen) };
        (p.style != 5).then_some((pixel(p.color), p.width.max(1)))
    }

    /// The DC's brush as a pixel, None for a null brush.
    pub(crate) fn dc_brush(&mut self, hdc: Handle) -> Option<u32> {
        let s = self.dc(hdc)?.s.clone();
        let b = if s.brush == handle(OBJ_BRUSH, DC_BRUSH) { Brush { style: 0, color: s.dc_brush, hatch: 0 } } else { self.brush(s.brush) };
        (b.style != 1).then_some(pixel(b.color))
    }

    /// The pixels `hdc` draws on, clipped. After drawing on them, call
    /// `done`.
    pub(crate) fn canvas(&mut self, hdc: Handle) -> Option<Canvas> {
        let dc = self.dc(hdc)?;
        let (bx0, by0, bx1, by1) = dc.bounds;
        let clip = dc.s.clip;
        let (ptr, w, h, stride) = match dc.target {
            Target::Surface { ptr, w, h, stride } => (ptr as *mut u32, w, h, stride as isize),
            Target::Bitmap(b) => match self.get(b) {
                Some(Obj::Bitmap(bm)) => {
                    let (p, s) = bm.pixels();
                    (p, bm.w, bm.h, s)
                }
                _ => return None,
            },
        };
        if ptr.is_null() {
            return None;
        }
        let mut c = Canvas { ptr, stride, x0: bx0.max(0), y0: by0.max(0), x1: bx1.min(w), y1: by1.min(h) };
        if let Some((x0, y0, x1, y1)) = clip {
            c.x0 = c.x0.max(x0);
            c.y0 = c.y0.max(y0);
            c.x1 = c.x1.min(x1);
            c.y1 = c.y1.min(y1);
        }
        Some(c)
    }
}

impl Table {
    /// The bitmap selected into memory DC `hdc`.
    pub(crate) fn selected_bitmap(&mut self, hdc: Handle) -> Option<Handle> {
        match self.dc(hdc)?.target {
            Target::Bitmap(b) => Some(b),
            _ => None,
        }
    }

    /// `hdc` was drawn on.
    pub(crate) fn done(&mut self, hdc: Handle) {
        if let Some(b) = self.selected_bitmap(hdc) {
            if let Some(Obj::Bitmap(bm)) = self.get(b) {
                bm.written();
            }
        }
    }

    pub(crate) fn is_dc(&mut self, hdc: Handle) -> bool {
        self.dc(hdc).is_some()
    }

    /// Select palette `pal` into `hdc`; the old one.
    pub(crate) fn dc_palette(&mut self, hdc: Handle, pal: Handle) -> Handle {
        match self.dc(hdc) {
            Some(dc) => core::mem::replace(&mut dc.palette, pal),
            None => 0,
        }
    }
}

impl Dc {
    /// Logical to device coordinates.
    fn dev(&self, x: i32, y: i32) -> (i32, i32) {
        (
            x - self.s.window.0 + self.s.viewport.0 + self.origin.0,
            y - self.s.window.1 + self.s.viewport.1 + self.origin.1,
        )
    }
}

fn screen_size() -> (i32, i32) {
    let mut v = [1024, 768];
    unsafe { VeilUserQuery(0, &mut v) };
    (v[0], v[1])
}

fn new_state() -> DcState {
    DcState {
        pen: handle(OBJ_PEN, BLACK_PEN),
        brush: handle(OBJ_BRUSH, WHITE_BRUSH),
        font: handle(OBJ_FONT, SYSTEM_FONT),
        text_color: 0,
        bk_color: 0xFF_FFFF,
        bk_mode: 2,
        text_align: 0,
        rop2: draw::R2_COPYPEN,
        fill_mode: 1,
        pos: (0, 0),
        viewport: (0, 0),
        window: (0, 0),
        dc_brush: 0xFF_FFFF,
        dc_pen: 0,
        clip: None,
    }
}

// -----| for user32 |-----

/// A DC on a window's pixels (`pixels` null: the screen, which draws
/// nothing): (0, 0) at `origin` in them, drawing only inside `clip`
/// ([left, top, right, bottom] in the pixels).
#[no_mangle]
pub extern "system" fn VeilGdiCreateDC(pixels: *mut u32, w: i32, h: i32, stride: usize, ox: i32, oy: i32, clip: *const [i32; 4]) -> Handle {
    let bounds = if clip.is_null() { (0, 0, w, h) } else {
        let c = unsafe { *clip };
        (c[0], c[1], c[2], c[3])
    };
    let dc = Dc {
        target: Target::Surface { ptr: pixels as usize, w, h, stride },
        memory: false,
        origin: (ox, oy),
        bounds,
        s: new_state(),
        saved: Vec::new(),
        palette: handle(OBJ_PAL, DEFAULT_PALETTE),
    };
    TABLE.lock().add(Obj::Dc(Box::new(dc)))
}

// -----| objects |-----

#[no_mangle]
pub extern "system" fn GetStockObject(n: i32) -> Handle {
    let n = n as u32;
    match stock_kind(n) {
        0 => 0,
        kind => {
            TABLE.lock().init();
            handle(kind, n)
        }
    }
}

#[no_mangle]
pub extern "system" fn CreatePen(style: i32, width: i32, color: u32) -> Handle {
    TABLE.lock().add(Obj::Pen(Pen { style: style as u32 & 0xF, width: width.max(1), color }))
}

#[no_mangle]
pub unsafe extern "system" fn CreatePenIndirect(lp: *const u8) -> Handle {
    let style = unsafe { *(lp as *const u32) };
    let width = unsafe { *(lp.add(4) as *const i32) };
    let color = unsafe { *(lp.add(12) as *const u32) };
    CreatePen(style as i32, width, color)
}

#[no_mangle]
pub unsafe extern "system" fn ExtCreatePen(style: u32, width: u32, brush: *const u8, _n: u32, _custom: *const u32) -> Handle {
    let color = if brush.is_null() { 0 } else { unsafe { *(brush.add(4) as *const u32) } };
    CreatePen((style & 0xF) as i32, width as i32, color)
}

#[no_mangle]
pub extern "system" fn CreateSolidBrush(color: u32) -> Handle {
    TABLE.lock().add(Obj::Brush(Brush { style: 0, color, hatch: 0 }))
}

/// Hatched brushes draw solid for now.
#[no_mangle]
pub extern "system" fn CreateHatchBrush(hatch: i32, color: u32) -> Handle {
    TABLE.lock().add(Obj::Brush(Brush { style: 2, color, hatch: hatch as u32 }))
}

#[no_mangle]
pub unsafe extern "system" fn CreateBrushIndirect(lb: *const u8) -> Handle {
    let style = unsafe { *(lb as *const u32) };
    let color = unsafe { *(lb.add(4) as *const u32) };
    let hatch = unsafe { *(lb.add(8) as *const u64) } as u32;
    TABLE.lock().add(Obj::Brush(Brush { style: if style == 1 { 1 } else { style.min(2) }, color, hatch }))
}

#[no_mangle]
pub extern "system" fn CreateBitmap(w: i32, h: i32, _planes: u32, bpp: u32, bits: *const u8) -> Handle {
    if w <= 0 || h <= 0 || w > 16384 || h > 16384 {
        return 0;
    }
    let mut bm = Bitmap { w, h, bpp: bpp.max(1) as u16, bits: alloc::vec![0; (w * h) as usize], dc: 0, dib: None };
    if !bits.is_null() {
        // Rows are word-aligned, top-down.
        let row = ((w as usize * bpp as usize + 15) / 16) * 2;
        for y in 0..h as usize {
            for x in 0..w as usize {
                let p = match bpp {
                    32 => (unsafe { *(bits.add(y * row + x * 4) as *const u32) }) & 0xFF_FFFF,
                    24 => unsafe {
                        let q = bits.add(y * row + x * 3);
                        (*q as u32) | (*q.add(1) as u32) << 8 | (*q.add(2) as u32) << 16
                    },
                    1 => {
                        if unsafe { *bits.add(y * row + x / 8) } & (0x80 >> (x % 8)) != 0 { 0xFF_FFFF } else { 0 }
                    }
                    _ => 0,
                };
                bm.bits[y * w as usize + x] = p;
            }
        }
    }
    TABLE.lock().add(Obj::Bitmap(bm))
}

#[no_mangle]
pub extern "system" fn CreateCompatibleBitmap(hdc: Handle, w: i32, h: i32) -> Handle {
    // Compatible with a memory DC that still has its stock bitmap means
    // monochrome, as on Windows.
    let mono = matches!(TABLE.lock().dc(hdc), Some(Dc { memory: true, target: Target::Bitmap(b), .. }) if *b == handle(OBJ_BITMAP, STOCK_BITMAP));
    let (w, h) = (w.max(1), h.max(1));
    CreateBitmap(w, h, 1, if mono { 1 } else { 32 }, core::ptr::null())
}

#[no_mangle]
pub extern "system" fn CreateCompatibleDC(_hdc: Handle) -> Handle {
    let stock = handle(OBJ_BITMAP, STOCK_BITMAP);
    let dc = Dc {
        target: Target::Bitmap(stock),
        memory: true,
        origin: (0, 0),
        bounds: (0, 0, i32::MAX, i32::MAX),
        s: new_state(),
        saved: Vec::new(),
        palette: handle(OBJ_PAL, DEFAULT_PALETTE),
    };
    TABLE.lock().add(Obj::Dc(Box::new(dc)))
}

#[no_mangle]
pub extern "system" fn DeleteDC(hdc: Handle) -> Bool {
    let mut t = TABLE.lock();
    let bitmap = match t.dc(hdc) {
        Some(dc) => match dc.target {
            Target::Bitmap(b) => b,
            _ => 0,
        },
        None => return 0,
    };
    if let Some(Obj::Bitmap(bm)) = t.get(bitmap) {
        bm.dc = 0;
    }
    if let Some((_, slot)) = slot_of(hdc) {
        t.slots[slot] = None;
    }
    1
}

#[no_mangle]
pub extern "system" fn DeleteObject(h: Handle) -> Bool {
    let mut t = TABLE.lock();
    let Some((_, slot)) = slot_of(h) else { return 0 };
    match t.get(h) {
        None => return 0,
        Some(Obj::Dc(_)) => return 0,
        // A bitmap selected into a DC stays.
        Some(Obj::Bitmap(bm)) if bm.dc != 0 => return 0,
        _ => {}
    }
    // Stock objects can't be deleted, but saying so is success.
    if slot >= STOCK_COUNT as usize {
        t.slots[slot] = None;
    }
    1
}

#[no_mangle]
pub extern "system" fn SelectObject(hdc: Handle, h: Handle) -> Handle {
    let mut t = TABLE.lock();
    let Some(kind) = t.get(h).map(|o| o.kind()) else { return 0 };
    let Some(dc) = t.dc(hdc) else { return 0 };
    match kind {
        OBJ_PEN => core::mem::replace(&mut dc.s.pen, h),
        OBJ_BRUSH => core::mem::replace(&mut dc.s.brush, h),
        OBJ_FONT => core::mem::replace(&mut dc.s.font, h),
        OBJ_BITMAP => {
            if !dc.memory {
                return 0;
            }
            let old = match dc.target {
                Target::Bitmap(b) => b,
                _ => return 0,
            };
            if old == h {
                return h;
            }
            match t.get(h) {
                Some(Obj::Bitmap(bm)) if bm.dc == 0 || h == handle(OBJ_BITMAP, STOCK_BITMAP) => bm.dc = hdc,
                _ => return HGDI_ERROR,
            }
            if let Some(Obj::Bitmap(bm)) = t.get(old) {
                bm.dc = 0;
            }
            if let Some(dc) = t.dc(hdc) {
                dc.target = Target::Bitmap(h);
            }
            old
        }
        _ => 0,
    }
}

#[no_mangle]
pub extern "system" fn GetCurrentObject(hdc: Handle, kind: u32) -> Handle {
    let mut t = TABLE.lock();
    let Some(dc) = t.dc(hdc) else { return 0 };
    match kind {
        OBJ_PEN => dc.s.pen,
        OBJ_BRUSH => dc.s.brush,
        OBJ_FONT => dc.s.font,
        OBJ_PAL => dc.palette,
        OBJ_BITMAP => match dc.target {
            Target::Bitmap(b) => b,
            _ => handle(OBJ_BITMAP, STOCK_BITMAP),
        },
        _ => 0,
    }
}

#[no_mangle]
pub extern "system" fn GetObjectType(h: Handle) -> u32 {
    let mut t = TABLE.lock();
    match t.get(h) {
        Some(Obj::Dc(dc)) => if dc.memory { OBJ_MEMDC } else { OBJ_DC },
        Some(o) => o.kind(),
        None => 0,
    }
}

fn get_object(h: Handle, size: i32, out: *mut u8, wide: bool) -> i32 {
    let mut t = TABLE.lock();
    let mut buf = [0u8; 92];
    let n = match t.get(h) {
        Some(Obj::Bitmap(bm)) if size >= 104 && bm.dib.is_some() => {
            // DIBSECTION: BITMAP, then BITMAPINFOHEADER, masks, section, offset.
            let sec = bm.dib.as_ref().unwrap();
            let f = &sec.fmt;
            let mut d = [0u8; 104];
            d[4..8].copy_from_slice(&f.w.to_le_bytes());
            d[8..12].copy_from_slice(&f.h.to_le_bytes());
            d[12..16].copy_from_slice(&(f.stride as u32).to_le_bytes());
            d[16..18].copy_from_slice(&1u16.to_le_bytes());
            d[18..20].copy_from_slice(&f.bpp.to_le_bytes());
            d[24..32].copy_from_slice(&(sec.ptr() as u64).to_le_bytes());
            d[32..36].copy_from_slice(&40u32.to_le_bytes());
            d[36..40].copy_from_slice(&f.w.to_le_bytes());
            d[40..44].copy_from_slice(&(if f.bottom_up { f.h } else { -f.h }).to_le_bytes());
            d[44..46].copy_from_slice(&1u16.to_le_bytes());
            d[46..48].copy_from_slice(&f.bpp.to_le_bytes());
            d[52..56].copy_from_slice(&(f.size() as u32).to_le_bytes());
            d[64..68].copy_from_slice(&(f.palette.len() as u32).to_le_bytes());
            for (i, m) in f.masks.iter().enumerate() {
                d[72 + i * 4..76 + i * 4].copy_from_slice(&m.to_le_bytes());
            }
            if !out.is_null() {
                unsafe { core::ptr::copy_nonoverlapping(d.as_ptr(), out, 104) };
            }
            return 104;
        }
        Some(Obj::Bitmap(bm)) => {
            buf[4..8].copy_from_slice(&bm.w.to_le_bytes());
            buf[8..12].copy_from_slice(&bm.h.to_le_bytes());
            let row = ((bm.w * bm.bpp as i32 + 15) / 16) * 2;
            buf[12..16].copy_from_slice(&row.to_le_bytes());
            buf[16..18].copy_from_slice(&1u16.to_le_bytes());
            buf[18..20].copy_from_slice(&bm.bpp.to_le_bytes());
            if let Some(sec) = bm.dib.as_ref() {
                buf[24..32].copy_from_slice(&(sec.ptr() as u64).to_le_bytes());
            }
            32
        }
        Some(Obj::Pen(p)) => {
            buf[0..4].copy_from_slice(&p.style.to_le_bytes());
            buf[4..8].copy_from_slice(&p.width.to_le_bytes());
            buf[12..16].copy_from_slice(&p.color.to_le_bytes());
            16
        }
        Some(Obj::Brush(b)) => {
            buf[0..4].copy_from_slice(&b.style.to_le_bytes());
            buf[4..8].copy_from_slice(&b.color.to_le_bytes());
            buf[8..16].copy_from_slice(&(b.hatch as u64).to_le_bytes());
            16
        }
        Some(Obj::Font(f)) => {
            buf = f.log;
            if wide {
                92
            } else {
                // LOGFONTA: the same 28 bytes, then the face name as bytes.
                let mut a = [0u8; 92];
                a[..28].copy_from_slice(&buf[..28]);
                for i in 0..32 {
                    a[28 + i] = buf[28 + 2 * i];
                }
                buf = a;
                60
            }
        }
        Some(Obj::Palette(colors)) => {
            buf[0..2].copy_from_slice(&(colors.len() as u16).to_le_bytes());
            2
        }
        _ => return 0,
    };
    if out.is_null() {
        return n;
    }
    let n = n.min(size.max(0));
    unsafe { core::ptr::copy_nonoverlapping(buf.as_ptr(), out, n as usize) };
    n
}

#[no_mangle]
pub extern "system" fn GetObjectW(h: Handle, size: i32, out: *mut u8) -> i32 {
    get_object(h, size, out, true)
}

#[no_mangle]
pub extern "system" fn GetObjectA(h: Handle, size: i32, out: *mut u8) -> i32 {
    get_object(h, size, out, false)
}

// -----| fonts: any request gets the built-in font, scaled |-----

#[no_mangle]
pub unsafe extern "system" fn CreateFontIndirectW(lf: *const u8) -> Handle {
    if lf.is_null() {
        return 0;
    }
    let mut log = [0u8; 92];
    unsafe { core::ptr::copy_nonoverlapping(lf, log.as_mut_ptr(), 92) };
    TABLE.lock().add(Obj::Font(Font { log, tt: None }))
}

#[no_mangle]
pub unsafe extern "system" fn CreateFontIndirectA(lf: *const u8) -> Handle {
    if lf.is_null() {
        return 0;
    }
    let mut log = [0u8; 92];
    unsafe { core::ptr::copy_nonoverlapping(lf, log.as_mut_ptr(), 28) };
    for i in 0..32 {
        log[28 + 2 * i] = unsafe { *lf.add(28 + i) };
    }
    unsafe { CreateFontIndirectW(log.as_ptr()) }
}

fn font_from_args(height: i32, width: i32, escapement: i32, orientation: i32, weight: i32, flags: [u32; 8]) -> [u8; 92] {
    let mut log = [0u8; 92];
    for (i, v) in [height, width, escapement, orientation, weight].iter().enumerate() {
        log[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
    }
    for (i, f) in flags.iter().enumerate() {
        log[20 + i] = *f as u8;
    }
    log
}

#[no_mangle]
pub unsafe extern "system" fn CreateFontW(h: i32, w: i32, esc: i32, orient: i32, weight: i32, italic: u32, underline: u32, strike: u32,
                                          charset: u32, out_prec: u32, clip_prec: u32, quality: u32, pitch: u32, face: *const u16) -> Handle {
    let mut log = font_from_args(h, w, esc, orient, weight, [italic, underline, strike, charset, out_prec, clip_prec, quality, pitch]);
    if !face.is_null() {
        for i in 0..31 {
            let c = unsafe { *face.add(i) };
            if c == 0 {
                break;
            }
            log[28 + 2 * i..30 + 2 * i].copy_from_slice(&c.to_le_bytes());
        }
    }
    unsafe { CreateFontIndirectW(log.as_ptr()) }
}

#[no_mangle]
pub unsafe extern "system" fn CreateFontA(h: i32, w: i32, esc: i32, orient: i32, weight: i32, italic: u32, underline: u32, strike: u32,
                                          charset: u32, out_prec: u32, clip_prec: u32, quality: u32, pitch: u32, face: *const u8) -> Handle {
    let mut log = font_from_args(h, w, esc, orient, weight, [italic, underline, strike, charset, out_prec, clip_prec, quality, pitch]);
    if !face.is_null() {
        for i in 0..31 {
            let c = unsafe { *face.add(i) };
            if c == 0 {
                break;
            }
            log[28 + 2 * i] = c;
        }
    }
    unsafe { CreateFontIndirectW(log.as_ptr()) }
}

// -----| DC state |-----

macro_rules! dc_field {
    ($set:ident, $get:ident, $field:ident, $fail:expr) => {
        #[no_mangle]
        pub extern "system" fn $set(hdc: Handle, v: u32) -> u32 {
            match TABLE.lock().dc(hdc) {
                Some(dc) => core::mem::replace(&mut dc.s.$field, v),
                None => $fail,
            }
        }
        #[no_mangle]
        pub extern "system" fn $get(hdc: Handle) -> u32 {
            TABLE.lock().dc(hdc).map_or($fail, |dc| dc.s.$field)
        }
    };
}

dc_field!(SetTextColor, GetTextColor, text_color, CLR_INVALID);
dc_field!(SetBkColor, GetBkColor, bk_color, CLR_INVALID);
dc_field!(SetBkMode, GetBkMode, bk_mode, 0);
dc_field!(SetTextAlign, GetTextAlign, text_align, 0xFFFF_FFFF);
dc_field!(SetROP2, GetROP2, rop2, 0);
dc_field!(SetPolyFillMode, GetPolyFillMode, fill_mode, 0);
dc_field!(SetDCBrushColor, GetDCBrushColor, dc_brush, CLR_INVALID);
dc_field!(SetDCPenColor, GetDCPenColor, dc_pen, CLR_INVALID);

/// Only MM_TEXT.
#[no_mangle]
pub extern "system" fn SetMapMode(_hdc: Handle, _mode: i32) -> i32 {
    1
}

#[no_mangle]
pub extern "system" fn GetMapMode(_hdc: Handle) -> i32 {
    1
}

#[no_mangle]
pub extern "system" fn SetStretchBltMode(_hdc: Handle, _mode: i32) -> i32 {
    1
}

#[no_mangle]
pub extern "system" fn GetStretchBltMode(_hdc: Handle) -> i32 {
    1
}

#[no_mangle]
pub extern "system" fn SetGraphicsMode(_hdc: Handle, _mode: i32) -> i32 {
    1
}

#[no_mangle]
pub extern "system" fn GdiFlush() -> Bool {
    1
}

#[no_mangle]
pub extern "system" fn GdiSetBatchLimit(_n: u32) -> u32 {
    1
}

fn point_out(out: *mut i32, p: (i32, i32)) {
    if !out.is_null() {
        unsafe {
            *out = p.0;
            *out.add(1) = p.1;
        }
    }
}

macro_rules! dc_origin {
    ($set:ident, $get:ident, $offset:ident, $field:ident) => {
        #[no_mangle]
        pub extern "system" fn $set(hdc: Handle, x: i32, y: i32, old: *mut i32) -> Bool {
            match TABLE.lock().dc(hdc) {
                Some(dc) => {
                    point_out(old, core::mem::replace(&mut dc.s.$field, (x, y)));
                    1
                }
                None => 0,
            }
        }
        #[no_mangle]
        pub extern "system" fn $get(hdc: Handle, out: *mut i32) -> Bool {
            match TABLE.lock().dc(hdc) {
                Some(dc) => {
                    point_out(out, dc.s.$field);
                    1
                }
                None => 0,
            }
        }
        #[no_mangle]
        pub extern "system" fn $offset(hdc: Handle, x: i32, y: i32, old: *mut i32) -> Bool {
            match TABLE.lock().dc(hdc) {
                Some(dc) => {
                    let o = dc.s.$field;
                    point_out(old, o);
                    dc.s.$field = (o.0 + x, o.1 + y);
                    1
                }
                None => 0,
            }
        }
    };
}

dc_origin!(SetViewportOrgEx, GetViewportOrgEx, OffsetViewportOrgEx, viewport);
dc_origin!(SetWindowOrgEx, GetWindowOrgEx, OffsetWindowOrgEx, window);

#[no_mangle]
pub extern "system" fn GetCurrentPositionEx(hdc: Handle, out: *mut i32) -> Bool {
    match TABLE.lock().dc(hdc) {
        Some(dc) => {
            point_out(out, dc.s.pos);
            1
        }
        None => 0,
    }
}

#[no_mangle]
pub extern "system" fn SaveDC(hdc: Handle) -> i32 {
    match TABLE.lock().dc(hdc) {
        Some(dc) => {
            dc.saved.push(dc.s.clone());
            dc.saved.len() as i32
        }
        None => 0,
    }
}

#[no_mangle]
pub extern "system" fn RestoreDC(hdc: Handle, n: i32) -> Bool {
    match TABLE.lock().dc(hdc) {
        Some(dc) => {
            let len = dc.saved.len() as i32;
            let level = if n < 0 { len + n + 1 } else { n };
            if level < 1 || level > len {
                return 0;
            }
            dc.saved.truncate(level as usize);
            dc.s = dc.saved.pop().unwrap();
            1
        }
        None => 0,
    }
}

/// Clipping: the DC's own rectangle and one intersected rectangle (regions
/// come later).
#[no_mangle]
pub extern "system" fn IntersectClipRect(hdc: Handle, l: i32, t: i32, r: i32, b: i32) -> i32 {
    match TABLE.lock().dc(hdc) {
        Some(dc) => {
            let (x0, y0) = dc.dev(l, t);
            let (x1, y1) = dc.dev(r, b);
            let c = match dc.s.clip {
                Some(c) => (c.0.max(x0), c.1.max(y0), c.2.min(x1), c.3.min(y1)),
                None => (x0, y0, x1, y1),
            };
            dc.s.clip = Some(c);
            if c.0 >= c.2 || c.1 >= c.3 { 1 } else { 2 } // NULLREGION, SIMPLEREGION
        }
        None => 0,
    }
}

#[no_mangle]
pub extern "system" fn SelectClipRgn(hdc: Handle, rgn: Handle) -> i32 {
    match TABLE.lock().dc(hdc) {
        Some(dc) if rgn == 0 => {
            dc.s.clip = None;
            2
        }
        Some(_) => 2,
        None => 0,
    }
}

#[no_mangle]
pub extern "system" fn GetClipBox(hdc: Handle, out: *mut [i32; 4]) -> i32 {
    let mut t = TABLE.lock();
    let Some(c) = t.canvas(hdc) else {
        // The screen DC: all of it.
        let (w, h) = screen_size();
        if t.dc(hdc).is_none() {
            return 0;
        }
        unsafe { *out = [0, 0, w, h] };
        return 2;
    };
    let dc = t.dc(hdc).unwrap();
    let (ox, oy) = dc.dev(0, 0);
    unsafe { *out = [c.x0 - ox, c.y0 - oy, c.x1 - ox, c.y1 - oy] };
    if c.x0 >= c.x1 || c.y0 >= c.y1 { 1 } else { 2 }
}

#[no_mangle]
pub extern "system" fn GetDeviceCaps(_hdc: Handle, index: i32) -> i32 {
    let (w, h) = screen_size();
    match index {
        2 => 1,                 // TECHNOLOGY: DT_RASDISPLAY
        4 => w * 254 / 960,     // HORZSIZE (mm)
        6 => h * 254 / 960,     // VERTSIZE
        8 | 118 => w,           // HORZRES, DESKTOPHORZRES
        10 | 117 => h,          // VERTRES, DESKTOPVERTRES
        12 => 32,               // BITSPIXEL
        14 => 1,                // PLANES
        24 => -1,               // NUMCOLORS
        38 => 0x7E99,           // RASTERCAPS
        40 => 36,               // ASPECTX
        42 => 36,               // ASPECTY
        44 => 51,               // ASPECTXY
        88 | 90 => 96,          // LOGPIXELSX/Y
        104 => 0,               // SIZEPALETTE
        116 => 60,              // VREFRESH
        _ => 0,
    }
}

// -----| drawing |-----

/// Run `f` with the canvas, the DC and its pen/brush if the DC draws
/// anywhere. False if `hdc` isn't a DC.
fn with_dc(hdc: Handle, f: impl FnOnce(&mut Table, Canvas)) -> Bool {
    let mut t = TABLE.lock();
    if t.dc(hdc).is_none() {
        return 0;
    }
    if let Some(c) = t.canvas(hdc) {
        f(&mut t, c);
        t.done(hdc);
    }
    1
}

fn dev(t: &mut Table, hdc: Handle, x: i32, y: i32) -> (i32, i32) {
    t.dc(hdc).map_or((x, y), |dc| dc.dev(x, y))
}

fn rop2(t: &mut Table, hdc: Handle) -> u32 {
    t.dc(hdc).map_or(draw::R2_COPYPEN, |dc| dc.s.rop2)
}

#[no_mangle]
pub extern "system" fn MoveToEx(hdc: Handle, x: i32, y: i32, old: *mut i32) -> Bool {
    match TABLE.lock().dc(hdc) {
        Some(dc) => {
            point_out(old, core::mem::replace(&mut dc.s.pos, (x, y)));
            1
        }
        None => 0,
    }
}

#[no_mangle]
pub extern "system" fn LineTo(hdc: Handle, x: i32, y: i32) -> Bool {
    let mut t = TABLE.lock();
    let Some(from) = t.dc(hdc).map(|dc| dc.s.pos) else { return 0 };
    if let (Some(c), Some((p, w))) = (t.canvas(hdc), t.dc_pen(hdc)) {
        let (x0, y0) = dev(&mut t, hdc, from.0, from.1);
        let (x1, y1) = dev(&mut t, hdc, x, y);
        draw::line(&c, x0, y0, x1, y1, w, p, rop2(&mut t, hdc));
        t.done(hdc);
    }
    t.dc(hdc).unwrap().s.pos = (x, y);
    1
}

unsafe fn points(pts: *const i32, n: i32) -> Vec<(i32, i32)> {
    (0..n.max(0) as usize).map(|i| unsafe { (*pts.add(2 * i), *pts.add(2 * i + 1)) }).collect()
}

#[no_mangle]
pub unsafe extern "system" fn Polyline(hdc: Handle, pts: *const i32, n: i32) -> Bool {
    let pts = unsafe { points(pts, n) };
    with_dc(hdc, |t, c| {
        if let Some((p, w)) = t.dc_pen(hdc) {
            let r2 = rop2(t, hdc);
            for seg in pts.windows(2) {
                let (x0, y0) = dev(t, hdc, seg[0].0, seg[0].1);
                let (x1, y1) = dev(t, hdc, seg[1].0, seg[1].1);
                draw::line(&c, x0, y0, x1, y1, w, p, r2);
            }
        }
    })
}

#[no_mangle]
pub unsafe extern "system" fn PolylineTo(hdc: Handle, pts: *const i32, n: i32) -> Bool {
    let list = unsafe { points(pts, n) };
    let Some(start) = TABLE.lock().dc(hdc).map(|dc| dc.s.pos) else { return 0 };
    let mut all = alloc::vec![start];
    all.extend_from_slice(&list);
    let r = unsafe { Polyline(hdc, all.as_ptr() as *const i32, all.len() as i32) };
    if let (Some(dc), Some(last)) = (TABLE.lock().dc(hdc), list.last()) {
        dc.s.pos = *last;
    }
    r
}

#[no_mangle]
pub unsafe extern "system" fn Polygon(hdc: Handle, pts: *const i32, n: i32) -> Bool {
    let pts = unsafe { points(pts, n) };
    with_dc(hdc, |t, c| {
        let r2 = rop2(t, hdc);
        let dpts: Vec<(i32, i32)> = pts.iter().map(|p| dev(t, hdc, p.0, p.1)).collect();
        let winding = t.dc(hdc).is_some_and(|dc| dc.s.fill_mode == 2);
        if let Some(b) = t.dc_brush(hdc) {
            draw::fill_polygon(&c, &dpts, winding, b, r2);
        }
        if let Some((p, w)) = t.dc_pen(hdc) {
            for i in 0..dpts.len() {
                let (a, b) = (dpts[i], dpts[(i + 1) % dpts.len()]);
                draw::line(&c, a.0, a.1, b.0, b.1, w, p, r2);
            }
        }
    })
}

#[no_mangle]
pub extern "system" fn Rectangle(hdc: Handle, l: i32, t_: i32, r: i32, b: i32) -> Bool {
    with_dc(hdc, |t, c| {
        let (x0, y0) = dev(t, hdc, l, t_);
        let (x1, y1) = dev(t, hdc, r, b);
        let (pen, brush, r2) = (t.dc_pen(hdc), t.dc_brush(hdc), rop2(t, hdc));
        draw::rectangle(&c, x0, y0, x1, y1, pen, brush, r2);
    })
}

#[no_mangle]
pub extern "system" fn Ellipse(hdc: Handle, l: i32, t_: i32, r: i32, b: i32) -> Bool {
    with_dc(hdc, |t, c| {
        let (x0, y0) = dev(t, hdc, l, t_);
        let (x1, y1) = dev(t, hdc, r, b);
        let (pen, brush, r2) = (t.dc_pen(hdc), t.dc_brush(hdc), rop2(t, hdc));
        draw::ellipse(&c, x0, y0, x1, y1, pen, brush, r2);
    })
}

/// Rounded corners come with G2; a rectangle until then.
#[no_mangle]
pub extern "system" fn RoundRect(hdc: Handle, l: i32, t: i32, r: i32, b: i32, _w: i32, _h: i32) -> Bool {
    Rectangle(hdc, l, t, r, b)
}

#[no_mangle]
pub extern "system" fn SetPixel(hdc: Handle, x: i32, y: i32, color: u32) -> u32 {
    let mut out = CLR_INVALID;
    with_dc(hdc, |t, c| {
        let (x, y) = dev(t, hdc, x, y);
        if c.contains(x, y) {
            c.put(x, y, pixel(color), draw::R2_COPYPEN);
            out = color & 0xFF_FFFF;
        }
    });
    out
}

#[no_mangle]
pub extern "system" fn SetPixelV(hdc: Handle, x: i32, y: i32, color: u32) -> Bool {
    (SetPixel(hdc, x, y, color) != CLR_INVALID) as Bool
}

#[no_mangle]
pub extern "system" fn GetPixel(hdc: Handle, x: i32, y: i32) -> u32 {
    let mut out = CLR_INVALID;
    with_dc(hdc, |t, c| {
        let (x, y) = dev(t, hdc, x, y);
        if let Some(p) = c.get(x, y) {
            out = colorref(p);
        }
    });
    out
}

#[no_mangle]
pub extern "system" fn PatBlt(hdc: Handle, x: i32, y: i32, w: i32, h: i32, rop: u32) -> Bool {
    with_dc(hdc, |t, c| {
        let (x0, y0) = dev(t, hdc, x, y);
        let (x0, x1) = if w < 0 { (x0 + w, x0) } else { (x0, x0 + w) };
        let (y0, y1) = if h < 0 { (y0 + h, y0) } else { (y0, y0 + h) };
        let pat = t.dc_brush(hdc).unwrap_or(0);
        let code = (rop >> 16) as u8;
        let (x0, x1, y0, y1) = (x0.max(c.x0), x1.min(c.x1), y0.max(c.y0), y1.min(c.y1));
        for yy in y0..y1 {
            for xx in x0..x1 {
                let at = c.at(xx, yy);
                unsafe { *at = draw::mix3(code, pat, 0, *at) };
            }
        }
    })
}

/// BitBlt and StretchBlt: copy (scaling, nearest pixel) a source rectangle
/// through a raster operation.
fn blt(dst: Handle, dx: i32, dy: i32, dw: i32, dh: i32, src: Handle, sx: i32, sy: i32, sw: i32, sh: i32, rop: u32) -> Bool {
    let mut t = TABLE.lock();
    if t.dc(dst).is_none() {
        return 0;
    }
    let code = (rop >> 16) as u8;
    // Does the operation read the source? (S and ~S differ in the code.)
    let uses_src = (code >> 2) & 0x33 != code & 0x33;
    let src_canvas = if uses_src {
        if t.dc(src).is_none() {
            return 0;
        }
        t.canvas(src)
    } else {
        None
    };
    let Some(c) = t.canvas(dst) else { return 1 };
    let (x0, y0) = dev(&mut t, dst, dx, dy);
    let (sx0, sy0) = if uses_src { dev(&mut t, src, sx, sy) } else { (0, 0) };
    let pat = t.dc_brush(dst).unwrap_or(0);
    if dw == 0 || dh == 0 {
        return 1;
    }
    let (sw, sh) = (if sw == 0 { 1 } else { sw }, if sh == 0 { 1 } else { sh });
    // Overlapping copies within one target go through a snapshot of the
    // source rectangle.
    let ((sxa, sxb), (sya, syb)) = (draw::span(sx0, sw), draw::span(sy0, sh));
    let snapshot: Option<Vec<u32>> = src_canvas.filter(|s| s.ptr == c.ptr).map(|s| {
        let mut v = Vec::with_capacity(((sxb - sxa) * (syb - sya)) as usize);
        for y in sya..syb {
            for x in sxa..sxb {
                v.push(s.get(x, y).unwrap_or(0));
            }
        }
        v
    });
    let ((xa, xb), (ya, yb)) = (draw::span(x0, dw), draw::span(y0, dh));
    for y in ya.max(c.y0)..yb.min(c.y1) {
        let ry = draw::source(sy0, sh, dh, draw::step(y0, dh, y));
        for x in xa.max(c.x0)..xb.min(c.x1) {
            let rx = draw::source(sx0, sw, dw, draw::step(x0, dw, x));
            let s = match (&snapshot, &src_canvas) {
                (Some(v), _) => {
                    if rx < sxa || rx >= sxb || ry < sya || ry >= syb {
                        continue;
                    }
                    v[((ry - sya) * (sxb - sxa) + rx - sxa) as usize]
                }
                (None, Some(s)) => match s.get(rx, ry) {
                    Some(p) => p,
                    // Outside the source: left alone.
                    None => continue,
                },
                _ => 0,
            };
            let at = c.at(x, y);
            unsafe { *at = draw::mix3(code, pat, s, *at) };
        }
    }
    t.done(dst);
    1
}

#[no_mangle]
pub extern "system" fn BitBlt(dst: Handle, x: i32, y: i32, w: i32, h: i32, src: Handle, sx: i32, sy: i32, rop: u32) -> Bool {
    blt(dst, x, y, w, h, src, sx, sy, w, h, rop)
}

#[no_mangle]
pub extern "system" fn StretchBlt(dst: Handle, x: i32, y: i32, w: i32, h: i32, src: Handle, sx: i32, sy: i32, sw: i32, sh: i32, rop: u32) -> Bool {
    blt(dst, x, y, w, h, src, sx, sy, sw, sh, rop)
}

#[no_mangle]
pub extern "system" fn DllMain(_module: usize, _reason: u32, _reserved: usize) -> i32 {
    1
}

pub(crate) fn table() -> dllrt::Guard<'static, Table> {
    TABLE.lock()
}

pub(crate) fn dc_text_state(t: &mut Table, hdc: Handle) -> Option<(u32, u32, u32, u32, (i32, i32))> {
    t.dc(hdc).map(|dc| (dc.s.text_color, dc.s.bk_color, dc.s.bk_mode, dc.s.text_align, dc.s.pos))
}

pub(crate) fn set_pos(t: &mut Table, hdc: Handle, pos: (i32, i32)) {
    if let Some(dc) = t.dc(hdc) {
        dc.s.pos = pos;
    }
}


pub(crate) fn to_device(t: &mut Table, hdc: Handle, x: i32, y: i32) -> (i32, i32) {
    dev(t, hdc, x, y)
}
