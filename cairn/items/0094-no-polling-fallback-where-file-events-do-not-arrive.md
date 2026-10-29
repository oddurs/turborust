---
id: 94
title: No polling fallback where file events do not arrive
type: feature
status: done
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

- [x] Polling can be chosen in config and on the command line
- [x] The same dispatch, debounce and access-event filtering apply
- [x] `doctor` recommends it on a filesystem that does not deliver events

## 2026-10-29

`[project] watch = { poll = "500ms" }` or `up --poll[=INTERVAL]` (the flag wins,
so a one-off session on a mounted volume needs no edit to a committed file).
Intervals under 10ms are refused at load.

Found while building it: notify's own `PollWatcher` is not usable here. It
stores modification times in whole seconds and only compares contents when told
to hash every file on every scan. So the second of two saves within one second
was lost for good, which is the ordinary editing rhythm; and the alternative
reads the whole tree every interval over exactly the network filesystems polling
exists for. The poller here is ours: full-precision mtime plus length per file,
the first scan taken before `watch_with` returns (so an edit right after start
is a change, not part of the baseline), `.git`/`target`/`node_modules` skipped at
any depth, symlinked directories not followed. Its batches go through the same
callback as native events, so filtering, relativizing and dispatch are shared.

`doctor` reads `/proc/self/mountinfo`, takes the deepest mount holding the
workspace, and warns for nfs, cifs/smb, 9p and drvfs (WSL2's Windows drives),
vboxsf, sshfs, grpcfuse and prl_fs, unless polling is already configured.

Mutation-checked: whole-second mtimes fail the same-second test; an empty
baseline fails the quiet-tree test; walking never-watched directories fails the
skip assertion.
