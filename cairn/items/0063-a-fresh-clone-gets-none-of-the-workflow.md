---
id: 63
title: A fresh clone gets none of the workflow
type: bug
status: done
milestone: v1.1
assignee: Oddur Sigurdsson
created: 2026-09-08
updated: 2026-10-28
priority: p1
effort: m
area: project
---

## Problem

`core.hooksPath` and `commit.template` are local git config, not tracked content.
So a fresh clone gets:

```
core.hooksPath: UNSET — hooks do not run
commit.template: UNSET
```

Every guarantee in `docs/git-workflow.md` — attribution rejected, conventional
subjects, formatting, clippy, tests — is enforced only on the one machine that
happened to run `git config` by hand. Anyone else gets the documentation and none
of the enforcement, which is the failure mode the hooks were written to avoid.

Related: the repository does not follow its own `turborust doctor` advice, and
several files a contributor expects are missing (`rust-toolchain.toml`,
`.editorconfig`, `CONTRIBUTING.md`).

## Proposal

One idempotent `scripts/setup` that installs the hooks, sets the template,
reports the toolchain, and says what it did. `scripts/agent` checks for the hooks
and points at it rather than silently working without them.

## Acceptance criteria

- [x] `scripts/setup` installs hooks and the commit template, and is safe to rerun
- [x] `scripts/agent` warns when the hooks are not installed
- [x] A fresh clone is one documented command away from the full workflow
- [x] The repository takes its own doctor advice where that advice is portable

## 2026-10-28

Closed after verifying on a genuinely fresh clone: `scripts/setup --check` reported both missing config entries and exited 1, `scripts/setup` then installed them, and `rust-toolchain.toml`, `.editorconfig` and `CONTRIBUTING.md` are all present. The work shipped in 80b363c; the item stayed in `doing` because that commit carried no `Refs` trailer, so nothing tied the merge back to it. This was the last open item in v1.1.
