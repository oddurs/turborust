//! What the binary says when something goes wrong before a process exists.
//!
//! Regression cover for cairn 0086: a failure with no process behind it — a
//! spawn that never happened — left `run` exiting 1 with nothing printed,
//! because a node's output was only shown once a process exited.
#![cfg(unix)]

use std::process::Command;

#[test]
fn run_says_why_a_task_could_not_start() {
    let dir = std::env::temp_dir().join(format!("turborust-errors-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("turborust.toml"),
        "[tasks.build]\ncwd = \"no/such/dir\"\ncmd = \"true\"\n",
    )
    .unwrap();

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_turborust"));
    cmd.args(["run", "build"]).current_dir(&dir);
    // Run from a git hook, these would point turborust at turborust's own
    // repository for its cache store (cairn 0083).
    for var in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_COMMON_DIR",
    ] {
        cmd.env_remove(var);
    }
    let out = cmd.output().expect("turborust runs");
    let _ = std::fs::remove_dir_all(&dir);

    let said = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!out.status.success());
    assert!(
        said.contains("working directory") && said.contains("does not exist"),
        "the reason should be printed, got:\n{said}"
    );
}
