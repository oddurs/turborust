---
id: 107
title: What happened is overwritten, not recorded
type: chore
status: backlog
created: 2026-10-29
updated: 2026-10-29
priority: p1
effort: l
area: engine
part_of: [103]
---

## Problem

The shared `AppState` holds only the latest status of each node; every transition overwrites the last. `why`, the TUI and the run summary cannot say what happened when, and a failing test can only print its log text.

## Acceptance criteria

- [ ] The engine appends every transition to an event log
- [ ] The shared state is a projection of that log, built by one function
- [ ] The test harness dumps the event sequence on a timeout
- [ ] Projecting the recorded log reproduces the live state
