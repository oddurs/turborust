---
id: 95
title: A task that hangs hangs the run
type: feature
status: done
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

- [x] A task over its `timeout` is stopped, tree and all, and fails saying why
- [x] The run's exit code distinguishes a timeout from a failure
- [x] `--continue` keeps unrelated work going after a timeout

## 2026-10-29

`timeout = "10m"` per task, `[project] task_timeout` as the default; a task's
own wins. Parsed at plan time, bounded as `0086` bounds durations, and zero is
refused (it would fail every task before it started).

At the limit the task is stopped the way a shutdown stops it (`0085`): the
process group gets `SIGTERM`, then a kill after the grace period, so a test
binary under `cargo test` goes too, not just the shell. It fails with
"timed out after 10m; stopped" and code 124, which is what GNU `timeout(1)`
exits with, so CI scripts that already check for it read it right.

Found while writing it: telling a timeout by its code is wrong, since a command
can exit 124 itself. The first draft did, and such a task would never have been marked
failed, so `run` waited on it forever. `wait_limited` now returns a flag beside
the code; `a_command_that_exits_124_itself_is_an_ordinary_failure` is the cover.

`alive()` in the test reads `/proc/<pid>/stat` and treats a zombie as gone: an
orphaned grandchild is reaped by whatever PID 1 is, and in a container that may
be nothing.

Mutation-checked: not stopping the tree, deciding by the code, and ignoring the
limit each fail a test.
