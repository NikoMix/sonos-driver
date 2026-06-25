//! The session lifecycle.
//!
//! [`run_session`] starts the stream server, points the player at it, runs the
//! capture → pipeline → encode → publish loop on a dedicated thread, supervises
//! that thread, then tears everything down cleanly. It is shared by
//! [`crate::Bridge::run`] and [`crate::Bridge::run_with`].
//!
//! The loop is **capture-paced**: one [`AudioSource::read`] yields one block,
//! which is resampled/converted, drained through the jitter buffer and encoded
//! before the next read. Real jitter absorption lives downstream in the stream
//! server's broadcast ring buffer and the Sonos device buffer; the pipeline
//! jitter buffer here bounds bursts and provides overrun telemetry. See
//! ADR-0007 for the rationale behind draining the buffer fully each iteration.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use audio_core::{
    AudioFormat, AudioFrame, AudioSource, Config, Encoder, Error, PipelineConfig, Result,
    SampleFormat, StreamSink, Transport,
};
use audio_pipeline::Pipeline;
use stream_server::{ServerConfig, StreamServer};

use crate::latency::LatencyAccumulator;
use crate::metrics::SharedMetrics;
use crate::player::PlayerControl;

/// The bridge always publishes interleaved stereo.
///
/// Sonos expects a stereo stream and the encoders validate that every frame
/// matches the negotiated channel count, so the pipeline up-mixes mono or
/// down-mixes surround to the front pair and the encoder's format stays fixed
/// for the whole session.
const OUTPUT_CHANNELS: u16 = 2;

/// Wire the full pipeline and block until the bridge is stopped or a stage
/// fails.
///
/// On a stop request or graceful end-of-stream the session tears down and
/// returns `Ok(())`; on a stage failure it tears down and propagates the error.
pub(crate) fn run_session(
    config: &Config,
    source: Box<dyn AudioSource>,
    control: Box<dyn PlayerControl>,
    stop: &Arc<AtomicBool>,
    metrics: &Arc<SharedMetrics>,
    latency: &Arc<LatencyAccumulator>,
) -> Result<()> {
    // A stop requested before we start is honoured as an immediate clean no-op.
    if stop.load(Ordering::SeqCst) {
        return Ok(());
    }

    let source_rate = source.format().sample_rate;
    let output_rate = config.pipeline.target_sample_rate.unwrap_or(source_rate);
    let output_format = AudioFormat::new(output_rate, OUTPUT_CHANNELS, SampleFormat::I16)?;

    // Build the encoder first: an unsupported profile must fail before we bind a
    // socket or contact the player.
    let encoder = audio_codec::encoder_for(config.stream.profile, output_format)?;
    let content_type = encoder.content_type().into_owned();

    // Start the HTTP stream server; it resolves the LAN-reachable URL itself.
    let mut server_config = ServerConfig::from_stream_config(&config.stream);
    server_config.content_type.clone_from(&content_type);
    let mut server = StreamServer::start(server_config)?;
    let stream_url = server.url().to_owned();

    tracing::info!(%stream_url, %content_type, output_rate, "bridge session starting");

    // Spawn the capture → pipeline → encode → publish loop.
    let worker = EncodeWorker {
        source,
        encoder,
        sink: server.sink(),
        output_rate,
        buffer_ms: config.pipeline.buffer_ms,
        stop: Arc::clone(stop),
        metrics: Arc::clone(metrics),
        latency: Arc::clone(latency),
    };
    let handle = thread::Builder::new()
        .name("bridge-encode".to_owned())
        .spawn(move || worker.run())
        .map_err(|error| Error::transport(format!("failed to spawn encode thread: {error}")))?;

    // Point the player at our stream and start playback; tear down on failure.
    if let Err(error) = start_playback(control.as_ref(), &stream_url, &content_type) {
        stop.store(true, Ordering::SeqCst);
        let _ = handle.join();
        let _ = control.stop();
        server.stop();
        return Err(error);
    }

    // Supervise until a stop is requested or the encode loop ends on its own.
    supervise(&server, &handle, control.as_ref(), stop, metrics);

    // Graceful teardown: stop the loop, join it, stop the player, stop the
    // server. The worker's own result is the session's result.
    stop.store(true, Ordering::SeqCst);
    let worker_result = handle
        .join()
        .unwrap_or_else(|_| Err(Error::pipeline("encode thread panicked")));
    if let Err(error) = control.stop() {
        tracing::warn!(%error, "failed to stop the player during teardown");
    }
    // The bridge owns the player for the session; release it now that playback
    // has stopped, before shutting the server down.
    drop(control);
    server.stop();
    worker_result
}

/// Point the player at `stream_url` and start playback.
fn start_playback(control: &dyn PlayerControl, stream_url: &str, content_type: &str) -> Result<()> {
    control.set_stream_uri(stream_url, content_type)?;
    control.play()
}

/// Poll until a stop is requested or the encode thread finishes, re-pointing the
/// player if a previously connected client drops away for long enough.
///
/// Reconnection is best-effort telemetry: a transient client drop on the LAN
/// should not end the session, and a debounce avoids thrashing on a clean
/// disconnect that is immediately followed by teardown.
fn supervise(
    server: &StreamServer,
    handle: &thread::JoinHandle<Result<()>>,
    control: &dyn PlayerControl,
    stop: &Arc<AtomicBool>,
    metrics: &Arc<SharedMetrics>,
) {
    const POLL: Duration = Duration::from_millis(50);
    const RECONNECT_DEBOUNCE: Duration = Duration::from_millis(750);

    let stream_url = server.url().to_owned();
    let content_type = server.config().content_type.clone();
    let mut seen_client = false;
    let mut dropped_at: Option<Instant> = None;

    while !stop.load(Ordering::SeqCst) && !handle.is_finished() {
        thread::sleep(POLL);

        if server.metrics().connected_clients > 0 {
            seen_client = true;
            dropped_at = None;
            continue;
        }

        if !seen_client {
            continue;
        }

        match dropped_at {
            None => dropped_at = Some(Instant::now()),
            Some(since) if since.elapsed() >= RECONNECT_DEBOUNCE => {
                match start_playback(control, &stream_url, &content_type) {
                    Ok(()) => {
                        metrics.inc_reconnects();
                        tracing::info!("re-pointed the player after a client drop");
                    }
                    Err(error) => tracing::warn!(%error, "player reconnect attempt failed"),
                }
                seen_client = false;
                dropped_at = None;
            }
            Some(_) => {}
        }
    }
}

/// Owns the source, encoder and sink and runs the encode loop on its own
/// thread, communicating with the rest of the bridge only through atomics.
struct EncodeWorker {
    source: Box<dyn AudioSource>,
    encoder: Box<dyn Encoder>,
    sink: Arc<dyn StreamSink>,
    output_rate: u32,
    buffer_ms: u32,
    stop: Arc<AtomicBool>,
    metrics: Arc<SharedMetrics>,
    latency: Arc<LatencyAccumulator>,
}

impl EncodeWorker {
    /// Run the loop to completion, returning the first stage error or `Ok(())`
    /// on a graceful stop or end-of-stream.
    fn run(mut self) -> Result<()> {
        // Built here, on the worker thread, so the pipeline (which is not
        // required to be `Send`) never crosses a thread boundary.
        let mut pipeline = Pipeline::new(&PipelineConfig {
            target_sample_rate: Some(self.output_rate),
            buffer_ms: self.buffer_ms,
        })
        .with_target_channels(OUTPUT_CHANNELS);

        let mut out = Vec::new();
        let prelude = self.encoder.prelude()?;
        if !prelude.is_empty() {
            self.sink.write(&prelude)?;
        }

        let result = self.pump(&mut pipeline, &mut out);

        // Best-effort flush of the encoder tail before closing the stream; a
        // failure here must not mask the loop's own result.
        out.clear();
        if self.encoder.finish(&mut out).is_ok() && !out.is_empty() {
            let _ = self.sink.write(&out);
        }
        self.sink.close();
        result
    }

    /// The capture loop: read, process, publish, repeat until stopped, the
    /// source ends, or a stage fails.
    fn pump(&mut self, pipeline: &mut Pipeline, out: &mut Vec<u8>) -> Result<()> {
        while !self.stop.load(Ordering::SeqCst) {
            match self.source.read() {
                Ok(Some(frame)) => {
                    let origin = frame.captured_at().unwrap_or_else(Instant::now);
                    self.metrics.inc_captured();
                    let processed = pipeline.process(frame)?;
                    self.publish(pipeline, processed, origin, out)?;
                }
                Ok(None) => break,
                Err(error) => {
                    self.stop.store(true, Ordering::SeqCst);
                    return Err(error);
                }
            }
        }
        Ok(())
    }

    /// Buffer (when enabled) and encode a processed frame.
    fn publish(
        &mut self,
        pipeline: &mut Pipeline,
        processed: AudioFrame,
        origin: Instant,
        out: &mut Vec<u8>,
    ) -> Result<()> {
        // Resampler warm-up yields empty frames; nothing to buffer or send.
        if processed.frame_count() == 0 {
            return Ok(());
        }

        // `buffer_ms == 0` disables jitter buffering (a zero-capacity buffer
        // retains nothing), so bypass it and encode directly for the lowest
        // latency.
        if self.buffer_ms == 0 {
            return self.encode_and_send(pipeline, &processed, origin, out);
        }

        pipeline.push(processed);
        // Drain fully: one read yields one block in this capture-paced loop, so
        // draining keeps the buffer at the live edge and the popped frame's
        // origin matches `origin`. The `is_empty` guard avoids a pop on empty,
        // which the buffer would otherwise count as a spurious underrun.
        while pipeline.jitter().is_some_and(|jitter| !jitter.is_empty()) {
            let Some(frame) = pipeline.pop() else { break };
            self.encode_and_send(pipeline, &frame, origin, out)?;
        }
        Ok(())
    }

    /// Encode one frame and publish the bytes, updating metrics and latency.
    fn encode_and_send(
        &mut self,
        pipeline: &Pipeline,
        frame: &AudioFrame,
        origin: Instant,
        out: &mut Vec<u8>,
    ) -> Result<()> {
        if frame.frame_count() == 0 {
            return Ok(());
        }
        out.clear();
        self.encoder.encode(frame, out)?;
        if !out.is_empty() {
            self.sink.write(out)?;
            self.metrics.inc_streamed();
            self.latency.record(origin.elapsed());
        }
        if let Some(jitter) = pipeline.jitter() {
            self.metrics.set_underruns(jitter.underruns());
        }
        Ok(())
    }
}
