//! `turborust mcp`: the read-only surface, for an agent (cairn 0077).
//!
//! The Model Context Protocol over stdio: newline-delimited JSON-RPC 2.0, as
//! the spec's stdio transport defines it. Only what a tool server needs is
//! implemented (`initialize`, `ping`, `tools/list`, `tools/call`), by hand,
//! because the protocol is that small and no crate for it is worth a new
//! dependency.
//!
//! Every tool answers with a payload that already has a contract (0078), and
//! declares that payload's schema as its `outputSchema`. Nothing here defines
//! a shape of its own:
//!
//! - `state` and `history` ask a running `up` over the control socket;
//! - `dry_run` runs `turborust run --dry-run=json`, which works with or
//!   without one;
//! - `run`, the only tool that changes anything, exists only under
//!   `turborust mcp --allow-run`.

use crate::contract::Output;
use serde_json::{Value, json};
use std::io::{BufRead, Write};
use std::path::PathBuf;

/// Protocol revisions this server speaks, newest first. The newest is
/// offered to a client that asks for one it does not know.
const PROTOCOL_VERSIONS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];

pub struct Server {
    /// `-c`, passed on to the commands this runs.
    pub config: Option<PathBuf>,
    pub allow_run: bool,
    /// The turborust binary to run for `dry_run` and `run`.
    pub exe: PathBuf,
}

/// Serves stdin to stdout until stdin closes. Blocking: call it off the
/// runtime `rt` belongs to, as `spawn_blocking` does.
pub fn serve(server: Server, rt: tokio::runtime::Handle) -> anyhow::Result<()> {
    let stdin = std::io::stdin();
    let mut out = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        if let Some(reply) = server.handle(&line, &rt) {
            writeln!(out, "{reply}")?;
            out.flush()?;
        }
    }
    Ok(())
}

impl Server {
    /// One message in, at most one out: notifications get no answer.
    pub fn handle(&self, line: &str, rt: &tokio::runtime::Handle) -> Option<Value> {
        let msg: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => return Some(error(Value::Null, -32700, &format!("parse error: {e}"))),
        };
        let id = msg.get("id").cloned();
        let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
        let params = msg.get("params").cloned().unwrap_or(json!({}));
        let id = id?; // a notification
        let result = match method {
            "initialize" => Ok(self.initialize(&params)),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({ "tools": self.tools() })),
            "tools/call" => self.call(&params, rt),
            other => Err((-32601, format!("unknown method `{other}`"))),
        };
        Some(match result {
            Ok(r) => json!({ "jsonrpc": "2.0", "id": id, "result": r }),
            Err((code, msg)) => error(id, code, &msg),
        })
    }

    fn initialize(&self, params: &Value) -> Value {
        let asked = params.get("protocolVersion").and_then(Value::as_str);
        let version = asked
            .filter(|v| PROTOCOL_VERSIONS.contains(v))
            .unwrap_or(PROTOCOL_VERSIONS[0]);
        json!({
            "protocolVersion": version,
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "turborust", "version": env!("CARGO_PKG_VERSION") },
            "instructions": "turborust supervises a Rust dev stack. `state` and `history` read a running `turborust up`; `dry_run` says what `run` would execute and why, without one.",
        })
    }

    fn tools(&self) -> Vec<Value> {
        let mut tools = vec![
            json!({
                "name": "state",
                "description": "Every node of the running `turborust up`: status, pid, restarts, and the compile errors and failing tests parsed from its output.",
                "inputSchema": { "type": "object", "properties": {} },
                "outputSchema": Output::State.schema(),
                "annotations": { "readOnlyHint": true },
            }),
            json!({
                "name": "history",
                "description": "What happened to the nodes of the running `turborust up`, oldest first: starts, readiness, restarts and why, failures.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node": { "type": "string", "description": "Only this node." },
                        "limit": { "type": "integer", "minimum": 1, "maximum": 10000, "description": "Newest entries to return (default 100)." },
                    },
                },
                "outputSchema": Output::History.schema(),
                "annotations": { "readOnlyHint": true },
            }),
            json!({
                "name": "dry_run",
                "description": "What `turborust run` would execute right now and what it would serve from cache, with the inputs that changed for each miss. Runs nothing.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "targets": { "type": "array", "items": { "type": "string" }, "description": "Tasks to plan; default all." },
                    },
                },
                "outputSchema": Output::DryRun.schema(),
                "annotations": { "readOnlyHint": true },
            }),
        ];
        if self.allow_run {
            tools.push(json!({
                "name": "run",
                "description": "Run tasks once, honouring the cache, and return the run's summary: each task's outcome, the issues found, and the exit code.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "targets": { "type": "array", "items": { "type": "string" }, "minItems": 1 },
                        "force": { "type": "boolean", "description": "Ignore cached results." },
                    },
                    "required": ["targets"],
                },
                "outputSchema": Output::Summary.schema(),
                "annotations": { "readOnlyHint": false, "destructiveHint": false },
            }));
        }
        tools
    }

    fn call(&self, params: &Value, rt: &tokio::runtime::Handle) -> Result<Value, (i64, String)> {
        let name = params.get("name").and_then(Value::as_str).unwrap_or("");
        let args = params.get("arguments").cloned().unwrap_or(json!({}));
        let strings = |key: &str| -> Vec<String> {
            args.get(key)
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default()
        };
        let outcome = match name {
            "state" => {
                rt.block_on(self.ask_up(|dir| async move { crate::control::state(&dir).await }))
            }
            "history" => {
                let node = args.get("node").and_then(Value::as_str).map(String::from);
                let limit = args
                    .get("limit")
                    .and_then(Value::as_u64)
                    .unwrap_or(100)
                    .clamp(1, 10_000) as usize;
                rt.block_on(self.ask_up(move |dir| async move {
                    let entries = crate::control::history(&dir, node.as_deref(), limit).await?;
                    Ok(entries.map(|history| {
                        json!(crate::contract::HistoryReport {
                            schema_version: Output::History.version(),
                            history,
                        })
                    }))
                }))
            }
            "dry_run" => {
                let mut cmd = vec!["run".to_string(), "--dry-run=json".to_string()];
                cmd.extend(strings("targets"));
                self.turborust(&cmd).and_then(|(stdout, _)| {
                    serde_json::from_str(&stdout).map_err(|e| format!("unreadable dry run: {e}"))
                })
            }
            "run" if self.allow_run => {
                let targets = strings("targets");
                if targets.is_empty() {
                    return Err((-32602, "`run` needs at least one target".into()));
                }
                let mut cmd = vec![
                    "run".to_string(),
                    "--summarize".to_string(),
                    "--output-logs".to_string(),
                    "none".to_string(),
                ];
                if args.get("force").and_then(Value::as_bool) == Some(true) {
                    cmd.push("--force".into());
                }
                cmd.extend(targets);
                // A run that fails is still an answer: its summary says how.
                self.turborust(&cmd)
                    .and_then(|(stdout, _)| summary_from(&stdout))
            }
            other => return Err((-32602, format!("unknown tool `{other}`"))),
        };
        Ok(match outcome {
            Ok(value) => json!({
                "content": [{ "type": "text", "text": value.to_string() }],
                "structuredContent": value,
                "isError": false,
            }),
            // Tool errors are results the model can read, not protocol errors.
            Err(why) => json!({
                "content": [{ "type": "text", "text": why }],
                "isError": true,
            }),
        })
    }

    /// Runs a control-socket question against the `up` here, and says plainly
    /// when there is none, with what still works without one.
    async fn ask_up<F, Fut>(&self, ask: F) -> Result<Value, String>
    where
        F: FnOnce(PathBuf) -> Fut,
        Fut: std::future::Future<Output = anyhow::Result<Option<Value>>>,
    {
        let ws =
            crate::config::Workspace::load(self.config.as_deref()).map_err(|e| format!("{e:#}"))?;
        match ask(ws.cache_dir()).await {
            Ok(Some(v)) => Ok(v),
            Ok(None) => Err(format!(
                "no `turborust up` is running in {}. `dry_run` works without one; start `turborust up` for live state.",
                ws.root.display()
            )),
            Err(e) => Err(format!("{e:#}")),
        }
    }

    /// Runs this turborust with `args`. A failure is reported from its
    /// structured error, not its prose.
    fn turborust(&self, args: &[String]) -> Result<(String, i32), String> {
        let mut cmd = std::process::Command::new(&self.exe);
        if let Some(c) = &self.config {
            cmd.arg("-c").arg(c);
        }
        cmd.args(args)
            .args(["--error-format", "json"])
            .stdin(std::process::Stdio::null());
        let out = cmd
            .output()
            .map_err(|e| format!("running turborust: {e}"))?;
        let code = out.status.code().unwrap_or(1);
        let stderr = String::from_utf8_lossy(&out.stderr);
        if let Some(report) = stderr
            .lines()
            .rev()
            .find_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter(|v| v.get("error").is_some())
        {
            let e = &report["error"];
            return Err(format!(
                "{} ({})",
                e["message"].as_str().unwrap_or("failed"),
                e["code"].as_str().unwrap_or("error")
            ));
        }
        Ok((String::from_utf8_lossy(&out.stdout).into_owned(), code))
    }
}

/// The summary a `run --summarize` wrote, from the path it printed last.
fn summary_from(stdout: &str) -> Result<Value, String> {
    let path = stdout
        .lines()
        .rev()
        .find_map(|l| l.strip_prefix("summary written to "))
        .ok_or("the run wrote no summary")?;
    let text = std::fs::read_to_string(path.trim()).map_err(|e| format!("reading {path}: {e}"))?;
    serde_json::from_str(&text).map_err(|e| format!("unreadable summary: {e}"))
}

fn error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}
