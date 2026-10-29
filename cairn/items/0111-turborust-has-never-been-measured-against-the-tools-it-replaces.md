---
id: 111
title: Turborust has never been measured against the tools it replaces
type: chore
status: done
created: 2026-10-29
updated: 2026-10-29
milestone: later
priority: p1
effort: l
area: engine
---

## Problem

The README compares turborust with overmind, foreman, process-compose and
watchexec on features. Nothing compares them on what a developer feels: how
long a stack takes to come up and go down, what an idle `up` costs, how much
output it can carry, how fast an edit becomes a restart, and what is left
running when it is killed.

## Acceptance criteria

- [x] A repeatable harness runs the same workload under turborust and the adjacent tools
- [x] Every gap the harness finds is either closed or written down with a reason
- [x] The results, and how to reproduce them, are in the repository

## Notes

`bench/compare.py` runs nine scenarios against process-compose, overmind,
hivemind, honcho and foreman (supervision) and watchexec and nodemon
(watching), each in its own config format, headless, output to `/dev/null`.
Processes are found by a token in their environment, so a tool's helpers count
as its overhead and anything left alive after a stop counts as leaked,
whoever started it. Results and how to reproduce them are in
`docs/benchmarks.md`; the raw run is `bench/results.jsonl`.

What the first run found, and what became of it:

- Startup paid for `rustc -V` through rustup's proxy (~125ms) and
  `git rev-parse` (~25ms) before anything started. The toolchain is now
  probed on a thread and only a cache key waits on it; the repository is read
  from `.git` (checked against git for a root, a subdirectory, a worktree and
  a non-repository). 74ms → 10ms for one service.
- Idle CPU grew with the number of children (10.6% for fifty): a wait thread
  per child polled every 20ms, `stop` polled every 25ms, `run`'s two waits
  polled every 20ms, and notify's debouncer ticked every 30ms forever. All now
  wait on notifications: a pidfd registered with tokio on Linux (no thread at
  all), `waitid(WNOWAIT)` on a thread on macOS, the board for `run`, and a
  debouncer that blocks on the event channel. 0.0% at every size. Windows
  still polls; nothing here measured it.
- Stopping fifty services took 1.3s because shutdown stopped one node at a
  time and each stop polled. `Plan::stop_waves` groups nodes so each wave
  stops at once and no node stops before its dependents. 16ms.
- Memory was unbounded three ways, found by the long-line and flood scenarios:
  lines had no length limit, the log store bounded lines but not bytes
  (20,000 lines × anything), and a node's diagnostics list grew for as long as
  it printed `error:` lines, since only a new build cleared it. A service
  logging an error nonstop grew turborust by 531MB in 20s. Lines are now cut at
  64KiB on a character boundary, the store holds at most 16MiB, and at most 256
  diagnostics are kept while every error is still counted. Flat at 8MB.
- Output: the pump took the state lock per line, `println!` made a system call
  per line, and each line was allocated several times over. Batching,
  a buffered writer synced once per batch, a shared process name and a
  borrowed strip for plain text took a 100,000-line profile from 511M to 257M
  instructions, and throughput from about 1.1M to 1.7M lines/s, ahead of
  every tool measured.
- Edit to restart was 161ms: the 120ms default window plus the debouncer's
  tick. The tick is gone, and the default is 50ms, watchexec's. 61ms, level
  with watchexec. A `coalesce` scenario checks that the shorter window still
  takes an atomic save, a 200-file checkout and a formatter writing twenty
  files 5ms apart as one change each; turborust did all three, watchexec and
  nodemon restarted two or three times for the formatter.

Three bugs surfaced on the way, each fixed with a test:

- A service's `debounce` was parsed, put in the schema, and never used. The
  shared watcher now waits for the shortest `debounce` among the planned
  services.
- Without a health check, a service's restart backoff only ever grew: the
  crash counter reset after a long run, the delay did not, so a service that
  crashed once a day waited longer each time up to `backoff_max`. Found while
  reading why the churn scenario recovered slowly. The slow recovery itself is
  policy (doubling to 30s) and stays.
- `stop` returned once the process group's leader had exited, not the group.
  The 25ms poll it used to run on hid that; with exits notified, a soak of the
  whole suite failed `a_task_over_its_timeout_is_stopped_tree_and_all` 6 times
  in 100 runs, because `sh -c 'sleep 60 & …; wait'` dies of the SIGTERM before
  the `sleep` it started has. `stop` now waits, within the same grace, until
  nothing in the group is running (zombies aside, so a container whose PID 1
  never reaps cannot stall it), then kills what is left. The old test went from
  2 failures in 240 runs to 0 in 480; a new one, whose child traps the SIGTERM
  and the hangup and takes 300ms, fails every time without the change.

Written down rather than closed: hivemind still starts ten and fifty services
faster (19ms and 94ms against 38ms and 177ms), because Go spawns with a
`vfork`-style clone while portable_pty forks; 0112. Spawning got cheaper here
by removing the wait thread per child, since every thread makes the next fork
slower (measured: 100 idle threads doubled the cost of a spawn). A child that
ignores SIGTERM takes turborust its full `stop_timeout` to stop, where hivemind
and overmind send SIGINT; that is which signal, not how fast.

CI then found two more, both events that notify's debouncer had been hiding:

- On Windows, the first read of a file after a while updates its last-access
  time, which arrives as a modification. The old debouncer opened every file
  to learn its ID when the watch began, which used up that update for every
  file present at startup; without it, `reading_a_nested_tree_is_not_a_change`
  failed. Real for files created later too. On Windows a modification now
  counts only if the file's mtime moved, and a directory's never does (its
  entries' own events say what changed).
- The polling backend never went through the debouncer: a scan between a
  save's truncate and its write, and the next scan, were two restarts
  (`with_polling_an_edit_restarts_its_service_once` on macOS). Polled changes
  now go through the same debouncer, waiting at least a scan and a half.
  `polling_takes_a_save_seen_by_two_scans_as_one_change` fails without it.

After all of it, a durability soak of the whole suite (4 workers × 25 rounds,
900 test-binary runs) had no failures.
