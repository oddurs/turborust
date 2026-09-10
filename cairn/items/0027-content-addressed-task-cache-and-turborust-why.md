---
id: 27
title: Content-addressed task cache and turborust why
type: feature
status: done
milestone: v0.0
created: 2026-09-08
updated: 2026-09-08
area: cache
effort: l
---

blake3 over input file contents, the command, the declared env keys and the
upstream tasks' keys. Content, not mtime — mtime-based invalidation is why other
watchers rebuild the world after a `git checkout`.

Every fingerprint retains its per-file hashes, which is what lets `turborust why`
answer the only question that matters when a rebuild surprises you:

    cache MISS — b3:9068e57b411c → b3:614d4a52304e
        ~ crates/shared/src/lib.rs  b3:5f16c070 -> b3:d33db911

Records are written through a temp file and renamed, so a crash mid-write cannot
leave a truncated record that reads back as a false hit.

Known gaps found later by audit: 0014, 0015, 0017.
