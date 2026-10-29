//! `turborust mcp`, driven over stdio the way an MCP client drives it
//! (cairn 0077). The live-`up` tools are covered by the control socket's own
//! tests; this pins the protocol, the contract and the opt-in.
#![cfg(unix)]

use serde_json::{Value, json};
use std::io::Write;
use std::process::{Command, Stdio};

fn session(dir: &std::path::Path, flags: &[&str], requests: &[Value]) -> Vec<Value> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_turborust"))
        .arg("mcp")
        .args(flags)
        .current_dir(dir)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    for r in requests {
        writeln!(stdin, "{r}").unwrap();
    }
    drop(stdin); // the client hangs up; the server must exit
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| serde_json::from_str(l).expect("one JSON-RPC message per line"))
        .collect()
}

fn call(id: u64, name: &str, arguments: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": "tools/call", "params": {"name": name, "arguments": arguments}})
}

#[test]
fn an_agent_gets_contracted_answers_and_no_way_to_run_unless_allowed() {
    let dir = std::env::temp_dir().join(format!("turborust-mcp-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("in.txt"), "1").unwrap();
    std::fs::write(
        dir.join("turborust.toml"),
        "[tasks.build]\ncmd = \"true\"\ninputs = [\"in.txt\"]\n",
    )
    .unwrap();

    let replies = session(
        &dir,
        &[],
        &[
            json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "1999-01-01", "capabilities": {}, "clientInfo": {"name": "t", "version": "0"}}}),
            json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
            json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
            call(3, "dry_run", json!({"targets": ["build"]})),
            call(4, "state", json!({})),
            call(5, "run", json!({"targets": ["build"]})),
            json!({"jsonrpc": "2.0", "id": 6, "method": "nope"}),
        ],
    );
    assert_eq!(
        replies.len(),
        6,
        "a notification gets no reply: {replies:#?}"
    );
    let by_id = |id: u64| replies.iter().find(|r| r["id"] == id).unwrap();

    // An unknown revision is offered the newest one this speaks.
    assert_eq!(by_id(1)["result"]["protocolVersion"], "2025-06-18");

    let tools = by_id(2)["result"]["tools"].as_array().unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(
        names,
        ["state", "history", "dry_run"],
        "read-only by default"
    );
    // The 0078 schema, not a second one.
    let schema = Command::new(env!("CARGO_BIN_EXE_turborust"))
        .args(["schema", "--json", "dry-run"])
        .output()
        .unwrap()
        .stdout;
    let schema: Value = serde_json::from_slice(&schema).unwrap();
    assert_eq!(tools[2]["outputSchema"], schema);

    let dry = &by_id(3)["result"];
    assert_eq!(dry["isError"], false);
    assert_eq!(dry["structuredContent"]["schemaVersion"], 1);
    assert_eq!(dry["structuredContent"]["tasks"][0]["task"], "build");

    // No `up` here: an answer that says so, and what still works.
    let state = &by_id(4)["result"];
    assert_eq!(state["isError"], true);
    let text = state["content"][0]["text"].as_str().unwrap();
    assert!(
        text.contains("no `turborust up` is running") && text.contains("dry_run"),
        "{text}"
    );

    assert_eq!(
        by_id(5)["error"]["code"],
        -32602,
        "`run` does not exist unless allowed"
    );
    assert_eq!(by_id(6)["error"]["code"], -32601);

    // Allowed, it runs and answers with the run's summary.
    let replies = session(
        &dir,
        &["--allow-run"],
        &[call(1, "run", json!({"targets": ["build"]}))],
    );
    let run = &replies[0]["result"];
    assert_eq!(run["isError"], false, "{run}");
    assert_eq!(run["structuredContent"]["exit_code"], 0);
    assert_eq!(run["structuredContent"]["tasks"][0]["task"], "build");
    let _ = std::fs::remove_dir_all(&dir);
}
