# Latency budget (the honest version)

Streaming Windows audio to a Sonos player **cannot** achieve sub-100 ms latency in
software. This is not a limitation of this project's code — it is a property of how
Sonos players consume network audio.

## Why there is a floor

- Sonos players buffer **~300–800 ms** of audio internally for every network source
  (UPnP/AirPlay/streaming). This buffer is **not configurable** through any public
  API; it exists so playback stays glitch-free and, for multi-room, perfectly
  synchronised across rooms.
- The only genuinely low-latency Sonos inputs (~75 ms class) are **proprietary,
  hardware-only** paths (e.g. the HDMI/line-in on specific models, with SonosNet
  synchronisation). They are patent-protected with no public network protocol.
- Other network paths are no better: **AirPlay 2 ≈ 2 s**, generic **UPnP ≈ 1–3 s**.

Prior art (RoomRelay, SonosMirror) independently confirms the same ceiling.

## What we deliver instead

The lowest *achievable* network latency, by:

1. **Minimising every millisecond we control** — event-driven WASAPI capture, a
   shallow jitter buffer, no unnecessary copies, and a no-compression hot path.
2. **Offering uncompressed L16 PCM** as the default `low-latency` profile, which adds
   essentially zero encode latency and lets the stream reach the Sonos buffer floor.
3. **Being explicit** about the trade-off so expectations are set correctly.

## Per-stage budget

Approximate, one-way, for a typical LAN. The **Sonos buffer dominates** and is
outside our control.

| Stage | L16 PCM | FLAC | AAC |
|-------|---------|------|-----|
| WASAPI capture (event-driven) | ~10–20 ms | ~10–20 ms | ~10–20 ms |
| Pipeline / resample | ~1–5 ms | ~1–5 ms | ~1–5 ms |
| Encode | ~0 ms | ~10–40 ms | ~100+ ms |
| HTTP transport (LAN) | ~5–20 ms | ~5–20 ms | ~5–20 ms |
| **Sonos buffer (fixed, not configurable)** | **~300–800 ms** | ~300–800 ms | ~1000–2500 ms |
| **Total (realistic)** | **~350–850 ms** | ~400–1000 ms | ~1200–3000 ms |

## Profiles

| Profile | Codec | Lossless | When to use |
|---------|-------|----------|-------------|
| `low-latency` | L16 PCM (`audio/L16`, big-endian, RFC 2586) | ✅ | Lowest latency; default. Highest bandwidth (~1.4 Mbps for CD stereo). |
| `wav` | RIFF/WAVE 16-bit PCM | ✅ | Same latency class as L16, broader player compatibility. |
| `flac` | FLAC (pure-Rust encoder) | ✅ | Lossless with ~½ the bandwidth; small added encode latency. |
| `aac` / `mp3` | lossy | ❌ | Maximum compatibility / lowest bandwidth; highest latency. |

## What "ultra-low-latency" means in this project

It means we are at the **physical floor the Sonos hardware imposes** (~350 ms with
L16 on a good LAN), not the sub-100 ms that is only possible over Sonos's private
hardware inputs. If you need true live-monitoring latency, no network-to-Sonos
software bridge — this one included — can provide it.

## Measuring it

The end-to-end latency harness (synthetic source → loopback HTTP sink) measures the
portion of latency **we** control. The Sonos buffer can only be characterised with a
real device and is documented here rather than asserted in CI.
