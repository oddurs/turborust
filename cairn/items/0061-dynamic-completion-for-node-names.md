---
id: 61
title: Dynamic completion for node names
type: feature
status: backlog
milestone: later
created: 2026-09-08
updated: 2026-09-08
priority: p3
effort: m
area: cli
---

## Problem

`turborust run <TAB>` completes nothing. The static completions from 0050 know
the subcommands and flags but not the nodes in the nearest config, which is the
part a person actually has to remember.

## Proposal

A dynamic completer that reads and resolves the config at completion time.

Deferred from 0050 on purpose: a completer runs on every keystroke, so a slow or
erroring one makes the whole shell feel broken. It needs a fast path that reads
`turborust.toml` directly without asking cargo for metadata, and it must fail
silently rather than printing an error into the command line.

## Acceptance criteria

- [ ] `run` and `up` complete node names from the nearest config
- [ ] Completion never blocks measurably, and never runs `cargo metadata`
- [ ] A broken or missing config completes to nothing rather than erroring
