---
id: 107
title: What happened is overwritten, not recorded
type: chore
status: done
created: 2026-10-29
updated: 2026-10-29
priority: p1
effort: l
area: engine
part_of: [103]
---

## Problem

The shared `AppState` holds only the latest status of each node; every transition overwrites the last. `why`, the TUI and the run summary cannot say what happened when, and a failing test can only print its log text.

## Acceptance criteria

- [x] The engine appends every transition to an event log
- [x] The shared state is a projection of that log, built by one function
- [x] The test harness dumps the event sequence on a timeout
- [x] Projecting the recorded log reproduces the live state

## Notes

A fact is a lifecycle change: status, reason, a counted restart, a spawn
(pid and start time), a pid change. `AppState::record` appends one to
`state.history` and applies it with `state::apply`, which is the only code
that writes those fields and also what `History::project` replays. That is
why the projection cannot drift from the live state: there is no second
writer to drift. The generation bump and the diagnostics reset moved out of
`Engine::set` into `apply` for the same reason. `Engine::set` keeps its no-op
for a repeated non-ready status, so the history carries no echoes.

Output lines are deliberately not facts. They already sit in their own
append-only store, and the counters derived from them (`ready_matches`,
`blocked_on_cargo_lock`, diagnostics) are views of output, not lifecycle.

The history is bounded like the output store (10,000 facts) but does not
forget: an evicted fact is folded into a base state, so base plus the kept
tail is still the whole history and `project()` stays exact after wrapping.
A plain ring buffer would make a long `up` unprojectable, which defeats the
point.

Replay is checked at the end of every `engine_sim` scenario (fake host, paused
clock) and every `supervisor` test (real processes). Both harnesses print the
numbered, timestamped history when a wait times out. Facts are stamped with
tokio's clock, so a simulated run reads in virtual time. Mutation-checked:
writing a restart directly instead of recording it fails two simulations, and
evicting without folding fails the cap test.

Not done: `why`, the TUI and the run summary do not read the history yet. The
history is the record they need; using it is follow-up work.
