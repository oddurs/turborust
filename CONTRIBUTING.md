# Contributing

```sh
git clone git@github.com:oddurs/turborust.git
cd turborust
scripts/setup        # hooks, commit template, tooling check
cargo test
```

`scripts/setup` is not optional decoration. The git hooks live in local config
rather than tracked content, so without it you get the files and none of the
enforcement — and you find out on your first pull request.

## Making a change

One unit of work, one worktree, one branch, one pull request. `main` only ever
advances through a merge.

```sh
scripts/agent new bug "the thing that is wrong"   # files a tracker item and branches
scripts/agent start 0042                          # or take an existing one
scripts/agent check                               # formatter, lints, tests, roadmap
scripts/agent commit "fix(proc): ..."
scripts/agent pr
```

[docs/git-workflow.md](docs/git-workflow.md) has the detail. `just` mirrors the
same commands if you prefer.

## What is expected of a change

- **A test that fails without it.** For a bug, write the test first and watch it
  fail; a regression test that passes against the old code is not one.
- **Notes that say why.** Commit bodies and tracker notes explain the reasoning,
  not the diff. Several bugs here were found because a previous note recorded
  what had already been ruled out.
- **Honest limits.** If something is unverified, say so where a reader will see
  it rather than where it is technically true. Windows pty support is written up
  this way in the README, and that is the standard.

## The roadmap

Work is tracked with `cairn` as Markdown files in `cairn/items`, rendered to
[ROADMAP.md](ROADMAP.md). `cairn next` shows what is ready to start.

Before proposing something turborust appears to be missing, check
`cairn search <topic> --all`: five capabilities comparable tools ship were
considered and declined, each with its reasoning. `search` hides dropped items
unless you pass `-A`.

## Things deliberately not set up for you

A faster linker is the single largest win available on rebuild time, and
`turborust doctor` will tell you so. It is **not** committed, because the right
answer depends on what is installed on your machine and pinning one would break
everyone without it. Run `turborust doctor --fix` and it will offer the change
for your setup.

## Attribution

Nothing in this repository attributes work to a tool, an assistant or a model —
not in commits, pull requests, comments or code. The `commit-msg` hook rejects it
and CI checks again. See [NOTICE.md](NOTICE.md).
