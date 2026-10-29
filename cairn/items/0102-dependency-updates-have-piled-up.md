---
id: 102
title: Dependency updates have piled up
type: chore
status: done
created: 2026-10-29
updated: 2026-10-29
priority: p2
effort: s
area: deps
---

## Problem

Three Dependabot PRs sat open, two of them since early September: `#4`
(`actions/checkout` 4 → 7), `#5` (`windows-sys` 0.60 → 0.61) and `#27` (ten
minor and patch updates). Each was weeks behind `main`, Dependabot had stopped
rebasing them, and `#4` conflicted with the workflow changes since.

## Acceptance criteria

- [x] All three updates are on `main`, in one change that passes CI on every platform
- [x] The Windows-only code is checked against the new `windows-sys`

## 2026-10-29

Applied here rather than by merging the bot branches: rebasing three stale
branches through conflicts is three CI rounds for one result, and their commits
carry no `Refs:` trailer. Dependabot closes a PR by itself once `main` has the
version it proposed.

`cargo update -p` for the ten crates in `#27` also took their transitive patch
releases, so a few land newer than the PR named (`toml` 1.1.8 rather than
1.1.6). `windows-sys` 0.61 is now turborust's own; the job-object code
type-checks against it (`cargo check --target x86_64-pc-windows-msvc
--features portable-hash`, pure-Rust blake3 because there is no MSVC C compiler
here), and the Windows CI job runs it. `actions/checkout@v7` replaces `@v4` in
all twelve uses across `ci.yml` and `release.yml`; its breaking change concerns
`pull_request_target` and `workflow_run`, which neither workflow uses.
