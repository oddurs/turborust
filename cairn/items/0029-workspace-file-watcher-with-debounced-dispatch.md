---
id: 29
title: Workspace file watcher with debounced dispatch
type: feature
status: done
milestone: v0.0
created: 2026-09-08
updated: 2026-09-08
area: watch
effort: m
---

One OS watcher for the whole workspace, dispatched in userspace against compiled
globsets. N watchers would mean N copies of every event and N times the file
descriptors.

Ignored top-level directories are never handed to the OS watcher at all —
registering `target/` on macOS means FSEvents streams every intermediate artifact
of every build, enough to starve the debouncer during a cold build.

Dispatch signals only the upstream-most affected nodes; dependents come back
through the readiness cascade rather than restarting a second time on their own
account.
