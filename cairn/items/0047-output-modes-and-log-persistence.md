---
id: 47
title: Output modes and log persistence
type: feature
status: done
milestone: v1.2
assignee: Oddur Sigurdsson
created: 2026-09-08
updated: 2026-09-08
priority: p2
effort: m
area: cli
---

## Problem

Output is always streamed, always interleaved, always to the terminal, and never
kept. That is right for one developer watching one screen and wrong for everything
else — in CI, interleaved output from parallel nodes is close to unreadable, and
nothing survives the run.

turborepo has `output_logs`, `log_order`, `log_file`, `log_prefix` and `color`.
These exist because the same tool has to serve a terminal and a build server.

## Design

```
--output-logs full|errors-only|new-only|none
--log-order   stream|grouped
--log-file    <path>
--color       auto|always|never
```

`grouped` buffers each node's output and prints it as one block when the node
finishes, prefixed with its name. That is the correct default in CI, where
interleaving is noise, and the wrong one interactively, where you want to watch.
So: `auto` — grouped when stdout is not a terminal, stream when it is.

`new-only` prints only nodes that actually ran, which pairs with a warm cache to
turn a hundred-line run into three.

`--log-file` writes ANSI-stripped text; the stripping already exists for the
diagnostic parser.

`--color` must honour `NO_COLOR` and `CLICOLOR_FORCE`, and detect a terminal
rather than assuming. Note the wrinkle: we set `FORCE_COLOR` on children so pty
output keeps its colour, so `--color never` has to strip on the way out rather
than ask children to be plain — they would ignore it anyway with a pty attached.

## Acceptance criteria

- [x] Four output modes, `errors-only` and `new-only` included
- [x] `--log-order` with a terminal-aware default
- [x] `--log-file` writes ANSI-stripped output
- [x] `NO_COLOR` is honoured
- [x] `--color never` strips output rather than relying on children

## 2026-09-08

Four verbosity modes, terminal-aware ordering, a log file, and colour control.

Two things fell out of the design that were not obvious up front:

1. `errors-only` and `new-only` cannot be decided while output is arriving — the outcome is not known until the node finishes. So they buffer even in stream order, and flush on the exit event. That meant pump_events had to learn about node outcomes, which it was already receiving and discarding.

2. `--color never` must strip on the way OUT, not ask children to be plain. Children run on a pty with FORCE_COLOR set precisely so cargo keeps its colours; asking them politely would be ignored. There is a test asserting no escapes survive.

The log file always gets everything, ANSI-stripped, regardless of terminal verbosity. It exists to be read after the fact, when filtering would only have destroyed evidence — so `--output-logs none --log-file x` is a sensible combination and is verified as one.

`--log-order auto` picks grouped when stdout is not a terminal. Interleaved parallel output is useful at a desk and close to unreadable in a CI log.

Verified all six paths against passing and failing tasks.
