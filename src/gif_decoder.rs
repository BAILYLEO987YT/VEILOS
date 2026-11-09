// =====================================================================
// -----| VEILOS Native GIF Decoder (no_std) |-----
// =====================================================================
//
// Supports:
//   [x] GIF87a and GIF89a headers
//   [x] Global and Local Color Tables
//   [x] LZW decompression (variable-width codes, max 12-bit)
//   [x] Animated GIFs (Graphic Control Extension with frame delays)
//   [x] Transparency
//   [x] Frame compositing for animation sequences

#![allow(dead_code)]

use alloc::vec::Vec;

// -----| public types |-----

/// A single decoded GIF frame with RGBA pixel data.
pub struct GifFrame {
    pub pixels:   Vec<u8>,   // RGBA pixels (width * height * 4)
    pub width:    u16,
    pub height:   u16,
    pub delay_ms: u16,       // Frame delay in milliseconds (0 = no delay)
    pub x:        u16,       // Frame X offset within canvas
    pub y:        u16,       // Frame Y offset within canvas
}

/// A fully decoded GIF with one or more frames.
pub struct DecodedGif {
    pub width:  u16,
    pub height: u16,
    pub frames: Vec<GifFrame>,
}

// -----| LZW decoder |-----

const MAX_CODE_SIZE: usize = 12;
const MAX_TABLE_SIZE: usize = 1 << MAX_CODE_SIZE; // 4096

struct LzwDecoder {
    min_code_size: u8,
    clear_code:    u16,
    eoi_code:      u16,
    next_code:     u16,
    code_size:     u8,
    // Table: each entry is (prefix, suffix). For root codes prefix == u16::MAX.
    table_prefix:  [u16; MAX_TABLE_SIZE],
    table_suffix:  [u8; MAX_TABLE_SIZE],
    table_len:     [u16; MAX_TABLE_SIZE],  // length of string for each code
    output:        Vec<u8>,
}

impl LzwDecoder {
    fn new(min_code_size: u8) -> Self {
        let clear_code = 1u16 << min_code_size;
        let eoi_code = clear_code + 1;

        let mut dec = Self {
            min_code_size,
            clear_code,
            eoi_code,
            next_code: 0,
            code_size: 0,
            table_prefix: [0u16; MAX_TABLE_SIZE],
            table_suffix: [0u8; MAX_TABLE_SIZE],
            table_len: [0u16; MAX_TABLE_SIZE],
            output: Vec::new(),
        };
        dec._reset();
        dec
    }

    fn _reset(&mut self) {
        self.code_size = self.min_code_size + 1;
        self.next_code = self.eoi_code + 1;

        // Initialize root entries (0..clear_code)
        for i in 0..self.clear_code {
            self.table_prefix[i as usize] = u16::MAX; // no prefix
            self.table_suffix[i as usize] = i as u8;
            self.table_len[i as usize] = 1;
        }
    }

    fn _output_code(&mut self, code: u16) {
        if code as usize >= MAX_TABLE_SIZE { return; }
        // Walk the chain and output bytes in reverse, then reverse
        let len = self.table_len[code as usize] as usize;
        let start = self.output.len();
        self.output.resize(start + len, 0);

        let mut c = code;
        let mut pos = start + len;
        while pos > start {
            if c as usize >= MAX_TABLE_SIZE { break; } // safety against malformed chain
            pos -= 1;
            self.output[pos] = self.table_suffix[c as usize];
            c = self.table_prefix[c as usize];
        }
    }

    fn _first_byte(&self, code: u16) -> u8 {
        let mut c = code;
        let mut safety = 0;
        while (c as usize) < MAX_TABLE_SIZE && self.table_prefix[c as usize] != u16::MAX {
            c = self.table_prefix[c as usize];
            safety += 1;
            if safety > MAX_TABLE_SIZE { return 0; }
        }
        if (c as usize) >= MAX_TABLE_SIZE { return 0; }
        self.table_suffix[c as usize]
    }

    /// Decode LZW-compressed sub-blocks into pixel indices.
    fn decode(&mut self, sub_blocks: &[u8]) -> Vec<u8> {
        self.output.clear();

        let mut bit_buf: u32 = 0;
        let mut bits_in_buf: u8 = 0;
        let mut pos: usize = 0;
        let mut prev_code: Option<u16> = None;

        loop {
            // Read next code
            while bits_in_buf < self.code_size {
                if pos >= sub_blocks.len() { return core::mem::take(&mut self.output); }
                bit_buf |= (sub_blocks[pos] as u32) << bits_in_buf;
                bits_in_buf += 8;
                pos += 1;
            }

            let mask = (1u32 << self.code_size) - 1;
            let code = (bit_buf & mask) as u16;
            bit_buf >>= self.code_size;
            bits_in_buf -= self.code_size;

            if code == self.clear_code {
                self._reset();
                prev_code = None;
                continue;
            }
            if code == self.eoi_code {
                break;
            }

            if let Some(prev) = prev_code {
                if code < self.next_code {
                    // Code exists in table
                    self._output_code(code);
                    let first = self._first_byte(code);

                    // Add new entry: prev + first byte of code
                    if (self.next_code as usize) < MAX_TABLE_SIZE {
                        let nc = self.next_code as usize;
                        self.table_prefix[nc] = prev;
                        self.table_suffix[nc] = first;
                        self.table_len[nc] = self.table_len[prev as usize] + 1;
                        self.next_code += 1;
                    }
                } else if code == self.next_code {
                    // Code == next_code (special case)
                    let first = self._first_byte(prev);

                    // Add new entry first
                    if (self.next_code as usize) < MAX_TABLE_SIZE {
                        let nc = self.next_code as usize;
                        self.table_prefix[nc] = prev;
                        self.table_suffix[nc] = first;
                        self.table_len[nc] = self.table_len[prev as usize] + 1;
                        self.next_code += 1;
                    }

                    // Output the new entry
                    self._output_code(code);
                } else {
                    // Invalid code, malformed stream
                    break;
                }

                // Grow code size if needed
                if self.next_code >= (1 << self.code_size) && self.code_size < MAX_CODE_SIZE as u8 {
                    self.code_size += 1;
                }
            } else {
                // First code after clear or start
                self._output_code(code);
            }

            prev_code = Some(code);
        }

        core::mem::take(&mut self.output)
    }
}

// -----| GIF parser |-----

fn read_u16_le(data: &[u8], off: usize) -> u16 {
    if off + 1 >= data.len() { return 0; }
    u16::from_le_bytes([data[off], data[off + 1]])
}

/// Decode a GIF file into one or more RGBA frames.
pub fn decode_gif(data: &[u8]) -> Option<DecodedGif> {
    if data.len() < 13 { return None; }

    // -----| header |-----
    let sig = &data[0..6];
    if sig != b"GIF87a" && sig != b"GIF89a" { return None; }

    let canvas_w = read_u16_le(data, 6);
    let canvas_h = read_u16_le(data, 8);
    
    // Safety check to prevent memory exhaustion and panics
    if canvas_w == 0 || canvas_h == 0 || canvas_w > 4096 || canvas_h > 4096 { return None; }
    
    let packed = data[10];
    let _bg_index = data[11];
    // data[12] = pixel aspect ratio (ignored)

    let has_gct = (packed & 0x80) != 0;
    let gct_size = if has_gct { 3 * (1usize << ((packed & 0x07) as usize + 1)) } else { 0 };

    let mut pos = 13;

    // -----| global color table |-----
    let mut gct = Vec::new();
    if has_gct {
        if pos + gct_size > data.len() { return None; }
        gct.extend_from_slice(&data[pos..pos + gct_size]);
        pos += gct_size;
    }

    let mut frames: Vec<GifFrame> = Vec::new();

    // Animation state from Graphic Control Extension
    let mut delay_ms: u16 = 0;
    let mut transparent_index: Option<u8> = None;
    let mut _dispose_method: u8 = 0;

    // Canvas buffer for compositing animated GIFs
    let canvas_pixels = canvas_w as usize * canvas_h as usize * 4;
    let mut canvas = Vec::new();
    canvas.resize(canvas_pixels, 0u8);

    // -----| block loop |-----
    while pos < data.len() {
        match data[pos] {
            0x2C => {
                // -----| Image Descriptor |-----
                if pos + 10 > data.len() { break; }
                pos += 1;

                let frame_x = read_u16_le(data, pos); pos += 2;
                let frame_y = read_u16_le(data, pos); pos += 2;
                let frame_w = read_u16_le(data, pos); pos += 2;
                let frame_h = read_u16_le(data, pos); pos += 2;
                let img_packed = data[pos]; pos += 1;

                let has_lct = (img_packed & 0x80) != 0;
                let _interlaced = (img_packed & 0x40) != 0;
                let lct_size = if has_lct { 3 * (1usize << ((img_packed & 0x07) as usize + 1)) } else { 0 };

                // Local color table
                let color_table = if has_lct {
                    if pos + lct_size > data.len() { break; }
                    let lct = &data[pos..pos + lct_size];
                    pos += lct_size;
                    lct
                } else {
                    &gct
                };

                // LZW minimum code size
                if pos >= data.len() { break; }
                let min_code_size = data[pos]; pos += 1;
                if min_code_size > 11 { break; } // Invalid

                // Collect sub-blocks into a single buffer
                let mut lzw_data = Vec::new();
                loop {
                    if pos >= data.len() { break; }
                    let block_size = data[pos] as usize; pos += 1;
                    if block_size == 0 { break; } // Block terminator
                    if pos + block_size > data.len() { break; }
                    lzw_data.extend_from_slice(&data[pos..pos + block_size]);
                    pos += block_size;
                }

                // Decompress
                let mut lzw = LzwDecoder::new(min_code_size);
                let indices = lzw.decode(&lzw_data);

                // Convert indices to RGBA and composite onto canvas
                let fw = frame_w as usize;
                let fh = frame_h as usize;
                let cw = canvas_w as usize;

                for py in 0..fh {
                    if py * fw >= indices.len() { break; }
                    for px in 0..fw {
                        let idx_pos = py * fw + px;
                        if idx_pos >= indices.len() { break; }

                        let color_idx = indices[idx_pos] as usize;

                        // Skip transparent pixels
                        if let Some(ti) = transparent_index {
                            if color_idx == ti as usize { continue; }
                        }

                        let cx = frame_x as usize + px;
                        let cy = frame_y as usize + py;
                        if cx >= cw || cy >= canvas_h as usize { continue; }

                        let canvas_off = (cy * cw + cx) * 4;
                        let ct_off = color_idx * 3;
                        if ct_off + 2 < color_table.len() && canvas_off + 3 < canvas.len() {
                            canvas[canvas_off]     = color_table[ct_off];     // R
                            canvas[canvas_off + 1] = color_table[ct_off + 1]; // G
                            canvas[canvas_off + 2] = color_table[ct_off + 2]; // B
                            canvas[canvas_off + 3] = 255;                     // A
                        }
                    }
                }

                // Snapshot the current canvas as a frame
                frames.push(GifFrame {
                    pixels: canvas.clone(),
                    width:  canvas_w,
                    height: canvas_h,
                    delay_ms,
                    x: 0,
                    y: 0,
                });

                // Reset per-frame state
                delay_ms = 0;
                transparent_index = None;
            }

            0x21 => {
                // -----| Extension Block |-----
                if pos + 1 >= data.len() { break; }
                pos += 1;
                let ext_label = data[pos]; pos += 1;

                match ext_label {
                    0xF9 => {
                        // Graphic Control Extension
                        if pos >= data.len() { break; }
                        let block_size = data[pos] as usize; pos += 1;
                        if block_size >= 4 && pos + 4 <= data.len() {
                            let gce_packed = data[pos];
                            _dispose_method = (gce_packed >> 2) & 0x07;
                            let has_transparent = (gce_packed & 0x01) != 0;
                            delay_ms = read_u16_le(data, pos + 1) * 10; // Convert centiseconds to ms
                            if has_transparent {
                                transparent_index = Some(data[pos + 3]);
                            } else {
                                transparent_index = None;
                            }
                        }
                        pos += block_size;
                        // Skip block terminator
                        if pos < data.len() && data[pos] == 0 { pos += 1; }
                    }
                    _ => {
                        // Skip unknown extension sub-blocks
                        loop {
                            if pos >= data.len() { break; }
                            let block_size = data[pos] as usize; pos += 1;
                            if block_size == 0 { break; }
                            pos += block_size;
                        }
                    }
                }
            }

            0x3B => {
                // -----| Trailer (end of GIF) |-----
                break;
            }

            _ => {
                // Unknown block, skip
                pos += 1;
            }
        }
    }

    if frames.is_empty() { return None; }

    Some(DecodedGif {
        width: canvas_w,
        height: canvas_h,
        frames,
    })
}
