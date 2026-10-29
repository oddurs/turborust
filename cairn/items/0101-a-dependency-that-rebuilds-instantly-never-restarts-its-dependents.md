---
id: 101
title: A dependency that rebuilds instantly never restarts its dependents
type: bug
status: done
created: 2026-10-28
updated: 2026-10-29
priority: p1
effort: s
area: engine
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

- [x] Establish whether `watch::watch` returns before the OS watch is armed
- [x] Find where the edit is lost, and fix that instead of the test
- [x] A deterministic test that fails without the fix
- [x] The original test, 1,200 runs under load, with no failure
- [x] 200 isolated suite runs with no failure of this test

## 2026-10-29

The title was wrong, and so were the second and third original criteria
("`watch` returns only once it is armed, and the harness's fixed sleep goes";
"200 isolated suite runs"): they assumed the watcher. It is not the watcher. On
Linux, notify's inotify `watch` waits for the event loop to confirm each
`add_watch`, so it returns armed. (The harness sleep stays: macOS FSEvents does
start asynchronously, which `0088` met.)

Tracing every hop — raw notify events, what the watcher sent, dispatch, the
task's rerun, the cache decision — and running the test six at a time until it
failed (10 in 1,200) showed the edit was never lost: the watcher sent it,
dispatch routed it to `check`, `check` reran. `api` simply never restarted.

Readiness travels on a `tokio::sync::watch<bool>`, which keeps only the latest
value. `check` reran in 5ms; when the monitor task that turns readiness edges
into `DepDown`/`DepUp` was not scheduled inside that window, it saw true then
true, and sent `DepUp`. A running node ignores `DepUp` on purpose — it is
normally the edge that started it, or the tail of a `DepDown` already acted on —
so the rebuild never reached it. Under load this is a race. A cache hit makes
the window tiny, because a hit reruns without yielding: on a single-threaded
runtime (the tests') it is lost every time, and under `up`'s multi-threaded one
only another worker picking the monitor up within microseconds saves it.
Reverting an edit is a hit, so reverting a change could leave the services that
depend on it running the code that was reverted.

The fix uses what the engine already keeps for this: each node's `generation`
bumps when it completes a run or becomes healthy, and each supervisor snapshots
its dependencies' generations before it spawns. A `DepUp` whose dependency's
generation has moved past that snapshot is a result this process predates, and
is treated as the down edge it hides. Echoes and stale `DepUp`s still find the
generation unchanged and are ignored, so cascades are not doubled. The same
check is applied while a node is still starting (`probe_until`), which had the
same blind spot; that path is covered by reasoning only, not by its own test.

`a_dependency_rebuilt_too_fast_to_see_go_down_still_restarts_its_dependents`
fails 3 out of 3 without the fix. The original test went from 10 failures in
1,200 runs (six in parallel) to 0 in 1,200.
