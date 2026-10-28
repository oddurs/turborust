---
id: 86
title: A config mistake runs quietly instead of being refused
type: bug
status: done
milestone: v1.4
created: 2026-10-28
updated: 2026-10-28
priority: p0
effort: s
area: config
---

## Problem

Feeding malformed configs to the built binary found four ways a mistake was
accepted and then did something other than what it said:

- **A missing `cwd` ran the command in `$HOME`.** The pty library falls back to
  the home directory when the directory does not exist. `cwd = "does/not/exist"`
  with `cmd = "echo $PWD"` printed `/root` and exited 0. With a mistyped `cwd`,
  a `rm -rf build` task would run in your home directory.
- **Outputs could point outside the workspace.** `outputs = ["../../x"]` or
  `["/etc/passwd"]` were accepted, and a cache hit restores outputs by writing
  them back — so a cache entry would write wherever the path pointed. moon and
  Bazel both refuse outputs outside the project.
- **A bad glob failed silently.** `inputs = ["src/[unclosed"]` made `run` exit 1
  with nothing printed. `why` showed the error; `run` did not, because a node's
  output was only flushed when a process exited, and there was no process.
  `ProcEvent::Failed` existed for exactly this — its comment says "a bad
  command, a missing binary, a bad cwd" — and nothing ever sent it.
- **Durations were clamped or saturated.** `-5s` became zero, `inf` and huge
  values saturated, and every deadline is `now + duration`: on platforms whose
  clock is a 64-bit nanosecond count, that addition panics.

## Acceptance criteria

- [x] A missing working directory is an error at spawn time, and nothing runs
- [x] An output that is absolute or climbs out with `..` is refused at load
- [x] A bad glob in `inputs`, `outputs`, `watch` or `ignore` is refused at load, naming the field
- [x] A failure with no process behind it is printed by `run`
- [x] Negative, non-finite and over-a-year durations are refused

## 2026-10-28

The cwd check is at spawn time rather than load time on purpose: a dependency may be what creates the directory, and refusing at load would break that. Path and glob checks are at load time, through the same `Matcher` the run builds, so they accept exactly what the run would. `a..b/x` and `..hidden` stay legal; only a `..` path segment, a leading separator or a drive letter is refused.

Found by probing, not by reading: twenty-odd malformed configs against `plan`, `run` and `up`. Most were already refused well — cycles, unknown dependencies, unknown signals, a name used for both a task and a service. Two odd-looking results were mine: `run` with a bad glob exits 1, not 0 as a pipe first suggested, and the "huge duration" cases did not panic on Linux, whose clock has the range; the panic is on the platforms whose clock does not.

Each fix was removed in turn and is caught by its test. The missing-cwd test writes its marker to an absolute path inside its sandbox, so a regression is detected without touching the real home directory.

Left as is: `port = 0` and an unparseable `health.http` URL are still accepted and simply never become ready. Both now report `unhealthy` after `ready_timeout` once 0073 lands, which is a clear enough signal that refusing them was not worth a separate change.
