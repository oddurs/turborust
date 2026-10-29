---
id: 74
title: Test failures are invisible where every other failure is visible
type: feature
status: done
milestone: v1.4
created: 2026-09-10
updated: 2026-10-29
priority: p1
effort: m
area: diag
---

## Problem

A failing `rustc` compile gets the full treatment: the diagnostic is parsed out
of cargo's output, given its error code, `file:line:col` and code frame, linked
into the editor with a `vscode://` URL, and thrown over the app in the browser
overlay.

A failing test gets none of it. It is plain text in a log pane. Same workspace,
same run, same kind of information — a location and a message — and one of them
is a first-class object while the other is scrollback.

That is backwards for a dev loop. In practice a test failure is the more common
signal, because the compile has to succeed first.

## What should happen

Parse test failures into the same diagnostic model that `rustc` errors already
use, so they arrive everywhere diagnostics already arrive.

Prefer `cargo nextest`'s machine-readable output where it is installed, and fall
back to `libtest`'s. Nextest is worth preferring for its own sake: process-per-test
isolation means a panicking test does not take the others with it, and its
output is designed to be parsed rather than read.

A failure should carry the test's name, the assertion message, the file and line
from the panic location, and the captured output — and then appear in the TUI,
in the overlay's Issues panel, and in `--json`.

## What this deliberately does not add

Not a `[tests.*]` node kind. A test run is a task: it has a command, inputs
derived from a crate's dependency closure, and a cache key. Everything about
selection already falls out of that — a test task declared with `cargo = "api"`
inherits `api`'s closure, so editing `crates/shared` re-runs exactly the test
tasks that could be affected, using machinery that already exists.

Inventing a node kind to get behaviour the existing one already has would be
the mistake `0057` describes. The gap here is presentation, and it should stay
that size.

## Acceptance criteria

- [x] Test failures are parsed into the diagnostic model, from nextest when
      present and libtest otherwise
- [x] They appear in the TUI, the overlay's Issues panel and `--json`
- [x] Panic locations are editor links, like compile errors are
- [x] Confirm that a watch-driven test task re-runs on an edit to a transitive
      path dependency, and write the test that keeps it true
- [x] `doctor` mentions nextest when it is absent, as a suggestion rather than a
      requirement

## 2026-10-29

Built on the existing diagnostic model: a failing test is a `Diagnostic` with `code = "panic"`, the panic's `file:line:col`, the message as the message, and what follows (`left:`/`right:`) as the frame. It then reaches the TUI sidebar, the overlay's Issues panel (with the same `vscode://` link, since that link is built from any diagnostic's location) and `run --summarize`'s new `issues` array without anything downstream knowing it is a test.

**Not machine-readable output, despite the spec preferring it.** libtest's `--format json` is nightly-only, and nextest's libtest-JSON output is still experimental (`NEXTEST_EXPERIMENTAL_LIBTEST_JSON`). Asking either would mean adding flags to the user's command. Both print the same panic line (`thread 'name' (id) panicked at file:l:c:`, and the pre-1.73 `panicked at 'msg', file:l:c`), so that line is what's read, from real output of a three-failure crate kept as fixtures: `cargo test`, with and without `RUST_BACKTRACE`, and nextest 0.9.150.

Two things the real nextest showed that a hand-written fixture had not:

- **On a pty, nextest's progress bar destroys its own lines.** It redraws in place, so through a pty the failures arrive run together and none could be read; the only issue was nextest's closing "error: test run failed". turborust now gives every child `NEXTEST_SHOW_PROGRESS=counter`, unless the node's env or (when not isolated) the ambient env already sets it. The bar was garbage in the log pane anyway, since that is not a terminal.
- **nextest indents a test's output by four spaces**, which ended up in the frame. The panic line's own indentation is now cut from the lines under it. "error: test run failed" is treated as a summary line, like cargo's "test failed, to rerun pass".

A service that panics is named for its thread ("`main` panicked"), not as a test, because the same line comes from any Rust process.

`doctor` suggests nextest as an Info finding. It is a suggestion, never a requirement.

The transitive-dependency criterion was already true, and is now pinned by `a_test_task_reruns_when_a_transitive_path_dependency_changes`: app→mid→base, an edit to base re-runs the task, an edit to an unrelated file does not.

A second bug surfaced while verifying this, in 0110's change rather than this one: `run --summarize` deadlocked. Noted and fixed on 0110.


A third bug, after this shipped: `run --summarize` could leave out the failures it exists to record. The summary was built before `drain_output`, so issues parsed from a run's last lines were still in the queue when it was taken. Invisible on an idle machine; under load, `run_summarize_writes_its_record_and_exits` failed 6 of 40 parallel runs, and it was the pre-push hook's full-suite run that caught it. Output is now drained first; 0 of 120 under the same load.
