//! `claude -p --output-format stream-json`, one config dir per account
//! (`CLAUDE_CONFIG_DIR`).

use super::proc::Proc;
use super::{brief, classify_error, RunCtx, RunResult, Usage};
use crate::events::{emit, Event};
use crate::mcp;
use crate::util::{resolve_bin, uuid_v4};
use anyhow::Result;

pub async fn run(ctx: &RunCtx<'_>) -> Result<RunResult> {
    let e = ctx.engine;
    let bin = resolve_bin("claude", e.cfg.bins.claude.as_deref())?;
    let mut args: Vec<String> = ["-p", "--output-format", "stream-json", "--verbose"].map(String::from).to_vec();
    if let Some(m) = &ctx.model.cli {
        args.extend(["--model".into(), m.clone()]);
    }
    if let Some(e) = ctx.effort() {
        args.extend(["--effort".into(), e.to_string()]);
    }
    // We choose the session id up front so a follow-up can `--resume` it.
    match &ctx.task.resume {
        Some(id) => args.extend(["--resume".into(), id.clone()]),
        None => {
            let id = uuid_v4();
            args.extend(["--session-id".into(), id.clone()]);
            emit(ctx.tx, Event::Session { id });
        }
    }
    if let Some(pm) = &ctx.profile.permission_mode {
        args.extend(["--permission-mode".into(), pm.clone()]);
    }
    if let Some(b) = mcp::bridge_for(e, ctx.profile)? {
        args.extend([
            "--mcp-config".into(),
            b.config_path,
            "--allowedTools".into(),
            b.allowed_tools,
            "--append-system-prompt".into(),
            b.system_prompt,
        ]);
    }
    args.extend(ctx.profile.claude_args.iter().cloned());

    let mut envs = vec![("FERMI_DEPTH".to_string(), (e.depth + 1).to_string())];
    if let Some(home) = ctx.account.home_dir() {
        std::fs::create_dir_all(&home)?;
        envs.push(("CLAUDE_CONFIG_DIR".into(), home.display().to_string()));
    }

    let mut p = Proc::spawn(&bin, &args, &envs, &ctx.task.cwd, &ctx.task.prompt, ctx.tx).await?;
    let (mut text, mut usage) = (String::new(), Usage::default());
    let (mut blocked, mut resets, mut error, mut got_result) = (false, None, None::<String>, false);

    while let Some(v) = p.next_json().await? {
        match v["type"].as_str().unwrap_or_default() {
            "assistant" => {
                for block in v["message"]["content"].as_array().into_iter().flatten() {
                    match block["type"].as_str() {
                        Some("text") => {
                            let t = block["text"].as_str().unwrap_or_default();
                            if !t.trim().is_empty() {
                                text = t.to_string();
                                emit(ctx.tx, Event::Text { text: t.to_string() });
                            }
                        }
                        Some("tool_use") => emit(ctx.tx, Event::Tool {
                            name: block["name"].as_str().unwrap_or("tool").to_string(),
                            detail: brief(&block["input"]),
                        }),
                        _ => {}
                    }
                }
            }
            "rate_limit_event" => {
                let info = &v["rate_limit_info"];
                let util = info["unifiedWindows"]
                    .as_object()
                    .and_then(|w| w.values().filter_map(|x| x["utilization"].as_f64()).reduce(f64::max));
                resets = info["resetsAt"].as_u64().or(resets);
                let is_blocked = info["status"].as_str() == Some("rejected");
                blocked |= is_blocked;
                e.store.observe_window(&ctx.account.id, util, resets);
                emit(ctx.tx, Event::Limit {
                    account: ctx.account.id.clone(),
                    utilization: util,
                    resets_at: resets,
                    blocked: is_blocked,
                });
            }
            "result" => {
                got_result = true;
                let u = &v["usage"];
                let n = |k: &str| u[k].as_u64().unwrap_or(0);
                usage = Usage {
                    input: n("input_tokens") + n("cache_creation_input_tokens") + n("cache_read_input_tokens"),
                    output: n("output_tokens"),
                    cost_usd: v["total_cost_usd"].as_f64(),
                };
                if v["is_error"].as_bool() == Some(true) {
                    let msg = v["result"].as_str().or(v["subtype"].as_str()).unwrap_or("claude reported an error");
                    error = Some(msg.to_string());
                } else if let Some(r) = v["result"].as_str() {
                    text = r.to_string();
                }
            }
            _ => {}
        }
    }

    let (success, stderr) = p.finish().await?;
    let mut res = if blocked {
        RunResult::limited(resets)
    } else if let Some(msg) = error {
        classify_error(msg, resets)
    } else if !got_result || !success {
        let msg = if stderr.is_empty() { "claude exited without a result".to_string() } else { stderr };
        classify_error(msg, resets)
    } else {
        RunResult::ok(text, usage)
    };
    res.usage = usage;
    Ok(res)
}
