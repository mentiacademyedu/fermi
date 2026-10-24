//! The bridge that lets a Claude session use the profile's other providers.
//! `fermi mcp --profile <name>` is a minimal stdio MCP server; every Claude
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

/// MCP config for a Claude run, or None inside a delegated call. The bridge
/// always offers `create_deck`, plus delegation when the profile has other
/// providers.
pub fn bridge_for(e: &Engine, profile: &Profile) -> Result<Option<Bridge>> {
    if e.depth > 0 {
        return Ok(None);
    }
    let (text, image) = delegates(e, profile);
    let exe = std::env::current_exe()?;
    let cfg = json!({ "mcpServers": { "fermi": {
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

    let mut tools = vec![
        "mcp__fermi__create_deck",
        "mcp__fermi__handoff",
        "mcp__fermi__submit_plan",
        "mcp__fermi__code_search",
        "mcp__fermi__code_symbol",
        "mcp__fermi__code_impact",
        "mcp__fermi__code_file",
        "mcp__fermi__code_replace",
        "mcp__fermi__run_checks",
    ];
    let mut prompt = format!(
        "You are running inside the fermi harness, profile {}. For slides or a presentation, call the fermi create_deck tool: \
         it writes a designed PowerPoint (.pptx) directly, with no code to run, and the file opens in fermi's slides viewer \
         where the user can present and edit it. Use it instead of writing a script or a Markdown deck, unless the user asks for those. \
         fermi also keeps a code index of this repository (Rust, Python, JavaScript/TypeScript, Go) that resolves calls, imports and \
         tests, which text search can't. For who calls a function, what it calls, which tests reach it, or what a change could \
         break, use the fermi code tools (code_symbol, code_impact, code_search, code_file) rather than grep: call code_symbol \
         before changing a function, and code_impact after changing code to find what could break and which tests to run. \
         To rewrite a whole function or type, code_replace swaps exactly that definition and refuses edits that don't parse. \
         Before saying you're done, call run_checks: fermi runs the project's build, tests and linters and reports exact \
         failures (file, line, failing tests); fix what it reports.",
        profile.name
    );
    if !image.is_empty() {
        tools.push("mcp__fermi__generate_image");
        prompt += " For any image creation request call the fermi generate_image tool, which uses an OpenAI image model and saves a file.";
    }
    if !text.is_empty() {
        tools.push("mcp__fermi__ask_model");
        let ids: Vec<_> = text.iter().map(|m| m.id.as_str()).collect();
        prompt += &format!(
            " You can delegate a self-contained subtask to another provider with the fermi ask_model tool (models: {}) when it is better suited or a second opinion helps.",
            ids.join(", ")
        );
    }
    Ok(Some(Bridge { config_path: path.display().to_string(), allowed_tools: tools.join(","), system_prompt: prompt }))
}

fn tool_list(e: &Engine, profile: &Profile) -> Value {
    let (text, image) = delegates(e, profile);
    // Team agents (their folder has fermi's marker) get the team tools.
    let team = std::env::current_dir().ok().and_then(|d| crate::team::marker(&d));
    let mut tools = vec![json!({
        "name": "create_deck",
        "description": "Create a PowerPoint deck (.pptx) from slide content, with a designed layout per slide. \
            No code needed. Keep slides short: a title and a few points each; use steps for processes, two_columns for comparisons, \
            table for data, image for pictures, stat for a key number, section to separate parts. Put details in notes.",
        "inputSchema": crate::deck::schema(),
    })];
    let target = json!({ "type": "object", "properties": { "target": { "type": "string", "description": "A name, Type::name, or path:line" } }, "required": ["target"] });
    tools.push(json!({ "name": "code_symbol", "description": "What a function, method or type does (signature, docs, body), who calls it, what it calls, and which tests reach it.", "inputSchema": target }));
    tools.push(json!({ "name": "code_search", "description": "Find functions, methods and types by name in this repository's code index.", "inputSchema": { "type": "object", "properties": { "query": { "type": "string" } }, "required": ["query"] } }));
    tools.push(json!({ "name": "code_impact", "description": "What the uncommitted changes (or changes since `base`) could break: changed symbols, callers affected (transitively), files importing them, tests to run, and changes no test reaches.", "inputSchema": { "type": "object", "properties": { "base": { "type": "string", "description": "A commit or branch to compare with; default HEAD" } } } }));
    tools.push(json!({ "name": "code_file", "description": "Outline of a file (its symbols), what it imports and what imports it.", "inputSchema": { "type": "object", "properties": { "path": { "type": "string", "description": "Path relative to the repository" } }, "required": ["path"] } }));
    tools.push(json!({
        "name": "code_replace",
        "description": "Replace one whole definition (function, method, struct, class…) with new source. Only that definition changes; \
            the edit is refused, and nothing written, if the file wouldn't parse or the definition would disappear.",
        "inputSchema": { "type": "object", "properties": {
            "target": { "type": "string", "description": "The definition: a name, Type::name, module::name or path:line" },
            "source": { "type": "string", "description": "The complete new definition, from its first keyword to its end" }
        }, "required": ["target", "source"] }
    }));
    tools.push(json!({
        "name": "run_checks",
        "description": "Run the project's checks (build, tests, lint, dependency advisories) the way fermi verifies work, and get \
            exact results: pass or fail per check, compiler errors with file and line, failing tests with their output.",
        "inputSchema": { "type": "object", "properties": {
            "checks": { "type": "array", "items": { "type": "string" }, "description": "Only these checks (by name); default all" }
        } }
    }));
    if let Some(m) = &team {
        let list = json!({ "type": "array", "items": { "type": "string" } });
        tools.push(json!({
            "name": "handoff",
            "description": "Finish your team task: record what you did for the agents who build on it and for the user. Call it once, at the end.",
            "inputSchema": { "type": "object", "properties": {
                "summary": { "type": "string", "description": "What you did, in a few sentences" },
                "files_changed": list, "decisions": list, "risks": list, "next_steps": list,
                "tests": { "type": "string", "description": "Commands you ran and their results" }
            }, "required": ["summary"] }
        }));
        if !m["plan"].is_null() {
            tools.push(json!({
                "name": "submit_plan",
                "description": "Submit the team's plan. Each task runs as its own agent in its own git worktree.",
                "inputSchema": { "type": "object", "properties": {
                    "summary": { "type": "string", "description": "The approach, in a few sentences" },
                    "tasks": { "type": "array", "items": { "type": "object", "properties": {
                        "id": { "type": "string", "description": "Short id like t1" },
                        "role": { "type": "string", "enum": ["architect", "developer", "tester", "security", "debugger", "researcher"] },
                        "title": { "type": "string" },
                        "description": { "type": "string", "description": "What to do, with enough context to do it alone" },
                        "depends_on": list,
                        "files": { "type": "array", "items": { "type": "string" }, "description": "Paths it may change" },
                        "done_when": { "type": "string", "description": "Acceptance criteria" }
                    }, "required": ["id", "role", "title", "description"] } }
                }, "required": ["summary", "tasks"] }
            }));
        }
    }
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

/// Runs one bridge tool as if called from `cwd` (the agent's folder).
pub async fn call_tool(e: &Engine, profile: &Profile, cwd: &std::path::Path, name: &str, args: &Value) -> Result<String> {
    let (text, image) = delegates(e, profile);
    let prompt = args["prompt"].as_str().unwrap_or_default().to_string();
    let cwd = cwd.to_path_buf();
    if let Some(kind) = name.strip_prefix("code_") {
        return code_tool(&cwd, kind, args);
    }
    if name == "run_checks" {
        let only: Vec<String> = args["checks"].as_array().into_iter().flatten().filter_map(|v| v.as_str().map(str::to_string)).collect();
        let report = crate::verify::run(&cwd, crate::verify::RunOpts { only: &only, ..Default::default() }).await?;
        return Ok(crate::verify::text(&report));
    }
    if name == "handoff" || name == "submit_plan" {
        let m = crate::team::marker(&cwd).ok_or_else(|| anyhow::anyhow!("this isn't a team task"))?;
        let key = if name == "handoff" { "handoff" } else { "plan" };
        let path = m[key].as_str().ok_or_else(|| anyhow::anyhow!("this task can't {name}"))?;
        if name == "submit_plan" {
            let (_, tasks) = crate::team::parse_plan(args)?; // say what's wrong while the architect can still fix it
            std::fs::write(path, serde_json::to_vec_pretty(args)?)?;
            return Ok(format!("Plan received: {} tasks. The user reviews it before the team starts. You can stop here.", tasks.len()));
        }
        std::fs::write(path, serde_json::to_vec_pretty(args)?)?;
        return Ok("Handoff recorded. You're done.".into());
    }
    if name == "create_deck" {
        let spec: crate::deck::DeckSpec =
            serde_json::from_value(args.clone()).map_err(|err| anyhow::anyhow!("bad deck description: {err}"))?;
        let mut out = cwd.join(args["path"].as_str().unwrap_or("deck.pptx"));
        if !out.extension().is_some_and(|x| x.eq_ignore_ascii_case("pptx")) {
            out.set_extension("pptx");
        }
        let n = crate::deck::write(&spec, &out, &cwd)?;
        return Ok(format!("Wrote {} ({n} slides). It opens in fermi's slides viewer.", out.display()));
    }
    let mut task = Task::new(prompt, cwd);
    task.profile = Some(profile.name.clone());
    task.parent = std::env::var("FERMI_TASK_ID").ok().and_then(|v| v.parse().ok());
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

/// The code index tools. The index stays loaded for the session and catches
/// up with edits on every call (only changed files are parsed again).
fn code_tool(cwd: &std::path::Path, kind: &str, args: &Value) -> Result<String> {
    use crate::code::{canonical, repo_root, text, Index};
    static INDEX: std::sync::Mutex<Option<Index>> = std::sync::Mutex::new(None);
    let root = repo_root(cwd);
    let mut slot = INDEX.lock().unwrap();
    match slot.as_mut() {
        Some(i) if i.root == canonical(&root)? => {
            i.update()?;
        }
        _ => *slot = Some(Index::open(&root)?),
    }
    let idx = slot.as_mut().unwrap();
    let arg = |k: &str| args[k].as_str().unwrap_or_default().to_string();
    Ok(match kind {
        "replace" => {
            let r = idx.replace_symbol(&arg("target"), &arg("source"))?;
            format!(
                "Replaced {} {} in {} (lines {}-{} are now {}-{}).\n{}",
                r["kind"].as_str().unwrap_or(""),
                r["symbol"].as_str().unwrap_or(""),
                r["file"].as_str().unwrap_or(""),
                r["before"]["line"],
                r["before"]["end"],
                r["after"]["line"],
                r["after"]["end"],
                r["patch"].as_str().unwrap_or("")
            )
        }
        "search" => text::search(&idx.search(&arg("query"), 30)),
        "symbol" => text::symbol(&idx.symbol(&arg("target"))),
        "file" => text::file(&idx.file(&arg("path"))),
        "impact" => {
            let base = arg("base");
            text::impact(&idx.impact(&idx.git_changes((!base.is_empty()).then_some(base.as_str()))?))
        }
        other => anyhow::bail!("unknown tool code_{other}"),
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
                    "serverInfo": { "name": "fermi", "version": env!("CARGO_PKG_VERSION") }
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
                    let cwd = std::env::current_dir().unwrap_or_default();
                    let result = match call_tool(&e, &p, &cwd, name, &msg["params"]["arguments"]).await {
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
