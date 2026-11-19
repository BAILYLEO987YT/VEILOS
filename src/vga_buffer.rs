// -----| The boot console |-----
//
// `println!` text during boot, before the desktop owns the screen. It used to
// write VGA text mode at 0xB8000, which exists only on BIOS machines with a
// VGA-compatible card in text mode. The bootloader (0.11) now hands over a
// linear framebuffer on BIOS and UEFI alike, and this draws the same
// 16-colour text into it with the kernel's 8x8 font (`ui::glyph`).
//
// A copy of the characters is kept (`cell`) for the remote-desktop capture.
// Once a display driver binds a head (graphics.rs), the screen is the
// desktop's: `stop_drawing` is called and text only reaches the copy (and
// the serial port, through `serial_println!`).

use core::fmt;
use core::sync::atomic::{AtomicBool, Ordering};
use spin::Mutex;

// -----| vga colours |-----

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum VgaColour {
    Black = 0,
    Blue = 1,
    Green = 2,
    Cyan = 3,
    Red = 4,
    Magenta = 5,
    Brown = 6,
    LightGray = 7,
    DarkGray = 8,
    LightBlue = 9,
    LightGreen = 10,
    LightCyan = 11,
    LightRed = 12,
    Pink = 13,
    Yellow = 14,
    White = 15,
}

/// The VGA palette as 0xRRGGBB.
const PALETTE: [u32; 16] = [
    0x000000, 0x0000AA, 0x00AA00, 0x00AAAA, 0xAA0000, 0xAA00AA, 0xAA5500, 0xAAAAAA,
    0x555555, 0x5555FF, 0x55FF55, 0x55FFFF, 0xFF5555, 0xFF55FF, 0xFFFF55, 0xFFFFFF,
];

/// Foreground in the low nibble, background in the high one, as in VGA.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(transparent)]
struct ColourCode(u8);

impl ColourCode {
    const fn new(foreground: VgaColour, background: VgaColour) -> ColourCode {
        ColourCode((background as u8) << 4 | (foreground as u8))
    }
}

// -----| the framebuffer |-----

/// How pixels are laid out (the bootloader's `PixelFormat`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pixels {
    /// Blue in the lowest byte (UEFI GOP's usual BGRX, and VBE's).
    Bgr,
    /// Red in the lowest byte.
    Rgb,
    /// One byte of grey.
    Grey,
}

/// The firmware framebuffer as the bootloader mapped it.
#[derive(Clone, Copy, Debug)]
pub struct FirmwareFb {
    /// Virtual address (the bootloader's mapping).
    pub virt: u64,
    pub byte_len: usize,
    pub width: usize,
    pub height: usize,
    /// Pixels per line.
    pub stride: usize,
    pub bytes_per_pixel: usize,
    pub pixels: Pixels,
}

static FIRMWARE_FB: Mutex<Option<FirmwareFb>> = Mutex::new(None);
static DRAWING: AtomicBool = AtomicBool::new(false);

/// The framebuffer the firmware set up, if there is one.
pub fn firmware_framebuffer() -> Option<FirmwareFb> {
    *FIRMWARE_FB.lock()
}

/// From kernel_main, first thing: draw the console here from now on.
pub fn init_framebuffer(fb: FirmwareFb) {
    *FIRMWARE_FB.lock() = Some(fb);
    let mut w = VGA_WRITER.lock();
    w.cols = (fb.width / CELL_W).clamp(1, MAX_COLS);
    w.rows = (fb.height / CELL_H).clamp(1, MAX_ROWS);
    DRAWING.store(true, Ordering::Release);
    w.clear_screen();
}

/// A display driver owns the screen now (graphics.rs): keep the text, stop
/// drawing it.
pub fn stop_drawing() {
    DRAWING.store(false, Ordering::Release);
}

const CELL_W: usize = 8;
const CELL_H: usize = 10;
const MAX_COLS: usize = 240;
const MAX_ROWS: usize = 108;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
struct ScreenChar {
    ascii_character: u8,
    color_code: ColourCode,
}

const BLANK: ScreenChar = ScreenChar { ascii_character: b' ', color_code: ColourCode::new(VgaColour::LightGray, VgaColour::Black) };

pub struct Writer {
    column_position: usize,
    colour_code: ColourCode,
    cols: usize,
    rows: usize,
    chars: [[ScreenChar; MAX_COLS]; MAX_ROWS],
}

impl Writer {
    pub fn write_byte(&mut self, byte: u8) {
        match byte {
            b'\n' => self.new_line(),
            byte => {
                if self.column_position >= self.cols {
                    self.new_line();
                }
                let (row, col) = (self.rows - 1, self.column_position);
                let ch = ScreenChar { ascii_character: byte, color_code: self.colour_code };
                self.chars[row][col] = ch;
                self.draw_cell(row, col, ch);
                self.column_position += 1;
            }
        }
    }

    fn new_line(&mut self) {
        // Move every row up one, redrawing only the cells that change:
        // reading the framebuffer back to move it would be far slower (it is
        // uncached or write-combining memory), and most cells are blank.
        let rows = self.rows;
        let blank = ScreenChar { ascii_character: b' ', color_code: self.colour_code };
        let drawing = self.screen().is_some();
        for row in 0..rows {
            for col in 0..self.cols {
                let next = if row + 1 < rows { self.chars[row + 1][col] } else { blank };
                if self.chars[row][col] != next {
                    self.chars[row][col] = next;
                    if drawing {
                        self.draw_cell(row, col, next);
                    }
                }
            }
        }
        self.column_position = 0;
    }

    fn clear_screen(&mut self) {
        self.chars = [[BLANK; MAX_COLS]; MAX_ROWS];
        if let Some(fb) = self.screen() {
            unsafe { core::ptr::write_bytes(fb.virt as *mut u8, 0, fb.stride * fb.height * fb.bytes_per_pixel) };
        }
        self.column_position = 0;
    }

    /// The framebuffer, while the console may draw on it.
    fn screen(&self) -> Option<FirmwareFb> {
        if !DRAWING.load(Ordering::Acquire) {
            return None;
        }
        FIRMWARE_FB.try_lock().and_then(|f| *f)
    }

    fn draw_cell(&self, row: usize, col: usize, ch: ScreenChar) {
        let Some(fb) = self.screen() else { return };
        let fg = PALETTE[(ch.color_code.0 & 0x0F) as usize];
        let bg = PALETTE[(ch.color_code.0 >> 4) as usize];
        let glyph = crate::ui::glyph(ch.ascii_character as char);
        let (x0, y0) = (col * CELL_W, row * CELL_H);
        for dy in 0..CELL_H {
            let bits = if (1..9).contains(&dy) { glyph[dy - 1] } else { 0 };
            for dx in 0..CELL_W {
                let on = bits & (0x80 >> dx) != 0;
                put_pixel(&fb, x0 + dx, y0 + dy, if on { fg } else { bg });
            }
        }
    }

    pub fn write_string(&mut self, s: &str) {
        for byte in s.bytes() {
            match byte {
                0x20..=0x7e | b'\n' => self.write_byte(byte),
                _ => self.write_byte(0xfe), // Print a ■ mapping for unprintable characters
            }
        }
    }

    // Allows us to change colours dynamically
    pub fn set_colour(&mut self, foreground: VgaColour, background: VgaColour) {
        self.colour_code = ColourCode::new(foreground, background);
    }

    /// The character cell at (col, row) of the bottom 80x25 of the console,
    /// as a VGA text cell (character, then colour in the high byte).
    pub fn cell(&self, col: usize, row: usize) -> u16 {
        let top = self.rows.saturating_sub(25);
        match self.chars.get(top + row).and_then(|r| r[..self.cols].get(col)) {
            Some(c) => c.ascii_character as u16 | (c.color_code.0 as u16) << 8,
            None => 0,
        }
    }
}

fn put_pixel(fb: &FirmwareFb, x: usize, y: usize, rgb: u32) {
    if x >= fb.width || y >= fb.height {
        return;
    }
    let at = (y * fb.stride + x) * fb.bytes_per_pixel;
    if at + fb.bytes_per_pixel > fb.byte_len {
        return;
    }
    let (r, g, b) = ((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8);
    let p = (fb.virt + at as u64) as *mut u8;
    unsafe {
        match (fb.pixels, fb.bytes_per_pixel) {
            (Pixels::Grey, _) => *p = ((r as u16 + g as u16 + b as u16) / 3) as u8,
            (Pixels::Bgr, 4) => (p as *mut u32).write_volatile(rgb),
            (Pixels::Rgb, 4) => (p as *mut u32).write_volatile(r as u32 | (g as u32) << 8 | (b as u32) << 16),
            (Pixels::Bgr, _) => {
                *p = b;
                *p.add(1) = g;
                *p.add(2) = r;
            }
            (Pixels::Rgb, _) => {
                *p = r;
                *p.add(1) = g;
                *p.add(2) = b;
            }
        }
    }
}

impl fmt::Write for Writer {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.write_string(s);
        Ok(())
    }
}

// -----| global interface |-----

pub static VGA_WRITER: Mutex<Writer> = Mutex::new(Writer {
    column_position: 0,
    colour_code: ColourCode::new(VgaColour::LightGray, VgaColour::Black),
    cols: 80,
    rows: 25,
    chars: [[BLANK; MAX_COLS]; MAX_ROWS],
});

#[doc(hidden)]
pub fn _print(args: fmt::Arguments) {
    use core::fmt::Write;
    // Interrupts off, so a handler that prints can't deadlock on the writer.
    x86_64::instructions::interrupts::without_interrupts(|| {
        VGA_WRITER.lock().write_fmt(args).unwrap();
    });
}
