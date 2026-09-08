# Common tasks. `just` with no argument lists them.
default:
    @just --list

# Formatter, linter, tests, roadmap — the same gate CI runs.
check:
    @scripts/agent check

# Take a tracker item: worktree, branch, claim.
start id:
    @scripts/agent start {{id}}

# Conventional commit with a Refs trailer.
commit msg:
    @scripts/agent commit "{{msg}}"

# Push and open a pull request.
pr:
    @scripts/agent pr

# Active worktrees.
wt:
    @scripts/agent list

# Remove worktrees whose branch has merged.
tidy:
    @scripts/agent clean

fmt:
    cargo fmt

test:
    cargo test

# Compile-check the Windows target from a host without an MSVC assembler.
check-windows:
    cargo check --target x86_64-pc-windows-msvc --features portable-hash
