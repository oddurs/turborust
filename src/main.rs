use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use turborust::cache::Cache;
use turborust::config::Workspace;
use turborust::contract::{DifferingOutput, Output, VerifyReport, VerifyTask};
use turborust::engine::{self, Engine};
use turborust::plan::{self, Kind};
use turborust::{cargo, doctor, serve, state, tui, watch};

#[derive(Parser)]
#[command(
    name = "turborust",
    about = "A dev orchestrator for full-Rust stacks: supervise, watch, cache, explain.",
    version
)]
struct Cli {
    /// Path to turborust.toml (default: nearest one, searching upward).
    #[arg(long, short = 'c', global = true)]
    config: Option<PathBuf>,
    /// Colour output: auto, always, never. `NO_COLOR` is honoured.
    #[arg(long, default_value = "auto", global = true)]
    color: String,
    /// How a failure is reported on stderr: text, or json with a stable
    /// `code` (see `turborust schema --json error`). Also
    /// `TURBORUST_ERROR_FORMAT`.
    #[arg(long, value_enum, global = true)]
    error_format: Option<ErrorFormat>,
    #[command(subcommand)]
    command: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Start services (and their task dependencies) and keep them running.
    Up {
        /// Services to start. Default: all of them.
        targets: Vec<String>,
        /// Select nodes: `web`, `web...` (and its dependencies), `...web` (and
        /// its dependents), `...web...`, or a glob. Repeatable.
        #[arg(long, short = 'F')]
        filter: Vec<String>,
        /// Stream logs to stdout instead of opening the TUI.
        #[arg(long)]
        no_tui: bool,
        /// Scan for changes instead of waiting for OS file events, every
        /// INTERVAL (default 500ms). For Docker bind mounts, WSL2's /mnt/c,
        /// NFS and SMB, which deliver no events.
        #[arg(long, value_name = "INTERVAL", num_args = 0..=1, default_missing_value = "500ms")]
        poll: Option<String>,
    },
    /// Run tasks once, in dependency order, honouring the cache.
    Run {
        targets: Vec<String>,
        /// Select tasks, with the same forms as `up --filter`.
        #[arg(long, short = 'F')]
        filter: Vec<String>,
        /// Only run tasks whose inputs changed since `--base`.
        #[arg(long)]
        affected: bool,
        /// Git ref to compare against. Merge-base semantics.
        #[arg(long, default_value = "origin/HEAD")]
        base: String,
        /// Ignore cached results and re-run everything.
        #[arg(long)]
        force: bool,
        /// Run everything not blocked by a failure, then report them all.
        #[arg(long = "continue")]
        keep_going: bool,
        /// Report what would run, and run nothing. `--dry-run=json` for the
        /// machine-readable form.
        #[arg(long, num_args = 0..=1, default_missing_value = "text")]
        dry_run: Option<String>,
        /// Write a JSON record of the run under `.turborust/summaries/`.
        #[arg(long)]
        summarize: bool,
        /// How much output to show: full, errors-only, new-only, none.
        #[arg(long, default_value = "full")]
        output_logs: String,
        /// stream prints as output arrives; grouped prints each node as a block
        /// when it finishes. auto follows whether stdout is a terminal.
        #[arg(long, default_value = "auto")]
        log_order: String,
        /// Also write ANSI-free output here.
        #[arg(long)]
        log_file: Option<PathBuf>,
        /// Extra arguments appended to the named task's command, after `--`.
        #[arg(last = true)]
        args: Vec<String>,
    },
    /// Attach this terminal to a node of a running `turborust up`.
    ///
    /// Ctrl-B then d detaches; the node keeps running. Ctrl-C reaches the child.
    Connect { node: String },
    /// Explain what a task would do right now, and why.
    Why { target: String },
    /// Run tasks twice and prove their outputs match.
    ///
    /// A cache is only sound if the same key really does imply the same
    /// outputs. Nothing else checks that, and a content-addressed cache is the
    /// only kind that cheaply can.
    Verify {
        /// Tasks to check. Defaults to every cacheable task in the plan.
        targets: Vec<String>,
        #[arg(long)]
        json: bool,
    },
    /// Report what is costing you seconds on every rebuild.
    Doctor {
        /// Offer to apply each suggestion, one at a time. Never applies anything
        /// without asking.
        #[arg(long)]
        fix: bool,
        /// Say where the time actually went: per task, how long it took, how
        /// often it hit the cache, and why it missed. Read from runs logged
        /// on this machine.
        #[arg(long)]
        profile: bool,
        /// With --profile: how far back to look, e.g. 24h, 7d.
        #[arg(long, default_value = "24h", requires = "profile")]
        since: String,
    },
    /// Print the resolved plan without running anything.
    Plan,
    /// Draw the node graph.
    Graph {
        #[arg(long, default_value = "mermaid")]
        format: String,
        /// Colour nodes by what a run would do right now.
        #[arg(long)]
        with_cache: bool,
    },
    /// Write a starter turborust.toml, inferred from the Cargo workspace.
    Init,
    /// Delete cached task results.
    Clean,
    /// What the cache is holding, and what it last dropped.
    Cache {
        #[arg(long)]
        json: bool,
    },
    /// Print a shell completion script.
    ///
    /// Generated rather than committed, so it cannot drift from the CLI.
    Completions {
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
    /// Print the man page, in roff.
    Man,
    /// Print a JSON Schema for turborust.toml, for editor completion, or with
    /// `--json` for one of the JSON payloads turborust prints and serves.
    Schema {
        /// Which payload: the schema of what that output promises.
        #[arg(long, value_enum, value_name = "OUTPUT")]
        json: Option<Output>,
    },
}

#[derive(Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum ErrorFormat {
    Text,
    Json,
}

fn main() {
    let cli = Cli::parse();
    let format = cli.error_format.unwrap_or_else(|| {
        match std::env::var("TURBORUST_ERROR_FORMAT").as_deref() {
            Ok("json") => ErrorFormat::Json,
            _ => ErrorFormat::Text,
        }
    });
    let result = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(anyhow::Error::from)
        .and_then(|rt| {
            let code = rt.block_on(dispatch(cli));
            // Drop the runtime before exiting so pty reader threads are not
            // killed mid-write, which would truncate the last lines of output.
            drop(rt);
            code
        });
    match result {
        Ok(code) => std::process::exit(code),
        Err(err) => {
            match format {
                // What returning the error from `main` printed.
                ErrorFormat::Text => eprintln!("Error: {err:?}"),
                ErrorFormat::Json => eprintln!(
                    "{}",
                    serde_json::to_string(&turborust::contract::error_report(&err))
                        .unwrap_or_default()
                ),
            }
            std::process::exit(1);
        }
    }
}

async fn dispatch(cli: Cli) -> Result<i32> {
    match cli.command.unwrap_or(Cmd::Up {
        targets: vec![],
        filter: vec![],
        no_tui: false,
        poll: None,
    }) {
        Cmd::Init => cmd_init(),
        Cmd::Doctor {
            profile: true,
            since,
            ..
        } => cmd_profile(cli.config.as_deref(), &since),
        Cmd::Doctor { fix, .. } => cmd_doctor(cli.config.as_deref(), fix),
        Cmd::Plan => cmd_plan(cli.config.as_deref()),
        Cmd::Graph { format, with_cache } => cmd_graph(cli.config.as_deref(), &format, with_cache),
        Cmd::Why { target } => cmd_why(cli.config.as_deref(), &target).await,
        Cmd::Verify { targets, json } => cmd_verify(cli.config.as_deref(), targets, json).await,
        Cmd::Connect { node } => {
            let ws = load(cli.config.as_deref())?;
            turborust::control::connect(&ws.cache_dir(), &node).await
        }
        Cmd::Clean => cmd_clean(cli.config.as_deref()),
        Cmd::Cache { json } => cmd_cache(cli.config.as_deref(), json),
        Cmd::Completions { shell } => {
            let mut cmd = <Cli as clap::CommandFactory>::command();
            let name = cmd.get_name().to_string();
            clap_complete::generate(shell, &mut cmd, name, &mut std::io::stdout());
            Ok(0)
        }
        Cmd::Schema { json: None } => {
            let schema = schemars::schema_for!(turborust::config::Config);
            println!("{}", serde_json::to_string_pretty(&schema)?);
            Ok(0)
        }
        Cmd::Schema { json: Some(output) } => {
            println!("{}", serde_json::to_string_pretty(&output.schema())?);
            Ok(0)
        }
        Cmd::Man => {
            let cmd = <Cli as clap::CommandFactory>::command();
            clap_mangen::Man::new(cmd).render(&mut std::io::stdout())?;
            Ok(0)
        }
        Cmd::Run {
            targets,
            filter,
            affected,
            base,
            force,
            keep_going,
            dry_run,
            summarize,
            output_logs,
            log_order,
            log_file,
            args,
        } => {
            let is_terminal = std::io::IsTerminal::is_terminal(&std::io::stdout());
            let reporting = Reporting {
                verbosity: turborust::output::parse_verbosity(&output_logs)?,
                order: turborust::output::parse_order(&log_order, is_terminal)?,
                colour: turborust::output::use_colour(
                    turborust::output::parse_colour(&cli.color)?,
                    is_terminal,
                ),
                log_file,
            };
            cmd_run(
                cli.config.as_deref(),
                targets,
                filter,
                Affected {
                    enabled: affected,
                    base,
                },
                RunOutput {
                    force,
                    keep_going,
                    dry_run,
                    summarize,
                    reporting,
                },
                args,
            )
            .await
        }
        Cmd::Up {
            targets,
            filter,
            no_tui,
            poll,
        } => cmd_up(cli.config.as_deref(), targets, filter, no_tui, poll).await,
    }
}

fn load(config: Option<&std::path::Path>) -> Result<Workspace> {
    Workspace::load(config)
}

/// Every node in the workspace, for questions that span the whole graph.
fn all_nodes(ws: &Workspace) -> Vec<String> {
    ws.config
        .services
        .keys()
        .chain(ws.config.tasks.keys())
        .cloned()
        .collect()
}

/// Combines positional targets with `--filter` patterns.
///
/// Both name nodes, so they union rather than intersect: `turborust up api
/// --filter ...check` is a reasonable thing to write and should mean both.
fn select(ws: &Workspace, targets: Vec<String>, filter: &[String]) -> Result<Vec<String>> {
    if filter.is_empty() {
        return Ok(targets);
    }
    let mut all = targets;
    all.extend(turborust::filter::expand(ws, filter)?);
    all.sort();
    all.dedup();
    Ok(all)
}

/// Opens the local cache, plus a shared one when the config names it.
fn open_cache(ws: &Workspace) -> Result<Cache> {
    let shared = ws.config.cache.shared.as_ref().map(|d| {
        let p = PathBuf::from(shellexpand_home(d));
        if p.is_absolute() { p } else { ws.root.join(p) }
    });
    let budget = turborust::config::parse_size(&ws.config.cache.max_size)?;
    let cache =
        Cache::with_shared(&ws.store_dir(), shared, ws.config.cache.push)?.with_budget(budget);

    // A checkout that predates the shared store keeps its results rather than
    // silently starting cold. Once only, and only into an empty store.
    let previous = ws.cache_dir();
    if cache.is_empty() && previous.join("runs").is_dir() {
        match cache.adopt(&previous) {
            Ok(n) if n > 0 => eprintln!("moved {n} cached task(s) into the shared store"),
            _ => {}
        }
    }
    Ok(cache)
}

/// Expands a leading `~`, which is where a shared cache usually lives.
fn shellexpand_home(path: &str) -> String {
    match path.strip_prefix("~/") {
        Some(rest) => std::env::var("HOME")
            .map(|h| format!("{h}/{rest}"))
            .unwrap_or_else(|_| path.to_string()),
        None => path.to_string(),
    }
}

/// `doctor --profile`: what the logged runs say, rather than what the
/// configuration suggests (cairn 0075).
fn cmd_profile(config: Option<&std::path::Path>, since: &str) -> Result<i32> {
    let ws = load(config)?;
    let window =
        turborust::config::parse_duration(since).with_context(|| format!("--since {since}"))?;
    let from = turborust::summary::now_secs().saturating_sub(window.as_secs());
    let records = turborust::profile::Log::at(&ws.cache_dir()).load();
    let profiles = turborust::profile::profile(&records, from);
    println!(
        "\n\u{1b}[1mturborust doctor --profile\u{1b}[0m  \u{1b}[2m{}\u{1b}[0m",
        ws.root.display()
    );
    let timings = turborust::profile::latest_timings(&ws.root);
    print!(
        "{}",
        turborust::profile::render(&profiles, &format!("last {since}"), timings.as_ref())
    );
    if !turborust::profile::enabled(ws.config.project.profile) {
        println!(
            "\n  \u{1b}[33mruns are not being logged\u{1b}[0m \u{1b}[2m(`[project] profile = false` or \
             TURBORUST_PROFILE=off), so this will not change.\u{1b}[0m"
        );
    }
    println!();
    Ok(0)
}

fn cmd_doctor(config: Option<&std::path::Path>, fix: bool) -> Result<i32> {
    let root = match load(config) {
        Ok(ws) => ws.root,
        // doctor is useful before there is any config at all.
        Err(_) => std::env::current_dir()?,
    };
    let meta = cargo::Metadata::load(&root).unwrap_or(None);
    // Nodes that opted into a private target directory, so doctor can price it.
    let split: Vec<String> = load(config)
        .ok()
        .and_then(|ws| plan::resolve(&ws, &[]).ok())
        .map(|p| {
            p.order
                .iter()
                .filter(|n| {
                    p.get(n)
                        .map(|x| x.target_dir == turborust::config::TargetDir::Split)
                        .unwrap_or(false)
                })
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    println!(
        "\n\u{1b}[1mturborust doctor\u{1b}[0m  \u{1b}[2m{}\u{1b}[0m",
        root.display()
    );
    let mut findings = doctor::diagnose_with(&root, meta.as_ref(), &split);
    let polling = load(config)
        .ok()
        .and_then(|ws| ws.config.project.watch?.poll)
        .is_some();
    findings.extend(doctor::check_watch_events(&root, polling));
    doctor::print(&findings);
    if fix {
        let n = doctor::fix_interactively(&root, &findings);
        if n > 0 {
            println!("  \u{1b}[2mapplied {n} change(s); rebuild to feel it.\u{1b}[0m\n");
        }
    }
    Ok(0)
}

fn cmd_plan(config: Option<&std::path::Path>) -> Result<i32> {
    let ws = load(config)?;
    let plan = plan::resolve(&ws, &[])?;
    println!();
    for name in &plan.order {
        let n = plan.get(name)?;
        let kind = if n.kind == Kind::Task {
            "task   "
        } else {
            "service"
        };
        println!("  \u{1b}[1m{name}\u{1b}[0m  \u{1b}[2m{kind}\u{1b}[0m");
        if let Some(c) = &n.cmd {
            println!("      cmd      {c}");
        }
        if let Some(s) = &n.serve {
            println!("      serve    {} on :{}", s.dir, s.port);
        }
        if !n.depends_on.is_empty() {
            println!("      after    {}", n.depends_on.join(", "));
        }
        // Its edges behave differently: down on its own rebuild, never restarted
        // for a change, so worth seeing before it surprises anyone.
        if n.reload == turborust::config::Reload::SelfManaged {
            let how = match &n.reloading {
                Some(p) => format!("out of ready on \"{p}\""),
                None => "\u{1b}[2mno `reloading` probe, so rebuilds are invisible\u{1b}[0m".into(),
            };
            println!("      reloads  itself — {how}");
        }
        let globs = if n.kind == Kind::Task {
            &n.inputs
        } else {
            &n.watch
        };
        if !globs.is_empty() {
            let src = match &n.derived_from_crate {
                Some(c) => format!(" \u{1b}[2m(derived from crate `{c}`)\u{1b}[0m"),
                None => String::new(),
            };
            println!("      watches  {}{}", globs.join(", "), src);
        }
        println!();
    }
    Ok(0)
}

fn cmd_graph(config: Option<&std::path::Path>, format: &str, with_cache: bool) -> Result<i32> {
    let ws = load(config)?;
    let plan = Arc::new(plan::resolve(&ws, &all_nodes(&ws))?);
    let format = turborust::graph::parse_format(format)?;

    // A prediction turns the picture from a diagram of the config into a diagram
    // of the next run, which is the version worth having.
    let prediction = if with_cache {
        let cache = Arc::new(open_cache(&ws)?);
        let (engine, _rx) = Engine::new(plan.clone(), cache.clone());
        Some(turborust::summary::predict(&engine, &cache)?)
    } else {
        None
    };

    print!(
        "{}",
        turborust::graph::render(&plan, format, prediction.as_ref())?
    );
    Ok(0)
}

fn cmd_clean(config: Option<&std::path::Path>) -> Result<i32> {
    let ws = load(config)?;
    let store = ws.store_dir();
    Cache::new(&store)?.clear()?;
    // Worth naming: the store is shared by every worktree of this repository, so
    // this is not a local action even though it was run from one checkout.
    println!(
        "cleared {} — shared by every worktree of this repository",
        store.display()
    );
    Ok(0)
}

/// What the store is holding. `clean` deletes; nothing showed.
fn cmd_cache(config: Option<&std::path::Path>, json: bool) -> Result<i32> {
    let ws = load(config)?;
    let cache = open_cache(&ws)?;
    let stats = cache.stats();
    let budget = cache.budget();
    let last = cache.last_sweep();

    if json {
        let report = turborust::contract::CacheReport {
            schema_version: Output::Cache.version(),
            bytes: stats.bytes,
            records: stats.records,
            budget_bytes: budget,
            by_task: stats.by_task,
            shared_bytes: cache.shared_bytes(),
            last_sweep: last,
        };
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(0);
    }

    let cap = match budget {
        Some(b) => format!(" of {}", human(b)),
        None => " (no limit)".into(),
    };
    println!(
        "\n  \u{1b}[1m{}\u{1b}[0m{cap}  \u{1b}[2m·  {} result(s)\u{1b}[0m",
        human(stats.bytes),
        stats.records
    );

    // Largest first: the reason to look at this is to find what is costing you.
    let mut rows: Vec<(&String, &(u64, usize))> = stats.by_task.iter().collect();
    rows.sort_by_key(|r| std::cmp::Reverse(r.1.0));
    if rows.is_empty() {
        println!("\n  \u{1b}[2mnothing cached yet\u{1b}[0m");
    }
    for (task, (bytes, count)) in rows {
        println!(
            "      {:>9}  {:<28} \u{1b}[2m{} result(s)\u{1b}[0m",
            human(*bytes),
            task,
            count
        );
    }

    if let Some(bytes) = cache.shared_bytes() {
        // Reported, never evicted: a shared store is not this machine's to
        // garbage-collect.
        println!(
            "\n  \u{1b}[2mshared store: {} — not evicted from here\u{1b}[0m",
            human(bytes)
        );
    }
    if let Some(s) = last {
        println!(
            "\n  \u{1b}[2mlast eviction dropped {} result(s), freeing {}\u{1b}[0m",
            s.dropped,
            human(s.freed)
        );
    }
    println!();
    Ok(0)
}

fn human(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[i])
    }
}

/// The feature the whole design exists to support: a straight answer to
/// "why did that rebuild?"
async fn cmd_why(config: Option<&std::path::Path>, target: &str) -> Result<i32> {
    let ws = load(config)?;
    let code = explain(&ws, target)?;
    print_history(&ws, target).await;
    Ok(code)
}

/// What a running `up` has seen happen to `target`, if one is running here.
/// Silence otherwise: `why` is about the cache first, and needs no server.
async fn print_history(ws: &Workspace, target: &str) {
    const SHOWN: usize = 15;
    let Ok(Some(entries)) = turborust::control::history(&ws.cache_dir(), Some(target), SHOWN).await
    else {
        return;
    };
    if entries.is_empty() {
        return;
    }
    println!("  \u{1b}[2mrecently, in the running `up`:\u{1b}[0m");
    for e in &entries {
        println!(
            "      {:>8}  {}",
            turborust::state::fmt_dur(std::time::Duration::from_millis(e.ms)),
            e.what
        );
    }
    println!();
}

fn explain(ws: &Workspace, target: &str) -> Result<i32> {
    let plan = Arc::new(plan::resolve(ws, &[target.to_string()])?);
    let node = plan.get(target)?.clone();
    let cache = Arc::new(Cache::new(&ws.store_dir())?);
    let (engine, _rx) = Engine::new(plan.clone(), cache.clone());

    println!("\n  \u{1b}[1m{target}\u{1b}[0m");
    if let Some(c) = &node.derived_from_crate {
        let closure: Vec<String> = plan
            .metadata
            .as_ref()
            .map(|m| {
                m.closure(c)
                    .map(|v| v.iter().map(|p| p.name.clone()).collect())
                    .unwrap_or_default()
            })
            .unwrap_or_default();
        println!(
            "  \u{1b}[2minputs derived from crate `{c}` and its path deps: {}\u{1b}[0m",
            closure.join(", ")
        );
    }

    if node.kind != Kind::Task {
        println!("  \u{1b}[2mservice — restarts when any of these change:\u{1b}[0m");
        for g in &node.watch {
            println!("      {g}");
        }
        println!();
        return Ok(0);
    }

    if node.inputs.is_empty() {
        println!("\n  no `inputs` declared, so this task is never cached — it runs every time.\n");
        return Ok(0);
    }

    engine.seed_stamps_from_cache(&node);
    let now = engine.fingerprint(&node)?;
    println!(
        "  \u{1b}[2m{} input files, key b3:{}\u{1b}[0m\n",
        now.files.len(),
        now.short()
    );

    match cache.load_latest(target) {
        None => {
            println!("  \u{1b}[33mnever run\u{1b}[0m — next run will be a miss.\n");
        }
        Some(rec) => {
            if rec.fingerprint.hash == now.hash {
                println!(
                    "  \u{1b}[32mcache HIT\u{1b}[0m — inputs match the last successful run \
                     ({}).\n  Next `run` will be skipped entirely.\n",
                    state::fmt_dur(Duration::from_millis(rec.duration_ms))
                );
            } else {
                let changes = turborust::cache::diff(&rec.fingerprint, &now);
                println!(
                    "  \u{1b}[33mcache MISS\u{1b}[0m — b3:{} \u{2192} b3:{}\n",
                    rec.fingerprint.short(),
                    now.short()
                );
                for c in changes.iter().take(40) {
                    println!("      {}", c.render());
                }
                if changes.len() > 40 {
                    println!("      \u{1b}[2m… and {} more\u{1b}[0m", changes.len() - 40);
                }
                println!();
            }
        }
    }
    Ok(0)
}

/// `--affected` and the ref it compares against.
struct Affected {
    enabled: bool,
    base: String,
}

/// How the run reports itself.
struct RunOutput {
    force: bool,
    keep_going: bool,
    dry_run: Option<String>,
    summarize: bool,
    reporting: Reporting,
}

struct Reporting {
    verbosity: turborust::output::Verbosity,
    order: turborust::output::Order,
    colour: bool,
    log_file: Option<PathBuf>,
}

async fn cmd_run(
    config: Option<&std::path::Path>,
    targets: Vec<String>,
    filter: Vec<String>,
    affected: Affected,
    output: RunOutput,
    args: Vec<String>,
) -> Result<i32> {
    let ws = load(config)?;
    let mut targets = select(&ws, targets, &filter)?;

    if affected.enabled {
        // Resolve the whole graph first: which nodes a change touches is a
        // question about all of them, not about the ones already named.
        let full = plan::resolve(&ws, &all_nodes(&ws))?;
        let changed = turborust::affected::changed_paths(&ws.root, &affected.base)?;
        let hit = turborust::affected::select(&full, &changed)?;
        if hit.is_empty() {
            println!("nothing affected since {}", affected.base);
            return Ok(0);
        }
        // Intersect when both were given: --filter narrows, --affected narrows
        // again. Unioning them would make each one widen the other, which is the
        // opposite of what someone combining two narrowing flags means.
        targets = if targets.is_empty() {
            hit
        } else {
            targets.into_iter().filter(|t| hit.contains(t)).collect()
        };
        if targets.is_empty() {
            println!("nothing selected is affected since {}", affected.base);
            return Ok(0);
        }
    }

    // A prediction executes nothing, so predicting the whole graph is the useful
    // default; requiring a target would make the common case the verbose one.
    if targets.is_empty() && output.dry_run.is_some() {
        targets = all_nodes(&ws);
    }
    if targets.is_empty() {
        anyhow::bail!("`run` needs at least one task name (see `turborust plan`)");
    }
    let plan = Arc::new(plan::resolve(&ws, &targets)?);
    let cache = Arc::new(open_cache(&ws)?);
    let (engine, rx) = Engine::new_verifying(plan.clone(), cache.clone(), ws.config.cache.verify);
    engine.set_force(output.force);
    if !args.is_empty() {
        // Only the task the user named: a dependency has no idea what
        // `-- --nocapture` means, and passing it down would break the build it
        // was meant to help debug.
        engine.set_passthrough(targets[0].clone(), args);
    }
    if let Some(format) = &output.dry_run {
        let prediction = turborust::summary::predict(&engine, &cache)?;
        if format == "json" {
            println!("{}", serde_json::to_string_pretty(&prediction)?);
        } else {
            print!("{}", prediction.render());
        }
        return Ok(0);
    }

    let reporter = turborust::output::Reporter::new(
        output.reporting.verbosity,
        output.reporting.order,
        output.reporting.colour,
        output.reporting.log_file.clone(),
    )?;
    tokio::spawn(engine::pump_events_with(
        engine.state.clone(),
        rx,
        Some(reporter),
    ));
    let started = turborust::summary::now_secs();
    let began = std::time::Instant::now();
    let mut stop = stop_requests(&engine)?;
    let code = engine::run_once_until(engine.clone(), output.keep_going, async {
        let request = stop.requested().await;
        eprintln!(
            "\nturborust: {} — stopping; send it again to force",
            request.name()
        );
        // Never success: an interrupted run did not pass.
        request.exit_code()
    })
    .await?;

    if output.summarize {
        // One guard: two `lock()`s in this literal would both live to the end
        // of the statement, and the second would wait on the first forever.
        let (issues, nodes) = {
            let st = engine.state.lock().unwrap();
            (st.snapshot().issues, st.history.tallies().clone())
        };
        let summary = turborust::summary::RunSummary {
            schema_version: turborust::summary::SCHEMA,
            schema: turborust::summary::SCHEMA,
            started_at: started,
            duration_ms: began.elapsed().as_millis() as u64,
            exit_code: code,
            global: plan.global.settled(),
            tasks: engine.outcomes(),
            issues,
            nodes,
        };
        let path = turborust::summary::write(&ws.cache_dir(), &summary)?;
        println!("summary written to {}", path.display());
    }
    // Every line out before exiting, and no longer than that takes: this was a
    // fixed 60ms sleep, paid by every run whether it printed anything or not.
    engine.drain_output(Duration::from_millis(200)).await;
    Ok(code)
}

async fn cmd_up(
    config: Option<&std::path::Path>,
    targets: Vec<String>,
    filter: Vec<String>,
    no_tui: bool,
    poll: Option<String>,
) -> Result<i32> {
    let ws = load(config)?;
    let targets = select(&ws, targets, &filter)?;
    // The flag wins over the config, so a one-off session on a mounted volume
    // needs no edit to a committed file.
    let poll = poll.or_else(|| ws.config.project.watch.as_ref()?.poll.clone());
    let backend = match &poll {
        Some(p) => watch::Backend::Poll(
            turborust::config::poll_interval(p).context("--poll / project.watch.poll")?,
        ),
        None => watch::Backend::Native,
    };
    let plan = Arc::new(plan::resolve(&ws, &targets)?);
    let watcher = watch::watch_with(&ws.root, plan.watch_quiet(), backend)
        .context("starting file watcher")?;
    let cache = Arc::new(open_cache(&ws)?);
    let (engine, rx) = Engine::new_verifying(plan.clone(), cache, ws.config.cache.verify);

    tokio::spawn(engine::pump_events(engine.state.clone(), rx, no_tui));

    // Before anything is spawned, so no child is ever running while a stop
    // request would still kill turborust outright.
    let mut stop = stop_requests(&engine)?;
    let (wires, supervisors) = engine::spawn_supervisors(engine.clone());
    let wires = Arc::new(wires);

    // In-process static servers for wasm/SPA frontends.
    for name in &plan.order {
        let node = plan.get(name)?;
        let Some(sv) = node.serve.clone() else {
            continue;
        };
        let dir = plan.root.join(&sv.dir);
        let proxy_rules = node.proxy.clone();
        let reload = engine.reload_tx.clone();
        let ev = engine.events_tx.clone();
        let name = name.clone();
        let overlay_cfg = ws.config.overlay.clone();
        let root = plan.root.clone();
        let app_state = engine.state.clone();
        let node_wires = wires.clone();
        tokio::spawn(async move {
            // The dist dir often does not exist until the first frontend build
            // finishes. Wait for it rather than failing at startup.
            loop {
                if dir.is_dir() {
                    break;
                }
                let _ = ev.send(turborust::proc::ProcEvent::Line(turborust::proc::LogLine {
                    seq: turborust::proc::next_seq(),
                    proc: name.as_str().into(),
                    text: format!("\u{1b}[2mwaiting for {} to exist…\u{1b}[0m", dir.display()),
                    transient: true,
                }));
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            let opts = serve::ServeOpts {
                dir,
                port: sv.port,
                serve: sv.clone(),
                spa: sv.spa,
                live_reload: sv.live_reload,
                reload,
                overlay: overlay_cfg,
                root,
                app: app_state,
                wires: node_wires,
                proxy: proxy_rules,
            };
            if let Err(e) = serve::serve(opts).await {
                let _ = ev.send(turborust::proc::ProcEvent::Line(turborust::proc::LogLine {
                    seq: turborust::proc::next_seq(),
                    proc: name.as_str().into(),
                    text: format!("serve failed: {e}"),
                    transient: false,
                }));
            }
        });
    }

    engine::spawn_watch_dispatch(watcher, engine.clone(), plan.clone(), wires.clone())?;

    // The control socket lives and dies with this `up`; nothing starts it
    // implicitly and nothing outlives the process that created it.
    let socket = turborust::control::socket_path(&ws.cache_dir());
    let control_engine = engine.clone();
    let control_path = socket.clone();
    tokio::spawn(async move {
        if let Err(e) = turborust::control::serve(control_engine, control_path).await {
            eprintln!("turborust: control socket unavailable: {e:#}");
        }
    });

    let ended = if no_tui {
        println!("turborust: {} node(s) — ctrl-c to stop", plan.order.len());
        Ok(Some(stop.requested().await))
    } else {
        tui::run(engine.clone(), wires.clone(), stop.requested()).await
    };
    if let Ok(Some(request)) = &ended {
        eprintln!(
            "\nturborust: {} — stopping; send it again to force",
            request.name()
        );
    }

    // Also when the dashboard failed: the supervisors are already running, and
    // returning the error first would leave their children behind.
    engine::shutdown(&engine, &wires, supervisors).await;
    let _ = std::fs::remove_file(&socket);
    // A requested stop is a clean one, as with `docker compose`.
    ended.map(|_| 0)
}

/// Stop requests for a command that has children to look after.
///
/// The first is the caller's to act on with an orderly shutdown. Any request
/// after a shutdown has begun kills every child's process tree and exits:
/// whoever sends it has decided not to wait, and exiting without the kill is
/// how children were orphaned (cairn 0085).
fn stop_requests(engine: &Arc<Engine>) -> Result<turborust::signals::Stop> {
    let signals = turborust::signals::Signals::install().context("installing signal handlers")?;
    let engine = engine.clone();
    Ok(turborust::signals::watch(signals, move |request| {
        eprintln!(
            "\nturborust: {} again — killing every process now",
            request.name()
        );
        engine.kill_all();
        std::process::exit(request.exit_code());
    }))
}

fn cmd_init() -> Result<i32> {
    let root = std::env::current_dir()?;
    let path = root.join("turborust.toml");
    if path.exists() {
        anyhow::bail!(
            "{} already exists. Delete it, or edit it directly — `plan` shows what it resolves to.",
            path.display()
        );
    }
    let meta = cargo::Metadata::load(&root).unwrap_or(None);
    let procfile = std::fs::read_to_string(root.join("Procfile"))
        .ok()
        .map(|text| turborust::procfile::parse(&text));
    let has_env = root.join(".env").is_file();
    let config = scaffold(meta.as_ref(), procfile.as_ref(), has_env);
    std::fs::write(&path, &config)?;

    println!("wrote {}", path.display());
    if let Some(p) = &procfile {
        println!(
            "  imported {} process(es) from Procfile{}",
            p.processes.len(),
            if has_env {
                ", and .env as env_file"
            } else {
                ""
            }
        );
        for why in &p.skipped {
            println!("  skipped Procfile {why}");
        }
    }
    match meta.as_ref() {
        Some(m) if !m.packages.is_empty() => {
            let bins = m.packages.values().filter(|p| p.has_bin).count();
            let wasm = m.packages.values().filter(|p| p.is_wasm_lib).count();
            println!(
                "  inferred from {} crate(s): {bins} binary, {wasm} wasm",
                m.packages.len()
            );
        }
        _ => println!("  no cargo workspace here, so the file is a starting point"),
    }
    println!("\nnext:");
    println!("  turborust plan     see what that resolves to, including derived globs");
    println!("  turborust doctor   what is costing you seconds on every rebuild");
    println!("  turborust up       start it");
    Ok(0)
}

/// Builds a starter config, inferred from the workspace where possible.
///
/// Deliberately verbose: a generated config that only lists what it inferred
/// teaches nothing, and the settings people most need are the ones they do not
/// know to look for.
fn scaffold(
    meta: Option<&cargo::Metadata>,
    procfile: Option<&turborust::procfile::Procfile>,
    has_env: bool,
) -> String {
    // taplo and the VS Code TOML extensions read this line, so an editor
    // completes and validates the file without any per-editor configuration.
    let mut out = String::from(
        "#:schema https://raw.githubusercontent.com/oddurs/turborust/main/turborust.schema.json\n\
         # Generated by `turborust init`. `turborust plan` shows what it resolves to.\n\n",
    );

    let procs: &[turborust::procfile::Process] =
        procfile.map(|p| p.processes.as_slice()).unwrap_or(&[]);
    let meta = meta.filter(|m| !m.packages.is_empty());
    // A Procfile process is what the project actually runs, so it wins a name
    // collision with a crate the workspace merely contains.
    let bins: Vec<&cargo::Package> = meta
        .iter()
        .flat_map(|m| m.packages.values())
        .filter(|p| p.has_bin && !procs.iter().any(|q| q.name == p.name))
        .collect();
    let wasm: Vec<&cargo::Package> = meta
        .iter()
        .flat_map(|m| m.packages.values())
        .filter(|p| p.is_wasm_lib)
        .collect();

    if procs.is_empty() && bins.is_empty() && wasm.is_empty() {
        out.push_str(EXAMPLE);
        return out;
    }

    out.push_str(
        "[project]\n\
         # Build parallelism. Defaults to your core count, capped at 8.\n\
         # concurrency = 4\n",
    );
    // foreman, honcho and overmind all read it, so the processes expect it.
    if has_env {
        out.push_str("env_file = \".env\"\n");
    }
    out.push('\n');

    for p in procs {
        let cmd = toml::Value::String(p.command.clone()).to_string();
        out.push_str(&format!(
            "# From the Procfile.\n[services.{}]\ncmd = {cmd}\n",
            p.name
        ));
        // The PORT foreman would have assigned, and the probe it implies.
        if let Some(port) = p.port {
            out.push_str(&format!("env = {{ PORT = \"{port}\" }}\nport = {port}\n"));
        }
        out.push_str("# watch = [\"src/**\"]   # restart it when these change\n\n");
    }

    for p in &bins {
        out.push_str(&format!(
            "[services.{name}]\n\
             # `cargo` derives the run command, the watch globs, and the rebuild\n\
             # edges from this crate's path dependencies — so editing a shared\n\
             # crate restarts this without you listing it.\n\
             cargo = \"{name}\"\n\
             # port = 8080                      # implies a TCP readiness probe\n\
             # health = {{ log = \"listening\" }}   # or wait for it to say so\n\n",
            name = p.name
        ));
    }

    for p in &wasm {
        let port = 8080 + bins.len() as u16;
        out.push_str(&format!(
            "[tasks.{name}-build]\n\
             cargo = \"{name}\"\n\
             cmd = \"trunk build\"        # or: cargo leptos build / dx build\n\
             outputs = [\"dist\"]\n\n\
             [services.{name}]\n\
             depends_on = [\"{name}-build\"]\n\
             serve = {{ dir = \"dist\", port = {port} }}\n",
            name = p.name,
        ));
        // One origin is the thing people reach for first and rarely know exists.
        if let Some(backend) = bins.first() {
            out.push_str(&format!(
                "# One origin: the page calls /api/... with a relative path, so there\n\
                 # is no CORS to configure and no API origin to swap for production.\n\
                 # proxy = [{{ path = \"/api\", to = \"http://127.0.0.1:8080\" }}]\n\
                 # (point `to` at wherever `{}` listens)\n",
                backend.name
            ));
        }
        out.push('\n');
    }

    out.push_str(
        "# [overlay]                  # the crab in the corner of the served page\n\
         # position = \"bottom-right\"\n\
         # errors = \"overlay\"        # overlay | badge | silent\n",
    );
    out
}

const EXAMPLE: &str = r#"[services.api]
cmd = "cargo run -p api"
port = 8080
watch = ["crates/**/*.rs"]

[tasks.build]
cmd = "cargo build"
inputs = ["crates/**/*.rs", "Cargo.lock"]
outputs = ["target/debug/api"]
"#;

/// Runs each task twice and compares what it produced.
///
/// The cache maps a key to a set of outputs and replays them on a hit. That is
/// only sound if the same key really does imply the same outputs — and nothing
/// checks. A task that embeds a timestamp, iterates a `HashMap`, or bakes in an
/// absolute path will have one of its several possible answers served forever,
/// silently.
///
/// Declared outputs are removed before each run. Without that, a second run of
/// an incremental tool is usually a no-op that reproduces byte-identical files
/// and proves nothing.
async fn cmd_verify(
    config: Option<&std::path::Path>,
    targets: Vec<String>,
    json: bool,
) -> Result<i32> {
    let ws = load(config)?;
    let all = targets.is_empty();
    // Resolving with no targets means "the default set", which is the services —
    // and a workspace of pure tasks has none. Verifying everything has to name
    // everything.
    let resolved: Vec<String> = if all {
        ws.config.tasks.keys().cloned().collect()
    } else {
        targets.clone()
    };
    if resolved.is_empty() {
        println!("\n  no tasks defined, so there is nothing to verify.\n");
        return Ok(0);
    }
    let plan = Arc::new(plan::resolve(&ws, &resolved)?);

    let checkable: Vec<String> = plan
        .nodes
        .values()
        .filter(|n| n.kind == Kind::Task && !n.outputs.is_empty())
        .filter(|n| all || targets.contains(&n.name))
        .map(|n| n.name.clone())
        .collect();

    if checkable.is_empty() {
        // Not a failure: there is simply nothing here whose determinism is
        // observable. A task with no declared outputs produces nothing to compare.
        println!("\n  no task declares `outputs`, so there is nothing to compare.\n");
        return Ok(0);
    }

    let cache = Arc::new(open_cache(&ws)?);
    let (engine, rx) = Engine::new(plan.clone(), cache);
    tokio::spawn(turborust::engine::pump_events(
        engine.state.clone(),
        rx,
        false,
    ));

    // Bring dependencies up first, so each pair of runs differs only in when it
    // happened.
    // Any request once this first build is done goes straight to the kill: the
    // comparison runs below spawn children too, and none of them may outlive us.
    let mut stop = stop_requests(&engine)?;
    let stopped = std::cell::Cell::new(false);
    let code = turborust::engine::run_once_until(engine.clone(), false, async {
        let request = stop.requested().await;
        stopped.set(true);
        request.exit_code()
    })
    .await?;
    if stopped.get() {
        return Ok(code);
    }
    if code != 0 {
        eprintln!("\n  the plan does not build, so there is nothing to verify.\n");
        return Ok(code);
    }

    let mut reports = Vec::new();
    for name in &checkable {
        let node = plan.get(name)?.clone();
        let mut runs = Vec::new();
        for _ in 0..2 {
            engine.clear_outputs(&node);
            let outcome = engine.run_task(name, true).await?;
            if outcome.code != 0 {
                anyhow::bail!("`{name}` failed while verifying (exit {})", outcome.code);
            }
            let files = engine.declared_outputs(&node);
            let mut hashes = std::collections::BTreeMap::new();
            for rel in &files {
                if let Ok(bytes) = std::fs::read(ws.root.join(rel)) {
                    hashes.insert(rel.clone(), blake3::hash(&bytes).to_hex().to_string());
                }
            }
            // Keep the first run's bytes so the second can be diffed against them.
            let stash = ws
                .cache_dir()
                .join("verify")
                .join(safe_name(name))
                .join(if runs.is_empty() { "a" } else { "b" });
            let _ = std::fs::remove_dir_all(&stash);
            for rel in &files {
                let dest = stash.join(rel);
                if let Some(parent) = dest.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let _ = std::fs::copy(ws.root.join(rel), dest);
            }
            runs.push((hashes, stash));
        }

        let (first, first_dir) = &runs[0];
        let (second, second_dir) = &runs[1];
        let mut differing = Vec::new();
        for (rel, a) in first {
            match second.get(rel) {
                Some(b) if b == a => {}
                Some(b) => differing.push(DifferingOutput {
                    path: rel.clone(),
                    first: Some(a.clone()),
                    second: Some(b.clone()),
                    divergence: turborust::cache::first_difference(
                        &first_dir.join(rel),
                        &second_dir.join(rel),
                    ),
                }),
                None => differing.push(DifferingOutput {
                    path: rel.clone(),
                    first: Some(a.clone()),
                    second: None,
                    divergence: None,
                }),
            }
        }
        for (rel, b) in second {
            if !first.contains_key(rel) {
                differing.push(DifferingOutput {
                    path: rel.clone(),
                    first: None,
                    second: Some(b.clone()),
                    divergence: None,
                });
            }
        }
        let _ = std::fs::remove_dir_all(ws.cache_dir().join("verify").join(safe_name(name)));
        reports.push(VerifyTask {
            task: name.clone(),
            deterministic: differing.is_empty(),
            outputs: first.len(),
            differing,
        });
    }

    let bad = reports.iter().filter(|r| !r.deterministic).count();

    if json {
        let report = VerifyReport {
            schema_version: Output::Verify.version(),
            deterministic: bad == 0,
            tasks: reports,
        };
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok((bad > 0) as i32);
    }

    println!();
    for r in &reports {
        let task = &r.task;
        if r.deterministic {
            println!(
                "  \u{1b}[32m✓\u{1b}[0m \u{1b}[1m{task}\u{1b}[0m \u{1b}[2m— deterministic ({} output(s), 2 runs identical)\u{1b}[0m",
                r.outputs
            );
            continue;
        }
        println!(
            "  \u{1b}[31m✗\u{1b}[0m \u{1b}[1m{task}\u{1b}[0m \u{1b}[31mNOT DETERMINISTIC\u{1b}[0m"
        );
        for d in &r.differing {
            let short = |v: &Option<String>| {
                v.as_deref()
                    .map(|h| format!("b3:{}", &h[..8.min(h.len())]))
                    .unwrap_or_else(|| "absent".into())
            };
            println!(
                "      ~ {}  {} -> {}",
                d.path,
                short(&d.first),
                short(&d.second)
            );
            if let Some(v) = &d.divergence {
                let (offset, run, la, lb) = (v.offset, v.run, v.left_len, v.right_len);
                let hint = if la != lb {
                    "different lengths — output is not just a stamped field"
                } else if run <= 16 {
                    "a short run: usually an embedded timestamp or build id"
                } else {
                    "a long run: usually ordering, such as map iteration"
                };
                println!(
                    "        differs at offset {offset:#x}, {run} byte(s) — \u{1b}[2m{hint}\u{1b}[0m"
                );
            }
        }
    }

    if bad > 0 {
        println!(
            "\n  \u{1b}[31m{bad} task(s) are not reproducible.\u{1b}[0m A cache key cannot promise\n  \
             an output it does not determine — see the causes in the docs.\n"
        );
    } else {
        println!();
    }
    Ok((bad > 0) as i32)
}

/// Task names reach the filesystem here; keep them to one safe segment.
fn safe_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod scaffold_tests {
    use super::*;

    #[test]
    fn a_workspaceless_scaffold_still_parses() {
        let toml = scaffold(None, None, false);
        toml::from_str::<turborust::config::Config>(&toml)
            .expect("the example config must be valid");
    }

    #[test]
    fn a_procfile_becomes_services_that_run_what_it_ran() {
        // Cover for cairn 0097.
        let procfile = turborust::procfile::parse(
            "web: bundle exec puma -p $PORT -C \"config/puma.rb\"\nworker: bundle exec sidekiq\n",
        );
        let toml = scaffold(None, Some(&procfile), true);
        let dir = std::env::temp_dir().join(format!("tr-procfile-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(".env"), "SECRET=1\n").unwrap();
        let ws = Workspace {
            root: dir.clone(),
            config: toml::from_str(&toml).unwrap_or_else(|e| panic!("{e}\n{toml}")),
            source: Some(toml.clone()),
        };
        let plan = plan::resolve(&ws, &[]).unwrap_or_else(|e| panic!("{e:#}\n{toml}"));
        let web = plan.get("web").unwrap();
        assert_eq!(
            web.cmd.as_deref(),
            Some("bundle exec puma -p $PORT -C \"config/puma.rb\"")
        );
        assert_eq!(web.env.get("PORT").map(String::as_str), Some("5000"));
        assert_eq!(
            web.port,
            Some(5000),
            "a process that reads $PORT is probed on it"
        );
        let worker = plan.get("worker").unwrap();
        assert_eq!(worker.cmd.as_deref(), Some("bundle exec sidekiq"));
        assert_eq!(worker.port, None);
        assert!(
            web.env_files.iter().any(|f| f.path.ends_with(".env")),
            "the .env beside the Procfile is loaded"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn without_a_dotenv_the_scaffold_does_not_ask_for_one() {
        let procfile = turborust::procfile::parse("web: ./serve\n");
        assert!(!scaffold(None, Some(&procfile), false).contains("env_file"));
    }

    #[test]
    fn the_scaffold_declares_its_schema() {
        // Editors pick the schema up from this line and nothing else.
        assert!(
            scaffold(None, None, false).starts_with("#:schema "),
            "no schema directive"
        );
    }

    #[test]
    fn every_commented_suggestion_is_a_real_setting() {
        // A generated config that suggests a key which does not exist is worse
        // than one that suggests nothing: `deny_unknown_fields` turns it into an
        // error the moment someone uncomments it.
        let toml = scaffold(None, None, false);
        for line in toml.lines() {
            let Some(body) = line.trim().strip_prefix("# ") else {
                continue;
            };
            if !body.contains(" = ") || body.starts_with('(') {
                continue;
            }
            let probe = format!("[services.x]\ncmd = \"true\"\n{body}");
            if toml::from_str::<turborust::config::Config>(&probe).is_err() {
                // Not every suggestion belongs under [services]; try a table.
                let probe = format!("[project]\n{body}");
                assert!(
                    toml::from_str::<turborust::config::Config>(&probe).is_ok(),
                    "suggested setting is not valid anywhere: {body}"
                );
            }
        }
    }
}
