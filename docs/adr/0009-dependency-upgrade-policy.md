# 0009 — Dependency upgrade policy and the rubato/audioadapter version pin

- Status: Accepted
- Date: 2026-06-25

## Context

Phase 3 hardening requires every dependency to sit at its latest *compatible*
version with zero known advisories (`cargo audit`) and only permissive licences
(`cargo deny`). When the fleet work landed, two direct dependencies were a major
version behind the newest crates.io release:

- `toml` 0.8 → 1.x, and
- the `audioadapter` / `audioadapter-buffers` family 3.x → 4.0.

A blanket "always take the newest major" rule is wrong here: one of those bumps
is safe and one would break the build. This ADR records the policy and the
specific, non-obvious constraint so future maintainers do not "fix" an
intentional pin.

## Decision

1. **Floor on major, lock on patch.** `[workspace.dependencies]` declares
   major-version floors only; `Cargo.lock` carries the exact patches. `cargo
   update` keeps locks current and `cargo audit` (zero vulnerabilities) plus
   `cargo deny` (licences + advisories) gate every change. These two are release
   gates, not advisory checks.

2. **`toml` 0.8 → 1.x (1.1.2+spec-1.1.0): adopted.** Verified API-compatible —
   the project only uses the serde entry points `toml::from_str`,
   `toml::to_string`, and `toml::to_string_pretty`, which remain available under
   toml 1.x default features. The internal backend change (the old
   `toml_edit`/`toml_write` stack is replaced by `toml_parser`/`toml_writer`,
   and `serde_spanned`/`toml_datetime`/`winnow` move to 1.x) is transparent to
   our config round-trip, which is covered by tests.

3. **`audioadapter-buffers` / `audioadapter` held at 3.x — deliberately.** The
   real-time resampler `rubato` (latest release 3.0.0) pins `audioadapter ^3.0`
   and `audioadapter-buffers ^3.0`. `audio-pipeline` constructs adapter buffers
   and hands them **directly** to rubato, so the adapter trait family must match
   the version rubato compiles against. Forcing the workspace to 4.0 would split
   the graph (two incompatible `audioadapter` majors) and break the resampler
   boundary at the type level. For this codebase, **3.x is the latest
   *compatible* version.** Re-evaluate when `rubato` publishes a release that
   targets the 4.x adapter family.

4. **Transitive majors are upstream's call.** `matchit` 0.8 (via `axum`) and
   `generic-array` 0.14 (via `flacenc`) trail a newer major, but both parents are
   already at their latest release and neither transitive crate carries an
   advisory. We do not override transitive versions; they advance when their
   parents do.

5. **CI action pins tracked too.** `actions/checkout` v4 → v7 and
   `github/codeql-action` v3 → v4, matching the Dependabot proposals.

## Consequences

- The dependency tree is at its latest *secure, compatible* state with a single,
  clearly documented exception (the rubato-locked adapter family) so the pin
  reads as intentional rather than stale.
- `toml` 1.x brings TOML-spec-1.1 parsing; our config schema round-trips
  unchanged, so existing config files keep working.
- The audioadapter 4.x bump is unblocked the moment `rubato` ships 4.x adapter
  support — at which point items 3 is revisited in a follow-up ADR.
- Because locks are gated by `cargo audit`/`cargo deny` on every update, routine
  `cargo update` runs stay safe with minimal supervision.
