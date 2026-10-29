---
id: 75
title: doctor advises but never measures
type: feature
status: done
milestone: v1.4
created: 2026-09-10
updated: 2026-10-29
claimed: 2026-10-29
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

- [x] Per-node run history is persisted across sessions and bounded in size
- [x] `doctor --profile` reports wall time, run count, hit rate and dominant
      miss reason per node
- [ ] Cold-build time is attributed to crates and to linking
- [x] Findings are ranked by what the reader can act on
- [x] Regressions are surfaced against the recorded baseline
- [x] History collection can be turned off, and is local-only — this is
      measurement of your machine for you, and `0058` still holds

## 2026-10-29

`src/profile.rs`. Every task run appends one line to `.turborust/profile/runs.jsonl`: when, how long, hit or not, ok or not, and **why** it missed. That last is new information, not just kept. The miss path now diffs the old key against the new one in full and names each kind of change: `env:X`, `dep:X`, `toolchain`, `config`, `lockfile`, `cmd`, all source files as one `files`, and for the cases with no diff, `first run`, `no inputs`, `loose env`, `forced`, `cache disabled`, or `unservable` (same key, but it had failed or its outputs were gone). `doctor --profile [--since 24h]` aggregates the window per task and ranks it.

Decisions:

- **Rank by fixability, then by time lost to misses.** The spec's own example is the reason: `files` co-occurs with nearly every miss, so "most frequent reason" would always say "your sources changed" and hide the one variable rebuilding everything. An actionable reason (an `env_keys` variable, an env-file value, no inputs, loose env) is named whenever it explains at least a quarter of misses, and the task ranks above ones that cost more for expected reasons.
- **The cold-build half reads `cargo build --timings` rather than running a build.** doctor starting a cold build would throw away the target directory's state to measure it. It reads the `UNIT_DATA` array cargo embeds in the newest report under `target/` (and each split node's target), sums units per crate, and reports the slowest crates and the final binaries.
- **Linking is not separated, so that criterion stays unticked.** cargo's report gives binary units a total only; it records a `link` section only when it has one, and stable cargo here (1.99) does not. Binaries are reported as "final binaries (compile + link)", and as linking only when the section exists. Saying "linking: 18s" when it is really codegen plus link would be the kind of confident wrong number this feature exists to replace.
- **Regressions:** the window's successful misses against earlier ones, by median, needing ≥5 earlier and ≥3 in the window, and a difference both over 20% and over 1s. Relative alone flags noise on 50ms tasks.
- **Bounded at 4 MiB** (about 20,000 runs) by dropping the older half, written aside and renamed. A torn line is skipped on read.
- **Tasks only.** A service's time-to-ready is not logged yet: readiness is decided in the lifecycle machine, not on the run path, and a service that never restarts has no "runs" to compare. Worth a follow-up if it is wanted.
- **`parse_duration` learned `d`**, for `--since 7d`. Config durations accept it too, still capped at a year.

A second bug, found by the end-to-end check, before it shipped: **the last task to finish in every `run` was never logged.** The first version logged in a wrapper after `run_task` returned, skipping runs ended by shutdown. But setting a task's final status is what lets the run end, and on the multi-threaded runtime another worker sets `shutting_down` while the finishing task is still storing its result, so the slowest task, the one most worth reporting, always vanished. Records are now written before each final status is set. `every_run_is_logged_for_the_profile_including_the_last_to_finish` runs multi-threaded on purpose. Under `#[tokio::test]`'s single thread the race cannot happen, and the test passed with the bug in place. With the original placement it fails 3 of 3 runs; fixed, it passes 3 of 3.

Checked by hand: a task keyed on `PROFILE` with the variable cycling, plus an edit, gives "keyed on env `PROFILE`, which changed on 4 of 4 misses" ranked above an uncacheable task and a task whose misses were source edits. On a real Dioxus project's timings report: syn 10s, web-sys 10s, js-sys 8.4s, and final binaries 640ms, for a 32s build.

