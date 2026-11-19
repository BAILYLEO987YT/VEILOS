// =============================================================================
// VEILOS: Intel Wireless 7265 (iwlwifi, "MVM" firmware), polled
// =============================================================================
//
// The Wi-Fi card in the test PC: an Intel Dual Band Wireless-AC 7265
// (8086:095A/095B), the 7000 family. The card runs Intel's firmware, which
// does the 802.11 work (scanning, frame timing, retries, encryption); the
// driver loads it, hands it commands through a command queue and gets
// replies, notifications and received frames through one receive queue. The
// layouts and the start-up order follow Linux's iwlwifi (dual GPL/BSD), the
// "pcie" transport for the 7000 family and the "mvm" operation mode:
//
//   * transport: the NIC is prepared (NIC_READY handshake), powered up (APM),
//     the firmware's sections are copied into its SRAM with the service DMA
//     channel, and the CPU is released; it answers with an ALIVE packet.
//     Receive: one ring of 256 descriptors (32-bit, address >> 8) of 4 KiB
//     buffers, with a status word the NIC updates (closed_rb_num). Transmit:
//     TFD rings of 256 128-byte descriptors per queue; the command queue is 0
//     ("DQA" mode), the scheduler is set up after ALIVE.
//   * firmware: the INIT image runs first (calibration: the PHY database it
//     reports is kept), then the runtime image is loaded and given that
//     database back, antenna and PHY configuration, Bluetooth coexistence,
//     DQA mode, the auxiliary station (for scanning), power (always awake)
//     and the regulatory domain ("ZZ": the card decides).
//   * scanning: the "LMAC" scan offload (this firmware has no UMAC scan); the
//     beacons and probe responses come back as received frames.
//
// Everything is polled: interrupts stay masked and the driver reads the
// interrupt cause register and the receive ring itself. MAC_ACCESS_REQ is
// held while the card is up, so it never sleeps under the driver.
//
// The firmware is not built in: it is read from /lib/firmware on the system
// volume (iwlwifi-7265D-29.ucode for the 7265D, the usual one), under Intel's
// redistribution licence (LICENCE.iwlwifi_firmware beside it).
//
// QEMU emulates none of this: it is tested on real hardware, reading the
// log with DMESG WIFI.

use alloc::{collections::VecDeque, format, string::String, vec, vec::Vec};
use spin::Mutex;

// ---- CSR registers (BAR0) ---------------------------------------------------
const CSR_HW_IF_CONFIG_REG: u32 = 0x000;
const CSR_INT_COALESCING: u32 = 0x004;
const CSR_INT: u32 = 0x008;
const CSR_INT_MASK: u32 = 0x00C;
const CSR_FH_INT_STATUS: u32 = 0x010;
const CSR_RESET: u32 = 0x020;
const CSR_GP_CNTRL: u32 = 0x024;
const CSR_HW_REV: u32 = 0x028;
const CSR_GIO_REG: u32 = 0x03C;
const CSR_UCODE_DRV_GP1_CLR: u32 = 0x05C;
const CSR_MBOX_SET_REG: u32 = 0x088;
const CSR_MAC_SHADOW_REG_CTRL: u32 = 0x0A8;
const CSR_GIO_CHICKEN_BITS: u32 = 0x100;
const CSR_DBG_HPET_MEM_REG: u32 = 0x240;
const CSR_DBG_LINK_PWR_MGMT_REG: u32 = 0x250;

const HW_IF_NIC_READY: u32 = 0x0040_0000;
const HW_IF_PREPARE: u32 = 0x0800_0000;
const HW_IF_HAP_WAKE_L1A: u32 = 0x0008_0000;
const HW_IF_RADIO_SI: u32 = 0x0000_0200;
const HW_IF_MAC_SI: u32 = 0x0000_0100;

const INT_HW_ERR: u32 = 1 << 29;
const INT_FH_TX: u32 = 1 << 27;
const INT_SW_ERR: u32 = 1 << 25;
const INT_RF_KILL: u32 = 1 << 7;

const RESET_SW_RESET: u32 = 0x80;
const RESET_MASTER_DISABLED: u32 = 0x100;
const RESET_STOP_MASTER: u32 = 0x200;
const RESET_LINK_PWR_MGMT_DISABLED: u32 = 0x8000_0000;

const GP_MAC_CLOCK_READY: u32 = 0x01;
const GP_INIT_DONE: u32 = 0x04;
const GP_MAC_ACCESS_REQ: u32 = 0x08;
const GP_GOING_TO_SLEEP: u32 = 0x10;
const GP_HW_RF_KILL_SW: u32 = 0x0800_0000;

const UCODE_SW_BIT_RFKILL: u32 = 0x02;
const UCODE_DRV_GP1_BIT_CMD_BLOCKED: u32 = 0x04;

// ---- HBUS: indirect access to the NIC's peripherals and SRAM ---------------
const HBUS_TARG_MEM_RADDR: u32 = 0x40C;
const HBUS_TARG_MEM_WADDR: u32 = 0x410;
const HBUS_TARG_MEM_WDAT: u32 = 0x418;
const HBUS_TARG_MEM_RDAT: u32 = 0x41C;
const HBUS_TARG_PRPH_WADDR: u32 = 0x444;
const HBUS_TARG_PRPH_RADDR: u32 = 0x448;
const HBUS_TARG_PRPH_WDAT: u32 = 0x44C;
const HBUS_TARG_PRPH_RDAT: u32 = 0x450;
const HBUS_TARG_WRPTR: u32 = 0x460;

// ---- FH: the DMA engine ------------------------------------------------------
const FH_KW_MEM_ADDR_REG: u32 = 0x197C;
const FH_RSCSR_CHNL0_STTS_WPTR_REG: u32 = 0x1BC0;
const FH_RSCSR_CHNL0_RBDCB_BASE_REG: u32 = 0x1BC4;
const FH_RSCSR_CHNL0_WPTR: u32 = 0x1BC8;
const FH_RSCSR_CHNL0_RDPTR: u32 = 0x1BCC;
const FH_MEM_RCSR_CHNL0_CONFIG_REG: u32 = 0x1C00;
const FH_MEM_RCSR_CHNL0_RBDCB_WPTR: u32 = 0x1C08;
const FH_MEM_RCSR_CHNL0_FLUSH_RB_REQ: u32 = 0x1C10;
const FH_MEM_RSSR_RX_STATUS_REG: u32 = 0x1C44;
const FH_TSSR_TX_STATUS_REG: u32 = 0x1EB0;
const FH_TX_CHICKEN_BITS_REG: u32 = 0x1E98;
const FH_SRVC_CHNL: u32 = 9;
const FH_SRVC_CHNL_SRAM_ADDR_REG: u32 = 0x19C8; // channel 9
const FH_TFDIB_CTRL0_REG: u32 = 0x1900 + 8 * FH_SRVC_CHNL;
const FH_TFDIB_CTRL1_REG: u32 = 0x1904 + 8 * FH_SRVC_CHNL;
const fn fh_tcsr_config(ch: u32) -> u32 {
    0x1D00 + 0x20 * ch
}
const fn fh_tcsr_buf_sts(ch: u32) -> u32 {
    0x1D08 + 0x20 * ch
}
const fn fh_cbbc_queue(q: u32) -> u32 {
    if q < 16 {
        0x19D0 + 4 * q
    } else if q < 20 {
        0x1BF0 + 4 * (q - 16)
    } else {
        0x1B20 + 4 * (q - 20)
    }
}
const FH_TX_CONFIG_DMA_ENABLE: u32 = 0x8000_0000;
const FH_TX_CONFIG_CREDIT_ENABLE: u32 = 0x8;
const FH_TX_CONFIG_CIRQ_HOST_ENDTFD: u32 = 0x0010_0000;
const FH_TX_CHICKEN_SCD_AUTO_RETRY_EN: u32 = 0x2;
const FH_MEM_TB_MAX_LENGTH: usize = 0x20000;

// ---- PRPH registers (through HBUS) ------------------------------------------
const APMG_CLK_EN_REG: u32 = 0x3004;
const APMG_CLK_DIS_REG: u32 = 0x3008;
const APMG_PS_CTRL_REG: u32 = 0x300C;
const APMG_PCIDEV_STT_REG: u32 = 0x3010;
const APMG_RTC_INT_STT_REG: u32 = 0x301C;
const APMG_CLK_VAL_DMA_CLK_RQT: u32 = 0x200;
const APMG_PCIDEV_STT_VAL_L1_ACT_DIS: u32 = 0x800;
const APMG_PS_CTRL_EARLY_PWR_OFF_RESET_DIS: u32 = 0x0040_0000;
const APMG_PS_CTRL_MSK_PWR_SRC: u32 = 0x0300_0000;
const APMG_RTC_INT_STT_RFKILL: u32 = 0x1000_0000;
const LMPM_CHICK: u32 = 0xA01FF8;
const SCD_BASE: u32 = 0xA02C00;
const SCD_SRAM_BASE_ADDR: u32 = SCD_BASE;
const SCD_DRAM_BASE_ADDR: u32 = SCD_BASE + 0x8;
const SCD_TXFACT: u32 = SCD_BASE + 0x10;
const SCD_QUEUECHAIN_SEL: u32 = SCD_BASE + 0xE8;
const SCD_GP_CTRL: u32 = SCD_BASE + 0x1A8;
const SCD_AGGR_SEL: u32 = SCD_BASE + 0x248;
const SCD_EN_CTRL: u32 = SCD_BASE + 0x254;
const fn scd_queue_rdptr(q: u32) -> u32 {
    if q < 20 { SCD_BASE + 0x68 + q * 4 } else { SCD_BASE + 0x2B4 + q * 4 }
}
const fn scd_queue_status_bits(q: u32) -> u32 {
    if q < 20 { SCD_BASE + 0x10C + q * 4 } else { SCD_BASE + 0x334 + q * 4 }
}
// SCD memory (in the NIC's SRAM, at the address SCD_SRAM_BASE_ADDR gives).
const SCD_CONTEXT_MEM_LOWER_BOUND: u32 = 0x600;
const SCD_TRANS_TBL_MEM_LOWER_BOUND: u32 = 0x7E0;
const SCD_GP_CTRL_AUTO_ACTIVE_MODE: u32 = 1 << 18;
const SCD_GP_CTRL_ENABLE_31_QUEUES: u32 = 1 << 0;

// ---- Firmware ------------------------------------------------------------------
const UCODE_ALIVE_NTFY: u8 = 0x01;
const REPLY_ERROR: u8 = 0x02;
const INIT_COMPLETE_NOTIF: u8 = 0x04;
const ADD_STA: u8 = 0x18;
const SCD_QUEUE_CFG: u8 = 0x1D;
const MAC_CONTEXT_CMD: u8 = 0x28;
const SCAN_OFFLOAD_REQUEST_CMD: u8 = 0x51;
const SCAN_OFFLOAD_COMPLETE: u8 = 0x6D;
const PHY_CONFIGURATION_CMD: u8 = 0x6A;
const CALIB_RES_NOTIF_PHY_DB: u8 = 0x6B;
const PHY_DB_CMD: u8 = 0x6C;
const POWER_TABLE_CMD: u8 = 0x77;
const NVM_ACCESS_CMD: u8 = 0x88;
const TX_ANT_CONFIGURATION_CMD: u8 = 0x98;
const BT_CONFIG: u8 = 0x9B;
const REPLY_RX_PHY_CMD: u8 = 0xC0;
const REPLY_RX_MPDU_CMD: u8 = 0xC1;
const MCC_UPDATE_CMD: u8 = 0xC8;
const SCAN_ITERATION_COMPLETE: u8 = 0xE7;
const DATA_PATH_GROUP: u8 = 0x5;
const DQA_ENABLE_CMD: u8 = 0x0;

const CMD_QUEUE: usize = 0; // IWL_MVM_DQA_CMD_QUEUE
const AUX_QUEUE: usize = 1; // IWL_MVM_DQA_AUX_QUEUE
const TX_FIFO_MCAST: u8 = 5;
const TX_FIFO_CMD: u32 = 7;
const MAC_INDEX_AUX: u32 = 4;
const AUX_STA_ID: u8 = 0;
const MAC_ID: u32 = 0;

const TFD_SIZE: usize = 128;
const TFD_QUEUE_SIZE: usize = 256;
const CMD_SLOTS: usize = 32;
const CMD_BUF: usize = 8192;
const RX_QUEUE_SIZE: usize = 256;
const RB_SIZE: usize = 4096;
const BC_TABLE_BYTES: usize = 640; // 320 u16 entries per queue
const NUM_QUEUES: usize = 31;
const FIRST_TB_SIZE: usize = 20;

/// PCI IDs: the 7265 (and the 3165, same family and firmware naming).
pub const DRIVER: crate::hal::Driver = crate::hal::Driver {
    name: "iwlwifi",
    class: crate::hal::DeviceClass::Network,
    matches: |d| d.vendor_id == 0x8086 && matches!(d.device_id, 0x095A | 0x095B | 0x3165 | 0x3166),
    probe,
    remove: None,
};

/// The one card, once probed.
pub static WIFI: Mutex<Option<Iwl>> = Mutex::new(None);

macro_rules! wlog {
    ($($arg:tt)*) => { crate::serial_println!("[WIFI] {}", format_args!($($arg)*)) };
}

fn probe(dev: &crate::pci::PciDevice) -> Result<(), String> {
    let bar = dev.bar(0);
    let base = bar.mem_base().filter(|&b| b != 0).ok_or("no memory BAR0")?;
    dev.enable_for_driver();
    // As Linux: no PCI retry timeout (config 0x41), else the card's RX
    // DMA can stall the CPU's C3 state.
    dev.address().write_u8(0x41, 0);
    map_mmio(base, bar.size.max(0x2000));
    let subsystem = dev.address().read_u16(0x2E);
    let iwl = Iwl::new(base, dev.device_id, subsystem)?;
    wlog!("{:02x}:{:02x}.{} {:04x}:{:04x} sub {:04x} at {:#x}, HW rev {:#x} ({})",
        dev.bus, dev.device, dev.function, dev.vendor_id, dev.device_id, subsystem, base,
        iwl.hw_rev, iwl.model());
    *WIFI.lock() = Some(iwl);
    // Bring the firmware up now, so DMESG WIFI shows how far it got; a
    // failure leaves the card for WIFI UP to retry.
    let mut guard = WIFI.lock();
    if let Some(iwl) = guard.as_mut() {
        if let Err(e) = iwl.up() {
            wlog!("not up: {}", e);
        }
    }
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

/// DMA memory below 4 GiB, kept for the driver's life: (physical, CPU).
#[derive(Clone, Copy)]
struct Dma {
    phys: u64,
    ptr: *mut u8,
}

impl Dma {
    fn new(bytes: usize) -> Result<Self, String> {
        let (phys, ptr) = crate::memory::DmaRegion::new(bytes).ok_or("out of DMA memory")?.leak();
        Ok(Self { phys, ptr })
    }
    fn at(&self, off: usize) -> *mut u8 {
        unsafe { self.ptr.add(off) }
    }
    fn w32(&self, off: usize, v: u32) {
        unsafe { (self.at(off) as *mut u32).write_volatile(v) }
    }
    fn r16(&self, off: usize) -> u16 {
        unsafe { (self.at(off) as *const u16).read_volatile() }
    }
    fn r32(&self, off: usize) -> u32 {
        unsafe { (self.at(off) as *const u32).read_volatile() }
    }
}

// ---- Firmware file -------------------------------------------------------------

struct Section {
    offset: u32,
    data: Vec<u8>,
}

#[derive(Default)]
struct FwImage {
    sections: Vec<Section>,
}

#[derive(Default)]
struct Firmware {
    name: String,
    version: String,
    init: FwImage,
    regular: FwImage,
    /// (flow trigger, event trigger) per image: [regular, init].
    calib: [(u32, u32); 2],
    phy_config: u32,
    n_scan_channels: usize,
    api: [u32; 4],
    capa: [u32; 4],
}

fn le16(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
fn le32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

impl Firmware {
    /// The TLV format (all firmware for the 7000 family and later).
    fn parse(name: &str, d: &[u8]) -> Result<Self, String> {
        if d.len() < 88 || le32(d, 0) != 0 || le32(d, 4) != 0x0A4C_5749 {
            return Err(format!("{}: not a TLV firmware file", name));
        }
        let human = &d[8..72];
        let end = human.iter().position(|&c| c == 0).unwrap_or(64);
        let mut fw = Firmware {
            name: String::from(name),
            version: String::from_utf8_lossy(&human[..end]).into_owned(),
            n_scan_channels: 40,
            ..Default::default()
        };
        let mut o = 88;
        while o + 8 <= d.len() {
            let (t, l) = (le32(d, o), le32(d, o + 4) as usize);
            let p = d.get(o + 8..o + 8 + l).ok_or("truncated TLV")?;
            match t {
                19 | 20 if l >= 4 => {
                    let img = if t == 19 { &mut fw.regular } else { &mut fw.init };
                    img.sections.push(Section { offset: le32(p, 0), data: p[4..].to_vec() });
                }
                22 if l >= 12 => {
                    let ty = le32(p, 0) as usize;
                    if ty < 2 {
                        fw.calib[ty] = (le32(p, 4), le32(p, 8));
                    }
                }
                23 if l >= 4 => fw.phy_config = le32(p, 0),
                31 if l >= 4 => fw.n_scan_channels = le32(p, 0) as usize,
                29 | 30 if l >= 8 => {
                    let (i, v) = (le32(p, 0) as usize, le32(p, 4));
                    if i < 4 {
                        if t == 29 { fw.api[i] = v } else { fw.capa[i] = v }
                    }
                }
                _ => {}
            }
            o += 8 + ((l + 3) & !3);
        }
        if fw.init.sections.is_empty() || fw.regular.sections.is_empty() {
            return Err(format!("{}: no INIT or runtime image", name));
        }
        Ok(fw)
    }

    fn capa(&self, bit: usize) -> bool {
        self.capa.get(bit / 32).is_some_and(|w| w >> (bit % 32) & 1 != 0)
    }
}

// ---- Packets from the card -----------------------------------------------------

/// One packet from the receive ring: a reply, a notification or a frame.
pub struct Packet {
    pub cmd: u8,
    pub group: u8,
    pub seq: u16,
    pub data: Vec<u8>,
}

impl Packet {
    fn is_reply_to(&self, seq: u16) -> bool {
        self.seq & 0x8000 == 0 && self.seq == seq
    }
}

/// A network heard while scanning.
#[derive(Clone)]
pub struct Bss {
    pub bssid: [u8; 6],
    pub ssid: Vec<u8>,
    pub channel: u8,
    pub signal: i8,
    pub privacy: bool,
    pub rsn: Option<Vec<u8>>,
    pub wpa: bool,
    pub beacon_int: u16,
    pub capability: u16,
    pub rates: Vec<u8>,
    pub last_seen: u64,
}

impl Bss {
    pub fn security(&self) -> &'static str {
        match (&self.rsn, self.wpa, self.privacy) {
            (Some(rsn), _, _) if rsn_has_akm(rsn, 2) => "WPA2-PSK",
            (Some(rsn), _, _) if rsn_has_akm(rsn, 8) => "WPA3-SAE",
            (Some(_), _, _) => "WPA2-EAP",
            (None, true, _) => "WPA1",
            (None, false, true) => "WEP",
            _ => "open",
        }
    }

    pub fn ssid_text(&self) -> String {
        if self.ssid.is_empty() || self.ssid.iter().all(|&c| c == 0) {
            return String::from("(hidden)");
        }
        self.ssid.iter().map(|&c| if (0x20..0x7F).contains(&c) { c as char } else { '?' }).collect()
    }
}

/// An RSN element's AKM suite list contains 00-0F-AC:`akm`.
pub fn rsn_has_akm(rsn: &[u8], akm: u8) -> bool {
    // version(2) group(4) pairwise count(2) + 4n, AKM count(2) + 4m
    if rsn.len() < 8 {
        return false;
    }
    let np = le16(rsn, 6) as usize;
    let at = 8 + 4 * np;
    if rsn.len() < at + 2 {
        return false;
    }
    let na = le16(rsn, at) as usize;
    (0..na).any(|i| rsn.get(at + 2 + 4 * i..at + 6 + 4 * i) == Some(&[0x00, 0x0F, 0xAC, akm][..]))
}

// ---- The card ---------------------------------------------------------------------

struct TxQueue {
    tfds: Dma,
    /// Per-slot buffers (commands: CMD_SLOTS of CMD_BUF; data: 256 of 4 KiB).
    bufs: Dma,
    buf_size: usize,
    #[allow(dead_code)] // data queues (stage 3)
    slots: usize,
    write: usize,
    read: usize,
}

pub struct Iwl {
    base: u64,
    device_id: u16,
    subsystem: u16,
    pub hw_rev: u32,
    fw: Option<Firmware>,
    rx_bd: Dma,
    rx_stts: Dma,
    rx_bufs: Vec<Dma>,
    rx_read: usize,
    rx_write: usize,
    kw: Dma,
    bc_tbls: Dma,
    load_buf: Option<Dma>,
    txq: Vec<Option<TxQueue>>,
    scd_base: u32,
    /// Notifications that arrived while waiting for something else.
    pending: VecDeque<Packet>,
    phy_db: Vec<(u16, u16, Vec<u8>)>, // (type, channel group, data)
    error_table: u32,
    pub mac: [u8; 6],
    valid_tx_ant: u8,
    valid_rx_ant: u8,
    pub nvm_version: u16,
    pub running: bool,
    pub state: String,
    pub scan_results: Vec<Bss>,
    last_phy: Vec<u8>,
    pub rx_frames: u64,
}

// MMIO and DMA owned by the driver, used under WIFI's lock.
unsafe impl Send for Iwl {}

impl Iwl {
    fn new(base: u64, device_id: u16, subsystem: u16) -> Result<Self, String> {
        let rx_bd = Dma::new(RX_QUEUE_SIZE * 4)?;
        let rx_stts = Dma::new(16)?;
        let mut rx_bufs = Vec::with_capacity(RX_QUEUE_SIZE);
        for _ in 0..RX_QUEUE_SIZE {
            rx_bufs.push(Dma::new(RB_SIZE)?);
        }
        let mut iwl = Self {
            base, device_id, subsystem, hw_rev: 0, fw: None,
            rx_bd, rx_stts, rx_bufs, rx_read: 0, rx_write: 0,
            kw: Dma::new(4096)?,
            bc_tbls: Dma::new(NUM_QUEUES * BC_TABLE_BYTES)?,
            load_buf: None,
            txq: (0..NUM_QUEUES).map(|_| None).collect(),
            scd_base: 0,
            pending: VecDeque::new(),
            phy_db: Vec::new(),
            error_table: 0,
            mac: [0; 6],
            valid_tx_ant: 0,
            valid_rx_ant: 0,
            nvm_version: 0,
            running: false,
            state: String::from("found"),
            scan_results: Vec::new(),
            last_phy: Vec::new(),
            rx_frames: 0,
        };
        iwl.hw_rev = iwl.r32(CSR_HW_REV);
        if iwl.hw_rev == 0xFFFF_FFFF {
            return Err(String::from("card not responding (HW rev all ones)"));
        }
        Ok(iwl)
    }

    /// 7265D: the same PCI IDs as the 7265, a later silicon (HW rev type 0x210).
    fn is_7265d(&self) -> bool {
        self.hw_rev & 0xFFF0 == 0x210
    }

    pub fn model(&self) -> &'static str {
        match self.device_id {
            0x3165 | 0x3166 => "Wireless-AC 3165",
            _ if self.is_7265d() => "Wireless-AC 7265 (7265D)",
            _ => "Wireless-AC 7265",
        }
    }

    fn firmware_names(&self) -> &'static [&'static str] {
        if self.is_7265d() || matches!(self.device_id, 0x3165 | 0x3166) {
            &["iwlwifi-7265D-29.ucode", "iwlwifi-7265D-27.ucode", "iwlwifi-7265D-22.ucode"]
        } else {
            &["iwlwifi-7265-17.ucode"]
        }
    }

    // ---- register access -----------------------------------------------------
    fn r32(&self, reg: u32) -> u32 {
        unsafe { ((self.base + reg as u64) as *const u32).read_volatile() }
    }
    fn w32(&self, reg: u32, v: u32) {
        unsafe { ((self.base + reg as u64) as *mut u32).write_volatile(v) }
    }
    fn w8(&self, reg: u32, v: u8) {
        unsafe { ((self.base + reg as u64) as *mut u8).write_volatile(v) }
    }
    fn set_bit(&self, reg: u32, mask: u32) {
        self.w32(reg, self.r32(reg) | mask);
    }
    fn clear_bit(&self, reg: u32, mask: u32) {
        self.w32(reg, self.r32(reg) & !mask);
    }
    fn poll_bit(&self, reg: u32, bits: u32, mask: u32, timeout_us: u64) -> bool {
        crate::time::spin_until(timeout_us, || self.r32(reg) & mask == bits & mask)
    }
    fn read_prph(&self, reg: u32) -> u32 {
        self.w32(HBUS_TARG_PRPH_RADDR, (reg & 0x000F_FFFF) | 3 << 24);
        self.r32(HBUS_TARG_PRPH_RDAT)
    }
    fn write_prph(&self, reg: u32, v: u32) {
        self.w32(HBUS_TARG_PRPH_WADDR, (reg & 0x000F_FFFF) | 3 << 24);
        self.w32(HBUS_TARG_PRPH_WDAT, v);
    }
    fn set_bits_prph(&self, reg: u32, mask: u32) {
        self.write_prph(reg, self.read_prph(reg) | mask);
    }
    fn clear_bits_prph(&self, reg: u32, mask: u32) {
        self.write_prph(reg, self.read_prph(reg) & !mask);
    }
    fn read_mem32(&self, addr: u32) -> u32 {
        self.w32(HBUS_TARG_MEM_RADDR, addr);
        self.r32(HBUS_TARG_MEM_RDAT)
    }
    fn write_mem32(&self, addr: u32, v: u32) {
        self.w32(HBUS_TARG_MEM_WADDR, addr);
        self.w32(HBUS_TARG_MEM_WDAT, v);
    }

    /// Keep the card's MAC awake for register and SRAM access.
    fn grab_nic_access(&self) -> bool {
        self.set_bit(CSR_GP_CNTRL, GP_MAC_ACCESS_REQ);
        let ok = self.poll_bit(CSR_GP_CNTRL, GP_MAC_CLOCK_READY, GP_MAC_CLOCK_READY | GP_GOING_TO_SLEEP, 15_000);
        if !ok {
            wlog!("no NIC access (GP_CNTRL {:#010x})", self.r32(CSR_GP_CNTRL));
        }
        ok
    }

    // ---- power-up ---------------------------------------------------------------

    fn set_hw_ready(&self) -> bool {
        self.set_bit(CSR_HW_IF_CONFIG_REG, HW_IF_NIC_READY);
        let ok = self.poll_bit(CSR_HW_IF_CONFIG_REG, HW_IF_NIC_READY, HW_IF_NIC_READY, 50);
        if ok {
            self.set_bit(CSR_MBOX_SET_REG, 1 << 5); // OS_ALIVE
        }
        ok
    }

    /// The NIC_READY handshake with the card (and the platform's ME).
    fn prepare_card_hw(&self) -> Result<(), String> {
        if self.set_hw_ready() {
            return Ok(());
        }
        self.set_bit(CSR_DBG_LINK_PWR_MGMT_REG, RESET_LINK_PWR_MGMT_DISABLED);
        crate::time::delay_ms(1);
        for _ in 0..10 {
            self.set_bit(CSR_HW_IF_CONFIG_REG, HW_IF_PREPARE);
            if crate::time::spin_until(150_000, || self.set_hw_ready()) {
                return Ok(());
            }
            crate::time::delay_ms(25);
        }
        Err(format!("card not ready (HW_IF_CONFIG {:#010x})", self.r32(CSR_HW_IF_CONFIG_REG)))
    }

    fn sw_reset(&self) {
        self.set_bit(CSR_RESET, RESET_SW_RESET);
        crate::time::delay_ms(6);
    }

    /// Basic power-up (APM): clocks on, the MAC out of its uninitialised state.
    fn apm_init(&self) -> Result<(), String> {
        self.set_bit(CSR_GIO_CHICKEN_BITS, 0x2000_0000); // no L0S exit timer
        self.set_bit(CSR_GIO_CHICKEN_BITS, 0x0080_0000); // L1A without L0S
        self.set_bit(CSR_DBG_HPET_MEM_REG, 0xFFFF_0000);
        self.set_bit(CSR_HW_IF_CONFIG_REG, HW_IF_HAP_WAKE_L1A);
        self.set_bit(CSR_GIO_REG, 0x2); // L0S disabled
        self.set_bit(CSR_GP_CNTRL, GP_INIT_DONE);
        if !self.poll_bit(CSR_GP_CNTRL, GP_MAC_CLOCK_READY, GP_MAC_CLOCK_READY, 25_000) {
            return Err(format!("MAC clock not ready (GP_CNTRL {:#010x})", self.r32(CSR_GP_CNTRL)));
        }
        if !self.grab_nic_access() {
            return Err(String::from("no NIC access after power-up"));
        }
        self.write_prph(APMG_CLK_EN_REG, APMG_CLK_VAL_DMA_CLK_RQT);
        crate::time::delay_us(20);
        self.set_bits_prph(APMG_PCIDEV_STT_REG, APMG_PCIDEV_STT_VAL_L1_ACT_DIS);
        self.write_prph(APMG_RTC_INT_STT_REG, APMG_RTC_INT_STT_RFKILL);
        Ok(())
    }

    fn disable_interrupts(&self) {
        self.w32(CSR_INT_MASK, 0);
        self.w32(CSR_INT, 0xFFFF_FFFF);
        self.w32(CSR_FH_INT_STATUS, 0xFFFF_FFFF);
    }

    /// The hardware RF-kill switch (laptops' airplane switch; a desktop
    /// card normally has none, so this is "on").
    pub fn rf_kill(&self) -> bool {
        self.r32(CSR_GP_CNTRL) & GP_HW_RF_KILL_SW == 0
    }

    fn start_hw(&self) -> Result<(), String> {
        self.prepare_card_hw()?;
        self.sw_reset();
        self.prepare_card_hw()?;
        self.apm_init()?;
        self.disable_interrupts();
        Ok(())
    }

    fn stop_device(&mut self) {
        self.running = false;
        self.disable_interrupts();
        // TX: FIFOs and DMA channels off.
        self.write_prph(SCD_TXFACT, 0);
        for ch in 0..8 {
            self.w32(fh_tcsr_config(ch), 0);
        }
        crate::time::spin_until(5_000, || self.r32(FH_TSSR_TX_STATUS_REG) & 0x00FF_0000 == 0x00FF_0000);
        // RX off.
        self.w32(FH_MEM_RCSR_CHNL0_CONFIG_REG, 0);
        crate::time::spin_until(1_000, || self.r32(FH_MEM_RSSR_RX_STATUS_REG) & (1 << 24) != 0);
        self.write_prph(APMG_CLK_DIS_REG, APMG_CLK_VAL_DMA_CLK_RQT);
        crate::time::delay_us(5);
        self.clear_bit(CSR_GP_CNTRL, GP_MAC_ACCESS_REQ);
        // Stop bus mastering, reset, back to uninitialised.
        self.set_bit(CSR_RESET, RESET_STOP_MASTER);
        self.poll_bit(CSR_RESET, RESET_MASTER_DISABLED, RESET_MASTER_DISABLED, 100);
        self.sw_reset();
        self.clear_bit(CSR_GP_CNTRL, GP_INIT_DONE);
        self.sw_reset();
        let _ = self.prepare_card_hw();
        self.disable_interrupts();
        self.pending.clear();
    }

    /// The card's configuration register from its revision and the
    /// firmware's radio description (Linux: iwl_mvm_nic_config).
    fn nic_config(&self) {
        let phy = self.fw.as_ref().map_or(0, |f| f.phy_config);
        let (ty, step, dash) = (phy & 3, phy >> 2 & 3, phy >> 4 & 3);
        let val = (self.hw_rev & 0xF) | ty << 10 | step << 14 | dash << 12 | HW_IF_RADIO_SI;
        let mask = 0xF | 0xC00 | 0xC000 | 0x3000 | HW_IF_RADIO_SI | HW_IF_MAC_SI | 0x200;
        let v = self.r32(CSR_HW_IF_CONFIG_REG);
        self.w32(CSR_HW_IF_CONFIG_REG, (v & !mask) | val);
        let ps = self.read_prph(APMG_PS_CTRL_REG);
        self.write_prph(APMG_PS_CTRL_REG, (ps & !APMG_PS_CTRL_EARLY_PWR_OFF_RESET_DIS) | APMG_PS_CTRL_EARLY_PWR_OFF_RESET_DIS);
    }

    // ---- queues ----------------------------------------------------------------

    fn rx_init(&mut self) {
        for i in 0..RX_QUEUE_SIZE {
            self.rx_bd.w32(i * 4, (self.rx_bufs[i].phys >> 8) as u32);
            unsafe { core::ptr::write_bytes(self.rx_bufs[i].ptr, 0, RB_SIZE) };
        }
        unsafe { core::ptr::write_bytes(self.rx_stts.ptr, 0, 16) };
        self.rx_read = 0;
        self.rx_write = RX_QUEUE_SIZE - 1;
        self.w32(FH_MEM_RCSR_CHNL0_CONFIG_REG, 0);
        self.w32(FH_MEM_RCSR_CHNL0_RBDCB_WPTR, 0);
        self.w32(FH_MEM_RCSR_CHNL0_FLUSH_RB_REQ, 0);
        self.w32(FH_RSCSR_CHNL0_RDPTR, 0);
        self.w32(FH_RSCSR_CHNL0_WPTR, 0);
        self.w32(FH_RSCSR_CHNL0_RBDCB_BASE_REG, (self.rx_bd.phys >> 8) as u32);
        self.w32(FH_RSCSR_CHNL0_STTS_WPTR_REG, (self.rx_stts.phys >> 4) as u32);
        // Enabled, ignore RXF empty, interrupts to host, 4 KiB buffers,
        // RB timeout 0x11, 256 descriptors (log2 8).
        self.w32(FH_MEM_RCSR_CHNL0_CONFIG_REG, 0x8000_0000 | 0x4 | 0x1000 | 0x11 << 4 | 8 << 20);
        self.w8(CSR_INT_COALESCING, 0x40);
        self.w32(FH_RSCSR_CHNL0_WPTR, (self.rx_write & !7) as u32);
    }

    fn tx_init(&mut self) -> Result<(), String> {
        self.write_prph(SCD_TXFACT, 0);
        self.w32(FH_KW_MEM_ADDR_REG, (self.kw.phys >> 4) as u32);
        for q in [CMD_QUEUE, AUX_QUEUE] {
            if self.txq[q].is_none() {
                let (slots, buf_size) = if q == CMD_QUEUE { (CMD_SLOTS, CMD_BUF) } else { (1, 4096) };
                self.txq[q] = Some(TxQueue {
                    tfds: Dma::new(TFD_QUEUE_SIZE * TFD_SIZE)?,
                    bufs: Dma::new(slots * buf_size)?,
                    buf_size, slots, write: 0, read: 0,
                });
            }
        }
        for q in 0..NUM_QUEUES {
            let phys = match &mut self.txq[q] {
                Some(t) => {
                    unsafe { core::ptr::write_bytes(t.tfds.ptr, 0, TFD_QUEUE_SIZE * TFD_SIZE) };
                    t.write = 0;
                    t.read = 0;
                    t.tfds.phys
                }
                None => 0,
            };
            self.w32(fh_cbbc_queue(q as u32), (phys >> 8) as u32);
        }
        self.set_bits_prph(SCD_GP_CTRL, SCD_GP_CTRL_AUTO_ACTIVE_MODE | SCD_GP_CTRL_ENABLE_31_QUEUES);
        Ok(())
    }

    /// After ALIVE: the scheduler, and the command queue on its FIFO.
    fn tx_start(&mut self) {
        self.scd_base = self.read_prph(SCD_SRAM_BASE_ADDR);
        let clear_end = (SCD_TRANS_TBL_MEM_LOWER_BOUND + NUM_QUEUES as u32 * 2) & 0xFFFC;
        let mut a = SCD_CONTEXT_MEM_LOWER_BOUND;
        while a < clear_end {
            self.write_mem32(self.scd_base + a, 0);
            a += 4;
        }
        self.write_prph(SCD_DRAM_BASE_ADDR, (self.bc_tbls.phys >> 10) as u32);
        // The command queue: inactive, no aggregation, pointers 0, window 64,
        // active on the command FIFO.
        let q = CMD_QUEUE as u32;
        self.write_prph(scd_queue_status_bits(q), 1 << 19);
        self.clear_bits_prph(SCD_AGGR_SEL, 1 << q);
        self.w32(HBUS_TARG_WRPTR, q << 8);
        self.write_prph(scd_queue_rdptr(q), 0);
        self.write_mem32(self.scd_base + SCD_CONTEXT_MEM_LOWER_BOUND + q * 8, 0);
        self.write_mem32(self.scd_base + SCD_CONTEXT_MEM_LOWER_BOUND + q * 8 + 4, 64 | 64 << 16);
        self.write_prph(scd_queue_status_bits(q), 1 << 3 | TX_FIFO_CMD | 1 << 4 | 0x017F_0000);
        self.write_prph(SCD_TXFACT, 0xFF);
        for ch in 0..8 {
            self.w32(fh_tcsr_config(ch), FH_TX_CONFIG_DMA_ENABLE | FH_TX_CONFIG_CREDIT_ENABLE);
        }
        let v = self.r32(FH_TX_CHICKEN_BITS_REG);
        self.w32(FH_TX_CHICKEN_BITS_REG, v | FH_TX_CHICKEN_SCD_AUTO_RETRY_EN);
        self.clear_bits_prph(APMG_PCIDEV_STT_REG, APMG_PCIDEV_STT_VAL_L1_ACT_DIS);
        let _ = SCD_QUEUECHAIN_SEL;
        let _ = SCD_EN_CTRL;
    }

    /// A data queue the firmware will schedule (DQA: the driver sets its
    /// pointers, SCD_QUEUE_CFG maps it to a station and FIFO).
    fn enable_txq(&mut self, q: usize, sta_id: u8, tid: u8, fifo: u8) -> Result<(), String> {
        let t = self.txq[q].as_mut().ok_or("queue not allocated")?;
        t.read = 0;
        t.write = 0;
        self.w32(HBUS_TARG_WRPTR, (q as u32) << 8);
        let cmd = [0, sta_id, tid, q as u8, 1, 0, fifo, 64, 0, 0, 0, 0];
        self.send_cmd(SCD_QUEUE_CFG, 0, &cmd)?;
        Ok(())
    }

    // ---- firmware load -----------------------------------------------------------

    fn load_section(&mut self, sec: &Section) -> Result<(), String> {
        if self.load_buf.is_none() {
            self.load_buf = Some(Dma::new(FH_MEM_TB_MAX_LENGTH)?);
        }
        let buf = self.load_buf.unwrap();
        for (i, chunk) in sec.data.chunks(FH_MEM_TB_MAX_LENGTH).enumerate() {
            let dst = sec.offset + (i * FH_MEM_TB_MAX_LENGTH) as u32;
            let extended = (0x40000..=0x57FFF).contains(&dst);
            if extended {
                self.set_bits_prph(LMPM_CHICK, 1);
            }
            unsafe { core::ptr::copy_nonoverlapping(chunk.as_ptr(), buf.ptr, chunk.len()) };
            self.w32(CSR_INT, INT_FH_TX);
            self.w32(CSR_FH_INT_STATUS, 0xFFFF_FFFF);
            self.w32(fh_tcsr_config(FH_SRVC_CHNL), 0); // pause
            self.w32(FH_SRVC_CHNL_SRAM_ADDR_REG, dst);
            self.w32(FH_TFDIB_CTRL0_REG, buf.phys as u32);
            self.w32(FH_TFDIB_CTRL1_REG, ((buf.phys >> 32) as u32) << 28 | chunk.len() as u32);
            self.w32(fh_tcsr_buf_sts(FH_SRVC_CHNL), 1 << 20 | 1 << 12 | 3);
            self.w32(fh_tcsr_config(FH_SRVC_CHNL),
                FH_TX_CONFIG_DMA_ENABLE | FH_TX_CONFIG_CIRQ_HOST_ENDTFD);
            let done = crate::time::spin_until(5_000_000, || {
                self.r32(CSR_INT) & INT_FH_TX != 0 || self.r32(CSR_FH_INT_STATUS) & 0x3 != 0
            });
            if extended {
                self.clear_bits_prph(LMPM_CHICK, 1);
            }
            if !done {
                return Err(format!("firmware chunk to {:#x} ({} bytes) not acknowledged", dst, chunk.len()));
            }
            self.w32(CSR_INT, INT_FH_TX);
            self.w32(CSR_FH_INT_STATUS, 0xFFFF_FFFF);
        }
        Ok(())
    }

    /// Load an image and start the card's CPU; then wait for ALIVE.
    fn start_fw(&mut self, init: bool) -> Result<(), String> {
        self.prepare_card_hw()?;
        self.disable_interrupts();
        self.w32(CSR_UCODE_DRV_GP1_CLR, UCODE_SW_BIT_RFKILL);
        self.w32(CSR_UCODE_DRV_GP1_CLR, UCODE_DRV_GP1_BIT_CMD_BLOCKED);
        self.w32(CSR_INT, 0xFFFF_FFFF);
        // nic_init: power, configuration, the rings.
        self.apm_init()?;
        let ps = self.read_prph(APMG_PS_CTRL_REG);
        self.write_prph(APMG_PS_CTRL_REG, ps & !APMG_PS_CTRL_MSK_PWR_SRC); // VMAIN
        self.nic_config();
        self.rx_init();
        self.tx_init()?;
        self.set_bit(CSR_MAC_SHADOW_REG_CTRL, 0x800F_FFFF);
        self.w32(CSR_UCODE_DRV_GP1_CLR, UCODE_SW_BIT_RFKILL);

        let fw = self.fw.take().ok_or("no firmware")?;
        let image = if init { &fw.init } else { &fw.regular };
        let mut result = Ok(());
        let mut n = 0;
        for sec in &image.sections {
            // CPU1/CPU2 or paging separator: this card has one CPU.
            if sec.offset == 0xFFFF_CCCC || sec.offset == 0xAAAA_BBBB {
                break;
            }
            if let Err(e) = self.load_section(sec) {
                result = Err(e);
                break;
            }
            n += 1;
        }
        self.fw = Some(fw);
        result?;
        // Release the CPU.
        self.w32(CSR_RESET, 0);
        let alive = self.wait_for(&[UCODE_ALIVE_NTFY], 1_000_000)
            .ok_or_else(|| format!("no ALIVE from the {} image ({} sections loaded); {}",
                if init { "INIT" } else { "runtime" }, n, self.why()))?;
        let status = if alive.data.len() >= 2 { le16(&alive.data, 0) } else { 0 };
        if alive.data.len() >= 24 {
            self.error_table = le32(&alive.data, 20);
        }
        wlog!("{} ALIVE: status {:#06x}, {} bytes, error log at {:#x}",
            if init { "INIT" } else { "runtime" }, status, alive.data.len(), self.error_table);
        if status != 0xCAFE {
            return Err(format!("firmware not valid (ALIVE status {:#06x})", status));
        }
        self.tx_start();
        self.running = true;
        Ok(())
    }

    /// What the card says when something didn't come: its interrupt cause,
    /// and the firmware's error log after a firmware error.
    fn why(&self) -> String {
        let int = self.r32(CSR_INT);
        let mut s = format!("CSR_INT {:#010x} GP_CNTRL {:#010x} RESET {:#x}", int, self.r32(CSR_GP_CNTRL), self.r32(CSR_RESET));
        if int & (INT_SW_ERR | INT_HW_ERR) != 0 || int == 0xFFFF_FFFF {
            s.push_str(&self.error_log());
        }
        s
    }

    fn error_log(&self) -> String {
        if self.error_table == 0 || self.error_table == 0xFFFF_FFFF {
            return String::from("; no error log address");
        }
        let w: Vec<u32> = (0..12).map(|i| self.read_mem32(self.error_table + 4 * i)).collect();
        format!("; FW ERROR id {:#x} pc {:#x} blink2 {:#x} ilink {:#x}/{:#x} data {:#x} {:#x} {:#x}",
            w[1], w[2], w[4], w[5], w[6], w[7], w[8], w[9])
    }

    // ---- receive ----------------------------------------------------------------

    /// Collect what the card has put on the receive ring.
    fn rx_poll(&mut self) -> Vec<Packet> {
        let mut out = Vec::new();
        let int = self.r32(CSR_INT);
        if int != 0 && int != 0xFFFF_FFFF {
            self.w32(CSR_INT, int);
            self.w32(CSR_FH_INT_STATUS, self.r32(CSR_FH_INT_STATUS));
            if int & (INT_SW_ERR | INT_HW_ERR) != 0 {
                wlog!("card error: CSR_INT {:#010x}{}", int, self.error_log());
                self.running = false;
                self.state = String::from("firmware error (see DMESG WIFI)");
            }
            if int & INT_RF_KILL != 0 {
                wlog!("RF kill switch now {}", if self.rf_kill() { "ON (radio off)" } else { "off" });
            }
        }
        let closed = (self.rx_stts.r16(0) & 0xFFF) as usize % RX_QUEUE_SIZE;
        let mut handed_back = 0;
        while self.rx_read != closed {
            let buf = self.rx_bufs[self.rx_read];
            let mut off = 0usize;
            while off + 8 <= RB_SIZE {
                let len_n_flags = buf.r32(off);
                if len_n_flags == 0x5555_0000 || len_n_flags & 0x3FFF == 0 {
                    break;
                }
                let len = (len_n_flags & 0x3FFF) as usize;
                if len < 4 || off + 4 + len > RB_SIZE {
                    break;
                }
                let hdr = buf.r32(off + 4);
                let data = unsafe { core::slice::from_raw_parts(buf.at(off + 8), len - 4) }.to_vec();
                out.push(Packet { cmd: hdr as u8, group: (hdr >> 8) as u8, seq: (hdr >> 16) as u16, data });
                off += (len + 4 + 0x3F) & !0x3F;
            }
            // Clear what was used (the card may not mark where its packets
            // end) and hand the descriptor back.
            unsafe { core::ptr::write_bytes(buf.ptr, 0, off.clamp(8, RB_SIZE)) };
            self.rx_read = (self.rx_read + 1) % RX_QUEUE_SIZE;
            self.rx_write = (self.rx_write + 1) % RX_QUEUE_SIZE;
            handed_back += 1;
        }
        if handed_back > 0 {
            self.w32(FH_RSCSR_CHNL0_WPTR, (self.rx_write & !7) as u32);
        }
        // Replies free their command slot.
        for p in &out {
            if p.seq & 0x8000 == 0 && (p.seq >> 8) as usize & 0x1F == CMD_QUEUE {
                if let Some(t) = self.txq[CMD_QUEUE].as_mut() {
                    t.read = ((p.seq & 0xFF) as usize + 1) % TFD_QUEUE_SIZE;
                }
            }
        }
        out
    }

    /// Wait for a notification (one of `cmds`), keeping everything else.
    fn wait_for(&mut self, cmds: &[u8], timeout_us: u64) -> Option<Packet> {
        if let Some(i) = self.pending.iter().position(|p| p.group == 0 && cmds.contains(&p.cmd)) {
            return self.pending.remove(i);
        }
        let deadline = crate::time::Deadline::after_us(timeout_us);
        loop {
            let mut found = None;
            for p in self.rx_poll() {
                if found.is_none() && cmds.contains(&p.cmd) && p.group == 0 {
                    found = Some(p);
                } else {
                    self.note(p);
                }
            }
            if found.is_some() {
                return found;
            }
            if deadline.expired() || self.r32(CSR_INT) & (INT_SW_ERR | INT_HW_ERR) != 0 && !self.running {
                return None;
            }
            core::hint::spin_loop();
        }
    }

    /// Handle a packet that isn't what's being waited for: scan results and
    /// PHY data are kept, anything else queued.
    fn note(&mut self, p: Packet) {
        match p.cmd {
            REPLY_RX_PHY_CMD => self.last_phy = p.data,
            REPLY_RX_MPDU_CMD => self.rx_mpdu(&p.data),
            CALIB_RES_NOTIF_PHY_DB if p.data.len() >= 4 => {
                let ty = le16(&p.data, 0);
                let len = le16(&p.data, 2) as usize;
                let chg = if (ty == 4 || ty == 5) && p.data.len() >= 6 { le16(&p.data, 4) } else { 0 };
                let body = p.data[4..(4 + len).min(p.data.len())].to_vec();
                self.phy_db.retain(|(t, g, _)| !(*t == ty && *g == chg));
                self.phy_db.push((ty, chg, body));
            }
            REPLY_ERROR => {
                let d = &p.data;
                if d.len() >= 16 {
                    wlog!("REPLY_ERROR: type {:#x} cmd {:#x} seq {:#x} data {:#x}",
                        le32(d, 0), d[4], le16(d, 6), le32(d, 8));
                }
            }
            _ => {
                if self.pending.len() >= 64 {
                    self.pending.pop_front();
                }
                self.pending.push_back(p);
            }
        }
    }

    // ---- commands ------------------------------------------------------------------

    /// Send a host command and wait for its reply (its payload).
    fn send_cmd(&mut self, cmd: u8, group: u8, payload: &[u8]) -> Result<Vec<u8>, String> {
        self.send_cmd_parts(cmd, group, &[payload])
    }

    fn send_cmd_parts(&mut self, cmd: u8, group: u8, parts: &[&[u8]]) -> Result<Vec<u8>, String> {
        let len: usize = parts.iter().map(|p| p.len()).sum();
        let hdr_len = if group != 0 { 8 } else { 4 };
        let total = hdr_len + len;
        let t = self.txq[CMD_QUEUE].as_mut().ok_or("no command queue")?;
        if total > t.buf_size {
            return Err(format!("command {:#x} too big ({} bytes)", cmd, total));
        }
        if (t.write + 1) % TFD_QUEUE_SIZE == t.read || (t.write + TFD_QUEUE_SIZE - t.read) % TFD_QUEUE_SIZE >= CMD_SLOTS - 1 {
            return Err(String::from("command queue full"));
        }
        let idx = t.write;
        let slot = idx % CMD_SLOTS;
        let seq = ((CMD_QUEUE as u16) << 8) | idx as u16;
        let buf = t.bufs.at(slot * t.buf_size);
        let buf_phys = t.bufs.phys + (slot * t.buf_size) as u64;
        unsafe {
            if group != 0 {
                let h = [cmd, group, seq as u8, (seq >> 8) as u8, len as u8, (len >> 8) as u8, 0, 0];
                core::ptr::copy_nonoverlapping(h.as_ptr(), buf, 8);
            } else {
                let h = [cmd, 0, seq as u8, (seq >> 8) as u8];
                core::ptr::copy_nonoverlapping(h.as_ptr(), buf, 4);
            }
            let mut at = hdr_len;
            for p in parts {
                core::ptr::copy_nonoverlapping(p.as_ptr(), buf.add(at), p.len());
                at += p.len();
            }
        }
        // The TFD: the first 20 bytes, then the rest in pieces under 4 KiB.
        let tfd = t.tfds.at(idx * TFD_SIZE);
        unsafe { core::ptr::write_bytes(tfd, 0, TFD_SIZE) };
        let mut tbs = Vec::new();
        let first = total.min(FIRST_TB_SIZE);
        tbs.push((buf_phys, first));
        let mut at = first;
        while at < total {
            let n = (total - at).min(4032);
            tbs.push((buf_phys + at as u64, n));
            at += n;
        }
        write_tfd(tfd, &tbs);
        t.write = (idx + 1) % TFD_QUEUE_SIZE;
        let wr = t.write as u32;
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        self.w32(HBUS_TARG_WRPTR, wr | (CMD_QUEUE as u32) << 8);

        // The reply.
        let deadline = crate::time::Deadline::after_us(2_000_000);
        loop {
            let mut reply = None;
            for p in self.rx_poll() {
                if reply.is_none() && p.is_reply_to(seq) {
                    reply = Some(p);
                } else {
                    self.note(p);
                }
            }
            if let Some(p) = reply {
                return Ok(p.data);
            }
            if deadline.expired() {
                return Err(format!("no reply to command {:#x}/{:#x}; {}", group, cmd, self.why()));
            }
            if !self.running && self.r32(CSR_INT) & INT_SW_ERR != 0 {
                return Err(format!("firmware error during command {:#x}; {}", cmd, self.why()));
            }
            core::hint::spin_loop();
        }
    }

    /// A command whose reply is a status word; Err unless it's `ok`.
    fn send_cmd_status(&mut self, cmd: u8, payload: &[u8], ok: u32) -> Result<(), String> {
        let r = self.send_cmd(cmd, 0, payload)?;
        let st = if r.len() >= 4 { le32(&r, 0) } else { 0xFFFF_FFFF };
        if st & 0xFF != ok {
            return Err(format!("command {:#x} failed: status {:#x}", cmd, st));
        }
        Ok(())
    }

    // ---- bring-up ---------------------------------------------------------------

    fn load_firmware_file(&mut self) -> Result<(), String> {
        if self.fw.is_some() {
            return Ok(());
        }
        let mut tried = Vec::new();
        for name in self.firmware_names() {
            let path = format!("/lib/firmware/{}", name);
            match crate::vfs::VFS.read_file(&path) {
                Ok(bytes) => {
                    let fw = Firmware::parse(name, &bytes)?;
                    wlog!("firmware {} ({} KiB): {}, INIT {} + runtime {} sections, PHY {:#010x}",
                        name, bytes.len() / 1024, fw.version, fw.init.sections.len(),
                        fw.regular.sections.len(), fw.phy_config);
                    self.fw = Some(fw);
                    return Ok(());
                }
                Err(_) => tried.push(path),
            }
        }
        Err(format!("firmware not found: {}", tried.join(", ")))
    }

    /// From power-up to a runtime firmware ready to scan.
    pub fn up(&mut self) -> Result<(), String> {
        self.state = String::from("starting");
        let r = self.up_inner();
        match &r {
            Ok(()) => self.state = String::from("ready"),
            Err(e) => self.state = format!("failed: {}", e),
        }
        r
    }

    fn up_inner(&mut self) -> Result<(), String> {
        self.load_firmware_file()?;
        if self.running {
            self.stop_device();
        }
        self.start_hw()?;
        wlog!("powered up; RF kill switch {}", if self.rf_kill() { "ON (radio off)" } else { "off" });

        // INIT image: calibrations, the NVM.
        self.phy_db.clear();
        self.start_fw(true)?;
        self.send_bt_config()?;
        self.read_nvm()?;
        let (flow, event) = self.fw.as_ref().map_or((0, 0), |f| f.calib[1]);
        self.send_cmd(TX_ANT_CONFIGURATION_CMD, 0, &(self.valid_tx_ant as u32).to_le_bytes())?;
        self.send_phy_cfg(flow, event)?;
        self.wait_for(&[INIT_COMPLETE_NOTIF], 2_000_000)
            .ok_or_else(|| format!("INIT calibration didn't complete; {}", self.why()))?;
        wlog!("INIT done: {} PHY DB sections", self.phy_db.len());

        // Runtime image.
        self.stop_device();
        self.start_hw()?;
        self.start_fw(false)?;
        self.send_cmd(TX_ANT_CONFIGURATION_CMD, 0, &(self.valid_tx_ant as u32).to_le_bytes())?;
        self.send_phy_db()?;
        let (flow, event) = self.fw.as_ref().map_or((0, 0), |f| f.calib[0]);
        self.send_phy_cfg(flow, event)?;
        self.send_bt_config()?;
        if self.fw.as_ref().is_some_and(|f| f.capa(12)) {
            self.send_cmd(DQA_ENABLE_CMD, DATA_PATH_GROUP, &(CMD_QUEUE as u32).to_le_bytes())?;
        }
        self.add_aux_sta()?;
        self.send_cmd(POWER_TABLE_CMD, 0, &[0, 0, 0, 0])?; // always awake (CAM)
        self.update_mcc()?;
        self.add_mac_context()?;
        wlog!("runtime firmware up: MAC {}, NVM {:#x}, antennas TX {:#x} RX {:#x}",
            mac_text(&self.mac), self.nvm_version, self.valid_tx_ant, self.valid_rx_ant);
        Ok(())
    }

    fn send_bt_config(&mut self) -> Result<(), String> {
        // BT_COEX_NW mode; MPLUT (if supported), SYNC2SCO, HIGH_BAND_RET.
        let mplut = self.fw.as_ref().is_some_and(|f| f.capa(67));
        let modules: u32 = (if mplut { 1 } else { 0 }) | 1 << 2 | 1 << 4;
        let mut c = Vec::new();
        c.extend_from_slice(&1u32.to_le_bytes());
        c.extend_from_slice(&modules.to_le_bytes());
        self.send_cmd(BT_CONFIG, 0, &c)?;
        Ok(())
    }

    fn send_phy_cfg(&mut self, flow: u32, event: u32) -> Result<(), String> {
        let mut phy = self.fw.as_ref().map_or(0, |f| f.phy_config);
        phy = (phy & !(0xF << 16 | 0xF << 20)) | (self.valid_tx_ant as u32) << 16 | (self.valid_rx_ant as u32) << 20;
        let mut c = Vec::new();
        c.extend_from_slice(&phy.to_le_bytes());
        c.extend_from_slice(&flow.to_le_bytes());
        c.extend_from_slice(&event.to_le_bytes());
        self.send_cmd(PHY_CONFIGURATION_CMD, 0, &c)?;
        Ok(())
    }

    /// The calibration results INIT reported, to the runtime image: the
    /// configuration, the non-channel calibration, then the per-channel-group
    /// PAPD and TX power tables.
    fn send_phy_db(&mut self) -> Result<(), String> {
        let mut db = core::mem::take(&mut self.phy_db);
        db.sort_by_key(|(t, g, _)| (match t { 1 => 0, 2 => 1, 4 => 2, 5 => 3, _ => 4 }, *g));
        let mut sent = 0;
        for (ty, _, data) in &db {
            let hdr = [*ty as u8, (*ty >> 8) as u8, data.len() as u8, (data.len() >> 8) as u8];
            self.send_cmd_parts(PHY_DB_CMD, 0, &[&hdr, data])?;
            sent += 1;
        }
        self.phy_db = db;
        if sent == 0 {
            wlog!("warning: no PHY DB from INIT");
        }
        Ok(())
    }

    /// The card's NVM, through the INIT firmware: the MAC address (HW
    /// section) and the antennas (SW section).
    fn read_nvm(&mut self) -> Result<(), String> {
        let hw = self.read_nvm_section(0)?;
        let sw = self.read_nvm_section(1)?;
        if hw.len() >= 0x15 * 2 + 6 {
            let a = &hw[0x15 * 2..0x15 * 2 + 6];
            self.mac = [a[1], a[0], a[3], a[2], a[5], a[4]];
        }
        if sw.len() >= 6 {
            self.nvm_version = le16(&sw, 0);
            let radio = le16(&sw, 2);
            self.valid_tx_ant = ((radio >> 8) & 0xF) as u8;
            self.valid_rx_ant = ((radio >> 12) & 0xF) as u8;
        }
        let fw_tx = (self.fw.as_ref().map_or(0, |f| f.phy_config) >> 16 & 0xF) as u8;
        let fw_rx = (self.fw.as_ref().map_or(0, |f| f.phy_config) >> 20 & 0xF) as u8;
        if self.valid_tx_ant == 0 || self.valid_tx_ant & fw_tx == 0 { self.valid_tx_ant = fw_tx; } else { self.valid_tx_ant &= fw_tx; }
        if self.valid_rx_ant == 0 || self.valid_rx_ant & fw_rx == 0 { self.valid_rx_ant = fw_rx; } else { self.valid_rx_ant &= fw_rx; }
        if self.valid_tx_ant == 0 { self.valid_tx_ant = 1; }
        if self.valid_rx_ant == 0 { self.valid_rx_ant = 1; }
        wlog!("NVM: HW {} bytes, SW {} bytes, MAC {}, version {:#x}, SKU {:#x}",
            hw.len(), sw.len(), mac_text(&self.mac), self.nvm_version,
            if sw.len() >= 6 { le16(&sw, 4) } else { 0 });
        if self.mac == [0; 6] || self.mac[0] & 1 != 0 {
            return Err(format!("no valid MAC address in the NVM ({})", mac_text(&self.mac)));
        }
        Ok(())
    }

    fn read_nvm_section(&mut self, section: u16) -> Result<Vec<u8>, String> {
        let mut out = Vec::new();
        loop {
            let off = out.len() as u16;
            // op_code 0 (read), target 0, type, offset, length 2 KiB.
            let mut c = [0u8; 8];
            c[2..4].copy_from_slice(&section.to_le_bytes());
            c[4..6].copy_from_slice(&off.to_le_bytes());
            c[6..8].copy_from_slice(&2048u16.to_le_bytes());
            let r = self.send_cmd(NVM_ACCESS_CMD, 0, &c)?;
            if r.len() < 8 {
                return Err(String::from("short NVM reply"));
            }
            // offset, length, type, status, data
            let (len, status) = (le16(&r, 2) as usize, le16(&r, 6));
            if status != 0 {
                if off != 0 && status == 1 {
                    break; // a section that is a multiple of 2 KiB
                }
                return Err(format!("NVM section {} read failed (status {})", section, status));
            }
            out.extend_from_slice(&r[8..(8 + len).min(r.len())]);
            if len < 2048 || out.len() > 16384 {
                break;
            }
        }
        Ok(out)
    }

    /// The auxiliary station (used by the firmware for scanning) on queue 1.
    fn add_aux_sta(&mut self) -> Result<(), String> {
        self.enable_txq(AUX_QUEUE, AUX_STA_ID, 8, TX_FIFO_MCAST)?;
        let mut c = [0u8; 44];
        c[2..4].copy_from_slice(&0xFFFFu16.to_le_bytes()); // tid_disable_tx
        c[4..8].copy_from_slice(&MAC_INDEX_AUX.to_le_bytes()); // mac_id_n_color
        c[16] = AUX_STA_ID;
        c[40..44].copy_from_slice(&(1u32 << AUX_QUEUE).to_le_bytes()); // tfd_queue_msk
        self.send_cmd_status(ADD_STA, &c, 1)
    }

    /// The regulatory domain: "ZZ" asks the card for its own (LAR).
    fn update_mcc(&mut self) -> Result<(), String> {
        if !self.fw.as_ref().is_some_and(|f| f.capa(1)) {
            return Ok(());
        }
        let mut c = [0u8; 32];
        c[0..2].copy_from_slice(&((b'Z' as u16) << 8 | b'Z' as u16).to_le_bytes());
        c[2] = 0x10; // MCC_SOURCE_GET_CURRENT
        match self.send_cmd(MCC_UPDATE_CMD, 0, &c) {
            Ok(r) if r.len() >= 6 => {
                let mcc = le16(&r, 4);
                wlog!("regulatory: status {:#x}, country {}{}", le32(&r, 0), (mcc >> 8) as u8 as char, mcc as u8 as char);
            }
            Ok(_) => {}
            Err(e) => wlog!("regulatory update failed: {}", e),
        }
        Ok(())
    }

    /// The station interface (MAC context 0), not associated.
    fn add_mac_context(&mut self) -> Result<(), String> {
        let c = self.mac_context_cmd(1, None, false, 0, 0, 0);
        self.send_cmd(MAC_CONTEXT_CMD, 0, &c)?;
        Ok(())
    }

    fn mac_context_cmd(&self, action: u32, bssid: Option<[u8; 6]>, assoc: bool, aid: u16, beacon_int: u16, dtim: u8) -> Vec<u8> {
        let mut c = vec![0u8; 148];
        let mut put = |o: usize, v: u32| c[o..o + 4].copy_from_slice(&v.to_le_bytes());
        put(0, MAC_ID); // id and color 0
        put(4, action);
        put(8, 5); // FW_MAC_TYPE_BSS_STA
        put(12, 0); // TSF id
        put(32, 0xF); // CCK basic rates 1, 2, 5.5, 11
        put(36, 0x15); // OFDM basic rates 6, 12, 24
        put(40, 0); // protection
        put(44, 1 << 5); // short preamble
        put(48, 1 << 4); // short slot
        put(52, 1 << 2 | if assoc { 0 } else { 1 << 6 }); // accept group; beacons until associated
        put(56, 1); // QoS: update EDCA
        // EDCA per firmware AC (BK, BE, VI, VO): cw_min, cw_max, aifsn, fifo, txop.
        let ac: [(u16, u16, u8, u8, u16); 4] = [(15, 1023, 7, 0, 0), (15, 1023, 3, 1, 0), (7, 15, 2, 2, 94), (3, 7, 2, 3, 47)];
        for (i, (cmin, cmax, aifs, fifo, txop)) in ac.iter().enumerate() {
            let o = 60 + i * 8;
            c[o..o + 2].copy_from_slice(&cmin.to_le_bytes());
            c[o + 2..o + 4].copy_from_slice(&cmax.to_le_bytes());
            c[o + 4] = *aifs;
            c[o + 5] = 1 << fifo;
            c[o + 6..o + 8].copy_from_slice(&(txop * 32).to_le_bytes());
        }
        c[16..22].copy_from_slice(&self.mac);
        c[24..30].copy_from_slice(&bssid.unwrap_or([0xFF; 6]));
        // Station data at 100: is_assoc, dtim_time, dtim_tsf, bi, reserved,
        // dtim_interval, data_policy, listen_interval, assoc_id, arrive time.
        let mut put = |o: usize, v: u32| c[o..o + 4].copy_from_slice(&v.to_le_bytes());
        put(100, assoc as u32);
        put(116, beacon_int as u32);
        put(124, beacon_int as u32 * dtim.max(1) as u32);
        put(132, 10); // listen interval
        put(136, aid as u32);
        c
    }

    // ---- scanning ----------------------------------------------------------------

    /// Scan every channel the card has (2.4 and 5 GHz) and gather what
    /// answers. Takes a few seconds.
    pub fn scan(&mut self) -> Result<usize, String> {
        if !self.running {
            return Err(format!("card not up ({})", self.state));
        }
        let max = self.fw.as_ref().map_or(40, |f| f.n_scan_channels).min(40);
        let mut channels: Vec<u8> = (1..=13).collect();
        channels.extend_from_slice(&[36, 40, 44, 48, 52, 56, 60, 64, 100, 104, 108, 112, 116, 120, 124, 128, 132, 136, 140, 149, 153, 157, 161, 165]);
        channels.truncate(max);
        self.scan_results.clear();
        self.pending.retain(|p| p.cmd != SCAN_OFFLOAD_COMPLETE);
        let cmd = self.scan_cmd(&channels, max);
        let r = self.send_cmd(SCAN_OFFLOAD_REQUEST_CMD, 0, &cmd)?;
        wlog!("scan started: {} channels, reply {:02x?}", channels.len(), &r[..r.len().min(8)]);
        let done = self.wait_for(&[SCAN_OFFLOAD_COMPLETE], 15_000_000)
            .ok_or_else(|| format!("scan didn't finish; {}", self.why()))?;
        // Late frames.
        for p in self.rx_poll() {
            self.note(p);
        }
        self.pending.retain(|p| p.cmd != SCAN_ITERATION_COMPLETE);
        let st = done.data.get(2).copied().unwrap_or(0);
        wlog!("scan of {} channels done (status {}): {} networks", channels.len(), st, self.scan_results.len());
        self.scan_results.sort_by(|a, b| b.signal.cmp(&a.signal));
        Ok(self.scan_results.len())
    }

    fn scan_cmd(&self, channels: &[u8], max: usize) -> Vec<u8> {
        const FIXED: usize = 764;
        let mut c = vec![0u8; FIXED + max * 12 + 528];
        let put32 = |c: &mut Vec<u8>, o: usize, v: u32| c[o..o + 4].copy_from_slice(&v.to_le_bytes());
        c[4] = channels.len() as u8;
        c[5] = 10; // active dwell
        c[6] = 110; // passive dwell
        c[7] = 44; // fragmented dwell
        c[8] = 90; // extended dwell
        let rx = self.valid_rx_ant as u16;
        let chain = rx << 1 | rx << 7 | rx << 4 | 1;
        c[10..12].copy_from_slice(&chain.to_le_bytes());
        put32(&mut c, 12, 1 << 0 | 1 << 7); // pass all, extended dwell
        put32(&mut c, 16, 0); // max out time
        put32(&mut c, 20, 0); // suspend time
        put32(&mut c, 24, if channels[0] <= 14 { 1 } else { 0 }); // band of the first channel
        put32(&mut c, 28, 1 << 2 | 1 << 6); // accept group, beacons
        // Probe request TX: 1 Mb/s CCK on 2.4 GHz, 6 Mb/s on 5 GHz, antenna A.
        let ant = (self.first_tx_ant() as u32) << 14;
        put32(&mut c, 32, 1 << 13 | 1 << 12);
        put32(&mut c, 36, 10 | 1 << 9 | ant);
        c[40] = AUX_STA_ID;
        put32(&mut c, 44, 1 << 13 | 1 << 12);
        put32(&mut c, 48, 13 | ant);
        c[52] = AUX_STA_ID;
        // 20 direct-SSID slots (none: wildcard), then priority, iterations.
        put32(&mut c, 736, 6); // priority
        put32(&mut c, 740, 1); // iterations
        put32(&mut c, 744, 0); // delay
        c[748..750].copy_from_slice(&0u16.to_le_bytes()); // schedule[0]: delay
        c[750] = 1; // iterations
        c[751] = 1; // full scan multiplier
        // Channels.
        for (i, &ch) in channels.iter().enumerate() {
            let o = FIXED + i * 12;
            put32(&mut c, o, 1 << 28); // partial
            c[o + 4..o + 6].copy_from_slice(&(ch as u16).to_le_bytes());
            c[o + 6..o + 8].copy_from_slice(&1u16.to_le_bytes());
        }
        // The probe request template.
        let p = FIXED + max * 12;
        let buf = p + 16;
        let mut f = Vec::new();
        f.extend_from_slice(&[0x40, 0x00, 0, 0]);
        f.extend_from_slice(&[0xFF; 6]);
        f.extend_from_slice(&self.mac);
        f.extend_from_slice(&[0xFF; 6]);
        f.extend_from_slice(&[0, 0]);
        f.extend_from_slice(&[0, 0]); // wildcard SSID
        let hdr_len = f.len();
        let b24 = [1, 8, 0x82, 0x84, 0x8B, 0x96, 0x0C, 0x12, 0x18, 0x24, 50, 4, 0x30, 0x48, 0x60, 0x6C];
        let b5 = [1, 8, 0x8C, 0x12, 0x98, 0x24, 0xB0, 0x48, 0x60, 0x6C];
        f.extend_from_slice(&b24);
        f.extend_from_slice(&b5);
        c[buf..buf + f.len()].copy_from_slice(&f);
        let seg = |c: &mut Vec<u8>, o: usize, off: usize, len: usize| {
            c[o..o + 2].copy_from_slice(&(off as u16).to_le_bytes());
            c[o + 2..o + 4].copy_from_slice(&(len as u16).to_le_bytes());
        };
        seg(&mut c, p, 0, hdr_len);
        seg(&mut c, p + 4, hdr_len, b24.len());
        seg(&mut c, p + 8, hdr_len + b24.len(), b5.len());
        seg(&mut c, p + 12, hdr_len + b24.len() + b5.len(), 0);
        c
    }

    fn first_tx_ant(&self) -> u8 {
        let a = self.valid_tx_ant;
        if a & 1 != 0 { 1 } else if a & 2 != 0 { 2 } else { 4 }
    }

    /// A received 802.11 frame (after its PHY info): beacons and probe
    /// responses go into the scan results.
    fn rx_mpdu(&mut self, d: &[u8]) {
        if d.len() < 4 {
            return;
        }
        let len = le16(d, 0) as usize;
        if d.len() < 4 + len + 4 || len < 24 {
            return;
        }
        let frame = &d[4..4 + len];
        let status = le32(d, 4 + len);
        if status & 3 != 3 {
            return; // CRC or overrun error
        }
        self.rx_frames += 1;
        let fc = le16(frame, 0);
        let (ty, sub) = (fc >> 2 & 3, fc >> 4 & 0xF);
        if ty == 0 && (sub == 8 || sub == 5) && len >= 36 {
            let signal = self.signal();
            let phy_ch = if self.last_phy.len() >= 24 { le16(&self.last_phy, 22) as u8 } else { 0 };
            self.add_bss(frame, signal, phy_ch);
        }
    }

    /// Signal (dBm) of the last frame: the stronger antenna's energy.
    fn signal(&self) -> i8 {
        if self.last_phy.len() < 32 {
            return -100;
        }
        let e = le32(&self.last_phy, 28); // non_cfg_phy[1]
        let (a, b) = ((e & 0xFF) as i32, (e >> 8 & 0xFF) as i32);
        let a = if a != 0 { -a } else { -128 };
        let b = if b != 0 { -b } else { -128 };
        a.max(b).max(-128) as i8
    }

    fn add_bss(&mut self, frame: &[u8], signal: i8, phy_ch: u8) {
        let mut bssid = [0u8; 6];
        bssid.copy_from_slice(&frame[16..22]);
        let beacon_int = le16(frame, 32);
        let capability = le16(frame, 34);
        let mut bss = Bss {
            bssid, ssid: Vec::new(), channel: phy_ch, signal, privacy: capability & 0x10 != 0,
            rsn: None, wpa: false, beacon_int, capability, rates: Vec::new(), last_seen: crate::time::now_ms(),
        };
        let mut o = 36;
        while o + 2 <= frame.len() {
            let (id, l) = (frame[o], frame[o + 1] as usize);
            let Some(v) = frame.get(o + 2..o + 2 + l) else { break };
            match id {
                0 => bss.ssid = v.to_vec(),
                1 | 50 => bss.rates.extend_from_slice(v),
                3 if l >= 1 => bss.channel = v[0],
                48 => bss.rsn = Some(v.to_vec()),
                221 if v.starts_with(&[0x00, 0x50, 0xF2, 0x01]) => bss.wpa = true,
                _ => {}
            }
            o += 2 + l;
        }
        if let Some(old) = self.scan_results.iter_mut().find(|b| b.bssid == bssid) {
            if old.ssid.is_empty() || !bss.ssid.is_empty() {
                *old = Bss { signal: old.signal.max(signal), ..bss };
            }
        } else if self.scan_results.len() < 128 {
            self.scan_results.push(bss);
        }
    }

    pub fn status_lines(&self) -> Vec<String> {
        let mut v = vec![format!("  {} ({:04x}, sub {:04x}), HW rev {:#x}", self.model(), self.device_id, self.subsystem, self.hw_rev)];
        v.push(format!("  state: {}", self.state));
        if let Some(fw) = &self.fw {
            v.push(format!("  firmware: {} ({})", fw.name, fw.version));
        }
        if self.mac != [0; 6] {
            v.push(format!("  MAC {}, antennas TX {:#x} RX {:#x}, RF kill {}", mac_text(&self.mac),
                self.valid_tx_ant, self.valid_rx_ant, if self.rf_kill() { "ON" } else { "off" }));
        }
        v
    }
}

fn write_tfd(tfd: *mut u8, tbs: &[(u64, usize)]) {
    unsafe {
        *tfd.add(3) = tbs.len() as u8;
        for (i, &(addr, len)) in tbs.iter().enumerate() {
            let tb = tfd.add(4 + i * 6);
            (tb as *mut u32).write_unaligned(addr as u32);
            let hi_n_len = ((addr >> 32) as u16 & 0xF) | (len as u16) << 4;
            (tb.add(4) as *mut u16).write_unaligned(hi_n_len);
        }
    }
}

pub fn mac_text(m: &[u8; 6]) -> String {
    format!("{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}", m[0], m[1], m[2], m[3], m[4], m[5])
}

// ---- terminal ---------------------------------------------------------------------

/// The terminal's WIFI command.
pub fn command(args: &[&str]) -> Vec<String> {
    let mut out = Vec::new();
    let sub = args.first().map(|s| s.to_ascii_uppercase()).unwrap_or_else(|| String::from("STATUS"));
    let mut guard = WIFI.lock();
    let Some(iwl) = guard.as_mut() else {
        out.push(String::from("  No supported Wi-Fi card (Intel 7265) found."));
        return out;
    };
    match sub.as_str() {
        "STATUS" => out.extend(iwl.status_lines()),
        "UP" => match iwl.up() {
            Ok(()) => out.push(format!("  Wi-Fi up, MAC {}", mac_text(&iwl.mac))),
            Err(e) => out.push(format!("  Wi-Fi start failed: {}", e)),
        },
        "SCAN" => match iwl.scan() {
            Ok(n) => {
                out.push(format!("  {} network(s):", n));
                out.push(String::from("  SIGNAL  CH   SECURITY  BSSID              SSID"));
                for b in &iwl.scan_results {
                    out.push(format!("  {:4} dBm {:3}  {:9} {}  {}", b.signal, b.channel, b.security(),
                        mac_text(&b.bssid), b.ssid_text()));
                }
            }
            Err(e) => out.push(format!("  Scan failed: {}", e)),
        },
        _ => out.push(String::from("Usage: WIFI [STATUS|UP|SCAN]")),
    }
    out
}
