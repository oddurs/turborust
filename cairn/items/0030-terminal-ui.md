---
id: 30
title: Terminal UI
type: feature
status: done
milestone: v0.0
created: 2026-09-08
updated: 2026-09-08
area: tui
effort: m
---

Sidebar of nodes with status glyphs, merged log pane with per-node focus, and a
keybar. Renders `AppState` and holds no supervisor state of its own, so `--no-tui`
and the TUI execute identical supervision logic.

Child ANSI is rendered faithfully rather than stripped, and `\r`-terminated
progress lines overwrite their predecessor instead of accumulating — without that,
a five-minute cargo build fills the scrollback with thousands of near-identical
frames and the actual errors scroll away.
