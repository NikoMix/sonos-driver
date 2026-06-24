//! Loopback integration tests for the chunked HTTP stream server.
//!
//! These exercise the crate through its public API and a hand-rolled,
//! dependency-free HTTP/1.1 client (`std::net` + threads). A bespoke client is
//! used deliberately: [`StreamServer::start`] owns its own `tokio` runtime and
//! calls `block_on` internally, so it cannot be driven from inside a
//! `#[tokio::test]` (that would nest runtimes and panic). Plain `#[test]`s with
//! blocking sockets are both correct and aligned with the crate's
//! "build-it-yourself" dependency ethos.

// Test fixtures map chunk/byte indices into `u8` patterns; the truncation is
// intentional and the values are compared against an identically-built oracle.
#![allow(clippy::cast_possible_truncation)]

use std::io::{BufRead, BufReader, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use audio_core::Transport;
use stream_server::{ServerConfig, StreamServer};

const READ_TIMEOUT: Duration = Duration::from_secs(15);

fn test_config(capacity: usize) -> ServerConfig {
    ServerConfig {
        bind_address: IpAddr::V4(Ipv4Addr::LOCALHOST),
        port: 0,
        path: "/stream".to_owned(),
        content_type: "audio/L16".to_owned(),
        channel_capacity: capacity,
    }
}

/// A deterministic, per-index chunk so order and content can be verified byte
/// for byte. Distinct `index` values yield distinct byte patterns.
fn make_chunk(index: u32, size: usize) -> Vec<u8> {
    let seed = index.wrapping_mul(2_654_435_761);
    (0..size)
        .map(|offset| (seed.wrapping_add(offset as u32)) as u8)
        .collect()
}

/// Block until the server reports at least `expected` connected clients, so the
/// producer only writes once every client has subscribed at the live edge.
fn wait_for_clients(server: &StreamServer, expected: u64) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while server.metrics().connected_clients < expected {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {expected} client(s) to connect"
        );
        thread::sleep(Duration::from_millis(5));
    }
}

fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.as_str())
}

/// Connect, issue the streaming `GET`, and parse the status line and headers,
/// leaving the reader positioned at the first chunk-size line.
fn open_stream(addr: SocketAddr) -> (BufReader<TcpStream>, u16, Vec<(String, String)>) {
    let stream = TcpStream::connect(addr).expect("connect to stream server");
    stream.set_nodelay(true).ok();
    stream
        .set_read_timeout(Some(READ_TIMEOUT))
        .expect("set read timeout");

    let mut writer = stream.try_clone().expect("clone stream for writing");
    write!(writer, "GET /stream HTTP/1.1\r\nHost: {addr}\r\n\r\n").expect("send request");
    writer.flush().expect("flush request");

    let mut reader = BufReader::new(stream);

    let mut status_line = String::new();
    reader
        .read_line(&mut status_line)
        .expect("read status line");
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .unwrap_or(0);

    let mut headers = Vec::new();
    loop {
        let mut line = String::new();
        let read = reader.read_line(&mut line).expect("read header line");
        if read == 0 || line == "\r\n" || line == "\n" {
            break;
        }
        if let Some((key, value)) = line.split_once(':') {
            headers.push((key.trim().to_ascii_lowercase(), value.trim().to_owned()));
        }
    }

    (reader, status, headers)
}

/// Decode a single HTTP/1.1 chunk. Returns `None` at the terminating zero-size
/// chunk or on end of stream.
fn read_one_chunk<R: BufRead>(reader: &mut R) -> Option<Vec<u8>> {
    let mut size_line = String::new();
    if reader.read_line(&mut size_line).ok()? == 0 {
        return None;
    }
    let size_field = size_line.trim();
    let size_hex = size_field.split(';').next()?;
    let size = usize::from_str_radix(size_hex, 16).ok()?;
    if size == 0 {
        let mut trailer = String::new();
        let _ = reader.read_line(&mut trailer);
        return None;
    }
    let mut payload = vec![0_u8; size];
    reader.read_exact(&mut payload).ok()?;
    let mut crlf = [0_u8; 2];
    reader.read_exact(&mut crlf).ok()?;
    Some(payload)
}

/// Decode an entire chunked body to completion.
fn read_chunked_body<R: BufRead>(reader: &mut R) -> Vec<u8> {
    let mut body = Vec::new();
    while let Some(chunk) = read_one_chunk(reader) {
        body.extend_from_slice(&chunk);
    }
    body
}

/// Compare two streams without dumping multi-megabyte vectors on failure:
/// report the length and the index of the first differing byte instead.
fn assert_stream_eq(actual: &[u8], expected: &[u8], label: &str) {
    assert_eq!(
        actual.len(),
        expected.len(),
        "{label}: received {} bytes, expected {}",
        actual.len(),
        expected.len()
    );
    if let Some(index) = actual.iter().zip(expected).position(|(a, b)| a != b) {
        panic!(
            "{label}: first byte mismatch at index {index} ({} != {})",
            actual[index], expected[index]
        );
    }
}

#[test]
fn streams_exact_bytes_with_chunked_framing_and_content_type() {
    let server = StreamServer::start(test_config(256)).expect("server starts");
    let addr = server.local_addr();
    let sink = server.sink();

    let client = thread::spawn(move || {
        let (mut reader, status, headers) = open_stream(addr);
        assert_eq!(status, 200, "expected HTTP 200");
        assert_eq!(
            header(&headers, "content-type"),
            Some("audio/L16"),
            "Content-Type must match the configured codec MIME"
        );
        assert_eq!(
            header(&headers, "transfer-encoding"),
            Some("chunked"),
            "the stream must use chunked transfer-encoding"
        );
        assert_eq!(
            header(&headers, "content-length"),
            None,
            "a chunked stream must not advertise a Content-Length"
        );
        read_chunked_body(&mut reader)
    });

    wait_for_clients(&server, 1);

    let mut expected = Vec::new();
    for index in 0..64_u32 {
        let chunk = make_chunk(index, 1024);
        expected.extend_from_slice(&chunk);
        sink.write(&chunk).expect("write chunk");
    }
    sink.close();

    let body = client.join().expect("client thread");
    assert_stream_eq(&body, &expected, "single client");

    let metrics = server.metrics();
    assert_eq!(metrics.total_chunks, 64);
    assert_eq!(metrics.total_bytes, 64 * 1024);
    assert_eq!(
        metrics.dropped_chunks, 0,
        "a keeping-up client drops nothing"
    );
}

#[test]
fn fans_out_identical_stream_to_many_clients() {
    let server = StreamServer::start(test_config(256)).expect("server starts");
    let addr = server.local_addr();
    let sink = server.sink();

    let client_count: u32 = 3;
    let clients: Vec<_> = (0..client_count)
        .map(|_| {
            thread::spawn(move || {
                let (mut reader, status, headers) = open_stream(addr);
                assert_eq!(status, 200);
                assert_eq!(header(&headers, "content-type"), Some("audio/L16"));
                read_chunked_body(&mut reader)
            })
        })
        .collect();

    wait_for_clients(&server, u64::from(client_count));

    let mut expected = Vec::new();
    for index in 0..48_u32 {
        let chunk = make_chunk(index, 2048);
        expected.extend_from_slice(&chunk);
        sink.write(&chunk).expect("write chunk");
    }
    sink.close();

    for client in clients {
        let body = client.join().expect("client thread");
        assert_stream_eq(&body, &expected, "fan-out client");
    }
    assert_eq!(server.metrics().dropped_chunks, 0);
}

#[test]
fn slow_client_drops_oldest_without_affecting_a_fast_client() {
    // A shallow ring buffer makes a non-reading client overrun quickly and
    // deterministically, independent of wall-clock timing.
    let server = StreamServer::start(test_config(64)).expect("server starts");
    let addr = server.local_addr();
    let sink = server.sink();

    let fast_read = Arc::new(AtomicU64::new(0));
    let slow_permits = Arc::new(AtomicU64::new(0));
    let done = Arc::new(AtomicBool::new(false));

    // Fast client: reads as quickly as possible, counting decoded chunks so the
    // producer can pace itself to stay within the ring buffer for this client.
    let fast_counter = Arc::clone(&fast_read);
    let fast = thread::spawn(move || {
        let (mut reader, status, _headers) = open_stream(addr);
        assert_eq!(status, 200);
        let mut body = Vec::new();
        while let Some(chunk) = read_one_chunk(&mut reader) {
            body.extend_from_slice(&chunk);
            fast_counter.fetch_add(1, Ordering::Relaxed);
        }
        body
    });

    // Slow client: only reads when the producer grants a permit, and is granted
    // far fewer permits than chunks produced, so it falls behind and the oldest
    // chunks are dropped for it alone.
    let slow_gate = Arc::clone(&slow_permits);
    let slow_done = Arc::clone(&done);
    let slow = thread::spawn(move || {
        let (mut reader, status, _headers) = open_stream(addr);
        assert_eq!(status, 200);
        let mut body = Vec::new();
        let mut read_count = 0_u64;
        loop {
            if read_count < slow_gate.load(Ordering::Relaxed) {
                match read_one_chunk(&mut reader) {
                    Some(chunk) => {
                        body.extend_from_slice(&chunk);
                        read_count += 1;
                    }
                    None => break,
                }
            } else if slow_done.load(Ordering::Relaxed) {
                break;
            } else {
                thread::sleep(Duration::from_millis(1));
            }
        }
        body
    });

    wait_for_clients(&server, 2);

    let count = 400_u32;
    let chunk_size = 4096_usize;
    let window = 16_u64;
    let mut expected = Vec::new();
    let mut all_writes_ok = true;

    for index in 0..count {
        // Pace production to the fast client so its broadcast receiver can never
        // lag (in-flight stays below the ring-buffer capacity); the slow client
        // is deliberately not part of this gate.
        let deadline = Instant::now() + Duration::from_secs(15);
        while u64::from(index).saturating_sub(fast_read.load(Ordering::Relaxed)) >= window {
            assert!(Instant::now() < deadline, "fast client stalled");
            thread::sleep(Duration::from_millis(1));
        }

        let chunk = make_chunk(index, chunk_size);
        expected.extend_from_slice(&chunk);
        all_writes_ok &= sink.write(&chunk).is_ok();

        // Grant the slow client a read roughly every fifth chunk.
        if index % 5 == 0 {
            slow_permits.fetch_add(1, Ordering::Relaxed);
        }
    }

    sink.close();
    // Release the slow client to drain whatever remains and observe EOF.
    slow_permits.store(u64::MAX, Ordering::Relaxed);
    done.store(true, Ordering::Relaxed);

    let fast_body = fast.join().expect("fast client thread");
    let slow_body = slow.join().expect("slow client thread");

    assert!(all_writes_ok, "the producer hot path never fails on write");
    assert_stream_eq(&fast_body, &expected, "fast client");

    let metrics = server.metrics();
    assert!(
        metrics.dropped_chunks > 0,
        "the slow client must have caused drop-oldest events"
    );
    assert!(
        slow_body.len() < expected.len(),
        "the slow client receives a truncated stream ({} of {} bytes)",
        slow_body.len(),
        expected.len()
    );
    assert_eq!(
        metrics.total_chunks,
        u64::from(count),
        "every produced chunk is accounted for exactly once"
    );
}
