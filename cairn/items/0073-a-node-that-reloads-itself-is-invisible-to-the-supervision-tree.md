---
id: 73
title: A node that reloads itself is invisible to the supervision tree
type: feature
status: backlog
milestone: v1.4
created: 2026-09-10
updated: 2026-09-10
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

- [ ] `reload = "self"` suppresses restart-on-change while keeping the node
      under supervision
- [ ] A `reloading` probe moves a node out of ready without a restart, and the
      dependency cascade fires
- [ ] Failing to return to ready within `ready_timeout` is reported as a failed
      reload, not as a healthy node
- [ ] Diagnostics from a self-reloading node reach the overlay's Issues panel
- [ ] A worked `dx serve` example in the README, verified against a real project
- [ ] `plan` marks self-reloading nodes, since their edges behave differently
