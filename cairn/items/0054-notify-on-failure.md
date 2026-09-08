---
id: 54
title: Notify on failure
type: feature
status: done
milestone: later
assignee: Oddur Sigurdsson
created: 2026-09-08
updated: 2026-09-08
priority: p3
effort: s
area: ui
---

## Problem

A build that fails while you are in another window is discovered whenever you next
look. watchexec has `--notify` for this.

## Proposal

Opt-in desktop notification when a node fails or recovers, via `notify-rust`.

Off by default, and rate-limited. A crash-looping service must not emit a
notification per restart — that is how a helpful feature becomes the reason
someone uninstalls the tool. Notify on the transition into failure, and once on
recovery; say nothing while the state is unchanged.

## Acceptance criteria

- [x] Opt-in via config
- [x] Notifies on transition into failure and on recovery
- [x] A crash loop produces one notification, not one per restart

## 2026-09-08

Opt-in via `[project] notify = true`, off by default.

The whole design is the rate limiting, and it is done by tracking TRANSITIONS rather than states. A crash-looping service restarts every few hundred milliseconds; notifying per restart is how a helpful feature becomes the reason someone uninstalls the tool. The notifier remembers what it last said about each node and speaks only when that changes — one notice on entering failure, one on recovery, silence in between. A test asserts fifty consecutive failures produce exactly one notification.

A node that starts healthy says nothing: only a node that was previously failing is worth announcing as recovered.

Sending is best-effort and ignores errors. A headless machine, a missing notification daemon and a denied permission are all reasons not to notify, and none of them is a reason to interrupt a build.

The decision logic is separated from the sending so it is testable without a desktop — which is what let the rate limiting be verified at all.
