---
id: 5
title: Readiness probe that matches a log line
type: feature
status: done
milestone: v0.1
assignee: Oddur Sigurdsson
created: 2026-09-08
updated: 2026-09-08
priority: p1
effort: m
area: health
---

## Problem

Readiness is limited to a TCP connect or an HTTP GET. Plenty of dev processes
announce themselves only on stdout — `trunk` prints its address, `cargo leptos`
prints when the watch is armed, migration tools print when they finish — and have
no port to probe or no port that means what you want.

Without a probe, `depends_on` degrades to "the process exists", which is the exact
weakness this project set out to fix.

## Proposal

```toml
health = { log = "listening on" }
```

Match against the node's output as it streams. The plumbing already exists: the
diagnostic parser reads every line in `pump_events`.

## Acceptance criteria

- [x] `health = { log = "..." }` marks a node healthy on first match
- [x] Matching is against ANSI-stripped text
- [x] `ready_timeout` still applies
- [x] A test asserts a service becomes healthy on its own output

## 2026-09-08

health = { log = "..." } marks a node ready when its output contains the substring.

Three details that matter:

1. Matched against ANSI-stripped text, so a colourised 'Serving at' still counts.
2. Reset on every Starting/Running transition, so a previous run's announcement cannot mark a fresh process ready — the same staleness class as the dependency wake-ups fixed in 0004.
3. Probes AND rather than OR. { tcp = 8788, log = "ready" } means both must pass. Precedence would have been the easier rule and the wrong one: declaring two probes plainly means you want both.

Checked in health::probe that the network half short-circuits correctly, since it now has to combine rather than return on the first match.

Verified through the CLI with a bundler-shaped service that sleeps, announces, then idles: the dependent task ran only after the announcement.
