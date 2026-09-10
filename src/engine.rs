//! The supervisor.
//!
//! One tokio task per node, connected by `watch` channels carrying readiness.
//! A node starts only once every dependency reports ready, and *stops* when any
//! dependency stops reporting ready. That single rule gives ordered startup,
//! ordered shutdown, and cascading restarts for free: rebuild a shared crate, its
//! task goes not-ready, every consumer stops, the task finishes, everyone comes
//! back in dependency order.

use crate::cache::{self, Cache, Fingerprint, Record};
use crate::config::{EnvMode, OnChange, SAFE_BASE_ENV, TargetDir};
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
            notifier: Mutex::new(crate::notify::Notifier::new(plan_notify)),
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

    /// The running child for `name`, if there is one.
    pub fn handle_for(&self, name: &str) -> Option<Arc<Handle>> {
        self.handles.lock().unwrap().get(name).cloned()
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

    fn set(&self, name: &str, status: Status) {
        let mut st = self.state.lock().unwrap();
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
            Status::Failed(_) => Some(true),
            Status::Healthy | Status::Done { .. } => Some(false),
            _ => None,
        };
        st.set(name, status);
        drop(st);
        if let Some(failing) = health
            && let Some(notice) = self.notifier.lock().unwrap().observe(name, failing)
        {
            crate::notify::show(&notice);
        }
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
        for k in &node.env_keys {
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
    fn child_env(&self, node: &Node) -> BTreeMap<String, String> {
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
        // Inline `env` is explicit and always wins.
        for (k, v) in &node.env {
            env.insert(k.clone(), v.clone());
        }
        env
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
            &self.child_env(node),
            node.env_mode == EnvMode::Strict,
            self.events_tx.clone(),
        )?;
        if wait_exit(&handle).await != 0 {
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
            &self.child_env(&node),
            node.env_mode == EnvMode::Strict,
            self.events_tx.clone(),
        )?;
        let code = wait_exit(&handle).await;
        let duration = start.elapsed();

        if code == 0 {
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
    let code = if keep_going {
        await_all_tasks(&engine, &tasks).await
    } else {
        await_tasks(&engine, &tasks).await
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
                    && matches!(node.status, Status::Failed(_) | Status::Backoff(_)))
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

async fn wait_exit(handle: &Handle) -> i32 {
    loop {
        if let Some(code) = handle.try_exit_code() {
            return code;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Wiring for one supervised node.
pub struct Wire {
    pub ctl_tx: ctl::Sender,
    pub ready_rx: watch::Receiver<bool>,
}

/// Starts a supervisor task per node and returns their control handles.
pub fn spawn_supervisors(engine: Arc<Engine>) -> (BTreeMap<String, Wire>, Supervisors) {
    let order = engine.plan.order.clone();

    let mut ctl: BTreeMap<String, (ctl::Sender, Option<ctl::Receiver>)> = BTreeMap::new();
    let mut ready: BTreeMap<String, (watch::Sender<bool>, watch::Receiver<bool>)> = BTreeMap::new();
    for name in &order {
        let (tx, rx) = ctl::channel();
        ctl.insert(name.clone(), (tx, Some(rx)));
        let (rtx, rrx) = watch::channel(false);
        ready.insert(name.clone(), (rtx, rrx));
    }

    let mut wires = BTreeMap::new();
    let mut joins: Supervisors = Supervisors::new();
    let mut helpers: Vec<tokio::task::JoinHandle<()>> = Vec::new();

    for name in &order {
        let node = engine.plan.get(name).expect("node in order").clone();
        let (ctl_tx, ctl_rx) = ctl
            .get_mut(name)
            .map(|(t, r)| (t.clone(), r.take().unwrap()))
            .unwrap();
        let (ready_tx, ready_rx) = ready.get(name).cloned_pair();

        // A watcher per dependency turns readiness edges into control messages,
        // so the main loop only ever selects on two things: the child and the mail.
        for dep in &node.depends_on {
            let mut dep_rx = ready.get(dep).unwrap().1.clone();
            let ctl_tx = ctl_tx.clone();
            let dep = dep.clone();
            helpers.push(tokio::spawn(async move {
                while dep_rx.changed().await.is_ok() {
                    let up = *dep_rx.borrow();
                    let msg = if up {
                        Ctl::DepUp(dep.clone())
                    } else {
                        Ctl::DepDown(dep.clone())
                    };
                    if !ctl_tx.send(msg) {
                        break;
                    }
                }
            }));
        }

        let dep_readers: Vec<(String, watch::Receiver<bool>)> = node
            .depends_on
            .iter()
            .map(|d| (d.clone(), ready.get(d).unwrap().1.clone()))
            .collect();

        wires.insert(name.clone(), Wire { ctl_tx, ready_rx });

        let engine = engine.clone();
        joins.insert(
            name.clone(),
            tokio::spawn(async move {
                supervise(engine, node, ctl_rx, ready_tx, dep_readers).await;
            }),
        );
    }

    // Dependency monitors are detached: dropping a JoinHandle leaves the task
    // running, and they end on their own when the readiness channels close.
    drop(helpers);
    (wires, joins)
}

trait ClonedPair {
    fn cloned_pair(&self) -> (watch::Sender<bool>, watch::Receiver<bool>);
}
impl ClonedPair for Option<&(watch::Sender<bool>, watch::Receiver<bool>)> {
    fn cloned_pair(&self) -> (watch::Sender<bool>, watch::Receiver<bool>) {
        let (t, r) = self.expect("channel exists");
        (t.clone(), r.clone())
    }
}

async fn supervise(
    engine: Arc<Engine>,
    node: Node,
    mut ctl: ctl::Receiver,
    ready: watch::Sender<bool>,
    mut deps: Vec<(String, watch::Receiver<bool>)>,
) {
    let name = node.name.clone();
    let mut backoff = node.backoff_min;

    loop {
        // 1. Block until every dependency is ready — while staying answerable.
        if !wait_for_deps(&engine, &name, &mut deps, &mut ctl).await {
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
            ready.send_replace(false);
            let outcome = match engine.run_task(&name, false).await {
                Ok(o) => o,
                Err(e) => {
                    engine.log(&name, format!("error: {e}"));
                    engine.set(&name, Status::Failed(1));
                    TaskOutcome {
                        cached: false,
                        code: 1,
                        duration: Duration::ZERO,
                        fingerprint: None,
                    }
                }
            };
            ready.send_replace(outcome.code == 0);
            if outcome.code == 0 && !outcome.cached {
                let _ = engine.reload_tx.send(RELOAD_FULL.to_string());
            }
            // A task stays put until something asks it to run again.
            match wait_for_rerun(&engine, &node.depends_on, &seen, &mut ctl).await {
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
            ready.send_replace(true);
            match wait_for_rerun(&engine, &node.depends_on, &seen, &mut ctl).await {
                Some(_) => continue,
                None => return,
            }
        };

        // A service holds a slot only while starting. Releasing it at `Healthy`
        // rather than at exit is what keeps a graph wider than `concurrency`
        // from deadlocking.
        let slot = engine.acquire_slot(&name).await;
        engine.set(&name, Status::Starting);
        let child_env = engine.child_env(&node);
        let handle = match proc::spawn_with(
            &name,
            &cmd,
            &node.cwd,
            &child_env,
            node.env_mode == EnvMode::Strict,
            engine.events_tx.clone(),
        ) {
            Ok(h) => h,
            Err(e) => {
                engine.log(&name, format!("spawn failed: {e}"));
                engine.set(&name, Status::Failed(127));
                if node.restart == crate::config::RestartPolicy::Never {
                    return;
                }
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(node.backoff_max);
                continue;
            }
        };
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
            Some(_) => match probe_until(&engine, &node, &handle, &mut ctl).await {
                Ready::Yes => {
                    engine.set(&name, Status::Healthy);
                    backoff = node.backoff_min;
                    true
                }
                Ready::No => {
                    engine.log(&name, "readiness probe never passed".into());
                    false
                }
                Ready::Interrupted(Ctl::Stop) => {
                    engine.set(&name, Status::Stopped);
                    ready.send_replace(false);
                    engine.forget_handle(&name);
                    let _ = handle.stop(node.stop_timeout).await;
                    return;
                }
                Ready::Interrupted(other) => {
                    let reason = match other {
                        Ctl::Restart(r) => r,
                        Ctl::DepDown(d) => format!("dependency `{d}` is rebuilding"),
                        _ => "changed".into(),
                    };
                    engine.log(&name, format!("restarting mid-startup: {reason}"));
                    ready.send_replace(false);
                    engine.forget_handle(&name);
                    let _ = handle.stop(node.stop_timeout).await;
                    continue;
                }
            },
        };
        drop(slot);
        ready.send_replace(ready_flag);
        if ready_flag {
            let _ = engine.reload_tx.send(RELOAD_FULL.to_string());
        }

        // 5. Run until the child exits or the mail says otherwise.
        let stop_reason = loop {
            tokio::select! {
                code = wait_exit(&handle) => break StopReason::Exited(code),
                msg = ctl.recv() => match msg {
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
                    Some(Ctl::DepDown(d)) => {
                        break StopReason::DepDown(format!("dependency `{d}` is rebuilding"))
                    }
                    // Already running, so a dependency coming back is either the
                    // edge that started us or the tail of a DepDown we have
                    // already acted on. Restarting here would double every
                    // cascade: once on the down edge, once on the up edge.
                    Some(Ctl::DepUp(_)) => continue,
                    Some(Ctl::Stop) | None => break StopReason::Stop,
                },
            }
        };

        ready.send_replace(false);

        match stop_reason {
            StopReason::Stop => {
                engine.set(&name, Status::Stopped);
                engine.forget_handle(&name);
                let _ = handle.stop(node.stop_timeout).await;
                return;
            }
            StopReason::Restart(reason) | StopReason::DepDown(reason) => {
                {
                    let mut st = engine.state.lock().unwrap();
                    if let Some(n) = st.nodes.get_mut(&name) {
                        n.restarts += 1;
                        n.reason = Some(reason.clone());
                    }
                }
                engine.log(&name, format!("restarting: {reason}"));
                engine.forget_handle(&name);
                let _ = handle.stop(node.stop_timeout).await;
                backoff = node.backoff_min;
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
                    match wait_for_rerun(&engine, &node.depends_on, &seen, &mut ctl).await {
                        Some(reason) => {
                            engine.log(&name, format!("restarting: {reason}"));
                            continue;
                        }
                        None => return,
                    }
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
                    }
                    _ => {}
                }
                continue;
            }
        }
    }
}

enum StopReason {
    Exited(i32),
    Restart(String),
    DepDown(String),
    Stop,
}

/// Waits for every dependency to report ready. `false` means shut down instead.
///
/// The control queue is watched throughout. Without that, a node waiting on a
/// dependency that will never become ready — because it failed — never sees
/// `Stop`, and shutdown burns that node's whole stop timeout waiting for a task
/// that is not listening. Same shape as the readiness probe in 0020.
async fn wait_for_deps(
    engine: &Engine,
    name: &str,
    deps: &mut [(String, watch::Receiver<bool>)],
    ctl: &mut ctl::Receiver,
) -> bool {
    for (dep, rx) in deps.iter_mut() {
        loop {
            if *rx.borrow_and_update() {
                break;
            }
            engine.set(name, Status::Waiting(dep.clone()));
            tokio::select! {
                changed = rx.changed() => {
                    if changed.is_err() {
                        return false;
                    }
                }
                msg = ctl.recv() => match msg {
                    Some(Ctl::Stop) | None => return false,
                    // Anything else is noise while we are not running.
                    Some(_) => {}
                },
            }
        }
    }
    true
}

/// Parks until there is genuinely something new to do. `None` means shut down.
///
/// Dependency wake-ups are only acted on when a dependency's generation differs
/// from what this node last consumed. Treating the messages themselves as the
/// trigger meant a message that arrived a moment too late to be drained caused a
/// second, identical run.
async fn wait_for_rerun(
    engine: &Engine,
    deps: &[String],
    seen: &[u64],
    ctl: &mut ctl::Receiver,
) -> Option<String> {
    loop {
        match ctl.recv().await {
            Some(Ctl::Restart(r)) => return Some(r),
            Some(Ctl::DepUp(d)) | Some(Ctl::DepDown(d)) => {
                if engine.generations(deps) != seen {
                    return Some(format!("dependency `{d}` changed"));
                }
            }
            Some(Ctl::Stop) | None => return None,
        }
    }
}

/// Outcome of waiting for a node to become ready.
enum Ready {
    Yes,
    /// The probe never passed, or the child died first.
    No,
    /// A control message arrived that outranks finishing startup.
    Interrupted(Ctl),
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
) -> Ready {
    let Some(h) = node.health.as_ref() else {
        return Ready::Yes;
    };
    let name = node.name.as_str();
    let interval = crate::config::parse_duration(&h.interval).unwrap_or(Duration::from_millis(300));
    let deadline = tokio::time::Instant::now() + node.ready_timeout;
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
        if log_ok && health::probe(h).await {
            return Ready::Yes;
        }
        if tokio::time::Instant::now() >= deadline {
            return Ready::No;
        }

        // Sleep, but stay answerable. `queue` is the policy that declines to be
        // interrupted here: it exists so a cold build is not killed halfway and
        // then started again from scratch.
        tokio::select! {
            _ = tokio::time::sleep(interval) => {}
            msg = ctl.recv() => match msg {
                Some(Ctl::Stop) | None => return Ready::Interrupted(Ctl::Stop),
                Some(Ctl::Restart(r)) if node.on_change == OnChange::Restart => {
                    return Ready::Interrupted(Ctl::Restart(r));
                }
                Some(Ctl::DepDown(d)) => return Ready::Interrupted(Ctl::DepDown(d)),
                Some(_) => {}
            },
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
                        // Readiness by announcement: matched on stripped text, so
                        // a colourised "Listening on ..." still counts.
                        if let Some(pattern) = &n.ready_pattern
                            && plain.contains(pattern.as_str())
                        {
                            n.log_ready = true;
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
        let grace = engine
            .plan
            .get(name)
            .map(|n| n.stop_timeout)
            .unwrap_or(Duration::from_secs(8))
            + Duration::from_secs(2);
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

    tokio::spawn(async move {
        // `watcher` must be moved whole: it carries the guard that keeps the OS
        // watch alive. `Watcher::recv` exists to make that unavoidable.
        while let Some(paths) = watcher.recv().await {
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
        let env = e.child_env(e.plan.get("build").unwrap());

        assert_eq!(env.get("TR_T1_DECLARED").map(String::as_str), Some("yes"));
        assert_eq!(env.get("TR_T1_PASSED").map(String::as_str), Some("yes"));
        assert!(
            !env.contains_key("TR_T1_SECRET"),
            "an undeclared variable reached a strict child: {env:?}"
        );
        // Without PATH nothing runs at all, so the safe base must survive.
        assert!(env.contains_key("PATH"), "safe base missing from {env:?}");
    }

    #[test]
    fn a_loose_child_inherits_and_is_therefore_uncacheable() {
        let src = "[tasks.build]\ncmd = \"true\"\ninputs = [\"x\"]\nenv_mode = \"loose\"";
        let e = engine_for(src, "build");
        let n = e.plan.get("build").unwrap();
        assert_eq!(n.env_mode, EnvMode::Loose);
        // Loose passes nothing explicit: the child inherits because we never clear.
        assert!(e.child_env(n).is_empty());
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
