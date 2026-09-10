---
id: 71
title: Nothing proves a task is deterministic
type: feature
status: done
milestone: v1.3
assignee: Oddur Sigurdsson
created: 2026-09-10
updated: 2026-09-10
priority: p1
effort: l
area: cache
---

## Problem

The cache maps a content-addressed key to a set of outputs and replays them on a
hit. That is only sound if the same key really does produce the same outputs.
Nothing checks. If a task embeds a timestamp, iterates a `HashMap`, bakes in an
absolute path, or reads an environment variable it forgot to declare, the cache
will serve one of its several possible answers, silently and forever.

`0014` fixed the case that could be fixed structurally — undeclared environment
variables — by filtering the child's environment. The rest cannot be fixed by
construction. It can only be detected.

## Why this tool and not another

Nx detects flaky tasks statistically: it watches results for the same task hash
across many CI runs and retries what disagrees. That needs a fleet and a hosted
service to see enough runs.

turborust already content-addresses every input and every output. It can settle
the question locally, in one command, in two runs — and say which file differed
and how. Almost nothing else in the ecosystem is holding the data to do that,
and nothing is doing it.

This is also the natural completion of `why`. `why` answers *why did this run*.
Determinism answers *should you believe the result when it does not run*.

## What should happen

```
$ turborust verify build

  build — ran twice from a clean target dir

  NOT DETERMINISTIC
      ~ target/release/api   b3:5f16c070 -> b3:d33db911
        differs at offset 0x1f40, 8 bytes
        likely: an embedded build timestamp

  check   deterministic (2 runs identical)
```

Two runs, separate target directories, same declared inputs and environment,
compare the declared outputs by hash. Report the first differing offset, because
"they differ" is not actionable and "they differ eight bytes in at 0x1f40" often
is.

And a passive mode, so this is caught in ordinary use rather than only when
someone goes looking:

```toml
[cache]
verify = "off"      # off | sample | always
```

`sample` re-runs a small fraction of cache hits and compares against what it was
about to replay. A mismatch is loud: it means the cache has been lying, and the
entry is dropped.

## Acceptance criteria

- [x] `turborust verify [task…]` runs each task twice in isolation and compares
      declared outputs
- [x] The report names the differing file, both hashes, and the first differing
      offset — plus the length of the differing run, which is what separates a
      stamped field from an ordering problem
- [x] `[cache] verify = "sample"` validates one hit in twenty during normal runs
- [x] A detected mismatch drops the poisoned entry and exits non-zero
- [x] The known-common causes are listed in the docs with their fixes, because
      "your build is non-deterministic" without a next step is a dead end

## 2026-09-10

Implemented. 'turborust verify [task...]' runs each task twice with its declared outputs deleted between runs, and compares what came back.

Deleting outputs first is the part that makes it mean anything: without it, the second run of an incremental tool is a no-op that reproduces byte-identical files and the check proves nothing.

The report gives the first differing byte offset and the length of the differing run, because 'they differ' is not actionable. A short run at a fixed offset with equal lengths is a stamped field; a long run is usually ordering; different lengths mean the output is genuinely being built differently. The README carries a causes-and-fixes table, since telling someone their build is non-deterministic without a next step is a dead end.

Passive mode is [cache] verify = off | sample | always, off by default because checking costs a rebuild. 'sample' is one hit in twenty, a fixed rate rather than a configurable float — the useful range is narrow and a tunable number nobody can reason about is worse than a decision. A mismatch drops the entry and fails the run.

Found while doing this, and it was mine from 0070: Config.cache is #[serde(default)], so a config with no [cache] table at all constructed SharedCache through a derived Default, which skips serde's per-field defaults and left max_size empty. Every command in such a workspace failed with 'empty size'. Default is now hand-written. Test: a_config_with_no_cache_table_still_has_a_budget.

Also found: resolving a plan with no targets means 'the default set', which is the services — so 'verify' with no arguments failed on a workspace of pure tasks. Verifying everything now names everything.
