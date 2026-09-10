<!-- cairn:begin -->
## Roadmap and issues

This project tracks its roadmap and issues with `cairn`. Every item is a Markdown file under `cairn/items`, described by the schema in `cairn.toml`.

**Do not create ad-hoc TODO, PLAN or NOTES files.** Create a cairn item instead, so the work appears on the board and in the generated roadmap.

### The loop

1. `cairn next` — what is ready to start. It excludes anything blocked by unfinished dependencies and puts work already in progress first.
2. `cairn claim <ID>` — take it before you start, so no one duplicates the work. `cairn claim --next` picks and claims the top-ranked unclaimed item in one step, and prints its body so you can begin immediately.
3. Do the work. Record what you learn: `cairn set <ID> <field>=<value>` for fields, `cairn note <ID> "<TEXT>"` for anything that needs a sentence — why you chose something, what you tried, what to watch for.
4. `cairn close <ID>` when it is done, or `cairn release <ID>` to hand it back.
5. `cairn check` before you report finished. It must pass.

### Commands

```sh
cairn next --json                 # ready work, ranked
cairn claim --next                # take the next ready item
cairn search <TEXT> --json        # titles, bodies and labels
cairn list --json                 # all open items
cairn list --filter 'blocked=false,priority=p0'
cairn show <ID> --json            # one item, including its body
cairn new "<TITLE>" --type <TYPE> --milestone <MILESTONE>
cairn set <ID> status=<STATUS>    # also labels+=x, or any field below
cairn note <ID> "<TEXT>"          # append reasoning; never replaces
cairn close <ID>
cairn check                       # validate; run before finishing
cairn render                      # regenerate ROADMAP.md
```

### Schema

- **Types**: `feature`, `bug`, `chore`, `docs`, `milestone`
- **Statuses**: `backlog` (open), `planned` (open), `doing` (active), `blocked` (active), `done` (done), `dropped` (dropped)
- **`milestone`**: names a `milestone` item, by key — what this ships in
- **`due`**: date, YYYY-MM-DD — when a milestone is meant to land
- **`part_of`**: names any items, by id, several allowed — a larger piece of work this belongs to
- **`priority`**: one of p0, p1, p2, p3 — p0 is a release blocker
- **`effort`**: one of s, m, l, xl — Rough size, not an estimate
- **`area`**: free text — Subsystem this touches
- **Milestones**: `v0.1` (due 2026-12-01), `v1.0` (due 2027-03-01), `later`
- **Saved views** (`cairn list --view NAME`): `now`, `next`, `triage`

### Rules

1. Before starting work, find or create the item and set it to an active status.
2. Use the fields above rather than inventing new ones; add new fields to `cairn.toml` first.
3. Never hand-edit the generated roadmap file — change items and run `cairn render`.
4. `cairn check` must pass before the work is considered done.


### Project conventions

- **Scope boundaries are filed as `dropped` items**, not deleted. Before proposing
  a feature this project appears to be missing, check
  `cairn search <topic> --all` — five things comparable tools ship were considered
  and declined with reasons (`0055`-`0059`). `search` hides dropped items unless
  you pass `-A`.
- Notes on closed items record *why*, not just *what*. When a fix surfaces a
  second bug, note it on the item rather than only fixing it — several bugs here
  were found that way.
- `cargo fmt`, `cargo clippy --all-targets` and `cairn check` must all be clean;
  CI enforces all three plus a rendered-roadmap-is-in-sync check.


### Git workflow

**Never commit to `main`.** Use `scripts/agent`, which is the whole workflow:

```
scripts/setup                install hooks and the commit template (once, per clone)
scripts/agent new <type> "…" file a tracker item and branch for it in one step
scripts/agent start <id>     worktree + branch + claim the item
scripts/agent check          formatter, linter, tests, roadmap
scripts/agent commit "<msg>" conventional commit with a Refs trailer
scripts/agent pr             push and open a pull request
scripts/agent clean          remove worktrees whose branch has merged
```

Worktrees, not branch switching: several agents work here at once, and two of
them sharing a checkout will collide over the index or `target/`.

`agent new` exists so filing an item never requires a commit on `main` — without
it, the workflow's first rule has to be broken in order to follow it.

Hooks in `.githooks/` reject unformatted code, non-conventional subjects, clippy
warnings, failing tests — and any attribution to a tool or model, which this
repository never carries. Full detail in [docs/git-workflow.md](docs/git-workflow.md).

<!-- cairn:end -->
