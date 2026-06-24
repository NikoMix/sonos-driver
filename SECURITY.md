# Security Policy

## Reporting a vulnerability

Please report security vulnerabilities **privately**. Do **not** open a public issue
for a security problem.

- Use GitHub's **[private vulnerability reporting](https://github.com/NikoMix/sonos-driver/security/advisories/new)**
  ("Report a vulnerability" on the Security tab), or
- contact the maintainer directly via their GitHub profile.

Please include: affected version/commit, a description, reproduction steps, and the
impact you observed. We aim to acknowledge reports within a few days.

## Scope

`sonos-driver` runs on the local machine and exposes a **local HTTP server on the LAN**
so Sonos players can pull the audio stream. Relevant concerns include:

- The bind address/port and who on the network can reach the stream endpoint.
- Handling of SSDP/UPnP responses from the network (untrusted input parsing).
- Any `unsafe` FFI used for WASAPI capture.

## Supported versions

The project is pre-1.0; only the latest `main` is supported. Security fixes land on
`main` and in the next release.

## Hardening already in place

- `cargo-audit` (RustSec) and `cargo-deny` run in CI and fail on known vulnerabilities
  or disallowed licenses.
- CodeQL (security-and-quality) scans run on PRs and weekly.
- `unsafe_code` is linted (`warn`) workspace-wide and confined to the capture layer.
