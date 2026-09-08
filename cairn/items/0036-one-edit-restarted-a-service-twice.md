---
id: 36
title: One edit restarted a service twice
type: bug
status: done
milestone: v0.0
created: 2026-09-08
updated: 2026-09-08
area: engine
effort: s
---

## What happened

Editing a shared crate matched both the `check` task and the `api` service,
because both derive their globs from the same cargo closure. The service restarted
immediately on its own watch hit, then again when the task finished and readiness
cascaded.

Log showed the symptom plainly:

    api | restarting: crates/shared/src/lib.rs
    api | restarting: dependency came back

## Fix

Two parts. Dispatch signals only the upstream-most affected nodes, leaving the
dependency edges to propagate. And a node already running ignores a `DepUp`: it is
either the edge that started it or the tail of a `DepDown` already acted on, so
restarting there doubles every cascade.
