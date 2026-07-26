# 0020 — A runtime routing app that maps outputs to rooms

- Status: **Accepted**
- Date: 2026-07-26
- Relates to: [ADR-0019](./0019-driverless-routing-mode.md),
  [ADR-0018](./0018-loopback-rename-and-service-hosting.md),
  [ADR-0012](./0012-gui-stack-and-threading.md),
  [ADR-0007](./0007-orchestration-testability-seams.md)

## Context

[ADR-0019](./0019-driverless-routing-mode.md) added a driverless mode that
streams a captured Windows output to one or more Sonos rooms without installing
anything. It is complete and it works, but its interface is a TOML file:

```toml
[[route]]
source = "default"
rooms = ["Küche", "Wohnzimmer"]
```

To change where the music goes you edit a file and restart a service. That is a
reasonable interface for a service and a poor one for a person. It also hides
the two facts a person actually needs in order to fill the file in: **which
outputs this PC has** and **which rooms the household has**. Both are
discoverable at runtime, and both were being left to the user to type correctly
from memory.

The driver mode does not have this problem, because the Windows sound picker
*is* the interface — but the driver mode needs test signing and a reboot, so it
is not available to everyone, and it cannot route per-application.

So there was a gap: the mode that everyone can run had the interface that nobody
wants to use.

### What the app is actually for

The tempting design is a rule builder — "add a rule", pick a source, pick a
destination, save. We deliberately did not build that, because it models the
wrong thing.

Windows already decides which output an application plays to; that decision is
made in the volume mixer and in each app's own settings, and it has been made
that way for fifteen years. Duplicating it inside this app would create a second
source of truth that silently disagrees with the first.

The only thing Windows does *not* know is where a given output should end up on
the network. That is a single fact per output, so the natural interface is not a
list of rules but **a table with one row per Windows output** and one question
per row: where does this go? A row that is set to *Off* behaves exactly as it
does today.

This also makes the empty state meaningful. On first run the table lists every
output the machine has, already populated, all set to *Off* — the user is
choosing from what exists rather than inventing identifiers.

## Decision

**Ship a small desktop app, `sonos-router`, as the primary interface to the
driverless mode. Keep both existing modes exactly as they are.**

The app is the existing `sonos-driver-gui` crate rebuilt around a routing table
instead of a single-stream control panel. Specifics worth recording:

1. **One row per output, keyed by device id.** The assignment map is keyed by
   the endpoint's stable id, not its name, so unplugging and replugging a USB
   DAC — or Windows renaming it — never silently moves a mapping onto a
   different device. Names are for humans and are shown; ids are what we store
   and match on.

2. **A synthetic "Default output" row.** The first row is not a device; it is a
   follower for whatever Windows' default happens to be, resolved late, at the
   moment streaming starts, and re-resolved on every retry. Routing "whatever I
   am currently listening on" is the common case, and pinning it to a concrete
   device at configuration time would quietly break the next time the default
   changed.

3. **Destinations are `Off`, a room, or `All rooms`.** `All rooms` reuses the
   existing group-forming path from ADR-0018 — it forms a real Sonos group
   rather than streaming to each room in parallel, because parallel streams
   drift audibly.

4. **A missing room does not fail the table.** If a speaker is powered off, its
   row is reported as offline and *skipped*; the remaining routes still start.
   The alternative — refusing to stream anything because one speaker is
   unplugged — is the wrong trade for a household appliance. The skipped rows
   are surfaced in the UI rather than swallowed.

5. **Apply while streaming means stop, then start.** Two workers would compete
   for stream-server ports and for control of the same rooms. Applying is
   therefore a restart, and the state machine models `Stopping` explicitly so
   that events arriving from the dying worker cannot be mistaken for the new
   one's.

6. **The app streams in-process; it does not talk to the service.** There is no
   IPC, no "is the service running?" state, and no way for the two to disagree
   about who owns a room. If the user wants an always-on headless setup they run
   the service against the same file — which works because the app writes
   `%APPDATA%\sonos-driver\routes.toml` in a format `sonos-driver cast --config`
   already accepts. The app is a *front end for the same engine*, not a client
   of the service.

7. **Per-user config, no elevation.** Nothing the app does requires
   administrator rights, so it does not ask for them.

8. **The engine is `sonos_cast_service::worker::run`, unchanged in substance.**
   The app adds an `Observer` to it so the worker can report what it is doing
   (`Discovering` / `Streaming(routes)` / `Failed` / `Stopped`) instead of only
   logging. The service passes a silent observer and behaves exactly as before.
   Reusing the worker means the app inherits its retry loop, its idle release
   and its shutdown semantics for free, and any fix to one fixes all three
   modes.

The routing table itself (`routes.rs`) and the state machine (`action.rs`) are
pure data structures with no egui, no threads and no I/O, in keeping with
[ADR-0007](./0007-orchestration-testability-seams.md). They carry the validation
rules, the conflict detection and the conversion to the worker's `Route` type,
and they are tested directly. The egui layer only renders state and dispatches
actions. This is what made it possible to validate the whole app end to end from
a headless harness (`examples/smoke.rs`) when synthetic UI input turned out not
to be deliverable in the development sandbox.

## Consequences

**Easier.** The driverless mode becomes usable without reading documentation:
launch, see your outputs, see your rooms, choose. Discovery failures, offline
speakers and conflicting assignments are now visible instead of being a silent
empty route list. The app and the service share one config file and one engine,
so there is no second implementation to keep in step.

**Harder.** There are now three interfaces to describe instead of two, and the
README has to be clear that they are not competing — the app configures the
driverless mode, it does not replace the driver. There is also a new failure
mode to explain: the app must keep running for audio to keep flowing, because it
*is* the streamer. The tray icon and the "keep it running" wording exist for
that reason, and the service remains the answer for people who want audio
without a logged-in session.

**Unchanged.** `sonosvad.sys`, `sonosvad-svc`, every `sonos-vad-*` crate, the
`cast` CLI and the `sonoscast` service are untouched. Deleting the app would
leave both original modes fully functional.

**Follow-up.** The app assumes rooms discovered once stay discovered for the
lifetime of a scan; a household that changes while the window is open needs a
manual rescan. Automatic topology-change subscriptions (UPnP eventing) would
remove that, and are worth doing only if it proves annoying in practice.
