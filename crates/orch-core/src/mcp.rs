//! The bridge that lets a Claude session use the profile's other providers.
//! `orch mcp --profile <name>` is a minimal stdio MCP server; every Claude
//! task at depth 0 gets it attached, and its tool calls go back through the
//! engine (same profile, same accounts, same failover).

use crate::config::{Profile, Provider};
use crate::engine::{Engine, Task};
use crate::models::ModelSpec;
use anyhow::Result;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;

pub struct Bridge {
    pub config_path: String,
    pub allowed_tools: String,
    pub system_prompt: String,
}

/// Non-Claude models this profile can reach, split into (text, image).
fn delegates(e: &Engine, profile: &Profile) -> (Vec<ModelSpec>, Vec<ModelSpec>) {
    let providers: Vec<Provider> = e.cfg.profile_accounts(profile).map(|a| a.provider).collect();
    e.catalog
        .iter()
        .filter(|m| m.provider().is_some_and(|p| p != Provider::Claude && providers.contains(&p)))
        .cloned()
        .partition(|m| !m.image_only())
}

/// MCP config for a Claude run, or None when there is nothing to delegate to
/// (or we are already inside a delegated call).
pub fn bridge_for(e: &Engine, profile: &Profile) -> Result<Option<Bridge>> {
    if e.depth > 0 {
        return Ok(None);
    }
    let (text, image) = delegates(e, profile);
    if text.is_empty() && image.is_empty() {
        return Ok(None);
    }
    let exe = std::env::current_exe()?;
    let cfg = json!({ "mcpServers": { "orch": {
        "type": "stdio",
        "command": exe.display().to_string(),
        "args": ["mcp", "--profile", profile.name],
    }}});
    let dir = e.home.join("run");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("mcp-{}.json", profile.name));
    let body = serde_json::to_string_pretty(&cfg)?;
    if std::fs::read_to_string(&path).ok().as_deref() != Some(body.as_str()) {
        std::fs::write(&path, body)?;
    }

    let mut tools = vec![];
    let mut prompt = format!("You are running inside the orch harness, profile {}.", profile.name);
    if !image.is_empty() {
        tools.push("mcp__orch__generate_image");
        prompt += " For any image creation request call the orch generate_image tool, which uses an OpenAI image model and saves a file.";
    }
    if !text.is_empty() {
        tools.push("mcp__orch__ask_model");
        let ids: Vec<_> = text.iter().map(|m| m.id.as_str()).collect();
        prompt += &format!(
            " You can delegate a self-contained subtask to another provider with the orch ask_model tool (models: {}) when it is better suited or a second opinion helps.",
            ids.join(", ")
        );
    }
    Ok(Some(Bridge { config_path: path.display().to_string(), allowed_tools: tools.join(","), system_prompt: prompt }))
}

fn tool_list(e: &Engine, profile: &Profile) -> Value {
    let (text, image) = delegates(e, profile);
    let mut tools = vec![];
    if !image.is_empty() {
        tools.push(json!({
            "name": "generate_image",
            "description": format!("Generate an image with {} and save it to disk. Returns the file path.", image[0].id),
            "inputSchema": { "type": "object", "properties": {
                "prompt": { "type": "string", "description": "Detailed description of the image" },
                "path": { "type": "string", "description": "Output file path (relative to the working directory). Optional." }
            }, "required": ["prompt"] }
        }));
    }
    if !text.is_empty() {
        let ids: Vec<_> = text.iter().map(|m| m.id.clone()).collect();
        tools.push(json!({
            "name": "ask_model",
            "description": format!("Send a self-contained task to another model and get its answer. Available: {}.", ids.join(", ")),
            "inputSchema": { "type": "object", "properties": {
                "prompt": { "type": "string", "description": "The full task, with all context the model needs" },
                "model": { "type": "string", "enum": ids, "description": "Model id; defaults to the first" }
            }, "required": ["prompt"] }
        }));
    }
    Value::Array(tools)
}

async fn call_tool(e: &Engine, profile: &Profile, name: &str, args: &Value) -> Result<String> {
    let (text, image) = delegates(e, profile);
    let prompt = args["prompt"].as_str().unwrap_or_default().to_string();
    let cwd = std::env::current_dir()?;
    let mut task = Task::new(prompt, cwd);
    task.profile = Some(profile.name.clone());
    match name {
        "generate_image" => {
            task.model = image.first().map(|m| m.id.clone());
            task.out = args["path"].as_str().map(PathBuf::from);
        }
        "ask_model" => {
            task.model = args["model"].as_str().map(str::to_string).or_else(|| text.first().map(|m| m.id.clone()));
        }
        _ => anyhow::bail!("unknown tool `{name}`"),
    }
    let (tx, _rx) = mpsc::unbounded_channel();
    let out = e.run(&task, &tx).await?;
    Ok(match out.artifacts.first() {
        Some(p) => format!("{}\n(generated by {} on account {})", p.display(), out.model, out.account),
        None => format!("{}\n\n(answered by {} on account {})", out.text, out.model, out.account),
    })
}

pub async fn serve(engine: Arc<Engine>, profile_name: String) -> Result<()> {
    let profile = Arc::new(engine.cfg.resolve_profile(Some(&profile_name), &std::env::current_dir()?)?);
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<Value>();
    let writer = tokio::spawn(async move {
        let mut stdout = tokio::io::stdout();
        while let Some(msg) = out_rx.recv().await {
            let mut line = msg.to_string();
            line.push('\n');
            if stdout.write_all(line.as_bytes()).await.is_err() || stdout.flush().await.is_err() {
                break;
            }
        }
    });

    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    while let Some(line) = lines.next_line().await? {
        let Ok(msg) = serde_json::from_str::<Value>(line.trim_start_matches('\u{feff}')) else { continue };
        let Some(id) = msg.get("id").cloned() else { continue }; // notifications need no reply
        let reply = |result: Value| json!({ "jsonrpc": "2.0", "id": id, "result": result });
        match msg["method"].as_str().unwrap_or_default() {
            "initialize" => {
                let version = msg["params"]["protocolVersion"].as_str().unwrap_or("2025-06-18");
                let _ = out_tx.send(reply(json!({
                    "protocolVersion": version,
                    "capabilities": { "tools": {} },
                    "serverInfo": { "name": "orch", "version": env!("CARGO_PKG_VERSION") }
                })));
            }
            "ping" => {
                let _ = out_tx.send(reply(json!({})));
            }
            "tools/list" => {
                let _ = out_tx.send(reply(json!({ "tools": tool_list(&engine, &profile) })));
            }
            "tools/call" => {
                let (e, p, tx) = (engine.clone(), profile.clone(), out_tx.clone());
                tokio::spawn(async move {
                    let name = msg["params"]["name"].as_str().unwrap_or_default();
                    let result = match call_tool(&e, &p, name, &msg["params"]["arguments"]).await {
                        Ok(text) => json!({ "content": [{ "type": "text", "text": text }] }),
                        Err(err) => json!({ "content": [{ "type": "text", "text": format!("{err:#}") }], "isError": true }),
                    };
                    let _ = tx.send(json!({ "jsonrpc": "2.0", "id": msg["id"], "result": result }));
                });
            }
            other => {
                let _ = out_tx.send(json!({
                    "jsonrpc": "2.0", "id": id,
                    "error": { "code": -32601, "message": format!("method not found: {other}") }
                }));
            }
        }
    }
    drop(out_tx);
    let _ = writer.await;
    Ok(())
}
