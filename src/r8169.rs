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
// receive and transmit configuration, lock. From the RTL8168G on (XID 0x4C0
// and later, Linux's MAC version 40+) three things differ, and the first
// real test (an RTL8168G, XID 0x4C0) had no link without them:
//   * the PHY's registers are reached through the OCP window (GPHY_OCP,
//     0xB8; standard MII register r at 0xA400 + 2r), not PHYAR;
//   * the PHY's PLL must be powered up (PMCH, 0x6F, bits 7:6), as Linux's
//     rtl_pll_power_up does;
//   * the PHY's link-down power saving (ALDPS) is turned off, as Linux does
//     for these chips, so the PHY keeps looking for a link partner;
//   * the chip's embedded controller may own the port ("out-of-band" mode,
//     which firmware and Windows use for Wake-on-LAN, keeping the PHY in a
//     low-power, slow mode): the driver takes the port back as Linux's
//     rtl_hw_init_8168g does (wait for the FIFOs to drain, RX/TX off,
//     MCU.NOW_IS_OOB cleared, the MAC OCP 0xE8DE handshake), and ungates
//     receive (MISC.RXDV_GATED_EN) once the rings are set up.
// Linux also applies per-revision PHY and "EPHY" tweaks and PHY firmware;
// none are here. The PHY is reset to its defaults, every speed advertised,
// and auto-negotiation restarted at every start. The chip revision (XID) and the
// PHY's status are logged and shown by NET STATUS.
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
const MAR0: u64 = 0x08; // multicast filter, 8 bytes
const PMCH: u64 = 0x6F; // power management (8168G+: PLL power)
const GPHY_OCP: u64 = 0xB8; // PHY registers through OCP (8168G+)
const OCP_STD_PHY: u32 = 0xA400; // MII register r at OCP_STD_PHY + 2r
const OCP_ALDPS: u32 = 0xA430; // PHY page 0xA43, register 0x10: bit 2 = ALDPS

const MCU: u64 = 0xD3; // 8168G+: embedded controller state
const MCU_NOW_IS_OOB: u8 = 1 << 7;
const MCU_RXTX_EMPTY: u8 = 1 << 5 | 1 << 4;
const MCU_LINK_LIST_RDY: u8 = 1 << 1;
const MISC: u64 = 0xF0;
const MISC_RXDV_GATED_EN: u32 = 1 << 19;
const TXCFG_EMPTY: u32 = 1 << 11;
const OCPDR: u64 = 0xB0; // MAC registers through OCP (8168G+)

// MII registers.
const BMCR: u32 = 0;
const BMSR: u32 = 1;
const ANAR: u32 = 4; // what we advertise
const ANLPAR: u32 = 5; // what the other end advertised (0: nobody there)
const GBCR: u32 = 9; // gigabit advertisement
const BMCR_RESET: u16 = 1 << 15;
const BMCR_ANENABLE: u16 = 1 << 12;
const BMCR_PDOWN: u16 = 1 << 11;
const BMCR_ANRESTART: u16 = 1 << 9;

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

/// RTL8168G and later (Linux's MAC version 40 on): PHY through OCP, PLL
/// power in PMCH. The older chips' XIDs are all below 0x4C0.
fn is_8168g_or_later(xid: u32) -> bool {
    xid & 0x7CF >= 0x4C0
}

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
        let newer = is_8168g_or_later(nic.xid);
        if newer {
            // The PHY's PLL on (Linux: rtl_pll_power_up for MAC version 40+).
            nic.w8(PMCH, nic.r8(PMCH) | 0xC0);
            crate::time::delay_ms(10);
            nic.leave_oob();
        }

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
        // Receive: unlimited DMA burst; broadcast, multicast (every group: the
        // filter all ones) and our own address. The top bits differ by
        // generation: a FIFO threshold before the 8168G, then 128-descriptor
        // interrupts, multiple-descriptor fetch and early-receive off (as
        // Linux's rtl_init_rxcfg). Transmit: standard gap, unlimited burst.
        let accept = 1 << 3 | 1 << 2 | 1 << 1;
        let rxcfg = if newer { 1 << 15 | 1 << 14 | 1 << 11 | 7 << 8 } else { 7 << 13 | 7 << 8 };
        nic.w32(RX_CONFIG, rxcfg | accept);
        for i in 0..8 {
            nic.w8(MAR0 + i, 0xFF);
        }
        nic.w32(TX_CONFIG, (txc & !0x0300_0700) | 3 << 24 | 7 << 8);
        nic.w8(CFG9346, 0x00); // lock
        if newer {
            // Frames may reach the rings now (Linux: rtl_disable_rxdvgate).
            nic.w32(MISC, nic.r32(MISC) & !MISC_RXDV_GATED_EN);
        }

        // The PHY: who it is (proves the register path works), as it was
        // left (what it advertised), then reset to its defaults with ALDPS
        // off, every speed advertised, powered up, auto-negotiation restarted.
        let id = (nic.phy_read(2), nic.phy_read(3));
        crate::serial_println!(
            "[RTL] chip XID {:#x} ({}), PMCH {:#04x}, MCU {:#04x}, MISC {:#010x}, PHY id {:04x?}/{:04x?}, \
             was: BMCR {:04x?} ANAR {:04x?} GBCR {:04x?}",
            nic.xid, if newer { "8168G or later: PHY via OCP" } else { "PHY via PHYAR" },
            nic.r8(PMCH), nic.r8(MCU), nic.r32(MISC), id.0, id.1,
            nic.phy_read(BMCR), nic.phy_read(ANAR), nic.phy_read(GBCR)
        );
        nic.phy_write(BMCR, BMCR_RESET);
        crate::time::spin_until(500_000, || nic.phy_read(BMCR).is_some_and(|v| v & BMCR_RESET == 0));
        if newer {
            if let Some(v) = nic.ocp_read(OCP_ALDPS) {
                nic.ocp_write(OCP_ALDPS, v & !(1 << 2));
            }
        }
        // 10/100 half and full, symmetric and asymmetric pause; 1000 half and full.
        nic.phy_write(ANAR, 0x0DE1);
        nic.phy_write(GBCR, 0x0300);
        let bmcr = nic.phy_read(BMCR).unwrap_or(0);
        nic.phy_write(BMCR, (bmcr & !BMCR_PDOWN) | BMCR_ANENABLE | BMCR_ANRESTART);
        // Auto-negotiation takes up to about 3 s at gigabit; allow more.
        let up = crate::time::spin_until(8_000_000, || nic.r8(PHY_STATUS) & 0x02 != 0);
        nic.link_was_up = up;
        let _ = nic.phy_read(BMSR); // link status latches low: read twice
        crate::serial_println!(
            "[RTL] MAC {}, link {} (PHY status {:#04x}, BMSR {:04x?}, ANAR {:04x?}, partner ANLPAR {:04x?})",
            nic.mac, nic.link_text(), nic.r8(PHY_STATUS), nic.phy_read(BMSR), nic.phy_read(ANAR), nic.phy_read(ANLPAR)
        );
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

    /// A PHY register through OCP (8168G and later).
    /// A MAC register through OCP (8168G and later; Linux's r8168_mac_ocp_*).
    fn mac_ocp_read(&self, reg: u32) -> u16 {
        self.w32(OCPDR, reg << 15);
        self.r32(OCPDR) as u16
    }

    fn mac_ocp_write(&self, reg: u32, value: u16) {
        self.w32(OCPDR, 1 << 31 | reg << 15 | value as u32);
    }

    /// Take the port back from the chip's embedded controller (Linux's
    /// rtl_hw_init_8168g): receive gated off, FIFOs drained, RX/TX off,
    /// out-of-band mode left, and the controller's link-list handshake.
    fn leave_oob(&self) {
        self.w32(MISC, self.r32(MISC) | MISC_RXDV_GATED_EN);
        crate::time::delay_ms(2);
        let tx_empty = crate::time::spin_until(100_000, || self.r32(TX_CONFIG) & TXCFG_EMPTY != 0);
        let rx_empty = crate::time::spin_until(100_000, || self.r8(MCU) & MCU_RXTX_EMPTY == MCU_RXTX_EMPTY);
        self.w8(CHIP_CMD, self.r8(CHIP_CMD) & !0x0C);
        crate::time::delay_ms(1);
        let mcu = self.r8(MCU);
        self.w8(MCU, mcu & !MCU_NOW_IS_OOB);
        let v = self.mac_ocp_read(0xE8DE);
        self.mac_ocp_write(0xE8DE, v & !(1 << 14));
        let ready1 = crate::time::spin_until(100_000, || self.r8(MCU) & MCU_LINK_LIST_RDY != 0);
        let v = self.mac_ocp_read(0xE8DE);
        self.mac_ocp_write(0xE8DE, v | 1 << 15);
        let ready2 = crate::time::spin_until(100_000, || self.r8(MCU) & MCU_LINK_LIST_RDY != 0);
        crate::serial_println!(
            "[RTL] out-of-band: MCU was {:#04x} ({}), FIFOs empty tx {} rx {}, link list ready {}/{}, MCU now {:#04x}, E8DE {:#06x}",
            mcu, if mcu & MCU_NOW_IS_OOB != 0 { "OOB: taking the port back" } else { "not OOB" },
            tx_empty, rx_empty, ready1, ready2, self.r8(MCU), self.mac_ocp_read(0xE8DE)
        );
    }

    fn ocp_read(&self, addr: u32) -> Option<u16> {
        self.w32(GPHY_OCP, addr << 15);
        let mut v = 0;
        crate::time::spin_until(10_000, || {
            v = self.r32(GPHY_OCP);
            v & (1 << 31) != 0
        })
        .then_some(v as u16)
    }

    fn ocp_write(&self, addr: u32, value: u16) {
        self.w32(GPHY_OCP, 1 << 31 | addr << 15 | value as u32);
        crate::time::spin_until(10_000, || self.r32(GPHY_OCP) & (1 << 31) == 0);
    }

    /// A standard MII register of the PHY, whichever way this chip reaches it.
    fn phy_read(&self, reg: u32) -> Option<u16> {
        if is_8168g_or_later(self.xid) {
            return self.ocp_read(OCP_STD_PHY + reg * 2);
        }
        self.w32(PHYAR, (reg & 0x1F) << 16);
        let mut v = 0;
        crate::time::spin_until(10_000, || {
            v = self.r32(PHYAR);
            v & (1 << 31) != 0
        })
        .then_some(v as u16)
    }

    fn phy_write(&self, reg: u32, value: u16) {
        if is_8168g_or_later(self.xid) {
            return self.ocp_write(OCP_STD_PHY + reg * 2, value);
        }
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
        format!("chip XID {:#x}, {}, PHY status {:#04x}, BMSR {:04x?}, partner {:04x?}, MCU {:#04x}, rx errors {}",
            self.xid, self.link_text(), self.r8(PHY_STATUS), self.phy_read(BMSR), self.phy_read(ANLPAR),
            self.r8(MCU), self.rx_errors)
    }
}
