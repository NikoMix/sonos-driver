# Code quality & security telemetry

A reproducible snapshot of the project's quality and security posture. Every number
here was produced by the tool named — nothing is hand-waved or estimated. Re-run the
commands in [Reproducing this snapshot](#reproducing-this-snapshot) to refresh it.

- **Snapshot date:** 2026-06-25
- **Toolchain:** `rustc 1.96.0` / `cargo 1.96.0` (host `x86_64-pc-windows-msvc`)
- **Tools:** `cargo-geiger 0.13.0`, `cargo-llvm-cov 0.8.7`, `cargo-audit 0.22.2`,
  `cargo-deny 0.19.9`

> CI is currently billing-blocked, so the **local gate is the source of truth**. All
> commands below were run locally in an initialised MSVC environment.

## Local quality gate

| Gate | Command | Result |
|------|---------|--------|
| Format | `cargo fmt --all --check` | ✅ Clean (no diffs) |
| Lint | `cargo clippy --workspace --all-targets --all-features -- -D warnings` | ✅ **0 warnings** (pedantic on, warnings denied) |
| Tests | `cargo test --workspace --all-features` | ✅ **157 passed, 0 failed** (unit + integration + doc-tests) |
| Release build | `cargo build --workspace --release` | ✅ Succeeds |

The test count includes the hardware-free end-to-end latency harness
(`crates/sonos-driver-core/tests/e2e_latency.rs`), which drives the full
`run_with` lifecycle over a real loopback HTTP socket with a synthetic source and an
in-process mock player.

## `unsafe` usage — `cargo-geiger`

`cargo-geiger` counts `unsafe` expressions actually compiled into the build (`x`) out
of the total found in each crate (`y`). Symbols: 🔒 = no `unsafe`, declares
`#![forbid(unsafe_code)]`; ❓ = no `unsafe`, but does not declare the lint;
☢️ = `unsafe` present.

### Workspace crates

| Crate | Symbol | `unsafe` expr (used/total) | Notes |
|-------|--------|----------------------------|-------|
| `audio-core` | 🔒 | 0/0 | Declares `#![forbid(unsafe_code)]`. |
| `audio-capture` | ☢️ | **96/96** | The **only** `unsafe` in the workspace: the WASAPI COM/FFI island in the private `wasapi` module (`#[allow(unsafe_code)]`). |
| `audio-pipeline` | 🔒 | 0/0 | Declares `#![forbid(unsafe_code)]`. |
| `audio-codec` | ❓ | 0/0 | No `unsafe` used, but does **not** declare `#![forbid(unsafe_code)]` (see [Observations](#observations--follow-ups)). |
| `stream-server` | 🔒 | 0/0 | Declares `#![forbid(unsafe_code)]`. |
| `sonos-control` | 🔒 | 0/0 | Declares `#![forbid(unsafe_code)]`. |
| `sonos-driver-core` | 🔒 | 0/0 | Declares `#![forbid(unsafe_code)]`. |
| `sonos-driver-cli` | 🔒 | 0/0 | Declares `#![forbid(unsafe_code)]`. |

**All first-party `unsafe` is confined to `audio-capture`'s WASAPI FFI island** —
exactly as the architecture intends. See
[ADR-0001](adr/0001-user-mode-wasapi-loopback.md) and
[ADR-0006](adr/0006-windows-rs-wasapi-bindings.md).

### Third-party dependencies (context)

System and async crates necessarily use `unsafe`; the notable contributors in our
dependency tree are unchanged upstream code:

| Dependency | `unsafe` expr (used/total) |
|------------|----------------------------|
| `windows 0.62.2` (WASAPI/COM bindings) | 4718/62026 |
| `tokio 1.52.3` (async runtime) | 2096/2906 |
| `rubato 3.0.0` (resampler) | 592/819 |
| `flacenc 0.5.1` (FLAC encoder) | 6/6 |

> Note: the dependency tree also contains a **third-party** `audio-core 0.2.1` from
> crates.io (4/4 `unsafe` expr), pulled in transitively by the resampling stack. It is
> a name collision and is **not** this workspace's internal `audio-core 0.1.0` (🔒).

## Test coverage — `cargo-llvm-cov`

`cargo llvm-cov --workspace --summary-only`:

| Metric | Coverage |
|--------|----------|
| Lines | **75.68%** (4355 total, 1059 missed) |
| Regions | 74.04% (7250 total, 1882 missed) |
| Functions | 68.88% (617 total, 192 missed) |

The headline number is **held down by code that cannot execute without audio
hardware**. The WASAPI FFI files report ~0% in a headless run — e.g.
`audio-capture\src\wasapi\endpoint.rs` (0%) and `wasapi\com.rs` (0%) — because they
talk to a real audio engine and COM. That path is verified instead by the
[manual capture checklist](AUDIO-CAPTURE-MANUAL-CHECKLIST.md).

Excluding the unrunnable FFI, the testable code is well covered, e.g.:

| File | Line coverage |
|------|---------------|
| `audio-capture\src\format.rs` | 98.66% |
| `audio-capture\src\device.rs` | 98.90% |
| `audio-capture\src\capture.rs` | 85.16% |

## Dependency advisories — `cargo-audit`

`cargo audit` (advisory DB: 1138 advisories; 150 crate dependencies scanned):

```
Scanning Cargo.lock for vulnerabilities (150 crate dependencies)
```

✅ **0 vulnerabilities** — exit code `0`, no advisories matched.

## Licences, bans & sources — `cargo-deny`

`cargo deny check`:

```
advisories ok, bans ok, licenses ok, sources ok
```

✅ All four checks pass: only permissively-licensed dependencies (per `deny.toml`), no
banned or duplicate crates, and only trusted sources.

## Observations & follow-ups

These are tracked here for transparency; none are blockers for v0.1.0.

- **`audio-codec` does not declare `#![forbid(unsafe_code)]`.** `cargo-geiger`
  confirms it uses **zero** `unsafe` today (0/0), but adding the lint would make that
  guarantee enforced rather than incidental, matching the other pure-Rust crates. This
  is a one-line crate-source change owned by the codec workstream, intentionally **not**
  made here (this workstream does not modify crate behaviour).
- **WASAPI FFI coverage is structurally ~0%** in automated runs and always will be
  without a hardware-in-the-loop CI runner; the manual checklist is the compensating
  control.

## Reproducing this snapshot

From a shell with the MSVC environment initialised (see the README's *Building*
section — the linking tools need it):

```powershell
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo build --workspace --release
cargo geiger --manifest-path crates\sonos-driver-cli\Cargo.toml --all-features
cargo llvm-cov --workspace --summary-only
cargo audit
cargo deny check
```

`cargo-geiger` is pointed at the `sonos-driver-cli` package manifest (an absolute path
is required) because the workspace root is a virtual manifest; rooting the report at the
top-level binary walks the entire crate graph.
