---
id: 80
title: 'Not doing: a hosted cache service'
type: feature
status: dropped
milestone: later
created: 2026-09-10
updated: 2026-09-10
priority: p2
area: cache
---

## Considered and declined

Turborepo has Vercel Remote Cache, free since late 2024. Nx has Nx Cloud. moon
has a remote CAS backend. The pattern is the same: the tool is open source and
the cache is the product.

## Why not

Two reasons, and the second is the real one.

Running a cache is an operations commitment — uptime, storage cost, auth,
retention, and a support obligation to people whose builds now depend on your
server being up. That is a business, not a feature, and it is not this one.

More importantly it inverts the trust model in `SharedCache`, which is the most
carefully argued thing in the config: `push` defaults to false because a build
cache maps inputs to *outputs*, so write access to a cache you read from is
arbitrary code execution on your machine. A hosted service asks everyone to make
that grant to an operator by default. Charging for it does not make it smaller.

`0013` is the version that keeps the property: point at a directory you already
trust, and contributing to it stays a separate, explicit decision.
