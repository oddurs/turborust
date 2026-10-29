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
use tokio::sync::{broadcast, mpsc, watch};

static SEQ: AtomicU64 = AtomicU64::new(1);

/// Next line number. Shared with the engine so its own notices interleave with
/// child output in the order they actually happened.
pub fn next_seq() -> u64 {
    SEQ.fetch_add(1, Ordering::Relaxed)
}

#[derive(Debug, Clone)]
pub struct LogLine {
    pub seq: u64,
    /// Shared rather than owned: one allocation per process, not per line.
    pub proc: Arc<str>,
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
    /// Not an event: whoever drains the queue notifies this on reaching it,
    /// so a sender knows everything sent before it has been handled.
    Flush(Arc<tokio::sync::Notify>),
}

/// Children whose exit has not been reported yet. Reported means after the
/// last of its output: the report follows the reader's end of file.
static UNREPORTED: AtomicU64 = AtomicU64::new(0);

/// Waits, at most `within`, until every child that has exited has had its
/// output and its exit reported. The queue may still hold them; see
/// [`ProcEvent::Flush`].
pub async fn output_read(within: Duration) {
    let deadline = tokio::time::Instant::now() + within;
    while UNREPORTED.load(Ordering::Acquire) > 0 && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
}

/// How long an exit report waits for the rest of the output. A grandchild
/// that keeps the pty open would otherwise hold it for as long as it runs.
const OUTPUT_GRACE: Duration = Duration::from_millis(100);

/// Counts a child until its exit has been reported.
struct OpenReader;

impl OpenReader {
    fn new() -> Self {
        UNREPORTED.fetch_add(1, Ordering::AcqRel);
        OpenReader
    }
}

impl Drop for OpenReader {
    fn drop(&mut self) {
        UNREPORTED.fetch_sub(1, Ordering::AcqRel);
    }
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
    /// The exit code, once the child is seen to have gone.
    ///
    /// Something to await rather than poll: a supervisor polling every child
    /// every 20ms was most of an idle `up`'s CPU, and its exits were seen up
    /// to 20ms late.
    exit: watch::Receiver<Option<i32>>,
    /// Set once turborust has signalled the child itself, so its death by that
    /// signal is not reported as news.
    stopped: Arc<std::sync::atomic::AtomicBool>,
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
        self.stopped.store(true, Ordering::Release);
        #[cfg(unix)]
        if let Some(pid) = self.pid {
            unsafe {
                libc::killpg(pid as libc::pid_t, libc::SIGTERM);
            }
        }
        #[cfg(unix)]
        let deadline = tokio::time::Instant::now() + grace;
        if tokio::time::timeout(grace, self.exited()).await.is_err() {
            self.kill_now();
            return Ok(());
        }
        // The leader is gone; what it started may not be. A shell running
        // `server & worker; wait` dies of the SIGTERM at once, while the two it
        // started are still on their way out — or ignoring it. Stopped means
        // the whole group, so the rest of the grace is theirs, then the kill.
        #[cfg(unix)]
        if let Some(pid) = self.pid {
            while group_alive(pid as libc::pid_t) {
                if tokio::time::Instant::now() >= deadline {
                    self.kill_now();
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        }
        Ok(())
    }

    /// Resolves with the exit code once the child has exited.
    pub async fn exited(&self) -> i32 {
        let mut exit = self.exit.clone();
        let seen = exit.wait_for(Option::is_some).await.map(|code| *code);
        match seen {
            Ok(code) => code.unwrap_or(-1),
            // The wait thread is gone without a code: it could not have been
            // started, so ask the child directly.
            Err(_) => loop {
                if let Some(code) = self.try_exit_code() {
                    return code;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            },
        }
    }

    /// Ends the child and everything it spawned, without waiting for anything.
    ///
    /// The escalation at the end of `stop`, and on its own the answer to a
    /// second stop request: by then nobody is willing to wait out a grace
    /// period, and anything left alive would be an orphan.
    pub fn kill_now(&self) {
        self.stopped.store(true, Ordering::Release);
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

const NEXTEST_PROGRESS: &str = "NEXTEST_SHOW_PROGRESS";

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
    // The pty library falls back to the home directory when `cwd` does not
    // exist, so a typo in `cwd` ran the command somewhere else entirely — a
    // `rm -rf build` in $HOME. Checked here, at the last moment, because a
    // dependency may be what creates the directory (cairn 0086).
    if !cwd.is_dir() {
        anyhow::bail!("working directory `{}` does not exist", cwd.display());
    }
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
    // nextest's progress bar redraws in place, and a log does not: on a pty its
    // lines arrive run together, and no failure can be read out of them. The
    // counter keeps a line per test. Only a default; a choice made is kept.
    let chosen = env.contains_key(NEXTEST_PROGRESS)
        || (!isolated && std::env::var_os(NEXTEST_PROGRESS).is_some());
    if !chosen {
        builder.env(NEXTEST_PROGRESS, "counter");
    }

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
    // Closed when the reader is done: the exit report waits for it, so a
    // child's exit never overtakes the end of its output. Grouped output is
    // printed at the exit, and lost whatever arrived after it.
    let (eof_async, mut eof_async_rx) = watch::channel(());
    let (eof_sync, eof_sync_rx) = std::sync::mpsc::channel::<()>();
    let unreported = OpenReader::new();
    std::thread::Builder::new()
        .name(format!("pty-{name}"))
        .spawn(move || {
            let _done = (eof_async, eof_sync);
            // A zero-capacity send blocks until the parent receives, so returning
            // from `recv` below means this thread is on the next instruction.
            let _ = ready_tx.send(());
            pump(reader, &Arc::from(pname.as_str()), tx, raw_tx, scroll_tx)
        })
        .context("spawning pty reader thread")?;
    let _ = ready_rx.recv();

    // On Linux, without forking a copy of turborust (cairn 0112); anywhere
    // that cannot be done, as portable_pty does it.
    #[cfg(target_os = "linux")]
    let spawned = fast_spawn::spawn(&builder, isolated, cwd, pair.master.tty_name().as_deref())
        .map(|c| Box::new(c) as Box<dyn portable_pty::Child + Send + Sync>);
    #[cfg(not(target_os = "linux"))]
    let spawned: std::io::Result<Box<dyn portable_pty::Child + Send + Sync>> =
        Err(std::io::ErrorKind::Unsupported.into());
    let child = match spawned {
        Ok(child) => child,
        Err(_) => pair
            .slave
            .spawn_command(builder)
            .with_context(|| format!("spawning `{cmd}` in {}", cwd.display()))?,
    };
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
    let (exit_tx, exit) = watch::channel(None);
    // Reaps the child once it has exited. On unix this succeeds first time;
    // elsewhere, and if no notification could be set up, it polls.
    let reap = move || loop {
        {
            let mut c = waiter.lock().unwrap();
            if let Ok(Some(status)) = c.try_wait() {
                break (
                    status.exit_code() as i32,
                    status.signal().map(str::to_string),
                );
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    // The supervisor hears at once; the report waits for the output.
    let stopped = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let we_stopped = stopped.clone();
    let line_name: Arc<str> = name.into();
    let report = move |(code, signal): (i32, Option<String>)| {
        // A signal turborust did not send is the one thing an exit code of 1
        // cannot tell apart from a failure, so it is said.
        if let Some(signal) = signal
            && !we_stopped.load(Ordering::Acquire)
        {
            let _ = tx.send(ProcEvent::Line(LogLine {
                seq: next_seq(),
                proc: line_name,
                text: format!("\u{1b}[2m[turborust]\u{1b}[0m killed by a signal: {signal}"),
                transient: false,
            }));
        }
        let _ = tx.send(ProcEvent::Exited { proc: pname, code });
        drop(unreported);
    };
    match exit_notifier(pid) {
        // Linux: the kernel says when it has exited, and no thread waits for it.
        // A thread per child was half of turborust's threads, and every thread
        // makes the next fork slower.
        Some(exited) => {
            drop(eof_sync_rx);
            tokio::spawn(async move {
                exited.await;
                let status = reap();
                let _ = exit_tx.send(Some(status.0));
                let _ = tokio::time::timeout(OUTPUT_GRACE, eof_async_rx.changed()).await;
                report(status);
            });
        }
        None => {
            std::thread::Builder::new()
                .name(format!("wait-{name}"))
                .spawn(move || {
                    // Blocks until the child has exited without reaping it, so
                    // the lock `reap` takes is free the whole time for `stop()`
                    // to signal through.
                    #[cfg(unix)]
                    if let Some(pid) = pid {
                        wait_without_reaping(pid);
                    }
                    let status = reap();
                    let _ = exit_tx.send(Some(status.0));
                    let _ = eof_sync_rx.recv_timeout(OUTPUT_GRACE);
                    report(status);
                })
                .context("spawning wait thread")?;
        }
    }

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
        exit,
        stopped,
    })
}

/// Starting a child with `posix_spawn` rather than `fork`.
///
/// A `fork` copies the parent's page tables, and portable_pty's `pre_exec`
/// hook — which forces one — then lists and closes every open descriptor in
/// the child. Both grow with the number of children already running, so the
/// fiftieth spawn cost three times the first. glibc and musl implement
/// `posix_spawn` with `CLONE_VM | CLONE_VFORK`: nothing is copied, and the
/// parent waits only until the child has exec'd, by which time it holds the
/// pty.
///
/// Everything portable_pty's hook does has an attribute here: the session
/// (`POSIX_SPAWN_SETSID`), the controlling terminal (a session leader that
/// opens a tty without `O_NOCTTY` acquires it, on Linux), default signal
/// dispositions and an empty mask. Closing descriptors needs nothing: every one
/// turborust opens is close-on-exec. The environment is the builder's own,
/// so the child sees exactly what portable_pty would have given it.
#[cfg(target_os = "linux")]
mod fast_spawn {
    use portable_pty::{Child, ChildKiller, CommandBuilder, ExitStatus};
    use std::ffi::CString;
    use std::io;
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;

    #[derive(Debug)]
    pub struct Spawned {
        pid: libc::pid_t,
        status: Option<ExitStatus>,
    }

    /// `Unsupported` for anything this cannot reproduce exactly, which sends
    /// the caller to portable_pty instead.
    pub fn spawn(
        builder: &CommandBuilder,
        isolated: bool,
        cwd: &Path,
        tty: Option<&Path>,
    ) -> io::Result<Spawned> {
        let unsupported = || io::Error::from(io::ErrorKind::Unsupported);
        let tty = tty.ok_or_else(unsupported)?;
        // The builder can only list its environment as UTF-8; a variable that
        // is not would be silently lost.
        if !isolated
            && std::env::vars_os().any(|(k, v)| k.to_str().is_none() || v.to_str().is_none())
        {
            return Err(unsupported());
        }
        let argv = builder.get_argv();
        if argv.is_empty() || !Path::new(&argv[0]).is_absolute() {
            return Err(unsupported());
        }
        let argv: Vec<CString> = argv
            .iter()
            .map(|a| CString::new(a.as_bytes()))
            .collect::<Result<_, _>>()?;
        // As portable_pty: the builder's variables, and SHELL from its own rule.
        let mut env: Vec<CString> = builder
            .iter_full_env_as_str()
            .filter(|(k, _)| *k != "SHELL")
            .map(|(k, v)| CString::new(format!("{k}={v}")))
            .collect::<Result<_, _>>()?;
        env.push(CString::new(format!("SHELL={}", builder.get_shell()))?);
        let tty = CString::new(tty.as_os_str().as_bytes())?;
        let cwd = CString::new(cwd.as_os_str().as_bytes())?;

        let mut argv_ptrs: Vec<*mut libc::c_char> =
            argv.iter().map(|a| a.as_ptr() as *mut _).collect();
        argv_ptrs.push(std::ptr::null_mut());
        let mut env_ptrs: Vec<*mut libc::c_char> =
            env.iter().map(|e| e.as_ptr() as *mut _).collect();
        env_ptrs.push(std::ptr::null_mut());

        // SAFETY: every pointer handed over outlives the call, the attribute
        // and file-action objects are initialised before use and destroyed
        // after, and posix_spawn writes only `pid`.
        unsafe {
            let mut actions: libc::posix_spawn_file_actions_t = std::mem::zeroed();
            let mut attr: libc::posix_spawnattr_t = std::mem::zeroed();
            check(libc::posix_spawn_file_actions_init(&mut actions))?;
            if let Err(e) = check(libc::posix_spawnattr_init(&mut attr)) {
                libc::posix_spawn_file_actions_destroy(&mut actions);
                return Err(e);
            }
            let result = (|| -> io::Result<libc::pid_t> {
                check(libc::posix_spawn_file_actions_addchdir_np(
                    &mut actions,
                    cwd.as_ptr(),
                ))?;
                check(libc::posix_spawn_file_actions_addopen(
                    &mut actions,
                    0,
                    tty.as_ptr(),
                    libc::O_RDWR,
                    0,
                ))?;
                check(libc::posix_spawn_file_actions_adddup2(&mut actions, 0, 1))?;
                check(libc::posix_spawn_file_actions_adddup2(&mut actions, 0, 2))?;
                let mut defaults: libc::sigset_t = std::mem::zeroed();
                libc::sigfillset(&mut defaults);
                libc::sigdelset(&mut defaults, libc::SIGKILL);
                libc::sigdelset(&mut defaults, libc::SIGSTOP);
                check(libc::posix_spawnattr_setsigdefault(&mut attr, &defaults))?;
                let mut mask: libc::sigset_t = std::mem::zeroed();
                libc::sigemptyset(&mut mask);
                check(libc::posix_spawnattr_setsigmask(&mut attr, &mask))?;
                let flags = libc::POSIX_SPAWN_SETSID
                    | libc::POSIX_SPAWN_SETSIGDEF as libc::c_short
                    | libc::POSIX_SPAWN_SETSIGMASK as libc::c_short;
                check(libc::posix_spawnattr_setflags(&mut attr, flags))?;
                let mut pid: libc::pid_t = 0;
                check(libc::posix_spawn(
                    &mut pid,
                    argv[0].as_ptr(),
                    &actions,
                    &attr,
                    argv_ptrs.as_ptr(),
                    env_ptrs.as_ptr(),
                ))?;
                Ok(pid)
            })();
            libc::posix_spawn_file_actions_destroy(&mut actions);
            libc::posix_spawnattr_destroy(&mut attr);
            Ok(Spawned {
                pid: result?,
                status: None,
            })
        }
    }

    /// posix_spawn's functions return an error number rather than set errno.
    fn check(code: libc::c_int) -> io::Result<()> {
        if code == 0 {
            Ok(())
        } else {
            Err(io::Error::from_raw_os_error(code))
        }
    }

    /// The status as portable_pty reports it: a signal is code 1, named.
    fn decode(raw: libc::c_int) -> ExitStatus {
        if libc::WIFSIGNALED(raw) {
            let sig = libc::WTERMSIG(raw);
            let name = unsafe { libc::strsignal(sig) };
            let name = if name.is_null() {
                format!("Signal {sig}")
            } else {
                unsafe { std::ffi::CStr::from_ptr(name) }
                    .to_string_lossy()
                    .into_owned()
            };
            ExitStatus::with_signal(&name)
        } else {
            ExitStatus::with_exit_code(libc::WEXITSTATUS(raw) as u32)
        }
    }

    impl Child for Spawned {
        fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
            if let Some(s) = &self.status {
                return Ok(Some(s.clone()));
            }
            let mut raw = 0;
            match unsafe { libc::waitpid(self.pid, &mut raw, libc::WNOHANG) } {
                0 => Ok(None),
                -1 => Err(io::Error::last_os_error()),
                _ => {
                    let s = decode(raw);
                    self.status = Some(s.clone());
                    Ok(Some(s))
                }
            }
        }

        fn wait(&mut self) -> io::Result<ExitStatus> {
            loop {
                if let Some(s) = &self.status {
                    return Ok(s.clone());
                }
                let mut raw = 0;
                if unsafe { libc::waitpid(self.pid, &mut raw, 0) } == -1 {
                    let e = io::Error::last_os_error();
                    if e.kind() != io::ErrorKind::Interrupted {
                        return Err(e);
                    }
                    continue;
                }
                self.status = Some(decode(raw));
            }
        }

        fn process_id(&self) -> Option<u32> {
            Some(self.pid as u32)
        }
    }

    impl ChildKiller for Spawned {
        fn kill(&mut self) -> io::Result<()> {
            // Once reaped, the pid may already be someone else's.
            if self.status.is_none() {
                unsafe { libc::kill(self.pid, libc::SIGKILL) };
            }
            Ok(())
        }

        fn clone_killer(&self) -> Box<dyn ChildKiller + Send + Sync> {
            Box::new(Killer(self.pid))
        }
    }

    #[derive(Debug)]
    struct Killer(libc::pid_t);

    impl ChildKiller for Killer {
        fn kill(&mut self) -> io::Result<()> {
            unsafe { libc::kill(self.0, libc::SIGKILL) };
            Ok(())
        }

        fn clone_killer(&self) -> Box<dyn ChildKiller + Send + Sync> {
            Box::new(Killer(self.0))
        }
    }
}

/// Resolves once `pid` has exited, without a thread: a pidfd turns readable
/// when its process ends. `None` where there is no pidfd (before Linux 5.3,
/// other systems) or no runtime to register it with.
#[cfg(target_os = "linux")]
fn exit_notifier(pid: Option<u32>) -> Option<impl std::future::Future<Output = ()> + Send> {
    use std::os::fd::{FromRawFd, OwnedFd};
    tokio::runtime::Handle::try_current().ok()?;
    let raw = unsafe { libc::syscall(libc::SYS_pidfd_open, pid? as libc::pid_t, 0) };
    if raw < 0 {
        return None;
    }
    let fd = unsafe { OwnedFd::from_raw_fd(raw as i32) };
    // SAFETY: the `OwnedFd` is moved into the `AsyncFd`, so the descriptor
    // stays open, and stays this pidfd, for as long as it is registered.
    let fd = unsafe {
        tokio::io::unix::AsyncFd::register_with_interest(fd, tokio::io::Interest::READABLE)
    }
    .ok()?;
    Some(async move {
        let _ = fd.readable().await;
    })
}

#[cfg(not(target_os = "linux"))]
fn exit_notifier(_pid: Option<u32>) -> Option<std::future::Ready<()>> {
    None
}

/// Whether anything in process group `pgid` is still running.
///
/// Zombies do not count: one waiting for a parent that never reaps it — PID 1
/// in many containers — is not running, and counting it would make every stop
/// wait out its whole grace period.
#[cfg(unix)]
fn group_alive(pgid: libc::pid_t) -> bool {
    // No member at all, zombies included: the usual answer, and the cheap one.
    if unsafe { libc::killpg(pgid, 0) } != 0 {
        return false;
    }
    #[cfg(target_os = "linux")]
    if let Ok(dir) = std::fs::read_dir("/proc") {
        return dir.flatten().any(|entry| {
            let name = entry.file_name();
            let Some(pid) = name
                .to_str()
                .filter(|n| n.bytes().all(|b| b.is_ascii_digit()))
            else {
                return false;
            };
            let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
                return false;
            };
            // `pid (comm) state ppid pgrp ...`; comm may hold spaces and parens.
            let Some(rest) = stat.rfind(')').map(|i| &stat[i + 1..]) else {
                return false;
            };
            let mut fields = rest.split_whitespace();
            let state = fields.next();
            let pgrp = fields.nth(1).and_then(|g| g.parse::<libc::pid_t>().ok());
            state != Some("Z") && pgrp == Some(pgid)
        });
    }
    unsafe { libc::killpg(pgid, 0) == 0 }
}

/// Blocks until `pid` has exited, leaving it a zombie for its owner to reap.
///
/// `WNOWAIT` is what makes this safe beside `portable_pty`'s own `try_wait`:
/// reaping here would leave that handle unable to report the status.
#[cfg(unix)]
fn wait_without_reaping(pid: u32) {
    loop {
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let r = unsafe {
            libc::waitid(
                libc::P_PID,
                pid as libc::id_t,
                &mut info,
                libc::WEXITED | libc::WNOWAIT,
            )
        };
        if r == 0 || std::io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
            return;
        }
    }
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
    name: &Arc<str>,
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
        // Copied only for someone attached, which is the uncommon case.
        if raw.receiver_count() > 0 {
            let _ = raw.send(buf[..n].to_vec());
        }
        {
            let mut back = scrollback.lock().unwrap();
            back.extend(&buf[..n]);
            let overflow = back.len().saturating_sub(SCROLLBACK_BYTES);
            back.drain(..overflow);
        }
        let mut rest = &buf[..n];
        while !rest.is_empty() {
            let end = rest.iter().position(|&b| b == b'\n' || b == b'\r');
            let (run, stop) = match end {
                Some(i) => (&rest[..i], Some(rest[i])),
                None => (rest, None),
            };
            pending.extend_from_slice(run);
            // A line with no end in sight is cut rather than held: one `cat` of
            // a minified bundle would otherwise sit in memory, then in the log
            // store, whole. Cut only once strictly longer, so the byte after
            // the cut is there to say whether it would split a character.
            while pending.len() > MAX_LINE {
                let cut = char_boundary(&pending, MAX_LINE);
                let tail = pending.split_off(cut);
                emit(&mut pending, name, false, &tx);
                pending = tail;
            }
            match stop {
                Some(b'\n') => emit(&mut pending, name, false, &tx),
                // Could be a lone CR (progress) or the CR of a CRLF; either way
                // flush now and let a following LF flush an empty line, which
                // `emit` drops.
                Some(_) => emit(&mut pending, name, true, &tx),
                None => {}
            }
            rest = match end {
                Some(i) => &rest[i + 1..],
                None => &[],
            };
        }
    }
    if !pending.is_empty() {
        emit(&mut pending, name, false, &tx);
    }
}

/// The longest line kept whole. Longer ones arrive as several lines.
const MAX_LINE: usize = 64 * 1024;

/// The last char boundary at or before `at`, so a cut never splits a
/// character into two replacement marks. `at` must be inside `bytes`.
fn char_boundary(bytes: &[u8], at: usize) -> usize {
    let mut i = at;
    // A continuation byte is 0b10xxxxxx; a character is at most four bytes.
    while i > 0 && at - i < 4 && bytes[i] & 0xC0 == 0x80 {
        i -= 1;
    }
    if i == 0 { at } else { i }
}

fn emit(
    pending: &mut Vec<u8>,
    name: &Arc<str>,
    transient: bool,
    tx: &mpsc::UnboundedSender<ProcEvent>,
) {
    if pending.is_empty() {
        return;
    }
    // One allocation for valid UTF-8, which is nearly all output.
    let text = match std::str::from_utf8(pending) {
        Ok(text) => text.to_owned(),
        Err(_) => String::from_utf8_lossy(pending).into_owned(),
    };
    pending.clear();
    let _ = tx.send(ProcEvent::Line(LogLine {
        seq: SEQ.fetch_add(1, Ordering::Relaxed),
        proc: name.clone(),
        text,
        transient,
    }));
}

/// Strips ANSI escape sequences. Used for log files and for width math.
///
/// Borrowed when there is nothing to strip, which is most lines: this runs on
/// every line of output, and copying each one was a measurable share of it.
pub fn strip_ansi(s: &str) -> std::borrow::Cow<'_, str> {
    if !s.contains('\u{1b}') {
        return std::borrow::Cow::Borrowed(s);
    }
    std::borrow::Cow::Owned(strip_escapes(s))
}

fn strip_escapes(s: &str) -> String {
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

    fn lines_of(input: Vec<u8>) -> Vec<(String, bool)> {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let (raw, _) = broadcast::channel(4);
        let back = Arc::new(Mutex::new(VecDeque::new()));
        pump(
            Box::new(std::io::Cursor::new(input)),
            &"t".into(),
            tx,
            raw,
            back,
        );
        let mut out = Vec::new();
        while let Ok(ProcEvent::Line(l)) = rx.try_recv() {
            out.push((l.text, l.transient));
        }
        out
    }

    #[test]
    fn output_splits_on_newlines_and_carriage_returns() {
        let got = lines_of(b"one\r\ntwo\n50%\r100%\rdone\ntail".to_vec());
        let want = [
            ("one", true),
            ("two", false),
            ("50%", true),
            ("100%", true),
            ("done", false),
            ("tail", false),
        ];
        assert_eq!(
            got,
            want.iter()
                .map(|(t, r)| (t.to_string(), *r))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_line_with_no_end_is_cut_not_held() {
        let got = lines_of(vec![b'x'; MAX_LINE * 3 + 10]);
        let lens: Vec<usize> = got.iter().map(|(t, _)| t.len()).collect();
        assert_eq!(lens, vec![MAX_LINE, MAX_LINE, MAX_LINE, 10]);
        let exact = lines_of(vec![b'x'; MAX_LINE * 2]);
        assert_eq!(exact.len(), 2, "an exact multiple leaves no empty tail");
    }

    #[test]
    fn a_cut_never_splits_a_character() {
        // Two-byte characters straddling every cut point.
        let mut input = vec![b'x'];
        input.extend("é".repeat(MAX_LINE).as_bytes());
        let got = lines_of(input);
        for (i, (t, _)) in got.iter().enumerate() {
            assert!(
                !t.contains('\u{fffd}'),
                "line {i} of {}, {} bytes, at {:?}",
                got.len(),
                t.len(),
                t.find('\u{fffd}')
            );
        }
        let total: usize = got.iter().map(|(t, _)| t.len()).sum();
        assert_eq!(total, 1 + 2 * MAX_LINE);
    }

    #[test]
    fn a_line_of_exactly_the_limit_is_one_line() {
        let mut input = vec![b'y'; MAX_LINE];
        input.push(b'\n');
        assert_eq!(lines_of(input).len(), 1);
    }

    /// Children run through the platform shell, so test fixtures have to be
    /// written in it. `cmd.exe` separates commands with `&`, not `;`, and has no
    /// `sleep` — skipping these on Windows would leave the job-object teardown,
    /// the one piece of platform-specific process code here, untested.
    fn echo_then_exit(word: &str, code: i32) -> String {
        if cfg!(windows) {
            format!("echo {word} & exit {code}")
        } else {
            format!("echo {word}; exit {code}")
        }
    }

    /// Announces itself, then stays alive until it is stopped.
    fn announce_then_wait(word: &str) -> String {
        if cfg!(windows) {
            // `ping` rather than `timeout`, which refuses to run when its input
            // is redirected — which it is, under a pty.
            format!("echo {word} & ping -n 301 127.0.0.1 > nul")
        } else {
            format!("echo {word}; sleep 300")
        }
    }

    #[test]
    fn strips_color_codes() {
        assert_eq!(strip_ansi("\u{1b}[32mok\u{1b}[0m"), "ok");
        assert_eq!(strip_ansi("plain"), "plain");
        assert_eq!(strip_ansi("\u{1b}]0;title\u{7}body"), "body");
    }

    // Unix only until someone can debug pty capture on Windows directly: both
    // of these see no child output at all there, and blind iteration against CI
    // has not found why. Everything else in the suite passes on Windows. See 0062.
    #[cfg(unix)]
    #[tokio::test]
    async fn captures_child_output_and_exit_code() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let h = spawn(
            "t",
            &echo_then_exit("hello", 3),
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

    #[cfg(unix)]
    #[tokio::test]
    async fn nextest_is_told_to_count_unless_told_otherwise() {
        async fn progress(env: &[(&str, &str)]) -> String {
            let env = env
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
            let (tx, mut rx) = mpsc::unbounded_channel();
            // Isolated, so the environment running this test cannot answer.
            let h = spawn_with(
                "t",
                "echo \"progress=$NEXTEST_SHOW_PROGRESS.\"",
                Path::new("."),
                &env,
                true,
                tx,
            )
            .unwrap();
            let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
            let mut said = String::new();
            while tokio::time::Instant::now() < deadline && !said.contains('.') {
                if let Ok(Some(ProcEvent::Line(l))) =
                    tokio::time::timeout(Duration::from_millis(200), rx.recv()).await
                {
                    said = strip_ansi(&l.text).into_owned();
                }
            }
            let _ = h.stop(Duration::from_millis(100)).await;
            said
        }
        assert_eq!(progress(&[]).await, "progress=counter.");
        assert_eq!(
            progress(&[("NEXTEST_SHOW_PROGRESS", "bar")]).await,
            "progress=bar."
        );
    }

    // Unix only until someone can debug pty capture on Windows directly: both
    // of these see no child output at all there, and blind iteration against CI
    // has not found why. Everything else in the suite passes on Windows. See 0062.
    /// Spawns `cmd` and waits for each of `words` in its output, in order,
    /// running `between` after each but the last.
    #[cfg(unix)]
    async fn expect_in_order(cmd: &str, words: &[&str], mut between: impl FnMut(&Handle, usize)) {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let h = spawn("t", cmd, Path::new("."), &Default::default(), tx).unwrap();
        let mut seen = String::new();
        for (i, word) in words.iter().enumerate() {
            let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
            loop {
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "never saw {word:?}; output was {seen:?}"
                );
                if let Ok(Some(ProcEvent::Line(l))) =
                    tokio::time::timeout(Duration::from_millis(250), rx.recv()).await
                {
                    let text = strip_ansi(&l.text).into_owned();
                    seen.push_str(&text);
                    seen.push('\n');
                    if text.contains(word) {
                        break;
                    }
                }
            }
            if i + 1 < words.len() {
                between(&h, i);
            }
        }
        h.kill_now();
    }

    /// The fast path is taken, not fallen back from: the terminal tests here
    /// pass either way, so they cannot tell.
    #[cfg(target_os = "linux")]
    #[test]
    fn linux_spawns_without_forking() {
        use portable_pty::Child as _;
        let pair = native_pty_system().openpty(PtySize::default()).unwrap();
        let mut b = CommandBuilder::new("/bin/sh");
        b.arg("-c");
        b.arg("exit 7");
        let mut child = fast_spawn::spawn(
            &b,
            false,
            &std::env::temp_dir(),
            pair.master.tty_name().as_deref(),
        )
        .expect("posix_spawn path");
        drop(pair.slave);
        assert_eq!(child.wait().unwrap().exit_code(), 7);
    }

    /// A child's exit is reported after the last of its output, never before:
    /// grouped output is printed at the exit, and lost whatever came after.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    async fn an_exit_is_reported_after_all_of_the_output() {
        for _ in 0..5 {
            let (tx, mut rx) = mpsc::unbounded_channel();
            let _h = spawn("t", "seq 1 20000", Path::new("."), &Default::default(), tx).unwrap();
            let mut lines = 0;
            loop {
                match tokio::time::timeout(Duration::from_secs(10), rx.recv()).await {
                    Ok(Some(ProcEvent::Line(_))) => lines += 1,
                    Ok(Some(ProcEvent::Exited { .. })) => break,
                    Ok(Some(_)) => {}
                    other => panic!("no exit reported: {other:?}"),
                }
            }
            assert_eq!(lines, 20000, "the exit overtook the output");
        }
    }

    /// A death by a signal turborust did not send is said, since its exit
    /// code alone reads as an ordinary failure; one turborust sent is not.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_signal_from_outside_is_reported_and_our_own_stop_is_not() {
        async fn lines_until_exit(rx: &mut mpsc::UnboundedReceiver<ProcEvent>) -> Vec<String> {
            let mut out = Vec::new();
            loop {
                match tokio::time::timeout(Duration::from_secs(10), rx.recv()).await {
                    Ok(Some(ProcEvent::Line(l))) => out.push(strip_ansi(&l.text).into_owned()),
                    Ok(Some(ProcEvent::Exited { .. })) => return out,
                    Ok(Some(_)) => {}
                    other => panic!("no exit: {other:?}"),
                }
            }
        }
        let (tx, mut rx) = mpsc::unbounded_channel();
        let h = spawn(
            "t",
            "exec sleep 30",
            Path::new("."),
            &Default::default(),
            tx,
        )
        .unwrap();
        unsafe { libc::kill(h.pid.unwrap() as libc::pid_t, libc::SIGKILL) };
        let lines = lines_until_exit(&mut rx).await;
        assert!(
            lines.iter().any(|l| l.contains("killed by a signal")),
            "{lines:?}"
        );

        let (tx, mut rx) = mpsc::unbounded_channel();
        let h = spawn(
            "t",
            "exec sleep 30",
            Path::new("."),
            &Default::default(),
            tx,
        )
        .unwrap();
        h.stop(Duration::from_secs(2)).await.unwrap();
        let lines = lines_until_exit(&mut rx).await;
        assert!(!lines.iter().any(|l| l.contains("killed")), "{lines:?}");
    }

    /// What `connect` relies on: ctrl-c typed into the pty reaches the child as
    /// SIGINT, which needs the pty to be its controlling terminal.
    #[cfg(unix)]
    #[tokio::test]
    async fn ctrl_c_through_the_pty_interrupts_the_child() {
        expect_in_order(
            "trap 'echo got-int; exit 0' INT; echo ready; while :; do sleep 0.05; done",
            &["ready", "got-int"],
            |h, _| assert!(h.write_stdin(&[0x03])),
        )
        .await;
    }

    /// A resize tells full-screen programs to redraw, by SIGWINCH to the
    /// terminal's foreground group.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_resize_reaches_the_child_as_sigwinch() {
        expect_in_order(
            "trap 'echo got-winch' WINCH; echo ready; while :; do sleep 0.05; done",
            &["ready", "got-winch"],
            |h, _| assert!(h.resize(50, 100)),
        )
        .await;
    }

    /// The child leads its own session — what lets the whole group be
    /// signalled — and its stdin is the terminal.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn the_child_leads_a_session_on_its_terminal() {
        expect_in_order(
            "[ -t 0 ] && [ -t 1 ] && echo is-a-tty; \
             awk '{ print ($6 == $1 && $7 != 0) ? \"session-leader-with-tty\" : \"no\" }' /proc/$$/stat; \
             sleep 5",
            &["is-a-tty", "session-leader-with-tty"],
            |_, _| {},
        )
        .await;
    }

    /// The leader dying is not the group stopping: here the shell dies of the
    /// SIGTERM at once, and the child it started takes 300ms to finish — after
    /// the hangup the leader's exit sends it, too. `stop` used to return with
    /// that child still running.
    #[cfg(unix)]
    #[tokio::test]
    async fn stop_returns_only_when_the_whole_group_is_gone() {
        let dir = std::env::temp_dir().join(format!("tr-group-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let h = spawn(
            "t",
            "sh -c 'trap \"sleep 0.3; exit 0\" TERM HUP; echo $$ > child; echo ready; \
             while :; do sleep 0.05; done' & wait",
            &dir,
            &Default::default(),
            tx,
        )
        .unwrap();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        let mut ready = false;
        while !ready && tokio::time::Instant::now() < deadline {
            if let Ok(Some(ProcEvent::Line(l))) =
                tokio::time::timeout(Duration::from_millis(250), rx.recv()).await
            {
                ready = strip_ansi(&l.text).contains("ready");
            }
        }
        assert!(ready, "child never started");
        let child: libc::pid_t = std::fs::read_to_string(dir.join("child"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();

        h.stop(Duration::from_secs(5)).await.unwrap();
        assert!(
            !group_alive(h.pid.unwrap() as libc::pid_t),
            "stop returned while child {child} was still running"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn stop_kills_the_whole_process_group() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        // The child announces itself before sleeping. Waiting for that line proves
        // it is actually running; asserting `try_exit_code().is_none()` straight
        // after spawn races the fork under load and flakes.
        let h = spawn(
            "t",
            &announce_then_wait("ready"),
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
