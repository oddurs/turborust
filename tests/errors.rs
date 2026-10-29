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

#[test]
fn init_imports_a_procfile_and_names_what_it_skipped() {
    // Cover for cairn 0097: a line init cannot translate is said out loud, not
    // left for the user to notice missing once `up` is running.
    let dir = std::env::temp_dir().join(format!("turborust-init-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("Procfile"),
        "web: ./serve -p $PORT\nrelease: ./migrate\n",
    )
    .unwrap();

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_turborust"));
    cmd.arg("init").current_dir(&dir);
    for var in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_COMMON_DIR",
    ] {
        cmd.env_remove(var);
    }
    let out = cmd.output().expect("turborust runs");
    let written = std::fs::read_to_string(dir.join("turborust.toml")).unwrap_or_default();
    let _ = std::fs::remove_dir_all(&dir);

    let said = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{said}");
    assert!(
        said.contains("imported 1 process(es) from Procfile"),
        "{said}"
    );
    assert!(
        said.contains("skipped Procfile line 2: `release`"),
        "{said}"
    );
    assert!(written.contains("[services.web]"), "{written}");
}

#[test]
fn run_summarize_writes_its_record_and_exits() {
    // `--summarize` once took the state lock twice in one statement and hung
    // after every run, with nothing left running; nothing exercised it.
    let dir = std::env::temp_dir().join(format!("turborust-summary-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("turborust.toml"),
        "[tasks.test]\ncmd = \"echo \\\"thread 'tests::adds' panicked at src/lib.rs:9:5:\\\"; echo nope; echo; exit 101\"\n",
    )
    .unwrap();

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_turborust"));
    cmd.args(["run", "test", "--summarize"])
        .current_dir(&dir)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    for var in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_COMMON_DIR",
    ] {
        cmd.env_remove(var);
    }
    let mut child = cmd.spawn().expect("turborust runs");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break Some(status);
        }
        if std::time::Instant::now() > deadline {
            let _ = child.kill();
            break None;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    };
    let record = std::fs::read_dir(dir.join(".turborust/summaries"))
        .ok()
        .and_then(|mut d| d.next())
        .and_then(|e| std::fs::read_to_string(e.ok()?.path()).ok())
        .unwrap_or_default();
    let _ = std::fs::remove_dir_all(&dir);

    let status = status.expect("`run --summarize` should exit, not hang");
    assert_eq!(status.code(), Some(101));
    let json: serde_json::Value = serde_json::from_str(&record).expect("a JSON record");
    let issue = &json["issues"][0];
    assert_eq!(issue["code"], "panic", "{record}");
    assert_eq!(issue["file"], "src/lib.rs", "{record}");
    assert_eq!(json["nodes"]["test"]["failures"], 1, "{record}");
}

/// Runs the binary in a fresh directory holding `config`, if any.
fn turborust_in(tag: &str, config: Option<&str>, args: &[&str]) -> std::process::Output {
    let dir = std::env::temp_dir().join(format!("turborust-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    if let Some(c) = config {
        std::fs::write(dir.join("turborust.toml"), c).unwrap();
    }
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_turborust"));
    cmd.args(args)
        .current_dir(&dir)
        .env_remove("TURBORUST_ERROR_FORMAT");
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
    out
}

#[test]
fn a_failure_has_a_stable_code_under_error_format_json() {
    // Cover for cairn 0078: a non-zero exit a pipeline can branch on without
    // reading English.
    let fine = "[tasks.a]\ncmd = \"true\"\n";
    let cases = [
        ("no-config", None, "plan", "config-not-found"),
        (
            "bad-toml",
            Some("[tasks.a\ncmd = 1"),
            "plan",
            "config-invalid",
        ),
        (
            "unknown-dep",
            Some("[tasks.a]\ncmd = \"true\"\ndepends_on = [\"nope\"]\n"),
            "plan",
            "config-invalid",
        ),
        ("unknown-target", Some(fine), "run", "plan-invalid"),
        ("nothing-up", Some(fine), "connect", "not-running"),
    ];
    for (tag, config, command, code) in cases {
        let args: &[&str] = match command {
            "run" => &["run", "nope", "--error-format", "json"],
            "connect" => &["connect", "a", "--error-format", "json"],
            _ => &["plan", "--error-format", "json"],
        };
        let out = turborust_in(tag, config, args);
        assert_eq!(out.status.code(), Some(1), "{tag}");
        let stderr = String::from_utf8_lossy(&out.stderr);
        let report: serde_json::Value = serde_json::from_str(stderr.trim())
            .unwrap_or_else(|e| panic!("{tag}: stderr is not one JSON value ({e}):\n{stderr}"));
        assert_eq!(report["schemaVersion"], 1, "{tag}");
        assert_eq!(report["error"]["code"], code, "{tag}: {stderr}");
        assert!(
            report["error"]["message"]
                .as_str()
                .is_some_and(|m| !m.is_empty())
        );
    }

    // The text form is what it always was.
    let out = turborust_in("no-config-text", None, &["plan"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.starts_with("Error: no turborust.toml found"),
        "{stderr}"
    );
}

#[test]
fn every_json_payload_says_which_version_it_is() {
    let config = "[tasks.a]\ncmd = \"true\"\ninputs = [\"turborust.toml\"]\n";
    for args in [
        &["run", "a", "--dry-run=json"][..],
        &["cache", "--json"][..],
    ] {
        let out = turborust_in("versioned", Some(config), args);
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let v: serde_json::Value =
            serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("{args:?}: {e}"));
        assert_eq!(v["schemaVersion"], 1, "{args:?}: {v}");
    }
}
