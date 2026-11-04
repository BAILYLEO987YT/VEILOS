//! The standard toolbar pictures (HINST_COMMCTRL's IDB_STD_*, IDB_VIEW_*,
//! IDB_HIST_*): VEILOS's own small drawings, made when first asked for, in
//! their documented order. One colour stands for transparent.

use alloc::vec::Vec;

/// The transparent colour, as COLORREF.
pub const KEY: u32 = 0x00FF_00FF;

const INK: u32 = 0x202020;
const PAPER: u32 = 0xFFFFFF;
const FOLDER: u32 = 0xE8C050;
const BLUE: u32 = 0x3070C0;
const RED: u32 = 0xC04030;
const GREEN: u32 = 0x40A050;
const GRAY: u32 = 0x909090;

struct Canvas {
    px: Vec<u32>,
    w: i32,
    h: i32,
    /// The drawing's cell, and its scale in halves (2 for 16 pixels, 3 for 24).
    x: i32,
    scale: i32,
}

impl Canvas {
    /// A filled box in the 16-square design grid.
    fn r(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, c: u32) {
        let s = self.scale;
        for y in y0 * s / 2..y1 * s / 2 {
            for x in x0 * s / 2..x1 * s / 2 {
                let (px, py) = (self.x + x, y);
                if px >= 0 && py >= 0 && px < self.w && py < self.h {
                    self.px[(py * self.w + px) as usize] = c;
                }
            }
        }
    }
    /// A box with an outline.
    fn b(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, fill: u32) {
        self.r(x0, y0, x1, y1, INK);
        self.r(x0 + 1, y0 + 1, x1 - 1, y1 - 1, fill);
    }
    /// A page.
    fn page(&mut self, x0: i32, y0: i32, x1: i32, y1: i32) {
        self.b(x0, y0, x1, y1, PAPER);
    }
    fn lines(&mut self, x0: i32, x1: i32, y0: i32, y1: i32) {
        let mut y = y0;
        while y < y1 {
            self.r(x0, y, x1, y + 1, GRAY);
            y += 2;
        }
    }
    /// Arrow along a row, pointing left or right.
    fn arrow(&mut self, y: i32, left: bool, c: u32) {
        self.r(if left { 6 } else { 2 }, y - 1, if left { 14 } else { 10 }, y + 1, c);
        for k in 0..5 {
            let x = if left { 6 - k } else { 9 + k };
            self.r(x, y - 4 + k, x + 1, y + 4 - k, c);
        }
    }
    fn folder(&mut self, x0: i32, y0: i32) {
        self.r(x0, y0, x0 + 6, y0 + 2, INK);
        self.b(x0, y0 + 1, x0 + 14, y0 + 11, FOLDER);
    }
}

fn std_picture(c: &mut Canvas, i: usize) {
    match i {
        // Cut: two blades and handles.
        0 => {
            for k in 0..7 {
                c.r(4 + k, 1 + k, 5 + k, 2 + k, INK);
                c.r(11 - k, 1 + k, 12 - k, 2 + k, INK);
            }
            c.b(2, 9, 7, 14, PAPER);
            c.b(9, 9, 14, 14, PAPER);
        }
        // Copy: two pages.
        1 => {
            c.page(1, 1, 10, 12);
            c.page(6, 4, 15, 15);
            c.lines(8, 13, 7, 13);
        }
        // Paste: a clipboard with a page.
        2 => {
            c.b(2, 2, 12, 15, FOLDER);
            c.b(5, 1, 9, 4, GRAY);
            c.page(7, 6, 15, 15);
            c.lines(9, 13, 8, 14);
        }
        3 => c.arrow(8, true, BLUE),
        4 => c.arrow(8, false, BLUE),
        // Delete: a red cross.
        5 => {
            for k in 0..10 {
                c.r(3 + k, 3 + k, 5 + k, 4 + k, RED);
                c.r(12 - k, 3 + k, 14 - k, 4 + k, RED);
            }
        }
        // New: a page with a folded corner.
        6 => {
            c.page(3, 1, 13, 15);
            c.r(10, 1, 13, 4, GRAY);
        }
        // Open: a folder.
        7 => {
            c.folder(1, 3);
            c.r(10, 1, 12, 3, GREEN);
        }
        // Save: a diskette.
        8 => {
            c.b(2, 1, 14, 15, BLUE);
            c.r(4, 2, 12, 7, PAPER);
            c.r(5, 10, 11, 15, GRAY);
        }
        // Print preview: a page and a lens.
        9 => {
            c.page(1, 1, 10, 15);
            c.b(8, 6, 14, 12, PAPER);
            c.r(13, 12, 15, 14, INK);
        }
        // Properties: a page with a check.
        10 => {
            c.page(2, 1, 14, 15);
            for k in 0..3 {
                c.r(4 + k, 7 + k, 5 + k, 9 + k, GREEN);
            }
            for k in 0..5 {
                c.r(7 + k, 9 - k, 8 + k, 11 - k, GREEN);
            }
        }
        // Help: a question mark.
        11 => {
            c.r(5, 2, 11, 4, BLUE);
            c.r(10, 3, 12, 7, BLUE);
            c.r(7, 6, 11, 8, BLUE);
            c.r(7, 8, 9, 10, BLUE);
            c.r(7, 12, 9, 14, BLUE);
        }
        // Find: a lens.
        12 => {
            c.b(2, 2, 10, 10, PAPER);
            for k in 0..5 {
                c.r(9 + k, 9 + k, 11 + k, 11 + k, INK);
            }
        }
        // Replace: a lens and a pen.
        13 => {
            c.b(1, 1, 8, 8, PAPER);
            c.r(7, 7, 10, 10, INK);
            for k in 0..6 {
                c.r(14 - k, 6 + k, 16 - k, 8 + k, BLUE);
            }
        }
        // Print: a printer.
        _ => {
            c.page(4, 1, 12, 7);
            c.b(1, 6, 15, 12, GRAY);
            c.page(4, 10, 12, 15);
        }
    }
}

fn view_picture(c: &mut Canvas, i: usize) {
    match i {
        // Large icons.
        0 => {
            c.b(1, 1, 7, 7, BLUE);
            c.b(9, 1, 15, 7, BLUE);
            c.b(1, 9, 7, 15, BLUE);
            c.b(9, 9, 15, 15, BLUE);
        }
        // Small icons.
        1 => {
            for y in [2, 7, 12] {
                for x in [2, 9] {
                    c.r(x, y, x + 2, y + 2, BLUE);
                    c.r(x + 3, y, x + 6, y + 1, INK);
                }
            }
        }
        // List.
        2 => {
            for y in [2, 6, 10] {
                c.r(2, y, 4, y + 2, BLUE);
                c.r(5, y, 14, y + 1, INK);
            }
        }
        // Details.
        3 => {
            c.r(2, 2, 14, 4, GRAY);
            for y in [6, 9, 12] {
                c.r(2, y, 7, y + 1, INK);
                c.r(9, y, 14, y + 1, GRAY);
            }
        }
        // Sort by name, size, date, type: lines and a small arrow.
        4..=7 => {
            c.lines(2, 9, 3, 14);
            c.r(11, 2, 13, 12, BLUE);
            for k in 0..3 {
                c.r(10 + k, 11 + k, 14 - k, 12 + k, BLUE);
            }
            c.r(2, 1, 2 + (i as i32 - 3) * 2, 2, RED);
        }
        // Parent folder.
        8 => {
            c.folder(1, 3);
            c.r(7, 6, 9, 12, INK);
            c.r(5, 7, 11, 8, INK);
            c.r(6, 6, 10, 7, INK);
        }
        // Network connect, disconnect.
        9 | 10 => {
            c.b(1, 2, 8, 8, BLUE);
            c.b(8, 9, 15, 15, BLUE);
            c.r(6, 8, 7, 12, INK);
            c.r(6, 11, 8, 12, INK);
            if i == 10 {
                for k in 0..10 {
                    c.r(3 + k, 3 + k, 5 + k, 4 + k, RED);
                }
            }
        }
        // New folder.
        _ => {
            c.folder(1, 4);
            c.r(11, 1, 13, 7, RED);
            c.r(9, 3, 15, 5, RED);
        }
    }
}

fn hist_picture(c: &mut Canvas, i: usize) {
    match i {
        0 => c.arrow(8, true, GREEN),
        1 => c.arrow(8, false, GREEN),
        // Favorites: a star; add: with a plus.
        2 | 3 => {
            c.r(7, 2, 9, 13, FOLDER);
            c.r(2, 6, 14, 9, FOLDER);
            c.r(5, 4, 11, 12, FOLDER);
            if i == 3 {
                c.r(12, 1, 14, 7, GREEN);
                c.r(10, 3, 16, 5, GREEN);
            }
        }
        // Folder tree.
        _ => {
            c.r(4, 3, 5, 13, INK);
            c.r(4, 7, 8, 8, INK);
            c.r(4, 12, 8, 13, INK);
            c.b(1, 1, 7, 5, FOLDER);
            c.b(8, 5, 14, 9, FOLDER);
            c.b(8, 10, 14, 14, FOLDER);
        }
    }
}

/// How many pictures a set has (by its IDB_ number), its picture function,
/// and whether it's the large form (24 pixels against 16).
fn set(id: u64) -> Option<(usize, fn(&mut Canvas, usize), bool)> {
    Some(match id {
        0 => (15, std_picture as fn(&mut Canvas, usize), false),
        1 => (15, std_picture, true),
        4 => (12, view_picture, false),
        5 => (12, view_picture, true),
        8 => (5, hist_picture, false),
        9 => (5, hist_picture, true),
        _ => return None,
    })
}

/// A strip of the set's pictures, each `cx` by `cy`: a bitmap with `KEY` for
/// its background, and the pictures in it.
pub fn make(id: u64, cx: i32, cy: i32) -> Option<(u64, usize)> {
    let (n, draw, large) = set(id)?;
    let w = cx * n as i32;
    let mut c = Canvas { px: alloc::vec![0x00FF00FF; (w * cy) as usize], w, h: cy, x: 0, scale: if large { 3 } else { 2 } };
    for i in 0..n {
        c.x = i as i32 * cx;
        draw(&mut c, i);
    }
    // Stored 0x00RRGGBB, as a 32-bit bitmap's pixels are.
    let bm = unsafe { crate::CreateBitmap(w, cy, 1, 32, c.px.as_ptr() as *const u8) };
    if bm == 0 { None } else { Some((bm, n)) }
}
