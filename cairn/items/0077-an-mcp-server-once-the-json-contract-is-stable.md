---
id: 77
title: An MCP server, once the JSON contract is stable
type: feature
status: done
milestone: later
depends_on:
- 78
created: 2026-09-10
updated: 2026-10-29
claimed: 2026-10-29
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

- [x] `turborust mcp` serves the read-only surface over the existing control
      socket
- [x] Mutating tools are opt-in and off by default
- [x] It reuses the `0078` schemas rather than defining a second set
- [x] It works against a running `up` and degrades usefully when none is running

## 2026-10-29

`turborust mcp`: MCP over stdio, newline-delimited JSON-RPC 2.0. Only what a tool server needs (`initialize`, `ping`, `tools/list`, `tools/call`) is written by hand in `src/mcp.rs`; no MCP crate was available, and the protocol subset is small enough that one would be a dependency for its own sake. It negotiates 2025-06-18, 2025-03-26 or 2024-11-05, and offers the newest to a client asking for anything else.

- **Tools.** `state` and `history` ask a running `up` over the control socket, which gained a `State` request returning the overlay's snapshot. `dry_run` runs `turborust run --dry-run=json` as a subprocess, so it works with no `up` at all. `run` exists only under `--allow-run`; without it, a call to `run` is an unknown tool, not a refusal to argue with.
- **No second set of shapes.** Every tool's `outputSchema` is `turborust schema --json <output>` for the payload it returns, and the result's `structuredContent` is that payload (the test compares them byte for byte). Subprocesses run with `--error-format json`, so a failure reaches the agent as a code and a message, not scraped prose.
- **Degrading.** With no `up`, `state` and `history` return a tool error that says so and names `dry_run` as what still works. A tool error rather than a protocol error, so the model reads it.
- **The opinion in this item held up in the building.** Almost all of the server is plumbing to payloads that already existed; the work that made this cheap was 0078's.

Verified by hand against a live `up`: `state` shows `api` healthy; `history` shows `starting → spawned → healthy`; `run` on a task that panics returns exit 101 with the panic as an issue at `src/x.rs`. `an_agent_gets_contracted_answers_and_no_way_to_run_unless_allowed` pins the protocol, the schemas, the degraded answer and the opt-in.

