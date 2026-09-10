---
id: 69
title: Downstream rebuilds when an upstream re-runs with identical output
type: bug
status: backlog
milestone: v1.3
created: 2026-09-10
updated: 2026-09-10
priority: p1
effort: l
area: cache
---

## Problem

A downstream node's cache key includes its upstreams' **keys**
(`engine.rs`, the `hashes` map: "Fingerprints computed this session, so a
downstream task's cache key includes its upstreams' keys").

So an upstream that re-runs always invalidates everything below it, whether or
not it produced anything different. Fix a typo in a doc comment in
`crates/shared`, and `check` correctly re-runs — its inputs did change — but
`build`, `test` and every dependent service rebuild too, on the strength of a
byte-identical artifact.

This is the most common shape of wasted time in a Rust workspace, because a
shared crate is exactly where edits land and exactly what everything depends on.

## What should happen

Key a downstream node on its upstream's **output fingerprint** when that
upstream declares `outputs`, and on the upstream's key when it does not.

No configuration knob. moon ships this as `dependencyCacheStrategy` with three
values (`hash`, `ignored`, `outputs`), which pushes a correctness decision onto
the user and defaults it wrong for precisely the tasks that matter. The
information needed to decide is already in the config: a task that declared its
outputs has told us exactly what it produces, so that is what to key on. A task
that declared none has nothing observable to offer, so its key is the only
honest proxy.

## The trade, stated plainly

A task whose real effect is not captured by its declared `outputs` — it writes
to a database, or mutates something in `target/` it never listed — will now
under-invalidate its dependents.

That is not a new trust assumption. `outputs` already carries exactly this
contract for cache *restore*: a hit replays the declared outputs and assumes
nothing else mattered. This change makes the same promise load-bearing in one
more place, which is an argument for documenting it harder, not for declining it.

## Acceptance criteria

- [ ] A downstream node is served from cache when an upstream re-runs and its
      declared outputs hash the same
- [ ] A downstream node rebuilds when an upstream's declared outputs change
- [ ] An upstream with no declared outputs keeps today's behaviour exactly
- [ ] `why` names which of the two rules applied, and for tasks keyed on outputs
      shows the upstream output hash rather than the upstream key
- [ ] The `outputs`-completeness contract is written down in the README next to
      the cache-key description
