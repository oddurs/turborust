---
id: 57
title: 'Not doing: codegen, codeowners, docker and VCS hooks'
type: feature
status: dropped
milestone: later
created: 2026-09-08
updated: 2026-09-08
area: scope
---

## Considered and declined

moon ships crates for `codegen`, `codeowners`, `docker` and `vcs-hooks`.

## Why not

Those make it a monorepo platform. turborust is a dev loop: supervise, watch,
cache, explain. Every one of those four is a well-served problem with dedicated
tools, and none of them shares machinery with the supervision tree or the cache.

The test applied here: does the feature need the dependency graph or the
fingerprint? Codegen and codeowners do not. Docker and git hooks do not. If it
does not need what makes this tool distinctive, it belongs somewhere else.
