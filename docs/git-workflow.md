# How work lands here

One unit of work, one worktree, one branch, one pull request. `main` only ever
advances through a merge.

This exists so the process is identical whoever is driving — a person, or several
agents working at once. Everything below is enforced by a hook or a script rather
than by memory, because a convention that depends on discipline is a convention
that decays.

## The loop

```
scripts/agent start 0041          # worktree + branch + claim the tracker item
cd ../.worktrees/turborust/fix-0041-...
                                  # do the work
scripts/agent check               # formatter, linter, tests, roadmap
scripts/agent commit "fix(proc): start the pty reader before the child"
scripts/agent pr                  # push, open a PR from the tracker item
scripts/agent clean               # once merged, remove the worktree
```

`just start 0041`, `just check`, `just pr` are aliases for the same thing.

## Why worktrees rather than branches

Two agents in one checkout will eventually collide over the index, `target/`, or
a half-applied edit — and when they do, the failure looks like a bug in the code
rather than a bug in the process. A worktree per branch makes that impossible:
separate directory, separate index, separate build.

```
turborust/                            the primary checkout, always on main
../.worktrees/turborust/<branch>/     one directory per branch
```

Worktrees live outside the repository so they never appear in `git status` and
never get committed by accident. `scripts/agent clean` removes the ones whose
branch has merged.

## Branches

`<type>/<slug>`, and when the work has a tracker item the slug starts with its
id, derived automatically:

```
fix/0041-turborust-connect-attach-to-a-running
feat/0042-proxy-one-origin-for-the-whole-stack
chore/0049-reverse-dependency-index-on-the-plan
```

Types: `feat` `fix` `chore` `docs` `perf` `refactor` `test` `build` `ci`.

## Commits

Conventional Commits, imperative, subject under 72 characters, no trailing
period. The body explains *why*; the diff already says what.

```
fix(proc): start the pty reader before the child

The reader thread was spawned after the child, so a command that wrote and
exited immediately could finish before anything was reading — macOS discards
the unread buffer when the last slave fd closes.

Refs: 0037
```

`scripts/agent commit` adds the `Refs:` trailer from the worktree, so it is never
the thing that gets forgotten.

## What the hooks enforce

| Hook | Runs | Rejects |
|---|---|---|
| `commit-msg` | every commit | attribution footers, non-conventional subjects, subjects over 72 chars |
| `pre-commit` | every commit | unformatted code, an invalid roadmap; re-renders `ROADMAP.md` |
| `pre-push` | every push | clippy warnings, failing tests |

Fast checks on commit, slow ones on push: a hook that makes committing feel
expensive is a hook people learn to skip with `--no-verify`.

Hooks live in `.githooks/` and are committed, so a fresh clone gets them:

```
git config core.hooksPath .githooks
```

## Cross-platform checks

`just check-windows` compile-checks the Windows target from macOS or Linux. It
passes `--all-targets` deliberately — without that the test code is not
cross-checked, and a `#[cfg(unix)]` constant referenced from a test will compile
locally and fail on CI.

## Attribution

Nothing in this repository — commit, PR, comment, changelog, or code — attributes
work to a tool, an assistant, or a model. The `commit-msg` hook rejects
`Co-Authored-By:` trailers naming one, generated-by footers, and the usual
markers, so it cannot happen by accident when a tool inserts them by default.

## Pull requests

State the problem, the approach, and what a reviewer should be sceptical about.
The template has a section for that last one; filling it in with "nothing" is a
real answer, leaving it blank is not.

`scripts/agent pr` builds the description from the branch's commits and appends
the tracker item, so the PR and the item cannot drift apart.
