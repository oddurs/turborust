---
id: 93
title: There is no way to load a .env file
type: feature
status: done
created: 2026-10-28
updated: 2026-10-28
priority: p1
effort: s
area: config
---

## Problem

Nearly every project that runs locally keeps its settings in a `.env` file, and
every Procfile runner reads one by default — foreman, honcho and hivemind from
the working directory, overmind layered as `~/.overmind.env`, `./.overmind.env`,
`./.env`. turborust has only inline `env` tables, so moving from any of them
means copying secrets into a committed config file, which is the one thing a
`.env` file exists to prevent.

## Precedent

foreman `--env`, honcho, overmind, Docker Compose `env_file`, watchexec
`--env-file`, mise `env._.file`.

## Proposal

`env_file = ".env"` per node and at project level, later files overriding
earlier and inline `env` overriding both. Missing files are an error unless
marked optional (`env_file = { path = ".env.local", required = false }`), so a
typo is not a silent empty environment — the lesson of `0086`.

The cache needs care, and is why this is not trivial: under strict environments
(`0014`) a value that reaches a task must be part of its key. Variables loaded
from a file are declared by that file, so they are hashed like inline `env`;
the file's *path* is not an input, its values are.

## Acceptance criteria

- [x] `env_file` loads `KEY=value` files for a node and for the project, with documented precedence
- [x] A missing required file is an error naming the node and the path
- [x] Values from an env file participate in task cache keys; changing one is a miss
- [x] Syntax matches what dotenv tools accept: comments, quotes, `export` prefixes

## 2026-10-28

`env_file` takes a path, `{ path, required = false }`, or a list of either, under
`[project]` and per node. Paths are relative to the workspace root. Precedence:
project files, then the node's, then inline `env`.

Files are read when a node starts, not once at plan time, because `up` never
re-resolves its plan: read once, an edit to `.env` would have needed a turborust
restart to take effect. The plan still loads every file once up front, so a
missing or malformed one is refused before anything runs (the `0086` rule).
That made `child_env` fallible; a file that disappears mid-session fails the
node's spawn with its path rather than starting it with less environment.

Keying: values from files are not in the config, so the config's hash cannot
cover them. Each one is keyed as `env_file:KEY` with a truncated blake3 of its
value, never the value: `.env` is where secrets live, and `why` prints key
metadata. Strict tasks see file variables, since the file declared them.

No `${VAR}` expansion. dotenv and Compose disagree about it, and a value that is
exactly what the file says is the one nobody can misread.

Mutation-checked: not keying, not reaching the child, keying the raw value and
not checking at plan time each fail a test.
