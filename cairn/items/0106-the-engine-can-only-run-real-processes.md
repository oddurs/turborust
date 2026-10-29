---
id: 106
title: The engine can only run real processes
type: chore
status: backlog
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

- [ ] Spawning and probing are traits with the real implementation as the default
- [ ] A fake implementation drives whole-engine tests on tokio's paused clock, without processes
- [ ] At least the dependency cascade, crash limits and timeouts are covered that way
