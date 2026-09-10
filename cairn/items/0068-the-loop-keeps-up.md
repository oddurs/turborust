---
id: 68
key: v1.4
title: The loop keeps up
type: milestone
status: backlog
depends_on:
- 67
created: 2026-09-10
updated: 2026-09-10
due: 2027-11-01
---

The dev loop moved while this tool was being built.

Rust frontends now hot-patch themselves: `dx serve` and `subsecond` swap
functions into a running process without restarting it, and `cargo leptos watch`
and `trunk serve` have always done their own watching. An orchestrator that
answers every file change with a restart is now actively destroying the state
those tools work to preserve.

Tests moved too. `cargo nextest` is fast enough to run on save, and this tool
already knows which crates a changed file reaches — but a failing test still
shows up as unstyled text in a log pane while a failing `rustc` diagnostic gets
a code frame, a file link and a takeover in the browser.

And `doctor` still reads configuration files. It can tell you that your linker
is slow. It cannot tell you that you spent forty minutes today waiting on one
node whose cache hit rate is four percent.
