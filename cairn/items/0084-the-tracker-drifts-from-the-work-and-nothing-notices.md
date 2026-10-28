---
id: 84
title: The tracker drifts from the work, and nothing notices
type: chore
status: doing
claimed: 2026-10-28
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

- `scripts/audit` reads the item files and git history directly, with no cairn.
  Severities follow established tools; see `docs/git-workflow.md` for the table
  and the precedent behind each row.
- It runs in `scripts/agent check` and in CI, where findings are annotations on
  the item file and, on a pull request, an item the branch leaves open is named.
- `scripts/agent pr` warns when the item the branch refs is still open, so the
  close rides in the PR.
- CI installs the real tracker (`cairn-md`, pinned) and no longer skips the
  roadmap check silently.

## Acceptance criteria

- [x] Run against `main` as it stood before the close-out, the audit reports every drift found by hand, and nothing else
- [x] It passes on the closed-out tracker
- [x] The `Refs:` warning fires for an open item a landed commit references
- [x] `agent check` and CI run it, and CI runs it even when cairn cannot install
- [x] `docs/git-workflow.md` says to close the item in the branch that finishes it
- [x] `tests/audit.rs` covers every finding, and fails against the first version
- [x] CI installs cairn from its real source, pinned, and fails if it cannot
- [ ] `ROADMAP.md` rendered by the pinned cairn, and the roadmap job green

## 2026-10-28

Filed by hand rather than with `cairn new`, because cairn could not be installed in the environment that did this work.

Against the pre-close-out `main` the audit reported exactly the hand-found set: four open milestones and `0004` as errors, `0063` as a stale-claim warning. It did not flag `0063` through the `Refs:` check, because 80b363c carried no trailer. That is the honest limit: the trailer check only sees work done through `agent commit`, which is why the stale-claim check exists alongside it rather than being redundant with it.

Warnings stay warnings on purpose. An item can legitimately span several PRs, so a landed `Refs:` on an open item is not proof of drift; `--strict` is there for anyone who wants it.

`0064` (the site) was also found open after its work landed, but nothing here would have caught it: it was an empty stub with no milestone and no trailer. Unscheduled stubs filed by `agent new` and never filled in are a gap this audit does not cover.

## 2026-10-28

Benchmarked the first version against established tools, and three of its calls were wrong.

- **Unticked criteria were an error.** cairn's own `check` (v0.3.0, `require_criteria`) makes this a warning, and says why: a tool that refuses teaches people to write criteria they can tick rather than criteria that are true. Downgraded. The opposite precedent, the GitHub actions that block a merge on an unchecked task list, is about a PR in progress, not a record of finished work.
- **A finished milestone left open was an error.** GitLab offers to close such a milestone; nothing insists. The real contradiction is the reverse, a *closed* milestone with open work, which is what Jira refuses when releasing a version. That is now the one error, and nothing in this tracker has ever been in that state.
- **Staleness was measured from `updated`.** cairn measures from `claimed` (`held_days`) and calls stale *visible, never revoked*. Matched, including the key name `claim_stale_after`, so the two agree once the project migrates to a format that has it.

Gaps the comparison exposed, now closed: empty sections on an open item (as issue forms require fields), which is the `0064` stub the first version could not see; findings as `::warning file=` annotations, the mechanism clippy, eslint and shellcheck actions use; a pull-request notice for an item the branch leaves open, as changeset-bot names a missing changeset; and tests, which the first version had none of. Against the first version, 10 of the 13 tests fail; against the pre-close-out `main` the new version reports all seven drifts found by hand.

The bigger finding was in CI. `cargo install cairn` resolves to an unrelated crate on crates.io ("build-gated version control", v0.0.0) that does not compile, and the step was `continue-on-error`, so the roadmap-in-sync check has been skipped on every run. The tracker's package is `cairn-md`, not yet on crates.io. CI now installs it from `oddurs/cairn` at tag v0.3.0 with `--locked`, and failure is failure. v0.3.0 reads format 2 without migrating: only commands that take the write lock refuse an older format, and `check` and `render` do not. Whether v0.3.0's render is byte-identical to the build that wrote the current `ROADMAP.md` is not yet known; that is the open criterion.

Not adopted: `shellcheck`, the established linter for these scripts, because it could not be installed in the environment that did this work. Worth adding to the lint job.
