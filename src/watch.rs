//! Filesystem watching.
//!
//! One watcher for the whole workspace, not one per service. N watchers means N
//! copies of every event and N times the file descriptors; dispatch is cheaper
//! done in userspace against compiled globsets.
//!
//! Ignored top-level directories are never handed to the OS watcher at all.
//! Registering `target/` on macOS means FSEvents streams every intermediate
//! artifact of every build at you — enough to starve the debouncer during a
//! cold `cargo build`.

use anyhow::{Context, Result};
use notify::RecursiveMode;
use std::collections::BTreeSet;
use std::path::Path;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

/// What a quiet spell delivers: everything that happened during it.
type Batch = std::result::Result<Vec<notify::Event>, Vec<notify::Error>>;

/// A live filesystem watch.
///
/// Both fields are private on purpose. The receiver used to be public, and
/// `tokio::spawn(async move { w.rx.recv().await })` compiled and ran — but under
/// edition-2021 disjoint capture the async block captures *only* `w.rx`, leaving
/// the debouncer behind to be dropped when the enclosing function returns. The
/// OS watch then stops, silently: no error, no closed channel until the next
/// poll, just a dev loop that never rebuilds again.
///
/// Going through `recv(&mut self)` forces the whole struct to be captured, so
/// the guard travels with the receiver and the mistake cannot be written.
pub struct Watcher {
    /// Dropping this stops the OS watch.
    _inner: Box<dyn std::any::Any + Send>,
    rx: mpsc::UnboundedReceiver<BTreeSet<String>>,
    warnings: Option<mpsc::UnboundedReceiver<String>>,
}

impl Watcher {
    /// Next batch of changed paths, workspace-relative. `None` once the watch ends.
    pub async fn recv(&mut self) -> Option<BTreeSet<String>> {
        self.rx.recv().await
    }

    /// Problems that mean some changes will not be seen, as they happen.
    ///
    /// Taken out once, so a dispatch loop can wait on both streams; only this
    /// receiver leaves, and the guard stays with `recv` as above.
    pub fn take_warnings(&mut self) -> mpsc::UnboundedReceiver<String> {
        self.warnings
            .take()
            .unwrap_or_else(|| mpsc::unbounded_channel().1)
    }
}

/// What to tell someone whose watch just stopped covering part of the tree.
///
/// Silence was the old answer, at both places this can happen — a directory
/// that failed to register at startup was skipped, and the error notify sends
/// when a directory created later cannot be watched was dropped — so edits there
/// simply never rebuilt anything (cairn 0089). Only the watch limit is worth a
/// warning: anything else here is a directory that vanished, which is harmless.
fn watch_warning(err: &notify::Error) -> Option<String> {
    if !matches!(err.kind, notify::ErrorKind::MaxFilesWatch) {
        return None;
    }
    let limit = std::fs::read_to_string("/proc/sys/fs/inotify/max_user_watches")
        .map(|s| format!(" (fs.inotify.max_user_watches = {})", s.trim()))
        .unwrap_or_default();
    Some(format!(
        "the OS file-watch limit is reached{limit}, so edits in some directories will \
         not be seen. Raise it, e.g. `sudo sysctl fs.inotify.max_user_watches=524288`, \
         and restart turborust"
    ))
}

/// Directories never handed to the OS watcher.
const NEVER_WATCH: [&str; 7] = [
    ".git",
    "target",
    "node_modules",
    ".turborust",
    "dist",
    ".next",
    ".direnv",
];

/// Where change notifications come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    /// The OS's own events: inotify, FSEvents, ReadDirectoryChangesW.
    Native,
    /// Scanning the tree at this interval, for filesystems that deliver no
    /// events — Docker Desktop bind mounts, WSL2's `/mnt/c`, NFS, SMB.
    Poll(Duration),
}

pub fn watch(root: &Path, debounce: Duration) -> Result<Watcher> {
    watch_with(root, debounce, Backend::Native)
}

/// Watches with the chosen backend. Both feed the same callback, so dispatch,
/// debouncing and the access-event filter are one piece of code, not two.
pub fn watch_with(root: &Path, debounce: Duration, backend: Backend) -> Result<Watcher> {
    let (tx, rx) = mpsc::unbounded_channel();
    let (warn_tx, warn_rx) = mpsc::unbounded_channel();
    let root_owned = root.to_path_buf();
    let runtime_warn = warn_tx.clone();

    let handler = move |res: Batch| {
        // Errors are not worth tearing the dev loop down for, but the one that
        // means part of the tree is no longer watched has to be said out loud.
        let events = match res {
            Ok(events) => events,
            Err(errors) => {
                if let Some(warning) = errors.iter().find_map(watch_warning) {
                    let _ = runtime_warn.send(warning);
                }
                return;
            }
        };
        let mut paths: BTreeSet<String> = BTreeSet::new();
        for ev in events {
            for p in &ev.paths {
                if let Some(rel) = relativize(&root_owned, p) {
                    paths.insert(rel);
                }
                // macOS FSEvents coalesces: a write to `src/a.rs` can surface
                // as a modification of `src` and nothing else. Glob patterns
                // match files, so a bare directory event would silently match
                // nothing and the rebuild would never fire. Expand one level.
                for child in expand_dir(&root_owned, p) {
                    paths.insert(child);
                }
            }
        }
        if !paths.is_empty() {
            let _ = tx.send(paths);
        }
    };

    let inner: Box<dyn std::any::Any + Send> = match backend {
        Backend::Native => {
            let (events_tx, events_rx) = std::sync::mpsc::channel();
            let mut w = notify::recommended_watcher(move |ev: notify::Result<notify::Event>| {
                if !is_access(&ev) {
                    let _ = events_tx.send(ev);
                }
            })
            .context("creating filesystem watcher")?;
            register(&mut w, root, &warn_tx)?;
            // Ends by itself when `w` is dropped: that drops the sender above.
            std::thread::Builder::new()
                .name("turborust debounce".into())
                .spawn(move || debounce_loop(events_rx, debounce, handler))
                .context("starting the debounce thread")?;
            Box::new(w)
        }
        Backend::Poll(every) => Box::new(Poller::start(root, every, handler)?),
    };

    Ok(Watcher {
        _inner: inner,
        rx,
        warnings: Some(warn_rx),
    })
}

/// Reading is not changing. inotify reports a file or directory being opened,
/// and `expand_dir` reads every directory it is handed — so on Linux each
/// access event produced the next, and a nested workspace restarted its
/// services about eight times a second, forever (cairn 0088). A write always
/// produces a modify event too, so nothing real is lost; watchexec ignores
/// access events by default for the same reason. Dropped before debouncing,
/// so a read cannot even hold a quiet spell open.
fn is_access(ev: &notify::Result<notify::Event>) -> bool {
    matches!(ev, Ok(e) if matches!(e.kind, notify::EventKind::Access(_)))
}

/// The longest a steady stream of changes can hold a batch back, unless the
/// quiet window asked for is itself long.
const LONGEST_BATCH: Duration = Duration::from_secs(1);

/// Groups events into batches that end after `quiet` with nothing new.
///
/// Waits on the channel rather than on a clock: notify's own debouncer woke
/// every quarter of its window forever, which was most of an idle `up`'s
/// wakeups, and added up to that quarter again to every edit.
fn debounce_loop(
    rx: std::sync::mpsc::Receiver<notify::Result<notify::Event>>,
    quiet: Duration,
    mut handler: impl FnMut(Batch),
) {
    use std::sync::mpsc::RecvTimeoutError;
    while let Ok(first) = rx.recv() {
        let mut events = Vec::new();
        let mut errors = Vec::new();
        let mut take = |ev: notify::Result<notify::Event>| match ev {
            Ok(e) => events.push(e),
            Err(e) => errors.push(e),
        };
        take(first);
        let began = Instant::now();
        let longest = LONGEST_BATCH.max(quiet * 4);
        let mut open = true;
        while open {
            let left = longest.saturating_sub(began.elapsed());
            if left.is_zero() {
                break;
            }
            match rx.recv_timeout(quiet.min(left)) {
                Ok(ev) => take(ev),
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => open = false,
            }
        }
        if !errors.is_empty() {
            handler(Err(errors));
        }
        if !events.is_empty() {
            handler(Ok(events));
        }
        if !open {
            return;
        }
    }
}

/// A scan of the tree every interval, for where OS events never arrive.
///
/// Not notify's `PollWatcher`: that one compares modification times in whole
/// seconds, so a second save within the same second as the first was never
/// seen at all — and its alternative, hashing every file on every scan, is the
/// wrong cost on exactly the network filesystems polling exists for. Here a
/// file is its full-precision mtime and its length.
///
/// The first scan happens before `start` returns, so an edit made the moment it
/// does is a change against that baseline rather than part of it.
struct Poller {
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

type Snapshot =
    std::collections::BTreeMap<std::path::PathBuf, (Option<std::time::SystemTime>, u64)>;

impl Poller {
    fn start<F>(root: &Path, every: Duration, mut handler: F) -> Result<Poller>
    where
        F: FnMut(Batch) + Send + 'static,
    {
        use std::sync::atomic::{AtomicBool, Ordering};
        let root = root.to_path_buf();
        let mut before = scan(&root)?;
        let stop = std::sync::Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        std::thread::Builder::new()
            .name("turborust poll".into())
            .spawn(move || {
                while !stopped.load(Ordering::Relaxed) {
                    std::thread::sleep(every);
                    let Ok(now) = scan(&root) else { continue };
                    let changed = diff(&before, &now);
                    before = now;
                    if changed.is_empty() || stopped.load(Ordering::Relaxed) {
                        continue;
                    }
                    let events = changed
                        .into_iter()
                        .map(|(path, kind)| notify::Event::new(kind).add_path(path))
                        .collect();
                    handler(Ok(events));
                }
            })
            .context("starting the polling thread")?;
        Ok(Poller { stop })
    }
}

impl Drop for Poller {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Every file under the watched top-level entries. Never-watched directories
/// are skipped at the top, as the native watcher skips them, and `.git`,
/// `target` and `node_modules` at any depth, since walking them on every scan
/// would cost more than everything else together. Symlinked directories are
/// not followed, so a link cycle cannot make a scan endless.
fn scan(root: &Path) -> Result<Snapshot> {
    const NEVER_DEEP: [&str; 3] = [".git", "target", "node_modules"];
    let mut out = Snapshot::new();
    let mut stack: Vec<std::path::PathBuf> = Vec::new();
    for entry in std::fs::read_dir(root)
        .with_context(|| format!("reading {}", root.display()))?
        .flatten()
    {
        if !NEVER_WATCH.contains(&entry.file_name().to_string_lossy().as_ref()) {
            stack.push(entry.path());
        }
    }
    while let Some(path) = stack.pop() {
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.is_dir() {
            let Ok(entries) = std::fs::read_dir(&path) else {
                continue;
            };
            for e in entries.flatten() {
                if !NEVER_DEEP.contains(&e.file_name().to_string_lossy().as_ref()) {
                    stack.push(e.path());
                }
            }
        } else {
            // A symlink to a file counts by its target, so an edit through it shows.
            let meta = std::fs::metadata(&path).unwrap_or(meta);
            if !meta.is_dir() {
                out.insert(path, (meta.modified().ok(), meta.len()));
            }
        }
    }
    Ok(out)
}

fn diff(before: &Snapshot, now: &Snapshot) -> Vec<(std::path::PathBuf, notify::EventKind)> {
    use notify::EventKind;
    use notify::event::{CreateKind, ModifyKind, RemoveKind};
    let mut out = Vec::new();
    for (path, stamp) in now {
        match before.get(path) {
            None => out.push((path.clone(), EventKind::Create(CreateKind::File))),
            Some(old) if old != stamp => {
                out.push((path.clone(), EventKind::Modify(ModifyKind::Any)))
            }
            Some(_) => {}
        }
    }
    for path in before.keys().filter(|p| !now.contains_key(*p)) {
        out.push((path.clone(), EventKind::Remove(RemoveKind::File)));
    }
    out
}

/// Hands the workspace's top-level entries, less the never-watched ones, to the
/// backend.
fn register(
    watcher: &mut impl notify::Watcher,
    root: &Path,
    warn_tx: &mpsc::UnboundedSender<String>,
) -> Result<()> {
    let mut watched = 0usize;
    for entry in std::fs::read_dir(root)
        .with_context(|| format!("reading {}", root.display()))?
        .flatten()
    {
        let name = entry.file_name().to_string_lossy().to_string();
        if NEVER_WATCH.contains(&name.as_str()) {
            continue;
        }
        let path = entry.path();
        let mode = if path.is_dir() {
            RecursiveMode::Recursive
        } else {
            RecursiveMode::NonRecursive
        };
        match watcher.watch(&path, mode) {
            Ok(()) => watched += 1,
            Err(e) => {
                // Notify registers what it can before giving up, so the rest of
                // this directory is partly watched; say so once and carry on.
                if let Some(warning) = watch_warning(&e) {
                    let _ = warn_tx.send(warning);
                    watched += 1;
                }
                // Anything else is a directory that vanished between readdir and
                // watch, which is not worth a word.
            }
        }
    }
    if watched == 0 {
        anyhow::bail!("nothing watchable under {}", root.display());
    }
    Ok(())
}

/// Immediate file children of `path`, if it is a directory.
///
/// One level only, and capped: this runs on the watcher callback thread, and a
/// directory event on a huge tree must not stall event delivery. Anything deeper
/// will arrive as its own event.
fn expand_dir(root: &Path, path: &Path) -> Vec<String> {
    const MAX: usize = 512;
    if !path.is_dir() {
        return Vec::new();
    }
    let Ok(entries) = std::fs::read_dir(path) else {
        return Vec::new();
    };
    entries
        .flatten()
        .take(MAX)
        .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
        .filter_map(|e| relativize(root, &e.path()))
        .collect()
}

/// Workspace-relative, forward-slashed. `None` for paths outside the workspace.
fn relativize(root: &Path, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(root).ok()?;
    let s = rel.to_string_lossy().replace('\\', "/");
    if s.is_empty() { None } else { Some(s) }
}

/// True if any changed path is one this matcher cares about.
pub fn any_match(paths: &BTreeSet<String>, matcher: &crate::cache::Matcher) -> bool {
    paths.iter().any(|p| matcher.matches(p))
}

/// The subset of `paths` this matcher cares about — used for "why did this restart".
pub fn matching(paths: &BTreeSet<String>, matcher: &crate::cache::Matcher) -> Vec<String> {
    paths
        .iter()
        .filter(|p| matcher.matches(p))
        .cloned()
        .collect()
}

pub fn resolve_path(root: &Path, p: &Path) -> Option<String> {
    relativize(root, p)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::Matcher;

    #[test]
    fn relativizes_inside_root() {
        let root = Path::new("/w");
        assert_eq!(
            relativize(root, Path::new("/w/src/a.rs")).as_deref(),
            Some("src/a.rs")
        );
        assert_eq!(relativize(root, Path::new("/elsewhere/a.rs")), None);
    }

    #[test]
    fn only_the_watch_limit_is_worth_a_warning() {
        // Cover for cairn 0089. The limit cannot be lowered from a test, so the
        // decision is pinned here and the end-to-end path was checked by hand
        // with `fs.inotify.max_user_watches` lowered.
        let limit = watch_warning(&notify::Error::new(notify::ErrorKind::MaxFilesWatch))
            .expect("the watch limit must be reported");
        assert!(
            limit.contains("will not be seen") && limit.contains("max_user_watches"),
            "{limit}"
        );
        // A directory that vanished between listing and watching is not news.
        let vanished = notify::Error::io(std::io::Error::from(std::io::ErrorKind::NotFound));
        assert_eq!(watch_warning(&vanished), None);
    }

    #[test]
    fn dispatch_selects_only_interested_matchers() {
        let paths: BTreeSet<String> = [
            "crates/api/src/main.rs".to_string(),
            "web/index.html".to_string(),
        ]
        .into();
        let api = Matcher::new(&["crates/api/**/*.rs".into()], &[]).unwrap();
        let web = Matcher::new(&["web/**".into()], &[]).unwrap();
        let none = Matcher::new(&["docs/**".into()], &[]).unwrap();
        assert_eq!(matching(&paths, &api), vec!["crates/api/src/main.rs"]);
        assert!(any_match(&paths, &web));
        assert!(!any_match(&paths, &none));
    }

    /// Writes repeatedly until the watcher reports the file, or time runs out.
    ///
    /// Waiting longer is not enough on its own: if the write lands before the OS
    /// watch is armed, no event is ever generated and there is nothing to wait
    /// for. Repeating the stimulus is what makes the test measure the watcher
    /// rather than the machine's scheduling.
    async fn write_until_seen(w: &mut Watcher, root: &Path, rel: &str) -> BTreeSet<String> {
        let mut seen: BTreeSet<String> = BTreeSet::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        let mut attempt = 0;
        while tokio::time::Instant::now() < deadline {
            attempt += 1;
            let _ = std::fs::write(root.join(rel), format!("// attempt {attempt}\n"));
            match tokio::time::timeout(Duration::from_millis(500), w.recv()).await {
                Ok(Some(batch)) => {
                    seen.extend(batch);
                    if seen
                        .iter()
                        .any(|p| p.ends_with(rel.rsplit('/').next().unwrap()))
                    {
                        return seen;
                    }
                }
                Ok(None) => {
                    panic!("the watch channel closed after {attempt} attempt(s); saw {seen:?}")
                }
                Err(_) => {}
            }
        }
        seen
    }

    #[tokio::test]
    async fn sees_a_real_write() {
        let dir = std::env::temp_dir().join(format!("turborust-watch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        let root = dir.canonicalize().unwrap();
        let mut w = watch(&root, Duration::from_millis(50)).unwrap();
        // Give the OS watcher a moment to arm before touching the tree.
        tokio::time::sleep(Duration::from_millis(300)).await;

        let seen = write_until_seen(&mut w, &root, "src/a.rs").await;
        let _ = std::fs::remove_dir_all(&root);
        assert!(
            seen.iter().any(|p| p.ends_with("a.rs")),
            "no event within the deadline; saw {seen:?}"
        );
    }

    /// Batches until the watcher has been quiet for `quiet`, or `None` if it
    /// never goes quiet within `within`.
    async fn settle(w: &mut Watcher, quiet: Duration, within: Duration) -> Option<usize> {
        let deadline = tokio::time::Instant::now() + within;
        let mut batches = 0;
        while tokio::time::Instant::now() < deadline {
            match tokio::time::timeout(quiet, w.recv()).await {
                Ok(Some(_)) => batches += 1,
                Ok(None) => panic!("the watch channel closed"),
                Err(_) => return Some(batches),
            }
        }
        None
    }

    #[tokio::test]
    async fn reading_a_nested_tree_is_not_a_change() {
        // Cover for cairn 0088. On Linux, inotify reports a file or directory
        // being *opened* — and reading a directory is what `expand_dir` does
        // for every directory event. Treated as changes, those access events
        // fed themselves: one edit, or merely arming the watch on a nested
        // tree, restarted a service about eight times a second, forever.
        // A flat `src/` never triggers it, which is why nothing caught it.
        let dir = std::env::temp_dir().join(format!("turborust-watch-read-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for d in ["crates/api/src", "crates/shared/src"] {
            std::fs::create_dir_all(dir.join(d)).unwrap();
        }
        std::fs::write(dir.join("crates/shared/src/lib.rs"), "").unwrap();
        let root = dir.canonicalize().unwrap();
        let mut w = watch(&root, Duration::from_millis(50)).unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;

        // Live first, so the silence below means something.
        let seen = write_until_seen(&mut w, &root, "crates/api/src/main.rs").await;
        assert!(seen.iter().any(|p| p.ends_with("main.rs")), "saw {seen:?}");

        let settled = settle(&mut w, Duration::from_millis(500), Duration::from_secs(5)).await;
        assert!(
            settled.is_some(),
            "one write left the watcher reporting forever"
        );

        // Exactly what a build, an editor's file tree or `expand_dir` does.
        for f in ["crates/api/src/main.rs", "crates/shared/src/lib.rs"] {
            let _ = std::fs::read_to_string(root.join(f)).unwrap();
        }
        for d in [
            "crates",
            "crates/api",
            "crates/api/src",
            "crates/shared/src",
        ] {
            let _ = std::fs::read_dir(root.join(d)).unwrap().count();
        }
        let after = tokio::time::timeout(Duration::from_secs(1), w.recv()).await;
        let _ = std::fs::remove_dir_all(&root);
        if let Ok(Some(batch)) = after {
            panic!("reading files was reported as a change: {batch:?}");
        }
    }
}

#[cfg(test)]
mod poll_tests {
    use super::*;

    #[tokio::test]
    async fn polling_sees_a_nested_write_and_is_quiet_otherwise() {
        // Cover for cairn 0094: where OS events never arrive, scanning is the
        // only way to see an edit. Same callback as the native watcher, so a
        // poll batch is filtered and relativized exactly as an event batch is.
        let dir = std::env::temp_dir().join(format!("turborust-poll-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("crates/api/src")).unwrap();
        std::fs::create_dir_all(dir.join("target/debug")).unwrap();
        std::fs::write(dir.join("crates/api/src/main.rs"), "").unwrap();
        let root = dir.canonicalize().unwrap();
        let mut w = watch_with(
            &root,
            Duration::from_millis(50),
            Backend::Poll(Duration::from_millis(100)),
        )
        .unwrap();

        let quiet = tokio::time::timeout(Duration::from_millis(600), w.recv()).await;
        assert!(quiet.is_err(), "an untouched tree was reported: {quiet:?}");

        // Never-watched directories stay unwatched when polling, too.
        std::fs::write(root.join("target/debug/out"), "x").unwrap();
        std::fs::write(root.join("crates/api/src/main.rs"), "// edited\n").unwrap();
        let batch = tokio::time::timeout(Duration::from_secs(5), w.recv())
            .await
            .expect("the poll never reported the edit")
            .unwrap();
        let _ = std::fs::remove_dir_all(&root);
        assert!(batch.contains("crates/api/src/main.rs"), "saw {batch:?}");
        assert!(
            !batch.iter().any(|p| p.starts_with("target/")),
            "polled a never-watched directory: {batch:?}"
        );
    }

    #[tokio::test]
    async fn polling_sees_a_second_save_within_the_same_second() {
        // notify's own PollWatcher compares whole-second mtimes, so the second
        // of two quick saves was lost for good. A same-length rewrite is the
        // hardest case: only the sub-second mtime differs.
        let dir = std::env::temp_dir().join(format!("turborust-poll2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        let root = dir.canonicalize().unwrap();
        let mut w = watch_with(
            &root,
            Duration::from_millis(20),
            Backend::Poll(Duration::from_millis(50)),
        )
        .unwrap();
        for n in 1..=3 {
            std::fs::write(root.join("src/a.rs"), format!("// save {n}\n")).unwrap();
            let batch = tokio::time::timeout(Duration::from_secs(3), w.recv())
                .await
                .unwrap_or_else(|_| panic!("save {n} was never reported"))
                .unwrap();
            assert!(batch.contains("src/a.rs"), "save {n}: saw {batch:?}");
        }
        let _ = std::fs::remove_dir_all(&root);
    }
}

#[cfg(test)]
mod guard_lifetime_tests {
    use super::*;

    /// Creates a watch and hands it to a spawned task, then returns — exactly the
    /// shape that used to drop the debouncer and silently kill all watching.
    fn hand_off(root: &Path) -> tokio::sync::mpsc::UnboundedReceiver<BTreeSet<String>> {
        let mut w = watch(root, Duration::from_millis(80)).unwrap();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        tokio::spawn(async move {
            while let Some(paths) = w.recv().await {
                if tx.send(paths).is_err() {
                    return;
                }
            }
        });
        rx
    }

    #[tokio::test]
    async fn watch_survives_being_moved_into_a_spawned_task() {
        let dir = std::env::temp_dir().join(format!("tr-guard-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        let root = dir.canonicalize().unwrap();

        // The creating scope returns here; only the spawned task holds the watch.
        let mut rx = hand_off(&root);
        tokio::time::sleep(Duration::from_millis(400)).await;

        // Retried for the same reason as `sees_a_real_write`: a write landing
        // before the OS watch is armed produces no event at all, so waiting
        // longer cannot help. A *closed* channel is different — that is the bug
        // this test exists to catch, so it fails immediately and says so.
        let mut seen: BTreeSet<String> = BTreeSet::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        let mut attempt = 0;
        // Keep going until the *file* is seen, not until anything is: FSEvents
        // often reports the parent directory first, and stopping there is what
        // made this flake with `saw {"src"}`.
        while tokio::time::Instant::now() < deadline && !seen.iter().any(|p| p.ends_with("a.rs")) {
            attempt += 1;
            let _ = std::fs::write(root.join("src/a.rs"), format!("// {attempt}\n"));
            match tokio::time::timeout(Duration::from_millis(500), rx.recv()).await {
                Ok(Some(batch)) => seen.extend(batch),
                Ok(None) => {
                    let _ = std::fs::remove_dir_all(&root);
                    panic!("the watch channel closed: the guard was dropped with the scope");
                }
                Err(_) => {}
            }
        }
        let _ = std::fs::remove_dir_all(&root);
        assert!(
            seen.iter().any(|p| p.ends_with("a.rs")),
            "no event within the deadline; saw {seen:?}"
        );
    }
}

#[cfg(test)]
mod dir_expansion_tests {
    use super::*;

    fn ev(path: &str) -> notify::Result<notify::Event> {
        Ok(notify::Event::new(notify::EventKind::Any).add_path(path.into()))
    }

    /// Runs the loop over events sent at the given offsets, then hangs up.
    fn batches(quiet_ms: u64, sends: &[(u64, &'static str)]) -> Vec<Vec<String>> {
        let (tx, rx) = std::sync::mpsc::channel();
        let out = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = out.clone();
        let t = std::thread::spawn(move || {
            debounce_loop(rx, Duration::from_millis(quiet_ms), move |b: Batch| {
                let paths = b
                    .unwrap()
                    .iter()
                    .flat_map(|e| e.paths.iter().map(|p| p.display().to_string()))
                    .collect();
                seen.lock().unwrap().push(paths);
            })
        });
        let start = Instant::now();
        for (at, path) in sends {
            let due = start + Duration::from_millis(*at);
            std::thread::sleep(due.saturating_duration_since(Instant::now()));
            tx.send(ev(path)).unwrap();
        }
        drop(tx);
        t.join().unwrap();
        std::sync::Arc::try_unwrap(out)
            .unwrap()
            .into_inner()
            .unwrap()
    }

    #[test]
    fn a_burst_of_events_is_one_batch() {
        let got = batches(80, &[(0, "a"), (10, "b"), (20, "a"), (30, "c")]);
        assert_eq!(got, vec![vec!["a", "b", "a", "c"]]);
    }

    #[test]
    fn events_a_quiet_spell_apart_are_two_batches() {
        let got = batches(40, &[(0, "a"), (300, "b")]);
        assert_eq!(got, vec![vec!["a"], vec!["b"]]);
    }

    #[test]
    fn a_steady_stream_is_still_delivered() {
        // An event every 20ms never leaves 40ms of quiet; without the cap the
        // first batch would wait for the stream to stop.
        let sends: Vec<(u64, &'static str)> = (0..90).map(|i| (i * 20, "x")).collect();
        let got = batches(40, &sends);
        assert!(
            got.len() >= 2,
            "flushed while the stream ran: {} batches",
            got.len()
        );
        assert_eq!(got.iter().map(Vec::len).sum::<usize>(), 90, "nothing lost");
    }

    #[test]
    fn hanging_up_delivers_what_is_pending() {
        let got = batches(10_000, &[(0, "a"), (5, "b")]);
        assert_eq!(got, vec![vec!["a", "b"]]);
    }

    #[test]
    fn directory_events_expand_to_their_files() {
        let dir = std::env::temp_dir().join(format!("tr-expand-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/a.rs"), "x").unwrap();
        std::fs::create_dir_all(dir.join("src/nested")).unwrap();
        let root = dir.canonicalize().unwrap();

        let got = expand_dir(&root, &root.join("src"));
        let _ = std::fs::remove_dir_all(&root);

        assert_eq!(
            got,
            vec!["src/a.rs".to_string()],
            "dirs must not be reported as files"
        );
    }

    #[test]
    fn expanding_a_file_yields_nothing() {
        assert!(expand_dir(Path::new("/w"), Path::new("/w/does/not/exist")).is_empty());
    }
}
