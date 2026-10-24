//! `codex exec --json`, one config dir per account (`CODEX_HOME`).

use super::{classify_error, RunCtx, RunResult, Usage};
use crate::events::{emit, Event};
use crate::trace::Ops;
use anyhow::Result;
use std::path::PathBuf;

/// The Codex config dir this account uses (`CODEX_HOME`).
pub fn codex_home(account: &crate::config::Account) -> Option<PathBuf> {
    account.home_dir().or_else(|| std::env::var_os("CODEX_HOME").map(PathBuf::from)).or_else(|| dirs::home_dir().map(|h| h.join(".codex")))
}

/// On Windows, Codex only enforces write sandboxes through its own Windows
/// sandbox; when that's not configured it silently runs read-only. Turn the
/// unelevated one on unless the account's config already picks a mode.
fn needs_windows_sandbox(account: &crate::config::Account, sandbox: Option<&str>) -> bool {
    if !cfg!(windows) || !matches!(sandbox, Some("workspace-write")) {
        return false;
    }
    let configured =
        codex_home(account).and_then(|h| std::fs::read_to_string(h.join("config.toml")).ok()).is_some_and(|t| t.contains("[windows]"));
    !configured
}

/// `sandbox` overrides the profile/task sandbox (image generation runs read-only).
pub async fn run(ctx: &RunCtx<'_>, prompt: &str, model: Option<&str>, sandbox: Option<&str>) -> Result<RunResult> {
    let e = ctx.engine;
    let bin = ctx.bin("codex", e.cfg.bins.codex.as_deref())?;
    // `exec resume <id>` continues a thread; it runs in the process's working
    // directory and has no --cd/--sandbox flags, so those go through config.
    let resume = ctx.task.resume.as_deref();
    let mut args: Vec<String> = vec!["exec".into()];
    if resume.is_some() {
        args.push("resume".into());
    }
    args.extend(["--json".into(), "--skip-git-repo-check".into()]);
    if resume.is_none() {
        args.extend(["-C".into(), ctx.task.cwd.display().to_string()]);
    }
    if let Some(m) = model {
        args.extend(["-m".into(), m.to_string()]);
    }
    if let Some(e) = ctx.effort() {
        // Unquoted on purpose: Codex reads a non-TOML value as a plain string,
        // and quotes would have to survive the npm .cmd shim on Windows.
        args.extend(["--config".into(), format!("model_reasoning_effort={e}")]);
    }
    let sandbox = sandbox.or(ctx.codex_sandbox());
    if let Some(s) = sandbox {
        match resume {
            Some(_) => args.extend(["--config".into(), format!("sandbox_mode={s}")]),
            None => args.extend(["--sandbox".into(), s.to_string()]),
        }
    }
    if needs_windows_sandbox(ctx.account, sandbox) {
        args.extend(["--config".into(), "windows.sandbox=unelevated".into()]);
    }
    args.extend(ctx.profile.codex_args.iter().cloned());
    if let Some(id) = resume {
        args.push(id.to_string());
    }
    args.push("-".into()); // prompt from stdin

    let mut envs = vec![("FERMI_DEPTH".to_string(), (e.depth + 1).to_string())];
    if let Some(home) = ctx.account.home_dir() {
        std::fs::create_dir_all(&home)?;
        envs.push(("CODEX_HOME".into(), home.display().to_string()));
    }

    let mut p = ctx.spawn(&bin, &args, &envs, prompt).await?;
    let (mut text, mut usage, mut error, mut completed) = (String::new(), Usage::default(), None::<String>, false);
    let mut session = None;
    let (mut ops, mut turns) = (Ops::new(ctx), 0u32);

    while let Some(v) = p.next_json().await? {
        let kind = v["type"].as_str().unwrap_or_default();
        match kind {
            "thread.started" => {
                if let Some(id) = v["thread_id"].as_str() {
                    session = Some(id.to_string());
                    emit(ctx.tx, Event::Session { id: id.to_string() });
                }
            }
            "turn.started" => turns += 1,
            "item.started" | "item.completed" => {
                let item = &v["item"];
                let s = |k: &str| item[k].as_str().unwrap_or_default().to_string();
                let id = item["id"].as_str();
                let call = format!("turn-{}", turns.max(1));
                // A tool item: its name and input; reported when it starts (or,
                // if it never said so, when it completes).
                let tool = match item["type"].as_str().unwrap_or_default() {
                    "command_execution" => Some(("shell".to_string(), serde_json::json!({ "command": item["command"] }))),
                    "file_change" => Some(("edit".to_string(), serde_json::json!({ "changes": item["changes"] }))),
                    "mcp_tool_call" => Some((format!("{}:{}", s("server"), s("tool")), item["arguments"].clone())),
                    "web_search" => Some(("web_search".to_string(), serde_json::json!({ "query": item["query"] }))),
                    _ => None,
                };
                if let Some((name, input)) = &tool {
                    let id = ops.tool(id, name, input, Some(&call));
                    if kind == "item.completed" {
                        let status = match item["status"].as_str() {
                            Some("failed") => "error",
                            Some("declined") => "declined",
                            _ => "ok",
                        };
                        let output = match item["type"].as_str() {
                            Some("mcp_tool_call") => {
                                let result = || crate::trace::content_text(&item["result"]["content"]);
                                item["error"]["message"].as_str().map(str::to_string).unwrap_or_else(result)
                            }
                            _ => s("aggregated_output"),
                        };
                        let code = item["exit_code"].as_i64().filter(|c| *c != 0).map(|c| format!("exit {c}"));
                        let code = code.or_else(|| (status != "ok").then(|| status.to_string()));
                        ops.result(&id, status, &output, code);
                    }
                } else if kind == "item.completed" {
                    match item["type"].as_str().unwrap_or_default() {
                        "agent_message" => {
                            text = s("text");
                            emit(ctx.tx, Event::Text { text: text.clone() });
                        }
                        "error" => emit(ctx.tx, Event::Log { line: s("message") }),
                        _ => {}
                    }
                }
            }
            "turn.completed" => {
                completed = true;
                let u = &v["usage"];
                usage.input += u["input_tokens"].as_u64().unwrap_or(0);
                usage.cached_input += u["cached_input_tokens"].as_u64().unwrap_or(0);
                usage.cache_write += u["cache_write_input_tokens"].as_u64().unwrap_or(0);
                usage.output += u["output_tokens"].as_u64().unwrap_or(0);
                usage.reported |= Usage::present(
                    u,
                    &[
                        ("input_tokens", Usage::INPUT),
                        ("cached_input_tokens", Usage::CACHED),
                        ("cache_write_input_tokens", Usage::CACHE_WRITE),
                        ("output_tokens", Usage::OUTPUT),
                    ],
                );
                ops.call(&format!("turn-{}", turns.max(1)), model, u);
            }
            "turn.failed" => error = v["error"]["message"].as_str().map(str::to_string),
            "error" => error = v["message"].as_str().map(str::to_string),
            _ => {}
        }
    }
    ops.finish();

    let (success, stderr) = p.finish().await?;
    let mut res = match error {
        Some(msg) if !completed => classify_error(msg, None),
        _ if !completed || !success => {
            classify_error(if stderr.is_empty() { "codex exited without completing".into() } else { stderr }, None)
        }
        _ => RunResult::ok(text, usage),
    };
    res.usage = usage;
    res.session = session;
    Ok(res)
}
