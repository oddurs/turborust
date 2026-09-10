---
id: 83
title: 'a test that shells out to git can reach the wrong repository'
type: chore
status: done
created: 2026-09-10
updated: 2026-09-10
priority: p1
effort: s
area: testing
---

## What happened

While writing the worktree test for `0072`, the test helper shelled out to `git`
directly instead of going through `affected::git_command`. Run from a terminal it
passed. Run from the pre-push hook it did real damage: git exports `GIT_DIR` into
every hook, the `git` child inherited it, and `git init -q .` — aimed at a
temporary directory but pointed by `GIT_DIR` at the actual turborust repository —
re-initialised that repository and set `core.bare = true`. The subsequent
`git worktree add` registered a worktree under `/var/folders/...` in it.

The working tree was intact and no commits were lost; `core.bare` was reset,
the stray worktree and its branch removed, and `git fsck --connectivity-only`
reported nothing but the expected dangling pre-squash commits.

`0072` fixed the cause by routing both `repository_identity` and the test through
`git_command`, which strips the variables git exports into hooks. This item is
the second layer.

## Why a second layer

The failure was silent, one-directional and expensive: a test wrote to a
repository it was never pointed at, and nothing in the test could have noticed.
Correct code was enough to fix it and is not enough to keep it fixed, because the
next person writing a git-touching test will reach for `Command::new("git")` —
it is the obvious thing to write, and it works when they run it.

## What was done

`assert_owns_its_repo` runs before anything that writes, and fails the test
unless `git rev-parse --git-common-dir` from that directory resolves to that
directory's own `.git`. A wrong repository now fails loudly instead of being
quietly modified.

The guard requires both paths to resolve rather than comparing two `Option`s:
a directory that is not a repository at all produced `None` on both sides and
compared equal, which is precisely the case it exists to catch.

## Acceptance criteria

- [x] Temp repositories are proved to be their own repository before being
      written to
- [x] The guard rejects a directory that is not a repository, pinned by a
      `#[should_panic]` test
- [x] Suite passes with `GIT_DIR` set, which is what a hook does
