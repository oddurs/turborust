---
id: 70
title: The cache is bounded by count, and evicts what you use most
type: bug
status: done
milestone: v1.3
assignee: Oddur Sigurdsson
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

- [x] Eviction is driven by a use timestamp updated on hit, not on write
- [x] Total store size stays under `max_size`; a single result larger than the
      budget is recorded but not archived, matching the existing 256 MiB rule
- [x] *No* shared cache is evicted from — see the note; the plan said to sweep
      one when `push = true`, and that was wrong
- [x] `turborust cache` reports size, per-task breakdown, budget and the last
      eviction. Hit rate is left to `0075`, which owns persisted run history
- [x] A regression test proves a frequently-read entry outlives a
      recently-written one

## 2026-09-10

Implemented. KEEP_PER_TASK is gone; the local store now has a byte budget ([cache] max_size, default 10GiB, "0" for no limit) and drops least-recently-used results first.

Use time is recorded by touching a zero-byte '<hash>.used' marker beside the record on every hit. Rewriting the record itself was the obvious alternative and is wrong: a record carries one hash per input file, so a large crate closure makes it big, and taxing the hit path to speed up eviction is the wrong trade. Records written before markers existed fall back to their own mtime.

Changed my mind on one acceptance criterion. The plan said to apply the budget to a shared store when push = true. That is wrong: a shared cache is not one machine's to garbage-collect, and evicting from it deletes results other people are still reading on the strength of a budget they never set. It is now reported by 'turborust cache' and never swept. A shared cache needs its own retention policy.

Hit rate is deliberately not in 'turborust cache'. It needs persisted per-run history, which is 0075's first acceptance criterion; building half of it here would duplicate that work. The command reports size, per-task breakdown, budget, shared-store size, and what the last eviction dropped.

Found while doing this: load_latest scanned every file in the runs directory and parsed the newest by mtime. Markers are touched on every hit, so the newest file is usually a marker — which would have made load_latest return nothing for any task actually being used, silently breaking 'why'. It now filters to .json. Test: a_use_marker_does_not_look_like_a_record.
