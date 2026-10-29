---
id: 113
title: A cached run still takes 130ms
type: bug
status: done
created: 2026-10-29
updated: 2026-10-29
milestone: later
priority: p2
effort: m
area: engine
---

## Problem

`turborust run hello`, with `hello` already cached, takes about 137ms, where
running its command directly takes 2ms. Two things account for nearly all of
it: `rustc -V`, which every cache key includes and which costs about 70ms
through rustup's proxy, and a fixed 60ms sleep at the end of every run "to let
the pump drain the last lines". A tool meant to run in pre-commit hooks and CI
pays both on every invocation, whether or not anything runs.

## Acceptance criteria

- [x] The end of a run waits for its output, and no longer
- [x] A cached run does not start `rustc` when nothing that decides its answer has changed
- [x] Changing toolchain still changes every key

## Notes

Measured with `hyperfine` on a task already cached (`echo hi`): 137ms before,
5.2ms after; `main` at the time, 160ms.

**The end of a run.** The 60ms sleep is gone. `Engine::drain_output` waits
until every child that has exited has been reported, then sends a
`ProcEvent::Flush` through the queue and waits for the pump to reach it, so
everything before it has been handled. It gives up after 200ms, for a
grandchild that keeps a pty open. Measuring it showed the sleep had been
losing output, not just time: a task printing 100,000 lines, streamed, came
through with 39,000 to 88,000 of them on `main`; all of them now, every run.

It also surfaced a second bug, from #44: with exits notified at once, a
child's exit event could overtake the end of its output, and grouped output —
printed at the exit — lost whatever came after (4 runs in 10 short by a few
thousand lines). The exit event now waits for the reader's end of file, up to
100ms; the supervisor still hears of the exit immediately.
`an_exit_is_reported_after_all_of_the_output` fails without the wait.

**The toolchain.** `rustc -V` is answered from a cache keyed by a fingerprint
of everything that decides it: `PATH`, the identity (device, inode, size,
mtime) of the `rustc` it finds, `RUSTUP_TOOLCHAIN`, `CARGO_HOME`, every
`rust-toolchain(.toml)` from the working directory up, rustup's
`settings.toml`, and the identity of each installed toolchain's `rustc`, which
`rustup update` replaces. A miss asks `rustc -V` in the background, as before,
and remembers the answer. A `rustc` this cannot see into is never remembered:
a script, or a link to anything but `rustc` or `rustup` (asdf's and mise's
shims pick a toolchain from files of their own). Tested against a fake
installation: each of those inputs moves the key, and both kinds of shim are
refused. The answer itself is unchanged, so existing cache keys still hit.

**Also.** A child killed by a signal turborust did not send now logs which
signal, since its exit code (1) reads like any failure; filed with a macOS
CI failure it would have explained as 0114.
