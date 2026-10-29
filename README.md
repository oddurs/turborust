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

It is also measured against them, not just compared: start and stop times,
idle cost, output throughput, edit-to-restart latency, and what each leaves
running when it is stopped or killed. See [docs/benchmarks.md](docs/benchmarks.md).

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

## Install

```
cargo install --locked --git https://github.com/oddurs/turborust
```

Needs Rust 1.89 or newer. Each tagged release also carries prebuilt binaries for
macOS, Linux (static, any distro) and Windows, with `SHA256SUMS`, a
build-provenance attestation, shell completions and a man page — see
[SECURITY.md](SECURITY.md#verifying-a-release) for checking them. From the first
release on, `cargo install turborust` works as well.

## Commands

```
turborust up [targets…]     start services and keep them running (TUI)
turborust run <task>…       run tasks once, in order, honouring the cache
turborust why <task>        why would this run — or not — right now
turborust doctor            what is costing you seconds on every rebuild
turborust plan              the resolved graph, with derived globs
turborust connect <node>    attach this terminal to a running process
turborust init              scaffold a config from your Cargo workspace and Procfile
turborust schema            JSON Schema for turborust.toml, for editor completion
turborust completions <sh>  shell completion script
turborust man               the man page
turborust verify [task…]    run tasks twice and prove their outputs match
turborust cache             what the cache is holding, and what it last dropped
turborust clean             drop cached task results
```

### Stopping

ctrl-c, SIGTERM (`docker stop`, systemd, a cancelled CI job) and SIGHUP (a closed
terminal) all mean the same thing: stop every node in reverse dependency order,
each within its `stop_timeout`, SIGTERM then SIGKILL to its whole process group.
Send any of them again to stop waiting and kill everything at once.

`up` exits 0 after a requested stop. `run` exits 128 + the signal number — 130
for ctrl-c, 143 for SIGTERM — so an interrupted run never reads as a passing one.

SIGKILL to turborust itself cannot be handled by any process, and a child that
ignores SIGHUP can outlive it. Under systemd the default `KillMode=control-group`
cleans that up; in a container, the container's own teardown does.

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

### Coming from foreman, overmind or honcho

`turborust init` next to a `Procfile` turns each `name: command` line into a
service that runs the same command, alongside whatever it infers from the cargo
workspace. A process whose command reads `$PORT` gets the port foreman would
have given it (5000, 5100, …) as `env` and as its readiness probe. A `.env`
beside the Procfile becomes `env_file`. Lines it cannot translate are named, not
dropped: `release`, which Heroku runs once per deploy, a duplicate name, or a
line that is not `name: command`. Then add `watch`, `depends_on` and `health`,
which a Procfile has no way to say.

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

Settings that do not belong in a committed file go in a `.env` file, in the same
`KEY=value` syntax foreman, overmind, Compose and dotenv read (comments, quotes,
`export` prefixes; no `${VAR}` expansion):

```toml
[project]
env_file = ".env"                    # every node

[services.api]
env_file = [".env.api", { path = ".env.local", required = false }]
```

Paths are relative to the workspace root. The project's files load first, then
the node's, then inline `env`, each overriding the last. A missing file is an
error unless it says `required = false`, so a typo is not a quietly empty
environment. Files are re-read whenever a node starts, so an edit to `.env`
applies on the next restart. Their values reach strict tasks, since the file
declared them, and they key the task: change one and the next run is a miss.
The key records a hash of each value, never the value itself.

A node that runs because of a change is told what changed: `TURBORUST_CHANGED`
holds the workspace-relative paths that matched its own globs, one per line. It
is empty on a start no change caused. Use it to test only what moved:

```toml
[tasks.test-changed]
cmd = "cargo nextest run $(printf '%s\n' \"$TURBORUST_CHANGED\" | sed -n 's|^crates/\\([^/]*\\)/.*|-p \\1|p' | sort -u)"
inputs = ["crates/**/*.rs"]
```

It is never part of a cache key, since it says why a task ran, not what it ran
on.

### What `outputs` promises

`outputs` is a claim that those files are everything the task produces that
anything downstream could care about. Two things rest on it: a cache hit replays
exactly those files and assumes nothing else was needed, and a dependent keys on
their contents rather than on the fact the task ran.

So a task that writes something it never declared — a file outside the globs, a
row in a database — can under-invalidate its dependents, in the same way it can
already be restored incompletely from cache. If a task has effects it cannot
declare, give it `cache = "disabled"` and it will neither be replayed nor be
treated as stable by anything downstream.

Outputs must stay inside the workspace: an absolute path or one that climbs out
with `..` is refused when the config is loaded. A hit writes those files back, so
an output outside the workspace would let a cache entry write wherever the path
pointed.

The safe base is passed but not hashed: without `PATH` nothing runs, and hashing it
would miss whenever a shell rearranged itself. Swapping toolchains via `PATH` is
covered by the global hash (`0017`), which folds in `rustc -V` directly.

### Time limits

```toml
[project]
task_timeout = "30m"   # every task, unless it says otherwise

[tasks.test]
cmd = "cargo test"
timeout = "10m"
```

A task still running at its limit is stopped as a shutdown stops it: the whole
process tree, `SIGTERM` first, then a kill after a grace period. It fails with
"timed out after 10m", and `run` exits **124** (the code `timeout(1)` uses), so
CI shows which task hung, not just a cancelled job. With `--continue` the rest
of the graph still runs. Services have no time limit; liveness is what checks
them.

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
health = { cmd = "pg_isready -h 127.0.0.1" }         # a command exits 0
```

`cmd` is for processes whose port opens before they can do anything: Postgres
accepts a connection while it is still answering "the database system is
starting up", so `tcp` passes too early. The command runs with the node's
environment and working directory, each attempt is cut off after `timeout`
(default `5s`), and if readiness gives up, the last thing the command said is
logged.

Every probe a node declares must pass, so `{ tcp = 8788, log = "ready" }` means
both. The log probe exists for the many dev processes that announce themselves on
stdout and have no port worth probing — `trunk`, `cargo leptos watch`, migrations.
It matches against ANSI-stripped output, and resets on every restart so a stale
announcement cannot mark a fresh process ready.

`depends_on` waits for **readiness**, not for "a process exists". A dependent
starts only once its dependency's health probe passes, and stops when that
dependency goes down — which is what makes rebuild cascades work with exactly one
restart per node.

A probe that gives up leaves the node **unhealthy**: still running, not ready,
and counted as a failure — so a one-shot `run` waiting on it ends instead of
waiting forever.

### Staying alive

Readiness is checked once, on the way up. A service that deadlocks or loses its
database afterwards keeps its process and its green dot. `liveness` keeps
asking:

```toml
[services.api]
cmd      = "cargo run -p api"
health   = { tcp = 8788 }
liveness = { http = "http://127.0.0.1:8788/healthz", every = "10s", failures = 3 }
```

It takes the same `tcp`, `http` and `cmd` probes as `health`, with `timeout`
per attempt (default `5s`), every `every` (default `10s`). One miss is not an
outage: only `failures` in a row (default `3`) count. Then the service goes
**unhealthy** and its dependents are told, as for any stop; when the probe
answers again it is healthy again and they come back. With `restart = "always"`
it is restarted instead, so a hang heals itself. Liveness pauses while a
`reload = "self"` process is rebuilding, since it is not expected to answer
then.

### Crash loops

`restart = "always"` restarts a crashed service with exponential backoff,
starting at `backoff_min` (default `250ms`) and doubling up to `backoff_max`
(default `30s`). On its own that never stops, which is right for a flaky
dependency and wrong for a typo:

```toml
[services.api]
restart      = "always"
max_restarts = 5
```

After five restarts in a row for crashing, the service stays **failed**, saying
"gave up after 5 restart(s) in a row", until a watched file changes or you
restart it from the TUI or the overlay. Either one is a new attempt with a fresh
count. A run that stayed up longer than `backoff_max` was not part of a loop, so
the crash that ends it starts the count again.

### Processes that reload themselves

`dx serve`, `cargo leptos watch` and `trunk serve` watch your sources and rebuild
on their own; `dx` even patches the running binary and keeps its state.
Restarting them would throw that away. Telling turborust to leave them alone
used to mean it stopped supervising them too — reading healthy through a failing
rebuild, telling dependents nothing. `reload = "self"` keeps the supervision and
drops the restart:

```toml
[services.web]
cmd       = "dx serve"
reload    = "self"                      # never restarted for a change
health    = { log = "<ready line>" }    # ready, and ready again
reloading = { log = "<rebuild line>" }  # a rebuild has started
```

Copy both strings from your tool's own output; they differ between tools and
versions. Then a rebuild is a state change rather than a restart:

- **`reloading`** matches → the node is `reloading`, out of ready. Dependents are
  told exactly as for a restart, and the TUI and overlay show work in flight.
- **`health`** matches again → `healthy`, and the cascade brings dependents back.
  No browser reload is sent: the process just patched itself, and a reload would
  discard what it kept.
- **rustc reports an error** → `unhealthy` at once, with the error in the
  overlay's Issues panel. A rebuild that goes quiet instead is `unhealthy` after
  `ready_timeout`. Either way it comes good by itself on the next edit that builds.

`turborust plan` marks these nodes, because their edges behave differently.

### When edits are not seen

Some filesystems never tell the OS watcher anything: Docker Desktop bind mounts
on macOS and Windows, WSL2 paths under `/mnt/c`, NFS, SMB, VM shared folders.
There the dev loop just stops rebuilding, without an error. Scan instead:

```toml
[project]
watch = { poll = "500ms" }
```

or `turborust up --poll` for one session. Changes found by scanning go through
the same dispatch and debounce as OS events. A file counts as changed when its
modification time (full precision) or length changes; contents are never read.
Scans skip `.git`, `target` and `node_modules` at any depth, as well as the
top-level directories the native watcher never registers. `turborust doctor`
warns when the workspace is on one of these filesystems.

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
  vars, declared outputs, env mode, and an upstream stamp per dependency. Results
  are stored *by key*, so alternating between two branches hits both ways instead
  of overwriting. A hit replays the archived outputs rather than assuming the tree
  was untouched; results above 256 MiB are recorded but not archived, and fall
  back to checking the files are still present.
- **Dependents key on what an upstream produced, not on the fact it ran.** If a
  task declares `outputs`, its dependents fold in a hash of those files' contents;
  if it declares none, they fold in its key. So fixing a doc comment in a shared
  crate re-runs `check` — its inputs did change — but does not rebuild everything
  below it on the strength of a byte-identical artifact. `why` names which rule
  applied:

  ```
  ~ [dep:gen]  outputs b3:350324e4 -> outputs b3:f0356a38
  ```
- **Supervision decides in one place and acts in another.** Each node's
  lifecycle is a pure state machine (`src/lifecycle.rs`): an event — a child
  exited, a probe answered, a timer fired, a dependency changed — goes in, and the
  actions to take come out. A thin executor does them and reports back. Every
  decision is tested by driving the machine through exact event sequences, in
  microseconds and without a single process; the integration suite proves the
  executor carries them out.
- **Dependencies are read, not told.** A node's readiness is a function of its
  status, which one function writes. Dependents wake on any change and compare
  their dependencies' status and generation against what they started with — so
  a dependency that rebuilt too fast to be seen leaving ready still moved its
  generation, and its dependents still restart.
- **Cascade dispatch.** When one edit matches several nodes, only the upstream-most
  are signalled; dependents come back through the readiness cascade instead of
  restarting twice.
- **Full page reload, not HMR.** A wasm binary has no module boundaries left to
  patch. Calling it HMR would be a lie.

## Roadmap

Tracked with `cairn` as Markdown files under `cairn/items`, which are reviewed
in pull requests like anything else, and rendered to
[ROADMAP.md](https://github.com/oddurs/turborust/blob/roadmap/ROADMAP.md) after each merge.

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
max_size = "10GiB"                  # local budget; "0" keeps everything
```

Any directory both machines can see: a network mount, a synced folder, another
checkout. A hit in the shared store is copied into the local one on the way past,
so the round trip happens once.

### Proving the cache is entitled to its answer

A hit is a promise: *this key reproduces these outputs*. That is only sound if
the task is deterministic, and nothing else in this ecosystem checks. A task
that embeds a timestamp, iterates a `HashMap`, or bakes in an absolute path will
have one of its several possible answers served forever, silently.

`0014` fixed the part that could be fixed by construction — undeclared
environment variables, by filtering the child's environment. The rest cannot be
prevented, only detected. A content-addressed cache is the only kind that can
settle it locally, in one command:

```
$ turborust verify

  ✓ check — deterministic (3 output(s), 2 runs identical)
  ✗ build NOT DETERMINISTIC
      ~ target/release/api  b3:5f16c070 -> b3:d33db911
        differs at offset 0x1f40, 8 byte(s) — a short run: usually an
        embedded timestamp or build id
```

Each task runs twice, with its declared outputs deleted first — otherwise a
second run of an incremental tool is a no-op that reproduces byte-identical
files and proves nothing. Exit status is non-zero if anything diverged.

The offset is the useful part. A short run at a fixed offset is a stamped field;
a difference that starts early and never resynchronises is usually ordering;
different lengths mean the output is genuinely being rebuilt differently.

Nx finds this statistically, by watching results for the same task hash across
many CI runs. That needs a fleet. This needs two runs.

#### Common causes, and what to do

| Symptom | Usual cause | Fix |
|---|---|---|
| Short run, fixed offset, same length | Build timestamp or build id | Set `SOURCE_DATE_EPOCH`, or strip it |
| Absolute paths in the output | `--remap-path-prefix` not set | Add it, or build from a stable root |
| Long divergence, same length | `HashMap`/`HashSet` iteration order | Use `BTreeMap`, or sort before writing |
| Different lengths each run | Parallel codegen writing in completion order | Sort, or pin `codegen-units` |
| Only under `verify`, never alone | The task reads something it did not declare | Declare it in `env_keys`, or `inputs` |

If a task genuinely cannot be made reproducible, `cache = "disabled"` is the
honest answer: it will neither be replayed nor treated as stable by anything
downstream.

#### Checking during ordinary work

```toml
[cache]
verify = "off"      # off | sample | always
```

`sample` re-runs one hit in twenty and compares it against what was about to be
replayed; `always` checks every hit and is roughly as slow as having no cache. A
mismatch drops the entry and fails the run, because a record that lies is worse
than no record.

Off by default: checking costs a rebuild, and that is a price to opt into rather
than one to discover.

### Where results live

Not in `.turborust/`. Results are content-addressed, so two checkouts of one
repository at one commit produce the same keys and the same artifacts — and this
project's own workflow is a worktree per branch, which means a per-checkout
store makes every new branch pay a full cold build for answers already on the
disk.

The store is therefore per **repository**, in your user cache directory, keyed by
`git rev-parse --git-common-dir` — which resolves to the same place from every
worktree of a repository, and is exactly the identity wanted. Outside a git
repository the workspace path is the only identity available, which is the old
behaviour. `.turborust/` keeps what is genuinely per-checkout: run summaries and
scratch space.

```toml
[cache]
dir = "vendor/cache"   # override, if you want it somewhere specific
```

An existing `.turborust/` cache is moved into the shared store the first time
turborust runs, so upgrading does not start you cold. Because the store is now
shared, `turborust clean` empties it for **every** worktree of the repository,
and says so.

Two turborust processes can now be writing the same key at the same moment.
Records go to a per-process temporary name and are renamed into place, and
archives are staged in a scratch directory and swapped in — a half-written
archive under the real name would pass `restore`'s existence check and replay
truncated files.

### What the local store keeps

`max_size` is a byte budget, and results are dropped least-recently-**used**
first — used meaning *read*, not written. Ordering by write time evicts the
entry you hit forty times a day in favour of a one-off produced last night on a
branch you have since deleted, which is backwards for the access pattern a cache
exists to serve. A hit touches a zero-byte marker beside the record rather than
rewriting the record, because a record carries one hash per input file and
taxing every hit to speed up eviction is the wrong trade.

`turborust cache` shows what is held, per task, and what the last eviction
dropped — so a miss caused by a sweep can be explained rather than looking like
the cache simply forgot.

A **shared** store is never evicted from. It is not one machine's to
garbage-collect: deleting from it would drop results other people are still
reading, on the strength of a budget they never set. Give a shared cache its own
retention policy.

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
want it today, use dioxus — and run it under turborust with
[`reload = "self"`](#processes-that-reload-themselves), which supervises it
without owning any of the patching. If turborust ever grows it, the realistic route is to
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
