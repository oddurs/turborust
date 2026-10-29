---
id: 103
title: Supervision is edge-triggered and untestable without real processes
type: chore
status: doing
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

- [ ] All five pieces are done
- [ ] No existing test was changed to make a piece pass
- [ ] A durability soak of the whole suite is clean after the last piece
