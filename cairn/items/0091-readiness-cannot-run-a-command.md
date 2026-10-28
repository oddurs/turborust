---
id: 91
title: Readiness cannot run a command
type: feature
status: done
created: 2026-10-28
updated: 2026-10-28
priority: p1
effort: s
area: engine
---

## Problem

A readiness probe can connect to a port, GET a URL, or wait for a log line. It
cannot run a command, which is how most databases say they are ready. Postgres
accepts TCP connections before it accepts queries — a connection during startup
gets `FATAL: the database system is starting up` — so `health = { tcp = 5432 }`
passes too early and the first migration fails, blaming itself. The honest probe
is `pg_isready`; the same goes for `redis-cli ping` and `mysqladmin ping`.

## Precedent

process-compose's `readiness_probe.exec`, Docker's `HEALTHCHECK CMD`, and
Kubernetes' exec probes: exit 0 is ready, anything else is not yet.

## Proposal

```toml
health = { cmd = "pg_isready -h 127.0.0.1 -p 5432" }
```

Run with the node's environment and `cwd`, a short per-attempt timeout, at the
probe's `interval`. Composes with the other probes as they already compose: all
declared must pass. Output goes to the node's log only on failure after
`ready_timeout`, so a slow start is not a wall of noise.

## Acceptance criteria

- [x] `health.cmd` passes on exit 0 and retries otherwise, until `ready_timeout`
- [x] It runs with the node's environment and working directory
- [x] A probe command that hangs is killed after a per-attempt timeout
- [x] A fixture shows `tcp` passing before `cmd` does (a listener standing in for Postgres; see the note)

## 2026-10-28

`health.cmd` runs through the shell with the node's own environment (strict
isolation included) and `cwd`, at `interval`, each attempt cut off after
`health.timeout` (default `5s`) with `kill_on_drop` so a hung probe takes no
process with it. If readiness gives up, the last three lines the command printed
are logged as "last readiness attempt: …" — the one moment that output is worth
its noise.

The Postgres criterion is met with a stand-in, and the criterion's text now says
so: the test opens a plain TCP listener (what Postgres looks like mid-startup)
and holds the probe command failing until a marker file appears, then shows the
node stays `starting` with the port open and goes ready only once the command
passes. A real Postgres would need a service container on three CI platforms to
prove the same ordering.

`health.interval` and `health.timeout` are now parsed at plan time, where a typo
used to fall back to a default — the `0086` rule, extended to probes.

Mutation-checked: ignoring `cmd` fails all four new tests; dropping the timeout
fails the hang test.
