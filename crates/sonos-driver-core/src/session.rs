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

use crate::arbiter::Arbitration;
use crate::idle::{IdleAction, IdleTracker};
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
    arbitration: Option<&Arbitration>,
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
    // Advertise the stream on the local interface that actually routes to this
    // player. On a multi-homed host (Wi-Fi + Ethernet, or virtual adapters) the
    // default route is frequently not the speaker's subnet, so a single
    // default-resolved URL would be unreachable and playback would be silent.
    let stream_url = resolve_stream_url(&server, control.as_ref());

    tracing::info!(%stream_url, %content_type, output_rate, "bridge session starting");

    // Shared between the encode thread (which observes every block) and the
    // supervisor below (which acts on the transitions).
    let idle = Arc::new(IdleTracker::new(
        config.idle.stop_after(),
        config.idle.silence_threshold(),
    ));
    if idle.is_enabled() {
        tracing::info!(
            release_after_secs = config.idle.stop_after_secs,
            "idle release enabled; the room is handed back while Windows is silent"
        );
    } else {
        tracing::info!("idle release disabled; the room stays occupied for the whole session");
    }

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
        idle: Arc::clone(&idle),
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
    idle.mark_resumed();

    // Supervise until a stop is requested or the encode loop ends on its own.
    if let Some(arbitration) = arbitration {
        arbitration.claim();
    }
    supervise(
        &server,
        &handle,
        control.as_ref(),
        &stream_url,
        stop,
        metrics,
        &idle,
        arbitration,
    );

    if let Some(arbitration) = arbitration {
        arbitration.release();
    }

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

/// Resolve the stream URL to advertise to the player.
///
/// When the player exposes its own LAN address, the URL is tailored to the local
/// interface that routes to it (multi-homed-correct); otherwise the server's
/// default-resolved URL is used (e.g. the loopback test harness).
fn resolve_stream_url(server: &StreamServer, control: &dyn PlayerControl) -> String {
    control.peer_ip().map_or_else(
        || server.url().to_owned(),
        |peer| server.stream_url_for(peer),
    )
}

/// Poll until a stop is requested or the encode thread finishes, releasing the
/// room while Windows is silent and re-pointing the player if a previously
/// connected client drops away for long enough.
///
/// Reconnection is best-effort telemetry: a transient client drop on the LAN
/// should not end the session, and a debounce avoids thrashing on a clean
/// disconnect that is immediately followed by teardown.
///
/// The two mechanisms have to be aware of each other, because releasing the
/// room *causes* a client drop: without the `is_released` guard the reconnect
/// logic would immediately grab the speakers back and idle release would never
/// hold.
fn supervise(
    server: &StreamServer,
    handle: &thread::JoinHandle<Result<()>>,
    control: &dyn PlayerControl,
    stream_url: &str,
    stop: &Arc<AtomicBool>,
    metrics: &Arc<SharedMetrics>,
    idle: &IdleTracker,
    arbitration: Option<&Arbitration>,
) {
    const POLL: Duration = Duration::from_millis(50);
    const RECONNECT_DEBOUNCE: Duration = Duration::from_millis(750);

    let content_type = server.config().content_type.clone();
    let mut state = SupervisorState::new();

    while !stop.load(Ordering::SeqCst) && !handle.is_finished() {
        thread::sleep(POLL);

        match idle.poll() {
            IdleAction::Release => {
                tracing::info!("no audio; releasing the room");
                if let Err(error) = control.stop() {
                    tracing::warn!(%error, "failed to stop the player for idle release");
                }
                // Hand the rooms back so a session that is still playing can
                // pick them up instead of leaving them silent.
                if let Some(arbitration) = arbitration {
                    arbitration.release();
                }
                state.active_rooms = None;
                state.preempted = false;
                // The player disconnecting is the expected consequence, not a
                // fault to reconnect from.
                state.forget_client();
                continue;
            }
            IdleAction::Resume => {
                tracing::info!("audio resumed; reclaiming the room");
                // Audio starting is what wins an argument, so claim before
                // touching the player: this is the moment that outranks
                // whoever was playing.
                if let Some(arbitration) = arbitration {
                    arbitration.claim();
                }
                state.active_rooms = None;
                state.preempted = false;
                match start_playback(control, stream_url, &content_type) {
                    Ok(()) => metrics.inc_reconnects(),
                    Err(error) => {
                        tracing::warn!(%error, "failed to resume playback after idle");
                        // Try again on the next audible poll rather than
                        // staying silently released for the rest of the
                        // session.
                        idle.mark_released_for_retry();
                    }
                }
                state.forget_client();
                continue;
            }
            IdleAction::None => {}
        }

        // While released we deliberately have no client; that is not a drop.
        if idle.is_released() {
            continue;
        }

        if let Some(arbitration) = arbitration {
            if !apply_ownership(
                arbitration,
                control,
                stream_url,
                &content_type,
                metrics,
                &mut state,
            ) {
                continue;
            }
        }

        if server.metrics().connected_clients > 0 {
            state.seen_client = true;
            state.dropped_at = None;
            continue;
        }

        if !state.seen_client {
            continue;
        }

        match state.dropped_at {
            None => state.dropped_at = Some(Instant::now()),
            Some(since) if since.elapsed() >= RECONNECT_DEBOUNCE => {
                match start_playback(control, stream_url, &content_type) {
                    Ok(()) => {
                        metrics.inc_reconnects();
                        idle.mark_resumed();
                        tracing::info!("re-pointed the player after a client drop");
                    }
                    Err(error) => tracing::warn!(%error, "player reconnect attempt failed"),
                }
                state.forget_client();
            }
            Some(_) => {}
        }
    }
}

/// The bits of [`supervise`] that have to survive from one poll tick to the
/// next.
struct SupervisorState {
    /// A client has connected at least once since playback last started.
    seen_client: bool,
    /// When the last client went away, for the reconnect debounce.
    dropped_at: Option<Instant>,
    /// Rooms last handed to the player, so an unchanged membership costs
    /// nothing.
    active_rooms: Option<Vec<String>>,
    /// Another output currently owns every room this session wanted.
    preempted: bool,
}

impl SupervisorState {
    fn new() -> Self {
        Self {
            seen_client: false,
            dropped_at: None,
            active_rooms: None,
            preempted: false,
        }
    }

    /// Forget the current client. Called whenever *we* stop or restart
    /// playback, so the disconnect we just caused is not mistaken for a fault
    /// worth reconnecting from.
    fn forget_client(&mut self) {
        self.seen_client = false;
        self.dropped_at = None;
    }
}

/// Brings the player in line with the shared ownership map.
///
/// Returns `false` when this session owns nothing right now, meaning the rest
/// of the tick has nothing to supervise.
fn apply_ownership(
    arbitration: &Arbitration,
    control: &dyn PlayerControl,
    stream_url: &str,
    content_type: &str,
    metrics: &SharedMetrics,
    state: &mut SupervisorState,
) -> bool {
    let owned = arbitration.reconcile();

    if owned.is_empty() {
        if !state.preempted {
            tracing::info!("another output took every room; standing down");
            if let Err(error) = control.stop() {
                tracing::warn!(%error, "failed to stop the player after pre-emption");
            }
            state.preempted = true;
            state.active_rooms = None;
            state.forget_client();
        }
        return false;
    }

    if state.preempted {
        tracing::info!(rooms = owned.len(), "rooms came back; resuming");
        if let Err(error) = start_playback(control, stream_url, content_type) {
            tracing::warn!(%error, "failed to resume after pre-emption");
            return false;
        }
        metrics.inc_reconnects();
        state.preempted = false;
        state.forget_client();
    }

    // Only the aggregate output acts on this; for a single room it is a no-op,
    // which is why it is safe to call unconditionally.
    if state.active_rooms.as_ref() != Some(&owned) {
        if let Err(error) = control.set_active_rooms(&owned) {
            tracing::warn!(%error, "failed to regroup after an ownership change");
        } else {
            state.active_rooms = Some(owned);
        }
    }

    true
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
    idle: Arc<IdleTracker>,
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
                    // Observed before the pipeline, so the answer reflects what
                    // Windows actually played rather than a resampler's
                    // warm-up or the jitter buffer's fill state.
                    self.idle.observe(&frame);
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

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::{apply_ownership, SupervisorState};
    use crate::arbiter::{Arbiter, Arbitration, SessionId};
    use crate::metrics::SharedMetrics;
    use crate::player::PlayerControl;
    use audio_core::Result;
    use std::sync::Arc;

    /// Records what the supervisor asked of the player, standing in for a real
    /// Sonos coordinator.
    #[derive(Default)]
    struct SpyPlayer {
        stops: Mutex<u32>,
        plays: Mutex<u32>,
        rooms: Mutex<Vec<Vec<String>>>,
    }

    impl PlayerControl for SpyPlayer {
        fn set_stream_uri(&self, _stream_url: &str, _content_type: &str) -> Result<()> {
            Ok(())
        }

        fn play(&self) -> Result<()> {
            *self.plays.lock().expect("plays") += 1;
            Ok(())
        }

        fn stop(&self) -> Result<()> {
            *self.stops.lock().expect("stops") += 1;
            Ok(())
        }

        fn set_active_rooms(&self, room_uuids: &[String]) -> Result<()> {
            self.rooms.lock().expect("rooms").push(room_uuids.to_vec());
            Ok(())
        }
    }

    fn rooms(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    fn tick(
        arbitration: &Arbitration,
        player: &SpyPlayer,
        metrics: &SharedMetrics,
        state: &mut SupervisorState,
    ) -> bool {
        apply_ownership(
            arbitration,
            player,
            "http://127.0.0.1:9000/stream",
            "audio/flac",
            metrics,
            state,
        )
    }

    #[test]
    fn the_aggregate_regroups_around_the_rooms_it_still_owns() {
        let arbiter = Arc::new(Arbiter::new());
        let all = Arbitration::new(
            Arc::clone(&arbiter),
            SessionId(1),
            rooms(&["kitchen", "bedroom"]),
        );
        let player = SpyPlayer::default();
        let metrics = SharedMetrics::default();
        let mut state = SupervisorState::new();

        all.claim();
        assert!(tick(&all, &player, &metrics, &mut state));
        assert_eq!(
            *player.rooms.lock().expect("rooms"),
            vec![rooms(&["kitchen", "bedroom"])],
            "the party must first form around every room"
        );

        // The bedroom's own endpoint starts playing: most recent wins.
        arbiter.claim(SessionId(2), &rooms(&["bedroom"]));
        assert!(tick(&all, &player, &metrics, &mut state));
        assert_eq!(
            player.rooms.lock().expect("rooms").last(),
            Some(&rooms(&["kitchen"])),
            "the party must shrink to what it still owns"
        );
        assert_eq!(
            *player.stops.lock().expect("stops"),
            0,
            "the party kept playing, so it must not have been stopped"
        );

        // The bedroom goes quiet; the party takes it back rather than leaving
        // the room silent.
        arbiter.release_all(SessionId(2));
        assert!(tick(&all, &player, &metrics, &mut state));
        assert_eq!(
            player.rooms.lock().expect("rooms").last(),
            Some(&rooms(&["kitchen", "bedroom"])),
        );
    }

    #[test]
    fn an_unchanged_membership_costs_no_regroup() {
        let arbiter = Arc::new(Arbiter::new());
        let all = Arbitration::new(Arc::clone(&arbiter), SessionId(1), rooms(&["kitchen"]));
        let player = SpyPlayer::default();
        let metrics = SharedMetrics::default();
        let mut state = SupervisorState::new();

        all.claim();
        for _ in 0..5 {
            assert!(tick(&all, &player, &metrics, &mut state));
        }
        assert_eq!(
            player.rooms.lock().expect("rooms").len(),
            1,
            "regrouping every 50 ms tick would hammer the household with UPnP calls"
        );
    }

    #[test]
    fn losing_every_room_stands_the_session_down_exactly_once() {
        let arbiter = Arc::new(Arbiter::new());
        let one = Arbitration::new(Arc::clone(&arbiter), SessionId(1), rooms(&["kitchen"]));
        let player = SpyPlayer::default();
        let metrics = SharedMetrics::default();
        let mut state = SupervisorState::new();

        one.claim();
        assert!(tick(&one, &player, &metrics, &mut state));

        arbiter.claim(SessionId(2), &rooms(&["kitchen"]));
        for _ in 0..5 {
            assert!(
                !tick(&one, &player, &metrics, &mut state),
                "a session owning nothing has nothing to supervise"
            );
        }
        assert_eq!(
            *player.stops.lock().expect("stops"),
            1,
            "standing down repeatedly would spam the player with stop calls"
        );
    }

    #[test]
    fn a_returning_room_resumes_playback() {
        let arbiter = Arc::new(Arbiter::new());
        let one = Arbitration::new(Arc::clone(&arbiter), SessionId(1), rooms(&["kitchen"]));
        let player = SpyPlayer::default();
        let metrics = SharedMetrics::default();
        let mut state = SupervisorState::new();

        one.claim();
        assert!(tick(&one, &player, &metrics, &mut state));

        arbiter.claim(SessionId(2), &rooms(&["kitchen"]));
        assert!(!tick(&one, &player, &metrics, &mut state));

        arbiter.release_all(SessionId(2));
        assert!(tick(&one, &player, &metrics, &mut state));
        assert_eq!(
            *player.plays.lock().expect("plays"),
            1,
            "the session must start playing again, not just re-own the room"
        );
        assert_eq!(metrics.snapshot().reconnects, 1);
    }
}
