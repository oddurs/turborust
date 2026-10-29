//! Resolves config + cargo metadata into concrete, runnable nodes.
//!
//! This is the layer that makes a full-Rust stack ergonomic. You name a crate;
//! turborust derives the command, and — more importantly — derives the watch and
//! input globs from the crate's *path-dependency closure*. Touch `crates/shared`
//! and both the axum backend and the wasm frontend that depend on it invalidate,
//! because cargo says they should.

use crate::cargo::Metadata;
use crate::config::{
    CachePolicy, EnvMode, Health, OnChange, ProxyRule, Reload, RestartPolicy, Serve, TargetDir,
    Workspace,
};
use anyhow::{Result, bail};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Service,
    Task,
}

/// A service's `debounce` when it sets none; the config default, in one place
/// for the nodes that have no config field for it.
const DEFAULT_DEBOUNCE: Duration = Duration::from_millis(50);

#[derive(Debug, Clone)]
pub struct Node {
    pub name: String,
    pub kind: Kind,
    /// `None` only for a pure `serve` node, which turborust hosts in-process.
    pub cmd: Option<String>,
    pub cwd: PathBuf,
    pub env: BTreeMap<String, String>,
    /// Project files, then the node's own; read when the node starts.
    pub env_files: Vec<crate::envfile::EnvFile>,
    pub depends_on: Vec<String>,
    pub watch: Vec<String>,
    pub ignore: Vec<String>,
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
    pub env_keys: Vec<String>,
    pub pass_through_env: Vec<String>,
    pub env_mode: EnvMode,
    pub target_dir: TargetDir,
    pub cache: CachePolicy,
    pub restart: RestartPolicy,
    pub on_change: OnChange,
    /// Parsed signal number for `on_change = "signal"`.
    pub signal: Option<i32>,
    pub reload: Reload,
    /// Output marking the start of a rebuild, for `reload = "self"`.
    pub reloading: Option<String>,
    /// Output marking a rebuild that failed, for `reload = "self"`.
    pub reload_failed: Option<String>,
    /// Checked while running; see `config::Liveness`.
    pub liveness: Option<Liveness>,
    pub port: Option<u16>,
    pub health: Option<Health>,
    pub serve: Option<Serve>,
    pub proxy: Vec<ProxyRule>,
    pub debounce: Duration,
    pub ready_timeout: Duration,
    pub stop_timeout: Duration,
    /// How long a task may run before it is stopped and failed. Tasks only.
    pub timeout: Option<Duration>,
    pub backoff_min: Duration,
    pub backoff_max: Duration,
    /// Crash restarts allowed in a row before giving up. Services only.
    pub max_restarts: Option<u32>,
    /// Set when globs came from cargo rather than the config file, so `why` can
    /// say where the watch set came from.
    pub derived_from_crate: Option<String>,
}

impl Node {
    pub fn is_cargo_cmd(&self) -> bool {
        self.cmd
            .as_deref()
            .map(|c| c.split_whitespace().any(|w| w == "cargo" || w == "trunk"))
            .unwrap_or(false)
    }
}

/// A resolved liveness probe: the same shape as readiness, checked on a timer.
#[derive(Debug, Clone)]
pub struct Liveness {
    pub probe: Health,
    pub every: Duration,
    pub failures: u32,
}

pub struct Plan {
    pub root: PathBuf,
    pub concurrency: Option<usize>,
    pub notify: bool,
    /// Direct dependents of each node — the reverse of `depends_on`.
    ///
    /// Built once here because three separate questions need it: which nodes a
    /// filter's `...name` form should include, which nodes a changed file makes
    /// affected, and which tasks a failure has blocked. Walking the forward edges
    /// for each of those is quadratic in the number of matched nodes, which small
    /// graphs hide and large ones do not.
    dependents: BTreeMap<String, Vec<String>>,
    pub global: crate::global::GlobalHash,
    pub nodes: BTreeMap<String, Node>,
    pub order: Vec<String>,
    pub global_ignore: Vec<String>,
    pub metadata: Option<Metadata>,
}

impl Plan {
    pub fn get(&self, name: &str) -> Result<&Node> {
        self.nodes
            .get(name)
            .ok_or_else(|| anyhow::anyhow!("unknown node `{name}`"))
    }

    /// Include globs plus the global ignore list, ready for `Matcher::new`.
    pub fn watch_matcher(&self, node: &Node) -> Result<crate::cache::Matcher> {
        let mut ignore = self.global_ignore.clone();
        ignore.extend(node.ignore.iter().cloned());
        crate::cache::Matcher::new(&node.watch, &ignore)
    }

    /// Every node `name` depends on, transitively. Excludes `name` itself.
    pub fn transitive_deps(&self, name: &str) -> BTreeSet<String> {
        self.reach(name, |n| {
            self.nodes
                .get(n)
                .map(|x| x.depends_on.clone())
                .unwrap_or_default()
        })
    }

    /// Every node that depends on `name`, transitively. Excludes `name` itself.
    pub fn transitive_dependents(&self, name: &str) -> BTreeSet<String> {
        self.reach(name, |n| {
            self.dependents.get(n).cloned().unwrap_or_default()
        })
    }

    /// How long the shared watcher waits for quiet before acting: the shortest
    /// `debounce` any service in the plan asks for, so a service that sets one
    /// gets it. One watcher serves every node, so the shortest has to win.
    pub fn watch_quiet(&self) -> Duration {
        self.nodes
            .values()
            .filter(|n| n.kind == Kind::Service)
            .map(|n| n.debounce)
            .min()
            .unwrap_or(DEFAULT_DEBOUNCE)
    }

    /// The order to stop nodes in, as waves that can each stop all at once.
    ///
    /// A node is in the wave after the last of its dependents, so nothing stops
    /// while something that needs it is still running, and nothing waits on a
    /// node it has no edge to. Stopping one node at a time made a stop of fifty
    /// independent services take over a second.
    pub fn stop_waves(&self) -> Vec<Vec<String>> {
        let mut wave: BTreeMap<&str, usize> = BTreeMap::new();
        // `order` puts every node after its dependencies, so walking it
        // backwards meets every dependent before the node it depends on.
        for name in self.order.iter().rev() {
            let w = self
                .dependents
                .get(name)
                .into_iter()
                .flatten()
                .filter_map(|d| wave.get(d.as_str()))
                .map(|w| w + 1)
                .max()
                .unwrap_or(0);
            wave.insert(name, w);
        }
        let mut waves: Vec<Vec<String>> = Vec::new();
        for name in self.order.iter().rev() {
            let w = wave[name.as_str()];
            if waves.len() <= w {
                waves.resize(w + 1, Vec::new());
            }
            waves[w].push(name.clone());
        }
        waves
    }

    /// Breadth-first reachability over whichever edge direction `edges` yields.
    ///
    /// No cycle guard beyond the visited set is needed: cycles are rejected when
    /// the config is validated, so the graph reaching here is acyclic.
    fn reach(&self, from: &str, edges: impl Fn(&str) -> Vec<String>) -> BTreeSet<String> {
        let mut seen = BTreeSet::new();
        let mut stack = edges(from);
        while let Some(n) = stack.pop() {
            if !seen.insert(n.clone()) {
                continue;
            }
            stack.extend(edges(&n));
        }
        seen
    }

    /// Given every node whose watch set matched a change, returns only those that
    /// should actually be signalled.
    ///
    /// A crate edit typically matches both a `check` task and the service that
    /// depends on it, because both derive their globs from the same cargo closure.
    /// Signalling both restarts the service twice: once immediately, once when the
    /// task finishes and readiness cascades. Keeping only the upstream-most nodes
    /// leaves the dependency edges to do the propagating, which is what they are for.
    pub fn dispatch_roots(&self, hits: &[String]) -> Vec<String> {
        let set: BTreeSet<&str> = hits.iter().map(|s| s.as_str()).collect();
        hits.iter()
            .filter(|h| {
                !self
                    .transitive_deps(h)
                    .iter()
                    .any(|d| set.contains(d.as_str()))
            })
            .cloned()
            .collect()
    }

    pub fn input_matcher(&self, node: &Node) -> Result<crate::cache::Matcher> {
        let mut ignore = self.global_ignore.clone();
        ignore.extend(node.ignore.iter().cloned());
        crate::cache::Matcher::new(&node.inputs, &ignore)
    }
}

/// The project's env files and then the node's, loaded once here so a missing
/// or malformed file is refused before anything runs.
fn env_files(
    ws: &Workspace,
    name: &str,
    own: Option<&crate::config::EnvFiles>,
) -> Result<Vec<crate::envfile::EnvFile>> {
    let files: Vec<crate::envfile::EnvFile> = ws
        .config
        .project
        .env_file
        .iter()
        .chain(own)
        .flat_map(|f| f.specs())
        .map(|(path, required)| crate::envfile::EnvFile {
            path: ws.root.join(path),
            required,
        })
        .collect();
    crate::envfile::load(&files).map_err(|e| anyhow::anyhow!("`{name}`: {e}"))?;
    Ok(files)
}

pub fn resolve(ws: &Workspace, targets: &[String]) -> Result<Plan> {
    let metadata = Metadata::load(&ws.root)?;

    let roots: Vec<String> = if targets.is_empty() {
        ws.config
            .services
            .keys()
            .chain(ws.config.tasks.keys().filter(|_| false))
            .cloned()
            .collect()
    } else {
        targets.to_vec()
    };
    if roots.is_empty() {
        bail!("no services defined; give a task name explicitly (`turborust run <task>`)");
    }
    let order = ws.topo_closure(&roots)?;

    let mut nodes = BTreeMap::new();
    for name in &order {
        let node = resolve_node(ws, metadata.as_ref(), name)?;
        check_paths(&node)?;
        nodes.insert(name.clone(), node);
    }

    let mut dependents: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (name, node) in &nodes {
        dependents.entry(name.clone()).or_default();
        for dep in &node.depends_on {
            dependents
                .entry(dep.clone())
                .or_default()
                .push(name.clone());
        }
    }

    Ok(Plan {
        root: ws.root.clone(),
        concurrency: ws.config.project.concurrency,
        notify: ws.config.project.notify,
        dependents,
        global: crate::global::GlobalHash::compute(&ws.root, ws.source.as_deref()),
        nodes,
        order,
        global_ignore: ws.config.project.ignore.clone(),
        metadata,
    })
}

fn resolve_node(ws: &Workspace, meta: Option<&Metadata>, name: &str) -> Result<Node> {
    use crate::config::parse_duration as pd;

    if let Some(s) = ws.config.services.get(name) {
        let mut cmd = s.cmd.clone();
        let mut watch = s.watch.clone();
        let mut derived = None;

        if let Some(krate) = &s.cargo {
            let meta = meta.ok_or_else(|| {
                anyhow::anyhow!(
                    "service `{name}` sets `cargo` but there is no Cargo workspace here"
                )
            })?;
            let pkg = meta.get(krate)?;
            if cmd.is_none() {
                if !pkg.has_bin {
                    bail!(
                        "crate `{krate}` has no bin target, so turborust cannot guess how to run it.\n\
                         Set `cmd` explicitly — e.g. `trunk serve`, `cargo leptos watch`, or `dx serve`."
                    );
                }
                let extra = if s.cargo_args.is_empty() {
                    String::new()
                } else {
                    format!(" {}", s.cargo_args.join(" "))
                };
                cmd = Some(format!("cargo run -p {krate}{extra}"));
            }
            if watch.is_empty() {
                watch = meta.source_globs(krate, &ws.root)?;
                derived = Some(krate.clone());
            }
        }

        if cmd.is_none() && s.serve.is_none() {
            bail!("service `{name}` has no command");
        }
        check_reload(name, s)?;
        if let Some(h) = &s.health {
            // Parsed here rather than where they are used, where a typo quietly
            // fell back to a default (cairn 0086's rule, applied to probes).
            for (key, value) in [("interval", &h.interval), ("timeout", &h.timeout)] {
                pd(value).map_err(|e| anyhow::anyhow!("service `{name}`: health.{key}: {e}"))?;
            }
        }

        // A declared port with no explicit probe is a readiness signal in itself.
        let health = s
            .health
            .clone()
            .or_else(|| {
                s.port.map(|p| Health {
                    tcp: Some(p),
                    http: None,
                    log: None,
                    cmd: None,
                    timeout: crate::config::d_probe_timeout(),
                    interval: "300ms".to_string(),
                })
            })
            // A `serve` node's readiness is its own listener coming up. Note that
            // `port` stays None for these: turborust owns that socket, so the
            // pre-start port reclaim must not wait for itself to let go.
            .or_else(|| {
                s.serve.as_ref().map(|sv| Health {
                    tcp: Some(sv.port),
                    http: None,
                    log: None,
                    cmd: None,
                    timeout: crate::config::d_probe_timeout(),
                    interval: "150ms".to_string(),
                })
            });

        return Ok(Node {
            name: name.to_string(),
            kind: Kind::Service,
            cmd,
            cwd: ws.resolve_cwd(&s.cwd),
            env: s.env.clone(),
            env_files: env_files(ws, name, s.env_file.as_ref())?,
            depends_on: s.depends_on.clone(),
            watch,
            ignore: s.ignore.clone(),
            inputs: Vec::new(),
            outputs: Vec::new(),
            env_keys: Vec::new(),
            pass_through_env: Vec::new(),
            // Services are never cached, so a filtered environment would only
            // break dev servers that legitimately read ambient configuration.
            env_mode: EnvMode::Loose,
            target_dir: s.target_dir,
            cache: CachePolicy::Disabled,
            restart: s.restart,
            on_change: s.on_change,
            signal: s.signal.as_deref().map(parse_signal).transpose()?,
            reload: s.reload,
            reloading: s.reloading.as_ref().map(|r| r.log.clone()),
            reload_failed: s.reloading.as_ref().and_then(|r| r.failed.clone()),
            liveness: s
                .liveness
                .as_ref()
                .map(|l| -> Result<Liveness> {
                    if l.tcp.is_none() && l.http.is_none() && l.cmd.is_none() {
                        bail!("service `{name}`: `liveness` needs one of `tcp`, `http` or `cmd`");
                    }
                    if l.failures == 0 {
                        bail!("service `{name}`: liveness.failures must be at least 1");
                    }
                    Ok(Liveness {
                        probe: Health {
                            tcp: l.tcp,
                            http: l.http.clone(),
                            log: None,
                            cmd: l.cmd.clone(),
                            timeout: l.timeout.clone(),
                            interval: l.every.clone(),
                        },
                        every: {
                            pd(&l.timeout)?;
                            pd(&l.every)?
                        },
                        failures: l.failures,
                    })
                })
                .transpose()?,
            port: s.port,
            health,
            serve: s.serve.clone(),
            // Longest prefix first, so `/api/deep` wins over `/api`.
            proxy: {
                let mut rules = s.proxy.clone();
                rules.sort_by_key(|r| std::cmp::Reverse(r.path.len()));
                rules
            },
            debounce: pd(&s.debounce)?,
            ready_timeout: pd(&s.ready_timeout)?,
            stop_timeout: pd(&s.stop_timeout)?,
            timeout: None,
            backoff_min: pd(&s.backoff_min)?,
            backoff_max: pd(&s.backoff_max)?,
            max_restarts: s.max_restarts,
            derived_from_crate: derived,
        });
    }

    let t = ws
        .config
        .tasks
        .get(name)
        .ok_or_else(|| anyhow::anyhow!("unknown node `{name}`"))?;

    let mut cmd = t.cmd.clone();
    let mut inputs = t.inputs.clone();
    let mut derived = None;
    if let Some(krate) = &t.cargo {
        let meta = meta.ok_or_else(|| {
            anyhow::anyhow!("task `{name}` sets `cargo` but there is no Cargo workspace here")
        })?;
        meta.get(krate)?;
        if cmd.is_none() {
            let extra = if t.cargo_args.is_empty() {
                String::new()
            } else {
                format!(" {}", t.cargo_args.join(" "))
            };
            cmd = Some(format!("cargo build -p {krate}{extra}"));
        }
        if inputs.is_empty() {
            inputs = meta.source_globs(krate, &ws.root)?;
            derived = Some(krate.clone());
        }
    }
    let cmd = cmd.ok_or_else(|| anyhow::anyhow!("task `{name}` has no command"))?;

    Ok(Node {
        name: name.to_string(),
        kind: Kind::Task,
        cmd: Some(cmd),
        cwd: ws.resolve_cwd(&t.cwd),
        env: t.env.clone(),
        env_files: env_files(ws, name, t.env_file.as_ref())?,
        depends_on: t.depends_on.clone(),
        watch: Vec::new(),
        ignore: t.ignore.clone(),
        inputs,
        outputs: t.outputs.clone(),
        env_keys: t.env_keys.clone(),
        pass_through_env: t.pass_through_env.clone(),
        env_mode: t.env_mode,
        target_dir: t.target_dir,
        cache: t.cache,
        restart: RestartPolicy::Never,
        on_change: OnChange::Restart,
        signal: None,
        reload: Reload::Restart,
        reloading: None,
        reload_failed: None,
        liveness: None,
        port: None,
        health: None,
        serve: None,
        proxy: Vec::new(),
        debounce: DEFAULT_DEBOUNCE,
        ready_timeout: pd("60s")?,
        stop_timeout: pd("8s")?,
        timeout: t
            .timeout
            .as_ref()
            .or(ws.config.project.task_timeout.as_ref())
            .map(|v| {
                let d = pd(v).map_err(|e| anyhow::anyhow!("task `{name}`: timeout: {e}"))?;
                if d.is_zero() {
                    bail!("task `{name}`: a timeout of zero would fail it before it starts");
                }
                Ok(d)
            })
            .transpose()?,
        backoff_min: pd("250ms")?,
        backoff_max: pd("30s")?,
        max_restarts: None,
        derived_from_crate: derived,
    })
}

/// Accepts "SIGHUP", "HUP" or a bare number.
fn parse_signal(name: &str) -> Result<i32> {
    let n = name.trim().to_ascii_uppercase();
    let n = n.strip_prefix("SIG").unwrap_or(&n);
    Ok(match n {
        "HUP" => 1,
        "INT" => 2,
        "QUIT" => 3,
        "USR1" => 10,
        "USR2" => 12,
        "TERM" => 15,
        "WINCH" => 28,
        other => other.parse().map_err(|_| {
            anyhow::anyhow!("unknown signal `{name}` (try SIGHUP, SIGUSR1, or a number)")
        })?,
    })
}

/// Refuses globs and output paths that cannot mean what they say.
///
/// Done here, once, rather than where each is used: a bad glob used to surface
/// mid-run as a task that exited 1 and printed nothing, and an output outside
/// the workspace was archived and then *restored* there by a cache hit — a
/// cache entry writing wherever the path pointed (cairn 0086).
fn check_paths(node: &Node) -> Result<()> {
    let name = &node.name;
    for (field, globs) in [
        ("inputs", &node.inputs),
        ("outputs", &node.outputs),
        ("watch", &node.watch),
        ("ignore", &node.ignore),
    ] {
        // The matcher the run will build, so this accepts exactly what it does.
        if let Err(e) = crate::cache::Matcher::new(globs, &[]) {
            bail!("`{name}`: {e:#} in `{field}`");
        }
    }
    for out in &node.outputs {
        let escapes = out.split(['/', '\\']).any(|seg| seg == "..");
        let absolute = out.starts_with(['/', '\\']) || out.get(1..2) == Some(":");
        if escapes || absolute {
            bail!(
                "`{name}`: output `{out}` is outside the workspace. Outputs are restored \
                 from the cache on a hit, so they must stay inside it"
            );
        }
    }
    Ok(())
}

/// Rejects reload settings that would be silently ignored.
///
/// Each of these used to be a way to write a config that looked supervised and
/// was not, which is the failure `reload = "self"` exists to remove.
fn check_reload(name: &str, s: &crate::config::Service) -> Result<()> {
    if s.reload == Reload::Restart {
        if s.reloading.is_some() {
            bail!(
                "service `{name}`: `reloading` only applies with `reload = \"self\"` — \
                 a service turborust restarts is out of ready from the moment it stops"
            );
        }
        return Ok(());
    }
    // `ignore` is accepted because it says the same thing; anything else asks
    // turborust to act on a change it has been told is not its to act on.
    if !matches!(s.on_change, OnChange::Restart | OnChange::Ignore) {
        bail!(
            "service `{name}`: `on_change` has no effect with `reload = \"self\"`, \
             because the process rebuilds itself"
        );
    }
    if s.reloading.is_some() && s.health.as_ref().and_then(|h| h.log.as_ref()).is_none() {
        bail!(
            "service `{name}`: `reloading` needs `health = {{ log = \"...\" }}`, \
             which is how a reload is known to have finished"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn plan_from(src: &str) -> Plan {
        let ws = Workspace {
            root: PathBuf::from("/w"),
            config: toml::from_str::<Config>(src).unwrap(),
            source: None,
        };
        resolve(&ws, &[]).unwrap()
    }

    fn plan_err(src: &str) -> String {
        let ws = Workspace {
            root: PathBuf::from("/w"),
            config: toml::from_str::<Config>(src).unwrap(),
            source: None,
        };
        match resolve(&ws, &[]) {
            Ok(_) => panic!("expected the config to be rejected"),
            Err(e) => e.to_string(),
        }
    }

    fn resolve_err(src: &str, target: &str) -> String {
        let ws = Workspace {
            root: PathBuf::from("/w"),
            config: toml::from_str::<Config>(src).unwrap(),
            source: None,
        };
        match resolve(&ws, &[target.to_string()]) {
            Ok(_) => panic!("expected the config to be rejected"),
            Err(e) => format!("{e:#}"),
        }
    }

    // Cover for cairn 0086: refused at plan time, not discovered mid-run.

    #[test]
    fn a_bad_glob_is_refused_with_the_field_it_is_in() {
        for field in ["inputs", "outputs", "ignore"] {
            let e = resolve_err(
                &format!("[tasks.a]\ncmd = \"x\"\n{field} = [\"src/[unclosed\"]\n"),
                "a",
            );
            assert!(e.contains("invalid glob `src/[unclosed`"), "{e}");
            assert!(e.contains(&format!("in `{field}`")), "{e}");
        }
        let e = resolve_err("[services.s]\ncmd = \"x\"\nwatch = [\"{a\"]\n", "s");
        assert!(e.contains("in `watch`"), "{e}");
    }

    #[test]
    fn an_output_outside_the_workspace_is_refused() {
        for out in [
            "../escape.txt",
            "dist/../../escape",
            "/etc/passwd",
            "C:\\\\x",
            "..\\\\x",
        ] {
            let e = resolve_err(
                &format!("[tasks.a]\ncmd = \"x\"\noutputs = ['{out}']\n"),
                "a",
            );
            assert!(e.contains("is outside the workspace"), "{out}: {e}");
        }
    }

    #[test]
    fn an_output_that_only_looks_like_it_escapes_is_fine() {
        let ws = Workspace {
            root: PathBuf::from("/w"),
            config: toml::from_str::<Config>(
                "[tasks.a]\ncmd = \"x\"\noutputs = [\"dist/**\", \"a..b/x\", \"..hidden\"]\n",
            )
            .unwrap(),
            source: None,
        };
        assert!(resolve(&ws, &["a".to_string()]).is_ok());
    }

    #[test]
    fn a_probe_duration_that_does_not_parse_is_refused() {
        // Cover for cairn 0091: these used to fall back to a default quietly.
        for (key, bad) in [("interval", "fast"), ("timeout", "-1s")] {
            let e = plan_err(&format!(
                "[services.db]\ncmd = \"x\"\nhealth = {{ cmd = \"true\", {key} = \"{bad}\" }}\n"
            ));
            assert!(e.contains(&format!("health.{key}")), "{e}");
        }
    }

    #[test]
    fn a_task_timeout_must_parse_and_be_more_than_nothing() {
        // Cover for cairn 0095.
        let with = |extra: &str| {
            plan_err(&format!(
                "{extra}[services.s]\ncmd = \"x\"\ndepends_on = [\"t\"]\n[tasks.t]\ncmd = \"x\"\n"
            ))
        };
        let own = |t: &str| {
            plan_err(&format!(
                "[services.s]\ncmd = \"x\"\ndepends_on = [\"t\"]\n[tasks.t]\ncmd = \"x\"\ntimeout = \"{t}\"\n"
            ))
        };
        let e = own("forever");
        assert!(e.contains("task `t`: timeout"), "{e}");
        let e = own("0s");
        assert!(e.contains("zero"), "{e}");
        let e = with("[project]\ntask_timeout = \"soon\"\n");
        assert!(e.contains("task `t`: timeout"), "{e}");
    }

    #[test]
    fn a_liveness_probe_must_probe_something() {
        // Cover for cairn 0092.
        let e = plan_err("[services.api]\ncmd = \"x\"\nliveness = { every = \"1s\" }\n");
        assert!(e.contains("needs one of `tcp`, `http` or `cmd`"), "{e}");
        let e =
            plan_err("[services.api]\ncmd = \"x\"\nliveness = { cmd = \"true\", failures = 0 }\n");
        assert!(e.contains("at least 1"), "{e}");
    }

    // Cover for cairn 0073: each of these used to be a config that looked
    // supervised and was not.

    #[test]
    fn a_self_reloading_service_carries_its_probe_into_the_plan() {
        let p = plan_from(
            r#"
            [services.web]
            cmd = "dx serve"
            reload = "self"
            health = { log = "server running" }
            reloading = { log = "rebuilding" }
        "#,
        );
        let n = p.get("web").unwrap();
        assert_eq!(n.reload, Reload::SelfManaged);
        assert_eq!(n.reloading.as_deref(), Some("rebuilding"));
    }

    #[test]
    fn reloading_without_reload_self_is_rejected() {
        let e = plan_err(
            r#"
            [services.web]
            cmd = "./web"
            health = { log = "up" }
            reloading = { log = "rebuilding" }
        "#,
        );
        assert!(e.contains("only applies with `reload = \"self\"`"), "{e}");
    }

    #[test]
    fn on_change_that_would_be_ignored_is_rejected() {
        let e = plan_err(
            r#"
            [services.web]
            cmd = "./web"
            reload = "self"
            on_change = "signal"
            signal = "SIGHUP"
        "#,
        );
        assert!(e.contains("`on_change` has no effect"), "{e}");
    }

    #[test]
    fn reloading_needs_a_log_probe_to_know_when_it_finished() {
        let e = plan_err(
            r#"
            [services.web]
            cmd = "./web"
            reload = "self"
            port = 8080
            reloading = { log = "rebuilding" }
        "#,
        );
        assert!(e.contains("needs `health = { log"), "{e}");
    }

    #[test]
    fn dispatch_signals_only_the_upstream_most_node() {
        let p = plan_from(
            r#"
            [tasks.check]
            cmd = "cargo check"
            inputs = ["crates/**/*.rs"]
            [services.api]
            cmd = "./api"
            depends_on = ["check"]
            watch = ["crates/**/*.rs"]
            [services.web]
            cmd = "./web"
            depends_on = ["api"]
            watch = ["crates/**/*.rs"]
        "#,
        );
        // All three match the same edit; only `check` should be signalled, and the
        // readiness cascade restarts api then web exactly once each.
        let roots = p.dispatch_roots(&["check".into(), "api".into(), "web".into()]);
        assert_eq!(roots, vec!["check"]);
    }

    #[test]
    fn independent_nodes_are_all_signalled() {
        let p = plan_from(
            r#"
            [services.a]
            cmd = "./a"
            watch = ["a/**"]
            [services.b]
            cmd = "./b"
            watch = ["b/**"]
        "#,
        );
        let roots = p.dispatch_roots(&["a".into(), "b".into()]);
        assert_eq!(roots, vec!["a", "b"]);
    }

    #[test]
    fn a_dependent_alone_is_still_signalled() {
        let p = plan_from(
            r#"
            [tasks.check]
            cmd = "cargo check"
            inputs = ["crates/**/*.rs"]
            [services.api]
            cmd = "./api"
            depends_on = ["check"]
            watch = ["config/**"]
        "#,
        );
        // Only the service matched, so nothing upstream will cascade for it.
        assert_eq!(p.dispatch_roots(&["api".into()]), vec!["api"]);
    }

    fn ws_from(src: &str) -> Workspace {
        Workspace {
            root: PathBuf::from("/w"),
            config: toml::from_str::<Config>(src).unwrap(),
            source: None,
        }
    }

    #[test]
    fn port_implies_a_tcp_readiness_probe() {
        let ws = ws_from(
            r#"
            [services.api]
            cmd = "./api"
            port = 8080
        "#,
        );
        let n = resolve_node(&ws, None, "api").unwrap();
        assert_eq!(n.health.unwrap().tcp, Some(8080));
    }

    #[test]
    fn explicit_health_wins_over_port() {
        let ws = ws_from(
            r#"
            [services.api]
            cmd = "./api"
            port = 8080
            health = { http = "http://127.0.0.1:8080/healthz" }
        "#,
        );
        let h = resolve_node(&ws, None, "api").unwrap().health.unwrap();
        assert_eq!(h.http.as_deref(), Some("http://127.0.0.1:8080/healthz"));
        assert_eq!(h.tcp, None);
    }

    #[test]
    fn cargo_without_a_workspace_is_a_clear_error() {
        let ws = ws_from(
            r#"
            [services.api]
            cargo = "api"
        "#,
        );
        let err = resolve_node(&ws, None, "api").unwrap_err().to_string();
        assert!(err.contains("no Cargo workspace"), "{err}");
    }

    #[test]
    fn tasks_never_restart() {
        let ws = ws_from(
            r#"
            [tasks.build]
            cmd = "make"
        "#,
        );
        let n = resolve_node(&ws, None, "build").unwrap();
        assert_eq!(n.restart, RestartPolicy::Never);
        assert_eq!(n.kind, Kind::Task);
    }
}

#[cfg(test)]
mod graph_tests {
    use super::*;
    use crate::config::Config;

    /// a -> b -> d, a -> c -> d  (arrows point at dependencies)
    fn diamond() -> Plan {
        let src = r#"
            [tasks.d]
            cmd = "true"
            [tasks.b]
            cmd = "true"
            depends_on = ["d"]
            [tasks.c]
            cmd = "true"
            depends_on = ["d"]
            [tasks.a]
            cmd = "true"
            depends_on = ["b", "c"]
        "#;
        let ws = Workspace {
            root: PathBuf::from("/w"),
            config: toml::from_str::<Config>(src).unwrap(),
            source: None,
        };
        resolve(&ws, &["a".to_string()]).unwrap()
    }

    fn names(s: BTreeSet<String>) -> Vec<String> {
        s.into_iter().collect()
    }

    #[test]
    fn dependencies_are_transitive_and_exclude_self() {
        assert_eq!(names(diamond().transitive_deps("a")), vec!["b", "c", "d"]);
        assert_eq!(names(diamond().transitive_deps("b")), vec!["d"]);
        assert!(diamond().transitive_deps("d").is_empty());
    }

    #[test]
    fn dependents_are_transitive_and_exclude_self() {
        assert_eq!(
            names(diamond().transitive_dependents("d")),
            vec!["a", "b", "c"]
        );
        assert_eq!(names(diamond().transitive_dependents("b")), vec!["a"]);
        assert!(diamond().transitive_dependents("a").is_empty());
    }

    #[test]
    fn the_watcher_waits_as_briefly_as_its_most_eager_service_asks() {
        let plan = |src: &str, targets: &[&str]| {
            let ws = Workspace {
                root: PathBuf::from("/w"),
                config: toml::from_str::<Config>(src).unwrap(),
                source: None,
            };
            let t: Vec<String> = targets.iter().map(|s| s.to_string()).collect();
            resolve(&ws, &t).unwrap()
        };
        let src = r#"
            [services.fast]
            cmd = "x"
            debounce = "30ms"
            [services.plain]
            cmd = "x"
            [tasks.t]
            cmd = "x"
        "#;
        assert_eq!(
            plan(src, &["fast", "plain"]).watch_quiet(),
            Duration::from_millis(30)
        );
        assert_eq!(plan(src, &["plain"]).watch_quiet(), DEFAULT_DEBOUNCE);
        assert_eq!(plan(src, &["t"]).watch_quiet(), DEFAULT_DEBOUNCE);
    }

    #[test]
    fn stop_waves_put_every_dependent_before_what_it_needs() {
        let waves: Vec<Vec<String>> = diamond()
            .stop_waves()
            .into_iter()
            .map(|mut w| {
                w.sort();
                w
            })
            .collect();
        assert_eq!(waves, vec![vec!["a"], vec!["b", "c"], vec!["d"]]);
    }

    #[test]
    fn a_diamond_is_visited_once_not_twice() {
        // `d` is reachable from `a` by two paths; it must appear once.
        let deps: Vec<String> = names(diamond().transitive_deps("a"));
        assert_eq!(deps.iter().filter(|n| *n == "d").count(), 1);
    }

    #[test]
    fn an_unknown_node_has_no_edges() {
        assert!(diamond().transitive_deps("nope").is_empty());
        assert!(diamond().transitive_dependents("nope").is_empty());
    }
}
