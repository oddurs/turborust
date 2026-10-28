---
id: 84
title: The tracker drifts from the work, and nothing notices
type: chore
status: done
created: 2026-10-28
updated: 2026-10-28
priority: p1
effort: s
area: project
---

## Problem

Closing out v0.1 through v1.3 found the tracker out of step with the code in
three ways, none of which any check reported:

- Four milestones (`v0.1`, `v1.0`, `v1.2`, `v1.3`) still `backlog` although
  every item scheduled in them was done. Only `v0.0` had ever been closed.
- `0063` still `doing` seven weeks after its work merged in 80b363c, holding
  `v1.1` at 80%.
- `0004` `done` with all four acceptance criteria unticked, so finished work
  read as unverified.

All three come from one cause. The `Refs:` trailer links a commit to its item
automatically, but closing the item is a manual step *after* the merge, and a
step after the merge belongs to nobody. `cairn check` validates that items are
well formed, not that they match the work. The one CI job that could have noticed
drift depends on installing cairn, and skips itself when it cannot.

## Proposal

Close in the branch, and audit what slips through.

- `scripts/audit` reads the item files and git history directly, with no cairn,
  and reports a milestone whose items are all finished, a done item with unticked
  criteria (errors), and an open item that a commit on the default branch refs or
  a claim untouched for 30 days (warnings).
- It runs in `scripts/agent check` and as a CI step that does not depend on cairn.
- `scripts/agent pr` warns when the item the branch refs is still open, so the
  close rides in the PR.

## Acceptance criteria

- [x] Run against `main` as it stood before the close-out, the audit reports every drift found by hand, and nothing else
- [x] It passes on the closed-out tracker
- [x] The `Refs:` warning fires for an open item a landed commit references
- [x] `agent check` and CI run it, and CI runs it even when cairn cannot install
- [x] `docs/git-workflow.md` says to close the item in the branch that finishes it

## 2026-10-28

Filed by hand rather than with `cairn new`, because cairn could not be installed in the environment that did this work.

Against the pre-close-out `main` the audit reported exactly the hand-found set: four open milestones and `0004` as errors, `0063` as a stale-claim warning. It did not flag `0063` through the `Refs:` check, because 80b363c carried no trailer. That is the honest limit: the trailer check only sees work done through `agent commit`, which is why the stale-claim check exists alongside it rather than being redundant with it.

Warnings stay warnings on purpose. An item can legitimately span several PRs, so a landed `Refs:` on an open item is not proof of drift; `--strict` is there for anyone who wants it.

`0064` (the site) was also found open after its work landed, but nothing here would have caught it: it was an empty stub with no milestone and no trailer. Unscheduled stubs filed by `agent new` and never filled in are a gap this audit does not cover.
