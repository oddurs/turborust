---
id: 9
title: Overlay resends the whole log tail on every frame
type: chore
status: done
milestone: v1.0
assignee: Oddur Sigurdsson
created: 2026-09-08
updated: 2026-09-08
priority: p2
effort: m
area: overlay
---

## Problem

Each snapshot carries the last 150 output lines. During a noisy build that is a
few KB re-serialised and re-sent every 250ms, and the browser re-renders the whole
`<pre>` each time — which also fights the user's scroll position.

Fine on loopback, wasteful, and the scroll behaviour is a real annoyance.

## Proposal

Split output onto its own SSE endpoint that appends, keyed by the sequence number
already on every `LogLine`. The state snapshot then carries counts only.

## Acceptance criteria

- [x] Output streams incrementally; snapshots no longer carry log text
- [x] Scroll position is preserved while new lines arrive
- [x] Reconnecting backfills from the last seen sequence number

## 2026-09-08

Output moved to its own SSE endpoint. Snapshots now carry log_count only; lines stream from /__turborust/logs?after=<seq> and the browser keeps its own buffer, capped at 2000 lines.

Every LogLine already had a monotonic seq, so resuming after a reconnect is just re-opening with the last one seen — no backfill of the whole tail, and no duplicates.

Two things this fixed beyond bandwidth:

1. The pane no longer re-renders wholesale every 250ms, so it stops fighting the reader scroll position. It also only repaints when the drawer is actually open.

2. engine.log() used seq: u64::MAX as a placeholder, which would have sorted every turborust notice to the end and broken resumption outright. Those now draw from the same counter as child output, so notices interleave in the order things actually happened. That bug was invisible while the whole tail was re-sent each frame.

Verified: /__turborust/logs?after=0 returns seq-tagged lines and the state frame no longer contains them.
