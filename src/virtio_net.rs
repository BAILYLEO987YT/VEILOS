// VirtIO-net legacy PCI driver for VEILOS
//
// Implements proper VirtIO 1.0 legacy (transitional) virtqueue layout:
//   - Descriptor table at offset 0
//   - Available ring immediately after descriptors
//   - Used ring at next 4096-aligned boundary after available ring
//
// This driver communicates with QEMU's virtio-net-pci device via
// legacy I/O port registers.

use smoltcp::wire::EthernetAddress;
use crate::memory::get_mapper;
use x86_64::{VirtAddr, structures::paging::Translate};
use x86_64::instructions::port::Port;
use alloc::vec::Vec;

fn virt_to_phys(vaddr: u64) -> u64 {
    let mapper = unsafe { get_mapper() };
    mapper.translate_addr(VirtAddr::new(vaddr))
        .expect("virt_to_phys translation failed")
        .as_u64()
}

pub const MTU: usize = 1514;

pub struct PacketBuf {
    pub data: [u8; MTU],
    pub len: usize,
}

impl PacketBuf {
    pub fn new() -> Self {
        Self { data: [0; MTU], len: 0 }
    }
}

// ---- VirtIO Queue Structures ----
//
// Legacy VirtIO queue layout (VirtIO spec §2.6.2):
//   QueuePFN points to a page-aligned region containing:
//     [Descriptor Table]  16 * queue_size bytes
//     [Available Ring]    6 + 2 * queue_size bytes
//     [padding to 4096-aligned boundary]
//     [Used Ring]         6 + 8 * queue_size bytes
//
// QEMU calculates these offsets from the single QueuePFN, so our
// in-memory layout MUST match exactly.

const VIRTQ_SIZE: usize = 256;  // Must match what QEMU reports

// Descriptor: 16 bytes each
#[repr(C)]
#[derive(Clone, Copy)]
pub struct VirtqDesc {
    pub addr: u64,
    pub len: u32,
    pub flags: u16,
    pub next: u16,
}

// Available ring: flags(2) + idx(2) + ring[N](2*N) + used_event(2)
// Total = 6 + 2*N bytes
#[repr(C)]
pub struct VirtqAvail {
    pub flags: u16,
    pub idx: u16,
    pub ring: [u16; VIRTQ_SIZE],
    pub used_event: u16,
}

// Used ring element: 8 bytes
#[repr(C)]
#[derive(Clone, Copy)]
pub struct VirtqUsedElem {
    pub id: u32,
    pub len: u32,
}

// Used ring: flags(2) + idx(2) + ring[N](8*N) + avail_event(2)
// Total = 6 + 8*N bytes
#[repr(C)]
pub struct VirtqUsed {
    pub flags: u16,
    pub idx: u16,
    pub ring: [VirtqUsedElem; VIRTQ_SIZE],
    pub avail_event: u16,
}

// Calculate layout sizes per VirtIO spec:
//   desc_size  = 16 * N
//   avail_size = 6 + 2 * N
//   avail_end  = desc_size + avail_size
//   used_offset = align_up(avail_end, 4096)
//   used_size  = 6 + 8 * N
//
// For N=256:
//   desc_size  = 4096
//   avail_size = 518
//   avail_end  = 4614
//   used_offset = 8192  (next 4096 boundary)
//   used_size  = 2054
//   total      = 8192 + 2054 = 10246 -> round up to 12288 (3 pages)

const DESC_SIZE: usize = 16 * VIRTQ_SIZE;          // 4096
const AVAIL_SIZE: usize = 6 + 2 * VIRTQ_SIZE;      // 518
const AVAIL_END: usize = DESC_SIZE + AVAIL_SIZE;    // 4614
const USED_OFFSET: usize = (AVAIL_END + 4095) & !4095;  // 8192
const USED_SIZE: usize = 6 + 8 * VIRTQ_SIZE;       // 2054
const QUEUE_TOTAL: usize = USED_OFFSET + USED_SIZE; // 10246

// Pad between avail and used to align used ring to 4096 boundary
const AVAIL_PAD: usize = USED_OFFSET - AVAIL_END;   // 3578

// We represent the entire queue region as a single aligned struct.
// The layout MUST be: desc | avail | pad | used — with used starting
// at exactly USED_OFFSET from the base.
#[repr(C, align(4096))]
pub struct VirtQueueMem {
    pub desc: [VirtqDesc; VIRTQ_SIZE],
    pub avail: VirtqAvail,
    _pad: [u8; AVAIL_PAD],
    pub used: VirtqUsed,
}

impl VirtQueueMem {
    pub const fn new() -> Self {
        Self {
            desc: [VirtqDesc { addr: 0, len: 0, flags: 0, next: 0 }; VIRTQ_SIZE],
            avail: VirtqAvail { flags: 0, idx: 0, ring: [0; VIRTQ_SIZE], used_event: 0 },
            _pad: [0; AVAIL_PAD],
            used: VirtqUsed { flags: 0, idx: 0, ring: [VirtqUsedElem { id: 0, len: 0 }; VIRTQ_SIZE], avail_event: 0 },
        }
    }
}

static mut RX_QUEUE: VirtQueueMem = VirtQueueMem::new();
static mut TX_QUEUE: VirtQueueMem = VirtQueueMem::new();

// DMA buffers: each slot is 2048 bytes (virtio-net header + MTU frame)
#[repr(C, align(4096))]
pub struct VirtBuffers {
    pub rx: [[u8; 2048]; VIRTQ_SIZE],
    pub tx: [[u8; 2048]; VIRTQ_SIZE],
}

impl VirtBuffers {
    pub const fn new() -> Self {
        Self {
            rx: [[0; 2048]; VIRTQ_SIZE],
            tx: [[0; 2048]; VIRTQ_SIZE],
        }
    }
}

static mut BUFFERS: VirtBuffers = VirtBuffers::new();

const VIRTQ_DESC_F_WRITE: u16 = 2;
const VIRTIO_NET_HDR_SIZE: usize = 10; // Legacy virtio-net header

pub struct VirtioNet {
    pub io_base: u16,
    pub mac: EthernetAddress,
    pub rx_queue: Vec<PacketBuf>,
    pub rx_count: u64,
    pub tx_count: u64,
    rx_last_used: u16,
    tx_avail_idx: u16,
}

impl VirtioNet {
    pub fn new(io_base: u16) -> Self {
        // 1. Read MAC address (available before reset on legacy devices)
        let mac_bytes: [u8; 6] = core::array::from_fn(|i| unsafe {
            let mut p = Port::<u8>::new(io_base + 0x14 + i as u16);
            p.read()
        });
        let mac = EthernetAddress(mac_bytes);
        crate::serial_println!("[NET] VirtIO-net MAC: {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
            mac_bytes[0], mac_bytes[1], mac_bytes[2], mac_bytes[3], mac_bytes[4], mac_bytes[5]);

        unsafe {
            let mut status_port: Port<u8> = Port::new(io_base + 0x12);

            // 2. Reset device
            status_port.write(0);
            // Wait ≥1 ms for the device to complete its reset sequence.
            // Use the PIT tick counter so the delay is wall-clock accurate
            // regardless of CPU frequency, instead of a fixed iteration count
            // that varies wildly across machines.
            {
                use core::sync::atomic::Ordering;
                let start = crate::interrupts::TICKS.load(Ordering::Relaxed);
                while crate::interrupts::TICKS.load(Ordering::Relaxed)
                    .wrapping_sub(start) < 2
                {
                    core::hint::spin_loop();
                }
            }

            // 3. Set ACKNOWLEDGE status bit
            status_port.write(1);

            // 4. Set DRIVER status bit
            status_port.write(1 | 2);

            // 5. Read host features
            let mut host_features_port: Port<u32> = Port::new(io_base + 0x00);
            let host_features = host_features_port.read();
            crate::serial_println!("[NET] Host features: {:#010x}", host_features);

            // 6. Write guest features: only VIRTIO_NET_F_MAC (bit 5)
            // Do NOT set VIRTIO_NET_F_STATUS or any other feature we don't support
            let mut guest_features_port: Port<u32> = Port::new(io_base + 0x04);
            let negotiated = host_features & (1 << 5); // Only MAC
            guest_features_port.write(negotiated);
            crate::serial_println!("[NET] Negotiated features: {:#010x}", negotiated);
        }

        let mut net = Self {
            io_base,
            mac,
            rx_queue: Vec::new(),
            rx_count: 0,
            tx_count: 0,
            rx_last_used: 0,
            tx_avail_idx: 0,
        };

        // 7. Setup virtqueues (before setting DRIVER_OK)
        net.init_queues();

        // 8. Set DRIVER_OK — device is live
        unsafe {
            let mut status_port: Port<u8> = Port::new(io_base + 0x12);
            status_port.write(1 | 2 | 4);
        }

        crate::serial_println!("[NET] VirtIO-net driver initialized successfully");
        net
    }

    fn init_queues(&mut self) {
        unsafe {
            let mut q_select: Port<u16> = Port::new(self.io_base + 0x0E);
            let mut q_size_port: Port<u16> = Port::new(self.io_base + 0x0C);
            let mut q_addr: Port<u32> = Port::new(self.io_base + 0x08);

            // ---- RX Queue (index 0) ----
            q_select.write(0);
            let rx_qsize = q_size_port.read();
            crate::serial_println!("[NET] RX queue size from device: {} (our VIRTQ_SIZE={})", rx_qsize, VIRTQ_SIZE);

            if rx_qsize == 0 {
                crate::serial_println!("[NET] ERROR: RX queue size is 0! Device may not be ready.");
                return;
            }

            // Verify layout assumptions
            let base_ptr = &raw const RX_QUEUE as usize;
            let avail_ptr = &raw const RX_QUEUE.avail as usize;
            let used_ptr = &raw const RX_QUEUE.used as usize;
            crate::serial_println!("[NET] RX queue layout: base=0x{:x}, avail=+0x{:x}, used=+0x{:x}",
                base_ptr, avail_ptr - base_ptr, used_ptr - base_ptr);

            // Write physical page frame number of the queue
            let rx_phys = virt_to_phys(&raw const RX_QUEUE as u64);
            crate::serial_println!("[NET] RX queue phys addr: 0x{:x}, PFN: 0x{:x}", rx_phys, rx_phys / 4096);
            q_addr.write((rx_phys / 4096) as u32);

            // Populate RX descriptors: each points to a DMA buffer for receiving
            for i in 0..VIRTQ_SIZE {
                let buf_phys = virt_to_phys(&raw const BUFFERS.rx[i] as u64);
                RX_QUEUE.desc[i].addr = buf_phys;
                RX_QUEUE.desc[i].len = 2048;
                RX_QUEUE.desc[i].flags = VIRTQ_DESC_F_WRITE; // Device writes to this buffer
                RX_QUEUE.desc[i].next = 0;

                // Add to available ring
                RX_QUEUE.avail.ring[i] = i as u16;
            }
            // Tell device all descriptors are available
            core::ptr::write_volatile(&raw mut RX_QUEUE.avail.idx, VIRTQ_SIZE as u16);

            // ---- TX Queue (index 1) ----
            q_select.write(1);
            let tx_qsize = q_size_port.read();
            crate::serial_println!("[NET] TX queue size from device: {}", tx_qsize);

            let tx_phys = virt_to_phys(&raw const TX_QUEUE as u64);
            crate::serial_println!("[NET] TX queue phys addr: 0x{:x}, PFN: 0x{:x}", tx_phys, tx_phys / 4096);
            q_addr.write((tx_phys / 4096) as u32);
        }

        // Kick RX queue so device knows buffers are available
        unsafe {
            let mut notify: Port<u16> = Port::new(self.io_base + 0x10);
            notify.write(0);
        }

        crate::serial_println!("[NET] Virtqueues initialized (RX + TX)");
    }

    /// Poll the RX virtqueue for completed (received) frames.
    pub fn poll_rx(&mut self) {
        unsafe {
            // Check used ring for new entries
            let used_idx = core::ptr::read_volatile(&raw const RX_QUEUE.used.idx);

            while self.rx_last_used != used_idx {
                let ring_idx = self.rx_last_used as usize % VIRTQ_SIZE;
                let elem = core::ptr::read_volatile(&raw const RX_QUEUE.used.ring[ring_idx]);
                let desc_idx = elem.id as usize;
                let total_len = elem.len as usize;

                // The buffer contains: [virtio-net header (10 bytes)][ethernet frame]
                if total_len > VIRTIO_NET_HDR_SIZE + 14 && desc_idx < VIRTQ_SIZE {
                    let frame_len = total_len - VIRTIO_NET_HDR_SIZE;
                    let frame_data = &BUFFERS.rx[desc_idx][VIRTIO_NET_HDR_SIZE..VIRTIO_NET_HDR_SIZE + frame_len];

                    let mut pkt = PacketBuf::new();
                    let copy_len = frame_len.min(MTU);
                    pkt.data[..copy_len].copy_from_slice(&frame_data[..copy_len]);
                    pkt.len = copy_len;
                    self.rx_queue.push(pkt);
                    self.rx_count += 1;

                    // DIAGNOSTIC: proves a frame actually landed in the ring and
                    // was handed off to the software FIFO that smoltcp drains.
                    crate::serial_println!(
                        "[NET] RX #{}: {} bytes, desc={}, used.idx={}, rx_queue depth={}",
                        self.rx_count, copy_len, desc_idx, used_idx, self.rx_queue.len()
                    );
                } else {
                    // DIAGNOSTIC: the device completed a descriptor but the frame
                    // failed our sanity check — worth knowing if this is happening.
                    crate::serial_println!(
                        "[NET] RX dropped: total_len={}, desc={} (sanity check failed)",
                        total_len, desc_idx
                    );
                }

                // Re-add descriptor to available ring for reuse
                let avail_idx = core::ptr::read_volatile(&raw const RX_QUEUE.avail.idx);
                core::ptr::write_volatile(
                    &raw mut RX_QUEUE.avail.ring[avail_idx as usize % VIRTQ_SIZE],
                    desc_idx as u16,
                );
                core::ptr::write_volatile(
                    &raw mut RX_QUEUE.avail.idx,
                    avail_idx.wrapping_add(1),
                );

                self.rx_last_used = self.rx_last_used.wrapping_add(1);
            }

            let mut notify: Port<u16> = Port::new(self.io_base + 0x10);
            notify.write(0);
        }
    }

    /// Transmit an Ethernet frame via the TX virtqueue.
    pub fn hw_transmit(&mut self, frame: &[u8]) {
        if frame.len() > MTU || frame.len() == 0 { return; }

        unsafe {
            let desc_idx = self.tx_avail_idx as usize % VIRTQ_SIZE;

            // Write virtio-net header (10 bytes of zeros) + frame into TX buffer
            BUFFERS.tx[desc_idx][..VIRTIO_NET_HDR_SIZE].fill(0);
            BUFFERS.tx[desc_idx][VIRTIO_NET_HDR_SIZE..VIRTIO_NET_HDR_SIZE + frame.len()]
                .copy_from_slice(frame);

            // Setup descriptor
            let buf_phys = virt_to_phys(&raw const BUFFERS.tx[desc_idx] as u64);
            TX_QUEUE.desc[desc_idx].addr = buf_phys;
            TX_QUEUE.desc[desc_idx].len = (VIRTIO_NET_HDR_SIZE + frame.len()) as u32;
            TX_QUEUE.desc[desc_idx].flags = 0; // Device reads from this buffer
            TX_QUEUE.desc[desc_idx].next = 0;

            // Add to available ring
            let avail_idx = core::ptr::read_volatile(&raw const TX_QUEUE.avail.idx);
            core::ptr::write_volatile(
                &raw mut TX_QUEUE.avail.ring[avail_idx as usize % VIRTQ_SIZE],
                desc_idx as u16,
            );
            core::ptr::write_volatile(
                &raw mut TX_QUEUE.avail.idx,
                avail_idx.wrapping_add(1),
            );

            self.tx_avail_idx = self.tx_avail_idx.wrapping_add(1);
            self.tx_count += 1;

            // DIAGNOSTIC: this proves the frame actually reached the virtqueue,
            // not just that the app layer queued it into the smoltcp socket.
            // used.idx is also logged so we can see whether the device is still
            // acknowledging TX completions (host consuming the ring) or whether
            // avail.idx and used.idx have started diverging (ring backing up).
            let tx_used_idx = core::ptr::read_volatile(&raw const TX_QUEUE.used.idx);
            crate::serial_println!(
                "[NET] TX #{}: {} bytes, desc={}, avail.idx={}, used.idx={}",
                self.tx_count, frame.len(), desc_idx, avail_idx.wrapping_add(1), tx_used_idx
            );

            // Kick TX queue
            let mut notify: Port<u16> = Port::new(self.io_base + 0x10);
            notify.write(1);
        }
    }
}
