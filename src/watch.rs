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
use notify_debouncer_full::{DebounceEventResult, new_debouncer};
use std::collections::BTreeSet;
use std::path::Path;
use std::time::Duration;
use tokio::sync::mpsc;

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
}

impl Watcher {
    /// Next batch of changed paths, workspace-relative. `None` once the watch ends.
    pub async fn recv(&mut self) -> Option<BTreeSet<String>> {
        self.rx.recv().await
    }
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

pub fn watch(root: &Path, debounce: Duration) -> Result<Watcher> {
    let (tx, rx) = mpsc::unbounded_channel();
    let root_owned = root.to_path_buf();

    let mut debouncer = new_debouncer(debounce, None, move |res: DebounceEventResult| {
        // Watch errors (a vanished directory, an exhausted fd limit) are not
        // worth tearing the dev loop down for; the next batch usually works.
        let Ok(events) = res else { return };
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
    })
    .context("creating filesystem watcher")?;

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
        // A directory vanishing between readdir and watch is not fatal.
        if debouncer.watch(&path, mode).is_ok() {
            watched += 1;
        }
    }
    if watched == 0 {
        anyhow::bail!("nothing watchable under {}", root.display());
    }

    Ok(Watcher {
        _inner: Box::new(debouncer),
        rx,
    })
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

    #[tokio::test]
    async fn sees_a_real_write() {
        let dir = std::env::temp_dir().join(format!("turborust-watch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        let root = dir.canonicalize().unwrap();
        let mut w = watch(&root, Duration::from_millis(50)).unwrap();
        // Give the OS watcher a moment to arm before touching the tree.
        tokio::time::sleep(Duration::from_millis(300)).await;
        std::fs::write(root.join("src/a.rs"), "fn main() {}").unwrap();
        let got = tokio::time::timeout(Duration::from_secs(10), w.recv()).await;
        let _ = std::fs::remove_dir_all(&root);
        let paths = got.expect("watcher timed out").expect("watcher closed");
        assert!(paths.iter().any(|p| p.ends_with("a.rs")), "{paths:?}");
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

        std::fs::write(root.join("src/a.rs"), "fn main() {}").unwrap();
        let got = tokio::time::timeout(Duration::from_secs(10), rx.recv()).await;
        let _ = std::fs::remove_dir_all(&root);

        let paths = got
            .expect("no event: the OS watch was dropped")
            .expect("channel closed");
        assert!(paths.iter().any(|p| p.ends_with("a.rs")), "{paths:?}");
    }
}

#[cfg(test)]
mod dir_expansion_tests {
    use super::*;

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
