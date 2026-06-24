//! Shared loopback mock servers for the `sonos-control` integration tests.
//!
//! These let the tests exercise the real SSDP search, HTTP, and SOAP code paths
//! against ephemeral `127.0.0.1` ports — no Sonos hardware and no multicast.
//!
//! Each test binary includes this module but only uses a subset of the helpers,
//! so `dead_code` is allowed here by design.
#![allow(dead_code)]

use std::io::{Read, Write};
use std::net::{Shutdown, SocketAddr, SocketAddrV4, TcpListener, TcpStream, UdpSocket};
use std::thread::{self, JoinHandle};
use std::time::Duration;

/// Build a `200 OK` HTTP response with the given XML body.
pub(crate) fn http_ok(body: &str) -> String {
    http_status(200, "OK", body)
}

/// Build an HTTP response with an explicit status code and body.
pub(crate) fn http_status(code: u16, reason: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {code} {reason}\r\n\
         Content-Type: text/xml; charset=\"utf-8\"\r\n\
         Content-Length: {len}\r\n\
         Connection: close\r\n\r\n{body}",
        len = body.len()
    )
}

/// Accept exactly one TCP connection on an ephemeral loopback port, capture the
/// full raw request (headers and body), reply with `response`, and return the
/// captured request via the join handle.
pub(crate) fn serve_http_once(response: String) -> (SocketAddr, JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock HTTP server");
    let addr = listener.local_addr().expect("mock HTTP server addr");
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept mock HTTP connection");
        let request = read_http_request(&mut stream);
        stream
            .write_all(response.as_bytes())
            .expect("write mock HTTP response");
        stream.flush().expect("flush mock HTTP response");
        let _ = stream.shutdown(Shutdown::Write);
        request
    });
    (addr, handle)
}

/// Read a complete HTTP request: headers up to `\r\n\r\n`, then `Content-Length`
/// body bytes if present.
fn read_http_request(stream: &mut TcpStream) -> String {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 1024];
    loop {
        let n = stream.read(&mut tmp).expect("read mock HTTP request");
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(pos) = find(&buf, b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&buf[..pos]);
            let content_length = head
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.trim()
                        .eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())
                        .flatten()
                })
                .unwrap_or(0);
            let body_start = pos + 4;
            while buf.len() < body_start + content_length {
                let n = stream.read(&mut tmp).expect("read mock HTTP body");
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&tmp[..n]);
            }
            break;
        }
    }
    String::from_utf8_lossy(&buf).into_owned()
}

/// Bind a UDP responder on an ephemeral loopback port that answers the first
/// SSDP `M-SEARCH` it receives with a canned `200 OK` advertising `location`.
pub(crate) fn spawn_ssdp_responder(location: String) -> (SocketAddrV4, JoinHandle<()>) {
    let socket = UdpSocket::bind("127.0.0.1:0").expect("bind mock SSDP responder");
    let addr = match socket.local_addr().expect("mock SSDP addr") {
        SocketAddr::V4(addr) => addr,
        SocketAddr::V6(_) => panic!("expected IPv4 loopback"),
    };
    let handle = thread::spawn(move || {
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("set ssdp read timeout");
        let mut buf = [0u8; 2048];
        if let Ok((_, src)) = socket.recv_from(&mut buf) {
            let reply = format!(
                "HTTP/1.1 200 OK\r\n\
                 CACHE-CONTROL: max-age=1800\r\n\
                 EXT:\r\n\
                 LOCATION: {location}\r\n\
                 SERVER: Linux UPnP/1.0 Sonos/1\r\n\
                 ST: urn:schemas-upnp-org:device:ZonePlayer:1\r\n\
                 USN: uuid:RINCON_TEST01400::urn:schemas-upnp-org:device:ZonePlayer:1\r\n\r\n"
            );
            let _ = socket.send_to(reply.as_bytes(), src);
        }
    });
    (addr, handle)
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}
