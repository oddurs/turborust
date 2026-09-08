---
id: 8
title: Restart a process from the overlay
type: feature
status: done
milestone: v1.0
assignee: Oddur Sigurdsson
created: 2026-09-08
updated: 2026-09-08
priority: p2
effort: s
area: overlay
---

## Problem

The TUI can restart a node with `r`. The browser overlay can only watch. When the
frontend is the thing you are looking at, switching to a terminal to press one key
is the whole friction the overlay exists to remove.

## Proposal

A restart control on each row in the Processes drawer, posting to a new
`POST /__turborust/restart/:node`.

Note the security shape: this endpoint mutates state, so it must bind loopback
only (it already does) and reject cross-origin requests. A dev server that any
page on the machine can drive is not acceptable even in dev.

## Acceptance criteria

- [x] Each process row has a restart control
- [x] The endpoint refuses requests without a same-origin `Sec-Fetch-Site`
- [x] Restarting from the browser shows the same reason string as the TUI

## 2026-09-08

A restart control on each row in the Processes drawer, posting to POST /__turborust/restart/:node.

The security shape mattered more than the button. The endpoint mutates supervisor state, so it requires Sec-Fetch-Site: same-origin (or none, for a direct navigation) and returns 403 otherwise. Browsers set that header themselves and no page can forge it, which is exactly the property needed — the server already binds loopback, but "loopback" includes every other page open on the machine, and a dev server any of them can drive is not acceptable even in development.

Verified both directions: a plain curl gets 403, the same request with the header gets 202 and the restart appears in the log with the same reason string the TUI uses.

This required moving spawn_supervisors ahead of the serve tasks in cmd_up so the control handles exist when the server is built. Harmless — the serve task already waits for its directory to appear.
