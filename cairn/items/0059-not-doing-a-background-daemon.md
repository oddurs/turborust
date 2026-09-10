---
id: 59
title: 'Not doing: a background daemon'
type: feature
status: dropped
milestone: later
created: 2026-09-08
updated: 2026-09-08
area: engine
---

## Considered and declined

turborepo and moon both run a daemon to keep file watchers warm between
invocations, so a short `run` does not pay to set up and tear down a watch.

## Why not

That cost only exists for tools whose primary mode is many short invocations.
turborust's primary mode is `up`, which is long-lived and holds its watcher for
its whole life — there is nothing to amortise.

A daemon would also add the failure modes daemons have: stale state, version skew
between client and daemon, an orphan holding a watch after an upgrade, and a
`turborust daemon stop` in everyone's troubleshooting notes.

The control socket in 0041 is deliberately *not* this: it lives and dies with a
running `up`, and nothing starts it implicitly.
