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

//! Integration harness for the supervision tree (cairn 0006).
//!
//! The supervisor is the most intricate part of the project and, until this
//! existed, was verified by hand with a shell script. Every regression found in
//! it so far was found by running that script and reading the log.
//!
//! These drive the real engine against real processes in a temp workspace and
//! assert on the state timeline rather than on wall-clock sleeps.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use turborust::cache::Cache;
use turborust::config::{Config, Workspace};
use turborust::engine::{self, Engine, Wire};
use turborust::plan::{self, Plan};
use turborust::state::Status;

struct Harness {
    dir: PathBuf,
    engine: Arc<Engine>,
    wires: Arc<BTreeMap<String, Wire>>,
    supervisors: Option<engine::Supervisors>,
    plan: Arc<Plan>,
}

impl Harness {
    fn start(tag: &str, toml_src: &str, targets: &[&str]) -> Harness {
        Self::start_in(tag, toml_src, targets, |_| {})
    }

    /// `start`, with the workspace laid out by `setup` before the plan is
    /// resolved — which is when cargo is asked about it.
    fn start_in(
        tag: &str,
        toml_src: &str,
        targets: &[&str],
        setup: impl FnOnce(&std::path::Path),
    ) -> Harness {
        let dir = std::env::temp_dir().join(format!("turborust-sup-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let dir = dir.canonicalize().unwrap();
        setup(&dir);

        let ws = Workspace {
            root: dir.clone(),
            config: toml::from_str::<Config>(toml_src).expect("config parses"),
            source: Some(toml_src.to_string()),
        };
        let targets: Vec<String> = targets.iter().map(|s| s.to_string()).collect();
        let plan = Arc::new(plan::resolve(&ws, &targets).expect("plan resolves"));
        let cache = Arc::new(Cache::new(&ws.cache_dir()).unwrap());
        let (engine, rx) = Engine::new(plan.clone(), cache);
        tokio::spawn(engine::pump_events(engine.state.clone(), rx, false));

        let (wires, supervisors) = engine::spawn_supervisors(engine.clone());
        Harness {
            dir,
            engine,
            wires: Arc::new(wires),
            supervisors: Some(supervisors),
            plan,
        }
    }

    /// Creates a directory in the workspace before anything watches it.
    fn seed(self, rel: &str) -> Self {
        std::fs::create_dir_all(self.dir.join(rel)).unwrap();
        self
    }

    /// Also route filesystem changes, as `turborust up` does.
    fn with_watcher(self) -> Self {
        self.with_backend(turborust::watch::Backend::Native)
    }

    fn with_backend(self, backend: turborust::watch::Backend) -> Self {
        let w =
            turborust::watch::watch_with(&self.dir, Duration::from_millis(60), backend).unwrap();
        engine::spawn_watch_dispatch(
            w,
            self.engine.clone(),
            self.plan.clone(),
            self.wires.clone(),
        )
        .unwrap();
        // Let the OS watch arm before anything touches the tree.
        std::thread::sleep(Duration::from_millis(350));
        self
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }

    fn status(&self, node: &str) -> Status {
        self.engine.state.lock().unwrap().nodes[node].status.clone()
    }

    fn restarts(&self, node: &str) -> u32 {
        self.engine.state.lock().unwrap().nodes[node].restarts
    }

    /// Waits for a predicate over the shared state, or panics with the timeline.
    async fn wait_for<F>(&self, what: &str, mut pred: F)
    where
        F: FnMut(&turborust::state::AppState) -> bool,
    {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        loop {
            if pred(&self.engine.state.lock().unwrap()) {
                return;
            }
            if tokio::time::Instant::now() >= deadline {
                let st = self.engine.state.lock().unwrap();
                let now: Vec<String> = st
                    .nodes
                    .iter()
                    .map(|(n, s)| format!("{n}={}", s.status.label()))
                    .collect();
                // The timeline is what explains an intermittent failure; the
                // final state alone only says that it happened.
                let log: Vec<String> = st
                    .logs
                    .view(None)
                    .iter()
                    .map(|l| format!("  {} | {}", l.proc, turborust::proc::strip_ansi(&l.text)))
                    .collect();
                panic!(
                    "timed out waiting for {what}; state was [{}]\nhistory:\n{}log:\n{}",
                    now.join(", "),
                    st.history.describe(),
                    log.join("\n")
                );
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    async fn shutdown(&mut self) {
        if let Some(s) = self.supervisors.take() {
            engine::shutdown(&self.engine, &self.wires, s).await;
        }
        // Real processes, real timing: the history must still rebuild the
        // state it describes (cairn 0107).
        let st = self.engine.state.lock().unwrap();
        assert_eq!(
            st.history.project(),
            st.lives(),
            "the history does not rebuild the state:\n{}",
            st.history.describe()
        );
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[tokio::test]
async fn a_dependent_starts_only_after_its_dependency_is_healthy() {
    // `db` needs a moment before it announces itself. If readiness were "the
    // process exists", `api` would start immediately and miss it.
    let h = Harness::start(
        "gating",
        r#"
        [services.db]
        cmd = "sleep 1; echo db-listening; sleep 30"
        health = { log = "db-listening", interval = "50ms" }

        [services.api]
        cmd = "echo api-up; sleep 30"
        depends_on = ["db"]
        "#,
        &["api"],
    );

    // Before db is ready, api must be parked rather than running.
    h.wait_for("db to start", |st| {
        matches!(st.nodes["db"].status, Status::Starting)
    })
    .await;
    assert!(
        matches!(h.status("api"), Status::Waiting(_) | Status::Pending),
        "api started before its dependency was ready: {:?}",
        h.status("api")
    );

    h.wait_for("both healthy", |st| {
        matches!(st.nodes["db"].status, Status::Healthy)
            && matches!(st.nodes["api"].status, Status::Healthy)
    })
    .await;
}

#[tokio::test]
async fn readiness_can_be_announced_on_stdout() {
    // Cover for 0005: a service with no port worth probing.
    let mut h = Harness::start(
        "logprobe",
        r#"
        [services.trunk]
        cmd = "sleep 1; echo 'Serving at http://127.0.0.1:9999'; sleep 30"
        health = { log = "Serving at", interval = "50ms" }
        ready_timeout = "15s"
        "#,
        &["trunk"],
    );

    assert!(matches!(
        h.status("trunk"),
        Status::Pending | Status::Starting
    ));
    h.wait_for("the announcement", |st| {
        matches!(st.nodes["trunk"].status, Status::Healthy)
    })
    .await;
    h.shutdown().await;
}

#[tokio::test]
async fn a_service_that_never_announces_never_becomes_healthy() {
    let mut h = Harness::start(
        "logprobe-neg",
        r#"
        [services.quiet]
        cmd = "echo nothing-of-interest; sleep 30"
        health = { log = "Serving at", interval = "50ms" }
        ready_timeout = "1s"
        restart = "never"
        "#,
        &["quiet"],
    );
    tokio::time::sleep(Duration::from_millis(1600)).await;
    assert!(
        matches!(h.status("quiet"), Status::Unhealthy),
        "a probe that gave up must read unhealthy, not healthy and not starting; was {:?}",
        h.status("quiet")
    );
    h.shutdown().await;
}

#[tokio::test]
async fn one_edit_restarts_each_node_exactly_once() {
    // The regression that produced two restarts per edit: the service matched the
    // same globs as the task it depends on, so it restarted on its own account
    // and again through the cascade.
    let mut h = Harness::start(
        "coalesce",
        r#"
        [tasks.check]
        cmd = "true"
        inputs = ["src/**"]

        [services.api]
        cmd = "sleep 30"
        depends_on = ["check"]
        watch = ["src/**"]
        debounce = "60ms"
        "#,
        &["api"],
    )
    .seed("src")
    .with_watcher();

    h.wait_for("api healthy", |st| {
        matches!(st.nodes["api"].status, Status::Healthy)
    })
    .await;
    let before = h.restarts("api");

    std::fs::write(h.path("src/a.rs"), "// one").unwrap();
    h.wait_for("the restart", |st| st.nodes["api"].restarts > before)
        .await;

    // Settle, then confirm the cascade did not fire a second time.
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(
        h.restarts("api"),
        before + 1,
        "one edit must produce exactly one restart"
    );
    h.shutdown().await;
}

/// A test task declared by crate re-runs when a crate it depends on through a
/// path changes, two levels down — the closure cargo reports, not a glob
/// anyone wrote (cairn 0074).
#[tokio::test]
async fn a_test_task_reruns_when_a_transitive_path_dependency_changes() {
    let mut h = Harness::start_in(
        "transitive",
        r#"
        [tasks.test]
        cargo = "app"
        cmd = "echo . >> runs"
        "#,
        &["test"],
        |root| {
            let w = |p: &str, text: &str| {
                let p = root.join(p);
                std::fs::create_dir_all(p.parent().unwrap()).unwrap();
                std::fs::write(p, text).unwrap();
            };
            w("Cargo.toml", "[workspace]\nmembers = [\"crates/*\"]\nresolver = \"2\"\n");
            for (name, dep) in [("app", Some("mid")), ("mid", Some("base")), ("base", None)] {
                let deps = dep
                    .map(|d| format!("{d} = {{ path = \"../{d}\" }}\n"))
                    .unwrap_or_default();
                w(
                    &format!("crates/{name}/Cargo.toml"),
                    &format!(
                        "[package]\nname = \"{name}\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\n[dependencies]\n{deps}"
                    ),
                );
                w(&format!("crates/{name}/src/lib.rs"), "");
            }
        },
    )
    .with_watcher();

    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while runs_in(&h) < 1 && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert_eq!(runs_in(&h), 1, "ran once to begin with");

    // Two path dependencies away from the crate the task names.
    std::fs::write(h.path("crates/base/src/lib.rs"), "pub fn changed() {}\n").unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while runs_in(&h) < 2 && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert_eq!(
        runs_in(&h),
        2,
        "an edit to `base` re-ran the test task of `app`"
    );

    // And an edit outside the closure does not.
    std::fs::create_dir_all(h.path("unrelated")).unwrap();
    std::fs::write(h.path("unrelated/notes.txt"), "x").unwrap();
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert_eq!(runs_in(&h), 2, "an edit outside the closure did not");
    h.shutdown().await;
}

/// `why` and anything else outside the `up` ask it what happened over the
/// control socket (cairn 0110).
#[tokio::test]
async fn the_history_can_be_asked_for_over_the_control_socket() {
    let mut h = Harness::start(
        "history",
        r#"
        [services.api]
        cmd = "sleep 30"
        "#,
        &["api"],
    );
    let cache_dir = h.dir.join(".turborust");
    tokio::spawn(turborust::control::serve(
        h.engine.clone(),
        turborust::control::socket_path(&cache_dir),
    ));
    h.wait_for("api healthy", |st| {
        matches!(st.nodes["api"].status, Status::Healthy)
    })
    .await;
    let mut entries = None;
    for _ in 0..100 {
        entries = turborust::control::history(&cache_dir, Some("api"), 10)
            .await
            .unwrap();
        if entries.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let entries = entries.expect("the socket answered");
    let what: Vec<&str> = entries.iter().map(|e| e.what.as_str()).collect();
    assert_eq!(what.first(), Some(&"starting"), "{what:?}");
    assert_eq!(what.last(), Some(&"healthy"), "{what:?}");
    assert!(entries.iter().all(|e| e.node == "api"));
    h.shutdown().await;
    assert_eq!(
        turborust::control::history(&h.dir.join("nowhere"), None, 10)
            .await
            .unwrap(),
        None,
        "no server is an answer, not an error"
    );
}

#[tokio::test]
async fn crash_backoff_doubles_and_is_capped() {
    let mut h = Harness::start(
        "backoff",
        r#"
        [services.flaky]
        cmd = "exit 1"
        restart = "always"
        backoff_min = "100ms"
        backoff_max = "400ms"
        "#,
        &["flaky"],
    );

    // Sample every change in the announced delay over a fixed window. Waiting for
    // a fourth *distinct* value would never finish — once capped it stops
    // changing, which is the property under test.
    let mut seen: Vec<Duration> = Vec::new();
    let until = tokio::time::Instant::now() + Duration::from_secs(4);
    while tokio::time::Instant::now() < until {
        if let Status::Backoff(d) = h.status("flaky")
            && seen.last() != Some(&d)
        {
            seen.push(d);
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    h.shutdown().await;

    assert!(seen.len() >= 3, "expected several backoffs, saw {seen:?}");
    assert_eq!(
        seen[0],
        Duration::from_millis(100),
        "first delay is backoff_min"
    );
    assert_eq!(seen[1], Duration::from_millis(200), "delay must double");
    assert_eq!(seen[2], Duration::from_millis(400), "and again");
    assert!(
        seen.iter().all(|d| *d <= Duration::from_millis(400)),
        "backoff_max must cap the delay: {seen:?}"
    );
}

#[tokio::test]
async fn shutdown_stops_nodes_in_reverse_dependency_order() {
    let log = std::env::temp_dir().join(format!("turborust-order-{}.log", std::process::id()));
    let _ = std::fs::remove_file(&log);
    let l = log.display();

    // Each service records its own stop. Order of the records is the assertion.
    //
    // The readiness probe is load-bearing: without one, "healthy" means merely
    // "spawned", and a stop could arrive before the shell had run its `trap`.
    // The service is only ready once it has said so, which is after the trap is
    // installed.
    let mut h = Harness::start(
        "order",
        &format!(
            r#"
        [services.db]
        cmd = "trap 'echo db >> {l}; exit 0' TERM; echo armed; while true; do sleep 0.1; done"
        health = {{ log = "armed", interval = "40ms" }}

        [services.api]
        cmd = "trap 'echo api >> {l}; exit 0' TERM; echo armed; while true; do sleep 0.1; done"
        depends_on = ["db"]
        health = {{ log = "armed", interval = "40ms" }}
        "#
        ),
        &["api"],
    );

    h.wait_for("both healthy", |st| {
        matches!(st.nodes["db"].status, Status::Healthy)
            && matches!(st.nodes["api"].status, Status::Healthy)
    })
    .await;

    h.shutdown().await;

    // Each service records itself from a TERM trap, so the write happens in the
    // child a moment after the supervisor observes the exit. Wait for both
    // records rather than racing them — the property under test is their order,
    // not how promptly a shell flushes under parallel test load.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let mut recorded = String::new();
    while std::time::Instant::now() < deadline {
        recorded = std::fs::read_to_string(&log).unwrap_or_default();
        if recorded.split_whitespace().count() >= 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let _ = std::fs::remove_file(&log);

    let order: Vec<&str> = recorded.split_whitespace().collect();
    assert_eq!(
        order,
        vec!["api", "db"],
        "a dependent must stop before the thing it depends on; \
         api={:?} db={:?}",
        h.status("api"),
        h.status("db")
    );
}

#[tokio::test]
async fn nothing_is_spawned_once_shutdown_has_begun() {
    // Cover for 0016: a restart queued just before a stop used to be drained
    // first, so a supervisor would start a process on its way out.
    let mut h = Harness::start(
        "no-spawn",
        r#"
        [services.api]
        cmd = "sleep 30"
        watch = ["src/**"]
        "#,
        &["api"],
    );
    h.wait_for("api healthy", |st| {
        matches!(st.nodes["api"].status, Status::Healthy)
    })
    .await;

    let wire = h.wires["api"].ctl_tx.clone();
    wire.send(turborust::ctl::Ctl::Restart("racing.rs".into()));
    h.shutdown().await;

    assert!(
        matches!(h.status("api"), Status::Stopped),
        "node should be stopped, was {:?}",
        h.status("api")
    );
    assert!(
        !wire.send(turborust::ctl::Ctl::Restart("later.rs".into())),
        "restarts must be refused once stopping"
    );
}

// --------------------------------------------------------- change policy ----
// Cover for cairn 0020.

#[tokio::test]
async fn on_change_ignore_leaves_a_service_alone() {
    let mut h = Harness::start(
        "ignore",
        r#"
        [services.api]
        cmd = "sleep 30"
        watch = ["src/**"]
        on_change = "ignore"
        debounce = "60ms"
        "#,
        &["api"],
    )
    .seed("src")
    .with_watcher();

    h.wait_for("api healthy", |st| {
        matches!(st.nodes["api"].status, Status::Healthy)
    })
    .await;
    std::fs::write(h.path("src/a.rs"), "// edit").unwrap();
    tokio::time::sleep(Duration::from_millis(1200)).await;

    assert_eq!(h.restarts("api"), 0, "`ignore` must not restart");
    assert!(matches!(h.status("api"), Status::Healthy));
    h.shutdown().await;
}

#[tokio::test]
async fn on_change_signal_notifies_without_restarting() {
    let sig = std::env::temp_dir().join(format!("turborust-sig-{}.log", std::process::id()));
    let _ = std::fs::remove_file(&sig);
    let s = sig.display();

    let mut h = Harness::start(
        "signal",
        &format!(
            r#"
        [services.api]
        cmd = "trap 'echo reloaded >> {s}' HUP; while true; do sleep 0.1; done"
        watch = ["src/**"]
        on_change = "signal"
        signal = "SIGHUP"
        debounce = "60ms"
        "#
        ),
        &["api"],
    )
    .seed("src")
    .with_watcher();

    h.wait_for("api healthy", |st| {
        matches!(st.nodes["api"].status, Status::Healthy)
    })
    .await;
    std::fs::write(h.path("src/a.rs"), "// edit").unwrap();

    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while tokio::time::Instant::now() < deadline
        && !std::fs::read_to_string(&sig)
            .unwrap_or_default()
            .contains("reloaded")
    {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let got = std::fs::read_to_string(&sig).unwrap_or_default();
    let restarts = h.restarts("api");
    h.shutdown().await;
    let _ = std::fs::remove_file(&sig);

    assert!(
        got.contains("reloaded"),
        "the process should have been signalled"
    );
    assert_eq!(restarts, 0, "`signal` must not restart");
}

#[tokio::test]
async fn a_stop_during_a_slow_startup_is_not_ignored() {
    // Before probe_until watched the control queue, a stop issued while a node
    // was still starting waited out the whole ready_timeout — ctrl-c during a
    // cold build looked like a hang.
    let mut h = Harness::start(
        "stop-during-start",
        r#"
        [services.slow]
        cmd = "sleep 60"
        health = { log = "never-printed", interval = "50ms" }
        ready_timeout = "60s"
        "#,
        &["slow"],
    );
    h.wait_for("it to be starting", |st| {
        matches!(st.nodes["slow"].status, Status::Starting)
    })
    .await;

    let began = std::time::Instant::now();
    h.shutdown().await;
    let took = began.elapsed();

    assert!(
        took < Duration::from_secs(10),
        "shutdown waited for the readiness timeout instead of the stop: {took:?}"
    );
    assert!(matches!(h.status("slow"), Status::Stopped));
}

// ----------------------------------------------------------- concurrency ----
// Cover for cairn 0043.

/// Four services, each slow to announce itself.
fn four_slow_services(concurrency: usize) -> String {
    let mut cfg = format!("[project]\nconcurrency = {concurrency}\n");
    for name in ["a", "b", "c", "d"] {
        cfg.push_str(&format!(
            "\n[services.{name}]\ncmd = \"sleep 0.6; echo up; sleep 30\"\n\
             health = {{ log = \"up\", interval = \"40ms\" }}\nready_timeout = \"20s\"\n"
        ));
    }
    cfg
}

#[tokio::test]
async fn more_services_than_slots_still_all_start() {
    // The deadlock this guards against: if a running service kept its permit,
    // the first `concurrency` services would hold every slot forever and the
    // rest would never start.
    let mut h = Harness::start(
        "slots-deadlock",
        &four_slow_services(1),
        &["a", "b", "c", "d"],
    );
    h.wait_for("all four healthy", |st| {
        ["a", "b", "c", "d"]
            .iter()
            .all(|n| matches!(st.nodes[*n].status, Status::Healthy))
    })
    .await;
    h.shutdown().await;
}

#[tokio::test]
async fn the_concurrency_limit_is_respected() {
    let mut h = Harness::start("slots-cap", &four_slow_services(2), &["a", "b", "c", "d"]);

    // Sample how many are building at once. Starting means "spawned, not yet
    // ready", which is exactly the window a permit covers.
    let mut peak = 0usize;
    let mut saw_queued = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        {
            let st = h.engine.state.lock().unwrap();
            let building = ["a", "b", "c", "d"]
                .iter()
                .filter(|n| matches!(st.nodes[**n].status, Status::Starting))
                .count();
            peak = peak.max(building);
            saw_queued |= ["a", "b", "c", "d"]
                .iter()
                .any(|n| matches!(st.nodes[*n].status, Status::Queued));
            if ["a", "b", "c", "d"]
                .iter()
                .all(|n| matches!(st.nodes[*n].status, Status::Healthy))
            {
                break;
            }
        }
        if tokio::time::Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    h.shutdown().await;

    assert!(peak > 0, "nothing was observed building");
    assert!(
        peak <= 2,
        "concurrency = 2 but {peak} nodes were building at once"
    );
    assert!(
        saw_queued,
        "a node held back by the limit should report `waiting: slot`"
    );
}

#[tokio::test]
async fn shutdown_is_prompt_when_a_node_is_blocked_on_a_dead_dependency() {
    // `api` waits on a dependency that will never be ready. Before the dependency
    // wait watched the control queue, that node never saw Stop and shutdown
    // burned its whole stop timeout — two such nodes cost twenty seconds.
    let mut h = Harness::start(
        "blocked-shutdown",
        r#"
        [tasks.broken]
        cmd = "exit 1"

        [services.api]
        cmd = "sleep 30"
        depends_on = ["broken"]
        "#,
        &["api"],
    );

    h.wait_for("the dependency to fail", |st| {
        matches!(st.nodes["broken"].status, Status::Failed(_))
    })
    .await;
    assert!(
        matches!(h.status("api"), Status::Waiting(_) | Status::Pending),
        "api should be parked waiting"
    );

    let began = std::time::Instant::now();
    h.shutdown().await;
    assert!(
        began.elapsed() < Duration::from_secs(5),
        "shutdown waited out a stop timeout instead of being heard: {:?}",
        began.elapsed()
    );
}

// ------------------------------------------------------------ reload = self --
// Cover for cairn 0073. `web` stands in for `dx serve`: it announces itself,
// then rebuilds whenever `go` appears — successfully, or with a rustc error
// when `broken` exists — and never exits. Nothing here restarts it; the point
// is that it is still supervised.

const SELF_RELOADER: &str = r#"
        [services.web]
        cmd = '''
        echo 'server running'
        while true; do
          if [ -f go ]; then
            rm -f go
            echo 'rebuilding'
            sleep 0.2
            if [ -f broken ]; then
              echo 'error[E0425]: cannot find value `greetng` in this scope'
              echo ' --> src/main.rs:1:1'
            else
              echo 'server running'
            fi
          fi
          sleep 0.03
        done
        '''
        reload = "self"
        health = { log = "server running", interval = "40ms" }
        reloading = { log = "rebuilding" }
        restart = "never"
"#;

fn self_reloader(extra: &str) -> String {
    format!("{SELF_RELOADER}\n{extra}")
}

impl Harness {
    fn pid(&self, node: &str) -> Option<u32> {
        self.engine.state.lock().unwrap().nodes[node].pid
    }

    fn reason(&self, node: &str) -> String {
        self.engine.state.lock().unwrap().nodes[node]
            .reason
            .clone()
            .unwrap_or_default()
    }

    fn trigger(&self) {
        std::fs::write(self.path("go"), "").unwrap();
    }

    fn lines_in(&self, file: &str) -> usize {
        std::fs::read_to_string(self.path(file))
            .map(|s| s.lines().count())
            .unwrap_or(0)
    }
}

#[tokio::test]
async fn a_self_reload_leaves_ready_and_returns_without_a_restart() {
    let mut h = Harness::start(
        "self-reload",
        &self_reloader(
            r#"
        [tasks.e2e]
        depends_on = ["web"]
        cmd = "echo ran >> e2e-runs"
        cache = "disabled"
        "#,
        ),
        &["e2e"],
    );
    h.wait_for("web healthy", |st| {
        matches!(st.nodes["web"].status, Status::Healthy)
    })
    .await;
    h.wait_for("e2e done", |st| {
        matches!(st.nodes["e2e"].status, Status::Done { .. })
    })
    .await;
    let pid = h.pid("web");
    assert_eq!(h.lines_in("e2e-runs"), 1);

    h.trigger();
    h.wait_for("web reloading", |st| {
        matches!(st.nodes["web"].status, Status::Reloading)
    })
    .await;
    h.wait_for("web healthy again", |st| {
        matches!(st.nodes["web"].status, Status::Healthy)
    })
    .await;
    // The cascade: a dependent is told about a rebuild it did not see happen.
    h.wait_for("e2e to run again", |_| h.lines_in("e2e-runs") == 2)
        .await;

    assert_eq!(
        h.restarts("web"),
        0,
        "a self-reloading node is never restarted"
    );
    assert_eq!(h.pid("web"), pid, "and keeps the same process");
    assert_eq!(h.reason("web"), "reloaded itself");
    h.shutdown().await;
}

#[tokio::test]
async fn a_watched_change_does_not_restart_a_self_reloading_node() {
    let mut h = Harness::start(
        "self-reload-watch",
        &SELF_RELOADER.replace(
            "reload = \"self\"",
            "reload = \"self\"\nwatch = [\"src/**\"]\ndebounce = \"60ms\"",
        ),
        &["web"],
    )
    .seed("src")
    .with_watcher();
    h.wait_for("web healthy", |st| {
        matches!(st.nodes["web"].status, Status::Healthy)
    })
    .await;
    let pid = h.pid("web");

    std::fs::write(h.path("src/main.rs"), "// edit").unwrap();
    tokio::time::sleep(Duration::from_millis(1000)).await;

    assert_eq!(h.restarts("web"), 0);
    assert_eq!(h.pid("web"), pid);
    assert!(matches!(h.status("web"), Status::Healthy));
    h.shutdown().await;
}

#[tokio::test]
async fn a_rebuild_that_fails_is_unhealthy_at_once_and_its_errors_are_issues() {
    let mut h = Harness::start(
        "self-reload-broken",
        &self_reloader(r#"ready_timeout = "30s""#),
        &["web"],
    );
    h.wait_for("web healthy", |st| {
        matches!(st.nodes["web"].status, Status::Healthy)
    })
    .await;

    std::fs::write(h.path("broken"), "").unwrap();
    let began = std::time::Instant::now();
    h.trigger();
    h.wait_for("web unhealthy", |st| {
        matches!(st.nodes["web"].status, Status::Unhealthy)
    })
    .await;
    assert!(
        began.elapsed() < Duration::from_secs(5),
        "rustc's error should decide it, not the 30s timeout: {:?}",
        began.elapsed()
    );
    assert_eq!(h.reason("web"), "rebuild failed with 1 error");

    // What the overlay's Issues panel renders.
    let snap = h.engine.state.lock().unwrap().snapshot();
    assert_eq!(snap.status, "error");
    assert!(
        snap.issues
            .iter()
            .any(|i| i.node == "web" && i.diagnostic.code.as_deref() == Some("E0425")),
        "the rebuild's error should be an issue"
    );

    // Fixed, and the process rebuilds by itself: healthy, still no restart.
    std::fs::remove_file(h.path("broken")).unwrap();
    h.trigger();
    h.wait_for("web healthy after the fix", |st| {
        matches!(st.nodes["web"].status, Status::Healthy)
    })
    .await;
    assert_eq!(h.restarts("web"), 0);
    let snap = h.engine.state.lock().unwrap().snapshot();
    assert!(snap.issues.is_empty(), "a fixed error must not linger");
    h.shutdown().await;
}

#[tokio::test]
async fn a_rebuild_that_never_finishes_is_reported_as_a_failed_reload() {
    // Announces a rebuild, then says nothing — no error to read, so only the
    // timeout can decide.
    let mut h = Harness::start(
        "self-reload-hang",
        r#"
        [services.web]
        cmd = "echo 'server running'; while [ ! -f go ]; do sleep 0.03; done; echo rebuilding; sleep 30"
        reload = "self"
        health = { log = "server running", interval = "40ms" }
        reloading = { log = "rebuilding" }
        ready_timeout = "600ms"
        restart = "never"
        "#,
        &["web"],
    );
    h.wait_for("web healthy", |st| {
        matches!(st.nodes["web"].status, Status::Healthy)
    })
    .await;
    h.trigger();
    h.wait_for("web unhealthy", |st| {
        matches!(st.nodes["web"].status, Status::Unhealthy)
    })
    .await;
    assert!(
        h.reason("web").starts_with("rebuild did not finish within"),
        "{}",
        h.reason("web")
    );
    h.shutdown().await;
}

#[tokio::test]
async fn a_rebuild_too_quick_to_observe_still_reaches_dependents() {
    // Both lines land between two polls of a deliberately slow probe. A flag
    // would see "ready" and nothing else; the counters see that a reload
    // happened, so the dependent still re-runs.
    let mut h = Harness::start(
        "self-reload-fast",
        r#"
        [services.web]
        cmd = "echo 'server running'; while true; do if [ -f go ]; then rm -f go; echo rebuilding; echo 'server running'; fi; sleep 0.02; done"
        reload = "self"
        health = { log = "server running", interval = "700ms" }
        reloading = { log = "rebuilding" }
        restart = "never"

        [tasks.e2e]
        depends_on = ["web"]
        cmd = "echo ran >> e2e-runs"
        cache = "disabled"
        "#,
        &["e2e"],
    );
    h.wait_for("the first e2e run", |_| h.lines_in("e2e-runs") == 1)
        .await;
    h.trigger();
    h.wait_for("e2e to run again", |_| h.lines_in("e2e-runs") == 2)
        .await;
    assert_eq!(h.restarts("web"), 0);
    h.shutdown().await;
}

#[tokio::test]
async fn a_first_build_slower_than_the_timeout_comes_good_by_itself() {
    let mut h = Harness::start(
        "self-reload-slow-start",
        r#"
        [services.web]
        cmd = "sleep 1.2; echo 'server running'; sleep 30"
        reload = "self"
        health = { log = "server running", interval = "40ms" }
        reloading = { log = "rebuilding" }
        ready_timeout = "300ms"
        restart = "never"
        "#,
        &["web"],
    );
    h.wait_for("web unhealthy after the timeout", |st| {
        matches!(st.nodes["web"].status, Status::Unhealthy)
    })
    .await;
    h.wait_for("web healthy once the build lands", |st| {
        matches!(st.nodes["web"].status, Status::Healthy)
    })
    .await;
    assert_eq!(h.restarts("web"), 0);
    h.shutdown().await;
}

#[tokio::test]
async fn a_rebuild_announced_while_startup_is_being_confirmed_is_not_missed() {
    // The ready line and a rebuild arrive together; the probe only looks every
    // 300ms, so by the time startup is confirmed the rebuild is already under
    // way. It must read as reloading, not healthy for the whole rebuild.
    let mut h = Harness::start(
        "self-reload-window",
        r#"
        [services.web]
        cmd = "echo 'server running'; echo rebuilding; sleep 1.5; echo 'server running'; sleep 30"
        reload = "self"
        health = { log = "server running", interval = "300ms" }
        reloading = { log = "rebuilding" }
        restart = "never"
        "#,
        &["web"],
    );
    h.wait_for("web reloading", |st| {
        matches!(st.nodes["web"].status, Status::Reloading)
    })
    .await;
    h.wait_for("web healthy", |st| {
        matches!(st.nodes["web"].status, Status::Healthy)
    })
    .await;
    h.shutdown().await;
}

// ------------------------------------------------------------- health.cmd --
// Cover for cairn 0091: readiness by command, for processes whose port opens
// before they can do anything (Postgres answering "the database system is
// starting up").

impl Harness {
    fn logs_of(&self, node: &str) -> String {
        self.engine
            .state
            .lock()
            .unwrap()
            .logs
            .view(Some(node))
            .iter()
            .map(|l| turborust::proc::strip_ansi(&l.text))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[tokio::test]
async fn a_command_probe_holds_readiness_after_the_port_opens() {
    // The port answers from the start, as Postgres's does; only the command
    // knows the process is not ready yet.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let mut h = Harness::start(
        "probe-cmd",
        &format!(
            r#"
            [services.db]
            cmd = "sleep 1.2; touch accepting; sleep 30"
            health = {{ tcp = {port}, cmd = "test -f accepting", interval = "50ms" }}
            restart = "never"
            "#
        ),
        &["db"],
    );
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert!(
        matches!(h.status("db"), Status::Starting),
        "the port was open but the command had not passed; was {:?}",
        h.status("db")
    );
    h.wait_for("db healthy", |st| {
        matches!(st.nodes["db"].status, Status::Healthy)
    })
    .await;
    drop(listener);
    h.shutdown().await;
}

#[tokio::test]
async fn a_command_probe_runs_with_the_nodes_environment() {
    let mut h = Harness::start(
        "probe-env",
        r#"
        [services.with]
        cmd = "sleep 30"
        env = { PROBE_ME = "yes" }
        health = { cmd = "test \"$PROBE_ME\" = yes", interval = "50ms" }
        restart = "never"

        [services.without]
        cmd = "sleep 30"
        health = { cmd = "test \"$PROBE_ME\" = yes", interval = "50ms" }
        ready_timeout = "800ms"
        restart = "never"
        "#,
        &["with", "without"],
    );
    h.wait_for("with healthy, without unhealthy", |st| {
        matches!(st.nodes["with"].status, Status::Healthy)
            && matches!(st.nodes["without"].status, Status::Unhealthy)
    })
    .await;
    h.shutdown().await;
}

#[tokio::test]
async fn a_probe_that_hangs_is_cut_off_and_reported() {
    let mut h = Harness::start(
        "probe-hang",
        r#"
        [services.db]
        cmd = "sleep 30"
        health = { cmd = "sleep 60", timeout = "200ms", interval = "50ms" }
        ready_timeout = "1s"
        restart = "never"
        "#,
        &["db"],
    );
    let began = std::time::Instant::now();
    h.wait_for("db unhealthy", |st| {
        matches!(st.nodes["db"].status, Status::Unhealthy)
    })
    .await;
    assert!(
        began.elapsed() < Duration::from_secs(5),
        "a hanging probe held up the supervisor: {:?}",
        began.elapsed()
    );
    assert!(
        h.logs_of("db").contains("did not finish within"),
        "{}",
        h.logs_of("db")
    );
    h.shutdown().await;
}

#[tokio::test]
async fn a_failing_probe_says_what_it_said() {
    let mut h = Harness::start(
        "probe-say",
        r#"
        [services.db]
        cmd = "sleep 30"
        health = { cmd = "echo 'no response' >&2; exit 2", interval = "50ms" }
        ready_timeout = "500ms"
        restart = "never"
        "#,
        &["db"],
    );
    h.wait_for("db unhealthy", |st| {
        matches!(st.nodes["db"].status, Status::Unhealthy)
    })
    .await;
    let logs = h.logs_of("db");
    assert!(
        logs.contains("last readiness attempt:") && logs.contains("(exit 2): no response"),
        "{logs}"
    );
    h.shutdown().await;
}

// --------------------------------------------------------------- liveness --
// Cover for cairn 0092: readiness was checked until it first passed and never
// again, so a server that wedged read healthy forever.

#[tokio::test]
async fn a_service_that_stops_answering_goes_down_and_comes_back() {
    let mut h = Harness::start(
        "liveness",
        r#"
        [services.api]
        cmd = "touch alive; sleep 30"
        # Without a readiness probe it is healthy the moment it spawns, before
        # the shell has created the file the test then removes.
        health = { cmd = "test -f alive", interval = "20ms" }
        liveness = { cmd = "test -f alive", every = "100ms", failures = 3 }
        restart = "never"

        [services.web]
        depends_on = ["api"]
        cmd = "sleep 30"
        restart = "never"
        "#,
        &["web"],
    );
    h.wait_for("both healthy", |st| {
        matches!(st.nodes["api"].status, Status::Healthy)
            && matches!(st.nodes["web"].status, Status::Healthy)
    })
    .await;
    let pid = h.pid("api");

    std::fs::remove_file(h.path("alive")).unwrap();
    h.wait_for("api unhealthy, and web told", |st| {
        matches!(st.nodes["api"].status, Status::Unhealthy)
            && matches!(st.nodes["web"].status, Status::Waiting(_))
    })
    .await;
    assert!(
        h.reason("api")
            .contains("not answering (3 checks in a row)"),
        "{}",
        h.reason("api")
    );
    assert_eq!(
        h.pid("api"),
        pid,
        "without restart = always it is not restarted"
    );

    std::fs::write(h.path("alive"), "").unwrap();
    h.wait_for("both healthy again", |st| {
        matches!(st.nodes["api"].status, Status::Healthy)
            && matches!(st.nodes["web"].status, Status::Healthy)
    })
    .await;
    h.shutdown().await;
}

#[tokio::test]
async fn with_restart_always_a_service_that_stops_answering_is_restarted() {
    let mut h = Harness::start(
        "liveness-restart",
        r#"
        [services.api]
        cmd = "sleep 30"
        liveness = { cmd = "test -f alive", every = "100ms", failures = 2 }
        restart = "always"
        "#,
        &["api"],
    );
    std::fs::write(h.path("alive"), "").unwrap();
    h.wait_for("api healthy", |st| {
        matches!(st.nodes["api"].status, Status::Healthy)
    })
    .await;
    let pid = h.pid("api");
    std::fs::remove_file(h.path("alive")).unwrap();
    h.wait_for("a restart", |st| st.nodes["api"].restarts >= 1)
        .await;
    std::fs::write(h.path("alive"), "").unwrap();
    h.wait_for("api healthy as a new process", |st| {
        matches!(st.nodes["api"].status, Status::Healthy) && st.nodes["api"].pid != pid
    })
    .await;
    h.shutdown().await;
}

#[tokio::test]
async fn one_missed_check_is_not_an_outage() {
    // Fails exactly once, then answers: below the threshold, nothing happens.
    let mut h = Harness::start(
        "liveness-blip",
        r#"
        [services.api]
        cmd = "sleep 30"
        liveness = { cmd = "if [ -f blipped ]; then exit 0; fi; touch blipped; exit 1", every = "80ms", failures = 2 }
        restart = "always"
        "#,
        &["api"],
    );
    h.wait_for("api healthy", |st| {
        matches!(st.nodes["api"].status, Status::Healthy)
    })
    .await;
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert!(h.path("blipped").exists(), "the probe never ran");
    assert_eq!(
        h.restarts("api"),
        0,
        "one miss below the threshold restarted it"
    );
    assert!(matches!(h.status("api"), Status::Healthy));
    h.shutdown().await;
}

#[tokio::test]
async fn liveness_still_runs_beside_a_self_reload_poll() {
    // The self-reload poll fires every 40ms. Liveness was nearly written as a
    // sleep inside the same select!, which that poll would reset every time:
    // the check would never come round.
    let mut h = Harness::start(
        "liveness-beside-reload",
        r#"
        [services.web]
        cmd = "touch alive; echo 'server running'; sleep 30"
        reload = "self"
        health = { log = "server running", interval = "40ms" }
        reloading = { log = "rebuilding" }
        liveness = { cmd = "test -f alive", every = "150ms", failures = 2 }
        restart = "never"
        "#,
        &["web"],
    );
    h.wait_for("web healthy", |st| {
        matches!(st.nodes["web"].status, Status::Healthy)
    })
    .await;
    std::fs::remove_file(h.path("alive")).unwrap();
    h.wait_for("web unhealthy", |st| {
        matches!(st.nodes["web"].status, Status::Unhealthy)
    })
    .await;
    h.shutdown().await;
}

#[tokio::test]
async fn a_nested_workspace_is_not_restarted_by_being_watched() {
    // Cover for cairn 0088: on Linux, watching a workspace laid out like a real
    // one — crates/<name>/src/*.rs — restarted its services about eight times a
    // second with nothing edited, because reading a directory was reported as
    // changing it. Every other test here watches a flat `src/`, which never
    // took that path.
    let h = Harness::start(
        "nested",
        r#"
        [services.api]
        cmd = "sleep 30"
        watch = ["crates/**/*.rs"]
        debounce = "60ms"
        "#,
        &["api"],
    );
    // The tree exists before the watch is armed, as a real workspace does.
    for d in ["crates/api/src", "crates/shared/src"] {
        std::fs::create_dir_all(h.path(d)).unwrap();
    }
    std::fs::write(h.path("crates/api/src/main.rs"), "fn main() {}").unwrap();
    std::fs::write(h.path("crates/shared/src/lib.rs"), "").unwrap();
    let mut h = h.with_watcher();

    h.wait_for("api healthy", |st| {
        matches!(st.nodes["api"].status, Status::Healthy)
    })
    .await;
    // FSEvents delivers events from just before a stream starts, so on macOS
    // the fixture's own writes can arrive once after arming. That is not the
    // bug; the bug never settles. So: wait for quiet, then count from there.
    let mut baseline = h.restarts("api");
    let mut quiet = false;
    for _ in 0..8 {
        tokio::time::sleep(Duration::from_millis(1000)).await;
        let now = h.restarts("api");
        if now == baseline {
            quiet = true;
            break;
        }
        baseline = now;
    }
    assert!(
        quiet,
        "the watch never went quiet: {baseline} restarts and counting"
    );
    assert!(baseline <= 1, "nothing was edited, yet {baseline} restarts");

    std::fs::write(h.path("crates/shared/src/lib.rs"), "// one").unwrap();
    h.wait_for("the restart", |st| st.nodes["api"].restarts > baseline)
        .await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(
        h.restarts("api"),
        baseline + 1,
        "one edit must produce exactly one restart"
    );
    h.shutdown().await;
}

#[tokio::test]
async fn a_node_without_liveness_is_never_probed_once_ready() {
    // Readiness is a startup check; only a declared `liveness` keeps asking.
    // Every probe run appends a line, so the count says how often it ran.
    let mut h = Harness::start(
        "no-liveness",
        r#"
        [services.api]
        cmd = "sleep 30"
        health = { cmd = "echo . >> probes", interval = "50ms" }
        restart = "never"
        "#,
        &["api"],
    );
    h.wait_for("api healthy", |st| {
        matches!(st.nodes["api"].status, Status::Healthy)
    })
    .await;
    let runs = || {
        std::fs::read_to_string(h.path("probes"))
            .unwrap()
            .lines()
            .count()
    };
    let at_ready = runs();
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert_eq!(
        runs(),
        at_ready,
        "a probe ran after readiness with no liveness declared"
    );
    h.shutdown().await;
}

#[tokio::test]
async fn with_polling_an_edit_restarts_its_service_once() {
    // Cover for cairn 0094: the polling backend feeds the same dispatch as the
    // native one, so a change found by scanning restarts exactly what an event
    // would have.
    let mut h = Harness::start(
        "poll",
        r#"
        [services.api]
        cmd = "sleep 30"
        watch = ["src/**"]
        debounce = "60ms"
        "#,
        &["api"],
    )
    .seed("src")
    .with_backend(turborust::watch::Backend::Poll(Duration::from_millis(100)));

    h.wait_for("api healthy", |st| {
        matches!(st.nodes["api"].status, Status::Healthy)
    })
    .await;
    let before = h.restarts("api");
    std::fs::write(h.path("src/a.rs"), "// one").unwrap();
    h.wait_for("the restart", |st| st.nodes["api"].restarts > before)
        .await;
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert_eq!(h.restarts("api"), before + 1, "one edit, one restart");
    h.shutdown().await;
}

fn runs_in(h: &Harness) -> usize {
    std::fs::read_to_string(h.path("runs"))
        .map(|s| s.lines().count())
        .unwrap_or(0)
}

#[tokio::test]
async fn a_crash_loop_gives_up_after_max_restarts_and_an_edit_brings_it_back() {
    // Cover for cairn 0096: a command that can never work used to be restarted,
    // and reported failing, forever.
    let mut h = Harness::start(
        "crashloop",
        r#"
        [services.api]
        cmd = "echo . >> runs; exit 3"
        restart = "always"
        max_restarts = 2
        backoff_min = "10ms"
        watch = ["src/**"]
        debounce = "60ms"
        "#,
        &["api"],
    )
    .seed("src")
    .with_watcher();

    h.wait_for("api given up", |st| {
        st.nodes["api"]
            .reason
            .as_deref()
            .is_some_and(|r| r.contains("gave up after 2 restart(s)"))
    })
    .await;
    assert!(matches!(h.status("api"), Status::Failed(3)));
    assert_eq!(runs_in(&h), 3, "the first run and two restarts");
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert_eq!(runs_in(&h), 3, "restarted after giving up");

    // A change is a new attempt with a fresh count, not one more crash.
    std::fs::write(h.path("src/a.rs"), "// fixed?").unwrap();
    h.wait_for("three more runs", |_| runs_in(&h) >= 6).await;
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert_eq!(runs_in(&h), 6, "the edit's attempt has the same allowance");
    assert!(h.reason("api").contains("gave up"), "{}", h.reason("api"));
    h.shutdown().await;
}

#[tokio::test]
async fn a_run_that_lasted_is_not_part_of_a_crash_loop() {
    // The third run stays up longer than backoff_max, so the crash that ends
    // it starts the count again: two more restarts before giving up. The
    // margins are wide both ways: a quick crash is a shell starting, which on
    // a loaded CI runner took over 200ms and was counted as having lasted.
    let mut h = Harness::start(
        "crash-settled",
        r#"
        [services.api]
        cmd = "n=$(cat runs 2>/dev/null | wc -l); echo . >> runs; if [ $n -eq 2 ]; then sleep 2.5; fi; exit 3"
        restart = "always"
        max_restarts = 2
        backoff_min = "10ms"
        backoff_max = "1s"
        "#,
        &["api"],
    );
    h.wait_for("api given up", |st| {
        st.nodes["api"]
            .reason
            .as_deref()
            .is_some_and(|r| r.contains("gave up"))
    })
    .await;
    assert_eq!(runs_in(&h), 5);
    h.shutdown().await;
}

#[tokio::test]
async fn without_max_restarts_a_crash_loop_keeps_trying() {
    let mut h = Harness::start(
        "crash-forever",
        r#"
        [services.api]
        cmd = "echo . >> runs; exit 3"
        restart = "always"
        backoff_min = "10ms"
        backoff_max = "20ms"
        "#,
        &["api"],
    );
    h.wait_for("many runs", |_| runs_in(&h) >= 8).await;
    assert!(!h.reason("api").contains("gave up"));
    h.shutdown().await;
}

#[tokio::test]
async fn a_restarted_node_is_told_which_of_its_files_changed() {
    // Cover for cairn 0098. `docs/` is outside api's globs, so it must not be
    // passed on even though it changed in the same batch.
    let mut h = Harness::start(
        "changed",
        r#"
        [tasks.check]
        cmd = "echo \"[$TURBORUST_CHANGED]\" >> check-saw"
        inputs = ["src/**"]

        [services.api]
        cmd = "echo \"[$TURBORUST_CHANGED]\" >> api-saw; sleep 30"
        depends_on = ["check"]
        watch = ["src/**"]
        debounce = "60ms"
        "#,
        &["api"],
    )
    .seed("src")
    .seed("docs")
    .with_watcher();
    h.wait_for("api healthy", |st| {
        matches!(st.nodes["api"].status, Status::Healthy)
    })
    .await;
    let saw = |f: &str| std::fs::read_to_string(h.path(f)).unwrap_or_default();
    h.wait_for("api's first start", |_| saw("api-saw").lines().count() >= 1)
        .await;
    assert_eq!(
        saw("api-saw"),
        "[]\n",
        "a first start was caused by no change"
    );

    let before = h.restarts("api");
    std::fs::write(h.path("docs/notes.md"), "x").unwrap();
    std::fs::write(h.path("src/a.rs"), "// one").unwrap();
    h.wait_for("the restart", |st| st.nodes["api"].restarts > before)
        .await;
    h.wait_for("api's second start", |_| {
        saw("api-saw").lines().count() >= 2
    })
    .await;
    // The task matched too, and reran in the cascade with the same news.
    for f in ["check-saw", "api-saw"] {
        let last = saw(f).lines().last().unwrap_or_default().to_string();
        assert_eq!(last, "[src/a.rs]", "{f}: {}", saw(f));
    }
    h.shutdown().await;
}

#[tokio::test]
async fn a_dependency_rebuilt_too_fast_to_see_go_down_still_restarts_its_dependents() {
    // Cover for cairn 0101. Readiness travels on a channel that keeps only its
    // latest value, so a dependency that reruns without yielding goes false and
    // back to true before anything observes the false. A cache hit is exactly
    // that, and reverting an edit produces one: the dependent then saw only
    // "up", ignored it, and kept running against code that had changed back.
    let h = Harness::start(
        "fast-edge",
        r#"
        [tasks.check]
        cmd = "true"
        inputs = ["src/**"]

        [services.api]
        cmd = "sleep 30"
        depends_on = ["check"]
        "#,
        &["api"],
    );
    std::fs::create_dir_all(h.path("src")).unwrap();
    std::fs::write(h.path("src/a.rs"), "// v1").unwrap();
    let mut h = h.with_watcher();
    h.wait_for("api healthy", |st| {
        matches!(st.nodes["api"].status, Status::Healthy)
    })
    .await;

    std::fs::write(h.path("src/a.rs"), "// v2").unwrap();
    h.wait_for("a restart for v2", |st| st.nodes["api"].restarts >= 1)
        .await;
    h.wait_for("api healthy on v2", |st| {
        matches!(st.nodes["api"].status, Status::Healthy)
    })
    .await;

    // Back to v1: `check` is served from cache, instantly.
    std::fs::write(h.path("src/a.rs"), "// v1").unwrap();
    h.wait_for("check served from cache", |st| {
        matches!(st.nodes["check"].status, Status::Done { cached: true, .. })
    })
    .await;
    h.wait_for("a restart for the revert", |st| {
        st.nodes["api"].restarts >= 2
    })
    .await;
    h.shutdown().await;
}
