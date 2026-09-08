---
id: 53
title: turborust graph
type: feature
status: done
milestone: later
assignee: Oddur Sigurdsson
depends_on:
- 46
created: 2026-09-08
updated: 2026-09-08
priority: p3
effort: s
area: cli
---

## Problem

`plan` prints the resolved nodes as a list. For anything past a handful of nodes
the shape of the graph — what is parallel, what is a bottleneck — is not visible
in a list.

## Proposal

`turborust graph [--format dot|mermaid]`, emitting the node graph with services
and tasks distinguished and derived-from-cargo edges marked. Mermaid because it
pastes into a README or a PR comment and renders; dot because it is what people
pipe into `dot -Tsvg`.

Optionally colour by cache status when combined with `--dry-run` (0046), which
turns the graph into a picture of what a run would actually do. That is the
version worth having; the plain graph is a stepping stone.

## Acceptance criteria

- [x] `graph` emits valid mermaid and dot
- [x] Services and tasks are visually distinct
- [x] The output renders without hand-editing

## 2026-09-08

Mermaid and dot, with services and tasks distinguished by SHAPE rather than a legend — rounded for services, square for tasks — so the picture explains itself. Nodes whose globs came from cargo say so under their name, which is the thing people most often doubt.

Mermaid node ids are sanitised while labels keep the real name, because a hyphen in an id breaks the mermaid parse. `web-ui` renders as `web_ui(["web-ui"])`. Dot quotes names instead, so it needs no such transform. Both have a test, since this is exactly the kind of thing that works on the demo and breaks on a real project.

`--with-cache` reuses the 0046 prediction to colour nodes green or amber, turning the output from a diagram of the config into a diagram of the next run. Without it no classDef is emitted at all — unused styling is noise, and there is a test asserting its absence.

Verified both formats against the demo workspace and confirmed the cache colouring appears only when asked for.
