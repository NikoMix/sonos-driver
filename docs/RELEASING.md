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

`scripts/package.ps1` builds the release binaries (the CLI, the Sonos Router app and the
driverless service) and assembles a portable, self-contained zip (those binaries + README
+ CHANGELOG + both licences + a generated example config). It needs no extra crate or
`xtask` (see ADR-0010) and only built-in PowerShell cmdlets.

From a Developer PowerShell for VS (or after running `vcvars64.bat`):

```powershell
pwsh -File scripts/package.ps1            # -> dist\sonos-driver-<version>-x86_64-pc-windows-msvc.zip
```

The script prints the archive path and size. To package already-built binaries without
rebuilding, use `-SkipBuild`; to change the output directory, use `-OutputDir <path>`.

### Include the kernel driver (optional)

The driver is built and signed by `driver/scripts`, not by the packaging script — that
needs the WDK and a certificate a release machine may not have. Build and sign it first,
then pass `-IncludeDriver`:

```powershell
.\driver\scripts\Build-Driver.ps1
.\driver\scripts\Sign-Driver.ps1
pwsh -File scripts/package.ps1 -IncludeDriver
```

The payload then also contains `sonosvad-svc.exe` and a `driver\` folder with the signed
`.sys`/`.inf`/`.cat`, all install scripts, and a generated `INSTALL.md` giving the exact
commands for the payload's flat layout (the scripts' defaults assume the repo tree).
Packaging **fails** if the artifacts are missing or unsigned, so a release can never claim
to ship a driver it does not.

### Build the installer (optional)

`scripts/build-installer.ps1` compiles a single-file Inno Setup installer
(`sonos-driver-<version>-x86_64-setup.exe`) from the **same payload** as the zip, so the
two artifacts are identical by construction (see ADR-0015):

```powershell
choco install innosetup                  # one-time: installs the ISCC compiler
pwsh -File scripts/build-installer.ps1   # -> dist\sonos-driver-<version>-x86_64-setup.exe

# ...with the driver offered as an optional "Full" component
pwsh -File scripts/build-installer.ps1 -IncludeDriver
```

If the Inno Setup compiler (`ISCC.exe`) is not found, the script warns and skips the
installer (the zip is unaffected) unless you pass `-RequireInstaller`. Attach the installer
to the Release alongside the zip in the Publish step below.

`-IncludeDriver` is forwarded to `package.ps1`, and the `.iss` detects the driver in the
staged payload at compile time — so the same script produces a core-only or a full
installer with no separate variant to maintain. The driver component copies files only;
installing the driver stays a manual, documented step (`driver\INSTALL.md`), because it
requires Secure Boot off and a reboot.

### Publish

1. Tag and push:
   ```powershell
   git tag -a vX.Y.Z -m "sonos-driver vX.Y.Z"
   git push origin vX.Y.Z
   ```
2. Create the GitHub Release and attach the zip, the installer, and a checksum file
   (uses the pre-installed `gh` CLI):
   ```powershell
   $sha = Get-FileHash dist\*.zip, dist\*setup.exe -Algorithm SHA256 |
     ForEach-Object { "{0}  {1}" -f $_.Hash.ToLower(), (Split-Path $_.Path -Leaf) }
   $sha | Set-Content dist\SHA256SUMS.txt -Encoding ascii
   gh release create vX.Y.Z `
     dist\sonos-driver-X.Y.Z-x86_64-pc-windows-msvc.zip `
     dist\sonos-driver-X.Y.Z-x86_64-setup.exe `
     dist\SHA256SUMS.txt `
     --title "sonos-driver vX.Y.Z" --generate-notes
   ```

## 4. Tag-driven release via GitHub Actions

`.github/workflows/release.yml` automates §3 once CI billing is restored. On a pushed tag
matching `v*` it first runs a **verify gate** — `cargo fmt --check`, `clippy -D warnings`,
the workspace test suite, a release build, `cargo-deny`, and `cargo-audit` — and only if
all are green does the package job run. That job builds both binaries, assembles the zip,
installs the Inno Setup compiler (`choco install innosetup`), compiles the installer,
computes a `SHA256SUMS.txt`, uploads all three as build artifacts, and (on tag pushes)
publishes a GitHub Release with them attached via the `gh` CLI. It can also be triggered
manually (`workflow_dispatch`) to produce the artifacts without publishing a release.

Every third-party GitHub Action is pinned to a full commit SHA and `actions/checkout` runs
with `persist-credentials: false`; no third-party publishing action is used — only `gh`
with the built-in `GITHUB_TOKEN` — to keep the release supply chain minimal. See
[ADR-0015](adr/0015-release-pipeline.md).

> **Why GitHub Releases, not GitHub Packages?** GitHub Packages hosts package-registry
> formats (npm, NuGet, Maven, containers); it has no channel for standalone Windows
> executables. A versioned GitHub **Release** with attached artifacts is the idiomatic
> distribution channel for this app.

## 5. Code signing (Authenticode) — honest status

The released binaries (`sonos-driver.exe` and `sonos-router.exe`) are **normal
user-mode executables** and are currently shipped **unsigned** — as is the installer.
Consequences and options:

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
  WHQL) discussed in [ADR-0014](adr/0014-virtual-audio-driver-reality.md). When a release
  is built with `-IncludeDriver`, the bundled `sonosvad.sys` is **test-signed**: it loads
  only on a machine with test signing enabled (Secure Boot off), which is why installing it
  is a deliberate, documented step rather than something the installer does. Production
  driver signing is tracked separately and is not part of this pipeline.

## 6. Verify the package

Before announcing, sanity-check the archive on a clean path:

```powershell
Expand-Archive dist\sonos-driver-X.Y.Z-x86_64-pc-windows-msvc.zip -DestinationPath $env:TEMP\sd-verify
$dir = "$env:TEMP\sd-verify\sonos-driver-X.Y.Z-x86_64-pc-windows-msvc"
& "$dir\sonos-driver.exe" --version        # CLI prints the version
& "$dir\sonos-cast-svc.exe" --help         # service binary responds
Start-Process "$dir\sonos-router.exe"      # app opens and shows its tray icon — then close it
```

Confirm the CLI version matches the tag and that all three binaries, the README, CHANGELOG,
both `LICENSE-*` files, and `sonos-driver.toml` are present in the extracted folder. If you
built the installer, run it on a throwaway path/VM and confirm it installs them and creates
the Start-menu shortcut.

For a `-IncludeDriver` release, also verify the driver package from the extracted payload —
this works without the C++ source tree, so it validates exactly what a user receives:

```powershell
& "$dir\driver\scripts\Test-Driver.ps1" -ArtifactDir "$dir\driver"
```

Expect every artifact check to PASS and exactly one SKIP (the INF-versus-headers check,
which needs the source tree). A FAIL here means the payload is not installable — do not
publish it.
