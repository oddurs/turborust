# turborust

[![CI](https://github.com/oddurs/turborust/actions/workflows/ci.yml/badge.svg)](https://github.com/oddurs/turborust/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue)](#licence)

A dev orchestrator for full-Rust stacks. One binary, one config file: it supervises
your services, watches the right files, caches your builds, and tells you exactly
why anything re-ran.

It is **not** a bundler and not a compiler. It orchestrates the tools you already
use — `cargo`, `trunk`, `cargo leptos`, `dx` — and makes everything *around* the
compile instant.

## Why

The pieces exist separately and none of them compose:

| Tool | Supervises | Watches | Cached task graph | Explains reruns |
|---|---|---|---|---|
| Overmind / foreman | ✅ | ✕ | ✕ | ✕ |
| watchexec | ✕ | ✅ | ✕ | ✕ |
| Turborepo | ✕ | ✕ | ✅ | partial |
| cargo-watch | ✕ | ✅ | ✕ | ✕ |
| **turborust** | ✅ | ✅ | ✅ | ✅ |

## The part that matters for Rust

You name a crate. turborust asks `cargo metadata` for that crate's **path-dependency
closure** and derives the watch and input globs from it.

```toml
[services.api]
cargo = "api"      # no watch globs — they come from cargo
port = 8788
```

Edit `crates/shared/src/lib.rs` and the api restarts, because cargo says the api
depends on `shared`. Hand-written globs are how the shared-crate problem happens:
you list `crates/api/**`, forget `crates/shared/**`, and spend ten minutes
debugging a phantom.

## Commands

```
turborust up [targets…]     start services and keep them running (TUI)
turborust run <task>…       run tasks once, in order, honouring the cache
turborust why <task>        why would this run — or not — right now
turborust doctor            what is costing you seconds on every rebuild
turborust plan              the resolved graph, with derived globs
turborust connect <node>    attach this terminal to a running process
turborust init              scaffold a config from your Cargo workspace
turborust schema            JSON Schema for turborust.toml, for editor completion
turborust completions <sh>  shell completion script
turborust man               the man page
turborust clean             drop cached task results
```

### `why` is the point

```
$ turborust why check

  check
  inputs derived from crate `api` and its path deps: api, shared
  5 input files, key b3:614d4a52304e

  cache MISS — b3:9068e57b411c → b3:614d4a52304e

      ~ crates/shared/src/lib.rs  b3:5f16c070 -> b3:d33db911
```

Content hashes, not mtimes — so `git checkout` does not invalidate the world.

## Config

```toml
[tasks.check]
cargo = "api"                       # derives inputs from the crate closure
cmd = "cargo check -p api"

[services.api]
cargo = "api"                       # derives `cargo run -p api` + watch globs
port = 8788
depends_on = ["check"]
health = { http = "http://127.0.0.1:8788/healthz" }

[services.web]
depends_on = ["api"]
serve = { dir = "dist", port = 8789 }   # hosted in-process, with live reload
```

`run` and `up` share one supervision tree, so `depends_on` means the same thing in
both: a task waits for its service dependencies to pass their readiness probes,
and `run` stops whatever it started in reverse dependency order before exiting.

### Serving beyond localhost

```toml
[services.web]
serve = { dir = "dist", port = 8789, host = "0.0.0.0", open = true }
```

`host` binds beyond loopback so you can open the page on a phone; `open` launches
a browser once the server is actually listening. Add `tls = { cert = "...", key =
"..." }` for HTTPS, which some browser APIs require.

**Binding beyond loopback closes the mutating endpoints.** Same-origin is not
sufficient once the origin is routable — every page on the network shares it — so
the overlay's restart control returns 403 unless you also set
`allow_remote_control = true`. Wanting to test on a phone is not consent to let
the network restart your processes.

### One origin

```toml
[services.web]
serve = { dir = "dist", port = 8789 }
proxy = [
  { path = "/api", to = "http://127.0.0.1:8788" },
  { path = "/v2",  to = "http://127.0.0.1:8788", strip_prefix = true },
]
```

The page calls `/api/...` with a relative path, so there is no CORS to configure
in development and no API origin to swap out for production.

Prefixes match on segment boundaries — `/api` does not swallow `/apidocs` — and
the longest match wins. Hop-by-hop headers are dropped rather than forwarded, and
response bodies stream, so an SSE endpoint behind the proxy still streams.

When the backend is restarting the proxy answers **502 with
`x-turborust-proxy: upstream-unreachable`** and a `Retry-After`, rather than a raw
connection error that makes your app look broken when it is merely rebuilding.

### Talking to a running process

```
$ turborust connect api
```

Attaches your terminal to a supervised process: answer its prompt, use its REPL,
hit a breakpoint. **Ctrl-B then `d`** detaches and leaves it running; Ctrl-C is
deliberately *not* the detach key, because it has to reach the child. Window
resizes propagate, and the recent output is replayed on attach, so a process
sitting at a prompt shows the prompt rather than a blank screen.

One terminal per process — two clients typing into the same shell interleave into
nonsense. In the TUI, `i` sends keys to the selected process and `Esc` stops;
the footer says which mode you are in.

The socket lives in a per-user directory, mode `0600`, named by a hash of the
workspace. Not in `.turborust/`: Unix socket paths are capped near 104 bytes and a
workspace a few directories deep overflows it, failing with a bare `EINVAL`.

### Environment

A task's command runs in a **filtered environment** by default. Only a small safe
base (`PATH`, `HOME`, `CARGO_HOME`, …), the variables it declares, and its inline
`env` reach the child:

```toml
[tasks.build]
cmd = "cargo build"
env_keys = ["PROFILE"]              # reaches the child AND keys the task
pass_through_env = ["SSH_AUTH_SOCK"] # reaches the child, does NOT key it
env_mode = "strict"                  # the default; "loose" inherits everything
```

This is a correctness property, not a preference. A cache key can only account for
variables it knows about, so the only way to guarantee an undeclared variable did
not change the build is to make sure the build never saw it. A `loose` task is
never served from cache — being honestly uncacheable beats being occasionally
wrong.

The safe base is passed but not hashed: without `PATH` nothing runs, and hashing it
would miss whenever a shell rearranged itself. Swapping toolchains via `PATH` is
covered by the global hash (`0017`), which folds in `rustc -V` directly.

### Build parallelism

```toml
[project]
concurrency = 4   # default: the machine's parallelism, capped at 8
```

Without a limit, a twenty-crate workspace launches twenty cargo processes at once
and thrashes. The limit is on *building*, not running: a service holds a slot only
until it is ready, because holding one for its whole life would deadlock the graph
the moment `concurrency` services were up and a dependent still needed a slot.

A node held back shows `waiting: slot`, distinct from `waiting: <dep>` — a bounded
queue that looked like a stuck dependency would be the same confusion the
cargo-lock indicator exists to prevent.

### Build isolation

```toml
[services.web]
cargo = "web"
target_dir = "split"   # a private CARGO_TARGET_DIR for this node
```

cargo takes an exclusive lock on its target directory, so two nodes building at
once serialise — turborust surfaces that as `waiting: cargo lock` rather than
letting it look like a hang.

`split` removes the contention, and the trade is real: every dependency not shared
with the workspace target dir is compiled twice and stored twice. It pays for a
wasm frontend, which compiles for a different target and shares nothing anyway. It
usually does not pay for two native binaries in the same workspace. `turborust
doctor` reports what the split is costing on disk.

### Readiness

```toml
health = { tcp = 8788 }                              # connect
health = { http = "http://127.0.0.1:8788/healthz" }  # any 2xx/3xx
health = { log = "Serving at" }                      # the process says so
```

Every probe a node declares must pass, so `{ tcp = 8788, log = "ready" }` means
both. The log probe exists for the many dev processes that announce themselves on
stdout and have no port worth probing — `trunk`, `cargo leptos watch`, migrations.
It matches against ANSI-stripped output, and resets on every restart so a stale
announcement cannot mark a fresh process ready.

`depends_on` waits for **readiness**, not for "a process exists". A dependent
starts only once its dependency's health probe passes, and stops when that
dependency goes down — which is what makes rebuild cascades work with exactly one
restart per node.

## The overlay

Serving a frontend injects a dev overlay into it: a crab in the corner that stays
out of the way until something breaks.

```toml
[overlay]
enabled  = true
position = "bottom-right"   # bottom-left | top-right | top-left
emoji    = "🦀"
theme    = "auto"           # auto | dark | light
errors   = "overlay"        # overlay | badge | silent
shortcut = "ctrl+`"
```

It surfaces four things:

- **Issues** — real rustc diagnostics parsed out of cargo's output: message, error
  code, `file:line:col`, and the code frame with rustc's own hint. The location is
  a `vscode://` link. A build error takes over the viewport at 94% opacity over a
  blur, so a ghost of your app stays visible; fixing the error dismisses it.
- **Processes** — every supervised service and task, with health, port, uptime,
  pid, restart count, and the file that caused the last restart.
- **Last build** — cache hit or miss, the blake3 key, and the input hashes that
  changed. `turborust why`, in the browser.
- **Output** — merged process output with a per-process filter.

Design constraints worth knowing about:

- It mounts in a **shadow root**. An injected overlay that inherits a global
  `* { box-sizing }` or a stray `button` rule from the host app is unshippable.
- It loads **no web fonts**. Pulling Google Fonts into a page you don't own is
  rude and measurably slows it.
- The state stream is one SSE endpoint sampled at 250ms that only sends a frame
  when the payload actually differs, so an idle page costs nothing.

## Design notes

- **PTY-backed children.** Processes on pipes see `isatty() == false` and drop
  color, buffer output, and hide progress. Children get a real pty, so `cargo`
  looks the way it does in your own terminal. The cost: stdout and stderr are
  merged by the kernel and cannot be separated again.
- **Process-group signalling.** `sh -c "cargo run"` spawns cargo which spawns your
  binary. Killing only the shell leaves the binary holding the port. turborust
  sends SIGTERM to the group, then SIGKILL after `stop_timeout`.
- **Content-addressed cache keys.** blake3 over inputs, command, declared env
  vars, declared outputs, env mode, and upstream keys. Results are stored *by key*,
  so alternating between two branches hits both ways instead of overwriting.
  A hit replays the archived outputs rather than assuming the tree was untouched;
  results above 256 MiB are recorded but not archived, and fall back to checking
  the files are still present.
- **Cascade dispatch.** When one edit matches several nodes, only the upstream-most
  are signalled; dependents come back through the readiness cascade instead of
  restarting twice.
- **Full page reload, not HMR.** A wasm binary has no module boundaries left to
  patch. Calling it HMR would be a lie.

## Roadmap

Tracked with `cairn` as Markdown files under `cairn/items`, rendered to
[ROADMAP.md](ROADMAP.md) — reviewable in a pull request like anything else.

- **v0.0** — what already exists, recorded after the fact, including the bugs
  found while building it
- **v0.1** — what has to be true before this is worth dogfooding
- **v1.0** — what has to be true before anyone else should depend on it

`cairn board` for the live view, `cairn next` for what is ready to start.

The blocker is `0014`: undeclared environment variables produce wrong cache hits,
reproduced against the binary. A build cache that returns stale answers silently
undermines everything else here, so it comes before any new feature.

## Sharing a cache

```toml
[cache]
shared = "~/team/turborust-cache"   # read results from here too
push = false                        # …and contribute yours. Off by default.
```

Any directory both machines can see: a network mount, a synced folder, another
checkout. A hit in the shared store is copied into the local one on the way past,
so the round trip happens once.

**`push` defaults to false, and that is the security model.** A build cache maps
inputs to *outputs*, so anyone who can write to a cache you read from can hand
your build arbitrary artifacts — a poisoned entry is indistinguishable from a fast
one. Read from a shared cache only if you would let those people push to your
branch. Publishing to one is a further step, so it is a separate, explicit choice
rather than something that happens because you pointed at a directory.

## Hot reload, and why this is a full page reload

turborust reloads the page; for a stylesheet change it swaps the sheet in place
and keeps your app's state. It does not hot-patch running Rust.

That is not laziness, and it is worth knowing where the ceiling is. The only Rust
project shipping true hot patching is [dioxus](https://github.com/DioxusLabs/dioxus)
(`dx serve`). Its implementation is not dev-server code at all: it parses Mach-O
and wasm object files, walks symbol tables, and builds an address map that
`subsecond` uses to redirect function calls inside a running binary, with
per-architecture and per-platform linking paths.

So hot patching is a binary-patching problem, not an orchestration one. If you
want it today, use dioxus. If turborust ever grows it, the realistic route is to
integrate `subsecond` rather than to reimplement it — and that is a project in its
own right, not a feature.

## Honest limits

- turborust cannot make `rustc` fast. It can avoid compiling when nothing changed,
  compile exactly the right things, and overlap everything else — but the floor is
  your build. `turborust doctor` targets that floor directly.
- Shared caching is filesystem-based (a mount or synced folder). There is no
  HTTP cache server, and no authentication beyond the filesystem's own.
- macOS and Linux are fully exercised. On Windows, 165 of 167 tests pass in CI;
  the two that do not are the pty tests, which capture no child output there.
  Windows pty behaviour — and the job-object process teardown those tests would
  exercise — is therefore **unverified** rather than working. The integration
  suites are Unix-gated too, because their fixtures are POSIX shell and turborust
  spawns through `cmd /C` there; a configurable shell would fix that half (`0062`).
- Remote/shared caching is not implemented; the cache is local, and is blocked on
  the two items above.

## The site

`site/` is turborust's own homepage: a small Rust generator that renders
Markdown, orchestrated and served by turborust itself.

```sh
turborust up          # builds site/dist and serves it on :8790
```

A cached build task feeding a serve node — editing a page rebuilds and reloads,
editing only the stylesheet swaps it in place and keeps your scroll position.

## Contributing

```sh
scripts/setup     # hooks, commit template, tooling check
cargo test
```

See [CONTRIBUTING.md](CONTRIBUTING.md). `main` only advances through a pull
request, and `scripts/agent` drives the whole loop.

## Demo

```
cargo build
./target/debug/turborust -c examples/fullstack/turborust.toml plan
./target/debug/turborust -c examples/fullstack/turborust.toml up
```

A shared crate, a backend that depends on it, and a frontend served by turborust.
Edit `examples/fullstack/crates/shared/src/lib.rs` and watch the cascade.

## Licence

MIT OR Apache-2.0, at your option. See [NOTICE.md](NOTICE.md) for what was read
during design and what was deliberately not — `bacon` is AGPL-3.0 and was excluded
from the audit on purpose.
