//! VEILOS `ws2_32.dll`: Winsock (WIN32_DESIGN.md, S4).
//!
//! A SOCKET is a kernel handle to a socket object (src/nt_socket.rs); every
//! call here is one VEIL_SOCKET syscall plus Winsock's shapes: sockaddr,
//! fd_set, WSAPOLLFD, addrinfo, hostent. IPv4 only so far. Errors go to
//! WSASetLastError, which is SetLastError, as on Windows.

#![no_std]

extern crate alloc;

#[path = "../../dllrt.rs"]
mod dllrt;

use alloc::vec::Vec;
use nt_abi::NtStatus;

type Socket = u64;
type Bool = i32;

const INVALID_SOCKET: Socket = u64::MAX;
const SOCKET_ERROR: i32 = -1;

#[link(name = "ntdll", kind = "raw-dylib")]
unsafe extern "system" {
    fn VeilSocket(op: u64, a: u64, b: u64, c: u64, d: u64, e: u64) -> NtStatus;
}

#[link(name = "kernel32", kind = "raw-dylib")]
unsafe extern "system" {
    fn SetLastError(e: u32);
    fn GetLastError() -> u32;
    fn CloseHandle(h: u64) -> Bool;
    fn SetHandleInformation(h: u64, mask: u32, flags: u32) -> Bool;
    fn CreateEventW(sa: usize, manual: Bool, initial: Bool, name: *const u16) -> u64;
    fn SetEvent(h: u64) -> Bool;
    fn ResetEvent(h: u64) -> Bool;
    fn WaitForMultipleObjectsEx(n: u32, handles: *const u64, all: Bool, ms: u32, alertable: Bool) -> u32;
    fn CreateThread(sa: usize, stack: usize, start: unsafe extern "system" fn(usize) -> u32, param: usize, flags: u32, tid: *mut u32) -> u64;
    fn Sleep(ms: u32);
}

#[no_mangle]
pub extern "system" fn DllMain(_module: usize, _reason: u32, _reserved: usize) -> i32 {
    1
}

// -----| errors |-----

const WSAEINTR: u32 = 10004;
const WSAEFAULT: u32 = 10014;
const WSAEINVAL: u32 = 10022;
const WSAENOTSOCK: u32 = 10038;
const WSAENOPROTOOPT: u32 = 10042;
const WSAEPROTONOSUPPORT: u32 = 10043;
const WSAESOCKTNOSUPPORT: u32 = 10044;
const WSAEOPNOTSUPP: u32 = 10045;
const WSAEAFNOSUPPORT: u32 = 10047;
const WSAVERNOTSUPPORTED: u32 = 10092;
const WSANOTINITIALISED: u32 = 10093;
const WSATYPE_NOT_FOUND: u32 = 10109;
const WSAHOST_NOT_FOUND: u32 = 11001;
const WSANO_DATA: u32 = 11004;

fn fail(e: u32) -> i32 {
    unsafe { SetLastError(e) };
    SOCKET_ERROR
}

/// The Winsock error behind a VEIL_SOCKET status.
fn error_of(s: NtStatus) -> u32 {
    let s = s as u32;
    if s & 0xF000_0000 == 0xE000_0000 {
        s & 0xFFFF
    } else {
        WSAENOTSOCK
    }
}

/// One VEIL_SOCKET call: Ok, or the error already set.
fn sys(op: u64, a: u64, b: u64, c: u64, d: u64, e: u64) -> Result<(), u32> {
    let s = unsafe { VeilSocket(op, a, b, c, d, e) };
    if s == 0 {
        Ok(())
    } else {
        let e = error_of(s);
        unsafe { SetLastError(e) };
        Err(e)
    }
}

fn result(r: Result<(), u32>) -> i32 {
    match r {
        Ok(()) => 0,
        Err(_) => SOCKET_ERROR,
    }
}

const OP_CREATE: u64 = 0;
const OP_BIND: u64 = 1;
const OP_CONNECT: u64 = 2;
const OP_LISTEN: u64 = 3;
const OP_ACCEPT: u64 = 4;
const OP_SEND: u64 = 5;
const OP_RECV: u64 = 6;
const OP_SENDTO: u64 = 7;
const OP_RECVFROM: u64 = 8;
const OP_SHUTDOWN: u64 = 9;
const OP_POLL: u64 = 10;
const OP_NAME: u64 = 11;
const OP_SET: u64 = 12;
const OP_GET: u64 = 13;
const OP_RESOLVE: u64 = 14;

const OPT_NONBLOCKING: u64 = 1;
const OPT_RECV_TIMEOUT: u64 = 2;
const OPT_SEND_TIMEOUT: u64 = 3;
const OPT_NODELAY: u64 = 4;
const OPT_KEEPALIVE: u64 = 5;
const OPT_RCVBUF: u64 = 6;
const OPT_SNDBUF: u64 = 7;
const OPT_REUSEADDR: u64 = 8;
const OPT_BROADCAST: u64 = 9;
const OPT_LINGER: u64 = 10;
const OPT_ERROR: u64 = 11;
const OPT_TYPE: u64 = 12;
const OPT_READABLE: u64 = 13;
const OPT_LISTENING: u64 = 14;

const POLL_READ: u32 = 1;
const POLL_WRITE: u32 = 2;
const POLL_ERROR: u32 = 4;
const POLL_HUP: u32 = 8;

fn get_opt(s: Socket, opt: u64) -> Result<u64, u32> {
    let mut v = 0u64;
    sys(OP_GET, s, opt, &mut v as *mut u64 as u64, 0, 0).map(|_| v)
}

fn set_opt(s: Socket, opt: u64, v: u64) -> Result<(), u32> {
    sys(OP_SET, s, opt, v, 0, 0)
}

// -----| start-up |-----

static STARTED: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

fn started() -> bool {
    STARTED.load(core::sync::atomic::Ordering::Relaxed) > 0
}

/// WSADATA on 64-bit Windows: the versions, two obsolete counts, the vendor
/// pointer, then the description and status strings.
#[no_mangle]
pub unsafe extern "system" fn WSAStartup(version: u16, data: *mut u8) -> i32 {
    let (major, minor) = (version & 0xFF, version >> 8);
    if major < 1 || (major == 1 && minor < 1) {
        return WSAVERNOTSUPPORTED as i32;
    }
    if data.is_null() {
        return WSAEFAULT as i32;
    }
    let granted = if major >= 2 { 0x0202 } else { version };
    unsafe {
        core::ptr::write_bytes(data, 0, 408);
        *(data as *mut u16) = granted;
        *(data.add(2) as *mut u16) = 0x0202;
        let description = b"WinSock 2.0";
        core::ptr::copy_nonoverlapping(description.as_ptr(), data.add(16), description.len());
        let running = b"Running";
        core::ptr::copy_nonoverlapping(running.as_ptr(), data.add(16 + 257), running.len());
    }
    STARTED.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    0
}

#[no_mangle]
pub extern "system" fn WSACleanup() -> i32 {
    let r = STARTED.fetch_update(core::sync::atomic::Ordering::Relaxed, core::sync::atomic::Ordering::Relaxed, |n| n.checked_sub(1));
    if r.is_err() {
        return fail(WSANOTINITIALISED);
    }
    0
}

#[no_mangle]
pub extern "system" fn WSAGetLastError() -> i32 {
    unsafe { GetLastError() as i32 }
}

#[no_mangle]
pub extern "system" fn WSASetLastError(e: i32) {
    unsafe { SetLastError(e as u32) }
}

// -----| sockets |-----

const AF_INET: i32 = 2;
const SOCK_STREAM: i32 = 1;
const SOCK_DGRAM: i32 = 2;
const IPPROTO_TCP: i32 = 6;
const IPPROTO_UDP: i32 = 17;

fn make_socket(af: i32, kind: i32, protocol: i32, inherit: bool) -> Socket {
    if !started() {
        unsafe { SetLastError(WSANOTINITIALISED) };
        return INVALID_SOCKET;
    }
    if af != AF_INET {
        unsafe { SetLastError(WSAEAFNOSUPPORT) };
        return INVALID_SOCKET;
    }
    let k = match (kind, protocol) {
        (SOCK_STREAM, 0 | IPPROTO_TCP) => 1,
        (SOCK_DGRAM, 0 | IPPROTO_UDP) => 2,
        (SOCK_STREAM | SOCK_DGRAM, _) => {
            unsafe { SetLastError(WSAEPROTONOSUPPORT) };
            return INVALID_SOCKET;
        }
        _ => {
            unsafe { SetLastError(WSAESOCKTNOSUPPORT) };
            return INVALID_SOCKET;
        }
    };
    let mut h = 0u64;
    if sys(OP_CREATE, k, &mut h as *mut u64 as u64, 0, 0, 0).is_err() {
        return INVALID_SOCKET;
    }
    if !inherit {
        unsafe { SetHandleInformation(h, 1, 0) };
    }
    h
}

#[no_mangle]
pub extern "system" fn socket(af: i32, kind: i32, protocol: i32) -> Socket {
    make_socket(af, kind, protocol, true)
}

const WSA_FLAG_NO_HANDLE_INHERIT: u32 = 0x80;

/// The protocol info, if given, says the family, type and protocol.
#[no_mangle]
pub unsafe extern "system" fn WSASocketW(af: i32, kind: i32, protocol: i32, info: *const u8, _group: u32, flags: u32) -> Socket {
    let (af, kind, protocol) = if info.is_null() {
        (af, kind, protocol)
    } else {
        // WSAPROTOCOL_INFOW: iAddressFamily at 48, iSocketType at 64, iProtocol at 68.
        unsafe { (*(info.add(48) as *const i32), *(info.add(64) as *const i32), *(info.add(68) as *const i32)) }
    };
    make_socket(af, kind, protocol, flags & WSA_FLAG_NO_HANDLE_INHERIT == 0)
}

#[no_mangle]
pub unsafe extern "system" fn WSASocketA(af: i32, kind: i32, protocol: i32, info: *const u8, group: u32, flags: u32) -> Socket {
    unsafe { WSASocketW(af, kind, protocol, info, group, flags) }
}

#[no_mangle]
pub extern "system" fn closesocket(s: Socket) -> i32 {
    if get_opt(s, OPT_TYPE).is_err() {
        return SOCKET_ERROR;
    }
    unsafe { CloseHandle(s) };
    0
}

fn check_name(name: *const u8, len: i32) -> Result<(), u32> {
    if name.is_null() || len < 16 {
        unsafe { SetLastError(WSAEFAULT) };
        return Err(WSAEFAULT);
    }
    Ok(())
}

#[no_mangle]
pub extern "system" fn bind(s: Socket, name: *const u8, len: i32) -> i32 {
    result(check_name(name, len).and_then(|_| sys(OP_BIND, s, name as u64, 0, 0, 0)))
}

#[no_mangle]
pub extern "system" fn connect(s: Socket, name: *const u8, len: i32) -> i32 {
    result(check_name(name, len).and_then(|_| sys(OP_CONNECT, s, name as u64, 0, 0, 0)))
}

#[no_mangle]
pub unsafe extern "system" fn WSAConnect(s: Socket, name: *const u8, len: i32, _caller: *const u8, _callee: *mut u8, _sqos: *const u8, _gqos: *const u8) -> i32 {
    connect(s, name, len)
}

#[no_mangle]
pub extern "system" fn listen(s: Socket, backlog: i32) -> i32 {
    result(sys(OP_LISTEN, s, backlog.max(1) as u64, 0, 0, 0))
}

#[no_mangle]
pub unsafe extern "system" fn accept(s: Socket, addr: *mut u8, len: *mut i32) -> Socket {
    if !addr.is_null() && (len.is_null() || unsafe { *len } < 16) {
        unsafe { SetLastError(WSAEFAULT) };
        return INVALID_SOCKET;
    }
    let mut h = 0u64;
    if sys(OP_ACCEPT, s, &mut h as *mut u64 as u64, addr as u64, 0, 0).is_err() {
        return INVALID_SOCKET;
    }
    if !addr.is_null() {
        unsafe { *len = 16 };
    }
    h
}

#[no_mangle]
pub unsafe extern "system" fn WSAAccept(s: Socket, addr: *mut u8, len: *mut i32, _condition: usize, _data: usize) -> Socket {
    unsafe { accept(s, addr, len) }
}

const MSG_PEEK: i32 = 2;

#[no_mangle]
pub extern "system" fn send(s: Socket, buf: *const u8, len: i32, _flags: i32) -> i32 {
    if len < 0 {
        return fail(WSAEINVAL);
    }
    let mut n = 0u64;
    match sys(OP_SEND, s, buf as u64, len as u64, &mut n as *mut u64 as u64, 0) {
        Ok(()) => n as i32,
        Err(_) => SOCKET_ERROR,
    }
}

#[no_mangle]
pub extern "system" fn recv(s: Socket, buf: *mut u8, len: i32, flags: i32) -> i32 {
    if len < 0 {
        return fail(WSAEINVAL);
    }
    let mut n = 0u64;
    match sys(OP_RECV, s, buf as u64, len as u64, &mut n as *mut u64 as u64, (flags & MSG_PEEK) as u64) {
        Ok(()) => n as i32,
        Err(_) => SOCKET_ERROR,
    }
}

#[no_mangle]
pub unsafe extern "system" fn sendto(s: Socket, buf: *const u8, len: i32, flags: i32, to: *const u8, tolen: i32) -> i32 {
    if to.is_null() {
        return send(s, buf, len, flags);
    }
    if tolen < 16 {
        return fail(WSAEFAULT);
    }
    let mut n = 0u64;
    match sys(OP_SENDTO, s, buf as u64, len.max(0) as u64, &mut n as *mut u64 as u64, to as u64) {
        Ok(()) => n as i32,
        Err(_) => SOCKET_ERROR,
    }
}

#[no_mangle]
pub unsafe extern "system" fn recvfrom(s: Socket, buf: *mut u8, len: i32, _flags: i32, from: *mut u8, fromlen: *mut i32) -> i32 {
    if !from.is_null() && (fromlen.is_null() || unsafe { *fromlen } < 16) {
        return fail(WSAEFAULT);
    }
    let mut n = 0u64;
    match sys(OP_RECVFROM, s, buf as u64, len.max(0) as u64, &mut n as *mut u64 as u64, from as u64) {
        Ok(()) => {
            if !from.is_null() {
                unsafe { *fromlen = 16 };
            }
            n as i32
        }
        Err(_) => SOCKET_ERROR,
    }
}

/// WSABUF: a length, then (8-aligned) the buffer.
unsafe fn wsabuf(bufs: *const u8, i: usize) -> (u32, *mut u8) {
    unsafe { (*(bufs.add(i * 16) as *const u32), *(bufs.add(i * 16 + 8) as *const *mut u8)) }
}

/// Overlapped I/O isn't offered yet: these complete before returning, and
/// say so through the OVERLAPPED too.
unsafe fn finish_overlapped(overlapped: *mut u8, n: u32) {
    if !overlapped.is_null() {
        unsafe {
            *(overlapped as *mut u64) = 0;
            *(overlapped.add(8) as *mut u64) = n as u64;
            let event = *(overlapped.add(24) as *const u64);
            if event != 0 {
                SetEvent(event & !1);
            }
        }
    }
}

#[no_mangle]
pub unsafe extern "system" fn WSASend(s: Socket, bufs: *const u8, count: u32, sent: *mut u32, flags: u32, overlapped: *mut u8, routine: usize) -> i32 {
    if routine != 0 {
        return fail(WSAEOPNOTSUPP);
    }
    let mut total = 0u32;
    for i in 0..count as usize {
        let (len, p) = unsafe { wsabuf(bufs, i) };
        let n = send(s, p, len as i32, flags as i32);
        if n < 0 {
            return SOCKET_ERROR;
        }
        total += n as u32;
        if (n as u32) < len {
            break;
        }
    }
    if !sent.is_null() {
        unsafe { *sent = total };
    }
    unsafe { finish_overlapped(overlapped, total) };
    0
}

#[no_mangle]
pub unsafe extern "system" fn WSARecv(s: Socket, bufs: *const u8, count: u32, got: *mut u32, flags: *mut u32, overlapped: *mut u8, routine: usize) -> i32 {
    if routine != 0 {
        return fail(WSAEOPNOTSUPP);
    }
    // One buffer at a time: a stream gives what it has into the first.
    let f = if flags.is_null() { 0 } else { (unsafe { *flags }) as i32 };
    let (len, p) = if count > 0 { unsafe { wsabuf(bufs, 0) } } else { (0, core::ptr::null_mut()) };
    let n = recv(s, p, len as i32, f);
    if n < 0 {
        return SOCKET_ERROR;
    }
    if !got.is_null() {
        unsafe { *got = n as u32 };
    }
    if !flags.is_null() {
        unsafe { *flags = 0 };
    }
    unsafe { finish_overlapped(overlapped, n as u32) };
    0
}

#[no_mangle]
pub unsafe extern "system" fn WSASendTo(s: Socket, bufs: *const u8, count: u32, sent: *mut u32, flags: u32, to: *const u8, tolen: i32, overlapped: *mut u8, routine: usize) -> i32 {
    if routine != 0 || count != 1 {
        return fail(WSAEOPNOTSUPP);
    }
    let (len, p) = unsafe { wsabuf(bufs, 0) };
    let n = unsafe { sendto(s, p, len as i32, flags as i32, to, tolen) };
    if n < 0 {
        return SOCKET_ERROR;
    }
    if !sent.is_null() {
        unsafe { *sent = n as u32 };
    }
    unsafe { finish_overlapped(overlapped, n as u32) };
    0
}

#[no_mangle]
pub unsafe extern "system" fn WSARecvFrom(s: Socket, bufs: *const u8, count: u32, got: *mut u32, flags: *mut u32, from: *mut u8, fromlen: *mut i32, overlapped: *mut u8, routine: usize) -> i32 {
    if routine != 0 || count != 1 {
        return fail(WSAEOPNOTSUPP);
    }
    let (len, p) = unsafe { wsabuf(bufs, 0) };
    let n = unsafe { recvfrom(s, p, len as i32, 0, from, fromlen) };
    if n < 0 {
        return SOCKET_ERROR;
    }
    if !got.is_null() {
        unsafe { *got = n as u32 };
    }
    if !flags.is_null() {
        unsafe { *flags = 0 };
    }
    unsafe { finish_overlapped(overlapped, n as u32) };
    0
}

#[no_mangle]
pub extern "system" fn shutdown(s: Socket, how: i32) -> i32 {
    result(sys(OP_SHUTDOWN, s, how as u64, 0, 0, 0))
}

#[no_mangle]
pub unsafe extern "system" fn getsockname(s: Socket, name: *mut u8, len: *mut i32) -> i32 {
    if name.is_null() || len.is_null() || unsafe { *len } < 16 {
        return fail(WSAEFAULT);
    }
    let r = sys(OP_NAME, s, 0, name as u64, 0, 0);
    if r.is_ok() {
        unsafe { *len = 16 };
    }
    result(r)
}

#[no_mangle]
pub unsafe extern "system" fn getpeername(s: Socket, name: *mut u8, len: *mut i32) -> i32 {
    if name.is_null() || len.is_null() || unsafe { *len } < 16 {
        return fail(WSAEFAULT);
    }
    let r = sys(OP_NAME, s, 1, name as u64, 0, 0);
    if r.is_ok() {
        unsafe { *len = 16 };
    }
    result(r)
}

// -----| options |-----

const SOL_SOCKET: i32 = 0xFFFF;
const SO_ACCEPTCONN: i32 = 2;
const SO_REUSEADDR: i32 = 4;
const SO_KEEPALIVE: i32 = 8;
const SO_BROADCAST: i32 = 0x20;
const SO_LINGER: i32 = 0x80;
const SO_DONTLINGER: i32 = !0x80;
const SO_EXCLUSIVEADDRUSE: i32 = !4;
const SO_SNDBUF: i32 = 0x1001;
const SO_RCVBUF: i32 = 0x1002;
const SO_SNDTIMEO: i32 = 0x1005;
const SO_RCVTIMEO: i32 = 0x1006;
const SO_ERROR: i32 = 0x1007;
const SO_TYPE: i32 = 0x1008;
const SO_UPDATE_ACCEPT_CONTEXT: i32 = 0x700B;
const SO_UPDATE_CONNECT_CONTEXT: i32 = 0x7010;
const TCP_NODELAY: i32 = 1;

fn read_int(val: *const u8, len: i32) -> Result<u64, u32> {
    if val.is_null() || len < 1 {
        unsafe { SetLastError(WSAEFAULT) };
        return Err(WSAEFAULT);
    }
    Ok(unsafe {
        match len {
            1 => *val as u64,
            2 | 3 => (val as *const u16).read_unaligned() as u64,
            _ => (val as *const u32).read_unaligned() as u64,
        }
    })
}

#[no_mangle]
pub extern "system" fn setsockopt(s: Socket, level: i32, name: i32, val: *const u8, len: i32) -> i32 {
    let opt = match (level, name) {
        (SOL_SOCKET, SO_RCVTIMEO) => OPT_RECV_TIMEOUT,
        (SOL_SOCKET, SO_SNDTIMEO) => OPT_SEND_TIMEOUT,
        (SOL_SOCKET, SO_KEEPALIVE) => OPT_KEEPALIVE,
        (SOL_SOCKET, SO_RCVBUF) => OPT_RCVBUF,
        (SOL_SOCKET, SO_SNDBUF) => OPT_SNDBUF,
        (SOL_SOCKET, SO_REUSEADDR) => OPT_REUSEADDR,
        (SOL_SOCKET, SO_BROADCAST) => OPT_BROADCAST,
        (SOL_SOCKET, SO_LINGER) => {
            // struct linger { u_short on; u_short seconds; }
            if val.is_null() || len < 4 {
                return fail(WSAEFAULT);
            }
            let on = unsafe { *(val as *const u16) } as u64;
            return result(set_opt(s, OPT_LINGER, on));
        }
        (SOL_SOCKET, SO_DONTLINGER) => {
            let v = match read_int(val, len) {
                Ok(v) => v,
                Err(_) => return SOCKET_ERROR,
            };
            return result(set_opt(s, OPT_LINGER, (v == 0) as u64));
        }
        // Accepted, with nothing to change here.
        (SOL_SOCKET, SO_EXCLUSIVEADDRUSE | SO_UPDATE_ACCEPT_CONTEXT | SO_UPDATE_CONNECT_CONTEXT) => {
            return result(get_opt(s, OPT_TYPE).map(|_| ()));
        }
        (IPPROTO_TCP, TCP_NODELAY) => OPT_NODELAY,
        // IPV6_V6ONLY and the like: no IPv6 sockets to change.
        (41, _) => return fail(WSAENOPROTOOPT),
        _ => return fail(WSAENOPROTOOPT),
    };
    match read_int(val, len) {
        Ok(v) => result(set_opt(s, opt, v)),
        Err(_) => SOCKET_ERROR,
    }
}

#[no_mangle]
pub unsafe extern "system" fn getsockopt(s: Socket, level: i32, name: i32, val: *mut u8, len: *mut i32) -> i32 {
    if val.is_null() || len.is_null() {
        return fail(WSAEFAULT);
    }
    let v = match (level, name) {
        (SOL_SOCKET, SO_RCVTIMEO) => get_opt(s, OPT_RECV_TIMEOUT),
        (SOL_SOCKET, SO_SNDTIMEO) => get_opt(s, OPT_SEND_TIMEOUT),
        (SOL_SOCKET, SO_KEEPALIVE) => get_opt(s, OPT_KEEPALIVE),
        (SOL_SOCKET, SO_RCVBUF) => get_opt(s, OPT_RCVBUF),
        (SOL_SOCKET, SO_SNDBUF) => get_opt(s, OPT_SNDBUF),
        (SOL_SOCKET, SO_REUSEADDR) => get_opt(s, OPT_REUSEADDR),
        (SOL_SOCKET, SO_BROADCAST) => get_opt(s, OPT_BROADCAST),
        (SOL_SOCKET, SO_ERROR) => get_opt(s, OPT_ERROR),
        (SOL_SOCKET, SO_TYPE) => get_opt(s, OPT_TYPE),
        (SOL_SOCKET, SO_ACCEPTCONN) => get_opt(s, OPT_LISTENING),
        (SOL_SOCKET, SO_LINGER) => {
            if unsafe { *len } < 4 {
                return fail(WSAEFAULT);
            }
            return match get_opt(s, OPT_LINGER) {
                Ok(on) => {
                    unsafe {
                        *(val as *mut u16) = on as u16;
                        *(val.add(2) as *mut u16) = 0;
                        *len = 4;
                    }
                    0
                }
                Err(_) => SOCKET_ERROR,
            };
        }
        (IPPROTO_TCP, TCP_NODELAY) => get_opt(s, OPT_NODELAY),
        _ => return fail(WSAENOPROTOOPT),
    };
    match v {
        Ok(v) => {
            if unsafe { *len } < 1 {
                return fail(WSAEFAULT);
            }
            unsafe {
                if *len == 1 {
                    *val = v as u8;
                } else if *len < 4 {
                    (val as *mut u16).write_unaligned(v as u16);
                    *len = 2;
                } else {
                    (val as *mut u32).write_unaligned(v as u32);
                    *len = 4;
                }
            }
            0
        }
        Err(_) => SOCKET_ERROR,
    }
}

const FIONBIO: i32 = 0x8004_667Eu32 as i32;
const FIONREAD: i32 = 0x4004_667F;

#[no_mangle]
pub unsafe extern "system" fn ioctlsocket(s: Socket, cmd: i32, arg: *mut u32) -> i32 {
    if arg.is_null() {
        return fail(WSAEFAULT);
    }
    match cmd {
        FIONBIO => result(set_opt(s, OPT_NONBLOCKING, unsafe { *arg } as u64)),
        FIONREAD => match get_opt(s, OPT_READABLE) {
            Ok(n) => {
                unsafe { *arg = n as u32 };
                0
            }
            Err(_) => SOCKET_ERROR,
        },
        _ => fail(WSAEINVAL),
    }
}

/// FIONBIO and FIONREAD as WSAIoctl. Extension functions (AcceptEx,
/// ConnectEx) aren't offered yet: asking for them fails, saying so.
#[no_mangle]
pub unsafe extern "system" fn WSAIoctl(s: Socket, code: u32, input: *const u8, in_len: u32, output: *mut u8, out_len: u32, returned: *mut u32, _overlapped: *mut u8, _routine: usize) -> i32 {
    match code as i32 {
        FIONBIO if in_len >= 4 && !input.is_null() => {
            let mut v = unsafe { *(input as *const u32) };
            unsafe { ioctlsocket(s, FIONBIO, &mut v) }
        }
        FIONREAD if out_len >= 4 && !output.is_null() => {
            let r = unsafe { ioctlsocket(s, FIONREAD, output as *mut u32) };
            if r == 0 && !returned.is_null() {
                unsafe { *returned = 4 };
            }
            r
        }
        _ => fail(WSAEOPNOTSUPP),
    }
}

// -----| waiting for sockets |-----

/// One kernel poll entry: the handle, what's wanted, what's ready.
#[repr(C)]
#[derive(Clone, Copy)]
struct Entry {
    handle: u64,
    wanted: u32,
    got: u32,
}

/// Wait on `list`: the number of entries with something to report.
fn kernel_poll(list: &mut [Entry], timeout_ms: u64) -> Result<usize, u32> {
    let mut n = 0u64;
    sys(OP_POLL, list.as_mut_ptr() as u64, list.len() as u64, timeout_ms, &mut n as *mut u64 as u64, 0).map(|_| n as usize)
}

/// fd_set on 64-bit Windows: a 32-bit count, then (8-aligned) the sockets.
unsafe fn fd_set_items(set: *const u8) -> &'static [u64] {
    if set.is_null() {
        return &[];
    }
    let n = unsafe { *(set as *const u32) }.min(1024) as usize;
    unsafe { core::slice::from_raw_parts(set.add(8) as *const u64, n) }
}

unsafe fn fd_set_keep(set: *mut u8, keep: impl Fn(u64) -> bool) -> usize {
    if set.is_null() {
        return 0;
    }
    let items: Vec<u64> = unsafe { fd_set_items(set) }.to_vec();
    let mut n = 0;
    for s in items {
        if keep(s) {
            unsafe { *(set.add(8 + n * 8) as *mut u64) = s };
            n += 1;
        }
    }
    unsafe { *(set as *mut u32) = n as u32 };
    n
}

#[no_mangle]
pub unsafe extern "system" fn select(_nfds: i32, read: *mut u8, write: *mut u8, except: *mut u8, timeout: *const i32) -> i32 {
    let mut list: Vec<Entry> = Vec::new();
    let mut add = |s: u64, bit: u32| match list.iter_mut().find(|e| e.handle == s) {
        Some(e) => e.wanted |= bit,
        None => list.push(Entry { handle: s, wanted: bit, got: 0 }),
    };
    for &s in unsafe { fd_set_items(read) } {
        add(s, POLL_READ);
    }
    for &s in unsafe { fd_set_items(write) } {
        add(s, POLL_WRITE);
    }
    for &s in unsafe { fd_set_items(except) } {
        add(s, POLL_ERROR);
    }
    if list.is_empty() {
        return fail(WSAEINVAL);
    }
    // struct timeval { long tv_sec; long tv_usec; }
    let ms = if timeout.is_null() {
        u64::MAX
    } else {
        unsafe { (*timeout).max(0) as u64 * 1000 + (*timeout.add(1)).max(0) as u64 / 1000 }
    };
    if kernel_poll(&mut list, ms).is_err() {
        return SOCKET_ERROR;
    }
    let got = |s: u64| list.iter().find(|e| e.handle == s).map_or(0, |e| e.got);
    // A closed connection reads (as 0 bytes); a failed connect is an exception.
    let mut n = unsafe { fd_set_keep(read, |s| got(s) & (POLL_READ | POLL_HUP) != 0) };
    n += unsafe { fd_set_keep(write, |s| got(s) & POLL_WRITE != 0) };
    n += unsafe { fd_set_keep(except, |s| got(s) & POLL_ERROR != 0) };
    n as i32
}

#[no_mangle]
pub unsafe extern "system" fn __WSAFDIsSet(s: Socket, set: *const u8) -> i32 {
    unsafe { fd_set_items(set) }.contains(&s) as i32
}

const POLLRDNORM: i16 = 0x0100;
const POLLRDBAND: i16 = 0x0200;
const POLLWRNORM: i16 = 0x0010;
const POLLERR: i16 = 0x0001;
const POLLHUP: i16 = 0x0002;
const POLLNVAL: i16 = 0x0004;

/// WSAPOLLFD: the socket, then events and revents (16-bit each).
#[no_mangle]
pub unsafe extern "system" fn WSAPoll(fds: *mut u8, count: u32, timeout: i32) -> i32 {
    if fds.is_null() && count > 0 {
        return fail(WSAEFAULT);
    }
    let mut list = Vec::new();
    let mut slots = Vec::new();
    for i in 0..count as usize {
        let p = unsafe { fds.add(i * 16) };
        let s = unsafe { *(p as *const u64) };
        unsafe { *(p.add(10) as *mut i16) = 0 };
        if s == INVALID_SOCKET || (s as i64) < 0 {
            continue;
        }
        if get_opt(s, OPT_TYPE).is_err() {
            unsafe { *(p.add(10) as *mut i16) = POLLNVAL };
            continue;
        }
        let events = unsafe { *(p.add(8) as *const i16) };
        let mut wanted = 0;
        if events & (POLLRDNORM | POLLRDBAND) != 0 {
            wanted |= POLL_READ;
        }
        if events & POLLWRNORM != 0 {
            wanted |= POLL_WRITE;
        }
        list.push(Entry { handle: s, wanted, got: 0 });
        slots.push(p);
    }
    let invalid = (0..count as usize).filter(|&i| unsafe { *(fds.add(i * 16 + 10) as *const i16) } != 0).count();
    let ms = if invalid > 0 { 0 } else if timeout < 0 { u64::MAX } else { timeout as u64 };
    if !list.is_empty() && kernel_poll(&mut list, ms).is_err() {
        return SOCKET_ERROR;
    }
    let mut n = invalid;
    for (e, p) in list.iter().zip(slots) {
        let events = unsafe { *(p.add(8) as *const i16) };
        let mut r = 0i16;
        if e.got & POLL_READ != 0 {
            r |= events & (POLLRDNORM | POLLRDBAND);
        }
        if e.got & POLL_WRITE != 0 {
            r |= events & POLLWRNORM;
        }
        if e.got & POLL_ERROR != 0 {
            r |= POLLERR;
        }
        if e.got & POLL_HUP != 0 {
            r |= POLLHUP;
        }
        unsafe { *(p.add(10) as *mut i16) = r };
        n += (r != 0) as usize;
    }
    n as i32
}

// -----| events |-----

#[no_mangle]
pub extern "system" fn WSACreateEvent() -> u64 {
    unsafe { CreateEventW(0, 1, 0, core::ptr::null()) }
}

#[no_mangle]
pub extern "system" fn WSACloseEvent(h: u64) -> Bool {
    unsafe { CloseHandle(h) }
}

#[no_mangle]
pub extern "system" fn WSASetEvent(h: u64) -> Bool {
    unsafe { SetEvent(h) }
}

#[no_mangle]
pub extern "system" fn WSAResetEvent(h: u64) -> Bool {
    unsafe { ResetEvent(h) }
}

#[no_mangle]
pub unsafe extern "system" fn WSAWaitForMultipleEvents(n: u32, events: *const u64, all: Bool, ms: u32, alertable: Bool) -> u32 {
    unsafe { WaitForMultipleObjectsEx(n, events, all, ms, alertable) }
}

// -----| network events |-----
//
// WSAEventSelect ties a socket to an event object: the event is set when the
// socket has something the mask asks about. A watcher thread, started on first
// use, waits on all such sockets at once and sets their events; once it has set
// one it leaves that socket alone until WSAEnumNetworkEvents says what happened
// (curl's waits are built on this). What's reported is what's true when asked:
// FD_READ while there's data, FD_WRITE while there's room, FD_CONNECT once.

const FD_READ: i32 = 1;
const FD_WRITE: i32 = 2;
const FD_ACCEPT: i32 = 8;
const FD_CONNECT: i32 = 16;
const FD_CLOSE: i32 = 32;

#[derive(Clone, Copy)]
struct Watch {
    socket: Socket,
    event: u64,
    mask: i32,
    /// Its event is set and nobody has asked yet.
    signalled: bool,
    connect_told: bool,
}

static WATCHES: dllrt::Lock<Vec<Watch>> = dllrt::Lock::new(Vec::new());
static WATCHER: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

fn wanted(mask: i32) -> u32 {
    let mut w = 0;
    if mask & (FD_READ | FD_ACCEPT | FD_CLOSE) != 0 {
        w |= POLL_READ;
    }
    if mask & (FD_WRITE | FD_CONNECT) != 0 {
        w |= POLL_WRITE;
    }
    w
}

unsafe extern "system" fn watcher(_: usize) -> u32 {
    loop {
        let mut list: Vec<Entry> = WATCHES.lock().iter().filter(|w| !w.signalled && w.mask != 0).map(|w| Entry { handle: w.socket, wanted: wanted(w.mask), got: 0 }).collect();
        if list.is_empty() {
            unsafe { Sleep(20) };
            continue;
        }
        // Short waits: sockets come and go while this one is out.
        if kernel_poll(&mut list, 50).is_err() {
            // A socket closed under it: drop those that are gone.
            WATCHES.lock().retain(|w| get_opt(w.socket, OPT_TYPE).is_ok());
            continue;
        }
        let mut watches = WATCHES.lock();
        for e in list.iter().filter(|e| e.got != 0) {
            if let Some(w) = watches.iter_mut().find(|w| w.socket == e.handle && !w.signalled) {
                w.signalled = true;
                unsafe { SetEvent(w.event) };
            }
        }
    }
}

#[no_mangle]
pub extern "system" fn WSAEventSelect(s: Socket, event: u64, mask: i32) -> i32 {
    if get_opt(s, OPT_TYPE).is_err() {
        return SOCKET_ERROR;
    }
    // As on Windows, the socket becomes non-blocking.
    if mask != 0 && set_opt(s, OPT_NONBLOCKING, 1).is_err() {
        return SOCKET_ERROR;
    }
    {
        let mut watches = WATCHES.lock();
        watches.retain(|w| w.socket != s);
        if mask != 0 {
            watches.push(Watch { socket: s, event, mask, signalled: false, connect_told: false });
        }
    }
    if mask != 0 && !WATCHER.swap(true, core::sync::atomic::Ordering::AcqRel) {
        unsafe { CreateThread(0, 0, watcher, 0, 0, core::ptr::null_mut()) };
    }
    0
}

/// WSANETWORKEVENTS: the events, then an error for each (FD_*_BIT order).
#[no_mangle]
pub unsafe extern "system" fn WSAEnumNetworkEvents(s: Socket, event: u64, out: *mut u8) -> i32 {
    if out.is_null() {
        return fail(WSAEFAULT);
    }
    let Some(mut w) = WATCHES.lock().iter().copied().find(|w| w.socket == s) else {
        if get_opt(s, OPT_TYPE).is_err() {
            return SOCKET_ERROR;
        }
        unsafe { core::ptr::write_bytes(out, 0, 44) };
        if event != 0 {
            unsafe { ResetEvent(event) };
        }
        return 0;
    };
    let mut list = [Entry { handle: s, wanted: wanted(w.mask), got: 0 }];
    if kernel_poll(&mut list, 0).is_err() {
        return SOCKET_ERROR;
    }
    let got = list[0].got;
    let listening = get_opt(s, OPT_LISTENING).unwrap_or(0) != 0;
    let mut events = 0i32;
    let mut errors = [0i32; 10];
    if listening {
        if w.mask & FD_ACCEPT != 0 && got & POLL_READ != 0 {
            events |= FD_ACCEPT;
        }
    } else {
        if w.mask & FD_CONNECT != 0 && !w.connect_told && got & (POLL_WRITE | POLL_ERROR) != 0 {
            events |= FD_CONNECT;
            w.connect_told = true;
            if got & POLL_ERROR != 0 {
                errors[4] = get_opt(s, OPT_ERROR).unwrap_or(0) as i32;
            }
        }
        if w.mask & FD_READ != 0 && got & POLL_READ != 0 && got & POLL_HUP == 0 {
            events |= FD_READ;
        }
        if w.mask & FD_WRITE != 0 && got & POLL_WRITE != 0 {
            events |= FD_WRITE;
        }
        if w.mask & FD_CLOSE != 0 && got & POLL_HUP != 0 {
            events |= FD_CLOSE;
            // Data still waiting is read before the close is seen.
            if w.mask & FD_READ != 0 && get_opt(s, OPT_READABLE).unwrap_or(0) > 0 {
                events |= FD_READ;
            }
        }
    }
    unsafe {
        *(out as *mut i32) = events;
        core::ptr::copy_nonoverlapping(errors.as_ptr(), out.add(4) as *mut i32, 10);
    }
    w.signalled = false;
    if let Some(slot) = WATCHES.lock().iter_mut().find(|x| x.socket == s) {
        *slot = w;
    }
    if event != 0 {
        unsafe { ResetEvent(event) };
    }
    0
}

// -----| byte order |-----

#[no_mangle]
pub extern "system" fn htons(v: u16) -> u16 {
    v.to_be()
}

#[no_mangle]
pub extern "system" fn ntohs(v: u16) -> u16 {
    u16::from_be(v)
}

#[no_mangle]
pub extern "system" fn htonl(v: u32) -> u32 {
    v.to_be()
}

#[no_mangle]
pub extern "system" fn ntohl(v: u32) -> u32 {
    u32::from_be(v)
}

// -----| addresses as text |-----

/// "a.b.c.d", each part 0-255; nothing else.
fn parse_ipv4(text: &[u8]) -> Option<[u8; 4]> {
    let mut out = [0u8; 4];
    let mut parts = 0;
    for part in text.split(|&c| c == b'.') {
        if parts == 4 || part.is_empty() || part.len() > 3 || !part.iter().all(u8::is_ascii_digit) {
            return None;
        }
        let v = part.iter().fold(0u32, |a, &d| a * 10 + (d - b'0') as u32);
        if v > 255 {
            return None;
        }
        out[parts] = v as u8;
        parts += 1;
    }
    (parts == 4).then_some(out)
}

fn format_ipv4(ip: [u8; 4], out: &mut [u8; 16]) -> usize {
    let mut n = 0;
    for (i, b) in ip.iter().enumerate() {
        if i > 0 {
            out[n] = b'.';
            n += 1;
        }
        let mut digits = [0u8; 3];
        let mut k = 0;
        let mut v = *b;
        loop {
            digits[k] = b'0' + v % 10;
            k += 1;
            v /= 10;
            if v == 0 {
                break;
            }
        }
        while k > 0 {
            k -= 1;
            out[n] = digits[k];
            n += 1;
        }
    }
    n
}

unsafe fn c_str<'a>(p: *const u8) -> &'a [u8] {
    if p.is_null() {
        return &[];
    }
    let mut n = 0;
    while unsafe { *p.add(n) } != 0 {
        n += 1;
    }
    unsafe { core::slice::from_raw_parts(p, n) }
}

/// A wide string as bytes (characters past 0x7F become '?').
unsafe fn w_str(p: *const u16) -> Vec<u8> {
    let mut v = Vec::new();
    if p.is_null() {
        return v;
    }
    let mut i = 0;
    loop {
        let c = unsafe { *p.add(i) };
        if c == 0 {
            return v;
        }
        v.push(if c < 0x80 { c as u8 } else { b'?' });
        i += 1;
    }
}

#[no_mangle]
pub unsafe extern "system" fn inet_addr(text: *const u8) -> u32 {
    match parse_ipv4(unsafe { c_str(text) }) {
        Some(ip) => u32::from_ne_bytes(ip),
        None => u32::MAX,
    }
}

static mut NTOA: [u8; 16] = [0; 16];

#[no_mangle]
pub extern "system" fn inet_ntoa(addr: u32) -> *const u8 {
    let mut out = [0u8; 16];
    let n = format_ipv4(addr.to_ne_bytes(), &mut out);
    unsafe {
        let buf = &mut *(&raw mut NTOA);
        buf[..n].copy_from_slice(&out[..n]);
        buf[n] = 0;
        buf.as_ptr()
    }
}

#[no_mangle]
pub unsafe extern "system" fn inet_pton(family: i32, text: *const u8, out: *mut u8) -> i32 {
    if family != AF_INET {
        unsafe { SetLastError(WSAEAFNOSUPPORT) };
        return -1;
    }
    match parse_ipv4(unsafe { c_str(text) }) {
        Some(ip) => {
            unsafe { core::ptr::copy_nonoverlapping(ip.as_ptr(), out, 4) };
            1
        }
        None => 0,
    }
}

#[no_mangle]
pub unsafe extern "system" fn InetPtonW(family: i32, text: *const u16, out: *mut u8) -> i32 {
    let mut t = unsafe { w_str(text) };
    t.push(0);
    unsafe { inet_pton(family, t.as_ptr(), out) }
}

#[no_mangle]
pub unsafe extern "system" fn inet_ntop(family: i32, addr: *const u8, out: *mut u8, size: usize) -> *const u8 {
    if family != AF_INET {
        unsafe { SetLastError(WSAEAFNOSUPPORT) };
        return core::ptr::null();
    }
    let mut text = [0u8; 16];
    let n = format_ipv4(unsafe { *(addr as *const [u8; 4]) }, &mut text);
    if size <= n {
        unsafe { SetLastError(WSAEINVAL) };
        return core::ptr::null();
    }
    unsafe {
        core::ptr::copy_nonoverlapping(text.as_ptr(), out, n);
        *out.add(n) = 0;
    }
    out
}

#[no_mangle]
pub unsafe extern "system" fn InetNtopW(family: i32, addr: *const u8, out: *mut u16, size: usize) -> *const u16 {
    let mut narrow = [0u8; 16];
    if unsafe { inet_ntop(family, addr, narrow.as_mut_ptr(), size.min(16)) }.is_null() {
        return core::ptr::null();
    }
    let n = narrow.iter().position(|&c| c == 0).unwrap_or(0);
    for i in 0..=n {
        unsafe { *out.add(i) = narrow[i] as u16 };
    }
    out
}

// -----| names |-----

/// The addresses of a name: numeric, "localhost", or from DNS.
fn lookup(name: &[u8]) -> Result<Vec<[u8; 4]>, u32> {
    if let Some(ip) = parse_ipv4(name) {
        return Ok(alloc::vec![ip]);
    }
    if name.eq_ignore_ascii_case(b"localhost") {
        return Ok(alloc::vec![[127, 0, 0, 1]]);
    }
    if name.is_empty() || name.len() > 253 {
        return Err(WSAHOST_NOT_FOUND);
    }
    let mut text = name.to_vec();
    text.push(0);
    let mut addrs = [[0u8; 4]; 4];
    let mut n = 0u64;
    let s = unsafe { VeilSocket(OP_RESOLVE, text.as_ptr() as u64, addrs.as_mut_ptr() as u64, 4, &mut n as *mut u64 as u64, 0) };
    if s != 0 {
        return Err(error_of(s));
    }
    Ok(addrs[..n as usize].to_vec())
}

fn service_port(service: &[u8]) -> Result<u16, u32> {
    if service.is_empty() {
        return Ok(0);
    }
    if service.iter().all(u8::is_ascii_digit) && service.len() <= 5 {
        let v = service.iter().fold(0u32, |a, &d| a * 10 + (d - b'0') as u32);
        return u16::try_from(v).map_err(|_| WSATYPE_NOT_FOUND);
    }
    const KNOWN: &[(&[u8], u16)] = &[(b"http", 80), (b"https", 443), (b"ftp", 21), (b"ssh", 22), (b"smtp", 25), (b"domain", 53), (b"git", 9418), (b"ntp", 123)];
    KNOWN.iter().find(|(n, _)| n.eq_ignore_ascii_case(service)).map(|&(_, p)| p).ok_or(WSATYPE_NOT_FOUND)
}

const AI_PASSIVE: i32 = 1;
const AI_CANONNAME: i32 = 2;
const AI_NUMERICHOST: i32 = 4;

/// addrinfo and its sockaddr, in one block: flags, family, socktype,
/// protocol, addrlen (8 bytes), canonname, addr, next; the sockaddr at 48.
fn addrinfo_chain(ips: &[[u8; 4]], port: u16, hints: [i32; 4], canon: Option<&[u8]>, wide: bool) -> *mut u8 {
    let mut head: *mut u8 = core::ptr::null_mut();
    for (i, ip) in ips.iter().enumerate().rev() {
        let block = alloc::boxed::Box::into_raw(alloc::boxed::Box::new([0u8; 64])) as *mut u8;
        unsafe {
            let [flags, _family, socktype, protocol] = hints;
            *(block as *mut i32) = flags;
            *(block.add(4) as *mut i32) = AF_INET;
            *(block.add(8) as *mut i32) = socktype;
            *(block.add(12) as *mut i32) = if protocol != 0 {
                protocol
            } else {
                match socktype {
                    SOCK_STREAM => IPPROTO_TCP,
                    SOCK_DGRAM => IPPROTO_UDP,
                    _ => 0,
                }
            };
            *(block.add(16) as *mut u64) = 16;
            let sa = block.add(48);
            *(sa as *mut u16) = AF_INET as u16;
            *(sa.add(2) as *mut u16) = port.to_be();
            core::ptr::copy_nonoverlapping(ip.as_ptr(), sa.add(4), 4);
            *(block.add(32) as *mut u64) = sa as u64;
            *(block.add(40) as *mut u64) = head as u64;
            if let (0, Some(name)) = (i, canon) {
                let text = if wide {
                    let w: Vec<u16> = name.iter().map(|&c| c as u16).chain(core::iter::once(0)).collect();
                    alloc::boxed::Box::into_raw(w.into_boxed_slice()) as *mut u16 as *mut u8
                } else {
                    let v: Vec<u8> = name.iter().copied().chain(core::iter::once(0)).collect();
                    alloc::boxed::Box::into_raw(v.into_boxed_slice()) as *mut u8
                };
                *(block.add(24) as *mut u64) = text as u64;
                // Its length, for freeing, in the unused 8 bytes after the sockaddr.
                *(block.add(56) as *mut u64) = name.len() as u64 + 1;
            }
        }
        head = block;
    }
    head
}

unsafe fn free_chain(mut p: *mut u8, wide: bool) {
    while !p.is_null() {
        unsafe {
            let next = *(p.add(40) as *const u64) as *mut u8;
            let canon = *(p.add(24) as *const u64);
            if canon != 0 {
                let n = *(p.add(56) as *const u64) as usize;
                if wide {
                    drop(alloc::boxed::Box::from_raw(core::ptr::slice_from_raw_parts_mut(canon as *mut u16, n)));
                } else {
                    drop(alloc::boxed::Box::from_raw(core::ptr::slice_from_raw_parts_mut(canon as *mut u8, n)));
                }
            }
            drop(alloc::boxed::Box::from_raw(p as *mut [u8; 64]));
            p = next;
        }
    }
}

fn get_addr_info(node: Option<&[u8]>, service: &[u8], hints: *const u8, out: *mut *mut u8, wide: bool) -> i32 {
    if out.is_null() {
        unsafe { SetLastError(WSAEINVAL) };
        return WSAEINVAL as i32;
    }
    unsafe { *out = core::ptr::null_mut() };
    let h = if hints.is_null() { [0; 4] } else { unsafe { [*(hints as *const i32), *(hints.add(4) as *const i32), *(hints.add(8) as *const i32), *(hints.add(12) as *const i32)] } };
    let [flags, family, _, _] = h;
    let r = (|| {
        if family != 0 && family != AF_INET {
            return Err(if family == 23 { WSAHOST_NOT_FOUND } else { WSAEAFNOSUPPORT });
        }
        let port = service_port(service)?;
        let ips = match node {
            None | Some(&[]) => alloc::vec![if flags & AI_PASSIVE != 0 { [0u8; 4] } else { [127, 0, 0, 1] }],
            Some(name) if flags & AI_NUMERICHOST != 0 => alloc::vec![parse_ipv4(name).ok_or(WSAHOST_NOT_FOUND)?],
            Some(name) => lookup(name)?,
        };
        let canon = if flags & AI_CANONNAME != 0 { node } else { None };
        Ok(addrinfo_chain(&ips, port, h, canon, wide))
    })();
    match r {
        Ok(chain) => {
            unsafe { *out = chain };
            0
        }
        Err(e) => {
            unsafe { SetLastError(e) };
            e as i32
        }
    }
}

#[no_mangle]
pub unsafe extern "system" fn getaddrinfo(node: *const u8, service: *const u8, hints: *const u8, out: *mut *mut u8) -> i32 {
    let node = (!node.is_null()).then(|| unsafe { c_str(node) });
    get_addr_info(node, unsafe { c_str(service) }, hints, out, false)
}

#[no_mangle]
pub unsafe extern "system" fn freeaddrinfo(p: *mut u8) {
    unsafe { free_chain(p, false) }
}

#[no_mangle]
pub unsafe extern "system" fn GetAddrInfoW(node: *const u16, service: *const u16, hints: *const u8, out: *mut *mut u8) -> i32 {
    let name = unsafe { w_str(node) };
    let node = (!node.is_null()).then_some(name.as_slice());
    get_addr_info(node, unsafe { &w_str(service) }, hints, out, true)
}

#[no_mangle]
pub unsafe extern "system" fn FreeAddrInfoW(p: *mut u8) {
    unsafe { free_chain(p, true) }
}

const NI_NAMEREQD: i32 = 4;

/// Numbers only: no reverse lookups (which NI_NAMEREQD says it must have).
#[no_mangle]
pub unsafe extern "system" fn getnameinfo(sa: *const u8, len: i32, host: *mut u8, host_len: u32, serv: *mut u8, serv_len: u32, flags: i32) -> i32 {
    if sa.is_null() || len < 16 {
        unsafe { SetLastError(WSAEFAULT) };
        return WSAEFAULT as i32;
    }
    if unsafe { *(sa as *const u16) } as i32 != AF_INET {
        unsafe { SetLastError(WSAEAFNOSUPPORT) };
        return WSAEAFNOSUPPORT as i32;
    }
    if flags & NI_NAMEREQD != 0 {
        unsafe { SetLastError(WSAHOST_NOT_FOUND) };
        return WSAHOST_NOT_FOUND as i32;
    }
    if !host.is_null() && host_len > 0 {
        let mut text = [0u8; 16];
        let n = format_ipv4(unsafe { *(sa.add(4) as *const [u8; 4]) }, &mut text);
        if host_len as usize <= n {
            unsafe { SetLastError(WSAEFAULT) };
            return WSAEFAULT as i32;
        }
        unsafe {
            core::ptr::copy_nonoverlapping(text.as_ptr(), host, n);
            *host.add(n) = 0;
        }
    }
    if !serv.is_null() && serv_len > 0 {
        let port = u16::from_be(unsafe { *(sa.add(2) as *const u16) });
        let mut digits = [0u8; 6];
        let mut k = 0;
        let mut v = port;
        loop {
            digits[k] = b'0' + (v % 10) as u8;
            k += 1;
            v /= 10;
            if v == 0 {
                break;
            }
        }
        if serv_len as usize <= k {
            unsafe { SetLastError(WSAEFAULT) };
            return WSAEFAULT as i32;
        }
        for i in 0..k {
            unsafe { *serv.add(i) = digits[k - 1 - i] };
        }
        unsafe { *serv.add(k) = 0 };
    }
    0
}

#[no_mangle]
pub unsafe extern "system" fn GetNameInfoW(sa: *const u8, len: i32, host: *mut u16, host_len: u32, serv: *mut u16, serv_len: u32, flags: i32) -> i32 {
    let mut h = [0u8; 64];
    let mut s = [0u8; 16];
    let r = unsafe { getnameinfo(sa, len, h.as_mut_ptr(), host_len.min(64), s.as_mut_ptr(), serv_len.min(16), flags) };
    if r != 0 {
        return r;
    }
    for (narrow, wide, n) in [(&h[..], host, host_len), (&s[..], serv, serv_len)] {
        if !wide.is_null() && n > 0 {
            let end = narrow.iter().position(|&c| c == 0).unwrap_or(0);
            for i in 0..=end {
                unsafe { *wide.add(i) = narrow[i] as u16 };
            }
        }
    }
    0
}

/// The one hostent gethostbyname answers with (as on Windows, per thread
/// there; here, per process).
static mut HOSTENT: [u64; 4] = [0; 4];
static mut HOST_NAME: [u8; 256] = [0; 256];
static mut HOST_ADDRS: [[u8; 4]; 4] = [[0; 4]; 4];
static mut HOST_LIST: [u64; 5] = [0; 5];
static mut HOST_ALIASES: [u64; 1] = [0];

#[no_mangle]
pub unsafe extern "system" fn gethostbyname(name: *const u8) -> *mut u8 {
    let name = unsafe { c_str(name) };
    let ips = match lookup(name) {
        Ok(ips) => ips,
        Err(e) => {
            unsafe { SetLastError(e) };
            return core::ptr::null_mut();
        }
    };
    unsafe {
        let n = name.len().min(255);
        let host_name = &mut *(&raw mut HOST_NAME);
        host_name[..n].copy_from_slice(&name[..n]);
        host_name[n] = 0;
        let addrs = &mut *(&raw mut HOST_ADDRS);
        let list = &mut *(&raw mut HOST_LIST);
        for (i, ip) in ips.iter().take(4).enumerate() {
            addrs[i] = *ip;
            list[i] = addrs[i].as_ptr() as u64;
        }
        list[ips.len().min(4)] = 0;
        // hostent: name, aliases, then type and length (16 bits each), then the list.
        let h = &mut *(&raw mut HOSTENT);
        h[0] = host_name.as_ptr() as u64;
        h[1] = (&raw const HOST_ALIASES) as u64;
        h[2] = (AF_INET as u64) | (4 << 16);
        h[3] = list.as_ptr() as u64;
        h.as_mut_ptr() as *mut u8
    }
}

#[no_mangle]
pub extern "system" fn gethostbyaddr(_addr: *const u8, _len: i32, _kind: i32) -> *mut u8 {
    unsafe { SetLastError(WSANO_DATA) };
    core::ptr::null_mut()
}

#[no_mangle]
pub unsafe extern "system" fn gethostname(out: *mut u8, len: i32) -> i32 {
    if out.is_null() {
        return fail(WSAEFAULT);
    }
    // There's no computer name to ask for yet.
    let name = b"VEILOS";
    let mut buf = [0u8; 64];
    buf[..name.len()].copy_from_slice(name);
    let n = name.len() as u32;
    if len <= n as i32 {
        return fail(WSAEFAULT);
    }
    unsafe {
        core::ptr::copy_nonoverlapping(buf.as_ptr(), out, n as usize);
        *out.add(n as usize) = 0;
    }
    0
}

#[no_mangle]
pub unsafe extern "system" fn GetHostNameW(out: *mut u16, len: i32) -> i32 {
    let mut narrow = [0u8; 64];
    let r = unsafe { gethostname(narrow.as_mut_ptr(), len.min(64)) };
    if r != 0 {
        return r;
    }
    let n = narrow.iter().position(|&c| c == 0).unwrap_or(0);
    for i in 0..=n {
        unsafe { *out.add(i) = narrow[i] as u16 };
    }
    0
}

/// Services and protocols by name aren't kept: these say there's no data.
#[no_mangle]
pub extern "system" fn getservbyname(_name: *const u8, _proto: *const u8) -> *mut u8 {
    unsafe { SetLastError(WSANO_DATA) };
    core::ptr::null_mut()
}

#[no_mangle]
pub extern "system" fn getservbyport(_port: i32, _proto: *const u8) -> *mut u8 {
    unsafe { SetLastError(WSANO_DATA) };
    core::ptr::null_mut()
}

#[no_mangle]
pub extern "system" fn getprotobyname(_name: *const u8) -> *mut u8 {
    unsafe { SetLastError(WSANO_DATA) };
    core::ptr::null_mut()
}

#[no_mangle]
pub extern "system" fn getprotobynumber(_n: i32) -> *mut u8 {
    unsafe { SetLastError(WSANO_DATA) };
    core::ptr::null_mut()
}

#[no_mangle]
pub extern "system" fn WSAIsBlocking() -> Bool {
    0
}

#[no_mangle]
pub extern "system" fn WSACancelBlockingCall() -> i32 {
    fail(WSAEINTR)
}
