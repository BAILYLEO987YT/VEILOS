// -----| Sockets for Win32 programs |-----
//
// What ws2_32.dll stands on: one syscall, VEIL_SOCKET(op, ...), over kernel
// socket objects that live in a process's handle table like files do, so a
// SOCKET is a handle that closes, duplicates and is inherited like one.
//
//   TCP and UDP go through the kernel's smoltcp stack (network.rs).
//   TCP to 127.0.0.1 never reaches smoltcp: a connection there is a pair of
//   byte queues in the kernel, made when a program connects to a listener on
//   this machine. (smoltcp has no loopback on the virtio interface.)
//   DNS is smoltcp's resolver, waited on here.
//
// Errors come back as 0xE000_0000 | the Winsock error number, which ws2_32
// hands to WSASetLastError. Blocking calls wait in the kernel, polling the
// stack themselves, and honour the socket's non-blocking flag and timeouts.

use alloc::collections::VecDeque;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU16, AtomicU32, Ordering};

use nt_abi::{status, NtStatus};
use smoltcp::iface::SocketHandle;
use smoltcp::socket::{dns, tcp, udp};
use smoltcp::time::Duration;
use smoltcp::wire::{IpAddress, IpEndpoint, IpListenEndpoint, Ipv4Address};
use spin::Mutex;

use crate::interrupts::syscall_stack_arg as stack;
use crate::network::{NetworkStack, NETWORK};
use crate::nt_file::ptr_ok;
use crate::nt_objects::KernelObjectRef;
use crate::process::Process;

// -----| errors |-----

const WSAEACCES: u32 = 10013;
const WSAEFAULT: u32 = 10014;
const WSAEINVAL: u32 = 10022;
const WSAEWOULDBLOCK: u32 = 10035;
const WSAEALREADY: u32 = 10037;
const WSAENOTSOCK: u32 = 10038;
const WSAEDESTADDRREQ: u32 = 10039;
const WSAEOPNOTSUPP: u32 = 10045;
const WSAEAFNOSUPPORT: u32 = 10047;
const WSAEADDRINUSE: u32 = 10048;
const WSAENETDOWN: u32 = 10050;
const WSAENETUNREACH: u32 = 10051;
const WSAECONNRESET: u32 = 10054;
const WSAENOBUFS: u32 = 10055;
const WSAEISCONN: u32 = 10056;
const WSAENOTCONN: u32 = 10057;
const WSAESHUTDOWN: u32 = 10058;
const WSAETIMEDOUT: u32 = 10060;
const WSAECONNREFUSED: u32 = 10061;
const WSAHOST_NOT_FOUND: u32 = 11001;
const WSATRY_AGAIN: u32 = 11002;

fn wsa(e: u32) -> NtStatus {
    (0xE000_0000 | e) as NtStatus
}

type Res<T> = Result<T, u32>;

// -----| sizes and times |-----

const TCP_BUFFER: usize = 128 * 1024;
const LOOP_BUFFER: usize = 256 * 1024;
const UDP_SLOTS: usize = 64;
const UDP_BUFFER: usize = 64 * 1024;
/// How long a connection attempt may take before WSAETIMEDOUT (Windows gives
/// up after about 21 seconds).
const CONNECT_MS: u64 = 21_000;
/// How long a lookup waits for the DNS server.
const RESOLVE_MS: u64 = 10_000;
/// A closed connection's last data and FIN get this long to go out.
const CLOSE_LINGER_MS: u64 = 60_000;
const MAX_BACKLOG: usize = 8;

const KIND_TCP: u32 = 1;
const KIND_UDP: u32 = 2;

const LOOPBACK: [u8; 4] = [127, 0, 0, 1];

fn now() -> u64 {
    crate::interrupts::TICKS.load(Ordering::Relaxed)
}

// -----| ports |-----

static NEXT_PORT: AtomicU16 = AtomicU16::new(49152);

/// A port from the dynamic range (49152-65535), not one a loopback listener has.
fn ephemeral_port() -> u16 {
    loop {
        let p = NEXT_PORT.fetch_add(1, Ordering::Relaxed);
        if p < 49152 {
            NEXT_PORT.store(49152, Ordering::Relaxed);
            continue;
        }
        if !LOOP_LISTENERS.lock().iter().any(|l| l.port == p) {
            return p;
        }
    }
}

// -----| loopback connections |-----

/// One direction of a loopback connection.
struct Flow {
    data: VecDeque<u8>,
    /// The sender shut down or closed: once drained, reads return 0.
    finished: bool,
    /// The receiver closed: sends fail.
    abandoned: bool,
}

impl Flow {
    fn new() -> Self {
        Flow { data: VecDeque::new(), finished: false, abandoned: false }
    }
}

/// A connection to 127.0.0.1: the client writes `up` and reads `down`.
pub struct LoopPair {
    up: Mutex<Flow>,
    down: Mutex<Flow>,
    client_port: u16,
}

impl LoopPair {
    fn outgoing(&self, server: bool) -> &Mutex<Flow> {
        if server { &self.down } else { &self.up }
    }
    fn incoming(&self, server: bool) -> &Mutex<Flow> {
        if server { &self.up } else { &self.down }
    }
}

/// A loopback listener: connections wait here until accepted.
struct LoopListener {
    port: u16,
    backlog: usize,
    pending: Arc<Mutex<VecDeque<Arc<LoopPair>>>>,
}

static LOOP_LISTENERS: Mutex<Vec<LoopListener>> = Mutex::new(Vec::new());

// -----| the socket object |-----

enum Conn {
    /// Made, not yet connected or listening.
    Fresh,
    /// A smoltcp TCP socket: connecting, connected, or done.
    Tcp(SocketHandle),
    /// Listening: smoltcp sockets waiting for connections (none when bound to
    /// 127.0.0.1 only), and the loopback queue.
    Listen { port: u16, waiting: Vec<SocketHandle>, pending: Arc<Mutex<VecDeque<Arc<LoopPair>>>> },
    /// One end of a loopback connection.
    Loop { pair: Arc<LoopPair>, server: bool },
    Udp(SocketHandle),
}

struct State {
    conn: Conn,
    /// Address and port given by bind (or chosen at connect).
    local: Option<([u8; 4], u16)>,
    /// Where a connected (or UDP default) peer is.
    peer: Option<([u8; 4], u16)>,
    connecting: bool,
    shut_read: bool,
    shut_write: bool,
    /// SO_ERROR: the last asynchronous failure, read once.
    error: u32,
}

pub struct SocketObj {
    kind: u32,
    state: Mutex<State>,
    nonblocking: AtomicBool,
    /// SO_RCVTIMEO and SO_SNDTIMEO in milliseconds; 0 means wait for ever.
    recv_timeout: AtomicU32,
    send_timeout: AtomicU32,
    /// Options kept to be read back: TCP_NODELAY, SO_KEEPALIVE, SO_RCVBUF,
    /// SO_SNDBUF, SO_REUSEADDR, SO_BROADCAST, SO_LINGER.
    options: Mutex<[u64; 8]>,
}

impl SocketObj {
    fn new(kind: u32, conn: Conn) -> Self {
        SocketObj {
            kind,
            state: Mutex::new(State { conn, local: None, peer: None, connecting: false, shut_read: false, shut_write: false, error: 0 }),
            nonblocking: AtomicBool::new(false),
            recv_timeout: AtomicU32::new(0),
            send_timeout: AtomicU32::new(0),
            options: Mutex::new([0, 0, 65536, 65536, 0, 0, 0, 0]),
        }
    }

    fn deadline(&self, sending: bool) -> Option<u64> {
        let ms = if sending { &self.send_timeout } else { &self.recv_timeout }.load(Ordering::Relaxed);
        (ms != 0).then(|| now() + ms as u64)
    }
}

impl Drop for SocketObj {
    fn drop(&mut self) {
        let state = self.state.get_mut();
        match core::mem::replace(&mut state.conn, Conn::Fresh) {
            Conn::Fresh => {}
            Conn::Tcp(h) => net_quiet(|s| s.close_tcp_gracefully(h, now() + CLOSE_LINGER_MS)),
            Conn::Udp(h) => net_quiet(|s| s.close_socket(h)),
            Conn::Listen { waiting, pending, .. } => {
                LOOP_LISTENERS.lock().retain(|l| !Arc::ptr_eq(&l.pending, &pending));
                // Connections never accepted are refused now.
                for pair in pending.lock().drain(..) {
                    pair.down.lock().finished = true;
                    pair.up.lock().abandoned = true;
                }
                net_quiet(|s| {
                    for h in waiting {
                        s.sockets.get_mut::<tcp::Socket>(h).abort();
                        s.close_tcp_gracefully(h, now() + CLOSE_LINGER_MS);
                    }
                });
            }
            Conn::Loop { pair, server } => {
                pair.outgoing(server).lock().finished = true;
                pair.incoming(server).lock().abandoned = true;
            }
        }
    }
}

// -----| waiting |-----

/// Run `f` on the network stack, after letting it move.
fn net<T>(f: impl FnOnce(&mut NetworkStack) -> T) -> Res<T> {
    let mut guard = NETWORK.lock();
    let stack = guard.as_mut().ok_or(WSAENETDOWN)?;
    stack.poll();
    Ok(f(stack))
}

fn net_quiet(f: impl FnOnce(&mut NetworkStack)) {
    if let Some(stack) = NETWORK.lock().as_mut() {
        f(stack);
    }
}

/// Ask `ready` until it has an answer, waiting between tries. None for
/// `deadline` waits for ever; `nonblocking` answers WSAEWOULDBLOCK at once.
fn wait<T>(nonblocking: bool, deadline: Option<u64>, mut ready: impl FnMut() -> Option<Res<T>>) -> Res<T> {
    loop {
        if let Some(r) = ready() {
            return r;
        }
        if nonblocking {
            return Err(WSAEWOULDBLOCK);
        }
        if deadline.is_some_and(|d| now() >= d) {
            return Err(WSAETIMEDOUT);
        }
        crate::loader::kill_if_requested();
        crate::nt_syscalls::wait_a_little();
    }
}

// -----| addresses |-----

fn read_sockaddr(p: u64) -> Res<([u8; 4], u16)> {
    if !ptr_ok(p, 16) {
        return Err(WSAEFAULT);
    }
    let b = unsafe { core::slice::from_raw_parts(p as *const u8, 16) };
    if u16::from_le_bytes([b[0], b[1]]) != 2 {
        return Err(WSAEAFNOSUPPORT);
    }
    Ok(([b[4], b[5], b[6], b[7]], u16::from_be_bytes([b[2], b[3]])))
}

fn write_sockaddr(p: u64, (ip, port): ([u8; 4], u16)) {
    if p == 0 || !ptr_ok(p, 16) {
        return;
    }
    let mut b = [0u8; 16];
    b[0] = 2;
    b[2..4].copy_from_slice(&port.to_be_bytes());
    b[4..8].copy_from_slice(&ip);
    unsafe { core::ptr::copy_nonoverlapping(b.as_ptr(), p as *mut u8, 16) };
}

fn endpoint((ip, port): ([u8; 4], u16)) -> IpEndpoint {
    IpEndpoint::new(IpAddress::Ipv4(Ipv4Address::new(ip[0], ip[1], ip[2], ip[3])), port)
}

fn from_endpoint(e: IpEndpoint) -> ([u8; 4], u16) {
    let ip = match e.addr {
        IpAddress::Ipv4(a) => a.octets(),
        #[allow(unreachable_patterns)]
        _ => [0; 4],
    };
    (ip, e.port)
}

fn is_loopback(ip: [u8; 4]) -> bool {
    ip[0] == 127
}

/// This machine's address on the network.
fn own_ip() -> [u8; 4] {
    crate::network::NETWORK_CONFIG.lock().ip.address().octets()
}

fn write_out(p: u64, v: u64) {
    if p != 0 && ptr_ok(p, 8) {
        unsafe { *(p as *mut u64) = v };
    }
}

// -----| the syscall |-----

fn socket_of(proc: &Process, h: u64) -> Res<Arc<SocketObj>> {
    match crate::nt_process::object(proc, h) {
        Some(KernelObjectRef::Socket(s)) => Ok(s),
        _ => Err(WSAENOTSOCK),
    }
}

/// VEIL_SOCKET(op, a, b, c, [d, e]): see nt_abi.
pub fn call(proc: &Process, op: u64, a: u64, b: u64, c: u64) -> NtStatus {
    let d = stack(4).unwrap_or(0);
    let e = stack(5).unwrap_or(0);
    let r = match op {
        0 => create(proc, a as u32, b),
        1 => socket_of(proc, a).and_then(|s| bind(&s, b)),
        2 => socket_of(proc, a).and_then(|s| connect(&s, b)),
        3 => socket_of(proc, a).and_then(|s| listen(&s, b as usize)),
        4 => socket_of(proc, a).and_then(|s| accept(proc, &s, b, c)),
        5 => socket_of(proc, a).and_then(|s| send(&s, b, c as usize, None)).map(|n| write_out(d, n as u64)),
        6 => socket_of(proc, a).and_then(|s| recv(&s, b, c as usize, e as u32, None)).map(|n| write_out(d, n as u64)),
        7 => socket_of(proc, a).and_then(|s| {
            let to = read_sockaddr(e)?;
            send(&s, b, c as usize, Some(to))
        }).map(|n| write_out(d, n as u64)),
        8 => socket_of(proc, a).and_then(|s| recv(&s, b, c as usize, 0, Some(e))).map(|n| write_out(d, n as u64)),
        9 => socket_of(proc, a).and_then(|s| shutdown(&s, b as u32)),
        10 => poll(proc, a, b as usize, c).map(|n| write_out(d, n as u64)),
        11 => socket_of(proc, a).and_then(|s| name(&s, b != 0).map(|n| write_sockaddr(c, n))),
        12 => socket_of(proc, a).and_then(|s| set_option(&s, b as u32, c)),
        13 => socket_of(proc, a).and_then(|s| get_option(&s, b as u32)).map(|v| write_out(c, v)),
        14 => resolve(a, b, c as usize).map(|n| write_out(d, n as u64)),
        _ => Err(WSAEINVAL),
    };
    match r {
        Ok(()) => status::SUCCESS,
        Err(e) => wsa(e),
    }
}

fn create(proc: &Process, kind: u32, out: u64) -> Res<()> {
    if kind != KIND_TCP && kind != KIND_UDP {
        return Err(WSAEOPNOTSUPP);
    }
    if !ptr_ok(out, 8) {
        return Err(WSAEFAULT);
    }
    if crate::capability::CapabilityChecker::gate(proc.pid, crate::capability::Capability::NetConnect).is_err() {
        return Err(WSAEACCES);
    }
    let obj = Arc::new(SocketObj::new(kind, Conn::Fresh));
    let h = proc.handles.lock().allocate_ref(KernelObjectRef::Socket(obj), 0x001F_01FF, 0);
    write_out(out, h as u64);
    Ok(())
}

fn bind(s: &SocketObj, sa: u64) -> Res<()> {
    let (ip, port) = read_sockaddr(sa)?;
    let mut st = s.state.lock();
    if st.local.is_some() {
        return Err(WSAEINVAL);
    }
    let port = if port == 0 { ephemeral_port() } else { port };
    if s.kind == KIND_UDP && !is_loopback(ip) {
        let h = net(|n| {
            let h = n.sockets.add(new_udp());
            n.sockets.get_mut::<udp::Socket>(h).bind(port).map(|_| h).map_err(|_| WSAEADDRINUSE)
        })??;
        st.conn = Conn::Udp(h);
    }
    st.local = Some((ip, port));
    Ok(())
}

fn new_udp() -> udp::Socket<'static> {
    udp::Socket::new(
        udp::PacketBuffer::new(vec![udp::PacketMetadata::EMPTY; UDP_SLOTS], vec![0u8; UDP_BUFFER]),
        udp::PacketBuffer::new(vec![udp::PacketMetadata::EMPTY; UDP_SLOTS], vec![0u8; UDP_BUFFER]),
    )
}

fn new_tcp() -> tcp::Socket<'static> {
    tcp::Socket::new(tcp::SocketBuffer::new(vec![0u8; TCP_BUFFER]), tcp::SocketBuffer::new(vec![0u8; TCP_BUFFER]))
}

fn connect(s: &SocketObj, sa: u64) -> Res<()> {
    let to = read_sockaddr(sa)?;
    if s.kind == KIND_UDP {
        s.state.lock().peer = Some(to);
        return Ok(());
    }
    {
        let mut st = s.state.lock();
        match st.conn {
            Conn::Fresh => {}
            Conn::Tcp(_) if st.connecting => return Err(WSAEALREADY),
            _ => return Err(WSAEISCONN),
        }
        if is_loopback(to.0) {
            // Before taking the listeners' lock: choosing a port takes it too.
            let client_port = st.local.map(|l| l.1).unwrap_or_else(ephemeral_port);
            let listeners = LOOP_LISTENERS.lock();
            let Some(l) = listeners.iter().find(|l| l.port == to.1) else { return Err(WSAECONNREFUSED) };
            let mut pending = l.pending.lock();
            if pending.len() >= l.backlog {
                return Err(WSAECONNREFUSED);
            }
            let pair = Arc::new(LoopPair { up: Mutex::new(Flow::new()), down: Mutex::new(Flow::new()), client_port });
            pending.push_back(pair.clone());
            st.conn = Conn::Loop { pair, server: false };
            st.local = Some((LOOPBACK, client_port));
            st.peer = Some(to);
            return Ok(());
        }
        let local_port = st.local.map(|l| l.1).unwrap_or_else(ephemeral_port);
        let h = net(|n| {
            let h = n.sockets.add(new_tcp());
            let cx = n.iface.context();
            match n.sockets.get_mut::<tcp::Socket>(h).connect(cx, endpoint(to), local_port) {
                Ok(()) => Ok(h),
                Err(_) => {
                    n.sockets.remove(h);
                    Err(WSAENETUNREACH)
                }
            }
        })??;
        st.conn = Conn::Tcp(h);
        st.local = Some((own_ip(), local_port));
        st.peer = Some(to);
        st.connecting = true;
    }
    let deadline = Some(now() + CONNECT_MS);
    let r = wait(s.nonblocking.load(Ordering::Relaxed), deadline, || connect_done(s));
    if r.is_err() && r != Err(WSAEWOULDBLOCK) {
        // A blocking connect that failed leaves the socket as if never tried.
        let mut st = s.state.lock();
        if let Conn::Tcp(h) = core::mem::replace(&mut st.conn, Conn::Fresh) {
            net_quiet(|n| {
                n.sockets.get_mut::<tcp::Socket>(h).abort();
                n.close_tcp_gracefully(h, now());
            });
        }
        st.connecting = false;
        st.peer = None;
    }
    r
}

/// Has a TCP connection attempt finished? Some(Err) when it failed.
fn connect_done(s: &SocketObj) -> Option<Res<()>> {
    let mut st = s.state.lock();
    let Conn::Tcp(h) = st.conn else { return Some(Ok(())) };
    if !st.connecting {
        return Some(Ok(()));
    }
    let state = match net(|n| n.sockets.get::<tcp::Socket>(h).state()) {
        Ok(state) => state,
        Err(e) => return Some(Err(e)),
    };
    match state {
        tcp::State::SynSent | tcp::State::SynReceived => None,
        tcp::State::Closed => {
            st.connecting = false;
            st.error = WSAECONNREFUSED;
            Some(Err(WSAECONNREFUSED))
        }
        _ => {
            st.connecting = false;
            Some(Ok(()))
        }
    }
}

fn listen(s: &SocketObj, backlog: usize) -> Res<()> {
    if s.kind != KIND_TCP {
        return Err(WSAEOPNOTSUPP);
    }
    let mut st = s.state.lock();
    match st.conn {
        Conn::Fresh => {}
        Conn::Listen { .. } => return Ok(()),
        _ => return Err(WSAEISCONN),
    }
    let Some((ip, port)) = st.local else { return Err(WSAEINVAL) };
    let backlog = backlog.clamp(1, MAX_BACKLOG);
    {
        let mut listeners = LOOP_LISTENERS.lock();
        if listeners.iter().any(|l| l.port == port) {
            return Err(WSAEADDRINUSE);
        }
        let pending = Arc::new(Mutex::new(VecDeque::new()));
        listeners.push(LoopListener { port, backlog, pending: pending.clone() });
        st.conn = Conn::Listen { port, waiting: Vec::new(), pending };
    }
    // Bound to 127.0.0.1 only: nothing on the network can reach it.
    if !is_loopback(ip) {
        let made = net(|n| {
            let mut waiting = Vec::new();
            for _ in 0..backlog {
                let h = n.sockets.add(new_tcp());
                let endpoint = IpListenEndpoint { addr: None, port };
                if n.sockets.get_mut::<tcp::Socket>(h).listen(endpoint).is_err() {
                    n.sockets.remove(h);
                    break;
                }
                waiting.push(h);
            }
            waiting
        });
        if let (Ok(made), Conn::Listen { waiting, .. }) = (made, &mut st.conn) {
            *waiting = made;
        }
    }
    Ok(())
}

/// A connection that's ready to be accepted, if there is one.
fn take_connection(s: &SocketObj) -> Option<Res<(Conn, ([u8; 4], u16), ([u8; 4], u16))>> {
    let mut st = s.state.lock();
    let Conn::Listen { port, waiting, pending } = &mut st.conn else { return Some(Err(WSAEINVAL)) };
    let port = *port;
    if let Some(pair) = pending.lock().pop_front() {
        let peer = (LOOPBACK, pair.client_port);
        return Some(Ok((Conn::Loop { pair, server: true }, (LOOPBACK, port), peer)));
    }
    if waiting.is_empty() {
        return None;
    }
    let found = net(|n| {
        let i = waiting.iter().position(|&h| {
            let sock = n.sockets.get::<tcp::Socket>(h);
            !matches!(sock.state(), tcp::State::Listen | tcp::State::SynReceived | tcp::State::Closed)
        })?;
        let h = waiting[i];
        // Another takes its place in the backlog.
        let fresh = n.sockets.add(new_tcp());
        let _ = n.sockets.get_mut::<tcp::Socket>(fresh).listen(IpListenEndpoint { addr: None, port });
        waiting[i] = fresh;
        let sock = n.sockets.get::<tcp::Socket>(h);
        let local = sock.local_endpoint().map(from_endpoint).unwrap_or((own_ip(), port));
        let peer = sock.remote_endpoint().map(from_endpoint).unwrap_or(([0; 4], 0));
        Some((h, local, peer))
    });
    match found {
        Ok(Some((h, local, peer))) => Some(Ok((Conn::Tcp(h), local, peer))),
        Ok(None) => None,
        Err(e) => Some(Err(e)),
    }
}

fn accept(proc: &Process, s: &SocketObj, out: u64, sa: u64) -> Res<()> {
    if !ptr_ok(out, 8) {
        return Err(WSAEFAULT);
    }
    let (conn, local, peer) = wait(s.nonblocking.load(Ordering::Relaxed), None, || take_connection(s))?;
    let obj = SocketObj::new(KIND_TCP, conn);
    {
        let mut st = obj.state.lock();
        st.local = Some(local);
        st.peer = Some(peer);
    }
    let h = proc.handles.lock().allocate_ref(KernelObjectRef::Socket(Arc::new(obj)), 0x001F_01FF, 0);
    write_out(out, h as u64);
    write_sockaddr(sa, peer);
    Ok(())
}

/// Copy from the program first: nothing touches its memory with the stack locked.
fn user_bytes(buf: u64, len: usize) -> Res<Vec<u8>> {
    if len > 0 && !ptr_ok(buf, len) {
        return Err(WSAEFAULT);
    }
    Ok(if len == 0 { Vec::new() } else { unsafe { core::slice::from_raw_parts(buf as *const u8, len) }.to_vec() })
}

fn send(s: &SocketObj, buf: u64, len: usize, to: Option<([u8; 4], u16)>) -> Res<usize> {
    let data = user_bytes(buf, len)?;
    let nonblocking = s.nonblocking.load(Ordering::Relaxed);
    let deadline = s.deadline(true);
    if s.kind == KIND_UDP {
        return send_udp(s, &data, to);
    }
    if s.state.lock().shut_write {
        return Err(WSAESHUTDOWN);
    }
    let mut done = 0;
    // A blocking send takes it all; a non-blocking one what fits now.
    let r = wait(nonblocking && done == 0, deadline, || {
        let st = s.state.lock();
        let step = match &st.conn {
            Conn::Loop { pair, server } => {
                let mut flow = pair.outgoing(*server).lock();
                if flow.abandoned {
                    return Some(Err(WSAECONNRESET));
                }
                let room = LOOP_BUFFER.saturating_sub(flow.data.len()).min(data.len() - done);
                flow.data.extend(&data[done..done + room]);
                Ok(room)
            }
            Conn::Tcp(h) => {
                let h = *h;
                net(|n| {
                    let sock = n.sockets.get_mut::<tcp::Socket>(h);
                    if !sock.may_send() {
                        return if matches!(sock.state(), tcp::State::SynSent | tcp::State::SynReceived) { Ok(0) } else { Err(WSAECONNRESET) };
                    }
                    Ok(sock.send_slice(&data[done..]).unwrap_or(0))
                })
                .and_then(|r| r)
            }
            _ => Err(WSAENOTCONN),
        };
        match step {
            Err(e) => Some(Err(e)),
            Ok(n) => {
                done += n;
                if done == data.len() || (nonblocking && done > 0) {
                    Some(Ok(()))
                } else {
                    None
                }
            }
        }
    });
    // Let it start out now rather than at the next tick.
    let _ = net(|_| ());
    match r {
        Ok(()) => Ok(done),
        Err(e) if done > 0 && e == WSAEWOULDBLOCK => Ok(done),
        Err(e) => Err(e),
    }
}

fn send_udp(s: &SocketObj, data: &[u8], to: Option<([u8; 4], u16)>) -> Res<usize> {
    let mut st = s.state.lock();
    let Some(to) = to.or(st.peer) else { return Err(WSAEDESTADDRREQ) };
    if is_loopback(to.0) {
        return Err(WSAENETUNREACH);
    }
    let h = match st.conn {
        Conn::Udp(h) => h,
        _ => {
            let port = ephemeral_port();
            let h = net(|n| {
                let h = n.sockets.add(new_udp());
                let _ = n.sockets.get_mut::<udp::Socket>(h).bind(port);
                h
            })?;
            st.conn = Conn::Udp(h);
            st.local = Some(([0; 4], port));
            h
        }
    };
    drop(st);
    net(|n| n.sockets.get_mut::<udp::Socket>(h).send_slice(data, endpoint(to)).map_err(|_| WSAENOBUFS))??;
    let _ = net(|_| ());
    Ok(data.len())
}

const MSG_PEEK: u32 = 2;

fn recv(s: &SocketObj, buf: u64, len: usize, flags: u32, from: Option<u64>) -> Res<usize> {
    if len > 0 && !ptr_ok(buf, len) {
        return Err(WSAEFAULT);
    }
    let nonblocking = s.nonblocking.load(Ordering::Relaxed);
    let deadline = s.deadline(false);
    let mut got = vec![0u8; len.min(1 << 20)];
    let mut peer = None;
    let n = wait(nonblocking, deadline, || {
        let st = s.state.lock();
        if st.shut_read {
            return Some(Ok(0));
        }
        match &st.conn {
            Conn::Loop { pair, server } => {
                let mut flow = pair.incoming(*server).lock();
                if flow.data.is_empty() {
                    return flow.finished.then_some(Ok(0));
                }
                let n = flow.data.len().min(got.len());
                for (i, b) in flow.data.iter().take(n).enumerate() {
                    got[i] = *b;
                }
                if flags & MSG_PEEK == 0 {
                    flow.data.drain(..n);
                }
                Some(Ok(n))
            }
            Conn::Tcp(h) => {
                let h = *h;
                let r = net(|n| {
                    let sock = n.sockets.get_mut::<tcp::Socket>(h);
                    if sock.can_recv() {
                        let r = if flags & MSG_PEEK != 0 { sock.peek_slice(&mut got) } else { sock.recv_slice(&mut got) };
                        return Some(Ok(r.unwrap_or(0)));
                    }
                    match sock.state() {
                        tcp::State::SynSent | tcp::State::SynReceived | tcp::State::Established | tcp::State::FinWait1 | tcp::State::FinWait2 => None,
                        // The peer sent FIN: the end of the stream.
                        tcp::State::CloseWait | tcp::State::LastAck | tcp::State::TimeWait | tcp::State::Closing => Some(Ok(0)),
                        tcp::State::Closed => Some(Err(WSAECONNRESET)),
                        tcp::State::Listen => Some(Err(WSAENOTCONN)),
                    }
                });
                match r {
                    Ok(r) => r,
                    Err(e) => Some(Err(e)),
                }
            }
            Conn::Udp(h) => {
                let h = *h;
                let r = net(|n| {
                    let sock = n.sockets.get_mut::<udp::Socket>(h);
                    if !sock.can_recv() {
                        return None;
                    }
                    Some(sock.recv_slice(&mut got).map(|(n, meta)| (n, from_endpoint(meta.endpoint))).map_err(|_| WSAENOBUFS))
                });
                match r {
                    Ok(Some(Ok((n, from)))) => {
                        peer = Some(from);
                        Some(Ok(n))
                    }
                    Ok(Some(Err(e))) => Some(Err(e)),
                    Ok(None) => None,
                    Err(e) => Some(Err(e)),
                }
            }
            Conn::Fresh if s.kind == KIND_UDP => Some(Err(WSAEINVAL)),
            _ => Some(Err(WSAENOTCONN)),
        }
    })?;
    if n > 0 {
        unsafe { core::ptr::copy_nonoverlapping(got.as_ptr(), buf as *mut u8, n) };
    }
    if let (Some(sa), Some(p)) = (from, peer.or_else(|| s.state.lock().peer)) {
        write_sockaddr(sa, p);
    }
    Ok(n)
}

fn shutdown(s: &SocketObj, how: u32) -> Res<()> {
    let mut st = s.state.lock();
    if how > 2 {
        return Err(WSAEINVAL);
    }
    if how != 1 {
        st.shut_read = true;
    }
    if how != 0 {
        st.shut_write = true;
        match &st.conn {
            Conn::Loop { pair, server } => pair.outgoing(*server).lock().finished = true,
            Conn::Tcp(h) => {
                let h = *h;
                net(|n| n.sockets.get_mut::<tcp::Socket>(h).close())?;
            }
            Conn::Fresh | Conn::Listen { .. } if s.kind == KIND_TCP => return Err(WSAENOTCONN),
            _ => {}
        }
    }
    Ok(())
}

const POLL_READ: u32 = 1;
const POLL_WRITE: u32 = 2;
const POLL_ERROR: u32 = 4;
const POLL_HUP: u32 = 8;

/// What a socket is ready for now: POLL_* bits.
fn readiness(s: &SocketObj) -> u32 {
    let mut st = s.state.lock();
    match &st.conn {
        Conn::Fresh => if s.kind == KIND_UDP { POLL_WRITE } else { 0 },
        Conn::Listen { waiting, pending, .. } => {
            if !pending.lock().is_empty() {
                return POLL_READ;
            }
            let waiting = waiting.clone();
            let ready = net(|n| waiting.iter().any(|&h| {
                !matches!(n.sockets.get::<tcp::Socket>(h).state(), tcp::State::Listen | tcp::State::SynReceived | tcp::State::Closed)
            }));
            if ready == Ok(true) { POLL_READ } else { 0 }
        }
        Conn::Loop { pair, server } => {
            let mut bits = 0;
            let incoming = pair.incoming(*server).lock();
            if !incoming.data.is_empty() || incoming.finished {
                bits |= POLL_READ;
            }
            if incoming.finished && incoming.data.is_empty() {
                bits |= POLL_HUP;
            }
            drop(incoming);
            let outgoing = pair.outgoing(*server).lock();
            if outgoing.abandoned {
                bits |= POLL_ERROR | POLL_HUP;
            } else if outgoing.data.len() < LOOP_BUFFER {
                bits |= POLL_WRITE;
            }
            bits
        }
        Conn::Tcp(h) => {
            let h = *h;
            let connecting = st.connecting;
            let r = net(|n| {
                let sock = n.sockets.get::<tcp::Socket>(h);
                let state = sock.state();
                let mut bits = 0;
                match state {
                    tcp::State::SynSent | tcp::State::SynReceived => {}
                    tcp::State::Closed => {
                        bits |= if connecting { POLL_ERROR } else { POLL_READ | POLL_HUP };
                    }
                    _ => {
                        if sock.can_recv() || !sock.may_recv() {
                            bits |= POLL_READ;
                        }
                        if !sock.may_recv() && !sock.can_recv() {
                            bits |= POLL_HUP;
                        }
                        if sock.can_send() {
                            bits |= POLL_WRITE;
                        }
                    }
                }
                (bits, state)
            });
            let Ok((bits, state)) = r else { return POLL_ERROR };
            if connecting {
                if state == tcp::State::Closed {
                    st.connecting = false;
                    st.error = WSAECONNREFUSED;
                } else if !matches!(state, tcp::State::SynSent | tcp::State::SynReceived) {
                    st.connecting = false;
                }
            }
            bits
        }
        Conn::Udp(h) => {
            let h = *h;
            let readable = net(|n| n.sockets.get::<udp::Socket>(h).can_recv()).unwrap_or(false);
            POLL_WRITE | if readable { POLL_READ } else { 0 }
        }
    }
}

/// Entries of { handle: u64, wanted: u32, got: u32 }; the count of entries
/// with something to report. `timeout` is milliseconds, u64::MAX for ever.
fn poll(proc: &Process, list: u64, count: usize, timeout: u64) -> Res<usize> {
    if count > 4096 || (count > 0 && !ptr_ok(list, count * 16)) {
        return Err(WSAEFAULT);
    }
    let mut sockets = Vec::with_capacity(count);
    for i in 0..count {
        let h = unsafe { *((list + i as u64 * 16) as *const u64) };
        sockets.push(socket_of(proc, h)?);
    }
    let deadline = if timeout == u64::MAX { None } else { Some(now() + timeout) };
    let found = wait(false, deadline, || {
        let mut n = 0;
        for (i, s) in sockets.iter().enumerate() {
            let p = list + i as u64 * 16;
            let wanted = unsafe { *((p + 8) as *const u32) };
            let got = readiness(s) & (wanted | POLL_ERROR | POLL_HUP);
            unsafe { *((p + 12) as *mut u32) = got };
            n += (got != 0) as usize;
        }
        (n > 0).then_some(Ok(n))
    });
    match found {
        Err(WSAETIMEDOUT) => Ok(0),
        r => r,
    }
}

fn name(s: &SocketObj, peer: bool) -> Res<([u8; 4], u16)> {
    let st = s.state.lock();
    if let Conn::Tcp(h) = st.conn {
        let h = h;
        let e = net(|n| {
            let sock = n.sockets.get::<tcp::Socket>(h);
            if peer { sock.remote_endpoint() } else { sock.local_endpoint() }
        })?;
        if let Some(e) = e {
            return Ok(from_endpoint(e));
        }
    }
    if peer {
        if matches!(st.conn, Conn::Fresh | Conn::Listen { .. }) && s.kind == KIND_TCP {
            return Err(WSAENOTCONN);
        }
        st.peer.ok_or(WSAENOTCONN)
    } else {
        st.local.ok_or(WSAEINVAL)
    }
}

// Option numbers, ws2_32's choice (not Winsock's).
const OPT_NONBLOCKING: u32 = 1;
const OPT_RECV_TIMEOUT: u32 = 2;
const OPT_SEND_TIMEOUT: u32 = 3;
const OPT_NODELAY: u32 = 4;
const OPT_KEEPALIVE: u32 = 5;
// 6-9: SO_RCVBUF, SO_SNDBUF, SO_REUSEADDR, SO_BROADCAST, kept as given.
const OPT_LINGER: u32 = 10;
const OPT_ERROR: u32 = 11;
const OPT_TYPE: u32 = 12;
const OPT_READABLE: u32 = 13;
const OPT_LISTENING: u32 = 14;

fn set_option(s: &SocketObj, opt: u32, v: u64) -> Res<()> {
    match opt {
        OPT_NONBLOCKING => s.nonblocking.store(v != 0, Ordering::Relaxed),
        OPT_RECV_TIMEOUT => s.recv_timeout.store(v as u32, Ordering::Relaxed),
        OPT_SEND_TIMEOUT => s.send_timeout.store(v as u32, Ordering::Relaxed),
        OPT_NODELAY..=OPT_LINGER => {
            s.options.lock()[(opt - OPT_NODELAY) as usize] = v;
            if let Conn::Tcp(h) = s.state.lock().conn {
                let _ = net(|n| {
                    let sock = n.sockets.get_mut::<tcp::Socket>(h);
                    match opt {
                        OPT_NODELAY => sock.set_nagle_enabled(v == 0),
                        OPT_KEEPALIVE => sock.set_keep_alive((v != 0).then(|| Duration::from_secs(60))),
                        _ => {}
                    }
                });
            }
        }
        _ => return Err(WSAEINVAL),
    }
    Ok(())
}

fn get_option(s: &SocketObj, opt: u32) -> Res<u64> {
    Ok(match opt {
        OPT_NONBLOCKING => s.nonblocking.load(Ordering::Relaxed) as u64,
        OPT_RECV_TIMEOUT => s.recv_timeout.load(Ordering::Relaxed) as u64,
        OPT_SEND_TIMEOUT => s.send_timeout.load(Ordering::Relaxed) as u64,
        OPT_NODELAY..=OPT_LINGER => s.options.lock()[(opt - OPT_NODELAY) as usize],
        OPT_ERROR => {
            let mut st = s.state.lock();
            core::mem::take(&mut st.error) as u64
        }
        OPT_TYPE => s.kind as u64,
        OPT_LISTENING => matches!(s.state.lock().conn, Conn::Listen { .. }) as u64,
        OPT_READABLE => {
            let st = s.state.lock();
            match &st.conn {
                Conn::Loop { pair, server } => pair.incoming(*server).lock().data.len() as u64,
                Conn::Tcp(h) => {
                    let h = *h;
                    net(|n| n.sockets.get::<tcp::Socket>(h).recv_queue() as u64)?
                }
                Conn::Udp(h) => {
                    let h = *h;
                    net(|n| n.sockets.get_mut::<udp::Socket>(h).peek().map(|(d, _)| d.len()).unwrap_or(0) as u64)?
                }
                _ => 0,
            }
        }
        _ => return Err(WSAEINVAL),
    })
}

/// The IPv4 addresses of a name (NUL-terminated), up to `max` of 4 bytes each.
fn resolve(name: u64, out: u64, max: usize) -> Res<usize> {
    let mut text = Vec::new();
    for i in 0..256u64 {
        if !ptr_ok(name + i, 1) {
            return Err(WSAEFAULT);
        }
        let b = unsafe { *((name + i) as *const u8) };
        if b == 0 {
            break;
        }
        text.push(b);
    }
    let Ok(text) = core::str::from_utf8(&text) else { return Err(WSAHOST_NOT_FOUND) };
    if max > 0 && !ptr_ok(out, max * 4) {
        return Err(WSAEFAULT);
    }
    let query = net(|n| {
        let dns = n.dns_socket();
        let cx = n.iface.context();
        n.sockets.get_mut::<dns::Socket>(dns).start_query(cx, text, smoltcp::wire::DnsQueryType::A).map(|q| (dns, q))
    })?;
    let Ok((dns, query)) = query else { return Err(WSAHOST_NOT_FOUND) };
    let r = wait(false, Some(now() + RESOLVE_MS), || {
        let r = net(|n| n.sockets.get_mut::<dns::Socket>(dns).get_query_result(query));
        match r {
            Err(e) => Some(Err(e)),
            Ok(Err(dns::GetQueryResultError::Pending)) => None,
            Ok(Err(dns::GetQueryResultError::Failed)) => Some(Err(WSAHOST_NOT_FOUND)),
            Ok(Ok(addrs)) => {
                let mut n = 0;
                for a in addrs.iter() {
                    if let IpAddress::Ipv4(v4) = a {
                        if n < max {
                            unsafe { core::ptr::copy_nonoverlapping(v4.octets().as_ptr(), (out + n as u64 * 4) as *mut u8, 4) };
                        }
                        n += 1;
                    }
                }
                Some(if n == 0 { Err(WSAHOST_NOT_FOUND) } else { Ok(n.min(max)) })
            }
        }
    });
    if r == Err(WSAETIMEDOUT) {
        net_quiet(|n| n.sockets.get_mut::<dns::Socket>(dns).cancel_query(query));
        return Err(WSATRY_AGAIN);
    }
    r
}
