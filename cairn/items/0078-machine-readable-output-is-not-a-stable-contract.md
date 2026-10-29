---
id: 78
title: Machine-readable output is not a stable contract
type: chore
status: done
milestone: v1.4
created: 2026-09-10
updated: 2026-10-29
claimed: 2026-10-29
priority: p2
effort: m
area: cli
---

## Problem

`plan`, `why`, `graph` and `--summarize` all emit JSON, and every one of those
shapes is an implementation detail that happens to be serialised. Nothing
documents them, nothing versions them, and nothing fails when a field is
renamed.

That was fine while the only consumer was a person reading output. It is not
fine now. `0060` — `up` panicking without a TTY — was reported by an automated
consumer driving this tool in a pipeline, which is a preview of how it will
mostly be used. Anything that scripts against these shapes today is scripting
against a moving target.

## What should happen

Treat machine-readable output as an interface:

- One documented schema per command, emitted by `turborust schema --json <cmd>`
  the way `turborust schema` already emits one for the config file
- A `schemaVersion` on every payload
- A CI job that fails when a schema changes without the version changing —
  the repository already has this exact pattern for the config schema and the
  rendered roadmap, so it is a third instance of a solved problem
- Errors as structured data on stderr, not prose, so a non-zero exit is
  interpretable without parsing English

## Why before the interesting thing

This is deliberately filed ahead of `0077`. An MCP server over an unversioned,
undocumented JSON shape is a liability with a nicer name: it multiplies the
consumers of a contract that does not exist yet. Fix the contract, then decide
whether to wrap it.

## Acceptance criteria

- [x] Every `--json` payload carries `schemaVersion`
- [x] `turborust schema --json <cmd>` emits the schema for each command
- [x] CI fails on an unversioned schema change
- [x] Errors are structured on stderr, with a stable code per failure kind
- [x] The compatibility promise is documented — what may change in a patch
      release and what may not

## 2026-10-29

Seven payloads, listed in `contract::Output`: `dry-run`, `summary`, `verify`, `cache`, the overlay's `state` frames and `history`, and the new `error`. Each derives its schema from the type that serialises it (schemars, which honours the serde attributes), so the schema cannot describe a field the payload does not have. `turborust schema --json <output>` prints it; `schemas/*.json` holds the committed copies; `docs/json.md` is the promise. The spec names `plan`, `why` and `graph` too, but none of them prints JSON. Adding JSON forms for those is a separate decision, and this item only has to cover the ones that exist.

Choices worth knowing:

- **`schemaVersion` everywhere, and `schema` kept beside it.** `cache` and `verify` had shipped `schemaVersion`; `dry-run` and `summary` had shipped `schema`. Renaming either would break the very consumers this exists for, so the two older payloads now carry both, with `schema` deprecated until their next version. Casing is mixed for the same reason, and is written down rather than "fixed".
- **One version per output, not one for the tool.** A breaking change to `verify` should not ask a consumer of `summary` to re-check anything.
- **"Unversioned schema change" means a breaking one.** A plain "schema file changed" diff, as for the config schema, would fail every added field, and the promise is that adding a field never needs a version. `scripts/schemas` flattens each schema into the places a consumer can read a value from (with `$ref`s resolved) and fails only when one is removed, loses a type, becomes nullable, or stops being always present, unless `schemaVersion` went up. It runs in CI against the base sha, and in `scripts/agent check` against `origin/main`. Checked against a mutated copy: it caught the removal, the retype and the new nullability, and let the added field through.
- **Structured errors are opt-in** (`--error-format json`, or `TURBORUST_ERROR_FORMAT=json`), because the default reader of stderr is a person. Codes attach where the failure is known (`contract::Failure`): finding, reading and validating the config, building the plan, and reaching a running `up`. Everything else is `error` until it earns a code. A `Failure` reads exactly like the error it wraps, so text output is byte-for-byte what it was; `a_failure_has_a_stable_code_under_error_format_json` covers all four codes and the unchanged text form.
- **The control socket is excluded.** It already has its own protocol handshake, and `connect` needs freedom to change it. 0077 (MCP) is where it would become public, and should decide that then.

Found along the way: the doc comment for the overlay's state stream had ended up on 0110's new `history` handler, in my own commit. Moved back. The history endpoint now returns `{schemaVersion, history}` rather than a bare array, which is safe because it has not shipped yet.

