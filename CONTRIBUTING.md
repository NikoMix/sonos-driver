# Contributing to sonos-driver

Thanks for your interest! This project values **high code quality with low
supervision** — the tooling is set up to keep the tree clean automatically.

## Ground rules

1. **Permissive licenses only.** New dependencies must be MIT/Apache-2.0/BSD/ISC/Zlib
   or similar. No copyleft (beyond MPL-2.0) and no paid/commercial-license crates. If a
   capability is only available under such terms, we build it in-house. `cargo-deny`
   enforces this.
2. **Be honest about latency.** Don't add claims or features implying sub-100 ms
   network streaming to Sonos — it isn't possible (see
   [docs/LATENCY-BUDGET.md](docs/LATENCY-BUDGET.md)).
3. **Build against the contracts.** New work should target the `audio-core` traits so
   crates stay decoupled and hardware-free testable.

## Local checks (must pass before opening a PR)

```powershell
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo deny check          # licenses + advisories
cargo audit               # known vulnerabilities
```

> On Windows, run these from a **Developer PowerShell for VS** (or after `vcvars64.bat`)
> so the MSVC linker libraries are on the path — see the README's *Building* section.

Install the supply-chain tools once with:

```powershell
cargo install cargo-deny cargo-audit --locked
```

## Style & conventions

- **Formatting:** `rustfmt` with the repo's [`rustfmt.toml`](rustfmt.toml) (LF endings).
- **Lints:** `clippy` with `pedantic` enabled; warnings are errors in CI. Prefer fixing
  the code over `#[allow(...)]`. When an allow is genuinely warranted, scope it to the
  smallest item and add a one-line justification comment.
- **Docs:** every public item carries rustdoc (`missing_docs` is on). Add domain
  acronyms to [`clippy.toml`](clippy.toml) `doc-valid-idents` rather than back-ticking
  prose everywhere.
- **Commits:** Conventional Commits (`feat:`, `fix:`, `docs:`, `deps:`, `ci:`, …).
- **Unsafe:** keep `unsafe` confined to the capture FFI layer and document the safety
  invariants.

## Workflow

1. Pick or open an issue (use the **Workstream task** template for WS-A…WS-G work).
2. Branch from `main`, implement with tests, keep the local checks green.
3. Open a PR using the template; link the issue (`Closes #N`).
4. CI (lint, test, x64 build, arm64 check, cargo-deny, cargo-audit, CodeQL) must pass.

## Dependency upgrades

Dependabot proposes weekly updates. Before a release we additionally run `cargo update`
and re-run `cargo audit` / `cargo deny` to ensure everything is current and
vulnerability-free.

## License of contributions

By contributing you agree your work is dual-licensed under
[MIT](LICENSE-MIT) OR [Apache-2.0](LICENSE-APACHE), matching the project.
