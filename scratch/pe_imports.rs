
use std::fs;
fn main() {
    let bytes = fs::read("storage/desktop/gui_test.exe").unwrap();
    // find kernel32.dll
    let k32_pos = bytes.windows(12).position(|w| w == b"kernel32.dll").unwrap();
    println!("kernel32.dll at {}", k32_pos);
    // scan backwards for 00 00
    // Actually, I can just scan the file for all strings matching [A-Za-z0-9_]{5,}
    let mut current_string = String::new();
    for &b in &bytes {
        if b >= 0x20 && b <= 0x7E {
            current_string.push(b as char);
        } else {
            if current_string.len() >= 5 {
                if current_string.contains("Process") || current_string.contains("Exit") {
                    println!("Found: {}", current_string);
                }
            }
            current_string.clear();
        }
    }
}
