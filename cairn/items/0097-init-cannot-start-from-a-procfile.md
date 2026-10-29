---
id: 97
title: init cannot start from a Procfile
type: feature
status: done
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

- [x] `init` reads a Procfile into services and reports what it skipped
- [x] The generated config runs the same processes the Procfile did

## 2026-10-29

`init` beside a `Procfile` writes one service per `name: command`, alongside the
cargo inference; a process name that is also a crate is taken from the Procfile,
since that is what the project actually runs. Commands are written through
`toml::Value` so quotes and backslashes survive, and the plan test checks the
resolved command is byte-for-byte the Procfile's.

A command that reads `$PORT` gets the port foreman would have assigned (5000,
then +100 per process in file order) as `env` and as `port`, which also makes it
its readiness probe. A `.env` beside it becomes `[project] env_file` (`0093`).

Skipped, and named on stdout with the line number: `release` (Heroku runs it
once per deploy, so a supervised service would be wrong; it belongs as a task),
duplicate names, invalid names, empty commands, and lines that are not
`name: command`. `watch`, `depends_on` and `health` are left to the user, since
a Procfile has no way to say them.

Mutation-checked: a scaffold that ignores the Procfile fails the plan test and
the CLI test.
