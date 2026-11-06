// =============================================================================
// VEILOS: console screen buffers (WIN32_DESIGN.md, S5)
// =============================================================================
//
// A screen buffer is a grid of cells, each a UTF-16 unit and a Windows
// attribute (colours in the low byte, COMMON_LVB_* above), with a cursor and
// a window: the part a terminal shows. Programs write to it with the console
// calls (cells, attributes, scrolling) and with text, which follows the
// buffer's output mode as Windows' console host does:
//
//   ENABLE_PROCESSED_OUTPUT       \r \n \b \t \a act; otherwise they're cells.
//   ENABLE_WRAP_AT_EOL_OUTPUT     text wraps at the right edge. Writing the
//                                 last column leaves the cursor there, waiting:
//                                 the next character wraps first.
//   ENABLE_VIRTUAL_TERMINAL_PROCESSING   escape sequences (VT100 and on) move
//                                 the cursor, erase, colour, scroll; their rows
//                                 count from the window's top.
//
// Colours from escape sequences become the 16 console colours: 256-colour and
// RGB ones the nearest of the classic palette, as conhost does.

use alloc::vec;
use alloc::vec::Vec;

pub const ENABLE_PROCESSED_OUTPUT: u32 = 0x1;
pub const ENABLE_WRAP_AT_EOL_OUTPUT: u32 = 0x2;
pub const ENABLE_VIRTUAL_TERMINAL_PROCESSING: u32 = 0x4;
pub const DISABLE_NEWLINE_AUTO_RETURN: u32 = 0x8;

const REVERSE_VIDEO: u16 = 0x4000;
const UNDERSCORE: u16 = 0x8000;
const FG_INTENSITY: u16 = 0x08;

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    pub ch: u16,
    pub attr: u16,
}

/// Where the escape-sequence reader is.
#[derive(Clone, PartialEq, Eq)]
enum Vt {
    Ground,
    Escape,
    /// ESC [ ... : parameters so far, any '?' or other private marker.
    Csi { params: Vec<u32>, current: Option<u32>, private: u16, intermediate: u16 },
    /// ESC ] ... until BEL or ESC \.
    Osc { text: Vec<u16>, escape: bool },
    /// ESC ( and the like: one more character, ignored.
    Charset,
}

pub struct ScreenBuffer {
    pub width: usize,
    pub height: usize,
    pub cells: Vec<Cell>,
    pub cursor: (usize, usize),
    /// The last column was just written: the next character wraps first.
    pub wrap_pending: bool,
    /// The current attribute, and the one SGR 0 goes back to.
    pub attr: u16,
    pub default_attr: u16,
    /// SGR 1 brightens the foreground, whichever colour comes after; the
    /// colour itself is kept apart so 22 can undo it.
    bold: bool,
    fg: u16,
    pub mode: u32,
    pub cursor_visible: bool,
    pub cursor_size: u32,
    /// The window: left, top, and its size.
    pub window: (usize, usize, usize, usize),
    vt: Vt,
    /// DECSTBM's margins (rows of the window), if set.
    margins: Option<(usize, usize)>,
    saved: (usize, usize, u16),
    /// The main screen's cells while the alternate one (?1049) is shown.
    alternate: Option<(Vec<Cell>, (usize, usize), u16)>,
    /// OSC 0/2: a new title for the console.
    pub title: Option<Vec<u16>>,
    /// A surrogate's first half, waiting for the second.
    high: Option<u16>,
}

fn blank(attr: u16) -> Cell {
    Cell { ch: b' ' as u16, attr }
}

impl ScreenBuffer {
    pub fn new(width: usize, height: usize, window_rows: usize) -> Self {
        let width = width.max(1);
        let height = height.max(1);
        ScreenBuffer {
            width,
            height,
            cells: vec![blank(0x07); width * height],
            cursor: (0, 0),
            wrap_pending: false,
            attr: 0x07,
            default_attr: 0x07,
            bold: false,
            fg: 0x07,
            mode: ENABLE_PROCESSED_OUTPUT | ENABLE_WRAP_AT_EOL_OUTPUT | ENABLE_VIRTUAL_TERMINAL_PROCESSING,
            cursor_visible: true,
            cursor_size: 25,
            window: (0, 0, width, window_rows.clamp(1, height)),
            vt: Vt::Ground,
            margins: None,
            saved: (0, 0, 0x07),
            alternate: None,
            title: None,
            high: None,
        }
    }

    pub fn cell(&self, x: usize, y: usize) -> Cell {
        self.cells[y * self.width + x]
    }

    fn set(&mut self, x: usize, y: usize, c: Cell) {
        if x < self.width && y < self.height {
            self.cells[y * self.width + x] = c;
        }
    }

    // -----| size and window |-----

    /// A new size: what fits is kept, the cursor and window stay inside.
    pub fn resize(&mut self, width: usize, height: usize) {
        let (width, height) = (width.max(1), height.max(1));
        let mut cells = vec![blank(self.default_attr); width * height];
        for y in 0..height.min(self.height) {
            for x in 0..width.min(self.width) {
                cells[y * width + x] = self.cell(x, y);
            }
        }
        self.cells = cells;
        self.width = width;
        self.height = height;
        self.cursor = (self.cursor.0.min(width - 1), self.cursor.1.min(height - 1));
        let (l, t, w, h) = self.window;
        let w = w.min(width);
        let h = h.min(height);
        self.window = (l.min(width - w), t.min(height - h), w, h);
        self.margins = None;
        self.wrap_pending = false;
    }

    /// Move the window so the cursor is in it (as the console host does
    /// after output).
    fn follow_cursor(&mut self) {
        let (l, t, w, h) = self.window;
        let (x, y) = self.cursor;
        let top = if y < t { y } else if y >= t + h { y + 1 - h } else { t };
        let left = if x < l { x } else if x >= l + w { x + 1 - w } else { l };
        self.window = (left, top, w, h);
    }

    // -----| scrolling |-----

    /// Rows `top..=bottom` up by `n` (new rows blank, in the current colours).
    fn scroll_up(&mut self, top: usize, bottom: usize, n: usize) {
        let n = n.min(bottom + 1 - top);
        for y in top..=bottom {
            for x in 0..self.width {
                let c = if y + n <= bottom { self.cell(x, y + n) } else { blank(self.attr) };
                self.set(x, y, c);
            }
        }
    }

    fn scroll_down(&mut self, top: usize, bottom: usize, n: usize) {
        let n = n.min(bottom + 1 - top);
        for y in (top..=bottom).rev() {
            for x in 0..self.width {
                let c = if y >= top + n { self.cell(x, y - n) } else { blank(self.attr) };
                self.set(x, y, c);
            }
        }
    }

    /// The rows a line feed scrolls when it reaches the bottom: the margins
    /// if they're set (and the cursor within them), else the whole buffer.
    fn scroll_region(&self) -> (usize, usize) {
        match self.margins {
            Some((t, b)) if self.cursor.1 >= self.window.1 + t && self.cursor.1 <= self.window.1 + b => (self.window.1 + t, self.window.1 + b),
            _ => (0, self.height - 1),
        }
    }

    fn line_feed(&mut self) {
        let (top, bottom) = self.scroll_region();
        if self.cursor.1 == bottom {
            self.scroll_up(top, bottom, 1);
        } else if self.cursor.1 + 1 < self.height {
            self.cursor.1 += 1;
        }
    }

    fn reverse_line_feed(&mut self) {
        let (top, bottom) = self.scroll_region();
        if self.cursor.1 == top {
            self.scroll_down(top, bottom, 1);
        } else if self.cursor.1 > 0 {
            self.cursor.1 -= 1;
        }
    }

    // -----| text |-----

    /// Text written to the buffer, as its mode says.
    pub fn write(&mut self, units: &[u16]) {
        for &u in units {
            self.unit(u);
        }
        self.follow_cursor();
    }

    fn unit(&mut self, u: u16) {
        let vt = self.mode & ENABLE_VIRTUAL_TERMINAL_PROCESSING != 0;
        if vt && self.vt != Vt::Ground {
            self.escape_unit(u);
            return;
        }
        if vt && u == 0x1B {
            self.vt = Vt::Escape;
            return;
        }
        if self.mode & ENABLE_PROCESSED_OUTPUT != 0 {
            match u {
                0x0D => {
                    self.cursor.0 = 0;
                    self.wrap_pending = false;
                    return;
                }
                0x0A => {
                    self.wrap_pending = false;
                    self.line_feed();
                    if !(vt && self.mode & DISABLE_NEWLINE_AUTO_RETURN != 0) {
                        self.cursor.0 = 0;
                    }
                    return;
                }
                0x08 => {
                    if self.wrap_pending {
                        self.wrap_pending = false;
                    } else if self.cursor.0 > 0 {
                        self.cursor.0 -= 1;
                    }
                    return;
                }
                0x09 => {
                    let next = ((self.cursor.0 / 8) + 1) * 8;
                    while self.cursor.0 < next.min(self.width - 1) {
                        self.put(b' ' as u16);
                    }
                    if next >= self.width {
                        self.cursor.0 = self.width - 1;
                    }
                    return;
                }
                0x07 => return,
                // Other controls show as their code page 437 glyphs, as
                // conhost draws them (ESC, with VT off, is ←).
                0x00..=0x1F => {
                    const GLYPHS: [u16; 32] = [
                        0x0020, 0x263A, 0x263B, 0x2665, 0x2666, 0x2663, 0x2660, 0x2022,
                        0x25D8, 0x25CB, 0x25D9, 0x2642, 0x2640, 0x266A, 0x266B, 0x263C,
                        0x25BA, 0x25C4, 0x2195, 0x203C, 0x00B6, 0x00A7, 0x25AC, 0x21A8,
                        0x2191, 0x2193, 0x2192, 0x2190, 0x221F, 0x2194, 0x25B2, 0x25BC,
                    ];
                    self.put(GLYPHS[u as usize]);
                    return;
                }
                _ => {}
            }
        }
        self.put(u);
    }

    /// One character into the cell at the cursor; the cursor moves on.
    fn put(&mut self, u: u16) {
        // A surrogate pair takes one cell, as the first half.
        if (0xD800..0xDC00).contains(&u) {
            self.high = Some(u);
            return;
        }
        let ch = if (0xDC00..0xE000).contains(&u) { self.high.take().unwrap_or(0xFFFD) } else { u };
        let wrap = self.mode & ENABLE_WRAP_AT_EOL_OUTPUT != 0;
        if self.wrap_pending && wrap {
            self.wrap_pending = false;
            self.cursor.0 = 0;
            self.line_feed();
        }
        let (x, y) = self.cursor;
        self.set(x, y, Cell { ch, attr: self.attr });
        if x + 1 < self.width {
            self.cursor.0 += 1;
        } else if wrap {
            self.wrap_pending = true;
        }
    }

    // -----| escape sequences |-----

    fn escape_unit(&mut self, u: u16) {
        let state = core::mem::replace(&mut self.vt, Vt::Ground);
        match state {
            Vt::Ground => {}
            Vt::Escape => match u {
                0x5B => self.vt = Vt::Csi { params: Vec::new(), current: None, private: 0, intermediate: 0 },
                0x5D => self.vt = Vt::Osc { text: Vec::new(), escape: false },
                0x28 | 0x29 | 0x2A | 0x2B => self.vt = Vt::Charset,
                // DECSC, DECRC
                0x37 => self.saved = (self.cursor.0, self.cursor.1, self.attr),
                0x38 => {
                    let (x, y, a) = self.saved;
                    self.cursor = (x.min(self.width - 1), y.min(self.height - 1));
                    self.attr = a;
                    self.wrap_pending = false;
                }
                // IND, NEL, RI
                0x44 => self.line_feed(),
                0x45 => {
                    self.line_feed();
                    self.cursor.0 = 0;
                }
                0x4D => self.reverse_line_feed(),
                // RIS: as new.
                0x63 => {
                    let (w, h, rows) = (self.width, self.height, self.window.3);
                    let mode = self.mode;
                    *self = ScreenBuffer::new(w, h, rows);
                    self.mode = mode;
                }
                _ => {}
            },
            Vt::Charset => {}
            Vt::Osc { mut text, escape } => {
                if u == 0x07 || (escape && u == 0x5C) {
                    self.osc(&text);
                } else if u == 0x1B {
                    self.vt = Vt::Osc { text, escape: true };
                } else {
                    text.push(u);
                    self.vt = Vt::Osc { text, escape: false };
                }
            }
            Vt::Csi { mut params, mut current, mut private, mut intermediate } => match u {
                0x30..=0x39 => {
                    current = Some(current.unwrap_or(0).saturating_mul(10).saturating_add((u - 0x30) as u32));
                    self.vt = Vt::Csi { params, current, private, intermediate };
                }
                0x3B | 0x3A => {
                    params.push(current.unwrap_or(0));
                    self.vt = Vt::Csi { params, current: None, private, intermediate };
                }
                0x3C..=0x3F => {
                    private = u;
                    self.vt = Vt::Csi { params, current, private, intermediate };
                }
                0x20..=0x2F => {
                    intermediate = u;
                    self.vt = Vt::Csi { params, current, private, intermediate };
                }
                0x40..=0x7E => {
                    if current.is_some() || !params.is_empty() {
                        params.push(current.unwrap_or(0));
                    }
                    self.csi(u as u8, &params, private, intermediate);
                }
                0x1B => self.vt = Vt::Escape,
                _ => {}
            },
        }
    }

    /// OSC: 0 and 2 set the title.
    fn osc(&mut self, text: &[u16]) {
        let Some(semi) = text.iter().position(|&c| c == b';' as u16) else { return };
        let kind = &text[..semi];
        if kind == [b'0' as u16] || kind == [b'2' as u16] {
            self.title = Some(text[semi + 1..].to_vec());
        }
    }

    fn csi(&mut self, op: u8, p: &[u32], private: u16, intermediate: u16) {
        let arg = |i: usize, default: u32| p.get(i).copied().filter(|&v| v != 0).unwrap_or(default) as usize;
        let (wl, wt, _, wh) = self.window;
        let _ = wl;
        let (top_margin, bottom_margin) = self.margins.unwrap_or((0, wh - 1));
        if private == b'?' as u16 {
            for &mode in p {
                let on = op == b'h';
                match mode {
                    25 => self.cursor_visible = on,
                    7 => {
                        if on { self.mode |= ENABLE_WRAP_AT_EOL_OUTPUT } else { self.mode &= !ENABLE_WRAP_AT_EOL_OUTPUT }
                    }
                    1049 | 1047 | 47 => self.alternate_screen(on),
                    _ => {}
                }
            }
            return;
        }
        if intermediate != 0 {
            // DECSCUSR (cursor shape) and the like: nothing to keep.
            return;
        }
        self.wrap_pending = false;
        match op {
            // CUU, CUD, CUF, CUB: stopping at the margins when inside them.
            b'A' => {
                let floor = if self.cursor.1 >= wt + top_margin { wt + top_margin } else { wt };
                self.cursor.1 = self.cursor.1.saturating_sub(arg(0, 1)).max(floor);
            }
            b'B' | b'e' => {
                let ceiling = if self.cursor.1 <= wt + bottom_margin { wt + bottom_margin } else { wt + wh - 1 };
                self.cursor.1 = (self.cursor.1 + arg(0, 1)).min(ceiling);
            }
            b'C' | b'a' => self.cursor.0 = (self.cursor.0 + arg(0, 1)).min(self.width - 1),
            b'D' => self.cursor.0 = self.cursor.0.saturating_sub(arg(0, 1)),
            b'E' => {
                self.cursor.1 = (self.cursor.1 + arg(0, 1)).min(wt + wh - 1);
                self.cursor.0 = 0;
            }
            b'F' => {
                self.cursor.1 = self.cursor.1.saturating_sub(arg(0, 1)).max(wt);
                self.cursor.0 = 0;
            }
            // CHA, HPA; VPA; CUP, HVP
            b'G' | b'`' => self.cursor.0 = (arg(0, 1) - 1).min(self.width - 1),
            b'd' => self.cursor.1 = wt + (arg(0, 1) - 1).min(wh - 1),
            b'H' | b'f' => {
                self.cursor.1 = wt + (arg(0, 1) - 1).min(wh - 1);
                self.cursor.0 = (arg(1, 1) - 1).min(self.width - 1);
            }
            // ED: below, above, all (and 3: the scrollback, the same here).
            b'J' => {
                let (x, y) = self.cursor;
                let fill = blank(self.attr);
                let (from, to) = match p.first().copied().unwrap_or(0) {
                    0 => (y * self.width + x, (wt + wh) * self.width),
                    1 => (wt * self.width, y * self.width + x + 1),
                    _ => (wt * self.width, (wt + wh) * self.width),
                };
                for i in from..to.min(self.cells.len()) {
                    self.cells[i] = fill;
                }
            }
            // EL: right, left, all.
            b'K' => {
                let (x, y) = self.cursor;
                let (from, to) = match p.first().copied().unwrap_or(0) {
                    0 => (x, self.width),
                    1 => (0, x + 1),
                    _ => (0, self.width),
                };
                for i in from..to.min(self.width) {
                    self.set(i, y, blank(self.attr));
                }
            }
            // ECH
            b'X' => {
                let (x, y) = self.cursor;
                for i in x..(x + arg(0, 1)).min(self.width) {
                    self.set(i, y, blank(self.attr));
                }
            }
            // ICH, DCH
            b'@' => {
                let (x, y) = self.cursor;
                let n = arg(0, 1).min(self.width - x);
                for i in (x..self.width).rev() {
                    let c = if i >= x + n { self.cell(i - n, y) } else { blank(self.attr) };
                    self.set(i, y, c);
                }
            }
            b'P' => {
                let (x, y) = self.cursor;
                let n = arg(0, 1).min(self.width - x);
                for i in x..self.width {
                    let c = if i + n < self.width { self.cell(i + n, y) } else { blank(self.attr) };
                    self.set(i, y, c);
                }
            }
            // IL, DL: within the margins, from the cursor's row.
            b'L' | b'M' => {
                let y = self.cursor.1;
                let (top, bottom) = (wt + top_margin, wt + bottom_margin);
                if y >= top && y <= bottom {
                    if op == b'L' {
                        self.scroll_down(y, bottom, arg(0, 1));
                    } else {
                        self.scroll_up(y, bottom, arg(0, 1));
                    }
                    self.cursor.0 = 0;
                }
            }
            // SU, SD
            b'S' => self.scroll_up(wt + top_margin, wt + bottom_margin, arg(0, 1)),
            b'T' => self.scroll_down(wt + top_margin, wt + bottom_margin, arg(0, 1)),
            b'm' => self.sgr(p),
            // DECSTBM: the margins, then the cursor home.
            b'r' => {
                let top = arg(0, 1) - 1;
                let bottom = (arg(1, wh as u32) - 1).min(wh - 1);
                self.margins = (top < bottom && !(top == 0 && bottom == wh - 1)).then_some((top, bottom));
                self.cursor = (0, wt);
            }
            // Save and restore the cursor (ANSI.SYS).
            b's' => self.saved = (self.cursor.0, self.cursor.1, self.attr),
            b'u' => {
                let (x, y, _) = self.saved;
                self.cursor = (x.min(self.width - 1), y.min(self.height - 1));
            }
            _ => {}
        }
    }

    fn alternate_screen(&mut self, on: bool) {
        if on && self.alternate.is_none() {
            let main = core::mem::replace(&mut self.cells, vec![blank(self.attr); self.width * self.height]);
            self.alternate = Some((main, self.cursor, self.attr));
        } else if !on {
            if let Some((cells, cursor, attr)) = self.alternate.take() {
                if cells.len() == self.cells.len() {
                    self.cells = cells;
                }
                self.cursor = (cursor.0.min(self.width - 1), cursor.1.min(self.height - 1));
                self.attr = attr;
            }
        }
    }

    /// SGR: colours and the like, as console attributes.
    fn sgr(&mut self, p: &[u32]) {
        let p = if p.is_empty() { &[0][..] } else { p };
        let mut i = 0;
        while i < p.len() {
            let v = p[i];
            match v {
                0 => {
                    self.attr = self.default_attr;
                    self.fg = self.default_attr & 0x0F;
                    self.bold = false;
                }
                1 => {
                    self.bold = true;
                    self.attr |= FG_INTENSITY;
                }
                22 => {
                    self.bold = false;
                    self.set_fg(self.fg);
                }
                4 => self.attr |= UNDERSCORE,
                24 => self.attr &= !UNDERSCORE,
                7 => self.attr |= REVERSE_VIDEO,
                27 => self.attr &= !REVERSE_VIDEO,
                30..=37 => self.set_fg(ansi_to_console(v - 30)),
                39 => self.set_fg(self.default_attr & 0x0F),
                40..=47 => self.set_bg(ansi_to_console(v - 40)),
                49 => self.attr = (self.attr & !0xF0) | (self.default_attr & 0xF0),
                90..=97 => self.set_fg(ansi_to_console(v - 90) | 8),
                100..=107 => self.set_bg(ansi_to_console(v - 100) | 8),
                38 | 48 => {
                    let colour = match p.get(i + 1) {
                        Some(5) => {
                            let c = p.get(i + 2).copied().unwrap_or(0);
                            i += 2;
                            Some(xterm_256(c))
                        }
                        Some(2) => {
                            let rgb = (p.get(i + 2).copied().unwrap_or(0), p.get(i + 3).copied().unwrap_or(0), p.get(i + 4).copied().unwrap_or(0));
                            i += 4;
                            Some(nearest(rgb))
                        }
                        _ => None,
                    };
                    if let Some(c) = colour {
                        if v == 38 { self.set_fg(c) } else { self.set_bg(c) }
                    }
                }
                _ => {}
            }
            i += 1;
        }
    }

    fn set_fg(&mut self, c: u16) {
        self.fg = c & 0x0F;
        let shown = if self.bold { self.fg | FG_INTENSITY } else { self.fg };
        self.attr = (self.attr & !0x0F) | shown;
    }

    /// SetConsoleTextAttribute: the attribute as given, bold forgotten.
    pub fn set_attribute(&mut self, attr: u16) {
        self.attr = attr;
        self.fg = attr & 0x0F;
        self.bold = false;
    }

    fn set_bg(&mut self, c: u16) {
        self.attr = (self.attr & !0xF0) | ((c & 0x0F) << 4);
    }
}

/// ANSI's colour order (black, red, green, yellow, blue, magenta, cyan,
/// white) as console bits (blue 1, green 2, red 4).
fn ansi_to_console(i: u32) -> u16 {
    (((i & 1) << 2) | (i & 2) | ((i & 4) >> 2)) as u16
}

/// The classic 16-colour palette, in console order.
const PALETTE: [(u32, u32, u32); 16] = [
    (0, 0, 0), (0, 0, 128), (0, 128, 0), (0, 128, 128), (128, 0, 0), (128, 0, 128), (128, 128, 0), (192, 192, 192),
    (128, 128, 128), (0, 0, 255), (0, 255, 0), (0, 255, 255), (255, 0, 0), (255, 0, 255), (255, 255, 0), (255, 255, 255),
];

fn nearest((r, g, b): (u32, u32, u32)) -> u16 {
    let d = |(pr, pg, pb): (u32, u32, u32)| {
        let (dr, dg, db) = (r as i64 - pr as i64, g as i64 - pg as i64, b as i64 - pb as i64);
        dr * dr + dg * dg + db * db
    };
    (0..16).min_by_key(|&i| d(PALETTE[i])).unwrap_or(7) as u16
}

/// xterm's 256 colours: the 16, a 6x6x6 cube, a grey ramp.
fn xterm_256(c: u32) -> u16 {
    match c {
        0..=7 => ansi_to_console(c),
        8..=15 => ansi_to_console(c - 8) | 8,
        16..=231 => {
            let c = c - 16;
            let level = |v: u32| if v == 0 { 0 } else { 55 + v * 40 };
            nearest((level(c / 36), level((c / 6) % 6), level(c % 6)))
        }
        _ => {
            let grey = 8 + (c.min(255) - 232) * 10;
            nearest((grey, grey, grey))
        }
    }
}
