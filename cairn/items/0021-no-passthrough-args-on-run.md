---
id: 21
title: No passthrough args on run
type: feature
status: done
milestone: v1.0
assignee: Oddur Sigurdsson
created: 2026-09-08
updated: 2026-09-08
priority: p3
effort: s
area: cli
---

## Problem

`turborust run test -- --nocapture` is not supported. Anything beyond the
configured `cmd` means editing the config.

## Proposal

Accept args after `--` and append them to the target task's command. They must
also participate in the cache key — turborepo carries `pass_through_args` in its
`TaskHashable` for exactly this reason, and omitting them would mean
`run test -- --ignored` served from the cache of a run that did something else.

## Acceptance criteria

- [x] `turborust run <task> -- <args>` appends to that task's command
- [x] Passthrough args participate in the cache key
- [x] Args reach only the named task, not its dependencies

## 2026-09-08

turborust run <task> -- <args> appends to that task command and keys it.

Keying was the part worth getting right: run test -- --ignored served from the cache of run test would be a wrong answer, so the passthrough is folded into the cmd meta rather than bolted on at spawn time. Verified: same args hit, different args miss.

Args reach only the named task. A dependency has no idea what --nocapture means and passing it down would break the build it was meant to help debug — confirmed in the same run, where the dependency took its own cache hit unaffected.

Stored on the Engine rather than threaded through run_task, for the same reason --force is: task execution is driven by supervisors, which have no argument to pass.
