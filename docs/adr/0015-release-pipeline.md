# 0015 — Release pipeline: dual-binary portable zip + Inno Setup installer, gated GitHub Actions release

- Status: Accepted
- Date: 2026-06-25

## Context

`sonos-driver` v0.1.0 shipped a single CLI binary packaged by
`scripts/package.ps1` into a portable zip (see
[ADR-0010](./0010-portable-release-packaging.md)). Since then the workspace gained
a second user-facing binary — the system-tray desktop GUI `sonos-driver-gui`
(see [ADR-0012](./0012-gui-stack-and-threading.md)) — so a release must now ship
**both** executables.

The owner asked for a *mature* release pipeline that (a) builds the whole
solution on every change and (b) publishes a versioned GitHub Release containing
a bundled zip **and** an installer. Several forces constrain how we do this:

- **No paid vendor licence.** Any installer tooling must be free to use,
  including commercially, with no purchased licence.
- **Permissive supply chain.** New tooling must not pull in copyleft or
  source-restricted licences, and CI actions are a supply-chain surface that
  should be pinned, not floating on mutable tags.
- **CI is billing-blocked today.** GitHub Actions cannot start runners on this
  private repo, so the pipeline must be *correct by construction* (it is the
  authoritative path once billing is restored) while local builds remain the
  authoritative release mechanism in the meantime (see `docs/RELEASING.md`).
- **One version source of truth.** `[workspace.package].version` already names
  the zip; the installer and release tag must derive from the same value with no
  second place to edit.

## Decision

1. **Two artifacts per release: a portable zip and a Windows installer.**
   - `scripts/package.ps1` builds **both** binaries
     (`cargo build --release -p sonos-driver-cli -p sonos-driver-gui`) and
     bundles `sonos-driver.exe`, `sonos-driver-gui.exe`, the README, CHANGELOG,
     both licences, and a generated example config into the existing portable
     zip. The zip remains the zero-install, "unzip and run" artifact.
   - `installer/sonos-driver.iss` + `scripts/build-installer.ps1` produce a
     single-file setup `.exe` using **Inno Setup**. The installer payload is
     produced by *expanding the zip*, so the two artifacts are guaranteed
     identical by construction. The installer adds Start-menu shortcuts, an
     optional desktop shortcut, and an uninstaller.

2. **Inno Setup as the installer tool.** Inno Setup is mature, widely used, and
   **free for commercial use** with no paid licence (its own permissive licence),
   satisfying the no-paid-vendor-licence constraint. It is not a Cargo
   dependency — it never enters the Rust dependency graph that `cargo deny`
   governs — and its compiler (`ISCC.exe`) is installed in CI on demand via
   `choco install innosetup`. WiX/MSI was rejected as heavier and more brittle
   for a two-binary user-mode app; MSIX was rejected because it pushes toward
   Store signing/identity. `build-installer.ps1` degrades gracefully (warns and
   skips) when ISCC is absent, so a contributor without Inno Setup can still cut
   the portable zip.

3. **A gated, tag-driven release workflow.** `.github/workflows/release.yml`
   runs a full `verify` job (fmt, clippy `-D warnings`, the workspace test
   suite, a release build, `cargo deny`, and `cargo audit`) and only then builds
   the artifacts, computes **SHA-256 checksums**, and publishes a GitHub Release
   with the zip, the installer, and a `SHA256SUMS.txt`. Continuous build/test on
   every push and PR stays in `.github/workflows/ci.yml`.

4. **Supply-chain hardening of the workflows.** Every third-party GitHub Action
   is pinned to a full commit SHA (with the human-readable version in a trailing
   comment), `actions/checkout` runs with `persist-credentials: false`, and each
   job declares least-privilege `permissions`.

5. **GitHub Releases, not GitHub Packages.** GitHub Packages targets package
   registries (npm, NuGet, Maven, containers) and has no first-class channel for
   standalone Windows executables. A versioned **GitHub Release** with attached
   artifacts is the idiomatic distribution channel for this desktop/CLI app, so
   that is what the pipeline publishes.

## Consequences

- A release now delivers both the GUI and the CLI through either a portable zip
  or a guided installer, covering both "power user" and "double-click" audiences
  from one version-stamped source.
- The installer payload can never drift from the zip because it is literally
  built from the zip; there is still exactly one place to bump the version.
- Inno Setup adds a build-time tool (installed in CI, optional locally) but **no
  runtime and no Cargo dependency**, so the `cargo deny` / `cargo audit` posture
  is unchanged.
- SHA-pinned actions mean a new action release is an explicit, reviewable bump
  rather than a silent change — at the cost of periodic maintenance to refresh
  the pins (Dependabot's `github-actions` ecosystem can automate this later).
- The pipeline is dormant until Actions billing is restored; until then,
  `docs/RELEASING.md` §3 (local `package.ps1` + `build-installer.ps1` + `gh
  release create`) is the authoritative release path. The installer's Inno Setup
  script is validated by the release workflow on first run; locally it is built
  only when ISCC is installed.
- The released executables remain **unsigned** (Authenticate/SmartScreen caveats
  unchanged from `docs/RELEASING.md` §5); code signing stays optional and out of
  scope per the no-paid-vendor-licence constraint.
