// -----| Sound: Intel HD Audio and the mixer |-----
//
// One output device, the first Intel High Definition Audio controller (PCI
// class 04:03) with a codec that has an output pin: what QEMU's `intel-hda`
// with `hda-output` or `hda-duplex` is, and what most PCs since 2004 have.
//
// The controller plays a ring of 48 kHz, 16-bit stereo samples in a loop
// (`RING_BYTES`, four buffer descriptors). The mixer keeps the part of the
// ring just ahead of the controller's read position (LPIB) filled
// (`LEAD_BYTES`, 80 ms). It runs from the timer interrupt every few
// milliseconds on whichever CPU gets there, so it never allocates or frees.
// Silence is written when nothing plays.
//
// Programs play through streams (`Stream`, made by VEIL_SOUND in
// nt_sound.rs): each has its own rate (1-192 kHz), 8 or 16 bits, mono or
// stereo, a queue of up to about 250 ms and a volume. The mixer converts each
// one to 48 kHz stereo (linear interpolation), adds them, applies the master
// volume (the Quick Settings slider) and clips. Everything is integer: the
// kernel doesn't use SSE.
//
// Codec commands go through the CORB/RIRB rings, polled. No interrupts.

use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use spin::Mutex;
use x86_64::instructions::interrupts::without_interrupts;

use crate::pci::PciDevice;

// =====================================================================
// -----| master volume |-----
// =====================================================================

static VOLUME: AtomicU8 = AtomicU8::new(50); // 0-100

pub fn get_volume() -> u8 {
    VOLUME.load(Ordering::Relaxed)
}

pub fn set_volume(vol: u8) {
    VOLUME.store(vol.min(100), Ordering::Relaxed);
}

/// The master volume as a Q16 gain: the square of the slider's fraction, so
/// the slider's middle sounds about half as loud.
fn master_gain() -> i64 {
    let v = get_volume() as i64;
    v * v * 65536 / 10_000
}

// =====================================================================
// -----| streams |-----
// =====================================================================

/// The mixer's output format.
pub const OUT_RATE: u32 = 48_000;

pub struct StreamState {
    pub rate: u32,
    pub channels: u16,
    pub bits: u16,
    /// Queued bytes: a fixed ring (`head`, `len`), never resized.
    buf: Vec<u8>,
    head: usize,
    len: usize,
    /// Bytes the mixer has taken from the queue since the stream opened.
    pub consumed: u64,
    /// Source frames per output frame, 32.32 fixed point.
    step: u64,
    /// How far between `cur` and `next` the next output frame is (0.32).
    frac: u64,
    cur: [i32; 2],
    next: [i32; 2],
    /// `next` holds a frame (false at the start and after a reset).
    primed: bool,
    pub paused: bool,
    /// Q16 gains, left and right (0x10000 = full).
    pub gain: [i64; 2],
}

impl StreamState {
    pub fn frame_bytes(&self) -> usize {
        self.channels as usize * self.bits as usize / 8
    }
    pub fn queued(&self) -> usize {
        self.len
    }
    pub fn free(&self) -> usize {
        self.buf.len() - self.len
    }

    /// Queue as much of `data` as fits, whole frames only. Returns the count.
    pub fn push(&mut self, data: &[u8]) -> usize {
        let fb = self.frame_bytes();
        let n = data.len().min(self.free()) / fb * fb;
        let cap = self.buf.len();
        let mut tail = (self.head + self.len) % cap;
        let mut done = 0;
        while done < n {
            let run = (n - done).min(cap - tail);
            self.buf[tail..tail + run].copy_from_slice(&data[done..done + run]);
            done += run;
            tail = (tail + run) % cap;
        }
        self.len += n;
        n
    }

    /// Drop everything queued (waveOutReset).
    pub fn flush(&mut self) {
        self.head = 0;
        self.len = 0;
        self.primed = false;
        self.frac = 0;
    }

    fn pop_frame(&mut self) -> Option<[i32; 2]> {
        let fb = self.frame_bytes();
        if self.len < fb {
            return None;
        }
        let cap = self.buf.len();
        let mut b = [0u8; 4];
        for (i, x) in b.iter_mut().enumerate().take(fb) {
            *x = self.buf[(self.head + i) % cap];
        }
        self.head = (self.head + fb) % cap;
        self.len -= fb;
        self.consumed += fb as u64;
        let s = |i: usize| -> i32 {
            if self.bits == 8 {
                (b[i] as i32 - 128) << 8
            } else {
                i16::from_le_bytes([b[2 * i], b[2 * i + 1]]) as i32
            }
        };
        Some(if self.channels == 1 { [s(0), s(0)] } else { [s(0), s(1)] })
    }

    /// Add up to `out.len() / 2` frames of this stream at 48 kHz into `out`.
    /// Stops early when the queue runs dry (the rest of it stays silent).
    fn mix_into(&mut self, out: &mut [i32]) {
        if self.paused {
            return;
        }
        if !self.primed {
            let Some(f) = self.pop_frame() else { return };
            self.cur = f;
            let Some(f) = self.pop_frame() else {
                // One frame only: play it as is.
                self.next = f;
                self.primed = true;
                return;
            };
            self.next = f;
            self.primed = true;
        }
        for o in out.chunks_exact_mut(2) {
            let t = (self.frac >> 16) as i64; // 0..65535
            for c in 0..2 {
                let a = self.cur[c] as i64;
                let b = self.next[c] as i64;
                let v = a + ((b - a) * t >> 16);
                o[c] += (v * self.gain[c] >> 16) as i32;
            }
            self.frac += self.step;
            while self.frac >= 1 << 32 {
                self.frac -= 1 << 32;
                match self.pop_frame() {
                    Some(f) => {
                        self.cur = self.next;
                        self.next = f;
                    }
                    None => {
                        // Starved: hold the position until more arrives.
                        self.cur = self.next;
                        self.primed = false;
                        self.frac = 0;
                        // `next` was consumed already; start over from the
                        // next frame written.
                        return;
                    }
                }
            }
        }
    }
}

pub struct Stream {
    pub state: Mutex<StreamState>,
}

/// Streams the mixer plays. Changed only with interrupts off (the mixer
/// runs in the timer interrupt and spins on these locks).
static STREAMS: Mutex<Vec<Arc<Stream>>> = Mutex::new(Vec::new());

/// Is there a device to play on?
pub fn present() -> bool {
    PRESENT.load(Ordering::Relaxed)
}
static PRESENT: AtomicBool = AtomicBool::new(false);

/// Lock a stream's state with interrupts off (the mixer may want it).
pub fn with_stream<R>(s: &Stream, f: impl FnOnce(&mut StreamState) -> R) -> R {
    without_interrupts(|| f(&mut s.state.lock()))
}

/// A new stream, added to the mixer. None for a format it doesn't take.
pub fn open_stream(rate: u32, channels: u16, bits: u16) -> Option<Arc<Stream>> {
    if !(1000..=192_000).contains(&rate) || !(1..=2).contains(&channels) || !(bits == 8 || bits == 16) {
        return None;
    }
    let frame = channels as usize * bits as usize / 8;
    // About 250 ms, at least 8 KiB.
    let cap = ((rate as usize / 4) * frame).max(8192) / frame * frame;
    let s = Arc::new(Stream {
        state: Mutex::new(StreamState {
            rate,
            channels,
            bits,
            buf: vec![0; cap],
            head: 0,
            len: 0,
            consumed: 0,
            step: ((rate as u64) << 32) / OUT_RATE as u64,
            frac: 0,
            cur: [0; 2],
            next: [0; 2],
            primed: false,
            paused: false,
            gain: [0x10000; 2],
        }),
    });
    without_interrupts(|| STREAMS.lock().push(s.clone()));
    Some(s)
}

/// Take a stream out of the mixer (its handle closed). Its memory is freed
/// here, never in the interrupt.
pub fn close_stream(s: &Arc<Stream>) {
    let removed = without_interrupts(|| {
        let mut list = STREAMS.lock();
        let i = list.iter().position(|x| Arc::ptr_eq(x, s));
        i.map(|i| list.swap_remove(i))
    });
    drop(removed);
}

// =====================================================================
// -----| HD Audio registers |-----
// =====================================================================

const GCAP: u64 = 0x00;
const GCTL: u64 = 0x08;
const STATESTS: u64 = 0x0E;
const CORBLBASE: u64 = 0x40;
const CORBUBASE: u64 = 0x44;
const CORBWP: u64 = 0x48;
const CORBRP: u64 = 0x4A;
const CORBCTL: u64 = 0x4C;
const CORBSIZE: u64 = 0x4E;
const RIRBLBASE: u64 = 0x50;
const RIRBUBASE: u64 = 0x54;
const RIRBWP: u64 = 0x58;
const RINTCNT: u64 = 0x5A;
const RIRBCTL: u64 = 0x5C;
const RIRBSTS: u64 = 0x5D;
const RIRBSIZE: u64 = 0x5E;
const DPLBASE: u64 = 0x70;
const DPUBASE: u64 = 0x74;
// Stream descriptor registers, from the descriptor's base.
const SD_CTL: u64 = 0x00;
const SD_STS: u64 = 0x03;
const SD_LPIB: u64 = 0x04;
const SD_CBL: u64 = 0x08;
const SD_LVI: u64 = 0x0C;
const SD_FMT: u64 = 0x12;
const SD_BDPL: u64 = 0x18;
const SD_BDPU: u64 = 0x1C;

/// 48 kHz, 16 bits, 2 channels.
const STREAM_FORMAT: u16 = 0x0011;
const STREAM_TAG: u32 = 1;

/// The ring the controller plays: 4 x 16 KiB, about 341 ms.
const BDL_ENTRIES: usize = 4;
const RING_BYTES: usize = BDL_ENTRIES * 16384;
/// How far ahead of the controller the mixer keeps the ring filled.
const LEAD_BYTES: usize = 80 * (OUT_RATE as usize / 1000) * 4;
/// Frames mixed per pass at most.
const CHUNK_FRAMES: usize = 1024;

struct Hda {
    base: u64,
    /// The output stream descriptor's registers.
    sd: u64,
    corb: *mut u32,
    rirb: *mut u64,
    corb_wp: u16,
    rirb_rp: u16,
    codec: u32,
    /// The sample ring (CPU view).
    ring: *mut i16,
    /// Where the mixer writes next, in bytes into the ring.
    write_pos: usize,
    acc: Box<[i32]>,
}

// The raw pointers are DMA memory owned by the driver for the kernel's life.
unsafe impl Send for Hda {}

static DEVICE: Mutex<Option<Hda>> = Mutex::new(None);

impl Hda {
    fn r8(&self, o: u64) -> u8 { unsafe { core::ptr::read_volatile((self.base + o) as *const u8) } }
    fn r16(&self, o: u64) -> u16 { unsafe { core::ptr::read_volatile((self.base + o) as *const u16) } }
    fn r32(&self, o: u64) -> u32 { unsafe { core::ptr::read_volatile((self.base + o) as *const u32) } }
    fn w8(&self, o: u64, v: u8) { unsafe { core::ptr::write_volatile((self.base + o) as *mut u8, v) } }
    fn w16(&self, o: u64, v: u16) { unsafe { core::ptr::write_volatile((self.base + o) as *mut u16, v) } }
    fn w32(&self, o: u64, v: u32) { unsafe { core::ptr::write_volatile((self.base + o) as *mut u32, v) } }

    /// Send one verb and wait for its answer.
    fn cmd(&mut self, verb: u32) -> Option<u32> {
        self.corb_wp = (self.corb_wp + 1) % 256;
        unsafe { core::ptr::write_volatile(self.corb.add(self.corb_wp as usize), verb) };
        self.w16(CORBWP, self.corb_wp);
        loop {
            let ok = crate::time::spin_until(20_000, || (self.r16(RIRBWP) & 0xFF) != self.rirb_rp);
            if !ok {
                crate::serial_println!("[AUDIO] verb {:#010x}: no answer", verb);
                return None;
            }
            self.rirb_rp = (self.rirb_rp + 1) % 256;
            let e = unsafe { core::ptr::read_volatile(self.rirb.add(self.rirb_rp as usize)) };
            // Clearing the response flag lets QEMU go on past RINTCNT answers.
            self.w8(RIRBSTS, 0x05);
            if e & (1 << 36) != 0 {
                continue; // unsolicited
            }
            return Some(e as u32);
        }
    }

    /// A 12-bit verb with an 8-bit payload.
    fn verb(&mut self, nid: u32, verb: u32, payload: u32) -> u32 {
        self.cmd((self.codec << 28) | (nid << 20) | (verb << 8) | (payload & 0xFF)).unwrap_or(0)
    }
    /// A 4-bit verb with a 16-bit payload.
    fn verb4(&mut self, nid: u32, verb: u32, payload: u32) -> u32 {
        self.cmd((self.codec << 28) | (nid << 20) | (verb << 16) | (payload & 0xFFFF)).unwrap_or(0)
    }
    fn param(&mut self, nid: u32, p: u32) -> u32 {
        self.verb(nid, 0xF00, p)
    }

    /// The nodes `nid` takes input from.
    fn connections(&mut self, nid: u32) -> Vec<u32> {
        let len = self.param(nid, 0x0E);
        let (count, long) = ((len & 0x7F) as u32, len & 0x80 != 0);
        let mut out = Vec::new();
        let per = if long { 2 } else { 4 };
        let mut i = 0;
        while i < count {
            let r = self.verb(nid, 0xF02, i);
            for k in 0..per {
                if i + k >= count {
                    break;
                }
                let e = if long { (r >> (16 * k)) & 0xFFFF } else { (r >> (8 * k)) & 0xFF };
                let range = if long { e & 0x8000 != 0 } else { e & 0x80 != 0 };
                let n = if long { e & 0x7FFF } else { e & 0x7F };
                if range {
                    if let Some(&prev) = out.last() {
                        for m in prev + 1..=n {
                            out.push(m);
                        }
                        continue;
                    }
                }
                out.push(n);
            }
            i += per;
        }
        out
    }

    /// Widget type: 0 output, 1 input, 2 mixer, 3 selector, 4 pin.
    fn wtype(&mut self, nid: u32) -> u32 {
        (self.param(nid, 0x09) >> 20) & 0xF
    }

    /// A path from `nid` back to an output converter: the nodes from `nid`
    /// to the converter, each with the connection index it uses.
    fn path_to_dac(&mut self, nid: u32, depth: u32, path: &mut Vec<(u32, u32)>) -> bool {
        if depth > 5 {
            return false;
        }
        let conns = self.connections(nid);
        for (i, &c) in conns.iter().enumerate() {
            let t = self.wtype(c);
            if t == 0 {
                path.push((nid, i as u32));
                path.push((c, 0));
                return true;
            }
            if t == 2 || t == 3 {
                path.push((nid, i as u32));
                if self.path_to_dac(c, depth + 1, path) {
                    return true;
                }
                path.pop();
            }
        }
        false
    }

    /// Power up, unmute and open the way from `path`'s pin to its converter.
    fn enable_path(&mut self, path: &[(u32, u32)]) {
        for (k, &(nid, idx)) in path.iter().enumerate() {
            self.verb(nid, 0x705, 0); // D0
            let caps = self.param(nid, 0x09);
            let t = (caps >> 20) & 0xF;
            if k + 1 < path.len() && (t == 3 || t == 4) {
                self.verb(nid, 0x701, idx); // connection select
            }
            // Output amp: 0 dB (its offset), unmuted, both channels.
            if caps & (1 << 2) != 0 {
                let ac = if caps & (1 << 3) != 0 { self.param(nid, 0x12) } else { self.param(1, 0x12) };
                let zero_db = ac & 0x7F;
                self.verb4(nid, 0x3, 0xB000 | zero_db);
            }
            // Input amp on the input used: unmuted, 0 dB.
            if caps & (1 << 1) != 0 && k + 1 < path.len() {
                let ac = if caps & (1 << 3) != 0 { self.param(nid, 0x0D) } else { self.param(1, 0x0D) };
                self.verb4(nid, 0x3, 0x7000 | (idx << 8) | (ac & 0x7F));
            }
        }
    }

    fn set_up_codec(&mut self) -> bool {
        let groups = self.param(0, 0x04);
        let (start, count) = ((groups >> 16) & 0xFF, groups & 0xFF);
        let Some(afg) = (start..start + count).find(|&n| self.param(n, 0x05) & 0xFF == 1) else {
            crate::serial_println!("[AUDIO] codec {}: no audio function group", self.codec);
            return false;
        };
        self.verb(afg, 0x705, 0); // D0
        let ws = self.param(afg, 0x04);
        let (wstart, wcount) = ((ws >> 16) & 0xFF, ws & 0xFF);
        let mut dacs: Vec<u32> = Vec::new();
        for nid in wstart..wstart + wcount {
            if self.wtype(nid) != 4 {
                continue;
            }
            let pincaps = self.param(nid, 0x0C);
            if pincaps & (1 << 4) == 0 {
                continue; // can't output
            }
            let config = self.verb(nid, 0xF1C, 0);
            if config >> 30 == 1 {
                continue; // nothing connected to it
            }
            let mut path = Vec::new();
            if !self.path_to_dac(nid, 0, &mut path) {
                continue;
            }
            self.enable_path(&path);
            let device = (config >> 20) & 0xF;
            let hp = if device == 2 && pincaps & (1 << 3) != 0 { 0x80 } else { 0 };
            self.verb(nid, 0x707, 0x40 | hp); // output enabled
            if pincaps & (1 << 16) != 0 {
                self.verb(nid, 0x70C, 0x02); // external amplifier on
            }
            let dac = path.last().unwrap().0;
            crate::serial_println!("[AUDIO] output pin {} (device {}) <- converter {}", nid, device, dac);
            if !dacs.contains(&dac) {
                dacs.push(dac);
            }
        }
        if dacs.is_empty() {
            crate::serial_println!("[AUDIO] codec {}: no output path", self.codec);
            return false;
        }
        for &dac in &dacs {
            self.verb4(dac, 0x2, STREAM_FORMAT as u32);
            self.verb(dac, 0x706, STREAM_TAG << 4);
        }
        true
    }

    /// Fill the ring up to `LEAD_BYTES` ahead of the controller.
    fn mix(&mut self) {
        let lpib = self.r32(self.sd + SD_LPIB) as usize % RING_BYTES;
        let mut ahead = (self.write_pos + RING_BYTES - lpib) % RING_BYTES;
        if ahead > LEAD_BYTES + CHUNK_FRAMES * 4 {
            // The controller passed us (a long stall): start again just ahead.
            self.write_pos = (lpib + 64 * 4) % RING_BYTES;
            ahead = 64 * 4;
        }
        let mut need = LEAD_BYTES.saturating_sub(ahead) / 4;
        if need == 0 {
            return;
        }
        let master = master_gain();
        let streams = STREAMS.lock();
        while need > 0 {
            let frames = need.min(CHUNK_FRAMES);
            let acc = &mut self.acc[..frames * 2];
            acc.fill(0);
            for s in streams.iter() {
                s.state.lock().mix_into(acc);
            }
            let mut pos = self.write_pos / 2; // in samples
            for &v in acc.iter() {
                let v = ((v as i64 * master) >> 16).clamp(-32768, 32767) as i16;
                unsafe { core::ptr::write_volatile(self.ring.add(pos), v) };
                pos = (pos + 1) % (RING_BYTES / 2);
            }
            self.write_pos = pos * 2;
            need -= frames;
        }
    }
}

static LAST_MIX_MS: AtomicU64 = AtomicU64::new(0);

/// From the timer interrupt: top the ring up every 4 ms or so.
pub fn tick(now_ms: u64) {
    if !present() {
        return;
    }
    let last = LAST_MIX_MS.load(Ordering::Relaxed);
    if now_ms.wrapping_sub(last) < 4 {
        return;
    }
    if LAST_MIX_MS.compare_exchange(last, now_ms, Ordering::Relaxed, Ordering::Relaxed).is_err() {
        return;
    }
    if let Some(mut dev) = DEVICE.try_lock() {
        if let Some(h) = dev.as_mut() {
            h.mix();
        }
    }
}

// =====================================================================
// -----| the driver |-----
// =====================================================================

/// Intel HD Audio controllers (class 04:03). Bound by `hal::bind`.
pub const DRIVER: crate::hal::Driver = crate::hal::Driver {
    name: "hda",
    class: crate::hal::DeviceClass::Multimedia,
    matches: |dev| dev.class == 0x04 && dev.subclass == 0x03,
    probe,
    remove: None,
};

fn map_mmio(phys: u64, bytes: u64) {
    use x86_64::structures::paging::{Mapper, Page, PageTableFlags, PhysFrame, Size4KiB};
    use x86_64::{PhysAddr, VirtAddr};
    let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::NO_CACHE;
    let mut mapper = unsafe { crate::memory::get_mapper() };
    let mut fa = crate::memory::GlobalFrameAllocator;
    let mut a = phys & !0xFFF;
    while a < phys + bytes {
        let frame = PhysFrame::<Size4KiB>::containing_address(PhysAddr::new(a));
        let page = Page::<Size4KiB>::containing_address(VirtAddr::new(a));
        if let Ok(tlb) = unsafe { mapper.map_to(page, frame, flags, &mut fa) } {
            tlb.flush();
        }
        a += 4096;
    }
}

fn probe(dev: &PciDevice) -> Result<(), alloc::string::String> {
    use alloc::string::String;
    if DEVICE.lock().is_some() {
        return Err(String::from("only one sound device is used"));
    }
    let base = dev.bar(0).mem_base().ok_or_else(|| String::from("no BAR0"))?;
    dev.enable_for_driver();
    map_mmio(base, 0x4000);

    // CORB (1 KiB), RIRB (2 KiB), BDL (256 B) in one page; the ring apart.
    let ctl = crate::memory::DmaRegion::new(4096).ok_or_else(|| String::from("no DMA memory"))?;
    let ring = crate::memory::DmaRegion::new(RING_BYTES).ok_or_else(|| String::from("no DMA memory"))?;
    let (ctl_phys, ctl_ptr) = ctl.leak();
    let (ring_phys, ring_ptr) = ring.leak();
    let corb_phys = ctl_phys;
    let rirb_phys = ctl_phys + 1024;
    let bdl_phys = ctl_phys + 3072;

    let mut h = Hda {
        base,
        sd: 0,
        corb: ctl_ptr as *mut u32,
        rirb: unsafe { ctl_ptr.add(1024) } as *mut u64,
        corb_wp: 0,
        rirb_rp: 0,
        codec: 0,
        ring: ring_ptr as *mut i16,
        write_pos: 0,
        acc: vec![0i32; CHUNK_FRAMES * 2].into_boxed_slice(),
    };

    // Reset the controller.
    h.w32(GCTL, h.r32(GCTL) & !1);
    crate::time::spin_until(100_000, || h.r32(GCTL) & 1 == 0);
    h.w32(GCTL, h.r32(GCTL) | 1);
    if !crate::time::spin_until(100_000, || h.r32(GCTL) & 1 == 1) {
        return Err(String::from("controller stays in reset"));
    }
    crate::time::delay_ms(2); // codecs announce themselves within 521 us
    let codecs = h.r16(STATESTS);
    if codecs == 0 {
        return Err(String::from("no codec"));
    }
    let gcap = h.r16(GCAP);
    let (iss, oss) = ((gcap >> 8) & 0xF, (gcap >> 12) & 0xF);
    if oss == 0 {
        return Err(String::from("no output streams"));
    }
    h.sd = 0x80 + 0x20 * iss as u64;

    // Command rings: 256 entries each.
    h.w8(CORBCTL, 0);
    h.w8(RIRBCTL, 0);
    crate::time::spin_until(10_000, || h.r8(CORBCTL) & 2 == 0 && h.r8(RIRBCTL) & 2 == 0);
    // 256 entries; the size registers are read-only where that's the only size.
    if h.r8(CORBSIZE) & 0xF0 != 0x40 {
        h.w8(CORBSIZE, 0x02);
    }
    h.w32(CORBLBASE, corb_phys as u32);
    h.w32(CORBUBASE, (corb_phys >> 32) as u32);
    h.w16(CORBRP, 0x8000);
    crate::time::spin_until(1_000, || h.r16(CORBRP) & 0x8000 != 0);
    h.w16(CORBRP, 0);
    crate::time::spin_until(1_000, || h.r16(CORBRP) & 0x8000 == 0);
    h.w16(CORBWP, 0);
    if h.r8(RIRBSIZE) & 0xF0 != 0x40 {
        h.w8(RIRBSIZE, 0x02);
    }
    h.w32(RIRBLBASE, rirb_phys as u32);
    h.w32(RIRBUBASE, (rirb_phys >> 32) as u32);
    h.w16(RIRBWP, 0x8000);
    h.w16(RINTCNT, 0xFF);
    h.w32(DPLBASE, 0);
    h.w32(DPUBASE, 0);
    h.w8(CORBCTL, 0x02);
    h.w8(RIRBCTL, 0x02);

    // The first codec with an output.
    let mut found = false;
    for c in 0..15u32 {
        if codecs & (1 << c) == 0 {
            continue;
        }
        h.codec = c;
        let vendor = h.param(0, 0x00);
        crate::serial_println!("[AUDIO] codec {}: vendor {:#010x}", c, vendor);
        if h.set_up_codec() {
            found = true;
            break;
        }
    }
    if !found {
        return Err(String::from("no codec with an output"));
    }

    // The output stream: reset, then the ring as four buffers.
    let sd = h.sd;
    h.w8(sd + SD_CTL, 0x01);
    crate::time::spin_until(10_000, || h.r8(sd + SD_CTL) & 1 == 1);
    h.w8(sd + SD_CTL, 0x00);
    crate::time::spin_until(10_000, || h.r8(sd + SD_CTL) & 1 == 0);
    let bdl = unsafe { ctl_ptr.add(3072) } as *mut u32;
    let part = RING_BYTES / BDL_ENTRIES;
    for i in 0..BDL_ENTRIES {
        let a = ring_phys + (i * part) as u64;
        unsafe {
            core::ptr::write_volatile(bdl.add(i * 4), a as u32);
            core::ptr::write_volatile(bdl.add(i * 4 + 1), (a >> 32) as u32);
            core::ptr::write_volatile(bdl.add(i * 4 + 2), part as u32);
            core::ptr::write_volatile(bdl.add(i * 4 + 3), 0);
        }
    }
    h.w32(sd + SD_CBL, RING_BYTES as u32);
    h.w16(sd + SD_LVI, (BDL_ENTRIES - 1) as u16);
    h.w16(sd + SD_FMT, STREAM_FORMAT);
    h.w32(sd + SD_BDPL, bdl_phys as u32);
    h.w32(sd + SD_BDPU, (bdl_phys >> 32) as u32);
    h.w8(sd + SD_STS, 0x1C);
    h.w8(sd + 0x02, (STREAM_TAG << 4) as u8);
    h.mix(); // silence ahead before it starts
    h.w8(sd + SD_CTL, 0x02); // run

    crate::serial_println!(
        "[AUDIO] HD Audio at {:#x}: {} in / {} out streams, playing 48 kHz 16-bit stereo",
        base, iss, oss
    );
    *DEVICE.lock() = Some(h);
    PRESENT.store(true, Ordering::Relaxed);
    Ok(())
}

/// For the SOUND shell command: device state and what's playing.
pub fn describe() -> alloc::string::String {
    use alloc::format;
    if !present() {
        return alloc::string::String::from("no sound device");
    }
    let lpib = without_interrupts(|| DEVICE.lock().as_ref().map(|h| h.r32(h.sd + SD_LPIB)).unwrap_or(0));
    let n = without_interrupts(|| STREAMS.lock().len());
    format!("HD Audio, volume {}%, {} stream(s), position {}", get_volume(), n, lpib)
}
