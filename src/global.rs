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
use std::sync::{Arc, OnceLock};

#[derive(Debug, Clone, Serialize, Default)]
pub struct GlobalHash {
    /// `rustc -V`, which covers the case strict env mode deliberately leaves
    /// open: swapping toolchains through `PATH` without changing any file.
    pub toolchain: Option<String>,
    /// Content hash of the resolved turborust config.
    pub config: Option<String>,
    /// Content hash of the lockfile, if the workspace has one.
    pub lockfile: Option<String>,
    /// `rustc -V` still running, when `toolchain` has not been filled in yet.
    ///
    /// Through rustup's proxy it takes over 100ms, and only a cache key needs
    /// the answer. Starting it here and waiting only when a key is built lets
    /// every service start without paying for it.
    #[serde(skip)]
    pub probe: Option<Arc<OnceLock<Option<String>>>>,
}

impl GlobalHash {
    pub fn compute(root: &Path, config_text: Option<&str>) -> GlobalHash {
        let probe = Arc::new(OnceLock::new());
        // Known already when nothing that decides the answer has changed since
        // it was last asked (cairn 0113); otherwise asked in the background.
        let remembered = Toolchain::from_env().map(|t| (t.remembered(), t));
        match remembered {
            Some((Some(version), _)) => {
                let _ = probe.set(Some(version));
            }
            other => {
                let slot = probe.clone();
                let ask = move || {
                    let version = rustc_version();
                    if let (Some((_, t)), Some(v)) = (&other, &version) {
                        t.remember(v);
                    }
                    let _ = slot.set(version);
                };
                let spawned = std::thread::Builder::new()
                    .name("rustc-version".into())
                    .spawn(ask);
                if spawned.is_err() {
                    let _ = probe.set(rustc_version());
                }
            }
        }
        GlobalHash {
            toolchain: None,
            config: config_text.map(|t| short(t.as_bytes())),
            lockfile: std::fs::read(root.join("Cargo.lock"))
                .ok()
                .map(|b| short(&b)),
            probe: Some(probe),
        }
    }

    /// The toolchain, waiting for the probe if it is still running.
    pub fn toolchain(&self) -> Option<String> {
        match (&self.toolchain, &self.probe) {
            (Some(t), _) => Some(t.clone()),
            (None, Some(p)) => p.wait().clone(),
            (None, None) => None,
        }
    }

    /// This hash with the probe's answer filled in, for writing down.
    pub fn settled(&self) -> GlobalHash {
        GlobalHash {
            toolchain: self.toolchain(),
            probe: None,
            ..self.clone()
        }
    }

    /// Key/value pairs to fold into a task fingerprint.
    ///
    /// Named so a `why` diff reads as `~ [global:toolchain]` rather than as an
    /// anonymous hash change — the whole point is being able to attribute a miss.
    pub fn meta(&self) -> BTreeMap<String, String> {
        let mut m = BTreeMap::new();
        if let Some(v) = self.toolchain() {
            m.insert("global:toolchain".into(), v);
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

/// What decides the answer `rustc -V` gives, read without running it.
///
/// Through rustup's proxy, `rustc -V` costs about 70ms — most of a cached
/// `run`. The answer depends on which `rustc` is first on `PATH`, and, when
/// that is rustup's proxy, on the toolchain rustup picks for this directory
/// and on what that toolchain holds. All of that is files and variables, so
/// their fingerprint keys a remembered answer. Anything this cannot see into
/// — a shim from a version manager, which picks a toolchain from files of its
/// own — is never remembered, and is asked every time.
struct Toolchain {
    key: String,
    store: std::path::PathBuf,
}

impl Toolchain {
    fn from_env() -> Option<Toolchain> {
        let path = std::env::var_os("PATH")?;
        let cwd = std::env::current_dir().ok()?;
        let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
        let var = |k: &str| std::env::var_os(k).map(std::path::PathBuf::from);
        let key = fingerprint(&Inputs {
            path: &path,
            cwd: &cwd,
            rustup_home: var("RUSTUP_HOME").or_else(|| home.as_ref().map(|h| h.join(".rustup"))),
            rustup_toolchain: std::env::var_os("RUSTUP_TOOLCHAIN"),
            cargo_home: var("CARGO_HOME"),
        })?;
        Some(Toolchain {
            key,
            store: crate::config::user_cache_root()
                .join("turborust")
                .join("toolchains"),
        })
    }

    fn remembered(&self) -> Option<String> {
        let v = std::fs::read_to_string(self.store.join(&self.key)).ok()?;
        let v = v.trim();
        (!v.is_empty()).then(|| v.to_string())
    }

    fn remember(&self, version: &str) {
        // Written aside and renamed, so a reader never sees half an answer.
        let tmp = self
            .store
            .join(format!("{}.{}", self.key, std::process::id()));
        if std::fs::create_dir_all(&self.store).is_ok() && std::fs::write(&tmp, version).is_ok() {
            let _ = std::fs::rename(&tmp, self.store.join(&self.key));
        }
    }
}

struct Inputs<'a> {
    path: &'a std::ffi::OsStr,
    cwd: &'a Path,
    rustup_home: Option<std::path::PathBuf>,
    rustup_toolchain: Option<std::ffi::OsString>,
    cargo_home: Option<std::path::PathBuf>,
}

/// The fingerprint of everything `rustc -V`'s answer depends on, or `None`
/// when the `rustc` that would run is not one whose inputs are all known.
fn fingerprint(i: &Inputs) -> Option<String> {
    use std::fmt::Write as _;
    let rustc = std::env::split_paths(i.path)
        .map(|d| d.join(if cfg!(windows) { "rustc.exe" } else { "rustc" }))
        .find(|p| p.is_file())?;
    // A real rustc, or rustup's proxy — a copy named `rustc`, or a link to
    // `rustup`. A link to anything else is some other tool's shim; a script
    // could read anything at all.
    let target = std::fs::canonicalize(&rustc).ok()?;
    let name = target.file_stem()?.to_str()?;
    if name != "rustc" && name != "rustup" {
        return None;
    }
    let mut head = [0u8; 2];
    if std::io::Read::read(&mut std::fs::File::open(&target).ok()?, &mut head).ok()? == 2
        && &head == b"#!"
    {
        return None;
    }
    let mut f = String::new();
    let _ = writeln!(f, "PATH={:?}", i.path);
    let _ = writeln!(f, "rustc={:?} {}", target, identity(&target)?);
    let _ = writeln!(f, "RUSTUP_TOOLCHAIN={:?}", i.rustup_toolchain);
    let _ = writeln!(f, "CARGO_HOME={:?}", i.cargo_home);
    // rustup's choice for this directory: an override file anywhere above it,
    // then its own settings (default toolchain, directory overrides).
    for dir in i.cwd.ancestors() {
        for file in ["rust-toolchain", "rust-toolchain.toml"] {
            if let Ok(text) = std::fs::read(dir.join(file)) {
                let _ = writeln!(f, "{:?}={}", dir.join(file), blake3::hash(&text));
            }
        }
    }
    if let Some(home) = &i.rustup_home {
        let _ = writeln!(f, "RUSTUP_HOME={home:?}");
        if let Ok(text) = std::fs::read(home.join("settings.toml")) {
            let _ = writeln!(f, "settings={}", blake3::hash(&text));
        }
        // What each installed toolchain holds: an update replaces its rustc.
        if let Ok(dir) = std::fs::read_dir(home.join("toolchains")) {
            let mut each: Vec<String> = dir
                .flatten()
                .filter_map(|e| {
                    let bin = e.path().join("bin").join(rustc.file_name()?);
                    Some(format!("{:?} {}", e.file_name(), identity(&bin)?))
                })
                .collect();
            each.sort();
            let _ = writeln!(f, "toolchains={each:?}");
        }
    }
    Some(blake3::hash(f.as_bytes()).to_hex()[..32].to_string())
}

/// A file as the filesystem knows it: replaced or rewritten, this changes.
fn identity(path: &Path) -> Option<String> {
    let m = std::fs::metadata(path).ok()?;
    let mtime = m
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_nanos();
    #[cfg(unix)]
    let id = {
        use std::os::unix::fs::MetadataExt;
        format!("{}:{}", m.dev(), m.ino())
    };
    #[cfg(not(unix))]
    let id = String::new();
    Some(format!("{id} {} {mtime}", m.len()))
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

    /// `rustc` as the platform names it.
    const RUSTC: &str = if cfg!(windows) { "rustc.exe" } else { "rustc" };

    /// A fake installation: a `rustc` first on PATH, and a rustup home with
    /// settings and one toolchain.
    struct Fake {
        dir: std::path::PathBuf,
    }

    impl Fake {
        fn new(tag: &str) -> Fake {
            let dir =
                std::env::temp_dir().join(format!("tr-toolchain-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            for d in ["bin", "rustup/toolchains/stable/bin", "work/sub"] {
                std::fs::create_dir_all(dir.join(d)).unwrap();
            }
            std::fs::write(dir.join("bin").join(RUSTC), b"\x7fELF proxy").unwrap();
            std::fs::write(
                dir.join("rustup/settings.toml"),
                "default_toolchain = \"stable\"\n",
            )
            .unwrap();
            std::fs::write(
                dir.join("rustup/toolchains/stable/bin").join(RUSTC),
                b"\x7fELF 1.0",
            )
            .unwrap();
            Fake { dir }
        }

        fn key(&self, toolchain: Option<&str>) -> Option<String> {
            fingerprint(&Inputs {
                path: self.dir.join("bin").as_os_str(),
                cwd: &self.dir.join("work/sub"),
                rustup_home: Some(self.dir.join("rustup")),
                rustup_toolchain: toolchain.map(Into::into),
                cargo_home: None,
            })
        }
    }

    impl Drop for Fake {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    #[test]
    fn the_toolchain_key_moves_with_everything_that_picks_the_toolchain() {
        let f = Fake::new("moves");
        let base = f.key(None).expect("a proxy named rustc is understood");
        assert_eq!(
            f.key(None).as_deref(),
            Some(base.as_str()),
            "stable when nothing moved"
        );

        assert_ne!(f.key(Some("nightly")).unwrap(), base, "RUSTUP_TOOLCHAIN");

        std::fs::write(
            f.dir.join("work/rust-toolchain.toml"),
            "[toolchain]\nchannel = \"1.80\"\n",
        )
        .unwrap();
        let pinned = f.key(None).unwrap();
        assert_ne!(pinned, base, "an override file above the working directory");

        std::fs::write(
            f.dir.join("rustup/settings.toml"),
            "default_toolchain = \"beta\"\n",
        )
        .unwrap();
        let defaulted = f.key(None).unwrap();
        assert_ne!(defaulted, pinned, "rustup's settings");

        // `rustup update` replaces the toolchain's rustc.
        std::thread::sleep(std::time::Duration::from_millis(20));
        let bin = f.dir.join("rustup/toolchains/stable/bin").join(RUSTC);
        std::fs::remove_file(&bin).unwrap();
        std::fs::write(&bin, b"\x7fELF 1.1").unwrap();
        assert_ne!(f.key(None).unwrap(), defaulted, "an updated toolchain");
    }

    #[test]
    fn a_rustc_this_cannot_see_into_is_never_remembered() {
        let f = Fake::new("opaque");
        // A version manager's shim script picks a toolchain from its own files.
        std::fs::write(
            f.dir.join("bin").join(RUSTC),
            "#!/bin/sh\nexec asdf exec rustc \"$@\"\n",
        )
        .unwrap();
        assert_eq!(f.key(None), None, "a script");
        #[cfg(unix)]
        {
            std::fs::remove_file(f.dir.join("bin/rustc")).unwrap();
            std::fs::write(f.dir.join("bin/mise"), b"\x7fELF mise").unwrap();
            std::os::unix::fs::symlink(f.dir.join("bin/mise"), f.dir.join("bin/rustc")).unwrap();
            assert_eq!(f.key(None), None, "a link to another tool");
        }
    }

    #[test]
    fn the_toolchain_probe_is_waited_for_and_written_down() {
        let g = GlobalHash::compute(&std::env::temp_dir(), None);
        assert!(g.toolchain.is_none(), "probed in the background");
        let settled = g.settled();
        assert_eq!(settled.toolchain, g.toolchain());
        assert!(settled.probe.is_none());
        assert_eq!(
            g.meta().get("global:toolchain"),
            settled.toolchain.as_ref(),
            "a key waits for the answer rather than leaving it out"
        );
    }

    #[test]
    fn meta_keys_are_attributable() {
        let g = GlobalHash {
            toolchain: Some("rustc 1.98.1".into()),
            config: Some("abc".into()),
            lockfile: None,
            probe: None,
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
            probe: None,
        };
        assert!(!g.meta().is_empty());
    }

    #[test]
    fn the_real_toolchain_is_detected_here() {
        assert!(rustc_version().unwrap_or_default().starts_with("rustc "));
    }
}
