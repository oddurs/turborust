---
id: 4
title: run <task> silently skips service dependencies
type: bug
status: done
milestone: v0.0
assignee: Oddur Sigurdsson
created: 2026-09-08
updated: 2026-09-08
priority: p0
effort: s
area: engine
---

## Problem

`Engine::run_targets` filters the resolved plan down to `Kind::Task` and runs only
those. If a task declares `depends_on = ["db"]` where `db` is a *service*, the
service is filtered out and the task runs without it — usually failing in a way
that blames the task.

`turborust up` handles this correctly through the readiness cascade; only the
one-shot `run` path is affected.

Found while reviewing `run_targets`, which also carries a dead `let _ = targets;`
left over from an earlier signature.

## Proposal

In `run`, start any service dependencies, wait for their readiness probes, run the
tasks, then stop the services in reverse order. That is the supervision tree the
`up` path already builds, so the fix is to reuse it rather than special-case it.

## Acceptance criteria

- [ ] A task depending on a service gets that service running before it starts
- [ ] Services started by `run` are stopped again before the process exits
- [ ] A test covers task-depends-on-service in the `run` path
- [ ] `let _ = targets;` is gone

## 2026-09-08

Fixed by routing `run` through the same supervision tree as `up` (engine::run_once) instead of a separate sequential loop. Services in the target closure are supervised normally, so readiness gating comes for free, and shutdown stops them in reverse dependency order.

Two things surfaced while fixing it:

1. A dead service dependency made the run hang forever rather than fail — dependents wait on a readiness edge that never comes. `await_tasks` now treats a service in Failed or Backoff as a failed run. A crashed dependency is a failure in one-shot mode, not something to ride out.

2. A task could run TWICE. Dependency wake-ups were acted on as messages, and a DepUp arriving a moment after `drain_stale` caused a second identical run. Fixed by comparing state instead of trusting message order: NodeState now carries a `generation` bumped on Done/Healthy, and a parked node only re-runs when a dependency's generation differs from the one it consumed. This was pre-existing and also affected `up`; it was just invisible there because a duplicate restart looks like a slow restart.

`--force` moved onto the Engine (AtomicBool) because task execution is driven by supervisors, which have no argument to thread through.

Verified the tests fail against the old code: 'task ran without its service dependency' and 'a dead dependency must fail the run' both trip. 10 consecutive green runs after.
