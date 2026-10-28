
use iced_x86::{Decoder, DecoderOptions, Formatter, NasmFormatter};
use std::fs;

fn main() {
    let bytes = fs::read("../../storage/desktop/gui_test.exe").unwrap();
    let text_start = 0x400; // file offset of .text
    let text_end = 0x600;
    
    let mut decoder = Decoder::new(64, &bytes[text_start..text_end], DecoderOptions::NONE);
    decoder.set_ip(0x140001000);
    
    let mut formatter = NasmFormatter::new();
    let mut output = String::new();
    
    for instruction in decoder {
        output.clear();
        formatter.format(&instruction, &mut output);
        println!("{:016X} {}", instruction.ip(), output);
        if instruction.ip() > 0x1400010D0 { break; }
    }
}
