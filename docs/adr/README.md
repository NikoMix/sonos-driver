# Architecture Decision Records

This directory captures the significant, hard-to-reverse decisions behind
`sonos-driver`, using the lightweight [ADR](https://adr.github.io/) format.

Each record is immutable once accepted: to change a decision, add a new ADR that
supersedes the old one (and update the old one's status to "Superseded by ADR-N").

## Index

- [0001 — User-mode WASAPI loopback (no kernel driver for v1)](./0001-user-mode-wasapi-loopback.md)
- [0002 — Modular `Encoder`/`Transport`/`AudioSource` trait contracts](./0002-modular-codec-and-transport-traits.md)
- [0003 — Honest latency targets (no sub-100 ms promise)](./0003-honest-latency-targets.md)

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
