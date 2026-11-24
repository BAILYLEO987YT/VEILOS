// =============================================================================
// VEILOS: NVMe
// =============================================================================
//
// NVM Express SSDs (PCI class 01:08:02), the disks most PCs since about 2017
// boot from. Start-up: disable the controller, give it an admin queue pair,
// enable it, Identify the controller and namespace 1, and create one I/O
// queue pair. Each namespace-1 disk is then a `block::Transport` like an
// AHCI port: reads and writes are single commands through a 64 KiB DMA
// bounce buffer, waited for by polling the completion queue's phase bit.
// The disk's lock means one command at a time, so one I/O queue is enough.
//
// Not yet: namespaces other than 1, 4 KiB-sector formats, interrupts.

use alloc::{boxed::Box, format, string::String};

const CAP: u64 = 0x00;
const CC: u64 = 0x14;
const CSTS: u64 = 0x1C;
const AQA: u64 = 0x24;
const ASQ: u64 = 0x28;
const ACQ: u64 = 0x30;

const QUEUE: usize = 64; // entries per queue
const SECTOR: usize = 512;
const BOUNCE: usize = 64 * 1024;

const ADMIN_CREATE_SQ: u8 = 0x01;
const ADMIN_CREATE_CQ: u8 = 0x05;
const ADMIN_IDENTIFY: u8 = 0x06;
const IO_WRITE: u8 = 0x01;
const IO_READ: u8 = 0x02;
const IO_FLUSH: u8 = 0x00;

/// NVMe controllers. Bound by `hal::bind` with the other storage.
pub const DRIVER: crate::hal::Driver = crate::hal::Driver {
    name: "nvme",
    class: crate::hal::DeviceClass::Storage,
    matches: |d| d.class == 0x01 && d.subclass == 0x08 && d.prog_if == 0x02,
    probe,
    remove: None,
};

static NEXT: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

fn probe(dev: &crate::hal::pci::PciDevice) -> Result<(), String> {
    let bar = dev.bar(0);
    let base = bar.mem_base().ok_or("no memory BAR0")?;
    dev.enable_for_driver();
    map_mmio(base, (bar.size as u64).max(0x4000));
    let (nvme, sectors, model) = Nvme::start(base)?;
    let name = format!("nvme{}", NEXT.fetch_add(1, core::sync::atomic::Ordering::Relaxed));
    crate::println!("     - {} {}: {} MiB", name, model, sectors / 2048);
    crate::serial_println!("[NVME] {:02x}:{:02x}.{} {}: {}, {} sectors", dev.bus, dev.device, dev.function, name, model, sectors);
    crate::fs::block::mount_disk(crate::fs::block::Disk::new(Box::new(nvme), name, sectors));
    Ok(())
}

fn map_mmio(phys: u64, size: u64) {
    use x86_64::structures::paging::{Mapper, Page, PageTableFlags, PhysFrame, Size4KiB};
    let mut mapper = unsafe { crate::mm::memory::get_mapper() };
    let mut frames = crate::mm::memory::GlobalFrameAllocator;
    let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::NO_CACHE;
    for off in (0..size).step_by(4096) {
        let page = Page::<Size4KiB>::containing_address(x86_64::VirtAddr::new(phys + off));
        let frame = PhysFrame::<Size4KiB>::containing_address(x86_64::PhysAddr::new(phys + off));
        if let Ok(f) = unsafe { mapper.map_to(page, frame, flags, &mut frames) } {
            f.flush();
        }
    }
}

fn dma(bytes: usize) -> Result<(u64, *mut u8), String> {
    crate::mm::memory::DmaRegion::new(bytes).map(|r| r.leak()).ok_or_else(|| String::from("no DMA memory"))
}

/// A submission queue and its completion queue.
struct QueuePair {
    sq_phys: u64,
    sq: *mut u32,
    cq_phys: u64,
    cq: *mut u32,
    sq_tail: usize,
    cq_head: usize,
    phase: bool,
    sq_doorbell: u64,
    cq_doorbell: u64,
    cid: u16,
}

impl QueuePair {
    fn new(base: u64, qid: u64, stride: u64) -> Result<Self, String> {
        let (sq_phys, sq) = dma(QUEUE * 64)?;
        let (cq_phys, cq) = dma(QUEUE * 16)?;
        Ok(Self {
            sq_phys, sq: sq as *mut u32, cq_phys, cq: cq as *mut u32,
            sq_tail: 0, cq_head: 0, phase: true,
            sq_doorbell: base + 0x1000 + (2 * qid) * stride,
            cq_doorbell: base + 0x1000 + (2 * qid + 1) * stride,
            cid: 0,
        })
    }

    /// Submit a command (dwords 1-15 as given; dword 0 gets the opcode and
    /// an id) and wait for its completion. Returns dword 0 of the result, or
    /// the status code.
    fn run(&mut self, opcode: u8, dw: [u32; 15], timeout_us: u64) -> Result<u32, u16> {
        self.cid = self.cid.wrapping_add(1);
        let cid = self.cid;
        unsafe {
            let e = self.sq.add(self.sq_tail * 16);
            e.write_volatile(opcode as u32 | (cid as u32) << 16);
            for (i, v) in dw.iter().enumerate() {
                e.add(1 + i).write_volatile(*v);
            }
        }
        self.sq_tail = (self.sq_tail + 1) % QUEUE;
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        unsafe { (self.sq_doorbell as *mut u32).write_volatile(self.sq_tail as u32) };

        let mut result = None;
        crate::arch::time::spin_until(timeout_us, || {
            let c = unsafe { self.cq.add(self.cq_head * 4) };
            let dw3 = unsafe { c.add(3).read_volatile() };
            if (dw3 & (1 << 16) != 0) != self.phase {
                return false;
            }
            let dw0 = unsafe { c.read_volatile() };
            self.cq_head += 1;
            if self.cq_head == QUEUE {
                self.cq_head = 0;
                self.phase = !self.phase;
            }
            unsafe { (self.cq_doorbell as *mut u32).write_volatile(self.cq_head as u32) };
            if dw3 as u16 == cid {
                let status = ((dw3 >> 17) & 0x7FFF) as u16;
                result = Some(if status == 0 { Ok(dw0) } else { Err(status) });
                return true;
            }
            false
        });
        result.unwrap_or(Err(0xFFFF))
    }
}

pub struct Nvme {
    io: QueuePair,
    nsid: u32,
    bounce_phys: u64,
    bounce: *mut u8,
    /// A page of PRP entries for transfers over two pages.
    prp_phys: u64,
    prp: *mut u64,
    /// Largest transfer the controller takes (MDTS), at most BOUNCE.
    max_bytes: usize,
}

// MMIO and DMA memory owned by this controller, used under the disk's lock.
unsafe impl Send for Nvme {}

fn r32(a: u64) -> u32 {
    unsafe { (a as *const u32).read_volatile() }
}
fn w32(a: u64, v: u32) {
    unsafe { (a as *mut u32).write_volatile(v) }
}
fn r64(a: u64) -> u64 {
    r32(a) as u64 | (r32(a + 4) as u64) << 32
}
fn w64(a: u64, v: u64) {
    w32(a, v as u32);
    w32(a + 4, (v >> 32) as u32);
}

impl Nvme {
    fn start(base: u64) -> Result<(Self, u64, String), String> {
        let cap = r64(base + CAP);
        let stride = 4u64 << ((cap >> 32) & 0xF);
        let timeout_us = (((cap >> 24) & 0xFF).max(1)) * 500_000;
        let mqes = (cap & 0xFFFF) as usize + 1;
        if mqes < QUEUE {
            return Err(format!("queues of {} entries are too small", mqes));
        }
        if (cap >> 48) & 0xF != 0 {
            return Err(String::from("needs pages larger than 4 KiB"));
        }

        // Disable, then set up the admin queues and enable.
        if r32(base + CC) & 1 != 0 {
            w32(base + CC, r32(base + CC) & !1);
        }
        if !crate::arch::time::spin_until(timeout_us, || r32(base + CSTS) & 1 == 0) {
            return Err(String::from("didn't disable"));
        }
        let mut admin = QueuePair::new(base, 0, stride)?;
        w32(base + AQA, ((QUEUE as u32 - 1) << 16) | (QUEUE as u32 - 1));
        w64(base + ASQ, admin.sq_phys);
        w64(base + ACQ, admin.cq_phys);
        // Enable; NVM command set, 4 KiB pages, round robin, 64-byte SQ and
        // 16-byte CQ entries.
        w32(base + CC, 1 | 6 << 16 | 4 << 20);
        if !crate::arch::time::spin_until(timeout_us, || r32(base + CSTS) & 3 != 0) {
            return Err(String::from("didn't become ready"));
        }
        if r32(base + CSTS) & 2 != 0 {
            return Err(String::from("controller fatal status"));
        }

        let (id_phys, id) = dma(4096)?;
        let ident = |admin: &mut QueuePair, cns: u32, nsid: u32| -> Result<(), String> {
            let mut dw = [0u32; 15];
            dw[0] = nsid;
            dw[5] = id_phys as u32;
            dw[6] = (id_phys >> 32) as u32;
            dw[9] = cns;
            admin.run(ADMIN_IDENTIFY, dw, 1_000_000).map(|_| ()).map_err(|s| format!("Identify failed: status {:#x}", s))
        };
        ident(&mut admin, 1, 0)?;
        let idb = unsafe { core::slice::from_raw_parts(id, 4096) };
        let model = String::from_utf8_lossy(&idb[24..64]).trim().into();
        let mdts = idb[77];
        let max_bytes = if mdts == 0 { BOUNCE } else { (4096usize << mdts).min(BOUNCE) };

        let nsid = 1;
        ident(&mut admin, 0, nsid)?;
        let idb = unsafe { core::slice::from_raw_parts(id, 4096) };
        let sectors = u64::from_le_bytes(idb[0..8].try_into().unwrap());
        let lbaf = (idb[26] & 0xF) as usize;
        let lbads = idb[128 + lbaf * 4 + 2];
        if sectors == 0 {
            return Err(String::from("namespace 1 is empty"));
        }
        if lbads != 9 {
            return Err(format!("namespace 1 has {}-byte sectors; only 512 is supported yet", 1u32 << lbads));
        }

        // I/O queue pair 1: completion queue first.
        let io = QueuePair::new(base, 1, stride)?;
        let mut dw = [0u32; 15];
        dw[5] = io.cq_phys as u32;
        dw[6] = (io.cq_phys >> 32) as u32;
        dw[9] = (QUEUE as u32 - 1) << 16 | 1;
        dw[10] = 1; // physically contiguous, no interrupts
        admin.run(ADMIN_CREATE_CQ, dw, 1_000_000).map_err(|s| format!("Create I/O CQ failed: {:#x}", s))?;
        let mut dw = [0u32; 15];
        dw[5] = io.sq_phys as u32;
        dw[6] = (io.sq_phys >> 32) as u32;
        dw[9] = (QUEUE as u32 - 1) << 16 | 1;
        dw[10] = 1 << 16 | 1; // completion queue 1, physically contiguous
        admin.run(ADMIN_CREATE_SQ, dw, 1_000_000).map_err(|s| format!("Create I/O SQ failed: {:#x}", s))?;

        let (bounce_phys, bounce) = dma(BOUNCE)?;
        let (prp_phys, prp) = dma(4096)?;
        Ok((Self { io, nsid, bounce_phys, bounce, prp_phys, prp: prp as *mut u64, max_bytes }, sectors, model))
    }

    /// Read or write `bytes` (a multiple of 512, at most `max_bytes`) at
    /// `lba` through the bounce buffer.
    fn rw(&mut self, write: bool, lba: u64, bytes: usize) -> bool {
        let pages = bytes.div_ceil(4096);
        let prp2 = match pages {
            1 => 0,
            2 => self.bounce_phys + 4096,
            _ => {
                for i in 1..pages {
                    unsafe { self.prp.add(i - 1).write_volatile(self.bounce_phys + (i * 4096) as u64) };
                }
                self.prp_phys
            }
        };
        let mut dw = [0u32; 15];
        dw[0] = self.nsid;
        dw[5] = self.bounce_phys as u32;
        dw[6] = (self.bounce_phys >> 32) as u32;
        dw[7] = prp2 as u32;
        dw[8] = (prp2 >> 32) as u32;
        dw[9] = lba as u32;
        dw[10] = (lba >> 32) as u32;
        dw[11] = (bytes / SECTOR - 1) as u32;
        match self.io.run(if write { IO_WRITE } else { IO_READ }, dw, 5_000_000) {
            Ok(_) => true,
            Err(s) => {
                crate::serial_println!("[NVME] {} at {} failed: status {:#x}", if write { "write" } else { "read" }, lba, s);
                false
            }
        }
    }

}

impl crate::fs::block::Transport for Nvme {
    fn read(&mut self, lba: u64, buf: &mut [u8]) -> bool {
        let chunk = self.max_bytes;
        for (i, part) in buf.chunks_mut(chunk).enumerate() {
            if !self.rw(false, lba + (i * chunk / SECTOR) as u64, part.len()) {
                return false;
            }
            unsafe { core::ptr::copy_nonoverlapping(self.bounce, part.as_mut_ptr(), part.len()) };
        }
        true
    }

    /// The drive's volatile write cache to flash (power-off, FlushFileBuffers).
    fn flush(&mut self) -> bool {
        let mut dw = [0u32; 15];
        dw[0] = self.nsid;
        self.io.run(IO_FLUSH, dw, 5_000_000).is_ok()
    }

    fn write(&mut self, lba: u64, buf: &[u8]) -> bool {
        let chunk = self.max_bytes;
        for (i, part) in buf.chunks(chunk).enumerate() {
            unsafe { core::ptr::copy_nonoverlapping(part.as_ptr(), self.bounce, part.len()) };
            if !self.rw(true, lba + (i * chunk / SECTOR) as u64, part.len()) {
                return false;
            }
        }
        true
    }
}
