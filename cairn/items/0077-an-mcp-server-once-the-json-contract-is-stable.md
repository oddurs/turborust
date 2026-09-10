---
id: 77
title: An MCP server, once the JSON contract is stable
type: feature
status: backlog
milestone: later
depends_on:
- 78
created: 2026-09-10
updated: 2026-09-10
priority: p3
effort: l
area: cli
---

## Problem

Automated consumers are already driving this tool — `0060` came from one — and
they currently do it by running commands and reading output that was designed
for a person. The questions they want to ask are exactly the ones this tool is
built to answer: what is in the graph, what is failing, why did this rebuild,
what is the state of the tree right now.

Turborepo shipped agent skills and `turbo docs` in 2.8 for the same reason.

## What should happen

An MCP server exposing the read-only surface — `plan`, `why`, `state`, `graph`,
recent output — plus `run` behind an explicit opt-in, over the control socket
that `0041` already built. `connect` proved the socket can carry an out-of-band
client; this is a second one that speaks a different protocol.

## Blocked on 0078, deliberately

This is filed after the JSON contract on purpose. An MCP server is a second
consumer of shapes that are currently undocumented and unversioned, and adding
it first would mean freezing those shapes by accident rather than by decision.

## The opinion, since this is the fashionable item

Most of the value here is not the protocol. It is that the answers are good:
`why` already produces a better explanation of a rebuild than any other build
tool exposes, to anyone or anything. A stable `--json` gets ninety percent of
the benefit to any consumer that can run a subprocess, which is all of them.
MCP is worth adding on top of that, and worth nothing without it.

## Acceptance criteria

- [ ] `turborust mcp` serves the read-only surface over the existing control
      socket
- [ ] Mutating tools are opt-in and off by default
- [ ] It reuses the `0078` schemas rather than defining a second set
- [ ] It works against a running `up` and degrades usefully when none is running
