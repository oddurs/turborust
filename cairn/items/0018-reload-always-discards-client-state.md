---
id: 18
title: Reload always discards client state
type: feature
status: done
milestone: v1.0
assignee: Oddur Sigurdsson
created: 2026-09-08
updated: 2026-09-08
priority: p2
effort: m
area: serve
---

## Problem

Every reload is `location.reload()`. Change a stylesheet and the running app is
thrown away — the route you were on, the form you had half-filled, the panel you
had open. For a CSS tweak that is pure loss.

## Proposal

cargo-leptos (MIT; design only) tiers its reload messages: a full reload, a CSS
swap, and a view update. The CSS path replaces the stylesheet link in place and
keeps the page alive.

Take the first two tiers, which are the ones that do not require framework
cooperation:

- a changed file matching only asset globs (`**/*.css`, images) sends `css`
- anything else sends `full`

The overlay's reload client already has the connection; this is a message type,
not new plumbing.

## Acceptance criteria

- [x] A CSS-only change swaps the stylesheet without a page reload
- [x] A wasm or HTML change still does a full reload
- [x] Client state survives a CSS change
- [x] The overlay reports which kind of reload it performed

## 2026-09-08

Reload is now tiered. A change touching only stylesheets broadcasts "css"; anything else — Rust, HTML, wasm — broadcasts "full".

The CSS path clones each stylesheet link with a fresh query string and removes the old one only once the replacement has LOADED, so the page never flashes unstyled. Third-party stylesheets on other origins are left alone: re-requesting them would be slow and pointless.

The decision is made in watch dispatch, where the changed paths are actually known, rather than at the node that happens to finish last.

Deliberately not attempted: cargo-leptos view updates, the third tier. That needs framework cooperation to patch a component tree, and turborust orchestrates frameworks rather than participating in them. The two tiers that work without cooperation are the two that are here.
