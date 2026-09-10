---
id: 22
title: turborust has no LICENSE
type: chore
status: done
milestone: v0.1
assignee: Oddur Sigurdsson
created: 2026-09-08
updated: 2026-09-08
priority: p1
effort: s
area: project
---

## Problem

There is no LICENSE file and no `license` field in `Cargo.toml`. As it stands
nobody can legally use, fork or depend on this — the default is all rights
reserved.

It also matters for what we are already doing: the audit that produced items
0014-0021 read Apache-2.0 and MIT projects for design, and we are considering
depending on `process-wrap` (Apache-2.0). Depending on permissive crates is
routine, but the project needs its own terms before any of that gets published.

## Proposal

MIT OR Apache-2.0, the Rust ecosystem default. It is compatible with every crate
we depend on and with every project audited.

Record the audit's licence hygiene alongside it: `bacon` is AGPL-3.0 and was
deliberately excluded from the audit — only its user-facing docs were read, never
its source — because reading copyleft source and then writing adjacent code into a
permissive project is a contamination risk. That decision should survive in the
repository, not just in a chat log.

## Acceptance criteria

- [x] LICENSE-MIT and LICENSE-APACHE at the repository root
- [x] `license = "MIT OR Apache-2.0"` in Cargo.toml
- [x] A short `NOTICE` or CONTRIBUTING note recording the AGPL exclusion

## 2026-09-08

Audit provenance, 2026-09-08. Read for design only; no code copied. Sources and licences: watchexec (Apache-2.0) — supervisor priority queue, process-wrap, on-busy-update; turborepo (MIT) — TaskHashable/GlobalHashable composition, env modes, output archiving; cargo-leptos (MIT) — tiered reload protocol; dioxus (Apache-2.0) — subsecond binary patching; moon (MIT), trunk (Apache-2.0), overmind (MIT), salsa (Apache-2.0) surveyed. bacon (AGPL-3.0) deliberately excluded: docs only, source never opened.

## 2026-09-08

MIT OR Apache-2.0, the Rust ecosystem default: compatible with every crate we depend on and every project audited. Cargo.toml also gained description/repository/keywords/categories so it is publishable.

NOTICE.md records the audit provenance as a table and, more importantly, the bacon exclusion — docs only, source never opened. That decision needed to live in the repository rather than a chat log, which was the actual point of this item.
