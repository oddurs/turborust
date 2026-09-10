---
id: 26
title: PTY-backed process supervision
type: feature
status: done
milestone: v0.0
created: 2026-09-08
updated: 2026-09-08
area: proc
effort: m
---

Children are spawned on a real pty rather than pipes, so `isatty()` is true and
cargo keeps its colour, its progress bars and its line buffering. The accepted
cost is that stdout and stderr are merged by the kernel and cannot be separated
again — the same trade Overmind makes.

Stopping signals the process *group*: `sh -c "cargo run"` spawns cargo which
spawns your binary, and killing only the shell leaves the binary holding the port.
SIGTERM, then SIGKILL after `stop_timeout`.
