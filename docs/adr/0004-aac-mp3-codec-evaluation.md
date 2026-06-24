# 0004 — FLAC adopted; AAC/MP3 deferred (licensing + maturity)

- Status: Accepted
- Date: 2026-06-24

## Context

Workstream WS-D completes the `audio-codec` crate's compressed-profile coverage.
The `CodecProfile` set (ADR-0002) includes `Flac`, `Aac`, and `Mp3` in addition to
the uncompressed `L16Pcm`/`Wav` paths. The bridge's hard constraints shape which
encoders we can ship:

- **Pure Rust / memory-safe.** The workspace forbids `unsafe` in the contract crate
  (`#![forbid(unsafe_code)]`) and warns on it everywhere else. A C library reached
  through FFI (a `*-sys` crate) reintroduces the memory-safety and supply-chain risk
  the project exists to avoid, and complicates the single-binary, no-runtime-deps
  story.
- **Permissive licence, no paid vendor licence.** `cargo deny` enforces an allow-list
  (MIT, Apache-2.0, BSD-2/3, ISC, Zlib, Unicode-3.0, MPL-2.0, CC0-1.0). Copyleft
  (GPL/LGPL) and bespoke/non-free codec licences are rejected, as are dependencies
  carrying patent-royalty obligations.
- **Quality, stability, security over speed.** A lossy encoder on a quality-sensitive
  audio path must be production-credible and independently testable, not merely
  present.

This ADR records the evaluation of FLAC, AAC, and MP3 encoders against those
constraints and the resulting decision for each profile.

## Decision

### FLAC — adopted and enabled

`FlacEncoder` is implemented with the pure-Rust [`flacenc`](https://crates.io/crates/flacenc)
crate (Apache-2.0) and wired into `encoder_for(CodecProfile::Flac, _)`. It streams the
`fLaC` + `STREAMINFO` header from `prelude()`, encodes fixed-size frames into the
caller's buffer, and emits the (possibly shorter) trailing frame from `finish()`. The
`f32 → i16` mapping is shared with the PCM encoders so the integer signal is identical
across L16/WAV/FLAC. A round-trip test decodes the output with the independent pure-Rust
[`claxon`](https://crates.io/crates/claxon) decoder (Apache-2.0, a dev-dependency) and
asserts **bit-exact** recovery across block-aligned, partial-final-frame,
tiny-final-frame, sub-block, and mono cases. FLAC is the lossless quality default; with
`flacenc`'s default features disabled the dependency tree stays small and is fully
`cargo deny` / `cargo audit` clean.

### AAC — deferred; keep `NotImplemented`

`encoder_for(CodecProfile::Aac, _)` returns `Error::NotImplemented` with a message that
points here.

Survey of AAC encoders available on crates.io:

| Crate | Encoder? | Pure Rust? | Licence | Verdict |
| --- | --- | --- | --- | --- |
| `fdk-aac` / `fdk-aac-sys` | yes | no (FFI to Fraunhofer FDK) | non-free FDK licence + patent terms | **Rejected** — fails licence + `unsafe`/FFI |
| `faac`-based bindings | yes | no (FFI) | GPL | **Rejected** — copyleft + FFI |
| `oxideav-aac` 0.1.5 | yes (AAC-LC) | **yes** (no C, no FFI) | **MIT** | **Licence-clean, but deferred** |

`oxideav-aac` (MIT) is a genuine pure-Rust AAC-LC encoder/decoder whose only dependency
is `oxideav-core` (MIT, "no C deps"); both would pass `cargo deny`. It is therefore the
first AAC option that satisfies the licensing and memory-safety constraints. We still
defer enabling it in WS-D, because:

- **Maturity.** First published 2026-04-17, currently `0.1.x` with a low download count,
  a single maintainer, and no published API docs. The sibling `oxideav-mp3` crate has
  had *all* of its real (`0.1.x`) releases yanked (see below) — an ecosystem-churn signal
  that argues against pinning an early `0.1` codec into a quality-critical path right now.
- **Acceptance testing.** AAC is lossy, so the FLAC strategy (assert bit-exact decode)
  does not apply. A credible, non-circular acceptance test needs an *independent* decoder
  (e.g. Symphonia's AAC decoder) plus a bounded reconstruction-error metric. Designing and
  tuning that is a workstream of its own and is out of WS-D's scope.

Deferring keeps WS-D focused on its lossless deliverable and keeps every quality gate
green with certainty, while not foreclosing AAC: adoption of `oxideav-aac` is tracked as
a dedicated follow-up workstream with proper perceptual testing.

### MP3 — keep `NotImplemented`

`encoder_for(CodecProfile::Mp3, _)` returns `Error::NotImplemented` with a message that
points here. There is no viable pure-Rust, permissively licensed MP3 *encoder*:

- `mp3lame-encoder` (0.2.x) — high-level bindings to the C LAME library; categorised
  `external-ffi-bindings`; **LGPL-3.0**. Fails both the FFI/`unsafe` and the permissive
  licence constraints.
- `shine` — a C fixed-point encoder reached through FFI; same objections.
- `oxideav-mp3` — a pure-Rust MIT crate, but **all `0.1.x` versions are yanked**; the
  highest non-yanked version is a `0.0.0` placeholder, so there is no usable release.

Existing pure-Rust MP3 work targets *decoding* (`minimp3`, `puremp3`, Symphonia), not
encoding. MP3 therefore stays unsupported until a maintained, permissively licensed,
pure-Rust encoder exists, or we choose to build one.

## Consequences

- ✅ FLAC gives a license-clean, pure-Rust, **lossless** compressed profile with a
  bit-exact round-trip proof; L16/WAV remain the lowest-latency paths.
- ✅ `#![forbid(unsafe_code)]` holds across the codec crate — no FFI was introduced.
- ✅ All six quality gates (fmt, clippy `-D warnings`, test, release build, `cargo deny`,
  `cargo audit`) stay green; no copyleft, non-free, or vulnerable dependency was added.
- ✅ Callers can rely on a precise contract: requesting AAC or MP3 yields a clear
  `Error::NotImplemented` rather than a silent fallback or a broken stream.
- ⚠️ AAC/MP3 compatibility is unavailable for now. Mitigation: Sonos players accept FLAC
  and PCM/WAV, which the bridge already provides, so no target device is left unserved.
- ⚠️ The AAC finding is time-sensitive: a license-clean pure-Rust encoder now exists, so
  this decision should be revisited on its own merits (tracked as a follow-up), not
  treated as permanent.
