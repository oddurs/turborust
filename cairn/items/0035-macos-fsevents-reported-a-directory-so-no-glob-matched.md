---
id: 35
title: macOS FSEvents reported a directory, so no glob matched
type: bug
status: done
milestone: v0.0
created: 2026-09-08
updated: 2026-09-08
area: watch
effort: s
---

## What happened

A write to `src/a.rs` surfaced as a modification of `src` and nothing else.
Glob patterns match files, so a bare directory event matched nothing and the
rebuild never fired.

Found by a test that failed on the first run — the kind of platform behaviour that
is invisible until it bites in production.

## Fix

Directory events expand one level to their immediate file children, capped at 512
entries and skipping subdirectories, since the expansion runs on the watcher
callback thread and must not stall event delivery. Anything deeper arrives as its
own event.
