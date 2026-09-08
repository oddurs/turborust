---
id: 28
title: Derive watch and input globs from the cargo dependency closure
type: feature
status: done
milestone: v0.0
created: 2026-09-08
updated: 2026-09-08
area: cargo
effort: m
---

`cargo = "api"` asks `cargo metadata` for that crate's path-dependency closure and
derives the watch and input globs from it.

This is the feature the whole tool is built around. The alternative — hand-written
globs — produces the shared-crate bug every time: you list `crates/api/**`, forget
`crates/shared/**`, and lose ten minutes to a phantom. The truth lives in
Cargo.toml, so we ask cargo instead of guessing.

Verified end to end: editing `crates/shared/src/lib.rs`, which no glob in the
config mentions, restarts the api.
