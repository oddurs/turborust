---
id: 100
title: 'Not doing: sandboxed tasks'
type: feature
status: dropped
milestone: later
created: 2026-10-28
updated: 2026-10-28
priority: p3
area: scope
---

## Considered and declined

mise can run a task with its filesystem, network and environment restricted
(`deny_net`, `deny_write`, `allow_read` and so on).

## Why not

Enforcing that a task touches only what it declares would make the cache
provably correct, and that is a real attraction. But the mechanism is kernel
specific — Landlock on Linux, Seatbelt on macOS, nothing equivalent on Windows —
and partial enforcement is worse than none, because it reads as a guarantee.
The cheap, portable half already exists: strict environments (`0014`) keep
undeclared variables out of tasks. Full hermeticity is Bazel's problem, and
`0079` explains why that is a different product.
