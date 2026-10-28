---
id: 101
title: An edit right after the watch starts can be missed
type: bug
status: backlog
created: 2026-10-28
updated: 2026-10-28
priority: p2
effort: s
area: watch
---

## Problem

In 100 isolated runs of the supervisor suite after `0088`,
`one_edit_restarts_each_node_exactly_once` failed once: the edit to `src/a.rs`
produced no restart and no log line at all. The harness waits a fixed 350ms
after `watch::watch` returns before touching the tree, so either the watch is
not armed when `watch` returns, or the one event was dropped between notify and
dispatch.

Either is a product question, not only a test one: a user who saves a file the
moment `turborust dev` prints its first line expects it to count.

## Acceptance criteria

- [ ] Establish whether `watch::watch` returns before the OS watch is armed
- [ ] If so, `watch` returns only once it is, and the harness's fixed sleep goes
- [ ] 200 isolated suite runs with no failure of this test
