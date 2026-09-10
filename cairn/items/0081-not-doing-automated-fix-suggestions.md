---
id: 81
title: 'Not doing: automated fix suggestions'
type: feature
status: dropped
milestone: later
created: 2026-09-10
updated: 2026-09-10
priority: p2
area: scope
---

## Considered and declined

Nx Cloud's self-healing CI analyses a failed task, proposes a fix, verifies it
and posts it to the pull request. It is the most-promoted feature in this
category right now.

## Why not

This tool's discipline is to say true things precisely: `why` reports the file
and the hash that caused a rebuild and stops there; `doctor --fix` applies only
changes it can state exactly, with consent (`0011`). A guessed fix is the
opposite posture, and being confidently wrong about someone's build is worse
than being silent — the whole reason `0014` was a p0 is that a tool which is
occasionally wrong quietly is worse than one that is honestly unhelpful.

There is also nowhere to put it. It needs a hosted model and a CI integration,
which means `0080` first, which is declined.

## What is in scope, and is the same want

Make the truth machine-readable, so an agent the developer chose can act on it
with full context. `0078` makes the output a contract and `0077` puts it behind
MCP. That is the same benefit with the judgement left where it belongs.
