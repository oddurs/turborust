---
id: 72
title: Sibling worktrees each start with a cold cache
type: bug
status: done
milestone: v1.3
assignee: Oddur Sigurdsson
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

- [x] Determine whether identical work in two worktrees produces identical keys
      today — they do, so there was no path-dependence to fix
- [x] The default store is shared across worktrees of one repository
- [x] `[cache] dir` overrides it, and an existing `.turborust/` cache is migrated
      rather than orphaned
- [x] Two worktrees at the same commit: the second `run` is a hit
- [x] Concurrent writes from two worktrees do not corrupt a record — per-process
      temp names for records, staged-and-swapped directories for archives

## 2026-09-10

Measured first, as the item required. Keys already match across worktrees — same task, same commit, key b3:791bd455db0e in both the primary checkout and a sibling worktree. So there was no path-dependence to fix; target_dir keys as the enum ('shared'/'split'), cwd is stripped to workspace-relative, and the global hash carries no paths. The only problem was where the store lives.

Cached results now live in a per-user store keyed by 'git rev-parse --git-common-dir', which resolves to the same place from every worktree of a repository and is exactly the identity wanted. Outside git the workspace path is the only identity available, which reproduces the old behaviour. .turborust/ keeps what is genuinely per-checkout: summaries and scratch.

Verified end to end: a sibling worktree now reports 'cache hit ... 1 output(s) restored' where it previously paid a full cold build.

Two processes can now be writing one key at once, so records go to a per-process temp name before the rename, and archives are staged and swapped rather than written in place. A half-written archive under the real name would pass restore's existence check and replay truncated files, which is the failure this had to be closed against before sharing became the default.

Caught by the 0069 tests: staging broke a task that declares outputs and legitimately produces none — the staging directory was never created, so the rename failed and archiving errored. Fixed by creating it up front. Worth recording because the failing test was about dependent invalidation and had nothing on its face to do with archiving.

'clean' now empties the store for every worktree of the repository. It says so rather than printing 'cache cleared'.

## 2026-09-10

The pre-push hook caught a bug the shell could not. My test helper shelled out to git directly, and git exports GIT_DIR into every hook it runs — so the helper operated on the turborust repository instead of the temp one and 'git commit' failed. Passed from a terminal, failed from a hook.

affected.rs already had git_command() for exactly this, with a fuller variable list than the one I had written inline. repository_identity and the test helper now both use it, so there is one definition rather than two that can drift. This is the same trap recorded on the --affected work.
