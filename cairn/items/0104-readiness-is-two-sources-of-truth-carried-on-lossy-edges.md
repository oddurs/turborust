---
id: 104
title: Readiness is two sources of truth, carried on lossy edges
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

A node's `Status` and its readiness channel are set separately, eleven `send_replace` calls beside the status writes, and nothing makes them agree. Readiness then reaches dependents as edges a monitor task turns into `DepUp`/`DepDown`, and edges can be lost (`0101`).

## Acceptance criteria

- [ ] Readiness is a function of status, published by the one place that sets status
- [ ] Each node publishes its readiness with its generation; dependents compare generations, not messages
- [ ] `DepUp`/`DepDown` and the per-dependency monitor tasks are gone
- [ ] Every existing supervisor and run-path test passes unchanged
