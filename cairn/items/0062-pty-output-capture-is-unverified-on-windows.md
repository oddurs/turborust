---
id: 62
title: pty output capture is unverified on Windows
type: bug
status: done
milestone: later
created: 2026-09-08
updated: 2026-10-29
claimed: 2026-10-29
priority: p2
effort: m
area: proc
---

## Problem

165 of 167 tests pass on `windows-latest`. The two that fail are the pty tests,
and both fail the same way: no child output is captured at all.

```
proc::tests::captures_child_output_and_exit_code -> expected to capture child stdout
proc::tests::stop_kills_the_whole_process_group  -> child never started
```

They are gated `#[cfg(unix)]` for now, which means Windows pty capture — and with
it the job-object teardown those tests would exercise — is **unverified**, not
working. The README says so rather than implying support.

## What was already ruled out

- **Shell syntax.** The fixtures were rewritten for `cmd.exe` (`&` rather than
  `;`, `ping` rather than `sleep`). No change.
- **Reader ordering.** The hypothesis was that ConPTY has nothing to read until
  it has a client, so a reader started before the child exits immediately.
  Starting the reader after the child on Windows made no difference, and the
  change was reverted rather than kept as unverified platform-specific code.

## Wider than the two unit tests

The integration suites (`tests/run_path.rs`, `tests/supervisor.rs`) are also
Unix-gated, for a different and more tractable reason: every fixture in them is a
POSIX shell snippet, and turborust spawns through `cmd /C` on Windows.

The fix for that half is a **configurable shell**:

```toml
[project]
shell = "bash"          # or the platform default
```

Windows runners have Git Bash, so this would make the whole suite portable and is
independently useful — plenty of Windows developers would rather write their
task commands in one shell than two. It is a small, well-understood change, and
unlike the pty problem it does not need anyone to debug ConPTY.

## Where to look next

Someone who can run Windows directly, rather than iterating against CI — that
approach was tried three times here and found nothing, which is itself the
finding.

Candidates worth checking first:

- Whether `portable-pty`'s ConPTY backend needs the reader on the same thread
  that created the pty
- Whether `cmd /C` output through ConPTY needs an explicit flush or a
  console-mode change before it is readable
- Whether the `\r\n` handling in `pump` interacts badly with ConPTY's own
  line discipline, since `\r` is treated as a transient line terminator

## Acceptance criteria

- [x] The cause is identified rather than worked around
- [x] Both tests run and pass on windows-latest
- [x] The `#[cfg(unix)]` gates are removed
- [x] A configurable shell makes the integration fixtures portable

## 2026-10-28

mise has `run_windows`, a per-task replacement command on Windows. A configurable shell (above) is the broader answer and makes most of that unnecessary, but a per-node override would still help where a command has no portable spelling at all. Noted from the `0090` survey rather than filed separately.

## 2026-10-29

A cause, found by reading rather than iterating: `portable-pty` 0.9.0 creates the pseudoconsole with `PSEUDOCONSOLE_INHERIT_CURSOR` (`src/win/psuedocon.rs`). With that flag ConPTY's first act is to ask the hosting terminal where the cursor is (`ESC[6n`), and it renders nothing until the answer comes back on its input. turborust never answered, which fits both symptoms exactly: no output at all from the first test, and "child never started" from the second, which waits for a line that is never rendered. It also explains why reordering the reader changed nothing: the reader was fine, there was simply nothing to read.

The reader now answers `ESC[1;1R` on Windows, once per query, unless a terminal is attached through `connect` to answer for itself. The query is found even when split across two reads. Not on Unix: a Unix pty asks nothing, and with echo on, a reply to a child that merely printed those bytes would land in its stdin and its log. The two tests are un-gated; whether this is the whole cause is for `windows-latest` to say.

Confirmed on `windows-latest` (#52's CI): `captures_child_output_and_exit_code`, `stop_kills_the_whole_process_group` and `the_reader_answers_a_cursor_query` all pass, in a unit suite of 241 with nothing ignored. The unanswered cursor query was the whole cause. The item stays open for the configurable shell, the one criterion left.


The configurable shell: `[project] shell`, or `shell` on a service or task. A name gets the flag that shell takes a command with (`-c`; `-Command` for pwsh and powershell; `/C` for cmd). An array is an exact argv with the command appended last. The shell is folded into the cache key only when one is set, so every existing key still hits, but the same text under another shell is another key. mise's `run_windows` (above) is covered by a per-node `shell` for most cases; a per-platform command was not added. What is left on this item is the last criterion, the integration fixtures running on Windows through the shell, tried separately because that is CI iteration and this is not.

The integration suite, through the shell: `tests/run_path.rs` now runs on `windows-latest` with Git Bash as its fixtures' shell. The first run passed 20 of 25. Four failures were the fixtures' own: the sandbox's canonical path is `\\?\C:\...`, and its backslashes are invalid escapes inside the TOML strings the fixtures build. Paths are now forward-slashed with no verbatim prefix, which Git Bash reads the same.

The fifth was a real bug, and the reason this was worth trying: **a stop on Windows asked the child for nothing.** Unix sends SIGTERM to the group and escalates after the grace; Windows had no polite step, so every stop waited out the whole `stop_timeout` (8s by default) and then terminated the job. A task over its timeout ran to completion meanwhile, here a `sleep 5; touch` that touched. `stop` now types Ctrl-C into the pseudoconsole first, which ConPTY delivers to its console processes as CTRL_C_EVENT, and terminates the job only when the grace runs out. With that, all 25 pass on Windows.

One test stays Unix-only, deliberately: the grandchild-pid one reads a pid from the shell, and Git Bash's are MSYS pids, not Windows ones. `tests/supervisor.rs` is still Unix-gated; its fixtures lean on signals (`trap`, `kill -0`) that have no Windows meaning, which is a different problem from the shell.

