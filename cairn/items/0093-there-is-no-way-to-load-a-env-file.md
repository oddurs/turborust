---
id: 93
title: There is no way to load a .env file
type: feature
status: backlog
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

- [ ] `env_file` loads `KEY=value` files for a node and for the project, with documented precedence
- [ ] A missing required file is an error naming the node and the path
- [ ] Values from an env file participate in task cache keys; changing one is a miss
- [ ] Syntax matches what dotenv tools accept: comments, quotes, `export` prefixes
