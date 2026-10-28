---
id: 73
title: A node that reloads itself is invisible to the supervision tree
type: feature
status: doing
milestone: v1.4
claimed: 2026-10-28
created: 2026-09-10
updated: 2026-10-28
priority: p0
effort: l
area: engine
---

## Problem

Rust frontends now reload themselves. `dx serve` uses `subsecond` to link thin
patches into a running process behind a jump table, keeping application state
across an edit; `cargo leptos watch` and `trunk serve` have always run their own
watchers. `0023` already concluded that turborust should not implement
hot patching. It said nothing about *supervising* a process that does.

Today the only available answer is `on_change = "ignore"`, which stops the
restart and stops everything else too:

- Readiness is start-based. A node that never restarts is never re-probed, so it
  reads healthy throughout a rebuild — including one that fails.
- The cascade never fires. Anything with `depends_on = ["web"]` is told nothing,
  because as far as the tree is concerned nothing happened.
- The TUI and the overlay show a steady green node while a build is in flight.
  The Issues panel stays empty while `rustc` is printing errors into the log.

So the supervision tree, whose entire job is knowing what is ready, is blind to
the one class of process that is increasingly how Rust frontends work. That is a
hole in the "full-Rust stack" claim on the front page, which is why this carries
p0 rather than being a nice new capability.

## What should happen

Treat "this node reloads itself" as a supervision mode, and give it the
reload-time analogue of a health probe:

```toml
[services.web]
cmd = "dx serve"
reload = "self"                      # turborust never restarts this node
health    = { log = "server running" }   # ready again
reloading = { log = "rebuilding" }       # left ready, without a restart
```

When `reloading` matches, the node transitions ready → building **in place**:
the cascade runs exactly as it would for a real restart, dependents are told,
the TUI and overlay show a build in flight, and diagnostics parsed out of the
node's output populate Issues the way they already do. When `health` matches
again the node is ready and dependents come back through the normal readiness
cascade.

The result is that turborust supervises a hot-patching process correctly —
without owning any of the patching — which nothing else does.

## Acceptance criteria

- [x] `reload = "self"` suppresses restart-on-change while keeping the node
      under supervision
- [x] A `reloading` probe moves a node out of ready without a restart, and the
      dependency cascade fires
- [x] Failing to return to ready within `ready_timeout` is reported as a failed
      reload, not as a healthy node
- [x] Diagnostics from a self-reloading node reach the overlay's Issues panel
- [ ] A worked `dx serve` example in the README, verified against a real project
- [x] `plan` marks self-reloading nodes, since their edges behave differently

## 2026-10-28

Built as specified, with the shape taken from the two closest precedents rather than invented. Tilt's `live_update` keeps "updating" distinct from "running", so a sync shows as in flight without a restart: that is `Status::Reloading`. Docker's health states (`starting` → `healthy` → `unhealthy`, where an unhealthy container is still running) gave `Status::Unhealthy` its name and its meaning. The overlay and TUI read both; `unhealthy` counts as a failure everywhere `failed` does, including the desktop notifier.

Three decisions worth knowing before changing this:

- **Counters, not flags.** The pump counts ready and reload matches, and records the ready count at each reload. A rebuild that starts and finishes between two polls is still seen, so dependents still re-run; two booleans could only say both lines happened, not in which order. Same reasoning as `generation` in 0004.
- **Diagnostics reset in the pump, at the moment the reload line matches,** not when the supervisor next looks. `set(Starting)` resets them, which is why there is a separate state: errors printed between "rebuilding" and the next poll would otherwise be wiped before anyone saw them.
- **No browser reload on a self-reload.** A normal service becoming ready sends `RELOAD_FULL`. Doing that here would discard exactly the state `subsecond` exists to keep.

Two things beyond the spec, both because the spec's version was worse in practice:

- A rustc error during a reload makes the node unhealthy immediately. The spec only had `ready_timeout`, which meant a failing rebuild read as "reloading" for up to 60s after rustc had already said how it ended.
- An unhealthy self-reloading node comes good on its own when the ready line next appears. These processes recover by themselves on the next edit that builds, and a first `dx` build slower than `ready_timeout` is ordinary; staying unhealthy until a manual restart would be wrong for both.

A second bug surfaced, pre-existing and not specific to this item: when a startup probe gave up, the node stayed `Starting` forever. In `up` that was a cosmetic lie. In a one-shot `run` it was a hang: `await_tasks` only treated `Failed` and `Backoff` services as dead, so a task depending on a service whose probe timed out waited with no end. Reproduced on main before fixing (`a_service_that_never_becomes_ready_fails_the_run_instead_of_hanging` hit its 10s timeout), now `Unhealthy`, which `run` treats as dead. Not fixed: an ordinary service that goes unhealthy at startup is never probed again. That is unchanged behaviour, now at least reported truthfully.

A third bug was found in review of this change, before it shipped: a rebuild announced in the window between the ready line and the supervisor noticing it (up to one probe interval) was counted as already seen, so the node read healthy for that whole rebuild. Fixed by treating "no ready line since the latest reload" as a reload in flight; pinned by `a_rebuild_announced_while_startup_is_being_confirmed_is_not_missed`, which failed before the fix.

Tests: four plan-validation cases (`reloading` without `reload = "self"`, an `on_change` that would be ignored, `reloading` without a log probe to end it, and the happy path), seven supervisor cases against a shell stand-in for `dx serve`, and the `run` hang. Every new mechanism was removed in turn and the suite re-run; each removal is caught by the test written for it. Stable across repeated runs and under three concurrent copies of the suite.

Open: the README example is written but not verified against a real `dx serve`, `cargo leptos watch` or `trunk serve` — none were available. It tells the reader to copy the ready and rebuild lines from their own tool's output rather than quoting strings this change has not seen. That criterion stays unticked until someone runs it.
