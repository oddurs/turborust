turborust is a dev orchestrator for full-Rust stacks: one binary that supervises
your services, watches the right files, caches your builds, and tells you exactly
why anything re-ran.

**How to read this.** `v0.0` is what already exists, recorded after the fact so
this file reflects reality rather than intentions — including the bugs found while
building it, which are kept because each note is the cheapest way to stop the bug
being reintroduced. `v0.1` is what has to be true before this is worth dogfooding.
`v1.0` is what has to be true before anyone else should depend on it.

**Where it stands.** Every milestone is closed. The runner works end to end — a
change to a shared crate restarts the backend, the browser reloads, and `turborust
why` names the file and the hash that caused it. The cache is trustworthy: a strict
filtered environment means an undeclared variable cannot change a build without
changing its key, a hit replays archived outputs rather than assuming the tree was
untouched, and the toolchain and config participate in every key. Results can be
shared between machines, read-only by default.

The supervision tree is covered by an integration harness rather than by a shell
script, which is the change that stops the next race being found by luck.

Two things are deliberately *not* claimed. The Windows process-tree teardown is
written and compile-checked against the real target but has never been run; CI
covers it on `windows-latest` and that is where it will first be exercised. And
`0023` records why hot patching stays out of scope: it is a binary-patching
problem, not an orchestration one.

**Out of scope, on purpose.** Five things comparable tools ship that this one will
not: asset pipelines (`0055`), toolchain installation (`0056`), codegen /
codeowners / docker / VCS hooks (`0057`), telemetry (`0058`), and a background
daemon (`0059`). Each is filed as a dropped item with its reasoning, so the
decision is discoverable rather than folklore. The test applied throughout: does
the feature need the dependency graph or the fingerprint? If not, it belongs in a
different tool.

**Provenance.** Items `0014`–`0023` came from an audit against watchexec,
turborepo, moon, cargo-leptos, trunk, dioxus, overmind and salsa — read for design
only, never copied. `bacon` was deliberately excluded: it is AGPL-3.0, and reading
copyleft source before writing adjacent code into a permissive project is a
contamination risk. See `0022`.
