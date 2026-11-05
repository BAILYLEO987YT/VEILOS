//! W20 (S4): TLS inside the program, as claude.exe does it: rustls over
//! std::net. Against w20srv.py (first argument: its address; on Windows
//! 127.0.0.1, from VEILOS 10.0.2.2), whose certificate the test CA signed,
//! then against a real site. Prints what shouldn't differ between machines.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;

use rustls::pki_types::{CertificateDer, ServerName};
use rustls::{ClientConfig, ClientConnection, RootCertStore};

const PORT: u16 = 47021;

fn checksum(data: &[u8]) -> u32 {
    data.iter().fold(2166136261u32, |h, &b| (h ^ b as u32).wrapping_mul(16777619))
}

fn config(roots: RootCertStore, versions: &[&'static rustls::SupportedProtocolVersion]) -> Arc<ClientConfig> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let cfg = ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(versions)
        .expect("protocol versions")
        .with_root_certificates(roots)
        .with_no_client_auth();
    Arc::new(cfg)
}

fn test_ca() -> RootCertStore {
    let mut roots = RootCertStore::empty();
    roots.add(CertificateDer::from(&include_bytes!("../../certs/ca.der")[..])).expect("test CA");
    roots
}

fn public_roots() -> RootCertStore {
    RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() }
}

struct Page {
    version: String,
    status: String,
    body: Vec<u8>,
}

/// One GET over a new connection, read to the server's close_notify.
fn fetch(cfg: Arc<ClientConfig>, addr: &str, name: &str, host: &str, path: &str) -> Result<Page, String> {
    let mut sock = TcpStream::connect(addr).map_err(|e| format!("connect: {e}"))?;
    let server_name = ServerName::try_from(name.to_string()).map_err(|e| format!("name: {e}"))?;
    let mut conn = ClientConnection::new(cfg, server_name).map_err(|e| format!("client: {e}"))?;
    let mut tls = rustls::Stream::new(&mut conn, &mut sock);
    let request = format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: w20tls\r\nConnection: close\r\n\r\n");
    tls.write_all(request.as_bytes()).map_err(|e| format!("write: {e}"))?;
    let mut response = Vec::new();
    tls.read_to_end(&mut response).map_err(|e| format!("read: {e}"))?;
    let version = format!("{:?}", conn.protocol_version().ok_or("no version")?);
    let split = response.windows(4).position(|w| w == b"\r\n\r\n").ok_or("no header end")?;
    let head = String::from_utf8_lossy(&response[..split]).to_string();
    let status = head.lines().next().unwrap_or("").to_string();
    Ok(Page { version, status, body: response[split + 4..].to_vec() })
}

fn main() {
    let server = std::env::args().nth(1).unwrap_or_else(|| "127.0.0.1".into());
    let addr = format!("{server}:{PORT}");
    let all = rustls::DEFAULT_VERSIONS;

    println!("--- the test server");
    match fetch(config(test_ca(), all), &addr, &server, "w20.test", "/hello") {
        Ok(p) => println!("hello: {} {} {:?}", p.version, p.status, String::from_utf8_lossy(&p.body)),
        Err(e) => println!("hello failed: {e}"),
    }
    match fetch(config(test_ca(), all), &addr, &server, "w20.test", "/big") {
        Ok(p) => println!("big: {} bytes, checksum {:08x}", p.body.len(), checksum(&p.body)),
        Err(e) => println!("big failed: {e}"),
    }
    match fetch(config(test_ca(), &[&rustls::version::TLS12]), &addr, &server, "w20.test", "/hello") {
        Ok(p) => println!("TLS 1.2 only: {} {}", p.version, p.status),
        Err(e) => println!("TLS 1.2 failed: {e}"),
    }
    match fetch(config(test_ca(), all), &addr, &server, "w20.test", "/nothing") {
        Ok(p) => println!("missing page: {}", p.status),
        Err(e) => println!("missing page failed: {e}"),
    }

    println!("--- certificates that must not pass");
    match fetch(config(test_ca(), all), &addr, "wrong.example", "wrong.example", "/hello") {
        Ok(_) => println!("wrong name: accepted"),
        Err(e) => println!("wrong name: {}", e.lines().next().unwrap_or("")),
    }
    match fetch(config(public_roots(), all), &addr, &server, "w20.test", "/hello") {
        Ok(_) => println!("unknown issuer: accepted"),
        Err(e) => println!("unknown issuer: {e}"),
    }

    println!("--- a real site");
    match fetch(config(public_roots(), all), "example.com:443", "example.com", "example.com", "/") {
        Ok(p) => println!("example.com: {}, says Example Domain: {}", p.status, String::from_utf8_lossy(&p.body).contains("Example Domain")),
        Err(e) => println!("example.com failed: {e}"),
    }
    println!("done");
    std::process::exit(7);
}
