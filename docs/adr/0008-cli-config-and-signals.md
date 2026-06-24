# 0008 — CLI configuration precedence, graceful stop, and control commands

- Status: Accepted
- Date: 2026-06-26

## Context

Workstream WS-G builds the `sonos-driver-cli` crate: the operator-facing front
end over the stable public APIs of `sonos-driver-core` and `sonos-control`. The
skeleton already parsed `profiles`/`list`/`stream`; Phase 2 makes it a complete,
scriptable control surface. The forces:

- **A full session must be configurable two ways.** Power users want a checked-in
  TOML file; quick runs want pure command-line flags. The two must compose
  predictably, and neither may require the other.
- **Streaming blocks; stopping must stay graceful.** `Bridge::run` blocks until a
  stop is requested via its `StopHandle`. The CLI needs to translate a Ctrl-C
  (or console-close) into that cooperative stop, print a clean message, and exit
  `0` — on Windows, without the crate itself using `unsafe`.
- **The CLI is being built in parallel with `Bridge::run` (WS-F).** Every wire-up
  must compile and be unit-testable now, against the frozen public API only, with
  no dependency on `run`'s internals and no edits to `sonos-driver-core` /
  `sonos-control`.
- **Operators need a control plane, not just a data plane.** Halting playback on a
  room is a one-shot UPnP action, independent of any running stream process.
- **Hygiene.** Pure Rust, `#![forbid(unsafe_code)]`, permissive licences only,
  no TLS, latest pinned dependencies, zero advisories — enforced locally by the
  same six gates as the rest of the workspace.

## Decision

- **Strict configuration precedence: flags > file > defaults.** Every
  config-bearing `stream` flag is an `Option<T>`; resolution starts from the
  loaded file (or `Config::default()` when `--config` is absent) and overlays only
  the flags the user actually set. A `0` value for `--target-sample-rate` is the
  explicit "keep the native capture rate" sentinel (mapped to `None`), distinct
  from "flag unset".
- **A global `--config <path>` flag plus a `config` subcommand.** `config example`
  prints a fully-commented TOML whose active values equal the built-in defaults
  and which always parses back into a valid `Config` (round-trip is a test).
  `config check` loads, validates, and reports the effective configuration.
- **Validation lives in the CLI and guards the bridge's invariants:** non-zero
  port, a non-empty and parseable `bind-address`, a `target-sample-rate` within
  `8000..=384000` Hz, and — when streaming — a non-empty room or uuid. Errors are
  human-readable and name the offending key. `discovery-timeout-secs == 0` is
  deliberately *not* rejected: it is the core `SonosConfig::default`, so failing it
  would break zero-config `stream`; `config check` instead surfaces it as a note.
- **Graceful stop via the permissive `ctrlc` crate (MIT/Apache).** The stream
  command acquires `bridge.stop_handle()` *before* calling `run()`, then installs
  a handler that calls `handle.stop()`. `ctrlc` encapsulates the platform signal
  registration, so the CLI keeps `#![forbid(unsafe_code)]`. The handler body is
  factored into a standalone function so the wiring is unit-tested by invoking the
  closure directly — no real signal, hardware, or implemented `run()` required.
- **`stop --room|--uuid` is a one-shot control command** built only on the public
  `sonos-control` surface: `--room` resolves via `controller_for_room`; `--uuid`
  resolves through the zone-group topology to the group coordinator (falling back
  to a direct match), then sends `AVTransport` `Stop`. No IPC to a running stream
  process is involved.
- **`list` gains zone-group context and `--json`.** Each row reports room, model,
  IP, uuid and the device's role (coordinator / member / standalone / unknown)
  plus its group label, derived from `discover_zone_groups`; `serde_json` powers
  the `--json` output for scripting. Missing topology degrades to a plain listing.
- **Exit codes are meaningful.** Success is `0`; a `core::Error::NotImplemented`
  (a capability a sibling workstream has not yet landed) maps to `2` with an
  informational message; any other error is `1`.

## Consequences

- ✅ A session is fully reproducible from a TOML file, fully expressible from
  flags, or any mix — with one obvious rule. Merge/override behaviour is pinned by
  tests that write a temp TOML and assert the effective `Config`.
- ✅ Ctrl-C wiring is correct and covered now, before `Bridge::run` exists; once
  WS-F lands, end-to-end stop works with no CLI change. The CLI stays `unsafe`-free.
- ✅ The control plane (`stop`) and richer `list` use only the frozen public API,
  so WS-F/WS-B keep ownership of their crates; nothing here forces a contract
  change. (Follow-up noted for WS-F: the `discovery-timeout-secs` default of `0`.)
- ✅ `ctrlc` and `serde_json` are permissively licensed and pass `cargo deny` and
  `cargo audit` with zero advisories; the whole crate is hardware-free in CI.
- ⚠️ `ctrlc::set_handler` may be installed only once per process. That suits a CLI
  (one `stream` per invocation) but means the handler is wired in the command path,
  not globally; tests exercise the closure rather than registering a real handler.
- ⚠️ Resolving `--uuid` to a coordinator relies on zone-group topology being
  reachable; when it is not, the CLI falls back to a direct device match, which can
  target a grouped member rather than its coordinator. Rooms (`--room`) avoid this.
