// =============================================================================
// VEILOS: Realtek RTL8111/8168 gigabit Ethernet (and RTL8169, RTL8101/8136)
// =============================================================================
//
// The NIC on most consumer motherboards that don't have Intel's. Registers
// through the MMIO BAR (BAR2 on PCIe parts, BAR1 on the PCI RTL8169), 64
// receive and 64 transmit descriptors of 2 KiB buffers in one DMA block
// below 4 GiB, polled (interrupts masked), as e1000.rs.
//
// Start-up follows what every RTL8168 revision accepts: reset, unlock the
// config registers, receive/transmit sizes, descriptor rings, enable RX/TX,
// receive and transmit configuration, lock. Linux also applies per-revision
// PHY and "EPHY" tweaks (a few hundred of them); none are here, so this
// relies on the PHY as the firmware left it, and restarts auto-negotiation
// (powering the PHY up if needed) when no link comes. The chip revision
// (XID) and PHY status are logged and shown by NET STATUS, so a machine that
// needs more says which.
//
// QEMU emulates none of these chips: this is tested on real hardware.

use alloc::{boxed::Box, format, string::String, vec::Vec};
use smoltcp::wire::EthernetAddress;

use crate::virtio_net::PacketBuf;

const IDR0: u64 = 0x00; // MAC address, 6 bytes
const TNPDS: u64 = 0x20; // transmit (normal priority) descriptor start, 64-bit
const CHIP_CMD: u64 = 0x37;
const TPPOLL: u64 = 0x38;
const IMR: u64 = 0x3C;
const ISR: u64 = 0x3E;
const TX_CONFIG: u64 = 0x40;
const RX_CONFIG: u64 = 0x44;
const CFG9346: u64 = 0x50;
const PHYAR: u64 = 0x60;
const PHY_STATUS: u64 = 0x6C;
const RX_MAX_SIZE: u64 = 0xDA;
const CPLUS_CMD: u64 = 0xE0;
const RDSAR: u64 = 0xE4; // receive descriptor start, 64-bit
const MAX_TX_PACKET: u64 = 0xEC;

const CMD_RESET: u8 = 1 << 4;
const CMD_RX_ENABLE: u8 = 1 << 3;
const CMD_TX_ENABLE: u8 = 1 << 2;

const DESC_OWN: u32 = 1 << 31;
const DESC_EOR: u32 = 1 << 30; // end of ring
const DESC_FS: u32 = 1 << 29;
const DESC_LS: u32 = 1 << 28;
const RX_RES: u32 = 1 << 21; // receive error summary

const RX_DESC: usize = 64;
const TX_DESC: usize = 64;
const BUF: usize = 2048;

const RX_DESCS_AT: usize = 0;
const TX_DESCS_AT: usize = RX_DESC * 16;
const RX_BUFS_AT: usize = TX_DESCS_AT + TX_DESC * 16;
const TX_BUFS_AT: usize = RX_BUFS_AT + RX_DESC * BUF;
const DMA_BYTES: usize = TX_BUFS_AT + TX_DESC * BUF;

/// Realtek NICs (and the D-Link and Linksys ones built on the same chips).
pub const DRIVER: crate::hal::Driver = crate::hal::Driver {
    name: "r8169",
    class: crate::hal::DeviceClass::Network,
    matches: |d| matches!(
        (d.vendor_id, d.device_id),
        (0x10EC, 0x8168) | (0x10EC, 0x8161) | (0x10EC, 0x8169) | (0x10EC, 0x8167) | (0x10EC, 0x8136)
            | (0x1186, 0x4300) | (0x1186, 0x4302) | (0x1737, 0x1032)
    ),
    probe,
    remove: None,
};

fn probe(dev: &crate::pci::PciDevice) -> Result<(), String> {
    // The register block: the memory BAR (BAR2 on PCIe chips, BAR1 on the
    // PCI RTL8169); BAR0 is the same registers in I/O space.
    let (bar, base) = [2usize, 1]
        .iter()
        .find_map(|&i| dev.bar(i).mem_base().filter(|&b| b != 0).map(|b| (i, b)))
        .ok_or("no memory BAR")?;
    dev.enable_for_driver();
    map_mmio(base, (dev.bar(bar).size as u64).max(0x1000));
    crate::serial_println!("[RTL] {:02x}:{:02x}.{} {:04x}:{:04x} registers at {:#x} (BAR{})",
        dev.bus, dev.device, dev.function, dev.vendor_id, dev.device_id, base, bar);
    let nic = Rtl::new(base)?;
    crate::network::attach_nic(Box::new(nic));
    Ok(())
}

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

pub struct Rtl {
    base: u64,
    mac: EthernetAddress,
    /// Chip revision: TxConfig bits 31:20, masked as Linux's XID.
    xid: u32,
    dma_phys: u64,
    dma: *mut u8,
    rx_next: usize,
    tx_next: usize,
    rx_count: u64,
    tx_count: u64,
    rx_errors: u64,
    link_was_up: bool,
}

// MMIO and DMA memory owned by this NIC, used under the network lock.
unsafe impl Send for Rtl {}

impl Rtl {
    fn r8(&self, reg: u64) -> u8 {
        unsafe { ((self.base + reg) as *const u8).read_volatile() }
    }
    fn w8(&self, reg: u64, v: u8) {
        unsafe { ((self.base + reg) as *mut u8).write_volatile(v) }
    }
    fn w16(&self, reg: u64, v: u16) {
        unsafe { ((self.base + reg) as *mut u16).write_volatile(v) }
    }
    fn r16(&self, reg: u64) -> u16 {
        unsafe { ((self.base + reg) as *const u16).read_volatile() }
    }
    fn r32(&self, reg: u64) -> u32 {
        unsafe { ((self.base + reg) as *const u32).read_volatile() }
    }
    fn w32(&self, reg: u64, v: u32) {
        unsafe { ((self.base + reg) as *mut u32).write_volatile(v) }
    }
    fn desc(&self, at: usize, i: usize) -> *mut u32 {
        unsafe { self.dma.add(at + i * 16) as *mut u32 }
    }

    fn new(base: u64) -> Result<Self, String> {
        let (dma_phys, dma) = crate::memory::DmaRegion::new(DMA_BYTES).ok_or("no DMA memory")?.leak();
        let mut nic = Self {
            base, mac: EthernetAddress([0; 6]), xid: 0, dma_phys, dma,
            rx_next: 0, tx_next: 0, rx_count: 0, tx_count: 0, rx_errors: 0, link_was_up: false,
        };
        let txc = nic.r32(TX_CONFIG);
        nic.xid = (txc >> 20) & 0xFCF;
        let mac: [u8; 6] = core::array::from_fn(|i| nic.r8(IDR0 + i as u64));
        if mac == [0; 6] || mac == [0xFF; 6] {
            return Err(format!("no MAC address (chip XID {:#x})", nic.xid));
        }
        nic.mac = EthernetAddress(mac);

        // Interrupts off; reset.
        nic.w16(IMR, 0);
        nic.w16(ISR, 0xFFFF);
        nic.w8(CHIP_CMD, CMD_RESET);
        if !crate::time::spin_until(100_000, || nic.r8(CHIP_CMD) & CMD_RESET == 0) {
            return Err(format!("reset didn't finish (chip XID {:#x})", nic.xid));
        }

        // The rings: every receive descriptor the NIC's, with its buffer.
        for i in 0..RX_DESC {
            nic.give_rx(i);
        }
        for i in 0..TX_DESC {
            let d = nic.desc(TX_DESCS_AT, i);
            unsafe {
                d.write_volatile(if i == TX_DESC - 1 { DESC_EOR } else { 0 });
                d.add(1).write_volatile(0);
                (d.add(2) as *mut u64).write_volatile(0);
            }
        }

        nic.w8(CFG9346, 0xC0); // unlock the config registers
        nic.w16(CPLUS_CMD, nic.r16(CPLUS_CMD) & !(1 << 6)); // no VLAN tag stripping
        nic.w16(RX_MAX_SIZE, BUF as u16);
        nic.w8(MAX_TX_PACKET, 0x3B);
        let tx = nic.dma_phys + TX_DESCS_AT as u64;
        let rx = nic.dma_phys + RX_DESCS_AT as u64;
        nic.w32(TNPDS, tx as u32);
        nic.w32(TNPDS + 4, (tx >> 32) as u32);
        nic.w32(RDSAR, rx as u32);
        nic.w32(RDSAR + 4, (rx >> 32) as u32);
        nic.w8(CHIP_CMD, CMD_RX_ENABLE | CMD_TX_ENABLE);
        // Receive: no FIFO threshold, unlimited DMA burst, broadcast,
        // multicast and our own address. Transmit: standard gap, unlimited burst.
        nic.w32(RX_CONFIG, 7 << 13 | 7 << 8 | 1 << 3 | 1 << 2 | 1 << 1);
        nic.w32(TX_CONFIG, (txc & !0x0300_0700) | 3 << 24 | 7 << 8);
        nic.w8(CFG9346, 0x00); // lock

        // Link: as the firmware left the PHY; else power it up and restart
        // auto-negotiation.
        let mut up = crate::time::spin_until(3_000_000, || nic.r8(PHY_STATUS) & 0x02 != 0);
        if !up {
            let bmcr = nic.phy_read(0).unwrap_or(0);
            crate::serial_println!("[RTL] no link in 3 s (BMCR {:#06x}): powering the PHY up, restarting auto-negotiation", bmcr);
            nic.phy_write(0, (bmcr & !(1 << 11)) | 1 << 12 | 1 << 9);
            up = crate::time::spin_until(5_000_000, || nic.r8(PHY_STATUS) & 0x02 != 0);
        }
        nic.link_was_up = up;
        crate::serial_println!("[RTL] chip XID {:#x}, MAC {}, link {}", nic.xid, nic.mac, nic.link_text());
        Ok(nic)
    }

    fn link_text(&self) -> String {
        let s = self.r8(PHY_STATUS);
        if s & 0x02 == 0 {
            return String::from("down");
        }
        let speed = if s & 0x10 != 0 { 1000 } else if s & 0x08 != 0 { 100 } else { 10 };
        format!("up at {} Mb/s{}", speed, if s & 0x01 != 0 { " full duplex" } else { "" })
    }

    /// Hand receive descriptor `i` back to the NIC, empty.
    fn give_rx(&self, i: usize) {
        let d = self.desc(RX_DESCS_AT, i);
        let eor = if i == RX_DESC - 1 { DESC_EOR } else { 0 };
        unsafe {
            (d.add(2) as *mut u64).write_volatile(self.dma_phys + (RX_BUFS_AT + i * BUF) as u64);
            d.add(1).write_volatile(0);
            core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
            d.write_volatile(DESC_OWN | eor | BUF as u32);
        }
    }

    fn phy_read(&self, reg: u32) -> Option<u16> {
        self.w32(PHYAR, (reg & 0x1F) << 16);
        let mut v = 0;
        crate::time::spin_until(10_000, || {
            v = self.r32(PHYAR);
            v & (1 << 31) != 0
        })
        .then_some(v as u16)
    }

    fn phy_write(&self, reg: u32, value: u16) {
        self.w32(PHYAR, 1 << 31 | (reg & 0x1F) << 16 | value as u32);
        crate::time::spin_until(10_000, || self.r32(PHYAR) & (1 << 31) == 0);
    }
}

impl crate::network::Nic for Rtl {
    fn mac(&self) -> EthernetAddress {
        self.mac
    }

    fn link_up(&self) -> bool {
        self.r8(PHY_STATUS) & 0x02 != 0
    }

    fn poll_rx(&mut self, out: &mut Vec<PacketBuf>) {
        let up = self.link_up();
        if up != self.link_was_up {
            self.link_was_up = up;
            crate::serial_println!("[RTL] link {}", self.link_text());
        }
        loop {
            let d = self.desc(RX_DESCS_AT, self.rx_next);
            let status = unsafe { d.read_volatile() };
            if status & DESC_OWN != 0 {
                break; // still the NIC's
            }
            // A whole frame in one buffer, no errors. The length counts the CRC.
            let len = (status & 0x3FFF) as usize;
            if status & RX_RES == 0 && status & (DESC_FS | DESC_LS) == DESC_FS | DESC_LS && len > 4 {
                let n = (len - 4).min(crate::virtio_net::MTU);
                let mut pkt = PacketBuf::new();
                unsafe { core::ptr::copy_nonoverlapping(self.dma.add(RX_BUFS_AT + self.rx_next * BUF), pkt.data.as_mut_ptr(), n) };
                pkt.len = n;
                out.push(pkt);
                self.rx_count += 1;
            } else {
                self.rx_errors += 1;
            }
            self.give_rx(self.rx_next);
            self.rx_next = (self.rx_next + 1) % RX_DESC;
        }
    }

    fn transmit(&mut self, frame: &[u8]) {
        if frame.is_empty() || frame.len() > BUF {
            return;
        }
        let i = self.tx_next;
        let d = self.desc(TX_DESCS_AT, i);
        // Still the NIC's from the last time round the ring: wait briefly.
        if unsafe { d.read_volatile() } & DESC_OWN != 0
            && !crate::time::spin_until(10_000, || unsafe { d.read_volatile() } & DESC_OWN == 0)
        {
            crate::serial_println!("[RTL] TX ring full; frame dropped");
            return;
        }
        // Short frames padded to the Ethernet minimum (the chip doesn't).
        let len = frame.len().max(60);
        let eor = if i == TX_DESC - 1 { DESC_EOR } else { 0 };
        unsafe {
            let buf = self.dma.add(TX_BUFS_AT + i * BUF);
            core::ptr::copy_nonoverlapping(frame.as_ptr(), buf, frame.len());
            core::ptr::write_bytes(buf.add(frame.len()), 0, len - frame.len());
            (d.add(2) as *mut u64).write_volatile(self.dma_phys + (TX_BUFS_AT + i * BUF) as u64);
            d.add(1).write_volatile(0);
            core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
            d.write_volatile(DESC_OWN | eor | DESC_FS | DESC_LS | len as u32);
        }
        self.w8(TPPOLL, 0x40); // normal-priority queue: go
        self.tx_next = (i + 1) % TX_DESC;
        self.tx_count += 1;
    }

    fn counts(&self) -> (u64, u64) {
        (self.rx_count, self.tx_count)
    }

    fn key(&self) -> u64 {
        self.base
    }

    fn kind(&self) -> &'static str {
        "rtl8168"
    }

    fn detail(&self) -> String {
        format!("chip XID {:#x}, {}, PHY status {:#04x}, rx errors {}",
            self.xid, self.link_text(), self.r8(PHY_STATUS), self.rx_errors)
    }
}
