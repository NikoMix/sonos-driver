# Architecture Decision Records

This directory captures the significant, hard-to-reverse decisions behind
`sonos-driver`, using the lightweight [ADR](https://adr.github.io/) format.

Each record is immutable once accepted: to change a decision, add a new ADR that
supersedes the old one (and update the old one's status to "Superseded by ADR-N").

## Index

- [0001 — User-mode WASAPI loopback (no kernel driver for v1)](./0001-user-mode-wasapi-loopback.md)
- [0002 — Modular `Encoder`/`Transport`/`AudioSource` trait contracts](./0002-modular-codec-and-transport-traits.md)
- [0003 — Honest latency targets (no sub-100 ms promise)](./0003-honest-latency-targets.md)
- [0004 — FLAC adopted; AAC/MP3 deferred (licensing + maturity)](./0004-aac-mp3-codec-evaluation.md)
- [0005 — `stream-server`: async `axum` HTTP with a broadcast ring buffer](./0005-stream-server-http.md)
- [0006 — `windows`(-rs) bindings with self-managed WASAPI FFI for `audio-capture`](./0006-windows-rs-wasapi-bindings.md)
- [0007 — Dependency-injected orchestration seams for hardware-free testing](./0007-orchestration-testability-seams.md)
- [0008 — CLI configuration precedence, graceful stop, and control commands](./0008-cli-config-and-signals.md)
- [0009 — Dependency upgrade policy and the rubato/audioadapter version pin](./0009-dependency-upgrade-policy.md)
- [0010 — Portable release packaging via a PowerShell script (no extra crate or xtask)](./0010-portable-release-packaging.md)
- [0013 — Codec `unsafe`-forbid parity, AAC re-evaluation, and the `audio-core` name collision](./0013-codec-hardening-and-aac-reeval.md)

## Template

```markdown
# NNNN — Title

- Status: Proposed | Accepted | Superseded by ADR-XXXX
- Date: YYYY-MM-DD

## Context
What is the problem and the forces at play?

## Decision
What did we decide, stated plainly?

## Consequences
What becomes easier or harder as a result? Trade-offs and follow-ups.
```
