
use std::fs;
use std::process::Command;
fn main() {
    let bytes = fs::read("storage/desktop/gui_test.exe").unwrap();
    let text = &bytes[0x400..0x600];
    fs::write("scratch/text.bin", text).unwrap();
}
