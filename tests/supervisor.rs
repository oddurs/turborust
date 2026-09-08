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
        let dir = std::env::temp_dir().join(format!("turborust-sup-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let dir = dir.canonicalize().unwrap();

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
        let w = turborust::watch::watch(&self.dir, Duration::from_millis(60)).unwrap();
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
                panic!(
                    "timed out waiting for {what}; state was [{}]",
                    now.join(", ")
                );
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    async fn shutdown(&mut self) {
        if let Some(s) = self.supervisors.take() {
            engine::shutdown(&self.engine, &self.wires, s).await;
        }
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
        !matches!(h.status("quiet"), Status::Healthy),
        "a probe that never matched must not report healthy"
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
