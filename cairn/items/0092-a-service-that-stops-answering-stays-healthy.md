---
id: 92
title: A service that stops answering stays healthy
type: feature
status: done
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

- [x] A failing liveness probe marks a running node `unhealthy` after `failures` misses, not one
- [x] Dependents see it go down, and come back with it
- [x] With `restart = "always"` the node is restarted; otherwise it recovers by itself when the probe passes
- [x] No probe is run for a node that declares none

## 2026-10-28

`liveness = { tcp | http | cmd, every = "10s", failures = 3, timeout = "5s" }`,
checked on a `tokio::time::interval` inside the supervisor's running loop, so a
check never runs concurrently with a restart of the same node. Only `failures`
misses in a row count; one answer resets the count.

Without `restart = "always"` an outage is a state, not an action: the node goes
`unhealthy`, out of ready, and its dependents wait, exactly as for a stop; the
next answer brings it back. With it, the node is restarted with the reason
"not answering (N checks in a row): …". Liveness only restores readiness it
took away itself (`took_down`), so a failed self-reload stays failed until the
process says otherwise.

Why an interval and not a `sleep(every)` in the select: the running loop wakes
for every other branch too, and a fresh sleep would restart each time, so a
node whose loop is kept busy (a `reload = "self"` node polling its output) would
never be checked. `liveness_still_runs_beside_a_self_reload_poll` fails with the
sleep version. Liveness also pauses while such a node is `reloading`, since
a rebuild is not an outage.

Mutation-checked: never enabled fails 4 tests; acting on the first miss fails
2; the fresh-sleep version fails the beside-reload test; probing a node that
declares no liveness fails `a_node_without_liveness_is_never_probed_once_ready`.

## 2026-10-29

`a_service_that_stops_answering_goes_down_and_comes_back` failed once on macOS
CI (on `#33`, after this merged): removing `alive` found no such file. A race in
the test: with no readiness probe a service is healthy the moment it spawns, so
"both healthy" could hold before the shell had run `touch alive`. Reproduced by
delaying the touch 0.3s; the test now declares `health = { cmd = "test -f
alive" }`, which makes healthy mean the file exists. Fixed on `0097`'s branch,
the next to land. The two sibling tests were already safe: one writes the file
itself, the other announces readiness after creating it.
