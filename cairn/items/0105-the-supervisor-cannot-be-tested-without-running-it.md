---
id: 105
title: The supervisor cannot be tested without running it
type: chore
status: done
created: 2026-10-29
updated: 2026-10-29
priority: p1
effort: xl
area: engine
part_of: [103]
---

## Problem

`supervise()` is a 321-line async loop that decides and acts in the same breath: spawning, probing, sleeping and status writes are interleaved with the decisions, so the only way to test a decision is to run real processes and wait. Tasks and services share the loop, with a task branch inside it.

## Acceptance criteria

- [x] The service lifecycle is a pure `step(state, event, now) -> actions` with no I/O
- [x] Tasks have a job machine of their own, not a branch in the service loop
- [x] Liveness, self-reload, crash limits and backoff are states and timers of the machine, not side structs
- [x] Deterministic tests drive the machine through event sequences, including every regression the integration suite covers
- [x] Every existing integration test passes unchanged

## 2026-10-29

`src/lifecycle.rs` holds two pure machines, `Service` and `Task`:
`on(event, deps, now) -> Vec<Action>`. No tokio, no I/O, no clock of its own —
time is a parameter and dependencies are a snapshot (`Deps`). The executor in
`engine.rs` (`supervise_service`, `supervise_task`) feeds them events from the
child, timers, the board and the control queue, carries out each action, and
reports any result as the next event. `CrashCount`, `give_up`, `SelfReload`,
`Liveness`, `probe_until`, `wait_for_deps` and `wait_for_rerun` are gone; their
logic is states and timers of the machines. `engine.rs` went from 2,412 lines
to 1,917.

The spec was the old loop, read line by line, and the oracle was the old suite:
every supervisor, run-path and signals test passes unchanged, and a 60-run
soak of those three suites (three in parallel) was clean.

`tests/lifecycle.rs` drives the machines directly: 29 tests, the whole file in
about 10ms, including a fixed-seed walk of 60,000 random events checking what
must always hold. It found a divergence before any process ran: the first
version reset the crash backoff whenever a service became healthy, but the old
loop resets it only when a readiness probe passes. A service with no probe is
"healthy" the instant it spawns, so that version would have pinned a crash
loop's backoff at the minimum forever; `crash_backoff_doubles_and_is_capped`
caught it.

Behaviour changed in one place, on purpose: the backoff after a failed spawn
was a plain sleep that a shutdown had to wait out; it is now the machine's
backoff state, which a stop ends at once. Two timing details are not identical
but are not decisions: a self-reload rebuild already under way at startup is
picked up at once rather than one poll interval later, and the next liveness
check is timed from the end of the last rather than on a fixed grid.

Idle cost is unchanged (an idle `up` of four services: 58 ticks in 20s before,
63 after, within noise); what it does cost is the 20ms exit polling `0106` can
remove.
