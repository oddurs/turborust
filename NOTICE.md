# Notices

turborust is licensed under either of [Apache License, Version 2.0](LICENSE-APACHE)
or [MIT license](LICENSE-MIT), at your option.

## Third-party source read during design

The design of the supervisor, the cache key, and the reload protocol was informed
by reading other projects. **No code was copied from any of them.** Where a design
is borrowed, the item that borrows it names the source.

| Project | Licence | What was read |
|---|---|---|
| watchexec | Apache-2.0 | supervisor control queue, process grouping, on-busy-update |
| turborepo | MIT | task/global hash composition, env modes, output archiving |
| cargo-leptos | MIT | tiered browser reload protocol |
| dioxus | Apache-2.0 | binary hot-patching (`subsecond`) — surveyed, not adopted |
| moon, trunk, overmind, salsa | MIT / Apache-2.0 | surveyed |

## Deliberate exclusion

**`bacon` (AGPL-3.0) was excluded from the audit.** Only its user-facing
documentation was read; its source was never opened. Reading copyleft source and
then writing adjacent code into a permissively licensed project is a contamination
risk, and no finding was worth taking it.

This is recorded here rather than left in a chat log so the decision survives.
