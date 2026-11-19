// =============================================================================
// VEILOS: USB
// =============================================================================
//
// xHCI host controllers (`xhci.rs`), which on a PC since about 2015 carry
// every USB port, with three kinds of device on them:
//
//   * keyboards and mice in HID boot protocol (`hid.rs`): their reports
//     become the set-1 scancodes and mouse movement the PS/2 drivers produce,
//     so everything above sees one keyboard and one mouse;
//   * mass storage, bulk-only transport with SCSI commands (`msc.rs`): a
//     `block::Transport`, so a USB stick's VEILOS volume can be `/`.
//
// Everything is polled. Commands and bulk transfers wait for their own
// completion; the BSP's timer tick (`poll`) collects keyboard and mouse
// reports. Not yet: hubs (devices behind one are skipped, with a log line),
// hot-plug after boot, isochronous transfers, USB 2 EHCI controllers.

pub mod hid;
pub mod msc;
pub mod xhci;

/// xHCI controllers (class 0C, subclass 03, interface 30). Bound by
/// `hal::bind` with the other serial-bus devices, before the root volume is
/// chosen, so a USB stick can hold it.
pub const XHCI_DRIVER: crate::hal::Driver = crate::hal::Driver {
    name: "xhci",
    class: crate::hal::DeviceClass::SerialBus,
    matches: |d| d.class == 0x0C && d.subclass == 0x03 && d.prog_if == 0x30,
    probe: xhci::probe,
    remove: None,
};

/// From the BSP's timer interrupt: collect keyboard and mouse reports.
/// Skips a controller that a transfer is using (it collects them itself).
pub fn poll() {
    xhci::poll_all();
}

// -----| standard requests and descriptors |-----

/// A control request's setup packet.
#[derive(Clone, Copy, Debug)]
pub struct Setup {
    pub request_type: u8,
    pub request: u8,
    pub value: u16,
    pub index: u16,
    pub length: u16,
}

impl Setup {
    pub fn to_u64(self) -> u64 {
        self.request_type as u64
            | (self.request as u64) << 8
            | (self.value as u64) << 16
            | (self.index as u64) << 32
            | (self.length as u64) << 48
    }

    pub fn get_descriptor(kind: u8, index: u8, length: u16) -> Self {
        Self { request_type: 0x80, request: 6, value: (kind as u16) << 8 | index as u16, index: 0, length }
    }

    pub fn set_configuration(value: u8) -> Self {
        Self { request_type: 0x00, request: 9, value: value as u16, index: 0, length: 0 }
    }
}

pub const DESC_DEVICE: u8 = 1;
pub const DESC_CONFIG: u8 = 2;
pub const DESC_INTERFACE: u8 = 4;
pub const DESC_ENDPOINT: u8 = 5;

/// An endpoint from a configuration descriptor.
#[derive(Clone, Copy, Debug)]
pub struct Endpoint {
    /// bEndpointAddress: number in bits 3:0, IN when bit 7 is set.
    pub address: u8,
    /// bmAttributes bits 1:0: 0 control, 1 isochronous, 2 bulk, 3 interrupt.
    pub kind: u8,
    pub max_packet: u16,
    pub interval: u8,
}

impl Endpoint {
    pub fn is_in(&self) -> bool {
        self.address & 0x80 != 0
    }
    /// xHCI's Device Context Index: endpoint number * 2, plus 1 for IN.
    pub fn dci(&self) -> u8 {
        (self.address & 0x0F) * 2 + self.is_in() as u8
    }
}

/// An interface and its endpoints.
#[derive(Clone, Debug)]
pub struct Interface {
    pub number: u8,
    pub class: u8,
    pub subclass: u8,
    pub protocol: u8,
    pub endpoints: alloc::vec::Vec<Endpoint>,
}

/// The interfaces of a configuration descriptor (all of wTotalLength).
pub fn parse_config(desc: &[u8]) -> alloc::vec::Vec<Interface> {
    let mut out: alloc::vec::Vec<Interface> = alloc::vec::Vec::new();
    let mut at = 0;
    while at + 2 <= desc.len() {
        let len = desc[at] as usize;
        if len < 2 || at + len > desc.len() {
            break;
        }
        let d = &desc[at..at + len];
        match d[1] {
            DESC_INTERFACE if len >= 9 => out.push(Interface {
                number: d[2],
                class: d[5],
                subclass: d[6],
                protocol: d[7],
                endpoints: alloc::vec::Vec::new(),
            }),
            DESC_ENDPOINT if len >= 7 => {
                if let Some(i) = out.last_mut() {
                    i.endpoints.push(Endpoint {
                        address: d[2],
                        kind: d[3] & 3,
                        max_packet: u16::from_le_bytes([d[4], d[5]]) & 0x7FF,
                        interval: d[6],
                    });
                }
            }
            _ => {}
        }
        at += len;
    }
    out
}
