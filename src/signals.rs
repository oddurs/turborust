//! Requests to stop, from outside the process.
//!
//! A supervisor that only listens for ctrl-c dies outright on everything else —
//! `docker stop`, systemd and CI cancellation send SIGTERM, a closed terminal
//! sends SIGHUP — and dying outright skips the shutdown that stops its children.
//! An ordinary child then dies of the pty hangup anyway; one that ignores
//! SIGHUP, as daemons and detaching dev servers do, is orphaned with its port.
//! So all three mean the same thing here: shut down cleanly.
//!
//! A second request means "now". Once a handler is installed the default action
//! is gone for good, so without that a second ctrl-c during a slow shutdown
//! would do nothing at all.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// What arrived.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Request {
    Interrupt,
    Terminate,
    Hangup,
}

impl Request {
    pub fn name(self) -> &'static str {
        match self {
            Request::Interrupt => "SIGINT",
            Request::Terminate => "SIGTERM",
            Request::Hangup => "SIGHUP",
        }
    }

    /// 128 + the signal number: what a shell reports for a process the signal
    /// ended, so an interrupted run reads the same whoever stopped it.
    pub fn exit_code(self) -> i32 {
        128 + match self {
            Request::Interrupt => 2,
            Request::Terminate => 15,
            Request::Hangup => 1,
        }
    }
}

/// Handlers for every stop request, installed together.
///
/// Installed once, before anything is spawned: a handler installed only when a
/// shutdown begins leaves the abrupt default in place until then.
pub struct Signals {
    #[cfg(unix)]
    int: tokio::signal::unix::Signal,
    #[cfg(unix)]
    term: tokio::signal::unix::Signal,
    #[cfg(unix)]
    hup: tokio::signal::unix::Signal,
    #[cfg(windows)]
    ctrl_c: tokio::signal::windows::CtrlC,
    #[cfg(windows)]
    ctrl_break: tokio::signal::windows::CtrlBreak,
    #[cfg(windows)]
    ctrl_close: tokio::signal::windows::CtrlClose,
}

impl Signals {
    pub fn install() -> std::io::Result<Signals> {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{SignalKind, signal};
            Ok(Signals {
                int: signal(SignalKind::interrupt())?,
                term: signal(SignalKind::terminate())?,
                hup: signal(SignalKind::hangup())?,
            })
        }
        // The console events are Windows' nearest equivalents: ctrl-break is
        // what a process manager sends a console app it wants gone, and close
        // is the window going away, as a hangup is the terminal going away.
        #[cfg(windows)]
        {
            use tokio::signal::windows;
            Ok(Signals {
                ctrl_c: windows::ctrl_c()?,
                ctrl_break: windows::ctrl_break()?,
                ctrl_close: windows::ctrl_close()?,
            })
        }
    }

    /// The next request.
    pub async fn recv(&mut self) -> Request {
        #[cfg(unix)]
        {
            tokio::select! {
                _ = self.int.recv() => Request::Interrupt,
                _ = self.term.recv() => Request::Terminate,
                _ = self.hup.recv() => Request::Hangup,
            }
        }
        #[cfg(windows)]
        {
            tokio::select! {
                _ = self.ctrl_c.recv() => Request::Interrupt,
                _ = self.ctrl_break.recv() => Request::Terminate,
                _ = self.ctrl_close.recv() => Request::Hangup,
            }
        }
    }
}

/// Listens for stop requests for the rest of the process's life.
///
/// The first request is handed to whoever is waiting in `Stop::requested`.
/// Any request after a shutdown has begun goes to `force` instead — and a
/// shutdown has begun once `requested` resolves *or is given up on*, which is
/// what quitting the dashboard or a run finishing amounts to. Without that, a
/// ctrl-c during those shutdowns would reach nobody: a handler, once installed,
/// is never uninstalled, so the default action does not come back.
pub fn watch(mut signals: Signals, force: impl FnOnce(Request) + Send + 'static) -> Stop {
    let (tx, rx) = tokio::sync::watch::channel(None);
    let begun = Arc::new(AtomicBool::new(false));
    let shutting_down = begun.clone();
    tokio::spawn(async move {
        loop {
            let request = signals.recv().await;
            if shutting_down.swap(true, Ordering::SeqCst) {
                force(request);
                return;
            }
            let _ = tx.send(Some(request));
        }
    });
    Stop { rx, begun }
}

pub struct Stop {
    rx: tokio::sync::watch::Receiver<Option<Request>>,
    begun: Arc<AtomicBool>,
}

impl Stop {
    /// The first stop request, including one that arrived before this was
    /// called. Resolving or being dropped both mean a shutdown has begun.
    pub async fn requested(&mut self) -> Request {
        let _begun = Begun(self.begun.clone());
        loop {
            if let Some(request) = *self.rx.borrow_and_update() {
                return request;
            }
            if self.rx.changed().await.is_err() {
                // The listener only ends by forcing an exit, so this is never
                // reached in practice; waiting forever is the honest fallback.
                std::future::pending::<()>().await;
            }
        }
    }
}

struct Begun(Arc<AtomicBool>);

impl Drop for Begun {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_codes_are_what_a_shell_reports() {
        assert_eq!(Request::Interrupt.exit_code(), 130);
        assert_eq!(Request::Terminate.exit_code(), 143);
        assert_eq!(Request::Hangup.exit_code(), 129);
    }
}
