//! A deliberately tiny synchronous HTTP/1.1 client over [`std::net::TcpStream`].
//!
//! Sonos LAN control is plain HTTP (no TLS), the control surface is synchronous
//! and off the audio hot path, and the request/response bodies are small. A
//! hand-rolled client therefore keeps the dependency and attack surface minimal
//! while satisfying every need of SSDP device-description fetches and UPnP SOAP
//! calls. It supports `Content-Length`, connection-close, and chunked responses.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use audio_core::{Error, Result};

/// Maximum bytes we are willing to read for a single control/description
/// response. Sonos payloads are a few KiB; this guards against a misbehaving or
/// hostile peer streaming unbounded data into memory.
const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;

/// A parsed `http://host[:port][/path]` URL (no scheme other than `http`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HttpUrl {
    /// Host portion (IP literal or hostname).
    pub(crate) host: String,
    /// TCP port (defaults to 80 when omitted).
    pub(crate) port: u16,
    /// Request path including any query, defaulting to `/`.
    pub(crate) path: String,
}

/// Parse a plain-HTTP URL. TLS (`https`) is rejected by design.
pub(crate) fn parse_http_url(url: &str) -> Result<HttpUrl> {
    let rest = url.strip_prefix("http://").ok_or_else(|| {
        if url.starts_with("https://") {
            Error::control("https is not supported (Sonos LAN control is plain HTTP)")
        } else {
            Error::control(format!("not an http URL: {url}"))
        }
    })?;

    let (authority, path) = match rest.find('/') {
        Some(idx) => (&rest[..idx], &rest[idx..]),
        None => (rest, "/"),
    };
    if authority.is_empty() {
        return Err(Error::control(format!("missing host in URL: {url}")));
    }

    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port_str)) => {
            let port = port_str
                .parse::<u16>()
                .map_err(|_| Error::control(format!("invalid port in URL: {url}")))?;
            (host, port)
        }
        None => (authority, 80),
    };

    Ok(HttpUrl {
        host: host.to_owned(),
        port,
        path: if path.is_empty() {
            "/".to_owned()
        } else {
            path.to_owned()
        },
    })
}

/// A parsed HTTP response.
#[derive(Debug)]
pub(crate) struct HttpResponse {
    /// Numeric status code (e.g. `200`, `500`).
    pub(crate) status: u16,
    /// Decoded response body.
    pub(crate) body: Vec<u8>,
}

impl HttpResponse {
    /// Body interpreted as UTF-8 (lossily), which every Sonos response is.
    pub(crate) fn body_str(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    /// True for any `2xx` status.
    pub(crate) fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

/// Issue an HTTP `GET`.
pub(crate) fn get(url: &str, timeout: Duration) -> Result<HttpResponse> {
    request("GET", url, &[], &[], timeout)
}

/// Issue an HTTP `POST` with the supplied headers and body.
pub(crate) fn post(
    url: &str,
    headers: &[(&str, &str)],
    body: &[u8],
    timeout: Duration,
) -> Result<HttpResponse> {
    request("POST", url, headers, body, timeout)
}

fn request(
    method: &str,
    url: &str,
    headers: &[(&str, &str)],
    body: &[u8],
    timeout: Duration,
) -> Result<HttpResponse> {
    let parsed = parse_http_url(url)?;
    let mut stream = connect(&parsed.host, parsed.port, timeout)?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;

    let mut req = String::with_capacity(256);
    req.push_str(method);
    req.push(' ');
    req.push_str(&parsed.path);
    req.push_str(" HTTP/1.1\r\n");
    req.push_str("Host: ");
    req.push_str(&parsed.host);
    req.push(':');
    req.push_str(&parsed.port.to_string());
    req.push_str("\r\n");
    req.push_str("User-Agent: sonos-driver/0.1\r\n");
    req.push_str("Accept: */*\r\n");
    for (name, value) in headers {
        req.push_str(name);
        req.push_str(": ");
        req.push_str(value);
        req.push_str("\r\n");
    }
    if method != "GET" || !body.is_empty() {
        req.push_str("Content-Length: ");
        req.push_str(&body.len().to_string());
        req.push_str("\r\n");
    }
    req.push_str("Connection: close\r\n\r\n");

    stream.write_all(req.as_bytes())?;
    if !body.is_empty() {
        stream.write_all(body)?;
    }
    stream.flush()?;

    let raw = read_to_end_capped(&mut stream)?;
    parse_response(&raw)
}

fn connect(host: &str, port: u16, timeout: Duration) -> Result<TcpStream> {
    let mut last_err: Option<std::io::Error> = None;
    for addr in (host, port).to_socket_addrs()? {
        match TcpStream::connect_timeout(&addr, timeout) {
            Ok(stream) => return Ok(stream),
            Err(err) => last_err = Some(err),
        }
    }
    Err(last_err.map_or_else(
        || Error::control(format!("could not resolve host: {host}")),
        Error::Io,
    ))
}

fn read_to_end_capped(stream: &mut TcpStream) -> Result<Vec<u8>> {
    let mut buf = Vec::with_capacity(8 * 1024);
    let mut chunk = [0u8; 8 * 1024];
    loop {
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        if buf.len() + n > MAX_RESPONSE_BYTES {
            return Err(Error::control("HTTP response exceeded size limit"));
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    Ok(buf)
}

fn parse_response(raw: &[u8]) -> Result<HttpResponse> {
    let split = find_subsequence(raw, b"\r\n\r\n")
        .ok_or_else(|| Error::control("malformed HTTP response: no header terminator"))?;
    let head = std::str::from_utf8(&raw[..split])
        .map_err(|_| Error::control("malformed HTTP response: non-UTF-8 headers"))?;
    let body_bytes = &raw[split + 4..];

    let mut lines = head.split("\r\n");
    let status_line = lines
        .next()
        .ok_or_else(|| Error::control("malformed HTTP response: empty"))?;
    let status = parse_status_line(status_line)?;

    let mut headers = Vec::new();
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            headers.push((name.trim().to_owned(), value.trim().to_owned()));
        }
    }

    let chunked = headers.iter().any(|(k, v)| {
        k.eq_ignore_ascii_case("transfer-encoding") && v.to_ascii_lowercase().contains("chunked")
    });
    let body = if chunked {
        decode_chunked(body_bytes)?
    } else {
        body_bytes.to_vec()
    };

    Ok(HttpResponse { status, body })
}

fn parse_status_line(line: &str) -> Result<u16> {
    let mut parts = line.split_whitespace();
    let _version = parts
        .next()
        .ok_or_else(|| Error::control("malformed HTTP status line"))?;
    let code = parts
        .next()
        .ok_or_else(|| Error::control("malformed HTTP status line"))?;
    code.parse::<u16>()
        .map_err(|_| Error::control(format!("invalid HTTP status code: {code}")))
}

fn decode_chunked(mut data: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(data.len());
    loop {
        let line_end = find_subsequence(data, b"\r\n")
            .ok_or_else(|| Error::control("malformed chunked body: missing size line"))?;
        let size_str = std::str::from_utf8(&data[..line_end])
            .map_err(|_| Error::control("malformed chunked body: bad size line"))?;
        let size_str = size_str.split(';').next().unwrap_or("").trim();
        let size = usize::from_str_radix(size_str, 16)
            .map_err(|_| Error::control(format!("invalid chunk size: {size_str}")))?;
        data = &data[line_end + 2..];
        if size == 0 {
            break;
        }
        if size > data.len() {
            return Err(Error::control("malformed chunked body: truncated chunk"));
        }
        out.extend_from_slice(&data[..size]);
        data = &data[size..];
        // Skip the trailing CRLF after the chunk data.
        if data.len() >= 2 {
            data = &data[2..];
        }
    }
    Ok(out)
}

fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_url_with_explicit_port_and_path() {
        let url = parse_http_url("http://192.168.1.50:1400/MediaRenderer/AVTransport/Control")
            .expect("parse");
        assert_eq!(url.host, "192.168.1.50");
        assert_eq!(url.port, 1400);
        assert_eq!(url.path, "/MediaRenderer/AVTransport/Control");
    }

    #[test]
    fn defaults_port_and_path() {
        let url = parse_http_url("http://example.local").expect("parse");
        assert_eq!(url.port, 80);
        assert_eq!(url.path, "/");
    }

    #[test]
    fn rejects_https() {
        assert!(matches!(
            parse_http_url("https://192.168.1.50/x"),
            Err(Error::Control(_))
        ));
    }

    #[test]
    fn parses_response_with_content_length() {
        let raw = b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nContent-Type: text/xml\r\n\r\nhello";
        let resp = parse_response(raw).expect("parse");
        assert_eq!(resp.status, 200);
        assert!(resp.is_success());
        assert_eq!(resp.body_str(), "hello");
    }

    #[test]
    fn decodes_chunked_response() {
        let raw =
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n6\r\n world\r\n0\r\n\r\n";
        let resp = parse_response(raw).expect("parse");
        assert_eq!(resp.body_str(), "hello world");
    }

    #[test]
    fn surfaces_non_success_status() {
        let raw = b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\n\r\n";
        let resp = parse_response(raw).expect("parse");
        assert_eq!(resp.status, 500);
        assert!(!resp.is_success());
    }
}
