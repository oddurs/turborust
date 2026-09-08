---
id: 55
title: 'Not doing: asset pipeline'
type: feature
status: dropped
milestone: later
created: 2026-09-08
updated: 2026-09-08
area: assets
---

## Considered and declined

cargo-leptos carries `tailwind_config_file`, `tailwind_input_file`, `style_file`,
`js_minify`, `wasm_opt_features`, `precompress`, `hash_files` and `browserquery`.
trunk has an equivalent set. It is a natural thing to want from a tool that serves
a frontend.

## Why not

Those belong to the framework's build tool, and turborust orchestrates that tool.
Building a second sass compiler and wasm-opt invocation would mean tracking every
framework's conventions forever, and being wrong about them between releases.

The correct shape is already available: declare `trunk build` or `cargo leptos
build` as a task with its inputs and outputs, and turborust caches it, gates on
it, and explains it. That gets the benefit without owning the pipeline.

Reconsider only if a framework has no build tool of its own and users are
hand-rolling one — which has not happened.
