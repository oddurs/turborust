---
id: 74
title: Test failures are invisible where every other failure is visible
type: feature
status: backlog
milestone: v1.4
created: 2026-09-10
updated: 2026-09-10
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

- [ ] Test failures are parsed into the diagnostic model, from nextest when
      present and libtest otherwise
- [ ] They appear in the TUI, the overlay's Issues panel and `--json`
- [ ] Panic locations are editor links, like compile errors are
- [ ] Confirm that a watch-driven test task re-runs on an edit to a transitive
      path dependency, and write the test that keeps it true
- [ ] `doctor` mentions nextest when it is absent, as a suggestion rather than a
      requirement
