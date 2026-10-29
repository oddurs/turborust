---
id: 104
title: Readiness is two sources of truth, carried on lossy edges
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

A node's `Status` and its readiness channel are set separately, eleven `send_replace` calls beside the status writes, and nothing makes them agree. Readiness then reaches dependents as edges a monitor task turns into `DepUp`/`DepDown`, and edges can be lost (`0101`).

## Acceptance criteria

- [x] Readiness is a function of status, published by the one place that sets status
- [x] Each node publishes its readiness with its generation; dependents compare generations, not messages
- [x] `DepUp`/`DepDown` and the per-dependency monitor tasks are gone
- [x] Every existing supervisor and run-path test passes unchanged

## 2026-10-29

`Engine::set` is now the only writer of status and the only source of
readiness: readiness is `Status::is_ready()` (`Healthy` or `Done`), and a write
ticks one board version (`watch<u64>`). The eleven `send_replace` calls, the
per-node readiness channels, the per-dependency monitor tasks and
`Wire::ready_rx` are gone; `Wire` carries only the control queue.

Dependents are level-triggered. On any board tick a supervisor recomputes from
a snapshot: a dependency not ready means stop ("dependency `x` is rebuilding"),
a dependency whose generation moved past what this process started against
means restart ("... produced a new result"). That covers what `DepDown`,
`DepUp` and `missed_down` did between them, and there is no stream of edges
left to drop one from.

Two consequences that needed care:

- A status that used to stay up while readiness went down needed a status of its
  own: `Stopping`, while a child is stopped for a restart. A task now writes
  `Running` before its run, so a cache hit, which otherwise never leaves `Done`,
  still takes it out of ready and stops its dependents for the duration.
- `set` is a no-op for an unchanged non-ready status. Without that, a node
  waiting on a dependency writes `Waiting`, the tick wakes it, it writes
  `Waiting` again, forever. Checked by CPU time, not only by reasoning: an idle
  `up` of four services and a failed task used 55 ticks in 20s both before and
  after this change.

`ctl` lost `DepUp`/`DepDown` and the priority between them, and with them two of
its own unit tests (`dependency_edges_outrank_restarts`,
`dependency_edges_still_flow_during_shutdown`), whose subject no longer exists:
dependency changes do not travel through the control queue at all, so neither
ordering against restarts nor suppression during shutdown applies. No
integration test changed; all 36 supervisor and 25 run-path tests pass as they
were.
