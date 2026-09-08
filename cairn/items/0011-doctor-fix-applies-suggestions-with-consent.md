---
id: 11
title: doctor --fix applies suggestions with consent
type: feature
status: done
milestone: v1.0
assignee: Oddur Sigurdsson
created: 2026-09-08
updated: 2026-09-08
priority: p3
effort: s
area: doctor
---

## Problem

`doctor` prints exactly the snippet to paste and deliberately writes nothing —
build configuration is the user's call. But copying four lines into
`.cargo/config.toml` by hand is friction, and the linker finding is the single
biggest iteration-speed win available.

## Proposal

`doctor --fix` shows a diff per finding and applies only what is confirmed.
Default stays read-only; there is no `--yes`.

## Acceptance criteria

- [x] Each finding is shown as a diff before anything is written
- [x] Declining one finding still offers the rest
- [x] Existing config is merged, never overwritten
- [x] Without `--fix`, behaviour is unchanged

## 2026-09-08

doctor --fix walks the applicable warnings one at a time, shows the exact lines it would add, and writes only what is confirmed. There is no --yes, deliberately: these findings change how your code is built, and a flag that applied them all unseen would be the opposite of the point.

Two implementation choices worth recording:

1. Appending rather than parse-merge-rewrite. Round-tripping a user Cargo.toml through a TOML serializer discards their comments and formatting, which is a rude thing to do to a file you were asked to help with. A test asserts the original bytes survive and a trailing comment is still there afterwards.

2. If the target table already exists, the fix declines rather than merging. Someone who already wrote [profile.dev] chose those values; appending a second table would be a duplicate key and rewriting theirs would be worse. A test asserts a deliberate debug = 2 is left alone.

Declining one finding does not skip the rest, per the acceptance criteria. Without a terminal the whole thing declines with an explanation instead of hanging on a read that will never be answered — verified by running it with stdin closed.

Snippets written to disk are stripped of the comment lines shown on screen, so the install hint for lld does not end up in .cargo/config.toml. That has its own test, since it is the kind of thing that silently rots.
