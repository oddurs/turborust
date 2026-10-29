---
id: 96
title: A crash loop never gives up
type: feature
status: done
created: 2026-10-28
updated: 2026-10-28
priority: p3
effort: s
area: engine
---

## Problem

A service that crashes on start is restarted with backoff capped at
`backoff_max` (30s), forever. Each crash is a failure notification. A typo in a
command that will never work keeps the loop — and the notifications — going
until someone stops it.

## Precedent

process-compose `availability.max_restarts`, systemd `StartLimitBurst`,
supervisord `startretries` ending in `FATAL`.

## Proposal

`max_restarts` (default unlimited, for compatibility) after which the node is
`failed` and stays there until an edit or a manual restart from the overlay.
Consecutive failures only: a service that ran for a while and then crashed has
not used up its retries.

## Acceptance criteria

- [x] After `max_restarts` consecutive crashes the node stops being restarted and says so
- [x] A watched-file change or a manual restart resets the count

## 2026-10-29

`max_restarts = N` (default: unlimited, as before) under `restart = "always"`.
After N restarts in a row for crashing, the node stays `failed` with
"gave up after N restart(s) in a row; waiting for a change or a manual restart".
A watched change or a restart from the TUI or overlay is a new attempt with a
fresh count; all three arrive as the same `Ctl::Restart`, so one path covers
them. Spawn failures count as crashes, since a command that cannot start loops
exactly as one that dies.

"Consecutive" needed a definition the item left open. A run that stayed up
longer than `backoff_max` was not part of a loop, so the crash ending it starts
the count again. That reuses a knob that already means "the longest wait between
attempts" instead of adding one. Counting resets on healthy readiness were
rejected: a service with no probe is healthy the instant it spawns, so it would
never give up.

Mutation-checked: never limiting, never settling, and not resetting on a change
each fail a test.

CI on macOS failed `a_run_that_lasted_is_not_part_of_a_crash_loop` with 6 runs
for 5. The test, not the rule: with `backoff_max = "200ms"`, a "quick" crash (a
shell starting, `cat | wc`) took longer than that on a loaded runner and was
counted as having lasted. Reproduced locally by adding 0.3s to each quick run;
the margins are now 1s against a 2.5s run, which passes with the same delay.
The real default, 30s, is far from either.
