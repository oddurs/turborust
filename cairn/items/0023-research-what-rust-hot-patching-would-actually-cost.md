---
id: 23
title: 'Research: what Rust hot patching would actually cost'
type: docs
status: done
milestone: later
assignee: Oddur Sigurdsson
created: 2026-09-08
updated: 2026-09-08
priority: p3
effort: xl
area: research
---

## Problem

"Why not real hot reload instead of a full page reload?" is going to be asked, and
the honest answer should be written down once rather than re-derived.

## Findings

dioxus is the only Rust project shipping this (`dx serve`, Apache-2.0). Its
`packages/cli/src/build/patch.rs` is not dev-server code at all — it parses
Mach-O and wasm object files, walks symbol tables, and builds an `AddressMap` /
`JumpTable` that `subsecond` uses to redirect function calls in a running binary.
It pulls in `object`, `walrus`, `wasmparser` and `target-lexicon`, and carries
per-architecture and per-platform linking paths.

The conclusion for turborust: hot patching is a binary-patching problem with
per-platform linker work, not something an orchestrator can bolt on. The current
README claim — full page reload, not HMR, because a wasm binary has no module
boundaries left to patch — is accurate and should stay.

If this is ever wanted, the realistic route is to integrate `subsecond` rather
than to reimplement it, and it is a project in its own right.

## Acceptance criteria

- [x] A short section in the README pointing at dioxus/subsecond for anyone who
      wants this today

## 2026-09-08

README gained a "Hot reload, and why this is a full page reload" section pointing at dioxus and subsecond for anyone who wants this today.

The finding, recorded so it does not need re-deriving: dioxus packages/cli/src/build/patch.rs is not dev-server code. It parses Mach-O and wasm object files, walks symbol tables, and builds an AddressMap/JumpTable that subsecond uses to redirect calls in a running binary — pulling in object, walrus, wasmparser and target-lexicon, with per-architecture and per-platform linking paths.

So the honest answer is that hot patching is a binary-patching problem with per-platform linker work, and an orchestrator cannot bolt it on. The README claim that a wasm binary has no module boundaries left to patch is accurate and stays. If turborust ever wants this, the route is integrating subsecond, not reimplementing it.

Read under Apache-2.0, for design only; nothing copied.
