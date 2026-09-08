---
id: 37
title: The pty reader started after the child, losing fast output
type: bug
status: done
milestone: v0.0
created: 2026-09-08
updated: 2026-09-08
area: proc
effort: s
---

## What happened

Surfaced as a test failing roughly 2 runs in 15. It was not the test.

The pty reader thread was spawned after the child, so a command that wrote and
exited immediately — a failing `cargo check`, an `echo` — could finish while
nothing was reading, and macOS discards the unread buffer when the last slave fd
closes.

Losing exactly the output of the fastest failures, in a tool whose purpose is to
show you failures.

## Fix

Clone the reader and start its thread before spawning the child; release the slave
only after the child has inherited it. 15 consecutive clean runs after.

The lesson worth keeping: the flake was the bug reporting itself honestly.
Deleting the assertion would have shipped it.

## 2026-09-08

Reopened in practice during the v0.1 run: the fix was necessary but not sufficient.

Starting the reader thread before the child narrowed the window but did not close it. Thread startup can take milliseconds under load, which is ample time for 'echo hello; exit 3' to run and exit before the thread reaches its first read(). The test kept flaking at roughly 1 run in 10.

Closed properly with a rendezvous: the reader thread sends on a zero-capacity sync_channel as its first act, and spawn() blocks on the matching recv. Returning from recv means the thread is on the very next instruction, so the residual window is a few instructions rather than a thread launch. Costs one context switch per spawn.

30 consecutive runs of the proc tests and 12 of the full suite, all green.

Worth keeping: the flake was real signal twice. Both times the temptation was to relax the assertion, and both times the assertion was right.
