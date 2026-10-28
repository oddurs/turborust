---
id: 94
title: No polling fallback where file events do not arrive
type: feature
status: backlog
created: 2026-10-28
updated: 2026-10-28
priority: p2
effort: s
area: watch
---

## Problem

OS file events do not cross every boundary people develop across. Docker Desktop
bind mounts on macOS and Windows, WSL2 paths under `/mnt/c`, NFS, SMB and
Vagrant shares deliver few or no events, so the watcher sees nothing and the
dev loop silently stops rebuilding — indistinguishable, from the outside, from
the watch-limit failure fixed in `0089`.

## Precedent

watchexec `--poll`, nodemon `--legacy-watch`, Vite `server.watch.usePolling`,
webpack `watchOptions.poll`. The `notify` crate this project already uses ships
a `PollWatcher` behind the same interface.

## Proposal

`[project] watch = { poll = "500ms" }` and `--poll` on `up`, switching to the
poll backend. `doctor` should suggest it when the workspace is on a filesystem
known not to deliver events (`/mnt/[a-z]` under WSL, 9p, NFS, CIFS).

## Acceptance criteria

- [ ] Polling can be chosen in config and on the command line
- [ ] The same dispatch, debounce and access-event filtering apply
- [ ] `doctor` recommends it on a filesystem that does not deliver events
