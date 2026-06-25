# 0016 — Default streaming profile is FLAC (Sonos rejects live L16/WAV)

- Status: Accepted
- Date: 2026-06-26

## Context

On real hardware — once multi-homed discovery (#38) and the per-target
advertised stream URL (#40) let a Sonos actually fetch the stream — playback
still failed for the historical default profile. Observed on a live Sonos
system:

| Profile | Result |
|---------|--------|
| `l16-pcm` (the old default) | ❌ "unable to play 'stream' — the file is in an unsupported format" |
| `wav` (RIFF/WAVE PCM) | ❌ "the song is not encoded correctly" |
| `flac` | ✅ plays |

The root cause is the **live-edge transport**, not the encoders. `stream-server`
fans audio out over a `tokio::broadcast` ring buffer: a client receives only the
chunks produced *after* it subscribes, and `BroadcastSink::write` discards bytes
when no client is connected. The encoder's one-time **prelude** (container
header) is written before the Sonos connects, so it is dropped. A player joining
the running stream therefore never sees a container header.

- Raw **L16** carries no in-band framing at all → the player cannot recognise the
  byte stream as playable PCM → "unsupported format".
- **WAV** needs its 44-byte RIFF/`data` header, which the live-edge model drops →
  the player sees headerless PCM where it expects RIFF → "not encoded correctly".
- **FLAC** frames are **self-describing**: every frame header carries the sample
  rate, channel count and bit depth, and begins with a sync code. A decoder that
  joins mid-stream resyncs on the next frame and decodes correctly **without** the
  `fLaC`/`STREAMINFO` prelude. This is exactly why FLAC is the only profile that
  survives the live-edge broadcast on real Sonos hardware.

This was confirmed empirically on the owner's hardware and is consistent with the
known behaviour of Sonos's UPnP `AVTransport` renderer.

## Decision

Make **`flac` the default streaming profile** (`CodecProfile::default_profile()`),
across the CLI and the GUI, because it is the only profile reliably playable over
the current live-edge transport on real Sonos players.

- `l16-pcm` (lowest latency) and `wav` (broad compatibility) remain fully
  selectable for renderers that accept them; only the *default* changes.
- The GUI codec picker leads with the default; the CLI `config example` template
  and the documented defaults all reflect `flac`.

This **revises only the default-profile selection** in
[ADR-0003](0003-honest-latency-targets.md). ADR-0003's latency-floor analysis is
unchanged, and the `low-latency` (L16) profile it describes is still available —
FLAC's `Balanced` class (~400–1000 ms) is well within the multi-second Sonos
buffer, so the change costs ~50 ms of best-case latency for a reliably playable
default. ADR-0003 stays the record for the honest-latency decision.

## Consequences

- ✅ Streaming to a real Sonos works out of the box: a fresh install (no config
  file, GUI defaults) now plays captured system audio.
- ✅ The default stays **lossless**, at roughly half the bandwidth of raw PCM.
- ✅ No new dependencies, no licence impact: `flacenc` (pure-Rust) already ships
  and is exercised by bit-exact round-trip tests (ADR-0004).
- ⚠️ Best-case latency rises from the `Low` (~350–850 ms) to the `Balanced`
  (~400–1000 ms) class — negligible against the fixed Sonos buffer. Users who want
  the absolute floor on a tolerant renderer can still select `l16-pcm`.
- ⚠️ `l16-pcm` and `wav` remain second-class over the live edge. A future
  enhancement — a **sticky-prelude replay** that re-sends each new client the
  container header before it joins the live edge — would make them viable too;
  it is deliberately out of scope here so the working transport is untouched and
  the real-hardware fix lands now. Tracked as a follow-up.
