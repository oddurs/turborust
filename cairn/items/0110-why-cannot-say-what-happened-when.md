---
id: 110
title: Why cannot say what happened when
type: feature
status: done
created: 2026-10-29
updated: 2026-10-29
milestone: later
priority: p2
effort: m
area: engine
---

## Problem

The engine now records every lifecycle change in `state.history` (0107), but nothing a user sees reads it. `why` explains only the current status, the TUI shows only the latest reason, and the run summary cannot say that a service restarted three times before it settled.

## Acceptance criteria

- [x] `why <node>` prints that node's recent history, with times
- [x] The control API serves the history, so the overlay can show it
- [x] The run summary counts restarts and failures from the history rather than from counters

## Notes

The history lives in the `up` process, and `why` is a separate one, so the
control socket gained a second request beside `attach`: `history`, with an
optional node and a limit, answered with `HistoryRecord`s (sequence,
milliseconds into the session, node, what happened). `control::history`
returns `None` when nothing is running, so `why` prints its cache explanation
as before and adds "recently, in the running `up`" only when there is one.
The dashboard serves the same records at `/__turborust/history`, read-only
like the state stream beside it, with `?node=` and `?limit=`.

The run summary's new `nodes` map counts restarts and failures (`failed` and
`unhealthy`) per node from the facts as they are recorded. Counted at record
time rather than by scanning, because the history summarises its oldest
entries into a base state once it passes 10,000, and a scan would then lose
them; a test checks the count past a cap of 5.

Not done: the overlay does not render the history yet. The endpoint is what it
would read; drawing it is the overlay's own work.
