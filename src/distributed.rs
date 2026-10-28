// -----| VEILOS Distributed Layer |-----
//
// Phase 16 roadmap items implemented here:
//
//   [x] VEILOS Node Mesh
//         Zero-config LAN discovery  — UDP broadcast on DISCOVERY_PORT (7700).
//                                      Each node sends NodeAnnouncement packets
//                                      every ANNOUNCE_INTERVAL ms. Replies with
//                                      NodeHello. No configuration required.
//         WAN relay support          — TCP connection to a relay server
//                                      (configurable IP:port). Nodes register
//                                      with a NodeId + capability bitmask; the
//                                      relay forwards discovery to remote subnets.
//         Distributed filesystem     — Files under the virtual mount "/net/<node>/"
//                                      are fetched from remote nodes over TCP on
//                                      DFS_PORT (7701) using a simple framed
//                                      request/response protocol. Writes are
//                                      forwarded to the owning node.
//         Remote execution           — RemoteTask: serialised task descriptor sent
//                                      over TCP on EXEC_PORT (7702). The remote
//                                      node runs the task in a sandboxed process
//                                      and streams stdout back to the caller.
//         Shared GPU workloads       — GpuCommand batches serialised and forwarded
//                                      to a remote node with available GPU capacity
//                                      via GPU_PORT (7703). Results (rendered frames
//                                      or compute outputs) streamed back.
//
//   [x] VEILOS Cloud Mode
//         Cloud VM deployments       — CloudVmManager: fetches a VM disk image from
//                                      a remote node or object store via HTTP/TCP,
//                                      stages it locally, then calls
//                                      hypervisor::launch_windows_vm().
//         Remote desktop             — RdpServer: captures the VGA framebuffer,
//                                      applies RLE compression, and streams frames
//                                      to a connected client over TCP on RDP_PORT
//                                      (7704). Input events (keyboard/mouse) are
//                                      received on the same connection and injected
//                                      into the local input queues.
//         Cloud gaming host mode     — GamingHostMode wraps the RDP server with
//                                      game-tuned settings: 60 fps target, adaptive
//                                      bitrate based on RTT, and GPU frame capture
//                                      instead of VGA framebuffer capture.
//
// Wire protocol (all ports)
// -------------------------
//   All messages are framed as:
//     [0..4]  magic:   u32 LE = 0x564C4453 ("VLDS")
//     [4..8]  length:  u32 LE = payload byte count
//     [8..12] msg_type: u32 LE = MessageType enum value
//     [12..]  payload: [u8; length]
//
// All TCP connections use the VEILOS network stack (smoltcp) directly.
// UDP discovery uses the smoltcp UDP socket API.

#![allow(dead_code)]

extern crate alloc;

use alloc::vec::Vec;
use alloc::vec;
use alloc::string::String;
use alloc::collections::BTreeMap;
use spin::Mutex;
use lazy_static::lazy_static;
use core::sync::atomic::Ordering;
use crate::println;
use crate::network::{NETWORK, is_ready, NetResult, NetError, NetworkStack};
use smoltcp::wire::{IpAddress, IpEndpoint, Ipv4Address};

// =====================================================================
// -----| Part 1 — Constants & Ports |-----
// =====================================================================

pub const DISCOVERY_PORT: u16  = 7700;
pub const DFS_PORT:       u16  = 7701;
pub const EXEC_PORT:      u16  = 7702;
pub const GPU_PORT:       u16  = 7703;
pub const RDP_PORT:       u16  = 7704;
pub const RELAY_PORT:     u16  = 7705;
/// Port used by the persistent announcement UDP socket (outgoing).
/// Must be distinct from DISCOVERY_PORT and DFS_PORT to avoid conflicts.
pub const ANNOUNCE_PORT:  u16  = 7706;
pub const WIRE_MAGIC:     u32  = 0x564C_4453; // "VLDS"
pub const ANNOUNCE_INTERVAL: u64 = 5_000;     // ms between LAN announcements
pub const NODE_TIMEOUT:   u64  = 30_000;      // ms before a node is considered gone

/// smoltcp's tcp::Socket::connect() treats local port 0 as an invalid,
/// unaddressable port and rejects it synchronously (ConnectError, mapped to
/// our NetError::ConnectFailed) — it does *not* auto-assign an ephemeral
/// port the way a full OS sockets API would. Every outgoing TCP connection
/// needs a real, distinct local port picked by the caller. This is a simple
/// rolling counter through the IANA dynamic/private range (49152-65535);
/// good enough for a mesh that only ever has a handful of connections open
/// at once.
static NEXT_EPHEMERAL_PORT: core::sync::atomic::AtomicU16 = core::sync::atomic::AtomicU16::new(49152);

pub fn next_ephemeral_port() -> u16 {
    let p = NEXT_EPHEMERAL_PORT.fetch_add(1, Ordering::Relaxed);
    if p < 49152 {
        // Wrapped past 65535 back through zero — reset and use the floor.
        NEXT_EPHEMERAL_PORT.store(49153, Ordering::Relaxed);
        49152
    } else {
        p
    }
}

// Node capability flags.
pub const CAP_DFS:        u32  = 1 << 0;  // Hosts shared filesystem
pub const CAP_EXEC:       u32  = 1 << 1;  // Accepts remote tasks
pub const CAP_GPU:        u32  = 1 << 2;  // Shares GPU compute
pub const CAP_RDP:        u32  = 1 << 3;  // Offers remote desktop
pub const CAP_RELAY:      u32  = 1 << 4;  // Acts as WAN relay
pub const CAP_GAMING:     u32  = 1 << 5;  // Cloud gaming host

// =====================================================================
// -----| Part X — Node Types |-----
// =====================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum NodeType {
    Delegate = 0,
    Compute = 1,
    Offload = 2,
    Storage = 3,
    Controller = 4,
    Hub = 5,
    /// Demoted node: this is what a losing side of a Controller/Controller
    /// election becomes. Purely passive by default — a human can promote
    /// it back to any other role at any time via the mesh manager UI.
    Tenant = 6,
    Unknown = 255,
}

impl NodeType {
    pub fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::Delegate,
            1 => Self::Compute,
            2 => Self::Offload,
            3 => Self::Storage,
            4 => Self::Controller,
            5 => Self::Hub,
            6 => Self::Tenant,
            _ => Self::Unknown,
        }
    }
}

// =====================================================================
// -----| Part 2 — Wire Protocol |-----
// =====================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum MessageType {
    // Discovery
    NodeAnnouncement  = 0x01,
    NodeHello         = 0x02,
    NodeBye           = 0x03,
    RelayRegister     = 0x04,
    RelayForward      = 0x05,

    // Distributed filesystem
    DfsReadRequest    = 0x10,
    DfsReadResponse   = 0x11,
    DfsWriteRequest   = 0x12,
    DfsWriteResponse  = 0x13,
    DfsListRequest    = 0x14,
    DfsListResponse   = 0x15,

    // Remote execution
    ExecRequest       = 0x20,
    ExecStdout        = 0x21,
    ExecComplete      = 0x22,
    ExecError         = 0x23,

    // GPU sharing
    GpuSubmitBatch    = 0x30,
    GpuFrameResult    = 0x31,
    GpuCapQuery       = 0x32,
    GpuCapResponse    = 0x33,

    // Remote desktop / cloud gaming
    RdpConnect        = 0x40,
    RdpFrame          = 0x41,
    RdpInputKey       = 0x42,
    RdpInputMouse     = 0x43,
    RdpDisconnect     = 0x44,

    // Cloud VM
    VmImageFetch      = 0x50,
    VmImageChunk      = 0x51,
    VmImageComplete   = 0x52,
    VmLaunch          = 0x53,
    VmStatus          = 0x54,

    // Cluster Orchestration
    ClusterSearchRequest = 0x60,
    ClusterResourceOffer = 0x61,
    TaskOffloadRequest   = 0x62,
    BatchJobRequest      = 0x63,
    ResourceSurrender    = 0x64,
    ClusterDisconnect    = 0x65,
    ClusterCapUpdate     = 0x66,
    ClusterTypeUpdate    = 0x67,
    ClusterRoleRequest   = 0x68,
    ClusterRoleResponse  = 0x69,

    Unknown           = 0xFF,
}

impl MessageType {
    fn from_u32(v: u32) -> Self {
        match v {
            0x01 => Self::NodeAnnouncement,
            0x02 => Self::NodeHello,
            0x03 => Self::NodeBye,
            0x04 => Self::RelayRegister,
            0x05 => Self::RelayForward,
            0x10 => Self::DfsReadRequest,
            0x11 => Self::DfsReadResponse,
            0x12 => Self::DfsWriteRequest,
            0x13 => Self::DfsWriteResponse,
            0x14 => Self::DfsListRequest,
            0x15 => Self::DfsListResponse,
            0x20 => Self::ExecRequest,
            0x21 => Self::ExecStdout,
            0x22 => Self::ExecComplete,
            0x23 => Self::ExecError,
            0x30 => Self::GpuSubmitBatch,
            0x31 => Self::GpuFrameResult,
            0x32 => Self::GpuCapQuery,
            0x33 => Self::GpuCapResponse,
            0x40 => Self::RdpConnect,
            0x41 => Self::RdpFrame,
            0x42 => Self::RdpInputKey,
            0x43 => Self::RdpInputMouse,
            0x44 => Self::RdpDisconnect,
            0x50 => Self::VmImageFetch,
            0x51 => Self::VmImageChunk,
            0x52 => Self::VmImageComplete,
            0x53 => Self::VmLaunch,
            0x54 => Self::VmStatus,
            0x60 => Self::ClusterSearchRequest,
            0x61 => Self::ClusterResourceOffer,
            0x62 => Self::TaskOffloadRequest,
            0x63 => Self::BatchJobRequest,
            0x64 => Self::ResourceSurrender,
            0x65 => Self::ClusterDisconnect,
            0x66 => Self::ClusterCapUpdate,
            0x67 => Self::ClusterTypeUpdate,
            0x68 => Self::ClusterRoleRequest,
            0x69 => Self::ClusterRoleResponse,
            _    => Self::Unknown,
        }
    }
}

/// Encode a framed message ready to send over TCP or UDP.
pub fn encode_message(msg_type: MessageType, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(12 + payload.len());
    out.extend_from_slice(&WIRE_MAGIC.to_le_bytes());
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(&(msg_type as u32).to_le_bytes());
    out.extend_from_slice(payload);
    out
}

/// Decode a framed message from a byte buffer.
/// Returns (MessageType, payload_slice) or None if the buffer is too short
/// or the magic is wrong.
pub fn decode_message(buf: &[u8]) -> Option<(MessageType, &[u8])> {
    if buf.len() < 12 { return None; }
    let magic = u32::from_le_bytes(buf[0..4].try_into().ok()?);
    if magic != WIRE_MAGIC { return None; }
    let length   = u32::from_le_bytes(buf[4..8].try_into().ok()?) as usize;
    let msg_type = u32::from_le_bytes(buf[8..12].try_into().ok()?);
    if buf.len() < 12 + length { return None; }
    Some((MessageType::from_u32(msg_type), &buf[12..12 + length]))
}

// =====================================================================
// -----| Part 3 — Node Identity |-----
// =====================================================================

/// Unique 8-byte node identifier derived from the MAC address.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct NodeId(pub [u8; 8]);

impl NodeId {
    /// Derive a NodeId from the VirtIO-net MAC address + a constant suffix.
    pub fn from_mac(mac: &[u8; 6]) -> Self {
        let mut id = [0u8; 8];
        id[..6].copy_from_slice(mac);
        id[6] = 0x56; // 'V'
        id[7] = 0x4C; // 'L'
        Self(id)
    }

    pub fn as_bytes(&self) -> &[u8; 8] { &self.0 }

    pub fn to_hex(&self) -> String {
        let mut s = String::new();
        for b in &self.0 {
            // Manual hex formatting (no std::fmt::Write in no_std context here)
            let hi = b >> 4;
            let lo = b & 0xF;
            s.push(if hi < 10 { (b'0' + hi) as char } else { (b'a' + hi - 10) as char });
            s.push(if lo < 10 { (b'0' + lo) as char } else { (b'a' + lo - 10) as char });
        }
        s
    }

    /// Parse the format produced by to_hex() (16 lowercase-or-uppercase hex
    /// chars, no separators). Returns None on any malformed input.
    pub fn from_hex(s: &str) -> Option<Self> {
        let s = s.trim();
        if s.len() != 16 { return None; }
        let mut id = [0u8; 8];
        let bytes = s.as_bytes();
        for i in 0..8 {
            let hi = (bytes[i * 2] as char).to_digit(16)?;
            let lo = (bytes[i * 2 + 1] as char).to_digit(16)?;
            id[i] = ((hi << 4) | lo) as u8;
        }
        Some(Self(id))
    }
}

/// A remote node's known state.
#[derive(Debug, Clone)]
pub struct RemoteNode {
    pub id:           NodeId,
    pub ip:           Ipv4Address,
    pub node_type:    NodeType,
    pub capabilities: u32,
    pub hostname:     String,
    /// PIT tick of the last received announcement.
    pub last_seen:    u64,
    /// How many nodes the *sender* considers itself to have in its own
    /// mesh (its peer count + itself) at the time it sent this message.
    /// Used purely as a tiebreaker signal for Controller election.
    pub mesh_size:    u32,
}

impl RemoteNode {
    pub fn is_alive(&self, current_tick: u64) -> bool {
        current_tick.wrapping_sub(self.last_seen) < NODE_TIMEOUT
    }
}

// =====================================================================
// -----| Part 4 — LAN Discovery (UDP Broadcast) |-----
// =====================================================================

/// Serialise a NodeAnnouncement payload.
/// Wire format: [id(8)][type(1)][caps(4)][hostname_len(1)][hostname(n)][mesh_size(4)]
pub(crate) fn encode_announcement(node_id: &NodeId, node_type: NodeType, caps: u32, hostname: &str, mesh_size: u32) -> Vec<u8> {
    let hostname_bytes = hostname.as_bytes();
    let mut out = Vec::with_capacity(18 + hostname_bytes.len());
    out.extend_from_slice(node_id.as_bytes());
    out.push(node_type as u8);
    out.extend_from_slice(&caps.to_le_bytes());
    out.push(hostname_bytes.len() as u8);
    out.extend_from_slice(hostname_bytes);
    out.extend_from_slice(&mesh_size.to_le_bytes());
    out
}

/// Parse a NodeAnnouncement from a UDP payload, also capturing source IP.
fn decode_announcement(payload: &[u8], src_ip: Ipv4Address) -> Option<RemoteNode> {
    if payload.len() < 14 { return None; }
    let id_bytes: [u8; 8] = payload[0..8].try_into().ok()?;
    let node_type = NodeType::from_u8(payload[8]);
    let caps = u32::from_le_bytes(payload[9..13].try_into().ok()?);
    let hn_len = payload[13] as usize;
    if payload.len() < 14 + hn_len { return None; }
let hostname = String::from(core::str::from_utf8(&payload[14..14 + hn_len]).unwrap_or("unknown"));

    // mesh_size is a newer field appended at the end. Default to 1 if an
    // older/short payload doesn't carry it, so election logic degrades
    // gracefully instead of panicking on a slice-index error.
    let mesh_size = if payload.len() >= 14 + hn_len + 4 {
        u32::from_le_bytes(payload[14 + hn_len..18 + hn_len].try_into().ok()?)
    } else {
        1
    };

    let tick = crate::interrupts::TICKS.load(Ordering::Relaxed);
    Some(RemoteNode {
        id:           NodeId(id_bytes),
        ip:           src_ip,
        node_type,
        capabilities: caps,
        hostname,
        last_seen:    tick,
        mesh_size,
    })
}

// =====================================================================
// -----| Part 5 — WAN Relay Client |-----
// =====================================================================

/// Configuration for connecting to a WAN relay server.
#[derive(Debug, Clone)]
pub struct RelayConfig {
    pub server_ip:   Ipv4Address,
    pub server_port: u16,
}

/// Relay registration payload: [NodeId(8)] + [NodeType(1)] + [caps(4)] + [hostname...] + [mesh_size(4)]
fn encode_relay_register(node_id: &NodeId, node_type: NodeType, caps: u32, hostname: &str, mesh_size: u32) -> Vec<u8> {
    encode_announcement(node_id, node_type, caps, hostname, mesh_size)
}

// =====================================================================
// -----| Part 6 — Node Mesh Manager |-----
// =====================================================================

pub struct NodeMesh {
    pub local_id:      NodeId,
    pub local_type:    NodeType,
    pub local_caps:    u32,
    pub hostname:      String,
    pub peers:         BTreeMap<NodeId, RemoteNode>,
    pub relay_config:  Option<RelayConfig>,
    last_announce:     u64,
    relay_connected:   bool,
    udp_listen_handle: Option<smoltcp::iface::SocketHandle>,
    /// Persistent UDP socket used for sending announcements, hellos, and
    /// other discovery messages.  Kept alive so that smoltcp has time to
    /// flush queued packets on the next poll() cycle.
    announce_handle:   Option<smoltcp::iface::SocketHandle>,
    /// Whether sockets have been initialized
    sockets_initialized: bool,
    /// Whether MAC has been updated from actual NIC
    mac_updated: bool,
    /// Set while we have an outstanding "make me Controller" request awaiting
    /// a response from whoever we currently believe is the Controller.
    pending_promotion: bool,
    /// Listening TCP sockets for the DFS and remote-exec servers. Each
    /// handles one connection at a time — plenty for a hobby mesh — and gets
    /// torn down and reopened (back into Listen state) after each request.
    dfs_listen_handle:  Option<smoltcp::iface::SocketHandle>,
    exec_listen_handle: Option<smoltcp::iface::SocketHandle>,
    dfs_listen_warned:  bool,
    exec_listen_warned: bool,
    last_listener_state_log: u64,
    /// Tick at which the *current connection* on this listener left the
    /// Listen state, if any — None while it's just sitting idle waiting
    /// for a connection. Used to detect a connection that's genuinely
    /// stuck (accepted but never produces a request), without punishing a
    /// listener that simply sat idle in Listen for a long time before a
    /// perfectly normal connection finally arrived.
    dfs_conn_started_at:  Option<u64>,
    exec_conn_started_at: Option<u64>,
    /// Set once a response has been queued via tcp_send() and we're
    /// waiting for it to actually drain out of the send buffer before
    /// calling close_socket() — closing immediately after tcp_send()
    /// destroys the socket before smoltcp gets a chance to transmit
    /// whatever was still queued, silently discarding the response.
    dfs_awaiting_close:  bool,
    exec_awaiting_close: bool,
}


impl NodeMesh {
    pub fn new(local_id: NodeId, hostname: &str) -> Self {
        let node_type = NodeType::Controller;
        // Advertise all capabilities this node provides.
        let caps = CAP_DFS | CAP_EXEC | CAP_GPU | CAP_RDP | CAP_GAMING;
        Self {
            local_id,
            local_type:      node_type,
            local_caps:      caps,
            hostname:        String::from(hostname),
            peers:           BTreeMap::new(),
            relay_config:    None,
            last_announce:   0,
            relay_connected: false,
            udp_listen_handle: None,
            announce_handle:   None,
            sockets_initialized: false,
            mac_updated: false,
            pending_promotion: false,
            dfs_listen_handle: None,
            exec_listen_handle: None,
            dfs_listen_warned: false,
            exec_listen_warned: false,
            last_listener_state_log: 0,
            dfs_conn_started_at: None,
            exec_conn_started_at: None,
            dfs_awaiting_close: false,
            exec_awaiting_close: false,
        }
    }

    pub fn init_sockets(&mut self) {
        println!("[DIST] init_sockets: sockets will be created on first tick");
        self.sockets_initialized = true;
    }

    /// Update the node ID with the actual MAC-derived ID. Safe to call more
    /// than once (e.g. once eagerly from network::init(), and again as a
    /// fallback from tick()) — it's a no-op after the first successful call.
    pub fn update_mac(&mut self, mac: &[u8; 6]) {
        if self.mac_updated { return; }
        self.local_id = NodeId::from_mac(mac);
        self.mac_updated = true;
        // serial_println! (not println!) so this is visible in the
        // vm*_serial.log capture, not just the in-VM VGA console.
        crate::serial_println!("[DIST] MAC-derived NodeId: {}", self.local_id.to_hex());
    }

    /// Size of the mesh as seen from here: ourselves plus every peer we
    /// currently consider alive. Used as the primary Controller-election
    /// signal — see `resolve_controller_conflict`.
    pub fn local_mesh_size(&self) -> u32 {
        self.peers.len() as u32 + 1
    }

    /// Find whichever known peer currently claims to be Controller, if any.
    /// Resolve a user-typed target into a NodeId. Accepts either the full
    /// 16-char hex ID (as shown by to_hex()/PEERS), or a short "peerN"
    /// alias (1-indexed, in the same order PEERS lists them — NodeId
    /// ascending, since peers is a BTreeMap). Aliases aren't stable across
    /// a peer dropping off and rejoining, or another peer appearing with a
    /// lower ID — check PEERS again if a command targeting an alias starts
    /// failing.
    pub fn resolve_target(&self, s: &str) -> Option<NodeId> {
        let s = s.trim();
        let lower = s.to_lowercase();
        if let Some(rest) = lower.strip_prefix("peer") {
            if let Ok(n) = rest.parse::<usize>() {
                if n >= 1 {
                    return self.peers.keys().nth(n - 1).copied();
                }
            }
        }
        NodeId::from_hex(s)
    }

    pub fn find_controller_peer(&self) -> Option<NodeId> {
        self.peers.iter()
            .find(|(_, node)| node.node_type == NodeType::Controller)
            .map(|(id, _)| *id)
    }

    /// Apply a role change to ourselves immediately, with no round-trip —
    /// you don't need anyone else's permission to change your own advertised
    /// role. Only used for self-targeted changes; changing *another* node's
    /// role always goes through the request/confirm flow in
    /// send_cluster_role_request / PendingRoleDialog.
    pub fn set_local_type_immediate(&mut self, new_type: NodeType) {
        self.local_type = new_type;
        self.update_capabilities_for_type();
        self.send_lan_announcement();
    }

    /// Request to become the new Controller. Always self-directed — this is
    /// the *only* way a non-Controller node can affect the Controller role;
    /// it can never be used to reassign some other, third node to Controller.
    /// Sent to whichever peer we currently believe holds the role; that peer
    /// decides whether to concede (see PendingRoleDialog::confirm in
    /// desktop.rs, which also double-checks it's actually still Controller
    /// before ceding, so a stale/duplicate request can't create two).
    pub fn send_promotion_request(&mut self) {
        if self.local_type == NodeType::Controller {
            crate::desktop::notify(crate::desktop::NotifCategory::Info, "MESH", "You are already the Controller.");
            return;
        }
        let controller_id = match self.find_controller_peer() {
            Some(id) => id,
            None => {
                crate::desktop::notify(crate::desktop::NotifCategory::Warning, "MESH", "No Controller found on the mesh to request promotion from.");
                return;
            }
        };
        self.pending_promotion = true;
        self.send_cluster_role_request(&controller_id, NodeType::Controller);
        crate::desktop::notify(crate::desktop::NotifCategory::Info, "MESH", "Requested promotion to Controller.");
    }

    /// Resolve a Controller/Controller collision.
    ///
    /// Rule (as designed): whichever side currently sees the bigger mesh
    /// (itself + its known peers) stays Controller. If both sides report
    /// the same mesh size, it's broken with a coin toss.
    ///
    /// The coin toss has to be deterministic and symmetric or it doesn't
    /// work: if each side just rolled its own independent random bit,
    /// both could roll "I win" (or both "I lose") and you're back to a
    /// split brain. Instead both sides derive the *same* seed by XORing
    /// the two node IDs together (XOR is commutative, so A^B == B^A),
    /// run the same PRNG on it, and use the result to pick the winning
    /// ID out of {local_id, peer_id} — so both sides independently land
    /// on the exact same winner without any extra round-trip.
    ///
    /// The losing side is demoted to Tenant and stays there — this
    /// function never auto-promotes. Re-promotion is a manual action via
    /// the mesh manager UI (SET CONTROLLER), per the design.
    fn resolve_controller_conflict(&mut self, peer_id: NodeId, peer_type: NodeType, peer_mesh_size: u32) {
        // Only relevant if we're currently claiming Controller and just
        // heard from another node that also claims Controller.
        if self.local_type != NodeType::Controller || peer_type != NodeType::Controller {
            return;
        }

        let local_size = self.local_mesh_size();

        let we_win = match local_size.cmp(&peer_mesh_size) {
            core::cmp::Ordering::Greater => true,
            core::cmp::Ordering::Less    => false,
            core::cmp::Ordering::Equal   => {
                let mut seed: u64 = 0;
                for i in 0..8 {
                    seed = (seed << 8) | (self.local_id.0[i] ^ peer_id.0[i]) as u64;
                }
                let coin_favors_higher_id = crate::stability::Rng::new(seed).next_bool();
                let winner = if coin_favors_higher_id {
                    self.local_id.max(peer_id)
                } else {
                    self.local_id.min(peer_id)
                };
                winner == self.local_id
            }
        };

        if we_win {
            crate::serial_println!(
                "[DIST] Controller conflict with {}: keeping Controller (local mesh={}, peer mesh={})",
                peer_id.to_hex(), local_size, peer_mesh_size
            );
        } else {
            self.local_type = NodeType::Tenant;
            crate::serial_println!(
                "[DIST] Controller conflict with {}: demoted self to Tenant (local mesh={}, peer mesh={})",
                peer_id.to_hex(), local_size, peer_mesh_size
            );
            crate::desktop::notify(
                crate::desktop::NotifCategory::Warning,
                "MESH",
                "Controller conflict — this node demoted to Tenant. Promote it manually if you want it in charge.",
            );
            // Let peers know about the new role immediately instead of
            // waiting for the next 5s announcement tick.
            self.send_lan_announcement();
        }
    }

    /// Return the persistent announcement socket handle, creating it if
    /// it doesn't exist yet.  The socket is bound to ANNOUNCE_PORT (7706)
    /// and is never closed so smoltcp can flush queued packets.
    fn ensure_announce_socket(&mut self) -> Option<smoltcp::iface::SocketHandle> {
        if let Some(h) = self.announce_handle {
            return Some(h);
        }
        if let Some(ref mut stack) = *NETWORK.lock() {
            if let Ok(handle) = stack.open_udp(0) {
                if stack.udp_bind(handle, ANNOUNCE_PORT).is_ok() {
                    self.announce_handle = Some(handle);
                    return Some(handle);
                }
                stack.close_socket(handle);
            }
        }
        None
    }

    pub fn set_relay(&mut self, server_ip: Ipv4Address, port: u16) {
        self.relay_config = Some(RelayConfig { server_ip, server_port: port });
        println!("[DIST] WAN relay configured: {}:{}", server_ip, port);
    }

    /// Periodic tick: send LAN broadcast, expire dead peers, maintain relay.
    pub fn tick(&mut self, current_tick: u64) {
        // Fallback MAC update — normally network::init() has already set
        // this deterministically before the mesh ever sends a packet, so
        // this is a no-op. Kept as a safety net in case init() ran before
        // the NIC was probed for any reason.
        if !self.mac_updated && is_ready() {
            if let Some(ref stack) = *crate::network::NETWORK.lock() {
                let mac = stack.device.mac.0;
                self.update_mac(&mac);
            }
        }

        // Expire stale peers.
        self.peers.retain(|_, node| node.is_alive(current_tick));

        // Note: DFS/exec servicing is NOT done here anymore — it has to
        // happen outside the NODE_MESH lock this method is called under,
        // since a serviced command (e.g. DIAG) can itself need to lock
        // NODE_MESH. See service_mesh_once(), called separately by
        // whatever calls this tick() method.

        // Update capabilities based on node type (resource advertisement)
        self.update_capabilities_for_type();

        // Send LAN announcement broadcast.
        if current_tick.wrapping_sub(self.last_announce) >= ANNOUNCE_INTERVAL {
            self.send_lan_announcement();
            self.last_announce = current_tick;
        }

        // Maintain WAN relay connection.
        if let Some(ref cfg) = self.relay_config {
            if !self.relay_connected {
                self.connect_relay(cfg.server_ip, cfg.server_port);
            }
        }

        // Poll incoming UDP discovery packets on DISCOVERY_PORT (7700).
        // QEMU socket-mode tunnels raw Ethernet frames between VMs at Layer 2.
        // The guest's smoltcp stack sees normal IP traffic — discovery packets
        // addressed to 10.0.2.255:DISCOVERY_PORT arrive like any other UDP datagram.
        let mut packets = alloc::vec::Vec::new();
        if let Some(ref mut stack) = *crate::network::NETWORK.lock() {
            // Create a single listener socket on DISCOVERY_PORT
            if self.udp_listen_handle.is_none() && self.sockets_initialized {
                match stack.open_udp(0) {
                    Ok(handle) => {
                        match stack.udp_bind(handle, DISCOVERY_PORT) {
                            Ok(()) => {
                                self.udp_listen_handle = Some(handle);
                                crate::serial_println!("[DIST] Listening for peer discovery packets on port {}", DISCOVERY_PORT);
                            }
                            Err(e) => {
                                crate::serial_println!("[DIST] Failed to bind discovery listener: {:?}", e);
                                stack.close_socket(handle);
                            }
                        }
                    }
                    Err(e) => {
                        crate::serial_println!("[DIST] Failed to open UDP socket for discovery: {:?}", e);
                    }
                }
            }

            if let Some(handle) = self.udp_listen_handle {
                let mut buf = [0u8; 2048];
                loop {
                    match stack.udp_recv(handle, &mut buf) {
                        Ok((len, endpoint)) => {
                            if let smoltcp::wire::IpAddress::Ipv4(ip) = endpoint.addr {
                                crate::serial_println!("[DIST] Received {} bytes from {}", len, ip);
                                packets.push((buf[..len].to_vec(), ip));
                            }
                        }
                        Err(_) => break,
                    }
                }
            }
        }

            for (pkt, ip) in packets {
                crate::serial_println!("[DIST] TICK processing incoming packet from {} ({} bytes)", ip, pkt.len());
                if pkt.len() >= 12 {
                    let magic = u32::from_le_bytes(pkt[0..4].try_into().unwrap_or([0;4]));
                    let length = u32::from_le_bytes(pkt[4..8].try_into().unwrap_or([0;4]));
                    let msg_type = u32::from_le_bytes(pkt[8..12].try_into().unwrap_or([0;4]));
                    crate::serial_println!("[DIST]   magic={:#x} len={} type={:#x}", magic, length, msg_type);
                }
                self.handle_udp_packet(&pkt, ip);
            }
    }

    /// Accept and service one pending DFS or exec request per tick, on each
    /// of the two listening sockets. Each socket is single-connection: after
    /// handling a request it's torn down and reopened into Listen state so
    /// the next caller can connect. That's plenty of throughput for the
    /// occasional remote file/exec request this mesh handles.
    /// Services listener bookkeeping (open/reopen, flush-then-close,
    /// stuck-connection recovery) and, if a request has fully arrived,
    /// reads and returns its raw bytes — but does NOT process it. Processing
    /// (running it through DistributedFs::handle_request / handle_exec_request)
    /// must happen *after* the caller drops the NODE_MESH lock this method
    /// was called under, since exec'd commands can themselves need to lock
    /// NODE_MESH (e.g. DIAG, PEERS) — doing that here, still inside this
    /// method's implicit lock, deadlocks the whole node solid (this is
    /// exactly what happened before this was split out: the log just stops
    /// dead mid-request, because tick() itself never returns again). Use
    /// `service_mesh_once()` rather than calling this directly.
    fn poll_dfs_exec_servers(&mut self) -> Option<(bool, smoltcp::iface::SocketHandle, Vec<u8>)> {
        if !is_ready() { return None; }
        let mut net = NETWORK.lock();
        let stack = net.as_mut()?;
        {
            // Finish closing a connection whose response has fully drained
            // out of the send buffer. Checked first, before anything else,
            // so a listener that's just waiting to flush isn't mistaken
            // for one that needs (re)opening.
            if self.dfs_awaiting_close {
                if let Some(h) = self.dfs_listen_handle {
                    if stack.tcp_send_queue_len(h) == 0 || !stack.tcp_is_active(h) {
                        crate::serial_println!("[DIST/DFS] Response flushed, closing connection");
                        stack.close_socket(h);
                        self.dfs_listen_handle = None;
                        self.dfs_awaiting_close = false;
                        self.dfs_conn_started_at = None;
                    }
                } else {
                    self.dfs_awaiting_close = false;
                }
            }
            if self.exec_awaiting_close {
                if let Some(h) = self.exec_listen_handle {
                    if stack.tcp_send_queue_len(h) == 0 || !stack.tcp_is_active(h) {
                        crate::serial_println!("[DIST/EXEC] Response flushed, closing connection");
                        stack.close_socket(h);
                        self.exec_listen_handle = None;
                        self.exec_awaiting_close = false;
                        self.exec_conn_started_at = None;
                    }
                } else {
                    self.exec_awaiting_close = false;
                }
            }

            // (Re)open the DFS listener if needed.
            if self.dfs_listen_handle.is_none() {
                match stack.open_tcp(0) {
                    Ok(h) => {
                        match stack.tcp_listen(h, DFS_PORT) {
                            Ok(()) => {
                                crate::serial_println!("[DIST/DFS] Listening on port {} (handle={:?})", DFS_PORT, h);
                                self.dfs_listen_handle = Some(h);
                                self.dfs_conn_started_at = None;
                            }
                            Err(e) => {
                                if !self.dfs_listen_warned {
                                    crate::serial_println!("[DIST/DFS] tcp_listen on port {} failed: {:?}", DFS_PORT, e);
                                    self.dfs_listen_warned = true;
                                }
                                stack.close_socket(h);
                            }
                        }
                    }
                    Err(e) => {
                        if !self.dfs_listen_warned {
                            crate::serial_println!("[DIST/DFS] open_tcp failed: {:?}", e);
                            self.dfs_listen_warned = true;
                        }
                    }
                }
            }
            // (Re)open the exec listener if needed.
            if self.exec_listen_handle.is_none() {
                match stack.open_tcp(0) {
                    Ok(h) => {
                        match stack.tcp_listen(h, EXEC_PORT) {
                            Ok(()) => {
                                crate::serial_println!("[DIST/EXEC] Listening on port {} (handle={:?})", EXEC_PORT, h);
                                self.exec_listen_handle = Some(h);
                                self.exec_conn_started_at = None;
                            }
                            Err(e) => {
                                if !self.exec_listen_warned {
                                    crate::serial_println!("[DIST/EXEC] tcp_listen on port {} failed: {:?}", EXEC_PORT, e);
                                    self.exec_listen_warned = true;
                                }
                                stack.close_socket(h);
                            }
                        }
                    }
                    Err(e) => {
                        if !self.exec_listen_warned {
                            crate::serial_println!("[DIST/EXEC] open_tcp failed: {:?}", e);
                            self.exec_listen_warned = true;
                        }
                    }
                }
            }

            // Periodic raw-state snapshot (rate-limited — this function can
            // be called extremely often from inside the client wait loops,
            // not just once per tick) so we can see whether an incoming
            // connection ever actually reaches/progresses either listener,
            // even if it never produces a full request.
            let now = crate::interrupts::TICKS.load(Ordering::Relaxed);
            if now.saturating_sub(self.last_listener_state_log) > 500 {
                self.last_listener_state_log = now;
                if let Some(h) = self.dfs_listen_handle {
                    crate::serial_println!("[DIST/DFS] listener (handle={:?}) state: {:?} remote: {:?}", h, stack.tcp_state(h), stack.tcp_remote_endpoint(h));
                }
                if let Some(h) = self.exec_listen_handle {
                    crate::serial_println!("[DIST/EXEC] listener (handle={:?}) state: {:?} remote: {:?}", h, stack.tcp_state(h), stack.tcp_remote_endpoint(h));
                }
            }

            // If a connection has been sitting non-Listen (i.e. it was
            // accepted) for too long without us successfully servicing a
            // request on it, force it closed and reopen — otherwise a peer
            // that connects and then stalls/aborts can permanently wedge
            // that listener for every future caller. Tracked from the
            // moment the connection actually left Listen, not from when
            // the listener socket was originally created — a listener can
            // legitimately sit idle in Listen for a long time before a
            // perfectly normal connection arrives, and that shouldn't count
            // against it.
            const STUCK_LISTENER_TIMEOUT_MS: u64 = 15_000;
            if let Some(h) = self.dfs_listen_handle {
                let state = stack.tcp_state(h);
                if state == smoltcp::socket::tcp::State::Listen {
                    self.dfs_conn_started_at = None;
                } else {
                    let started = *self.dfs_conn_started_at.get_or_insert(now);
                    if now.saturating_sub(started) > STUCK_LISTENER_TIMEOUT_MS {
                        crate::serial_println!("[DIST/DFS] Connection stuck in {:?} for >{}ms, forcing reset", state, STUCK_LISTENER_TIMEOUT_MS);
                        stack.close_socket(h);
                        self.dfs_listen_handle = None;
                        self.dfs_conn_started_at = None;
                    }
                }
            }
            if let Some(h) = self.exec_listen_handle {
                let state = stack.tcp_state(h);
                if state == smoltcp::socket::tcp::State::Listen {
                    self.exec_conn_started_at = None;
                } else {
                    let started = *self.exec_conn_started_at.get_or_insert(now);
                    if now.saturating_sub(started) > STUCK_LISTENER_TIMEOUT_MS {
                        crate::serial_println!("[DIST/EXEC] Connection stuck in {:?} for >{}ms, forcing reset", state, STUCK_LISTENER_TIMEOUT_MS);
                        stack.close_socket(h);
                        self.exec_listen_handle = None;
                        self.exec_conn_started_at = None;
                    }
                }
            }

            // Read a pending DFS request, if any — return it for the caller
            // to process outside our lock. Gate on Established (see notes
            // on the equivalent exec branch below; same reasoning applies).
            if let Some(h) = self.dfs_listen_handle {
                if !self.dfs_awaiting_close && stack.tcp_state(h) == smoltcp::socket::tcp::State::Established {
                    let mut buf = vec![0u8; 65536];
                    if let Ok(n) = stack.tcp_recv(h, &mut buf) {
                        if n > 0 {
                            crate::serial_println!("[DIST/DFS] Read request ({} bytes), handing off for processing", n);
                            buf.truncate(n);
                            return Some((true, h, buf));
                        }
                    }
                }
            }

            // Read a pending exec request, if any. (Same reasoning. Gating
            // on Established rather than tcp_can_recv() — unreliable right
            // after Listen->Established — or an unconditional recv, which
            // was found to interfere with the socket's own handshake
            // processing when hammered from the client busy-wait loops.)
            if let Some(h) = self.exec_listen_handle {
                if !self.exec_awaiting_close && stack.tcp_state(h) == smoltcp::socket::tcp::State::Established {
                    let mut buf = vec![0u8; 65536];
                    if let Ok(n) = stack.tcp_recv(h, &mut buf) {
                        if n > 0 {
                            crate::serial_println!("[DIST/EXEC] Read request ({} bytes), handing off for processing", n);
                            buf.truncate(n);
                            return Some((false, h, buf));
                        }
                    }
                }
            }
        }
        None
    }

    /// Queue a response for a request previously returned by
    /// poll_dfs_exec_servers(), and mark that connection to be closed once
    /// the response has actually finished draining out.
    pub fn deliver_response(&mut self, is_dfs: bool, handle: smoltcp::iface::SocketHandle, resp: &[u8]) {
        if let Some(ref mut stack) = *NETWORK.lock() {
            let _ = stack.tcp_send(handle, resp);
        }
        if is_dfs { self.dfs_awaiting_close = true; } else { self.exec_awaiting_close = true; }
    }

    pub fn send_lan_announcement(&mut self) {
        if !is_ready() { return; }
        
        let payload  = encode_announcement(&self.local_id, self.local_type, self.local_caps, &self.hostname, self.local_mesh_size());
        let msg      = encode_message(MessageType::NodeAnnouncement, &payload);

        if let Some(ref mut stack) = *NETWORK.lock() {
            // Ensure we have a socket handle
            let handle = match self.ensure_announce_socket_locked(stack) {
                Some(h) => h,
                None => return,
            };

            // Strategy 1: Send to each known peer directly (most reliable)
            for (_id, peer) in self.peers.iter() {
                let dst = IpEndpoint::new(IpAddress::Ipv4(peer.ip), DISCOVERY_PORT);
                match stack.udp_send(handle, &msg, dst) {
                    Ok(()) => crate::serial_println!("[DIST] Sent announcement to peer at {}", peer.ip),
                    Err(_) => crate::serial_println!("[DIST] FAILED to send announcement to peer at {} (TX buffer full?)", peer.ip),
                }
            }

            // Strategy 2: Subnet broadcast, for discovering new peers.
            // On a raw socket netdev (point-to-point Ethernet-over-UDP, as used
            // by run_dual.ps1) broadcast is delivered natively — there's no NAT
            // or SLIRP layer to defeat it, so a blind per-address unicast sweep
            // (previously "Strategy 4", 19 sends to 10.0.2.2..20 every single
            // call) is unnecessary. It was also actively harmful: those extra
            // sends went to addresses with no real host behind them, could never
            // resolve via ARP, and permanently occupied slots in the UDP
            // socket's fixed 16-slot TX buffer (see open_udp() in network.rs) —
            // wedging it solid after the first tick or two and silently
            // dropping every future send, including the ones to the real peer.
            let subnet_bcast = IpEndpoint::new(
                IpAddress::Ipv4(Ipv4Address::new(10, 0, 2, 255)),
                DISCOVERY_PORT,
            );
            if stack.udp_send(handle, &msg, subnet_bcast).is_err() {
                crate::serial_println!("[DIST] FAILED to send subnet broadcast (TX buffer full?)");
            }

            // Strategy 3: Limited broadcast, as a fallback discovery path.
            let limited_bcast = IpEndpoint::new(
                IpAddress::Ipv4(Ipv4Address::BROADCAST),
                DISCOVERY_PORT,
            );
            if stack.udp_send(handle, &msg, limited_bcast).is_err() {
                crate::serial_println!("[DIST] FAILED to send limited broadcast (TX buffer full?)");
            }
        }
    }

    /// Return the persistent announcement socket handle, creating it if needed.
    /// This version takes an already-locked stack reference to avoid deadlock.
    fn ensure_announce_socket_locked(&mut self, stack: &mut NetworkStack) -> Option<smoltcp::iface::SocketHandle> {
        if let Some(h) = self.announce_handle {
            return Some(h);
        }
        if let Ok(handle) = stack.open_udp(0) {
            if stack.udp_bind(handle, ANNOUNCE_PORT).is_ok() {
                self.announce_handle = Some(handle);
                return Some(handle);
            }
            stack.close_socket(handle);
        }
        None
    }

    /// Process an incoming UDP packet on DISCOVERY_PORT.
    pub fn handle_udp_packet(&mut self, data: &[u8], src: Ipv4Address) {
        // Debug: show raw packet header for unrecognized packets
        if data.len() < 12 || u32::from_le_bytes(data[0..4].try_into().unwrap_or([0;4])) != WIRE_MAGIC {
            crate::serial_println!("[DIST] Dropping packet from {}: len={} magic={:#x}",
                src, data.len(), if data.len() >= 4 { u32::from_le_bytes(data[0..4].try_into().unwrap_or([0;4])) } else { 0 });
            return;
        }
        let (msg_type, payload) = match decode_message(data) {
            Some(m) => m,
            None    => return,
        };

        match msg_type {
            MessageType::NodeAnnouncement => {
                if let Some(node) = decode_announcement(payload, src) {
                    if node.id != self.local_id {
                        let is_new = !self.peers.contains_key(&node.id);
                        if is_new {
                            println!("[DIST] New peer: {} ({}) caps={:#b}",
                                node.hostname, src, node.capabilities);
                            // Send hello reply (will lock NETWORK separately)
                            self.send_hello(src);
                        }
                        let (peer_id, peer_type, peer_mesh_size) = (node.id, node.node_type, node.mesh_size);
                        self.peers.insert(node.id, node);
                        self.resolve_controller_conflict(peer_id, peer_type, peer_mesh_size);
                    }
                }
            }
            MessageType::NodeHello => {
                if let Some(node) = decode_announcement(payload, src) {
                    if node.id != self.local_id {
                        let (peer_id, peer_type, peer_mesh_size) = (node.id, node.node_type, node.mesh_size);
                        self.peers.insert(node.id, node);
                        self.resolve_controller_conflict(peer_id, peer_type, peer_mesh_size);
                    }
                }
            }
            MessageType::NodeBye => {
                // Payload is just the 8-byte NodeId.
                if payload.len() >= 8 {
                    let id_bytes: [u8; 8] = payload[0..8].try_into().unwrap_or([0;8]);
                    self.peers.remove(&NodeId(id_bytes));
                }
            }
            MessageType::ClusterSearchRequest => {
                crate::serial_println!("[DIST] RECV ClusterSearchRequest from {}", src);
                if let Some(node) = decode_announcement(payload, src) {
                    if node.id != self.local_id {
                        let (peer_id, peer_type, peer_mesh_size) = (node.id, node.node_type, node.mesh_size);
                        self.peers.insert(node.id, node);
                        self.resolve_controller_conflict(peer_id, peer_type, peer_mesh_size);
                    }
                }
                // Always acknowledge a cluster search to build the mesh
                self.send_resource_offer(src);
            }
            MessageType::ClusterResourceOffer => {
                crate::serial_println!("[DIST] RECV ClusterResourceOffer from {}", src);
                println!("[DIST] Received cluster resource offer from {}", src);
                if let Some(node) = decode_announcement(payload, src) {
                    if node.id != self.local_id {
                        self.peers.insert(node.id, node.clone());
                        crate::desktop::notify(crate::desktop::NotifCategory::Success, "MESH", "New peer discovered!");
                        crate::serial_println!("[DIST] New peer discovered! {:?}", node);
                        self.resolve_controller_conflict(node.id, node.node_type, node.mesh_size);
                    }
                }
            }
            MessageType::ResourceSurrender => {
                println!("[DIST] Peer {} surrendered resources to the cluster", src);
            }
            MessageType::ClusterDisconnect => {
                println!("[DIST] Received remote cluster disconnect from {}", src);
                // Real implementation would gracefully yield cluster tasks here
            }
            MessageType::ClusterCapUpdate => {
                if payload.len() >= 4 {
                    let caps = u32::from_le_bytes(payload[0..4].try_into().unwrap_or([0;4]));
                    println!("[DIST] Received cluster capability update from {}: {:#010x}", src, caps);
                    self.local_caps = caps;
                }
            }
            MessageType::ClusterTypeUpdate => {
                if payload.len() >= 1 {
                    let new_type = NodeType::from_u8(payload[0]);
                    println!("[DIST] Received cluster type update from {}: {:?}", src, new_type);
                    self.local_type = new_type;
                    crate::desktop::notify(crate::desktop::NotifCategory::Warning, "MESH", "Node type updated remotely");
                    // Send an announcement to broadcast new type
                    self.send_lan_announcement();
                }
            }
            MessageType::ClusterRoleRequest => {
                // Payload: [requestor_id(8)] + [proposed_type(1)]
                if payload.len() >= 9 {
                    let req_id: [u8; 8] = payload[0..8].try_into().unwrap_or([0;8]);
                    let requestor = NodeId(req_id);
                    let proposed = NodeType::from_u8(payload[8]);
                    println!("[DIST] Role request from {}: {:?}", requestor.to_hex(), proposed);

                    // Authorization: a request to reassign *someone else's*
                    // role (proposed != Controller, i.e. "you become X") is
                    // only honored if it comes from whoever we currently
                    // recognize as Controller — otherwise any node could
                    // command any other node's role. A promotion request
                    // (proposed == Controller) is always shown, since anyone
                    // is allowed to *ask* to become Controller; whether it's
                    // actually granted is checked again on confirm (see
                    // PendingRoleDialog::confirm), which refuses unless we
                    // genuinely are the Controller right now.
                    let is_promotion = proposed == NodeType::Controller;
                    let sender_is_controller = self.peers.get(&requestor)
                        .map(|p| p.node_type == NodeType::Controller)
                        .unwrap_or(false);
                    if !is_promotion && !sender_is_controller {
                        crate::serial_println!(
                            "[DIST] Ignoring role request from {} (not the Controller, proposed non-promotion change)",
                            requestor.to_hex()
                        );
                        return;
                    }

                    crate::desktop::PENDING_ROLE_DIALOG.lock().show_request(requestor, src, proposed);
                }
            }
            MessageType::ClusterRoleResponse => {
                // Payload: [requestor_id(8)] + [accepted(bool)]
                if payload.len() >= 9 {
                    let req_id: NodeId = NodeId(payload[0..8].try_into().unwrap_or([0;8]));
                    let accepted = payload[8] != 0;

                    // If this response is to a promotion request *we* sent,
                    // handle it directly here rather than through the UI
                    // dialog state (we are the requester, not the confirmer,
                    // so we never had a dialog open for this in the first
                    // place).
                    if self.pending_promotion && req_id == self.local_id {
                        self.pending_promotion = false;
                        if accepted {
                            println!("[DIST] Promotion accepted by {} — becoming Controller", src);
                            self.local_type = NodeType::Controller;
                            self.update_capabilities_for_type();
                            self.send_lan_announcement();
                            crate::desktop::notify(crate::desktop::NotifCategory::Success, "MESH", "You are now the Controller.");
                        } else {
                            println!("[DIST] Promotion denied by {}", src);
                            crate::desktop::notify(crate::desktop::NotifCategory::Warning, "MESH", "Promotion request denied.");
                        }
                        return;
                    }

                    if accepted {
                        println!("[DIST] Role request accepted by node {}", src);
                        crate::desktop::PENDING_ROLE_DIALOG.lock().confirm_by_peer(&req_id);
                    } else {
                        println!("[DIST] Role request denied by node {}", src);
                        crate::desktop::PENDING_ROLE_DIALOG.lock().deny_by_peer(&req_id);
                    }
                }
            }
            _ => {}
        }
    }

    fn send_hello(&mut self, dst_ip: Ipv4Address) {
        let payload = encode_announcement(&self.local_id, self.local_type, self.local_caps, &self.hostname, self.local_mesh_size());
        let msg     = encode_message(MessageType::NodeHello, &payload);
        let dst     = IpEndpoint::new(IpAddress::Ipv4(dst_ip), DISCOVERY_PORT);
        let handle = match self.ensure_announce_socket() {
            Some(h) => h,
            None => return,
        };
        if let Some(ref mut stack) = *NETWORK.lock() {
            let _ = stack.udp_send(handle, &msg, dst);
        }
    }

    fn connect_relay(&mut self, ip: Ipv4Address, port: u16) {
        let payload = encode_relay_register(&self.local_id, self.local_type, self.local_caps, &self.hostname, self.local_mesh_size());
        let msg     = encode_message(MessageType::RelayRegister, &payload);
        let remote  = IpEndpoint::new(IpAddress::Ipv4(ip), port);

        let mut net = NETWORK.lock();
        let stack = match net.as_mut() { Some(s) => s, None => return };

        if let Ok(handle) = stack.open_tcp(0) {
            if stack.tcp_connect(handle, remote, next_ephemeral_port()).is_ok() {
                let _ = stack.tcp_send(handle, &msg);
                stack.close_socket(handle);
                self.relay_connected = true;
                println!("[DIST] Registered with WAN relay {}:{}", ip, port);
            } else {
                stack.close_socket(handle);
            }
        }
    }

    /// Broadcast a search request across the local mesh network to find available resources.
    pub fn search_cluster(&self) {
        if !is_ready() { return; }
        let payload = encode_announcement(&self.local_id, self.local_type, self.local_caps, &self.hostname, self.local_mesh_size());
        let msg = encode_message(MessageType::ClusterSearchRequest, &payload);

        let local_ip = crate::network::NETWORK_CONFIG.lock().ip.address();

        if let Some(ref mut stack) = *NETWORK.lock() {
            if let Ok(handle) = stack.open_udp(0) {
                let _ = stack.udp_bind(handle, 0);

                // Subnet broadcast.
                let subnet_bcast = IpEndpoint::new(
                    IpAddress::Ipv4(Ipv4Address::new(10, 0, 2, 255)),
                    DISCOVERY_PORT,
                );
                let _ = stack.udp_send(handle, &msg, subnet_bcast);

                // Directed unicast sweep.
                for last_octet in 2u8..=20 {
                    let ip = Ipv4Address::new(10, 0, 2, last_octet);
                    if ip == local_ip { continue; }
                    let dst = IpEndpoint::new(IpAddress::Ipv4(ip), DISCOVERY_PORT);
                    let _ = stack.udp_send(handle, &msg, dst);
                }

                stack.close_socket(handle);
            }
        }
        println!("[DIST] Searching cluster for available resources...");
    }

    /// Announce the surrender of resources to the local mesh network.
    pub fn surrender_resources(&self) {
        if !is_ready() { return; }
        let payload = encode_announcement(&self.local_id, self.local_type, self.local_caps, &self.hostname, self.local_mesh_size());
        let msg = encode_message(MessageType::ResourceSurrender, &payload);
        let bcast = IpEndpoint::new(IpAddress::Ipv4(Ipv4Address::BROADCAST), DISCOVERY_PORT);
        if let Some(ref mut stack) = *NETWORK.lock() {
            if let Ok(handle) = stack.open_udp(0) {
                let _ = stack.udp_bind(handle, 0);
                let _ = stack.udp_send(handle, &msg, bcast);
                stack.close_socket(handle);
            }
        }
        println!("[DIST] Surrendered local resources to the cluster.");
    }

    fn send_resource_offer(&mut self, dst_ip: Ipv4Address) {
        let payload = encode_announcement(&self.local_id, self.local_type, self.local_caps, &self.hostname, self.local_mesh_size());
        let msg = encode_message(MessageType::ClusterResourceOffer, &payload);
        let dst = IpEndpoint::new(IpAddress::Ipv4(dst_ip), DISCOVERY_PORT);
        let handle = match self.ensure_announce_socket() {
            Some(h) => h,
            None => return,
        };
        if let Some(ref mut stack) = *NETWORK.lock() {
            let _ = stack.udp_send(handle, &msg, dst);
        }
    }

    pub fn send_cluster_disconnect(&mut self, target: &NodeId) {
        let (msg, dst) = match self.peers.get(target) {
            Some(node) => (
                encode_message(MessageType::ClusterDisconnect, &[]),
                IpEndpoint::new(IpAddress::Ipv4(node.ip), DISCOVERY_PORT),
            ),
            None => return,
        };
        let handle = match self.ensure_announce_socket() {
            Some(h) => h,
            None => return,
        };
        if let Some(ref mut stack) = *NETWORK.lock() {
            let _ = stack.udp_send(handle, &msg, dst);
        }
        println!("[DIST] Sent cluster disconnect to node {}", target.to_hex());
    }

    pub fn send_cluster_type_update(&mut self, target: &NodeId, new_type: NodeType) {
        let (msg, dst) = match self.peers.get(target) {
            Some(node) => (
                encode_message(MessageType::ClusterTypeUpdate, &[new_type as u8]),
                IpEndpoint::new(IpAddress::Ipv4(node.ip), DISCOVERY_PORT),
            ),
            None => return,
        };
        let handle = match self.ensure_announce_socket() {
            Some(h) => h,
            None => return,
        };
        if let Some(ref mut stack) = *NETWORK.lock() {
            let _ = stack.udp_send(handle, &msg, dst);
        }
        println!("[DIST] Sent cluster type update to node {}", target.to_hex());
    }

    pub fn send_cluster_cap_update(&mut self, target: &NodeId, new_caps: u32) {
        let (msg, dst) = match self.peers.get(target) {
            Some(node) => (
                encode_message(MessageType::ClusterCapUpdate, &new_caps.to_le_bytes()),
                IpEndpoint::new(IpAddress::Ipv4(node.ip), DISCOVERY_PORT),
            ),
            None => return,
        };
        let handle = match self.ensure_announce_socket() {
            Some(h) => h,
            None => return,
        };
        if let Some(ref mut stack) = *NETWORK.lock() {
            let _ = stack.udp_send(handle, &msg, dst);
        }
        println!("[DIST] Sent cluster capability update to node {}", target.to_hex());
    }

    /// Send a role request to a target node (initiator side).
    /// Payload: [local_id(8)] + [proposed_type(1)]
    pub fn send_cluster_role_request(&mut self, target: &NodeId, proposed_type: NodeType) {
        let (msg, dst) = match self.peers.get(target) {
            Some(node) => {
                let mut payload = Vec::with_capacity(9);
                payload.extend_from_slice(self.local_id.as_bytes());
                payload.push(proposed_type as u8);
                (encode_message(MessageType::ClusterRoleRequest, &payload),
                 IpEndpoint::new(IpAddress::Ipv4(node.ip), DISCOVERY_PORT))
            },
            None => return,
        };
        crate::serial_println!("[DIST] Sending role request to {} at {}", target.to_hex(), dst);
        if let Some(ref mut stack) = *NETWORK.lock() {
            let handle = match self.ensure_announce_socket_locked(stack) {
                Some(h) => h,
                None => return,
            };
            if stack.udp_send(handle, &msg, dst).is_err() {
                crate::serial_println!("[DIST] Failed to send role request");
            } else {
                crate::serial_println!("[DIST] Role request sent successfully to {}", target.to_hex());
            }
        }
        println!("[DIST] Sent role request to node {}: {:?}", target.to_hex(), proposed_type);
    }

    /// Send a role response to an initiator (target side).
    /// Payload: [requestor_id(8)] + [accepted(1)]
    pub fn send_cluster_role_response(&mut self, target_ip: Ipv4Address, requestor_id: &NodeId, accepted: bool) {
        let mut payload = Vec::with_capacity(9);
        payload.extend_from_slice(requestor_id.as_bytes());
        payload.push(if accepted { 1 } else { 0 });
        let msg = encode_message(MessageType::ClusterRoleResponse, &payload);
        let dst = IpEndpoint::new(IpAddress::Ipv4(target_ip), DISCOVERY_PORT);
        let handle = match self.ensure_announce_socket() {
            Some(h) => h,
            None => return,
        };
        if let Some(ref mut stack) = *NETWORK.lock() {
            let _ = stack.udp_send(handle, &msg, dst);
        }
        println!("[DIST] Sent role response to {}: {}", target_ip, if accepted { "accepted" } else { "denied" });
    }

    /// Find the first peer with a given capability.
    pub fn find_peer_with(&self, cap: u32) -> Option<&RemoteNode> {
        self.peers.values().find(|n| n.capabilities & cap != 0)
    }

    pub fn peer_count(&self) -> usize { self.peers.len() }

    /// Update capabilities based on node type.
    /// Compute nodes advertise EXEC capability (CPU resources).
    /// Storage nodes advertise DFS capability (storage resources).
    /// Other node types (Controller, Delegate, Offload, Hub) keep all capabilities.
    pub fn update_capabilities_for_type(&mut self) {
        let new_caps = match self.local_type {
            NodeType::Compute => CAP_DFS | CAP_EXEC | CAP_GPU | CAP_RDP | CAP_GAMING,
            NodeType::Storage => CAP_DFS,
            NodeType::Offload => 0,
            // Tenant = lost a Controller election (or was manually demoted).
            // Passive by default; a human can promote it back at any time.
            NodeType::Tenant => 0,
            NodeType::Delegate | NodeType::Controller | NodeType::Hub | NodeType::Unknown => {
                CAP_DFS | CAP_EXEC | CAP_GPU | CAP_RDP | CAP_GAMING
            }
        };
        
        if new_caps != self.local_caps {
            self.local_caps = new_caps;
            println!("[DIST] Updated capabilities for {:?} node: {:#x}", self.local_type, new_caps);
        }
    }
}

// =====================================================================
// -----| Part 7 — Distributed Filesystem |-----
// =====================================================================
//
// Files under "/net/<node_id_hex>/<path>" are served from the remote node
// at that NodeId. Reads and writes are forwarded over TCP.

pub struct DistributedFs;

/// tcp_connect() only *initiates* the handshake; actually completing it (and
/// later, actually receiving a response) requires repeated iface.poll()
/// calls, same as any other network traffic. These synchronous client calls
/// don't have anyone else polling on their behalf while they block, so they
/// have to drive it themselves. Both loops are bounded by a tick-based
/// timeout so a dead/unreachable peer fails cleanly instead of hanging.
const TCP_WAIT_TIMEOUT_MS: u64 = 5000;

fn wait_for_tcp_connect(handle: smoltcp::iface::SocketHandle) -> Result<(), DfsError> {
    let start = crate::interrupts::TICKS.load(Ordering::Relaxed);
    let mut iterations: u32 = 0;
    loop {
        {
            let mut net = NETWORK.lock();
            let stack = net.as_mut().ok_or(DfsError::NetworkError)?;
            stack.poll();
            if stack.tcp_may_send(handle) {
                crate::serial_println!("[DIST/TCP] Connect succeeded after {} poll iterations", iterations);
                return Ok(());
            }
            if !stack.tcp_is_active(handle) {
                crate::serial_println!("[DIST/TCP] Connect failed: socket went inactive after {} poll iterations (connection refused/reset, or nobody listening)", iterations);
                return Err(DfsError::NetworkError);
            }
        }
        // While we're stuck waiting on our own connection, keep answering
        // anyone else's incoming DFS/exec requests — otherwise the whole
        // main loop being frozen inside this busy-wait means we silently
        // stop responding to peers for the entire wait, which is exactly
        // what caused two nodes doing this at once to deadlock each other.
        service_mesh_once();
        iterations += 1;
        let now = crate::interrupts::TICKS.load(Ordering::Relaxed);
        if now.saturating_sub(start) > TCP_WAIT_TIMEOUT_MS {
            crate::serial_println!("[DIST/TCP] Connect timed out after {}ms ({} poll iterations) — socket still active but never reached Established", TCP_WAIT_TIMEOUT_MS, iterations);
            return Err(DfsError::NetworkError);
        }
    }
}

fn wait_for_tcp_data(handle: smoltcp::iface::SocketHandle) -> Result<(), DfsError> {
    let start = crate::interrupts::TICKS.load(Ordering::Relaxed);
    let mut iterations: u32 = 0;
    loop {
        {
            let mut net = NETWORK.lock();
            let stack = net.as_mut().ok_or(DfsError::NetworkError)?;
            stack.poll();
            if stack.tcp_can_recv(handle) {
                crate::serial_println!("[DIST/TCP] Data ready after {} poll iterations", iterations);
                return Ok(());
            }
            if !stack.tcp_is_active(handle) {
                crate::serial_println!("[DIST/TCP] Wait-for-data failed: socket went inactive after {} poll iterations (peer closed without responding?)", iterations);
                return Err(DfsError::NetworkError);
            }
        }
        // Same reason as in wait_for_tcp_connect above — keep servicing
        // incoming requests while we wait for our own.
        service_mesh_once();
        iterations += 1;
        let now = crate::interrupts::TICKS.load(Ordering::Relaxed);
        if now.saturating_sub(start) > TCP_WAIT_TIMEOUT_MS {
            crate::serial_println!("[DIST/TCP] Wait-for-data timed out after {}ms ({} poll iterations)", TCP_WAIT_TIMEOUT_MS, iterations);
            return Err(DfsError::NetworkError);
        }
    }
}

impl DistributedFs {
    /// Parse a DFS path: "/net/<node_hex>/<rest>" → (NodeId, rest_path)
    fn parse_path(path: &str) -> Option<(NodeId, &str)> {
        let path = path.strip_prefix("/net/")?;
        let slash = path.find('/')?;
        let node_hex = &path[..slash];
        let rest     = &path[slash..];

        if node_hex.len() != 16 { return None; }
        let mut id = [0u8; 8];
        for i in 0..8 {
            let byte_str = &node_hex[i*2..i*2+2];
            id[i] = u8::from_str_radix(byte_str, 16).ok()?;
        }
        Some((NodeId(id), rest))
    }

    /// Read a file from a remote node.
    pub fn read_file(path: &str) -> Result<Vec<u8>, DfsError> {
        let (node_id, remote_path) = Self::parse_path(path)
            .ok_or(DfsError::InvalidPath)?;

        let node_ip = NODE_MESH.lock()
            .peers.get(&node_id)
            .map(|n| n.ip)
            .ok_or(DfsError::NodeNotFound)?;

        let path_bytes = remote_path.as_bytes();
        let payload    = path_bytes.to_vec();
        let msg        = encode_message(MessageType::DfsReadRequest, &payload);

        let remote = IpEndpoint::new(IpAddress::Ipv4(node_ip), DFS_PORT);
        let handle = {
            let mut net = NETWORK.lock();
            let stack   = net.as_mut().ok_or(DfsError::NetworkError)?;
            let handle = stack.open_tcp(0).map_err(|_| DfsError::NetworkError)?;
            stack.tcp_connect(handle, remote, next_ephemeral_port()).map_err(|_| DfsError::NetworkError)?;
            handle
        };

        // Wait for the handshake to actually complete before sending.
        if let Err(e) = wait_for_tcp_connect(handle) {
            let mut net = NETWORK.lock();
            if let Some(s) = net.as_mut() { s.close_socket(handle); }
            return Err(e);
        }

        // Send request.
        {
            let mut net = NETWORK.lock();
            let stack = net.as_mut().ok_or(DfsError::NetworkError)?;
            stack.tcp_send(handle, &msg).map_err(|_| DfsError::NetworkError)?;
        }

        // Wait for the response to actually arrive before reading it.
        if let Err(e) = wait_for_tcp_data(handle) {
            let mut net = NETWORK.lock();
            if let Some(s) = net.as_mut() { s.close_socket(handle); }
            return Err(e);
        }

        // Read response (wait for full framed message).
        let mut buf = vec![0u8; 65536];
        let n = {
            let mut net = NETWORK.lock();
            let stack = net.as_mut().ok_or(DfsError::NetworkError)?;
            stack.tcp_recv(handle, &mut buf).map_err(|_| DfsError::NetworkError)?
        };

        {
            let mut net = NETWORK.lock();
            if let Some(s) = net.as_mut() { s.close_socket(handle); }
        }

        let (msg_type, payload) = decode_message(&buf[..n])
            .ok_or(DfsError::ProtocolError)?;

        if msg_type != MessageType::DfsReadResponse {
            return Err(DfsError::ProtocolError);
        }

        // First 4 bytes of payload: status (0 = OK).
        if payload.len() < 4 { return Err(DfsError::ProtocolError); }
        let status = u32::from_le_bytes(payload[0..4].try_into().unwrap());
        if status != 0 { return Err(DfsError::RemoteError(status)); }

        let data = payload[4..].to_vec();
        Ok(data)
    }

    /// Write a file to a remote node.
    pub fn write_file(path: &str, data: &[u8]) -> Result<(), DfsError> {
        let (node_id, remote_path) = Self::parse_path(path)
            .ok_or(DfsError::InvalidPath)?;

        let node_ip = NODE_MESH.lock()
            .peers.get(&node_id)
            .map(|n| n.ip)
            .ok_or(DfsError::NodeNotFound)?;

        let path_bytes = remote_path.as_bytes();
        let mut payload = Vec::with_capacity(2 + path_bytes.len() + data.len());
        payload.extend_from_slice(&(path_bytes.len() as u16).to_le_bytes());
        payload.extend_from_slice(path_bytes);
        payload.extend_from_slice(data);
        let msg = encode_message(MessageType::DfsWriteRequest, &payload);

        let remote = IpEndpoint::new(IpAddress::Ipv4(node_ip), DFS_PORT);
        let handle = {
            let mut net = NETWORK.lock();
            let stack   = net.as_mut().ok_or(DfsError::NetworkError)?;
            let handle  = stack.open_tcp(0).map_err(|_| DfsError::NetworkError)?;
            stack.tcp_connect(handle, remote, next_ephemeral_port()).map_err(|_| DfsError::NetworkError)?;
            handle
        };

        if let Err(e) = wait_for_tcp_connect(handle) {
            let mut net = NETWORK.lock();
            if let Some(s) = net.as_mut() { s.close_socket(handle); }
            return Err(e);
        }

        let result = {
            let mut net = NETWORK.lock();
            let stack = net.as_mut().ok_or(DfsError::NetworkError)?;
            stack.tcp_send(handle, &msg).map_err(|_| DfsError::NetworkError)
        };

        {
            let mut net = NETWORK.lock();
            if let Some(s) = net.as_mut() { s.close_socket(handle); }
        }

        result.map(|_| ())
    }

    /// Handle an incoming DFS request on the server side.
    /// Returns the response bytes to send back.
    pub fn handle_request(data: &[u8]) -> Vec<u8> {
        let (msg_type, payload) = match decode_message(data) {
            Some(m) => m,
            None    => return encode_message(MessageType::DfsReadResponse,
                                             &[1,0,0,0]), // status=1 (error)
        };

        match msg_type {
            MessageType::DfsReadRequest => {
                let path = core::str::from_utf8(payload).unwrap_or("/");
                match crate::vfs::VFS.read_file(path) {
                    Ok(data) => {
                        let mut resp = vec![0u8; 4];
                        resp.extend_from_slice(&data);
                        encode_message(MessageType::DfsReadResponse, &resp)
                    }
                    Err(_) => encode_message(MessageType::DfsReadResponse, &[1,0,0,0]),
                }
            }
            MessageType::DfsWriteRequest => {
                if payload.len() < 2 {
                    return encode_message(MessageType::DfsWriteResponse, &[1,0,0,0]);
                }
                let path_len = u16::from_le_bytes([payload[0], payload[1]]) as usize;
                if payload.len() < 2 + path_len {
                    return encode_message(MessageType::DfsWriteResponse, &[1,0,0,0]);
                }
                let path = core::str::from_utf8(&payload[2..2+path_len]).unwrap_or("/");
                let data = &payload[2+path_len..];
                match crate::vfs::VFS.write_file(path, data) {
                    Ok(_)  => encode_message(MessageType::DfsWriteResponse, &[0,0,0,0]),
                    Err(_) => encode_message(MessageType::DfsWriteResponse, &[1,0,0,0]),
                }
            }
            _ => encode_message(MessageType::DfsReadResponse, &[2,0,0,0]), // unknown
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DfsError {
    InvalidPath,
    NodeNotFound,
    NetworkError,
    ProtocolError,
    RemoteError(u32),
}

// =====================================================================
// -----| Part 8 — Remote Execution |-----
// =====================================================================

/// A task to execute on a remote node.
#[derive(Debug, Clone)]
pub struct RemoteTask {
    /// The command to run (passed to the remote node's terminal executor).
    pub command:    String,
    /// Arguments as a space-separated string.
    pub args:       String,
    /// Maximum execution time in milliseconds (0 = no limit).
    pub timeout_ms: u64,
}

impl RemoteTask {
    pub fn new(command: &str, args: &str) -> Self {
        Self {
            command:    String::from(command),
            args:       String::from(args),
            timeout_ms: 30_000,
        }
    }

    fn serialise(&self) -> Vec<u8> {
        let cmd   = self.command.as_bytes();
        let args  = self.args.as_bytes();
        let mut out = Vec::with_capacity(10 + cmd.len() + args.len());
        out.extend_from_slice(&(cmd.len() as u16).to_le_bytes());
        out.extend_from_slice(cmd);
        out.extend_from_slice(&(args.len() as u16).to_le_bytes());
        out.extend_from_slice(args);
        out.extend_from_slice(&self.timeout_ms.to_le_bytes());
        out
    }

    fn deserialise(data: &[u8]) -> Option<Self> {
        if data.len() < 12 { return None; }
        let cmd_len  = u16::from_le_bytes([data[0], data[1]]) as usize;
        if data.len() < 2 + cmd_len { return None; }
        let command  = String::from(core::str::from_utf8(&data[2..2+cmd_len]).ok()?);
        let offset   = 2 + cmd_len;
        if data.len() < offset + 2 { return None; }
        let args_len = u16::from_le_bytes([data[offset], data[offset+1]]) as usize;
        if data.len() < offset + 2 + args_len + 8 { return None; }
        let args     = String::from(core::str::from_utf8(
                           &data[offset+2..offset+2+args_len]).ok()?);
        let timeout_off = offset + 2 + args_len;
        let timeout_ms  = u64::from_le_bytes(data[timeout_off..timeout_off+8].try_into().ok()?);
        Some(Self { command, args, timeout_ms })
    }
}

/// Submit a task to a remote node. Returns stdout bytes.
pub fn remote_exec(node_id: &NodeId, task: RemoteTask) -> Result<Vec<u8>, DfsError> {
    let node_ip = NODE_MESH.lock()
        .peers.get(node_id)
        .map(|n| n.ip)
        .ok_or(DfsError::NodeNotFound)?;

    let payload = task.serialise();
    let msg     = encode_message(MessageType::ExecRequest, &payload);
    let remote  = IpEndpoint::new(IpAddress::Ipv4(node_ip), EXEC_PORT);

    let handle = {
        let mut net = NETWORK.lock();
        let stack   = net.as_mut().ok_or(DfsError::NetworkError)?;
        let handle  = match stack.open_tcp(0) {
            Ok(h) => h,
            Err(e) => { crate::serial_println!("[DIST/EXEC] open_tcp failed: {:?}", e); return Err(DfsError::NetworkError); }
        };
        if let Err(e) = stack.tcp_connect(handle, remote, next_ephemeral_port()) {
            crate::serial_println!("[DIST/EXEC] tcp_connect to {:?} failed: {:?}", remote, e);
            stack.close_socket(handle);
            return Err(DfsError::NetworkError);
        }
        crate::serial_println!("[DIST/EXEC] tcp_connect to {:?} initiated, handle={:?}", remote, handle);
        handle
    };

    if let Err(e) = wait_for_tcp_connect(handle) {
        let mut net = NETWORK.lock();
        if let Some(s) = net.as_mut() { s.close_socket(handle); }
        return Err(e);
    }

    {
        let mut net = NETWORK.lock();
        let stack = net.as_mut().ok_or(DfsError::NetworkError)?;
        if let Err(e) = stack.tcp_send(handle, &msg) {
            crate::serial_println!("[DIST/EXEC] tcp_send failed: {:?}", e);
            drop(net);
            let mut net = NETWORK.lock();
            if let Some(s) = net.as_mut() { s.close_socket(handle); }
            return Err(DfsError::NetworkError);
        }
        crate::serial_println!("[DIST/EXEC] Sent {} bytes", msg.len());
    }

    // Accumulate stdout chunks until ExecComplete. Our own server currently
    // sends the whole result as a single ExecComplete (no separate
    // ExecStdout chunks), so its payload is captured too, not just skipped —
    // this also keeps it correct against any future server that does chunk.
    let mut stdout = Vec::new();
    loop {
        if let Err(e) = wait_for_tcp_data(handle) {
            let mut net = NETWORK.lock();
            if let Some(s) = net.as_mut() { s.close_socket(handle); }
            return Err(e);
        }

        let mut buf = vec![0u8; 8192];
        let n = {
            let mut net = NETWORK.lock();
            let stack = net.as_mut().ok_or(DfsError::NetworkError)?;
            stack.tcp_recv(handle, &mut buf).map_err(|_| DfsError::NetworkError)?
        };
        if n == 0 { break; }

        if let Some((msg_type, payload)) = decode_message(&buf[..n]) {
            match msg_type {
                MessageType::ExecStdout => stdout.extend_from_slice(payload),
                MessageType::ExecComplete => {
                    stdout.extend_from_slice(payload);
                    break;
                }
                MessageType::ExecError => {
                    let mut net = NETWORK.lock();
                    if let Some(s) = net.as_mut() { s.close_socket(handle); }
                    return Err(DfsError::RemoteError(1));
                }
                _ => {}
            }
        }
    }

    {
        let mut net = NETWORK.lock();
        if let Some(s) = net.as_mut() { s.close_socket(handle); }
    }
    Ok(stdout)
}

/// Handle an incoming remote execution request on the server side.
///
/// This dispatcher handles three message types:
///   ExecRequest (0x20)        — legacy RemoteTask; runs a terminal command.
///   TaskOffloadRequest (0x62) — TaskCapsule; executes a worker-thread step.
///   BatchJobRequest (0x63)    — multiple concatenated TaskCapsules; each is
///                               executed in order and all results returned.
pub fn handle_exec_request(data: &[u8]) -> Vec<u8> {
    let (msg_type, payload) = match decode_message(data) {
        Some(m) => m,
        None    => return encode_message(MessageType::ExecError, b"bad frame"),
    };

    match msg_type {
        // ---- Legacy terminal-command path --------------------------------
        MessageType::ExecRequest => {
            let task = match RemoteTask::deserialise(payload) {
                Some(t) => t,
                None    => return encode_message(MessageType::ExecError, b"bad task"),
            };
            println!("[DIST/EXEC] Remote task: {} {}", task.command, task.args);
            let mut output: Vec<alloc::string::String> = Vec::new();
            let cmd = alloc::format!("{} {}", task.command, task.args).to_uppercase();
            crate::task::terminal::execute_command_pub(&cmd, &mut output);
            let stdout: alloc::string::String = output.join("\n");
            encode_message(MessageType::ExecComplete, stdout.as_bytes())
        }

        // ---- Single TaskCapsule offload ----------------------------------
        MessageType::TaskOffloadRequest => {
            let result = handle_task_capsule(payload);
            encode_message(MessageType::ExecComplete, &result.serialise())
        }

        // ---- Batch of TaskCapsules ---------------------------------------
        MessageType::BatchJobRequest => {
            // Payload layout: [capsule_count: u16] then capsule_count framed
            // capsules, each preceded by [capsule_len: u32].
            if payload.len() < 2 {
                return encode_message(MessageType::ExecError, b"bad batch header");
            }
            let count = u16::from_le_bytes([payload[0], payload[1]]) as usize;
            let mut cursor = 2usize;
            let mut all_results: Vec<u8> = Vec::new();

            // Prepend a u16 result count so the receiver knows how many
            // TaskResult frames follow.
            all_results.extend_from_slice(&(count as u16).to_le_bytes());

            for i in 0..count {
                if payload.len() < cursor + 4 {
                    crate::serial_println!("[DIST/BATCH] Capsule {} missing length prefix", i);
                    break;
                }
                let capsule_len =
                    u32::from_le_bytes(payload[cursor..cursor+4].try_into().unwrap_or([0;4]))
                    as usize;
                cursor += 4;

                if payload.len() < cursor + capsule_len {
                    crate::serial_println!("[DIST/BATCH] Capsule {} truncated", i);
                    break;
                }
                let capsule_bytes = &payload[cursor..cursor + capsule_len];
                cursor += capsule_len;

                let result = handle_task_capsule(capsule_bytes);
                // Each result in the batch stream is length-prefixed.
                let result_bytes = result.serialise();
                all_results.extend_from_slice(&(result_bytes.len() as u32).to_le_bytes());
                all_results.extend_from_slice(&result_bytes);
            }

            encode_message(MessageType::ExecComplete, &all_results)
        }

        _ => encode_message(MessageType::ExecError, b"wrong type"),
    }
}

/// Execute a single serialised TaskCapsule and return a TaskResult.
///
/// Steps:
///   1. Deserialise — reject malformed capsules immediately.
///   2. Capability check — verify the local node advertises everything the
///      capsule requires (using the distributed CAP_* bitmask).
///   3. Asset pre-fetch — pull any `asset_paths` from DFS into the local VFS
///      under a temporary `/tmp/job_<id>/` directory so the executor can read
///      them by path.
///   4. Execute — run the capsule's embedded command via the terminal executor.
///      For native threads the command is encoded in the first 256 bytes of
///      `input_data` as a UTF-8 string; the remainder is passed as stdin data.
///   5. Checkpoint write — for native capsules only, write a step checkpoint
///      via the CheckpointManager so retries can resume here.
///   6. Return a TaskResult with the output or error.
fn handle_task_capsule(payload: &[u8]) -> crate::task_capsule::TaskResult {
    use crate::task_capsule::{TaskCapsule, TaskResult};

    // 1. Deserialise
    let capsule = match TaskCapsule::deserialise(payload) {
        Some(c) => c,
        None    => {
            crate::serial_println!("[DIST/CAPSULE] Failed to deserialise capsule");
            return TaskResult::err(0, 0, "bad capsule payload");
        }
    };

    crate::serial_println!(
        "[DIST/CAPSULE] Received: {}",
        capsule.describe()
    );

    // 2. Capability check — local node must advertise what the capsule needs.
    //    capability_requirements uses the distributed CAP_* bitmask, which is
    //    a u64 but currently only the lower 6 bits (DFS/EXEC/GPU/RDP/RELAY/GAMING)
    //    are defined; treat unrecognised bits as a pass so future additions don't
    //    break backward compatibility.
    let local_caps = NODE_MESH.lock().local_caps as u64;
    let required   = capsule.capability_requirements;
    // Only enforce bits we actually define (lower 6).
    const KNOWN_CAPS_MASK: u64 = 0x3F;
    if (required & KNOWN_CAPS_MASK) & !local_caps != 0 {
        let missing = (required & KNOWN_CAPS_MASK) & !local_caps;
        crate::serial_println!(
            "[DIST/CAPSULE] Capability check failed for job {:#x}: missing {:#010b}",
            capsule.job_id, missing
        );
        return TaskResult::err(
            capsule.job_id, capsule.step_index,
            "remote node lacks required capabilities",
        );
    }

    // 3. Asset pre-fetch — pull DFS assets into /tmp/job_<id>/ on the local VFS.
    if !capsule.asset_paths.is_empty() {
        for asset_path in &capsule.asset_paths {
            match crate::distributed::DistributedFs::read_file(asset_path) {
                Ok(data) => {
                    // Store under /tmp/job_<id>/<filename>
                    let filename = asset_path.split('/').last().unwrap_or("asset");
                    let local_path = alloc::format!(
                        "/tmp/job_{:#x}/{}",
                        capsule.job_id, filename
                    );
                    let _ = crate::vfs::VFS.write_file(&local_path, &data);
                    crate::serial_println!(
                        "[DIST/CAPSULE] Fetched asset {} -> {}",
                        asset_path, local_path
                    );
                }
                Err(e) => {
                    crate::serial_println!(
                        "[DIST/CAPSULE] Asset fetch failed for {}: {:?}",
                        asset_path, e
                    );
                    // Non-fatal: the executor may not need every asset.
                }
            }
        }
    }

    // 4. Execute — the command is the first line of input_data (UTF-8, up to
    //    256 bytes or first NUL/LF, whichever comes first).
    let command_str = if capsule.input_data.is_empty() {
        alloc::string::String::from("ECHO [capsule: no command]")
    } else {
        let end = capsule.input_data.iter()
            .take(256)
            .position(|&b| b == 0 || b == b'\n')
            .unwrap_or(capsule.input_data.len().min(256));
        core::str::from_utf8(&capsule.input_data[..end])
            .unwrap_or("ECHO [bad utf8]")
            .trim()
            .to_uppercase()
            .into()
    };

    crate::serial_println!(
        "[DIST/CAPSULE] Executing job={:#x} step={} cmd={}",
        capsule.job_id, capsule.step_index, command_str
    );

    let mut output: Vec<alloc::string::String> = Vec::new();
    crate::task::terminal::execute_command_pub(&command_str, &mut output);
    let stdout = output.join("\n");

    // 5. Checkpoint — native threads only.
    if capsule.is_native {
        crate::checkpoint::CheckpointManager::write_checkpoint(
            capsule.job_id,
            capsule.step_index,
            stdout.as_bytes(),
        );
    }

    crate::serial_println!(
        "[DIST/CAPSULE] Completed job={:#x} step={} output={}B",
        capsule.job_id, capsule.step_index, stdout.len()
    );

    TaskResult::ok(capsule.job_id, capsule.step_index, stdout.into_bytes())
}

/// Send a TaskCapsule to a remote node and return the TaskResult.
///
/// This is the client-side counterpart of `handle_task_capsule`.  It
/// serialises the capsule into a `TaskOffloadRequest` frame, sends it
/// over TCP to `EXEC_PORT` on the target node, and deserialises the
/// `ExecComplete` response back into a `TaskResult`.
pub fn remote_exec_capsule(
    node_id: &NodeId,
    capsule: &crate::task_capsule::TaskCapsule,
) -> Result<crate::task_capsule::TaskResult, DfsError> {
    use crate::task_capsule::TaskResult;

    let node_ip = NODE_MESH.lock()
        .peers.get(node_id)
        .map(|n| n.ip)
        .ok_or(DfsError::NodeNotFound)?;

    let capsule_bytes = capsule.serialise();
    let msg = encode_message(MessageType::TaskOffloadRequest, &capsule_bytes);
    let remote = IpEndpoint::new(IpAddress::Ipv4(node_ip), EXEC_PORT);

    let handle = {
        let mut net = NETWORK.lock();
        let stack = net.as_mut().ok_or(DfsError::NetworkError)?;
        let h = stack.open_tcp(0).map_err(|_| DfsError::NetworkError)?;
        stack.tcp_connect(h, remote, next_ephemeral_port())
            .map_err(|_| { DfsError::NetworkError })?;
        h
    };

    if let Err(e) = wait_for_tcp_connect(handle) {
        let mut net = NETWORK.lock();
        if let Some(s) = net.as_mut() { s.close_socket(handle); }
        return Err(e);
    }

    {
        let mut net = NETWORK.lock();
        let stack = net.as_mut().ok_or(DfsError::NetworkError)?;
        stack.tcp_send(handle, &msg).map_err(|_| DfsError::NetworkError)?;
    }

    if let Err(e) = wait_for_tcp_data(handle) {
        let mut net = NETWORK.lock();
        if let Some(s) = net.as_mut() { s.close_socket(handle); }
        return Err(e);
    }

    let mut buf = vec![0u8; 65536];
    let n = {
        let mut net = NETWORK.lock();
        let stack = net.as_mut().ok_or(DfsError::NetworkError)?;
        stack.tcp_recv(handle, &mut buf).map_err(|_| DfsError::NetworkError)?
    };
    {
        let mut net = NETWORK.lock();
        if let Some(s) = net.as_mut() { s.close_socket(handle); }
    }

    let (resp_type, resp_payload) = decode_message(&buf[..n])
        .ok_or(DfsError::ProtocolError)?;

    if resp_type == MessageType::ExecError {
        return Err(DfsError::RemoteError(1));
    }
    if resp_type != MessageType::ExecComplete {
        return Err(DfsError::ProtocolError);
    }

    // The ExecComplete payload for a capsule response is a serialised TaskResult.
    TaskResult::deserialise(resp_payload).ok_or(DfsError::ProtocolError)
}

// =====================================================================
// -----| Part 9 — Shared GPU Workloads |-----
// =====================================================================

/// Submit a batch of GPU commands to a remote node for execution.
/// The remote node renders/computes and returns a raw frame buffer.
pub fn remote_gpu_submit(
    node_id: &NodeId,
    commands: &[crate::gpu_driver::GpuCommand],
    width: u32, height: u32,
) -> Result<Vec<u8>, DfsError> {
    let node_ip = NODE_MESH.lock()
        .peers.get(node_id)
        .filter(|n| n.capabilities & CAP_GPU != 0)
        .map(|n| n.ip)
        .ok_or(DfsError::NodeNotFound)?;

    // Serialise commands as (type_u8, param_u64 × 6) per command.
    let mut payload = Vec::with_capacity(4 + commands.len() * 49 + 8);
    payload.extend_from_slice(&(commands.len() as u32).to_le_bytes());
    payload.extend_from_slice(&width.to_le_bytes());
    payload.extend_from_slice(&height.to_le_bytes());

    for cmd in commands {
        // Full serialisation — encode as (discriminant + 6 u64 params) to cover all variants.
        
        let mut disc = 0u8; let mut p = [0u64; 6];
        
        match cmd {
            crate::gpu_driver::GpuCommand::FillRect { x, y, w, h, color } => {
                p[0] = *x as u64; p[1] = *y as u64; p[2] = *w as u64; p[3] = *h as u64; p[4] = *color as u64;
            }
            crate::gpu_driver::GpuCommand::BitBlt { src_x, src_y, dst_x, dst_y, w, h } => {
                disc = 1; p[0] = *src_x as u64; p[1] = *src_y as u64; p[2] = *dst_x as u64; p[3] = *dst_y as u64; p[4] = *w as u64; p[5] = *h as u64;
            }
            crate::gpu_driver::GpuCommand::UploadSurface { vram_offset, width, height, stride, src_ptr } => {
                disc = 2; p[0] = *vram_offset; p[1] = *width as u64; p[2] = *height as u64; p[3] = *stride as u64; p[4] = *src_ptr;
            }
            crate::gpu_driver::GpuCommand::Present { surface_vram_offset, x, y, w, h } => {
                disc = 3; p[0] = *surface_vram_offset; p[1] = *x as u64; p[2] = *y as u64; p[3] = *w as u64; p[4] = *h as u64;
            }
            crate::gpu_driver::GpuCommand::SetVertexShader { spirv_vram_offset } => {
                disc = 4; p[0] = *spirv_vram_offset;
            }
            crate::gpu_driver::GpuCommand::SetFragmentShader { spirv_vram_offset } => {
                disc = 5; p[0] = *spirv_vram_offset;
            }
            crate::gpu_driver::GpuCommand::BindVertexBuffer { vram_offset, stride } => {
                disc = 6; p[0] = *vram_offset; p[1] = *stride as u64;
            }
            crate::gpu_driver::GpuCommand::BindIndexBuffer { vram_offset, index_type } => {
                disc = 7; p[0] = *vram_offset; p[1] = *index_type as u64;
            }
            crate::gpu_driver::GpuCommand::DrawIndexed { index_count, instance_count, first_index } => {
                disc = 8; p[0] = *index_count as u64; p[1] = *instance_count as u64; p[2] = *first_index as u64;
            }
            crate::gpu_driver::GpuCommand::Draw { vertex_count, instance_count, first_vertex } => {
                disc = 9; p[0] = *vertex_count as u64; p[1] = *instance_count as u64; p[2] = *first_vertex as u64;
            }
            crate::gpu_driver::GpuCommand::BindDescriptorSet { set_vram_offset } => {
                disc = 10; p[0] = *set_vram_offset;
            }
            crate::gpu_driver::GpuCommand::SetViewport { x, y, w, h, min_depth, max_depth } => {
                disc = 11; p[0] = x.to_bits() as u64; p[1] = y.to_bits() as u64; p[2] = w.to_bits() as u64; p[3] = h.to_bits() as u64; p[4] = min_depth.to_bits() as u64; p[5] = max_depth.to_bits() as u64;
            }
            crate::gpu_driver::GpuCommand::SetScissor { x, y, w, h } => {
                disc = 12; p[0] = *x as u64; p[1] = *y as u64; p[2] = *w as u64; p[3] = *h as u64;
            }
            crate::gpu_driver::GpuCommand::BeginRenderPass { surface_vram_offset, clear_color, clear_depth } => {
                disc = 13; p[0] = *surface_vram_offset; p[1] = *clear_color as u64; p[2] = clear_depth.to_bits() as u64;
            }
            crate::gpu_driver::GpuCommand::EndRenderPass => {
                disc = 14;
            }
            crate::gpu_driver::GpuCommand::PipelineBarrier { src_stage, dst_stage } => {
                disc = 15; p[0] = *src_stage as u64; p[1] = *dst_stage as u64;
            }
            crate::gpu_driver::GpuCommand::Nop => {
                disc = 16;
            }
        }
        
        payload.push(disc);
        for val in &p {
            payload.extend_from_slice(&val.to_le_bytes());
        }
    }

    let msg    = encode_message(MessageType::GpuSubmitBatch, &payload);
    let remote = IpEndpoint::new(IpAddress::Ipv4(node_ip), GPU_PORT);
    let mut net = NETWORK.lock();
    let stack   = net.as_mut().ok_or(DfsError::NetworkError)?;
    let handle  = stack.open_tcp(0).map_err(|_| DfsError::NetworkError)?;
    stack.tcp_connect(handle, remote, next_ephemeral_port()).map_err(|_| DfsError::NetworkError)?;
    stack.tcp_send(handle, &msg).map_err(|_| DfsError::NetworkError)?;

    // Receive the rendered frame.
    let mut frame_buf = vec![0u8; (width * height * 4) as usize + 16];
    let n = stack.tcp_recv(handle, &mut frame_buf).map_err(|_| DfsError::NetworkError)?;
    stack.close_socket(handle);

    let (msg_type, frame_data) = decode_message(&frame_buf[..n])
        .ok_or(DfsError::ProtocolError)?;
    if msg_type != MessageType::GpuFrameResult {
        return Err(DfsError::ProtocolError);
    }
    Ok(frame_data.to_vec())
}

// =====================================================================
// -----| Part 10 — Remote Desktop Server |-----
// =====================================================================

/// Configuration for the remote desktop server.
pub struct RdpConfig {
    pub width:      u32,
    pub height:     u32,
    /// Target frame rate (fps).
    pub target_fps: u32,
    /// Use GPU framebuffer capture instead of VGA (gaming mode).
    pub gpu_capture: bool,
}

impl RdpConfig {
    pub fn desktop() -> Self {
        Self { width: 1024, height: 768, target_fps: 30, gpu_capture: false }
    }
    pub fn gaming() -> Self {
        Self { width: 1920, height: 1080, target_fps: 60, gpu_capture: true }
    }
}

/// RLE-compress a raw RGBA frame. Returns compressed bytes.
/// Format: [count: u16][pixel: u32] repeated. count = run length.
fn rle_compress_frame(pixels: &[u32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(pixels.len() * 2);
    if pixels.is_empty() { return out; }

    let mut i   = 0usize;
    while i < pixels.len() {
        let val   = pixels[i];
        let start = i;
        while i < pixels.len() && pixels[i] == val && i - start < 0xFFFF {
            i += 1;
        }
        let count = (i - start) as u16;
        out.extend_from_slice(&count.to_le_bytes());
        out.extend_from_slice(&val.to_le_bytes());
    }
    out
}

/// Capture the current framebuffer as RGBA pixels.
fn capture_framebuffer(width: u32, height: u32, gpu_mode: bool) -> Vec<u32> {
    if gpu_mode {
        // GPU capture: read from the VirtFb pixel buffer.
        if let Some(vm) = crate::hypervisor::ACTIVE_VM.lock().as_ref() {
            let devs = vm.devices.lock();
            let _ = devs;
        }
    }
    
    // VGA capture: read from the VGA_WRITER buffer which sits at 0xb8000
    let total = (width * height) as usize;
    let mut pixels = vec![0u32; total];
    
    // The VGA text buffer is 80x25 characters. 
    // We'll roughly map it to the RDP frame buffer using the background colours.
    let vga_ptr = 0xb8000 as *const u16;
    for y in 0..height {
        for x in 0..width {
            // Map screen coordinate to VGA character cell (8x16 font assumed)
            let vga_x = (x / 8) % 80;
            let vga_y = (y / 16) % 25;
            let cell_idx = (vga_y * 80 + vga_x) as isize;
            let cell = unsafe { core::ptr::read_volatile(vga_ptr.offset(cell_idx)) };
            let bg_color = ((cell >> 12) & 0xF) as u32;
            
            // Simple color mapping
            let rgb = match bg_color {
                0 => 0x000000, 1 => 0x0000AA, 2 => 0x00AA00, 3 => 0x00AAAA,
                4 => 0xAA0000, 5 => 0xAA00AA, 6 => 0xAA5500, 7 => 0xAAAAAA,
                8 => 0x555555, 9 => 0x5555FF, 10 => 0x55FF55, 11 => 0x55FFFF,
                12 => 0xFF5555, 13 => 0xFF55FF, 14 => 0xFFFF55, _ => 0xFFFFFF,
            };
            pixels[(y * width + x) as usize] = rgb;
        }
    }
    
    pixels
}

/// Serve one RDP frame to a connected client socket handle.
pub fn rdp_send_frame(handle: smoltcp::iface::SocketHandle, cfg: &RdpConfig) -> NetResult<()> {
    let pixels     = capture_framebuffer(cfg.width, cfg.height, cfg.gpu_capture);
    let compressed = rle_compress_frame(&pixels);

    // Frame header: width(4) + height(4) + compressed_len(4) + data.
    let mut payload = Vec::with_capacity(12 + compressed.len());
    payload.extend_from_slice(&cfg.width.to_le_bytes());
    payload.extend_from_slice(&cfg.height.to_le_bytes());
    payload.extend_from_slice(&(compressed.len() as u32).to_le_bytes());
    payload.extend_from_slice(&compressed);

    let msg = encode_message(MessageType::RdpFrame, &payload);
    let mut net = NETWORK.lock();
    let stack   = net.as_mut().ok_or(NetError::NotReady)?;
    stack.tcp_send(handle, &msg).map(|_| ())
}

/// Process an incoming input event from a remote desktop client.
pub fn rdp_handle_input(data: &[u8]) {
    let (msg_type, payload) = match decode_message(data) { Some(m) => m, None => return };
    match msg_type {
        MessageType::RdpInputKey => {
            if payload.len() >= 2 {
                let scancode = payload[0];
                // Inject into the keyboard queue.
                crate::task::keyboard::add_scancode(scancode);
            }
        }
        MessageType::RdpInputMouse => {
            if payload.len() >= 9 {
                let x       = i32::from_le_bytes(payload[0..4].try_into().unwrap_or([0;4]));
                let y       = i32::from_le_bytes(payload[4..8].try_into().unwrap_or([0;4]));
                let buttons = payload[8];
                
                if let Some(mut state) = crate::task::mouse::MOUSE_STATE.try_lock() {
                    state.x = x.clamp(0, 1023);
                    state.y = y.clamp(0, 767);
                    state.left_button = (buttons & 1) != 0;
                    state.right_button = (buttons & 2) != 0;
                }
            }
        }
        _ => {}
    }
}

// =====================================================================
// -----| Part 11 — Cloud Gaming Host Mode |-----
// =====================================================================

pub struct GamingHostMode {
    pub cfg:         RdpConfig,
    pub client_handle: Option<smoltcp::iface::SocketHandle>,
    pub frame_count: u64,
    pub connected:   bool,
}

impl GamingHostMode {
    pub fn new() -> Self {
        Self {
            cfg:           RdpConfig::gaming(),
            client_handle: None,
            frame_count:   0,
            connected:     false,
        }
    }

    /// Accept a new gaming client connection.
    pub fn accept(&mut self, handle: smoltcp::iface::SocketHandle) {
        self.client_handle = Some(handle);
        self.connected     = true;
        self.frame_count   = 0;
        println!("[DIST/GAMING] Cloud gaming client connected.");
    }

    /// Stream the current frame to the connected client.
    pub fn stream_frame(&mut self) {
        if !self.connected { return; }
        let handle = match self.client_handle { Some(h) => h, None => return };
        match rdp_send_frame(handle, &self.cfg) {
            Ok(_)  => self.frame_count += 1,
            Err(_) => {
                self.connected = false;
                println!("[DIST/GAMING] Client disconnected after {} frames",
                    self.frame_count);
            }
        }
    }

    /// Process input from the connected gaming client.
    pub fn process_input(&self, data: &[u8]) {
        rdp_handle_input(data);
    }
}

lazy_static! {
    pub static ref GAMING_HOST: Mutex<GamingHostMode> =
        Mutex::new(GamingHostMode::new());
}

// =====================================================================
// -----| Part 12 — Cloud VM Manager |-----
// =====================================================================

/// Manages fetching and launching cloud VM disk images.
pub struct CloudVmManager {
    /// Tracks in-progress image downloads: (image_name → bytes_received).
    active_downloads: BTreeMap<String, Vec<u8>>,
}

impl CloudVmManager {
    pub const fn new() -> Self {
        Self { active_downloads: BTreeMap::new() }
    }

    /// Fetch a VM disk image from a remote node and launch it.
    /// `image_name` is the filename on the remote node (e.g. "WIN10.IMG").
    pub fn fetch_and_launch(
        &mut self, source_node: &NodeId, image_name: &str,
    ) -> Result<(), CloudVmError> {
        println!("[DIST/VM] Fetching image '{}' from node {}",
            image_name, source_node.to_hex());

        // Build fetch request.
        let msg = encode_message(
            MessageType::VmImageFetch,
            image_name.as_bytes(),
        );

        let node_ip = NODE_MESH.lock()
            .peers.get(source_node)
            .map(|n| n.ip)
            .ok_or(CloudVmError::NodeNotFound)?;

        let remote = IpEndpoint::new(IpAddress::Ipv4(node_ip), DFS_PORT);
        let mut net = NETWORK.lock();
        let stack   = net.as_mut().ok_or(CloudVmError::NetworkError)?;
        let handle  = stack.open_tcp(0).map_err(|_| CloudVmError::NetworkError)?;
        stack.tcp_connect(handle, remote, next_ephemeral_port())
            .map_err(|_| CloudVmError::NetworkError)?;
        stack.tcp_send(handle, &msg)
            .map_err(|_| CloudVmError::NetworkError)?;

        // Receive image chunks.
        let mut image_data: Vec<u8> = Vec::new();
        let mut chunk_buf = vec![0u8; 65536];
        loop {
            let n = stack.tcp_recv(handle, &mut chunk_buf)
                .map_err(|_| CloudVmError::NetworkError)?;
            if n == 0 { break; }
            if let Some((msg_type, payload)) = decode_message(&chunk_buf[..n]) {
                match msg_type {
                    MessageType::VmImageChunk    => image_data.extend_from_slice(payload),
                    MessageType::VmImageComplete => break,
                    _ => {}
                }
            }
        }
        stack.close_socket(handle);
        drop(net);

        println!("[DIST/VM] Image received: {} bytes", image_data.len());

        // Write image to the local VFS so the hypervisor can boot from it.
        let local_path = alloc::format!("/{}", image_name);
        crate::vfs::VFS
            .write_file(&local_path, &image_data)
            .map_err(|_| CloudVmError::StorageError)?;

        // Launch the VM.
        crate::hypervisor::launch_windows_vm(&local_path, 0x7C00)
            .map_err(|_| CloudVmError::HypervisorError)?;

        println!("[DIST/VM] VM launched from '{}'", local_path);
        Ok(())
    }
}

#[derive(Debug, Clone, Copy)]
pub enum CloudVmError {
    NodeNotFound,
    NetworkError,
    StorageError,
    HypervisorError,
}

lazy_static! {
    pub static ref CLOUD_VM: Mutex<CloudVmManager> =
        Mutex::new(CloudVmManager::new());
}

// =====================================================================
// -----| Part 13 — Global Node Mesh |-----
// =====================================================================

lazy_static! {
    pub static ref NODE_MESH: Mutex<NodeMesh> = {
        // Use placeholder MAC - will be updated in init() to avoid deadlock
        let node_id = NodeId::from_mac(&[0x52,0x54,0x00,0x12,0x34,0x56]);
        Mutex::new(NodeMesh::new(node_id, "veilos-node"))
    };
}

/// Periodic tick — call from the timer background task or scheduler.
/// Read one pending DFS/exec request (if any) and process it — deliberately
/// two separate NODE_MESH locks with the actual processing happening in
/// between, unlocked. Processing a request can itself need to lock
/// NODE_MESH (DIAG, PEERS, etc. all do), so doing it while still holding
/// the lock from reading the request would deadlock the node solid.
pub fn service_mesh_once() {
    let pending = NODE_MESH.lock().poll_dfs_exec_servers();
    if let Some((is_dfs, handle, data)) = pending {
        let resp = if is_dfs {
            DistributedFs::handle_request(&data)
        } else {
            handle_exec_request(&data)
        };
        NODE_MESH.lock().deliver_response(is_dfs, handle, &resp);
    }
}

pub fn tick() {
    let current_tick = crate::interrupts::TICKS.load(Ordering::Relaxed);
    NODE_MESH.lock().tick(current_tick);
    service_mesh_once();
    crate::anticheat::KUSD.lock().tick();
}

// =====================================================================
// -----| Init |-----
// =====================================================================

pub fn init() {
    // Just mark sockets as ready - MAC update happens in tick() 
    // to avoid deadlock when NETWORK is already locked elsewhere
    NODE_MESH.lock().init_sockets();

    // The default inbound firewall policy is Deny (Secure Mode). All of our
    // mesh sockets use pid 0, and without an explicit allow rule every
    // inbound tcp_recv() on the DFS/exec ports is silently rejected by that
    // default — the connection and handshake still succeed (those aren't
    // firewall-gated), so the data visibly arrives and gets ACKed, but it
    // can never actually be read back out. Explicitly allow just these two
    // ports, inbound, for pid 0 — everything else stays default-denied.
    {
        use crate::network::{FIREWALL, FirewallRule, Direction, SocketProto};
        let mut fw = FIREWALL.lock();
        // Port 0 in a FirewallRule means "any port" (see Firewall::evaluate).
        // A port-specific rule isn't enough here: it covers reads on our
        // listening sockets (7701/7702) fine, but our own outgoing mesh
        // connections (DFS/exec clients, relay, scheduler offload) use a
        // fresh random ephemeral local port every single call — there's no
        // fixed set of ports to allow-list for those. pid 0 is exclusively
        // our own mesh networking code, never an arbitrary user process, so
        // giving it unrestricted inbound TCP doesn't weaken the firewall
        // for anything that's actually sandboxed.
        fw.add_rule(FirewallRule::allow_port(0, Direction::Inbound, SocketProto::Tcp, 0));
    }

    println!("[DIST] Distributed Layer initialized (MAC will be updated on first tick).");
}

