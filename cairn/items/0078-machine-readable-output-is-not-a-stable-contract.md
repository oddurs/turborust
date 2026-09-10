---
id: 78
title: Machine-readable output is not a stable contract
type: chore
status: backlog
milestone: v1.4
created: 2026-09-10
updated: 2026-09-10
priority: p2
effort: m
area: cli
---

## Problem

`plan`, `why`, `graph` and `--summarize` all emit JSON, and every one of those
shapes is an implementation detail that happens to be serialised. Nothing
documents them, nothing versions them, and nothing fails when a field is
renamed.

That was fine while the only consumer was a person reading output. It is not
fine now. `0060` — `up` panicking without a TTY — was reported by an automated
consumer driving this tool in a pipeline, which is a preview of how it will
mostly be used. Anything that scripts against these shapes today is scripting
against a moving target.

## What should happen

Treat machine-readable output as an interface:

- One documented schema per command, emitted by `turborust schema --json <cmd>`
  the way `turborust schema` already emits one for the config file
- A `schemaVersion` on every payload
- A CI job that fails when a schema changes without the version changing —
  the repository already has this exact pattern for the config schema and the
  rendered roadmap, so it is a third instance of a solved problem
- Errors as structured data on stderr, not prose, so a non-zero exit is
  interpretable without parsing English

## Why before the interesting thing

This is deliberately filed ahead of `0077`. An MCP server over an unversioned,
undocumented JSON shape is a liability with a nicer name: it multiplies the
consumers of a contract that does not exist yet. Fix the contract, then decide
whether to wrap it.

## Acceptance criteria

- [ ] Every `--json` payload carries `schemaVersion`
- [ ] `turborust schema --json <cmd>` emits the schema for each command
- [ ] CI fails on an unversioned schema change
- [ ] Errors are structured on stderr, with a stable code per failure kind
- [ ] The compatibility promise is documented — what may change in a patch
      release and what may not
