---
id: 46
title: 'Predict a run: --dry-run and --summarize'
type: feature
status: done
milestone: v1.2
assignee: Oddur Sigurdsson
created: 2026-09-08
updated: 2026-09-08
priority: p1
effort: m
area: cli
---

## Problem

There is no way to ask what a run *would* do. `plan` shows the resolved graph and
`why` explains one task, but nothing answers "if I run this now, what executes and
what is cached" without actually doing it. And after a run, nothing is left behind
that a build server could read.

## Design

**`--dry-run`.** Resolve, compute every fingerprint, consult the cache, print the
plan annotated with hit/miss and the reason for each miss — then exit 0 without
spawning anything. This is `why`, applied to the whole graph, and it reuses the
same diff machinery.

`--dry-run=json` for the machine-readable form.

**`--summarize`.** After a real run, write
`.turborust/summaries/<timestamp>.json`: per node its command, cache status, key,
duration, exit code, and the changed inputs that caused a miss. Plus a run-level
total and the global hash components.

The point of the summary is answering "why was CI slow today" a week later, when
the terminal output is gone. That means it must record *why* a task missed, not
only that it did — a summary that says "cache miss" and nothing else is a log
line, not evidence.

Both formats need a schema version field from the first release; a JSON output
without one becomes unversioned forever.

## Acceptance criteria

- [x] `--dry-run` prints hit/miss per node and runs nothing
- [x] Misses show the input that caused them, as `why` does
- [x] `--dry-run=json` emits the same information as JSON
- [x] `--summarize` writes a per-run JSON file
- [x] Both carry a schema version

## 2026-09-08

`--dry-run` reuses the fingerprint and diff machinery `why` already had, applied to the whole graph. Misses show what moved, because a prediction that says "cache miss" and nothing else is a log line rather than evidence.

Three outcomes, not two: cached, would-run, and would-run-because-no-inputs-declared. Collapsing the third into a plain miss would hide the actual reason, which is a config omission rather than a change.

With no target given, a dry run predicts the entire graph. Requiring a target would make the common case the verbose one, and a prediction executes nothing so there is no risk in the wider default.

`--summarize` writes .turborust/summaries/<epoch>.json with per-task cache status, key, duration, exit code and what changed, plus the global hash components. The timestamp is epoch seconds, not a formatted date: a consumer can format it, and a formatted one cannot be sorted reliably.

Both carry schema 1 from the first release, with a test asserting it — a JSON output shipped without a version is unversioned forever.

Engine gained an outcomes list, recorded where it already publishes the overlay build row, so the summary reports what actually happened rather than being reconstructed afterwards.

Note to self: this note initially failed to save because cairn parsed a leading `--dry-run` as a flag. Pass `--` before note text that starts with a dash.
