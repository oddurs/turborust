---
id: 92
title: A service that stops answering stays healthy
type: feature
status: backlog
created: 2026-10-28
updated: 2026-10-28
priority: p1
effort: m
area: engine
---

## Problem

Readiness is checked until a service first passes, and never again. A server
that deadlocks, exhausts its pool or wedges on a lock keeps reading `healthy`
in the dashboard and the overlay, and its dependents are never told. The
supervision tree's whole job is knowing what is ready, and after startup it
stops looking.

## Precedent

process-compose's `liveness_probe` with `failure_threshold`; Docker
`HEALTHCHECK` retries turning a running container `unhealthy`; Kubernetes
liveness probes. All three keep probing a running process and act only after
several consecutive failures, so one slow response is not an outage.

## Proposal

An opt-in `liveness` probe, same shape as `health`, with `failures` (default 3)
and `every` (default 10s). On that many consecutive failures the node becomes
`unhealthy` — the state `0073` introduced — and the readiness cascade runs as it
does for a restart. `restart = "always"` then restarts it; otherwise it stays
unhealthy until it answers again. Off by default: a probe nobody asked for is a
restart nobody expected.

## Acceptance criteria

- [ ] A failing liveness probe marks a running node `unhealthy` after `failures` misses, not one
- [ ] Dependents see it go down, and come back with it
- [ ] With `restart = "always"` the node is restarted; otherwise it recovers by itself when the probe passes
- [ ] No probe is run for a node that declares none
