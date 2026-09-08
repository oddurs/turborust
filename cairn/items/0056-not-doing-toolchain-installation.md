---
id: 56
title: 'Not doing: toolchain installation'
type: feature
status: dropped
milestone: later
created: 2026-09-08
updated: 2026-09-08
area: toolchain
---

## Considered and declined

moon installs and pins language runtimes through a `toolchain` plugin system, so a
checkout can bootstrap itself.

## Why not

rustup already does this, is universally installed alongside cargo, and is
configured by a `rust-toolchain.toml` that cargo itself honours. Duplicating it
would mean a second source of truth for which compiler you are using — precisely
the ambiguity the global hash (0017) exists to eliminate.

What turborust should do instead, and does: record `rustc -V` in the cache key so
a toolchain change invalidates, and let `doctor` report when the environment is
working against you. Observing the toolchain is in scope; managing it is not.
