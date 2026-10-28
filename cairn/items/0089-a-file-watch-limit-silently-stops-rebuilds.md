---
id: 89
title: A file-watch limit silently stops rebuilds
type: bug
status: done
milestone: v1.4
created: 2026-10-28
updated: 2026-10-28
priority: p1
effort: s
area: watch
---

## Problem

When Linux's inotify limit (`fs.inotify.max_user_watches`, 8192 on many
distributions) runs out, part of the tree goes unwatched and edits there never
rebuild anything — and turborust said nothing. Both places notify reports it
were discarded: a directory that failed to register at startup was skipped with
`.is_ok()`, and the error notify sends when a later directory cannot be watched
was dropped by `let Ok(events) = res else { return }`. notify's own comment on
that error is that the handler "should be notified... Otherwise, the user might
expect that a recursive watch is continuing to work correctly, but it's not."

Measured with the limit lowered to 100 on a 300-directory workspace: an edit
beyond the limit, no restart, no message.

## Fix

The watch limit becomes a warning — once per message — logged where the
dashboard and `--no-tui` both show it, with the sysctl that fixes it. Any other
registration error is a directory that vanished between listing and watching,
and stays silent. watchexec and Vite both report this condition.

## Acceptance criteria

- [x] An exhausted watch limit at startup is reported, with the fix
- [x] One that runs out later, as directories are created, is reported too
- [x] Nothing is said when the limit is not the problem
- [x] The decision is pinned by a unit test

## 2026-10-28

Checked end to end by lowering `fs.inotify.max_user_watches` to 100 in the environment that did this work and restoring it afterwards: the warning appears once, naming the current limit, and with the normal limit there is no output at all. CI cannot lower a kernel limit, so the test covers `watch_warning` directly rather than the path through the kernel.

Not done: `doctor` could predict this before it happens, by comparing the directory count it would watch against the limit. Worth adding to `0075`, which is about doctor measuring rather than advising.
