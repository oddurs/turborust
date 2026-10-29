//! The bash glue for node-name completion, in a real bash (cairn 0061).
#![cfg(unix)]

use std::process::Command;

#[test]
fn bash_completes_node_names_and_falls_through_elsewhere() {
    if Command::new("bash").arg("--version").output().is_err() {
        return;
    }
    let bin = std::path::Path::new(env!("CARGO_BIN_EXE_turborust"));
    let dir = std::env::temp_dir().join(format!("turborust-completion-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("turborust.toml"),
        "[tasks.build]\ncmd = \"x\"\n[tasks.check]\ncmd = \"x\"\n[services.web]\ncmd = \"x\"\n",
    )
    .unwrap();
    let script = Command::new(bin)
        .args(["completions", "bash"])
        .output()
        .unwrap()
        .stdout;
    std::fs::write(dir.join("c.bash"), script).unwrap();
    // No negative indices: macOS ships bash 3.2.
    let probe = r#"
        source ./c.bash
        t() { COMP_WORDS=("$@"); COMP_CWORD=$(($# - 1)); COMPREPLY=()
              _turborust_nodes turborust "${COMP_WORDS[COMP_CWORD]}" "${COMP_WORDS[COMP_CWORD-1]}"
              echo "${COMPREPLY[*]}"; }
        t turborust run b
        t turborust why ""
        t turborust up -F w
        t turborust run --base x
    "#;
    let out = Command::new("bash")
        .args(["-c", probe])
        .current_dir(&dir)
        .env(
            "PATH",
            format!(
                "{}:{}",
                bin.parent().unwrap().display(),
                std::env::var("PATH").unwrap()
            ),
        )
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    let said = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<&str> = said.lines().collect();
    assert_eq!(
        lines,
        ["build", "build check web", "web", ""],
        "{said}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
