---
id: 110
title: Why cannot say what happened when
type: feature
status: backlog
created: 2026-10-29
updated: 2026-10-29
milestone: later
priority: p2
effort: m
area: engine
---

## Problem

The engine now records every lifecycle change in `state.history` (0107), but nothing a user sees reads it. `why` explains only the current status, the TUI shows only the latest reason, and the run summary cannot say that a service restarted three times before it settled.

## Acceptance criteria

- [ ] `why <node>` prints that node's recent history, with times
- [ ] The control API serves the history, so the overlay can show it
- [ ] The run summary counts restarts and failures from the history rather than from counters
