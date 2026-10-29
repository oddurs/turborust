# Benchmarks

turborust against the tools it overlaps with on a developer's machine, on the
things a developer feels: how long a stack takes to come up and go down, what
an idle `up` costs, how much output it carries, how quickly an edit becomes a
restart, and what is left running when something goes wrong.

Everything here is produced by [`bench/compare.py`](../bench/compare.py), and
the tables by [`bench/report.py`](../bench/report.py) from the raw results in
[`bench/results.jsonl`](../bench/results.jsonl). Numbers are medians.

## How it measures

- Every tool gets the same workload, written in its own config format
  (`turborust.toml`, `process-compose.yaml`, a `Procfile`), and runs headless
  with its output going to `/dev/null`.
- Each service is `touch <marker>; exec sleep 100000`. "Started" is the moment
  every marker exists. "Stopped" means the supervisor has exited after a
  SIGTERM and nothing it started is still alive.
- Processes are found through a token in their environment, so a tool's own
  helpers (overmind's tmux server, foreman's runner) count as its overhead.
  Anything still carrying the token after a stop counts as left running,
  whoever started it.
- Memory is proportional set size (PSS) summed over the supervisor and its
  helpers, so libraries shared with other processes are not counted in full.
  Idle CPU is measured over five seconds, one second after start.
- Watching tools are measured from the moment a file is written to the moment
  the restarted command is running, with each tool's default debounce.

## Setup

Linux 6.18, 4 vCPU (Intel Xeon @ 2.80GHz), in a container. Versions:
process-compose 1.124.0, overmind 2.5.1 (tmux 3.4), hivemind 1.1.0, honcho
2.0.0, foreman 0.90.0, watchexec 2.8.0, nodemon 3.1.14. turborust is a release
build of this commit.

To run it yourself:

```sh
bench/install.sh                         # builds the other tools under bench/.tools
export PATH=…                            # as install.sh prints
cargo build --release
python3 bench/compare.py lifecycle all   # or any scenario below
```

Each scenario is one command: `lifecycle`, `stubborn`, `killed`, `throughput`,
`longline`, `flood`, `churn`, `watch`, `coalesce`. Environment variables set
the sizes (`REPS`, `NS`, `LINES`, `MB`, `N`, `SECONDS_`, `EDITS`).

## What changed because of it

The first run measured turborust as of `70679aa`. Every gap it found was
closed or written down (cairn 0111):

| | before | after |
|---|---|---|
| start, 1 service | 74 ms | 10 ms |
| start, 10 services | 98 ms | 26 ms |
| start, 50 services | 252 ms | 112 ms |
| stop, 50 services | 1335 ms | 16 ms |
| idle CPU, 50 services | 10.6% | 0.0% |
| memory after a 64 MiB line | 75 MB | 25 MB |
| memory growth, 20 s of error lines | 531 MB | 0 MB |
| output, 1M lines | 1.1 M lines/s | 1.7 M lines/s |
| edit to restart | 161 ms | 61 ms |
| `run` of a cached task | 160 ms | 5 ms |

- **Start.** `rustc -V` (over 100ms through rustup's proxy) and `git rev-parse`
  ran before anything started. The toolchain is now probed in the background
  and only a cache key waits for it, and the repository is read from `.git`.
  On Linux a child is started with `posix_spawn` instead of a `fork` of the
  whole process, which grew dearer with every child already running (0112).
- **Idle CPU and stop.** Every child had a thread polling for its exit every
  20ms, and every stop polled on a timer. Exits are now notified: by a pidfd on
  Linux, by a thread blocked in `waitid` on macOS. Windows still polls. Shutdown stops each wave of
  independent nodes at once instead of one node at a time.
- **Memory.** Lines had no length limit and the log store limited only how
  many lines it kept, so one long line, or twenty thousand of them, was held
  whole. Lines are now cut at 64 KiB, and the store holds at most 16 MiB.
  The diagnostics list for a node grew for as long as it logged `error:` lines,
  and is now capped.
- **Edit to restart.** notify's debouncer woke every 30ms whether or not
  anything had changed, and added up to that again to every edit. turborust
  now waits on the events themselves. The default window is 50ms, watchexec's,
  rather than 120ms. A service's own `debounce`, which was accepted and
  ignored, now applies.
- **Throughput.** Output is read in batches under one lock instead of one lock
  per line, and written to stdout through a buffer instead of a system call per
  line. Per line, the process name is shared rather than copied and plain text
  is no longer copied to strip escapes it does not have. A profile of 100,000
  lines went from 511 to 257 million instructions.
- **A cached `run`.** It paid for `rustc -V` and then slept 60ms before
  exiting, whether or not anything ran. The toolchain's answer is now
  remembered against a fingerprint of everything that decides it, and the end
  of a run waits for its output and no longer (cairn 0113). The sleep had also
  been losing output: a streamed task printing 100,000 lines lost up to 60% of
  them.
- **Bugs.** Without a health check, a service's restart backoff never reset
  after a long run, so a service that crashed once a day waited longer each
  time; it now resets after a run longer than `backoff_max`. A stop returned
  once the process group's leader had gone, which could be before what the
  leader had started; it now waits for the whole group. A service's
  `debounce` was ignored (above). Two more were hiding behind notify's
  debouncer: on Windows the first read of a file reported it modified, and a
  polled save seen by two scans restarted twice.

## Results

<!-- generated by bench/report.py -->
#### 1 service

| tool | start | stop | idle CPU | memory (PSS) | left running |
|---|---|---|---|---|---|
| turborust | 10 ms | 3.4 ms | 0.0% | 7.3 MB | 0 |
| process-compose | 74 ms | 10 ms | 0.0% | 52.8 MB | 0 |
| overmind | 35 ms | 1187 ms | 0.4% | 10.5 MB | 0 |
| hivemind | 8.3 ms | 3.0 ms | 0.0% | 4.5 MB | 0 |
| honcho | 102 ms | 139 ms | 0.2% | 16.9 MB | 0 |
| foreman | 202 ms | 303 ms | 0.0% | 21.8 MB | 0 |

#### 10 services

| tool | start | stop | idle CPU | memory (PSS) | left running |
|---|---|---|---|---|---|
| turborust | 26 ms | 5.5 ms | 0.0% | 8.3 MB | 0 |
| process-compose | 86 ms | 10 ms | 0.0% | 53.9 MB | 0 |
| overmind | 66 ms | 1154 ms | 0.4% | 10.6 MB | 0 |
| hivemind | 19 ms | 3.0 ms | 0.0% | 5.4 MB | 0 |
| honcho | 122 ms | 135 ms | 0.2% | 38.7 MB | 0 |
| foreman | 206 ms | 1208 ms | 0.0% | 22.0 MB | 0 |

#### 50 services

| tool | start | stop | idle CPU | memory (PSS) | left running |
|---|---|---|---|---|---|
| turborust | 112 ms | 14 ms | 0.0% | 11.6 MB | 0 |
| process-compose | 184 ms | 12 ms | 0.0% | 57.0 MB | 0 |
| overmind | 205 ms | 1313 ms | 0.8% | 11.7 MB | 0 |
| hivemind | 94 ms | 7.7 ms | 0.0% | 8.4 MB | 0 |
| honcho | 260 ms | 163 ms | 0.4% | 128.6 MB | 0 |
| foreman | 367 ms | 5123 ms | 0.0% | 22.9 MB | 0 |

#### A child that ignores SIGTERM, with a grandchild

| tool | stopped in | left running |
|---|---|---|
| turborust | 8057 ms | 0 |
| process-compose | never | 4 |
| overmind | 1193 ms | 0 |
| hivemind | 3.3 ms | 0 |
| honcho | never | 5 |
| foreman | never | 2 |

#### The supervisor is SIGKILLed

| tool | processes left running |
|---|---|
| turborust | 0 |
| process-compose | 3 |
| overmind | 4 |
| hivemind | 0 |
| honcho | 5 |
| foreman | 3 |

#### One service printing 1,000,000 lines

| tool | time | lines/s | supervisor CPU |
|---|---|---|---|
| turborust | 583 ms | 1.71 M | 0.96 s |
| process-compose | 810 ms | 1.23 M | 0.90 s |
| overmind | 1971 ms | 0.51 M | 3.75 s |
| hivemind | 772 ms | 1.30 M | 0.72 s |
| honcho | 2627 ms | 0.38 M | 2.66 s |
| foreman | 18038 ms | 0.06 M | 17.84 s |

#### One 64 MiB line with no newline

| tool | memory afterwards |
|---|---|
| turborust | 25 MB |
| process-compose | 119 MB |
| overmind | 152 MB |
| hivemind | 134 MB |
| honcho | 82 MB |
| foreman | 89 MB |

#### An error line printed nonstop for 20 s

| tool | memory at 2 s | memory at 20 s | growth |
|---|---|---|---|
| turborust | 8.1 MB | 8.1 MB | 0.0 MB |
| process-compose | 62.4 MB | 64.5 MB | 1.9 MB |
| overmind | 20.9 MB | 21.3 MB | 0.2 MB |
| hivemind | 10.2 MB | 9.9 MB | -0.2 MB |
| honcho | 108.8 MB | 876.0 MB | 683.6 MB |
| foreman | 23.2 MB | 23.2 MB | 0.0 MB |

#### 20 services, a random one SIGKILLed every 50 ms for 20 s

| tool | kills | all back | back within | left running after stop |
|---|---|---|---|---|
| turborust | 140 | 20/20 | 15.0 s | 0 |
| process-compose | 388 | 20/20 | 1.1 s | 0 |

#### Edit to restarted process, default settings

| tool | median | p90 | missed |
|---|---|---|---|
| turborust | 61 ms | 62 ms | 0 |
| watchexec | 60 ms | 60 ms | 0 |
| nodemon | 32 ms | 38 ms | 0 |

#### Restarts caused by one logical change (three runs each)

| tool | atomic save | 200 files at once | 20 files, 5 ms apart |
|---|---|---|---|
| turborust | 1/1/1 | 1/1/1 | 1/1/1 |
| watchexec | 1/1/1 | 1/1/1 | 2/3/3 |
| nodemon | 1/1/1 | 1/1/0 | 2/2/2 |

## Reading the results

- **Leaks.** turborust and hivemind leave nothing behind, however they are
  stopped. A turborust child runs on a pty in its own session, so when
  turborust dies, even by SIGKILL, the kernel hangs up the terminal and every
  process on it goes too. process-compose, overmind, honcho and foreman all
  leave processes running after a SIGKILL. process-compose, honcho and foreman
  also leave them after a normal stop when a child ignores SIGTERM.
- **A child that ignores SIGTERM.** turborust waits out its `stop_timeout`
  (8s by default), then kills the process group. hivemind and overmind are
  faster here because they stop processes with SIGINT, which this child does
  not ignore. That is a difference in which signal is sent, not in speed.
- **Output.** turborust drains a flood of output fastest, though it spends
  more CPU per line than hivemind: it also keeps the lines, parses compiler
  diagnostics out of them and feeds the dashboard.
- **Starting many services.** hivemind is still a little faster at ten and
  fifty. On Linux turborust starts children with `posix_spawn`, which, like Go,
  copies nothing of the parent (cairn 0112); most of what remains at fifty is
  the workload itself, fifty shells each starting a `touch` on four CPUs. On
  macOS turborust still forks.
- **Recovering from a crash storm.** process-compose was configured to restart
  with no backoff. turborust doubles its backoff on consecutive crashes, up to
  `backoff_max` (30s by default), so a service killed again and again stays
  down longer before it comes back. That trade is deliberate: a real crash loop
  should not hammer the machine.
- **Edit to restart.** nodemon is faster because it does not debounce. It also
  restarted twice for a formatter that rewrote twenty files 5ms apart, and on
  one run did not restart at all for a 200-file checkout. watchexec restarted
  two or three times for the formatter. turborust restarted once for every
  pattern.
