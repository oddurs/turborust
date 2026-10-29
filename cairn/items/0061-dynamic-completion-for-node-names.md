---
id: 61
title: Dynamic completion for node names
type: feature
status: done
milestone: later
created: 2026-09-08
updated: 2026-10-29
claimed: 2026-10-29
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

- [x] `run` and `up` complete node names from the nearest config
- [x] Completion never blocks measurably, and never runs `cargo metadata`
- [x] A broken or missing config completes to nothing rather than erroring

## 2026-10-29

A hidden `turborust __nodes` prints every service and task in the nearest config, and the generated bash, zsh and fish scripts call it when the word being completed is a node: after `up`, `run`, `why`, `connect` or `verify`, or after `-F`/`--filter`. Anywhere else they fall through to the generated completion, unchanged.

- **Not clap_complete's dynamic completion.** It exists, but only behind `unstable-dynamic`, which also turns on clap's `unstable-ext`, and either may change in any release. A completer that breaks breaks the user's shell, so the glue is ours and small: one function per shell, plus one `complete` line for fish.
- **The fast path reads the file as plain TOML** (`toml::Table`) and takes the keys of `[services]` and `[tasks]`. There is no `cargo metadata`, no validation and no plan, so a config with an unknown key or a dangling dependency still completes its names. Any error prints nothing and exits 0, because output there lands in the command line. 3.8 ms median (4.0 ms max) on a 20-node config with a release build, process start included; outside a workspace, nothing.
- **zsh** registers itself at the end of its generated script. The node-aware function is inserted before that block and is what gets registered, both when sourced and when autoloaded from `fpath`. If the generated tail ever changes shape, the static script is emitted unchanged rather than half-edited.
- PowerShell and elvish keep the static script.

Checked in real shells, bash 5, zsh 5.9 and fish 3.7, against a config with `build`, `check` and `lint`:
- `run b` → `build`; `why` → all three; `up -F c` → `check`;
- `run --base x` and `clean x` fall through;
- zsh registers `_turborust_nodes`, falls back for `clean`, and parses with `zsh -n`;
- fish's `complete -C "turborust why ch"` → `check`.

