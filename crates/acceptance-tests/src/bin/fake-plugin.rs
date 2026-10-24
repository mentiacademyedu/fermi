//! A deterministic plugin (an MCP server over stdio) for the plugin tests.
//!
//! Flags: `--label <name>` (shown in its answers), `--tools a,b,…` (which
//! tools it offers; default `echo`), `--legacy` (an older server: no
//! `server/discover`, needs `initialize`), `--marker <file>` (a line is
//! appended each time it starts), `--log <file>` (every request it receives),
//! `--sentinel <file>` (what `read_sentinel` reads on its own), `--script
//! <file>` (the replies `complete` gives, in order, as a model adapter).
//!
//! Tools: `echo {text}`, `greet {name}`, `add {a, b}`, `boom` (panics with a
//! distinctive message), `hang` (never answers), `fail` (a tool error),
//! `read_file {path}`, `read_sentinel`, `env` (the variables it was given),
//! `complete` (adapter).

use serde_json::{json, Value};
use std::io::{BufRead, Write};

const MODERN: &str = "2026-07-28";

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

fn append(path: &Option<String>, line: &str) {
    if let Some(p) = path {
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(p) {
            let _ = writeln!(f, "{line}");
        }
    }
}

fn schema(tool: &str) -> Value {
    match tool {
        "echo" => {
            json!({ "type": "object", "properties": { "text": { "type": "string", "minLength": 1 } }, "required": ["text"], "additionalProperties": false })
        }
        "greet" => json!({ "type": "object", "properties": { "name": { "type": "string" } }, "required": ["name"] }),
        "add" => {
            json!({ "type": "object", "properties": { "a": { "type": "integer" }, "b": { "type": "integer" } }, "required": ["a", "b"] })
        }
        "read_file" => json!({ "type": "object", "properties": { "path": { "type": "string" } }, "required": ["path"] }),
        "complete" => json!({ "type": "object", "properties": { "prompt": { "type": "string" } }, "required": ["prompt"] }),
        _ => json!({ "type": "object", "properties": {} }),
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let label = flag(&args, "--label").unwrap_or_else(|| "plugin".into());
    let tools: Vec<String> = flag(&args, "--tools").unwrap_or_else(|| "echo".into()).split(',').map(str::to_string).collect();
    let legacy = args.iter().any(|a| a == "--legacy");
    let log = flag(&args, "--log");
    append(&flag(&args, "--marker"), &format!("start {}", std::process::id()));

    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    let mut send = |v: Value| {
        let _ = writeln!(out, "{v}");
        let _ = out.flush();
    };
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let Ok(msg) = serde_json::from_str::<Value>(&line) else { continue };
        append(&log, &json!({ "method": msg["method"], "params": msg["params"] }).to_string());
        let Some(id) = msg.get("id").cloned() else { continue };
        let ok = |r: Value| json!({ "jsonrpc": "2.0", "id": id, "result": r });
        let fail = |code: i64, m: String| json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": m } });
        let done = |mut r: Value| {
            if !legacy {
                r["resultType"] = json!("complete");
            }
            ok(r)
        };
        match msg["method"].as_str().unwrap_or_default() {
            "server/discover" if !legacy => send(done(json!({
                "supportedVersions": [MODERN], "capabilities": { "tools": {} },
                "_meta": { "io.modelcontextprotocol/serverInfo": { "name": label, "version": "1.0.0" } }
            }))),
            "initialize" => send(ok(json!({
                "protocolVersion": msg["params"]["protocolVersion"], "capabilities": { "tools": {} },
                "serverInfo": { "name": label, "version": "1.0.0" }
            }))),
            "tools/list" => {
                let list: Vec<Value> = tools
                    .iter()
                    .map(|t| json!({ "name": t, "description": format!("{t} from {label}"), "inputSchema": schema(t) }))
                    .collect();
                send(done(json!({ "tools": list })));
            }
            "tools/call" => {
                let name = msg["params"]["name"].as_str().unwrap_or_default().to_string();
                let a = &msg["params"]["arguments"];
                if !tools.contains(&name) {
                    send(fail(-32602, format!("Unknown tool: {name}")));
                    continue;
                }
                let text = |t: String| done(json!({ "content": [{ "type": "text", "text": t }], "isError": false }));
                let error = |t: String| done(json!({ "content": [{ "type": "text", "text": t }], "isError": true }));
                match name.as_str() {
                    "echo" => match a["text"].as_str() {
                        Some(t) if !t.is_empty() => {
                            let mut r = text(t.to_string());
                            r["result"]["structuredContent"] = json!({ "echo": t });
                            send(r)
                        }
                        _ => send(error("echo needs a non-empty `text`".into())),
                    },
                    "greet" => send(text(format!("Hello, {}! ({label})", a["name"].as_str().unwrap_or("?")))),
                    "add" => send(text((a["a"].as_i64().unwrap_or(0) + a["b"].as_i64().unwrap_or(0)).to_string())),
                    "boom" => panic!("kaboom in {label}: distinctive failure 7f3a"),
                    "hang" => std::thread::sleep(std::time::Duration::from_secs(600)),
                    "fail" => send(error(format!("fail says no ({label})"))),
                    "read_file" => match std::fs::read_to_string(a["path"].as_str().unwrap_or_default()) {
                        Ok(t) => send(text(t)),
                        Err(e) => send(error(format!("can't read: {e}"))),
                    },
                    "read_sentinel" => match flag(&args, "--sentinel").map(std::fs::read_to_string) {
                        Some(Ok(t)) => send(text(t)),
                        Some(Err(e)) => send(error(format!("can't read the sentinel: {e}"))),
                        None => send(error("no --sentinel".into())),
                    },
                    "env" => {
                        let names: Vec<String> = std::env::vars().map(|(k, _)| k).collect();
                        send(text(serde_json::to_string(&names).unwrap()))
                    }
                    "complete" => {
                        // The next scripted reply (a counter file keeps the place across processes).
                        let script = flag(&args, "--script").unwrap_or_default();
                        let replies: Vec<Value> =
                            std::fs::read(&script).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
                        let counter = format!("{script}.n");
                        let n: usize = std::fs::read_to_string(&counter).ok().and_then(|s| s.trim().parse().ok()).unwrap_or(0);
                        let _ = std::fs::write(&counter, (n + 1).to_string());
                        let reply = replies.get(n.min(replies.len().saturating_sub(1))).cloned().unwrap_or(json!({ "text": "ok" }));
                        let mut r = text(reply.to_string());
                        r["result"]["structuredContent"] = reply;
                        send(r)
                    }
                    _ => send(fail(-32602, format!("Unknown tool: {name}"))),
                }
            }
            other => send(fail(-32601, format!("Method not found: {other}"))),
        }
    }
}
