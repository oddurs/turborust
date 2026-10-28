---
id: 96
title: A crash loop never gives up
type: feature
status: backlog
created: 2026-10-28
updated: 2026-10-28
priority: p3
effort: s
area: engine
---

## Problem

A service that crashes on start is restarted with backoff capped at
`backoff_max` (30s), forever. Each crash is a failure notification. A typo in a
command that will never work keeps the loop — and the notifications — going
until someone stops it.

## Precedent

process-compose `availability.max_restarts`, systemd `StartLimitBurst`,
supervisord `startretries` ending in `FATAL`.

## Proposal

`max_restarts` (default unlimited, for compatibility) after which the node is
`failed` and stays there until an edit or a manual restart from the overlay.
Consecutive failures only: a service that ran for a while and then crashed has
not used up its retries.

## Acceptance criteria

- [ ] After `max_restarts` consecutive crashes the node stops being restarted and says so
- [ ] A watched-file change or a manual restart resets the count
