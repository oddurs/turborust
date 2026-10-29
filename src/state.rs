//! Observable supervisor state, shared between the engine and any UI.
//!
//! The UI never asks the engine questions; it renders this. That keeps the
//! supervisor loop free of rendering concerns and means a headless run and a TUI
//! run execute exactly the same code path.

use crate::diag::{Diagnostic, Parser};
use crate::proc::LogLine;
use serde::Serialize;
use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    /// Declared but not yet reached in the startup order.
    Pending,
    /// Blocked on a dependency; holds the dependency name.
    Waiting(String),
    /// Ready to build, but the concurrency limit is full.
    ///
    /// Distinct from `Waiting` on purpose: a bounded queue that looked like a
    /// stuck dependency would be the same confusion the cargo-lock indicator
    /// exists to prevent.
    Queued,
    /// Process spawned, readiness probe not yet passing.
    Starting,
    /// Probe passing (or no probe declared).
    Healthy,
    /// A `reload = "self"` process announced a rebuild and has not finished it.
    /// Still running — that is the difference from `Starting`, and why entering
    /// this state clears nothing.
    Reloading,
    /// Running, but its readiness probe gave up — at startup, or after a reload
    /// that did not finish. Docker's word for a container in the same state.
    Unhealthy,
    /// A task that is currently executing.
    Running,
    /// A task that finished successfully.
    Done { cached: bool, duration: Duration },
    /// Exited non-zero.
    Failed(i32),
    /// Exited zero, and policy says leave it exited.
    Exited,
    /// Waiting out restart backoff; holds the delay.
    Backoff(Duration),
    /// Deliberately stopped by us.
    Stopped,
    /// Being stopped so it can start again: a restart for a change, or for a
    /// dependency. Its own state rather than the one it is leaving, because
    /// readiness is read from status, and a process on its way down must not
    /// read as ready while its stop timeout runs.
    Stopping,
}

impl Status {
    pub fn label(&self) -> String {
        match self {
            Status::Pending => "pending".into(),
            Status::Waiting(d) => format!("waiting: {d}"),
            Status::Queued => "waiting: slot".into(),
            Status::Starting => "starting".into(),
            Status::Healthy => "healthy".into(),
            Status::Reloading => "reloading".into(),
            Status::Unhealthy => "unhealthy".into(),
            Status::Running => "running".into(),
            Status::Done { cached: true, .. } => "cached".into(),
            Status::Done {
                cached: false,
                duration,
            } => format!("done {}", fmt_dur(*duration)),
            Status::Failed(c) => format!("failed ({c})"),
            Status::Exited => "exited".into(),
            Status::Backoff(d) => format!("retry in {}", fmt_dur(*d)),
            Status::Stopped => "stopped".into(),
            Status::Stopping => "stopping".into(),
        }
    }

    /// Stable machine name for the wire; `label()` is the human string.
    pub fn code(&self) -> &'static str {
        match self {
            Status::Pending => "pending",
            Status::Waiting(_) => "waiting",
            Status::Queued => "queued",
            Status::Starting => "starting",
            Status::Healthy => "healthy",
            Status::Reloading => "reloading",
            Status::Unhealthy => "unhealthy",
            Status::Running => "running",
            Status::Done { .. } => "done",
            Status::Failed(_) => "failed",
            Status::Exited => "exited",
            Status::Backoff(_) => "backoff",
            Status::Stopped => "stopped",
            Status::Stopping => "stopping",
        }
    }

    pub fn is_bad(&self) -> bool {
        matches!(
            self,
            Status::Failed(_) | Status::Backoff(_) | Status::Unhealthy
        )
    }

    /// Whether dependents may run against this node: the one definition of
    /// readiness. The engine publishes it from status alone, so the two cannot
    /// disagree (cairn 0104).
    pub fn is_ready(&self) -> bool {
        self.is_good()
    }

    pub fn is_good(&self) -> bool {
        matches!(self, Status::Healthy | Status::Done { .. })
    }
}

pub fn fmt_dur(d: Duration) -> String {
    let ms = d.as_millis();
    if ms < 1_000 {
        format!("{ms}ms")
    } else if ms < 60_000 {
        format!("{:.1}s", d.as_secs_f64())
    } else {
        format!("{}m{:02}s", d.as_secs() / 60, d.as_secs() % 60)
    }
}

#[derive(Debug, Clone)]
pub struct NodeState {
    /// "service" or "task" — carried here so a snapshot needs no plan lookup.
    pub kind: &'static str,
    pub port: Option<u16>,
    pub status: Status,
    pub pid: Option<u32>,
    pub restarts: u32,
    pub started_at: Option<Instant>,
    /// Human-readable cause of the most recent (re)start.
    pub reason: Option<String>,
    /// Set when the child reported cargo's target-dir lock contention, which
    /// otherwise looks exactly like a hang.
    pub blocked_on_cargo_lock: bool,
    /// Diagnostics scraped from this node's own output.
    pub diag: Parser,
    /// Substring that marks this node ready, from `health = { log = "..." }`.
    pub ready_pattern: Option<String>,
    /// Set once `ready_pattern` has been seen since the node last started.
    pub log_ready: bool,
    /// Substring that marks the start of a self-managed rebuild.
    pub reload_pattern: Option<String>,
    /// Times `ready_pattern` has matched, ever.
    ///
    /// Counters rather than flags, for the reason `generation` is one: a reload
    /// that starts and finishes between two polls must still be seen, and two
    /// flags can only say that both happened, not in which order.
    pub ready_matches: u64,
    /// Times `reload_pattern` has matched, ever.
    pub reload_matches: u64,
    /// `ready_matches` as it stood at the latest reload, so "ready again" means
    /// a match that came after it.
    pub ready_matches_at_reload: u64,
    /// Bumped every time this node completes a run or becomes healthy.
    ///
    /// Dependents compare generations rather than trusting edges: a node that
    /// already ran against a result finds the generation unchanged and is not
    /// run again, and one that missed a fast rebuild finds it moved (0101).
    pub generation: u64,
}

impl Default for NodeState {
    fn default() -> Self {
        NodeState {
            kind: "service",
            port: None,
            status: Status::Pending,
            pid: None,
            restarts: 0,
            started_at: None,
            reason: None,
            blocked_on_cargo_lock: false,
            diag: Parser::new(),
            ready_pattern: None,
            log_ready: false,
            reload_pattern: None,
            ready_matches: 0,
            reload_matches: 0,
            ready_matches_at_reload: 0,
            generation: 0,
        }
    }
}

/// One thing that happened to a node's lifecycle (cairn 0107).
///
/// Output lines are not facts: they already live in their own append-only
/// store, and diagnostics and log readiness are derived from those.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fact {
    Status(Status),
    /// Why it last (re)started, or why it is not running.
    Reason(String),
    /// A restart was counted, for this reason.
    Restarted(String),
    /// A child started, with this pid.
    Spawned(Option<u32>),
    /// The child's pid became known, or changed.
    Pid(Option<u32>),
}

#[derive(Debug, Clone)]
pub struct Entry {
    /// Position in the whole history, from 1, gaps never.
    pub seq: u64,
    pub at: Instant,
    pub node: String,
    pub fact: Fact,
}

impl Entry {
    /// One line, for a test that timed out or a bug report.
    pub fn describe(&self, origin: Instant) -> String {
        let t = self.at.saturating_duration_since(origin);
        let what = match &self.fact {
            Fact::Status(s) => s.label(),
            Fact::Reason(r) => format!("because {r}"),
            Fact::Restarted(r) => format!("restart counted: {r}"),
            Fact::Spawned(pid) => format!("spawned pid {}", fmt_pid(*pid)),
            Fact::Pid(pid) => format!("pid {}", fmt_pid(*pid)),
        };
        format!("#{} +{} {} {what}", self.seq, fmt_dur(t), self.node)
    }
}

fn fmt_pid(pid: Option<u32>) -> String {
    pid.map_or("-".into(), |p| p.to_string())
}

/// What happens to a node when `fact` is recorded: the one function the live
/// state and a replay are both built by, so they cannot drift.
pub fn apply(n: &mut NodeState, fact: &Fact, at: Instant) {
    match fact {
        Fact::Status(status) => {
            // A new attempt invalidates the previous attempt's diagnostics;
            // without this a fixed error stays on screen until something else
            // overwrites it.
            if matches!(status, Status::Running | Status::Starting) {
                n.diag.reset();
                n.log_ready = false;
            }
            // Reaching a usable result is what dependents care about.
            if status.is_ready() {
                n.generation += 1;
            }
            n.status = status.clone();
        }
        Fact::Reason(r) => n.reason = Some(r.clone()),
        Fact::Restarted(r) => {
            n.restarts += 1;
            n.reason = Some(r.clone());
        }
        Fact::Spawned(pid) => {
            n.pid = *pid;
            n.started_at = Some(at);
        }
        Fact::Pid(pid) => n.pid = *pid,
    }
}

/// The lifecycle half of a node, which a projection must reproduce exactly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Life {
    pub status: Status,
    pub reason: Option<String>,
    pub restarts: u32,
    pub pid: Option<u32>,
    pub started_at: Option<Instant>,
    pub generation: u64,
}

/// Every fact ever recorded, oldest first.
///
/// Bounded the way the output store is, but without forgetting: an entry that
/// falls off the front is folded into `base`, so `base` plus the entries kept
/// is always the whole history. A long `up` keeps the recent past in detail
/// and the rest as a summary.
pub struct History {
    base: BTreeMap<String, NodeState>,
    entries: VecDeque<Entry>,
    cap: usize,
    next: u64,
    pub origin: Instant,
}

impl History {
    fn new(seed: &BTreeMap<String, NodeState>, cap: usize) -> History {
        History {
            base: seed.clone(),
            entries: VecDeque::new(),
            cap: cap.max(1),
            next: 1,
            origin: now(),
        }
    }

    fn push(&mut self, node: &str, fact: Fact, at: Instant) {
        if self.entries.len() == self.cap
            && let Some(old) = self.entries.pop_front()
            && let Some(n) = self.base.get_mut(&old.node)
        {
            apply(n, &old.fact, old.at);
        }
        self.entries.push_back(Entry {
            seq: self.next,
            at,
            node: node.into(),
            fact,
        });
        self.next += 1;
    }

    pub fn entries(&self) -> impl Iterator<Item = &Entry> {
        self.entries.iter()
    }

    /// Facts recorded, ever; more than `entries()` holds once it has wrapped.
    pub fn total(&self) -> u64 {
        self.next - 1
    }

    /// The node states this history describes, rebuilt from nothing but it.
    pub fn project(&self) -> BTreeMap<String, Life> {
        let mut nodes = self.base.clone();
        for e in &self.entries {
            if let Some(n) = nodes.get_mut(&e.node) {
                apply(n, &e.fact, e.at);
            }
        }
        nodes.iter().map(|(k, n)| (k.clone(), n.life())).collect()
    }

    /// The kept entries, one per line.
    pub fn describe(&self) -> String {
        let skipped = self.total() - self.entries.len() as u64;
        let mut out = if skipped > 0 {
            format!("({skipped} earlier facts summarised)\n")
        } else {
            String::new()
        };
        for e in &self.entries {
            out.push_str(&e.describe(self.origin));
            out.push('\n');
        }
        out
    }
}

/// tokio's clock, which is the system's except under a paused test clock, where
/// it makes a history read in the simulation's own time.
fn now() -> Instant {
    tokio::time::Instant::now().into_std()
}

/// Facts kept in detail; a busy hour of `up` is a few hundred.
const HISTORY_CAP: usize = 10_000;

impl NodeState {
    pub fn life(&self) -> Life {
        Life {
            status: self.status.clone(),
            reason: self.reason.clone(),
            restarts: self.restarts,
            pid: self.pid,
            started_at: self.started_at,
            generation: self.generation,
        }
    }

    pub fn uptime(&self) -> Option<Duration> {
        self.started_at.map(|t| t.elapsed())
    }
}

/// Bounded ring buffer of output lines.
///
/// `\r`-terminated lines overwrite their predecessor instead of accumulating.
/// Without this a five-minute `cargo build` fills the scrollback with thousands
/// of near-identical progress frames and the actual errors scroll away.
pub struct LogStore {
    lines: VecDeque<LogLine>,
    cap: usize,
    /// Text held, in bytes, against `byte_cap`.
    bytes: usize,
    byte_cap: usize,
    last_was_transient: BTreeMap<Arc<str>, bool>,
}

/// Output text kept in memory, whatever the line count. A count alone bounded
/// nothing: twenty thousand lines of a minified bundle is over a gigabyte.
const LOG_BYTES: usize = 16 * 1024 * 1024;

impl LogStore {
    pub fn new(cap: usize) -> Self {
        Self::with_byte_cap(cap, LOG_BYTES)
    }

    pub fn with_byte_cap(cap: usize, byte_cap: usize) -> Self {
        LogStore {
            lines: VecDeque::with_capacity(cap.min(4096)),
            cap,
            bytes: 0,
            byte_cap,
            last_was_transient: BTreeMap::new(),
        }
    }

    pub fn push(&mut self, line: LogLine) {
        // Looked up before inserting: a key is allocated once per process, not
        // once per line.
        let replace = match self.last_was_transient.get_mut(&*line.proc) {
            Some(last) => std::mem::replace(last, line.transient),
            None => {
                self.last_was_transient
                    .insert(line.proc.clone(), line.transient);
                false
            }
        };
        if replace && let Some(idx) = self.lines.iter().rposition(|l| l.proc == line.proc) {
            self.bytes = self.bytes - self.lines[idx].text.len() + line.text.len();
            self.lines[idx] = line;
        } else {
            self.bytes += line.text.len();
            self.lines.push_back(line);
        }
        // The newest line always stays, so a single line over budget is shown.
        while self.lines.len() > 1 && (self.lines.len() > self.cap || self.bytes > self.byte_cap) {
            if let Some(old) = self.lines.pop_front() {
                self.bytes -= old.text.len();
            }
        }
    }

    /// Bytes of text held.
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    pub fn len(&self) -> usize {
        self.lines.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// All lines, or only those from `filter`.
    pub fn view(&self, filter: Option<&str>) -> Vec<&LogLine> {
        self.lines
            .iter()
            .filter(|l| filter.is_none_or(|f| &*l.proc == f))
            .collect()
    }
}

/// What the last task run did — the overlay's "why did that happen" surface.
#[derive(Debug, Clone, Serialize)]
pub struct BuildInfo {
    pub node: String,
    pub cached: bool,
    pub duration_ms: u64,
    /// Short blake3 cache key.
    pub key: String,
    /// Inputs that differed from the previous run, newest build first.
    pub changed: Vec<ChangedFile>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChangedFile {
    pub path: String,
    pub from: String,
    pub to: String,
}

/// One node, flattened for the wire.
#[derive(Debug, Serialize)]
pub struct NodeSnapshot {
    pub name: String,
    pub kind: &'static str,
    pub status: &'static str,
    pub label: String,
    pub port: Option<u16>,
    pub pid: Option<u32>,
    pub uptime_ms: Option<u64>,
    pub restarts: u32,
    pub reason: Option<String>,
    pub blocked: bool,
    pub errors: usize,
}

/// The whole debug UI's data, in one message.
#[derive(Debug, Serialize)]
pub struct LogSnapshot {
    /// Monotonic line number, so the browser can resume after a reconnect
    /// instead of refetching the whole tail.
    pub seq: u64,
    pub proc: String,
    pub text: String,
}

#[derive(Debug, Serialize)]
pub struct Snapshot {
    /// Aggregate: "error" | "building" | "ready" | "stopped".
    pub status: &'static str,
    pub nodes: Vec<NodeSnapshot>,
    pub issues: Vec<IssueSnapshot>,
    pub build: Option<BuildInfo>,
    /// How many lines exist, so the overlay can show a count without carrying
    /// the lines themselves in every frame.
    pub log_count: usize,
    pub shutting_down: bool,
}

#[derive(Debug, Serialize)]
pub struct IssueSnapshot {
    pub node: String,
    #[serde(flatten)]
    pub diagnostic: Diagnostic,
}

/// Most lines returned by one `logs_since` call. A reconnect after a noisy
/// build should not try to ship the whole buffer in a single frame.
const LOG_PAGE: usize = 500;

pub struct AppState {
    pub nodes: BTreeMap<String, NodeState>,
    pub logs: LogStore,
    /// Node whose logs the UI is showing; `None` means all.
    pub focus: Option<String>,
    pub selected: usize,
    pub scroll: usize,
    /// Set once shutdown begins, so the UI can say so.
    pub shutting_down: bool,
    pub build: Option<BuildInfo>,
    /// What happened, in order. The lifecycle fields of `nodes` are its
    /// projection: they change only through [`AppState::record`].
    pub history: History,
}

impl AppState {
    pub fn new(nodes: &[(String, &'static str, Option<u16>)], log_cap: usize) -> Self {
        Self::with_history(nodes, log_cap, HISTORY_CAP)
    }

    pub fn with_history(
        nodes: &[(String, &'static str, Option<u16>)],
        log_cap: usize,
        history_cap: usize,
    ) -> Self {
        let nodes: BTreeMap<String, NodeState> = nodes
            .iter()
            .map(|(n, kind, port)| {
                (
                    n.clone(),
                    NodeState {
                        kind,
                        port: *port,
                        ..Default::default()
                    },
                )
            })
            .collect();
        AppState {
            history: History::new(&nodes, history_cap),
            nodes,
            logs: LogStore::new(log_cap),
            focus: None,
            selected: 0,
            scroll: 0,
            shutting_down: false,
            build: None,
        }
    }

    /// Everything the browser overlay renders, in one serializable value.
    pub fn snapshot(&self) -> Snapshot {
        let mut issues = Vec::new();
        for (name, node) in &self.nodes {
            for d in node.diag.diagnostics() {
                issues.push(IssueSnapshot {
                    node: name.clone(),
                    diagnostic: d.clone(),
                });
            }
        }
        // Errors first: a warning must never push an error off the top.
        issues.sort_by_key(|i| (i.diagnostic.level != crate::diag::Level::Error) as u8);

        let nodes: Vec<NodeSnapshot> = self
            .nodes
            .iter()
            .map(|(name, n)| NodeSnapshot {
                name: name.clone(),
                kind: n.kind,
                status: n.status.code(),
                label: if n.blocked_on_cargo_lock {
                    "waiting: cargo lock".to_string()
                } else {
                    n.status.label()
                },
                port: n.port,
                pid: n.pid,
                uptime_ms: n.uptime().map(|d| d.as_millis() as u64),
                restarts: n.restarts,
                reason: n.reason.clone(),
                blocked: n.blocked_on_cargo_lock,
                errors: n.diag.errors(),
            })
            .collect();

        // The bubble shows one thing, so the aggregate is a priority order:
        // an error outranks work in progress, which outranks calm.
        let status = if self.shutting_down {
            "stopped"
        } else if issues
            .iter()
            .any(|i| i.diagnostic.level == crate::diag::Level::Error)
            || nodes
                .iter()
                .any(|n| matches!(n.status, "failed" | "unhealthy"))
        {
            "error"
        } else if nodes
            .iter()
            .any(|n| matches!(n.status, "running" | "starting" | "reloading" | "waiting"))
        {
            "building"
        } else {
            "ready"
        };

        Snapshot {
            status,
            nodes,
            issues,
            build: self.build.clone(),
            log_count: self.logs.len(),
            shutting_down: self.shutting_down,
        }
    }

    /// Output lines newer than `after`, oldest first.
    ///
    /// The overlay streams output incrementally rather than receiving the tail in
    /// every state frame: re-sending a few KB every 250ms is wasteful, and
    /// re-rendering the whole pane fights the reader's scroll position.
    pub fn logs_since(&self, after: u64) -> Vec<LogSnapshot> {
        self.logs
            .view(None)
            .into_iter()
            .filter(|l| l.seq > after && l.seq != u64::MAX)
            .take(LOG_PAGE)
            .map(|l| LogSnapshot {
                seq: l.seq,
                proc: l.proc.to_string(),
                // The overlay does its own styling; raw escapes would render as
                // garbage inside a <pre>.
                text: crate::proc::strip_ansi(&l.text).into_owned(),
            })
            .collect()
    }

    pub fn set_ready_pattern(&mut self, name: &str, pattern: Option<String>) {
        if let Some(n) = self.nodes.get_mut(name) {
            n.ready_pattern = pattern;
        }
    }

    pub fn set_reload_pattern(&mut self, name: &str, pattern: Option<String>) {
        if let Some(n) = self.nodes.get_mut(name) {
            n.reload_pattern = pattern;
        }
    }

    /// Appends `fact` to the history and applies it: the only way a node's
    /// lifecycle changes. A fact about a node not in the plan is dropped.
    pub fn record(&mut self, name: &str, fact: Fact) {
        let at = now();
        if let Some(n) = self.nodes.get_mut(name) {
            apply(n, &fact, at);
            self.history.push(name, fact, at);
        }
    }

    /// The lifecycle of every node, as it stands.
    pub fn lives(&self) -> BTreeMap<String, Life> {
        self.nodes
            .iter()
            .map(|(k, n)| (k.clone(), n.life()))
            .collect()
    }

    pub fn names(&self) -> Vec<String> {
        self.nodes.keys().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seeded(cap: usize) -> AppState {
        AppState::with_history(
            &[
                ("api".into(), "service", Some(3000)),
                ("build".into(), "task", None),
            ],
            100,
            cap,
        )
    }

    /// A run's worth of facts, in an order a real run could produce.
    fn churn(st: &mut AppState, rounds: u32) {
        for i in 0..rounds {
            st.record("build", Fact::Status(Status::Running));
            st.record(
                "build",
                Fact::Status(Status::Done {
                    cached: i % 2 == 0,
                    duration: Duration::from_millis(i as u64),
                }),
            );
            st.record("api", Fact::Status(Status::Stopping));
            st.record("api", Fact::Restarted(format!("round {i}")));
            st.record("api", Fact::Status(Status::Starting));
            st.record("api", Fact::Spawned(Some(100 + i)));
            st.record("api", Fact::Pid(Some(200 + i)));
            st.record("api", Fact::Status(Status::Healthy));
            st.record("ghost", Fact::Reason("not in the plan".into()));
        }
    }

    #[test]
    fn the_history_rebuilds_every_lifecycle() {
        let mut st = seeded(10_000);
        churn(&mut st, 50);
        assert_eq!(
            st.history.total(),
            50 * 8,
            "facts about unknown nodes dropped"
        );
        assert_eq!(st.history.project(), st.lives());
        assert_eq!(st.nodes["api"].restarts, 50);
        assert_eq!(st.nodes["api"].generation, 50);
        assert_eq!(st.nodes["build"].generation, 50);
        assert_eq!(st.nodes["api"].pid, Some(249));
    }

    #[test]
    fn a_history_past_its_cap_summarises_without_forgetting() {
        let mut st = seeded(7);
        churn(&mut st, 50);
        assert_eq!(st.history.entries().count(), 7);
        assert_eq!(st.history.total(), 400);
        assert_eq!(st.history.project(), st.lives());
        let seqs: Vec<u64> = st.history.entries().map(|e| e.seq).collect();
        assert_eq!(seqs, (394..=400).collect::<Vec<_>>(), "the newest are kept");
        assert!(
            st.history
                .describe()
                .starts_with("(393 earlier facts summarised)")
        );
    }

    #[test]
    fn an_empty_history_projects_the_seed() {
        let st = seeded(3);
        assert_eq!(st.history.project(), st.lives());
        assert_eq!(st.history.total(), 0);
    }

    fn line(proc: &str, text: &str, transient: bool) -> LogLine {
        LogLine {
            seq: 0,
            proc: proc.into(),
            text: text.into(),
            transient,
        }
    }

    #[test]
    fn transient_lines_overwrite_in_place() {
        let mut s = LogStore::new(100);
        s.push(line("api", "Compiling 1/10", true));
        s.push(line("api", "Compiling 5/10", true));
        s.push(line("api", "Compiling 9/10", true));
        assert_eq!(s.len(), 1, "progress frames must collapse");
        assert_eq!(s.view(None)[0].text, "Compiling 9/10");
    }

    #[test]
    fn a_real_line_after_progress_is_kept() {
        let mut s = LogStore::new(100);
        s.push(line("api", "Compiling", true));
        s.push(line("api", "Finished", false));
        s.push(line("api", "Listening on :8080", false));
        let texts: Vec<&str> = s.view(None).iter().map(|l| l.text.as_str()).collect();
        assert_eq!(texts, vec!["Finished", "Listening on :8080"]);
    }

    #[test]
    fn interleaved_processes_do_not_overwrite_each_other() {
        let mut s = LogStore::new(100);
        s.push(line("api", "progress", true));
        s.push(line("web", "hello", false));
        s.push(line("api", "progress2", true));
        assert_eq!(s.len(), 2);
        assert_eq!(s.view(Some("web")).len(), 1);
        assert_eq!(s.view(Some("api"))[0].text, "progress2");
    }

    #[test]
    fn ring_buffer_respects_its_cap() {
        let mut s = LogStore::new(3);
        for i in 0..10 {
            s.push(line("api", &format!("l{i}"), false));
        }
        assert_eq!(s.len(), 3);
        assert_eq!(s.view(None)[0].text, "l7");
    }

    #[test]
    fn the_store_is_bounded_by_bytes_as_well_as_lines() {
        let mut s = LogStore::with_byte_cap(1000, 100);
        for i in 0..50 {
            s.push(line("api", &format!("{i:08}"), false));
        }
        assert!(s.bytes() <= 100, "{} bytes held", s.bytes());
        assert_eq!(s.len(), 12, "the newest twelve 8-byte lines");
        assert_eq!(s.view(None).last().unwrap().text, "00000049");
        // Progress frames replace each other, and the count follows.
        s.push(line("api", "50%", true));
        s.push(line("api", "100%", true));
        assert_eq!(
            s.bytes(),
            s.view(None).iter().map(|l| l.text.len()).sum::<usize>()
        );
        // A line bigger than the whole budget is still the one shown.
        s.push(line("api", &"x".repeat(500), false));
        assert_eq!(s.len(), 1);
        assert_eq!(s.bytes(), 500);
    }

    #[test]
    fn durations_format_readably() {
        assert_eq!(fmt_dur(Duration::from_millis(340)), "340ms");
        assert_eq!(fmt_dur(Duration::from_millis(2400)), "2.4s");
        assert_eq!(fmt_dur(Duration::from_secs(125)), "2m05s");
    }
}
