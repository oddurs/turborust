//! Workspace-wide inputs that belong in every task's cache key.
//!
//! A task key built only from that task's own files answers "did my sources
//! change". It cannot answer "did the compiler change", "did the lockfile move",
//! or "did someone edit the config that decides what gets hashed at all" — and
//! each of those changes the output while leaving every per-task key untouched.
//!
//! Computed once per run and mixed into every key, so a toolchain upgrade
//! invalidates the whole workspace exactly once rather than per task.

use serde::Serialize;
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Default)]
pub struct GlobalHash {
    /// `rustc -V`, which covers the case strict env mode deliberately leaves
    /// open: swapping toolchains through `PATH` without changing any file.
    pub toolchain: Option<String>,
    /// Content hash of the resolved turborust config.
    pub config: Option<String>,
    /// Content hash of the lockfile, if the workspace has one.
    pub lockfile: Option<String>,
}

impl GlobalHash {
    pub fn compute(root: &Path, config_text: Option<&str>) -> GlobalHash {
        GlobalHash {
            toolchain: rustc_version(),
            config: config_text.map(|t| short(t.as_bytes())),
            lockfile: std::fs::read(root.join("Cargo.lock"))
                .ok()
                .map(|b| short(&b)),
        }
    }

    /// Key/value pairs to fold into a task fingerprint.
    ///
    /// Named so a `why` diff reads as `~ [global:toolchain]` rather than as an
    /// anonymous hash change — the whole point is being able to attribute a miss.
    pub fn meta(&self) -> BTreeMap<String, String> {
        let mut m = BTreeMap::new();
        if let Some(v) = &self.toolchain {
            m.insert("global:toolchain".into(), v.clone());
        }
        if let Some(v) = &self.config {
            m.insert("global:config".into(), v.clone());
        }
        if let Some(v) = &self.lockfile {
            m.insert("global:lockfile".into(), v.clone());
        }
        m
    }
}

fn short(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().chars().take(16).collect()
}

/// `rustc -V`. `None` when rustc is absent — turborust is still useful for
/// orchestrating processes that have nothing to do with Rust, and refusing to
/// run there would be a strange way to enforce a cache property.
fn rustc_version() -> Option<String> {
    let out = std::process::Command::new("rustc")
        .arg("-V")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_text_changes_the_global_meta() {
        let root = std::env::temp_dir();
        let a = GlobalHash::compute(&root, Some("[tasks.a]\ncmd = \"x\""));
        let b = GlobalHash::compute(&root, Some("[tasks.a]\ncmd = \"y\""));
        assert_ne!(a.config, b.config);
        assert_ne!(a.meta(), b.meta());
    }

    #[test]
    fn meta_keys_are_attributable() {
        let g = GlobalHash {
            toolchain: Some("rustc 1.98.1".into()),
            config: Some("abc".into()),
            lockfile: None,
        };
        let m = g.meta();
        assert_eq!(
            m.get("global:toolchain").map(String::as_str),
            Some("rustc 1.98.1")
        );
        assert!(
            !m.contains_key("global:lockfile"),
            "absent inputs must not appear"
        );
    }

    #[test]
    fn a_workspace_without_rust_still_produces_a_hash() {
        let g = GlobalHash {
            toolchain: None,
            config: Some("x".into()),
            lockfile: None,
        };
        assert!(!g.meta().is_empty());
    }

    #[test]
    fn the_real_toolchain_is_detected_here() {
        assert!(rustc_version().unwrap_or_default().starts_with("rustc "));
    }
}
