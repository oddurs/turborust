//! Config parsing and validation for `turborust.toml`.
//!
//! Two node kinds share one graph: long-running `services` and one-shot `tasks`.
//! Keeping them in the same dependency space is the point — a service can wait on
//! a task (build before serve) and a task can wait on a service (migrate after db).

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const CONFIG_NAMES: [&str; 2] = ["turborust.toml", ".turborust.toml"];

#[derive(Debug, Deserialize, schemars::JsonSchema, Default)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub project: Project,
    #[serde(default)]
    pub overlay: Overlay,
    #[serde(default)]
    pub cache: SharedCache,
    #[serde(default)]
    pub services: BTreeMap<String, Service>,
    #[serde(default)]
    pub tasks: BTreeMap<String, Task>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Project {
    #[serde(default)]
    pub name: Option<String>,
    /// Globs excluded from every watcher and every input hash.
    #[serde(default = "default_ignore")]
    pub ignore: Vec<String>,
    /// Show a desktop notification when a node fails or recovers.
    #[serde(default)]
    pub notify: bool,
    /// How many nodes may be building at once. Defaults to the machine's
    /// parallelism, capped, because builds are memory-hungry rather than
    /// merely CPU-hungry.
    pub concurrency: Option<usize>,
}

/// Hand-written rather than derived: `#[derive(Default)]` would give `ignore` an
/// empty vec, and `#[serde(default)]` on the `project` *field* calls this — so a
/// config with no `[project]` table would silently lose every default exclusion
/// and start hashing `target/`.
impl Default for Project {
    fn default() -> Self {
        Project {
            name: None,
            ignore: default_ignore(),
            notify: false,
            concurrency: None,
        }
    }
}

fn default_ignore() -> Vec<String> {
    [
        "**/.git/**",
        "**/target/**",
        "**/node_modules/**",
        "**/.turborust/**",
        "**/dist/**",
        "**/.next/**",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

#[derive(Debug, Deserialize, schemars::JsonSchema, Clone)]
#[serde(deny_unknown_fields)]
pub struct Service {
    /// Command to run. Optional when `cargo` or `serve` supplies one.
    #[serde(default)]
    pub cmd: Option<String>,
    /// Cargo package name. Derives the run command, the watch set, and the
    /// rebuild edges from `cargo metadata` — including transitive path deps,
    /// which is the part hand-written globs always get wrong.
    pub cargo: Option<String>,
    /// Extra args appended to the derived cargo command.
    #[serde(default)]
    pub cargo_args: Vec<String>,
    #[serde(default)]
    pub target_dir: TargetDir,
    /// Serve a directory over HTTP with live-reload injection (wasm frontends).
    pub serve: Option<Serve>,
    /// Forward matching paths to another origin, so the whole stack is one origin.
    #[serde(default)]
    pub proxy: Vec<ProxyRule>,
    pub cwd: Option<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Reclaimed before start and checked for readiness if no explicit health probe.
    pub port: Option<u16>,
    pub health: Option<Health>,
    #[serde(default)]
    pub depends_on: Vec<String>,
    #[serde(default)]
    pub watch: Vec<String>,
    #[serde(default)]
    pub ignore: Vec<String>,
    #[serde(default)]
    pub restart: RestartPolicy,
    /// What a watched-file change does to this service.
    #[serde(default)]
    pub on_change: OnChange,
    /// Signal sent when `on_change = "signal"`, e.g. "SIGHUP".
    pub signal: Option<String>,
    /// Debounce window for watch events before acting.
    #[serde(default = "d_debounce")]
    pub debounce: String,
    #[serde(default = "d_ready_timeout")]
    pub ready_timeout: String,
    #[serde(default = "d_stop_timeout")]
    pub stop_timeout: String,
    #[serde(default = "d_backoff_min")]
    pub backoff_min: String,
    #[serde(default = "d_backoff_max")]
    pub backoff_max: String,
}

fn d_debounce() -> String {
    "120ms".into()
}
fn d_ready_timeout() -> String {
    "60s".into()
}
fn d_stop_timeout() -> String {
    "8s".into()
}
fn d_backoff_min() -> String {
    "250ms".into()
}
fn d_backoff_max() -> String {
    "30s".into()
}

/// What a watched-file change does to a running service.
#[derive(Debug, Deserialize, schemars::JsonSchema, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum OnChange {
    /// Stop and start again, interrupting a startup already in progress.
    #[default]
    Restart,
    /// Let a startup finish before acting, so a cold build is never killed
    /// halfway and then started from scratch.
    Queue,
    /// Send `signal` instead of restarting — for processes that reload
    /// their own configuration.
    Signal,
    /// Do nothing.
    Ignore,
}

#[derive(Debug, Deserialize, schemars::JsonSchema, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum RestartPolicy {
    /// Restart on crash and on watched-file change.
    #[default]
    Always,
    /// Restart only when watched files change; a clean exit stays exited.
    OnChange,
    /// Never restarted automatically.
    Never,
}

/// A second cache directory, shared with other machines or checkouts.
///
/// The trust model is the whole design, so it is stated here rather than in a
/// doc comment nobody reads: a build cache maps inputs to **outputs**, so anyone
/// who can write to a cache you read from can hand your build arbitrary
/// artifacts. Reading is a trust decision; writing is a bigger one.
///
/// Therefore `push` defaults to false. Pointing at a shared directory gets you
/// its results; contributing yours is a separate, explicit choice.
#[derive(Debug, Deserialize, schemars::JsonSchema, Clone)]
#[serde(deny_unknown_fields)]
pub struct SharedCache {
    /// Directory to read results from, in addition to the local cache. A network
    /// mount, a synced folder, or another checkout.
    pub shared: Option<String>,
    /// Also write this machine's results there. Off unless asked for.
    #[serde(default)]
    pub push: bool,
    /// Byte budget for the local store: `10GiB`, `500MB`, or `0` to keep
    /// everything. Least-recently-*used* results are dropped first.
    ///
    /// Deliberately not applied to a shared store. A shared cache is not this
    /// machine's to garbage-collect — evicting from it would delete results
    /// other people are still reading, on the strength of one machine's budget.
    #[serde(default = "d_cache_budget")]
    pub max_size: String,
    /// Re-run some cache hits and check they really reproduce what was stored.
    #[serde(default)]
    pub verify: VerifyMode,
    /// Where to keep results. Defaults to a per-user store shared by every
    /// worktree of this repository, because results are content-addressed and
    /// therefore portable between them.
    pub dir: Option<String>,
}

/// How much of the cache to check against reality as you go.
///
/// `turborust verify` answers the question deliberately; this answers it during
/// ordinary work, which is when a cache actually starts lying to you.
#[derive(Debug, Deserialize, schemars::JsonSchema, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum VerifyMode {
    /// Trust the cache. The default: checking costs a rebuild.
    #[default]
    Off,
    /// Check one hit in [`VERIFY_SAMPLE_RATE`].
    Sample,
    /// Check every hit. Correct, and roughly as slow as having no cache.
    Always,
}

/// One hit in this many is re-run under `verify = "sample"`.
///
/// Fixed rather than configurable: the useful range is narrow, and a float in a
/// config file invites tuning a number nobody can reason about.
pub const VERIFY_SAMPLE_RATE: u64 = 20;

fn d_cache_budget() -> String {
    "10GiB".into()
}

/// Hand-written, not derived. `Config.cache` is `#[serde(default)]`, so a config
/// with no `[cache]` table at all constructs this through `Default` — which
/// skips serde's per-field defaults entirely and would leave `max_size` empty.
impl Default for SharedCache {
    fn default() -> Self {
        SharedCache {
            shared: None,
            push: false,
            max_size: d_cache_budget(),
            verify: VerifyMode::Off,
            dir: None,
        }
    }
}

/// The in-browser dev overlay.
///
/// Deliberately shaped like Turbopack's `devIndicators`: one table, sensible
/// defaults, and every value also overridable at runtime from the overlay's own
/// preferences pane — so moving the bubble out of the way of your own UI never
/// requires editing a file and restarting.
#[derive(Debug, Deserialize, schemars::JsonSchema, Clone)]
#[serde(deny_unknown_fields)]
pub struct Overlay {
    #[serde(default = "d_true")]
    pub enabled: bool,
    #[serde(default)]
    pub position: Position,
    /// The face in the bubble. Any emoji; the crab is just the default.
    #[serde(default = "d_emoji")]
    pub emoji: String,
    #[serde(default)]
    pub theme: Theme,
    /// Toggles the panel. Parsed in the browser, not here.
    #[serde(default = "d_shortcut")]
    pub shortcut: String,
    #[serde(default)]
    pub errors: ErrorMode,
}

impl Default for Overlay {
    fn default() -> Self {
        Overlay {
            enabled: true,
            position: Position::default(),
            emoji: d_emoji(),
            theme: Theme::default(),
            shortcut: d_shortcut(),
            errors: ErrorMode::default(),
        }
    }
}

fn d_emoji() -> String {
    "\u{1F980}".into()
}
fn d_shortcut() -> String {
    "ctrl+`".into()
}

#[derive(Debug, Deserialize, schemars::JsonSchema, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum Position {
    #[default]
    BottomRight,
    BottomLeft,
    TopRight,
    TopLeft,
}

impl Position {
    pub fn as_str(&self) -> &'static str {
        match self {
            Position::BottomRight => "bottom-right",
            Position::BottomLeft => "bottom-left",
            Position::TopRight => "top-right",
            Position::TopLeft => "top-left",
        }
    }
}

#[derive(Debug, Deserialize, schemars::JsonSchema, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum Theme {
    /// Follow the viewer's `prefers-color-scheme`.
    #[default]
    Auto,
    Dark,
    Light,
}

impl Theme {
    pub fn as_str(&self) -> &'static str {
        match self {
            Theme::Auto => "auto",
            Theme::Dark => "dark",
            Theme::Light => "light",
        }
    }
}

/// What a failed build does to the page.
#[derive(Debug, Deserialize, schemars::JsonSchema, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum ErrorMode {
    /// Take over the viewport, the way a compile error deserves.
    #[default]
    Overlay,
    /// Only mark the bubble; stay out of the way.
    Badge,
    /// Show nothing.
    Silent,
}

impl ErrorMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            ErrorMode::Overlay => "overlay",
            ErrorMode::Badge => "badge",
            ErrorMode::Silent => "silent",
        }
    }
}

/// One forwarding rule for the static server.
///
/// The point is a single origin: a page served here can call `/api/...` with a
/// relative path, so there is no CORS to configure in development and no API
/// origin to swap out for production.
#[derive(Debug, Deserialize, schemars::JsonSchema, Clone)]
#[serde(deny_unknown_fields)]
pub struct ProxyRule {
    /// Path prefix to match, e.g. `/api`.
    pub path: String,
    /// Origin to forward to, e.g. `http://127.0.0.1:8788`.
    pub to: String,
    /// Remove the matched prefix before forwarding, for a backend that does not
    /// know it is mounted under one.
    #[serde(default)]
    pub strip_prefix: bool,
    /// Proxy WebSocket upgrades on this prefix.
    #[serde(default)]
    pub websocket: bool,
}

/// Static file serving for a wasm/SPA frontend, with browser live-reload.
#[derive(Debug, Deserialize, schemars::JsonSchema, Clone)]
#[serde(deny_unknown_fields)]
pub struct Serve {
    /// Directory of built assets, relative to the workspace root.
    pub dir: String,
    pub port: u16,
    /// Fall back to index.html for unknown paths (client-side routing).
    #[serde(default = "d_true")]
    pub spa: bool,
    /// Inject a reload client into served HTML and push reloads on rebuild.
    #[serde(default = "d_true")]
    pub live_reload: bool,
    /// Address to bind. Defaults to loopback.
    ///
    /// Binding beyond loopback is what lets you open the page on a phone, and it
    /// also makes every endpoint reachable by anything on the network — see
    /// `allow_remote_control`.
    #[serde(default = "d_loopback")]
    pub host: String,
    /// Open a browser once the server is ready.
    #[serde(default)]
    pub open: bool,
    /// Serve over HTTPS with this certificate and key.
    pub tls: Option<Tls>,
    /// Permit the mutating endpoints (restart) when bound beyond loopback.
    ///
    /// Off by default and separate from `host` on purpose: wanting to test on a
    /// phone is not consent to let the network restart your processes.
    #[serde(default)]
    pub allow_remote_control: bool,
}

fn d_loopback() -> String {
    "127.0.0.1".into()
}

#[derive(Debug, Deserialize, schemars::JsonSchema, Clone)]
#[serde(deny_unknown_fields)]
pub struct Tls {
    /// PEM certificate chain.
    pub cert: String,
    /// PEM private key.
    pub key: String,
}

impl Serve {
    /// True when this binds somewhere other than loopback.
    pub fn is_remote(&self) -> bool {
        !matches!(self.host.as_str(), "127.0.0.1" | "localhost" | "::1")
    }

    /// Whether state-changing endpoints should be served at all.
    pub fn control_allowed(&self) -> bool {
        !self.is_remote() || self.allow_remote_control
    }
}

fn d_true() -> bool {
    true
}

#[derive(Debug, Deserialize, schemars::JsonSchema, Clone)]
#[serde(deny_unknown_fields)]
pub struct Health {
    /// TCP connect probe against this port.
    pub tcp: Option<u16>,
    /// HTTP GET; any 2xx/3xx counts as healthy.
    pub http: Option<String>,
    /// Substring the process must print before it counts as ready.
    ///
    /// For the many dev processes that announce themselves on stdout and have no
    /// port worth probing — `trunk`, `cargo leptos watch`, migration tools.
    /// Matched against ANSI-stripped output.
    pub log: Option<String>,
    #[serde(default = "d_health_interval")]
    pub interval: String,
}

fn d_health_interval() -> String {
    "300ms".into()
}

#[derive(Debug, Deserialize, schemars::JsonSchema, Clone)]
#[serde(deny_unknown_fields)]
pub struct Task {
    #[serde(default)]
    pub cmd: Option<String>,
    /// Cargo package name; auto-derives `inputs` from the crate and its path deps.
    pub cargo: Option<String>,
    #[serde(default)]
    pub cargo_args: Vec<String>,
    pub cwd: Option<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub depends_on: Vec<String>,
    /// Globs whose content hash forms the cache key. Empty inputs = never cached.
    #[serde(default)]
    pub inputs: Vec<String>,
    #[serde(default)]
    pub outputs: Vec<String>,
    #[serde(default)]
    pub ignore: Vec<String>,
    /// Env vars whose values are folded into the cache key.
    #[serde(default)]
    pub env_keys: Vec<String>,
    /// Env vars passed through to the child but *not* folded into the key.
    /// The escape hatch for things that must reach the build without keying it.
    #[serde(default)]
    pub pass_through_env: Vec<String>,
    #[serde(default)]
    pub env_mode: EnvMode,
    #[serde(default)]
    pub target_dir: TargetDir,
    #[serde(default)]
    pub cache: CachePolicy,
}

/// How much of the ambient environment a task's command may see.
///
/// The default is `strict`, and that is a correctness property rather than a
/// preference: the cache key can only account for variables it knows about, so
/// the only way to guarantee an undeclared variable did not change the build is
/// to make sure the build never saw it.
#[derive(Debug, Deserialize, schemars::JsonSchema, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum EnvMode {
    /// Only the safe base, `env_keys`, `pass_through_env` and inline `env`.
    #[default]
    Strict,
    /// Inherit the full ambient environment — and therefore never cache.
    Loose,
}

impl EnvMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            EnvMode::Strict => "strict",
            EnvMode::Loose => "loose",
        }
    }
}

/// Where a node's cargo builds put their artifacts.
///
/// cargo takes an exclusive lock on its target directory, so two nodes building
/// at once serialise. Splitting removes the contention and is a clear win when
/// the two builds share nothing anyway — a wasm frontend and a native backend
/// compile for different targets. It is usually a loss for two native binaries
/// in one workspace, because every shared dependency is then compiled twice and
/// stored twice.
#[derive(Debug, Deserialize, schemars::JsonSchema, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum TargetDir {
    /// The workspace default. Shared, and therefore serialised under contention.
    #[default]
    Shared,
    /// A private target directory for this node.
    Split,
}

impl TargetDir {
    pub fn as_str(&self) -> &'static str {
        match self {
            TargetDir::Shared => "shared",
            TargetDir::Split => "split",
        }
    }
}

/// Variables always passed to a strict child, and deliberately *not* hashed.
///
/// Without `PATH` and `HOME` essentially nothing runs, so excluding them is not
/// an option; hashing them would make the cache miss whenever a shell rearranged
/// its path, which is often. The residual risk — swapping toolchains via `PATH`
/// without invalidating — is covered by the global hash in item 0017, which folds
/// in `rustc -V` directly.
/// Ceiling on the inferred default. Twenty parallel rustc invocations will
/// exhaust memory long before they exhaust cores.
pub const MAX_INFERRED_CONCURRENCY: usize = 8;

/// Resolved build parallelism.
pub fn concurrency(configured: Option<usize>) -> usize {
    configured.filter(|n| *n > 0).unwrap_or_else(|| {
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
            .min(MAX_INFERRED_CONCURRENCY)
    })
}

pub const SAFE_BASE_ENV: &[&str] = &[
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "SHELL",
    "TERM",
    "TMPDIR",
    "TZ",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    // Without these cargo cannot find its own toolchain or registry.
    "CARGO_HOME",
    "RUSTUP_HOME",
    "RUSTUP_TOOLCHAIN",
    // Windows needs these to start a process at all.
    "SystemRoot",
    "SystemDrive",
    "COMSPEC",
    "PATHEXT",
    "USERPROFILE",
    "APPDATA",
];

#[derive(Debug, Deserialize, schemars::JsonSchema, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum CachePolicy {
    #[default]
    Enabled,
    Disabled,
}

/// A loaded config plus the directory it was found in.
#[derive(Debug)]
pub struct Workspace {
    pub root: PathBuf,
    pub config: Config,
    /// The config file verbatim, so the global hash covers settings that change
    /// what gets hashed without appearing in any task's own inputs.
    pub source: Option<String>,
}

impl Workspace {
    pub fn load(explicit: Option<&Path>) -> Result<Self> {
        let path = match explicit {
            Some(p) => p.to_path_buf(),
            None => find_config()?,
        };
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        let config: Config =
            toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        let root = path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."))
            .canonicalize()
            .unwrap_or_else(|_| PathBuf::from("."));
        let ws = Workspace {
            root,
            config,
            source: Some(text),
        };
        ws.validate()?;
        Ok(ws)
    }

    /// Per-checkout state: run summaries, scratch space, and the name the
    /// control socket is derived from. Stays beside the code it describes.
    pub fn cache_dir(&self) -> PathBuf {
        self.root.join(".turborust")
    }

    /// Where cached results live.
    ///
    /// Not in `.turborust/`, which is per-checkout. This project's own workflow
    /// is a worktree per branch, and results are content-addressed — two
    /// worktrees at the same commit produce the same keys and the same
    /// artifacts, so giving each its own store means paying a full cold build
    /// for answers already on the disk.
    ///
    /// Keyed by *repository*, not by checkout: `git rev-parse --git-common-dir`
    /// resolves to the primary repository for every worktree of it, which is
    /// exactly the identity wanted. Outside a repository the root path is the
    /// only identity available, which reproduces the old behaviour.
    pub fn store_dir(&self) -> PathBuf {
        if let Some(dir) = &self.config.cache.dir {
            let p = PathBuf::from(expand_home(dir));
            return if p.is_absolute() {
                p
            } else {
                self.root.join(p)
            };
        }
        let identity = repository_identity(&self.root).unwrap_or_else(|| self.root.clone());
        let key = blake3::hash(identity.to_string_lossy().as_bytes()).to_hex();
        user_cache_root().join("turborust").join(&key[..16])
    }

    pub fn resolve_cwd(&self, cwd: &Option<String>) -> PathBuf {
        match cwd {
            Some(c) => self.root.join(c),
            None => self.root.clone(),
        }
    }

    pub fn is_service(&self, name: &str) -> bool {
        self.config.services.contains_key(name)
    }

    pub fn is_task(&self, name: &str) -> bool {
        self.config.tasks.contains_key(name)
    }

    pub fn deps_of(&self, name: &str) -> &[String] {
        if let Some(s) = self.config.services.get(name) {
            &s.depends_on
        } else if let Some(t) = self.config.tasks.get(name) {
            &t.depends_on
        } else {
            &[]
        }
    }

    fn validate(&self) -> Result<()> {
        let mut names: HashSet<&str> = HashSet::new();
        for k in self.config.services.keys() {
            names.insert(k.as_str());
        }
        for k in self.config.tasks.keys() {
            if !names.insert(k.as_str()) {
                bail!("`{k}` is declared as both a service and a task; names share one namespace");
            }
        }
        if names.is_empty() {
            bail!("config declares no services and no tasks");
        }
        for (name, deps) in self.edges() {
            for d in deps {
                if !names.contains(d.as_str()) {
                    bail!("`{name}` depends on `{d}`, which is not defined");
                }
            }
        }
        for (name, s) in &self.config.services {
            if s.cmd.is_none() && s.cargo.is_none() && s.serve.is_none() {
                bail!("service `{name}` needs one of `cmd`, `cargo`, or `serve`");
            }
        }
        for (name, t) in &self.config.tasks {
            if t.cmd.is_none() && t.cargo.is_none() {
                bail!("task `{name}` needs `cmd` or `cargo`");
            }
        }
        self.detect_cycle()?;
        Ok(())
    }

    fn edges(&self) -> Vec<(&str, &Vec<String>)> {
        let mut v: Vec<(&str, &Vec<String>)> = Vec::new();
        for (k, s) in &self.config.services {
            v.push((k.as_str(), &s.depends_on));
        }
        for (k, t) in &self.config.tasks {
            v.push((k.as_str(), &t.depends_on));
        }
        v
    }

    fn detect_cycle(&self) -> Result<()> {
        let adj: HashMap<&str, &Vec<String>> = self.edges().into_iter().collect();
        // 0 = unvisited, 1 = on stack, 2 = done
        let mut state: HashMap<&str, u8> = HashMap::new();
        let mut stack: Vec<&str> = Vec::new();

        fn walk<'a>(
            n: &'a str,
            adj: &HashMap<&'a str, &'a Vec<String>>,
            state: &mut HashMap<&'a str, u8>,
            stack: &mut Vec<&'a str>,
        ) -> Result<()> {
            match state.get(n) {
                Some(2) => return Ok(()),
                Some(1) => {
                    let at = stack.iter().position(|x| *x == n).unwrap_or(0);
                    let mut path: Vec<&str> = stack[at..].to_vec();
                    path.push(n);
                    bail!("dependency cycle: {}", path.join(" -> "));
                }
                _ => {}
            }
            state.insert(n, 1);
            stack.push(n);
            if let Some(deps) = adj.get(n) {
                for d in deps.iter() {
                    // `d` borrows from config, which outlives this walk.
                    let d: &'a str = adj
                        .keys()
                        .find(|k| **k == d.as_str())
                        .copied()
                        .unwrap_or("");
                    if !d.is_empty() {
                        walk(d, adj, state, stack)?;
                    }
                }
            }
            stack.pop();
            state.insert(n, 2);
            Ok(())
        }

        let keys: Vec<&str> = adj.keys().copied().collect();
        for k in keys {
            walk(k, &adj, &mut state, &mut stack)?;
        }
        Ok(())
    }

    /// Every node reachable from `roots`, in dependency-first order.
    pub fn topo_closure(&self, roots: &[String]) -> Result<Vec<String>> {
        let mut out: Vec<String> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        fn visit(ws: &Workspace, n: &str, seen: &mut HashSet<String>, out: &mut Vec<String>) {
            if !seen.insert(n.to_string()) {
                return;
            }
            for d in ws.deps_of(n) {
                visit(ws, d, seen, out);
            }
            out.push(n.to_string());
        }
        for r in roots {
            if !self.is_service(r) && !self.is_task(r) {
                bail!("unknown target `{r}`");
            }
            visit(self, r, &mut seen, &mut out);
        }
        Ok(out)
    }
}

fn find_config() -> Result<PathBuf> {
    let mut dir = std::env::current_dir()?;
    loop {
        for name in CONFIG_NAMES {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
        if !dir.pop() {
            bail!("no turborust.toml found in this directory or any parent (try `turborust init`)");
        }
    }
}

/// The git repository a path belongs to, shared by all of its worktrees.
fn repository_identity(root: &Path) -> Option<PathBuf> {
    // Via `git_command`, which strips the variables git exports into every hook.
    // A partial list here would be a second place to get this wrong, and it is
    // already wrong-by-default: a hook invoking turborust makes a plain `git`
    // child answer about the hook's repository, not the one it was pointed at.
    let out = crate::affected::git_command(root)
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let path = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (!path.is_empty()).then(|| PathBuf::from(path))
}

/// The per-user cache directory this platform expects.
fn user_cache_root() -> PathBuf {
    #[cfg(windows)]
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        return PathBuf::from(local);
    }
    if let Ok(xdg) = std::env::var("XDG_CACHE_HOME")
        && !xdg.is_empty()
    {
        return PathBuf::from(xdg);
    }
    if let Ok(home) = std::env::var("HOME") {
        #[cfg(target_os = "macos")]
        return PathBuf::from(home).join("Library").join("Caches");
        #[cfg(not(target_os = "macos"))]
        return PathBuf::from(home).join(".cache");
    }
    std::env::temp_dir()
}

/// Expands a leading `~`, which is where a user-level path usually starts.
pub fn expand_home(path: &str) -> String {
    match path.strip_prefix("~/") {
        Some(rest) => std::env::var("HOME")
            .map(|h| format!("{h}/{rest}"))
            .unwrap_or_else(|_| path.to_string()),
        None => path.to_string(),
    }
}

/// Parses `10GiB`, `500MB`, `2048`, or `0` for no limit.
///
/// Both the decimal and binary units are accepted and mean what they say: `MB`
/// is 10^6 and `MiB` is 2^20. Guessing which one someone meant is how a budget
/// ends up 5% wrong in the direction nobody checked.
pub fn parse_size(s: &str) -> Result<Option<u64>> {
    let s = s.trim();
    if s.is_empty() {
        bail!("empty size");
    }
    let (num, unit) = match s.find(|c: char| c.is_ascii_alphabetic()) {
        Some(i) => (&s[..i], s[i..].trim()),
        None => (s, ""),
    };
    let n: f64 = num
        .trim()
        .parse()
        .with_context(|| format!("`{s}` is not a valid size"))?;
    if n < 0.0 {
        bail!("`{s}` is not a valid size");
    }
    let scale = match unit.to_ascii_lowercase().as_str() {
        "" | "b" => 1.0,
        "kb" => 1e3,
        "mb" => 1e6,
        "gb" => 1e9,
        "tb" => 1e12,
        "k" | "kib" => 1024.0,
        "m" | "mib" => 1024.0 * 1024.0,
        "g" | "gib" => 1024.0 * 1024.0 * 1024.0,
        "t" | "tib" => 1024.0 * 1024.0 * 1024.0 * 1024.0,
        other => bail!("unknown size unit `{other}` in `{s}` (use B, KB, MB, GB, KiB, MiB, GiB)"),
    };
    let bytes = (n * scale) as u64;
    // Zero is "no limit" rather than "keep nothing": a budget of zero bytes
    // would make every store immediately evict itself, which no one means.
    Ok((bytes > 0).then_some(bytes))
}

/// Parses `500ms`, `2s`, `1m`, or a bare number (seconds).
pub fn parse_duration(s: &str) -> Result<Duration> {
    let s = s.trim();
    if s.is_empty() {
        bail!("empty duration");
    }
    let (num, unit) = match s.find(|c: char| c.is_ascii_alphabetic()) {
        Some(i) => (&s[..i], &s[i..]),
        None => (s, "s"),
    };
    let n: f64 = num
        .trim()
        .parse()
        .with_context(|| format!("`{s}` is not a valid duration"))?;
    let millis = match unit.trim() {
        "ms" => n,
        "s" => n * 1_000.0,
        "m" => n * 60_000.0,
        "h" => n * 3_600_000.0,
        other => bail!("unknown duration unit `{other}` in `{s}` (use ms, s, m, h)"),
    };
    Ok(Duration::from_millis(millis.max(0.0) as u64))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ws_at(root: &Path, toml_src: &str) -> Workspace {
        Workspace {
            root: root.to_path_buf(),
            config: toml::from_str(toml_src).unwrap(),
            source: Some(toml_src.to_string()),
        }
    }

    /// The same guard the production path uses, plus a hard check that we are
    /// operating on the repository we think we are.
    ///
    /// This is not defensive dressing. Without `git_command` the hook-exported
    /// `GIT_DIR` sends every `git` child at the *hook's* repository — and a
    /// `git init` aimed there re-initialises a real checkout and marks it bare.
    /// That happened while writing this test. A wrong repository must fail the
    /// test rather than quietly modify somebody's work.
    fn git(args: &[&str], cwd: &Path) {
        let out = crate::affected::git_command(cwd)
            .args(args)
            .output()
            .expect("git is available");
        assert!(
            out.status.success(),
            "git {args:?} failed in {}: {}",
            cwd.display(),
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// Refuses to continue unless `dir` is its own repository.
    fn assert_owns_its_repo(dir: &Path) {
        let out = crate::affected::git_command(dir)
            .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
            .output()
            .expect("git is available");
        let seen = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim().to_string());
        let expected = dir.join(".git");
        // Both must resolve. Comparing two `None`s would pass for a directory
        // that is not a repository at all, which is the case this exists to catch.
        let (Ok(seen), Ok(expected)) = (seen.canonicalize(), expected.canonicalize()) else {
            panic!(
                "{} is not its own git repository (git reported {:?}) — refusing to touch it",
                dir.display(),
                String::from_utf8_lossy(&out.stdout).trim()
            );
        };
        assert_eq!(
            seen,
            expected,
            "this test is pointed at {} instead of its own temporary repository — \
             refusing to touch it",
            seen.display()
        );
    }

    #[test]
    #[should_panic(expected = "refusing to touch it")]
    fn the_repository_guard_rejects_a_directory_that_is_not_its_own_repo() {
        let dir = std::env::temp_dir().join(format!("tr-guard-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // No `git init`: the guard must refuse rather than let a caller write
        // into whatever repository git happens to resolve to.
        assert_owns_its_repo(&dir);
    }

    #[test]
    fn every_worktree_of_a_repository_shares_one_store() {
        let base = std::env::temp_dir().join(format!("tr-wt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let repo = base.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        git(&["init", "-q", "."], &repo);
        // Before anything that writes: prove we are in the temp repository and
        // not somebody's actual checkout.
        assert_owns_its_repo(&repo);
        git(&["config", "user.email", "a@b.c"], &repo);
        git(&["config", "user.name", "t"], &repo);
        std::fs::write(repo.join("f"), "x").unwrap();
        git(&["add", "-A"], &repo);
        git(&["commit", "-qm", "init"], &repo);
        git(&["worktree", "add", "-q", "../wt"], &repo);

        let cfg = "[tasks.a]\ncmd = \"true\"";
        let primary = ws_at(&repo.canonicalize().unwrap(), cfg);
        let sibling = ws_at(&base.join("wt").canonicalize().unwrap(), cfg);

        // The whole point: results are content-addressed, so two checkouts of one
        // repository at one commit produce the same keys — and giving each its
        // own store means paying a cold build for answers already on disk.
        assert_eq!(
            primary.store_dir(),
            sibling.store_dir(),
            "worktrees of one repository must share a store"
        );
        // …and it is not inside either checkout.
        assert!(!primary.store_dir().starts_with(&repo));

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn an_unrelated_directory_gets_its_own_store() {
        let a = std::env::temp_dir().join(format!("tr-solo-a-{}", std::process::id()));
        let b = std::env::temp_dir().join(format!("tr-solo-b-{}", std::process::id()));
        for d in [&a, &b] {
            let _ = std::fs::remove_dir_all(d);
            std::fs::create_dir_all(d).unwrap();
        }
        let cfg = "[tasks.a]\ncmd = \"true\"";
        assert_ne!(ws_at(&a, cfg).store_dir(), ws_at(&b, cfg).store_dir());
        for d in [&a, &b] {
            let _ = std::fs::remove_dir_all(d);
        }
    }

    #[test]
    fn an_explicit_cache_dir_wins() {
        let root = std::env::temp_dir().join(format!("tr-dir-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let ws = ws_at(
            &root,
            "[cache]\ndir = \"vendor/cache\"\n\n[tasks.a]\ncmd = \"true\"",
        );
        assert_eq!(ws.store_dir(), root.join("vendor/cache"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_config_with_no_cache_table_still_has_a_budget() {
        // `Config.cache` is #[serde(default)], so a missing `[cache]` table goes
        // through Default rather than serde's per-field defaults. A derived
        // Default left max_size empty and every command failed with "empty size".
        let cfg: Config = toml::from_str("[tasks.a]\ncmd = \"true\"").unwrap();
        assert_eq!(
            parse_size(&cfg.cache.max_size).unwrap(),
            Some(10 * 1024 * 1024 * 1024)
        );
    }

    #[test]
    fn durations() {
        assert_eq!(parse_duration("500ms").unwrap(), Duration::from_millis(500));
        assert_eq!(parse_duration("2s").unwrap(), Duration::from_secs(2));
        assert_eq!(parse_duration("1m").unwrap(), Duration::from_secs(60));
        assert_eq!(parse_duration("3").unwrap(), Duration::from_secs(3));
        assert!(parse_duration("7x").is_err());
    }

    fn ws(toml_src: &str) -> Result<Workspace> {
        let config: Config = toml::from_str(toml_src)?;
        let ws = Workspace {
            root: PathBuf::from("."),
            config,
            source: None,
        };
        ws.validate()?;
        Ok(ws)
    }

    #[test]
    fn rejects_cycles() {
        let err = ws(r#"
            [tasks.a]
            cmd = "true"
            depends_on = ["b"]
            [tasks.b]
            cmd = "true"
            depends_on = ["a"]
        "#)
        .unwrap_err();
        assert!(err.to_string().contains("cycle"), "{err}");
    }

    #[test]
    fn rejects_unknown_dep() {
        let err = ws(r#"
            [tasks.a]
            cmd = "true"
            depends_on = ["nope"]
        "#)
        .unwrap_err();
        assert!(err.to_string().contains("not defined"), "{err}");
    }

    #[test]
    fn topo_is_dependency_first() {
        let w = ws(r#"
            [tasks.codegen]
            cmd = "true"
            [tasks.build]
            cmd = "true"
            depends_on = ["codegen"]
            [services.api]
            cmd = "true"
            depends_on = ["build"]
        "#)
        .unwrap();
        let order = w.topo_closure(&["api".into()]).unwrap();
        assert_eq!(order, vec!["codegen", "build", "api"]);
    }
}

#[cfg(test)]
mod defaults_tests {
    use super::*;

    #[test]
    fn omitted_project_table_still_gets_default_ignores() {
        let c: Config = toml::from_str("[tasks.a]\ncmd = \"true\"").unwrap();
        assert!(
            c.project.ignore.iter().any(|g| g.contains("target")),
            "a config with no [project] table must still exclude target/: {:?}",
            c.project.ignore
        );
    }

    #[test]
    fn explicit_project_ignore_replaces_the_defaults() {
        let c: Config =
            toml::from_str("[project]\nignore = [\"custom/**\"]\n[tasks.a]\ncmd = \"true\"")
                .unwrap();
        assert_eq!(c.project.ignore, vec!["custom/**"]);
    }
}

#[cfg(test)]
mod overlay_tests {
    use super::*;

    #[test]
    fn overlay_defaults_to_a_crab_in_the_bottom_right() {
        let c: Config = toml::from_str("[tasks.a]\ncmd = \"true\"").unwrap();
        assert!(c.overlay.enabled);
        assert_eq!(c.overlay.emoji, "🦀");
        assert_eq!(c.overlay.position.as_str(), "bottom-right");
        assert_eq!(c.overlay.theme.as_str(), "auto");
        assert_eq!(c.overlay.errors.as_str(), "overlay");
    }

    #[test]
    fn overlay_is_configurable() {
        let c: Config = toml::from_str(
            "[overlay]\nposition = \"top-left\"\nemoji = \"🦞\"\ntheme = \"dark\"\nerrors = \"badge\"\n[tasks.a]\ncmd = \"true\"",
        )
        .unwrap();
        assert_eq!(c.overlay.position.as_str(), "top-left");
        assert_eq!(c.overlay.emoji, "🦞");
        assert_eq!(c.overlay.theme.as_str(), "dark");
        assert_eq!(c.overlay.errors.as_str(), "badge");
    }

    #[test]
    fn rejects_an_unknown_position() {
        let err = toml::from_str::<Config>("[overlay]\nposition = \"middle\"").unwrap_err();
        assert!(err.to_string().contains("bottom-right"), "{err}");
    }

    #[test]
    fn overlay_can_be_turned_off() {
        let c: Config =
            toml::from_str("[overlay]\nenabled = false\n[tasks.a]\ncmd = \"true\"").unwrap();
        assert!(!c.overlay.enabled);
    }
}

#[cfg(test)]
mod concurrency_tests {
    use super::*;

    #[test]
    fn an_explicit_value_is_honoured() {
        assert_eq!(concurrency(Some(3)), 3);
    }

    #[test]
    fn zero_falls_back_rather_than_deadlocking() {
        // A semaphore with no permits would hang the whole graph.
        assert!(concurrency(Some(0)) >= 1);
    }

    #[test]
    fn the_inferred_default_is_capped() {
        let n = concurrency(None);
        assert!(n >= 1);
        assert!(
            n <= MAX_INFERRED_CONCURRENCY,
            "a big machine must not default to {n} parallel builds"
        );
    }
}

#[cfg(test)]
mod strictness_tests {
    use super::*;

    #[test]
    fn a_typo_in_a_key_is_rejected_and_named() {
        // Silently ignoring this is how someone spends an afternoon wondering why
        // their health check never runs.
        let err = toml::from_str::<Config>("[services.api]\ncmd = \"x\"\nheath = { tcp = 80 }")
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("heath"),
            "the error should name the bad key: {err}"
        );
    }

    #[test]
    fn unknown_keys_are_rejected_at_every_level() {
        for src in [
            "[project]\nignoer = []",
            "[tasks.a]\ncmd = \"x\"\ninputz = []",
            "[overlay]\npositon = \"top-left\"",
            "[cache]\nshraed = \"x\"",
            "[services.a]\ncmd = \"x\"\nserve = { dir = \"d\", prot = 1 }",
        ] {
            assert!(
                toml::from_str::<Config>(src).is_err(),
                "should have rejected: {src}"
            );
        }
    }

    #[test]
    fn valid_configs_still_parse() {
        let src = r#"
            [project]
            concurrency = 2
            [overlay]
            position = "top-left"
            [cache]
            shared = "x"
            push = true
            [tasks.build]
            cargo = "api"
            inputs = ["src/**"]
            outputs = ["out"]
            env_keys = ["PROFILE"]
            env_mode = "loose"
            [services.api]
            cargo = "api"
            port = 8080
            health = { log = "ready" }
            on_change = "signal"
            signal = "SIGHUP"
            target_dir = "split"
            proxy = [{ path = "/api", to = "http://localhost:1", strip_prefix = true }]
        "#;
        toml::from_str::<Config>(src).expect("a fully-featured config must parse");
    }

    #[test]
    fn the_schema_covers_the_config() {
        let schema = serde_json::to_string(&schemars::schema_for!(Config)).unwrap();
        for expected in [
            "services",
            "tasks",
            "overlay",
            "env_mode",
            "on_change",
            "proxy",
        ] {
            assert!(schema.contains(expected), "schema is missing {expected}");
        }
    }
}

#[cfg(test)]
mod serve_binding_tests {
    use super::*;

    fn serve(host: &str, allow: bool) -> Serve {
        Serve {
            dir: "dist".into(),
            port: 8789,
            spa: true,
            live_reload: true,
            host: host.into(),
            open: false,
            tls: None,
            allow_remote_control: allow,
        }
    }

    #[test]
    fn loopback_is_the_default_and_permits_control() {
        let c: Config =
            toml::from_str("[services.web]\nserve = { dir = \"d\", port = 1 }").unwrap();
        let s = c.services["web"].serve.as_ref().unwrap();
        assert_eq!(s.host, "127.0.0.1");
        assert!(!s.is_remote());
        assert!(s.control_allowed());
    }

    #[test]
    fn binding_to_the_network_disables_control_by_default() {
        // Wanting to test on a phone is not consent to let the network restart
        // your processes.
        assert!(serve("0.0.0.0", false).is_remote());
        assert!(!serve("0.0.0.0", false).control_allowed());
        assert!(!serve("192.168.1.20", false).control_allowed());
    }

    #[test]
    fn control_can_be_opted_back_in_explicitly() {
        assert!(serve("0.0.0.0", true).control_allowed());
    }

    #[test]
    fn the_loopback_aliases_are_all_recognised() {
        for host in ["127.0.0.1", "localhost", "::1"] {
            assert!(!serve(host, false).is_remote(), "{host} is loopback");
        }
    }
}
