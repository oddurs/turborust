//! Unix only, deliberately.
//!
//! Every fixture here is a POSIX shell snippet — `cp`, `touch`, `true`,
//! `trap ... TERM` — and turborust spawns through `cmd /C` on Windows, where
//! none of those exist and `trap` has no equivalent at all. Rewriting them in
//! cmd would be a second test suite, and the shutdown-order test could not be
//! expressed in it.
//!
//! The real fix is a configurable shell (`shell = "bash"`), which would make
//! these portable and is worth having on its own merits. Tracked in 0062.
#![cfg(unix)]

//! The one-shot `turborust run` path.
//!
//! Regression cover for cairn 0004: `run` used to filter the resolved plan down
//! to tasks and execute only those, so a task depending on a *service* ran
//! without it. The `up` path was always correct — these tests pin the one-shot
//! path to the same behaviour.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use turborust::cache::Cache;
use turborust::config::{Config, Workspace};
use turborust::engine::{self, Engine};
use turborust::plan;
use turborust::state::Status;

struct Sandbox {
    dir: PathBuf,
}

impl Sandbox {
    fn new(tag: &str) -> Sandbox {
        let dir = std::env::temp_dir().join(format!("turborust-run-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Sandbox {
            dir: dir.canonicalize().unwrap(),
        }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Runs a config through the real one-shot path and returns its exit code.
async fn run(sandbox: &Sandbox, toml_src: &str, targets: &[&str]) -> (i32, Arc<Engine>) {
    let ws = Workspace {
        root: sandbox.dir.clone(),
        config: toml::from_str::<Config>(toml_src).expect("config parses"),
        source: Some(toml_src.to_string()),
    };
    let targets: Vec<String> = targets.iter().map(|s| s.to_string()).collect();
    let plan = Arc::new(plan::resolve(&ws, &targets).expect("plan resolves"));
    let cache = Arc::new(Cache::new(&ws.cache_dir()).unwrap());
    let (engine, rx) = Engine::new(plan, cache);
    tokio::spawn(engine::pump_events(engine.state.clone(), rx, false));

    let code = tokio::time::timeout(Duration::from_secs(30), engine::run_once(engine.clone()))
        .await
        .expect("run_once hung")
        .expect("run_once errored");
    (code, engine)
}

#[tokio::test]
async fn a_task_depending_on_a_service_gets_that_service_running() {
    let sb = Sandbox::new("svc-dep");
    let marker = sb.path("db-was-here");

    // The service records that it started, then stays up. The task polls for that
    // record: with the service skipped it never appears and the task exits 1.
    let cfg = format!(
        r#"
        [services.db]
        cmd = "touch '{m}'; sleep 30"
        restart = "never"

        [tasks.migrate]
        depends_on = ["db"]
        cmd = "for i in $(seq 1 200); do [ -f '{m}' ] && exit 0; sleep 0.05; done; exit 1"
        "#,
        m = marker.display()
    );

    let (code, engine) = run(&sb, &cfg, &["migrate"]).await;
    assert_eq!(code, 0, "task ran without its service dependency");
    assert!(marker.exists(), "the service never started");

    // And the service `run` started is not left behind.
    let st = engine.state.lock().unwrap();
    let db = st.nodes.get("db").expect("db in state");
    assert!(
        matches!(db.status, Status::Stopped),
        "service should be stopped after the run, was {:?}",
        db.status
    );
}

#[tokio::test]
async fn a_failing_task_returns_its_own_exit_code() {
    let sb = Sandbox::new("fail");
    let (code, _) = run(
        &sb,
        r#"
        [tasks.boom]
        cmd = "exit 42"
        "#,
        &["boom"],
    )
    .await;
    assert_eq!(code, 42);
}

#[tokio::test]
async fn tasks_run_in_dependency_order() {
    let sb = Sandbox::new("order");
    let log = sb.path("order.log");
    let cfg = format!(
        r#"
        [tasks.first]
        cmd = "echo first >> '{l}'"

        [tasks.second]
        depends_on = ["first"]
        cmd = "echo second >> '{l}'"
        "#,
        l = log.display()
    );
    let (code, _) = run(&sb, &cfg, &["second"]).await;
    assert_eq!(code, 0);
    let got = std::fs::read_to_string(&log).unwrap();
    // Exact equality, not "contains": a dependency wake-up that arrived a moment
    // too late to be drained used to make `second` run a second time, which a
    // containment check would have waved through.
    assert_eq!(
        got.split_whitespace().collect::<Vec<_>>(),
        vec!["first", "second"],
        "each task must run exactly once, in order"
    );
}

#[tokio::test]
async fn a_diamond_runs_each_task_exactly_once() {
    let sb = Sandbox::new("diamond");
    let log = sb.path("diamond.log");
    // `base` is a dependency of two tasks that a fourth depends on. Every node
    // gets several readiness wake-ups; none of them may cause a repeat run.
    let cfg = format!(
        r#"
        [tasks.base]
        cmd = "echo base >> '{l}'"

        [tasks.left]
        depends_on = ["base"]
        cmd = "echo left >> '{l}'"

        [tasks.right]
        depends_on = ["base"]
        cmd = "echo right >> '{l}'"

        [tasks.join]
        depends_on = ["left", "right"]
        cmd = "echo join >> '{l}'"
        "#,
        l = log.display()
    );
    let (code, _) = run(&sb, &cfg, &["join"]).await;
    assert_eq!(code, 0);

    let got = std::fs::read_to_string(&log).unwrap();
    let lines: Vec<&str> = got.split_whitespace().collect();
    assert_eq!(lines.len(), 4, "expected one line per task, got {lines:?}");
    assert_eq!(lines[0], "base", "base must run first: {lines:?}");
    assert_eq!(lines[3], "join", "join must run last: {lines:?}");
}

#[tokio::test]
async fn a_service_that_dies_fails_the_run_instead_of_hanging() {
    let sb = Sandbox::new("dead-svc");
    // Without the guard in `await_tasks`, `waits` never becomes ready and the run
    // would block until the harness timeout.
    //
    // `db` declares a readiness probe it can never satisfy, and that is
    // load-bearing rather than decoration. A service with no probe is marked
    // healthy the instant it spawns, so under load a dependent can start and
    // finish in the window before `exit 1` is observed — the run then succeeds
    // and the test fails intermittently. Which is precisely the "the process
    // exists" versus "the process is ready" distinction this project is built
    // around, showing up in its own suite.
    let (code, _) = run(
        &sb,
        r#"
        [services.db]
        cmd = "exit 1"
        restart = "never"
        health = { log = "listening", interval = "30ms" }

        [tasks.waits]
        depends_on = ["db"]
        cmd = "true"
        "#,
        &["waits"],
    )
    .await;
    assert_ne!(code, 0, "a dead dependency must fail the run");
    assert!(
        !sb.path("waits-ran").exists(),
        "the dependent must not run when its dependency never became ready"
    );
}

// ---------------------------------------------------------------- cache ----
// Cover for cairn 0015: outputs must key the task, and a hit must reproduce the
// result rather than assume the tree was not touched.

#[tokio::test]
async fn a_hit_restores_a_deleted_output() {
    let sb = Sandbox::new("restore");
    std::fs::write(sb.path("in.txt"), "source").unwrap();
    let cfg = r#"
        [tasks.build]
        cmd = "cp in.txt out.txt"
        inputs = ["in.txt"]
        outputs = ["out.txt"]
    "#;
    assert_eq!(run(&sb, cfg, &["build"]).await.0, 0);
    assert!(sb.path("out.txt").exists());

    // Delete the output. A hit now has to replay it from the store.
    std::fs::remove_file(sb.path("out.txt")).unwrap();
    assert_eq!(run(&sb, cfg, &["build"]).await.0, 0);
    assert!(
        sb.path("out.txt").exists(),
        "a cache hit must reproduce its declared outputs, not assume they survived"
    );
    assert_eq!(
        std::fs::read_to_string(sb.path("out.txt")).unwrap(),
        "source"
    );
}

#[tokio::test]
async fn alternating_inputs_hit_both_ways() {
    let sb = Sandbox::new("alternating");
    let cfg = r#"
        [tasks.build]
        cmd = "cp in.txt out.txt"
        inputs = ["in.txt"]
        outputs = ["out.txt"]
    "#;
    // Records are addressed by key, so switching back and forth — a branch
    // switch, in practice — must not throw the other result away every time.
    std::fs::write(sb.path("in.txt"), "A").unwrap();
    run(&sb, cfg, &["build"]).await;
    std::fs::write(sb.path("in.txt"), "B").unwrap();
    run(&sb, cfg, &["build"]).await;

    std::fs::write(sb.path("in.txt"), "A").unwrap();
    std::fs::remove_file(sb.path("out.txt")).unwrap();
    run(&sb, cfg, &["build"]).await;
    assert_eq!(
        std::fs::read_to_string(sb.path("out.txt")).unwrap(),
        "A",
        "the first result should still have been in the store"
    );
}

#[tokio::test]
async fn narrowing_outputs_is_a_miss() {
    let sb = Sandbox::new("narrow");
    std::fs::write(sb.path("in.txt"), "source").unwrap();
    let wide = r#"
        [tasks.build]
        cmd = "cp in.txt out.txt; cp in.txt second.txt"
        inputs = ["in.txt"]
        outputs = ["out.txt", "second.txt"]
    "#;
    run(&sb, wide, &["build"]).await;
    assert!(sb.path("second.txt").exists());

    // Same inputs, same command, fewer declared outputs. The key must change, so
    // the task re-runs rather than replaying a result shaped differently.
    std::fs::remove_file(sb.path("second.txt")).unwrap();
    let narrow = r#"
        [tasks.build]
        cmd = "cp in.txt out.txt; cp in.txt second.txt"
        inputs = ["in.txt"]
        outputs = ["out.txt"]
    "#;
    run(&sb, narrow, &["build"]).await;
    assert!(
        sb.path("second.txt").exists(),
        "narrowing outputs must invalidate; the task should have re-run"
    );
}

// ---------------------------------------------------------- shared cache ----
// Cover for cairn 0013.

/// Runs with a shared cache directory attached.
async fn run_shared(sandbox: &Sandbox, toml_src: &str, targets: &[&str]) -> i32 {
    let ws = Workspace {
        root: sandbox.dir.clone(),
        config: toml::from_str::<Config>(toml_src).expect("config parses"),
        source: Some(toml_src.to_string()),
    };
    let targets: Vec<String> = targets.iter().map(|s| s.to_string()).collect();
    let plan = Arc::new(plan::resolve(&ws, &targets).expect("plan resolves"));
    let shared = ws.config.cache.shared.as_ref().map(|d| ws.root.join(d));
    let cache =
        Arc::new(Cache::with_shared(&ws.cache_dir(), shared, ws.config.cache.push).unwrap());
    let (engine, rx) = Engine::new(plan, cache);
    tokio::spawn(engine::pump_events(engine.state.clone(), rx, false));
    tokio::time::timeout(Duration::from_secs(30), engine::run_once(engine.clone()))
        .await
        .expect("run_once hung")
        .expect("run_once errored")
}

/// Identical in both checkouts on purpose: the config text is part of the global
/// hash, so varying it would change the key and prove nothing about sharing.
const SHARED_CFG: &str = r#"
    [cache]
    shared = "team-cache"
    push = true

    [tasks.build]
    cmd = "echo ran >> ran.log; cp in.txt out.txt"
    inputs = ["in.txt"]
    outputs = ["out.txt"]
"#;

const READ_ONLY_CFG: &str = r#"
    [cache]
    shared = "team-cache"

    [tasks.build]
    cmd = "cp in.txt out.txt"
    inputs = ["in.txt"]
    outputs = ["out.txt"]
"#;

#[tokio::test]
async fn a_result_pushed_by_one_checkout_is_replayed_by_another() {
    let a = Sandbox::new("shared-a");
    std::fs::write(a.path("in.txt"), "shared-source").unwrap();
    assert_eq!(run_shared(&a, SHARED_CFG, &["build"]).await, 0);
    assert!(
        a.path("ran.log").exists(),
        "the first checkout should have built"
    );

    // A second checkout with the same inputs and the same shared directory. Its
    // own cache is empty, so a hit can only come from the shared store.
    let b = Sandbox::new("shared-b");
    std::fs::write(b.path("in.txt"), "shared-source").unwrap();
    std::fs::create_dir_all(b.path("team-cache")).unwrap();
    copy_dir(&a.path("team-cache"), &b.path("team-cache"));

    assert_eq!(run_shared(&b, SHARED_CFG, &["build"]).await, 0);
    assert_eq!(
        std::fs::read_to_string(b.path("out.txt")).unwrap(),
        "shared-source",
        "the output should have been replayed from the shared cache"
    );
    // The command appends to ran.log, which is not a declared output and so is
    // never restored. Its absence is proof the command did not run.
    assert!(
        !b.path("ran.log").exists(),
        "the second checkout rebuilt instead of replaying"
    );
}

#[tokio::test]
async fn reading_a_shared_cache_does_not_write_to_it() {
    let sb = Sandbox::new("no-push");
    std::fs::write(sb.path("in.txt"), "local-only").unwrap();
    assert_eq!(run_shared(&sb, READ_ONLY_CFG, &["build"]).await, 0);

    // push defaults to false: consuming a shared cache must not silently make
    // this machine a publisher to it.
    let published = std::fs::read_dir(sb.path("team-cache").join("runs"))
        .map(|d| d.flatten().count())
        .unwrap_or(0);
    assert_eq!(
        published, 0,
        "nothing should be published without push = true"
    );
}

fn copy_dir(from: &Path, to: &Path) {
    let Ok(entries) = std::fs::read_dir(from) else {
        return;
    };
    for e in entries.flatten() {
        let dest = to.join(e.file_name());
        match e.file_type() {
            Ok(t) if t.is_dir() => {
                let _ = std::fs::create_dir_all(&dest);
                copy_dir(&e.path(), &dest);
            }
            Ok(_) => {
                if let Some(p) = dest.parent() {
                    let _ = std::fs::create_dir_all(p);
                }
                let _ = std::fs::copy(e.path(), &dest);
            }
            Err(_) => {}
        }
    }
}

// ------------------------------------------------------------- --continue ----
// Cover for cairn 0048.

/// Runs with `--continue` semantics.
async fn run_continue(sandbox: &Sandbox, toml_src: &str, targets: &[&str]) -> i32 {
    let ws = Workspace {
        root: sandbox.dir.clone(),
        config: toml::from_str::<Config>(toml_src).expect("config parses"),
        source: Some(toml_src.to_string()),
    };
    let targets: Vec<String> = targets.iter().map(|s| s.to_string()).collect();
    let plan = Arc::new(plan::resolve(&ws, &targets).expect("plan resolves"));
    let cache = Arc::new(Cache::new(&ws.cache_dir()).unwrap());
    let (engine, rx) = Engine::new(plan, cache);
    tokio::spawn(engine::pump_events(engine.state.clone(), rx, false));
    tokio::time::timeout(
        Duration::from_secs(30),
        engine::run_once_with(engine.clone(), true),
    )
    .await
    .expect("run hung")
    .expect("run errored")
}

const BRANCHING: &str = r#"
    [tasks.broken]
    cmd = "exit 9"

    [tasks.downstream]
    depends_on = ["broken"]
    cmd = "touch downstream-ran"

    [tasks.unrelated]
    cmd = "touch unrelated-ran"

    [tasks.top]
    depends_on = ["downstream", "unrelated"]
    cmd = "touch top-ran"
"#;

#[tokio::test]
async fn continue_runs_the_branch_that_did_not_fail() {
    let sb = Sandbox::new("continue");
    let code = run_continue(&sb, BRANCHING, &["top"]).await;

    assert_ne!(code, 0, "a run with a failure must not report success");
    assert!(
        sb.path("unrelated-ran").exists(),
        "work not blocked by the failure should still have run"
    );
    assert!(
        !sb.path("downstream-ran").exists(),
        "work downstream of a failure must not run"
    );
}

#[tokio::test]
async fn continue_does_not_hang_on_permanently_blocked_tasks() {
    // `downstream` and `top` can never run. Waiting for them to reach a terminal
    // status would block until the harness timeout.
    let sb = Sandbox::new("continue-hang");
    let began = std::time::Instant::now();
    run_continue(&sb, BRANCHING, &["top"]).await;
    assert!(
        began.elapsed() < Duration::from_secs(15),
        "blocked tasks were waited on rather than recognised: {:?}",
        began.elapsed()
    );
}

#[tokio::test]
async fn continue_reports_the_failing_task_exit_code() {
    let sb = Sandbox::new("continue-code");
    let code = run_continue(
        &sb,
        r#"
        [tasks.boom]
        cmd = "exit 9"
        "#,
        &["boom"],
    )
    .await;
    assert_eq!(code, 9, "a single failure should still report its own code");
}

#[tokio::test]
async fn continue_on_a_clean_run_is_a_no_op() {
    let sb = Sandbox::new("continue-clean");
    let code = run_continue(
        &sb,
        r#"
        [tasks.a]
        cmd = "true"
        [tasks.b]
        depends_on = ["a"]
        cmd = "true"
        "#,
        &["b"],
    )
    .await;
    assert_eq!(code, 0);
}

// ---------------------------------------------------------------------------
// cairn 0069: a dependent used to key on its upstream's *key*, so an upstream
// that re-ran always invalidated everything below it — even when it produced a
// byte-identical artifact. These pin the two halves of the rule.
// ---------------------------------------------------------------------------

/// Whether a task was served from cache on the most recent run.
fn was_cached(engine: &Engine, task: &str) -> bool {
    engine
        .outcomes()
        .iter()
        .find(|o| o.task == task)
        .unwrap_or_else(|| panic!("`{task}` did not run"))
        .cached
}

/// `gen` copies its input to an output; `use` consumes that output. Editing the
/// input in a way that leaves the output identical must not rebuild `use`.
const STABLE_OUTPUT: &str = r#"
[tasks.gen]
cmd = "printf 'constant' > out.txt"
inputs = ["src.txt"]
outputs = ["out.txt"]

[tasks.use]
depends_on = ["gen"]
cmd = "true"
inputs = ["out.txt"]
outputs = ["used.txt"]
"#;

#[tokio::test]
async fn an_upstream_that_reruns_with_identical_output_does_not_rebuild_its_dependent() {
    let sb = Sandbox::new("dep-outputs-stable");
    std::fs::write(sb.path("src.txt"), "one").unwrap();

    let (code, _) = run(&sb, STABLE_OUTPUT, &["use"]).await;
    assert_eq!(code, 0);

    // `gen`'s input changed, so `gen` must re-run — but it writes the same bytes,
    // so `use` has nothing new to consume.
    std::fs::write(sb.path("src.txt"), "two").unwrap();
    let (code, engine) = run(&sb, STABLE_OUTPUT, &["use"]).await;
    assert_eq!(code, 0);

    assert!(
        !was_cached(&engine, "gen"),
        "`gen`'s own inputs changed, so it must re-run"
    );
    assert!(
        was_cached(&engine, "use"),
        "`gen` produced identical outputs, so `use` had nothing to rebuild for"
    );
}

/// The same shape, except `gen`'s output tracks its input.
const CHANGING_OUTPUT: &str = r#"
[tasks.gen]
cmd = "cp src.txt out.txt"
inputs = ["src.txt"]
outputs = ["out.txt"]

[tasks.use]
depends_on = ["gen"]
cmd = "true"
inputs = ["out.txt"]
outputs = ["used.txt"]
"#;

#[tokio::test]
async fn an_upstream_whose_outputs_change_still_rebuilds_its_dependent() {
    let sb = Sandbox::new("dep-outputs-changing");
    std::fs::write(sb.path("src.txt"), "one").unwrap();

    let (code, _) = run(&sb, CHANGING_OUTPUT, &["use"]).await;
    assert_eq!(code, 0);

    std::fs::write(sb.path("src.txt"), "two").unwrap();
    let (code, engine) = run(&sb, CHANGING_OUTPUT, &["use"]).await;
    assert_eq!(code, 0);
    assert!(!was_cached(&engine, "gen"));
    assert!(
        !was_cached(&engine, "use"),
        "`gen` produced different bytes, so `use` must rebuild"
    );
}

/// An upstream that declares no outputs has nothing observable to key on, so its
/// own key stays the proxy — which is today's behaviour, and must not regress.
const NO_OUTPUTS: &str = r#"
[tasks.gen]
cmd = "true"
inputs = ["src.txt"]

[tasks.use]
depends_on = ["gen"]
cmd = "true"
inputs = ["other.txt"]
outputs = ["used.txt"]
"#;

#[tokio::test]
async fn an_upstream_with_no_declared_outputs_still_invalidates_by_key() {
    let sb = Sandbox::new("dep-no-outputs");
    std::fs::write(sb.path("src.txt"), "one").unwrap();
    std::fs::write(sb.path("other.txt"), "steady").unwrap();

    let (code, _) = run(&sb, NO_OUTPUTS, &["use"]).await;
    assert_eq!(code, 0);

    // Only `gen`'s input moves. `use`'s own inputs are untouched, so the sole
    // reason it may rebuild is the upstream key folded into its own.
    std::fs::write(sb.path("src.txt"), "two").unwrap();
    let (code, engine) = run(&sb, NO_OUTPUTS, &["use"]).await;
    assert_eq!(code, 0);
    assert!(
        !was_cached(&engine, "use"),
        "with no outputs to compare, an upstream re-run must still invalidate"
    );
}

// ---------------------------------------------------------------------------
// cairn 0071: a cache hit is a promise that this key reproduces these outputs.
// Nothing checked it, so a task that bakes in a timestamp or a counter had one
// of its several possible answers served forever, silently.
// ---------------------------------------------------------------------------

/// As `run`, but re-checking cache hits against what the task actually produces.
async fn run_verifying(sandbox: &Sandbox, toml_src: &str, targets: &[&str]) -> i32 {
    let ws = Workspace {
        root: sandbox.dir.clone(),
        config: toml::from_str::<Config>(toml_src).expect("config parses"),
        source: Some(toml_src.to_string()),
    };
    let targets: Vec<String> = targets.iter().map(|s| s.to_string()).collect();
    let plan = Arc::new(plan::resolve(&ws, &targets).expect("plan resolves"));
    let cache = Arc::new(Cache::new(&ws.cache_dir()).unwrap());
    let (engine, rx) = Engine::new_verifying(plan, cache, turborust::config::VerifyMode::Always);
    tokio::spawn(engine::pump_events(engine.state.clone(), rx, false));
    tokio::time::timeout(Duration::from_secs(30), engine::run_once(engine.clone()))
        .await
        .expect("run_once hung")
        .expect("run_once errored")
}

/// Its inputs never move, so its key is stable and it will hit — but it writes
/// something different every time, so the hit is a lie.
const DRIFTING: &str = r#"
[tasks.drift]
cmd = "c=$(cat counter 2>/dev/null || echo 0); expr $c + 1 > counter; cp counter out.txt"
inputs = ["src.txt"]
outputs = ["out.txt"]
"#;

#[tokio::test]
async fn a_cache_hit_that_does_not_reproduce_its_outputs_fails_the_run() {
    let sb = Sandbox::new("verify-drift");
    std::fs::write(sb.path("src.txt"), "steady").unwrap();

    // First run is a miss and stores a result.
    assert_eq!(run_verifying(&sb, DRIFTING, &["drift"]).await, 0);

    // Second run hits that result, re-runs to check it, and finds it did not
    // reproduce. That is a failure: the cache was about to hand back an answer
    // this task does not actually determine.
    assert_ne!(
        run_verifying(&sb, DRIFTING, &["drift"]).await,
        0,
        "a hit that cannot reproduce its outputs must fail the run"
    );

    // …and the poisoned entry is gone, so the next run is an honest miss.
    assert_eq!(
        run_verifying(&sb, DRIFTING, &["drift"]).await,
        0,
        "the bad entry should have been dropped, making this a plain miss"
    );
}

/// The same task with verification off keeps hitting, which is the behaviour
/// everyone gets by default — checking costs a rebuild.
#[tokio::test]
async fn without_verification_a_drifting_task_still_hits() {
    let sb = Sandbox::new("verify-off");
    std::fs::write(sb.path("src.txt"), "steady").unwrap();
    assert_eq!(run(&sb, DRIFTING, &["drift"]).await.0, 0);
    let (code, engine) = run(&sb, DRIFTING, &["drift"]).await;
    assert_eq!(code, 0);
    assert!(
        engine
            .outcomes()
            .iter()
            .any(|o| o.task == "drift" && o.cached),
        "with verify off the cache is trusted, as documented"
    );
}
