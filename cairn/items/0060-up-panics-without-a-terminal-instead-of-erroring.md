---
id: 60
title: up panics without a terminal instead of erroring
type: bug
status: done
milestone: v1.1
created: 2026-09-08
updated: 2026-09-08
priority: p1
effort: s
area: tui
---

## Problem

Reported from the field by someone driving turborust headlessly.

`turborust up` with no tty panicked out of ratatui rather than failing cleanly:

```
$ turborust up </dev/null
thread 'main' panicked at ratatui-0.30.2/src/init.rs:366:16:
failed to initialize terminal: Os { code: 6, ... "Device not configured" }
```

Exit 101, and the message names ratatui's internals. A reader cannot tell from
that whether turborust is broken, their config is wrong, or their environment
simply has no terminal. Anyone running from CI, a script or an agent hits it
first, before anything else about the tool.

Worse, it wrote a terminal escape into redirected stdout:

```
$ turborust up </dev/null >out.txt 2>err.txt
$ cat -v out.txt
^[[?1049l
```

That is the leave-alt-screen sequence — emitted even though the alt screen was
never successfully entered — and it corrupts anything capturing stdout.

## Fix

Check for a terminal on **both** stdin and stdout before touching the terminal at
all, and fail with a message that names the cause and the two alternatives:

```
`turborust up` needs a terminal for its dashboard.
  For a script, CI, or an agent: `turborust up --no-tui` streams the same output,
  and `turborust run <task>` is the non-interactive way to build.
```

Both ends matter: the dashboard draws to stdout and reads keys from stdin, so a
pipe on either side makes it unusable.

`ratatui::try_init` and `try_restore` replace the panicking `init`/`restore`, so
a raw-mode failure returns an error, and nothing is restored that was never
entered — which is what was writing the stray escape.

## Acceptance criteria

- [x] `turborust up` without a terminal exits non-zero without panicking
- [x] The error names the cause and both alternatives
- [x] Nothing is written to stdout when it is not a terminal
- [x] `up --no-tui`, `run` and `plan` are unaffected

## 2026-09-08

Two defects in one report, and the second was the more damaging.

The panic was the visible one: ratatui::init panics on a raw-mode failure, so the message named ratatui internals and exited 101. Replaced with try_init, plus a check for a terminal on BOTH stdin and stdout before touching the terminal at all — the dashboard draws to one and reads keys from the other, so a pipe on either side makes it unusable.

The stdout corruption was worse, because it is silent. `restore()` was called unconditionally in the error path, writing the leave-alt-screen escape into whatever was capturing stdout — for an alt screen that had never been entered. Anything parsing that output gets eight bytes of garbage before the real content. try_restore, and only after a successful init.

The error now names the cause and both alternatives, because "needs a terminal" without saying what to do instead just moves the confusion:

    `turborust up` needs a terminal for its dashboard.
      For a script, CI, or an agent: `turborust up --no-tui` streams the same output,
      and `turborust run <task>` is the non-interactive way to build.

Verified against the exact reported invocation: exit 1 instead of 101, zero bytes on stdout instead of eight. --no-tui, run and plan confirmed unaffected.

Worth noting where this came from: every headless path was tested throughout, and `up` with a TUI never was, because the test harness always drives the engine directly. The one path exercised only by a human was the one that broke for everyone who was not one.
