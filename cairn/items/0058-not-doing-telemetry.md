---
id: 58
title: 'Not doing: telemetry'
type: chore
status: dropped
milestone: later
created: 2026-09-08
updated: 2026-09-08
area: project
---

## Considered and declined

turborepo has `--no-update-notifier`, `--anon-profile` and a telemetry subsystem.

## Why not

A dev tool sits between a developer and their source. Phoning home from that
position requires a trust budget this project has no reason to spend, and no
question it needs answered badly enough to spend it.

If usage data is ever genuinely needed, the honest form is a command the user runs
deliberately to produce a report they can read first — not a background collector
with an opt-out.
