# 0005 — `stream-server`: async `axum` HTTP with a broadcast ring buffer

- Status: Accepted
- Date: 2026-06-25

## Context

Workstream WS-C implements the `stream-server` crate: the local HTTP server that
Sonos players pull audio from. ADR-0002 froze the `Transport` / `StreamSink`
contracts; this ADR records *how* the server realises them. The forces:

- **One producer, many consumers.** A grouped Sonos room is several players, each
  opening its own HTTP connection, that must all receive the *same* encoded
  byte stream.
- **The audio thread must never block.** `StreamSink::write` is on the capture →
  encode → publish hot path. A slow or stalled player must not stall the encoder
  or any other player; latency must stay bounded (see `docs/LATENCY-BUDGET.md`).
- **Sonos needs a long-lived, length-unknown HTTP body.** The stream is open-ended,
  so chunked transfer-encoding (no `Content-Length`) is required, with the codec's
  MIME type as `Content-Type`.
- **Sonos must dial us back on the LAN.** The advertised `stream_url` has to carry a
  LAN-reachable IP, not `0.0.0.0` or loopback, when bound to all interfaces.
- **Hygiene.** Pure-Rust, `#![forbid(unsafe_code)]`, permissive licences only, no
  TLS (Sonos fetches over plain HTTP on the LAN), latest pinned dependencies, and
  zero advisories.

## Decision

- **`axum` 0.8 on `hyper` 1 + `tokio`**, chosen over `tiny_http`. `hyper` gives
  correct, production-grade HTTP/1.1 chunked streaming and effortless multi-client
  concurrency on an async runtime; `tiny_http`'s thread-per-connection, blocking
  model fits the fan-out and back-pressure requirements far less naturally. Only
  the minimal features are enabled (`http1`, `tokio`); no TLS, no router extras.
- **The crate owns a multi-threaded `tokio` runtime.** `StreamServer::start` is a
  synchronous call: it binds a `std::net::TcpListener` first (so the real port is
  known even for `port = 0`), then builds the runtime and spawns the accept loop
  and a telemetry task. The handle joins the runtime on `stop`/`Drop`.
- **Fan-out via a bounded `tokio::sync::broadcast::<Bytes>` channel** acting as a
  per-stream ring buffer. `StreamSink::write` copies the chunk once into a
  reference-counted `Bytes` and `send`s it — wait-free with respect to clients.
  Each connection `subscribe()`s at the live edge; a connection that cannot keep
  up receives `RecvError::Lagged(n)`, i.e. the broadcast has already dropped the
  **oldest** chunks for that subscriber alone. The producer never blocks and other
  clients are unaffected.
- **Graceful end-of-stream vs hard shutdown are distinct signals**, each a
  `watch::<bool>`. `StreamSink::close` *drains* each connection's buffered backlog
  before ending its response (clean tail); server shutdown ends responses
  immediately. New clients always join at the live edge.
- **LAN IP discovery is built in-crate**, no dependency: a UDP socket is "connected"
  to a well-known routable address so the OS routing table selects the egress
  interface, and its local address is read back. No datagrams are sent. Loopback
  and concrete binds are advertised verbatim; only an unspecified bind triggers
  discovery, falling back to loopback.
- **Telemetry via `tracing`**: connected clients, bytes/second, and dropped chunks
  are exposed as a `ServerMetrics` snapshot and logged once per second when active.

## Consequences

- ✅ Many grouped players stream concurrently off one encoder with a single copy
  per chunk; fan-out is N cheap `Arc` clones.
- ✅ A slow player degrades only itself (drop-oldest, bounded buffering) and is
  proven isolated by a deterministic loopback test that gates the producer on a
  fast client's progress while a slow client falls behind.
- ✅ Correct chunked framing and `Content-Type`, verified byte-for-byte by an
  integration test using a dependency-free `std::net` HTTP client.
- ✅ `unsafe`-free; `axum`/`hyper`/`tokio`/`bytes` are permissively licensed and
  pass `cargo deny` and `cargo audit` with zero advisories.
- ⚠️ Owning a `tokio` runtime means teardown must run in a synchronous context;
  dropping a `StreamServer` inside another runtime's async task would panic. This
  is documented on the type and matches how the orchestration layer drives it.
- ⚠️ The broadcast ring trades completeness for liveness: a lagging client loses
  old audio rather than adding latency. For a real-time bridge that is the correct
  trade-off, but it means the transport is intentionally lossy under back-pressure.
- ⚠️ Plain HTTP only. TLS is out of scope (LAN delivery to Sonos); exposing the
  stream beyond the LAN would require revisiting this.
