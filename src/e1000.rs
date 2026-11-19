// =============================================================================
// VEILOS: Intel gigabit Ethernet (e1000 / e1000e)
// =============================================================================
//
// Three families with one register core and the same legacy descriptors:
//
//   * classic e1000 (8254x; QEMU's `e1000`, the 82540EM);
//   * e1000e controllers on PCIe (82571-82574, 82583; QEMU's `e1000e`, the
//     82574L);
//   * the LAN controllers built into Intel chipsets ("PCH": 82577/82578,
//     82579, I217, I218, I219). These are what desktops and laptops since
//     about 2010 have, the I219 with Skylake and later. Their PHY is a
//     separate chip, on a firmware-shared bus: before a reset the driver
//     takes the software semaphore (EXTCNF_CTRL.SWFLAG), and if the PHY
//     doesn't answer (left in a low-power or SMBus mode by the firmware or
//     the ME), its power is cycled through LANPHYPC, as Linux does.
//
// 64 receive and 64 transmit descriptors of 2 KiB buffers, in one DMA block
// below 4 GiB. Polled (interrupts masked), from the network stack's poll.

use alloc::{boxed::Box, format, string::String, vec::Vec};
use smoltcp::wire::EthernetAddress;

use crate::virtio_net::PacketBuf;

const CTRL: u64 = 0x0000;
const STATUS: u64 = 0x0008;
const EECD: u64 = 0x0010;
const EERD: u64 = 0x0014;
const CTRL_EXT: u64 = 0x0018;
const MDIC: u64 = 0x0020;
const IMC: u64 = 0x00D8;
const RCTL: u64 = 0x0100;
const TCTL: u64 = 0x0400;
const TIPG: u64 = 0x0410;
const EXTCNF_CTRL: u64 = 0x0F00;
const RDBAL: u64 = 0x2800;
const RDBAH: u64 = 0x2804;
const RDLEN: u64 = 0x2808;
const RDH: u64 = 0x2810;
const RDT: u64 = 0x2818;
const TDBAL: u64 = 0x3800;
const TDBAH: u64 = 0x3804;
const TDLEN: u64 = 0x3808;
const TDH: u64 = 0x3810;
const TDT: u64 = 0x3818;
const MTA: u64 = 0x5200;
const RAL0: u64 = 0x5400;
const RAH0: u64 = 0x5404;

const CTRL_SLU: u32 = 1 << 6;
const CTRL_ASDE: u32 = 1 << 5;
const CTRL_LRST: u32 = 1 << 3;
const CTRL_ILOS: u32 = 1 << 7;
const CTRL_RST: u32 = 1 << 26;
const CTRL_LANPHYPC_OVERRIDE: u32 = 1 << 16;
const CTRL_LANPHYPC_VALUE: u32 = 1 << 17;
const CTRL_PHY_RST: u32 = 1 << 31;
const CTRL_EXT_FORCE_SMBUS: u32 = 1 << 11;
const STATUS_LU: u32 = 1 << 1;
const STATUS_LAN_INIT_DONE: u32 = 1 << 9;
const SWFLAG: u32 = 1 << 5;

const RX_DESC: usize = 64;
const TX_DESC: usize = 64;
const BUF: usize = 2048;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Family {
    Classic,
    Pcie,
    Pch,
}

fn family(device: u16) -> Option<Family> {
    const CLASSIC: &[u16] = &[0x1000, 0x1001, 0x1004, 0x1008, 0x100C, 0x100D, 0x100E, 0x100F, 0x1010, 0x1011,
        0x1012, 0x1015, 0x1016, 0x1017, 0x1018, 0x1019, 0x101A, 0x101D, 0x101E, 0x1026, 0x1027, 0x1028,
        0x1075, 0x1076, 0x1077, 0x1078, 0x1079, 0x107A, 0x107B, 0x107C, 0x108A, 0x1099, 0x10B5];
    const PCIE: &[u16] = &[0x105E, 0x105F, 0x1060, 0x107D, 0x107E, 0x107F, 0x108B, 0x108C, 0x109A, 0x10A4,
        0x10A5, 0x10B9, 0x10BC, 0x10D3, 0x10D5, 0x10D9, 0x10DA, 0x10F6, 0x150C];
    const PCH: &[u16] = &[
        0x10EA, 0x10EB, 0x10EF, 0x10F0, // 82577, 82578
        0x1502, 0x1503, // 82579
        0x153A, 0x153B, // I217
        0x155A, 0x1559, 0x15A0, 0x15A1, 0x15A2, 0x15A3, // I218
        0x156F, 0x1570, 0x15B7, 0x15B8, 0x15B9, 0x15D6, 0x15D7, 0x15D8, 0x15E3, // I219: Skylake/Kaby Lake
        0x15BB, 0x15BC, 0x15BD, 0x15BE, 0x15DF, 0x15E0, 0x15E1, 0x15E2, // I219: Cannon/Ice Lake
        0x0D4C, 0x0D4D, 0x0D4E, 0x0D4F, 0x0D53, 0x0D55, 0x15F4, 0x15F5, 0x15F9, 0x15FA, 0x15FB, 0x15FC, // Comet/Tiger Lake
        0x1A1C, 0x1A1D, 0x1A1E, 0x1A1F, 0x0DC5, 0x0DC6, 0x0DC7, 0x0DC8, // Alder/Raptor Lake
        0x550A, 0x550B, 0x550C, 0x550D, 0x550E, 0x550F, 0x5510, 0x5511, // Meteor Lake
    ];
    if CLASSIC.contains(&device) {
        Some(Family::Classic)
    } else if PCIE.contains(&device) {
        Some(Family::Pcie)
    } else if PCH.contains(&device) {
        Some(Family::Pch)
    } else {
        None
    }
}

/// Intel e1000/e1000e NICs. Bound by `hal::bind`.
pub const DRIVER: crate::hal::Driver = crate::hal::Driver {
    name: "e1000",
    class: crate::hal::DeviceClass::Network,
    matches: |d| d.vendor_id == 0x8086 && family(d.device_id).is_some(),
    probe,
    remove: None,
};

fn probe(dev: &crate::pci::PciDevice) -> Result<(), String> {
    let fam = family(dev.device_id).ok_or("not an e1000")?;
    let bar = dev.bar(0);
    let base = bar.mem_base().ok_or("no memory BAR0")?;
    dev.enable_for_driver();
    map_mmio(base, (bar.size as u64).max(0x20000));
    crate::serial_println!("[E1000] {:02x}:{:02x}.{} {:04x} ({:?}) at {:#x}", dev.bus, dev.device, dev.function, dev.device_id, fam, base);
    let nic = E1000::new(base, fam)?;
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

pub struct E1000 {
    base: u64,
    family: Family,
    mac: EthernetAddress,
    /// RX descriptors, TX descriptors, RX buffers, TX buffers.
    dma_phys: u64,
    dma: *mut u8,
    rx_next: usize,
    tx_next: usize,
    rx_count: u64,
    tx_count: u64,
    link_was_up: bool,
}

// MMIO and DMA memory owned by this NIC, used under the network lock.
unsafe impl Send for E1000 {}

const RX_DESCS_AT: usize = 0;
const TX_DESCS_AT: usize = RX_DESC * 16;
const RX_BUFS_AT: usize = TX_DESCS_AT + TX_DESC * 16;
const TX_BUFS_AT: usize = RX_BUFS_AT + RX_DESC * BUF;
const DMA_BYTES: usize = TX_BUFS_AT + TX_DESC * BUF;

impl E1000 {
    fn r(&self, reg: u64) -> u32 {
        unsafe { ((self.base + reg) as *const u32).read_volatile() }
    }
    fn w(&self, reg: u64, v: u32) {
        unsafe { ((self.base + reg) as *mut u32).write_volatile(v) }
    }
    fn desc(&self, at: usize, i: usize) -> *mut u8 {
        unsafe { self.dma.add(at + i * 16) }
    }

    fn new(base: u64, family: Family) -> Result<Self, String> {
        let (dma_phys, dma) = crate::memory::DmaRegion::new(DMA_BYTES).ok_or("no DMA memory")?.leak();
        let mut nic = Self {
            base, family, mac: EthernetAddress([0; 6]), dma_phys, dma,
            rx_next: 0, tx_next: 0, rx_count: 0, tx_count: 0, link_was_up: false,
        };
        nic.w(IMC, 0xFFFF_FFFF);
        if family == Family::Pch {
            nic.pch_phy_power_up();
        }
        nic.reset();
        nic.mac = nic.read_mac().ok_or("no MAC address (RAL0 empty and no EEPROM)")?;
        nic.setup_rings();
        // Link up: auto speed detection, out of reset and out of loss-of-signal inversion.
        let ctrl = nic.r(CTRL);
        nic.w(CTRL, (ctrl | CTRL_SLU | CTRL_ASDE) & !(CTRL_LRST | CTRL_ILOS | CTRL_PHY_RST));
        let up = crate::time::spin_until(3_000_000, || nic.r(STATUS) & STATUS_LU != 0);
        if !up && family == Family::Pch {
            // Restart auto-negotiation on the PHY (BMCR: AN enable + restart).
            if let Some(bmcr) = nic.mdic_read(0) {
                nic.mdic_write(0, bmcr | 0x1200);
            }
        }
        nic.link_was_up = nic.r(STATUS) & STATUS_LU != 0;
        crate::serial_println!(
            "[E1000] MAC {}, link {}{}",
            nic.mac,
            if nic.link_was_up { "up" } else { "down" },
            if nic.link_was_up { format!(" at {} Mb/s", nic.speed()) } else { String::new() }
        );
        Ok(nic)
    }

    fn speed(&self) -> u32 {
        match (self.r(STATUS) >> 6) & 3 {
            0 => 10,
            1 => 100,
            _ => 1000,
        }
    }

    /// The chipset LAN's PHY must answer on MDIO before the MAC is reset:
    /// out of SMBus mode, and, if it still doesn't, power-cycled through
    /// LANPHYPC (the firmware or ME can leave it in a low-power state).
    fn pch_phy_power_up(&mut self) {
        let ext = self.r(CTRL_EXT);
        if ext & CTRL_EXT_FORCE_SMBUS != 0 {
            self.w(CTRL_EXT, ext & !CTRL_EXT_FORCE_SMBUS);
        }
        if self.mdic_read(2).is_some() {
            return;
        }
        crate::serial_println!("[E1000] PHY doesn't answer: power-cycling it (LANPHYPC)");
        let ctrl = self.r(CTRL);
        self.w(CTRL, (ctrl | CTRL_LANPHYPC_OVERRIDE) & !CTRL_LANPHYPC_VALUE);
        crate::time::delay_ms(1);
        self.w(CTRL, ctrl & !(CTRL_LANPHYPC_OVERRIDE | CTRL_LANPHYPC_VALUE));
        crate::time::delay_ms(50);
        if self.mdic_read(2).is_none() {
            crate::serial_println!("[E1000] WARN: the PHY still doesn't answer");
        }
    }

    fn reset(&mut self) {
        // The chipset LAN shares its PHY with firmware: hold the semaphore.
        let pch = self.family == Family::Pch;
        if pch {
            let got = crate::time::spin_until(1_000_000, || {
                let e = self.r(EXTCNF_CTRL);
                if e & SWFLAG == 0 {
                    self.w(EXTCNF_CTRL, e | SWFLAG);
                }
                self.r(EXTCNF_CTRL) & SWFLAG != 0
            });
            if !got {
                crate::serial_println!("[E1000] WARN: firmware holds the PHY semaphore; resetting anyway");
            }
        }
        self.w(CTRL, self.r(CTRL) | CTRL_RST);
        crate::time::delay_ms(if pch { 20 } else { 2 });
        crate::time::spin_until(100_000, || self.r(CTRL) & CTRL_RST == 0);
        self.w(IMC, 0xFFFF_FFFF);
        if pch {
            // The NVM is read back into the registers; then LAN init is done.
            crate::time::spin_until(10_000, || self.r(EECD) & (1 << 9) != 0);
            crate::time::spin_until(150_000, || self.r(STATUS) & STATUS_LAN_INIT_DONE != 0);
            self.w(STATUS, self.r(STATUS) & !STATUS_LAN_INIT_DONE);
            self.w(EXTCNF_CTRL, self.r(EXTCNF_CTRL) & !SWFLAG);
        }
    }

    /// RAL0/RAH0, which the hardware loads from its NVM at reset; else the
    /// EEPROM through EERD (classic and PCIe parts).
    fn read_mac(&self) -> Option<EthernetAddress> {
        let (lo, hi) = (self.r(RAL0), self.r(RAH0));
        let mac = [lo as u8, (lo >> 8) as u8, (lo >> 16) as u8, (lo >> 24) as u8, hi as u8, (hi >> 8) as u8];
        if hi & (1 << 31) != 0 && mac != [0; 6] && mac != [0xFF; 6] {
            return Some(EthernetAddress(mac));
        }
        if self.family == Family::Pch {
            return None;
        }
        let mut words = [0u16; 3];
        for (i, w) in words.iter_mut().enumerate() {
            let (cmd, done) = if self.family == Family::Classic { ((i as u32) << 8 | 1, 1 << 4) } else { ((i as u32) << 2 | 1, 1 << 1) };
            self.w(EERD, cmd);
            if !crate::time::spin_until(10_000, || self.r(EERD) & done != 0) {
                return None;
            }
            *w = (self.r(EERD) >> 16) as u16;
        }
        let mac = [words[0] as u8, (words[0] >> 8) as u8, words[1] as u8, (words[1] >> 8) as u8, words[2] as u8, (words[2] >> 8) as u8];
        // Receive on it from now on.
        self.w(RAL0, u32::from_le_bytes([mac[0], mac[1], mac[2], mac[3]]));
        self.w(RAH0, u16::from_le_bytes([mac[4], mac[5]]) as u32 | 1 << 31);
        Some(EthernetAddress(mac))
    }

    fn setup_rings(&mut self) {
        for i in 0..128 {
            self.w(MTA + i * 4, 0);
        }
        for i in 0..RX_DESC {
            let d = self.desc(RX_DESCS_AT, i);
            unsafe {
                (d as *mut u64).write_volatile(self.dma_phys + (RX_BUFS_AT + i * BUF) as u64);
                (d.add(8) as *mut u64).write_volatile(0);
            }
        }
        let rx = self.dma_phys + RX_DESCS_AT as u64;
        self.w(RDBAL, rx as u32);
        self.w(RDBAH, (rx >> 32) as u32);
        self.w(RDLEN, (RX_DESC * 16) as u32);
        self.w(RDH, 0);
        self.w(RDT, RX_DESC as u32 - 1);
        // Enable, broadcast, 2 KiB buffers, strip the CRC.
        self.w(RCTL, 1 << 1 | 1 << 15 | 1 << 26);

        let tx = self.dma_phys + TX_DESCS_AT as u64;
        self.w(TDBAL, tx as u32);
        self.w(TDBAH, (tx >> 32) as u32);
        self.w(TDLEN, (TX_DESC * 16) as u32);
        self.w(TDH, 0);
        self.w(TDT, 0);
        // Enable, pad short packets, collision threshold and distance.
        self.w(TCTL, 1 << 1 | 1 << 3 | 0x0F << 4 | 0x3F << 12);
        self.w(TIPG, 0x0060_200A);
    }
}

impl crate::network::Nic for E1000 {
    fn mac(&self) -> EthernetAddress {
        self.mac
    }

    fn link_up(&self) -> bool {
        self.r(STATUS) & STATUS_LU != 0
    }

    fn poll_rx(&mut self, out: &mut Vec<PacketBuf>) {
        let up = self.r(STATUS) & STATUS_LU != 0;
        if up != self.link_was_up {
            self.link_was_up = up;
            crate::serial_println!("[E1000] link {}", if up { format!("up at {} Mb/s", self.speed()) } else { String::from("down") });
        }
        loop {
            let d = self.desc(RX_DESCS_AT, self.rx_next);
            let status = unsafe { d.add(12).read_volatile() };
            if status & 1 == 0 {
                break; // DD: not filled yet
            }
            let len = unsafe { (d.add(8) as *const u16).read_volatile() } as usize;
            let errors = unsafe { d.add(13).read_volatile() };
            // EOP set and no errors: a whole frame in one buffer.
            if status & 2 != 0 && errors == 0 && len > 0 && len <= crate::virtio_net::MTU {
                let mut pkt = PacketBuf::new();
                unsafe { core::ptr::copy_nonoverlapping(self.dma.add(RX_BUFS_AT + self.rx_next * BUF), pkt.data.as_mut_ptr(), len) };
                pkt.len = len;
                out.push(pkt);
                self.rx_count += 1;
            }
            unsafe {
                d.add(12).write_volatile(0);
            }
            // Give the descriptor back.
            self.w(RDT, self.rx_next as u32);
            self.rx_next = (self.rx_next + 1) % RX_DESC;
        }
    }

    fn transmit(&mut self, frame: &[u8]) {
        if frame.is_empty() || frame.len() > BUF {
            return;
        }
        let i = self.tx_next;
        let d = self.desc(TX_DESCS_AT, i);
        // A descriptor still in flight (DD clear after we set it): wait briefly.
        let busy = unsafe { (d.add(8) as *const u64).read_volatile() } != 0 && unsafe { d.add(12).read_volatile() } & 1 == 0;
        if busy && !crate::time::spin_until(10_000, || unsafe { d.add(12).read_volatile() } & 1 != 0) {
            crate::serial_println!("[E1000] TX ring full; frame dropped");
            return;
        }
        unsafe {
            core::ptr::copy_nonoverlapping(frame.as_ptr(), self.dma.add(TX_BUFS_AT + i * BUF), frame.len());
            (d as *mut u64).write_volatile(self.dma_phys + (TX_BUFS_AT + i * BUF) as u64);
            // length, CSO 0, CMD = EOP | IFCS | RS, status 0, CSS 0, special 0
            (d.add(8) as *mut u64).write_volatile(frame.len() as u64 | (0x0B << 24));
        }
        self.tx_next = (i + 1) % TX_DESC;
        self.w(TDT, self.tx_next as u32);
        self.tx_count += 1;
    }

    fn counts(&self) -> (u64, u64) {
        (self.rx_count, self.tx_count)
    }

    fn key(&self) -> u64 {
        self.base
    }

    fn kind(&self) -> &'static str {
        match self.family {
            Family::Classic => "e1000",
            Family::Pcie => "e1000e",
            Family::Pch => "e1000e (chipset LAN)",
        }
    }
}

impl E1000 {
    /// Read a PHY register over MDIO (PHY address 1, or 2 for chipset LAN).
    fn mdic_read(&self, reg: u32) -> Option<u16> {
        let phy = if self.family == Family::Pch { 2 } else { 1 };
        self.w(MDIC, reg << 16 | phy << 21 | 2 << 26);
        let mut v = 0;
        let ok = crate::time::spin_until(10_000, || {
            v = self.r(MDIC);
            v & (1 << 28) != 0
        });
        (ok && v & (1 << 30) == 0).then_some(v as u16)
    }

    fn mdic_write(&self, reg: u32, value: u16) {
        let phy = if self.family == Family::Pch { 2 } else { 1 };
        self.w(MDIC, value as u32 | reg << 16 | phy << 21 | 1 << 26);
        crate::time::spin_until(10_000, || self.r(MDIC) & (1 << 28) != 0);
    }
}
