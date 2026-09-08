---
id: 32
title: Browser dev overlay
type: feature
status: done
milestone: v0.0
created: 2026-09-08
updated: 2026-09-08
area: overlay
effort: l
---

A crab in the corner of the served page: issues, processes, last build and merged
output, with a build failure taking over the viewport at 94% over a blur so a
ghost of the app stays visible.

Issues are real rustc diagnostics parsed out of cargo's output — message, code,
`file:line:col`, and the code frame with rustc's own hint — with the location as a
`vscode://` link. The last-build drawer is `turborust why`, in the browser.

Configured through `[overlay]`, shaped like Turbopack's `devIndicators`, with
every value also overridable from the overlay's own preferences pane.

It mounts in a shadow root and loads no web fonts: an injected overlay that
inherits a global `* { box-sizing }` from the host app is unshippable, and pulling
Google Fonts into a page you do not own is rude.
