---
id: 13
title: Shared build cache across machines
type: feature
status: done
milestone: later
assignee: Oddur Sigurdsson
depends_on:
- 6
- 15
created: 2026-09-08
updated: 2026-09-08
priority: p3
effort: xl
area: cache
---

## Problem

The cache is local. A team, or one person across two machines, rebuilds work that
has already been built somewhere else.

## Proposal

Deferred deliberately. The local cache has to be provably correct first — a wrong
remote cache hit is far worse than a slow build, and every correctness bug found
so far (mtime vs content, outputs deleted under a valid key, stale invalidation)
would have been amplified by sharing.

Revisit once the integration harness (0006) covers cache correctness properly.

## Acceptance criteria

- [x] Cache correctness is covered by tests before any sharing is built
- [x] A design note exists on trust: who may write to a shared cache, and why

## 2026-09-08

Implemented as a second cache directory rather than an HTTP cache server: any path both machines can see — a network mount, a synced folder, another checkout. Reads fall through to it and pull a hit into the local cache on the way past, so the round trip happens once. No protocol to design, no auth story to get wrong.

push defaults to false, and that IS the design note the acceptance criteria asked for. A build cache maps inputs to outputs, so anyone who can write to a cache you read from can hand your build arbitrary artifacts, and a poisoned entry is indistinguishable from a fast one. Reading is a trust decision; publishing is a bigger one, so pointing at a directory gets you its results and nothing more. The README states this in the same terms.

The prerequisite criterion was already met before this: cache correctness is covered by the env-mode tests (0014), the outputs/restore tests (0015) and the global-hash work (0017). This item stayed blocked on 0006 and 0015 for exactly that reason, and unblocking it was the right sequence.

Two tests, and the first one I wrote was wrong in an instructive way: I changed the command in the second checkout to prove the output could only have been replayed, forgetting that cmd is part of the key — so of course it missed. The correct proof is an identical config in both checkouts and a command that appends to a file which is NOT a declared output; its absence in the second checkout is proof the command never ran. The config text is part of the global hash, so it has to match too.
