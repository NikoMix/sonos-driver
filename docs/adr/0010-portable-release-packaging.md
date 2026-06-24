# 0010 — Portable release packaging via a PowerShell script (no extra crate or xtask)

- Status: Accepted
- Date: 2026-06-25

## Context

`sonos-driver` ships a single Windows binary (`sonos-driver.exe`) plus a handful of
companion files (README, licences, an example config). We want a one-command way to
produce a portable, self-contained release archive that a user can unzip and run
**without** installing a Rust toolchain.

The options considered:

1. **A dedicated `xtask` crate** (the common Cargo pattern for build automation). This
   adds a whole crate to the workspace, pulls in its own dependencies, lengthens build
   and CI times, and has to be kept under the same `cargo-deny`/`cargo-audit` scrutiny
   as shipping code — disproportionate for "zip up five files".
2. **A GitHub Actions release workflow.** CI is currently billing-blocked and out of
   scope for this workstream, and a workflow cannot be run locally for a quick release.
3. **A small PowerShell script.** The project is Windows-only and already documents
   PowerShell as the build shell, so a script is the lowest-friction option that runs
   identically on a developer's machine and (later) in CI.

## Decision

Provide `scripts/package.ps1`: a self-contained, comment-based-help PowerShell script
that builds the release binary (skippable with `-SkipBuild`), stages the binary +
`README.md` + `CHANGELOG.md` + `LICENSE-MIT` + `LICENSE-APACHE` + an example
`sonos-driver.toml`, and zips them to
`dist\sonos-driver-<version>-x86_64-pc-windows-msvc.zip`.

Specific choices:

- **No new crate and no `xtask`.** Packaging is not application logic; keeping it out of
  the Cargo workspace avoids dependency, audit, and build-time cost.
- **Read the version from `[workspace.package]`** in the workspace `Cargo.toml` so the
  archive name always matches the real version with no duplicated source of truth.
- **Generate the example config from the binary itself** (`sonos-driver config example`)
  rather than committing a separate sample, so the bundled config can never drift from
  the code. A minimal static fallback is used only if that invocation fails.
- **Use only built-in cmdlets** (`Compress-Archive`); no third-party PowerShell modules,
  consistent with the permissive-only, minimal-dependency posture.
- **Write to `dist/`**, which is git-ignored.

## Consequences

- A release is one command (`pwsh -File scripts/package.ps1`) and is reproducible
  locally; the same script can be called from a release workflow later with no rework.
- The script shares the build's MSVC-environment requirement (it shells out to `cargo`);
  this is documented in its help and in the README, and `-SkipBuild` sidesteps it when a
  binary already exists.
- Packaging logic lives in PowerShell, not Rust, so it is not covered by the Rust test
  suite. The surface is tiny and is exercised manually as part of cutting a release.
- If the project ever targets non-Windows hosts, this script would need a cross-platform
  sibling (or a move to `cargo-dist`/`xtask`); that is explicitly out of scope while the
  bridge is Windows-only.
