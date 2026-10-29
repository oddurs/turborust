---
id: 106
title: The engine can only run real processes
type: chore
status: done
created: 2026-10-29
updated: 2026-10-29
priority: p1
effort: m
area: engine
part_of: [103]
---

## Problem

Spawning goes straight to `proc::spawn_with` and probes straight to the network, so the engine above the pure core can only be exercised with real children and real time.

## Acceptance criteria

- [x] Spawning and probing are traits with the real implementation as the default
- [x] A fake implementation drives whole-engine tests on tokio's paused clock, without processes
- [x] At least the dependency cascade, crash limits and timeouts are covered that way

## 2026-10-29

`src/host.rs`: a `Host` trait (spawn, probe, reclaim a port) and a `Child` trait
(pid, exit code, stop, kill, signal, and an optional pty for attaching). `Real`
is `proc::spawn_with`, the network and command probes, and the port wait,
moved behind it unchanged; `Engine::with_host` takes any other. Every process
the engine starts goes through it: services, tasks, and `verify`'s re-runs.
`connect` and the TUI's input mode reach the pty through `Child::terminal()`,
which a fake does not have.

`host::fake::FakeHost` scripts children (run forever, exit after a time with a
code, fail to spawn), switches probes, holds ports, and records spawns and
their times. It is public, not `cfg(test)`, because integration tests are a
separate crate.

`tests/engine_sim.rs` runs the whole engine on it under `start_paused`: 10 tests
in about half a second, covering the dependency cascade (one restart each),
backoff measured to the millisecond in virtual time, the crash limit and a
manual restart after it, a ten-minute task timeout in under five real seconds,
readiness timing out at exactly `ready_timeout`, liveness taking a dependency
and its dependent down and back, a held port, a failing spawn that a shutdown
no longer has to wait out, signalling, and shutdown order. Mutation-checked:
stopping the backoff doubling, and ignoring an unready dependency, each fail a
test.

`tokio`'s `test-util` feature is a dev-dependency only; the shipped binary is
built without it. Not done here: removing the 20ms exit polling, which needs an
exit notification from the pty reader rather than a trait.
