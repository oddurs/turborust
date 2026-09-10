---
id: 51
title: JSON schema for turborust.toml
type: chore
status: done
milestone: later
assignee: Oddur Sigurdsson
created: 2026-09-08
updated: 2026-09-08
priority: p3
effort: m
area: config
---

## Problem

The config has grown to services, tasks, health probes, env modes, overlay,
cache, target dirs and change policies. It is all documented in the README and
none of it is discoverable in an editor. A typo in a key name is currently a
runtime error at best, and silently ignored at worst.

## Proposal

Derive a schema with `schemars`, emit it via `turborust schema`, and ship it so
editors can pick it up.

Two things this would catch that the parser does not: unknown keys, which serde
currently accepts silently, and enum values, which today only fail once the config
is loaded. Adding `deny_unknown_fields` is arguably the higher-value half and does
not need the schema at all — worth doing first, and separately, because it is a
breaking change for anyone with a typo they have not noticed.

## Acceptance criteria

- [x] `turborust schema` emits a valid JSON Schema
- [x] It covers every table and enum
- [x] Unknown keys are rejected with the offending key named

## 2026-09-08

Both halves, and the item was right that the smaller one is worth more.

`deny_unknown_fields` on every config struct means a typo now fails loudly and lists the keys that were expected:

    unknown field `heath`, expected one of `cmd`, `cargo`, ..., `health`, ...

Previously serde accepted it silently, so a mistyped `health` key meant a service that never got a readiness probe and a `depends_on` that quietly degraded to "the process exists" — the exact weakness this project was built to fix, reintroduced by a missing letter.

It is a breaking change for anyone carrying a typo they have not noticed, which is the point rather than a side effect.

`turborust schema` emits a draft-2020-12 JSON Schema derived from the same types, so editors can complete and validate. Generated, never committed, for the same reason as the completions in 0050.

Tests cover a typo at every nesting level — project, task, overlay, cache, and an inline table — plus a fully-featured config that must still parse, since over-strictness would be just as bad as none.
