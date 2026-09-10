---
id: 49
title: Reverse dependency index on the plan
type: chore
status: done
milestone: v1.2
assignee: Oddur Sigurdsson
created: 2026-09-08
updated: 2026-09-08
priority: p1
effort: s
area: plan
---

## Problem

Three separate items want the same missing thing: "what depends on this node".

- `--filter ...web` (0044) needs dependents
- `--affected` (0045) expands changed nodes to their dependents
- `--continue` (0048) needs to know which tasks are blocked by a failure

`Plan::dispatch_roots` also walks `transitive_deps` once per hit today, which is
quadratic in the number of matched nodes. Small graphs hide it.

## Proposal

Build a forward and reverse adjacency map once in `plan::resolve`, and expose
`dependents_of` and `dependencies_of` alongside the existing `transitive_deps`.
Cycles are already rejected at config validation, so the walks need no cycle
guard beyond a visited set.

Filed separately because doing it inside whichever of the three lands first would
bury a shared change in an unrelated item.

## Acceptance criteria

- [x] `Plan` exposes transitive dependents and dependencies
- [x] `dispatch_roots` uses the index rather than re-walking
- [x] Behaviour is unchanged; the existing dispatch tests still pass

## 2026-09-08

Plan now builds a dependents map in resolve, and both directions go through one breadth-first `reach` helper parameterised by which edge list to follow. That collapsed transitive_deps and transitive_dependents into the same six lines rather than two near-identical walks.

No cycle guard beyond the visited set: cycles are rejected during config validation, so the graph that reaches resolve is acyclic. That is stated at the function rather than left as an assumption someone has to rediscover.

dispatch_roots keeps its semantics and loses a level of nesting. Its existing tests still pass unchanged, which was the point of doing this separately — a shared change buried inside 0044 would have been indistinguishable from that feature.

Four tests on a diamond, including that a node reachable by two paths appears once, and that an unknown node yields empty rather than panicking.
