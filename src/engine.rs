//! The supervisor.
//!
//! One tokio task per node. A node starts only once every dependency is ready,
//! *stops* when any dependency stops being ready, and restarts when a dependency
//! produces a result newer than the one it started against. Those rules give
//! ordered startup, ordered shutdown, and cascading restarts: rebuild a shared
//! crate, its task goes not-ready, every consumer stops, the task finishes,
//! everyone comes back in dependency order.
//!
//! The rules are level-triggered. Every status write goes through
//! [`Engine::set`], which derives readiness from the status, bumps the node's
//! generation on each new result, and ticks one board version. A supervisor
//! waiting on its dependencies wakes on any tick and decides from a snapshot of
//! their status and generations — so there is no stream of up/down messages to
//! lose an edge from (cairn 0101, 0104).

use crate::cache::{self, Cache, Fingerprint, Record};
use crate::config::{EnvMode, OnChange, Reload, SAFE_BASE_ENV, TargetDir};
use crate::ctl;
pub use crate::ctl::Ctl;

/// Reload kinds sent to the browser. A stylesheet swap keeps the running app —
/// its route, its form state, the panel you had open — where a full reload
/// discards all of it for the sake of a colour change.
pub const RELOAD_FULL: &str = "full";
pub const RELOAD_CSS: &str = "css";
use crate::health;
use crate::plan::{Kind, Node, Plan};
use crate::proc::{self, Handle, ProcEvent};
use crate::state::{AppState, Status};
use anyhow::Result;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::{broadcast, mpsc, watch};

#[derive(Debug, Clone)]
pub struct TaskOutcome {
    pub cached: bool,
    pub code: i32,
    pub duration: Duration,
    pub fingerprint: Option<Fingerprint>,
}

pub struct Engine {
    pub plan: Arc<Plan>,
    pub state: Arc<Mutex<AppState>>,
    pub cache: Arc<Cache>,
    pub events_tx: mpsc::UnboundedSender<ProcEvent>,
    pub reload_tx: broadcast::Sender<String>,
    /// What each completed node contributes to its dependents' cache keys.
    ///
    /// `out:<hash>` when the node declared outputs and produced some — then a
    /// dependent rebuilds because its upstream produced something *different*,
    /// not merely because it ran. `key:<hash>` otherwise: a task that declared
    /// no outputs has nothing observable to offer, so its own key is the only
    /// honest proxy for it.
    stamps: Mutex<BTreeMap<String, String>>,
    /// Set by `run --force`. Lives here rather than on the call because task
    /// execution is driven by supervisors, which have no argument to thread.
    force: AtomicBool,
    /// Extra arguments for one named task, from `run <task> -- <args>`.
    passthrough: Mutex<Option<(String, Vec<String>)>>,
    /// Live children, so an out-of-band client can reach a node's terminal.
    handles: Mutex<BTreeMap<String, Arc<Handle>>>,
    /// Nodes with a terminal attached. One at a time: two clients typing into the
    /// same shell interleave into nonsense.
    attached: Mutex<std::collections::BTreeSet<String>>,
    /// Desktop notifications, when the project asked for them.
    notifier: Mutex<crate::notify::Notifier>,
    /// Per-task outcomes for this run, in completion order.
    outcomes: Mutex<Vec<crate::summary::TaskOutcomeRecord>>,
    /// Paths that matched each node's globs since it last ran, handed to its
    /// next process as `TURBORUST_CHANGED`.
    changed: Mutex<BTreeMap<String, std::collections::BTreeSet<String>>>,
    /// How much of the cache to check against reality, and how many hits have
    /// gone by since the last check.
    verify: crate::config::VerifyMode,
    hits: AtomicU64,
    /// Build parallelism.
    ///
    /// The resource being limited is *building*, not running. A service holds a
    /// permit only until it is ready; holding one for its whole life would
    /// deadlock the graph the moment `concurrency` services were up and a
    /// dependent still needed a slot.
    slots: Arc<tokio::sync::Semaphore>,
    /// Ticks on every status change. Dependents wake on it and read the board
    /// from `state`; the value itself means nothing but "something changed".
    board: watch::Sender<u64>,
}

impl Engine {
    pub fn new(
        plan: Arc<Plan>,
        cache: Arc<Cache>,
    ) -> (Arc<Self>, mpsc::UnboundedReceiver<ProcEvent>) {
        Self::new_verifying(plan, cache, crate::config::VerifyMode::Off)
    }

    /// As [`Engine::new`], but re-checking some share of cache hits against what
    /// the task actually produces.
    pub fn new_verifying(
        plan: Arc<Plan>,
        cache: Arc<Cache>,
        verify: crate::config::VerifyMode,
    ) -> (Arc<Self>, mpsc::UnboundedReceiver<ProcEvent>) {
        let (events_tx, events_rx) = mpsc::unbounded_channel();
        let (reload_tx, _) = broadcast::channel(64);
        let seed: Vec<(String, &'static str, Option<u16>)> = plan
            .order
            .iter()
            .map(|n| {
                let node = plan.get(n).expect("node in order");
                let kind = if node.kind == Kind::Task {
                    "task"
                } else {
                    "service"
                };
                // A `serve` node's port lives in its serve block, not `port`.
                let port = node.port.or_else(|| node.serve.as_ref().map(|s| s.port));
                (n.clone(), kind, port)
            })
            .collect();
        let concurrency = crate::config::concurrency(plan.concurrency);
        let plan_notify = plan.notify;
        let state = Arc::new(Mutex::new(AppState::new(&seed, 20_000)));
        {
            let mut st = state.lock().unwrap();
            for name in &plan.order {
                let pattern = plan
                    .get(name)
                    .ok()
                    .and_then(|n| n.health.as_ref())
                    .and_then(|h| h.log.clone());
                st.set_ready_pattern(name, pattern);
                let reloading = plan.get(name).ok().and_then(|n| n.reloading.clone());
                st.set_reload_pattern(name, reloading);
            }
        }
        let engine = Arc::new(Engine {
            plan,
            state,
            cache,
            events_tx,
            reload_tx,
            stamps: Mutex::new(BTreeMap::new()),
            verify,
            hits: AtomicU64::new(0),
            force: AtomicBool::new(false),
            passthrough: Mutex::new(None),
            handles: Mutex::new(BTreeMap::new()),
            attached: Mutex::new(std::collections::BTreeSet::new()),
            slots: Arc::new(tokio::sync::Semaphore::new(concurrency)),
            outcomes: Mutex::new(Vec::new()),
            changed: Mutex::new(BTreeMap::new()),
            notifier: Mutex::new(crate::notify::Notifier::new(plan_notify)),
            board: watch::channel(0).0,
        });
        (engine, events_rx)
    }

    /// Waits for a build slot, marking the node queued while it waits.
    ///
    /// The permit is returned by dropping it, which every caller does once the
    /// node has finished building — a task on completion, a service on becoming
    /// ready.
    async fn acquire_slot(&self, name: &str) -> tokio::sync::OwnedSemaphorePermit {
        if let Ok(permit) = self.slots.clone().try_acquire_owned() {
            return permit;
        }
        self.set(name, Status::Queued);
        self.slots
            .clone()
            .acquire_owned()
            .await
            .expect("the semaphore is never closed")
    }

    /// Publishes a node's child so `connect` can reach it.
    fn register_handle(&self, name: &str, handle: Arc<Handle>) {
        self.handles
            .lock()
            .unwrap()
            .insert(name.to_string(), handle);
    }

    fn forget_handle(&self, name: &str) {
        self.handles.lock().unwrap().remove(name);
    }

    /// Waits for a task's child, published for as long as it runs.
    ///
    /// Only services used to be published, so nothing could reach a task
    /// mid-run: shutdown waited out its stop timeout and exited with the task
    /// still going, and a forced stop could not kill it (cairn 0085).
    async fn wait_registered(&self, name: &str, handle: Handle) -> i32 {
        let handle = Arc::new(handle);
        self.register_handle(name, handle.clone());
        let code = wait_exit(&handle).await;
        self.forget_handle(name);
        code
    }

    /// `wait_registered`, but a task still running at `limit` is stopped as a
    /// shutdown would stop it — the whole tree, `grace` to go quietly — and
    /// fails with [`TIMED_OUT`], saying which limit it hit.
    ///
    /// The flag, not the code, says it timed out: a command may exit 124 itself.
    async fn wait_limited(
        &self,
        name: &str,
        handle: Handle,
        limit: Duration,
        grace: Duration,
    ) -> (i32, bool) {
        let handle = Arc::new(handle);
        self.register_handle(name, handle.clone());
        let outcome = tokio::select! {
            code = wait_exit(&handle) => (code, false),
            _ = tokio::time::sleep(limit) => {
                let _ = handle.stop(grace).await;
                let _ = wait_exit(&handle).await;
                self.fail(
                    name,
                    format!("timed out after {}; stopped", fmt_limit(limit)),
                    TIMED_OUT,
                );
                (TIMED_OUT, true)
            }
        };
        self.forget_handle(name);
        outcome
    }

    /// Records paths that matched `name`'s globs, for its next run.
    pub fn note_changed(&self, name: &str, paths: impl IntoIterator<Item = String>) {
        self.changed
            .lock()
            .unwrap()
            .entry(name.to_string())
            .or_default()
            .extend(paths);
    }

    /// The paths since `name` last ran, newline-separated, and forgets them —
    /// so a run that was not caused by a change sees an empty value.
    fn take_changed(&self, name: &str) -> String {
        self.changed
            .lock()
            .unwrap()
            .remove(name)
            .map(|set| set.into_iter().collect::<Vec<_>>().join("\n"))
            .unwrap_or_default()
    }

    /// The child environment plus `TURBORUST_CHANGED`. Separate from
    /// `child_env` on purpose: probes and the cache key must not see it — it
    /// says why a node runs, not what it runs on.
    fn spawn_env(&self, node: &Node, changed: String) -> Result<BTreeMap<String, String>> {
        let mut env = self.child_env(node)?;
        env.insert("TURBORUST_CHANGED".into(), changed);
        Ok(env)
    }

    /// The running child for `name`, if there is one.
    pub fn handle_for(&self, name: &str) -> Option<Arc<Handle>> {
        self.handles.lock().unwrap().get(name).cloned()
    }

    /// Every declared probe but the log line, which only the event pump sees.
    ///
    /// One place for both callers — startup and a self-reload coming back — so
    /// they cannot disagree about what ready means.
    async fn probes_pass(&self, node: &Node, h: &crate::config::Health) -> Result<(), String> {
        if !health::probe(h).await {
            return Err(match (&h.tcp, &h.http) {
                (Some(port), _) => format!("nothing answering on port {port}"),
                (_, Some(url)) => format!("{url} not answering with 2xx/3xx"),
                _ => "not answering".into(),
            });
        }
        if let Some(cmd) = &h.cmd {
            let timeout =
                crate::config::parse_duration(&h.timeout).unwrap_or(Duration::from_secs(5));
            health::exec_probe(
                cmd,
                &node.cwd,
                &self.child_env(node).map_err(|e| e.to_string())?,
                node.env_mode == EnvMode::Strict,
                timeout,
            )
            .await?;
        }
        Ok(())
    }

    /// Kills every running child's whole process tree at once.
    ///
    /// For the second stop request, which means the orderly shutdown already
    /// under way is taking too long. Nothing is waited for.
    pub fn kill_all(&self) {
        for handle in self.handles.lock().unwrap().values() {
            handle.kill_now();
        }
    }

    pub fn node_names(&self) -> Vec<String> {
        self.plan.order.clone()
    }

    /// Takes the single attach slot for a node. `false` if already taken.
    pub fn claim_attach(&self, name: &str) -> bool {
        self.attached.lock().unwrap().insert(name.to_string())
    }

    pub fn release_attach(&self, name: &str) {
        self.attached.lock().unwrap().remove(name);
    }

    /// Makes every task in this run ignore its cached result.
    pub fn set_force(&self, force: bool) {
        self.force.store(force, Ordering::Relaxed);
    }

    /// Appends extra arguments to one task's command for this run only.
    pub fn set_passthrough(&self, task: String, args: Vec<String>) {
        *self.passthrough.lock().unwrap() = Some((task, args));
    }

    /// The extra arguments for `name`, if any were given.
    fn passthrough_for(&self, name: &str) -> Option<String> {
        let guard = self.passthrough.lock().unwrap();
        let (task, args) = guard.as_ref()?;
        (task == name && !args.is_empty()).then(|| args.join(" "))
    }

    /// A node's command, with any passthrough arguments appended.
    fn command_for(&self, node: &Node) -> String {
        let base = node.cmd.clone().unwrap_or_default();
        match self.passthrough_for(&node.name) {
            Some(extra) => format!("{base} {extra}"),
            None => base,
        }
    }

    /// The one place a node's status is written, and so the one place its
    /// readiness changes: readiness is `status.is_ready()`, never stored apart.
    ///
    /// Writing the status a node already has is a no-op — except for a new
    /// result (`Done`, `Healthy`), which bumps the generation however it got
    /// there. Without the no-op a node waiting on a dependency, which writes
    /// `Waiting` and then waits for the board to tick, would wake itself
    /// forever.
    fn set(&self, name: &str, status: Status) {
        let mut st = self.state.lock().unwrap();
        let same = st.nodes.get(name).is_some_and(|n| n.status == status);
        if same && !status.is_ready() {
            return;
        }
        // A new attempt invalidates the previous attempt's diagnostics; without
        // this a fixed error stays on screen until something else overwrites it.
        if matches!(status, Status::Running | Status::Starting)
            && let Some(n) = st.nodes.get_mut(name)
        {
            n.diag.reset();
            n.log_ready = false;
        }
        // Reaching a usable result is what dependents care about.
        if matches!(status, Status::Done { .. } | Status::Healthy)
            && let Some(n) = st.nodes.get_mut(name)
        {
            n.generation += 1;
        }
        // Only health transitions are notified; see the notifier for why.
        let health = match status {
            Status::Failed(_) | Status::Unhealthy => Some(true),
            Status::Healthy | Status::Done { .. } => Some(false),
            _ => None,
        };
        st.set(name, status);
        drop(st);
        self.board.send_modify(|v| *v = v.wrapping_add(1));
        if let Some(failing) = health
            && let Some(notice) = self.notifier.lock().unwrap().observe(name, failing)
        {
            crate::notify::show(&notice);
        }
    }

    /// A receiver that wakes on every status change, for supervisors waiting
    /// on their dependencies.
    fn watch_board(&self) -> watch::Receiver<u64> {
        self.board.subscribe()
    }

    /// The first of `deps` that is not ready, if any.
    fn first_unready(&self, deps: &[String]) -> Option<String> {
        let st = self.state.lock().unwrap();
        deps.iter()
            .find(|d| !st.nodes.get(*d).is_some_and(|n| n.status.is_ready()))
            .cloned()
    }

    /// Current generation of each named node, for staleness comparison.
    fn generations(&self, names: &[String]) -> Vec<u64> {
        let st = self.state.lock().unwrap();
        names
            .iter()
            .map(|n| st.nodes.get(n).map(|x| x.generation).unwrap_or(0))
            .collect()
    }

    /// Everything that contributes to a task's cache key.
    pub fn fingerprint(&self, node: &Node) -> Result<Fingerprint> {
        let matcher = self.plan.input_matcher(node)?;
        let files = cache::fingerprint_files(&self.plan.root, &matcher)?;

        let mut meta = BTreeMap::new();
        meta.insert("cmd".to_string(), self.command_for(node));
        meta.insert(
            "cwd".to_string(),
            node.cwd
                .strip_prefix(&self.plan.root)
                .unwrap_or(&node.cwd)
                .to_string_lossy()
                .to_string(),
        );
        // The mode itself is keyed: the same command under loose and strict can
        // legitimately produce different results.
        meta.insert("env_mode".to_string(), node.env_mode.as_str().to_string());
        meta.insert("outputs".to_string(), node.outputs.join("\u{1f}"));
        // Splitting moves where artifacts land, so it has to key the task.
        meta.insert(
            "target_dir".to_string(),
            node.target_dir.as_str().to_string(),
        );
        // Values from env files are not in the config, so the config's hash does
        // not cover them. Keyed by hash, not value: they are where secrets live,
        // and `why` prints what changed.
        let from_files = crate::envfile::load(&node.env_files).map_err(anyhow::Error::msg)?;
        for (k, v) in &from_files {
            if !node.env.contains_key(k) {
                meta.insert(
                    format!("env_file:{k}"),
                    blake3::hash(v.as_bytes()).to_hex()[..16].to_string(),
                );
            }
        }
        for k in &node.env_keys {
            if from_files.contains_key(k) && !node.env.contains_key(k) {
                continue;
            }
            let v = node
                .env
                .get(k)
                .cloned()
                .or_else(|| std::env::var(k).ok())
                .unwrap_or_default();
            meta.insert(format!("env:{k}"), v);
        }
        // Workspace-wide inputs: the toolchain, the config, the lockfile.
        meta.extend(self.plan.global.meta());
        // Upstream stamps fold in, so a change deep in the graph reaches every
        // downstream node without re-hashing its files.
        let stamps = self.stamps.lock().unwrap();
        for dep in &node.depends_on {
            if let Some(h) = stamps.get(dep) {
                meta.insert(format!("dep:{dep}"), h.clone());
            }
        }
        drop(stamps);

        Ok(cache::combine(files, meta))
    }

    /// Whether this hit is one of the ones being checked.
    fn should_verify(&self) -> bool {
        match self.verify {
            crate::config::VerifyMode::Off => false,
            crate::config::VerifyMode::Always => true,
            crate::config::VerifyMode::Sample => {
                let n = self.hits.fetch_add(1, Ordering::Relaxed);
                n.is_multiple_of(crate::config::VERIFY_SAMPLE_RATE)
            }
        }
    }

    fn stamp(&self, name: &str, value: String) {
        self.stamps.lock().unwrap().insert(name.to_string(), value);
    }

    /// Resolves each dependency's stamp from the cache, for callers that inspect
    /// a key without running anything.
    ///
    /// Without this `why` omits every `dep:` entry and reports a key that a real
    /// run would never produce — which was true before dependents keyed on
    /// outputs, and is worse now that the stamp is the interesting part.
    pub fn seed_stamps_from_cache(&self, node: &Node) {
        for dep in &node.depends_on {
            let Ok(upstream) = self.plan.get(dep) else {
                continue;
            };
            let Some(rec) = self.cache.load_latest(dep) else {
                continue;
            };
            let stamp = rec
                .output_hash
                .clone()
                .or_else(|| cache::hash_outputs(&self.plan.root, &rec.outputs))
                .map(|h| format!("out:{h}"))
                .unwrap_or_else(|| format!("key:{}", rec.fingerprint.hash));
            self.stamp(dep, stamp);
            self.seed_stamps_from_cache(upstream);
        }
    }

    /// Everything this run did, for `--summarize`.
    pub fn outcomes(&self) -> Vec<crate::summary::TaskOutcomeRecord> {
        self.outcomes.lock().unwrap().clone()
    }

    /// Publishes what the last task run did, for the overlay's build row.
    fn record_build(
        &self,
        name: &str,
        cached: bool,
        duration: Duration,
        fp: &Fingerprint,
        changed: Vec<crate::state::ChangedFile>,
    ) {
        self.outcomes
            .lock()
            .unwrap()
            .push(crate::summary::TaskOutcomeRecord {
                task: name.to_string(),
                cached,
                duration_ms: duration.as_millis() as u64,
                key: fp.short(),
                exit_code: 0,
                changed: changed
                    .iter()
                    .map(|c| format!("{} {} -> {}", c.path, c.from, c.to))
                    .collect(),
            });
        self.state.lock().unwrap().build = Some(crate::state::BuildInfo {
            node: name.to_string(),
            cached,
            duration_ms: duration.as_millis() as u64,
            key: fp.short(),
            changed,
        });
    }

    /// The environment a node's command actually runs with.
    ///
    /// Under strict mode this is the complete set — the child sees nothing else.
    ///
    /// Fails only when an env file has gone missing or stopped parsing since the
    /// plan was built; the failure says which.
    fn child_env(&self, node: &Node) -> Result<BTreeMap<String, String>> {
        let mut env = BTreeMap::new();
        if node.env_mode == EnvMode::Strict {
            for k in SAFE_BASE_ENV {
                if let Ok(v) = std::env::var(k) {
                    env.insert((*k).to_string(), v);
                }
            }
            for k in node.env_keys.iter().chain(node.pass_through_env.iter()) {
                if let Ok(v) = std::env::var(k) {
                    env.insert(k.clone(), v);
                }
            }
        }
        // A private target directory, when this node asked for one. Kept under
        // the workspace target dir so `cargo clean` and .gitignore still cover it.
        if node.target_dir == TargetDir::Split {
            let dir = self
                .plan
                .root
                .join("target")
                .join("turborust")
                .join(&node.name);
            env.insert("CARGO_TARGET_DIR".into(), dir.to_string_lossy().to_string());
        }
        // Env files are declared, so they reach the child under strict mode too;
        // inline `env` is explicit and wins over them.
        env.extend(crate::envfile::load(&node.env_files).map_err(anyhow::Error::msg)?);
        for (k, v) in &node.env {
            env.insert(k.clone(), v.clone());
        }
        Ok(env)
    }

    /// Archives a task's declared outputs, returning what was captured.
    ///
    /// A result too large to store is recorded as unarchived rather than
    /// silently dropped, so a later hit knows it has to fall back to checking the
    /// files are still on disk.
    fn capture_outputs(&self, node: &Node, name: &str, hash: &str) -> (Vec<String>, bool) {
        if node.outputs.is_empty() {
            return (Vec::new(), true);
        }
        let Ok(matcher) = self.output_matcher(node) else {
            return (Vec::new(), false);
        };
        let files = cache::collect_outputs(&self.plan.root, &matcher);
        match self.cache.archive(name, hash, &self.plan.root, &files) {
            Ok(Some(_)) => (files, true),
            Ok(None) => {
                self.log(
                    name,
                    format!(
                        "outputs exceed {} MiB; caching the result but not its files",
                        cache::MAX_ARCHIVE_BYTES / 1024 / 1024
                    ),
                );
                (files, false)
            }
            Err(e) => {
                self.log(name, format!("could not archive outputs: {e}"));
                (files, false)
            }
        }
    }

    /// Outputs are matched without the global ignore list: `target/` is normally
    /// ignored, and is exactly where build outputs live.
    fn output_matcher(&self, node: &Node) -> Result<cache::Matcher> {
        cache::Matcher::new(&node.outputs, &[])
    }

    /// The files a node's `outputs` globs currently match on disk.
    pub fn declared_outputs(&self, node: &Node) -> Vec<String> {
        match self.output_matcher(node) {
            Ok(m) => cache::collect_outputs(&self.plan.root, &m),
            Err(_) => Vec::new(),
        }
    }

    /// Removes a node's declared outputs, so the next run has to produce them.
    ///
    /// Without this a second run of an incremental tool is often a no-op that
    /// trivially reproduces byte-identical files, and the check proves nothing.
    pub fn clear_outputs(&self, node: &Node) {
        for rel in self.declared_outputs(node) {
            let _ = std::fs::remove_file(self.plan.root.join(rel));
        }
    }

    fn outputs_present(&self, node: &Node) -> bool {
        node.outputs.iter().all(|o| self.plan.root.join(o).exists())
    }

    /// Re-runs a task that just hit, and reports if it did not reproduce what
    /// the cache had stored.
    ///
    /// Returns `None` when the cache told the truth.
    async fn recheck(&self, node: &Node, name: &str, rec: &Record) -> Result<Option<String>> {
        let promised = rec.output_hash.clone().unwrap_or_default();
        self.clear_outputs(node);
        let cmd = self.command_for(node);
        let handle = proc::spawn_with(
            name,
            &cmd,
            &node.cwd,
            &self.child_env(node)?,
            node.env_mode == EnvMode::Strict,
            self.events_tx.clone(),
        )?;
        if self.wait_registered(name, handle).await != 0 {
            // The task cannot be re-run right now. That is a problem, but it is
            // not evidence the stored result was wrong.
            return Ok(Some(format!(
                "verify: `{name}` failed when re-run, so its cached result could not be checked"
            )));
        }
        let produced = cache::hash_outputs(&self.plan.root, &self.declared_outputs(node));
        if produced.as_deref() == Some(promised.as_str()) {
            return Ok(None);
        }
        Ok(Some(format!(
            "verify: `{name}` did not reproduce its cached outputs \
             (stored b3:{}, produced b3:{}). The entry has been dropped; run \
             `turborust verify {name}` to see which file differs.",
            promised.chars().take(8).collect::<String>(),
            produced
                .as_deref()
                .map(|h| h.chars().take(8).collect::<String>())
                .unwrap_or_else(|| "nothing".into()),
        )))
    }

    /// Runs a task, honouring the cache. Never restarts; that is the caller's job.
    pub async fn run_task(&self, name: &str, force: bool) -> Result<TaskOutcome> {
        let node = self.plan.get(name)?.clone();
        let start = Instant::now();
        // Taken now, hit or miss: left behind on a cache hit, it would be handed
        // to some later run that no change caused.
        let changed_paths = self.take_changed(name);

        let fp = if node.inputs.is_empty() {
            None
        } else {
            Some(self.fingerprint(&node)?)
        };
        if let Some(fp) = &fp {
            // Seeded with the key so a dependent always has something to fold in;
            // replaced by an output stamp below if this node produces any.
            self.stamp(name, format!("key:{}", fp.hash));
        }

        let force = force || self.force.load(Ordering::Relaxed);
        // A loose task sees variables the key cannot account for, so serving it
        // from cache would be a guess. Being honestly uncacheable beats being
        // occasionally wrong.
        let cacheable = fp.is_some()
            && node.cache == crate::config::CachePolicy::Enabled
            && node.env_mode != EnvMode::Loose
            && !force;
        if cacheable
            && let Some(fp) = &fp
            && let Some(rec) = self.cache.load(name, &fp.hash)
            && rec.exit_code == 0
        {
            // A hit must actually reproduce the result: either replay the stored
            // artifacts, or — for a result too large to archive — confirm the
            // files are still where the previous run left them. A key match alone
            // would happily "replay" a build someone deleted.
            let replayed = if rec.archived {
                self.cache
                    .restore(name, &fp.hash, &self.plan.root, &rec.outputs)
                    .unwrap_or(false)
            } else {
                self.outputs_present(&node)
            };
            // A hit is a promise that this key reproduces these outputs. Under
            // `verify` a share of those promises get tested, because a cache
            // that has started lying does it silently and forever.
            if replayed
                && rec.output_hash.is_some()
                && !node.outputs.is_empty()
                && self.should_verify()
                && let Some(failure) = self.recheck(&node, name, &rec).await?
            {
                self.cache.forget(name, &fp.hash);
                self.log(name, failure.clone());
                self.set(name, Status::Failed(1));
                return Ok(TaskOutcome {
                    cached: false,
                    code: 1,
                    duration: start.elapsed(),
                    fingerprint: Some(fp.clone()),
                });
            }
            if replayed {
                // The replayed outputs are what dependents key on, exactly as
                // they would after a real run.
                if let Some(h) = rec
                    .output_hash
                    .clone()
                    .or_else(|| cache::hash_outputs(&self.plan.root, &rec.outputs))
                {
                    self.stamp(name, format!("out:{h}"));
                }
                let d = Duration::from_millis(rec.duration_ms);
                self.record_build(name, true, d, fp, Vec::new());
                self.set(
                    name,
                    Status::Done {
                        cached: true,
                        duration: d,
                    },
                );
                let how = if rec.archived && !rec.outputs.is_empty() {
                    format!(", {} output(s) restored", rec.outputs.len())
                } else {
                    String::new()
                };
                self.log(
                    name,
                    format!(
                        "cache hit  b3:{}  (saved {}{how})",
                        fp.short(),
                        crate::state::fmt_dur(d)
                    ),
                );
                return Ok(TaskOutcome {
                    cached: true,
                    code: 0,
                    duration: d,
                    fingerprint: Some(fp.clone()),
                });
            }
        }

        // A miss is worth explaining before the build starts, not after.
        let changed = match (&fp, self.cache.load_latest(name)) {
            (Some(now), Some(prev)) => cache::diff(&prev.fingerprint, now)
                .iter()
                .filter_map(|c| match c {
                    cache::Change::Modified { path, from, to } => Some(crate::state::ChangedFile {
                        path: path.clone(),
                        from: from.chars().take(8).collect(),
                        to: to.chars().take(8).collect(),
                    }),
                    cache::Change::Added(path, to) => Some(crate::state::ChangedFile {
                        path: path.clone(),
                        from: "new".into(),
                        to: to.chars().take(8).collect(),
                    }),
                    _ => None,
                })
                .take(20)
                .collect(),
            _ => Vec::new(),
        };

        // Held until this task finishes: running a task IS building.
        let _slot = self.acquire_slot(name).await;
        self.set(name, Status::Running);
        let cmd = self.command_for(&node);
        let handle = proc::spawn_with(
            name,
            &cmd,
            &node.cwd,
            &self.spawn_env(&node, changed_paths)?,
            node.env_mode == EnvMode::Strict,
            self.events_tx.clone(),
        )?;
        let (code, timed_out) = match node.timeout {
            None => (self.wait_registered(name, handle).await, false),
            Some(limit) => {
                self.wait_limited(name, handle, limit, node.stop_timeout)
                    .await
            }
        };
        let duration = start.elapsed();

        if timed_out {
            // Already reported and marked failed by `wait_limited`.
        } else if code == 0 {
            if let Some(fp) = &fp {
                self.record_build(name, false, duration, fp, changed);
            }
            self.set(
                name,
                Status::Done {
                    cached: false,
                    duration,
                },
            );
            if let Some(fp) = &fp {
                let (outputs, archived) = self.capture_outputs(&node, name, &fp.hash);
                let output_hash = cache::hash_outputs(&self.plan.root, &outputs);
                if let Some(h) = &output_hash {
                    self.stamp(name, format!("out:{h}"));
                }
                let _ = self.cache.store(&Record {
                    task: name.to_string(),
                    fingerprint: fp.clone(),
                    exit_code: 0,
                    duration_ms: duration.as_millis() as u64,
                    outputs,
                    archived,
                    output_hash,
                });
            }
        } else if self.state.lock().unwrap().shutting_down {
            // Ended by the shutdown, not by its own failure: reporting it as
            // failed would raise a failure notification for a ctrl-c.
            self.set(name, Status::Stopped);
        } else {
            self.set(name, Status::Failed(code));
        }
        Ok(TaskOutcome {
            cached: false,
            code,
            duration,
            fingerprint: fp,
        })
    }

    /// A node failed before it had a process to fail with — a spawn that did
    /// not happen, a fingerprint that could not be taken.
    ///
    /// The reporter only shows a node's output once that node finishes, and
    /// "finishes" used to mean a process exiting. With no process there was no
    /// exit, so the reason sat in a buffer nobody flushed: `run` exited 1 and
    /// printed nothing (cairn 0086).
    fn fail(&self, name: &str, error: String, code: i32) {
        self.log(name, error.clone());
        let _ = self.events_tx.send(ProcEvent::Failed {
            proc: name.to_string(),
            error,
        });
        self.set(name, Status::Failed(code));
    }

    fn log(&self, proc: &str, text: String) {
        let _ = self.events_tx.send(ProcEvent::Line(crate::proc::LogLine {
            seq: crate::proc::next_seq(),
            proc: proc.to_string(),
            text: format!("\u{1b}[2m[turborust]\u{1b}[0m {text}"),
            transient: false,
        }));
    }
}

/// Runs the plan's tasks once, in dependency order, then stops whatever it started.
///
/// Services in the closure are supervised exactly as `up` supervises them, so a
/// task that depends on a service waits for that service's readiness probe rather
/// than running without it. Reusing the supervision tree is the point: a separate
/// sequential path for `run` is what let the two behaviours drift apart.
pub async fn run_once(engine: Arc<Engine>) -> Result<i32> {
    run_once_with(engine, false).await
}

/// One-shot run. With `keep_going`, a failure stops only what depends on it.
pub async fn run_once_with(engine: Arc<Engine>, keep_going: bool) -> Result<i32> {
    run_once_until(engine, keep_going, std::future::pending()).await
}

/// One-shot run that `stop` can end early, with the exit code it resolves to.
///
/// An early end still goes through the full shutdown — reverse dependency
/// order, each node's stop timeout — because skipping it is what orphaned the
/// children of an interrupted `run` (cairn 0085).
pub async fn run_once_until(
    engine: Arc<Engine>,
    keep_going: bool,
    stop: impl std::future::Future<Output = i32>,
) -> Result<i32> {
    let tasks: Vec<String> = engine
        .plan
        .order
        .iter()
        .filter(|n| {
            engine
                .plan
                .get(n)
                .map(|x| x.kind == Kind::Task)
                .unwrap_or(false)
        })
        .cloned()
        .collect();
    if tasks.is_empty() {
        anyhow::bail!("no tasks to run (see `turborust plan`)");
    }

    let (wires, supervisors) = spawn_supervisors(engine.clone());
    let finished = async {
        if keep_going {
            await_all_tasks(&engine, &tasks).await
        } else {
            await_tasks(&engine, &tasks).await
        }
    };
    let code = tokio::select! {
        code = finished => code,
        code = stop => code,
    };
    shutdown(&engine, &wires, supervisors).await;
    Ok(code)
}

/// Waits for every task to settle, letting unrelated work finish after a failure.
///
/// "Settled" has to include *blocked*: a task whose dependency failed will never
/// run, so waiting for it to reach a terminal status would hang forever. That is
/// a reachability question over the dependency graph, which is what makes the
/// reverse index worth having.
async fn await_all_tasks(engine: &Engine, tasks: &[String]) -> i32 {
    loop {
        let (failed, blocked, settled) = {
            let st = engine.state.lock().unwrap();
            let failed: Vec<String> = tasks
                .iter()
                .filter(|n| matches!(st.nodes[*n].status, Status::Failed(_)))
                .cloned()
                .collect();

            // Anything downstream of a failure is blocked, not failing.
            let mut blocked: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
            for name in &failed {
                blocked.extend(engine.plan.transitive_dependents(name));
            }
            let settled = tasks.iter().all(|n| {
                blocked.contains(n)
                    || matches!(
                        st.nodes[n].status,
                        Status::Done { .. } | Status::Failed(_) | Status::Exited
                    )
            });
            (failed, blocked, settled)
        };

        if settled {
            if failed.is_empty() {
                return 0;
            }
            let skipped: Vec<&String> = tasks.iter().filter(|n| blocked.contains(*n)).collect();
            engine.log(
                "turborust",
                format!(
                    "{} task(s) failed: {}{}",
                    failed.len(),
                    failed.join(", "),
                    if skipped.is_empty() {
                        String::new()
                    } else {
                        format!(
                            "; {} skipped because of them: {}",
                            skipped.len(),
                            skipped
                                .iter()
                                .map(|s| s.as_str())
                                .collect::<Vec<_>>()
                                .join(", ")
                        )
                    }
                ),
            );
            // The first failure's own code, so a single-failure run still
            // reports what the task reported.
            let st = engine.state.lock().unwrap();
            return failed
                .iter()
                .find_map(|n| match st.nodes[n].status {
                    Status::Failed(c) if c != 0 => Some(c),
                    _ => None,
                })
                .unwrap_or(1);
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Blocks until every task has finished, or something makes finishing impossible.
async fn await_tasks(engine: &Engine, tasks: &[String]) -> i32 {
    loop {
        {
            let st = engine.state.lock().unwrap();

            // A task that failed ends the run with its own exit code.
            for name in tasks {
                if let Some(Status::Failed(code)) = st.nodes.get(name).map(|n| n.status.clone()) {
                    return code;
                }
            }

            // A service that died takes its dependents' readiness with it, so
            // waiting for those tasks would hang forever. In a one-shot run a
            // crashed dependency is a failure, not something to ride out.
            let dead = st.nodes.iter().find_map(|(name, node)| {
                (node.kind == "service"
                    && matches!(
                        node.status,
                        Status::Failed(_) | Status::Backoff(_) | Status::Unhealthy
                    ))
                .then(|| name.clone())
            });

            let all_done = tasks.iter().all(|n| {
                matches!(
                    st.nodes.get(n).map(|x| &x.status),
                    Some(Status::Done { .. })
                )
            });
            drop(st);

            if let Some(name) = dead {
                engine.log(&name, "service failed; giving up on this run".into());
                return 1;
            }
            if all_done {
                return 0;
            }
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// The exit code of a task stopped for running past its `timeout`: the one GNU
/// `timeout(1)` uses, so scripts that already know it read it right.
pub const TIMED_OUT: i32 = 124;

/// A limit as it would have been written: `10m`, not `600s`.
fn fmt_limit(d: Duration) -> String {
    let ms = d.as_millis();
    match () {
        _ if ms.is_multiple_of(3_600_000) => format!("{}h", ms / 3_600_000),
        _ if ms.is_multiple_of(60_000) => format!("{}m", ms / 60_000),
        _ if ms.is_multiple_of(1000) => format!("{}s", ms / 1000),
        _ => format!("{ms}ms"),
    }
}

async fn wait_exit(handle: &Handle) -> i32 {
    loop {
        if let Some(code) = handle.try_exit_code() {
            return code;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Wiring for one supervised node: its control queue. Readiness is not
/// wired; it is read from status (see [`Engine::set`]).
pub struct Wire {
    pub ctl_tx: ctl::Sender,
}

/// Starts a supervisor task per node and returns their control handles.
pub fn spawn_supervisors(engine: Arc<Engine>) -> (BTreeMap<String, Wire>, Supervisors) {
    let mut wires = BTreeMap::new();
    let mut joins: Supervisors = Supervisors::new();
    for name in &engine.plan.order {
        let node = engine.plan.get(name).expect("node in order").clone();
        let (ctl_tx, ctl_rx) = ctl::channel();
        wires.insert(name.clone(), Wire { ctl_tx });
        let engine = engine.clone();
        joins.insert(
            name.clone(),
            tokio::spawn(async move {
                supervise(engine, node, ctl_rx).await;
            }),
        );
    }
    (wires, joins)
}

async fn supervise(engine: Arc<Engine>, node: Node, mut ctl: ctl::Receiver) {
    let name = node.name.clone();
    let mut board = engine.watch_board();
    let mut backoff = node.backoff_min;
    let mut crashes = CrashCount::new(&node);

    loop {
        // 1. Block until every dependency is ready — while staying answerable.
        if !wait_for_deps(&engine, &node, &mut board, &mut ctl).await {
            engine.set(&name, Status::Stopped);
            return;
        }
        if ctl.drain_stale() {
            // Shutdown began while we were blocked on dependencies. Returning
            // here is what keeps `up` from spawning a process on its way out.
            engine.set(&name, Status::Stopped);
            return;
        }
        // Snapshot what we are about to run against, so a later wake-up can tell
        // a genuinely new dependency result from an echo of this one.
        let seen = engine.generations(&node.depends_on);

        if node.kind == Kind::Task {
            // Out of ready before anything else, so dependents stop now rather
            // than after the run — a cache hit never otherwise leaves `Done`.
            engine.set(&name, Status::Running);
            let outcome = match engine.run_task(&name, false).await {
                Ok(o) => o,
                Err(e) => {
                    engine.fail(&name, format!("error: {e:#}"), 1);
                    TaskOutcome {
                        cached: false,
                        code: 1,
                        duration: Duration::ZERO,
                        fingerprint: None,
                    }
                }
            };
            if outcome.code == 0 && !outcome.cached {
                let _ = engine.reload_tx.send(RELOAD_FULL.to_string());
            }
            // A task stays put until something asks it to run again.
            match wait_for_rerun(&engine, &node.depends_on, &seen, &mut board, &mut ctl).await {
                Some(reason) => {
                    if let Some(n) = engine.state.lock().unwrap().nodes.get_mut(&name) {
                        n.reason = Some(reason);
                    }
                    continue;
                }
                None => return,
            }
        }

        // 2. Services: reclaim the port before binding it again.
        if let Some(port) = node.port
            && !health::wait_port_free(port, Duration::from_secs(5))
                .await
                .unwrap_or(true)
        {
            let who = health::port_holder(port).unwrap_or_else(|| "unknown process".into());
            engine.log(
                &name,
                format!("port {port} still held by {who}; starting anyway"),
            );
        }

        // 3. Spawn (or, for a pure `serve` node, mark ready — it is hosted in-process).
        let Some(cmd) = node.cmd.clone() else {
            engine.set(&name, Status::Healthy);
            match wait_for_rerun(&engine, &node.depends_on, &seen, &mut board, &mut ctl).await {
                Some(_) => continue,
                None => return,
            }
        };

        // A service holds a slot only while starting. Releasing it at `Healthy`
        // rather than at exit is what keeps a graph wider than `concurrency`
        // from deadlocking.
        let slot = engine.acquire_slot(&name).await;
        engine.set(&name, Status::Starting);
        let spawned = engine
            .spawn_env(&node, engine.take_changed(&name))
            .and_then(|child_env| {
                proc::spawn_with(
                    &name,
                    &cmd,
                    &node.cwd,
                    &child_env,
                    node.env_mode == EnvMode::Strict,
                    engine.events_tx.clone(),
                )
            });
        let handle = match spawned {
            Ok(h) => h,
            Err(e) => {
                engine.fail(&name, format!("spawn failed: {e:#}"), 127);
                if node.restart == crate::config::RestartPolicy::Never {
                    return;
                }
                if crashes.exhausted(Duration::ZERO) {
                    drop(slot);
                    if !give_up(
                        &engine,
                        &node,
                        &seen,
                        &mut board,
                        &mut ctl,
                        &mut crashes,
                        127,
                    )
                    .await
                    {
                        return;
                    }
                    backoff = node.backoff_min;
                    continue;
                }
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(node.backoff_max);
                continue;
            }
        };
        let started = Instant::now();
        let handle = Arc::new(handle);
        engine.register_handle(&name, handle.clone());
        {
            let mut st = engine.state.lock().unwrap();
            if let Some(n) = st.nodes.get_mut(&name) {
                n.pid = handle.pid;
                n.started_at = Some(Instant::now());
            }
        }

        // 4. Readiness. Only after this do dependents get to start.
        let health_cfg = node.health.clone();
        let ready_flag = match &health_cfg {
            None => {
                engine.set(&name, Status::Healthy);
                true
            }
            Some(_) => {
                match probe_until(&engine, &node, &handle, &mut ctl, &mut board, &seen).await {
                    Ready::Yes => {
                        engine.set(&name, Status::Healthy);
                        backoff = node.backoff_min;
                        true
                    }
                    Ready::No => {
                        // Not `Starting`: nothing is starting any more, and a node
                        // left reading that way kept a one-shot `run` waiting on it
                        // forever.
                        engine.log(&name, "readiness probe never passed".into());
                        engine.set(&name, Status::Unhealthy);
                        if let Some(n) = engine.state.lock().unwrap().nodes.get_mut(&name) {
                            n.reason = Some(format!(
                                "not ready within {}",
                                crate::state::fmt_dur(node.ready_timeout)
                            ));
                        }
                        false
                    }
                    Ready::Stop => {
                        engine.set(&name, Status::Stopped);
                        // Unpublished only once it is gone: a forced stop during the grace
                        // period finds children through the published handles (cairn 0085).
                        let _ = handle.stop(node.stop_timeout).await;
                        engine.forget_handle(&name);
                        return;
                    }
                    Ready::Interrupted(reason) => {
                        engine.set(&name, Status::Stopping);
                        engine.log(&name, format!("restarting mid-startup: {reason}"));
                        let _ = handle.stop(node.stop_timeout).await;
                        engine.forget_handle(&name);
                        continue;
                    }
                }
            }
        };
        drop(slot);
        if ready_flag {
            let _ = engine.reload_tx.send(RELOAD_FULL.to_string());
        }

        // 5. Run until the child exits or the mail says otherwise.
        let mut own = SelfReload::new(&engine, &node, ready_flag);
        let mut alive = Liveness::new(&node);
        let stop_reason = loop {
            tokio::select! {
                code = wait_exit(&handle) => break StopReason::Exited(code),
                _ = tokio::time::sleep(own.poll), if own.watching => {
                    own.step(&engine, &node).await;
                }
                _ = alive.tick.tick(), if alive.enabled => {
                    if let Some(why) = alive.check(&engine, &node).await {
                        break StopReason::Restart(why);
                    }
                }
                changed = board.changed() => {
                    if changed.is_err() {
                        break StopReason::Stop;
                    }
                    if let Some(why) = dependency_moved(&engine, &node, &seen) {
                        break StopReason::Dependency(why);
                    }
                }
                msg = ctl.recv() => match msg {
                    // The process is watching the same files and rebuilding
                    // itself; a restart now would throw away what it keeps.
                    Some(Ctl::Restart(_)) if node.reload == Reload::SelfManaged => continue,
                    Some(Ctl::Restart(r)) => match node.on_change {
                        OnChange::Ignore => continue,
                        OnChange::Signal => {
                            match node.signal {
                                Some(sig) if handle.signal(sig) => {
                                    engine.log(&name, format!("signalled ({sig}): {r}"));
                                }
                                Some(sig) => {
                                    engine.log(&name, format!("could not send signal {sig}"));
                                }
                                None => engine.log(
                                    &name,
                                    "on_change = \"signal\" needs a `signal` value".into(),
                                ),
                            }
                            continue;
                        }
                        // Once running, `queue` and `restart` agree: there is no
                        // startup left to protect.
                        OnChange::Restart | OnChange::Queue => break StopReason::Restart(r),
                    },
                    Some(Ctl::Stop) | None => break StopReason::Stop,
                },
            }
        };

        match stop_reason {
            StopReason::Stop => {
                engine.set(&name, Status::Stopped);
                let _ = handle.stop(node.stop_timeout).await;
                engine.forget_handle(&name);
                return;
            }
            StopReason::Restart(reason) | StopReason::Dependency(reason) => {
                engine.set(&name, Status::Stopping);
                {
                    let mut st = engine.state.lock().unwrap();
                    if let Some(n) = st.nodes.get_mut(&name) {
                        n.restarts += 1;
                        n.reason = Some(reason.clone());
                    }
                }
                engine.log(&name, format!("restarting: {reason}"));
                let _ = handle.stop(node.stop_timeout).await;
                engine.forget_handle(&name);
                backoff = node.backoff_min;
                crashes.reset();
                continue;
            }
            StopReason::Exited(code) => {
                // The child is gone; nothing should be able to attach to it.
                engine.forget_handle(&name);
                let policy = node.restart;
                use crate::config::RestartPolicy::*;
                let should_restart = match policy {
                    Never => false,
                    OnChange => false, // only a watch event brings it back
                    Always => true,
                };
                if code == 0 && policy != Always {
                    engine.set(&name, Status::Exited);
                } else if code != 0 {
                    engine.set(&name, Status::Failed(code));
                }
                if !should_restart {
                    // Stay parked, but remain restartable from a watch event.
                    match wait_for_rerun(&engine, &node.depends_on, &seen, &mut board, &mut ctl)
                        .await
                    {
                        Some(reason) => {
                            engine.log(&name, format!("restarting: {reason}"));
                            continue;
                        }
                        None => return,
                    }
                }
                if crashes.exhausted(started.elapsed()) {
                    if !give_up(
                        &engine,
                        &node,
                        &seen,
                        &mut board,
                        &mut ctl,
                        &mut crashes,
                        code,
                    )
                    .await
                    {
                        return;
                    }
                    backoff = node.backoff_min;
                    continue;
                }
                // Exponential backoff bounded by `backoff_max`: a service that
                // crashes instantly must not become a fork bomb.
                engine.set(&name, Status::Backoff(backoff));
                let delay = backoff;
                backoff = (backoff * 2).min(node.backoff_max);
                let interrupted = tokio::select! {
                    _ = tokio::time::sleep(delay) => None,
                    msg = ctl.recv() => msg,
                };
                match interrupted {
                    Some(Ctl::Stop) => {
                        engine.set(&name, Status::Stopped);
                        return;
                    }
                    Some(Ctl::Restart(r)) => {
                        engine.log(&name, format!("restarting: {r}"));
                        backoff = node.backoff_min;
                        crashes.reset();
                    }
                    _ => {}
                }
                continue;
            }
        }
    }
}

/// Crash restarts in a row, against `max_restarts`.
struct CrashCount {
    limit: Option<u32>,
    /// A run longer than this was not part of a crash loop.
    settled: Duration,
    restarts: u32,
}

impl CrashCount {
    fn new(node: &Node) -> CrashCount {
        CrashCount {
            limit: node.max_restarts,
            settled: node.backoff_max,
            restarts: 0,
        }
    }

    /// Records a crash after a run of `ran`, and says whether the restart it
    /// would earn is one too many.
    fn exhausted(&mut self, ran: Duration) -> bool {
        if ran > self.settled {
            self.restarts = 0;
        }
        let Some(limit) = self.limit else {
            return false;
        };
        if self.restarts >= limit {
            return true;
        }
        self.restarts += 1;
        false
    }

    fn reset(&mut self) {
        self.restarts = 0;
    }
}

/// Parks a node that has used up its restarts until a change or a manual
/// restart asks for it again. `false` means the supervisor should end.
async fn give_up(
    engine: &Engine,
    node: &Node,
    seen: &[u64],
    board: &mut watch::Receiver<u64>,
    ctl: &mut ctl::Receiver,
    crashes: &mut CrashCount,
    code: i32,
) -> bool {
    let name = node.name.as_str();
    let why = format!(
        "gave up after {} restart(s) in a row; waiting for a change or a manual restart",
        crashes.restarts
    );
    engine.log(name, why.clone());
    engine.set(name, Status::Failed(code));
    if let Some(n) = engine.state.lock().unwrap().nodes.get_mut(name) {
        n.reason = Some(why);
    }
    match wait_for_rerun(engine, &node.depends_on, seen, board, ctl).await {
        Some(reason) => {
            engine.log(name, format!("restarting: {reason}"));
            crashes.reset();
            true
        }
        None => false,
    }
}

enum StopReason {
    Exited(i32),
    Restart(String),
    Dependency(String),
    Stop,
}

/// Follows a `reload = "self"` node through the rebuilds it runs itself.
///
/// The supervisor never restarts such a node for a change, so without this it
/// would read healthy straight through a failing rebuild and tell its
/// dependents nothing. This is readiness for a process that does not restart:
/// `reloading` is the edge out of ready, `health` the edge back in, and the
/// usual cascade runs on both.
///
/// Driven by polling the match counters the event pump keeps, at the probe
/// interval — the same way the startup probe watches for its log line.
struct SelfReload {
    poll: Duration,
    /// False for nodes with nothing to follow.
    watching: bool,
    /// `reload_matches` already acted on.
    seen: u64,
    phase: Phase,
}

enum Phase {
    Ready,
    Rebuilding {
        deadline: tokio::time::Instant,
    },
    /// Out of ready, and waiting for the process to recover by itself — which a
    /// self-reloading process does, on the next edit that compiles.
    Unready,
}

impl SelfReload {
    fn new(engine: &Engine, node: &Node, ready: bool) -> SelfReload {
        // Rebuilds announced during the first build belong to startup, which
        // the probe has already accounted for — unless one came after the last
        // ready line. The probe only looks every `interval`, so a rebuild can
        // already be under way by the time startup is confirmed; leaving the
        // latest one unseen makes the first step pick it up.
        let seen = engine
            .state
            .lock()
            .unwrap()
            .nodes
            .get(&node.name)
            .map(|n| {
                let in_flight =
                    n.reload_matches > 0 && n.ready_matches_at_reload == n.ready_matches;
                n.reload_matches - u64::from(in_flight)
            })
            .unwrap_or(0);
        let poll = node
            .health
            .as_ref()
            .and_then(|h| crate::config::parse_duration(&h.interval).ok())
            .unwrap_or(Duration::from_millis(300));
        SelfReload {
            poll,
            watching: node.reload == Reload::SelfManaged && node.reloading.is_some(),
            seen,
            // A first build slower than `ready_timeout` is common for exactly
            // these processes; it should come good when it finishes, not stay
            // unhealthy until restarted.
            phase: if ready { Phase::Ready } else { Phase::Unready },
        }
    }

    async fn step(&mut self, engine: &Engine, node: &Node) {
        let name = node.name.as_str();
        let Some((reloads, ready_again, errors)) =
            engine.state.lock().unwrap().nodes.get(name).map(|n| {
                (
                    n.reload_matches,
                    n.ready_matches > n.ready_matches_at_reload,
                    n.diag.errors(),
                )
            })
        else {
            return;
        };

        if reloads != self.seen {
            self.seen = reloads;
            // The down edge, without a restart: `Reloading` is not ready, so
            // dependents see exactly what they would if this node had been
            // stopped and started.
            engine.set(name, Status::Reloading);
            set_reason(engine, name, "rebuilding itself".into());
            self.phase = Phase::Rebuilding {
                deadline: tokio::time::Instant::now() + node.ready_timeout,
            };
        }

        if matches!(self.phase, Phase::Ready) {
            return;
        }
        let probes_pass = match &node.health {
            Some(h) => engine.probes_pass(node, h).await.is_ok(),
            None => true,
        };
        if ready_again && probes_pass {
            engine.set(name, Status::Healthy);
            set_reason(engine, name, "reloaded itself".into());
            // Deliberately no browser reload: the process has just patched
            // itself in place, and a full reload would discard the state it
            // went to the trouble of keeping.
            self.phase = Phase::Ready;
            return;
        }
        if let Phase::Rebuilding { deadline } = self.phase {
            let failure = if errors > 0 {
                // rustc has already said how this ends; waiting out the
                // timeout would only delay saying so.
                Some(format!(
                    "rebuild failed with {errors} error{}",
                    if errors == 1 { "" } else { "s" }
                ))
            } else if tokio::time::Instant::now() >= deadline {
                Some(format!(
                    "rebuild did not finish within {}",
                    crate::state::fmt_dur(node.ready_timeout)
                ))
            } else {
                None
            };
            if let Some(why) = failure {
                engine.log(name, format!("reload failed: {why}"));
                engine.set(name, Status::Unhealthy);
                set_reason(engine, name, why);
                self.phase = Phase::Unready;
            }
        }
    }
}

/// Keeps checking a running service, because readiness stops looking once it
/// first passes (cairn 0092).
///
/// A persistent interval rather than a sleep in the `select!`: every other
/// branch firing would restart a sleep, and a self-reloading node polls every
/// few milliseconds, so the check would never come round.
struct Liveness {
    enabled: bool,
    tick: tokio::time::Interval,
    misses: u32,
    /// Set when this check took the node out of ready, so it only puts back
    /// what it took away — a failed self-reload is not liveness's to clear.
    took_down: bool,
}

impl Liveness {
    fn new(node: &Node) -> Liveness {
        let every = node
            .liveness
            .as_ref()
            .map(|l| l.every)
            .unwrap_or(Duration::from_secs(3600));
        let mut tick = tokio::time::interval_at(tokio::time::Instant::now() + every, every);
        // A probe that runs long must not be followed by a burst of catch-up checks.
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        Liveness {
            enabled: node.liveness.is_some(),
            tick,
            misses: 0,
            took_down: false,
        }
    }

    /// One check. `Some(reason)` asks the supervisor to restart the node.
    async fn check(&mut self, engine: &Engine, node: &Node) -> Option<String> {
        let l = node.liveness.as_ref()?;
        let name = node.name.as_str();
        // A process rebuilding itself is not expected to answer meanwhile.
        let rebuilding = engine
            .state
            .lock()
            .unwrap()
            .nodes
            .get(name)
            .is_some_and(|n| matches!(n.status, Status::Reloading));
        if rebuilding {
            return None;
        }
        match engine.probes_pass(node, &l.probe).await {
            Ok(()) => {
                self.misses = 0;
                if self.took_down {
                    self.took_down = false;
                    engine.log(name, "answering again".into());
                    engine.set(name, Status::Healthy);
                    set_reason(engine, name, "answering again".into());
                }
                None
            }
            Err(why) => {
                self.misses += 1;
                if self.misses != l.failures {
                    return None;
                }
                let why = format!("not answering ({} checks in a row): {why}", self.misses);
                engine.log(name, why.clone());
                if node.restart == crate::config::RestartPolicy::Always {
                    return Some(why);
                }
                self.took_down = true;
                engine.set(name, Status::Unhealthy);
                set_reason(engine, name, why);
                None
            }
        }
    }
}

fn set_reason(engine: &Engine, name: &str, reason: String) {
    if let Some(n) = engine.state.lock().unwrap().nodes.get_mut(name) {
        n.reason = Some(reason);
    }
}

/// Waits for every dependency to be ready. `false` means shut down instead.
///
/// The control queue is watched throughout. Without that, a node waiting on a
/// dependency that will never become ready — because it failed — never sees
/// `Stop`, and shutdown burns that node's whole stop timeout waiting for a task
/// that is not listening. Same shape as the readiness probe in 0020.
async fn wait_for_deps(
    engine: &Engine,
    node: &Node,
    board: &mut watch::Receiver<u64>,
    ctl: &mut ctl::Receiver,
) -> bool {
    loop {
        // Marked seen before reading, so a change made after the read — our
        // own `Waiting` write included — wakes the wait below instead of
        // slipping between the two.
        board.borrow_and_update();
        let Some(dep) = engine.first_unready(&node.depends_on) else {
            return true;
        };
        engine.set(&node.name, Status::Waiting(dep));
        tokio::select! {
            changed = board.changed() => {
                if changed.is_err() {
                    return false;
                }
            }
            msg = ctl.recv() => match msg {
                Some(Ctl::Stop) | None => return false,
                // A restart while not running is noise: starting is what
                // this is waiting to do anyway.
                Some(_) => {}
            },
        }
    }
}

/// Parks until there is genuinely something new to do. `None` means shut down.
///
/// A dependency is acted on only when its generation differs from what this
/// node last consumed, so a node that already ran against a result is not run
/// against it again — which is what a message arriving a moment too late to be
/// drained used to cause.
async fn wait_for_rerun(
    engine: &Engine,
    deps: &[String],
    seen: &[u64],
    board: &mut watch::Receiver<u64>,
    ctl: &mut ctl::Receiver,
) -> Option<String> {
    loop {
        board.borrow_and_update();
        if let Some(d) = moved_dependency(engine, deps, seen) {
            return Some(format!("dependency `{d}` changed"));
        }
        tokio::select! {
            changed = board.changed() => {
                if changed.is_err() {
                    return None;
                }
            }
            msg = ctl.recv() => match msg {
                Some(Ctl::Restart(r)) => return Some(r),
                Some(Ctl::Stop) | None => return None,
            },
        }
    }
}

/// The first dependency whose generation has moved past `seen`.
fn moved_dependency(engine: &Engine, deps: &[String], seen: &[u64]) -> Option<String> {
    let now = engine.generations(deps);
    deps.iter()
        .zip(now.iter().zip(seen))
        .find(|(_, (now, then))| now != then)
        .map(|(d, _)| d.clone())
}

/// Why a running node should stop for its dependencies, if it should.
///
/// Two conditions, both read from the board rather than from messages: a
/// dependency that is not ready (it is rebuilding, or gone), or one whose
/// generation has moved past what this process started against — a result
/// it never ran on, which a fast enough rebuild produces without ever
/// visibly leaving ready (cairn 0101).
fn dependency_moved(engine: &Engine, node: &Node, seen: &[u64]) -> Option<String> {
    if let Some(d) = engine.first_unready(&node.depends_on) {
        return Some(format!("dependency `{d}` is rebuilding"));
    }
    moved_dependency(engine, &node.depends_on, seen)
        .map(|d| format!("dependency `{d}` produced a new result"))
}

/// Outcome of waiting for a node to become ready.
enum Ready {
    Yes,
    /// The probe never passed, or the child died first.
    No,
    /// Shutdown began.
    Stop,
    /// Something outranks finishing startup: a change, or a dependency.
    Interrupted(String),
}

/// Probes until ready, giving up early if the child dies first.
///
/// Every declared probe must pass. The log half is checked here rather than in
/// `health`, because only the engine can see a node's output.
///
/// The control queue is watched throughout. Without that, a stop issued during a
/// cold build was not seen until the readiness probe timed out — so ctrl-c during
/// `cargo build` appeared to hang for the full `ready_timeout`.
async fn probe_until(
    engine: &Engine,
    node: &Node,
    handle: &Handle,
    ctl: &mut ctl::Receiver,
    board: &mut watch::Receiver<u64>,
    seen: &[u64],
) -> Ready {
    let Some(h) = node.health.as_ref() else {
        return Ready::Yes;
    };
    let name = node.name.as_str();
    let interval = crate::config::parse_duration(&h.interval).unwrap_or(Duration::from_millis(300));
    let deadline = tokio::time::Instant::now() + node.ready_timeout;
    // Why the last attempt failed, said once if the probe gives up: "never
    // passed" alone sends someone to guess which half of the probe it was.
    let mut last_failure: Option<String>;
    loop {
        if handle.try_exit_code().is_some() {
            return Ready::No;
        }
        let log_ok = h.log.is_none()
            || engine
                .state
                .lock()
                .unwrap()
                .nodes
                .get(name)
                .map(|n| n.log_ready)
                .unwrap_or(false);
        if log_ok {
            match engine.probes_pass(node, h).await {
                Ok(()) => return Ready::Yes,
                Err(why) => last_failure = Some(why),
            }
        } else {
            last_failure = h.log.as_ref().map(|l| format!("\"{l}\" not printed yet"));
        }
        if tokio::time::Instant::now() >= deadline {
            if let Some(why) = last_failure {
                engine.log(name, format!("last readiness attempt: {why}"));
            }
            return Ready::No;
        }

        // Sleep, but stay answerable. `queue` is the policy that declines to be
        // interrupted here: it exists so a cold build is not killed halfway and
        // then started again from scratch.
        let pause = tokio::time::sleep(interval);
        tokio::pin!(pause);
        loop {
            tokio::select! {
                _ = &mut pause => break,
                changed = board.changed() => {
                    if changed.is_err() {
                        return Ready::Stop;
                    }
                    if let Some(why) = dependency_moved(engine, node, seen) {
                        return Ready::Interrupted(why);
                    }
                }
                msg = ctl.recv() => match msg {
                    Some(Ctl::Stop) | None => return Ready::Stop,
                    Some(Ctl::Restart(r))
                        if node.on_change == OnChange::Restart && node.reload == Reload::Restart =>
                    {
                        return Ready::Interrupted(r);
                    }
                    Some(_) => {}
                },
            }
        }
    }
}

/// Drains process events into the shared state. Single consumer, so the log
/// store needs no locking discipline beyond the state mutex.
pub async fn pump_events(
    state: Arc<Mutex<AppState>>,
    rx: mpsc::UnboundedReceiver<ProcEvent>,
    echo: bool,
) {
    let reporter = echo.then(|| {
        crate::output::Reporter::new(
            crate::output::Verbosity::Full,
            crate::output::Order::Stream,
            true,
            None,
        )
        .expect("a reporter with no log file cannot fail")
    });
    pump_events_with(state, rx, reporter).await
}

/// Drains process events, reporting through `reporter` when there is one.
pub async fn pump_events_with(
    state: Arc<Mutex<AppState>>,
    mut rx: mpsc::UnboundedReceiver<ProcEvent>,
    mut reporter: Option<crate::output::Reporter>,
) {
    while let Some(ev) = rx.recv().await {
        match ev {
            ProcEvent::Line(line) => {
                let plain = proc::strip_ansi(&line.text);
                {
                    let mut st = state.lock().unwrap();
                    // Cargo's package-cache lock is a global mutex across the
                    // workspace. Without surfacing it, a blocked build is
                    // indistinguishable from a hang.
                    if let Some(n) = st.nodes.get_mut(&line.proc) {
                        if plain.contains("Blocking waiting for file lock") {
                            n.blocked_on_cargo_lock = true;
                        } else if plain.contains("Compiling") || plain.contains("Finished") {
                            n.blocked_on_cargo_lock = false;
                        }
                        // cargo re-announces "Compiling" at the top of each attempt,
                        // which is the earliest reliable signal that previous
                        // diagnostics are stale.
                        if plain.trim_start().starts_with("Compiling")
                            || plain.trim_start().starts_with("Checking")
                        {
                            n.diag.reset();
                        }
                        // A self-managed rebuild starting. Checked before readiness,
                        // so a line that is both reads as a rebuild that finished.
                        if let Some(pattern) = &n.reload_pattern
                            && plain.contains(pattern.as_str())
                        {
                            n.reload_matches += 1;
                            n.ready_matches_at_reload = n.ready_matches;
                            // Here rather than when the supervisor next looks: the
                            // errors this rebuild prints may already be on their
                            // way, and resetting later would throw them away.
                            n.diag.reset();
                        }
                        // Readiness by announcement: matched on stripped text, so
                        // a colourised "Listening on ..." still counts.
                        if let Some(pattern) = &n.ready_pattern
                            && plain.contains(pattern.as_str())
                        {
                            n.log_ready = true;
                            n.ready_matches += 1;
                        }
                        n.diag.push_line(&line.text);
                    }
                    st.logs.push(line.clone());
                }
                if let Some(r) = reporter.as_mut() {
                    r.line(&line);
                }
            }
            ProcEvent::Started { proc, pid } => {
                let mut st = state.lock().unwrap();
                if let Some(n) = st.nodes.get_mut(&proc) {
                    n.pid = pid;
                }
            }
            // A node's outcome is what decides whether its buffered output is
            // worth showing, so the reporter is told here rather than guessing.
            ProcEvent::Exited { proc, code } => {
                if let Some(r) = reporter.as_mut() {
                    r.mark_ran(&proc);
                    r.finish(&proc, code != 0);
                }
            }
            ProcEvent::Failed { proc, .. } => {
                if let Some(r) = reporter.as_mut() {
                    r.finish(&proc, true);
                }
            }
        }
    }
}

/// Stops every node in reverse dependency order and waits for each to finish.
///
/// Awaits the supervisor tasks rather than polling shared state for a status that
/// looks final. The bound comes from each node's own `stop_timeout` plus slack for
/// reaping, instead of one global constant that silently truncated a slow stop.
pub async fn shutdown(
    engine: &Engine,
    wires: &BTreeMap<String, Wire>,
    mut supervisors: Supervisors,
) {
    engine.state.lock().unwrap().shutting_down = true;

    for name in engine.plan.order.iter().rev() {
        let Some(w) = wires.get(name) else { continue };
        w.ctl_tx.send(Ctl::Stop);

        let Some(handle) = supervisors.remove(name) else {
            continue;
        };
        let stop_timeout = engine
            .plan
            .get(name)
            .map(|n| n.stop_timeout)
            .unwrap_or(Duration::from_secs(8));
        // A task mid-run is waiting on its child, not reading its mail. Ending
        // the child is what lets its supervisor see the Stop at all; without
        // this the wait below timed out and the task was left running.
        if engine.plan.get(name).is_ok_and(|n| n.kind == Kind::Task)
            && let Some(child) = engine.handle_for(name)
        {
            let _ = child.stop(stop_timeout).await;
        }
        let grace = stop_timeout + Duration::from_secs(2);
        if tokio::time::timeout(grace, handle).await.is_err() {
            engine.log(
                name,
                "supervisor did not exit within its stop timeout".into(),
            );
        }
    }
}

/// Supervisor tasks, taken by `shutdown` so each can be awaited by name.
pub type Supervisors = std::collections::HashMap<String, tokio::task::JoinHandle<()>>;

/// Routes filesystem changes to the nodes that care about them.
///
/// Lives here rather than in the CLI so it can be driven by tests: dispatch is
/// where "one edit, one restart" is decided, and that is exactly the behaviour
/// that needs covering.
pub fn spawn_watch_dispatch(
    mut watcher: crate::watch::Watcher,
    engine: Arc<Engine>,
    plan: Arc<Plan>,
    wires: Arc<BTreeMap<String, Wire>>,
) -> Result<()> {
    let mut matchers: Vec<(String, crate::cache::Matcher)> = Vec::new();
    for name in &plan.order {
        let node = plan.get(name)?;
        let m = if node.kind == Kind::Task {
            plan.input_matcher(node)?
        } else {
            plan.watch_matcher(node)?
        };
        if !m.is_empty() {
            matchers.push((name.clone(), m));
        }
    }
    if matchers.is_empty() {
        return Ok(());
    }

    let mut warnings = watcher.take_warnings();
    tokio::spawn(async move {
        let mut said: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        // `watcher` must be moved whole: it carries the guard that keeps the OS
        // watch alive. `Watcher::recv` exists to make that unavoidable.
        loop {
            let paths = tokio::select! {
                paths = watcher.recv() => match paths {
                    Some(paths) => paths,
                    None => return,
                },
                // A watch that stopped covering part of the tree, said where both
                // the dashboard and `--no-tui` show it — once per message, since
                // the limit error repeats for every directory that cannot be added.
                Some(warning) = warnings.recv() => {
                    if said.insert(warning.clone()) {
                        engine.log("watch", format!("warning: {warning}"));
                    }
                    continue;
                }
            };
            if engine.state.lock().unwrap().shutting_down {
                return;
            }
            // Collect every node that matched, then signal only the upstream-most
            // ones; dependents come back via the readiness cascade rather than
            // being restarted a second time on their own account.
            let mut hits: Vec<String> = Vec::new();
            let mut reasons: BTreeMap<String, String> = BTreeMap::new();
            for (name, matcher) in &matchers {
                let files = crate::watch::matching(&paths, matcher);
                let Some(first) = files.first() else { continue };
                engine.note_changed(name, files.iter().cloned());
                reasons.insert(
                    name.clone(),
                    if files.len() == 1 {
                        first.clone()
                    } else {
                        format!("{first} (+{} more)", files.len() - 1)
                    },
                );
                hits.push(name.clone());
            }
            // A change touching only stylesheets can be swapped in place. Anything
            // else — Rust, HTML, wasm — needs the page rebuilt.
            let css_only = paths.iter().all(|p| {
                p.rsplit('.')
                    .next()
                    .is_some_and(|e| e.eq_ignore_ascii_case("css"))
            });
            if css_only {
                let _ = engine.reload_tx.send(RELOAD_CSS.to_string());
            }

            for name in plan.dispatch_roots(&hits) {
                let reason = reasons.get(&name).cloned().unwrap_or_default();
                if let Some(wire) = wires.get(&name) {
                    wire.ctl_tx.send(Ctl::Restart(reason));
                }
            }
        }
    });
    Ok(())
}

#[cfg(test)]
mod env_tests {
    use super::*;
    use crate::config::{Config, Workspace};
    use std::sync::Mutex;

    /// Process environment is global, and these tests write to it. Cargo runs
    /// them on threads of one process, so they must not overlap.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn engine_for(src: &str, name: &str) -> Arc<Engine> {
        let root = std::env::temp_dir().join(format!("turborust-envt-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let ws = Workspace {
            root,
            config: toml::from_str::<Config>(src).unwrap(),
            source: Some(src.to_string()),
        };
        let plan = Arc::new(crate::plan::resolve(&ws, &[name.to_string()]).unwrap());
        let cache = Arc::new(Cache::new(&ws.cache_dir()).unwrap());
        Engine::new(plan, cache).0
    }

    fn cfg(declared: &str, passed: &str) -> String {
        format!(
            "[tasks.build]\ncmd = \"true\"\ninputs = [\"x\"]\nenv_keys = [\"{declared}\"]\npass_through_env = [\"{passed}\"]"
        )
    }

    #[test]
    fn strict_is_the_default() {
        let e = engine_for(&cfg("A_D", "A_P"), "build");
        assert_eq!(e.plan.get("build").unwrap().env_mode, EnvMode::Strict);
    }

    #[test]
    fn a_strict_child_never_sees_an_undeclared_variable() {
        let _g = ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::set_var("TR_T1_DECLARED", "yes");
            std::env::set_var("TR_T1_PASSED", "yes");
            std::env::set_var("TR_T1_SECRET", "leak");
        }
        let e = engine_for(&cfg("TR_T1_DECLARED", "TR_T1_PASSED"), "build");
        let env = e.child_env(e.plan.get("build").unwrap()).unwrap();

        assert_eq!(env.get("TR_T1_DECLARED").map(String::as_str), Some("yes"));
        assert_eq!(env.get("TR_T1_PASSED").map(String::as_str), Some("yes"));
        assert!(
            !env.contains_key("TR_T1_SECRET"),
            "an undeclared variable reached a strict child: {env:?}"
        );
        // Without PATH nothing runs at all, so the safe base must survive.
        assert!(env.contains_key("PATH"), "safe base missing from {env:?}");
    }

    /// An engine over its own root, with files written there before the plan
    /// is resolved — the plan loads env files, so they must exist first.
    fn engine_in(tag: &str, files: &[(&str, &str)], src: &str, name: &str) -> Arc<Engine> {
        let root = std::env::temp_dir().join(format!("turborust-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        for (path, text) in files {
            std::fs::write(root.join(path), text).unwrap();
        }
        let ws = Workspace {
            root,
            config: toml::from_str::<Config>(src).unwrap(),
            source: Some(src.to_string()),
        };
        let plan = Arc::new(crate::plan::resolve(&ws, &[name.to_string()]).unwrap());
        let cache = Arc::new(Cache::new(&ws.cache_dir()).unwrap());
        Engine::new(plan, cache).0
    }

    #[test]
    fn env_files_reach_a_strict_child_in_order_and_inline_env_wins() {
        let src = r#"
            [project]
            env_file = ".env"
            [tasks.build]
            cmd = "true"
            inputs = ["x"]
            env_file = [".env.build", { path = ".env.local", required = false }]
            env = { INLINE = "inline" }
        "#;
        let e = engine_in(
            "envfile-order",
            &[
                (".env", "SHARED=project\nPROJECT=1\nINLINE=file\n"),
                (".env.build", "SHARED=node\n"),
            ],
            src,
            "build",
        );
        let env = e.child_env(e.plan.get("build").unwrap()).unwrap();
        assert_eq!(env["PROJECT"], "1");
        assert_eq!(
            env["SHARED"], "node",
            "the node's file must override the project's"
        );
        assert_eq!(
            env["INLINE"], "inline",
            "inline env must override every file"
        );
    }

    #[test]
    fn a_changed_env_file_value_is_a_cache_miss_and_is_never_stored_in_the_clear() {
        let src = "[tasks.build]\ncmd = \"true\"\ninputs = [\"x\"]\nenv_file = \".env\"";
        let e = engine_in(
            "envfile-key",
            &[(".env", "TOKEN=hunter2\n"), ("x", "")],
            src,
            "build",
        );
        let node = e.plan.get("build").unwrap().clone();
        let before = e.fingerprint(&node).unwrap();
        assert!(
            !before.meta.values().any(|v| v.contains("hunter2")),
            "an env file value was written into the key's metadata: {:?}",
            before.meta
        );

        std::fs::write(e.plan.root.join(".env"), "TOKEN=hunter3\n").unwrap();
        let after = e.fingerprint(&node).unwrap();
        assert_ne!(
            before.hash, after.hash,
            "a changed env file value must change the key"
        );
        assert!(after.meta.contains_key("env_file:TOKEN"));
    }

    #[test]
    fn the_changed_paths_never_reach_a_cache_key() {
        // Cover for cairn 0098: TURBORUST_CHANGED says why a task ran, not what
        // it ran on, so the same inputs must give the same key either way.
        let src =
            "[tasks.build]\ncmd = \"true\"\ninputs = [\"x\"]\nenv_keys = [\"TURBORUST_CHANGED\"]";
        let e = engine_in("changed-key", &[("x", "")], src, "build");
        let node = e.plan.get("build").unwrap().clone();
        let before = e.fingerprint(&node).unwrap();
        e.note_changed("build", ["x".to_string()]);
        assert_eq!(e.fingerprint(&node).unwrap().hash, before.hash);
        let env = e.spawn_env(&node, e.take_changed("build")).unwrap();
        assert_eq!(env["TURBORUST_CHANGED"], "x");
        assert!(
            e.take_changed("build").is_empty(),
            "taking the paths forgets them"
        );
    }

    #[test]
    fn a_missing_required_env_file_is_refused_with_its_node_and_path() {
        let src = "[tasks.build]\ncmd = \"true\"\nenv_file = \".env.missing\"";
        let ws = Workspace {
            root: std::env::temp_dir().join(format!("turborust-envmiss-{}", std::process::id())),
            config: toml::from_str::<Config>(src).unwrap(),
            source: Some(src.to_string()),
        };
        let Err(err) = crate::plan::resolve(&ws, &["build".to_string()]) else {
            panic!("a missing required env file was accepted");
        };
        let msg = format!("{err:#}");
        assert!(
            msg.contains("`build`") && msg.contains(".env.missing"),
            "{msg}"
        );
    }

    #[test]
    fn a_loose_child_inherits_and_is_therefore_uncacheable() {
        let src = "[tasks.build]\ncmd = \"true\"\ninputs = [\"x\"]\nenv_mode = \"loose\"";
        let e = engine_for(src, "build");
        let n = e.plan.get("build").unwrap();
        assert_eq!(n.env_mode, EnvMode::Loose);
        // Loose passes nothing explicit: the child inherits because we never clear.
        assert!(e.child_env(n).unwrap().is_empty());
    }

    #[test]
    fn declared_env_values_participate_in_the_key() {
        let _g = ENV_LOCK.lock().unwrap();
        let e = engine_for(&cfg("TR_T2_DECLARED", "TR_T2_PASSED"), "build");
        let n = e.plan.get("build").unwrap();
        unsafe { std::env::set_var("TR_T2_DECLARED", "one") };
        let a = e.fingerprint(n).unwrap();
        unsafe { std::env::set_var("TR_T2_DECLARED", "two") };
        let b = e.fingerprint(n).unwrap();
        assert_ne!(a.hash, b.hash, "a declared variable must key the task");
    }

    #[test]
    fn pass_through_values_do_not_participate_in_the_key() {
        let _g = ENV_LOCK.lock().unwrap();
        let e = engine_for(&cfg("TR_T3_DECLARED", "TR_T3_PASSED"), "build");
        let n = e.plan.get("build").unwrap();
        unsafe { std::env::set_var("TR_T3_PASSED", "one") };
        let a = e.fingerprint(n).unwrap();
        unsafe { std::env::set_var("TR_T3_PASSED", "two") };
        let b = e.fingerprint(n).unwrap();
        assert_eq!(
            a.hash, b.hash,
            "pass_through_env is the explicit escape hatch"
        );
    }

    #[test]
    fn changing_env_mode_changes_the_key() {
        let strict = engine_for(&cfg("TR_T4_D", "TR_T4_P"), "build");
        let loose = engine_for(
            &format!("{}\nenv_mode = \"loose\"", cfg("TR_T4_D", "TR_T4_P")),
            "build",
        );
        assert_ne!(
            strict
                .fingerprint(strict.plan.get("build").unwrap())
                .unwrap()
                .hash,
            loose
                .fingerprint(loose.plan.get("build").unwrap())
                .unwrap()
                .hash
        );
    }

    #[test]
    fn changing_declared_outputs_changes_the_key() {
        let a = engine_for(
            "[tasks.b]\ncmd = \"true\"\ninputs = [\"x\"]\noutputs = [\"one\"]",
            "b",
        );
        let c = engine_for(
            "[tasks.b]\ncmd = \"true\"\ninputs = [\"x\"]\noutputs = [\"one\", \"two\"]",
            "b",
        );
        assert_ne!(
            a.fingerprint(a.plan.get("b").unwrap()).unwrap().hash,
            c.fingerprint(c.plan.get("b").unwrap()).unwrap().hash,
            "outputs must participate in the key"
        );
    }
}
