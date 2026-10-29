---
id: 112
title: Starting many services forks a copy of turborust for each
type: feature
status: done
created: 2026-10-29
updated: 2026-10-29
milestone: later
priority: p3
effort: m
area: engine
---

## Problem

Every child is started through `portable_pty`'s `spawn_command`, which uses a
`pre_exec` hook for the session and controlling-terminal setup. A `pre_exec`
hook forces a full `fork`, so the kernel copies turborust's page tables for
each child, and in the child the hook lists and closes every open descriptor.
Both grow with the number of children already running: one spawn takes about
1ms first and about 3ms with fifty services up (measured in 0111).

hivemind, which also runs every process on a pty, starts ten services in 18ms
and fifty in about 100ms, against turborust's 46ms and 165ms, because Go
starts processes with a `vfork`-style clone that copies nothing.

## Approach

On Linux, start children with `posix_spawn`, which glibc implements with
`CLONE_VM | CLONE_VFORK`: `POSIX_SPAWN_SETSID` for the session, the pty's
slave opened by path as stdin (a session leader opening a tty without
`O_NOCTTY` acquires it as its controlling terminal on Linux), duplicated onto
stdout and stderr, `POSIX_SPAWN_SETSIGDEF` and `SETSIGMASK` in place of the
signal reset, and `posix_spawn_file_actions_addchdir_np` for the working
directory. Every descriptor turborust opens is close-on-exec, so nothing needs
closing. macOS and Windows keep the current path.

## Acceptance criteria

- [ ] Fifty services start in no more than hivemind's time on the same machine
- [x] `connect`, resize, ctrl-c passthrough and group signalling still work on Linux
- [x] The integration suite passes unchanged

## Notes

Done as planned: `proc::fast_spawn` starts `/bin/sh` with `posix_spawn`,
`POSIX_SPAWN_SETSID`, every signal back to its default and an empty mask, a
`chdir` file action, and the pty's slave opened by path as fd 0 and duplicated
onto 1 and 2. The environment is the `CommandBuilder`'s, listed through
`iter_full_env_as_str` with `SHELL` from `get_shell`, exactly as portable_pty
would set it. The child is a small `portable_pty::Child` over `waitpid`, which
reports a signal death the way portable_pty does (code 1, named). Anything it
cannot reproduce exactly — no tty name, a relative program, an environment
variable that is not UTF-8 — returns `Unsupported` and portable_pty spawns it
instead. macOS and Windows are unchanged.

There were no tests of what the pty is for, so three were added, and they pass
on either path: ctrl-c written to the pty reaches the child as SIGINT, a
resize reaches it as SIGWINCH, and the child leads its own session with the pty
as its terminal. Dropping `POSIX_SPAWN_SETSID` fails all three. A fourth checks
that the fast path is actually taken.

Measured with `bench/compare.py lifecycle` (median of 7): ten services 40ms →
26ms, fifty 176ms → 112ms, against hivemind's 19ms and 99ms. The first
criterion is not met. What is left is not the spawn: raising `concurrency` from
4 to 64 changes nothing, and fifty shells each starting a `touch` on four CPUs
is most of both tools' time. The remainder, about a quarter of a millisecond a
child, is turborust's own per-node work — a pty reader thread, a pidfd, state
and history — which is not worth trading away for it.

`group_alive` now asks `killpg(pgid, 0)` first: no member at all is the usual
answer after a stop, and a `/proc` scan per stop had pushed stopping fifty
services from 16ms to 30ms. Back to 14ms.
