---
id: 90
title: Survey local process managers, watchers and task runners
type: chore
status: done
created: 2026-10-28
updated: 2026-10-28
priority: p2
effort: s
area: project
---

## Problem

`0066` surveyed monorepo orchestrators, the Rust dev loop, cluster dev tools and
agent tooling. It did not look at the tools turborust overlaps most directly on
a developer's machine.

## What was surveyed

- **Process managers:** process-compose, overmind, hivemind, foreman, honcho.
- **Watchers:** watchexec, nodemon, Vite and webpack's watch options.
- **Task runners:** mise tasks, just.
- **Container dev loops:** Docker Compose `watch`.

## What was filed

Gaps, unscheduled so they surface in the `triage` view:

- `0091` readiness cannot run a command (`pg_isready`) — p1
- `0092` a service that stops answering stays healthy (liveness) — p1
- `0093` no `.env` file support, which every Procfile runner has — p1
- `0094` no polling fallback for Docker, WSL2 and network mounts — p2
- `0095` a hung task hangs the run (task timeouts) — p2
- `0096` a crash loop never gives up (`max_restarts`) — p3
- `0097` `init` cannot start from a Procfile — p3
- `0098` a command cannot see which files changed — p3

Declined, with reasons: replicas and scaling (`0099`), sandboxed tasks (`0100`).
Docker Compose's file *sync* into containers is already covered by `0082`.
mise's `run_windows` is noted on `0062`, where a configurable shell is the
broader answer.

## What the survey found that was not a feature

Checking the watcher against watchexec's behaviour turned up two bugs, fixed
separately: on Linux a nested workspace restarted its services forever because
reads were treated as changes (`0088`), and an exhausted inotify limit stopped
rebuilds without a word (`0089`).

## Acceptance criteria

- [x] Process managers, watchers, task runners and container dev loops surveyed
- [x] Every gap checked against the code before being filed, not assumed
- [x] Declined ideas filed with reasons
