---
id: 10
title: Opt-in per-node CARGO_TARGET_DIR
type: feature
status: done
milestone: v1.0
assignee: Oddur Sigurdsson
created: 2026-09-08
updated: 2026-09-08
priority: p2
effort: m
area: cargo
---

## Problem

cargo takes an exclusive lock on the target directory, so two nodes building at
once serialise. turborust surfaces this ("waiting: cargo lock") rather than hiding
it, which is right — but surfacing is not solving. On a workspace with a backend
and a wasm frontend, the two builds always contend.

## Proposal

Opt-in `target_dir = "split"` on a node, giving it its own `CARGO_TARGET_DIR`.

The trade is real and must be documented, not glossed: separate target dirs mean
dependencies are compiled once per node and disk usage multiplies. It is a win for
a wasm frontend (different target triple, so nothing is shared anyway) and usually
a loss for two native binaries in one workspace.

## Acceptance criteria

- [x] `target_dir = "split" | "shared"` per node, defaulting to shared
- [x] `turborust doctor` reports the disk cost when split is in use
- [x] The trade-off is documented in the README, including when not to use it

## 2026-09-08

target_dir = "split" | "shared" per node, shared by default.

A split node gets CARGO_TARGET_DIR pointing at target/turborust/<node>, deliberately kept UNDER the workspace target dir so cargo clean and existing .gitignore rules still cover it. Putting it elsewhere would leave artifacts nobody expects and nothing cleans.

It participates in the cache key, because splitting moves where artifacts land and a task keyed without it could replay a result whose outputs are somewhere else.

doctor prices the choice rather than just permitting it: it reports which nodes are split and how much target/turborust currently holds, with the trade stated plainly — splitting pays for a wasm frontend, which compiles for a different target and shares nothing anyway, and usually does not pay for two native binaries in one workspace. The README says the same.

Verified: a shared task sees CARGO_TARGET_DIR unset, a split task sees its own path.

Left as-is on purpose: turborust still surfaces cargo lock contention as "waiting: cargo lock" for shared nodes. Splitting is opt-in because the default should be the one that does not silently multiply disk usage.
