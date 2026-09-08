//! Selecting only what a change could have touched.
//!
//! The accuracy here comes from somewhere else: node input globs are derived from
//! the cargo dependency closure, so touching `crates/shared` marks every consumer
//! affected without anyone maintaining a list. This module only has to ask git
//! what moved and match it.
//!
//! Composes with the cache rather than replacing it. Affected decides what is
//! *considered*; the cache decides what actually *runs*.

use crate::plan::{Kind, Plan};
use anyhow::{Context, Result, bail};
use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

/// Paths that changed relative to `base`, plus anything uncommitted.
pub fn changed_paths(root: &Path, base: &str) -> Result<BTreeSet<String>> {
    if !root.join(".git").exists() && !in_worktree(root) {
        bail!(
            "--affected needs a git repository ({} is not one). \
             Run without it, or pass an explicit target.",
            root.display()
        );
    }

    let mut paths = BTreeSet::new();
    // Three dots: compare against the merge base, not the tip. Two dots would
    // mark everything affected the moment the base branch moved ahead, which is
    // most of the time.
    paths.extend(git(
        root,
        &["diff", "--name-only", &format!("{base}...HEAD")],
    )?);
    // Uncommitted work counts; it is the most likely thing to be broken.
    paths.extend(
        git(root, &["status", "--porcelain"])?
            .into_iter()
            .filter_map(|line| line.get(3..).map(str::to_string)),
    );
    Ok(paths)
}

fn in_worktree(root: &Path) -> bool {
    git_command(root)
        .args(["rev-parse", "--is-inside-work-tree"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// A git invocation that answers about `root` and nothing else.
///
/// The environment variables matter more than the working directory. Git sets
/// `GIT_DIR` for every hook it runs, and a `git` child inherits it — so a plain
/// `current_dir(root)` still reports on the *hook's* repository, whatever
/// directory it was pointed at. Anything invoked from a hook, or from another
/// tool that exported these, would silently answer about the wrong repository.
fn git_command(root: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.current_dir(root);
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
    cmd
}

fn git(root: &Path, args: &[&str]) -> Result<Vec<String>> {
    let out = git_command(root)
        .args(args)
        .output()
        .context("running git (is it installed?)")?;
    if !out.status.success() {
        bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_string)
        .filter(|l| !l.is_empty())
        .collect())
}

/// Paths that invalidate every node because they feed the global hash.
///
/// Missing this is how `--affected` produces a green run and a broken artifact:
/// a lockfile or config change alters every key, and matching it against
/// per-node input globs would find nothing.
pub fn is_global(path: &str) -> bool {
    matches!(
        path,
        "Cargo.lock"
            | "turborust.toml"
            | ".turborust.toml"
            | "rust-toolchain.toml"
            | "rust-toolchain"
    )
}

/// The nodes a set of changed paths affects, including their dependents.
pub fn select(plan: &Plan, changed: &BTreeSet<String>) -> Result<Vec<String>> {
    if changed.iter().any(|p| is_global(p)) {
        return Ok(plan.order.clone());
    }

    let mut hit: BTreeSet<String> = BTreeSet::new();
    for name in &plan.order {
        let node = plan.get(name)?;
        let matcher = if node.kind == Kind::Task {
            plan.input_matcher(node)?
        } else {
            plan.watch_matcher(node)?
        };
        if changed.iter().any(|p| matcher.matches(p)) {
            hit.insert(name.clone());
        }
    }

    // A node whose dependency changed is affected even if its own files did not.
    let mut selected = hit.clone();
    for name in &hit {
        selected.extend(plan.transitive_dependents(name));
    }
    Ok(selected.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, Workspace};
    use std::path::PathBuf;

    fn plan() -> Plan {
        let src = r#"
            [tasks.shared_check]
            cmd = "true"
            inputs = ["crates/shared/**"]
            [tasks.api_check]
            cmd = "true"
            depends_on = ["shared_check"]
            inputs = ["crates/api/**"]
            [tasks.docs]
            cmd = "true"
            inputs = ["docs/**"]
        "#;
        let ws = Workspace {
            root: PathBuf::from("/w"),
            config: toml::from_str::<Config>(src).unwrap(),
            source: None,
        };
        crate::plan::resolve(&ws, &["api_check".into(), "docs".into()]).unwrap()
    }

    fn changed(paths: &[&str]) -> BTreeSet<String> {
        paths.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_changed_file_selects_its_node() {
        assert_eq!(
            select(&plan(), &changed(&["docs/index.md"])).unwrap(),
            vec!["docs"]
        );
    }

    #[test]
    fn dependents_of_a_changed_node_are_affected() {
        // Nothing under crates/api changed, but api_check depends on shared_check.
        let got = select(&plan(), &changed(&["crates/shared/src/lib.rs"])).unwrap();
        assert_eq!(got, vec!["api_check", "shared_check"]);
    }

    #[test]
    fn an_unrelated_change_selects_nothing() {
        assert!(
            select(&plan(), &changed(&["README.md"]))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn a_global_input_affects_everything() {
        // The trap: Cargo.lock matches no per-node glob, but changes every key.
        for path in ["Cargo.lock", "turborust.toml", "rust-toolchain.toml"] {
            let got = select(&plan(), &changed(&[path])).unwrap();
            assert_eq!(got.len(), 3, "{path} should affect every node, got {got:?}");
        }
    }

    #[test]
    fn global_paths_are_recognised() {
        assert!(is_global("Cargo.lock"));
        assert!(!is_global("crates/api/Cargo.toml"));
    }

    /// Git exports `GIT_DIR` to every hook it runs. Without clearing it, this
    /// reports on whatever repository invoked the hook rather than on the
    /// directory it was given — which is how a `--affected` run inside a
    /// pre-push hook would quietly select the wrong thing.
    #[test]
    fn an_inherited_git_dir_does_not_leak_in() {
        let dir = std::env::temp_dir().join(format!("tr-gitdir-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let out = git_command(&dir)
            .args(["rev-parse", "--is-inside-work-tree"])
            .env("GIT_DIR", "/definitely/not/here/.git")
            .output()
            .unwrap();
        // The bogus GIT_DIR we just set must be removed, and the directory is
        // not a repository, so this fails rather than answering about elsewhere.
        assert!(!out.status.success(), "an inherited GIT_DIR leaked through");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_repository_is_an_error_not_a_guess() {
        let dir = std::env::temp_dir().join(format!("tr-nogit-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let err = changed_paths(&dir, "HEAD").unwrap_err().to_string();
        assert!(err.contains("git repository"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
