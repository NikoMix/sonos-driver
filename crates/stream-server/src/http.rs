//! The async HTTP serving layer: shared state, the streaming handler, the
//! accept loop, and periodic telemetry.

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::extract::State;
use axum::http::{header, HeaderValue};
use axum::response::Response;
use axum::routing::get;
use axum::Router;
use bytes::Bytes;
use tokio::net::TcpListener;
use tokio::sync::{broadcast, mpsc, watch};
use tokio_stream::wrappers::ReceiverStream;

use crate::metrics::Metrics;

/// Depth of the per-connection hand-off buffer between the broadcast receiver
/// and the HTTP body writer. Kept intentionally tiny so back-pressure from a
/// slow socket surfaces as broadcast lag (drop-oldest) rather than as added
/// latency on the live edge.
const CONNECTION_BUFFER: usize = 4;

/// State shared by every connection handler, cloned cheaply per request.
#[derive(Clone)]
pub(crate) struct AppState {
    /// Producer side of the fan-out broadcast; handlers `subscribe()` to it.
    pub(crate) tx: broadcast::Sender<Bytes>,
    /// Shared telemetry counters.
    pub(crate) metrics: Arc<Metrics>,
    /// Pre-parsed `Content-Type` advertised to every client.
    pub(crate) content_type: HeaderValue,
    /// Set to `true` to tear the whole server down.
    pub(crate) shutdown_rx: watch::Receiver<bool>,
    /// Set to `true` when the producer ends the stream (`StreamSink::close`).
    pub(crate) sink_closed_rx: watch::Receiver<bool>,
}

/// Resolve as soon as `rx` holds — or transitions to — `true`. Also resolves if
/// the sender is dropped, so teardown always makes progress. Unlike a bare
/// `changed().await`, this returns immediately when the flag is *already* set,
/// which removes the race between a late subscriber and an early signal.
pub(crate) async fn flagged(mut rx: watch::Receiver<bool>) {
    if *rx.borrow_and_update() {
        return;
    }
    while rx.changed().await.is_ok() {
        if *rx.borrow_and_update() {
            return;
        }
    }
}

/// Sender half of a single connection's body channel.
type BodySender = mpsc::Sender<Result<Bytes, Infallible>>;

/// Flush every chunk already buffered for this connection, recording any
/// drop-oldest events, then return. Called when the producer signals
/// end-of-stream so a client receives a clean tail before its response closes —
/// the graceful counterpart to a hard server shutdown, which drops the backlog.
async fn drain_backlog(
    broadcast_rx: &mut broadcast::Receiver<Bytes>,
    body_tx: &BodySender,
    metrics: &Metrics,
) {
    loop {
        match broadcast_rx.try_recv() {
            Ok(bytes) => {
                if body_tx.send(Ok(bytes)).await.is_err() {
                    return; // client disconnected
                }
            }
            Err(broadcast::error::TryRecvError::Lagged(skipped)) => metrics.record_drops(skipped),
            Err(broadcast::error::TryRecvError::Empty | broadcast::error::TryRecvError::Closed) => {
                return;
            }
        }
    }
}

/// Handle one streaming connection: subscribe at the live edge and pump encoded
/// chunks into a chunked HTTP response until shutdown, end-of-stream, or client
/// disconnect.
async fn stream_handler(State(state): State<AppState>) -> Response {
    let (tx, rx) = mpsc::channel::<Result<Bytes, Infallible>>(CONNECTION_BUFFER);
    let mut broadcast_rx = state.tx.subscribe();
    let metrics = Arc::clone(&state.metrics);
    let shutdown_rx = state.shutdown_rx.clone();
    let sink_closed_rx = state.sink_closed_rx.clone();

    tokio::spawn(async move {
        // Counts this client for as long as the task lives, even on early exit.
        let _guard = metrics.client_connected();
        let mut shutdown = std::pin::pin!(flagged(shutdown_rx));
        let mut sink_closed = std::pin::pin!(flagged(sink_closed_rx));

        loop {
            tokio::select! {
                biased;
                () = &mut shutdown => break,
                () = &mut sink_closed => {
                    // End-of-stream: deliver everything already buffered, then
                    // finish this response cleanly.
                    drain_backlog(&mut broadcast_rx, &tx, &metrics).await;
                    break;
                }
                received = broadcast_rx.recv() => match received {
                    Ok(bytes) => {
                        // Forwarding awaits when the client socket is full; that
                        // back-pressure makes *this* receiver lag at the
                        // broadcast layer, never the shared producer.
                        if tx.send(Ok(bytes)).await.is_err() {
                            break; // client disconnected: body receiver dropped
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        // This client could not keep up; the broadcast already
                        // dropped the oldest chunks for it alone.
                        metrics.record_drops(skipped);
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    });

    let mut response = Response::new(Body::from_stream(ReceiverStream::new(rx)));
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, state.content_type);
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-cache, no-store, must-revalidate"),
    );
    response
}

/// Run the accept loop until the shutdown flag is set. Errors are logged rather
/// than propagated because this runs as a detached task on the crate's runtime.
pub(crate) async fn serve(
    listener: TcpListener,
    path: String,
    state: AppState,
    shutdown_rx: watch::Receiver<bool>,
) {
    let app = Router::new()
        .route(&path, get(stream_handler))
        .with_state(state);

    if let Err(error) = axum::serve(listener, app)
        .with_graceful_shutdown(flagged(shutdown_rx))
        .await
    {
        tracing::error!(%error, "stream-server accept loop terminated unexpectedly");
    }
}

/// Periodically emit `tracing` telemetry (clients, throughput, drops) until the
/// server shuts down. Quiet while fully idle to avoid log spam.
pub(crate) async fn telemetry_loop(metrics: Arc<Metrics>, shutdown_rx: watch::Receiver<bool>) {
    let mut ticker = tokio::time::interval(Duration::from_secs(1));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut shutdown = std::pin::pin!(flagged(shutdown_rx));
    let mut last_bytes = 0_u64;

    loop {
        tokio::select! {
            () = &mut shutdown => break,
            _ = ticker.tick() => {
                let snapshot = metrics.snapshot();
                let bytes_per_sec = snapshot.total_bytes.saturating_sub(last_bytes);
                last_bytes = snapshot.total_bytes;
                if snapshot.connected_clients > 0 || bytes_per_sec > 0 || snapshot.dropped_chunks > 0
                {
                    tracing::debug!(
                        clients = snapshot.connected_clients,
                        bytes_per_sec,
                        dropped_chunks = snapshot.dropped_chunks,
                        "stream-server telemetry"
                    );
                }
            }
        }
    }
}
