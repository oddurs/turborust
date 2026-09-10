---
id: 6
title: Integration harness for the supervisor
type: chore
status: done
milestone: v0.1
assignee: Oddur Sigurdsson
created: 2026-09-08
updated: 2026-09-08
priority: p1
effort: m
area: testing
---

## Problem

The supervision tree — dependency ordering, readiness cascade, restart coalescing,
backoff, shutdown — is the most intricate part of the project and is verified by
hand with a shell script against `examples/fullstack`. Every regression in it so
far was found by running that script and reading the log, not by a test.

## Proposal

A harness that builds a `Plan` in-memory from a TOML string, points nodes at
trivial shell commands, drives the engine, and asserts on the state timeline
rather than on wall-clock sleeps.

Worth covering:

- a dependent starts only after its dependency reports healthy
- one edit that matches several nodes produces exactly one restart each
- crash backoff doubles and is capped
- shutdown stops nodes in reverse dependency order

## Acceptance criteria

- [x] `tests/supervisor.rs` exists and runs in CI time (< 10s)
- [x] The four behaviours above are asserted
- [x] No `sleep` longer than the poll interval

## 2026-09-08

Partially covered by tests/run_path.rs (added with 0004): dependency ordering, exactly-once execution across a diamond, service startup/shutdown around a one-shot run, and failure propagation. Still uncovered and the reason this stays open: restart coalescing under a watch event, crash backoff doubling and its cap, and reverse-order shutdown of a running `up`. Those need the engine driven with a synthetic clock rather than real sleeps.

## 2026-09-08

tests/supervisor.rs drives the real engine against real processes in a temp workspace, asserting on the state timeline rather than on wall-clock sleeps. 7 tests, 4.0s.

Covered: readiness gating (a dependent stays parked until its dependency is healthy), log-announced readiness and its negative case, one-edit-one-restart under a live watcher, backoff doubling and its cap, reverse-order shutdown, and no-spawn-after-stop.

Two things this required, both improvements in their own right:

- Watch dispatch moved out of main.rs into engine::spawn_watch_dispatch. It decides 'one edit, one restart', which is precisely the behaviour needing cover, and it was unreachable from a test while it lived in the binary. main.rs is now thin CLI.
- Harness::wait_for panics with the whole node timeline rather than a bare timeout, because a supervisor test that just says 'timed out' tells you nothing.

Reverse-order shutdown is asserted by having each service trap TERM and append its name to a file; the file's contents are the assertion.

The backoff test first ran to a 15s deadline waiting for a fourth distinct delay — which never comes, because not changing once capped IS the property. Rewritten to sample changes over a fixed 4s window, which also brought the suite under the 10s budget in the acceptance criteria.
