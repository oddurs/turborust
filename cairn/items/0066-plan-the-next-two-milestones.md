---
id: 66
title: plan the next two milestones
type: chore
status: done
created: 2026-09-10
updated: 2026-09-10
priority: p2
---

## Problem

Everything through v1.2 is closed. The tracker has three open items — a
workflow bug, a Windows gap and a docs site — and no answer to "what is this
tool for next". A roadmap that has been fully delivered is not a roadmap.

## What was done

Surveyed the adjacent tools across four paradigms and filed what survived
contact with this project's scope test.

**Monorepo orchestrators** — Turborepo 2.5-2.10, Nx, moon 2.3. Three ideas worth
taking: keying downstream work on an upstream's outputs rather than its key
(moon 2.3), a size-bounded local cache (Turborepo 2.10), and worktree support
(Turborepo 2.8). One idea worth taking in inverted form: Nx detects flaky tasks
statistically across a CI fleet, where a content-addressed cache can simply
prove non-determinism locally in two runs.

**Rust dev loop** — dioxus `dx serve`, `subsecond`, `cargo leptos watch`,
`trunk serve`, `cargo nextest`, bacon. The important change since `0023`: Rust
frontends now hot-patch themselves, and an orchestrator whose answer to every
edit is a restart destroys the state those tools exist to preserve.

**Kubernetes inner loop** — Tilt, Skaffold, DevSpace, Garden. Declined wholesale,
but Tilt's separation of build status from runtime status is exactly what a
self-reloading node needs and was taken.

**Agent-facing tooling** — Turborepo's agent skills and `turbo docs`, MCP
generally. Taken with a precondition: the JSON contract has to exist before
anything else consumes it.

## What was filed

Two milestones. **v1.3 "A cache you can audit"** (`0067`) makes the load-bearing
claim precise, bounded, portable and provable: `0069` `0070` `0071` `0072`.
**v1.4 "The loop keeps up"** (`0068`) closes the gap that opened while this was
being built: `0073` `0074` `0075` `0078`.

Two deferred: `0076` deferred input hashing, `0077` an MCP server behind `0078`.

Four declined, in the style of `0055`-`0059` so the reasoning survives: remote
execution (`0079`), a hosted cache service (`0080`), automated fix suggestions
(`0081`), container orchestration (`0082`).

## The one that matters most

`0073`. The others are improvements; that one is a hole in a claim on the front
page. "A dev orchestrator for full-Rust stacks" stops being true when the Rust
frontend tools reload themselves and this tool cannot represent it.

## Acceptance criteria

- [x] Landscape surveyed across orchestrators, the Rust dev loop, cluster dev
      tools and agent tooling
- [x] Two milestones with items under each
- [x] Everything declined is filed with its reasoning, not dropped silently
- [x] `cairn check` passes and `ROADMAP.md` is regenerated
