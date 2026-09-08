---
id: 7
title: Shutdown polls instead of awaiting supervisors
type: chore
status: done
milestone: v0.1
assignee: Oddur Sigurdsson
created: 2026-09-08
updated: 2026-09-08
priority: p2
effort: s
area: engine
---

## Problem

`engine::shutdown` sends `Ctl::Stop` and then polls shared state every 50ms for up
to 12 seconds, inferring completion from status values. The supervisor `JoinHandle`s
are collected into `_joins` and dropped.

It works, but it is a poll loop standing in for a join, and the 12s cap is a magic
number that silently truncates a slow stop.

## Proposal

Return the join handles from `spawn_supervisors` and await them, bounded by each
node's own `stop_timeout` rather than one global constant.

## Acceptance criteria

- [x] Shutdown awaits supervisor tasks
- [x] The 12s constant is gone
- [x] Ctrl-C during a cold `cargo build` still exits promptly

## 2026-09-08

Done alongside 0016, since both live in the same code path.

spawn_supervisors now returns a HashMap<String, JoinHandle> instead of an anonymous Vec, and shutdown awaits each supervisor by name in reverse dependency order. The bound is that node's own stop_timeout plus two seconds for reaping, so the 12-second global constant is gone — it was a magic number that silently truncated a slow stop and reported success.

If a supervisor does miss its window, that is now logged against the node rather than passing unnoticed.

The dependency-monitor tasks are dropped rather than tracked: dropping a tokio JoinHandle detaches the task rather than aborting it, and they end on their own when the readiness channels close. An earlier draft used mem::forget, which achieved the same detach but leaked the Vec and read as though something were being hidden.
