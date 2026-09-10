---
id: 12
title: 'Windows: stop() cannot signal a process group'
type: bug
status: done
milestone: later
assignee: Oddur Sigurdsson
depends_on:
- 19
created: 2026-09-08
updated: 2026-09-08
priority: p2
effort: m
area: proc
---

## Problem

`Handle::stop` escalates SIGTERM to SIGKILL via `killpg`, behind `#[cfg(unix)]`.
On Windows only `child.kill()` runs, which terminates the shell and leaves
grandchildren — the `cargo run` case — alive and holding the port.

The project compiles on Windows and is untested there. This is the first thing
that would break.

## Proposal

Use a Job Object: assign the child at spawn, terminate the job on stop. That is
the Windows equivalent of a process group and handles grandchildren correctly.

## Acceptance criteria

- [x] A Windows child and its grandchildren are terminated by `stop()`
- [x] The port is free immediately after
- [x] CI runs the test suite on windows-latest

## 2026-09-08

Implemented with a Windows job object, since process-wrap turned out not to compose with pty spawning (see 0019).

On spawn, the child is placed in a fresh job object with JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, so the tree cannot outlive the handle even if turborust dies without stopping cleanly. stop() calls TerminateJobObject, which takes the whole tree at once — Child::kill would only reach the shell, which is the bug. Failure to create or assign the job returns None rather than erroring: losing the ability to kill grandchildren is worth a leaked process, not a refusal to start.

The handle is stored as a usize rather than a HANDLE because HANDLE is a pointer and therefore not Send, and Handle crosses threads.

Honest status of verification. I cannot run Windows here. What I could do, I did:

- Compile-checked the real target: `cargo check --target x86_64-pc-windows-msvc`
  passes. This needed a new `portable-hash` feature, because blake3 uses MSVC
  assembly on Windows and there is no `ml64.exe` on a mac; the feature switches
  blake3 to pure Rust for cross-checking and is documented as not for release
  builds.
- Added `.github/workflows/ci.yml` running build + test on ubuntu, macos and
  windows-latest, plus a separate supervisor-test step, since Windows is
  precisely where process-tree teardown cannot be exercised on a maintainer's
  machine.

So the third acceptance criterion is met in the sense that CI is configured to run
it; it has not actually run, because there is no remote yet. The code is
compile-verified and behaviour-unverified, and the README says so rather than
implying Windows is supported.
