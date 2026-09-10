---
id: 67
key: v1.3
title: A cache you can audit
type: milestone
status: backlog
depends_on:
- 40
created: 2026-09-10
updated: 2026-09-10
due: 2027-09-01
---

The cache is the load-bearing claim in this project. `why` explains it, the
README leads with it, and every performance argument rests on it. So it has to
be precise about what it invalidates, bounded in what it keeps, portable across
the checkouts people actually work in, and able to prove its own answers.

Four properties, in that order:

- **Precise.** A downstream node should rebuild when its upstream's *output*
  changes, not merely when its upstream re-ran.
- **Bounded.** A store that keeps ten results per task keeps ten copies of a
  300 MiB artifact, forever, and evicts by write time — which is to say it
  evicts the entries you use most.
- **Portable.** Content-addressed results are portable by construction. Two
  worktrees of the same repository getting cold caches is a bug in where the
  store lives, not a fact about caching.
- **Provable.** A content-addressed cache is the only kind that can prove a
  build is non-deterministic. Nothing else in the ecosystem can say it as
  cheaply, and no one is saying it.
