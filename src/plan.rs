//! Resolves config + cargo metadata into concrete, runnable nodes.
//!
//! This is the layer that makes a full-Rust stack ergonomic. You name a crate;
//! turborust derives the command, and — more importantly — derives the watch and
//! input globs from the crate's *path-dependency closure*. Touch `crates/shared`
//! and both the axum backend and the wasm frontend that depend on it invalidate,
//! because cargo says they should.

use crate::cargo::Metadata;
use crate::config::{
    CachePolicy, EnvMode, Health, OnChange, ProxyRule, RestartPolicy, Serve, TargetDir, Workspace,
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

#[derive(Debug, Clone)]
pub struct Node {
    pub name: String,
    pub kind: Kind,
    /// `None` only for a pure `serve` node, which turborust hosts in-process.
    pub cmd: Option<String>,
    pub cwd: PathBuf,
    pub env: BTreeMap<String, String>,
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
    pub port: Option<u16>,
    pub health: Option<Health>,
    pub serve: Option<Serve>,
    pub proxy: Vec<ProxyRule>,
    pub debounce: Duration,
    pub ready_timeout: Duration,
    pub stop_timeout: Duration,
    pub backoff_min: Duration,
    pub backoff_max: Duration,
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
        nodes.insert(name.clone(), resolve_node(ws, metadata.as_ref(), name)?);
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

        // A declared port with no explicit probe is a readiness signal in itself.
        let health = s
            .health
            .clone()
            .or_else(|| {
                s.port.map(|p| Health {
                    tcp: Some(p),
                    http: None,
                    log: None,
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
                    interval: "150ms".to_string(),
                })
            });

        return Ok(Node {
            name: name.to_string(),
            kind: Kind::Service,
            cmd,
            cwd: ws.resolve_cwd(&s.cwd),
            env: s.env.clone(),
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
            backoff_min: pd(&s.backoff_min)?,
            backoff_max: pd(&s.backoff_max)?,
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
        port: None,
        health: None,
        serve: None,
        proxy: Vec::new(),
        debounce: pd("120ms")?,
        ready_timeout: pd("60s")?,
        stop_timeout: pd("8s")?,
        backoff_min: pd("250ms")?,
        backoff_max: pd("30s")?,
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
