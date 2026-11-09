// src/audio.rs

use core::sync::atomic::{AtomicU8, Ordering};

static VOLUME: AtomicU8 = AtomicU8::new(50); // 0-100

pub fn get_volume() -> u8 {
    VOLUME.load(Ordering::Relaxed)
}

pub fn set_volume(vol: u8) {
    let v = if vol > 100 { 100 } else { vol };
    VOLUME.store(v, Ordering::Relaxed);
    // Real hardware mixer update would go here
}
