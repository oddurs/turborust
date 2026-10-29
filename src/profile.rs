//! Where the seconds went (cairn 0075).
//!
//! `doctor`'s other checks read configuration and say what a good one looks
//! like. This reads what actually happened: every task run is appended to a
//! small log under `.turborust/`, with how long it took, whether it hit, and if
//! not, which inputs moved. `doctor --profile` turns that into "you waited
//! 19 minutes on `build`, and it missed on 28 runs because `PROFILE` changed".
//!
//! Local only, and bounded: it is a measurement of your machine for you, never
//! sent anywhere (0058 still holds), and `[project] profile = false` or
//! `TURBORUST_PROFILE=off` stops it being written at all.

use crate::cache::Change;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Where the log lives, under the checkout's `.turborust/`.
pub const LOG: &str = "profile/runs.jsonl";

/// Past this the log is cut to its newer half. About 20,000 runs: months of
/// ordinary work, and small enough to read whole on every `doctor --profile`.
const MAX_BYTES: u64 = 4 * 1024 * 1024;

/// One task run, as the log keeps it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunRecord {
    /// Seconds since the epoch.
    pub at: u64,
    pub node: String,
    pub ms: u64,
    pub cached: bool,
    pub ok: bool,
    /// Why it did not hit: one entry per kind of change, from [`reasons`].
    /// Empty for a hit.
    #[serde(default)]
    pub why: Vec<String>,
}

/// Whether a run should be logged at all.
pub fn enabled(configured: Option<bool>) -> bool {
    let env_off = std::env::var("TURBORUST_PROFILE")
        .is_ok_and(|v| matches!(v.as_str(), "off" | "0" | "false"));
    configured.unwrap_or(true) && !env_off
}

/// What moved between two keys, as one entry per kind of change.
///
/// Files collapse to `files`, because a different file moves every time and
/// "which file" is never the pattern; everything else keeps its name, because
/// "`env:PROFILE` again" is exactly the pattern worth seeing.
pub fn reasons(changes: &[Change]) -> Vec<String> {
    let mut out: Vec<String> = changes
        .iter()
        .map(|c| match c {
            Change::Meta { key, .. } => key
                .strip_prefix("global:")
                .unwrap_or(key.as_str())
                .to_string(),
            _ => "files".to_string(),
        })
        .collect();
    out.sort();
    out.dedup();
    out
}

pub struct Log {
    path: PathBuf,
}

impl Log {
    pub fn at(cache_dir: &Path) -> Log {
        Log {
            path: cache_dir.join(LOG),
        }
    }

    pub fn append(&self, record: &RunRecord) -> std::io::Result<()> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut line = serde_json::to_string(record).map_err(std::io::Error::other)?;
        line.push('\n');
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        f.write_all(line.as_bytes())?;
        if f.metadata()?.len() > MAX_BYTES {
            drop(f);
            self.trim()?;
        }
        Ok(())
    }

    /// Keeps the newer half. Written aside and renamed, so a reader never sees
    /// a half-written log.
    fn trim(&self) -> std::io::Result<()> {
        let text = std::fs::read_to_string(&self.path)?;
        let lines: Vec<&str> = text.lines().collect();
        let keep = &lines[lines.len() / 2..];
        let tmp = self.path.with_extension("jsonl.tmp");
        std::fs::write(&tmp, keep.join("\n") + "\n")?;
        std::fs::rename(&tmp, &self.path)
    }

    /// Every record still readable. A line that does not parse, from an
    /// interrupted write, is skipped rather than failing the report.
    pub fn load(&self) -> Vec<RunRecord> {
        std::fs::read_to_string(&self.path)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    }
}

/// One node's runs over the window.
#[derive(Debug, Clone, PartialEq)]
pub struct NodeProfile {
    pub node: String,
    pub runs: usize,
    pub hits: usize,
    pub failed: usize,
    /// All the time its runs took, hits included.
    pub wall_ms: u64,
    /// The time spent on runs that did not hit: what a fix could win back.
    pub miss_ms: u64,
    /// The reason most misses share, and on how many it appeared.
    pub dominant: Option<(String, usize)>,
    pub misses: usize,
    pub regression: Option<Regression>,
}

/// A node's misses taking longer than they used to.
#[derive(Debug, Clone, PartialEq)]
pub struct Regression {
    pub median_ms: u64,
    pub baseline_ms: u64,
    /// How many earlier misses the baseline is the median of.
    pub baseline_runs: usize,
    /// When the baseline ends: the window's start.
    pub since: u64,
}

impl NodeProfile {
    /// What the reader could do about this node, and whether it is something
    /// they can change at all.
    pub fn advice(&self) -> Option<Advice> {
        let (reason, count) = self.dominant.as_ref()?;
        Some(advise(reason, *count, self.misses))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Advice {
    pub why: String,
    pub fix: Option<String>,
}

impl Advice {
    pub fn actionable(&self) -> bool {
        self.fix.is_some()
    }
}

fn advise(reason: &str, count: usize, misses: usize) -> Advice {
    let of = format!("{count} of {misses} misses");
    let plain = |why: String| Advice { why, fix: None };
    if let Some(var) = reason.strip_prefix("env:") {
        return Advice {
            why: format!("keyed on env `{var}`, which changed on {of}"),
            fix: Some(format!(
                "if `{var}` does not change the output, move it from `env_keys` to `pass_through_env`"
            )),
        };
    }
    if let Some(var) = reason.strip_prefix("env_file:") {
        return Advice {
            why: format!("`{var}` from an env file changed on {of}"),
            fix: Some(format!(
                "if `{var}` does not change the output, keep it out of the env file this task reads"
            )),
        };
    }
    if let Some(dep) = reason.strip_prefix("dep:") {
        return plain(format!("`{dep}` produced a new result on {of}"));
    }
    match reason {
        "no inputs" => Advice {
            why: "declares no inputs, so it is never cached".into(),
            fix: Some("declare `inputs`, or `cargo = \"<crate>\"` to derive them".into()),
        },
        "loose env" => Advice {
            why: "runs with `env_mode = \"loose\"`, which is never cached".into(),
            fix: Some(
                "set `env_mode = \"strict\"` and declare the variables it needs in `env_keys`"
                    .into(),
            ),
        },
        "files" => plain(format!(
            "its sources changed on {of}: the cache doing its job"
        )),
        "toolchain" => plain(format!("the Rust toolchain changed on {of}")),
        "lockfile" => plain(format!("Cargo.lock changed on {of}")),
        "config" => plain(format!("turborust.toml changed on {of}")),
        "cmd" => plain(format!(
            "its command changed on {of} (arguments after `--` count)"
        )),
        "first run" => plain(format!("{of} had no earlier result to compare with")),
        "forced" => plain(format!("{of} were forced (`--force`, `verify`)")),
        "cache disabled" => plain("has `cache = \"disabled\"`".into()),
        "unservable" => plain(format!(
            "{of} had the previous key, but it had failed or its outputs were gone"
        )),
        other => plain(format!("its `{other}` changed on {of}")),
    }
}

/// Every node with runs since `since`, ranked by what the reader can act on:
/// the time a fixable cause cost first, then the rest by the time they cost.
pub fn profile(records: &[RunRecord], since: u64) -> Vec<NodeProfile> {
    let mut by_node: BTreeMap<&str, Vec<&RunRecord>> = BTreeMap::new();
    for r in records {
        by_node.entry(&r.node).or_default().push(r);
    }
    let mut out: Vec<NodeProfile> = by_node
        .into_iter()
        .filter_map(|(node, runs)| {
            let (before, window): (Vec<&RunRecord>, Vec<&RunRecord>) =
                runs.into_iter().partition(|r| r.at < since);
            if window.is_empty() {
                return None;
            }
            let misses: Vec<&&RunRecord> = window.iter().filter(|r| !r.cached).collect();
            let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
            for r in &misses {
                for w in &r.why {
                    *counts.entry(w.as_str()).or_default() += 1;
                }
            }
            // A cause the reader can fix is named whenever it explains a fair
            // share of misses, even if source edits explain more: sources move
            // on almost every miss, so "files" would otherwise always win and
            // hide the one variable that is costing a rebuild each time.
            let top = |actionable: bool| {
                counts
                    .iter()
                    .filter(|(w, n)| !actionable || advise(w, **n, 0).actionable())
                    .max_by_key(|(_, n)| **n)
                    .map(|(w, n)| (w.to_string(), *n))
            };
            let dominant = top(true)
                .filter(|(_, n)| n * 4 >= misses.len())
                .or_else(|| top(false));
            Some(NodeProfile {
                node: node.to_string(),
                runs: window.len(),
                hits: window.iter().filter(|r| r.cached).count(),
                failed: window.iter().filter(|r| !r.ok).count(),
                wall_ms: window.iter().map(|r| r.ms).sum(),
                miss_ms: misses.iter().map(|r| r.ms).sum(),
                dominant,
                misses: misses.len(),
                regression: regression(&before, &misses, since),
            })
        })
        .collect();
    out.sort_by_key(|p| {
        let actionable = p.advice().is_some_and(|a| a.actionable());
        (std::cmp::Reverse(actionable), std::cmp::Reverse(p.miss_ms))
    });
    out
}

/// A slowdown worth saying: the window's successful misses against the ones
/// before it, by median, when both have enough runs to mean something and the
/// difference is both relative and absolute — 20% of a 50ms task is noise.
fn regression(before: &[&RunRecord], window: &[&&RunRecord], since: u64) -> Option<Regression> {
    let built = |rs: &mut dyn Iterator<Item = &RunRecord>| -> Vec<u64> {
        let mut v: Vec<u64> = rs.filter(|r| !r.cached && r.ok).map(|r| r.ms).collect();
        v.sort_unstable();
        v
    };
    let base = built(&mut before.iter().copied());
    let now = built(&mut window.iter().map(|r| **r));
    if base.len() < 5 || now.len() < 3 {
        return None;
    }
    let (b, n) = (median(&base), median(&now));
    (n > b + b / 5 && n - b >= 1000).then_some(Regression {
        median_ms: n,
        baseline_ms: b,
        baseline_runs: base.len(),
        since,
    })
}

fn median(sorted: &[u64]) -> u64 {
    sorted[sorted.len() / 2]
}

/// The report `doctor --profile` prints.
pub fn render(profiles: &[NodeProfile], window: &str, timings: Option<&Timings>) -> String {
    let mut out = String::new();
    let total: u64 = profiles.iter().map(|p| p.wall_ms).sum();
    if profiles.is_empty() {
        out.push_str(&format!(
            "\n  \u{1b}[2mno task runs recorded in the {window}. Runs are logged as \
             `run` and `up` execute them.\u{1b}[0m\n"
        ));
    } else {
        out.push_str(&format!(
            "\n  \u{1b}[1m{window}\u{1b}[0m — {} of waiting across {} node{}\n",
            human_ms(total),
            profiles.len(),
            if profiles.len() == 1 { "" } else { "s" }
        ));
    }
    for p in profiles {
        let failed = if p.failed > 0 {
            format!("   {} failed", p.failed)
        } else {
            String::new()
        };
        out.push_str(&format!(
            "\n  \u{1b}[1m{:<12}\u{1b}[0m {:>6}   {} run{}   cache {}/{}{failed}\n",
            p.node,
            human_ms(p.wall_ms),
            p.runs,
            if p.runs == 1 { "" } else { "s" },
            p.hits,
            p.runs
        ));
        match p.advice() {
            Some(a) => {
                out.push_str(&format!("      {}\n", a.why));
                if let Some(fix) = a.fix {
                    out.push_str(&format!("      \u{1b}[32m→ {fix}\u{1b}[0m\n"));
                }
            }
            None if p.misses == 0 => out.push_str("      \u{1b}[2mevery run hit\u{1b}[0m\n"),
            None => {}
        }
        if let Some(r) = &p.regression {
            out.push_str(&format!(
                "      \u{1b}[33mslower: {} median, +{} against {} run{} before {}\u{1b}[0m\n",
                human_ms(r.median_ms),
                human_ms(r.median_ms - r.baseline_ms),
                r.baseline_runs,
                if r.baseline_runs == 1 { "" } else { "s" },
                date(r.since)
            ));
        }
    }
    match timings {
        Some(t) => out.push_str(&t.render()),
        None => out.push_str(
            "\n  \u{1b}[2mfor where a cold build goes, run `cargo build --timings` once; \
             doctor --profile reads its report.\u{1b}[0m\n",
        ),
    }
    out
}

/// `1.2s`, `19m`, `2h 5m`: as precise as a reader can use.
pub fn human_ms(ms: u64) -> String {
    let s = ms / 1000;
    match s {
        0 => format!("{ms}ms"),
        1..=59 if ms < 10_000 => format!("{:.1}s", ms as f64 / 1000.0),
        1..=59 => format!("{s}s"),
        60..=3599 => format!("{}m", s / 60),
        _ => format!("{}h {}m", s / 3600, (s % 3600) / 60),
    }
}

/// `2026-10-29`, from seconds since the epoch, in UTC.
fn date(secs: u64) -> String {
    // Days to civil date (Howard Hinnant's algorithm), to avoid a dependency
    // for one line of output.
    let z = (secs / 86_400) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Where one cold build went, from cargo's own `--timings` report.
#[derive(Debug, Clone, PartialEq)]
pub struct Timings {
    pub report: PathBuf,
    /// The build's wall time, as cargo recorded it.
    pub total_s: Option<f64>,
    /// The slowest crates, slowest first: name and seconds, summed across
    /// that crate's units (a lib and its build script, say).
    pub crates: Vec<(String, f64)>,
    /// Time in the final binaries, which is codegen of the binary crate plus
    /// linking. cargo only splits the two when it records a `link` section.
    pub binaries_s: f64,
    pub link_s: Option<f64>,
}

impl Timings {
    fn render(&self) -> String {
        let mut out = format!(
            "\n  \u{1b}[1mlast cold build\u{1b}[0m   {}  \u{1b}[2m({})\u{1b}[0m\n",
            self.total_s
                .map(|s| human_ms((s * 1000.0) as u64))
                .unwrap_or_else(|| "?".into()),
            self.report.display()
        );
        let names: Vec<String> = self
            .crates
            .iter()
            .take(5)
            .map(|(n, s)| format!("{n} {}", human_ms((s * 1000.0) as u64)))
            .collect();
        if !names.is_empty() {
            out.push_str(&format!("      slowest crates: {}\n", names.join(", ")));
        }
        match self.link_s {
            Some(l) => out.push_str(&format!(
                "      linking: {}\n",
                human_ms((l * 1000.0) as u64)
            )),
            None if self.binaries_s > 0.0 => out.push_str(&format!(
                "      final binaries (compile + link): {}\n",
                human_ms((self.binaries_s * 1000.0) as u64)
            )),
            None => {}
        }
        out
    }
}

/// The newest `--timings` report under the workspace's target directories.
pub fn latest_timings(root: &Path) -> Option<Timings> {
    let mut dirs = vec![root.join("target").join("cargo-timings")];
    // Nodes with `target_dir = "split"` build under target/turborust/<node>.
    if let Ok(split) = std::fs::read_dir(root.join("target").join("turborust")) {
        dirs.extend(split.flatten().map(|e| e.path().join("cargo-timings")));
    }
    let newest = dirs
        .iter()
        .filter_map(|d| std::fs::read_dir(d).ok())
        .flat_map(|d| d.flatten())
        .filter(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            // `cargo-timing.html` is a copy of the latest stamped one.
            name.starts_with("cargo-timing-") && name.ends_with(".html")
        })
        .max_by_key(|e| e.metadata().and_then(|m| m.modified()).ok())?
        .path();
    let html = std::fs::read_to_string(&newest).ok()?;
    parse_timings(&html, newest)
}

#[derive(Deserialize)]
struct Unit {
    name: String,
    #[serde(default)]
    target: String,
    duration: f64,
    #[serde(default)]
    sections: Option<Vec<(String, Section)>>,
}

#[derive(Deserialize)]
struct Section {
    start: f64,
    end: f64,
}

/// Reads the `UNIT_DATA` array cargo embeds in its report. The report is HTML
/// for people; this array is the one structured thing in it, and its fields
/// have been stable across cargo releases since `--timings` stabilised.
pub fn parse_timings(html: &str, report: PathBuf) -> Option<Timings> {
    let start = html.find("const UNIT_DATA = ")? + "const UNIT_DATA = ".len();
    let end = start + html[start..].find("];")? + 1;
    let units: Vec<Unit> = serde_json::from_str(&html[start..end]).ok()?;
    let total_s = html
        .split("Total time:</td><td>")
        .nth(1)
        .and_then(|rest| rest.split('s').next())
        .and_then(|n| n.trim().parse().ok());

    let mut by_crate: BTreeMap<String, f64> = BTreeMap::new();
    let mut binaries_s = 0.0;
    let mut link_s = None::<f64>;
    for u in &units {
        let link = u
            .sections
            .iter()
            .flatten()
            .find(|(name, _)| name == "link")
            .map(|(_, s)| s.end - s.start);
        if let Some(l) = link {
            *link_s.get_or_insert(0.0) += l;
        }
        if u.target.contains("\"bin\"") {
            binaries_s += u.duration;
        } else {
            *by_crate.entry(u.name.clone()).or_default() += u.duration;
        }
    }
    let mut crates: Vec<(String, f64)> = by_crate.into_iter().collect();
    crates.sort_by(|a, b| b.1.total_cmp(&a.1));
    Some(Timings {
        report,
        total_s,
        crates,
        binaries_s,
        link_s,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(at: u64, node: &str, ms: u64, cached: bool, why: &[&str]) -> RunRecord {
        RunRecord {
            at,
            node: node.into(),
            ms,
            cached,
            ok: true,
            why: why.iter().map(|w| w.to_string()).collect(),
        }
    }

    #[test]
    fn a_change_collapses_to_its_kind() {
        let changes = vec![
            Change::Modified {
                path: "src/a.rs".into(),
                from: "1".into(),
                to: "2".into(),
            },
            Change::Added("src/b.rs".into(), "3".into()),
            Change::Meta {
                key: "env:PROFILE".into(),
                from: "a".into(),
                to: "b".into(),
            },
            Change::Meta {
                key: "global:toolchain".into(),
                from: "a".into(),
                to: "b".into(),
            },
        ];
        assert_eq!(reasons(&changes), ["env:PROFILE", "files", "toolchain"]);
    }

    #[test]
    fn the_fixable_cause_ranks_above_the_bigger_expected_one() {
        let mut log = Vec::new();
        // `check` costs more, but only because its sources keep changing.
        for i in 0..10 {
            log.push(run(100 + i, "check", 60_000, false, &["files"]));
        }
        // `build` costs less, and misses because of one variable.
        for i in 0..10 {
            let why: &[&str] = if i < 8 {
                &["env:PROFILE", "files"]
            } else {
                &["files"]
            };
            log.push(run(100 + i, "build", 30_000, false, why));
        }
        log.push(run(120, "build", 5, true, &[]));
        let p = profile(&log, 50);
        assert_eq!(p[0].node, "build", "{p:#?}");
        assert_eq!(p[0].runs, 11);
        assert_eq!(p[0].hits, 1);
        assert_eq!(p[0].dominant, Some(("env:PROFILE".into(), 8)));
        let a = p[0].advice().unwrap();
        assert_eq!(
            a.why,
            "keyed on env `PROFILE`, which changed on 8 of 10 misses"
        );
        assert!(a.actionable());
        assert_eq!(p[1].node, "check");
        assert!(!p[1].advice().unwrap().actionable());
    }

    #[test]
    fn only_the_window_is_reported_and_earlier_runs_are_its_baseline() {
        let mut log = Vec::new();
        for i in 0..6 {
            log.push(run(10 + i, "build", 60_000, false, &["files"]));
        }
        for i in 0..3 {
            log.push(run(1000 + i, "build", 82_000, false, &["files"]));
        }
        log.push(run(5, "old", 1_000, false, &["files"]));
        let p = profile(&log, 1000);
        assert_eq!(
            p.len(),
            1,
            "a node with no runs in the window is not reported"
        );
        assert_eq!(p[0].runs, 3);
        let r = p[0]
            .regression
            .as_ref()
            .expect("a 22s slowdown is reported");
        assert_eq!(
            (r.median_ms, r.baseline_ms, r.baseline_runs),
            (82_000, 60_000, 6)
        );

        // Small, or relative-only, differences are noise.
        let mut quiet = Vec::new();
        for i in 0..6 {
            quiet.push(run(10 + i, "t", 100, false, &["files"]));
        }
        for i in 0..3 {
            quiet.push(run(1000 + i, "t", 300, false, &["files"]));
        }
        assert_eq!(profile(&quiet, 1000)[0].regression, None);
    }

    #[test]
    fn the_log_is_bounded_and_survives_a_torn_line() {
        let dir = std::env::temp_dir().join(format!("turborust-profile-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let log = Log::at(&dir);
        let record = run(1, "build", 1, false, &["files"]);
        log.append(&record).unwrap();
        // An interrupted write leaves half a line; the next append starts anew.
        std::fs::OpenOptions::new()
            .append(true)
            .open(dir.join(LOG))
            .unwrap()
            .write_all(b"{\"at\":2,\"no")
            .unwrap();
        std::fs::OpenOptions::new()
            .append(true)
            .open(dir.join(LOG))
            .unwrap()
            .write_all(b"\n")
            .unwrap();
        log.append(&record).unwrap();
        assert_eq!(log.load(), vec![record.clone(), record.clone()]);

        let line = serde_json::to_string(&record).unwrap().len() as u64 + 1;
        for _ in 0..(MAX_BYTES / line + 10) {
            log.append(&record).unwrap();
        }
        let size = std::fs::metadata(dir.join(LOG)).unwrap().len();
        assert!(size <= MAX_BYTES, "{size} bytes");
        assert!(
            size > MAX_BYTES / 3,
            "trimming keeps the newer half: {size}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cargo_s_timing_report_is_read_for_crates_and_binaries() {
        let html = r#"<td>Total time:</td><td>32.8s</td>
const UNIT_DATA = [
{"i":1,"name":"web-sys","version":"0.3","mode":"todo","target":"","features":[],"start":0.2,"duration":10.14,"unblocked_units":[],"unblocked_rmeta_units":[],"sections":[["frontend",{"start":0.0,"end":8.58}],["codegen",{"start":8.58,"end":10.14}]]},
{"i":2,"name":"syn","version":"2","mode":"todo","target":"","features":[],"start":0.1,"duration":5.2,"unblocked_units":[],"unblocked_rmeta_units":[],"sections":null},
{"i":3,"name":"syn","version":"1","mode":"todo","target":"","features":[],"start":0.1,"duration":5.07,"unblocked_units":[],"unblocked_rmeta_units":[],"sections":null},
{"i":4,"name":"libc","version":"0.2","mode":"run-custom-build","target":" build-script (run)","features":[],"start":0.1,"duration":0.03,"unblocked_units":[],"unblocked_rmeta_units":[]},
{"i":5,"name":"app","version":"0.1","mode":"todo","target":" app \"bin\"","features":[],"start":30.0,"duration":2.5,"unblocked_units":[],"unblocked_rmeta_units":[],"sections":null}
];
const CONCURRENCY_DATA = [];"#;
        let t = parse_timings(html, PathBuf::from("r.html")).unwrap();
        assert_eq!(t.total_s, Some(32.8));
        assert_eq!(t.crates[0].0, "syn", "both syn units count");
        assert!((t.crates[0].1 - 10.27).abs() < 1e-9);
        assert_eq!(t.crates[1].0, "web-sys");
        assert_eq!(t.binaries_s, 2.5);
        assert_eq!(t.link_s, None);
    }

    #[test]
    fn dates_are_civil() {
        assert_eq!(date(0), "1970-01-01");
        assert_eq!(date(1_793_307_258), "2026-10-29");
    }
}
