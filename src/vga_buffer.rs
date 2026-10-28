use volatile::Volatile;
use core::fmt;
use lazy_static::lazy_static;
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(transparent)]
struct ColourCode(u8);

impl ColourCode {
    fn new(foreground: VgaColour, background: VgaColour) -> ColourCode {
        ColourCode((background as u8) << 4 | (foreground as u8))
    }
}

// -----| vga character & buffer |-----

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
struct ScreenChar {
    ascii_character: u8,
    color_code: ColourCode,
}

const VGA_WIDTH: usize = 80;
const VGA_HEIGHT: usize = 25;

#[repr(transparent)]
struct Buffer {
    // We use Volatile because the VGA buffer is memory-mapped hardware.
    // Without Volatile, the compiler might optimize away writes to the buffer.
    chars: [[Volatile<ScreenChar>; VGA_WIDTH]; VGA_HEIGHT],
}

// -----| vga writer state |-----

pub struct Writer {
    column_position: usize,
    colour_code: ColourCode,
    buffer: &'static mut Buffer,
}

impl Writer {
    pub fn write_byte(&mut self, byte: u8) {
        match byte {
            b'\n' => self.new_line(),
            byte => {
                if self.column_position >= VGA_WIDTH {
                    self.new_line();
                }

                let row = VGA_HEIGHT - 1;
                let col = self.column_position;

                let colour_code = self.colour_code;
                self.buffer.chars[row][col].write(ScreenChar {
                    ascii_character: byte,
                    color_code: colour_code,
                });
                self.column_position += 1;
            }
        }
    }

    fn new_line(&mut self) {
        // Shift every row up by 1 to scroll the screen
        for row in 1..VGA_HEIGHT {
            for col in 0..VGA_WIDTH {
                let character = self.buffer.chars[row][col].read();
                self.buffer.chars[row - 1][col].write(character);
            }
        }
        self.clear_row(VGA_HEIGHT - 1);
        self.column_position = 0;
    }

    fn clear_row(&mut self, row: usize) {
        let blank = ScreenChar {
            ascii_character: b' ',
            color_code: self.colour_code,
        };
        for col in 0..VGA_WIDTH {
            self.buffer.chars[row][col].write(blank);
        }
    }

    pub fn write_string(&mut self, s: &str) {
        for byte in s.bytes() {
            match byte {
                // Printable ASCII byte or newline
                0x20..=0x7e | b'\n' => self.write_byte(byte),
                // Not part of printable ASCII range
                _ => self.write_byte(0xfe), // Print a ■ mapping for unprintable characters
            }
        }
    }
    
    // Allows us to change colours dynamically
    pub fn set_colour(&mut self, foreground: VgaColour, background: VgaColour) {
        self.colour_code = ColourCode::new(foreground, background);
    }
}

impl fmt::Write for Writer {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.write_string(s);
        Ok(())
    }
}

// -----| global interface |-----

lazy_static! {
    pub static ref VGA_WRITER: Mutex<Writer> = Mutex::new(Writer {
        column_position: 0,
        colour_code: ColourCode::new(VgaColour::LightGray, VgaColour::Black),
        buffer: unsafe { &mut *(0xb8000 as *mut Buffer) },
    });
}

#[doc(hidden)]
pub fn _print(args: fmt::Arguments) {
    use core::fmt::Write;
    // We use without_interrupts so the CPU can't jump to an interrupt handler
    // while we're in the middle of writing to the VGA buffer (which would cause a deadlock).
    x86_64::instructions::interrupts::without_interrupts(|| {
        VGA_WRITER.lock().write_fmt(args).unwrap();
    });
}
