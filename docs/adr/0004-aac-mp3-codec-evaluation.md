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
| `oxideav-aac` 0.1.5 | **no** (decoder-only) | **yes** (no C, no FFI) | **MIT** | **Licence-clean, but cannot encode yet** |

`oxideav-aac` (MIT, dependency `oxideav-core` MIT) is a genuine pure-Rust AAC effort and
would pass `cargo deny`, but a Phase 4 re-verification of v0.1.5 against the crate's own
source shows it is **decoder-only**: the `Encoder` side is not wired. Its README states the
bit-exact wire writers exist but "there is no rate-control / psychoacoustic encoder
back-end to drive them," and `register()` installs only the AAC *decoder*. There is thus no
PCM→AAC path to adopt today. Even once an encoder back-end ships, two further costs apply:

- **Maturity.** First published 2026-04-17, currently `0.1.x` with a low download count and
  a single maintainer. The sibling `oxideav-mp3` crate has had *all* of its real (`0.1.x`)
  releases yanked (see below) — an ecosystem-churn signal that argues against pinning an
  early `0.1` codec into a quality-critical path.
- **Acceptance testing.** AAC is lossy, so the FLAC strategy (assert bit-exact decode)
  does not apply. A credible, non-circular acceptance test needs an *independent* decoder
  (e.g. Symphonia's AAC decoder) plus a bounded reconstruction-error metric. Designing and
  tuning that is a workstream of its own.

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
- ⚠️ AAC/MP3 remain deferred. As re-verified in Phase 4 (2026-06-25), **no** licence-clean
  pure-Rust AAC *encoder* exists yet (`oxideav-aac` 0.1.5 is decoder-only). Revisit when
  `oxideav-aac` — or another permissive pure-Rust crate — ships a working encoder back-end;
  tracked in ADR-0013 and issue #18.

## Update — Phase 4 re-evaluation (2026-06-25)

Phase 4 re-verified the AAC/MP3 encoder landscape against crate **source** (not crates.io
descriptions). Findings: `oxideav-aac` 0.1.5 (MIT) is **decoder-only** — its encoder
back-end (MDCT analysis, psychoacoustic model, bit-allocation, rate-control) is not
implemented per the crate's own README; only bitstream write primitives exist. `fdk-aac`
binds the Fraunhofer FDK library, whose licence grants no patent rights and is not
permissive. `symphonia` is decode-only; `faac`/`faac-sys` are not published on crates.io.
Writing a spec-compliant AAC-LC encoder from scratch is a multi-month effort yielding a
*lossy* codec with *higher* latency (~1200–3000 ms) than the FLAC lossless path already
shipped (~400–1000 ms, bit-transparent) — net-negative for this project. **Decision
unchanged: `Aac`/`Mp3` keep returning `Error::NotImplemented`.** See ADR-0013 for the full
verdict and re-evaluation trigger. This supersedes the earlier "a license-clean pure-Rust
encoder now exists" framing, which relied on the crate's misleading description.
