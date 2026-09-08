//! PTY-backed process supervision.
//!
//! Children are spawned on a real pty rather than pipes. This is not a detail:
//! a child on a pipe sees `isatty() == false` and turns off color, switches to
//! block buffering, and hides progress bars. `cargo` in particular becomes
//! unrecognisable. Renting a pty per child costs a file descriptor and buys back
//! output that looks exactly like it does in your own terminal.
//!
//! The cost is that stdout and stderr are merged by the kernel and cannot be
//! separated again — an accepted trade, and the same one Overmind makes.

use anyhow::{Context, Result};
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};
use std::collections::VecDeque;
use std::io::Read;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{broadcast, mpsc};

static SEQ: AtomicU64 = AtomicU64::new(1);

/// Next line number. Shared with the engine so its own notices interleave with
/// child output in the order they actually happened.
pub fn next_seq() -> u64 {
    SEQ.fetch_add(1, Ordering::Relaxed)
}

#[derive(Debug, Clone)]
pub struct LogLine {
    pub seq: u64,
    pub proc: String,
    pub text: String,
    /// Terminated by `\r`, not `\n` — a progress update that should overwrite the
    /// previous line rather than accumulate. cargo emits thousands of these.
    pub transient: bool,
}

#[derive(Debug, Clone)]
pub enum ProcEvent {
    Started {
        proc: String,
        pid: Option<u32>,
    },
    Line(LogLine),
    Exited {
        proc: String,
        code: i32,
    },
    /// Spawn itself failed — a bad command, a missing binary, a bad cwd.
    Failed {
        proc: String,
        error: String,
    },
}

/// A running child plus the handles needed to stop it deterministically.
pub struct Handle {
    pub name: String,
    pub pid: Option<u32>,
    /// Windows job object owning the child and everything it spawns, stored as a
    /// raw `usize` because `HANDLE` is a pointer and therefore not `Send`.
    #[cfg(windows)]
    job: Option<usize>,
    child: Arc<Mutex<Box<dyn portable_pty::Child + Send + Sync>>>,
    /// Held so the master side stays open for the child's lifetime.
    ///
    /// Behind a mutex purely so `Handle` is `Sync`: `MasterPty` is `Send` but not
    /// `Sync`, and supervisors hold a `&Handle` across awaits inside `tokio::spawn`.
    master: Mutex<Box<dyn MasterPty + Send>>,
    /// The pty's input side. Taken once at spawn and held here, because
    /// `take_writer` can only succeed once.
    writer: Mutex<Option<Box<dyn std::io::Write + Send>>>,
    /// Raw output, before line splitting.
    ///
    /// The log store cannot serve an attached terminal: it holds *lines*, and a
    /// prompt like `Continue? [y/N] ` never ends one. Anyone attached needs the
    /// bytes as they arrive.
    raw: broadcast::Sender<Vec<u8>>,
    /// The tail of that raw stream, for a client that attaches after the fact.
    ///
    /// Broadcast only reaches subscribers who were already listening, so without
    /// this, attaching to a process sitting at a prompt shows a blank screen —
    /// the prompt was emitted before anyone was there to hear it, and it never
    /// ended a line so the log store does not have it either.
    scrollback: Arc<Mutex<VecDeque<u8>>>,
}

impl Handle {
    /// Ends the child and everything it spawned, giving it `grace` to exit first.
    ///
    /// Stopping the whole tree is what prevents orphans: `sh -c "cargo run"`
    /// spawns cargo which spawns your binary. Killing only the shell leaves the
    /// binary alive and holding the port — the classic "address already in use"
    /// on restart.
    ///
    /// Unix does this with process groups; Windows has no equivalent signal, so
    /// the tree is a job object and the escalation is a single terminate.
    pub async fn stop(&self, grace: Duration) -> Result<()> {
        #[cfg(unix)]
        if let Some(pid) = self.pid {
            unsafe {
                libc::killpg(pid as libc::pid_t, libc::SIGTERM);
            }
        }
        let deadline = tokio::time::Instant::now() + grace;
        loop {
            {
                let mut c = self.child.lock().unwrap();
                if c.try_wait().ok().flatten().is_some() {
                    return Ok(());
                }
            }
            if tokio::time::Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        #[cfg(unix)]
        if let Some(pid) = self.pid {
            unsafe {
                libc::killpg(pid as libc::pid_t, libc::SIGKILL);
            }
        }
        // Terminating the job takes the whole tree at once, which is the point:
        // `Child::kill` would only reach the shell.
        #[cfg(windows)]
        if let Some(job) = self.job {
            unsafe {
                windows_sys::Win32::System::JobObjects::TerminateJobObject(job as _, 1);
            }
        }
        let mut c = self.child.lock().unwrap();
        let _ = c.kill();
        Ok(())
    }

    /// Sends a signal to the child's process group without stopping it.
    ///
    /// For `on_change = "signal"`: processes that reload their own configuration
    /// want a nudge, not a restart.
    pub fn signal(&self, sig: i32) -> bool {
        #[cfg(unix)]
        if let Some(pid) = self.pid {
            return unsafe { libc::killpg(pid as libc::pid_t, sig) } == 0;
        }
        // Windows has no signals; a job object can only be terminated.
        let _ = sig;
        false
    }

    /// The most recent raw output, as a terminal would have shown it.
    pub fn scrollback(&self) -> Vec<u8> {
        self.scrollback.lock().unwrap().iter().copied().collect()
    }

    /// Raw output from now on. Lagging receivers lose the oldest chunks, which
    /// for a terminal is the right failure: catching up matters more than
    /// replaying scrollback nobody is looking at.
    pub fn subscribe_raw(&self) -> broadcast::Receiver<Vec<u8>> {
        self.raw.subscribe()
    }

    /// Writes to the child's stdin. `false` if the pty is gone.
    pub fn write_stdin(&self, bytes: &[u8]) -> bool {
        let mut guard = self.writer.lock().unwrap();
        let Some(w) = guard.as_mut() else {
            return false;
        };
        w.write_all(bytes).and_then(|()| w.flush()).is_ok()
    }

    /// Tells the child its window changed, so full-screen programs redraw.
    pub fn resize(&self, rows: u16, cols: u16) -> bool {
        self.master
            .lock()
            .unwrap()
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .is_ok()
    }

    pub fn try_exit_code(&self) -> Option<i32> {
        let mut c = self.child.lock().unwrap();
        c.try_wait().ok().flatten().map(|s| s.exit_code() as i32)
    }
}

/// Spawns `cmd` under `/bin/sh -c` on a fresh pty.
///
/// Going through a shell is deliberate: it gives Procfile-compatible semantics
/// (pipes, `&&`, env expansion) and it makes the child a process-group leader
/// under the pty, which is what makes group-signalling work.
pub fn spawn(
    name: &str,
    cmd: &str,
    cwd: &Path,
    env: &std::collections::BTreeMap<String, String>,
    events: mpsc::UnboundedSender<ProcEvent>,
) -> Result<Handle> {
    spawn_with(name, cmd, cwd, env, false, events)
}

/// Spawns `cmd` on a pty, optionally with the ambient environment removed.
///
/// `isolated` is how strict env mode is enforced: the child is given exactly the
/// map it is handed and nothing else, so a variable that was never declared
/// cannot reach the build and therefore cannot invalidate a cache key that does
/// not mention it.
pub fn spawn_with(
    name: &str,
    cmd: &str,
    cwd: &Path,
    env: &std::collections::BTreeMap<String, String>,
    isolated: bool,
    events: mpsc::UnboundedSender<ProcEvent>,
) -> Result<Handle> {
    let pty = native_pty_system();
    let pair = pty
        .openpty(PtySize {
            rows: 40,
            cols: 160,
            pixel_width: 0,
            pixel_height: 0,
        })
        .context("allocating pty")?;

    let mut builder = if cfg!(windows) {
        let mut b = CommandBuilder::new("cmd");
        b.arg("/C");
        b.arg(cmd);
        b
    } else {
        let mut b = CommandBuilder::new("/bin/sh");
        b.arg("-c");
        b.arg(cmd);
        b
    };
    if isolated {
        builder.env_clear();
    }
    builder.cwd(cwd);
    for (k, v) in env {
        builder.env(k, v);
    }
    // Children that respect these produce far better output under a supervisor.
    builder.env(
        "TERM",
        std::env::var("TERM").unwrap_or_else(|_| "xterm-256color".into()),
    );
    builder.env("FORCE_COLOR", "1");
    builder.env("CLICOLOR_FORCE", "1");
    builder.env("TURBORUST", "1");
    builder.env("TURBORUST_PROC", name);

    // The reader starts BEFORE the child, and the child is not spawned until the
    // reader thread is actually running. A command that writes and exits
    // immediately — a failing `cargo check`, an `echo` — can otherwise finish
    // while nothing is reading, and macOS discards the unread buffer when the
    // last slave fd closes. Losing exactly the output of the fastest failures is
    // the worst thing that can happen to a tool that exists to show you failures.
    //
    // Spawning the thread first is not enough on its own: thread startup can take
    // milliseconds under load, which is ample time for `echo` to run and exit.
    // The rendezvous below costs one context switch and removes the window.
    let reader = pair
        .master
        .try_clone_reader()
        .context("cloning pty reader")?;
    let writer = pair.master.take_writer().ok();
    let (raw, _) = broadcast::channel(RAW_BACKLOG);
    let scrollback = Arc::new(Mutex::new(VecDeque::with_capacity(SCROLLBACK_BYTES)));
    let tx = events.clone();
    let raw_tx = raw.clone();
    let scroll_tx = scrollback.clone();
    let pname = name.to_string();
    let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel::<()>(0);
    std::thread::Builder::new()
        .name(format!("pty-{name}"))
        .spawn(move || {
            // A zero-capacity send blocks until the parent receives, so returning
            // from `recv` below means this thread is on the next instruction.
            let _ = ready_tx.send(());
            pump(reader, &pname, tx, raw_tx, scroll_tx)
        })
        .context("spawning pty reader thread")?;
    let _ = ready_rx.recv();

    let child = pair
        .slave
        .spawn_command(builder)
        .with_context(|| format!("spawning `{cmd}` in {}", cwd.display()))?;
    let pid = child.process_id();
    // Released only after the child has inherited it, so the master sees EOF
    // when the child (and only the child) is done writing.
    drop(pair.slave);

    let _ = events.send(ProcEvent::Started {
        proc: name.to_string(),
        pid,
    });

    // Windows has no process groups. A job object is the equivalent container,
    // and KILL_ON_JOB_CLOSE means the tree cannot outlive this handle even if
    // turborust dies without stopping cleanly.
    #[cfg(windows)]
    let job = pid.and_then(create_job_for);

    let child = Arc::new(Mutex::new(child));
    let waiter = child.clone();
    let tx = events.clone();
    let pname = name.to_string();
    std::thread::Builder::new()
        .name(format!("wait-{name}"))
        .spawn(move || {
            // `wait()` holds the lock, so poll instead and leave `stop()` free to
            // take the lock and signal.
            let code = loop {
                {
                    let mut c = waiter.lock().unwrap();
                    if let Ok(Some(status)) = c.try_wait() {
                        break status.exit_code() as i32;
                    }
                }
                std::thread::sleep(Duration::from_millis(20));
            };
            let _ = tx.send(ProcEvent::Exited { proc: pname, code });
        })
        .context("spawning wait thread")?;

    Ok(Handle {
        name: name.to_string(),
        pid,
        #[cfg(windows)]
        job,
        child,
        master: Mutex::new(pair.master),
        writer: Mutex::new(writer),
        raw,
        scrollback,
    })
}

/// Creates a job object holding `pid` and everything it goes on to spawn.
///
/// Returns `None` on failure rather than erroring: losing the ability to kill
/// grandchildren is worth a leaked process, not a refusal to start.
#[cfg(windows)]
fn create_job_for(pid: u32) -> Option<usize> {
    use windows_sys::Win32::Foundation::{CloseHandle, FALSE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject,
    };
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE,
    };

    unsafe {
        let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        if job.is_null() {
            return None;
        }

        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let ok = SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &info as *const _ as *const std::ffi::c_void,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        );
        if ok == 0 {
            CloseHandle(job);
            return None;
        }

        let process = OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, FALSE, pid);
        if process.is_null() {
            CloseHandle(job);
            return None;
        }
        let assigned = AssignProcessToJobObject(job, process);
        CloseHandle(process);
        if assigned == 0 {
            CloseHandle(job);
            return None;
        }
        Some(job as usize)
    }
}

/// Chunks of raw output held for a client that has not read them yet.
const RAW_BACKLOG: usize = 512;

/// Raw bytes kept for a client that attaches later. Roughly a screenful of
/// scrollback; enough for context without holding a build's entire output twice.
const SCROLLBACK_BYTES: usize = 16 * 1024;

/// Reads the pty, emitting both raw chunks and split lines.
///
/// Two consumers with different needs: the log store wants lines, an attached
/// terminal wants bytes. Splitting once here is cheaper than reading twice, and
/// there is only one reader available anyway.
fn pump(
    mut reader: Box<dyn Read + Send>,
    name: &str,
    tx: mpsc::UnboundedSender<ProcEvent>,
    raw: broadcast::Sender<Vec<u8>>,
    scrollback: Arc<Mutex<VecDeque<u8>>>,
) {
    let mut buf = [0u8; 8192];
    let mut pending: Vec<u8> = Vec::with_capacity(256);
    loop {
        let n = match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(_) => break, // EIO on macOS when the child closes the slave: normal EOF
        };
        // Sending fails only when nobody is attached, which is the common case.
        let _ = raw.send(buf[..n].to_vec());
        {
            let mut back = scrollback.lock().unwrap();
            back.extend(&buf[..n]);
            let overflow = back.len().saturating_sub(SCROLLBACK_BYTES);
            back.drain(..overflow);
        }
        for &b in &buf[..n] {
            match b {
                b'\n' => {
                    emit(&mut pending, name, false, &tx);
                }
                b'\r' => {
                    // Could be a lone CR (progress) or the CR of a CRLF; either way
                    // flush now and let a following LF flush an empty line, which
                    // `emit` drops.
                    emit(&mut pending, name, true, &tx);
                }
                _ => pending.push(b),
            }
        }
    }
    if !pending.is_empty() {
        emit(&mut pending, name, false, &tx);
    }
}

fn emit(pending: &mut Vec<u8>, name: &str, transient: bool, tx: &mpsc::UnboundedSender<ProcEvent>) {
    if pending.is_empty() {
        return;
    }
    let text = String::from_utf8_lossy(pending).to_string();
    pending.clear();
    let _ = tx.send(ProcEvent::Line(LogLine {
        seq: SEQ.fetch_add(1, Ordering::Relaxed),
        proc: name.to_string(),
        text,
        transient,
    }));
}

/// Strips ANSI escape sequences. Used for log files and for width math.
pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        match chars.peek() {
            Some('[') => {
                chars.next();
                // CSI: parameters and intermediates, then a final byte in @-~.
                for c in chars.by_ref() {
                    if ('\u{40}'..='\u{7e}').contains(&c) {
                        break;
                    }
                }
            }
            Some(']') => {
                chars.next();
                // OSC: runs until BEL or ST.
                while let Some(c) = chars.next() {
                    if c == '\u{7}' {
                        break;
                    }
                    if c == '\u{1b}' && chars.peek() == Some(&'\\') {
                        chars.next();
                        break;
                    }
                }
            }
            _ => {
                chars.next();
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_color_codes() {
        assert_eq!(strip_ansi("\u{1b}[32mok\u{1b}[0m"), "ok");
        assert_eq!(strip_ansi("plain"), "plain");
        assert_eq!(strip_ansi("\u{1b}]0;title\u{7}body"), "body");
    }

    #[tokio::test]
    async fn captures_child_output_and_exit_code() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let h = spawn(
            "t",
            "echo hello; exit 3",
            Path::new("."),
            &Default::default(),
            tx,
        )
        .unwrap();
        let mut saw_line = false;
        let mut code = None;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        // The pty reader and the wait thread are independent, so `Exited` can
        // arrive while output is still in the pipe. Keep draining after it —
        // stopping at the exit event drops the tail and flakes.
        while tokio::time::Instant::now() < deadline && !(saw_line && code.is_some()) {
            match tokio::time::timeout(Duration::from_millis(200), rx.recv()).await {
                Ok(Some(ProcEvent::Line(l))) => {
                    if strip_ansi(&l.text).contains("hello") {
                        saw_line = true;
                    }
                }
                Ok(Some(ProcEvent::Exited { code: c, .. })) => code = Some(c),
                Ok(Some(_)) => {}
                Ok(None) => break,
                Err(_) => {}
            }
        }
        assert!(saw_line, "expected to capture child stdout");
        assert_eq!(code, Some(3));
        let _ = h.stop(Duration::from_millis(100)).await;
    }

    #[tokio::test]
    async fn stop_kills_the_whole_process_group() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        // The child announces itself before sleeping. Waiting for that line proves
        // it is actually running; asserting `try_exit_code().is_none()` straight
        // after spawn races the fork under load and flakes.
        let h = spawn(
            "t",
            "echo ready; sleep 300",
            Path::new("."),
            &Default::default(),
            tx,
        )
        .unwrap();

        let mut running = false;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while tokio::time::Instant::now() < deadline && !running {
            if let Ok(Some(ProcEvent::Line(l))) =
                tokio::time::timeout(Duration::from_millis(250), rx.recv()).await
                && strip_ansi(&l.text).contains("ready")
            {
                running = true;
            }
        }
        assert!(running, "child never started");
        assert!(
            h.try_exit_code().is_none(),
            "child should still be sleeping"
        );

        h.stop(Duration::from_millis(500)).await.unwrap();

        // Reaping is done by the wait thread, so give it a moment to observe.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while h.try_exit_code().is_none() && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(
            h.try_exit_code().is_some(),
            "child should be reaped after stop"
        );
    }
}
