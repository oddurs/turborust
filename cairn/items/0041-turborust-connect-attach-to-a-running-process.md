---
id: 41
title: 'turborust connect: attach to a running process'
type: feature
status: done
milestone: v1.1
assignee: Oddur Sigurdsson
created: 2026-09-08
updated: 2026-09-08
priority: p0
effort: l
area: proc
---

## Problem

We allocate a real pty for every child and never let anyone type into it. The
child believes it has a terminal, and there is no terminal on the other end.

So the first time a process asks a question — a migration confirming a
destructive change, a `cargo run` binary with a REPL, a debugger breakpoint, an
`ssh` host-key prompt — the dev loop is stuck and the only fix is to kill
turborust and run the thing by hand. That is exactly the point at which a
supervisor gets abandoned.

Overmind's whole value proposition is `overmind connect <proc>`. mprocs forwards
stdin from its TUI. We do neither.

## Design

Two halves, usable independently.

**A control socket.** `up` binds a Unix domain socket at `.turborust/control.sock`
(and removes it on shutdown, and on startup if stale). This is the seam every
other out-of-band command needs later — `restart`, `status`, `stop` — so it is
worth building once, properly, rather than special-casing `connect`.

Framing: newline-delimited JSON for control messages, then a raw byte stream once
a session is established. No length-prefixed binary protocol; a dev tool's control
channel should be debuggable with `nc`.

**The attach itself.** Output is already solved: every line is in the log store
with a monotonic `seq`, so the client streams from there exactly as the browser
overlay does. Only input is new. `MasterPty::take_writer()` gives a writer we have
never used; `connect` relays the client's bytes into it.

Client side:

- put the terminal in raw mode, restore it on exit even on panic (a tool that
  leaves your shell in raw mode is unforgivable)
- forward SIGWINCH to `MasterPty::resize`, so the child's idea of the window
  matches reality
- a detach key that is not Ctrl-C, because Ctrl-C must reach the child. Overmind
  uses Ctrl-B then D; follow that rather than inventing one.

**In the TUI.** A mode toggle: `i` sends keystrokes to the focused node, Esc
returns to navigation. The footer must say which mode is active — a TUI where
typing sometimes restarts your server and sometimes types into it, with no
indication, is worse than no input at all.

## Risks

Only one client may hold the writer at a time; a second `connect` to the same node
must be refused with a clear message rather than interleaving two people's
keystrokes.

## Acceptance criteria

- [x] `turborust connect <node>` attaches to a running node's pty
- [x] Keystrokes reach the child; a prompt can be answered
- [x] Ctrl-C reaches the child rather than detaching
- [x] The detach key detaches without stopping the node
- [x] The terminal is restored on detach, on error, and on panic
- [x] Window resize propagates to the child
- [x] A second attach to the same node is refused with an explanation
- [x] The TUI has an input mode, and shows which mode it is in
- [x] The socket is removed on shutdown and a stale one is reclaimed

## 2026-09-08

Built as designed: a control socket plus a pty writer we had never used.

The design assumed output could be replayed from the log store. That was wrong, and finding out why was the useful part: the log store holds LINES, and a prompt like "Continue? [y/N] " never ends one, so it is not there. Attaching to a process sitting at a prompt showed a blank screen — the exact case the feature exists for. Fixed by keeping a 16 KiB raw ring buffer per child, fed from the same read that feeds the line splitter, and replaying that on attach. Live output goes over a broadcast channel; a lagging client skips chunks, which for a terminal is the right failure.

A real bug surfaced in testing: the socket would not bind. Unix socket paths are capped near 104 bytes by sockaddr_un, and a workspace nested a few directories deep overflows it — bind fails with a bare EINVAL that reads as "sockets are broken" rather than "your path is long". So the socket is not in .turborust/ after all; it lives in a per-user temp directory named by a hash of the workspace, which both sides derive independently and therefore always agree on. Mode 0600 on the socket and 0700 on the directory, because this grants write access to a child stdin and a shared machine should not offer that to other users. A regression test asserts the deep path stays under the limit.

Detach is Ctrl-B then d, following Overmind rather than inventing a sequence. Ctrl-C must reach the child or attaching is useless for the programs that most need it. The split-across-reads case is handled and tested: two keystrokes almost always arrive as two reads, and a lone Ctrl-B is still delivered because readline uses it.

Terminal restoration is a Drop guard plus a panic hook. A tool that exits leaving raw mode on gives you a shell that does not echo and no obvious way back.

TUI input mode is `i` / Esc with a highlighted INPUT badge in the footer, and only engages for a node that is actually running.

Verified end to end against a service that prints an un-newlined prompt: prompt replayed on late attach, answered over the socket, second attach refused, slot released on detach, version skew explained, unknown node lists the real ones, stale socket reclaimed after kill -9, socket removed on clean shutdown.
