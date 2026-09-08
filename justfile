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
# --all-targets matters: without it the tests are never cross-checked, and a
# unix-only constant referenced from a test compiles here and fails on CI.
check-windows:
    cargo check --all-targets --target x86_64-pc-windows-msvc --features portable-hash
