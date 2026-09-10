---
id: 25
title: Supervision tree with a readiness cascade
type: feature
status: done
milestone: v0.0
created: 2026-09-08
updated: 2026-09-08
area: engine
effort: l
---

One tokio task per node, joined by `watch` channels carrying readiness. A node
starts only once every dependency reports ready, and stops when any dependency
stops reporting ready.

That single rule buys ordered startup, ordered shutdown and cascading restarts
without any of them being written separately. `depends_on` therefore means
readiness, not "a process exists" — which is the weakness that made the same field
decorative in the tools this was measured against.

Restart policies (`always`, `on-change`, `never`), exponential backoff bounded by
`backoff_max`, and port reclamation before rebinding all hang off the same loop.
