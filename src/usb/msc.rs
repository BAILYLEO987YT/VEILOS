// =============================================================================
// VEILOS: USB mass storage (bulk-only transport, SCSI)
// =============================================================================
//
// A USB stick or disk: each request is a 31-byte Command Block Wrapper on the
// bulk-OUT endpoint, the data on bulk-IN or bulk-OUT, and a 13-byte Command
// Status Wrapper on bulk-IN. The commands are SCSI: INQUIRY, TEST UNIT
// READY (retried while the medium spins up), READ CAPACITY, READ/WRITE
// (10, or 16 past 2 TiB). A `UsbMsc` is a `block::Transport`, so the disk
// mounts like any other and its VEILOS volume can be `/`.

use alloc::{boxed::Box, format, string::String, sync::Arc};
use spin::Mutex;

use super::xhci::{Controller, MscFound, CC_STALL};
use super::Endpoint;

const SECTOR: usize = 512;
/// Data moved per SCSI command (and the DMA buffer's size).
const CHUNK: usize = 64 * 1024;
const CBW_SIG: u32 = 0x4342_5355; // "USBC"
const CSW_SIG: u32 = 0x5342_5355; // "USBS"

pub struct UsbMsc {
    ctrl: Arc<Mutex<Controller>>,
    slot: u8,
    bulk_in: Endpoint,
    bulk_out: Endpoint,
    tag: u32,
    /// CBW at +0, CSW at +512.
    cmd_phys: u64,
    cmd: *mut u8,
    buf_phys: u64,
    buf: *mut u8,
}

// DMA buffers owned by this device, used under the disk's lock.
unsafe impl Send for UsbMsc {}

static NEXT: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

/// Start a mass-storage device found by xhci::enumerate and mount it.
pub fn attach(ctrl: Arc<Mutex<Controller>>, found: MscFound) {
    let name = format!("usb{}", NEXT.fetch_add(1, core::sync::atomic::Ordering::Relaxed));
    match UsbMsc::new(ctrl, found) {
        Ok((dev, sectors)) => {
            crate::println!("     - USB disk {}: {} MiB", name, sectors / 2048);
            crate::block::mount_disk(crate::block::Disk::new(Box::new(dev), name, sectors));
        }
        Err(e) => crate::serial_println!("[USB] {}: {}", name, e),
    }
}

impl UsbMsc {
    fn new(ctrl: Arc<Mutex<Controller>>, f: MscFound) -> Result<(Self, u64), String> {
        let (cmd_phys, cmd) = crate::memory::DmaRegion::new(1024).ok_or("no DMA memory")?.leak();
        let (buf_phys, buf) = crate::memory::DmaRegion::new(CHUNK).ok_or("no DMA memory")?.leak();
        let mut dev = Self { ctrl, slot: f.slot, bulk_in: f.bulk_in, bulk_out: f.bulk_out, tag: 1, cmd_phys, cmd, buf_phys, buf };

        let inq = dev.scsi_in(&[0x12, 0, 0, 0, 36, 0], 36)?;
        let text = |r: core::ops::Range<usize>| String::from_utf8_lossy(&inq[r.start.min(inq.len())..r.end.min(inq.len())]).trim().into();
        let (vendor, product): (String, String) = (text(8..16), text(16..32));
        if inq.first().is_some_and(|&t| t & 0x1F != 0) {
            return Err(format!("{} {} is not a disk (SCSI type {})", vendor, product, inq[0] & 0x1F));
        }
        // The medium may need a moment (a card reader, a disk spinning up).
        let mut ready = false;
        for _ in 0..20 {
            if dev.scsi(&[0, 0, 0, 0, 0, 0], 0, true, None).is_ok() {
                ready = true;
                break;
            }
            let _ = dev.scsi_in(&[0x03, 0, 0, 0, 18, 0], 18); // REQUEST SENSE clears the condition
            crate::time::delay_ms(100);
        }
        if !ready {
            return Err(format!("{} {}: not ready (no medium?)", vendor, product));
        }
        let cap = dev.scsi_in(&[0x25, 0, 0, 0, 0, 0, 0, 0, 0, 0], 8)?;
        if cap.len() < 8 {
            return Err(String::from("short READ CAPACITY"));
        }
        let mut last = u32::from_be_bytes([cap[0], cap[1], cap[2], cap[3]]) as u64;
        let mut block = u32::from_be_bytes([cap[4], cap[5], cap[6], cap[7]]);
        if last == 0xFFFF_FFFF {
            let mut cb = [0u8; 16];
            cb[0] = 0x9E;
            cb[1] = 0x10;
            cb[13] = 32;
            let c16 = dev.scsi_in(&cb, 32)?;
            if c16.len() >= 12 {
                last = u64::from_be_bytes(c16[0..8].try_into().unwrap());
                block = u32::from_be_bytes(c16[8..12].try_into().unwrap());
            }
        }
        crate::serial_println!("[USB] {} {}: {} sectors of {} bytes", vendor, product, last + 1, block);
        if block as usize != SECTOR {
            return Err(format!("{}-byte sectors aren't supported yet", block));
        }
        Ok((dev, last + 1))
    }

    /// One SCSI command: CBW, data (IN into `buf`, or OUT from `out`), CSW.
    /// Returns the data bytes moved.
    fn scsi(&mut self, cb: &[u8], len: usize, data_in: bool, out: Option<&[u8]>) -> Result<usize, String> {
        let tag = self.tag;
        self.tag = self.tag.wrapping_add(1);
        let mut cbw = [0u8; 31];
        cbw[0..4].copy_from_slice(&CBW_SIG.to_le_bytes());
        cbw[4..8].copy_from_slice(&tag.to_le_bytes());
        cbw[8..12].copy_from_slice(&(len as u32).to_le_bytes());
        cbw[12] = if data_in { 0x80 } else { 0 };
        cbw[14] = cb.len() as u8;
        cbw[15..15 + cb.len()].copy_from_slice(cb);
        unsafe { core::ptr::copy_nonoverlapping(cbw.as_ptr(), self.cmd, 31) };
        if let Some(out) = out {
            unsafe { core::ptr::copy_nonoverlapping(out.as_ptr(), self.buf, len) };
        }

        let ctrl = self.ctrl.clone();
        let mut c = ctrl.lock();
        let (slot, bin, bout) = (self.slot, self.bulk_in, self.bulk_out);
        c.bulk(slot, bout.dci(), self.cmd_phys, 31, 1_000_000).map_err(|cc| format!("CBW failed: code {}", cc))?;
        let mut moved = 0;
        if len > 0 {
            let ep = if data_in { bin } else { bout };
            match c.bulk(slot, ep.dci(), self.buf_phys, len, 5_000_000) {
                Ok(n) => moved = n,
                Err(CC_STALL) => c.recover_endpoint(slot, &ep), // the CSW still follows
                Err(cc) => return Err(format!("data stage failed: code {}", cc)),
            }
        }
        let csw_phys = self.cmd_phys + 512;
        let mut status = c.bulk(slot, bin.dci(), csw_phys, 13, 1_000_000);
        if status == Err(CC_STALL) {
            c.recover_endpoint(slot, &bin);
            status = c.bulk(slot, bin.dci(), csw_phys, 13, 1_000_000);
        }
        drop(c);
        status.map_err(|cc| format!("CSW failed: code {}", cc))?;
        let csw = unsafe { core::slice::from_raw_parts(self.cmd.add(512), 13) };
        let sig = u32::from_le_bytes(csw[0..4].try_into().unwrap());
        let ctag = u32::from_le_bytes(csw[4..8].try_into().unwrap());
        if sig != CSW_SIG || ctag != tag {
            return Err(String::from("bad CSW"));
        }
        match csw[12] {
            0 => Ok(moved),
            1 => Err(format!("command {:#04x} failed", cb[0])),
            _ => Err(format!("phase error on command {:#04x}", cb[0])),
        }
    }

    fn scsi_in(&mut self, cb: &[u8], len: usize) -> Result<alloc::vec::Vec<u8>, String> {
        let n = self.scsi(cb, len, true, None)?;
        let mut v = alloc::vec![0u8; n];
        unsafe { core::ptr::copy_nonoverlapping(self.buf, v.as_mut_ptr(), n) };
        Ok(v)
    }

    /// READ/WRITE (10), or (16) past the 32-bit LBA range.
    fn rw_cb(write: bool, lba: u64, count: u32) -> ([u8; 16], usize) {
        let mut cb = [0u8; 16];
        if lba + count as u64 <= 0xFFFF_FFFF {
            cb[0] = if write { 0x2A } else { 0x28 };
            cb[2..6].copy_from_slice(&(lba as u32).to_be_bytes());
            cb[7..9].copy_from_slice(&(count as u16).to_be_bytes());
            (cb, 10)
        } else {
            cb[0] = if write { 0x8A } else { 0x88 };
            cb[2..10].copy_from_slice(&lba.to_be_bytes());
            cb[10..14].copy_from_slice(&count.to_be_bytes());
            (cb, 16)
        }
    }
}

impl crate::block::Transport for UsbMsc {
    fn read(&mut self, lba: u64, buf: &mut [u8]) -> bool {
        for (i, chunk) in buf.chunks_mut(CHUNK).enumerate() {
            let at = lba + (i * CHUNK / SECTOR) as u64;
            let (cb, n) = Self::rw_cb(false, at, (chunk.len() / SECTOR) as u32);
            match self.scsi(&cb[..n], chunk.len(), true, None) {
                Ok(m) if m == chunk.len() => unsafe { core::ptr::copy_nonoverlapping(self.buf, chunk.as_mut_ptr(), m) },
                Ok(m) => {
                    crate::serial_println!("[USB] read at {}: {} of {} bytes", at, m, chunk.len());
                    return false;
                }
                Err(e) => {
                    crate::serial_println!("[USB] read at {}: {}", at, e);
                    return false;
                }
            }
        }
        true
    }

    fn write(&mut self, lba: u64, buf: &[u8]) -> bool {
        for (i, chunk) in buf.chunks(CHUNK).enumerate() {
            let at = lba + (i * CHUNK / SECTOR) as u64;
            let (cb, n) = Self::rw_cb(true, at, (chunk.len() / SECTOR) as u32);
            if let Err(e) = self.scsi(&cb[..n], chunk.len(), false, Some(chunk)) {
                crate::serial_println!("[USB] write at {}: {}", at, e);
                return false;
            }
        }
        true
    }
}
