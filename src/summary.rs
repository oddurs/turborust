//! Predicting a run, and recording what one did.
//!
//! `--dry-run` is `why`, applied to the whole graph: it answers "what executes
//! and what is cached" without doing anything. `--summarize` leaves the same
//! information behind after a real run, so "why was CI slow on Tuesday" is
//! answerable on Thursday, when the terminal output is gone.
//!
//! Both carry a schema version from the first release. A JSON output shipped
//! without one is unversioned forever.

use crate::cache::{self, Cache};
use crate::engine::Engine;
use crate::plan::Kind;
use anyhow::Result;
use serde::Serialize;
use std::path::{Path, PathBuf};

pub const SCHEMA: u32 = 1;

#[derive(Debug, Serialize)]
pub struct Prediction {
    pub schema: u32,
    pub tasks: Vec<TaskPrediction>,
}

#[derive(Debug, Serialize)]
pub struct TaskPrediction {
    pub task: String,
    pub command: String,
    /// True if this task would be served from cache.
    pub cached: bool,
    pub key: String,
    /// Why it would miss. Empty when it would hit, or when it is uncacheable.
    pub changed: Vec<String>,
    /// Set when the task declares no inputs and so is never cached.
    pub uncacheable: bool,
}

/// What a run would do, without doing any of it.
pub fn predict(engine: &Engine, cache: &Cache) -> Result<Prediction> {
    let mut tasks = Vec::new();
    for name in &engine.plan.order {
        let node = engine.plan.get(name)?;
        if node.kind != Kind::Task {
            continue;
        }
        let command = node.cmd.clone().unwrap_or_default();

        if node.inputs.is_empty() {
            tasks.push(TaskPrediction {
                task: name.clone(),
                command,
                cached: false,
                key: String::new(),
                changed: Vec::new(),
                uncacheable: true,
            });
            continue;
        }

        let fp = engine.fingerprint(node)?;
        let hit = cache.load(name, &fp.hash).is_some_and(|r| r.exit_code == 0);
        // A miss is only useful if it says what moved, which is the same diff
        // `why` renders.
        let changed = if hit {
            Vec::new()
        } else {
            cache
                .load_latest(name)
                .map(|prev| {
                    cache::diff(&prev.fingerprint, &fp)
                        .iter()
                        .take(20)
                        .map(|c| c.render())
                        .collect()
                })
                .unwrap_or_default()
        };
        tasks.push(TaskPrediction {
            task: name.clone(),
            command,
            cached: hit,
            key: fp.short(),
            changed,
            uncacheable: false,
        });
    }
    Ok(Prediction {
        schema: SCHEMA,
        tasks,
    })
}

impl Prediction {
    /// Human-readable form, shaped like `plan` so the two read alike.
    pub fn render(&self) -> String {
        let mut out = String::from("\n");
        for t in &self.tasks {
            let (mark, label) = if t.uncacheable {
                (
                    "\u{1b}[2m·\u{1b}[0m",
                    "would run (no inputs declared)".to_string(),
                )
            } else if t.cached {
                ("\u{1b}[32m✓\u{1b}[0m", format!("cached  b3:{}", t.key))
            } else {
                ("\u{1b}[33m→\u{1b}[0m", format!("would run  b3:{}", t.key))
            };
            out.push_str(&format!("  {mark} \u{1b}[1m{}\u{1b}[0m  {label}\n", t.task));
            out.push_str(&format!("      \u{1b}[2m{}\u{1b}[0m\n", t.command));
            for change in t.changed.iter().take(5) {
                out.push_str(&format!("      \u{1b}[2m{change}\u{1b}[0m\n"));
            }
            out.push('\n');
        }
        let would_run = self.tasks.iter().filter(|t| !t.cached).count();
        out.push_str(&format!(
            "  \u{1b}[2m{} of {} would run; nothing was executed.\u{1b}[0m\n\n",
            would_run,
            self.tasks.len()
        ));
        out
    }
}

#[derive(Debug, Serialize)]
pub struct RunSummary {
    pub schema: u32,
    /// Seconds since the epoch. Not a formatted date: a consumer can format it,
    /// and a formatted one cannot be sorted reliably.
    pub started_at: u64,
    pub duration_ms: u64,
    pub exit_code: i32,
    pub global: crate::global::GlobalHash,
    pub tasks: Vec<TaskOutcomeRecord>,
}

#[derive(Debug, Serialize, Clone)]
pub struct TaskOutcomeRecord {
    pub task: String,
    pub cached: bool,
    pub duration_ms: u64,
    pub key: String,
    pub exit_code: i32,
    /// Inputs that differed from the previous run, when this was a miss.
    pub changed: Vec<String>,
}

/// Writes a summary next to the cache and returns where it went.
pub fn write(cache_dir: &Path, summary: &RunSummary) -> Result<PathBuf> {
    let dir = cache_dir.join("summaries");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{}.json", summary.started_at));
    std::fs::write(&path, serde_json::to_vec_pretty(summary)?)?;
    Ok(path)
}

pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prediction(tasks: Vec<TaskPrediction>) -> Prediction {
        Prediction {
            schema: SCHEMA,
            tasks,
        }
    }

    fn task(name: &str, cached: bool, uncacheable: bool) -> TaskPrediction {
        TaskPrediction {
            task: name.into(),
            command: "cargo build".into(),
            cached,
            key: "abc123".into(),
            changed: if cached {
                vec![]
            } else {
                vec!["~ src/a.rs  b3:1 -> b3:2".into()]
            },
            uncacheable,
        }
    }

    #[test]
    fn the_render_distinguishes_the_three_outcomes() {
        let out = prediction(vec![
            task("hit", true, false),
            task("miss", false, false),
            task("never", false, true),
        ])
        .render();
        assert!(out.contains("cached  b3:abc123"));
        assert!(out.contains("would run  b3:abc123"));
        assert!(out.contains("no inputs declared"));
    }

    #[test]
    fn a_miss_shows_what_moved() {
        let out = prediction(vec![task("miss", false, false)]).render();
        assert!(
            out.contains("src/a.rs"),
            "a miss with no reason is just a log line: {out}"
        );
    }

    #[test]
    fn the_tally_counts_only_what_would_run() {
        let out = prediction(vec![task("a", true, false), task("b", false, false)]).render();
        assert!(out.contains("1 of 2 would run"), "{out}");
        assert!(out.contains("nothing was executed"));
    }

    #[test]
    fn both_formats_carry_a_schema_version() {
        let json = serde_json::to_string(&prediction(vec![])).unwrap();
        assert!(json.contains("\"schema\":1"), "{json}");

        let summary = RunSummary {
            schema: SCHEMA,
            started_at: 1,
            duration_ms: 2,
            exit_code: 0,
            global: crate::global::GlobalHash::default(),
            tasks: vec![],
        };
        assert!(
            serde_json::to_string(&summary)
                .unwrap()
                .contains("\"schema\":1")
        );
    }

    #[test]
    fn a_summary_lands_where_it_says_it_did() {
        let dir = std::env::temp_dir().join(format!("tr-sum-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let summary = RunSummary {
            schema: SCHEMA,
            started_at: 1234,
            duration_ms: 10,
            exit_code: 0,
            global: crate::global::GlobalHash::default(),
            tasks: vec![],
        };
        let path = write(&dir, &summary).unwrap();
        assert!(path.ends_with("1234.json"));
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("\"schema\": 1")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
