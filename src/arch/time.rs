// =============================================================================
// VEILOS: kernel timebase
// =============================================================================
//
// The TSC is calibrated once at boot against PIT channel 2 and is then the
// clock for busy-wait delays and driver timeouts (`delay_us`, `Deadline`,
// `spin_until`). Before this, drivers counted loop iterations, so a "500 ms"
// timeout lasted anywhere from microseconds to seconds depending on the CPU
// and accelerator.
//
// The PIT channel-0 interrupt (1 kHz) still drives the scheduler and
// SleepFuture wakeups, but the time it publishes (`interrupts::TICKS`) is
// read from the TSC, so lost interrupts don't slow the clock. Channel 2 is
// used here so channel 0 is never disturbed.
//
// `calibrate` is shared with `smp`, which measures the LAPIC timer the same
// way.
//
// Newer chipsets can gate the 8254 off entirely. Without PIT channel 2 the
// TSC rate comes from CPUID leaf 0x15 (crystal and ratio) or 0x16 (base
// MHz), else from the HPET once paging is up (`calibrate_late`), and the
// LAPIC timer is then measured against the TSC. Without channel 0's
// interrupt the BSP ticks from its LAPIC timer instead
// (interrupts::check_tick_source).

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use x86_64::instructions::port::Port;

/// PIT input clock.
pub const PIT_HZ: u64 = 1_193_182;

/// Calibration window: 10 ms of PIT channel 2 (fits the 16-bit counter).
const CALIBRATION_PIT_COUNTS: u16 = 11_932;
/// Rounds per calibration; the shortest is kept, since an SMI or a host
/// preemption can only make a round look longer.
const CALIBRATION_ROUNDS: usize = 3;

/// Used if calibration fails. Deliberately high: on a slower CPU delays and
/// timeouts run long, never short.
const FALLBACK_TSC_HZ: u64 = 4_000_000_000;

static TSC_HZ: AtomicU64 = AtomicU64::new(FALLBACK_TSC_HZ);
static TSC_BOOT: AtomicU64 = AtomicU64::new(0);
static TSC_CALIBRATED: AtomicBool = AtomicBool::new(false);
/// PIT channel 2 failed to count once: don't wait for it again.
static PIT_DEAD: AtomicBool = AtomicBool::new(false);

/// Give up on PIT channel 2 after this many TSC cycles: 0.1 s at 5 GHz, and
/// the 10 ms window needs at most 50 M even there.
const PIT_GIVE_UP_TSC: u64 = 500_000_000;

#[inline(always)]
pub fn rdtsc() -> u64 {
    unsafe { core::arch::x86_64::_rdtsc() }
}

/// Run PIT channel 2 one-shot for `counts` input clocks, sampling `sample`
/// right after it starts and right after it fires. Returns `None` if OUT2
/// never rises (no working channel 2).
fn pit_ch2_measure(counts: u16, sample: &mut impl FnMut() -> u64) -> Option<(u64, u64)> {
    let mut gate: Port<u8> = Port::new(0x61); // ch2 gate + speaker control
    let mut cmd:  Port<u8> = Port::new(0x43);
    let mut ch2:  Port<u8> = Port::new(0x42);
    unsafe {
        let saved = gate.read();
        gate.write(saved & !0x03);          // gate low, speaker off
        cmd.write(0b1011_0000);             // ch2, lobyte/hibyte, mode 0, binary
        ch2.write(counts as u8);
        ch2.write((counts >> 8) as u8);
        gate.write((saved & !0x02) | 0x01); // gate high: start counting
        let start = sample();
        let t_start = rdtsc();
        // OUT2 (bit 5 of 0x61) rises at terminal count. Bounded by the TSC
        // so a machine without channel 2 can't hang boot (an iteration count
        // would be seconds of port reads).
        let t0 = rdtsc();
        while gate.read() & 0x20 == 0 {
            if rdtsc().wrapping_sub(t0) > PIT_GIVE_UP_TSC {
                gate.write(saved);
                return None;
            }
        }
        let end = sample();
        gate.write(saved);
        // 10 ms that passed in under 100 000 TSC cycles (10 MHz) didn't pass:
        // OUT2 was already high, no 8254 behind the port.
        if rdtsc().wrapping_sub(t_start) < 100_000 {
            return None;
        }
        Some((start, end))
    }
}

/// Measure how fast `sample` advances, in units per second, against PIT
/// channel 2. `sample` must be monotonic over the 10 ms window. Runs with
/// interrupts off. Returns `None` if channel 2 doesn't work.
pub(crate) fn calibrate(mut sample: impl FnMut() -> u64) -> Option<u64> {
    if !PIT_DEAD.load(Ordering::Relaxed) {
        let best = x86_64::instructions::interrupts::without_interrupts(|| {
            let mut best = u64::MAX;
            for _ in 0..CALIBRATION_ROUNDS {
                match pit_ch2_measure(CALIBRATION_PIT_COUNTS, &mut sample) {
                    Some((a, b)) => best = best.min(b.wrapping_sub(a)),
                    None => break,
                }
            }
            best
        });
        if best != u64::MAX && best != 0 {
            return Some((best as u128 * PIT_HZ as u128 / CALIBRATION_PIT_COUNTS as u128) as u64);
        }
        PIT_DEAD.store(true, Ordering::Relaxed);
        crate::serial_println!("[TIME] PIT channel 2 doesn't count (gated off?)");
    }
    // No PIT: against the TSC, if its rate is known some other way.
    if !is_calibrated() {
        return None;
    }
    let best = x86_64::instructions::interrupts::without_interrupts(|| {
        let mut best = u64::MAX;
        for _ in 0..CALIBRATION_ROUNDS {
            let window = us_to_tsc(10_000);
            let a = sample();
            let t0 = rdtsc();
            while rdtsc().wrapping_sub(t0) < window {
                core::hint::spin_loop();
            }
            best = best.min(sample().wrapping_sub(a));
        }
        best
    });
    (best != 0).then_some(best * 100)
}

/// The TSC's rate from CPUID: leaf 0x15 (TSC/crystal ratio and the crystal
/// frequency, Intel since Skylake), else leaf 0x16's base frequency in MHz.
fn tsc_hz_from_cpuid() -> Option<(u64, &'static str)> {
    use core::arch::x86_64::__cpuid;
    let max = unsafe { __cpuid(0) }.eax;
    if max >= 0x15 {
        let l = unsafe { __cpuid(0x15) };
        if l.eax != 0 && l.ebx != 0 && l.ecx != 0 {
            return Some((l.ecx as u64 * l.ebx as u64 / l.eax as u64, "CPUID 0x15"));
        }
    }
    if max >= 0x16 {
        let mhz = unsafe { __cpuid(0x16) }.eax & 0xFFFF;
        if mhz != 0 {
            return Some((mhz as u64 * 1_000_000, "CPUID 0x16"));
        }
    }
    None
}

fn set_tsc_hz(hz: u64, how: &str) {
    TSC_HZ.store(hz, Ordering::Relaxed);
    TSC_CALIBRATED.store(true, Ordering::Release);
    crate::serial_println!(
        "[TIME] TSC {}.{:03} MHz ({}, invariant: {})",
        hz / 1_000_000, (hz / 1_000) % 1_000, how, tsc_is_invariant()
    );
}

/// After paging is up: if neither the PIT nor CPUID gave the TSC's rate,
/// measure it against the HPET (from the ACPI HPET table). Only for the
/// rate: nothing else uses the HPET.
pub fn calibrate_late() {
    if is_calibrated() {
        return;
    }
    let Some(table) = crate::arch::smp::acpi_table(b"HPET") else {
        crate::serial_println!("[TIME] WARN: no PIT, no CPUID rate, no HPET: delays assume {} MHz", FALLBACK_TSC_HZ / 1_000_000);
        return;
    };
    let base = {
        let p = crate::mm::memory::get_phys_mem_offset().as_u64() + table + 44;
        unsafe { core::ptr::read_unaligned(p as *const u64) }
    };
    if base == 0 || base & 0xFFF != 0 {
        crate::serial_println!("[TIME] WARN: HPET table gives base {:#x}; not used", base);
        return;
    }
    // One uncached page, identity-mapped like the other MMIO.
    {
        use x86_64::structures::paging::{Mapper, Page, PageTableFlags, PhysFrame, Size4KiB};
        let mut mapper = unsafe { crate::mm::memory::get_mapper() };
        let mut frames = crate::mm::memory::GlobalFrameAllocator;
        let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::NO_CACHE;
        let page = Page::<Size4KiB>::containing_address(x86_64::VirtAddr::new(base));
        let frame = PhysFrame::<Size4KiB>::containing_address(x86_64::PhysAddr::new(base));
        if let Ok(f) = unsafe { mapper.map_to(page, frame, flags, &mut frames) } {
            f.flush();
        }
    }
    let reg = |off: u64| unsafe { core::ptr::read_volatile((base + off) as *const u64) };
    let caps = reg(0);
    let period_fs = caps >> 32;
    let wide = caps & (1 << 13) != 0;
    if period_fs == 0 || period_fs > 100_000_000 {
        crate::serial_println!("[TIME] WARN: HPET period {} fs is invalid; not used", period_fs);
        return;
    }
    // Start the main counter if the firmware left it stopped (no interrupt
    // routing changes: LEG_RT stays as it is).
    let conf = reg(0x10);
    if conf & 1 == 0 {
        unsafe { core::ptr::write_volatile((base + 0x10) as *mut u64, conf | 1) };
    }
    let counter = || if wide { reg(0xF0) } else { reg(0xF0) & 0xFFFF_FFFF };
    let window = 10_000_000_000_000u64 / period_fs; // 10 ms of HPET counts
    let mut best_tsc = u64::MAX;
    let mut counts = 0u64;
    for _ in 0..CALIBRATION_ROUNDS {
        x86_64::instructions::interrupts::without_interrupts(|| {
            let c0 = counter();
            let t0 = rdtsc();
            let mut c1 = c0;
            while (if wide { c1.wrapping_sub(c0) } else { (c1.wrapping_sub(c0)) & 0xFFFF_FFFF }) < window {
                c1 = counter();
            }
            let t1 = rdtsc();
            let elapsed = if wide { c1.wrapping_sub(c0) } else { c1.wrapping_sub(c0) & 0xFFFF_FFFF };
            if t1 - t0 < best_tsc {
                best_tsc = t1 - t0;
                counts = elapsed;
            }
        });
    }
    let hz = (best_tsc as u128 * 1_000_000_000_000_000 / (counts as u128 * period_fs as u128)) as u64;
    set_tsc_hz(hz, "HPET");
}

fn tsc_is_invariant() -> bool {
    use core::arch::x86_64::__cpuid;
    __cpuid(0x8000_0000).eax >= 0x8000_0007
        && __cpuid(0x8000_0007).edx & (1 << 8) != 0
}

/// Calibrate the TSC. Call once on the BSP, early in boot (needs only port
/// I/O): PIT channel 2, else CPUID. Without either, `calibrate_late` tries
/// the HPET once paging is up.
pub fn init() {
    TSC_BOOT.store(rdtsc(), Ordering::Relaxed);
    if option_env!("VEILOS_NO_PIT") == Some("1") {
        // Testing: behave as on a chipset with the 8254 gated off.
        PIT_DEAD.store(true, Ordering::Relaxed);
    }
    // A TSC outside 100 MHz..20 GHz means channel 2 isn't really counting:
    // without an 8254 (gated off, or QEMU's pit=off) port 0x61 can still
    // answer, with OUT2 already high, and the "10 ms" lasts no time at all.
    let pit = calibrate(rdtsc).filter(|&hz| (100_000_000..=20_000_000_000).contains(&hz));
    if pit.is_none() && !PIT_DEAD.swap(true, Ordering::Relaxed) {
        crate::serial_println!("[TIME] PIT channel 2 gives no plausible TSC rate: treated as absent");
    }
    if let Some(hz) = pit {
        set_tsc_hz(hz, "PIT");
    } else if let Some((hz, how)) = tsc_hz_from_cpuid() {
        set_tsc_hz(hz, how);
    } else {
        crate::serial_println!(
            "[TIME] no PIT channel 2 and no CPUID rate: assuming a {} MHz TSC until the HPET is reached",
            FALLBACK_TSC_HZ / 1_000_000
        );
    }
}

/// PIT channel 2 was found not to count.
pub fn pit_dead() -> bool {
    PIT_DEAD.load(Ordering::Relaxed)
}

pub fn tsc_hz() -> u64 {
    TSC_HZ.load(Ordering::Relaxed)
}

pub fn is_calibrated() -> bool {
    TSC_CALIBRATED.load(Ordering::Acquire)
}

#[inline]
fn us_to_tsc(us: u64) -> u64 {
    (us as u128 * tsc_hz() as u128 / 1_000_000) as u64
}

/// Microseconds since `init`.
pub fn now_us() -> u64 {
    let elapsed = rdtsc().wrapping_sub(TSC_BOOT.load(Ordering::Relaxed));
    (elapsed as u128 * 1_000_000 / tsc_hz() as u128) as u64
}

/// Milliseconds since `init`.
pub fn now_ms() -> u64 {
    now_us() / 1_000
}

/// Nanoseconds since `init`.
pub fn now_ns() -> u64 {
    let elapsed = rdtsc().wrapping_sub(TSC_BOOT.load(Ordering::Relaxed));
    (elapsed as u128 * 1_000_000_000 / tsc_hz() as u128) as u64
}

/// Busy-wait for `us` microseconds. Works with interrupts on or off.
pub fn delay_us(us: u64) {
    Deadline::after_us(us).wait();
}

/// Busy-wait for `ms` milliseconds. For long waits with interrupts on, prefer
/// `interrupts::SleepFuture` so the executor keeps running.
pub fn delay_ms(ms: u64) {
    delay_us(ms.saturating_mul(1_000));
}

/// A point in time measured on the TSC, for driver timeouts.
#[derive(Clone, Copy, Debug)]
pub struct Deadline {
    end_tsc: u64,
}

impl Deadline {
    pub fn after_us(us: u64) -> Self {
        Self { end_tsc: rdtsc().saturating_add(us_to_tsc(us)) }
    }

    pub fn after_ms(ms: u64) -> Self {
        Self::after_us(ms.saturating_mul(1_000))
    }

    pub fn expired(&self) -> bool {
        rdtsc() >= self.end_tsc
    }

    pub fn wait(&self) {
        while !self.expired() {
            core::hint::spin_loop();
        }
    }
}

/// Poll `done` until it returns true or `timeout_us` passes. Returns whether
/// `done` succeeded (it is checked once more after the deadline, so a slow
/// final poll can't be reported as a timeout).
pub fn spin_until(timeout_us: u64, mut done: impl FnMut() -> bool) -> bool {
    let deadline = Deadline::after_us(timeout_us);
    loop {
        if done() {
            return true;
        }
        if deadline.expired() {
            return done();
        }
        core::hint::spin_loop();
    }
}
