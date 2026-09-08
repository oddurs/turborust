---
id: 43
title: Nothing bounds how many builds run at once
type: bug
status: done
milestone: v1.1
assignee: Oddur Sigurdsson
created: 2026-09-08
updated: 2026-09-08
priority: p1
effort: m
area: engine
---

## Problem

Every node whose dependencies are ready is spawned immediately. There is no limit
anywhere in the supervisor. On a twenty-crate workspace with per-crate check
tasks, `up` launches twenty cargo processes at once, and the machine thrashes
rather than building.

turborepo defaults to a concurrency cap for exactly this reason.

## Design

```toml
[project]
concurrency = 4     # default: available_parallelism, capped at 8
```

A semaphore acquired before spawning, released once the node reaches a terminal
state (a task finishing, a service becoming healthy). Services hold a permit only
while *starting*, not for their whole life — a permit held by a long-running
server would deadlock the graph the moment `concurrency` services were up.

That is the subtle part and the reason this is worth designing rather than
sprinkling a semaphore in: the resource being limited is *build parallelism*, not
process count, and a service is expensive only until it is ready.

**Interaction with cargo's own lock, stated honestly.** Nodes sharing a target
directory already serialise inside cargo, so the cap changes little there — it
mainly stops the pile-up of processes waiting on that lock, which is what makes
the machine unusable. The cap matters most for split target dirs (0010) and for
non-cargo tasks, which have no lock of their own.

The TUI and overlay should show a node waiting on a permit as `waiting: slot`,
distinct from `waiting: <dep>` — otherwise a bounded queue looks like a hung
dependency, which is the same class of confusion the cargo-lock indicator exists
to prevent.

## Acceptance criteria

- [x] `concurrency` bounds simultaneous node startup
- [x] A running service does not hold a permit
- [x] More services than permits still all reach healthy (no deadlock)
- [x] Waiting on a permit is visibly distinct from waiting on a dependency
- [x] A test asserts the cap is respected under a graph wider than the cap

## 2026-09-08

A semaphore, with the whole design living in one decision: what exactly is being limited.

Building, not running. A task holds a permit for its execution; a service holds one only from spawn until it reports ready, released at the Healthy transition rather than at exit. Holding one for a service lifetime would deadlock the graph the moment `concurrency` services were up and a dependent still needed a slot — so that case has its own test with concurrency = 1 and four services, which hangs under the wrong design and passes under this one.

RAII does the releasing, so the early returns in the readiness path — interrupted by a stop, or by a dependency going down mid-startup — cannot leak a permit.

New Status::Queued renders as `waiting: slot` and, in the TUI, as an amber ◔ rather than a grey ○: it is work about to happen, not work stuck behind something else. The test asserts it is actually observed, since a limit nobody can see is indistinguishable from a hang.

Default is available_parallelism capped at 8. The cap is the point — twenty parallel rustc invocations exhaust memory long before they exhaust cores. concurrency = 0 falls back rather than being taken literally, since a semaphore with no permits would hang everything.

Honest note carried over from the design: nodes sharing a target directory already serialise inside cargo, so the cap mainly stops the pile-up of processes waiting on that lock. It matters most for split target dirs (0010) and non-cargo tasks.
