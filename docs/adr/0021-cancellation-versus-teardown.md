# 0021 — Cancellation and teardown are separate signals

- Status: **Accepted**
- Date: 2026-07-26
- Relates to: [ADR-0018](./0018-loopback-rename-and-service-hosting.md),
  [ADR-0008](./0008-cli-config-and-signals.md),
  [ADR-0020](./0020-runtime-routing-app.md)

## Context

Building the routing app surfaced a bug that had been latent in the multi-room
path since it was written, and that the app made obvious because the app shows
what the worker is doing.

The symptom: a route to an unreachable speaker failed once, the worker logged
`retrying error=... retry_in_secs=5`, and then the worker **stopped** instead of
retrying. Every mode was affected — `sonos-driver cast`, the `sonoscast`
service and the new app — but only the app displayed it, as a stream that
silently gave up after one failure.

The cause was a conflation of two things that happen to have the same type.

`Bridge` and its session code use an `Arc<AtomicBool>` as *its own teardown
flag*: when a session ends, for any reason, it sets the flag to unwind its own
worker threads. That is correct and local.

`MultiRoomBridge` uses an `Arc<AtomicBool>` for something quite different: the
*cancellation* signal adopted from the caller's `StopHandle`, meaning "the user
pressed Ctrl-C / clicked Stop / the service is shutting down".

`run_sessions_with` then handed every session `Arc::clone(&self.stop)` — the
same allocation. So the moment any one room's bridge finished, for any reason
including success, it latched the caller's cancellation flag. The retry loop in
`worker::run` checks that flag before sleeping, correctly concluded it had been
asked to stop, and exited for good.

Both signals are "a bool that means stop", which is exactly why sharing them
looked reasonable. They are not the same:

|  | **Cancellation** | **Teardown** |
| --- | --- | --- |
| Who raises it | outside the operation | the operation itself |
| Direction | top-down | bottom-up |
| Scope | the whole run | one session |
| Lifetime | one-way, permanent | ends with the session |
| Meaning | "do not start more work" | "my threads may exit" |

A signal that travels in both directions through the same variable will
eventually be read as the wrong one, and it was.

## Decision

**Give every session its own teardown flag, and relay cancellation into them
rather than sharing one flag with them.**

`MultiRoomBridge::run_sessions_with` now creates a fresh `Arc<AtomicBool>` per
session. A session setting its own flag affects nothing but itself. Cancellation
travels in one direction only: a single relay thread watches the supervisor's
stop flag and, when it is raised, raises each session's flag in turn. The relay
polls at 50 ms, which is far below any human-perceptible shutdown delay and
costs one sleeping thread for the lifetime of a run; it is shut down after the
sessions are joined.

The invariant this establishes, and which the regression tests assert:

> A session ending — successfully or not — must never be observable as the
> caller having requested a stop.

## Consequences

**Fixed.** Retry works in all three modes. A room that is powered off, or a
transient network failure, no longer permanently kills the stream — which was
the difference between a demo and something that can be left running. This was
the single most damaging bug found in the project so far, and it was invisible
from the CLI because the CLI's normal ending *is* a stop.

**Easier.** The two concepts are now distinguishable in the code, so future
changes to shutdown behaviour have an obvious place to go. `is_stop_requested()`
now means only what its name says.

**Harder.** There is one extra thread per multi-room run, and cancellation is
delivered up to 50 ms later than before. Neither matters at these timescales,
but a future design wanting instant fan-out should use a broadcast primitive
rather than lowering the poll interval.

**Lesson worth keeping.** The bug survived a green test suite because the
existing test asserted the flags *were* shared — it encoded the defect as the
specification, in a test whose name said so out loud. The tests now assert the
behaviour that matters (a finished session does not cancel the run; an external
stop still reaches every session) rather than the mechanism that implements it.
