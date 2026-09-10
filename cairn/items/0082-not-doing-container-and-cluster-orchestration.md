---
id: 82
title: 'Not doing: container and cluster orchestration'
type: feature
status: dropped
milestone: later
created: 2026-09-10
updated: 2026-09-10
priority: p2
area: scope
---

## Considered and declined

Tilt, Skaffold, DevSpace and Garden own the Kubernetes inner loop: build an
image, deploy it, and — in Tilt's case with `live_update` — sync changed files
into a running container instead of rebuilding it. Tilt's dashboard, which gives
every resource an update status and a runtime status, is a genuinely good design
and worth stealing from at the UI level.

## Why not

turborust supervises processes on your machine, and that is the whole premise:
a pty per child so `cargo` looks like `cargo`, process-group signalling, a
control socket you can attach a terminal to. None of that survives the move into
a container, and all of it is what makes the tool feel direct.

The moment images and manifests are involved, the interesting problems become
image layer caching and cluster state reconciliation, which have nothing in
common with the supervision tree or the fingerprint. That is `0057`'s test, and
this fails it.

## What was worth taking

Tilt's two-status model — how the *build* went, separately from what the
*process* is doing right now — is exactly the distinction `0073` needs in order
to represent a node that is rebuilding itself while still serving. Taken; the
rest declined.
