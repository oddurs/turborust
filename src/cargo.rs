//! Cargo workspace introspection.
//!
//! For a full-Rust stack the single most common bug in hand-written dev tooling is
//! the *shared crate problem*: `crates/shared` changes, the backend rebuilds, the
//! frontend does not, and you debug a phantom for ten minutes. Hand-written watch
//! globs get this wrong every time because the truth lives in `Cargo.toml`, not in
//! the directory layout. So we ask cargo.

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize)]
struct RawMetadata {
    packages: Vec<RawPackage>,
    workspace_members: Vec<String>,
    target_directory: PathBuf,
}

#[derive(Debug, Deserialize)]
struct RawPackage {
    id: String,
    #[allow(dead_code)]
    name: String,
    manifest_path: PathBuf,
    targets: Vec<RawTarget>,
    dependencies: Vec<RawDep>,
}

#[derive(Debug, Deserialize)]
struct RawTarget {
    #[allow(dead_code)]
    name: String,
    kind: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct RawDep {
    #[allow(dead_code)]
    name: String,
    /// Present only for path dependencies — our signal for "lives in this repo".
    #[serde(default)]
    path: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct Package {
    pub name: String,
    /// Directory containing this crate's Cargo.toml.
    pub dir: PathBuf,
    pub has_bin: bool,
    pub is_wasm_lib: bool,
    /// Names of in-workspace crates this package depends on, directly.
    pub local_deps: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct Metadata {
    pub packages: BTreeMap<String, Package>,
    pub target_dir: PathBuf,
}

impl Metadata {
    /// Runs `cargo metadata` at `root`. Returns `Ok(None)` when there is no
    /// Cargo workspace here — turborust stays useful for non-Rust processes.
    pub fn load(root: &Path) -> Result<Option<Metadata>> {
        if !root.join("Cargo.toml").is_file() {
            return Ok(None);
        }
        let out = std::process::Command::new("cargo")
            .args(["metadata", "--format-version", "1", "--no-deps"])
            .current_dir(root)
            .output()
            .context("running `cargo metadata` (is cargo on PATH?)")?;
        if !out.status.success() {
            bail!(
                "cargo metadata failed:\n{}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        let raw: RawMetadata =
            serde_json::from_slice(&out.stdout).context("parsing cargo metadata output")?;

        let members: BTreeSet<&str> = raw.workspace_members.iter().map(|s| s.as_str()).collect();
        let member_names: BTreeSet<String> = raw
            .packages
            .iter()
            .filter(|p| members.contains(p.id.as_str()))
            .map(|p| p.name.clone())
            .collect();

        let mut packages = BTreeMap::new();
        for p in raw
            .packages
            .iter()
            .filter(|p| members.contains(p.id.as_str()))
        {
            let dir = p
                .manifest_path
                .parent()
                .map(|d| d.to_path_buf())
                .unwrap_or_else(|| root.to_path_buf());
            let has_bin = p.targets.iter().any(|t| t.kind.iter().any(|k| k == "bin"));
            // Trunk/Leptos/Dioxus frontends compile as cdylib (or a lib built for
            // wasm32); either way they are not something we `cargo run`.
            let is_wasm_lib = p
                .targets
                .iter()
                .any(|t| t.kind.iter().any(|k| k == "cdylib"));
            let local_deps = p
                .dependencies
                .iter()
                .filter(|d| d.path.is_some() || member_names.contains(&d.name))
                .map(|d| d.name.clone())
                .collect();
            packages.insert(
                p.name.clone(),
                Package {
                    name: p.name.clone(),
                    dir,
                    has_bin,
                    is_wasm_lib,
                    local_deps,
                },
            );
        }
        Ok(Some(Metadata {
            packages,
            target_dir: raw.target_directory,
        }))
    }

    pub fn get(&self, name: &str) -> Result<&Package> {
        self.packages.get(name).with_context(|| {
            let known: Vec<&str> = self.packages.keys().map(|s| s.as_str()).collect();
            format!(
                "no crate named `{name}` in this workspace (found: {})",
                known.join(", ")
            )
        })
    }

    /// `name` plus every in-workspace crate it depends on, transitively.
    ///
    /// This closure is what makes a change to a shared crate correctly invalidate
    /// every consumer — backend and frontend alike.
    pub fn closure(&self, name: &str) -> Result<Vec<&Package>> {
        self.get(name)?;
        let mut seen: BTreeSet<String> = BTreeSet::new();
        let mut queue: VecDeque<String> = VecDeque::new();
        queue.push_back(name.to_string());
        let mut out = Vec::new();
        while let Some(n) = queue.pop_front() {
            if !seen.insert(n.clone()) {
                continue;
            }
            let Some(pkg) = self.packages.get(&n) else {
                continue;
            };
            out.push(pkg);
            for d in &pkg.local_deps {
                if !seen.contains(d) {
                    queue.push_back(d.clone());
                }
            }
        }
        Ok(out)
    }

    /// Watch/input globs covering a crate and its local dependency closure,
    /// expressed relative to `root`.
    pub fn source_globs(&self, name: &str, root: &Path) -> Result<Vec<String>> {
        let mut globs = Vec::new();
        for pkg in self.closure(name)? {
            let rel = pkg.dir.strip_prefix(root).unwrap_or(&pkg.dir);
            let base = rel.to_string_lossy().replace('\\', "/");
            let base = if base.is_empty() || base == "." {
                String::new()
            } else {
                format!("{base}/")
            };
            globs.push(format!("{base}**/*.rs"));
            globs.push(format!("{base}Cargo.toml"));
        }
        globs.push("Cargo.lock".to_string());
        globs.sort();
        globs.dedup();
        Ok(globs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta() -> Metadata {
        let mk = |name: &str, deps: &[&str]| Package {
            name: name.into(),
            dir: PathBuf::from("/w/crates").join(name),
            has_bin: true,
            is_wasm_lib: false,
            local_deps: deps.iter().map(|s| s.to_string()).collect(),
        };
        let mut packages = BTreeMap::new();
        for p in [
            mk("api", &["shared"]),
            mk("web", &["shared"]),
            mk("shared", &[]),
        ] {
            packages.insert(p.name.clone(), p);
        }
        Metadata {
            packages,
            target_dir: PathBuf::from("/w/target"),
        }
    }

    #[test]
    fn closure_pulls_in_shared_crates() {
        let m = meta();
        let names: Vec<&str> = m
            .closure("api")
            .unwrap()
            .iter()
            .map(|p| p.name.as_str())
            .collect();
        assert_eq!(names, vec!["api", "shared"]);
    }

    #[test]
    fn source_globs_cover_the_closure() {
        let m = meta();
        let g = m.source_globs("web", Path::new("/w")).unwrap();
        assert!(g.contains(&"crates/web/**/*.rs".to_string()), "{g:?}");
        assert!(g.contains(&"crates/shared/**/*.rs".to_string()), "{g:?}");
        assert!(g.contains(&"Cargo.lock".to_string()));
    }

    #[test]
    fn unknown_crate_is_a_clear_error() {
        let err = meta().closure("nope").unwrap_err().to_string();
        assert!(err.contains("api"), "error should list known crates: {err}");
    }
}
