---
id: 45
title: 'Run only what changed: --affected'
type: feature
status: done
milestone: v1.2
assignee: Oddur Sigurdsson
depends_on:
- 44
- 49
created: 2026-09-08
updated: 2026-09-08
priority: p1
effort: l
area: cli
---

## Problem

CI rebuilds everything on every push. The cache helps, but the graph is still
walked, every fingerprint is still computed, and every unchanged task still logs.
On a large workspace the honest answer is "most of this run was pointless".

turborepo has `--affected` and a base/head pair; moon has a whole `affected`
crate. It is the single feature that makes a build tool viable on a build server.

## Design

```
turborust run --affected              # since the merge-base with the default branch
turborust run --affected --base HEAD~1
```

Steps:

1. Ask git for changed paths: `git diff --name-only <base>...HEAD`, plus
   `git status --porcelain` so uncommitted work counts. Use the three-dot form so
   the comparison is against the merge base, not the tip — otherwise every commit
   on the base branch marks everything affected.
2. Match those paths against each node's input/watch matchers. The matchers
   already exist and already come from the cargo dependency closure, which is what
   makes this accurate for Rust: touching `crates/shared` marks every consumer
   affected without anyone maintaining a list.
3. Expand to dependents through the reverse index from 0044.

**Global inputs are the trap.** A change to `Cargo.lock`, `turborust.toml`, or the
toolchain affects everything, because they are in the global hash. `--affected`
must treat those as marking the whole graph, or it will confidently skip work that
genuinely changed. This is the kind of thing that produces a green CI run and a
broken artifact.

Degrade honestly: not a git repo, or git missing, means `--affected` errors rather
than silently selecting everything or nothing.

**This composes with the cache rather than replacing it.** Affected decides what
is *considered*; the cache decides what actually *runs*. Both, not either.

## Acceptance criteria

- [x] `--affected` selects nodes whose inputs changed since the base
- [x] Merge-base semantics (three-dot), not tip-to-tip
- [x] Uncommitted changes count
- [x] Dependents of an affected node are affected
- [x] A change to a global-hash input marks everything affected
- [x] Outside a git repo it errors clearly instead of guessing

## 2026-09-08

Ask git what moved, match it against node input globs, expand to dependents.

The accuracy is inherited rather than built: input globs come from the cargo dependency closure, so touching crates/shared marks every consumer affected without anyone maintaining a list. Verified — editing only the shared crate selects both shared_check and api_check, though no file under crates/api changed.

Three-dot diff, not two. Two dots would mark everything affected the moment the base branch moved ahead, which is most of the time in CI. Uncommitted changes count too, since they are the most likely thing to be broken.

The trap the design flagged is handled and tested: Cargo.lock, turborust.toml and rust-toolchain.toml feed the global hash, so they change every key while matching no per-node glob. Missing that produces a green run and a broken artifact. They short-circuit to selecting everything.

--filter and --affected INTERSECT rather than union. Both are narrowing flags; unioning them would make each widen the other, which is the opposite of what someone combining them means. Positional targets and --filter still union, because both are naming things to include.

Outside a git repo it errors and says so rather than defaulting to everything or nothing — guessing either way is worse than refusing.
