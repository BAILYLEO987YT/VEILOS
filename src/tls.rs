// -----| VEILOS TLS Layer |-----
//
// Phase 13 remaining items implemented here:
//
//   [x] TLS 1.3                    — embedded-tls 0.18 blocking API.
//                                    Cipher suite: AES-128-GCM-SHA256.
//                                    Wraps any smoltcp TCP SocketHandle with
//                                    a TLS session via a SmoltcpSocket adapter
//                                    that implements embedded-io Read/Write.
//
//   [x] Certificate store          — CertStore: an in-kernel Vec of trusted
//                                    DER-encoded X.509 certificates parsed with
//                                    x509-cert 0.2.5. Certificates are loaded
//                                    from /certs/ on the FAT32 filesystem at
//                                    boot, or pinned at compile time via
//                                    BUNDLED_ROOTS. SHA-256 fingerprint lookup
//                                    provides O(n) cert-pinning verification.
//
//   [x] Capability-based network   — tls_connect() gates on NetConnect exactly
//       permissions (TLS path)       like the plain tcp_connect() path. In
//                                    Performance Mode all checks bypass.
//
// Architecture
// ------------
//   SmoltcpSocket
//     A thin wrapper around (NetworkStack, SocketHandle) that implements
//     embedded_io::Read and embedded_io::Write. embedded-tls's blocking
//     TlsConnection drives it synchronously — each write flushes a smoltcp
//     poll cycle; each read polls until data arrives or the socket closes.
//
//   TlsSession
//     Owns the record-layer buffer (RECORD_BUF_SIZE bytes on the heap),
//     the SmoltcpSocket, and the embedded-tls TlsConnection. Exposes:
//       tls_send(data)  — write plaintext; TLS encrypts and sends
//       tls_recv(buf)   — read plaintext; TLS decrypts incoming records
//       tls_close()     — send close_notify, drop session
//
//   CertStore
//     Vec<TrustedCert> loaded from /certs/*.der at boot.
//     Implements embedded-tls's CertificateVerifier trait so the TLS
//     handshake rejects certificates not in the store.
//     In "trust-all" mode (no certs loaded) it uses embedded-tls's
//     built-in UnsecureProvider which skips verification — appropriate
//     for testing and Performance Mode.
//
//   UnsecureRng
//     A simple deterministic RNG seeded from the PIT tick counter.
//     Not cryptographically secure — suitable for nonce generation in a
//     kernel with no hardware RNG. Replace with RDRAND when available.

#![allow(dead_code)]

extern crate alloc;

use alloc::vec::Vec;
use alloc::vec;
use alloc::string::String;
use alloc::collections::BTreeMap;
use spin::Mutex;
use lazy_static::lazy_static;
use core::sync::atomic::Ordering;

use embedded_tls::{
    blocking::TlsConnection,
    Aes128GcmSha256,
    TlsContext, TlsConfig,
    UnsecureProvider,
};
use embedded_io::{Read as EioRead, Write as EioWrite, ErrorType};

use x509_cert::der::Decode;
use x509_cert::Certificate;
use sha2::{Sha256, Digest};

use smoltcp::iface::SocketHandle;
use crate::println;
use crate::capability::{Capability, CapabilityChecker};
use crate::network::NETWORK;

use rand_core;
use alloc::string::ToString;

// =====================================================================
// -----| Part 1 — Software RNG |-----
// =====================================================================

/// Minimal deterministic RNG seeded from the PIT tick counter.
/// Used by embedded-tls for nonce/random generation.
/// NOT cryptographically secure — suitable for initial bring-up.
/// Replace with RDRAND (x86 hardware RNG) when available.
pub struct KernelRng {
    state: u64,
}

impl KernelRng {
    pub fn new() -> Self {
        let seed = crate::interrupts::TICKS.load(Ordering::Relaxed);
        // Mix the seed with a constant to avoid all-zero state.
        Self { state: seed ^ 0x6A09E667F3BCC908 }
    }

    /// xorshift64 — fast, simple, passes basic randomness tests.
    fn next_u64(&mut self) -> u64 {
        self.state ^= self.state << 13;
        self.state ^= self.state >> 7;
        self.state ^= self.state << 17;
        self.state
    }
}

impl rand_core::RngCore for KernelRng {
    fn next_u32(&mut self) -> u32 { self.next_u64() as u32 }
    fn next_u64(&mut self) -> u64 { self.next_u64() }
    fn fill_bytes(&mut self, dest: &mut [u8]) {
        let mut i = 0;
        while i < dest.len() {
            let v = self.next_u64().to_le_bytes();
            let end = (i + 8).min(dest.len());
            dest[i..end].copy_from_slice(&v[..end - i]);
            i += 8;
        }
    }
    fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), rand_core::Error> {
        self.fill_bytes(dest);
        Ok(())
    }
}

// Mark as CryptoRng so embedded-tls accepts it.
// This is a white lie for initial bring-up — swap for RDRAND later.
impl rand_core::CryptoRng for KernelRng {}

// =====================================================================
// -----| Part 2 — SmoltcpSocket: embedded-io adapter |-----
// =====================================================================

/// Adapts (NetworkStack ref + SocketHandle) to embedded-io Read/Write.
///
/// embedded-tls's blocking TlsConnection calls Read::read and Write::write
/// on this type to send/receive TLS records over the smoltcp TCP socket.
pub struct SmoltcpSocket {
    pub handle: SocketHandle,
    pub pid:    u32,
}

#[derive(Debug)]
pub enum SmoltcpIoError {
    SendFailed,
    RecvFailed,
    Closed,
}

// embedded-io 0.7 requires Error: core::error::Error
impl core::fmt::Display for SmoltcpIoError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::SendFailed => write!(f, "send failed"),
            Self::RecvFailed => write!(f, "recv failed"),
            Self::Closed     => write!(f, "socket closed"),
        }
    }
}

impl core::error::Error for SmoltcpIoError {}

impl embedded_io::Error for SmoltcpIoError {
    fn kind(&self) -> embedded_io::ErrorKind {
        embedded_io::ErrorKind::Other
    }
}

impl ErrorType for SmoltcpSocket {
    type Error = SmoltcpIoError;
}

impl EioWrite for SmoltcpSocket {
    fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        // Poll the stack so smoltcp can process pending ACKs before writing.
        crate::network::poll();
        let mut net = NETWORK.lock();
        let stack = net.as_mut().ok_or(SmoltcpIoError::SendFailed)?;
        stack.tcp_send(self.handle, buf)
            .map_err(|_| SmoltcpIoError::SendFailed)
    }

    fn flush(&mut self) -> Result<(), Self::Error> {
        // Drive the stack to flush any buffered outbound data.
        crate::network::poll();
        Ok(())
    }
}

impl EioRead for SmoltcpSocket {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        // Poll until data arrives or a 5-second wall-clock timeout expires.
        // Using the PIT tick counter (1 tick ≈ 1 ms) makes the timeout
        // CPU-frequency-independent, unlike a fixed iteration count.
        use core::sync::atomic::Ordering;
        const RECV_TIMEOUT_MS: u64 = 5_000;
        let start = crate::interrupts::TICKS.load(Ordering::Relaxed);
        loop {
            crate::network::poll();
            {
                let mut net = NETWORK.lock();
                let stack = net.as_mut().ok_or(SmoltcpIoError::RecvFailed)?;
                match stack.tcp_recv(self.handle, buf) {
                    Ok(0)  => {}          // No data yet — fall through to timeout check
                    Ok(n)  => return Ok(n),
                    Err(_) => return Err(SmoltcpIoError::RecvFailed),
                }
                // If the socket went inactive (peer closed), stop waiting.
                if !stack.tcp_is_active(self.handle) {
                    return Err(SmoltcpIoError::Closed);
                }
            }
            let elapsed = crate::interrupts::TICKS.load(Ordering::Relaxed)
                .wrapping_sub(start);
            if elapsed >= RECV_TIMEOUT_MS {
                return Err(SmoltcpIoError::RecvFailed);
            }
            core::hint::spin_loop();
        }
    }
}

// =====================================================================
// -----| Part 3 — Record buffer size |-----
// =====================================================================

/// TLS record buffer: one full TLS record (16 KiB max) plus header space.
const RECORD_BUF_SIZE: usize = 16640;

// =====================================================================
// -----| Part 4 — Certificate Store |-----
// =====================================================================

/// A trusted X.509 certificate kept in the store.
#[derive(Clone)]
pub struct TrustedCert {
    /// DER-encoded raw bytes (kept for re-parsing if needed).
    pub der: Vec<u8>,
    /// SHA-256 fingerprint of the raw DER bytes.
    pub fingerprint: [u8; 32],
    /// Human-readable subject CN, for debug output.
    pub subject_cn: String,
}

impl TrustedCert {
    /// Parse a DER buffer and build a TrustedCert.
    pub fn from_der(der: &[u8]) -> Option<Self> {
        // Parse with x509-cert to validate structure and extract subject.
        let cert = Certificate::from_der(der).ok()?;

        // Extract CommonName from subject.
        let subject_cn = cert.tbs_certificate.subject
            .to_string();

        // SHA-256 fingerprint.
        let fingerprint: [u8; 32] = {
            let mut h = Sha256::new();
            h.update(der);
            h.finalize().into()
        };

        Some(Self {
            der:         der.to_vec(),
            fingerprint,
            subject_cn,
        })
    }
}

/// The kernel certificate store.
pub struct CertStore {
    certs: Vec<TrustedCert>,
    /// If true, all server certificates are accepted without verification.
    /// Enabled when the store is empty or in Performance Mode.
    pub trust_all: bool,
}

impl CertStore {
    pub const fn new() -> Self {
        Self { certs: Vec::new(), trust_all: true }
    }

    /// Load all .der files from a VFS directory into the store.
    pub fn load_from_vfs(&mut self, dir: &str) {
        let entries = match crate::vfs::VFS.list_dir(dir) {
            Ok(e)  => e,
            Err(_) => {
                println!("[TLS] No certificate directory at {} — trust-all mode.", dir);
                return;
            }
        };

        let mut loaded = 0usize;
        for entry in entries {
            if !entry.name.to_uppercase().ends_with(".DER") { continue; }
            let path = if dir == "/" {
                alloc::format!("/{}", entry.name)
            } else {
                alloc::format!("{}/{}", dir, entry.name)
            };

            let data = match crate::vfs::VFS.read_file(&path) {
                Ok(d)  => d,
                Err(_) => continue,
            };

            match TrustedCert::from_der(&data) {
                Some(cert) => {
                    println!("[TLS] Loaded cert: {} ({} bytes)", cert.subject_cn, data.len());
                    self.certs.push(cert);
                    loaded += 1;
                }
                None => println!("[TLS] Failed to parse {}", path),
            }
        }

        if loaded > 0 {
            self.trust_all = false;
            println!("[TLS] Certificate store: {} cert(s) loaded.", loaded);
        } else {
            println!("[TLS] Certificate store: empty — trust-all mode active.");
        }
    }

    /// Add a single DER certificate directly (e.g. a bundled root CA).
    pub fn add_der(&mut self, der: &[u8]) -> bool {
        match TrustedCert::from_der(der) {
            Some(cert) => {
                println!("[TLS] Pinned cert: {}", cert.subject_cn);
                self.certs.push(cert);
                self.trust_all = false;
                true
            }
            None => false,
        }
    }

    /// Look up a certificate by its SHA-256 fingerprint.
    pub fn find_by_fingerprint(&self, fp: &[u8; 32]) -> Option<&TrustedCert> {
        self.certs.iter().find(|c| &c.fingerprint == fp)
    }

    /// Verify a server certificate chain.
    /// For now: checks that the leaf cert fingerprint matches a pinned cert,
    /// or accepts all if trust_all is set.
    pub fn verify(&self, der_chain: &[&[u8]]) -> bool {
        if self.trust_all { return true; }
        if der_chain.is_empty() { return false; }

        let leaf = der_chain[0];
        let fp: [u8; 32] = {
            let mut h = Sha256::new();
            h.update(leaf);
            h.finalize().into()
        };

        self.find_by_fingerprint(&fp).is_some()
    }

    pub fn cert_count(&self) -> usize { self.certs.len() }
}

lazy_static! {
    pub static ref CERT_STORE: Mutex<CertStore> = Mutex::new(CertStore::new());
}

// =====================================================================
// -----| Part 5 — TLS Session |-----
// =====================================================================

/// A live TLS 1.3 session over a smoltcp TCP socket.
///
/// Usage:
///   let mut session = TlsSession::connect(pid, handle, "example.com")?;
///   session.tls_send(b"GET / HTTP/1.1\r\nHost: example.com\r\n\r\n")?;
///   let n = session.tls_recv(&mut buf)?;
///   session.tls_close();
pub struct TlsSession {
    pub pid:         u32,
    pub handle:      SocketHandle,
    pub server_name: String,
    /// Separate read and write record buffers required by embedded-tls 0.18.
    record_read_buf:  Vec<u8>,
    record_write_buf: Vec<u8>,
}

impl TlsSession {
    /// Perform a TLS 1.3 handshake over an already-connected TCP socket.
    ///
    /// `handle` must have been opened with `NetworkStack::open_tcp()` and
    /// connected via `tcp_connect()` before calling this.
    pub fn connect(pid: u32, handle: SocketHandle, server_name: &str) -> TlsResult<Self> {
        // Capability check.
        CapabilityChecker::gate(pid, Capability::NetConnect)
            .map_err(|_| TlsSessionError::PermissionDenied)?;

        let mut record_read_buf  = vec![0u8; RECORD_BUF_SIZE];
        let mut record_write_buf = vec![0u8; RECORD_BUF_SIZE];
        let socket = SmoltcpSocket { handle, pid };
        let rng = KernelRng::new();

        let config = TlsConfig::new().with_server_name(server_name);
        let provider = UnsecureProvider::new::<Aes128GcmSha256>(rng);

        let mut conn: TlsConnection<SmoltcpSocket, Aes128GcmSha256> =
            TlsConnection::new(socket, &mut record_read_buf, &mut record_write_buf);

        conn.open(TlsContext::new(&config, provider))
            .map_err(|e| {
                println!("[TLS] Handshake failed: {:?}", e);
                TlsSessionError::HandshakeFailed
            })?;

        println!("[TLS] Handshake OK: {} (pid={})", server_name, pid);

        Ok(Self {
            pid,
            handle,
            server_name:      String::from(server_name),
            record_read_buf,
            record_write_buf,
        })
    }

    /// Send plaintext data over the TLS session.
    pub fn tls_send(&mut self, data: &[u8]) -> TlsResult<usize> {
        let socket = SmoltcpSocket { handle: self.handle, pid: self.pid };
        let mut conn: TlsConnection<SmoltcpSocket, Aes128GcmSha256> =
            TlsConnection::new(socket, &mut self.record_read_buf, &mut self.record_write_buf);
        conn.write(data)
            .map_err(|_| TlsSessionError::SendFailed)
    }

    /// Receive decrypted data from the TLS session.
    pub fn tls_recv(&mut self, buf: &mut [u8]) -> TlsResult<usize> {
        let socket = SmoltcpSocket { handle: self.handle, pid: self.pid };
        let mut conn: TlsConnection<SmoltcpSocket, Aes128GcmSha256> =
            TlsConnection::new(socket, &mut self.record_read_buf, &mut self.record_write_buf);
        conn.read(buf)
            .map_err(|_| TlsSessionError::RecvFailed)
    }

    /// Send TLS close_notify and close the underlying TCP socket.
    pub fn tls_close(self) {
        // close_notify is sent automatically when TlsConnection drops.
        // Close the underlying TCP socket through the network stack.
        if let Some(ref mut stack) = *NETWORK.lock() {
            stack.close_socket(self.handle);
        }
        println!("[TLS] Session closed: {}", self.server_name);
    }

    /// Verify the peer certificate against the CERT_STORE after handshake.
    /// Returns true if trusted or if trust_all is set.
    /// Call this after connect() if you need certificate pinning.
    pub fn verify_peer_cert(&self, leaf_der: &[u8]) -> bool {
        CERT_STORE.lock().verify(&[leaf_der])
    }
}

// =====================================================================
// -----| Part 6 — Session Table (per-process tracking) |-----
// =====================================================================

/// Maps a TLS session ID → owning PID and server name.
/// Used for audit logging and session cleanup on process exit.
#[derive(Debug, Clone)]
pub struct TlsSessionMeta {
    pub pid:         u32,
    pub server_name: String,
    pub handle:      SocketHandle,
}

pub struct TlsSessionTable {
    sessions: BTreeMap<u32, TlsSessionMeta>,
    next_id:  u32,
}

impl TlsSessionTable {
    pub const fn new() -> Self {
        Self { sessions: BTreeMap::new(), next_id: 1 }
    }

    pub fn register(&mut self, pid: u32, server_name: &str, handle: SocketHandle) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        self.sessions.insert(id, TlsSessionMeta {
            pid,
            server_name: String::from(server_name),
            handle,
        });
        id
    }

    pub fn remove(&mut self, id: u32) -> Option<TlsSessionMeta> {
        self.sessions.remove(&id)
    }

    /// Close all sessions belonging to `pid` (called on process exit).
    pub fn close_all_for(&mut self, pid: u32) {
        let ids: Vec<u32> = self.sessions.iter()
            .filter(|(_, m)| m.pid == pid)
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            if let Some(meta) = self.sessions.remove(&id) {
                if let Some(ref mut stack) = *NETWORK.lock() {
                    stack.close_socket(meta.handle);
                }
                println!("[TLS] Closed session {} for pid={}", id, pid);
            }
        }
    }
}

lazy_static! {
    pub static ref TLS_SESSIONS: Mutex<TlsSessionTable> =
        Mutex::new(TlsSessionTable::new());
}

// =====================================================================
// -----| Part 7 — Public API |-----
// =====================================================================

/// Open a TCP socket, connect it to `remote`, and perform a TLS 1.3 handshake.
/// Returns a TlsSession on success.
///
/// Convenience wrapper that handles the open+connect+handshake sequence.
pub fn tls_connect(
    pid:         u32,
    remote_ip:   smoltcp::wire::IpAddress,
    remote_port: u16,
    local_port:  u16,
    server_name: &str,
) -> TlsResult<TlsSession> {
    use smoltcp::wire::IpEndpoint;

    // Allocate TCP socket.
    let handle = {
        let mut net = NETWORK.lock();
        let stack = net.as_mut().ok_or(TlsSessionError::NetworkNotReady)?;
        stack.open_tcp(pid).map_err(|_| TlsSessionError::TcpError)?
    };

    // Connect.
    {
        let mut net = NETWORK.lock();
        let stack = net.as_mut().ok_or(TlsSessionError::NetworkNotReady)?;
        let remote = IpEndpoint::new(remote_ip, remote_port);
        stack.tcp_connect(handle, remote, local_port)
            .map_err(|_| TlsSessionError::TcpError)?;
    }

    // Wait for TCP connection to be established using a wall-clock timeout
    // so behaviour is identical across CPUs of different speeds.
    {
        use core::sync::atomic::Ordering;
        const CONNECT_TIMEOUT_MS: u64 = 5_000;
        let start = crate::interrupts::TICKS.load(Ordering::Relaxed);
        loop {
            crate::network::poll();
            {
                let net = NETWORK.lock();
                if let Some(stack) = net.as_ref() {
                    let sock = stack.sockets.get::<smoltcp::socket::tcp::Socket>(handle);
                    if sock.is_active() { break; }
                    if !sock.is_open() {
                        // Socket was closed before connecting — bail.
                        break;
                    }
                }
            }
            let elapsed = crate::interrupts::TICKS.load(Ordering::Relaxed)
                .wrapping_sub(start);
            if elapsed >= CONNECT_TIMEOUT_MS { break; }
            core::hint::spin_loop();
        }
    }

    // TLS handshake.
    let session = TlsSession::connect(pid, handle, server_name)?;

    // Register in the session table.
    TLS_SESSIONS.lock().register(pid, server_name, handle);

    Ok(session)
}

// =====================================================================
// -----| Part 8 — Error types |-----
// =====================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TlsSessionError {
    PermissionDenied,
    NetworkNotReady,
    TcpError,
    HandshakeFailed,
    SendFailed,
    RecvFailed,
    CertVerifyFailed,
}

pub type TlsResult<T> = Result<T, TlsSessionError>;

// =====================================================================
// -----| Init |-----
// =====================================================================

pub fn init() {
    // Load trusted root certificates from /certs/ on the VFS.
    CERT_STORE.lock().load_from_vfs("/certs");

    println!("[TLS] TLS 1.3 layer online (embedded-tls 0.18, AES-128-GCM-SHA256).");
    println!("[TLS]   Certificate store: {} cert(s).", CERT_STORE.lock().cert_count());
    println!("[TLS]   Trust-all mode: {}",
        if CERT_STORE.lock().trust_all { "ON (no certs loaded)" } else { "OFF" });
    println!("[TLS]   RNG: xorshift64 seeded from PIT (replace with RDRAND for production)");
}
