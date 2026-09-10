//! Iteration-speed diagnostics.
//!
//! An honest note about "snappy": turborust makes everything *around* the compile
//! instant — zero-cost cache hits, exactly-correct invalidation, overlapped restart
//! and port reclaim, sub-frame reload. It cannot make rustc fast. For a Rust stack
//! the edit-to-running loop is dominated by codegen and linking, so the highest-
//! leverage thing a runner can do is tell you which knobs are costing you seconds
//! on every single save.
//!
//! Findings are printed with the exact snippet to paste. Nothing is written to
//! your Cargo.toml — build configuration is the user's call, not the tool's.

use crate::cargo::Metadata;
use std::path::Path;

pub struct Finding {
    pub severity: Severity,
    pub title: String,
    pub detail: String,
    pub fix: Option<String>,
    /// Where the fix goes, when it can be applied automatically.
    pub apply: Option<Fix>,
}

/// A change `doctor --fix` can make, once the user agrees to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fix {
    /// File to change, relative to the workspace root.
    pub path: String,
    /// TOML table this introduces, e.g. `profile.dev`.
    pub table: String,
    /// Exactly the lines to append.
    pub snippet: String,
}

/// What applying a fix would do.
#[derive(Debug, PartialEq, Eq)]
pub enum Applied {
    /// The file did not mention this table; the snippet was appended.
    Added,
    /// The table is already present — declining beats silently producing a
    /// duplicate key or clobbering a setting the user chose deliberately.
    AlreadyPresent,
    Failed(String),
}

/// Appends a fix, refusing when the target table already exists.
///
/// Appending rather than parse-merge-rewrite is deliberate: round-tripping a
/// user's Cargo.toml through a TOML serializer discards their comments and
/// formatting, which is a rude thing to do to a file you were asked to help with.
pub fn apply_fix(root: &Path, fix: &Fix) -> Applied {
    let path = root.join(&fix.path);
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    if existing.contains(&format!("[{}]", fix.table)) {
        return Applied::AlreadyPresent;
    }
    if let Some(parent) = path.parent()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        return Applied::Failed(e.to_string());
    }
    let mut out = existing;
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    if !out.is_empty() {
        out.push('\n');
    }
    out.push_str(fix.snippet.trim_end());
    out.push('\n');
    match std::fs::write(&path, out) {
        Ok(()) => Applied::Added,
        Err(e) => Applied::Failed(e.to_string()),
    }
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Severity {
    Warn,
    Info,
    Ok,
}

impl Severity {
    fn glyph(&self) -> &'static str {
        match self {
            Severity::Warn => "\u{1b}[33m!\u{1b}[0m",
            Severity::Info => "\u{1b}[36mi\u{1b}[0m",
            Severity::Ok => "\u{1b}[32m✓\u{1b}[0m",
        }
    }
}

pub fn diagnose(root: &Path, meta: Option<&Metadata>) -> Vec<Finding> {
    diagnose_with(root, meta, &[])
}

/// `split_nodes` names nodes configured with a private target directory, so the
/// disk cost of that choice can be reported rather than discovered later.
pub fn diagnose_with(root: &Path, meta: Option<&Metadata>, split_nodes: &[String]) -> Vec<Finding> {
    let mut out = Vec::new();
    let cargo_cfg = read_any(root, &[".cargo/config.toml", ".cargo/config"]);
    let manifest = std::fs::read_to_string(root.join("Cargo.toml")).unwrap_or_default();

    out.push(check_linker(&cargo_cfg));
    out.push(check_debuginfo(&manifest));
    out.push(check_incremental());
    out.push(check_dep_optimization(&manifest, meta));
    if let Some(f) = check_cloud_synced_target(root, meta) {
        out.push(f);
    }
    if let Some(f) = check_sccache(&cargo_cfg) {
        out.push(f);
    }
    if let Some(f) = check_split_targets(root, split_nodes) {
        out.push(f);
    }
    out
}

/// Reports what private target directories are costing on disk.
fn check_split_targets(root: &Path, split_nodes: &[String]) -> Option<Finding> {
    if split_nodes.is_empty() {
        return None;
    }
    let base = root.join("target").join("turborust");
    let bytes = dir_size(&base);
    Some(Finding {
        apply: None,
        severity: Severity::Info,
        title: format!(
            "{} node(s) build into private target directories",
            split_nodes.len()
        ),
        detail: format!(
            "{} currently uses {}. Splitting removes cargo's lock contention, and \
             costs a full copy of every dependency that is not shared with the \
             workspace target dir. It pays for a wasm frontend, which compiles for \
             a different target anyway; it usually does not pay for two native \
             binaries in the same workspace.",
            split_nodes.join(", "),
            human_bytes(bytes)
        ),
        fix: None,
    })
}

fn dir_size(path: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };
    entries
        .flatten()
        .map(|e| match e.file_type() {
            Ok(t) if t.is_dir() => dir_size(&e.path()),
            Ok(_) => e.metadata().map(|m| m.len()).unwrap_or(0),
            Err(_) => 0,
        })
        .sum()
}

fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KiB", "MiB", "GiB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

fn read_any(root: &Path, names: &[&str]) -> String {
    for n in names {
        if let Ok(s) = std::fs::read_to_string(root.join(n)) {
            return s;
        }
    }
    String::new()
}

fn which(bin: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|p| p.join(bin).is_file()))
}

fn check_linker(cargo_cfg: &str) -> Finding {
    let configured = cargo_cfg.contains("link-arg=-fuse-ld")
        || cargo_cfg.contains("linker =")
        || cargo_cfg.contains("mold")
        || cargo_cfg.contains("lld");
    if configured {
        return Finding {
            apply: None,
            severity: Severity::Ok,
            title: "linker".into(),
            detail: "a faster linker is configured".into(),
            fix: None,
        };
    }
    // Linking dominates incremental rebuilds: the codegen is cached, the link is not.
    let (available, snippet) = if cfg!(target_os = "macos") {
        let has_lld = which("ld64.lld") || which("lld");
        (
            has_lld,
            "# .cargo/config.toml\n\
             [target.aarch64-apple-darwin]\n\
             rustflags = [\"-C\", \"link-arg=-fuse-ld=lld\"]\n\
             # install: brew install llvm"
                .to_string(),
        )
    } else {
        (
            which("mold"),
            "# .cargo/config.toml\n\
             [target.x86_64-unknown-linux-gnu]\n\
             rustflags = [\"-C\", \"link-arg=-fuse-ld=mold\"]\n\
             # install: apt install mold"
                .to_string(),
        )
    };
    let (cfg_path, table) = if cfg!(target_os = "macos") {
        (".cargo/config.toml", "target.aarch64-apple-darwin")
    } else {
        (".cargo/config.toml", "target.x86_64-unknown-linux-gnu")
    };
    Finding {
        apply: Some(Fix {
            path: cfg_path.into(),
            table: table.into(),
            snippet: snippet
                .lines()
                .filter(|l| !l.trim_start().starts_with('#'))
                .collect::<Vec<_>>()
                .join("\n"),
        }),
        severity: Severity::Warn,
        title: "default linker in use".into(),
        detail: format!(
            "Incremental rebuilds are link-bound — codegen is cached, linking is not. \
             A faster linker typically cuts 30-60% off edit-to-running.{}",
            if available {
                " A faster linker is already installed."
            } else {
                ""
            }
        ),
        fix: Some(snippet),
    }
}

fn check_debuginfo(manifest: &str) -> Finding {
    if manifest.contains("line-tables-only")
        || manifest.contains("debug = 1")
        || manifest.contains("debug = 0")
    {
        return Finding {
            apply: None,
            severity: Severity::Ok,
            title: "dev debuginfo".into(),
            detail: "dev profile already trims debug info".into(),
            fix: None,
        };
    }
    Finding {
        apply: Some(Fix {
            path: "Cargo.toml".into(),
            table: "profile.dev".into(),
            snippet: "[profile.dev]\ndebug = \"line-tables-only\"".into(),
        }),
        severity: Severity::Warn,
        title: "full debuginfo in dev profile".into(),
        detail: "Full DWARF is the single largest contributor to link time. \
                 `line-tables-only` keeps backtraces readable and panics located."
            .into(),
        fix: Some(
            "# Cargo.toml (workspace root)\n\
             [profile.dev]\n\
             debug = \"line-tables-only\""
                .into(),
        ),
    }
}

fn check_incremental() -> Finding {
    match std::env::var("CARGO_INCREMENTAL").as_deref() {
        Ok("0") => Finding {
            apply: None,
            severity: Severity::Warn,
            title: "CARGO_INCREMENTAL=0".into(),
            detail: "Incremental compilation is disabled in this environment. \
                     That is right for CI and wrong for a dev loop."
                .into(),
            fix: Some("unset CARGO_INCREMENTAL".into()),
        },
        _ => Finding {
            apply: None,
            severity: Severity::Ok,
            title: "incremental compilation".into(),
            detail: "enabled".into(),
            fix: None,
        },
    }
}

fn check_dep_optimization(manifest: &str, meta: Option<&Metadata>) -> Finding {
    let has_wasm = meta
        .map(|m| m.packages.values().any(|p| p.is_wasm_lib))
        .unwrap_or(false);
    if manifest.contains("[profile.dev.package") {
        return Finding {
            apply: None,
            severity: Severity::Ok,
            title: "dependency opt-level".into(),
            detail: "per-package dev profile overrides are configured".into(),
            fix: None,
        };
    }
    Finding {
        apply: None,
        severity: if has_wasm {
            Severity::Warn
        } else {
            Severity::Info
        },
        title: "unoptimized dependencies in dev".into(),
        detail: format!(
            "Dependencies are compiled once and then cached, so optimizing them costs \
             nothing per-iteration but makes the running app dramatically faster.{}",
            if has_wasm {
                " This workspace has a wasm target, where debug-built dependencies are especially slow."
            } else {
                ""
            }
        ),
        fix: Some(
            "# Cargo.toml (workspace root)\n\
             [profile.dev.package.\"*\"]\n\
             opt-level = 3"
                .into(),
        ),
    }
}

/// A `target/` directory inside a cloud-synced folder is a genuinely brutal and
/// very common macOS trap: every build artifact gets uploaded, and the sync daemon
/// contends with cargo for the same inodes.
fn check_cloud_synced_target(root: &Path, meta: Option<&Metadata>) -> Option<Finding> {
    let target = meta
        .map(|m| m.target_dir.clone())
        .unwrap_or_else(|| root.join("target"));
    let s = target.to_string_lossy();
    let synced = [
        "/Library/Mobile Documents/",
        "/Dropbox/",
        "/Google Drive/",
        "/OneDrive/",
    ]
    .iter()
    .find(|marker| s.contains(**marker))?;
    Some(Finding {
        apply: None,
        severity: Severity::Warn,
        title: "target/ is inside a cloud-synced folder".into(),
        detail: format!(
            "{} sits under `{}`. The sync daemon will upload every object file and \
             contend with cargo for the same inodes. This can multiply build times.",
            target.display(),
            synced.trim_matches('/')
        ),
        fix: Some("export CARGO_TARGET_DIR=\"$HOME/.cargo-target/$(basename $PWD)\"".into()),
    })
}

fn check_sccache(cargo_cfg: &str) -> Option<Finding> {
    if cargo_cfg.contains("sccache") || !which("sccache") {
        return None;
    }
    Some(Finding {
        apply: None,
        severity: Severity::Info,
        title: "sccache is installed but not wired up".into(),
        detail: "Shares compiled dependency artifacts across workspaces — a large win \
                 if you have several Rust projects that share crates."
            .into(),
        fix: Some("# .cargo/config.toml\n[build]\nrustc-wrapper = \"sccache\"".into()),
    })
}

pub fn print(findings: &[Finding]) {
    let warns = findings
        .iter()
        .filter(|f| f.severity == Severity::Warn)
        .count();
    println!();
    for f in findings {
        println!("  {} \u{1b}[1m{}\u{1b}[0m", f.severity.glyph(), f.title);
        if f.severity != Severity::Ok {
            for line in wrap(&f.detail, 74) {
                println!("      \u{1b}[2m{line}\u{1b}[0m");
            }
            if let Some(fix) = &f.fix {
                println!();
                for line in fix.lines() {
                    println!("      \u{1b}[36m{line}\u{1b}[0m");
                }
            }
        }
        println!();
    }
    if warns == 0 {
        println!("  \u{1b}[32mNothing obvious left on the table.\u{1b}[0m\n");
    } else {
        println!(
            "  \u{1b}[2m{warns} suggestion{} — none applied automatically; build config is yours.\u{1b}[0m\n",
            if warns == 1 { "" } else { "s" }
        );
    }
}

fn wrap(s: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut cur = String::new();
    for word in s.split_whitespace() {
        if !cur.is_empty() && cur.len() + 1 + word.len() > width {
            lines.push(std::mem::take(&mut cur));
        }
        if !cur.is_empty() {
            cur.push(' ');
        }
        cur.push_str(word);
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_full_debuginfo_and_accepts_the_fix() {
        assert_eq!(
            check_debuginfo("[package]\nname = \"x\"").severity,
            Severity::Warn
        );
        assert_eq!(
            check_debuginfo("[profile.dev]\ndebug = \"line-tables-only\"").severity,
            Severity::Ok
        );
    }

    #[test]
    fn respects_an_existing_linker_config() {
        let cfg = "[target.aarch64-apple-darwin]\nrustflags = [\"-C\", \"link-arg=-fuse-ld=lld\"]";
        assert_eq!(check_linker(cfg).severity, Severity::Ok);
        assert_eq!(check_linker("").severity, Severity::Warn);
    }

    fn meta_with_target(dir: &str) -> Metadata {
        Metadata {
            target_dir: dir.into(),
            ..Default::default()
        }
    }

    #[test]
    fn detects_cloud_synced_target_dirs() {
        let synced =
            meta_with_target("/Users/x/Library/Mobile Documents/com~apple~CloudDocs/p/target");
        let f = check_cloud_synced_target(Path::new("/w"), Some(&synced)).expect("should flag");
        assert_eq!(f.severity, Severity::Warn);

        let normal = meta_with_target("/Users/x/Code/p/target");
        assert!(check_cloud_synced_target(Path::new("/w"), Some(&normal)).is_none());
    }

    #[test]
    fn wraps_without_losing_words() {
        let text = "the quick brown fox jumps over the lazy dog";
        let out = wrap(text, 12);
        assert!(out.iter().all(|l| l.len() <= 12), "{out:?}");
        assert_eq!(out.join(" "), text);
    }
}

/// Walks the applicable findings, showing each change and applying only what the
/// user agrees to.
///
/// There is deliberately no `--yes`. The findings change how your code is built;
/// a flag that applies them all unseen would be the opposite of the point.
pub fn fix_interactively(root: &Path, findings: &[Finding]) -> usize {
    use std::io::Write;

    let applicable: Vec<&Finding> = findings
        .iter()
        .filter(|f| f.severity == Severity::Warn && f.apply.is_some())
        .collect();

    if applicable.is_empty() {
        println!("  \u{1b}[32mNothing to apply.\u{1b}[0m\n");
        return 0;
    }

    if !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        println!(
            "  \u{1b}[33m--fix needs a terminal to ask for consent.\u{1b}[0m\n  \
             Run it interactively, or paste the snippets above.\n"
        );
        return 0;
    }

    let mut applied = 0;
    for f in applicable {
        let fix = f.apply.as_ref().expect("filtered above");
        println!("  \u{1b}[1m{}\u{1b}[0m", f.title);
        println!("  \u{1b}[2m{}\u{1b}[0m", fix.path);
        for line in fix.snippet.lines() {
            println!("      \u{1b}[32m+ {line}\u{1b}[0m");
        }
        print!("\n  apply? [y/N] ");
        let _ = std::io::stdout().flush();

        let mut answer = String::new();
        if std::io::stdin().read_line(&mut answer).is_err() {
            break;
        }
        // Declining one finding must not skip the rest.
        if !matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
            println!("  \u{1b}[2mskipped\u{1b}[0m\n");
            continue;
        }

        match apply_fix(root, fix) {
            Applied::Added => {
                println!("  \u{1b}[32mwritten to {}\u{1b}[0m\n", fix.path);
                applied += 1;
            }
            Applied::AlreadyPresent => println!(
                "  \u{1b}[33m[{}] already exists in {}; left alone\u{1b}[0m\n",
                fix.table, fix.path
            ),
            Applied::Failed(e) => println!("  \u{1b}[31mfailed: {e}\u{1b}[0m\n"),
        }
    }
    applied
}

#[cfg(test)]
mod fix_tests {
    use super::*;

    fn tmp(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("tr-fix-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn fix() -> Fix {
        Fix {
            path: "Cargo.toml".into(),
            table: "profile.dev".into(),
            snippet: "[profile.dev]\ndebug = \"line-tables-only\"".into(),
        }
    }

    #[test]
    fn appends_to_an_existing_file_without_disturbing_it() {
        let d = tmp("append");
        let original = "[package]\nname = \"x\"  # keep this comment\n";
        std::fs::write(d.join("Cargo.toml"), original).unwrap();

        assert_eq!(apply_fix(&d, &fix()), Applied::Added);

        let got = std::fs::read_to_string(d.join("Cargo.toml")).unwrap();
        assert!(
            got.starts_with(original),
            "existing content must survive byte-for-byte"
        );
        assert!(got.contains("line-tables-only"));
        assert!(
            got.contains("# keep this comment"),
            "comments must not be round-tripped away"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn refuses_when_the_table_already_exists() {
        let d = tmp("exists");
        std::fs::write(d.join("Cargo.toml"), "[profile.dev]\ndebug = 2\n").unwrap();
        assert_eq!(apply_fix(&d, &fix()), Applied::AlreadyPresent);
        let got = std::fs::read_to_string(d.join("Cargo.toml")).unwrap();
        assert!(
            got.contains("debug = 2"),
            "a deliberate setting must not be clobbered"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn creates_missing_directories() {
        let d = tmp("mkdir");
        let f = Fix {
            path: ".cargo/config.toml".into(),
            table: "target.aarch64-apple-darwin".into(),
            snippet: "[target.aarch64-apple-darwin]\nrustflags = []".into(),
        };
        assert_eq!(apply_fix(&d, &f), Applied::Added);
        assert!(d.join(".cargo/config.toml").is_file());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_linker_and_debuginfo_findings_are_applicable() {
        let all = diagnose(&tmp("diag"), None);
        let applicable: Vec<&str> = all
            .iter()
            .filter(|f| f.apply.is_some())
            .map(|f| f.title.as_str())
            .collect();
        assert!(
            applicable.iter().any(|t| t.contains("linker")),
            "{applicable:?}"
        );
        assert!(
            applicable.iter().any(|t| t.contains("debuginfo")),
            "{applicable:?}"
        );
    }

    #[test]
    fn applied_snippets_carry_no_comment_lines() {
        // The printed advice includes install hints; the written file must not.
        for f in diagnose(&tmp("nocomment"), None) {
            if let Some(fix) = &f.apply {
                assert!(
                    !fix.snippet.lines().any(|l| l.trim_start().starts_with('#')),
                    "snippet for {} still has comments: {}",
                    f.title,
                    fix.snippet
                );
            }
        }
    }
}
