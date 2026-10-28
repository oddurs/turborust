---
id: 99
title: 'Not doing: replicas and scaling'
type: feature
status: dropped
milestone: later
created: 2026-10-28
updated: 2026-10-28
priority: p3
area: scope
---

## Considered and declined

process-compose runs `replicas` of a process and scales them at runtime
(`process scale`, F2 in its TUI); pm2 does the same for Node.

## Why not

It answers a deployment question. A dev stack runs one of each thing, and the
cases for more than one — load testing, a queue with several workers — are
served better by the program's own concurrency setting than by turborust
multiplying processes that then share a port, a log and a readiness edge.
Scaling is the first step towards the orchestration `0082` declines.
