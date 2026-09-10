---
id: 34
title: Watching died silently when the watcher moved into a spawned task
type: bug
status: done
milestone: v0.0
created: 2026-09-08
updated: 2026-09-08
area: watch
effort: s
---

## What happened

`tokio::spawn(async move { w.rx.recv().await })` compiled and ran, but under
edition-2021 disjoint capture the async block captured *only* `w.rx`, leaving the
debouncer guard behind to be dropped when the enclosing function returned. The OS
watch then stopped with no error and no closed channel — just a dev loop that
never rebuilt again.

Cost about an hour to find, because every isolated reproduction worked: a probe
binary with the same code kept its guard alive by accident of scope.

## Fix

Made the mistake unwriteable rather than remembered. `rx` is private and access
goes through `Watcher::recv(&mut self)`, which forces the whole struct to be
captured. The old pattern is now a compile error.

Regression test: `watch::guard_lifetime_tests` hands a watcher to a spawned task
from a function that then returns, and asserts events still arrive.
