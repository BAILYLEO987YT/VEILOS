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
const INT_FH_RX_ALL: u32 = 1 << 31 | 1 << 28; // FH_RX, RX_PERIODIC
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
const ADD_STA_KEY: u8 = 0x17;
const TX_CMD: u8 = 0x1C;
const PHY_CONTEXT_CMD: u8 = 0x08;
const TIME_EVENT_CMD: u8 = 0x29;
const TIME_EVENT_NOTIFICATION: u8 = 0x2A;
const BINDING_CONTEXT_CMD: u8 = 0x2B;
const TIME_QUOTA_CMD: u8 = 0x2C;
const MISSED_BEACONS_NOTIFICATION: u8 = 0xA2;
const DATA_PATH_GROUP: u8 = 0x5;
const DQA_ENABLE_CMD: u8 = 0x0;

const CMD_QUEUE: usize = 0; // IWL_MVM_DQA_CMD_QUEUE
const AUX_QUEUE: usize = 1; // IWL_MVM_DQA_AUX_QUEUE
/// The AP's queue (IWL_MVM_DQA_BSS_CLIENT_QUEUE): management frames and,
/// since this driver associates without QoS, all data too.
const BSS_QUEUE: usize = 4;
const BSS_QUEUE_SLOTS: usize = 64;
const TX_FIFO_VO: u8 = 3;
const AP_STA_ID: u8 = 1;
const PHY_ID: u32 = 0;
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
    // INTx off at the PCI level: the card's interrupt causes are enabled as
    // Linux and OpenBSD do (the firmware may look at them), but VEILOS
    // polls, so none may reach the CPU.
    dev.set_command_bits(1 << 10);
    // As Linux: no PCI retry timeout (config 0x41), else the card's RX
    // DMA can stall the CPU's C3 state.
    dev.address().write_u8(0x41, 0);
    map_mmio(base, bar.size.max(0x2000));
    let subsystem = dev.address().read_u16(0x2E);
    let iwl = Iwl::new(base, dev.device_id, subsystem)?;
    wlog!("WPA crypto self-test {}", if crate::wpa::self_test() { "ok" } else { "FAILED" });
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
    pub base: u64,
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
    /// grab_nic_access nesting depth.
    nic_access: core::cell::Cell<u32>,
    /// CSR_INT and FH_INT bits seen since the last command was sent.
    int_seen: u32,
    fh_seen: u32,
    alive_scd: u32,
    event_log: u32,
    logs_dumped: bool,
    pub mac: [u8; 6],
    valid_tx_ant: u8,
    valid_rx_ant: u8,
    pub nvm_version: u16,
    pub running: bool,
    pub state: String,
    pub scan_results: Vec<Bss>,
    last_phy: Vec<u8>,
    pub rx_frames: u64,
    /// The network joined (or being joined).
    pub link: Option<Link>,
    /// Management frames for us (authentication, association, deauth).
    mgmt_rx: VecDeque<Vec<u8>>,
    /// EAPOL frames from the AP (after the LLC header).
    eapol_rx: VecDeque<Vec<u8>>,
    /// Ethernet frames for the network stack.
    pub eth_rx: VecDeque<Vec<u8>>,
    phy_added: bool,
    /// WIFI UP <letters>: start-up steps to leave out (diagnosis).
    pub skip: String,
    /// Start-up steps done so far, for the error message.
    steps: String,
    mac_added: bool,
    pub tx_ok: u64,
    pub tx_failed: u64,
    pub rx_data: u64,
    pub rx_dropped: u64,
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
            nic_access: core::cell::Cell::new(0),
            int_seen: 0,
            fh_seen: 0,
            alive_scd: 0,
            event_log: 0,
            logs_dumped: false,
            mac: [0; 6],
            valid_tx_ant: 0,
            valid_rx_ant: 0,
            nvm_version: 0,
            running: false,
            state: String::from("found"),
            scan_results: Vec::new(),
            last_phy: Vec::new(),
            rx_frames: 0,
            link: None,
            mgmt_rx: VecDeque::new(),
            eapol_rx: VecDeque::new(),
            eth_rx: VecDeque::new(),
            phy_added: false,
            skip: String::new(),
            steps: String::new(),
            mac_added: false,
            tx_ok: 0,
            tx_failed: 0,
            rx_data: 0,
            rx_dropped: 0,
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
        self.grab_nic_access();
        self.w32(HBUS_TARG_PRPH_RADDR, (reg & 0x000F_FFFF) | 3 << 24);
        let v = self.r32(HBUS_TARG_PRPH_RDAT);
        self.release_nic_access();
        v
    }
    fn write_prph(&self, reg: u32, v: u32) {
        self.grab_nic_access();
        self.w32(HBUS_TARG_PRPH_WADDR, (reg & 0x000F_FFFF) | 3 << 24);
        self.w32(HBUS_TARG_PRPH_WDAT, v);
        self.release_nic_access();
    }
    fn set_bits_prph(&self, reg: u32, mask: u32) {
        self.write_prph(reg, self.read_prph(reg) | mask);
    }
    fn clear_bits_prph(&self, reg: u32, mask: u32) {
        self.write_prph(reg, self.read_prph(reg) & !mask);
    }
    fn read_mem32(&self, addr: u32) -> u32 {
        self.grab_nic_access();
        self.w32(HBUS_TARG_MEM_RADDR, addr);
        let v = self.r32(HBUS_TARG_MEM_RDAT);
        self.release_nic_access();
        v
    }
    fn write_mem32(&self, addr: u32, v: u32) {
        self.grab_nic_access();
        self.w32(HBUS_TARG_MEM_WADDR, addr);
        self.w32(HBUS_TARG_MEM_WDAT, v);
        self.release_nic_access();
    }

    /// Keep the card's MAC awake for register and SRAM access, a command
    /// in flight, or (before the firmware runs) the whole bring-up. Nested;
    /// the last release lets the card sleep again, as Linux does: a MAC
    /// kept awake for good may stall the firmware's radio work until its
    /// watchdog fires (seen on the PC: NMI_INTERRUPT_WDG on the first PHY
    /// context and on the scan).
    fn grab_nic_access(&self) -> bool {
        let depth = self.nic_access.get();
        self.nic_access.set(depth + 1);
        if depth > 0 {
            return true;
        }
        self.set_bit(CSR_GP_CNTRL, GP_MAC_ACCESS_REQ);
        let ok = self.poll_bit(CSR_GP_CNTRL, GP_MAC_CLOCK_READY, GP_MAC_CLOCK_READY | GP_GOING_TO_SLEEP, 15_000);
        if !ok {
            wlog!("no NIC access (GP_CNTRL {:#010x})", self.r32(CSR_GP_CNTRL));
        }
        ok
    }

    fn release_nic_access(&self) {
        let depth = self.nic_access.get();
        if depth == 0 {
            return;
        }
        self.nic_access.set(depth - 1);
        if depth == 1 {
            self.clear_bit(CSR_GP_CNTRL, GP_MAC_ACCESS_REQ);
        }
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
        self.nic_access.set(0);
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
        // Every queue gets a ring, as Linux: the scheduler and firmware may
        // look at any of them (one page of buffer for the unused ones).
        for q in 0..NUM_QUEUES {
            if self.txq[q].is_none() {
                let (slots, buf_size) = match q {
                    CMD_QUEUE => (CMD_SLOTS, CMD_BUF),
                    BSS_QUEUE => (BSS_QUEUE_SLOTS, 4096),
                    _ => (1, 4096),
                };
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
        // The scheduler's chain extension (on by default) misbehaves; OpenBSD
        // turns it off on every family.
        self.write_prph(SCD_BASE + 0x244, 0); // SCD_CHAINEXT_EN
        // The command queue: inactive, no aggregation, pointers 0, window 64,
        // active on the command FIFO.
        let q = CMD_QUEUE as u32;
        // The scheduler off while the command queue is set up, then on for
        // that queue (Linux: scd_set_active, which the mvm firmware needs).
        self.write_prph(SCD_EN_CTRL, 0);
        self.write_prph(scd_queue_status_bits(q), 1 << 19);
        self.clear_bits_prph(SCD_AGGR_SEL, 1 << q);
        self.w32(HBUS_TARG_WRPTR, q << 8);
        self.write_prph(scd_queue_rdptr(q), 0);
        self.write_mem32(self.scd_base + SCD_CONTEXT_MEM_LOWER_BOUND + q * 8, 0);
        self.write_mem32(self.scd_base + SCD_CONTEXT_MEM_LOWER_BOUND + q * 8 + 4, 64 | 64 << 16);
        self.write_prph(scd_queue_status_bits(q), 1 << 3 | TX_FIFO_CMD | 1 << 4 | 0x017F_0000);
        self.write_prph(SCD_EN_CTRL, 1 << q);
        self.write_prph(SCD_TXFACT, 0xFF);
        for ch in 0..8 {
            self.w32(fh_tcsr_config(ch), FH_TX_CONFIG_DMA_ENABLE | FH_TX_CONFIG_CREDIT_ENABLE);
        }
        let v = self.r32(FH_TX_CHICKEN_BITS_REG);
        self.w32(FH_TX_CHICKEN_BITS_REG, v | FH_TX_CHICKEN_SCD_AUTO_RETRY_EN);
        self.clear_bits_prph(APMG_PCIDEV_STT_REG, APMG_PCIDEV_STT_VAL_L1_ACT_DIS);
        let _ = SCD_QUEUECHAIN_SEL;
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
                // OpenBSD's iwm loads this 32-byte section too (WIFI UP X).
                if self.skip.contains('X') {
                    let _ = self.load_section(sec);
                }
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
        // Interrupt causes on (as Linux's iwl_enable_interrupts and iwm's
        // iwm_enable_interrupts before releasing the CPU); INTx is off at
        // the PCI level, so this only lets the causes through to CSR_INT.
        if !self.skip.contains('I') {
            self.w32(CSR_INT_MASK, INT_FH_RX_ALL | INT_HW_ERR | INT_FH_TX | INT_SW_ERR | INT_RF_KILL | 1 << 6 | 1 << 3 | 1 << 1 | 1);
        }
        // Release the CPU.
        self.w32(CSR_RESET, 0);
        let alive = self.wait_for(&[UCODE_ALIVE_NTFY], 1_000_000)
            .ok_or_else(|| format!("no ALIVE from the {} image ({} sections loaded); {}",
                if init { "INIT" } else { "runtime" }, n, self.why()))?;
        let status = if alive.data.len() >= 2 { le16(&alive.data, 0) } else { 0 };
        if alive.data.len() >= 24 {
            self.error_table = le32(&alive.data, 20);
        }
        if alive.data.len() >= 44 {
            self.alive_scd = le32(&alive.data, 40);
            self.event_log = le32(&alive.data, 24);
        }
        self.logs_dumped = false;
        wlog!("{} ALIVE: status {:#06x}, {} bytes, seq {:#06x}, error log {:#x}, SCD {:#x}",
            if init { "INIT" } else { "runtime" }, status, alive.data.len(), alive.seq, self.error_table, self.alive_scd);
        if status != 0xCAFE {
            return Err(format!("firmware not valid (ALIVE status {:#06x})", status));
        }
        self.tx_start();
        // The bring-up's hold (apm_init) ends: from here on only around
        // commands and register access (power-up may have taken it twice).
        self.nic_access.set(0);
        self.clear_bit(CSR_GP_CNTRL, GP_MAC_ACCESS_REQ);
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

    /// After a command got no reply: did the card fetch it (TX DMA, the
    /// scheduler's read pointer), and is the receive ring moving?
    fn dump_queues(&self, seq: u16) {
        let q = CMD_QUEUE as u32;
        let (w, r) = self.txq[CMD_QUEUE].as_ref().map_or((0, 0), |t| (t.write, t.read));
        wlog!("cmd seq {:#06x}: INT seen {:#010x} FH seen {:#010x}; queue w {} r {}; SCD rdptr {:#x} status {:#010x}",
            seq, self.int_seen, self.fh_seen, w, r, self.read_prph(scd_queue_rdptr(q)), self.read_prph(scd_queue_status_bits(q)));
        wlog!("SCD base {:#x} (ALIVE {:#x}) EN {:#x} TXFACT {:#x} GP {:#x} ctx {:#x}/{:#x}; FH TX status {:#010x} ch7 cfg {:#010x}",
            self.scd_base, self.alive_scd, self.read_prph(SCD_EN_CTRL), self.read_prph(SCD_TXFACT), self.read_prph(SCD_GP_CTRL),
            self.read_mem32(self.scd_base + SCD_CONTEXT_MEM_LOWER_BOUND),
            self.read_mem32(self.scd_base + SCD_CONTEXT_MEM_LOWER_BOUND + 4),
            self.r32(FH_TSSR_TX_STATUS_REG), self.r32(fh_tcsr_config(7)));
        wlog!("RX: status closed {} finished {}, read {} write {}, RDPTR {:#x}; GP1 {:#x}; FH RX status {:#010x}",
            self.rx_stts.r16(0) & 0xFFF, self.rx_stts.r16(4) & 0xFFF, self.rx_read, self.rx_write,
            self.r32(FH_RSCSR_CHNL0_RDPTR), self.r32(0x054), self.r32(FH_MEM_RSSR_RX_STATUS_REG));
        if let Some(t) = self.txq[CMD_QUEUE].as_ref() {
            let i = (w + TFD_QUEUE_SIZE - 1) % TFD_QUEUE_SIZE;
            let d: Vec<u32> = (0..4).map(|k| unsafe { (t.tfds.at(i * TFD_SIZE + 4 * k) as *const u32).read_volatile() }).collect();
            wlog!("TFD {} at {:#x}: {:08x} {:08x} {:08x} {:08x}", i, t.tfds.phys + (i * TFD_SIZE) as u64, d[0], d[1], d[2], d[3]);
        }
        wlog!("{}", self.error_log());
    }

    /// The firmware's whole error table (LOG_ERROR_TABLE_API_S_VER_3:
    /// words 23 hcmd, 24-28 ISRs, 29 last command, 30 wait_event caller,
    /// 35 PMG clocks, 37 flow handler) and the last event log entries.
    fn dump_fw_logs(&mut self) {
        if self.logs_dumped || self.error_table == 0 || self.error_table == 0xFFFF_FFFF {
            return;
        }
        self.logs_dumped = true;
        let w: Vec<u32> = (0..40).map(|i| self.read_mem32(self.error_table + 4 * i)).collect();
        for (i, chunk) in w.chunks(8).enumerate() {
            let t: Vec<String> = chunk.iter().map(|v| format!("{:08x}", v)).collect();
            wlog!("ERR w{:02}: {}", i * 8, t.join(" "));
        }
        if self.event_log == 0 || self.event_log == 0xFFFF_FFFF {
            return;
        }
        // Header: capacity, type (1: with timestamps), wraps, next entry.
        let cap = self.read_mem32(self.event_log);
        let ty = self.read_mem32(self.event_log + 4);
        let wraps = self.read_mem32(self.event_log + 8);
        let next = self.read_mem32(self.event_log + 12);
        wlog!("EVT log at {:#x}: capacity {} type {} wraps {} next {}", self.event_log, cap, ty, wraps, next);
        if cap == 0 || cap > 4096 || next > cap {
            return;
        }
        let words = if ty == 1 { 3 } else { 2 };
        let n = 24.min(if wraps > 0 { cap } else { next });
        let mut ev = Vec::new();
        for k in 0..n {
            let idx = (next + cap - n + k) % cap;
            let at = self.event_log + 16 + idx * 4 * words;
            let id = self.read_mem32(at);
            let data = self.read_mem32(at + 4 * (words - 1));
            ev.push(format!("{}:{:x}", id, data));
        }
        for chunk in ev.chunks(8) {
            wlog!("EVT: {}", chunk.join(" "));
        }
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
            let fh = self.r32(CSR_FH_INT_STATUS);
            self.w32(CSR_FH_INT_STATUS, fh);
            self.int_seen |= int;
            self.fh_seen |= fh;
            if int & (INT_SW_ERR | INT_HW_ERR) != 0 {
                wlog!("card error: CSR_INT {:#010x}{}", int, self.error_log());
                self.dump_fw_logs();
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
        // Frames' TX status frees their slot on the AP's queue.
        for p in &out {
            if p.cmd == TX_CMD && p.seq & 0x8000 == 0 && (p.seq >> 8) as usize & 0x1F == BSS_QUEUE {
                if let Some(t) = self.txq[BSS_QUEUE].as_mut() {
                    t.read = ((p.seq & 0xFF) as usize + 1) % TFD_QUEUE_SIZE;
                }
                let st = if p.data.len() >= 38 { le16(&p.data, 36) & 0xFF } else { 0 };
                if st == 1 || st == 2 {
                    self.tx_ok += 1;
                    if let Some(l) = self.link.as_mut() { l.tx_fail_run = 0; }
                } else {
                    self.tx_failed += 1;
                    if let Some(l) = self.link.as_mut() { l.tx_fail_run += 1; l.last_tx_status = st; }
                }
            }
        }
        // Replies free their command slot.
        for p in &out {
            if p.seq & 0x8000 == 0 && (p.seq >> 8) as usize & 0x1F == CMD_QUEUE {
                if let Some(t) = self.txq[CMD_QUEUE].as_mut() {
                    // Only a reply to a command still outstanding (some
                    // notifications come without the RX-frame bit).
                    let idx = (p.seq & 0xFF) as usize;
                    let outstanding = (t.write + TFD_QUEUE_SIZE - t.read) % TFD_QUEUE_SIZE;
                    if (idx + TFD_QUEUE_SIZE - t.read) % TFD_QUEUE_SIZE < outstanding {
                        t.read = (idx + 1) % TFD_QUEUE_SIZE;
                    }
                }
            }
        }
        out
    }

    /// Wait for a notification (one of `cmds`), keeping everything else.
    fn wait_for(&mut self, cmds: &[u8], timeout_us: u64) -> Option<Packet> {
        if let Some(i) = self.pending.iter().position(|p| p.group <= 1 && cmds.contains(&p.cmd)) {
            return self.pending.remove(i);
        }
        let deadline = crate::time::Deadline::after_us(timeout_us);
        loop {
            let mut found = None;
            for p in self.rx_poll() {
                if found.is_none() && cmds.contains(&p.cmd) && p.group <= 1 {
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
        if p.group > 1 {
            if self.pending.len() >= 64 {
                self.pending.pop_front();
            }
            self.pending.push_back(p);
            return;
        }
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
                wlog!("REPLY_ERROR (seq {:#06x}): {:02x?}", p.seq, &d[..d.len().min(24)]);
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
        // Always the wide header (with the length), legacy commands in
        // LONG_GROUP: Linux's mvm does (wide_cmd_header, DEF_ID), and the
        // firmware refuses some commands with the short one.
        let group = if group == 0 { 1 } else { group };
        let hdr_len = 8;
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
        // The scheduler's byte count for the slot (OpenBSD writes one for
        // commands too: CRC + delimiter, station 0).
        let tbl = self.bc_tbls.at(CMD_QUEUE * BC_TABLE_BYTES);
        unsafe {
            (tbl.add(idx * 2) as *mut u16).write_volatile(8);
            if idx < 64 {
                (tbl.add((TFD_QUEUE_SIZE + idx) * 2) as *mut u16).write_volatile(8);
            }
        }
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        self.int_seen = 0;
        self.fh_seen = 0;
        self.grab_nic_access();
        self.w32(HBUS_TARG_WRPTR, wr | (CMD_QUEUE as u32) << 8);
        let r = self.wait_reply(seq, group, cmd);
        self.release_nic_access();
        r
    }

    fn wait_reply(&mut self, seq: u16, group: u8, cmd: u8) -> Result<Vec<u8>, String> {

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
                self.dump_queues(seq);
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
        let mut r = self.up_inner();
        if let Err(e) = &r {
            if !self.steps.is_empty() {
                r = Err(format!("{} [steps: {}]", e, self.steps));
            }
        }
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
        if !self.skip.contains('B') {
            self.send_bt_config()?;
        }
        self.read_nvm()?;
        if !self.skip.contains('S') {
            self.send_sf_config()?; // as OpenBSD's iwm, before the calibrations
        }
        let (flow, event) = self.fw.as_ref().map_or((0, 0), |f| f.calib[1]);
        self.send_cmd(TX_ANT_CONFIGURATION_CMD, 0, &(self.valid_tx_ant as u32).to_le_bytes())?;
        self.send_phy_cfg(flow, event)?;
        self.wait_for(&[INIT_COMPLETE_NOTIF], 2_000_000)
            .ok_or_else(|| format!("INIT calibration didn't complete; {}", self.why()))?;
        wlog!("INIT done: {} PHY DB sections", self.phy_db.len());

        let inv: Vec<String> = self.phy_db.iter().map(|(t, g, d)| format!("{}/{}:{}", t, g, d.len())).collect();
        for chunk in inv.chunks(8) {
            wlog!("PHY DB (type/group:bytes): {}", chunk.join(" "));
        }

        // Runtime image. `skip` (WIFI UP <letters>) leaves steps out, to
        // find which one the firmware objects to: S smart FIFO, B Bluetooth,
        // D DQA, A aux station, P PHY contexts, M regulatory, C MAC context;
        // 1: antenna A only; O: PHY contexts before the aux station; N no
        // PHY DB; E/F: PHY_CONFIGURATION without event/flow calibrations;
        // X: load the separator section too; I: interrupt causes masked.
        let skip = self.skip.clone();
        let has = |c: char| skip.contains(c);
        if has('1') {
            self.valid_tx_ant = 1;
            self.valid_rx_ant = 1;
        }
        self.steps.clear();
        self.stop_device();
        self.start_hw()?;
        self.start_fw(false)?;
        if !has('S') {
            self.step("SF");
            self.send_sf_config()?;
        }
        self.step("TXANT");
        self.send_cmd(TX_ANT_CONFIGURATION_CMD, 0, &(self.valid_tx_ant as u32).to_le_bytes())?;
        if !has('N') {
            self.step("PHYDB");
            self.send_phy_db()?;
        }
        let (mut flow, mut event) = self.fw.as_ref().map_or((0, 0), |f| f.calib[0]);
        if has('E') {
            event = 0;
        }
        if has('F') {
            flow = 0;
        }
        self.step("PHYCFG");
        self.send_phy_cfg(flow, event)?;
        // Does the firmware survive what PHY_CONFIGURATION starts (runtime
        // calibrations) on its own, before anything touches the radio?
        let deadline = crate::time::Deadline::after_us(1_500_000);
        while !deadline.expired() && self.running {
            for p in self.rx_poll() {
                self.note(p);
            }
        }
        if !self.running {
            return Err(format!("firmware died after PHY_CONFIGURATION (flow {:#x} event {:#x}), nothing sent", flow, event));
        }
        wlog!("alive 1.5 s after PHY_CONFIGURATION (flow {:#x} event {:#x})", flow, event);
        if !has('B') {
            self.step("BT");
            self.send_bt_config()?;
        }
        if !has('D') && self.fw.as_ref().is_some_and(|f| f.capa(12)) {
            self.step("DQA");
            self.send_cmd(DQA_ENABLE_CMD, DATA_PATH_GROUP, &(CMD_QUEUE as u32).to_le_bytes())?;
        }
        if !has('A') && !has('O') {
            self.step("AUX");
            self.add_aux_sta()?;
        }
        // As Linux's iwl_mvm_up: every PHY context, on the first channel.
        if !has('P') {
            for id in 0..3 {
                self.step("PHYCTX");
                self.phy_context(id, 1, 1)?;
            }
            self.phy_added = true;
        }
        if has('O') && !has('A') {
            self.step("AUX");
            self.add_aux_sta()?;
        }
        self.step("POWER");
        self.send_cmd(POWER_TABLE_CMD, 0, &[0, 0, 0, 0])?; // always awake (CAM)
        if !has('M') {
            self.step("MCC");
            self.update_mcc()?;
        }
        // The interface coming up (mac80211's add_interface): MAC context 0.
        if !has('C') {
            self.step("MAC");
            self.add_mac_context()?;
            self.mac_added = true;
        }
        wlog!("runtime firmware up: MAC {}, NVM {:#x}, antennas TX {:#x} RX {:#x}",
            mac_text(&self.mac), self.nvm_version, self.valid_tx_ant, self.valid_rx_ant);
        Ok(())
    }

    fn step(&mut self, name: &str) {
        if !self.steps.is_empty() {
            self.steps.push(' ');
        }
        self.steps.push_str(name);
    }

    /// Smart FIFO, state INIT_OFF (no interface associated), with Linux's
    /// default watermarks and timers (iwl_mvm_sf_update at start-up).
    fn send_sf_config(&mut self) -> Result<(), String> {
        let mut c = Vec::with_capacity(92);
        let mut put = |v: u32| c.extend_from_slice(&v.to_le_bytes());
        put(3); // SF_INIT_OFF
        put(4096); // watermark[LONG_DELAY_ON]: SF_W_MARK_SCAN
        put(8192); // watermark[FULL_ON]: SF_W_MARK_MIMO2 (not associated)
        for _ in 0..10 {
            put(1_000_000); // long delay timeouts: SF_LONG_DELAY_AGING_TIMER
        }
        for _ in 0..5 {
            put(400); // aging timer (default)
            put(160); // idle timer (default)
        }
        self.send_cmd(0xD1, 0, &c)?;
        Ok(())
    }

    /// Bluetooth coexistence. The 7265 shares its antennas with its own
    /// Bluetooth core, which nothing drives here: BT_COEX_WIFI gives the
    /// antennas to Wi-Fi for good (OpenBSD's iwm does this). Linux's
    /// BT_COEX_NW (WIFI UP L) arbitrates with the Bluetooth core; with an
    /// idle core the firmware hit its watchdog on every radio command.
    fn send_bt_config(&mut self) -> Result<(), String> {
        let (mode, modules): (u32, u32) = if self.skip.contains('L') {
            let mplut = self.fw.as_ref().is_some_and(|f| f.capa(67));
            (1, (if mplut { 1 } else { 0 }) | 1 << 2 | 1 << 4)
        } else {
            (3, 1 << 4)
        };
        let mut c = Vec::new();
        c.extend_from_slice(&mode.to_le_bytes());
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

    /// PHY context `id` (a radio channel configuration) on `channel`, 20 MHz.
    fn phy_context(&mut self, id: u32, action: u32, channel: u8) -> Result<(), String> {
        let mut c = [0u8; 32];
        c[0..4].copy_from_slice(&id.to_le_bytes());
        c[4..8].copy_from_slice(&action.to_le_bytes());
        c[16] = if channel <= 14 { 1 } else { 0 }; // band: 2.4 / 5 GHz
        c[17] = channel;
        c[18] = 0; // 20 MHz
        c[19] = 0;
        c[20..24].copy_from_slice(&(self.valid_tx_ant as u32).to_le_bytes());
        let rx = self.valid_rx_ant as u32;
        c[24..28].copy_from_slice(&(rx << 1 | 2 << 10 | 2 << 12).to_le_bytes());
        self.send_cmd(PHY_CONTEXT_CMD, 0, &c)?;
        Ok(())
    }

    /// The station interface (MAC context 0), not associated.
    fn add_mac_context(&mut self) -> Result<(), String> {
        let c = self.mac_context_cmd(1, None, false, 0, 100, 1);
        if let Err(e) = self.send_cmd(MAC_CONTEXT_CMD, 0, &c) {
            for (i, chunk) in c.chunks(32).enumerate() {
                wlog!("MAC ctx +{:3}: {:02x?}", i * 32, chunk);
            }
            return Err(e);
        }
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
        } else if ty == 0 && frame[4..10] == self.mac && matches!(sub, 1 | 3 | 10 | 11 | 12) {
            if sub == 10 || sub == 12 {
                self.lost_link(frame);
            } else if self.mgmt_rx.len() < 16 {
                self.mgmt_rx.push_back(frame.to_vec());
            }
        } else if ty == 2 {
            self.rx_data_frame(frame, status);
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
        if let Some(l) = &self.link {
            v.push(format!("  network '{}' {} ch {}, {}, {}", l.bss.ssid_text(), mac_text(&l.bss.bssid), l.bss.channel,
                if l.wpa { "WPA2" } else { "open" }, if l.authorized { "link UP" } else if l.associated { "associated, keys pending" } else { "joining" }));
            v.push(format!("  rate {} Mb/s; TX ok {} failed {}; RX {} dropped {}", l.rate_500k as f32 / 2.0,
                self.tx_ok, self.tx_failed, self.rx_data, self.rx_dropped));
        }
        v
    }
}

// ---- Joining a network --------------------------------------------------------------

/// The network joined (or being joined).
pub struct Link {
    pub bss: Bss,
    pub aid: u16,
    pub associated: bool,
    /// Data may flow: open network associated, or WPA2 keys installed.
    pub authorized: bool,
    pub wpa: bool,
    /// Data rate (rate_n_flags), from the AP's rates and the signal.
    pub rate: u32,
    rate_500k: u8,
    pub tx_fail_run: u32,
    pub last_tx_status: u16,
    pmk: [u8; 32],
    ptk: Option<crate::wpa::Ptk>,
    anonce: [u8; 32],
    snonce: [u8; 32],
    replay: Option<u64>,
    /// Our RSN element as sent in the association request.
    rsn_ie: Vec<u8>,
    tx_pn: u64,
    gtk_id: u8,
    last_rx_seq: u16,
    pub since_ms: u64,
}

/// Words of a command line, "double quotes" keeping spaces in one word.
pub fn quoted_words(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let (mut quoted, mut any) = (false, false);
    for c in s.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                any = true;
            }
            ' ' | '\t' if !quoted => {
                if any {
                    out.push(core::mem::take(&mut cur));
                    any = false;
                }
            }
            _ => {
                cur.push(c);
                any = true;
            }
        }
    }
    if any {
        out.push(cur);
    }
    out
}

/// Legacy rate (500 kb/s units) to the firmware's PLCP value, CCK flag.
fn plcp(rate: u8) -> Option<(u32, bool)> {
    Some(match rate {
        2 => (10, true),
        4 => (20, true),
        11 => (55, true),
        22 => (110, true),
        12 => (13, false),
        18 => (15, false),
        24 => (5, false),
        36 => (7, false),
        48 => (9, false),
        72 => (11, false),
        96 => (1, false),
        108 => (3, false),
        _ => return None,
    })
}

impl Iwl {
    fn rate_n_flags(&self, rate: u8) -> u32 {
        let (p, cck) = plcp(rate).unwrap_or((13, false));
        p | if cck { 1 << 9 } else { 0 } | (self.first_tx_ant() as u32) << 14
    }

    /// Lowest basic rate for management frames: 1 Mb/s on 2.4 GHz, 6 on 5.
    fn mgmt_rate(&self, channel: u8) -> u32 {
        self.rate_n_flags(if channel <= 14 { 2 } else { 12 })
    }

    /// A data rate from what the AP supports and how loud it is.
    fn pick_rate(bss: &Bss) -> u8 {
        let want = match bss.signal {
            s if s >= -62 => 108,
            s if s >= -68 => 72,
            s if s >= -74 => 48,
            s if s >= -80 => 24,
            s if s >= -85 => 12,
            _ => 2,
        };
        let mut rates: Vec<u8> = bss.rates.iter().map(|r| r & 0x7F).filter(|&r| plcp(r).is_some()).collect();
        if rates.is_empty() {
            rates = if bss.channel <= 14 { vec![2, 4, 11, 22] } else { vec![12, 24, 48] };
        }
        rates.sort();
        rates.iter().rev().find(|&&r| r <= want).copied().unwrap_or(rates[0])
    }

    /// Step the data rate down after repeated losses (and keep a floor).
    fn adapt_rate(&mut self) {
        let Some(l) = self.link.as_ref() else { return };
        if l.tx_fail_run < 4 {
            return;
        }
        let mut rates: Vec<u8> = l.bss.rates.iter().map(|r| r & 0x7F).filter(|&r| plcp(r).is_some()).collect();
        rates.sort();
        let cur = l.rate_500k;
        if let Some(&lower) = rates.iter().rev().find(|&&r| r < cur) {
            let rnf = self.rate_n_flags(lower);
            let l = self.link.as_mut().unwrap();
            wlog!("TX: {} losses in a row (status {:#x}); rate {} -> {} Mb/s", l.tx_fail_run, l.last_tx_status,
                cur as f32 / 2.0, lower as f32 / 2.0);
            l.rate_500k = lower;
            l.rate = rnf;
            l.tx_fail_run = 0;
        }
    }

    /// Queue an 802.11 frame on the AP's queue. `key`: CCMP with this TK
    /// (the frame already has its CCMP header).
    fn tx_frame(&mut self, frame: &[u8], hdr_len: usize, rate: u32, tid: u8, pm_timeout: u16, key: Option<[u8; 16]>) -> Result<(), String> {
        let t = self.txq[BSS_QUEUE].as_mut().ok_or("no AP queue")?;
        let used = (t.write + TFD_QUEUE_SIZE - t.read) % TFD_QUEUE_SIZE;
        if used >= BSS_QUEUE_SLOTS - 1 {
            return Err(String::from("TX queue full"));
        }
        let idx = t.write;
        let slot = idx % BSS_QUEUE_SLOTS;
        let buf = t.bufs.at(slot * t.buf_size);
        let buf_phys = t.bufs.phys + (slot * t.buf_size) as u64;
        let padded = (hdr_len + 3) & !3;
        let total = 4 + 56 + padded + (frame.len() - hdr_len);
        if total > t.buf_size {
            return Err(String::from("frame too big"));
        }
        let unicast = frame[4] & 1 == 0;
        let mut flags: u32 = 1 << 13; // the firmware numbers the frames (SEQ_CTL)
        if unicast {
            flags |= 1 << 3; // ACK
        }
        if padded != hdr_len {
            flags |= 1 << 20; // MH_PAD
        }
        let mut c = [0u8; 56];
        c[0..2].copy_from_slice(&(frame.len() as u16).to_le_bytes());
        c[4..8].copy_from_slice(&flags.to_le_bytes());
        c[12..16].copy_from_slice(&rate.to_le_bytes());
        c[16] = AP_STA_ID;
        if let Some(k) = key {
            c[17] = 2; // TX_CMD_SEC_CCM
            c[20..36].copy_from_slice(&k);
        }
        c[40..44].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes()); // lifetime: infinite
        let scratch = buf_phys + 4 + 8;
        c[44..48].copy_from_slice(&(scratch as u32).to_le_bytes());
        c[48] = (scratch >> 32) as u8;
        c[49] = 60; // RTS retries
        c[50] = 15; // data retries
        c[51] = tid;
        c[52..54].copy_from_slice(&pm_timeout.to_le_bytes());
        let seq = ((BSS_QUEUE as u16) << 8) | idx as u16;
        unsafe {
            let h = [TX_CMD, 0, seq as u8, (seq >> 8) as u8];
            core::ptr::copy_nonoverlapping(h.as_ptr(), buf, 4);
            core::ptr::copy_nonoverlapping(c.as_ptr(), buf.add(4), 56);
            core::ptr::copy_nonoverlapping(frame.as_ptr(), buf.add(60), hdr_len);
            core::ptr::write_bytes(buf.add(60 + hdr_len), 0, padded - hdr_len);
            core::ptr::copy_nonoverlapping(frame.as_ptr().add(hdr_len), buf.add(60 + padded), frame.len() - hdr_len);
        }
        let tfd = t.tfds.at(idx * TFD_SIZE);
        unsafe { core::ptr::write_bytes(tfd, 0, TFD_SIZE) };
        let mut tbs = vec![(buf_phys, FIRST_TB_SIZE), (buf_phys + FIRST_TB_SIZE as u64, 60 + padded - FIRST_TB_SIZE)];
        if frame.len() > hdr_len {
            tbs.push((buf_phys + (60 + padded) as u64, frame.len() - hdr_len));
        }
        write_tfd(tfd, &tbs);
        // Byte count for the scheduler: frame + CRC + delimiter (+ MIC).
        let bc = (frame.len() + 8 + if key.is_some() { 8 } else { 0 }) as u16 | (AP_STA_ID as u16) << 12;
        let tbl = self.bc_tbls.at(BSS_QUEUE * BC_TABLE_BYTES);
        unsafe {
            (tbl.add(idx * 2) as *mut u16).write_volatile(bc);
            if idx < 64 {
                (tbl.add((TFD_QUEUE_SIZE + idx) * 2) as *mut u16).write_volatile(bc);
            }
        }
        let t = self.txq[BSS_QUEUE].as_mut().unwrap();
        t.write = (idx + 1) % TFD_QUEUE_SIZE;
        let wr = t.write as u32;
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        self.w32(HBUS_TARG_WRPTR, wr | (BSS_QUEUE as u32) << 8);
        Ok(())
    }

    fn mgmt_header(&self, subtype: u16, bssid: &[u8; 6]) -> Vec<u8> {
        let mut f = Vec::with_capacity(128);
        f.extend_from_slice(&(subtype << 4).to_le_bytes());
        f.extend_from_slice(&[0, 0]);
        f.extend_from_slice(bssid);
        f.extend_from_slice(&self.mac);
        f.extend_from_slice(bssid);
        f.extend_from_slice(&[0, 0]);
        f
    }

    /// Wait for a management frame of `subtype` from `bssid`.
    fn wait_mgmt(&mut self, subtype: u16, bssid: &[u8; 6], timeout_us: u64) -> Option<Vec<u8>> {
        let deadline = crate::time::Deadline::after_us(timeout_us);
        loop {
            for p in self.rx_poll() {
                self.note(p);
            }
            while let Some(f) = self.mgmt_rx.pop_front() {
                if le16(&f, 0) >> 4 & 0xF == subtype && f[10..16] == bssid[..] {
                    return Some(f);
                }
            }
            if deadline.expired() || !self.running {
                return None;
            }
            core::hint::spin_loop();
        }
    }

    /// Join `ssid`: open, or WPA2-PSK with `pass`. Blocks until the link is
    /// up (keys installed) or it fails.
    pub fn connect(&mut self, ssid: &[u8], pass: Option<&[u8]>) -> Result<(), String> {
        if !self.running {
            return Err(format!("card not up ({})", self.state));
        }
        if self.link.is_some() {
            self.disconnect("joining another network");
        }
        let mut found = self.best_bss(ssid);
        if found.is_none() {
            self.scan()?;
            found = self.best_bss(ssid);
        }
        let bss = found.ok_or_else(|| format!("no network '{}' found by the scan", String::from_utf8_lossy(ssid)))?;
        let wpa = match bss.security() {
            "open" => false,
            "WPA2-PSK" => true,
            other => return Err(format!("'{}' uses {}; only open and WPA2-Personal are supported", bss.ssid_text(), other)),
        };
        let mut rsn_ie = Vec::new();
        let mut pmk = [0u8; 32];
        if wpa {
            let rsn = bss.rsn.clone().unwrap_or_default();
            let group = rsn.get(2..6).map(|g| g.to_vec()).unwrap_or_default();
            if group != [0x00, 0x0F, 0xAC, 0x04] {
                return Err(format!("the network's group cipher is {:02x?}, not AES-CCMP (set the router to WPA2-AES)", group));
            }
            if !rsn_pairwise_has(&rsn, 4) {
                return Err(String::from("the network doesn't offer AES-CCMP (set the router to WPA2-AES)"));
            }
            let pass = pass.ok_or("this network needs a password: WIFI CONNECT <ssid> <password>")?;
            if pass.len() == 64 && pass.iter().all(|c| c.is_ascii_hexdigit()) {
                for i in 0..32 {
                    let h = core::str::from_utf8(&pass[2 * i..2 * i + 2]).unwrap_or("00");
                    pmk[i] = u8::from_str_radix(h, 16).unwrap_or(0);
                }
            } else if (8..=63).contains(&pass.len()) {
                pmk = crate::wpa::pmk(pass, &bss.ssid);
            } else {
                return Err(String::from("a WPA2 password is 8 to 63 characters"));
            }
            // version 1, group CCMP, 1 pairwise CCMP, 1 AKM PSK, capabilities 0
            rsn_ie = vec![48, 20, 1, 0, 0x00, 0x0F, 0xAC, 4, 1, 0, 0x00, 0x0F, 0xAC, 4, 1, 0, 0x00, 0x0F, 0xAC, 2, 0, 0];
        }
        let rate_500k = Self::pick_rate(&bss);
        let mut snonce = [0u8; 32];
        crate::entropy::fill(&mut snonce);
        wlog!("joining '{}' {} ch {} {} dBm, {}; data rate {} Mb/s", bss.ssid_text(), mac_text(&bss.bssid),
            bss.channel, bss.signal, if wpa { "WPA2-PSK" } else { "open" }, rate_500k as f32 / 2.0);
        self.link = Some(Link {
            rate: self.rate_n_flags(rate_500k),
            rate_500k, bss: bss.clone(), aid: 0, associated: false, authorized: false, wpa,
            tx_fail_run: 0, last_tx_status: 0, pmk, ptk: None, anonce: [0; 32], snonce,
            replay: None, rsn_ie, tx_pn: 1, gtk_id: 0, last_rx_seq: 0xFFFF, since_ms: crate::time::now_ms(),
        });
        self.state = format!("joining {}", bss.ssid_text());
        let r = self.join(&bss);
        match &r {
            Ok(()) => {
                self.state = format!("connected to {}", bss.ssid_text());
                wlog!("connected to '{}' (AID {})", bss.ssid_text(), self.link.as_ref().map_or(0, |l| l.aid));
            }
            Err(e) => {
                wlog!("join failed: {}", e);
                self.disconnect("join failed");
            }
        }
        r
    }

    fn best_bss(&self, ssid: &[u8]) -> Option<Bss> {
        self.scan_results.iter().filter(|b| b.ssid == ssid).max_by_key(|b| b.signal).cloned()
    }

    fn join(&mut self, bss: &Bss) -> Result<(), String> {
        // Firmware side: the channel (PHY context), the MAC bound to it, the
        // BSSID, the AP as station 1 with queue 4, time on the channel.
        if !self.mac_added {
            self.add_mac_context()?;
            self.mac_added = true;
        }
        self.phy_context(PHY_ID, if self.phy_added { 2 } else { 1 }, bss.channel)?;
        self.phy_added = true;

        let mut b = [0u8; 24];
        b[0..4].copy_from_slice(&PHY_ID.to_le_bytes());
        b[4..8].copy_from_slice(&1u32.to_le_bytes()); // add
        b[8..12].copy_from_slice(&MAC_ID.to_le_bytes());
        b[12..16].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
        b[16..20].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
        b[20..24].copy_from_slice(&PHY_ID.to_le_bytes());
        let r = self.send_cmd(BINDING_CONTEXT_CMD, 0, &b)?;
        if r.len() >= 4 && le32(&r, 0) != 0 {
            return Err(format!("binding refused (status {:#x})", le32(&r, 0)));
        }

        let m = self.mac_context_cmd(2, Some(bss.bssid), false, 0, bss.beacon_int, 1);
        self.send_cmd(MAC_CONTEXT_CMD, 0, &m)?;

        self.enable_txq(BSS_QUEUE, AP_STA_ID, 8, TX_FIFO_VO)?;
        self.add_ap_sta(&bss.bssid, false, 0)?;

        // Stay on the channel through authentication and association.
        let mut te = [0u8; 36];
        te[0..4].copy_from_slice(&MAC_ID.to_le_bytes());
        te[4..8].copy_from_slice(&1u32.to_le_bytes()); // add
        te[8..12].copy_from_slice(&0u32.to_le_bytes()); // TE_BSS_STA_AGGRESSIVE_ASSOC
        te[16..20].copy_from_slice(&500u32.to_le_bytes()); // max delay (TU)
        te[24..28].copy_from_slice(&1u32.to_le_bytes()); // interval
        te[28..32].copy_from_slice(&1500u32.to_le_bytes()); // duration (TU)
        te[32] = 1; // repeat
        te[34..36].copy_from_slice(&(1u16 | 2 | 1 << 11).to_le_bytes()); // notify start/end, start now
        let r = self.send_cmd(TIME_EVENT_CMD, 0, &te)?;
        if r.len() >= 4 && le32(&r, 0) != 0 {
            wlog!("time event refused (status {:#x})", le32(&r, 0));
        }
        match self.wait_for(&[TIME_EVENT_NOTIFICATION], 700_000) {
            Some(n) if n.data.len() >= 24 => wlog!("on channel {}: time event action {:#x} status {}", bss.channel,
                le32(&n.data, 16), le32(&n.data, 20)),
            _ => wlog!("no time event start; trying anyway"),
        }

        // 802.11 open-system authentication.
        let mgmt_rate = self.mgmt_rate(bss.channel);
        let mut auth = None;
        for attempt in 1..=3 {
            let mut f = self.mgmt_header(11, &bss.bssid);
            f.extend_from_slice(&[0, 0, 1, 0, 0, 0]); // algorithm 0, transaction 1, status 0
            self.tx_frame(&f, 24, mgmt_rate, 8, 2, None)?;
            if let Some(r) = self.wait_mgmt(11, &bss.bssid, 400_000) {
                auth = Some(r);
                break;
            }
            wlog!("no authentication reply (try {}, TX ok {} failed {})", attempt, self.tx_ok, self.tx_failed);
        }
        let auth = auth.ok_or("the AP didn't answer authentication")?;
        let status = if auth.len() >= 30 { le16(&auth, 28) } else { 0xFFFF };
        if status != 0 {
            return Err(format!("authentication refused (status {})", status));
        }
        wlog!("authenticated");

        // Association: no QoS, no HT (legacy rates), RSN if WPA2.
        let mut f = self.mgmt_header(0, &bss.bssid);
        let mut cap: u16 = 0x0001 | 0x0020; // ESS, short preamble
        if bss.channel <= 14 {
            cap |= 0x0400; // short slot time
        }
        if bss.privacy {
            cap |= 0x0010;
        }
        f.extend_from_slice(&cap.to_le_bytes());
        f.extend_from_slice(&10u16.to_le_bytes()); // listen interval
        f.push(0);
        f.push(bss.ssid.len() as u8);
        f.extend_from_slice(&bss.ssid);
        let rates: Vec<u8> = if bss.rates.is_empty() {
            if bss.channel <= 14 { vec![0x82, 0x84, 0x8B, 0x96, 0x0C, 0x12, 0x18, 0x24, 0x30, 0x48, 0x60, 0x6C] }
            else { vec![0x8C, 0x12, 0x98, 0x24, 0xB0, 0x48, 0x60, 0x6C] }
        } else {
            bss.rates.clone()
        };
        f.push(1);
        f.push(rates.len().min(8) as u8);
        f.extend_from_slice(&rates[..rates.len().min(8)]);
        if rates.len() > 8 {
            f.push(50);
            f.push((rates.len() - 8) as u8);
            f.extend_from_slice(&rates[8..]);
        }
        let rsn_ie = self.link.as_ref().map(|l| l.rsn_ie.clone()).unwrap_or_default();
        f.extend_from_slice(&rsn_ie);
        let mut assoc = None;
        for attempt in 1..=3 {
            self.tx_frame(&f, 24, mgmt_rate, 8, 3, None)?;
            if let Some(r) = self.wait_mgmt(1, &bss.bssid, 600_000) {
                assoc = Some(r);
                break;
            }
            wlog!("no association reply (try {})", attempt);
        }
        let assoc = assoc.ok_or("the AP didn't answer association")?;
        if assoc.len() < 30 {
            return Err(String::from("short association reply"));
        }
        let status = le16(&assoc, 26);
        if status != 0 {
            return Err(format!("association refused (status {}{})", status,
                match status { 17 => ": AP full", 18 => ": rates", 27 | 45 => ": needs HT/VHT", 40 | 41 | 42 => ": RSN/cipher", 43 => ": AKM", _ => "" }));
        }
        let aid = le16(&assoc, 28) & 0x3FFF;
        wlog!("associated, AID {}", aid);

        // The firmware: associated (no beacons to the host), AID, air time.
        let m = self.mac_context_cmd(2, Some(bss.bssid), true, aid, bss.beacon_int, 1);
        self.send_cmd(MAC_CONTEXT_CMD, 0, &m)?;
        self.add_ap_sta(&bss.bssid, true, aid)?;
        let mut q = [0u8; 48];
        q[0..4].copy_from_slice(&PHY_ID.to_le_bytes()); // binding id = PHY id
        q[4..8].copy_from_slice(&128u32.to_le_bytes());
        q[8..12].copy_from_slice(&128u32.to_le_bytes());
        for i in 1..4 {
            q[i * 12..i * 12 + 4].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
        }
        self.send_cmd(TIME_QUOTA_CMD, 0, &q)?;
        if let Some(l) = self.link.as_mut() {
            l.aid = aid;
            l.associated = true;
        }

        let wpa = self.link.as_ref().is_some_and(|l| l.wpa);
        if !wpa {
            if let Some(l) = self.link.as_mut() {
                l.authorized = true;
            }
            return Ok(());
        }
        // The 4-way handshake: the AP starts it.
        let deadline = crate::time::Deadline::after_us(8_000_000);
        loop {
            for p in self.rx_poll() {
                self.note(p);
            }
            while let Some(e) = self.eapol_rx.pop_front() {
                self.handle_eapol(&e);
            }
            if self.link.as_ref().is_some_and(|l| l.authorized) {
                return Ok(());
            }
            if self.link.is_none() {
                return Err(String::from("the AP dropped us during the handshake (wrong password?)"));
            }
            if deadline.expired() {
                let got = self.link.as_ref().map_or(0, |l| if l.ptk.is_some() { 1 } else { 0 });
                return Err(if got == 0 {
                    String::from("no WPA handshake from the AP")
                } else {
                    String::from("handshake not finished (wrong password?)")
                });
            }
            core::hint::spin_loop();
        }
    }

    fn add_ap_sta(&mut self, bssid: &[u8; 6], modify: bool, aid: u16) -> Result<(), String> {
        let mut c = [0u8; 44];
        c[0] = modify as u8;
        c[2..4].copy_from_slice(&0xFFFFu16.to_le_bytes()); // no aggregation
        c[4..8].copy_from_slice(&MAC_ID.to_le_bytes());
        c[8..14].copy_from_slice(bssid);
        c[16] = AP_STA_ID;
        let msk: u32 = 3 << 26 | 3 << 28 | 1 << 17;
        c[24..28].copy_from_slice(&msk.to_le_bytes());
        c[36..38].copy_from_slice(&aid.to_le_bytes());
        c[40..44].copy_from_slice(&(1u32 << BSS_QUEUE).to_le_bytes());
        self.send_cmd_status(ADD_STA, &c, 1)
    }

    /// An EAPOL-Key frame from the AP (4-way and group key handshakes).
    fn handle_eapol(&mut self, e: &[u8]) {
        if e.len() < 99 || e[1] != 3 || e[4] != 2 {
            return;
        }
        let info = u16::from_be_bytes([e[5], e[6]]);
        let replay = u64::from_be_bytes(e[9..17].try_into().unwrap());
        let data_len = u16::from_be_bytes([e[97], e[98]]) as usize;
        let Some(key_data) = e.get(99..99 + data_len) else { return };
        let Some(link) = self.link.as_ref() else { return };
        if info & 7 != 2 {
            wlog!("EAPOL: key descriptor version {} not supported", info & 7);
            return;
        }
        if link.replay.is_some_and(|r| replay < r) {
            return; // old
        }
        let pairwise = info & 0x08 != 0;
        let has_mic = info & 0x100 != 0;
        let ack = info & 0x80 != 0;
        if pairwise && ack && !has_mic {
            // Message 1: our nonce, the PTK, message 2.
            let mut anonce = [0u8; 32];
            anonce.copy_from_slice(&e[17..49]);
            let ptk = crate::wpa::ptk(&link.pmk, &link.bss.bssid, &self.mac, &anonce, &link.snonce);
            let kck = ptk.kck;
            let rsn = link.rsn_ie.clone();
            let snonce = link.snonce;
            let l = self.link.as_mut().unwrap();
            l.anonce = anonce;
            l.ptk = Some(ptk);
            l.replay = Some(replay);
            let msg2 = eapol_key(0x010A, replay, &snonce, &rsn, &kck);
            wlog!("EAPOL 1/4 received; sending 2/4");
            let _ = self.send_eapol(&msg2, false);
        } else if pairwise && ack && has_mic {
            // Message 3: check the MIC, unwrap the GTK, message 4, keys.
            let Some(ptk) = link.ptk.as_ref() else { return };
            if !mic_ok(e, &ptk.kck) {
                wlog!("EAPOL 3/4: bad MIC (wrong password?)");
                return;
            }
            if e[17..49] != link.anonce {
                wlog!("EAPOL 3/4: ANonce differs from 1/4");
                return;
            }
            let (kek, kck, tk) = (ptk.kek, ptk.kck, ptk.tk);
            let gtk = if info & 0x1000 != 0 {
                crate::wpa::aes_unwrap(&kek, key_data).and_then(|d| crate::wpa::find_gtk(&d))
            } else {
                crate::wpa::find_gtk(key_data)
            };
            self.link.as_mut().unwrap().replay = Some(replay);
            let msg4 = eapol_key(0x030A, replay, &[0; 32], &[], &kck);
            wlog!("EAPOL 3/4 ok (GTK {}); sending 4/4", if gtk.is_some() { "found" } else { "MISSING" });
            let _ = self.send_eapol(&msg4, false);
            // Let 4/4 leave unencrypted before the pairwise key is in.
            crate::time::delay_ms(5);
            for p in self.rx_poll() {
                self.note(p);
            }
            if let Err(err) = self.install_key(0, 0, &tk, false) {
                wlog!("installing the pairwise key failed: {}", err);
                return;
            }
            if let Some((id, key)) = gtk {
                if let Err(err) = self.install_gtk(id, &key, &e[65..73]) {
                    wlog!("installing the group key failed: {}", err);
                }
            }
            if let Some(l) = self.link.as_mut() {
                l.authorized = true;
                wlog!("WPA2 keys installed: link up");
            }
        } else if !pairwise && ack && has_mic {
            // Group key handshake (the router changing its group key).
            let Some(ptk) = link.ptk.as_ref() else { return };
            if !mic_ok(e, &ptk.kck) {
                wlog!("EAPOL group 1/2: bad MIC");
                return;
            }
            let (kek, kck) = (ptk.kek, ptk.kck);
            let gtk = crate::wpa::aes_unwrap(&kek, key_data).and_then(|d| crate::wpa::find_gtk(&d));
            self.link.as_mut().unwrap().replay = Some(replay);
            let msg = eapol_key(0x0302, replay, &[0; 32], &[], &kck);
            let _ = self.send_eapol(&msg, true);
            if let Some((id, key)) = gtk {
                match self.install_gtk(id, &key, &e[65..73]) {
                    Ok(()) => wlog!("new group key {} installed", id),
                    Err(err) => wlog!("installing the new group key failed: {}", err),
                }
            }
        }
    }

    fn install_gtk(&mut self, id: u8, key: &[u8], rsc: &[u8]) -> Result<(), String> {
        if key.len() != 16 {
            return Err(format!("group key of {} bytes (not CCMP)", key.len()));
        }
        let mut k = [0u8; 16];
        k.copy_from_slice(key);
        let mut seq = [0u8; 6];
        seq.copy_from_slice(&rsc[..6]);
        self.install_key(4 + id, id, &k, true)?;
        let _ = seq;
        if let Some(l) = self.link.as_mut() {
            l.gtk_id = id;
        }
        Ok(())
    }

    /// ADD_STA_KEY (version 1): a CCMP key for the AP's station.
    fn install_key(&mut self, offset: u8, id: u8, key: &[u8; 16], group: bool) -> Result<(), String> {
        let mut c = [0u8; 64];
        c[0] = AP_STA_ID;
        c[1] = offset;
        let flags: u16 = 2 | 1 << 3 | (id as u16 & 3) << 8 | if group { 1 << 14 } else { 0 };
        c[2..4].copy_from_slice(&flags.to_le_bytes());
        c[4..20].copy_from_slice(key);
        self.send_cmd_status(ADD_STA_KEY, &c, 1)
    }

    /// Send an EAPOL frame to the AP (encrypted once the PTK is in).
    fn send_eapol(&mut self, eapol: &[u8], encrypt: bool) -> Result<(), String> {
        let bssid = self.link.as_ref().ok_or("no link")?.bss.bssid;
        self.send_data(&bssid, 0x888E, eapol, encrypt, 2)
    }

    /// An Ethernet payload to `dst` through the AP: 802.11 data header
    /// (to DS), CCMP header if encrypting, LLC/SNAP, payload.
    fn send_data(&mut self, dst: &[u8; 6], ethertype: u16, payload: &[u8], encrypt: bool, pm_timeout: u16) -> Result<(), String> {
        let link = self.link.as_mut().ok_or("no link")?;
        let tk = if encrypt { link.ptk.as_ref().map(|p| p.tk) } else { None };
        let mut f = Vec::with_capacity(payload.len() + 48);
        let fc: u16 = 0x0008 | 0x0100 | if tk.is_some() { 0x4000 } else { 0 };
        f.extend_from_slice(&fc.to_le_bytes());
        f.extend_from_slice(&[0, 0]);
        f.extend_from_slice(&link.bss.bssid);
        f.extend_from_slice(&self.mac);
        f.extend_from_slice(dst);
        f.extend_from_slice(&[0, 0]);
        if tk.is_some() {
            let pn = link.tx_pn;
            link.tx_pn += 1;
            f.extend_from_slice(&[pn as u8, (pn >> 8) as u8, 0, 0x20, (pn >> 16) as u8, (pn >> 24) as u8, (pn >> 32) as u8, (pn >> 40) as u8]);
        }
        f.extend_from_slice(&[0xAA, 0xAA, 0x03, 0, 0, 0]);
        f.extend_from_slice(&ethertype.to_be_bytes());
        f.extend_from_slice(payload);
        let rate = link.rate;
        self.tx_frame(&f, 24, rate, 0, pm_timeout, tk)
    }

    /// A data frame from the air: the AP's frames for us become Ethernet
    /// frames (or EAPOL for the handshake).
    fn rx_data_frame(&mut self, frame: &[u8], status: u32) {
        let fc = le16(frame, 0);
        let Some(link) = self.link.as_mut() else { return };
        if fc & 0x0300 != 0x0200 || frame[10..16] != link.bss.bssid {
            return; // not from our AP (from DS only)
        }
        let sub = fc >> 4 & 0xF;
        if sub & 4 != 0 {
            return; // no data (null frames)
        }
        let hdr = if sub & 8 != 0 { 26 } else { 24 } + if fc & 0x8000 != 0 && sub & 8 != 0 { 4 } else { 0 };
        let seq = le16(frame, 22);
        if fc & 0x0800 != 0 && seq == link.last_rx_seq {
            return; // a retry of what we had
        }
        link.last_rx_seq = seq;
        let body = if fc & 0x4000 != 0 {
            // Decrypted by the card: CCM, MIC checked; the CCMP header stays,
            // the MIC is still on the end.
            if status & 0x700 != 0x200 || status & (1 << 6) == 0 || frame.len() < hdr + 16 {
                self.rx_dropped += 1;
                return;
            }
            &frame[hdr + 8..frame.len() - 8]
        } else {
            if frame.len() < hdr {
                return;
            }
            &frame[hdr..]
        };
        if body.len() < 8 || body[..6] != [0xAA, 0xAA, 0x03, 0, 0, 0] {
            self.rx_dropped += 1;
            return;
        }
        let ethertype = u16::from_be_bytes([body[6], body[7]]);
        let payload = &body[8..];
        if ethertype == 0x888E {
            if self.eapol_rx.len() < 8 {
                self.eapol_rx.push_back(payload.to_vec());
            }
            return;
        }
        // Until the keys are in, only EAPOL; after, nothing unencrypted.
        if !link.authorized || (link.wpa && fc & 0x4000 == 0) {
            self.rx_dropped += 1;
            return;
        }
        if frame[16..22] == self.mac {
            return; // our own broadcast, relayed back by the AP
        }
        let mut eth = Vec::with_capacity(payload.len() + 14);
        eth.extend_from_slice(&frame[4..10]); // DA
        eth.extend_from_slice(&frame[16..22]); // SA
        eth.extend_from_slice(&ethertype.to_be_bytes());
        eth.extend_from_slice(payload);
        self.rx_data += 1;
        if self.eth_rx.len() >= 256 {
            self.eth_rx.pop_front();
        }
        self.eth_rx.push_back(eth);
    }

    /// Deauthentication or disassociation from the AP.
    fn lost_link(&mut self, frame: &[u8]) {
        let Some(link) = self.link.as_ref() else { return };
        if frame[10..16] != link.bss.bssid {
            return;
        }
        let reason = if frame.len() >= 26 { le16(frame, 24) } else { 0 };
        wlog!("{} by the AP, reason {}{}", if le16(frame, 0) >> 4 & 0xF == 12 { "deauthenticated" } else { "disassociated" },
            reason, match reason { 2 | 15 => " (handshake timeout: wrong password?)", 14 => " (MIC failure)", 23 => " (802.1X failed)", _ => "" });
        self.drop_link();
        self.state = format!("disconnected by the AP (reason {})", reason);
    }

    /// Leave the network: tell the AP, then undo the firmware's state.
    pub fn disconnect(&mut self, why: &str) {
        let Some(link) = self.link.as_ref() else { return };
        if link.associated && self.running {
            let mut f = self.mgmt_header(12, &link.bss.bssid);
            f.extend_from_slice(&3u16.to_le_bytes()); // leaving
            let rate = self.mgmt_rate(link.bss.channel);
            let _ = self.tx_frame(&f, 24, rate, 8, 2, None);
            crate::time::delay_ms(20);
        }
        wlog!("leaving the network ({})", why);
        self.drop_link();
        self.state = String::from("ready (not connected)");
    }

    fn drop_link(&mut self) {
        let Some(link) = self.link.take() else { return };
        self.eth_rx.clear();
        self.eapol_rx.clear();
        if !self.running {
            return;
        }
        // Undo in reverse: quota, station, MAC back to unassociated, binding.
        let mut q = [0u8; 48];
        for i in 0..4 {
            q[i * 12..i * 12 + 4].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
        }
        let _ = self.send_cmd(TIME_QUOTA_CMD, 0, &q);
        let mut r = [0u8; 4];
        r[0] = AP_STA_ID;
        let _ = self.send_cmd(0x19, 0, &r); // REMOVE_STA
        let m = self.mac_context_cmd(2, None, false, 0, 0, 0);
        let _ = self.send_cmd(MAC_CONTEXT_CMD, 0, &m);
        let mut b = [0u8; 24];
        b[0..4].copy_from_slice(&PHY_ID.to_le_bytes());
        b[4..8].copy_from_slice(&3u32.to_le_bytes()); // remove
        b[8..12].copy_from_slice(&MAC_ID.to_le_bytes());
        b[12..16].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
        b[16..20].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
        b[20..24].copy_from_slice(&PHY_ID.to_le_bytes());
        let _ = self.send_cmd(BINDING_CONTEXT_CMD, 0, &b);
        // The AP's queue: the firmware stops scheduling it.
        let cmd = [0, AP_STA_ID, 8, BSS_QUEUE as u8, 0, 0, TX_FIFO_VO, 64, 0, 0, 0, 0];
        let _ = self.send_cmd(SCD_QUEUE_CFG, 0, &cmd);
        let _ = link;
    }

    /// Keep things moving while connected (called from the network poll):
    /// received frames, the group key handshake, missed beacons.
    pub fn service(&mut self) {
        if !self.running {
            return;
        }
        for p in self.rx_poll() {
            if p.cmd == MISSED_BEACONS_NOTIFICATION && p.data.len() >= 12 {
                // mac_id, missed since the last frame received, missed in a row
                let consecutive = le32(&p.data, 4);
                if consecutive >= 30 && self.link.is_some() {
                    wlog!("{} beacons missed in a row: link lost", consecutive);
                    self.drop_link();
                    self.state = String::from("link lost (AP out of range?)");
                }
                continue;
            }
            self.note(p);
        }
        while let Some(e) = self.eapol_rx.pop_front() {
            self.handle_eapol(&e);
        }
        self.adapt_rate();
        // Old notifications nobody waits for.
        while self.pending.len() > 16 {
            self.pending.pop_front();
        }
    }

    /// An Ethernet frame from the network stack, out to the AP.
    pub fn transmit_eth(&mut self, eth: &[u8]) {
        if eth.len() < 14 || !self.link.as_ref().is_some_and(|l| l.authorized) {
            return;
        }
        let mut dst = [0u8; 6];
        dst.copy_from_slice(&eth[0..6]);
        let ethertype = u16::from_be_bytes([eth[12], eth[13]]);
        let encrypt = self.link.as_ref().is_some_and(|l| l.wpa);
        if self.send_data(&dst, ethertype, &eth[14..], encrypt, 0).is_err() {
            self.tx_failed += 1;
        }
    }

    pub fn link_up(&self) -> bool {
        self.running && self.link.as_ref().is_some_and(|l| l.authorized)
    }
}

/// An RSN element's pairwise cipher list contains 00-0F-AC:`suite`.
fn rsn_pairwise_has(rsn: &[u8], suite: u8) -> bool {
    if rsn.len() < 8 {
        return false;
    }
    let n = le16(rsn, 6) as usize;
    (0..n).any(|i| rsn.get(8 + 4 * i..12 + 4 * i) == Some(&[0x00, 0x0F, 0xAC, suite][..]))
}

/// An EAPOL-Key frame (RSN descriptor, version 2) with its MIC.
fn eapol_key(info: u16, replay: u64, nonce: &[u8; 32], key_data: &[u8], kck: &[u8; 16]) -> Vec<u8> {
    let body_len = 95 + key_data.len();
    let mut e = Vec::with_capacity(4 + body_len);
    e.push(1); // 802.1X-2001
    e.push(3); // EAPOL-Key
    e.extend_from_slice(&(body_len as u16).to_be_bytes());
    e.push(2); // RSN key descriptor
    e.extend_from_slice(&info.to_be_bytes());
    e.extend_from_slice(&0u16.to_be_bytes()); // key length
    e.extend_from_slice(&replay.to_be_bytes());
    e.extend_from_slice(nonce);
    e.extend_from_slice(&[0; 16]); // IV
    e.extend_from_slice(&[0; 8]); // RSC
    e.extend_from_slice(&[0; 8]); // reserved
    e.extend_from_slice(&[0; 16]); // MIC
    e.extend_from_slice(&(key_data.len() as u16).to_be_bytes());
    e.extend_from_slice(key_data);
    let m = crate::wpa::mic(kck, &e);
    e[81..97].copy_from_slice(&m);
    e
}

fn mic_ok(e: &[u8], kck: &[u8; 16]) -> bool {
    let body_len = u16::from_be_bytes([e[2], e[3]]) as usize;
    let Some(frame) = e.get(..4 + body_len) else { return false };
    let mut copy = frame.to_vec();
    copy[81..97].fill(0);
    crate::wpa::mic(kck, &copy) == e[81..97]
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

// ---- The network stack's view: a NIC ------------------------------------------------

static ATTACHED: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// The first time a network is joined, the card joins the network stack
/// as a NIC (the logical NIC fails over to it while the wired NIC has no
/// link; the interface takes its MAC and DHCP restarts).
fn attach_to_network(mac: [u8; 6], base: u64) {
    if ATTACHED.swap(true, core::sync::atomic::Ordering::AcqRel) {
        return;
    }
    crate::network::attach_nic(alloc::boxed::Box::new(WifiNic { mac, base, rx: 0, tx: 0, up: true }));
}

/// The card as a `network::Nic`. Frames go through `WIFI`; if the driver is
/// busy (a scan or a join in the terminal), this poll skips it.
pub struct WifiNic {
    mac: [u8; 6],
    base: u64,
    rx: u64,
    tx: u64,
    up: bool,
}

impl crate::network::Nic for WifiNic {
    fn mac(&self) -> smoltcp::wire::EthernetAddress {
        smoltcp::wire::EthernetAddress(self.mac)
    }

    fn link_up(&self) -> bool {
        WIFI.try_lock().map_or(self.up, |w| w.as_ref().is_some_and(|i| i.link_up()))
    }

    fn poll_rx(&mut self, out: &mut Vec<crate::virtio_net::PacketBuf>) {
        let Some(mut guard) = WIFI.try_lock() else { return };
        let Some(iwl) = guard.as_mut() else { return };
        iwl.service();
        self.up = iwl.link_up();
        while let Some(f) = iwl.eth_rx.pop_front() {
            let mut pkt = crate::virtio_net::PacketBuf::new();
            let n = f.len().min(crate::virtio_net::MTU);
            pkt.data[..n].copy_from_slice(&f[..n]);
            pkt.len = n;
            out.push(pkt);
            self.rx += 1;
        }
    }

    fn transmit(&mut self, frame: &[u8]) {
        let mut guard = WIFI.lock();
        if let Some(iwl) = guard.as_mut() {
            iwl.transmit_eth(frame);
            self.tx += 1;
        }
    }

    fn counts(&self) -> (u64, u64) {
        (self.rx, self.tx)
    }

    fn key(&self) -> u64 {
        self.base
    }

    fn kind(&self) -> &'static str {
        "iwlwifi"
    }

    fn detail(&self) -> String {
        let Some(guard) = WIFI.try_lock() else { return String::from("Wi-Fi (busy)") };
        let Some(iwl) = guard.as_ref() else { return String::new() };
        match &iwl.link {
            Some(l) => format!("Wi-Fi '{}' ch {} {} dBm, {} Mb/s, TX ok {} failed {}, RX {} dropped {}",
                l.bss.ssid_text(), l.bss.channel, l.bss.signal, l.rate_500k as f32 / 2.0,
                iwl.tx_ok, iwl.tx_failed, iwl.rx_data, iwl.rx_dropped),
            None => format!("Wi-Fi not connected ({})", iwl.state),
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
        "UP" => match {
            iwl.skip = args.get(1).map(|s| s.to_ascii_uppercase()).unwrap_or_default();
            iwl.up()
        } {
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
        "CONNECT" => {
            let words = quoted_words(&args[1..].join(" "));
            let Some(ssid) = words.first() else {
                out.push(String::from("Usage: WIFI CONNECT <ssid> [password]   (\"quotes\" around an SSID with spaces)"));
                return out;
            };
            let pass = words.get(1);
            match iwl.connect(ssid.as_bytes(), pass.map(|p| p.as_bytes())) {
                Ok(()) => {
                    out.push(format!("  Connected to {}. Getting an address (DHCP): see NET STATUS.", ssid));
                    let mac = iwl.mac;
                    let base = iwl.base;
                    drop(guard);
                    attach_to_network(mac, base);
                }
                Err(e) => out.push(format!("  Connect failed: {}", e)),
            }
        }
        "DISCONNECT" => {
            iwl.disconnect("by request");
            out.push(String::from("  Disconnected."));
        }
        _ => out.push(String::from("Usage: WIFI [STATUS|UP|SCAN|CONNECT <ssid> [password]|DISCONNECT]")),
    }
    out
}
