---
id: 19
title: Adopt process-wrap for process grouping
type: chore
status: done
milestone: v1.0
assignee: Oddur Sigurdsson
created: 2026-09-08
updated: 2026-09-08
priority: p2
effort: m
area: proc
---

## Problem

`Handle::stop` hand-rolls `killpg` behind `#[cfg(unix)]`. It is correct on Unix
and absent on Windows (0012).

## Proposal

watchexec uses the `process-wrap` crate, which offers one API over
`ProcessGroup::leader()` and `ProcessSession` on Unix and `JobObject` on Windows.
Depending on it replaces our `cfg`-gated code and closes 0012 without us writing
any Windows-specific process code.

Two things to check before adopting, because they are the reason our current code
exists:

- it must compose with `portable-pty`, since we spawn onto a pty rather than pipes
- the pty child must remain a process-group leader, which is what makes group
  signalling work at all

If those do not hold together, keep the hand-rolled Unix path and use
`process-wrap` only for the Windows arm.

## Acceptance criteria

- [x] Grandchildren are terminated on stop, on Unix and Windows
- [x] pty output is unchanged (colour, progress, `isatty`)
- [x] 0012 is closed or explicitly re-scoped

## 2026-09-08

Re-scoped: process-wrap is NOT adopted, and the reason is structural rather than a preference.

process-wrap wraps std::process::Command / tokio::process::Command. portable_pty::SlavePty::spawn_command takes portable-pty's own CommandBuilder, because it has to dup the slave fds into the child before exec. There is no seam where process-wrap could apply without giving up the pty — and the pty is load-bearing: it is what keeps cargo's colour, progress bars and line buffering. Trading that for a dependency would be a bad deal.

So the Unix path keeps its killpg escalation, and Windows gets a job object directly (see 0012). The item's own fallback clause anticipated exactly this.

Worth recording for anyone re-opening the question: the incompatibility is between pty-backed spawning and command-wrapping in general, not a gap in process-wrap. Any crate of that shape will have the same problem.
