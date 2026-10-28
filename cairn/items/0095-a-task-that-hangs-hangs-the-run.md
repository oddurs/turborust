---
id: 95
title: A task that hangs hangs the run
type: feature
status: backlog
created: 2026-10-28
updated: 2026-10-28
priority: p2
effort: s
area: engine
---

## Problem

A task has no time limit. A test that deadlocks or a build waiting on a prompt
holds `turborust run` until something outside kills it — in CI, the job's own
timeout, which reports a cancelled pipeline rather than which task hung.

## Precedent

mise `timeout` per task, Bazel's test timeouts, GitHub Actions
`timeout-minutes`, cargo-nextest's slow-test termination.

## Proposal

`timeout = "10m"` per task and a project default. On expiry the task's process
tree is stopped as a shutdown would stop it (`0085`), the task fails with a
distinct exit code and a message naming the limit, and with `--continue` the
rest of the graph carries on. Durations are bounded as `0086` bounds them.

## Acceptance criteria

- [ ] A task over its `timeout` is stopped, tree and all, and fails saying why
- [ ] The run's exit code distinguishes a timeout from a failure
- [ ] `--continue` keeps unrelated work going after a timeout
