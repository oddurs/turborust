---
id: 20
title: 'on-busy-update: restart is the only policy'
type: feature
status: done
milestone: v1.0
assignee: Oddur Sigurdsson
created: 2026-09-08
updated: 2026-09-08
priority: p3
effort: m
area: engine
---

## Problem

A change to a watched file always restarts the node. That is the right default and
the only option. Some processes want a signal instead (a config reload on SIGHUP),
some want the current run to finish first, and some want nothing at all.

## Proposal

watchexec exposes `--on-busy-update` with `queue`, `restart`, `signal` and
`do-nothing`. Mirror it per node:

```toml
[services.api]
on_change = "restart"   # queue | restart | signal | ignore
signal = "SIGHUP"       # when on_change = "signal"
```

`queue` in particular is what a long task wants: finish this run, then run once
more for the changes that arrived during it — rather than being killed mid-flight.

## Acceptance criteria

- [x] Four policies, `restart` remaining the default
- [x] `signal` sends to the process group, not just the shell
- [x] `queue` coalesces changes arriving during a run into exactly one re-run

## 2026-09-08

Four policies on services: restart (default), queue, signal, ignore.

Implementing this surfaced a real bug, which is most of the value here. probe_until never read the control queue, so a stop issued while a node was still starting was not seen until the readiness probe timed out. Ctrl-c during a cold cargo build looked like a hang for the full ready_timeout, up to 60s. probe_until now selects on the control queue as well as its sleep, and a new supervisor test asserts shutdown-during-startup completes in under 10s.

That same change is what makes restart and queue actually differ. Before it, nothing could interrupt startup, so every policy behaved like queue. Now restart interrupts a startup already in progress, and queue declines to be interrupted — which is the point of queue: a cold build should not be killed halfway and started again from scratch.

signal sends to the process group rather than the shell, so a config reload reaches the real process; it is Unix-only and says so on Windows, where a job object can only be terminated. Signal names accept SIGHUP, HUP or a number.

Once a service is running, queue and restart agree — there is no startup left to protect — and that is commented at the match arm so it does not read as a missing case.
