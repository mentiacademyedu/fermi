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
    let mut args: Vec<String> =
        ["-p", "--output-format", "stream-json", "--verbose", "--include-partial-messages"].map(String::from).to_vec();
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
    let sec = &e.cfg.security;
    let requested = ctx.permission().unwrap_or("default").to_string();
    let mut mode = ctx.permission().map(str::to_string);
    // Claude never asks in bypass mode, so commands needing approval couldn't be held.
    if mode.as_deref() == Some("bypassPermissions") && !sec.confirm_commands.is_empty() {
        emit(
            ctx.tx,
            Event::Log {
                line: "commands that need your approval are configured, so this runs in acceptEdits mode; fermi allows everything else"
                    .into(),
            },
        );
        mode = Some("acceptEdits".into());
    }
    if let Some(pm) = &mode {
        args.extend(["--permission-mode".into(), pm.clone()]);
    }
    let mut allowed: Vec<String> = ctx.task.allowed.clone();
    if let Some(b) = mcp::bridge_for(e, ctx.profile)? {
        args.extend(["--mcp-config".into(), b.config_path, "--append-system-prompt".into(), b.system_prompt]);
        allowed.insert(0, b.allowed_tools);
        // Every permission question goes to fermi's policy (see `policy::decide`).
        if crate::policy::active(sec) {
            args.extend(["--permission-prompt-tool".into(), "mcp__fermi__permission".into()]);
        }
    }
    if !allowed.is_empty() {
        // One flag, one rule per argument: rules may contain spaces or commas.
        args.push("--allowedTools".into());
        args.extend(allowed);
    }
    let mut denied = crate::policy::claude_read_rules(&sec.deny_read);
    denied.extend(sec.deny_commands.iter().map(|c| format!("Bash({c})")));
    if !denied.is_empty() {
        args.push("--disallowedTools".into());
        args.extend(denied);
    }
    args.extend(ctx.profile.claude_args.iter().cloned());

    // The bridge works in the same fermi home as this engine.
    let mut envs = vec![
        ("FERMI_DEPTH".to_string(), (e.depth + 1).to_string()),
        ("FERMI_HOME".to_string(), e.home.display().to_string()),
        // What the user chose, for the permission hook (the run may use acceptEdits).
        ("FERMI_PERMISSION_MODE".to_string(), requested),
    ];
    // Calls the agent delegates through fermi's bridge are attributed to this task.
    if let Some(id) = ctx.task.id {
        envs.push(("FERMI_TASK_ID".into(), id.to_string()));
    }
    if let Some(home) = ctx.account.home_dir() {
        std::fs::create_dir_all(&home)?;
        envs.push(("CLAUDE_CONFIG_DIR".into(), home.display().to_string()));
    }

    let jail = ctx.jail(&bin).await?;
    let mut p = Proc::spawn(&bin, &args, &envs, &ctx.task.cwd, &ctx.task.input(), ctx.tx, jail.as_ref()).await?;
    let (mut text, mut usage) = (String::new(), Usage::default());
    let (mut blocked, mut resets, mut error, mut got_result) = (false, None, None::<String>, false);

    while let Some(v) = p.next_json().await? {
        match v["type"].as_str().unwrap_or_default() {
            // Live text of the main conversation (subagents have a parent id).
            "stream_event" if v["parent_tool_use_id"].is_null() => {
                let ev = &v["event"];
                if ev["type"] == "content_block_delta" && ev["delta"]["type"] == "text_delta" {
                    if let Some(t) = ev["delta"]["text"].as_str() {
                        emit(ctx.tx, Event::Delta { text: t.to_string() });
                    }
                }
            }
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
                        Some("tool_use") => emit(
                            ctx.tx,
                            Event::Tool { name: block["name"].as_str().unwrap_or("tool").to_string(), detail: brief(&block["input"]) },
                        ),
                        _ => {}
                    }
                }
            }
            "rate_limit_event" => {
                let info = &v["rate_limit_info"];
                let util =
                    info["unifiedWindows"].as_object().and_then(|w| w.values().filter_map(|x| x["utilization"].as_f64()).reduce(f64::max));
                resets = info["resetsAt"].as_u64().or(resets);
                let is_blocked = info["status"].as_str() == Some("rejected");
                blocked |= is_blocked;
                e.store.observe_window(&ctx.account.id, util, resets);
                emit(ctx.tx, Event::Limit { account: ctx.account.id.clone(), utilization: util, resets_at: resets, blocked: is_blocked });
            }
            "result" => {
                got_result = true;
                let u = &v["usage"];
                let n = |k: &str| u[k].as_u64().unwrap_or(0);
                usage = Usage {
                    input: n("input_tokens") + n("cache_creation_input_tokens") + n("cache_read_input_tokens"),
                    cached_input: n("cache_read_input_tokens"),
                    cache_write: n("cache_creation_input_tokens"),
                    output: n("output_tokens"),
                    cost_usd: v["total_cost_usd"].as_f64(),
                };
                // Calls that needed an approval nobody could give (we run headless).
                let mut seen = std::collections::HashSet::new();
                for d in v["permission_denials"].as_array().into_iter().flatten() {
                    let tool = d["tool_name"].as_str().unwrap_or("tool").to_string();
                    let input = &d["tool_input"];
                    let rules = allow_rules(&tool, input);
                    if seen.insert(rules.join(",")) {
                        emit(ctx.tx, Event::Blocked { detail: brief(input), tool, rules });
                    }
                }
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

/// Rules that let a blocked call through next time: shell commands by the
/// programs they run (`Bash(node:*)`), anything else by tool name.
pub fn allow_rules(tool: &str, input: &serde_json::Value) -> Vec<String> {
    let Some(cmd) = input["command"].as_str().filter(|_| matches!(tool, "Bash" | "PowerShell")) else {
        return vec![tool.to_string()];
    };
    let mut out: Vec<String> = vec![];
    for seg in cmd.split(['\n', ';', '|', '&']) {
        // Skip `X=1` environment assignments and PowerShell's call operator.
        let prog = seg.split_whitespace().find(|w| !w.contains('=') && *w != "&").unwrap_or("");
        let prog = prog.trim_matches(|c| c == '"' || c == '\'' || c == '(' || c == ')');
        if prog.is_empty() || !prog.chars().all(|c| c.is_alphanumeric() || "._-/\\:".contains(c)) {
            continue;
        }
        let rule = format!("{tool}({prog}:*)");
        if !out.contains(&rule) {
            out.push(rule);
        }
    }
    if out.is_empty() {
        out.push(format!("{tool}({cmd})"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::allow_rules;
    use serde_json::json;

    #[test]
    fn rules_for_blocked_calls() {
        assert_eq!(allow_rules("Bash", &json!({ "command": "node make_slide.js" })), ["Bash(node:*)"]);
        assert_eq!(
            allow_rules("PowerShell", &json!({ "command": "cd C:\\pruebasfermi; node make_slide.js && npm test" })),
            ["PowerShell(cd:*)", "PowerShell(node:*)", "PowerShell(npm:*)"]
        );
        assert_eq!(allow_rules("Bash", &json!({ "command": "FOO=1 python x.py | head" })), ["Bash(python:*)", "Bash(head:*)"]);
        assert_eq!(allow_rules("WebFetch", &json!({ "url": "https://x" })), ["WebFetch"]);
    }
}
