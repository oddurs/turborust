---
id: 97
title: init cannot start from a Procfile
type: feature
status: backlog
created: 2026-10-28
updated: 2026-10-28
priority: p3
effort: s
area: config
---

## Problem

The people most likely to want this tool already run their stack with foreman,
overmind, hivemind or honcho, from a `Procfile`. `turborust init` infers a
config from the cargo workspace and ignores the Procfile next to it, so moving
over means retyping every process.

## Proposal

When a `Procfile` exists, `init` turns each `name: command` line into a service,
keeps the inferred cargo nodes, and says what it could not translate. Paired
with `0093`, a `.env` beside it is picked up as `env_file`.

## Acceptance criteria

- [ ] `init` reads a Procfile into services and reports what it skipped
- [ ] The generated config runs the same processes the Procfile did
