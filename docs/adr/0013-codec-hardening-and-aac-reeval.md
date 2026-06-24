# 0013 — Codec `unsafe`-forbid parity, AAC re-evaluation, and the `audio-core` name collision

- Status: Accepted
- Date: 2026-06-25

## Context

Phase 4 ("complete the implementation") includes a hardening pass over the
`audio-codec` crate plus two record-keeping decisions that surfaced during the
Phase 3 quality-telemetry review (`docs/QUALITY.md`):

1. **`unsafe`-lint parity.** Every first-party crate except `audio-codec` declares
   `#![forbid(unsafe_code)]`. `audio-capture` is the deliberate exception — its WASAPI
   COM/FFI island carries `#[allow(unsafe_code)]` (ADR-0001, ADR-0006). `cargo-geiger`
   confirmed `audio-codec` compiles **zero** `unsafe` (0/0), but it did not *declare*
   the lint, so the guarantee was incidental rather than enforced. A future contributor
   could add `unsafe` to the codec path and no gate would object.

2. **AAC/MP3 encoder availability.** ADR-0004 (WS-D) deferred AAC and MP3 and, in good
   faith, described `oxideav-aac` 0.1.5 as "a genuine pure-Rust AAC-LC encoder/decoder"
   that was merely *deferred* for maturity reasons — leaving the door open for near-term
   adoption. That description was taken from the crate's crates.io summary, which a
   Phase 4 source-level re-verification found to be misleading.

3. **`audio-core` name collision.** The dependency graph contains a **third-party**
   `audio-core 0.2.1` (crates.io), pulled in transitively by the resampling stack
   (`rubato` → `audioadapter` → `audio-core`). It shares a name with this workspace's
   internal `audio-core 0.1.0`. This produced cosmetic noise in `cargo-geiger` output and
   raised the question of whether the internal crate should be renamed.

## Decision

### 1. Declare `#![forbid(unsafe_code)]` in `audio-codec`

`crates/audio-codec/src/lib.rs` now declares `#![forbid(unsafe_code)]`. The crate already
used no `unsafe`, so this is behaviour-preserving; it converts an incidental property into
a compiler-enforced invariant and brings the codec crate to parity with `audio-core`,
`audio-pipeline`, `stream-server`, `sonos-control`, `sonos-driver-core`, and
`sonos-driver-cli`. `audio-capture` remains the single, documented `unsafe` island.

### 2. Keep AAC/MP3 `NotImplemented`; correct the ADR-0004 record

A source-level re-verification of `oxideav-aac` 0.1.5 shows it is **decoder-only**: the
encoder back-end (MDCT analysis, psychoacoustic model, bit-allocation, rate-control) is
not implemented; only AAC bitstream *write primitives* exist, with no analysis chain to
drive them, and the crate registers only the AAC *decoder*. There is therefore **no
licence-clean, pure-Rust AAC encoder** available today:

| Candidate | Encoder today? | Pure Rust? | Licence | Verdict |
| --- | --- | --- | --- | --- |
| `oxideav-aac` 0.1.5 | **no** (decoder-only) | yes | MIT | Licence-clean but cannot encode |
| `fdk-aac` / `fdk-aac-sys` | yes | no (FFI → Fraunhofer FDK) | non-free FDK + patents | Rejected (licence + FFI) |
| `symphonia` 0.5/0.6 | no (decode-only) | yes | MPL-2.0 | Not an encoder |
| `faac` / `faac-sys` | — | — | — | Not published on crates.io |

Hand-writing a spec-compliant AAC-LC encoder is a multi-month undertaking that would
yield a **lossy** codec with **higher** end-to-end latency (~1200–3000 ms incl. the fixed
Sonos buffer) than the **lossless** FLAC path the bridge already ships (~400–1000 ms,
bit-transparent). Building it is therefore net-negative for this project's goals.

**Decision:** `encoder_for(CodecProfile::Aac, _)` and `(CodecProfile::Mp3, _)` continue to
return `audio_core::Error::NotImplemented`. ADR-0004 has been annotated with an
"Update — Phase 4 re-evaluation" section that corrects the encoder-availability claim.

**Re-evaluation trigger (issue #18):** revisit if and when `oxideav-aac` (or another
permissively licensed, pure-Rust crate) ships a *working* encoder back-end with a stable
release, at which point the previously documented perceptual-acceptance-test work
(independent decoder + bounded reconstruction-error metric) becomes the gating task.

### 3. Keep the internal `audio-core` name (do not rename)

The internal crate **keeps** the name `audio-core`. The collision with the unrelated
crates.io `audio-core 0.2.1` is harmless: Cargo resolves and links crates by their
*source* (a workspace **path** dependency vs a **registry** dependency), not by name
alone, so both coexist in the graph without ambiguity. Renaming the internal crate would
touch all eight crates' `Cargo.toml` and every `use audio_core::…` path for **zero**
functional benefit — the workspace is an application, not a published library, so there is
no external consumer that could be confused. The cost (a large, conflict-prone diff across
the whole tree) is not justified. Documented here and in `docs/QUALITY.md` so the cosmetic
geiger noise is not mistaken for a defect.

## Consequences

- ✅ `#![forbid(unsafe_code)]` is now enforced across **all** pure-Rust first-party crates;
  `audio-capture` stays the sole, intentional `unsafe` island.
- ✅ The ADR record is now factually accurate: no pure-Rust AAC encoder exists yet, so the
  deferral is grounded in verified evidence rather than a misread crate description.
- ✅ Callers keep a precise contract — AAC/MP3 yield a clear `NotImplemented`, never a
  silent or broken fallback. FLAC remains the lossless quality default; L16/WAV remain the
  lowest-latency paths. No target Sonos device is left unserved (all accept FLAC + PCM/WAV).
- ✅ No dependency, public API, or runtime behaviour changed; all six quality gates remain
  green and `cargo audit` stays at zero advisories.
- ⚠️ AAC/MP3 compatibility is still unavailable; the trigger above keeps the door open
  without committing to an early, unproven, or non-free codec.
