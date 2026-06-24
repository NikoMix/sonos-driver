# 0011 — Per-application capture via WASAPI process loopback

- Status: Accepted
- Date: 2026-06-25

## Context

WS-K (issue #29) extends `audio-capture` so the bridge can stream the audio of a
**single application** (e.g. just the browser, or a game) — or, inversely,
*everything except* one application — instead of only the whole default render
endpoint covered by [ADR-0006](./0006-windows-rs-wasapi-bindings.md).

Constraints carried over from the project charter and the issue:

- **Pure user-mode.** No kernel driver, no APO, no service. The feature must be a
  library capability reachable from the existing CLI.
- **`unsafe` stays confined.** All COM/FFI remains inside the audited
  `audio_capture::wasapi` island; every other crate keeps
  `#![forbid(unsafe_code)]`.
- **Full back-compat.** The default-device loopback stays the default; the new
  path is purely additive in `audio-core::CaptureConfig` and the CLI.
- **Permissive deps only**, latest releases, `cargo deny` / `cargo audit` clean.

Windows exposes exactly the right primitive in user mode: the
**process-loopback** virtual device, activated asynchronously via
`ActivateAudioInterfaceAsync(VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK, …)` with an
`AUDIOCLIENT_ACTIVATION_PARAMS` blob naming the target PID and an
include/exclude *process-tree* mode. It is the same mechanism as Microsoft's
classic `ApplicationLoopback` sample. Notable forces and gotchas:

- The activation is **asynchronous**: a COM completion handler
  (`IActivateAudioInterfaceCompletionHandler`) fires on an MTA thread, and the
  activating thread must wait for it before reading the activated `IAudioClient`.
- The activation-params blob must stay alive until the completion event is
  signalled.
- `GetMixFormat()` is **not** callable on the loopback client — the format must
  be specified explicitly. We request 16-bit PCM and let the engine convert with
  `AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM`.
- The API needs **build 20348+** (Windows 11 / Windows Server 2022). Older builds
  return `E_NOTIMPL` from `GetActivateResult`. That must surface as a clear typed
  error, never a panic.
- Shared-mode only; the stream is silent while the target has no active render
  stream (acceptable — it tracks the app, including its own silence).

## Decision

Add a **second `AudioSource` activation path** in `audio-capture`, selected from
config, that brings up an `IAudioClient` on the process-loopback virtual device.
The default-device path is untouched and remains the default.

Concretely:

- **FFI (`wasapi::process_loopback`).** A `#[implement]`-based COM completion
  handler signals an event; the activating thread waits (bounded timeout), then
  `GetActivateResult` yields the `IAudioClient`. The client is initialised with
  `AUDCLNT_STREAMFLAGS_LOOPBACK | _EVENTCALLBACK | _AUTOCONVERTPCM` against an
  explicit `WAVEFORMATEX` (16-bit PCM, 48 kHz, stereo), then drained by the
  **shared `wasapi::pump` loop** that already serves the default-device path —
  so buffer handling, silence flags, and decode-to-`f32` are identical.
- **Process resolution is pure & tested.** A name (`"spotify.exe"`, `.exe`
  optional) or numeric PID is resolved against a Toolhelp snapshot in pure,
  unit-tested logic (`audio_capture::process`): case-insensitive basename match,
  tree-root preference on duplicates, lowest-PID tie-break; explicit PIDs must
  exist. Only the snapshot enumeration itself is `unsafe`, inside `wasapi`.
- **Runtime OS gate.** Before activating, `current_os_build()` (RtlGetVersion) is
  checked against `MIN_PROCESS_LOOPBACK_BUILD = 20348`; older builds return
  `audio_core::Error::Capture` with a clear message.
- **Additive config & CLI.** `CaptureConfig` gains `process: Option<String>` and
  `exclude_process: bool` (serde/TOML, defaults preserve today's behaviour).
  `sonos-driver-cli` gains `--process <name|pid>` and `--exclude-process`
  (precedence flags > file > defaults). A non-blank `process` selects the
  per-application path and **takes precedence over `device`**; `validate` rejects
  `exclude-process` without a `process` and a blank `process`.
- **Dependency.** No new third-party crate: the existing first-party
  `windows`(-rs) features already cover the API. `windows-core` is added as a
  *direct* dependency pinned to the same release as `windows` purely so the
  `#[implement]` macro's generated `::windows_core` paths resolve under the
  umbrella crate (it is already in the lock file transitively).

The safe/unsafe boundary from ADR-0006 holds unchanged: the FFI module hands the
rest of the crate only plain, owned, `Send` data; no COM pointer leaves the
module or crosses a thread.

## Consequences

- ✅ Per-application capture (and exclude) is available in pure user-mode with no
  driver, reusing the existing pump/decode/telemetry machinery.
- ✅ `unsafe` stays confined to `audio_capture::wasapi`; the rest of the
  workspace remains `forbid(unsafe_code)`. The pedantic `clippy` profile is kept
  by scoping the macro-generated lint allowances to a private handler module.
- ✅ Hardware-free tests cover the testable surface: activation-params blob
  construction, the OS-version gate decision, process-name→PID resolution, and
  Config/CLI parsing + precedence. Live capture is verified by the manual
  checklist (`docs/AUDIO-CAPTURE-MANUAL-CHECKLIST.md`).
- ⚠️ Requires Windows 11 / Server 2022 (build 20348+). On older builds the
  feature fails fast with a typed error; the default-device path still works.
- ⚠️ Shared-mode loopback is silent when the target app is not rendering, and the
  engine resamples to our explicit format (`AUTOCONVERTPCM`) rather than exposing
  the app's native mix format — an accepted trade-off for a stable, fixed
  capture format.
- ⚠️ Slightly more FFI to maintain (async activation + completion handler) and a
  one-line direct `windows-core` pin tied to windows-rs releases; both are
  `cargo update`-managed alongside the existing bindings.
