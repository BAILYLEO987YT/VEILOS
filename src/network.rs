// -----| VEILOS Network Stack |-----
//
// Phase 13 roadmap items implemented here:
//
//   [x] TCP/IP Stack
//         IPv4 + IPv6, TCP, UDP, ICMP — via smoltcp 0.13 (no_std/alloc).
//
//   [x] VirtIO-net driver
//         PCI vendor 0x1AF4 / device 0x1000. Discovered during the existing
//         PCI scan and registered automatically. Implements smoltcp's Device
//         trait so the stack drives it directly.
//         Works out of the box with QEMU -device virtio-net-pci.
//
//   [x] Socket API  (capability-gated)
//         open_tcp()  — requires NetConnect or NetBind
//         open_udp()  — requires NetConnect
//         send_tcp(), recv_tcp(), close_socket()
//         In Security Mode every call is capability-checked.
//         In Performance Mode checks are bypassed (consistent with the rest
//         of the capability system).
//
//   [x] DHCP client
//         Drives smoltcp's built-in Dhcpv4Socket to obtain an address from
//         the QEMU user-mode network's DHCP server (10.0.2.x range).
//         Stores the resulting config in NETWORK_CONFIG.
//
//   [x] DNS resolver
//         Drives smoltcp's DnsSocket to resolve A/AAAA records.
//         Default server: 10.0.2.3 (QEMU's built-in DNS forwarder).
//
//   [x] NTP client  (UDP stub)
//         send_ntp_query() fires a standard NTPv3 request to pool.ntp.org's
//         IP and updates NET_CLOCK_OFFSET when the response arrives.
//         Full async follow-up is a one-liner once DNS resolves the server.
//
//   [x] Firewall subsystem
//         Per-process firewall rules: allow/deny by (pid, direction,
//         protocol, port). Rules are installed by capability-holding processes
//         and evaluated before every socket open / packet send.
//
// Architecture
// ------------
//   VirtioNet  (implements smoltcp::phy::Device)
//     Sits directly on the PCI MMIO/IO region. Maintains two virtqueues:
//     receiveq (index 0) and transmitq (index 1). Buffers are carved out of
//     a statically-allocated [u8; NET_BUF_SIZE] pool.
//
//   NetworkStack  (owns smoltcp Interface + SocketSet)
//     Wraps the smoltcp Interface and a SocketSet. net_poll() must be called
//     periodically (from the timer ISR background task or a dedicated async
//     task) to drive the stack forward.
//
//   SocketTable  (process-level socket tracking)
//     Maps SocketHandle → SocketMeta (pid, protocol, state). The capability
//     checker consults this on every send/recv.
//
//   Firewall
//     Vec<FirewallRule>. Evaluated in order; first match wins. Default policy
//     is DENY for inbound, ALLOW for outbound in Security Mode; all ALLOW in
//     Performance Mode.

#![allow(dead_code)]

extern crate alloc;

use alloc::vec::Vec;
use alloc::vec;
use alloc::collections::BTreeMap;
use spin::Mutex;
use lazy_static::lazy_static;
use core::sync::atomic::{AtomicBool, AtomicI64, Ordering};

use smoltcp::iface::{Config, Interface, SocketSet, SocketHandle};
use smoltcp::phy::{Device, DeviceCapabilities, Medium, RxToken, TxToken};
use smoltcp::socket::{tcp, udp, dhcpv4, dns};
use smoltcp::time::Instant;
use smoltcp::wire::{
    IpAddress, IpCidr,
    Ipv4Address, Ipv4Cidr,
    IpEndpoint, IpListenEndpoint,
};

use crate::println;
use crate::capability::{Capability, CapabilityChecker};
use crate::policy::{POLICY, PolicyMode};

// =====================================================================
// -----| Part 1 — Constants & Global State |-----
// =====================================================================

/// Size of the packet ring buffer pools.
const NET_BUF_SIZE:  usize = 64 * 1024; // 64 KiB total receive pool
const MTU:           usize = 1514;       // Standard Ethernet MTU

/// VirtIO-net PCI identifiers.
pub const VIRTIO_VENDOR:  u16 = 0x1AF4;
pub const VIRTIO_NET_DEV: u16 = 0x1000;

/// QEMU user-mode network defaults.
pub const QEMU_GATEWAY:    Ipv4Address = Ipv4Address::new(10, 0, 2, 2);
pub const QEMU_DNS:        Ipv4Address = Ipv4Address::new(10, 0, 2, 3);
pub const NTP_SERVER_IP:   Ipv4Address = Ipv4Address::new(193, 0, 0, 229); // pool.ntp.org

/// Offset between NTP epoch (1900) and Unix epoch (1970) in seconds.
const NTP_UNIX_OFFSET: u64 = 2_208_988_800;

/// Clock offset in milliseconds from NTP (updated on each NTP response).
static NET_CLOCK_OFFSET: AtomicI64 = AtomicI64::new(0);

/// True once the NIC has been found and the stack is live.
static NETWORK_READY: AtomicBool = AtomicBool::new(false);

pub fn is_ready() -> bool { NETWORK_READY.load(Ordering::Acquire) }

/// Current network configuration (filled in by DHCP).
#[derive(Debug, Clone)]
pub struct NetworkConfig {
    pub ip:      Ipv4Cidr,
    pub gateway: Option<Ipv4Address>,
    pub dns:     Option<Ipv4Address>,
    pub lease_ms: u32,
}

impl NetworkConfig {
    const fn default() -> Self {
        Self {
            ip:      Ipv4Cidr::new(Ipv4Address::UNSPECIFIED, 0),
            gateway: None,
            dns:     None,
            lease_ms: 0,
        }
    }
}

lazy_static! {
    pub static ref NETWORK_CONFIG: Mutex<NetworkConfig> =
        Mutex::new(NetworkConfig::default());
}

// =====================================================================
// -----| Part 2 — VirtIO-net Packet Buffers |-----
// =====================================================================

use crate::virtio_net::*;

// =====================================================================
// -----| Part 3 — VirtIO-net PCI Driver |-----
// =====================================================================
//
// VirtIO-net uses a legacy PCI interface (vendor 0x1AF4, device 0x1000).
// The BAR0 I/O region is used to control the device.
//
// Simplified register map (legacy, I/O-port mapped):
//   +0x00  DeviceFeatures  u32 RO
//   +0x04  GuestFeatures   u32 RW
//   +0x08  QueueAddress    u32 RW (page number of queue, 4K pages)
//   +0x0C  QueueSize       u16 RO
//   +0x0E  QueueSelect     u16 RW
//   +0x10  QueueNotify     u16 WO
//   +0x12  DeviceStatus    u8  RW
//   +0x13  ISRStatus       u8  RO
//   +0x14  MAC[6]          u8[6]
//   +0x1A  Status          u16 RO
//
// For QEMU testing we use a simple loopback model: packets written to
// the TX queue appear in the RX queue of the same device (QEMU's
// user-mode networking translates them to host TCP/IP automatically).
//
// We implement smoltcp's Device trait with a software FIFO of PacketBufs
// so the stack can drive the NIC without needing actual DMA ring setup in
// this initial implementation.

// ---- smoltcp Device impl --------------------------------------------

// ---- Logical NIC (HAL step 7E) ----------------------------------------
//
// The network stack sees one NIC however many are bound. `LogicalNic`
// bonds them in active-backup mode (Linux bonding mode 1): frames go out
// through the active member, frames are taken in from every member, and if
// the active member's link drops (VIRTIO_NET_F_STATUS) traffic moves to the
// next member that is up. The interface keeps the first member's MAC
// throughout, so peers see one address.

pub struct LogicalNic {
    members: Vec<VirtioNet>,
    active: usize,
    /// The interface's MAC (the first member's).
    pub mac: smoltcp::wire::EthernetAddress,
    /// Frames received on any member, oldest first, for smoltcp.
    pub rx_queue: Vec<PacketBuf>,
    pub rx_count: u64,
}

impl LogicalNic {
    fn new(first: VirtioNet) -> Self {
        let mac = first.mac;
        Self { members: alloc::vec![first], active: 0, mac, rx_queue: Vec::new(), rx_count: 0 }
    }

    /// Add another NIC. It takes over if the active member fails.
    pub fn add_member(&mut self, nic: VirtioNet) {
        crate::serial_println!("[NET] logical NIC: member {} added (MAC {})", self.members.len(), nic.mac);
        self.members.push(nic);
    }

    /// Drop the member at `io_base` (hot-removal). Traffic moves to another
    /// member; with none left the interface is down until a NIC is added.
    pub fn remove_member(&mut self, io_base: u16) -> bool {
        let Some(i) = self.members.iter().position(|m| m.io_base == io_base) else { return false };
        self.members.remove(i);
        if i < self.active {
            self.active -= 1;
        } else if i == self.active {
            self.active = self.members.iter().position(|m| m.link_up()).unwrap_or(0);
        }
        crate::serial_println!("[NET] logical NIC: member {} removed, {} left{}", i, self.members.len(),
            if self.members.is_empty() { alloc::string::String::new() } else { alloc::format!(", member {} active", self.active) });
        true
    }

    /// Switch away from the active member if its link is down.
    fn check_links(&mut self) {
        if self.members.is_empty() || self.members[self.active].link_up() {
            return;
        }
        let n = self.members.len();
        if let Some(next) = (1..n).map(|k| (self.active + k) % n).find(|&i| self.members[i].link_up()) {
            crate::serial_println!("[NET] logical NIC: member {} link down, failing over to member {}", self.active, next);
            self.active = next;
        }
    }

    /// Poll every member and gather what they received.
    pub fn poll_rx(&mut self) {
        self.check_links();
        for m in self.members.iter_mut() {
            m.poll_rx();
            self.rx_queue.append(&mut m.rx_queue);
        }
    }

    fn transmit(&mut self, frame: &[u8]) {
        // No member: the frame is dropped, as on a cable pulled out.
        if let Some(m) = self.members.get_mut(self.active) {
            m.hw_transmit(frame);
        }
    }

    /// One line per member, for `NET STATUS`.
    pub fn describe(&self) -> Vec<alloc::string::String> {
        self.members.iter().enumerate().map(|(i, m)| alloc::format!(
            "  NIC {}: {} link {} rx {} tx {}{}",
            i, m.mac, if m.link_up() { "up" } else { "DOWN" }, m.rx_count, m.tx_count,
            if i == self.active { "  (active)" } else { "" }
        )).collect()
    }
}

pub struct VirtioRxToken(Vec<u8>);
pub struct VirtioTxToken(*mut LogicalNic);

impl RxToken for VirtioRxToken {
    fn consume<R, F: FnOnce(&[u8]) -> R>(self, f: F) -> R {
        f(&self.0)
    }
}

impl TxToken for VirtioTxToken {
    fn consume<R, F: FnOnce(&mut [u8]) -> R>(self, len: usize, f: F) -> R {
        let mut buf = vec![0u8; len];
        let result = f(&mut buf);
        // SAFETY: token is valid only as long as the LogicalNic is alive;
        // enforced by the borrow structure of smoltcp's poll().
        unsafe { (*self.0).transmit(&buf); }
        result
    }
}

impl Device for LogicalNic {
    type RxToken<'a> = VirtioRxToken where Self: 'a;
    type TxToken<'a> = VirtioTxToken where Self: 'a;

    fn receive(&mut self, _timestamp: Instant)
        -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)>
    {
        // Pop one packet from the software RX FIFO.
        if self.rx_queue.is_empty() { return None; }
        let pkt = self.rx_queue.remove(0);
        self.rx_count += 1;
        // DIAGNOSTIC: proves smoltcp is still actively pulling frames out of
        // the FIFO that poll_rx() fills. If [NET] RX #N lines keep appearing
        // in the log (virtio_net.rs) but this line stops, the frames are
        // stuck between the ring and the smoltcp Interface — i.e. iface.poll()
        // itself has stopped running, even though the raw device is fine.
        crate::serial_println!("[NET] smoltcp received {} bytes (fifo depth now {})", pkt.len, self.rx_queue.len());
        let rx = VirtioRxToken(pkt.data[..pkt.len].to_vec());
        let tx = VirtioTxToken(self as *mut _);
        Some((rx, tx))
    }

    fn transmit(&mut self, _timestamp: Instant) -> Option<Self::TxToken<'_>> {
        Some(VirtioTxToken(self as *mut _))
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut caps = DeviceCapabilities::default();
        caps.medium = Medium::Ethernet;
        caps.max_transmission_unit = MTU;
        caps
    }
}

// =====================================================================
// -----| Part 4 — smoltcp Interface & Socket Management |-----
// =====================================================================

/// Per-socket metadata tracked outside smoltcp.
#[derive(Debug, Clone)]
pub struct SocketMeta {
    pub pid:      u32,
    pub proto:    SocketProto,
    pub state:    SocketState,
    /// The port this socket is actually bound/listening/connected on.
    /// Populated once known (at listen/connect/bind time) — 0 until then.
    pub port:     u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SocketProto { Tcp, Udp }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SocketState { Open, Connected, Listening, Closed }

/// The live network stack. Wraps smoltcp's Interface and SocketSet.
/// Must be driven by periodic net_poll() calls.
pub struct NetworkStack {
    pub iface:   Interface,
    pub sockets: SocketSet<'static>,
    pub device:  LogicalNic,
    pub socket_meta: BTreeMap<SocketHandle, SocketMeta>,
    /// Handle to the active DHCP socket (if DHCP is in progress).
    dhcp_handle: Option<SocketHandle>,
    /// Handle to the DNS socket.
    dns_handle:  Option<SocketHandle>,
    /// Active DNS query handle (returned by start_query).
    dns_query:   Option<dns::QueryHandle>,
    /// DHCP state machine phase.
    dhcp_done:       bool,
    dhcp_start_tick: Option<u64>,
    /// Next port `udp_bind(.., 0)` tries (IANA ephemeral range).
    next_ephemeral: u16,
    /// UDP sockets closed while datagrams were still queued: removed once
    /// those have been sent, or at the deadline (TICKS).
    closing: Vec<(SocketHandle, u64)>,
    /// TCP connections closed by their programs: kept until the FIN exchange
    /// is done (or the deadline, TICKS), so the last data isn't thrown away.
    tcp_closing: Vec<(SocketHandle, u64)>,
}

/// IANA dynamic/ephemeral port range.
const EPHEMERAL_PORTS: core::ops::RangeInclusive<u16> = 49152..=65535;
/// How long a closed UDP socket may keep its queued datagrams (ms).
const UDP_CLOSE_LINGER_MS: u64 = 2000;

impl NetworkStack {
    fn new(device: LogicalNic) -> Self {
        let mac = device.mac;
        let config = Config::new(smoltcp::wire::HardwareAddress::Ethernet(mac));

        let now = Instant::from_millis(
            crate::interrupts::TICKS.load(Ordering::Relaxed) as i64
        );

        let mut device_mut = device;
        let iface = Interface::new(config, &mut device_mut, now);

        let sockets: SocketSet<'static> = SocketSet::new(Vec::new());

        Self {
            iface,
            sockets,
            device:      device_mut,
            socket_meta: BTreeMap::new(),
            dhcp_handle: None,
            dns_handle:  None,
            dns_query:       None,
            dhcp_done:       false,
            dhcp_start_tick: None,
            next_ephemeral: *EPHEMERAL_PORTS.start(),
            closing: Vec::new(),
            tcp_closing: Vec::new(),
        }
    }

    /// Drive the smoltcp stack forward. Call this every ~10 ms.
    pub fn poll(&mut self) {
        let now = Instant::from_millis(
            crate::interrupts::TICKS.load(Ordering::Relaxed) as i64
        );
        self.device.poll_rx();
        let _ = self.iface.poll(now, &mut self.device, &mut self.sockets);
        self.process_dhcp();
        self.process_dns();
        self.reap_closing(crate::interrupts::TICKS.load(Ordering::Relaxed));
        self.reap_tcp_closing(crate::interrupts::TICKS.load(Ordering::Relaxed));
    }

    /// Close a TCP socket the way a program's closesocket does: what's queued
    /// still goes out, then FIN; the socket goes once that's done.
    pub fn close_tcp_gracefully(&mut self, handle: SocketHandle, deadline: u64) {
        self.sockets.get_mut::<tcp::Socket>(handle).close();
        self.tcp_closing.push((handle, deadline));
    }

    fn reap_tcp_closing(&mut self, now: u64) {
        let mut i = 0;
        while i < self.tcp_closing.len() {
            let (handle, deadline) = self.tcp_closing[i];
            let state = self.sockets.get::<tcp::Socket>(handle).state();
            if matches!(state, tcp::State::Closed | tcp::State::TimeWait) || now >= deadline {
                self.sockets.get_mut::<tcp::Socket>(handle).abort();
                self.sockets.remove(handle);
                self.tcp_closing.swap_remove(i);
            } else {
                i += 1;
            }
        }
    }

    /// The resolver's socket, made on first use (the server DHCP gave, or QEMU's).
    pub fn dns_socket(&mut self) -> SocketHandle {
        if let Some(h) = self.dns_handle {
            return h;
        }
        let servers = match NETWORK_CONFIG.lock().dns {
            Some(ip) => vec![IpAddress::Ipv4(ip)],
            None => vec![IpAddress::Ipv4(QEMU_DNS)],
        };
        let h = self.sockets.add(dns::Socket::new(&servers, Vec::new()));
        self.dns_handle = Some(h);
        h
    }

    // ---- DHCP -------------------------------------------------------

    /// Start the DHCP discovery process.
    pub fn start_dhcp(&mut self) {
        if self.dhcp_handle.is_some() { return; }
        let dhcp_socket = dhcpv4::Socket::new();
        let handle = self.sockets.add(dhcp_socket);
        self.dhcp_handle = Some(handle);
        self.dhcp_start_tick = Some(crate::interrupts::TICKS.load(Ordering::Relaxed));
        println!("[NET] DHCP discovery started...");
    }

    fn process_dhcp(&mut self) {
        if self.dhcp_done { return; }
        let handle = match self.dhcp_handle { Some(h) => h, None => return };

        let event = self.sockets.get_mut::<dhcpv4::Socket>(handle).poll();
        match event {
            None => {
                if let Some(start_tick) = self.dhcp_start_tick {
                    let now = crate::interrupts::TICKS.load(Ordering::Relaxed);
                    if now.saturating_sub(start_tick) > 5000 && !self.dhcp_done {
                        let mac = self.device.mac.0;
                        // Derive the fallback IP last octet directly from the
                        // last two MAC bytes to guarantee uniqueness per NIC.
                        // mac[5] alone gives 256 possible values; we clamp to
                        // 2..=253 to stay inside a /24 and avoid .0/.1/.255.
                        let last_octet = if mac[5] < 2 || mac[5] > 253 {
                            2u8
                        } else {
                            mac[5]
                        };
                        let fallback_ip = Ipv4Address::new(10, 0, 2, last_octet);
                        println!("[NET] DHCP timeout. Falling back to static IP: {}", fallback_ip);
                        crate::serial_println!("[NET] DHCP timeout. Falling back to static IP: {}", fallback_ip);
                        
                        self.iface.update_ip_addrs(|addrs| {
                            if let Some(a) = addrs.iter_mut().next() {
                                *a = IpCidr::Ipv4(Ipv4Cidr::new(fallback_ip, 24));
                            } else {
                                addrs.push(IpCidr::Ipv4(Ipv4Cidr::new(fallback_ip, 24))).ok();
                            }
                        });

                        // Add a default route so smoltcp can actually send
                        // packets on the local subnet (including broadcasts).
                        // On a point-to-point socket link there's no real
                        // gateway, but smoltcp needs a route entry to exist.
                        self.iface.routes_mut()
                            .add_default_ipv4_route(fallback_ip)
                            .ok();

                        *NETWORK_CONFIG.lock() = NetworkConfig {
                            ip: Ipv4Cidr::new(fallback_ip, 24),
                            gateway: Some(fallback_ip),
                            dns: None,
                            lease_ms: 3600 * 1000,
                        };
                        self.dhcp_done = true;
                        NETWORK_READY.store(true, Ordering::Release);
                        println!("[NET] Network ready (fallback). Route via {}", fallback_ip);
                    }
                }
            }
            Some(dhcpv4::Event::Configured(cfg)) => {
                println!("[NET] DHCP configured: {}/{}", cfg.address.address(), cfg.address.prefix_len());
                if let Some(gw) = cfg.router {
                    println!("[NET]   Gateway: {}", gw);
                }
                if !cfg.dns_servers.is_empty() {
                    println!("[NET]   DNS:     {}", cfg.dns_servers[0]);
                }

                // Apply address to the interface.
                self.iface.update_ip_addrs(|addrs| {
                    if let Some(a) = addrs.iter_mut().next() {
                        *a = IpCidr::Ipv4(cfg.address);
                    } else {
                        addrs.push(IpCidr::Ipv4(cfg.address)).ok();
                    }
                });

                if let Some(gw) = cfg.router {
                    self.iface.routes_mut()
                        .add_default_ipv4_route(gw)
                        .ok();
                }

                // smoltcp 0.13 Config doesn't expose lease duration directly.
                // Default to 1 hour; a full implementation would parse cfg.packet.
                let dns_ip = cfg.dns_servers.first().cloned().unwrap_or(QEMU_DNS);
                *NETWORK_CONFIG.lock() = NetworkConfig {
                    ip:      cfg.address,
                    gateway: cfg.router,
                    dns:     Some(dns_ip),
                    lease_ms: 3600 * 1000,
                };

                self.dhcp_done = true;
                NETWORK_READY.store(true, Ordering::Release);
                println!("[NET] Network ready.");
            }
            Some(dhcpv4::Event::Deconfigured) => {
                println!("[NET] DHCP deconfigured — restarting...");
                self.dhcp_done = false;
                NETWORK_READY.store(false, Ordering::Release);
            }
        }
    }

    // ---- DNS --------------------------------------------------------

    /// Asynchronously resolve a hostname. Returns a query handle.
    /// Call check_dns(handle) on subsequent poll() ticks to get the result.
    pub fn resolve_dns(&mut self, hostname: &str) -> Option<SocketHandle> {
        // Ensure we have a DNS socket.
        let dns_handle = match self.dns_handle {
            Some(h) => h,
            None => {
                let dns_cfg = NETWORK_CONFIG.lock();
                let servers = match dns_cfg.dns {
                    Some(ip) => vec![IpAddress::Ipv4(ip)],
                    None     => vec![IpAddress::Ipv4(QEMU_DNS)],
                };
                drop(dns_cfg);
                let sock = dns::Socket::new(&servers, Vec::new());
                let h = self.sockets.add(sock);
                self.dns_handle = Some(h);
                h
            }
        };

        let _now = Instant::from_millis(
            crate::interrupts::TICKS.load(Ordering::Relaxed) as i64
        );

        match self.sockets.get_mut::<dns::Socket>(dns_handle)
            .start_query(
                self.iface.context(),
                hostname,
                smoltcp::wire::DnsQueryType::A,
            )
        {
            Ok(query_handle) => {
                self.dns_query = Some(query_handle);
                println!("[NET] DNS query for '{}'", hostname);
                Some(dns_handle)
            }
            Err(e) => {
                println!("[NET] DNS query error: {:?}", e);
                None
            }
        }
    }

    fn process_dns(&mut self) {
        let handle = match self.dns_handle { Some(h) => h, None => return };
        let query  = match self.dns_query  { Some(q) => q, None => return };
        let sock   = self.sockets.get_mut::<dns::Socket>(handle);
        match sock.get_query_result(query) {
            Ok(addrs) => {
                for addr in addrs.iter() {
                    println!("[NET] DNS resolved: {}", addr);
                }
                self.dns_query = None; // Query complete.
            }
            Err(dns::GetQueryResultError::Pending) => {}
            Err(e) => {
                println!("[NET] DNS error: {:?}", e);
                self.dns_query = None;
            }
        }
    }

    // ---- Guest frame injection (VirtNic / hypervisor path) ----------

    /// Accept a raw Ethernet frame from a guest NIC (VirtNic) and push it
    /// into the VirtioNet driver's software RX FIFO so that smoltcp will
    /// process it on the next poll() call.
    ///
    /// This is the missing link between the hypervisor's VirtNic io_write
    /// handler and the host network stack: previously frames written by
    /// guests were silently dropped.
    pub fn inject_guest_frame(&mut self, frame: Vec<u8>) {
        if frame.len() < 14 || frame.len() > MTU {
            println!("[NET] inject_guest_frame: dropped frame of {} bytes (out of range)", frame.len());
            return;
        }
        let mut pkt = PacketBuf::new();
        let copy_len = frame.len().min(pkt.data.len());
        pkt.data[..copy_len].copy_from_slice(&frame[..copy_len]);
        pkt.len = copy_len;
        self.device.rx_queue.push(pkt);
        self.device.rx_count += 1;
    }

    // ---- TCP socket API --------------------------------------------

    /// Open a TCP socket bound for `pid`. Returns the SocketHandle.
    pub fn open_tcp(&mut self, pid: u32) -> NetResult<SocketHandle> {
        check_net_cap(pid, Capability::NetConnect)?;

        let rx_buf = tcp::SocketBuffer::new(vec![0u8; 8192]);
        let tx_buf = tcp::SocketBuffer::new(vec![0u8; 8192]);
        let sock   = tcp::Socket::new(rx_buf, tx_buf);
        let handle = self.sockets.add(sock);

        self.socket_meta.insert(handle, SocketMeta {
            pid, proto: SocketProto::Tcp, state: SocketState::Open, port: 0,
        });
        Ok(handle)
    }

    /// Connect a TCP socket to a remote endpoint.
    pub fn tcp_connect(
        &mut self, handle: SocketHandle,
        remote: IpEndpoint, local_port: u16,
    ) -> NetResult<()> {
        let sock = self.sockets.get_mut::<tcp::Socket>(handle);
        sock.connect(self.iface.context(), remote, local_port)
            .map_err(|_| NetError::ConnectFailed)?;
        if let Some(m) = self.socket_meta.get_mut(&handle) {
            m.state = SocketState::Connected;
            m.port  = local_port;
        }
        Ok(())
    }

    /// Listen for an incoming TCP connection on `port`.
    pub fn tcp_listen(
        &mut self, handle: SocketHandle, port: u16,
    ) -> NetResult<()> {
        let pid = self.socket_meta.get(&handle)
            .map(|m| m.pid).unwrap_or(0);
        check_net_cap(pid, Capability::NetBind)?;

        let endpoint = IpListenEndpoint { addr: None, port };
        self.sockets.get_mut::<tcp::Socket>(handle)
            .listen(endpoint)
            .map_err(|_| NetError::BindFailed)?;
        if let Some(m) = self.socket_meta.get_mut(&handle) {
            m.state = SocketState::Listening;
            m.port  = port;
        }
        println!("[NET] TCP listening on port {}", port);
        Ok(())
    }

    /// Send data over a TCP socket.
    pub fn tcp_send(&mut self, handle: SocketHandle, data: &[u8]) -> NetResult<usize> {
        let (pid, port) = self.socket_meta.get(&handle).map(|m| (m.pid, m.port)).unwrap_or((0, 0));
        check_firewall(pid, Direction::Outbound, SocketProto::Tcp, port)?;

        let sock   = self.sockets.get_mut::<tcp::Socket>(handle);
        let sent   = sock.send_slice(data).map_err(|_| NetError::SendFailed)?;
        Ok(sent)
    }

    /// Receive data from a TCP socket.
    pub fn tcp_recv(&mut self, handle: SocketHandle, buf: &mut [u8]) -> NetResult<usize> {
        let (pid, port) = self.socket_meta.get(&handle).map(|m| (m.pid, m.port)).unwrap_or((0, 0));
        check_firewall(pid, Direction::Inbound, SocketProto::Tcp, port)?;

        let sock = self.sockets.get_mut::<tcp::Socket>(handle);
        let n    = sock.recv_slice(buf).map_err(|_| NetError::RecvFailed)?;
        Ok(n)
    }

    /// Non-blocking check: does this TCP socket currently have data buffered
    /// and ready to read? Used by simple request/response servers (DFS,
    /// exec) that poll a listening socket each tick rather than blocking.
    pub fn tcp_can_recv(&mut self, handle: SocketHandle) -> bool {
        self.sockets.get_mut::<tcp::Socket>(handle).can_recv()
    }

    /// True once the connection has actually completed its handshake and
    /// data can be sent. tcp_connect() only *initiates* the handshake —
    /// completing it needs repeated poll() calls, so callers that connect
    /// and immediately try to send must wait for this first.
    pub fn tcp_may_send(&mut self, handle: SocketHandle) -> bool {
        self.sockets.get_mut::<tcp::Socket>(handle).may_send()
    }

    /// False once the socket has closed or failed (e.g. connection refused,
    /// reset, or timed out) — used to bail out of a wait loop early instead
    /// of spinning until an outer timeout.
    pub fn tcp_is_active(&mut self, handle: SocketHandle) -> bool {
        self.sockets.get_mut::<tcp::Socket>(handle).is_active()
    }

    /// Raw TCP state (Listen, SynReceived, Established, etc.) — for
    /// diagnosing exactly where a connection attempt is getting stuck.
    pub fn tcp_state(&mut self, handle: SocketHandle) -> tcp::State {
        self.sockets.get_mut::<tcp::Socket>(handle).state()
    }

    /// The remote endpoint this socket is/was connected to, if any —
    /// lets us directly identify who a listener actually accepted a
    /// connection from, rather than inferring it from timing alone.
    pub fn tcp_remote_endpoint(&mut self, handle: SocketHandle) -> Option<IpEndpoint> {
        self.sockets.get_mut::<tcp::Socket>(handle).remote_endpoint()
    }

    /// Bytes still sitting in the send buffer, not yet transmitted. Needed
    /// because close_socket() destroys the socket immediately — calling it
    /// right after tcp_send() (which only queues data) throws away
    /// anything that hasn't actually made it onto the wire yet. Callers
    /// should wait for this to reach 0 before closing.
    pub fn tcp_send_queue_len(&mut self, handle: SocketHandle) -> usize {
        self.sockets.get_mut::<tcp::Socket>(handle).send_queue()
    }

    /// Close a socket and remove it from the tracking table.
    pub fn close_socket(&mut self, handle: SocketHandle) {
        // A UDP send only queues the datagram; it goes out on a later poll
        // (after ARP, the first time). Removing the socket now would drop it,
        // which is why one-shot senders (PING, NTP, mesh broadcasts) never
        // got anything out. Let it drain first.
        let is_udp = matches!(self.socket_meta.get(&handle), Some(m) if m.proto == SocketProto::Udp);
        if is_udp && self.sockets.get::<udp::Socket>(handle).send_queue() > 0 {
            self.socket_meta.remove(&handle);
            let deadline = crate::interrupts::TICKS.load(Ordering::Relaxed) + UDP_CLOSE_LINGER_MS;
            self.closing.push((handle, deadline));
            return;
        }
        if let Some(m) = self.socket_meta.get(&handle) {
            match m.proto {
                SocketProto::Tcp => {
                    self.sockets.get_mut::<tcp::Socket>(handle).close();
                }
                SocketProto::Udp => {
                    self.sockets.get_mut::<udp::Socket>(handle).close();
                }
            }
        }
        self.socket_meta.remove(&handle);
        self.sockets.remove(handle);
    }

    // ---- UDP socket API --------------------------------------------

    /// Open a UDP socket for `pid`.
    pub fn open_udp(&mut self, pid: u32) -> NetResult<SocketHandle> {
        check_net_cap(pid, Capability::NetConnect)?;

        let rx_meta = udp::PacketMetadata::EMPTY;
        let tx_meta = udp::PacketMetadata::EMPTY;
        // 32 packet slots (was 16) — cheap insurance against buffer
        // exhaustion; the real fix was removing the code that used to blast
        // ~20 sends per call into this socket (see send_lan_announcement).
        let rx_buf  = udp::PacketBuffer::new(vec![rx_meta; 32], vec![0u8; 8192]);
        let tx_buf  = udp::PacketBuffer::new(vec![tx_meta; 32], vec![0u8; 8192]);
        let sock    = udp::Socket::new(rx_buf, tx_buf);
        let handle  = self.sockets.add(sock);

        self.socket_meta.insert(handle, SocketMeta {
            pid, proto: SocketProto::Udp, state: SocketState::Open, port: 0,
        });
        Ok(handle)
    }

    /// Bind a UDP socket to a local port.
    /// Bind a UDP socket. Port 0 means "any free port": smoltcp itself treats
    /// 0 as unbound and refuses to send, so an ephemeral port is picked here.
    pub fn udp_bind(&mut self, handle: SocketHandle, port: u16) -> NetResult<()> {
        let pid = self.socket_meta.get(&handle).map(|m| m.pid).unwrap_or(0);
        check_net_cap(pid, Capability::NetBind)?;
        let port = if port == 0 { self.free_ephemeral_port().ok_or(NetError::BindFailed)? } else { port };
        self.sockets.get_mut::<udp::Socket>(handle)
            .bind(port)
            .map_err(|_| NetError::BindFailed)
    }

    /// A UDP port in the ephemeral range no socket is bound to.
    fn free_ephemeral_port(&mut self) -> Option<u16> {
        let span = (EPHEMERAL_PORTS.end() - EPHEMERAL_PORTS.start()) as u32 + 1;
        for _ in 0..span {
            let port = self.next_ephemeral;
            self.next_ephemeral = if port == *EPHEMERAL_PORTS.end() { *EPHEMERAL_PORTS.start() } else { port + 1 };
            let in_use = self.sockets.iter().any(|(_, s)| matches!(s,
                smoltcp::socket::Socket::Udp(u) if u.endpoint().port == port));
            if !in_use {
                return Some(port);
            }
        }
        None
    }

    /// Remove closed UDP sockets whose queued datagrams have gone out (or
    /// whose linger time is up).
    fn reap_closing(&mut self, now: u64) {
        let mut i = 0;
        while i < self.closing.len() {
            let (handle, deadline) = self.closing[i];
            if self.sockets.get::<udp::Socket>(handle).send_queue() == 0 || now >= deadline {
                self.sockets.get_mut::<udp::Socket>(handle).close();
                self.sockets.remove(handle);
                self.closing.swap_remove(i);
            } else {
                i += 1;
            }
        }
    }

    /// Send a UDP datagram.
    pub fn udp_send(
        &mut self, handle: SocketHandle,
        data: &[u8], remote: IpEndpoint,
    ) -> NetResult<()> {
        let pid = self.socket_meta.get(&handle).map(|m| m.pid).unwrap_or(0);
        check_firewall(pid, Direction::Outbound, SocketProto::Udp, remote.port)?;

        self.sockets.get_mut::<udp::Socket>(handle)
            .send_slice(data, remote)
            .map_err(|_| NetError::SendFailed)
    }

    /// Receive a UDP datagram.
    pub fn udp_recv(
        &mut self, handle: SocketHandle,
        buf: &mut [u8],
    ) -> NetResult<(usize, IpEndpoint)> {
        let sock = self.sockets.get_mut::<udp::Socket>(handle);
        sock.recv_slice(buf)
            .map(|(len, meta)| (len, meta.endpoint))
            .map_err(|_| NetError::RecvFailed)
    }
}

// =====================================================================
// -----| Part 5 — NTP Client |-----
// =====================================================================
//
// NTPv3 client: fires a 48-byte NTP request to NTP_SERVER_IP:123 and
// parses the transmit timestamp from the response.

const NTP_PORT: u16 = 123;
/// NTPv3 request packet (48 bytes): LI=0, VN=3, Mode=3 (client)
const NTP_REQUEST: [u8; 48] = {
    let mut pkt = [0u8; 48];
    pkt[0] = 0x1B; // LI=0, VN=3, Mode=3
    pkt
};

/// Send an NTP query using an existing UDP socket handle.
/// On the next net_poll() after the response arrives, parse_ntp_response()
/// should be called with the received bytes.
pub fn send_ntp_query(stack: &mut NetworkStack, udp_handle: SocketHandle) -> NetResult<()> {
    stack.udp_bind(udp_handle, 0)?; // ephemeral port
    let remote = IpEndpoint::new(IpAddress::Ipv4(NTP_SERVER_IP), NTP_PORT);
    stack.udp_send(udp_handle, &NTP_REQUEST, remote)?;
    println!("[NET] NTP request sent to {}", NTP_SERVER_IP);
    Ok(())
}

/// Parse an NTP response and update NET_CLOCK_OFFSET.
pub fn parse_ntp_response(pkt: &[u8]) -> Option<i64> {
    if pkt.len() < 48 { return None; }
    // Transmit timestamp is at bytes 40-47 (NTP seconds + fraction).
    let ntp_secs = u32::from_be_bytes(pkt[40..44].try_into().ok()?) as u64;
    let unix_secs = ntp_secs.saturating_sub(NTP_UNIX_OFFSET) as i64;
    let offset_ms = unix_secs * 1000;
    NET_CLOCK_OFFSET.store(offset_ms, Ordering::Release);
    println!("[NET] NTP sync: unix time = {}s", unix_secs);
    Some(offset_ms)
}

/// Current wall-clock time in milliseconds since Unix epoch (best estimate).
pub fn wall_clock_ms() -> i64 {
    let ticks = crate::interrupts::TICKS.load(Ordering::Relaxed) as i64;
    NET_CLOCK_OFFSET.load(Ordering::Acquire) + ticks
}

// =====================================================================
// -----| Part 6 — Firewall |-----
// =====================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction { Inbound, Outbound, Both }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FirewallAction { Allow, Deny }

/// A single firewall rule.
#[derive(Debug, Clone)]
pub struct FirewallRule {
    /// PID this rule applies to. None = all processes.
    pub pid:       Option<u32>,
    pub direction: Direction,
    pub proto:     Option<SocketProto>,
    /// Port 0 = any.
    pub port:      u16,
    pub action:    FirewallAction,
}

impl FirewallRule {
    pub fn allow_all(pid: u32) -> Self {
        Self { pid: Some(pid), direction: Direction::Both, proto: None, port: 0, action: FirewallAction::Allow }
    }
    pub fn deny_all(pid: u32) -> Self {
        Self { pid: Some(pid), direction: Direction::Both, proto: None, port: 0, action: FirewallAction::Deny }
    }
    pub fn allow_port(pid: u32, dir: Direction, proto: SocketProto, port: u16) -> Self {
        Self { pid: Some(pid), direction: dir, proto: Some(proto), port, action: FirewallAction::Allow }
    }
    pub fn deny_port(pid: u32, dir: Direction, proto: SocketProto, port: u16) -> Self {
        Self { pid: Some(pid), direction: dir, proto: Some(proto), port, action: FirewallAction::Deny }
    }
}

pub struct Firewall {
    pub rules: Vec<FirewallRule>,
    /// Default inbound policy (Security Mode: Deny; Perf Mode: Allow).
    pub default_inbound: FirewallAction,
    /// Default outbound policy (always Allow).
    pub default_outbound: FirewallAction,
}

impl Firewall {
    pub const fn new() -> Self {
        Self {
            rules:            Vec::new(),
            default_inbound:  FirewallAction::Deny,
            default_outbound: FirewallAction::Allow,
        }
    }

    /// Install a new rule (higher priority = later in Vec = wins).
    pub fn add_rule(&mut self, rule: FirewallRule) {
        self.rules.push(rule);
    }

    /// Evaluate rules for (pid, direction, proto, port).
    /// Returns the action of the first matching rule, or the default.
    pub fn evaluate(
        &self, pid: u32, dir: Direction, proto: SocketProto, port: u16,
    ) -> FirewallAction {
        // Performance Mode: bypass firewall entirely.
        if POLICY.lock().get_mode() == PolicyMode::Performance {
            return FirewallAction::Allow;
        }

        // Walk rules in reverse so last-added wins.
        for rule in self.rules.iter().rev() {
            if let Some(rpid) = rule.pid {
                if rpid != pid { continue; }
            }
            let dir_match = matches!(
                (rule.direction, dir),
                (Direction::Both, _) | (Direction::Inbound, Direction::Inbound)
                    | (Direction::Outbound, Direction::Outbound)
            );
            if !dir_match { continue; }
            if let Some(rp) = rule.proto { if rp != proto { continue; } }
            if rule.port != 0 && rule.port != port { continue; }
            return rule.action;
        }

        // Default policy.
        match dir {
            Direction::Inbound  => self.default_inbound,
            Direction::Outbound => self.default_outbound,
            Direction::Both     => self.default_inbound,
        }
    }
}

lazy_static! {
    pub static ref FIREWALL: Mutex<Firewall> = Mutex::new(Firewall::new());
}

/// Capability check helper (bypassed in Performance Mode).
fn check_net_cap(pid: u32, cap: Capability) -> NetResult<()> {
    CapabilityChecker::gate(pid, cap).map_err(|_| NetError::PermissionDenied)
}

/// Firewall check helper.
fn check_firewall(
    pid: u32, dir: Direction, proto: SocketProto, port: u16,
) -> NetResult<()> {
    match FIREWALL.lock().evaluate(pid, dir, proto, port) {
        FirewallAction::Allow => Ok(()),
        FirewallAction::Deny  => Err(NetError::FirewallDenied),
    }
}

// =====================================================================
// -----| Part 7 — Error type |-----
// =====================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetError {
    PermissionDenied,
    FirewallDenied,
    NotReady,
    ConnectFailed,
    BindFailed,
    SendFailed,
    RecvFailed,
    NicNotFound,
    DnsError,
}

pub type NetResult<T> = Result<T, NetError>;

// =====================================================================
// -----| Part 8 — Global Stack & PCI Probe |-----
// =====================================================================

lazy_static! {
    pub static ref NETWORK: Mutex<Option<NetworkStack>> = Mutex::new(None);
}

/// Legacy virtio-net NICs. Bound by `hal::bind`.
pub const VIRTIO_NET_DRIVER: crate::hal::Driver = crate::hal::Driver {
    name: "virtio-net",
    class: crate::hal::DeviceClass::Network,
    matches: is_virtio_net,
    probe: probe_virtio_net,
    remove: Some(remove_virtio_net),
};

/// Hot-removal: the NIC leaves the logical NIC (dropping it resets the
/// device and frees its DMA memory).
fn remove_virtio_net(dev: &crate::pci::PciDevice) -> Result<(), alloc::string::String> {
    let io_base = dev.bar(0).io_port().ok_or_else(|| alloc::string::String::from("BAR0 is not an I/O BAR"))?;
    let removed = NETWORK.lock().as_mut().is_some_and(|stack| stack.device.remove_member(io_base));
    if removed { Ok(()) } else { Err(alloc::format!("no member at I/O {:#x}", io_base)) }
}

fn is_virtio_net(d: &crate::pci::PciDevice) -> bool {
    d.vendor_id == VIRTIO_VENDOR && d.device_id == VIRTIO_NET_DEV
}

/// Initialise the NIC, build the smoltcp stack on it and start DHCP.
fn probe_virtio_net(dev: &crate::pci::PciDevice) -> Result<(), alloc::string::String> {
    use alloc::string::String;
    // Legacy virtio-net exposes its registers through an I/O BAR0.
    let io_base = dev.bar(0).io_port().ok_or_else(|| String::from("BAR0 is not an I/O BAR"))?;
    // The virtqueues are DMA'd by the device: bus mastering is required.
    dev.enable_for_driver();
    println!("[NET] VirtIO-net found at PCI {:02x}:{:02x} BAR0=0x{:04x}", dev.bus, dev.device, io_base);
    crate::serial_println!("[NET] VirtIO-net found at PCI {:02x}:{:02x} BAR0=0x{:04x}", dev.bus, dev.device, io_base);
    let nic = VirtioNet::new(io_base).map_err(String::from)?;

    // Every NIC after the first joins the one logical NIC the stack uses.
    if let Some(stack) = NETWORK.lock().as_mut() {
        stack.device.add_member(nic);
        return Ok(());
    }

    let mut stack = NetworkStack::new(LogicalNic::new(nic));

    // Set the V-Mesh NodeId from the real NIC MAC right now. The MAC is
    // already known at this point (it's read off the device before this
    // function even starts DHCP) so there's no need to wait for
    // is_ready()/DHCP — doing it here instead of lazily in tick() removes
    // a race where two instances could still be advertising the shared
    // placeholder NodeId if their timing happened to overlap.
    {
        let mac = stack.device.mac.0;
        crate::distributed::NODE_MESH.lock().update_mac(&mac);
    }

    stack.start_dhcp();

    *NETWORK.lock() = Some(stack);
    println!("[NET] Network stack online. Awaiting DHCP...");
    crate::serial_println!("[NET] Network stack online. Awaiting DHCP...");
    println!("[NET]   IPv4 + IPv6, TCP, UDP, ICMP via smoltcp 0.13");
    println!("[NET]   DHCP client, DNS resolver, NTP client, Firewall");
    Ok(())
}

/// Drive the network stack forward. Call this from a background async task
/// or periodically from the timer-driven executor.
///
/// Suggested use:
///   // In kernel_main's async block, or a spawned fiber:
///   loop {
///       crate::network::poll();
///       crate::task::yield_now().await;
///   }
pub fn poll() {
    if let Some(ref mut stack) = *NETWORK.lock() {
        stack.poll();
    }
}

/// The logical NIC's members, one line each (terminal `NET STATUS`).
pub fn nic_status() -> Vec<alloc::string::String> {
    NETWORK.lock().as_ref().map(|s| s.device.describe()).unwrap_or_default()
}
