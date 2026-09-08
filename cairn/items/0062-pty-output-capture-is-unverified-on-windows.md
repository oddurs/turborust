---
id: 62
title: pty output capture is unverified on Windows
type: bug
status: backlog
milestone: later
created: 2026-09-08
updated: 2026-09-08
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

- [ ] The cause is identified rather than worked around
- [ ] Both tests run and pass on windows-latest
- [ ] The `#[cfg(unix)]` gates are removed
