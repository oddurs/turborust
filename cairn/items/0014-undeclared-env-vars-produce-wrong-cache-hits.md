---
id: 14
title: Undeclared env vars produce wrong cache hits
type: bug
status: done
milestone: v0.1
assignee: Oddur Sigurdsson
created: 2026-09-08
updated: 2026-09-08
priority: p0
effort: m
area: cache
---

## Problem

The cache key folds in only the env vars a task names in `env_keys`. The child
still inherits the full parent environment, so any undeclared variable changes the
build's *output* without changing its *key*.

Reproduced against the binary before the fix:

```
$ GREETING=hello   turborust run build     # writes "hello" to out.txt
$ GREETING=goodbye turborust run build
   build | [turborust] cache hit  b3:27b6d1f4c607  (saved 25ms)
$ cat out.txt
hello
```

The tool returned a stale answer and reported success. For a build cache this is
the worst failure mode there is — worse than never caching at all, because it is
silent.

## Proposal

Take turborepo's shape (MIT; read for design, not copied). Two env modes:

- **strict** (default): the child is spawned with a *filtered* environment
  containing only the declared `env_keys` plus a small safe base (`PATH`, `HOME`,
  `TERM`, ...). An undeclared variable then cannot affect the build, so it does
  not need to be in the key. This makes the guarantee structural rather than
  relying on the user to declare correctly.
- **loose**: inherit everything, and mark the task uncacheable — honest about the
  fact that we can no longer key it.

`pass_through_env` covers the escape hatch for variables that must reach the child
without being part of the key.

## Acceptance criteria

- [x] `env_mode = "strict" | "loose"` per task, strict by default
- [x] Strict tasks run with a filtered environment
- [x] Loose tasks are never served from cache
- [x] A test reproduces the failure above and asserts a miss
- [x] Changing `env_mode` invalidates the key

## 2026-09-08

Fixed with a strict-by-default filtered environment rather than by widening the
key, because keying can only ever cover variables the user remembered to declare.

A strict child gets exactly: a safe base (PATH, HOME, CARGO_HOME, RUSTUP_HOME,
TERM, locale, plus the Windows essentials), its declared `env_keys`, its
`pass_through_env`, and its inline `env`. Nothing else. So an undeclared variable
cannot change the build, and therefore cannot invalidate a key that does not
mention it. Verified against the original reproduction: with `GREETING` undeclared
the build now reads `unset` regardless of the ambient value, so the cache hit is
correct; declaring it makes a changed value a miss.

Two deliberate calls worth recording:

1. The safe base is passed but NOT hashed. Without PATH nothing runs, and hashing
   PATH would miss whenever a shell rearranged itself. The residual risk — swapping
   toolchains via PATH without invalidating — is exactly what 0017's global hash
   covers by folding in `rustc -V`.

2. Services default to loose, not strict. They are never cached, so filtering
   would only break dev servers that legitimately read ambient configuration, with
   no correctness benefit.

Found while verifying: the cache keeps only the LAST record per task, so
alternating between two input sets always misses. Noted on 0015, where hash-keyed
artifact storage fixes it naturally.

## 2026-09-08

Recovering this file: I truncated it with `open(f,'w').write(open(f).read())`,
where the `'w'` truncates before the read runs. Body and note restored verbatim
from the session; the acceptance criteria above are ticked because the work was
genuinely done and verified, not because the file was rewritten.
