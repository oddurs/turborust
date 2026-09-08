---
id: 16
title: Control queue has no priority, so Stop can be starved
type: bug
status: done
milestone: v0.1
assignee: Oddur Sigurdsson
created: 2026-09-08
updated: 2026-09-08
priority: p1
effort: m
area: engine
---

## Problem

`Ctl` is a flat unbounded mpsc drained FIFO. During shutdown we send `Ctl::Stop`
to every node, but any `Ctl::Restart` already queued by a watch event is processed
first — so the supervisor stops the child, loops, waits for dependencies, and
**spawns a fresh process while we are trying to exit**, only then seeing `Stop`.

Save a file and hit ctrl-c immediately and this is the path taken.

## Proposal

watchexec (Apache-2.0) solves this with a three-level priority queue modelled on
`systemctl`, and — the part that matters most — while a graceful-stop timer is
running, the *normal* priority queue is disabled entirely, so nothing can jump in
front of a stop already in progress.

Adopt the same shape:

- `Urgent` — Stop
- `High` — dependency edges
- `Normal` — watch-driven restarts, suppressed once a stop is in flight

## Acceptance criteria

- [x] Stop is never processed after a restart that was queued before it
- [x] No process is spawned once shutdown has begun
- [x] A test queues Restart then Stop and asserts no respawn

## 2026-09-08

Replaced the flat mpsc with a three-level priority queue in a new src/ctl.rs: Urgent (Stop), High (dependency edges), Normal (watch-driven restarts).

Two rules, and the second matters as much as the ordering:

1. Urgent and High are drained before Normal, so a Stop queued behind a pile of Restarts is still handled first.
2. Once a Stop has been sent, the sender REFUSES further Normal messages outright. Ordering alone is not enough — a restart arriving after the stop was drained would still give a supervisor something to act on after it should have exited. Dependency edges keep flowing during shutdown because they carry real information (a dependency's child may already be gone); only restarts are suppressed.

The supervisor also now checks for a pending stop immediately after unblocking on dependencies and returns instead of spawning. That is the specific hole: a node parked waiting for a dependency would wake, find the world had moved on, and start a process on the way out.

Shape is modelled on watchexec's supervisor (Apache-2.0), which takes it from systemctl; the implementation is our own.

Verified: touched a watched file and sent SIGINT in the same instant — the exact race — and the process exited in under a second with no leftover listeners and no orphaned children.
