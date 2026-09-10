---
id: 72
title: Sibling worktrees each start with a cold cache
type: bug
status: backlog
milestone: v1.3
created: 2026-09-10
updated: 2026-09-10
priority: p2
effort: m
area: cache
---

## Problem

The cache lives in `.turborust/` inside the checkout. This repository's own
workflow is a worktree per branch — `scripts/agent start` creates one for every
piece of work — so every branch begins with an empty cache and pays a full cold
build for results it very often already has.

The results are content-addressed. Two worktrees of the same repository at the
same commit produce the same keys and the same artifacts. Recomputing them is
pure waste, and it is waste this project inflicts on itself every day.

Turborepo added worktree support in 2.8 for the same reason.

## What should happen

Default the store to a per-user location keyed by repository identity rather
than by checkout path, so sibling worktrees share it. `[cache] dir` keeps
overriding it, and `.turborust/` stays the place for per-checkout state that
genuinely is per-checkout.

There is a precedent in the codebase for the shape: the control socket already
lives in a per-user directory named by a hash of the workspace, because Unix
socket paths overflow when a workspace sits a few directories deep.

## Check first, then build

The sharing only pays if keys actually match across worktrees. The key folds in
`target_dir`, and if that resolves to an absolute path then two worktrees
already produce different keys and moving the store achieves nothing on its own.

So the first acceptance criterion is a measurement, not a change.

## Acceptance criteria

- [ ] Determine whether identical work in two worktrees produces identical keys
      today; if not, fix the path-dependence first and record what it was
- [ ] The default store is shared across worktrees of one repository
- [ ] `[cache] dir` overrides it, and an existing `.turborust/` cache is migrated
      rather than orphaned
- [ ] Two worktrees at the same commit: the second `run` is a hit
- [ ] Concurrent writes from two worktrees do not corrupt a record — this is the
      first time two turborust processes share a store by default
