//! End-to-end, hardware-free latency harness for the bridge run loop.
//!
//! Drives [`Bridge::run_with`] with a paced [`SyntheticSource`] and an
//! in-process [`PlayerControl`] mock, pulls the encoded stream over loopback
//! HTTP, and asserts the loop publishes audio, records controllable latency,
//! honours `StopHandle::stop`, and tears the player down. No audio hardware and
//! no Sonos device are required, so it runs in CI. See
//! `docs/adr/0007-orchestration-testability-seams.md`.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use audio_core::{
    AudioFormat, AudioFrame, AudioSource, CodecProfile, Config, Error, PipelineConfig, Result,
    StreamConfig, SyntheticSource,
};
use sonos_driver_core::{Bridge, PlayerControl};

/// Wraps a [`SyntheticSource`], sleeping for each block's wall-clock duration so
/// the otherwise free-running generator emits audio at real time — mimicking a
/// live capture device and exercising the loop's pacing.
struct PacedSource {
    inner: SyntheticSource,
}

impl PacedSource {
    fn new(inner: SyntheticSource) -> Self {
        Self { inner }
    }
}

impl AudioSource for PacedSource {
    fn format(&self) -> AudioFormat {
        self.inner.format()
    }

    fn read(&mut self) -> Result<Option<AudioFrame>> {
        let frame = self.inner.read()?;
        if let Some(ref frame) = frame {
            thread::sleep(frame.duration());
        }
        Ok(frame)
    }
}

/// The calls an in-process [`PlayerControl`] mock observed.
#[derive(Default)]
struct MockCalls {
    set_uri: u32,
    play: u32,
    stop: u32,
    last_url: String,
    last_content_type: String,
}

/// An in-process [`PlayerControl`] that records what the bridge asked of it,
/// standing in for a real Sonos player with no device on the LAN.
struct MockPlayer {
    calls: Arc<Mutex<MockCalls>>,
}

impl MockPlayer {
    fn new() -> Self {
        Self {
            calls: Arc::new(Mutex::new(MockCalls::default())),
        }
    }

    fn calls(&self) -> Arc<Mutex<MockCalls>> {
        Arc::clone(&self.calls)
    }
}

impl PlayerControl for MockPlayer {
    fn set_stream_uri(&self, stream_url: &str, content_type: &str) -> Result<()> {
        let mut calls = self.calls.lock().expect("mock calls poisoned");
        calls.set_uri += 1;
        stream_url.clone_into(&mut calls.last_url);
        content_type.clone_into(&mut calls.last_content_type);
        Ok(())
    }

    fn play(&self) -> Result<()> {
        self.calls.lock().expect("mock calls poisoned").play += 1;
        Ok(())
    }

    fn stop(&self) -> Result<()> {
        self.calls.lock().expect("mock calls poisoned").stop += 1;
        Ok(())
    }
}

/// A loopback harness [`Config`] for `profile`: bind the stream server to an
/// ephemeral loopback port and resample to 44.1 kHz with an 80 ms buffer.
fn harness_config(profile: CodecProfile) -> Config {
    Config {
        stream: StreamConfig {
            profile,
            bind_address: "127.0.0.1".to_owned(),
            port: 0,
        },
        pipeline: PipelineConfig {
            target_sample_rate: Some(44_100),
            buffer_ms: 80,
        },
        ..Config::default()
    }
}

/// Block until the bridge points the mock player at a URL, returning it.
fn wait_for_url(calls: &Arc<Mutex<MockCalls>>, timeout: Duration) -> String {
    let deadline = Instant::now() + timeout;
    loop {
        {
            let guard = calls.lock().expect("mock calls poisoned");
            if guard.set_uri > 0 && !guard.last_url.is_empty() {
                return guard.last_url.clone();
            }
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for set_stream_uri"
        );
        thread::sleep(Duration::from_millis(20));
    }
}

/// Split `http://host:port/path` into its authority and path.
fn split_url(url: &str) -> (String, String) {
    let rest = url.strip_prefix("http://").expect("an http:// url");
    match rest.find('/') {
        Some(slash) => (rest[..slash].to_owned(), rest[slash..].to_owned()),
        None => (rest.to_owned(), "/".to_owned()),
    }
}

/// Open an HTTP/1.1 streaming GET against the server, returning a reader
/// positioned at the first chunk plus the advertised content type.
fn open_stream(authority: &str, path: &str) -> (BufReader<TcpStream>, String) {
    let stream = TcpStream::connect(authority).expect("connect to stream server");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("set read timeout");

    let request =
        format!("GET {path} HTTP/1.1\r\nHost: {authority}\r\nConnection: keep-alive\r\n\r\n");
    (&stream)
        .write_all(request.as_bytes())
        .expect("send request");

    let mut reader = BufReader::new(stream);
    let mut status = String::new();
    reader.read_line(&mut status).expect("read status line");
    assert!(
        status.starts_with("HTTP/1.1 200"),
        "unexpected status line: {status:?}"
    );

    let mut content_type = String::new();
    loop {
        let mut line = String::new();
        let read = reader.read_line(&mut line).expect("read header line");
        let trimmed = line.trim_end();
        if read == 0 || trimmed.is_empty() {
            break;
        }
        if let Some(value) = strip_header(trimmed, "content-type") {
            value.trim().clone_into(&mut content_type);
        }
    }
    (reader, content_type)
}

/// Case-insensitively strip a `name:` prefix from a header line.
fn strip_header<'a>(line: &'a str, name: &str) -> Option<&'a str> {
    let (head, value) = line.split_once(':')?;
    head.trim().eq_ignore_ascii_case(name).then_some(value)
}

/// Read and return the payload of one HTTP chunk, or an empty vector at the
/// terminating zero-length chunk.
fn read_one_chunk(reader: &mut BufReader<TcpStream>) -> Vec<u8> {
    let mut size_line = String::new();
    reader.read_line(&mut size_line).expect("read chunk size");
    let size_hex = size_line
        .trim_end()
        .split(';')
        .next()
        .unwrap_or_default()
        .trim();
    let size = usize::from_str_radix(size_hex, 16).expect("parse chunk size");
    if size == 0 {
        return Vec::new();
    }

    let mut payload = vec![0_u8; size];
    reader.read_exact(&mut payload).expect("read chunk payload");
    let mut crlf = [0_u8; 2];
    reader.read_exact(&mut crlf).expect("read chunk terminator");
    payload
}

#[test]
fn streaming_loop_publishes_audio_until_stopped() {
    const MIN_BODY_BYTES: usize = 4096;

    let bridge = Bridge::new(harness_config(CodecProfile::L16Pcm));
    let stop = bridge.stop_handle();

    // 48 kHz source → 44.1 kHz output exercises the resampler; 10 ms blocks.
    let source = PacedSource::new(SyntheticSource::new(48_000, 2, 440.0, 0.25, 480));
    let mock = MockPlayer::new();
    let calls = mock.calls();

    let worker = thread::spawn(move || {
        let mut bridge = bridge;
        let result = bridge.run_with(Box::new(source), Box::new(mock));
        (result, bridge.metrics(), bridge.controllable_latency())
    });

    // The mock records the URL the bridge dials, so the loopback client learns
    // the ephemeral port without the bridge having to expose it.
    let url = wait_for_url(&calls, Duration::from_secs(5));
    assert!(
        url.starts_with("http://127.0.0.1:"),
        "expected a loopback url, got {url:?}"
    );
    let (authority, path) = split_url(&url);
    let (mut reader, content_type) = open_stream(&authority, &path);
    // L16 is header-less (RFC 2586): the MIME type must carry the output rate
    // and channel count so the player interprets the stream correctly. The
    // 48 kHz source is resampled to the 44.1 kHz harness output, stereo.
    assert_eq!(content_type, "audio/L16;rate=44100;channels=2");

    let mut total = 0_usize;
    while total < MIN_BODY_BYTES {
        let chunk = read_one_chunk(&mut reader);
        if chunk.is_empty() {
            break;
        }
        total += chunk.len();
    }
    assert!(
        total >= MIN_BODY_BYTES,
        "expected at least {MIN_BODY_BYTES} streamed bytes, got {total}"
    );

    stop.stop();
    let (result, metrics, latency) = worker.join().expect("encode thread joined");
    result.expect("run_with returned Ok after a clean stop");

    assert!(metrics.frames_captured > 0, "no frames captured");
    assert!(metrics.frames_streamed > 0, "no frames streamed");
    assert_eq!(metrics.reconnects, 0, "unexpected reconnect");
    assert!(
        latency.samples > 0,
        "controllable latency was never measured"
    );
    assert!(latency.mean > Duration::ZERO, "mean latency was zero");

    let calls = calls.lock().expect("mock calls poisoned");
    assert!(calls.set_uri >= 1, "player was never pointed at the stream");
    assert!(calls.play >= 1, "player was never started");
    assert!(calls.stop >= 1, "player was never stopped on teardown");
    // The same corrected MIME flows into the DIDL-Lite protocolInfo the player
    // is handed, so Sonos sees the rate/channels for the header-less L16 stream.
    assert_eq!(calls.last_content_type, "audio/L16;rate=44100;channels=2");
}

#[test]
fn bounded_source_ends_without_an_explicit_stop() {
    let config = harness_config(CodecProfile::L16Pcm);
    // 5760 frames / 480 per read = 12 paced reads (~120 ms) then end-of-stream.
    let source =
        PacedSource::new(SyntheticSource::new(48_000, 2, 440.0, 0.25, 480).with_limit(5_760));
    let mock = MockPlayer::new();
    let calls = mock.calls();

    let (tx, rx) = mpsc::channel();
    let _worker = thread::spawn(move || {
        let mut bridge = Bridge::new(config);
        let result = bridge.run_with(Box::new(source), Box::new(mock));
        let _ = tx.send((result, bridge.metrics()));
    });

    // The bounded source guarantees termination; the timeout only stops a
    // regression from hanging CI.
    let (result, metrics) = rx
        .recv_timeout(Duration::from_secs(15))
        .expect("bounded run terminated on its own");
    result.expect("run_with returned Ok at end-of-stream");

    assert!(metrics.frames_captured > 0, "no frames captured");
    assert!(metrics.frames_streamed > 0, "no frames streamed");

    let calls = calls.lock().expect("mock calls poisoned");
    assert!(calls.set_uri >= 1, "player was never pointed at the stream");
    assert!(calls.stop >= 1, "player was never stopped on teardown");
}

#[test]
fn unsupported_profile_fails_before_touching_the_player() {
    let config = harness_config(CodecProfile::Aac);
    let source = SyntheticSource::new(48_000, 2, 440.0, 0.25, 480);
    let mock = MockPlayer::new();
    let calls = mock.calls();

    let mut bridge = Bridge::new(config);
    let result = bridge.run_with(Box::new(source), Box::new(mock));
    assert!(
        matches!(result, Err(Error::NotImplemented(_))),
        "expected NotImplemented for an unsupported profile"
    );

    let calls = calls.lock().expect("mock calls poisoned");
    assert_eq!(calls.set_uri, 0, "player was contacted despite a failure");
    assert_eq!(calls.play, 0, "player was started despite a failure");
}

/// A paced source that emits digital silence until `audible` is set, then a
/// tone. Stands in for a Windows machine that nobody is playing anything on.
struct QuietThenLoudSource {
    inner: SyntheticSource,
    audible: Arc<AtomicBool>,
}

impl QuietThenLoudSource {
    fn new(audible: Arc<AtomicBool>) -> Self {
        Self {
            inner: SyntheticSource::new(48_000, 2, 440.0, 0.25, 480),
            audible,
        }
    }
}

impl AudioSource for QuietThenLoudSource {
    fn format(&self) -> AudioFormat {
        self.inner.format()
    }

    fn read(&mut self) -> Result<Option<AudioFrame>> {
        let frame = self.inner.read()?;
        let Some(mut frame) = frame else {
            return Ok(None);
        };
        if !self.audible.load(Ordering::SeqCst) {
            frame.samples_mut().fill(0.0);
        }
        thread::sleep(frame.duration());
        Ok(Some(frame))
    }
}

/// Wait until `probe` holds, or fail with `what`.
fn wait_until<F>(calls: &Arc<Mutex<MockCalls>>, timeout: Duration, what: &str, probe: F)
where
    F: Fn(&MockCalls) -> bool,
{
    let deadline = Instant::now() + timeout;
    loop {
        if probe(&calls.lock().expect("mock calls poisoned")) {
            return;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn silence_releases_the_room_and_audio_reclaims_it() {
    let mut config = harness_config(CodecProfile::L16Pcm);
    // One second, the shortest the config granularity allows, to keep the test
    // honest without making it slow.
    config.idle.stop_after_secs = 1;

    let audible = Arc::new(AtomicBool::new(false));
    let source = QuietThenLoudSource::new(Arc::clone(&audible));
    let mock = MockPlayer::new();
    let calls = mock.calls();

    let mut bridge = Bridge::new(config);
    let stop = bridge.stop_handle();
    let worker = thread::spawn(move || bridge.run_with(Box::new(source), Box::new(mock)));

    // Playback starts immediately: the room is claimed before anyone knows
    // whether there is anything to play.
    wait_until(&calls, Duration::from_secs(10), "initial play", |c| {
        c.play >= 1
    });

    // Silence must then hand it back, without ending the session.
    wait_until(&calls, Duration::from_secs(10), "idle release", |c| {
        c.stop >= 1
    });
    assert!(
        !worker.is_finished(),
        "the session ended instead of merely releasing the room"
    );

    // And sound must take it back.
    audible.store(true, Ordering::SeqCst);
    wait_until(&calls, Duration::from_secs(10), "idle resume", |c| {
        c.play >= 2
    });

    stop.stop();
    worker
        .join()
        .expect("session thread panicked")
        .expect("session returned Ok");

    let calls = calls.lock().expect("mock calls poisoned");
    assert!(
        calls.set_uri >= 2,
        "resuming must re-point the player, not just press play"
    );
}
