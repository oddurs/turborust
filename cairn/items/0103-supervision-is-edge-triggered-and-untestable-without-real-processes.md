---
id: 103
title: Supervision is edge-triggered and untestable without real processes
type: chore
status: done
created: 2026-10-29
updated: 2026-10-29
priority: p1
effort: xl
area: engine
---

## Problem

Every hard bug this tracker recorded in the supervisor came from one design:
dependents learn about a dependency from a stream of `DepUp`/`DepDown`
messages, derived from a `watch<bool>` that keeps only its latest value, and
interpreted by rules ("a running node ignores `DepUp`"). `0101` was that stream
losing an edge; the `generation` counter that fixed it is the second patch for
the same weakness, after a task that ran twice.

The supervisor itself is one 321-line `select!` loop with liveness, self-reload,
crash counting and timeouts bolted on as side structs, so its tests can only be
real processes against wall-clock sleeps, and two CI failures in one day
(`0092`, `0096`) were timing margins, not bugs. A node's state lives in two
places — `Status` in a shared mutex and the readiness channels — set
independently. Tasks and services share one loop with a task branch inside it.

## Plan

The pieces, each its own item and change, with the existing integration suite
as the oracle at every step:

- `0108` the roadmap renders on `main`, so these land without serialising on it
- `0104` readiness is derived from status and published with its generation,
  level-triggered, replacing `DepUp`/`DepDown` and the per-edge forwarders
- `0105` supervision is a pure state machine — `step(state, event) -> actions` —
  with a thin executor, and tasks get a job machine of their own
- `0106` process spawning and probing sit behind traits, so the engine runs
  against fakes on a paused clock
- `0107` an append-only event log, of which the shared state is a projection

## Acceptance criteria

- [x] All five pieces are done
- [x] No existing test was changed to make a piece pass
- [x] A durability soak of the whole suite is clean after the last piece

## Notes

All five pieces landed: 0108/0109 (#38, #40), 0104 (#39), 0105 (#41), 0106
(#42), 0107 (#43). The integration suites (`supervisor`, `run_path`,
`signals`, `errors`) passed unchanged at every step, which was the point of
using them as the oracle.

"No existing test was changed to make a piece pass" holds with one recorded
exception: 0104 deleted the two `ctl` tests for `DepUp`/`DepDown` along with
the messages themselves. They tested a mechanism that no longer exists, not
behaviour, and the behaviour they guarded is covered by the dependency tests
in `supervisor` and `engine_sim`. 0107 added assertions to the `supervisor`
harness (replay at shutdown, history on timeout); it loosened nothing.

The final durability soak was run on the tip of #43, with all five pieces in:
4 workers × 25 rounds = 100 full-suite runs (900 test-binary runs), 0
failures, 539s.

Still not done, and outside this item: the 20ms exit poll, which needs an exit
notification from the pty reader; and reading the history in `why`, the TUI
and the summary (0110).
