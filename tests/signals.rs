//! Stopping turborust from outside: the real binary, real signals.
//!
//! Regression cover for cairn 0085. Only ctrl-c on `up` used to shut down
//! cleanly; SIGTERM (`docker stop`, systemd, CI cancellation) and SIGHUP (a
//! closed terminal) killed turborust outright, and SIGINT did the same to `run`.
//! A child that ignores SIGHUP survived all of them, holding its port.
//!
//! Every child here ignores SIGHUP, because that is the case that orphans: an
//! ordinary child dies of the pty hangup whether or not turborust shuts down
//! properly, which would let these tests pass against the bug.
//!
//! Unix only: these are POSIX signals and POSIX shell fixtures.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

struct Workspace {
    dir: PathBuf,
}

impl Workspace {
    fn new(tag: &str, toml_src: &str) -> Workspace {
        let dir = std::env::temp_dir().join(format!("turborust-sig-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let dir = dir.canonicalize().unwrap();
        std::fs::write(dir.join("turborust.toml"), toml_src).unwrap();
        Workspace { dir }
    }

    /// The binary, with the variables a git hook exports stripped. Run from the
    /// pre-push hook without this, turborust would resolve its cache store from
    /// `GIT_DIR` — turborust's own repository — rather than this directory.
    /// Same hazard as cairn 0083.
    fn turborust(&self, args: &[&str]) -> Child {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_turborust"));
        cmd.args(args)
            .current_dir(&self.dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        for var in [
            "GIT_DIR",
            "GIT_WORK_TREE",
            "GIT_INDEX_FILE",
            "GIT_COMMON_DIR",
            "GIT_OBJECT_DIRECTORY",
            "GIT_ALTERNATE_OBJECT_DIRECTORIES",
            "GIT_PREFIX",
        ] {
            cmd.env_remove(var);
        }
        cmd.spawn().expect("turborust starts")
    }

    /// The pid a fixture wrote, once it has. The fixture `exec`s, so this is
    /// the long-lived process itself rather than a shell in front of it.
    fn child_pid(&self, file: &str) -> u32 {
        let path = self.dir.join(file);
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if let Ok(s) = std::fs::read_to_string(&path)
                && let Ok(pid) = s.trim().parse()
            {
                // Give the `exec` a moment to replace the shell.
                std::thread::sleep(Duration::from_millis(300));
                return pid;
            }
            assert!(Instant::now() < deadline, "the child never started");
            std::thread::sleep(Duration::from_millis(25));
        }
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Running and not a zombie. An orphan reparented to a PID 1 that does not reap
/// — common in containers — lingers as a zombie, and `kill(pid, 0)` still
/// succeeds on one; it holds nothing, so it does not count.
fn alive(pid: u32) -> bool {
    let out = Command::new("ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output()
        .expect("ps runs");
    let stat = String::from_utf8_lossy(&out.stdout);
    let stat = stat.trim();
    !stat.is_empty() && !stat.starts_with('Z')
}

/// Waits for `pid` to be gone, giving up after `within`.
fn gone_within(pid: u32, within: Duration) -> bool {
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        if !alive(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    !alive(pid)
}

fn send(child: &Child, sig: i32) {
    assert_eq!(
        unsafe { libc::kill(child.id() as i32, sig) },
        0,
        "kill failed"
    );
}

fn wait(child: &mut Child, within: Duration) -> Option<ExitStatus> {
    let deadline = Instant::now() + within;
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return Some(status);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// Kills whatever a failed assertion left behind, so one failure does not
/// leave a process running for the rest of the suite.
fn cleanup(child: &mut Child, pid: u32) {
    let _ = child.kill();
    unsafe {
        libc::kill(pid as i32, libc::SIGKILL);
    }
}

const SIGNALS: [(i32, &str); 3] = [
    (libc::SIGTERM, "SIGTERM"),
    (libc::SIGHUP, "SIGHUP"),
    (libc::SIGINT, "SIGINT"),
];

fn service(ignore: &str, stop_timeout: &str) -> String {
    format!(
        r#"
        [services.api]
        cmd = "trap '' {ignore}; echo $$ > api.pid; exec sleep 300"
        restart = "never"
        stop_timeout = "{stop_timeout}"
        "#
    )
}

fn task(ignore: &str) -> String {
    format!(
        r#"
        [tasks.long]
        cmd = "trap '' {ignore}; echo $$ > long.pid; exec sleep 300"
        cache = "disabled"
        "#
    )
}

#[test]
fn up_shuts_down_cleanly_on_every_stop_request() {
    for (sig, name) in SIGNALS {
        let ws = Workspace::new(&format!("up-{name}"), &service("HUP", "5s"));
        let mut up = ws.turborust(&["up", "--no-tui"]);
        let pid = ws.child_pid("api.pid");

        send(&up, sig);
        let status = wait(&mut up, Duration::from_secs(10));
        let orphaned = !gone_within(pid, Duration::from_secs(2));
        cleanup(&mut up, pid);

        let status = status.unwrap_or_else(|| panic!("`up` did not exit on {name}"));
        assert!(!orphaned, "{name} left the service running");
        // A requested stop is a successful one, as with `docker compose`.
        assert_eq!(status.code(), Some(0), "`up` after {name}: {status:?}");
    }
}

#[test]
fn run_shuts_down_cleanly_and_reports_the_interruption() {
    for (sig, name) in SIGNALS {
        let ws = Workspace::new(&format!("run-{name}"), &task("HUP"));
        let mut run = ws.turborust(&["run", "long"]);
        let pid = ws.child_pid("long.pid");

        send(&run, sig);
        let status = wait(&mut run, Duration::from_secs(10));
        let orphaned = !gone_within(pid, Duration::from_secs(2));
        cleanup(&mut run, pid);

        let status = status.unwrap_or_else(|| panic!("`run` did not exit on {name}"));
        assert!(!orphaned, "{name} left the task running");
        // An interrupted run must never read as a passing one; 128 + n is what
        // a shell would have reported for a process the signal ended.
        assert_eq!(
            status.code(),
            Some(128 + sig),
            "`run` after {name}: {status:?}"
        );
    }
}

#[test]
fn a_second_request_stops_waiting_and_kills_everything() {
    // A child that ignores SIGTERM too, with a stop timeout far longer than
    // anyone would wait: the first request starts a shutdown that will sit on
    // it, and the second must not. Twice, because a node that is still
    // starting stops through a different path from one that is running.
    let running = service("HUP TERM", "60s");
    let starting = format!("{running}health = {{ log = \"never printed\" }}\n");
    for (state, cfg) in [("running", running), ("starting", starting)] {
        let ws = Workspace::new(&format!("twice-{state}"), &cfg);
        let mut up = ws.turborust(&["up", "--no-tui"]);
        let pid = ws.child_pid("api.pid");

        send(&up, libc::SIGTERM);
        std::thread::sleep(Duration::from_millis(800));
        let still_waiting = up.try_wait().unwrap().is_none();

        // Not `send`: if the first request already ended turborust (the bug),
        // this fails, and the assertions below should say why rather than this.
        unsafe {
            libc::kill(up.id() as i32, libc::SIGTERM);
        }
        let status = wait(&mut up, Duration::from_secs(5));
        let orphaned = !gone_within(pid, Duration::from_secs(2));
        cleanup(&mut up, pid);

        assert!(
            still_waiting,
            "{state}: the first request should be waiting out the stop timeout"
        );
        assert!(
            status.is_some(),
            "{state}: a second request should exit at once"
        );
        assert!(
            !orphaned,
            "{state}: forcing the stop left the service running"
        );
    }
}

#[test]
fn the_fixture_really_ignores_the_hangup() {
    // Guards the suite against passing vacuously: if this shell did not honour
    // `trap '' HUP`, every child above would die of the pty hangup regardless.
    let dir = std::env::temp_dir();
    let pidfile: PathBuf = dir.join(format!("turborust-sig-fixture-{}", std::process::id()));
    let mut sh = Command::new("sh")
        .arg("-c")
        .arg(format!(
            "trap '' HUP; echo $$ > '{}'; exec sleep 30",
            pidfile.display()
        ))
        .spawn()
        .unwrap();
    let pid: u32 = loop {
        if let Ok(s) = std::fs::read_to_string(&pidfile)
            && let Ok(p) = s.trim().parse()
        {
            break p;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    std::thread::sleep(Duration::from_millis(200));
    unsafe {
        libc::kill(pid as i32, libc::SIGHUP);
    }
    let survived = !gone_within(pid, Duration::from_millis(500));
    let _ = sh.kill();
    let _ = sh.wait();
    let _ = std::fs::remove_file(Path::new(&pidfile));
    assert!(
        survived,
        "`trap '' HUP` did not survive exec, so the suite proves nothing"
    );
}
