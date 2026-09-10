---
id: 33
title: turborust doctor
type: feature
status: done
milestone: v0.0
created: 2026-09-08
updated: 2026-09-08
area: doctor
effort: m
---

Reports what is costing seconds on every rebuild: the default linker (incremental
rebuilds are link-bound — codegen is cached, linking is not), full DWARF in the
dev profile, unoptimized dependencies, `CARGO_INCREMENTAL=0`, and a `target/`
directory inside a cloud-synced folder.

Prints the exact snippet and writes nothing. Build configuration is the user's
call; 0011 covers applying it with consent.

This exists because the honest limit of the whole project is that it cannot make
rustc fast. It can avoid compiling when nothing changed and compile exactly the
right things — but the floor is your build, so the tool should at least name what
is holding the floor down.
