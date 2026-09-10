---
id: 17
title: 'No global hash: toolchain and config changes do not invalidate'
type: bug
status: done
milestone: v1.0
assignee: Oddur Sigurdsson
created: 2026-09-08
updated: 2026-09-08
priority: p1
effort: m
area: cache
---

## Problem

Every key is built from a task's own inputs, command, declared env and upstream
keys. Nothing workspace-wide participates. So none of these invalidate anything:

- upgrading the toolchain (`rustc -V` changes, output changes, key does not)
- editing `[project] ignore` in `turborust.toml`, which changes what gets hashed
- a `Cargo.lock` change, for tasks whose inputs were written by hand rather than
  derived from a crate

## Proposal

A `global_hash` mixed into every task key, as turborepo does (MIT; design only):
the toolchain version, the resolved `turborust.toml`, and the lockfile.

Keep the existing `turborust-v1` prefix as the tool-version component — that part
we already have, and it is what lets a change to the hashing scheme invalidate
everything at once.

## Acceptance criteria

- [x] `rustc -V` participates in every key
- [x] `turborust.toml` content participates in every key
- [x] `turborust why` attributes a global-hash miss to the specific component

## 2026-09-08

A GlobalHash (src/global.rs) computed once per run and mixed into every task key: rustc -V, a hash of the config file verbatim, and a hash of Cargo.lock.

Keys are named global:toolchain / global:config / global:lockfile rather than folded into one opaque value, so a miss is attributable. Verified — appending a [project] table that no task input covers produces:

    cache MISS — b3:0a68ac555dd8 → b3:390b0d43495b
        ~ [global:config]  7c7d93ec417a5d81 -> b5369307ca9bd083

The toolchain component closes the gap deliberately left open by 0014: strict env mode passes PATH without hashing it, so swapping toolchains through PATH changed the output without changing any key. rustc -V now covers exactly that.

Workspace gained a  field holding the config text. Hashing the parsed struct would have missed settings like [project] ignore, which change what gets hashed without appearing in any task's own inputs.

rustc being absent yields None rather than an error: turborust orchestrates plenty of processes that have nothing to do with Rust, and refusing to run there would be a strange way to enforce a cache property.
