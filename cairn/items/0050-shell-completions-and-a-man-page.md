---
id: 50
title: Shell completions and a man page
type: chore
status: done
milestone: later
assignee: Oddur Sigurdsson
created: 2026-09-08
updated: 2026-09-08
priority: p3
effort: s
area: cli
---

## Problem

No completions and no man page. Both are cheap, and their absence is one of the
signals people use to judge whether a CLI is finished.

## Proposal

`clap_complete` and `clap_mangen`, generated from the existing derive tree, behind
`turborust completions <shell>` and `turborust man`. Generated at runtime rather
than committed, so they cannot drift from the CLI.

Node names would ideally complete too — `turborust run <TAB>` offering the tasks
in the nearest config. That needs a dynamic completer and is worth doing only if
the static version lands cleanly first.

## Acceptance criteria

- [x] `completions bash|zsh|fish|powershell` emits a usable script
- [x] `man` emits a valid man page
- [x] Neither is committed to the repository

## 2026-09-08

clap_complete and clap_mangen, generated from the existing derive tree at runtime. Nothing committed, so neither can drift from the CLI — which was the point of generating rather than checking in.

Verified as artifacts rather than as byte counts: the bash and zsh scripts are parsed with `bash -n` and `zsh -n`, and the man page is rendered through `man` and reads correctly. A completion script that is merely non-empty is not evidence of anything.

Dynamic node-name completion (`turborust run <TAB>` offering the tasks in the nearest config) is deliberately not attempted here. It needs a completer that reads and resolves the config at completion time, which is a different mechanism with its own failure modes — a slow or erroring completer makes the shell feel broken. Worth a separate item if anyone wants it.
