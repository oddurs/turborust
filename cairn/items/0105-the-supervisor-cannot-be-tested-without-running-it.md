---
id: 105
title: The supervisor cannot be tested without running it
type: chore
status: backlog
created: 2026-10-29
updated: 2026-10-29
priority: p1
effort: xl
area: engine
part_of: [103]
---

## Problem

`supervise()` is a 321-line async loop that decides and acts in the same breath: spawning, probing, sleeping and status writes are interleaved with the decisions, so the only way to test a decision is to run real processes and wait. Tasks and services share the loop, with a task branch inside it.

## Acceptance criteria

- [ ] The service lifecycle is a pure `step(state, event, now) -> actions` with no I/O
- [ ] Tasks have a job machine of their own, not a branch in the service loop
- [ ] Liveness, self-reload, crash limits and backoff are states and timers of the machine, not side structs
- [ ] Deterministic tests drive the machine through event sequences, including every regression the integration suite covers
- [ ] Every existing integration test passes unchanged
