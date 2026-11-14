// -----| Address space layout randomization |-----
//
// Images: a random shift chosen once per boot, as Windows does, so a DLL or
// program is at the same address in every process of that boot (Git Bash's
// fork copies the parent's memory into a child that must look the same) and
// at a different one after a reboot. All DLLs share one shift, so they keep
// their 16 MiB spacing and never overlap one another; each program gets its
// own. Both modes: the layout must not change when the mode does, or a child
// started after a switch wouldn't match its parent. An image without
// relocations stays at its preferred base.
//
// Security Mode also places each process's first stack at a random 64 KiB
// step inside its slot.

use core::sync::atomic::{AtomicU64, Ordering};

/// Shifts are whole 64 KiB steps below 64 GiB.
const STEPS: u64 = 1 << 20;
const STEP: u64 = 0x1_0000;

static SEED: AtomicU64 = AtomicU64::new(0);

fn seed() -> u64 {
    let s = SEED.load(Ordering::Relaxed);
    if s != 0 {
        return s;
    }
    let mut b = [0u8; 8];
    crate::entropy::fill(&mut b);
    let new = u64::from_le_bytes(b) | 1;
    match SEED.compare_exchange(0, new, Ordering::Relaxed, Ordering::Relaxed) {
        Ok(_) => new,
        Err(old) => old,
    }
}

fn mix(mut x: u64) -> u64 {
    x ^= x >> 33;
    x = x.wrapping_mul(0xFF51_AFD7_ED55_8CCD);
    x ^= x >> 33;
    x = x.wrapping_mul(0xC4CE_B9FE_1A85_EC53);
    x ^ (x >> 33)
}

/// How many places an image is offered before its preferred base.
pub const TRIES: u64 = 8;

/// This boot's `try_`th shift for every DLL. Every process tries them in the
/// same order and takes the first free place, so they agree.
pub fn dll_shift(try_: u64) -> u64 {
    (mix(seed().wrapping_add(try_)) % STEPS) * STEP
}

/// This boot's `try_`th shift for the program `name` (lower case, no path).
pub fn exe_shift(name: &str, try_: u64) -> u64 {
    let mut h = seed() ^ 0x9E37_79B9_7F4A_7C15 ^ try_;
    for b in name.bytes() {
        h = mix(h ^ b as u64);
    }
    (h % STEPS) * STEP
}

/// Security Mode: where in a `slot`-byte slot a `size`-byte stack goes.
pub fn stack_offset(slot: u64, size: u64) -> u64 {
    if crate::policy::performance() || size >= slot {
        return 0;
    }
    let mut b = [0u8; 8];
    crate::entropy::fill(&mut b);
    (u64::from_le_bytes(b) % ((slot - size) / STEP)) * STEP
}

/// IMAGE_FILE_RELOCS_STRIPPED in a PE's headers: it can't move.
pub fn relocs_stripped(headers: &[u8]) -> bool {
    let Some(b) = headers.get(0x3C..0x40) else { return true };
    let pe = u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize;
    headers.get(pe + 22..pe + 24).is_none_or(|c| u16::from_le_bytes([c[0], c[1]]) & 1 != 0)
}
