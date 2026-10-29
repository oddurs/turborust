//! The supervisor's control queue.
//!
//! A flat FIFO is the wrong shape here. Shutdown sends `Stop` to every node, but
//! any `Restart` already queued by a watch event is drained first — so a
//! supervisor stops its child, loops, waits for dependencies, and spawns a fresh
//! process *while we are trying to exit*. Save a file and hit ctrl-c and that is
//! the path taken.
//!
//! Two priorities fix it, and one extra rule matters as much as the ordering:
//! once a stop is in flight the normal queue is closed entirely, so nothing can
//! get in front of a shutdown already under way.
//!
//! Dependency changes are not messages. They used to be (`DepUp`/`DepDown`, at
//! a priority between these two), and a stream of edges can drop one (cairn
//! 0101); dependents now read their dependencies' status from the engine's
//! board instead (0104), so this queue carries only what someone asked for. The shape is modelled on
//! watchexec's supervisor (Apache-2.0), which in turn borrows it from
//! `systemctl`; the implementation here is our own.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::mpsc;

#[derive(Debug, Clone)]
pub enum Ctl {
    /// Something this node watches changed on disk, or someone asked for a
    /// restart. Never more urgent than a shutdown.
    Restart(String),
    /// Shut this node down. Outranks everything.
    Stop,
}

impl Ctl {
    fn priority(&self) -> Priority {
        match self {
            Ctl::Stop => Priority::Urgent,
            Ctl::Restart(_) => Priority::Normal,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Priority {
    Normal,
    Urgent,
}

#[derive(Debug, Clone)]
pub struct Sender {
    urgent: mpsc::UnboundedSender<Ctl>,
    normal: mpsc::UnboundedSender<Ctl>,
    stopping: Arc<AtomicBool>,
}

impl Sender {
    /// Queues a control message at its own priority.
    ///
    /// Normal-priority messages are dropped once a stop is in flight — a restart
    /// requested during shutdown cannot be honoured, and queueing it would only
    /// give a supervisor something to act on after it should have exited.
    pub fn send(&self, msg: Ctl) -> bool {
        let priority = msg.priority();
        if priority == Priority::Urgent {
            // Set the flag only after the stop is queued, so a receiver that sees
            // `stopping` always finds the message waiting for it.
            let ok = self.urgent.send(msg).is_ok();
            self.stopping.store(true, Ordering::SeqCst);
            return ok;
        }
        if self.stopping.load(Ordering::SeqCst) {
            return false;
        }
        self.normal.send(msg).is_ok()
    }

    pub fn is_stopping(&self) -> bool {
        self.stopping.load(Ordering::SeqCst)
    }
}

#[derive(Debug)]
pub struct Receiver {
    urgent: mpsc::UnboundedReceiver<Ctl>,
    normal: mpsc::UnboundedReceiver<Ctl>,
    stopping: Arc<AtomicBool>,
}

impl Receiver {
    /// Next message, highest priority first. `None` once every sender is gone.
    pub async fn recv(&mut self) -> Option<Ctl> {
        // Anything already queued is taken in priority order before waiting.
        if let Ok(msg) = self.urgent.try_recv() {
            return Some(msg);
        }
        if !self.stopping.load(Ordering::SeqCst)
            && let Ok(msg) = self.normal.try_recv()
        {
            return Some(msg);
        }

        if self.stopping.load(Ordering::SeqCst) {
            self.urgent.recv().await
        } else {
            tokio::select! {
                msg = self.urgent.recv() => msg,
                msg = self.normal.recv() => msg,
            }
        }
    }

    /// Discards anything queued, except a stop. Used after a node has been
    /// blocked on dependencies, where the backlog describes a world that has
    /// already moved on.
    pub fn drain_stale(&mut self) -> bool {
        let mut saw_stop = false;
        while let Ok(msg) = self.urgent.try_recv() {
            if matches!(msg, Ctl::Stop) {
                saw_stop = true;
            }
        }
        while self.normal.try_recv().is_ok() {}
        saw_stop || self.is_stopping()
    }

    pub fn is_stopping(&self) -> bool {
        self.stopping.load(Ordering::SeqCst)
    }
}

pub fn channel() -> (Sender, Receiver) {
    let (ut, ur) = mpsc::unbounded_channel();
    let (nt, nr) = mpsc::unbounded_channel();
    let stopping = Arc::new(AtomicBool::new(false));
    (
        Sender {
            urgent: ut,
            normal: nt,
            stopping: stopping.clone(),
        },
        Receiver {
            urgent: ur,
            normal: nr,
            stopping,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn stop_overtakes_a_restart_queued_before_it() {
        let (tx, mut rx) = channel();
        tx.send(Ctl::Restart("a.rs".into()));
        tx.send(Ctl::Restart("b.rs".into()));
        tx.send(Ctl::Stop);
        // This is the whole bug: FIFO would hand back a Restart here, and the
        // supervisor would spawn a process on the way out.
        assert!(matches!(rx.recv().await, Some(Ctl::Stop)));
    }

    #[tokio::test]
    async fn restarts_are_refused_once_stopping() {
        let (tx, mut rx) = channel();
        tx.send(Ctl::Stop);
        assert!(
            !tx.send(Ctl::Restart("late.rs".into())),
            "should be refused"
        );
        assert!(matches!(rx.recv().await, Some(Ctl::Stop)));
        // Nothing else may arrive; a late restart must not resurrect the node.
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(80), rx.recv())
                .await
                .is_err(),
            "queue should be quiet after a stop"
        );
    }

    #[tokio::test]
    async fn ordering_is_preserved_within_a_priority() {
        let (tx, mut rx) = channel();
        tx.send(Ctl::Restart("first".into()));
        tx.send(Ctl::Restart("second".into()));
        let a = rx.recv().await;
        let b = rx.recv().await;
        assert!(matches!(a, Some(Ctl::Restart(ref r)) if r == "first"));
        assert!(matches!(b, Some(Ctl::Restart(ref r)) if r == "second"));
    }

    #[tokio::test]
    async fn drain_stale_reports_a_pending_stop() {
        let (tx, mut rx) = channel();
        tx.send(Ctl::Restart("x".into()));
        assert!(!rx.drain_stale(), "no stop was queued");
        tx.send(Ctl::Stop);
        assert!(rx.drain_stale(), "a queued stop must survive draining");
    }

    #[tokio::test]
    async fn recv_ends_when_every_sender_is_gone() {
        let (tx, mut rx) = channel();
        drop(tx);
        assert!(rx.recv().await.is_none());
    }
}
