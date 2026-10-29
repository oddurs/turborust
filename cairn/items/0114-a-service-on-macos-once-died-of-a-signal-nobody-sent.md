---
id: 114
title: A service on macOS once died of a signal nobody sent
type: bug
status: backlog
created: 2026-10-29
updated: 2026-10-29
milestone: later
priority: p2
effort: m
area: engine
---

## Problem

In #45's CI, macOS, the second `--test supervisor` pass:
`a_command_probe_runs_with_the_nodes_environment` saw `with` (`cmd = "sleep 30"`)
become healthy and fail with exit code 1 in the same millisecond, about 20ms
after it was spawned. portable_pty reports a death by signal as code 1. The
re-run passed; `main` passed the same test on the same code.

Not reproduced on Linux with the macOS paths forced on (a waiter thread per
child, portable_pty's fork-based spawn, `killpg`-only group checks), in about
60 runs under load. No test in that binary sends signals of its own, and
turborust signals a child only through its own handle's `stop`, `kill_now` and
`signal`, none of which ran for `with`.

Since 0113 a child killed by a signal turborust did not send logs
`killed by a signal: <name>`, which the harness's timeout dump shows.

## Acceptance criteria

- [ ] The signal is known, from that line, the next time this happens
- [ ] Its sender is found and fixed, or the reason it cannot be is written down
