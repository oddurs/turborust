# JSON output

Everything turborust prints or serves as JSON is an interface, with a schema
and a version. Script against it the way you would against an API.

| output | where | schema |
|---|---|---|
| `dry-run` | `turborust run --dry-run=json` | [`schemas/dry-run.json`](../schemas/dry-run.json) |
| `summary` | `turborust run --summarize`, written under `.turborust/summaries/` | [`schemas/summary.json`](../schemas/summary.json) |
| `verify` | `turborust verify --json` | [`schemas/verify.json`](../schemas/verify.json) |
| `cache` | `turborust cache --json` | [`schemas/cache.json`](../schemas/cache.json) |
| `state` | each frame of the overlay's `/__turborust/state` stream | [`schemas/state.json`](../schemas/state.json) |
| `history` | the overlay's `/__turborust/history?node=&limit=` | [`schemas/history.json`](../schemas/history.json) |
| `error` | stderr, under `--error-format json` | [`schemas/error.json`](../schemas/error.json) |

`turborust schema --json <output>` prints the schema for the binary you have,
which is the one to trust if it differs from the files linked here.

## The promise

Every payload carries `schemaVersion`, an integer, one per output.

**Without a new version**, a release may:

- add a field, anywhere;
- add a value to a field that holds one of several strings (a node `status`,
  an error `code`);
- add an output.

**Only with a new version**, a release may:

- remove or rename a field;
- change what type a field holds;
- make a field that was always present optional, or one that was never null
  nullable;
- change what a field means.

So a consumer should:

- ignore fields it does not know;
- treat a string it does not recognise as "something else";
- check `schemaVersion` and refuse a version it was not written for.

CI enforces the version rules (`scripts/schemas`): a change that removes,
retypes or loosens a field in `schemas/` without raising that output's version
fails the build.

## Errors

With `--error-format json` (or `TURBORUST_ERROR_FORMAT=json`), a command that
fails prints exactly one JSON value on stderr and exits 1:

```json
{"schemaVersion":1,"error":{"code":"config-invalid","message":"parsing turborust.toml","causes":["TOML parse error at line 1, column 9 …"]}}
```

| code | meaning |
|---|---|
| `config-not-found` | no `turborust.toml` here or in any parent, or `-c` names a missing file |
| `config-invalid` | the config could not be read, parsed or validated |
| `plan-invalid` | the config is valid, but no plan could be built from it (an unknown target, say) |
| `not-running` | the command needs a running `turborust up`, and there is none |
| `error` | anything not yet given a code of its own |

`message` and `causes` are for people. Their wording can change in any
release; branch on `code`.

Not covered:

- **A failing task is not an error.** `run` exits with the task's own exit
  code, and its output is the task's. Use `--summarize` for a record of what
  failed.
- **Argument errors** (an unknown flag, a missing value) are reported by the
  argument parser before turborust runs, as text, with exit code 2.

## Two names for one version

`dry-run` and `summary` shipped first, with the version in a field named
`schema`. They now carry `schemaVersion` as well, with the same value. `schema`
stays until their next version, and is deprecated: read `schemaVersion`.

Field names are not all cased the same way: `cache`, `verify`, `history` and
`error` use camelCase, the rest snake_case. Renaming them would break the very
consumers this contract is for, so they stay as they shipped.

`turborust mcp` returns these same payloads as its tools' `structuredContent`,
and declares their schemas as each tool's `outputSchema`.

The control socket under `.turborust/` is not part of this contract. It is how
`connect` and `why` reach a running `up`, versioned by its own handshake, and
may change in any release.
