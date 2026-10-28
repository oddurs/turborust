---
id: 91
title: Readiness cannot run a command
type: feature
status: backlog
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

- [ ] `health.cmd` passes on exit 0 and retries otherwise, until `ready_timeout`
- [ ] It runs with the node's environment and working directory
- [ ] A probe command that hangs is killed after a per-attempt timeout
- [ ] A Postgres fixture shows `tcp` passing before `cmd` does
