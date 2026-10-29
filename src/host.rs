//! What the engine needs from the machine it runs on: starting processes,
//! probing them, freeing ports.
//!
//! Behind a trait so the whole engine — supervisors, dependency cascades,
//! timeouts, crash limits — can run against [`fake::FakeHost`] on tokio's paused
//! clock, with no process ever started and no time actually passing (cairn
//! 0106). [`Real`] is what `up` and `run` use.

use crate::config::Health;
use crate::proc::{self, Handle, ProcEvent};
use std::collections::BTreeMap;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Everything needed to start a node's command.
pub struct SpawnRequest<'a> {
    pub name: &'a str,
    pub cmd: &'a str,
    pub cwd: &'a Path,
    pub env: &'a BTreeMap<String, String>,
    /// Strict environment: the child sees `env` and nothing else.
    pub isolated: bool,
    pub events: mpsc::UnboundedSender<ProcEvent>,
}

/// One readiness or liveness probe of a node.
pub struct ProbeRequest<'a> {
    pub name: &'a str,
    pub health: &'a Health,
    pub cwd: &'a Path,
    /// The node's environment, for a `cmd` probe.
    pub env: &'a BTreeMap<String, String>,
    pub isolated: bool,
}

/// A running child, real or not.
pub trait Child: Send + Sync {
    fn pid(&self) -> Option<u32>;
    fn try_exit_code(&self) -> Option<i32>;
    /// Resolves with the exit code once the child has exited.
    ///
    /// Polls by default, which is free under a paused clock; a real child
    /// overrides it with a notification.
    fn exited(&self) -> BoxFuture<'_, i32> {
        Box::pin(async move {
            loop {
                if let Some(code) = self.try_exit_code() {
                    return code;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
    }
    /// Ends the child and everything it started, giving it `grace` first.
    fn stop(&self, grace: Duration) -> BoxFuture<'_, ()>;
    /// Ends the child and everything it started, now.
    fn kill_now(&self);
    fn signal(&self, sig: i32) -> bool;
    /// The pty behind a real child, for attaching a terminal to it.
    fn terminal(&self) -> Option<&Handle> {
        None
    }
}

pub trait Host: Send + Sync {
    fn spawn(&self, req: SpawnRequest<'_>) -> anyhow::Result<Arc<dyn Child>>;
    /// Every declared probe but the log line, which only the engine can see.
    fn probe<'a>(&'a self, req: ProbeRequest<'a>) -> BoxFuture<'a, Result<(), String>>;
    /// Waits for `port` to be free. `Some(holder)` if it still is not.
    fn reclaim_port(&self, port: u16) -> BoxFuture<'_, Option<String>>;
}

impl Child for Handle {
    fn pid(&self) -> Option<u32> {
        self.pid
    }
    fn try_exit_code(&self) -> Option<i32> {
        Handle::try_exit_code(self)
    }
    fn exited(&self) -> BoxFuture<'_, i32> {
        Box::pin(Handle::exited(self))
    }
    fn stop(&self, grace: Duration) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            let _ = Handle::stop(self, grace).await;
        })
    }
    fn kill_now(&self) {
        Handle::kill_now(self)
    }
    fn signal(&self, sig: i32) -> bool {
        Handle::signal(self, sig)
    }
    fn terminal(&self) -> Option<&Handle> {
        Some(self)
    }
}

/// Real processes on a pty, real sockets, real ports.
pub struct Real;

impl Host for Real {
    fn spawn(&self, req: SpawnRequest<'_>) -> anyhow::Result<Arc<dyn Child>> {
        let h = proc::spawn_with(
            req.name,
            req.cmd,
            req.cwd,
            req.env,
            req.isolated,
            req.events,
        )?;
        Ok(Arc::new(h))
    }

    fn probe<'a>(&'a self, req: ProbeRequest<'a>) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            let h = req.health;
            if !crate::health::probe(h).await {
                return Err(match (&h.tcp, &h.http) {
                    (Some(port), _) => format!("nothing answering on port {port}"),
                    (_, Some(url)) => format!("{url} not answering with 2xx/3xx"),
                    _ => "not answering".into(),
                });
            }
            if let Some(cmd) = &h.cmd {
                let timeout =
                    crate::config::parse_duration(&h.timeout).unwrap_or(Duration::from_secs(5));
                crate::health::exec_probe(cmd, req.cwd, req.env, req.isolated, timeout).await?;
            }
            Ok(())
        })
    }

    fn reclaim_port(&self, port: u16) -> BoxFuture<'_, Option<String>> {
        Box::pin(async move {
            let free = crate::health::wait_port_free(port, Duration::from_secs(5))
                .await
                .unwrap_or(true);
            (!free).then(|| {
                crate::health::port_holder(port).unwrap_or_else(|| "unknown process".into())
            })
        })
    }
}

/// A host that starts nothing: children are scripted, probes are switches.
///
/// Public rather than test-only because integration tests are a separate crate
/// and need it. Time is tokio's, so under `start_paused` a child scripted to
/// exit after ten minutes does so the moment everything else is idle.
pub mod fake {
    use super::*;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};

    /// How the next child of a node behaves.
    #[derive(Debug, Clone, Default)]
    pub struct Script {
        /// Exit by itself after this long; `None` runs until stopped.
        pub exit_after: Option<Duration>,
        /// The code it exits with by itself.
        pub code: i32,
        /// Fail the spawn instead, with this error.
        pub spawn_error: Option<String>,
    }

    impl Script {
        pub fn forever() -> Script {
            Script::default()
        }
        pub fn exits(after: Duration, code: i32) -> Script {
            Script {
                exit_after: Some(after),
                code,
                spawn_error: None,
            }
        }
        pub fn fails_to_spawn(why: &str) -> Script {
            Script {
                spawn_error: Some(why.into()),
                ..Script::default()
            }
        }
    }

    struct FakeChild {
        pid: u32,
        started: tokio::time::Instant,
        script: Script,
        /// Set when stopped or killed: the code it "exited" with.
        ended: Mutex<Option<i32>>,
        signalled: AtomicBool,
    }

    impl Child for FakeChild {
        fn pid(&self) -> Option<u32> {
            Some(self.pid)
        }
        fn try_exit_code(&self) -> Option<i32> {
            if let Some(code) = *self.ended.lock().unwrap() {
                return Some(code);
            }
            let after = self.script.exit_after?;
            (tokio::time::Instant::now() >= self.started + after).then_some(self.script.code)
        }
        fn stop(&self, _grace: Duration) -> BoxFuture<'_, ()> {
            Box::pin(async move { self.kill_now() })
        }
        fn kill_now(&self) {
            let mut ended = self.ended.lock().unwrap();
            if ended.is_none() {
                // What a shell killed by SIGTERM reports.
                *ended = Some(143);
            }
        }
        fn signal(&self, _sig: i32) -> bool {
            self.signalled.store(true, Ordering::SeqCst);
            true
        }
    }

    #[derive(Default)]
    struct State {
        scripts: BTreeMap<String, Vec<Script>>,
        default_script: BTreeMap<String, Script>,
        probe_ok: BTreeMap<String, bool>,
        spawns: BTreeMap<String, u32>,
        spawned_at: BTreeMap<String, Vec<tokio::time::Instant>>,
        live: BTreeMap<String, Arc<FakeChild>>,
        next_pid: u32,
        held_ports: BTreeMap<u16, String>,
    }

    #[derive(Default)]
    pub struct FakeHost {
        state: Mutex<State>,
    }

    impl FakeHost {
        pub fn new() -> Arc<FakeHost> {
            Arc::new(FakeHost::default())
        }

        /// How every child of `node` behaves, unless a queued script says
        /// otherwise.
        pub fn always(&self, node: &str, script: Script) {
            self.state
                .lock()
                .unwrap()
                .default_script
                .insert(node.into(), script);
        }

        /// How the next child of `node` behaves, after any already queued.
        pub fn then(&self, node: &str, script: Script) {
            self.state
                .lock()
                .unwrap()
                .scripts
                .entry(node.into())
                .or_default()
                .push(script);
        }

        /// Make `node`'s probes answer, or not.
        pub fn set_probe(&self, node: &str, ok: bool) {
            self.state.lock().unwrap().probe_ok.insert(node.into(), ok);
        }

        /// Make `port` read as held by `holder` until released.
        pub fn hold_port(&self, port: u16, holder: &str) {
            self.state
                .lock()
                .unwrap()
                .held_ports
                .insert(port, holder.into());
        }

        /// How many times `node` has been spawned, failed spawns included.
        pub fn spawns(&self, node: &str) -> u32 {
            self.state
                .lock()
                .unwrap()
                .spawns
                .get(node)
                .copied()
                .unwrap_or(0)
        }

        /// When each spawn of `node` happened, on tokio's clock.
        pub fn spawn_times(&self, node: &str) -> Vec<tokio::time::Instant> {
            self.state
                .lock()
                .unwrap()
                .spawned_at
                .get(node)
                .cloned()
                .unwrap_or_default()
        }

        /// Whether `node`'s latest child is still running.
        pub fn running(&self, node: &str) -> bool {
            self.state
                .lock()
                .unwrap()
                .live
                .get(node)
                .is_some_and(|c| c.try_exit_code().is_none())
        }

        /// Make `node`'s latest child exit now with `code`, as a crash would.
        pub fn crash(&self, node: &str, code: i32) {
            if let Some(c) = self.state.lock().unwrap().live.get(node) {
                let mut ended = c.ended.lock().unwrap();
                if ended.is_none() {
                    *ended = Some(code);
                }
            }
        }

        /// Whether `node`'s latest child has been signalled.
        pub fn signalled(&self, node: &str) -> bool {
            self.state
                .lock()
                .unwrap()
                .live
                .get(node)
                .is_some_and(|c| c.signalled.load(Ordering::SeqCst))
        }
    }

    impl Host for FakeHost {
        fn spawn(&self, req: SpawnRequest<'_>) -> anyhow::Result<Arc<dyn Child>> {
            let mut st = self.state.lock().unwrap();
            *st.spawns.entry(req.name.into()).or_default() += 1;
            st.spawned_at
                .entry(req.name.into())
                .or_default()
                .push(tokio::time::Instant::now());
            let queued = st.scripts.get_mut(req.name).filter(|q| !q.is_empty());
            let script = match queued {
                Some(q) => q.remove(0),
                None => st.default_script.get(req.name).cloned().unwrap_or_default(),
            };
            if let Some(why) = script.spawn_error {
                anyhow::bail!("{why}");
            }
            st.next_pid += 1;
            let child = Arc::new(FakeChild {
                pid: 10_000 + st.next_pid,
                started: tokio::time::Instant::now(),
                script,
                ended: Mutex::new(None),
                signalled: AtomicBool::new(false),
            });
            st.live.insert(req.name.into(), child.clone());
            let _ = req.events.send(ProcEvent::Started {
                proc: req.name.into(),
                pid: Some(child.pid),
            });
            Ok(child)
        }

        fn probe<'a>(&'a self, req: ProbeRequest<'a>) -> BoxFuture<'a, Result<(), String>> {
            let ok = self
                .state
                .lock()
                .unwrap()
                .probe_ok
                .get(req.name)
                .copied()
                .unwrap_or(true);
            Box::pin(async move {
                if ok {
                    Ok(())
                } else {
                    Err("fake probe says no".into())
                }
            })
        }

        fn reclaim_port(&self, port: u16) -> BoxFuture<'_, Option<String>> {
            let held = self.state.lock().unwrap().held_ports.get(&port).cloned();
            Box::pin(async move { held })
        }
    }
}
