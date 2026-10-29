---
id: 109
title: The roadmap render cannot push to main
type: bug
status: done
created: 2026-10-29
updated: 2026-10-29
priority: p1
effort: s
area: workflow
part_of: [103]
---

## Problem

`0108` moved the roadmap render out of pull requests into a workflow that
commits `ROADMAP.md` to `main` after each merge. Its first run was rejected:
`main` is protected, "changes must be made through a pull request" with three
required status checks, and a bot push is neither. A pull request opened by the
workflow would not help either: one opened with the workflow's token triggers no
workflows, so its required checks would never report and it could never merge.

## Acceptance criteria

- [x] The rendered roadmap is published somewhere a bot may write, after every merge that touches items
- [x] `ROADMAP.md` is no longer tracked on `main`, so no pull request can conflict on it
- [x] Every link to it points at the published copy
- [x] Item files whose titles changed are renamed to match, which `cairn check` warned about in that run

## 2026-10-29

Published on a `roadmap` branch: `main` plus one commit adding the render,
replaced by force on every run, so the branch never accumulates a history and
always reads as "main, rendered". Nothing else writes to it. `ROADMAP.md` is
gitignored on `main`, so a local `cairn render` stays a preview, and CI fails a
pull request that tracks it.

The rejected run said what else was wrong: two item files no longer matched
their titles — `0101`, retitled when its real cause was found, and `0103`.
Both renamed.
