//! What a supervisor decides, without doing any of it.
//!
//! A node's lifecycle used to live in one async loop that decided and acted in
//! the same breath — spawning, probing and sleeping between its decisions — so
//! the only way to test a decision was to run real processes and wait (cairn
//! 0105). Here the decisions are a pure function, `on(event, deps, now)`, that
//! returns what to do; the engine's executor does it and reports back with
//! another event. Time is a parameter and dependencies are a snapshot, so a test
//! can drive a node through any sequence exactly, at any speed.
//!
//! Services and tasks have separate machines. They share only the start of a
//! cycle — wait for dependencies, drain stale requests, note what is being run
//! against — which is [`Gate`].

use crate::config::{OnChange, Reload, RestartPolicy};
use crate::state::Status;
use std::time::{Duration, Instant};

/// What a node's dependencies look like right now. Built by the executor from
/// the engine's board before every call.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Deps {
    /// The first dependency that is not ready, if any.
    pub unready: Option<String>,
    /// Each dependency's generation, in `depends_on` order.
    pub generations: Vec<u64>,
    /// Dependency names, in the same order.
    pub names: Vec<String>,
}

impl Deps {
    /// The first dependency whose generation differs from `seen`.
    fn moved_since(&self, seen: &[u64]) -> Option<&str> {
        self.names
            .iter()
            .zip(self.generations.iter().zip(seen))
            .find(|(_, (now, then))| now != then)
            .map(|(d, _)| d.as_str())
    }
}

/// The timers a machine can ask for. One of each at a time; setting one again
/// replaces it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Timer {
    /// The next readiness probe at startup.
    Probe,
    /// The end of a restart backoff.
    Backoff,
    /// The next liveness check.
    Liveness,
    /// The next look at a self-reloading process's output counters.
    ReloadPoll,
}

/// What a probe was for, so its answer goes to the right place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Purpose {
    /// Startup readiness: the log line first, then every other probe.
    Ready,
    /// A self-reloading process coming back: every probe but the log line.
    Reload,
    /// The liveness probe.
    Liveness,
}

/// The output counters the event pump keeps for a self-reloading process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ReloadCounters {
    pub reload_matches: u64,
    pub ready_matches: u64,
    pub ready_matches_at_reload: u64,
    /// Errors rustc has reported since the rebuild began.
    pub errors: usize,
    /// Lines matching `reloading.failed` since the latest reload or ready line.
    pub failures: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// The supervisor has just started.
    Begin,
    /// The board ticked: some node's status changed.
    Board,
    /// Someone asked for a restart: a watched change, the TUI, the overlay.
    Restart(String),
    /// Shut down.
    Stop,
    /// The control queue was drained; `stop` if a stop was among what was there.
    Drained {
        stop: bool,
    },
    /// The port is free, or as free as waiting made it.
    PortReclaimed,
    /// A build slot is ours.
    SlotAcquired,
    Spawned,
    SpawnFailed(String),
    /// The child exited by itself.
    Exited(i32),
    /// A child we asked to stop has gone.
    ChildStopped,
    Timer(Timer),
    Probed {
        purpose: Purpose,
        result: Result<(), String>,
    },
    ReloadCounters(ReloadCounters),
    /// A task's run finished: its exit code, and whether it was a cache hit.
    JobDone {
        code: i32,
        cached: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    SetStatus(Status),
    SetReason(String),
    Log(String),
    /// Log, mark failed with `code`, and tell the reporter, for a failure with
    /// no process behind it.
    Fail {
        error: String,
        code: i32,
    },
    /// Count a restart against the node, with its reason.
    CountRestart(String),
    /// Discard queued control messages; answers with `Drained`.
    DrainControl,
    /// Wait for `port` to be free; answers with `PortReclaimed`.
    ReclaimPort(u16),
    /// Take a build slot; answers with `SlotAcquired`.
    AcquireSlot,
    ReleaseSlot,
    /// Start the child; answers with `Spawned` or `SpawnFailed`.
    Spawn,
    /// Stop the child, giving it `grace`; answers with `ChildStopped`.
    StopChild {
        grace: Duration,
    },
    /// Send `sig` to the child, because `reason` changed.
    Signal {
        sig: i32,
        reason: String,
    },
    Probe(Purpose),
    /// Read the output counters; answers with `ReloadCounters`.
    ReadReloadCounters,
    SetTimer(Timer, Instant),
    CancelTimer(Timer),
    /// Tell the browser to reload.
    ReloadBrowser,
    /// Run the task once, honouring the cache; answers with `JobDone`.
    RunJob,
    /// The supervisor is finished.
    Exit,
}

/// The settings a service's lifecycle depends on, taken from its plan node.
#[derive(Debug, Clone)]
pub struct ServiceConfig {
    /// `false` for a pure `serve` node, which turborust hosts in-process.
    pub has_cmd: bool,
    pub port: Option<u16>,
    /// Whether a readiness probe is declared at all.
    pub probed: bool,
    pub probe_interval: Duration,
    pub ready_timeout: Duration,
    pub stop_timeout: Duration,
    pub restart: RestartPolicy,
    pub on_change: OnChange,
    pub signal: Option<i32>,
    pub reload: Reload,
    /// Whether a `reloading` pattern is declared, which is what self-reload
    /// following needs besides `reload = "self"`.
    pub follows_reloads: bool,
    /// `(every, failures)` for a liveness probe.
    pub liveness: Option<(Duration, u32)>,
    pub backoff_min: Duration,
    pub backoff_max: Duration,
    pub max_restarts: Option<u32>,
}

impl ServiceConfig {
    pub fn from_node(node: &crate::plan::Node) -> ServiceConfig {
        ServiceConfig {
            has_cmd: node.cmd.is_some(),
            port: node.port,
            probed: node.health.is_some(),
            probe_interval: node
                .health
                .as_ref()
                .and_then(|h| crate::config::parse_duration(&h.interval).ok())
                .unwrap_or(Duration::from_millis(300)),
            ready_timeout: node.ready_timeout,
            stop_timeout: node.stop_timeout,
            restart: node.restart,
            on_change: node.on_change,
            signal: node.signal,
            reload: node.reload,
            follows_reloads: node.reload == Reload::SelfManaged && node.reloading.is_some(),
            liveness: node.liveness.as_ref().map(|l| (l.every, l.failures)),
            backoff_min: node.backoff_min,
            backoff_max: node.backoff_max,
            max_restarts: node.max_restarts,
        }
    }
}

/// The start of every cycle, shared by both machines: wait until every
/// dependency is ready, discard requests made while waiting, and note the
/// generations being run against.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Gate {
    /// Waiting on a dependency.
    Waiting,
    /// Every dependency ready; waiting for the control queue to be drained.
    Draining,
}

/// Where a gate step leaves the caller.
enum Gated {
    /// Still waiting; these actions are what that takes.
    Wait(Vec<Action>),
    /// Through, running against these generations.
    Open(Vec<u64>),
    /// A stop arrived; the actions end the supervisor.
    Stopped(Vec<Action>),
}

impl Gate {
    fn on(&mut self, ev: &Event, deps: &Deps) -> Gated {
        match (self.clone(), ev) {
            (_, Event::Stop) => {
                Gated::Stopped(vec![Action::SetStatus(Status::Stopped), Action::Exit])
            }
            (Gate::Draining, Event::Drained { stop: true }) => {
                Gated::Stopped(vec![Action::SetStatus(Status::Stopped), Action::Exit])
            }
            (Gate::Draining, Event::Drained { stop: false }) => {
                Gated::Open(deps.generations.clone())
            }
            (Gate::Draining, _) => Gated::Wait(Vec::new()),
            // A restart while not running is noise: starting is what this is
            // waiting to do anyway.
            (Gate::Waiting, _) => match &deps.unready {
                Some(dep) => Gated::Wait(vec![Action::SetStatus(Status::Waiting(dep.clone()))]),
                None => {
                    *self = Gate::Draining;
                    Gated::Wait(vec![Action::DrainControl])
                }
            },
        }
    }
}

/// Consecutive crash restarts, against `max_restarts`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Crashes {
    limit: Option<u32>,
    /// A run longer than this was not part of a crash loop.
    settled: Duration,
    restarts: u32,
}

impl Crashes {
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
}

/// How a self-reloading process stands, while it runs.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Rebuild {
    /// Not followed: `reload = "self"` without a `reloading` pattern, or not
    /// self-reloading at all.
    Off,
    /// The first look at the counters has not happened yet.
    Unknown {
        ready: bool,
    },
    Ready {
        seen: u64,
    },
    Rebuilding {
        seen: u64,
        deadline: Instant,
    },
    /// Out of ready, waiting for the process to recover by itself — which a
    /// self-reloading process does on the next edit that compiles.
    Unready {
        seen: u64,
    },
}

impl Rebuild {
    fn seen(&self) -> u64 {
        match self {
            Rebuild::Ready { seen }
            | Rebuild::Rebuilding { seen, .. }
            | Rebuild::Unready { seen } => *seen,
            _ => 0,
        }
    }
}

/// Liveness, while a process runs.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Alive {
    every: Duration,
    failures: u32,
    misses: u32,
    /// This check took the node out of ready, so it may put it back — and
    /// only then: a failed self-reload is not liveness's to clear.
    took_down: bool,
}

/// Why a running process is being stopped, which decides what comes after.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Then {
    /// Shutdown: the supervisor ends.
    Exit,
    /// A restart: start a new cycle.
    Cycle,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Phase {
    Gate(Gate),
    Reclaiming,
    Queued,
    Spawning,
    Starting {
        deadline: Instant,
    },
    Running {
        rebuild: Rebuild,
        alive: Option<Alive>,
        /// The pending answer of a reload poll, held while its probe runs.
        pending: Option<ReloadCounters>,
    },
    Stopping(Then),
    Backoff,
    /// Not running, until a change or a manual restart asks again.
    Parked {
        /// Log "restarting: …" when woken.
        announce: bool,
        /// Clear the crash count when woken: set after giving up.
        reset: bool,
    },
    /// A pure `serve` node, hosted in-process: ready while parked.
    Served,
    Done,
}

/// A service's lifecycle.
#[derive(Debug, Clone)]
pub struct Service {
    cfg: ServiceConfig,
    phase: Phase,
    /// The generations this cycle runs against.
    seen: Vec<u64>,
    backoff: Duration,
    crashes: Crashes,
    started: Option<Instant>,
}

impl Service {
    pub fn new(cfg: ServiceConfig) -> Service {
        Service {
            backoff: cfg.backoff_min,
            crashes: Crashes {
                limit: cfg.max_restarts,
                settled: cfg.backoff_max,
                restarts: 0,
            },
            cfg,
            phase: Phase::Gate(Gate::Waiting),
            seen: Vec::new(),
            started: None,
        }
    }

    /// Whether the executor should be watching for the child to exit.
    pub fn has_child(&self) -> bool {
        matches!(self.phase, Phase::Starting { .. } | Phase::Running { .. })
    }

    pub fn is_done(&self) -> bool {
        self.phase == Phase::Done
    }

    /// One step: what `ev` means for this node, given its dependencies and the time.
    pub fn on(&mut self, ev: Event, deps: &Deps, now: Instant) -> Vec<Action> {
        let mut out = Vec::new();
        self.step(ev, deps, now, &mut out);
        if out.contains(&Action::Exit) {
            self.phase = Phase::Done;
        }
        out
    }

    fn step(&mut self, ev: Event, deps: &Deps, now: Instant, out: &mut Vec<Action>) {
        match self.phase.clone() {
            Phase::Done => {}
            Phase::Gate(mut gate) => {
                let gated = gate.on(&ev, deps);
                self.phase = Phase::Gate(gate);
                match gated {
                    Gated::Wait(acts) => out.extend(acts),
                    Gated::Stopped(acts) => out.extend(acts),
                    Gated::Open(seen) => {
                        self.seen = seen;
                        self.after_gate(out);
                    }
                }
            }
            Phase::Reclaiming => match ev {
                Event::PortReclaimed => self.acquire(out),
                Event::Stop => self.exit_stopped(out),
                _ => {}
            },
            Phase::Queued => match ev {
                Event::SlotAcquired => {
                    out.push(Action::SetStatus(Status::Starting));
                    out.push(Action::Spawn);
                    self.phase = Phase::Spawning;
                }
                Event::Stop => self.exit_stopped(out),
                _ => {}
            },
            Phase::Spawning => match ev {
                Event::Spawned => {
                    self.started = Some(now);
                    if self.cfg.probed {
                        self.phase = Phase::Starting {
                            deadline: now + self.cfg.ready_timeout,
                        };
                        out.push(Action::Probe(Purpose::Ready));
                    } else {
                        out.push(Action::SetStatus(Status::Healthy));
                        self.became_ready(true, now, out);
                    }
                }
                Event::SpawnFailed(e) => self.spawn_failed(e, now, out),
                _ => {}
            },
            Phase::Starting { deadline } => self.starting(ev, deps, now, deadline, out),
            Phase::Running {
                rebuild,
                alive,
                pending,
            } => self.running(ev, deps, now, rebuild, alive, pending, out),
            Phase::Stopping(then) => {
                if ev == Event::ChildStopped {
                    match then {
                        Then::Exit => out.push(Action::Exit),
                        Then::Cycle => self.cycle(deps, now, out),
                    }
                }
            }
            Phase::Backoff => match ev {
                Event::Timer(Timer::Backoff) => self.cycle(deps, now, out),
                Event::Stop => {
                    out.push(Action::CancelTimer(Timer::Backoff));
                    self.exit_stopped(out);
                }
                Event::Restart(r) => {
                    out.push(Action::CancelTimer(Timer::Backoff));
                    out.push(Action::Log(format!("restarting: {r}")));
                    self.backoff = self.cfg.backoff_min;
                    self.crashes.restarts = 0;
                    self.cycle(deps, now, out);
                }
                // Dependencies are not consulted while backing off: the next
                // cycle starts by waiting for them anyway.
                _ => {}
            },
            Phase::Parked { announce, reset } => {
                let reason = match &ev {
                    Event::Restart(r) => Some(r.clone()),
                    Event::Board => deps
                        .moved_since(&self.seen)
                        .map(|d| format!("dependency `{d}` changed")),
                    Event::Stop => {
                        out.push(Action::Exit);
                        return;
                    }
                    _ => None,
                };
                if let Some(reason) = reason {
                    if announce {
                        out.push(Action::Log(format!("restarting: {reason}")));
                    }
                    if reset {
                        self.crashes.restarts = 0;
                    }
                    self.cycle(deps, now, out);
                }
            }
            Phase::Served => {
                let wake = match &ev {
                    Event::Restart(_) => true,
                    Event::Board => deps.moved_since(&self.seen).is_some(),
                    Event::Stop => {
                        out.push(Action::Exit);
                        return;
                    }
                    _ => false,
                };
                if wake {
                    self.cycle(deps, now, out);
                }
            }
        }
    }

    /// A new cycle: back to waiting on dependencies, evaluated at once.
    fn cycle(&mut self, deps: &Deps, now: Instant, out: &mut Vec<Action>) {
        self.phase = Phase::Gate(Gate::Waiting);
        self.step(Event::Board, deps, now, out);
    }

    fn after_gate(&mut self, out: &mut Vec<Action>) {
        if !self.cfg.has_cmd {
            out.push(Action::SetStatus(Status::Healthy));
            self.phase = Phase::Served;
            return;
        }
        match self.cfg.port {
            Some(port) => {
                out.push(Action::ReclaimPort(port));
                self.phase = Phase::Reclaiming;
            }
            None => self.acquire(out),
        }
    }

    fn acquire(&mut self, out: &mut Vec<Action>) {
        out.push(Action::AcquireSlot);
        self.phase = Phase::Queued;
    }

    fn exit_stopped(&mut self, out: &mut Vec<Action>) {
        out.push(Action::SetStatus(Status::Stopped));
        out.push(Action::Exit);
    }

    fn spawn_failed(&mut self, e: String, now: Instant, out: &mut Vec<Action>) {
        out.push(Action::Fail {
            error: format!("spawn failed: {e}"),
            code: 127,
        });
        out.push(Action::ReleaseSlot);
        if self.cfg.restart == RestartPolicy::Never {
            out.push(Action::Exit);
            return;
        }
        if self.crashes.exhausted(Duration::ZERO) {
            self.give_up(127, out);
            return;
        }
        // The backoff after a failed spawn is a backoff like any other, which
        // a stop can now end early; it used to be a plain sleep that a
        // shutdown had to wait out.
        self.start_backoff(now, false, out);
    }

    fn give_up(&mut self, code: i32, out: &mut Vec<Action>) {
        let why = format!(
            "gave up after {} restart(s) in a row; waiting for a change or a manual restart",
            self.crashes.restarts
        );
        out.push(Action::Log(why.clone()));
        out.push(Action::SetStatus(Status::Failed(code)));
        out.push(Action::SetReason(why));
        self.backoff = self.cfg.backoff_min;
        self.phase = Phase::Parked {
            announce: true,
            reset: true,
        };
    }

    fn start_backoff(&mut self, now: Instant, show: bool, out: &mut Vec<Action>) {
        let delay = self.backoff;
        if show {
            out.push(Action::SetStatus(Status::Backoff(delay)));
        }
        // Exponential, bounded by `backoff_max`: a service that crashes
        // instantly must not become a fork bomb.
        self.backoff = (self.backoff * 2).min(self.cfg.backoff_max);
        out.push(Action::SetTimer(Timer::Backoff, now + delay));
        self.phase = Phase::Backoff;
    }

    /// Readiness reached, or given up on: release the slot, tell the browser,
    /// and start watching the running process.
    fn became_ready(&mut self, ready: bool, now: Instant, out: &mut Vec<Action>) {
        out.push(Action::ReleaseSlot);
        if ready {
            out.push(Action::ReloadBrowser);
        }
        let rebuild = if self.cfg.follows_reloads {
            out.push(Action::ReadReloadCounters);
            Rebuild::Unknown { ready }
        } else {
            Rebuild::Off
        };
        let alive = self.cfg.liveness.map(|(every, failures)| {
            out.push(Action::SetTimer(Timer::Liveness, now + every));
            Alive {
                every,
                failures,
                misses: 0,
                took_down: false,
            }
        });
        self.phase = Phase::Running {
            rebuild,
            alive,
            pending: None,
        };
    }

    /// Why a running node should stop for its dependencies, if it should.
    ///
    /// Read from the board, not from messages: a dependency that is not ready
    /// (rebuilding, or gone), or one whose generation moved past what this
    /// process started against — a result it never ran on, which a fast
    /// enough rebuild produces without ever visibly leaving ready (0101).
    fn dependency_moved(&self, deps: &Deps) -> Option<String> {
        if let Some(d) = &deps.unready {
            return Some(format!("dependency `{d}` is rebuilding"));
        }
        deps.moved_since(&self.seen)
            .map(|d| format!("dependency `{d}` produced a new result"))
    }

    fn starting(
        &mut self,
        ev: Event,
        deps: &Deps,
        now: Instant,
        deadline: Instant,
        out: &mut Vec<Action>,
    ) {
        match ev {
            Event::Probed {
                purpose: Purpose::Ready,
                result: Ok(()),
            } => {
                out.push(Action::CancelTimer(Timer::Probe));
                out.push(Action::SetStatus(Status::Healthy));
                // Only a passing probe proves the process is past whatever was
                // crashing it. A node with no probe is "healthy" the instant it
                // spawns, and resetting there would pin a crash loop's backoff
                // at the minimum forever.
                self.backoff = self.cfg.backoff_min;
                self.became_ready(true, now, out);
            }
            Event::Probed {
                purpose: Purpose::Ready,
                result: Err(why),
            } => {
                if now >= deadline {
                    out.push(Action::Log(format!("last readiness attempt: {why}")));
                    self.not_ready(now, out);
                } else {
                    out.push(Action::SetTimer(
                        Timer::Probe,
                        now + self.cfg.probe_interval,
                    ));
                }
            }
            Event::Timer(Timer::Probe) => out.push(Action::Probe(Purpose::Ready)),
            // The child died before it was ready: readiness has failed, which
            // is said first, and then the exit is handled like any other.
            Event::Exited(code) => {
                out.push(Action::CancelTimer(Timer::Probe));
                self.not_ready(now, out);
                self.step(Event::Exited(code), deps, now, out);
            }
            Event::Board => {
                if let Some(why) = self.dependency_moved(deps) {
                    self.interrupt_startup(why, out);
                }
            }
            Event::Restart(r) => {
                // `queue` is the policy that declines to be interrupted here:
                // it exists so a cold build is not killed halfway and then
                // started again from scratch.
                if self.cfg.on_change == OnChange::Restart && self.cfg.reload == Reload::Restart {
                    self.interrupt_startup(r, out);
                }
            }
            Event::Stop => {
                out.push(Action::CancelTimer(Timer::Probe));
                out.push(Action::SetStatus(Status::Stopped));
                out.push(Action::StopChild {
                    grace: self.cfg.stop_timeout,
                });
                self.phase = Phase::Stopping(Then::Exit);
            }
            _ => {}
        }
    }

    /// Readiness never came: still running, not ready, and counted as a
    /// failure — so a one-shot `run` waiting on it ends instead of waiting.
    fn not_ready(&mut self, now: Instant, out: &mut Vec<Action>) {
        out.push(Action::Log("readiness probe never passed".into()));
        out.push(Action::SetStatus(Status::Unhealthy));
        out.push(Action::SetReason(format!(
            "not ready within {}",
            crate::state::fmt_dur(self.cfg.ready_timeout)
        )));
        self.became_ready(false, now, out);
    }

    fn interrupt_startup(&mut self, reason: String, out: &mut Vec<Action>) {
        out.push(Action::CancelTimer(Timer::Probe));
        out.push(Action::SetStatus(Status::Stopping));
        out.push(Action::Log(format!("restarting mid-startup: {reason}")));
        out.push(Action::ReleaseSlot);
        out.push(Action::StopChild {
            grace: self.cfg.stop_timeout,
        });
        self.phase = Phase::Stopping(Then::Cycle);
    }

    #[allow(clippy::too_many_arguments)]
    fn running(
        &mut self,
        ev: Event,
        deps: &Deps,
        now: Instant,
        mut rebuild: Rebuild,
        mut alive: Option<Alive>,
        pending: Option<ReloadCounters>,
        out: &mut Vec<Action>,
    ) {
        match ev {
            Event::Exited(code) => {
                self.cancel_running_timers(out);
                self.exited(code, now, out);
            }
            Event::Board => {
                if let Some(why) = self.dependency_moved(deps) {
                    self.restart_running(why, out);
                }
            }
            Event::Restart(r) => {
                // The process is watching the same files and rebuilding itself;
                // a restart now would throw away what it keeps.
                if self.cfg.reload == Reload::SelfManaged {
                    return;
                }
                match self.cfg.on_change {
                    OnChange::Ignore => {}
                    OnChange::Signal => match self.cfg.signal {
                        Some(sig) => out.push(Action::Signal { sig, reason: r }),
                        None => out.push(Action::Log(
                            "on_change = \"signal\" needs a `signal` value".into(),
                        )),
                    },
                    // Once running, `queue` and `restart` agree: there is no
                    // startup left to protect.
                    OnChange::Restart | OnChange::Queue => self.restart_running(r, out),
                }
            }
            Event::Stop => {
                self.cancel_running_timers(out);
                out.push(Action::SetStatus(Status::Stopped));
                out.push(Action::StopChild {
                    grace: self.cfg.stop_timeout,
                });
                self.phase = Phase::Stopping(Then::Exit);
            }
            Event::Timer(Timer::Liveness) => {
                // A process rebuilding itself is not expected to answer meanwhile.
                if matches!(rebuild, Rebuild::Rebuilding { .. }) {
                    if let Some(a) = &alive {
                        out.push(Action::SetTimer(Timer::Liveness, now + a.every));
                    }
                } else {
                    out.push(Action::Probe(Purpose::Liveness));
                }
            }
            Event::Probed {
                purpose: Purpose::Liveness,
                result,
            } => {
                let Some(a) = alive.as_mut() else { return };
                // The next check is timed from the end of this one, so a probe
                // that runs long is not followed by a burst of catch-up checks.
                out.push(Action::SetTimer(Timer::Liveness, now + a.every));
                match result {
                    Ok(()) => {
                        a.misses = 0;
                        if a.took_down {
                            a.took_down = false;
                            out.push(Action::Log("answering again".into()));
                            out.push(Action::SetStatus(Status::Healthy));
                            out.push(Action::SetReason("answering again".into()));
                        }
                    }
                    Err(why) => {
                        a.misses += 1;
                        if a.misses == a.failures {
                            let why =
                                format!("not answering ({} checks in a row): {why}", a.misses);
                            out.push(Action::Log(why.clone()));
                            if self.cfg.restart == RestartPolicy::Always {
                                self.restart_running(why, out);
                                return;
                            }
                            a.took_down = true;
                            out.push(Action::SetStatus(Status::Unhealthy));
                            out.push(Action::SetReason(why));
                        }
                    }
                }
                self.phase = Phase::Running {
                    rebuild,
                    alive,
                    pending,
                };
            }
            Event::Timer(Timer::ReloadPoll) => out.push(Action::ReadReloadCounters),
            Event::ReloadCounters(c) => {
                out.push(Action::SetTimer(
                    Timer::ReloadPoll,
                    now + self.cfg.probe_interval,
                ));
                if let Rebuild::Unknown { ready } = rebuild {
                    // Rebuilds announced during the first build belong to
                    // startup, which the probe accounted for — unless one came
                    // after the last ready line: the probe looks only every
                    // `interval`, so a rebuild can already be under way when
                    // startup is confirmed, and leaving it unseen makes the
                    // next look pick it up.
                    let in_flight =
                        c.reload_matches > 0 && c.ready_matches_at_reload == c.ready_matches;
                    let seen = c.reload_matches - u64::from(in_flight);
                    // A first build slower than `ready_timeout` is common for
                    // exactly these processes; it should come good when it
                    // finishes, not stay unhealthy until restarted.
                    rebuild = if ready {
                        Rebuild::Ready { seen }
                    } else {
                        Rebuild::Unready { seen }
                    };
                    if !in_flight {
                        self.phase = Phase::Running {
                            rebuild,
                            alive,
                            pending: None,
                        };
                        return;
                    }
                }
                if c.reload_matches != rebuild.seen() {
                    // The down edge, without a restart: `Reloading` is not
                    // ready, so dependents see exactly what they would if this
                    // node had been stopped and started.
                    out.push(Action::SetStatus(Status::Reloading));
                    out.push(Action::SetReason("rebuilding itself".into()));
                    rebuild = Rebuild::Rebuilding {
                        seen: c.reload_matches,
                        deadline: now + self.cfg.ready_timeout,
                    };
                }
                if let Rebuild::Ready { seen } = rebuild
                    && c.failures > 0
                {
                    // A tool that announces only how a rebuild ended (`dx`
                    // prints nothing when one starts) fails from ready: that
                    // line is both the down edge and the verdict.
                    out.push(Action::Log("reload failed: the process reported it".into()));
                    out.push(Action::SetStatus(Status::Unhealthy));
                    out.push(Action::SetReason("rebuild failed".into()));
                    self.phase = Phase::Running {
                        rebuild: Rebuild::Unready { seen },
                        alive,
                        pending: None,
                    };
                    return;
                }
                if matches!(rebuild, Rebuild::Ready { .. }) {
                    self.phase = Phase::Running {
                        rebuild,
                        alive,
                        pending: None,
                    };
                    return;
                }
                if self.cfg.probed {
                    out.push(Action::Probe(Purpose::Reload));
                    self.phase = Phase::Running {
                        rebuild,
                        alive,
                        pending: Some(c),
                    };
                } else {
                    self.phase = Phase::Running {
                        rebuild,
                        alive,
                        pending: None,
                    };
                    self.reload_settle(c, true, now, out);
                }
            }
            Event::Probed {
                purpose: Purpose::Reload,
                result,
            } => {
                self.phase = Phase::Running {
                    rebuild,
                    alive,
                    pending: None,
                };
                if let Some(c) = pending {
                    self.reload_settle(c, result.is_ok(), now, out);
                }
            }
            _ => {}
        }
    }

    /// The second half of a reload poll, once any probe it needed has answered.
    fn reload_settle(
        &mut self,
        c: ReloadCounters,
        probes_pass: bool,
        now: Instant,
        out: &mut Vec<Action>,
    ) {
        let Phase::Running { rebuild, .. } = &mut self.phase else {
            return;
        };
        let seen = rebuild.seen();
        // A failure is forgotten by the next ready line, so one still counted
        // means the ready line before it is not this rebuild coming back.
        let ready_again = c.ready_matches > c.ready_matches_at_reload && c.failures == 0;
        if ready_again && probes_pass {
            out.push(Action::SetStatus(Status::Healthy));
            out.push(Action::SetReason("reloaded itself".into()));
            // Deliberately no browser reload: the process has just patched
            // itself in place, and a full reload would discard the state it
            // went to the trouble of keeping.
            *rebuild = Rebuild::Ready { seen };
            return;
        }
        if let Rebuild::Rebuilding { deadline, .. } = *rebuild {
            let failure = if c.failures > 0 {
                Some("rebuild failed".to_string())
            } else if c.errors > 0 {
                // rustc has already said how this ends; waiting out the
                // timeout would only delay saying so.
                Some(format!(
                    "rebuild failed with {} error{}",
                    c.errors,
                    if c.errors == 1 { "" } else { "s" }
                ))
            } else if now >= deadline {
                Some(format!(
                    "rebuild did not finish within {}",
                    crate::state::fmt_dur(self.cfg.ready_timeout)
                ))
            } else {
                None
            };
            if let Some(why) = failure {
                out.push(Action::Log(format!("reload failed: {why}")));
                out.push(Action::SetStatus(Status::Unhealthy));
                out.push(Action::SetReason(why));
                *rebuild = Rebuild::Unready { seen };
            }
        }
    }

    fn cancel_running_timers(&self, out: &mut Vec<Action>) {
        if self.cfg.liveness.is_some() {
            out.push(Action::CancelTimer(Timer::Liveness));
        }
        if self.cfg.follows_reloads {
            out.push(Action::CancelTimer(Timer::ReloadPoll));
        }
    }

    /// Stop the running process for `reason`, and start again.
    fn restart_running(&mut self, reason: String, out: &mut Vec<Action>) {
        self.cancel_running_timers(out);
        out.push(Action::SetStatus(Status::Stopping));
        out.push(Action::CountRestart(reason.clone()));
        out.push(Action::Log(format!("restarting: {reason}")));
        out.push(Action::StopChild {
            grace: self.cfg.stop_timeout,
        });
        self.backoff = self.cfg.backoff_min;
        self.crashes.restarts = 0;
        self.phase = Phase::Stopping(Then::Cycle);
    }

    fn exited(&mut self, code: i32, now: Instant, out: &mut Vec<Action>) {
        let policy = self.cfg.restart;
        if code == 0 && policy != RestartPolicy::Always {
            out.push(Action::SetStatus(Status::Exited));
        } else if code != 0 {
            out.push(Action::SetStatus(Status::Failed(code)));
        }
        if policy != RestartPolicy::Always {
            // Parked, but restartable by a change.
            self.phase = Phase::Parked {
                announce: true,
                reset: false,
            };
            return;
        }
        let ran = self
            .started
            .map(|s| now.saturating_duration_since(s))
            .unwrap_or_default();
        // A run that outlasted the longest backoff ended a crash loop if there
        // was one, so this crash starts the delays over — the same line the
        // crash count uses. Without a probe nothing else resets them, and a
        // service that crashed once a day waited longer each day (cairn 0111).
        if ran > self.crashes.settled {
            self.backoff = self.cfg.backoff_min;
        }
        if self.crashes.exhausted(ran) {
            self.give_up(code, out);
            return;
        }
        self.start_backoff(now, true, out);
    }
}

/// A task's lifecycle: wait, run, park until asked again.
///
/// Its own machine rather than a branch in the service's: a task has no child to
/// watch between runs, no readiness probe, no restart policy, and runs to
/// completion as one action.
#[derive(Debug, Clone)]
pub struct Task {
    phase: TaskPhase,
    seen: Vec<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TaskPhase {
    Gate(Gate),
    Running,
    Parked,
    Done,
}

impl Default for Task {
    fn default() -> Self {
        Task::new()
    }
}

impl Task {
    pub fn new() -> Task {
        Task {
            phase: TaskPhase::Gate(Gate::Waiting),
            seen: Vec::new(),
        }
    }

    pub fn is_done(&self) -> bool {
        self.phase == TaskPhase::Done
    }

    pub fn on(&mut self, ev: Event, deps: &Deps) -> Vec<Action> {
        let mut out = Vec::new();
        match self.phase.clone() {
            TaskPhase::Done => {}
            TaskPhase::Gate(mut gate) => {
                let gated = gate.on(&ev, deps);
                self.phase = TaskPhase::Gate(gate);
                match gated {
                    Gated::Wait(acts) | Gated::Stopped(acts) => out.extend(acts),
                    Gated::Open(seen) => {
                        self.seen = seen;
                        // Out of ready before anything else, so dependents stop
                        // now rather than after the run: a cache hit never
                        // otherwise leaves `Done`.
                        out.push(Action::SetStatus(Status::Running));
                        out.push(Action::RunJob);
                        self.phase = TaskPhase::Running;
                    }
                }
            }
            TaskPhase::Running => {
                if let Event::JobDone { code, cached } = ev {
                    if code == 0 && !cached {
                        out.push(Action::ReloadBrowser);
                    }
                    self.phase = TaskPhase::Parked;
                }
            }
            // A task stays put until something asks it to run again.
            TaskPhase::Parked => {
                let reason = match &ev {
                    Event::Restart(r) => Some(r.clone()),
                    Event::Board => deps
                        .moved_since(&self.seen)
                        .map(|d| format!("dependency `{d}` changed")),
                    Event::Stop => {
                        out.push(Action::Exit);
                        None
                    }
                    _ => None,
                };
                if let Some(reason) = reason {
                    out.push(Action::SetReason(reason));
                    self.phase = TaskPhase::Gate(Gate::Waiting);
                    out.extend(self.on(Event::Board, deps));
                }
            }
        }
        if out.contains(&Action::Exit) {
            self.phase = TaskPhase::Done;
        }
        out
    }
}
