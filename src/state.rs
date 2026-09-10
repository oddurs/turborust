//! Observable supervisor state, shared between the engine and any UI.
//!
//! The UI never asks the engine questions; it renders this. That keeps the
//! supervisor loop free of rendering concerns and means a headless run and a TUI
//! run execute exactly the same code path.

use crate::diag::{Diagnostic, Parser};
use crate::proc::LogLine;
use serde::Serialize;
use std::collections::{BTreeMap, VecDeque};
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
}

impl Status {
    pub fn label(&self) -> String {
        match self {
            Status::Pending => "pending".into(),
            Status::Waiting(d) => format!("waiting: {d}"),
            Status::Queued => "waiting: slot".into(),
            Status::Starting => "starting".into(),
            Status::Healthy => "healthy".into(),
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
            Status::Running => "running",
            Status::Done { .. } => "done",
            Status::Failed(_) => "failed",
            Status::Exited => "exited",
            Status::Backoff(_) => "backoff",
            Status::Stopped => "stopped",
        }
    }

    pub fn is_bad(&self) -> bool {
        matches!(self, Status::Failed(_) | Status::Backoff(_))
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
    /// Bumped every time this node completes a run or becomes healthy.
    ///
    /// Dependents compare generations rather than trusting the order of control
    /// messages: a stale `DepUp` that arrives after a dependent already consumed
    /// the same result finds an unchanged generation and is ignored. Racing on
    /// message arrival is what made a task occasionally run twice.
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
            generation: 0,
        }
    }
}

impl NodeState {
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
    last_was_transient: BTreeMap<String, bool>,
}

impl LogStore {
    pub fn new(cap: usize) -> Self {
        LogStore {
            lines: VecDeque::with_capacity(cap.min(4096)),
            cap,
            last_was_transient: BTreeMap::new(),
        }
    }

    pub fn push(&mut self, line: LogLine) {
        let replace = self
            .last_was_transient
            .get(&line.proc)
            .copied()
            .unwrap_or(false);
        self.last_was_transient
            .insert(line.proc.clone(), line.transient);
        if replace && let Some(idx) = self.lines.iter().rposition(|l| l.proc == line.proc) {
            self.lines[idx] = line;
            return;
        }
        if self.lines.len() == self.cap {
            self.lines.pop_front();
        }
        self.lines.push_back(line);
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
            .filter(|l| filter.is_none_or(|f| l.proc == f))
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
}

impl AppState {
    pub fn new(nodes: &[(String, &'static str, Option<u16>)], log_cap: usize) -> Self {
        AppState {
            nodes: nodes
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
                .collect(),
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
            || nodes.iter().any(|n| n.status == "failed")
        {
            "error"
        } else if nodes
            .iter()
            .any(|n| matches!(n.status, "running" | "starting" | "waiting"))
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
                proc: l.proc.clone(),
                // The overlay does its own styling; raw escapes would render as
                // garbage inside a <pre>.
                text: crate::proc::strip_ansi(&l.text),
            })
            .collect()
    }

    pub fn set_ready_pattern(&mut self, name: &str, pattern: Option<String>) {
        if let Some(n) = self.nodes.get_mut(name) {
            n.ready_pattern = pattern;
        }
    }

    pub fn set(&mut self, name: &str, status: Status) {
        if let Some(n) = self.nodes.get_mut(name) {
            n.status = status;
        }
    }

    pub fn names(&self) -> Vec<String> {
        self.nodes.keys().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn durations_format_readably() {
        assert_eq!(fmt_dur(Duration::from_millis(340)), "340ms");
        assert_eq!(fmt_dur(Duration::from_millis(2400)), "2.4s");
        assert_eq!(fmt_dur(Duration::from_secs(125)), "2m05s");
    }
}
