---
id: 31
title: Static server with live reload for wasm frontends
type: feature
status: done
milestone: v0.0
created: 2026-09-08
updated: 2026-09-08
area: serve
effort: m
---

In-process static serving with SPA fallback and browser live reload.

Two details that hand-rolled dev servers routinely get wrong, both handled:
`.wasm` is served as `application/wasm`, without which browsers refuse
`instantiateStreaming` and blame your code in the console; and a missing asset
404s rather than falling back to index.html, because returning HTML for a typo'd
wasm path produces a browser error that means nothing.

Reload is a full page reload, not HMR — see 0023 for why that is the honest
description, and 0018 for the tier that would preserve client state.
