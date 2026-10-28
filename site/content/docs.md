---
title: "Docs — turborust"
description: "Install turborust, write a config, and understand what it does."
---

<div class="doc-head"><div class="wrap narrow">

<p class="eyebrow">documentation</p>

# Docs

<p class="lede">Everything below fits in one config file. <code>turborust plan</code> shows what that file resolves to, including the globs it derived for you.</p>

</div></div>

<div class="doc-body"><div class="wrap narrow">

## Install

```
cargo install --locked --git https://github.com/oddurs/turborust
cd your-workspace
turborust init          # infers a starting config from your cargo workspace and Procfile
turborust plan          # see what it resolved to
turborust up            # start it
```

Each tagged release also carries prebuilt binaries for macOS, Linux (static) and
Windows, with checksums, build provenance, shell completions and a man page; from
the first release on, `cargo install turborust` works too. Rust 1.89 or newer.

`init` writes a config with a `#:schema` line, so an editor completes and
validates the file with no per-editor setup.

## Services and tasks

A **service** runs until stopped. A **task** runs once and is cached. They share
one dependency graph, so a service can wait on a task and a task can wait on a
service.

```
[tasks.check]
cargo = "api"
cmd = "cargo check -p api"

[services.api]
cargo = "api"
port = 8788
depends_on = ["check"]
health = { http = "http://127.0.0.1:8788/healthz" }
```

`cargo = "api"` is the important line. It asks `cargo metadata` for that crate's
path-dependency closure and derives the run command, the watch globs and the
rebuild edges from it. Hand-written globs are how the shared-crate bug happens:
you list `crates/api/**`, forget `crates/shared/**`, and lose ten minutes to a
phantom.

## Readiness

```
health = { tcp = 8788 }
health = { http = "http://127.0.0.1:8788/healthz" }
health = { log = "Serving at" }
health = { cmd = "pg_isready -h 127.0.0.1" }
```

Every probe a node declares must pass, so `{ tcp = 8788, log = "ready" }` means
both. The log probe exists for the many processes that announce themselves on
stdout and have no port worth probing.

**`depends_on` waits for readiness, not for a process to exist.** That distinction
is the whole point: a dependent starts only once its dependency answers, and
stops when that dependency goes down.

Readiness is checked on the way up; `liveness` keeps checking after:

```
liveness = { http = "http://127.0.0.1:8788/healthz", every = "10s", failures = 3 }
```

Three misses in a row make the service unhealthy and tell its dependents; an
answer brings it back. With `restart = "always"` it is restarted instead.

`max_restarts = 5` stops a crash loop: after five restarts in a row the service
stays failed until a change or a manual restart.

A process that rebuilds itself — `dx serve`, `cargo leptos watch`, `trunk serve`
— is never restarted for a change, but stays supervised:

```
reload    = "self"
health    = { log = "<ready line>" }
reloading = { log = "<rebuild line>" }
```

A rebuild takes it out of ready and tells its dependents, as a restart would; the
ready line brings it back. A rustc error makes it unhealthy at once, with the
error in the overlay.

On a filesystem that delivers no change events (Docker bind mounts, WSL2's
`/mnt/c`, NFS, SMB), `[project] watch = { poll = "500ms" }` or `up --poll` scans
for changes instead, and `doctor` says when you need it.

## The cache

The key is a blake3 hash over input file contents, the command, declared
environment variables, declared outputs, the toolchain, the config, and every
upstream key.

```
[tasks.build]
cargo = "api"
inputs = ["crates/**/*.rs", "Cargo.lock"]
outputs = ["target/release/api"]
env_keys = ["PROFILE"]
```

A task's command runs in a **filtered environment** by default. Only a small safe
base, the variables it declares, and its inline `env` reach the child.

That is a correctness property rather than a preference. A key can only account
for variables it knows about, so the only way to guarantee an undeclared one did
not change the build is to make sure the build never saw it. A `loose` task is
never served from cache — being honestly uncacheable beats being occasionally
wrong.

`env_file = ".env"`, per node or under `[project]`, loads a dotenv file. Its
values reach strict tasks and key them, by hash rather than by value; a missing
file is an error unless marked `required = false`.

`timeout = "10m"` on a task, or `task_timeout` under `[project]`, stops a task
that runs too long, process tree and all, and fails the run with exit code 124.

A node restarted by a change gets `TURBORUST_CHANGED`: the paths that matched its
globs, one per line. It never keys the cache.

A hit **replays** the archived outputs rather than assuming the tree was
untouched, and results are stored by key, so alternating between two branches
hits both ways.

## Commands

| | |
|---|---|
| `up` | supervise everything, with a dashboard. Needs a terminal; `--no-tui` for scripts |
| `run <task>` | run once, in order, honouring the cache |
| `why <task>` | what would run right now, and what changed |
| `connect <node>` | attach your terminal to a running process |
| `doctor` | what is costing you seconds on every rebuild |
| `plan` / `graph` | the resolved graph, as a list or a diagram |
| `init` / `schema` | scaffold a config; emit its JSON Schema |

Useful flags on `run`: `--filter`, `--affected --base <ref>`, `--dry-run`,
`--summarize`, `--continue`, `--output-logs`, `--log-order`.

## Talking to a process

```
turborust connect api
```

Attaches your terminal to a supervised process — answer its prompt, use its
REPL, hit a breakpoint. **Ctrl-B then `d`** detaches and leaves it running;
Ctrl-C is deliberately not the detach key, because it has to reach the child.

## Serving a frontend

```
[services.web]
depends_on = ["web-build"]
serve = { dir = "dist", port = 8789 }
proxy = [{ path = "/api", to = "http://127.0.0.1:8788" }]
```

One origin: the page calls `/api/...` with a relative path, so there is no CORS
to configure and no API origin to swap for production. Prefixes match on segment
boundaries, so `/api` does not swallow `/apidocs`. While the backend restarts the
proxy answers a recognisable 502 rather than a raw connection error.

`.wasm` is served as `application/wasm`, without which browsers refuse
`instantiateStreaming` and blame your code in the console.

## Making it fast

```
turborust doctor
turborust doctor --fix     # shows each change, applies only what you confirm
```

The largest win is almost always the linker: incremental rebuilds are link-bound,
because codegen is cached and linking is not. `doctor` prints the exact snippet
and writes nothing unless asked — build configuration is your call.

## This site

The page you are reading is built by a small Rust generator and served by
turborust:

```
[tasks.site-build]
cargo = "site"
cmd = "cargo run --quiet --manifest-path site/Cargo.toml"
inputs = ["site/src/**", "site/content/**", "site/static/**", "site/Cargo.toml"]
outputs = ["site/dist"]

[services.site]
depends_on = ["site-build"]
serve = { dir = "site/dist", port = 8790 }
```

Edit a Markdown file and the page reloads. Edit only the stylesheet and it swaps
in place, keeping your scroll position.

</div></div>
