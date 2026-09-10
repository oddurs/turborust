---
id: 44
title: Select what runs with --filter
type: feature
status: done
milestone: v1.2
assignee: Oddur Sigurdsson
depends_on:
- 49
created: 2026-09-08
updated: 2026-09-08
priority: p1
effort: m
area: cli
---

## Problem

`up` starts every service; `run` needs every task named explicitly. There is no
way to say "just the frontend and what it needs", which on a workspace of any size
is the normal thing to want.

## Design

Adopt turborepo's `...` idiom rather than inventing one — it is already learned,
and the semantics are unambiguous once you know the rule that dots point at the
part of the graph you are *including*.

```
--filter web         just web
--filter web...      web and everything it depends on
--filter ...web      web and everything that depends on it
--filter '...web...' both directions
--filter 'crates/*'  glob over node names
```

Repeatable, unioned. `--filter` without `...` on `up` is the common case: start
one service and its dependency closure, which is what `plan::topo_closure`
already computes.

The dependents direction needs a reverse index, which the plan does not build
today. It is cheap to compute once at resolve time and `dispatch_roots` would
benefit from it too — that currently walks `transitive_deps` per hit.

**Interaction with the cache, stated because it will be asked:** filtering is
*selection*, not invalidation. A filtered-in task still takes its cache hit. The
two are orthogonal and both apply.

## Acceptance criteria

- [x] Exact names, globs, and the four `...` forms
- [x] Repeatable and unioned
- [x] Works on both `up` and `run`
- [x] An unmatched filter is an error naming the available nodes, not a silent
      empty run
- [x] Filtered-in tasks still hit cache

## 2026-09-08

turborepo dot idiom, all four forms, plus globs, repeatable and unioned, on both up and run.

Expansion works from the config depends_on edges rather than a resolved Plan, so selecting nodes does not first require asking cargo about the workspace. That also meant not needing 0049 after all for this item — the reverse index there is on Plan, and building a tiny reverse map from config edges is cheaper than resolving twice.

An unmatched filter is an error listing the real node names. Silently selecting nothing would present as a successful empty run, which is the failure mode that wastes an afternoon in CI.

Positional targets and --filter union rather than intersect: `up api --filter ...check` is a reasonable thing to write and plainly means both.

Note on `name...`: it selects the node and its dependencies, which is what resolution does anyway, so it is a no-op in practice. Implemented for fidelity with the idiom rather than dropped, because a filter language where one documented form silently does nothing is worse than one that is merely redundant.

Verified: --filter build gives build+check, --filter ...check gives check+build, --filter docs gives docs alone, --filter "*c*" globs to check+docs.
