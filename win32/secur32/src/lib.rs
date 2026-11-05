//! VEILOS `secur32.dll`: Schannel, the TLS client behind SSPI (WIN32_DESIGN.md,
//! S4), done with rustls.
//!
//! A program drives the handshake through InitializeSecurityContext, passing
//! what it reads from the socket and sending what comes back, then turns data
//! into records and back with EncryptMessage and DecryptMessage. Schannel hands
//! out whole records only: a partial one is SEC_E_INCOMPLETE_MESSAGE, and
//! whatever follows the records it used comes back as SECBUFFER_EXTRA, which
//! is why records are fed to rustls one at a time here.
//!
//! Certificates are checked against Mozilla's roots (webpki-roots) plus any
//! DER or PEM certificates in C:\system\certs, unless the program asked to
//! check them itself (SCH_CRED_MANUAL_CRED_VALIDATION). Revocation isn't
//! checked.

#![allow(clippy::missing_safety_doc)]
// The function tables hold the functions as addresses.
#![allow(function_casts_as_integer)]

use std::io::{Read, Write};
use std::sync::{Arc, Mutex};

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, ClientConnection, DigitallySignedStruct, RootCertStore, SignatureScheme};

type Status = i32;

const SEC_E_OK: Status = 0;
const SEC_I_CONTINUE_NEEDED: Status = 0x0009_0312;
const SEC_I_CONTEXT_EXPIRED: Status = 0x0009_0317;
const SEC_I_RENEGOTIATE: Status = 0x0009_0321;
const SEC_E_INSUFFICIENT_MEMORY: Status = 0x8009_0300u32 as i32;
const SEC_E_INVALID_HANDLE: Status = 0x8009_0301u32 as i32;
const SEC_E_UNSUPPORTED_FUNCTION: Status = 0x8009_0302u32 as i32;
const SEC_E_INTERNAL_ERROR: Status = 0x8009_0304u32 as i32;
const SEC_E_SECPKG_NOT_FOUND: Status = 0x8009_0305u32 as i32;
const SEC_E_INVALID_TOKEN: Status = 0x8009_0308u32 as i32;
const SEC_E_MESSAGE_ALTERED: Status = 0x8009_030Fu32 as i32;
const SEC_E_INCOMPLETE_MESSAGE: Status = 0x8009_0318u32 as i32;
const SEC_E_BUFFER_TOO_SMALL: Status = 0x8009_0321u32 as i32;
const SEC_E_WRONG_PRINCIPAL: Status = 0x8009_0322u32 as i32;
const SEC_E_UNTRUSTED_ROOT: Status = 0x8009_0325u32 as i32;
const SEC_E_ILLEGAL_MESSAGE: Status = 0x8009_0326u32 as i32;
const SEC_E_CERT_UNKNOWN: Status = 0x8009_0327u32 as i32;
const SEC_E_CERT_EXPIRED: Status = 0x8009_0328u32 as i32;
const SEC_E_ALGORITHM_MISMATCH: Status = 0x8009_0331u32 as i32;
const SEC_E_INVALID_PARAMETER: Status = 0x8009_035Du32 as i32;

const SECBUFFER_EMPTY: u32 = 0;
const SECBUFFER_DATA: u32 = 1;
const SECBUFFER_TOKEN: u32 = 2;
const SECBUFFER_MISSING: u32 = 4;
const SECBUFFER_EXTRA: u32 = 5;
const SECBUFFER_STREAM_TRAILER: u32 = 6;
const SECBUFFER_STREAM_HEADER: u32 = 7;
const SECBUFFER_APPLICATION_PROTOCOLS: u32 = 18;
const SECBUFFER_TYPE_MASK: u32 = 0x0FFF_FFFF;

const ISC_REQ_ALLOCATE_MEMORY: u32 = 0x100;

const SCH_CRED_NO_SERVERNAME_CHECK: u32 = 0x4;
const SCH_CRED_MANUAL_CRED_VALIDATION: u32 = 0x8;
const SP_PROT_TLS1_2_CLIENT: u32 = 0x800;
const SP_PROT_TLS1_3_CLIENT: u32 = 0x2000;

/// The most a record carries, and what a record adds around its data: Schannel
/// says it in SecPkgContext_StreamSizes; the trailer is room enough for every
/// suite rustls offers (TLS 1.2 GCM puts an 8-byte nonce in front, which here
/// is simply part of the run of bytes after the header).
const MAX_MESSAGE: u32 = 16384;
const HEADER: u32 = 5;
const TRAILER: u32 = 64;

#[repr(C)]
pub struct SecHandle {
    lower: usize,
    upper: usize,
}

#[repr(C)]
pub struct SecBuffer {
    size: u32,
    kind: u32,
    data: *mut u8,
}

#[repr(C)]
pub struct SecBufferDesc {
    version: u32,
    count: u32,
    buffers: *mut SecBuffer,
}

unsafe fn buffers<'a>(desc: *mut SecBufferDesc) -> &'a mut [SecBuffer] {
    if desc.is_null() || unsafe { (*desc).buffers.is_null() } {
        return &mut [];
    }
    unsafe { std::slice::from_raw_parts_mut((*desc).buffers, (*desc).count as usize) }
}

// -----| tracing |-----

/// VEILOS_SCHANNEL_TRACE=1 in the environment: each call and what it did, on
/// stderr.
fn tracing() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VEILOS_SCHANNEL_TRACE").is_some())
}

unsafe fn describe(desc: *mut SecBufferDesc) -> String {
    unsafe { buffers(desc) }.iter().map(|b| format!("{}:{}", b.kind & SECBUFFER_TYPE_MASK, b.size)).collect::<Vec<_>>().join(" ")
}

// -----| memory handed to the program |-----

/// Blocks FreeContextBuffer gives back: their size sits in front.
fn give(bytes: &[u8]) -> *mut u8 {
    let layout = std::alloc::Layout::from_size_align(bytes.len() + 16, 16).unwrap();
    unsafe {
        let p = std::alloc::alloc(layout);
        if p.is_null() {
            return p;
        }
        *(p as *mut usize) = bytes.len();
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), p.add(16), bytes.len());
        p.add(16)
    }
}

#[no_mangle]
pub unsafe extern "system" fn FreeContextBuffer(p: *mut u8) -> Status {
    if p.is_null() {
        return SEC_E_OK;
    }
    unsafe {
        let base = p.sub(16);
        let len = *(base as *const usize);
        std::alloc::dealloc(base, std::alloc::Layout::from_size_align(len + 16, 16).unwrap());
    }
    SEC_E_OK
}

// -----| the package |-----

const PACKAGE_A: &[u8] = b"Microsoft Unified Security Protocol Provider\0";
const COMMENT_A: &[u8] = b"Schannel Security Package\0";

fn wide(s: &[u8]) -> &'static [u16] {
    Box::leak(s.iter().map(|&c| c as u16).collect::<Vec<_>>().into_boxed_slice())
}

/// SecPkgInfo: capabilities, version, RPC id, largest token, name, comment.
fn package_info(wide_names: bool) -> *mut u8 {
    let (name, comment) = if wide_names {
        (wide(PACKAGE_A).as_ptr() as usize, wide(COMMENT_A).as_ptr() as usize)
    } else {
        (PACKAGE_A.as_ptr() as usize, COMMENT_A.as_ptr() as usize)
    };
    let mut info = [0u8; 32];
    info[0..4].copy_from_slice(&0x0010_7B3Fu32.to_le_bytes());
    info[4..6].copy_from_slice(&1u16.to_le_bytes());
    info[6..8].copy_from_slice(&14u16.to_le_bytes());
    info[8..12].copy_from_slice(&24576u32.to_le_bytes());
    info[16..24].copy_from_slice(&name.to_le_bytes());
    info[24..32].copy_from_slice(&comment.to_le_bytes());
    give(&info)
}

fn known_package(name: &[u8]) -> bool {
    let n = String::from_utf8_lossy(name).to_ascii_lowercase();
    n == "microsoft unified security protocol provider" || n == "schannel" || n == "default tls ssp"
}

unsafe fn c_str<'a>(p: *const u8) -> &'a [u8] {
    if p.is_null() {
        return &[];
    }
    unsafe { std::ffi::CStr::from_ptr(p as *const _) }.to_bytes()
}

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
pub unsafe extern "system" fn QuerySecurityPackageInfoA(name: *const u8, out: *mut *mut u8) -> Status {
    if !known_package(unsafe { c_str(name) }) {
        return SEC_E_SECPKG_NOT_FOUND;
    }
    unsafe { *out = package_info(false) };
    SEC_E_OK
}

#[no_mangle]
pub unsafe extern "system" fn QuerySecurityPackageInfoW(name: *const u16, out: *mut *mut u8) -> Status {
    if !known_package(&unsafe { w_str(name) }) {
        return SEC_E_SECPKG_NOT_FOUND;
    }
    unsafe { *out = package_info(true) };
    SEC_E_OK
}

#[no_mangle]
pub unsafe extern "system" fn EnumerateSecurityPackagesA(count: *mut u32, out: *mut *mut u8) -> Status {
    unsafe {
        *count = 1;
        *out = package_info(false);
    }
    SEC_E_OK
}

#[no_mangle]
pub unsafe extern "system" fn EnumerateSecurityPackagesW(count: *mut u32, out: *mut *mut u8) -> Status {
    unsafe {
        *count = 1;
        *out = package_info(true);
    }
    SEC_E_OK
}

// -----| credentials |-----

struct Credentials {
    manual: bool,
    no_name_check: bool,
    tls12: bool,
    tls13: bool,
}

const MAGIC_CRED: usize = 0x5645_494C_4352_4544;
const MAGIC_CTX: usize = 0x5645_494C_4354_5854;

/// SCHANNEL_CRED (version 4) or SCH_CREDENTIALS (version 5): what's checked,
/// and which protocol versions.
unsafe fn read_credentials(auth: *const u8) -> Credentials {
    let mut c = Credentials { manual: false, no_name_check: false, tls12: true, tls13: true };
    if auth.is_null() {
        return c;
    }
    let version = unsafe { *(auth as *const u32) };
    let (flags, protocols) = match version {
        4 => unsafe { (*(auth.add(72) as *const u32), *(auth.add(56) as *const u32)) },
        5 => unsafe { (*(auth.add(52) as *const u32), 0) },
        _ => (0, 0),
    };
    c.manual = flags & SCH_CRED_MANUAL_CRED_VALIDATION != 0;
    c.no_name_check = flags & SCH_CRED_NO_SERVERNAME_CHECK != 0;
    if protocols & (SP_PROT_TLS1_2_CLIENT | SP_PROT_TLS1_3_CLIENT) != 0 {
        c.tls12 = protocols & SP_PROT_TLS1_2_CLIENT != 0;
        c.tls13 = protocols & SP_PROT_TLS1_3_CLIENT != 0;
    }
    c
}

unsafe fn acquire(package: bool, auth: *const u8, cred: *mut SecHandle, expiry: *mut i64) -> Status {
    if !package {
        return SEC_E_SECPKG_NOT_FOUND;
    }
    if cred.is_null() {
        return SEC_E_INVALID_PARAMETER;
    }
    let c = Box::new(unsafe { read_credentials(auth) });
    unsafe {
        (*cred).lower = Box::into_raw(c) as usize;
        (*cred).upper = MAGIC_CRED;
        if !expiry.is_null() {
            *expiry = i64::MAX;
        }
    }
    SEC_E_OK
}

#[no_mangle]
pub unsafe extern "system" fn AcquireCredentialsHandleA(_principal: *const u8, package: *const u8, _use: u32, _logon: *const u8, auth: *const u8, _get_key: usize, _arg: usize, cred: *mut SecHandle, expiry: *mut i64) -> Status {
    unsafe { acquire(known_package(c_str(package)), auth, cred, expiry) }
}

#[no_mangle]
pub unsafe extern "system" fn AcquireCredentialsHandleW(_principal: *const u16, package: *const u16, _use: u32, _logon: *const u8, auth: *const u8, _get_key: usize, _arg: usize, cred: *mut SecHandle, expiry: *mut i64) -> Status {
    unsafe { acquire(known_package(&w_str(package)), auth, cred, expiry) }
}

unsafe fn credentials<'a>(h: *const SecHandle) -> Option<&'a Credentials> {
    if h.is_null() || unsafe { (*h).upper } != MAGIC_CRED || unsafe { (*h).lower } == 0 {
        return None;
    }
    Some(unsafe { &*((*h).lower as *const Credentials) })
}

#[no_mangle]
pub unsafe extern "system" fn FreeCredentialsHandle(h: *mut SecHandle) -> Status {
    if unsafe { credentials(h) }.is_none() {
        return SEC_E_INVALID_HANDLE;
    }
    unsafe {
        drop(Box::from_raw((*h).lower as *mut Credentials));
        (*h).lower = 0;
        (*h).upper = 0;
    }
    SEC_E_OK
}

// -----| checking certificates |-----

/// Mozilla's roots, and C:\system\certs: each file a DER certificate, or PEM
/// with one or more.
fn roots() -> RootCertStore {
    let mut store = RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() };
    if let Ok(dir) = std::fs::read_dir("C:\\system\\certs") {
        for entry in dir.flatten() {
            let Ok(bytes) = std::fs::read(entry.path()) else { continue };
            for der in der_certificates(&bytes) {
                let _ = store.add(CertificateDer::from(der));
            }
        }
    }
    store
}

/// The certificates in a file: PEM blocks, or the file itself as DER.
fn der_certificates(bytes: &[u8]) -> Vec<Vec<u8>> {
    let text = String::from_utf8_lossy(bytes);
    let mut out = Vec::new();
    let mut rest = text.as_ref();
    while let Some(start) = rest.find("-----BEGIN CERTIFICATE-----") {
        let body = &rest[start + 27..];
        let Some(end) = body.find("-----END CERTIFICATE-----") else { break };
        if let Some(der) = base64(&body[..end]) {
            out.push(der);
        }
        rest = &body[end..];
    }
    if out.is_empty() && bytes.first() == Some(&0x30) {
        out.push(bytes.to_vec());
    }
    out
}

fn base64(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut acc = 0u32;
    let mut bits = 0;
    for c in text.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => break,
            b'\r' | b'\n' | b' ' | b'\t' => continue,
            _ => return None,
        } as u32;
        acc = acc << 6 | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

/// For SCH_CRED_MANUAL_CRED_VALIDATION, where the program checks the chain
/// itself afterwards, and SCH_CRED_NO_SERVERNAME_CHECK, where the chain is
/// checked but not the name. The signatures over the handshake are always
/// checked.
#[derive(Debug)]
struct Lenient {
    inner: Arc<dyn ServerCertVerifier>,
    check_chain: bool,
}

impl ServerCertVerifier for Lenient {
    fn verify_server_cert(&self, end: &CertificateDer<'_>, intermediates: &[CertificateDer<'_>], name: &ServerName<'_>, ocsp: &[u8], now: UnixTime) -> Result<ServerCertVerified, rustls::Error> {
        if !self.check_chain {
            return Ok(ServerCertVerified::assertion());
        }
        match self.inner.verify_server_cert(end, intermediates, name, ocsp, now) {
            Err(rustls::Error::InvalidCertificate(rustls::CertificateError::NotValidForName))
            | Err(rustls::Error::InvalidCertificate(rustls::CertificateError::NotValidForNameContext { .. })) => Ok(ServerCertVerified::assertion()),
            r => r,
        }
    }
    fn verify_tls12_signature(&self, message: &[u8], cert: &CertificateDer<'_>, dss: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls12_signature(message, cert, dss)
    }
    fn verify_tls13_signature(&self, message: &[u8], cert: &CertificateDer<'_>, dss: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls13_signature(message, cert, dss)
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.inner.supported_verify_schemes()
    }
}

fn config(c: &Credentials, alpn: Vec<Vec<u8>>) -> Result<Arc<ClientConfig>, Status> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut versions: Vec<&'static rustls::SupportedProtocolVersion> = Vec::new();
    if c.tls13 {
        versions.push(&rustls::version::TLS13);
    }
    if c.tls12 {
        versions.push(&rustls::version::TLS12);
    }
    let builder = ClientConfig::builder_with_provider(provider.clone()).with_protocol_versions(&versions).map_err(|_| SEC_E_ALGORITHM_MISMATCH)?;
    let webpki = rustls::client::WebPkiServerVerifier::builder_with_provider(Arc::new(roots()), provider).build().map_err(|_| SEC_E_INTERNAL_ERROR)?;
    let mut cfg = if c.manual || c.no_name_check {
        let lenient = Lenient { inner: webpki, check_chain: !c.manual };
        builder.dangerous().with_custom_certificate_verifier(Arc::new(lenient)).with_no_client_auth()
    } else {
        builder.with_webpki_verifier(webpki).with_no_client_auth()
    };
    cfg.alpn_protocols = alpn;
    Ok(Arc::new(cfg))
}

fn status_of(e: &rustls::Error) -> Status {
    use rustls::CertificateError as C;
    match e {
        rustls::Error::InvalidCertificate(c) => match c {
            C::UnknownIssuer | C::BadSignature => SEC_E_UNTRUSTED_ROOT,
            C::NotValidForName | C::NotValidForNameContext { .. } => SEC_E_WRONG_PRINCIPAL,
            C::Expired | C::ExpiredContext { .. } | C::NotValidYet | C::NotValidYetContext { .. } => SEC_E_CERT_EXPIRED,
            _ => SEC_E_CERT_UNKNOWN,
        },
        rustls::Error::PeerIncompatible(_) => SEC_E_ALGORITHM_MISMATCH,
        rustls::Error::DecryptError => SEC_E_MESSAGE_ALTERED,
        _ => SEC_E_ILLEGAL_MESSAGE,
    }
}

// -----| contexts |-----

struct Context {
    conn: ClientConnection,
    /// ApplyControlToken(SCHANNEL_SHUTDOWN): the next InitializeSecurityContext
    /// says goodbye.
    shutting_down: bool,
    closed: bool,
}

struct ContextBox(Mutex<Context>);

unsafe fn context<'a>(h: *const SecHandle) -> Option<&'a ContextBox> {
    if h.is_null() || unsafe { (*h).upper } != MAGIC_CTX || unsafe { (*h).lower } == 0 {
        return None;
    }
    Some(unsafe { &*((*h).lower as *const ContextBox) })
}

/// The length of the TLS record at the front, once it's all there.
fn record(bytes: &[u8]) -> Option<usize> {
    if bytes.len() < 5 {
        return None;
    }
    let len = 5 + u16::from_be_bytes([bytes[3], bytes[4]]) as usize;
    (bytes.len() >= len).then_some(len)
}

/// SEC_APPLICATION_PROTOCOLS: list sizes, then lists of ALPN names (each a
/// length byte and the name).
unsafe fn alpn_names(buf: &SecBuffer) -> Vec<Vec<u8>> {
    let mut names = Vec::new();
    if buf.data.is_null() || buf.size < 4 {
        return names;
    }
    let bytes = unsafe { std::slice::from_raw_parts(buf.data, buf.size as usize) };
    let total = (u32::from_le_bytes(bytes[0..4].try_into().unwrap()) as usize + 4).min(bytes.len());
    let mut at = 4;
    while at + 6 <= total {
        let ext = u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
        let size = u16::from_le_bytes([bytes[at + 4], bytes[at + 5]]) as usize;
        let list = &bytes[(at + 6).min(total)..(at + 6 + size).min(total)];
        if ext == 2 {
            let mut i = 0;
            while i < list.len() {
                let n = list[i] as usize;
                if i + 1 + n > list.len() {
                    break;
                }
                names.push(list[i + 1..i + 1 + n].to_vec());
                i += 1 + n;
            }
        }
        at += 6 + size;
    }
    names
}

/// Put `out` in the output token: allocated if the program asked for that, or
/// into the buffer it gave.
unsafe fn put_token(desc: *mut SecBufferDesc, flags: u32, out: &[u8]) -> Result<(), Status> {
    let bufs = unsafe { buffers(desc) };
    // A token buffer, or (as curl gives on the first call) an empty one that
    // becomes it.
    let i = bufs.iter().position(|b| b.kind & SECBUFFER_TYPE_MASK == SECBUFFER_TOKEN).or_else(|| bufs.iter().position(|b| b.kind & SECBUFFER_TYPE_MASK == SECBUFFER_EMPTY));
    let Some(token) = i.map(|i| &mut bufs[i]) else {
        return if out.is_empty() { Ok(()) } else { Err(SEC_E_INVALID_TOKEN) };
    };
    token.kind = SECBUFFER_TOKEN;
    if flags & ISC_REQ_ALLOCATE_MEMORY != 0 {
        if out.is_empty() {
            token.data = std::ptr::null_mut();
            token.size = 0;
        } else {
            token.data = give(out);
            if token.data.is_null() {
                return Err(SEC_E_INSUFFICIENT_MEMORY);
            }
            token.size = out.len() as u32;
        }
    } else {
        if (token.size as usize) < out.len() {
            return Err(SEC_E_BUFFER_TOO_SMALL);
        }
        if !out.is_empty() {
            unsafe { std::ptr::copy_nonoverlapping(out.as_ptr(), token.data, out.len()) };
        }
        token.size = out.len() as u32;
    }
    Ok(())
}

/// Give rustls one whole record. read_tls takes what fits in its buffer at a
/// time, so it's fed, and its packets processed, until the record is all in.
fn feed(conn: &mut ClientConnection, mut record: &[u8]) -> Result<(), rustls::Error> {
    while !record.is_empty() {
        match conn.read_tls(&mut record) {
            Ok(0) | Err(_) => return Err(rustls::Error::General("record not taken".into())),
            Ok(_) => {}
        }
        conn.process_new_packets()?;
    }
    Ok(())
}

fn pending_output(conn: &mut ClientConnection) -> Vec<u8> {
    let mut out = Vec::new();
    while conn.wants_write() {
        if conn.write_tls(&mut out).unwrap_or(0) == 0 {
            break;
        }
    }
    out
}

unsafe fn initialize(cred: *const SecHandle, ctx: *mut SecHandle, target: Vec<u8>, flags: u32, input: *mut SecBufferDesc, new_ctx: *mut SecHandle, output: *mut SecBufferDesc, attrs: *mut u32) -> Status {
    if !attrs.is_null() {
        unsafe { *attrs = flags };
    }
    let existing = unsafe { context(ctx) };
    let Some(boxed) = existing.or_else(|| {
        // The first call: the ClientHello.
        let creds = unsafe { credentials(cred) }?;
        let alpn = unsafe { buffers(input) }
            .iter()
            .find(|b| b.kind & SECBUFFER_TYPE_MASK == SECBUFFER_APPLICATION_PROTOCOLS)
            .map(|b| unsafe { alpn_names(b) })
            .unwrap_or_default();
        let cfg = config(creds, alpn).ok()?;
        let name = String::from_utf8_lossy(&target).to_string();
        let server = ServerName::try_from(if name.is_empty() { "localhost".to_string() } else { name }).ok()?;
        let conn = ClientConnection::new(cfg, server).ok()?;
        let b: &'static ContextBox = Box::leak(Box::new(ContextBox(Mutex::new(Context { conn, shutting_down: false, closed: false }))));
        unsafe {
            (*new_ctx).lower = b as *const ContextBox as usize;
            (*new_ctx).upper = MAGIC_CTX;
        }
        Some(b)
    }) else {
        return if unsafe { credentials(cred) }.is_none() && existing.is_none() { SEC_E_INVALID_HANDLE } else { SEC_E_INTERNAL_ERROR };
    };
    if existing.is_some() && !new_ctx.is_null() && new_ctx != ctx {
        unsafe {
            (*new_ctx).lower = (*ctx).lower;
            (*new_ctx).upper = (*ctx).upper;
        }
    }
    let mut c = boxed.0.lock().unwrap();
    if c.shutting_down {
        c.shutting_down = false;
        c.conn.send_close_notify();
        let out = pending_output(&mut c.conn);
        return match unsafe { put_token(output, flags, &out) } {
            Ok(()) => SEC_E_OK,
            Err(e) => e,
        };
    }

    // What the server sent: whole records, one at a time, while the handshake
    // lasts. A partial record, or anything after the handshake, stays the
    // program's (SECBUFFER_EXTRA).
    let ins = unsafe { buffers(input) };
    let mut used = 0usize;
    let mut total = 0usize;
    let mut fed_any = false;
    if let Some(tok) = ins.iter().find(|b| b.kind & SECBUFFER_TYPE_MASK == SECBUFFER_TOKEN) {
        let data: &[u8] = if tok.data.is_null() { &[] } else { unsafe { std::slice::from_raw_parts(tok.data, tok.size as usize) } };
        total = data.len();
        while c.conn.is_handshaking() {
            let Some(n) = record(&data[used..]) else { break };
            let fed = feed(&mut c.conn, &data[used..used + n]);
            used += n;
            fed_any = true;
            if let Err(e) = fed {
                // The alert for the server goes out with the error.
                let out = pending_output(&mut c.conn);
                let _ = unsafe { put_token(output, flags, &out) };
                return status_of(&e);
            }
        }
    }
    let out = pending_output(&mut c.conn);
    if let Err(e) = unsafe { put_token(output, flags, &out) } {
        return e;
    }
    let leftover = total - used;
    if c.conn.is_handshaking() && !fed_any && out.is_empty() && total > 0 {
        // Not one whole record yet: the program reads more and calls again
        // with all of it.
        if let Some(missing) = ins.iter_mut().find(|b| b.kind & SECBUFFER_TYPE_MASK == SECBUFFER_EMPTY) {
            missing.kind = SECBUFFER_MISSING;
            missing.size = 0;
        }
        return SEC_E_INCOMPLETE_MESSAGE;
    }
    if leftover > 0 {
        if let Some(extra) = ins.iter_mut().find(|b| b.kind & SECBUFFER_TYPE_MASK == SECBUFFER_EMPTY) {
            extra.kind = SECBUFFER_EXTRA;
            extra.size = leftover as u32;
            extra.data = std::ptr::null_mut();
        }
    }
    if c.conn.is_handshaking() { SEC_I_CONTINUE_NEEDED } else { SEC_E_OK }
}

#[no_mangle]
pub unsafe extern "system" fn InitializeSecurityContextA(cred: *const SecHandle, ctx: *mut SecHandle, target: *const u8, flags: u32, _reserved: u32, _rep: u32, input: *mut SecBufferDesc, _reserved2: u32, new_ctx: *mut SecHandle, output: *mut SecBufferDesc, attrs: *mut u32, expiry: *mut i64) -> Status {
    if !expiry.is_null() {
        unsafe { *expiry = i64::MAX };
    }
    let before = if tracing() { unsafe { describe(input) } } else { String::new() };
    let r = unsafe { initialize(cred, ctx, c_str(target).to_vec(), flags, input, new_ctx, output, attrs) };
    if tracing() {
        eprintln!("[schannel] ISC in [{}] -> {:08x} in [{}] out [{}]", before, r, unsafe { describe(input) }, unsafe { describe(output) });
    }
    r
}

#[no_mangle]
pub unsafe extern "system" fn InitializeSecurityContextW(cred: *const SecHandle, ctx: *mut SecHandle, target: *const u16, flags: u32, _reserved: u32, _rep: u32, input: *mut SecBufferDesc, _reserved2: u32, new_ctx: *mut SecHandle, output: *mut SecBufferDesc, attrs: *mut u32, expiry: *mut i64) -> Status {
    if !expiry.is_null() {
        unsafe { *expiry = i64::MAX };
    }
    unsafe { initialize(cred, ctx, w_str(target), flags, input, new_ctx, output, attrs) }
}

#[no_mangle]
pub unsafe extern "system" fn DeleteSecurityContext(h: *mut SecHandle) -> Status {
    if unsafe { context(h) }.is_none() {
        return SEC_E_INVALID_HANDLE;
    }
    unsafe {
        drop(Box::from_raw((*h).lower as *mut ContextBox));
        (*h).lower = 0;
        (*h).upper = 0;
    }
    SEC_E_OK
}

const SCHANNEL_SHUTDOWN: u32 = 1;

#[no_mangle]
pub unsafe extern "system" fn ApplyControlToken(h: *mut SecHandle, input: *mut SecBufferDesc) -> Status {
    let Some(b) = (unsafe { context(h) }) else { return SEC_E_INVALID_HANDLE };
    let bufs = unsafe { buffers(input) };
    let Some(token) = bufs.first() else { return SEC_E_INVALID_TOKEN };
    if token.data.is_null() || token.size < 4 {
        return SEC_E_INVALID_TOKEN;
    }
    if unsafe { *(token.data as *const u32) } != SCHANNEL_SHUTDOWN {
        return SEC_E_UNSUPPORTED_FUNCTION;
    }
    b.0.lock().unwrap().shutting_down = true;
    SEC_E_OK
}

// -----| the server's certificates |-----
//
// What SECPKG_ATTR_REMOTE_CERT_CONTEXT hands out: a CERT_CONTEXT for the
// server's certificate, whose store holds the rest of what it sent. crypt32's
// functions for walking and freeing these forward here.

/// CERT_CONTEXT: encoding, the DER and its length, CERT_INFO (zeros here:
/// programs reading the certificate parse the DER), the store.
#[repr(C)]
pub struct CertContext {
    encoding: u32,
    der: *const u8,
    der_len: u32,
    info: *const u8,
    store: *mut CertStore,
}

pub struct CertStore {
    contexts: Vec<*mut CertContext>,
    ders: Vec<Vec<u8>>,
}

static CERT_INFO_ZEROS: [u8; 256] = [0; 256];

fn make_store(chain: &[CertificateDer<'_>]) -> *mut CertStore {
    let store = Box::into_raw(Box::new(CertStore { contexts: Vec::new(), ders: chain.iter().map(|c| c.to_vec()).collect() }));
    unsafe {
        let s = &mut *store;
        for d in &s.ders {
            let ctx = Box::into_raw(Box::new(CertContext { encoding: 0x0001_0001, der: d.as_ptr(), der_len: d.len() as u32, info: CERT_INFO_ZEROS.as_ptr(), store }));
            s.contexts.push(ctx);
        }
    }
    store
}

fn store_of<'a>(ctx: *const CertContext) -> Option<&'a CertStore> {
    if ctx.is_null() {
        return None;
    }
    let store = unsafe { (*ctx).store };
    (!store.is_null()).then(|| unsafe { &*store })
}

/// The next certificate after `prev` (the first when `prev` is NULL).
#[no_mangle]
pub unsafe extern "system" fn CertEnumCertificatesInStore(store: *mut CertStore, prev: *const CertContext) -> *const CertContext {
    if store.is_null() {
        return std::ptr::null();
    }
    let s = unsafe { &*store };
    let next = match s.contexts.iter().position(|&c| c as *const _ == prev) {
        Some(i) => i + 1,
        None if prev.is_null() => 0,
        None => return std::ptr::null(),
    };
    s.contexts.get(next).map_or(std::ptr::null(), |&c| c as *const _)
}

#[no_mangle]
pub unsafe extern "system" fn CertDuplicateCertificateContext(ctx: *const CertContext) -> *const CertContext {
    ctx
}

/// The contexts belong to their store, which goes with the server
/// certificate's own context (the one a program was given).
#[no_mangle]
pub unsafe extern "system" fn CertFreeCertificateContext(ctx: *const CertContext) -> i32 {
    if let Some(s) = store_of(ctx) {
        if s.contexts.first().map(|&c| c as *const _) == Some(ctx) {
            let store = unsafe { (*ctx).store };
            unsafe {
                for &c in &(*store).contexts {
                    drop(Box::from_raw(c));
                }
                drop(Box::from_raw(store));
            }
        }
    }
    1
}

/// Stores are freed with their certificate; closing one is nothing more.
#[no_mangle]
pub unsafe extern "system" fn CertCloseStore(_store: *mut CertStore, _flags: u32) -> i32 {
    1
}

const SECPKG_ATTR_REMOTE_CERT_CONTEXT: u32 = 0x53;
const SECPKG_ATTR_STREAM_SIZES: u32 = 4;
const SECPKG_ATTR_APPLICATION_PROTOCOL: u32 = 35;
const SECPKG_ATTR_CONNECTION_INFO: u32 = 0x5A;

unsafe fn query(h: *const SecHandle, attr: u32, out: *mut u8) -> Status {
    let Some(b) = (unsafe { context(h) }) else { return SEC_E_INVALID_HANDLE };
    if out.is_null() {
        return SEC_E_INVALID_PARAMETER;
    }
    let c = b.0.lock().unwrap();
    match attr {
        SECPKG_ATTR_STREAM_SIZES => {
            let sizes = [HEADER, TRAILER, MAX_MESSAGE, 4, 1];
            unsafe { std::ptr::copy_nonoverlapping(sizes.as_ptr(), out as *mut u32, 5) };
            SEC_E_OK
        }
        // SecPkgContext_ApplicationProtocol: negotiated or not, as ALPN, then
        // the name with its length.
        SECPKG_ATTR_APPLICATION_PROTOCOL => {
            unsafe { std::ptr::write_bytes(out, 0, 264) };
            if let Some(p) = c.conn.alpn_protocol() {
                let n = p.len().min(255);
                unsafe {
                    *(out as *mut u32) = 1;
                    *(out.add(4) as *mut u32) = 2;
                    *out.add(8) = n as u8;
                    std::ptr::copy_nonoverlapping(p.as_ptr(), out.add(9), n);
                }
            }
            SEC_E_OK
        }
        // SecPkgContext_ConnectionInfo: protocol, cipher, strength, hash, key exchange.
        SECPKG_ATTR_CONNECTION_INFO => {
            let protocol = match c.conn.protocol_version() {
                Some(rustls::ProtocolVersion::TLSv1_3) => SP_PROT_TLS1_3_CLIENT,
                Some(rustls::ProtocolVersion::TLSv1_2) => SP_PROT_TLS1_2_CLIENT,
                _ => 0,
            };
            let info = [protocol, 0x6610, 256, 0x800C, 0, 0, 0xAA02, 255];
            unsafe { std::ptr::copy_nonoverlapping(info.as_ptr(), out as *mut u32, 8) };
            SEC_E_OK
        }
        SECPKG_ATTR_REMOTE_CERT_CONTEXT => match c.conn.peer_certificates() {
            Some(chain) if !chain.is_empty() => {
                let store = make_store(chain);
                unsafe { *(out as *mut *const CertContext) = (&(*store).contexts)[0] };
                SEC_E_OK
            }
            _ => SEC_E_INTERNAL_ERROR,
        },
        _ => SEC_E_UNSUPPORTED_FUNCTION,
    }
}

#[no_mangle]
pub unsafe extern "system" fn QueryContextAttributesA(h: *const SecHandle, attr: u32, out: *mut u8) -> Status {
    unsafe { query(h, attr, out) }
}

#[no_mangle]
pub unsafe extern "system" fn QueryContextAttributesW(h: *const SecHandle, attr: u32, out: *mut u8) -> Status {
    unsafe { query(h, attr, out) }
}

/// Header, data, trailer: one record made of the data, laid over all three
/// (they're one run of memory, as programs give them).
#[no_mangle]
pub unsafe extern "system" fn EncryptMessage(h: *mut SecHandle, qop: u32, desc: *mut SecBufferDesc, seq: u32) -> Status {
    let before = if tracing() { unsafe { describe(desc) } } else { String::new() };
    let r = unsafe { encrypt(h, qop, desc, seq) };
    if tracing() {
        eprintln!("[schannel] Encrypt [{}] -> {:08x} [{}]", before, r, unsafe { describe(desc) });
    }
    r
}

unsafe fn encrypt(h: *mut SecHandle, _qop: u32, desc: *mut SecBufferDesc, _seq: u32) -> Status {
    let Some(b) = (unsafe { context(h) }) else { return SEC_E_INVALID_HANDLE };
    let bufs = unsafe { buffers(desc) };
    let find = |kind: u32| bufs.iter().position(|x| x.kind & SECBUFFER_TYPE_MASK == kind);
    let (Some(hi), Some(di), Some(ti)) = (find(SECBUFFER_STREAM_HEADER), find(SECBUFFER_DATA), find(SECBUFFER_STREAM_TRAILER)) else {
        return SEC_E_INVALID_TOKEN;
    };
    let plain = unsafe { std::slice::from_raw_parts(bufs[di].data, bufs[di].size as usize) }.to_vec();
    let mut c = b.0.lock().unwrap();
    if c.conn.writer().write_all(&plain).is_err() {
        return SEC_E_INTERNAL_ERROR;
    }
    let out = pending_output(&mut c.conn);
    let room = (bufs[hi].size + bufs[di].size + bufs[ti].size) as usize;
    if out.len() > room || out.len() < HEADER as usize + plain.len() {
        return SEC_E_BUFFER_TOO_SMALL;
    }
    unsafe { std::ptr::copy_nonoverlapping(out.as_ptr(), bufs[hi].data, out.len()) };
    bufs[hi].size = HEADER;
    bufs[ti].size = (out.len() - HEADER as usize - plain.len()) as u32;
    SEC_E_OK
}

/// One record from the front of the data buffer, decrypted where it lay:
/// header, data, trailer, and whatever came after it as SECBUFFER_EXTRA.
#[no_mangle]
pub unsafe extern "system" fn DecryptMessage(h: *mut SecHandle, desc: *mut SecBufferDesc, seq: u32, qop: *mut u32) -> Status {
    let before = if tracing() { unsafe { describe(desc) } } else { String::new() };
    let r = unsafe { decrypt(h, desc, seq, qop) };
    if tracing() {
        eprintln!("[schannel] Decrypt [{}] -> {:08x} [{}]", before, r, unsafe { describe(desc) });
    }
    r
}

unsafe fn decrypt(h: *mut SecHandle, desc: *mut SecBufferDesc, _seq: u32, _qop: *mut u32) -> Status {
    let Some(b) = (unsafe { context(h) }) else { return SEC_E_INVALID_HANDLE };
    let bufs = unsafe { buffers(desc) };
    if bufs.len() < 4 {
        return SEC_E_INVALID_TOKEN;
    }
    let Some(di) = bufs.iter().position(|x| x.kind & SECBUFFER_TYPE_MASK == SECBUFFER_DATA) else { return SEC_E_INVALID_TOKEN };
    let start = bufs[di].data;
    let total = bufs[di].size as usize;
    let data = unsafe { std::slice::from_raw_parts(start, total) };
    let Some(n) = record(data) else {
        bufs[di].kind = SECBUFFER_MISSING;
        bufs[di].size = if data.len() >= 5 { (5 + u16::from_be_bytes([data[3], data[4]]) as usize - data.len()) as u32 } else { 5 - data.len() as u32 };
        return SEC_E_INCOMPLETE_MESSAGE;
    };
    let mut c = b.0.lock().unwrap();
    if let Err(e) = feed(&mut c.conn, &data[..n]) {
        return status_of(&e);
    }
    let mut plain = Vec::new();
    let mut eof = false;
    loop {
        let mut chunk = [0u8; 16384];
        match c.conn.reader().read(&mut chunk) {
            Ok(0) => {
                eof = true;
                break;
            }
            Ok(k) => plain.extend_from_slice(&chunk[..k]),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
            Err(_) => {
                eof = true;
                break;
            }
        }
    }
    if eof && plain.is_empty() {
        c.closed = true;
    }
    // In place, after the 5-byte header: never longer than what it came from.
    let plain_len = plain.len().min(n - 5);
    unsafe { std::ptr::copy_nonoverlapping(plain.as_ptr(), start.add(5), plain_len) };
    let set = |s: &mut SecBuffer, kind: u32, p: *mut u8, size: usize| {
        s.kind = kind;
        s.data = p;
        s.size = size as u32;
    };
    set(&mut bufs[0], SECBUFFER_STREAM_HEADER, start, 5);
    set(&mut bufs[1], SECBUFFER_DATA, unsafe { start.add(5) }, plain_len);
    set(&mut bufs[2], SECBUFFER_STREAM_TRAILER, unsafe { start.add(5 + plain_len) }, n - 5 - plain_len);
    if total > n {
        set(&mut bufs[3], SECBUFFER_EXTRA, unsafe { start.add(n) }, total - n);
    } else {
        set(&mut bufs[3], SECBUFFER_EMPTY, std::ptr::null_mut(), 0);
    }
    if c.closed {
        return SEC_I_CONTEXT_EXPIRED;
    }
    // Something to send back (a key update's answer): InitializeSecurityContext
    // again gives it, as after a renegotiation request.
    if c.conn.wants_write() {
        return SEC_I_RENEGOTIATE;
    }
    SEC_E_OK
}

#[allow(non_snake_case)]
fn SEC_E_DECRYPT_FAILURE() -> Status {
    0x8009_0330u32 as i32
}

// -----| what this package doesn't do |-----

unsafe extern "system" fn unsupported() -> Status {
    SEC_E_UNSUPPORTED_FUNCTION
}

#[no_mangle]
pub unsafe extern "system" fn CompleteAuthToken(_h: *mut SecHandle, _d: *mut SecBufferDesc) -> Status {
    SEC_E_UNSUPPORTED_FUNCTION
}

#[no_mangle]
pub unsafe extern "system" fn AcceptSecurityContext() -> Status {
    SEC_E_UNSUPPORTED_FUNCTION
}

#[no_mangle]
pub unsafe extern "system" fn QueryCredentialsAttributesA() -> Status {
    SEC_E_UNSUPPORTED_FUNCTION
}

#[no_mangle]
pub unsafe extern "system" fn QueryCredentialsAttributesW() -> Status {
    SEC_E_UNSUPPORTED_FUNCTION
}

#[no_mangle]
pub unsafe extern "system" fn SetContextAttributesA() -> Status {
    SEC_E_UNSUPPORTED_FUNCTION
}

#[no_mangle]
pub unsafe extern "system" fn SetContextAttributesW() -> Status {
    SEC_E_UNSUPPORTED_FUNCTION
}

// -----| the function tables |-----

/// SecurityFunctionTable (version 4): the version, then 31 entries in SSPI's
/// order. The A and W tables differ only in which of each pair they hold.
#[repr(C)]
pub struct Table {
    version: usize,
    f: [usize; 31],
}

fn table(wide: bool) -> Table {
    let u = unsupported as usize;
    let pick = |a: usize, w: usize| if wide { w } else { a };
    Table {
        version: 4,
        f: [
            pick(EnumerateSecurityPackagesA as usize, EnumerateSecurityPackagesW as usize),
            pick(QueryCredentialsAttributesA as usize, QueryCredentialsAttributesW as usize),
            pick(AcquireCredentialsHandleA as usize, AcquireCredentialsHandleW as usize),
            FreeCredentialsHandle as usize,
            u, // Reserved2
            pick(InitializeSecurityContextA as usize, InitializeSecurityContextW as usize),
            AcceptSecurityContext as usize,
            CompleteAuthToken as usize,
            DeleteSecurityContext as usize,
            ApplyControlToken as usize,
            pick(QueryContextAttributesA as usize, QueryContextAttributesW as usize),
            u, // ImpersonateSecurityContext
            u, // RevertSecurityContext
            u, // MakeSignature
            u, // VerifySignature
            FreeContextBuffer as usize,
            pick(QuerySecurityPackageInfoA as usize, QuerySecurityPackageInfoW as usize),
            u, // Reserved3
            u, // Reserved4
            u, // ExportSecurityContext
            u, // ImportSecurityContext
            u, // AddCredentials
            u, // Reserved8
            u, // QuerySecurityContextToken
            EncryptMessage as usize,
            DecryptMessage as usize,
            pick(SetContextAttributesA as usize, SetContextAttributesW as usize),
            u, // SetCredentialsAttributes
            u, // ChangeAccountPassword
            u, // QueryContextAttributesEx
            u, // QueryCredentialsAttributesEx
        ],
    }
}

static TABLE_A: std::sync::OnceLock<Table> = std::sync::OnceLock::new();
static TABLE_W: std::sync::OnceLock<Table> = std::sync::OnceLock::new();

#[no_mangle]
pub extern "system" fn InitSecurityInterfaceA() -> *const Table {
    TABLE_A.get_or_init(|| table(false))
}

#[no_mangle]
pub extern "system" fn InitSecurityInterfaceW() -> *const Table {
    TABLE_W.get_or_init(|| table(true))
}
