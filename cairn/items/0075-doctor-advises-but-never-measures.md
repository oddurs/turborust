---
id: 75
title: doctor advises but never measures
type: feature
status: backlog
milestone: v1.4
created: 2026-09-10
updated: 2026-09-10
priority: p1
effort: l
area: doctor
---

## Problem

`doctor` reads `Cargo.toml` and `.cargo/config.toml` and tells you what a good
configuration looks like: use a fast linker, drop dev debuginfo, optimise
dependencies, do not put `target/` in a synced folder, wire up sccache. Useful,
and entirely static. It is a linter.

It never says where your seconds actually went. And turborust is the process
that ran every one of those builds — it holds per-node durations, cache hits and
misses, the reason for each miss, how long nodes spent blocked on a concurrency
slot or a cargo lock, and it holds them across sessions.

The most actionable number a build tool can show is not "which crate is slow".
It is "you waited nineteen minutes on `build` today, and it missed cache on
thirty-one of thirty-four runs because `PROFILE` keeps changing". That sentence
requires the graph, the fingerprint and history. This tool has all three and
reports none of it.

## What should happen

Keep the static checks. Add measurement.

```
$ turborust doctor --profile

  today — 41 min of waiting across 3 nodes

  build        19m   34 runs   cache 3/34
      misses keyed on env PROFILE, which changed on 28 of them
      → declare it, or stop exporting it from your shell

  web          14m   12 runs   cache 0/12
      never hits: target_dir = "split" and outputs are undeclared
      → declare outputs so results can be replayed

  check         8m   34 runs   cache 30/34   healthy

  cold `up`     94s median, +22s since 2026-09-02
      61s in codegen (serde_derive, sqlx-macros), 18s linking
```

Two halves, and the second is the hard one:

- **Per-node history.** Wall time, run count, hit rate, and the dominant reason
  for misses. All of this is already computed and then thrown away.
- **Where a cold build goes.** Attribute time within a build to crates and to the
  link step, from `cargo build --timings` or `--message-format=json` timings.

Rank by what the reader could change. A slow crate they do not own is trivia; a
node that never hits cache because of one undeclared variable is a fix.

## Why this belongs here rather than in cargo

cargo sees one invocation. turborust sees the graph, the cache, the parallelism
ceiling and the history. "This crate takes 12s" is a cargo-shaped fact. "You
rebuild this crate forty times a day and never needed to" is not.

## Acceptance criteria

- [ ] Per-node run history is persisted across sessions and bounded in size
- [ ] `doctor --profile` reports wall time, run count, hit rate and dominant
      miss reason per node
- [ ] Cold-build time is attributed to crates and to linking
- [ ] Findings are ranked by what the reader can act on
- [ ] Regressions are surfaced against the recorded baseline
- [ ] History collection can be turned off, and is local-only — this is
      measurement of your machine for you, and `0058` still holds
