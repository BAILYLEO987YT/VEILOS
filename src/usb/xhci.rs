// =============================================================================
// VEILOS: xHCI host controller (USB 1.1/2.0/3.x)
// =============================================================================
//
// One `Controller` per xHCI function. Start-up: take it from the firmware
// (USB legacy support capability: the BIOS keeps emulating a PS/2 keyboard
// through SMIs until asked), reset it, give it a device context array,
// scratchpad pages, a command ring and one event ring, and run it. Then each
// root port with something connected is reset, given a slot, addressed and
// configured (`enumerate`).
//
// All DMA memory comes from memory::DmaRegion (below 4 GiB, contiguous,
// zeroed). Rings are one page: 255 TRBs and a link TRB back to the start.
// Nothing uses interrupts: `process_events` reads the event ring when a
// command or transfer waits for its completion, and from the BSP's timer
// tick (`poll_all`) for keyboard and mouse reports.

use alloc::{format, string::String, sync::Arc, vec, vec::Vec};
use spin::Mutex;

use super::{Endpoint, Interface, Setup};

const RING_TRBS: usize = 256;
const EVENT_TRBS: usize = 256;

// TRB types.
const TRB_NORMAL: u32 = 1;
const TRB_SETUP: u32 = 2;
const TRB_DATA: u32 = 3;
const TRB_STATUS: u32 = 4;
const TRB_LINK: u32 = 6;
const TRB_ENABLE_SLOT: u32 = 9;
const TRB_ADDRESS_DEVICE: u32 = 11;
const TRB_CONFIGURE_EP: u32 = 12;
const TRB_EVALUATE_CTX: u32 = 13;
const TRB_RESET_EP: u32 = 14;
const TRB_SET_TR_DEQUEUE: u32 = 16;
const EV_TRANSFER: u32 = 32;
const EV_COMMAND: u32 = 33;

// TRB control bits.
const TRB_CYCLE: u32 = 1 << 0;
const TRB_TC: u32 = 1 << 1; // link: toggle cycle
const TRB_ISP: u32 = 1 << 2;
const TRB_CH: u32 = 1 << 4;
const TRB_IOC: u32 = 1 << 5;
const TRB_IDT: u32 = 1 << 6;

// Completion codes.
pub const CC_SUCCESS: u8 = 1;
pub const CC_STALL: u8 = 6;
pub const CC_SHORT_PACKET: u8 = 13;

// PORTSC bits.
const PORT_CCS: u32 = 1 << 0;
const PORT_PED: u32 = 1 << 1;
const PORT_PR: u32 = 1 << 4;
const PORT_PP: u32 = 1 << 9;
const PORT_PRC: u32 = 1 << 21;
/// Write-1-to-clear change bits, PED (writing 1 disables the port), PR and
/// LWS: what a read-modify-write of PORTSC must not write back.
const PORT_WRITE_MASK: u32 = !(PORT_PED | PORT_PR | (1 << 16) | (0x7F << 17));

fn dma(bytes: usize) -> Option<(u64, *mut u8)> {
    crate::memory::DmaRegion::new(bytes).map(|r| r.leak())
}

// -----| rings |-----

/// A producer ring (the command ring, or one endpoint's transfer ring).
struct Ring {
    phys: u64,
    trbs: *mut u32,
    enqueue: usize,
    cycle: bool,
}

impl Ring {
    fn new() -> Option<Self> {
        let (phys, virt) = dma(RING_TRBS * 16)?;
        let ring = Self { phys, trbs: virt as *mut u32, enqueue: 0, cycle: true };
        // The last TRB links back to the first and toggles the cycle state.
        ring.write(RING_TRBS - 1, phys, 0, TRB_LINK << 10 | TRB_TC);
        Some(ring)
    }

    fn write(&self, i: usize, param: u64, status: u32, control: u32) {
        unsafe {
            let t = self.trbs.add(i * 4);
            t.write_volatile(param as u32);
            t.add(1).write_volatile((param >> 32) as u32);
            t.add(2).write_volatile(status);
            core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
            // The control word (with the cycle bit) last: it hands the TRB over.
            t.add(3).write_volatile(control);
        }
    }

    /// Queue a TRB; returns its physical address.
    fn push(&mut self, param: u64, status: u32, control: u32) -> u64 {
        let at = self.phys + (self.enqueue * 16) as u64;
        let control = (control & !TRB_CYCLE) | self.cycle as u32;
        self.write(self.enqueue, param, status, control);
        self.enqueue += 1;
        if self.enqueue == RING_TRBS - 1 {
            // Hand the link TRB over too (chained if this TD continues).
            let chain = control & TRB_CH;
            self.write(RING_TRBS - 1, self.phys, 0, TRB_LINK << 10 | TRB_TC | chain | self.cycle as u32);
            self.enqueue = 0;
            self.cycle = !self.cycle;
        }
        at
    }

    /// Where the next TRB goes, with the cycle state (for Set TR Dequeue).
    fn dequeue_pointer(&self) -> u64 {
        self.phys + (self.enqueue * 16) as u64 | self.cycle as u64
    }
}

/// The consumer side of the controller's event ring.
struct EventRing {
    phys: u64,
    trbs: *mut u32,
    dequeue: usize,
    cycle: bool,
}

impl EventRing {
    fn next(&mut self) -> Option<[u32; 4]> {
        let t = unsafe { self.trbs.add(self.dequeue * 4) };
        let control = unsafe { t.add(3).read_volatile() };
        if (control & 1 != 0) != self.cycle {
            return None;
        }
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        let ev = unsafe { [t.read_volatile(), t.add(1).read_volatile(), t.add(2).read_volatile(), control] };
        self.dequeue += 1;
        if self.dequeue == EVENT_TRBS {
            self.dequeue = 0;
            self.cycle = !self.cycle;
        }
        Some(ev)
    }
}

// -----| devices |-----

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HidKind {
    Keyboard,
    Mouse,
}

/// An interrupt-IN endpoint delivering boot-protocol reports.
struct HidEndpoint {
    dci: u8,
    kind: HidKind,
    buf_phys: u64,
    buf: *mut u8,
    len: usize,
    /// The keyboard's previous report (for key-up/key-down).
    last: [u8; 8],
}

/// What a finished transfer reported: completion code, bytes not
/// transferred, and the TRB the event names.
#[derive(Clone, Copy)]
struct Done {
    cc: u8,
    remaining: u32,
    trb: u64,
}

struct Slot {
    port: u8,
    speed: u8,
    dev_ctx: *mut u8,
    in_phys: u64,
    in_ctx: *mut u8,
    rings: [Option<Ring>; 32],
    done: [Option<Done>; 32],
    hid: Vec<HidEndpoint>,
}

/// A mass-storage interface found while enumerating; attached once the
/// controller is unlocked (mounting it reads through the controller).
pub struct MscFound {
    pub slot: u8,
    pub interface: u8,
    pub bulk_in: Endpoint,
    pub bulk_out: Endpoint,
}

/// Where a device hangs: which root port, through which hubs, at what speed.
#[derive(Clone, Copy)]
struct Attach {
    root_port: u8,
    /// xHCI route string: the hub port at each tier below the root, 4 bits each.
    route: u32,
    /// Hubs between the root port and this device.
    tier: u8,
    /// xHCI speed: 1 full, 2 low, 3 high, 4+ SuperSpeed.
    speed: u8,
    /// A low/full-speed device behind a high-speed hub: that hub's slot and
    /// port (its transaction translator).
    tt: Option<(u8, u8)>,
}

impl Attach {
    /// "3" for root port 3, "3.2.1" behind hubs.
    fn path(&self) -> String {
        let mut s = format!("{}", self.root_port);
        for t in 0..self.tier {
            s += &format!(".{}", (self.route >> (4 * t)) & 0xF);
        }
        s
    }
}

// Hub class requests (USB 2.0 11.24, USB 3.2 10.14).
const HUB_PORT_RESET: u16 = 4;
const HUB_PORT_POWER: u16 = 8;
const HUB_C_PORT_CONNECTION: u16 = 16;
const HUB_C_PORT_RESET: u16 = 20;
const HUB_C_PORT_LINK_STATE: u16 = 25;
const HUB_C_BH_PORT_RESET: u16 = 29;

pub struct Controller {
    pub name: String,
    op: u64,
    rt: u64,
    db: u64,
    ctx_size: usize,
    max_slots: u8,
    max_ports: u8,
    dcbaa: *mut u64,
    cmd: Ring,
    events: EventRing,
    cmd_done: Option<Done>,
    slots: Vec<Option<Slot>>,
    /// A page for control-transfer data.
    ctl_phys: u64,
    ctl: *mut u8,
}

// Its raw pointers are DMA memory and MMIO owned by this controller, used
// under its lock.
unsafe impl Send for Controller {}

static CONTROLLERS: Mutex<Vec<Arc<Mutex<Controller>>>> = Mutex::new(Vec::new());

fn r32(a: u64) -> u32 {
    unsafe { (a as *const u32).read_volatile() }
}
fn w32(a: u64, v: u32) {
    unsafe { (a as *mut u32).write_volatile(v) }
}
fn w64(a: u64, v: u64) {
    w32(a, v as u32);
    w32(a + 4, (v >> 32) as u32);
}

/// Identity-map an MMIO range, uncached, as the other drivers do.
fn map_mmio(phys: u64, size: u64) {
    use x86_64::structures::paging::{Mapper, Page, PageTableFlags, PhysFrame, Size4KiB};
    let mut mapper = unsafe { crate::memory::get_mapper() };
    let mut frames = crate::memory::GlobalFrameAllocator;
    let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::NO_CACHE;
    for off in (0..size).step_by(4096) {
        let page = Page::<Size4KiB>::containing_address(x86_64::VirtAddr::new(phys + off));
        let frame = PhysFrame::<Size4KiB>::containing_address(x86_64::PhysAddr::new(phys + off));
        if let Ok(f) = unsafe { mapper.map_to(page, frame, flags, &mut frames) } {
            f.flush();
        }
    }
}

/// HAL probe: start the controller and every device on its root ports.
pub fn probe(dev: &crate::pci::PciDevice) -> Result<(), String> {
    let bar = dev.bar(0);
    let base = bar.mem_base().ok_or_else(|| String::from("no memory BAR0"))?;
    dev.enable_for_driver();
    map_mmio(base, (bar.size as u64).max(0x10000));
    let name = format!("{:02x}:{:02x}.{}", dev.bus, dev.device, dev.function);
    let mut c = Controller::start(base, name)?;
    let found = c.scan_ports();
    let c = Arc::new(Mutex::new(c));
    CONTROLLERS.lock().push(c.clone());
    for m in found {
        super::msc::attach(c.clone(), m);
    }
    Ok(())
}

/// From the BSP's timer tick: keyboard and mouse reports, on every
/// controller not busy with a transfer.
pub fn poll_all() {
    let Some(list) = CONTROLLERS.try_lock() else { return };
    for c in list.iter() {
        if let Some(mut c) = c.try_lock() {
            c.process_events();
        }
    }
}

impl Controller {
    fn start(base: u64, name: String) -> Result<Self, String> {
        let caplength = r32(base) & 0xFF;
        let hcs1 = r32(base + 4);
        let hcs2 = r32(base + 8);
        let hcc1 = r32(base + 0x10);
        let op = base + caplength as u64;
        let rt = base + (r32(base + 0x18) & !0x1F) as u64;
        let db = base + (r32(base + 0x14) & !0x3) as u64;
        let max_slots = (hcs1 & 0xFF) as u8;
        let max_ports = (hcs1 >> 24) as u8;
        let ctx_size = if hcc1 & (1 << 2) != 0 { 64 } else { 32 };
        crate::serial_println!(
            "[XHCI] {}: version {:x}.{:02x}, {} slots, {} ports, {}-byte contexts",
            name, r32(base) >> 24, (r32(base) >> 16) & 0xFF, max_slots, max_ports, ctx_size
        );

        take_from_firmware(base, hcc1);

        // Stop, then reset.
        w32(op, r32(op) & !1);
        if !crate::time::spin_until(100_000, || r32(op + 4) & 1 != 0) {
            return Err(String::from("didn't halt"));
        }
        w32(op, r32(op) | 2);
        if !crate::time::spin_until(1_000_000, || r32(op) & 2 == 0 && r32(op + 4) & (1 << 11) == 0) {
            return Err(String::from("reset didn't finish"));
        }

        w32(op + 0x38, max_slots as u32); // CONFIG: MaxSlotsEn

        // Device context base address array, with scratchpad pages in slot 0.
        let (dcbaa_phys, dcbaa) = dma(2048).ok_or_else(|| String::from("no DMA memory"))?;
        let dcbaa = dcbaa as *mut u64;
        let scratch = ((hcs2 >> 21) & 0x1F) << 5 | (hcs2 >> 27);
        if scratch > 0 {
            let (arr_phys, arr) = dma(scratch as usize * 8).ok_or_else(|| String::from("no DMA memory"))?;
            for i in 0..scratch as usize {
                let (p, _) = dma(4096).ok_or_else(|| String::from("no DMA memory"))?;
                unsafe { (arr as *mut u64).add(i).write_volatile(p) };
            }
            unsafe { dcbaa.write_volatile(arr_phys) };
        }
        w64(op + 0x30, dcbaa_phys);

        let cmd = Ring::new().ok_or_else(|| String::from("no DMA memory"))?;
        w64(op + 0x18, cmd.phys | 1); // CRCR, ring cycle state 1

        // One event ring segment for interrupter 0 (polled: IMAN.IE stays off).
        let (ev_phys, ev) = dma(EVENT_TRBS * 16).ok_or_else(|| String::from("no DMA memory"))?;
        let (erst_phys, erst) = dma(64).ok_or_else(|| String::from("no DMA memory"))?;
        unsafe {
            (erst as *mut u64).write_volatile(ev_phys);
            (erst as *mut u32).add(2).write_volatile(EVENT_TRBS as u32);
        }
        let ir = rt + 0x20;
        w32(ir + 8, 1); // ERSTSZ
        w64(ir + 0x18, ev_phys); // ERDP
        w64(ir + 0x10, erst_phys); // ERSTBA (last: it starts the event ring)

        let (ctl_phys, ctl) = dma(4096).ok_or_else(|| String::from("no DMA memory"))?;

        w32(op, r32(op) | 1); // Run
        if !crate::time::spin_until(100_000, || r32(op + 4) & 1 == 0) {
            return Err(String::from("didn't start"));
        }

        let mut slots = Vec::new();
        slots.resize_with(max_slots as usize + 1, || None);
        Ok(Self {
            name,
            op,
            rt,
            db,
            ctx_size,
            max_slots,
            max_ports,
            dcbaa,
            cmd,
            events: EventRing { phys: ev_phys, trbs: ev as *mut u32, dequeue: 0, cycle: true },
            cmd_done: None,
            slots,
            ctl_phys,
            ctl,
        })
    }

    // -----| events |-----

    /// Read every event the controller has posted: command completions and
    /// transfer completions are recorded for whoever waits on them;
    /// keyboard and mouse reports are handled and their transfer queued
    /// again.
    pub fn process_events(&mut self) {
        let mut any = false;
        while let Some(ev) = self.events.next() {
            any = true;
            let param = ev[0] as u64 | (ev[1] as u64) << 32;
            let cc = (ev[2] >> 24) as u8;
            let kind = (ev[3] >> 10) & 0x3F;
            let slot = (ev[3] >> 24) as usize;
            match kind {
                // For a command, `remaining` holds the slot id it names.
                EV_COMMAND => self.cmd_done = Some(Done { cc, remaining: slot as u32, trb: param }),
                EV_TRANSFER => {
                    let dci = ((ev[3] >> 16) & 0x1F) as usize;
                    let remaining = ev[2] & 0xFF_FFFF;
                    self.transfer_done(slot, dci, Done { cc, remaining, trb: param });
                }
                _ => {} // port status changes: no hot-plug yet
            }
        }
        if any {
            let ir = self.rt + 0x20;
            w64(ir + 0x18, self.events.phys + (self.events.dequeue * 16) as u64 | 1 << 3);
        }
    }

    fn transfer_done(&mut self, slot: usize, dci: usize, done: Done) {
        let db = self.db;
        let Some(Some(s)) = self.slots.get_mut(slot) else { return };
        if let Some(h) = s.hid.iter_mut().find(|h| h.dci as usize == dci) {
            if matches!(done.cc, CC_SUCCESS | CC_SHORT_PACKET) {
                let len = h.len.saturating_sub(done.remaining as usize);
                let report = unsafe { core::slice::from_raw_parts(h.buf, len) };
                match h.kind {
                    HidKind::Keyboard => super::hid::keyboard_report(report, &mut h.last),
                    HidKind::Mouse => super::hid::mouse_report(report),
                }
            }
            if let Some(ring) = s.rings[dci].as_mut() {
                ring.push(h.buf_phys, h.len as u32, TRB_NORMAL << 10 | TRB_IOC | TRB_ISP);
                w32(db + 4 * slot as u64, dci as u32);
            }
            return;
        }
        s.done[dci] = Some(done);
    }

    fn ring_command(&mut self, param: u64, status: u32, control: u32) -> Result<Done, String> {
        self.cmd_done = None;
        let trb = self.cmd.push(param, status, control);
        w32(self.db, 0);
        let mut result = None;
        crate::time::spin_until(1_000_000, || {
            self.process_events();
            if let Some(d) = self.cmd_done.filter(|d| d.trb == trb) {
                result = Some(d);
            }
            result.is_some()
        });
        result.ok_or_else(|| format!("command {} timed out", (control >> 10) & 0x3F))
    }

    /// Run a command; returns the slot id its completion names.
    fn command(&mut self, param: u64, control: u32) -> Result<u8, String> {
        let d = self.ring_command(param, 0, control)?;
        if d.cc != CC_SUCCESS {
            return Err(format!("command {} failed: code {}", (control >> 10) & 0x3F, d.cc));
        }
        Ok(d.remaining as u8)
    }

    /// Wait for the transfer ending at `last_trb` on (slot, dci).
    fn wait_transfer(&mut self, slot: u8, dci: u8, timeout_us: u64) -> Option<Done> {
        let mut out = None;
        crate::time::spin_until(timeout_us, || {
            self.process_events();
            if let Some(Some(s)) = self.slots.get_mut(slot as usize) {
                out = s.done[dci as usize].take();
            }
            out.is_some()
        });
        out
    }

    // -----| contexts |-----

    fn in_ctx(&self, slot: u8, index: usize) -> *mut u32 {
        let s = self.slots[slot as usize].as_ref().unwrap();
        unsafe { s.in_ctx.add(index * self.ctx_size) as *mut u32 }
    }

    fn clear_input(&self, slot: u8) {
        let s = self.slots[slot as usize].as_ref().unwrap();
        unsafe { core::ptr::write_bytes(s.in_ctx, 0, 33 * self.ctx_size) };
    }

    /// Fill endpoint `dci`'s input context.
    fn set_ep(&self, slot: u8, dci: u8, ep_type: u32, max_packet: u32, interval: u32, ring: u64) {
        let e = self.in_ctx(slot, 1 + dci as usize);
        unsafe {
            e.write_volatile(interval << 16);
            e.add(1).write_volatile(3 << 1 | ep_type << 3 | max_packet << 16); // CErr 3
            e.add(2).write_volatile(ring as u32 | 1); // DCS 1
            e.add(3).write_volatile((ring >> 32) as u32);
            let avg = if ep_type == 4 { 8 } else { max_packet };
            e.add(4).write_volatile(avg | max_packet << 16);
        }
    }

    // -----| ports and enumeration |-----

    /// Reset each root port with a device and set the device up. Returns
    /// the mass-storage devices, for msc::attach once unlocked.
    fn scan_ports(&mut self) -> Vec<MscFound> {
        // USB 3 links train on their own after the controller starts.
        crate::time::delay_ms(100);
        let mut found = Vec::new();
        for port in 1..=self.max_ports {
            let portsc = self.op + 0x400 + 0x10 * (port as u64 - 1);
            let sc = r32(portsc);
            if sc & PORT_PP == 0 {
                w32(portsc, (sc & PORT_WRITE_MASK) | PORT_PP);
                crate::time::delay_ms(20);
            }
            if r32(portsc) & PORT_CCS == 0 {
                continue;
            }
            if r32(portsc) & PORT_PED == 0 {
                w32(portsc, (r32(portsc) & PORT_WRITE_MASK) | PORT_PR);
                crate::time::spin_until(500_000, || r32(portsc) & PORT_PRC != 0);
                w32(portsc, (r32(portsc) & PORT_WRITE_MASK) | PORT_PRC);
                crate::time::delay_ms(10); // recovery after reset
            }
            let sc = r32(portsc);
            if sc & PORT_PED == 0 {
                crate::serial_println!("[XHCI] {} port {}: device didn't enable (PORTSC {:#x})", self.name, port, sc);
                continue;
            }
            let speed = ((sc >> 10) & 0xF) as u8;
            let at = Attach { root_port: port, route: 0, tier: 0, speed, tt: None };
            if let Err(e) = self.enumerate(at, &mut found) {
                crate::serial_println!("[XHCI] {} port {}: {}", self.name, port, e);
            }
        }
        found
    }

    /// Set up the device at `at`: a hub's own devices too. Mass-storage
    /// devices found go to `found`.
    fn enumerate(&mut self, at: Attach, found: &mut Vec<MscFound>) -> Result<(), String> {
        let (port, speed) = (at.root_port, at.speed);
        let slot = self.command(0, TRB_ENABLE_SLOT << 10)?;
        if slot == 0 || slot > self.max_slots {
            return Err(format!("bad slot id {}", slot));
        }
        let (dev_phys, dev_ctx) = dma(4096).ok_or("no DMA memory")?;
        let (in_phys, in_ctx) = dma(4096).ok_or("no DMA memory")?;
        unsafe { self.dcbaa.add(slot as usize).write_volatile(dev_phys) };
        let ep0 = Ring::new().ok_or("no DMA memory")?;
        let ep0_phys = ep0.phys;
        let mut rings: [Option<Ring>; 32] = Default::default();
        rings[1] = Some(ep0);
        self.slots[slot as usize] = Some(Slot {
            port, speed, dev_ctx, in_phys, in_ctx, rings, done: [None; 32], hid: Vec::new(),
        });

        // Address Device: slot context and EP0.
        let mut mps0: u32 = match speed { 1 | 2 => 8, 3 => 64, _ => 512 };
        self.clear_input(slot);
        unsafe {
            let ctrl = self.in_ctx(slot, 0);
            ctrl.add(1).write_volatile(0b11); // add slot + EP0
            let sc = self.in_ctx(slot, 1);
            sc.write_volatile(at.route | (speed as u32) << 20 | 1 << 27);
            sc.add(1).write_volatile((port as u32) << 16);
            if let Some((hub_slot, hub_port)) = at.tt {
                sc.add(2).write_volatile(hub_slot as u32 | (hub_port as u32) << 8);
            }
        }
        self.set_ep(slot, 1, 4, mps0, 0, ep0_phys);
        self.command(in_phys, TRB_ADDRESS_DEVICE << 10 | (slot as u32) << 24)?;

        // The device descriptor: first 8 bytes for EP0's real packet size.
        let d8 = self.control_in(slot, Setup::get_descriptor(super::DESC_DEVICE, 0, 8))?;
        if d8.len() < 8 {
            return Err(String::from("short device descriptor"));
        }
        let real = if speed >= 4 { 1u32 << d8[7] } else { d8[7] as u32 };
        if real != mps0 && real != 0 {
            mps0 = real;
            self.clear_input(slot);
            unsafe { self.in_ctx(slot, 0).add(1).write_volatile(0b10) };
            self.set_ep(slot, 1, 4, mps0, 0, ep0_phys);
            self.command(in_phys, TRB_EVALUATE_CTX << 10 | (slot as u32) << 24)?;
        }
        let dev = self.control_in(slot, Setup::get_descriptor(super::DESC_DEVICE, 0, 18))?;
        if dev.len() < 18 {
            return Err(String::from("short device descriptor"));
        }
        let (vid, pid) = (u16::from_le_bytes([dev[8], dev[9]]), u16::from_le_bytes([dev[10], dev[11]]));
        let c9 = self.control_in(slot, Setup::get_descriptor(super::DESC_CONFIG, 0, 9))?;
        if c9.len() < 9 {
            return Err(String::from("short configuration descriptor"));
        }
        let total = u16::from_le_bytes([c9[2], c9[3]]).min(4096);
        let config_value = c9[5];
        let cfg = self.control_in(slot, Setup::get_descriptor(super::DESC_CONFIG, 0, total))?;
        let interfaces = super::parse_config(&cfg);
        crate::serial_println!(
            "[XHCI] {} port {} slot {}: {:04x}:{:04x}, speed {}, class {:02x}, {} interface(s)",
            self.name, at.path(), slot, vid, pid, speed, dev[4], interfaces.len()
        );
        if let Some(hub) = interfaces.iter().find(|i| i.class == 9).cloned() {
            return self.setup_hub(slot, at, config_value, &hub, found);
        }

        // What VEILOS can drive: boot keyboards and mice, bulk-only SCSI storage.
        let keyboard = interfaces.iter().find(|i| i.class == 3 && i.subclass == 1 && i.protocol == 1).cloned();
        let mouse = interfaces.iter().find(|i| i.class == 3 && i.subclass == 1 && i.protocol == 2).cloned();
        let storage = interfaces.iter().find(|i| i.class == 8 && i.subclass == 6 && i.protocol == 0x50).cloned();
        if keyboard.is_none() && mouse.is_none() && storage.is_none() {
            crate::serial_println!("[XHCI]   nothing VEILOS drives on it");
            return Ok(());
        }
        self.control_out(slot, Setup::set_configuration(config_value))?;

        let mut eps: Vec<Endpoint> = Vec::new();
        let mut hid: Vec<(Interface, HidKind, Endpoint)> = Vec::new();
        for (iface, kind) in [(keyboard, HidKind::Keyboard), (mouse, HidKind::Mouse)] {
            if let Some(i) = iface {
                if let Some(e) = i.endpoints.iter().find(|e| e.kind == 3 && e.is_in()) {
                    eps.push(*e);
                    hid.push((i.clone(), kind, *e));
                }
            }
        }
        let mut msc = None;
        if let Some(i) = &storage {
            let bin = i.endpoints.iter().find(|e| e.kind == 2 && e.is_in());
            let bout = i.endpoints.iter().find(|e| e.kind == 2 && !e.is_in());
            if let (Some(bin), Some(bout)) = (bin, bout) {
                eps.push(*bin);
                eps.push(*bout);
                msc = Some(MscFound { slot, interface: i.number, bulk_in: *bin, bulk_out: *bout });
            }
        }
        self.configure_endpoints(slot, speed, &eps, None)?;

        for (iface, kind, e) in hid {
            // Boot protocol (fixed report formats), and reports only on change.
            let _ = self.control_out(slot, Setup { request_type: 0x21, request: 0x0B, value: 0, index: iface.number as u16, length: 0 });
            let _ = self.control_out(slot, Setup { request_type: 0x21, request: 0x0A, value: 0, index: iface.number as u16, length: 0 });
            let (buf_phys, buf) = dma(64).ok_or("no DMA memory")?;
            let len = (e.max_packet as usize).clamp(3, 64);
            let s = self.slots[slot as usize].as_mut().unwrap();
            s.hid.push(HidEndpoint { dci: e.dci(), kind, buf_phys, buf, len, last: [0; 8] });
            if let Some(ring) = s.rings[e.dci() as usize].as_mut() {
                ring.push(buf_phys, len as u32, TRB_NORMAL << 10 | TRB_IOC | TRB_ISP);
            }
            w32(self.db + 4 * slot as u64, e.dci() as u32);
            crate::serial_println!("[XHCI]   {:?} on interface {}", kind, iface.number);
        }
        if let Some(m) = msc {
            crate::serial_println!("[XHCI]   mass storage (bulk-only SCSI)");
            found.push(m);
        }
        Ok(())
    }

    // -----| hubs |-----

    /// A hub: tell the controller it is one, power its ports, and set up
    /// what is plugged into them. Done once, at start-up, like the root
    /// ports (no hot-plug).
    fn setup_hub(&mut self, slot: u8, at: Attach, config_value: u8, hub: &Interface, found: &mut Vec<MscFound>) -> Result<(), String> {
        if at.tier >= 5 {
            return Err(String::from("hubs nested more than 5 deep"));
        }
        let usb3 = at.speed >= 4;
        self.control_out(slot, Setup::set_configuration(config_value))?;
        if usb3 {
            // SET_HUB_DEPTH: where its ports' route-string nibble goes.
            self.control_out(slot, Setup { request_type: 0x20, request: 12, value: at.tier as u16, index: 0, length: 0 })?;
        }
        let kind: u16 = if usb3 { 0x2A } else { 0x29 };
        let desc = self.control_in(slot, Setup { request_type: 0xA0, request: 6, value: kind << 8, index: 0, length: 16 })?;
        if desc.len() < 7 {
            return Err(String::from("short hub descriptor"));
        }
        let ports = desc[2].min(15);
        let chars = u16::from_le_bytes([desc[3], desc[4]]);
        let power_good_ms = (desc[5] as u64 * 2).max(20);
        // Think time of the transaction translator (high-speed hubs only).
        let ttt = if at.speed == 3 { ((chars >> 5) & 3) as u32 } else { 0 };
        crate::serial_println!("[XHCI]   a {} hub, {} ports", if usb3 { "USB 3" } else { "USB 2" }, ports);

        // The controller must know it is a hub (and its ports) before it
        // routes anything through it. Its status-change endpoint is set up
        // too, though not used yet.
        let eps: Vec<Endpoint> = hub.endpoints.iter().filter(|e| e.kind == 3 && e.is_in()).copied().collect();
        self.configure_endpoints(slot, at.speed, &eps, Some((ports, ttt)))?;

        for p in 1..=ports {
            let _ = self.hub_feature(slot, true, HUB_PORT_POWER, p);
        }
        crate::time::delay_ms(power_good_ms + 100); // power good, then connect debounce

        for p in 1..=ports {
            let mut st = self.hub_port_status(slot, p).unwrap_or((0, 0));
            if usb3 {
                // Link training: wait while the link state is Polling (7).
                crate::time::spin_until(500_000, || {
                    st = self.hub_port_status(slot, p).unwrap_or((0, 0));
                    (st.0 >> 5) & 0xF != 7
                });
            }
            if st.0 & 1 == 0 {
                continue; // nothing connected
            }
            let _ = self.hub_feature(slot, true, HUB_PORT_RESET, p);
            let reset = crate::time::spin_until(500_000, || {
                st = self.hub_port_status(slot, p).unwrap_or((0, 0));
                st.1 & (1 << 4) != 0 && st.0 & (1 << 4) == 0
            });
            for f in [HUB_C_PORT_RESET, HUB_C_PORT_CONNECTION] {
                let _ = self.hub_feature(slot, false, f, p);
            }
            if usb3 {
                for f in [HUB_C_BH_PORT_RESET, HUB_C_PORT_LINK_STATE] {
                    let _ = self.hub_feature(slot, false, f, p);
                }
            }
            crate::time::delay_ms(10); // recovery after reset
            let st = self.hub_port_status(slot, p).unwrap_or((0, 0));
            let child = Attach {
                root_port: at.root_port,
                route: at.route | (p as u32) << (4 * at.tier),
                tier: at.tier + 1,
                speed: if usb3 {
                    4
                } else if st.0 & (1 << 9) != 0 {
                    2
                } else if st.0 & (1 << 10) != 0 {
                    3
                } else {
                    1
                },
                tt: None,
            };
            let child = Attach {
                tt: match (child.speed, at.speed) {
                    (1 | 2, 3) => Some((slot, p)), // this hub translates
                    (1 | 2, _) => at.tt,           // a translator further up, if any
                    _ => None,
                },
                ..child
            };
            if !reset || st.0 & 2 == 0 {
                crate::serial_println!("[XHCI] {} port {}: device didn't enable (hub port status {:#06x})", self.name, child.path(), st.0);
                continue;
            }
            if let Err(e) = self.enumerate(child, found) {
                crate::serial_println!("[XHCI] {} port {}: {}", self.name, child.path(), e);
            }
        }
        Ok(())
    }

    /// SET_FEATURE / CLEAR_FEATURE on a hub port.
    fn hub_feature(&mut self, slot: u8, set: bool, feature: u16, port: u8) -> Result<(), String> {
        let request = if set { 3 } else { 1 };
        self.control_out(slot, Setup { request_type: 0x23, request, value: feature, index: port as u16, length: 0 })
    }

    /// GET_STATUS on a hub port: (wPortStatus, wPortChange).
    fn hub_port_status(&mut self, slot: u8, port: u8) -> Result<(u16, u16), String> {
        let s = self.control_in(slot, Setup { request_type: 0xA3, request: 0, value: 0, index: port as u16, length: 4 })?;
        if s.len() < 4 {
            return Err(String::from("short port status"));
        }
        Ok((u16::from_le_bytes([s[0], s[1]]), u16::from_le_bytes([s[2], s[3]])))
    }

    /// Add endpoints `eps` to the slot. `hub`: the device is a hub with
    /// this many ports and this TT think time.
    fn configure_endpoints(&mut self, slot: u8, speed: u8, eps: &[Endpoint], hub: Option<(u8, u32)>) -> Result<(), String> {
        self.clear_input(slot);
        let max_dci = eps.iter().map(|e| e.dci()).max().unwrap_or(1);
        let mut add = 1u32; // the slot context
        for e in eps {
            let ring = Ring::new().ok_or("no DMA memory")?;
            let ring_phys = ring.phys;
            self.slots[slot as usize].as_mut().unwrap().rings[e.dci() as usize] = Some(ring);
            let ep_type = match (e.kind, e.is_in()) {
                (2, false) => 2,
                (2, true) => 6,
                (3, false) => 3,
                _ => 7,
            };
            // Interrupt interval as 2^n x 125 us.
            let interval = if e.kind == 3 {
                if speed >= 3 {
                    (e.interval.max(1) - 1).min(15) as u32
                } else {
                    let frames = (e.interval.max(1) as u32) * 8;
                    (31 - frames.leading_zeros()).clamp(3, 10)
                }
            } else {
                0
            };
            self.set_ep(slot, e.dci(), ep_type, e.max_packet as u32, interval, ring_phys);
            add |= 1 << e.dci();
        }
        unsafe {
            self.in_ctx(slot, 0).add(1).write_volatile(add);
            // The slot context as it is now, with room for the new endpoints.
            let s = self.slots[slot as usize].as_ref().unwrap();
            core::ptr::copy_nonoverlapping(s.dev_ctx, self.in_ctx(slot, 1) as *mut u8, self.ctx_size);
            let sc = self.in_ctx(slot, 1);
            sc.write_volatile((sc.read_volatile() & !(0x1F << 27)) | (max_dci as u32) << 27);
            if let Some((ports, ttt)) = hub {
                sc.write_volatile(sc.read_volatile() | 1 << 26); // Hub
                let d1 = sc.add(1);
                d1.write_volatile((d1.read_volatile() & 0x00FF_FFFF) | (ports as u32) << 24);
                let d2 = sc.add(2);
                d2.write_volatile((d2.read_volatile() & !(3 << 16)) | ttt << 16);
            }
            // The output context's dword 3 (state, address) means nothing here.
            sc.add(3).write_volatile(0);
        }
        let in_phys = self.slots[slot as usize].as_ref().unwrap().in_phys;
        self.command(in_phys, TRB_CONFIGURE_EP << 10 | (slot as u32) << 24)?;
        Ok(())
    }

    // -----| transfers |-----

    fn control(&mut self, slot: u8, setup: Setup, data_in: bool) -> Result<usize, String> {
        let len = setup.length as usize;
        let ctl_phys = self.ctl_phys;
        let s = self.slots[slot as usize].as_mut().ok_or("no such slot")?;
        let ring = s.rings[1].as_mut().ok_or("no EP0 ring")?;
        let trt = if len == 0 { 0 } else if data_in { 3 } else { 2 };
        ring.push(setup.to_u64(), 8, TRB_SETUP << 10 | TRB_IDT | trt << 16);
        if len > 0 {
            ring.push(ctl_phys, len as u32, TRB_DATA << 10 | (data_in as u32) << 16 | TRB_ISP);
        }
        // Status stage: the opposite direction of the data, IN if none.
        let status_in = len == 0 || !data_in;
        ring.push(0, 0, TRB_STATUS << 10 | TRB_IOC | (status_in as u32) << 16);
        s.done[1] = None;
        w32(self.db + 4 * slot as u64, 1);
        let mut moved = len;
        loop {
            let d = self.wait_transfer(slot, 1, 1_000_000).ok_or("control transfer timed out")?;
            match d.cc {
                CC_SHORT_PACKET => moved = len.saturating_sub(d.remaining as usize), // then the status stage completes
                CC_SUCCESS => return Ok(moved),
                cc => return Err(format!("control transfer failed: code {}", cc)),
            }
        }
    }

    pub fn control_in(&mut self, slot: u8, setup: Setup) -> Result<Vec<u8>, String> {
        let n = self.control(slot, setup, true)?;
        let mut out = vec![0u8; n];
        unsafe { core::ptr::copy_nonoverlapping(self.ctl, out.as_mut_ptr(), n) };
        Ok(out)
    }

    pub fn control_out(&mut self, slot: u8, setup: Setup) -> Result<(), String> {
        self.control(slot, setup, false).map(|_| ())
    }

    /// A bulk transfer of `len` bytes at physical `phys` (contiguous) on
    /// endpoint `dci`. Returns the bytes moved, or the completion code.
    pub fn bulk(&mut self, slot: u8, dci: u8, phys: u64, len: usize, timeout_us: u64) -> Result<usize, u8> {
        let db = self.db;
        let s = self.slots[slot as usize].as_mut().ok_or(0u8)?;
        let ring = s.rings[dci as usize].as_mut().ok_or(0u8)?;
        // One TRB per page (a TRB mustn't cross a 64 KiB boundary), chained.
        let mut trbs: Vec<(u64, usize, usize)> = Vec::new(); // (trb, offset, length)
        let mut off = 0usize;
        loop {
            let at = phys + off as u64;
            let n = ((4096 - (at & 0xFFF)) as usize).min(len - off);
            let last = off + n >= len;
            let ctrl = TRB_NORMAL << 10 | TRB_ISP | if last { TRB_IOC } else { TRB_CH };
            trbs.push((ring.push(at, n as u32, ctrl), off, n));
            off += n;
            if last {
                break;
            }
        }
        s.done[dci as usize] = None;
        w32(db + 4 * slot as u64, dci as u32);
        let d = self.wait_transfer(slot, dci, timeout_us).ok_or(0u8)?;
        match d.cc {
            CC_SUCCESS => Ok(len - d.remaining as usize),
            CC_SHORT_PACKET => {
                // The TD ended at the TRB the event names.
                let (_, o, n) = trbs.iter().find(|t| t.0 == d.trb).copied().unwrap_or((0, 0, len));
                Ok(o + n.saturating_sub(d.remaining as usize))
            }
            cc => Err(cc),
        }
    }

    /// After a stall: Reset Endpoint, point the ring past what was queued,
    /// and clear the device's halt (CLEAR_FEATURE ENDPOINT_HALT).
    pub fn recover_endpoint(&mut self, slot: u8, ep: &Endpoint) {
        let dci = ep.dci();
        let _ = self.command(0, TRB_RESET_EP << 10 | (dci as u32) << 16 | (slot as u32) << 24);
        let deq = self.slots[slot as usize].as_ref().and_then(|s| s.rings[dci as usize].as_ref()).map(|r| r.dequeue_pointer());
        if let Some(deq) = deq {
            let _ = self.command(deq, TRB_SET_TR_DEQUEUE << 10 | (dci as u32) << 16 | (slot as u32) << 24);
        }
        let _ = self.control_out(slot, Setup { request_type: 0x02, request: 1, value: 0, index: ep.address as u16, length: 0 });
    }
}

/// USB legacy support (extended capability 1): ask the firmware to give
/// the controller up, and turn off the SMIs it uses to emulate PS/2.
fn take_from_firmware(base: u64, hcc1: u32) {
    let mut off = ((hcc1 >> 16) as u64) << 2;
    while off != 0 {
        let at = base + off;
        let cap = r32(at);
        if cap & 0xFF == 1 {
            if cap & (1 << 16) != 0 {
                w32(at, cap | 1 << 24); // OS owned
                let released = crate::time::spin_until(1_000_000, || r32(at) & (1 << 16) == 0);
                crate::serial_println!("[XHCI] BIOS handoff: {}", if released { "the firmware let go" } else { "WARN: the firmware kept it" });
            }
            // USBLEGCTLSTS: SMI enables off, change bits cleared.
            w32(at + 4, (r32(at + 4) & !0xE01F) | 0xE000_0000);
            return;
        }
        let next = ((cap >> 8) & 0xFF) as u64;
        if next == 0 {
            return;
        }
        off += next << 2;
    }
}
