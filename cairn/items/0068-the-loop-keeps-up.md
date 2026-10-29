---
id: 68
key: v1.4
title: The loop keeps up
type: milestone
status: done
depends_on:
- 67
created: 2026-09-10
updated: 2026-10-29
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

## 2026-10-29

Closed: all nine items are done (`0073`-`0075`, `0078`, `0085`-`0089`). The three moves it was for:

- **Self-reloading processes are supervised, not restarted** (`0073`). Verified against a real `dx serve` 0.7.10 project, which led to `reloading.failed` for tools that print no rebuild-start line.
- **Failing tests are diagnostics** (`0074`), from `cargo test` and from nextest as it prints on a pty.
- **`doctor --profile` measures** (`0075`). It reports wall time, hit rate, why each task missed and what to change, from runs it logged on this machine.

Along the way: history readable from `why`, the control socket and `--summarize` (`0110`), and a versioned JSON contract (`0078`), with `0077` (MCP) still to follow it. Verified at close-out with fmt, clippy, `scripts/agent check` and the full suite green on this branch, on top of every other change in the milestone.
