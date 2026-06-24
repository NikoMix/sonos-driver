# Releasing `sonos-driver`

This guide describes how to cut a `sonos-driver` release. The project versions with
[Semantic Versioning](https://semver.org/); the single source of truth for the version is
the `version` field in `[workspace.package]` of the workspace `Cargo.toml`, which every
crate inherits.

> **Current reality — CI is billing-blocked.** GitHub Actions on this private repository
> cannot start runners (jobs fail in ~1–3 s with no runner assigned — the signature of an
> Actions spending limit). Until the repository owner makes the repo public, raises the
> Actions spending limit, or adds a self-hosted runner, **releases are cut locally** with
> the steps in [§3](#3-cut-the-release-locally-authoritative-today). The tag-driven
> workflow in [§4](#4-tag-driven-release-via-github-actions) is ready and correct for when
> billing is restored.

## 1. Versioning

- Bump `version` in `[workspace.package]` (workspace `Cargo.toml`) following SemVer.
- Pre-1.0, breaking changes bump the **minor**; fixes and additive changes bump the
  **patch**.
- The CLI reports this version via `sonos-driver --version`, and `scripts/package.ps1`
  reads it to name the release archive — so there is no second place to update.

## 2. Pre-release checklist

Run every gate locally from an MSVC-initialised shell (see the README's *Building*
section). All must be green — they are the release gate because CI cannot run:

```powershell
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings   # pedantic, -D warnings
cargo test --workspace --all-features
cargo build --workspace --release
cargo deny check                                                       # licences, advisories, bans, sources
cargo audit                                                            # zero vulnerabilities is mandatory
```

Then:

1. Update `CHANGELOG.md` — move items from *Unreleased* into a new `## [x.y.z] - YYYY-MM-DD`
   section.
2. Bump `[workspace.package].version` (§1).
3. Commit: `git commit -m "release: vX.Y.Z"`.
4. Verify the working tree is clean and on `main`.

## 3. Cut the release locally (authoritative today)

`scripts/package.ps1` builds the release binary and assembles a portable, self-contained
zip (binary + README + CHANGELOG + both licences + a generated example config). It needs no
extra crate or `xtask` (see ADR-0010) and only built-in PowerShell cmdlets.

From a Developer PowerShell for VS (or after running `vcvars64.bat`):

```powershell
pwsh -File scripts/package.ps1            # -> dist\sonos-driver-<version>-x86_64-pc-windows-msvc.zip
```

The script prints the archive path and size. To package an already-built binary without
rebuilding, use `-SkipBuild`; to change the output directory, use `-OutputDir <path>`.

### Publish

1. Tag and push:
   ```powershell
   git tag -a vX.Y.Z -m "sonos-driver vX.Y.Z"
   git push origin vX.Y.Z
   ```
2. Create the GitHub Release and attach the zip (uses the pre-installed `gh` CLI):
   ```powershell
   gh release create vX.Y.Z dist\sonos-driver-X.Y.Z-x86_64-pc-windows-msvc.zip `
     --title "sonos-driver vX.Y.Z" --generate-notes
   ```

## 4. Tag-driven release via GitHub Actions

`.github/workflows/release.yml` automates §3 once CI billing is restored. On a pushed tag
matching `v*` it: checks out, installs the stable Rust toolchain, runs
`scripts/package.ps1`, uploads the zip as a build artifact, and (on tag pushes) publishes a
GitHub Release with the archive attached via the `gh` CLI. It can also be triggered
manually (`workflow_dispatch`) to produce the artifact without publishing a release. No
third-party publishing action is used — only `gh` with the built-in `GITHUB_TOKEN` — to
keep the release supply chain minimal.

## 5. Code signing (Authenticode) — honest status

The released `sonos-driver.exe` is a **normal user-mode executable** and is currently
shipped **unsigned**. Consequences and options:

- On first run, Microsoft **SmartScreen** / Defender may show an "unknown publisher"
  prompt. This is expected for an unsigned, low-reputation binary and does not indicate
  malware.
- To remove the prompt you may sign the binary with a standard **Authenticode**
  certificate (OV or EV) from a commercial CA, using `signtool sign /fd SHA256 /tr <RFC3161
  timestamp URL> /td SHA256 sonos-driver.exe`. Such a certificate is **paid** and
  identity-vetted; the project does **not** bundle one, in line with its
  "no paid vendor licence" constraint. Signing is therefore **optional** and left to the
  distributor.
- **This is *application* (Authenticode) signing only.** It is unrelated to — and must not
  be confused with — the **kernel-driver** signing (EV cert + Partner Center attestation /
  WHQL) discussed in [ADR-0014](adr/0014-virtual-audio-driver-reality.md). This project
  ships no kernel driver, so none of that pipeline applies.

## 6. Verify the package

Before announcing, sanity-check the archive on a clean path:

```powershell
Expand-Archive dist\sonos-driver-X.Y.Z-x86_64-pc-windows-msvc.zip -DestinationPath $env:TEMP\sd-verify
& "$env:TEMP\sd-verify\sonos-driver-X.Y.Z-x86_64-pc-windows-msvc\sonos-driver.exe" --version
```

Confirm the version matches the tag and that the README, CHANGELOG, both `LICENSE-*` files,
and `sonos-driver.toml` are present in the extracted folder.
