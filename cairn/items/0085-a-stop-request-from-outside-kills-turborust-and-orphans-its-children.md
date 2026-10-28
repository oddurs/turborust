---
id: 85
title: A stop request from outside kills turborust and orphans its children
type: bug
status: done
milestone: v1.4
created: 2026-10-28
updated: 2026-10-28
priority: p0
effort: s
area: engine
---

## Problem

Only one way of stopping turborust was graceful: ctrl-c on `up`. Everything
else killed the process outright, skipping the shutdown that stops each child:

| signal  | sent by                                       | `up`     | `run`    |
|---------|-----------------------------------------------|----------|----------|
| SIGINT  | ctrl-c                                        | graceful | abrupt   |
| SIGTERM | `docker stop`, systemd, CI cancellation, kill | abrupt   | abrupt   |
| SIGHUP  | terminal closed, ssh dropped                  | abrupt   | abrupt   |

A child usually dies anyway, of the pty hangup when turborust's end of the
terminal closes. A child that ignores SIGHUP — daemons, and dev servers that
detach from their terminal — does not. Measured with `trap '' HUP; exec sleep …` as
both a service and a task: orphaned in every case except ctrl-c on `up`, the one
graceful path, and still holding whatever it held, so the next `up` reports the port
as taken by "unknown process".

And once tokio owned SIGINT, a second ctrl-c during a slow shutdown did nothing
at all. There was no way to force the issue short of `kill -9`, which is the one
signal guaranteed to orphan.

## Proposal

Do what process supervisors do. watchexec, docker compose, overmind and
tini/dumb-init all treat SIGINT, SIGTERM and SIGHUP as "shut down cleanly", and
all but tini treat a second signal as "now".

- One place that installs handlers for all three at startup (ctrl-c,
  ctrl-break and console close on Windows), used by `up` — with and without the
  dashboard — and by `run`.
- The first request runs the normal shutdown: reverse dependency order, each
  node's `stop_timeout`, SIGTERM then SIGKILL to the process group.
- A second request kills every child's process group immediately and exits.
- `up` exits 0 after a requested stop, as `docker compose` does. `run` exits
  128 + the signal number, so an interrupted run is never mistaken for a
  passing one.

SIGKILL to turborust itself stays out of reach, as it is for every supervisor
including tini. `PR_SET_PDEATHSIG` looks like the answer and is not: it fires
when the *thread* that spawned the child exits, and tokio retires worker
threads, so it would kill healthy children at random.

## Acceptance criteria

- [x] SIGTERM, SIGHUP and SIGINT each stop `up --no-tui`, the dashboard, and `run` through the normal shutdown
- [x] A child that ignores SIGHUP is gone afterwards, in every case
- [x] A second request during shutdown kills everything and exits at once
- [x] An interrupted `run` exits 128 + the signal number
- [x] Tests drive the real binary with real signals

## 2026-10-28

Fixed as proposed. `signals::Signals` installs SIGINT, SIGTERM and SIGHUP together (ctrl-c, ctrl-break and console close on Windows, compile-checked against `x86_64-pc-windows-msvc` but not run), and `signals::watch` listens for the life of the process: the first request goes to whoever is waiting, and any request once a shutdown has begun kills every child's process tree and exits. "Begun" is set by a drop guard, so quitting the dashboard with `q` and a `run` finishing count too. Without that, a ctrl-c during those shutdowns reached nobody — tokio never uninstalls a handler, so the default action does not come back.

Two more bugs surfaced, both pre-existing, both found because the first fix was not enough to make the tests pass:

- **Nothing could reach a task mid-run.** Only services published their child handle. A task's supervisor was blocked waiting on its child, not reading its mail, so shutdown sent `Stop`, waited out `stop_timeout` + 2s, gave up, and exited with the task still running — under `up` as well as `run`, so a ctrl-c during a long `cargo build` task left cargo behind. Tasks now publish their child for as long as it runs, and shutdown stops a running task's tree directly. A task ended this way reads `stopped`, not `failed`, so a ctrl-c does not raise a failure notification. Side effect, and a welcome one: a running task can now be attached to from the dashboard and with `connect`.
- **Every stop path unpublished the child before stopping it.** So during the grace period the dying child was invisible, and a forced stop could not find it. All four paths now unpublish after.

Also: when the dashboard could not start, `up` returned the error before shutting down supervisors it had already started. It now shuts down first.

Measured end to end before and after, against the built binary, with `trap '' HUP; exec sleep …` as service and task, across SIGINT, SIGTERM, SIGHUP and SIGKILL to `up --no-tui` and `run`: orphaned in every catchable case except ctrl-c on `up` before; clean in every catchable case after. The dashboard was checked separately under `script(1)`: it exits on SIGTERM and SIGHUP, the child is gone, and the leave-alt-screen sequence is written. SIGKILL still orphans a child that ignores SIGHUP, as it does under every supervisor; documented, with systemd's `KillMode=control-group` as the answer.

Tests drive the real binary with real signals. Each piece was removed in turn and is caught: the shutdown not stopping a running task, a task child not being published, the second request not killing, and forget-before-stop — which the first version of the force test missed, because its service had no readiness probe and so never took the still-starting path. It now runs both.

One unexplained failure to record: `one_edit_restarts_each_node_exactly_once` timed out once in a full `cargo test` on this branch, then passed in 12 further full and supervisor-suite runs here and 8 on main. Not reproduced, so not attributed; noted in case it recurs.
