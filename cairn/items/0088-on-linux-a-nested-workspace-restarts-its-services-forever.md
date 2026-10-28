---
id: 88
title: On Linux, a nested workspace restarts its services forever
type: bug
status: done
milestone: v1.4
created: 2026-10-28
updated: 2026-10-28
priority: p0
effort: s
area: watch
---

## Problem

On Linux, `turborust up` on a workspace laid out like a real one —
`crates/<name>/src/*.rs` — restarted its services about eight times a second,
forever, with nothing edited. Measured against the built binary with
`watch = ["crates/**/*.rs"]`: 50 starts in six seconds, every one blamed on
`crates/api/src/main.rs`, which nobody had touched.

inotify reports a file or directory being *opened*. The watcher treated every
event as a change, and for each directory event `expand_dir` reads that
directory to find the files in it — which is itself an open, which is the next
event. Arming a recursive watch opens every subdirectory, so the loop started on
its own.

macOS never showed it: FSEvents does not report opens, and this project is
developed on macOS. Every Linux test watched a flat `src/`, which never takes
the directory-expansion path.

## Fix

Access events are dropped before anything else sees them. A write always
produces a modify event as well, so nothing real is lost; watchexec ignores
access events by default for the same reason.

## Acceptance criteria

- [x] A nested workspace with nothing edited is never restarted
- [x] One edit restarts each affected service exactly once
- [x] A watcher test proves reads and directory listings are not changes, and fails without the fix
- [x] A supervisor test on a nested layout, failing without the fix

## 2026-10-28

Found while surveying adjacent tools for feature gaps, not by any test: checking whether the watcher reported an exhausted inotify limit (it does not — `0089`) produced a restart storm that turned out not to need the limit at all. Without the fix, the new supervisor test counts 26 restarts with nothing edited; with it, zero, then exactly one for one edit.

Likely related: `one_edit_restarts_each_node_exactly_once` has failed intermittently on main and on every branch today (1 in 30 runs on main; first recorded on `0085`), with its task reading `cached` after the edit — a rerun that saw no change, which is what a spurious event produces. Not yet shown to be the same cause; recorded so the next look starts here.
