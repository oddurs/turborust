---
id: 108
title: Every pull request renders the roadmap
type: chore
status: done
created: 2026-10-29
updated: 2026-10-29
priority: p1
effort: s
area: workflow
part_of: [103]
---

## Problem

`ROADMAP.md` is generated from the items but committed, and CI required every
pull request to carry an up-to-date render. Two pull requests closing items
both rewrite the same progress line, so they could only land one after another,
each re-rendered on top of the last — a CI round per merge, and the reason
auto-merge could not be turned on. The pre-commit hook re-rendered and staged
the file too, so the conflict reached every branch that touched an item.

## Acceptance criteria

- [x] A workflow renders `ROADMAP.md` on `main` after a merge and commits it if it changed
- [x] Pull requests no longer render it, and fail if they edit it
- [x] The pre-commit hook validates items but no longer regenerates the file
- [x] AGENTS.md, CONTRIBUTING.md and the git-workflow doc say how it works now
