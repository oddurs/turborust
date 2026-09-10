---
id: 79
title: 'Not doing: remote execution and distributed builds'
type: feature
status: dropped
milestone: later
created: 2026-09-10
updated: 2026-09-10
priority: p2
area: scope
---

## Considered and declined

Bazel and Buck2 execute actions on a remote worker fleet through the Remote
Execution API. Nx Cloud distributes tasks across agents and splits slow suites
into per-file tasks with Atomizer. moon ships a remote content-addressable store.
All of them turn a build into something a cluster does.

## Why not

It fails the test the rest of these boundaries are drawn with — not "does it
need the graph or the fingerprint", which it does, but the one behind it: is
this the same product.

Remote execution needs a scheduler, a worker protocol, sandboxed and
hermetically-described actions, artifact transport, and someone running the
fleet. Hermetic action descriptions in particular are a different tool: it means
knowing every input to `cargo build` including the toolchain, which is what
Bazel's rules_rust spends its complexity budget on. turborust deliberately does
not own the build — it orchestrates cargo, and cargo is not hermetic.

The honest local-first version already exists. `0013` shares results through a
directory both machines can see, and the trust model is written down: whoever can
write to a cache you read from can hand your build arbitrary outputs.

Reconsider if cargo becomes hermetically describable, which would change the
premise rather than the appetite.
