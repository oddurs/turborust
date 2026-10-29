---
id: 112
title: Starting many services forks a copy of turborust for each
type: feature
status: backlog
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
- [ ] `connect`, resize, ctrl-c passthrough and group signalling still work on Linux
- [ ] The integration suite passes unchanged
