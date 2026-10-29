//! The whole engine, on a clock that only moves when nothing else can.
//!
//! Cover for cairn 0106. Supervisors, the dependency board, timeouts, backoff
//! and crash limits all run for real here; only the host is fake — no process is
//! started — and tokio's clock is paused, so it advances exactly when every task
//! is waiting. A ten-minute timeout takes microseconds, a backoff can be measured
//! to the millisecond, and nothing depends on how loaded the machine is.

use std::sync::Arc;
use std::time::Duration;
use turborust::cache::Cache;
use turborust::config::{Config, VerifyMode, Workspace};
use turborust::engine::{self, Ctl, Engine, Wire};
use turborust::host::fake::{FakeHost, Script};
use turborust::state::Status;

struct Sim {
    engine: Arc<Engine>,
    host: Arc<FakeHost>,
    wires: std::collections::BTreeMap<String, Wire>,
    supervisors: Option<engine::Supervisors>,
    _dir: Dir,
}

struct Dir(std::path::PathBuf);
impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Builds an engine over `toml` with a fake host, scripted by `script` before
/// any supervisor starts.
fn sim(tag: &str, toml: &str, targets: &[&str], script: impl FnOnce(&FakeHost)) -> Sim {
    let dir = std::env::temp_dir().join(format!("turborust-sim-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let ws = Workspace {
        root: dir.clone(),
        config: toml::from_str::<Config>(toml).expect("config parses"),
        source: Some(toml.to_string()),
    };
    let targets: Vec<String> = targets.iter().map(|s| s.to_string()).collect();
    let plan = Arc::new(turborust::plan::resolve(&ws, &targets).expect("plan resolves"));
    let cache = Arc::new(Cache::new(&ws.cache_dir()).unwrap());
    let host = FakeHost::new();
    script(&host);
    let (engine, rx) = Engine::with_host(plan, cache, VerifyMode::Off, host.clone());
    tokio::spawn(engine::pump_events(engine.state.clone(), rx, false));
    let (wires, supervisors) = engine::spawn_supervisors(engine.clone());
    Sim {
        engine,
        host,
        wires,
        supervisors: Some(supervisors),
        _dir: Dir(dir),
    }
}

impl Sim {
    fn status(&self, node: &str) -> Status {
        self.engine.state.lock().unwrap().nodes[node].status.clone()
    }

    fn restarts(&self, node: &str) -> u32 {
        self.engine.state.lock().unwrap().nodes[node].restarts
    }

    fn reason(&self, node: &str) -> String {
        self.engine.state.lock().unwrap().nodes[node]
            .reason
            .clone()
            .unwrap_or_default()
    }

    fn log(&self) -> String {
        let st = self.engine.state.lock().unwrap();
        st.logs
            .view(None)
            .iter()
            .map(|l| format!("{} | {}", l.proc, turborust::proc::strip_ansi(&l.text)))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Waits, in virtual time, for `pred`; an hour of it is a hang.
    async fn until(&self, what: &str, pred: impl Fn(&Sim) -> bool) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3600);
        while !pred(self) {
            if tokio::time::Instant::now() >= deadline {
                let st = self.engine.state.lock().unwrap();
                let now: Vec<String> = st
                    .nodes
                    .iter()
                    .map(|(n, s)| format!("{n}={}", s.status.label()))
                    .collect();
                let history = st.history.describe();
                drop(st);
                panic!(
                    "never: {what}; state was [{}]\nhistory:\n{history}log:\n{}",
                    now.join(", "),
                    self.log()
                );
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    fn send(&self, node: &str, msg: Ctl) {
        self.wires[node].ctl_tx.send(msg);
    }

    /// Stops everything, then checks that the recorded history alone rebuilds
    /// every node's lifecycle exactly as the run left it (cairn 0107). Every
    /// scenario here ends this way, so each one is also a replay test.
    async fn shutdown(&mut self) {
        let sups = self.supervisors.take().unwrap();
        engine::shutdown(&self.engine, &self.wires, sups).await;
        assert_replays(&self.engine);
    }
}

fn assert_replays(engine: &Engine) {
    let st = engine.state.lock().unwrap();
    assert!(st.history.total() > 0, "a run with no history");
    assert_eq!(
        st.history.project(),
        st.lives(),
        "the history does not rebuild the state:\n{}",
        st.history.describe()
    );
}

fn healthy(node: &'static str) -> impl Fn(&Sim) -> bool {
    move |s| s.status(node) == Status::Healthy
}

#[tokio::test(start_paused = true)]
async fn a_dependency_that_crashes_takes_its_dependents_down_and_back_once() {
    let mut s = sim(
        "cascade",
        r#"
        [services.db]
        cmd = "db"
        [services.api]
        cmd = "api"
        depends_on = ["db"]
        [services.web]
        cmd = "web"
        depends_on = ["api"]
        "#,
        &["web"],
        |_| {},
    );
    s.until("all healthy", |s| {
        ["db", "api", "web"]
            .iter()
            .all(|n| s.status(n) == Status::Healthy)
    })
    .await;
    s.host.crash("db", 1);
    s.until("db back", |s| {
        s.host.spawns("db") == 2 && s.status("db") == Status::Healthy
    })
    .await;
    s.until("all healthy again", |s| {
        ["api", "web"]
            .iter()
            .all(|n| s.status(n) == Status::Healthy)
    })
    .await;
    // Settle, then confirm: one restart each, not one per edge.
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert_eq!((s.host.spawns("api"), s.host.spawns("web")), (2, 2));
    assert_eq!((s.restarts("api"), s.restarts("web")), (1, 1));
    s.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn crash_backoff_doubles_to_its_cap_in_virtual_time() {
    let mut s = sim(
        "backoff",
        r#"
        [services.api]
        cmd = "api"
        backoff_min = "100ms"
        backoff_max = "1s"
        "#,
        &["api"],
        |h| h.always("api", Script::exits(Duration::ZERO, 3)),
    );
    s.until("seven spawns", |s| s.host.spawns("api") >= 7).await;
    let t = s.host.spawn_times("api");
    let gaps: Vec<u128> = t.windows(2).map(|w| (w[1] - w[0]).as_millis()).collect();
    // Each gap is the backoff plus the exit-poll interval (20ms) it took to
    // notice the crash, so compare against the backoff with that slack.
    for (gap, backoff) in gaps.iter().zip([100, 200, 400, 800, 1000, 1000]) {
        assert!(
            (backoff..backoff + 60).contains(gap),
            "gaps {gaps:?}: expected about {backoff}ms"
        );
    }
    s.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn a_crash_loop_gives_up_after_max_restarts_until_asked_again() {
    let mut s = sim(
        "giveup",
        r#"
        [services.api]
        cmd = "api"
        max_restarts = 2
        backoff_min = "10ms"
        "#,
        &["api"],
        |h| h.always("api", Script::exits(Duration::ZERO, 3)),
    );
    s.until("given up", |s| s.reason("api").starts_with("gave up"))
        .await;
    tokio::time::sleep(Duration::from_secs(60)).await;
    assert_eq!(s.host.spawns("api"), 3, "the first run and two restarts");
    assert_eq!(s.status("api"), Status::Failed(3));
    s.send("api", Ctl::Restart("manual restart".into()));
    s.until("three more", |s| s.host.spawns("api") == 6).await;
    s.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn a_task_over_its_timeout_fails_with_124_without_waiting_ten_minutes() {
    let dir = std::env::temp_dir().join(format!("turborust-sim-timeout-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let toml = r#"
        [tasks.hang]
        cmd = "hang"
        timeout = "10m"
    "#;
    let ws = Workspace {
        root: dir.clone(),
        config: toml::from_str::<Config>(toml).unwrap(),
        source: Some(toml.into()),
    };
    let plan = Arc::new(turborust::plan::resolve(&ws, &["hang".into()]).unwrap());
    let cache = Arc::new(Cache::new(&ws.cache_dir()).unwrap());
    let host = FakeHost::new();
    host.always("hang", Script::forever());
    let (engine, rx) = Engine::with_host(plan, cache, VerifyMode::Off, host.clone());
    tokio::spawn(engine::pump_events(engine.state.clone(), rx, false));
    let real = std::time::Instant::now();
    let virt = tokio::time::Instant::now();
    let code = engine::run_once(engine.clone()).await.unwrap();
    assert_eq!(code, engine::TIMED_OUT);
    assert!(
        virt.elapsed() >= Duration::from_secs(600),
        "{:?}",
        virt.elapsed()
    );
    assert!(
        real.elapsed() < Duration::from_secs(5),
        "{:?}",
        real.elapsed()
    );
    assert!(!host.running("hang"), "the timed-out child was stopped");
    assert_replays(&engine);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test(start_paused = true)]
async fn readiness_that_never_comes_is_unhealthy_after_ready_timeout() {
    let mut s = sim(
        "unready",
        r#"
        [services.db]
        cmd = "db"
        health = { tcp = 5432, interval = "500ms" }
        ready_timeout = "20s"
        "#,
        &["db"],
        |h| h.set_probe("db", false),
    );
    let began = tokio::time::Instant::now();
    s.until("unhealthy", |s| s.status("db") == Status::Unhealthy)
        .await;
    let took = began.elapsed();
    assert!(
        (Duration::from_secs(20)..Duration::from_secs(21)).contains(&took),
        "{took:?}"
    );
    assert!(
        s.log()
            .contains("last readiness attempt: fake probe says no"),
        "{}",
        s.log()
    );
    // Still running: unhealthy, not dead.
    assert!(s.host.running("db"));
    // And it comes good by itself if the process does.
    s.host.set_probe("db", true);
    s.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn liveness_takes_a_silent_service_down_and_back_with_its_dependent() {
    let mut s = sim(
        "liveness",
        r#"
        [services.db]
        cmd = "db"
        liveness = { tcp = 5432, every = "10s", failures = 3 }
        restart = "on-change"
        [services.api]
        cmd = "api"
        depends_on = ["db"]
        "#,
        &["api"],
        |_| {},
    );
    s.until("both healthy", |s| healthy("db")(s) && healthy("api")(s))
        .await;
    let stopped = tokio::time::Instant::now();
    s.host.set_probe("db", false);
    s.until("db unhealthy", |s| s.status("db") == Status::Unhealthy)
        .await;
    // Three checks ten seconds apart: the third miss is about 30s in.
    let took = stopped.elapsed();
    assert!(
        (Duration::from_secs(20)..Duration::from_secs(31)).contains(&took),
        "{took:?}"
    );
    s.until("api waiting", |s| {
        matches!(s.status("api"), Status::Waiting(_))
    })
    .await;
    s.host.set_probe("db", true);
    s.until("both healthy again", |s| {
        healthy("db")(s) && healthy("api")(s)
    })
    .await;
    assert_eq!(
        s.host.spawns("db"),
        1,
        "liveness without restart = always does not restart"
    );
    assert_eq!(s.host.spawns("api"), 2);
    s.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn a_port_still_held_is_named_and_the_service_starts_anyway() {
    let mut s = sim(
        "port",
        r#"
        [services.api]
        cmd = "api"
        port = 8080
        "#,
        &["api"],
        |h| {
            h.hold_port(8080, "pid 4242 (old-api)");
        },
    );
    s.until("api healthy", healthy("api")).await;
    assert!(
        s.log()
            .contains("port 8080 still held by pid 4242 (old-api); starting anyway"),
        "{}",
        s.log()
    );
    s.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn a_spawn_that_fails_is_reported_and_retried_until_shutdown_ends_it() {
    let mut s = sim(
        "spawnfail",
        r#"
        [services.api]
        cmd = "api"
        backoff_min = "1s"
        backoff_max = "1s"
        "#,
        &["api"],
        |h| h.always("api", Script::fails_to_spawn("no such file")),
    );
    s.until("retried", |s| s.host.spawns("api") >= 3).await;
    assert_eq!(s.status("api"), Status::Failed(127));
    assert!(
        s.log().contains("spawn failed: no such file"),
        "{}",
        s.log()
    );
    // A shutdown during the retry backoff does not wait it out.
    let began = tokio::time::Instant::now();
    s.shutdown().await;
    assert!(
        began.elapsed() < Duration::from_millis(100),
        "{:?}",
        began.elapsed()
    );
}

#[tokio::test(start_paused = true)]
async fn on_change_signal_signals_instead_of_restarting() {
    let mut s = sim(
        "signal",
        r#"
        [services.api]
        cmd = "api"
        on_change = "signal"
        signal = "SIGHUP"
        "#,
        &["api"],
        |_| {},
    );
    s.until("api healthy", healthy("api")).await;
    s.send("api", Ctl::Restart("config.toml".into()));
    s.until("signalled", |s| s.host.signalled("api")).await;
    assert_eq!(s.host.spawns("api"), 1);
    assert!(s.log().contains("config.toml"), "{}", s.log());
    s.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn shutdown_stops_in_reverse_dependency_order() {
    let mut s = sim(
        "order",
        r#"
        [services.db]
        cmd = "db"
        [services.api]
        cmd = "api"
        depends_on = ["db"]
        "#,
        &["api"],
        |_| {},
    );
    s.until("both healthy", |s| healthy("db")(s) && healthy("api")(s))
        .await;
    s.shutdown().await;
    assert!(!s.host.running("api") && !s.host.running("db"));
    assert_eq!(
        (s.status("db"), s.status("api")),
        (Status::Stopped, Status::Stopped)
    );
}
