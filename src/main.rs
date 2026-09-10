use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use turborust::cache::Cache;
use turborust::config::Workspace;
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
    /// Report what is costing you seconds on every rebuild.
    Doctor {
        /// Offer to apply each suggestion, one at a time. Never applies anything
        /// without asking.
        #[arg(long)]
        fix: bool,
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
    /// Print a shell completion script.
    ///
    /// Generated rather than committed, so it cannot drift from the CLI.
    Completions {
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
    /// Print the man page, in roff.
    Man,
    /// Print a JSON Schema for turborust.toml, for editor completion.
    Schema,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let code = rt.block_on(dispatch(cli))?;
    // Drop the runtime before exiting so pty reader threads are not killed
    // mid-write, which would truncate the last lines of output.
    drop(rt);
    std::process::exit(code);
}

async fn dispatch(cli: Cli) -> Result<i32> {
    match cli.command.unwrap_or(Cmd::Up {
        targets: vec![],
        filter: vec![],
        no_tui: false,
    }) {
        Cmd::Init => cmd_init(),
        Cmd::Doctor { fix } => cmd_doctor(cli.config.as_deref(), fix),
        Cmd::Plan => cmd_plan(cli.config.as_deref()),
        Cmd::Graph { format, with_cache } => cmd_graph(cli.config.as_deref(), &format, with_cache),
        Cmd::Why { target } => cmd_why(cli.config.as_deref(), &target),
        Cmd::Connect { node } => {
            let ws = load(cli.config.as_deref())?;
            turborust::control::connect(&ws.cache_dir(), &node).await
        }
        Cmd::Clean => cmd_clean(cli.config.as_deref()),
        Cmd::Completions { shell } => {
            let mut cmd = <Cli as clap::CommandFactory>::command();
            let name = cmd.get_name().to_string();
            clap_complete::generate(shell, &mut cmd, name, &mut std::io::stdout());
            Ok(0)
        }
        Cmd::Schema => {
            let schema = schemars::schema_for!(turborust::config::Config);
            println!("{}", serde_json::to_string_pretty(&schema)?);
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
        } => cmd_up(cli.config.as_deref(), targets, filter, no_tui).await,
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
    Cache::with_shared(&ws.cache_dir(), shared, ws.config.cache.push)
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
    let findings = doctor::diagnose_with(&root, meta.as_ref(), &split);
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
    Cache::new(&ws.cache_dir())?.clear()?;
    println!("cache cleared");
    Ok(0)
}

/// The feature the whole design exists to support: a straight answer to
/// "why did that rebuild?"
fn cmd_why(config: Option<&std::path::Path>, target: &str) -> Result<i32> {
    let ws = load(config)?;
    let plan = Arc::new(plan::resolve(&ws, &[target.to_string()])?);
    let node = plan.get(target)?.clone();
    let cache = Arc::new(Cache::new(&ws.cache_dir())?);
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
    let (engine, rx) = Engine::new(plan.clone(), cache.clone());
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
    let code = engine::run_once_with(engine.clone(), output.keep_going).await?;

    if output.summarize {
        let summary = turborust::summary::RunSummary {
            schema: turborust::summary::SCHEMA,
            started_at: started,
            duration_ms: began.elapsed().as_millis() as u64,
            exit_code: code,
            global: plan.global.clone(),
            tasks: engine.outcomes(),
        };
        let path = turborust::summary::write(&ws.cache_dir(), &summary)?;
        println!("summary written to {}", path.display());
    }
    // Let the pump drain the last lines before the process exits.
    tokio::time::sleep(Duration::from_millis(60)).await;
    Ok(code)
}

async fn cmd_up(
    config: Option<&std::path::Path>,
    targets: Vec<String>,
    filter: Vec<String>,
    no_tui: bool,
) -> Result<i32> {
    let ws = load(config)?;
    let targets = select(&ws, targets, &filter)?;
    let watcher =
        watch::watch(&ws.root, Duration::from_millis(120)).context("starting file watcher")?;
    let plan = Arc::new(plan::resolve(&ws, &targets)?);
    let cache = Arc::new(open_cache(&ws)?);
    let (engine, rx) = Engine::new(plan.clone(), cache);

    tokio::spawn(engine::pump_events(engine.state.clone(), rx, no_tui));

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
                    proc: name.clone(),
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
                    proc: name.clone(),
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

    if no_tui {
        println!("turborust: {} node(s) — ctrl-c to stop", plan.order.len());
        tokio::signal::ctrl_c().await.ok();
    } else {
        tui::run(engine.clone(), wires.clone()).await?;
    }

    engine::shutdown(&engine, &wires, supervisors).await;
    let _ = std::fs::remove_file(&socket);
    Ok(0)
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
    let config = scaffold(meta.as_ref());
    std::fs::write(&path, &config)?;

    println!("wrote {}", path.display());
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
fn scaffold(meta: Option<&cargo::Metadata>) -> String {
    // taplo and the VS Code TOML extensions read this line, so an editor
    // completes and validates the file without any per-editor configuration.
    let mut out = String::from(
        "#:schema https://raw.githubusercontent.com/oddurs/turborust/main/turborust.schema.json\n\
         # Generated by `turborust init`. `turborust plan` shows what it resolves to.\n\n",
    );

    let Some(meta) = meta.filter(|m| !m.packages.is_empty()) else {
        out.push_str(EXAMPLE);
        return out;
    };

    let bins: Vec<&cargo::Package> = meta.packages.values().filter(|p| p.has_bin).collect();
    let wasm: Vec<&cargo::Package> = meta.packages.values().filter(|p| p.is_wasm_lib).collect();

    if bins.is_empty() && wasm.is_empty() {
        out.push_str(EXAMPLE);
        return out;
    }

    out.push_str(
        "[project]\n\
         # Build parallelism. Defaults to your core count, capped at 8.\n\
         # concurrency = 4\n\n",
    );

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

#[cfg(test)]
mod scaffold_tests {
    use super::*;

    #[test]
    fn a_workspaceless_scaffold_still_parses() {
        let toml = scaffold(None);
        toml::from_str::<turborust::config::Config>(&toml)
            .expect("the example config must be valid");
    }

    #[test]
    fn the_scaffold_declares_its_schema() {
        // Editors pick the schema up from this line and nothing else.
        assert!(
            scaffold(None).starts_with("#:schema "),
            "no schema directive"
        );
    }

    #[test]
    fn every_commented_suggestion_is_a_real_setting() {
        // A generated config that suggests a key which does not exist is worse
        // than one that suggests nothing: `deny_unknown_fields` turns it into an
        // error the moment someone uncomments it.
        let toml = scaffold(None);
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
