---
id: 48
title: 'Keep going after a failure: --continue'
type: feature
status: done
milestone: v1.2
assignee: Oddur Sigurdsson
depends_on:
- 49
created: 2026-09-08
updated: 2026-09-08
priority: p2
effort: s
area: engine
---

## Problem

`run` stops at the first failing task. In CI that means one broken crate hides
every other failure, so a fix-and-push cycle discovers them one at a time.

## Design

`--continue`: a failure marks that node failed and skips everything downstream of
it, but every node not blocked by it still runs. At the end, report all failures
together and exit non-zero.

The supervision tree already has the right shape: a failed node never reports
ready, so its dependents already stay parked. What has to change is
`await_tasks`, which returns on the first `Status::Failed`. Under `--continue` it
waits until every task has reached a terminal state, then reports.

"Terminal" has to include *blocked* — a task whose dependency failed will never
run, and waiting for it would hang. The state to detect is "failed, or waiting on
something that failed", which is a reachability question over the same reverse
index 0044 builds.

The summary at the end matters as much as the flag: a list of which tasks failed,
and which were skipped because of which failure. Otherwise `--continue` just
produces more output to scroll through.

## Acceptance criteria

- [x] `--continue` runs everything not blocked by a failure
- [x] Tasks downstream of a failure are reported skipped, not failed
- [x] The run exits non-zero and lists every failure
- [x] A graph where one branch fails and another succeeds does not hang

## 2026-09-08

Implemented, and it uncovered a bug considerably more interesting than the feature.

The feature: `--continue` waits for every task to settle, where settled includes *blocked* — a task whose dependency failed will never run, so waiting for it to reach a terminal status would hang forever. Blocked-ness is transitive_dependents of the failed set, which is what 0049 was built for. The run reports which tasks failed and which were skipped because of them, and exits with the first failure own code.

The bug: a node waiting on a dependency never observed `Stop`. `rx.wait_for(|v| *v)` on a watch channel that will never become true blocks forever and cannot see the control queue, so shutdown burned that node entire stop timeout — two blocked nodes cost twenty seconds. Exactly the shape of the probe_until bug from 0020, in the one remaining place that awaited without watching the mail. The dependency wait now selects on both, and the run_path suite went from 20.1s to 0.2s as a side effect. It has its own supervisor test.

A test flake that was also a real lesson: shutdown_stops_nodes_in_reverse_dependency_order failed about 1 run in 7 with only db recorded. Both nodes were Stopped, so ordering was correct — but api trap had never fired. The services had no health probe, so "healthy" meant merely "spawned", and SIGTERM could arrive before the shell had executed its `trap` line. api is stopped first, so it lost that race and db did not. Fixed by giving both services a log readiness probe, so ready genuinely means the trap is installed. 20 consecutive green runs after.

Worth keeping: the flake was not testing noise. It was the difference between "the process exists" and "the process is ready" — the exact distinction this project was built around — showing up in its own test suite.
