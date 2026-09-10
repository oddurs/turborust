//! Content-addressed fingerprints and the task cache.
//!
//! The cache key is a blake3 hash over: the command line, the declared env keys,
//! the fingerprints of upstream tasks, and the content of every input file. Content,
//! not mtime — mtime-based invalidation is why every other watcher occasionally
//! rebuilds the world after a `git checkout`.
//!
//! Every fingerprint keeps its per-file hashes so `turborust why` can answer the
//! only question that matters when a rebuild surprises you: *which file, exactly?*

use anyhow::{Context, Result};
use globset::{Glob, GlobSet, GlobSetBuilder};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Fingerprint {
    /// The overall cache key.
    pub hash: String,
    /// Repo-relative path -> content hash. Sorted, so the key is order-stable.
    pub files: BTreeMap<String, String>,
    /// Non-file contributors, recorded so `why` can attribute a change to them.
    pub meta: BTreeMap<String, String>,
}

impl Fingerprint {
    pub fn short(&self) -> String {
        self.hash.chars().take(12).collect()
    }
}

/// Compiles include/exclude globs into a matcher rooted at the workspace.
pub struct Matcher {
    include: GlobSet,
    exclude: GlobSet,
    empty: bool,
}

impl Matcher {
    pub fn new(include: &[String], exclude: &[String]) -> Result<Self> {
        Ok(Matcher {
            include: build_set(include)?,
            exclude: build_set(exclude)?,
            empty: include.is_empty(),
        })
    }

    pub fn is_empty(&self) -> bool {
        self.empty
    }

    /// `rel` must be workspace-relative and use forward slashes.
    pub fn matches(&self, rel: &str) -> bool {
        if self.empty {
            return false;
        }
        self.include.is_match(rel) && !self.exclude.is_match(rel)
    }
}

fn build_set(globs: &[String]) -> Result<GlobSet> {
    let mut b = GlobSetBuilder::new();
    for g in globs {
        b.add(Glob::new(g).with_context(|| format!("invalid glob `{g}`"))?);
        // `crates/api/**/*.rs` should also match `crates/api/main.rs`; globset's
        // `**` wants an intervening separator, so add the collapsed form too.
        let collapsed = g.replace("/**/", "/");
        if collapsed != *g
            && let Ok(gl) = Glob::new(&collapsed)
        {
            b.add(gl);
        }
    }
    Ok(b.build()?)
}

/// Walks the workspace and hashes every file matching `matcher`.
pub fn fingerprint_files(root: &Path, matcher: &Matcher) -> Result<BTreeMap<String, String>> {
    let mut files = BTreeMap::new();
    if matcher.is_empty() {
        return Ok(files);
    }
    let mut walker = ignore::WalkBuilder::new(root);
    walker
        .hidden(false)
        .git_ignore(true)
        .git_global(false)
        .parents(false);
    for entry in walker.build() {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue, // unreadable paths are not worth failing a build over
        };
        if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            continue;
        }
        let Ok(rel) = entry.path().strip_prefix(root) else {
            continue;
        };
        let rel = rel.to_string_lossy().replace('\\', "/");
        if !matcher.matches(&rel) {
            continue;
        }
        let bytes = match std::fs::read(entry.path()) {
            Ok(b) => b,
            Err(_) => continue,
        };
        files.insert(rel, blake3::hash(&bytes).to_hex().to_string());
    }
    Ok(files)
}

/// Combines file hashes and non-file inputs into one stable key.
pub fn combine(files: BTreeMap<String, String>, meta: BTreeMap<String, String>) -> Fingerprint {
    let mut h = blake3::Hasher::new();
    h.update(b"turborust-v1\0");
    for (k, v) in &meta {
        h.update(k.as_bytes());
        h.update(b"\0");
        h.update(v.as_bytes());
        h.update(b"\0");
    }
    h.update(b"--files--\0");
    for (k, v) in &files {
        h.update(k.as_bytes());
        h.update(b"\0");
        h.update(v.as_bytes());
        h.update(b"\0");
    }
    Fingerprint {
        hash: h.finalize().to_hex().to_string(),
        files,
        meta,
    }
}

/// A single change between two fingerprints, in `why` output order.
#[derive(Debug, PartialEq, Eq)]
pub enum Change {
    Added(String, String),
    Removed(String, String),
    Modified {
        path: String,
        from: String,
        to: String,
    },
    Meta {
        key: String,
        from: String,
        to: String,
    },
}

impl Change {
    pub fn render(&self) -> String {
        let s = |h: &str| h.chars().take(8).collect::<String>();
        match self {
            Change::Added(p, h) => format!("+ {p}  (new, b3:{})", s(h)),
            Change::Removed(p, h) => format!("- {p}  (was b3:{})", s(h)),
            Change::Modified { path, from, to } => {
                format!("~ {path}  b3:{} -> b3:{}", s(from), s(to))
            }
            Change::Meta { key, from, to } => {
                // Dependency stamps are `key:<hex>` or `out:<hex>`. Printed raw
                // they are two 64-character hashes and a reader learns nothing.
                if let (Some(a), Some(b)) = (stamp(from), stamp(to)) {
                    format!("~ [{key}]  {a} -> {b}")
                } else {
                    format!("~ [{key}]  {from} -> {to}")
                }
            }
        }
    }
}

/// Renders a dependency stamp as its rule and a short hash: `outputs b3:5f16c070`.
fn stamp(v: &str) -> Option<String> {
    let (tag, hex) = v.split_once(':')?;
    let rule = match tag {
        "out" => "outputs",
        "key" => "key",
        _ => return None,
    };
    let short: String = hex.chars().take(8).collect();
    (short.len() == 8 && short.chars().all(|c| c.is_ascii_hexdigit()))
        .then(|| format!("{rule} b3:{short}"))
}

/// Everything that differs between `old` and `new`.
pub fn diff(old: &Fingerprint, new: &Fingerprint) -> Vec<Change> {
    let mut out = Vec::new();
    for (k, v) in &new.meta {
        match old.meta.get(k) {
            Some(prev) if prev != v => out.push(Change::Meta {
                key: k.clone(),
                from: prev.clone(),
                to: v.clone(),
            }),
            _ => {}
        }
    }
    for (path, h) in &new.files {
        match old.files.get(path) {
            None => out.push(Change::Added(path.clone(), h.clone())),
            Some(prev) if prev != h => out.push(Change::Modified {
                path: path.clone(),
                from: prev.clone(),
                to: h.clone(),
            }),
            _ => {}
        }
    }
    for (path, h) in &old.files {
        if !new.files.contains_key(path) {
            out.push(Change::Removed(path.clone(), h.clone()));
        }
    }
    out
}

/// Copies the named relative paths from one artifact directory to another.
fn copy_tree(from: &Path, to: &Path, files: &[String]) -> Result<()> {
    for rel in files {
        let src = from.join(rel);
        if !src.is_file() {
            continue;
        }
        let dest = to.join(rel);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(&src, &dest).with_context(|| format!("copying {rel}"))?;
    }
    Ok(())
}

/// Collects the files a task declared as outputs.
///
/// Deliberately does *not* respect `.gitignore`, unlike input hashing: build
/// outputs are almost always ignored, so honouring it here would archive nothing
/// and silently make every task un-restorable.
pub fn collect_outputs(root: &Path, matcher: &Matcher) -> Vec<String> {
    let mut found = Vec::new();
    if matcher.is_empty() {
        return found;
    }
    let mut walker = ignore::WalkBuilder::new(root);
    walker
        .hidden(false)
        .git_ignore(false)
        .git_global(false)
        .ignore(false)
        .parents(false);
    for entry in walker.build().flatten() {
        if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            continue;
        }
        let Ok(rel) = entry.path().strip_prefix(root) else {
            continue;
        };
        let rel = rel.to_string_lossy().replace('\\', "/");
        if matcher.matches(&rel) {
            found.push(rel);
        }
    }
    found.sort();
    found
}

/// Hashes the *contents* of a task's declared outputs.
///
/// This is what a dependent keys on when its upstream declared outputs: the
/// question that matters downstream is what the upstream produced, not whether
/// it happened to run.
///
/// `None` when nothing was produced. An empty set of outputs would hash to a
/// single constant, so every task that declared outputs and produced none would
/// collide with every other — and a build that exits 0 without writing its
/// declared outputs is exactly the case where a dependent must not be told
/// "nothing changed".
pub fn hash_outputs(root: &Path, files: &[String]) -> Option<String> {
    if files.is_empty() {
        return None;
    }
    let mut sorted: Vec<&String> = files.iter().collect();
    sorted.sort();
    let mut h = blake3::Hasher::new();
    h.update(b"turborust-outputs-v1\0");
    let mut any = false;
    for rel in sorted {
        let Ok(bytes) = std::fs::read(root.join(rel)) else {
            continue;
        };
        any = true;
        h.update(rel.as_bytes());
        h.update(b"\0");
        h.update(blake3::hash(&bytes).as_bytes());
        h.update(b"\0");
    }
    any.then(|| h.finalize().to_hex().to_string())
}

/// On-disk record of one successful run, addressed by its cache key.
#[derive(Debug, Serialize, Deserialize)]
pub struct Record {
    pub task: String,
    pub fingerprint: Fingerprint,
    pub exit_code: i32,
    pub duration_ms: u64,
    /// Output files captured, relative to the workspace root.
    #[serde(default)]
    pub outputs: Vec<String>,
    /// False when the outputs were too large to archive; such a record can only
    /// be replayed if the files are still on disk.
    #[serde(default)]
    pub archived: bool,
    /// [`hash_outputs`] over `outputs`, so a dependent can key on what this task
    /// produced without re-reading the files on every hit.
    ///
    /// Absent on records written before dependents keyed on outputs; those fall
    /// back to hashing on demand.
    #[serde(default)]
    pub output_hash: Option<String>,
}

/// Outputs larger than this are not archived. A dev cache that can silently eat
/// the disk is worse than one that occasionally rebuilds.
pub const MAX_ARCHIVE_BYTES: u64 = 256 * 1024 * 1024;

/// How much may be written between sweeps before one is forced.
///
/// A sweep walks the whole store, so doing it on every `store` would make a
/// warm cache quadratic in its own size. An eighth of the budget bounds the
/// overshoot to something the disk will not notice.
const SWEEP_EVERY: u64 = 8;

/// One result in the store, as the sweeper sees it.
struct Entry {
    /// Milliseconds since the epoch.
    used_at: u64,
    bytes: u64,
    record: PathBuf,
    artifacts: PathBuf,
    marker: PathBuf,
}

/// What the store is currently holding.
#[derive(Debug, Default, Serialize)]
pub struct Stats {
    pub bytes: u64,
    pub records: usize,
    /// Task name -> (bytes, record count).
    pub by_task: BTreeMap<String, (u64, usize)>,
}

/// What the last eviction dropped, so a surprising miss can be explained.
#[derive(Debug, Serialize, Deserialize)]
pub struct Sweep {
    /// Seconds since the epoch.
    pub at: u64,
    pub dropped: usize,
    pub freed: u64,
}

pub struct Cache {
    runs: PathBuf,
    artifacts: PathBuf,
    /// A second store, read after the local one and written only if `push`.
    shared: Option<PathBuf>,
    push: bool,
    /// Byte budget for the local store; `None` keeps everything.
    budget: Option<u64>,
    /// Bytes written since the last sweep, so the walk is amortised.
    written: std::sync::atomic::AtomicU64,
}

/// Milliseconds since the epoch.
fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Reads a use marker. `None` when it is absent or not a number.
fn read_millis(path: &Path) -> Option<u64> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

/// Total size of a directory tree, or 0 if it is not there.
fn dir_bytes(dir: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries
        .flatten()
        .map(|e| match e.file_type() {
            Ok(t) if t.is_dir() => dir_bytes(&e.path()),
            _ => e.metadata().map(|m| m.len()).unwrap_or(0),
        })
        .sum()
}

/// Task names come from config keys; sanitise so `a/b` cannot escape the dir.
fn safe(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

impl Cache {
    pub fn new(cache_dir: &Path) -> Result<Self> {
        Self::with_shared(cache_dir, None, false)
    }

    /// Sets the byte budget for the local store. `None` keeps everything.
    pub fn with_budget(mut self, budget: Option<u64>) -> Self {
        self.budget = budget;
        self
    }

    /// Adds a second store, read after the local one.
    ///
    /// A hit there is copied into the local cache on the way past, so the network
    /// round trip happens once rather than on every build.
    pub fn with_shared(cache_dir: &Path, shared: Option<PathBuf>, push: bool) -> Result<Self> {
        let runs = cache_dir.join("runs");
        let artifacts = cache_dir.join("artifacts");
        for d in [&runs, &artifacts] {
            std::fs::create_dir_all(d)
                .with_context(|| format!("creating cache dir {}", d.display()))?;
        }
        if let Some(dir) = &shared {
            // A shared directory that cannot be created is a configuration
            // mistake worth reporting now rather than as a silent stream of
            // misses later.
            std::fs::create_dir_all(dir.join("runs"))
                .with_context(|| format!("creating shared cache {}", dir.display()))?;
            std::fs::create_dir_all(dir.join("artifacts"))?;
        }
        Ok(Cache {
            runs,
            artifacts,
            shared,
            push,
            budget: None,
            written: std::sync::atomic::AtomicU64::new(0),
        })
    }

    fn shared_record(&self, task: &str, hash: &str) -> Option<PathBuf> {
        Some(
            self.shared
                .as_ref()?
                .join("runs")
                .join(safe(task))
                .join(format!("{}.json", &hash[..hash.len().min(32)])),
        )
    }

    fn shared_artifacts(&self, task: &str, hash: &str) -> Option<PathBuf> {
        Some(
            self.shared
                .as_ref()?
                .join("artifacts")
                .join(safe(task))
                .join(&hash[..hash.len().min(32)]),
        )
    }

    fn record_path(&self, task: &str, hash: &str) -> PathBuf {
        self.runs
            .join(safe(task))
            .join(format!("{}.json", &hash[..hash.len().min(32)]))
    }

    fn artifact_dir(&self, task: &str, hash: &str) -> PathBuf {
        self.artifacts
            .join(safe(task))
            .join(&hash[..hash.len().min(32)])
    }

    /// The record for this exact key, if one exists.
    ///
    /// Addressed by hash rather than by task, so alternating between two sets of
    /// inputs hits both times instead of always overwriting the other's result.
    pub fn load(&self, task: &str, hash: &str) -> Option<Record> {
        if let Ok(bytes) = std::fs::read(self.record_path(task, hash))
            && let Ok(rec) = serde_json::from_slice::<Record>(&bytes)
        {
            self.touch(task, hash);
            return Some(rec);
        }
        // Fall through to the shared store, pulling anything found into the local
        // one so the next build does not pay for it again.
        let remote = self.shared_record(task, hash)?;
        let bytes = std::fs::read(remote).ok()?;
        let rec: Record = serde_json::from_slice(&bytes).ok()?;
        let _ = self.pull_from_shared(task, hash, &rec);
        self.touch(task, hash);
        Some(rec)
    }

    /// Records that a result was used, by writing the time into a marker file
    /// beside it.
    ///
    /// The record itself is not rewritten: it carries a fingerprint with one
    /// hash per input file, so a large crate closure makes it big, and paying
    /// that write on every cache hit would tax the fast path to speed up
    /// eviction. A marker costs one small write.
    ///
    /// The time is the marker's *contents*, not its mtime. Relying on mtime
    /// failed on Windows: re-creating an already-empty file truncates nothing,
    /// so NTFS does not treat it as a write and the timestamp never moves —
    /// every entry then looks equally stale and eviction falls back to write
    /// order, which is the bug this exists to fix. Content also survives a
    /// cache directory being copied, which mtime does not reliably do.
    fn touch(&self, task: &str, hash: &str) {
        let path = self.marker_path(task, hash);
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(path, now_millis().to_string());
    }

    fn marker_path(&self, task: &str, hash: &str) -> PathBuf {
        self.runs
            .join(safe(task))
            .join(format!("{}.used", &hash[..hash.len().min(32)]))
    }

    /// Copies a shared result into the local cache.
    fn pull_from_shared(&self, task: &str, hash: &str, rec: &Record) -> Result<()> {
        if rec.archived
            && let Some(src) = self.shared_artifacts(task, hash)
        {
            copy_tree(&src, &self.artifact_dir(task, hash), &rec.outputs)?;
        }
        let dest = self.record_path(task, hash);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(dest, serde_json::to_vec_pretty(rec)?)?;
        Ok(())
    }

    /// The most recent record for a task, whatever its key — used by `why` to
    /// say what changed since last time.
    pub fn load_latest(&self, task: &str) -> Option<Record> {
        let dir = self.runs.join(safe(task));
        let mut newest: Option<(std::time::SystemTime, PathBuf)> = None;
        for e in std::fs::read_dir(dir).ok()?.flatten() {
            // Only records. The directory also holds `.used` markers, which are
            // empty and touched on every hit — so the newest file here is
            // usually a marker, and parsing it would make this return nothing.
            if e.path().extension().and_then(|x| x.to_str()) != Some("json") {
                continue;
            }
            let Ok(m) = e.metadata() else { continue };
            let Ok(t) = m.modified() else { continue };
            if newest.as_ref().is_none_or(|(bt, _)| t > *bt) {
                newest = Some((t, e.path()));
            }
        }
        let (_, path) = newest?;
        serde_json::from_slice(&std::fs::read(path).ok()?).ok()
    }

    pub fn store(&self, record: &Record) -> Result<()> {
        let path = self.record_path(&record.task, &record.fingerprint.hash);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(record)?)?;
        // Atomic rename: a crash mid-write must not leave a corrupt record that
        // would be read back as a false cache hit.
        std::fs::rename(&tmp, &path)?;
        // A freshly stored result counts as used: it was produced for this run.
        self.touch(&record.task, &record.fingerprint.hash);
        self.note_written(&record.task, &record.fingerprint.hash);

        // Contributing to a shared cache is opt-in: see the note on SharedCache.
        if self.push
            && let Some(remote) = self.shared_record(&record.task, &record.fingerprint.hash)
        {
            if let Some(parent) = remote.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if record.archived
                && let Some(dest) = self.shared_artifacts(&record.task, &record.fingerprint.hash)
            {
                let _ = copy_tree(
                    &self.artifact_dir(&record.task, &record.fingerprint.hash),
                    &dest,
                    &record.outputs,
                );
            }
            let _ = std::fs::write(remote, serde_json::to_vec_pretty(record)?);
        }
        Ok(())
    }

    /// Copies a task's declared outputs into the store. Returns the bytes stored,
    /// or `None` when the outputs exceed [`MAX_ARCHIVE_BYTES`].
    pub fn archive(
        &self,
        task: &str,
        hash: &str,
        root: &Path,
        files: &[String],
    ) -> Result<Option<u64>> {
        let total: u64 = files
            .iter()
            .filter_map(|f| std::fs::metadata(root.join(f)).ok())
            .map(|m| m.len())
            .sum();
        if total > MAX_ARCHIVE_BYTES {
            return Ok(None);
        }
        let dir = self.artifact_dir(task, hash);
        // Rebuild from scratch: a half-written archive from a previous crash must
        // not be mistaken for a complete one.
        let _ = std::fs::remove_dir_all(&dir);
        for rel in files {
            let dest = dir.join(rel);
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(root.join(rel), &dest).with_context(|| format!("archiving {rel}"))?;
        }
        Ok(Some(total))
    }

    /// Copies a stored result back into the workspace. `false` if incomplete.
    pub fn restore(&self, task: &str, hash: &str, root: &Path, files: &[String]) -> Result<bool> {
        let dir = self.artifact_dir(task, hash);
        for rel in files {
            let src = dir.join(rel);
            if !src.is_file() {
                return Ok(false);
            }
        }
        for rel in files {
            let dest = root.join(rel);
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(dir.join(rel), &dest).with_context(|| format!("restoring {rel}"))?;
        }
        Ok(true)
    }

    /// Drops least-recently-used results until the store fits its budget.
    ///
    /// "Recently used" means read, not written. Ordering by write time — which
    /// is what this did before — evicts the entry you hit forty times a day in
    /// favour of a one-off produced last night on a branch you have deleted,
    /// which is precisely backwards for the access pattern a cache exists to
    /// serve.
    fn sweep(&self) {
        let Some(budget) = self.budget else { return };
        let mut entries = self.entries();
        let mut total: u64 = entries.iter().map(|e| e.bytes).sum();
        if total <= budget {
            return;
        }
        // Oldest use first.
        entries.sort_by_key(|e| e.used_at);
        let (mut dropped, mut freed) = (0usize, 0u64);
        for e in entries {
            if total <= budget {
                break;
            }
            let _ = std::fs::remove_file(&e.record);
            let _ = std::fs::remove_file(&e.marker);
            let _ = std::fs::remove_dir_all(&e.artifacts);
            total = total.saturating_sub(e.bytes);
            dropped += 1;
            freed += e.bytes;
        }
        // Recorded so a miss that a sweep caused can be explained rather than
        // looking like the cache simply forgot.
        if dropped > 0 {
            let at = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let sweep = Sweep { at, dropped, freed };
            if let Ok(bytes) = serde_json::to_vec_pretty(&sweep) {
                let _ = std::fs::write(self.sweep_path(), bytes);
            }
        }
    }

    fn sweep_path(&self) -> PathBuf {
        self.runs
            .parent()
            .unwrap_or(&self.runs)
            .join("last-sweep.json")
    }

    /// What the last eviction dropped, if one has happened.
    pub fn last_sweep(&self) -> Option<Sweep> {
        serde_json::from_slice(&std::fs::read(self.sweep_path()).ok()?).ok()
    }

    /// Size of the shared store, reported but never evicted from.
    pub fn shared_bytes(&self) -> Option<u64> {
        let dir = self.shared.as_ref()?;
        Some(dir_bytes(&dir.join("runs")) + dir_bytes(&dir.join("artifacts")))
    }

    /// Every stored result, with its size and last use.
    ///
    /// Only the local store is walked. A shared cache is not this machine's to
    /// garbage-collect: evicting from it would delete results other people are
    /// still reading, on the strength of one machine's budget.
    fn entries(&self) -> Vec<Entry> {
        let mut out = Vec::new();
        let Ok(tasks) = std::fs::read_dir(&self.runs) else {
            return out;
        };
        for task in tasks.flatten() {
            let Ok(records) = std::fs::read_dir(task.path()) else {
                continue;
            };
            let task_name = task.file_name().to_string_lossy().to_string();
            for rec in records.flatten() {
                let path = rec.path();
                if path.extension().and_then(|e| e.to_str()) != Some("json") {
                    continue;
                }
                let Some(hash) = path.file_stem().map(|h| h.to_string_lossy().to_string()) else {
                    continue;
                };
                let record_bytes = rec.metadata().map(|m| m.len()).unwrap_or(0);
                let marker = path.with_extension("used");
                let artifacts = self.artifacts.join(&task_name).join(&hash);
                // A record written before markers existed has never been read
                // since; its own mtime is the best evidence available.
                let used_at = read_millis(&marker).unwrap_or_else(|| {
                    rec.metadata()
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| d.as_millis() as u64)
                        .unwrap_or(0)
                });
                out.push(Entry {
                    used_at,
                    bytes: record_bytes + dir_bytes(&artifacts),
                    record: path,
                    artifacts,
                    marker,
                });
            }
        }
        out
    }

    /// What the local store is holding, for `turborust cache`.
    pub fn stats(&self) -> Stats {
        let mut stats = Stats::default();
        for e in self.entries() {
            let task = e
                .record
                .parent()
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            stats.bytes += e.bytes;
            stats.records += 1;
            let slot = stats.by_task.entry(task).or_insert((0, 0));
            slot.0 += e.bytes;
            slot.1 += 1;
        }
        stats
    }

    pub fn budget(&self) -> Option<u64> {
        self.budget
    }

    /// Tracks bytes added, and sweeps once enough has accumulated to be worth
    /// the walk.
    fn note_written(&self, task: &str, hash: &str) {
        let Some(budget) = self.budget else { return };
        let added = dir_bytes(&self.artifact_dir(task, hash));
        let before = self
            .written
            .fetch_add(added, std::sync::atomic::Ordering::Relaxed);
        if before + added >= budget / SWEEP_EVERY {
            self.written.store(0, std::sync::atomic::Ordering::Relaxed);
            self.sweep();
        }
    }

    pub fn clear(&self) -> Result<()> {
        for d in [&self.runs, &self.artifacts] {
            if d.exists() {
                std::fs::remove_dir_all(d)?;
                std::fs::create_dir_all(d)?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fp(files: &[(&str, &str)]) -> Fingerprint {
        combine(
            files
                .iter()
                .map(|(a, b)| (a.to_string(), b.to_string()))
                .collect(),
            BTreeMap::new(),
        )
    }

    #[test]
    fn parses_sizes_in_both_decimal_and_binary_units() {
        use crate::config::parse_size;
        assert_eq!(parse_size("2048").unwrap(), Some(2048));
        assert_eq!(parse_size("1KiB").unwrap(), Some(1024));
        assert_eq!(parse_size("1kb").unwrap(), Some(1000));
        assert_eq!(parse_size("10GiB").unwrap(), Some(10 * 1024 * 1024 * 1024));
        // Zero is "keep everything", not "keep nothing" — a budget of zero
        // bytes would make every store evict itself immediately.
        assert_eq!(parse_size("0").unwrap(), None);
        assert!(parse_size("10 furlongs").is_err());
        assert!(parse_size("-1").is_err());
    }

    /// Builds a store holding `n` results for `task`, each with one output file
    /// of `bytes` bytes. Returns the cache and the workspace root.
    fn store_with(tag: &str, budget: Option<u64>, keys: &[&str], bytes: usize) -> (Cache, PathBuf) {
        let root = std::env::temp_dir().join(format!("tr-evict-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let cache = Cache::new(&root.join("cache")).unwrap().with_budget(budget);
        for key in keys {
            let out = format!("{key}.bin");
            std::fs::write(root.join(&out), vec![b'x'; bytes]).unwrap();
            cache
                .archive("t", key, &root, std::slice::from_ref(&out))
                .unwrap();
            cache
                .store(&Record {
                    task: "t".into(),
                    // Records are addressed by their fingerprint's hash, so the
                    // keys have to actually differ or they overwrite each other.
                    fingerprint: Fingerprint {
                        hash: (*key).to_string(),
                        files: BTreeMap::new(),
                        meta: BTreeMap::new(),
                    },
                    exit_code: 0,
                    duration_ms: 1,
                    outputs: vec![out],
                    archived: true,
                    output_hash: None,
                })
                .unwrap();
        }
        (cache, root)
    }

    #[test]
    fn eviction_keeps_what_is_read_and_drops_what_is_merely_recent() {
        // Four results, then a budget that only fits some of them.
        let (cache, root) = store_with("lru", None, &["aaa", "bbb", "ccc"], 4096);

        // `aaa` is the oldest by write time and the newest by use. Ordering by
        // write time — which is what this did before — would evict exactly it.
        std::thread::sleep(std::time::Duration::from_millis(5));
        assert!(cache.load("t", "aaa").is_some(), "warm-up read");

        let budget = 6 * 1024;
        let cache = cache.with_budget(Some(budget));
        cache.sweep();

        assert!(
            cache.load("t", "aaa").is_some(),
            "the entry being read must survive the entry merely written later"
        );
        assert!(cache.stats().bytes <= budget, "store must fit its budget");
        let sweep = cache.last_sweep().expect("an eviction was recorded");
        assert!(sweep.dropped > 0 && sweep.freed > 0);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_second_read_records_a_later_use_than_the_first() {
        // The direct form of what eviction depends on. Windows caught this the
        // hard way: when the marker was empty and its mtime was the timestamp,
        // re-creating it truncated nothing, NTFS did not count it as a write,
        // and every entry stayed equally stale forever.
        let (cache, root) = store_with("touch", None, &["aaa"], 16);
        let marker = cache.marker_path("t", "aaa");
        let first = read_millis(&marker).expect("storing records a use");
        std::thread::sleep(std::time::Duration::from_millis(5));
        cache.load("t", "aaa").unwrap();
        let second = read_millis(&marker).expect("reading records a use");
        assert!(
            second > first,
            "a read must advance the use time ({first} -> {second})"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn no_budget_keeps_everything() {
        let (cache, root) = store_with("nobudget", None, &["aaa", "bbb", "ccc"], 4096);
        cache.sweep();
        assert_eq!(cache.stats().records, 3);
        assert!(cache.last_sweep().is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_use_marker_does_not_look_like_a_record() {
        // `.used` markers live beside records and are touched on every hit, so
        // the newest file in the directory is usually a marker. load_latest must
        // skip them or it returns nothing for a task that is being used.
        let (cache, root) = store_with("marker", None, &["aaa"], 16);
        assert!(cache.load("t", "aaa").is_some());
        assert!(
            cache.load_latest("t").is_some(),
            "a touched marker must not hide the record it belongs to"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn outputs_hash_by_content_not_by_name() {
        let dir = std::env::temp_dir().join(format!("tr-outhash-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a"), b"same").unwrap();
        let first = hash_outputs(&dir, &["a".into()]).unwrap();

        // Rewriting identical bytes must not move the hash: that is the whole
        // point of keying a dependent on outputs rather than on a re-run.
        std::fs::write(dir.join("a"), b"same").unwrap();
        assert_eq!(hash_outputs(&dir, &["a".into()]).unwrap(), first);

        std::fs::write(dir.join("a"), b"different").unwrap();
        assert_ne!(hash_outputs(&dir, &["a".into()]).unwrap(), first);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_task_that_produced_nothing_has_no_output_hash() {
        let dir = std::env::temp_dir().join(format!("tr-outnone-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // Declared but absent. Hashing the empty set would give every such task
        // the same stamp, which would tell a dependent "nothing changed" about a
        // build that produced none of what it promised.
        assert_eq!(hash_outputs(&dir, &[]), None);
        assert_eq!(hash_outputs(&dir, &["missing".into()]), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_dependency_stamp_renders_as_its_rule_and_a_short_hash() {
        let h = "5f16c0708c3d4e2a";
        assert_eq!(
            Change::Meta {
                key: "dep:gen".into(),
                from: format!("out:{h}"),
                to: format!("out:{h}"),
            }
            .render(),
            "~ [dep:gen]  outputs b3:5f16c070 -> outputs b3:5f16c070"
        );
        // Anything that is not a stamp still prints verbatim.
        assert_eq!(
            Change::Meta {
                key: "env:PROFILE".into(),
                from: "dev".into(),
                to: "release".into(),
            }
            .render(),
            "~ [env:PROFILE]  dev -> release"
        );
    }

    #[test]
    fn key_is_order_independent_but_content_sensitive() {
        assert_eq!(
            fp(&[("a", "1"), ("b", "2")]).hash,
            fp(&[("b", "2"), ("a", "1")]).hash
        );
        assert_ne!(fp(&[("a", "1")]).hash, fp(&[("a", "2")]).hash);
    }

    #[test]
    fn renaming_a_file_changes_the_key() {
        assert_ne!(fp(&[("a", "1")]).hash, fp(&[("b", "1")]).hash);
    }

    #[test]
    fn meta_participates_in_the_key() {
        let a = combine(
            BTreeMap::new(),
            [("cmd".to_string(), "x".to_string())].into(),
        );
        let b = combine(
            BTreeMap::new(),
            [("cmd".to_string(), "y".to_string())].into(),
        );
        assert_ne!(a.hash, b.hash);
    }

    #[test]
    fn diff_names_the_changed_file() {
        let old = fp(&[("src/a.rs", "aa"), ("src/gone.rs", "cc")]);
        let new = fp(&[("src/a.rs", "bb"), ("src/new.rs", "dd")]);
        let changes = diff(&old, &new);
        assert!(
            changes
                .iter()
                .any(|c| matches!(c, Change::Modified { path, .. } if path == "src/a.rs"))
        );
        assert!(
            changes
                .iter()
                .any(|c| matches!(c, Change::Added(p, _) if p == "src/new.rs"))
        );
        assert!(
            changes
                .iter()
                .any(|c| matches!(c, Change::Removed(p, _) if p == "src/gone.rs"))
        );
    }

    #[test]
    fn matcher_respects_excludes() {
        let m = Matcher::new(&["crates/**/*.rs".into()], &["**/target/**".into()]).unwrap();
        assert!(m.matches("crates/api/src/main.rs"));
        assert!(!m.matches("crates/api/target/debug/x.rs"));
        assert!(!m.matches("README.md"));
    }

    #[test]
    fn empty_include_matches_nothing() {
        let m = Matcher::new(&[], &[]).unwrap();
        assert!(m.is_empty());
        assert!(!m.matches("anything.rs"));
    }
}
