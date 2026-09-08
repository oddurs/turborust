---
id: 38
title: A task could run twice from a stale dependency wake-up
type: bug
status: done
milestone: v0.0
created: 2026-09-08
updated: 2026-09-08
area: engine
effort: m
---

## What happened

`tasks_run_in_dependency_order` failed 3 runs in 5 with the log reading
`first second second`.

Dependency wake-ups were acted on as *messages*, and a `DepUp` arriving a moment
after `drain_stale` triggered a second identical run. Message ordering is not a
sound basis for that decision — there is always a window.

Pre-existing, and it affected `up` as well as `run`. It was invisible there
because a duplicate restart just looks like a slow restart.

## Fix

Compare state rather than trusting message order. `NodeState` carries a
`generation` bumped on `Done`/`Healthy`, and a parked node re-runs only when a
dependency's generation differs from the one it consumed. A stale message now
finds an unchanged generation and is ignored.

Covered by `a_diamond_runs_each_task_exactly_once`, which asserts an exact line
count rather than containment — a containment check would have waved this through.
