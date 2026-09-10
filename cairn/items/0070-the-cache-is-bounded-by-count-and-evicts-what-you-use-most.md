---
id: 70
title: The cache is bounded by count, and evicts what you use most
type: bug
status: backlog
milestone: v1.3
created: 2026-09-10
updated: 2026-09-10
priority: p1
effort: m
area: cache
---

## Problem

Two defects in one policy. `Cache::prune` keeps `KEEP_PER_TASK = 10` results per
task and deletes the rest.

**It is bounded by count, not by bytes.** A task whose outputs are a 300 MiB
release binary keeps 3 GiB. Per task. The shared cache directory (`0013`) has no
bound at all, and it is the one most likely to be on a network mount somebody
else pays for.

**It evicts by mtime, which here means write time, not use time.** So the entry
you hit forty times a day — your main branch's build — is evicted before the
one-off you produced last night on a branch you have already deleted. The
policy is precisely inverted for the access pattern it exists to serve.

That second one is worth stating as its own bug: mtime is *not* wrong as a
recency signal, but it is only recorded on write. Nothing touches a record when
it is read, so "newest" means "most recently produced" when it needs to mean
"most recently useful".

## What should happen

```toml
[cache]
max_size = "10GiB"   # default; "0" disables eviction
```

- Evict by least-recently-*used*, with the access time stamped on cache hit
- Bound by total bytes across `runs/` and `artifacts/`, not by count per task
- Apply the same budget to the shared cache when `push = true`, and never evict
  from a shared cache this machine only reads

And, because this project's whole posture is explaining itself rather than
being trusted: a `turborust cache` that reports what is being held — total size,
size by task, hit rate, and what the last eviction dropped. `clean` deletes;
nothing currently shows.

## Acceptance criteria

- [ ] Eviction is driven by a use timestamp updated on hit, not on write
- [ ] Total store size stays under `max_size`; a single result larger than the
      budget is recorded but not archived, matching the existing 256 MiB rule
- [ ] A read-only shared cache is never evicted from
- [ ] `turborust cache` reports size, per-task breakdown and hit rate
- [ ] A regression test proves a frequently-read entry outlives a
      recently-written one
